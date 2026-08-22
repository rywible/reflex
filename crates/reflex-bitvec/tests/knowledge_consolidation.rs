use std::collections::BTreeSet;
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::Path;
use std::time::Duration;

use reflex::internal_experiments::{ExperienceVerdictInspection, inspect_experience_segment};
use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};
use reflex_bundle::{CanonicalBundle, SegmentKind};

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one public-path gate proves extraction, constrained reuse, and semantic equivalence"
)]
fn verified_chains_become_bounded_macros_for_later_sessions() {
    let directory = std::env::temp_dir().join(format!(
        "reflex-knowledge-consolidation-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let trained = directory.join("trained.bundle");
    let constrained = directory.join("constrained.bundle");
    let learned_full = directory.join("learned-full.bundle");
    let bootstrap_constrained = directory.join("bootstrap-constrained.bundle");
    let bootstrap_full = directory.join("bootstrap-full.bundle");

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
    let trained_snapshot = snapshot(&trained);
    assert!(
        trained_snapshot.knowledge_generation >= 1
            && trained_snapshot
                .derived
                .iter()
                .any(|operator| operator.active && operator.steps == 2 && operator.support >= 8),
        "eight distinct verified claims must consolidate the repeated two-step chain"
    );

    std::fs::copy(&trained, &constrained).unwrap();
    std::fs::copy(&trained, &learned_full).unwrap();
    let heldout = nested_seeds(9..=16);
    let replay_count = trained_snapshot.artifact_count
        + trained_snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.accepted)
            .count()
        + heldout.len();
    improve(
        BitVecDomain::unary_u8(),
        request(
            heldout.clone(),
            u64::try_from(replay_count + heldout.len()).unwrap(),
            BundlePlan::Resume {
                source: constrained.clone(),
                target: constrained.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    improve(
        BitVecDomain::unary_u8(),
        request(
            heldout.clone(),
            10_000,
            BundlePlan::Resume {
                source: learned_full.clone(),
                target: learned_full.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    improve(
        BitVecDomain::unary_u8(),
        request(
            heldout.clone(),
            u64::try_from(heldout.len() * 2).unwrap(),
            BundlePlan::Fresh {
                target: bootstrap_constrained.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let learned = snapshot(&constrained);
    let bootstrap = snapshot(&bootstrap_constrained);
    let learned_new = &learned.attempts[trained_snapshot.attempts.len()..];
    assert!(
        learned_new.iter().any(|attempt| {
            attempt.accepted && attempt.nodes == 3 && attempt.operator.starts_with(b"derived:")
        }) && bootstrap
            .attempts
            .iter()
            .filter(|attempt| attempt.accepted)
            .all(|attempt| attempt.nodes == 5),
        "protected Derived Operator exploration must reach a two-step result inside a budget where Bootstrap reaches only intermediates"
    );

    improve(
        BitVecDomain::unary_u8(),
        request(
            heldout.clone(),
            10_000,
            BundlePlan::Resume {
                source: learned_full.clone(),
                target: learned_full.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .expect("a completed bundle containing exercised Derived Operators must recover");
    improve(
        BitVecDomain::unary_u8(),
        request(
            heldout,
            10_000,
            BundlePlan::Fresh {
                target: bootstrap_full.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let learned_full_snapshot = snapshot(&learned_full);
    let bootstrap_full_snapshot = snapshot(&bootstrap_full);
    let learned_semantics = learned_full_snapshot.attempts[trained_snapshot.attempts.len()..]
        .iter()
        .filter(|attempt| attempt.accepted)
        .map(|attempt| attempt.canonical.clone())
        .collect::<BTreeSet<_>>();
    let bootstrap_semantics = bootstrap_full_snapshot
        .attempts
        .iter()
        .filter(|attempt| attempt.accepted)
        .map(|attempt| attempt.canonical.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(learned_semantics, bootstrap_semantics);

    std::fs::remove_dir_all(directory).ok();
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

struct Snapshot {
    artifact_count: usize,
    knowledge_generation: u64,
    derived: Vec<Derived>,
    attempts: Vec<Attempt>,
}

struct Derived {
    active: bool,
    steps: usize,
    support: usize,
}

struct Attempt {
    canonical: Vec<u8>,
    operator: Vec<u8>,
    nodes: u32,
    accepted: bool,
}

fn snapshot(path: &Path) -> Snapshot {
    let bytes = std::fs::read(path).unwrap();
    let bundle = CanonicalBundle::decode(&bytes, 16 * 1024 * 1024).unwrap();
    let artifacts = bundle.segment(SegmentKind::Artifacts);
    let revisions = bundle.segment(SegmentKind::Revisions);
    let experience = inspect_experience_segment(bundle.segment(SegmentKind::Experience)).unwrap();
    let artifact_count =
        usize::try_from(u64::from_le_bytes(artifacts[..8].try_into().unwrap())).unwrap();
    let knowledge_length =
        usize::try_from(u64::from_le_bytes(revisions[64..72].try_into().unwrap())).unwrap();
    let mut knowledge = &revisions[72..72 + knowledge_length];
    assert_eq!(take(&mut knowledge, 5), b"RFKS\x02");
    let knowledge_generation = read_u64(&mut knowledge);
    let champion_length = usize::try_from(read_u64(&mut knowledge)).unwrap();
    let mut champion = take(&mut knowledge, champion_length);
    assert_eq!(take(&mut champion, 5), b"RFKR\x02");
    read_u64(&mut champion);
    let active_count = usize::try_from(read_u64(&mut champion)).unwrap();
    take(&mut champion, active_count * 32);
    let operator_count = usize::try_from(read_u64(&mut champion)).unwrap();
    let mut derived = Vec::with_capacity(operator_count);
    for _ in 0..operator_count {
        take(&mut champion, 32);
        let symbol_length = usize::try_from(read_u64(&mut champion)).unwrap();
        take(&mut champion, symbol_length);
        let active = take(&mut champion, 1)[0] == 1;
        read_u64(&mut champion);
        read_u64(&mut champion);
        let step_count = usize::try_from(read_u64(&mut champion)).unwrap();
        for _ in 0..step_count {
            let length = usize::try_from(read_u64(&mut champion)).unwrap();
            take(&mut champion, length);
        }
        let support_count = usize::try_from(read_u64(&mut champion)).unwrap();
        take(&mut champion, support_count * 32);
        derived.push(Derived {
            active,
            steps: step_count,
            support: support_count,
        });
    }

    let attempts = experience
        .attempts
        .into_iter()
        .map(|attempt| {
            let canonical = attempt.canonical_candidate;
            let nodes = u32::from_le_bytes(canonical[..4].try_into().unwrap());
            Attempt {
                canonical,
                operator: attempt.operator_symbol,
                nodes,
                accepted: attempt.verdict == ExperienceVerdictInspection::Accepted,
            }
        })
        .collect();
    Snapshot {
        artifact_count,
        knowledge_generation,
        derived,
        attempts,
    }
}

fn read_u64(bytes: &mut &[u8]) -> u64 {
    u64::from_le_bytes(take(bytes, 8).try_into().unwrap())
}

fn take<'a>(bytes: &mut &'a [u8], count: usize) -> &'a [u8] {
    let (value, remainder) = bytes.split_at(count);
    *bytes = remainder;
    value
}
