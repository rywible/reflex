# Lean public-path optimizer Development v26 result

## Result

The first production online-feedback run proves that a Model Revision trained during an Improvement Session can improve later allocation in that same Session. After promotion, the Learned queue received 464 Candidate checks and produced 27 kernel-accepted strict proof collapses. The concurrent Bootstrap queue received 496 and produced seven. Learned therefore achieved a 5.82% hit rate versus 1.41%, a 4.1× advantage, while the protected origin check produced one additional collapse.

On the unchanged eight-claim held-out Development corpus, Full found five collapses after 18 fresh Candidate checks and removed 5,684 local proof nodes. Bootstrap found four after 120 and removed 1,417; no-model found one after 18 and removed five. At the registered request-16 prefix, Full found four collapses with 0.864 seconds of completed Candidate Verification-batch CPU while Bootstrap found one with 0.832 seconds. Full therefore retained the 4× equal-prefix discovery advantage and used roughly 6.7× fewer final Candidate checks than Bootstrap.

The checked-in [machine summary](./lean-public-optimizer-development-v26-summary.json) contains the decision-relevant fields. The complete report remains at `/tmp/reflex-lean-public-optimizer-development-v26-32x8.json`; its harness content SHA-256 is `2bdb2f1c6ff3a5701636673611183d1191e14ec9dc0c0b3ac10b61b92576f885` and its file SHA-256 is `7a4fa287bc23bf6da58ebc0f9dcfa80eb29ae198539481da6c51e19d9dac1d0b`.

## Online feedback

The active generation-one model first served a Learned allocation in epoch four. Across the complete 992-Candidate training Experience, the queue accounting was:

| Queue | Verified | Accepted | Hit rate |
| --- | ---: | ---: | ---: |
| Protected origin | 32 | 1 | 3.13% |
| Learned | 464 | 27 | 5.82% |
| Bootstrap | 496 | 7 | 1.41% |

This is direct within-Session evidence for search → Verify → train → search: the learned policy did not merely score a frozen log after the run. Training admitted 35 strict improvements across all 32 correctness claims, five more accepted attempts and five more covered claims than v25's end-only training run.

The gain remains localized to proof substitution. Proof substitution produced 34 of the 35 accepted Candidates and abstraction produced one; application, composition, rewriting, and anti-unification produced none. No-derived again matched Full byte-for-byte in outcomes, because no Derived Operator became active.

## Selection exhaustion defect

V26 also exposes a bounded-corpus scheduling defect. The promoted generation remained one even as retained Experience grew from the first 32 examples to 992. A no-Verification fixed-Experience audit of the resulting bundle found 32 Replay claims, zero remaining Selection claims, and a complete-data model that did not reproduce the active champion.

The cause is mechanical: training was attempted at every power-of-two Experience crossing and once again at finalization, but a Selection claim retires after three comparisons. The first three online comparisons therefore consumed every Selection use early. Later challengers—including the model trained from complete Experience—had no disjoint corpus on which they could earn promotion. The first viable online champion had become operationally fossilized.

This diagnosis also explains the important regression against v25. V25's end-only 992-example model found all eight held-out collapses; v26's early online champion found five. V26 still handily beats its contemporaneous Bootstrap and no-model counterfactuals, so online allocation has causal value, but the active model is not the best model the accumulated Experience could train.

## Economics and isolation

Full used 53.829 seconds of end-to-end CPU for five discoveries, or 10.766 seconds each. Bootstrap used 56.362 seconds for four, or 14.091 seconds each. At the Candidate layer, Full used 0.879 seconds for five discoveries, or 0.176 seconds each; Bootstrap used 10.174 seconds for four, or 2.543 seconds each.

Training completed in 263.3 seconds elapsed and 264.0 seconds process-plus-worker CPU. Its peak resident use was 19.976 GB under the 32 GiB Runtime allowance. Peak treatment residency was 23.688 GB. The supervisor kept a 40 GiB process boundary, reserved CPU 7, and preserved 16 GiB for the host and SSH session throughout.

## Decision

Keep the current 24-feature, seven-head, 1,445-byte linear model and the 1:1 cooperative queue. The next intervention is to align online-training cadence with the three-use Selection budget, preserve a final comparison after Experience matures, and avoid duplicate training at an unchanged Experience watermark. Then repeat this exact constrained run once. Parameter growth and another operator intervention remain frozen until the feedback loop can replace its first champion.

This is positive Development evidence and a mechanistic defect diagnosis, not Scientific Confirmation.
