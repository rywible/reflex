use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsString;
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reflex::internal_experiments::{
    CandidateAllocationQueueInspection, CandidateFateInspection, CandidateFateOutcomeInspection,
    CandidateNoveltyFilterReasonInspection, ExperienceVerdictInspection,
    compare_candidate_features, inspect_domain_resources, inspect_experience_segment,
    inspect_session_segment,
};
use reflex::{
    BundlePlan, Direction, DomainDefinition, GoalSet, ImprovementRequest, NonEmpty,
    NonZeroDuration, Objective, OptimizationGoal, ParetoUpdate, Preference, ResourceEnvelope,
    ResourceUsage, StructuralProtocol, improve,
};
use reflex_bundle::{CanonicalBundle, SegmentKind};
use reflex_lean::ast::{LeanArtifact, LeanExpr};
use reflex_lean::catalog::LeanCatalog;
use reflex_lean::domain::{LeanCorpus, LeanDomain, LeanMetric, LeanSeedScope, LeanStructure};
use reflex_lean::temporal::{TemporalExample, TemporalSnapshot};
use reflex_lean::worker::{IndexedTheorem, LeanWorker, LeanWorkerConfig, VerificationItem};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::harness::{
    AnyError, HostEnvironment, HostIsolation, LEAN_PUBLIC_NESTED_RESIDENT_LIMIT,
    LEAN_PUBLIC_NESTED_WALL_LIMIT, capture_large_campaign_child, completion_name, environment,
    hash_file, hash_json, hex, inherited_host_isolation, parse_flag_values, require_absent,
    require_clean, require_release,
};

const DEVELOPMENT_SCHEMA: &str = "reflex-lean-public-optimizer-development-v27";
const RUNTIME_RESIDENT_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const SUPERVISOR_RESIDENT_BYTES: u64 = LEAN_PUBLIC_NESTED_RESIDENT_LIMIT;
const HOST_CPU_RESERVE: usize = 1;
const WORKER_THREADS: usize = 6;
const DURABLE_BYTES: u64 = 1024 * 1024 * 1024;
const SUPERVISOR_WALL_LIMIT: Duration = LEAN_PUBLIC_NESTED_WALL_LIMIT;
const SUPERVISOR_CAPABILITY: &str = "reflex-lean-public-optimizer-supervisor-v1";
const PRIMARY_PROOF_NODE_LIMIT: usize = 100_000;
const SELECTION_POOL_MULTIPLIER: usize = 8;
const LIBRARY_CANDIDATES_PER_SEED: usize = 8;

struct Arguments {
    lake: PathBuf,
    december_root: PathBuf,
    september_catalog: PathBuf,
    december_catalog: PathBuf,
    work: PathBuf,
    output: PathBuf,
    training_artifacts: usize,
    heldout_artifacts: usize,
    training_verification_requests: u64,
    verification_requests: u64,
    preflight_only: bool,
}

struct DevelopmentCorpus {
    config: LeanWorkerConfig,
    training_corpus: LeanCorpus,
    heldout_corpus: LeanCorpus,
    training: usize,
    heldout: usize,
    september_catalog_sha256: String,
    december_catalog_sha256: String,
    heldout_seed_nodes: HashMap<String, usize>,
    selected_artifacts: Vec<SelectedArtifact>,
}

#[derive(Serialize)]
struct SelectedArtifact {
    role: &'static str,
    declaration: String,
    statement_hash: u64,
    proof_nodes: usize,
}

struct SelectedTheorem {
    example: TemporalExample,
    theorem: IndexedTheorem,
}

#[derive(Serialize)]
struct Usage {
    worker_threads: usize,
    resident_bytes: u64,
    verification_requests: u64,
    durable_bytes: u64,
    elapsed_ns: u64,
    cpu_ns: u64,
}

#[derive(Serialize)]
struct DomainResources {
    requested_worker_threads: usize,
    external_worker_lanes: usize,
    runtime_worker_lanes: usize,
    runtime_stack_bytes: u64,
    durability_stack_bytes: u64,
    external_worker_bytes: u64,
    operator_bytes: u64,
    maximum_candidate_capacity: usize,
    operator_scratch_bytes_per_lane: u64,
    operator_scratch_bytes: u64,
    fixed_resident_bytes: u64,
    dynamic_headroom_bytes: u64,
}

#[derive(Serialize)]
struct TreatmentResult {
    treatment: &'static str,
    completion: String,
    heldout_pareto_artifacts: usize,
    strict_proof_node_improvements: usize,
    proof_nodes_removed: usize,
    strict_improvements: Vec<StrictImprovement>,
    candidate_verification_curve: Vec<VerificationCurvePoint>,
    usage: Usage,
    bundle_sha256: String,
}

#[derive(Serialize)]
struct VerificationCurvePoint {
    request_budget: usize,
    reached: bool,
    observed_requests: usize,
    strict_discoveries: usize,
    cpu_ns_through_completed_batch: u64,
}

#[derive(Serialize)]
struct DiscoveryEfficiencyComparison {
    request_budget: usize,
    full_strict_discoveries: usize,
    bootstrap_strict_discoveries: usize,
    full_candidate_cpu_ns: u64,
    bootstrap_candidate_cpu_ns: u64,
    full_uses_less_candidate_cpu_per_improvement: bool,
}

#[derive(Serialize)]
struct StrictImprovement {
    observer_sequence: u64,
    artifact_key: String,
    declaration: String,
    seed_proof_nodes: usize,
    proof_nodes: usize,
    proof_nodes_removed: usize,
}

#[derive(Serialize)]
struct LearningSummary {
    generation: u64,
    champion_present: bool,
}

#[derive(Serialize)]
struct BundleSummary {
    schema: &'static str,
    identity: String,
    completed: bool,
    completion: Option<&'static str>,
    requested: Usage,
    usage: Usage,
    artifact_records: u64,
    experience_entries: u64,
    experience_claims: usize,
    accepted_claims: usize,
    accepted_experience: u64,
    refuted_experience: u64,
    unknown_experience: u64,
    candidate_fates: usize,
    novelty_filtered_candidates: usize,
    policy_deferred_candidates: usize,
    verification_interrupted_candidates: usize,
    verified_by_queue: [usize; 4],
    admitted_candidates: usize,
    candidate_fate_trace: Vec<CandidateFateSummary>,
    learning: LearningSummary,
    knowledge_revision: String,
    model_revision: String,
    durable_size_bound: u64,
    recovery: RecoverySummary,
    segment_logical_bytes: Vec<SegmentLogicalBytes>,
}

#[derive(Serialize)]
struct RecoverySummary {
    pareto_artifacts: usize,
    frontier_artifacts: usize,
    deferred_candidates: usize,
    deferred_canonical_bytes: u64,
    pending_parents: usize,
}

#[derive(Serialize)]
struct CandidateFateSummary {
    candidate_key: String,
    claim_digest: String,
    parent_key: String,
    operator_digest: String,
    epoch: u64,
    generation_rank: u32,
    proposal_limit: u32,
    policy_rank: Option<u32>,
    verification_batch_cpu_ns: Option<u64>,
    verification_batch_size: Option<u32>,
    outcome: &'static str,
    novelty_filter_reason: Option<&'static str>,
    allocation_queue: Option<&'static str>,
    bootstrap_rank: Option<u32>,
    learned_rank: Option<u32>,
    verdict: Option<&'static str>,
    admitted: bool,
    strict_improvement: bool,
}

#[derive(Serialize)]
struct FeatureDevelopmentReport {
    schema: &'static str,
    status: &'static str,
    retained_bundle_sha256: String,
    examples: usize,
    claim_operator_feature_groups: usize,
    mixed_verdict_claim_operator_feature_groups: usize,
    accepted_examples_in_mixed_groups: usize,
    accepted_refuted_collision_groups: usize,
    accepted_in_accepted_refuted_groups: usize,
    accepted_refuted_collision_fraction_ppm: u64,
    collision_examples_in_operational_top_k: [usize; 7],
    collision_accepted_in_operational_top_k: [usize; 7],
    collision_groups: Vec<CollisionGroupTrace>,
    accepted_examples: usize,
    proposal_informed_examples: usize,
    replay_claims: usize,
    selection_claims: usize,
    feature_count: usize,
    head_count: usize,
    baseline_selection_loss: [f32; 7],
    structural_selection_loss: [f32; 7],
    balanced_structural_selection_loss: [f32; 7],
    baseline_training_cpu_ns: u64,
    structural_training_cpu_ns: u64,
    balanced_structural_training_cpu_ns: u64,
    feature_extraction_cpu_ns: u64,
    baseline_model_revision: String,
    structural_model_revision: String,
    balanced_structural_model_revision: String,
    model_bytes: usize,
    baseline_reproduces_champion: bool,
    structural_promotes_over_baseline: bool,
    ranking_budgets: [usize; 7],
    bootstrap_accepted_at_k: [usize; 7],
    baseline_accepted_at_k: [usize; 7],
    structural_accepted_at_k: [usize; 7],
    balanced_structural_accepted_at_k: [usize; 7],
    global_bootstrap_accepted_at_k: [usize; 7],
    global_baseline_accepted_at_k: [usize; 7],
    global_structural_accepted_at_k: [usize; 7],
    global_balanced_structural_accepted_at_k: [usize; 7],
    evaluated_at_k: [usize; 7],
    selection_accepted: usize,
    host: HostEnvironment,
    content_sha256: String,
}

#[derive(Serialize)]
struct DonorAudit {
    proof_substitution_attempts: usize,
    accepted_proof_substitutions: usize,
    reconstructed_attempts: usize,
    ambiguous_attempts: usize,
    unmatched_attempts: usize,
    accepted_refuted_collision_groups: usize,
    accepted_in_accepted_refuted_groups: usize,
    accepted_refuted_collision_fraction_ppm: u64,
    collision_examples_in_operational_top_k: [usize; 7],
    collision_accepted_in_operational_top_k: [usize; 7],
    collision_groups: Vec<CollisionGroupTrace>,
    traces: Vec<DonorTrace>,
}

#[derive(Clone, Serialize)]
struct DonorTrace {
    candidate_key: String,
    claim_digest: String,
    epoch: u64,
    verdict: &'static str,
    operational_policy_rank: u32,
    strict_improvement: bool,
    feature_bits: Vec<u32>,
    support_key_sha256: Option<String>,
    proof_term_sha256: Option<String>,
    donor_declarations: Vec<String>,
}

#[derive(Serialize)]
struct CollisionGroupTrace {
    claim_digest: String,
    operator: &'static str,
    feature_bits: Vec<u32>,
    members: Vec<DonorTrace>,
}

#[derive(Serialize)]
struct SegmentLogicalBytes {
    segment: &'static str,
    bytes: usize,
}

struct ExperienceSummary {
    entries: u64,
    claims: usize,
    accepted_claims: usize,
    verdicts: [u64; 3],
    candidate_fates: usize,
    novelty_filtered: usize,
    policy_deferred: usize,
    verification_interrupted: usize,
    verified_by_queue: [usize; 4],
    admitted: usize,
    candidate_fate_trace: Vec<CandidateFateSummary>,
}

