use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::harness::{
    AnyError, capture_child_bounded, duration_ns, hash_json, hex, parse_flag_values,
};

const SCHEMA: &str = "reflex-directional-receipt-v4";
const DEFAULT_RECEIPT: &str = "target/directional/receipt.json";
const MAX_TOTAL_TIMEOUT_SECONDS: u64 = 990;
const OUTPUT_TAIL_BYTES: usize = 16 * 1024;
const GIBIBYTE: u64 = 1024 * 1024 * 1024;
const TINY_LEAN_TEST: &str = "pinned_worker_indexes_and_kernel_checks_a_seed";

const REQUIRED_GATE_NAMES: [&str; 12] = [
    "strict-fmt",
    "reflex-unit-tests",
    "bitvec-unit-tests",
    "bitvec-directional-harness",
    "checkpoint-recovery",
    "knowledge-consolidation",
    "current-knowledge-recovery",
    "v20-bundle-migration",
    "xtask-unit-tests",
    "strict-clippy-default",
    "strict-clippy",
    "tiny-lean-kernel-smoke",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum GateStatus {
    Passed,
    Failed,
    Blocked,
    NotRun,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum RunStatus {
    Passed,
    Failed,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct RepositoryState {
    revision: String,
    dirty: bool,
    source_snapshot_sha256: String,
    source_entries: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the receipt records independent bounded-run failure evidence explicitly"
)]
struct GateReceipt {
    name: String,
    command: Vec<String>,
    timeout_seconds: u64,
    resident_limit_bytes: u64,
    status: GateStatus,
    wall_ns: u64,
    process_tree_cpu_ns: u64,
    peak_process_tree_resident_bytes: u64,
    exit_code: Option<i32>,
    timed_out: bool,
    resident_limit_exceeded: bool,
    stdout_tail: String,
    stderr_tail: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    output_limit_exceeded: bool,
    detail: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct DirectionalReceipt {
    schema: String,
    repository: RepositoryState,
    execution_source_snapshot_sha256: String,
    gate_plan_sha256: String,
    started_unix_ms: u64,
    total_wall_ns: u64,
    maximum_total_timeout_seconds: u64,
    status: RunStatus,
    gates: Vec<GateReceipt>,
    content_sha256: String,
}

#[derive(Debug)]
struct GateSpec {
    name: &'static str,
    arguments: Vec<OsString>,
    environment: Vec<(OsString, OsString)>,
    timeout: Duration,
    resident_limit_bytes: u64,
    blocker: Option<String>,
}

#[derive(Debug, Serialize)]
struct CanonicalGatePlan {
    maximum_total_timeout_seconds: u64,
    gates: Vec<CanonicalGateSpec>,
}

#[derive(Debug, Serialize)]
struct CanonicalGateSpec {
    name: String,
    command: Vec<String>,
    environment: Vec<(String, String)>,
    timeout_seconds: u64,
    resident_limit_bytes: u64,
    blocker: Option<String>,
}

#[derive(Debug)]
struct LeanFixture {
    lake: PathBuf,
    mathlib: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SourceSnapshot {
    sha256: String,
    entries: u64,
}

#[expect(
    clippy::too_many_lines,
    reason = "the fixed Directional protocol remains contiguous and auditable"
)]
pub(super) fn run(arguments: &[String]) -> Result<(), AnyError> {
    let root = repository_root()?;
    let output = parse_output(arguments, &root)?;
    let cargo = cargo_program();
    let started_unix_ms = unix_time_ms()?;
    let started = Instant::now();
    let repository = repository_state(&root)?;
    let source_paths = source_paths(&root)?;
    let expected_snapshot = SourceSnapshot {
        sha256: repository.source_snapshot_sha256.clone(),
        entries: repository.source_entries,
    };
    let execution_root = materialize_source_snapshot(&root, &expected_snapshot)?;
    let work = root.join("target/directional/work");
    std::fs::create_dir_all(&work)?;

    let planned = planned_gates(&execution_root, discover_lean_fixture());
    validate_plan_bound(&planned)?;
    let gate_plan_sha256 = gate_plan_sha256(&cargo, &planned)?;
    let mut halted = false;
    let mut gates = Vec::new();
    for gate in planned {
        let remaining =
            Duration::from_secs(MAX_TOTAL_TIMEOUT_SECONDS).saturating_sub(started.elapsed());
        let mut receipt = if let Some(detail) = gate.blocker.as_ref() {
            blocked_receipt(&cargo, &gate, detail)
        } else if halted {
            not_run_receipt(&cargo, &gate)
        } else if remaining.is_zero() {
            global_timeout_receipt(&cargo, &gate)
        } else {
            execute_gate(&cargo, &gate, &work, gate.timeout.min(remaining))
        };
        match repository_state(&root) {
            Ok(current) => mark_source_mutation(&mut receipt, &repository, &current),
            Err(error) => {
                receipt.status = GateStatus::Failed;
                receipt.detail = Some(format!(
                    "could not re-snapshot source state after the gate: {error}"
                ));
            }
        }
        match source_snapshot_for_paths(&execution_root, &source_paths) {
            Ok(current) if current == expected_snapshot => {}
            Ok(current) => mark_execution_copy_mutation(&mut receipt, &expected_snapshot, &current),
            Err(error) => {
                receipt.status = GateStatus::Failed;
                receipt.detail = Some(format!(
                    "could not verify immutable source copy after the gate: {error}"
                ));
            }
        }
        halted |= receipt.status != GateStatus::Passed;
        gates.push(receipt);
    }

    if let Some(last) = gates.last_mut() {
        match repository_state(&root) {
            Ok(current) => mark_source_mutation(last, &repository, &current),
            Err(error) => {
                last.status = GateStatus::Failed;
                last.detail = Some(format!(
                    "could not perform the final source-state snapshot: {error}"
                ));
            }
        }
        match source_snapshot_for_paths(&execution_root, &source_paths) {
            Ok(current) if current == expected_snapshot => {}
            Ok(current) => mark_execution_copy_mutation(last, &expected_snapshot, &current),
            Err(error) => {
                last.status = GateStatus::Failed;
                last.detail = Some(format!(
                    "could not perform the final immutable-copy snapshot: {error}"
                ));
            }
        }
    }

    let total_wall = started.elapsed();
    let status = if total_wall > Duration::from_secs(MAX_TOTAL_TIMEOUT_SECONDS) {
        RunStatus::Failed
    } else {
        overall_status(&gates)
    };
    let mut receipt = DirectionalReceipt {
        schema: SCHEMA.to_owned(),
        repository,
        execution_source_snapshot_sha256: expected_snapshot.sha256,
        gate_plan_sha256,
        started_unix_ms,
        total_wall_ns: duration_ns(total_wall),
        maximum_total_timeout_seconds: MAX_TOTAL_TIMEOUT_SECONDS,
        status,
        gates,
        content_sha256: String::new(),
    };
    receipt.content_sha256 = hash_json(&receipt)?;
    write_receipt(&output, &receipt)?;
    println!("{}", serde_json::to_string_pretty(&receipt)?);

    match status {
        RunStatus::Passed => Ok(()),
        RunStatus::Failed => Err(format!(
            "directional confidence gates failed; receipt: {}",
            output.display()
        )
        .into()),
        RunStatus::Blocked => Err(format!(
            "directional confidence gates are blocked; receipt: {}",
            output.display()
        )
        .into()),
    }
}

fn mark_execution_copy_mutation(
    receipt: &mut GateReceipt,
    expected: &SourceSnapshot,
    current: &SourceSnapshot,
) {
    if current != expected {
        receipt.status = GateStatus::Failed;
        receipt.detail = Some(format!(
            "immutable Directional source copy changed: expected {}, observed {}",
            expected.sha256, current.sha256
        ));
    }
}

fn mark_source_mutation(
    receipt: &mut GateReceipt,
    initial: &RepositoryState,
    current: &RepositoryState,
) {
    if current != initial {
        receipt.status = GateStatus::Failed;
        receipt.detail = Some(format!(
            "source tree changed during Directional gates: initial {}, observed {}",
            initial.source_snapshot_sha256, current.source_snapshot_sha256
        ));
    }
}

fn parse_output(arguments: &[String], root: &Path) -> Result<PathBuf, AnyError> {
    let values = parse_flag_values(arguments, &["--output"], "directional")?;
    let requested = values
        .get("--output")
        .map_or_else(|| PathBuf::from(DEFAULT_RECEIPT), PathBuf::from);
    Ok(if requested.is_absolute() {
        requested
    } else {
        root.join(requested)
    })
}

fn repository_root() -> Result<PathBuf, AnyError> {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| "xtask manifest has no repository parent".into())
}

fn cargo_program() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"))
}

