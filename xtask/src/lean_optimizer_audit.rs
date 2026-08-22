use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reflex::internal_experiments::compare_candidate_features;
use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, ParetoUpdate, Preference, ResourceEnvelope, ResourceUsage, improve,
};
use reflex_bundle::{CanonicalBundle, SegmentKind};
use reflex_lean::catalog::LeanCatalog;
use reflex_lean::domain::{LeanCorpus, LeanDomain, LeanMetric, LeanSeedScope};
use reflex_lean::temporal::{TemporalExample, TemporalSnapshot};
use reflex_lean::worker::{IndexedTheorem, LeanWorker, LeanWorkerConfig, VerificationItem};
use serde::Serialize;

use crate::harness::{
    AnyError, HostEnvironment, HostIsolation, HostIsolationPolicy, capture_child_host_isolated,
    environment, hash_file, hash_json, hex, inherited_host_isolation, parse_flag_values,
    require_absent, require_clean, require_release,
};

const DEVELOPMENT_SCHEMA: &str = "reflex-lean-public-optimizer-development-v7";
const RUNTIME_RESIDENT_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const SUPERVISOR_RESIDENT_BYTES: u64 = 40 * 1024 * 1024 * 1024;
const HOST_MEMORY_RESERVE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const HOST_CPU_RESERVE: usize = 1;
const WORKER_THREADS: usize = 6;
const DURABLE_BYTES: u64 = 1024 * 1024 * 1024;
const SUPERVISOR_WALL_LIMIT: Duration = Duration::from_mins(35);
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
struct TreatmentResult {
    treatment: &'static str,
    completion: String,
    heldout_pareto_artifacts: usize,
    strict_proof_node_improvements: usize,
    proof_nodes_removed: usize,
    strict_improvements: Vec<StrictImprovement>,
    usage: Usage,
    bundle_sha256: String,
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
    artifact_records: u64,
    experience_entries: u64,
    experience_claims: usize,
    accepted_claims: usize,
    accepted_experience: u64,
    refuted_experience: u64,
    unknown_experience: u64,
    learning: LearningSummary,
    knowledge_revision: String,
    model_revision: String,
    segment_logical_bytes: Vec<SegmentLogicalBytes>,
}

