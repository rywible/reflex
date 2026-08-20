//! Reflex domain SDK (§10).
//!
//! Defines the minimal contracts a verifiable world must implement:
//!
//! - [`Domain`] — the typed native interface (§10.1): canonical identity,
//!   batched candidate enumeration, batched application, artifact
//!   reconstruction, verification, and utility evaluation.
//! - [`ErasedDomain`] / [`DomainAdapter`] — the object-safe facade used by
//!   runtime-selected domains (P5.2); every semantic method of the typed
//!   trait is exposed, with typed payloads carried by arena handles so the
//!   native hot path performs no serialization.
//! - [`EpisodeArena`] — generation-stamped arena-backed payload handles;
//!   stale handles are detected by arena generation (P5.2 AC).
//! - The transition contract (§10.3) and verifier receipt (§10.5) — shared
//!   with `reflex-types`, re-exported here.
//! - [`conformance`] — the domain conformance kit and reference fixture
//!   domain (P5.6).
//!
//! # Verifier authority (INV-RFX-1)
//!
//! `Domain::verify` is the *only* admission path for an accepted artifact.
//! A search result is not solved without an accepted, well-formed
//! [`VerificationReceipt`] that names the verifier implementation digest,
//! the artifact digest, the semantic anchor, the verdict, and the resources
//! consumed (§10.5). Receipts must remain traceable: an [`Accepted`]
//! receipt carries a non-empty replay command and a non-zero implementation
//! digest (see [`VerificationReceipt::is_well_formed`]).
//!
//! [`Accepted`]: VerificationStatus::Accepted

use reflex_canonical::CanonicalEncode;
use reflex_types::{
    CandidateId, Digest, FeatureSchemaId, GenerationId, ResearchNodeId, StateId, TaskId,
};
use std::sync::Arc;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Shared contract types (owned by reflex-types; re-exported for compatibility)
// ---------------------------------------------------------------------------

/// Arena handle for a domain state (§10.1). Carries the issuing arena
/// generation; stale handles are rejected by every arena accessor.
pub use reflex_types::{CandidateHandle, CandidateIndex, StateHandle};

/// Transition contract (§10.3): one outcome per applied candidate.
pub use reflex_types::{
    AndGroupRef, DomainWitnessRef, InvalidCandidateCode, TransitionOutcome, UnresolvedCode,
};

/// Raw utility observation (§16.5, INV-RFX-7); owned by `reflex-economics`.
pub use reflex_economics::{
    BetterDirection, ConfidenceClass, RationalOrFloat, UnitRegistry, UtilityObservation,
};

mod arena;
mod capabilities;
pub mod conformance;
mod erased;
mod receipt;

pub use arena::{ArtifactHandle, EpisodeArena, VerificationHandle};
pub use capabilities::DomainCapabilities;
pub use erased::{DomainAdapter, ErasedDomain};
pub use receipt::{
    ReplayCommand, VerificationFailure, VerificationReceipt, VerificationStatus, VerifyBudget,
};

// ---------------------------------------------------------------------------
// Error taxonomy (§10.1, P5.1 AC)
// ---------------------------------------------------------------------------

/// Domain-side error classes (P5.1 AC: "error classes distinguish invalid
/// candidate, resource exhaustion, unresolved verification, internal
/// invariant, and infrastructure failure").
#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum DomainError {
    #[error("invalid task: {0}")]
    InvalidTask(String),
    #[error("invalid state handle {0}")]
    InvalidStateHandle(u32),
    #[error("invalid artifact handle {0}")]
    InvalidArtifactHandle(usize),
    #[error("invalid verification handle {0}")]
    InvalidVerificationHandle(usize),
    #[error(
        "stale state handle: index {index} issued in generation {issued_generation}, arena is in generation {current_generation}"
    )]
    StaleStateHandle {
        index: u32,
        issued_generation: u64,
        current_generation: u64,
    },
    #[error(
        "stale artifact handle: index {index} issued in generation {issued_generation}, arena is in generation {current_generation}"
    )]
    StaleArtifactHandle {
        index: u32,
        issued_generation: u64,
        current_generation: u64,
    },
    #[error(
        "stale verification handle: index {index} issued in generation {issued_generation}, arena is in generation {current_generation}"
    )]
    StaleVerificationHandle {
        index: u32,
        issued_generation: u64,
        current_generation: u64,
    },
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

/// Verifier-side failures. A `Timeout` or `BudgetExhausted` result is
/// unresolved/censored — never falsity (P5.5 AC).
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

// ---------------------------------------------------------------------------
// Batches (§12.1 caller-owned buffers)
// ---------------------------------------------------------------------------

