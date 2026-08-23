# Forecast durable checkpoints in encoded bytes

The Runtime forecasts pre-Verification durability from the prospective Domain Bundle encoding, never from resident-memory estimates. The private Experience Ledger reports its exact logical encoded length, the Runtime reports the exact logical Recovery length, and the bundle codec owns a conservative encoded-size bound that preserves unchanged stored segments while applying the compressor's published maximum size to replaced compressed segments. A Verification Cohort may start only when that bound fits the durable Resource Envelope.

Canonical Candidate bytes are retained with each deferred Candidate after novelty filtering. The same bytes establish Candidate identity, become Experience, and encode the restart-complete Recovery tail. Recovery rechecks canonical round trips before retaining bytes from an imported bundle. This removes repeated structural encoding from cohort drain and makes durable forecasting constant-time in Artifact structure after the initial novelty pass.

## Context

The v23 32×8 Lean retry implemented move-aware resident accounting but reproduced v22 exactly: 352 Candidate Verification requests, 25 Accepted attempts across 23 claims, 601 generated Candidates, 360 selected Candidates, and no promoted Model Revision. Reported peak resident use fell from 19.734 GB to 18.824 GB, yet the final selected cohort still did not reach Verification. This falsified resident double-counting as the limiting cause while retaining ADR 0072 as a real ownership-accounting correction.

The pre-Verification durable gate ran before the resident gate. It added `candidate_pipeline_reserve` and `recovery_resident_bytes` to the current checkpoint length, mixing RAM bytes with the one-GiB durable limit. The completed v23 bundle was 90,731,283 encoded bytes. Its Experience and Recovery segments were 146,877,499 and 84,979,992 logical bytes; even the bundle codec's maximum-compression-output bound for those complete segments was only 246,819,697 bytes. Recovery contained 134 deferred Candidates with 84,951,685 canonical bytes. The durable refusal therefore did not describe a possible checkpoint.

A minimized BitVec regression reproduced the same defect in milliseconds: a 5,120-byte durable envelope published a 1,471-byte restart-complete bundle after refusing the second Verification request. Encoded forecasting lets the request run and publishes the complete 2,245-byte bundle under the identical envelope.

## Consequences

The bundle codec becomes the deep module for durable framing and compression bounds; the Runtime no longer needs to understand stored-segment overhead or LZ4 expansion. Resident reservations remain separately conservative and now include retained canonical Candidate storage. Exact post-Verification checkpoints are still checked before publication, so the bound is a pre-dispatch safety proof rather than a replacement for atomic durability checks.

Internal phase reports now record the first normal Resource refusal category. A future activation-null can distinguish durable preflight, resident preflight, epoch residency, time, and Verification allowance without inference from aggregate usage.

Runtime revision 18 pins the corrected admission policy because a revision-17 Resume could advance past a cohort its original Runtime would refuse. Lean Development schema v24 preserves v23 as an activation-null before one identical retry.
