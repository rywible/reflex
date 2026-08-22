use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use std::time::Instant;

use reflex_lean::ast::LeanName;
use reflex_lean::catalog::LeanCatalog;
use reflex_lean::temporal::{
    POTENTIAL_HEADS, PotentialHead, RelationshipKind, TemporalPair, TemporalSnapshot,
    certify_relationship, consolidate_certificates, migrate_theorems,
};
use reflex_lean::worker::{IndexedTheorem, LeanWorker, LeanWorkerConfig};
use reflex_lean::{LeanSnapshotPin, temporal::RelationshipCandidate};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::harness::{
    AnyError, HostEnvironment, capture_child_bounded, duration_ns, environment, hash_file,
    hash_json, parse_flag_values, peak_process_resident_bytes, require_absent, require_clean,
    require_release,
};

const SCHEMA: &str = "reflex-lean-temporal-audit-result-v1";
const FREEZE_SCHEMA: &str = "reflex-lean-temporal-audit-freeze-v1";
const LOCK_SCHEMA: &str = "reflex-lean-temporal-audit-lock-v1";
const REPLAY_PER_HEAD: usize = 16;
const RELATIONSHIP_LIMIT: usize = 64;
const WALL_LIMIT: Duration = Duration::from_hours(24);
const RESIDENT_LIMIT: u64 = 48 * 1024 * 1024 * 1024;
const SUPERVISOR_CAPABILITY: &str = "reflex-lean-temporal-audit-supervisor-v1";
const CPU_CHECKPOINT_SECONDS: [u64; 4] = [3_600, 14_400, 57_600, 230_400];
const IN_PROCESS_LANES: usize = 6;

struct Arguments {
    manifest: PathBuf,
    lock: PathBuf,
    lake: PathBuf,
    december_root: PathBuf,
    december_catalog: PathBuf,
    audit_root: PathBuf,
    audit_catalog: PathBuf,
    output: PathBuf,
    critique_packet: PathBuf,
}

struct FinalizeArguments {
    mechanical_report: PathBuf,
    critique_packet: PathBuf,
    assessment: PathBuf,
    output: PathBuf,
}

#[derive(Clone, Deserialize)]
struct FrozenArtifact {
    declaration: String,
    module: String,
    semantic_family: String,
}

#[derive(Deserialize)]
struct Ranking {
    treatment: String,
    training_cpu_ns: u64,
    #[serde(rename = "ranking_cpu_ns")]
    selection_cpu_ns: [u64; POTENTIAL_HEADS],
    heads: [Vec<FrozenArtifact>; POTENTIAL_HEADS],
}

#[derive(Deserialize)]
struct Manifest {
    schema: String,
    protocol_sha256: String,
    catalog_sha256: [String; 3],
    audit_artifact_set_sha256: String,
    rankings: Vec<Ranking>,
    content_sha256: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct Lock {
    schema: String,
    status: String,
    freeze_manifest_file_sha256: String,
    freeze_manifest_content_sha256: String,
    protocol_sha256: String,
    audit_artifact_set_sha256: String,
    audit_boundary: String,
    mathlib_commit: String,
    mathlib_commit_timestamp: String,
    boundary_evidence_url: String,
    boundary_evidence_sha256: String,
    boundary_evidence_response: String,
    lean_toolchain: String,
    lean_toolchain_alias: String,
    lean_version: String,
    lean_commit: String,
    checkout_access: String,
    host: LockedHostEnvironment,
    content_sha256: String,
}

#[derive(Clone, Deserialize, Serialize)]
struct LockedHostEnvironment {
    git_revision: String,
    git_dirty: bool,
    rustc: String,
    cargo: String,
    build_profile: String,
    rustflags: String,
    target_features: String,
    target_arch: String,
    target_os: String,
    cpu_description: String,
    available_parallelism: usize,
}

#[derive(Serialize)]
struct HeadOutcome {
    head: &'static str,
    selected: usize,
    missing: usize,
    statistical_units: usize,
    raw_mean: f64,
    directional_utility: f64,
    evaluation_cpu_ns: u64,
    exhaustion_cpu_ns: u64,
    anytime: Vec<AnytimeHeadOutcome>,
}

#[derive(Serialize)]
struct AnytimeHeadOutcome {
    cpu_seconds: u64,
    cpu_used_ns: u64,
    artifacts_evaluated: usize,
    statistical_units: usize,
    directional_utility: f64,
    exhausted: bool,
}

#[derive(Serialize)]
struct TreatmentOutcome {
    treatment: String,
    heads: Vec<HeadOutcome>,
}

#[derive(Serialize)]
struct ParetoOutcome {
    head: &'static str,
    full: f64,
    virtual_best_baseline: f64,
    difference: f64,
    simultaneous_lower_bound_99: f64,
    baseline: String,
}

#[derive(Serialize)]
struct AblationOutcome {
    treatment: String,
    full_wins: usize,
    ties: usize,
    full_losses: usize,
    passed: bool,
}

#[derive(Serialize)]
struct TimeToUtilityOutcome {
    head: &'static str,
    common_target: f64,
    full_cpu_ns: Option<u64>,
    virtual_best_baseline_cpu_ns: Option<u64>,
    baseline: String,
    speedup: Option<f64>,
}

#[derive(Serialize)]
struct TimeGate {
    heads: Vec<TimeToUtilityOutcome>,
    geometric_mean_speedup: Option<f64>,
    point_gate_passed: bool,
    interval_gate_evaluated: bool,
    simultaneous_lower_bound_99: Option<f64>,
    passed: bool,
}

#[derive(Clone, Serialize)]
struct ReplayRecord {
    treatment: String,
    head: &'static str,
    rank: usize,
    declaration: String,
    module: String,
    semantic_family: String,
    accepted: bool,
    diagnostic: String,
    kernel_evidence_sha256: String,
    preparation_cpu_upper_bound_ns: u64,
    cumulative_cpu_upper_bound_ns: u64,
    proof_nodes: usize,
    proof_depth: usize,
    proof_encoded_bytes: usize,
    source_dependencies: usize,
    source_axioms: usize,
    replayed_dependencies: Option<usize>,
    replayed_axioms: Option<usize>,
    elegance_preserved: bool,
}

#[derive(Serialize)]
struct ReplaySummary {
    attempted: usize,
    accepted: usize,
    rejected: usize,
    cold_recovery_attempted: usize,
    cold_recovery_decisions_matched: usize,
    cold_recovery_evidence_matched: usize,
    records: Vec<ReplayRecord>,
}

#[derive(Serialize)]
struct RelationshipSummary {
    attempted: usize,
    certified: usize,
    rejected_or_unavailable: usize,
    exact: usize,
    definitional: usize,
    specialization: usize,
    derivation: usize,
    family_collapse: usize,
    corpus_compression: usize,
    proof_nodes_removed: usize,
    records: Vec<RelationshipRecord>,
}

#[derive(Serialize)]
struct RelationshipRecord {
    earlier_declaration: String,
    later_declaration: String,
    expected_kind: RelationshipKind,
    certified_kind: Option<RelationshipKind>,
    kernel_accepted: bool,
    kernel_dependencies: Vec<String>,
    kernel_axioms: Vec<String>,
    kernel_diagnostic: String,
    kernel_evidence_sha256: String,
    proof_nodes_removed: usize,
}

struct RelationshipEvaluation {
    summary: RelationshipSummary,
    critique: HashMap<String, Vec<CritiqueRelationship>>,
    kernel_calls: usize,
    kernel_cpu_upper_bound_ns: u64,
}

#[derive(Serialize)]
struct Economics {
    wall_ns: u64,
    controller_cpu_ns: u64,
    controller_peak_resident_bytes: u64,
    combined_resident_upper_bound_bytes: u64,
    catalog_durable_bytes: [u64; 2],
    result_durable_bytes: usize,
    kernel_calls: usize,
    kernel_cpu_upper_bound_ns: u64,
    verifier_lifetime_cpu_upper_bound_ns: u64,
}

#[derive(Serialize)]
struct AnytimeCheckpoint {
    cpu_seconds: u64,
    all_artifacts_exhausted: bool,
    maximum_exhaustion_cpu_ns: u64,
    outcome_reference: &'static str,
}

#[derive(Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the sealed report retains independent scientific gates rather than collapsing state"
)]
struct Report {
    schema: &'static str,
    status: &'static str,
    mechanical_results_sealed: bool,
    mechanical_gate_passed: bool,
    confirmed: bool,
    protocol_sha256: String,
    freeze_manifest_file_sha256: String,
    freeze_manifest_content_sha256: String,
    audit_lock_content_sha256: String,
    execution_receipt_file_sha256: String,
    audit_artifact_set_sha256: String,
    december_catalog_sha256: String,
    audit_catalog_sha256: String,
    audit_mathlib_commit: String,
    audit_lean_commit: String,
    audit_examples: usize,
    audit_new_declarations: usize,
    outcomes: Vec<TreatmentOutcome>,
    pareto: Vec<ParetoOutcome>,
    pareto_dominates: bool,
    causal_ablations: Vec<AblationOutcome>,
    causal_ablations_passed: bool,
    anytime: Vec<AnytimeCheckpoint>,
    time_to_utility: TimeGate,
    replay: ReplaySummary,
    relationships: RelationshipSummary,
    no_regression_passed: bool,
    wall_limit_passed: bool,
    economics: Economics,
    host: HostEnvironment,
    protocol_deviations: Vec<String>,
    content_sha256: String,
}

#[derive(Serialize)]
struct CritiqueItem {
    blinded_id: String,
    declaration: String,
    module: String,
    semantic_family: String,
    proof_nodes: usize,
    proof_depth: usize,
    proof_encoded_bytes: usize,
    dependencies: usize,
    allowed_axioms: usize,
    target_profile: [f32; POTENTIAL_HEADS],
    certified_future_relationships: Vec<CritiqueRelationship>,
}

#[derive(Clone, Serialize)]
struct CritiqueRelationship {
    kind: RelationshipKind,
    later_declaration: String,
    proof_nodes_removed: usize,
}

#[derive(Serialize)]
struct CritiquePacket {
    schema: &'static str,
    status: &'static str,
    mechanical_report_file_sha256: String,
    instructions: &'static str,
    items: Vec<CritiqueItem>,
    content_sha256: String,
}

#[derive(Serialize)]
struct ExecutionReceipt<'a> {
    schema: &'static str,
    status: &'static str,
    protocol_sha256: &'a str,
    freeze_manifest_file_sha256: &'a str,
    audit_lock_content_sha256: &'a str,
    git_revision: &'a str,
}

