use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
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

pub(super) fn capture_child(
    executable: &Path,
    arguments: &[OsString],
    evidence_prefix: &Path,
    timeout: Option<Duration>,
) -> Result<ChildCapture, AnyError> {
    let stdout_path = evidence_prefix.with_extension("child.stdout");
    let stderr_path = evidence_prefix.with_extension("child.stderr");
    let stdout_file = std::fs::File::create(&stdout_path)?;
    let stderr_file = std::fs::File::create(&stderr_path)?;
    let mut child = Command::new(executable)
        .args(arguments)
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .spawn()?;
    let timed_out = if let Some(timeout) = timeout {
        let started = Instant::now();
        loop {
            if child.try_wait()?.is_some() {
                break false;
            }
            if started.elapsed() >= timeout {
                child.kill()?;
                break true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    } else {
        false
    };
    let status = child.wait()?;
    let stdout = std::fs::read_to_string(&stdout_path)?;
    let stderr = std::fs::read_to_string(&stderr_path)?;
    std::fs::remove_file(stdout_path)?;
    std::fs::remove_file(stderr_path)?;
    Ok(ChildCapture {
        status,
        stdout,
        stderr,
        timed_out,
    })
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
