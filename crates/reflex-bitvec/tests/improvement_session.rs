use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::time::Duration;

use reflex::{
    BundlePlan, Completion, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration,
    Objective, OptimizationGoal, Preference, ResourceEnvelope, SessionError, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};

#[test]
fn session_returns_a_smaller_verified_equivalent() {
    let seed = Expression::xor(Expression::input(), Expression::constant(0));
    let seeds = SeedScope::one(seed);
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference = Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), [])
        .expect("one priority tier names the one objective");
    let goals = GoalSet::one(
        OptimizationGoal::new([], objectives, preference, None)
            .expect("the goal is structurally valid"),
    );

    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-tracer-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let request = ImprovementRequest::new(
        goals,
        seeds,
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
    .expect("the request is structurally valid");

    let mut observed_smaller = false;
    let outcome = improve(BitVecDomain::unary_u8(), request, |update| {
        observed_smaller |= update.added().iter().any(|artifact| {
            artifact.artifact().node_count() == 1
                && (0..=u8::MAX).all(|input| artifact.artifact().evaluate(input) == input)
        });
        ControlFlow::Continue(())
    })
    .expect("the Improvement Session succeeds");

    assert!(
        observed_smaller
            && outcome.completion() == Completion::NoEligibleWork
            && outcome.usage().verification_requests == 2
            && outcome.pareto().artifacts().iter().any(|artifact| {
                artifact.artifact().node_count() == 1
                    && artifact.provenance() == b"simplify-known-identity"
                    && (0..=u8::MAX).all(|input| artifact.artifact().evaluate(input) == input)
            })
            && bundle_path.is_file(),
        "the verified one-node equivalent is observed, retained, and durably published"
    );

    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn resume_rejects_corrupt_verification_metadata() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-recovery-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));

    let make_request = |bundle| {
        let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
        ImprovementRequest::new(
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
                NonZeroU64::new(10_000).unwrap(),
            ),
            bundle,
        )
        .unwrap()
    };

    improve(
        BitVecDomain::unary_u8(),
        make_request(BundlePlan::Fresh {
            target: bundle_path.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    let mut corrupted = std::fs::read(&bundle_path).unwrap();
    *corrupted.last_mut().expect("bundle has a kernel revision") ^= 0xff;
    std::fs::write(&bundle_path, corrupted).unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        make_request(BundlePlan::Resume {
            source: bundle_path.clone(),
            target: bundle_path.clone(),
        }),
        |_| ControlFlow::Continue(()),
    );

    assert!(
        matches!(result, Err(SessionError::CorruptBundle)),
        "Resume must reject bytes that fail bundle integrity before replay"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn resume_preserves_keys_and_observes_only_frontier_deltas() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-resume-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let make_request = |bundle| {
        let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
        ImprovementRequest::new(
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
                NonZeroU64::new(10_000).unwrap(),
            ),
            bundle,
        )
        .unwrap()
    };
    let fresh = improve(
        BitVecDomain::unary_u8(),
        make_request(BundlePlan::Fresh {
            target: bundle_path.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let retained_key = fresh.pareto().artifacts()[0].key();

    let mut update_count = 0;
    let resumed = improve(
        BitVecDomain::unary_u8(),
        make_request(BundlePlan::Resume {
            source: bundle_path.clone(),
            target: bundle_path.clone(),
        }),
        |_| {
            update_count += 1;
            ControlFlow::Continue(())
        },
    )
    .unwrap();

    assert!(
        resumed.pareto().artifacts()[0].key() == retained_key && update_count == 0,
        "Resume reverifies retained state without reporting it as a new Pareto improvement"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn resume_continues_search_from_a_persisted_verified_intermediate() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-resume-frontier-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let make_request = |bundle| {
        let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
        ImprovementRequest::new(
            GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
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
            bundle,
        )
        .unwrap()
    };
    let stopped = improve(
        BitVecDomain::unary_u8(),
        make_request(BundlePlan::Fresh {
            target: bundle_path.clone(),
        }),
        |update| {
            if update
                .added()
                .iter()
                .any(|artifact| artifact.artifact().node_count() == 3)
            {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        },
    )
    .unwrap();
    assert_eq!(stopped.completion(), Completion::StoppedByObserver);

    let resumed = improve(
        BitVecDomain::unary_u8(),
        make_request(BundlePlan::Resume {
            source: bundle_path.clone(),
            target: bundle_path.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    assert!(
        resumed.pareto().artifacts().len() == 1
            && resumed.pareto().artifacts()[0].artifact().node_count() == 1,
        "Resume must rebuild search eligibility from persisted verified Seed lineage"
    );
    std::fs::remove_file(bundle_path).ok();
}
