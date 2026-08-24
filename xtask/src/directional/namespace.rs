use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io::{Read as _, Seek as _, SeekFrom, Write as _};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use crate::harness::{
    AnyError, ExecutableIdentity, PinnedExecutable, capture_child_bounded_clean,
    directional_namespace_seccomp_filter, hash_json, pin_executable, pinned_fd_path,
    require_sealed_memfd,
};

use super::{GateReceipt, GateSpec, SourceSnapshot};

const SCHEMA: &str = "reflex-directional-namespace-v2";
const SOURCE_MOUNT_NAME: &str = "source";
const BUILD_MOUNT_NAME: &str = "build";
const TARGET_DIRECTORY_NAME: &str = "target";
const TEMPORARY_DIRECTORY_NAME: &str = "tmp";
const CARGO_HOME_NAME: &str = "cargo-home";
const SOURCE_MINIMUM_BYTES: u64 = 64 * 1024 * 1024;
const SOURCE_MAXIMUM_BYTES: u64 = 8 * 1024 * 1024 * 1024;
const DIRECTIONAL_MEMORY_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const BUILD_MINIMUM_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const SOURCE_MINIMUM_INODES: u64 = 4_096;
const SOURCE_MAXIMUM_INODES: u64 = 262_144;
const BUILD_INODES: u64 = 524_288;
const INNER_RESIDENT_LIMIT: u64 = DIRECTIONAL_MEMORY_BYTES;
const INNER_OUTPUT_MARKER: &str = "reflex-directional-namespace-outcome-v1:";
const CAMPAIGN_SOURCE_SCHEMA: &str = "reflex-directional-campaign-source-v1";