fn validate_plan_bound(gates: &[GateSpec]) -> Result<(), AnyError> {
    let names = gates.iter().map(|gate| gate.name).collect::<Vec<_>>();
    if names != REQUIRED_GATE_NAMES
        || gates
            .iter()
            .any(|gate| gate.timeout.is_zero() || gate.resident_limit_bytes == 0)
    {
        return Err("Directional gate plan is not the exact nonzero required plan".into());
    }
    let planned_seconds = gates
        .iter()
        .try_fold(0_u64, |total, gate| {
            total.checked_add(gate.timeout.as_secs())
        })
        .ok_or("Directional gate timeout sum overflowed")?;
    if planned_seconds > MAX_TOTAL_TIMEOUT_SECONDS {
        return Err(format!(
            "Directional gate plan permits {planned_seconds} seconds, exceeding the {MAX_TOTAL_TIMEOUT_SECONDS}-second global bound"
        )
        .into());
    }
    Ok(())
}

fn gate_plan_sha256(cargo: &OsStr, gates: &[GateSpec]) -> Result<String, AnyError> {
    hash_json(&canonical_gate_plan(cargo, gates))
}

fn canonical_gate_plan(cargo: &OsStr, gates: &[GateSpec]) -> CanonicalGatePlan {
    CanonicalGatePlan {
        maximum_total_timeout_seconds: MAX_TOTAL_TIMEOUT_SECONDS,
        gates: gates
            .iter()
            .map(|gate| CanonicalGateSpec {
                name: gate.name.to_owned(),
                command: display_command(cargo, &gate.arguments),
                environment: gate
                    .environment
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.to_string_lossy().into_owned(),
                            value.to_string_lossy().into_owned(),
                        )
                    })
                    .collect(),
                timeout_seconds: gate.timeout.as_secs(),
                resident_limit_bytes: gate.resident_limit_bytes,
                blocker: gate.blocker.clone(),
            })
            .collect(),
    }
}

fn repository_state(root: &Path) -> Result<RepositoryState, AnyError> {
    let revision = git_output(root, &["rev-parse", "HEAD"])?;
    let changes = git_output(root, &["status", "--porcelain", "--untracked-files=normal"])?;
    let snapshot = source_snapshot(root)?;
    Ok(RepositoryState {
        revision,
        dirty: !changes.is_empty(),
        source_snapshot_sha256: snapshot.sha256,
        source_entries: snapshot.entries,
    })
}

fn git_output(root: &Path, arguments: &[&str]) -> Result<String, AnyError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed with {}: {}",
            arguments.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn source_snapshot(root: &Path) -> Result<SourceSnapshot, AnyError> {
    let paths = source_paths(root)?;
    source_snapshot_for_paths(root, &paths)
}

fn source_paths(root: &Path) -> Result<Vec<Vec<u8>>, AnyError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git source enumeration failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let mut paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    paths.sort_unstable();
    paths.dedup();
    Ok(paths)
}

