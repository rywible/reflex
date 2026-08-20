use crate::{
    BudgetFired, BudgetSet, FrontierKey, OrderedScore, SearchError, SearchKernel, SearchPolicy,
    SearchStats,
};
use reflex_domain::{Domain, VerifyBudget};
use reflex_types::{
    ArtifactId, CandidateId, Digest, ModelCheckpointId, StateId, TaskId, UnresolvedCode,
};
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::sync::Mutex;
use std::time::Instant;

fn seconds_to_ns(seconds: f64) -> u64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        return 0;
    }
    let nanos = seconds * 1_000_000_000.0;
    nanos.min(u64::MAX as f64) as u64
}

/// Replay mode cost ladder (§11.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplayMode {
    Logical,
    Policy,
    FullVerifier,
}

/// One recorded frontier decision.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TranscriptDecision {
    pub step: usize,
    pub score_bits: u32,
    pub logical_cost: u32,
    pub depth: u32,
    pub state_id: StateId,
    pub candidate_id: CandidateId,
    pub insertion_seq: u64,
}

/// Candidate enumeration and exact score bits observed for one expanded
/// state. Logical replay consumes the recorded score bits but must reproduce
/// the state and candidate batch from the immutable domain input.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptExpansion {
    pub state_id: StateId,
    pub candidate_ids: Vec<CandidateId>,
    pub score_bits: Vec<u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptWitness {
    pub artifact: ArtifactId,
    pub verification: Option<Digest>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TranscriptTransitionOutcome {
    Closed {
        witness: TranscriptWitness,
    },
    Obligations {
        group_id: u64,
        children: Vec<StateId>,
    },
    Contradiction {
        certificate: Option<TranscriptWitness>,
    },
    Invalid {
        code: u32,
    },
    Unresolved {
        code: UnresolvedCode,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptTransition {
    pub state_id: StateId,
    pub candidate_id: CandidateId,
    pub outcome: TranscriptTransitionOutcome,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayInputIdentity {
    pub domain_digest: Digest,
    pub task_id: TaskId,
    pub policy_id: ModelCheckpointId,
    pub budget_limits: crate::BudgetLimits,
}

impl TranscriptDecision {
    pub fn frontier_key(&self) -> FrontierKey {
        FrontierKey {
            score_key: OrderedScore(self.score_bits),
            logical_cost: Reverse(self.logical_cost),
            depth: Reverse(self.depth),
            state_id: Reverse(self.state_id),
            candidate_id: Reverse(self.candidate_id),
            insertion_seq: Reverse(self.insertion_seq),
        }
    }
}

/// Immutable search transcript for audit/replay (§11.8).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SearchTranscript {
    pub cell_seed: u64,
    pub decisions: Vec<TranscriptDecision>,
    #[serde(default)]
    pub expansions: Vec<TranscriptExpansion>,
    #[serde(default)]
    pub transitions: Vec<TranscriptTransition>,
    #[serde(default)]
    pub inputs: Option<ReplayInputIdentity>,
    pub budget_exhaustion: Option<BudgetFired>,
    #[serde(default)]
    pub budget_counters: crate::BudgetCounters,
    pub solved: bool,
    #[serde(default)]
    pub receipts: Vec<reflex_types::Digest>,
}

impl SearchTranscript {
    pub fn new(cell_seed: u64) -> Self {
        Self {
            cell_seed,
            decisions: Vec::new(),
            expansions: Vec::new(),
            transitions: Vec::new(),
            inputs: None,
            budget_exhaustion: None,
            budget_counters: crate::BudgetCounters::default(),
            solved: false,
            receipts: Vec::new(),
        }
    }

    pub fn record_decision(&mut self, decision: TranscriptDecision) {
        self.decisions.push(decision);
    }

    pub fn record_expansion(&mut self, expansion: TranscriptExpansion) {
        self.expansions.push(expansion);
    }

    pub fn record_transition(&mut self, transition: TranscriptTransition) {
        self.transitions.push(transition);
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum DivergenceField {
    Input,
    State,
    CandidateBatch,
    Transition,
    FrontierKey,
    Score,
    CandidateOrder,
    Budget,
    Outcome,
    Receipt,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Divergence {
    pub step: usize,
    pub field: DivergenceField,
    pub expected: String,
    pub observed: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DivergenceReport {
    pub divergences: Vec<Divergence>,
}

impl DivergenceReport {
    pub fn first(&self) -> Option<&Divergence> {
        self.divergences.first()
    }

    pub fn divergence_count(&self) -> usize {
        self.divergences.len()
    }

    pub fn passed(&self) -> bool {
        self.divergences.is_empty()
    }
}

pub struct ReplayOutcome {
    pub report: DivergenceReport,
    pub stats: SearchStats,
    pub transcript: SearchTranscript,
    /// Canonical digest of the independently reconstructed full-verifier
    /// result. Present only in [`ReplayMode::FullVerifier`] for a solved run.
    pub full_verification: Option<Digest>,
}

pub struct ReplayEngine;

struct RecordedScorePolicy {
    model_id: ModelCheckpointId,
    expansions: Vec<TranscriptExpansion>,
    cursor: Mutex<usize>,
}

impl SearchPolicy for RecordedScorePolicy {
    fn model_id(&self) -> ModelCheckpointId {
        self.model_id
    }

    fn score_batch(
        &self,
        _features: &crate::FeatureBatch,
        candidate_ids: &[CandidateId],
        output: &mut [f32],
        _telemetry: &mut crate::InferenceTelemetry,
    ) -> Result<(), crate::PolicyError> {
        let mut cursor = self.cursor.lock().map_err(|_| {
            crate::PolicyError::InferenceFailed("logical replay score cursor poisoned".to_string())
        })?;
        let Some(expected) = self.expansions.get(*cursor) else {
            output.fill(0.0);
            return Ok(());
        };
        *cursor += 1;
        if expected.candidate_ids == candidate_ids
            && expected.score_bits.len() == candidate_ids.len()
        {
            for (value, &bits) in output.iter_mut().zip(&expected.score_bits) {
                *value = OrderedScore(bits).to_f32();
            }
        } else {
            // Keep the replay bounded and let the transcript comparator name
            // the exact candidate-batch divergence.
            output.fill(0.0);
        }
        Ok(())
    }
}

impl ReplayEngine {
    /// Replay a recorded transcript against a fresh search run.
    pub fn replay<D: Domain>(
        original: &SearchTranscript,
        domain: &D,
        ranker: &dyn SearchPolicy,
        budget: BudgetSet,
        task: &D::Task,
        mode: ReplayMode,
    ) -> Result<ReplayOutcome, SearchError> {
        let Some(inputs) = &original.inputs else {
            let report = DivergenceReport {
                divergences: vec![Divergence {
                    step: 0,
                    field: DivergenceField::Input,
                    expected: "immutable replay input identity".to_string(),
                    observed: "missing".to_string(),
                }],
            };
            return Ok(ReplayOutcome {
                report,
                stats: SearchStats::default(),
                transcript: original.clone(),
                full_verification: None,
            });
        };

        let observed_domain = domain.capabilities().domain_digest;
        let observed_task = domain.task_id(task)?;
        let observed_policy = ranker.model_id();
        let policy_mismatch = mode != ReplayMode::Logical && inputs.policy_id != observed_policy;
        if inputs.domain_digest != observed_domain
            || inputs.task_id != observed_task
            || inputs.budget_limits != budget.limits
            || policy_mismatch
        {
            let observed = ReplayInputIdentity {
                domain_digest: observed_domain,
                task_id: observed_task,
                policy_id: observed_policy,
                budget_limits: budget.limits.clone(),
            };
            return Ok(ReplayOutcome {
                report: DivergenceReport {
                    divergences: vec![Divergence {
                        step: 0,
                        field: DivergenceField::Input,
                        expected: format!("{inputs:?}"),
                        observed: format!("{observed:?}"),
                    }],
                },
                stats: SearchStats::default(),
                transcript: original.clone(),
                full_verification: None,
            });
        }

        if original.solved
            && !original
                .receipts
                .iter()
                .any(|receipt| *receipt != reflex_types::Digest::ZERO)
        {
            return Ok(ReplayOutcome {
                report: DivergenceReport {
                    divergences: vec![Divergence {
                        step: original.decisions.len(),
                        field: DivergenceField::Receipt,
                        expected: "at least one accepted non-zero receipt".to_string(),
                        observed: "no accepted receipt".to_string(),
                    }],
                },
                stats: SearchStats::default(),
                transcript: original.clone(),
                full_verification: None,
            });
        }

        let recorded_ranker;
        let replay_ranker: &dyn SearchPolicy = if mode == ReplayMode::Logical {
            recorded_ranker = RecordedScorePolicy {
                model_id: inputs.policy_id,
                expansions: original.expansions.clone(),
                cursor: Mutex::new(0),
            };
            &recorded_ranker
        } else {
            ranker.validate_available()?;
            ranker
        };

        let mut kernel = SearchKernel::with_seed(domain, replay_ranker, budget, original.cell_seed);
        let result = kernel.run(task)?;
        let replay_transcript = kernel.transcript().clone();
        let mut stats = kernel.stats().clone();
        let full_verification = if mode == ReplayMode::FullVerifier {
            if let Some(solved) = result.clone() {
                let artifact = domain.reconstruct_artifact(solved, kernel.episode_arena())?;
                let verify_budget = VerifyBudget {
                    max_cpu_ns: seconds_to_ns(inputs.budget_limits.process_cpu_seconds),
                    max_wall_ns: seconds_to_ns(inputs.budget_limits.wall_seconds),
                    max_memory_bytes: inputs.budget_limits.rss_bytes,
                };
                let started = Instant::now();
                let verification = domain.verify(&artifact, verify_budget).map_err(|error| {
                    SearchError::InvariantViolation(format!("full verifier replay failed: {error}"))
                })?;
                let elapsed = started.elapsed().as_nanos() as u64;
                stats.verifier_calls = stats.verifier_calls.saturating_add(1);
                stats.verifier_cpu_ns = stats.verifier_cpu_ns.saturating_add(elapsed);
                stats.search_cpu_ns = stats.search_cpu_ns.saturating_add(elapsed);
                Some(
                    reflex_canonical::content_id(
                        b"reflex.full-replay.verification.v1",
                        &verification,
                    )
                    .map_err(|error| SearchError::InvariantViolation(error.to_string()))?,
                )
            } else {
                None
            }
        } else {
            None
        };
        let mut report = Self::compare_transcripts(original, &replay_transcript);
        if report.passed() && original.solved != result.is_some() {
            report.divergences.push(Divergence {
                step: replay_transcript.decisions.len(),
                field: DivergenceField::Outcome,
                expected: format!("solved={}", original.solved),
                observed: format!("solved={}", result.is_some()),
            });
        }
        if report.passed() && original.budget_exhaustion != kernel.budget().first_fired {
            report.divergences.push(Divergence {
                step: replay_transcript.decisions.len(),
                field: DivergenceField::Budget,
                expected: format!("{:?}", original.budget_exhaustion),
                observed: format!("{:?}", kernel.budget().first_fired),
            });
        }

        Ok(ReplayOutcome {
            report,
            stats,
            transcript: replay_transcript,
            full_verification,
        })
    }

    /// Compare two transcripts directly (for edited-score detection).
    pub fn compare_transcripts(
        expected: &SearchTranscript,
        observed: &SearchTranscript,
    ) -> DivergenceReport {
        let mut report = DivergenceReport::default();
        if expected.inputs != observed.inputs {
            report.divergences.push(Divergence {
                step: 0,
                field: DivergenceField::Input,
                expected: format!("{:?}", expected.inputs),
                observed: format!("{:?}", observed.inputs),
            });
            return report;
        }
        for (index, (exp, obs)) in expected
            .expansions
            .iter()
            .zip(&observed.expansions)
            .enumerate()
        {
            let difference = if exp.state_id != obs.state_id {
                Some(DivergenceField::State)
            } else if exp.candidate_ids != obs.candidate_ids {
                Some(DivergenceField::CandidateBatch)
            } else if exp.score_bits != obs.score_bits {
                Some(DivergenceField::Score)
            } else {
                None
            };
            if let Some(field) = difference {
                report.divergences.push(Divergence {
                    step: index,
                    field,
                    expected: format!("{exp:?}"),
                    observed: format!("{obs:?}"),
                });
                return report;
            }
        }
        if expected.expansions.len() != observed.expansions.len() {
            report.divergences.push(Divergence {
                step: expected.expansions.len().min(observed.expansions.len()),
                field: DivergenceField::CandidateBatch,
                expected: format!("{} expansions", expected.expansions.len()),
                observed: format!("{} expansions", observed.expansions.len()),
            });
            return report;
        }
        for (index, (expected, observed)) in expected
            .transitions
            .iter()
            .zip(&observed.transitions)
            .enumerate()
        {
            if expected != observed {
                report.divergences.push(Divergence {
                    step: index,
                    field: DivergenceField::Transition,
                    expected: format!("{expected:?}"),
                    observed: format!("{observed:?}"),
                });
                return report;
            }
        }
        if expected.transitions.len() != observed.transitions.len() {
            report.divergences.push(Divergence {
                step: expected.transitions.len().min(observed.transitions.len()),
                field: DivergenceField::Transition,
                expected: format!("{} transitions", expected.transitions.len()),
                observed: format!("{} transitions", observed.transitions.len()),
            });
            return report;
        }
        for (i, (exp, obs)) in expected
            .decisions
            .iter()
            .zip(observed.decisions.iter())
            .enumerate()
        {
            if exp != obs {
                report.divergences.push(Divergence {
                    step: i,
                    field: if exp.score_bits != obs.score_bits {
                        DivergenceField::Score
                    } else if exp.state_id != obs.state_id {
                        DivergenceField::State
                    } else if exp.candidate_id != obs.candidate_id {
                        DivergenceField::CandidateOrder
                    } else {
                        DivergenceField::FrontierKey
                    },
                    expected: format!("{:?}", exp),
                    observed: format!("{:?}", obs),
                });
                return report;
            }
        }
        if expected.decisions.len() != observed.decisions.len() {
            report.divergences.push(Divergence {
                step: expected.decisions.len().min(observed.decisions.len()),
                field: DivergenceField::CandidateOrder,
                expected: expected.decisions.len().to_string(),
                observed: observed.decisions.len().to_string(),
            });
        }
        if report.passed() && expected.budget_counters != observed.budget_counters {
            report.divergences.push(Divergence {
                step: expected.decisions.len(),
                field: DivergenceField::Budget,
                expected: format!("{:?}", expected.budget_counters),
                observed: format!("{:?}", observed.budget_counters),
            });
        }
        if report.passed() && expected.receipts != observed.receipts {
            report.divergences.push(Divergence {
                step: expected.decisions.len(),
                field: DivergenceField::Receipt,
                expected: format!("{:?}", expected.receipts),
                observed: format!("{:?}", observed.receipts),
            });
        }
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SearchBudget, UniformRanker};
    use reflex_domain::conformance::FixtureDomain;
    use reflex_types::ModelCheckpointId;

    #[test]
    fn test_replay_detects_edited_score() {
        let mut original = SearchTranscript::new(42);
        original.decisions.push(TranscriptDecision {
            step: 0,
            score_bits: 100,
            logical_cost: 1,
            depth: 0,
            state_id: StateId::from_digest(reflex_types::Digest::hash_blake3(b"s")),
            candidate_id: CandidateId::from_digest(reflex_types::Digest::hash_blake3(b"c")),
            insertion_seq: 0,
        });
        let mut edited = original.clone();
        edited.decisions[0].score_bits = 999;
        let report = ReplayEngine::compare_transcripts(&original, &edited);
        assert_eq!(report.divergence_count(), 1);
        assert_eq!(report.first().unwrap().field, DivergenceField::Score);
    }

    #[test]
    fn test_replay_detects_edited_receipt() {
        let original = SearchTranscript::new(42);
        let mut edited = original.clone();
        edited
            .receipts
            .push(reflex_types::Digest::hash_blake3(b"forged"));
        let report = ReplayEngine::compare_transcripts(&original, &edited);
        assert_eq!(report.first().unwrap().field, DivergenceField::Receipt);
    }

    #[test]
    fn test_logical_replay_matches() {
        let domain = FixtureDomain::new();
        let task = reflex_domain::conformance::FixtureTask {
            start: 0,
            target: 3,
            candidates_per_state: 2,
            allow_duplicate_ids: false,
        };
        let ranker = UniformRanker::with_seed(
            ModelCheckpointId::from_digest(reflex_types::Digest::hash_blake3(b"uniform")),
            123,
        );
        let budget: BudgetSet = SearchBudget::default_for_test().into();
        let mut kernel = SearchKernel::with_seed(&domain, &ranker, budget.clone(), 123);
        let _ = kernel.run(&task).unwrap();
        let transcript = kernel.transcript().clone();
        let outcome = ReplayEngine::replay(
            &transcript,
            &domain,
            &ranker,
            budget,
            &task,
            ReplayMode::Logical,
        )
        .unwrap();
        assert!(
            outcome.report.passed(),
            "divergences: {:?}",
            outcome.report.divergences
        );
    }

    #[test]
    fn test_logical_replay_does_not_require_policy_execution() {
        let domain = FixtureDomain::new();
        let task = reflex_domain::conformance::FixtureTask {
            start: 0,
            target: 3,
            candidates_per_state: 2,
            allow_duplicate_ids: false,
        };
        let model_id =
            ModelCheckpointId::from_digest(reflex_types::Digest::hash_blake3(b"missing"));
        let available = UniformRanker::new(model_id);
        let unavailable =
            crate::UnavailableLearnedRanker::new(model_id, "checkpoint intentionally unavailable");
        let budget = BudgetSet::default_for_test();
        let mut original = SearchKernel::with_seed(&domain, &available, budget.clone(), 7);
        let _ = original.run(&task).unwrap();
        let transcript = original.transcript().clone();
        let logical = ReplayEngine::replay(
            &transcript,
            &domain,
            &unavailable,
            budget.clone(),
            &task,
            ReplayMode::Logical,
        )
        .unwrap();
        assert!(logical.report.passed());

        let policy = ReplayEngine::replay(
            &transcript,
            &domain,
            &unavailable,
            budget,
            &task,
            ReplayMode::Policy,
        );
        assert!(policy.is_err());
    }

    #[test]
    fn test_logical_replay_audits_step_sequence() {
        let domain = FixtureDomain::new();
        let task = reflex_domain::conformance::FixtureTask {
            start: 0,
            target: 1,
            candidates_per_state: 1,
            allow_duplicate_ids: false,
        };
        let ranker = UniformRanker::new(ModelCheckpointId::from_digest(
            reflex_types::Digest::hash_blake3(b"uniform"),
        ));
        let mut transcript = SearchTranscript::new(1);
        transcript.decisions.push(TranscriptDecision {
            step: 9,
            score_bits: 1,
            logical_cost: 1,
            depth: 0,
            state_id: StateId::from_digest(reflex_types::Digest::hash_blake3(b"s")),
            candidate_id: CandidateId::from_digest(reflex_types::Digest::hash_blake3(b"c")),
            insertion_seq: 0,
        });
        let replay = ReplayEngine::replay(
            &transcript,
            &domain,
            &ranker,
            BudgetSet::default_for_test(),
            &task,
            ReplayMode::Logical,
        )
        .unwrap();
        assert_eq!(
            replay.report.first().map(|d| &d.field),
            Some(&DivergenceField::Input),
            "a hand-edited transcript without immutable inputs must fail before its step numbers are trusted"
        );
    }

    #[test]
    fn test_logical_replay_names_edited_candidate_batch() {
        let domain = FixtureDomain::new();
        let task = reflex_domain::conformance::FixtureTask {
            start: 0,
            target: 3,
            candidates_per_state: 2,
            allow_duplicate_ids: false,
        };
        let model_id = ModelCheckpointId::from_digest(reflex_types::Digest::hash_blake3(b"u"));
        let ranker = UniformRanker::new(model_id);
        let budget = BudgetSet::default_for_test();
        let mut kernel = SearchKernel::with_seed(&domain, &ranker, budget.clone(), 9);
        let _ = kernel.run(&task).unwrap();
        let mut edited = kernel.transcript().clone();
        edited.expansions[0].candidate_ids.swap(0, 1);

        let replay = ReplayEngine::replay(
            &edited,
            &domain,
            &ranker,
            budget,
            &task,
            ReplayMode::Logical,
        )
        .unwrap();
        assert_eq!(
            replay.report.first().map(|d| &d.field),
            Some(&DivergenceField::CandidateBatch)
        );
    }

    #[test]
    fn test_replay_names_edited_transition() {
        let domain = FixtureDomain::new();
        let task = reflex_domain::conformance::FixtureTask {
            start: 0,
            target: 3,
            candidates_per_state: 2,
            allow_duplicate_ids: false,
        };
        let ranker = UniformRanker::new(ModelCheckpointId::from_digest(
            reflex_types::Digest::hash_blake3(b"u"),
        ));
        let mut kernel =
            SearchKernel::with_seed(&domain, &ranker, BudgetSet::default_for_test(), 9);
        let _ = kernel.run(&task).unwrap();
        let original = kernel.transcript().clone();
        let mut edited = original.clone();
        edited.transitions[0].outcome = TranscriptTransitionOutcome::Unresolved {
            code: UnresolvedCode::VerifierUnavailable,
        };
        assert_eq!(
            ReplayEngine::compare_transcripts(&original, &edited)
                .first()
                .map(|divergence| &divergence.field),
            Some(&DivergenceField::Transition)
        );
    }
}