pub(crate) const SETUP_COMMAND: &str = "directional-namespace-setup";
pub(crate) const GATES_COMMAND: &str = "directional-namespace-gates";
pub(crate) const PROBE_COMMAND: &str = "directional-namespace-probe";
pub(crate) const SMOKE_COMMAND: &str = "directional-namespace-smoke";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct FilesystemBounds {
    pub(super) source_bytes: u64,
    pub(super) source_inodes: u64,
    pub(super) build_bytes: u64,
    pub(super) build_inodes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct NamespacePaths {
    pub(super) root: PathBuf,
    pub(super) source: PathBuf,
    pub(super) build: PathBuf,
    pub(super) target: PathBuf,
    pub(super) temporary: PathBuf,
    pub(super) cargo_home: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct ToolIdentity {
    pub(super) role: String,
    pub(super) identity: ExecutableIdentity,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct TreeIdentity {
    canonical_root: PathBuf,
    manifest_sha256: String,
    bytes: u64,
    inodes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(super) struct IsolationContract {
    pub(super) schema: String,
    pub(super) outer_user_namespace: String,
    pub(super) paths: NamespacePaths,
    pub(super) bounds: FilesystemBounds,
    pub(super) environment: Vec<(String, String)>,
    pub(super) tools: Vec<ToolIdentity>,
    pub(super) toolchain_sha256: String,
    pub(super) mathlib: Option<TreeIdentity>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct WireGate {
    name: String,
    command: Vec<String>,
    arguments: Vec<String>,
    environment: Vec<(String, String)>,
    timeout_seconds: u64,
    resident_limit_bytes: u64,
    blocker: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct NamespacePlan {
    contract: IsolationContract,
    repository_root: PathBuf,
    source_paths: Vec<Vec<u8>>,
    expected_snapshot: SourceSnapshot,
    tool_fds: BTreeMap<String, i32>,
    gates: Vec<WireGate>,
    populate_cargo_home: bool,
    populate_toolchain: bool,
    toolchain_root: PathBuf,
    mathlib_root: Option<PathBuf>,
    content_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct CampaignSourcePlan {
    schema: String,
    preparer_user_namespace: String,
    repository_root: PathBuf,
    source_paths: Vec<Vec<u8>>,
    expected_snapshot: SourceSnapshot,
    paths: NamespacePaths,
    bounds: FilesystemBounds,
    mount_descriptor: i32,
    mount_identity: ExecutableIdentity,
    parent_descriptor: i32,
    root_descriptor: i32,
    root_device: u64,
    root_inode: u64,
    content_sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct NamespaceOutcome {
    pub(super) execution_snapshot: SourceSnapshot,
    pub(super) gates: Vec<GateReceipt>,
}

pub(super) struct PreparedNamespace {
    contract: IsolationContract,
    root: PathBuf,
    source_paths: Vec<Vec<u8>>,
    expected_snapshot: SourceSnapshot,
    tools: TrustedTools,
    populate_cargo_home: bool,
    populate_toolchain: bool,
    toolchain_root: PathBuf,
    mathlib_root: Option<PathBuf>,
}

impl CampaignSourcePlan {
    pub(crate) fn source_root(&self) -> &Path {
        &self.paths.source
    }

    #[cfg(test)]
    pub(crate) fn mount_root(&self) -> &Path {
        &self.paths.root
    }

    pub(crate) fn build_environment(&self) -> Vec<(OsString, OsString)> {
        vec![
            (
                "CARGO_TARGET_DIR".into(),
                self.paths.target.clone().into_os_string(),
            ),
            (
                "TMPDIR".into(),
                self.paths.temporary.clone().into_os_string(),
            ),
        ]
    }

    pub(crate) fn validate(&self) -> Result<(), AnyError> {
        let mut unhashed = self.clone();
        let claimed = std::mem::take(&mut unhashed.content_sha256);
        if self.schema != CAMPAIGN_SOURCE_SCHEMA
            || self.preparer_user_namespace.is_empty()
            || !self.repository_root.is_absolute()
            || self.mount_descriptor < 3
            || self.parent_descriptor < 3
            || self.root_descriptor < 3
            || self.root_device == 0
            || self.root_inode == 0
            || self.mount_identity.content_sha256.is_empty()
            || self.mount_identity.size == 0
            || claimed.is_empty()
            || hash_json(&unhashed)? != claimed
        {
            return Err("Directional Campaign source plan identity is invalid".into());
        }
        validate_paths(&self.paths)?;
        validate_source_paths(&self.source_paths)?;
        if self.expected_snapshot.entries != u64::try_from(self.source_paths.len())?
            || self.bounds.source_bytes < SOURCE_MINIMUM_BYTES
            || self.bounds.source_bytes > SOURCE_MAXIMUM_BYTES
            || self.bounds.source_inodes < SOURCE_MINIMUM_INODES
            || self.bounds.source_inodes > SOURCE_MAXIMUM_INODES
            || self
                .bounds
                .source_bytes
                .checked_add(self.bounds.build_bytes)
                != Some(DIRECTIONAL_MEMORY_BYTES)
            || self.bounds.build_bytes < BUILD_MINIMUM_BYTES
            || self.bounds.build_inodes != BUILD_INODES
        {
            return Err("Directional Campaign source plan bounds are invalid".into());
        }
        Ok(())
    }
}

struct NamespaceRootCleanup(PathBuf);

impl Drop for NamespaceRootCleanup {
    fn drop(&mut self) {
        if self.0.parent() == Some(Path::new("/tmp"))
            && self
                .0
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("reflex-directional-"))
        {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

pub(super) struct TrustedTools {
    tools: BTreeMap<String, PinnedExecutable>,
}

impl FilesystemBounds {
    pub(super) fn for_snapshot(bytes: u64, entries: u64) -> Result<Self, AnyError> {
        let requested_source_bytes = bytes
            .checked_mul(5)
            .and_then(|value| value.checked_div(4))
            .and_then(|value| value.checked_add(SOURCE_MINIMUM_BYTES))
            .ok_or("Directional source tmpfs byte bound overflowed")?;
        if requested_source_bytes > SOURCE_MAXIMUM_BYTES {
            return Err("Directional source exceeds its fixed tmpfs byte policy".into());
        }
        let source_bytes = requested_source_bytes.max(SOURCE_MINIMUM_BYTES);
        let requested_source_inodes = entries
            .checked_mul(4)
            .and_then(|value| value.checked_add(SOURCE_MINIMUM_INODES))
            .ok_or("Directional source tmpfs inode bound overflowed")?;
        if requested_source_inodes > SOURCE_MAXIMUM_INODES {
            return Err("Directional source exceeds its fixed tmpfs inode policy".into());
        }
        let source_inodes = requested_source_inodes.max(SOURCE_MINIMUM_INODES);
        let build_bytes = DIRECTIONAL_MEMORY_BYTES
            .checked_sub(source_bytes)
            .filter(|bytes| *bytes >= BUILD_MINIMUM_BYTES)
            .ok_or("Directional source leaves insufficient bounded build memory")?;
        Ok(Self {
            source_bytes,
            source_inodes,
            build_bytes,
            build_inodes: BUILD_INODES,
        })
    }
}

impl NamespacePaths {
    pub(super) fn for_nonce(nonce: &str) -> Result<Self, AnyError> {
        if nonce.is_empty()
            || !nonce
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("Directional namespace nonce is not canonical".into());
        }
        let root = Path::new("/tmp").join(format!("reflex-directional-{nonce}"));
        Ok(Self {
            source: root.join(SOURCE_MOUNT_NAME),
            build: root.join(BUILD_MOUNT_NAME),
            target: root.join(BUILD_MOUNT_NAME).join(TARGET_DIRECTORY_NAME),
            temporary: root.join(BUILD_MOUNT_NAME).join(TEMPORARY_DIRECTORY_NAME),
            cargo_home: root.join(BUILD_MOUNT_NAME).join(CARGO_HOME_NAME),
            root,
        })
    }
}

impl IsolationContract {
    pub(super) fn new(
        outer_user_namespace: String,
        paths: NamespacePaths,
        bounds: FilesystemBounds,
        tools: Vec<ToolIdentity>,
        toolchain_sha256: String,
        mathlib: Option<TreeIdentity>,
        jobs: usize,
    ) -> Result<Self, AnyError> {
        if outer_user_namespace.is_empty() || toolchain_sha256.is_empty() || jobs == 0 {
            return Err("Directional namespace contract omits an authority".into());
        }
        validate_paths(&paths)?;
        validate_tools(&tools)?;
        let environment = exact_gate_environment(&paths, jobs);
        Ok(Self {
            schema: SCHEMA.into(),
            outer_user_namespace,
            paths,
            bounds,
            environment,
            tools,
            toolchain_sha256,
            mathlib,
        })
    }

    pub(super) fn validate(&self) -> Result<(), AnyError> {
        if self.schema != SCHEMA
            || self.outer_user_namespace.is_empty()
            || self.toolchain_sha256.is_empty()
        {
            return Err("Directional namespace contract schema or authority is invalid".into());
        }
        validate_paths(&self.paths)?;
        validate_tools(&self.tools)?;
        let has_lake = self.tools.iter().any(|tool| tool.role == "lake");
        if has_lake != self.mathlib.is_some()
            || self.mathlib.as_ref().is_some_and(|identity| {
                !identity.canonical_root.is_absolute()
                    || identity.manifest_sha256.is_empty()
                    || identity.bytes == 0
                    || identity.inodes == 0
                    || identity.bytes > self.bounds.source_bytes
                    || identity.inodes > self.bounds.source_inodes
            })
        {
            return Err("Directional Lean fixture identity is incomplete".into());
        }
        if self.bounds.source_bytes < SOURCE_MINIMUM_BYTES
            || self.bounds.source_bytes > SOURCE_MAXIMUM_BYTES
            || self.bounds.source_inodes < SOURCE_MINIMUM_INODES
            || self.bounds.source_inodes > SOURCE_MAXIMUM_INODES
            || self
                .bounds
                .source_bytes
                .checked_add(self.bounds.build_bytes)
                != Some(DIRECTIONAL_MEMORY_BYTES)
            || self.bounds.build_bytes < BUILD_MINIMUM_BYTES
            || self.bounds.build_inodes != BUILD_INODES
        {
            return Err("Directional namespace filesystem bounds are invalid".into());
        }
        let jobs = environment_map(&self.environment)?
            .get("CARGO_BUILD_JOBS")
            .ok_or("Directional environment omits CARGO_BUILD_JOBS")?
            .parse::<usize>()?;
        if jobs == 0 || self.environment != exact_gate_environment(&self.paths, jobs) {
            return Err("Directional namespace environment is not the exact allowlist".into());
        }
        Ok(())
    }

    pub(super) fn lean_fixture(&self) -> Result<super::LeanFixture, String> {
        let Some(_) = self.mathlib.as_ref() else {
            return Err(
                "tiny Lean smoke is blocked: no pinned mathlib fixture was recorded".into(),
            );
        };
        let lake = self
            .tools
            .iter()
            .find(|tool| tool.role == "lake")
            .ok_or_else(|| {
                "tiny Lean smoke is blocked: no sealed lake tool was recorded".to_owned()
            })?;
        let _ = lake;
        Ok(super::LeanFixture {
            lake: self.paths.source.join(".directional-tools/bin/lake"),
            mathlib: self.paths.source.join(".directional-mathlib"),
        })
    }

    pub(super) fn verify_external_trees(&self) -> Result<(), AnyError> {
        let (toolchain_sha256, _, _) = tree_identity(&trusted_toolchain_root()?)?;
        if toolchain_sha256 != self.toolchain_sha256 {
            return Err("Directional Harness receipt Rust toolchain identity is stale".into());
        }
        if let Some(expected) = &self.mathlib {
            let fixture = super::discover_lean_fixture()
                .map_err(|error| format!("Directional Lean fixture is unavailable: {error}"))?;
            let lake_root = std::fs::canonicalize(&fixture.lake)?;
            let recorded_lake = self
                .tools
                .iter()
                .find(|tool| tool.role == "lake")
                .ok_or("Directional receipt omits its lake identity")?;
            if Path::new(&recorded_lake.identity.canonical_path) != lake_root
                || std::fs::canonicalize(&fixture.mathlib)? != expected.canonical_root
                || tree_snapshot(&expected.canonical_root)? != *expected
            {
                return Err("Directional Harness receipt Lean fixture identity is stale".into());
            }
        }
        Ok(())
    }
}

impl TrustedTools {
    pub(super) fn discover(lake: Option<&Path>) -> Result<Self, AnyError> {
        let toolchain = trusted_toolchain_root()?;
        let mut tools = BTreeMap::new();
        for (role, path) in [
            ("cargo", toolchain.join("bin/cargo")),
            ("cargo-clippy", toolchain.join("bin/cargo-clippy")),
            ("cargo-fmt", toolchain.join("bin/cargo-fmt")),
            ("clippy-driver", toolchain.join("bin/clippy-driver")),
            ("rustc", toolchain.join("bin/rustc")),
            ("rustdoc", toolchain.join("bin/rustdoc")),
            ("rustfmt", toolchain.join("bin/rustfmt")),
            ("ar", trusted_system_tool("ar")?),
            ("cc", trusted_system_tool("cc")?),
            ("ld", trusted_system_tool("ld")?),
            ("mount", trusted_system_tool("mount")?),
            ("setpriv", trusted_system_tool("setpriv")?),
            ("unshare", trusted_system_tool("unshare")?),
            ("xtask", std::env::current_exe()?),
        ] {
            if tools.insert(role.into(), pin_executable(&path)?).is_some() {
                return Err(format!("duplicate Directional tool role {role}").into());
            }
        }
        if let Some(lake) = lake {
            tools.insert("lake".into(), pin_executable(lake)?);
        }
        Ok(Self { tools })
    }

    pub(super) fn identities(&self) -> Vec<ToolIdentity> {
        self.tools
            .iter()
            .map(|(role, tool)| ToolIdentity {
                role: role.clone(),
                identity: tool.identity.clone(),
            })
            .collect()
    }

    pub(super) fn rediscover_identities(
        expected: &[ToolIdentity],
    ) -> Result<Vec<ToolIdentity>, AnyError> {
        let lake = expected
            .iter()
            .find(|tool| tool.role == "lake")
            .map(|tool| Path::new(&tool.identity.canonical_path));
        Ok(Self::discover(lake)?.identities())
    }

    pub(super) fn executable(&self, role: &str) -> Result<&PinnedExecutable, AnyError> {
        self.tools
            .get(role)
            .ok_or_else(|| format!("Directional tool role {role} is not pinned").into())
    }

    pub(super) fn descriptor_path(&self, role: &str) -> Result<PathBuf, AnyError> {
        Ok(pinned_fd_path(&self.executable(role)?.file))
    }
}

pub(super) fn current_cargo_label() -> Result<OsString, AnyError> {
    let tools = TrustedTools::discover(None)?;
    let cargo = tools.executable("cargo")?;
    Ok(format!("sealed-cargo:{}", cargo.identity.content_sha256).into())
}

pub(crate) fn prepare_campaign_source_plan(
    root: &Path,
    source_paths: Vec<Vec<u8>>,
    expected_snapshot: SourceSnapshot,
) -> Result<(CampaignSourcePlan, Vec<std::fs::File>), AnyError> {
    let repository_root = std::fs::canonicalize(root)?;
    validate_source_paths(&source_paths)?;
    if super::source_snapshot_for_paths(&repository_root, &source_paths)? != expected_snapshot {
        return Err("source changed before the Campaign source plan was sealed".into());
    }
    let source_bytes =
        source_paths
            .iter()
            .try_fold(0_u64, |total, encoded| -> Result<u64, AnyError> {
                let relative = super::path_from_git_bytes(encoded)?;
                match std::fs::symlink_metadata(repository_root.join(relative)) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                        Err("Directional Campaign source contains a symlink or non-file".into())
                    }
                    Ok(metadata) => total
                        .checked_add(metadata.len())
                        .ok_or_else(|| "Directional Campaign source byte count overflowed".into()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(total),
                    Err(error) => Err(error.into()),
                }
            })?;
    let nonce = format!("campaign-{}", random_nonce()?);
    let paths = NamespacePaths::for_nonce(&nonce)?;
    std::fs::create_dir(&paths.root)?;
    std::fs::set_permissions(&paths.root, std::fs::Permissions::from_mode(0o700))?;
    let parent = open_directory_nofollow(
        paths
            .root
            .parent()
            .ok_or("Campaign source root has no parent")?,
    )?;
    let root_directory = open_directory_nofollow(&paths.root)?;
    let root_metadata = root_directory.metadata()?;
    let bounds = FilesystemBounds::for_snapshot(
        source_bytes,
        u64::try_from(source_paths.len()).unwrap_or(u64::MAX),
    )?;
    let mount = pin_executable(&trusted_system_tool("mount")?)?;
    let mount_descriptor = mount.file.as_raw_fd();
    let mut plan = CampaignSourcePlan {
        schema: CAMPAIGN_SOURCE_SCHEMA.into(),
        preparer_user_namespace: current_user_namespace()?,
        repository_root,
        source_paths,
        expected_snapshot,
        paths,
        bounds,
        mount_descriptor,
        mount_identity: mount.identity,
        parent_descriptor: parent.as_raw_fd(),
        root_descriptor: root_directory.as_raw_fd(),
        root_device: root_metadata.dev(),
        root_inode: root_metadata.ino(),
        content_sha256: String::new(),
    };
    plan.content_sha256 = hash_json(&plan)?;
    plan.validate()?;
    Ok((plan, vec![mount.file, parent, root_directory]))
}

pub(crate) fn materialize_campaign_source(plan: &CampaignSourcePlan) -> Result<PathBuf, AnyError> {
    plan.validate()?;
    if current_user_namespace()? == plan.preparer_user_namespace {
        return Err(
            "Campaign source materialization requires the trusted child user namespace".into(),
        );
    }
    validate_pinned_descriptor(plan.mount_descriptor, &plan.mount_identity)?;
    let current = super::source_snapshot_for_paths(&plan.repository_root, &plan.source_paths)?;
    if current != plan.expected_snapshot {
        return Err("source changed before Campaign materialization".into());
    }
    require_campaign_root_identity(plan)?;
    std::fs::create_dir(&plan.paths.source)?;
    std::fs::create_dir(&plan.paths.build)?;
    mount_tmpfs(
        &plan.paths.source,
        plan.bounds.source_bytes,
        plan.bounds.source_inodes,
        true,
    )?;
    mount_tmpfs(
        &plan.paths.build,
        plan.bounds.build_bytes,
        plan.bounds.build_inodes,
        true,
    )?;
    create_build_layout(&plan.paths)?;
    copy_manifest_paths(
        &plan.repository_root,
        &plan.paths.source,
        &plan.source_paths,
    )?;
    if super::source_snapshot_for_paths(&plan.paths.source, &plan.source_paths)?
        != plan.expected_snapshot
    {
        return Err("private Campaign source failed exact snapshot verification".into());
    }
    remount_readonly(&plan.paths.source)?;
    attest_campaign_mounts(plan)?;
    require_write_denied(&plan.paths.source)?;
    validate_pinned_descriptor(plan.mount_descriptor, &plan.mount_identity)?;
    Ok(plan.paths.source.clone())
}

pub(crate) fn cleanup_campaign_source(plan: &CampaignSourcePlan) -> Result<(), AnyError> {
    use nix::unistd::{UnlinkatFlags, unlinkat};

    plan.validate()?;
    require_campaign_root_identity(plan)?;
    let root = std::fs::File::open(format!("/proc/self/fd/{}", plan.root_descriptor))?;
    let mut names = std::fs::read_dir(format!("/proc/self/fd/{}", plan.root_descriptor))?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<Result<Vec<_>, _>>()?;
    names.sort();
    if names
        .iter()
        .any(|name| name != SOURCE_MOUNT_NAME && name != BUILD_MOUNT_NAME)
    {
        return Err("Campaign source cleanup found an unexpected root entry".into());
    }
    for name in [SOURCE_MOUNT_NAME, BUILD_MOUNT_NAME] {
        if names.iter().any(|observed| observed == name) {
            unlinkat(&root, name, UnlinkatFlags::RemoveDir)?;
        }
    }
    require_campaign_root_identity(plan)?;
    let parent = std::fs::File::open(format!("/proc/self/fd/{}", plan.parent_descriptor))?;
    unlinkat(
        &parent,
        plan.paths
            .root
            .file_name()
            .ok_or("Campaign source root has no file name")?,
        UnlinkatFlags::RemoveDir,
    )?;
    Ok(())
}

impl PreparedNamespace {
    pub(super) fn prepare(
        root: &Path,
        source_paths: Vec<Vec<u8>>,
        expected_snapshot: SourceSnapshot,
        lean: Option<&super::LeanFixture>,
    ) -> Result<Self, AnyError> {
        let source_bytes =
            source_paths
                .iter()
                .try_fold(0_u64, |total, encoded| -> Result<u64, AnyError> {
                    let relative = super::path_from_git_bytes(encoded)?;
                    match std::fs::symlink_metadata(root.join(relative)) {
                        Ok(metadata)
                            if metadata.file_type().is_symlink() || !metadata.is_file() =>
                        {
                            Err(
                                "Directional execution source contains a symlink or non-file"
                                    .into(),
                            )
                        }
                        Ok(metadata) => total
                            .checked_add(metadata.len())
                            .ok_or_else(|| "Directional source byte count overflowed".into()),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(total),
                        Err(error) => Err(error.into()),
                    }
                })?;
        let nonce = random_nonce()?;
        let paths = NamespacePaths::for_nonce(&nonce)?;
        let tools = TrustedTools::discover(lean.map(|fixture| fixture.lake.as_path()))?;
        let toolchain_root = trusted_toolchain_root()?;
        let (toolchain_sha256, toolchain_bytes, toolchain_inodes) = tree_identity(&toolchain_root)?;
        let mathlib = lean
            .map(|fixture| tree_snapshot(&fixture.mathlib))
            .transpose()?;
        let mathlib_bytes = mathlib.as_ref().map_or(0, |identity| identity.bytes);
        let mathlib_inodes = mathlib.as_ref().map_or(0, |identity| identity.inodes);
        let source_and_tool_bytes = tools
            .tools
            .values()
            .try_fold(source_bytes, |total, tool| {
                total
                    .checked_add(tool.identity.size)
                    .ok_or("Directional tool byte count overflowed")
            })?
            .checked_add(toolchain_bytes)
            .and_then(|bytes| bytes.checked_add(mathlib_bytes))
            .ok_or("Directional private tree byte count overflowed")?;
        let jobs = std::thread::available_parallelism()?
            .get()
            .saturating_sub(1)
            .max(1);
        let contract = IsolationContract::new(
            current_user_namespace()?,
            paths,
            FilesystemBounds::for_snapshot(
                source_and_tool_bytes,
                u64::try_from(source_paths.len())
                    .unwrap_or(u64::MAX)
                    .saturating_add(toolchain_inodes)
                    .saturating_add(mathlib_inodes),
            )?,
            tools.identities(),
            toolchain_sha256,
            mathlib.clone(),
            jobs,
        )?;
        Ok(Self {
            contract,
            root: root.to_path_buf(),
            source_paths,
            expected_snapshot,
            tools,
            populate_cargo_home: true,
            populate_toolchain: true,
            toolchain_root,
            mathlib_root: lean.map(|fixture| fixture.mathlib.clone()),
        })
    }

    pub(super) fn source_root(&self) -> &Path {
        &self.contract.paths.source
    }

    pub(super) fn contract(&self) -> &IsolationContract {
        &self.contract
    }

    pub(super) fn lean_fixture(&self) -> Option<super::LeanFixture> {
        self.contract.mathlib.as_ref()?;
        Some(super::LeanFixture {
            lake: self
                .contract
                .paths
                .source
                .join(".directional-tools/bin/lake"),
            mathlib: self.contract.paths.source.join(".directional-mathlib"),
        })
    }

    pub(super) fn cargo_label(&self) -> Result<OsString, AnyError> {
        let cargo = self.tools.executable("cargo")?;
        Ok(format!("sealed-cargo:{}", cargo.identity.content_sha256).into())
    }

    pub(super) fn environment(&self) -> Vec<(OsString, OsString)> {
        self.contract
            .environment
            .iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect()
    }

    pub(super) fn execute(self, gates: &[GateSpec]) -> Result<NamespaceOutcome, AnyError> {
        self.contract.validate()?;
        let cargo_label = self.cargo_label()?;
        let tool_fds = self
            .tools
            .tools
            .iter()
            .map(|(role, tool)| (role.clone(), tool.file.as_raw_fd()))
            .collect();
        let mut plan = NamespacePlan {
            contract: self.contract,
            repository_root: self.root,
            source_paths: self.source_paths,
            expected_snapshot: self.expected_snapshot,
            tool_fds,
            gates: gates
                .iter()
                .map(|gate| wire_gate(gate, &cargo_label))
                .collect::<Result<Vec<_>, _>>()?,
            populate_cargo_home: self.populate_cargo_home,
            populate_toolchain: self.populate_toolchain,
            toolchain_root: self.toolchain_root,
            mathlib_root: self.mathlib_root,
            content_sha256: String::new(),
        };
        plan.content_sha256 = hash_json(&plan)?;
        let plan_file = sealed_data("reflex-directional-plan", &serde_json::to_vec(&plan)?)?;
        let plan_fd = plan_file.as_raw_fd();
        let _cleanup = NamespaceRootCleanup(plan.contract.paths.root.clone());
        let unshare = self.tools.descriptor_path("unshare")?;
        let xtask = self.tools.descriptor_path("xtask")?;
        let arguments = vec![
            "--user".into(),
            "--map-root-user".into(),
            "--mount".into(),
            "--fork".into(),
            "--kill-child=KILL".into(),
            "--propagation".into(),
            "private".into(),
            xtask.into_os_string(),
            SETUP_COMMAND.into(),
            "--plan-fd".into(),
            plan_fd.to_string().into(),
        ];
        let capture = capture_child_bounded_clean(
            &unshare,
            &arguments,
            &plan.contract.paths.root.with_extension("evidence"),
            Duration::from_secs(super::MAX_TOTAL_TIMEOUT_SECONDS),
            INNER_RESIDENT_LIMIT,
            &[],
        )?;
        if !capture.status.success()
            || capture.timed_out
            || capture.resident_limit_exceeded
            || capture.output_limit_exceeded
            || capture.boundary.cleanup_failed
            || capture.boundary.evidence_failed
        {
            return Err(format!(
                "Directional namespace runner failed: status={:?}, timeout={}, resident={}, output={}, stderr={}",
                capture.status.code(),
                capture.timed_out,
                capture.resident_limit_exceeded,
                capture.output_limit_exceeded,
                capture.stderr
            )
            .into());
        }
        let encoded = capture
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix(INNER_OUTPUT_MARKER))
            .ok_or("Directional namespace runner omitted its bounded outcome")?;
        serde_json::from_str(encoded).map_err(Into::into)
    }
}

fn wire_gate(gate: &GateSpec, cargo: &OsStr) -> Result<WireGate, AnyError> {
    Ok(WireGate {
        name: gate.name.into(),
        command: super::display_command(cargo, &gate.arguments),
        arguments: utf8_values(&gate.arguments, "gate arguments")?,
        environment: gate
            .environment
            .iter()
            .map(|(name, value)| {
                Ok((
                    name.to_str()
                        .ok_or("Directional environment name is not UTF-8")?
                        .to_owned(),
                    value
                        .to_str()
                        .ok_or("Directional environment value is not UTF-8")?
                        .to_owned(),
                ))
            })
            .collect::<Result<Vec<_>, AnyError>>()?,
        timeout_seconds: gate.timeout.as_secs(),
        resident_limit_bytes: gate.resident_limit_bytes,
        blocker: gate.blocker.clone(),
    })
}

fn utf8_values(values: &[OsString], label: &str) -> Result<Vec<String>, AnyError> {
    values
        .iter()
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("Directional {label} are not UTF-8").into())
        })
        .collect()
}

fn trusted_system_tool(name: &str) -> Result<PathBuf, AnyError> {
    [Path::new("/usr/bin"), Path::new("/bin")]
        .into_iter()
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
        .ok_or_else(|| format!("trusted system tool {name} is unavailable at a fixed path").into())
}

fn trusted_toolchain_root() -> Result<PathBuf, AnyError> {
    let home = host_home_directory()?;
    let toolchains = home.join(".rustup/toolchains");
    let mut matches = std::fs::read_dir(&toolchains)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("1.97."))
                && path.join("bin/cargo").is_file()
                && path.join("bin/rustc").is_file()
        })
        .collect::<Vec<_>>();
    matches.sort();
    if matches.len() != 1 {
        return Err(format!(
            "expected exactly one fixed Rust 1.97 toolchain under {}, found {}",
            toolchains.display(),
            matches.len()
        )
        .into());
    }
    std::fs::canonicalize(matches.pop().expect("the exact toolchain exists")).map_err(Into::into)
}

fn host_home_directory() -> Result<PathBuf, AnyError> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    let uid = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))
        .and_then(|line| line.split_ascii_whitespace().next())
        .ok_or("Linux process status omits its real UID")?;
    let passwd = std::fs::read_to_string("/etc/passwd")?;
    let mut homes = passwd.lines().filter_map(|line| {
        let fields = line.split(':').collect::<Vec<_>>();
        (fields.len() >= 6 && fields[2] == uid).then(|| PathBuf::from(fields[5]))
    });
    let home = homes
        .next()
        .ok_or("the current Linux UID has no passwd home directory")?;
    if homes.next().is_some() || !home.is_absolute() {
        return Err("the current Linux UID has an ambiguous passwd home directory".into());
    }
    Ok(home)
}

