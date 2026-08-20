use reflex_types::Digest;
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::process::Command;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum XtaskError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serde_json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("toml error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("{0}")]
    Message(String),
}

pub type Result<T> = std::result::Result<T, XtaskError>;

pub fn msg<T: Into<String>>(m: T) -> XtaskError {
    XtaskError::Message(m.into())
}

#[derive(Debug, Clone)]
pub struct ShellResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

pub fn run_cmd(program: &str, args: &[&str]) -> ShellResult {
    run_cmd_with_env(program, args, &[])
}

pub fn run_cmd_with_env(program: &str, args: &[&str], env: &[(&str, &str)]) -> ShellResult {
    let output = Command::new(program)
        .args(args)
        .envs(env.iter().copied())
        .output();
    match output {
        Ok(output) => {
            let ok = output.status.success();
            ShellResult {
                success: ok,
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            }
        }
        Err(e) => ShellResult {
            success: false,
            stdout: String::new(),
            stderr: format!("failed to spawn {program}: {e}"),
        },
    }
}

pub fn run_cmd_in_dir(dir: &Path, program: &str, args: &[&str]) -> ShellResult {
    let output = Command::new(program).current_dir(dir).args(args).output();
    match output {
        Ok(output) => ShellResult {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Err(e) => ShellResult {
            success: false,
            stdout: String::new(),
            stderr: format!("failed to spawn {program} in {}: {e}", dir.display()),
        },
    }
}

pub fn cmd_exists(program: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {program} > /dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn git_head_short() -> String {
    let r = run_cmd("git", &["rev-parse", "--short", "HEAD"]);
    let commit = r.stdout.trim();
    if !r.success || commit.is_empty() {
        return "unknown".to_string();
    }
    let status = run_cmd("git", &["status", "--porcelain"]);
    if status.success && !status.stdout.is_empty() {
        format!("{commit}-dirty")
    } else if status.success {
        commit.to_string()
    } else {
        format!("{commit}-unknown-status")
    }
}

pub fn git_head() -> Result<String> {
    let result = run_cmd("git", &["rev-parse", "HEAD"]);
    let commit = result.stdout.trim();
    if !result.success || commit.is_empty() {
        return Err(msg("cannot resolve exact git commit"));
    }
    Ok(commit.into())
}

pub fn git_commit_time_iso() -> String {
    let r = run_cmd("git", &["log", "-1", "--format=%cI"]);
    r.stdout.trim().to_string()
}

pub fn sha256_hex(data: &[u8]) -> String {
    Digest::hash_sha256(data).to_hex()
}

pub fn write_pretty(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(value)?;
    fs::write(path, text)?;
    Ok(())
}

pub fn read_json(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/// UTC ISO-8601 timestamp without external crates.
pub fn now_utc_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days as i64);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as i64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as i64;
    (if m <= 2 { y + 1 } else { y }, m, d)
}
