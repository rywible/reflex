use async_trait::async_trait;
use reflex_meta::{
    ArtifactRef, CellLease, CellSummary, ClaimRequest, ActiveModel, ExperimentStatus, ExperimentSummary,
    FinalAttempt, FinalizeResult, LeaseStatus, MetaError, MetaStore, NewCell, NewExperiment,
    PromotionRequest, PromotionResult,
};
use reflex_types::{CellId, Digest, ExperimentId};
use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub enum SqliteWrite {
    CreateExperiment(
        NewExperiment,
        oneshot::Sender<Result<ExperimentId, MetaError>>,
    ),
    EnqueueCells(Vec<NewCell>, oneshot::Sender<Result<(), MetaError>>),
    ClaimCell(
        ClaimRequest,
        oneshot::Sender<Result<Option<CellLease>, MetaError>>,
    ),
    Heartbeat(CellLease, oneshot::Sender<Result<LeaseStatus, MetaError>>),
    PublishArtifacts(
        CellLease,
        Vec<ArtifactRef>,
        oneshot::Sender<Result<(), MetaError>>,
    ),
    FinalizeAttempt(
        CellLease,
        FinalAttempt,
        oneshot::Sender<Result<FinalizeResult, MetaError>>,
    ),
    Promote(
        PromotionRequest,
        oneshot::Sender<Result<PromotionResult, MetaError>>,
    ),
    Checkpoint(oneshot::Sender<Result<(), MetaError>>),
}

pub fn configure(conn: &Connection) -> Result<(), rusqlite::Error> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

pub fn apply_migrations(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    let tx = conn.transaction()?;
    tx.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS experiments (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            domain TEXT NOT NULL,
            manifest_digest TEXT NOT NULL,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP
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
            attempt_no INTEGER NOT NULL DEFAULT 0,
            accepted_attempt_no INTEGER,
            fencing_token INTEGER NOT NULL DEFAULT 0,
            lease_expires_at INTEGER,
            completion_manifest_digest TEXT,
            created_at DATETIME DEFAULT CURRENT_TIMESTAMP,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS artifacts (
            cell_id TEXT NOT NULL REFERENCES cells(id),
            digest TEXT NOT NULL,
            kind TEXT NOT NULL,
            PRIMARY KEY (cell_id, digest)
        );

        CREATE TABLE IF NOT EXISTS active_models (
            role TEXT PRIMARY KEY,
            checkpoint_id TEXT NOT NULL,
            updated_at DATETIME DEFAULT CURRENT_TIMESTAMP
        );
        "#,
    )?;
    tx.commit()?;
    Ok(())
}

