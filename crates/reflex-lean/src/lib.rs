//! Lean proof optimization for Reflex.

pub mod ast;
pub mod catalog;
pub mod domain;
pub mod worker;

/// The final mathlib commit on its default branch before 2025-01-01 UTC.
pub const MATHLIB_COMMIT: &str = "7178aee7a431bb7527da15c3507836d8dfefcda4";
/// The exact Lean toolchain selected by the pinned mathlib commit.
pub const LEAN_TOOLCHAIN: &str = "leanprover/lean4:v4.15.0-rc1";
/// The Lean compiler commit selected by [`LEAN_TOOLCHAIN`].
pub const LEAN_COMMIT: &str = "ffac974dba799956a97d63ffcb13a774f700149c";
