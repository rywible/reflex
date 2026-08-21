# Bootstrap Reference Domain Baseline v4

Status: exploratory baseline; not Scientific Confirmation

Protocol: [`bootstrap-reference-domain-v4-protocol.md`](./bootstrap-reference-domain-v4-protocol.md)

Raw report: [`bootstrap-reference-domain-v4.json`](./bootstrap-reference-domain-v4.json)

Runtime revision: `d7e7ac107b54918ebfbc3c0bbfbf7f3ba38d39c3`

Report content SHA-256: `4b59781f71a097a7b0d48d4630b090abdc213dbed8bb9b9bd029ae660650230b`

## Result

All 96 assigned release-mode processes completed without exclusion or Protocol Deviation. Every fresh and recovered Session produced the same 256-Artifact semantic outcome digest for the expanded Domain Identity, `339c0c6d5e20b4c8af292e7b32699576e474bc7828e451481d36e31d1bdb070f`.

Version 4 supersedes v3 because wrapping-add and rotate-left changed the Reference Domain's Semantic Identity even though this comparator deliberately retains the same XOR-only Development Corpus. Fresh search still performs 1,024 Verification requests; completed recovery performs 1,793. No future Sealed Audit semantic functions were generated or inspected.

The one-worker p50 was 203.06 ms on the ordinary filesystem and 184.05 ms on `/dev/shm`, corresponding to 5,047 and 5,565 whole-Session verifications/s. Those results are within 0.8% of v3 wall time. The final 931,571-byte bundle and 6,919,994-byte accounted resident peak differ from v3 only by the longer Semantic Identity. One-worker completed recovery took 74.92 ms and 66.09 ms, respectively.

Additional workers remain a memory cost without throughput benefit. From one to eight workers, filesystem p50 throughput changed from 5,047 to 5,013 verifications/s, while RAM-backed throughput changed from 5,565 to 5,507.

## Frozen comparator

| Workers | Storage | Wall p50 | Wall p95 | CPU p50 | Verifications/s p50 | Peak resident p50 | Recovery wall p50 |
| ---: | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | filesystem | 203.06 ms | 204.13 ms | 186.65 ms | 5,047 | 6.92 MB | 74.92 ms |
| 1 | ramfs | 184.05 ms | 184.39 ms | 184.78 ms | 5,565 | 6.92 MB | 66.09 ms |
| 2 | filesystem | 203.39 ms | 215.29 ms | 187.57 ms | 5,044 | 9.02 MB | 74.47 ms |
| 2 | ramfs | 184.92 ms | 185.26 ms | 185.73 ms | 5,542 | 9.02 MB | 66.73 ms |
| 4 | filesystem | 205.56 ms | 208.42 ms | 188.79 ms | 4,986 | 13.21 MB | 75.56 ms |
| 4 | ramfs | 184.87 ms | 185.23 ms | 185.92 ms | 5,543 | 13.21 MB | 66.88 ms |
| 8 | filesystem | 204.86 ms | 208.12 ms | 189.11 ms | 5,013 | 21.60 MB | 75.74 ms |
| 8 | ramfs | 186.04 ms | 187.92 ms | 187.67 ms | 5,507 | 21.60 MB | 67.47 ms |

Equal-budget causal comparisons use one worker as the primary Bootstrap treatment. This is an operational comparator only; scientific claims require the separately frozen causal protocol and untouched semantic groups.
