use std::collections::BTreeMap;
use std::ffi::OsString;
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use cpu_time::ProcessTime;
use reflex::{
    BundlePlan, Direction, DomainDefinition, GoalSet, ImprovementRequest, NonEmpty,
    NonZeroDuration, Objective, OptimizationGoal, Preference, ResourceEnvelope, StructuralProtocol,
    improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

mod build;
mod causal;
mod harness;
mod lean;
mod performance;
mod scaling;

use harness::{
    AnyError, HostEnvironment, capture_child, completion_name, duration_ns, environment, hash_json,
    hex, require_absent, require_clean, require_release,
};

const PROTOCOL_VERSION: &str = "reflex-bootstrap-baseline-v7";
const CORPUS_NAME: &str = "unary-u8-full-ops-development-v2";
const EXPECTED_SEMANTIC_OUTCOME: &str =
    "ecaf1feba1d8d45511b2b3b01fd85d0a9be829ac48814f3bc29e9daa1b8d5582";
const WARMUPS: u32 = 2;
const REPLICATES: u32 = 10;
const RESIDENT_BYTES: u64 = 1024 * 1024 * 1024;
const DURABLE_BYTES: u64 = 256 * 1024 * 1024;
const TIME_SECONDS: u64 = 120;
const VERIFICATION_REQUESTS: u64 = 100_000;

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Assignment {
    worker_threads: usize,
    storage: String,
    replicate: u32,
    warmup: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct ChildResult {
    assignment: Assignment,
    completion: String,
    session_wall_ns: u64,
    process_cpu_ns: u64,
    reported_elapsed_ns: u64,
    reported_cpu_ns: u64,
    resident_bytes: u64,
    verification_requests: u64,
    durable_bytes: u64,
    bundle_bytes: u64,
    pareto_artifacts: usize,
    observer_additions: u64,
    semantic_outcome_sha256: String,
    recovery_valid: bool,
    recovery_failure: Option<String>,
    recovery_wall_ns: Option<u64>,
    recovery_process_cpu_ns: Option<u64>,
    recovery_verification_requests: Option<u64>,
    recovery_semantic_outcome_sha256: Option<String>,
}

#[derive(Debug, Serialize)]
struct RecordedRun {
    assignment: Assignment,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    result: Option<ChildResult>,
    failure: Option<String>,
}

#[derive(Debug, Serialize)]
struct Protocol {
    version: &'static str,
    corpus: &'static str,
    corpus_sha256: String,
    warmups: u32,
    measured_replicates: u32,
    worker_treatments: Vec<usize>,
    storage_treatments: Vec<String>,
    resident_bytes: u64,
    durable_bytes: u64,
    elapsed_seconds: u64,
    cpu_seconds: u64,
    verification_requests: u64,
    status: &'static str,
}

#[derive(Debug, Serialize)]
struct Summary {
    worker_threads: usize,
    storage: String,
    successful_replicates: usize,
    wall_p50_ns: u64,
    wall_p95_ns: u64,
    cpu_p50_ns: u64,
    verifications_per_second_p50: u64,
    resident_bytes_p50: u64,
    bytes_per_pareto_artifact_p50: u64,
    recovery_wall_p50_ns: u64,
}

#[derive(Debug, Serialize)]
struct Report {
    schema: &'static str,
    protocol_sha256: String,
    protocol: Protocol,
    environment: HostEnvironment,
    protocol_deviations: Vec<String>,
    semantic_outcome_sha256: Option<String>,
    summaries: Vec<Summary>,
    runs: Vec<RecordedRun>,
    content_sha256: String,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("xtask: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), AnyError> {
    let mut arguments = std::env::args().skip(1);
    match arguments.next().as_deref() {
        Some("baseline") => {
            let arguments = arguments.collect::<Vec<_>>();
            let output = parse_output(&arguments)?;
            run_baseline(&output)
        }
        Some("baseline-child") => {
            let arguments = arguments.collect::<Vec<_>>();
            run_child(&arguments)
        }
        Some("causal-confirm") => {
            let arguments = arguments.collect::<Vec<_>>();
            causal::run_confirm(&arguments)
        }
        Some("causal-child") => {
            let arguments = arguments.collect::<Vec<_>>();
            causal::run_child(&arguments)
        }
        Some("causal-materialize-audit") => {
            let arguments = arguments.collect::<Vec<_>>();
            causal::materialize_audit(&arguments)
        }
        Some("causal-development-performance") => {
            let arguments = arguments.collect::<Vec<_>>();
            causal::run_development_performance(&arguments)
        }
        Some("build-native") => {
            let arguments = arguments.collect::<Vec<_>>();
            build::build_native(&arguments)
        }
        Some("perf-smoke") => {
            let arguments = arguments.collect::<Vec<_>>();
            performance::run(&arguments)
        }
        Some("instrumentation-overhead") => {
            let arguments = arguments.collect::<Vec<_>>();
            performance::run_instrumentation_overhead(&arguments)
        }
        Some("verification-scaling") => {
            let arguments = arguments.collect::<Vec<_>>();
            scaling::run(&arguments)
        }
        Some("verification-scaling-child") => {
            let arguments = arguments.collect::<Vec<_>>();
            scaling::run_child(&arguments)
        }
        Some("lean-development") => {
            let arguments = arguments.collect::<Vec<_>>();
            lean::run_development(&arguments)
        }
        Some("lean-catalog") => {
            let arguments = arguments.collect::<Vec<_>>();
            lean::build_catalog(&arguments)
        }
        Some("lean-catalog-check") => {
            let arguments = arguments.collect::<Vec<_>>();
            lean::check_catalog(&arguments)
        }
        _ => Err(
            "usage: cargo run --release -p xtask -- <baseline|causal-confirm|causal-materialize-audit|causal-development-performance|build-native|perf-smoke|instrumentation-overhead|verification-scaling|lean-catalog|lean-catalog-check|lean-development> [arguments]"
                .into(),
        ),
    }
}

fn parse_output(arguments: &[String]) -> Result<PathBuf, AnyError> {
    match arguments {
        [] => Ok(PathBuf::from(
            "docs/baselines/bootstrap-reference-domain-v7.json",
        )),
        [flag, path] if flag == "--output" => Ok(PathBuf::from(path)),
        _ => Err("baseline accepts only an optional --output PATH".into()),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the baseline controller keeps the frozen assignment and reporting protocol auditable"
)]
fn run_baseline(output: &Path) -> Result<(), AnyError> {
    require_release("baseline")?;
    let available = std::thread::available_parallelism()?.get();
    let worker_treatments = [1, 2, 4, 8]
        .into_iter()
        .filter(|workers| *workers <= available)
        .collect::<Vec<_>>();
    let corpus_sha256 = corpus_hash()?;
    let storage_treatments = vec!["filesystem".to_owned(), "ramfs".to_owned()];
    let protocol = Protocol {
        version: PROTOCOL_VERSION,
        corpus: CORPUS_NAME,
        corpus_sha256,
        warmups: WARMUPS,
        measured_replicates: REPLICATES,
        worker_treatments,
        storage_treatments,
        resident_bytes: RESIDENT_BYTES,
        durable_bytes: DURABLE_BYTES,
        elapsed_seconds: TIME_SECONDS,
        cpu_seconds: TIME_SECONDS,
        verification_requests: VERIFICATION_REQUESTS,
        status: "exploratory",
    };
    let protocol_sha256 = hash_json(&protocol)?;
    let environment = environment()?;
    require_clean(&environment, "baseline")?;
    require_absent(output, "baseline report")?;
    let filesystem_root = std::env::current_dir()?.join("target/reflex-baseline-work");
    std::fs::create_dir_all(&filesystem_root)?;
    let ramfs_root =
        PathBuf::from("/dev/shm").join(format!("reflex-baseline-{}", std::process::id()));
    let mut deviations = Vec::new();
    if let Err(error) = std::fs::create_dir(&ramfs_root) {
        deviations.push(format!("ramfs treatment unavailable: {error}"));
    }

    let executable = std::env::current_exe()?;
    let mut runs = Vec::new();
    for &worker_threads in &protocol.worker_treatments {
        for storage in &protocol.storage_treatments {
            let root = if storage == "ramfs" {
                if !ramfs_root.is_dir() {
                    continue;
                }
                &ramfs_root
            } else {
                &filesystem_root
            };
            for ordinal in 0..WARMUPS + REPLICATES {
                let assignment = Assignment {
                    worker_threads,
                    storage: storage.clone(),
                    warmup: ordinal < WARMUPS,
                    replicate: if ordinal < WARMUPS {
                        ordinal
                    } else {
                        ordinal - WARMUPS
                    },
                };
                let target = root.join(format!(
                    "workers-{worker_threads}-{storage}-{ordinal}.bundle"
                ));
                runs.push(run_assignment(&executable, assignment, &target)?);
                if target.is_file() {
                    std::fs::remove_file(target)?;
                }
            }
        }
    }
    if ramfs_root.is_dir() {
        std::fs::remove_dir(&ramfs_root)?;
    }
    if filesystem_root.is_dir() {
        std::fs::remove_dir(&filesystem_root)?;
    }

    let successful_digests = runs
        .iter()
        .filter_map(|run| run.result.as_ref())
        .map(|result| result.semantic_outcome_sha256.clone())
        .collect::<std::collections::BTreeSet<_>>();
    if successful_digests.len() > 1 {
        deviations.push("semantic outcomes differed across assigned runs".into());
    }
    let semantic_outcome_sha256 = successful_digests.into_iter().next();
    if semantic_outcome_sha256
        .as_deref()
        .is_some_and(|digest| digest != EXPECTED_SEMANTIC_OUTCOME)
    {
        deviations.push(format!(
            "semantic outcome differs from frozen Bootstrap comparator {EXPECTED_SEMANTIC_OUTCOME}"
        ));
    }
    let summaries = summarize(&runs);
    let failures = runs.iter().filter(|run| run.failure.is_some()).count();
    let deviation_count = deviations.len();
    let mut report = Report {
        schema: "reflex-performance-report-v1",
        protocol_sha256,
        protocol,
        environment,
        protocol_deviations: deviations,
        semantic_outcome_sha256,
        summaries,
        runs,
        content_sha256: String::new(),
    };
    report.content_sha256 = hash_json(&report)?;
    let bytes = serde_json::to_vec_pretty(&report)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, bytes)?;
    println!("wrote {} ({failures} failed assignments)", output.display());
    if failures == 0 && deviation_count == 0 {
        Ok(())
    } else {
        Err(format!(
            "{failures} baseline assignments failed and {deviation_count} protocol deviations occurred; raw evidence was retained"
        )
        .into())
    }
}

fn run_assignment(
    executable: &Path,
    assignment: Assignment,
    target: &Path,
) -> Result<RecordedRun, AnyError> {
    let arguments = [
        OsString::from("baseline-child"),
        OsString::from("--workers"),
        OsString::from(assignment.worker_threads.to_string()),
        OsString::from("--storage"),
        OsString::from(&assignment.storage),
        OsString::from("--replicate"),
        OsString::from(assignment.replicate.to_string()),
        OsString::from("--warmup"),
        OsString::from(if assignment.warmup { "true" } else { "false" }),
        OsString::from("--target"),
        target.as_os_str().to_owned(),
    ];
    let capture = capture_child(executable, &arguments, target, None)?;
    let (result, failure) =
        parse_child_output(capture.status.success(), &capture.stdout, &capture.stderr);
    Ok(RecordedRun {
        assignment,
        exit_code: capture.status.code(),
        stdout: capture.stdout,
        stderr: capture.stderr,
        result,
        failure,
    })
}

fn parse_child_output(
    success: bool,
    stdout: &str,
    stderr: &str,
) -> (Option<ChildResult>, Option<String>) {
    if !success {
        return (None, Some(format!("child failed: {stderr}")));
    }
    match serde_json::from_str::<ChildResult>(stdout.trim()) {
        Ok(result) => {
            let failure = (!result.recovery_valid).then(|| {
                format!(
                    "recovery failed: {}",
                    result.recovery_failure.as_deref().unwrap_or("unspecified")
                )
            });
            (Some(result), failure)
        }
        Err(error) => (None, Some(format!("malformed child output: {error}"))),
    }
}

fn run_child(arguments: &[String]) -> Result<(), AnyError> {
    let value = |flag: &str| -> Result<&str, AnyError> {
        arguments
            .windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
            .ok_or_else(|| format!("missing {flag}").into())
    };
    let assignment = Assignment {
        worker_threads: value("--workers")?.parse()?,
        storage: value("--storage")?.to_owned(),
        replicate: value("--replicate")?.parse()?,
        warmup: value("--warmup")?.parse()?,
    };
    let target = PathBuf::from(value("--target")?);
    let result = measure_assignment(assignment, &target)?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn measure_assignment(assignment: Assignment, target: &Path) -> Result<ChildResult, AnyError> {
    let mut observer_additions = 0_u64;
    let wall_started = Instant::now();
    let cpu_started = ProcessTime::try_now()?;
    let outcome = improve(
        BitVecDomain::unary_u8(),
        request(
            assignment.worker_threads,
            corpus(),
            BundlePlan::Fresh {
                target: target.to_path_buf(),
            },
        )?,
        |update| {
            observer_additions = observer_additions
                .saturating_add(u64::try_from(update.added().len()).unwrap_or(u64::MAX));
            ControlFlow::Continue(())
        },
    )?;
    let session_wall_ns = duration_ns(wall_started.elapsed());
    let process_cpu_ns = duration_ns(cpu_started.try_elapsed()?);
    let semantic_outcome_sha256 = outcome_digest(outcome.pareto().artifacts());
    let usage = outcome.usage();
    let bundle_bytes = std::fs::metadata(target)?.len();

    let recovery = (|| -> Result<(u64, u64, u64, String), AnyError> {
        let recovery_wall_started = Instant::now();
        let recovery_cpu_started = ProcessTime::try_now()?;
        let recovered = improve(
            BitVecDomain::unary_u8(),
            request(
                assignment.worker_threads,
                corpus(),
                BundlePlan::Resume {
                    source: target.to_path_buf(),
                    target: target.to_path_buf(),
                },
            )?,
            |_| ControlFlow::Continue(()),
        )?;
        Ok((
            duration_ns(recovery_wall_started.elapsed()),
            duration_ns(recovery_cpu_started.try_elapsed()?),
            recovered.usage().verification_requests,
            outcome_digest(recovered.pareto().artifacts()),
        ))
    })();
    let (
        recovery_valid,
        recovery_failure,
        recovery_wall_ns,
        recovery_process_cpu_ns,
        recovery_verification_requests,
        recovery_semantic_outcome_sha256,
    ) = match recovery {
        Ok((wall, cpu, requests, digest)) if digest == semantic_outcome_sha256 => (
            true,
            None,
            Some(wall),
            Some(cpu),
            Some(requests),
            Some(digest),
        ),
        Ok((wall, cpu, requests, digest)) => (
            false,
            Some("completed recovery changed the Pareto Artifact-key digest".into()),
            Some(wall),
            Some(cpu),
            Some(requests),
            Some(digest),
        ),
        Err(error) => (false, Some(error.to_string()), None, None, None, None),
    };
    Ok(ChildResult {
        assignment,
        completion: completion_name(outcome.completion()).into(),
        session_wall_ns,
        process_cpu_ns,
        reported_elapsed_ns: duration_ns(usage.elapsed_time),
        reported_cpu_ns: duration_ns(usage.cpu_time),
        resident_bytes: usage.resident_bytes,
        verification_requests: usage.verification_requests,
        durable_bytes: usage.durable_bytes,
        bundle_bytes,
        pareto_artifacts: outcome.pareto().artifacts().len(),
        observer_additions,
        semantic_outcome_sha256,
        recovery_valid,
        recovery_failure,
        recovery_wall_ns,
        recovery_process_cpu_ns,
        recovery_verification_requests,
        recovery_semantic_outcome_sha256,
    })
}

fn request(
    worker_threads: usize,
    seeds: NonEmpty<Expression>,
    bundle: BundlePlan,
) -> Result<ImprovementRequest<BitVecDomain>, AnyError> {
    let objectives = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference = Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), [])?;
    Ok(ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, None)?),
        SeedScope::new(seeds),
        ResourceEnvelope::new(
            NonZeroUsize::new(worker_threads).ok_or("workers must be nonzero")?,
            NonZeroU64::new(RESIDENT_BYTES).unwrap(),
            NonZeroU64::new(DURABLE_BYTES).unwrap(),
            NonZeroDuration::new(Duration::from_secs(TIME_SECONDS)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(TIME_SECONDS)).unwrap(),
            NonZeroU64::new(VERIFICATION_REQUESTS).unwrap(),
        ),
        bundle,
    )?)
}

