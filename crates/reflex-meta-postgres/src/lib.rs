use async_trait::async_trait;
use reflex_meta::{
    ArtifactRef, CellLease, ClaimRequest, ExperimentStatus, FinalAttempt, FinalizeResult,
    LeaseStatus, MetaError, MetaStore, NewCell, NewExperiment, PromotionRequest, PromotionResult,
};
use reflex_types::{CellId, Digest, ExperimentId};
use std::str::FromStr;

pub const POSTGRES_SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS experiments (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    domain TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    created_at TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE IF NOT EXISTS cells (
    id TEXT PRIMARY KEY,
    experiment_id TEXT NOT NULL REFERENCES experiments(id),
    generation_id TEXT NOT NULL,
    manifest_digest TEXT NOT NULL,
    resource_class TEXT NOT NULL,
    priority INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'ready',
    worker_id TEXT,
    attempt_no BIGINT NOT NULL DEFAULT 0,
    accepted_attempt_no BIGINT,
    fencing_token BIGINT NOT NULL DEFAULT 0,
    lease_expires_at TIMESTAMPTZ,
    completion_manifest_digest TEXT,
    created_at TIMESTAMPTZ DEFAULT now(),
    updated_at TIMESTAMPTZ DEFAULT now()
);

CREATE TABLE IF NOT EXISTS artifacts (
    cell_id TEXT NOT NULL REFERENCES cells(id),
    digest TEXT NOT NULL,
    kind TEXT NOT NULL,
    created_at TIMESTAMPTZ DEFAULT now(),
    PRIMARY KEY (cell_id, digest)
);

CREATE TABLE IF NOT EXISTS active_models (
    role TEXT PRIMARY KEY,
    checkpoint_id TEXT NOT NULL,
    updated_at TIMESTAMPTZ DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_cells_claim_queue ON cells (state, resource_class, priority DESC, created_at, id);
CREATE INDEX IF NOT EXISTS idx_artifacts_cell_digest ON artifacts (cell_id, digest, kind);
"#;

pub const POSTGRES_CLAIM_SQL: &str = r#"
WITH next AS (
    SELECT c.id
    FROM cells AS c
    WHERE c.state = 'ready'
      AND c.resource_class = $1
    ORDER BY c.priority DESC, c.created_at, c.id
    FOR UPDATE SKIP LOCKED
    LIMIT 1
)
UPDATE cells AS c
SET state = 'running',
    worker_id = $2,
    attempt_no = c.attempt_no + 1,
    fencing_token = c.fencing_token + 1,
    lease_expires_at = now() + ($3 || ' seconds')::interval,
    updated_at = now()
FROM next
WHERE c.id = next.id
RETURNING c.id, c.attempt_no, c.fencing_token, c.manifest_digest;
"#;

pub const POSTGRES_FINALIZE_SQL: &str = r#"
UPDATE cells
SET state = $4,
    accepted_attempt_no = $2,
    completion_manifest_digest = $5,
    lease_expires_at = NULL,
    updated_at = now()
WHERE id = $1
  AND state = 'running'
  AND attempt_no = $2
  AND fencing_token = $3
  AND EXISTS (
      SELECT 1 FROM artifacts
      WHERE cell_id = $1 AND digest = $5 AND kind = 'cell-completion-manifest'
  )
RETURNING id;
"#;

pub struct PostgresMetaStore {
    conn_str: String,
}

impl PostgresMetaStore {
    pub fn new(conn_str: String) -> Self {
        Self { conn_str }
    }

    pub fn schema_sql() -> &'static str {
        POSTGRES_SCHEMA_SQL
    }

    pub fn claim_sql() -> &'static str {
        POSTGRES_CLAIM_SQL
    }

    pub fn finalize_sql() -> &'static str {
        POSTGRES_FINALIZE_SQL
    }
}

