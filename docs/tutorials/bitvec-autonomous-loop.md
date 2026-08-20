# Bit-vector tutorial: two-generation autonomous loop

Run the P10 dogfood path locally with the native Rust stack:

```bash
cargo run -p reflex -- init
cargo run -p reflex -- experiment run --config config/bitvec-tutorial.toml
# Copy the printed experiment ID:
cargo run -p reflex -- experiment status --id <experiment-id> --limit 1
cargo run -p reflex -- experiment resume --id <experiment-id>
```

`experiment run` completes every generation declared by the immutable plan in
one foreground process. `resume` reopens and verifies the current atomic
evidence bundle; after an interruption it re-executes only work beyond the last
committed barrier. Status is paged; when `next_cursor` is present, pass it back
with `--after`.

## What this proves

1. **Generation 1** collects verified bit-vector search episodes through the
   stable, uniform, and heuristic lanes required by the registered plan.
2. Dataset compilation produces receipt-backed viable and certified-dead
   labels for observed candidates; it never invents candidates to manufacture
   an `Unknown` row.
3. A micro-MLP ranker trains on verified pairwise supervision. A separate
   censored fixture proves that genuine unknown labels receive zero gradient
   and are never treated as negatives.
4. A real immutable checkpoint and held-out evaluation are published.
5. Promotion is rejected because the tutorial supplies no accepted benchmark
   evidence. No benchmark, knowledge, or economics artifact is invented.
6. **Generation 2** runs in the same invocation and its root set is committed
   with generation 1 into the final atomic evidence bundle.

## Unknown and censored labels

Reflex distinguishes three candidate knowledge classes:

| Label | Meaning | Training gradient |
|-------|---------|-------------------|
| **Viable** | Verified route exists (receipt in the evidence root set) | Supervised (pairwise / listwise among viable only) |
| **Unknown** | Untried or **censored** (budget exhausted, episode incomplete) | **Zero** — never treated as a negative |
| **KnownDead** | Oracle or verifier certified false | Pairwise vs viable only when a dead certificate exists |

Censored search failures (action budget, node budget, CPU limit) produce `Unknown` labels with
coverage metadata like `censored_budget_exhausted`. They do **not** become `KnownDead` and must not
leak into pairwise accuracy denominators.

The bit-vector domain verifies every accepted optimization over an **exhaustive u8 truth table**
(independent assignments per variable, not correlated environment slots). A rewrite is accepted
only after `BitvecDomain::verify` returns `is_equivalent: true` with a content-addressed receipt
digest — never a fabricated default receipt.

## Frozen corpus and invariants

The tutorial uses separate `frozen_train()` and `frozen_eval()` task sets,
which are disjoint by `TaskId` digest. Blocking invariants INV-RFX-1 through INV-RFX-6 are covered by
`cargo test -p reflex-integration-tests`.

## Expected outputs

Generation 1 publishes non-empty verified training evidence, then reports
held-out metrics for frozen-eval. Its final state is `rejected` unless a real,
accepted benchmark evidence package is supplied through a registered workflow.

See also: `quickstart.md`, `ledger-and-cas.md`, `protocol-and-fleet.md`.
