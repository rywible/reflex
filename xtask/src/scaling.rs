use std::ffi::OsString;
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cpu_time::ProcessTime;
use reflex::{
    BundlePlan, Direction, GoalSet, ImprovementRequest, NonEmpty, NonZeroDuration, Objective,
    OptimizationGoal, Preference, ResourceEnvelope, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};
use serde::{Deserialize, Serialize};

use crate::harness::{
    AnyError, capture_large_campaign_child, duration_ns, environment, require_absent,
    require_release,
};
use crate::quantile;

const WARMUPS: u32 = 1;
const REPLICATES: u32 = 5;
const EXPRESSION_LEAVES: usize = 256;
const SEEDS: u8 = 32;
const ONE_LANE: usize = 1;
const MANY_LANES: usize = 7;
const REQUIRED_SPEEDUP_MILLIS: u64 = 6_000;
const MAX_CPU_RATIO_MILLIS: u64 = 1_200;

#[derive(Debug, Deserialize, Serialize)]
struct ChildResult {
    workers: usize,
    wall_ns: u64,
    cpu_ns: u64,
    verification_requests: u64,
    pareto_artifacts: usize,
    semantic_outcome_sha256: String,
}

#[derive(Debug, Serialize)]
struct RecordedRun {
    workers: usize,
    warmup: bool,
    replicate: u32,
    verification_wall_ns: u64,
    child: ChildResult,
}

#[derive(Debug, Serialize)]
struct Summary {
    one_lane_verification_wall_p50_ns: u64,
    many_lane_verification_wall_p50_ns: u64,
    verification_speedup_millis: u64,
    one_lane_cpu_p50_ns: u64,
    many_lane_cpu_p50_ns: u64,
    aggregate_cpu_ratio_millis: u64,
    semantic_outcomes_match: bool,
    passes_six_x_wall_gate: bool,
    passes_twenty_percent_cpu_gate: bool,
}

#[derive(Debug, Serialize)]
struct Report {
    schema: &'static str,
    status: &'static str,
    warning: &'static str,
    environment: crate::harness::HostEnvironment,
    expression_leaves: usize,
    seeds: u8,
    warmups: u32,
    measured_replicates: u32,
    lane_treatments: [usize; 2],
    summary: Summary,
    runs: Vec<RecordedRun>,
}