fn source_snapshot_for_paths(root: &Path, paths: &[Vec<u8>]) -> Result<SourceSnapshot, AnyError> {
    let mut digest = Sha256::new();
    digest.update(b"reflex-directional-source-snapshot-v1\0");
    digest.update((paths.len() as u64).to_le_bytes());
    for encoded_path in paths {
        digest.update((encoded_path.len() as u64).to_le_bytes());
        digest.update(encoded_path);
        let relative = path_from_git_bytes(encoded_path)?;
        let absolute = root.join(relative);
        match std::fs::symlink_metadata(&absolute) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                digest.update(b"120000");
                let target = git_bytes_from_path(&std::fs::read_link(&absolute)?)?;
                digest.update((target.len() as u64).to_le_bytes());
                digest.update(target);
            }
            Ok(metadata) if metadata.is_file() => {
                digest.update(source_file_mode(&metadata));
                let contents = std::fs::read(&absolute)?;
                digest.update((contents.len() as u64).to_le_bytes());
                digest.update(contents);
            }
            Ok(_) => {
                return Err(format!(
                    "source snapshot cannot canonically encode non-file path {}",
                    absolute.display()
                )
                .into());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A cached path absent from the working tree is a deletion,
                // not an omission from the snapshot.
                digest.update(b"deleted");
                digest.update(0_u64.to_le_bytes());
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(SourceSnapshot {
        sha256: hex(&digest.finalize()),
        entries: u64::try_from(paths.len()).unwrap_or(u64::MAX),
    })
}

fn directional_source_root(root: &Path, snapshot_sha256: &str) -> PathBuf {
    root.join("target/directional/sources")
        .join(snapshot_sha256)
}

fn materialize_source_snapshot(
    root: &Path,
    expected: &SourceSnapshot,
) -> Result<PathBuf, AnyError> {
    let paths = source_paths(root)?;
    let current = source_snapshot_for_paths(root, &paths)?;
    if &current != expected {
        return Err("source changed before its immutable Directional copy was materialized".into());
    }
    let destination = directional_source_root(root, &expected.sha256);
    if destination.exists() {
        let copied = source_snapshot_for_paths(&destination, &paths)?;
        if copied != *expected {
            return Err(format!(
                "immutable Directional source copy differs at {}",
                destination.display()
            )
            .into());
        }
        return Ok(destination);
    }
    let parent = destination
        .parent()
        .ok_or("Directional source-copy path has no parent")?;
    std::fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".source-{}-{}.tmp",
        expected.sha256,
        std::process::id()
    ));
    std::fs::create_dir(&staging)?;
    let copy = (|| -> Result<(), AnyError> {
        for encoded_path in &paths {
            let relative = path_from_git_bytes(encoded_path)?;
            let source = root.join(&relative);
            let target = staging.join(&relative);
            match std::fs::symlink_metadata(&source) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(format!(
                        "immutable Directional execution does not follow source symlink {}",
                        source.display()
                    )
                    .into());
                }
                Ok(metadata) if metadata.is_file() => {
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::copy(&source, &target)?;
                    set_immutable_file_mode(&target, &metadata)?;
                }
                Ok(_) => {
                    return Err(format!(
                        "immutable Directional source is not a regular file: {}",
                        source.display()
                    )
                    .into());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        std::fs::create_dir(staging.join("target"))?;
        let copied = source_snapshot_for_paths(&staging, &paths)?;
        if copied != *expected {
            return Err("immutable Directional source copy failed identity verification".into());
        }
        freeze_source_directories(&staging)?;
        std::fs::rename(&staging, &destination)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if copy.is_err() {
        let _ = make_tree_writable(&staging);
        let _ = std::fs::remove_dir_all(&staging);
    }
    copy?;
    Ok(destination)
}

#[cfg(unix)]
fn set_immutable_file_mode(path: &Path, source: &std::fs::Metadata) -> Result<(), AnyError> {
    use std::os::unix::fs::PermissionsExt as _;
    let mode = if source.permissions().mode() & 0o111 == 0 {
        0o444
    } else {
        0o555
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_immutable_file_mode(path: &Path, _source: &std::fs::Metadata) -> Result<(), AnyError> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(true);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(unix)]
fn freeze_source_directories(root: &Path) -> Result<(), AnyError> {
    use std::os::unix::fs::PermissionsExt as _;
    fn visit(path: &Path, target: &Path) -> Result<(), AnyError> {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            if entry.path() == target {
                continue;
            }
            if entry.file_type()?.is_dir() {
                visit(&entry.path(), target)?;
            }
        }
        if path != target {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o555))?;
        }
        Ok(())
    }
    visit(root, &root.join("target"))
}

#[cfg(not(unix))]
fn freeze_source_directories(_root: &Path) -> Result<(), AnyError> {
    Ok(())
}

#[cfg(unix)]
fn make_tree_writable(root: &Path) -> Result<(), AnyError> {
    use std::os::unix::fs::PermissionsExt as _;
    if !root.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            make_tree_writable(&entry.path())?;
        }
    }
    std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o755))?;
    Ok(())
}

#[cfg(not(unix))]
fn make_tree_writable(_root: &Path) -> Result<(), AnyError> {
    Ok(())
}

#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the cross-platform Git path decoder is fallible on non-Unix targets"
)]
fn path_from_git_bytes(bytes: &[u8]) -> Result<PathBuf, AnyError> {
    use std::os::unix::ffi::OsStringExt as _;
    Ok(PathBuf::from(OsString::from_vec(bytes.to_vec())))
}

#[cfg(not(unix))]
fn path_from_git_bytes(bytes: &[u8]) -> Result<PathBuf, AnyError> {
    Ok(PathBuf::from(String::from_utf8(bytes.to_vec())?))
}

#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the cross-platform symlink-target encoder is fallible on non-Unix targets"
)]
fn git_bytes_from_path(path: &Path) -> Result<Vec<u8>, AnyError> {
    use std::os::unix::ffi::OsStrExt as _;
    Ok(path.as_os_str().as_bytes().to_vec())
}

#[cfg(not(unix))]
fn git_bytes_from_path(path: &Path) -> Result<Vec<u8>, AnyError> {
    Ok(path
        .to_str()
        .ok_or("source path is not canonical UTF-8")?
        .as_bytes()
        .to_vec())
}

#[cfg(unix)]
fn source_file_mode(metadata: &std::fs::Metadata) -> &'static [u8] {
    use std::os::unix::fs::PermissionsExt as _;
    if metadata.permissions().mode() & 0o111 == 0 {
        b"100644"
    } else {
        b"100755"
    }
}

#[cfg(not(unix))]
fn source_file_mode(_metadata: &std::fs::Metadata) -> &'static [u8] {
    b"100644"
}

fn planned_gates(root: &Path, lean: Result<LeanFixture, String>) -> Vec<GateSpec> {
    let manifest = root.join("Cargo.toml");
    let manifest = manifest.as_os_str().to_owned();
    let common_environment = cargo_environment();
    let (lean_environment, lean_blocker) = match lean {
        Ok(fixture) => (
            common_environment
                .iter()
                .cloned()
                .chain([
                    (OsString::from("RUST_TEST_THREADS"), OsString::from("1")),
                    (
                        OsString::from("REFLEX_LEAN_LAKE"),
                        fixture.lake.into_os_string(),
                    ),
                    (
                        OsString::from("REFLEX_LEAN_MATHLIB"),
                        fixture.mathlib.into_os_string(),
                    ),
                ])
                .collect(),
            None,
        ),
        Err(blocker) => (common_environment.clone(), Some(blocker)),
    };

    let mut gates = standard_gates(&manifest, &common_environment);
    gates.push(lean_gate(&manifest, lean_environment, lean_blocker));
    gates
}

