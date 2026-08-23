use std::collections::BTreeSet;
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use reflex::internal_experiments::{
    ExperienceVerdictInspection, inspect_experience_segment, inspect_intelligence_revision_segment,
    inspect_knowledge_revision_segment, inspect_session_segment,
};
use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};
use reflex_bundle::{CanonicalBundle, SegmentKind};

const CRASH_HELPER_ENV: &str = "REFLEX_KNOWLEDGE_SHADOW_CRASH_HELPER";
const CRASH_SOURCE_ENV: &str = "REFLEX_KNOWLEDGE_SHADOW_CRASH_SOURCE";
const CRASH_TARGET_ENV: &str = "REFLEX_KNOWLEDGE_SHADOW_CRASH_TARGET";
const FAULT_PHASE_ENV: &str = "REFLEX_INTERNAL_TEST_FAULT_PHASE";

#[test]
fn immediate_compression_without_descendant_value_remains_verified() {
    let directory = std::env::temp_dir().join(format!(
        "reflex-knowledge-consolidation-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let trained = directory.join("trained.bundle");
    let learned_full = directory.join("learned-full.bundle");

    improve(
        BitVecDomain::unary_u8(),
        request(
            nested_seeds(1..=8),
            10_000,
            BundlePlan::Fresh {
                target: trained.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let trained_snapshot = inspect_bundle_knowledge(&trained);
    assert_eq!(
        trained_snapshot.generation, 0,
        "a proposed Knowledge Product must not bypass verification and causal promotion"
    );
    assert!(trained_snapshot.derived.is_empty());
    let trained_bundle =
        CanonicalBundle::decode(&std::fs::read(&trained).unwrap(), 16 * 1024 * 1024).unwrap();
    let compiler =
        inspect_intelligence_revision_segment(trained_bundle.segment(SegmentKind::Revisions))
            .unwrap();
    assert_eq!(compiler.promoted_knowledge, 0);
    assert!(compiler.verified_knowledge >= 1);

    std::fs::copy(&trained, &learned_full).unwrap();
    let heldout = nested_seeds(9..=16);
    improve(
        BitVecDomain::unary_u8(),
        request(
            heldout.clone(),
            10_000,
            BundlePlan::Fork {
                source: learned_full.clone(),
                target: learned_full.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let learned_full_snapshot = inspect_bundle_knowledge(&learned_full);
    let learned_bundle =
        CanonicalBundle::decode(&std::fs::read(&learned_full).unwrap(), 16 * 1024 * 1024).unwrap();
    let compiler =
        inspect_intelligence_revision_segment(learned_bundle.segment(SegmentKind::Revisions))
            .unwrap();
    assert_eq!(learned_full_snapshot.generation, 0);
    assert_eq!(compiler.promoted_knowledge, 0);
    assert!(compiler.verified_knowledge >= 1);
    assert_eq!(compiler.open_shadows, 0);
    assert_eq!(compiler.completed_shadows, 1);
    assert!(
        compiler.contextual_contrasts >= 2,
        "immediate compression without extra verified descendants must remain only Verified"
    );

    improve(
        BitVecDomain::unary_u8(),
        request(
            nested_seeds(17..=24),
            10_000,
            BundlePlan::Fork {
                source: learned_full.clone(),
                target: learned_full.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .expect("a completed non-promoting Knowledge campaign must recover and retry");

    std::fs::remove_dir_all(directory).ok();
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one public-path gate proves causal promotion, active reuse, and Bootstrap equivalence"
)]
fn verified_descendant_value_promotes_and_reuses_the_exact_derived_operator() {
    let directory = std::env::temp_dir().join(format!(
        "reflex-knowledge-positive-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let trained = directory.join("trained.bundle");
    let learned = directory.join("learned.bundle");
    let bootstrap = directory.join("bootstrap.bundle");

    improve(
        BitVecDomain::unary_u8(),
        request(
            deep_nested_seeds(1..=8),
            10_000,
            BundlePlan::Fresh {
                target: trained.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let provisional = inspect_bundle_knowledge(&trained);
    assert_eq!(provisional.generation, 0);
    assert!(provisional.derived.is_empty());

    std::fs::copy(&trained, &learned).unwrap();
    improve(
        BitVecDomain::unary_u8(),
        request(
            deep_nested_seeds(9..=16),
            10_000,
            BundlePlan::Fork {
                source: learned.clone(),
                target: learned.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let promoted = inspect_bundle_knowledge(&learned);
    let compiler = inspect_bundle_intelligence(&learned);
    assert_eq!(promoted.generation, 1);
    assert_eq!(compiler.promoted_knowledge, 1);
    assert!(
        promoted
            .derived
            .iter()
            .any(|operator| operator.active && operator.steps == 2 && operator.support >= 8)
    );

    let before = inspect_bundle_experience(&learned).attempts.len();
    let later = deep_nested_seeds(17..=24);
    improve(
        BitVecDomain::unary_u8(),
        request(
            later.clone(),
            10_000,
            BundlePlan::Fork {
                source: learned.clone(),
                target: learned.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    improve(
        BitVecDomain::unary_u8(),
        request(
            later,
            10_000,
            BundlePlan::Fresh {
                target: bootstrap.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    let learned_experience = inspect_bundle_experience(&learned);
    let learned_later = &learned_experience.attempts[before..];
    let derived_attempt = learned_later.iter().any(|attempt| {
        attempt.verdict == ExperienceVerdictInspection::Accepted
            && attempt.operator_symbol.starts_with(b"derived:")
    });
    assert!(
        derived_attempt,
        "active Derived Operator must contribute on later held-out claims: {:?}",
        learned_later
            .iter()
            .map(|attempt| (
                String::from_utf8_lossy(&attempt.operator_symbol).into_owned(),
                attempt.allocation_queue,
                attempt.verdict,
            ))
            .collect::<Vec<_>>()
    );
    let learned_semantics = learned_later
        .iter()
        .filter(|attempt| attempt.verdict == ExperienceVerdictInspection::Accepted)
        .map(|attempt| attempt.canonical_candidate.clone())
        .collect::<BTreeSet<_>>();
    let bootstrap_semantics = inspect_bundle_experience(&bootstrap)
        .attempts
        .into_iter()
        .filter(|attempt| attempt.verdict == ExperienceVerdictInspection::Accepted)
        .map(|attempt| attempt.canonical_candidate)
        .collect::<BTreeSet<_>>();
    assert_eq!(learned_semantics, bootstrap_semantics);

    std::fs::remove_dir_all(directory).ok();
}

#[test]
#[cfg(debug_assertions)]
fn treatment_crash_invalidates_open_campaign_and_allows_fresh_retry() {
    let directory = std::env::temp_dir().join(format!(
        "reflex-knowledge-shadow-crash-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let trained = directory.join("trained.bundle");
    let interrupted = directory.join("interrupted.bundle");

    improve(
        BitVecDomain::unary_u8(),
        request(
            nested_seeds(1..=8),
            10_000,
            BundlePlan::Fresh {
                target: trained.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "knowledge_shadow_treatment_crash_helper",
            "--nocapture",
        ])
        .env(CRASH_HELPER_ENV, "1")
        .env(CRASH_SOURCE_ENV, &trained)
        .env(CRASH_TARGET_ENV, &interrupted)
        .env(FAULT_PHASE_ENV, "knowledge-shadow-treatment-completed")
        .status()
        .unwrap();
    assert!(
        !status.success() && status.code().is_none(),
        "the treatment-completed cut must abort the subprocess"
    );
    let crashed = inspect_bundle_intelligence(&interrupted);
    assert_eq!(crashed.open_shadows, 1);
    assert_eq!(crashed.interrupted_shadows, 0);

    improve(
        BitVecDomain::unary_u8(),
        request(
            nested_seeds(9..=16),
            10_000,
            BundlePlan::Resume {
                source: interrupted.clone(),
                target: interrupted.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .expect("restart-complete recovery must reconcile the retained Open campaign");
    let recovered = inspect_bundle_intelligence(&interrupted);
    assert_eq!(recovered.open_shadows, 0);
    assert_eq!(recovered.interrupted_shadows, 1);
    assert!(
        recovered.completed_shadows >= 1,
        "after interruption reconciliation the same Verified product must be eligible for a fresh campaign"
    );

    let campaigns_after_recovery =
        recovered.completed_shadows + recovered.interrupted_shadows + recovered.open_shadows;
    improve(
        BitVecDomain::unary_u8(),
        request(
            nested_seeds(17..=24),
            10_000,
            BundlePlan::Fork {
                source: interrupted.clone(),
                target: interrupted.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .expect("an interrupted campaign must not strand future Knowledge trials");
    let retried = inspect_bundle_intelligence(&interrupted);
    assert!(
        retried.completed_shadows + retried.interrupted_shadows + retried.open_shadows
            > campaigns_after_recovery,
        "a later completed Session must be able to schedule another fresh campaign"
    );

    std::fs::remove_dir_all(directory).ok();
}

#[test]
#[cfg(debug_assertions)]
fn verification_fault_cuts_recover_the_exact_old_open_or_terminal_state() {
    let directory = std::env::temp_dir().join(format!(
        "reflex-knowledge-verification-crash-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    std::fs::create_dir_all(&directory).unwrap();
    for (phase, retained_open) in [
        ("knowledge-verification-open-staged", false),
        ("knowledge-verification-open-published", true),
        ("knowledge-verification-settlement-staged", true),
        ("knowledge-verification-settlement-published", false),
    ] {
        let interrupted = directory.join(format!("{phase}.bundle"));
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "knowledge_shadow_treatment_crash_helper",
                "--nocapture",
            ])
            .env(CRASH_HELPER_ENV, "1")
            .env(CRASH_TARGET_ENV, &interrupted)
            .env(FAULT_PHASE_ENV, phase)
            .status()
            .unwrap();
        assert!(
            !status.success() && status.code().is_none(),
            "the configured Knowledge verification cut must abort: {phase}"
        );
        let opened =
            CanonicalBundle::decode(&std::fs::read(&interrupted).unwrap(), 16 * 1024 * 1024)
                .unwrap();
        let interrupted_session =
            inspect_session_segment(opened.segment(SegmentKind::Session)).unwrap();
        assert!(!interrupted_session.completed);
        if retained_open {
            assert!(
                interrupted_session.usage.verification_requests > 8,
                "a durable Open must charge every reserved obligation: {phase}"
            );
        }

        let outcome = improve(
            BitVecDomain::unary_u8(),
            request(
                nested_seeds(9..=16),
                10_000,
                BundlePlan::Resume {
                    source: interrupted.clone(),
                    target: interrupted.clone(),
                },
            ),
            |_| ControlFlow::Continue(()),
        )
        .expect("every legal cut must recover an exact old, Open, or terminal state");
        assert!(
            outcome.usage().verification_requests
                >= interrupted_session.usage.verification_requests,
            "recovery must not release published request usage: {phase}"
        );
        if retained_open {
            assert!(inspect_bundle_intelligence(&interrupted).invalidated_knowledge >= 1);
        }
    }

    std::fs::remove_dir_all(directory).ok();
}

#[test]
fn knowledge_shadow_treatment_crash_helper() {
    if std::env::var_os(CRASH_HELPER_ENV).is_none() {
        return;
    }
    let target = std::path::PathBuf::from(std::env::var_os(CRASH_TARGET_ENV).unwrap());
    let bundle = std::env::var_os(CRASH_SOURCE_ENV).map_or_else(
        || BundlePlan::Fresh {
            target: target.clone(),
        },
        |source| BundlePlan::Resume {
            source: source.into(),
            target: target.clone(),
        },
    );
    improve(
        BitVecDomain::unary_u8(),
        request(nested_seeds(9..=16), 10_000, bundle),
        |_| ControlFlow::Continue(()),
    )
    .expect("the configured Knowledge shadow fault must abort this process");
    panic!("configured Knowledge shadow fault was not reached");
}

fn nested_seeds(constants: impl IntoIterator<Item = u8>) -> Vec<Expression> {
    constants
        .into_iter()
        .map(|constant| {
            Expression::xor(
                Expression::xor(
                    Expression::xor(Expression::input(), Expression::constant(constant)),
                    Expression::constant(0),
                ),
                Expression::constant(0),
            )
        })
        .collect()
}

fn deep_nested_seeds(constants: impl IntoIterator<Item = u8>) -> Vec<Expression> {
    constants
        .into_iter()
        .map(|constant| {
            Expression::xor(
                Expression::xor(
                    Expression::xor(
                        Expression::xor(Expression::input(), Expression::constant(constant)),
                        Expression::constant(0),
                    ),
                    Expression::constant(0),
                ),
                Expression::constant(0),
            )
        })
        .collect()
}

fn request(
    seeds: Vec<Expression>,
    verification_requests: u64,
    bundle: BundlePlan,
) -> ImprovementRequest<BitVecDomain> {
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::new(NonEmpty::try_from_iter(seeds).unwrap()),
        ResourceEnvelope::new(
            NonZeroUsize::new(2).unwrap(),
            NonZeroU64::new(64 * 1024 * 1024).unwrap(),
            NonZeroU64::new(64 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(10)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(10)).unwrap(),
            NonZeroU64::new(verification_requests).unwrap(),
        ),
        bundle,
    )
    .unwrap()
}

fn inspect_bundle_knowledge(path: &Path) -> reflex::internal_experiments::KnowledgeInspection {
    let bytes = std::fs::read(path).unwrap();
    let bundle = CanonicalBundle::decode(&bytes, 16 * 1024 * 1024).unwrap();
    inspect_knowledge_revision_segment(bundle.segment(SegmentKind::Revisions)).unwrap()
}

fn inspect_bundle_intelligence(
    path: &Path,
) -> reflex::internal_experiments::IntelligenceInspection {
    let bytes = std::fs::read(path).unwrap();
    let bundle = CanonicalBundle::decode(&bytes, 16 * 1024 * 1024).unwrap();
    inspect_intelligence_revision_segment(bundle.segment(SegmentKind::Revisions)).unwrap()
}

fn inspect_bundle_experience(path: &Path) -> reflex::internal_experiments::ExperienceInspection {
    let bytes = std::fs::read(path).unwrap();
    let bundle = CanonicalBundle::decode(&bytes, 16 * 1024 * 1024).unwrap();
    inspect_experience_segment(bundle.segment(SegmentKind::Experience)).unwrap()
}
