# ADR 0009: Schema Compatibility Range

## Status
Accepted

## Date
2026-08-19

## Context
Reflex persists versioned formats for native runtime artifact identity, atomic
evidence bundles, ledger segments, and protocol frames (proto
`reflex.domain.v1`). Binaries must be able to state, at runtime,
which schema revisions they understand; a binary that silently writes a
format the fleet cannot read violates the replayability and cleanup
invariants (INV-RFX-11, INV-RFX-23).

## Decision
Embed a schema-compatibility string in every binary at build time via
`build.rs` (git commit, dirty marker, target, profile, and the schema
line `arena:1 bundle:1 ledger:1 proto:1`), surfaced by `--version` and
the local evidence manifest. The format is
`<component>:<major>` per format; a component bumps when a format change
is not backward-readable by the previous revision. Bumping a component
requires an ADR and a migration plan; `docs/invariants.md` and the
`docs/storage/` and `docs/protocols/` specifications must be updated in
the same change.

## Consequences
- Any binary reports exactly what its native path writes and reads; incompatible
  local artifacts are detectable from `--version` output alone.
- Schema bumps are release-blocking, evidenced decisions.
- The embedded line is regenerated per build, so dirty local builds are
  distinguishable from released binaries.

## Related
- Master-plan sections: §4, §7, §9, §10.3, §22.3
- Invariants: INV-RFX-9, INV-RFX-11, INV-RFX-12, INV-RFX-23
- ADRs: 0008, 0014 (native component replacement)