#[derive(Serialize)]
struct FailureReport<'a> {
    schema: &'static str,
    status: &'static str,
    error: &'a str,
    timed_out: bool,
    resident_limit_exceeded: bool,
    child_exit_code: Option<i32>,
    child_stdout: &'a str,
    child_stderr: &'a str,
    process_tree_cpu_ns: u64,
    peak_process_tree_resident_bytes: u64,
}

#[derive(Deserialize)]
struct FailureIdentity {
    schema: String,
    status: String,
}

#[derive(Deserialize)]
struct MechanicalIdentity {
    schema: String,
    mechanical_gate_passed: bool,
    protocol_sha256: String,
    freeze_manifest_file_sha256: String,
    audit_lock_content_sha256: String,
    execution_receipt_file_sha256: String,
    content_sha256: String,
}

#[derive(Deserialize)]
struct ReceiptIdentity {
    schema: String,
    protocol_sha256: String,
    freeze_manifest_file_sha256: String,
    audit_lock_content_sha256: String,
}

#[derive(Deserialize)]
struct CritiquePacketIdentity {
    schema: String,
    mechanical_report_file_sha256: String,
    items: Vec<CritiqueItemIdentity>,
    content_sha256: String,
}

#[derive(Deserialize)]
struct CritiqueItemIdentity {
    blinded_id: String,
}

#[derive(Deserialize, Serialize)]
struct CritiqueAssessment {
    schema: String,
    reviewer: String,
    critique_packet_file_sha256: String,
    items: Vec<AssessedCritiqueItem>,
    overall_assessment: String,
    protocol_deviations: Vec<String>,
}

#[derive(Deserialize, Serialize)]
struct AssessedCritiqueItem {
    blinded_id: String,
    generality: String,
    proof_collapse: String,
    local_elegance: String,
    family_elegance: String,
    corpus_compression: String,
    human_value: String,
    notes: String,
}

#[derive(Serialize)]
struct FinalReport {
    schema: &'static str,
    status: &'static str,
    critique_complete: bool,
    mechanical_gate_passed: bool,
    scientifically_confirmed: bool,
    mechanical_report_file_sha256: String,
    mechanical_report_content_sha256: String,
    critique_packet_file_sha256: String,
    critique_packet_content_sha256: String,
    critique_assessment_file_sha256: String,
    critique: CritiqueAssessment,
    next_domain: &'static str,
    next_domain_role: &'static str,
    content_sha256: String,
}

#[derive(Serialize)]
struct FailedExecutionFinalReport {
    schema: &'static str,
    status: &'static str,
    critique_complete: bool,
    mechanical_gate_passed: bool,
    scientifically_confirmed: bool,
    execution_receipt_file_sha256: String,
    failure_report_file_sha256: String,
    failure_status: String,
    next_domain: &'static str,
    next_domain_role: &'static str,
    content_sha256: String,
}

pub fn confirm(arguments: &[String]) -> Result<(), AnyError> {
    require_release("lean-temporal-audit-confirm")?;
    let host = environment()?;
    require_clean(&host, SCHEMA)?;
    if host.available_parallelism < IN_PROCESS_LANES + 2 {
        return Err("Lean Temporal Audit host cannot supply its registered eight lanes".into());
    }
    parse(arguments)?;
    require_absent(
        &execution_receipt_path()?,
        "Lean Temporal Audit execution receipt",
    )?;
    let executable = std::env::current_exe()?;
    let child_arguments = std::iter::once(OsString::from("lean-temporal-audit-confirm-child"))
        .chain(arguments.iter().map(OsString::from))
        .collect::<Vec<_>>();
    let evidence_prefix = std::env::temp_dir().join(format!(
        "reflex-lean-temporal-audit-supervisor-{}",
        std::process::id()
    ));
    let capture = capture_child_bounded(
        &executable,
        &child_arguments,
        &evidence_prefix,
        WALL_LIMIT,
        RESIDENT_LIMIT,
        &[(
            OsString::from("REFLEX_LEAN_AUDIT_SUPERVISOR_CAPABILITY"),
            OsString::from(SUPERVISOR_CAPABILITY),
        )],
    )?;
    if capture.status.success() && !capture.timed_out && !capture.resident_limit_exceeded {
        print!("{}", capture.stdout);
        return Ok(());
    }
    let error = if capture.timed_out {
        "Lean Temporal Audit exceeded its 24-hour wall limit"
    } else if capture.resident_limit_exceeded {
        "Lean Temporal Audit exceeded its 48-GiB combined resident limit"
    } else {
        "Lean Temporal Audit child failed"
    };
    retain_failure_if_exposed(&FailureReport {
        schema: "reflex-lean-temporal-audit-failure-v1",
        status: "sealed-failure; rerun-forbidden",
        error,
        timed_out: capture.timed_out,
        resident_limit_exceeded: capture.resident_limit_exceeded,
        child_exit_code: capture.status.code(),
        child_stdout: &capture.stdout,
        child_stderr: &capture.stderr,
        process_tree_cpu_ns: capture.process_tree_cpu_ns,
        peak_process_tree_resident_bytes: capture.peak_process_tree_resident_bytes,
    })?;
    Err(format!("{error}: {}", capture.stderr.trim()).into())
}

pub fn confirm_child(arguments: &[String]) -> Result<(), AnyError> {
    if std::env::var("REFLEX_LEAN_AUDIT_SUPERVISOR_CAPABILITY").as_deref()
        != Ok(SUPERVISOR_CAPABILITY)
    {
        return Err("Lean Temporal Audit child execution requires its bounded supervisor".into());
    }
    require_supervising_parent()?;
    confirm_once(arguments)
}

pub fn finalize(arguments: &[String]) -> Result<(), AnyError> {
    require_release("lean-temporal-audit-finalize")?;
    let arguments = parse_finalize(arguments)?;
    require_absent(&arguments.output, "Lean Temporal Audit final report")?;
    require_registered_finalize_paths(&arguments)?;
    let receipt_path = execution_receipt_path()?;
    if !receipt_path.exists() {
        return Err("Lean Temporal Audit cannot finalize before its one-shot execution".into());
    }
    let failure_path = failure_report_path()?;
    if failure_path.exists() {
        return finalize_failed_execution(&receipt_path, &failure_path, &arguments.output);
    }
    let receipt: ReceiptIdentity = serde_json::from_slice(&std::fs::read(&receipt_path)?)?;
    let mechanical: MechanicalIdentity =
        serde_json::from_slice(&std::fs::read(&arguments.mechanical_report)?)?;
    let packet: CritiquePacketIdentity =
        serde_json::from_slice(&std::fs::read(&arguments.critique_packet)?)?;
    let assessment: CritiqueAssessment =
        serde_json::from_slice(&std::fs::read(&arguments.assessment)?)?;
    if mechanical.schema != SCHEMA
        || receipt.schema != "reflex-lean-temporal-audit-execution-v1"
        || packet.schema != "reflex-lean-blinded-mathematical-critique-v1"
        || assessment.schema != "reflex-lean-mathematical-critique-assessment-v1"
    {
        return Err("Lean Temporal Audit finalization schema differs".into());
    }
    let mechanical_file_sha256 = hash_file(&arguments.mechanical_report)?;
    let packet_file_sha256 = hash_file(&arguments.critique_packet)?;
    verify_self_hashed_json(&arguments.mechanical_report, &mechanical.content_sha256)?;
    verify_self_hashed_json(&arguments.critique_packet, &packet.content_sha256)?;
    if packet.mechanical_report_file_sha256 != mechanical_file_sha256
        || assessment.critique_packet_file_sha256 != packet_file_sha256
        || mechanical.execution_receipt_file_sha256 != hash_file(&receipt_path)?
        || mechanical.protocol_sha256 != receipt.protocol_sha256
        || mechanical.freeze_manifest_file_sha256 != receipt.freeze_manifest_file_sha256
        || mechanical.audit_lock_content_sha256 != receipt.audit_lock_content_sha256
    {
        return Err("Lean Temporal Audit critique chain is not content-addressed".into());
    }
    validate_assessment(&packet, &assessment)?;
    let confirmed = mechanical.mechanical_gate_passed && assessment.protocol_deviations.is_empty();
    let mut report = FinalReport {
        schema: "reflex-lean-temporal-audit-final-v1",
        status: if confirmed {
            "scientific-confirmation"
        } else {
            "null-result"
        },
        critique_complete: true,
        mechanical_gate_passed: mechanical.mechanical_gate_passed,
        scientifically_confirmed: confirmed,
        mechanical_report_file_sha256: mechanical_file_sha256,
        mechanical_report_content_sha256: mechanical.content_sha256,
        critique_packet_file_sha256: packet_file_sha256,
        critique_packet_content_sha256: packet.content_sha256,
        critique_assessment_file_sha256: hash_file(&arguments.assessment)?,
        critique: assessment,
        next_domain: "Wrela",
        next_domain_role: "registered-third-domain; not-rescue-analysis",
        content_sha256: String::new(),
    };
    report.content_sha256 = hash_json(&report)?;
    if let Some(parent) = arguments.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&arguments.output)?;
    serde_json::to_writer_pretty(file, &report)?;
    println!(
        "status={} content_sha256={} next_domain=Wrela",
        report.status, report.content_sha256
    );
    Ok(())
}

