//! Object-safe erased domain facade (§10.1, P5.2).
//!
//! [`ErasedDomain`] exposes every semantic method of the typed [`Domain`]
//! trait through arena-backed payload handles, so runtime-selected domains
//! run without serialization in the native hot path: typed payloads stay in
//! the [`EpisodeArena`], and the typed adapter ([`DomainAdapter`]) performs
//! the downcasts. Canonical serialization remains confined to durable or
//! process boundaries.

use crate::{
    ArtifactHandle, CandidateBatch, CandidateBatchBuilder, Domain, DomainCapabilities, DomainError,
    EpisodeArena, FeatureBatch, SolvedRoot, StateHandle, TransitionBatch, UtilityContext,
    UtilityObservation, VerificationHandle, VerifyBudget, VerifyError,
};
use reflex_types::{CandidateIndex, StateId, TaskId};
use std::any::Any;
use std::sync::Arc;

/// Object-safe domain facade (P5.2).
///
/// Implementations operate on arena handles and caller-owned buffers; no
/// serde/protobuf call occurs on the native expansion/apply hot path. The
/// typed [`DomainAdapter`] bridges a typed [`Domain`] to this interface.
///
/// Task and artifact payloads cross the erased boundary as `&dyn Any` and
/// arena handles: the caller that selected a concrete domain already holds
/// the concrete task type, and the adapter downcasts — typed execution and
/// erased execution therefore produce identical state/candidate ids and
/// artifacts (P5.2 AC).
#[async_trait::async_trait]
pub trait ErasedDomain: Send + Sync {
    fn capabilities(&self) -> DomainCapabilities;

    /// Canonical identity of the given task.
    fn task_id(&self, task: &(dyn Any + Send + Sync)) -> Result<TaskId, DomainError>;

