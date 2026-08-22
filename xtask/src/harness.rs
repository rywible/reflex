use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use reflex::Completion;
use serde::Serialize;
use sha2::{Digest, Sha256};

const ISOLATION_MEMORY_LIMIT: &str = "REFLEX_HOST_ISOLATION_MEMORY_LIMIT";
const ISOLATION_MEMORY_RESERVE: &str = "REFLEX_HOST_ISOLATION_MEMORY_RESERVE";
const ISOLATION_TOTAL_MEMORY: &str = "REFLEX_HOST_ISOLATION_TOTAL_MEMORY";
const ISOLATION_AVAILABLE_MEMORY: &str = "REFLEX_HOST_ISOLATION_AVAILABLE_MEMORY";
const ISOLATION_ALLOWED_CPUS: &str = "REFLEX_HOST_ISOLATION_ALLOWED_CPUS";
const ISOLATION_RESERVED_CPUS: &str = "REFLEX_HOST_ISOLATION_RESERVED_CPUS";

pub(super) type AnyError = Box<dyn std::error::Error>;

#[derive(Debug, Serialize)]
pub(super) struct HostEnvironment {
    pub(super) git_revision: String,
    pub(super) git_dirty: bool,
    pub(super) rustc: String,
    pub(super) cargo: String,
    pub(super) build_profile: &'static str,
    pub(super) rustflags: &'static str,
    pub(super) target_features: &'static str,
    pub(super) target_arch: &'static str,
    pub(super) target_os: &'static str,
    pub(super) cpu_description: String,
    pub(super) available_parallelism: usize,
}

pub(super) struct ChildCapture {
    pub(super) status: ExitStatus,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) timed_out: bool,
    pub(super) resident_limit_exceeded: bool,
    pub(super) process_tree_cpu_ns: u64,
    pub(super) peak_process_tree_resident_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct HostIsolation {
    pub(super) total_memory_bytes: u64,
    pub(super) available_memory_bytes: u64,
    pub(super) memory_limit_bytes: u64,
    pub(super) memory_reserve_bytes: u64,
    pub(super) allowed_cpu_list: String,
    pub(super) reserved_cpus: Vec<usize>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct HostIsolationPolicy {
    pub(super) memory_limit_bytes: u64,
    pub(super) memory_reserve_bytes: u64,
    pub(super) cpu_reserve: usize,
}

pub(super) fn require_release(protocol: &str) -> Result<(), AnyError> {
    if cfg!(debug_assertions) {
        Err(format!("the {protocol} harness must run with --release").into())
    } else {
        Ok(())
    }
}

pub(super) fn environment() -> Result<HostEnvironment, AnyError> {
    Ok(HostEnvironment {
        git_revision: command_output("git", &["rev-parse", "HEAD"])?,
        git_dirty: !command_output("git", &["status", "--porcelain"])?.is_empty(),
        rustc: command_output("rustc", &["-Vv"])?,
        cargo: command_output("cargo", &["-Vv"])?,
        build_profile: env!("REFLEX_BUILD_PROFILE"),
        rustflags: env!("REFLEX_BUILD_RUSTFLAGS"),
        target_features: env!("REFLEX_BUILD_TARGET_FEATURES"),
        target_arch: std::env::consts::ARCH,
        target_os: std::env::consts::OS,
        cpu_description: cpu_description(),
        available_parallelism: std::thread::available_parallelism()?.get(),
    })
}

pub(super) fn require_clean(environment: &HostEnvironment, protocol: &str) -> Result<(), AnyError> {
    if environment.git_dirty {
        Err(format!("{protocol} execution requires a clean committed worktree").into())
    } else {
        Ok(())
    }
}

pub(super) fn require_absent(path: &Path, artifact: &str) -> Result<(), AnyError> {
    if path.exists() {
        Err(format!(
            "refusing to replace an existing {artifact}: {}",
            path.display()
        )
        .into())
    } else {
        Ok(())
    }
}

pub(super) fn parse_flag_values<'a>(
    arguments: &'a [String],
    allowed: &[&str],
    command: &str,
) -> Result<BTreeMap<&'a str, &'a str>, AnyError> {
    let mut values = BTreeMap::new();
    let mut chunks = arguments.chunks_exact(2);
    for pair in &mut chunks {
        let flag = pair[0].as_str();
        if !allowed.contains(&flag) {
            return Err(format!("unknown {command} argument {flag}").into());
        }
        if values.insert(flag, pair[1].as_str()).is_some() {
            return Err(format!("duplicate argument {flag}").into());
        }
    }
    if let [flag] = chunks.remainder() {
        return Err(format!("{flag} requires a value").into());
    }
    Ok(values)
}

