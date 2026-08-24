use std::fmt::Write as _;
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reflex::internal_experiments::{
    CandidateFateInspection, CandidateFateOutcomeInspection, ExperienceVerdictInspection,
    empty_legacy_learning_state, inspect_experience_segment, legacy_learning_revision,
    legacy_v12_intelligence_checkpoint, legacy_v13_intelligence_checkpoint,
    pre_action_v23_experience_segment,
};
use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, SessionError, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};
use reflex_bundle::{CanonicalBundle, SegmentKind};
use sha2::{Digest, Sha256};

const COMPLETED_V20: &[u8] = include_bytes!("fixtures/v20-completed-rich.bundle");
const INTERRUPTED_V20: &[u8] = include_bytes!("fixtures/v20-interrupted.bundle");
const AUTHENTIC_COMPLETED_V22: &[u8] = include_bytes!("fixtures/v22-completed-authentic.bundle");
const AUTHENTIC_INTERRUPTED_V22: &[u8] =
    include_bytes!("fixtures/v22-interrupted-authentic.bundle");
const AUTHENTIC_COMPLETED_V23: &[u8] = include_bytes!("fixtures/v23-completed-authentic.bundle");
const AUTHENTIC_COMPLETED_V24: &[u8] = include_bytes!("fixtures/v24-completed-authentic.bundle");
const AUTHENTIC_INTERRUPTED_V24: &[u8] =
    include_bytes!("fixtures/v24-interrupted-authentic.bundle");
const AUTHENTIC_COMPLETED_V25: &[u8] = include_bytes!("fixtures/v25-completed-authentic.bundle");
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);
const V20_ATTEMPTS: [([u8; 32], ExperienceVerdictInspection); 4] = [
    (
        [
            235, 239, 21, 39, 54, 124, 226, 57, 227, 155, 211, 71, 31, 31, 105, 172, 140, 119, 90,
            62, 99, 212, 85, 151, 93, 203, 16, 141, 206, 12, 223, 198,
        ],
        ExperienceVerdictInspection::Accepted,
    ),
    (
        [
            50, 47, 91, 255, 247, 49, 224, 214, 152, 213, 72, 171, 236, 230, 169, 128, 14, 93, 206,
            228, 12, 157, 31, 169, 211, 12, 98, 24, 157, 17, 152, 61,
        ],
        ExperienceVerdictInspection::Refuted,
    ),
    (
        [
            70, 29, 81, 44, 159, 148, 156, 146, 30, 59, 35, 95, 235, 210, 37, 30, 41, 41, 220, 226,
            55, 179, 184, 198, 157, 246, 148, 179, 168, 156, 158, 252,
        ],
        ExperienceVerdictInspection::Refuted,
    ),
    (
        [
            142, 23, 182, 200, 219, 216, 208, 211, 59, 117, 22, 74, 121, 60, 7, 192, 58, 218, 8,
            55, 78, 4, 49, 38, 22, 113, 207, 6, 3, 85, 172, 137,
        ],
        ExperienceVerdictInspection::Refuted,
    ),
];

#[test]
fn authentic_completed_v22_through_v24_fixtures_recover_without_source_mutation() {
    let fixtures = [
        (
            "v22",
            AUTHENTIC_COMPLETED_V22,
            "5286f0d072acaea9ac83cef110a24acc4d5096edb99b88269bc18a0fad094280",
        ),
        (
            "v23",
            AUTHENTIC_COMPLETED_V23,
            "d0452ce39ca79c63bf9e478452a883fc9dceda887562a724beeb0a775e97b2e4",
        ),
        (
            "v24",
            AUTHENTIC_COMPLETED_V24,
            "8e9f441f28e657dd53005c3a7dd76ab63a86af0d6c6f09951332f8f71069adcd",
        ),
    ];
    for (revision, fixture, expected_hash) in fixtures {
        assert_eq!(sha256_hex(fixture), expected_hash);
        assert_eq!(
            runtime_revision(fixture),
            revision[1..].parse::<u64>().unwrap()
        );
        let directory = TestDirectory::new(&format!("authentic-{revision}"));
        let source = directory.path().join("source.bundle");
        let target = directory.path().join("target.bundle");
        std::fs::write(&source, fixture).unwrap();

        improve(
            BitVecDomain::unary_u8(),
            request_with_verifications(
                BundlePlan::Fork {
                    source: source.clone(),
                    target: target.clone(),
                },
                10_000,
            ),
            |_| ControlFlow::Continue(()),
        )
        .unwrap_or_else(|error| {
            panic!("authentic {revision} fixture failed to recover: {error:?}")
        });

        assert_eq!(std::fs::read(&source).unwrap(), fixture);
        assert_eq!(runtime_revision(&std::fs::read(target).unwrap()), 25);
    }
}

