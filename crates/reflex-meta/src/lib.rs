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
    #[error("generation not found: {0}")]
    GenerationNotFound(GenerationId),
    #[error("generation state changed: expected {expected:?}, found {actual:?}")]
    GenerationStateConflict {
        expected: DurableGenerationState,
        actual: DurableGenerationState,
    },
    #[error("invalid generation transition: {reason}")]
    InvalidGenerationTransition { reason: String },
    #[error("generation command id {0} was reused with different content")]
    GenerationCommandConflict(Digest),
    #[error("invalid cell control request: {reason}")]
    InvalidCellControl { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewExperiment {
    pub id: ExperimentId,
    pub name: String,
    pub domain: String,
    pub manifest_digest: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewCell {
    pub id: CellId,
    pub experiment_id: ExperimentId,
    pub generation_id: GenerationId,
    pub manifest_digest: Digest,
    pub resource_class: String,
    pub priority: i32,
}

pub fn validate_new_experiment(record: &NewExperiment) -> Result<(), MetaError> {
    if record.id == ExperimentId::from_digest(Digest::ZERO)
        || record.manifest_digest == Digest::ZERO
        || record.name.trim().is_empty()
        || record.name.len() > 256
        || record.name.chars().any(char::is_control)
        || record.domain.trim().is_empty()
        || record.domain.len() > 256
        || record.domain.chars().any(char::is_control)
    {
        return Err(MetaError::InvalidCellControl {
            reason: "experiment requires non-zero identities and bounded non-empty name/domain"
                .into(),
        });
    }
    Ok(())
}

pub fn validate_new_cells(cells: &[NewCell]) -> Result<(), MetaError> {
    if cells.is_empty() {
        return Err(MetaError::InvalidCellControl {
            reason: "cell enqueue batch must not be empty".into(),
        });
    }
    let mut ids = HashMap::new();
    for cell in cells {
        if cell.id == CellId::from_digest(Digest::ZERO)
            || cell.experiment_id == ExperimentId::from_digest(Digest::ZERO)
            || cell.generation_id == GenerationId::from_digest(Digest::ZERO)
            || cell.manifest_digest == Digest::ZERO
            || cell.resource_class.trim().is_empty()
            || cell.resource_class.len() > 128
            || cell.resource_class.chars().any(char::is_control)
            || ids.insert(cell.id, ()).is_some()
        {
            return Err(MetaError::InvalidCellControl {
                reason: "cells require unique non-zero identities and bounded resource classes"
                    .into(),
            });
        }
    }
    Ok(())
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

pub fn validate_claim_request(request: &ClaimRequest) -> Result<(), MetaError> {
    if request.worker_id == WorkerId::from_digest(Digest::ZERO)
        || request.resource_class.trim().is_empty()
        || request.resource_class.len() > 128
        || request.resource_class.chars().any(char::is_control)
        || !(1..=86_400).contains(&request.lease_duration_secs)
    {
        return Err(MetaError::InvalidCellControl {
            reason: "claim requires a non-zero worker, bounded resource class, and lease duration in 1..=86400 seconds".into(),
        });
    }
    Ok(())
}

pub fn validate_cell_lease(lease: &CellLease) -> Result<(), MetaError> {
    if lease.cell_id == CellId::from_digest(Digest::ZERO)
        || lease.manifest_digest == Digest::ZERO
        || lease.attempt_no == 0
        || lease.fencing_token == 0
        || lease.lease_expires_at_timestamp == 0
    {
        return Err(MetaError::InvalidCellControl {
            reason: "lease requires non-zero cell, manifest, attempt, fencing, and expiration identities".into(),
        });
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaseStatus {
    Active,
    Revoked,
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CancellationMode {
    /// Cancel queued work and allow currently owned leases to publish normally.
    Drain,
    /// Cancel queued work and fence every running attempt immediately.
    Kill,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationCancellationRequest {
    pub generation_id: GenerationId,
    pub mode: CancellationMode,
    pub cause: String,
    pub operator_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationCancellationResult {
    pub queued_cancelled: u64,
    pub running_fenced: u64,
    pub running_draining: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationReconcileRequest {
    pub generation_id: GenerationId,
    pub observed_at_timestamp: u64,
    pub max_infrastructure_attempts: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationReconcileResult {
    pub retried: u64,
    pub attempts_exhausted: u64,
    pub still_owned: u64,
}

pub fn validate_reconcile_request(request: &GenerationReconcileRequest) -> Result<(), MetaError> {
    if request.generation_id == GenerationId::from_digest(Digest::ZERO)
        || request.observed_at_timestamp == 0
        || request.max_infrastructure_attempts == 0
    {
        return Err(MetaError::InvalidCellControl {
            reason: "generation, observation time, and infrastructure retry bound must be non-zero"
                .into(),
        });
    }
    Ok(())
}

pub fn validate_cancellation_request(
    request: &GenerationCancellationRequest,
) -> Result<(), MetaError> {
    if request.generation_id == GenerationId::from_digest(Digest::ZERO)
        || request.cause.trim().is_empty()
        || request.operator_id.trim().is_empty()
    {
        return Err(MetaError::InvalidCellControl {
            reason: "generation, cause, and operator must be non-empty".into(),
        });
    }
    Ok(())
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

pub fn validate_artifact_refs(artifacts: &[ArtifactRef]) -> Result<(), MetaError> {
    if artifacts.is_empty() {
        return Err(MetaError::InvalidCellControl {
            reason: "artifact publication must not be empty".into(),
        });
    }
    let mut identities = HashMap::new();
    for artifact in artifacts {
        if artifact.digest == Digest::ZERO
            || artifact.kind.trim().is_empty()
            || artifact.kind.len() > 128
            || artifact.kind.chars().any(char::is_control)
        {
            return Err(MetaError::InvalidCellControl {
                reason: "artifacts require non-zero digests and bounded non-empty kinds".into(),
            });
        }
        if identities
            .insert(artifact.digest, artifact.kind.as_str())
            .is_some()
        {
            return Err(MetaError::InvalidCellControl {
                reason: format!("duplicate artifact digest {}", artifact.digest),
            });
        }
    }
    Ok(())
}

pub fn validate_final_attempt(result: &FinalAttempt) -> Result<(), MetaError> {
    let error_valid = match (&result.error_code, result.accepted) {
        (None, true) => true,
        (Some(code), false) => {
            !code.trim().is_empty() && code.len() <= 128 && !code.chars().any(char::is_control)
        }
        _ => false,
    };
    if result.completion_manifest_digest == Digest::ZERO || !error_valid {
        return Err(MetaError::InvalidCellControl {
            reason: "final attempt requires a non-zero completion manifest and exactly one accepted-or-error outcome".into(),
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FinalizeResult {
    pub cell_id: CellId,
    pub accepted_attempt_no: u32,
    pub state: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionRequest {
    pub candidate: ModelCheckpointId,
    pub active_stable: Option<ModelCheckpointId>,
    pub evaluation_report_digest: Digest,
    pub generation_id: GenerationId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionResult {
    pub promoted: bool,
    pub active: ModelCheckpointId,
    pub receipt_digest: Digest,
}

/// One metadata transaction which both advances the generation and swaps the
/// authoritative stable-model pointer. This prevents a process death between
/// those writes from producing a promoted generation with the old model (or
/// the inverse).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationPromotionRequest {
    pub transition: GenerationTransitionCommand,
    pub promotion: PromotionRequest,
    pub policy_digest: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationPromotionResult {
    pub generation: GenerationRecord,
    pub promotion: PromotionResult,
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
    pub incomplete_cells: usize,
    pub cancelled_cells: usize,
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

/// Durable counterpart of the scheduler state machine. Kept in the metadata
/// crate so persistence backends do not depend on scheduler implementation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableGenerationState {
    Bootstrap,
    Collecting,
    Verifying,
    CompilingDataset,
    Training,
    Evaluating,
    PromotionPending,
    Promoted,
    Rejected,
    Stopped,
    Failed,
}

impl DurableGenerationState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bootstrap => "bootstrap",
            Self::Collecting => "collecting",
            Self::Verifying => "verifying",
            Self::CompilingDataset => "compiling_dataset",
            Self::Training => "training",
            Self::Evaluating => "evaluating",
            Self::PromotionPending => "promotion_pending",
            Self::Promoted => "promoted",
            Self::Rejected => "rejected",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Result<Self, MetaError> {
        match value {
            "bootstrap" => Ok(Self::Bootstrap),
            "collecting" => Ok(Self::Collecting),
            "verifying" => Ok(Self::Verifying),
            "compiling_dataset" => Ok(Self::CompilingDataset),
            "training" => Ok(Self::Training),
            "evaluating" => Ok(Self::Evaluating),
            "promotion_pending" => Ok(Self::PromotionPending),
            "promoted" => Ok(Self::Promoted),
            "rejected" => Ok(Self::Rejected),
            "stopped" => Ok(Self::Stopped),
            "failed" => Ok(Self::Failed),
            other => Err(MetaError::Database(format!(
                "unknown durable generation state {other}"
            ))),
        }
    }

    pub const fn permits(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Bootstrap, Self::Collecting)
                | (Self::Collecting, Self::Verifying)
                | (Self::Verifying, Self::CompilingDataset)
                | (Self::CompilingDataset, Self::Training)
                | (Self::Training, Self::Evaluating)
                | (
                    Self::Evaluating,
                    Self::PromotionPending | Self::Promoted | Self::Rejected
                )
                | (Self::PromotionPending, Self::Promoted | Self::Rejected)
                | (
                    Self::Promoted | Self::Rejected,
                    Self::Collecting | Self::Stopped
                )
                | (_, Self::Failed | Self::Stopped)
        ) || self as u8 == next as u8
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationArtifactRole {
    CollectionManifest,
    AcceptedEvidence,
    Dataset,
    Checkpoint,
    EvaluationReport,
    AcceptedEvaluation,
    PromotionReceipt,
    StopReceipt,
    FailureReport,
    Other(String),
}

impl GenerationArtifactRole {
    pub fn as_str(&self) -> &str {
        match self {
            Self::CollectionManifest => "collection_manifest",
            Self::AcceptedEvidence => "accepted_evidence",
            Self::Dataset => "dataset",
            Self::Checkpoint => "checkpoint",
            Self::EvaluationReport => "evaluation_report",
            Self::AcceptedEvaluation => "accepted_evaluation",
            Self::PromotionReceipt => "promotion_receipt",
            Self::StopReceipt => "stop_receipt",
            Self::FailureReport => "failure_report",
            Self::Other(value) => value,
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "collection_manifest" => Self::CollectionManifest,
            "accepted_evidence" => Self::AcceptedEvidence,
            "dataset" => Self::Dataset,
            "checkpoint" => Self::Checkpoint,
            "evaluation_report" => Self::EvaluationReport,
            "accepted_evaluation" => Self::AcceptedEvaluation,
            "promotion_receipt" => Self::PromotionReceipt,
            "stop_receipt" => Self::StopReceipt,
            "failure_report" => Self::FailureReport,
            other => Self::Other(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GenerationArtifactRef {
    pub digest: Digest,
    pub role: GenerationArtifactRole,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationFailure {
    pub code: String,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewGenerationRecord {
    pub id: GenerationId,
    pub experiment_id: ExperimentId,
    pub ordinal: u32,
    pub cause: String,
    pub operator_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationTransitionCommand {
    /// Stable command identity makes retry after an ambiguous commit idempotent.
    pub command_id: Digest,
    pub generation_id: GenerationId,
    pub expected: DurableGenerationState,
    pub next: DurableGenerationState,
    pub artifacts: Vec<GenerationArtifactRef>,
    pub cause: String,
    pub operator_id: String,
    pub failure: Option<GenerationFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationTransitionRecord {
    pub revision: u64,
    pub command: GenerationTransitionCommand,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationRecord {
    pub id: GenerationId,
    pub experiment_id: ExperimentId,
    pub ordinal: u32,
    pub state: DurableGenerationState,
    pub revision: u64,
    pub artifacts: Vec<GenerationArtifactRef>,
    pub cause: String,
    pub operator_id: String,
    pub failure: Option<GenerationFailure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationCursor {
    pub ordinal: u32,
    pub id: GenerationId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationPage {
    pub records: Vec<GenerationRecord>,
    pub next: Option<GenerationCursor>,
}

pub fn validate_new_generation(record: &NewGenerationRecord) -> Result<(), MetaError> {
    if record.id == GenerationId::from_digest(Digest::ZERO)
        || record.experiment_id == ExperimentId::from_digest(Digest::ZERO)
    {
        return Err(MetaError::InvalidGenerationTransition {
            reason: "generation and experiment identities must be non-zero".into(),
        });
    }
    if record.cause.trim().is_empty() || record.operator_id.trim().is_empty() {
        return Err(MetaError::InvalidGenerationTransition {
            reason: "cause and operator identity must be non-empty".into(),
        });
    }
    Ok(())
}

pub fn validate_generation_identity(
    existing: &GenerationRecord,
    requested: &NewGenerationRecord,
) -> Result<(), MetaError> {
    if existing.id != requested.id
        || existing.experiment_id != requested.experiment_id
        || existing.ordinal != requested.ordinal
    {
        return Err(MetaError::InvalidGenerationTransition {
            reason: "generation identity was reused with different experiment or ordinal".into(),
        });
    }
    Ok(())
}

pub fn validate_generation_transition(
    current: &GenerationRecord,
    command: &GenerationTransitionCommand,
) -> Result<(), MetaError> {
    if command.command_id == Digest::ZERO
        || command.generation_id == GenerationId::from_digest(Digest::ZERO)
    {
        return Err(MetaError::InvalidGenerationTransition {
            reason: "command and generation identities must be non-zero".into(),
        });
    }
    validate_generation_artifacts(&command.artifacts)?;
    if command.generation_id != current.id {
        return Err(MetaError::GenerationNotFound(command.generation_id));
    }
    if command.expected != current.state {
        return Err(MetaError::GenerationStateConflict {
            expected: command.expected,
            actual: current.state,
        });
    }
    if !current.state.permits(command.next) {
        return Err(MetaError::InvalidGenerationTransition {
            reason: format!(
                "{} -> {} is not permitted",
                current.state.as_str(),
                command.next.as_str()
            ),
        });
    }
    if command.cause.trim().is_empty() || command.operator_id.trim().is_empty() {
        return Err(MetaError::InvalidGenerationTransition {
            reason: "cause and operator identity must be non-empty".into(),
        });
    }
    if command.next == DurableGenerationState::Failed {
        let failure =
            command
                .failure
                .as_ref()
                .ok_or_else(|| MetaError::InvalidGenerationTransition {
                    reason: "failed transition requires terminal failure details".into(),
                })?;
        if failure.code.trim().is_empty() || failure.detail.trim().is_empty() {
            return Err(MetaError::InvalidGenerationTransition {
                reason: "terminal failure code and detail must be non-empty".into(),
            });
        }
    } else if command.failure.is_some() {
        return Err(MetaError::InvalidGenerationTransition {
            reason: "failure details are only valid for failed transitions".into(),
        });
    }
    let has_role = |role: GenerationArtifactRole| {
        current
            .artifacts
            .iter()
            .chain(&command.artifacts)
            .any(|artifact| artifact.role == role)
    };
    let required_roles: &[GenerationArtifactRole] = match command.next {
        DurableGenerationState::Verifying => &[GenerationArtifactRole::CollectionManifest],
        DurableGenerationState::CompilingDataset => &[GenerationArtifactRole::AcceptedEvidence],
        DurableGenerationState::Training => &[GenerationArtifactRole::Dataset],
        DurableGenerationState::Evaluating => &[GenerationArtifactRole::Checkpoint],
        DurableGenerationState::PromotionPending | DurableGenerationState::Promoted => &[
            GenerationArtifactRole::Checkpoint,
            GenerationArtifactRole::AcceptedEvaluation,
        ],
        DurableGenerationState::Rejected => &[GenerationArtifactRole::EvaluationReport],
        DurableGenerationState::Stopped => &[GenerationArtifactRole::StopReceipt],
        DurableGenerationState::Failed => &[GenerationArtifactRole::FailureReport],
        _ => &[],
    };
    for role in required_roles {
        if !has_role(role.clone()) {
            return Err(MetaError::InvalidGenerationTransition {
                reason: format!(
                    "{} transition requires {} artifact",
                    command.next.as_str(),
                    role.as_str()
                ),
            });
        }
    }
    Ok(())
}

pub fn validate_generation_promotion(
    current: &GenerationRecord,
    request: &GenerationPromotionRequest,
) -> Result<Digest, MetaError> {
    let transition = &request.transition;
    let promotion = &request.promotion;
    if transition.expected != DurableGenerationState::PromotionPending
        || transition.next != DurableGenerationState::Promoted
        || transition.generation_id != promotion.generation_id
        || request.policy_digest == Digest::ZERO
    {
        return Err(MetaError::InvalidGenerationTransition {
            reason: "atomic promotion requires promotion_pending -> promoted with matching non-zero identities".into(),
        });
    }
    let receipt = reflex_ml_core::promotion_receipt_digest(
        &promotion.candidate,
        promotion.active_stable.as_ref(),
        &promotion.evaluation_report_digest,
        promotion.generation_id.digest(),
        &request.policy_digest,
    );
    let has = |role: GenerationArtifactRole, digest: Digest| {
        current
            .artifacts
            .iter()
            .chain(&transition.artifacts)
            .any(|artifact| artifact.role == role && artifact.digest == digest)
    };
    if !has(
        GenerationArtifactRole::Checkpoint,
        *promotion.candidate.digest(),
    ) || !has(
        GenerationArtifactRole::AcceptedEvaluation,
        promotion.evaluation_report_digest,
    ) || !has(GenerationArtifactRole::PromotionReceipt, receipt)
    {
        return Err(MetaError::InvalidGenerationTransition {
            reason: "atomic promotion must name the candidate checkpoint, accepted evaluation, and derived promotion receipt".into(),
        });
    }
    validate_generation_transition(current, transition)?;
    Ok(receipt)
}

fn validate_generation_artifacts(artifacts: &[GenerationArtifactRef]) -> Result<(), MetaError> {
    let mut references = std::collections::HashSet::new();
    let mut singleton_roles = std::collections::HashSet::new();
    for artifact in artifacts {
        if artifact.digest == Digest::ZERO {
            return Err(MetaError::InvalidGenerationTransition {
                reason: "generation artifact digest must be non-zero".into(),
            });
        }
        let role = artifact.role.as_str().to_string();
        if role.trim().is_empty() {
            return Err(MetaError::InvalidGenerationTransition {
                reason: "generation artifact role must be non-empty".into(),
            });
        }
        if matches!(&artifact.role, GenerationArtifactRole::Other(_))
            && GenerationArtifactRole::parse(&role) != artifact.role.clone()
        {
            return Err(MetaError::InvalidGenerationTransition {
                reason: format!("custom generation artifact role {role} is reserved"),
            });
        }
        if !references.insert((artifact.digest, role.clone())) {
            return Err(MetaError::InvalidGenerationTransition {
                reason: format!(
                    "duplicate generation artifact {} with role {role}",
                    artifact.digest
                ),
            });
        }
        if matches!(
            &artifact.role,
            GenerationArtifactRole::CollectionManifest
                | GenerationArtifactRole::Dataset
                | GenerationArtifactRole::Checkpoint
                | GenerationArtifactRole::EvaluationReport
                | GenerationArtifactRole::AcceptedEvaluation
                | GenerationArtifactRole::PromotionReceipt
                | GenerationArtifactRole::StopReceipt
                | GenerationArtifactRole::FailureReport
        ) && !singleton_roles.insert(role.clone())
        {
            return Err(MetaError::InvalidGenerationTransition {
                reason: format!("generation artifact role {role} must be unique"),
            });
        }
    }
    Ok(())
}

pub fn apply_generation_transition(
    current: &GenerationRecord,
    command: &GenerationTransitionCommand,
) -> Result<GenerationRecord, MetaError> {
    validate_generation_transition(current, command)?;
    let mut next = current.clone();
    next.state = command.next;
    next.revision =
        next.revision
            .checked_add(1)
            .ok_or_else(|| MetaError::InvalidGenerationTransition {
                reason: "generation revision overflow".into(),
            })?;
    for artifact in &command.artifacts {
        if !next.artifacts.contains(artifact) {
            next.artifacts.push(artifact.clone());
        }
    }
    validate_generation_artifacts(&next.artifacts)?;
    next.artifacts.sort_by(|left, right| {
        (left.role.as_str(), left.digest.to_hex())
            .cmp(&(right.role.as_str(), right.digest.to_hex()))
    });
    next.cause.clone_from(&command.cause);
    next.operator_id.clone_from(&command.operator_id);
    next.failure.clone_from(&command.failure);
    Ok(next)
}

pub fn validate_generation_page_limit(limit: usize) -> Result<(), MetaError> {
    if limit == 0 || limit > 10_000 {
        return Err(MetaError::InvalidGenerationTransition {
            reason: "generation page limit must be in 1..=10000".into(),
        });
    }
    Ok(())
}

#[async_trait]
pub trait MetaStore: Send + Sync {
    async fn create_experiment(&self, record: NewExperiment) -> Result<ExperimentId, MetaError>;
    async fn enqueue_cells(&self, cells: &[NewCell]) -> Result<(), MetaError>;
    async fn claim_cell(&self, request: ClaimRequest) -> Result<Option<CellLease>, MetaError>;
    async fn heartbeat(&self, lease: &CellLease) -> Result<LeaseStatus, MetaError>;
    async fn reconcile_generation_cells(
        &self,
        request: GenerationReconcileRequest,
    ) -> Result<GenerationReconcileResult, MetaError>;
    async fn cancel_generation_cells(
        &self,
        request: GenerationCancellationRequest,
    ) -> Result<GenerationCancellationResult, MetaError>;
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
    async fn commit_generation_promotion(
        &self,
        request: GenerationPromotionRequest,
    ) -> Result<GenerationPromotionResult, MetaError>;
    async fn get_experiment_status(&self, id: ExperimentId) -> Result<ExperimentStatus, MetaError>;
    async fn get_cell_manifest(&self, id: CellId) -> Result<Option<Digest>, MetaError>;
    async fn list_experiments(&self) -> Result<Vec<ExperimentSummary>, MetaError>;
    async fn list_cells(&self) -> Result<Vec<CellSummary>, MetaError>;
    async fn list_active_models(&self) -> Result<Vec<ActiveModel>, MetaError>;
    async fn create_generation(
        &self,
        record: NewGenerationRecord,
    ) -> Result<GenerationRecord, MetaError>;
    async fn compare_and_set_generation(
        &self,
        command: GenerationTransitionCommand,
    ) -> Result<GenerationRecord, MetaError>;
    async fn get_generation(&self, id: GenerationId)
    -> Result<Option<GenerationRecord>, MetaError>;
    async fn list_generations(
        &self,
        experiment_id: ExperimentId,
        after: Option<GenerationCursor>,
        limit: usize,
    ) -> Result<GenerationPage, MetaError>;
    async fn list_generation_transitions(
        &self,
        generation_id: GenerationId,
        after_revision: u64,
        limit: usize,
    ) -> Result<Vec<GenerationTransitionRecord>, MetaError>;
}

#[derive(Default)]
pub struct MemoryMetaStore {
    experiments: RwLock<HashMap<ExperimentId, NewExperiment>>,
    cells: RwLock<HashMap<CellId, MemoryCellState>>,
    active_model: RwLock<Option<ModelCheckpointId>>,
    generations: RwLock<HashMap<GenerationId, MemoryGenerationState>>,
}

struct MemoryGenerationState {
    record: GenerationRecord,
    commands: HashMap<Digest, GenerationTransitionRecord>,
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
        validate_new_experiment(&record)?;
        let id = record.id;
        let mut experiments = self.experiments.write().await;
        if let Some(existing) = experiments.get(&id) {
            if existing == &record {
                return Ok(id);
            }
            return Err(MetaError::InvalidCellControl {
                reason: format!("experiment identity {id} was reused with different content"),
            });
        }
        experiments.insert(id, record);
        Ok(id)
    }

    async fn enqueue_cells(&self, cells: &[NewCell]) -> Result<(), MetaError> {
        validate_new_cells(cells)?;
        let mut guard = self.cells.write().await;
        for cell in cells {
            if let Some(existing) = guard.get(&cell.id)
                && existing.cell != *cell
            {
                return Err(MetaError::InvalidCellControl {
                    reason: format!(
                        "cell identity {} was reused with different content",
                        cell.id
                    ),
                });
            }
        }
        for c in cells {
            guard.entry(c.id).or_insert_with(|| MemoryCellState {
                cell: c.clone(),
                state: "ready".to_string(),
                worker_id: None,
                attempt_no: 0,
                fencing_token: 0,
                lease_expires_at: 0,
                artifacts: Vec::new(),
                completion_manifest: None,
            });
        }
        Ok(())
    }

    async fn claim_cell(&self, request: ClaimRequest) -> Result<Option<CellLease>, MetaError> {
        validate_claim_request(&request)?;
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

        let next_attempt =
            state
                .attempt_no
                .checked_add(1)
                .ok_or_else(|| MetaError::InvalidCellControl {
                    reason: "attempt number overflow".into(),
                })?;
        let next_fencing_token =
            state
                .fencing_token
                .checked_add(1)
                .ok_or_else(|| MetaError::InvalidCellControl {
                    reason: "fencing token overflow".into(),
                })?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| MetaError::InvalidCellControl {
                reason: format!("system clock precedes Unix epoch: {error}"),
            })?
            .as_secs();
        let lease_expires_at = now
            .checked_add(request.lease_duration_secs)
            .ok_or_else(|| MetaError::InvalidCellControl {
                reason: "lease expiration overflows timestamp range".into(),
            })?;

        state.state = "running".to_string();
        state.worker_id = Some(request.worker_id);
        state.attempt_no = next_attempt;
        state.fencing_token = next_fencing_token;
        state.lease_expires_at = lease_expires_at;
        state.artifacts.clear();

        Ok(Some(CellLease {
            cell_id: *cell_id,
            attempt_no: state.attempt_no,
            fencing_token: state.fencing_token,
            manifest_digest: state.cell.manifest_digest,
            lease_expires_at_timestamp: state.lease_expires_at,
        }))
    }

    async fn heartbeat(&self, lease: &CellLease) -> Result<LeaseStatus, MetaError> {
        validate_cell_lease(lease)?;
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

    async fn reconcile_generation_cells(
        &self,
        request: GenerationReconcileRequest,
    ) -> Result<GenerationReconcileResult, MetaError> {
        validate_reconcile_request(&request)?;
        let mut result = GenerationReconcileResult::default();
        let mut cells = self.cells.write().await;
        if cells.values().any(|state| {
            state.cell.generation_id == request.generation_id
                && state.state == "running"
                && state.lease_expires_at <= request.observed_at_timestamp
                && state.fencing_token == u64::MAX
        }) {
            return Err(MetaError::InvalidCellControl {
                reason: "fencing token overflow while reconciling generation".into(),
            });
        }
        for state in cells
            .values_mut()
            .filter(|state| state.cell.generation_id == request.generation_id)
            .filter(|state| state.state == "running")
        {
            if state.lease_expires_at > request.observed_at_timestamp {
                result.still_owned += 1;
                continue;
            }
            state.worker_id = None;
            state.lease_expires_at = 0;
            state.fencing_token += 1;
            if state.attempt_no < request.max_infrastructure_attempts {
                state.state = "ready".into();
                result.retried += 1;
            } else {
                state.state = "incomplete".into();
                result.attempts_exhausted += 1;
            }
        }
        Ok(result)
    }

    async fn cancel_generation_cells(
        &self,
        request: GenerationCancellationRequest,
    ) -> Result<GenerationCancellationResult, MetaError> {
        validate_cancellation_request(&request)?;
        let mut result = GenerationCancellationResult::default();
        let mut cells = self.cells.write().await;
        if request.mode == CancellationMode::Kill
            && cells.values().any(|state| {
                state.cell.generation_id == request.generation_id
                    && state.state == "running"
                    && state.fencing_token == u64::MAX
            })
        {
            return Err(MetaError::InvalidCellControl {
                reason: "fencing token overflow while cancelling generation".into(),
            });
        }
        for state in cells
            .values_mut()
            .filter(|state| state.cell.generation_id == request.generation_id)
        {
            if state.state == "ready" {
                state.state = "cancelled".into();
                result.queued_cancelled += 1;
            } else if state.state == "running" {
                match request.mode {
                    CancellationMode::Drain => result.running_draining += 1,
                    CancellationMode::Kill => {
                        state.state = "cancelled".into();
                        state.worker_id = None;
                        state.lease_expires_at = 0;
                        state.fencing_token += 1;
                        result.running_fenced += 1;
                    }
                }
            }
        }
        Ok(result)
    }

    async fn publish_attempt_artifacts(
        &self,
        lease: &CellLease,
        artifacts: &[ArtifactRef],
    ) -> Result<(), MetaError> {
        validate_cell_lease(lease)?;
        validate_artifact_refs(artifacts)?;
        let mut guard = self.cells.write().await;
        let state = guard
            .get_mut(&lease.cell_id)
            .ok_or(MetaError::CellNotFound(lease.cell_id))?;
        if state.fencing_token != lease.fencing_token
            || state.attempt_no != lease.attempt_no
            || state.state != "running"
        {
            return Err(MetaError::StaleFence {
                cell_id: lease.cell_id,
                attempt_no: lease.attempt_no,
                token: lease.fencing_token,
            });
        }
        for artifact in artifacts {
            if let Some(existing) = state
                .artifacts
                .iter()
                .find(|existing| existing.digest == artifact.digest)
            {
                if existing.kind != artifact.kind {
                    return Err(MetaError::InvalidCellControl {
                        reason: format!(
                            "artifact {} was already published with kind `{}`",
                            artifact.digest, existing.kind
                        ),
                    });
                }
            } else {
                state.artifacts.push(artifact.clone());
            }
        }
        Ok(())
    }

    async fn finalize_attempt(
        &self,
        lease: &CellLease,
        result: FinalAttempt,
    ) -> Result<FinalizeResult, MetaError> {
        validate_cell_lease(lease)?;
        validate_final_attempt(&result)?;
        let mut guard = self.cells.write().await;
        let state = guard
            .get_mut(&lease.cell_id)
            .ok_or(MetaError::CellNotFound(lease.cell_id))?;
        if state.fencing_token != lease.fencing_token || state.attempt_no != lease.attempt_no {
            return Err(MetaError::StaleFence {
                cell_id: lease.cell_id,
                attempt_no: lease.attempt_no,
                token: lease.fencing_token,
            });
        }
        if state.state != "running" {
            return Err(MetaError::AlreadyFinalized(lease.cell_id));
        }
        if !state.artifacts.iter().any(|artifact| {
            artifact.digest == result.completion_manifest_digest
                && artifact.kind == "cell-completion-manifest"
        }) {
            return Err(MetaError::MissingArtifact(
                result.completion_manifest_digest,
            ));
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
        let receipt_digest = reflex_ml_core::promotion_receipt_digest(
            &request.candidate,
            request.active_stable.as_ref(),
            &request.evaluation_report_digest,
            request.generation_id.digest(),
            &Digest::hash_blake3(b"promotion-policy-v1"),
        );
        Ok(PromotionResult {
            promoted: true,
            active: request.candidate,
            receipt_digest,
        })
    }

    async fn commit_generation_promotion(
        &self,
        request: GenerationPromotionRequest,
    ) -> Result<GenerationPromotionResult, MetaError> {
        let mut generations = self.generations.write().await;
        let state = generations
            .get_mut(&request.transition.generation_id)
            .ok_or(MetaError::GenerationNotFound(
                request.transition.generation_id,
            ))?;
        let mut active = self.active_model.write().await;
        if let Some(previous) = state.commands.get(&request.transition.command_id) {
            if previous.command != request.transition {
                return Err(MetaError::GenerationCommandConflict(
                    request.transition.command_id,
                ));
            }
            if *active != Some(request.promotion.candidate) {
                return Err(MetaError::PromotionRejected {
                    reason: "committed promotion command disagrees with active model".into(),
                });
            }
            let receipt_digest =
                validate_generation_promotion(&state.record, &request).or_else(|error| {
                    if state.record.state == DurableGenerationState::Promoted {
                        Ok(reflex_ml_core::promotion_receipt_digest(
                            &request.promotion.candidate,
                            request.promotion.active_stable.as_ref(),
                            &request.promotion.evaluation_report_digest,
                            request.promotion.generation_id.digest(),
                            &request.policy_digest,
                        ))
                    } else {
                        Err(error)
                    }
                })?;
            return Ok(GenerationPromotionResult {
                generation: state.record.clone(),
                promotion: PromotionResult {
                    promoted: true,
                    active: request.promotion.candidate,
                    receipt_digest,
                },
            });
        }
        match request.promotion.active_stable {
            Some(expected) if *active != Some(expected) => {
                return Err(MetaError::PromotionRejected {
                    reason: "active stable model changed before promotion".into(),
                });
            }
            _ => {}
        }
        let receipt_digest = validate_generation_promotion(&state.record, &request)?;
        let next = apply_generation_transition(&state.record, &request.transition)?;
        state.commands.insert(
            request.transition.command_id,
            GenerationTransitionRecord {
                revision: next.revision,
                command: request.transition,
            },
        );
        state.record = next.clone();
        *active = Some(request.promotion.candidate);
        Ok(GenerationPromotionResult {
            generation: next,
            promotion: PromotionResult {
                promoted: true,
                active: request.promotion.candidate,
                receipt_digest,
            },
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
            incomplete_cells: 0,
            cancelled_cells: 0,
        };
        for (_, c) in cells.iter().filter(|(_, c)| c.cell.experiment_id == id) {
            status.total_cells += 1;
            match c.state.as_str() {
                "ready" => status.ready_cells += 1,
                "running" => status.running_cells += 1,
                "succeeded" => status.succeeded_cells += 1,
                "failed" => status.failed_cells += 1,
                "incomplete" => status.incomplete_cells += 1,
                "cancelled" => status.cancelled_cells += 1,
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
            .values()
            .map(|c| CellSummary {
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

    async fn create_generation(
        &self,
        record: NewGenerationRecord,
    ) -> Result<GenerationRecord, MetaError> {
        validate_new_generation(&record)?;
        if !self
            .experiments
            .read()
            .await
            .contains_key(&record.experiment_id)
        {
            return Err(MetaError::ExperimentNotFound(record.experiment_id));
        }
        let mut generations = self.generations.write().await;
        if let Some(existing) = generations.get(&record.id) {
            validate_generation_identity(&existing.record, &record)?;
            return Ok(existing.record.clone());
        }
        let snapshot = GenerationRecord {
            id: record.id,
            experiment_id: record.experiment_id,
            ordinal: record.ordinal,
            state: DurableGenerationState::Bootstrap,
            revision: 0,
            artifacts: Vec::new(),
            cause: record.cause,
            operator_id: record.operator_id,
            failure: None,
        };
        generations.insert(
            snapshot.id,
            MemoryGenerationState {
                record: snapshot.clone(),
                commands: HashMap::new(),
            },
        );
        Ok(snapshot)
    }

    async fn compare_and_set_generation(
        &self,
        command: GenerationTransitionCommand,
    ) -> Result<GenerationRecord, MetaError> {
        let mut generations = self.generations.write().await;
        let state = generations
            .get_mut(&command.generation_id)
            .ok_or(MetaError::GenerationNotFound(command.generation_id))?;
        if let Some(previous) = state.commands.get(&command.command_id) {
            if previous.command == command {
                return Ok(state.record.clone());
            }
            return Err(MetaError::GenerationCommandConflict(command.command_id));
        }
        let next = apply_generation_transition(&state.record, &command)?;
        state.commands.insert(
            command.command_id,
            GenerationTransitionRecord {
                revision: next.revision,
                command,
            },
        );
        state.record = next.clone();
        Ok(next)
    }

    async fn get_generation(
        &self,
        id: GenerationId,
    ) -> Result<Option<GenerationRecord>, MetaError> {
        Ok(self
            .generations
            .read()
            .await
            .get(&id)
            .map(|state| state.record.clone()))
    }

    async fn list_generations(
        &self,
        experiment_id: ExperimentId,
        after: Option<GenerationCursor>,
        limit: usize,
    ) -> Result<GenerationPage, MetaError> {
        validate_generation_page_limit(limit)?;
        let generations = self.generations.read().await;
        let mut records: Vec<_> = generations
            .values()
            .map(|state| &state.record)
            .filter(|record| record.experiment_id == experiment_id)
            .filter(|record| {
                after.is_none_or(|cursor| {
                    (record.ordinal, record.id.to_hex()) > (cursor.ordinal, cursor.id.to_hex())
                })
            })
            .cloned()
            .collect();
        records.sort_by_key(|record| (record.ordinal, record.id.to_hex()));
        records.truncate(limit);
        let next = records.last().map(|record| GenerationCursor {
            ordinal: record.ordinal,
            id: record.id,
        });
        Ok(GenerationPage { records, next })
    }

    async fn list_generation_transitions(
        &self,
        generation_id: GenerationId,
        after_revision: u64,
        limit: usize,
    ) -> Result<Vec<GenerationTransitionRecord>, MetaError> {
        validate_generation_page_limit(limit)?;
        let generations = self.generations.read().await;
        let state = generations
            .get(&generation_id)
            .ok_or(MetaError::GenerationNotFound(generation_id))?;
        let mut records: Vec<_> = state
            .commands
            .values()
            .filter(|record| record.revision > after_revision)
            .cloned()
            .collect();
        records.sort_by_key(|record| record.revision);
        records.truncate(limit);
        Ok(records)
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
            resource_class: "reference-4vcpu-8gb".to_string(),
            priority: 10,
        }])
        .await
        .unwrap();

        let worker_id = WorkerId::from_digest(Digest::hash_blake3(b"worker"));
        let lease = meta
            .claim_cell(ClaimRequest {
                worker_id,
                resource_class: "reference-4vcpu-8gb".to_string(),
                lease_duration_secs: 60,
            })
            .await
            .unwrap()
            .unwrap();

        assert_eq!(lease.cell_id, cell_id);
        assert_eq!(lease.fencing_token, 1);

        let status = meta.heartbeat(&lease).await.unwrap();
        assert_eq!(status, LeaseStatus::Active);

        let completion = Digest::hash_blake3(b"completion");
        meta.publish_attempt_artifacts(
            &lease,
            &[ArtifactRef {
                digest: completion,
                kind: "cell-completion-manifest".into(),
            }],
        )
        .await
        .unwrap();
        let final_res = meta
            .finalize_attempt(
                &lease,
                FinalAttempt {
                    accepted: true,
                    completion_manifest_digest: completion,
                    error_code: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(final_res.state, "succeeded");
    }

    #[tokio::test]
    async fn expired_attempts_retry_with_fencing_and_kill_cancels_claims() {
        let meta = MemoryMetaStore::new();
        let experiment_id = ExperimentId::from_digest(Digest::hash_blake3(b"reconcile-exp"));
        let generation_id = GenerationId::from_digest(Digest::hash_blake3(b"reconcile-gen"));
        meta.create_experiment(NewExperiment {
            id: experiment_id,
            name: "reconcile".into(),
            domain: "fixture".into(),
            manifest_digest: Digest::hash_blake3(b"manifest"),
        })
        .await
        .unwrap();
        let cell_id = CellId::from_digest(Digest::hash_blake3(b"reconcile-cell"));
        meta.enqueue_cells(&[NewCell {
            id: cell_id,
            experiment_id,
            generation_id,
            manifest_digest: Digest::hash_blake3(b"cell-manifest"),
            resource_class: "local".into(),
            priority: 0,
        }])
        .await
        .unwrap();
        let first = meta
            .claim_cell(ClaimRequest {
                worker_id: WorkerId::from_digest(Digest::hash_blake3(b"worker-1")),
                resource_class: "local".into(),
                lease_duration_secs: 1,
            })
            .await
            .unwrap()
            .unwrap();
        let stale_completion = Digest::hash_blake3(b"stale-completion");
        meta.publish_attempt_artifacts(
            &first,
            &[ArtifactRef {
                digest: stale_completion,
                kind: "cell-completion-manifest".into(),
            }],
        )
        .await
        .unwrap();
        let reconciled = meta
            .reconcile_generation_cells(GenerationReconcileRequest {
                generation_id,
                observed_at_timestamp: first.lease_expires_at_timestamp + 1,
                max_infrastructure_attempts: 2,
            })
            .await
            .unwrap();
        assert_eq!(reconciled.retried, 1);
        assert_eq!(meta.heartbeat(&first).await.unwrap(), LeaseStatus::Revoked);
        let second = meta
            .claim_cell(ClaimRequest {
                worker_id: WorkerId::from_digest(Digest::hash_blake3(b"worker-2")),
                resource_class: "local".into(),
                lease_duration_secs: 60,
            })
            .await
            .unwrap()
            .unwrap();
        assert!(second.fencing_token > first.fencing_token);
        assert!(matches!(
            meta.finalize_attempt(
                &second,
                FinalAttempt {
                    accepted: true,
                    completion_manifest_digest: stale_completion,
                    error_code: None,
                },
            )
            .await,
            Err(MetaError::MissingArtifact(digest)) if digest == stale_completion
        ));
        let cancelled = meta
            .cancel_generation_cells(GenerationCancellationRequest {
                generation_id,
                mode: CancellationMode::Kill,
                cause: "operator requested".into(),
                operator_id: "test".into(),
            })
            .await
            .unwrap();
        assert_eq!(cancelled.running_fenced, 1);
        assert_eq!(meta.heartbeat(&second).await.unwrap(), LeaseStatus::Revoked);
    }

    fn artifact(role: GenerationArtifactRole, name: &[u8]) -> GenerationArtifactRef {
        GenerationArtifactRef {
            digest: Digest::hash_blake3(name),
            role,
        }
    }

    #[tokio::test]
    async fn durable_generation_cas_is_restartable_and_fail_closed() {
        let meta = MemoryMetaStore::new();
        let experiment_id = ExperimentId::from_digest(Digest::hash_blake3(b"generation-exp"));
        let generation_id = GenerationId::from_digest(Digest::hash_blake3(b"generation"));
        assert!(
            meta.create_generation(NewGenerationRecord {
                id: GenerationId::from_digest(Digest::ZERO),
                experiment_id,
                ordinal: 0,
                cause: "invalid".into(),
                operator_id: "scheduler".into(),
            })
            .await
            .is_err()
        );
        meta.create_experiment(NewExperiment {
            id: experiment_id,
            name: "generation experiment".into(),
            domain: "fixture".into(),
            manifest_digest: Digest::hash_blake3(b"generation-manifest"),
        })
        .await
        .unwrap();
        meta.create_generation(NewGenerationRecord {
            id: generation_id,
            experiment_id,
            ordinal: 1,
            cause: "experiment created".into(),
            operator_id: "scheduler".into(),
        })
        .await
        .unwrap();
        let collecting = GenerationTransitionCommand {
            command_id: Digest::hash_blake3(b"collect"),
            generation_id,
            expected: DurableGenerationState::Bootstrap,
            next: DurableGenerationState::Collecting,
            artifacts: vec![],
            cause: "begin collection".into(),
            operator_id: "scheduler".into(),
            failure: None,
        };
        let first = meta
            .compare_and_set_generation(collecting.clone())
            .await
            .unwrap();
        let replay = meta.compare_and_set_generation(collecting).await.unwrap();
        assert_eq!(first, replay);
        assert_eq!(replay.revision, 1);
        let mut conflicting_replay = replay.clone();
        conflicting_replay.cause = "different payload".into();
        let conflict = GenerationTransitionCommand {
            command_id: Digest::hash_blake3(b"collect"),
            generation_id,
            expected: DurableGenerationState::Collecting,
            next: DurableGenerationState::Collecting,
            artifacts: vec![],
            cause: conflicting_replay.cause,
            operator_id: "scheduler".into(),
            failure: None,
        };
        assert!(matches!(
            meta.compare_and_set_generation(conflict).await,
            Err(MetaError::GenerationCommandConflict(_))
        ));

        let promotion = GenerationTransitionCommand {
            command_id: Digest::hash_blake3(b"premature-promotion"),
            generation_id,
            expected: DurableGenerationState::Collecting,
            next: DurableGenerationState::Promoted,
            artifacts: vec![artifact(GenerationArtifactRole::Checkpoint, b"checkpoint")],
            cause: "promote".into(),
            operator_id: "scheduler".into(),
            failure: None,
        };
        assert!(matches!(
            meta.compare_and_set_generation(promotion).await,
            Err(MetaError::InvalidGenerationTransition { .. })
        ));

        let evaluating = GenerationRecord {
            id: generation_id,
            experiment_id,
            ordinal: 1,
            state: DurableGenerationState::Evaluating,
            revision: 5,
            artifacts: vec![artifact(GenerationArtifactRole::Checkpoint, b"checkpoint")],
            cause: "evaluation running".into(),
            operator_id: "scheduler".into(),
            failure: None,
        };
        assert!(matches!(
            apply_generation_transition(
                &evaluating,
                &GenerationTransitionCommand {
                    command_id: Digest::hash_blake3(b"missing-evaluation"),
                    generation_id,
                    expected: DurableGenerationState::Evaluating,
                    next: DurableGenerationState::Promoted,
                    artifacts: vec![],
                    cause: "invalid promotion".into(),
                    operator_id: "promotion-gate".into(),
                    failure: None,
                }
            ),
            Err(MetaError::InvalidGenerationTransition { .. })
        ));

        assert!(matches!(
            meta.compare_and_set_generation(GenerationTransitionCommand {
                command_id: Digest::hash_blake3(b"zero-artifact"),
                generation_id,
                expected: DurableGenerationState::Collecting,
                next: DurableGenerationState::Failed,
                artifacts: vec![GenerationArtifactRef {
                    digest: Digest::ZERO,
                    role: GenerationArtifactRole::FailureReport,
                }],
                cause: "invalid failure".into(),
                operator_id: "scheduler".into(),
                failure: Some(GenerationFailure {
                    code: "invalid".into(),
                    detail: "zero artifact".into(),
                }),
            })
            .await,
            Err(MetaError::InvalidGenerationTransition { .. })
        ));

        let failed = meta
            .compare_and_set_generation(GenerationTransitionCommand {
                command_id: Digest::hash_blake3(b"failed"),
                generation_id,
                expected: DurableGenerationState::Collecting,
                next: DurableGenerationState::Failed,
                artifacts: vec![artifact(GenerationArtifactRole::FailureReport, b"failure")],
                cause: "worker exhaustion".into(),
                operator_id: "scheduler".into(),
                failure: Some(GenerationFailure {
                    code: "resource_exhausted".into(),
                    detail: "declared generation budget exhausted".into(),
                }),
            })
            .await
            .unwrap();
        assert_eq!(failed.state, DurableGenerationState::Failed);
        assert_eq!(
            meta.get_generation(generation_id).await.unwrap(),
            Some(failed)
        );
    }
}
