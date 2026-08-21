use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::time::Duration;

use reflex::{
    BundlePlan, Completion, Direction, GoalSet, ImprovementRequest, MeasurementConstraint,
    NonEmpty, NonZeroDuration, Objective, OptimizationGoal, Preference, ResourceEnvelope,
    SuccessCondition, ThresholdRelation, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};

#[test]
fn verification_budget_exhaustion_is_a_successful_completion() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-budget-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(0),
        )),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(1).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();

    let outcome = improve(BitVecDomain::unary_u8(), request, |_| {
        ControlFlow::Continue(())
    })
    .expect("exhausting a granted budget is not an error");

    assert!(
        outcome.completion() == Completion::ResourceEnvelopeExhausted
            && outcome.usage().verification_requests == 1
            && outcome.pareto().artifacts().len() == 1
            && outcome.pareto().artifacts()[0].artifact().node_count() == 3
            && bundle_path.is_file(),
        "Seed replay consumes the budget, preserves the verified Seed, and publishes it"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn elapsed_deadline_rejects_a_mandatory_state_that_cannot_start() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-elapsed-budget-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(0),
        )),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_nanos(1)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(10_000).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();

    let result = improve(BitVecDomain::unary_u8(), request, |_| {
        ControlFlow::Continue(())
    });

    assert!(
        matches!(result, Err(reflex::SessionError::Resource)) && !bundle_path.exists(),
        "an expired deadline cannot schedule mandatory Seed replay"
    );
}

#[test]
fn observer_time_counts_against_the_elapsed_deadline() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-observer-time-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(0),
        )),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_millis(1)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(10_000).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();

    let outcome = improve(BitVecDomain::unary_u8(), request, |_| {
        std::thread::sleep(Duration::from_millis(5));
        ControlFlow::Continue(())
    })
    .unwrap();

    assert!(
        outcome.completion() == Completion::ResourceEnvelopeExhausted
            && outcome.usage().verification_requests == 1
            && outcome.usage().worker_threads == 1
            && outcome.usage().resident_bytes > 0
            && outcome.usage().durable_bytes > 0
            && outcome.usage().elapsed_time >= Duration::from_millis(5),
        "serialized observer execution belongs to the Session's elapsed usage"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn process_cpu_deadline_rejects_a_mandatory_state_that_cannot_start() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-cpu-time-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(0),
        )),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_nanos(1)).unwrap(),
            NonZeroU64::new(10_000).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();

    let result = improve(BitVecDomain::unary_u8(), request, |_| {
        ControlFlow::Continue(())
    });

    assert!(
        matches!(result, Err(reflex::SessionError::Resource)) && !bundle_path.exists(),
        "an expired process CPU budget cannot schedule mandatory Seed replay"
    );
}

#[test]
fn session_runs_inside_its_bounded_worker_set() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-worker-set-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(Expression::input()),
        ResourceEnvelope::new(
            NonZeroUsize::new(2).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(10_000).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();
    let mut observer_thread = None;

    let outcome = improve(BitVecDomain::unary_u8(), request, |_| {
        observer_thread = std::thread::current().name().map(str::to_owned);
        ControlFlow::Continue(())
    })
    .unwrap();

    assert!(
        outcome.usage().worker_threads == 2
            && observer_thread.is_some_and(|name| name.starts_with("reflex-worker-")),
        "all Session phases execute inside the caller-bounded fixed worker set"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn durable_budget_rejects_an_uncheckpointable_frontier_atomically() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-durable-budget-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let goal = |direction| {
        let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, direction));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
        OptimizationGoal::new([], objectives, preference, None).unwrap()
    };
    let baseline_request = ImprovementRequest::new(
        GoalSet::one(goal(Direction::Minimize)),
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(0),
        )),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(1).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();
    improve(BitVecDomain::unary_u8(), baseline_request, |_| {
        ControlFlow::Continue(())
    })
    .unwrap();
    let baseline = std::fs::read(&bundle_path).unwrap();
    let durable_limit = NonZeroU64::new(baseline.len() as u64).unwrap();
    let goals =
        GoalSet::try_from_iter([goal(Direction::Minimize), goal(Direction::Maximize)]).unwrap();
    let request = ImprovementRequest::new(
        goals,
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(0),
        )),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            durable_limit,
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(10_000).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();
    let mut updates = 0;

    let outcome = improve(BitVecDomain::unary_u8(), request, |_| {
        updates += 1;
        ControlFlow::Continue(())
    })
    .unwrap();

    assert!(
        outcome.completion() == Completion::ResourceEnvelopeExhausted
            && outcome.pareto().artifacts().len() == 1
            && outcome.pareto().artifacts()[0].artifact().node_count() == 3
            && outcome.usage().durable_bytes == baseline.len() as u64
            && updates == 1
            && std::fs::read(&bundle_path).unwrap() == baseline,
        "an unaffordable Pareto transition must not be observed or replace the last bundle"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn resident_limit_rejects_a_mandatory_state_that_cannot_fit() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-resident-minimum-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(Expression::input()),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(10_000).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();

    let result = improve(BitVecDomain::unary_u8(), request, |_| {
        ControlFlow::Continue(())
    });

    assert!(
        matches!(result, Err(reflex::SessionError::Resource)) && !bundle_path.exists(),
        "a mandatory verified state that cannot fit is rejected before publication"
    );
}