fn tree_identity(root: &Path) -> Result<(String, u64, u64), AnyError> {
    fn visit(root: &Path, path: &Path, digest: &mut sha2::Sha256) -> Result<(u64, u64), AnyError> {
        let mut entries = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        let mut bytes = 0_u64;
        let mut inodes = 1_u64;
        for entry in entries {
            let file_type = entry.file_type()?;
            let relative = entry.path().strip_prefix(root)?.to_path_buf();
            let encoded = relative
                .to_str()
                .ok_or("Directional toolchain path is not UTF-8")?;
            digest.update((encoded.len() as u64).to_le_bytes());
            digest.update(encoded.as_bytes());
            if file_type.is_symlink() {
                return Err(format!(
                    "Directional tree identity rejected symlink {}",
                    entry.path().display()
                )
                .into());
            }
            if file_type.is_dir() {
                digest.update(b"directory");
                let (child_bytes, child_inodes) = visit(root, &entry.path(), digest)?;
                bytes = bytes
                    .checked_add(child_bytes)
                    .ok_or("tree bytes overflowed")?;
                inodes = inodes
                    .checked_add(child_inodes)
                    .ok_or("tree inodes overflowed")?;
            } else if file_type.is_file() {
                digest.update(b"file");
                let metadata = entry.metadata()?;
                digest.update(if metadata.permissions().mode() & 0o111 == 0 {
                    b"100644"
                } else {
                    b"100755"
                });
                let length = metadata.len();
                digest.update(length.to_le_bytes());
                let mut file = std::fs::File::open(entry.path())?;
                std::io::copy(&mut file, &mut digest_writer(digest))?;
                bytes = bytes.checked_add(length).ok_or("tree bytes overflowed")?;
                inodes = inodes.checked_add(1).ok_or("tree inodes overflowed")?;
            } else {
                return Err("Directional tree identity rejected a non-file".into());
            }
        }
        Ok((bytes, inodes))
    }

    let metadata = std::fs::symlink_metadata(root)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("Directional tree shape rejected {}", root.display()).into());
    }
    let mut digest = sha2::Sha256::new();
    digest.update(b"reflex-directional-toolchain-v1\0");
    let (bytes, inodes) = visit(root, root, &mut digest)?;
    Ok((crate::harness::hex(&digest.finalize()), bytes, inodes))
}

