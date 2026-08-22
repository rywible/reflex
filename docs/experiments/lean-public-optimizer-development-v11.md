# Lean public-path optimizer Development v11 result

## Result

Claim-complete generation repaired the lost-opportunity failure from v7. Fresh Bootstrap, Full, no-model, and no-derived each found all four held-out kernel-Verified Proof Collapses by Candidate Verification request 4. Each treatment replaced 12,922 proof nodes with four one-node proofs, including the previously lost `hfdifferential_apply` collapse from 8,167 nodes to one.

This result rejects the active linear model as a causal improvement at this envelope. Full found no discovery that no-model or Bootstrap missed and used 53.549 seconds of process CPU, compared with 47.639 seconds for no-model and 43.258 seconds for Bootstrap. Full therefore failed both the more-improvements gate and the lower-CPU-per-improvement gate. No-derived was operationally indistinguishable from Full on discoveries and nearly identical in CPU.

The checked-in [machine summary](./lean-public-optimizer-development-v11-summary.json) contains the decision-relevant fields. The complete 312 KiB report remains at `/tmp/reflex-lean-public-optimizer-development-v11.json`; its harness content hash is `460c2be38a73178498061af3183713c460c80eceab5232944e6c57e3dc7dc798` and file SHA-256 is `0e8d02735b44d59073c878c3fc9083afd91dbc81a195defc9a4968a60174a482`. The summary is intentionally not described as the complete report.

## What changed from v7

The v7 trace showed that a correctness claim could lose every useful proposal when earlier claims consumed the shared generation window. Runtime revision 7 now protects generation coverage by correctness claim across both enumeration stages, redistributes unused capacity deterministically, and accounts grouped generation memory before expansion. Under that policy, every treatment generated the known one-node substitution for each held-out claim and selected those four Candidates at policy ranks zero through three.

The repair localizes the old `hfdifferential_apply` miss to generation coverage rather than learned ranking. Its Candidate was generated at rank 56 under fresh Bootstrap and rank 37 under retained-state treatments, selected at protected policy rank 2, and kernel-Accepted in every treatment.

## Activation and isolation evidence

The run used clean git revision `16b8c582325fdcb4659eb83e8e2408460be3f840`, 16 pre-2025 training Artifacts, four disjoint held-out Artifacts, a 1,024-request training envelope, and equal 128-request treatment envelopes. Training consumed 74.056 CPU seconds, peaked at 18.735 GB resident, and promoted generation one. Its retained training Domain Bundle has SHA-256 `ae59e86fe6e058ecfce658cbd7dd6a9f4f9cf9c2653a29682b97c1f916cab9f1`.

The release build used AArch64 NEON. The supervisor allowed CPUs 0–6 and 40 GiB while reserving CPU 7 and 16 GiB for the host. Peak treatment residency was 19.016 GB. No 2026 data was accessed.

The no-kernel audit reconstructed all 295 proof substitutions, with 26 ambiguous donor matches, no unmatched substitutions, and no Accepted/Refuted feature-collision groups. It therefore removes collision ambiguity from this particular diagnosis; it does not establish that the representation is generally sufficient.

## Interpretation limits

All four successes came from the protected per-claim queue. Protection chooses the first generated Candidate for each claim before learned and Bootstrap ordering can affect the remainder. On this corpus, proof substitution is the first Operator and the exact one-node donor is emitted first, so the live treatment does not expose causal learned-ranking value even though a champion is active.

Candidate Verification curves exclude seed and recovery Verification. Bootstrap spent four requests checking Seeds and recorded 124 Candidate fates. Retained-state treatments spent 46 requests on recovery and recorded 82 Candidate fates. The end-to-end envelope is equal, but the curves must not be read as equal Candidate budgets.

Every curve point within a treatment carries the same completed-batch CPU because the Runtime verifies the selected remainder as one batch. The run therefore measures final ordering and end-to-end economics, not a choose–verify–learn feedback loop between registered cutoffs.

## Fixed-Experience diagnostic

The v8 feature-development analysis reused the exact 1,008 retained examples and requested no new kernel labels. It compared 24-feature baseline, structural, and claim-balanced linear trainers. The structural challenger did not promote and did not advance an Accepted Selection Candidate before the top-32 cutoff. Claim balancing moved one additional Accepted Candidate into the top four within its claim, but substantially worsened the dominant forecast losses. Feature extraction took 0.590 CPU seconds and each training treatment took less than 0.81 milliseconds, confirming that parameter count and training throughput are not the current constraints.

## Decision

Do not increase model capacity. The next loop improvement must create feedback and honest economics before giving the model more authority:

1. verify bounded Candidate cohorts so new Accepted Artifacts can affect subsequent search within the same Optimization Run;
2. retain deferred generation work so cohorting cannot discard uncovered parent opportunities;
3. measure Verification cost at a defensible granularity instead of training the cost head on the constant value of one request;
4. record counterfactual Bootstrap and learned ranks for protected Candidates so protection no longer masks attribution;
5. add donor-aware retrieval and relation features before another model-family or size treatment.

This is Development evidence, not Scientific Confirmation.