#[derive(Serialize)]
struct Report {
    schema: &'static str,
    status: &'static str,
    training_artifacts: usize,
    heldout_artifacts: usize,
    training_verification_limit: u64,
    evaluation_verification_limit: u64,
    primary_proof_node_limit: usize,
    primary_selection: &'static str,
    september_catalog_sha256: String,
    december_catalog_sha256: String,
    selected_artifacts: Vec<SelectedArtifact>,
    training_usage: Usage,
    training_learning: LearningSummary,
    training_domain_resources: DomainResources,
    heldout_domain_resources: DomainResources,
    phase_zero_information_audit: DonorAudit,
    full: TreatmentResult,
    no_model: TreatmentResult,
    no_derived: TreatmentResult,
    bootstrap: TreatmentResult,
    full_has_more_improvements: bool,
    full_uses_less_end_to_end_cpu_per_improvement: bool,
    request_16_candidate_efficiency: Option<DiscoveryEfficiencyComparison>,
    full_noninferior_after_16_requests: bool,
    host: HostEnvironment,
    host_isolation: HostIsolation,
    content_sha256: String,
}

struct ReportInputs {
    host: HostEnvironment,
    host_isolation: HostIsolation,
    training_artifacts: usize,
    heldout_artifacts: usize,
    september_catalog_sha256: String,
    december_catalog_sha256: String,
    selected_artifacts: Vec<SelectedArtifact>,
    training_usage: Usage,
    training_learning: LearningSummary,
    training_domain_resources: DomainResources,
    heldout_domain_resources: DomainResources,
    phase_zero_information_audit: DonorAudit,
    full: TreatmentResult,
    no_model: TreatmentResult,
    no_derived: TreatmentResult,
    bootstrap: TreatmentResult,
}

#[derive(Serialize)]
struct PreflightReport {
    schema: &'static str,
    status: &'static str,
    training_artifacts: usize,
    heldout_artifacts: usize,
    selected_artifacts: Vec<SelectedArtifact>,
    runtime_resident_limit_bytes: u64,
    training_domain_resources: DomainResources,
    heldout_domain_resources: DomainResources,
    host: HostEnvironment,
    host_isolation: HostIsolation,
    content_sha256: String,
}

pub fn development(arguments: &[String]) -> Result<(), AnyError> {
    require_release("lean-public-optimizer-development")?;
    let host = environment()?;
    require_clean(&host, DEVELOPMENT_SCHEMA)?;
    if host.available_parallelism < WORKER_THREADS + HOST_CPU_RESERVE {
        return Err(
            "Lean public-optimizer development cannot preserve its host CPU reserve".into(),
        );
    }
    let parsed = parse(arguments)?;
    require_absent(&parsed.output, "Lean public-optimizer development report")?;
    if parsed.work.exists() {
        return Err(format!(
            "Lean public-optimizer work directory already exists: {}",
            parsed.work.display()
        )
        .into());
    }
    let executable = std::env::current_exe()?;
    let child_arguments =
        std::iter::once(OsString::from("lean-public-optimizer-development-child"))
            .chain(arguments.iter().map(OsString::from))
            .collect::<Vec<_>>();
    let evidence_prefix = std::env::temp_dir().join(format!(
        "reflex-lean-public-optimizer-supervisor-{}",
        std::process::id()
    ));
    let phase_report_prefix = parsed.work.join("runtime-phase");
    let isolation = inherited_host_isolation()?;
    let capture = capture_large_campaign_child(
        &executable,
        &child_arguments,
        &evidence_prefix,
        Some(SUPERVISOR_WALL_LIMIT),
        Some(SUPERVISOR_RESIDENT_BYTES),
        &[
            (
                OsString::from("REFLEX_LEAN_OPTIMIZER_SUPERVISOR_CAPABILITY"),
                OsString::from(SUPERVISOR_CAPABILITY),
            ),
            (
                OsString::from("REFLEX_INTERNAL_PHASE_REPORT_PREFIX"),
                phase_report_prefix.into_os_string(),
            ),
        ],
    )?;
    if capture.status.success()
        && !capture.timed_out
        && !capture.resident_limit_exceeded
        && !capture.output_limit_exceeded
    {
        print!("{}", capture.stdout);
        return Ok(());
    }
    let failure = if capture.timed_out {
        "exceeded its 35-minute supervised wall limit"
    } else if capture.resident_limit_exceeded {
        "reached its 39.5-GiB supervised resident boundary"
    } else if capture.output_limit_exceeded {
        "exceeded its bounded diagnostic-output allowance"
    } else {
        "failed inside its hard host-isolated boundary"
    };
    Err(format!(
        "Lean public-optimizer development {failure}; reserved CPUs {:?}, preserved {} memory bytes; child stderr: {}",
        isolation.reserved_cpus,
        isolation.memory_reserve_bytes,
        capture.stderr.trim()
    )
    .into())
}

pub fn bundle_summary(arguments: &[String]) -> Result<(), AnyError> {
    let values = parse_flag_values(arguments, &["--bundle"], "lean-bundle-summary")?;
    let path = values
        .get("--bundle")
        .ok_or("lean-bundle-summary requires --bundle PATH")?;
    let bytes = std::fs::read(path)?;
    let decoded = CanonicalBundle::decode(&bytes, 1024 * 1024 * 1024)?;
    let session = inspect_session_segment(decoded.segment(SegmentKind::Session))?;
    let artifacts = decoded.segment(SegmentKind::Artifacts);
    if artifacts.len() < 8 {
        return Err("Lean Bundle has a truncated Artifact count".into());
    }
    let artifact_records = u64::from_le_bytes(artifacts[..8].try_into()?);
    let experience = experience_summary(decoded.segment(SegmentKind::Experience))?;
    let recovery = recovery_summary(decoded.segment(SegmentKind::Recovery))?;
    let revisions = decoded.segment(SegmentKind::Revisions);
    if revisions.len() < 64 {
        return Err("Lean Bundle has a truncated Revisions segment".into());
    }
    let mut state = &revisions[64..];
    take_summary_sized(&mut state)?;
    let learning = learning_header(take_summary_sized(&mut state)?)?;
    if !state.is_empty() {
        return Err("Lean Bundle has trailing Revisions state".into());
    }
    let segment_logical_bytes = [
        ("session", SegmentKind::Session),
        ("revisions", SegmentKind::Revisions),
        ("artifacts", SegmentKind::Artifacts),
        ("experience", SegmentKind::Experience),
        ("recovery", SegmentKind::Recovery),
    ]
    .into_iter()
    .map(|(segment, kind)| SegmentLogicalBytes {
        segment,
        bytes: decoded.segment(kind).len(),
    })
    .collect();
    let summary = BundleSummary {
        schema: "reflex-lean-bundle-summary-v3",
        identity: String::from_utf8(decoded.identity().to_vec())?,
        completed: session.completed,
        completion: session.completion.map(completion_name),
        requested: usage(session.requested),
        usage: usage(session.usage),
        artifact_records,
        experience_entries: experience.entries,
        experience_claims: experience.claims,
        accepted_claims: experience.accepted_claims,
        accepted_experience: experience.verdicts[0],
        refuted_experience: experience.verdicts[1],
        unknown_experience: experience.verdicts[2],
        candidate_fates: experience.candidate_fates,
        novelty_filtered_candidates: experience.novelty_filtered,
        policy_deferred_candidates: experience.policy_deferred,
        verification_interrupted_candidates: experience.verification_interrupted,
        verified_by_queue: experience.verified_by_queue,
        admitted_candidates: experience.admitted,
        candidate_fate_trace: experience.candidate_fate_trace,
        learning,
        knowledge_revision: hex(&revisions[..32]),
        model_revision: hex(&revisions[32..64]),
        durable_size_bound: CanonicalBundle::replacement_size_bound(
            &bytes,
            &[
                (
                    SegmentKind::Experience,
                    decoded.segment(SegmentKind::Experience).len() as u64,
                ),
                (
                    SegmentKind::Recovery,
                    decoded.segment(SegmentKind::Recovery).len() as u64,
                ),
            ],
        )?,
        recovery,
        segment_logical_bytes,
    };
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

pub fn feature_development(arguments: &[String]) -> Result<(), AnyError> {
    const SCHEMA: &str = "reflex-lean-model-feature-development-v15";
    require_release("lean-model-feature-development")?;
    let host = environment()?;
    require_clean(&host, SCHEMA)?;
    let values = parse_flag_values(
        arguments,
        &["--bundle", "--lake", "--december-root", "--output"],
        "lean-model-feature-development",
    )?;
    let bundle = values
        .get("--bundle")
        .map(PathBuf::from)
        .ok_or("lean-model-feature-development requires --bundle PATH")?;
    let lake = values
        .get("--lake")
        .map(PathBuf::from)
        .ok_or("lean-model-feature-development requires --lake PATH")?;
    let december_root = values
        .get("--december-root")
        .map(PathBuf::from)
        .ok_or("lean-model-feature-development requires --december-root PATH")?;
    let output = values
        .get("--output")
        .map(PathBuf::from)
        .ok_or("lean-model-feature-development requires --output PATH")?;
    require_absent(&output, "Lean model-feature Development report")?;
    let domain = LeanDomain::new(
        LeanWorkerConfig::pinned(lake, december_root),
        LeanCorpus::default(),
    )?;
    let request = request(
        LeanSeedScope {
            start: 0,
            count: 16,
        },
        1_024,
        BundlePlan::Resume {
            source: bundle.clone(),
            target: bundle.clone(),
        },
    )?;
    let comparison = compare_candidate_features(&domain, &request, &bundle)?;
    let collisions = substitution_information_audit(&bundle, None)?;
    let duration_ns = |duration: Duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
    let mut report = FeatureDevelopmentReport {
        schema: SCHEMA,
        status: "development-only; fixed retained Experience; no Verification requests; no 2026 exposure",
        retained_bundle_sha256: hash_file(&bundle)?,
        examples: comparison.examples,
        claim_operator_feature_groups: comparison.claim_operator_feature_groups,
        mixed_verdict_claim_operator_feature_groups: comparison
            .mixed_verdict_claim_operator_feature_groups,
        accepted_examples_in_mixed_groups: comparison.accepted_examples_in_mixed_groups,
        accepted_refuted_collision_groups: collisions.accepted_refuted_collision_groups,
        accepted_in_accepted_refuted_groups: collisions.accepted_in_accepted_refuted_groups,
        accepted_refuted_collision_fraction_ppm: collisions.accepted_refuted_collision_fraction_ppm,
        collision_examples_in_operational_top_k: collisions.collision_examples_in_operational_top_k,
        collision_accepted_in_operational_top_k: collisions.collision_accepted_in_operational_top_k,
        collision_groups: collisions.collision_groups,
        accepted_examples: comparison.accepted_examples,
        proposal_informed_examples: comparison.proposal_informed_examples,
        replay_claims: comparison.replay_claims,
        selection_claims: comparison.selection_claims,
        feature_count: comparison.feature_count,
        head_count: 7,
        baseline_selection_loss: comparison.baseline_selection_loss,
        structural_selection_loss: comparison.structural_selection_loss,
        balanced_structural_selection_loss: comparison.balanced_structural_selection_loss,
        baseline_training_cpu_ns: duration_ns(comparison.baseline_training_cpu),
        structural_training_cpu_ns: duration_ns(comparison.structural_training_cpu),
        balanced_structural_training_cpu_ns: duration_ns(
            comparison.balanced_structural_training_cpu,
        ),
        feature_extraction_cpu_ns: duration_ns(comparison.feature_extraction_cpu),
        baseline_model_revision: hex(&comparison.baseline_model_revision),
        structural_model_revision: hex(&comparison.structural_model_revision),
        balanced_structural_model_revision: hex(&comparison.balanced_structural_model_revision),
        model_bytes: comparison.model_bytes,
        baseline_reproduces_champion: comparison.baseline_reproduces_champion,
        structural_promotes_over_baseline: comparison.structural_promotes_over_baseline,
        ranking_budgets: comparison.ranking_budgets,
        bootstrap_accepted_at_k: comparison.bootstrap_accepted_at_k,
        baseline_accepted_at_k: comparison.baseline_accepted_at_k,
        structural_accepted_at_k: comparison.structural_accepted_at_k,
        balanced_structural_accepted_at_k: comparison.balanced_structural_accepted_at_k,
        global_bootstrap_accepted_at_k: comparison.global_bootstrap_accepted_at_k,
        global_baseline_accepted_at_k: comparison.global_baseline_accepted_at_k,
        global_structural_accepted_at_k: comparison.global_structural_accepted_at_k,
        global_balanced_structural_accepted_at_k: comparison
            .global_balanced_structural_accepted_at_k,
        evaluated_at_k: comparison.evaluated_at_k,
        selection_accepted: comparison.selection_accepted,
        host,
        content_sha256: String::new(),
    };
    write_feature_report(&output, &mut report)
}

fn write_feature_report(
    output: &Path,
    report: &mut FeatureDevelopmentReport,
) -> Result<(), AnyError> {
    report.content_sha256 = hash_json(report)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, serde_json::to_vec_pretty(report)?)?;
    println!("{}", serde_json::to_string(report)?);
    Ok(())
}

