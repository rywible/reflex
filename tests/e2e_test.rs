use reflex_dataset::{CandidateKnowledge, DecisionGroup};
use reflex_domain::{Domain, VerifyBudget};
use reflex_domain_bitvec::{BitvecArtifact, BitvecDomain, BitvecTask, BvExpr};
use reflex_eval::evaluate_model_offline;
use reflex_ml_micro::{MicroMlp, MicroTrainer};
use reflex_report::{ScientificReport, ScientificReportParams};
use reflex_scheduler::{GenerationCoordinator, GenerationState, PromotionPolicy, StopPolicy};
use reflex_search::{SearchBudget, SearchKernel, UniformRanker};
use reflex_types::{Digest, ExperimentId, ModelCheckpointId, StateId};
use std::collections::HashMap;

#[test]
fn test_end_to_end_autonomous_two_generation_loop() {
    // 1. Setup Bit-Vector domain
    let domain = BitvecDomain::new();

    // 2. Generate task
    let task = BitvecTask {
        initial: BvExpr::Xor(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0))),
        target_max_cost: 1,
    };

    // 3. Generation 1: Solve with Uniform Policy
    let uniform = UniformRanker::new(ModelCheckpointId::from_digest(Digest::hash_blake3(b"uniform-model")));
    let mut search = SearchKernel::new(&domain, &uniform, SearchBudget::default_for_test());
    let solved = search.run(&task).unwrap();
    assert!(solved.is_some(), "Generation 1 must solve task");

    // 4. Exhaustive verification of artifact
    let artifact = BitvecArtifact {
        original: task.initial.clone(),
        optimized: BvExpr::Const(0),
    };
    let receipt = domain
        .verify(
            &artifact,
            VerifyBudget {
                max_cpu_ns: 1_000_000,
                max_wall_ns: 1_000_000,
                max_memory_bytes: 1024,
            },
        )
        .unwrap();
    assert!(receipt.is_equivalent);

    // 5. Compile decision group
    let state_id = StateId::from_digest(Digest::hash_blake3(b"s1"));
    let cand1 = reflex_types::CandidateId::from_digest(Digest::hash_blake3(b"c1"));
    let cand2 = reflex_types::CandidateId::from_digest(Digest::hash_blake3(b"c2"));

    let decision_group = DecisionGroup {
        state_id,
        candidate_ids: vec![cand1, cand2],
        labels: vec![
            CandidateKnowledge::Viable {
                best_actions_to_go: 1,
                receipts: smallvec::smallvec![Digest::ZERO],
            },
            CandidateKnowledge::Unknown,
        ],
        feature_ref: Digest::ZERO,
        source_episodes: Vec::new(),
        coverage: "complete".to_string(),
    };

    // 6. Train candidate micro-ranker
    let micro_mlp = MicroMlp::random(8, 16, 42);
    let mut trainer = MicroTrainer::new(micro_mlp, 0.05, 0.001);
    let features = vec![
        1.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.1, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.2,
    ];
    for _ in 0..50 {
        trainer.train_step_pairwise(&features, 0, 1);
    }
    let trained_model = trainer.model;

    // 7. Offline evaluation & promotion
    let mut feats_map = HashMap::new();
    feats_map.insert(state_id, features);

    let eval_report = evaluate_model_offline(&trained_model, &[decision_group], &feats_map, 8);
    assert_eq!(eval_report.top1_viable_rate, 1.0);

    let mut coord = GenerationCoordinator::new(
        StopPolicy::default(),
        PromotionPolicy::default(),
        None,
        None,
    );
    coord.transition_to(GenerationState::Collecting).unwrap();
    coord.transition_to(GenerationState::Verifying).unwrap();
    coord
        .transition_to(GenerationState::CompilingDataset)
        .unwrap();
    coord.transition_to(GenerationState::Training).unwrap();
    coord.transition_to(GenerationState::Evaluating).unwrap();
    let cand_id = ModelCheckpointId::from_digest(Digest::hash_blake3(b"gen1-promoted-model"));
    let state_after_eval = coord.handle_evaluation_result(&eval_report, cand_id);
    assert_eq!(state_after_eval, GenerationState::Promoted);
    assert_eq!(coord.active_model, Some(cand_id));

    // 8. Reconstruct strict report
    let report = ScientificReport::build(ScientificReportParams {
        experiment_id: ExperimentId::from_digest(Digest::hash_blake3(b"exp-e2e")),
        title: "Bitvec Autonomous Self-Improvement",
        domain: "bitvec-v1",
        total_cells: 10,
        solved_cells: 10,
        total_cpu_seconds: 2.5,
        total_ml_overhead_seconds: 0.1,
        evaluation: eval_report,
        economics: reflex_economics::EconomicLedgerSummary {
            net_utility_value: 1200.0,
            ..Default::default()
        },
    });

    assert_eq!(report.solve_rate, 1.0);
    assert!(report.ml_overhead_share < 0.10);
}
