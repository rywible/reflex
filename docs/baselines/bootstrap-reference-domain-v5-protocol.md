# Bootstrap Reference Domain Baseline v5 Protocol

Status: frozen exploratory protocol

Protocol version: `reflex-bootstrap-baseline-v5`

Protocol SHA-256 for the declared 1/2/4/8-worker matrix: `98bb32c2506224f908917ffe251926f2fafce5178430c5ceaf6a4ed8f0eaada4`

Development Corpus: `unary-u8-full-ops-development-v2`

Corpus SHA-256: `c5c962fcd9f8016d6b5e5dec31890af5f9d6e5067402ead17637086a21dde95c`

Expected semantic-outcome SHA-256: `ecaf1feba1d8d45511b2b3b01fd85d0a9be829ac48814f3bc29e9daa1b8d5582`

## Purpose and supersession

This protocol supersedes v4 after the Reference Domain completed its unary `u8` constructor family, added explicit structural composition and location capabilities, preserved Seed provenance, and began charging domain Artifact storage to the Resource Envelope. The v4 protocol and report remain immutable evidence for the earlier implementation.

Version 5 freezes the same whole-Session public-`improve` workload against Semantic Identity `reflex-bitvec/u8/unary/full-ops/masked-shifts/select-nonzero/canonical-dag/v3`. It is an Exploratory Run under `docs/EXPERIMENTS.md`, not Scientific Confirmation. Every assignment starts with empty Knowledge and the Bootstrap Model; learning and consolidation occur only after that assignment's search and are tested through completed recovery.

The primary descriptive outcome is verified Candidate throughput per wall-clock second. Supporting outcomes are whole-Session wall and process CPU time, Runtime-reported elapsed and CPU use, peak resident bytes, durable and final bundle bytes, bytes per retained Pareto Artifact, observer additions, Pareto cardinality, completed-bundle recovery latency and cost, and the content-addressed semantic outcome.

## Fixed corpus

The Development Corpus contains all 256 unary `u8` functions `x xor c`, one for each `c` in `0..=255`, each presented through the same two-layer reducible `xor(_, 0)` lineage. The harness hashes canonical Artifact encodings in constant order. No Sealed Audit function is selected, generated, or inspected by this baseline.

## Treatments and resources

- Worker treatments: 1, 2, 4, and 8, omitting only treatments above available parallelism and recording that difference in the protocol hash.
- Storage treatments: ordinary repository filesystem and Linux `/dev/shm` RAM filesystem; unavailability is retained as a Protocol Deviation.
- Warmups: 2 process-isolated Sessions per worker/storage cell.
- Measured replicates: 10 process-isolated Sessions per worker/storage cell.
- Resident limit: 1 GiB.
- Durable limit: 256 MiB.
- Elapsed and process CPU limits: 120 seconds each.
- Verification-request limit: 100,000.
- Goal: minimize exact node count, with no Success Condition.

Every assignment starts a fresh process through public `improve`, then invokes completed `Resume` against the same bundle. Warmups are retained but excluded from summaries.

## Analysis and retention

The harness requires a release build, a clean committed worktree, and a new output path. Every assigned process, nonzero exit, malformed output, and Protocol Deviation is retained. Summaries use nearest-rank p50 and p95 wall latency, p50 CPU, p50 verifier throughput, p50 peak resident bytes, p50 bytes per Pareto Artifact, and p50 recovery latency. Successful treatments must agree on the frozen sorted Pareto Artifact-key digest, and recovery must reproduce its originating digest.

The JSON report records immutable provenance, raw outputs, summaries, and its own content hash. Reproduce with:

```bash
cargo run --release -p xtask -- baseline
```

Later investigations create new versioned protocols and never overwrite this record.