fn fraction_ppm(numerator: usize, denominator: usize) -> Result<u64, AnyError> {
    Ok(u64::try_from(numerator)?
        .saturating_mul(1_000_000)
        .checked_div(u64::try_from(denominator)?)
        .unwrap_or(0))
}

pub fn development_child(arguments: &[String]) -> Result<(), AnyError> {
    if std::env::var("REFLEX_LEAN_OPTIMIZER_SUPERVISOR_CAPABILITY").as_deref()
        != Ok(SUPERVISOR_CAPABILITY)
    {
        return Err(
            "Lean public-optimizer child execution requires its host-isolated supervisor".into(),
        );
    }
    require_supervising_parent()?;
    let isolation = inherited_host_isolation()?;
    development_once(arguments, isolation)
}

fn development_once(arguments: &[String], isolation: HostIsolation) -> Result<(), AnyError> {
    require_release("lean-public-optimizer-development-child")?;
    let host = environment()?;
    require_clean(&host, DEVELOPMENT_SCHEMA)?;
    let arguments = parse(arguments)?;
    require_absent(
        &arguments.output,
        "Lean public-optimizer development report",
    )?;
    if arguments.work.exists() {
        return Err(format!(
            "Lean public-optimizer work directory already exists: {}",
            arguments.work.display()
        )
        .into());
    }
    std::fs::create_dir_all(&arguments.work)?;

    let prepared = prepare_corpus(&arguments)?;
    if arguments.preflight_only {
        return write_preflight_report(&arguments, prepared, host, isolation);
    }
    let inputs = run_treatments(&arguments, prepared, host, isolation)?;
    write_report(&arguments, inputs)
}

fn run_treatments(
    arguments: &Arguments,
    prepared: DevelopmentCorpus,
    host: HostEnvironment,
    isolation: HostIsolation,
) -> Result<ReportInputs, AnyError> {
    let training_bundle = arguments.work.join("training.bundle");
    let bootstrap_template_bundle = arguments.work.join("bootstrap-template.bundle");
    let full_bundle = arguments.work.join("full.bundle");
    let no_model_seed = arguments.work.join("no-model-seed.bundle");
    let no_model_bundle = arguments.work.join("no-model.bundle");
    let no_derived_seed = arguments.work.join("no-derived-seed.bundle");
    let no_derived_bundle = arguments.work.join("no-derived.bundle");
    let bootstrap_bundle = arguments.work.join("bootstrap.bundle");
    let (training_domain_resources, heldout_domain_resources) =
        development_domain_resources(&prepared)?;
    build_bootstrap_template(
        &prepared.config,
        prepared.training_corpus.clone(),
        prepared.training,
        &bootstrap_template_bundle,
    )?;
    let training_usage = run_training(
        &prepared.config,
        prepared.training_corpus.clone(),
        prepared.training,
        arguments.training_verification_requests,
        &training_bundle,
    )?;
    let training_learning = learning_summary(&training_bundle)?;
    if !training_learning.champion_present {
        return Err(format!(
            "Lean training produced no promoted Model Revision after {} Artifacts and {} Verification requests; a Full versus no-model treatment would be a placebo",
            prepared.training, training_usage.verification_requests
        )
        .into());
    }
    let phase_zero_information_audit = information_audit_for_training(
        &prepared.config,
        &prepared.training_corpus,
        &training_bundle,
    )?;
    let bootstrap = run_fresh_treatment(
        "bootstrap",
        &prepared.config,
        &prepared.heldout_corpus,
        prepared.heldout,
        arguments.verification_requests,
        &prepared.heldout_seed_nodes,
        &bootstrap_bundle,
    )?;
    ablate_lean_bundle(
        &training_bundle,
        &no_model_seed,
        Some(&bootstrap_template_bundle),
        false,
    )?;
    ablate_lean_bundle(&training_bundle, &no_derived_seed, None, true)?;
    let full = run_forked_treatment(
        "full",
        &prepared.config,
        &prepared.heldout_corpus,
        prepared.heldout,
        arguments.verification_requests,
        &training_bundle,
        &full_bundle,
        &prepared.heldout_seed_nodes,
    )?;
    let no_model = run_forked_treatment(
        "no-model",
        &prepared.config,
        &prepared.heldout_corpus,
        prepared.heldout,
        arguments.verification_requests,
        &no_model_seed,
        &no_model_bundle,
        &prepared.heldout_seed_nodes,
    )?;
    let no_derived = run_forked_treatment(
        "no-derived",
        &prepared.config,
        &prepared.heldout_corpus,
        prepared.heldout,
        arguments.verification_requests,
        &no_derived_seed,
        &no_derived_bundle,
        &prepared.heldout_seed_nodes,
    )?;
    Ok(ReportInputs {
        host,
        host_isolation: isolation,
        training_artifacts: prepared.training,
        heldout_artifacts: prepared.heldout,
        september_catalog_sha256: prepared.september_catalog_sha256,
        december_catalog_sha256: prepared.december_catalog_sha256,
        selected_artifacts: prepared.selected_artifacts,
        training_usage,
        training_learning,
        training_domain_resources,
        heldout_domain_resources,
        phase_zero_information_audit,
        full,
        no_model,
        no_derived,
        bootstrap,
    })
}

fn ablate_lean_bundle(
    source: &Path,
    target: &Path,
    model_template: Option<&Path>,
    derived: bool,
) -> Result<(), AnyError> {
    crate::causal::ablate_bundle(
        source,
        target,
        model_template,
        derived,
        RUNTIME_RESIDENT_BYTES,
    )
}

fn development_domain_resources(
    prepared: &DevelopmentCorpus,
) -> Result<(DomainResources, DomainResources), AnyError> {
    Ok((
        domain_resources(LeanDomain::new(
            prepared.config.clone(),
            prepared.training_corpus.clone(),
        )?)?,
        domain_resources(LeanDomain::new(
            prepared.config.clone(),
            prepared.heldout_corpus.clone(),
        )?)?,
    ))
}

fn domain_resources(domain: LeanDomain) -> Result<DomainResources, AnyError> {
    let inspection = inspect_domain_resources(&domain, WORKER_THREADS)
        .ok_or("Lean Domain cannot satisfy the registered worker allocation")?;
    let resources = DomainResources {
        requested_worker_threads: inspection.requested_worker_threads,
        external_worker_lanes: inspection.external_worker_lanes,
        runtime_worker_lanes: inspection.runtime_worker_lanes,
        runtime_stack_bytes: inspection.runtime_stack_bytes,
        durability_stack_bytes: inspection.durability_stack_bytes,
        external_worker_bytes: inspection.external_worker_bytes,
        operator_bytes: inspection.operator_bytes,
        maximum_candidate_capacity: inspection.maximum_candidate_capacity,
        operator_scratch_bytes_per_lane: inspection.operator_scratch_bytes_per_lane,
        operator_scratch_bytes: inspection.operator_scratch_bytes,
        fixed_resident_bytes: inspection.fixed_resident_bytes,
        dynamic_headroom_bytes: RUNTIME_RESIDENT_BYTES
            .saturating_sub(inspection.fixed_resident_bytes),
    };
    drop(domain);
    Ok(resources)
}

