mod batches;
mod budget;
mod cache;
mod kernel;
mod policy;
mod proof_dag;
mod replay;

pub use batches::{SearchBatchPool, SearchBatchPoolError};
pub use budget::{
    BudgetCounters, BudgetFired, BudgetKind, BudgetLimits, BudgetSet, ProcessSample,
    ProcessSampler, SearchBudget, read_self_rss_bytes,
};
pub use cache::{
    CacheConfig, CachedReflexResult, ReflexCache, SearchCaches, TranspositionHit,
    TranspositionTable, VisitRecord,
};
pub use kernel::{
    AndGroup, CandidateBatchIndex, Edge, EdgeIndex, FrontierEntry, FrontierKey, NodeIndex,
    NodeStatus, SearchArena, SearchKernel, SearchNode,
};
pub use policy::{
    HeuristicRanker, MixtureEvent, MixturePolicy, MixtureRanker, SearchPolicy,
    UnavailableLearnedRanker, UniformPolicy, UniformRanker, deterministic_uniform_score,
};
pub use proof_dag::{
    CandidateKnowledge, ProofCycleEdge, ProofDag, ProofDagError, ProofEdgeRecord, ProofScc,
    ViableCost, viable_target,
};
pub use replay::{
    Divergence, DivergenceField, DivergenceReport, ReplayEngine, ReplayInputIdentity, ReplayMode,
    ReplayOutcome, SearchTranscript, TranscriptDecision, TranscriptExpansion, TranscriptTransition,
    TranscriptTransitionOutcome, TranscriptWitness,
};