#[expect(
    clippy::too_many_lines,
    reason = "the complete bounded confidence plan remains contiguous and auditable"
)]
fn standard_gates(manifest: &OsStr, common_environment: &[(OsString, OsString)]) -> Vec<GateSpec> {
    vec![
        GateSpec {
            name: "strict-fmt",
            arguments: os_arguments([
                OsStr::new("fmt"),
                OsStr::new("--manifest-path"),
                manifest,
                OsStr::new("--all"),
                OsStr::new("--"),
                OsStr::new("--check"),
            ]),
            environment: common_environment.to_vec(),
            timeout: Duration::from_secs(30),
            resident_limit_bytes: 2 * GIBIBYTE,
            blocker: None,
        },
        GateSpec {
            name: "reflex-unit-tests",
            arguments: os_arguments([
                OsStr::new("test"),
                OsStr::new("--manifest-path"),
                manifest,
                OsStr::new("-p"),
                OsStr::new("reflex"),
                OsStr::new("--lib"),
                OsStr::new("--all-features"),
            ]),
            environment: common_environment.to_vec(),
            timeout: Duration::from_secs(90),
            resident_limit_bytes: 8 * GIBIBYTE,
            blocker: None,
        },
        GateSpec {
            name: "bitvec-unit-tests",
            arguments: os_arguments([
                OsStr::new("test"),
                OsStr::new("--manifest-path"),
                manifest,
                OsStr::new("-p"),
                OsStr::new("reflex-bitvec"),
                OsStr::new("--lib"),
                OsStr::new("--"),
                OsStr::new("--test-threads=1"),
            ]),
            environment: common_environment.to_vec(),
            timeout: Duration::from_secs(30),
            resident_limit_bytes: 8 * GIBIBYTE,
            blocker: None,
        },
        GateSpec {
            name: "bitvec-directional-harness",
            arguments: os_arguments([
                OsStr::new("test"),
                OsStr::new("--manifest-path"),
                manifest,
                OsStr::new("-p"),
                OsStr::new("reflex-bitvec"),
                OsStr::new("--test"),
                OsStr::new("directional_harness"),
            ]),
            environment: common_environment.to_vec(),
            timeout: Duration::from_secs(90),
            resident_limit_bytes: 8 * GIBIBYTE,
            blocker: None,
        },
        bitvec_integration_gate(
            "checkpoint-recovery",
            "checkpoint_recovery",
            manifest,
            common_environment,
            Duration::from_secs(45),
        ),
        bitvec_integration_gate(
            "knowledge-consolidation",
            "knowledge_consolidation",
            manifest,
            common_environment,
            Duration::from_secs(45),
        ),
        bitvec_integration_gate(
            "current-knowledge-recovery",
            "current_knowledge_recovery",
            manifest,
            common_environment,
            Duration::from_secs(30),
        ),
        bitvec_integration_gate(
            "v20-bundle-migration",
            "v20_bundle_migration",
            manifest,
            common_environment,
            Duration::from_secs(30),
        ),
        GateSpec {
            name: "xtask-unit-tests",
            arguments: os_arguments([
                OsStr::new("test"),
                OsStr::new("--manifest-path"),
                manifest,
                OsStr::new("-p"),
                OsStr::new("xtask"),
                OsStr::new("--"),
                OsStr::new("--test-threads=1"),
            ]),
            environment: common_environment.to_vec(),
            // This gate is deliberately serial: several repository tests use
            // process CPU as an authority and parallel test work would be
            // charged to the wrong experiment. Consumed-corpus framing checks
            // make the serial wall bound larger than the parallel fast path.
            timeout: Duration::from_mins(2),
            resident_limit_bytes: 8 * GIBIBYTE,
            blocker: None,
        },
        GateSpec {
            name: "strict-clippy-default",
            arguments: os_arguments([
                OsStr::new("clippy"),
                OsStr::new("--manifest-path"),
                manifest,
                OsStr::new("-p"),
                OsStr::new("reflex"),
                OsStr::new("--all-targets"),
                OsStr::new("--no-default-features"),
                OsStr::new("--"),
                OsStr::new("-D"),
                OsStr::new("warnings"),
            ]),
            environment: common_environment.to_vec(),
            timeout: Duration::from_mins(2),
            resident_limit_bytes: 8 * GIBIBYTE,
            blocker: None,
        },
        GateSpec {
            name: "strict-clippy",
            arguments: os_arguments([
                OsStr::new("clippy"),
                OsStr::new("--manifest-path"),
                manifest,
                OsStr::new("--workspace"),
                OsStr::new("--all-targets"),
                OsStr::new("--all-features"),
                OsStr::new("--"),
                OsStr::new("-D"),
                OsStr::new("warnings"),
            ]),
            environment: common_environment.to_vec(),
            timeout: Duration::from_mins(3),
            resident_limit_bytes: 12 * GIBIBYTE,
            blocker: None,
        },
    ]
}

fn bitvec_integration_gate(
    name: &'static str,
    target: &str,
    manifest: &OsStr,
    environment: &[(OsString, OsString)],
    timeout: Duration,
) -> GateSpec {
    GateSpec {
        name,
        arguments: vec![
            OsString::from("test"),
            OsString::from("--manifest-path"),
            manifest.to_owned(),
            OsString::from("-p"),
            OsString::from("reflex-bitvec"),
            OsString::from("--test"),
            OsString::from(target),
            OsString::from("--"),
            OsString::from("--test-threads=1"),
        ],
        environment: environment.to_vec(),
        timeout,
        resident_limit_bytes: 8 * GIBIBYTE,
        blocker: None,
    }
}

fn lean_gate(
    manifest: &OsStr,
    environment: Vec<(OsString, OsString)>,
    blocker: Option<String>,
) -> GateSpec {
    GateSpec {
        name: "tiny-lean-kernel-smoke",
        arguments: os_arguments([
            OsStr::new("test"),
            OsStr::new("--manifest-path"),
            manifest,
            OsStr::new("-p"),
            OsStr::new("reflex-lean"),
            OsStr::new("--test"),
            OsStr::new("worker_smoke"),
            OsStr::new("--"),
            OsStr::new("--ignored"),
            OsStr::new("--exact"),
            OsStr::new(TINY_LEAN_TEST),
            OsStr::new("--nocapture"),
        ]),
        environment,
        timeout: Duration::from_mins(3),
        resident_limit_bytes: 20 * GIBIBYTE,
        blocker,
    }
}

fn os_arguments<const N: usize>(arguments: [&OsStr; N]) -> Vec<OsString> {
    arguments.into_iter().map(OsStr::to_owned).collect()
}

fn cargo_environment() -> Vec<(OsString, OsString)> {
    let jobs = std::thread::available_parallelism()
        .map_or(1, std::num::NonZeroUsize::get)
        .saturating_sub(1)
        .max(1);
    vec![
        (
            OsString::from("CARGO_BUILD_JOBS"),
            OsString::from(jobs.to_string()),
        ),
        (OsString::from("CARGO_TERM_COLOR"), OsString::from("never")),
    ]
}

fn discover_lean_fixture() -> Result<LeanFixture, String> {
    let configured_lake = std::env::var_os("REFLEX_LEAN_LAKE");
    let configured_mathlib = std::env::var_os("REFLEX_LEAN_MATHLIB");
    match (configured_lake, configured_mathlib) {
        (Some(lake), Some(mathlib)) => validate_lean_fixture(lake.into(), mathlib.into()),
        (Some(_), None) | (None, Some(_)) => {
            Err("tiny Lean smoke requires REFLEX_LEAN_LAKE and REFLEX_LEAN_MATHLIB together".into())
        }
        (None, None) => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| "tiny Lean smoke is blocked: HOME is unavailable".to_owned())?;
            validate_lean_fixture(
                home.join(".elan/bin/lake"),
                home.join(format!(
                    ".cache/reflex/mathlib4-{}",
                    reflex_lean::MATHLIB_COMMIT
                )),
            )
        }
    }
}

