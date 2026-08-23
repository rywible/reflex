use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use reflex::internal_experiments::{
    ExperienceVerdictInspection, hostile_current_knowledge_checkpoint, inspect_experience_segment,
    inspect_knowledge_recovery_obligation_count, inspect_knowledge_revision_segment,
};
use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, SessionError, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};
use reflex_bundle::{CanonicalBundle, SegmentKind};

const FIXTURE_VERIFICATIONS: u64 = 10_000;
const MAXIMUM_BUNDLE_BYTES: u64 = 64 * 1024 * 1024;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

#[test]
fn current_promoted_knowledge_resume_and_fork_replay_every_positive_through_the_kernel() {
    let directory = TestDirectory::new("current-promoted-knowledge-recovery");
    let source = directory.path().join("source.bundle");
    let resumed = directory.path().join("resumed.bundle");
    let forked = directory.path().join("forked.bundle");
    build_promoted_knowledge_bundle(&source);
    let source_bytes = std::fs::read(&source).unwrap();
    let source_bundle = CanonicalBundle::decode(&source_bytes, MAXIMUM_BUNDLE_BYTES).unwrap();
    let source_knowledge =
        inspect_knowledge_revision_segment(source_bundle.segment(SegmentKind::Revisions)).unwrap();
    assert_eq!(source_knowledge.generation, 1);
    assert!(
        source_knowledge
            .derived
            .iter()
            .any(|operator| operator.active)
    );

    let stored = read_u64(source_bundle.segment(SegmentKind::Artifacts));
    let source_experience =
        inspect_experience_segment(source_bundle.segment(SegmentKind::Experience)).unwrap();
    let accepted = source_experience
        .attempts
        .iter()
        .filter(|attempt| attempt.verdict == ExperienceVerdictInspection::Accepted)
        .count();
    let accepted = u64::try_from(accepted).unwrap();
    let knowledge_obligations = current_intelligence(source_bundle.segment(SegmentKind::Revisions));
    let knowledge_obligations = inspect_knowledge_recovery_obligation_count(knowledge_obligations)
        .map(u64::try_from)
        .unwrap()
        .unwrap();
    assert!(knowledge_obligations > 0);
    let recovery_verifications = stored
        .checked_add(accepted)
        .and_then(|count| count.checked_add(1))
        .and_then(|count| count.checked_add(knowledge_obligations))
        .unwrap();
    assert!(recovery_verifications < FIXTURE_VERIFICATIONS);

    for bundle in [
        BundlePlan::Resume {
            source: source.clone(),
            target: resumed.clone(),
        },
        BundlePlan::Fork {
            source: source.clone(),
            target: forked.clone(),
        },
    ] {
        let outcome = improve(
            BitVecDomain::unary_u8(),
            request(deep_nested_seeds(17..=17), recovery_verifications, bundle),
            |_| ControlFlow::Continue(()),
        )
        .expect("a valid promoted current Knowledge Revision must recover");
        assert_eq!(
            outcome.usage().verification_requests,
            recovery_verifications,
            "the exact envelope must be consumed by stored Artifact, Seed, and positive Experience replay through the installed Kernel",
        );
    }

    assert_eq!(std::fs::read(&source).unwrap(), source_bytes);
    for recovered in [&resumed, &forked] {
        let bytes = std::fs::read(recovered).unwrap();
        let bundle = CanonicalBundle::decode(&bytes, MAXIMUM_BUNDLE_BYTES).unwrap();
        let knowledge =
            inspect_knowledge_revision_segment(bundle.segment(SegmentKind::Revisions)).unwrap();
        assert_eq!(knowledge.generation, source_knowledge.generation);
        assert_eq!(knowledge.derived, source_knowledge.derived);
        assert_eq!(
            inspect_experience_segment(bundle.segment(SegmentKind::Experience))
                .unwrap()
                .attempts
                .len(),
            source_experience.attempts.len(),
            "an exact replay-only allowance must leave no Candidate search budget",
        );
    }
}