#[async_trait]
impl MetaStore for PostgresMetaStore {
    async fn create_experiment(&self, record: NewExperiment) -> Result<ExperimentId, MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move {
            if let Err(e) = connection.await {
                tracing::error!("postgres connection error: {}", e);
            }
        });

        client
            .execute(
                "INSERT INTO experiments (id, name, domain, manifest_digest) VALUES ($1, $2, $3, $4)",
                &[
                    &record.id.to_hex(),
                    &record.name,
                    &record.domain,
                    &record.manifest_digest.to_hex(),
                ],
            )
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        Ok(record.id)
    }

    async fn enqueue_cells(&self, cells: &[NewCell]) -> Result<(), MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });

        for c in cells {
            client
                .execute(
                    r#"INSERT INTO cells 
                    (id, experiment_id, generation_id, manifest_digest, resource_class, priority, state) 
                    VALUES ($1, $2, $3, $4, $5, $6, 'ready')
                    ON CONFLICT (id) DO NOTHING"#,
                    &[
                        &c.id.to_hex(),
                        &c.experiment_id.to_hex(),
                        &c.generation_id.to_hex(),
                        &c.manifest_digest.to_hex(),
                        &c.resource_class,
                        &c.priority,
                    ],
                )
                .await
                .map_err(|e| MetaError::Database(e.to_string()))?;
        }
        Ok(())
    }

    async fn claim_cell(&self, request: ClaimRequest) -> Result<Option<CellLease>, MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let duration_str = request.lease_duration_secs.to_string();
        let rows = client
            .query(
                POSTGRES_CLAIM_SQL,
                &[
                    &request.resource_class,
                    &request.worker_id.to_hex(),
                    &duration_str,
                ],
            )
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        if let Some(row) = rows.into_iter().next() {
            let id_str: String = row.get(0);
            let attempt_no: i64 = row.get(1);
            let fencing_token: i64 = row.get(2);
            let manifest_str: String = row.get(3);

            let cell_id =
                CellId::from_str(&id_str).map_err(|e| MetaError::Database(e.to_string()))?;
            let manifest_digest =
                Digest::from_str(&manifest_str).map_err(|e| MetaError::Database(e.to_string()))?;

            let now_ts = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let lease_expires_at_timestamp = now_ts + request.lease_duration_secs;

            Ok(Some(CellLease {
                cell_id,
                attempt_no: attempt_no as u32,
                fencing_token: fencing_token as u64,
                manifest_digest,
                lease_expires_at_timestamp,
            }))
        } else {
            Ok(None)
        }
    }

    async fn heartbeat(&self, lease: &CellLease) -> Result<LeaseStatus, MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let rows_affected = client
            .execute(
                "UPDATE cells SET lease_expires_at = now() + interval '60 seconds' WHERE id = $1 AND fencing_token = $2 AND state = 'running'",
                &[&lease.cell_id.to_hex(), &(lease.fencing_token as i64)],
            )
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        if rows_affected > 0 {
            Ok(LeaseStatus::Active)
        } else {
            Ok(LeaseStatus::Revoked)
        }
    }

    async fn publish_attempt_artifacts(
        &self,
        lease: &CellLease,
        artifacts: &[ArtifactRef],
    ) -> Result<(), MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });

        for art in artifacts {
            client
                .execute(
                    "INSERT INTO artifacts (cell_id, digest, kind) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
                    &[&lease.cell_id.to_hex(), &art.digest.to_hex(), &art.kind],
                )
                .await
                .map_err(|e| MetaError::Database(e.to_string()))?;
        }
        Ok(())
    }

    async fn finalize_attempt(
        &self,
        lease: &CellLease,
        result: FinalAttempt,
    ) -> Result<FinalizeResult, MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });

        // Verify artifact exists in artifacts table
        let art_rows = client
            .query(
                "SELECT 1 FROM artifacts WHERE cell_id = $1 AND digest = $2",
                &[
                    &lease.cell_id.to_hex(),
                    &result.completion_manifest_digest.to_hex(),
                ],
            )
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        if art_rows.is_empty() {
            return Err(MetaError::MissingArtifact(
                result.completion_manifest_digest,
            ));
        }

        let state_str = if result.accepted {
            "succeeded"
        } else {
            "failed"
        };
        let attempt_i64 = lease.attempt_no as i64;
        let fence_i64 = lease.fencing_token as i64;

        let rows = client
            .query(
                POSTGRES_FINALIZE_SQL,
                &[
                    &lease.cell_id.to_hex(),
                    &attempt_i64,
                    &fence_i64,
                    &state_str,
                    &result.completion_manifest_digest.to_hex(),
                ],
            )
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        if rows.is_empty() {
            return Err(MetaError::StaleFence {
                cell_id: lease.cell_id,
                attempt_no: lease.attempt_no,
                token: lease.fencing_token,
            });
        }

        Ok(FinalizeResult {
            cell_id: lease.cell_id,
            accepted_attempt_no: lease.attempt_no,
            state: state_str.to_string(),
        })
    }

    async fn compare_and_promote(
        &self,
        request: PromotionRequest,
    ) -> Result<PromotionResult, MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let rows = client
            .query(
                "SELECT checkpoint_id FROM active_models WHERE role = 'ranker'",
                &[],
            )
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        let current_checkpoint = rows.into_iter().next().map(|r| r.get::<_, String>(0));

        match request.active_stable {
            Some(expected) if current_checkpoint.as_deref() != Some(&expected.to_hex()) => {
                return Err(MetaError::PromotionRejected {
                    reason: "active stable model changed before promotion".to_string(),
                });
            }
            _ => {}
        }

        client
            .execute(
                r#"INSERT INTO active_models (role, checkpoint_id, updated_at) 
                VALUES ('ranker', $1, now()) 
                ON CONFLICT (role) DO UPDATE SET checkpoint_id = excluded.checkpoint_id, updated_at = now()"#,
                &[&request.candidate.to_hex()],
            )
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        let receipt_digest = Digest::hash_blake3(b"postgres-promotion-receipt");
        Ok(PromotionResult {
            promoted: true,
            active: request.candidate,
            receipt_digest,
        })
    }

    async fn get_experiment_status(&self, id: ExperimentId) -> Result<ExperimentStatus, MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let rows = client
            .query(
                "SELECT name, domain FROM experiments WHERE id = $1",
                &[&id.to_hex()],
            )
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        let (name, domain) = if let Some(row) = rows.into_iter().next() {
            (row.get::<_, String>(0), row.get::<_, String>(1))
        } else {
            return Err(MetaError::ExperimentNotFound(id));
        };

        let mut status = ExperimentStatus {
            id,
            name,
            domain,
            total_cells: 0,
            ready_cells: 0,
            running_cells: 0,
            succeeded_cells: 0,
            failed_cells: 0,
        };

        let count_rows = client
            .query(
                "SELECT state, COUNT(*) FROM cells WHERE experiment_id = $1 GROUP BY state",
                &[&id.to_hex()],
            )
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        for r in count_rows {
            let state: String = r.get(0);
            let count: i64 = r.get(1);
            status.total_cells += count as usize;
            match state.as_str() {
                "ready" => status.ready_cells += count as usize,
                "running" => status.running_cells += count as usize,
                "succeeded" => status.succeeded_cells += count as usize,
                "failed" => status.failed_cells += count as usize,
                _ => {}
            }
        }

        Ok(status)
    }

    async fn get_cell_manifest(&self, id: CellId) -> Result<Option<Digest>, MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move {
            let _ = connection.await;
        });

        let rows = client
            .query(
                "SELECT manifest_digest FROM cells WHERE id = $1",
                &[&id.to_hex()],
            )
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        if let Some(row) = rows.into_iter().next() {
            let s: String = row.get(0);
            let digest = Digest::from_str(&s).map_err(|e| MetaError::Database(e.to_string()))?;
            Ok(Some(digest))
        } else {
            Ok(None)
        }
    }

    async fn list_experiments(&self) -> Result<Vec<reflex_meta::ExperimentSummary>, MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move { let _ = connection.await; });

        let rows = client
            .query("SELECT id, name, domain, created_at::text FROM experiments ORDER BY created_at", &[])
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            let id_str: String = row.get(0);
            let name: String = row.get(1);
            let domain: String = row.get(2);
            let created_at: String = row.get(3);
            let id = ExperimentId::from_str(&id_str).map_err(|e| MetaError::Database(e.to_string()))?;
            results.push(reflex_meta::ExperimentSummary { id, name, domain, created_at });
        }
        Ok(results)
    }

    async fn list_cells(&self) -> Result<Vec<reflex_meta::CellSummary>, MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move { let _ = connection.await; });

        let rows = client
            .query("SELECT id, experiment_id, generation_id, state, resource_class, priority, attempt_no, fencing_token FROM cells ORDER BY created_at", &[])
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            let id_str: String = row.get(0);
            let exp_str: String = row.get(1);
            let gen_str: String = row.get(2);
            let state: String = row.get(3);
            let resource_class: String = row.get(4);
            let priority: i32 = row.get(5);
            let attempt_no: i64 = row.get(6);
            let fencing_token: i64 = row.get(7);
            let id = CellId::from_str(&id_str).map_err(|e| MetaError::Database(e.to_string()))?;
            let experiment_id = ExperimentId::from_str(&exp_str).map_err(|e| MetaError::Database(e.to_string()))?;
            let generation_id = reflex_types::GenerationId::from_str(&gen_str).map_err(|e| MetaError::Database(e.to_string()))?;
            results.push(reflex_meta::CellSummary {
                id,
                experiment_id,
                generation_id,
                state,
                resource_class,
                priority,
                attempt_no: attempt_no as u32,
                fencing_token: fencing_token as u64,
            });
        }
        Ok(results)
    }

    async fn list_active_models(&self) -> Result<Vec<reflex_meta::ActiveModel>, MetaError> {
        let (client, connection) = tokio_postgres::connect(&self.conn_str, tokio_postgres::NoTls)
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;
        tokio::spawn(async move { let _ = connection.await; });

        let rows = client
            .query("SELECT role, checkpoint_id, updated_at::text FROM active_models", &[])
            .await
            .map_err(|e| MetaError::Database(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            let role: String = row.get(0);
            let checkpoint_id: String = row.get(1);
            let updated_at: String = row.get(2);
            results.push(reflex_meta::ActiveModel { role, checkpoint_id, updated_at });
        }
        Ok(results)
    }
}