/// Caller-owned structure-of-arrays builder for one state's candidates
/// (§10.2, §12.1). The hot enumeration path fills these buffers without
/// framework-side allocation.
pub struct CandidateBatchBuilder {
    pub ids: Vec<CandidateId>,
    pub classes: Vec<u16>,
    pub tie_breaks: Vec<u64>,
    pub payload_handles: Vec<CandidateHandle>,
    pub flags: Vec<u16>,
    hard_limit: Option<usize>,
    overflow_requested: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchCapacityError {
    #[error("candidate batch capacity exceeded: limit {limit}, requested {requested}")]
    CandidateCapacity { limit: usize, requested: usize },
}

impl CandidateBatchBuilder {
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            ids: Vec::with_capacity(capacity),
            classes: Vec::with_capacity(capacity),
            tie_breaks: Vec::with_capacity(capacity),
            payload_handles: Vec::with_capacity(capacity),
            flags: Vec::with_capacity(capacity),
            hard_limit: None,
            overflow_requested: None,
        }
    }

    /// Preallocate and enforce a hard row limit without permitting `Vec` to
    /// grow beyond it. Infallible domain adapters may continue calling
    /// [`Self::add`]; overflow is retained and returned by
    /// [`Self::capacity_error`] after enumeration.
    pub fn with_limit(limit: usize) -> Self {
        let mut builder = Self::with_capacity(limit);
        builder.hard_limit = Some(limit);
        builder
    }

    pub fn clear(&mut self) {
        self.ids.clear();
        self.classes.clear();
        self.tie_breaks.clear();
        self.payload_handles.clear();
        self.flags.clear();
        self.overflow_requested = None;
    }

    pub fn capacity(&self) -> usize {
        self.ids.capacity()
    }

    pub fn add(
        &mut self,
        id: CandidateId,
        class: u16,
        tie_break: u64,
        handle: CandidateHandle,
        flag: u16,
    ) {
        if let Some(limit) = self.hard_limit
            && self.len() >= limit
        {
            self.overflow_requested.get_or_insert(self.len() + 1);
            return;
        }
        self.ids.push(id);
        self.classes.push(class);
        self.tie_breaks.push(tie_break);
        self.payload_handles.push(handle);
        self.flags.push(flag);
    }

    pub fn try_add(
        &mut self,
        id: CandidateId,
        class: u16,
        tie_break: u64,
        handle: CandidateHandle,
        flag: u16,
        capacity: usize,
    ) -> Result<(), BatchCapacityError> {
        let requested = self.len() + 1;
        let effective_capacity = self
            .hard_limit
            .map_or(capacity, |limit| limit.min(capacity));
        if requested > effective_capacity {
            return Err(BatchCapacityError::CandidateCapacity {
                limit: effective_capacity,
                requested,
            });
        }
        self.add(id, class, tie_break, handle, flag);
        Ok(())
    }

    pub fn capacity_error(&self) -> Option<BatchCapacityError> {
        self.overflow_requested
            .map(|requested| BatchCapacityError::CandidateCapacity {
                limit: self.hard_limit.unwrap_or(self.len()),
                requested,
            })
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

    pub fn build_into(&mut self, out: &mut CandidateBatch) {
        let count = self.ids.len();
        out.group_offsets.clear();
        out.group_offsets.extend_from_slice(&[0, count as u32]);
        out.ids.clear();
        out.ids.append(&mut self.ids);
        out.classes.clear();
        out.classes.append(&mut self.classes);
        out.tie_breaks.clear();
        out.tie_breaks.append(&mut self.tie_breaks);
        out.payload_handles.clear();
        out.payload_handles.append(&mut self.payload_handles);
        out.flags.clear();
        out.flags.append(&mut self.flags);
    }
}

impl Default for CandidateBatchBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Structure-of-arrays candidate batch grouped by state (§12.1).
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

    pub fn group_count(&self) -> usize {
        self.group_offsets.len().saturating_sub(1)
    }

    pub fn group_range(&self, group: usize) -> Option<(usize, usize)> {
        let start = *self.group_offsets.get(group)? as usize;
        let end = *self.group_offsets.get(group + 1)? as usize;
        Some((start, end))
    }

    pub fn group_ids(&self, group: usize) -> Option<&[CandidateId]> {
        let (start, end) = self.group_range(group)?;
        Some(&self.ids[start..end])
    }

    pub fn group_classes(&self, group: usize) -> Option<&[u16]> {
        let (start, end) = self.group_range(group)?;
        Some(&self.classes[start..end])
    }
}

/// Row-major feature batch (§12.1). Caller-owned; the domain fills `values`.
#[derive(Clone, Debug)]
pub struct FeatureBatch {
    pub rows: usize,
    pub cols: usize,
    pub values: Vec<f32>,
    pub schema: FeatureSchemaId,
}