fn finalize_failed_execution(
    receipt: &Path,
    failure: &Path,
    output: &Path,
) -> Result<(), AnyError> {
    let failure_identity: FailureIdentity = serde_json::from_slice(&std::fs::read(failure)?)?;
    if failure_identity.schema != "reflex-lean-temporal-audit-failure-v1"
        || failure_identity.status != "sealed-failure; rerun-forbidden"
    {
        return Err("Lean Temporal Audit failure report identity differs".into());
    }
    let mut report = FailedExecutionFinalReport {
        schema: "reflex-lean-temporal-audit-final-v1",
        status: "null-result",
        critique_complete: false,
        mechanical_gate_passed: false,
        scientifically_confirmed: false,
        execution_receipt_file_sha256: hash_file(receipt)?,
        failure_report_file_sha256: hash_file(failure)?,
        failure_status: failure_identity.status,
        next_domain: "Wrela",
        next_domain_role: "registered-third-domain; not-rescue-analysis",
        content_sha256: String::new(),
    };
    report.content_sha256 = hash_json(&report)?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)?;
    serde_json::to_writer_pretty(file, &report)?;
    println!(
        "status=null-result content_sha256={} next_domain=Wrela",
        report.content_sha256
    );
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the one-shot audit controller keeps exposure, sealing, replay, and resource gates visibly ordered"
)]
fn confirm_once(arguments: &[String]) -> Result<(), AnyError> {
    require_release("lean-temporal-audit-confirm")?;
    let run_started = Instant::now();
    let run_cpu = cpu_time::ProcessTime::now();
    let host = environment()?;
    require_clean(&host, SCHEMA)?;
    let arguments = parse(arguments)?;
    require_registered_confirm_paths(&arguments)?;
    for (path, description) in [
        (&arguments.audit_catalog, "Lean Temporal Audit catalog"),
        (&arguments.output, "Lean Temporal Audit result"),
        (
            &arguments.critique_packet,
            "blinded mathematical critique packet",
        ),
    ] {
        require_absent(path, description)?;
    }
    let manifest_bytes = std::fs::read(&arguments.manifest)?;
    let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
    let lock: Lock = serde_json::from_slice(&std::fs::read(&arguments.lock)?)?;
    validate_inputs(&arguments, &manifest, &lock)?;
    let receipt_path = execution_receipt_path()?;
    require_absent(&receipt_path, "Lean Temporal Audit execution receipt")?;
    let receipt = ExecutionReceipt {
        schema: "reflex-lean-temporal-audit-execution-v1",
        status: "audit exposure started; this receipt is never replaced",
        protocol_sha256: &manifest.protocol_sha256,
        freeze_manifest_file_sha256: &lock.freeze_manifest_file_sha256,
        audit_lock_content_sha256: &lock.content_sha256,
        git_revision: &host.git_revision,
    };
    if let Some(parent) = receipt_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let receipt_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&receipt_path)?;
    serde_json::to_writer_pretty(receipt_file, &receipt)?;
    let execution_receipt_file_sha256 = hash_file(&receipt_path)?;

    let december_catalog = LeanCatalog::load(&arguments.december_catalog)?;
    if december_catalog.content_sha256() != manifest.catalog_sha256[2] {
        return Err("December catalog differs from the frozen manifest".into());
    }
    let audit_config = LeanWorkerConfig::for_snapshot(
        &arguments.lake,
        &arguments.audit_root,
        LeanSnapshotPin::new(
            lock.mathlib_commit.clone(),
            lock.lean_toolchain.clone(),
            lock.lean_toolchain_alias.clone(),
            lock.lean_version.clone(),
            lock.lean_commit.clone(),
        ),
    );
    let primary_workers_started = Instant::now();
    let audit_worker = LeanWorker::start(&audit_config)?;
    let audit_catalog = LeanCatalog::build(&audit_worker, 4096)?;
    audit_catalog.save_new(&arguments.audit_catalog)?;
    let december = TemporalSnapshot::from_catalog(&december_catalog);
    let audit = TemporalSnapshot::from_catalog(&audit_catalog);
    let pair = TemporalPair::derive(&december, &audit)?;
    let targets = pair
        .examples
        .iter()
        .map(|example| (example.declaration.to_string(), example.targets))
        .collect::<HashMap<_, _>>();
    let outcomes = evaluate(&manifest.rankings, &targets);
    let pareto = pareto_outcomes(&manifest.rankings, &targets)?;
    let pareto_dominates = pareto.iter().all(|outcome| outcome.difference >= 0.0)
        && pareto.iter().any(|outcome| outcome.difference > 0.0)
        && pareto
            .iter()
            .all(|outcome| outcome.simultaneous_lower_bound_99 >= 0.0);
    let causal_ablations = ablation_outcomes(&manifest.rankings, &targets)?;
    let causal_ablations_passed = causal_ablations.iter().all(|outcome| outcome.passed);
    let maximum_exhaustion_cpu_ns = outcomes
        .iter()
        .flat_map(|outcome| &outcome.heads)
        .map(|head| head.exhaustion_cpu_ns)
        .max()
        .unwrap_or(u64::MAX);
    let anytime = CPU_CHECKPOINT_SECONDS
        .into_iter()
        .enumerate()
        .map(|(checkpoint, cpu_seconds)| AnytimeCheckpoint {
            cpu_seconds,
            all_artifacts_exhausted: outcomes
                .iter()
                .flat_map(|outcome| &outcome.heads)
                .all(|head| head.anytime[checkpoint].exhausted),
            maximum_exhaustion_cpu_ns,
            outcome_reference: "per-treatment, per-head anytime outcomes",
        })
        .collect::<Vec<_>>();

    let verifier_resident = 2 * reflex_lean::worker::DEFAULT_WORKER_RESIDENT_BYTES;
    if peak_process_resident_bytes().saturating_add(verifier_resident) > RESIDENT_LIMIT {
        return Err("Lean Temporal Audit cannot start the second verifier inside 48 GiB".into());
    }
    let december_worker = LeanWorker::start(&LeanWorkerConfig::pinned(
        &arguments.lake,
        &arguments.december_root,
    ))?;
    let (records, sources, kernel_calls, kernel_cpu_upper_bound_ns) =
        replay_rankings(&manifest.rankings, &december_worker, &audit_worker)?;
    let time_to_utility = time_gate(&records, &targets);
    let relationship_evaluation = certify_selected_relationships(
        &manifest.rankings,
        &pair.relationship_candidates,
        &december_worker,
        &audit_worker,
    )?;
    drop(december_worker);
    drop(audit_worker);
    let primary_workers_wall_ns = duration_ns(primary_workers_started.elapsed());

    let recovery_started = Instant::now();
    let recovered = LeanWorker::start(&audit_config)?;
    let (recovery, recovery_usage) = migrate_theorems(&sources, &recovered)?;
    let expected = records
        .iter()
        .map(|record| (record.declaration.as_str(), record.accepted))
        .collect::<HashMap<_, _>>();
    let recovered_names = recovery
        .migrated
        .iter()
        .map(|theorem| theorem.name.to_string())
        .collect::<HashSet<_>>();
    let cold_recovery_decisions_matched = sources
        .iter()
        .filter(|source| {
            let name = source.name.to_string();
            recovered_names.contains(&name) == expected.get(name.as_str()).copied().unwrap_or(false)
        })
        .count();
    let mut recovered_evidence = HashMap::new();
    for theorem in &recovery.migrated {
        recovered_evidence.insert(
            theorem.name.to_string(),
            migration_evidence_hash(&theorem.name.to_string(), Some(theorem), "")?,
        );
    }
    for failure in &recovery.failures {
        recovered_evidence.insert(
            failure.declaration.to_string(),
            migration_evidence_hash(&failure.declaration.to_string(), None, &failure.diagnostic)?,
        );
    }
    let expected_evidence = records
        .iter()
        .map(|record| {
            (
                record.declaration.as_str(),
                record.kernel_evidence_sha256.as_str(),
            )
        })
        .collect::<HashMap<_, _>>();
    let cold_recovery_evidence_matched = sources
        .iter()
        .filter(|source| {
            let name = source.name.to_string();
            recovered_evidence.get(&name).map(String::as_str)
                == expected_evidence.get(name.as_str()).copied()
        })
        .count();
    drop(recovered);
    let recovery_worker_wall_ns = duration_ns(recovery_started.elapsed());

    let replay = ReplaySummary {
        attempted: records.len(),
        accepted: records.iter().filter(|record| record.accepted).count(),
        rejected: records.iter().filter(|record| !record.accepted).count(),
        cold_recovery_attempted: sources.len(),
        cold_recovery_decisions_matched,
        cold_recovery_evidence_matched,
        records,
    };
    let controller_peak = peak_process_resident_bytes();
    let combined_resident_upper_bound_bytes = controller_peak.saturating_add(verifier_resident);
    let wall_limit_passed = run_started.elapsed() <= WALL_LIMIT;
    let no_regression = replay.rejected == 0
        && replay
            .records
            .iter()
            .all(|record| record.elegance_preserved)
        && replay.cold_recovery_attempted == replay.cold_recovery_decisions_matched
        && replay.cold_recovery_attempted == replay.cold_recovery_evidence_matched
        && combined_resident_upper_bound_bytes <= RESIDENT_LIMIT
        && wall_limit_passed;
    let mechanical_gate_passed =
        pareto_dominates && causal_ablations_passed && time_to_utility.passed && no_regression;
    let critique_items = build_critique_items(
        &manifest.rankings,
        &sources,
        &targets,
        &relationship_evaluation.critique,
    )?;
    let mut report = Report {
        schema: SCHEMA,
        status: "mechanical-results-sealed; mathematical-critique-pending",
        mechanical_results_sealed: true,
        mechanical_gate_passed,
        confirmed: false,
        protocol_sha256: manifest.protocol_sha256,
        freeze_manifest_file_sha256: lock.freeze_manifest_file_sha256,
        freeze_manifest_content_sha256: manifest.content_sha256,
        audit_lock_content_sha256: lock.content_sha256,
        execution_receipt_file_sha256,
        audit_artifact_set_sha256: manifest.audit_artifact_set_sha256,
        december_catalog_sha256: december_catalog.content_sha256().into(),
        audit_catalog_sha256: audit_catalog.content_sha256().into(),
        audit_mathlib_commit: lock.mathlib_commit,
        audit_lean_commit: lock.lean_commit,
        audit_examples: pair.examples.len(),
        audit_new_declarations: pair.later_new_declarations,
        outcomes,
        pareto,
        pareto_dominates,
        causal_ablations,
        causal_ablations_passed,
        anytime,
        time_to_utility,
        replay,
        relationships: relationship_evaluation.summary,
        no_regression_passed: no_regression,
        wall_limit_passed,
        economics: Economics {
            wall_ns: duration_ns(run_started.elapsed()),
            controller_cpu_ns: duration_ns(run_cpu.elapsed()),
            controller_peak_resident_bytes: controller_peak,
            combined_resident_upper_bound_bytes,
            catalog_durable_bytes: [
                std::fs::metadata(&arguments.december_catalog)?.len(),
                std::fs::metadata(&arguments.audit_catalog)?.len(),
            ],
            result_durable_bytes: 0,
            kernel_calls: kernel_calls + relationship_evaluation.kernel_calls + 1,
            kernel_cpu_upper_bound_ns: kernel_cpu_upper_bound_ns
                .saturating_add(relationship_evaluation.kernel_cpu_upper_bound_ns)
                .saturating_add(duration_ns(recovery_usage.cpu_upper_bound)),
            verifier_lifetime_cpu_upper_bound_ns: primary_workers_wall_ns
                .saturating_mul(2)
                .saturating_add(recovery_worker_wall_ns),
        },
        host,
        protocol_deviations: Vec::new(),
        content_sha256: String::new(),
    };
    let encoded = finalize_report(&mut report)?;
    if let Some(parent) = arguments.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&arguments.output, encoded)?;
    let report_file_sha256 = hash_file(&arguments.output)?;
    write_critique_packet(
        &arguments.critique_packet,
        &report_file_sha256,
        critique_items,
    )?;
    println!(
        "mechanical_gate_passed={} pareto_dominates={} report_sha256={} audit_catalog_sha256={}",
        report.mechanical_gate_passed,
        report.pareto_dominates,
        report_file_sha256,
        report.audit_catalog_sha256
    );
    Ok(())
}