fn validate_lean_fixture(lake: PathBuf, mathlib: PathBuf) -> Result<LeanFixture, String> {
    if !lake.is_file() {
        return Err(format!(
            "tiny Lean smoke is blocked: lake executable fixture is absent at {}",
            lake.display()
        ));
    }
    if !mathlib.is_dir() {
        return Err(format!(
            "tiny Lean smoke is blocked: pinned mathlib fixture is absent at {}",
            mathlib.display()
        ));
    }
    Ok(LeanFixture { lake, mathlib })
}

fn execute_gate(
    cargo: &OsStr,
    gate: &GateSpec,
    work: &Path,
    effective_timeout: Duration,
) -> GateReceipt {
    let started = Instant::now();
    let captured = capture_child_bounded(
        Path::new(cargo),
        &gate.arguments,
        &work.join(gate.name),
        effective_timeout,
        gate.resident_limit_bytes,
        &gate.environment,
    );
    match captured {
        Ok(capture) => {
            let (stdout_tail, stdout_tail_truncated) = output_tail(&capture.stdout);
            let (stderr_tail, stderr_tail_truncated) = output_tail(&capture.stderr);
            let status = if capture.status.success()
                && !capture.timed_out
                && !capture.resident_limit_exceeded
                && !capture.output_limit_exceeded
            {
                GateStatus::Passed
            } else {
                GateStatus::Failed
            };
            let detail = failure_detail(
                status,
                capture.status.code(),
                capture.timed_out,
                capture.resident_limit_exceeded,
                capture.output_limit_exceeded,
                gate,
            );
            GateReceipt {
                name: gate.name.to_owned(),
                command: display_command(cargo, &gate.arguments),
                timeout_seconds: gate.timeout.as_secs(),
                resident_limit_bytes: gate.resident_limit_bytes,
                status,
                wall_ns: duration_ns(started.elapsed()),
                process_tree_cpu_ns: capture.process_tree_cpu_ns,
                peak_process_tree_resident_bytes: capture.peak_process_tree_resident_bytes,
                exit_code: capture.status.code(),
                timed_out: capture.timed_out,
                resident_limit_exceeded: capture.resident_limit_exceeded,
                stdout_tail,
                stderr_tail,
                stdout_truncated: capture.stdout_truncated || stdout_tail_truncated,
                stderr_truncated: capture.stderr_truncated || stderr_tail_truncated,
                output_limit_exceeded: capture.output_limit_exceeded,
                detail,
            }
        }
        Err(error) => GateReceipt {
            name: gate.name.to_owned(),
            command: display_command(cargo, &gate.arguments),
            timeout_seconds: gate.timeout.as_secs(),
            resident_limit_bytes: gate.resident_limit_bytes,
            status: GateStatus::Failed,
            wall_ns: duration_ns(started.elapsed()),
            process_tree_cpu_ns: 0,
            peak_process_tree_resident_bytes: 0,
            exit_code: None,
            timed_out: false,
            resident_limit_exceeded: false,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            output_limit_exceeded: false,
            detail: Some(format!("could not execute bounded gate: {error}")),
        },
    }
}

fn failure_detail(
    status: GateStatus,
    exit_code: Option<i32>,
    timed_out: bool,
    resident_limit_exceeded: bool,
    output_limit_exceeded: bool,
    gate: &GateSpec,
) -> Option<String> {
    if status == GateStatus::Passed {
        None
    } else if timed_out {
        Some(format!(
            "exceeded the hard {} second timeout",
            gate.timeout.as_secs()
        ))
    } else if resident_limit_exceeded {
        Some(format!(
            "exceeded the hard {} byte resident limit",
            gate.resident_limit_bytes
        ))
    } else if output_limit_exceeded {
        Some("exceeded the bounded diagnostic-output allowance".to_owned())
    } else {
        Some(exit_code.map_or_else(
            || "subprocess terminated without an exit code".to_owned(),
            |code| format!("subprocess exited with code {code}"),
        ))
    }
}

fn blocked_receipt(cargo: &OsStr, gate: &GateSpec, detail: &str) -> GateReceipt {
    empty_receipt(cargo, gate, GateStatus::Blocked, Some(detail.to_owned()))
}

fn not_run_receipt(cargo: &OsStr, gate: &GateSpec) -> GateReceipt {
    empty_receipt(
        cargo,
        gate,
        GateStatus::NotRun,
        Some("not run because an earlier confidence gate did not pass".into()),
    )
}

fn global_timeout_receipt(cargo: &OsStr, gate: &GateSpec) -> GateReceipt {
    let mut receipt = empty_receipt(
        cargo,
        gate,
        GateStatus::Failed,
        Some(format!(
            "the {MAX_TOTAL_TIMEOUT_SECONDS}-second Directional Harness deadline expired before this gate"
        )),
    );
    receipt.timed_out = true;
    receipt
}

fn empty_receipt(
    cargo: &OsStr,
    gate: &GateSpec,
    status: GateStatus,
    detail: Option<String>,
) -> GateReceipt {
    GateReceipt {
        name: gate.name.to_owned(),
        command: display_command(cargo, &gate.arguments),
        timeout_seconds: gate.timeout.as_secs(),
        resident_limit_bytes: gate.resident_limit_bytes,
        status,
        wall_ns: 0,
        process_tree_cpu_ns: 0,
        peak_process_tree_resident_bytes: 0,
        exit_code: None,
        timed_out: false,
        resident_limit_exceeded: false,
        stdout_tail: String::new(),
        stderr_tail: String::new(),
        stdout_truncated: false,
        stderr_truncated: false,
        output_limit_exceeded: false,
        detail,
    }
}

fn display_command(cargo: &OsStr, arguments: &[OsString]) -> Vec<String> {
    std::iter::once(cargo)
        .chain(arguments.iter().map(OsString::as_os_str))
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect()
}

fn output_tail(output: &str) -> (String, bool) {
    if output.len() <= OUTPUT_TAIL_BYTES {
        return (output.to_owned(), false);
    }
    let mut start = output.len() - OUTPUT_TAIL_BYTES;
    while !output.is_char_boundary(start) {
        start += 1;
    }
    (
        format!("[earlier output elided]\n{}", &output[start..]),
        true,
    )
}

fn overall_status(gates: &[GateReceipt]) -> RunStatus {
    if gates.iter().any(|gate| gate.status == GateStatus::Failed) {
        RunStatus::Failed
    } else if gates.iter().any(|gate| gate.status == GateStatus::Blocked) {
        RunStatus::Blocked
    } else {
        RunStatus::Passed
    }
}

fn write_receipt(path: &Path, receipt: &DirectionalReceipt) -> Result<(), AnyError> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("receipt path has no parent: {}", path.display()))?;
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".directional-receipt-{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec_pretty(receipt)?;
    let mut file = std::fs::File::create(&temporary)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)?;
    std::fs::File::open(parent)?.sync_all()?;
    Ok(())
}

