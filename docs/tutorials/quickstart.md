# Quickstart

Build Reflex and run your first cell.

## Prerequisites

- Rust toolchain 1.97.1 (`rust-toolchain.toml` auto-installs via rustup)
- `cargo-deny` ≥ 0.20 for policy checks (CI uses
  `EmbarkStudios/cargo-deny-action@v2`)

## Build

```bash
cargo build --workspace
cargo build --release            # release profile for real runs
```

## Check provenance

Every binary embeds build metadata at compile time:

```bash
cargo run -p reflex -- --version
```

Expected shape:

```
reflex 0.1.0 (commit: <sha>[-dirty], profile: debug, target: <triple>,
schema: arena:1 bundle:1 ledger:1 proto:1)
```

A `-dirty` marker means the tree had uncommitted changes at build time —
fine for local dev, a red flag for release binaries.

## Bit-vector tutorial (P10)

```bash
cargo run -p reflex -- init
cargo run -p reflex -- experiment plan --config config/bitvec-tutorial.toml
cargo run -p reflex -- experiment run --config config/bitvec-tutorial.toml
cargo test -p reflex local_run_completes_all_generations_and_reconstructs_from_current -- --exact
```

`experiment plan` resolves the TOML into an immutable manifest, validates all
identity-bearing fields, and prints its digest without writing durable state.
`experiment run` executes the three mandatory collection lanes in one local
process, keeps hot artifacts in the bounded arena, commits exact artifacts and
verification receipts in atomic evidence bundles, trains a real micro-MLP
checkpoint, and evaluates it on the disjoint frozen-eval task set. With no accepted
benchmark package, the registered candidate is rejected fail-closed; rejection
is the expected truthful result, not a tutorial failure.

Direct CLI cell execution remains fail-closed until a cell can be loaded from
its immutable manifest, ledger, and evidence-bundle inputs; the CLI does not provide a
passing-shaped placeholder command.

See `bitvec-autonomous-loop.md` for unknown/censored label semantics and the two-generation loop.

## Policy checks

```bash
cargo deny check              # advisories, bans, licenses, sources
scripts/check-unsafe.sh       # unsafe-code allowlist
cargo fmt --check
cargo clippy --workspace -- -D warnings
```

## Next

- `ledger-and-cas.md` — inspect what a run persisted.
- `protocol-and-fleet.md` — external verifier process protocol.
