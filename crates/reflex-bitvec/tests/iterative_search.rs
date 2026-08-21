use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::time::Duration;

use reflex::{
    BundlePlan, Completion, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration,
    Objective, OptimizationGoal, Preference, ResourceEnvelope, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};

#[test]
fn session_reinvests_in_a_verified_intermediate_artifact() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-iterative-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let seed = Expression::xor(
        Expression::xor(Expression::input(), Expression::constant(0)),
        Expression::constant(0),
    );
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(seed),
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
        outcome.pareto().artifacts().len() == 1
            && outcome.pareto().artifacts()[0].artifact().node_count() == 1
            && outcome.usage().verification_requests == 3,
        "the second Operator application may run only after its parent was Verified"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn observer_can_stop_before_the_next_search_epoch() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-epoch-stop-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let seed = Expression::xor(
        Expression::xor(Expression::input(), Expression::constant(0)),
        Expression::constant(0),
    );
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(seed),
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

    let outcome = improve(BitVecDomain::unary_u8(), request, |update| {
        if update
            .added()
            .iter()
            .any(|artifact| artifact.artifact().node_count() == 3)
        {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
    .unwrap();

    assert!(
        outcome.completion() == Completion::StoppedByObserver
            && outcome.pareto().artifacts()[0].artifact().node_count() == 3
            && outcome.usage().verification_requests == 2,
        "observer stop is applied before another Candidate can consume resources"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn observer_receives_one_monotonic_delta_per_completed_epoch() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-epoch-deltas-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let seed = Expression::xor(
        Expression::xor(Expression::input(), Expression::constant(0)),
        Expression::constant(0),
    );
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(seed),
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
    let mut deltas = Vec::new();
    let mut affected_goals = Vec::new();

    improve(BitVecDomain::unary_u8(), request, |update| {
        affected_goals.push(update.affected_goals().to_vec());
        deltas.push((
            update.sequence(),
            update
                .added()
                .iter()
                .map(|artifact| artifact.artifact().node_count())
                .collect::<Vec<_>>(),
            update.removed().len(),
        ));
        ControlFlow::Continue(())
    })
    .unwrap();

    assert_eq!(
        deltas,
        vec![(1, vec![4], 0), (2, vec![3], 1), (3, vec![1], 1)],
        "each update must atomically describe one committed Pareto transition"
    );
    assert!(
        affected_goals.iter().all(|ids| ids.len() == 1)
            && affected_goals
                .windows(2)
                .all(|pair| pair[0][0] == pair[1][0]),
        "each delta names the same stable canonical Goal ID"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn canonical_dedup_does_not_reverify_an_already_known_artifact() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-search-dedup-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let seeds = NonEmpty::try_from_iter([
        Expression::input(),
        Expression::xor(Expression::input(), Expression::constant(0)),
    ])
    .unwrap();
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::new(seeds),
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

    assert_eq!(
        outcome.usage().verification_requests,
        2,
        "ArtifactKey dedup must happen before Candidate verification and scheduling"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn candidate_dedup_is_scoped_to_the_seed_relative_correctness_claim() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-claim-relative-dedup-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let seeds = NonEmpty::try_from_iter(
        [1, 2].map(|constant| Expression::xor(Expression::input(), Expression::constant(constant))),
    )
    .unwrap();
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::new(seeds),
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

    assert_eq!(
        outcome.usage().verification_requests,
        4,
        "the same Candidate Artifact requires independent verdicts for distinct Correctness Claims"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn pareto_dominance_never_crosses_seed_relative_correctness_claims() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-seed-relative-pareto-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let seeds = NonEmpty::try_from_iter([0, 1].map(|constant| {
        Expression::xor(
            Expression::xor(
                Expression::xor(Expression::input(), Expression::constant(constant)),
                Expression::constant(0),
            ),
            Expression::constant(0),
        )
    }))
    .unwrap();
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::new(seeds),
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
    let artifacts = outcome.pareto().artifacts();

    assert!(
        artifacts.len() == 2
            && artifacts[0].origin_key() != artifacts[1].origin_key()
            && artifacts
                .iter()
                .any(|artifact| artifact.artifact().node_count() == 1)
            && artifacts
                .iter()
                .any(|artifact| artifact.artifact().node_count() == 3),
        "incomparable Correctness Claims require independent per-Seed Pareto retention"
    );
    let resume_seeds = NonEmpty::try_from_iter([0, 1].map(|constant| {
        Expression::xor(
            Expression::xor(
                Expression::xor(Expression::input(), Expression::constant(constant)),
                Expression::constant(0),
            ),
            Expression::constant(0),
        )
    }))
    .unwrap();
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let resumed = improve(
        BitVecDomain::unary_u8(),
        ImprovementRequest::new(
            GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
            SeedScope::new(resume_seeds),
            ResourceEnvelope::new(
                NonZeroUsize::new(1).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroU64::new(10_000).unwrap(),
            ),
            BundlePlan::Resume {
                source: bundle_path.clone(),
                target: bundle_path.clone(),
            },
        )
        .unwrap(),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    assert_eq!(resumed.pareto().artifacts().len(), 2);
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn verification_budget_stops_before_an_unaffordable_epoch() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-epoch-budget-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let seed = Expression::xor(
        Expression::xor(Expression::input(), Expression::constant(0)),
        Expression::constant(0),
    );
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(seed),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(2).unwrap(),
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
        outcome.completion() == Completion::ResourceEnvelopeExhausted
            && outcome.usage().verification_requests == 2
            && outcome.pareto().artifacts()[0].artifact().node_count() == 3,
        "the second search epoch cannot start when its verification is unaffordable"
    );
    std::fs::remove_file(bundle_path).ok();
}
