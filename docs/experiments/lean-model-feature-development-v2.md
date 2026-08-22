# Lean fixed-Experience operator-retaining structural-16 Development v2

The second 16-dimensional representation also fails the paired gate. It retained eight operator buckets while compressing node, depth, constructor-distance, and root-shape summaries into seven non-bias dimensions. The analyzer reused the same 1,008 Experience entries, exact 8/8 Replay/Selection claim split, targets, ordering, FTRL settings, and zero new Verification labels. Baseline retraining again reproduced the persisted champion exactly.

Relative to baseline, the dominant correctness/value Selection losses increased 3.8%, which is inside the 5% protected-head tolerance, and descendant-potential loss improved 75%. But verification-cost loss increased from `0.00008350` to `0.00021560`, and aggregate loss did not improve by the required 1%. The representation therefore does not promote. Both models remain 997 bytes; feature extraction took 0.587 CPU seconds for all Candidates.

The [machine-readable report](./lean-model-feature-development-v2.json) has content hash `f5b387a0de58b4853c427f8dc97fab4341dc43d917a389382e47fe7e1f48630d` and repository file SHA-256 `95f3accf399579fb42e3d256792a3d2757be33acc801339f421df00a85dcff43`.

This result supports a conservative third family rather than more capacity: retain the baseline's bias, clipped parent/Candidate sizes, fractional reduction, and exact eight operator buckets; replace epoch leakage and the three redundant tail dimensions with normalized log Candidate size, Candidate depth, constructor-histogram distance, and root-constructor equality.
