use crate::BitvecArtifact;
use crate::corpus::BitvecTask;
use crate::expr::BvExpr;
use crate::features::{BITVEC_FEATURE_DIM, extract_row};
use crate::rewrites::{BvCandidate, generate_rewrites};
use crate::verify::{BitvecVerification, verify_equivalent};
use reflex_domain::{
    CandidateBatch, CandidateBatchBuilder, CandidateHandle, CandidateIndex, Domain,
    DomainCapabilities, DomainError, DomainWitnessRef, EpisodeArena, FeatureBatch, SolvedRoot,
    StateHandle, TransitionBatch, TransitionOutcome, UtilityContext, UtilityObservation,
    VerifyBudget, VerifyError,
};
use reflex_economics::{BetterDirection, ConfidenceClass};
use reflex_types::{
    ActionSchemaId, ArtifactId, CandidateId, Digest, EvaluatorId, FeatureSchemaId,
    InvalidCandidateCode, MetricId, StateId, TaskId, UnitId,
};

/// Search state for a bit-vector task.
///
/// The task's closure target is part of state identity: dropping it would make
/// two tasks with the same expression but different optimization objectives
/// collide in the transposition cache.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BitvecState {
    pub expr: BvExpr,
    pub target_max_cost: u32,
}

impl reflex_canonical::CanonicalEncode for BitvecState {
    fn encode_canonical(
        &self,
        out: &mut reflex_canonical::CanonicalWriter,
    ) -> Result<(), reflex_canonical::CanonicalError> {
        self.expr.encode_canonical(out)?;
        out.write_u32(self.target_max_cost)
    }
}

#[derive(Clone)]
pub struct BitvecDomain {
    capabilities: DomainCapabilities,
}

impl BitvecDomain {
    pub fn new() -> Self {
        Self {
            capabilities: DomainCapabilities {
                domain_id: "bitvec-v1".to_string(),
                domain_digest: Digest::hash_blake3(b"bitvec-v1-domain"),
                action_schema: ActionSchemaId::from_digest(Digest::hash_blake3(b"bitvec-actions")),
                feature_schema: FeatureSchemaId::from_digest(Digest::hash_blake3(
                    b"bitvec-features-v2",
                )),
                feature_dimension: BITVEC_FEATURE_DIM,
                max_candidates_per_state: 64,
                deterministic_generation: true,
                supports_exact_cache: true,
            },
        }
    }

    fn expr_from_path(
        &self,
        state: StateHandle,
        solved: &SolvedRoot,
        arena: &EpisodeArena,
    ) -> Result<BvExpr, DomainError> {
        let mut cursor = state;
        let mut visited = std::collections::HashSet::new();
        loop {
            if !visited.insert(cursor) {
                return Err(DomainError::Application(
                    "solved path contains a state cycle".into(),
                ));
            }
            let current: &BitvecState = arena
                .get_state(cursor)
                .ok_or(DomainError::InvalidStateHandle(cursor.0))?;
            let Some((_, cand_handle, children)) = solved
                .solved_edges
                .iter()
                .find(|(candidate_state, _, _)| *candidate_state == cursor)
            else {
                return Ok(current.expr.clone());
            };
            let rewrites = generate_rewrites(&current.expr);
            let candidate = rewrites
                .get(cand_handle.0 as usize)
                .ok_or_else(|| DomainError::Application("missing rewrite".into()))?;
            if children.len() != 1 {
                return Ok(candidate.resulting_expr.clone());
            }
            cursor = children[0];
        }
    }
}

impl Default for BitvecDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl Domain for BitvecDomain {
    type Task = BitvecTask;
    type State = BitvecState;
    type Candidate = BvCandidate;
    type Transition = BvExpr;
    type Artifact = BitvecArtifact;
    type Verification = BitvecVerification;

    fn capabilities(&self) -> DomainCapabilities {
        self.capabilities.clone()
    }

    fn task_id(&self, task: &Self::Task) -> Result<TaskId, DomainError> {
        task.initial
            .validate()
            .map_err(|e| DomainError::InvalidTask(format!("{e:?}")))?;
        let digest = reflex_canonical::content_id(b"bitvec.task.v1", task)
            .map_err(|e| DomainError::InvalidTask(e.to_string()))?;
        Ok(TaskId::from_digest(digest))
    }

