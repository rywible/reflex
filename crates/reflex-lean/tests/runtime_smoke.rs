use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::time::Duration;

use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, improve,
};
use reflex_lean::domain::{LeanCorpus, LeanDomain, LeanMetric, LeanSeedScope};
use reflex_lean::worker::{LeanWorker, LeanWorkerConfig};

#[test]
#[ignore = "requires the pinned Lean and mathlib installations"]
fn runtime_accounts_for_lazy_worker_start_and_replays_a_seed() {
    let lake = std::env::var_os("REFLEX_LEAN_LAKE").expect("REFLEX_LEAN_LAKE is required");
    let mathlib =
        std::env::var_os("REFLEX_LEAN_MATHLIB").expect("REFLEX_LEAN_MATHLIB is required");
    let config = LeanWorkerConfig::pinned(lake, mathlib);
    let builder = LeanWorker::start(&config).unwrap();
    let corpus = LeanCorpus::verified_page(&builder, builder.index_page(0, 1).unwrap()).unwrap();
    drop(builder);

    let bundle = std::env::temp_dir().join(format!(
        "reflex-lean-runtime-smoke-{}.bundle",
        std::process::id()
    ));
    let objectives = NonEmpty::one(Objective::new(LeanMetric::ProofNodes, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(LeanMetric::ProofNodes)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        LeanSeedScope { start: 0, count: 1 },
        ResourceEnvelope::new(
            NonZeroUsize::new(2).unwrap(),
            NonZeroU64::new(6 * 1024 * 1024 * 1024).unwrap(),
            NonZeroU64::new(256 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_mins(1)).unwrap(),
            NonZeroDuration::new(Duration::from_mins(1)).unwrap(),
            NonZeroU64::new(100).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle.clone(),
        },
    )
    .unwrap();
    let outcome = improve(LeanDomain::new(config, corpus).unwrap(), request, |_| {
        ControlFlow::Break(())
    })
    .unwrap();
    assert_eq!(outcome.pareto().artifacts().len(), 1);
    assert!(outcome.usage().verification_requests > 0);
    std::fs::remove_file(bundle).ok();
}