fn validate_inputs(
    arguments: &Arguments,
    manifest: &Manifest,
    lock: &Lock,
) -> Result<(), AnyError> {
    if manifest.schema != FREEZE_SCHEMA || lock.schema != LOCK_SCHEMA {
        return Err("Lean Temporal Audit manifest or lock schema differs".into());
    }
    if hash_file(&arguments.manifest)? != lock.freeze_manifest_file_sha256
        || manifest.content_sha256 != lock.freeze_manifest_content_sha256
        || manifest.protocol_sha256 != lock.protocol_sha256
        || manifest.audit_artifact_set_sha256 != lock.audit_artifact_set_sha256
    {
        return Err("Lean Temporal Audit manifest does not match its committed lock".into());
    }
    if lock.content_sha256.is_empty() {
        return Err("Lean Temporal Audit lock has no content identity".into());
    }
    if hex(&Sha256::digest(lock.boundary_evidence_response.as_bytes()))
        != lock.boundary_evidence_sha256
    {
        return Err("Lean Temporal Audit boundary response identity differs".into());
    }
    let mut unhashed_lock = lock.clone();
    let expected_lock_hash = std::mem::take(&mut unhashed_lock.content_sha256);
    if hash_json(&unhashed_lock)? != expected_lock_hash {
        return Err("Lean Temporal Audit lock content identity differs".into());
    }
    if git_output(&["rev-parse", "HEAD^"])? != lock.host.git_revision {
        return Err(
            "Lean Temporal Audit confirmation must run from the committed lock revision".into(),
        );
    }
    Ok(())
}

fn evaluate(
    rankings: &[Ranking],
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
) -> Vec<TreatmentOutcome> {
    parallel_map_indexed(rankings.len(), |index| {
        let ranking = &rankings[index];
        TreatmentOutcome {
            treatment: ranking.treatment.clone(),
            heads: ranking
                .heads
                .iter()
                .enumerate()
                .map(|(head, artifacts)| evaluate_head(ranking, head, artifacts, targets))
                .collect(),
        }
    })
}

fn evaluate_head(
    ranking: &Ranking,
    head: usize,
    artifacts: &[FrozenArtifact],
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
) -> HeadOutcome {
    let preparation_cpu_ns = ranking
        .training_cpu_ns
        .saturating_add(ranking.selection_cpu_ns[head]);
    let evaluation_cpu = cpu_time::ProcessTime::now();
    let mut units = BTreeMap::<String, Vec<f64>>::new();
    let mut selected = 0_usize;
    let mut anytime = CPU_CHECKPOINT_SECONDS.map(|cpu_seconds| AnytimeHeadOutcome {
        cpu_seconds,
        cpu_used_ns: preparation_cpu_ns,
        artifacts_evaluated: 0,
        statistical_units: 0,
        directional_utility: 0.0,
        exhausted: artifacts.is_empty(),
    });
    for (index, artifact) in artifacts.iter().enumerate() {
        let target = targets.get(&artifact.declaration);
        selected += usize::from(target.is_some());
        let raw = target.map_or_else(
            || {
                if PotentialHead::ALL[head].lower_is_better() {
                    1.0
                } else {
                    0.0
                }
            },
            |targets| f64::from(targets[head]),
        );
        units
            .entry(statistical_unit(
                &artifact.module,
                &artifact.semantic_family,
            ))
            .or_default()
            .push(raw);
        let cpu_used_ns = preparation_cpu_ns.saturating_add(duration_ns(evaluation_cpu.elapsed()));
        let utility = directional(head, family_mean(&units));
        for outcome in &mut anytime {
            if cpu_used_ns <= outcome.cpu_seconds.saturating_mul(1_000_000_000) {
                outcome.cpu_used_ns = cpu_used_ns;
                outcome.artifacts_evaluated = index + 1;
                outcome.statistical_units = units.len();
                outcome.directional_utility = utility;
                outcome.exhausted = index + 1 == artifacts.len();
            }
        }
    }
    let evaluation_cpu_ns = duration_ns(evaluation_cpu.elapsed());
    let raw_mean = family_mean(&units);
    HeadOutcome {
        head: head_name(head),
        selected,
        missing: artifacts.len().saturating_sub(selected),
        statistical_units: units.len(),
        raw_mean,
        directional_utility: directional(head, raw_mean),
        evaluation_cpu_ns,
        exhaustion_cpu_ns: preparation_cpu_ns.saturating_add(evaluation_cpu_ns),
        anytime: anytime.into(),
    }
}

fn family_mean(units: &BTreeMap<String, Vec<f64>>) -> f64 {
    mean_or_zero(
        &units
            .values()
            .map(|values| mean(values))
            .collect::<Vec<_>>(),
    )
}

fn ablation_outcomes(
    rankings: &[Ranking],
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
) -> Result<Vec<AblationOutcome>, AnyError> {
    let full = rankings
        .iter()
        .find(|ranking| ranking.treatment == "full")
        .ok_or("frozen rankings omit Full")?;
    Ok(rankings
        .iter()
        .filter(|ranking| {
            matches!(
                ranking.treatment.as_str(),
                "bootstrap" | "no-model" | "no-consolidation" | "immediate-only"
            )
        })
        .map(|ablation| {
            let mut full_wins = 0;
            let mut ties = 0;
            let mut full_losses = 0;
            for head in 0..POTENTIAL_HEADS {
                let (_, _, difference) =
                    paired_family_point(&full.heads[head], &ablation.heads[head], head, targets);
                match difference.total_cmp(&0.0) {
                    std::cmp::Ordering::Greater => full_wins += 1,
                    std::cmp::Ordering::Equal => ties += 1,
                    std::cmp::Ordering::Less => full_losses += 1,
                }
            }
            AblationOutcome {
                treatment: ablation.treatment.clone(),
                full_wins,
                ties,
                full_losses,
                passed: full_losses == 0 && full_wins != 0,
            }
        })
        .collect())
}

fn pareto_outcomes(
    rankings: &[Ranking],
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
) -> Result<Vec<ParetoOutcome>, AnyError> {
    let full_ranking = rankings
        .iter()
        .find(|ranking| ranking.treatment == "full")
        .ok_or("frozen rankings omit Full artifacts")?;
    let baseline_rankings = ["uniform", "dependency-light", "historical-reuse"].map(|name| {
        rankings
            .iter()
            .find(|ranking| ranking.treatment == name)
            .expect("three frozen baseline rankings exist")
    });
    let bound_values = parallel_map_indexed(POTENTIAL_HEADS * 3, |job| {
        let head = job / 3;
        let baseline = job % 3;
        clustered_difference_lower_bound(
            &full_ranking.heads[head],
            &baseline_rankings[baseline].heads[head],
            head,
            targets,
        )
    });
    let mut lower_bounds = std::array::from_fn::<_, POTENTIAL_HEADS, _>(|_| [0.0; 3]);
    for (job, value) in bound_values.into_iter().enumerate() {
        lower_bounds[job / 3][job % 3] = value;
    }
    Ok((0..POTENTIAL_HEADS)
        .map(|head| {
            let paired = baseline_rankings.map(|ranking| {
                let (full, baseline, difference) = paired_family_point(
                    &full_ranking.heads[head],
                    &ranking.heads[head],
                    head,
                    targets,
                );
                (ranking.treatment.as_str(), full, baseline, difference)
            });
            let best = paired
                .iter()
                .max_by(|left, right| left.2.total_cmp(&right.2).then_with(|| right.0.cmp(left.0)))
                .expect("three frozen baselines exist");
            ParetoOutcome {
                head: head_name(head),
                full: best.1,
                virtual_best_baseline: best.2,
                difference: best.3,
                simultaneous_lower_bound_99: lower_bounds[head]
                    .into_iter()
                    .fold(f64::INFINITY, f64::min),
                baseline: best.0.to_owned(),
            }
        })
        .collect())
}

fn paired_family_point(
    full: &[FrozenArtifact],
    comparator: &[FrozenArtifact],
    head: usize,
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
) -> (f64, f64, f64) {
    let mut families = BTreeMap::<(String, String), (Vec<f64>, Vec<f64>)>::new();
    for (artifacts, side) in [(full, 0_usize), (comparator, 1_usize)] {
        for artifact in artifacts {
            let utility = targets
                .get(&artifact.declaration)
                .map_or(0.0, |targets| directional(head, f64::from(targets[head])));
            let values = families
                .entry((artifact.module.clone(), artifact.semantic_family.clone()))
                .or_default();
            if side == 0 {
                values.0.push(utility);
            } else {
                values.1.push(utility);
            }
        }
    }
    let (mut full_total, mut comparator_total) = (0.0, 0.0);
    for (full, comparator) in families.values() {
        full_total += mean_or_zero(full);
        comparator_total += mean_or_zero(comparator);
    }
    let denominator = f64::from(u32::try_from(families.len()).unwrap_or(u32::MAX));
    if denominator == 0.0 {
        return (0.0, 0.0, 0.0);
    }
    let full = full_total / denominator;
    let comparator = comparator_total / denominator;
    (full, comparator, full - comparator)
}

