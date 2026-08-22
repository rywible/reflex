//! Production Lean proof optimization for Reflex.
//!
//! The domain pins the final pre-2025 Mathlib environment, stores actual
//! elaborated proof bodies, and delegates only trust-zero kernel checks and
//! environment indexing to a persistent local Lean worker. The Rust Runtime
//! retains ownership of search, learning, persistence, and resource control.
//! External worker execution currently requires Linux so a hard address-space
//! ceiling can back the resident Resource Envelope charge.

pub mod ast;
pub mod catalog;
pub mod domain;
#[doc(hidden)]
pub mod temporal;
pub mod worker;

/// The final mathlib commit on its default branch before 2025-01-01 UTC.
pub const MATHLIB_COMMIT: &str = "7178aee7a431bb7527da15c3507836d8dfefcda4";
/// The exact Lean toolchain selected by the pinned mathlib commit.
pub const LEAN_TOOLCHAIN: &str = "leanprover/lean4:v4.15.0-rc1";
/// The version reported by the executable selected by [`LEAN_TOOLCHAIN`].
pub const LEAN_VERSION: &str = "4.15.0-rc1";
/// Version string exposed to Lean programs by the pinned compiler.
pub const LEAN_RUNTIME_VERSION: &str = "4.15.0";
/// The Lean compiler commit selected by [`LEAN_TOOLCHAIN`].
pub const LEAN_COMMIT: &str = "ffac974dba799956a97d63ffcb13a774f700149c";
/// Canonical Lean Artifact wire-format revision.
pub const ARTIFACT_FORMAT_VERSION: u32 = 2;
/// Lean Verification Kernel contract revision.
pub const KERNEL_CONTRACT_VERSION: u32 = 2;

/// Immutable identity of one locally installed Lean/mathlib environment.
///
/// Reflex normally uses [`LeanSnapshotPin::final_pre_2025`]. Earlier pins are
/// explicit inputs to temporal Development and Semantic Migration; they never
/// weaken the exact-environment checks performed by the worker.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct LeanSnapshotPin {
    pub mathlib_commit: String,
    pub lean_toolchain: String,
    pub lean_version: String,
    pub lean_runtime_version: String,
    pub lean_commit: String,
}

impl LeanSnapshotPin {
    #[must_use]
    pub fn new(
        mathlib_commit: impl Into<String>,
        lean_toolchain: impl Into<String>,
        lean_version: impl Into<String>,
        lean_runtime_version: impl Into<String>,
        lean_commit: impl Into<String>,
    ) -> Self {
        Self {
            mathlib_commit: mathlib_commit.into(),
            lean_toolchain: lean_toolchain.into(),
            lean_version: lean_version.into(),
            lean_runtime_version: lean_runtime_version.into(),
            lean_commit: lean_commit.into(),
        }
    }

    #[must_use]
    pub fn final_pre_2025() -> Self {
        Self::new(
            MATHLIB_COMMIT,
            LEAN_TOOLCHAIN,
            LEAN_VERSION,
            LEAN_RUNTIME_VERSION,
            LEAN_COMMIT,
        )
    }
}