fn corpus() -> NonEmpty<Expression> {
    NonEmpty::try_from_iter((u8::MIN..=u8::MAX).map(|constant| {
        let semantic = Expression::xor(Expression::input(), Expression::constant(constant));
        Expression::xor(
            Expression::xor(semantic, Expression::constant(0)),
            Expression::constant(0),
        )
    }))
    .expect("the fixed u8 corpus is nonempty")
}

fn corpus_hash() -> Result<String, AnyError> {
    let domain = BitVecDomain::unary_u8();
    let mut digest = Sha256::new();
    digest.update(CORPUS_NAME.as_bytes());
    let mut scratch = ();
    for expression in corpus().into_vec() {
        let mut canonical = Vec::new();
        domain
            .structure()
            .encode_canonical(&expression, &mut canonical, &mut scratch)?;
        digest.update((canonical.len() as u64).to_le_bytes());
        digest.update(canonical);
    }
    Ok(hex(&digest.finalize()))
}

fn outcome_digest<D: DomainDefinition>(artifacts: &[reflex::VerifiedArtifact<D>]) -> String {
    let mut keys = artifacts
        .iter()
        .map(reflex::VerifiedArtifact::key)
        .collect::<Vec<_>>();
    keys.sort_unstable();
    let mut digest = Sha256::new();
    for key in keys {
        digest.update(key.as_bytes());
    }
    hex(&digest.finalize())
}

