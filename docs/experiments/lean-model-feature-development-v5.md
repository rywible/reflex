# Lean fixed-Experience outcome-balancing Development v5

Inverse-frequency outcome weighting is rejected. Starting from conservative structural-16, the analyzer balanced positive and zero targets independently by head on the same eight Replay claims, capped weights at 32, and evaluated against the untouched eight Selection claims. It used no new labels and no additional parameters.

Balanced training raised the five dominant Selection losses from about `0.01494` to `0.09966` and produced exactly the baseline top-k Accepted ranking `[3, 3, 3, 6, 6, 7, 7]`. It therefore erased structural-16's one-candidate top-16 advantage without creating another ranking benefit. The [machine-readable report](./lean-model-feature-development-v5.json) has content hash `1f44d0796bc7eb0fe9b3bf36e03b729d80b9819354b96fc63b93f65cced04515` and repository file SHA-256 `3079146101ed1544eaa3e8bd6f870c11d2afffa8f5cce0ec4f8d488a3142b8df`.

The failure rules out naïve class weighting. Subsequent trainer work must optimize within-claim order directly while retaining individual-example calibration.
