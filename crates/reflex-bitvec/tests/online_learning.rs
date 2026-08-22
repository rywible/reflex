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
    reason = "one public-path scenario compares training, constrained allocation, and sufficient-budget semantics"
)]
fn promoted_model_changes_later_allocation_but_not_sufficient_budget_semantics() {
    let directory = std::env::temp_dir().join(format!(
        "reflex-online-learning-{}-{}",
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
            training_seeds(),
            10_000,
            BundlePlan::Fresh {
                target: trained.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let trained_snapshot = snapshot(&trained);
    assert!(trained_snapshot.model_generation >= 1);

    std::fs::copy(&trained, &constrained).unwrap();
    std::fs::copy(&trained, &learned_full).unwrap();
    let heldout = heldout_seeds();
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
            u64::try_from(replay_count + 10).unwrap(),
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
            u64::try_from(heldout.len() + 10).unwrap(),
            BundlePlan::Fresh {
                target: bootstrap_constrained.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let learned_constrained = snapshot(&constrained);
    let bootstrap_constrained = snapshot(&bootstrap_constrained);
    let learned_new = &learned_constrained.attempts[trained_snapshot.attempts.len()..];

    let learned_accepted = learned_new
        .iter()
        .filter(|attempt| attempt.accepted)
        .count();
    let bootstrap_accepted = bootstrap_constrained
        .attempts
        .iter()
        .filter(|attempt| attempt.accepted)
        .count();
    assert!(
        learned_accepted > 0
            && learned_new.iter().any(|attempt| !attempt.accepted)
            && bootstrap_accepted == 0,
        "the learned allocator must prefer useful work while retaining protected exploration; learned accepted {learned_accepted}, Bootstrap accepted {bootstrap_accepted}"
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
    .unwrap();
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
    let learned_full = snapshot(&learned_full);
    let bootstrap_full = snapshot(&bootstrap_full);
    let learned_semantics = learned_full.attempts[trained_snapshot.attempts.len()..]
        .iter()
        .filter(|attempt| attempt.accepted)
        .map(|attempt| attempt.canonical.clone())
        .collect::<BTreeSet<_>>();
    let bootstrap_semantics = bootstrap_full
        .attempts
        .iter()
        .filter(|attempt| attempt.accepted)
        .map(|attempt| attempt.canonical.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(learned_semantics, bootstrap_semantics);

    std::fs::remove_dir_all(directory).ok();
}

fn training_seeds() -> Vec<Expression> {
    let refuted = (1..=96)
        .map(|constant| Expression::xor(Expression::input(), Expression::constant(constant)));
    let useful = (97..=192).map(|constant| {
        Expression::xor(
            Expression::xor(Expression::input(), Expression::constant(constant)),
            Expression::constant(0),
        )
    });
    refuted.chain(useful).collect()
}

fn heldout_seeds() -> Vec<Expression> {
    let refuted = (193..=224)
        .map(|constant| Expression::xor(Expression::input(), Expression::constant(constant)));
    let useful = (225..=255).map(|constant| {
        Expression::xor(
            Expression::xor(Expression::input(), Expression::constant(constant)),
            Expression::constant(0),
        )
    });
    refuted.chain(useful).collect()
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
    model_generation: u64,
    attempts: Vec<Attempt>,
}

struct Attempt {
    canonical: Vec<u8>,
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
    let learning_offset = 72 + knowledge_length;
    let learning_length = usize::try_from(u64::from_le_bytes(
        revisions[learning_offset..learning_offset + 8]
            .try_into()
            .unwrap(),
    ))
    .unwrap();
    let learning = &revisions[learning_offset + 8..learning_offset + 8 + learning_length];
    assert_eq!(&learning[..5], b"RFLS\x02");
    let model_generation = u64::from_le_bytes(learning[5..13].try_into().unwrap());
    let attempts = experience
        .attempts
        .into_iter()
        .map(|attempt| Attempt {
            canonical: attempt.canonical_candidate,
            accepted: attempt.verdict == ExperienceVerdictInspection::Accepted,
        })
        .collect();
    Snapshot {
        artifact_count,
        model_generation,
        attempts,
    }
}