fn write_preflight_report(
    arguments: &Arguments,
    prepared: DevelopmentCorpus,
    host: HostEnvironment,
    host_isolation: HostIsolation,
) -> Result<(), AnyError> {
    let training_domain_resources = domain_resources(LeanDomain::new(
        prepared.config.clone(),
        prepared.training_corpus,
    )?)?;
    let heldout_domain_resources =
        domain_resources(LeanDomain::new(prepared.config, prepared.heldout_corpus)?)?;
    let mut report = PreflightReport {
        schema: "reflex-lean-public-optimizer-preflight-v1",
        status: "development-only; corpus prepared and kernel-replayed; no Improvement Session",
        training_artifacts: prepared.training,
        heldout_artifacts: prepared.heldout,
        selected_artifacts: prepared.selected_artifacts,
        runtime_resident_limit_bytes: RUNTIME_RESIDENT_BYTES,
        training_domain_resources,
        heldout_domain_resources,
        host,
        host_isolation,
        content_sha256: String::new(),
    };
    report.content_sha256 = hash_json(&report)?;
    if let Some(parent) = arguments.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&arguments.output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn build_bootstrap_template(
    config: &LeanWorkerConfig,
    corpus: LeanCorpus,
    training: usize,
    target: &Path,
) -> Result<(), AnyError> {
    finish_training(improve(
        LeanDomain::new(config.clone(), corpus)?,
        request(
            LeanSeedScope {
                start: 0,
                count: training,
            },
            u64::try_from(training).unwrap_or(u64::MAX),
            BundlePlan::Fresh {
                target: target.to_path_buf(),
            },
        )?,
        |_| ControlFlow::Continue(()),
    )?);
    Ok(())
}

fn run_training(
    config: &LeanWorkerConfig,
    corpus: LeanCorpus,
    training: usize,
    verification_requests: u64,
    target: &Path,
) -> Result<Usage, AnyError> {
    Ok(finish_training(improve(
        LeanDomain::new(config.clone(), corpus)?,
        request(
            LeanSeedScope {
                start: 0,
                count: training,
            },
            verification_requests,
            BundlePlan::Fresh {
                target: target.to_path_buf(),
            },
        )?,
        |_| ControlFlow::Continue(()),
    )?))
}

fn write_report(arguments: &Arguments, inputs: ReportInputs) -> Result<(), AnyError> {
    let ReportInputs {
        host,
        host_isolation,
        training_artifacts,
        heldout_artifacts,
        september_catalog_sha256,
        december_catalog_sha256,
        selected_artifacts,
        training_usage,
        training_learning,
        training_domain_resources,
        heldout_domain_resources,
        phase_zero_information_audit,
        full,
        no_model,
        no_derived,
        bootstrap,
    } = inputs;
    let full_has_more_improvements =
        full.strict_proof_node_improvements > bootstrap.strict_proof_node_improvements;
    let full_uses_less_end_to_end_cpu_per_improvement = nonzero_ratio_is_strictly_less(
        full.usage.cpu_ns,
        full.strict_proof_node_improvements,
        bootstrap.usage.cpu_ns,
        bootstrap.strict_proof_node_improvements,
    )
    .unwrap_or(false);
    let request_16_candidate_efficiency = discovery_efficiency_at(
        16,
        &full.candidate_verification_curve,
        &bootstrap.candidate_verification_curve,
    );
    let comparable_prefixes = full
        .candidate_verification_curve
        .iter()
        .zip(&bootstrap.candidate_verification_curve)
        .filter(|(full, bootstrap)| full.request_budget >= 16 && full.reached && bootstrap.reached)
        .collect::<Vec<_>>();
    let full_noninferior_after_16_requests = !comparable_prefixes.is_empty()
        && comparable_prefixes
            .iter()
            .all(|(full, bootstrap)| full.strict_discoveries >= bootstrap.strict_discoveries);
    let mut report = Report {
        schema: DEVELOPMENT_SCHEMA,
        status: "development-only; no 2026 exposure",
        training_artifacts,
        heldout_artifacts,
        training_verification_limit: arguments.training_verification_requests,
        evaluation_verification_limit: arguments.verification_requests,
        primary_proof_node_limit: PRIMARY_PROOF_NODE_LIMIT,
        primary_selection: "human-facing declarations with <=100000 proof nodes, distinct statement fingerprints, and a kernel-accepted strictly shorter pre-2025 library proof; held-out names are absent from September and held-out statement fingerprints are disjoint from selected training; development opportunity corpus, not confirmation sampling",
        september_catalog_sha256,
        december_catalog_sha256,
        selected_artifacts,
        training_usage,
        training_learning,
        training_domain_resources,
        heldout_domain_resources,
        phase_zero_information_audit,
        full,
        no_model,
        no_derived,
        bootstrap,
        full_has_more_improvements,
        full_uses_less_end_to_end_cpu_per_improvement,
        request_16_candidate_efficiency,
        full_noninferior_after_16_requests,
        host,
        host_isolation,
        content_sha256: String::new(),
    };
    report.content_sha256 = hash_json(&report)?;
    if let Some(parent) = arguments.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&arguments.output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn nonzero_ratio_is_strictly_less(
    left_numerator: u64,
    left_denominator: usize,
    right_numerator: u64,
    right_denominator: usize,
) -> Option<bool> {
    (left_denominator > 0 && right_denominator > 0).then(|| {
        ratio_is_strictly_less(
            left_numerator,
            left_denominator as u64,
            right_numerator,
            right_denominator as u64,
        )
    })
}

fn ratio_is_strictly_less(
    left_numerator: u64,
    left_denominator: u64,
    right_numerator: u64,
    right_denominator: u64,
) -> bool {
    debug_assert!(left_denominator > 0 && right_denominator > 0);
    u128::from(left_numerator) * u128::from(right_denominator)
        < u128::from(right_numerator) * u128::from(left_denominator)
}

fn discovery_efficiency_at(
    request_budget: usize,
    full: &[VerificationCurvePoint],
    bootstrap: &[VerificationCurvePoint],
) -> Option<DiscoveryEfficiencyComparison> {
    let full = full
        .iter()
        .find(|point| point.request_budget == request_budget && point.reached)?;
    let bootstrap = bootstrap
        .iter()
        .find(|point| point.request_budget == request_budget && point.reached)?;
    Some(DiscoveryEfficiencyComparison {
        request_budget,
        full_strict_discoveries: full.strict_discoveries,
        bootstrap_strict_discoveries: bootstrap.strict_discoveries,
        full_candidate_cpu_ns: full.cpu_ns_through_completed_batch,
        bootstrap_candidate_cpu_ns: bootstrap.cpu_ns_through_completed_batch,
        full_uses_less_candidate_cpu_per_improvement: nonzero_ratio_is_strictly_less(
            full.cpu_ns_through_completed_batch,
            full.strict_discoveries,
            bootstrap.cpu_ns_through_completed_batch,
            bootstrap.strict_discoveries,
        )
        .unwrap_or(false),
    })
}

fn prepare_corpus(arguments: &Arguments) -> Result<DevelopmentCorpus, AnyError> {
    let september_catalog = LeanCatalog::load(&arguments.september_catalog)?;
    let december_catalog = LeanCatalog::load(&arguments.december_catalog)?;
    let september_catalog_sha256 = september_catalog.content_sha256().to_owned();
    let december_catalog_sha256 = december_catalog.content_sha256().to_owned();
    let september_artifacts =
        TemporalSnapshot::from_catalog(&september_catalog).forecast_artifacts();
    let december_artifacts = TemporalSnapshot::from_catalog(&december_catalog).forecast_artifacts();
    let (earlier_names, training_pool, heldout_pool) =
        selection_pools(&september_artifacts, &december_artifacts, arguments);
    let config = LeanWorkerConfig::pinned(&arguments.lake, &arguments.december_root);
    let worker = LeanWorker::start(&config)?;
    let training_candidates = fetch_primary(&worker, &training_pool, training_pool.len())?;
    let heldout_candidates = fetch_primary(&worker, &heldout_pool, heldout_pool.len())?;
    let selected_names = training_candidates
        .iter()
        .chain(&heldout_candidates)
        .map(|selected| selected.theorem.name.clone())
        .collect::<HashSet<_>>();
    let training_library_candidates = fetch_library(
        &worker,
        &december_artifacts,
        &training_candidates,
        &selected_names,
        |artifact| earlier_names.contains(&artifact.declaration),
    )?;
    let heldout_library_candidates = fetch_library(
        &worker,
        &december_artifacts,
        &heldout_candidates,
        &selected_names,
        |_| true,
    )?;
    let training = select_verified_improvements(
        &worker,
        &training_candidates,
        &training_library_candidates,
        arguments.training_artifacts,
    )?;
    let training_statements = training
        .iter()
        .map(|seed| seed.example.statement_hash)
        .collect::<HashSet<_>>();
    let heldout_candidates = heldout_candidates
        .into_iter()
        .filter(|seed| !training_statements.contains(&seed.example.statement_hash))
        .collect::<Vec<_>>();
    let heldout = select_verified_improvements(
        &worker,
        &heldout_candidates,
        &heldout_library_candidates,
        arguments.heldout_artifacts,
    )?;
    let training_library = library_for(&training, training_library_candidates);
    let heldout_library = library_for(&heldout, heldout_library_candidates);
    let training_count = training.len();
    let heldout_count = heldout.len();

    let (training_corpus, heldout_corpus) = build_corpora(
        &worker,
        &training,
        &heldout,
        &training_library,
        &heldout_library,
    )?;
    drop(worker);
    let heldout_seed_nodes = heldout
        .iter()
        .map(|selected| {
            (
                selected.theorem.name.to_string(),
                selected.theorem.proof_term.node_count(),
            )
        })
        .collect();
    let selected_artifacts =
        selected_artifacts(&training, &heldout, &training_library, &heldout_library);
    Ok(DevelopmentCorpus {
        config,
        training_corpus,
        heldout_corpus,
        training: training_count,
        heldout: heldout_count,
        september_catalog_sha256,
        december_catalog_sha256,
        heldout_seed_nodes,
        selected_artifacts,
    })
}

fn selection_pools<'a>(
    september: &[TemporalExample],
    december: &'a [TemporalExample],
    arguments: &Arguments,
) -> (
    HashSet<reflex_lean::ast::LeanName>,
    Vec<&'a TemporalExample>,
    Vec<&'a TemporalExample>,
) {
    let earlier_names = september
        .iter()
        .map(|artifact| artifact.declaration.clone())
        .collect::<HashSet<_>>();
    let earlier_families = september
        .iter()
        .map(|artifact| artifact.semantic_group)
        .collect::<HashSet<_>>();
    let earlier_statement_counts = statement_counts(
        december
            .iter()
            .filter(|artifact| earlier_names.contains(&artifact.declaration)),
    );
    let december_statement_counts = statement_counts(december.iter());
    let training = deterministic_prefix(
        december.iter().filter(|artifact| {
            earlier_names.contains(&artifact.declaration)
                && is_human_facing(&artifact.declaration)
                && earlier_statement_counts
                    .get(&artifact.statement_hash)
                    .is_some_and(|count| *count >= 2)
        }),
        arguments
            .training_artifacts
            .saturating_mul(SELECTION_POOL_MULTIPLIER),
    );
    let heldout = deterministic_prefix(
        december.iter().filter(|artifact| {
            !earlier_names.contains(&artifact.declaration)
                && !earlier_families.contains(&artifact.semantic_group)
                && is_human_facing(&artifact.declaration)
                && december_statement_counts
                    .get(&artifact.statement_hash)
                    .is_some_and(|count| *count >= 2)
        }),
        arguments
            .heldout_artifacts
            .saturating_mul(SELECTION_POOL_MULTIPLIER),
    );
    (earlier_names, training, heldout)
}

fn build_corpora(
    worker: &LeanWorker,
    training: &[SelectedTheorem],
    heldout: &[SelectedTheorem],
    training_library: &[SelectedTheorem],
    heldout_library: &[SelectedTheorem],
) -> Result<(LeanCorpus, LeanCorpus), AnyError> {
    let seeds = |selected: &[SelectedTheorem]| {
        selected
            .iter()
            .map(|selected| selected.theorem.clone())
            .collect::<Vec<_>>()
    };
    let shared_library = seeds(training)
        .into_iter()
        .chain(seeds(training_library))
        .collect::<Vec<_>>();
    let training_corpus =
        LeanCorpus::verified_seeds_with_library(worker, seeds(training), shared_library.clone())?;
    let heldout_corpus = LeanCorpus::verified_seeds_with_library(
        worker,
        seeds(heldout),
        shared_library
            .into_iter()
            .chain(seeds(heldout_library))
            .collect(),
    )?;
    Ok((training_corpus, heldout_corpus))
}

fn statement_counts<'a>(
    artifacts: impl Iterator<Item = &'a TemporalExample>,
) -> HashMap<u64, usize> {
    let mut counts = HashMap::new();
    for artifact in artifacts {
        *counts.entry(artifact.statement_hash).or_default() += 1;
    }
    counts
}

fn deterministic_prefix<'a>(
    artifacts: impl Iterator<Item = &'a TemporalExample>,
    count: usize,
) -> Vec<&'a TemporalExample> {
    let mut artifacts = artifacts.collect::<Vec<_>>();
    artifacts.sort_unstable_by(|left, right| {
        (&left.semantic_group, &left.declaration).cmp(&(&right.semantic_group, &right.declaration))
    });
    let mut statements = HashSet::new();
    artifacts
        .into_iter()
        .filter(|artifact| statements.insert(artifact.statement_hash))
        .take(count)
        .collect()
}

