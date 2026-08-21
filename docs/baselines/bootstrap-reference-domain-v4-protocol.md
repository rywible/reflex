# Bootstrap Reference Domain Baseline v4 Protocol

Status: frozen exploratory protocol

Protocol version: `reflex-bootstrap-baseline-v4`

Protocol SHA-256 for the declared 1/2/4/8-worker matrix: `406b68fe3e85deab4c2fa8456bbfdd47082b0edcaba1dc265407fb66b45dd68d`

Development Corpus: `unary-u8-xor-development-v1`

Corpus SHA-256: `653f5b41c219a3ce89d1d1358a84fbb12ec6cc2aaccc9c8976b2a89884d9726c`

## Purpose and supersession

This protocol supersedes v3 after the Reference Domain's Semantic Identity expanded from XOR-only expressions to canonical unary `u8` wrapping-add, rotate-left, and XOR expression DAGs. The v3 protocol and report remain immutable evidence for the prior Semantic Identity.

Version 4 freezes the same whole-Session, public-`improve` workload against the expanded production domain. It is an Exploratory Run under `docs/EXPERIMENTS.md`; it is not Scientific Confirmation. The unchanged XOR Development Corpus deliberately measures semantic-migration overhead without exposing any future Sealed Audit functions. Each fresh Campaign remains pinned to empty Knowledge and the Bootstrap Model during search; post-search consolidation and training affect only its completed bundle and recovery.

The primary descriptive outcome is verified Candidate throughput per wall-clock second. Supporting outcomes are whole-Session wall and process CPU time, Runtime-reported elapsed and CPU use, peak resident bytes, durable and final bundle bytes, bytes per retained Pareto Artifact, observer additions, Pareto cardinality, completed-bundle recovery latency and cost, and semantic outcome digest.

## Fixed corpus

The Development Corpus contains all 256 unary `u8` functions `x xor c`, one for each `c` in `0..=255`, each presented through the same two-layer reducible `xor(_, 0)` lineage. The harness hashes their canonical Artifact encodings in constant order. No wrapping-add or rotate-left semantic function is selected, generated, or inspected by this baseline.

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

Every assigned process, nonzero exit, malformed output, and Protocol Deviation is retained. Summaries use nearest-rank p50 and p95 wall latency, p50 CPU, p50 verifier throughput, p50 peak resident bytes, p50 bytes per Pareto Artifact, and p50 recovery latency. Successful treatments must agree on the sorted Pareto Artifact-key digest, and recovery must reproduce its originating digest.

The JSON report records immutable provenance, raw outputs, summaries, and its own content hash. Reproduce with:

```bash
cargo run --release -p xtask -- baseline
```

Later investigations create new versioned protocols and never overwrite this record.
