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
use reflex_lean::worker::{IndexedTheorem, LeanWorker, LeanWorkerConfig};
use serde::Serialize;

use crate::harness::{
    AnyError, HostEnvironment, HostIsolation, HostIsolationPolicy, capture_child_host_isolated,
    environment, hash_file, hash_json, inherited_host_isolation, parse_flag_values, require_absent,
    require_clean, require_release,
};

const DEVELOPMENT_SCHEMA: &str = "reflex-lean-public-optimizer-development-v1";
const RUNTIME_RESIDENT_BYTES: u64 = 32 * 1024 * 1024 * 1024;
const SUPERVISOR_RESIDENT_BYTES: u64 = 40 * 1024 * 1024 * 1024;
const HOST_MEMORY_RESERVE_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const HOST_CPU_RESERVE: usize = 1;
const WORKER_THREADS: usize = 6;
const DURABLE_BYTES: u64 = 1024 * 1024 * 1024;
const SUPERVISOR_WALL_LIMIT: Duration = Duration::from_mins(35);
const SUPERVISOR_CAPABILITY: &str = "reflex-lean-public-optimizer-supervisor-v1";

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
    corpus: LeanCorpus,
    training: usize,
    heldout: usize,
    heldout_seed_nodes: HashMap<String, usize>,
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
    training_usage: Usage,
    full: TreatmentResult,
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
    training_usage: Usage,
    full: TreatmentResult,
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

    let training_bundle = arguments.work.join("training.bundle");
    let full_bundle = arguments.work.join("full.bundle");
    let bootstrap_bundle = arguments.work.join("bootstrap.bundle");
    let training_outcome = improve(
        LeanDomain::new(prepared.config.clone(), prepared.corpus.clone())?,
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
    )?;
    let training_usage = usage(training_outcome.usage());
    let full_outcome = improve(
        LeanDomain::new(prepared.config.clone(), prepared.corpus.clone())?,
        request(
            LeanSeedScope {
                start: prepared.training,
                count: prepared.heldout,
            },
            arguments.verification_requests,
            BundlePlan::Resume {
                source: training_bundle,
                target: full_bundle.clone(),
            },
        )?,
        |_| ControlFlow::Continue(()),
    )?;
    let bootstrap_outcome = improve(
        LeanDomain::new(prepared.config, prepared.corpus)?,
        request(
            LeanSeedScope {
                start: prepared.training,
                count: prepared.heldout,
            },
            arguments.verification_requests,
            BundlePlan::Fresh {
                target: bootstrap_bundle.clone(),
            },
        )?,
        |_| ControlFlow::Continue(()),
    )?;
    let full = treatment(
        "full",
        &full_outcome,
        &prepared.heldout_seed_nodes,
        &full_bundle,
    )?;
    let bootstrap = treatment(
        "bootstrap",
        &bootstrap_outcome,
        &prepared.heldout_seed_nodes,
        &bootstrap_bundle,
    )?;
    write_report(
        &arguments,
        ReportInputs {
            host,
            host_isolation: isolation,
            training_artifacts: prepared.training,
            heldout_artifacts: prepared.heldout,
            training_usage,
            full,
            bootstrap,
        },
    )
}

fn write_report(arguments: &Arguments, inputs: ReportInputs) -> Result<(), AnyError> {
    let ReportInputs {
        host,
        host_isolation,
        training_artifacts,
        heldout_artifacts,
        training_usage,
        full,
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
        training_usage,
        full,
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
    let september_artifacts =
        TemporalSnapshot::from_catalog(&september_catalog).forecast_artifacts();
    let december_artifacts = TemporalSnapshot::from_catalog(&december_catalog).forecast_artifacts();
    let earlier_names = september_artifacts
        .iter()
        .map(|artifact| artifact.declaration.clone())
        .collect::<HashSet<_>>();
    let earlier_families = september_artifacts
        .iter()
        .map(|artifact| artifact.semantic_group)
        .collect::<HashSet<_>>();
    let training_pool = deterministic_prefix(
        december_artifacts
            .iter()
            .filter(|artifact| earlier_names.contains(&artifact.declaration)),
        arguments.training_artifacts.saturating_mul(8),
    );
    let heldout_pool = deterministic_prefix(
        december_artifacts
            .iter()
            .filter(|artifact| !earlier_families.contains(&artifact.semantic_group)),
        arguments.heldout_artifacts.saturating_mul(8),
    );
    let config = LeanWorkerConfig::pinned(&arguments.lake, &arguments.december_root);
    let worker = LeanWorker::start(&config)?;
    let names = training_pool
        .iter()
        .chain(&heldout_pool)
        .map(|artifact| artifact.declaration.clone())
        .collect::<Vec<_>>();
    let mut by_name = worker
        .fetch(&names)?
        .into_iter()
        .map(|theorem| (theorem.name.clone(), theorem))
        .collect::<HashMap<_, _>>();
    let mut training = training_pool
        .iter()
        .filter_map(|artifact| by_name.remove(&artifact.declaration))
        .take(arguments.training_artifacts)
        .collect::<Vec<_>>();
    let heldout = heldout_pool
        .iter()
        .filter_map(|artifact| by_name.remove(&artifact.declaration))
        .take(arguments.heldout_artifacts)
        .collect::<Vec<_>>();
    if training.len() != arguments.training_artifacts
        || heldout.len() != arguments.heldout_artifacts
    {
        return Err("Lean development corpus cannot satisfy the requested fetchable scopes".into());
    }
    let training_count = training.len();
    let heldout_count = heldout.len();
    training.extend(heldout);
    let ordered: Vec<IndexedTheorem> = training;
    let corpus = LeanCorpus::verified_theorems(&worker, ordered)?;
    drop(worker);
    let heldout_seed_nodes = corpus
        .entries()
        .iter()
        .skip(training_count)
        .map(|entry| {
            (
                entry.name.to_string(),
                entry.artifact.proof_term.node_count(),
            )
        })
        .collect();
    Ok(DevelopmentCorpus {
        config,
        corpus,
        training: training_count,
        heldout: heldout_count,
        heldout_seed_nodes,
    })
}

fn deterministic_prefix<'a>(
    artifacts: impl Iterator<Item = &'a TemporalExample>,
    count: usize,
) -> Vec<&'a TemporalExample> {
    let mut artifacts = artifacts.collect::<Vec<_>>();
    artifacts.sort_unstable_by_key(|artifact| artifact.semantic_group);
    artifacts.truncate(count);
    artifacts
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

fn treatment(
    name: &'static str,
    outcome: &reflex::SessionOutcome<LeanDomain>,
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
    Ok(TreatmentResult {
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
    })
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
