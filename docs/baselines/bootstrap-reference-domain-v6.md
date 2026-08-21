# Bootstrap Reference Domain Baseline v6

Status: exploratory baseline; not Scientific Confirmation

Protocol: [`bootstrap-reference-domain-v6-protocol.md`](./bootstrap-reference-domain-v6-protocol.md)

Raw report: [`bootstrap-reference-domain-v6.json`](./bootstrap-reference-domain-v6.json)

Runtime revision: `4a55d61345b03e9b472fabbdfa43d5c91587a0d9`

Report file SHA-256: `2d8b477eab19e29a38a1821ba33fa8bcdf7c4414658d11e2d77d60f7a45863d1`

Report content SHA-256: `03ec7da8c56391626057611ec4ca1ddae5f6bacc38b46ae961bb441cdb1a60f7`

## Result

All 96 assigned release-mode processes completed without exclusion or Protocol Deviation. Every fresh and recovered Session produced the frozen 256-Artifact semantic-outcome digest `ecaf1feba1d8d45511b2b3b01fd85d0a9be829ac48814f3bc29e9daa1b8d5582`.

Version 6 supersedes v5 after the Derived Operator predecessor-recovery invariant was corrected. The search path and comparator semantics are unchanged. Fresh search performs 1,024 Verification requests; completed recovery performs 1,793. No Sealed Audit semantic function was generated or inspected.

The one-worker p50 was 264.12 ms on the ordinary filesystem and 246.50 ms on `/dev/shm`, corresponding to 3,877 and 4,155 whole-Session verifications/s. The final bundle is 1,019,592 bytes and the one-worker accounted resident peak is 7,390,797 bytes. One-worker completed recovery took 76.49 ms and 68.98 ms, respectively.

## Frozen comparator

| Workers | Storage | Wall p50 | Wall p95 | CPU p50 | Verifications/s p50 | Peak resident p50 | Recovery wall p50 |
| ---: | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | filesystem | 264.12 ms | 267.08 ms | 249.19 ms | 3,877 | 7.39 MB | 76.49 ms |
| 1 | ramfs | 246.50 ms | 247.29 ms | 247.37 ms | 4,155 | 7.39 MB | 68.98 ms |
| 2 | filesystem | 265.30 ms | 280.23 ms | 249.51 ms | 3,861 | 9.49 MB | 77.34 ms |
| 2 | ramfs | 246.23 ms | 246.60 ms | 247.66 ms | 4,159 | 9.49 MB | 68.92 ms |
| 4 | filesystem | 263.11 ms | 266.17 ms | 249.97 ms | 3,895 | 13.68 MB | 76.54 ms |
| 4 | ramfs | 246.72 ms | 250.35 ms | 248.98 ms | 4,153 | 13.68 MB | 69.09 ms |
| 8 | filesystem | 267.39 ms | 270.19 ms | 254.26 ms | 3,831 | 22.07 MB | 78.86 ms |
| 8 | ramfs | 247.10 ms | 251.45 ms | 251.52 ms | 4,144 | 22.07 MB | 69.33 ms |

The successor causal harness pins the report's file, protocol, content, and semantic-outcome hashes and refuses audit exposure if the comparator is missing, modified, dirty, incomplete, or contains a Protocol Deviation.