    /// Inserts the initial state of `task` into `arena`.
    fn initial_state(
        &self,
        task: &(dyn Any + Send + Sync),
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

    /// Reconstructs the solved artifact into `arena` and returns its handle.
    fn reconstruct_artifact(
        &self,
        solved: SolvedRoot,
        arena: &mut EpisodeArena,
    ) -> Result<ArtifactHandle, DomainError>;

    /// Verifies the arena-resident artifact; the typed verification is
    /// stored into `arena` and its handle returned.
    fn verify(
        &self,
        artifact: ArtifactHandle,
        budget: VerifyBudget,
        arena: &mut EpisodeArena,
    ) -> Result<VerificationHandle, VerifyError>;

    /// Emits utility observations for the verified artifact.
    fn evaluate_utility(
        &self,
        artifact: ArtifactHandle,
        verification: VerificationHandle,
        context: &UtilityContext,
        arena: &EpisodeArena,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError>;
}

/// Typed adapter: erases a concrete [`Domain`] behind [`ErasedDomain`].
///
/// All downcasts happen inside the adapter; the runtime calls the erased
/// facade only through arena handles and caller-owned buffers (P5.2).
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

    fn task_id(&self, task: &(dyn Any + Send + Sync)) -> Result<TaskId, DomainError> {
        let task = task
            .downcast_ref::<D::Task>()
            .ok_or_else(|| DomainError::InvalidTask("erased task type mismatch".to_string()))?;
        self.domain.task_id(task)
    }

    fn initial_state(
        &self,
        task: &(dyn Any + Send + Sync),
        arena: &mut EpisodeArena,
    ) -> Result<StateHandle, DomainError> {
        let task = task
            .downcast_ref::<D::Task>()
            .ok_or_else(|| DomainError::InvalidTask("erased task type mismatch".to_string()))?;
        self.domain.initial_state(task, arena)
    }

    fn state_id(&self, state: StateHandle, arena: &EpisodeArena) -> Result<StateId, DomainError> {
        self.domain.state_id(state, arena)
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

    fn reconstruct_artifact(
        &self,
        solved: SolvedRoot,
        arena: &mut EpisodeArena,
    ) -> Result<ArtifactHandle, DomainError> {
        let artifact = self.domain.reconstruct_artifact(solved, arena)?;
        Ok(arena.insert_artifact(artifact))
    }

    fn verify(
        &self,
        artifact: ArtifactHandle,
        budget: VerifyBudget,
        arena: &mut EpisodeArena,
    ) -> Result<VerificationHandle, VerifyError> {
        let artifact = arena
            .resolve_artifact(artifact)
            .map_err(|e| VerifyError::Failed(format!("artifact access failed: {e}")))?;
        let artifact = artifact
            .downcast_ref::<D::Artifact>()
            .ok_or_else(|| VerifyError::Failed("erased artifact type mismatch".to_string()))?;
        let verification = self.domain.verify(artifact, budget)?;
        Ok(arena.insert_verification(verification))
    }

    fn evaluate_utility(
        &self,
        artifact: ArtifactHandle,
        verification: VerificationHandle,
        context: &UtilityContext,
        arena: &EpisodeArena,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError> {
        context.validate()?;
        let artifact = arena
            .resolve_artifact(artifact)
            .map_err(|e| DomainError::InvariantViolation(format!("artifact access failed: {e}")))?;
        let artifact = artifact.downcast_ref::<D::Artifact>().ok_or_else(|| {
            DomainError::InvariantViolation("erased artifact type mismatch".to_string())
        })?;
        let verification = arena.resolve_verification(verification).map_err(|e| {
            DomainError::InvariantViolation(format!("verification access failed: {e}"))
        })?;
        let verification = verification
            .downcast_ref::<D::Verification>()
            .ok_or_else(|| {
                DomainError::InvariantViolation("erased verification type mismatch".to_string())
            })?;
        let original_len = output.len();
        if let Err(error) = self
            .domain
            .evaluate_utility(artifact, verification, context, output)
        {
            output.truncate(original_len);
            return Err(error);
        }
        if let Some(error) = output[original_len..]
            .iter()
            .find_map(|observation| context.validate_observation(observation).err())
        {
            output.truncate(original_len);
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance::{FixtureDomain, FixtureTask};
    use crate::{TransitionBatch, TransitionOutcome, VerifyBudget};
    use reflex_types::Digest;

    fn fixture() -> (FixtureDomain, FixtureTask) {
        (
            FixtureDomain::new(),
            FixtureTask {
                start: 0,
                target: 3,
                candidates_per_state: 3,
                allow_duplicate_ids: false,
            },
        )
    }

    #[test]
    fn test_erased_domain_exposes_all_semantic_methods() {
        let (domain, task) = fixture();
        let erased = DomainAdapter::new(Arc::new(domain));
        let mut arena = EpisodeArena::new();

        // task_id
        let task_id = erased.task_id(&task).unwrap();
        assert_eq!(task_id, crate::conformance::fixture_task_id(&task).unwrap());

        // initial_state
        let root = erased.initial_state(&task, &mut arena).unwrap();
        let root_id = erased.state_id(root, &arena).unwrap();
        assert_eq!(
            root_id,
            crate::conformance::fixture_state_id(task.start, task.target).unwrap()
        );

        // enumerate_candidates
        let mut builder = CandidateBatchBuilder::new();
        erased
            .enumerate_candidates(root, &arena, &mut builder)
            .unwrap();
        assert_eq!(builder.len(), task.candidates_per_state);
        let batch = builder.build();

        // extract_features
        let cap = erased.capabilities();
        let mut features = FeatureBatch::new(
            batch.len(),
            cap.feature_dimension.max(1),
            cap.feature_schema,
        );
        erased
            .extract_features(&[root], &batch, &arena, &mut features)
            .unwrap();
        assert_eq!(features.rows, batch.len());

        // apply_candidates
        let selection = vec![CandidateIndex(0)];
        let mut transitions = TransitionBatch::new();
        erased
            .apply_candidates(root, &batch, &selection, &mut arena, &mut transitions)
            .unwrap();
        assert_eq!(transitions.outcomes.len(), 1);
        assert!(matches!(
            transitions.outcomes[0],
            TransitionOutcome::Obligations { .. }
        ));

        // reconstruct_artifact (through the arena)
        let solved = SolvedRoot {
            root_state: root,
            solved_edges: Vec::new(),
        };
        let ah = erased.reconstruct_artifact(solved, &mut arena).unwrap();
        assert_eq!(arena.artifact_count(), 1);

        // verify
        let budget = VerifyBudget {
            max_cpu_ns: 10_000_000,
            max_wall_ns: 10_000_000,
            max_memory_bytes: 1 << 20,
        };
        let vh = erased.verify(ah, budget, &mut arena).unwrap();
        assert_eq!(arena.verification_count(), 1);

        // evaluate_utility
        let mut observations = Vec::new();
        erased
            .evaluate_utility(
                ah,
                vh,
                &UtilityContext {
                    subject: reflex_types::ResearchNodeId::from_digest(Digest::hash_blake3(
                        b"erased-subject",
                    )),
                    population: Digest::hash_blake3(b"erased-population"),
                    observed_at_generation: reflex_types::GenerationId::from_digest(
                        Digest::hash_blake3(b"erased-generation"),
                    ),
                    accepted_verification: Digest::hash_blake3(b"erased-verification"),
                    accepted_evidence: vec![Digest::hash_blake3(b"erased-verification")],
                    cell_cpu_ns: 0,
                    model_inference_cpu_ns: 0,
                    retrieval_cpu_ns: 0,
                    verified_actions_count: 0,
                },
                &arena,
                &mut observations,
            )
            .unwrap();
        assert_eq!(observations.len(), 1);
        assert!(observations[0].value.is_finite());
    }

    #[test]
    fn test_erased_task_type_mismatch_rejected() {
        let (domain, task) = fixture();
        let erased = DomainAdapter::new(Arc::new(domain));
        let wrong: &(dyn Any + Send + Sync) = &"not a task".to_string();
        assert!(matches!(
            erased.task_id(wrong),
            Err(DomainError::InvalidTask(_))
        ));
        let _ = task;
    }
}