fn summarize(runs: &[RecordedRun]) -> Vec<Summary> {
    let mut groups = BTreeMap::<(usize, String), Vec<&ChildResult>>::new();
    for run in runs {
        if !run.assignment.warmup
            && let Some(result) = run.result.as_ref()
        {
            groups
                .entry((
                    run.assignment.worker_threads,
                    run.assignment.storage.clone(),
                ))
                .or_default()
                .push(result);
        }
    }
    groups
        .into_iter()
        .map(|((worker_threads, storage), results)| {
            let wall = results
                .iter()
                .map(|result| result.session_wall_ns)
                .collect();
            let cpu = results.iter().map(|result| result.process_cpu_ns).collect();
            let throughput = results
                .iter()
                .map(|result| {
                    result
                        .verification_requests
                        .saturating_mul(1_000_000_000)
                        .checked_div(result.session_wall_ns.max(1))
                        .unwrap_or(0)
                })
                .collect();
            let resident = results.iter().map(|result| result.resident_bytes).collect();
            let bytes_per_artifact = results
                .iter()
                .map(|result| {
                    result
                        .bundle_bytes
                        .checked_div(result.pareto_artifacts.max(1) as u64)
                        .unwrap_or(0)
                })
                .collect();
            let recovery = results
                .iter()
                .filter_map(|result| result.recovery_wall_ns)
                .collect();
            Summary {
                worker_threads,
                storage,
                successful_replicates: results.len(),
                wall_p50_ns: quantile(wall, 50),
                wall_p95_ns: quantile(
                    results
                        .iter()
                        .map(|result| result.session_wall_ns)
                        .collect(),
                    95,
                ),
                cpu_p50_ns: quantile(cpu, 50),
                verifications_per_second_p50: quantile(throughput, 50),
                resident_bytes_p50: quantile(resident, 50),
                bytes_per_pareto_artifact_p50: quantile(bytes_per_artifact, 50),
                recovery_wall_p50_ns: quantile(recovery, 50),
            }
        })
        .collect()
}

