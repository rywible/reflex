use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::time::{Duration, Instant};

use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, improve,
};
use reflex_lean::ast::LeanName;
use reflex_lean::domain::{LeanCorpus, LeanDomain, LeanMetric, LeanSeedScope};
use reflex_lean::worker::{LeanWorker, LeanWorkerConfig};

#[test]
#[ignore = "requires the pinned Lean and mathlib installations"]
fn runtime_finds_a_real_collapse_and_replays_it_in_a_clean_process() {
    let lake = std::env::var_os("REFLEX_LEAN_LAKE").expect("REFLEX_LEAN_LAKE is required");
    let mathlib = std::env::var_os("REFLEX_LEAN_MATHLIB").expect("REFLEX_LEAN_MATHLIB is required");
    let config = LeanWorkerConfig::pinned(lake, mathlib);
    let builder = LeanWorker::start(&config).unwrap();
    let names = [
        LeanName::from_dotted("ContinuousMap.compactOpen_eq_iInf_induced"),
        LeanName::from_dotted("ContinuousMap.compactOpen_eq_sInf_induced"),
    ];
    let corpus = LeanCorpus::verified_theorems(&builder, builder.fetch(&names).unwrap()).unwrap();
    drop(builder);

    let fresh_bundle = std::env::temp_dir().join(format!(
        "reflex-lean-runtime-smoke-{}.bundle",
        std::process::id()
    ));
    let restored_bundle = fresh_bundle.with_extension("restored.bundle");
    let request = |plan| {
        let objectives = NonEmpty::one(Objective::new(LeanMetric::ProofNodes, Direction::Minimize));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(LeanMetric::ProofNodes)), []).unwrap();
        ImprovementRequest::new(
            GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
            LeanSeedScope { start: 0, count: 1 },
            ResourceEnvelope::new(
                NonZeroUsize::new(2).unwrap(),
                NonZeroU64::new(20 * 1024 * 1024 * 1024).unwrap(),
                NonZeroU64::new(256 * 1024 * 1024).unwrap(),
                NonZeroDuration::new(Duration::from_mins(1)).unwrap(),
                NonZeroDuration::new(Duration::from_mins(1)).unwrap(),
                NonZeroU64::new(100).unwrap(),
            ),
            plan,
        )
        .unwrap()
    };
    let outcome = improve(
        LeanDomain::new(config.clone(), corpus.clone()).unwrap(),
        request(BundlePlan::Fresh {
            target: fresh_bundle.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    assert_eq!(outcome.pareto().artifacts().len(), 1);
    let improved_nodes = outcome.pareto().artifacts()[0]
        .artifact()
        .proof_term
        .node_count();
    eprintln!(
        "completion={:?} usage={:?} seed_nodes={} pareto_nodes={improved_nodes}",
        outcome.completion(),
        outcome.usage(),
        corpus.entries()[0].artifact.proof_term.node_count()
    );
    assert!(improved_nodes < corpus.entries()[0].artifact.proof_term.node_count());
    assert!(outcome.usage().verification_requests > 0);

    let restore_started = Instant::now();
    let restored = improve(
        LeanDomain::new(config, corpus).unwrap(),
        request(BundlePlan::Resume {
            source: fresh_bundle.clone(),
            target: restored_bundle.clone(),
        }),
        |_| ControlFlow::Break(()),
    )
    .unwrap();
    assert!(restore_started.elapsed() < Duration::from_mins(1));
    assert_eq!(
        restored.pareto().artifacts()[0]
            .artifact()
            .proof_term
            .node_count(),
        improved_nodes
    );
    std::fs::remove_file(fresh_bundle).unwrap();
    std::fs::remove_file(restored_bundle).unwrap();
}