fn tree_snapshot(root: &Path) -> Result<TreeIdentity, AnyError> {
    let canonical_root = std::fs::canonicalize(root)?;
    let (manifest_sha256, bytes, inodes) = tree_identity(&canonical_root)?;
    Ok(TreeIdentity {
        canonical_root,
        manifest_sha256,
        bytes,
        inodes,
    })
}

fn validate_paths(paths: &NamespacePaths) -> Result<(), AnyError> {
    let values = [
        paths.root.as_path(),
        paths.source.as_path(),
        paths.build.as_path(),
        paths.target.as_path(),
        paths.temporary.as_path(),
        paths.cargo_home.as_path(),
    ];
    if values.iter().any(|path| !path.is_absolute())
        || paths.source == paths.build
        || !paths.source.starts_with(&paths.root)
        || !paths.build.starts_with(&paths.root)
        || !paths.target.starts_with(&paths.build)
        || !paths.temporary.starts_with(&paths.build)
        || !paths.cargo_home.starts_with(&paths.build)
    {
        return Err("Directional namespace paths do not form the exact isolated layout".into());
    }
    Ok(())
}

fn open_directory_nofollow(path: &Path) -> Result<std::fs::File, AnyError> {
    #[cfg(target_arch = "aarch64")]
    const O_DIRECTORY: i32 = 0x4_000;
    #[cfg(not(target_arch = "aarch64"))]
    const O_DIRECTORY: i32 = 0x1_0000;
    #[cfg(target_arch = "aarch64")]
    const O_NOFOLLOW: i32 = 0x8_000;
    #[cfg(not(target_arch = "aarch64"))]
    const O_NOFOLLOW: i32 = 0x2_0000;

    use nix::fcntl::{FcntlArg, FdFlag, fcntl};

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_DIRECTORY | O_NOFOLLOW)
        .open(path)?;
    let flags = FdFlag::from_bits_truncate(fcntl(&file, FcntlArg::F_GETFD)?);
    fcntl(&file, FcntlArg::F_SETFD(flags - FdFlag::FD_CLOEXEC))?;
    Ok(file)
}

fn require_campaign_root_identity(plan: &CampaignSourcePlan) -> Result<(), AnyError> {
    let descriptor = std::fs::File::open(format!("/proc/self/fd/{}", plan.root_descriptor))?;
    let descriptor_metadata = descriptor.metadata()?;
    let path_metadata = std::fs::symlink_metadata(&plan.paths.root)?;
    if !descriptor_metadata.is_dir()
        || !path_metadata.is_dir()
        || descriptor_metadata.dev() != plan.root_device
        || descriptor_metadata.ino() != plan.root_inode
        || path_metadata.dev() != plan.root_device
        || path_metadata.ino() != plan.root_inode
    {
        return Err("Campaign source root identity changed".into());
    }
    Ok(())
}

pub(super) fn validate_source_paths(paths: &[Vec<u8>]) -> Result<(), AnyError> {
    use std::path::Component;

    let malformed = paths.iter().any(|encoded| {
        let Ok(path) = super::path_from_git_bytes(encoded) else {
            return true;
        };
        !path.is_relative()
            || path
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
    });
    if paths.is_empty() || paths.windows(2).any(|pair| pair[0] >= pair[1]) || malformed {
        return Err("Directional source manifest is empty, unsorted, duplicate, or unsafe".into());
    }
    Ok(())
}

fn validate_tools(tools: &[ToolIdentity]) -> Result<(), AnyError> {
    const REQUIRED: [&str; 6] = ["cargo", "mount", "rustc", "setpriv", "unshare", "xtask"];
    let mut roles = tools
        .iter()
        .map(|tool| tool.role.as_str())
        .collect::<Vec<_>>();
    roles.sort_unstable();
    roles.dedup();
    if !REQUIRED
        .iter()
        .all(|required| roles.binary_search(required).is_ok())
        || roles.len() != tools.len()
        || tools.iter().any(|tool| {
            tool.identity.content_sha256.is_empty()
                || tool.identity.size == 0
                || !Path::new(&tool.identity.canonical_path).is_absolute()
        })
    {
        return Err("Directional namespace tool identities are incomplete or ambiguous".into());
    }
    Ok(())
}

pub(super) fn exact_gate_environment(paths: &NamespacePaths, jobs: usize) -> Vec<(String, String)> {
    let toolchain = paths.source.join(".directional-toolchain");
    let path = format!(
        "{}:{}",
        toolchain.join("bin").display(),
        paths.source.join(".directional-tools/bin").display()
    );
    vec![
        ("CARGO_BUILD_JOBS".into(), jobs.to_string()),
        ("CARGO_HOME".into(), paths.cargo_home.display().to_string()),
        ("CARGO_NET_OFFLINE".into(), "true".into()),
        (
            "CARGO_TARGET_DIR".into(),
            paths.target.display().to_string(),
        ),
        ("CARGO_TERM_COLOR".into(), "never".into()),
        (
            "HOME".into(),
            paths.build.join("home").display().to_string(),
        ),
        ("PATH".into(), path),
        (
            "RUSTC".into(),
            toolchain.join("bin/rustc").display().to_string(),
        ),
        (
            "RUSTDOC".into(),
            toolchain.join("bin/rustdoc").display().to_string(),
        ),
        (
            "RUSTFLAGS".into(),
            format!("--sysroot={}", toolchain.display()),
        ),
        (
            "RUSTDOCFLAGS".into(),
            format!("--sysroot={}", toolchain.display()),
        ),
        ("RUST_BACKTRACE".into(), "0".into()),
        ("RUST_TEST_THREADS".into(), "1".into()),
        ("TMPDIR".into(), paths.temporary.display().to_string()),
    ]
}

fn environment_map(environment: &[(String, String)]) -> Result<BTreeMap<&str, &str>, AnyError> {
    let mut map = BTreeMap::new();
    for (name, value) in environment {
        if name.is_empty()
            || value.is_empty()
            || map.insert(name.as_str(), value.as_str()).is_some()
        {
            return Err("Directional environment contains an empty or duplicate entry".into());
        }
    }
    Ok(map)
}

#[cfg(test)]
pub(super) fn tmpfs_arguments(
    path: &Path,
    bytes: u64,
    inodes: u64,
    executable: bool,
) -> Vec<OsString> {
    let mut options = format!("size={bytes},nr_inodes={inodes},mode=0700,uid=0,gid=0,nosuid,nodev");
    options.push_str(if executable { ",exec" } else { ",noexec" });
    [
        OsStr::new("-n"),
        OsStr::new("--internal-only"),
        OsStr::new("-t"),
        OsStr::new("tmpfs"),
        OsStr::new("-o"),
        OsStr::new(&options),
        OsStr::new("tmpfs"),
        path.as_os_str(),
    ]
    .into_iter()
    .map(OsStr::to_owned)
    .collect()
}

