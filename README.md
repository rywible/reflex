# Reflex Framework

A platform for verified self-improving search, learning, and discovery.

## Invariant and Architecture Authority
Reflex learns search and discovery policies from self-generated, externally verified experience.
See `docs/reflex-framework-master-plan-rust.md` and `docs/invariants.md`.

## Quick Start
```bash
cargo build --workspace
cargo xtask check
cargo xtask task verify P0.1
```

## Running the Bit-Vector Tutorial
```bash
cargo run --bin reflex -- init
cargo run --bin reflex -- experiment run --config config/bitvec-tutorial.toml
```
