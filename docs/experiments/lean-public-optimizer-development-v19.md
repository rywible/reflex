# Lean public-path optimizer Development v19 result

## Result

The corrected human-facing corpus reproduces v18's causal result. Full found all four held-out kernel-Verified Proof Collapses within 16 Candidate Verification requests. Bootstrap found one at request 16 and three after 124 observed Candidate requests; no-model found one at request 16 and two overall. No-derived again matched Full exactly on discoveries.

At request 16, Full used 2.058 seconds of completed Verification-batch CPU for four discoveries, or 0.514 seconds per discovery. Bootstrap used 1.309 seconds for one discovery. Full therefore delivered 4× the strict discoveries and 2.54× lower Candidate Verification CPU per discovery at the registered prefix. It removed 12,922 local proof nodes, compared with Bootstrap's 4,756 and no-model's 998.

The checked-in [machine summary](./lean-public-optimizer-development-v19-summary.json) contains the decision-relevant fields. The complete report remains at `/tmp/reflex-lean-public-optimizer-development-v19.json`; its harness content SHA-256 is `75ee66382b37c731cc3ea1490cf19e1c92d827cac076ee91c63a70e61f5f4963` and its file SHA-256 is `87d526c219321183a9906fde0d2fdad8afaeb47d29f9ed605675ed3c3f1a39e0`.

## Correction and reproduction

Schema v19 recursively excludes compiler-generated dotted numeric, leading-underscore, and `proof_N` declaration-name components from training and held-out Seeds. The two generated v18 training declarations were replaced by `WithTop.top_ne_natCast` and `Derivation.map_natCast`. The held-out claims and treatment envelopes were unchanged.

The replacement corpus required 714 training Verification requests rather than v18's 671 and produced 123 audited proof-substitution attempts rather than 103. All 123 were reconstructed without ambiguity, unmatched donors, or Accepted/Refuted feature collisions. Training admitted 16 strict improvements, stopped with no eligible work, and promoted generation one.

The nearly identical held-out discovery curves show that v18's model effect was not an artifact of the generated training declarations. The exact model state did change, and it reached the same operational behavior on the four held-out claims.

## Economics

Full used 67.371 seconds of end-to-end CPU for four improvements, or 16.843 seconds each. Bootstrap used 52.625 seconds for three, or 17.542 seconds each. The end-to-end per-improvement advantage is about 4.0%; setup and recovery still dominate both sessions, so the stronger decision evidence remains the request-16 discovery curve rather than a broad wall-time claim.

The run used release-mode AArch64 NEON, six worker threads on CPUs 0–6, a 40 GiB supervisor boundary, one reserved CPU, and 16 GiB reserved memory. Peak treatment residency was 19.722 GB. The host reserve remained intact.

## Interpretation

The model has now shown reproducible causal ranking value through the public Improvement Session on a corrected Development corpus. It learned to allocate a small Verification budget among indexed donor proofs better than Bootstrap and the same retained state without a model.

This remains exact-statement theorem reuse. It is useful proof deduplication and refactoring, not theorem invention, global corpus compression, abstraction discovery, or evidence of mathematical taste. Four opportunity-selected held-out claims are also too few for a statistical generalization claim.

## Decision

The current linear model clears the activation gate. It does not clear the model-capacity gate. The next evidence should come from more independent correctness claims under the same total Verification budget, followed by an offline, claim-grouped comparison of pointwise and pairwise linear ranking on identical retained Experience. A larger model remains unjustified until richer data and a ranking-aligned objective leave residual representable error.

This is positive Development evidence, not Scientific Confirmation.
