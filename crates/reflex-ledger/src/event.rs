//! Event families (§8.4) and their compact, versioned binary encoding (P3.1, P3.2).
//!
//! Format v1 (schema_version 1, little-endian, deterministic):
//!
//! ```text
//! every event inside a block payload:
//!   uvarint  seq_delta   (delta from the previous event's sequence; the first
//!                         event in a block is relative to BlockHeader.first_sequence)
//!   u8       type_tag
//!   fields...
//! ```
//!
//! Field codecs:
//!   u8/u16/u32/u64/f32/f64 : fixed little-endian
//!   bool                    : u8 0|1
//!   uvarint                 : LEB128
//!   string                  : uvarint byte length + UTF-8 bytes
//!   Option<string>          : u8 0|1 + string
//!   id32 (any typed digest id) : 32 bytes raw blake3 digest (algorithm fixed by
//!                                the segment schema version; no per-id tag byte)
//!   Digest                  : u8 algorithm (0=blake3, 1=sha256) + 32 bytes
//!
//! PolicyScoreEvent is the high-cardinality "candidate score" event. Rows encode
//! `u64 candidate-prefix + f32 score` = 12 bytes per candidate, where the prefix
//! is the first 8 bytes of the candidate's blake3 digest. Decoding resolves the
//! prefix against the full CandidateIds carried by CandidateBatchEvent instances
//! earlier in the same segment (explicit causal reference, §8.4 / P3.4 step 4).
//! A prefix that matches zero or multiple candidates is a decode error, never a
//! silent mis-resolution. This keeps a candidate score row at 12 bytes — under
//! the 32-byte-per-candidate gate (P3.1 performance acceptance) — while full
//! durable IDs live in the batch definition event.

use crate::LedgerError;
use crate::codec::{Dec, Enc};
use reflex_types::{
    ArtifactId, CandidateId, CellId, Digest, EpisodeId, ExperimentId, FeatureSchemaId,
    GenerationId, MetricId, ModelCheckpointId, ResearchNodeId, StateId, TaskId, UnitId, VerifierId,
    WorkerId,
};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Type tags — part of the wire format; never renumber.
// ---------------------------------------------------------------------------

pub(crate) const TAG_EXPERIMENT_LIFECYCLE: u8 = 0;
pub(crate) const TAG_CELL_LIFECYCLE: u8 = 1;
pub(crate) const TAG_WORKER_SESSION: u8 = 2;
pub(crate) const TAG_TASK_START: u8 = 3;
pub(crate) const TAG_TASK_END: u8 = 4;
pub(crate) const TAG_EPISODE_START: u8 = 5;
pub(crate) const TAG_EPISODE_END: u8 = 6;
pub(crate) const TAG_STATE_DISCOVERY: u8 = 7;
pub(crate) const TAG_CANDIDATE_BATCH: u8 = 8;
pub(crate) const TAG_FEATURE_REF: u8 = 9;
pub(crate) const TAG_POLICY_SCORE: u8 = 10;
pub(crate) const TAG_CANDIDATE_APPLICATION: u8 = 11;
pub(crate) const TAG_CACHE_OBSERVATION: u8 = 12;
pub(crate) const TAG_ARTIFACT_CONSTRUCTION: u8 = 13;
pub(crate) const TAG_VERIFICATION_RECEIPT: u8 = 14;
pub(crate) const TAG_UTILITY_OBSERVATION: u8 = 15;
pub(crate) const TAG_MODEL_SHADOW_SCORE: u8 = 16;
pub(crate) const TAG_RESOURCE_SAMPLE: u8 = 17;
pub(crate) const TAG_LINEAGE_EDGE: u8 = 18;
pub(crate) const TAG_INCIDENT: u8 = 19;
pub(crate) const TAG_GENERATION_LIFECYCLE: u8 = 20;
pub(crate) const TAG_ATTEMPT_LIFECYCLE: u8 = 21;
pub(crate) const TAG_CANCELLATION: u8 = 22;

#[cfg(test)]
pub(crate) const EVENT_TAG_COUNT: u8 = 23;

/// Per-candidate row size of PolicyScoreEvent in format v1 (8-byte digest
/// prefix + f32 score). Under the 32-byte-per-candidate gate (P3.1).
pub const CANDIDATE_ROW_BYTES: usize = 12;

