# ADR 0010: Supply-Chain Policy (cargo-deny + Exception Registry)

## Status
Accepted

## Date
2026-08-19

## Context
Reflex treats its dependency graph as part of its correctness surface
(P0.3). Unpinned registry requirements, unapproved licenses, advisories,
and unvetted Git sources can change behavior or introduce vulnerabilities
without a code review. cargo-deny 0.20.x enforces the policy; the removed
`vulnerability`/`notice`/`unlicensed`/`copyleft` keys (0.16.0, PR#611)
mean advisories and licenses are denied by default, and the allow-list is
the full license policy.

## Decision
`deny.toml` enforces:

- **Advisories**: all advisory classes denied by default (0.20 default);
  `unmaintained`/`unsound` at `all` scope; `yanked = "deny"`. The ignore
  list is empty except entries mirrored in `deny-exceptions.json`
  (currently: RUSTSEC-2024-0436 `paste`, see ADR 0006).
- **Licenses**: allow-list of permissive licenses (MIT, Apache-2.0,
  BSD-2/3-Clause, ISC, Zlib, Unlicense, 0BSD, CC0-1.0, Unicode-3.0,
  BSL-1.0, MIT-0, Apache-2.0 WITH LLVM-exception); every other license —
  including all copyleft — is denied. Copyleft exceptions exist only for
  `colored 3.1.1` and `option-ext 0.2.0` (MPL-2.0 weak copyleft,
  registry-backed).
- **Bans**: `multiple-versions = "warn"` globally with per-crate `deny`
  for supply-chain-critical crates (tokio, hyper, openssl, serde,
  reqwest, blake3, rusqlite, tokio-postgres, datafusion, arrow, parquet,
  burn, axum, prost, object_store, crc32c, safetensors); `skip-tree` only
  for `ndarray@0.17.2` (ADR 0006). `wildcards = "warn"` pending
  `publish = false` on member crates (see below).
- **Sources**: crates.io only; `allow-git = []` — a Git source requires a
  new ADR.
- **Graph**: targets limited to x86_64/aarch64 Linux and macOS, keeping
  UEFI-only `r-efi` out of policy.

`deny-exceptions.json` is the human-reviewed registry: every
`[advisories] ignore`, `[licenses.exceptions]`, `[bans] skip-tree`, and
per-crate `deny` override must have a registry entry with owner,
rationale, expiry date, and evidence links. The registry is uploaded as
an artifact from CI and reviewed on renewal.

## Consequences
- CI fails on vulnerabilities, unmaintained/unsound advisories, yanked
  versions, unallowed licenses, and second major lines of critical
  crates.
- Exceptions are time-boxed and owner-attributed; expiring entries force
  re-review.
- Known gap: workspace crates do not yet set `publish = false`, so
  cargo-deny treats them as public and flags internal
  `<crate>.workspace = true` deps as wildcards. Until members set
  `publish = false`, `wildcards = "warn"` and `allow-wildcard-paths =
  true` keep CI green; registry requirements stay pinned via Cargo.toml
  exact pins. Owner: workspace Cargo.toml reconciliation.
- Fast-lane SBOM verification is `cargo xtask sbom --verify`, invoked
  only through `cargo xtask check` (CI does not duplicate the check).

## Related
- Master-plan sections: §22.3, P0.3
- Invariants: INV-RFX-22
- ADRs: 0006, 0007, 0008
