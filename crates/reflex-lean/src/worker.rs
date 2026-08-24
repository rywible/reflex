use std::io::{BufRead, BufReader, BufWriter, Write};
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ast::{LeanEnvironmentIdentity, LeanExpr, LeanName};
use crate::{ARTIFACT_FORMAT_VERSION, KERNEL_CONTRACT_VERSION, LeanSnapshotPin};

pub const PROTOCOL_VERSION: usize = 1;
pub const DEFAULT_WORKER_RESIDENT_BYTES: u64 = 16 * 1024 * 1024 * 1024;
pub const MINIMUM_WORKER_RESIDENT_BYTES: u64 = 12 * 1024 * 1024 * 1024;
const MAX_ITEMS_PER_TRANSACTION: usize = 1;

#[derive(Debug)]
pub enum WorkerError {
    Io(std::io::Error),
    Protocol(String),
    Json(serde_json::Error),
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "Lean worker I/O failed: {error}"),
            Self::Protocol(error) => write!(formatter, "Lean worker protocol failed: {error}"),
            Self::Json(error) => write!(formatter, "Lean worker JSON failed: {error}"),
        }
    }
}

impl std::error::Error for WorkerError {}

impl From<std::io::Error> for WorkerError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for WorkerError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Clone, Debug)]
pub struct LeanWorkerConfig {
    pub lake_executable: PathBuf,
    pub mathlib_root: PathBuf,
    pub worker_source: PathBuf,
    pub resident_bytes: NonZeroU64,
    pub snapshot: LeanSnapshotPin,
}

impl LeanWorkerConfig {
    pub fn pinned(lake_executable: impl Into<PathBuf>, mathlib_root: impl Into<PathBuf>) -> Self {
        Self {
            lake_executable: lake_executable.into(),
            mathlib_root: mathlib_root.into(),
            worker_source: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("worker/ReflexLeanWorker.lean"),
            resident_bytes: NonZeroU64::new(DEFAULT_WORKER_RESIDENT_BYTES)
                .unwrap_or(NonZeroU64::MIN),
            snapshot: LeanSnapshotPin::final_pre_2025(),
        }
    }

    #[must_use]
    pub fn for_snapshot(
        lake_executable: impl Into<PathBuf>,
        mathlib_root: impl Into<PathBuf>,
        snapshot: LeanSnapshotPin,
    ) -> Self {
        Self {
            snapshot,
            ..Self::pinned(lake_executable, mathlib_root)
        }
    }

    pub fn validate(&self) -> Result<(), WorkerError> {
        for (description, path) in [
            ("lake executable", self.lake_executable.as_path()),
            ("mathlib root", self.mathlib_root.as_path()),
            ("worker source", self.worker_source.as_path()),
        ] {
            if !path.exists() {
                return Err(WorkerError::Protocol(format!(
                    "{description} does not exist: {}",
                    path.display()
                )));
            }
        }
        if self.resident_bytes.get() < MINIMUM_WORKER_RESIDENT_BYTES {
            return Err(WorkerError::Protocol(format!(
                "Lean worker resident allowance is below the hard {MINIMUM_WORKER_RESIDENT_BYTES} byte minimum"
            )));
        }
        validate_snapshot_pin(&self.snapshot)?;
        verify_git_checkout(&self.mathlib_root, &self.snapshot.mathlib_commit, "mathlib")?;
        let selected_toolchain = std::fs::read_to_string(self.mathlib_root.join("lean-toolchain"))?;
        if selected_toolchain.trim() != self.snapshot.lean_toolchain {
            return Err(WorkerError::Protocol(format!(
                "mathlib selects Lean toolchain {}, expected {}",
                selected_toolchain.trim(),
                self.snapshot.lean_toolchain
            )));
        }
        verify_manifest_dependencies(&self.mathlib_root)?;
        run_checked(
            &self.lake_executable,
            &["build", "Mathlib"],
            &self.mathlib_root,
            "pinned Mathlib build validation",
        )?;
        let lean_commit = run_checked(
            &self.lake_executable,
            &["env", "lean", "--githash"],
            &self.mathlib_root,
            "Lean compiler identity",
        )?;
        if lean_commit.trim() != self.snapshot.lean_commit {
            return Err(WorkerError::Protocol(format!(
                "Lean compiler is {}, expected {}",
                lean_commit.trim(),
                self.snapshot.lean_commit
            )));
        }
        let lean_version = run_checked(
            &self.lake_executable,
            &["env", "lean", "--version"],
            &self.mathlib_root,
            "Lean compiler version",
        )?;
        if !lean_version.contains(&format!("version {},", self.snapshot.lean_version)) {
            return Err(WorkerError::Protocol(format!(
                "Lean compiler version differs from {}: {}",
                self.snapshot.lean_version,
                lean_version.trim(),
            )));
        }
        #[cfg(not(target_os = "linux"))]
        return Err(WorkerError::Protocol(
            "external Lean workers currently require Linux prlimit for a hard memory bound".into(),
        ));
        #[cfg(target_os = "linux")]
        run_checked(
            Path::new("prlimit"),
            &["--version"],
            &self.mathlib_root,
            "Linux worker resource limiter",
        )?;
        Ok(())
    }