pub(super) fn peak_process_resident_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .map_or(0, |status| parse_peak_resident_bytes(&status))
}

pub(super) fn capture_child(
    executable: &Path,
    arguments: &[OsString],
    evidence_prefix: &Path,
    timeout: Option<Duration>,
) -> Result<ChildCapture, AnyError> {
    capture_child_with_limits(executable, arguments, evidence_prefix, timeout, None, &[])
}

pub(super) fn capture_child_with_environment(
    executable: &Path,
    arguments: &[OsString],
    evidence_prefix: &Path,
    timeout: Option<Duration>,
    environment: &[(OsString, OsString)],
) -> Result<ChildCapture, AnyError> {
    capture_child_with_limits(
        executable,
        arguments,
        evidence_prefix,
        timeout,
        None,
        environment,
    )
}

pub(super) fn capture_child_bounded(
    executable: &Path,
    arguments: &[OsString],
    evidence_prefix: &Path,
    timeout: Duration,
    resident_bytes: u64,
    environment: &[(OsString, OsString)],
) -> Result<ChildCapture, AnyError> {
    capture_child_with_limits(
        executable,
        arguments,
        evidence_prefix,
        Some(timeout),
        Some(resident_bytes),
        environment,
    )
}

pub(super) fn capture_child_host_isolated(
    executable: &Path,
    arguments: &[OsString],
    evidence_prefix: &Path,
    timeout: Duration,
    policy: HostIsolationPolicy,
    environment: &[(OsString, OsString)],
) -> Result<(ChildCapture, HostIsolation), AnyError> {
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (
            executable,
            arguments,
            evidence_prefix,
            timeout,
            policy,
            environment,
        );
        return Err(
            "host-isolated experimental children currently require Linux systemd and taskset"
                .into(),
        );
    }
    #[cfg(target_os = "linux")]
    {
        let isolation = detect_host_isolation(policy)?;
        let child_arguments = [
            OsString::from("--user"),
            OsString::from("--scope"),
            OsString::from("--quiet"),
            OsString::from("-p"),
            OsString::from(format!("MemoryMax={}", isolation.memory_limit_bytes)),
            OsString::from("-p"),
            OsString::from("MemorySwapMax=0"),
            OsString::from("-p"),
            OsString::from("OOMPolicy=kill"),
            OsString::from("--"),
            OsString::from("taskset"),
            OsString::from("--cpu-list"),
            OsString::from(&isolation.allowed_cpu_list),
            executable.as_os_str().to_owned(),
        ]
        .into_iter()
        .chain(arguments.iter().cloned())
        .collect::<Vec<_>>();
        let mut isolated_environment = environment.to_vec();
        isolated_environment.extend([
            isolation_environment(ISOLATION_MEMORY_LIMIT, isolation.memory_limit_bytes),
            isolation_environment(ISOLATION_MEMORY_RESERVE, isolation.memory_reserve_bytes),
            isolation_environment(ISOLATION_TOTAL_MEMORY, isolation.total_memory_bytes),
            isolation_environment(ISOLATION_AVAILABLE_MEMORY, isolation.available_memory_bytes),
            (
                OsString::from(ISOLATION_ALLOWED_CPUS),
                OsString::from(&isolation.allowed_cpu_list),
            ),
            (
                OsString::from(ISOLATION_RESERVED_CPUS),
                OsString::from(
                    isolation
                        .reserved_cpus
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            ),
        ]);
        let capture = capture_child_bounded(
            Path::new("systemd-run"),
            &child_arguments,
            evidence_prefix,
            timeout,
            isolation.memory_limit_bytes,
            &isolated_environment,
        )?;
        Ok((capture, isolation))
    }
}

#[cfg(target_os = "linux")]
fn isolation_environment(name: &str, value: u64) -> (OsString, OsString) {
    (OsString::from(name), OsString::from(value.to_string()))
}

