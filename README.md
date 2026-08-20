# Reflex Framework

A platform for verified self-improving search, learning, and discovery.

## Invariant and Architecture Authority
Reflex learns search and discovery policies from self-generated, externally verified experience.
See `docs/reflex-framework-master-plan-rust.md` and `docs/invariants.md`.

## Quick Start
```bash
cargo build --workspace
cargo xtask check
```

## Running the Bit-Vector Tutorial
```bash
cargo run --bin reflex -- init
cargo run --bin reflex -- experiment plan --config config/bitvec-tutorial.toml
cargo run --bin reflex -- experiment run --config config/bitvec-tutorial.toml
cargo test -p reflex-integration-tests --test e2e_test \
  held_out_negative_result_is_artifact_backed_and_durable -- --exact --nocapture
```

The CLI runs collection, dataset assembly, training, and held-out evaluation
in one bounded local process. Hot artifacts stay in the in-memory arena;
declared barriers publish atomic evidence bundles under `.reflex/evidence`.
The tutorial is expected to end in a scientific rejection because no
registered benchmark evidence is supplied; it never manufactures promotion
evidence.