fn clustered_difference_lower_bound(
    full: &[FrozenArtifact],
    baseline: &[FrozenArtifact],
    head: usize,
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
) -> f64 {
    let mut families = BTreeMap::<(String, String), (Vec<f64>, Vec<f64>)>::new();
    for artifact in full {
        let utility = targets
            .get(&artifact.declaration)
            .map_or(0.0, |targets| directional(head, f64::from(targets[head])));
        families
            .entry((artifact.module.clone(), artifact.semantic_family.clone()))
            .or_default()
            .0
            .push(utility);
    }
    for artifact in baseline {
        let utility = targets
            .get(&artifact.declaration)
            .map_or(0.0, |targets| directional(head, f64::from(targets[head])));
        families
            .entry((artifact.module.clone(), artifact.semantic_family.clone()))
            .or_default()
            .1
            .push(utility);
    }
    let mut modules = BTreeMap::<String, Vec<(String, f64)>>::new();
    for ((module, family), (full, baseline)) in families {
        modules
            .entry(module)
            .or_default()
            .push((family, mean_or_zero(&full) - mean_or_zero(&baseline)));
    }
    let modules = modules.into_values().collect::<Vec<_>>();
    if modules.is_empty() {
        return f64::NEG_INFINITY;
    }
    let mut samples = Vec::with_capacity(10_000);
    for replicate in 0..10_000_u64 {
        let mut state =
            splitmix64(replicate ^ u64::try_from(head).unwrap_or(u64::MAX) ^ 0x7061_7265_746f_3939);
        let mut total = 0.0;
        for _ in 0..modules.len() {
            state = splitmix64(state);
            let module_count = u64::try_from(modules.len()).unwrap_or(u64::MAX);
            let module = &modules[usize::try_from(state % module_count).unwrap_or(0)];
            let mut module_total = 0.0;
            for _ in 0..module.len() {
                state = splitmix64(state);
                let family_count = u64::try_from(module.len()).unwrap_or(u64::MAX);
                module_total += module[usize::try_from(state % family_count).unwrap_or(0)].1;
            }
            total += module_total / f64::from(u32::try_from(module.len()).unwrap_or(u32::MAX));
        }
        samples.push(total / f64::from(u32::try_from(modules.len()).unwrap_or(u32::MAX)));
    }
    samples.sort_unstable_by(f64::total_cmp);
    // Bonferroni alpha = 0.01 / (7 heads * 3 baselines). Ten
    // thousand deterministic resamples put the lower rank at index four.
    samples[4]
}

fn time_gate(
    records: &[ReplayRecord],
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
) -> TimeGate {
    let heads = time_heads(records, targets, None);
    let geometric_mean_speedup = geometric_mean(
        &heads
            .iter()
            .filter_map(|outcome| outcome.speedup)
            .collect::<Vec<_>>(),
        POTENTIAL_HEADS,
    );
    let point_gate_passed = geometric_mean_speedup.is_some_and(|speedup| speedup >= 10.0);
    let mut interval_gate_evaluated = false;
    let mut simultaneous_lower_bound_99 = None;
    if point_gate_passed {
        interval_gate_evaluated = true;
        let modules =
            std::array::from_fn::<_, POTENTIAL_HEADS, _>(|head| bootstrap_modules(records, head));
        let mut speedups = parallel_map_indexed(10_000, |replicate| {
            let weights = std::array::from_fn(|head| {
                bootstrap_weights(
                    &modules[head],
                    u64::try_from(replicate).unwrap_or(u64::MAX)
                        ^ u64::try_from(head).unwrap_or(u64::MAX),
                )
            });
            let sample = time_heads(records, targets, Some(&weights));
            let values = sample
                .iter()
                .filter_map(|outcome| outcome.speedup)
                .collect::<Vec<_>>();
            geometric_mean(&values, POTENTIAL_HEADS).unwrap_or(0.0)
        });
        speedups.sort_unstable_by(f64::total_cmp);
        simultaneous_lower_bound_99 = speedups.get(99).copied();
    }
    let passed = point_gate_passed && simultaneous_lower_bound_99.is_some_and(|lower| lower > 3.0);
    TimeGate {
        heads,
        geometric_mean_speedup,
        point_gate_passed,
        interval_gate_evaluated,
        simultaneous_lower_bound_99,
        passed,
    }
}

fn time_heads(
    records: &[ReplayRecord],
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
    weights: Option<&[HashMap<String, u32>; POTENTIAL_HEADS]>,
) -> Vec<TimeToUtilityOutcome> {
    (0..POTENTIAL_HEADS)
        .map(|head| {
            let full = replay_sequence(records, "full", head);
            let head_weights = weights.map(|weights| &weights[head]);
            let full_final = final_utility(&full, head, targets, head_weights);
            let baselines = ["uniform", "dependency-light", "historical-reuse"].map(|name| {
                let sequence = replay_sequence(records, name, head);
                let utility = final_utility(&sequence, head, targets, head_weights);
                (name, sequence, utility)
            });
            let best = baselines
                .iter()
                .max_by(|left, right| left.2.total_cmp(&right.2).then_with(|| right.0.cmp(left.0)))
                .expect("three frozen baselines exist");
            let common_target = full_final.min(best.2);
            let full_cpu_ns = time_to_target(&full, head, targets, head_weights, common_target);
            let baseline = best.0.to_owned();
            let virtual_best_baseline_cpu_ns =
                time_to_target(&best.1, head, targets, head_weights, common_target);
            let speedup = virtual_best_baseline_cpu_ns
                .zip(full_cpu_ns)
                .filter(|(_, full)| *full != 0)
                .map(|(baseline, full)| {
                    std::time::Duration::from_nanos(baseline).as_secs_f64()
                        / std::time::Duration::from_nanos(full).as_secs_f64()
                });
            TimeToUtilityOutcome {
                head: head_name(head),
                common_target,
                full_cpu_ns,
                virtual_best_baseline_cpu_ns,
                baseline,
                speedup,
            }
        })
        .collect()
}

fn replay_sequence<'a>(
    records: &'a [ReplayRecord],
    treatment: &str,
    head: usize,
) -> Vec<&'a ReplayRecord> {
    let mut sequence = records
        .iter()
        .filter(|record| record.treatment == treatment && record.head == head_name(head))
        .collect::<Vec<_>>();
    sequence.sort_unstable_by_key(|record| record.rank);
    sequence
}

fn final_utility(
    sequence: &[&ReplayRecord],
    head: usize,
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
    weights: Option<&HashMap<String, u32>>,
) -> f64 {
    weighted_utility(sequence, head, targets, weights).unwrap_or(0.0)
}

fn time_to_target(
    sequence: &[&ReplayRecord],
    head: usize,
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
    weights: Option<&HashMap<String, u32>>,
    target: f64,
) -> Option<u64> {
    if target <= 0.0 {
        return sequence
            .first()
            .map(|record| record.preparation_cpu_upper_bound_ns);
    }
    for end in 1..=sequence.len() {
        let prefix = &sequence[..end];
        if weighted_utility(prefix, head, targets, weights).is_some_and(|value| value >= target) {
            return Some(prefix[end - 1].cumulative_cpu_upper_bound_ns);
        }
    }
    None
}

fn weighted_utility(
    sequence: &[&ReplayRecord],
    head: usize,
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
    weights: Option<&HashMap<String, u32>>,
) -> Option<f64> {
    let mut units = BTreeMap::<String, (f64, u64)>::new();
    for record in sequence {
        let value = if record.accepted {
            targets
                .get(&record.declaration)
                .map_or(0.0, |targets| directional(head, f64::from(targets[head])))
        } else {
            0.0
        };
        let entry = units
            .entry(statistical_unit(&record.module, &record.semantic_family))
            .or_default();
        entry.0 += value;
        entry.1 = entry.1.saturating_add(1);
    }
    let mut total = 0.0;
    let mut count = 0_u64;
    if let Some(weights) = weights {
        for (unit, weight) in weights {
            let value = units.get(unit).map_or(0.0, |(unit_total, unit_count)| {
                unit_total / f64::from(u32::try_from(*unit_count).unwrap_or(u32::MAX))
            });
            total += value * f64::from(*weight);
            count = count.saturating_add(u64::from(*weight));
        }
    } else {
        for (unit_total, unit_count) in units.into_values() {
            let value = unit_total / f64::from(u32::try_from(unit_count).unwrap_or(u32::MAX));
            total += value;
            count = count.saturating_add(1);
        }
    }
    (count != 0).then_some(total / f64::from(u32::try_from(count).unwrap_or(u32::MAX)))
}

fn geometric_mean(values: &[f64], required: usize) -> Option<f64> {
    if values.len() != required
        || values
            .iter()
            .any(|value| *value <= 0.0 || !value.is_finite())
    {
        return None;
    }
    Some(
        (values.iter().map(|value| value.ln()).sum::<f64>()
            / f64::from(u32::try_from(required).unwrap_or(u32::MAX)))
        .exp(),
    )
}

fn bootstrap_modules(records: &[ReplayRecord], head: usize) -> Vec<Vec<String>> {
    let mut modules = BTreeMap::<String, HashSet<String>>::new();
    for record in records
        .iter()
        .filter(|record| record.head == head_name(head))
    {
        modules
            .entry(record.module.clone())
            .or_default()
            .insert(statistical_unit(&record.module, &record.semantic_family));
    }
    modules
        .into_values()
        .map(|families| {
            let mut families = families.into_iter().collect::<Vec<_>>();
            families.sort_unstable();
            families
        })
        .collect()
}

fn statistical_unit(module: &str, family: &str) -> String {
    format!("{module}\0{family}")
}

fn mean_or_zero(values: &[f64]) -> f64 {
    if values.is_empty() { 0.0 } else { mean(values) }
}

