use async_trait::async_trait;
use reflex_types::{CellId, Digest, ExperimentId, GenerationId, ModelCheckpointId, WorkerId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum MetaError {
    #[error("experiment not found: {0}")]
    ExperimentNotFound(ExperimentId),
    #[error("cell not found: {0}")]
    CellNotFound(CellId),
    #[error("stale fencing token {token} for cell {cell_id}, attempt {attempt_no}")]
    StaleFence {
        cell_id: CellId,
        attempt_no: u32,
        token: u64,
    },
    #[error("lease expired or revoked for cell {0}")]
    LeaseExpired(CellId),
    #[error("attempt already finalized for cell {0}")]
    AlreadyFinalized(CellId),
    #[error("database error: {0}")]
    Database(String),
    #[error("missing required CAS artifact {0}")]
    MissingArtifact(Digest),
    #[error("promotion rejected: {reason}")]
    PromotionRejected { reason: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewExperiment {
    pub id: ExperimentId,
    pub name: String,
    pub domain: String,
    pub manifest_digest: Digest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewCell {
    pub id: CellId,
    pub experiment_id: ExperimentId,
    pub generation_id: GenerationId,
    pub manifest_digest: Digest,
    pub resource_class: String,
    pub priority: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClaimRequest {
    pub worker_id: WorkerId,
    pub resource_class: String,
    pub lease_duration_secs: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CellLease {
    pub cell_id: CellId,
    pub attempt_no: u32,
    pub fencing_token: u64,
    pub manifest_digest: Digest,
    pub lease_expires_at_timestamp: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaseStatus {
    Active,
    Revoked,
    Expired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub digest: Digest,
    pub kind: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinalAttempt {
    pub accepted: bool,
    pub completion_manifest_digest: Digest,
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinalizeResult {
    pub cell_id: CellId,
    pub accepted_attempt_no: u32,
    pub state: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromotionRequest {
    pub candidate: ModelCheckpointId,
    pub active_stable: Option<ModelCheckpointId>,
    pub evaluation_report_digest: Digest,
    pub generation_id: GenerationId,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromotionResult {
    pub promoted: bool,
    pub active: ModelCheckpointId,
    pub receipt_digest: Digest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentStatus {
    pub id: ExperimentId,
    pub name: String,
    pub domain: String,
    pub total_cells: usize,
    pub ready_cells: usize,
    pub running_cells: usize,
    pub succeeded_cells: usize,
    pub failed_cells: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentSummary {
    pub id: ExperimentId,
    pub name: String,
    pub domain: String,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CellSummary {
    pub id: CellId,
    pub experiment_id: ExperimentId,
    pub generation_id: GenerationId,
    pub state: String,
    pub resource_class: String,
    pub priority: i32,
    pub attempt_no: u32,
    pub fencing_token: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActiveModel {
    pub role: String,
    pub checkpoint_id: String,
    pub updated_at: String,
}

#[async_trait]
pub trait MetaStore: Send + Sync {
    async fn create_experiment(&self, record: NewExperiment) -> Result<ExperimentId, MetaError>;
    async fn enqueue_cells(&self, cells: &[NewCell]) -> Result<(), MetaError>;
    async fn claim_cell(&self, request: ClaimRequest) -> Result<Option<CellLease>, MetaError>;
    async fn heartbeat(&self, lease: &CellLease) -> Result<LeaseStatus, MetaError>;
    async fn publish_attempt_artifacts(
        &self,
        lease: &CellLease,
        artifacts: &[ArtifactRef],
    ) -> Result<(), MetaError>;
    async fn finalize_attempt(
        &self,
        lease: &CellLease,
        result: FinalAttempt,
    ) -> Result<FinalizeResult, MetaError>;
    async fn compare_and_promote(
        &self,
        request: PromotionRequest,
    ) -> Result<PromotionResult, MetaError>;
    async fn get_experiment_status(&self, id: ExperimentId) -> Result<ExperimentStatus, MetaError>;
    async fn get_cell_manifest(&self, id: CellId) -> Result<Option<Digest>, MetaError>;
    async fn list_experiments(&self) -> Result<Vec<ExperimentSummary>, MetaError>;
    async fn list_cells(&self) -> Result<Vec<CellSummary>, MetaError>;
    async fn list_active_models(&self) -> Result<Vec<ActiveModel>, MetaError>;
}

#[derive(Default)]
pub struct MemoryMetaStore {
    experiments: RwLock<HashMap<ExperimentId, NewExperiment>>,
    cells: RwLock<HashMap<CellId, MemoryCellState>>,
    active_model: RwLock<Option<ModelCheckpointId>>,
}

struct MemoryCellState {
    cell: NewCell,
    state: String, // "ready", "running", "succeeded", "failed"
    worker_id: Option<WorkerId>,
    attempt_no: u32,
    fencing_token: u64,
    lease_expires_at: u64,
    artifacts: Vec<ArtifactRef>,
    completion_manifest: Option<Digest>,
}

impl MemoryMetaStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MetaStore for MemoryMetaStore {
    async fn create_experiment(&self, record: NewExperiment) -> Result<ExperimentId, MetaError> {
        let id = record.id;
        self.experiments.write().await.insert(id, record);
        Ok(id)
    }

    async fn enqueue_cells(&self, cells: &[NewCell]) -> Result<(), MetaError> {
        let mut guard = self.cells.write().await;
        for c in cells {
            guard.insert(
                c.id,
                MemoryCellState {
                    cell: c.clone(),
                    state: "ready".to_string(),
                    worker_id: None,
                    attempt_no: 0,
                    fencing_token: 0,
                    lease_expires_at: 0,
                    artifacts: Vec::new(),
                    completion_manifest: None,
                },
            );
        }
        Ok(())
    }

    async fn claim_cell(&self, request: ClaimRequest) -> Result<Option<CellLease>, MetaError> {
        let mut guard = self.cells.write().await;
        let mut ready: Vec<_> = guard
            .iter_mut()
            .filter(|(_, state)| {
                state.state == "ready" && state.cell.resource_class == request.resource_class
            })
            .collect();

        if ready.is_empty() {
            return Ok(None);
        }

        ready.sort_by_key(|b| std::cmp::Reverse(b.1.cell.priority));
        let (cell_id, state) = ready.remove(0);

        state.state = "running".to_string();
        state.worker_id = Some(request.worker_id);
        state.attempt_no += 1;
        state.fencing_token += 1;
        state.lease_expires_at = 9999999999;

        Ok(Some(CellLease {
            cell_id: *cell_id,
            attempt_no: state.attempt_no,
            fencing_token: state.fencing_token,
            manifest_digest: state.cell.manifest_digest,
            lease_expires_at_timestamp: state.lease_expires_at,
        }))
    }

    async fn heartbeat(&self, lease: &CellLease) -> Result<LeaseStatus, MetaError> {
        let guard = self.cells.read().await;
        match guard.get(&lease.cell_id) {
            Some(state)
                if state.fencing_token == lease.fencing_token && state.state == "running" =>
            {
                Ok(LeaseStatus::Active)
            }
            _ => Ok(LeaseStatus::Revoked),
        }
    }

    async fn publish_attempt_artifacts(
        &self,
        lease: &CellLease,
        artifacts: &[ArtifactRef],
    ) -> Result<(), MetaError> {
        let mut guard = self.cells.write().await;
        let state = guard
            .get_mut(&lease.cell_id)
            .ok_or(MetaError::CellNotFound(lease.cell_id))?;
        if state.fencing_token != lease.fencing_token {
            return Err(MetaError::StaleFence {
                cell_id: lease.cell_id,
                attempt_no: lease.attempt_no,
                token: lease.fencing_token,
            });
        }
        state.artifacts.extend_from_slice(artifacts);
        Ok(())
    }

    async fn finalize_attempt(
        &self,
        lease: &CellLease,
        result: FinalAttempt,
    ) -> Result<FinalizeResult, MetaError> {
        let mut guard = self.cells.write().await;
        let state = guard
            .get_mut(&lease.cell_id)
            .ok_or(MetaError::CellNotFound(lease.cell_id))?;
        if state.fencing_token != lease.fencing_token {
            return Err(MetaError::StaleFence {
                cell_id: lease.cell_id,
                attempt_no: lease.attempt_no,
                token: lease.fencing_token,
            });
        }
        if state.state != "running" {
            return Err(MetaError::AlreadyFinalized(lease.cell_id));
        }

        state.state = if result.accepted {
            "succeeded".to_string()
        } else {
            "failed".to_string()
        };
        state.completion_manifest = Some(result.completion_manifest_digest);

        Ok(FinalizeResult {
            cell_id: lease.cell_id,
            accepted_attempt_no: state.attempt_no,
            state: state.state.clone(),
        })
    }

    async fn compare_and_promote(
        &self,
        request: PromotionRequest,
    ) -> Result<PromotionResult, MetaError> {
        let mut active = self.active_model.write().await;
        match request.active_stable {
            Some(expected_active) if *active != Some(expected_active) => {
                return Err(MetaError::PromotionRejected {
                    reason: "active stable model changed before promotion".to_string(),
                });
            }
            _ => {}
        }
        *active = Some(request.candidate);
        let receipt_digest = Digest::hash_blake3(b"promotion-receipt");
        Ok(PromotionResult {
            promoted: true,
            active: request.candidate,
            receipt_digest,
        })
    }

    async fn get_experiment_status(&self, id: ExperimentId) -> Result<ExperimentStatus, MetaError> {
        let exp = self
            .experiments
            .read()
            .await
            .get(&id)
            .cloned()
            .ok_or(MetaError::ExperimentNotFound(id))?;
        let cells = self.cells.read().await;
        let mut status = ExperimentStatus {
            id,
            name: exp.name,
            domain: exp.domain,
            total_cells: 0,
            ready_cells: 0,
            running_cells: 0,
            succeeded_cells: 0,
            failed_cells: 0,
        };
        for (_, c) in cells.iter().filter(|(_, c)| c.cell.experiment_id == id) {
            status.total_cells += 1;
            match c.state.as_str() {
                "ready" => status.ready_cells += 1,
                "running" => status.running_cells += 1,
                "succeeded" => status.succeeded_cells += 1,
                "failed" => status.failed_cells += 1,
                _ => {}
            }
        }
        Ok(status)
    }

    async fn get_cell_manifest(&self, id: CellId) -> Result<Option<Digest>, MetaError> {
        let cells = self.cells.read().await;
        Ok(cells.get(&id).map(|c| c.cell.manifest_digest))
    }

    async fn list_experiments(&self) -> Result<Vec<ExperimentSummary>, MetaError> {
        let exps = self.experiments.read().await;
        Ok(exps
            .values()
            .map(|e| ExperimentSummary {
                id: e.id,
                name: e.name.clone(),
                domain: e.domain.clone(),
                created_at: String::new(),
            })
            .collect())
    }

    async fn list_cells(&self) -> Result<Vec<CellSummary>, MetaError> {
        let cells = self.cells.read().await;
        Ok(cells
            .iter()
            .map(|(_, c)| CellSummary {
                id: c.cell.id,
                experiment_id: c.cell.experiment_id,
                generation_id: c.cell.generation_id,
                state: c.state.clone(),
                resource_class: c.cell.resource_class.clone(),
                priority: c.cell.priority,
                attempt_no: c.attempt_no,
                fencing_token: c.fencing_token,
            })
            .collect())
    }

    async fn list_active_models(&self) -> Result<Vec<ActiveModel>, MetaError> {
        let active = self.active_model.read().await;
        Ok(active
            .as_ref()
            .map(|mp| ActiveModel {
                role: "ranker".to_string(),
                checkpoint_id: mp.to_hex(),
                updated_at: String::new(),
            })
            .into_iter()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_meta_store_conformance() {
        let meta = MemoryMetaStore::new();
        let exp_id = ExperimentId::from_digest(Digest::hash_blake3(b"exp"));
        meta.create_experiment(NewExperiment {
            id: exp_id,
            name: "test_exp".to_string(),
            domain: "bitvec".to_string(),
            manifest_digest: Digest::hash_blake3(b"exp_manifest"),
        })
        .await
        .unwrap();

        let cell_id = CellId::from_digest(Digest::hash_blake3(b"cell"));
        meta.enqueue_cells(&[NewCell {
            id: cell_id,
            experiment_id: exp_id,
            generation_id: GenerationId::from_digest(Digest::hash_blake3(b"gen")),
            manifest_digest: Digest::hash_blake3(b"cell_manifest"),
            resource_class: "performance_4x_8gb".to_string(),
            priority: 10,
        }])
        .await
        .unwrap();

        let worker_id = WorkerId::from_digest(Digest::hash_blake3(b"worker"));
        let lease = meta
            .claim_cell(ClaimRequest {
                worker_id,
                resource_class: "performance_4x_8gb".to_string(),
                lease_duration_secs: 60,
            })
            .await
            .unwrap()
            .unwrap();

        assert_eq!(lease.cell_id, cell_id);
        assert_eq!(lease.fencing_token, 1);

        let status = meta.heartbeat(&lease).await.unwrap();
        assert_eq!(status, LeaseStatus::Active);

        let final_res = meta
            .finalize_attempt(
                &lease,
                FinalAttempt {
                    accepted: true,
                    completion_manifest_digest: Digest::hash_blake3(b"completion"),
                    error_code: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(final_res.state, "succeeded");
    }
}