fn mount_tmpfs(path: &Path, bytes: u64, inodes: u64, executable: bool) -> Result<(), AnyError> {
    use nix::mount::{MsFlags, mount};

    let data = format!("size={bytes},nr_inodes={inodes},mode=0700,uid=0,gid=0");
    let mut flags = MsFlags::MS_NOSUID | MsFlags::MS_NODEV;
    if !executable {
        flags |= MsFlags::MS_NOEXEC;
    }
    mount(
        Some("tmpfs"),
        path,
        Some("tmpfs"),
        flags,
        Some(data.as_str()),
    )?;
    Ok(())
}

fn remount_readonly(path: &Path) -> Result<(), AnyError> {
    use nix::mount::{MsFlags, mount};

    mount::<str, Path, str, str>(
        None,
        path,
        None,
        MsFlags::MS_REMOUNT | MsFlags::MS_RDONLY | MsFlags::MS_NOSUID | MsFlags::MS_NODEV,
        None,
    )?;
    Ok(())
}

#[cfg(test)]
pub(super) fn readonly_remount_arguments(path: &Path) -> Vec<OsString> {
    [
        OsStr::new("-n"),
        OsStr::new("--internal-only"),
        OsStr::new("-o"),
        OsStr::new("remount,ro,nosuid,nodev,exec"),
        path.as_os_str(),
    ]
    .into_iter()
    .map(OsStr::to_owned)
    .collect()
}

fn readonly_remount_rw_arguments(path: &Path) -> Vec<OsString> {
    [
        OsStr::new("-n"),
        OsStr::new("--internal-only"),
        OsStr::new("-o"),
        OsStr::new("remount,rw"),
        path.as_os_str(),
    ]
    .into_iter()
    .map(OsStr::to_owned)
    .collect()
}

pub(super) fn setpriv_arguments(
    contract: &IsolationContract,
    xtask: &Path,
    plan_fd: i32,
) -> Vec<OsString> {
    let writable = contract.paths.build.display();
    vec![
        "--nnp".into(),
        "--bounding-set=-all".into(),
        "--inh-caps=-all".into(),
        "--ambient-caps=-all".into(),
        "--securebits=+noroot,+noroot_locked,+no_setuid_fixup,+no_setuid_fixup_locked".into(),
        "--landlock-access".into(),
        "fs".into(),
        "--landlock-rule".into(),
        "path-beneath:execute,read-file,read-dir:/".into(),
        "--landlock-rule".into(),
        format!(
            "path-beneath:write-file,remove-dir,remove-file,make-dir,make-reg,make-sock,make-fifo,make-sym,refer,truncate:{writable}"
        )
        .into(),
        xtask.as_os_str().to_owned(),
        GATES_COMMAND.into(),
        "--plan-fd".into(),
        plan_fd.to_string().into(),
    ]
}

pub(crate) fn run_setup(arguments: &[String]) -> Result<(), AnyError> {
    let plan_fd = parse_plan_fd(arguments)?;
    let plan = read_plan(plan_fd)?;
    require_setup_authority(&plan)?;
    create_mount_layout(&plan.contract.paths)?;
    mount_tmpfs(
        &plan.contract.paths.source,
        plan.contract.bounds.source_bytes,
        plan.contract.bounds.source_inodes,
        true,
    )?;
    mount_tmpfs(
        &plan.contract.paths.build,
        plan.contract.bounds.build_bytes,
        plan.contract.bounds.build_inodes,
        true,
    )?;
    create_build_layout(&plan.contract.paths)?;
    if plan.populate_cargo_home {
        populate_cargo_home(&plan)?;
    }
    copy_repository_source(&plan)?;
    if let (Some(mathlib_root), Some(expected)) =
        (plan.mathlib_root.as_ref(), plan.contract.mathlib.as_ref())
    {
        let destination = plan.contract.paths.source.join(".directional-mathlib");
        copy_tree_nofollow(mathlib_root, &destination)?;
        let observed = tree_snapshot(&destination)?;
        if observed.manifest_sha256 != expected.manifest_sha256
            || observed.bytes != expected.bytes
            || observed.inodes != expected.inodes
        {
            return Err("private Directional mathlib failed exact verification".into());
        }
    }
    if plan.populate_toolchain {
        copy_tree_nofollow(
            &plan.toolchain_root,
            &plan.contract.paths.source.join(".directional-toolchain"),
        )?;
        let (observed, _, _) =
            tree_identity(&plan.contract.paths.source.join(".directional-toolchain"))?;
        if observed != plan.contract.toolchain_sha256 {
            return Err("private Directional toolchain failed exact verification".into());
        }
    }
    install_pinned_tools(&plan)?;
    require_source_unchanged(&plan)?;
    remount_readonly(&plan.contract.paths.source)?;
    attest_mounts(&plan.contract)?;

    let filter = directional_namespace_seccomp_filter()?;
    require_sealed_memfd(&filter)?;
    let setpriv = tool_path(&plan, "setpriv")?;
    let xtask = tool_path(&plan, "xtask")?;
    let mut boundary_arguments = setpriv_arguments(&plan.contract, &xtask, plan_fd);
    boundary_arguments.insert(0, pinned_fd_path(&filter).into_os_string());
    boundary_arguments.insert(0, "--seccomp-filter".into());
    let mut command = Command::new(setpriv);
    command.args(boundary_arguments).env_clear();
    for (name, value) in &plan.contract.environment {
        command.env(name, value);
    }
    let error = command.exec();
    Err(format!("sealed setpriv could not enter the Directional gate boundary: {error}").into())
}

pub(crate) fn run_gates(arguments: &[String]) -> Result<(), AnyError> {
    let plan_fd = parse_plan_fd(arguments)?;
    let plan = read_plan(plan_fd)?;
    require_gate_authority(&plan)?;
    require_escape_denied(&plan)?;
    let cargo = tool_path(&plan, "cargo")?;
    let work = plan.contract.paths.build.join("evidence");
    std::fs::create_dir(&work)?;
    let started = std::time::Instant::now();
    let mut halted = false;
    let mut receipts = Vec::with_capacity(plan.gates.len());
    for wire in &plan.gates {
        let gate = gate_from_wire(wire)?;
        let remaining =
            Duration::from_secs(super::MAX_TOTAL_TIMEOUT_SECONDS).saturating_sub(started.elapsed());
        let mut receipt = if let Some(detail) = gate.blocker.as_ref() {
            super::blocked_receipt(cargo.as_os_str(), &gate, detail)
        } else if halted {
            super::not_run_receipt(cargo.as_os_str(), &gate)
        } else if remaining.is_zero() {
            super::global_timeout_receipt(cargo.as_os_str(), &gate)
        } else {
            super::execute_gate(cargo.as_os_str(), &gate, &work, gate.timeout.min(remaining))
        };
        if let Err(error) = require_source_unchanged(&plan) {
            receipt.status = super::GateStatus::Failed;
            receipt.detail = Some(format!(
                "private read-only Directional source lost integrity: {error}"
            ));
        }
        if let Err(error) = attest_gate_boundary(&plan.contract) {
            receipt.status = super::GateStatus::Failed;
            receipt.detail = Some(format!("Directional gate boundary changed: {error}"));
        }
        receipt.command.clone_from(&wire.command);
        halted |= receipt.status != super::GateStatus::Passed;
        receipts.push(receipt);
    }
    require_source_unchanged(&plan)?;
    attest_gate_boundary(&plan.contract)?;
    let outcome = NamespaceOutcome {
        execution_snapshot: super::source_snapshot_for_paths(
            &plan.contract.paths.source,
            &plan.source_paths,
        )?,
        gates: receipts,
    };
    println!("{INNER_OUTPUT_MARKER}{}", serde_json::to_string(&outcome)?);
    Ok(())
}

pub(crate) fn run_probe(arguments: &[String]) -> Result<(), AnyError> {
    if arguments.is_empty() {
        Ok(())
    } else {
        Err("Directional namespace probe accepts no arguments".into())
    }
}

pub(crate) fn run_smoke(arguments: &[String]) -> Result<(), AnyError> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    if !arguments.is_empty() {
        return Err("Directional namespace smoke accepts no arguments".into());
    }
    let root = super::repository_root()?;
    let source_paths = vec![b"Cargo.toml".to_vec()];
    let expected_snapshot = super::source_snapshot_for_paths(&root, &source_paths)?;
    let mut prepared =
        PreparedNamespace::prepare(&root, source_paths, expected_snapshot.clone(), None)?;
    prepared.populate_cargo_home = false;
    prepared.populate_toolchain = false;
    let gate = GateSpec {
        name: "namespace-security-smoke",
        arguments: vec!["--version".into()],
        environment: prepared.environment(),
        timeout: Duration::from_secs(20),
        resident_limit_bytes: 512 * 1024 * 1024,
        blocker: None,
    };
    let external_source = prepared.source_root().join("Cargo.toml");
    let stop = Arc::new(AtomicBool::new(false));
    let attempts = Arc::new(AtomicU64::new(0));
    let attack_stop = Arc::clone(&stop);
    let attack_attempts = Arc::clone(&attempts);
    let attacker = std::thread::spawn(move || {
        while !attack_stop.load(Ordering::Acquire) {
            if external_source.parent().is_some_and(Path::is_dir) {
                let _ = std::fs::write(&external_source, b"hostile transient replacement");
                attack_attempts.fetch_add(1, Ordering::Relaxed);
            }
            std::thread::yield_now();
        }
    });
    let result = prepared.execute(&[gate]);
    stop.store(true, Ordering::Release);
    attacker
        .join()
        .map_err(|_| "Directional external mutation probe panicked")?;
    let outcome = result?;
    if outcome.execution_snapshot != expected_snapshot
        || outcome.gates.len() != 1
        || outcome.gates[0].status != super::GateStatus::Passed
        || attempts.load(Ordering::Relaxed) == 0
    {
        return Err("Directional namespace security smoke did not pass exactly".into());
    }
    Ok(())
}

fn parse_plan_fd(arguments: &[String]) -> Result<i32, AnyError> {
    if let [flag, value] = arguments
        && flag == "--plan-fd"
    {
        let descriptor = value.parse::<i32>()?;
        if descriptor >= 3 {
            return Ok(descriptor);
        }
    }
    Err("Directional namespace command requires one inherited plan descriptor".into())
}