fn fetch_primary(
    worker: &LeanWorker,
    pool: &[&TemporalExample],
    count: usize,
) -> Result<Vec<SelectedTheorem>, AnyError> {
    let names = pool
        .iter()
        .map(|artifact| artifact.declaration.clone())
        .collect::<Vec<_>>();
    let mut by_name = worker
        .fetch(&names)?
        .into_iter()
        .map(|theorem| (theorem.name.clone(), theorem))
        .collect::<HashMap<_, _>>();
    Ok(pool
        .iter()
        .filter_map(|example| {
            let theorem = by_name.remove(&example.declaration)?;
            (theorem.proof_term.node_count() <= PRIMARY_PROOF_NODE_LIMIT).then(|| SelectedTheorem {
                example: (*example).clone(),
                theorem,
            })
        })
        .take(count)
        .collect())
}

fn fetch_library(
    worker: &LeanWorker,
    available: &[TemporalExample],
    seeds: &[SelectedTheorem],
    excluded_names: &HashSet<reflex_lean::ast::LeanName>,
    eligible: impl Fn(&TemporalExample) -> bool,
) -> Result<Vec<SelectedTheorem>, AnyError> {
    let mut candidates = Vec::new();
    for seed in seeds {
        let mut alternatives = available
            .iter()
            .filter(|artifact| {
                artifact.statement_hash == seed.example.statement_hash
                    && !excluded_names.contains(&artifact.declaration)
                    && eligible(artifact)
            })
            .collect::<Vec<_>>();
        alternatives.sort_unstable_by(|left, right| left.declaration.cmp(&right.declaration));
        candidates.extend(alternatives.into_iter().take(LIBRARY_CANDIDATES_PER_SEED));
    }
    candidates.sort_unstable_by(|left, right| left.declaration.cmp(&right.declaration));
    candidates.dedup_by(|left, right| left.declaration == right.declaration);
    let fetched = fetch_primary(worker, &candidates, candidates.len())?;
    Ok(fetched)
}

fn select_verified_improvements(
    worker: &LeanWorker,
    candidates: &[SelectedTheorem],
    library: &[SelectedTheorem],
    count: usize,
) -> Result<Vec<SelectedTheorem>, AnyError> {
    let mut selected = Vec::with_capacity(count);
    for seed in candidates {
        let mut accepted = false;
        for alternative in library.iter().filter(|alternative| {
            alternative.example.statement_hash == seed.example.statement_hash
                && alternative.theorem.proof_term.node_count()
                    < seed.theorem.proof_term.node_count()
        }) {
            let (results, _) = worker.verify(&[VerificationItem {
                level_params: seed.theorem.level_params.clone(),
                claim_proposition: seed.theorem.proposition.clone(),
                candidate_proposition: seed.theorem.proposition.clone(),
                proof_term: alternative.theorem.proof_term.clone(),
                allowed_axioms: seed.theorem.axioms.clone(),
            }])?;
            if results[0].accepted {
                accepted = true;
                break;
            }
        }
        if accepted {
            selected.push(SelectedTheorem {
                example: seed.example.clone(),
                theorem: seed.theorem.clone(),
            });
            if selected.len() == count {
                break;
            }
        }
    }
    if selected.len() != count {
        return Err(format!(
            "Lean development found {} kernel-accepted primary improvements, required {count}",
            selected.len()
        )
        .into());
    }
    Ok(selected)
}

fn library_for(seeds: &[SelectedTheorem], library: Vec<SelectedTheorem>) -> Vec<SelectedTheorem> {
    let statements = seeds
        .iter()
        .map(|seed| seed.example.statement_hash)
        .collect::<HashSet<_>>();
    library
        .into_iter()
        .filter(|artifact| statements.contains(&artifact.example.statement_hash))
        .collect()
}

fn is_human_facing(name: &reflex_lean::ast::LeanName) -> bool {
    match name {
        reflex_lean::ast::LeanName::Anonymous => true,
        reflex_lean::ast::LeanName::Num { .. } => false,
        reflex_lean::ast::LeanName::Str { parent, value } => {
            !value.starts_with('_')
                && !value.strip_prefix("proof_").is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
                })
                && is_human_facing(parent)
        }
    }
}

fn selected_artifacts(
    training: &[SelectedTheorem],
    heldout: &[SelectedTheorem],
    training_library: &[SelectedTheorem],
    heldout_library: &[SelectedTheorem],
) -> Vec<SelectedArtifact> {
    let mut selected = Vec::new();
    for (role, artifacts) in [
        ("training", training),
        ("heldout", heldout),
        ("training-library", training_library),
        ("heldout-library", heldout_library),
    ] {
        selected.extend(artifacts.iter().map(|artifact| SelectedArtifact {
            role,
            declaration: artifact.theorem.name.to_string(),
            statement_hash: artifact.example.statement_hash,
            proof_nodes: artifact.theorem.proof_term.node_count(),
        }));
    }
    selected
}

fn request(
    seeds: LeanSeedScope,
    verification_requests: u64,
    bundle: BundlePlan,
) -> Result<ImprovementRequest<LeanDomain>, AnyError> {
    let goals = [
        LeanMetric::ProofNodes,
        LeanMetric::ProofDepth,
        LeanMetric::EncodedBytes,
        LeanMetric::AllowedAxiomCount,
    ]
    .map(|metric| {
        OptimizationGoal::new(
            [],
            NonEmpty::one(Objective::new(metric, Direction::Minimize)),
            Preference::tiered(NonEmpty::one(NonEmpty::one(metric)), [])?,
            None,
        )
    })
    .into_iter()
    .collect::<Result<Vec<_>, _>>()?;
    Ok(ImprovementRequest::new(
        GoalSet::try_from_iter(goals).map_err(|_| "Lean development goals cannot be empty")?,
        seeds,
        ResourceEnvelope::new(
            NonZeroUsize::new(WORKER_THREADS).ok_or("worker lanes must be nonzero")?,
            NonZeroU64::new(RUNTIME_RESIDENT_BYTES).ok_or("resident limit must be nonzero")?,
            NonZeroU64::new(DURABLE_BYTES).ok_or("durable limit must be nonzero")?,
            NonZeroDuration::new(Duration::from_mins(10)).ok_or("elapsed limit must be nonzero")?,
            NonZeroDuration::new(Duration::from_mins(10)).ok_or("CPU limit must be nonzero")?,
            NonZeroU64::new(verification_requests)
                .ok_or("verification requests must be nonzero")?,
        ),
        bundle,
    )?)
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
        return Err("Lean public-optimizer child parent is not its registered supervisor".into());
    }
    let command = std::fs::read(format!("/proc/{parent}/cmdline"))?;
    let arguments = command
        .split(|byte| *byte == 0)
        .filter_map(|value| std::str::from_utf8(value).ok())
        .collect::<Vec<_>>();
    if !arguments.contains(&"lean-public-optimizer-development")
        || arguments.contains(&"lean-public-optimizer-development-child")
    {
        return Err(
            "Lean public-optimizer child parent has no registered supervisor command".into(),
        );
    }
    Ok(())
}

fn run_fresh_treatment(
    name: &'static str,
    config: &LeanWorkerConfig,
    corpus: &LeanCorpus,
    heldout: usize,
    verification_requests: u64,
    seed_nodes: &HashMap<String, usize>,
    target: &Path,
) -> Result<TreatmentResult, AnyError> {
    let mut strict_improvements = Vec::new();
    let outcome = improve(
        LeanDomain::new(config.clone(), corpus.clone())?,
        request(
            LeanSeedScope {
                start: 0,
                count: heldout,
            },
            verification_requests,
            BundlePlan::Fresh {
                target: target.to_path_buf(),
            },
        )?,
        |update| {
            record_strict_improvements(&update, seed_nodes, &mut strict_improvements);
            ControlFlow::Continue(())
        },
    )?;
    finish_treatment(name, outcome, seed_nodes, strict_improvements, target, 0)
}

#[expect(
    clippy::too_many_arguments,
    reason = "a resumed causal treatment also binds the immutable source Bundle"
)]
fn run_forked_treatment(
    name: &'static str,
    config: &LeanWorkerConfig,
    corpus: &LeanCorpus,
    heldout: usize,
    verification_requests: u64,
    source: &Path,
    target: &Path,
    seed_nodes: &HashMap<String, usize>,
) -> Result<TreatmentResult, AnyError> {
    let fate_offset = bundle_candidate_fate_count(source)?;
    let mut strict_improvements = Vec::new();
    let outcome = improve(
        LeanDomain::new(config.clone(), corpus.clone())?,
        request(
            LeanSeedScope {
                start: 0,
                count: heldout,
            },
            verification_requests,
            BundlePlan::Fork {
                source: source.to_path_buf(),
                target: target.to_path_buf(),
            },
        )?,
        |update| {
            record_strict_improvements(&update, seed_nodes, &mut strict_improvements);
            ControlFlow::Continue(())
        },
    )?;
    finish_treatment(
        name,
        outcome,
        seed_nodes,
        strict_improvements,
        target,
        fate_offset,
    )
}

fn finish_training(outcome: reflex::SessionOutcome<LeanDomain>) -> Usage {
    let summary = usage(outcome.usage());
    drop(outcome);
    summary
}

fn finish_treatment(
    name: &'static str,
    outcome: reflex::SessionOutcome<LeanDomain>,
    seed_nodes: &HashMap<String, usize>,
    strict_improvements: Vec<StrictImprovement>,
    bundle: &Path,
    fate_offset: usize,
) -> Result<TreatmentResult, AnyError> {
    let heldout = outcome
        .pareto()
        .artifacts()
        .iter()
        .filter_map(|artifact| {
            let declaration = artifact.artifact().declaration.name.to_string();
            seed_nodes
                .get(&declaration)
                .map(|seed| (*seed, artifact.artifact().proof_term.node_count()))
        })
        .collect::<Vec<_>>();
    let summary = TreatmentResult {
        treatment: name,
        completion: format!("{:?}", outcome.completion()),
        heldout_pareto_artifacts: heldout.len(),
        strict_proof_node_improvements: heldout
            .iter()
            .filter(|(seed, artifact)| artifact < seed)
            .count(),
        proof_nodes_removed: heldout
            .iter()
            .map(|(seed, artifact)| seed.saturating_sub(*artifact))
            .sum(),
        strict_improvements,
        candidate_verification_curve: candidate_verification_curve(bundle, fate_offset)?,
        usage: usage(outcome.usage()),
        bundle_sha256: hash_file(bundle)?,
    };
    drop(outcome);
    Ok(summary)
}

fn bundle_candidate_fate_count(bundle: &Path) -> Result<usize, AnyError> {
    let bytes = std::fs::read(bundle)?;
    let decoded = CanonicalBundle::decode(&bytes, RUNTIME_RESIDENT_BYTES)?;
    Ok(
        inspect_experience_segment(decoded.segment(SegmentKind::Experience))
            .map_err(|_| "Lean Bundle has malformed Experience framing")?
            .candidate_fates
            .len(),
    )
}