pub(super) fn inherited_host_isolation() -> Result<HostIsolation, AnyError> {
    #[cfg(not(target_os = "linux"))]
    return Err("host-isolated experimental children currently require Linux".into());
    #[cfg(target_os = "linux")]
    {
        let number = |name: &str| -> Result<u64, AnyError> {
            Ok(std::env::var(name)
                .map_err(|_| format!("host isolation evidence omits {name}"))?
                .parse()?)
        };
        let allowed_cpu_list = std::env::var(ISOLATION_ALLOWED_CPUS)?;
        let allowed_cpus = parse_cpu_list(&allowed_cpu_list)
            .ok_or("host isolation evidence has an invalid allowed CPU list")?;
        let reserved_cpu_list = std::env::var(ISOLATION_RESERVED_CPUS)?;
        let reserved_cpus = parse_cpu_list(&reserved_cpu_list)
            .ok_or("host isolation evidence has an invalid reserved CPU list")?;
        let status = std::fs::read_to_string("/proc/self/status")?;
        let actual_cpu_list = status
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
            .and_then(parse_cpu_list)
            .ok_or("Linux process metadata omits the allowed CPU list")?;
        if actual_cpu_list != allowed_cpus {
            return Err("taskset did not establish the registered host CPU reserve".into());
        }
        let memory_limit_bytes = number(ISOLATION_MEMORY_LIMIT)?;
        if current_cgroup_memory_limit()? != memory_limit_bytes {
            return Err("systemd did not establish the registered host memory boundary".into());
        }
        Ok(HostIsolation {
            total_memory_bytes: number(ISOLATION_TOTAL_MEMORY)?,
            available_memory_bytes: number(ISOLATION_AVAILABLE_MEMORY)?,
            memory_limit_bytes,
            memory_reserve_bytes: number(ISOLATION_MEMORY_RESERVE)?,
            allowed_cpu_list,
            reserved_cpus,
        })
    }
}

#[cfg(target_os = "linux")]
fn current_cgroup_memory_limit() -> Result<u64, AnyError> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup")?;
    let path = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or("Linux process metadata omits the unified cgroup")?;
    let limit = std::fs::read_to_string(format!("/sys/fs/cgroup{path}/memory.max"))?;
    Ok(limit.trim().parse()?)
}

#[cfg(target_os = "linux")]
fn detect_host_isolation(policy: HostIsolationPolicy) -> Result<HostIsolation, AnyError> {
    let memory = std::fs::read_to_string("/proc/meminfo")?;
    let (total_memory_bytes, available_memory_bytes) =
        parse_memory_info(&memory).ok_or("Linux memory information omits host totals")?;
    let status = std::fs::read_to_string("/proc/self/status")?;
    let cpu_list = status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .and_then(parse_cpu_list)
        .ok_or("Linux process metadata omits the allowed CPU list")?;
    plan_host_isolation(
        total_memory_bytes,
        available_memory_bytes,
        &cpu_list,
        policy.memory_limit_bytes,
        policy.memory_reserve_bytes,
        policy.cpu_reserve,
    )
}

fn plan_host_isolation(
    total_memory_bytes: u64,
    available_memory_bytes: u64,
    cpus: &[usize],
    memory_limit_bytes: u64,
    memory_reserve_bytes: u64,
    cpu_reserve: usize,
) -> Result<HostIsolation, AnyError> {
    if memory_limit_bytes == 0 || memory_reserve_bytes == 0 {
        return Err("host memory limit and reserve must both be nonzero".into());
    }
    let required_memory = memory_limit_bytes
        .checked_add(memory_reserve_bytes)
        .ok_or("host memory policy overflowed")?;
    if total_memory_bytes < required_memory || available_memory_bytes < required_memory {
        return Err(format!(
            "host memory reserve cannot be guaranteed: experiment requires {memory_limit_bytes} bytes while preserving {memory_reserve_bytes} bytes, but the host reports {available_memory_bytes} available of {total_memory_bytes} total"
        )
        .into());
    }
    if cpu_reserve == 0 || cpus.len() <= cpu_reserve {
        return Err("host CPU reserve must leave at least one experiment CPU".into());
    }
    let experiment_cpu_count = cpus.len() - cpu_reserve;
    let (allowed_cpus, reserved_cpus) = cpus.split_at(experiment_cpu_count);
    Ok(HostIsolation {
        total_memory_bytes,
        available_memory_bytes,
        memory_limit_bytes,
        memory_reserve_bytes,
        allowed_cpu_list: allowed_cpus
            .iter()
            .map(usize::to_string)
            .collect::<Vec<_>>()
            .join(","),
        reserved_cpus: reserved_cpus.to_vec(),
    })
}

fn parse_memory_info(contents: &str) -> Option<(u64, u64)> {
    let kibibytes = |key: &str| {
        contents.lines().find_map(|line| {
            let value = line.strip_prefix(key)?.trim();
            let value = value.strip_suffix("kB")?.trim().parse::<u64>().ok()?;
            Some(value.saturating_mul(1024))
        })
    };
    Some((kibibytes("MemTotal:")?, kibibytes("MemAvailable:")?))
}

