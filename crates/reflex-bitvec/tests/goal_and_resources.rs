use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::time::Duration;

use reflex::{
    BundlePlan, Completion, Direction, GoalError, GoalSet, ImprovementRequest,
    MeasurementConstraint, MeasurementTolerance, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, SessionError, SuccessCondition,
    ThresholdRelation, improve,
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
fn durable_preflight_does_not_refuse_a_checkpoint_that_fits() {
    let durable_bytes = 8 * 1024;
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-durable-preflight-{}-{}.bundle",
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
            NonZeroU64::new(durable_bytes).unwrap(),
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
    let encoded_bytes = std::fs::metadata(&bundle_path).unwrap().len();

    assert_eq!(outcome.usage().verification_requests, 2);
    assert!(encoded_bytes <= durable_bytes);
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn resume_does_not_charge_verification_for_refuted_experience() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-negative-experience-budget-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let request = |verification_requests, bundle| {
        let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
        ImprovementRequest::new(
            GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
            SeedScope::one(Expression::xor(
                Expression::input(),
                Expression::constant(1),
            )),
            ResourceEnvelope::new(
                NonZeroUsize::new(1).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroU64::new(verification_requests).unwrap(),
            ),
            bundle,
        )
        .unwrap()
    };
    improve(
        BitVecDomain::unary_u8(),
        request(
            10_000,
            BundlePlan::Fresh {
                target: bundle_path.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    let outcome = improve(
        BitVecDomain::unary_u8(),
        request(
            2,
            BundlePlan::Resume {
                source: bundle_path.clone(),
                target: bundle_path.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    assert!(
        outcome.completion() == Completion::ResourceEnvelopeExhausted
            && outcome.usage().verification_requests == 2,
        "only the retained Artifact and current Seed consume mandatory recovery Verification"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn resume_still_replays_accepted_experience() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-positive-experience-budget-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let request = |verification_requests, bundle| {
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
                NonZeroU64::new(verification_requests).unwrap(),
            ),
            bundle,
        )
        .unwrap()
    };
    improve(
        BitVecDomain::unary_u8(),
        request(
            10_000,
            BundlePlan::Fresh {
                target: bundle_path.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        request(
            3,
            BundlePlan::Resume {
                source: bundle_path.clone(),
                target: bundle_path.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    );

    assert!(
        matches!(result, Err(SessionError::Resource)),
        "two retained Artifacts, one current Seed, and one Accepted Experience outcome require four replays"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn seed_ingestion_cannot_spend_candidate_verifications() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-seed-budget-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let request = ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::new(
            NonEmpty::try_from_iter([Expression::input(), Expression::constant(0)]).unwrap(),
        ),
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

    assert!(matches!(
        improve(
            BitVecDomain::unary_u8(),
            request,
            |_| ControlFlow::Continue(())
        ),
        Err(SessionError::Resource)
    ));
    assert!(!bundle_path.exists());
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
            NonZeroDuration::new(Duration::from_millis(20)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroU64::new(10_000).unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
    )
    .unwrap();

    let outcome = improve(BitVecDomain::unary_u8(), request, |_| {
        std::thread::sleep(Duration::from_millis(30));
        ControlFlow::Continue(())
    })
    .unwrap();

    assert!(
        outcome.completion() == Completion::ResourceEnvelopeExhausted
            && outcome.usage().verification_requests == 1
            && outcome.usage().worker_threads == 1
            && outcome.usage().resident_bytes > 0
            && outcome.usage().durable_bytes > 0
            && outcome.usage().elapsed_time >= Duration::from_millis(30),
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
            NonZeroU64::new(32 * 1024 * 1024).unwrap(),
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

fn durable_budget_request(
    target: PathBuf,
    durable_bytes: NonZeroU64,
    verification_requests: NonZeroU64,
) -> ImprovementRequest<BitVecDomain> {
    let goal = |direction| {
        let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, direction));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
        OptimizationGoal::new([], objectives, preference, None).unwrap()
    };
    ImprovementRequest::new(
        GoalSet::try_from_iter([goal(Direction::Minimize), goal(Direction::Maximize)]).unwrap(),
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(0),
        )),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            durable_bytes,
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            verification_requests,
        ),
        BundlePlan::Fresh { target },
    )
    .unwrap()
}

#[test]
fn durable_budget_rejects_an_uncheckpointable_frontier_atomically() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-durable-budget-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let baseline_request = durable_budget_request(
        bundle_path.clone(),
        NonZeroU64::new(16 * 1024 * 1024).unwrap(),
        NonZeroU64::new(1).unwrap(),
    );
    improve(BitVecDomain::unary_u8(), baseline_request, |_| {
        ControlFlow::Continue(())
    })
    .unwrap();
    let baseline = std::fs::read(&bundle_path).unwrap();
    let below_baseline_path = bundle_path.with_extension("below-baseline.bundle");
    let below_baseline_request = durable_budget_request(
        below_baseline_path.clone(),
        NonZeroU64::new((baseline.len() as u64).saturating_sub(1)).unwrap(),
        NonZeroU64::new(1).unwrap(),
    );
    let below_baseline = improve(BitVecDomain::unary_u8(), below_baseline_request, |_| {
        ControlFlow::Continue(())
    });

    assert!(
        matches!(below_baseline, Err(reflex::SessionError::Resource))
            && !below_baseline_path.exists(),
        "one byte below the exact verified baseline remains unaffordable"
    );
    let durable_limit = NonZeroU64::new(baseline.len() as u64).unwrap();
    let request = durable_budget_request(
        bundle_path.clone(),
        durable_limit,
        NonZeroU64::new(10_000).unwrap(),
    );
    let mut updates = 0;

    let outcome = improve(BitVecDomain::unary_u8(), request, |_| {
        updates += 1;
        ControlFlow::Continue(())
    })
    .unwrap();
    let published_bytes = std::fs::read(&bundle_path).unwrap().len() as u64;

    assert!(
        outcome.completion() == Completion::ResourceEnvelopeExhausted
            && outcome.pareto().artifacts().len() == 1
            && outcome.pareto().artifacts()[0].artifact().node_count() == 3
            && outcome.usage().durable_bytes == published_bytes
            && published_bytes <= baseline.len() as u64
            && updates == 1
            && published_bytes != 0,
        "an unaffordable Pareto transition must not be observed or enter the sealed state"
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
fn exact_initial_checkpoint_peak_is_admitted_without_counting_it_twice() {
    let directory = std::env::temp_dir();
    let baseline_path = directory.join(format!(
        "reflex-initial-peak-baseline-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let exact_path = baseline_path.with_extension("exact.bundle");
    let under_path = baseline_path.with_extension("under.bundle");
    let make_request = |resident_bytes, target| {
        let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
        let preference =
            Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
        ImprovementRequest::new(
            GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
            SeedScope::one(Expression::input()),
            ResourceEnvelope::new(
                NonZeroUsize::new(1).unwrap(),
                NonZeroU64::new(resident_bytes).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroU64::new(1).unwrap(),
            ),
            BundlePlan::Fresh { target },
        )
        .unwrap()
    };
    let baseline = improve(
        BitVecDomain::unary_u8(),
        make_request(16 * 1024 * 1024, baseline_path.clone()),
        |_| ControlFlow::Break(()),
    )
    .unwrap();
    let exact_peak = baseline.usage().resident_bytes;

    let exact = improve(
        BitVecDomain::unary_u8(),
        make_request(exact_peak, exact_path.clone()),
        |_| ControlFlow::Break(()),
    )
    .unwrap();

    assert_eq!(exact.usage().resident_bytes, exact_peak);
    let under = improve(
        BitVecDomain::unary_u8(),
        make_request(exact_peak - 1, under_path.clone()),
        |_| ControlFlow::Break(()),
    );
    assert!(matches!(under, Err(SessionError::Resource)));
    std::fs::remove_file(baseline_path).ok();
    std::fs::remove_file(exact_path).ok();
    std::fs::remove_file(under_path).ok();
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

    // The baseline is the exact one-seed seal peak. Leave explicit room for
    // constructing the larger-budget Session's fixed intelligence inventory;
    // the assertion below still requires Candidate growth itself to be refused.
    let resident_limit = baseline.usage().resident_bytes.saturating_add(64 * 1024);
    let outcome = improve(
        BitVecDomain::unary_u8(),
        request(resident_limit, 10_000, target_path.clone()),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    assert!(
        outcome.completion() == Completion::ResourceEnvelopeExhausted
            && outcome.pareto().artifacts().len() == 1
            && outcome.pareto().artifacts()[0].artifact().node_count() == 3
            && outcome.usage().verification_requests == 1
            && outcome.usage().resident_bytes <= resident_limit,
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

#[test]
fn incompatible_success_condition_is_rejected_before_search() {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-invalid-success-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let constraints = [MeasurementConstraint::new(
        Metric::NodeCount,
        ThresholdRelation::AtMost,
        1,
    )];
    let success = SuccessCondition::all(NonEmpty::one(MeasurementConstraint::new(
        Metric::NodeCount,
        ThresholdRelation::AtLeast,
        2,
    )));
    let request = ImprovementRequest::new(
        GoalSet::one(
            OptimizationGoal::new(constraints, objectives, preference, Some(success)).unwrap(),
        ),
        SeedScope::one(Expression::input()),
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

    assert!(matches!(
        improve(
            BitVecDomain::unary_u8(),
            request,
            |_| ControlFlow::Continue(())
        ),
        Err(SessionError::InvalidGoal(
            GoalError::IncompatibleSuccessCondition
        ))
    ));
    assert!(!bundle_path.exists());
}

#[test]
fn tolerances_must_uniquely_reference_objectives() {
    let objective: NonEmpty<Objective<BitVecDomain>> =
        NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let unrelated = Preference::tiered(
        NonEmpty::one(NonEmpty::one(Metric::NodeCount)),
        [MeasurementTolerance::new(Metric::Depth, 1)],
    )
    .unwrap();
    assert!(matches!(
        OptimizationGoal::new([], objective, unrelated, None),
        Err(GoalError::ToleranceDoesNotReferenceObjective)
    ));

    let objective: NonEmpty<Objective<BitVecDomain>> =
        NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let duplicate = Preference::tiered(
        NonEmpty::one(NonEmpty::one(Metric::NodeCount)),
        [
            MeasurementTolerance::new(Metric::NodeCount, 1),
            MeasurementTolerance::new(Metric::NodeCount, 2),
        ],
    )
    .unwrap();
    assert!(matches!(
        OptimizationGoal::new([], objective, duplicate, None),
        Err(GoalError::DuplicateTolerance)
    ));
}

#[test]
fn preference_tiers_steer_bootstrap_allocation_without_scalarizing_the_frontier() {
    fn run(first: Metric, second: Metric, suffix: &str) -> Vec<usize> {
        let bundle_path = std::env::temp_dir().join(format!(
            "reflex-preference-{suffix}-{}-{}.bundle",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        let deep = Expression::xor(
            Expression::rotate_left(
                Expression::rotate_left(Expression::rotate_left(Expression::input(), 1), 1),
                1,
            ),
            Expression::constant(0),
        );
        let wide = Expression::xor(
            Expression::xor(
                Expression::wrapping_add(Expression::input(), Expression::constant(1)),
                Expression::wrapping_add(Expression::input(), Expression::constant(2)),
            ),
            Expression::constant(0),
        );
        let objectives = NonEmpty::try_from_iter([
            Objective::new(Metric::NodeCount, Direction::Minimize),
            Objective::new(Metric::Depth, Direction::Minimize),
        ])
        .unwrap();
        let tiers = NonEmpty::try_from_iter([NonEmpty::one(first), NonEmpty::one(second)]).unwrap();
        let preference = Preference::tiered(
            tiers,
            [
                MeasurementTolerance::new(Metric::NodeCount, 0),
                MeasurementTolerance::new(Metric::Depth, 0),
            ],
        )
        .unwrap();
        let request = ImprovementRequest::new(
            GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
            SeedScope::new(NonEmpty::try_from_iter([deep, wide]).unwrap()),
            ResourceEnvelope::new(
                NonZeroUsize::new(1).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroU64::new(16 * 1024 * 1024).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
                NonZeroU64::new(3).unwrap(),
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
        let counts = outcome
            .pareto()
            .artifacts()
            .iter()
            .map(|artifact| artifact.artifact().node_count())
            .collect();
        std::fs::remove_file(bundle_path).ok();
        counts
    }

    let node_first = run(Metric::NodeCount, Metric::Depth, "node");
    let depth_first = run(Metric::Depth, Metric::NodeCount, "depth");
    assert!(
        node_first.contains(&4) && !depth_first.contains(&4),
        "changing only tier order must change which affordable opportunity is explored"
    );
}