fn execute_write(conn: &mut Connection, cmd: SqliteWrite) {
    match cmd {
        SqliteWrite::CreateExperiment(record, reply) => {
            let res = (|| -> Result<ExperimentId, MetaError> {
                let tx = conn
                    .transaction()
                    .map_err(|e| MetaError::Database(e.to_string()))?;
                tx.execute(
                    "INSERT INTO experiments (id, name, domain, manifest_digest) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        record.id.to_hex(),
                        record.name,
                        record.domain,
                        record.manifest_digest.to_hex()
                    ],
                )
                .map_err(|e| MetaError::Database(e.to_string()))?;
                tx.commit()
                    .map_err(|e| MetaError::Database(e.to_string()))?;
                Ok(record.id)
            })();
            let _ = reply.send(res);
        }
        SqliteWrite::EnqueueCells(cells, reply) => {
            let res = (|| -> Result<(), MetaError> {
                let tx = conn
                    .transaction()
                    .map_err(|e| MetaError::Database(e.to_string()))?;
                for c in &cells {
                    tx.execute(
                        r#"INSERT OR IGNORE INTO cells 
                        (id, experiment_id, generation_id, manifest_digest, resource_class, priority, state) 
                        VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'ready')"#,
                        params![
                            c.id.to_hex(),
                            c.experiment_id.to_hex(),
                            c.generation_id.to_hex(),
                            c.manifest_digest.to_hex(),
                            c.resource_class,
                            c.priority
                        ],
                    )
                    .map_err(|e| MetaError::Database(e.to_string()))?;
                }
                tx.commit()
                    .map_err(|e| MetaError::Database(e.to_string()))?;
                Ok(())
            })();
            let _ = reply.send(res);
        }
        SqliteWrite::ClaimCell(req, reply) => {
            let res = (|| -> Result<Option<CellLease>, MetaError> {
                let tx = conn
                    .transaction()
                    .map_err(|e| MetaError::Database(e.to_string()))?;

                let now_ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let mut stmt = tx
                    .prepare(
                        r#"SELECT id, manifest_digest, attempt_no, fencing_token 
                        FROM cells 
                        WHERE (state = 'ready' OR (state = 'running' AND lease_expires_at <= ?1)) 
                          AND resource_class = ?2 
                        ORDER BY priority DESC, created_at ASC 
                        LIMIT 1"#,
                    )
                    .map_err(|e| MetaError::Database(e.to_string()))?;

                let found = stmt
                    .query_row(params![now_ts as i64, req.resource_class], |row| {
                        let id_str: String = row.get(0)?;
                        let manifest_str: String = row.get(1)?;
                        let attempt_no: i64 = row.get(2)?;
                        let fencing_token: i64 = row.get(3)?;
                        Ok((id_str, manifest_str, attempt_no as u32, fencing_token as u64))
                    })
                    .ok();
                drop(stmt);

                if let Some((id_str, manifest_str, attempt_no, fencing_token)) = found {
                    let next_attempt = attempt_no + 1;
                    let next_fence = fencing_token + 1;
                    let expires_at = now_ts + req.lease_duration_secs;
                    let cell_id = CellId::from_str(&id_str)
                        .map_err(|e| MetaError::Database(e.to_string()))?;
                    let manifest_digest = Digest::from_str(&manifest_str)
                        .map_err(|e| MetaError::Database(e.to_string()))?;

                    tx.execute(
                        r#"UPDATE cells 
                        SET state = 'running', worker_id = ?1, attempt_no = ?2, fencing_token = ?3, 
                            lease_expires_at = ?4, updated_at = CURRENT_TIMESTAMP 
                        WHERE id = ?5"#,
                        params![
                            req.worker_id.to_hex(),
                            next_attempt as i64,
                            next_fence as i64,
                            expires_at as i64,
                            id_str
                        ],
                    )
                    .map_err(|e| MetaError::Database(e.to_string()))?;
                    tx.commit()
                        .map_err(|e| MetaError::Database(e.to_string()))?;

                    Ok(Some(CellLease {
                        cell_id,
                        attempt_no: next_attempt,
                        fencing_token: next_fence,
                        manifest_digest,
                        lease_expires_at_timestamp: expires_at,
                    }))
                } else {
                    Ok(None)
                }
            })();
            let _ = reply.send(res);
        }
        SqliteWrite::Heartbeat(lease, reply) => {
            let res = (|| -> Result<LeaseStatus, MetaError> {
                let now_ts = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let expires_at = now_ts + 60;

                let rows_affected = conn
                    .execute(
                        "UPDATE cells SET lease_expires_at = ?1 WHERE id = ?2 AND fencing_token = ?3 AND state = 'running'",
                        params![expires_at as i64, lease.cell_id.to_hex(), lease.fencing_token as i64],
                    )
                    .map_err(|e| MetaError::Database(e.to_string()))?;

                if rows_affected > 0 {
                    Ok(LeaseStatus::Active)
                } else {
                    Ok(LeaseStatus::Revoked)
                }
            })();
            let _ = reply.send(res);
        }
        SqliteWrite::PublishArtifacts(lease, artifacts, reply) => {
            let res = (|| -> Result<(), MetaError> {
                let tx = conn
                    .transaction()
                    .map_err(|e| MetaError::Database(e.to_string()))?;
                let token_i64: i64 = tx
                    .query_row(
                        "SELECT fencing_token FROM cells WHERE id = ?1",
                        params![lease.cell_id.to_hex()],
                        |row| row.get(0),
                    )
                    .map_err(|_| MetaError::CellNotFound(lease.cell_id))?;
                let token = token_i64 as u64;

                if token != lease.fencing_token {
                    return Err(MetaError::StaleFence {
                        cell_id: lease.cell_id,
                        attempt_no: lease.attempt_no,
                        token: lease.fencing_token,
                    });
                }

                for art in &artifacts {
                    tx.execute(
                        "INSERT OR IGNORE INTO artifacts (cell_id, digest, kind) VALUES (?1, ?2, ?3)",
                        params![lease.cell_id.to_hex(), art.digest.to_hex(), art.kind],
                    )
                    .map_err(|e| MetaError::Database(e.to_string()))?;
                }
                tx.commit()
                    .map_err(|e| MetaError::Database(e.to_string()))?;
                Ok(())
            })();
            let _ = reply.send(res);
        }
        SqliteWrite::FinalizeAttempt(lease, final_attempt, reply) => {
            let res = (|| -> Result<FinalizeResult, MetaError> {
                let tx = conn
                    .transaction()
                    .map_err(|e| MetaError::Database(e.to_string()))?;
                let (token_i64, state, attempt_no_i64): (i64, String, i64) = tx
                    .query_row(
                        "SELECT fencing_token, state, attempt_no FROM cells WHERE id = ?1",
                        params![lease.cell_id.to_hex()],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                    )
                    .map_err(|_| MetaError::CellNotFound(lease.cell_id))?;
                let token = token_i64 as u64;
                let attempt_no = attempt_no_i64 as u32;

                if token != lease.fencing_token {
                    return Err(MetaError::StaleFence {
                        cell_id: lease.cell_id,
                        attempt_no: lease.attempt_no,
                        token: lease.fencing_token,
                    });
                }
                if state != "running" {
                    return Err(MetaError::AlreadyFinalized(lease.cell_id));
                }

                // Check that completion manifest artifact was pre-published
                let art_count: i64 = tx
                    .query_row(
                        "SELECT COUNT(*) FROM artifacts WHERE cell_id = ?1 AND digest = ?2",
                        params![
                            lease.cell_id.to_hex(),
                            final_attempt.completion_manifest_digest.to_hex()
                        ],
                        |row| row.get(0),
                    )
                    .unwrap_or(0);

                if art_count == 0 {
                    return Err(MetaError::MissingArtifact(
                        final_attempt.completion_manifest_digest,
                    ));
                }

                let next_state = if final_attempt.accepted {
                    "succeeded"
                } else {
                    "failed"
                };
                tx.execute(
                    r#"UPDATE cells 
                    SET state = ?1, accepted_attempt_no = ?2, completion_manifest_digest = ?3, 
                        updated_at = CURRENT_TIMESTAMP 
                    WHERE id = ?4"#,
                    params![
                        next_state,
                        attempt_no,
                        final_attempt.completion_manifest_digest.to_hex(),
                        lease.cell_id.to_hex()
                    ],
                )
                .map_err(|e| MetaError::Database(e.to_string()))?;
                tx.commit()
                    .map_err(|e| MetaError::Database(e.to_string()))?;

                Ok(FinalizeResult {
                    cell_id: lease.cell_id,
                    accepted_attempt_no: attempt_no,
                    state: next_state.to_string(),
                })
            })();
            let _ = reply.send(res);
        }
        SqliteWrite::Promote(req, reply) => {
            let res = (|| -> Result<PromotionResult, MetaError> {
                let tx = conn
                    .transaction()
                    .map_err(|e| MetaError::Database(e.to_string()))?;
                let current_opt: Option<String> = tx
                    .query_row(
                        "SELECT checkpoint_id FROM active_models WHERE role = 'ranker'",
                        [],
                        |row| row.get(0),
                    )
                    .ok();

                match req.active_stable {
                    Some(expected) if current_opt.as_deref() != Some(&expected.to_hex()) => {
                        return Err(MetaError::PromotionRejected {
                            reason: "active stable model changed before promotion".to_string(),
                        });
                    }
                    _ => {}
                }

                tx.execute(
                    r#"INSERT INTO active_models (role, checkpoint_id, updated_at) 
                    VALUES ('ranker', ?1, CURRENT_TIMESTAMP) 
                    ON CONFLICT(role) DO UPDATE SET checkpoint_id = excluded.checkpoint_id, updated_at = CURRENT_TIMESTAMP"#,
                    params![req.candidate.to_hex()],
                )
                .map_err(|e| MetaError::Database(e.to_string()))?;
                tx.commit()
                    .map_err(|e| MetaError::Database(e.to_string()))?;

                let receipt_digest = Digest::hash_blake3(b"sqlite-promotion-receipt");
                Ok(PromotionResult {
                    promoted: true,
                    active: req.candidate,
                    receipt_digest,
                })
            })();
            let _ = reply.send(res);
        }
        SqliteWrite::Checkpoint(reply) => {
            let res = conn
                .pragma_update(None, "wal_checkpoint", "TRUNCATE")
                .map_err(|e| MetaError::Database(e.to_string()));
            let _ = reply.send(res);
        }
    }
}

