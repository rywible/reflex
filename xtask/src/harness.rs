use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsString;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use reflex::Completion;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const ISOLATION_MEMORY_LIMIT: &str = "REFLEX_HOST_ISOLATION_MEMORY_LIMIT";
const ISOLATION_MEMORY_RESERVE: &str = "REFLEX_HOST_ISOLATION_MEMORY_RESERVE";
const ISOLATION_ALLOWED_CPUS: &str = "REFLEX_HOST_ISOLATION_ALLOWED_CPUS";
const ISOLATION_RESERVED_CPUS: &str = "REFLEX_HOST_ISOLATION_RESERVED_CPUS";
const ISOLATION_HOST_CPUS: &str = "REFLEX_HOST_ISOLATION_HOST_CPUS";
const ISOLATION_TASK_LIMIT: &str = "REFLEX_HOST_ISOLATION_TASK_LIMIT";
const LARGE_CAMPAIGN_CAPABILITY: &str = "REFLEX_LARGE_CAMPAIGN_CAPABILITY";
const LARGE_CAMPAIGN_NONCE: &str = "REFLEX_LARGE_CAMPAIGN_NONCE";
const LARGE_CAMPAIGN_PARENT_PID: &str = "REFLEX_LARGE_CAMPAIGN_PARENT_PID";
const LARGE_CAMPAIGN_MONITOR_PID: &str = "REFLEX_LARGE_CAMPAIGN_MONITOR_PID";
const RELAY_REPORT_NONCE: &str = "REFLEX_HOST_RELAY_REPORT_PUBLIC_NONCE";
const RELAY_UNIT: &str = "REFLEX_HOST_RELAY_UNIT";
const RELAY_TARGET_EXECUTABLE: &str = "REFLEX_HOST_RELAY_TARGET_EXECUTABLE";
const RELAY_TARGET_ARGUMENTS: &str = "REFLEX_HOST_RELAY_TARGET_ARGUMENTS";
const RELAY_LAUNCHER_IDENTITY: &str = "REFLEX_HOST_RELAY_LAUNCHER_IDENTITY";
const RELAY_TARGET_IDENTITY: &str = "REFLEX_HOST_RELAY_TARGET_IDENTITY";
const RELAY_TARGET_ARGUMENTS_SHA256: &str = "REFLEX_HOST_RELAY_TARGET_ARGUMENTS_SHA256";
const TARGET_READY_PATH: &str = "REFLEX_HOST_TARGET_READY_PATH";
const TARGET_READY_TEMPORARY: &str = "REFLEX_HOST_TARGET_READY_TEMPORARY";
const TARGET_ACK_PATH: &str = "REFLEX_HOST_TARGET_ACK_PATH";
const TARGET_TRUSTED_USER_NAMESPACE: &str = "REFLEX_HOST_TRUSTED_USER_NAMESPACE";
const TARGET_HOST_UID: &str = "REFLEX_HOST_TARGET_HOST_UID";
const TARGET_RELAY_IDENTITY: &str = "REFLEX_HOST_TARGET_RELAY_IDENTITY";
const TARGET_TASKSET_IDENTITY: &str = "REFLEX_HOST_TARGET_TASKSET_IDENTITY";
const TARGET_SETPRIV_IDENTITY: &str = "REFLEX_HOST_TARGET_SETPRIV_IDENTITY";
const TARGET_EXECUTABLE_IDENTITY: &str = "REFLEX_HOST_TARGET_EXECUTABLE_IDENTITY";
const TARGET_ARGUMENTS: &str = "REFLEX_HOST_TARGET_ARGUMENTS";
const TARGET_ENVIRONMENT: &str = "REFLEX_HOST_TARGET_ENVIRONMENT";
const TARGET_CAMPAIGN_SOURCE_PLAN: &str = "REFLEX_HOST_TARGET_CAMPAIGN_SOURCE_PLAN";
const TARGET_ALLOWED_CPUS: &str = "REFLEX_HOST_TARGET_ALLOWED_CPUS";
const PINNED_RELAY_FD: &str = "REFLEX_HOST_PINNED_RELAY_FD";
const PINNED_UNSHARE_FD: &str = "REFLEX_HOST_PINNED_UNSHARE_FD";
const PINNED_TASKSET_FD: &str = "REFLEX_HOST_PINNED_TASKSET_FD";
const PINNED_SETPRIV_FD: &str = "REFLEX_HOST_PINNED_SETPRIV_FD";
const PINNED_SECCOMP_FD: &str = "REFLEX_HOST_PINNED_SECCOMP_FD";
const PINNED_TARGET_FD: &str = "REFLEX_HOST_PINNED_TARGET_FD";
const GIBIBYTE: u64 = 1024 * 1024 * 1024;
const SUPERVISOR_OBSERVATION_HEADROOM: u64 = 512 * 1024 * 1024;
const MONITOR_HANDSHAKE_BYTES: usize = 32;
const MONITOR_RESPONSE_DOMAIN: &[u8] = b"reflex-monitor-response-v1\0";
const MONITOR_CONFIRMATION_DOMAIN: &[u8] = b"reflex-monitor-confirmation-v1\0";
const RELAY_RESPONSE_DOMAIN: &[u8] = b"reflex-host-relay-response-v1\0";
const RELAY_CONFIRMATION_DOMAIN: &[u8] = b"reflex-host-relay-confirmation-v1\0";
const RELAY_AUTHENTICATION_DOMAIN: &[u8] = b"reflex-host-relay-authentication-v1\0";
const RELAY_REPORT_DOMAIN: &[u8] = b"reflex-host-relay-terminal-report-v1\0";
const RELAY_REPORT_FRAME: &[u8] = b"\0reflex-host-terminal-v2\0";
pub(super) const HOST_ISOLATION_RELAY_COMMAND: &str = "host-isolation-relay";
pub(super) const HOST_ISOLATION_TARGET_COMMAND: &str = "host-isolation-target";
pub(super) const HOST_ISOLATION_EXEC_COMMAND: &str = "host-isolation-exec";
const HOST_ISOLATION_RELAY_TEST: &str =
    "harness::tests::host_isolation_relay_process_for_live_smokes";
const HOST_ISOLATION_TARGET_TEST: &str =
    "harness::tests::host_isolation_target_process_for_live_smokes";
const HOST_ISOLATION_EXEC_TEST: &str =
    "harness::tests::host_isolation_exec_process_for_live_smokes";
#[cfg(test)]
const TEST_RELAY_MONITOR_COMMAND: &str = "REFLEX_HOST_TEST_RELAY_MONITOR_COMMAND";
#[cfg(test)]
const TEST_RELAY_REPORT_MODE: &str = "REFLEX_HOST_TEST_RELAY_REPORT_MODE";
const CAPTURE_STREAM_LIMIT_BYTES: u64 = 8 * 1024 * 1024;
const RELAY_REPORT_LIMIT_BYTES: usize = 4096;
#[cfg(test)]
const OUTPUT_READER_JOIN_TIMEOUT: Duration = Duration::from_secs(2);
const SYSTEM_CONTROL_TIMEOUT: Duration = Duration::from_secs(3);
const SYSTEM_CONTROL_OUTPUT_LIMIT_BYTES: usize = 64 * 1024;
const HOST_ISOLATION_TASK_LIMIT: u64 = 256;
const _: () = assert!(2 * CAPTURE_STREAM_LIMIT_BYTES <= SUPERVISOR_OBSERVATION_HEADROOM);
static SYSTEMD_SCOPE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
pub(super) const LEAN_PUBLIC_NESTED_WALL_LIMIT: Duration = Duration::from_mins(35);
pub(super) const LEAN_PUBLIC_NESTED_RESIDENT_LIMIT: u64 =
    40 * GIBIBYTE - SUPERVISOR_OBSERVATION_HEADROOM;
pub(super) const LEAN_TEMPORAL_NESTED_WALL_LIMIT: Duration = Duration::from_hours(24);
pub(super) const LEAN_TEMPORAL_NESTED_RESIDENT_LIMIT: u64 =
    48 * GIBIBYTE - SUPERVISOR_OBSERVATION_HEADROOM;

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

#[expect(
    clippy::struct_excessive_bools,
    reason = "the capture records independent process-boundary failure evidence explicitly"
)]
pub(super) struct ChildCapture {
    pub(super) status: ExitStatus,
    pub(super) stdout: String,
    pub(super) stderr: String,
    pub(super) timed_out: bool,
    pub(super) resident_limit_exceeded: bool,
    pub(super) boundary: ChildBoundaryEvidence,
    pub(super) process_tree_cpu_ns: u64,
    pub(super) peak_process_tree_resident_bytes: u64,
    pub(super) stdout_truncated: bool,
    pub(super) stderr_truncated: bool,
    pub(super) output_limit_exceeded: bool,
}

#[expect(
    clippy::struct_excessive_bools,
    reason = "each bit records independent kernel or cleanup evidence"
)]
pub(super) struct ChildBoundaryEvidence {
    pub(super) cgroup_oom_killed: bool,
    #[allow(
        dead_code,
        reason = "campaign receipts will expose typed task exhaustion"
    )]
    pub(super) cgroup_pids_exhausted: bool,
    pub(super) cleanup_failed: bool,
    pub(super) evidence_failed: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct RelayReport {
    schema: String,
    nonce: String,
    unit: String,
    relay_pid: u32,
    child_pid: u32,
    termination: RelayTermination,
    target_identity: ExecutableIdentity,
    target_arguments_sha256: String,
    authentication_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ExecutableIdentity {
    pub(super) canonical_path: String,
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) mode: u32,
    pub(super) size: u64,
    pub(super) changed_seconds: i64,
    pub(super) changed_nanoseconds: i64,
    pub(super) content_sha256: String,
}

#[cfg(target_os = "linux")]
pub(super) struct PinnedExecutable {
    pub(super) file: std::fs::File,
    pub(super) identity: ExecutableIdentity,
}