    fn initial_state(
        &self,
        task: &Self::Task,
        arena: &mut EpisodeArena,
    ) -> Result<StateHandle, DomainError> {
        task.initial
            .validate()
            .map_err(|e| DomainError::InvalidTask(format!("{e:?}")))?;
        let state = BitvecState {
            expr: task.initial.clone(),
            target_max_cost: task.target_max_cost,
        };
        let digest = reflex_canonical::content_id(b"bitvec.state.v2", &state)
            .map_err(|e| DomainError::InvalidTask(e.to_string()))?;
        let state_id = StateId::from_digest(digest);
        Ok(arena.insert_state(state, state_id))
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
        let state: &BitvecState = arena
            .get_state(state)
            .ok_or(DomainError::InvalidStateHandle(state.0))?;

        let rewrites = generate_rewrites(&state.expr);
        for (idx, r) in rewrites.into_iter().enumerate() {
            let digest = reflex_canonical::content_id(b"bitvec.cand.v1", &r)
                .map_err(|e| DomainError::Enumeration(e.to_string()))?;
            let cand_id = CandidateId::from_digest(digest);
            let handle = CandidateHandle(idx as u32);
            let class = if r.cost_delta < 0 { 0 } else { 1 };
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
        if states.is_empty() {
            return Ok(());
        }
        let state_handle = states[0];
        let state: &BitvecState = arena
            .get_state(state_handle)
            .ok_or(DomainError::InvalidStateHandle(state_handle.0))?;

        let rewrites = generate_rewrites(&state.expr);

        for row in 0..candidates.len() {
            let cand_opt = rewrites.get(row);
            let feat = extract_row(&state.expr, cand_opt);
            output.row_mut(row).copy_from_slice(&feat);
        }
        Ok(())
    }

    fn apply_candidates(
        &self,
        state: StateHandle,
        _candidates: &CandidateBatch,
        selection: &[CandidateIndex],
        arena: &mut EpisodeArena,
        output: &mut TransitionBatch,
    ) -> Result<(), DomainError> {
        let state: BitvecState = arena
            .get_state(state)
            .cloned()
            .ok_or(DomainError::InvalidStateHandle(state.0))?;

        let rewrites = generate_rewrites(&state.expr);

        for &CandidateIndex(idx) in selection {
            let Some(cand) = rewrites.get(idx) else {
                output.add(TransitionOutcome::invalid(InvalidCandidateCode::Other(
                    idx as u32,
                )));
                continue;
            };

            let next_state = BitvecState {
                expr: cand.resulting_expr.clone(),
                target_max_cost: state.target_max_cost,
            };
            let digest = reflex_canonical::content_id(b"bitvec.state.v2", &next_state)
                .map_err(|e| DomainError::Application(e.to_string()))?;
            let child_id = StateId::from_digest(digest);
            let child_handle = arena.insert_state(next_state.clone(), child_id);

            // A terminal edge is admitted only after the domain verifier has
            // checked the concrete rewrite. Search never treats an artifact
            // identifier alone as proof of closure (INV-RFX-1).
            if next_state.expr.cost() <= next_state.target_max_cost {
                let artifact = BitvecArtifact {
                    original: state.expr.clone(),
                    optimized: next_state.expr,
                };
                let verification = verify_equivalent(&artifact.original, &artifact.optimized);
                let artifact_digest =
                    reflex_canonical::content_id(b"bitvec.artifact.v1", &artifact)
                        .map_err(|e| DomainError::Application(e.to_string()))?;
                let verification_digest =
                    reflex_canonical::content_id(b"bitvec.verify.v1", &verification)
                        .map_err(|e| DomainError::Application(e.to_string()))?;
                let witness = DomainWitnessRef {
                    artifact: ArtifactId::from_digest(artifact_digest),
                    verification: Some(verification_digest),
                };
                if verification.is_equivalent {
                    output.add(TransitionOutcome::closed_with(witness));
                } else {
                    output.add(TransitionOutcome::contradiction_with(witness));
                }
            } else {
                let group_id =
                    u64::from_le_bytes(child_id.0.as_bytes()[..8].try_into().unwrap_or([0; 8]));
                output.add(TransitionOutcome::obligations(group_id, vec![child_handle]));
            }
        }
        Ok(())
    }

    fn reconstruct_artifact(
        &self,
        solved: SolvedRoot,
        arena: &EpisodeArena,
    ) -> Result<Self::Artifact, DomainError> {
        let original: &BitvecState = arena
            .get_state(solved.root_state)
            .ok_or(DomainError::InvalidStateHandle(solved.root_state.0))?;

        let optimized = if solved.solved_edges.is_empty() {
            original.expr.clone()
        } else {
            self.expr_from_path(solved.root_state, &solved, arena)?
        };

        Ok(BitvecArtifact {
            optimized,
            original: original.expr.clone(),
        })
    }

    fn verify(
        &self,
        artifact: &Self::Artifact,
        _budget: VerifyBudget,
    ) -> Result<Self::Verification, VerifyError> {
        artifact
            .original
            .validate()
            .map_err(|e| VerifyError::Failed(format!("{e:?}")))?;
        artifact
            .optimized
            .validate()
            .map_err(|e| VerifyError::Failed(format!("{e:?}")))?;
        Ok(verify_equivalent(&artifact.original, &artifact.optimized))
    }

    fn evaluate_utility(
        &self,
        artifact: &Self::Artifact,
        verification: &Self::Verification,
        context: &UtilityContext,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError> {
        context.validate()?;
        if verification.is_equivalent {
            let cost_orig = artifact.original.cost();
            let cost_opt = artifact.optimized.cost();
            let saved = if cost_orig > cost_opt {
                (cost_orig - cost_opt) as f64
            } else {
                0.0
            };

            output.push(UtilityObservation {
                subject: context.subject,
                evaluator: EvaluatorId::from_digest(Digest::hash_blake3(b"bitvec-evaluator")),
                metric: MetricId::from_digest(Digest::hash_blake3(b"ops_saved")),
                value: reflex_economics::RationalOrFloat::Float(saved),
                unit: UnitId::from_digest(Digest::hash_blake3(b"operations")),
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
    use crate::expr::ShiftKind;

    #[test]
    fn test_bitvec_exhaustive_verification() {
        let domain = BitvecDomain::new();

        let artifact_valid = BitvecArtifact {
            original: BvExpr::Xor(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0))),
            optimized: BvExpr::Const(0),
        };
        let res_valid = domain
            .verify(
                &artifact_valid,
                VerifyBudget {
                    max_cpu_ns: 1_000_000,
                    max_wall_ns: 1_000_000,
                    max_memory_bytes: 1024,
                },
            )
            .unwrap();
        assert!(res_valid.is_equivalent);
        assert_eq!(res_valid.counterexample, None);

        let artifact_invalid = BitvecArtifact {
            original: BvExpr::Add(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0))),
            optimized: BvExpr::Const(0),
        };
        let res_invalid = domain
            .verify(
                &artifact_invalid,
                VerifyBudget {
                    max_cpu_ns: 1_000_000,
                    max_wall_ns: 1_000_000,
                    max_memory_bytes: 1024,
                },
            )
            .unwrap();
        assert!(!res_invalid.is_equivalent);
        assert!(res_invalid.counterexample.is_some());
    }

    #[test]
    fn test_reconstruct_follows_solved_path() {
        let domain = BitvecDomain::new();
        let task = &crate::corpus::frozen_corpus()[0];
        let mut arena = EpisodeArena::new();
        let root = domain.initial_state(task, &mut arena).unwrap();
        let original = arena.get_state::<BitvecState>(root).unwrap().expr.clone();

        let rewrites = generate_rewrites(&original);
        let cand = &rewrites[0];
        let child_expr = cand.resulting_expr.clone();
        let child_state = BitvecState {
            expr: child_expr,
            target_max_cost: task.target_max_cost,
        };
        let child_digest = reflex_canonical::content_id(b"bitvec.state.v2", &child_state).unwrap();
        let child = arena.insert_state(child_state, StateId::from_digest(child_digest));

        let solved = SolvedRoot {
            root_state: root,
            solved_edges: vec![(root, CandidateHandle(0), vec![child])],
        };
        let artifact = domain.reconstruct_artifact(solved, &arena).unwrap();
        assert_eq!(artifact.optimized, BvExpr::Const(0));
        assert_eq!(artifact.original, original);
    }

    #[test]
    fn test_shift_select_in_language() {
        let expr = BvExpr::Select(
            Box::new(BvExpr::Var(0)),
            Box::new(BvExpr::Shift {
                kind: ShiftKind::LogicalLeft,
                value: Box::new(BvExpr::Const(1)),
                amount: Box::new(BvExpr::Const(2)),
            }),
            Box::new(BvExpr::Const(0)),
        );
        assert!(expr.validate().is_ok());
        let domain = BitvecDomain::new();
        let task = BitvecTask {
            initial: expr,
            target_max_cost: 3,
        };
        assert!(domain.task_id(&task).is_ok());
    }

    #[test]
    fn task_target_is_preserved_in_state_identity_and_closure() {
        let domain = BitvecDomain::new();
        let expr = BvExpr::Add(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Const(0)));
        let mut strict_arena = EpisodeArena::new();
        let strict = domain
            .initial_state(
                &BitvecTask {
                    initial: expr.clone(),
                    target_max_cost: 0,
                },
                &mut strict_arena,
            )
            .unwrap();
        let mut permissive_arena = EpisodeArena::new();
        let permissive = domain
            .initial_state(
                &BitvecTask {
                    initial: expr,
                    target_max_cost: 1,
                },
                &mut permissive_arena,
            )
            .unwrap();
        assert_ne!(
            domain.state_id(strict, &strict_arena).unwrap(),
            domain.state_id(permissive, &permissive_arena).unwrap()
        );

        let mut strict_candidates = CandidateBatchBuilder::new();
        domain
            .enumerate_candidates(strict, &strict_arena, &mut strict_candidates)
            .unwrap();
        let strict_candidates = strict_candidates.build();
        let mut strict_transitions = TransitionBatch::new();
        domain
            .apply_candidates(
                strict,
                &strict_candidates,
                &[CandidateIndex(0)],
                &mut strict_arena,
                &mut strict_transitions,
            )
            .unwrap();
        assert!(matches!(
            strict_transitions.outcomes.first(),
            Some(TransitionOutcome::Obligations { .. })
        ));

        let mut candidates = CandidateBatchBuilder::new();
        domain
            .enumerate_candidates(permissive, &permissive_arena, &mut candidates)
            .unwrap();
        let candidates = candidates.build();
        let mut transitions = TransitionBatch::new();
        domain
            .apply_candidates(
                permissive,
                &candidates,
                &[CandidateIndex(0)],
                &mut permissive_arena,
                &mut transitions,
            )
            .unwrap();
        assert!(matches!(
            transitions.outcomes.first(),
            Some(TransitionOutcome::Closed { .. })
        ));
    }
}
