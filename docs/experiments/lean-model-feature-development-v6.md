# Lean fixed-Experience capacity audit v6

The v25 retained Experience rejects a model-capacity increase. The promoted champion is a 1,445-byte linear model over 24 features. It trains in 1.406 milliseconds and reproduces its canonical Model Revision exactly.

The fixed corpus contains 992 Candidate examples across 24 Replay claims and eight Selection claims, with 30 Accepted examples. Every claim/operator/feature group is unique and there is no mixed Accepted/Refuted collision. Conservative structural and claim-balanced structural challengers produce exactly the champion's accepted top-k ranking on both claim-grouped and global views. Structural loss is marginally lower on most heads but does not clear Operational Promotion; balancing substantially worsens dominant losses without changing ranking.

At the selection top two, all three learned variants recover six of seven Accepted examples while Bootstrap recovers none. At global top 16, all learned variants recover four versus zero for Bootstrap. This agrees with the v25 online causal result and leaves no observed ranking benefit for more parameters on the current data.

Feature extraction takes 1.441 seconds, roughly one thousand times baseline model training. The next performance target is therefore cached/incremental structural feature extraction. The next intelligence target is broader and harder verified Experience, especially operators that construct proofs rather than substitute exact-statement donors.

The [machine summary](./lean-model-feature-development-v6-summary.json) records the decision fields. The complete report remains at `/tmp/reflex-lean-model-feature-development-v25.json`, with harness content SHA-256 `e43679a6cae6416565cad0832a8bcab13453adb4907d2247c677b24f44ce32e4` and file SHA-256 `c9526ba6735f0b344de147618a700b4934074e18197582d4596fedde81bae224`.

This used no new Verification requests and is Development evidence, not Scientific Confirmation.