fn read_plan(descriptor: i32) -> Result<NamespacePlan, AnyError> {
    let path = PathBuf::from(format!("/proc/self/fd/{descriptor}"));
    let metadata = std::fs::metadata(&path)?;
    if !metadata.is_file() || metadata.len() > 8 * 1024 * 1024 {
        return Err("Directional namespace plan is not a bounded regular descriptor".into());
    }
    let mut plan = serde_json::from_slice::<NamespacePlan>(&std::fs::read(path)?)?;
    let claimed = std::mem::take(&mut plan.content_sha256);
    if claimed.is_empty() || hash_json(&plan)? != claimed {
        return Err("Directional namespace plan identity is invalid".into());
    }
    plan.content_sha256 = claimed;
    plan.contract.validate()?;
    validate_tool_descriptors(&plan)?;
    validate_wire_gates(&plan)?;
    Ok(plan)
}

fn validate_wire_gates(plan: &NamespacePlan) -> Result<(), AnyError> {
    let cargo = plan
        .contract
        .tools
        .iter()
        .find(|tool| tool.role == "cargo")
        .ok_or("Directional contract omits Cargo")?;
    let cargo_label = format!("sealed-cargo:{}", cargo.identity.content_sha256);
    let base = &plan.contract.environment;
    for gate in &plan.gates {
        if gate.name.is_empty()
            || gate.command.first() != Some(&cargo_label)
            || gate.arguments.is_empty()
            || gate.timeout_seconds == 0
            || gate.resident_limit_bytes == 0
        {
            return Err("Directional namespace gate plan is incomplete".into());
        }
        let mut expected = base.clone();
        if gate.name == "tiny-lean-kernel-smoke" && plan.contract.mathlib.is_some() {
            expected.extend([
                (
                    "REFLEX_LEAN_LAKE".into(),
                    plan.contract
                        .paths
                        .source
                        .join(".directional-tools/bin/lake")
                        .display()
                        .to_string(),
                ),
                (
                    "REFLEX_LEAN_MATHLIB".into(),
                    plan.contract
                        .paths
                        .source
                        .join(".directional-mathlib")
                        .display()
                        .to_string(),
                ),
            ]);
        }
        if gate.environment != expected {
            return Err("Directional gate environment is not the exact allowlist".into());
        }
    }
    Ok(())
}

fn validate_tool_descriptors(plan: &NamespacePlan) -> Result<(), AnyError> {
    if plan.tool_fds.len() != plan.contract.tools.len() {
        return Err("Directional namespace plan has an incomplete tool descriptor set".into());
    }
    for expected in &plan.contract.tools {
        let descriptor = *plan
            .tool_fds
            .get(&expected.role)
            .ok_or("Directional namespace plan omits a tool descriptor")?;
        if descriptor < 3 {
            return Err("Directional namespace tool descriptor is not inherited".into());
        }
        let mut file = std::fs::File::open(format!("/proc/self/fd/{descriptor}"))?;
        require_sealed_memfd(&file)?;
        let metadata = file.metadata()?;
        let mut digest = sha2::Sha256::new();
        std::io::copy(&mut file, &mut digest_writer(&mut digest))?;
        file.seek(SeekFrom::Start(0))?;
        if crate::harness::hex(&digest.finalize()) != expected.identity.content_sha256
            || metadata.len() != expected.identity.size
            || metadata.permissions().mode() & 0o111 == 0
        {
            return Err(format!(
                "Directional namespace tool {} does not match its sealed identity",
                expected.role
            )
            .into());
        }
    }
    let rustc = plan
        .contract
        .tools
        .iter()
        .find(|tool| tool.role == "rustc")
        .ok_or("Directional contract omits rustc")?;
    let expected_root = Path::new(&rustc.identity.canonical_path)
        .parent()
        .and_then(Path::parent)
        .ok_or("Directional rustc identity has no toolchain root")?;
    if plan.toolchain_root != expected_root {
        return Err("Directional private toolchain root is not bound to rustc".into());
    }
    if plan.mathlib_root.as_ref()
        != plan
            .contract
            .mathlib
            .as_ref()
            .map(|tree| &tree.canonical_root)
    {
        return Err("Directional private mathlib root is not bound to its exact identity".into());
    }
    Ok(())
}

fn validate_pinned_descriptor(
    descriptor: i32,
    expected: &ExecutableIdentity,
) -> Result<(), AnyError> {
    if descriptor < 3 {
        return Err("Directional pinned executable descriptor is invalid".into());
    }
    let mut file = std::fs::File::open(format!("/proc/self/fd/{descriptor}"))?;
    require_sealed_memfd(&file)?;
    let metadata = file.metadata()?;
    let mut digest = sha2::Sha256::new();
    std::io::copy(&mut file, &mut digest_writer(&mut digest))?;
    if crate::harness::hex(&digest.finalize()) != expected.content_sha256
        || metadata.len() != expected.size
        || metadata.permissions().mode() & 0o111 == 0
    {
        return Err("Directional pinned executable descriptor identity changed".into());
    }
    Ok(())
}

fn digest_writer(digest: &mut sha2::Sha256) -> impl std::io::Write + '_ {
    struct DigestWriter<'a>(&'a mut sha2::Sha256);
    impl std::io::Write for DigestWriter<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            use sha2::Digest as _;
            self.0.update(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    DigestWriter(digest)
}

fn tool_path(plan: &NamespacePlan, role: &str) -> Result<PathBuf, AnyError> {
    let descriptor = plan
        .tool_fds
        .get(role)
        .ok_or_else(|| format!("Directional namespace plan omits tool role {role}"))?;
    Ok(PathBuf::from(format!("/proc/self/fd/{descriptor}")))
}

fn require_setup_authority(plan: &NamespacePlan) -> Result<(), AnyError> {
    if current_user_namespace()? == plan.contract.outer_user_namespace {
        return Err("Directional setup did not enter a private user namespace".into());
    }
    let uid_map = std::fs::read_to_string("/proc/self/uid_map")?;
    let mappings = uid_map
        .lines()
        .map(|line| line.split_ascii_whitespace().collect::<Vec<_>>())
        .collect::<Vec<_>>();
    if mappings.len() != 1
        || mappings[0].len() != 3
        || mappings[0][0] != "0"
        || mappings[0][2] != "1"
    {
        return Err("Directional setup user namespace is not exactly mapped".into());
    }
    Ok(())
}

fn require_gate_authority(plan: &NamespacePlan) -> Result<(), AnyError> {
    require_setup_authority(plan)?;
    attest_gate_boundary(&plan.contract)
}

fn require_escape_denied(plan: &NamespacePlan) -> Result<(), AnyError> {
    let xtask = tool_path(plan, "xtask")?;
    require_command_denied(
        &tool_path(plan, "unshare")?,
        &[
            OsString::from("--user"),
            OsString::from("--map-root-user"),
            xtask.into_os_string(),
            OsString::from(PROBE_COMMAND),
        ],
        "nested user namespace",
    )?;
    require_command_denied(
        &tool_path(plan, "mount")?,
        &readonly_remount_rw_arguments(&plan.contract.paths.source),
        "source remount",
    )?;
    let outside = Path::new("/tmp").join(format!(
        ".reflex-directional-landlock-probe-{}",
        std::process::id()
    ));
    if std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&outside)
        .is_ok()
    {
        let _ = std::fs::remove_file(&outside);
        return Err("Directional Landlock boundary permits writes outside build tmpfs".into());
    }
    let inside = plan.contract.paths.build.join("landlock-write-probe");
    std::fs::write(&inside, b"bounded")?;
    std::fs::remove_file(inside)?;
    Ok(())
}

fn require_command_denied(
    executable: &Path,
    arguments: &[OsString],
    capability: &str,
) -> Result<(), AnyError> {
    let output = Command::new(executable)
        .args(arguments)
        .env_clear()
        .output()?;
    if output.status.success() {
        return Err(format!("Directional boundary permits {capability}").into());
    }
    if output.stdout.len() > 64 * 1024 || output.stderr.len() > 64 * 1024 {
        return Err(format!("Directional {capability} probe exceeded its output bound").into());
    }
    Ok(())
}

fn attest_gate_boundary(contract: &IsolationContract) -> Result<(), AnyError> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    for required in [
        "NoNewPrivs:\t1",
        "CapInh:\t0000000000000000",
        "CapPrm:\t0000000000000000",
        "CapEff:\t0000000000000000",
        "CapBnd:\t0000000000000000",
        "CapAmb:\t0000000000000000",
    ] {
        if !status.lines().any(|line| line == required) {
            return Err(format!("process status omits {required}").into());
        }
    }
    attest_mounts(contract)?;
    require_write_denied(&contract.paths.source)
}

fn create_mount_layout(paths: &NamespacePaths) -> Result<(), AnyError> {
    std::fs::create_dir(&paths.root)?;
    std::fs::set_permissions(&paths.root, std::fs::Permissions::from_mode(0o700))?;
    std::fs::create_dir(&paths.source)?;
    std::fs::create_dir(&paths.build)?;
    Ok(())
}

fn create_build_layout(paths: &NamespacePaths) -> Result<(), AnyError> {
    for path in [
        paths.target.clone(),
        paths.temporary.clone(),
        paths.cargo_home.clone(),
        paths.build.join("home"),
    ] {
        std::fs::create_dir(path)?;
    }
    Ok(())
}

fn populate_cargo_home(plan: &NamespacePlan) -> Result<(), AnyError> {
    let registry = host_home_directory()?.join(".cargo/registry");
    if !registry.is_dir() {
        return Err("the fixed Cargo registry cache is unavailable".into());
    }
    let packages = cargo_lock_packages(&plan.repository_root.join("Cargo.lock"))?;
    let destination = plan.contract.paths.cargo_home.join("registry");
    std::fs::create_dir(&destination)?;
    copy_tree_nofollow(&registry.join("index"), &destination.join("index"))?;
    copy_selected_registry_entries(
        &registry.join("src"),
        &destination.join("src"),
        &packages,
        false,
    )?;
    copy_selected_registry_entries(
        &registry.join("cache"),
        &destination.join("cache"),
        &packages,
        true,
    )
}