fn substitution_information_audit(
    bundle: &Path,
    donor_context: Option<(&LeanDomain, &[LeanArtifact])>,
) -> Result<DonorAudit, AnyError> {
    let mut donors_by_proof = HashMap::<[u8; 32], Vec<String>>::new();
    let mut donors_by_support = HashMap::<[u8; 32], Vec<String>>::new();
    if let Some((_, library)) = donor_context {
        for artifact in library {
            donors_by_support
                .entry(Sha256::digest(serde_json::to_vec(artifact)?).into())
                .or_default()
                .push(artifact.declaration.name.to_string());
            donors_by_proof
                .entry(proof_term_digest(&artifact.proof_term)?)
                .or_default()
                .push(artifact.declaration.name.to_string());
        }
    }
    for declarations in donors_by_proof.values_mut() {
        declarations.sort_unstable();
        declarations.dedup();
    }
    for declarations in donors_by_support.values_mut() {
        declarations.sort_unstable();
        declarations.dedup();
    }
    let bytes = std::fs::read(bundle)?;
    let decoded = CanonicalBundle::decode(&bytes, RUNTIME_RESIDENT_BYTES)?;
    let experience = inspect_experience_segment(decoded.segment(SegmentKind::Experience))
        .map_err(|_| "Lean Bundle has malformed Experience framing")?;
    let mut verified_fates = HashMap::new();
    for fate in &experience.candidate_fates {
        let CandidateFateOutcomeInspection::Verified {
            strict_improvement, ..
        } = fate.outcome
        else {
            continue;
        };
        let rank = fate
            .policy_rank
            .ok_or("Verified Candidate Fate has no operational policy rank")?;
        if verified_fates
            .insert(
                (fate.candidate_key, fate.claim_digest, fate.epoch),
                (rank, strict_improvement),
            )
            .is_some()
        {
            return Err("Lean Candidate Fate identity is not unique".into());
        }
    }
    let mut scratch = <LeanStructure as StructuralProtocol<LeanDomain>>::Scratch::default();
    let mut traces = Vec::new();
    for attempt in experience
        .attempts
        .iter()
        .filter(|attempt| attempt.operator_symbol == b"lean-proof-substitution")
    {
        let (operational_policy_rank, strict_improvement) = verified_fates
            .get(&(attempt.candidate_key, attempt.claim_digest, attempt.epoch))
            .copied()
            .ok_or("Lean proof-substitution attempt has no matching Verified Candidate Fate")?;
        let proof_digest = if let Some((domain, _)) = donor_context {
            let candidate = domain
                .structure()
                .decode_canonical(&attempt.canonical_candidate, &mut scratch)
                .map_err(|_| "Lean Candidate Experience has malformed canonical structure")?;
            let proof_digest = proof_term_digest(&candidate.proof_term)?;
            Some(proof_digest)
        } else {
            None
        };
        let donor_declarations = attempt.support_key.map_or_else(
            || {
                proof_digest.map_or_else(Vec::new, |digest| {
                    donors_by_proof.get(&digest).cloned().unwrap_or_default()
                })
            },
            |support_key| {
                donors_by_support
                    .get(&support_key)
                    .cloned()
                    .unwrap_or_default()
            },
        );
        traces.push(DonorTrace {
            candidate_key: hex(&attempt.candidate_key),
            claim_digest: hex(&attempt.claim_digest),
            epoch: attempt.epoch,
            verdict: match attempt.verdict {
                ExperienceVerdictInspection::Accepted => "accepted",
                ExperienceVerdictInspection::Refuted => "refuted",
                ExperienceVerdictInspection::Unknown => "unknown",
            },
            operational_policy_rank,
            strict_improvement,
            feature_bits: attempt.feature_bits.clone(),
            support_key_sha256: attempt.support_key.as_ref().map(|key| hex(key)),
            proof_term_sha256: proof_digest.as_ref().map(|digest| hex(digest)),
            donor_declarations,
        });
    }
    summarize_substitution_information(traces)
}

fn summarize_substitution_information(traces: Vec<DonorTrace>) -> Result<DonorAudit, AnyError> {
    const OPERATIONAL_BUDGETS: [u32; 7] = [1, 4, 8, 16, 32, 64, 128];

    let accepted_proof_substitutions = traces
        .iter()
        .filter(|trace| trace.verdict == "accepted")
        .count();
    let collision_groups = accepted_refuted_collision_groups(&traces);
    let accepted_in_accepted_refuted_groups = collision_groups
        .iter()
        .flat_map(|group| &group.members)
        .filter(|member| member.verdict == "accepted")
        .count();
    let (collision_examples_in_operational_top_k, collision_accepted_in_operational_top_k) =
        operational_collision_occupancy(&collision_groups, OPERATIONAL_BUDGETS);
    let reconstructed_attempts = traces
        .iter()
        .filter(|trace| trace.proof_term_sha256.is_some() && !trace.donor_declarations.is_empty())
        .count();
    let ambiguous_attempts = traces
        .iter()
        .filter(|trace| trace.donor_declarations.len() > 1)
        .count();
    let accepted_refuted_collision_fraction_ppm = fraction_ppm(
        accepted_in_accepted_refuted_groups,
        accepted_proof_substitutions,
    )?;
    Ok(DonorAudit {
        proof_substitution_attempts: traces.len(),
        accepted_proof_substitutions,
        reconstructed_attempts,
        ambiguous_attempts,
        unmatched_attempts: traces.len().saturating_sub(reconstructed_attempts),
        accepted_refuted_collision_groups: collision_groups.len(),
        accepted_in_accepted_refuted_groups,
        accepted_refuted_collision_fraction_ppm,
        collision_examples_in_operational_top_k,
        collision_accepted_in_operational_top_k,
        collision_groups,
        traces,
    })
}

fn accepted_refuted_collision_groups(traces: &[DonorTrace]) -> Vec<CollisionGroupTrace> {
    let mut grouped = BTreeMap::<(String, Vec<u32>), (u8, Vec<DonorTrace>)>::new();
    for trace in traces {
        let verdict = match trace.verdict {
            "accepted" => 1,
            "refuted" => 2,
            _ => 4,
        };
        let group = grouped
            .entry((trace.claim_digest.clone(), trace.feature_bits.clone()))
            .or_default();
        group.0 |= verdict;
        group.1.push(trace.clone());
    }
    grouped
        .into_iter()
        .filter(|(_, (verdicts, _))| *verdicts & 1 != 0 && *verdicts & 2 != 0)
        .map(|((claim_digest, feature_bits), (_, mut members))| {
            members.sort_unstable_by_key(|member| member.operational_policy_rank);
            CollisionGroupTrace {
                claim_digest,
                operator: "lean-proof-substitution",
                feature_bits,
                members,
            }
        })
        .collect()
}

fn operational_collision_occupancy(
    groups: &[CollisionGroupTrace],
    budgets: [u32; 7],
) -> ([usize; 7], [usize; 7]) {
    let mut examples = [0; 7];
    let mut accepted = [0; 7];
    for member in groups.iter().flat_map(|group| &group.members) {
        for (index, budget) in budgets.into_iter().enumerate() {
            if member.operational_policy_rank < budget {
                examples[index] += 1;
                accepted[index] += usize::from(member.verdict == "accepted");
            }
        }
    }
    (examples, accepted)
}

fn proof_term_digest(proof: &LeanExpr) -> Result<[u8; 32], AnyError> {
    Ok(Sha256::digest(serde_json::to_vec(proof)?).into())
}

fn information_audit_for_training(
    config: &LeanWorkerConfig,
    corpus: &LeanCorpus,
    bundle: &Path,
) -> Result<DonorAudit, AnyError> {
    let domain = LeanDomain::new(config.clone(), corpus.clone())?;
    substitution_information_audit(bundle, Some((&domain, corpus.operator_library())))
}

fn candidate_verification_curve(
    bundle: &Path,
    fate_offset: usize,
) -> Result<Vec<VerificationCurvePoint>, AnyError> {
    let bytes = std::fs::read(bundle)?;
    let decoded = CanonicalBundle::decode(&bytes, RUNTIME_RESIDENT_BYTES)?;
    let experience = inspect_experience_segment(decoded.segment(SegmentKind::Experience))
        .map_err(|_| "Lean Bundle has malformed Experience framing")?;
    let fates = experience
        .candidate_fates
        .get(fate_offset..)
        .ok_or("Lean treatment Candidate Fate offset exceeds retained Experience")?;
    verification_curve(fates)
}

fn verification_curve(
    fates: &[CandidateFateInspection],
) -> Result<Vec<VerificationCurvePoint>, AnyError> {
    const BUDGETS: [usize; 7] = [1, 4, 8, 16, 32, 64, 128];

    let mut batches = BTreeMap::<u64, Vec<&CandidateFateInspection>>::new();
    for fate in fates {
        if matches!(
            fate.outcome,
            CandidateFateOutcomeInspection::Verified { .. }
        ) {
            batches.entry(fate.epoch).or_default().push(fate);
        }
    }
    let mut reached = [None; BUDGETS.len()];
    let mut requests = 0_usize;
    let mut strict_discoveries = 0_usize;
    let mut cpu_ns = 0_u64;
    for batch in batches.values_mut() {
        batch.sort_unstable_by_key(|fate| {
            fate.policy_rank
                .expect("Verified Candidate Fate has a policy rank")
        });
        let batch_size = batch[0]
            .verification_batch_size
            .ok_or("Verified Candidate Fate is missing its Verification batch size")?;
        if usize::try_from(batch_size)? != batch.len()
            || batch.iter().any(|fate| {
                fate.verification_batch_size != Some(batch_size)
                    || fate.verification_batch_cpu_ns != batch[0].verification_batch_cpu_ns
            })
        {
            return Err("Lean Candidate Fates disagree about their Verification batch".into());
        }
        cpu_ns = cpu_ns.saturating_add(
            batch[0]
                .verification_batch_cpu_ns
                .ok_or("Verified Candidate Fate is missing Verification batch CPU")?,
        );
        for fate in batch {
            requests += 1;
            if matches!(
                fate.outcome,
                CandidateFateOutcomeInspection::Verified {
                    strict_improvement: true,
                    ..
                }
            ) {
                strict_discoveries += 1;
            }
            if let Some(index) = BUDGETS.iter().position(|budget| *budget == requests) {
                reached[index] = Some((strict_discoveries, cpu_ns));
            }
        }
    }
    Ok(BUDGETS
        .into_iter()
        .enumerate()
        .map(|(index, request_budget)| {
            let point = reached[index];
            VerificationCurvePoint {
                request_budget,
                reached: point.is_some(),
                observed_requests: requests.min(request_budget),
                strict_discoveries: point.map_or(strict_discoveries, |value| value.0),
                cpu_ns_through_completed_batch: point.map_or(cpu_ns, |value| value.1),
            }
        })
        .collect())
}

fn record_strict_improvements(
    update: &ParetoUpdate<'_, LeanDomain>,
    seed_nodes: &HashMap<String, usize>,
    output: &mut Vec<StrictImprovement>,
) {
    for artifact in update.added() {
        let artifact_key = hex(artifact.key().as_bytes());
        let declaration = artifact.artifact().declaration.name.to_string();
        let Some(seed_proof_nodes) = seed_nodes.get(&declaration).copied() else {
            continue;
        };
        let proof_nodes = artifact.artifact().proof_term.node_count();
        if proof_nodes >= seed_proof_nodes
            || output
                .iter()
                .any(|known| known.artifact_key == artifact_key)
        {
            continue;
        }
        output.push(StrictImprovement {
            observer_sequence: update.sequence(),
            artifact_key,
            declaration,
            seed_proof_nodes,
            proof_nodes,
            proof_nodes_removed: seed_proof_nodes - proof_nodes,
        });
    }
}