#[test]
fn authentic_current_v25_fixture_is_a_byte_exact_format_oracle() {
    let expected_hash = "23a0d428829c66ed4d1a8dec0511fad4d5e68f0e6459b269c4fab4fabed0d69b";
    assert_eq!(sha256_hex(AUTHENTIC_COMPLETED_V25), expected_hash);
    assert_eq!(runtime_revision(AUTHENTIC_COMPLETED_V25), 25);

    let directory = TestDirectory::new("authentic-v25");
    let oracle = directory.path().join("oracle.bundle");
    std::fs::write(&oracle, AUTHENTIC_COMPLETED_V25).unwrap();
    assert_eq!(std::fs::read(oracle).unwrap(), AUTHENTIC_COMPLETED_V25);
}

#[test]
fn authentic_interrupted_v22_and_v24_fixtures_are_rejected_byte_exactly() {
    let fixtures = [
        (
            "v22",
            AUTHENTIC_INTERRUPTED_V22,
            "f6b32c4ec81d45b9f9bb251eb62cb2d93703817e0126fb46b06ab571d36911f2",
        ),
        (
            "v24",
            AUTHENTIC_INTERRUPTED_V24,
            "f3535108bc6acf9b19ca9c9db7bbde19835dec84a42508035957bcbf625014c5",
        ),
    ];
    for (revision, fixture, expected_hash) in fixtures {
        assert_eq!(sha256_hex(fixture), expected_hash);
        let directory = TestDirectory::new(&format!("authentic-interrupted-{revision}"));
        let source = directory.path().join("source.bundle");
        std::fs::write(&source, fixture).unwrap();

        let result = improve(
            BitVecDomain::unary_u8(),
            interrupted_request(BundlePlan::Resume {
                source: source.clone(),
                target: source.clone(),
            }),
            |_| ControlFlow::Continue(()),
        );

        assert!(matches!(result, Err(SessionError::IncompatibleBundle)));
        assert_eq!(std::fs::read(source).unwrap(), fixture);
    }
}

#[test]
fn completed_v20_resume_is_rejected_without_replacing_the_source() {
    let directory = TestDirectory::new("v20-completed-resume");
    let source = directory.path().join("source.bundle");
    std::fs::write(&source, COMPLETED_V20).unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    );

    assert!(matches!(result, Err(SessionError::IncompatibleBundle)));
    assert_eq!(std::fs::read(source).unwrap(), COMPLETED_V20);
}

#[test]
fn completed_v20_with_current_revisions_framing_is_rejected_before_import() {
    let directory = TestDirectory::new("v20-current-revisions-framing");
    let source = directory.path().join("source.bundle");
    let target = directory.path().join("target.bundle");
    let crossed = relabel_segment_version(COMPLETED_V20, SegmentKind::Revisions, 5);
    std::fs::write(&source, &crossed).unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Fork {
            source: source.clone(),
            target,
        }),
        |_| ControlFlow::Continue(()),
    );

    assert!(matches!(result, Err(SessionError::CorruptBundle)));
    assert_eq!(std::fs::read(source).unwrap(), crossed);
}