fn cargo_lock_packages(path: &Path) -> Result<Vec<String>, AnyError> {
    let contents = std::fs::read_to_string(path)?;
    let mut packages = Vec::new();
    let mut name = None;
    for line in contents.lines() {
        if line == "[[package]]" {
            name = None;
        } else if let Some(value) = line
            .strip_prefix("name = \"")
            .and_then(|v| v.strip_suffix('"'))
        {
            name = Some(value.to_owned());
        } else if let Some(version) = line
            .strip_prefix("version = \"")
            .and_then(|value| value.strip_suffix('"'))
            && let Some(name) = name.take()
        {
            packages.push(format!("{name}-{version}"));
        }
    }
    packages.sort();
    packages.dedup();
    if packages.is_empty() {
        return Err("Cargo.lock contains no bounded registry package set".into());
    }
    Ok(packages)
}

fn copy_selected_registry_entries(
    source: &Path,
    destination: &Path,
    packages: &[String],
    archives: bool,
) -> Result<(), AnyError> {
    std::fs::create_dir(destination)?;
    for registry in std::fs::read_dir(source)? {
        let registry = registry?;
        if !registry.file_type()?.is_dir() {
            return Err("Cargo registry cache contains a non-directory root".into());
        }
        let target_registry = destination.join(registry.file_name());
        std::fs::create_dir(&target_registry)?;
        for entry in std::fs::read_dir(registry.path())? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().ok_or("Cargo registry entry is not UTF-8")?;
            let package = name.strip_suffix(".crate").unwrap_or(name);
            if packages
                .binary_search_by(|expected| expected.as_str().cmp(package))
                .is_err()
            {
                continue;
            }
            if archives {
                copy_regular_file_nofollow(&entry.path(), &target_registry.join(name), false)?;
            } else {
                copy_tree_nofollow(&entry.path(), &target_registry.join(name))?;
            }
        }
    }
    Ok(())
}

fn copy_tree_nofollow(source: &Path, destination: &Path) -> Result<(), AnyError> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("Directional tree copy rejected {}", source.display()).into());
    }
    std::fs::create_dir(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            return Err(format!(
                "Directional tree copy rejected symlink {}",
                source_path.display()
            )
            .into());
        }
        if file_type.is_dir() {
            copy_tree_nofollow(&source_path, &destination_path)?;
        } else if file_type.is_file() {
            let executable = entry.metadata()?.permissions().mode() & 0o111 != 0;
            copy_regular_file_nofollow(&source_path, &destination_path, executable)?;
        } else {
            return Err(format!(
                "Directional tree copy rejected non-file {}",
                source_path.display()
            )
            .into());
        }
    }
    Ok(())
}

fn copy_repository_source(plan: &NamespacePlan) -> Result<(), AnyError> {
    copy_manifest_paths(
        &plan.repository_root,
        &plan.contract.paths.source,
        &plan.source_paths,
    )
}

fn copy_manifest_paths(
    source_root: &Path,
    destination_root: &Path,
    source_paths: &[Vec<u8>],
) -> Result<(), AnyError> {
    validate_source_paths(source_paths)?;
    for encoded in source_paths {
        let relative = super::path_from_git_bytes(encoded)?;
        let source = source_root.join(&relative);
        let destination = destination_root.join(&relative);
        match std::fs::symlink_metadata(&source) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err(format!(
                    "Directional execution source contains a symlink or non-file: {}",
                    source.display()
                )
                .into());
            }
            Ok(metadata) => copy_regular_file_nofollow(
                &source,
                &destination,
                metadata.permissions().mode() & 0o111 != 0,
            )?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(&destination).is_ok() {
                    return Err(format!(
                        "Directional deleted source path unexpectedly exists: {}",
                        destination.display()
                    )
                    .into());
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn install_pinned_tools(plan: &NamespacePlan) -> Result<(), AnyError> {
    let directory = plan.contract.paths.source.join(".directional-tools/bin");
    std::fs::create_dir_all(&directory)?;
    for (role, descriptor) in &plan.tool_fds {
        let destination = directory.join(role);
        let expected = plan
            .contract
            .tools
            .iter()
            .find(|tool| tool.role == *role)
            .ok_or("Directional copied tool has no identity")?;
        validate_pinned_descriptor(*descriptor, &expected.identity)?;
        copy_pinned_descriptor(*descriptor, &destination)?;
        if file_sha256(&destination)? != expected.identity.content_sha256 {
            return Err(format!("private Directional tool {role} failed verification").into());
        }
    }
    Ok(())
}

fn copy_pinned_descriptor(descriptor: i32, destination: &Path) -> Result<(), AnyError> {
    let mut input = std::fs::File::open(format!("/proc/self/fd/{descriptor}"))?;
    let expected = input.metadata()?.len();
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o500)
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    if output.metadata()?.len() != expected {
        return Err("Directional sealed descriptor copy changed length".into());
    }
    Ok(())
}

fn file_sha256(path: &Path) -> Result<String, AnyError> {
    let mut file = std::fs::File::open(path)?;
    let mut digest = sha2::Sha256::new();
    std::io::copy(&mut file, &mut digest_writer(&mut digest))?;
    Ok(crate::harness::hex(&digest.finalize()))
}

fn copy_regular_file_nofollow(
    source: &Path,
    destination: &Path,
    executable: bool,
) -> Result<(), AnyError> {
    #[cfg(target_arch = "aarch64")]
    const O_NOFOLLOW: i32 = 0x8000;
    #[cfg(not(target_arch = "aarch64"))]
    const O_NOFOLLOW: i32 = 0x2_0000;

    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!("Directional copy rejected {}", source.display()).into());
    }
    let mut input = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(O_NOFOLLOW)
        .open(source)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(if executable { 0o500 } else { 0o400 })
        .open(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.sync_all()?;
    if input.metadata()?.len() != metadata.len() || output.metadata()?.len() != metadata.len() {
        return Err(format!("Directional copy raced for {}", source.display()).into());
    }
    Ok(())
}

fn attest_mounts(contract: &IsolationContract) -> Result<(), AnyError> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
    let source = mountinfo_line(&mountinfo, &contract.paths.source)?;
    let build = mountinfo_line(&mountinfo, &contract.paths.build)?;
    require_mount_options(source, &["ro", "nosuid", "nodev"])?;
    require_mount_options(build, &["rw", "nosuid", "nodev"])?;
    if !source.contains(" - tmpfs ") || !build.contains(" - tmpfs ") {
        return Err("Directional private filesystems are not tmpfs mounts".into());
    }
    require_root_owned_mount(&contract.paths.source)?;
    require_root_owned_mount(&contract.paths.build)?;
    Ok(())
}

fn attest_campaign_mounts(plan: &CampaignSourcePlan) -> Result<(), AnyError> {
    let mountinfo = std::fs::read_to_string("/proc/self/mountinfo")?;
    let source = mountinfo_line(&mountinfo, &plan.paths.source)?;
    let build = mountinfo_line(&mountinfo, &plan.paths.build)?;
    require_mount_options(source, &["ro", "nosuid", "nodev"])?;
    require_mount_options(build, &["rw", "nosuid", "nodev"])?;
    if !source.contains(" - tmpfs ") || !build.contains(" - tmpfs ") {
        return Err("Directional Campaign private filesystems are not tmpfs mounts".into());
    }
    require_root_owned_mount(&plan.paths.source)?;
    require_root_owned_mount(&plan.paths.build)?;
    Ok(())
}

fn require_root_owned_mount(path: &Path) -> Result<(), AnyError> {
    use std::os::unix::fs::MetadataExt as _;

    let metadata = std::fs::metadata(path)?;
    if metadata.uid() != 0 || metadata.gid() != 0 || metadata.permissions().mode() & 0o777 != 0o700
    {
        return Err(format!(
            "Directional private mount has wrong owner or mode: {}",
            path.display()
        )
        .into());
    }
    Ok(())
}

fn mountinfo_line<'a>(mountinfo: &'a str, path: &Path) -> Result<&'a str, AnyError> {
    let target = path.to_str().ok_or("Directional mount path is not UTF-8")?;
    mountinfo
        .lines()
        .find(|line| line.split_ascii_whitespace().nth(4) == Some(target))
        .ok_or_else(|| format!("Directional mount {} is absent", path.display()).into())
}

fn require_mount_options(line: &str, required: &[&str]) -> Result<(), AnyError> {
    let options = line
        .split_ascii_whitespace()
        .nth(5)
        .ok_or("Directional mount evidence omits options")?;
    if required
        .iter()
        .any(|required| !options.split(',').any(|option| option == *required))
    {
        return Err(format!("Directional mount lacks required options: {line}").into());
    }
    Ok(())
}

fn require_write_denied(source: &Path) -> Result<(), AnyError> {
    let existing = source.join("Cargo.toml");
    let created = source.join(".directional-write-probe");
    let renamed = source.join(".directional-rename-probe");
    if std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(&existing)
        .is_ok()
        || std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(created)
            .is_ok()
        || std::fs::rename(&existing, renamed).is_ok()
        || std::fs::remove_file(existing).is_ok()
    {
        return Err("Directional source mutation unexpectedly succeeded".into());
    }
    Ok(())
}

fn require_source_unchanged(plan: &NamespacePlan) -> Result<(), AnyError> {
    let current =
        super::source_snapshot_for_paths(&plan.contract.paths.source, &plan.source_paths)?;
    if current != plan.expected_snapshot {
        return Err("Directional source snapshot changed".into());
    }
    Ok(())
}

fn gate_from_wire(wire: &WireGate) -> Result<GateSpec, AnyError> {
    if wire.name.is_empty() || wire.timeout_seconds == 0 || wire.resident_limit_bytes == 0 {
        return Err("Directional namespace gate is unbounded".into());
    }
    Ok(GateSpec {
        name: Box::leak(wire.name.clone().into_boxed_str()),
        arguments: wire.arguments.iter().map(Into::into).collect(),
        environment: wire
            .environment
            .iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect(),
        timeout: Duration::from_secs(wire.timeout_seconds),
        resident_limit_bytes: wire.resident_limit_bytes,
        blocker: wire.blocker.clone(),
    })
}

fn sealed_data(name: &str, bytes: &[u8]) -> Result<std::fs::File, AnyError> {
    use nix::fcntl::{FcntlArg, SealFlag, fcntl};
    use nix::sys::memfd::{MFdFlags, memfd_create};

    let descriptor = memfd_create(name, MFdFlags::MFD_ALLOW_SEALING)?;
    let mut file = std::fs::File::from(descriptor);
    file.write_all(bytes)?;
    file.seek(SeekFrom::Start(0))?;
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

fn current_user_namespace() -> Result<String, AnyError> {
    std::fs::read_link("/proc/self/ns/user")?
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| "Linux user namespace identity is not UTF-8".into())
}

