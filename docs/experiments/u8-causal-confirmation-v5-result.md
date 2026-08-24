# Unary `u8` causal confirmation v5: confirmed

Status: **Scientific Confirmation under the preregistered v5 protocol**

All forty preregistered assignments completed on 2026-08-21 at git revision `663137e25ef2b1ed286d81e658698660b935c541`. There were no failed assignments, invalid evaluations, invalid completed recoveries, exclusions, or Protocol Deviations. Each treatment completed ten independent paired Campaign replicates, consumed exactly 10,500 verification requests per replicate, and retained all 8,190 audit-origin Pareto Artifacts.

The preregistered claim is confirmed: under equal evaluation Resource Envelopes, the full shared-and-consolidated stack reduced aggregate node count on previously unseen unary `u8` semantic Campaigns more than isolated Bootstrap and both one-factor ablations. Every paired effect was positive, every Bonferroni-adjusted family-wise 99% lower bound exceeded its practical threshold, and no protected aggregate regressed pairwise.

## Confirmatory contrasts

Effects are `comparison - full`, so positive values favor the full stack.

| Comparison | Practical threshold | Mean effect | Family-wise 99% lower bound | Paired range | Decision |
| :--- | ---: | ---: | ---: | ---: | :--- |
| Bootstrap | 500 nodes | 1,918.0 | 1,915 | 1,913–1,923 | Pass |
| No Model | 250 nodes | 899.4 | 897 | 896–902 | Pass |
| No Derived Operators | 200 nodes | 385.0 | 385 | 385–385 | Pass |

Mean aggregate node counts across the 8,190 final audit-origin Artifacts were 74,456.9 for `full`, 75,356.3 for `no-model`, 74,841.9 for `no-derived`, and 76,374.9 for `bootstrap`. Aggregate evaluator-operation counts are identical to node counts in this reference domain. Aggregate depth and encoded bytes also did not regress in any paired comparison.

## Execution diagnostics

These are diagnostics, not confirmation gates. One-worker wall p50 was 24.49 s for `full`, 21.61 s for `no-model`, 24.27 s for `no-derived`, and 19.21 s for `bootstrap`. Accounted resident p50 remained between 53.59 and 62.21 MiB, and durable p50 remained between 7.12 and 8.84 MiB, within the 256 MiB limits. The result establishes better verified output quality under the equal verifier envelope; it does not claim lower wall latency for the learned stack.

## Integrity and retention

- Experiment Specification SHA-256: `05b0e891b6023d5fe1e7b9db3b8d08e109e50422d3c9e6bd0bf16964bd644f14`
- Audit Corpus SHA-256: `5e1e3ad15fb06719b79855fbd18ce057c8f536f9d5e53d8575e13c9c4671a2ce`
- Report content SHA-256: `edd594aa3034d4feeaace8a3e30646311fe14c66eb683b127a0a48d5a8100350`
- Original uncompressed report file SHA-256: `dbd52db0f23c6630df5b16342c94638a70ec46a9c8b23636c4f1d961281bff40`
- Deterministically compressed report file SHA-256: `3bc9dc8aa49ca917c4855e21186a5f6632babc08e59ff33d855fd9d12e1661b8`
- Consumed audit artifact file SHA-256: `1f2577048b2496e5cbeb7044753f61353d374b82856330822168154119bf3320`
- Full training bundle SHA-256: `fb236d2ba93824ad3341c674311733b192a68c2312cd754e3cc229acacd2b4e0`
- Full Knowledge Revision: `a9c49da71f2548f8a694b6f70bfb5027d81ca4409ccb9c88ae7a31dbe517692e`
- Full Model Revision: `5cac5293745e5276b63df289fb1e3fc447fe44a13d1b977a1e63181fb0e97fed`

The complete report is [`u8-causal-confirmation-v5.json.gz`](./u8-causal-confirmation-v5.json.gz). All 81,900 exposed semantic groups are retained in [`u8-causal-confirmation-v5-consumed-audit.json`](./u8-causal-confirmation-v5-consumed-audit.json) and become Development Corpus for later experiments.

V3 and v4 remain immutable Protocol Deviations and make no confirmatory claims. V5 is the first valid confirmatory execution after the campaign-scale recovery correction and post-format baseline.
