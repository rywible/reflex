# Bootstrap Reference Domain Baseline v7

Status: exploratory baseline; not Scientific Confirmation

Protocol: [`bootstrap-reference-domain-v7-protocol.md`](./bootstrap-reference-domain-v7-protocol.md)

Raw report: [`bootstrap-reference-domain-v7.json`](./bootstrap-reference-domain-v7.json)

Runtime revision: `268c08e9222ff03d320c37f7aefd7e5a30ea299d`

Report file SHA-256: `b8a5e2c8ef87a88c4576d934e8d9e234961942cc599973d789a5fe4e29ad6daa`

Report content SHA-256: `18af93061337a068e4c883dd149632d87b8d197d2ff33ad87e1c074aedf1f117`

## Result

All 96 assigned release-mode processes completed without exclusion or Protocol Deviation. Every fresh and recovered Session produced the frozen 256-Artifact semantic-outcome digest `ecaf1feba1d8d45511b2b3b01fd85d0a9be829ac48814f3bc29e9daa1b8d5582`.

Version 7 supersedes v6 after Knowledge Revisions gained explicit Experience-ledger watermarks and exact prefix validation. The search path and comparator semantics are unchanged. Fresh search performs 1,024 Verification requests; completed recovery performs 1,793. No Sealed Audit semantic function was generated or inspected.

The one-worker p50 was 266.90 ms on the ordinary filesystem and 245.54 ms on `/dev/shm`, corresponding to 3,854 and 4,170 whole-Session verifications/s. The final bundle is 1,019,608 bytes and the one-worker accounted resident peak is 7,390,853 bytes. One-worker completed recovery took 77.46 ms and 68.85 ms, respectively.

## Frozen comparator

| Workers | Storage | Wall p50 | Wall p95 | CPU p50 | Verifications/s p50 | Peak resident p50 | Recovery wall p50 |
| ---: | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | filesystem | 266.90 ms | 269.35 ms | 248.64 ms | 3,854 | 7.39 MB | 77.46 ms |
| 1 | ramfs | 245.54 ms | 246.29 ms | 246.35 ms | 4,170 | 7.39 MB | 68.85 ms |
| 2 | filesystem | 266.61 ms | 269.20 ms | 249.33 ms | 3,843 | 9.49 MB | 77.68 ms |
| 2 | ramfs | 246.07 ms | 249.99 ms | 247.44 ms | 4,163 | 9.49 MB | 68.65 ms |
| 4 | filesystem | 269.06 ms | 286.97 ms | 251.27 ms | 3,819 | 13.68 MB | 78.32 ms |
| 4 | ramfs | 246.71 ms | 247.70 ms | 249.09 ms | 4,151 | 13.68 MB | 69.20 ms |
| 8 | filesystem | 265.98 ms | 270.25 ms | 253.57 ms | 3,852 | 22.07 MB | 77.33 ms |
| 8 | ramfs | 245.91 ms | 246.46 ms | 250.18 ms | 4,164 | 22.07 MB | 69.09 ms |

The successor causal harness pins the report's file, protocol, content, and semantic-outcome hashes and refuses audit exposure if the comparator is missing, modified, dirty, incomplete, or contains a Protocol Deviation.