#[derive(Debug, Deserialize, Serialize)]
struct TargetReady {
    schema: String,
    nonce: String,
    process: u32,
    user_namespace: String,
    uid_map: String,
    arguments: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case", tag = "kind")]
enum RelayTermination {
    Exit { code: i32 },
    Signal { signal: i32, core_dumped: bool },
}

struct RelayExpectation {
    nonce: String,
    challenge: [u8; MONITOR_HANDSHAKE_BYTES],
    target_identity: ExecutableIdentity,
    target_arguments_sha256: String,
    target_ready: PathBuf,
    target_ready_temporary: PathBuf,
    target_ack: PathBuf,
    relay_identity: ExecutableIdentity,
}

impl Drop for RelayExpectation {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.target_ready);
        let _ = std::fs::remove_file(&self.target_ready_temporary);
        let _ = std::fs::remove_file(&self.target_ack);
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct HostIsolation {
    pub(super) total_memory_bytes: u64,
    pub(super) available_memory_bytes: u64,
    pub(super) memory_limit_bytes: u64,
    pub(super) memory_reserve_bytes: u64,
    pub(super) allowed_cpu_list: String,
    pub(super) reserved_cpus: Vec<usize>,
    pub(super) cpu_enforcement: String,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct HostIsolationPolicy {
    pub(super) memory_limit_bytes: u64,
    pub(super) memory_reserve_bytes: u64,
    pub(super) cpu_reserve: usize,
    pub(super) experiment_cpu_limit: Option<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum LargeCampaign {
    Baseline,
    CausalConfirmation,
    CausalDevelopment,
    VerificationScaling,
    LeanDevelopment,
    LeanTasteDevelopment,
    LeanPublicOptimizerDevelopment,
    LeanModelFeatureDevelopment,
    LeanTemporalAuditConfirmation,
}

impl LargeCampaign {
    #[cfg(test)]
    pub(super) const COMMANDS: [&'static str; 9] = [
        "baseline",
        "causal-confirm",
        "causal-development-performance",
        "verification-scaling",
        "lean-development",
        "lean-taste-development",
        "lean-public-optimizer-development",
        "lean-model-feature-development",
        "lean-temporal-audit-confirm",
    ];
    pub(super) const CHILD_COMMANDS: [&'static str; 5] = [
        "baseline-child",
        "causal-child",
        "verification-scaling-child",
        "lean-public-optimizer-development-child",
        "lean-temporal-audit-confirm-child",
    ];

    pub(super) fn for_command(command: &str) -> Option<Self> {
        match command {
            "baseline" => Some(Self::Baseline),
            "causal-confirm" => Some(Self::CausalConfirmation),
            "causal-development-performance" => Some(Self::CausalDevelopment),
            "verification-scaling" => Some(Self::VerificationScaling),
            "lean-development" => Some(Self::LeanDevelopment),
            "lean-taste-development" => Some(Self::LeanTasteDevelopment),
            "lean-public-optimizer-development" => Some(Self::LeanPublicOptimizerDevelopment),
            "lean-model-feature-development" => Some(Self::LeanModelFeatureDevelopment),
            "lean-temporal-audit-confirm" => Some(Self::LeanTemporalAuditConfirmation),
            _ => None,
        }
    }

    pub(super) const fn command(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::CausalConfirmation => "causal-confirm",
            Self::CausalDevelopment => "causal-development-performance",
            Self::VerificationScaling => "verification-scaling",
            Self::LeanDevelopment => "lean-development",
            Self::LeanTasteDevelopment => "lean-taste-development",
            Self::LeanPublicOptimizerDevelopment => "lean-public-optimizer-development",
            Self::LeanModelFeatureDevelopment => "lean-model-feature-development",
            Self::LeanTemporalAuditConfirmation => "lean-temporal-audit-confirm",
        }
    }

    pub(super) const fn isolation_policy(self) -> HostIsolationPolicy {
        let (memory_limit_bytes, memory_reserve_bytes) = match self {
            Self::Baseline => (2 * GIBIBYTE, 16 * GIBIBYTE),
            Self::CausalConfirmation | Self::CausalDevelopment | Self::VerificationScaling => {
                (4 * GIBIBYTE, 16 * GIBIBYTE)
            }
            Self::LeanDevelopment | Self::LeanModelFeatureDevelopment => {
                (16 * GIBIBYTE, 16 * GIBIBYTE)
            }
            Self::LeanTasteDevelopment => (24 * GIBIBYTE, 16 * GIBIBYTE),
            Self::LeanPublicOptimizerDevelopment => (40 * GIBIBYTE, 16 * GIBIBYTE),
            Self::LeanTemporalAuditConfirmation => (48 * GIBIBYTE, 8 * GIBIBYTE),
        };
        HostIsolationPolicy {
            memory_limit_bytes,
            memory_reserve_bytes,
            cpu_reserve: 1,
            experiment_cpu_limit: match self {
                Self::VerificationScaling => Some(7),
                _ => None,
            },
        }
    }

    pub(super) fn permits_child(self, command: &str) -> bool {
        matches!(
            (self, command),
            (Self::Baseline, "baseline-child")
                | (
                    Self::CausalConfirmation | Self::CausalDevelopment,
                    "causal-child"
                )
                | (Self::VerificationScaling, "verification-scaling-child")
                | (
                    Self::LeanPublicOptimizerDevelopment,
                    "lean-public-optimizer-development-child"
                )
                | (
                    Self::LeanTemporalAuditConfirmation,
                    "lean-temporal-audit-confirm-child"
                )
        )
    }

    const fn wall_limit(self) -> Duration {
        match self {
            Self::VerificationScaling => Duration::from_mins(10),
            Self::LeanPublicOptimizerDevelopment => Duration::from_mins(40),
            Self::Baseline
            | Self::CausalConfirmation
            | Self::CausalDevelopment
            | Self::LeanDevelopment
            | Self::LeanTasteDevelopment
            | Self::LeanModelFeatureDevelopment => Duration::from_hours(6),
            Self::LeanTemporalAuditConfirmation => Duration::from_mins(1_445),
        }
    }
}

const fn outer_observation_resident_limit(policy: HostIsolationPolicy) -> u64 {
    policy
        .memory_limit_bytes
        .saturating_add(SUPERVISOR_OBSERVATION_HEADROOM)
}

pub(super) fn require_large_campaign_child(command: &str) -> Result<(), AnyError> {
    let capability = std::env::var(LARGE_CAMPAIGN_CAPABILITY).ok();
    let campaign = authenticate_large_campaign_parent(command, capability.as_deref())?;
    verify_large_campaign_parent_ancestry(campaign)?;
    let isolation = inherited_host_isolation()?;
    let policy = campaign.isolation_policy();
    if isolation.memory_limit_bytes != policy.memory_limit_bytes
        || isolation.memory_reserve_bytes != policy.memory_reserve_bytes
        || isolation.reserved_cpus.len() != policy.cpu_reserve
        || policy.experiment_cpu_limit.is_some_and(|limit| {
            parse_cpu_list(&isolation.allowed_cpu_list).is_none_or(|cpus| cpus.len() != limit)
        })
    {
        return Err(format!("{command} inherited the wrong host isolation policy").into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn verify_large_campaign_parent_ancestry(campaign: LargeCampaign) -> Result<(), AnyError> {
    let expected_parent = std::env::var(LARGE_CAMPAIGN_PARENT_PID)
        .map_err(|_| "large-Campaign child omits its parent PID evidence")?
        .parse::<u32>()?;
    let status = std::fs::read_to_string("/proc/self/status")?;
    let actual_parent = status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:")?.trim().parse::<u32>().ok())
        .ok_or("Linux process metadata omits PPid")?;
    if actual_parent != expected_parent {
        return Err("large-Campaign child is not a direct child of its claimed parent".into());
    }
    let current_executable = std::fs::canonicalize(std::env::current_exe()?)?;
    let parent_executable = std::fs::canonicalize(format!("/proc/{actual_parent}/exe"))?;
    let command_line = std::fs::read(format!("/proc/{actual_parent}/cmdline"))?;
    let parent_environment = std::fs::read(format!("/proc/{actual_parent}/environ"))?;
    let nonce = std::env::var(LARGE_CAMPAIGN_NONCE)
        .map_err(|_| "large-Campaign child omits its run nonce")?;
    validate_parent_process_evidence(
        campaign,
        &current_executable,
        &parent_executable,
        &command_line,
        &parent_environment,
        &nonce,
    )
}

#[cfg(not(target_os = "linux"))]
fn verify_large_campaign_parent_ancestry(_campaign: LargeCampaign) -> Result<(), AnyError> {
    Err("large-Campaign ancestry evidence requires Linux".into())
}

fn validate_parent_process_evidence(
    campaign: LargeCampaign,
    current_executable: &Path,
    parent_executable: &Path,
    command_line: &[u8],
    parent_environment: &[u8],
    nonce: &str,
) -> Result<(), AnyError> {
    if current_executable != parent_executable {
        return Err("large-Campaign child parent is not the same xtask executable".into());
    }
    let arguments = command_line
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>();
    if arguments.get(1).copied() != Some(campaign.command().as_bytes()) {
        return Err("large-Campaign child parent command line is not the registered parent".into());
    }
    if nonce.len() != 64 || !nonce.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("large-Campaign run nonce is malformed".into());
    }
    let expected = format!("{LARGE_CAMPAIGN_NONCE}={nonce}");
    if !parent_environment
        .split(|byte| *byte == 0)
        .any(|entry| entry == expected.as_bytes())
    {
        return Err("large-Campaign run nonce is not inherited from the direct parent".into());
    }
    Ok(())
}

fn authenticate_large_campaign_parent(
    command: &str,
    capability: Option<&str>,
) -> Result<LargeCampaign, AnyError> {
    let capability = capability
        .ok_or_else(|| format!("{command} requires an authenticated large-Campaign parent"))?;
    let campaign = LargeCampaign::for_command(capability)
        .ok_or("large-Campaign child received an unknown parent capability")?;
    if !campaign.permits_child(command) {
        return Err(format!("{command} is not authorized by the {capability} parent").into());
    }
    Ok(campaign)
}

pub(super) fn enter_large_campaign_with_source(
    campaign: LargeCampaign,
    arguments: &[String],
    prepare: impl FnOnce() -> Result<
        (
            crate::directional::namespace::CampaignSourcePlan,
            Vec<std::fs::File>,
        ),
        AnyError,
    >,
) -> Result<Option<ChildCapture>, AnyError> {
    if std::env::var_os(LARGE_CAMPAIGN_CAPABILITY).is_some() {
        return enter_large_campaign_with_optional_source(campaign, arguments, None);
    }
    let (plan, descriptors) = prepare()?;
    let result = enter_large_campaign_with_optional_source(campaign, arguments, Some(&plan));
    let cleanup = crate::directional::namespace::cleanup_campaign_source(&plan);
    drop(descriptors);
    match (result, cleanup) {
        (Ok(capture), Ok(())) => Ok(capture),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(cleanup)) => Err(format!("Campaign source cleanup failed ({cleanup})").into()),
        (Err(error), Err(cleanup)) => Err(format!(
            "large-Campaign supervision failed ({error}) and source cleanup failed ({cleanup})"
        )
        .into()),
    }
}

fn enter_large_campaign_with_optional_source(
    campaign: LargeCampaign,
    arguments: &[String],
    source: Option<&crate::directional::namespace::CampaignSourcePlan>,
) -> Result<Option<ChildCapture>, AnyError> {
    if let Some(capability) = std::env::var_os(LARGE_CAMPAIGN_CAPABILITY) {
        if capability != campaign.command() {
            return Err("large-Campaign supervisor capability does not match the command".into());
        }
        let isolation = inherited_host_isolation()?;
        let policy = campaign.isolation_policy();
        if isolation.memory_limit_bytes != policy.memory_limit_bytes
            || isolation.memory_reserve_bytes != policy.memory_reserve_bytes
            || isolation.reserved_cpus.len() != policy.cpu_reserve
            || policy.experiment_cpu_limit.is_some_and(|limit| {
                parse_cpu_list(&isolation.allowed_cpu_list).is_none_or(|cpus| cpus.len() != limit)
            })
        {
            return Err("large-Campaign child inherited the wrong host isolation policy".into());
        }
        require_monitor_handshake(campaign)?;
        return Ok(None);
    }
    let source =
        source.ok_or("large-Campaign launch requires a receipt-bound immutable source plan")?;

    let executable = std::env::current_exe()?;
    let handshake = monitor_handshake()?;
    let nonce = hex(&handshake);
    let child_arguments = std::iter::once(OsString::from(campaign.command()))
        .chain(arguments.iter().map(OsString::from))
        .collect::<Vec<_>>();
    let evidence_prefix = std::env::temp_dir().join(format!(
        "reflex-large-campaign-{}-{}",
        campaign.command(),
        std::process::id()
    ));
    let mut environment = vec![
        (
            OsString::from(LARGE_CAMPAIGN_CAPABILITY),
            OsString::from(campaign.command()),
        ),
        (OsString::from(LARGE_CAMPAIGN_NONCE), OsString::from(nonce)),
        (
            OsString::from(LARGE_CAMPAIGN_MONITOR_PID),
            OsString::from(std::process::id().to_string()),
        ),
    ];
    source.validate()?;
    environment.extend(source.build_environment());
    environment.push((
        OsString::from(TARGET_CAMPAIGN_SOURCE_PLAN),
        OsString::from(serde_json::to_string(source)?),
    ));
    let (capture, _) = capture_child_host_isolated_with_handshake(
        &executable,
        &child_arguments,
        &evidence_prefix,
        campaign.wall_limit(),
        campaign.isolation_policy(),
        &environment,
        &handshake,
    )?;
    Ok(Some(capture))
}

#[cfg(target_os = "linux")]
#[expect(
    clippy::too_many_lines,
    reason = "the relay keeps one linear attestation and execution protocol"
)]
pub(super) fn run_host_isolation_relay() -> Result<(), AnyError> {
    use std::os::unix::process::ExitStatusExt as _;

    let authentication = {
        let stdin = std::io::stdin();
        let stderr = std::io::stderr();
        complete_relay_control_exchange(stdin.lock(), stderr.lock())?
    };
    let nonce = std::env::var(RELAY_REPORT_NONCE)?;
    let unit = std::env::var(RELAY_UNIT)?;
    if !process_belongs_to_systemd_unit(std::process::id(), &unit)
        .map_err(|error| format!("relay unit attestation failed: {error}"))?
    {
        return Err("host-isolation relay is outside its registered systemd unit".into());
    }
    if let Ok(capability) = std::env::var(LARGE_CAMPAIGN_CAPABILITY) {
        let campaign = LargeCampaign::for_command(&capability)
            .ok_or("host-isolation relay received an unknown Campaign capability")?;
        verify_live_campaign_monitor(campaign)?;
    }
    #[cfg(test)]
    if let Ok(command) = std::env::var(TEST_RELAY_MONITOR_COMMAND) {
        verify_live_monitor(command.as_bytes())?;
    }
    let executable = PathBuf::from(
        std::env::var_os(RELAY_TARGET_EXECUTABLE).ok_or("relay target executable absent")?,
    );
    let launcher_identity =
        serde_json::from_str::<ExecutableIdentity>(&std::env::var(RELAY_LAUNCHER_IDENTITY)?)?;
    let taskset_identity =
        serde_json::from_str::<ExecutableIdentity>(&std::env::var(TARGET_TASKSET_IDENTITY)?)?;
    let setpriv_identity =
        serde_json::from_str::<ExecutableIdentity>(&std::env::var(TARGET_SETPRIV_IDENTITY)?)?;
    require_running_executable_identity(&serde_json::from_str::<ExecutableIdentity>(
        &std::env::var(TARGET_RELAY_IDENTITY)?,
    )?)
    .map_err(|error| format!("relay executable attestation failed: {error}"))?;
    require_pinned_fd_identity(PINNED_UNSHARE_FD, &launcher_identity)?;
    require_pinned_fd_identity(PINNED_TASKSET_FD, &taskset_identity)?;
    require_pinned_fd_identity(PINNED_SETPRIV_FD, &setpriv_identity)?;
    let expected_launcher = format!("/proc/self/fd/{}", std::env::var(PINNED_UNSHARE_FD)?);
    if executable != Path::new(&expected_launcher) {
        return Err("relay launcher does not use its pinned descriptor".into());
    }
    let arguments = serde_json::from_str::<Vec<String>>(&std::env::var(RELAY_TARGET_ARGUMENTS)?)?;
    let target_identity =
        serde_json::from_str::<ExecutableIdentity>(&std::env::var(RELAY_TARGET_IDENTITY)?)?;
    require_pinned_fd_identity(PINNED_TARGET_FD, &target_identity)?;
    let target_arguments_sha256 = std::env::var(RELAY_TARGET_ARGUMENTS_SHA256)?;
    #[cfg(test)]
    if let Ok(mode) = std::env::var(TEST_RELAY_REPORT_MODE) {
        inject_test_relay_frame(
            &mode,
            &nonce,
            &unit,
            &target_identity,
            &target_arguments_sha256,
        )?;
    }
    let mut child = Command::new(&executable)
        .args(arguments)
        .spawn()
        .map_err(|error| {
            format!(
                "relay could not launch pinned namespace shim {}: {error}",
                executable.display()
            )
        })?;
    let child_pid = child.id();
    attest_target_ready(child_pid, &unit)
        .map_err(|error| format!("target ready attestation failed: {error}"))?;
    let status = child.wait()?;
    let termination = if let Some(code) = status.code() {
        RelayTermination::Exit { code }
    } else if let Some(signal) = status.signal() {
        RelayTermination::Signal {
            signal,
            core_dumped: status.core_dumped(),
        }
    } else {
        return Err("relay child has no terminal exit evidence".into());
    };
    require_pinned_fd_identity(PINNED_UNSHARE_FD, &launcher_identity)?;
    require_pinned_fd_identity(PINNED_TASKSET_FD, &taskset_identity)?;
    require_pinned_fd_identity(PINNED_SETPRIV_FD, &setpriv_identity)?;
    require_pinned_fd_identity(PINNED_TARGET_FD, &target_identity)?;
    let mut relay_report = RelayReport {
        schema: "reflex-host-relay-report-v2".into(),
        nonce,
        unit,
        relay_pid: std::process::id(),
        child_pid,
        termination,
        target_identity,
        target_arguments_sha256,
        authentication_sha256: String::new(),
    };
    relay_report.authentication_sha256 =
        relay_report_authentication(&relay_report, &authentication.key)?;
    let bytes = serde_json::to_vec(&relay_report)?;
    write_relay_frame(std::io::stderr().lock(), &bytes)?;
    loop {
        std::thread::park();
    }
}

#[cfg(all(test, target_os = "linux"))]
fn inject_test_relay_frame(
    mode: &str,
    nonce: &str,
    unit: &str,
    target_identity: &ExecutableIdentity,
    target_arguments_sha256: &str,
) -> Result<(), AnyError> {
    let forged = serde_json::to_vec(&RelayReport {
        schema: "reflex-host-relay-report-v2".into(),
        nonce: nonce.into(),
        unit: unit.into(),
        relay_pid: std::process::id(),
        child_pid: std::process::id().saturating_add(1),
        termination: RelayTermination::Exit { code: 99 },
        target_identity: target_identity.clone(),
        target_arguments_sha256: target_arguments_sha256.into(),
        authentication_sha256: "00".repeat(32),
    })?;
    match mode {
        "partial" => std::io::stderr().write_all(RELAY_REPORT_FRAME)?,
        "malformed" => write_relay_frame(std::io::stderr().lock(), b"not-json")?,
        "forged-live" | "forged-then-exit" | "forged-then-signal" => {
            write_relay_frame(std::io::stderr().lock(), &forged)?;
        }
        "oversize" => {
            let mut stderr = std::io::stderr().lock();
            stderr.write_all(RELAY_REPORT_FRAME)?;
            stderr.write_all(&u32::try_from(RELAY_REPORT_LIMIT_BYTES + 1)?.to_be_bytes())?;
            stderr.write_all(&vec![0_u8; RELAY_REPORT_LIMIT_BYTES + 1])?;
            stderr.flush()?;
        }
        _ => return Err(format!("unknown test relay report mode {mode}").into()),
    }
    Ok(())
}

fn write_relay_frame(mut writer: impl std::io::Write, payload: &[u8]) -> Result<(), AnyError> {
    if payload.len() > RELAY_REPORT_LIMIT_BYTES {
        return Err("host-isolation relay terminal report exceeds its byte limit".into());
    }
    writer.write_all(RELAY_REPORT_FRAME)?;
    writer.write_all(&u32::try_from(payload.len())?.to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn run_host_isolation_target() -> Result<(), AnyError> {
    use std::os::unix::process::CommandExt as _;

    let trusted_namespace = std::env::var(TARGET_TRUSTED_USER_NAMESPACE)?;
    let host_uid = std::env::var(TARGET_HOST_UID)?.parse::<u32>()?;
    require_target_user_namespace(&trusted_namespace, host_uid)?;
    let ready =
        PathBuf::from(std::env::var_os(TARGET_READY_PATH).ok_or("target ready path absent")?);
    let temporary = PathBuf::from(
        std::env::var_os(TARGET_READY_TEMPORARY).ok_or("target ready temporary absent")?,
    );
    let ack = PathBuf::from(std::env::var_os(TARGET_ACK_PATH).ok_or("target ack path absent")?);
    let nonce = hex(&monitor_handshake()?);
    let relay = serde_json::from_str::<ExecutableIdentity>(&std::env::var(TARGET_RELAY_IDENTITY)?)?;
    let arguments = std::env::args().collect::<Vec<_>>();
    let target_ready = TargetReady {
        schema: "reflex-host-target-ready-v1".into(),
        nonce: nonce.clone(),
        process: std::process::id(),
        user_namespace: current_user_namespace()?,
        uid_map: std::fs::read_to_string("/proc/self/uid_map")?,
        arguments,
    };
    publish_create_new(&temporary, &ready, &serde_json::to_vec(&target_ready)?)?;
    let deadline = Instant::now() + SYSTEM_CONTROL_TIMEOUT;
    loop {
        match std::fs::symlink_metadata(&ack) {
            Ok(_) => {
                let bytes = read_bounded_regular_file(&ack)?;
                if !constant_time_eq(&bytes, nonce.as_bytes()) {
                    return Err("target launch acknowledgment is invalid".into());
                }
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            return Err("target launch acknowledgment deadline elapsed".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    std::fs::remove_file(&ready)?;
    std::fs::remove_file(&ack)?;
    require_target_user_namespace(&trusted_namespace, host_uid)?;
    protect_cgroup_filesystem()?;
    if let Some(serialized) = std::env::var_os(TARGET_CAMPAIGN_SOURCE_PLAN) {
        let serialized = serialized
            .to_str()
            .ok_or("Campaign source plan is not UTF-8")?;
        let plan =
            serde_json::from_str::<crate::directional::namespace::CampaignSourcePlan>(serialized)?;
        let source = crate::directional::namespace::materialize_campaign_source(&plan)?;
        std::env::set_current_dir(source)?;
    } else if std::env::var_os(LARGE_CAMPAIGN_CAPABILITY).is_some() {
        return Err("large-Campaign target omits its immutable source plan".into());
    }
    let taskset =
        serde_json::from_str::<ExecutableIdentity>(&std::env::var(TARGET_TASKSET_IDENTITY)?)?;
    let setpriv =
        serde_json::from_str::<ExecutableIdentity>(&std::env::var(TARGET_SETPRIV_IDENTITY)?)?;
    require_pinned_fd_identity(PINNED_TASKSET_FD, &taskset)?;
    require_pinned_fd_identity(PINNED_SETPRIV_FD, &setpriv)?;
    require_running_executable_identity(&relay)?;
    let allowed_cpus = std::env::var(TARGET_ALLOWED_CPUS)?;
    let mut arguments = vec![
        "--cpu-list".to_owned(),
        allowed_cpus,
        format!("/proc/self/fd/{}", std::env::var(PINNED_SETPRIV_FD)?),
        "--nnp".to_owned(),
        "--securebits=+noroot,+noroot_locked".to_owned(),
        "--bounding-set=-all".to_owned(),
        "--inh-caps=-all".to_owned(),
        "--ambient-caps=-all".to_owned(),
        "--seccomp-filter".to_owned(),
        format!("/proc/self/fd/{}", std::env::var(PINNED_SECCOMP_FD)?),
        "--".to_owned(),
        format!("/proc/self/fd/{}", std::env::var(PINNED_RELAY_FD)?),
    ];
    if cfg!(test) {
        arguments.extend([
            HOST_ISOLATION_EXEC_TEST.to_owned(),
            "--ignored".to_owned(),
            "--exact".to_owned(),
            "--nocapture".to_owned(),
        ]);
    } else {
        arguments.push(HOST_ISOLATION_EXEC_COMMAND.to_owned());
    }
    let error = Command::new(format!(
        "/proc/self/fd/{}",
        std::env::var(PINNED_TASKSET_FD)?
    ))
    .args(arguments)
    .exec();
    Err(format!("trusted taskset exec failed: {error}").into())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn run_host_isolation_target() -> Result<(), AnyError> {
    Err("host-isolation target shim requires Linux".into())
}

#[cfg(target_os = "linux")]
pub(super) fn run_host_isolation_exec() -> Result<(), AnyError> {
    let trusted_namespace = std::env::var(TARGET_TRUSTED_USER_NAMESPACE)?;
    let host_uid = std::env::var(TARGET_HOST_UID)?.parse::<u32>()?;
    require_target_user_namespace(&trusted_namespace, host_uid)?;
    let relay = serde_json::from_str::<ExecutableIdentity>(&std::env::var(TARGET_RELAY_IDENTITY)?)?;
    require_running_executable_identity(&relay)?;
    require_seccomp_affinity_boundary()?;
    require_cgroup_filesystem_read_only()?;
    require_campaign_source_directory()?;
    inherited_host_isolation()?;
    let target =
        serde_json::from_str::<ExecutableIdentity>(&std::env::var(TARGET_EXECUTABLE_IDENTITY)?)?;
    require_pinned_fd_identity(PINNED_TARGET_FD, &target)?;
    let arguments = serde_json::from_str::<Vec<String>>(&std::env::var(TARGET_ARGUMENTS)?)?;
    require_target_arguments_identity(&arguments, &std::env::var(RELAY_TARGET_ARGUMENTS_SHA256)?)?;
    let environment =
        serde_json::from_str::<Vec<(String, String)>>(&std::env::var(TARGET_ENVIRONMENT)?)?;
    exec_pinned_target(&target, &arguments, &environment)
}

#[cfg(target_os = "linux")]
fn require_campaign_source_directory() -> Result<(), AnyError> {
    let Some(serialized) = std::env::var_os(TARGET_CAMPAIGN_SOURCE_PLAN) else {
        if std::env::var_os(LARGE_CAMPAIGN_CAPABILITY).is_some() {
            return Err("large-Campaign exec omits its immutable source plan".into());
        }
        return Ok(());
    };
    let serialized = serialized
        .to_str()
        .ok_or("Campaign source plan is not UTF-8")?;
    let plan =
        serde_json::from_str::<crate::directional::namespace::CampaignSourcePlan>(serialized)?;
    plan.validate()?;
    if std::env::current_dir()? != plan.source_root() {
        return Err("large-Campaign exec is outside its immutable source root".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn exec_pinned_target(
    target: &ExecutableIdentity,
    arguments: &[String],
    environment: &[(String, String)],
) -> Result<(), AnyError> {
    use nix::unistd::fexecve;
    use std::ffi::CString;

    let descriptor = std::env::var(PINNED_TARGET_FD)?.parse::<i32>()?;
    let fd = std::fs::File::open(format!("/proc/self/fd/{descriptor}"))?;
    let mut argv = Vec::with_capacity(arguments.len() + 1);
    argv.push(CString::new(target.canonical_path.as_bytes())?);
    argv.extend(
        arguments
            .iter()
            .map(|argument| CString::new(argument.as_bytes()))
            .collect::<Result<Vec<_>, _>>()?,
    );
    let env = environment
        .iter()
        .map(|(name, value)| CString::new(format!("{name}={value}")))
        .collect::<Result<Vec<_>, _>>()?;
    let error = fexecve(&fd, &argv, &env).expect_err("successful fexecve never returns");
    Err(format!("intended pinned target exec failed: {error}").into())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn run_host_isolation_exec() -> Result<(), AnyError> {
    Err("host-isolation exec shim requires Linux".into())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn run_host_isolation_relay() -> Result<(), AnyError> {
    Err("host-isolation relay requires Linux".into())
}

fn monitor_handshake() -> Result<[u8; MONITOR_HANDSHAKE_BYTES], AnyError> {
    let mut handshake = [0_u8; MONITOR_HANDSHAKE_BYTES];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut handshake)?;
    if handshake.iter().all(|byte| *byte == 0) {
        return Err("the host supervisor produced an invalid monitor handshake".into());
    }
    Ok(handshake)
}

struct RelayAuthentication {
    key: [u8; MONITOR_HANDSHAKE_BYTES],
}

fn complete_relay_control_exchange(
    mut reader: impl Read,
    mut writer: impl std::io::Write,
) -> Result<RelayAuthentication, AnyError> {
    let mut challenge = [0_u8; MONITOR_HANDSHAKE_BYTES];
    reader.read_exact(&mut challenge)?;
    let relay_nonce = monitor_handshake()?;
    let response = relay_control_digest(RELAY_RESPONSE_DOMAIN, &challenge, &relay_nonce);
    writer.write_all(&relay_nonce)?;
    writer.write_all(&response)?;
    writer.flush()?;

    let mut confirmation = [0_u8; MONITOR_HANDSHAKE_BYTES];
    reader.read_exact(&mut confirmation)?;
    let expected = relay_control_digest(RELAY_CONFIRMATION_DOMAIN, &challenge, &relay_nonce);
    if !constant_time_eq(&confirmation, &expected) {
        return Err("host-isolation relay received an invalid supervisor confirmation".into());
    }
    Ok(RelayAuthentication {
        key: relay_control_digest(RELAY_AUTHENTICATION_DOMAIN, &challenge, &relay_nonce),
    })
}

fn relay_control_digest(
    domain: &[u8],
    challenge: &[u8; MONITOR_HANDSHAKE_BYTES],
    relay_nonce: &[u8; MONITOR_HANDSHAKE_BYTES],
) -> [u8; MONITOR_HANDSHAKE_BYTES] {
    Sha256::new()
        .chain_update(domain)
        .chain_update(challenge)
        .chain_update(relay_nonce)
        .finalize()
        .into()
}

fn relay_report_authentication(
    report: &RelayReport,
    authentication: &[u8; MONITOR_HANDSHAKE_BYTES],
) -> Result<String, AnyError> {
    let mut authenticated = report.clone();
    authenticated.authentication_sha256.clear();
    let canonical = serde_json::to_vec(&authenticated)?;
    let message_digest = Sha256::digest(canonical);
    let message = [RELAY_REPORT_DOMAIN, message_digest.as_slice()].concat();
    Ok(hex(&hmac_sha256(authentication, &message)))
}

fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; MONITOR_HANDSHAKE_BYTES] {
    const BLOCK_BYTES: usize = 64;
    let mut padded_key = [0_u8; BLOCK_BYTES];
    if key.len() > BLOCK_BYTES {
        padded_key[..MONITOR_HANDSHAKE_BYTES].copy_from_slice(&Sha256::digest(key));
    } else {
        padded_key[..key.len()].copy_from_slice(key);
    }
    let mut inner_pad = [0x36_u8; BLOCK_BYTES];
    let mut outer_pad = [0x5c_u8; BLOCK_BYTES];
    for index in 0..BLOCK_BYTES {
        inner_pad[index] ^= padded_key[index];
        outer_pad[index] ^= padded_key[index];
    }
    let inner = Sha256::new()
        .chain_update(inner_pad)
        .chain_update(message)
        .finalize();
    Sha256::new()
        .chain_update(outer_pad)
        .chain_update(inner)
        .finalize()
        .into()
}

#[cfg(target_os = "linux")]
fn publish_create_new(temporary: &Path, final_path: &Path, bytes: &[u8]) -> Result<(), AnyError> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::hard_link(temporary, final_path)?;
    std::fs::remove_file(temporary)?;
    std::fs::File::open(final_path.parent().ok_or("evidence path has no parent")?)?.sync_all()?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_target_user_namespace(trusted_namespace: &str, host_uid: u32) -> Result<(), AnyError> {
    let current = current_user_namespace()?;
    let uid_map = std::fs::read_to_string("/proc/self/uid_map")?;
    validate_target_user_namespace(&current, &uid_map, trusted_namespace, host_uid)
}

fn validate_target_user_namespace(
    current: &str,
    uid_map: &str,
    trusted_namespace: &str,
    host_uid: u32,
) -> Result<(), AnyError> {
    let mappings = uid_map
        .lines()
        .map(|line| {
            line.split_whitespace()
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    if current == trusted_namespace || mappings.as_slice() != [vec![0, u64::from(host_uid), 1]] {
        return Err("target lacks its exact descendant user-namespace boundary".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn attest_target_ready(process: u32, unit: &str) -> Result<(), AnyError> {
    let ready =
        PathBuf::from(std::env::var_os(TARGET_READY_PATH).ok_or("target ready path absent")?);
    let ack = PathBuf::from(std::env::var_os(TARGET_ACK_PATH).ok_or("target ack path absent")?);
    let trusted_namespace = std::env::var(TARGET_TRUSTED_USER_NAMESPACE)?;
    let host_uid = std::env::var(TARGET_HOST_UID)?.parse::<u32>()?;
    let deadline = Instant::now() + SYSTEM_CONTROL_TIMEOUT;
    let bytes = loop {
        match std::fs::symlink_metadata(&ready) {
            Ok(_) => break read_bounded_regular_file(&ready)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        if Instant::now() >= deadline {
            return Err("target namespace attestation deadline elapsed".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let evidence = serde_json::from_slice::<TargetReady>(&bytes)?;
    let actual_namespace = std::fs::read_link(format!("/proc/{process}/ns/user"))?
        .to_str()
        .ok_or("target user namespace identity is not UTF-8")?
        .to_owned();
    let actual_uid_map = std::fs::read_to_string(format!("/proc/{process}/uid_map"))?;
    if evidence.schema != "reflex-host-target-ready-v1"
        || evidence.process != process
        || evidence.user_namespace != actual_namespace
        || evidence.uid_map != actual_uid_map
        || evidence.nonce.len() != 2 * MONITOR_HANDSHAKE_BYTES
    {
        return Err("target namespace attestation identity is invalid".into());
    }
    validate_target_user_namespace(
        &actual_namespace,
        &actual_uid_map,
        &trusted_namespace,
        host_uid,
    )?;
    let expected_executable = std::fs::read_link("/proc/self/exe")?;
    let actual_executable = std::fs::read_link(format!("/proc/{process}/exe"))?;
    let expected_identity =
        serde_json::from_str::<ExecutableIdentity>(&std::env::var(TARGET_RELAY_IDENTITY)?)?;
    let command_line = std::fs::read(format!("/proc/{process}/cmdline"))?;
    let observed_arguments = command_line
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>();
    if actual_executable != expected_executable {
        return Err("target namespace shim executable path is invalid".into());
    }
    if executable_metadata(Path::new(&format!("/proc/{process}/exe")))?
        != executable_metadata_from_identity(&expected_identity)
    {
        return Err("target namespace shim executable content is invalid".into());
    }
    require_target_shim_arguments(
        &evidence.arguments,
        &format!("/proc/self/fd/{}", std::env::var(PINNED_RELAY_FD)?),
    )?;
    if !observed_arguments.is_empty()
        && observed_arguments
            != evidence
                .arguments
                .iter()
                .map(String::as_bytes)
                .collect::<Vec<_>>()
    {
        return Err("target namespace shim kernel and ready command lines differ".into());
    }
    if !process_belongs_to_systemd_unit(process, unit)? {
        return Err("target namespace shim cgroup identity is invalid".into());
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&ack)?;
    file.write_all(evidence.nonce.as_bytes())?;
    file.sync_all()?;
    std::fs::File::open(ack.parent().ok_or("target acknowledgment has no parent")?)?.sync_all()?;
    Ok(())
}

fn require_monitor_handshake(campaign: LargeCampaign) -> Result<(), AnyError> {
    verify_live_campaign_monitor(campaign)?;
    let expected = std::env::var(LARGE_CAMPAIGN_NONCE)
        .map_err(|_| "large-Campaign parent omits its monitor nonce")?;
    complete_monitor_round_trip(&expected)?;
    verify_live_campaign_monitor_process(campaign).map(|_| ())
}

fn complete_monitor_round_trip(expected: &str) -> Result<(), AnyError> {
    let mut challenge = [0_u8; MONITOR_HANDSHAKE_BYTES];
    let mut input = std::io::stdin().lock();
    input
        .read_exact(&mut challenge)
        .map_err(|error| format!("large-Campaign parent omitted its monitor challenge: {error}"))?;
    validate_monitor_handshake(expected, &challenge)?;

    let response = monitor_handshake_digest(MONITOR_RESPONSE_DOMAIN, &challenge);
    let mut output = std::io::stderr().lock();
    output.write_all(&response)?;
    output.flush()?;

    let mut confirmation = [0_u8; MONITOR_HANDSHAKE_BYTES];
    input.read_exact(&mut confirmation).map_err(|error| {
        format!("large-Campaign parent omitted its live monitor confirmation: {error}")
    })?;
    let expected_confirmation = monitor_handshake_digest(MONITOR_CONFIRMATION_DOMAIN, &challenge);
    if confirmation != expected_confirmation {
        return Err("large-Campaign parent returned the wrong live monitor confirmation".into());
    }
    Ok(())
}

fn validate_monitor_handshake(expected: &str, handshake: &[u8]) -> Result<(), AnyError> {
    if handshake.len() != MONITOR_HANDSHAKE_BYTES || hex(handshake) != expected {
        return Err("large-Campaign parent supplied the wrong monitor challenge".into());
    }
    Ok(())
}

fn monitor_handshake_digest(domain: &[u8], challenge: &[u8]) -> [u8; MONITOR_HANDSHAKE_BYTES] {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(challenge);
    digest.finalize().into()
}

#[cfg(target_os = "linux")]
fn verify_live_campaign_monitor(campaign: LargeCampaign) -> Result<(), AnyError> {
    verify_live_monitor(campaign.command().as_bytes())
}

#[cfg(target_os = "linux")]
fn verify_live_monitor(expected_command: &[u8]) -> Result<(), AnyError> {
    if target_user_namespace_active()? {
        return verify_live_relay_parent().map(|_| ());
    }
    let monitor = verify_live_monitor_process(expected_command)?;
    let stdin = pipe_identity(Path::new("/proc/self/fd/0"))?;
    let stderr = pipe_identity(Path::new("/proc/self/fd/2"))?;
    let monitor_pipes = monitor_pipe_identities(monitor)?;
    validate_monitor_channel_ownership(stdin, stderr, &monitor_pipes)
}

#[cfg(target_os = "linux")]
fn verify_live_campaign_monitor_process(campaign: LargeCampaign) -> Result<u32, AnyError> {
    verify_live_monitor_process(campaign.command().as_bytes())
}

#[cfg(target_os = "linux")]
fn verify_live_monitor_process(expected_command: &[u8]) -> Result<u32, AnyError> {
    if target_user_namespace_active()? {
        return verify_live_relay_parent();
    }
    let monitor = std::env::var(LARGE_CAMPAIGN_MONITOR_PID)
        .map_err(|_| "large-Campaign parent omits its live monitor PID")?
        .parse::<u32>()?;
    let current = std::process::id();
    let status = std::fs::read_to_string("/proc/self/status")?;
    let direct_parent = status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:")?.trim().parse::<u32>().ok())
        .ok_or("Linux process metadata omits PPid")?;
    let current_executable = std::fs::read_link("/proc/self/exe")?;
    let monitor_executable_path = PathBuf::from(format!("/proc/{monitor}/exe"));
    let monitor_executable = std::fs::read_link(&monitor_executable_path)?;
    let monitor_command_line = std::fs::read(format!("/proc/{monitor}/cmdline"))?;
    let executable_match = validate_monitor_process_evidence(
        expected_command,
        current,
        direct_parent,
        monitor,
        &current_executable,
        &monitor_executable,
        &monitor_command_line,
    );
    if let Err(error) = executable_match {
        let relay = std::env::var(TARGET_RELAY_IDENTITY)
            .ok()
            .and_then(|value| serde_json::from_str::<ExecutableIdentity>(&value).ok());
        if !error.to_string().contains("same xtask executable")
            || relay.as_ref().is_none_or(|identity| {
                require_running_executable_identity(identity).is_err()
                    || monitor_executable != Path::new(&identity.canonical_path)
                    || !matches!(
                        hash_file(&monitor_executable_path),
                        Ok(digest) if digest == identity.content_sha256
                    )
            })
        {
            return Err(error);
        }
    }
    let reserved = parse_cpu_list(&std::env::var(ISOLATION_RESERVED_CPUS)?)
        .ok_or("large-Campaign monitor has an invalid reserved CPU set")?;
    let monitor_status = std::fs::read_to_string(format!("/proc/{monitor}/status"))?;
    let monitor_cpus = monitor_status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .and_then(parse_cpu_list)
        .ok_or("large-Campaign monitor omits its CPU affinity")?;
    if monitor_cpus != reserved {
        return Err("large-Campaign outer monitor is not pinned to its CPU reserve".into());
    }
    Ok(monitor)
}

#[cfg(target_os = "linux")]
fn target_user_namespace_active() -> Result<bool, AnyError> {
    let mappings = std::fs::read_to_string("/proc/self/uid_map")?
        .split_whitespace()
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(matches!(mappings.as_slice(), [0, _, 1]))
}

#[cfg(target_os = "linux")]
fn verify_live_relay_parent() -> Result<u32, AnyError> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let parent = status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:")?.trim().parse::<u32>().ok())
        .ok_or("Linux process metadata omits PPid")?;
    let command_line = std::fs::read(format!("/proc/{parent}/cmdline"))?;
    let arguments = command_line
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>();
    let expected = if cfg!(test) {
        HOST_ISOLATION_RELAY_TEST
    } else {
        HOST_ISOLATION_RELAY_COMMAND
    };
    let current_cgroup = unified_process_cgroup(std::process::id())?;
    let parent_cgroup = unified_process_cgroup(parent)?;
    if arguments.get(1).copied() != Some(expected.as_bytes()) || parent_cgroup != current_cgroup {
        return Err("large-Campaign target lacks its exact attested relay parent".into());
    }
    Ok(parent)
}

#[cfg(target_os = "linux")]
fn unified_process_cgroup(process: u32) -> Result<String, AnyError> {
    std::fs::read_to_string(format!("/proc/{process}/cgroup"))?
        .lines()
        .find_map(|line| line.strip_prefix("0::").map(str::to_owned))
        .ok_or_else(|| "process omits its unified cgroup".into())
}

#[cfg(not(target_os = "linux"))]
fn verify_live_campaign_monitor(_campaign: LargeCampaign) -> Result<(), AnyError> {
    Err("large-Campaign live monitor evidence requires Linux".into())
}

#[cfg(not(target_os = "linux"))]
fn verify_live_monitor(_expected_command: &[u8]) -> Result<(), AnyError> {
    Err("large-Campaign live monitor evidence requires Linux".into())
}

#[cfg(not(target_os = "linux"))]
fn verify_live_campaign_monitor_process(_campaign: LargeCampaign) -> Result<u32, AnyError> {
    Err("large-Campaign live monitor evidence requires Linux".into())
}

#[cfg(not(target_os = "linux"))]
fn verify_live_monitor_process(_expected_command: &[u8]) -> Result<u32, AnyError> {
    Err("large-Campaign live monitor evidence requires Linux".into())
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct PipeIdentity {
    device: u64,
    inode: u64,
}

#[cfg(target_os = "linux")]
fn pipe_identity(path: &Path) -> Result<PipeIdentity, AnyError> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    let metadata = std::fs::metadata(path)?;
    if !metadata.file_type().is_fifo() {
        return Err(format!(
            "monitor round-trip endpoint is not a pipe: {}",
            path.display()
        )
        .into());
    }
    Ok(PipeIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(target_os = "linux")]
fn monitor_pipe_identities(monitor: u32) -> Result<BTreeSet<PipeIdentity>, AnyError> {
    let mut identities = BTreeSet::new();
    for entry in std::fs::read_dir(format!("/proc/{monitor}/fd"))? {
        let entry = entry?;
        if let Ok(identity) = pipe_identity(&entry.path()) {
            identities.insert(identity);
        }
    }
    Ok(identities)
}

fn validate_monitor_channel_ownership(
    stdin: PipeIdentity,
    stderr: PipeIdentity,
    monitor_pipes: &BTreeSet<PipeIdentity>,
) -> Result<(), AnyError> {
    if stdin == stderr || !monitor_pipes.contains(&stdin) || !monitor_pipes.contains(&stderr) {
        return Err(
            "large-Campaign round-trip pipes are not held by its declared live monitor".into(),
        );
    }
    Ok(())
}

fn validate_monitor_process_evidence(
    expected_command: &[u8],
    current: u32,
    direct_parent: u32,
    monitor: u32,
    current_executable: &Path,
    monitor_executable: &Path,
    monitor_command_line: &[u8],
) -> Result<(), AnyError> {
    if monitor == current || monitor == direct_parent {
        return Err(
            "large-Campaign monitor PID does not identify its distinct outer process".into(),
        );
    }
    if current_executable != monitor_executable {
        return Err("large-Campaign monitor is not the same xtask executable".into());
    }
    let arguments = monitor_command_line
        .split(|byte| *byte == 0)
        .filter(|argument| !argument.is_empty())
        .collect::<Vec<_>>();
    if arguments.get(1).copied() != Some(expected_command) {
        return Err("large-Campaign monitor command line is not the registered Campaign".into());
    }
    Ok(())
}

pub(super) fn capture_large_campaign_child(
    executable: &Path,
    arguments: &[OsString],
    evidence_prefix: &Path,
    timeout: Option<Duration>,
    resident_bytes: Option<u64>,
    environment: &[(OsString, OsString)],
) -> Result<ChildCapture, AnyError> {
    if std::env::var_os(LARGE_CAMPAIGN_CAPABILITY).is_none() {
        return Err(
            "large-Campaign child launch requires authenticated host isolation evidence".into(),
        );
    }
    inherited_host_isolation()?;
    let nonce = std::env::var_os(LARGE_CAMPAIGN_NONCE)
        .ok_or("large-Campaign child launch omits the supervised-run nonce")?;
    let mut child_environment = environment.to_vec();
    child_environment.extend([
        (
            OsString::from(LARGE_CAMPAIGN_PARENT_PID),
            OsString::from(std::process::id().to_string()),
        ),
        (OsString::from(LARGE_CAMPAIGN_NONCE), nonce),
    ]);
    capture_child_with_limits(
        executable,
        arguments,
        evidence_prefix,
        timeout,
        resident_bytes,
        &child_environment,
    )
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

#[cfg(test)]
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

#[allow(
    dead_code,
    reason = "the Directional namespace runner consumes this seam"
)]
pub(super) fn capture_child_bounded_clean(
    executable: &Path,
    arguments: &[OsString],
    evidence_prefix: &Path,
    timeout: Duration,
    resident_bytes: u64,
    environment: &[(OsString, OsString)],
) -> Result<ChildCapture, AnyError> {
    capture_child_with_limits_and_boundary(
        executable,
        arguments,
        evidence_prefix,
        Some(timeout),
        Some(resident_bytes),
        environment,
        &TerminationBoundary::ProcessTree,
        None,
        None,
        true,
    )
}

#[cfg(test)]
pub(super) fn capture_child_host_isolated(
    executable: &Path,
    arguments: &[OsString],
    evidence_prefix: &Path,
    timeout: Duration,
    policy: HostIsolationPolicy,
    environment: &[(OsString, OsString)],
) -> Result<(ChildCapture, HostIsolation), AnyError> {
    capture_child_host_isolated_with_optional_handshake(
        executable,
        arguments,
        evidence_prefix,
        timeout,
        policy,
        environment,
        None,
    )
}

fn capture_child_host_isolated_with_handshake(
    executable: &Path,
    arguments: &[OsString],
    evidence_prefix: &Path,
    timeout: Duration,
    policy: HostIsolationPolicy,
    environment: &[(OsString, OsString)],
    handshake: &[u8; MONITOR_HANDSHAKE_BYTES],
) -> Result<(ChildCapture, HostIsolation), AnyError> {
    capture_child_host_isolated_with_optional_handshake(
        executable,
        arguments,
        evidence_prefix,
        timeout,
        policy,
        environment,
        Some(handshake),
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "the retained-scope launch and its evidence binding remain contiguous and auditable"
)]
fn capture_child_host_isolated_with_optional_handshake(
    executable: &Path,
    arguments: &[OsString],
    evidence_prefix: &Path,
    timeout: Duration,
    policy: HostIsolationPolicy,
    environment: &[(OsString, OsString)],
    handshake: Option<&[u8; MONITOR_HANDSHAKE_BYTES]>,
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
            handshake,
        );
        return Err(
            "host-isolated experimental children currently require Linux systemd and taskset"
                .into(),
        );
    }
    #[cfg(target_os = "linux")]
    {
        let isolation = detect_host_isolation(policy)?;
        let unit = format!(
            "reflex-xtask-{}-{}",
            std::process::id(),
            SYSTEMD_SCOPE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let target_ready = evidence_prefix.with_extension("target-ready.json");
        let target_ready_temporary = evidence_prefix.with_extension("target-ready.tmp");
        let target_ack = evidence_prefix.with_extension("target-ack");
        if [&target_ready, &target_ready_temporary, &target_ack]
            .into_iter()
            .any(|path| path.exists())
        {
            return Err("host-isolation relay evidence path already exists".into());
        }
        let target = pin_intended_executable(executable)?;
        let target_identity = target.identity.clone();
        let target_arguments = arguments
            .iter()
            .map(|argument| {
                argument
                    .to_str()
                    .ok_or("host-isolation argument is not UTF-8")
                    .map(str::to_owned)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let target_arguments_sha256 = hash_json(&target_arguments)?;
        let taskset = pin_trusted_tool("taskset")?;
        let taskset_identity = taskset.identity.clone();
        let setpriv = pin_trusted_tool("setpriv")?;
        let setpriv_identity = setpriv.identity.clone();
        let seccomp_filter = sched_setaffinity_seccomp_filter()?;
        let unshare = pin_trusted_tool("unshare")?;
        let unshare_identity = unshare.identity.clone();
        let systemd_run = pin_trusted_tool("systemd-run")?;
        let relay = pin_executable(&std::env::current_exe()?)?;
        let relay_identity = relay.identity.clone();
        let trusted_user_namespace = current_user_namespace()?;
        let host_uid = current_effective_uid()?;
        let relay_challenge = monitor_handshake()?;
        let relay_expectation = RelayExpectation {
            nonce: hex(&monitor_handshake()?),
            challenge: relay_challenge,
            target_identity: target_identity.clone(),
            target_arguments_sha256: target_arguments_sha256.clone(),
            target_ready: target_ready.clone(),
            target_ready_temporary: target_ready_temporary.clone(),
            target_ack: target_ack.clone(),
            relay_identity: relay_identity.clone(),
        };
        let mut child_arguments = vec![
            OsString::from("--user"),
            OsString::from("--scope"),
            OsString::from("--quiet"),
            OsString::from(format!("--unit={unit}")),
            OsString::from("-p"),
            OsString::from(format!("MemoryMax={}", isolation.memory_limit_bytes)),
            OsString::from("-p"),
            OsString::from("MemorySwapMax=0"),
            OsString::from("-p"),
            OsString::from("OOMPolicy=kill"),
            OsString::from("-p"),
            OsString::from(format!("TasksMax={HOST_ISOLATION_TASK_LIMIT}")),
            OsString::from("-p"),
            OsString::from(format!(
                "AllowedCPUs={}",
                isolation_boundary_cpu_list(&isolation)
            )),
            OsString::from("--"),
            pinned_fd_path(&relay.file).into_os_string(),
        ];
        if cfg!(test) {
            child_arguments.extend([
                OsString::from(HOST_ISOLATION_RELAY_TEST),
                OsString::from("--ignored"),
                OsString::from("--exact"),
                OsString::from("--nocapture"),
            ]);
        } else {
            child_arguments.push(OsString::from(HOST_ISOLATION_RELAY_COMMAND));
        }
        let mut relay_target_arguments = vec![
            "--user".to_owned(),
            "--map-root-user".to_owned(),
            "--mount".to_owned(),
            "--cgroup".to_owned(),
            "--propagation".to_owned(),
            "private".to_owned(),
            "--".to_owned(),
            pinned_fd_path(&relay.file)
                .to_str()
                .ok_or("pinned relay descriptor path is not UTF-8")?
                .to_owned(),
        ];
        if cfg!(test) {
            relay_target_arguments.extend([
                HOST_ISOLATION_TARGET_TEST.to_owned(),
                "--ignored".to_owned(),
                "--exact".to_owned(),
                "--nocapture".to_owned(),
            ]);
        } else {
            relay_target_arguments.push(HOST_ISOLATION_TARGET_COMMAND.to_owned());
        }
        let mut target_overrides = environment.to_vec();
        target_overrides.extend([
            isolation_environment(ISOLATION_MEMORY_LIMIT, isolation.memory_limit_bytes),
            isolation_environment(ISOLATION_MEMORY_RESERVE, isolation.memory_reserve_bytes),
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
            (
                OsString::from(ISOLATION_HOST_CPUS),
                OsString::from(isolation_host_cpu_list(&isolation)),
            ),
            isolation_environment(ISOLATION_TASK_LIMIT, HOST_ISOLATION_TASK_LIMIT),
        ]);
        let target_environment = merged_target_environment(&target_overrides)?;
        let mut isolated_environment = trusted_supervisor_environment();
        isolated_environment.extend([
            isolation_environment(ISOLATION_MEMORY_LIMIT, isolation.memory_limit_bytes),
            isolation_environment(ISOLATION_MEMORY_RESERVE, isolation.memory_reserve_bytes),
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
            (
                OsString::from(ISOLATION_HOST_CPUS),
                OsString::from(isolation_host_cpu_list(&isolation)),
            ),
            isolation_environment(ISOLATION_TASK_LIMIT, HOST_ISOLATION_TASK_LIMIT),
            (
                OsString::from(RELAY_REPORT_NONCE),
                OsString::from(&relay_expectation.nonce),
            ),
            (
                OsString::from(RELAY_UNIT),
                OsString::from(format!("{unit}.scope")),
            ),
            (
                OsString::from(RELAY_TARGET_EXECUTABLE),
                pinned_fd_path(&unshare.file).into_os_string(),
            ),
            (
                OsString::from(RELAY_LAUNCHER_IDENTITY),
                OsString::from(serde_json::to_string(&unshare_identity)?),
            ),
            (
                OsString::from(RELAY_TARGET_ARGUMENTS),
                OsString::from(serde_json::to_string(&relay_target_arguments)?),
            ),
            (
                OsString::from(RELAY_TARGET_IDENTITY),
                OsString::from(serde_json::to_string(&target_identity)?),
            ),
            (
                OsString::from(RELAY_TARGET_ARGUMENTS_SHA256),
                OsString::from(&target_arguments_sha256),
            ),
            (
                OsString::from(TARGET_READY_PATH),
                target_ready.into_os_string(),
            ),
            (
                OsString::from(TARGET_READY_TEMPORARY),
                target_ready_temporary.into_os_string(),
            ),
            (OsString::from(TARGET_ACK_PATH), target_ack.into_os_string()),
            (
                OsString::from(TARGET_TRUSTED_USER_NAMESPACE),
                OsString::from(trusted_user_namespace),
            ),
            (
                OsString::from(TARGET_HOST_UID),
                OsString::from(host_uid.to_string()),
            ),
            (
                OsString::from(TARGET_RELAY_IDENTITY),
                OsString::from(serde_json::to_string(&relay_identity)?),
            ),
            (
                OsString::from(TARGET_TASKSET_IDENTITY),
                OsString::from(serde_json::to_string(&taskset_identity)?),
            ),
            (
                OsString::from(TARGET_SETPRIV_IDENTITY),
                OsString::from(serde_json::to_string(&setpriv_identity)?),
            ),
            (
                OsString::from(TARGET_EXECUTABLE_IDENTITY),
                OsString::from(serde_json::to_string(&target_identity)?),
            ),
            (
                OsString::from(TARGET_ARGUMENTS),
                OsString::from(serde_json::to_string(&target_arguments)?),
            ),
            (
                OsString::from(TARGET_ENVIRONMENT),
                OsString::from(serde_json::to_string(&target_environment)?),
            ),
            (
                OsString::from(TARGET_ALLOWED_CPUS),
                OsString::from(&isolation.allowed_cpu_list),
            ),
            pinned_environment(&relay.file, PINNED_RELAY_FD),
            pinned_environment(&unshare.file, PINNED_UNSHARE_FD),
            pinned_environment(&taskset.file, PINNED_TASKSET_FD),
            pinned_environment(&setpriv.file, PINNED_SETPRIV_FD),
            pinned_environment(&seccomp_filter, PINNED_SECCOMP_FD),
            pinned_environment(&target.file, PINNED_TARGET_FD),
        ]);
        for name in [
            LARGE_CAMPAIGN_CAPABILITY,
            LARGE_CAMPAIGN_NONCE,
            LARGE_CAMPAIGN_MONITOR_PID,
        ] {
            if let Some((_, value)) = environment.iter().rev().find(|(key, _)| key == name) {
                isolated_environment.push((OsString::from(name), value.clone()));
            }
        }
        #[cfg(test)]
        for name in [TEST_RELAY_MONITOR_COMMAND, TEST_RELAY_REPORT_MODE] {
            if let Some((_, value)) = environment.iter().rev().find(|(key, _)| key == name) {
                isolated_environment.push((OsString::from(name), value.clone()));
            }
        }
        let capture = capture_child_with_limits_and_boundary(
            &pinned_fd_path(&systemd_run.file),
            &child_arguments,
            evidence_prefix,
            Some(timeout),
            Some(outer_observation_resident_limit(policy)),
            &isolated_environment,
            &TerminationBoundary::SystemdUnit(format!("{unit}.scope")),
            handshake,
            Some(&relay_expectation),
            true,
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
        let policy = std::env::var(LARGE_CAMPAIGN_CAPABILITY)
            .ok()
            .and_then(|command| LargeCampaign::for_command(&command))
            .map_or_else(
                || {
                    Ok::<_, AnyError>(HostIsolationPolicy {
                        memory_limit_bytes: number(ISOLATION_MEMORY_LIMIT)?,
                        memory_reserve_bytes: number(ISOLATION_MEMORY_RESERVE)?,
                        cpu_reserve: parse_cpu_list(&std::env::var(ISOLATION_RESERVED_CPUS)?)
                            .ok_or("host isolation evidence has an invalid reserved CPU list")?
                            .len(),
                        experiment_cpu_limit: None,
                    })
                },
                |campaign| Ok(campaign.isolation_policy()),
            )?;
        let allowed_cpu_list = std::env::var(ISOLATION_ALLOWED_CPUS)?;
        let allowed_cpus = parse_cpu_list(&allowed_cpu_list)
            .ok_or("host isolation evidence has an invalid allowed CPU list")?;
        let reserved_cpu_list = std::env::var(ISOLATION_RESERVED_CPUS)?;
        let reserved_cpus = parse_cpu_list(&reserved_cpu_list)
            .ok_or("host isolation evidence has an invalid reserved CPU list")?;
        let host_cpus = parse_cpu_list(&std::env::var(ISOLATION_HOST_CPUS)?)
            .ok_or("host isolation evidence has an invalid host CPU list")?;
        let status = std::fs::read_to_string("/proc/self/status")?;
        let actual_cpu_list = status
            .lines()
            .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
            .and_then(parse_cpu_list)
            .ok_or("Linux process metadata omits the allowed CPU list")?;
        if actual_cpu_list != allowed_cpus {
            return Err("taskset did not establish the registered host CPU reserve".into());
        }
        let cgroup = current_cgroup_path()?;
        require_seccomp_affinity_boundary()?;
        let exact_cpuset = std::fs::read_to_string(cgroup.join("cpuset.cpus.effective"))
            .ok()
            .map(|value| {
                parse_cpu_list(&value).ok_or_else(|| -> AnyError {
                    "exact target cgroup has an invalid effective cpuset".into()
                })
            })
            .transpose()?;
        let effective_cpus = exact_cpuset.as_deref().unwrap_or(&actual_cpu_list);
        validate_cpu_boundary(
            &allowed_cpus,
            &reserved_cpus,
            effective_cpus,
            &host_cpus,
            policy.cpu_reserve,
        )?;
        let memory_limit_bytes = effective_cgroup_limit(&cgroup, "memory.max")?
            .ok_or("the inherited cgroup has no finite memory ceiling")?;
        if memory_limit_bytes != policy.memory_limit_bytes {
            return Err("systemd did not establish the registered host memory boundary".into());
        }
        if read_controller_number(&cgroup, "memory.swap.max")? != Some(0) {
            return Err("systemd did not disable swap inside the experimental cgroup".into());
        }
        if read_controller_number(&cgroup, "pids.max")? != Some(HOST_ISOLATION_TASK_LIMIT) {
            return Err("systemd did not establish the registered task boundary".into());
        }
        if number(ISOLATION_TASK_LIMIT)? != HOST_ISOLATION_TASK_LIMIT {
            return Err("host isolation evidence has the wrong task limit".into());
        }
        let memory = std::fs::read_to_string("/proc/meminfo")?;
        let (total_memory_bytes, available_memory_bytes) =
            parse_memory_info(&memory).ok_or("Linux memory information omits host totals")?;
        let current_memory = read_controller_number(&cgroup, "memory.current")?
            .ok_or("the inherited cgroup omits current memory consumption")?;
        validate_live_memory(
            policy,
            total_memory_bytes,
            available_memory_bytes,
            current_memory,
        )?;
        Ok(HostIsolation {
            total_memory_bytes,
            available_memory_bytes,
            memory_limit_bytes,
            memory_reserve_bytes: policy.memory_reserve_bytes,
            allowed_cpu_list,
            reserved_cpus,
            cpu_enforcement: if exact_cpuset.is_some() {
                "cgroup-cpuset+seccomp-affinity".into()
            } else {
                "seccomp-affinity".into()
            },
        })
    }
}

#[cfg(target_os = "linux")]
fn current_cgroup_path() -> Result<std::path::PathBuf, AnyError> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup")?;
    let path = cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or("Linux process metadata omits the unified cgroup")?;
    Ok(Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

#[cfg(target_os = "linux")]
fn read_controller_number(cgroup: &Path, name: &str) -> Result<Option<u64>, AnyError> {
    let value = std::fs::read_to_string(cgroup.join(name))?;
    let value = value.trim();
    if value == "max" {
        Ok(None)
    } else {
        Ok(Some(value.parse()?))
    }
}

#[cfg(target_os = "linux")]
fn effective_cgroup_limit(cgroup: &Path, name: &str) -> Result<Option<u64>, AnyError> {
    let root = Path::new("/sys/fs/cgroup");
    let mut current = Some(cgroup);
    let mut effective = None;
    while let Some(path) = current {
        if path.starts_with(root) {
            if path.join(name).exists()
                && let Some(limit) = read_controller_number(path, name)?
            {
                effective = Some(effective.map_or(limit, |prior: u64| prior.min(limit)));
            }
            if path == root {
                break;
            }
        }
        current = path.parent();
    }
    Ok(effective)
}

fn validate_cpu_boundary(
    allowed: &[usize],
    reserved: &[usize],
    effective: &[usize],
    host: &[usize],
    required_reserve: usize,
) -> Result<(), AnyError> {
    if reserved.len() != required_reserve || allowed.iter().any(|cpu| reserved.contains(cpu)) {
        return Err("the experimental and reserved CPU sets are not exactly disjoint".into());
    }
    let mut union = allowed.to_vec();
    union.extend_from_slice(reserved);
    union.sort_unstable();
    if effective != allowed {
        return Err("the target cgroup CPU set exceeds the experimental CPUs".into());
    }
    if union != host {
        return Err(
            "the experimental and reserved CPU sets do not cover the registered host CPUs".into(),
        );
    }
    Ok(())
}

fn validate_live_memory(
    policy: HostIsolationPolicy,
    total: u64,
    available: u64,
    current: u64,
) -> Result<(), AnyError> {
    let required_total = policy
        .memory_limit_bytes
        .checked_add(policy.memory_reserve_bytes)
        .ok_or("host memory policy overflowed")?;
    let unspent_boundary = policy.memory_limit_bytes.saturating_sub(current);
    let required_available = policy
        .memory_reserve_bytes
        .checked_add(unspent_boundary)
        .ok_or("host live-memory requirement overflowed")?;
    if total < required_total || available < required_available {
        return Err("live host memory cannot preserve the fixed experimental reserve".into());
    }
    Ok(())
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
        policy.experiment_cpu_limit,
    )
}

fn plan_host_isolation(
    total_memory_bytes: u64,
    available_memory_bytes: u64,
    cpus: &[usize],
    memory_limit_bytes: u64,
    memory_reserve_bytes: u64,
    cpu_reserve: usize,
    experiment_cpu_limit: Option<usize>,
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
    let experiment_cpu_count =
        experiment_cpu_limit.unwrap_or(cpus.len().saturating_sub(cpu_reserve));
    let selected_cpu_count = experiment_cpu_count
        .checked_add(cpu_reserve)
        .ok_or("host CPU policy overflowed")?;
    if cpu_reserve == 0 || experiment_cpu_count == 0 || cpus.len() < selected_cpu_count {
        return Err("host CPU reserve must leave at least one experiment CPU".into());
    }
    let selected_cpus = &cpus[..selected_cpu_count];
    let (allowed_cpus, reserved_cpus) = selected_cpus.split_at(experiment_cpu_count);
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
        cpu_enforcement: "seccomp-affinity".into(),
    })
}

fn isolation_boundary_cpu_list(isolation: &HostIsolation) -> String {
    parse_cpu_list(&isolation.allowed_cpu_list)
        .unwrap_or_default()
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn isolation_host_cpu_list(isolation: &HostIsolation) -> String {
    let mut cpus = parse_cpu_list(&isolation.allowed_cpu_list).unwrap_or_default();
    cpus.extend_from_slice(&isolation.reserved_cpus);
    cpus.sort_unstable();
    cpus.iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",")
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
    capture_child_with_limits_and_boundary(
        executable,
        arguments,
        evidence_prefix,
        timeout,
        resident_bytes,
        environment,
        &TerminationBoundary::ProcessTree,
        None,
        None,
        false,
    )
}

enum TerminationBoundary {
    ProcessTree,
    SystemdUnit(String),
}

struct BoundaryCleanupGuard<'a> {
    boundary: &'a TerminationBoundary,
    root: u32,
    campaign_deadline: Option<Instant>,
    armed: bool,
}

#[cfg(target_os = "linux")]
struct AffinityGuard {
    original: nix::sched::CpuSet,
}

#[cfg(target_os = "linux")]
impl Drop for AffinityGuard {
    fn drop(&mut self) {
        let _ = nix::sched::sched_setaffinity(nix::unistd::Pid::from_raw(0), &self.original);
    }
}

#[cfg(target_os = "linux")]
fn reserve_outer_monitor(cpus: &[usize]) -> Result<AffinityGuard, AnyError> {
    use nix::sched::{CpuSet, sched_getaffinity, sched_setaffinity};
    use nix::unistd::Pid;

    let original = sched_getaffinity(Pid::from_raw(0))?;
    let mut reserved = CpuSet::new();
    for &cpu in cpus {
        reserved.set(cpu)?;
    }
    sched_setaffinity(Pid::from_raw(0), &reserved)?;
    Ok(AffinityGuard { original })
}

impl<'a> BoundaryCleanupGuard<'a> {
    fn new(
        boundary: &'a TerminationBoundary,
        root: u32,
        campaign_deadline: Option<Instant>,
    ) -> Self {
        Self {
            boundary,
            root,
            campaign_deadline,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for BoundaryCleanupGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let deadline = terminal_control_deadline(self.campaign_deadline);
            let _ = terminate_boundary_before(self.boundary, self.root, deadline, None);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct CgroupMemoryEvents {
    oom: u64,
    oom_kill: u64,
}

#[derive(Default)]
struct CgroupMemoryObserver {
    #[cfg(target_os = "linux")]
    file: Option<std::fs::File>,
    #[cfg(target_os = "linux")]
    cgroup_path: Option<PathBuf>,
}

#[derive(Clone, Copy, Default)]
struct CgroupPidsEvents {
    max: u64,
}

#[derive(Default)]
struct CgroupPidsObserver {
    #[cfg(target_os = "linux")]
    file: Option<std::fs::File>,
    #[cfg(target_os = "linux")]
    cgroup_path: Option<PathBuf>,
}

impl CgroupPidsObserver {
    #[cfg(target_os = "linux")]
    fn read(&mut self, _unit: &str) -> Result<CgroupPidsEvents, AnyError> {
        use std::io::Seek as _;

        if self.file.is_none() {
            let cgroup = self
                .cgroup_path
                .as_ref()
                .ok_or("the experimental cgroup path was not resolved")?;
            self.file = Some(std::fs::File::open(cgroup.join("pids.events"))?);
        }
        let file = self
            .file
            .as_mut()
            .ok_or("the experimental cgroup has no pids-events handle")?;
        file.seek(std::io::SeekFrom::Start(0))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        parse_pids_events(&contents)
            .ok_or_else(|| "the experimental cgroup has malformed pids.events".into())
    }

    #[cfg(not(target_os = "linux"))]
    fn read(&mut self, _unit: &str) -> Result<CgroupPidsEvents, AnyError> {
        Err("systemd cgroup evidence requires Linux".into())
    }

    fn at(cgroup_path: Option<&Path>) -> Self {
        Self {
            #[cfg(target_os = "linux")]
            file: None,
            #[cfg(target_os = "linux")]
            cgroup_path: cgroup_path.map(Path::to_path_buf),
        }
    }
}

impl CgroupMemoryObserver {
    #[cfg(target_os = "linux")]
    fn read(&mut self, _unit: &str) -> Result<CgroupMemoryEvents, AnyError> {
        use std::io::Seek as _;

        if self.file.is_none() {
            let cgroup = self
                .cgroup_path
                .as_ref()
                .ok_or("the experimental cgroup path was not resolved")?;
            self.file = Some(std::fs::File::open(cgroup.join("memory.events"))?);
        }
        let file = self
            .file
            .as_mut()
            .ok_or("the experimental cgroup has no memory-events handle")?;
        file.seek(std::io::SeekFrom::Start(0))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)?;
        parse_memory_events(&contents)
            .ok_or_else(|| "the experimental cgroup has malformed memory.events".into())
    }

    #[cfg(not(target_os = "linux"))]
    fn read(&mut self, _unit: &str) -> Result<CgroupMemoryEvents, AnyError> {
        Err("systemd cgroup evidence requires Linux".into())
    }

    fn at(cgroup_path: Option<&Path>) -> Self {
        Self {
            #[cfg(target_os = "linux")]
            file: None,
            #[cfg(target_os = "linux")]
            cgroup_path: cgroup_path.map(Path::to_path_buf),
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the process supervisor keeps its launch, observation, and cleanup protocol contiguous"
)]
fn capture_child_with_limits_and_boundary(
    executable: &Path,
    arguments: &[OsString],
    _evidence_prefix: &Path,
    timeout: Option<Duration>,
    resident_bytes: Option<u64>,
    environment: &[(OsString, OsString)],
    boundary: &TerminationBoundary,
    monitor_challenge: Option<&[u8; MONITOR_HANDSHAKE_BYTES]>,
    relay: Option<&RelayExpectation>,
    clear_environment: bool,
) -> Result<ChildCapture, AnyError> {
    let started = Instant::now();
    let supervisor_deadline = timeout.and_then(|limit| started.checked_add(limit));
    let clock_ticks_per_second = clock_ticks_per_second().ok();
    let child_cpu_before = completed_child_cpu_ticks().ok();
    let mut command = Command::new(executable);
    if relay.is_some() || clear_environment {
        command.env_clear();
    }
    command
        .args(arguments)
        .envs(environment.iter().cloned())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    if matches!(boundary, TerminationBoundary::ProcessTree) {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    if monitor_challenge.is_some() || relay.is_some() {
        command.stdin(Stdio::piped());
    }
    let mut child = command.spawn()?;
    let mut boundary_guard = BoundaryCleanupGuard::new(boundary, child.id(), supervisor_deadline);
    #[cfg(target_os = "linux")]
    let _outer_affinity = if relay.is_some() {
        let reserved = environment
            .iter()
            .rev()
            .find(|(name, _)| name == ISOLATION_RESERVED_CPUS)
            .and_then(|(_, value)| value.to_str())
            .and_then(parse_cpu_list)
            .ok_or("host supervisor omits its reserved CPU set")?;
        Some(reserve_outer_monitor(&reserved)?)
    } else {
        None
    };
    let output_limit_exceeded = Arc::new(AtomicBool::new(false));
    let setup_control_deadline = terminal_boundary_deadline(boundary, supervisor_deadline);
    let readers = (|| -> Result<(_, _, _, _, _), AnyError> {
        let cgroup_path = if let TerminationBoundary::SystemdUnit(unit) = boundary {
            Some(wait_for_systemd_cgroup_path_before(
                unit,
                setup_control_deadline,
            )?)
        } else {
            None
        };
        let stdout = child
            .stdout
            .take()
            .ok_or("the monitored child has no stdout pipe")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("the monitored child has no stderr pipe")?;
        let stdout_reader = spawn_bounded_reader(stdout, Arc::clone(&output_limit_exceeded))?;
        let (stderr_reader, relay_authentication, terminal_report) = if let Some(relay) = relay {
            let stdin = child
                .stdin
                .take()
                .ok_or("the retained relay has no control input channel")?;
            let (sender, receiver) = std::sync::mpsc::sync_channel(1);
            let (report_sender, report_receiver) = std::sync::mpsc::sync_channel(1);
            let reader = spawn_bounded_reader_with_control_exchanges(
                stderr,
                stdin,
                relay.challenge,
                monitor_challenge.copied(),
                Arc::clone(&output_limit_exceeded),
                sender,
                report_sender,
            )?;
            (reader, Some(receiver), Some(report_receiver))
        } else if let Some(challenge) = monitor_challenge {
            let stdin = child
                .stdin
                .take()
                .ok_or("the monitored child has no round-trip input channel")?;
            (
                spawn_bounded_reader_with_monitor_round_trip(
                    stderr,
                    stdin,
                    *challenge,
                    Arc::clone(&output_limit_exceeded),
                )?,
                None,
                None,
            )
        } else {
            (
                spawn_bounded_reader(stderr, Arc::clone(&output_limit_exceeded))?,
                None,
                None,
            )
        };
        Ok((
            stdout_reader,
            stderr_reader,
            relay_authentication,
            terminal_report,
            cgroup_path,
        ))
    })();
    let (stdout_reader, stderr_reader, relay_authentication, terminal_report, cgroup_path) =
        match readers {
            Ok(readers) => readers,
            Err(error) => {
                let cleanup = cleanup_spawned_child_before(
                    boundary,
                    &mut child,
                    setup_control_deadline,
                    None,
                );
                boundary_guard.disarm();
                return match cleanup {
                    Ok(()) => Err(error),
                    Err(cleanup) => Err(format!(
                        "capture setup failed ({error}) and boundary cleanup failed ({cleanup})"
                    )
                    .into()),
                };
            }
        };
    let mut peak_process_tree_resident_bytes = 0;
    let mut memory_events = CgroupMemoryEvents::default();
    let mut memory_observer = CgroupMemoryObserver::at(cgroup_path.as_deref());
    let mut pids_events = CgroupPidsEvents::default();
    let mut pids_observer = CgroupPidsObserver::at(cgroup_path.as_deref());
    let mut memory_evidence_seen = false;
    let mut memory_evidence_lost = false;
    let mut pids_evidence_seen = false;
    let mut pids_evidence_lost = false;
    let mut terminal_memory_observed = false;
    let mut boundary_cleanup_failed = false;
    let mut boundary_quiescence_proven = false;
    let mut relay_termination = None;
    let mut relay_authentication_key = None;
    let mut authenticated_terminal_report = None;
    let mut terminal_deadline = None;
    let supervision = (|| -> Result<(ExitStatus, bool, bool, bool), AnyError> {
        let (timed_out, resident_limit_exceeded, output_limit_exceeded) = loop {
            if let TerminationBoundary::SystemdUnit(unit) = boundary {
                let observation = memory_observer.read(unit);
                observe_memory_events(
                    &observation,
                    false,
                    &mut memory_events,
                    &mut memory_evidence_seen,
                    &mut memory_evidence_lost,
                );
                observe_pids_events(
                    &pids_observer.read(unit),
                    false,
                    &mut pids_events,
                    &mut pids_evidence_seen,
                    &mut pids_evidence_lost,
                );
            }
            let current_resident = process_tree_resident_bytes(child.id())
                .saturating_add(peak_process_resident_bytes());
            peak_process_tree_resident_bytes =
                peak_process_tree_resident_bytes.max(current_resident);
            let output_limit_exceeded = output_limit_exceeded.load(Ordering::Acquire);
            let elapsed = started.elapsed();
            if relay_authentication_key.is_none()
                && let Some(receiver) = &relay_authentication
            {
                match receiver.try_recv() {
                    Ok(Ok(authentication)) => relay_authentication_key = Some(authentication),
                    Ok(Err(error)) => return Err(error.into()),
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        return Err("retained relay control exchange ended without evidence".into());
                    }
                }
            }
            if authenticated_terminal_report.is_none()
                && let Some(receiver) = &terminal_report
            {
                match receiver.try_recv() {
                    Ok(report) => authenticated_terminal_report = Some(report),
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        return Err("retained relay terminal channel ended without evidence".into());
                    }
                }
            }
            let completed = child.try_wait()?;
            if output_limit_exceeded {
                if let TerminationBoundary::SystemdUnit(unit) = boundary {
                    observe_memory_events(
                        &memory_observer.read(unit),
                        true,
                        &mut memory_events,
                        &mut memory_evidence_seen,
                        &mut memory_evidence_lost,
                    );
                    observe_pids_events(
                        &pids_observer.read(unit),
                        true,
                        &mut pids_events,
                        &mut pids_evidence_seen,
                        &mut pids_evidence_lost,
                    );
                    terminal_memory_observed = true;
                }
                let deadline = *terminal_deadline.get_or_insert_with(|| {
                    terminal_boundary_deadline(boundary, supervisor_deadline)
                });
                boundary_cleanup_failed = terminate_boundary_before(
                    boundary,
                    child.id(),
                    deadline,
                    cgroup_path.as_deref(),
                )
                .is_err();
                let _ = child.kill();
                break (false, false, true);
            }
            if resident_bytes.is_some_and(|limit| current_resident > limit) {
                if let TerminationBoundary::SystemdUnit(unit) = boundary {
                    observe_memory_events(
                        &memory_observer.read(unit),
                        true,
                        &mut memory_events,
                        &mut memory_evidence_seen,
                        &mut memory_evidence_lost,
                    );
                    observe_pids_events(
                        &pids_observer.read(unit),
                        true,
                        &mut pids_events,
                        &mut pids_evidence_seen,
                        &mut pids_evidence_lost,
                    );
                    terminal_memory_observed = true;
                }
                let deadline = *terminal_deadline.get_or_insert_with(|| {
                    terminal_boundary_deadline(boundary, supervisor_deadline)
                });
                boundary_cleanup_failed = terminate_boundary_before(
                    boundary,
                    child.id(),
                    deadline,
                    cgroup_path.as_deref(),
                )
                .is_err();
                let _ = child.kill();
                break (false, true, false);
            }
            if timeout.is_some_and(|limit| elapsed >= limit) {
                if let TerminationBoundary::SystemdUnit(unit) = boundary {
                    observe_memory_events(
                        &memory_observer.read(unit),
                        true,
                        &mut memory_events,
                        &mut memory_evidence_seen,
                        &mut memory_evidence_lost,
                    );
                    observe_pids_events(
                        &pids_observer.read(unit),
                        true,
                        &mut pids_events,
                        &mut pids_evidence_seen,
                        &mut pids_evidence_lost,
                    );
                    terminal_memory_observed = true;
                }
                let deadline = *terminal_deadline.get_or_insert_with(|| {
                    terminal_boundary_deadline(boundary, supervisor_deadline)
                });
                boundary_cleanup_failed = terminate_boundary_before(
                    boundary,
                    child.id(),
                    deadline,
                    cgroup_path.as_deref(),
                )
                .is_err();
                let _ = child.kill();
                break (true, false, false);
            }
            let completed_relay = match (
                relay,
                relay_authentication_key.as_ref(),
                authenticated_terminal_report.as_ref(),
            ) {
                (Some(expectation), Some(authentication), Some(report)) => {
                    let control_deadline = *terminal_deadline.get_or_insert_with(|| {
                        terminal_boundary_deadline(boundary, supervisor_deadline)
                    });
                    let termination = validate_relay_report(
                        report,
                        expectation,
                        boundary,
                        authentication,
                        control_deadline,
                        cgroup_path.as_deref(),
                    )?;
                    authenticated_terminal_report = None;
                    Some((termination, control_deadline))
                }
                _ => None,
            };
            if let Some((termination, control_deadline)) = completed_relay {
                if timeout.is_some_and(|limit| started.elapsed() >= limit) {
                    if let TerminationBoundary::SystemdUnit(unit) = boundary {
                        observe_memory_events(
                            &memory_observer.read(unit),
                            true,
                            &mut memory_events,
                            &mut memory_evidence_seen,
                            &mut memory_evidence_lost,
                        );
                        terminal_memory_observed = true;
                    }
                    boundary_cleanup_failed = terminate_boundary_before(
                        boundary,
                        child.id(),
                        control_deadline,
                        cgroup_path.as_deref(),
                    )
                    .is_err();
                    let _ = child.kill();
                    break (true, false, false);
                }
                if let TerminationBoundary::SystemdUnit(unit) = boundary {
                    observe_memory_events(
                        &memory_observer.read(unit),
                        true,
                        &mut memory_events,
                        &mut memory_evidence_seen,
                        &mut memory_evidence_lost,
                    );
                    observe_pids_events(
                        &pids_observer.read(unit),
                        true,
                        &mut pids_events,
                        &mut pids_evidence_seen,
                        &mut pids_evidence_lost,
                    );
                    terminal_memory_observed = true;
                    let deactivation = deactivate_systemd_unit_before(
                        unit,
                        cgroup_path.as_deref(),
                        control_deadline,
                    );
                    boundary_quiescence_proven = deactivation.is_ok();
                    boundary_cleanup_failed = deactivation.is_err();
                }
                relay_termination = Some(termination);
                break (false, false, false);
            }
            if completed.is_some() {
                let deadline = *terminal_deadline.get_or_insert_with(|| {
                    terminal_boundary_deadline(boundary, supervisor_deadline)
                });
                if matches!(boundary, TerminationBoundary::ProcessTree)
                    && terminate_boundary_before(
                        boundary,
                        child.id(),
                        deadline,
                        cgroup_path.as_deref(),
                    )
                    .is_err()
                {
                    boundary_cleanup_failed = true;
                }
                break (false, false, false);
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let deadline = *terminal_deadline
            .get_or_insert_with(|| terminal_boundary_deadline(boundary, supervisor_deadline));
        let mut status = wait_child_before(&mut child, deadline)?;
        if let Some(termination) = relay_termination {
            status = relay_exit_status(termination)?;
        }
        if let TerminationBoundary::SystemdUnit(unit) = boundary {
            if !terminal_memory_observed {
                observe_memory_events(
                    &memory_observer.read(unit),
                    true,
                    &mut memory_events,
                    &mut memory_evidence_seen,
                    &mut memory_evidence_lost,
                );
                observe_pids_events(
                    &pids_observer.read(unit),
                    true,
                    &mut pids_events,
                    &mut pids_evidence_seen,
                    &mut pids_evidence_lost,
                );
            }
            let deadline = *terminal_deadline
                .get_or_insert_with(|| terminal_boundary_deadline(boundary, supervisor_deadline));
            if !boundary_quiescence_proven
                && !matches!(
                    boundary_is_quiescent_before(unit, cgroup_path.as_deref(), deadline),
                    Ok(true)
                )
            {
                boundary_cleanup_failed = true;
                let _ = terminate_boundary_before(
                    boundary,
                    child.id(),
                    deadline,
                    cgroup_path.as_deref(),
                );
            }
        }
        Ok((
            status,
            timed_out,
            resident_limit_exceeded,
            output_limit_exceeded,
        ))
    })();
    let (status, timed_out, resident_limit_exceeded, mut output_limit_exceeded) = match supervision
    {
        Ok(result) => result,
        Err(error) => {
            let deadline = *terminal_deadline
                .get_or_insert_with(|| terminal_boundary_deadline(boundary, supervisor_deadline));
            let cleanup = cleanup_spawned_child_before(
                boundary,
                &mut child,
                deadline,
                cgroup_path.as_deref(),
            );
            boundary_guard.disarm();
            let stdout = join_bounded_reader_before(
                stdout_reader,
                "stdout after supervision failure",
                deadline,
            );
            let stderr = join_bounded_reader_before(
                stderr_reader,
                "stderr after supervision failure",
                deadline,
            );
            let mut failures = vec![format!("child supervision failed ({error})")];
            if let Err(cleanup) = cleanup {
                failures.push(format!("boundary cleanup failed ({cleanup})"));
            }
            if let Err(stdout) = stdout {
                failures.push(format!("stdout shutdown failed ({stdout})"));
            }
            if let Err(stderr) = stderr {
                failures.push(format!("stderr shutdown failed ({stderr})"));
            }
            return Err(failures.join("; ").into());
        }
    };
    let child_cpu_after = completed_child_cpu_ticks().ok();
    let process_tree_cpu_ns = child_cpu_before
        .zip(child_cpu_after)
        .zip(clock_ticks_per_second)
        .map_or(0, |((before, after), frequency)| {
            ticks_to_nanoseconds(after.saturating_sub(before), frequency)
        });
    let deadline = *terminal_deadline
        .get_or_insert_with(|| terminal_boundary_deadline(boundary, supervisor_deadline));
    let stdout = join_bounded_reader_before(stdout_reader, "stdout", deadline);
    let stderr = join_bounded_reader_before(stderr_reader, "stderr", deadline);
    boundary_guard.disarm();
    let (stdout, stdout_truncated) = stdout?;
    let (stderr, stderr_truncated) = stderr?;
    output_limit_exceeded |= stdout_truncated || stderr_truncated;
    Ok(ChildCapture {
        status,
        stdout,
        stderr,
        timed_out,
        resident_limit_exceeded: resident_limit_exceeded
            || memory_resource_exhausted(memory_events)
            || pids_resource_exhausted(pids_events),
        boundary: ChildBoundaryEvidence {
            cgroup_oom_killed: memory_resource_exhausted(memory_events),
            cgroup_pids_exhausted: pids_resource_exhausted(pids_events),
            cleanup_failed: boundary_cleanup_failed,
            evidence_failed: matches!(boundary, TerminationBoundary::SystemdUnit(_))
                && (!memory_evidence_seen
                    || memory_evidence_lost
                    || !pids_evidence_seen
                    || pids_evidence_lost),
        },
        process_tree_cpu_ns,
        peak_process_tree_resident_bytes,
        stdout_truncated,
        stderr_truncated,
        output_limit_exceeded,
    })
}

fn observe_memory_events(
    observation: &Result<CgroupMemoryEvents, AnyError>,
    terminal: bool,
    events: &mut CgroupMemoryEvents,
    seen: &mut bool,
    lost: &mut bool,
) {
    match observation {
        Ok(observed) => {
            *events = (*events).max(*observed);
            *seen = true;
        }
        Err(_) if terminal || *seen => *lost = true,
        Err(_) => {}
    }
}

fn observe_pids_events(
    observation: &Result<CgroupPidsEvents, AnyError>,
    terminal: bool,
    events: &mut CgroupPidsEvents,
    seen: &mut bool,
    lost: &mut bool,
) {
    match observation {
        Ok(observed) => {
            *events = (*events).max(*observed);
            *seen = true;
        }
        Err(_) if terminal => *lost = true,
        Err(_) => {}
    }
}

fn spawn_bounded_reader<R>(
    reader: R,
    exceeded: Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<std::io::Result<(String, bool)>>>
where
    R: Read + Send + 'static,
{
    std::thread::Builder::new()
        .name("reflex-bounded-child-output".into())
        .spawn(move || read_bounded_stream(reader, &exceeded))
}

fn spawn_bounded_reader_with_monitor_round_trip<R, W>(
    mut reader: R,
    mut writer: W,
    challenge: [u8; MONITOR_HANDSHAKE_BYTES],
    exceeded: Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<std::io::Result<(String, bool)>>>
where
    R: Read + Send + 'static,
    W: std::io::Write + Send + 'static,
{
    std::thread::Builder::new()
        .name("reflex-monitor-round-trip-and-stderr".into())
        .spawn(move || {
            writer.write_all(&challenge)?;
            writer.flush()?;

            let mut response = [0_u8; MONITOR_HANDSHAKE_BYTES];
            reader.read_exact(&mut response)?;
            let expected_response = monitor_handshake_digest(MONITOR_RESPONSE_DOMAIN, &challenge);
            if response != expected_response {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "the child returned the wrong live monitor response",
                ));
            }

            let confirmation = monitor_handshake_digest(MONITOR_CONFIRMATION_DOMAIN, &challenge);
            writer.write_all(&confirmation)?;
            writer.flush()?;
            drop(writer);
            read_bounded_stream(reader, &exceeded)
        })
}

fn spawn_bounded_reader_with_control_exchanges<R, W>(
    mut reader: R,
    mut writer: W,
    relay_challenge: [u8; MONITOR_HANDSHAKE_BYTES],
    monitor_challenge: Option<[u8; MONITOR_HANDSHAKE_BYTES]>,
    exceeded: Arc<AtomicBool>,
    authentication: std::sync::mpsc::SyncSender<Result<[u8; MONITOR_HANDSHAKE_BYTES], String>>,
    terminal_report: std::sync::mpsc::SyncSender<RelayReport>,
) -> std::io::Result<std::thread::JoinHandle<std::io::Result<(String, bool)>>>
where
    R: Read + Send + 'static,
    W: std::io::Write + Send + 'static,
{
    std::thread::Builder::new()
        .name("reflex-relay-control-and-stderr".into())
        .spawn(move || {
            let exchange = (|| -> std::io::Result<[u8; MONITOR_HANDSHAKE_BYTES]> {
                writer.write_all(&relay_challenge)?;
                writer.flush()?;
                let mut relay_nonce = [0_u8; MONITOR_HANDSHAKE_BYTES];
                let mut response = [0_u8; MONITOR_HANDSHAKE_BYTES];
                reader.read_exact(&mut relay_nonce)?;
                reader.read_exact(&mut response)?;
                let expected =
                    relay_control_digest(RELAY_RESPONSE_DOMAIN, &relay_challenge, &relay_nonce);
                if !constant_time_eq(&response, &expected) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "the retained relay returned the wrong control response",
                    ));
                }
                let confirmation =
                    relay_control_digest(RELAY_CONFIRMATION_DOMAIN, &relay_challenge, &relay_nonce);
                writer.write_all(&confirmation)?;
                writer.flush()?;
                Ok(relay_control_digest(
                    RELAY_AUTHENTICATION_DOMAIN,
                    &relay_challenge,
                    &relay_nonce,
                ))
            })();
            let authentication_result = exchange.as_ref().map_or_else(
                |error| Err(error.to_string()),
                |authentication| Ok(*authentication),
            );
            let _ = authentication.send(authentication_result);
            let authentication = exchange?;

            if let Some(challenge) = monitor_challenge {
                writer.write_all(&challenge)?;
                writer.flush()?;
                let mut response = [0_u8; MONITOR_HANDSHAKE_BYTES];
                reader.read_exact(&mut response)?;
                let expected = monitor_handshake_digest(MONITOR_RESPONSE_DOMAIN, &challenge);
                if !constant_time_eq(&response, &expected) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "the child returned the wrong live monitor response",
                    ));
                }
                let confirmation =
                    monitor_handshake_digest(MONITOR_CONFIRMATION_DOMAIN, &challenge);
                writer.write_all(&confirmation)?;
                writer.flush()?;
            }
            drop(writer);
            read_bounded_relay_stream(reader, &exceeded, &authentication, &terminal_report)
        })
}

fn read_bounded_relay_stream(
    mut reader: impl Read,
    exceeded: &AtomicBool,
    authentication: &[u8; MONITOR_HANDSHAKE_BYTES],
    terminal_report: &std::sync::mpsc::SyncSender<RelayReport>,
) -> std::io::Result<(String, bool)> {
    let capacity = usize::try_from(CAPTURE_STREAM_LIMIT_BYTES).unwrap_or(usize::MAX);
    let mut tail = VecDeque::with_capacity(capacity);
    let mut pending = Vec::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        if total > CAPTURE_STREAM_LIMIT_BYTES {
            exceeded.store(true, Ordering::Release);
        }
        let overflow = tail.len().saturating_add(count).saturating_sub(capacity);
        tail.drain(..overflow);
        tail.extend(&buffer[..count]);
        pending.extend_from_slice(&buffer[..count]);
        extract_authenticated_relay_frames(&mut pending, authentication, terminal_report);
        let maximum_pending = RELAY_REPORT_FRAME.len() + 4 + RELAY_REPORT_LIMIT_BYTES;
        if pending.len() > maximum_pending {
            let retain = RELAY_REPORT_FRAME.len().saturating_sub(1);
            pending.drain(..pending.len().saturating_sub(retain));
        }
    }
    let truncated = total > CAPTURE_STREAM_LIMIT_BYTES;
    let bytes = tail.into_iter().collect::<Vec<_>>();
    let mut output = String::new();
    if truncated {
        output.push_str("[earlier child output truncated by supervisor]\n");
    }
    output.push_str(&String::from_utf8_lossy(&bytes));
    Ok((output, truncated))
}

fn extract_authenticated_relay_frames(
    pending: &mut Vec<u8>,
    authentication: &[u8; MONITOR_HANDSHAKE_BYTES],
    terminal_report: &std::sync::mpsc::SyncSender<RelayReport>,
) {
    loop {
        let Some(position) = pending
            .windows(RELAY_REPORT_FRAME.len())
            .position(|window| window == RELAY_REPORT_FRAME)
        else {
            return;
        };
        let header = position + RELAY_REPORT_FRAME.len();
        if pending.len() < header + 4 {
            return;
        }
        let length = u32::from_be_bytes(pending[header..header + 4].try_into().unwrap_or([0; 4]));
        let length = usize::try_from(length).unwrap_or(usize::MAX);
        if length > RELAY_REPORT_LIMIT_BYTES {
            pending.drain(..=position);
            continue;
        }
        let end = header + 4 + length;
        if pending.len() < end {
            return;
        }
        let payload = pending[header + 4..end].to_vec();
        pending.drain(..end);
        if let Ok(report) = serde_json::from_slice::<RelayReport>(&payload)
            && relay_report_authentication(&report, authentication).is_ok_and(|expected| {
                constant_time_eq(expected.as_bytes(), report.authentication_sha256.as_bytes())
            })
        {
            let _ = terminal_report.try_send(report);
        }
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |difference, (left, right)| {
            difference | (left ^ right)
        })
        == 0
}

fn read_bounded_stream(
    mut reader: impl Read,
    exceeded: &AtomicBool,
) -> std::io::Result<(String, bool)> {
    read_bounded_stream_with_limit(&mut reader, exceeded, CAPTURE_STREAM_LIMIT_BYTES)
}

fn read_bounded_stream_with_limit(
    mut reader: impl Read,
    exceeded: &AtomicBool,
    limit: u64,
) -> std::io::Result<(String, bool)> {
    let capacity = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut tail = VecDeque::with_capacity(capacity);
    let mut total = 0_u64;
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        if total > limit {
            exceeded.store(true, Ordering::Release);
        }
        if count >= capacity {
            tail.clear();
            tail.extend(&buffer[count - capacity..count]);
        } else {
            let overflow = tail.len().saturating_add(count).saturating_sub(capacity);
            tail.drain(..overflow);
            tail.extend(&buffer[..count]);
        }
    }
    let truncated = total > limit;
    let bytes = tail.into_iter().collect::<Vec<_>>();
    let mut output = String::new();
    if truncated {
        output.push_str("[earlier child output truncated by supervisor]\n");
    }
    output.push_str(&String::from_utf8_lossy(&bytes));
    Ok((output, truncated))
}

fn spawn_control_reader<R>(
    stream: R,
    name: &str,
    exceeded: Arc<AtomicBool>,
) -> std::io::Result<std::thread::JoinHandle<std::io::Result<(String, bool)>>>
where
    R: Read + Send + 'static,
{
    std::thread::Builder::new()
        .name(format!("reflex-control-{name}"))
        .spawn(move || {
            read_bounded_stream_with_limit(
                stream,
                &exceeded,
                u64::try_from(SYSTEM_CONTROL_OUTPUT_LIMIT_BYTES).unwrap_or(u64::MAX),
            )
        })
}

#[cfg(test)]
fn join_bounded_reader(
    reader: std::thread::JoinHandle<std::io::Result<(String, bool)>>,
    name: &str,
) -> Result<(String, bool), AnyError> {
    join_bounded_reader_before(reader, name, Instant::now() + OUTPUT_READER_JOIN_TIMEOUT)
}

fn join_bounded_reader_before(
    reader: std::thread::JoinHandle<std::io::Result<(String, bool)>>,
    name: &str,
    deadline: Instant,
) -> Result<(String, bool), AnyError> {
    while !reader.is_finished() {
        if Instant::now() >= deadline {
            return Err(format!("the bounded {name} reader did not terminate").into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    reader
        .join()
        .map_err(|_| format!("the bounded {name} reader panicked"))?
        .map_err(Into::into)
}

fn cleanup_spawned_child_before(
    boundary: &TerminationBoundary,
    child: &mut std::process::Child,
    deadline: Instant,
    cgroup_path: Option<&Path>,
) -> Result<(), AnyError> {
    if let TerminationBoundary::SystemdUnit(unit) = boundary
        && boundary_is_quiescent_before(unit, cgroup_path, deadline).unwrap_or(false)
    {
        return wait_child_before(child, deadline).map(|_| ());
    }
    let cleanup = terminate_boundary_before(boundary, child.id(), deadline, cgroup_path);
    let _ = child.kill();
    let reaped = wait_child_before(child, deadline);
    match (cleanup, reaped) {
        (Ok(()), Ok(_)) => Ok(()),
        (Err(cleanup), Ok(_)) => Err(cleanup),
        (Ok(()), Err(reap)) => Err(reap),
        (Err(cleanup), Err(reap)) => {
            Err(format!("boundary cleanup failed ({cleanup}) and root reap failed ({reap})").into())
        }
    }
}

fn wait_child_before(
    child: &mut std::process::Child,
    deadline: Instant,
) -> Result<ExitStatus, AnyError> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err("monitored root process did not terminate before its reap deadline".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

impl CgroupMemoryEvents {
    fn max(self, other: Self) -> Self {
        Self {
            oom: self.oom.max(other.oom),
            oom_kill: self.oom_kill.max(other.oom_kill),
        }
    }
}

impl CgroupPidsEvents {
    const fn max(self, other: Self) -> Self {
        Self {
            max: if self.max > other.max {
                self.max
            } else {
                other.max
            },
        }
    }
}

const fn memory_resource_exhausted(events: CgroupMemoryEvents) -> bool {
    events.oom > 0 || events.oom_kill > 0
}

const fn pids_resource_exhausted(events: CgroupPidsEvents) -> bool {
    events.max > 0
}

#[cfg(target_os = "linux")]
fn validate_relay_report(
    report: &RelayReport,
    expectation: &RelayExpectation,
    boundary: &TerminationBoundary,
    authentication: &[u8; MONITOR_HANDSHAKE_BYTES],
    deadline: Instant,
    cgroup_path: Option<&Path>,
) -> Result<RelayTermination, AnyError> {
    let TerminationBoundary::SystemdUnit(unit) = boundary else {
        return Err("relay report is not bound to a systemd unit".into());
    };
    let bytes = serde_json::to_vec(report)?;
    let report = decode_relay_report(
        &bytes,
        &expectation.nonce,
        unit,
        authentication,
        &expectation.target_identity,
        &expectation.target_arguments_sha256,
    )?;
    let cgroup_path = cgroup_path.ok_or("host-isolation cgroup path was not retained")?;
    let relay_executable = PathBuf::from(format!("/proc/{}/exe", report.relay_pid));
    let relay_file = std::fs::File::open(&relay_executable)?;
    if require_sealed_executable(&relay_file).is_err()
        || executable_metadata(&relay_executable)?
            != executable_metadata_from_identity(&expectation.relay_identity)
        || !process_belongs_to_cgroup(report.relay_pid, cgroup_path)?
    {
        return Err("host-isolation relay report identity is invalid".into());
    }
    require_stable_relay_only_state(cgroup_path, report.relay_pid, deadline)?;
    Ok(report.termination)
}

fn decode_relay_report(
    bytes: &[u8],
    expected_nonce: &str,
    expected_unit: &str,
    authentication: &[u8; MONITOR_HANDSHAKE_BYTES],
    expected_target: &ExecutableIdentity,
    expected_arguments_sha256: &str,
) -> Result<RelayReport, AnyError> {
    let report = serde_json::from_slice::<RelayReport>(bytes)
        .map_err(|error| format!("host-isolation relay report is malformed: {error}"))?;
    if report.schema != "reflex-host-relay-report-v2"
        || report.nonce != expected_nonce
        || report.unit != expected_unit
        || report.relay_pid == 0
        || report.child_pid == 0
        || report.target_identity != *expected_target
        || report.target_arguments_sha256 != expected_arguments_sha256
    {
        return Err("host-isolation relay report identity is invalid".into());
    }
    let expected_authentication = relay_report_authentication(&report, authentication)?;
    if !constant_time_eq(
        report.authentication_sha256.as_bytes(),
        expected_authentication.as_bytes(),
    ) {
        return Err("host-isolation relay report authentication is invalid".into());
    }
    relay_exit_status(report.termination)?;
    Ok(report)
}

#[cfg(target_os = "linux")]
fn read_bounded_regular_file(path: &Path) -> Result<Vec<u8>, AnyError> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    const O_NONBLOCK: i32 = 0x800;
    #[cfg(target_arch = "aarch64")]
    const O_NOFOLLOW: i32 = 0x8000;
    #[cfg(not(target_arch = "aarch64"))]
    const O_NOFOLLOW: i32 = 0x2_0000;
    let path_metadata = std::fs::symlink_metadata(path)?;
    if !path_metadata.file_type().is_file() {
        return Err("host-isolation relay report is not a bounded regular file".into());
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NONBLOCK | O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.file_type().is_file()
        || metadata.dev() != path_metadata.dev()
        || metadata.ino() != path_metadata.ino()
        || metadata.len() > u64::try_from(RELAY_REPORT_LIMIT_BYTES).unwrap_or(u64::MAX)
    {
        return Err("host-isolation relay report is not a bounded regular file".into());
    }
    let mut bytes = Vec::with_capacity(
        RELAY_REPORT_LIMIT_BYTES.min(usize::try_from(metadata.len()).unwrap_or(usize::MAX)),
    );
    std::io::Read::by_ref(&mut file)
        .take(u64::try_from(RELAY_REPORT_LIMIT_BYTES + 1).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)?;
    if bytes.len() > RELAY_REPORT_LIMIT_BYTES {
        return Err("host-isolation relay report exceeds its byte limit".into());
    }
    Ok(bytes)
}

#[cfg(not(target_os = "linux"))]
fn read_bounded_regular_file(_path: &Path) -> Result<Vec<u8>, AnyError> {
    Err("host-isolation relay evidence requires Linux".into())
}

#[cfg(target_os = "linux")]
fn require_stable_relay_only_state(
    cgroup: &Path,
    relay_pid: u32,
    deadline: Instant,
) -> Result<(), AnyError> {
    for sample in 0..2 {
        if Instant::now() >= deadline {
            return Err("host-isolation relay terminal-state deadline elapsed".into());
        }
        let processes = std::fs::read_to_string(cgroup.join("cgroup.procs"))?;
        let children =
            std::fs::read_to_string(format!("/proc/{relay_pid}/task/{relay_pid}/children"))?;
        validate_relay_only_state(&processes, &children, relay_pid)?;
        if sample == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}

fn validate_relay_only_state(
    cgroup_processes: &str,
    relay_children: &str,
    relay_pid: u32,
) -> Result<(), AnyError> {
    let processes = cgroup_processes
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()?;
    let children = relay_children
        .split_whitespace()
        .map(str::parse::<u32>)
        .collect::<Result<Vec<_>, _>>()?;
    if processes.as_slice() != [relay_pid] || !children.is_empty() {
        return Err("host-isolation relay is not the only terminal scope process".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn process_belongs_to_systemd_unit(process: u32, unit: &str) -> Result<bool, AnyError> {
    process_belongs_to_systemd_unit_before(process, unit, Instant::now() + SYSTEM_CONTROL_TIMEOUT)
}

#[cfg(target_os = "linux")]
fn process_belongs_to_systemd_unit_before(
    process: u32,
    unit: &str,
    deadline: Instant,
) -> Result<bool, AnyError> {
    let process_cgroups = std::fs::read_to_string(format!("/proc/{process}/cgroup"))?;
    let cgroup = process_cgroups
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or("relay process omits its unified cgroup")?;
    Ok(
        Path::new("/sys/fs/cgroup").join(cgroup.trim_start_matches('/'))
            == systemd_cgroup_path_before(unit, deadline)?,
    )
}

#[cfg(target_os = "linux")]
fn process_belongs_to_cgroup(process: u32, expected: &Path) -> Result<bool, AnyError> {
    let process_cgroups = std::fs::read_to_string(format!("/proc/{process}/cgroup"))?;
    let cgroup = process_cgroups
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or("process omits its unified cgroup")?;
    Ok(Path::new("/sys/fs/cgroup").join(cgroup.trim_start_matches('/')) == expected)
}

#[cfg(not(target_os = "linux"))]
fn validate_relay_report(
    _report: &RelayReport,
    _expectation: &RelayExpectation,
    _boundary: &TerminationBoundary,
    _authentication: &[u8; MONITOR_HANDSHAKE_BYTES],
    _deadline: Instant,
    _cgroup_path: Option<&Path>,
) -> Result<RelayTermination, AnyError> {
    Err("host-isolation relay evidence requires Linux".into())
}

#[cfg(target_os = "linux")]
fn relay_exit_status(termination: RelayTermination) -> Result<ExitStatus, AnyError> {
    use std::os::unix::process::ExitStatusExt as _;

    let raw = match termination {
        RelayTermination::Exit { code } if (0..=255).contains(&code) => code << 8,
        RelayTermination::Signal {
            signal,
            core_dumped,
        } if (1..=127).contains(&signal) => signal | if core_dumped { 0x80 } else { 0 },
        _ => return Err("host-isolation relay termination value is invalid".into()),
    };
    Ok(ExitStatus::from_raw(raw))
}

#[cfg(not(target_os = "linux"))]
fn relay_exit_status(_termination: RelayTermination) -> Result<ExitStatus, AnyError> {
    Err("host-isolation relay exit evidence requires Linux".into())
}

#[cfg(target_os = "linux")]
fn systemd_cgroup_path_before(
    unit: &str,
    deadline: Instant,
) -> Result<std::path::PathBuf, AnyError> {
    let output = run_control_command_before(
        "systemctl",
        &["--user", "show", "--property=ControlGroup", "--value", unit],
        deadline,
    )?;
    if !output.status.success() {
        return Err(format!("cannot resolve systemd scope {unit}").into());
    }
    let relative = String::from_utf8(output.stdout)?;
    let relative = relative.trim().trim_start_matches('/');
    if relative.is_empty() {
        return Err(format!("systemd scope {unit} has no registered cgroup").into());
    }
    Ok(Path::new("/sys/fs/cgroup").join(relative))
}

#[cfg(target_os = "linux")]
fn wait_for_systemd_cgroup_path_before(unit: &str, deadline: Instant) -> Result<PathBuf, AnyError> {
    let mut last_error = None;
    while Instant::now() < deadline {
        match systemd_cgroup_path_before(unit, deadline) {
            Ok(path) => return Ok(path),
            Err(error) => last_error = Some(error),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(format!(
        "systemd scope {unit} cgroup resolution deadline elapsed ({})",
        last_error.map_or_else(|| "no observation".into(), |error| error.to_string())
    )
    .into())
}

#[cfg(target_os = "linux")]
fn deactivate_systemd_unit_before(
    unit: &str,
    cgroup_path: Option<&Path>,
    deadline: Instant,
) -> Result<(), AnyError> {
    let output = run_control_command_before("systemctl", &["--user", "stop", unit], deadline)?;
    if !output.status.success() {
        return Err(format!("cannot stop retained systemd unit {unit}").into());
    }
    for _ in 0..100 {
        if boundary_is_quiescent_before(unit, cgroup_path, deadline)? {
            return Ok(());
        }
        let output = run_control_command_before(
            "systemctl",
            &["--user", "show", "--property=ActiveState", "--value", unit],
            deadline,
        )?;
        if output.status.success()
            && matches!(
                String::from_utf8(output.stdout)?.trim(),
                "inactive" | "failed"
            )
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(format!("retained systemd unit {unit} did not deactivate").into())
}

fn terminal_control_deadline(campaign_deadline: Option<Instant>) -> Instant {
    let control_deadline = Instant::now() + SYSTEM_CONTROL_TIMEOUT;
    campaign_deadline.map_or(control_deadline, |deadline| deadline.min(control_deadline))
}

fn terminal_boundary_deadline(
    boundary: &TerminationBoundary,
    campaign_deadline: Option<Instant>,
) -> Instant {
    terminal_control_deadline(match boundary {
        TerminationBoundary::SystemdUnit(_) => campaign_deadline,
        TerminationBoundary::ProcessTree => None,
    })
}

fn parse_memory_events(contents: &str) -> Option<CgroupMemoryEvents> {
    let value = |key: &str| {
        contents.lines().find_map(|line| {
            let (name, value) = line.split_once(' ')?;
            (name == key).then(|| value.parse::<u64>().ok()).flatten()
        })
    };
    Some(CgroupMemoryEvents {
        oom: value("oom")?,
        oom_kill: value("oom_kill")?,
    })
}

fn parse_pids_events(contents: &str) -> Option<CgroupPidsEvents> {
    contents.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == "max")
            .then(|| value.parse().ok().map(|max| CgroupPidsEvents { max }))
            .flatten()
    })
}

fn parse_cgroup_populated(contents: &str) -> Option<bool> {
    contents.lines().find_map(|line| {
        let (name, value) = line.split_once(' ')?;
        (name == "populated").then_some(match value {
            "0" => Some(false),
            "1" => Some(true),
            _ => None,
        })?
    })
}

#[cfg(target_os = "linux")]
fn boundary_is_quiescent_before(
    unit: &str,
    cgroup_path: Option<&Path>,
    deadline: Instant,
) -> Result<bool, AnyError> {
    let resolved;
    let path = if let Some(path) = cgroup_path {
        path
    } else {
        resolved = systemd_cgroup_path_before(unit, deadline)?;
        &resolved
    };
    if let Ok(events) = std::fs::read_to_string(path.join("cgroup.events"))
        && parse_cgroup_populated(&events) == Some(false)
    {
        return Ok(true);
    }
    let output = run_control_command_before(
        "systemctl",
        &["--user", "show", "--property=ActiveState", "--value", unit],
        deadline,
    )?;
    if !output.status.success() {
        return Ok(false);
    }
    Ok(matches!(
        String::from_utf8(output.stdout)?.trim(),
        "inactive" | "failed"
    ))
}

#[cfg(target_os = "linux")]
fn wait_for_boundary_quiescence_at_before(
    unit: &str,
    cgroup_path: Option<&Path>,
    deadline: Instant,
) -> Result<(), AnyError> {
    for _ in 0..100 {
        if boundary_is_quiescent_before(unit, cgroup_path, deadline)? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("systemd scope {unit} cleanup deadline elapsed").into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(format!("systemd scope {unit} remained populated after termination").into())
}

#[cfg(target_os = "linux")]
fn terminate_boundary_before(
    boundary: &TerminationBoundary,
    root: u32,
    deadline: Instant,
    cgroup_path: Option<&Path>,
) -> Result<(), AnyError> {
    match boundary {
        TerminationBoundary::ProcessTree => {
            if terminate_process_tree_before(root, deadline) {
                Ok(())
            } else {
                Err("bounded child process group did not become quiescent".into())
            }
        }
        TerminationBoundary::SystemdUnit(unit) => {
            let killed = run_control_command_before(
                "systemctl",
                &["--user", "kill", "--kill-whom=all", "--signal=KILL", unit],
                deadline,
            )
            .is_ok_and(|output| output.status.success());
            if killed && wait_for_boundary_quiescence_at_before(unit, cgroup_path, deadline).is_ok()
            {
                return Ok(());
            }
            cgroup_kill_fallback_before(
                unit,
                deadline,
                || {
                    cgroup_path.map_or_else(
                        || systemd_cgroup_path_before(unit, deadline),
                        |path| Ok(path.to_path_buf()),
                    )
                },
                |path| std::fs::write(path.join("cgroup.kill"), "1").map_err(Into::into),
                |path| wait_for_boundary_quiescence_at_before(unit, Some(path), deadline),
            )
        }
    }
}

#[cfg(target_os = "linux")]
fn cgroup_kill_fallback_before(
    unit: &str,
    deadline: Instant,
    resolve: impl FnOnce() -> Result<PathBuf, AnyError>,
    kill: impl FnOnce(&Path) -> Result<(), AnyError>,
    quiescence: impl FnOnce(&Path) -> Result<(), AnyError>,
) -> Result<(), AnyError> {
    if Instant::now() >= deadline {
        return Err(format!("systemd scope {unit} cleanup deadline elapsed").into());
    }
    let cgroup = resolve()?;
    if Instant::now() >= deadline {
        return Err(format!("systemd scope {unit} cleanup deadline elapsed").into());
    }
    kill(&cgroup)?;
    if Instant::now() >= deadline {
        return Err(format!("systemd scope {unit} cleanup deadline elapsed").into());
    }
    quiescence(&cgroup)
}

#[cfg(not(target_os = "linux"))]
fn terminate_boundary_before(
    boundary: &TerminationBoundary,
    root: u32,
    deadline: Instant,
    _cgroup_path: Option<&Path>,
) -> Result<(), AnyError> {
    match boundary {
        TerminationBoundary::ProcessTree => {
            if terminate_process_tree_before(root, deadline) {
                Ok(())
            } else {
                Err("bounded child process group did not become quiescent".into())
            }
        }
        TerminationBoundary::SystemdUnit(_) => {
            Err("systemd unit termination requires Linux".into())
        }
    }
}

#[cfg(target_os = "linux")]
fn terminate_process_tree_before(root: u32, deadline: Instant) -> bool {
    use nix::sys::signal::{Signal, kill, killpg};
    use nix::unistd::Pid;

    if let Ok(root) = i32::try_from(root) {
        let _ = killpg(Pid::from_raw(root), Signal::SIGKILL);
    }
    let mut processes = process_tree_ids(root);
    processes.reverse();
    for process in processes {
        if let Ok(process) = i32::try_from(process) {
            let _ = kill(Pid::from_raw(process), Signal::SIGKILL);
        }
    }
    while Instant::now() < deadline {
        match process_group_exists(root) {
            Ok(false) => return true,
            Ok(true) => {}
            Err(_) => return false,
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    matches!(process_group_exists(root), Ok(false))
}

#[cfg(not(target_os = "linux"))]
fn terminate_process_tree_before(_root: u32, _deadline: Instant) -> bool {
    true
}

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

#[cfg(target_os = "linux")]
fn process_group_exists(group: u32) -> Result<bool, AnyError> {
    for entry in std::fs::read_dir("/proc")? {
        let entry = entry?;
        let process = entry.file_name();
        let process = process.to_string_lossy();
        if !process.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let stat = match std::fs::read_to_string(entry.path().join("stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let fields = stat
            .get(
                stat.rfind(')')
                    .ok_or("Linux process stat omits its command terminator")?
                    + 1..,
            )
            .ok_or("Linux process stat command terminator is out of bounds")?;
        let process_group = fields
            .split_whitespace()
            .nth(2)
            .ok_or("Linux process stat omits its process group")?
            .parse::<u32>()?;
        if process_group == group {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(not(target_os = "linux"))]
fn process_group_exists(_group: u32) -> Result<bool, AnyError> {
    Ok(false)
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

#[cfg(target_os = "linux")]
#[derive(Eq, PartialEq)]
struct ExecutableMetadata {
    device: u64,
    inode: u64,
    mode: u32,
    size: u64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

#[cfg(target_os = "linux")]
fn executable_metadata(path: &Path) -> Result<ExecutableMetadata, AnyError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let metadata = std::fs::metadata(path)?;
    if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!("{} is not a regular executable", path.display()).into());
    }
    Ok(ExecutableMetadata {
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        size: metadata.size(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    })
}

#[cfg(target_os = "linux")]
pub(super) fn pinned_fd_path(file: &std::fs::File) -> PathBuf {
    use std::os::fd::AsRawFd as _;

    PathBuf::from(format!("/proc/self/fd/{}", file.as_raw_fd()))
}

#[cfg(target_os = "linux")]
fn pinned_fd_number(file: &std::fs::File) -> String {
    use std::os::fd::AsRawFd as _;

    file.as_raw_fd().to_string()
}

#[cfg(target_os = "linux")]
pub(super) fn pin_executable(path: &Path) -> Result<PinnedExecutable, AnyError> {
    use nix::fcntl::{FcntlArg, SealFlag, fcntl};
    use nix::sys::memfd::{MFdFlags, memfd_create};
    use nix::sys::stat::{Mode, fchmod};
    use std::io::{Seek as _, SeekFrom};
    use std::os::unix::fs::OpenOptionsExt as _;

    #[cfg(target_arch = "aarch64")]
    const O_NOFOLLOW: i32 = 0x8000;
    #[cfg(not(target_arch = "aarch64"))]
    const O_NOFOLLOW: i32 = 0x2_0000;

    let canonical = std::fs::canonicalize(path)?;
    let mut source = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(&canonical)?;
    let mut magic = [0_u8; 4];
    source.read_exact(&mut magic)?;
    source.seek(SeekFrom::Start(0))?;
    if magic != *b"\x7fELF" {
        return Err(format!(
            "pinned executable {} is not an ELF object; scripts are rejected",
            canonical.display()
        )
        .into());
    }
    let descriptor = memfd_create("reflex-pinned-elf", MFdFlags::MFD_ALLOW_SEALING)?;
    let mut file = std::fs::File::from(descriptor);
    std::io::copy(&mut source, &mut file)?;
    file.seek(SeekFrom::Start(0))?;
    fchmod(&file, Mode::S_IRUSR | Mode::S_IXUSR)?;
    let required_seals = SealFlag::F_SEAL_WRITE
        | SealFlag::F_SEAL_GROW
        | SealFlag::F_SEAL_SHRINK
        | SealFlag::F_SEAL_SEAL;
    fcntl(&file, FcntlArg::F_ADD_SEALS(required_seals))?;
    require_sealed_executable(&file)?;
    let identity = executable_identity_from_file(&mut file, &canonical)?;
    Ok(PinnedExecutable { file, identity })
}

#[cfg(target_os = "linux")]
fn require_sealed_executable(file: &std::fs::File) -> Result<(), AnyError> {
    use nix::fcntl::{FcntlArg, SealFlag, fcntl};

    let required = SealFlag::F_SEAL_WRITE
        | SealFlag::F_SEAL_GROW
        | SealFlag::F_SEAL_SHRINK
        | SealFlag::F_SEAL_SEAL;
    let observed = SealFlag::from_bits_retain(fcntl(file, FcntlArg::F_GET_SEALS)?);
    if !observed.contains(required) {
        return Err("pinned executable memfd is not immutably sealed".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn require_sealed_memfd(file: &std::fs::File) -> Result<(), AnyError> {
    require_sealed_executable(file)
}

#[cfg(target_os = "linux")]
fn protect_cgroup_filesystem() -> Result<(), AnyError> {
    use nix::mount::{MsFlags, mount};

    let cgroup = Path::new("/sys/fs/cgroup");
    mount(
        Some("cgroup2"),
        cgroup,
        Some("cgroup2"),
        MsFlags::MS_NOSUID | MsFlags::MS_NODEV | MsFlags::MS_NOEXEC,
        None::<&str>,
    )
    .map_err(|error| format!("cannot mount the private cgroup namespace root: {error}"))?;
    mount(
        Some(cgroup),
        cgroup,
        None::<&str>,
        MsFlags::MS_BIND,
        None::<&str>,
    )
    .map_err(|error| format!("cannot create the private cgroup bind mount: {error}"))?;
    mount(
        None::<&Path>,
        cgroup,
        None::<&str>,
        MsFlags::MS_BIND
            | MsFlags::MS_REMOUNT
            | MsFlags::MS_RDONLY
            | MsFlags::MS_NOSUID
            | MsFlags::MS_NODEV
            | MsFlags::MS_NOEXEC,
        None::<&str>,
    )
    .map_err(|error| format!("cannot remount the private cgroup view read-only: {error}"))?;
    require_cgroup_filesystem_read_only()
}

#[cfg(target_os = "linux")]
fn require_cgroup_filesystem_read_only() -> Result<(), AnyError> {
    let protected = std::fs::read_to_string("/proc/self/mountinfo")?
        .lines()
        .filter_map(|line| line.split_once(" - ").map(|(mount, _)| mount))
        .filter_map(|mount| {
            let mut fields = mount.split_whitespace();
            let root = fields.nth(3)?;
            let mountpoint = fields.next()?;
            let options = fields.next()?;
            (mountpoint == "/sys/fs/cgroup").then_some((root, options))
        })
        .any(|(root, options)| root == "/" && options.split(',').any(|option| option == "ro"));
    if !protected {
        return Err("the target cgroup filesystem is not a read-only namespace root".into());
    }
    if std::fs::read_to_string("/proc/self/cgroup")?.trim() != "0::/" {
        return Err("the target is not rooted in its private cgroup namespace".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn sched_setaffinity_seccomp_filter() -> Result<std::fs::File, AnyError> {
    namespace_seccomp_filter("reflex-host-boundary", true)
}

#[cfg(target_os = "linux")]
pub(super) fn directional_namespace_seccomp_filter() -> Result<std::fs::File, AnyError> {
    namespace_seccomp_filter("reflex-directional-namespace-boundary", false)
}

#[cfg(target_os = "linux")]
fn namespace_seccomp_filter(
    name: &str,
    deny_sched_setaffinity: bool,
) -> Result<std::fs::File, AnyError> {
    #[cfg(target_arch = "aarch64")]
    const AUDIT_ARCH: u32 = 0xc000_00b7;
    #[cfg(target_arch = "x86_64")]
    const AUDIT_ARCH: u32 = 0xc000_003e;
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    return Err(
        "the Directional namespace seccomp filter does not support this architecture".into(),
    );

    const BPF_LD_W_ABS: u16 = 0x20;
    const BPF_JMP_JEQ_K: u16 = 0x15;
    const BPF_JMP_JSET_K: u16 = 0x45;
    const BPF_RET_K: u16 = 0x06;
    const SECCOMP_RET_ERRNO_EPERM: u32 = 0x0005_0001;
    const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
    const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
    const SECCOMP_DATA_NR: u32 = 0;
    const SECCOMP_DATA_ARCH: u32 = 4;
    const SECCOMP_DATA_ARG0_LOW: u32 = 16;

    let mut denied = vec![
        nix::libc::SYS_unshare,
        nix::libc::SYS_setns,
        nix::libc::SYS_mount,
        nix::libc::SYS_umount2,
        nix::libc::SYS_pivot_root,
        nix::libc::SYS_fsopen,
        nix::libc::SYS_fsconfig,
        nix::libc::SYS_fsmount,
        nix::libc::SYS_move_mount,
        nix::libc::SYS_open_tree,
        nix::libc::SYS_mount_setattr,
    ];
    if deny_sched_setaffinity {
        denied.push(nix::libc::SYS_sched_setaffinity);
    }
    let mut instructions = vec![
        (BPF_LD_W_ABS, 0, 0, SECCOMP_DATA_ARCH),
        (BPF_JMP_JEQ_K, 1, 0, AUDIT_ARCH),
        (BPF_RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS),
        (BPF_LD_W_ABS, 0, 0, SECCOMP_DATA_NR),
    ];
    for syscall in denied {
        instructions.push((BPF_JMP_JEQ_K, 0, 1, u32::try_from(syscall)?));
        instructions.push((BPF_RET_K, 0, 0, SECCOMP_RET_ERRNO_EPERM));
    }
    instructions.extend([
        (BPF_JMP_JEQ_K, 0, 1, u32::try_from(nix::libc::SYS_clone3)?),
        // ENOSYS preserves libc/Rust's ordinary legacy-clone fallback while
        // preventing clone3 from carrying namespace flags we cannot inspect.
        (BPF_RET_K, 0, 0, 0x0005_0026),
        (BPF_JMP_JEQ_K, 0, 3, u32::try_from(nix::libc::SYS_clone)?),
        (BPF_LD_W_ABS, 0, 0, SECCOMP_DATA_ARG0_LOW),
        (
            BPF_JMP_JSET_K,
            0,
            1,
            u32::try_from(nix::libc::CLONE_NEWUSER | nix::libc::CLONE_NEWNS)?,
        ),
        (BPF_RET_K, 0, 0, SECCOMP_RET_ERRNO_EPERM),
        (BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW),
    ]);
    sealed_seccomp_filter(name, &instructions)
}

#[cfg(target_os = "linux")]
fn sealed_seccomp_filter(
    name: &str,
    instructions: &[(u16, u8, u8, u32)],
) -> Result<std::fs::File, AnyError> {
    use nix::fcntl::{FcntlArg, SealFlag, fcntl};
    use nix::sys::memfd::{MFdFlags, memfd_create};

    let descriptor = memfd_create(name, MFdFlags::MFD_ALLOW_SEALING)?;
    let mut file = std::fs::File::from(descriptor);
    for &(code, jump_true, jump_false, value) in instructions {
        file.write_all(&code.to_ne_bytes())?;
        file.write_all(&[jump_true, jump_false])?;
        file.write_all(&value.to_ne_bytes())?;
    }
    file.sync_all()?;
    fcntl(
        &file,
        FcntlArg::F_ADD_SEALS(
            SealFlag::F_SEAL_WRITE
                | SealFlag::F_SEAL_GROW
                | SealFlag::F_SEAL_SHRINK
                | SealFlag::F_SEAL_SEAL,
        ),
    )?;
    require_sealed_memfd(&file)?;
    Ok(file)
}

#[cfg(target_os = "linux")]
fn require_seccomp_affinity_boundary() -> Result<(), AnyError> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let field = |name: &str| {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .map(str::trim)
    };
    if field("NoNewPrivs:") != Some("1")
        || field("Seccomp:") != Some("2")
        || ["CapInh:", "CapPrm:", "CapEff:", "CapBnd:", "CapAmb:"]
            .into_iter()
            .any(|name| field(name) != Some("0000000000000000"))
    {
        return Err("the inherited CPU reserve seccomp boundary is absent".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn executable_identity_from_file(
    file: &mut std::fs::File,
    canonical: &Path,
) -> Result<ExecutableIdentity, AnyError> {
    use std::io::{Seek as _, SeekFrom};
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let metadata = file.metadata()?;
    if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!("{} is not a regular executable", canonical.display()).into());
    }
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(ExecutableIdentity {
        canonical_path: canonical
            .to_str()
            .ok_or("canonical executable path is not UTF-8")?
            .to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        size: metadata.size(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        content_sha256: hex(&digest.finalize()),
    })
}

#[cfg(target_os = "linux")]
fn require_pinned_fd_identity(
    fd_name: &str,
    identity: &ExecutableIdentity,
) -> Result<(), AnyError> {
    let fd = std::env::var(fd_name)?;
    let path = PathBuf::from(format!("/proc/self/fd/{fd}"));
    let file = std::fs::File::open(&path).map_err(|error| {
        format!("pinned executable descriptor {fd_name} is unavailable: {error}")
    })?;
    require_sealed_executable(&file)
        .map_err(|error| format!("pinned executable descriptor {fd_name}: {error}"))?;
    if executable_metadata(&path).map_err(|error| {
        format!("cannot inspect pinned executable descriptor {fd_name}: {error}")
    })? != executable_metadata_from_identity(identity)
    {
        return Err(format!("pinned executable descriptor {fd_name} changed identity").into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn pinned_environment(file: &std::fs::File, name: &str) -> (OsString, OsString) {
    (OsString::from(name), OsString::from(pinned_fd_number(file)))
}

#[cfg(target_os = "linux")]
const fn executable_metadata_from_identity(identity: &ExecutableIdentity) -> ExecutableMetadata {
    ExecutableMetadata {
        device: identity.device,
        inode: identity.inode,
        mode: identity.mode,
        size: identity.size,
        changed_seconds: identity.changed_seconds,
        changed_nanoseconds: identity.changed_nanoseconds,
    }
}

#[cfg(target_os = "linux")]
#[cfg(test)]
fn require_executable_metadata_identity(identity: &ExecutableIdentity) -> Result<(), AnyError> {
    if executable_metadata(Path::new(&identity.canonical_path))?
        != executable_metadata_from_identity(identity)
    {
        return Err(format!(
            "executable identity changed for {}",
            identity.canonical_path
        )
        .into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn require_running_executable_identity(identity: &ExecutableIdentity) -> Result<(), AnyError> {
    if executable_metadata(Path::new("/proc/self/exe"))?
        != executable_metadata_from_identity(identity)
    {
        return Err("running target shim differs from its registered identity".into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[cfg(test)]
fn executable_identity(path: &Path) -> Result<ExecutableIdentity, AnyError> {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    let canonical = std::fs::canonicalize(path)?;
    let metadata = std::fs::metadata(&canonical)?;
    if !metadata.file_type().is_file() || metadata.permissions().mode() & 0o111 == 0 {
        return Err(format!("{} is not a regular executable", canonical.display()).into());
    }
    Ok(ExecutableIdentity {
        canonical_path: canonical
            .to_str()
            .ok_or("canonical executable path is not UTF-8")?
            .to_owned(),
        device: metadata.dev(),
        inode: metadata.ino(),
        mode: metadata.mode(),
        size: metadata.size(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
        content_sha256: hash_file(&canonical)?,
    })
}

#[cfg(target_os = "linux")]
#[cfg(test)]
fn require_executable_identity(identity: &ExecutableIdentity) -> Result<(), AnyError> {
    if executable_identity(Path::new(&identity.canonical_path))? != *identity {
        return Err(format!(
            "executable identity changed for {}",
            identity.canonical_path
        )
        .into());
    }
    Ok(())
}

fn require_target_arguments_identity(
    arguments: &[String],
    expected_sha256: &str,
) -> Result<(), AnyError> {
    if hash_json(&arguments)? != expected_sha256 {
        return Err("target argument identity changed before exec".into());
    }
    Ok(())
}

fn require_target_shim_arguments(
    arguments: &[String],
    expected_executable: &str,
) -> Result<(), AnyError> {
    let mut expected = vec![expected_executable.to_owned()];
    if cfg!(test) {
        expected.extend([
            HOST_ISOLATION_TARGET_TEST.to_owned(),
            "--ignored".to_owned(),
            "--exact".to_owned(),
            "--nocapture".to_owned(),
        ]);
    } else {
        expected.push(HOST_ISOLATION_TARGET_COMMAND.to_owned());
    }
    if arguments != expected {
        return Err(format!(
            "target namespace shim command line is invalid: expected {expected:?}, found {arguments:?}"
        )
        .into());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn trusted_tool_path(name: &str) -> Result<PathBuf, AnyError> {
    [
        PathBuf::from("/usr/bin").join(name),
        PathBuf::from("/bin").join(name),
    ]
    .into_iter()
    .find(|path| path.exists())
    .ok_or_else(|| format!("required trusted host tool {name} is unavailable").into())
}

#[cfg(target_os = "linux")]
fn pin_trusted_tool(name: &str) -> Result<PinnedExecutable, AnyError> {
    pin_executable(&trusted_tool_path(name)?)
}

#[cfg(target_os = "linux")]
fn pin_intended_executable(path: &Path) -> Result<PinnedExecutable, AnyError> {
    if path.is_absolute() || path.components().count() > 1 {
        return pin_executable(path);
    }
    pin_trusted_tool(
        path.to_str()
            .ok_or("intended executable name is not UTF-8")?,
    )
}

#[cfg(target_os = "linux")]
fn current_user_namespace() -> Result<String, AnyError> {
    Ok(std::fs::read_link("/proc/self/ns/user")?
        .to_str()
        .ok_or("Linux user namespace identity is not UTF-8")?
        .to_owned())
}

#[cfg(target_os = "linux")]
fn current_effective_uid() -> Result<u32, AnyError> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|uids| uids.split_whitespace().nth(1))
        .ok_or_else(|| "Linux process status omits its effective UID".into())
        .and_then(|uid| uid.parse().map_err(Into::into))
}

fn merged_target_environment(
    overrides: &[(OsString, OsString)],
) -> Result<Vec<(String, String)>, AnyError> {
    let mut environment = std::env::vars().collect::<BTreeMap<_, _>>();
    for (name, value) in overrides {
        environment.insert(
            name.to_str()
                .ok_or("target environment name is not UTF-8")?
                .to_owned(),
            value
                .to_str()
                .ok_or("target environment value is not UTF-8")?
                .to_owned(),
        );
    }
    for internal in [
        RELAY_REPORT_NONCE,
        RELAY_UNIT,
        RELAY_TARGET_EXECUTABLE,
        RELAY_TARGET_ARGUMENTS,
        RELAY_LAUNCHER_IDENTITY,
        RELAY_TARGET_IDENTITY,
        RELAY_TARGET_ARGUMENTS_SHA256,
        TARGET_READY_PATH,
        TARGET_READY_TEMPORARY,
        TARGET_ACK_PATH,
        TARGET_TRUSTED_USER_NAMESPACE,
        TARGET_HOST_UID,
        TARGET_RELAY_IDENTITY,
        TARGET_TASKSET_IDENTITY,
        TARGET_SETPRIV_IDENTITY,
        TARGET_EXECUTABLE_IDENTITY,
        TARGET_ARGUMENTS,
        TARGET_ENVIRONMENT,
        TARGET_CAMPAIGN_SOURCE_PLAN,
        TARGET_ALLOWED_CPUS,
        PINNED_RELAY_FD,
        PINNED_UNSHARE_FD,
        PINNED_TASKSET_FD,
        PINNED_SETPRIV_FD,
        PINNED_SECCOMP_FD,
        PINNED_TARGET_FD,
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
        "LD_AUDIT",
        "GLIBC_TUNABLES",
    ] {
        environment.remove(internal);
    }
    Ok(environment.into_iter().collect())
}

fn trusted_supervisor_environment() -> Vec<(OsString, OsString)> {
    trusted_supervisor_environment_from(std::env::vars_os())
}

fn trusted_supervisor_environment_from(
    environment: impl IntoIterator<Item = (OsString, OsString)>,
) -> Vec<(OsString, OsString)> {
    let source = environment.into_iter().collect::<BTreeMap<_, _>>();
    let mut environment = vec![(OsString::from("PATH"), OsString::from("/usr/bin:/bin"))];
    for name in [
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "HOME",
        "LANG",
    ] {
        if let Some(value) = source.get(&OsString::from(name)) {
            environment.push((OsString::from(name), value.clone()));
        }
    }
    environment
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

fn run_control_command_before(
    program: &str,
    arguments: &[&str],
    deadline: Instant,
) -> Result<std::process::Output, AnyError> {
    run_control_command_with_environment_before(
        program,
        arguments,
        deadline,
        &trusted_supervisor_environment(),
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "control capture keeps deadline, output, reap, and process-group cleanup failures together"
)]
fn run_control_command_with_environment_before(
    program: &str,
    arguments: &[&str],
    deadline: Instant,
    environment: &[(OsString, OsString)],
) -> Result<std::process::Output, AnyError> {
    if Instant::now() >= deadline {
        return Err(format!("{program} control deadline elapsed before launch").into());
    }
    #[cfg(target_os = "linux")]
    let pinned = pin_trusted_tool(program)?;
    #[cfg(target_os = "linux")]
    let executable = pinned_fd_path(&pinned.file);
    #[cfg(not(target_os = "linux"))]
    let executable = PathBuf::from(program);
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .env_clear()
        .envs(environment.iter().cloned())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        command.process_group(0);
    }
    let mut child = command.spawn()?;
    let control_process_group = child.id();
    let exceeded = Arc::new(AtomicBool::new(false));
    let Some(stdout) = child.stdout.take() else {
        return abort_control_setup(
            &mut child,
            Vec::new(),
            "control command omits stdout",
            deadline,
        );
    };
    let stdout_reader = match spawn_control_reader(stdout, "stdout", Arc::clone(&exceeded)) {
        Ok(reader) => reader,
        Err(error) => {
            return abort_control_setup(&mut child, Vec::new(), &error.to_string(), deadline);
        }
    };
    let Some(stderr) = child.stderr.take() else {
        return abort_control_setup(
            &mut child,
            vec![stdout_reader],
            "control command omits stderr",
            deadline,
        );
    };
    let stderr_reader = match spawn_control_reader(stderr, "stderr", Arc::clone(&exceeded)) {
        Ok(reader) => reader,
        Err(error) => {
            return abort_control_setup(
                &mut child,
                vec![stdout_reader],
                &error.to_string(),
                deadline,
            );
        }
    };
    let mut terminal_error = None;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(error) => {
                terminal_error = Some(format!("{program} control observation failed: {error}"));
                let (status, quiescent) = stop_control_child(&mut child, deadline);
                if !quiescent {
                    terminal_error
                        .as_mut()
                        .expect("control failure exists")
                        .push_str("; process group remained live");
                }
                break status;
            }
        }
        if exceeded.load(Ordering::Acquire) {
            terminal_error = Some(format!("{program} control output exceeded its byte limit"));
            let (status, quiescent) = stop_control_child(&mut child, deadline);
            if !quiescent {
                terminal_error
                    .as_mut()
                    .expect("control failure exists")
                    .push_str("; process group remained live");
            }
            break status;
        }
        if Instant::now() >= deadline {
            terminal_error = Some(format!("{program} control command exceeded its deadline"));
            let (status, quiescent) = stop_control_child(&mut child, deadline);
            if !quiescent {
                terminal_error
                    .as_mut()
                    .expect("control failure exists")
                    .push_str("; process group remained live");
            }
            break status;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let mut failures = terminal_error.into_iter().collect::<Vec<_>>();
    if !terminate_process_tree_before(control_process_group, deadline) {
        failures.push(format!("{program} control process group remained live"));
    }
    let stdout = join_bounded_reader_before(stdout_reader, "control stdout", deadline);
    let stderr = join_bounded_reader_before(stderr_reader, "control stderr", deadline);
    if status.is_none() {
        failures.push(format!("{program} control child could not be reaped"));
    }
    let (stdout, stdout_truncated) = match stdout {
        Ok(stdout) => stdout,
        Err(error) => {
            failures.push(error.to_string());
            (String::new(), false)
        }
    };
    let (stderr, stderr_truncated) = match stderr {
        Ok(stderr) => stderr,
        Err(error) => {
            failures.push(error.to_string());
            (String::new(), false)
        }
    };
    if stdout_truncated || stderr_truncated || exceeded.load(Ordering::Acquire) {
        failures.push(format!("{program} control output exceeded its byte limit"));
    }
    if !failures.is_empty() {
        failures.sort();
        failures.dedup();
        return Err(failures.join("; ").into());
    }
    Ok(std::process::Output {
        status: status.ok_or("control command lost its exit status")?,
        stdout: stdout.into_bytes(),
        stderr: stderr.into_bytes(),
    })
}

fn abort_control_setup(
    child: &mut std::process::Child,
    readers: Vec<std::thread::JoinHandle<std::io::Result<(String, bool)>>>,
    error: &str,
    deadline: Instant,
) -> Result<std::process::Output, AnyError> {
    let (status, quiescent) = stop_control_child(child, deadline);
    let mut failures = vec![format!("control capture setup failed ({error})")];
    if status.is_none() {
        failures.push("control child reap failed".into());
    }
    if !quiescent {
        failures.push("control child process group remained live".into());
    }
    for reader in readers {
        if let Err(error) = join_bounded_reader_before(reader, "control setup failure", deadline) {
            failures.push(format!("control reader shutdown failed ({error})"));
        }
    }
    Err(failures.join("; ").into())
}

fn stop_control_child(
    child: &mut std::process::Child,
    deadline: Instant,
) -> (Option<ExitStatus>, bool) {
    let process_group = child.id();
    let _ = child.kill();
    let status = wait_child_before(child, deadline).ok();
    let quiescent = terminate_process_tree_before(process_group, deadline);
    (status, quiescent)
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
        CAPTURE_STREAM_LIMIT_BYTES, HostIsolationPolicy, LARGE_CAMPAIGN_NONCE,
        LEAN_PUBLIC_NESTED_RESIDENT_LIMIT, LEAN_PUBLIC_NESTED_WALL_LIMIT,
        LEAN_TEMPORAL_NESTED_RESIDENT_LIMIT, LEAN_TEMPORAL_NESTED_WALL_LIMIT, LargeCampaign,
        SUPERVISOR_OBSERVATION_HEADROOM, authenticate_large_campaign_parent, capture_child_bounded,
        capture_child_host_isolated, capture_large_campaign_child, inherited_host_isolation,
        isolation_boundary_cpu_list, observe_memory_events, outer_observation_resident_limit,
        parse_cgroup_populated, parse_completed_child_cpu_ticks, parse_cpu_list,
        parse_memory_events, parse_memory_info, parse_peak_resident_bytes, parse_pids_events,
        parse_resident_bytes, plan_host_isolation, read_bounded_stream, ticks_to_nanoseconds,
        validate_cpu_boundary, validate_live_memory, validate_monitor_handshake,
        validate_parent_process_evidence,
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
            None,
        )
        .expect("the host can satisfy the isolation policy");

        assert_eq!(isolation.memory_limit_bytes, 40 * gib);
        assert_eq!(isolation.allowed_cpu_list, "0,1,2,3,4,5,6");
        assert_eq!(isolation.reserved_cpus, vec![7]);
    }

    #[test]
    fn scaling_selects_exactly_seven_experiment_cpus_and_one_reserve_on_large_hosts() {
        let gib = 1024_u64.pow(3);
        let isolation = plan_host_isolation(
            64 * gib,
            62 * gib,
            &(0..16).collect::<Vec<_>>(),
            4 * gib,
            16 * gib,
            1,
            Some(7),
        )
        .expect("a sixteen-CPU host supports the fixed scaling treatment");
        assert_eq!(isolation.allowed_cpu_list, "0,1,2,3,4,5,6");
        assert_eq!(isolation.reserved_cpus, vec![7]);
        assert_eq!(isolation_boundary_cpu_list(&isolation), "0,1,2,3,4,5,6");
        assert_eq!(
            super::isolation_host_cpu_list(&isolation),
            "0,1,2,3,4,5,6,7"
        );
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
            None,
        )
        .expect_err("available memory equal to work plus reserve is required");

        assert!(error.to_string().contains("host memory reserve"));
    }

    #[test]
    fn inherited_cpu_boundary_requires_an_exact_disjoint_partition() {
        assert!(validate_cpu_boundary(&[0, 1, 2], &[3], &[0, 1, 2], &[0, 1, 2, 3], 1).is_ok());
        assert!(validate_cpu_boundary(&[0, 1, 2], &[2], &[0, 1, 2], &[0, 1, 2, 3], 1).is_err());
        assert!(validate_cpu_boundary(&[0, 1], &[3], &[0, 1], &[0, 1, 2, 3], 1).is_err());
        assert!(
            validate_cpu_boundary(&[0, 1, 2], &[3, 4], &[0, 1, 2], &[0, 1, 2, 3, 4], 1).is_err()
        );
    }

    #[test]
    fn inherited_memory_boundary_uses_live_headroom() {
        let gib = 1024_u64.pow(3);
        let policy = HostIsolationPolicy {
            memory_limit_bytes: 40 * gib,
            memory_reserve_bytes: 16 * gib,
            cpu_reserve: 1,
            experiment_cpu_limit: None,
        };
        assert!(validate_live_memory(policy, 64 * gib, 52 * gib, 4 * gib).is_ok());
        assert!(validate_live_memory(policy, 64 * gib, 51 * gib, 4 * gib).is_err());
        assert!(validate_live_memory(policy, 55 * gib, 64 * gib, 4 * gib).is_err());
    }

    #[test]
    fn every_large_campaign_preserves_host_cpu_and_memory() {
        for command in LargeCampaign::COMMANDS {
            let campaign = LargeCampaign::for_command(command)
                .expect("every registered large parent has one launch classification");
            let policy = campaign.isolation_policy();
            assert_eq!(campaign.command(), command);
            assert_eq!(policy.cpu_reserve, 1);
            assert!(policy.memory_limit_bytes > 0);
            assert!(policy.memory_reserve_bytes >= 8 * 1024 * 1024 * 1024);
        }
    }

    #[test]
    fn nested_supervisors_have_cleanup_headroom_inside_the_outer_boundary() {
        for (campaign, nested_wall, nested_resident) in [
            (
                LargeCampaign::LeanPublicOptimizerDevelopment,
                LEAN_PUBLIC_NESTED_WALL_LIMIT,
                LEAN_PUBLIC_NESTED_RESIDENT_LIMIT,
            ),
            (
                LargeCampaign::LeanTemporalAuditConfirmation,
                LEAN_TEMPORAL_NESTED_WALL_LIMIT,
                LEAN_TEMPORAL_NESTED_RESIDENT_LIMIT,
            ),
        ] {
            let policy = campaign.isolation_policy();
            assert!(campaign.wall_limit() > nested_wall);
            assert_eq!(
                policy.memory_limit_bytes - nested_resident,
                SUPERVISOR_OBSERVATION_HEADROOM
            );
            assert!(outer_observation_resident_limit(policy) > nested_resident);
        }
    }

    #[test]
    fn scientific_child_authentication_rejects_absent_and_wrong_parents() {
        assert!(authenticate_large_campaign_parent("baseline-child", None).is_err());
        assert!(
            authenticate_large_campaign_parent("baseline-child", Some("verification-scaling"))
                .is_err()
        );
        assert_eq!(
            authenticate_large_campaign_parent("baseline-child", Some("baseline")).unwrap(),
            LargeCampaign::Baseline
        );
        assert_eq!(
            authenticate_large_campaign_parent(
                "causal-child",
                Some("causal-development-performance")
            )
            .unwrap(),
            LargeCampaign::CausalDevelopment
        );
    }

    #[test]
    fn scientific_child_requires_same_executable_command_and_inherited_nonce() {
        let nonce = "ab".repeat(32);
        let environment = format!("A=B\0{LARGE_CAMPAIGN_NONCE}={nonce}\0");
        let valid = validate_parent_process_evidence(
            LargeCampaign::Baseline,
            std::path::Path::new("/tmp/xtask"),
            std::path::Path::new("/tmp/xtask"),
            b"/tmp/xtask\0baseline\0--output\0report.json\0",
            environment.as_bytes(),
            &nonce,
        );
        assert!(valid.is_ok());
        assert!(
            validate_parent_process_evidence(
                LargeCampaign::Baseline,
                std::path::Path::new("/tmp/xtask"),
                std::path::Path::new("/tmp/forged"),
                b"/tmp/xtask\0baseline\0",
                environment.as_bytes(),
                &nonce,
            )
            .is_err()
        );
        assert!(
            validate_parent_process_evidence(
                LargeCampaign::Baseline,
                std::path::Path::new("/tmp/xtask"),
                std::path::Path::new("/tmp/xtask"),
                b"/tmp/xtask\0verification-scaling\0",
                environment.as_bytes(),
                &nonce,
            )
            .is_err()
        );
        assert!(
            validate_parent_process_evidence(
                LargeCampaign::Baseline,
                std::path::Path::new("/tmp/xtask"),
                std::path::Path::new("/tmp/xtask"),
                b"/tmp/xtask\0baseline\0",
                b"A=B\0",
                &nonce,
            )
            .is_err()
        );
    }

    #[test]
    fn monitor_round_trip_uses_an_exact_challenge_and_distinct_domains() {
        let handshake = [0x5a; super::MONITOR_HANDSHAKE_BYTES];
        let expected = super::hex(&handshake);
        assert!(validate_monitor_handshake(&expected, &handshake).is_ok());
        assert!(validate_monitor_handshake(&expected, &handshake[..31]).is_err());
        let mut wrong = handshake;
        wrong[0] ^= 1;
        assert!(validate_monitor_handshake(&expected, &wrong).is_err());
        assert_ne!(
            super::monitor_handshake_digest(super::MONITOR_RESPONSE_DOMAIN, &handshake),
            super::monitor_handshake_digest(super::MONITOR_CONFIRMATION_DOMAIN, &handshake)
        );
    }

    #[test]
    fn live_monitor_identity_requires_a_distinct_same_binary_registered_campaign() {
        let executable = std::path::Path::new("/tmp/xtask");
        let command_line = b"/tmp/xtask\0baseline\0";
        assert!(
            super::validate_monitor_process_evidence(
                LargeCampaign::Baseline.command().as_bytes(),
                10,
                20,
                30,
                executable,
                executable,
                command_line,
            )
            .is_ok()
        );
        for invalid_monitor in [10, 20] {
            assert!(
                super::validate_monitor_process_evidence(
                    LargeCampaign::Baseline.command().as_bytes(),
                    10,
                    20,
                    invalid_monitor,
                    executable,
                    executable,
                    command_line,
                )
                .is_err()
            );
        }
        assert!(
            super::validate_monitor_process_evidence(
                LargeCampaign::Baseline.command().as_bytes(),
                10,
                20,
                30,
                executable,
                std::path::Path::new("/tmp/not-xtask"),
                command_line,
            )
            .is_err()
        );
        assert!(
            super::validate_monitor_process_evidence(
                LargeCampaign::Baseline.command().as_bytes(),
                10,
                20,
                30,
                executable,
                executable,
                b"/tmp/xtask\0verification-scaling\0",
            )
            .is_err()
        );
    }

    #[test]
    fn valid_monitor_process_evidence_does_not_substitute_for_channel_ownership() {
        let executable = std::path::Path::new("/tmp/xtask");
        assert!(
            super::validate_monitor_process_evidence(
                LargeCampaign::Baseline.command().as_bytes(),
                10,
                20,
                30,
                executable,
                executable,
                b"/tmp/xtask\0baseline\0",
            )
            .is_ok()
        );

        let stdin = super::PipeIdentity {
            device: 1,
            inode: 10,
        };
        let stderr = super::PipeIdentity {
            device: 1,
            inode: 11,
        };
        let unrelated_monitor_pipes = std::collections::BTreeSet::from([
            super::PipeIdentity {
                device: 1,
                inode: 12,
            },
            super::PipeIdentity {
                device: 1,
                inode: 13,
            },
        ]);
        assert!(
            super::validate_monitor_channel_ownership(stdin, stderr, &unrelated_monitor_pipes)
                .is_err()
        );
        assert!(
            super::validate_monitor_channel_ownership(
                stdin,
                stderr,
                &std::collections::BTreeSet::from([stdin, stderr]),
            )
            .is_ok()
        );
    }

    #[test]
    fn relay_reports_reject_partial_malformed_and_forged_evidence() {
        let authentication = [0x3c; super::MONITOR_HANDSHAKE_BYTES];
        let target = super::ExecutableIdentity {
            canonical_path: "/bin/target".into(),
            device: 1,
            inode: 2,
            mode: 0o100_755,
            size: 3,
            changed_seconds: 4,
            changed_nanoseconds: 5,
            content_sha256: "11".repeat(32),
        };
        let arguments_sha256 = "22".repeat(32);
        let mut report = super::RelayReport {
            schema: "reflex-host-relay-report-v2".into(),
            nonce: "nonce".into(),
            unit: "unit.scope".into(),
            relay_pid: 10,
            child_pid: 11,
            termination: super::RelayTermination::Exit { code: 7 },
            target_identity: target.clone(),
            target_arguments_sha256: arguments_sha256.clone(),
            authentication_sha256: String::new(),
        };
        report.authentication_sha256 = super::relay_report_authentication(&report, &authentication)
            .expect("report authentication is reproducible");
        let bytes = serde_json::to_vec(&report).unwrap();
        let decode = |bytes: &[u8], nonce: &str, unit: &str, authentication: &[u8; 32]| {
            super::decode_relay_report(
                bytes,
                nonce,
                unit,
                authentication,
                &target,
                &arguments_sha256,
            )
        };
        assert!(decode(&bytes, "nonce", "unit.scope", &authentication).is_ok());
        assert!(decode(b"{", "nonce", "unit.scope", &authentication).is_err());
        assert!(decode(&bytes, "forged", "unit.scope", &authentication).is_err());
        assert!(decode(&bytes, "nonce", "other.scope", &authentication).is_err());
        assert!(decode(&bytes, "nonce", "unit.scope", &[0x7d; 32]).is_err());

        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let mut framed = Vec::new();
        let mut forged_report = report.clone();
        forged_report.authentication_sha256 = "00".repeat(32);
        super::write_relay_frame(&mut framed, &serde_json::to_vec(&forged_report).unwrap())
            .unwrap();
        super::write_relay_frame(&mut framed, &bytes).unwrap();
        super::extract_authenticated_relay_frames(&mut framed, &authentication, &sender);
        assert_eq!(receiver.try_recv().unwrap().child_pid, report.child_pid);
        assert!(receiver.try_recv().is_err());

        let mut substituted = serde_json::from_slice::<super::RelayReport>(&bytes).unwrap();
        substituted.target_identity.inode += 1;
        substituted.authentication_sha256 =
            super::relay_report_authentication(&substituted, &authentication).unwrap();
        assert!(
            decode(
                &serde_json::to_vec(&substituted).unwrap(),
                "nonce",
                "unit.scope",
                &authentication,
            )
            .is_err()
        );
        let mut altered_arguments = serde_json::from_slice::<super::RelayReport>(&bytes).unwrap();
        altered_arguments.target_arguments_sha256 = "33".repeat(32);
        altered_arguments.authentication_sha256 =
            super::relay_report_authentication(&altered_arguments, &authentication).unwrap();
        assert!(
            decode(
                &serde_json::to_vec(&altered_arguments).unwrap(),
                "nonce",
                "unit.scope",
                &authentication,
            )
            .is_err()
        );

        let mut invalid = report;
        invalid.termination = super::RelayTermination::Signal {
            signal: 0,
            core_dumped: false,
        };
        assert!(
            super::decode_relay_report(
                &serde_json::to_vec(&invalid).unwrap(),
                "nonce",
                "unit.scope",
                &authentication,
                &target,
                &arguments_sha256,
            )
            .is_err()
        );
    }

    #[test]
    fn relay_report_hmac_matches_the_sha256_standard_vector() {
        assert_eq!(
            super::hex(&super::hmac_sha256(&[0x0b; 20], b"Hi There")),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn executable_identity_detects_in_place_substitution() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = std::env::temp_dir().join(format!(
            "reflex-executable-identity-test-{}",
            std::process::id()
        ));
        std::fs::write(&path, b"first executable bytes").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let identity = super::executable_identity(&path).unwrap();
        assert!(super::require_executable_identity(&identity).is_ok());
        assert!(super::require_executable_metadata_identity(&identity).is_ok());
        std::fs::write(&path, b"other executable bytes").unwrap();
        assert!(super::require_executable_identity(&identity).is_err());
        let replaced_identity = super::executable_identity(&path).unwrap();
        let replacement = path.with_extension("replacement");
        std::fs::write(&replacement, b"other executable bytes").unwrap();
        std::fs::set_permissions(&replacement, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert!(super::require_executable_metadata_identity(&replaced_identity).is_err());
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn target_argument_identity_rejects_an_altered_vector() {
        let registered = vec!["--exact".to_owned(), "registered".to_owned()];
        let digest = super::hash_json(&registered).unwrap();
        assert!(super::require_target_arguments_identity(&registered, &digest).is_ok());
        let altered = vec!["--exact".to_owned(), "altered".to_owned()];
        assert!(super::require_target_arguments_identity(&altered, &digest).is_err());
    }

    #[test]
    fn target_shim_ready_record_requires_its_exact_argument_vector() {
        let executable = "/absolute/xtask";
        let exact = vec![
            executable.to_owned(),
            super::HOST_ISOLATION_TARGET_TEST.to_owned(),
            "--ignored".to_owned(),
            "--exact".to_owned(),
            "--nocapture".to_owned(),
        ];
        assert!(super::require_target_shim_arguments(&exact, executable).is_ok());
        let mut altered = exact;
        altered.swap(2, 3);
        assert!(super::require_target_shim_arguments(&altered, executable).is_err());
    }

    #[test]
    fn relay_terminal_state_requires_only_the_idle_relay() {
        assert!(super::validate_relay_only_state("41\n", "", 41).is_ok());
        assert!(super::validate_relay_only_state("41\n42\n", "", 41).is_err());
        assert!(super::validate_relay_only_state("41\n", "42\n", 41).is_err());
        assert!(super::validate_relay_only_state("", "", 41).is_err());
    }

    #[test]
    fn target_rendezvous_reads_are_bounded_regular_files() {
        use std::io::Write as _;

        let directory = std::env::temp_dir().join(format!(
            "reflex-relay-input-test-{}-{}",
            std::process::id(),
            super::SYSTEMD_SCOPE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let regular = directory.join("regular");
        std::fs::write(&regular, b"report").unwrap();
        assert_eq!(
            super::read_bounded_regular_file(&regular).unwrap(),
            b"report"
        );

        let oversized = directory.join("oversized");
        let mut file = std::fs::File::create(&oversized).unwrap();
        file.write_all(&vec![0_u8; super::RELAY_REPORT_LIMIT_BYTES + 1])
            .unwrap();
        assert!(super::read_bounded_regular_file(&oversized).is_err());

        let symlink = directory.join("symlink");
        std::os::unix::fs::symlink(&regular, &symlink).unwrap();
        assert!(super::read_bounded_regular_file(&symlink).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn every_kernel_oom_attempt_is_resource_evidence() {
        assert!(!super::memory_resource_exhausted(
            super::CgroupMemoryEvents {
                oom: 0,
                oom_kill: 0,
            }
        ));
        assert!(super::memory_resource_exhausted(
            super::CgroupMemoryEvents {
                oom: 1,
                oom_kill: 0,
            }
        ));
        assert!(super::memory_resource_exhausted(
            super::CgroupMemoryEvents {
                oom: 0,
                oom_kill: 1,
            }
        ));
    }

    #[test]
    fn host_control_commands_obey_their_deadline() {
        let started = std::time::Instant::now();
        let error = super::run_control_command_before(
            "sleep",
            &["30"],
            started + Duration::from_millis(50),
        )
        .expect_err("a host control command cannot outlive its deadline");
        assert!(error.to_string().contains("deadline"));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn host_control_output_is_capped_while_produced() {
        let started = std::time::Instant::now();
        let error = super::run_control_command_before("yes", &[], started + Duration::from_secs(2))
            .expect_err("unbounded control output must be terminated");
        assert!(error.to_string().contains("byte limit"));
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn terminal_system_control_uses_one_aggregate_deadline() {
        let now = std::time::Instant::now();
        let campaign = now + Duration::from_millis(50);
        let deadline = super::terminal_control_deadline(Some(campaign));
        assert!(deadline <= campaign);
        assert!(deadline <= now + super::SYSTEM_CONTROL_TIMEOUT);
    }

    #[test]
    fn non_campaign_process_tree_retains_one_terminal_cleanup_window() {
        let now = std::time::Instant::now();
        let execution = now + Duration::from_millis(20);
        let deadline = super::terminal_boundary_deadline(
            &super::TerminationBoundary::ProcessTree,
            Some(execution),
        );
        let after = std::time::Instant::now();
        assert!(deadline > execution);
        assert!(deadline >= now + super::SYSTEM_CONTROL_TIMEOUT);
        assert!(deadline <= after + super::SYSTEM_CONTROL_TIMEOUT);
    }

    #[test]
    fn cgroup_kill_fallback_cannot_reset_the_aggregate_deadline() {
        use std::cell::Cell;

        let started = std::time::Instant::now();
        let deadline = started + Duration::from_millis(60);
        std::thread::sleep(Duration::from_millis(45));
        let resolved = Cell::new(false);
        let killed = Cell::new(false);
        let result = super::cgroup_kill_fallback_before(
            "test.scope",
            deadline,
            || {
                resolved.set(true);
                std::thread::sleep(Duration::from_millis(25));
                Ok(std::path::PathBuf::from("/unused"))
            },
            |_| {
                killed.set(true);
                Ok(())
            },
            |_| Ok(()),
        );
        assert!(result.unwrap_err().to_string().contains("deadline"));
        assert!(resolved.get());
        assert!(!killed.get(), "fallback write received a fresh time window");
        assert!(started.elapsed() < Duration::from_millis(250));
    }

    #[test]
    fn trusted_control_environment_excludes_loader_poison() {
        let trusted = super::trusted_supervisor_environment_from([
            (OsString::from("PATH"), OsString::from("/tmp/hostile")),
            (
                OsString::from("DBUS_SESSION_BUS_ADDRESS"),
                OsString::from("unix:path=/tmp/test-bus"),
            ),
            (OsString::from("LD_PRELOAD"), OsString::from("/tmp/a.so")),
            (
                OsString::from("LD_LIBRARY_PATH"),
                OsString::from("/tmp/lib"),
            ),
            (OsString::from("LD_AUDIT"), OsString::from("/tmp/audit.so")),
            (
                OsString::from("GLIBC_TUNABLES"),
                OsString::from("glibc.malloc.check=3"),
            ),
            (OsString::from("UNREGISTERED"), OsString::from("leak")),
        ]);
        let output = super::run_control_command_with_environment_before(
            "env",
            &[],
            std::time::Instant::now() + Duration::from_secs(2),
            &trusted,
        )
        .expect("sealed env control command runs");
        assert!(output.status.success());
        let stdout = String::from_utf8(output.stdout).unwrap();
        let observed = stdout
            .lines()
            .map(|line| line.split_once('=').unwrap())
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(observed.get("PATH"), Some(&"/usr/bin:/bin"));
        assert_eq!(
            observed.get("DBUS_SESSION_BUS_ADDRESS"),
            Some(&"unix:path=/tmp/test-bus")
        );
        assert_eq!(observed.len(), 2);
        for forbidden in [
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "LD_AUDIT",
            "GLIBC_TUNABLES",
            "UNREGISTERED",
        ] {
            assert!(!observed.contains_key(forbidden));
        }
    }

    #[test]
    fn self_consistent_challenge_without_a_live_round_trip_fails() {
        const CHILD_MARKER: &str = "REFLEX_INCOMPLETE_MONITOR_ROUND_TRIP_CHILD";
        let challenge = [0x6d; super::MONITOR_HANDSHAKE_BYTES];
        if std::env::var_os(CHILD_MARKER).is_some() {
            let error = super::complete_monitor_round_trip(&super::hex(&challenge))
                .expect_err("a one-way challenge cannot establish monitor liveness");
            assert!(error.to_string().contains("confirmation"));
            return;
        }

        let executable = std::env::current_exe().expect("test executable exists");
        let mut child = std::process::Command::new(executable)
            .args([
                "--exact",
                "harness::tests::self_consistent_challenge_without_a_live_round_trip_fails",
                "--nocapture",
            ])
            .env(CHILD_MARKER, "1")
            .env(LARGE_CAMPAIGN_NONCE, super::hex(&challenge))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .expect("round-trip child starts");
        let mut stdin = child.stdin.take().expect("child stdin is piped");
        std::io::Write::write_all(&mut stdin, &challenge).expect("challenge is written");
        drop(stdin);
        let output = child.wait_with_output().expect("round-trip child exits");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn terminal_memory_evidence_loss_is_always_fail_closed() {
        let mut events = super::CgroupMemoryEvents::default();
        let mut seen = true;
        let mut lost = false;
        observe_memory_events(
            &Err("terminal evidence unavailable".into()),
            true,
            &mut events,
            &mut seen,
            &mut lost,
        );
        assert!(lost);
    }

    #[test]
    fn captured_output_is_a_bounded_tail_with_explicit_truncation() {
        let mut bytes = vec![b'x'; usize::try_from(CAPTURE_STREAM_LIMIT_BYTES).unwrap() + 32];
        bytes.splice(bytes.len() - 4.., *b"tail");
        let exceeded = std::sync::atomic::AtomicBool::new(false);
        let (captured, truncated) =
            read_bounded_stream(std::io::Cursor::new(bytes), &exceeded).unwrap();
        assert!(truncated);
        assert!(exceeded.load(std::sync::atomic::Ordering::Acquire));
        assert!(captured.starts_with("[earlier child output truncated by supervisor]\n"));
        assert!(captured.ends_with("tail"));
        assert!(captured.len() <= usize::try_from(CAPTURE_STREAM_LIMIT_BYTES).unwrap() + 64);
    }

    #[test]
    fn pinned_executable_is_sealed_across_in_place_swap_and_restore() {
        use nix::errno::Errno;

        let directory =
            std::env::temp_dir().join(format!("reflex-sealed-exec-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let source = directory.join("target");
        std::fs::copy("/usr/bin/true", &source).unwrap();
        let pinned = super::pin_executable(&source).unwrap();
        assert_eq!(
            nix::unistd::write(&pinned.file, b"x").unwrap_err(),
            Errno::EPERM
        );
        std::fs::copy("/usr/bin/false", &source).unwrap();
        assert!(
            std::process::Command::new(super::pinned_fd_path(&pinned.file))
                .status()
                .unwrap()
                .success()
        );
        std::fs::copy("/usr/bin/true", &source).unwrap();
        let script = directory.join("script");
        std::fs::write(&script, b"#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(&script).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut permissions, 0o700);
        std::fs::set_permissions(&script, permissions).unwrap();
        assert!(super::pin_executable(&script).is_err());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn large_campaign_descendants_refuse_to_launch_without_the_parent_boundary() {
        let prefix = std::env::temp_dir().join(format!(
            "reflex-unsupervised-large-child-test-{}",
            std::process::id()
        ));
        let error = capture_large_campaign_child(
            std::path::Path::new("/definitely-not-a-reflex-child"),
            &[],
            &prefix,
            Some(Duration::from_secs(1)),
            None,
            &[],
        )
        .err()
        .expect("a descendant must not launch outside its classified parent cgroup");
        assert!(error.to_string().contains("host isolation evidence"));
    }

    #[test]
    fn linux_host_inputs_are_parsed_without_guessing_cpu_ids() {
        assert_eq!(
            parse_memory_info("MemTotal: 65536 kB\nMemAvailable: 49152 kB\n"),
            Some((64 * 1024 * 1024, 48 * 1024 * 1024))
        );
        assert_eq!(parse_cpu_list("0-2,5,7-8"), Some(vec![0, 1, 2, 5, 7, 8]));
        assert_eq!(parse_cpu_list("2-1"), None);
        let events = parse_memory_events("low 0\nhigh 0\nmax 3\noom 2\noom_kill 1\n")
            .expect("kernel memory events parse");
        assert_eq!(events.oom, 2);
        assert_eq!(events.oom_kill, 1);
        assert_eq!(parse_pids_events("max 7\n").unwrap().max, 7);
        assert_eq!(
            parse_cgroup_populated("populated 0\nfrozen 0\n"),
            Some(false)
        );
        assert_eq!(
            parse_cgroup_populated("populated 1\nfrozen 0\n"),
            Some(true)
        );
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and taskset"]
    fn host_isolated_child_observes_kernel_boundaries() {
        const CHILD_MARKER: &str = "REFLEX_HOST_ISOLATION_INTEGRATION_CHILD";
        const MEMORY_LIMIT: u64 = 512 * 1024 * 1024;
        if std::env::var_os(CHILD_MARKER).is_some() {
            use nix::sched::{CpuSet, sched_setaffinity};
            use nix::unistd::Pid;

            let isolation = inherited_host_isolation().expect("child isolation is authentic");
            assert_eq!(isolation.memory_limit_bytes, MEMORY_LIMIT);
            assert_eq!(
                std::thread::available_parallelism()
                    .expect("child parallelism is visible")
                    .get(),
                isolation.allowed_cpu_list.split(',').count()
            );
            let host = parse_cpu_list(&std::env::var(super::ISOLATION_HOST_CPUS).unwrap()).unwrap();
            let mut widened = CpuSet::new();
            for cpu in host {
                widened.set(cpu).unwrap();
            }
            assert_eq!(
                sched_setaffinity(Pid::from_raw(0), &widened).unwrap_err(),
                nix::errno::Errno::EPERM
            );
            let descendant = std::process::Command::new("/usr/bin/taskset")
                .args([
                    "--cpu-list",
                    &std::env::var(super::ISOLATION_HOST_CPUS).unwrap(),
                    "/usr/bin/true",
                ])
                .status()
                .unwrap();
            assert!(!descendant.success());
            super::require_cgroup_filesystem_read_only()
                .expect("the target sees a read-only cgroup filesystem");
            let scope = super::current_cgroup_path().unwrap();
            let self_escape =
                std::fs::write(scope.join("cgroup.procs"), std::process::id().to_string())
                    .expect_err("the target cannot rewrite its scope membership");
            assert!(matches!(
                self_escape.raw_os_error(),
                Some(nix::libc::EROFS | nix::libc::EPERM)
            ));
            assert!(super::process_belongs_to_cgroup(std::process::id(), &scope).unwrap());
            let mut descendant = std::process::Command::new("/usr/bin/sleep")
                .arg("30")
                .spawn()
                .unwrap();
            let descendant_escape =
                std::fs::write(scope.join("cgroup.procs"), descendant.id().to_string())
                    .expect_err("a descendant cannot rewrite its scope membership");
            assert!(matches!(
                descendant_escape.raw_os_error(),
                Some(nix::libc::EROFS | nix::libc::EPERM)
            ));
            assert!(super::process_belongs_to_cgroup(descendant.id(), &scope).unwrap());
            descendant.kill().unwrap();
            descendant.wait().unwrap();
            let namespace_escape = std::process::Command::new("/usr/bin/unshare")
                .args(["--mount", "/usr/bin/true"])
                .status()
                .unwrap();
            assert!(!namespace_escape.success());
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
                experiment_cpu_limit: None,
            },
            &[(OsString::from(CHILD_MARKER), OsString::from("1"))],
        )
        .expect("host-isolated child starts");

        assert_eq!(isolation.memory_limit_bytes, MEMORY_LIMIT);
        assert!(!capture.timed_out, "{}", capture.stderr);
        assert!(!capture.resident_limit_exceeded, "{}", capture.stderr);
        assert!(!capture.boundary.evidence_failed, "{}", capture.stderr);
        assert!(!capture.boundary.cleanup_failed, "{}", capture.stderr);
        assert!(capture.status.success(), "{}", capture.stderr);
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope, pids controller, and user namespaces"]
    fn host_isolated_fork_storm_is_classified_and_cleaned() {
        const CHILD_MARKER: &str = "REFLEX_HOST_PIDS_INTEGRATION_CHILD";
        if std::env::var_os(CHILD_MARKER).is_some() {
            let mut children = Vec::new();
            for _ in 0..400 {
                match std::process::Command::new("/usr/bin/sleep")
                    .arg("0.2")
                    .spawn()
                {
                    Ok(child) => children.push(child),
                    Err(_) => break,
                }
            }
            for mut child in children {
                let _ = child.wait();
            }
            return;
        }
        let prefix = std::env::temp_dir().join(format!(
            "reflex-host-pids-integration-test-{}",
            std::process::id()
        ));
        let (capture, _) = capture_child_host_isolated(
            &std::env::current_exe().unwrap(),
            &[
                OsString::from("--exact"),
                OsString::from(
                    "harness::tests::host_isolated_fork_storm_is_classified_and_cleaned",
                ),
                OsString::from("--nocapture"),
                OsString::from("--ignored"),
            ],
            &prefix,
            Duration::from_secs(5),
            HostIsolationPolicy {
                memory_limit_bytes: 512 * 1024 * 1024,
                memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                cpu_reserve: 1,
                experiment_cpu_limit: None,
            },
            &[(OsString::from(CHILD_MARKER), OsString::from("1"))],
        )
        .expect("fork storm remains supervised");
        assert!(capture.boundary.cgroup_pids_exhausted);
        assert!(capture.resident_limit_exceeded);
        assert!(!capture.boundary.cleanup_failed, "{}", capture.stderr);
        assert!(!capture.boundary.evidence_failed, "{}", capture.stderr);
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope, user namespaces, and Python"]
    fn scoped_target_cannot_inspect_outer_or_relay_memory() {
        const CHILD_MARKER: &str = "REFLEX_ANTI_PTRACE_INTEGRATION_CHILD";
        const OUTER_PID: &str = "REFLEX_ANTI_PTRACE_OUTER_PID";
        const TEST_NAME: &str =
            "harness::tests::scoped_target_cannot_inspect_outer_or_relay_memory";
        if std::env::var_os(CHILD_MARKER).is_some() {
            let status = std::fs::read_to_string("/proc/self/status").unwrap();
            let relay = status
                .lines()
                .find_map(|line| line.strip_prefix("PPid:")?.trim().parse::<u32>().ok())
                .unwrap();
            let outer = std::env::var(OUTER_PID).unwrap();
            let script = r#"
import ctypes, errno, os, sys
libc = ctypes.CDLL(None, use_errno=True)
class IOV(ctypes.Structure):
    _fields_ = [("base", ctypes.c_void_p), ("length", ctypes.c_size_t)]
for raw_pid in sys.argv[1:]:
    pid = int(raw_pid)
    try:
        os.open(f"/proc/{pid}/mem", os.O_RDONLY)
        raise SystemExit(f"proc mem unexpectedly opened for {pid}")
    except OSError as error:
        if error.errno not in (errno.EACCES, errno.EPERM):
            raise
    if libc.ptrace(16, pid, None, None) != -1 or ctypes.get_errno() != errno.EPERM:
        raise SystemExit(f"ptrace unexpectedly reached {pid}: {ctypes.get_errno()}")
    local_value = ctypes.c_long()
    local = IOV(ctypes.addressof(local_value), ctypes.sizeof(local_value))
    remote = IOV(1, ctypes.sizeof(local_value))
    for operation in (libc.process_vm_readv, libc.process_vm_writev):
        ctypes.set_errno(0)
        result = operation(pid, ctypes.byref(local), 1, ctypes.byref(remote), 1, 0)
        if result != -1 or ctypes.get_errno() != errno.EPERM:
            raise SystemExit(f"process-vm unexpectedly reached {pid}: {result}/{ctypes.get_errno()}")
"#;
            let output = std::process::Command::new("/usr/bin/python3")
                .args(["-c", script, &outer, &relay.to_string()])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let prefix = std::env::temp_dir().join(format!(
            "reflex-anti-ptrace-integration-test-{}",
            std::process::id()
        ));
        let executable = std::env::current_exe().unwrap();
        let (capture, _) = capture_child_host_isolated(
            &executable,
            &[
                OsString::from(TEST_NAME),
                OsString::from("--ignored"),
                OsString::from("--exact"),
                OsString::from("--nocapture"),
            ],
            &prefix,
            Duration::from_secs(20),
            HostIsolationPolicy {
                memory_limit_bytes: 512 * 1024 * 1024,
                memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                cpu_reserve: 1,
                experiment_cpu_limit: None,
            },
            &[
                (OsString::from(CHILD_MARKER), OsString::from("1")),
                (
                    OsString::from(OUTER_PID),
                    OsString::from(std::process::id().to_string()),
                ),
            ],
        )
        .expect("the anti-inspection target is supervised");
        assert!(capture.status.success(), "{}", capture.stderr);
        assert!(!capture.boundary.evidence_failed, "{}", capture.stderr);
        assert!(!capture.boundary.cleanup_failed, "{}", capture.stderr);
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and user namespaces"]
    fn trusted_taskset_resolution_ignores_target_path() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory =
            std::env::temp_dir().join(format!("reflex-fake-taskset-test-{}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let marker = directory.join("fake-ran");
        let fake = directory.join("taskset");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\ntouch {}\nexit 91\n", marker.display()),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let prefix = directory.join("evidence");
        let (capture, _) = capture_child_host_isolated(
            std::path::Path::new("sh"),
            &[OsString::from("-c"), OsString::from("exit 0")],
            &prefix,
            Duration::from_secs(20),
            HostIsolationPolicy {
                memory_limit_bytes: 512 * 1024 * 1024,
                memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                cpu_reserve: 1,
                experiment_cpu_limit: None,
            },
            &[(OsString::from("PATH"), directory.clone().into_os_string())],
        )
        .expect("trusted absolute taskset launches despite target PATH");
        assert!(capture.status.success(), "{}", capture.stderr);
        assert!(!marker.exists());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and user namespaces"]
    fn scoped_target_exec_uses_sealed_image_after_source_substitution() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "reflex-target-substitution-test-{}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let target = directory.join("target");
        std::fs::copy("/bin/sh", &target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let replacement = directory.join("replacement");
        let command = format!(
            "printf '#!/bin/sh\\nexit 91\\n' > '{}'; chmod 755 '{}'; mv '{}' '{}'",
            replacement.display(),
            replacement.display(),
            replacement.display(),
            target.display()
        );
        let prefix = directory.join("evidence");
        let (capture, _) = capture_child_host_isolated(
            &target,
            &[OsString::from("-c"), OsString::from(command)],
            &prefix,
            Duration::from_secs(20),
            HostIsolationPolicy {
                memory_limit_bytes: 512 * 1024 * 1024,
                memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                cpu_reserve: 1,
                experiment_cpu_limit: None,
            },
            &[],
        )
        .expect("sealed target remains the exact launched object");
        assert!(capture.status.success(), "{}", capture.stderr);
        assert!(std::fs::read(&target).unwrap().starts_with(b"#!/bin/sh"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and taskset"]
    fn host_isolation_relay_process_for_live_smokes() {
        if std::env::var_os(super::RELAY_REPORT_NONCE).is_some() {
            super::run_host_isolation_relay().expect("the host-isolation relay completes");
        }
    }

    #[test]
    #[ignore = "private host-isolation target shim"]
    fn host_isolation_target_process_for_live_smokes() {
        if std::env::var_os(super::TARGET_READY_PATH).is_some() {
            super::run_host_isolation_target().expect("the target shim execs its target");
        }
    }

    #[test]
    #[ignore = "private host-isolation final exec shim"]
    fn host_isolation_exec_process_for_live_smokes() {
        if std::env::var_os(super::TARGET_EXECUTABLE_IDENTITY).is_some() {
            super::run_host_isolation_exec().expect("the final shim execs its target");
        }
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and taskset"]
    fn host_isolated_parent_consumes_its_live_monitor_handshake() {
        const CHILD_MARKER: &str = "REFLEX_MONITOR_HANDSHAKE_INTEGRATION_CHILD";
        const MONITOR_COMMAND: &str = "REFLEX_MONITOR_HANDSHAKE_TEST_COMMAND";
        const TEST_NAME: &str =
            "harness::tests::host_isolated_parent_consumes_its_live_monitor_handshake";
        const MEMORY_LIMIT: u64 = 512 * 1024 * 1024;
        if std::env::var_os(CHILD_MARKER).is_some() {
            let monitor_command =
                std::env::var(MONITOR_COMMAND).expect("the monitor command identity is present");
            super::verify_live_monitor(monitor_command.as_bytes())
                .expect("the round-trip channels belong to the declared live monitor");
            let expected = std::env::var(super::LARGE_CAMPAIGN_NONCE)
                .expect("the monitor challenge identity is present");
            super::complete_monitor_round_trip(&expected)
                .expect("the live monitor round trip is valid");
            super::verify_live_monitor_process(monitor_command.as_bytes())
                .expect("the declared monitor remains live through the round trip");
            return;
        }

        let prefix = std::env::temp_dir().join(format!(
            "reflex-monitor-handshake-integration-test-{}",
            std::process::id()
        ));
        let executable = std::env::current_exe().expect("test executable exists");
        let monitor_command = std::env::args()
            .nth(1)
            .expect("the live test monitor has a command argument");
        let handshake = [0xa5; super::MONITOR_HANDSHAKE_BYTES];
        let nonce = super::hex(&handshake);
        let (capture, _) = super::capture_child_host_isolated_with_handshake(
            &executable,
            &[
                OsString::from("--exact"),
                OsString::from(TEST_NAME),
                OsString::from("--nocapture"),
                OsString::from("--ignored"),
            ],
            &prefix,
            Duration::from_secs(10),
            HostIsolationPolicy {
                memory_limit_bytes: MEMORY_LIMIT,
                memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                cpu_reserve: 1,
                experiment_cpu_limit: None,
            },
            &[
                (OsString::from(CHILD_MARKER), OsString::from("1")),
                (
                    OsString::from(MONITOR_COMMAND),
                    OsString::from(monitor_command),
                ),
                (OsString::from(LARGE_CAMPAIGN_NONCE), OsString::from(nonce)),
                (
                    OsString::from(super::LARGE_CAMPAIGN_MONITOR_PID),
                    OsString::from(std::process::id().to_string()),
                ),
            ],
            &handshake,
        )
        .expect("host-isolated monitor handshake starts");
        assert!(capture.status.success(), "{}", capture.stderr);
        assert!(!capture.boundary.evidence_failed, "{}", capture.stderr);
        assert!(!capture.boundary.cleanup_failed, "{}", capture.stderr);
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and taskset"]
    fn live_monitor_identity_without_owned_round_trip_channels_is_rejected() {
        const DECOY_MARKER: &str = "REFLEX_DECOY_LIVE_MONITOR_CHILD";
        const TARGET_MARKER: &str = "REFLEX_UNOWNED_MONITOR_CHANNEL_CHILD";
        const TEST_NAME: &str =
            "harness::tests::live_monitor_identity_without_owned_round_trip_channels_is_rejected";
        const MEMORY_LIMIT: u64 = 512 * 1024 * 1024;

        if std::env::var_os(DECOY_MARKER).is_some() {
            std::thread::sleep(Duration::from_secs(10));
            return;
        }
        assert!(
            std::env::var_os(TARGET_MARKER).is_none(),
            "a relay with unowned monitor channels launched its target"
        );

        let executable = std::env::current_exe().expect("test executable exists");
        let mut decoy = std::process::Command::new(&executable)
            .args([TEST_NAME, "--ignored", "--exact", "--nocapture"])
            .env(DECOY_MARKER, "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("decoy monitor starts");
        let challenge = [0xb7; super::MONITOR_HANDSHAKE_BYTES];
        let nonce = super::hex(&challenge);
        let prefix = std::env::temp_dir().join(format!(
            "reflex-unowned-monitor-channel-test-{}",
            std::process::id()
        ));
        let captured = super::capture_child_host_isolated_with_handshake(
            &executable,
            &[
                OsString::from(TEST_NAME),
                OsString::from("--ignored"),
                OsString::from("--exact"),
                OsString::from("--nocapture"),
            ],
            &prefix,
            Duration::from_secs(10),
            HostIsolationPolicy {
                memory_limit_bytes: MEMORY_LIMIT,
                memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                cpu_reserve: 1,
                experiment_cpu_limit: None,
            },
            &[
                (OsString::from(TARGET_MARKER), OsString::from("1")),
                (
                    OsString::from(super::TEST_RELAY_MONITOR_COMMAND),
                    OsString::from(TEST_NAME),
                ),
                (OsString::from(LARGE_CAMPAIGN_NONCE), OsString::from(nonce)),
                (
                    OsString::from(super::LARGE_CAMPAIGN_MONITOR_PID),
                    OsString::from(decoy.id().to_string()),
                ),
            ],
            &challenge,
        );
        let _ = decoy.kill();
        let _ = decoy.wait();
        let error = match captured {
            Err(error) => error,
            Ok((capture, _)) => panic!(
                "unowned monitor channels unexpectedly launched: status={:?} stdout={} stderr={}",
                capture.status, capture.stdout, capture.stderr
            ),
        };
        assert!(
            error.to_string().contains("response")
                || error.to_string().contains("control")
                || error.to_string().contains("evidence"),
            "unexpected channel-ownership failure: {error}"
        );
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and taskset"]
    fn host_isolated_timeout_kills_the_registered_scope() {
        let prefix = std::env::temp_dir().join(format!(
            "reflex-host-isolation-timeout-test-{}",
            std::process::id()
        ));
        let (capture, _) = capture_child_host_isolated(
            std::path::Path::new("sh"),
            &[OsString::from("-c"), OsString::from("sleep 30 & wait")],
            &prefix,
            Duration::from_millis(100),
            HostIsolationPolicy {
                memory_limit_bytes: 512 * 1024 * 1024,
                memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                cpu_reserve: 1,
                experiment_cpu_limit: None,
            },
            &[],
        )
        .expect("host-isolated timeout is classified");
        assert!(
            capture.timed_out,
            "status={:?} stdout={} stderr={}",
            capture.status, capture.stdout, capture.stderr
        );
        assert!(!capture.boundary.evidence_failed);
        assert!(!capture.boundary.cleanup_failed);
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and taskset"]
    fn host_isolated_relay_preserves_signal_termination() {
        use std::os::unix::process::ExitStatusExt as _;

        let prefix = std::env::temp_dir().join(format!(
            "reflex-host-isolation-signal-test-{}",
            std::process::id()
        ));
        let (capture, _) = capture_child_host_isolated(
            std::path::Path::new("sh"),
            &[OsString::from("-c"), OsString::from("kill -TERM $$")],
            &prefix,
            Duration::from_secs(10),
            HostIsolationPolicy {
                memory_limit_bytes: 512 * 1024 * 1024,
                memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                cpu_reserve: 1,
                experiment_cpu_limit: None,
            },
            &[],
        )
        .expect("host-isolated signal termination is classified");
        assert_eq!(capture.status.signal(), Some(15));
        assert!(!capture.boundary.evidence_failed);
        assert!(!capture.boundary.cleanup_failed);
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and taskset"]
    fn host_isolated_relay_preserves_exit_termination() {
        let prefix = std::env::temp_dir().join(format!(
            "reflex-host-isolation-exit-test-{}",
            std::process::id()
        ));
        let (capture, _) = capture_child_host_isolated(
            std::path::Path::new("sh"),
            &[OsString::from("-c"), OsString::from("exit 7")],
            &prefix,
            Duration::from_secs(10),
            HostIsolationPolicy {
                memory_limit_bytes: 512 * 1024 * 1024,
                memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                cpu_reserve: 1,
                experiment_cpu_limit: None,
            },
            &[],
        )
        .expect("host-isolated exit termination is classified");
        assert_eq!(capture.status.code(), Some(7));
        assert!(!capture.boundary.evidence_failed);
        assert!(!capture.boundary.cleanup_failed);
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and taskset"]
    fn authentic_report_with_a_live_descendant_fails_and_cleans_up_the_scope() {
        const CHILD_MARKER: &str = "REFLEX_LIVE_RELAY_DESCENDANT_CHILD";
        const TEST_NAME: &str =
            "harness::tests::authentic_report_with_a_live_descendant_fails_and_cleans_up_the_scope";
        if std::env::var_os(CHILD_MARKER).is_some() {
            let descendant = std::process::Command::new("sleep")
                .arg("30")
                .spawn()
                .expect("a target descendant starts");
            std::mem::forget(descendant);
            return;
        }

        let prefix = std::env::temp_dir().join(format!(
            "reflex-host-isolation-live-descendant-test-{}",
            std::process::id()
        ));
        let executable = std::env::current_exe().unwrap();
        let started = std::time::Instant::now();
        let result = capture_child_host_isolated(
            &executable,
            &[
                OsString::from(TEST_NAME),
                OsString::from("--ignored"),
                OsString::from("--exact"),
                OsString::from("--nocapture"),
            ],
            &prefix,
            Duration::from_secs(5),
            HostIsolationPolicy {
                memory_limit_bytes: 512 * 1024 * 1024,
                memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                cpu_reserve: 1,
                experiment_cpu_limit: None,
            },
            &[(OsString::from(CHILD_MARKER), OsString::from("1"))],
        );
        let error = match result {
            Err(error) => error,
            Ok((capture, _)) => panic!(
                "live descendant unexpectedly accepted: status={:?} stdout={} stderr={}",
                capture.status, capture.stdout, capture.stderr
            ),
        };
        assert!(error.to_string().contains("only terminal scope process"));
        assert!(started.elapsed() < Duration::from_secs(8));
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and taskset"]
    fn untrusted_relay_report_inputs_fail_promptly_and_clean_up_the_scope() {
        const CHILD_MARKER: &str = "REFLEX_INVALID_RELAY_REPORT_CHILD";
        const TEST_NAME: &str =
            "harness::tests::untrusted_relay_report_inputs_fail_promptly_and_clean_up_the_scope";
        if std::env::var_os(CHILD_MARKER).is_some() {
            for name in [
                super::RELAY_REPORT_NONCE,
                super::RELAY_UNIT,
                super::RELAY_TARGET_IDENTITY,
                super::RELAY_TARGET_ARGUMENTS_SHA256,
            ] {
                assert!(std::env::var_os(name).is_none(), "target leaked {name}");
            }
            let mode = std::env::var(super::TEST_RELAY_REPORT_MODE).unwrap();
            match mode.as_str() {
                "forged-then-exit" => std::process::exit(7),
                "forged-then-signal" => {
                    assert!(
                        std::process::Command::new("kill")
                            .args(["-TERM", &std::process::id().to_string()])
                            .status()
                            .unwrap()
                            .success()
                    );
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_secs(30));
            return;
        }

        let executable = std::env::current_exe().unwrap();
        for mode in [
            "partial",
            "malformed",
            "forged-live",
            "forged-then-exit",
            "forged-then-signal",
            "oversize",
        ] {
            let prefix = std::env::temp_dir().join(format!(
                "reflex-host-isolation-{mode}-relay-test-{}",
                std::process::id()
            ));
            let result = capture_child_host_isolated(
                &executable,
                &[
                    OsString::from(TEST_NAME),
                    OsString::from("--ignored"),
                    OsString::from("--exact"),
                    OsString::from("--nocapture"),
                ],
                &prefix,
                Duration::from_secs(5),
                HostIsolationPolicy {
                    memory_limit_bytes: 512 * 1024 * 1024,
                    memory_reserve_bytes: 16 * 1024 * 1024 * 1024,
                    cpu_reserve: 1,
                    experiment_cpu_limit: None,
                },
                &[
                    (OsString::from(CHILD_MARKER), OsString::from("1")),
                    (
                        OsString::from(super::TEST_RELAY_REPORT_MODE),
                        OsString::from(mode),
                    ),
                ],
            );
            let (capture, _) = result.expect("forged frames remain bounded supervision input");
            match mode {
                "forged-then-exit" => {
                    assert_eq!(capture.status.code(), Some(7));
                    assert!(!capture.timed_out);
                }
                "forged-then-signal" => {
                    use std::os::unix::process::ExitStatusExt as _;
                    assert_eq!(capture.status.signal(), Some(15));
                    assert!(!capture.timed_out);
                }
                _ => assert!(capture.timed_out, "{mode}: {}", capture.stderr),
            }
            assert!(
                !capture.boundary.cleanup_failed,
                "{mode}: {}",
                capture.stderr
            );
        }
    }

    #[test]
    #[ignore = "requires a Linux user systemd scope and taskset"]
    fn forged_environment_inside_a_real_cgroup_cannot_launch_a_scientific_child() {
        const CHILD_MARKER: &str = "REFLEX_FORGED_SCIENTIFIC_CHILD";
        assert!(
            std::env::var_os(CHILD_MARKER).is_none(),
            "a forged Campaign environment reached the scientific target"
        );

        let prefix = std::env::temp_dir().join(format!(
            "reflex-forged-scientific-child-test-{}",
            std::process::id()
        ));
        let executable = std::env::current_exe().unwrap();
        let forged_nonce = "cd".repeat(32);
        let result = capture_child_host_isolated(
            &executable,
            &[
                OsString::from("--exact"),
                OsString::from(
                    "harness::tests::forged_environment_inside_a_real_cgroup_cannot_launch_a_scientific_child",
                ),
                OsString::from("--nocapture"),
                OsString::from("--ignored"),
            ],
            &prefix,
            Duration::from_secs(10),
            LargeCampaign::Baseline.isolation_policy(),
            &[
                (OsString::from(CHILD_MARKER), OsString::from("1")),
                (
                    OsString::from(super::LARGE_CAMPAIGN_CAPABILITY),
                    OsString::from("baseline"),
                ),
                (
                    OsString::from(LARGE_CAMPAIGN_NONCE),
                    OsString::from(forged_nonce),
                ),
                (
                    OsString::from(super::LARGE_CAMPAIGN_PARENT_PID),
                    OsString::from(std::process::id().to_string()),
                ),
            ],
        );
        match result {
            Err(error) => assert!(
                error.to_string().contains("monitor")
                    || error.to_string().contains("control")
                    || error.to_string().contains("evidence"),
                "unexpected forged-Campaign error: {error}"
            ),
            Ok((capture, _)) => assert!(
                !capture.status.success()
                    && (capture.stderr.contains("monitor") || capture.boundary.evidence_failed),
                "a forged Campaign unexpectedly launched: status={:?} stdout={} stderr={}",
                capture.status,
                capture.stdout,
                capture.stderr
            ),
        }
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

    #[test]
    fn completed_root_does_not_leave_pipe_holding_descendants() {
        let prefix = std::env::temp_dir().join(format!(
            "reflex-capture-descendant-test-{}",
            std::process::id()
        ));
        let started = std::time::Instant::now();
        let capture = capture_child_bounded(
            std::path::Path::new("sh"),
            &[OsString::from("-c"), OsString::from("sleep 30 & exit 0")],
            &prefix,
            Duration::from_secs(3),
            u64::MAX,
            &[],
        )
        .expect("root completion cleans up its pipe-holding descendants");
        assert!(capture.status.success());
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn escaped_output_holder_cannot_block_a_reader_join() {
        let (reader, writer) = std::os::unix::net::UnixStream::pair().unwrap();
        let exceeded = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let reader = super::spawn_bounded_reader(reader, exceeded).unwrap();
        let started = std::time::Instant::now();
        let error = super::join_bounded_reader(reader, "escaped-holder test")
            .expect_err("an escaped writer keeps EOF absent but cannot block the supervisor");
        assert!(error.to_string().contains("did not terminate"));
        assert!(started.elapsed() < Duration::from_secs(3));
        drop(writer);
    }
}
