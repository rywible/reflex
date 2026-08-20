//! Validation and launch policy for untrusted domain processes (P16.6).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use thiserror::Error;

const MAX_SECRET_SCAN_BYTES: usize = 1024 * 1024;

#[derive(Error, Debug)]
pub enum SandboxError {
    #[error("path traversal rejected: {0}")]
    PathTraversal(String),
    #[error("sandbox root is unavailable: {0}")]
    RootUnavailable(String),
    #[error("memory limit exceeded: requested {requested} max {max}")]
    MemoryLimit { requested: u64, max: u64 },
    #[error("frame size limit exceeded: {size} > {max}")]
    FrameTooLarge { size: usize, max: usize },
    #[error("fork bomb / process limit exceeded")]
    ProcessLimit,
    #[error("secret leakage blocked")]
    SecretLeakage,
    #[error("sandbox policy is invalid: {0}")]
    InvalidPolicy(String),
    #[error("strong external-process isolation is unavailable: {0}")]
    IsolationUnavailable(String),
}

#[derive(Clone, Debug)]
pub struct SandboxPolicy {
    pub max_memory_bytes: u64,
    pub max_frame_bytes: usize,
    pub max_processes: u32,
    pub max_open_files: u64,
    pub allow_network: bool,
    pub workspace_root: PathBuf,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self {
            max_memory_bytes: 1024 * 1024 * 1024,
            max_frame_bytes: 1024 * 1024,
            max_processes: 32,
            max_open_files: 256,
            allow_network: false,
            workspace_root: PathBuf::from(".reflex/sandbox"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxLimits {
    pub address_space_bytes: u64,
    pub processes: u32,
    pub open_files: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxExecutableMapping {
    pub host_path: PathBuf,
    pub sandbox_path: PathBuf,
}

impl SandboxPolicy {
    pub fn validate(&self) -> Result<(), SandboxError> {
        if self.max_memory_bytes == 0 || self.max_frame_bytes == 0 {
            return Err(SandboxError::InvalidPolicy(
                "memory and frame limits must be non-zero".into(),
            ));
        }
        if self.max_processes == 0 || self.max_open_files < 3 {
            return Err(SandboxError::InvalidPolicy(
                "process limit must be non-zero and open-file limit at least three".into(),
            ));
        }
        Ok(())
    }

    /// Resolve an existing path beneath the sandbox root. Absolute paths,
    /// parent components, and symlink escapes are rejected.
    pub fn validate_path(&self, path: &Path) -> Result<PathBuf, SandboxError> {
        self.validate()?;
        validate_relative_components(path)?;
        let root = self.canonical_root()?;
        let resolved = root
            .join(path)
            .canonicalize()
            .map_err(|_| SandboxError::PathTraversal(path.display().to_string()))?;
        if !resolved.starts_with(&root) {
            return Err(SandboxError::PathTraversal(path.display().to_string()));
        }
        Ok(resolved)
    }

    /// Resolve a not-yet-created output beneath an existing sandbox parent.
    pub fn validate_output_path(&self, path: &Path) -> Result<PathBuf, SandboxError> {
        self.validate()?;
        validate_relative_components(path)?;
        let root = self.canonical_root()?;
        let parent = path.parent().unwrap_or_else(|| Path::new(""));
        let canonical_parent = root
            .join(parent)
            .canonicalize()
            .map_err(|_| SandboxError::PathTraversal(path.display().to_string()))?;
        if !canonical_parent.starts_with(&root) {
            return Err(SandboxError::PathTraversal(path.display().to_string()));
        }
        let file_name = path
            .file_name()
            .ok_or_else(|| SandboxError::PathTraversal(path.display().to_string()))?;
        Ok(canonical_parent.join(file_name))
    }

    fn canonical_root(&self) -> Result<PathBuf, SandboxError> {
        self.workspace_root.canonicalize().map_err(|error| {
            SandboxError::RootUnavailable(format!("{}: {error}", self.workspace_root.display()))
        })
    }

    /// Map a host executable to the exact read-only path visible inside the
    /// sandbox. Relative executables must live under the immutable root;
    /// absolute executables are restricted to the read-only system tree.
    pub fn map_executable(
        &self,
        executable: &Path,
    ) -> Result<SandboxExecutableMapping, SandboxError> {
        self.validate()?;
        let (host_path, sandbox_path) = if executable.is_absolute() {
            let canonical = executable.canonicalize().map_err(|error| {
                SandboxError::RootUnavailable(format!("{}: {error}", executable.display()))
            })?;
            let allowed = [Path::new("/usr"), Path::new("/bin")]
                .iter()
                .any(|root| canonical.starts_with(root));
            if !allowed {
                return Err(SandboxError::PathTraversal(
                    executable.display().to_string(),
                ));
            }
            (canonical.clone(), canonical)
        } else {
            let host = self.validate_path(executable)?;
            let relative = host
                .strip_prefix(self.canonical_root()?)
                .map_err(|_| SandboxError::PathTraversal(executable.display().to_string()))?
                .to_path_buf();
            (host, Path::new("/work").join(relative))
        };
        if !host_path.is_file() {
            return Err(SandboxError::InvalidPolicy(format!(
                "sandbox executable is not a regular file: {}",
                host_path.display()
            )));
        }
        Ok(SandboxExecutableMapping {
            host_path,
            sandbox_path,
        })
    }

    /// Validate a dedicated writable scratch directory that is disjoint from
    /// the immutable input tree. Keeping it outside `workspace_root` prevents
    /// the same inode from being reachable through both `/work` and a writable
    /// mount.
    pub fn validate_scratch_path(&self, path: &Path) -> Result<PathBuf, SandboxError> {
        self.validate()?;
        if !path.is_absolute()
            || std::fs::symlink_metadata(path)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(true)
        {
            return Err(SandboxError::InvalidPolicy(
                "scratch must be an existing absolute non-symlink directory".into(),
            ));
        }
        let scratch = path.canonicalize().map_err(|error| {
            SandboxError::RootUnavailable(format!("{}: {error}", path.display()))
        })?;
        let root = self.canonical_root()?;
        if !scratch.is_dir()
            || scratch == root
            || scratch.starts_with(&root)
            || root.starts_with(&scratch)
        {
            return Err(SandboxError::InvalidPolicy(
                "scratch must be a dedicated directory disjoint from the immutable root".into(),
            ));
        }
        Ok(scratch)
    }

    pub fn check_frame_size(&self, size: usize) -> Result<(), SandboxError> {
        if size > self.max_frame_bytes {
            return Err(SandboxError::FrameTooLarge {
                size,
                max: self.max_frame_bytes,
            });
        }
        Ok(())
    }

    pub fn check_memory_request(&self, bytes: u64) -> Result<(), SandboxError> {
        if bytes > self.max_memory_bytes {
            return Err(SandboxError::MemoryLimit {
                requested: bytes,
                max: self.max_memory_bytes,
            });
        }
        Ok(())
    }

    /// Limits requested by the policy. This does not claim they have been
    /// installed; use `prepare_linux_command` for an enforceable launch.
    pub fn limits(&self) -> Result<SandboxLimits, SandboxError> {
        self.validate()?;
        Ok(SandboxLimits {
            address_space_bytes: self.max_memory_bytes,
            processes: self.max_processes,
            open_files: self.max_open_files,
        })
    }

    /// Construct a Linux command using a strong bubblewrap boundary plus
    /// kernel rlimits. If bubblewrap or prlimit is absent, this fails closed.
    /// The child sees a minimal environment, read-only system and immutable
    /// input roots, one dedicated writable scratch mount, and no network
    /// namespace by default.
    #[cfg(target_os = "linux")]
    pub fn prepare_linux_command(
        &self,
        executable: &Path,
        writable_scratch: &Path,
        arguments: &[String],
        requested_environment: &BTreeMap<String, String>,
    ) -> Result<Command, SandboxError> {
        self.validate()?;
        let executable = self.map_executable(executable)?;
        let writable_scratch = self.validate_scratch_path(writable_scratch)?;
        let root = self.canonical_root()?;
        if executable.host_path.starts_with(&writable_scratch) {
            return Err(SandboxError::InvalidPolicy(
                "scratch must be dedicated and must not contain the executable".into(),
            ));
        }
        let argument_bytes = arguments.iter().try_fold(0usize, |total, argument| {
            total
                .checked_add(argument.len())
                .ok_or_else(|| SandboxError::InvalidPolicy("argument byte count overflow".into()))
        })?;
        let environment_bytes =
            requested_environment
                .iter()
                .try_fold(0usize, |total, (key, value)| {
                    total
                        .checked_add(key.len())
                        .and_then(|sum| sum.checked_add(value.len()))
                        .ok_or_else(|| {
                            SandboxError::InvalidPolicy("environment byte count overflow".into())
                        })
                })?;
        self.check_frame_size(argument_bytes.saturating_add(environment_bytes))?;
        let bwrap = required_executable(&["/usr/bin/bwrap", "/bin/bwrap"], "bubblewrap")?;
        let prlimit = required_executable(&["/usr/bin/prlimit", "/bin/prlimit"], "prlimit")?;
        let limits = self.limits()?;

        let mut command = Command::new(bwrap);
        command
            .arg("--die-with-parent")
            .arg("--new-session")
            .arg("--unshare-user")
            .args(["--uid", "65534", "--gid", "65534"])
            .arg("--unshare-pid")
            .arg("--unshare-ipc")
            .arg("--unshare-uts");
        if !self.allow_network {
            command.arg("--unshare-net");
        }
        for system_path in ["/usr", "/bin", "/lib", "/lib64"] {
            if Path::new(system_path).exists() {
                command.args(["--ro-bind", system_path, system_path]);
            }
        }
        for system_file in ["/etc/ld.so.cache", "/etc/localtime"] {
            if Path::new(system_file).is_file() {
                command.args(["--ro-bind", system_file, system_file]);
            }
        }
        command
            .args(["--proc", "/proc", "--dev", "/dev", "--ro-bind"])
            .arg(&root)
            .arg("/work")
            .arg("--bind")
            .arg(&writable_scratch)
            .arg("/scratch")
            .arg("--bind")
            .arg(&writable_scratch)
            .arg("/tmp")
            .args(["--chdir", "/scratch", "--"])
            .arg(prlimit)
            .arg(format!("--as={}", limits.address_space_bytes))
            .arg(format!("--nproc={}", limits.processes))
            .arg(format!("--nofile={}", limits.open_files))
            .arg("--")
            .arg(executable.sandbox_path)
            .args(arguments)
            .env_clear();
        command.env("HOME", "/scratch").env("TMPDIR", "/scratch");
        for (key, value) in sanitized_environment(requested_environment) {
            command.env(key, value);
        }
        Ok(command)
    }

    #[cfg(not(target_os = "linux"))]
    pub fn prepare_linux_command(
        &self,
        _executable: &Path,
        _writable_scratch: &Path,
        _arguments: &[String],
        _requested_environment: &BTreeMap<String, String>,
    ) -> Result<Command, SandboxError> {
        Err(SandboxError::IsolationUnavailable(
            "the Linux bubblewrap backend is not available on this platform".into(),
        ))
    }

    pub fn reject_secret_output(&self, output: &str) -> Result<(), SandboxError> {
        if output.len() > MAX_SECRET_SCAN_BYTES {
            return Err(SandboxError::SecretLeakage);
        }
        let lower = output.to_ascii_lowercase();
        if [
            "reflex_worker_token",
            "password=",
            "secret=",
            "api_key=",
            "bearer ",
        ]
        .iter()
        .any(|marker| lower.contains(marker))
        {
            return Err(SandboxError::SecretLeakage);
        }
        Ok(())
    }
}

fn validate_relative_components(path: &Path) -> Result<(), SandboxError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(SandboxError::PathTraversal(path.display().to_string()));
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn required_executable(candidates: &[&str], name: &str) -> Result<PathBuf, SandboxError> {
    candidates
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .ok_or_else(|| {
            SandboxError::IsolationUnavailable(format!(
                "required `{name}` launcher is not installed"
            ))
        })
}

fn sanitized_environment(requested: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    const ALLOWED: &[&str] = &["LANG", "LC_ALL", "TZ"];
    requested
        .iter()
        .filter(|(key, _)| ALLOWED.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> (tempfile::TempDir, SandboxPolicy) {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("nested")).unwrap();
        let policy = SandboxPolicy {
            workspace_root: temp.path().to_path_buf(),
            ..SandboxPolicy::default()
        };
        (temp, policy)
    }

    #[test]
    fn rejects_absolute_parent_and_symlink_escape() {
        let (temp, policy) = policy();
        assert!(policy.validate_path(Path::new("/etc/passwd")).is_err());
        assert!(policy.validate_path(Path::new("../etc/passwd")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/etc", temp.path().join("escape")).unwrap();
            assert!(policy.validate_path(Path::new("escape/passwd")).is_err());
        }
    }

    #[test]
    fn accepts_only_paths_below_canonical_root() {
        let (_temp, policy) = policy();
        let path = policy.validate_path(Path::new("nested")).unwrap();
        assert!(path.starts_with(policy.workspace_root.canonicalize().unwrap()));
        assert!(
            policy
                .validate_output_path(Path::new("nested/new.bin"))
                .is_ok()
        );
    }

    #[test]
    fn limits_frames_memory_and_secret_output() {
        let (_temp, policy) = policy();
        assert!(policy.check_frame_size(policy.max_frame_bytes + 1).is_err());
        assert!(
            policy
                .check_memory_request(policy.max_memory_bytes + 1)
                .is_err()
        );
        assert!(
            policy
                .reject_secret_output("Authorization: Bearer abc")
                .is_err()
        );
        assert!(
            policy
                .reject_secret_output(&"x".repeat(MAX_SECRET_SCAN_BYTES + 1))
                .is_err()
        );
    }

    #[test]
    fn environment_drops_credentials_and_paths() {
        let requested = BTreeMap::from([
            ("LANG".into(), "C.UTF-8".into()),
            ("PATH".into(), "/evil".into()),
            ("REFLEX_WORKER_TOKEN".into(), "secret".into()),
        ]);
        assert_eq!(
            sanitized_environment(&requested),
            BTreeMap::from([("LANG".into(), "C.UTF-8".into())])
        );
    }

    #[test]
    fn maps_relative_executable_and_rejects_writable_or_arbitrary_absolute_paths() {
        let (temp, policy) = policy();
        let executable = temp.path().join("worker");
        std::fs::write(&executable, b"worker").unwrap();
        let mapping = policy.map_executable(Path::new("worker")).unwrap();
        assert_eq!(mapping.host_path, executable.canonicalize().unwrap());
        assert_eq!(mapping.sandbox_path, Path::new("/work/worker"));
        assert!(policy.map_executable(Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn writable_scratch_is_disjoint_from_immutable_root() {
        let (root, policy) = policy();
        assert!(
            policy
                .validate_scratch_path(&root.path().join("nested"))
                .is_err()
        );
        let scratch_parent = tempfile::tempdir().unwrap();
        let scratch = scratch_parent.path().join("cell-scratch");
        std::fs::create_dir(&scratch).unwrap();
        assert_eq!(
            policy.validate_scratch_path(&scratch).unwrap(),
            scratch.canonicalize().unwrap()
        );
        assert!(policy.validate_scratch_path(Path::new("relative")).is_err());
    }
}
