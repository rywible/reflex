# Bootstrap Reference Domain Baseline v2

Status: exploratory baseline; not Scientific Confirmation

Protocol: [`bootstrap-reference-domain-v2-protocol.md`](./bootstrap-reference-domain-v2-protocol.md)

Raw report: [`bootstrap-reference-domain-v2.json`](./bootstrap-reference-domain-v2.json)

Runtime revision: `3860face0420d1c0e8c003680e0635472362d04c`

Report content SHA-256: `b2478546f613664c131947b12f873dbf58053ac4e54fb17a19b4416628162ef4`

## Result

All 96 assigned release-mode processes completed without exclusion or Protocol Deviation. Every fresh and recovered Session produced the same 256-Artifact semantic outcome digest, `8c5e4703787465fece63053886929642901c74fdbd02ff4412d0a15fca366130`.

Version 2 supersedes the v1 performance comparator because v1 incorrectly deduplicated the same Candidate Artifact across distinct Seed-relative Correctness Claims. The corrected workload performs 1,024 Verification requests per fresh Session. It also seals immutable Experience, delayed consequences, encoded Measurements, corpus roles, and learned revision state. Completed recovery performs 1,793 Verification requests because both retained Artifacts and Experience observations must replay before they can affect search or learning. The v1 report remains historical evidence but is not a valid comparator for this Runtime.

The one-worker p50 was 200.72 ms on the ordinary filesystem and 182.52 ms on `/dev/shm`. RAM-backed checkpointing reduced end-to-end wall time by 9.1%; the smaller effect than v1 is expected because corrected Verification, target derivation, FTRL training, and Experience replay now occupy a larger fraction of the Session. The reported 5,105 and 5,611 verifications/s are whole-Session rates, not isolated Verification-Kernel throughput.

Additional workers still do not improve this serialized Candidate pipeline. From one to eight workers, filesystem p50 throughput changed from 5,105 to 5,088 verifications/s, a 0.3% regression, while RAM-backed throughput changed from 5,611 to 5,587, a 0.4% regression. Peak accounted resident memory rose from 6,919,590 bytes to 21,599,654 bytes, primarily because every granted worker owns its reserved stack while Candidate-scale execution remains serialized.

The final bundle retained 256 Pareto Artifacts in 898,447 bytes, or 3,509 bytes per Pareto Artifact by integer division. The size increase over v1 is attributable to the restart-complete Experience and Model Revision state rather than an unexplained storage regression. One-worker completed recovery took 73.68 ms on the ordinary filesystem and 65.45 ms on `/dev/shm`.

## Frozen comparator

| Workers | Storage | Wall p50 | Wall p95 | CPU p50 | Verifications/s p50 | Peak resident p50 | Recovery wall p50 |
| ---: | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | filesystem | 200.72 ms | 206.78 ms | 185.26 ms | 5,105 | 6.92 MB | 73.68 ms |
| 1 | ramfs | 182.52 ms | 182.97 ms | 183.21 ms | 5,611 | 6.92 MB | 65.45 ms |
| 2 | filesystem | 203.92 ms | 206.17 ms | 185.54 ms | 5,025 | 9.02 MB | 74.40 ms |
| 2 | ramfs | 182.20 ms | 183.76 ms | 183.11 ms | 5,620 | 9.02 MB | 65.31 ms |
| 4 | filesystem | 203.19 ms | 213.40 ms | 185.89 ms | 5,045 | 13.21 MB | 74.98 ms |
| 4 | ramfs | 182.45 ms | 182.73 ms | 183.56 ms | 5,617 | 13.21 MB | 66.15 ms |
| 8 | filesystem | 201.67 ms | 203.92 ms | 186.18 ms | 5,088 | 21.60 MB | 74.95 ms |
| 8 | ramfs | 183.32 ms | 184.89 ms | 184.84 ms | 5,587 | 21.60 MB | 66.06 ms |

Equal-budget causal comparisons continue to use one worker as the primary Bootstrap treatment. Extra workers remain a measured memory cost until Candidate-scale parallel execution demonstrates an end-to-end gain. This baseline describes operational performance only; it does not establish that learned allocation causes better held-out outcomes.
