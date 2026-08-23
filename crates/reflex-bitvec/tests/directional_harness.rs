mod support;

use reflex::internal_experiments::inspect_intelligence_revision_segment;
use reflex::{BundlePlan, Completion, NonEmpty};
use reflex_bitvec::{Expression, SeedScope};
use reflex_bundle::{CanonicalBundle, SegmentKind};

use support::{ResourceLimits, TestDirectory, run_identity_session, run_node_count_session};

const FIRST_XOR_MASK: u8 = 0x25;
const SECOND_XOR_MASK: u8 = 0x29;

#[test]
fn public_improve_is_a_restart_complete_deterministic_resource_bounded_seam() {
    let directory = TestDirectory::new("public-improve-tracer");
    let limits = ResourceLimits::tiny();
    let first_path = directory.path().join("first.bundle");
    let second_path = directory.path().join("second.bundle");
    let resumed_path = directory.path().join("resumed.bundle");

    let first = run_identity_session(
        BundlePlan::Fresh {
            target: first_path.clone(),
        },
        limits,
    );
    let second = run_identity_session(
        BundlePlan::Fresh {
            target: second_path.clone(),
        },
        limits,
    );
    let resumed = run_identity_session(
        BundlePlan::Resume {
            source: first_path.clone(),
            target: resumed_path.clone(),
        },
        limits,
    );

    assert_eq!(first.completion, Completion::NoEligibleWork);
    assert_eq!(second.completion, first.completion);
    assert_eq!(resumed.completion, first.completion);
    assert_eq!(first.semantic, second.semantic);
    assert_eq!(first.updates, second.updates);
    assert_eq!(
        first.usage.verification_requests,
        second.usage.verification_requests
    );
    assert_eq!(resumed.semantic, first.semantic);
    assert!(resumed.updates.is_empty());
    assert_eq!(
        intelligence_snapshot(&first_path),
        intelligence_snapshot(&second_path)
    );
    let first_intelligence = intelligence_snapshot(&first_path);
    let resumed_intelligence = intelligence_snapshot(&resumed_path);
    assert!(
        resumed_intelligence.receipts >= first_intelligence.receipts,
        "Resume may perform new bounded internal work but cannot erase causal receipts"
    );
    assert_eq!(
        resumed_intelligence.settlements, resumed_intelligence.receipts,
        "every new internal action is terminally settled"
    );
    assert_eq!(
        resumed_intelligence.consequences, first_intelligence.consequences,
        "a no-artifact Resume cannot fabricate semantic consequences"
    );
    assert_eq!(resumed_intelligence.open_shadows, 0);

    for run in [&first, &second, &resumed] {
        assert!(run.bundle_is_file);
        limits.assert_contains(run.usage);
    }
}

#[test]
fn attractive_refuted_decoys_never_cross_the_verification_seam() {
    let directory = TestDirectory::new("refuted-decoys");
    let limits = ResourceLimits::tiny();
    let bundle_path = directory.path().join("decoys.bundle");
    let run = run_node_count_session(
        SeedScope::one(Expression::xor(
            Expression::input(),
            Expression::constant(FIRST_XOR_MASK),
        )),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
        limits,
    );
    let expected = (u8::MIN..=u8::MAX)
        .map(|input| input ^ FIRST_XOR_MASK)
        .collect::<Vec<_>>();

    assert_eq!(
        run.completion,
        Completion::NoEligibleWork,
        "usage: {:?}",
        run.usage
    );
    assert!(!run.semantic.artifacts.is_empty());
    assert!(
        run.semantic
            .artifacts
            .iter()
            .all(|artifact| artifact.truth_table == expected)
    );
    assert!(run.bundle_is_file);
    limits.assert_contains(run.usage);
    let intelligence = intelligence_snapshot(&bundle_path);
    assert!(intelligence.receipts > 0);
    assert_eq!(intelligence.settlements, intelligence.receipts);
}

#[test]
fn refuted_decoys_do_not_block_a_verified_two_step_collapse() {
    let directory = TestDirectory::new("two-step-collapse");
    let limits = ResourceLimits::tiny();
    let run = run_node_count_session(
        SeedScope::one(Expression::xor(
            Expression::xor(Expression::input(), Expression::constant(FIRST_XOR_MASK)),
            Expression::constant(FIRST_XOR_MASK),
        )),
        BundlePlan::Fresh {
            target: directory.path().join("two-step.bundle"),
        },
        limits,
    );
    let identity = (u8::MIN..=u8::MAX).collect::<Vec<_>>();

    assert_eq!(run.completion, Completion::NoEligibleWork);
    assert!(run.semantic.contains_artifact(1, &identity));
    assert!(run.bundle_is_file);
    limits.assert_contains(run.usage);
}

#[test]
fn competing_claims_retain_independent_verified_improvements() {
    let directory = TestDirectory::new("competing-claims");
    let limits = ResourceLimits::training();
    let bundle_path = directory.path().join("claims.bundle");
    let padded_xor = |constant| {
        Expression::xor(
            Expression::xor(
                Expression::xor(Expression::input(), Expression::constant(constant)),
                Expression::constant(0),
            ),
            Expression::constant(0),
        )
    };
    let run = run_node_count_session(
        SeedScope::new(
            NonEmpty::try_from_iter(
                [0, FIRST_XOR_MASK, SECOND_XOR_MASK]
                    .into_iter()
                    .chain(1..=13)
                    .map(padded_xor),
            )
            .unwrap(),
        ),
        BundlePlan::Fresh {
            target: bundle_path.clone(),
        },
        limits,
    );
    let expected = [
        (1, (u8::MIN..=u8::MAX).collect::<Vec<_>>()),
        (
            3,
            (u8::MIN..=u8::MAX)
                .map(|input| input ^ FIRST_XOR_MASK)
                .collect::<Vec<_>>(),
        ),
        (
            3,
            (u8::MIN..=u8::MAX)
                .map(|input| input ^ SECOND_XOR_MASK)
                .collect::<Vec<_>>(),
        ),
    ];

    assert_eq!(run.completion, Completion::NoEligibleWork);
    assert!(run.semantic.contains_artifacts(&expected));
    assert!(run.bundle_is_file);
    limits.assert_contains(run.usage);
    let intelligence = intelligence_snapshot(&bundle_path);
    assert_eq!(intelligence.settlements, intelligence.receipts);
    assert!(
        intelligence.active_specialists >= 1,
        "a class-diverse public-path campaign must durably spawn a native specialist; inspection: {intelligence:?}"
    );
}

fn intelligence_snapshot(
    path: &std::path::Path,
) -> reflex::internal_experiments::IntelligenceInspection {
    let bytes = std::fs::read(path).unwrap();
    let bundle = CanonicalBundle::decode(&bytes, 16 * 1024 * 1024).unwrap();
    inspect_intelligence_revision_segment(bundle.segment(SegmentKind::Revisions)).unwrap()
}