fn parse_cpu_list(value: &str) -> Option<Vec<usize>> {
    let mut cpus = Vec::new();
    for part in value.trim().split(',') {
        if let Some((start, end)) = part.split_once('-') {
            let start = start.parse::<usize>().ok()?;
            let end = end.parse::<usize>().ok()?;
            if start > end {
                return None;
            }
            cpus.extend(start..=end);
        } else {
            cpus.push(part.parse::<usize>().ok()?);
        }
    }
    if cpus.is_empty() || cpus.windows(2).any(|pair| pair[0] >= pair[1]) {
        None
    } else {
        Some(cpus)
    }
}

fn capture_child_with_limits(
    executable: &Path,
    arguments: &[OsString],
    evidence_prefix: &Path,
    timeout: Option<Duration>,
    resident_bytes: Option<u64>,
    environment: &[(OsString, OsString)],
) -> Result<ChildCapture, AnyError> {
    let stdout_path = evidence_prefix.with_extension("child.stdout");
    let stderr_path = evidence_prefix.with_extension("child.stderr");
    let stdout_file = std::fs::File::create(&stdout_path)?;
    let stderr_file = std::fs::File::create(&stderr_path)?;
    let clock_ticks_per_second = clock_ticks_per_second().ok();
    let child_cpu_before = completed_child_cpu_ticks().ok();
    let mut child = Command::new(executable)
        .args(arguments)
        .envs(environment.iter().cloned())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()?;
    let started = Instant::now();
    let mut peak_process_tree_resident_bytes = 0;
    let (timed_out, resident_limit_exceeded) = loop {
        let current_resident =
            process_tree_resident_bytes(child.id()).saturating_add(peak_process_resident_bytes());
        peak_process_tree_resident_bytes = peak_process_tree_resident_bytes.max(current_resident);
        if child.try_wait()?.is_some() {
            break (false, false);
        }
        if resident_bytes.is_some_and(|limit| current_resident > limit) {
            terminate_process_tree(child.id());
            let _ = child.kill();
            break (false, true);
        }
        if timeout.is_some_and(|timeout| started.elapsed() >= timeout) {
            terminate_process_tree(child.id());
            let _ = child.kill();
            break (true, false);
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let status = child.wait()?;
    let child_cpu_after = completed_child_cpu_ticks().ok();
    let process_tree_cpu_ns = child_cpu_before
        .zip(child_cpu_after)
        .zip(clock_ticks_per_second)
        .map_or(0, |((before, after), frequency)| {
            ticks_to_nanoseconds(after.saturating_sub(before), frequency)
        });
    let stdout = std::fs::read_to_string(&stdout_path)?;
    let stderr = std::fs::read_to_string(&stderr_path)?;
    std::fs::remove_file(stdout_path)?;
    std::fs::remove_file(stderr_path)?;
    Ok(ChildCapture {
        status,
        stdout,
        stderr,
        timed_out,
        resident_limit_exceeded,
        process_tree_cpu_ns,
        peak_process_tree_resident_bytes,
    })
}

#[cfg(target_os = "linux")]
fn terminate_process_tree(root: u32) {
    let mut processes = process_tree_ids(root);
    processes.reverse();
    for process in processes {
        let _ = Command::new("kill")
            .args(["-KILL", "--", &process.to_string()])
            .status();
    }
}

#[cfg(not(target_os = "linux"))]
fn terminate_process_tree(_root: u32) {}

fn process_tree_ids(root: u32) -> Vec<u32> {
    let mut pending = vec![root];
    let mut seen = BTreeSet::new();
    while let Some(process) = pending.pop() {
        if !seen.insert(process) {
            continue;
        }
        let children = format!("/proc/{process}/task/{process}/children");
        if let Ok(children) = std::fs::read_to_string(children) {
            pending.extend(
                children
                    .split_whitespace()
                    .filter_map(|child| child.parse::<u32>().ok()),
            );
        }
    }
    seen.into_iter().collect()
}

fn process_tree_resident_bytes(root: u32) -> u64 {
    process_tree_ids(root)
        .into_iter()
        .map(|process| {
            let root = format!("/proc/{process}");
            std::fs::read_to_string(format!("{root}/status"))
                .map_or(0, |status| parse_resident_bytes(&status))
        })
        .fold(0_u64, u64::saturating_add)
}

fn parse_resident_bytes(status: &str) -> u64 {
    status
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("VmRSS:")?.trim();
            let kibibytes = value.strip_suffix("kB")?.trim().parse::<u64>().ok()?;
            Some(kibibytes.saturating_mul(1024))
        })
        .unwrap_or(0)
}