fn usage(usage: ResourceUsage) -> Usage {
    Usage {
        worker_threads: usage.worker_threads,
        resident_bytes: usage.resident_bytes,
        verification_requests: usage.verification_requests,
        durable_bytes: usage.durable_bytes,
        elapsed_ns: u64::try_from(usage.elapsed_time.as_nanos()).unwrap_or(u64::MAX),
        cpu_ns: u64::try_from(usage.cpu_time.as_nanos()).unwrap_or(u64::MAX),
    }
}

fn learning_summary(bundle: &Path) -> Result<LearningSummary, AnyError> {
    let bytes = std::fs::read(bundle)?;
    let decoded = CanonicalBundle::decode(&bytes, RUNTIME_RESIDENT_BYTES)?;
    let revisions = decoded.segment(SegmentKind::Revisions);
    if revisions.len() < 64 {
        return Err("Lean training Bundle has a truncated Revisions segment".into());
    }
    let mut state = &revisions[64..];
    take_summary_sized(&mut state)?;
    let learning = take_summary_sized(&mut state)?;
    if !state.is_empty() {
        return Err("Lean training Bundle has trailing Revisions state".into());
    }
    learning_header(learning)
}

fn experience_summary(experience: &[u8]) -> Result<ExperienceSummary, AnyError> {
    let experience = inspect_experience_segment(experience)
        .map_err(|_| "Lean Bundle has malformed Experience framing")?;
    let entries = u64::try_from(experience.attempts.len())?;
    let mut verdicts = [0_u64; 3];
    let mut claims = HashSet::new();
    let mut accepted_claims = HashSet::new();
    let candidate_fates = experience.candidate_fates.len();
    let mut novelty_filtered = 0_usize;
    let mut policy_deferred = 0_usize;
    let mut verification_interrupted = 0_usize;
    let mut verified_by_queue = [0_usize; 4];
    let mut admitted = 0_usize;
    let queue_index = |queue| match queue {
        CandidateAllocationQueueInspection::ProtectedOrigin => 0,
        CandidateAllocationQueueInspection::ProtectedDerived => 1,
        CandidateAllocationQueueInspection::Learned => 2,
        CandidateAllocationQueueInspection::Bootstrap => 3,
    };
    for fate in &experience.candidate_fates {
        match fate.outcome {
            CandidateFateOutcomeInspection::NoveltyFiltered { .. } => novelty_filtered += 1,
            CandidateFateOutcomeInspection::PolicyDeferred { .. } => policy_deferred += 1,
            CandidateFateOutcomeInspection::VerificationInterrupted { .. } => {
                verification_interrupted += 1;
            }
            CandidateFateOutcomeInspection::Verified {
                allocation_queue,
                admitted: was_admitted,
                ..
            } => {
                verified_by_queue[queue_index(allocation_queue)] += 1;
                admitted += usize::from(was_admitted);
            }
        }
    }
    let candidate_fate_trace = experience
        .candidate_fates
        .iter()
        .map(candidate_fate_summary)
        .collect();
    for attempt in experience.attempts {
        claims.insert(attempt.claim_digest);
        let index = match attempt.verdict {
            ExperienceVerdictInspection::Accepted => 0,
            ExperienceVerdictInspection::Refuted => 1,
            ExperienceVerdictInspection::Unknown => 2,
        };
        verdicts[index] = verdicts[index].saturating_add(1);
        if attempt.verdict == ExperienceVerdictInspection::Accepted {
            accepted_claims.insert(attempt.claim_digest);
        }
    }
    Ok(ExperienceSummary {
        entries,
        claims: claims.len(),
        accepted_claims: accepted_claims.len(),
        verdicts,
        candidate_fates,
        novelty_filtered,
        policy_deferred,
        verification_interrupted,
        verified_by_queue,
        admitted,
        candidate_fate_trace,
    })
}

fn recovery_summary(mut recovery: &[u8]) -> Result<RecoverySummary, AnyError> {
    let pareto_artifacts = usize::try_from(read_summary_u64(&mut recovery)?)?;
    take_summary(&mut recovery, pareto_artifacts.saturating_mul(32))?;
    let frontier_artifacts = usize::try_from(read_summary_u64(&mut recovery)?)?;
    take_summary(&mut recovery, frontier_artifacts.saturating_mul(32))?;
    let deferred_candidates = usize::try_from(read_summary_u64(&mut recovery)?)?;
    let mut deferred_canonical_bytes = 0_u64;
    for _ in 0..deferred_candidates {
        let canonical = take_summary_sized(&mut recovery)?;
        deferred_canonical_bytes = deferred_canonical_bytes.saturating_add(canonical.len() as u64);
        take_summary(&mut recovery, 32)?;
        take_summary_sized(&mut recovery)?;
        take_summary(
            &mut recovery,
            4 + 1 + reflex::domain::PROPOSAL_FEATURE_COUNT * 4,
        )?;
        match take_summary(&mut recovery, 1)?[0] {
            0 => {}
            1 => {
                take_summary(&mut recovery, 32)?;
            }
            _ => return Err("Lean Bundle has invalid Recovery provenance framing".into()),
        }
    }
    let pending_parents = usize::try_from(read_summary_u64(&mut recovery)?)?;
    for _ in 0..pending_parents {
        take_summary(&mut recovery, 32)?;
        let offsets = usize::try_from(read_summary_u64(&mut recovery)?)?;
        take_summary(&mut recovery, offsets.saturating_mul(8))?;
        match take_summary(&mut recovery, 1)?[0] {
            0 | 1 => {}
            _ => return Err("Lean Bundle has invalid pending-parent framing".into()),
        }
    }
    if !recovery.is_empty() {
        return Err("Lean Bundle has trailing Recovery state".into());
    }
    Ok(RecoverySummary {
        pareto_artifacts,
        frontier_artifacts,
        deferred_candidates,
        deferred_canonical_bytes,
        pending_parents,
    })
}

fn candidate_fate_summary(fate: &CandidateFateInspection) -> CandidateFateSummary {
    let mut summary = CandidateFateSummary {
        candidate_key: hex(&fate.candidate_key),
        claim_digest: hex(&fate.claim_digest),
        parent_key: hex(&fate.parent_key),
        operator_digest: hex(&fate.operator_digest),
        epoch: fate.epoch,
        generation_rank: fate.generation_rank,
        proposal_limit: fate.proposal_limit,
        policy_rank: fate.policy_rank,
        verification_batch_cpu_ns: fate.verification_batch_cpu_ns,
        verification_batch_size: fate.verification_batch_size,
        outcome: "novelty-filtered",
        novelty_filter_reason: None,
        allocation_queue: None,
        bootstrap_rank: None,
        learned_rank: None,
        verdict: None,
        admitted: false,
        strict_improvement: false,
    };
    let queue_name = |queue| match queue {
        CandidateAllocationQueueInspection::ProtectedOrigin => "protected-origin",
        CandidateAllocationQueueInspection::ProtectedDerived => "protected-derived",
        CandidateAllocationQueueInspection::Learned => "learned",
        CandidateAllocationQueueInspection::Bootstrap => "bootstrap",
    };
    match fate.outcome {
        CandidateFateOutcomeInspection::NoveltyFiltered { reason } => {
            summary.novelty_filter_reason = Some(match reason {
                CandidateNoveltyFilterReasonInspection::KnownArtifact => "known-artifact",
                CandidateNoveltyFilterReasonInspection::DuplicateCandidate => "duplicate-candidate",
                CandidateNoveltyFilterReasonInspection::PriorNegativeExperience => {
                    "prior-negative-experience"
                }
            });
        }
        CandidateFateOutcomeInspection::PolicyDeferred {
            bootstrap_rank,
            learned_rank,
        } => {
            summary.outcome = "policy-deferred";
            summary.bootstrap_rank = bootstrap_rank;
            summary.learned_rank = learned_rank;
        }
        CandidateFateOutcomeInspection::VerificationInterrupted {
            allocation_queue,
            bootstrap_rank,
            learned_rank,
        } => {
            summary.outcome = "verification-interrupted";
            summary.allocation_queue = Some(queue_name(allocation_queue));
            summary.bootstrap_rank = bootstrap_rank;
            summary.learned_rank = learned_rank;
        }
        CandidateFateOutcomeInspection::Verified {
            verdict,
            allocation_queue,
            bootstrap_rank,
            learned_rank,
            admitted,
            strict_improvement,
        } => {
            summary.outcome = "verified";
            summary.allocation_queue = Some(queue_name(allocation_queue));
            summary.bootstrap_rank = bootstrap_rank;
            summary.learned_rank = learned_rank;
            summary.verdict = Some(match verdict {
                ExperienceVerdictInspection::Accepted => "accepted",
                ExperienceVerdictInspection::Refuted => "refuted",
                ExperienceVerdictInspection::Unknown => "unknown",
            });
            summary.admitted = admitted;
            summary.strict_improvement = strict_improvement;
        }
    }
    summary
}

fn learning_header(learning: &[u8]) -> Result<LearningSummary, AnyError> {
    if learning.len() < 14 || &learning[..5] != b"RFLS\x02" {
        return Err("Lean training Bundle has incompatible Learning state".into());
    }
    let generation = u64::from_le_bytes(learning[5..13].try_into()?);
    let champion_present = match learning[13] {
        0 => false,
        1 => {
            if learning.len() < 22 {
                return Err("Lean training Bundle has a truncated champion Model".into());
            }
            let model_bytes = usize::try_from(u64::from_le_bytes(learning[14..22].try_into()?))?;
            if model_bytes > learning.len() - 22 {
                return Err("Lean training Bundle has a truncated champion Model".into());
            }
            true
        }
        _ => return Err("Lean training Bundle has an invalid champion marker".into()),
    };
    if generation == 0 && champion_present {
        return Err("Lean training Bundle has a champion at generation zero".into());
    }
    Ok(LearningSummary {
        generation,
        champion_present,
    })
}

fn take_summary_sized<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], AnyError> {
    let length = usize::try_from(read_summary_u64(input)?)?;
    take_summary(input, length)
}

fn read_summary_u64(input: &mut &[u8]) -> Result<u64, AnyError> {
    Ok(u64::from_le_bytes(take_summary(input, 8)?.try_into()?))
}

fn take_summary<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], AnyError> {
    if input.len() < count {
        return Err("Lean Bundle has a truncated payload".into());
    }
    let (value, remainder) = input.split_at(count);
    *input = remainder;
    Ok(value)
}