#[test]
fn current_v25_with_legacy_revisions_framing_is_rejected_before_resume() {
    let directory = TestDirectory::new("v22-legacy-revisions-framing");
    let source = directory.path().join("source.bundle");
    improve(
        BitVecDomain::unary_u8(),
        request_with_verifications(
            BundlePlan::Fresh {
                target: source.clone(),
            },
            8,
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let crossed =
        relabel_segment_version(&std::fs::read(&source).unwrap(), SegmentKind::Revisions, 4);
    std::fs::write(&source, &crossed).unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    );

    assert!(matches!(result, Err(SessionError::CorruptBundle)));
    assert_eq!(std::fs::read(source).unwrap(), crossed);
}

#[test]
fn current_v25_resume_and_fork_preserve_core_owned_policy_and_knowledge() {
    let directory = TestDirectory::new("v22-runtime-policy-state");
    let source = directory.path().join("source.bundle");
    let resumed = directory.path().join("resumed.bundle");
    let forked = directory.path().join("forked.bundle");
    improve(
        BitVecDomain::unary_u8(),
        request_with_verifications(
            BundlePlan::Fresh {
                target: source.clone(),
            },
            8,
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let source_policy = runtime_policy_revision_id(&std::fs::read(&source).unwrap());
    let source_knowledge = knowledge_revision_id(&std::fs::read(&source).unwrap());
    assert_eq!(runtime_revision(&std::fs::read(&source).unwrap()), 25);
    assert!(!has_standalone_knowledge(&std::fs::read(&source).unwrap()));
    assert!(!has_standalone_runtime_policy(
        &std::fs::read(&source).unwrap()
    ));

    let resumed_outcome = improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Resume {
            source: source.clone(),
            target: resumed.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let forked_outcome = improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Fork {
            source: source.clone(),
            target: forked.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    assert!(resumed_outcome.usage().verification_requests > 0);
    assert!(forked_outcome.usage().verification_requests > 0);

    assert_eq!(
        runtime_policy_revision_id(&std::fs::read(&resumed).unwrap()),
        source_policy
    );
    assert_eq!(
        runtime_policy_revision_id(&std::fs::read(&forked).unwrap()),
        source_policy
    );
    assert_eq!(
        knowledge_revision_id(&std::fs::read(resumed).unwrap()),
        source_knowledge
    );
    assert_eq!(
        knowledge_revision_id(&std::fs::read(forked).unwrap()),
        source_knowledge
    );
}

#[test]
fn completed_v23_migrates_pre_action_experience_and_reseals_v25() {
    let directory = TestDirectory::new("v23-action-provenance-migration");
    let source = directory.path().join("source.bundle");
    let legacy = as_completed_v23(&current_bundle_bytes(&directory));
    assert_eq!(runtime_revision(&legacy), 23);
    std::fs::write(&source, legacy).unwrap();

    improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    assert_eq!(runtime_revision(&std::fs::read(source).unwrap()), 25);
}

#[test]
fn completed_v24_defaults_the_absent_generation_phase_and_reseals_v25() {
    let directory = TestDirectory::new("v24-generation-phase-migration");
    let source = directory.path().join("source.bundle");
    let legacy = as_completed_v24(&current_bundle_bytes(&directory));
    assert_eq!(runtime_revision(&legacy), 24);
    std::fs::write(&source, legacy).unwrap();

    improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    assert_eq!(runtime_revision(&std::fs::read(source).unwrap()), 25);
}

#[test]
fn completed_v21_migrates_its_standalone_policy_into_rfic_once() {
    let directory = TestDirectory::new("v21-runtime-policy-migration");
    let source = directory.path().join("source.bundle");
    improve(
        BitVecDomain::unary_u8(),
        request_with_verifications(
            BundlePlan::Fresh {
                target: source.clone(),
            },
            2,
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let legacy = as_completed_v21(&std::fs::read(&source).unwrap());
    std::fs::write(&source, &legacy).unwrap();

    improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    let migrated = std::fs::read(source).unwrap();
    assert_eq!(runtime_revision(&migrated), 25);
    assert!(!has_standalone_knowledge(&migrated));
    assert!(!has_standalone_runtime_policy(&migrated));
    let bootstrap_revision: [u8; 32] = Sha256::digest(bootstrap_runtime_policy_revision()).into();
    assert_eq!(runtime_policy_revision_id(&migrated), bootstrap_revision);
}

#[test]
fn interrupted_v21_is_rejected_without_importing_or_replacing_the_source() {
    let directory = TestDirectory::new("v21-interrupted-policy");
    let source = directory.path().join("source.bundle");
    let current = current_bundle_bytes_with_verifications(&directory, 2);
    let completed = as_completed_v21(&current);
    let interrupted = as_interrupted(&completed);
    std::fs::write(&source, &interrupted).unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        interrupted_request(BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    );

    assert!(matches!(result, Err(SessionError::IncompatibleBundle)));
    assert_eq!(std::fs::read(source).unwrap(), interrupted);
}

#[test]
fn completed_v22_imports_standalone_knowledge_once_and_reseals_v25() {
    let directory = TestDirectory::new("v22-knowledge-migration");
    let source = directory.path().join("source.bundle");
    let current = current_bundle_bytes_with_verifications(&directory, 2);
    let legacy = as_completed_v22(&current);
    std::fs::write(&source, &legacy).unwrap();

    improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    let migrated = std::fs::read(source).unwrap();
    assert_eq!(runtime_revision(&migrated), 25);
    assert!(!has_standalone_knowledge(&migrated));
}

#[test]
fn completed_v22_rejects_a_valid_but_artifact_incompatible_knowledge_revision() {
    let directory = TestDirectory::new("v22-hostile-knowledge");
    let source = directory.path().join("source.bundle");
    let current = current_bundle_bytes_with_verifications(&directory, 2);
    let legacy = as_completed_v22(&current);
    let hostile_knowledge = artifact_incompatible_knowledge_state();
    let hostile = replace_v22_knowledge(&legacy, &hostile_knowledge);
    std::fs::write(&source, &hostile).unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    );

    let error = result
        .err()
        .expect("hostile v22 Knowledge must be rejected");
    assert!(
        matches!(error, SessionError::CorruptBundle),
        "hostile v22 Knowledge must be corrupt, got {error:?}"
    );
    assert_eq!(std::fs::read(source).unwrap(), hostile);
}

#[test]
fn interrupted_v22_is_rejected_without_importing_or_replacing_the_source() {
    let directory = TestDirectory::new("v22-interrupted-knowledge");
    let source = directory.path().join("source.bundle");
    let current = current_bundle_bytes_with_verifications(&directory, 2);
    let completed = as_completed_v22(&current);
    let interrupted = as_interrupted(&completed);
    std::fs::write(&source, &interrupted).unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        interrupted_request(BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    );

    assert!(matches!(result, Err(SessionError::IncompatibleBundle)));
    assert_eq!(std::fs::read(source).unwrap(), interrupted);
}

#[test]
fn crossed_v20_v21_session_payloads_are_rejected_without_mutating_the_source() {
    let directory = TestDirectory::new("crossed-session-payloads");
    let current = current_bundle_bytes(&directory);

    assert_crossed_payload_rejected(
        &directory,
        "v20-session-current-state",
        &current,
        COMPLETED_V20,
        SegmentKind::Session,
        false,
    );
    assert_crossed_payload_rejected(
        &directory,
        "current-session-v20-state",
        COMPLETED_V20,
        &current,
        SegmentKind::Session,
        false,
    );
}

#[test]
fn crossed_v20_v21_revisions_payloads_are_rejected_without_mutating_the_source() {
    let directory = TestDirectory::new("crossed-revisions-payloads");
    let current = current_bundle_bytes(&directory);

    assert_crossed_payload_rejected(
        &directory,
        "v20-revisions-current-state",
        &current,
        COMPLETED_V20,
        SegmentKind::Revisions,
        true,
    );
    assert_crossed_payload_rejected(
        &directory,
        "current-revisions-v20-state",
        COMPLETED_V20,
        &current,
        SegmentKind::Revisions,
        false,
    );
}

#[test]
fn crossed_v20_v21_experience_payloads_are_rejected_without_mutating_the_source() {
    let directory = TestDirectory::new("crossed-experience-payloads");
    let current = current_bundle_bytes(&directory);

    assert_crossed_payload_rejected(
        &directory,
        "v20-experience-current-state",
        &current,
        COMPLETED_V20,
        SegmentKind::Experience,
        true,
    );
    assert_crossed_payload_rejected(
        &directory,
        "current-experience-v20-state",
        COMPLETED_V20,
        &current,
        SegmentKind::Experience,
        false,
    );
}

#[test]
fn completed_v20_fork_preserves_verified_artifacts_and_complete_experience_in_v25() {
    let directory = TestDirectory::new("v20-completed-fork");
    let source = directory.path().join("source.bundle");
    let target = directory.path().join("target.bundle");
    std::fs::write(&source, COMPLETED_V20).unwrap();
    let v20_bundle = CanonicalBundle::decode(COMPLETED_V20, 16 * 1024 * 1024).unwrap();
    let v20_artifact_keys = artifact_keys(&v20_bundle);
    assert_eq!(v20_artifact_keys.len(), 3);

    improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Fork {
            source: source.clone(),
            target: target.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    let output = std::fs::read(target).unwrap();
    let v21 = snapshot(&output);
    assert_eq!(runtime_revision(&output), 25);
    assert!(!has_standalone_knowledge(&output));
    assert!(
        v20_artifact_keys
            .iter()
            .all(|key| v21.artifact_keys.contains(key))
    );
    for (candidate_key, verdict) in V20_ATTEMPTS {
        assert!(v21.attempts.iter().any(|attempt| {
            attempt.candidate_key == candidate_key && attempt.verdict == verdict
        }));
        assert!(v21.candidate_fates.iter().any(|fate| {
            fate.candidate_key == candidate_key
                && matches!(
                    fate.outcome,
                    CandidateFateOutcomeInspection::Verified {
                        verdict: actual,
                        ..
                    } if actual == verdict
                )
        }));
    }
    assert_eq!(v21.attempts.len(), 4);
    assert!(v21.candidate_fates.len() >= 4);
    assert_eq!(v21.consequence_count, 3);
    assert_eq!(v21.measurements.len(), 1);
    assert_eq!(v21.measurements[0].1, 6);
    assert_eq!(std::fs::read(source).unwrap(), COMPLETED_V20);
}

#[test]
fn interrupted_v20_resume_is_rejected_without_replacing_the_source() {
    let directory = TestDirectory::new("v20-interrupted-resume");
    let source = directory.path().join("source.bundle");
    std::fs::write(&source, INTERRUPTED_V20).unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        interrupted_request(BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    );

    assert!(matches!(result, Err(SessionError::IncompatibleBundle)));
    assert_eq!(std::fs::read(source).unwrap(), INTERRUPTED_V20);
}

#[test]
fn interrupted_v20_fork_is_rejected_without_replacing_the_source() {
    let directory = TestDirectory::new("v20-interrupted-fork");
    let source = directory.path().join("source.bundle");
    std::fs::write(&source, INTERRUPTED_V20).unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        interrupted_request(BundlePlan::Fork {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    );

    assert!(matches!(result, Err(SessionError::IncompatibleBundle)));
    assert_eq!(std::fs::read(source).unwrap(), INTERRUPTED_V20);
}

#[test]
fn failed_v20_import_replay_leaves_the_source_byte_identical() {
    let directory = TestDirectory::new("v20-failed-import");
    let source = directory.path().join("source.bundle");
    let invalid_replay = v20_with_invalid_replay_evidence();
    std::fs::write(&source, &invalid_replay).unwrap();

    let result = improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Fork {
            source: source.clone(),
            target: source.clone(),
        }),
        |_| ControlFlow::Continue(()),
    );

    assert!(matches!(result, Err(SessionError::CorruptBundle)));
    assert_eq!(std::fs::read(source).unwrap(), invalid_replay);
}

#[derive(Debug)]
struct BundleSnapshot {
    artifact_keys: Vec<[u8; 32]>,
    attempts: Vec<AttemptSnapshot>,
    candidate_fates: Vec<CandidateFateInspection>,
    consequence_count: usize,
    measurements: Vec<(Vec<u8>, usize)>,
}

#[derive(Debug, Eq, PartialEq)]
struct AttemptSnapshot {
    candidate_key: [u8; 32],
    verdict: ExperienceVerdictInspection,
}

fn snapshot(bytes: &[u8]) -> BundleSnapshot {
    let bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    let experience = inspect_experience_segment(bundle.segment(SegmentKind::Experience)).unwrap();
    BundleSnapshot {
        artifact_keys: artifact_keys(&bundle),
        attempts: experience
            .attempts
            .into_iter()
            .map(|attempt| AttemptSnapshot {
                candidate_key: attempt.candidate_key,
                verdict: attempt.verdict,
            })
            .collect(),
        candidate_fates: experience.candidate_fates,
        consequence_count: experience.consequence_count,
        measurements: experience
            .measurements
            .into_iter()
            .map(|measurement| (measurement.environment, measurement.value_count))
            .collect(),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

fn artifact_keys(bundle: &CanonicalBundle) -> Vec<[u8; 32]> {
    let mut input = bundle.segment(SegmentKind::Artifacts);
    let count = usize::try_from(read_u64(&mut input)).unwrap();
    let mut keys = Vec::with_capacity(count);
    for _ in 0..count {
        let canonical = take_sized(&mut input);
        let mut digest = Sha256::new();
        digest.update(b"reflex-artifact-v1\0");
        digest.update((bundle.identity().len() as u64).to_le_bytes());
        digest.update(bundle.identity());
        digest.update(canonical);
        keys.push(digest.finalize().into());
        take_sized(&mut input);
        take_sized(&mut input);
        read_u64(&mut input);
        take(&mut input, 32);
        if take(&mut input, 1)[0] == 1 {
            take(&mut input, 32);
        }
        take_sized(&mut input);
    }
    assert!(input.is_empty());
    keys
}

fn runtime_revision(bytes: &[u8]) -> u64 {
    let bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    let session = bundle.segment(SegmentKind::Session);
    u64::from_le_bytes(session[1..9].try_into().unwrap())
}

fn runtime_policy_revision_id(bytes: &[u8]) -> [u8; 32] {
    let bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    bundle.segment(SegmentKind::Revisions)[64..96]
        .try_into()
        .unwrap()
}

fn knowledge_revision_id(bytes: &[u8]) -> [u8; 32] {
    let bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    bundle.segment(SegmentKind::Revisions)[..32]
        .try_into()
        .unwrap()
}

fn has_standalone_knowledge(bytes: &[u8]) -> bool {
    let bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    let mut revisions = &bundle.segment(SegmentKind::Revisions)[128..];
    let first = take_sized(&mut revisions);
    !first.starts_with(b"RFIC")
}

fn has_standalone_runtime_policy(bytes: &[u8]) -> bool {
    let bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    let mut revisions = &bundle.segment(SegmentKind::Revisions)[128..];
    if take_sized(&mut revisions).starts_with(b"RFIC") {
        return false;
    }
    take_sized(&mut revisions);
    revisions.starts_with(&(bootstrap_runtime_policy_state().len() as u64).to_le_bytes())
}

fn as_completed_v21(bytes: &[u8]) -> Vec<u8> {
    let pre_action = as_completed_v23(bytes);
    let bytes = pre_action.as_slice();
    let mut bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    let source = bundle.segment(SegmentKind::Revisions);
    let mut input = &source[128..];
    let knowledge = empty_legacy_knowledge_state();
    let intelligence = legacy_v12_intelligence_checkpoint(take_sized(&mut input)).unwrap();
    let learning = empty_legacy_learning_state();
    assert!(input.is_empty());
    let policy = bootstrap_runtime_policy_state();
    let mut revisions = source[..128].to_vec();
    revisions[..32].copy_from_slice(&legacy_knowledge_revision_id(&bundle, &knowledge));
    revisions[32..64].copy_from_slice(
        &legacy_learning_revision(&learning, std::str::from_utf8(bundle.identity()).unwrap())
            .unwrap(),
    );
    revisions[64..96].copy_from_slice(&Sha256::digest(&policy));
    revisions[96..128].copy_from_slice(&intelligence[intelligence.len() - 32..]);
    push_sized(&mut revisions, &knowledge);
    push_sized(&mut revisions, &learning);
    push_sized(&mut revisions, &policy);
    push_sized(&mut revisions, &intelligence);
    bundle.replace_segment(SegmentKind::Revisions, revisions);

    let mut session = bundle.segment(SegmentKind::Session).to_vec();
    session[1..9].copy_from_slice(&21_u64.to_le_bytes());
    bundle.replace_segment(SegmentKind::Session, session);
    let relabeled = relabel_segment_version(&bundle.encode(), SegmentKind::Revisions, 5);
    let mut legacy = CanonicalBundle::decode(&relabeled, 16 * 1024 * 1024).unwrap();
    let mut session = legacy.segment(SegmentKind::Session).to_vec();
    session[9..41].copy_from_slice(&legacy.restart_state_root());
    legacy.replace_segment(SegmentKind::Session, session);
    legacy.encode()
}

fn as_completed_v22(bytes: &[u8]) -> Vec<u8> {
    let pre_action = as_completed_v23(bytes);
    let bytes = pre_action.as_slice();
    let mut bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    let source = bundle.segment(SegmentKind::Revisions);
    let mut input = &source[128..];
    let intelligence = legacy_v13_intelligence_checkpoint(take_sized(&mut input)).unwrap();
    assert!(input.is_empty());
    let knowledge = empty_legacy_knowledge_state();
    let mut revisions = source[..128].to_vec();
    revisions[..32].copy_from_slice(&legacy_knowledge_revision_id(&bundle, &knowledge));
    revisions[96..128].copy_from_slice(&intelligence[intelligence.len() - 32..]);
    push_sized(&mut revisions, &knowledge);
    push_sized(&mut revisions, &intelligence);
    bundle.replace_segment(SegmentKind::Revisions, revisions);

    let mut session = bundle.segment(SegmentKind::Session).to_vec();
    session[1..9].copy_from_slice(&22_u64.to_le_bytes());
    bundle.replace_segment(SegmentKind::Session, session);
    let relabeled = relabel_segment_version(&bundle.encode(), SegmentKind::Revisions, 6);
    let mut legacy = CanonicalBundle::decode(&relabeled, 16 * 1024 * 1024).unwrap();
    let mut session = legacy.segment(SegmentKind::Session).to_vec();
    session[9..41].copy_from_slice(&legacy.restart_state_root());
    legacy.replace_segment(SegmentKind::Session, session);
    legacy.encode()
}

fn as_completed_v23(bytes: &[u8]) -> Vec<u8> {
    let mut bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    assert_eq!(runtime_revision(bytes), 25);
    let experience =
        pre_action_v23_experience_segment(bundle.segment(SegmentKind::Experience)).unwrap();
    bundle.replace_segment(SegmentKind::Experience, experience);

    let mut recovery = bundle.segment(SegmentKind::Recovery).to_vec();
    assert_eq!(recovery[recovery.len() - 9..], [0; 9]);
    recovery.truncate(recovery.len() - 9);
    bundle.replace_segment(SegmentKind::Recovery, recovery);

    let mut session = bundle.segment(SegmentKind::Session).to_vec();
    session[1..9].copy_from_slice(&23_u64.to_le_bytes());
    bundle.replace_segment(SegmentKind::Session, session);
    let mut pre_action = CanonicalBundle::decode(&bundle.encode(), 16 * 1024 * 1024).unwrap();
    let mut session = pre_action.segment(SegmentKind::Session).to_vec();
    session[9..41].copy_from_slice(&pre_action.restart_state_root());
    pre_action.replace_segment(SegmentKind::Session, session);
    pre_action.encode()
}

fn as_completed_v24(bytes: &[u8]) -> Vec<u8> {
    let mut bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    assert_eq!(runtime_revision(bytes), 25);

    let mut recovery = bundle.segment(SegmentKind::Recovery).to_vec();
    assert_eq!(
        recovery.pop(),
        Some(0),
        "completed current state is not mid-generation"
    );
    bundle.replace_segment(SegmentKind::Recovery, recovery);

    let mut session = bundle.segment(SegmentKind::Session).to_vec();
    session[1..9].copy_from_slice(&24_u64.to_le_bytes());
    bundle.replace_segment(SegmentKind::Session, session);
    let mut legacy = CanonicalBundle::decode(&bundle.encode(), 16 * 1024 * 1024).unwrap();
    let mut session = legacy.segment(SegmentKind::Session).to_vec();
    session[9..41].copy_from_slice(&legacy.restart_state_root());
    legacy.replace_segment(SegmentKind::Session, session);
    legacy.encode()
}

fn replace_v22_knowledge(bytes: &[u8], knowledge: &[u8]) -> Vec<u8> {
    let mut bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    let source = bundle.segment(SegmentKind::Revisions);
    let mut input = &source[128..];
    take_sized(&mut input);
    let intelligence = take_sized(&mut input).to_vec();
    assert!(input.is_empty());
    let mut revisions = source[..128].to_vec();
    revisions[..32].copy_from_slice(&legacy_knowledge_revision_id(&bundle, knowledge));
    push_sized(&mut revisions, knowledge);
    push_sized(&mut revisions, &intelligence);
    bundle.replace_segment(SegmentKind::Revisions, revisions);
    let mut session = bundle.segment(SegmentKind::Session).to_vec();
    session[9..41].copy_from_slice(&bundle.restart_state_root());
    bundle.replace_segment(SegmentKind::Session, session);
    bundle.encode()
}

fn as_interrupted(bytes: &[u8]) -> Vec<u8> {
    let mut bundle = CanonicalBundle::decode(bytes, 16 * 1024 * 1024).unwrap();
    let mut session = bundle.segment(SegmentKind::Session).to_vec();
    session[0] = 0;
    bundle.replace_segment(SegmentKind::Session, session);
    bundle.encode()
}

fn empty_legacy_knowledge_state() -> Vec<u8> {
    let mut revision = b"RFKR\x02".to_vec();
    revision.extend_from_slice(&0_u64.to_le_bytes());
    revision.extend_from_slice(&0_u64.to_le_bytes());
    revision.extend_from_slice(&0_u64.to_le_bytes());
    let mut state = b"RFKS\x02".to_vec();
    state.extend_from_slice(&0_u64.to_le_bytes());
    push_sized(&mut state, &revision);
    state.push(0);
    state
}

fn artifact_incompatible_knowledge_state() -> Vec<u8> {
    let mut predecessor = b"RFKR\x02".to_vec();
    predecessor.extend_from_slice(&0_u64.to_le_bytes());
    predecessor.extend_from_slice(&0_u64.to_le_bytes());
    predecessor.extend_from_slice(&0_u64.to_le_bytes());
    let mut champion = b"RFKR\x02".to_vec();
    champion.extend_from_slice(&0_u64.to_le_bytes());
    champion.extend_from_slice(&1_u64.to_le_bytes());
    champion.extend_from_slice(&[0xff; 32]);
    champion.extend_from_slice(&0_u64.to_le_bytes());
    let mut state = b"RFKS\x02".to_vec();
    state.extend_from_slice(&1_u64.to_le_bytes());
    push_sized(&mut state, &champion);
    state.push(1);
    push_sized(&mut state, &predecessor);
    state
}

fn legacy_knowledge_revision_id(bundle: &CanonicalBundle, state: &[u8]) -> [u8; 32] {
    let mut state_input = state;
    assert_eq!(take(&mut state_input, 5), b"RFKS\x02");
    read_u64(&mut state_input);
    let revision = take_sized(&mut state_input);
    let mut revision_digest = Sha256::new();
    revision_digest.update(b"reflex-knowledge-revision-v1\0");
    revision_digest.update((bundle.identity().len() as u64).to_le_bytes());
    revision_digest.update(bundle.identity());
    revision_digest.update(revision);

    let mut digest = Sha256::new();
    digest.update(b"reflex-knowledge-revision-v1\0");
    digest.update((bundle.identity().len() as u64).to_le_bytes());
    digest.update(bundle.identity());
    let mut artifacts = bundle.segment(SegmentKind::Artifacts);
    let count = read_u64(&mut artifacts);
    for _ in 0..count {
        let canonical = take_sized(&mut artifacts);
        let mut artifact_digest = Sha256::new();
        artifact_digest.update(b"reflex-artifact-v1\0");
        artifact_digest.update((bundle.identity().len() as u64).to_le_bytes());
        artifact_digest.update(bundle.identity());
        artifact_digest.update(canonical);
        digest.update(artifact_digest.finalize());
        take_sized(&mut artifacts);
        take_sized(&mut artifacts);
        read_u64(&mut artifacts);
        digest.update(take(&mut artifacts, 32));
        let parent = take(&mut artifacts, 1)[0];
        digest.update([parent]);
        if parent == 1 {
            digest.update(take(&mut artifacts, 32));
        }
        let provenance = take_sized(&mut artifacts);
        digest.update((provenance.len() as u64).to_le_bytes());
        digest.update(provenance);
    }
    assert!(artifacts.is_empty());
    digest.update(revision_digest.finalize());
    digest.finalize().into()
}

fn bootstrap_runtime_policy_revision() -> Vec<u8> {
    let mut revision = b"RFPR\x02".to_vec();
    for value in [8_u16, 1, 1, 1, 2, 8, 8] {
        revision.extend_from_slice(&value.to_le_bytes());
    }
    for value in [32_u32, 8, 32, 128] {
        revision.extend_from_slice(&value.to_le_bytes());
    }
    for value in [250_u16, 50] {
        revision.extend_from_slice(&value.to_le_bytes());
    }
    let checksum: [u8; 32] = Sha256::digest(&revision).into();
    revision.extend_from_slice(&checksum);
    revision
}

fn bootstrap_runtime_policy_state() -> Vec<u8> {
    let mut state = b"RFPS\x01".to_vec();
    state.extend_from_slice(&0_u64.to_le_bytes());
    state.extend_from_slice(&bootstrap_runtime_policy_revision());
    state.push(0);
    state.extend_from_slice(&0_u16.to_le_bytes());
    let checksum: [u8; 32] = Sha256::digest(&state).into();
    state.extend_from_slice(&checksum);
    state
}

fn push_sized(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
}

fn v20_with_invalid_replay_evidence() -> Vec<u8> {
    let mut bundle = CanonicalBundle::decode(COMPLETED_V20, 16 * 1024 * 1024).unwrap();
    let mut artifacts = bundle.segment(SegmentKind::Artifacts).to_vec();
    let mut offset = 8;
    for _ in 0..2 {
        let length = usize::try_from(u64::from_le_bytes(
            artifacts[offset..offset + 8].try_into().unwrap(),
        ))
        .unwrap();
        offset += 8 + length;
    }
    let evidence_length = usize::try_from(u64::from_le_bytes(
        artifacts[offset..offset + 8].try_into().unwrap(),
    ))
    .unwrap();
    assert_eq!(evidence_length, 256);
    artifacts[offset + 8] ^= 0xff;
    bundle.replace_segment(SegmentKind::Artifacts, artifacts);
    bundle.encode()
}

fn current_bundle_bytes(directory: &TestDirectory) -> Vec<u8> {
    current_bundle_bytes_with_verifications(directory, 64)
}

fn current_bundle_bytes_with_verifications(
    directory: &TestDirectory,
    verifications: u64,
) -> Vec<u8> {
    let path = directory.path().join("current.bundle");
    improve(
        BitVecDomain::unary_u8(),
        request_with_verifications(
            BundlePlan::Fresh {
                target: path.clone(),
            },
            verifications,
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    std::fs::read(path).unwrap()
}

fn assert_crossed_payload_rejected(
    directory: &TestDirectory,
    label: &str,
    base: &[u8],
    donor: &[u8],
    kind: SegmentKind,
    resume: bool,
) {
    let mut crossed = CanonicalBundle::decode(base, 16 * 1024 * 1024).unwrap();
    let donor = CanonicalBundle::decode(donor, 16 * 1024 * 1024).unwrap();
    crossed.replace_segment(kind, donor.segment(kind).to_vec());
    let crossed = crossed.encode();
    let source = directory.path().join(format!("{label}.bundle"));
    let target = directory.path().join(format!("{label}-target.bundle"));
    std::fs::write(&source, &crossed).unwrap();
    let bundle = if resume {
        BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        }
    } else {
        BundlePlan::Fork {
            source: source.clone(),
            target,
        }
    };

    let result = improve(BitVecDomain::unary_u8(), request(bundle), |_| {
        ControlFlow::Continue(())
    });

    assert!(
        matches!(result, Err(SessionError::CorruptBundle)),
        "crossed {kind:?} payload must be rejected as corrupt"
    );
    assert_eq!(std::fs::read(source).unwrap(), crossed);
}

fn relabel_segment_version(bytes: &[u8], target: SegmentKind, version: u32) -> Vec<u8> {
    let mut output = bytes.to_vec();
    let content_len = output.len() - 32;
    let mut offset = 8;
    let identity_len = usize::try_from(u64::from_le_bytes(
        output[offset..offset + 8].try_into().unwrap(),
    ))
    .unwrap();
    offset += 8 + identity_len + 4;
    for _ in 0..5 {
        if output[offset] == target as u8 {
            output[offset + 1..offset + 5].copy_from_slice(&version.to_le_bytes());
            let checksum: [u8; 32] = Sha256::digest(&output[..content_len]).into();
            output[content_len..].copy_from_slice(&checksum);
            return output;
        }
        let stored_len = usize::try_from(u64::from_le_bytes(
            output[offset + 5..offset + 13].try_into().unwrap(),
        ))
        .unwrap();
        offset += 13 + stored_len + 32;
    }
    panic!("encoded Bundle contains every canonical Segment")
}

fn take_sized<'a>(input: &mut &'a [u8]) -> &'a [u8] {
    let count = usize::try_from(read_u64(input)).unwrap();
    take(input, count)
}

fn read_u64(input: &mut &[u8]) -> u64 {
    u64::from_le_bytes(take(input, 8).try_into().unwrap())
}

fn take<'a>(input: &mut &'a [u8], count: usize) -> &'a [u8] {
    let (value, remainder) = input.split_at(count);
    *input = remainder;
    value
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "reflex-v20-migration-{label}-{}-{sequence}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).ok();
    }
}

fn request(bundle: BundlePlan) -> ImprovementRequest<BitVecDomain> {
    request_with_verifications(bundle, 64)
}

fn request_with_verifications(
    bundle: BundlePlan,
    verification_requests: u64,
) -> ImprovementRequest<BitVecDomain> {
    let goal = OptimizationGoal::new(
        [],
        NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize)),
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap(),
        None,
    )
    .unwrap();
    let seeds = SeedScope::new(
        NonEmpty::try_from_iter([
            Expression::xor(Expression::input(), Expression::constant(0)),
            Expression::xor(Expression::input(), Expression::constant(1)),
        ])
        .unwrap(),
    );
    ImprovementRequest::new(
        GoalSet::one(goal),
        seeds,
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroU64::new(16 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(10)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(10)).unwrap(),
            NonZeroU64::new(verification_requests).unwrap(),
        ),
        bundle,
    )
    .unwrap()
}

fn interrupted_request(bundle: BundlePlan) -> ImprovementRequest<BitVecDomain> {
    let goal = OptimizationGoal::new(
        [],
        NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize)),
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap(),
        None,
    )
    .unwrap();
    let seeds = SeedScope::one(Expression::xor(
        Expression::xor(Expression::input(), Expression::constant(0)),
        Expression::constant(0),
    ));
    ImprovementRequest::new(
        GoalSet::one(goal),
        seeds,
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
}