impl FeatureBatch {
    pub fn new(rows: usize, cols: usize, schema: FeatureSchemaId) -> Self {
        Self::with_capacity(rows, cols, schema)
    }

    pub fn with_capacity(rows: usize, cols: usize, schema: FeatureSchemaId) -> Self {
        Self {
            rows,
            cols,
            values: vec![0.0; rows * cols],
            schema,
        }
    }

    pub fn prepare(&mut self, rows: usize, cols: usize, schema: FeatureSchemaId) {
        self.rows = rows;
        self.cols = cols;
        self.schema = schema;
        let needed = rows * cols;
        if self.values.len() < needed {
            self.values.resize(needed, 0.0);
        }
        self.values[..needed].fill(0.0);
    }

    #[inline]
    pub fn row(&self, index: usize) -> &[f32] {
        let start = index * self.cols;
        &self.values[start..start + self.cols]
    }

    #[inline]
    pub fn row_mut(&mut self, index: usize) -> &mut [f32] {
        let start = index * self.cols;
        &mut self.values[start..start + self.cols]
    }
}

/// One [`TransitionOutcome`] per applied candidate (§10.3).
#[derive(Clone, Debug, Default)]
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

// ---------------------------------------------------------------------------
// Solved root (§10.1, §11.1)
// ---------------------------------------------------------------------------

/// The verified solution route of one search: the root state and every
/// retained edge of a completely solved AND-OR tree (§11.1).
#[derive(Clone, Debug)]
pub struct SolvedRoot {
    pub root_state: StateHandle,
    pub solved_edges: Vec<(StateHandle, CandidateHandle, Vec<StateHandle>)>,
}

// ---------------------------------------------------------------------------
// Utility observation context (§10.1, §16.5)
// ---------------------------------------------------------------------------

/// Accounting context handed to `evaluate_utility` so a domain can compute
/// honest net utility (INV-RFX-8: search cost includes ML).
#[derive(Clone, Debug)]
pub struct UtilityContext {
    pub subject: ResearchNodeId,
    pub population: Digest,
    pub observed_at_generation: GenerationId,
    pub accepted_verification: Digest,
    pub accepted_evidence: Vec<Digest>,
    pub cell_cpu_ns: u64,
    pub model_inference_cpu_ns: u64,
    pub retrieval_cpu_ns: u64,
    pub verified_actions_count: u32,
}

impl UtilityContext {
    pub fn validate(&self) -> Result<(), DomainError> {
        let zero_subject = ResearchNodeId::from_digest(Digest::ZERO);
        let zero_generation = GenerationId::from_digest(Digest::ZERO);
        let charged = self
            .model_inference_cpu_ns
            .checked_add(self.retrieval_cpu_ns)
            .ok_or_else(|| {
                DomainError::UtilityEvaluation("utility CPU accounting overflow".into())
            })?;
        if self.subject == zero_subject
            || self.population == Digest::ZERO
            || self.observed_at_generation == zero_generation
            || self.accepted_verification == Digest::ZERO
            || self.accepted_evidence.is_empty()
            || self.accepted_evidence.contains(&Digest::ZERO)
            || self
                .accepted_evidence
                .iter()
                .enumerate()
                .any(|(index, digest)| self.accepted_evidence[..index].contains(digest))
            || !self.accepted_evidence.contains(&self.accepted_verification)
            || charged > self.cell_cpu_ns
        {
            return Err(DomainError::UtilityEvaluation(
                "utility context requires non-zero subject, population, generation, accepted verification/evidence provenance, and consistent CPU accounting".into(),
            ));
        }
        Ok(())
    }