pub(super) fn require_current_passed_receipt() -> Result<(), AnyError> {
    let root = repository_root()?;
    let cargo = cargo_program();
    let current = repository_state(&root)?;
    let execution_root = directional_source_root(&root, &current.source_snapshot_sha256);
    let gates = planned_gates(&execution_root, discover_lean_fixture());
    verify_passed_receipt(&root, &root.join(DEFAULT_RECEIPT), &cargo, &gates)
}

fn verify_passed_receipt(
    root: &Path,
    path: &Path,
    cargo: &OsStr,
    expected_gates: &[GateSpec],
) -> Result<(), AnyError> {
    validate_plan_bound(expected_gates)?;
    let bytes = std::fs::read(path).map_err(|error| {
        format!(
            "a passed Directional Harness receipt is required at {}: {error}",
            path.display()
        )
    })?;
    let receipt = serde_json::from_slice::<DirectionalReceipt>(&bytes).map_err(|error| {
        format!(
            "Directional Harness receipt at {} is not canonical v4 data: {error}",
            path.display()
        )
    })?;
    if receipt.schema != SCHEMA {
        return Err(format!(
            "Directional Harness receipt schema mismatch: expected {SCHEMA}, found {}",
            receipt.schema
        )
        .into());
    }
    if receipt.status != RunStatus::Passed {
        return Err("Directional Harness receipt did not pass".into());
    }
    if receipt.execution_source_snapshot_sha256 != receipt.repository.source_snapshot_sha256 {
        return Err("Directional receipt does not bind its immutable execution source".into());
    }
    if receipt.maximum_total_timeout_seconds != MAX_TOTAL_TIMEOUT_SECONDS
        || receipt.total_wall_ns > duration_ns(Duration::from_secs(MAX_TOTAL_TIMEOUT_SECONDS))
    {
        return Err("Directional Harness receipt violates its exact total timeout bound".into());
    }
    let expected_plan_sha256 = gate_plan_sha256(cargo, expected_gates)?;
    if receipt.gate_plan_sha256 != expected_plan_sha256 {
        return Err("Directional Harness receipt gate-plan identity is stale or invalid".into());
    }
    let gate_names = receipt
        .gates
        .iter()
        .map(|gate| gate.name.as_str())
        .collect::<Vec<_>>();
    if gate_names != REQUIRED_GATE_NAMES {
        return Err(
            "Directional Harness receipt does not contain the exact required gate plan".into(),
        );
    }
    if receipt.gates.iter().any(|gate| {
        gate.status != GateStatus::Passed
            || gate.exit_code != Some(0)
            || gate.timed_out
            || gate.resident_limit_exceeded
            || gate.output_limit_exceeded
            || gate.detail.is_some()
            || gate.peak_process_tree_resident_bytes > gate.resident_limit_bytes
    }) {
        return Err("Directional Harness receipt contains an inconsistent successful gate".into());
    }
    for (receipt_gate, expected) in receipt.gates.iter().zip(expected_gates) {
        if receipt_gate.command != display_command(cargo, &expected.arguments)
            || receipt_gate.timeout_seconds != expected.timeout.as_secs()
            || receipt_gate.resident_limit_bytes != expected.resident_limit_bytes
        {
            return Err(
                "Directional Harness receipt does not match the exact required gate plan".into(),
            );
        }
    }

    let mut unhashed = receipt.clone();
    let claimed_hash = std::mem::take(&mut unhashed.content_sha256);
    if claimed_hash.is_empty() || hash_json(&unhashed)? != claimed_hash {
        return Err("Directional Harness receipt content hash is invalid".into());
    }
    let current = repository_state(root)?;
    if receipt.repository != current {
        return Err(format!(
            "Directional Harness receipt is stale: recorded source snapshot {}, current source snapshot {}",
            receipt.repository.source_snapshot_sha256, current.source_snapshot_sha256
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
fn launch_with_receipt<T>(
    root: &Path,
    receipt: &Path,
    launch: impl FnOnce() -> Result<T, AnyError>,
) -> Result<T, AnyError> {
    let (cargo, gates) = test_gate_plan(root);
    verify_passed_receipt(root, receipt, &cargo, &gates)?;
    launch()
}

#[cfg(test)]
fn write_passed_receipt_for_test(root: &Path, path: &Path) -> Result<(), AnyError> {
    let (cargo, planned) = test_gate_plan(root);
    let gates = planned
        .iter()
        .map(|gate| GateReceipt {
            name: gate.name.to_owned(),
            command: display_command(&cargo, &gate.arguments),
            timeout_seconds: gate.timeout.as_secs(),
            resident_limit_bytes: gate.resident_limit_bytes,
            status: GateStatus::Passed,
            wall_ns: 1,
            process_tree_cpu_ns: 1,
            peak_process_tree_resident_bytes: 1,
            exit_code: Some(0),
            timed_out: false,
            resident_limit_exceeded: false,
            stdout_tail: String::new(),
            stderr_tail: String::new(),
            stdout_truncated: false,
            stderr_truncated: false,
            output_limit_exceeded: false,
            detail: None,
        })
        .collect();
    let repository = repository_state(root)?;
    let mut receipt = DirectionalReceipt {
        schema: SCHEMA.to_owned(),
        execution_source_snapshot_sha256: repository.source_snapshot_sha256.clone(),
        repository,
        gate_plan_sha256: gate_plan_sha256(&cargo, &planned)?,
        started_unix_ms: 1,
        total_wall_ns: 1,
        maximum_total_timeout_seconds: MAX_TOTAL_TIMEOUT_SECONDS,
        status: RunStatus::Passed,
        gates,
        content_sha256: String::new(),
    };
    receipt.content_sha256 = hash_json(&receipt)?;
    write_receipt(path, &receipt)
}

#[cfg(test)]
fn test_gate_plan(root: &Path) -> (OsString, Vec<GateSpec>) {
    let cargo = OsString::from("test-cargo");
    let state = repository_state(root).expect("test repository state exists");
    let execution_root = directional_source_root(root, &state.source_snapshot_sha256);
    let gates = planned_gates(
        &execution_root,
        Ok(LeanFixture {
            lake: PathBuf::from("/test-fixture/lake"),
            mathlib: PathBuf::from("/test-fixture/mathlib"),
        }),
    );
    (cargo, gates)
}

fn unix_time_ms() -> Result<u64, AnyError> {
    let millis = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    Ok(u64::try_from(millis).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ffi::OsStr;
    #[cfg(unix)]
    use std::os::unix::fs::{PermissionsExt as _, symlink};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    use super::{
        DEFAULT_RECEIPT, GateStatus, LeanFixture, MAX_TOTAL_TIMEOUT_SECONDS, RunStatus,
        output_tail, parse_output, planned_gates,
    };

    struct TestRepository(PathBuf);

    impl TestRepository {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "reflex-directional-{label}-{}-{}",
                std::process::id(),
                super::unix_time_ms().unwrap()
            ));
            std::fs::create_dir_all(&path).unwrap();
            let status = Command::new("git")
                .args(["init", "-q"])
                .current_dir(&path)
                .status()
                .unwrap();
            assert!(status.success());
            std::fs::write(path.join(".gitignore"), "/target/\n").unwrap();
            std::fs::write(path.join("tracked.rs"), "fn baseline() {}\n").unwrap();
            let status = Command::new("git")
                .args(["add", ".gitignore", "tracked.rs"])
                .current_dir(&path)
                .status()
                .unwrap();
            assert!(status.success());
            let status = Command::new("git")
                .args([
                    "-c",
                    "user.name=Reflex Test",
                    "-c",
                    "user.email=reflex@example.invalid",
                    "commit",
                    "-q",
                    "-m",
                    "fixture",
                ])
                .current_dir(&path)
                .status()
                .unwrap();
            assert!(status.success());
            Self(path)
        }

        fn receipt(&self) -> PathBuf {
            self.0.join("target/directional/receipt.json")
        }

        fn verify_receipt(&self, path: &Path) -> Result<(), super::AnyError> {
            let (cargo, gates) = super::test_gate_plan(&self.0);
            super::verify_passed_receipt(&self.0, path, &cargo, &gates)
        }
    }

    impl Drop for TestRepository {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn arguments_only_select_the_receipt_destination() {
        let root = Path::new("/repo");
        assert_eq!(
            parse_output(&[], root).expect("default output is valid"),
            root.join(DEFAULT_RECEIPT)
        );
        assert_eq!(
            parse_output(&["--output".into(), "elsewhere.json".into()], root)
                .expect("explicit output is valid"),
            root.join("elsewhere.json")
        );
        assert!(parse_output(&["--timeout".into(), "999".into()], root).is_err());
    }

    #[test]
    fn gate_plan_is_bounded_and_contains_no_large_campaign() {
        let gates = planned_gates(
            Path::new("/repo"),
            Ok(LeanFixture {
                lake: PathBuf::from("/fixture/lake"),
                mathlib: PathBuf::from("/fixture/mathlib"),
            }),
        );
        assert_eq!(
            gates.iter().map(|gate| gate.name).collect::<Vec<_>>(),
            [
                "strict-fmt",
                "reflex-unit-tests",
                "bitvec-unit-tests",
                "bitvec-directional-harness",
                "checkpoint-recovery",
                "knowledge-consolidation",
                "current-knowledge-recovery",
                "v20-bundle-migration",
                "xtask-unit-tests",
                "strict-clippy-default",
                "strict-clippy",
                "tiny-lean-kernel-smoke",
            ]
        );
        let timeout_seconds = gates.iter().map(|gate| gate.timeout.as_secs()).sum::<u64>();
        assert_eq!(timeout_seconds, MAX_TOTAL_TIMEOUT_SECONDS);
        assert!(super::validate_plan_bound(&gates).is_ok());
        assert!(gates.iter().all(|gate| gate.timeout.as_secs() > 0));

        let mut over_bound = planned_gates(
            Path::new("/repo"),
            Ok(LeanFixture {
                lake: PathBuf::from("/fixture/lake"),
                mathlib: PathBuf::from("/fixture/mathlib"),
            }),
        );
        over_bound[0].timeout = over_bound[0].timeout.saturating_add(Duration::from_secs(1));
        assert!(super::validate_plan_bound(&over_bound).is_err());

        let command = gates
            .iter()
            .flat_map(|gate| gate.arguments.iter())
            .map(|argument| argument.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ");
        for forbidden in ["baseline", "causal-confirm", "development", "runtime_smoke"] {
            assert!(
                !command.contains(forbidden),
                "forbidden campaign: {forbidden}"
            );
        }
    }

    #[test]
    fn unavailable_lean_fixture_is_an_explicit_blocked_gate() {
        let gates = planned_gates(Path::new("/repo"), Err("fixture absent".into()));
        let lean = gates.last().expect("Lean gate is present");
        assert_eq!(lean.name, "tiny-lean-kernel-smoke");
        assert_eq!(lean.blocker.as_deref(), Some("fixture absent"));
        assert_eq!(
            super::blocked_receipt(OsStr::new("cargo"), lean, "fixture absent").status,
            GateStatus::Blocked
        );
    }

    #[test]
    fn receipt_status_prefers_failure_and_preserves_blocking() {
        let gate = planned_gates(Path::new("/repo"), Err("fixture absent".into()))
            .pop()
            .expect("Lean gate is present");
        let blocked = super::blocked_receipt(OsStr::new("cargo"), &gate, "fixture absent");
        assert_eq!(super::overall_status(&[blocked]), RunStatus::Blocked);

        let mut failed = super::not_run_receipt(OsStr::new("cargo"), &gate);
        failed.status = GateStatus::Failed;
        let blocked = super::blocked_receipt(OsStr::new("cargo"), &gate, "fixture absent");
        assert_eq!(super::overall_status(&[failed, blocked]), RunStatus::Failed);
    }

    #[test]
    fn receipt_output_is_utf8_safe_and_bounded() {
        let output = format!("{}é", "x".repeat(super::OUTPUT_TAIL_BYTES));
        let (tail, truncated) = output_tail(&output);
        assert!(truncated);
        assert!(tail.starts_with("[earlier output elided]\n"));
        assert!(tail.ends_with('é'));
        assert!(tail.len() <= super::OUTPUT_TAIL_BYTES + 32);

        assert_eq!(
            output_tail("complete output"),
            ("complete output".into(), false)
        );
    }

    #[test]
    fn absent_and_stale_receipts_block_before_the_campaign_launches() {
        let repository = TestRepository::new("receipt-guard");
        let receipt = repository.receipt();
        let launches = Cell::new(0_u8);
        assert!(
            super::launch_with_receipt(&repository.0, &receipt, || {
                launches.set(launches.get() + 1);
                Ok(())
            })
            .is_err()
        );
        assert_eq!(launches.get(), 0);

        super::write_passed_receipt_for_test(&repository.0, &receipt).unwrap();
        super::launch_with_receipt(&repository.0, &receipt, || {
            launches.set(launches.get() + 1);
            Ok(())
        })
        .unwrap();
        assert_eq!(launches.get(), 1);

        std::fs::write(repository.0.join("tracked.rs"), "fn changed() {}\n").unwrap();
        assert!(
            super::launch_with_receipt(&repository.0, &receipt, || {
                launches.set(launches.get() + 1);
                Ok(())
            })
            .is_err()
        );
        assert_eq!(launches.get(), 1);
    }

    #[test]
    fn every_source_tree_mutation_invalidates_the_passed_receipt() {
        fn assert_blocked(label: &str, mutate: impl FnOnce(&Path)) {
            let repository = TestRepository::new(label);
            let receipt = repository.receipt();
            super::write_passed_receipt_for_test(&repository.0, &receipt).unwrap();
            mutate(&repository.0);
            let launches = Cell::new(0_u8);
            assert!(
                super::launch_with_receipt(&repository.0, &receipt, || {
                    launches.set(1);
                    Ok(())
                })
                .is_err()
            );
            assert_eq!(launches.get(), 0);
        }

        assert_blocked("contents", |root| {
            std::fs::write(root.join("tracked.rs"), "fn changed() {}\n").unwrap();
        });
        assert_blocked("deletion", |root| {
            std::fs::remove_file(root.join("tracked.rs")).unwrap();
        });
        assert_blocked("untracked", |root| {
            std::fs::write(root.join("new.rs"), "fn untracked() {}\n").unwrap();
        });
        #[cfg(unix)]
        assert_blocked("mode", |root| {
            let path = root.join("tracked.rs");
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).unwrap();
        });
        #[cfg(unix)]
        assert_blocked("symlink", |root| {
            symlink("tracked.rs", root.join("source-link")).unwrap();
        });
    }

    #[test]
    fn source_mutation_during_a_gate_prevents_a_passed_receipt() {
        let repository = TestRepository::new("mid-gate-mutation");
        let initial = super::repository_state(&repository.0).unwrap();
        let (cargo, gates) = super::test_gate_plan(&repository.0);
        let mut receipt = super::empty_receipt(&cargo, &gates[0], GateStatus::Passed, None);
        receipt.exit_code = Some(0);
        std::fs::write(
            repository.0.join("tracked.rs"),
            "fn changed_during_gate() {}\n",
        )
        .unwrap();
        let current = super::repository_state(&repository.0).unwrap();
        super::mark_source_mutation(&mut receipt, &initial, &current);
        assert_eq!(receipt.status, GateStatus::Failed);
        assert!(receipt.detail.unwrap().contains("source tree changed"));
    }

    #[test]
    fn gates_consume_an_immutable_content_addressed_source_copy() {
        let repository = TestRepository::new("immutable-source-copy");
        let initial = super::source_snapshot(&repository.0).unwrap();
        let execution = super::materialize_source_snapshot(&repository.0, &initial).unwrap();
        let copied = execution.join("tracked.rs");
        assert_eq!(
            std::fs::read_to_string(&copied).unwrap(),
            "fn baseline() {}\n"
        );

        std::fs::write(
            repository.0.join("tracked.rs"),
            "fn transient_change() {}\n",
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&copied).unwrap(),
            "fn baseline() {}\n"
        );
        std::fs::write(repository.0.join("tracked.rs"), "fn baseline() {}\n").unwrap();
        assert_eq!(
            super::source_snapshot_for_paths(
                &execution,
                &super::source_paths(&repository.0).unwrap()
            )
            .unwrap(),
            initial
        );
        #[cfg(unix)]
        assert!(std::fs::write(&copied, "fn mutation() {}\n").is_err());

        super::make_tree_writable(&execution).unwrap();
        std::fs::remove_dir_all(execution).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn immutable_gate_copy_rejects_source_symlinks() {
        let repository = TestRepository::new("immutable-source-symlink");
        symlink("tracked.rs", repository.0.join("source-link")).unwrap();
        let initial = super::source_snapshot(&repository.0).unwrap();
        let error = super::materialize_source_snapshot(&repository.0, &initial)
            .expect_err("gate execution must not follow mutable source links");
        assert!(error.to_string().contains("source symlink"));
    }

    #[test]
    fn failed_or_content_tampered_receipts_cannot_authorize_a_launch() {
        let repository = TestRepository::new("receipt-integrity");
        let receipt_path = repository.receipt();
        super::write_passed_receipt_for_test(&repository.0, &receipt_path).unwrap();
        let mut receipt = serde_json::from_slice::<super::DirectionalReceipt>(
            &std::fs::read(&receipt_path).unwrap(),
        )
        .unwrap();
        receipt.gates[0].wall_ns = receipt.gates[0].wall_ns.saturating_add(1);
        std::fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
        assert!(repository.verify_receipt(&receipt_path).is_err());

        super::write_passed_receipt_for_test(&repository.0, &receipt_path).unwrap();
        let mut receipt = serde_json::from_slice::<super::DirectionalReceipt>(
            &std::fs::read(&receipt_path).unwrap(),
        )
        .unwrap();
        receipt.status = RunStatus::Failed;
        receipt.content_sha256.clear();
        receipt.content_sha256 = super::hash_json(&receipt).unwrap();
        std::fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
        assert!(repository.verify_receipt(&receipt_path).is_err());
    }

    #[test]
    fn rehashed_gate_plan_tampering_cannot_authorize_a_launch() {
        let repository = TestRepository::new("receipt-plan-integrity");
        let receipt_path = repository.receipt();

        for mutate in [
            |receipt: &mut super::DirectionalReceipt| {
                receipt.gates[0].command = vec!["true".to_owned()];
            },
            |receipt: &mut super::DirectionalReceipt| {
                receipt.gates[0].resident_limit_bytes = u64::MAX;
            },
        ] {
            super::write_passed_receipt_for_test(&repository.0, &receipt_path).unwrap();
            let mut receipt = serde_json::from_slice::<super::DirectionalReceipt>(
                &std::fs::read(&receipt_path).unwrap(),
            )
            .unwrap();
            mutate(&mut receipt);
            receipt.content_sha256.clear();
            receipt.content_sha256 = super::hash_json(&receipt).unwrap();
            std::fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
            assert!(repository.verify_receipt(&receipt_path).is_err());
        }
    }

    #[test]
    fn rehashed_inconsistent_success_and_total_bound_cannot_authorize_a_launch() {
        let repository = TestRepository::new("receipt-success-integrity");
        let receipt_path = repository.receipt();

        for mutate in [
            |receipt: &mut super::DirectionalReceipt| {
                receipt.gates[0].exit_code = Some(9);
            },
            |receipt: &mut super::DirectionalReceipt| {
                receipt.gates[0].timed_out = true;
            },
            |receipt: &mut super::DirectionalReceipt| {
                receipt.gates[0].resident_limit_exceeded = true;
            },
            |receipt: &mut super::DirectionalReceipt| {
                receipt.maximum_total_timeout_seconds = u64::MAX;
            },
            |receipt: &mut super::DirectionalReceipt| {
                receipt.execution_source_snapshot_sha256 = "different-source".into();
            },
        ] {
            super::write_passed_receipt_for_test(&repository.0, &receipt_path).unwrap();
            let mut receipt = serde_json::from_slice::<super::DirectionalReceipt>(
                &std::fs::read(&receipt_path).unwrap(),
            )
            .unwrap();
            mutate(&mut receipt);
            receipt.content_sha256.clear();
            receipt.content_sha256 = super::hash_json(&receipt).unwrap();
            std::fs::write(&receipt_path, serde_json::to_vec_pretty(&receipt).unwrap()).unwrap();
            assert!(repository.verify_receipt(&receipt_path).is_err());
        }
    }
}
