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
        1,
    )));
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, Some(success)).unwrap()),
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

    assert_eq!(outcome.completion(), Completion::SuccessConditionsSatisfied);
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