fn parse_peak_resident_bytes(status: &str) -> u64 {
    status
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("VmHWM:")?.trim();
            let kibibytes = value.strip_suffix("kB")?.trim().parse::<u64>().ok()?;
            Some(kibibytes.saturating_mul(1024))
        })
        .unwrap_or(0)
}

fn completed_child_cpu_ticks() -> Result<u64, AnyError> {
    let stat = std::fs::read_to_string("/proc/self/stat")?;
    parse_completed_child_cpu_ticks(&stat)
        .ok_or_else(|| "cannot parse completed child CPU ticks from /proc/self/stat".into())
}

fn parse_completed_child_cpu_ticks(stat: &str) -> Option<u64> {
    let fields = stat.get(stat.rfind(')')? + 1..)?.split_whitespace();
    let values = fields
        .skip(13)
        .take(2)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    Some(values[0].saturating_add(values[1]))
}

fn clock_ticks_per_second() -> Result<u64, AnyError> {
    static VALUE: OnceLock<Result<u64, String>> = OnceLock::new();
    VALUE
        .get_or_init(|| {
            command_output("getconf", &["CLK_TCK"])
                .and_then(|value| value.parse().map_err(Into::into))
                .map_err(|error| error.to_string())
        })
        .clone()
        .map_err(Into::into)
}

fn ticks_to_nanoseconds(ticks: u64, ticks_per_second: u64) -> u64 {
    u64::try_from(
        u128::from(ticks)
            .saturating_mul(1_000_000_000)
            .checked_div(u128::from(ticks_per_second))
            .unwrap_or(u128::MAX),
    )
    .unwrap_or(u64::MAX)
}

pub(super) fn hash_json(value: &impl Serialize) -> Result<String, AnyError> {
    Ok(hex(&Sha256::digest(serde_json::to_vec(value)?)))
}

pub(super) fn hash_file(path: &Path) -> Result<String, AnyError> {
    Ok(hex(&Sha256::digest(std::fs::read(path)?)))
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    output
}

pub(super) fn duration_ns(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

pub(super) const fn completion_name(completion: Completion) -> &'static str {
    match completion {
        Completion::ResourceEnvelopeExhausted => "resource-envelope-exhausted",
        Completion::SuccessConditionsSatisfied => "success-conditions-satisfied",
        Completion::StoppedByObserver => "stopped-by-observer",
        Completion::NoEligibleWork => "no-eligible-work",
    }
}

