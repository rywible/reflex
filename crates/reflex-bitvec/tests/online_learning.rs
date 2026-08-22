use std::collections::BTreeSet;
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::Path;
use std::time::Duration;

use reflex::internal_experiments::{
    CandidateAllocationQueueInspection, CandidateFateInspection, CandidateFateOutcomeInspection,
    ExperienceVerdictInspection, inspect_experience_segment,
};
use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};
use reflex_bundle::{CanonicalBundle, SegmentKind};

#[test]
fn verification_is_cohorted_before_the_envelope_is_exhausted() {
    let directory = std::env::temp_dir().join(format!(
        "reflex-verification-cohort-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let bundle = directory.join("cohorted.bundle");

    improve(
        BitVecDomain::unary_u8(),
        request(
            vec![
                multi_choice_seed(225),
                multi_choice_seed(226),
                multi_choice_seed(227),
                multi_choice_seed(228),
            ],
            50,
            BundlePlan::Fresh {
                target: bundle.clone(),
            },
        ),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    let snapshot = snapshot(&bundle);
    let verified = snapshot
        .candidate_fates
        .iter()
        .filter_map(|fate| match fate.outcome {
            CandidateFateOutcomeInspection::Verified { .. } => {
                Some((fate.epoch, fate.verification_batch_size.unwrap()))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        verified.len() > 8,
        "fixture must exercise more than one cohort"
    );
    assert!(
        verified.iter().all(|(_, batch_size)| *batch_size <= 8),
        "one correctness-claim pair should be reconsidered after at most eight checks"
    );
    assert!(
        verified
            .iter()
            .map(|(epoch, _)| *epoch)
            .collect::<BTreeSet<_>>()
            .len()
            >= 2,
        "the envelope must contain more than one admission-feedback step"
    );

    std::fs::remove_dir_all(directory).ok();
}

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
    assert!(
        trained_snapshot.candidate_fates.iter().any(|fate| {
            matches!(
                fate.outcome,
                CandidateFateOutcomeInspection::Verified {
                    allocation_queue: CandidateAllocationQueueInspection::ProtectedOrigin,
                    bootstrap_rank: Some(_),
                    learned_rank: None,
                    ..
                }
            )
        }),
        "protected Bootstrap work must retain its counterfactual Bootstrap rank"
    );
    let training_claims = trained_snapshot
        .attempts
        .iter()
        .map(|attempt| attempt.claim_digest)
        .collect::<BTreeSet<_>>()
        .len();
    assert!(
        trained_snapshot.model_generation >= 1,
        "training retained {} attempts over {training_claims} claims ({} accepted; queues: origin {} accepted {}, derived {}, learned {}, Bootstrap {} accepted {}) across {} Candidate Fates",
        trained_snapshot.attempts.len(),
        trained_snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.accepted)
            .count(),
        trained_snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.allocation_queue
                == CandidateAllocationQueueInspection::ProtectedOrigin)
            .count(),
        trained_snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.accepted
                && attempt.allocation_queue == CandidateAllocationQueueInspection::ProtectedOrigin)
            .count(),
        trained_snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.allocation_queue
                == CandidateAllocationQueueInspection::ProtectedDerived)
            .count(),
        trained_snapshot
            .attempts
            .iter()
            .filter(
                |attempt| attempt.allocation_queue == CandidateAllocationQueueInspection::Learned
            )
            .count(),
        trained_snapshot
            .attempts
            .iter()
            .filter(
                |attempt| attempt.allocation_queue == CandidateAllocationQueueInspection::Bootstrap
            )
            .count(),
        trained_snapshot
            .attempts
            .iter()
            .filter(|attempt| attempt.accepted
                && attempt.allocation_queue == CandidateAllocationQueueInspection::Bootstrap)
            .count(),
        trained_snapshot.candidate_fates.len(),
    );

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
            BundlePlan::Fork {
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
    let learned_fates =
        &learned_constrained.candidate_fates[trained_snapshot.candidate_fates.len()..];

    let learned_accepted = learned_new
        .iter()
        .filter(|attempt| attempt.accepted)
        .count();
    let learned_queue_accepted = learned_new
        .iter()
        .filter(|attempt| {
            attempt.accepted
                && attempt.allocation_queue == CandidateAllocationQueueInspection::Learned
        })
        .count();
    let bootstrap_accepted = bootstrap_constrained
        .attempts
        .iter()
        .filter(|attempt| attempt.accepted)
        .count();
    assert!(
        learned_accepted > 0
            && learned_queue_accepted > 0
            && learned_new.iter().any(|attempt| !attempt.accepted)
            && learned_accepted > bootstrap_accepted,
        "the learned allocator must beat Bootstrap while retaining protected exploration; learned accepted {learned_accepted}, Bootstrap accepted {bootstrap_accepted}"
    );
    assert_eq!(
        learned_fates
            .iter()
            .filter(|fate| {
                matches!(
                    fate.outcome,
                    CandidateFateOutcomeInspection::Verified { .. }
                )
            })
            .count(),
        learned_new.len(),
        "every Verification attempt must have exactly one Candidate Fate"
    );
    assert!(
        learned_fates.iter().any(|fate| {
            matches!(
                fate.outcome,
                CandidateFateOutcomeInspection::PolicyDeferred { .. }
            )
        }),
        "constrained allocation must retain the causal denominator that did not reach Verification"
    );

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
    let learned_full_fates =
        &learned_full.candidate_fates[trained_snapshot.candidate_fates.len()..];
    assert!(
        learned_full_fates.iter().any(|fate| {
            matches!(
                fate.outcome,
                CandidateFateOutcomeInspection::Verified {
                    allocation_queue: CandidateAllocationQueueInspection::ProtectedOrigin,
                    bootstrap_rank: Some(_),
                    learned_rank: Some(_),
                    ..
                }
            )
        }),
        "protected learned work must retain both counterfactual policy ranks"
    );
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
    (1..=192).map(multi_choice_seed).collect()
}

fn heldout_seeds() -> Vec<Expression> {
    (193..=224).map(multi_choice_seed).collect()
}

fn multi_choice_seed(constant: u8) -> Expression {
    Expression::xor(
        Expression::xor(Expression::input(), Expression::constant(constant)),
        Expression::constant(1),
    )
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
    candidate_fates: Vec<CandidateFateInspection>,
}

struct Attempt {
    claim_digest: [u8; 32],
    canonical: Vec<u8>,
    accepted: bool,
    allocation_queue: CandidateAllocationQueueInspection,
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
            claim_digest: attempt.claim_digest,
            canonical: attempt.canonical_candidate,
            accepted: attempt.verdict == ExperienceVerdictInspection::Accepted,
            allocation_queue: attempt.allocation_queue,
        })
        .collect();
    Snapshot {
        artifact_count,
        model_generation,
        attempts,
        candidate_fates: experience.candidate_fates,
    }
}
