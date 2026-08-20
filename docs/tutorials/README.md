# Tutorials

Hands-on guides for Reflex. Commands assume a workspace build with the
toolchain pinned in `rust-toolchain.toml` (1.97.1).

## Contents

- `quickstart.md` — build, version output, and the verified bit-vector run.
- `ledger-and-cas.md` — inspecting ledger segments and CAS objects.
- `protocol-and-fleet.md` — daemon health, worker fleet, and protocol
  verification.

## Conventions

- Every binary reports build provenance via `--version`:
  `<name> <semver> (commit: <sha>[-dirty], profile: <debug|release>,
  target: <triple>, schema: arena:<n> bundle:<n> ledger:<n>
  proto:<n>)` (ADR 0009).
- The workspace is unsafe-free: `#![forbid(unsafe_code)]` everywhere;
  `scripts/check-unsafe.sh` enforces the allowlist.