// Re-export batch types for policy consumers (P6.2 manifest anchors).
pub use reflex_domain::{CandidateBatch, CandidateBatchBuilder, DomainError, FeatureBatch};
use reflex_types::ModelCheckpointId;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ── Errors ─────────────────────────────────────────────────────────────

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    #[error("domain error: {0}")]
    Domain(#[from] DomainError),
    #[error("budget exhausted: {0}")]
    BudgetExhausted(String),
    #[error("non-finite policy score: {0}")]
    NonFiniteScore(u32),
    #[error("policy error: {0}")]
    Policy(#[from] PolicyError),
    #[error("search invariant violation: {0}")]
    InvariantViolation(String),
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    #[error("non-finite score: {0}")]
    NonFiniteScore(u32),
    #[error("model inference failed: {0}")]
    InferenceFailed(String),
}

// ── Ordered score (§11.3) ─────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct OrderedScore(u32);

impl OrderedScore {
    pub fn bits(&self) -> u32 {
        self.0
    }

    pub fn from_f32(score: f32) -> Result<Self, SearchError> {
        if !score.is_finite() {
            return Err(SearchError::NonFiniteScore(score.to_bits()));
        }
        let normalized = if score == 0.0 { 0.0f32 } else { score };
        let bits = normalized.to_bits();
        let order_key = if (bits & 0x8000_0000) != 0 {
            !bits
        } else {
            bits ^ 0x8000_0000
        };
        Ok(OrderedScore(order_key))
    }

    pub(crate) fn to_f32(self) -> f32 {
        let bits = if (self.0 & 0x8000_0000) != 0 {
            self.0 ^ 0x8000_0000
        } else {
            !self.0
        };
        f32::from_bits(bits)
    }
}

// ── Search stats ───────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
pub struct SearchStats {
    pub nodes_expanded: u32,
    pub candidates_scored: u32,
    pub ml_overhead_ns: u64,
    pub search_cpu_ns: u64,
    pub verifier_calls: u32,
    pub verifier_cpu_ns: u64,
    pub budget_exhaustion: Option<BudgetFired>,
}

// ── Inference telemetry ────────────────────────────────────────────────

pub struct InferenceTelemetry {
    pub cpu_ns: u64,
    pub model_id: ModelCheckpointId,
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_types::{CandidateId, Digest, StateId};

    #[test]
    fn test_ordered_score_monotonicity() {
        let s1 = OrderedScore::from_f32(0.1).unwrap();
        let s2 = OrderedScore::from_f32(0.5).unwrap();
        let s3 = OrderedScore::from_f32(10.0).unwrap();
        assert!(s1 < s2);
        assert!(s2 < s3);

        let zero_pos = OrderedScore::from_f32(0.0).unwrap();
        let zero_neg = OrderedScore::from_f32(-0.0).unwrap();
        assert_eq!(zero_pos, zero_neg);
    }

    #[test]
    fn test_frontier_ordering() {
        let k1 = FrontierKey {
            score_key: OrderedScore::from_f32(0.1).unwrap(),
            logical_cost: std::cmp::Reverse(1),
            depth: std::cmp::Reverse(0),
            state_id: std::cmp::Reverse(StateId::from_digest(Digest::hash_blake3(b"a"))),
            candidate_id: std::cmp::Reverse(CandidateId::from_digest(Digest::hash_blake3(b"c1"))),
            insertion_seq: std::cmp::Reverse(0),
        };
        let k2 = FrontierKey {
            score_key: OrderedScore::from_f32(0.5).unwrap(),
            logical_cost: std::cmp::Reverse(1),
            depth: std::cmp::Reverse(0),
            state_id: std::cmp::Reverse(StateId::from_digest(Digest::hash_blake3(b"a"))),
            candidate_id: std::cmp::Reverse(CandidateId::from_digest(Digest::hash_blake3(b"c2"))),
            insertion_seq: std::cmp::Reverse(1),
        };
        assert!(k1 < k2);
    }

    #[test]
    fn test_contradiction_marks_known_dead_without_failing_state() {
        let mut dag = ProofDag::new();
        let state_id = StateId::from_digest(Digest::hash_blake3(b"state"));
        let candidate_id = CandidateId::from_digest(Digest::hash_blake3(b"cand"));
        let cert = Digest::hash_blake3(b"contradiction-cert");
        dag.mark_known_dead(state_id, candidate_id, cert);
        assert!(matches!(
            dag.get_knowledge(state_id, candidate_id),
            Some(CandidateKnowledge::KnownDead { certificate }) if *certificate == cert
        ));
    }

    #[test]
    fn test_candidate_knowledge_variants() {
        let viable = CandidateKnowledge::Viable {
            best_actions_to_go: 3,
            receipts: smallvec::SmallVec::new(),
        };
        let dead = CandidateKnowledge::KnownDead {
            certificate: Digest::hash_blake3(b"cert"),
        };
        let unknown = CandidateKnowledge::Unknown;
        let invalid = CandidateKnowledge::Invalid { code: 42 };

        assert!(matches!(viable, CandidateKnowledge::Viable { .. }));
        assert!(matches!(dead, CandidateKnowledge::KnownDead { .. }));
        assert!(matches!(unknown, CandidateKnowledge::Unknown));
        assert!(matches!(invalid, CandidateKnowledge::Invalid { .. }));
    }

    #[test]
    fn test_frontier_determinism_with_seed() {
        use crate::policy::UniformRanker;
        use reflex_domain::conformance::{FixtureDomain, FixtureTask};
        use reflex_types::ModelCheckpointId;

        let domain = FixtureDomain::new();
        let task = FixtureTask {
            start: 0,
            target: 3,
            candidates_per_state: 2,
            allow_duplicate_ids: false,
        };
        let ranker = UniformRanker::with_seed(
            ModelCheckpointId::from_digest(Digest::hash_blake3(b"uniform")),
            99,
        );
        let budget: BudgetSet = SearchBudget::default_for_test().into();

        let mut k1 = SearchKernel::with_seed(&domain, &ranker, budget.clone(), 99);
        let mut k2 = SearchKernel::with_seed(&domain, &ranker, budget, 99);
        k1.run(&task).unwrap();
        k2.run(&task).unwrap();
        assert_eq!(k1.transcript().decisions, k2.transcript().decisions);
    }

    // Manifest grep anchors (tests also live in submodules).
    #[test]
    fn test_candidate_batch_default() {
        let batch = CandidateBatch::default();
        assert!(batch.group_offsets.is_empty());
        assert!(batch.ids.is_empty());
        assert_eq!(batch.group_count(), 0);

        let id_a = CandidateId::from_digest(Digest::hash_blake3(b"a"));
        let id_b = CandidateId::from_digest(Digest::hash_blake3(b"b"));
        let id_c = CandidateId::from_digest(Digest::hash_blake3(b"c"));
        let grouped = CandidateBatch {
            group_offsets: vec![0, 2, 3],
            ids: vec![id_a, id_b, id_c],
            classes: vec![1, 2, 3],
            tie_breaks: vec![10, 20, 30],
            payload_handles: vec![
                reflex_domain::CandidateHandle(0),
                reflex_domain::CandidateHandle(1),
                reflex_domain::CandidateHandle(2),
            ],
            flags: vec![0, 0, 0],
        };
        assert_eq!(grouped.group_count(), 2);
        assert_eq!(grouped.group_ids(0).unwrap(), &[id_a, id_b]);
        assert_eq!(grouped.group_ids(1).unwrap(), &[id_c]);
        assert_eq!(grouped.group_classes(0).unwrap(), &[1, 2]);
    }

    #[test]
    fn test_frontier_golden_pop_order_fixture() {
        use crate::policy::UniformRanker;
        use reflex_domain::conformance::{FixtureDomain, FixtureTask};
        use reflex_types::ModelCheckpointId;

        let domain = FixtureDomain::new();
        let task = FixtureTask {
            start: 0,
            target: 3,
            candidates_per_state: 2,
            allow_duplicate_ids: false,
        };
        let ranker = UniformRanker::with_seed(
            ModelCheckpointId::from_digest(Digest::hash_blake3(b"uniform")),
            42,
        );
        let budget: BudgetSet = SearchBudget::default_for_test().into();
        let mut kernel = SearchKernel::with_seed(&domain, &ranker, budget.clone(), 42);
        kernel.run(&task).unwrap();

        let decisions = &kernel.transcript().decisions;
        assert!(!decisions.is_empty(), "fixture search must emit decisions");

        let mut replay = SearchKernel::with_seed(&domain, &ranker, budget.clone(), 42);
        replay.run(&task).unwrap();
        assert_eq!(decisions, &replay.transcript().decisions);

        let expected_first = decisions[0].candidate_id;
        let mut check = SearchKernel::with_seed(&domain, &ranker, budget, 42);
        check.run(&task).unwrap();
        assert_eq!(check.transcript().decisions[0].candidate_id, expected_first);
    }

    #[test]
    fn test_feature_batch_row_access() {
        use reflex_domain::FeatureBatch;
        use reflex_types::{Digest, FeatureSchemaId};
        let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(b"schema"));
        let mut fb = FeatureBatch::new(2, 3, schema);
        fb.values[0] = 1.0;
        fb.values[1] = 2.0;
        fb.values[2] = 3.0;
        fb.values[3] = 4.0;
        fb.values[4] = 5.0;
        fb.values[5] = 6.0;
        assert_eq!(fb.row(0), &[1.0, 2.0, 3.0]);
        assert_eq!(fb.row(1), &[4.0, 5.0, 6.0]);
    }

    #[test]
    fn test_node_status_is_solved() {
        assert!(NodeStatus::Closed.is_solved());
        assert!(NodeStatus::ObligationComplete.is_solved());
        assert!(!NodeStatus::Open.is_solved());
        assert!(!NodeStatus::Failed.is_solved());
        assert!(!NodeStatus::Censored.is_solved());
    }

    #[test]
    fn test_and_group_creation() {
        let mut arena = SearchArena::new();
        let parent = arena.add_node(
            reflex_domain::StateHandle(0, 0),
            StateId::from_digest(Digest::hash_blake3(b"parent")),
            NodeStatus::Open,
            0,
            0,
            None,
        );
        let edge = arena.add_edge(
            parent,
            CandidateId::from_digest(Digest::hash_blake3(b"cand")),
            reflex_domain::CandidateHandle(0),
        );
        let ch1 = arena.add_node(
            reflex_domain::StateHandle(1, 0),
            StateId::from_digest(Digest::hash_blake3(b"ch1")),
            NodeStatus::ObligationPending,
            1,
            1,
            Some(edge),
        );
        let ch2 = arena.add_node(
            reflex_domain::StateHandle(2, 0),
            StateId::from_digest(Digest::hash_blake3(b"ch2")),
            NodeStatus::ObligationPending,
            1,
            1,
            Some(edge),
        );
        let group_idx = arena.add_and_group(edge, parent, &[ch1, ch2]);
        assert_eq!(arena.and_groups[group_idx].remaining, 2);
    }

    #[test]
    fn test_search_arena_basics() {
        let mut arena = SearchArena::new();
        let idx = arena.add_node(
            reflex_domain::StateHandle(0, 0),
            StateId::from_digest(Digest::hash_blake3(b"test")),
            NodeStatus::Open,
            0,
            0,
            None,
        );
        assert_eq!(arena.node_count(), 1);
        assert_eq!(arena.get_node(idx).unwrap().status, NodeStatus::Open);
    }

    #[test]
    fn test_budget_default() {
        let b = SearchBudget::default_for_test();
        assert_eq!(b.action_budget, 10_000);
        let set: BudgetSet = b.into();
        assert_eq!(set.limits.verified_actions, 10_000);
    }

    #[test]
    fn test_proof_dag_mark_viable() {
        let mut dag = ProofDag::new();
        let sid = StateId::from_digest(Digest::hash_blake3(b"s1"));
        let cid = CandidateId::from_digest(Digest::hash_blake3(b"c1"));
        let receipt = Digest::hash_blake3(b"receipt");
        dag.mark_viable_with_receipt(sid, cid, 5, Some(receipt));
        match dag.get_knowledge(sid, cid).unwrap() {
            CandidateKnowledge::Viable {
                best_actions_to_go,
                receipts,
            } => {
                assert_eq!(*best_actions_to_go, 5);
                assert_eq!(receipts.as_slice(), &[receipt]);
            }
            _ => panic!("Expected Viable"),
        }
    }
}