fn parallel_map_indexed<T, F>(len: usize, function: F) -> Vec<T>
where
    T: Send,
    F: Fn(usize) -> T + Sync,
{
    let next = AtomicUsize::new(0);
    let slots = (0..len).map(|_| Mutex::new(None)).collect::<Vec<_>>();
    std::thread::scope(|scope| {
        for _ in 0..IN_PROCESS_LANES.min(len) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    if index >= len {
                        break;
                    }
                    *slots[index].lock().expect("parallel result slot poisoned") =
                        Some(function(index));
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|slot| {
            slot.into_inner()
                .expect("parallel result slot poisoned")
                .expect("parallel result slot omitted")
        })
        .collect()
}

fn bootstrap_weights(modules: &[Vec<String>], replicate: u64) -> HashMap<String, u32> {
    let mut state = splitmix64(replicate ^ 0x6c65_616e_6175_6469);
    let mut weights = HashMap::new();
    for _ in 0..modules.len() {
        state = splitmix64(state);
        let module_count = u64::try_from(modules.len()).unwrap_or(u64::MAX);
        let module = &modules[usize::try_from(state % module_count).unwrap_or(0)];
        for _ in 0..module.len() {
            state = splitmix64(state);
            let family_count = u64::try_from(module.len()).unwrap_or(u64::MAX);
            let family = &module[usize::try_from(state % family_count).unwrap_or(0)];
            *weights.entry(family.clone()).or_default() += 1;
        }
    }
    weights
}

const fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

fn replay_rankings(
    rankings: &[Ranking],
    december: &LeanWorker,
    audit: &LeanWorker,
) -> Result<(Vec<ReplayRecord>, Vec<IndexedTheorem>, usize, u64), AnyError> {
    let mut records = Vec::new();
    let mut unique_sources = BTreeMap::<String, IndexedTheorem>::new();
    let mut calls = 0_usize;
    let mut cpu = 0_u64;
    for ranking in rankings {
        for (head, artifacts) in ranking.heads.iter().enumerate() {
            let preparation = ranking
                .training_cpu_ns
                .saturating_add(ranking.selection_cpu_ns[head]);
            let mut cumulative = preparation;
            for (rank, artifact) in artifacts.iter().take(REPLAY_PER_HEAD).enumerate() {
                let fetch_started = Instant::now();
                let name = LeanName::from_dotted(&artifact.declaration);
                let mut fetched = december.fetch(&[name])?;
                cumulative = cumulative.saturating_add(duration_ns(fetch_started.elapsed()));
                let Some(source) = fetched.pop() else {
                    records.push(ReplayRecord {
                        treatment: ranking.treatment.clone(),
                        head: head_name(head),
                        rank,
                        declaration: artifact.declaration.clone(),
                        module: artifact.module.clone(),
                        semantic_family: artifact.semantic_family.clone(),
                        accepted: false,
                        diagnostic: "source theorem unavailable".into(),
                        kernel_evidence_sha256: String::new(),
                        preparation_cpu_upper_bound_ns: preparation,
                        cumulative_cpu_upper_bound_ns: cumulative,
                        proof_nodes: 0,
                        proof_depth: 0,
                        proof_encoded_bytes: 0,
                        source_dependencies: 0,
                        source_axioms: 0,
                        replayed_dependencies: None,
                        replayed_axioms: None,
                        elegance_preserved: false,
                    });
                    continue;
                };
                unique_sources
                    .entry(artifact.declaration.clone())
                    .or_insert_with(|| source.clone());
                let (migration, usage) = migrate_theorems(std::slice::from_ref(&source), audit)?;
                calls = calls.saturating_add(1);
                let used = duration_ns(usage.cpu_upper_bound);
                cpu = cpu.saturating_add(used);
                cumulative = cumulative.saturating_add(used);
                let accepted = !migration.migrated.is_empty();
                let diagnostic = migration
                    .failures
                    .first()
                    .map_or_else(String::new, |failure| failure.diagnostic.clone());
                let kernel_evidence_sha256 = migration_evidence_hash(
                    &artifact.declaration,
                    migration.migrated.first(),
                    &diagnostic,
                )?;
                let proof_nodes = source.proof_term.node_count();
                let proof_depth = source.proof_term.depth();
                let proof_encoded_bytes = serde_json::to_vec(&source.proof_term)?.len();
                let replayed = migration.migrated.first();
                let replayed_dependencies = replayed.map(|theorem| theorem.dependencies.len());
                let replayed_axioms = replayed.map(|theorem| theorem.axioms.len());
                let elegance_preserved = replayed.is_some_and(|theorem| {
                    theorem.proof_term.node_count() <= proof_nodes
                        && theorem.proof_term.depth() <= proof_depth
                        && serde_json::to_vec(&theorem.proof_term)
                            .is_ok_and(|encoded| encoded.len() <= proof_encoded_bytes)
                        && theorem.dependencies.len() <= source.dependencies.len()
                        && theorem.axioms.len() <= source.axioms.len()
                });
                records.push(ReplayRecord {
                    treatment: ranking.treatment.clone(),
                    head: head_name(head),
                    rank,
                    declaration: artifact.declaration.clone(),
                    module: artifact.module.clone(),
                    semantic_family: artifact.semantic_family.clone(),
                    accepted,
                    diagnostic,
                    kernel_evidence_sha256,
                    preparation_cpu_upper_bound_ns: preparation,
                    cumulative_cpu_upper_bound_ns: cumulative,
                    proof_nodes,
                    proof_depth,
                    proof_encoded_bytes,
                    source_dependencies: source.dependencies.len(),
                    source_axioms: source.axioms.len(),
                    replayed_dependencies,
                    replayed_axioms,
                    elegance_preserved,
                });
            }
        }
    }
    Ok((records, unique_sources.into_values().collect(), calls, cpu))
}

fn migration_evidence_hash(
    declaration: &str,
    migrated: Option<&IndexedTheorem>,
    diagnostic: &str,
) -> Result<String, AnyError> {
    let mut digest = Sha256::new();
    digest.update(b"reflex-lean-migration-evidence-v1\0");
    digest.update(declaration.as_bytes());
    digest.update([0]);
    if let Some(theorem) = migrated {
        digest.update([1]);
        digest.update(serde_json::to_vec(theorem)?);
    } else {
        digest.update([0]);
        digest.update(diagnostic.as_bytes());
    }
    Ok(hex(&digest.finalize()))
}

fn certify_selected_relationships(
    rankings: &[Ranking],
    candidates: &[RelationshipCandidate],
    december: &LeanWorker,
    audit: &LeanWorker,
) -> Result<RelationshipEvaluation, AnyError> {
    let selected = rankings
        .iter()
        .find(|ranking| ranking.treatment == "full")
        .into_iter()
        .flat_map(|ranking| ranking.heads.iter())
        .flat_map(|head| head.iter())
        .map(|candidate| candidate.declaration.as_str())
        .collect::<HashSet<_>>();
    let eligible = candidates
        .iter()
        .filter(|candidate| selected.contains(candidate.earlier.to_string().as_str()))
        .collect::<Vec<_>>();
    let sample = stratified_relationships(&eligible, RELATIONSHIP_LIMIT);
    let mut certified = Vec::new();
    let mut records = Vec::with_capacity(sample.len());
    let mut calls = 0_usize;
    let mut cpu = 0_u64;
    for candidate in &sample {
        let result = certify_relationship(december, audit, candidate)?;
        if let Some(usage) = result.usage {
            calls = calls.saturating_add(1);
            cpu = cpu.saturating_add(duration_ns(usage.cpu_upper_bound));
        }
        if let Some(certificate) = result.certificate {
            records.push(certified_relationship_record(candidate, &certificate)?);
            certified.push(certificate);
        } else if let Some(rejection) = result.rejection {
            records.push(rejected_relationship_record(candidate, &rejection)?);
        } else {
            records.push(unavailable_relationship_record(candidate));
        }
    }
    let consolidated = consolidate_certificates(&certified);
    let count = |kind| {
        certified
            .iter()
            .filter(|certificate| certificate.kind == kind)
            .count()
    };
    let consolidated_count = |kind| {
        consolidated
            .iter()
            .filter(|relationship| relationship.kind == kind)
            .count()
    };
    let mut critique_relationships = HashMap::<String, Vec<CritiqueRelationship>>::new();
    for certificate in &certified {
        critique_relationships
            .entry(certificate.earlier.name.to_string())
            .or_default()
            .push(CritiqueRelationship {
                kind: certificate.kind,
                later_declaration: certificate.later.name.to_string(),
                proof_nodes_removed: certificate.proof_nodes_removed,
            });
    }
    Ok(RelationshipEvaluation {
        summary: RelationshipSummary {
            attempted: sample.len(),
            certified: certified.len(),
            rejected_or_unavailable: sample.len().saturating_sub(certified.len()),
            exact: count(RelationshipKind::Exact),
            definitional: count(RelationshipKind::Definitional),
            specialization: count(RelationshipKind::Specialization),
            derivation: count(RelationshipKind::Derivation),
            family_collapse: consolidated_count(RelationshipKind::FamilyCollapse),
            corpus_compression: consolidated_count(RelationshipKind::CorpusCompression),
            proof_nodes_removed: certified
                .iter()
                .map(|certificate| certificate.proof_nodes_removed)
                .sum(),
            records,
        },
        critique: critique_relationships,
        kernel_calls: calls,
        kernel_cpu_upper_bound_ns: cpu,
    })
}

fn certified_relationship_record(
    candidate: &RelationshipCandidate,
    certificate: &reflex_lean::temporal::CertifiedRelationship,
) -> Result<RelationshipRecord, AnyError> {
    Ok(RelationshipRecord {
        earlier_declaration: candidate.earlier.to_string(),
        later_declaration: candidate.later.to_string(),
        expected_kind: candidate.expected,
        certified_kind: Some(certificate.kind),
        kernel_accepted: certificate.verification.accepted,
        kernel_dependencies: certificate
            .verification
            .dependencies
            .iter()
            .map(ToString::to_string)
            .collect(),
        kernel_axioms: certificate
            .verification
            .axioms
            .iter()
            .map(ToString::to_string)
            .collect(),
        kernel_diagnostic: certificate.verification.diagnostic.clone(),
        kernel_evidence_sha256: relationship_evidence_hash(candidate, &certificate.verification)?,
        proof_nodes_removed: certificate.proof_nodes_removed,
    })
}

fn unavailable_relationship_record(candidate: &RelationshipCandidate) -> RelationshipRecord {
    RelationshipRecord {
        earlier_declaration: candidate.earlier.to_string(),
        later_declaration: candidate.later.to_string(),
        expected_kind: candidate.expected,
        certified_kind: None,
        kernel_accepted: false,
        kernel_dependencies: Vec::new(),
        kernel_axioms: Vec::new(),
        kernel_diagnostic: "relationship unavailable or rejected without a certificate".into(),
        kernel_evidence_sha256: String::new(),
        proof_nodes_removed: 0,
    }
}

fn rejected_relationship_record(
    candidate: &RelationshipCandidate,
    verification: &reflex_lean::worker::VerificationResult,
) -> Result<RelationshipRecord, AnyError> {
    Ok(RelationshipRecord {
        earlier_declaration: candidate.earlier.to_string(),
        later_declaration: candidate.later.to_string(),
        expected_kind: candidate.expected,
        certified_kind: None,
        kernel_accepted: verification.accepted,
        kernel_dependencies: verification
            .dependencies
            .iter()
            .map(ToString::to_string)
            .collect(),
        kernel_axioms: verification
            .axioms
            .iter()
            .map(ToString::to_string)
            .collect(),
        kernel_diagnostic: verification.diagnostic.clone(),
        kernel_evidence_sha256: relationship_evidence_hash(candidate, verification)?,
        proof_nodes_removed: 0,
    })
}

fn stratified_relationships<'a>(
    candidates: &[&'a RelationshipCandidate],
    limit: usize,
) -> Vec<&'a RelationshipCandidate> {
    let exact_limit = limit.saturating_mul(2).div_ceil(3);
    let mut selected = candidates
        .iter()
        .copied()
        .filter(|candidate| candidate.expected == RelationshipKind::Exact)
        .take(exact_limit)
        .collect::<Vec<_>>();
    let specialization_limit = limit.saturating_sub(selected.len()).div_ceil(2);
    selected.extend(
        candidates
            .iter()
            .copied()
            .filter(|candidate| candidate.expected == RelationshipKind::Specialization)
            .take(specialization_limit),
    );
    selected.extend(
        candidates
            .iter()
            .copied()
            .filter(|candidate| candidate.expected == RelationshipKind::Derivation)
            .take(limit.saturating_sub(selected.len())),
    );
    selected
}

