use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Duration;

use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, ResourceUsage, improve,
};
use reflex_lean::catalog::LeanCatalog;
use reflex_lean::domain::{LeanCorpus, LeanDomain, LeanMetric, LeanSeedScope};
use reflex_lean::temporal::{TemporalExample, TemporalSnapshot};
use reflex_lean::worker::{IndexedTheorem, LeanWorker, LeanWorkerConfig, VerificationItem};
use serde::Serialize;

use crate::harness::{
    AnyError, HostEnvironment, HostIsolation, HostIsolationPolicy, capture_child_host_isolated,
    environment, hash_file, hash_json, inherited_host_isolation, parse_flag_values, require_absent,
    require_clean, require_release,
};

const DEVELOPMENT_SCHEMA: &str = "reflex-lean-public-optimizer-development-v6";
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
    usage: Usage,
    bundle_sha256: String,
}

#[derive(Serialize)]
struct Report {
    schema: &'static str,
    status: &'static str,
    training_artifacts: usize,
    heldout_artifacts: usize,
    primary_proof_node_limit: usize,
    primary_selection: &'static str,
    september_catalog_sha256: String,
    december_catalog_sha256: String,
    selected_artifacts: Vec<SelectedArtifact>,
    training_usage: Usage,
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
            arguments.verification_requests,
            BundlePlan::Fresh {
                target: training_bundle.clone(),
            },
        )?,
        |_| ControlFlow::Continue(()),
    )?);
    let bootstrap = finish_treatment(
        "bootstrap",
        improve(
            LeanDomain::new(prepared.config.clone(), prepared.heldout_corpus.clone())?,
            request(
                LeanSeedScope {
                    start: 0,
                    count: prepared.heldout,
                },
                arguments.verification_requests,
                BundlePlan::Fresh {
                    target: bootstrap_bundle.clone(),
                },
            )?,
            |_| ControlFlow::Continue(()),
        )?,
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
        primary_proof_node_limit: PRIMARY_PROOF_NODE_LIMIT,
        primary_selection: "human-facing declarations with <=100000 proof nodes, distinct statement fingerprints, and a kernel-accepted strictly shorter pre-2025 library proof; held-out names are absent from September and held-out statement fingerprints are disjoint from selected training; development opportunity corpus, not confirmation sampling",
        september_catalog_sha256,
        december_catalog_sha256,
        selected_artifacts,
        training_usage,
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

#[expect(
    clippy::too_many_arguments,
    reason = "a causal treatment binds its immutable source, output, corpus, budget, and result accounting"
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
    finish_treatment(
        name,
        improve(
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
            |_| ControlFlow::Continue(()),
        )?,
        seed_nodes,
        target,
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
        usage: usage(outcome.usage()),
        bundle_sha256: hash_file(bundle)?,
    };
    drop(outcome);
    Ok(summary)
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
            verification_requests: 1,
        };

        let september = [earlier_seed, earlier_peer];
        let december = [drifted_seed, drifted_peer, new_seed, new_peer];
        let (_, _, heldout) = selection_pools(&september, &december, &arguments);

        assert_eq!(heldout.len(), 1);
        assert_eq!(heldout[0].statement_hash, 2);
    }
}