#[test]
fn current_authenticated_but_domain_invalid_knowledge_is_rejected_without_mutating_its_source() {
    let directory = TestDirectory::new("current-hostile-knowledge-recovery");
    let valid = directory.path().join("valid.bundle");
    let source = directory.path().join("hostile.bundle");
    let forked = directory.path().join("forked.bundle");
    build_promoted_knowledge_bundle(&valid);
    let valid_bytes = std::fs::read(valid).unwrap();
    let hostile = with_domain_invalid_knowledge(&valid_bytes);
    assert_ne!(hostile, valid_bytes);
    std::fs::write(&source, &hostile).unwrap();

    for bundle in [
        BundlePlan::Resume {
            source: source.clone(),
            target: source.clone(),
        },
        BundlePlan::Fork {
            source: source.clone(),
            target: forked.clone(),
        },
    ] {
        let result = improve(
            BitVecDomain::unary_u8(),
            request(deep_nested_seeds(17..=17), 64, bundle),
            |_| ControlFlow::Continue(()),
        );
        assert!(
            matches!(result, Err(SessionError::CorruptBundle)),
            "authenticated current Knowledge that is invalid for the installed Domain must be rejected",
        );
        assert_eq!(
            std::fs::read(&source).unwrap(),
            hostile,
            "failed Resume and Fork must leave their source byte-identical",
        );
    }
}

fn build_promoted_knowledge_bundle(path: &Path) {
    improve(
        BitVecDomain::unary_u8(),
        request(
            deep_nested_seeds(1..=8),
            FIXTURE_VERIFICATIONS,
            BundlePlan::Fresh {
                target: path.to_path_buf(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let fork = path.with_extension("fork");
    improve(
        BitVecDomain::unary_u8(),
        request(
            deep_nested_seeds(9..=16),
            FIXTURE_VERIFICATIONS,
            BundlePlan::Fork {
                source: path.to_path_buf(),
                target: fork.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    std::fs::rename(fork, path).unwrap();
}

fn with_domain_invalid_knowledge(bytes: &[u8]) -> Vec<u8> {
    const REVISION_IDS_BYTES: usize = 4 * 32;
    const INTELLIGENCE_ID_START: usize = 3 * 32;
    let mut bundle = CanonicalBundle::decode(bytes, MAXIMUM_BUNDLE_BYTES).unwrap();
    let source = bundle.segment(SegmentKind::Revisions);
    let mut input = &source[REVISION_IDS_BYTES..];
    let intelligence = take_sized(&mut input);
    assert!(input.is_empty());
    let hostile = hostile_current_knowledge_checkpoint(intelligence).unwrap();
    let hostile_identity = &hostile[hostile.len() - 32..];
    let mut revisions = source[..REVISION_IDS_BYTES].to_vec();
    revisions[INTELLIGENCE_ID_START..REVISION_IDS_BYTES].copy_from_slice(hostile_identity);
    push_sized(&mut revisions, &hostile);
    bundle.replace_segment(SegmentKind::Revisions, revisions);
    let mut session = bundle.segment(SegmentKind::Session).to_vec();
    session[9..41].copy_from_slice(&bundle.restart_state_root());
    bundle.replace_segment(SegmentKind::Session, session);
    let hostile = bundle.encode();

    let decoded = CanonicalBundle::decode(&hostile, MAXIMUM_BUNDLE_BYTES).unwrap();
    inspect_knowledge_revision_segment(decoded.segment(SegmentKind::Revisions))
        .expect("the hostile Core remains structurally authenticated and restorable");
    hostile
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

fn read_u64(input: &[u8]) -> u64 {
    u64::from_le_bytes(input[..8].try_into().unwrap())
}

fn push_sized(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&u64::try_from(bytes.len()).unwrap().to_le_bytes());
    output.extend_from_slice(bytes);
}

fn take_sized<'a>(input: &mut &'a [u8]) -> &'a [u8] {
    let count = usize::try_from(read_u64(input)).unwrap();
    let (value, remainder) = input.split_at(count + 8);
    *input = remainder;
    &value[8..]
}

fn current_intelligence(revisions: &[u8]) -> &[u8] {
    const REVISION_IDS_BYTES: usize = 4 * 32;
    let mut input = &revisions[REVISION_IDS_BYTES..];
    let intelligence = take_sized(&mut input);
    assert!(input.is_empty());
    intelligence
}

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "reflex-current-knowledge-{label}-{}-{sequence}",
            std::process::id(),
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
