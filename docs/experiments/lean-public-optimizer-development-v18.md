# Lean public-path optimizer Development v18 result

## Result

The active 24-feature linear model produced the first positive public-path Lean allocation result. Full found all four held-out kernel-Verified Proof Collapses within 16 Candidate Verification requests. Bootstrap found one by request 16 and three after 124 observed Candidate requests; no-model found one by request 16 and two in total. Full therefore has causal ranking value on this Development corpus: removing the model removes two discoveries, while Full also recovers the collapse Bootstrap misses.

At request 16, Full used 2.072 seconds of completed Verification-batch CPU for four discoveries, or 0.518 seconds per discovery. Bootstrap used 1.349 seconds for one discovery. The learned path delivered 4× the strict discoveries and 2.60× lower Candidate Verification CPU per discovery at the registered prefix. Full removed 12,922 local proof nodes, compared with Bootstrap's 4,756 and no-model's 998.

The checked-in [machine summary](./lean-public-optimizer-development-v18-summary.json) contains the decision-relevant fields. The complete 132 KiB report remains at `/tmp/reflex-lean-public-optimizer-development-v18.json`; its harness content SHA-256 is `7e9cb9d9e9e8a2b48a3456f91e99a755de415df1f79eb744255a06c4a1e0cc24` and its file SHA-256 is `42d9978e35a3cc4a581aaf87f14ccf098d52b0557a1dce4fb77792e735b1f8f7`.

## What the model learned

Training covered 16 correctness claims and stopped with no eligible work after 671 Verification requests. It retained 655 Candidate fates, admitted 16 strict improvements, and promoted generation one. The proof-substitution audit reconstructed all 103 relevant attempts without ambiguity, unmatched donors, or Accepted/Refuted feature collisions.

The result is best understood as learned premise allocation. Indexed retrieval exposes several plausible donor proofs for each held-out claim. The model's donor relation and structural features move all four correct donors into the first 16 checks. The same retained state without the model finds only two.

No-derived was operationally indistinguishable from Full: both found the same four improvements at the same prefixes with nearly identical CPU. This experiment therefore provides no evidence that admitted proof artifacts improved later generation. The causal mechanism is retained Experience → promoted model → better donor ordering.

## Mathematical inspection

The four improvements are exact-statement or definitionally equal library aliases:

- `CompleteLattice.isCompactlyGenerated_of_wellFoundedGT` reuses `CompleteLattice.isCompactlyGenerated_of_wellFounded`;
- `Matrix.det_updateCol_eq_zero` reuses `Matrix.det_updateColumn_eq_zero`;
- `hfdifferential_apply` reuses `apply_hfdifferential`;
- `Matrix.fromCols_mul_fromRows` reuses `Matrix.fromColumns_mul_fromRows`.

Each replacement is a one-node reference to an already verified theorem, accepted for the Seed proposition by the pinned Lean kernel. The largest local collapse replaces the 8,167-node proof of `hfdifferential_apply` with `apply_hfdifferential`.

This is useful library deduplication and proof refactoring, not theorem invention. It removes duplicated local proof bodies because the donor already exists, but it does not establish global corpus compression, abstraction discovery, or mathematical novelty.

## Economics and limits

Every treatment had the same 128-request end-to-end envelope. Forked treatments spend part of it replaying retained verified state, so their Candidate curves stop at 76 observed requests while fresh Bootstrap reaches 124. Full wins despite receiving fewer new Candidate checks.

Full's end-to-end CPU was 68.606 seconds for four improvements, while Bootstrap used 51.817 seconds for three. The resulting per-improvement advantage is only about 0.7% and is dominated by roughly 39–46 seconds of setup and recovery per treatment. It should not be presented as a robust speed result. The request-16 Candidate-path comparison is the meaningful Development efficiency result; report schema v19 records that comparison separately with exact ratio arithmetic.

The corpus is opportunity-selected and has only four held-out claims. In addition, the v18 “human-facing” filter admitted two compiler-generated training declarations containing `_auxLemma.N`. The held-out claims are human-facing, the causal treatment comparison remains real for the executed corpus, and no 2026 data was exposed; however, the protocol description was not literal. Schema v19 excludes generated name components before any broader run.

The release run used AArch64 NEON, six worker threads on CPUs 0–6, a 40 GiB supervisor boundary, one reserved CPU, and 16 GiB reserved memory. Peak treatment residency was 19.607 GB, and the host reserve remained intact.

## Decision

Do not increase model capacity. First reproduce the result with the corrected human-facing corpus. If it survives, broaden the number of independent correctness claims at a fixed total Verification budget and report paired per-claim discovery curves. Only then compare pointwise and claim-conditioned pairwise linear ranking on identical retained Experience. Derived-artifact and compression work remains behind an end-to-end treatment where a derived proof actually changes a later held-out search.

This is positive Development evidence, not Scientific Confirmation.

The corrected [v19 reproduction](./lean-public-optimizer-development-v19.md) replaces the two generated training declarations and reproduces the same causal treatment result.