fn command_output(program: &str, arguments: &[&str]) -> Result<String, AnyError> {
    let output = Command::new(program).args(arguments).output()?;
    if !output.status.success() {
        return Err(format!("{program} failed with {}", output.status).into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn cpu_description() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|cpuinfo| {
            let fields = cpuinfo
                .lines()
                .filter_map(|line| line.split_once(':'))
                .map(|(name, value)| (name.trim(), value.trim()))
                .collect::<BTreeMap<_, _>>();
            [
                "model name",
                "Hardware",
                "Processor",
                "CPU implementer",
                "CPU architecture",
                "CPU part",
                "CPU variant",
                "CPU revision",
            ]
            .into_iter()
            .filter_map(|name| fields.get(name).map(|value| format!("{name}={value}")))
            .reduce(|mut description, field| {
                description.push_str("; ");
                description.push_str(&field);
                description
            })
        })
        .unwrap_or_else(|| "unavailable".into())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::time::Duration;

    use super::{
        HostIsolationPolicy, capture_child_bounded, capture_child_host_isolated,
        inherited_host_isolation, parse_completed_child_cpu_ticks, parse_cpu_list,
        parse_memory_info, parse_peak_resident_bytes, parse_resident_bytes, plan_host_isolation,
        ticks_to_nanoseconds,
    };

    #[test]
    fn linux_resident_parser_uses_current_rss_in_bytes() {
        let status = "Name:\tworker\nVmPeak:\t900 kB\nVmRSS:\t123 kB\nVmHWM:\t456 kB\n";
        assert_eq!(parse_resident_bytes(status), 123 * 1024);
        assert_eq!(parse_peak_resident_bytes(status), 456 * 1024);
        assert_eq!(parse_resident_bytes("Name:\tworker\n"), 0);
    }

    #[test]
    fn linux_stat_parser_reads_completed_descendant_ticks() {
        let stat = "7 (name with spaces) S 1 2 3 4 5 6 7 8 9 10 11 12 13 17 19 20";
        assert_eq!(parse_completed_child_cpu_ticks(stat), Some(30));
    }

    #[test]
    fn clock_ticks_convert_without_floating_point() {
        assert_eq!(ticks_to_nanoseconds(25, 100), 250_000_000);
        assert_eq!(ticks_to_nanoseconds(u64::MAX, 0), u64::MAX);
    }

    #[test]
    fn host_isolation_reserves_memory_and_a_logical_cpu() {
        let gib = 1024_u64.pow(3);
        let isolation = plan_host_isolation(
            64 * gib,
            62 * gib,
            &(0..8).collect::<Vec<_>>(),
            40 * gib,
            16 * gib,
            1,
        )
        .expect("the host can satisfy the isolation policy");

        assert_eq!(isolation.memory_limit_bytes, 40 * gib);
        assert_eq!(isolation.allowed_cpu_list, "0,1,2,3,4,5,6");
        assert_eq!(isolation.reserved_cpus, vec![7]);
    }

    #[test]
    fn host_isolation_refuses_to_spend_the_reserve() {
        let gib = 1024_u64.pow(3);
        let error = plan_host_isolation(
            64 * gib,
            48 * gib,
            &(0..8).collect::<Vec<_>>(),
            40 * gib,
            16 * gib,
            1,
        )
        .expect_err("available memory equal to work plus reserve is required");

        assert!(error.to_string().contains("host memory reserve"));
    }

    #[test]
    fn linux_host_inputs_are_parsed_without_guessing_cpu_ids() {
        assert_eq!(
            parse_memory_info("MemTotal: 65536 kB\nMemAvailable: 49152 kB\n"),
            Some((64 * 1024 * 1024, 48 * 1024 * 1024))
        );
        assert_eq!(parse_cpu_list("0-2,5,7-8"), Some(vec![0, 1, 2, 5, 7, 8]));
        assert_eq!(parse_cpu_list("2-1"), None);
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and taskset"]
    fn host_isolated_child_observes_kernel_boundaries() {
        const CHILD_MARKER: &str = "REFLEX_HOST_ISOLATION_INTEGRATION_CHILD";
        const MEMORY_LIMIT: u64 = 512 * 1024 * 1024;
        if std::env::var_os(CHILD_MARKER).is_some() {
            let isolation = inherited_host_isolation().expect("child isolation is authentic");
            assert_eq!(isolation.memory_limit_bytes, MEMORY_LIMIT);
            assert_eq!(
                std::thread::available_parallelism()
                    .expect("child parallelism is visible")
                    .get(),
                isolation.allowed_cpu_list.split(',').count()
            );
            return;
        }

        let prefix = std::env::temp_dir().join(format!(
            "reflex-host-isolation-integration-test-{}",
            std::process::id()
        ));
        let executable = std::env::current_exe().expect("test executable exists");
        let (capture, isolation) = capture_child_host_isolated(
            &executable,
            &[
                OsString::from("--exact"),
                OsString::from("harness::tests::host_isolated_child_observes_kernel_boundaries"),
                OsString::from("--nocapture"),
                OsString::from("--ignored"),
            ],
            &prefix,
            Duration::from_secs(10),
            HostIsolationPolicy {
                memory_limit_bytes: MEMORY_LIMIT,
                memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                cpu_reserve: 1,
            },
            &[(OsString::from(CHILD_MARKER), OsString::from("1"))],
        )
        .expect("host-isolated child starts");

        assert_eq!(isolation.memory_limit_bytes, MEMORY_LIMIT);
        assert!(!capture.timed_out, "{}", capture.stderr);
        assert!(!capture.resident_limit_exceeded, "{}", capture.stderr);
        assert!(capture.status.success(), "{}", capture.stderr);
    }

    #[test]
    fn bounded_child_is_killed_at_its_wall_limit() {
        let prefix = std::env::temp_dir().join(format!(
            "reflex-capture-timeout-test-{}",
            std::process::id()
        ));
        let capture = capture_child_bounded(
            std::path::Path::new("sh"),
            &[OsString::from("-c"), OsString::from("sleep 2")],
            &prefix,
            Duration::from_millis(20),
            u64::MAX,
            &[],
        )
        .expect("bounded child supervision succeeds");
        assert!(capture.timed_out);
        assert!(!capture.resident_limit_exceeded);
    }
}
