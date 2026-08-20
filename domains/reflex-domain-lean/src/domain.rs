use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter, content_id};
use reflex_domain::{
    CandidateBatch, CandidateBatchBuilder, CandidateHandle, CandidateIndex, Domain,
    DomainCapabilities, DomainError, EpisodeArena, FeatureBatch, SolvedRoot, StateHandle,
    TransitionBatch, UtilityContext, UtilityObservation, VerifyBudget, VerifyError,
};
use reflex_economics::{BetterDirection, ConfidenceClass};
use reflex_types::{
    ActionSchemaId, CandidateId, Digest, EvaluatorId, FeatureSchemaId, MetricId, StateId, TaskId,
    UnitId,
};
use serde::{Deserialize, Serialize};

use crate::kernel::{
    KernelReceipt, KernelSandboxConfig, verify_with_kernel, verify_with_kernel_sandboxed,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanTask {
    pub theorem_name: String,
    pub module: String,
    pub goal_state: String,
}

impl CanonicalEncode for LeanTask {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.theorem_name)?;
        out.write_str(&self.module)?;
        out.write_str(&self.goal_state)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanGoalState {
    pub theorem_name: String,
    /// Immutable declared theorem type for kernel replay (INV-RFX-1).
    pub theorem_statement: String,
    pub hyps: Vec<(String, String)>,
    /// Current local goal under search (may narrow after intros).
    pub target: String,
    pub proof_prefix: Vec<String>,
}

impl CanonicalEncode for LeanGoalState {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.theorem_name)?;
        out.write_str(&self.theorem_statement)?;
        out.write_u32(self.hyps.len() as u32)?;
        for (name, ty) in &self.hyps {
            out.write_str(name)?;
            out.write_str(ty)?;
        }
        out.write_str(&self.target)?;
        out.write_u32(self.proof_prefix.len() as u32)?;
        for p in &self.proof_prefix {
            out.write_str(p)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanProofArtifact {
    pub theorem_name: String,
    /// Real theorem statement verified by the kernel (INV-RFX-1).
    pub theorem_statement: String,
    pub proof_script: String,
    pub replay_ref: Option<String>,
}

impl CanonicalEncode for LeanProofArtifact {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.theorem_name)?;
        out.write_str(&self.theorem_statement)?;
        out.write_str(&self.proof_script)?;
        if let Some(r) = &self.replay_ref {
            out.write_str(r)?;
        } else {
            out.write_str("")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanKernelReceipt {
    pub kernel_certified: bool,
    pub axioms_used: Vec<String>,
    pub kernel_cpu_ns: u64,
}

impl CanonicalEncode for LeanKernelReceipt {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_bool(self.kernel_certified)?;
        out.write_u32(self.axioms_used.len() as u32)?;
        for a in &self.axioms_used {
            out.write_str(a)?;
        }
        out.write_u64(self.kernel_cpu_ns)?;
        Ok(())
    }
}

impl From<KernelReceipt> for LeanKernelReceipt {
    fn from(r: KernelReceipt) -> Self {
        Self {
            kernel_certified: r.kernel_certified,
            axioms_used: r.axioms_used,
            kernel_cpu_ns: r.kernel_cpu_ns,
        }
    }
}

#[derive(Clone)]
pub struct LeanDomain {
    capabilities: DomainCapabilities,
    kernel_sandbox: Option<KernelSandboxConfig>,
}

impl LeanDomain {
    pub fn new() -> Self {
        Self {
            capabilities: DomainCapabilities {
                domain_id: "lean4-reflex-v2".to_string(),
                domain_digest: Digest::hash_blake3(b"lean4-reflex-v2"),
                action_schema: ActionSchemaId::from_digest(Digest::hash_blake3(b"lean-tactics")),
                feature_schema: FeatureSchemaId::from_digest(Digest::hash_blake3(b"lean-features")),
                feature_dimension: 8,
                max_candidates_per_state: 128,
                deterministic_generation: true,
                supports_exact_cache: true,
            },
            kernel_sandbox: None,
        }
    }

    pub fn with_kernel_sandbox(config: KernelSandboxConfig) -> Self {
        Self {
            kernel_sandbox: Some(config),
            ..Self::new()
        }
    }
}

impl Default for LeanDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl Domain for LeanDomain {
    type Task = LeanTask;
    type State = LeanGoalState;
    type Candidate = String;
    type Transition = String;
    type Artifact = LeanProofArtifact;
    type Verification = LeanKernelReceipt;

    fn capabilities(&self) -> DomainCapabilities {
        self.capabilities.clone()
    }

    fn task_id(&self, task: &Self::Task) -> Result<TaskId, DomainError> {
        let digest = content_id(b"lean.task.v1", task)
            .map_err(|e| DomainError::InvalidTask(e.to_string()))?;
        Ok(TaskId::from_digest(digest))
    }

    fn initial_state(
        &self,
        task: &Self::Task,
        arena: &mut EpisodeArena,
    ) -> Result<StateHandle, DomainError> {
        let init_goal = LeanGoalState {
            theorem_name: task.theorem_name.clone(),
            theorem_statement: task.goal_state.clone(),
            hyps: Vec::new(),
            target: task.goal_state.clone(),
            proof_prefix: Vec::new(),
        };
        let digest = content_id(b"lean.state.v1", &init_goal)
            .map_err(|e| DomainError::InvalidTask(e.to_string()))?;
        let state_id = StateId::from_digest(digest);
        Ok(arena.insert_state(init_goal, state_id))
    }

    fn state_id(&self, state: StateHandle, arena: &EpisodeArena) -> Result<StateId, DomainError> {
        arena
            .get_state_id(state)
            .ok_or(DomainError::InvalidStateHandle(state.0))
    }

    fn enumerate_candidates(
        &self,
        state: StateHandle,
        arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError> {
        let goal: &LeanGoalState = arena
            .get_state(state)
            .ok_or(DomainError::InvalidStateHandle(state.0))?;

        let mut tactics = Vec::new();

        if goal.target.starts_with("forall")
            || goal.target.starts_with('∀')
            || goal.target.contains("->")
        {
            tactics.push("intro h");
            tactics.push("intro a b");
        }

        for (name, ty) in &goal.hyps {
            if ty == &goal.target {
                tactics.push(name.as_str());
            }
        }

        if goal.target.contains('=') {
            let parts: Vec<&str> = goal.target.split('=').map(|s| s.trim()).collect();
            if parts.len() == 2 && parts[0] == parts[1] {
                tactics.push("rfl");
            }
        }

        tactics.push("omega");
        tactics.push("simp");
        tactics.push("ring");
        tactics.push("exact h");
        tactics.push("apply Eq.symm");

        let mut seen = std::collections::HashSet::new();
        let mut deduped = Vec::new();
        for t in tactics {
            if seen.insert(t) {
                deduped.push(t);
            }
        }

        for (idx, &t) in deduped.iter().enumerate() {
            let digest = content_id(b"lean.tactic.v1", &t.to_string())
                .map_err(|e| DomainError::Enumeration(e.to_string()))?;
            let cand_id = CandidateId::from_digest(digest);
            let handle = CandidateHandle(idx as u32);
            let class = match t {
                "omega" | "rfl" | "exact h" => 0,
                "intro h" | "intro a b" => 1,
                "simp" | "ring" => 2,
                _ => 3,
            };
            output.add(cand_id, class, idx as u64, handle, 0);
        }

        Ok(())
    }

    fn extract_features(
        &self,
        states: &[StateHandle],
        candidates: &CandidateBatch,
        arena: &EpisodeArena,
        output: &mut FeatureBatch,
    ) -> Result<(), DomainError> {
        let state_handle = states.first().copied().unwrap_or(StateHandle(0, 0));
        let goal: Option<&LeanGoalState> = arena.get_state(state_handle);

        let (num_hyps, target_len, is_forall) = if let Some(g) = goal {
            (
                g.hyps.len() as f32,
                g.target.len() as f32,
                if g.target.starts_with('∀') || g.target.starts_with("forall") {
                    1.0
                } else {
                    0.0
                },
            )
        } else {
            (0.0, 10.0, 0.0)
        };

        for row in 0..candidates.len() {
            let row_slice = output.row_mut(row);
            let class = candidates.classes.get(row).copied().unwrap_or(0);
            row_slice[0] = num_hyps;
            row_slice[1] = target_len;
            row_slice[2] = is_forall;
            row_slice[3] = (class as f32) * 2.0 + 1.0;
            row_slice[4] = if class == 0 { 5.0 } else { 1.0 };
            row_slice[5] = target_len * 0.05;
            row_slice[6] = num_hyps * 0.2;
            row_slice[7] = 1.0 / (target_len + 1.0);
        }
        Ok(())
    }

    fn apply_candidates(
        &self,
        state: StateHandle,
        _candidates: &CandidateBatch,
        _selection: &[CandidateIndex],
        arena: &mut EpisodeArena,
        _output: &mut TransitionBatch,
    ) -> Result<(), DomainError> {
        let _: &LeanGoalState = arena
            .get_state(state)
            .ok_or(DomainError::InvalidStateHandle(state.0))?;
        Err(DomainError::Application(
            "VerifierUnavailable: tactic application requires the external Project Reflex Lean worker"
                .to_string(),
        ))
    }

    fn reconstruct_artifact(
        &self,
        solved: SolvedRoot,
        arena: &EpisodeArena,
    ) -> Result<Self::Artifact, DomainError> {
        let goal: &LeanGoalState = arena
            .get_state(solved.root_state)
            .ok_or(DomainError::InvalidStateHandle(solved.root_state.0))?;

        if goal.theorem_name.trim().is_empty() {
            return Err(DomainError::Application(
                "reconstruct: missing theorem_name on goal".to_string(),
            ));
        }
        if goal.theorem_statement.trim().is_empty() {
            return Err(DomainError::Application(
                "reconstruct: missing theorem_statement on goal".to_string(),
            ));
        }
        // Replay only the search trajectory — never invent closing tactics.
        let script = goal.proof_prefix.join("; ");
        if script.trim().is_empty() {
            return Err(DomainError::Application(
                "reconstruct: empty proof trajectory".to_string(),
            ));
        }

        let replay_ref = content_id(b"lean.replay.v1", &script)
            .ok()
            .map(|d| d.to_string());

        Ok(LeanProofArtifact {
            theorem_name: goal.theorem_name.clone(),
            theorem_statement: goal.theorem_statement.clone(),
            proof_script: script,
            replay_ref,
        })
    }

    fn verify(
        &self,
        artifact: &Self::Artifact,
        _budget: VerifyBudget,
    ) -> Result<Self::Verification, VerifyError> {
        let receipt = match &self.kernel_sandbox {
            Some(config) => verify_with_kernel_sandboxed(
                config,
                &artifact.theorem_name,
                &artifact.theorem_statement,
                &artifact.proof_script,
            )?,
            None => verify_with_kernel(
                &artifact.theorem_name,
                &artifact.theorem_statement,
                &artifact.proof_script,
            )?,
        };
        if !receipt.kernel_certified {
            return Err(VerifyError::Failed(
                "kernel did not certify proof".to_string(),
            ));
        }
        Ok(receipt.into())
    }

    fn evaluate_utility(
        &self,
        _artifact: &Self::Artifact,
        verification: &Self::Verification,
        context: &UtilityContext,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError> {
        context.validate()?;
        if verification.kernel_certified {
            output.push(UtilityObservation {
                subject: context.subject,
                evaluator: EvaluatorId::from_digest(Digest::hash_blake3(b"lean-evaluator")),
                metric: MetricId::from_digest(Digest::hash_blake3(b"theorems_solved")),
                value: reflex_economics::RationalOrFloat::Float(1.0),
                unit: UnitId::from_digest(Digest::hash_blake3(b"theorems")),
                direction: BetterDirection::HigherIsBetter,
                population: context.population,
                evidence: context.accepted_evidence.clone(),
                confidence: ConfidenceClass::ObservedExact,
                observed_at_generation: context.observed_at_generation,
                restricted_work: false,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::is_lean_available;

    #[test]
    fn test_lean_verifier_rejects_invalid_script() {
        let domain = LeanDomain::new();
        let invalid_artifact = LeanProofArtifact {
            theorem_name: "Nat.add_comm".to_string(),
            theorem_statement: "∀ (a b : Nat), a + b = b + a".to_string(),
            proof_script: "invalid gibberish without tactics".to_string(),
            replay_ref: None,
        };
        let res = domain.verify(
            &invalid_artifact,
            VerifyBudget {
                max_cpu_ns: 1000,
                max_wall_ns: 1000,
                max_memory_bytes: 1000,
            },
        );
        assert!(res.is_err());
    }

    #[test]
    fn test_lean_never_fabricates_without_kernel() {
        if is_lean_available() {
            return;
        }
        let domain = LeanDomain::new();
        let artifact = LeanProofArtifact {
            theorem_name: "Nat.add_comm".to_string(),
            theorem_statement: "∀ (a b : Nat), a + b = b + a".to_string(),
            proof_script: "intro a b; omega".to_string(),
            replay_ref: None,
        };
        let res = domain.verify(
            &artifact,
            VerifyBudget {
                max_cpu_ns: 1000,
                max_wall_ns: 1000,
                max_memory_bytes: 1000,
            },
        );
        assert!(res.is_err());
        match res {
            Err(VerifyError::Unresolved(msg)) => assert!(msg.contains("VerifierUnavailable")),
            Err(VerifyError::Failed(_)) => {}
            Ok(r) => panic!("must not fabricate kernel_certified: {r:?}"),
            _ => panic!("unexpected error type"),
        }
    }

    #[test]
    fn test_toy_transition_closure_is_unavailable() {
        let domain = LeanDomain::new();
        let task = LeanTask {
            theorem_name: "example".to_string(),
            module: "External.ProjectReflex".to_string(),
            goal_state: "∀ (a : Nat), a = a".to_string(),
        };
        let mut arena = EpisodeArena::new();
        let state = domain.initial_state(&task, &mut arena).unwrap();
        let mut builder = CandidateBatchBuilder::new();
        domain
            .enumerate_candidates(state, &arena, &mut builder)
            .unwrap();
        let candidates = builder.build();
        let mut transitions = TransitionBatch::new();
        let result = domain.apply_candidates(
            state,
            &candidates,
            &[CandidateIndex(0)],
            &mut arena,
            &mut transitions,
        );
        assert!(matches!(
            result,
            Err(DomainError::Application(message)) if message.contains("VerifierUnavailable")
        ));
        assert!(transitions.outcomes.is_empty());
    }
}