#[test]
fn resident_limit_rejects_growth_before_frontier_admission() {
    let baseline_path = std::env::temp_dir().join(format!(
        "reflex-resident-baseline-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let target_path = baseline_path.with_extension("limited.bundle");
    let goal = |direction| {
        let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, direction));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
        OptimizationGoal::new([], objectives, preference, None).unwrap()
    };
    let request = |resident_bytes, verification_requests, target| {
        ImprovementRequest::new(
            GoalSet::try_from_iter([goal(Direction::Minimize), goal(Direction::Maximize)]).unwrap(),
            SeedScope::one(Expression::xor(
                Expression::input(),
                Expression::constant(0),
            )),
            ResourceEnvelope::new(
                NonZeroUsize::new(1).unwrap(),
                NonZeroU64::new(resident_bytes).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroU64::new(verification_requests).unwrap(),
            ),
            BundlePlan::Fresh { target },
        )
        .unwrap()
    };
    let baseline = improve(
        BitVecDomain::unary_u8(),
        request(16 * 1024 * 1024, 1, baseline_path.clone()),
        |_| ControlFlow::Break(()),
    )
    .unwrap();

    let outcome = improve(
        BitVecDomain::unary_u8(),
        request(baseline.usage().resident_bytes, 10_000, target_path.clone()),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    assert!(
        outcome.completion() == Completion::ResourceEnvelopeExhausted
            && outcome.pareto().artifacts().len() == 1
            && outcome.pareto().artifacts()[0].artifact().node_count() == 3
            && outcome.usage().verification_requests == 1
            && outcome.usage().resident_bytes <= baseline.usage().resident_bytes,
        "working growth is rejected before another Candidate batch is scheduled"
    );
    std::fs::remove_file(baseline_path).ok();
    std::fs::remove_file(target_path).ok();
}

#[test]
fn satisfying_every_goal_success_condition_stops_the_session() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-success-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let success = SuccessCondition::all(NonEmpty::one(MeasurementConstraint::new(
        Metric::NodeCount,
        ThresholdRelation::AtMost,
        3,
    )));
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, Some(success)).unwrap()),
        SeedScope::one(Expression::xor(
            Expression::xor(Expression::input(), Expression::constant(0)),
            Expression::constant(0),
        )),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(10_000).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();

    let outcome = improve(BitVecDomain::unary_u8(), request, |_| {
        ControlFlow::Continue(())
    })
    .unwrap();

    assert!(
        outcome.completion() == Completion::SuccessConditionsSatisfied
            && outcome.usage().verification_requests == 2
            && outcome.pareto().artifacts()[0].artifact().node_count() == 3,
        "success at an epoch boundary must stop before another Candidate is scheduled"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn constraints_exclude_results_without_changing_verification() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-constraint-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let constraints = [MeasurementConstraint::new(
        Metric::NodeCount,
        ThresholdRelation::AtMost,
        0,
    )];
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new(constraints, objectives, preference, None).unwrap()),
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(0),
        )),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(10_000).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();

    let outcome = improve(BitVecDomain::unary_u8(), request, |_| {
        ControlFlow::Continue(())
    })
    .unwrap();

    assert!(
        outcome.pareto().artifacts().is_empty() && outcome.usage().verification_requests == 2,
        "the Candidate remains mechanically checked but is ineligible for this Goal"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn each_goal_retains_its_own_pareto_frontier() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-multigoal-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let goal = |direction| {
        let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, direction));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
        OptimizationGoal::new([], objectives, preference, None).unwrap()
    };
    let goals =
        GoalSet::try_from_iter([goal(Direction::Minimize), goal(Direction::Maximize)]).unwrap();
    let request = ImprovementRequest::new(
        goals,
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(0),
        )),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(10_000).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();

    let outcome = improve(BitVecDomain::unary_u8(), request, |_| {
        ControlFlow::Continue(())
    })
    .unwrap();
    let mut node_counts = outcome
        .pareto()
        .artifacts()
        .iter()
        .map(|artifact| artifact.artifact().node_count())
        .collect::<Vec<_>>();
    node_counts.sort_unstable();

    assert_eq!(node_counts, [1, 3]);
    std::fs::remove_file(bundle_path).ok();
}