fn relationship_evidence_hash(
    candidate: &RelationshipCandidate,
    verification: &reflex_lean::worker::VerificationResult,
) -> Result<String, AnyError> {
    let mut digest = Sha256::new();
    digest.update(b"reflex-lean-relationship-evidence-v1\0");
    digest.update(candidate.earlier.to_string().as_bytes());
    digest.update([0]);
    digest.update(candidate.later.to_string().as_bytes());
    digest.update([candidate.expected.evidence_code()]);
    digest.update(serde_json::to_vec(verification)?);
    Ok(hex(&digest.finalize()))
}

fn build_critique_items(
    rankings: &[Ranking],
    sources: &[IndexedTheorem],
    targets: &HashMap<String, [f32; POTENTIAL_HEADS]>,
    relationships: &HashMap<String, Vec<CritiqueRelationship>>,
) -> Result<Vec<CritiqueItem>, AnyError> {
    let source = sources
        .iter()
        .map(|theorem| (theorem.name.to_string(), theorem))
        .collect::<HashMap<_, _>>();
    let full = rankings
        .iter()
        .find(|ranking| ranking.treatment == "full")
        .ok_or("frozen rankings omit Full")?;
    let mut artifacts = full
        .heads
        .iter()
        .flat_map(|head| head.iter())
        .filter(|artifact| source.contains_key(&artifact.declaration))
        .collect::<Vec<_>>();
    artifacts.sort_unstable_by_key(|artifact| {
        let mut digest = Sha256::new();
        digest.update(b"reflex-lean-blinded-critique-v1\0");
        digest.update(artifact.semantic_family.as_bytes());
        digest.finalize()
    });
    artifacts.dedup_by_key(|artifact| artifact.declaration.as_str());
    if artifacts.len() < 32 {
        return Err(format!(
            "blinded mathematical critique requires 32 replayed Full artifacts, found {}",
            artifacts.len()
        )
        .into());
    }
    let mut items = Vec::with_capacity(32);
    for artifact in artifacts.into_iter().take(32) {
        let theorem = source
            .get(&artifact.declaration)
            .expect("critique artifacts were filtered to replayed sources");
        let target_profile = targets
            .get(&artifact.declaration)
            .copied()
            .ok_or("critique artifact is absent from the audit target set")?;
        let mut digest = Sha256::new();
        digest.update(b"reflex-lean-blinded-item-v1\0");
        digest.update(artifact.declaration.as_bytes());
        let blinded_id = hex(&digest.finalize());
        items.push(CritiqueItem {
            blinded_id,
            declaration: artifact.declaration.clone(),
            module: artifact.module.clone(),
            semantic_family: artifact.semantic_family.clone(),
            proof_nodes: theorem.proof_term.node_count(),
            proof_depth: theorem.proof_term.depth(),
            proof_encoded_bytes: serde_json::to_vec(&theorem.proof_term)?.len(),
            dependencies: theorem.dependencies.len(),
            allowed_axioms: theorem.axioms.len(),
            target_profile,
            certified_future_relationships: relationships
                .get(&artifact.declaration)
                .cloned()
                .unwrap_or_default(),
        });
    }
    Ok(items)
}

fn write_critique_packet(
    path: &PathBuf,
    report_sha256: &str,
    items: Vec<CritiqueItem>,
) -> Result<(), AnyError> {
    let mut packet = CritiquePacket {
        schema: "reflex-lean-blinded-mathematical-critique-v1",
        status: "mechanical-results-sealed; critique-pending",
        mechanical_report_file_sha256: report_sha256.into(),
        instructions: "Inspect mathematical generality, lemma collapse, proof elegance, and likely human value without treatment labels; do not alter the sealed mechanical decision.",
        items,
        content_sha256: String::new(),
    };
    packet.content_sha256 = hash_json(&packet)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec_pretty(&packet)?)?;
    Ok(())
}

fn finalize_report(report: &mut Report) -> Result<Vec<u8>, AnyError> {
    for _ in 0..8 {
        report.content_sha256.clear();
        report.content_sha256 = hash_json(report)?;
        let encoded = serde_json::to_vec_pretty(report)?;
        if report.economics.result_durable_bytes == encoded.len() {
            return Ok(encoded);
        }
        report.economics.result_durable_bytes = encoded.len();
    }
    Err("Lean Temporal Audit durable-byte accounting did not converge".into())
}

fn execution_receipt_path() -> Result<PathBuf, AnyError> {
    Ok(repository_root()?.join("docs/experiments/lean-temporal-audit-v1.started.json"))
}

fn failure_report_path() -> Result<PathBuf, AnyError> {
    Ok(repository_root()?.join("docs/experiments/lean-temporal-audit-v1.failed.json"))
}

fn registered_path(file: &str) -> Result<PathBuf, AnyError> {
    Ok(repository_root()?.join("docs/experiments").join(file))
}

fn require_registered_confirm_paths(arguments: &Arguments) -> Result<(), AnyError> {
    if arguments.manifest != registered_path("lean-temporal-audit-v1-freeze.json")?
        || arguments.lock != registered_path("lean-temporal-audit-v1-lock.json")?
        || arguments.output != registered_path("lean-temporal-audit-v1-mechanical.json")?
        || arguments.critique_packet
            != registered_path("lean-temporal-audit-v1-critique-packet.json")?
    {
        return Err(
            "Lean Temporal Audit seal artifacts must use their registered absolute paths".into(),
        );
    }
    Ok(())
}

fn require_registered_finalize_paths(arguments: &FinalizeArguments) -> Result<(), AnyError> {
    if arguments.mechanical_report != registered_path("lean-temporal-audit-v1-mechanical.json")?
        || arguments.critique_packet
            != registered_path("lean-temporal-audit-v1-critique-packet.json")?
        || arguments.assessment
            != registered_path("lean-temporal-audit-v1-critique-assessment.json")?
        || arguments.output != registered_path("lean-temporal-audit-v1-final.json")?
    {
        return Err(
            "Lean Temporal Audit final artifacts must use their registered absolute paths".into(),
        );
    }
    Ok(())
}

fn verify_self_hashed_json(path: &PathBuf, expected: &str) -> Result<(), AnyError> {
    let bytes = std::fs::read(path)?;
    let mut compact = Vec::with_capacity(bytes.len());
    let mut in_string = false;
    let mut escaped = false;
    for byte in bytes {
        if in_string {
            compact.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
            compact.push(byte);
        } else if !byte.is_ascii_whitespace() {
            compact.push(byte);
        }
    }
    let suffix = format!("\"content_sha256\":\"{expected}\"}}");
    if !compact.ends_with(suffix.as_bytes()) {
        return Err("content-addressed JSON has a malformed terminal identity".into());
    }
    compact.truncate(compact.len().saturating_sub(suffix.len()));
    compact.extend_from_slice(b"\"content_sha256\":\"\"}");
    if hex(&Sha256::digest(&compact)) != expected {
        return Err("content-addressed JSON identity does not recompute".into());
    }
    Ok(())
}

fn repository_root() -> Result<PathBuf, AnyError> {
    Ok(PathBuf::from(git_output(&[
        "rev-parse",
        "--show-toplevel",
    ])?))
}

fn require_supervising_parent() -> Result<(), AnyError> {
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    let fields = stat
        .rsplit_once(')')
        .ok_or("Linux process metadata is malformed")?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let parent = fields
        .get(1)
        .ok_or("Linux process metadata omits the parent")?;
    let parent_executable = std::fs::read_link(format!("/proc/{parent}/exe"))?;
    if parent_executable != std::env::current_exe()? {
        return Err("Lean Temporal Audit child parent is not its registered supervisor".into());
    }
    let command = std::fs::read(format!("/proc/{parent}/cmdline"))?;
    let arguments = command
        .split(|byte| *byte == 0)
        .filter_map(|value| std::str::from_utf8(value).ok())
        .collect::<Vec<_>>();
    if !arguments.contains(&"lean-temporal-audit-confirm")
        || arguments.contains(&"lean-temporal-audit-confirm-child")
    {
        return Err("Lean Temporal Audit child parent has no registered supervisor command".into());
    }
    Ok(())
}