fn quantile(mut values: Vec<u64>, percentile: usize) -> u64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let index = (values.len() - 1).saturating_mul(percentile).div_ceil(100);
    values[index.min(values.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_and_protocol_are_content_addressed() {
        let corpus = corpus_hash().unwrap();
        assert_eq!(
            corpus,
            "c5c962fcd9f8016d6b5e5dec31890af5f9d6e5067402ead17637086a21dde95c"
        );
        let protocol = Protocol {
            version: PROTOCOL_VERSION,
            corpus: CORPUS_NAME,
            corpus_sha256: corpus,
            warmups: WARMUPS,
            measured_replicates: REPLICATES,
            worker_treatments: vec![1, 2, 4, 8],
            storage_treatments: vec!["filesystem".into(), "ramfs".into()],
            resident_bytes: RESIDENT_BYTES,
            durable_bytes: DURABLE_BYTES,
            elapsed_seconds: TIME_SECONDS,
            cpu_seconds: TIME_SECONDS,
            verification_requests: VERIFICATION_REQUESTS,
            status: "exploratory",
        };
        assert_eq!(
            hash_json(&protocol).unwrap(),
            "14b108b35f336c134fa94139de5430adba0904f8471ffa44de4fb7c55fc0b880"
        );
    }

    #[test]
    fn malformed_and_failed_children_are_retained_as_failures() {
        let (result, malformed) = parse_child_output(true, "not-json", "");
        let (failed_result, failed) = parse_child_output(false, "", "killed");
        assert!(
            result.is_none()
                && malformed.is_some_and(|failure| failure.contains("malformed"))
                && failed_result.is_none()
                && failed.is_some_and(|failure| failure.contains("killed"))
        );
    }

    #[test]
    fn aggregation_uses_declared_nearest_rank_percentiles() {
        assert_eq!(quantile(vec![40, 10, 30, 20], 50), 30);
        assert_eq!(quantile(vec![40, 10, 30, 20], 95), 40);
        assert_eq!(quantile(Vec::new(), 50), 0);
    }
}