#[derive(Serialize)]
struct FeatureDevelopmentReport {
    schema: &'static str,
    status: &'static str,
    retained_bundle_sha256: String,
    examples: usize,
    replay_claims: usize,
    selection_claims: usize,
    feature_count: usize,
    head_count: usize,
    baseline_selection_loss: [f32; 7],
    structural_selection_loss: [f32; 7],
    baseline_training_cpu_ns: u64,
    structural_training_cpu_ns: u64,
    feature_extraction_cpu_ns: u64,
    baseline_model_revision: String,
    structural_model_revision: String,
    model_bytes: usize,
    baseline_reproduces_champion: bool,
    structural_promotes_over_baseline: bool,
    host: HostEnvironment,
    content_sha256: String,
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
    full: TreatmentResult,
    no_model: TreatmentResult,
    no_derived: TreatmentResult,
    bootstrap: TreatmentResult,
    full_has_more_improvements: bool,
    full_uses_less_cpu_per_improvement: bool,
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
    full: TreatmentResult,
    no_model: TreatmentResult,
    no_derived: TreatmentResult,
    bootstrap: TreatmentResult,
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
    let (capture, isolation) = capture_child_host_isolated(
        &executable,
        &child_arguments,
        &evidence_prefix,
        SUPERVISOR_WALL_LIMIT,
        HostIsolationPolicy {
            memory_limit_bytes: SUPERVISOR_RESIDENT_BYTES,
            memory_reserve_bytes: HOST_MEMORY_RESERVE_BYTES,
            cpu_reserve: HOST_CPU_RESERVE,
        },
        &[(
            OsString::from("REFLEX_LEAN_OPTIMIZER_SUPERVISOR_CAPABILITY"),
            OsString::from(SUPERVISOR_CAPABILITY),
        )],
    )?;
    if capture.status.success() && !capture.timed_out && !capture.resident_limit_exceeded {
        print!("{}", capture.stdout);
        return Ok(());
    }
    let failure = if capture.timed_out {
        "exceeded its 35-minute supervised wall limit"
    } else if capture.resident_limit_exceeded {
        "reached its 40-GiB supervised resident boundary"
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
    let artifacts = decoded.segment(SegmentKind::Artifacts);
    if artifacts.len() < 8 {
        return Err("Lean Bundle has a truncated Artifact count".into());
    }
    let artifact_records = u64::from_le_bytes(artifacts[..8].try_into()?);
    let experience = experience_summary(decoded.segment(SegmentKind::Experience))?;
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
        schema: "reflex-lean-bundle-summary-v1",
        identity: String::from_utf8(decoded.identity().to_vec())?,
        artifact_records,
        experience_entries: experience.entries,
        experience_claims: experience.claims,
        accepted_claims: experience.accepted_claims,
        accepted_experience: experience.verdicts[0],
        refuted_experience: experience.verdicts[1],
        unknown_experience: experience.verdicts[2],
        learning,
        knowledge_revision: hex(&revisions[..32]),
        model_revision: hex(&revisions[32..64]),
        segment_logical_bytes,
    };
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

pub fn feature_development(arguments: &[String]) -> Result<(), AnyError> {
    const SCHEMA: &str = "reflex-lean-model-feature-development-v3";
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
    let duration_ns = |duration: Duration| u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
    let mut report = FeatureDevelopmentReport {
        schema: SCHEMA,
        status: "development-only; fixed retained Experience; no Verification requests; no 2026 exposure",
        retained_bundle_sha256: hash_file(&bundle)?,
        examples: comparison.examples,
        replay_claims: comparison.replay_claims,
        selection_claims: comparison.selection_claims,
        feature_count: 16,
        head_count: 7,
        baseline_selection_loss: comparison.baseline_selection_loss,
        structural_selection_loss: comparison.structural_selection_loss,
        baseline_training_cpu_ns: duration_ns(comparison.baseline_training_cpu),
        structural_training_cpu_ns: duration_ns(comparison.structural_training_cpu),
        feature_extraction_cpu_ns: duration_ns(comparison.feature_extraction_cpu),
        baseline_model_revision: hex(&comparison.baseline_model_revision),
        structural_model_revision: hex(&comparison.structural_model_revision),
        model_bytes: comparison.model_bytes,
        baseline_reproduces_champion: comparison.baseline_reproduces_champion,
        structural_promotes_over_baseline: comparison.structural_promotes_over_baseline,
        host,
        content_sha256: String::new(),
    };
    report.content_sha256 = hash_json(&report)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
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
    build_bootstrap_template(
        &prepared.config,
        prepared.training_corpus.clone(),
        prepared.training,
        &bootstrap_template_bundle,
    )?;
    let training_usage = finish_training(improve(
        LeanDomain::new(prepared.config.clone(), prepared.training_corpus)?,
        request(
            LeanSeedScope {
                start: 0,
                count: prepared.training,
            },
            arguments.training_verification_requests,
            BundlePlan::Fresh {
                target: training_bundle.clone(),
            },
        )?,
        |_| ControlFlow::Continue(()),
    )?);
    let training_learning = learning_summary(&training_bundle)?;
    if !training_learning.champion_present {
        return Err(format!(
            "Lean training produced no promoted Model Revision after {} Artifacts and {} Verification requests; a Full versus no-model treatment would be a placebo",
            prepared.training, training_usage.verification_requests
        )
        .into());
    }
    let bootstrap = run_fresh_treatment(
        "bootstrap",
        &prepared.config,
        &prepared.heldout_corpus,
        prepared.heldout,
        arguments.verification_requests,
        &prepared.heldout_seed_nodes,
        &bootstrap_bundle,
    )?;
    crate::causal::ablate_bundle(
        &training_bundle,
        &no_model_seed,
        Some(&bootstrap_template_bundle),
        false,
    )?;
    crate::causal::ablate_bundle(&training_bundle, &no_derived_seed, None, true)?;
    let full = run_resumed_treatment(
        "full",
        &prepared.config,
        &prepared.heldout_corpus,
        prepared.heldout,
        arguments.verification_requests,
        &training_bundle,
        &full_bundle,
        &prepared.heldout_seed_nodes,
    )?;
    let no_model = run_resumed_treatment(
        "no-model",
        &prepared.config,
        &prepared.heldout_corpus,
        prepared.heldout,
        arguments.verification_requests,
        &no_model_seed,
        &no_model_bundle,
        &prepared.heldout_seed_nodes,
    )?;
    let no_derived = run_resumed_treatment(
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
        full,
        no_model,
        no_derived,
        bootstrap,
    })
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
        full,
        no_model,
        no_derived,
        bootstrap,
    } = inputs;
    let full_has_more_improvements =
        full.strict_proof_node_improvements > bootstrap.strict_proof_node_improvements;
    let cpu_per_improvement = |result: &TreatmentResult| {
        result
            .usage
            .cpu_ns
            .checked_div(u64::try_from(result.strict_proof_node_improvements).unwrap_or(u64::MAX))
    };
    let full_uses_less_cpu_per_improvement = cpu_per_improvement(&full)
        .zip(cpu_per_improvement(&bootstrap))
        .is_some_and(|(full, bootstrap)| full < bootstrap);
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
        full,
        no_model,
        no_derived,
        bootstrap,
        full_has_more_improvements,
        full_uses_less_cpu_per_improvement,
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
    let display = name.to_string();
    let component = display.rsplit('.').next().unwrap_or(&display);
    !component.strip_prefix("proof_").is_some_and(|suffix| {
        !suffix.is_empty() && suffix.bytes().all(|byte| byte.is_ascii_digit())
    })
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
    finish_treatment(name, outcome, seed_nodes, strict_improvements, target)
}