// ---------------------------------------------------------------------------
// Event families — §8.4
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExperimentLifecycleEvent {
    pub experiment_id: ExperimentId,
    pub action: String,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellLifecycleEvent {
    pub cell_id: CellId,
    pub experiment_id: ExperimentId,
    pub generation_id: GenerationId,
    pub action: String,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GenerationLifecycleEvent {
    pub generation_id: GenerationId,
    pub cell_id: CellId,
    pub experiment_id: ExperimentId,
    pub action: String,
    pub outcome: Option<String>,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AttemptLifecycleEvent {
    /// Per-cell attempt number (attempt_no semantics); start/end/accept/reject.
    pub attempt_id: u64,
    pub cell_id: CellId,
    pub generation_id: GenerationId,
    pub action: String,
    pub outcome: Option<String>,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerSessionEvent {
    pub worker_id: WorkerId,
    pub action: String,
    pub calibration_data: Vec<u8>,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskStartEvent {
    pub task_id: TaskId,
    pub cell_id: CellId,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskEndEvent {
    pub task_id: TaskId,
    pub cell_id: CellId,
    pub outcome: String,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EpisodeStartEvent {
    pub episode_id: EpisodeId,
    pub task_id: TaskId,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EpisodeEndEvent {
    pub episode_id: EpisodeId,
    pub status: String,
    pub actions_taken: u32,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateDiscoveryEvent {
    pub state_id: StateId,
    pub episode_id: EpisodeId,
    pub depth: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateBatchEvent {
    pub state_id: StateId,
    pub candidate_ids: Vec<CandidateId>,
    pub classes: Vec<u16>,
    pub tie_breaks: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureRefEvent {
    pub state_id: StateId,
    pub feature_schema: FeatureSchemaId,
    pub payload_handle: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyScoreEvent {
    pub state_id: StateId,
    pub model_id: ModelCheckpointId,
    pub candidate_ids: Vec<CandidateId>,
    pub scores: Vec<f32>,
    pub selected_candidate: CandidateId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateApplicationEvent {
    pub state_id: StateId,
    pub candidate_id: CandidateId,
    pub and_child_states: Vec<StateId>,
    pub outcome: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CacheObservationEvent {
    pub state_id: StateId,
    pub hit: bool,
    pub cache_key: Digest,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArtifactConstructionEvent {
    pub artifact_id: ArtifactId,
    pub episode_id: EpisodeId,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerificationReceiptEvent {
    pub artifact_id: ArtifactId,
    pub verifier: VerifierId,
    pub status: String,
    pub cpu_ns: u64,
    pub wall_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UtilityObservationEvent {
    pub subject: ResearchNodeId,
    pub metric: MetricId,
    pub value: f64,
    pub unit: UnitId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelShadowScoreEvent {
    pub model_id: ModelCheckpointId,
    pub state_id: StateId,
    pub candidate_id: CandidateId,
    pub score: f32,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct ResourceSampleEvent {
    pub user_cpu_ns: u64,
    pub sys_cpu_ns: u64,
    pub rss_bytes: u64,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineageEdgeEvent {
    pub parent: ResearchNodeId,
    pub child: ResearchNodeId,
    pub edge_type: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Default)]
pub struct IncidentEvent {
    pub severity: String,
    pub description: String,
    pub episode_id: Option<EpisodeId>,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CancellationTarget {
    Episode(EpisodeId),
    Cell(CellId),
    Attempt { cell_id: CellId, attempt_id: u64 },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CancellationEvent {
    pub target: CancellationTarget,
    pub reason: String,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    ExperimentLifecycle(ExperimentLifecycleEvent),
    CellLifecycle(CellLifecycleEvent),
    GenerationLifecycle(GenerationLifecycleEvent),
    AttemptLifecycle(AttemptLifecycleEvent),
    WorkerSession(WorkerSessionEvent),
    TaskStart(TaskStartEvent),
    TaskEnd(TaskEndEvent),
    EpisodeStart(EpisodeStartEvent),
    EpisodeEnd(EpisodeEndEvent),
    StateDiscovery(StateDiscoveryEvent),
    CandidateBatch(CandidateBatchEvent),
    FeatureRef(FeatureRefEvent),
    PolicyScore(PolicyScoreEvent),
    CandidateApplication(CandidateApplicationEvent),
    CacheObservation(CacheObservationEvent),
    ArtifactConstruction(ArtifactConstructionEvent),
    VerificationReceipt(VerificationReceiptEvent),
    UtilityObservation(UtilityObservationEvent),
    ModelShadowScore(ModelShadowScoreEvent),
    ResourceSample(ResourceSampleEvent),
    LineageEdge(LineageEdgeEvent),
    Incident(IncidentEvent),
    Cancellation(CancellationEvent),
}

impl Event {
    const MAX_TEXT_BYTES: usize = 16 * 1024;
    const MAX_VECTOR_ITEMS: usize = 65_536;
    const MAX_BLOB_BYTES: usize = 1024 * 1024;

    /// Validate evidence identities, geometry, finite values, and allocation
    /// bounds before an event crosses the durable ledger boundary.
    pub fn validate(&self) -> Result<(), LedgerError> {
        let nonzero = |field: &str, digest: &Digest| {
            if *digest == Digest::ZERO {
                Err(LedgerError::Encoding(format!(
                    "{field} must have a non-zero identity"
                )))
            } else {
                Ok(())
            }
        };
        let text = |field: &str, value: &str| {
            if value.trim().is_empty()
                || value.len() > Self::MAX_TEXT_BYTES
                || value.chars().any(char::is_control)
            {
                Err(LedgerError::Encoding(format!(
                    "{field} must be non-empty, bounded text without control characters"
                )))
            } else {
                Ok(())
            }
        };
        match self {
            Self::ExperimentLifecycle(event) => {
                nonzero("experiment_id", event.experiment_id.digest())?;
                text("action", &event.action)
            }
            Self::CellLifecycle(event) => {
                nonzero("cell_id", event.cell_id.digest())?;
                nonzero("experiment_id", event.experiment_id.digest())?;
                nonzero("generation_id", event.generation_id.digest())?;
                text("action", &event.action)
            }
            Self::GenerationLifecycle(event) => {
                nonzero("generation_id", event.generation_id.digest())?;
                nonzero("cell_id", event.cell_id.digest())?;
                nonzero("experiment_id", event.experiment_id.digest())?;
                text("action", &event.action)?;
                if let Some(outcome) = &event.outcome {
                    text("outcome", outcome)?;
                }
                Ok(())
            }
            Self::AttemptLifecycle(event) => {
                nonzero("cell_id", event.cell_id.digest())?;
                nonzero("generation_id", event.generation_id.digest())?;
                text("action", &event.action)?;
                if let Some(outcome) = &event.outcome {
                    text("outcome", outcome)?;
                }
                Ok(())
            }
            Self::WorkerSession(event) => {
                nonzero("worker_id", event.worker_id.digest())?;
                text("action", &event.action)?;
                if event.calibration_data.len() > Self::MAX_BLOB_BYTES {
                    return Err(LedgerError::Encoding(
                        "worker calibration payload exceeds ledger event bound".into(),
                    ));
                }
                Ok(())
            }
            Self::TaskStart(event) => {
                nonzero("task_id", event.task_id.digest())?;
                nonzero("cell_id", event.cell_id.digest())
            }
            Self::TaskEnd(event) => {
                nonzero("task_id", event.task_id.digest())?;
                nonzero("cell_id", event.cell_id.digest())?;
                text("outcome", &event.outcome)
            }
            Self::EpisodeStart(event) => {
                nonzero("episode_id", event.episode_id.digest())?;
                nonzero("task_id", event.task_id.digest())
            }
            Self::EpisodeEnd(event) => {
                nonzero("episode_id", event.episode_id.digest())?;
                text("status", &event.status)
            }
            Self::StateDiscovery(event) => {
                nonzero("state_id", event.state_id.digest())?;
                nonzero("episode_id", event.episode_id.digest())
            }
            Self::CandidateBatch(event) => {
                nonzero("state_id", event.state_id.digest())?;
                if event.candidate_ids.is_empty()
                    || event.candidate_ids.len() > Self::MAX_VECTOR_ITEMS
                    || event.classes.len() != event.candidate_ids.len()
                    || event.tie_breaks.len() != event.candidate_ids.len()
                {
                    return Err(LedgerError::Encoding(
                        "candidate batch geometry is empty, oversized, or inconsistent".into(),
                    ));
                }
                let mut unique =
                    std::collections::HashSet::with_capacity(event.candidate_ids.len());
                for candidate in &event.candidate_ids {
                    nonzero("candidate_id", candidate.digest())?;
                    if !unique.insert(*candidate) {
                        return Err(LedgerError::Encoding(
                            "candidate batch contains a duplicate identity".into(),
                        ));
                    }
                }
                Ok(())
            }
            Self::FeatureRef(event) => {
                nonzero("state_id", event.state_id.digest())?;
                nonzero("feature_schema", event.feature_schema.digest())?;
                if event.payload_handle.is_empty()
                    || event.payload_handle.len() > Self::MAX_BLOB_BYTES
                {
                    return Err(LedgerError::Encoding(
                        "feature payload handle must be non-empty and bounded".into(),
                    ));
                }
                Ok(())
            }
            Self::PolicyScore(event) => {
                nonzero("state_id", event.state_id.digest())?;
                nonzero("model_id", event.model_id.digest())?;
                nonzero("selected_candidate", event.selected_candidate.digest())?;
                if event.candidate_ids.is_empty()
                    || event.candidate_ids.len() > Self::MAX_VECTOR_ITEMS
                    || event.scores.len() != event.candidate_ids.len()
                    || event.scores.iter().any(|score| !score.is_finite())
                    || !event.candidate_ids.contains(&event.selected_candidate)
                {
                    return Err(LedgerError::Encoding(
                        "policy score geometry, values, or selected candidate are invalid".into(),
                    ));
                }
                let mut unique =
                    std::collections::HashSet::with_capacity(event.candidate_ids.len());
                for candidate in &event.candidate_ids {
                    nonzero("candidate_id", candidate.digest())?;
                    if !unique.insert(*candidate) {
                        return Err(LedgerError::Encoding(
                            "policy score contains a duplicate candidate identity".into(),
                        ));
                    }
                }
                Ok(())
            }
            Self::CandidateApplication(event) => {
                nonzero("state_id", event.state_id.digest())?;
                nonzero("candidate_id", event.candidate_id.digest())?;
                text("outcome", &event.outcome)?;
                if event.and_child_states.len() > Self::MAX_VECTOR_ITEMS {
                    return Err(LedgerError::Encoding(
                        "candidate application child list exceeds event bound".into(),
                    ));
                }
                let mut unique =
                    std::collections::HashSet::with_capacity(event.and_child_states.len());
                for child in &event.and_child_states {
                    nonzero("and_child_state", child.digest())?;
                    if !unique.insert(*child) {
                        return Err(LedgerError::Encoding(
                            "candidate application contains a duplicate child state".into(),
                        ));
                    }
                }
                Ok(())
            }
            Self::CacheObservation(event) => {
                nonzero("state_id", event.state_id.digest())?;
                nonzero("cache_key", &event.cache_key)
            }
            Self::ArtifactConstruction(event) => {
                nonzero("artifact_id", event.artifact_id.digest())?;
                nonzero("episode_id", event.episode_id.digest())
            }
            Self::VerificationReceipt(event) => {
                nonzero("artifact_id", event.artifact_id.digest())?;
                nonzero("verifier", event.verifier.digest())?;
                text("status", &event.status)
            }
            Self::UtilityObservation(event) => {
                nonzero("subject", event.subject.digest())?;
                nonzero("metric", event.metric.digest())?;
                nonzero("unit", event.unit.digest())?;
                if !event.value.is_finite() {
                    return Err(LedgerError::Encoding(
                        "utility observation value must be finite".into(),
                    ));
                }
                Ok(())
            }
            Self::ModelShadowScore(event) => {
                nonzero("model_id", event.model_id.digest())?;
                nonzero("state_id", event.state_id.digest())?;
                nonzero("candidate_id", event.candidate_id.digest())?;
                if !event.score.is_finite() {
                    return Err(LedgerError::Encoding("shadow score must be finite".into()));
                }
                Ok(())
            }
            Self::ResourceSample(_) => Ok(()),
            Self::LineageEdge(event) => {
                nonzero("lineage parent", event.parent.digest())?;
                nonzero("lineage child", event.child.digest())?;
                if event.parent == event.child {
                    return Err(LedgerError::Encoding(
                        "lineage edge cannot be a self-edge".into(),
                    ));
                }
                text("edge_type", &event.edge_type)
            }
            Self::Incident(event) => {
                text("severity", &event.severity)?;
                text("description", &event.description)?;
                if let Some(episode) = event.episode_id {
                    nonzero("episode_id", episode.digest())?;
                }
                Ok(())
            }
            Self::Cancellation(event) => {
                match &event.target {
                    CancellationTarget::Episode(id) => nonzero("episode_id", id.digest())?,
                    CancellationTarget::Cell(id) => nonzero("cell_id", id.digest())?,
                    CancellationTarget::Attempt { cell_id, .. } => {
                        nonzero("cell_id", cell_id.digest())?
                    }
                }
                text("reason", &event.reason)
            }
        }
    }

    /// JSON export for operators/tooling. The on-disk format is binary; this
    /// is a debug/tooling helper only.
    pub fn to_json(&self) -> Result<String, LedgerError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|e| LedgerError::Encoding(e.to_string()))
    }

    /// JSON import for operators/tooling. The on-disk format is binary; this
    /// is a debug/tooling helper only.
    pub fn from_json(json: &str) -> Result<Self, LedgerError> {
        let event: Self =
            serde_json::from_str(json).map_err(|e| LedgerError::Encoding(e.to_string()))?;
        event.validate()?;
        Ok(event)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequencedEvent {
    pub sequence: u64,
    pub event: Event,
}

// ---------------------------------------------------------------------------
// Zeroed constructors (decode staging; IDs are blake3 digests, no Default)
// ---------------------------------------------------------------------------

macro_rules! zero_id {
    ($ty:ident) => {
        $ty::from_digest(Digest::ZERO)
    };
}

impl Default for ExperimentLifecycleEvent {
    fn default() -> Self {
        Self {
            experiment_id: zero_id!(ExperimentId),
            action: String::new(),
            timestamp_ns: 0,
        }
    }
}
impl Default for CellLifecycleEvent {
    fn default() -> Self {
        Self {
            cell_id: zero_id!(CellId),
            experiment_id: zero_id!(ExperimentId),
            generation_id: zero_id!(GenerationId),
            action: String::new(),
            timestamp_ns: 0,
        }
    }
}
impl Default for GenerationLifecycleEvent {
    fn default() -> Self {
        Self {
            generation_id: zero_id!(GenerationId),
            cell_id: zero_id!(CellId),
            experiment_id: zero_id!(ExperimentId),
            action: String::new(),
            outcome: None,
            timestamp_ns: 0,
        }
    }
}
impl Default for AttemptLifecycleEvent {
    fn default() -> Self {
        Self {
            attempt_id: 0,
            cell_id: zero_id!(CellId),
            generation_id: zero_id!(GenerationId),
            action: String::new(),
            outcome: None,
            timestamp_ns: 0,
        }
    }
}
impl Default for WorkerSessionEvent {
    fn default() -> Self {
        Self {
            worker_id: zero_id!(WorkerId),
            action: String::new(),
            calibration_data: Vec::new(),
            timestamp_ns: 0,
        }
    }
}
impl Default for TaskStartEvent {
    fn default() -> Self {
        Self {
            task_id: zero_id!(TaskId),
            cell_id: zero_id!(CellId),
            timestamp_ns: 0,
        }
    }
}
impl Default for TaskEndEvent {
    fn default() -> Self {
        Self {
            task_id: zero_id!(TaskId),
            cell_id: zero_id!(CellId),
            outcome: String::new(),
            timestamp_ns: 0,
        }
    }
}
impl Default for EpisodeStartEvent {
    fn default() -> Self {
        Self {
            episode_id: zero_id!(EpisodeId),
            task_id: zero_id!(TaskId),
            timestamp_ns: 0,
        }
    }
}
impl Default for EpisodeEndEvent {
    fn default() -> Self {
        Self {
            episode_id: zero_id!(EpisodeId),
            status: String::new(),
            actions_taken: 0,
            timestamp_ns: 0,
        }
    }
}
impl Default for StateDiscoveryEvent {
    fn default() -> Self {
        Self {
            state_id: zero_id!(StateId),
            episode_id: zero_id!(EpisodeId),
            depth: 0,
        }
    }
}
impl Default for CandidateBatchEvent {
    fn default() -> Self {
        Self {
            state_id: zero_id!(StateId),
            candidate_ids: Vec::new(),
            classes: Vec::new(),
            tie_breaks: Vec::new(),
        }
    }
}
impl Default for FeatureRefEvent {
    fn default() -> Self {
        Self {
            state_id: zero_id!(StateId),
            feature_schema: zero_id!(FeatureSchemaId),
            payload_handle: Vec::new(),
        }
    }
}
impl Default for PolicyScoreEvent {
    fn default() -> Self {
        Self {
            state_id: zero_id!(StateId),
            model_id: zero_id!(ModelCheckpointId),
            candidate_ids: Vec::new(),
            scores: Vec::new(),
            selected_candidate: zero_id!(CandidateId),
        }
    }
}
impl Default for CandidateApplicationEvent {
    fn default() -> Self {
        Self {
            state_id: zero_id!(StateId),
            candidate_id: zero_id!(CandidateId),
            and_child_states: Vec::new(),
            outcome: String::new(),
        }
    }
}
impl Default for CacheObservationEvent {
    fn default() -> Self {
        Self {
            state_id: zero_id!(StateId),
            hit: false,
            cache_key: Digest::ZERO,
            timestamp_ns: 0,
        }
    }
}
impl Default for ArtifactConstructionEvent {
    fn default() -> Self {
        Self {
            artifact_id: zero_id!(ArtifactId),
            episode_id: zero_id!(EpisodeId),
            size_bytes: 0,
        }
    }
}
impl Default for VerificationReceiptEvent {
    fn default() -> Self {
        Self {
            artifact_id: zero_id!(ArtifactId),
            verifier: zero_id!(VerifierId),
            status: String::new(),
            cpu_ns: 0,
            wall_ns: 0,
        }
    }
}
impl Default for UtilityObservationEvent {
    fn default() -> Self {
        Self {
            subject: zero_id!(ResearchNodeId),
            metric: zero_id!(MetricId),
            value: 0.0,
            unit: zero_id!(UnitId),
        }
    }
}
impl Default for ModelShadowScoreEvent {
    fn default() -> Self {
        Self {
            model_id: zero_id!(ModelCheckpointId),
            state_id: zero_id!(StateId),
            candidate_id: zero_id!(CandidateId),
            score: 0.0,
            timestamp_ns: 0,
        }
    }
}
impl Default for LineageEdgeEvent {
    fn default() -> Self {
        Self {
            parent: zero_id!(ResearchNodeId),
            child: zero_id!(ResearchNodeId),
            edge_type: String::new(),
        }
    }
}
impl Default for CancellationEvent {
    fn default() -> Self {
        Self {
            target: CancellationTarget::Cell(zero_id!(CellId)),
            reason: String::new(),
            timestamp_ns: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

pub(crate) fn event_tag(event: &Event) -> u8 {
    match event {
        Event::ExperimentLifecycle(_) => TAG_EXPERIMENT_LIFECYCLE,
        Event::CellLifecycle(_) => TAG_CELL_LIFECYCLE,
        Event::GenerationLifecycle(_) => TAG_GENERATION_LIFECYCLE,
        Event::AttemptLifecycle(_) => TAG_ATTEMPT_LIFECYCLE,
        Event::WorkerSession(_) => TAG_WORKER_SESSION,
        Event::TaskStart(_) => TAG_TASK_START,
        Event::TaskEnd(_) => TAG_TASK_END,
        Event::EpisodeStart(_) => TAG_EPISODE_START,
        Event::EpisodeEnd(_) => TAG_EPISODE_END,
        Event::StateDiscovery(_) => TAG_STATE_DISCOVERY,
        Event::CandidateBatch(_) => TAG_CANDIDATE_BATCH,
        Event::FeatureRef(_) => TAG_FEATURE_REF,
        Event::PolicyScore(_) => TAG_POLICY_SCORE,
        Event::CandidateApplication(_) => TAG_CANDIDATE_APPLICATION,
        Event::CacheObservation(_) => TAG_CACHE_OBSERVATION,
        Event::ArtifactConstruction(_) => TAG_ARTIFACT_CONSTRUCTION,
        Event::VerificationReceipt(_) => TAG_VERIFICATION_RECEIPT,
        Event::UtilityObservation(_) => TAG_UTILITY_OBSERVATION,
        Event::ModelShadowScore(_) => TAG_MODEL_SHADOW_SCORE,
        Event::ResourceSample(_) => TAG_RESOURCE_SAMPLE,
        Event::LineageEdge(_) => TAG_LINEAGE_EDGE,
        Event::Incident(_) => TAG_INCIDENT,
        Event::Cancellation(_) => TAG_CANCELLATION,
    }
}

/// Encodes the type tag + event body into `enc`. The sequence delta framing is
/// handled by the encoder (`crate::encoder::EventEncoder`).
pub(crate) fn encode_event(enc: &mut Enc, event: &Event) {
    enc.u8(event_tag(event));
    match event {
        Event::ExperimentLifecycle(e) => {
            enc.id32(e.experiment_id.digest().as_bytes());
            enc.str(&e.action);
            enc.u64(e.timestamp_ns);
        }
        Event::CellLifecycle(e) => {
            enc.id32(e.cell_id.digest().as_bytes());
            enc.id32(e.experiment_id.digest().as_bytes());
            enc.id32(e.generation_id.digest().as_bytes());
            enc.str(&e.action);
            enc.u64(e.timestamp_ns);
        }
        Event::GenerationLifecycle(e) => {
            enc.id32(e.generation_id.digest().as_bytes());
            enc.id32(e.cell_id.digest().as_bytes());
            enc.id32(e.experiment_id.digest().as_bytes());
            enc.str(&e.action);
            enc.opt_str(&e.outcome);
            enc.u64(e.timestamp_ns);
        }
        Event::AttemptLifecycle(e) => {
            enc.u64(e.attempt_id);
            enc.id32(e.cell_id.digest().as_bytes());
            enc.id32(e.generation_id.digest().as_bytes());
            enc.str(&e.action);
            enc.opt_str(&e.outcome);
            enc.u64(e.timestamp_ns);
        }
        Event::WorkerSession(e) => {
            enc.id32(e.worker_id.digest().as_bytes());
            enc.str(&e.action);
            enc.vec_bytes(&e.calibration_data);
            enc.u64(e.timestamp_ns);
        }
        Event::TaskStart(e) => {
            enc.id32(e.task_id.digest().as_bytes());
            enc.id32(e.cell_id.digest().as_bytes());
            enc.u64(e.timestamp_ns);
        }
        Event::TaskEnd(e) => {
            enc.id32(e.task_id.digest().as_bytes());
            enc.id32(e.cell_id.digest().as_bytes());
            enc.str(&e.outcome);
            enc.u64(e.timestamp_ns);
        }
        Event::EpisodeStart(e) => {
            enc.id32(e.episode_id.digest().as_bytes());
            enc.id32(e.task_id.digest().as_bytes());
            enc.u64(e.timestamp_ns);
        }
        Event::EpisodeEnd(e) => {
            enc.id32(e.episode_id.digest().as_bytes());
            enc.str(&e.status);
            enc.u32(e.actions_taken);
            enc.u64(e.timestamp_ns);
        }
        Event::StateDiscovery(e) => {
            enc.id32(e.state_id.digest().as_bytes());
            enc.id32(e.episode_id.digest().as_bytes());
            enc.u32(e.depth);
        }
        Event::CandidateBatch(e) => {
            enc.id32(e.state_id.digest().as_bytes());
            enc.varint(e.candidate_ids.len() as u64);
            for (i, c) in e.candidate_ids.iter().enumerate() {
                enc.id32(c.digest().as_bytes());
                enc.u16(e.classes.get(i).copied().unwrap_or(0));
                enc.u64(e.tie_breaks.get(i).copied().unwrap_or(0));
            }
        }
        Event::FeatureRef(e) => {
            enc.id32(e.state_id.digest().as_bytes());
            enc.id32(e.feature_schema.digest().as_bytes());
            enc.vec_bytes(&e.payload_handle);
        }
        Event::PolicyScore(e) => {
            enc.id32(e.state_id.digest().as_bytes());
            enc.id32(e.model_id.digest().as_bytes());
            let n = e.candidate_ids.len();
            enc.u16(n as u16);
            let selected = e
                .candidate_ids
                .iter()
                .position(|c| *c == e.selected_candidate)
                .unwrap_or(0xFFFF);
            enc.u16(selected as u16);
            for (i, c) in e.candidate_ids.iter().enumerate() {
                enc.u64(candidate_prefix(c));
                enc.f32(e.scores.get(i).copied().unwrap_or(0.0));
            }
        }
        Event::CandidateApplication(e) => {
            enc.id32(e.state_id.digest().as_bytes());
            enc.id32(e.candidate_id.digest().as_bytes());
            enc.varint(e.and_child_states.len() as u64);
            for s in &e.and_child_states {
                enc.id32(s.digest().as_bytes());
            }
            enc.str(&e.outcome);
        }
        Event::CacheObservation(e) => {
            enc.id32(e.state_id.digest().as_bytes());
            enc.bool(e.hit);
            enc.digest(&e.cache_key);
            enc.u64(e.timestamp_ns);
        }
        Event::ArtifactConstruction(e) => {
            enc.id32(e.artifact_id.digest().as_bytes());
            enc.id32(e.episode_id.digest().as_bytes());
            enc.u64(e.size_bytes);
        }
        Event::VerificationReceipt(e) => {
            enc.id32(e.artifact_id.digest().as_bytes());
            enc.id32(e.verifier.digest().as_bytes());
            enc.str(&e.status);
            enc.u64(e.cpu_ns);
            enc.u64(e.wall_ns);
        }
        Event::UtilityObservation(e) => {
            enc.id32(e.subject.digest().as_bytes());
            enc.id32(e.metric.digest().as_bytes());
            enc.f64(e.value);
            enc.id32(e.unit.digest().as_bytes());
        }
        Event::ModelShadowScore(e) => {
            enc.id32(e.model_id.digest().as_bytes());
            enc.id32(e.state_id.digest().as_bytes());
            enc.id32(e.candidate_id.digest().as_bytes());
            enc.f32(e.score);
            enc.u64(e.timestamp_ns);
        }
        Event::ResourceSample(e) => {
            enc.u64(e.user_cpu_ns);
            enc.u64(e.sys_cpu_ns);
            enc.u64(e.rss_bytes);
            enc.u64(e.timestamp_ns);
        }
        Event::LineageEdge(e) => {
            enc.id32(e.parent.digest().as_bytes());
            enc.id32(e.child.digest().as_bytes());
            enc.str(&e.edge_type);
        }
        Event::Incident(e) => {
            enc.str(&e.severity);
            enc.str(&e.description);
            match &e.episode_id {
                Some(ep) => {
                    enc.u8(1);
                    enc.id32(ep.digest().as_bytes());
                }
                None => enc.u8(0),
            }
            enc.u64(e.timestamp_ns);
        }
        Event::Cancellation(e) => {
            match &e.target {
                CancellationTarget::Episode(ep) => {
                    enc.u8(0);
                    enc.id32(ep.digest().as_bytes());
                }
                CancellationTarget::Cell(c) => {
                    enc.u8(1);
                    enc.id32(c.digest().as_bytes());
                }
                CancellationTarget::Attempt {
                    cell_id,
                    attempt_id,
                } => {
                    enc.u8(2);
                    enc.id32(cell_id.digest().as_bytes());
                    enc.u64(*attempt_id);
                }
            }
            enc.str(&e.reason);
            enc.u64(e.timestamp_ns);
        }
    }
}

pub(crate) fn candidate_prefix(c: &CandidateId) -> u64 {
    u64::from_le_bytes(c.digest().as_bytes()[0..8].try_into().unwrap())
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

pub(crate) type CandidateUniverse = std::collections::HashMap<u64, Vec<CandidateId>>;

pub(crate) fn decode_event(dec: &mut Dec, tag: u8, out: &mut Event) -> Result<(), LedgerError> {
    let decoded = match tag {
        TAG_EXPERIMENT_LIFECYCLE => {
            if let Event::ExperimentLifecycle(e) = out {
                dec.decode_experiment_lifecycle(e)
            } else {
                let mut e = ExperimentLifecycleEvent::default();
                dec.decode_experiment_lifecycle(&mut e)?;
                *out = Event::ExperimentLifecycle(e);
                Ok(())
            }
        }
        TAG_CELL_LIFECYCLE => {
            if let Event::CellLifecycle(e) = out {
                dec.decode_cell_lifecycle(e)
            } else {
                let mut e = CellLifecycleEvent::default();
                dec.decode_cell_lifecycle(&mut e)?;
                *out = Event::CellLifecycle(e);
                Ok(())
            }
        }
        TAG_GENERATION_LIFECYCLE => {
            if let Event::GenerationLifecycle(e) = out {
                dec.decode_generation_lifecycle(e)
            } else {
                let mut e = GenerationLifecycleEvent::default();
                dec.decode_generation_lifecycle(&mut e)?;
                *out = Event::GenerationLifecycle(e);
                Ok(())
            }
        }
        TAG_ATTEMPT_LIFECYCLE => {
            if let Event::AttemptLifecycle(e) = out {
                dec.decode_attempt_lifecycle(e)
            } else {
                let mut e = AttemptLifecycleEvent::default();
                dec.decode_attempt_lifecycle(&mut e)?;
                *out = Event::AttemptLifecycle(e);
                Ok(())
            }
        }
        TAG_WORKER_SESSION => {
            if let Event::WorkerSession(e) = out {
                dec.decode_worker_session(e)
            } else {
                let mut e = WorkerSessionEvent::default();
                dec.decode_worker_session(&mut e)?;
                *out = Event::WorkerSession(e);
                Ok(())
            }
        }
        TAG_TASK_START => {
            if let Event::TaskStart(e) = out {
                dec.decode_task_start(e)
            } else {
                let mut e = TaskStartEvent::default();
                dec.decode_task_start(&mut e)?;
                *out = Event::TaskStart(e);
                Ok(())
            }
        }
        TAG_TASK_END => {
            if let Event::TaskEnd(e) = out {
                dec.decode_task_end(e)
            } else {
                let mut e = TaskEndEvent::default();
                dec.decode_task_end(&mut e)?;
                *out = Event::TaskEnd(e);
                Ok(())
            }
        }
        TAG_EPISODE_START => {
            if let Event::EpisodeStart(e) = out {
                dec.decode_episode_start(e)
            } else {
                let mut e = EpisodeStartEvent::default();
                dec.decode_episode_start(&mut e)?;
                *out = Event::EpisodeStart(e);
                Ok(())
            }
        }
        TAG_EPISODE_END => {
            if let Event::EpisodeEnd(e) = out {
                dec.decode_episode_end(e)
            } else {
                let mut e = EpisodeEndEvent::default();
                dec.decode_episode_end(&mut e)?;
                *out = Event::EpisodeEnd(e);
                Ok(())
            }
        }
        TAG_STATE_DISCOVERY => {
            if let Event::StateDiscovery(e) = out {
                dec.decode_state_discovery(e)
            } else {
                let mut e = StateDiscoveryEvent::default();
                dec.decode_state_discovery(&mut e)?;
                *out = Event::StateDiscovery(e);
                Ok(())
            }
        }
        TAG_CANDIDATE_BATCH => {
            if let Event::CandidateBatch(e) = out {
                dec.decode_candidate_batch(e)
            } else {
                let mut e = CandidateBatchEvent::default();
                dec.decode_candidate_batch(&mut e)?;
                *out = Event::CandidateBatch(e);
                Ok(())
            }
        }
        TAG_FEATURE_REF => {
            if let Event::FeatureRef(e) = out {
                dec.decode_feature_ref(e)
            } else {
                let mut e = FeatureRefEvent::default();
                dec.decode_feature_ref(&mut e)?;
                *out = Event::FeatureRef(e);
                Ok(())
            }
        }
        TAG_POLICY_SCORE => {
            if let Event::PolicyScore(e) = out {
                dec.decode_policy_score(e)
            } else {
                let mut e = PolicyScoreEvent::default();
                dec.decode_policy_score(&mut e)?;
                *out = Event::PolicyScore(e);
                Ok(())
            }
        }
        TAG_CANDIDATE_APPLICATION => {
            if let Event::CandidateApplication(e) = out {
                dec.decode_candidate_application(e)
            } else {
                let mut e = CandidateApplicationEvent::default();
                dec.decode_candidate_application(&mut e)?;
                *out = Event::CandidateApplication(e);
                Ok(())
            }
        }
        TAG_CACHE_OBSERVATION => {
            if let Event::CacheObservation(e) = out {
                dec.decode_cache_observation(e)
            } else {
                let mut e = CacheObservationEvent::default();
                dec.decode_cache_observation(&mut e)?;
                *out = Event::CacheObservation(e);
                Ok(())
            }
        }
        TAG_ARTIFACT_CONSTRUCTION => {
            if let Event::ArtifactConstruction(e) = out {
                dec.decode_artifact_construction(e)
            } else {
                let mut e = ArtifactConstructionEvent::default();
                dec.decode_artifact_construction(&mut e)?;
                *out = Event::ArtifactConstruction(e);
                Ok(())
            }
        }
        TAG_VERIFICATION_RECEIPT => {
            if let Event::VerificationReceipt(e) = out {
                dec.decode_verification_receipt(e)
            } else {
                let mut e = VerificationReceiptEvent::default();
                dec.decode_verification_receipt(&mut e)?;
                *out = Event::VerificationReceipt(e);
                Ok(())
            }
        }
        TAG_UTILITY_OBSERVATION => {
            if let Event::UtilityObservation(e) = out {
                dec.decode_utility_observation(e)
            } else {
                let mut e = UtilityObservationEvent::default();
                dec.decode_utility_observation(&mut e)?;
                *out = Event::UtilityObservation(e);
                Ok(())
            }
        }
        TAG_MODEL_SHADOW_SCORE => {
            if let Event::ModelShadowScore(e) = out {
                dec.decode_model_shadow_score(e)
            } else {
                let mut e = ModelShadowScoreEvent::default();
                dec.decode_model_shadow_score(&mut e)?;
                *out = Event::ModelShadowScore(e);
                Ok(())
            }
        }
        TAG_RESOURCE_SAMPLE => {
            if let Event::ResourceSample(e) = out {
                dec.decode_resource_sample(e)
            } else {
                let mut e = ResourceSampleEvent::default();
                dec.decode_resource_sample(&mut e)?;
                *out = Event::ResourceSample(e);
                Ok(())
            }
        }
        TAG_LINEAGE_EDGE => {
            if let Event::LineageEdge(e) = out {
                dec.decode_lineage_edge(e)
            } else {
                let mut e = LineageEdgeEvent::default();
                dec.decode_lineage_edge(&mut e)?;
                *out = Event::LineageEdge(e);
                Ok(())
            }
        }
        TAG_INCIDENT => {
            if let Event::Incident(e) = out {
                dec.decode_incident(e)
            } else {
                let mut e = IncidentEvent::default();
                dec.decode_incident(&mut e)?;
                *out = Event::Incident(e);
                Ok(())
            }
        }
        TAG_CANCELLATION => {
            if let Event::Cancellation(e) = out {
                dec.decode_cancellation(e)
            } else {
                let mut e = CancellationEvent::default();
                dec.decode_cancellation(&mut e)?;
                *out = Event::Cancellation(e);
                Ok(())
            }
        }
        other => Err(LedgerError::Encoding(format!("unknown event tag {other}"))),
    };
    decoded?;
    out.validate()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::codec::encode_event_into;
    use bytes::BytesMut;
    use reflex_types::DigestAlgorithm;

    pub(crate) fn make_all_events() -> Vec<Event> {
        let id = |seed: &[u8]| Digest::hash_blake3(seed);
        vec![
            Event::ExperimentLifecycle(ExperimentLifecycleEvent {
                experiment_id: ExperimentId::from_digest(id(b"exp")),
                action: "start".into(),
                timestamp_ns: 100,
            }),
            Event::CellLifecycle(CellLifecycleEvent {
                cell_id: CellId::from_digest(id(b"cell")),
                experiment_id: ExperimentId::from_digest(id(b"exp")),
                generation_id: GenerationId::from_digest(id(b"gen")),
                action: "started".into(),
                timestamp_ns: 200,
            }),
            Event::GenerationLifecycle(GenerationLifecycleEvent {
                generation_id: GenerationId::from_digest(id(b"gen")),
                cell_id: CellId::from_digest(id(b"cell")),
                experiment_id: ExperimentId::from_digest(id(b"exp")),
                action: "end".into(),
                outcome: Some("complete".into()),
                timestamp_ns: 300,
            }),
            Event::AttemptLifecycle(AttemptLifecycleEvent {
                attempt_id: 7,
                cell_id: CellId::from_digest(id(b"cell")),
                generation_id: GenerationId::from_digest(id(b"gen")),
                action: "accept".into(),
                outcome: None,
                timestamp_ns: 400,
            }),
            Event::WorkerSession(WorkerSessionEvent {
                worker_id: WorkerId::from_digest(id(b"worker")),
                action: "calibrated".into(),
                calibration_data: vec![1, 2, 3],
                timestamp_ns: 500,
            }),
            Event::TaskStart(TaskStartEvent {
                task_id: TaskId::from_digest(id(b"task")),
                cell_id: CellId::from_digest(id(b"cell")),
                timestamp_ns: 600,
            }),
            Event::TaskEnd(TaskEndEvent {
                task_id: TaskId::from_digest(id(b"task")),
                cell_id: CellId::from_digest(id(b"cell")),
                outcome: "solved".into(),
                timestamp_ns: 700,
            }),
            Event::EpisodeStart(EpisodeStartEvent {
                episode_id: EpisodeId::from_digest(id(b"ep")),
                task_id: TaskId::from_digest(id(b"task")),
                timestamp_ns: 800,
            }),
            Event::EpisodeEnd(EpisodeEndEvent {
                episode_id: EpisodeId::from_digest(id(b"ep")),
                status: "closed".into(),
                actions_taken: 12,
                timestamp_ns: 900,
            }),
            Event::StateDiscovery(StateDiscoveryEvent {
                state_id: StateId::from_digest(id(b"state")),
                episode_id: EpisodeId::from_digest(id(b"ep")),
                depth: 3,
            }),
            Event::CandidateBatch(CandidateBatchEvent {
                state_id: StateId::from_digest(id(b"state")),
                candidate_ids: vec![
                    CandidateId::from_digest(id(b"cand-0")),
                    CandidateId::from_digest(id(b"cand-1")),
                ],
                classes: vec![1, 2],
                tie_breaks: vec![10, 20],
            }),
            Event::FeatureRef(FeatureRefEvent {
                state_id: StateId::from_digest(id(b"state")),
                feature_schema: FeatureSchemaId::from_digest(id(b"schema")),
                payload_handle: vec![9, 8, 7],
            }),
            Event::PolicyScore(PolicyScoreEvent {
                state_id: StateId::from_digest(id(b"state")),
                model_id: ModelCheckpointId::from_digest(id(b"model")),
                candidate_ids: vec![
                    CandidateId::from_digest(id(b"cand-0")),
                    CandidateId::from_digest(id(b"cand-1")),
                ],
                scores: vec![0.9, 0.1],
                selected_candidate: CandidateId::from_digest(id(b"cand-0")),
            }),
            Event::CandidateApplication(CandidateApplicationEvent {
                state_id: StateId::from_digest(id(b"state")),
                candidate_id: CandidateId::from_digest(id(b"cand-0")),
                and_child_states: vec![StateId::from_digest(id(b"child-0"))],
                outcome: "Closed".into(),
            }),
            Event::CacheObservation(CacheObservationEvent {
                state_id: StateId::from_digest(id(b"state")),
                hit: true,
                cache_key: Digest {
                    algorithm: DigestAlgorithm::Sha256,
                    bytes: id(b"key").bytes,
                },
                timestamp_ns: 1000,
            }),
            Event::ArtifactConstruction(ArtifactConstructionEvent {
                artifact_id: ArtifactId::from_digest(id(b"artifact")),
                episode_id: EpisodeId::from_digest(id(b"ep")),
                size_bytes: 4096,
            }),
            Event::VerificationReceipt(VerificationReceiptEvent {
                artifact_id: ArtifactId::from_digest(id(b"artifact")),
                verifier: VerifierId::from_digest(id(b"verifier")),
                status: "accepted".into(),
                cpu_ns: 111,
                wall_ns: 222,
            }),
            Event::UtilityObservation(UtilityObservationEvent {
                subject: ResearchNodeId::from_digest(id(b"node")),
                metric: MetricId::from_digest(id(b"metric")),
                value: 3.25,
                unit: UnitId::from_digest(id(b"unit")),
            }),
            Event::ModelShadowScore(ModelShadowScoreEvent {
                model_id: ModelCheckpointId::from_digest(id(b"model")),
                state_id: StateId::from_digest(id(b"state")),
                candidate_id: CandidateId::from_digest(id(b"cand-1")),
                score: 0.42,
                timestamp_ns: 1100,
            }),
            Event::ResourceSample(ResourceSampleEvent {
                user_cpu_ns: 1,
                sys_cpu_ns: 2,
                rss_bytes: 3,
                timestamp_ns: 1200,
            }),
            Event::LineageEdge(LineageEdgeEvent {
                parent: ResearchNodeId::from_digest(id(b"node")),
                child: ResearchNodeId::from_digest(id(b"child-0")),
                edge_type: "derives".into(),
            }),
            Event::Incident(IncidentEvent {
                severity: "warn".into(),
                description: "late barrier".into(),
                episode_id: Some(EpisodeId::from_digest(id(b"ep"))),
                timestamp_ns: 1300,
            }),
            Event::Cancellation(CancellationEvent {
                target: CancellationTarget::Attempt {
                    cell_id: CellId::from_digest(id(b"cell")),
                    attempt_id: 3,
                },
                reason: "user interrupt".into(),
                timestamp_ns: 1400,
            }),
        ]
    }

    #[test]
    fn test_all_families_roundtrip() {
        let events = make_all_events();
        assert_eq!(events.len(), EVENT_TAG_COUNT as usize);
        for ev in events {
            let mut buf = BytesMut::new();
            encode_event_into(&mut buf, &ev);
            let universe = if let Event::PolicyScore(p) = &ev {
                // Prefix resolution needs a universe; seed it from the event's
                // own candidates (one unique prefix per candidate here).
                let mut universe = std::collections::HashMap::new();
                for c in &p.candidate_ids {
                    universe
                        .entry(candidate_prefix(c))
                        .or_insert_with(Vec::new)
                        .push(*c);
                }
                Some(universe)
            } else {
                None
            };
            let mut out = Event::ResourceSample(ResourceSampleEvent::default());
            let mut dec = Dec::new(&buf);
            dec.pos = 1; // skip the tag byte; decode_event reads the body
            dec.candidates = universe.as_ref();
            let res = decode_event(&mut dec, event_tag(&ev), &mut out);
            if res.is_err() {
                panic!(
                    "tag {} failed: {:?} (buf len {}, dec.pos {})",
                    event_tag(&ev),
                    res.err(),
                    buf.len(),
                    dec.pos
                );
            }
            assert_eq!(dec.pos, buf.len(), "trailing bytes for {}", event_tag(&ev));
            assert_eq!(out, ev, "roundtrip failed for tag {}", event_tag(&ev));
        }
    }

    #[test]
    fn test_unknown_tag_rejected() {
        let mut out = Event::ResourceSample(ResourceSampleEvent::default());
        let mut dec = Dec::new(&[0u8; 1]);
        assert!(matches!(
            decode_event(&mut dec, EVENT_TAG_COUNT, &mut out),
            Err(LedgerError::Encoding(_))
        ));
    }

    #[test]
    fn test_truncated_payload_rejected() {
        let events = make_all_events();
        for ev in events {
            let mut buf = BytesMut::new();
            encode_event_into(&mut buf, &ev);
            for cut in 0..buf.len() {
                let mut out = Event::ResourceSample(ResourceSampleEvent::default());
                let mut dec = Dec::new(&buf[..cut]);
                dec.pos = 1; // skip the tag byte; keep only a partial body
                assert!(
                    decode_event(&mut dec, event_tag(&ev), &mut out).is_err(),
                    "cut={cut} should fail for tag {}",
                    event_tag(&ev)
                );
            }
        }
    }

    #[test]
    fn test_candidate_row_size_under_32_bytes() {
        // Per-candidate wire cost of a candidate score event (excluding shared
        // state/model features): 8-byte digest prefix + f32 score = 12 bytes.
        const { assert!(CANDIDATE_ROW_BYTES < 32) };
        let mut buf = BytesMut::new();
        let mut enc = Enc { buf: &mut buf };
        let ev = make_all_events()
            .into_iter()
            .find_map(|e| match e {
                Event::PolicyScore(p) => Some(p),
                _ => None,
            })
            .unwrap();
        encode_event(&mut enc, &Event::PolicyScore(ev));
        // 1-byte tag + 2 candidates * 12 bytes of row data
        // + 68 bytes of shared state.
        assert_eq!(buf.len(), 1 + 68 + 2 * CANDIDATE_ROW_BYTES);
    }

    #[test]
    fn test_json_tooling_helpers() {
        let ev = make_all_events().remove(0);
        let json = ev.to_json().unwrap();
        let back = Event::from_json(&json).unwrap();
        assert_eq!(back, ev);
    }

    #[test]
    fn durable_validation_rejects_poison_id_and_nonfinite_score() {
        let mut events = make_all_events();
        let lifecycle = events.remove(0);
        let Event::ExperimentLifecycle(mut lifecycle) = lifecycle else {
            unreachable!();
        };
        lifecycle.experiment_id = ExperimentId::from_digest(Digest::ZERO);
        assert!(Event::ExperimentLifecycle(lifecycle).validate().is_err());

        let mut score = make_all_events()
            .into_iter()
            .find_map(|event| match event {
                Event::PolicyScore(score) => Some(score),
                _ => None,
            })
            .unwrap();
        score.scores[0] = f32::NAN;
        assert!(Event::PolicyScore(score).validate().is_err());
    }

    #[test]
    fn json_import_rejects_structurally_valid_but_unauthoritative_event() {
        let json = serde_json::to_string(&Event::UtilityObservation(
            UtilityObservationEvent::default(),
        ))
        .unwrap();
        assert!(Event::from_json(&json).is_err());
    }
}