    pub fn validate_observation(
        &self,
        observation: &UtilityObservation,
    ) -> Result<(), DomainError> {
        self.validate()?;
        if observation.subject != self.subject
            || observation.population != self.population
            || observation.observed_at_generation != self.observed_at_generation
            || !observation.value.is_finite()
            || observation.evaluator.digest() == &Digest::ZERO
            || observation.metric.digest() == &Digest::ZERO
            || observation.unit.digest() == &Digest::ZERO
            || observation.evidence.contains(&Digest::ZERO)
            || self
                .accepted_evidence
                .iter()
                .any(|digest| !observation.evidence.contains(digest))
        {
            return Err(DomainError::UtilityEvaluation(
                "utility observation does not preserve its accepted context provenance".into(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Verifier trait (§10.1, P5.5)
// ---------------------------------------------------------------------------

/// Verifier authority (INV-RFX-1).
///
/// A domain must return an artifact through its own verification or through
/// an external verifier with the same semantic contract; acceptance is never
/// inferred from scores or tests. Receipts produced from
/// [`Self::Verification`] must be traceable via [`VerificationReceipt`].
pub trait Verifier: Send + Sync {
    type Artifact: CanonicalEncode;
    type Verification: CanonicalEncode;

    fn verify(
        &self,
        artifact: &Self::Artifact,
        budget: VerifyBudget,
    ) -> Result<Self::Verification, VerifyError>;
}

// ---------------------------------------------------------------------------
// Domain trait (§10.1)
// ---------------------------------------------------------------------------

/// Typed native domain interface (§10.1).
///
/// Associated types carry canonical identity and codecs; the trait separates
/// deterministic legality from learned scoring (scores are guidance, never
/// identity — §11.3). OR choices are expressed by [`apply_candidates`]
/// producing zero or more AND child obligations ([`TransitionOutcome::Obligations`],
/// §11.1). A domain cannot return an accepted artifact without verifier
/// evidence: acceptance enters through [`verify`] only (P5.1 AC, INV-RFX-1).
///
/// The candidate contract (§10.2): enumeration must be complete for the
/// declared action class, deterministic under the canonical state,
/// identity-stable, finite or explicitly capped with a registered generation
/// budget, independent of model scores, and unable to invoke hidden proof
/// search as an uncharged legality test.
///
/// [`apply_candidates`]: Domain::apply_candidates
/// [`verify`]: Domain::verify
pub trait Domain: Send + Sync + 'static {
    type Task: CanonicalEncode + Send + Sync + 'static;
    type State: Send + Sync + 'static;
    type Candidate: Send + Sync + 'static;
    type Transition: Send + Sync + 'static;
    type Artifact: CanonicalEncode + Send + Sync + 'static;
    type Verification: CanonicalEncode + Send + Sync + 'static;

    fn capabilities(&self) -> DomainCapabilities;
    fn task_id(&self, task: &Self::Task) -> Result<TaskId, DomainError>;
    fn initial_state(
        &self,
        task: &Self::Task,
        arena: &mut EpisodeArena,
    ) -> Result<StateHandle, DomainError>;
    fn state_id(&self, state: StateHandle, arena: &EpisodeArena) -> Result<StateId, DomainError>;

    /// Fills the caller-owned builder with this state's candidates (§10.2).
    fn enumerate_candidates(
        &self,
        state: StateHandle,
        arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError>;

    /// Fills the caller-owned feature batch for the given states/candidates.
    fn extract_features(
        &self,
        states: &[StateHandle],
        candidates: &CandidateBatch,
        arena: &EpisodeArena,
        output: &mut FeatureBatch,
    ) -> Result<(), DomainError>;

    /// Applies the selected candidates, appending one [`TransitionOutcome`]
    /// per candidate to `output` (§10.3).
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

    /// Emits raw, immutable utility observations for the verified artifact
    /// (§16.5, INV-RFX-7).
    fn evaluate_utility(
        &self,
        artifact: &Self::Artifact,
        verification: &Self::Verification,
        context: &UtilityContext,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError>;
}

/// Convenience for code that keeps an adapter behind an `Arc`.
pub fn erase<D: Domain>(domain: Arc<D>) -> Arc<dyn ErasedDomain> {
    Arc::new(DomainAdapter::new(domain))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_types::StateHandle;

    #[test]
    fn test_state_handle_generation() {
        let h = StateHandle::new(3, 42);
        assert_eq!(h.index(), 3);
        assert_eq!(h.generation(), 42);
        assert_ne!(h, StateHandle::new(3, 43));
    }

    #[test]
    fn utility_context_rejects_missing_or_inconsistent_provenance() {
        let verification = Digest::hash_blake3(b"accepted-verification");
        let mut context = UtilityContext {
            subject: ResearchNodeId::from_digest(Digest::hash_blake3(b"subject")),
            population: Digest::hash_blake3(b"population"),
            observed_at_generation: GenerationId::from_digest(Digest::hash_blake3(b"generation")),
            accepted_verification: verification,
            accepted_evidence: vec![verification],
            cell_cpu_ns: 10,
            model_inference_cpu_ns: 4,
            retrieval_cpu_ns: 3,
            verified_actions_count: 1,
        };
        assert!(context.validate().is_ok());
        context.population = Digest::ZERO;
        assert!(context.validate().is_err());
        context.population = Digest::hash_blake3(b"population");
        context.accepted_evidence.clear();
        assert!(context.validate().is_err());
        context.accepted_evidence.push(verification);
        context.retrieval_cpu_ns = 7;
        assert!(context.validate().is_err());
    }
}