fn git_output(arguments: &[&str]) -> Result<String, AnyError> {
    let output = Command::new("git").args(arguments).output()?;
    if !output.status.success() {
        return Err("Lean Temporal Audit must run inside its committed repository".into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn retain_failure_if_exposed(report: &FailureReport<'_>) -> Result<(), AnyError> {
    if !execution_receipt_path()?.exists() {
        return Ok(());
    }
    let path = failure_report_path()?;
    if path.exists() {
        return Ok(());
    }
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    serde_json::to_writer_pretty(file, report)?;
    Ok(())
}

fn validate_assessment(
    packet: &CritiquePacketIdentity,
    assessment: &CritiqueAssessment,
) -> Result<(), AnyError> {
    if packet.items.len() != 32
        || assessment.items.len() != 32
        || assessment.reviewer.trim().is_empty()
        || assessment.overall_assessment.trim().is_empty()
        || assessment
            .protocol_deviations
            .iter()
            .any(|deviation| deviation.trim().is_empty())
    {
        return Err("Lean Temporal Audit critique must completely assess 32 items".into());
    }
    let packet_ids = packet
        .items
        .iter()
        .map(|item| item.blinded_id.as_str())
        .collect::<HashSet<_>>();
    let assessed_ids = assessment
        .items
        .iter()
        .map(|item| item.blinded_id.as_str())
        .collect::<HashSet<_>>();
    if packet_ids.len() != 32 || assessed_ids != packet_ids {
        return Err("Lean Temporal Audit critique IDs differ from the blinded packet".into());
    }
    for item in &assessment.items {
        for rating in [
            &item.generality,
            &item.proof_collapse,
            &item.local_elegance,
            &item.family_elegance,
            &item.corpus_compression,
        ] {
            if !matches!(rating.as_str(), "low" | "medium" | "high" | "not-observed") {
                return Err("Lean Temporal Audit elegance ratings use an unknown value".into());
            }
        }
        if !matches!(
            item.human_value.as_str(),
            "unlikely" | "plausible" | "clear" | "not-assessable"
        ) || item.notes.trim().is_empty()
        {
            return Err("Lean Temporal Audit human-value assessment is incomplete".into());
        }
    }
    Ok(())
}

fn parse_finalize(arguments: &[String]) -> Result<FinalizeArguments, AnyError> {
    let values = parse_flag_values(
        arguments,
        &[
            "--mechanical-report",
            "--critique-packet",
            "--assessment",
            "--output",
        ],
        "lean-temporal-audit-finalize",
    )?;
    let path = |flag| -> Result<PathBuf, AnyError> {
        values
            .get(flag)
            .map(|value| PathBuf::from(*value))
            .ok_or_else(|| format!("lean-temporal-audit-finalize requires {flag}").into())
    };
    Ok(FinalizeArguments {
        mechanical_report: path("--mechanical-report")?,
        critique_packet: path("--critique-packet")?,
        assessment: path("--assessment")?,
        output: path("--output")?,
    })
}

fn parse(arguments: &[String]) -> Result<Arguments, AnyError> {
    let values = parse_flag_values(
        arguments,
        &[
            "--manifest",
            "--lock",
            "--lake",
            "--december-root",
            "--december-catalog",
            "--audit-root",
            "--audit-catalog",
            "--output",
            "--critique-packet",
        ],
        "lean-temporal-audit-confirm",
    )?;
    let path = |flag| -> Result<PathBuf, AnyError> {
        values
            .get(flag)
            .map(|value| PathBuf::from(*value))
            .ok_or_else(|| format!("lean-temporal-audit-confirm requires {flag}").into())
    };
    Ok(Arguments {
        manifest: path("--manifest")?,
        lock: path("--lock")?,
        lake: path("--lake")?,
        december_root: path("--december-root")?,
        december_catalog: path("--december-catalog")?,
        audit_root: path("--audit-root")?,
        audit_catalog: path("--audit-catalog")?,
        output: path("--output")?,
        critique_packet: path("--critique-packet")?,
    })
}

fn mean(values: &[f64]) -> f64 {
    if values.is_empty() {
        return f64::NAN;
    }
    values.iter().sum::<f64>() / f64::from(u32::try_from(values.len()).unwrap_or(u32::MAX))
}

fn directional(head: usize, value: f64) -> f64 {
    if PotentialHead::ALL[head].lower_is_better() {
        1.0 - value
    } else {
        value
    }
}

fn head_name(head: usize) -> &'static str {
    PotentialHead::ALL[head].name()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut output, byte| {
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
        output
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(name: &str, module: &str, family: &str) -> FrozenArtifact {
        FrozenArtifact {
            declaration: name.into(),
            module: module.into(),
            semantic_family: family.into(),
        }
    }

    #[test]
    fn module_nested_bootstrap_is_deterministic_and_directional() {
        let full = vec![artifact("A", "M1", "f1"), artifact("B", "M2", "f2")];
        let baseline = vec![artifact("C", "M1", "f3"), artifact("D", "M2", "f4")];
        let targets = [
            ("A".into(), [1.0; POTENTIAL_HEADS]),
            ("B".into(), [1.0; POTENTIAL_HEADS]),
            ("C".into(), [0.0; POTENTIAL_HEADS]),
            ("D".into(), [0.0; POTENTIAL_HEADS]),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>();
        let first = clustered_difference_lower_bound(&full, &baseline, 0, &targets);
        let second = clustered_difference_lower_bound(&full, &baseline, 0, &targets);
        assert!(first >= 0.0);
        assert!((first - second).abs() < f64::EPSILON);
    }

    #[test]
    fn missing_audit_artifacts_remain_in_every_head_denominator() {
        let ranking = Ranking {
            treatment: "full".into(),
            training_cpu_ns: 10,
            selection_cpu_ns: [20; POTENTIAL_HEADS],
            heads: std::array::from_fn(|_| {
                vec![
                    artifact("present", "M", "f1"),
                    artifact("missing", "M", "f2"),
                ]
            }),
        };
        let targets = [("present".into(), [1.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0])]
            .into_iter()
            .collect::<HashMap<_, _>>();
        let outcomes = evaluate(&[ranking], &targets);
        for head in &outcomes[0].heads {
            assert_eq!(head.selected, 1);
            assert_eq!(head.missing, 1);
            assert!((head.directional_utility - 0.5).abs() < f64::EPSILON);
            assert!(head.exhaustion_cpu_ns >= 30);
        }
    }

    #[test]
    fn point_gates_weight_module_nested_families_not_declarations() {
        let ranking = Ranking {
            treatment: "full".into(),
            training_cpu_ns: 2 * 3_600 * 1_000_000_000,
            selection_cpu_ns: [0; POTENTIAL_HEADS],
            heads: std::array::from_fn(|_| {
                vec![
                    artifact("A", "M", "shared"),
                    artifact("B", "M", "shared"),
                    artifact("C", "M", "other"),
                ]
            }),
        };
        let targets = [
            ("A".into(), [1.0; POTENTIAL_HEADS]),
            ("B".into(), [1.0; POTENTIAL_HEADS]),
            ("C".into(), [0.0; POTENTIAL_HEADS]),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>();
        let outcomes = evaluate(&[ranking], &targets);
        let anticipation = &outcomes[0].heads[0];
        assert_eq!(anticipation.statistical_units, 2);
        assert!((anticipation.directional_utility - 0.5).abs() < f64::EPSILON);
        assert_eq!(anticipation.anytime[0].artifacts_evaluated, 0);
        assert!(!anticipation.anytime[0].exhausted);
        assert_eq!(anticipation.anytime[1].artifacts_evaluated, 3);
        assert!(anticipation.anytime[1].exhausted);
    }

    #[test]
    fn paired_point_gate_keeps_absent_families_in_both_denominators() {
        let full = vec![artifact("A", "M", "full-only")];
        let comparator = vec![artifact("B", "M", "comparator-only")];
        let targets = [
            ("A".into(), [1.0; POTENTIAL_HEADS]),
            ("B".into(), [1.0; POTENTIAL_HEADS]),
        ]
        .into_iter()
        .collect::<HashMap<_, _>>();
        let (full, comparator, difference) = paired_family_point(&full, &comparator, 0, &targets);
        assert!((full - 0.5).abs() < f64::EPSILON);
        assert!((comparator - 0.5).abs() < f64::EPSILON);
        assert!(difference.abs() < f64::EPSILON);
    }

    #[test]
    fn parallel_analysis_retains_input_order() {
        assert_eq!(
            parallel_map_indexed(32, |index| index * index),
            (0..32).map(|index| index * index).collect::<Vec<_>>()
        );
    }

    #[test]
    fn finalizer_recomputes_terminal_json_content_identity() {
        #[derive(Serialize)]
        struct SelfHashed<'a> {
            value: &'a str,
            content_sha256: String,
        }

        let mut document = SelfHashed {
            value: "verified",
            content_sha256: String::new(),
        };
        document.content_sha256 = hash_json(&document).expect("test JSON hashes");
        let path =
            std::env::temp_dir().join(format!("reflex-self-hash-test-{}.json", std::process::id()));
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&document).expect("test JSON encodes"),
        )
        .expect("test JSON writes");
        verify_self_hashed_json(&path, &document.content_sha256)
            .expect("untampered identity recomputes");
        std::fs::write(
            &path,
            b"{\"value\":\"tampered\",\"content_sha256\":\"bad\"}",
        )
        .expect("tampered JSON writes");
        assert!(verify_self_hashed_json(&path, "bad").is_err());
        std::fs::remove_file(path).expect("temporary test JSON removes");
    }

    #[test]
    fn splitmix_stream_changes_with_replicate() {
        assert_ne!(splitmix64(1), splitmix64(2));
        assert_eq!(splitmix64(1), splitmix64(1));
    }

    #[test]
    fn time_gate_includes_preparation_cpu_and_bootstraps_nested_families() {
        let mut records = Vec::new();
        let mut targets = HashMap::new();
        for head in 0..POTENTIAL_HEADS {
            for (treatment, cpu) in [
                ("full", 100_u64),
                ("uniform", 1_000),
                ("dependency-light", 1_000),
                ("historical-reuse", 1_000),
            ] {
                let declaration = format!("{treatment}.{head}");
                targets.insert(declaration.clone(), [1.0; POTENTIAL_HEADS]);
                records.push(ReplayRecord {
                    treatment: treatment.into(),
                    head: head_name(head),
                    rank: 0,
                    declaration,
                    module: format!("M{head}"),
                    semantic_family: format!("family-{head}"),
                    accepted: true,
                    diagnostic: String::new(),
                    kernel_evidence_sha256: format!("evidence-{treatment}-{head}"),
                    preparation_cpu_upper_bound_ns: cpu / 2,
                    cumulative_cpu_upper_bound_ns: cpu,
                    proof_nodes: 1,
                    proof_depth: 1,
                    proof_encoded_bytes: 1,
                    source_dependencies: 0,
                    source_axioms: 0,
                    replayed_dependencies: Some(0),
                    replayed_axioms: Some(0),
                    elegance_preserved: true,
                });
            }
        }
        let gate = time_gate(&records, &targets);
        assert!(gate.point_gate_passed);
        assert!(gate.interval_gate_evaluated);
        assert!(gate.passed);
        assert!(
            gate.geometric_mean_speedup
                .is_some_and(|speedup| speedup >= 10.0)
        );
        assert_eq!(
            time_to_target(&[&records[0]], 0, &targets, None, 0.0),
            Some(records[0].preparation_cpu_upper_bound_ns)
        );
    }
}