#[expect(
    clippy::too_many_arguments,
    reason = "a resumed causal treatment also binds the immutable source Bundle"
)]
fn run_resumed_treatment(
    name: &'static str,
    config: &LeanWorkerConfig,
    corpus: &LeanCorpus,
    heldout: usize,
    verification_requests: u64,
    source: &Path,
    target: &Path,
    seed_nodes: &HashMap<String, usize>,
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
            BundlePlan::Resume {
                source: source.to_path_buf(),
                target: target.to_path_buf(),
            },
        )?,
        |update| {
            record_strict_improvements(&update, seed_nodes, &mut strict_improvements);
            ControlFlow::Continue(())
        },
    )?;
    finish_treatment(name, outcome, seed_nodes, strict_improvements, target)
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
        usage: usage(outcome.usage()),
        bundle_sha256: hash_file(bundle)?,
    };
    drop(outcome);
    Ok(summary)
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

fn experience_summary(mut experience: &[u8]) -> Result<ExperienceSummary, AnyError> {
    let entries = read_summary_u64(&mut experience)?;
    let mut verdicts = [0_u64; 3];
    let mut claims = HashSet::new();
    let mut accepted_claims = HashSet::new();
    for _ in 0..entries {
        take_summary(&mut experience, 32 * 2)?;
        let claim: [u8; 32] = take_summary(&mut experience, 32)?.try_into()?;
        claims.insert(claim);
        take_summary(&mut experience, 32 * 2)?;
        take_summary_sized(&mut experience)?;
        let verdict = take_summary(&mut experience, 1)?[0];
        let Some(index) = verdict
            .checked_sub(1)
            .map(usize::from)
            .filter(|index| *index < 3)
        else {
            return Err("Lean Bundle has an invalid Experience verdict".into());
        };
        verdicts[index] = verdicts[index].saturating_add(1);
        if verdict == 1 {
            accepted_claims.insert(claim);
        }
        take_summary_sized(&mut experience)?;
        take_summary(&mut experience, 16 * 4 + 4 + 8)?;
    }
    Ok(ExperienceSummary {
        entries,
        claims: claims.len(),
        accepted_claims: accepted_claims.len(),
        verdicts,
    })
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
        ],
        "lean-public-optimizer-development",
    )?;
    let value = |flag| -> Result<&str, AnyError> {
        values
            .get(flag)
            .copied()
            .ok_or_else(|| format!("lean-public-optimizer-development requires {flag}").into())
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
    fn primary_selection_excludes_generated_components_and_duplicate_statements() {
        let generated = example("Test.theorem.proof_7", 1, 0);
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
        };

        let september = [earlier_seed, earlier_peer];
        let december = [drifted_seed, drifted_peer, new_seed, new_peer];
        let (_, _, heldout) = selection_pools(&september, &december, &arguments);

        assert_eq!(heldout.len(), 1);
        assert_eq!(heldout[0].statement_hash, 2);
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
    fn experience_summary_counts_all_three_verdicts_and_rejects_invalid_values() {
        let encoded = |verdicts: &[u8]| {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&(verdicts.len() as u64).to_le_bytes());
            for verdict in verdicts {
                bytes.extend_from_slice(&[0; 32 * 5]);
                bytes.extend_from_slice(&0_u64.to_le_bytes());
                bytes.push(*verdict);
                bytes.extend_from_slice(&0_u64.to_le_bytes());
                bytes.extend_from_slice(&[0; 16 * 4 + 4 + 8]);
            }
            bytes
        };
        let summary = experience_summary(&encoded(&[1, 2, 2, 3])).unwrap();
        assert_eq!(summary.entries, 4);
        assert_eq!(summary.claims, 1);
        assert_eq!(summary.accepted_claims, 1);
        assert_eq!(summary.verdicts, [1, 2, 1]);
        assert!(experience_summary(&encoded(&[0])).is_err());
        assert!(experience_summary(&encoded(&[4])).is_err());
    }
}
