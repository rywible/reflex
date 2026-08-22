# Lean fixed-Experience conservative structural-16 Development v3

The conservative 16-dimensional representation preserves baseline bias, clipped sizes, fractional reduction, and all eight operator buckets while replacing epoch leakage and three redundant dimensions with log Candidate size, Candidate depth, constructor-histogram distance, and root-constructor equality.

On the same 1,008 retained examples and exact 8/8 Replay/Selection claim split, six of seven Selection losses improve. Descendant-potential loss falls 49%; the five correctness/value losses each improve about 0.24%. The constant verification-cost head regresses 9.4%, however, and aggregate loss improves only about 0.28%, below the existing 1% promotion threshold. The representation therefore remains an unpromoted Development candidate. Baseline training again reproduces the exact persisted champion; no new Verification labels were requested.

The [machine-readable report](./lean-model-feature-development-v3.json) has content hash `d56b2fea0bf9de595400a6333c718326a574882d4244556673e6aee4128df13e` and repository file SHA-256 `4dbd71a63e3a3f891e8d5ae937e20a702236d147764b50e464e23ff49a222832`.

This result exposes a metric defect before another feature treatment: a constant request-count target and sparse duplicated value targets can dominate or flatten calibration loss without measuring whether Accepted Candidates move earlier. The analyzer must add the predeclared per-claim top-k Accepted precision and recall before the representation is judged further.
