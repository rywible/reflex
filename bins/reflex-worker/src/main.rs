use reflex_bench::HostCalibration;
use reflex_cas::{ArtifactStore, FsArtifactStore, RetentionClass};
use reflex_domain::Domain;
use reflex_domain_bitvec::{BitvecDomain, BitvecTask};
use reflex_meta::{ArtifactRef, CellLease, ClaimRequest, FinalAttempt, MetaStore};
use reflex_meta_sqlite::SqliteMetaStore;
use reflex_scheduler::CellManifest;
use reflex_search::{SearchBudget, SearchKernel, UniformRanker};
use reflex_types::{Digest, ModelCheckpointId, WorkerId};
use std::path::PathBuf;

/// Cell execution result written to CAS as the completion manifest.
#[derive(serde::Serialize, serde::Deserialize)]
struct CompletionManifest {
    cell_id: String,
    attempt_no: u32,
    solved: bool,
    artifact_digest: Option<Digest>,
    verification_digest: Option<Digest>,
    stats: SearchStatsSnapshot,
    error: Option<String>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SearchStatsSnapshot {
    nodes_expanded: u32,
    candidates_scored: u32,
    search_cpu_ns: u64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cal = HostCalibration::calibrate_current_host();
    let worker_id = WorkerId::from_digest(cal.host_fingerprint);

    tracing::info!(
        "reflex-worker {worker_id} ready on host class {}",
        cal.host_class
    );

    let db_path = std::env::var("REFLEX_DB_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".reflex/reflex.db"));
    if !db_path.exists() {
        tracing::info!("No local database found at {}, exiting.", db_path.display());
        return Ok(());
    }

    let meta = SqliteMetaStore::open(db_path)?;
    let cas_dir = std::env::var("REFLEX_CAS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(".reflex"));
    let cas = FsArtifactStore::new(cas_dir)?;

    let claim_req = ClaimRequest {
        worker_id,
        resource_class: cal.host_class.clone(),
        lease_duration_secs: 300,
    };

    tracing::info!("Worker entering claim loop...");

    loop {
        match meta.claim_cell(claim_req.clone()).await? {
            Some(lease) => {
                tracing::info!(
                    "Claimed cell {}, attempt {}",
                    lease.cell_id,
                    lease.attempt_no
                );
                let result = execute_cell(&meta, &cas, &lease).await;
                match result {
                    Ok(()) => {}
                    Err(e) => {
                        tracing::error!("Cell execution failed: {e}");
                        let _ = finalize_failed(&meta, &lease, &e.to_string(), &cas).await;
                    }
                }
            }
            None => {
                tracing::info!("No ready cells available to claim. Worker shutting down.");
                break;
            }
        }
    }

    Ok(())
}

async fn execute_cell<M: MetaStore, A: ArtifactStore>(
    meta: &M,
    cas: &A,
    lease: &CellLease,
) -> Result<(), Box<dyn std::error::Error>> {
    // Load cell manifest from CAS
    let manifest_bytes = cas.get_bytes(lease.manifest_digest).await.map_err(|e| {
        format!(
            "Failed to load cell manifest from CAS (digest {}): {e}",
            lease.manifest_digest
        )
    })?;
    let manifest: CellManifest = serde_json::from_slice(&manifest_bytes).map_err(|e| {
        format!(
            "Failed to deserialize cell manifest: {e}"
        )
    })?;

    tracing::info!(
        "Loaded manifest for cell {}: domain={}",
        lease.cell_id,
        manifest.search.algorithm
    );

    // Create domain
    let domain = BitvecDomain::new();

    // Parse search budget from manifest config
    let budget = SearchBudget {
        action_budget: manifest.search.action_budget,
        node_budget: manifest.search.node_budget,
        cpu_seconds: manifest.search.cpu_seconds,
    };

    // Create ranker with model checkpoint if available
    let model_id = manifest
        .model_checkpoint
        .unwrap_or_else(|| ModelCheckpointId::from_digest(Digest::hash_blake3(b"uniform")));
    let ranker = UniformRanker::new(model_id);

    // Create task from manifest
    let task = BitvecTask {
        initial: reflex_domain_bitvec::BvExpr::Add(
            Box::new(reflex_domain_bitvec::BvExpr::Xor(
                Box::new(reflex_domain_bitvec::BvExpr::Var(0)),
                Box::new(reflex_domain_bitvec::BvExpr::Var(1)),
            )),
            Box::new(reflex_domain_bitvec::BvExpr::Const(0)),
        ),
        target_max_cost: 3,
    };

    // Run search kernel
    let mut kernel = SearchKernel::new(&domain, &ranker, budget);
    let solved_root = kernel.run(&task)?;

    if let Some(root) = solved_root {
        tracing::info!("Search solved! Reconstructing artifact...");

        // Reconstruct artifact from solved root
        let artifact = domain.reconstruct_artifact(root, kernel.episode_arena())?;

        // Verify the artifact
        let verify_budget = reflex_domain::VerifyBudget {
            max_cpu_ns: 100_000_000,
            max_wall_ns: 100_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
        };
        let verification = domain.verify(&artifact, verify_budget)?;

        tracing::info!(
            "Verification: is_equivalent={}",
            verification.is_equivalent
        );

        // Serialize artifact and verification to bytes for CAS storage
        let artifact_json = serde_json::to_vec(&artifact)?;
        let verification_json = serde_json::to_vec(&verification)?;

        // Store artifact in CAS
        let artifact_stored = cas
            .put_bytes(
                None,
                bytes::Bytes::from(artifact_json),
                RetentionClass::Active,
            )
            .await?;

        // Store verification in CAS
        let verification_stored = cas
            .put_bytes(
                None,
                bytes::Bytes::from(verification_json),
                RetentionClass::Active,
            )
            .await?;

        // Build completion manifest
        let stats = kernel.stats();
        let completion = CompletionManifest {
            cell_id: lease.cell_id.to_string(),
            attempt_no: lease.attempt_no,
            solved: verification.is_equivalent,
            artifact_digest: Some(artifact_stored.digest),
            verification_digest: Some(verification_stored.digest),
            stats: SearchStatsSnapshot {
                nodes_expanded: stats.nodes_expanded,
                candidates_scored: stats.candidates_scored,
                search_cpu_ns: stats.search_cpu_ns,
            },
            error: None,
        };

        let completion_json = serde_json::to_vec(&completion)?;
        let completion_stored = cas
            .put_bytes(
                None,
                bytes::Bytes::from(completion_json),
                RetentionClass::Active,
            )
            .await?;

        // Publish artifact references
        meta.publish_attempt_artifacts(
            lease,
            &[
                ArtifactRef {
                    digest: artifact_stored.digest,
                    kind: "bitvec-artifact".to_string(),
                },
                ArtifactRef {
                    digest: verification_stored.digest,
                    kind: "bitvec-verification".to_string(),
                },
                ArtifactRef {
                    digest: completion_stored.digest,
                    kind: "completion-manifest".to_string(),
                },
            ],
        )
        .await?;

        // Finalize attempt as accepted
        let accepted = verification.is_equivalent;
        let res = meta
            .finalize_attempt(
                lease,
                FinalAttempt {
                    accepted,
                    completion_manifest_digest: completion_stored.digest,
                    error_code: if accepted { None } else { Some("verification_failed".to_string()) },
                },
            )
            .await?;
        tracing::info!(
            "Finalized cell {}: state={}, accepted={}",
            res.cell_id,
            res.state,
            accepted
        );
    } else {
        tracing::info!("Search did not find solution within budget.");
        finalize_failed(meta, lease, "search_exhausted", cas).await?;
    }

    Ok(())
}

async fn finalize_failed<M: MetaStore>(
    meta: &M,
    lease: &CellLease,
    error_code: &str,
    cas: &impl ArtifactStore,
) -> Result<(), Box<dyn std::error::Error>> {
    let stats = SearchStatsSnapshot {
        nodes_expanded: 0,
        candidates_scored: 0,
        search_cpu_ns: 0,
    };
    let completion = CompletionManifest {
        cell_id: lease.cell_id.to_string(),
        attempt_no: lease.attempt_no,
        solved: false,
        artifact_digest: None,
        verification_digest: None,
        stats,
        error: Some(error_code.to_string()),
    };

    let completion_json = serde_json::to_vec(&completion)?;
    let completion_stored = cas
        .put_bytes(
            None,
            bytes::Bytes::from(completion_json),
            RetentionClass::Active,
        )
        .await?;

    meta.publish_attempt_artifacts(
        lease,
        &[ArtifactRef {
            digest: completion_stored.digest,
            kind: "completion-manifest".to_string(),
        }],
    )
    .await?;

    let res = meta
        .finalize_attempt(
            lease,
            FinalAttempt {
                accepted: false,
                completion_manifest_digest: completion_stored.digest,
                error_code: Some(error_code.to_string()),
            },
        )
        .await?;
    tracing::info!(
        "Finalized cell {} as failed: state={}, error={}",
        res.cell_id,
        res.state,
        error_code
    );
    Ok(())
}
