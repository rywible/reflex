# Bootstrap Reference Domain Baseline v5

Status: exploratory baseline; not Scientific Confirmation

Protocol: [`bootstrap-reference-domain-v5-protocol.md`](./bootstrap-reference-domain-v5-protocol.md)

Raw report: [`bootstrap-reference-domain-v5.json`](./bootstrap-reference-domain-v5.json)

Runtime revision: `130aee91a52064a1fc72f0e48e365e5495c6b74e`

Report file SHA-256: `86815912731844b5364428be606f6d471fb8dd84759e33fb5abe7919afaefe71`

Report content SHA-256: `d5ebcd52766d5163ca04027c9937a0628b9dab28d80de656d7109f4755cd0dd5`

## Result

All 96 assigned release-mode processes completed without exclusion or Protocol Deviation. Every fresh and recovered Session produced the frozen 256-Artifact semantic-outcome digest `ecaf1feba1d8d45511b2b3b01fd85d0a9be829ac48814f3bc29e9daa1b8d5582`.

Version 5 supersedes v4 after the Reference Domain completed its constructor family, added explicit typed structural composition and locations, preserved Seed and derivation provenance in Domain Bundles, and charged dynamic Artifact storage to the resident Resource Envelope. Fresh search performs 1,024 Verification requests; completed recovery performs 1,793. No Sealed Audit semantic function was generated or inspected.

The one-worker p50 was 265.76 ms on the ordinary filesystem and 246.65 ms on `/dev/shm`, corresponding to 3,857 and 4,155 whole-Session verifications/s. The final bundle is 1,019,592 bytes and the one-worker accounted resident peak is 7,390,797 bytes. One-worker completed recovery took 77.21 ms and 69.09 ms, respectively.

Additional workers remain a memory cost without throughput benefit for this small workload. From one to eight workers, filesystem p50 throughput changed from 3,857 to 3,841 verifications/s, while RAM-backed throughput changed from 4,155 to 4,157; accounted resident memory rose from 7.39 MB to 22.07 MB.

## Frozen comparator

| Workers | Storage | Wall p50 | Wall p95 | CPU p50 | Verifications/s p50 | Peak resident p50 | Recovery wall p50 |
| ---: | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | filesystem | 265.76 ms | 268.70 ms | 249.08 ms | 3,857 | 7.39 MB | 77.21 ms |
| 1 | ramfs | 246.65 ms | 247.88 ms | 247.51 ms | 4,155 | 7.39 MB | 69.09 ms |
| 2 | filesystem | 267.17 ms | 268.57 ms | 250.45 ms | 3,836 | 9.49 MB | 77.27 ms |
| 2 | ramfs | 247.04 ms | 251.66 ms | 248.45 ms | 4,146 | 9.49 MB | 69.16 ms |
| 4 | filesystem | 267.58 ms | 272.92 ms | 252.13 ms | 3,828 | 13.68 MB | 78.15 ms |
| 4 | ramfs | 246.25 ms | 248.14 ms | 248.49 ms | 4,159 | 13.68 MB | 68.93 ms |
| 8 | filesystem | 266.62 ms | 272.54 ms | 252.91 ms | 3,841 | 22.07 MB | 77.85 ms |
| 8 | ramfs | 246.35 ms | 247.15 ms | 250.65 ms | 4,157 | 22.07 MB | 68.94 ms |

The v3 causal harness pins the report's file, protocol, content, and semantic-outcome hashes and refuses to expose a new audit corpus if the comparator is missing, modified, dirty, incomplete, or contains a Protocol Deviation. Equal-budget causal comparisons still execute their own paired one-worker Bootstrap treatment; this baseline is the prior operational and throughput comparator.
