use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::harness::{
    AnyError, HostEnvironment, capture_child_with_environment, duration_ns, environment,
    require_release,
};
use crate::{Assignment, ChildResult, parse_child_output, quantile};

const WARMUPS: u32 = 1;
const REPLICATES: u32 = 5;
const CHILD_TIMEOUT: Duration = Duration::from_secs(30);
const OVERHEAD_WARMUP_PAIRS: u32 = 2;
const OVERHEAD_REPLICATES: u32 = 15;
const ONE_PERCENT_PARTS_PER_MILLION: i64 = 10_000;

#[derive(Debug, Serialize)]
struct RecordedRun {
    order: u32,
    instrumented: bool,
    controller_wall_ns: u64,
    process_tree_cpu_ns: u64,
    peak_process_tree_resident_bytes: u64,
    exit_code: Option<i32>,
    result: Option<ChildResult>,
    failure: Option<String>,
    phase_reports: Vec<String>,
}

#[derive(Debug, Serialize)]
struct Summary {
    successful_replicates: usize,
    controller_wall_p50_ns: u64,
    controller_wall_p95_ns: u64,
    session_wall_p50_ns: u64,
    session_wall_p95_ns: u64,
    session_cpu_p50_ns: u64,
    process_tree_cpu_p50_ns: u64,
    peak_process_tree_resident_bytes_p50: u64,
    verifications_per_second_p50: u64,
    recovery_wall_p50_ns: u64,
}

#[derive(Debug, Serialize)]
struct Report {
    schema: &'static str,
    status: &'static str,
    warning: &'static str,
    environment: HostEnvironment,
    warmups: u32,
    measured_replicates: u32,
    summary: Summary,
    runs: Vec<RecordedRun>,
}

#[derive(Debug, Serialize)]
struct OverheadSummary {
    successful_pairs: usize,
    session_wall_overhead_p50_parts_per_million: i64,
    session_cpu_overhead_p50_parts_per_million: i64,
    passes_one_percent_gate: bool,
}

#[derive(Debug, Serialize)]
struct OverheadReport {
    schema: &'static str,
    status: &'static str,
    warning: &'static str,
    environment: HostEnvironment,
    warmup_pairs: u32,
    measured_pairs: u32,
    threshold_parts_per_million: i64,
    summary: OverheadSummary,
    runs: Vec<RecordedRun>,
}

