# Lean Candidate feature audit v1

## Scope

This is a local code and retained-Experience audit of the generic Runtime Candidate representation at git revision `8ab711cc3b5ecc36b5add85c7ad620b0352bc0e0`. It precedes any increase in model capacity or training-corpus size. The current model is a seven-head linear FTRL predictor over 16 features: 112 effective coefficients and 224 persisted FTRL accumulator values, excluding calibration state and derivable cached weights.

## Current representation

| Index | Current value | Finding |
| ---: | --- | --- |
| 0 | bias | Sound. |
| 1 | `min(parent_nodes / 1024, 1)` | All large Lean parents collapse to the same value. |
| 2 | `min(candidate_nodes / 1024, 1)` | All large Lean Candidates collapse to the same value. |
| 3 | signed fractional node reduction | Useful but linear-scale compression is poor across 1-to-100,000-node proofs. |
| 4 | `epoch / 1024` | Encodes search history and scheduling, not portable Candidate structure; it can learn run order as a shortcut. |
| 5–12 | eight-bucket operator one-hot | Stable but collision-prone; Lean currently has more semantic operator distinctions than buckets. |
| 13 | reduction × selected operator bucket | A bug in representational intent: the selected bucket is always `1`, so this exactly duplicates feature 3 and contains no operator interaction. |
| 14 | Candidate larger than parent | Derivable from feature 3. |
| 15 | clipped Candidate/parent node ratio divided by four | Strongly redundant with feature 3 and loses large ratios. |

The representation ignores depth, leaf/branch structure, constructor distribution, root shape, immediate density, and parent-to-Candidate structural distance even though the common Structural Protocol exposes constructors and child indexes. It also ignores domain Measurements until after Verification, which is correct; pre-Verification features must remain structural or provenance-derived and may never masquerade as correctness.

## Retained evidence

The v7 activation bundle contains 1,008 kernel-labeled attempts but only nine Accepted outcomes. Those attempts cover 13 of 16 requested claims because Bootstrap ordering spent the entire envelope before proposing work for three roots. Therefore the activation failure is not evidence that more parameters are needed. It first establishes a claim-coverage defect, 111:1 class imbalance, severe large-proof feature saturation, three redundant dimensions, and no real operator interaction.

## Same-capacity correction candidate

The first representation change, after measuring the active current model, should remain 16-dimensional:

| Indexes | Proposed nested v2 family |
| --- | --- |
| 0 | bias |
| 1–3 | normalized `ln(1 + parent_nodes)`, `ln(1 + candidate_nodes)`, and signed log node ratio |
| 4–6 | normalized parent depth, Candidate depth, and signed log depth ratio |
| 7–9 | parent leaf fraction, Candidate leaf fraction, and leaf-fraction delta |
| 10 | L1 distance between stable schema-indexed constructor histograms |
| 11 | root-constructor equality |
| 12–15 | four stable signed operator-hash buckets |

This keeps inference to a small fixed dot product. Structural summaries should be computed in one post-order pass over the view already required for Candidate accounting, cached for frontier parents, and retained only as derivable epoch-local state. Immediate payload traversal is deliberately excluded from v2 until measured because Lean currently serializes node immediates through JSON and that cost could dominate selection.

## Decision

Do not increase parameter count yet. First protect one deterministic exploration opportunity per correctness claim and run the existing 16-feature model against Bootstrap at 128 total evaluation requests. If it promotes but has no causal ranking advantage, replay the exact retained Experience through the same-capacity v2 representation. Only a same-capacity feature win unlocks the model-size sweep.
