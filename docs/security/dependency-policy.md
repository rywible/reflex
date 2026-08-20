# Dependency Policy

Operational policy for the dependency graph (ADR 0010). The policy is
enforced by `deny.toml` + `cargo deny check` in CI, with human-reviewed
exceptions in `deny-exceptions.json`.

## Rules

1. **Advisories**: vulnerability, unmaintained, unsound, notice, and
   yanked all fail CI. `[advisories] ignore` entries require a
   `deny-exceptions.json` registry entry (id, owner, rationale, expiry,
   evidence).
2. **Licenses**: only the permissive allow-list in `deny.toml` is
   permitted. Anything else — including all copyleft — fails CI unless it
   has both a `[licenses.exceptions]` entry and a registry entry.
3. **Versions**: registry requirements are exact-pinned in Cargo.toml.
   A second semver-major line of a critical crate (`[bans] deny` list)
   fails CI; other splits warn. `skip-tree` entries require registry
   entries.
4. **Sources**: crates.io only. `allow-git` is empty; a Git source
   requires an ADR.
5. **Wildcards**: informational (`warn`) until member crates set
   `publish = false` (cargo-deny then applies `allow-wildcard-paths` to
   the internal path deps). This is the known gap recorded in
   ADR 0010.

## Exception Workflow

1. Identify the offending crate/advisory; write the registry entry in
   `deny-exceptions.json` with owner, rationale, expiry (default 1 year),
   and evidence links.
2. Mirror the exception in `deny.toml` (ignore / exceptions / skip-tree).
3. CI re-run must be green; the change is reviewed as a normal PR.
4. On expiry, the entry fails a review check; renew only with a fresh
   rationale (typically an ADR for the migration that removes the
   exception).

## Checking Locally

```bash
cargo deny check                 # all checks, all features
cargo deny check advisories      # single check for fast iteration
```

CI runs the same command via `EmbarkStudios/cargo-deny-action@v2`.

## Current Exceptions

See `deny-exceptions.json` (registry) and `deny.toml` (enforcement).
Current set: `ndarray@0.17.2` (Burn split), `colored`/`option-ext`
(MPL-2.0 weak copyleft), `paste` RUSTSEC-2024-0436 — all expire
2027-08-19 with the Burn migration (ADR 0006).