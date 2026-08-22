use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use reflex::Completion;
use serde::Serialize;
use sha2::{Digest, Sha256};

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
) -> Result<ChildCapture, AnyError> {
    capture_child_with_limits(
        executable,
        arguments,
        evidence_prefix,
        Some(timeout),
        Some(resident_bytes),
        &[],
    )
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
        capture_child_bounded, parse_completed_child_cpu_ticks, parse_peak_resident_bytes,
        parse_resident_bytes, ticks_to_nanoseconds,
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
        )
        .expect("bounded child supervision succeeds");
        assert!(capture.timed_out);
        assert!(!capture.resident_limit_exceeded);
    }
}