pub(super) fn run(arguments: &[String]) -> Result<(), AnyError> {
    require_release("verification-scaling")?;
    let output = match arguments {
        [flag, path] if flag == "--output" => PathBuf::from(path),
        _ => return Err("verification-scaling requires --output PATH".into()),
    };
    require_absent(&output, "verification scaling report")?;
    if std::thread::available_parallelism()?.get() < MANY_LANES {
        return Err("verification-scaling requires at least seven experimental CPU lanes".into());
    }
    let host = environment()?;
    let root = std::env::current_dir()?.join(format!(
        "target/reflex-verification-scaling-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let result = run_all(&root, host);
    let cleanup = std::fs::remove_dir_all(&root);
    let report = result?;
    cleanup?;
    let summary = &report.summary;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    println!("wrote {}", output.display());
    if !summary.passes_six_x_wall_gate
        || !summary.passes_twenty_percent_cpu_gate
        || !summary.semantic_outcomes_match
    {
        return Err("candidate-heavy verification scaling gate failed".into());
    }
    Ok(())
}

fn run_all(root: &Path, host: crate::harness::HostEnvironment) -> Result<Report, AnyError> {
    let executable = std::env::current_exe()?;
    let mut runs = Vec::new();
    let total_runs = WARMUPS.saturating_add(REPLICATES);
    for workers in [ONE_LANE, MANY_LANES] {
        for ordinal in 0..total_runs {
            let warmup = ordinal < WARMUPS;
            let replicate = ordinal.saturating_sub(WARMUPS);
            let prefix = root.join(format!("workers-{workers}-run-{ordinal}"));
            let target = prefix.with_extension("bundle");
            let arguments = [
                OsString::from("verification-scaling-child"),
                OsString::from("--workers"),
                OsString::from(workers.to_string()),
                OsString::from("--target"),
                target.as_os_str().to_owned(),
            ];
            let environment = [(
                OsString::from("REFLEX_INTERNAL_PHASE_REPORT_PREFIX"),
                prefix.as_os_str().to_owned(),
            )];
            let capture = capture_large_campaign_child(
                &executable,
                &arguments,
                &prefix,
                Some(Duration::from_secs(10)),
                None,
                &environment,
            )?;
            if capture.timed_out || capture.output_limit_exceeded || !capture.status.success() {
                return Err(
                    format!("verification scaling child failed: {}", capture.stderr).into(),
                );
            }
            let child: ChildResult = serde_json::from_str(capture.stdout.trim())?;
            let phase = std::fs::read_to_string(prefix.with_extension("0.phase"))?;
            let verification_wall_ns = phase_value(&phase, "verification_kernel_ns")?;
            std::fs::remove_file(prefix.with_extension("0.phase"))?;
            std::fs::remove_file(target)?;
            runs.push(RecordedRun {
                workers,
                warmup,
                replicate,
                verification_wall_ns,
                child,
            });
        }
    }
    let values = |workers, field: fn(&RecordedRun) -> u64| {
        runs.iter()
            .filter(|run| !run.warmup && run.workers == workers)
            .map(field)
            .collect::<Vec<_>>()
    };
    let one_wall = quantile(values(ONE_LANE, |run| run.verification_wall_ns), 50);
    let many_wall = quantile(values(MANY_LANES, |run| run.verification_wall_ns), 50);
    let one_cpu = quantile(values(ONE_LANE, |run| run.child.cpu_ns), 50);
    let many_cpu = quantile(values(MANY_LANES, |run| run.child.cpu_ns), 50);
    let speedup = one_wall
        .saturating_mul(1_000)
        .checked_div(many_wall.max(1))
        .unwrap_or(0);
    let cpu_ratio = many_cpu
        .saturating_mul(1_000)
        .checked_div(one_cpu.max(1))
        .unwrap_or(u64::MAX);
    let semantic_outcomes_match = runs
        .iter()
        .map(|run| &run.child.semantic_outcome_sha256)
        .collect::<std::collections::BTreeSet<_>>()
        .len()
        == 1;
    Ok(Report {
        schema: "reflex-candidate-verification-scaling-v2",
        status: "development-only",
        warning: "This bounded gate is performance evidence, not Scientific Confirmation.",
        environment: host,
        expression_leaves: EXPRESSION_LEAVES,
        seeds: SEEDS,
        warmups: WARMUPS,
        measured_replicates: REPLICATES,
        lane_treatments: [ONE_LANE, MANY_LANES],
        summary: Summary {
            one_lane_verification_wall_p50_ns: one_wall,
            many_lane_verification_wall_p50_ns: many_wall,
            verification_speedup_millis: speedup,
            one_lane_cpu_p50_ns: one_cpu,
            many_lane_cpu_p50_ns: many_cpu,
            aggregate_cpu_ratio_millis: cpu_ratio,
            semantic_outcomes_match,
            passes_six_x_wall_gate: speedup >= REQUIRED_SPEEDUP_MILLIS,
            passes_twenty_percent_cpu_gate: cpu_ratio <= MAX_CPU_RATIO_MILLIS,
        },
        runs,
    })
}

pub(super) fn run_child(arguments: &[String]) -> Result<(), AnyError> {
    let value = |flag: &str| -> Result<&str, AnyError> {
        arguments
            .windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
            .ok_or_else(|| format!("missing {flag}").into())
    };
    let workers = value("--workers")?.parse::<usize>()?;
    let target = PathBuf::from(value("--target")?);
    let started = Instant::now();
    let cpu_started = ProcessTime::try_now()?;
    let outcome = improve(BitVecDomain::unary_u8(), request(workers, target)?, |_| {
        ControlFlow::Continue(())
    })?;
    let result = ChildResult {
        workers,
        wall_ns: duration_ns(started.elapsed()),
        cpu_ns: duration_ns(cpu_started.try_elapsed()?),
        verification_requests: outcome.usage().verification_requests,
        pareto_artifacts: outcome.pareto().artifacts().len(),
        semantic_outcome_sha256: crate::outcome_digest(outcome.pareto().artifacts()),
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn request(workers: usize, target: PathBuf) -> Result<ImprovementRequest<BitVecDomain>, AnyError> {
    let objective = NonEmpty::one(Objective::new(Metric::NodeCount, Direction::Minimize));
    let preference = Preference::tiered(NonEmpty::one(NonEmpty::one(Metric::NodeCount)), [])?;
    Ok(ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objective, preference, None)?),
        SeedScope::new(corpus()),
        ResourceEnvelope::new(
            NonZeroUsize::new(workers).ok_or("workers must be nonzero")?,
            NonZeroU64::new(512 * 1024 * 1024).unwrap(),
            NonZeroU64::new(128 * 1024 * 1024).unwrap(),
            NonZeroDuration::new(Duration::from_secs(30)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(30)).unwrap(),
            NonZeroU64::new(544).unwrap(),
        ),
        BundlePlan::Fresh { target },
    )?)
}

fn corpus() -> NonEmpty<Expression> {
    let mut level = (0..EXPRESSION_LEAVES)
        .map(|value| {
            let constant = u8::try_from(value % 256).expect("value is reduced to the u8 range");
            Expression::wrapping_add(Expression::input(), Expression::constant(constant))
        })
        .collect::<Vec<_>>();
    while level.len() > 1 {
        level = level
            .chunks(2)
            .map(|pair| match pair {
                [left, right] => Expression::wrapping_add(left.clone(), right.clone()),
                [last] => last.clone(),
                _ => unreachable!("chunks of two are never empty"),
            })
            .collect();
    }
    let base = level.pop().expect("the balanced expression is nonempty");
    NonEmpty::try_from_iter(
        (0..SEEDS).map(|value| Expression::xor(base.clone(), Expression::constant(value))),
    )
    .expect("the scaling corpus is nonempty")
}

fn phase_value(report: &str, name: &str) -> Result<u64, AnyError> {
    report
        .lines()
        .find_map(|line| line.strip_prefix(name)?.strip_prefix('=')?.parse().ok())
        .ok_or_else(|| format!("phase report omitted {name}").into())
}

#[cfg(test)]
mod tests {
    use super::{MANY_LANES, ONE_LANE};

    #[test]
    fn scaling_treatments_fit_an_eight_cpu_host_with_one_reserved_cpu() {
        assert_eq!([ONE_LANE, MANY_LANES], [1, 7]);
        assert_eq!(MANY_LANES + 1, 8);
    }
}
