# Bootstrap Reference Domain Baseline v3

Status: exploratory baseline; not Scientific Confirmation

Protocol: [`bootstrap-reference-domain-v3-protocol.md`](./bootstrap-reference-domain-v3-protocol.md)

Raw report: [`bootstrap-reference-domain-v3.json`](./bootstrap-reference-domain-v3.json)

Runtime revision: `bf44cff292286b1853703a99787244a6274fc802`

Report content SHA-256: `3a3a8d0a8e8dbd229f16f7a2d65bfdecde0923d01064e344fb102fb27dfc8825`

## Result

All 96 assigned release-mode processes completed without exclusion or Protocol Deviation. Every fresh and recovered Session produced the same 256-Artifact semantic outcome digest, `8c5e4703787465fece63053886929642901c74fdbd02ff4412d0a15fca366130`.

Version 3 supersedes the v2 operational comparator because the production Runtime now performs Knowledge Consolidation, seals a canonical Knowledge Revision with a bounded active Artifact index and verified Derived Operators, and validates that state during completed recovery. Fresh Campaigns remain pinned to empty Knowledge and the Bootstrap Model, so the 1,024 fresh Verification requests and their search allocation are unchanged. Completed recovery still performs 1,793 Verification requests for retained Artifacts, Seeds, and Experience.

The one-worker p50 was 204.65 ms on the ordinary filesystem and 185.06 ms on `/dev/shm`, corresponding to 5,019 and 5,539 whole-Session verifications/s. Relative to v2, wall time increased 2.0% and 1.4%, respectively. This is the measured cost of consolidation, revision encoding, and stronger recovery validation rather than a semantic workload change.

The final bundle retained 256 Pareto Artifacts in 931,562 bytes, or 3,638 bytes per Pareto Artifact by integer division. The 33,115-byte increase over v2 contains the canonical Knowledge Revision, predecessor, active index, Derived Operator program, and compression consequences. One-worker completed recovery took 77.62 ms on the ordinary filesystem and 67.48 ms on `/dev/shm`.

Additional workers still do not improve the serialized Candidate pipeline. From one to eight workers, filesystem p50 throughput changed from 5,019 to 5,004 verifications/s, a 0.3% regression, while RAM-backed throughput changed from 5,539 to 5,527, a 0.2% regression. Peak accounted resident memory rose from 6,919,949 bytes to 21,600,013 bytes as worker stacks were reserved without Candidate-scale parallel work.

## Frozen comparator

| Workers | Storage | Wall p50 | Wall p95 | CPU p50 | Verifications/s p50 | Peak resident p50 | Recovery wall p50 |
| ---: | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | filesystem | 204.65 ms | 207.73 ms | 187.79 ms | 5,019 | 6.92 MB | 77.62 ms |
| 1 | ramfs | 185.06 ms | 185.57 ms | 185.80 ms | 5,539 | 6.92 MB | 67.48 ms |
| 2 | filesystem | 204.60 ms | 206.13 ms | 187.82 ms | 5,016 | 9.02 MB | 76.06 ms |
| 2 | ramfs | 184.91 ms | 185.82 ms | 185.78 ms | 5,538 | 9.02 MB | 67.46 ms |
| 4 | filesystem | 203.59 ms | 205.81 ms | 188.15 ms | 5,037 | 13.21 MB | 76.03 ms |
| 4 | ramfs | 185.12 ms | 185.94 ms | 186.16 ms | 5,534 | 13.21 MB | 67.66 ms |
| 8 | filesystem | 204.96 ms | 209.22 ms | 188.97 ms | 5,004 | 21.60 MB | 76.47 ms |
| 8 | ramfs | 185.45 ms | 186.06 ms | 186.89 ms | 5,527 | 21.60 MB | 67.80 ms |

Equal-budget causal comparisons use one worker as the primary Bootstrap treatment. Extra workers remain a measured memory cost until Candidate-scale parallel execution demonstrates an end-to-end gain. This baseline describes operational performance only; it does not establish that learned allocation or Derived Operators cause better held-out outcomes.
