// Build-time version identity shared by all Reflex binaries.
//
// The values are embedded by each binary's `build.rs` via
// `cargo:rustc-env` directives (see `bins/*/build.rs`). Keeping the
// formatting in one module keeps native Reflex binaries on one compatibility
// line.
//
// Each binary uses a different subset of the items below (for example,
// `reflex` prints only `VERSION_LINE` via clap), so dead-code is expected
// per binary and suppressed.

#![allow(dead_code)]

/// Schema compatibility range, as defined by ADR-0009.
///
/// Each component is bumped on an incompatible change to the
/// corresponding persisted format:
///
/// - `arena`    — bounded in-memory artifact format (crates/reflex-cas)
/// - `bundle`   — atomic local evidence bundle format (crates/reflex-cas)
/// - `ledger`   — evidence segment format (crates/reflex-ledger)
/// - `proto`    — external domain protocol (proto/reflex/domain/v1)
pub const SCHEMA_COMPAT: &str = env!("REFLEX_SCHEMA_COMPAT");

/// Canonical version line: `<version> (commit: <hash>, profile: <profile>, target: <triple>, schema: <compat>)`.
pub const VERSION_LINE: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (commit: ",
    env!("REFLEX_GIT_COMMIT"),
    ", profile: ",
    env!("REFLEX_PROFILE"),
    ", target: ",
    env!("REFLEX_TARGET_TRIPLE"),
    ", schema: ",
    env!("REFLEX_SCHEMA_COMPAT"),
    ")",
);

/// Full `--version` output for a binary, e.g. `reflex 0.1.0 (commit: ...)`.
pub fn version_text(binary: &str) -> String {
    format!("{binary} {VERSION_LINE}")
}
