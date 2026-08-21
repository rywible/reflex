# Unary `u8` causal confirmation v3: recovery deviation

Status: **Protocol Deviation — no confirmatory claim**

All forty preregistered assignments completed on 2026-08-21 at git revision `aca298723c7fe2641781dd37d0a5a794cee33b10`. Every evaluation consumed the exact 10,500-request, one-worker envelope and produced all 8,190 audit-origin Pareto Artifacts. However, completed recovery rejected all twenty bundles whose treatments retained Derived Operators (`full` and `no-model`) as corrupt. The twenty `no-derived` and `bootstrap` bundles recovered successfully.

Because the decision rule required valid recovery for every assignment, no paired contrast was computed and neither a causal effect nor a null effect is confirmed.

## Retained evidence

- Experiment Specification SHA-256: `32d8beccdbd2d42135dc6b5b345819e7d217f831e04dd8945ba657408f5a11eb`
- Audit Corpus SHA-256: `585c9e7ec1f2c64fb34fb2d9a300e72d5d29c2ea3ff34fca250807c4d990aaaf`
- Report content SHA-256: `5712040cca863e36052864585b763bcda3628808dbf7558f58e041dca88a3e68`
- Original uncompressed report file SHA-256: `7109243cbc88ce4cd3327ed4250fd1f6d3306d5853520c0a48dbd61acd48e4b7`
- Deterministically compressed report file SHA-256: `8e2aca95ad9042ef08877cc7b78fd6ed0bf3b1c1cf22001477d80aa2c5e26ea3`
- Consumed audit artifact file SHA-256: `6f8abbd94ac3d701259cd2f52ed464e88b2c2ad2057d5d3f0c61df537cd2ae98`
- Full training bundle SHA-256: `b599880d8e4ae85e88d157e423457d9341c6b9de75cd016247851c604fb78427`
- Full Knowledge Revision: `9743f1771c98d355b17908c6e25a9ec5b93670a9aa7d3a8ebf92c5331c8400fd`
- Full Model Revision: `5cac5293745e5276b63df289fb1e3fc447fe44a13d1b977a1e63181fb0e97fed`

The complete machine-readable report, including every raw child result and per-Artifact Pareto record, is [`u8-causal-confirmation-v3.json.gz`](./u8-causal-confirmation-v3.json.gz). All 81,900 exposed semantic groups are retained separately in [`u8-causal-confirmation-v3-consumed-audit.json`](./u8-causal-confirmation-v3-consumed-audit.json) and are Development Corpus for every later experiment.

## Descriptive evaluation outcomes

These values are disclosed for provenance only. They are not confirmatory contrasts because the recovery gate failed.

| Replicate | Full | No model | No derived | Bootstrap | Bootstrap − full | No model − full | No derived − full |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 0 | 74,472 | 75,372 | 74,857 | 76,391 | 1,919 | 900 | 385 |
| 1 | 74,466 | 75,360 | 74,851 | 76,377 | 1,911 | 894 | 385 |
| 2 | 74,463 | 75,365 | 74,848 | 76,386 | 1,923 | 902 | 385 |
| 3 | 74,451 | 75,347 | 74,836 | 76,366 | 1,915 | 896 | 385 |
| 4 | 74,466 | 75,366 | 74,851 | 76,383 | 1,917 | 900 | 385 |
| 5 | 74,458 | 75,354 | 74,843 | 76,369 | 1,911 | 896 | 385 |
| 6 | 74,467 | 75,367 | 74,852 | 76,386 | 1,919 | 900 | 385 |
| 7 | 74,449 | 75,347 | 74,834 | 76,366 | 1,917 | 898 | 385 |
| 8 | 74,454 | 75,350 | 74,839 | 76,367 | 1,913 | 896 | 385 |
| 9 | 74,457 | 75,351 | 74,842 | 76,370 | 1,913 | 894 | 385 |

The descriptive mean differences were 1,915.8 nodes for Bootstrap, 897.6 for the Model ablation, and 385 for the Derived Operator ablation. These observations may inform diagnosis but must not alter the already declared practical thresholds for a successor confirmation.

## Root cause and correction

The bundle bytes were valid. Recovery validation incorrectly required an immutable predecessor Knowledge Revision's historical Derived Operator trial counters to equal totals from the newer complete Experience Ledger. Any later use of a Derived Operator therefore made the predecessor appear corrupt. Champion counters must equal the current ledger; predecessor counters must instead be internally valid historical prefixes.

The correction is covered through the public `improve` seam by evaluating a completed bundle containing exercised Derived Operators and then reopening it. A successor confirmation must use a new specification, a fresh post-fix Bootstrap comparator, and audit semantic groups disjoint from v1, v2, and v3.
