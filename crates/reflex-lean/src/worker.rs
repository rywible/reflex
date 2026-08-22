use std::io::{BufRead, BufReader, BufWriter, Write};
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ast::{LeanEnvironmentIdentity, LeanExpr, LeanName};
use crate::{LEAN_COMMIT, LEAN_TOOLCHAIN, MATHLIB_COMMIT};

pub const PROTOCOL_VERSION: usize = 1;
pub const DEFAULT_WORKER_RESIDENT_BYTES: u64 = 4 * 1024 * 1024 * 1024;

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
}

impl LeanWorkerConfig {
    pub fn pinned(
        lake_executable: impl Into<PathBuf>,
        mathlib_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            lake_executable: lake_executable.into(),
            mathlib_root: mathlib_root.into(),
            worker_source: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("worker/ReflexLeanWorker.lean"),
            resident_bytes: NonZeroU64::new(DEFAULT_WORKER_RESIDENT_BYTES)
                .unwrap_or(NonZeroU64::MIN),
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
        let head = std::fs::read_to_string(self.mathlib_root.join(".git/HEAD"))?;
        if !head.contains(MATHLIB_COMMIT) {
            let output = Command::new("git")
                .args(["rev-parse", "HEAD"])
                .current_dir(&self.mathlib_root)
                .output()?;
            let actual = String::from_utf8_lossy(&output.stdout);
            if actual.trim() != MATHLIB_COMMIT {
                return Err(WorkerError::Protocol(format!(
                    "mathlib checkout is {}, expected {MATHLIB_COMMIT}",
                    actual.trim()
                )));
            }
        }
        Ok(())
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
    pub proposition: LeanExpr,
    pub proof_term: LeanExpr,
    pub allowed_axioms: Vec<LeanName>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerificationResult {
    pub accepted: bool,
    pub axioms: Vec<LeanName>,
    pub diagnostic: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TheoremFingerprint {
    pub name: LeanName,
    pub statement_hash: String,
    pub dependencies: Vec<LeanName>,
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
    Ping { id: String },
    Verify { id: String, items: &'a [VerificationItem] },
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
    Fetch { id: String, names: &'a [LeanName] },
    Shutdown { id: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum Response {
    Ready { handshake: Handshake },
    Pong { id: String },
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
    Stopped { id: String },
    Failed { id: String, diagnostic: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Handshake {
    protocol_version: usize,
    mathlib_commit: String,
    lean_toolchain: String,
    lean_commit: String,
    trust_level: usize,
}

struct Process {
    child: Child,
    input: BufWriter<ChildStdin>,
    output: BufReader<ChildStdout>,
    next_id: u64,
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
        let worker_root = config.worker_source.parent().ok_or_else(|| {
            WorkerError::Protocol("worker source has no parent directory".into())
        })?;
        let mut child = Command::new(&config.lake_executable)
            .args([
                "env",
                "lean",
                "--trust=0",
                "--threads=1",
                "-DwarningAsError=true",
            ])
            .arg(format!("--root={}", worker_root.display()))
            .arg("--run")
            .arg(&config.worker_source)
            .current_dir(&config.mathlib_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let input = child.stdin.take().ok_or_else(|| {
            WorkerError::Protocol("worker did not expose standard input".into())
        })?;
        let output = child.stdout.take().ok_or_else(|| {
            WorkerError::Protocol("worker did not expose standard output".into())
        })?;
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
            || handshake.mathlib_commit != MATHLIB_COMMIT
            || handshake.lean_toolchain != LEAN_TOOLCHAIN
            || handshake.lean_commit != LEAN_COMMIT
            || handshake.trust_level != 0
        {
            return Err(WorkerError::Protocol(
                "worker reported an incompatible semantic identity".into(),
            ));
        }
        let environment = LeanEnvironmentIdentity {
            mathlib_commit: handshake.mathlib_commit,
            lean_toolchain: handshake.lean_toolchain,
            lean_commit: handshake.lean_commit,
        };
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
        let id = process.id();
        match process.transact(&Request::Verify {
            id: id.clone(),
            items,
        })? {
            Response::Verified {
                id: response_id,
                results,
            } if response_id == id && results.len() == items.len() => {
                Ok((results, self.usage(started.elapsed())))
            }
            response => Err(unexpected("verified batch", response)),
        }
    }

    pub fn verify_bounded(
        &self,
        items: &[VerificationItem],
        deadline: Duration,
    ) -> Result<(Vec<VerificationResult>, WorkerUsage), WorkerError> {
        let started = Instant::now();
        let mut process = self.lock()?;
        let id = process.id();
        match process.transact_bounded(
            &Request::Verify {
                id: id.clone(),
                items,
            },
            deadline,
        )? {
            Response::Verified {
                id: response_id,
                results,
            } if response_id == id && results.len() == items.len() => {
                Ok((results, self.usage(started.elapsed())))
            }
            response => Err(unexpected("bounded verified batch", response)),
        }
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
        let id = process.id();
        match process.transact(&Request::Fetch {
            id: id.clone(),
            names,
        })? {
            Response::Fetched {
                id: response_id,
                artifacts,
            } if response_id == id && artifacts.len() == names.len() => Ok(artifacts),
            response => Err(unexpected("fetched theorem bodies", response)),
        }
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
        _ => WorkerError::Protocol(format!("worker returned an unexpected response to {expected}")),
    }
}

fn decode_json<T: for<'de> Deserialize<'de>>(encoded: &str) -> Result<T, WorkerError> {
    let mut deserializer = serde_json::Deserializer::from_str(encoded);
    deserializer.disable_recursion_limit();
    Ok(T::deserialize(&mut deserializer)?)
}

#[cfg(unix)]
fn terminate_process(pid: u32) {
    let _ = Command::new("kill")
        .args(["-KILL", &pid.to_string()])
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