pub struct SqliteMetaStore {
    path: PathBuf,
    writer_tx: mpsc::Sender<SqliteWrite>,
}

impl SqliteMetaStore {
    pub fn open(path: PathBuf) -> Result<Self, MetaError> {
        let (tx, mut rx) = mpsc::channel::<SqliteWrite>(1024);
        let worker_path = path.clone();

        std::thread::Builder::new()
            .name("reflex-sqlite-writer".into())
            .spawn(move || {
                let mut conn = Connection::open(&worker_path).expect("open sqlite");
                configure(&conn).expect("configure sqlite");
                apply_migrations(&mut conn).expect("migrate sqlite");

                while let Some(cmd) = rx.blocking_recv() {
                    execute_write(&mut conn, cmd);
                }
            })
            .map_err(|e| MetaError::Database(e.to_string()))?;

        Ok(Self {
            path,
            writer_tx: tx,
        })
    }
}

#[async_trait]
impl MetaStore for SqliteMetaStore {
    async fn create_experiment(&self, record: NewExperiment) -> Result<ExperimentId, MetaError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.writer_tx
            .send(SqliteWrite::CreateExperiment(record, reply_tx))
            .await
            .map_err(|_| MetaError::Database("writer actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| MetaError::Database("writer dropped reply".into()))?
    }

    async fn enqueue_cells(&self, cells: &[NewCell]) -> Result<(), MetaError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.writer_tx
            .send(SqliteWrite::EnqueueCells(cells.to_vec(), reply_tx))
            .await
            .map_err(|_| MetaError::Database("writer actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| MetaError::Database("writer dropped reply".into()))?
    }

    async fn claim_cell(&self, request: ClaimRequest) -> Result<Option<CellLease>, MetaError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.writer_tx
            .send(SqliteWrite::ClaimCell(request, reply_tx))
            .await
            .map_err(|_| MetaError::Database("writer actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| MetaError::Database("writer dropped reply".into()))?
    }

    async fn heartbeat(&self, lease: &CellLease) -> Result<LeaseStatus, MetaError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.writer_tx
            .send(SqliteWrite::Heartbeat(lease.clone(), reply_tx))
            .await
            .map_err(|_| MetaError::Database("writer actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| MetaError::Database("writer dropped reply".into()))?
    }

    async fn publish_attempt_artifacts(
        &self,
        lease: &CellLease,
        artifacts: &[ArtifactRef],
    ) -> Result<(), MetaError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.writer_tx
            .send(SqliteWrite::PublishArtifacts(
                lease.clone(),
                artifacts.to_vec(),
                reply_tx,
            ))
            .await
            .map_err(|_| MetaError::Database("writer actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| MetaError::Database("writer dropped reply".into()))?
    }

    async fn finalize_attempt(
        &self,
        lease: &CellLease,
        result: FinalAttempt,
    ) -> Result<FinalizeResult, MetaError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.writer_tx
            .send(SqliteWrite::FinalizeAttempt(
                lease.clone(),
                result,
                reply_tx,
            ))
            .await
            .map_err(|_| MetaError::Database("writer actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| MetaError::Database("writer dropped reply".into()))?
    }

    async fn compare_and_promote(
        &self,
        request: PromotionRequest,
    ) -> Result<PromotionResult, MetaError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.writer_tx
            .send(SqliteWrite::Promote(request, reply_tx))
            .await
            .map_err(|_| MetaError::Database("writer actor closed".into()))?;
        reply_rx
            .await
            .map_err(|_| MetaError::Database("writer dropped reply".into()))?
    }

    async fn get_experiment_status(&self, id: ExperimentId) -> Result<ExperimentStatus, MetaError> {
        let conn =
            Connection::open_with_flags(&self.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| MetaError::Database(e.to_string()))?;

        let (name, domain): (String, String) = conn
            .query_row(
                "SELECT name, domain FROM experiments WHERE id = ?1",
                params![id.to_hex()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| MetaError::ExperimentNotFound(id))?;

        let mut stmt = conn
            .prepare("SELECT state, COUNT(*) FROM cells WHERE experiment_id = ?1 GROUP BY state")
            .map_err(|e| MetaError::Database(e.to_string()))?;

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

        let rows = stmt
            .query_map(params![id.to_hex()], |row| {
                let state: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                Ok((state, count))
            })
            .map_err(|e| MetaError::Database(e.to_string()))?;

        for r in rows {
            let (st, count) = r.map_err(|e| MetaError::Database(e.to_string()))?;
            let count = count as usize;
            status.total_cells += count;
            match st.as_str() {
                "ready" => status.ready_cells += count,
                "running" => status.running_cells += count,
                "succeeded" => status.succeeded_cells += count,
                "failed" => status.failed_cells += count,
                _ => {}
            }
        }

        Ok(status)
    }

    async fn get_cell_manifest(&self, id: CellId) -> Result<Option<Digest>, MetaError> {
        let conn =
            Connection::open_with_flags(&self.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| MetaError::Database(e.to_string()))?;
        let manifest_opt: Option<String> = conn
            .query_row(
                "SELECT manifest_digest FROM cells WHERE id = ?1",
                params![id.to_hex()],
                |row| row.get(0),
            )
            .ok();
        match manifest_opt {
            Some(s) => {
                let digest =
                    Digest::from_str(&s).map_err(|e| MetaError::Database(e.to_string()))?;
                Ok(Some(digest))
            }
            None => Ok(None),
        }
    }

    async fn list_experiments(&self) -> Result<Vec<ExperimentSummary>, MetaError> {
        let conn =
            Connection::open_with_flags(&self.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| MetaError::Database(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT id, name, domain, created_at FROM experiments ORDER BY created_at DESC")
            .map_err(|e| MetaError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let id_str: String = row.get(0)?;
                let name: String = row.get(1)?;
                let domain: String = row.get(2)?;
                let created_at: String = row.get(3)?;
                Ok((id_str, name, domain, created_at))
            })
            .map_err(|e| MetaError::Database(e.to_string()))?;
        let mut results = Vec::new();
        for r in rows {
            let (id_str, name, domain, created_at) =
                r.map_err(|e| MetaError::Database(e.to_string()))?;
            let id = ExperimentId::from_str(&id_str)
                .map_err(|e| MetaError::Database(e.to_string()))?;
            results.push(ExperimentSummary {
                id,
                name,
                domain,
                created_at,
            });
        }
        Ok(results)
    }

    async fn list_cells(&self) -> Result<Vec<CellSummary>, MetaError> {
        let conn =
            Connection::open_with_flags(&self.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| MetaError::Database(e.to_string()))?;
        let mut stmt = conn
            .prepare(
                r#"SELECT id, experiment_id, generation_id, state, resource_class, priority,
                   attempt_no, fencing_token FROM cells ORDER BY created_at DESC"#,
            )
            .map_err(|e| MetaError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let id_str: String = row.get(0)?;
                let exp_str: String = row.get(1)?;
                let gen_str: String = row.get(2)?;
                let state: String = row.get(3)?;
                let resource_class: String = row.get(4)?;
                let priority: i32 = row.get(5)?;
                let attempt_no: i64 = row.get(6)?;
                let fencing_token: i64 = row.get(7)?;
                Ok((
                    id_str, exp_str, gen_str, state, resource_class, priority, attempt_no,
                    fencing_token,
                ))
            })
            .map_err(|e| MetaError::Database(e.to_string()))?;
        let mut results = Vec::new();
        for r in rows {
            let (id_str, exp_str, gen_str, state, resource_class, priority, attempt_no, fencing_token) =
                r.map_err(|e| MetaError::Database(e.to_string()))?;
            let id = CellId::from_str(&id_str)
                .map_err(|e| MetaError::Database(e.to_string()))?;
            let experiment_id = ExperimentId::from_str(&exp_str)
                .map_err(|e| MetaError::Database(e.to_string()))?;
            let generation_id = reflex_types::GenerationId::from_str(&gen_str)
                .map_err(|e| MetaError::Database(e.to_string()))?;
            results.push(CellSummary {
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

    async fn list_active_models(&self) -> Result<Vec<ActiveModel>, MetaError> {
        let conn =
            Connection::open_with_flags(&self.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .map_err(|e| MetaError::Database(e.to_string()))?;
        let mut stmt = conn
            .prepare("SELECT role, checkpoint_id, updated_at FROM active_models")
            .map_err(|e| MetaError::Database(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let role: String = row.get(0)?;
                let checkpoint_id: String = row.get(1)?;
                let updated_at: String = row.get(2)?;
                Ok((role, checkpoint_id, updated_at))
            })
            .map_err(|e| MetaError::Database(e.to_string()))?;
        let mut results = Vec::new();
        for r in rows {
            let (role, checkpoint_id, updated_at) =
                r.map_err(|e| MetaError::Database(e.to_string()))?;
            results.push(ActiveModel {
                role,
                checkpoint_id,
                updated_at,
            });
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_sqlite_meta_store() {
        let temp_dir = tempfile::tempdir().unwrap();
        let db_path = temp_dir.path().join("reflex.db");
        let store = SqliteMetaStore::open(db_path).unwrap();

        let exp_id = ExperimentId::from_digest(Digest::hash_blake3(b"sqlite-exp"));
        store
            .create_experiment(NewExperiment {
                id: exp_id,
                name: "test_sqlite".to_string(),
                domain: "bitvec".to_string(),
                manifest_digest: Digest::hash_blake3(b"manifest"),
            })
            .await
            .unwrap();

        let cell_id = CellId::from_digest(Digest::hash_blake3(b"sqlite-cell"));
        store
            .enqueue_cells(&[NewCell {
                id: cell_id,
                experiment_id: exp_id,
                generation_id: reflex_types::GenerationId::from_digest(Digest::hash_blake3(b"gen")),
                manifest_digest: Digest::hash_blake3(b"cell_man"),
                resource_class: "performance_4x_8gb".to_string(),
                priority: 5,
            }])
            .await
            .unwrap();

        let lease = store
            .claim_cell(ClaimRequest {
                worker_id: reflex_types::WorkerId::from_digest(Digest::hash_blake3(b"w1")),
                resource_class: "performance_4x_8gb".to_string(),
                lease_duration_secs: 60,
            })
            .await
            .unwrap()
            .unwrap();

        assert_eq!(lease.cell_id, cell_id);
        assert_eq!(lease.fencing_token, 1);

        let comp_digest = Digest::hash_blake3(b"comp");
        store
            .publish_attempt_artifacts(
                &lease,
                &[ArtifactRef {
                    digest: comp_digest,
                    kind: "cell-completion-manifest".to_string(),
                }],
            )
            .await
            .unwrap();

        let final_res = store
            .finalize_attempt(
                &lease,
                FinalAttempt {
                    accepted: true,
                    completion_manifest_digest: comp_digest,
                    error_code: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(final_res.state, "succeeded");

        let status = store.get_experiment_status(exp_id).await.unwrap();
        assert_eq!(status.succeeded_cells, 1);
    }
}