    pub fn environment_identity(&self) -> Result<LeanEnvironmentIdentity, WorkerError> {
        let worker_source = std::fs::read(&self.worker_source)?;
        Ok(LeanEnvironmentIdentity {
            mathlib_commit: self.snapshot.mathlib_commit.clone(),
            lean_toolchain: self.snapshot.lean_toolchain.clone(),
            lean_commit: self.snapshot.lean_commit.clone(),
            artifact_format: ARTIFACT_FORMAT_VERSION,
            kernel_contract: KERNEL_CONTRACT_VERSION,
            worker_source_sha256: hex(&Sha256::digest(worker_source)),
        })
    }

    fn lean_invocation(&self) -> Result<LeanInvocation, WorkerError> {
        let value = |name| {
            run_checked(
                &self.lake_executable,
                &["env", "printenv", name],
                &self.mathlib_root,
                &format!("Lake environment variable {name}"),
            )
            .map(|value| value.trim_end().to_owned())
        };
        let executable = PathBuf::from(
            run_checked(
                &self.lake_executable,
                &["env", "which", "lean"],
                &self.mathlib_root,
                "Lake Lean executable resolution",
            )?
            .trim_end(),
        );
        if !executable.is_file() {
            return Err(WorkerError::Protocol(format!(
                "Lake resolved a missing Lean executable: {}",
                executable.display()
            )));
        }
        Ok(LeanInvocation {
            executable,
            lean_path: value("LEAN_PATH")?,
            lean_src_path: value("LEAN_SRC_PATH")?,
            library_path: value("LD_LIBRARY_PATH")?,
            sysroot: value("LEAN_SYSROOT")?,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedTheorem {
    pub name: LeanName,
    pub level_params: Vec<LeanName>,
    pub proposition: LeanExpr,
    pub proof_term: LeanExpr,
    pub dependencies: Vec<LeanName>,
    pub axioms: Vec<LeanName>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationItem {
    pub level_params: Vec<LeanName>,
    pub claim_proposition: LeanExpr,
    pub candidate_proposition: LeanExpr,
    pub proof_term: LeanExpr,
    pub allowed_axioms: Vec<LeanName>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationResult {
    pub accepted: bool,
    pub dependencies: Vec<LeanName>,
    pub axioms: Vec<LeanName>,
    pub diagnostic: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TheoremFingerprint {
    pub name: LeanName,
    pub module_name: LeanName,
    pub statement_hash: String,
    pub dependencies: Vec<LeanName>,
    pub kind: String,
    pub locally_eligible: bool,
}

#[derive(Clone, Debug)]
pub struct IndexPage {
    pub total: usize,
    pub offset: usize,
    pub artifacts: Vec<IndexedTheorem>,
}

#[derive(Clone, Debug)]
pub struct FingerprintPage {
    pub total: usize,
    pub offset: usize,
    pub fingerprints: Vec<TheoremFingerprint>,
}

#[derive(Clone, Copy, Debug)]
pub struct WorkerUsage {
    pub elapsed: Duration,
    pub cpu_upper_bound: Duration,
    pub resident_upper_bound: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
enum Request<'a> {
    Ping {
        id: String,
    },
    Verify {
        id: String,
        items: &'a [VerificationItem],
    },
    Index {
        id: String,
        offset: usize,
        limit: usize,
    },
    Fingerprints {
        id: String,
        offset: usize,
        limit: usize,
    },
    Fetch {
        id: String,
        names: &'a [LeanName],
    },
    Shutdown {
        id: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum Response {
    Ready {
        handshake: Handshake,
    },
    Pong {
        id: String,
    },
    Verified {
        id: String,
        results: Vec<VerificationResult>,
    },
    Indexed {
        id: String,
        total: usize,
        offset: usize,
        artifacts: Vec<IndexedTheorem>,
    },
    Fingerprinted {
        id: String,
        total: usize,
        offset: usize,
        fingerprints: Vec<TheoremFingerprint>,
    },
    Fetched {
        id: String,
        artifacts: Vec<IndexedTheorem>,
    },
    Stopped {
        id: String,
    },
    Failed {
        id: String,
        diagnostic: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Handshake {
    protocol_version: usize,
    lean_version: String,
    lean_commit: String,
    trust_level: usize,
}

struct Process {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<ChildStdout>,
    next_id: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LakeManifest {
    packages_dir: PathBuf,
    packages: Vec<LakePackage>,
}

#[derive(Deserialize)]
struct LakePackage {
    name: String,
    #[serde(rename = "type")]
    source_type: String,
    rev: String,
}

struct LeanInvocation {
    executable: PathBuf,
    lean_path: String,
    lean_src_path: String,
    library_path: String,
    sysroot: String,
}

impl Process {
    fn transact(&mut self, request: &Request<'_>) -> Result<Response, WorkerError> {
        serde_json::to_writer(&mut self.input, request)?;
        self.input.write_all(b"\n")?;
        self.input.flush()?;
        let mut line = String::new();
        if self.output.read_line(&mut line)? == 0 {
            return Err(WorkerError::Protocol(format!(
                "worker exited before responding: {:?}",
                self.child.try_wait()?
            )));
        }
        decode_json(&line)
    }

    fn transact_bounded(
        &mut self,
        request: &Request<'_>,
        deadline: Duration,
    ) -> Result<Response, WorkerError> {
        let pid = self.child.id();
        let (finished, completion) = mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if completion.recv_timeout(deadline).is_err() {
                terminate_process(pid);
            }
        });
        let response = self.transact(request);
        let _ = finished.send(());
        watchdog
            .join()
            .map_err(|_| WorkerError::Protocol("worker watchdog panicked".into()))?;
        response
    }

    fn id(&mut self) -> String {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        id.to_string()
    }
}

pub struct LeanWorker {
    process: Mutex<Process>,
    resident_bytes: NonZeroU64,
    environment: LeanEnvironmentIdentity,
}

impl LeanWorker {
    pub fn start(config: &LeanWorkerConfig) -> Result<Self, WorkerError> {
        Self::start_internal(config, None)
    }

    pub fn start_bounded(
        config: &LeanWorkerConfig,
        deadline: Duration,
    ) -> Result<Self, WorkerError> {
        Self::start_internal(config, Some(deadline))
    }

    fn start_internal(
        config: &LeanWorkerConfig,
        deadline: Option<Duration>,
    ) -> Result<Self, WorkerError> {
        config.validate()?;
        let invocation = config.lean_invocation()?;
        let worker_root = config
            .worker_source
            .parent()
            .ok_or_else(|| WorkerError::Protocol("worker source has no parent directory".into()))?;
        #[cfg(target_os = "linux")]
        let mut command = {
            let mut command = Command::new("prlimit");
            command
                .arg(format!("--as={}", config.resident_bytes.get()))
                .arg("--")
                .arg(&invocation.executable);
            command
        };
        #[cfg(not(target_os = "linux"))]
        let mut command = Command::new(&invocation.executable);
        command
            .args(["--trust=0", "--threads=1", "-DwarningAsError=true"])
            .arg(format!("--root={}", worker_root.display()))
            .arg("--run")
            .arg(&config.worker_source)
            .current_dir(&config.mathlib_root)
            .env("LEAN_PATH", &invocation.lean_path)
            .env("LEAN_SRC_PATH", &invocation.lean_src_path)
            .env("LD_LIBRARY_PATH", &invocation.library_path)
            .env("LEAN_SYSROOT", &invocation.sysroot)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            command.process_group(0);
        }
        let mut child = command.spawn()?;
        let input = child
            .stdin
            .take()
            .ok_or_else(|| WorkerError::Protocol("worker did not expose standard input".into()))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| WorkerError::Protocol("worker did not expose standard output".into()))?;
        let mut process = Process {
            child,
            input: BufWriter::new(input),
            output: BufReader::new(output),
            next_id: 1,
        };
        let (finished, watchdog) = deadline.map_or((None, None), |deadline| {
            let pid = process.child.id();
            let (finished, completion) = mpsc::channel();
            let watchdog = std::thread::spawn(move || {
                if completion.recv_timeout(deadline).is_err() {
                    terminate_process(pid);
                }
            });
            (Some(finished), Some(watchdog))
        });
        let mut line = String::new();
        let read = process.output.read_line(&mut line);
        if let Some(finished) = finished {
            let _ = finished.send(());
        }
        if let Some(watchdog) = watchdog {
            watchdog
                .join()
                .map_err(|_| WorkerError::Protocol("startup watchdog panicked".into()))?;
        }
        if read? == 0 {
            return Err(WorkerError::Protocol(
                "worker exited before its handshake".into(),
            ));
        }
        let Response::Ready { handshake } = decode_json(&line)? else {
            return Err(WorkerError::Protocol(
                "worker's first frame was not a handshake".into(),
            ));
        };
        if handshake.protocol_version != PROTOCOL_VERSION
            || handshake.lean_version != config.snapshot.lean_runtime_version
            || handshake.lean_commit != config.snapshot.lean_commit
            || handshake.trust_level != 0
        {
            return Err(WorkerError::Protocol(
                "worker reported an incompatible environment or kernel pin".into(),
            ));
        }
        let environment = config.environment_identity()?;
        Ok(Self {
            process: Mutex::new(process),
            resident_bytes: config.resident_bytes,
            environment,
        })
    }

    #[must_use]
    pub fn environment(&self) -> &LeanEnvironmentIdentity {
        &self.environment
    }

    #[must_use]
    pub fn worker_lanes(&self) -> NonZeroUsize {
        NonZeroUsize::MIN
    }

    #[must_use]
    pub fn resident_bytes(&self) -> NonZeroU64 {
        self.resident_bytes
    }

    pub fn ping(&self) -> Result<WorkerUsage, WorkerError> {
        let started = Instant::now();
        let mut process = self.lock()?;
        let id = process.id();
        match process.transact(&Request::Ping { id: id.clone() })? {
            Response::Pong { id: response_id } if response_id == id => {
                Ok(self.usage(started.elapsed()))
            }
            response => Err(unexpected("pong", response)),
        }
    }

    pub fn verify(
        &self,
        items: &[VerificationItem],
    ) -> Result<(Vec<VerificationResult>, WorkerUsage), WorkerError> {
        let started = Instant::now();
        let mut process = self.lock()?;
        let mut combined = Vec::with_capacity(items.len());
        for items in items.chunks(MAX_ITEMS_PER_TRANSACTION) {
            let id = process.id();
            match process.transact(&Request::Verify {
                id: id.clone(),
                items,
            })? {
                Response::Verified {
                    id: response_id,
                    results,
                } if response_id == id && results.len() == items.len() => {
                    combined.extend(results);
                }
                response => return Err(unexpected("verified page", response)),
            }
        }
        Ok((combined, self.usage(started.elapsed())))
    }

    pub fn verify_bounded(
        &self,
        items: &[VerificationItem],
        deadline: Duration,
    ) -> Result<(Vec<VerificationResult>, WorkerUsage), WorkerError> {
        let started = Instant::now();
        let mut process = self.lock()?;
        let mut combined = Vec::with_capacity(items.len());
        for items in items.chunks(MAX_ITEMS_PER_TRANSACTION) {
            let id = process.id();
            let remaining = deadline.saturating_sub(started.elapsed());
            match process.transact_bounded(
                &Request::Verify {
                    id: id.clone(),
                    items,
                },
                remaining,
            )? {
                Response::Verified {
                    id: response_id,
                    results,
                } if response_id == id && results.len() == items.len() => {
                    combined.extend(results);
                }
                response => return Err(unexpected("bounded verified page", response)),
            }
        }
        Ok((combined, self.usage(started.elapsed())))
    }

    pub fn index_page(&self, offset: usize, limit: usize) -> Result<IndexPage, WorkerError> {
        let mut process = self.lock()?;
        let id = process.id();
        match process.transact(&Request::Index {
            id: id.clone(),
            offset,
            limit,
        })? {
            Response::Indexed {
                id: response_id,
                total,
                offset: response_offset,
                artifacts,
            } if response_id == id && response_offset == offset => Ok(IndexPage {
                total,
                offset,
                artifacts,
            }),
            response => Err(unexpected("index page", response)),
        }
    }

    pub fn fingerprint_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<FingerprintPage, WorkerError> {
        let mut process = self.lock()?;
        let id = process.id();
        match process.transact(&Request::Fingerprints {
            id: id.clone(),
            offset,
            limit,
        })? {
            Response::Fingerprinted {
                id: response_id,
                total,
                offset: response_offset,
                fingerprints,
            } if response_id == id && response_offset == offset => Ok(FingerprintPage {
                total,
                offset,
                fingerprints,
            }),
            response => Err(unexpected("theorem fingerprint page", response)),
        }
    }

    pub fn fetch(&self, names: &[LeanName]) -> Result<Vec<IndexedTheorem>, WorkerError> {
        let mut process = self.lock()?;
        let mut combined = Vec::with_capacity(names.len());
        for names in names.chunks(MAX_ITEMS_PER_TRANSACTION) {
            let id = process.id();
            match process.transact(&Request::Fetch {
                id: id.clone(),
                names,
            })? {
                Response::Fetched {
                    id: response_id,
                    artifacts,
                } if response_id == id && artifacts.len() == names.len() => {
                    combined.extend(artifacts);
                }
                response => return Err(unexpected("fetched theorem page", response)),
            }
        }
        Ok(combined)
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Process>, WorkerError> {
        self.process
            .lock()
            .map_err(|_| WorkerError::Protocol("worker mutex was poisoned".into()))
    }

    fn usage(&self, elapsed: Duration) -> WorkerUsage {
        WorkerUsage {
            elapsed,
            cpu_upper_bound: elapsed,
            resident_upper_bound: self.resident_bytes.get(),
        }
    }
}

impl Drop for LeanWorker {
    fn drop(&mut self) {
        let Ok(process) = self.process.get_mut() else {
            return;
        };
        let id = process.id();
        let _ = process.transact(&Request::Shutdown { id });
        let _ = process.child.wait();
    }
}

fn unexpected(expected: &str, response: Response) -> WorkerError {
    match response {
        Response::Failed { id, diagnostic } => WorkerError::Protocol(format!(
            "worker request {id} failed while awaiting {expected}: {diagnostic}"
        )),
        Response::Stopped { id } => WorkerError::Protocol(format!(
            "worker stopped after request {id} while awaiting {expected}"
        )),
        _ => WorkerError::Protocol(format!(
            "worker returned an unexpected response to {expected}"
        )),
    }
}

fn decode_json<T: for<'de> Deserialize<'de>>(encoded: &str) -> Result<T, WorkerError> {
    let mut deserializer = serde_json::Deserializer::from_str(encoded);
    deserializer.disable_recursion_limit();
    Ok(T::deserialize(&mut deserializer)?)
}

fn run_checked(
    executable: &Path,
    arguments: &[&str],
    directory: &Path,
    description: &str,
) -> Result<String, WorkerError> {
    let output = Command::new(executable)
        .args(arguments)
        .current_dir(directory)
        .output()?;
    if !output.status.success() {
        return Err(WorkerError::Protocol(format!(
            "{description} failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn validate_snapshot_pin(snapshot: &LeanSnapshotPin) -> Result<(), WorkerError> {
    let valid_commit = |value: &str| {
        value.len() == 40
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if !valid_commit(&snapshot.mathlib_commit) || !valid_commit(&snapshot.lean_commit) {
        return Err(WorkerError::Protocol(
            "Lean snapshot commits must be canonical lowercase SHA-1 identities".into(),
        ));
    }
    if !snapshot.lean_toolchain.starts_with("leanprover/lean4:v")
        || snapshot.lean_version.is_empty()
        || snapshot.lean_runtime_version.is_empty()
        || [
            snapshot.lean_toolchain.as_str(),
            snapshot.lean_version.as_str(),
            snapshot.lean_runtime_version.as_str(),
        ]
        .iter()
        .any(|value| value.chars().any(char::is_whitespace))
    {
        return Err(WorkerError::Protocol(
            "Lean snapshot toolchain and versions are not canonical".into(),
        ));
    }
    Ok(())
}

fn verify_git_checkout(path: &Path, expected: &str, description: &str) -> Result<(), WorkerError> {
    let actual = run_checked("git".as_ref(), &["rev-parse", "HEAD"], path, description)?;
    if actual.trim() != expected {
        return Err(WorkerError::Protocol(format!(
            "{description} checkout is {}, expected {expected}",
            actual.trim()
        )));
    }
    let status = run_checked(
        "git".as_ref(),
        &["status", "--porcelain=v1", "--untracked-files=all"],
        path,
        description,
    )?;
    if !status.trim().is_empty() {
        return Err(WorkerError::Protocol(format!(
            "{description} checkout is dirty: {}",
            status.lines().next().unwrap_or("unknown change")
        )));
    }
    Ok(())
}

fn verify_manifest_dependencies(mathlib_root: &Path) -> Result<(), WorkerError> {
    let manifest: LakeManifest =
        serde_json::from_slice(&std::fs::read(mathlib_root.join("lake-manifest.json"))?)?;
    for package in manifest
        .packages
        .iter()
        .filter(|package| package.source_type == "git")
    {
        let path = mathlib_root
            .join(&manifest.packages_dir)
            .join(&package.name);
        verify_git_checkout(
            &path,
            &package.rev,
            &format!("Lake package {}", package.name),
        )?;
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(unix)]
fn terminate_process(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pid}")])
        .status();
}

#[cfg(windows)]
fn terminate_process(pid: u32) {
    let _ = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .status();
}

#[allow(dead_code)]
fn _assert_paths_are_paths(_: &Path) {}