fn parse(arguments: &[String]) -> Result<Arguments, AnyError> {
    let values = parse_flag_values(
        arguments,
        &[
            "--lake",
            "--december-root",
            "--september-catalog",
            "--december-catalog",
            "--work",
            "--output",
            "--training-artifacts",
            "--heldout-artifacts",
            "--training-verification-requests",
            "--verification-requests",
            "--mode",
        ],
        "lean-public-optimizer-development",
    )?;
    let value = |flag| -> Result<&str, AnyError> {
        values
            .get(flag)
            .copied()
            .ok_or_else(|| format!("lean-public-optimizer-development requires {flag}").into())
    };
    let preflight_only = match values.get("--mode").copied().unwrap_or("run") {
        "run" => false,
        "preflight" => true,
        mode => return Err(format!("unknown Lean optimizer Development mode {mode}").into()),
    };
    Ok(Arguments {
        lake: value("--lake")?.into(),
        december_root: value("--december-root")?.into(),
        september_catalog: value("--september-catalog")?.into(),
        december_catalog: value("--december-catalog")?.into(),
        work: value("--work")?.into(),
        output: value("--output")?.into(),
        training_artifacts: value("--training-artifacts")?.parse()?,
        heldout_artifacts: value("--heldout-artifacts")?.parse()?,
        training_verification_requests: value("--training-verification-requests")?.parse()?,
        verification_requests: value("--verification-requests")?.parse()?,
        preflight_only,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_lean::ast::LeanName;
    use reflex_lean::temporal::POTENTIAL_HEADS;

    fn example(name: &str, statement_hash: u64, semantic_group: u8) -> TemporalExample {
        TemporalExample {
            declaration: LeanName::from_dotted(name),
            module: LeanName::from_dotted("Test.Module"),
            statement_hash,
            semantic_group: [semantic_group; 32],
            features: Vec::new(),
            targets: [0.0; POTENTIAL_HEADS],
        }
    }

    #[test]
    fn collision_groups_are_claim_relative_and_use_operational_policy_rank() {
        let trace = |marker: &str, claim: &str, verdict, rank, feature_bits, donor_declarations| {
            DonorTrace {
                candidate_key: marker.into(),
                claim_digest: claim.into(),
                epoch: 1,
                verdict,
                operational_policy_rank: rank,
                strict_improvement: marker == "accepted",
                feature_bits,
                support_key_sha256: None,
                proof_term_sha256: Some(marker.into()),
                donor_declarations,
            }
        };
        let traces = vec![
            trace(
                "accepted",
                "claim-a",
                "accepted",
                0,
                vec![1],
                vec!["A".into()],
            ),
            trace(
                "refuted",
                "claim-a",
                "refuted",
                2,
                vec![1],
                vec!["B".into()],
            ),
            trace("unknown", "claim-a", "unknown", 1, vec![1], Vec::new()),
            trace(
                "distinct",
                "claim-a",
                "accepted",
                3,
                vec![2],
                vec!["C".into()],
            ),
            trace(
                "other-claim",
                "claim-b",
                "refuted",
                0,
                vec![1],
                vec!["D".into()],
            ),
        ];

        let groups = accepted_refuted_collision_groups(&traces);
        let occupancy = operational_collision_occupancy(&groups, [1, 2, 4, 8, 16, 32, 64]);
        let summary = summarize_substitution_information(traces).unwrap();

        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].claim_digest, "claim-a");
        assert_eq!(groups[0].members.len(), 3);
        assert_eq!(groups[0].members[0].donor_declarations, ["A"]);
        assert_eq!(occupancy.0, [1, 2, 3, 3, 3, 3, 3]);
        assert_eq!(occupancy.1, [1, 1, 1, 1, 1, 1, 1]);
        assert_eq!(summary.accepted_proof_substitutions, 2);
        assert_eq!(summary.accepted_in_accepted_refuted_groups, 1);
        assert_eq!(summary.accepted_refuted_collision_fraction_ppm, 500_000);
        assert_eq!(
            summary.collision_examples_in_operational_top_k,
            [1, 3, 3, 3, 3, 3, 3]
        );
    }

    #[test]
    fn verification_curve_reports_registered_prefixes_with_accounted_batch_cpu() {
        let fates = (0..4_u32)
            .map(|rank| CandidateFateInspection {
                candidate_key: [u8::try_from(rank).unwrap(); 32],
                claim_digest: [1; 32],
                parent_key: [2; 32],
                operator_digest: [3; 32],
                support_key: None,
                epoch: 7,
                generation_rank: rank,
                proposal_limit: 8,
                policy_rank: Some(rank),
                verification_batch_cpu_ns: Some(1_000),
                verification_batch_size: Some(4),
                outcome: CandidateFateOutcomeInspection::Verified {
                    verdict: ExperienceVerdictInspection::Accepted,
                    allocation_queue: CandidateAllocationQueueInspection::Bootstrap,
                    bootstrap_rank: Some(rank),
                    learned_rank: None,
                    admitted: rank == 1,
                    strict_improvement: rank == 1,
                },
            })
            .collect::<Vec<_>>();

        let curve = verification_curve(&fates).unwrap();

        assert!(
            curve[0].reached
                && curve[0].observed_requests == 1
                && curve[0].strict_discoveries == 0
                && curve[0].cpu_ns_through_completed_batch == 1_000
                && curve[1].reached
                && curve[1].strict_discoveries == 1
                && !curve[2].reached
        );
    }

    #[test]
    fn discovery_efficiency_compares_exact_ratios_at_a_registered_prefix() {
        let full = VerificationCurvePoint {
            request_budget: 16,
            reached: true,
            observed_requests: 16,
            strict_discoveries: 4,
            cpu_ns_through_completed_batch: 2_071_896_436,
        };
        let bootstrap = VerificationCurvePoint {
            request_budget: 16,
            reached: true,
            observed_requests: 16,
            strict_discoveries: 1,
            cpu_ns_through_completed_batch: 1_348_917_507,
        };

        let comparison = discovery_efficiency_at(16, &[full], &[bootstrap]).unwrap();

        assert_eq!(comparison.full_strict_discoveries, 4);
        assert_eq!(comparison.bootstrap_strict_discoveries, 1);
        assert!(comparison.full_uses_less_candidate_cpu_per_improvement);
        assert!(ratio_is_strictly_less(10, 3, 7, 2));
        assert!(!ratio_is_strictly_less(7, 2, 10, 3));
    }

    #[test]
    fn primary_selection_excludes_generated_components_and_duplicate_statements() {
        let generated = example("Test.theorem.proof_7", 1, 0);
        let auxiliary = example("Test.theorem._auxLemma.9", 4, 4);
        let first = example("Test.first", 2, 1);
        let duplicate = example("Test.duplicate", 2, 2);
        let second = example("Test.second", 3, 3);
        let artifacts = [&generated, &duplicate, &second, &first];

        let selected = deterministic_prefix(
            artifacts
                .into_iter()
                .filter(|artifact| is_human_facing(&artifact.declaration)),
            8,
        );

        assert_eq!(
            selected
                .iter()
                .map(|artifact| artifact.statement_hash)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(!is_human_facing(&generated.declaration));
        assert!(!is_human_facing(&auxiliary.declaration));
        assert!(is_human_facing(&LeanName::from_dotted(
            "Test.proof_by_cases"
        )));
    }

    #[test]
    fn heldout_pool_never_relabels_an_earlier_name_after_family_drift() {
        let earlier_seed = example("Test.same", 1, 1);
        let earlier_peer = example("Test.samePeer", 1, 1);
        let drifted_seed = example("Test.same", 1, 9);
        let drifted_peer = example("Test.samePeer", 1, 9);
        let new_seed = example("Test.new", 2, 2);
        let new_peer = example("Test.newPeer", 2, 2);
        let arguments = Arguments {
            lake: PathBuf::new(),
            december_root: PathBuf::new(),
            september_catalog: PathBuf::new(),
            december_catalog: PathBuf::new(),
            work: PathBuf::new(),
            output: PathBuf::new(),
            training_artifacts: 1,
            heldout_artifacts: 1,
            training_verification_requests: 1,
            verification_requests: 1,
            preflight_only: false,
        };

        let september = [earlier_seed, earlier_peer];
        let december = [drifted_seed, drifted_peer, new_seed, new_peer];
        let (_, _, heldout) = selection_pools(&september, &december, &arguments);

        assert_eq!(heldout.len(), 1);
        assert_eq!(heldout[0].statement_hash, 2);
    }

    #[test]
    fn preflight_mode_is_explicit_and_does_not_change_the_registered_budgets() {
        let arguments = [
            "--lake",
            "/lake",
            "--december-root",
            "/mathlib",
            "--september-catalog",
            "/september",
            "--december-catalog",
            "/december",
            "--work",
            "/work",
            "--output",
            "/output",
            "--training-artifacts",
            "32",
            "--heldout-artifacts",
            "8",
            "--training-verification-requests",
            "1024",
            "--verification-requests",
            "128",
            "--mode",
            "preflight",
        ]
        .map(str::to_owned);

        let parsed = parse(&arguments).unwrap();

        assert!(parsed.preflight_only);
        assert_eq!(parsed.training_artifacts, 32);
        assert_eq!(parsed.heldout_artifacts, 8);
        assert_eq!(parsed.training_verification_requests, 1_024);
        assert_eq!(parsed.verification_requests, 128);
    }

    #[test]
    fn learning_header_distinguishes_bootstrap_from_a_promoted_champion() {
        let mut bootstrap = b"RFLS\x02".to_vec();
        bootstrap.extend_from_slice(&0_u64.to_le_bytes());
        bootstrap.push(0);
        let summary = learning_header(&bootstrap).unwrap();
        assert_eq!(summary.generation, 0);
        assert!(!summary.champion_present);

        let mut promoted = b"RFLS\x02".to_vec();
        promoted.extend_from_slice(&2_u64.to_le_bytes());
        promoted.push(1);
        promoted.extend_from_slice(&3_u64.to_le_bytes());
        promoted.extend_from_slice(&[1, 2, 3]);
        let summary = learning_header(&promoted).unwrap();
        assert_eq!(summary.generation, 2);
        assert!(summary.champion_present);

        promoted[14..22].copy_from_slice(&4_u64.to_le_bytes());
        assert!(learning_header(&promoted).is_err());
    }

    #[test]
    fn recovery_summary_reads_cursor_and_canonical_payload_counts() {
        let mut recovery = Vec::new();
        let push_u64 = |output: &mut Vec<u8>, value: u64| {
            output.extend_from_slice(&value.to_le_bytes());
        };
        push_u64(&mut recovery, 1);
        recovery.extend_from_slice(&[1; 32]);
        push_u64(&mut recovery, 1);
        recovery.extend_from_slice(&[2; 32]);
        push_u64(&mut recovery, 1);
        push_u64(&mut recovery, 3);
        recovery.extend_from_slice(b"dag");
        recovery.extend_from_slice(&[3; 32]);
        push_u64(&mut recovery, 2);
        recovery.extend_from_slice(b"op");
        recovery.extend_from_slice(&8_u32.to_le_bytes());
        recovery.push(0);
        recovery.extend_from_slice(&[0; reflex::domain::PROPOSAL_FEATURE_COUNT * 4]);
        recovery.push(1);
        recovery.extend_from_slice(&[4; 32]);
        push_u64(&mut recovery, 1);
        recovery.extend_from_slice(&[5; 32]);
        push_u64(&mut recovery, 2);
        recovery.extend_from_slice(&0_u64.to_le_bytes());
        recovery.extend_from_slice(&8_u64.to_le_bytes());
        recovery.push(1);

        let summary = recovery_summary(&recovery).unwrap();

        assert_eq!(summary.pareto_artifacts, 1);
        assert_eq!(summary.frontier_artifacts, 1);
        assert_eq!(summary.deferred_candidates, 1);
        assert_eq!(summary.deferred_canonical_bytes, 3);
        assert_eq!(summary.pending_parents, 1);
    }

    #[test]
    fn experience_summary_rejects_malformed_framing() {
        assert!(experience_summary(&[]).is_err());
        assert!(experience_summary(&0_u64.to_le_bytes()).is_err());
    }
}