fn random_nonce() -> Result<String, AnyError> {
    let mut random = [0_u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    let time = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    Ok(format!(
        "{}-{time:x}-{}",
        std::process::id(),
        crate::harness::hex(&random)
    ))
}

#[cfg(test)]
pub(super) fn fixture_contract(label: &str) -> IsolationContract {
    let paths = NamespacePaths::for_nonce(label).expect("the fixture nonce is canonical");
    IsolationContract::new(
        "user:[4026531837]".into(),
        paths,
        FilesystemBounds::for_snapshot(1024, 8).expect("the fixture bounds are valid"),
        ["cargo", "mount", "rustc", "setpriv", "unshare", "xtask"]
            .into_iter()
            .map(|role| ToolIdentity {
                role: role.into(),
                identity: ExecutableIdentity {
                    canonical_path: format!("/trusted/{role}"),
                    device: 1,
                    inode: 2,
                    mode: 0o100_755,
                    size: 3,
                    changed_seconds: 4,
                    changed_nanoseconds: 5,
                    content_sha256: format!("sha256-{role}"),
                },
            })
            .collect(),
        "fixture-toolchain-sha256".into(),
        None,
        7,
    )
    .expect("the fixture contract is valid")
}

#[cfg(test)]
mod tests {
    use super::{
        IsolationContract, exact_gate_environment, readonly_remount_arguments, setpriv_arguments,
        tmpfs_arguments,
    };
    use std::path::Path;

    fn contract() -> IsolationContract {
        super::fixture_contract("fixture-1")
    }

    #[test]
    fn contract_ignores_hostile_ambient_build_environment() {
        let contract = contract();
        let expected = exact_gate_environment(&contract.paths, 7);
        assert_eq!(contract.environment, expected);
        assert!(!contract.environment.iter().any(|(name, _)| {
            matches!(
                name.as_str(),
                "CARGO_ENCODED_RUSTFLAGS" | "RUSTC_WRAPPER" | "CARGO_TARGET_DIR_EVIL"
            )
        }));
        assert_eq!(
            contract
                .environment
                .iter()
                .find(|(name, _)| name == "PATH")
                .unwrap()
                .1,
            "/tmp/reflex-directional-fixture-1/source/.directional-toolchain/bin:/tmp/reflex-directional-fixture-1/source/.directional-tools/bin"
        );
        assert_eq!(
            contract
                .environment
                .iter()
                .find(|(name, _)| name == "RUSTFLAGS")
                .unwrap()
                .1,
            "--sysroot=/tmp/reflex-directional-fixture-1/source/.directional-toolchain"
        );
        contract.validate().unwrap();
    }

    #[test]
    fn contract_binds_distinct_bounded_source_and_build_filesystems() {
        let contract = contract();
        assert_ne!(contract.paths.source, contract.paths.build);
        assert_eq!(contract.paths.target, contract.paths.build.join("target"));
        assert_eq!(contract.paths.temporary, contract.paths.build.join("tmp"));
        assert!(contract.bounds.source_bytes < contract.bounds.build_bytes);

        let source = tmpfs_arguments(
            &contract.paths.source,
            contract.bounds.source_bytes,
            contract.bounds.source_inodes,
            true,
        );
        let build = tmpfs_arguments(
            &contract.paths.build,
            contract.bounds.build_bytes,
            contract.bounds.build_inodes,
            true,
        );
        assert!(source.iter().any(|argument| argument
            == "size=67110144,nr_inodes=4128,mode=0700,uid=0,gid=0,nosuid,nodev,exec"));
        assert!(
            build
                .iter()
                .any(|argument| argument.to_string_lossy().contains("size=17112759040"))
        );
        assert_eq!(
            contract.bounds.source_bytes + contract.bounds.build_bytes,
            super::DIRECTIONAL_MEMORY_BYTES
        );
        assert!(super::FilesystemBounds::for_snapshot(super::SOURCE_MAXIMUM_BYTES, 8).is_err());
        assert!(
            readonly_remount_arguments(&contract.paths.source)
                .iter()
                .any(|argument| argument == "remount,ro,nosuid,nodev,exec")
        );
    }

    #[test]
    fn privilege_boundary_is_non_relaxable_and_only_build_is_writable() {
        let contract = contract();
        let arguments = setpriv_arguments(&contract, Path::new("/proc/self/fd/9"), 10);
        for required in [
            "--nnp",
            "--bounding-set=-all",
            "--inh-caps=-all",
            "--ambient-caps=-all",
            "--landlock-access",
            "fs",
            "path-beneath:execute,read-file,read-dir:/",
        ] {
            assert!(arguments.iter().any(|argument| argument == required));
        }
        let write_rule = arguments
            .iter()
            .find(|argument| {
                argument
                    .to_string_lossy()
                    .starts_with("path-beneath:write-file")
            })
            .unwrap()
            .to_string_lossy();
        assert!(write_rule.ends_with("/tmp/reflex-directional-fixture-1/build"));
        assert!(!write_rule.contains("/source"));
    }

    #[test]
    fn malformed_layout_environment_and_tools_fail_closed() {
        let mut malformed = contract();
        malformed.paths.source = malformed.paths.build.clone();
        assert!(malformed.validate().is_err());

        let mut malformed = contract();
        malformed
            .environment
            .push(("RUSTC_WRAPPER".into(), "/evil".into()));
        assert!(malformed.validate().is_err());

        let mut malformed = contract();
        malformed.tools.retain(|tool| tool.role != "rustc");
        assert!(malformed.validate().is_err());
    }

    #[test]
    fn stale_namespace_target_fails_closed_before_mounting() {
        let paths =
            super::NamespacePaths::for_nonce(&format!("stale-target-{}", std::process::id()))
                .unwrap();
        let _cleanup = super::NamespaceRootCleanup(paths.root.clone());
        std::fs::create_dir(&paths.root).unwrap();
        assert!(super::create_mount_layout(&paths).is_err());
    }

    #[test]
    fn trusted_system_tool_resolution_ignores_path() {
        let unshare = super::trusted_system_tool("unshare").unwrap();
        assert!(matches!(
            unshare.parent(),
            Some(parent) if parent == Path::new("/usr/bin") || parent == Path::new("/bin")
        ));
        assert_ne!(unshare, Path::new("/tmp/hostile-path/unshare"));
    }

    #[test]
    fn campaign_source_plan_binds_snapshot_bounds_and_sealed_mount() {
        let root =
            std::env::temp_dir().join(format!("reflex-campaign-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("present.rs"), "fn present() {}\n").unwrap();
        let paths = vec![b"deleted.rs".to_vec(), b"present.rs".to_vec()];
        let snapshot = super::super::source_snapshot_for_paths(&root, &paths).unwrap();
        let (plan, files) =
            super::prepare_campaign_source_plan(&root, paths, snapshot.clone()).unwrap();
        assert_eq!(plan.expected_snapshot, snapshot);
        assert!(plan.bounds.source_bytes >= super::SOURCE_MINIMUM_BYTES);
        assert!(plan.bounds.source_inodes >= super::SOURCE_MINIMUM_INODES);
        assert_eq!(files.len(), 3);
        plan.validate().unwrap();

        let mut altered = plan.clone();
        altered.expected_snapshot.sha256 = "forged".into();
        assert!(altered.validate().is_err());
        super::cleanup_campaign_source(&plan).unwrap();
        assert!(!plan.mount_root().exists());
        drop(files);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_copy_preserves_executable_mode_and_tracked_absence() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = std::env::temp_dir().join(format!(
            "reflex-manifest-copy-source-{}",
            std::process::id()
        ));
        let destination = std::env::temp_dir().join(format!(
            "reflex-manifest-copy-destination-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&destination);
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&destination).unwrap();
        let executable = root.join("tool");
        std::fs::write(&executable, "tool\n").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let paths = vec![b"deleted".to_vec(), b"tool".to_vec()];
        super::copy_manifest_paths(&root, &destination, &paths).unwrap();
        assert!(!destination.join("deleted").exists());
        assert_ne!(
            std::fs::metadata(destination.join("tool"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(destination).unwrap();
    }

    #[test]
    fn mathlib_tree_identity_is_exact_and_private_fixture_paths_are_canonical() {
        use std::os::unix::fs::PermissionsExt as _;

        let source = std::env::temp_dir().join(format!(
            "reflex-mathlib-identity-source-{}",
            std::process::id()
        ));
        let copy = std::env::temp_dir().join(format!(
            "reflex-mathlib-identity-copy-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&source);
        let _ = std::fs::remove_dir_all(&copy);
        std::fs::create_dir_all(source.join("Mathlib/Data")).unwrap();
        std::fs::write(source.join("Mathlib/Data/Test.olean"), b"verified").unwrap();
        let executable = source.join("lakefile.lean");
        std::fs::write(&executable, b"fixture").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        let expected = super::tree_snapshot(&source).unwrap();
        super::copy_tree_nofollow(&source, &copy).unwrap();
        let observed = super::tree_snapshot(&copy).unwrap();
        assert_eq!(observed.manifest_sha256, expected.manifest_sha256);
        assert_eq!(observed.bytes, expected.bytes);
        assert_eq!(observed.inodes, expected.inodes);
        std::fs::write(source.join("Mathlib/Data/Test.olean"), b"changed").unwrap();
        assert_ne!(
            super::tree_snapshot(&source).unwrap().manifest_sha256,
            expected.manifest_sha256
        );

        let mut contract = contract();
        let mut lake = contract
            .tools
            .iter()
            .find(|tool| tool.role == "mount")
            .unwrap()
            .clone();
        lake.role = "lake".into();
        lake.identity.canonical_path = "/trusted/lake".into();
        contract.tools.push(lake);
        contract.mathlib = Some(expected);
        contract.validate().unwrap();
        let fixture = contract.lean_fixture().unwrap();
        assert!(fixture.lake.starts_with(&contract.paths.source));
        assert!(fixture.mathlib.starts_with(&contract.paths.source));
        std::fs::remove_dir_all(source).unwrap();
        std::fs::remove_dir_all(copy).unwrap();
    }
}
