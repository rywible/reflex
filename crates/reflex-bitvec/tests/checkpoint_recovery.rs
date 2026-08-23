use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::process::Command;
use std::time::{Duration, Instant};

use reflex::internal_experiments::inspect_experience_segment;
use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};
use reflex_bundle::{CanonicalBundle, SegmentKind};

const HELPER_ENV: &str = "REFLEX_CHECKPOINT_CRASH_HELPER";
const TARGET_ENV: &str = "REFLEX_CHECKPOINT_CRASH_TARGET";
const PAUSE_ENV: &str = "REFLEX_CHECKPOINT_INITIAL_PAUSE_MS";
const LOOP_ENV: &str = "REFLEX_CHECKPOINT_CRASH_LOOP";
const COHORT_ENV: &str = "REFLEX_CHECKPOINT_COHORT_HELPER";
const SOURCE_ENV: &str = "REFLEX_CHECKPOINT_CRASH_SOURCE";
#[cfg(debug_assertions)]
const FAULT_PHASE_ENV: &str = "REFLEX_INTERNAL_TEST_FAULT_PHASE";
#[cfg(debug_assertions)]
const FAULT_OCCURRENCE_ENV: &str = "REFLEX_INTERNAL_TEST_FAULT_OCCURRENCE";

#[test]
fn observer_boundary_checkpoint_survives_an_abrupt_process_exit() {
    let bundle_path = crash_checkpoint("observer-boundary");

    let outcome = improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Resume {
            source: bundle_path.clone(),
            target: bundle_path.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    assert!(
        outcome.pareto().artifacts()[0].artifact().node_count() == 1
            && outcome.usage().verification_requests == 7,
        "interrupted Resume must retain prior usage and charge Artifact, Experience, Seed, and continued-work Verification"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn interrupted_resume_rejects_resource_drift_without_replacing_the_checkpoint() {
    let bundle_path = crash_checkpoint("resource-drift");
    let checkpoint = std::fs::read(&bundle_path).unwrap();
    let drifted_request = make_request(
        BundlePlan::Resume {
            source: bundle_path.clone(),
            target: bundle_path.clone(),
        },
        nested_seed(),
        Direction::Minimize,
        2,
    );

    let result = improve(BitVecDomain::unary_u8(), drifted_request, |_| {
        ControlFlow::Continue(())
    });

    assert!(
        matches!(result, Err(reflex::SessionError::IncompatibleBundle))
            && std::fs::read(&bundle_path).unwrap() == checkpoint,
        "an interrupted Session is continuation state, so request drift must fail atomically"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn interrupted_bundle_cannot_be_forked_or_replaced() {
    let bundle_path = crash_checkpoint("fork-interrupted");
    let checkpoint = std::fs::read(&bundle_path).unwrap();
    let result = improve(
        BitVecDomain::unary_u8(),
        make_request(
            BundlePlan::Fork {
                source: bundle_path.clone(),
                target: bundle_path.clone(),
            },
            nested_seed(),
            Direction::Minimize,
            1,
        ),
        |_| ControlFlow::Continue(()),
    );

    assert!(
        matches!(result, Err(reflex::SessionError::IncompatibleBundle))
            && std::fs::read(&bundle_path).unwrap() == checkpoint,
        "Fork must not silently abandon an interrupted restart-complete search tail"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn interrupted_resume_rejects_goal_and_seed_scope_drift() {
    let bundle_path = crash_checkpoint("semantic-drift");
    let checkpoint = std::fs::read(&bundle_path).unwrap();
    let resume = || BundlePlan::Resume {
        source: bundle_path.clone(),
        target: bundle_path.clone(),
    };
    let goal_result = improve(
        BitVecDomain::unary_u8(),
        make_request(resume(), nested_seed(), Direction::Maximize, 1),
        |_| ControlFlow::Continue(()),
    );
    let scope_result = improve(
        BitVecDomain::unary_u8(),
        make_request(
            resume(),
            Expression::xor(Expression::input(), Expression::constant(0)),
            Direction::Minimize,
            1,
        ),
        |_| ControlFlow::Continue(()),
    );

    assert!(
        matches!(goal_result, Err(reflex::SessionError::IncompatibleBundle))
            && matches!(scope_result, Err(reflex::SessionError::IncompatibleBundle))
            && std::fs::read(&bundle_path).unwrap() == checkpoint,
        "interrupted continuation cannot silently change its value criteria or Seed Scope"
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
fn interrupted_resume_retains_elapsed_resource_usage() {
    let bundle_path = crash_checkpoint_with_pause("elapsed-usage", 50);

    let outcome = improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Resume {
            source: bundle_path.clone(),
            target: bundle_path.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    assert!(
        outcome.usage().elapsed_time >= Duration::from_millis(40),
        "interrupted Resume must add new elapsed use to the persisted Session usage: {:?}",
        outcome.usage()
    );
    std::fs::remove_file(bundle_path).ok();
}

#[test]
#[cfg(debug_assertions)]
fn refuted_cohort_checkpoint_recovers_the_deferred_tail() {
    let directory = std::env::temp_dir().join(format!(
        "reflex-cohort-recovery-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    std::fs::create_dir(&directory).unwrap();
    let uninterrupted_path = directory.join("uninterrupted.bundle");
    let interrupted_path = directory.join("interrupted.bundle");
    improve(
        BitVecDomain::unary_u8(),
        cohort_request(BundlePlan::Fresh {
            target: uninterrupted_path.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "checkpoint_crash_helper", "--nocapture"])
        .env(HELPER_ENV, "1")
        .env(COHORT_ENV, "1")
        .env(FAULT_PHASE_ENV, "cohort-published")
        .env(FAULT_OCCURRENCE_ENV, "1")
        .env(TARGET_ENV, &interrupted_path)
        .status()
        .unwrap();
    assert!(
        !status.success() && status.code().is_none(),
        "the helper must abort immediately after publishing a refuted cohort"
    );
    improve(
        BitVecDomain::unary_u8(),
        cohort_request(BundlePlan::Resume {
            source: interrupted_path.clone(),
            target: interrupted_path.clone(),
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();

    assert_eq!(
        experience_identities(&interrupted_path),
        experience_identities(&uninterrupted_path),
        "Resume must decide the exact deferred Candidate tail retained by the uninterrupted run"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
#[cfg(debug_assertions)]
fn interrupted_recovery_preserves_exact_search_frontier_membership() {
    let directory = std::env::temp_dir().join(format!(
        "reflex-frontier-recovery-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    std::fs::create_dir(&directory).unwrap();
    let original = directory.join("original.bundle");
    let narrowed = directory.join("narrowed.bundle");
    let recovered = directory.join("recovered.bundle");
    let initial_status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "checkpoint_crash_helper", "--nocapture"])
        .env(HELPER_ENV, "1")
        .env(COHORT_ENV, "1")
        .env(FAULT_PHASE_ENV, "atomic-rename")
        .env(FAULT_OCCURRENCE_ENV, "1")
        .env(TARGET_ENV, &original)
        .status()
        .unwrap();
    assert!(!initial_status.success() && initial_status.code().is_none());
    let expected_frontier_count = narrow_initial_frontier(&original, &narrowed);

    let recovery_status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "checkpoint_crash_helper", "--nocapture"])
        .env(HELPER_ENV, "1")
        .env(COHORT_ENV, "1")
        .env(SOURCE_ENV, &narrowed)
        .env(FAULT_PHASE_ENV, "atomic-rename")
        .env(FAULT_OCCURRENCE_ENV, "1")
        .env(TARGET_ENV, &recovered)
        .status()
        .unwrap();
    assert!(!recovery_status.success() && recovery_status.code().is_none());
    assert_eq!(
        recovery_frontier_count(&recovered),
        expected_frontier_count,
        "interrupted recovery must not schedule retained Artifacts absent from the sealed Search Frontier"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn randomized_process_kills_always_leave_restart_complete_state() {
    let directory = std::env::temp_dir().join(format!(
        "reflex-randomized-kills-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    std::fs::create_dir(&directory).unwrap();
    let mut random = 0x4d59_5df4_d0f3_3173_u64;

    for generation in 0..16 {
        let target = directory.join(format!("generation-{generation}.bundle"));
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "checkpoint_crash_helper", "--nocapture"])
            .env(HELPER_ENV, "1")
            .env(LOOP_ENV, "1")
            .env(TARGET_ENV, &target)
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !target.is_file() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "checkpoint helper exited before publishing a generation"
            );
            assert!(
                Instant::now() < deadline,
                "checkpoint publication timed out"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        std::thread::sleep(Duration::from_micros(random % 2_000));
        child.kill().unwrap();
        assert!(!child.wait().unwrap().success());

        let outcome = improve(
            BitVecDomain::unary_u8(),
            request(BundlePlan::Resume {
                source: target.clone(),
                target,
            }),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
        assert_eq!(outcome.pareto().artifacts()[0].artifact().node_count(), 1);
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
#[cfg(debug_assertions)]
fn deterministic_faults_across_randomized_mutation_phases_recover_exactly() {
    let directory = std::env::temp_dir().join(format!(
        "reflex-phase-faults-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    std::fs::create_dir(&directory).unwrap();
    let uninterrupted_target = directory.join("uninterrupted.bundle");
    let uninterrupted = improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Fresh {
            target: uninterrupted_target,
        }),
        |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let expected_key = uninterrupted.pareto().artifacts()[0].key();
    let mut phases = [
        ("candidate-created", 1_u64),
        ("verdict-recorded", 1),
        ("experience-appended", 1),
        ("measurement-completed", 1),
        ("admission-completed", 1),
        ("revision-sealed", 2),
        ("manifest-sealed", 2),
        ("atomic-rename", 2),
        ("pareto-published", 1),
    ];
    let mut random = 0xa076_1d64_78bd_642f_u64;
    for index in (1..phases.len()).rev() {
        random ^= random << 13;
        random ^= random >> 7;
        random ^= random << 17;
        let selected = random % u64::try_from(index + 1).unwrap();
        phases.swap(index, usize::try_from(selected).unwrap());
    }

    for (index, (phase, occurrence)) in phases.into_iter().enumerate() {
        let target = directory.join(format!("phase-{index}.bundle"));
        let status = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "checkpoint_crash_helper", "--nocapture"])
            .env(HELPER_ENV, "1")
            .env(FAULT_PHASE_ENV, phase)
            .env(FAULT_OCCURRENCE_ENV, occurrence.to_string())
            .env(TARGET_ENV, &target)
            .status()
            .unwrap();
        assert!(
            !status.success() && status.code().is_none(),
            "fault phase {phase} must abort the helper rather than merely exit unsuccessfully"
        );
        assert!(
            target.is_file(),
            "fault phase {phase} must preserve the preceding sealed generation"
        );

        let recovered = improve(
            BitVecDomain::unary_u8(),
            request(BundlePlan::Resume {
                source: target.clone(),
                target: target.clone(),
            }),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
        assert_eq!(
            recovered.pareto().artifacts()[0].key(),
            expected_key,
            "fault phase {phase} must converge to the uninterrupted semantic result"
        );
        let replayed = improve(
            BitVecDomain::unary_u8(),
            request(BundlePlan::Resume {
                source: target.clone(),
                target,
            }),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
        assert_eq!(
            replayed.pareto().artifacts()[0].key(),
            expected_key,
            "fault phase {phase} recovery must leave replayable, deduplicated Experience"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn checkpoint_crash_helper() {
    if std::env::var_os(HELPER_ENV).is_none() {
        return;
    }
    let target = std::path::PathBuf::from(std::env::var_os(TARGET_ENV).unwrap());
    if std::env::var_os(LOOP_ENV).is_some() {
        loop {
            improve(
                BitVecDomain::unary_u8(),
                request(BundlePlan::Fresh {
                    target: target.clone(),
                }),
                |_| ControlFlow::Continue(()),
            )
            .unwrap();
        }
    }
    #[cfg(debug_assertions)]
    if std::env::var_os(FAULT_PHASE_ENV).is_some() {
        if std::env::var_os(COHORT_ENV).is_some() {
            let bundle = std::env::var_os(SOURCE_ENV).map_or(
                BundlePlan::Fresh {
                    target: target.clone(),
                },
                |source| BundlePlan::Resume {
                    source: source.into(),
                    target,
                },
            );
            improve(BitVecDomain::unary_u8(), cohort_request(bundle), |_| {
                ControlFlow::Continue(())
            })
            .expect("the configured cohort fault must terminate this process");
            panic!("configured private cohort fault was not reached");
        }
        let bundle = std::env::var_os(SOURCE_ENV).map_or(
            BundlePlan::Fresh {
                target: target.clone(),
            },
            |source| BundlePlan::Resume {
                source: source.into(),
                target,
            },
        );
        improve(BitVecDomain::unary_u8(), request(bundle), |_| {
            ControlFlow::Continue(())
        })
        .expect("the configured private fault phase must terminate this process");
        panic!("configured private fault phase was not reached");
    }
    let _ = improve(
        BitVecDomain::unary_u8(),
        request(BundlePlan::Fresh { target }),
        |update| {
            if update
                .added()
                .iter()
                .any(|artifact| artifact.artifact().node_count() == 4)
                && let Ok(milliseconds) = std::env::var(PAUSE_ENV)
                && let Ok(milliseconds) = milliseconds.parse()
            {
                std::thread::sleep(Duration::from_millis(milliseconds));
            }
            if update
                .added()
                .iter()
                .any(|artifact| artifact.artifact().node_count() == 3)
            {
                std::process::abort();
            }
            ControlFlow::Continue(())
        },
    );
}

fn cohort_request(bundle: BundlePlan) -> ImprovementRequest<BitVecDomain> {
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    let seeds = (225..=232).map(multi_choice_seed);
    ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::new(NonEmpty::try_from_iter(seeds).unwrap()),
        ResourceEnvelope::new(
            NonZeroUsize::new(2).unwrap(),
            NonZeroU64::new(64 * 1024 * 1024).unwrap(),
            NonZeroU64::new(64 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(10)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(10)).unwrap(),
            NonZeroU64::new(1_000).unwrap(),
        ),
        bundle,
    )
    .unwrap()
}

fn multi_choice_seed(constant: u8) -> Expression {
    Expression::xor(
        Expression::xor(Expression::input(), Expression::constant(constant)),
        Expression::constant(1),
    )
}

fn experience_identities(path: &std::path::Path) -> Vec<([u8; 32], [u8; 32], u8)> {
    let bundle = CanonicalBundle::decode(&std::fs::read(path).unwrap(), 64 * 1024 * 1024).unwrap();
    let experience = inspect_experience_segment(bundle.segment(SegmentKind::Experience)).unwrap();
    experience
        .attempts
        .into_iter()
        .map(|attempt| {
            let verdict = match attempt.verdict {
                reflex::internal_experiments::ExperienceVerdictInspection::Accepted => 0,
                reflex::internal_experiments::ExperienceVerdictInspection::Refuted => 1,
                reflex::internal_experiments::ExperienceVerdictInspection::Unknown => 2,
            };
            (attempt.candidate_key, attempt.claim_digest, verdict)
        })
        .collect()
}

#[cfg(debug_assertions)]
fn narrow_initial_frontier(source: &std::path::Path, target: &std::path::Path) -> u64 {
    let bundle =
        CanonicalBundle::decode(&std::fs::read(source).unwrap(), 64 * 1024 * 1024).unwrap();
    let recovery = bundle.segment(SegmentKind::Recovery);
    let pareto_count =
        usize::try_from(u64::from_le_bytes(recovery[..8].try_into().unwrap())).unwrap();
    let frontier_count_offset = 8 + pareto_count * 32;
    let frontier_count = usize::try_from(u64::from_le_bytes(
        recovery[frontier_count_offset..frontier_count_offset + 8]
            .try_into()
            .unwrap(),
    ))
    .unwrap();
    assert!(frontier_count > 1);
    let frontier_start = frontier_count_offset + 8;
    let frontier_end = frontier_start + frontier_count * 32;
    let removed_key = &recovery[frontier_end - 32..frontier_end];
    let deferred_count =
        u64::from_le_bytes(recovery[frontier_end..frontier_end + 8].try_into().unwrap());
    assert_eq!(
        deferred_count, 0,
        "the first checkpoint has no deferred work"
    );
    let pending_count_offset = frontier_end + 8;
    let pending_count = usize::try_from(u64::from_le_bytes(
        recovery[pending_count_offset..pending_count_offset + 8]
            .try_into()
            .unwrap(),
    ))
    .unwrap();
    let mut pending_input = &recovery[pending_count_offset + 8..];
    let mut pending_records = Vec::with_capacity(pending_count);
    for _ in 0..pending_count {
        let offset_count = usize::try_from(u64::from_le_bytes(
            pending_input[32..40].try_into().unwrap(),
        ))
        .unwrap();
        let record_len = 32 + 8 + offset_count * 8 + 1;
        let (record, rest) = pending_input.split_at(record_len);
        pending_records.push(record);
        pending_input = rest;
    }
    assert!(pending_input.is_empty());

    let mut narrowed = Vec::with_capacity(recovery.len() - 64);
    narrowed.extend_from_slice(&recovery[..frontier_count_offset]);
    narrowed.extend_from_slice(&(frontier_count as u64 - 1).to_le_bytes());
    narrowed.extend_from_slice(&recovery[frontier_start..frontier_end - 32]);
    narrowed.extend_from_slice(&0_u64.to_le_bytes());
    let retained_pending = pending_records
        .into_iter()
        .filter(|record| &record[..32] != removed_key)
        .collect::<Vec<_>>();
    narrowed.extend_from_slice(&(retained_pending.len() as u64).to_le_bytes());
    for record in retained_pending {
        narrowed.extend_from_slice(record);
    }
    let encoded = CanonicalBundle::new(
        bundle.identity().to_vec(),
        bundle.segment(SegmentKind::Session).to_vec(),
        bundle.segment(SegmentKind::Revisions).to_vec(),
        bundle.segment(SegmentKind::Artifacts).to_vec(),
        bundle.segment(SegmentKind::Experience).to_vec(),
        narrowed,
    )
    .encode();
    std::fs::write(target, encoded).unwrap();
    frontier_count as u64 - 1
}

fn recovery_frontier_count(path: &std::path::Path) -> u64 {
    let bundle = CanonicalBundle::decode(&std::fs::read(path).unwrap(), 64 * 1024 * 1024).unwrap();
    let recovery = bundle.segment(SegmentKind::Recovery);
    let pareto_count = u64::from_le_bytes(recovery[..8].try_into().unwrap());
    let frontier_offset = 8 + usize::try_from(pareto_count).unwrap() * 32;
    u64::from_le_bytes(
        recovery[frontier_offset..frontier_offset + 8]
            .try_into()
            .unwrap(),
    )
}

fn crash_checkpoint(label: &str) -> std::path::PathBuf {
    crash_checkpoint_process(label, None)
}

fn crash_checkpoint_with_pause(label: &str, milliseconds: u64) -> std::path::PathBuf {
    crash_checkpoint_process(label, Some(milliseconds))
}

fn crash_checkpoint_process(label: &str, pause_ms: Option<u64>) -> std::path::PathBuf {
    let bundle_path = std::env::temp_dir().join(format!(
        "reflex-checkpoint-crash-{label}-{}-{}.bundle",
        std::process::id(),
        std::thread::current().name().unwrap_or("unnamed")
    ));
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", "checkpoint_crash_helper", "--nocapture"])
        .env(HELPER_ENV, "1")
        .env(TARGET_ENV, &bundle_path);
    if let Some(milliseconds) = pause_ms {
        command.env(PAUSE_ENV, milliseconds.to_string());
    }
    let status = command.status().unwrap();
    assert!(!status.success(), "the helper must terminate abruptly");
    assert!(
        bundle_path.is_file(),
        "a sealed generation must exist before the observer sees its Pareto transition"
    );
    bundle_path
}

fn request(bundle: BundlePlan) -> ImprovementRequest<BitVecDomain> {
    make_request(bundle, nested_seed(), Direction::Minimize, 1)
}

fn nested_seed() -> Expression {
    Expression::xor(
        Expression::xor(Expression::input(), Expression::constant(0)),
        Expression::constant(0),
    )
}

fn make_request(
    bundle: BundlePlan,
    seed: Expression,
    direction: Direction,
    worker_threads: usize,
) -> ImprovementRequest<BitVecDomain> {
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, direction));
    let preference =
        Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), []).unwrap();
    ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None).unwrap()),
        SeedScope::one(seed),
        ResourceEnvelope::new(
            NonZeroUsize::new(worker_threads).unwrap(),
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
