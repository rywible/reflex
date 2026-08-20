# ADR 0008: tokio 1.52.4 / rusqlite 0.40.1 Pins

## Status
Superseded by ADR 0014 for native v1

## Date
2026-08-19

## Context
The workspace previously declared `tokio = "~1.52"` while the lockfile
resolved 1.52.4, and `rusqlite` was unpinned at the caret line. Runtime
and metadata-storage versions are supply-chain critical: every worker,
the daemon, and the metadata stores link them (INV-RFX-12). A floating
requirement allows the lockfile to move to a version that has not been
verified against the schema and protocol surface.

## Decision
Pin exact versions: `tokio = "1.52.4"` (workspace dependency; workspace
members inherit the pin via `tokio.workspace = true`) and
`rusqlite = "=0.40.1"` (reflex-meta-sqlite). Both are also covered by
`deny.toml` `[bans] deny` entries (ADR 0010), so a second semver-major
line fails CI. A pin may only be raised by an ADR that verifies the
schema-compat range (ADR 0009) and re-runs the invariant tests.

## Consequences
- Lockfile-stable runtime and metadata-store versions across all
  binaries.
- Version bumps become deliberate, evidenced changes instead of lockfile
  drift.
- The `[bans] deny` entries require an ADR-0010 exception before a second
  tokio/rusqlite line can appear, even transitively.

ADR 0014 removed `rusqlite` and every SQL metadata backend from the native v1
workspace. The Tokio version remains controlled by the root workspace and
lockfile, but this record no longer authorizes a database dependency.

## Related
- Master-plan sections: §4.4, §22.3
- Invariants: INV-RFX-12, INV-RFX-15
- ADRs: 0009, 0010
