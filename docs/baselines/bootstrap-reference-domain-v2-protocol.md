# Bootstrap Reference Domain Baseline v2 Protocol

Status: frozen exploratory protocol

Protocol version: `reflex-bootstrap-baseline-v2`

Protocol SHA-256 for the declared 1/2/4/8-worker matrix: `9a7883c6898e80afec03b8b873fbaf263b55eefc219cadcd00977a43984fd403`

Development Corpus: `unary-u8-xor-development-v1`

Corpus SHA-256: `653f5b41c219a3ce89d1d1358a84fbb12ec6cc2aaccc9c8976b2a89884d9726c`

## Purpose and supersession

This protocol supersedes the v1 Bootstrap performance comparator after claim-relative Candidate deduplication corrected a semantic defect: equal Artifact encodings under distinct Seed-relative Correctness Claims require independent Verification. The v1 protocol and raw report remain immutable historical evidence, but their throughput denominator and bundle shape are not valid comparators for the corrected Runtime.

Version 2 freezes the same whole-Session, public-`improve` workload against the corrected Runtime. It is an Exploratory Run under `docs/EXPERIMENTS.md`; it is not Scientific Confirmation and cannot serve as a sealed audit corpus. Although a fresh Session may train a challenger while sealing its bundle, its Campaign remains pinned to the Bootstrap Revision for the entire measured search.

The primary descriptive outcome is verified Candidate throughput per wall-clock second. Supporting outcomes are whole-Session wall and process CPU time, Runtime-reported elapsed and CPU use, peak resident bytes, durable and final bundle bytes, bytes per retained Pareto Artifact, observer additions, Pareto cardinality, completed-bundle recovery latency and cost, and semantic outcome digest.

## Fixed corpus

The corpus contains all 256 unary `u8` semantic functions `x xor c`, one for each `c` in `0..=255`. Each function is presented through the same two-layer reducible `xor(_, 0)` lineage. The complete semantic-function family, not surface rows, is the unit of corpus construction. The harness hashes the actual canonical Artifact encodings in constant order.

This is Development Corpus material. It may influence implementation and tuning and must never be relabeled as sealed evidence.

## Treatments and resources

- Worker treatments: 1, 2, 4, and 8, omitting only treatments above the host's available parallelism and recording that difference in the protocol hash.
- Storage treatments: ordinary repository filesystem and Linux `/dev/shm` RAM filesystem. An unavailable RAM filesystem is retained as a Protocol Deviation.
- Warmups: 2 process-isolated Sessions per worker/storage cell.
- Measured replicates: 10 process-isolated Sessions per worker/storage cell.
- Resident limit: 1 GiB.
- Durable limit: 256 MiB.
- Elapsed limit: 120 seconds.
- Process CPU limit: 120 seconds.
- Verification-request limit: 100,000.
- Goal: minimize exact node count, with no Success Condition.

Every assignment starts a fresh process and invokes the public `improve` operation. It then invokes a completed `Resume` against the same bundle to measure Artifact, Seed, and Experience Verification replay, transient-index reconstruction, and atomic republication. Warmups are marked and retained in raw output but excluded from summaries.

## Analysis and retention

The harness reports every assigned process, including nonzero exits, stderr, malformed output, and Protocol Deviations. Summaries include nearest-rank p50 and p95 wall latency, p50 CPU, p50 verifier throughput, p50 peak resident bytes, p50 bytes per Pareto Artifact, and p50 recovery wall latency. No assignment may be excluded after observing its result.

All successful treatments must produce the same sorted Pareto Artifact-key digest and the recovered digest must equal its originating fresh digest. A mismatch is a Protocol Deviation, not an exclusion.

The JSON report records the Git revision and dirty state, complete Rust and Cargo version output, target architecture and OS, CPU description, available parallelism, canonical protocol and corpus hashes, raw child output, derived summaries, and a content hash computed with its own hash field empty. The reproduction command is:

```bash
cargo run --release -p xtask -- baseline
```

The resulting versioned report is the Bootstrap comparator for the corrected production Runtime. Later performance investigations create new versioned protocols and never overwrite this record.
