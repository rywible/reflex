use reflex_canonical::CanonicalEncode;
use reflex_types::{
    ActionSchemaId, ArtifactId, AssumptionId, CandidateId, Digest, FeatureSchemaId, ResearchNodeId,
    StateId, TaskId, UnitId, VerifierId,
};
use serde::{Deserialize, Serialize};
use std::any::Any;
use std::sync::Arc;
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("invalid task: {0}")]
    InvalidTask(String),
    #[error("invalid state handle {0}")]
    InvalidStateHandle(u32),
    #[error("invalid candidate index {0}")]
    InvalidCandidateIndex(usize),
    #[error("candidate enumeration error: {0}")]
    Enumeration(String),
    #[error("candidate application error: {0}")]
    Application(String),
    #[error("feature extraction error: {0}")]
    FeatureExtraction(String),
    #[error("artifact reconstruction error: {0}")]
    Reconstruction(String),
    #[error("utility evaluation error: {0}")]
    UtilityEvaluation(String),
    #[error("internal domain invariant violated: {0}")]
    InvariantViolation(String),
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    #[error("verification failed: {0}")]
    Failed(String),
    #[error("verifier timeout after {0} ns")]
    Timeout(u64),
    #[error("budget exhausted: {0}")]
    BudgetExhausted(String),
    #[error("unresolved verification: {0}")]
    Unresolved(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateHandle(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateHandle(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateIndex(pub usize);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainCapabilities {
    pub domain_id: String,
    pub domain_digest: Digest,
    pub action_schema: ActionSchemaId,
    pub feature_schema: FeatureSchemaId,
    pub feature_dimension: usize,
    pub max_candidates_per_state: u32,
    pub deterministic_generation: bool,
    pub supports_exact_cache: bool,
}

#[derive(Default)]
pub struct EpisodeArena {
    states: Vec<Box<dyn Any + Send + Sync>>,
    candidates: Vec<Box<dyn Any + Send + Sync>>,
    state_ids: Vec<StateId>,
}

impl EpisodeArena {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_state<T: Any + Send + Sync>(&mut self, state: T, id: StateId) -> StateHandle {
        let handle = StateHandle(self.states.len() as u32);
        self.states.push(Box::new(state));
        self.state_ids.push(id);
        handle
    }

    pub fn get_state<T: Any>(&self, handle: StateHandle) -> Option<&T> {
        self.states.get(handle.0 as usize)?.downcast_ref::<T>()
    }

    pub fn get_state_id(&self, handle: StateHandle) -> Option<StateId> {
        self.state_ids.get(handle.0 as usize).copied()
    }

    pub fn insert_candidate<T: Any + Send + Sync>(&mut self, cand: T) -> CandidateHandle {
        let handle = CandidateHandle(self.candidates.len() as u32);
        self.candidates.push(Box::new(cand));
        handle
    }

    pub fn get_candidate<T: Any>(&self, handle: CandidateHandle) -> Option<&T> {
        self.candidates.get(handle.0 as usize)?.downcast_ref::<T>()
    }

    pub fn clear(&mut self) {
        self.states.clear();
        self.candidates.clear();
        self.state_ids.clear();
    }
}

pub struct CandidateBatchBuilder {
    pub ids: Vec<CandidateId>,
    pub classes: Vec<u16>,
    pub tie_breaks: Vec<u64>,
    pub payload_handles: Vec<CandidateHandle>,
    pub flags: Vec<u16>,
}

impl CandidateBatchBuilder {
    pub fn new() -> Self {
        Self {
            ids: Vec::new(),
            classes: Vec::new(),
            tie_breaks: Vec::new(),
            payload_handles: Vec::new(),
            flags: Vec::new(),
        }
    }

    pub fn add(
        &mut self,
        id: CandidateId,
        class: u16,
        tie_break: u64,
        handle: CandidateHandle,
        flag: u16,
    ) {
        self.ids.push(id);
        self.classes.push(class);
        self.tie_breaks.push(tie_break);
        self.payload_handles.push(handle);
        self.flags.push(flag);
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn build(self) -> CandidateBatch {
        let count = self.ids.len();
        CandidateBatch {
            group_offsets: vec![0, count as u32],
            ids: self.ids,
            classes: self.classes,
            tie_breaks: self.tie_breaks,
            payload_handles: self.payload_handles,
            flags: self.flags,
        }
    }
}

impl Default for CandidateBatchBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Default)]
pub struct CandidateBatch {
    pub group_offsets: Vec<u32>,
    pub ids: Vec<CandidateId>,
    pub classes: Vec<u16>,
    pub tie_breaks: Vec<u64>,
    pub payload_handles: Vec<CandidateHandle>,
    pub flags: Vec<u16>,
}

impl CandidateBatch {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct FeatureBatch {
    pub rows: usize,
    pub cols: usize,
    pub values: Vec<f32>,
    pub schema: FeatureSchemaId,
}

impl FeatureBatch {
    pub fn new(rows: usize, cols: usize, schema: FeatureSchemaId) -> Self {
        Self {
            rows,
            cols,
            values: vec![0.0; rows * cols],
            schema,
        }
    }

    pub fn row(&self, index: usize) -> &[f32] {
        let start = index * self.cols;
        &self.values[start..start + self.cols]
    }

    pub fn row_mut(&mut self, index: usize) -> &mut [f32] {
        let start = index * self.cols;
        &mut self.values[start..start + self.cols]
    }
}

#[derive(Clone, Debug)]
pub enum TransitionOutcome {
    Closed,
    Obligations { and_child_states: Vec<StateHandle> },
    Contradiction,
    Invalid { code: u32 },
    Unresolved { code: u32 },
}

#[derive(Clone, Debug)]
pub struct TransitionBatch {
    pub outcomes: Vec<TransitionOutcome>,
}

impl TransitionBatch {
    pub fn new() -> Self {
        Self {
            outcomes: Vec::new(),
        }
    }

    pub fn add(&mut self, outcome: TransitionOutcome) {
        self.outcomes.push(outcome);
    }
}

impl Default for TransitionBatch {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug)]
pub struct SolvedRoot {
    pub root_state: StateHandle,
    pub solved_edges: Vec<(StateHandle, CandidateHandle, Vec<StateHandle>)>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifyBudget {
    pub max_cpu_ns: u64,
    pub max_wall_ns: u64,
    pub max_memory_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReplayCommand {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerificationReceipt {
    pub verifier: VerifierId,
    pub implementation: Digest,
    pub semantic_anchor: Digest,
    pub artifact: ArtifactId,
    pub status: String, // "Verified", "Counterexample", "Timeout"
    pub proof_or_certificate: Option<Digest>,
    pub assumptions: Vec<AssumptionId>,
    pub cpu_ns: u64,
    pub wall_ns: u64,
    pub peak_rss_bytes: u64,
    pub replay_command: ReplayCommand,
}

#[derive(Clone, Debug)]
pub struct UtilityContext {
    pub cell_cpu_ns: u64,
    pub model_inference_cpu_ns: u64,
    pub retrieval_cpu_ns: u64,
    pub verified_actions_count: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UtilityObservation {
    pub subject: ResearchNodeId,
    pub metric: reflex_types::MetricId,
    pub value: f64,
    pub unit: UnitId,
    pub direction: String, // "Minimize", "Maximize"
}

pub trait Verifier: Send + Sync {
    type Artifact: CanonicalEncode;
    type Verification: CanonicalEncode;

    fn verify(
        &self,
        artifact: &Self::Artifact,
        budget: VerifyBudget,
    ) -> Result<Self::Verification, VerifyError>;
}

pub trait Domain: Send + Sync + 'static {
    type Task: CanonicalEncode + Send + Sync;
    type State: Send + Sync;
    type Candidate: Send + Sync;
    type Transition: Send + Sync;
    type Artifact: CanonicalEncode + Send + Sync;
    type Verification: CanonicalEncode + Send + Sync;

    fn capabilities(&self) -> DomainCapabilities;
    fn task_id(&self, task: &Self::Task) -> Result<TaskId, DomainError>;
    fn initial_state(
        &self,
        task: &Self::Task,
        arena: &mut EpisodeArena,
    ) -> Result<StateHandle, DomainError>;
    fn state_id(&self, state: StateHandle, arena: &EpisodeArena) -> Result<StateId, DomainError>;

    fn enumerate_candidates(
        &self,
        state: StateHandle,
        arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError>;

    fn extract_features(
        &self,
        states: &[StateHandle],
        candidates: &CandidateBatch,
        arena: &EpisodeArena,
        output: &mut FeatureBatch,
    ) -> Result<(), DomainError>;

    fn apply_candidates(
        &self,
        state: StateHandle,
        candidates: &CandidateBatch,
        selection: &[CandidateIndex],
        arena: &mut EpisodeArena,
        output: &mut TransitionBatch,
    ) -> Result<(), DomainError>;

    fn reconstruct_artifact(
        &self,
        solved: SolvedRoot,
        arena: &EpisodeArena,
    ) -> Result<Self::Artifact, DomainError>;

    fn verify(
        &self,
        artifact: &Self::Artifact,
        budget: VerifyBudget,
    ) -> Result<Self::Verification, VerifyError>;

    fn evaluate_utility(
        &self,
        artifact: &Self::Artifact,
        verification: &Self::Verification,
        context: &UtilityContext,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError>;
}

#[async_trait::async_trait]
pub trait ErasedDomain: Send + Sync {
    fn capabilities(&self) -> DomainCapabilities;

    fn enumerate_candidates(
        &self,
        state: StateHandle,
        arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError>;

    fn extract_features(
        &self,
        states: &[StateHandle],
        candidates: &CandidateBatch,
        arena: &EpisodeArena,
        output: &mut FeatureBatch,
    ) -> Result<(), DomainError>;

    fn apply_candidates(
        &self,
        state: StateHandle,
        candidates: &CandidateBatch,
        selection: &[CandidateIndex],
        arena: &mut EpisodeArena,
        output: &mut TransitionBatch,
    ) -> Result<(), DomainError>;
}

pub struct DomainAdapter<D: Domain> {
    domain: Arc<D>,
}

impl<D: Domain> DomainAdapter<D> {
    pub fn new(domain: Arc<D>) -> Self {
        Self { domain }
    }
}

#[async_trait::async_trait]
impl<D: Domain> ErasedDomain for DomainAdapter<D> {
    fn capabilities(&self) -> DomainCapabilities {
        self.domain.capabilities()
    }

    fn enumerate_candidates(
        &self,
        state: StateHandle,
        arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError> {
        self.domain.enumerate_candidates(state, arena, output)
    }

    fn extract_features(
        &self,
        states: &[StateHandle],
        candidates: &CandidateBatch,
        arena: &EpisodeArena,
        output: &mut FeatureBatch,
    ) -> Result<(), DomainError> {
        self.domain
            .extract_features(states, candidates, arena, output)
    }

    fn apply_candidates(
        &self,
        state: StateHandle,
        candidates: &CandidateBatch,
        selection: &[CandidateIndex],
        arena: &mut EpisodeArena,
        output: &mut TransitionBatch,
    ) -> Result<(), DomainError> {
        self.domain
            .apply_candidates(state, candidates, selection, arena, output)
    }
}