pub(super) fn run(arguments: &[String]) -> Result<(), AnyError> {
    let output = parse_output(arguments)?;
    require_release("perf-smoke")?;
    let environment = environment()?;
    let root = std::env::current_dir()?.join(format!(
        "target/reflex-development-performance-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let result = run_all(&root, environment);
    let cleanup = std::fs::remove_dir(&root);
    let report = result?;
    cleanup?;
    let bytes = serde_json::to_vec_pretty(&report)?;
    if let Some(output) = output {
        if output.exists() {
            return Err(format!(
                "performance smoke report already exists: {}",
                output.display()
            )
            .into());
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&output, bytes)?;
        println!("wrote {}", output.display());
    } else {
        println!("{}", String::from_utf8(bytes)?);
    }
    if report.runs.iter().any(|run| run.failure.is_some()) {
        return Err("one or more development performance assignments failed".into());
    }
    Ok(())
}

pub(super) fn run_instrumentation_overhead(arguments: &[String]) -> Result<(), AnyError> {
    let output = parse_output(arguments)?;
    require_release("instrumentation-overhead")?;
    let environment = environment()?;
    let root = std::env::current_dir()?.join(format!(
        "target/reflex-instrumentation-overhead-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let result = run_overhead_pairs(&root, environment);
    let cleanup = std::fs::remove_dir(&root);
    let report = result?;
    cleanup?;
    let bytes = serde_json::to_vec_pretty(&report)?;
    if let Some(output) = output {
        if output.exists() {
            return Err(format!("overhead report already exists: {}", output.display()).into());
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&output, bytes)?;
        println!("wrote {}", output.display());
    } else {
        println!("{}", String::from_utf8(bytes)?);
    }
    if report.runs.iter().any(|run| run.failure.is_some()) {
        return Err("one or more instrumentation overhead assignments failed".into());
    }
    if !report.summary.passes_one_percent_gate {
        return Err("private instrumentation exceeded the one-percent median overhead gate".into());
    }
    Ok(())
}

fn parse_output(arguments: &[String]) -> Result<Option<PathBuf>, AnyError> {
    match arguments {
        [] => Ok(None),
        [flag, path] if flag == "--output" => Ok(Some(PathBuf::from(path))),
        _ => Err("perf-smoke accepts only an optional --output PATH".into()),
    }
}

fn run_all(root: &Path, environment: HostEnvironment) -> Result<Report, AnyError> {
    let executable = std::env::current_exe()?;
    let mut runs = Vec::new();
    let total_runs = WARMUPS.saturating_add(REPLICATES);
    for order in 0..total_runs {
        let assignment = Assignment {
            worker_threads: 1,
            storage: "filesystem".into(),
            replicate: order.saturating_sub(WARMUPS),
            warmup: order < WARMUPS,
        };
        let target = root.join(format!("run-{order}.bundle"));
        runs.push(run_assignment(
            &executable,
            &assignment,
            order,
            &target,
            None,
        )?);
        if target.is_file() {
            std::fs::remove_file(target)?;
        }
    }
    Ok(Report {
        schema: "reflex-development-performance-v1",
        status: "development-only",
        warning: "This short dirty-tree diagnostic is not scientific confirmation evidence.",
        environment,
        warmups: WARMUPS,
        measured_replicates: REPLICATES,
        summary: summarize(&runs),
        runs,
    })
}

fn run_overhead_pairs(
    root: &Path,
    environment: HostEnvironment,
) -> Result<OverheadReport, AnyError> {
    let executable = std::env::current_exe()?;
    let mut runs = Vec::new();
    let pair_count = OVERHEAD_WARMUP_PAIRS.saturating_add(OVERHEAD_REPLICATES);
    for pair in 0..pair_count {
        let warmup = pair < OVERHEAD_WARMUP_PAIRS;
        for treatment_order in 0..2_u32 {
            let instrumented = treatment_order == pair % 2;
            let order = pair.saturating_mul(2).saturating_add(treatment_order);
            let assignment = Assignment {
                worker_threads: 1,
                storage: "filesystem".into(),
                replicate: pair.saturating_sub(OVERHEAD_WARMUP_PAIRS),
                warmup,
            };
            let target = root.join(format!("run-{order}.bundle"));
            let report_prefix = instrumented.then(|| root.join(format!("phase-{order}")));
            runs.push(run_assignment(
                &executable,
                &assignment,
                order,
                &target,
                report_prefix.as_deref(),
            )?);
            if target.is_file() {
                std::fs::remove_file(target)?;
            }
        }
    }
    let summary = summarize_overhead(&runs);
    Ok(OverheadReport {
        schema: "reflex-instrumentation-overhead-v1",
        status: "development-only",
        warning: "This paired dirty-tree diagnostic is not scientific confirmation evidence.",
        environment,
        warmup_pairs: OVERHEAD_WARMUP_PAIRS,
        measured_pairs: OVERHEAD_REPLICATES,
        threshold_parts_per_million: ONE_PERCENT_PARTS_PER_MILLION,
        summary,
        runs,
    })
}

fn run_assignment(
    executable: &Path,
    assignment: &Assignment,
    order: u32,
    target: &Path,
    phase_report_prefix: Option<&Path>,
) -> Result<RecordedRun, AnyError> {
    let arguments = [
        OsString::from("performance-child"),
        OsString::from("--workers"),
        OsString::from(assignment.worker_threads.to_string()),
        OsString::from("--storage"),
        OsString::from(&assignment.storage),
        OsString::from("--replicate"),
        OsString::from(assignment.replicate.to_string()),
        OsString::from("--warmup"),
        OsString::from(assignment.warmup.to_string()),
        OsString::from("--target"),
        target.as_os_str().to_owned(),
    ];
    let child_environment = phase_report_prefix.map_or_else(Vec::new, |prefix| {
        vec![(
            OsString::from("REFLEX_INTERNAL_PHASE_REPORT_PREFIX"),
            prefix.as_os_str().to_owned(),
        )]
    });
    let started = Instant::now();
    let capture = capture_child_with_environment(
        executable,
        &arguments,
        target,
        Some(CHILD_TIMEOUT),
        &child_environment,
    )?;
    let controller_wall_ns = duration_ns(started.elapsed());
    let (result, failure) = if capture.timed_out {
        (
            None,
            Some("child exceeded the 30-second smoke timeout".into()),
        )
    } else if capture.output_limit_exceeded {
        (
            None,
            Some("child exceeded the bounded diagnostic-output allowance".into()),
        )
    } else {
        parse_child_output(capture.status.success(), &capture.stdout, &capture.stderr)
    };
    let phase_reports = phase_report_prefix.map_or_else(Vec::new, |prefix| {
        (0..2_u32)
            .filter_map(|ordinal| {
                let mut path = prefix.to_path_buf();
                path.set_extension(format!("{ordinal}.phase"));
                let report = std::fs::read_to_string(&path).ok()?;
                let _ = std::fs::remove_file(path);
                Some(report)
            })
            .collect()
    });
    let instrumented = phase_report_prefix.is_some();
    let failure = failure.or_else(|| {
        (instrumented && phase_reports.len() != 2)
            .then(|| "instrumented child did not emit both aggregate phase reports".into())
    });
    Ok(RecordedRun {
        order,
        instrumented,
        controller_wall_ns,
        process_tree_cpu_ns: capture.process_tree_cpu_ns,
        peak_process_tree_resident_bytes: capture.peak_process_tree_resident_bytes,
        exit_code: capture.status.code(),
        result,
        failure,
        phase_reports,
    })
}

fn summarize(runs: &[RecordedRun]) -> Summary {
    let measured = runs
        .iter()
        .filter(|run| {
            run.result
                .as_ref()
                .is_some_and(|result| !result.assignment.warmup)
        })
        .collect::<Vec<_>>();
    let results = measured
        .iter()
        .filter_map(|run| run.result.as_ref())
        .collect::<Vec<_>>();
    Summary {
        successful_replicates: results.len(),
        controller_wall_p50_ns: quantile(
            measured.iter().map(|run| run.controller_wall_ns).collect(),
            50,
        ),
        controller_wall_p95_ns: quantile(
            measured.iter().map(|run| run.controller_wall_ns).collect(),
            95,
        ),
        session_wall_p50_ns: quantile(
            results
                .iter()
                .map(|result| result.session_wall_ns)
                .collect(),
            50,
        ),
        session_wall_p95_ns: quantile(
            results
                .iter()
                .map(|result| result.session_wall_ns)
                .collect(),
            95,
        ),
        session_cpu_p50_ns: quantile(
            results.iter().map(|result| result.process_cpu_ns).collect(),
            50,
        ),
        process_tree_cpu_p50_ns: quantile(
            measured.iter().map(|run| run.process_tree_cpu_ns).collect(),
            50,
        ),
        peak_process_tree_resident_bytes_p50: quantile(
            measured
                .iter()
                .map(|run| run.peak_process_tree_resident_bytes)
                .collect(),
            50,
        ),
        verifications_per_second_p50: quantile(
            results
                .iter()
                .map(|result| {
                    result
                        .verification_requests
                        .saturating_mul(1_000_000_000)
                        .checked_div(result.session_wall_ns.max(1))
                        .unwrap_or(0)
                })
                .collect(),
            50,
        ),
        recovery_wall_p50_ns: quantile(
            results
                .iter()
                .filter_map(|result| result.recovery_wall_ns)
                .collect(),
            50,
        ),
    }
}

fn summarize_overhead(runs: &[RecordedRun]) -> OverheadSummary {
    let mut wall = Vec::new();
    let mut cpu = Vec::new();
    for replicate in 0..OVERHEAD_REPLICATES {
        let matching = runs.iter().filter(|run| {
            run.result.as_ref().is_some_and(|result| {
                !result.assignment.warmup && result.assignment.replicate == replicate
            })
        });
        let mut off = None;
        let mut on = None;
        for run in matching {
            if run.instrumented {
                on = run.result.as_ref();
            } else {
                off = run.result.as_ref();
            }
        }
        if let Some((off, on)) = off.zip(on) {
            wall.push(overhead_parts_per_million(
                off.session_wall_ns,
                on.session_wall_ns,
            ));
            cpu.push(overhead_parts_per_million(
                off.process_cpu_ns,
                on.process_cpu_ns,
            ));
        }
    }
    let successful_pairs = wall.len();
    let wall_p50 = signed_quantile(wall, 50);
    let cpu_p50 = signed_quantile(cpu, 50);
    OverheadSummary {
        successful_pairs,
        session_wall_overhead_p50_parts_per_million: wall_p50,
        session_cpu_overhead_p50_parts_per_million: cpu_p50,
        passes_one_percent_gate: successful_pairs == OVERHEAD_REPLICATES as usize
            && wall_p50 < ONE_PERCENT_PARTS_PER_MILLION
            && cpu_p50 < ONE_PERCENT_PARTS_PER_MILLION,
    }
}

fn overhead_parts_per_million(baseline: u64, instrumented: u64) -> i64 {
    let difference = i128::from(instrumented) - i128::from(baseline);
    let ratio = difference
        .saturating_mul(1_000_000)
        .checked_div(i128::from(baseline.max(1)))
        .unwrap_or(i128::MAX);
    i64::try_from(ratio).unwrap_or(if ratio.is_negative() {
        i64::MIN
    } else {
        i64::MAX
    })
}

fn signed_quantile(mut values: Vec<i64>, percentile: usize) -> i64 {
    if values.is_empty() {
        return 0;
    }
    values.sort_unstable();
    let index = (values.len() - 1).saturating_mul(percentile).div_ceil(100);
    values[index.min(values.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::{overhead_parts_per_million, parse_output, signed_quantile};

    #[test]
    fn output_is_optional_but_explicit() {
        assert_eq!(parse_output(&[]).unwrap(), None);
        assert_eq!(
            parse_output(&["--output".into(), "report.json".into()]).unwrap(),
            Some("report.json".into())
        );
        assert!(parse_output(&["report.json".into()]).is_err());
    }

    #[test]
    fn overhead_is_reported_as_signed_parts_per_million() {
        assert_eq!(overhead_parts_per_million(1_000, 1_010), 10_000);
        assert_eq!(overhead_parts_per_million(1_000, 990), -10_000);
        assert_eq!(signed_quantile(vec![-20, 0, 10, 30, 40], 50), 10);
    }
}
