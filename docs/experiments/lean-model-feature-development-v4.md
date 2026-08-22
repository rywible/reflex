# Lean fixed-Experience conservative structural ranking Development v4

This run repeats the conservative structural-16 v3 treatment and adds per-claim top-k ranking outcomes using the runtime's exact seven-head forecast ordering. Baseline retraining again reproduces the persisted champion; all 1,008 examples, 8/8 claim roles, and kernel labels are unchanged, and no Verification request is made.

The eight Selection claims contain seven Accepted Candidates. Baseline and structural-16 both rank three Accepted outcomes within top-1, remain tied through top-8, and rank six within top-8. At top-16, structural-16 has recovered all seven while baseline has recovered six; both saturate by top-32. Structural features therefore move one verified success earlier, but the advantage is modest and does not satisfy the calibration-loss promotion gate.

The [machine-readable report](./lean-model-feature-development-v4.json) has content hash `224604e058988d6e0b3bc052e9a6b47978f2a469df8d7057735d2ab1f3c712d6` and repository file SHA-256 `4d770f62b7cd55121a5d09bb2fadc4355a3bc85101e5d42825fe490c39691d29`.

This closes the representation-only iteration. The next fixed-corpus Development treatment keeps 16 features and seven linear heads but addresses the unweighted 995:13 class imbalance and the constant request-count cost target. Larger feature families remain locked.
