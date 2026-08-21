# Bootstrap Reference Domain Baseline

Status: exploratory baseline; not Scientific Confirmation

Protocol: [`bootstrap-reference-domain-protocol.md`](./bootstrap-reference-domain-protocol.md)

Raw report: [`bootstrap-reference-domain.json`](./bootstrap-reference-domain.json)

Runtime revision: `b7ea23a938163db3f9f271f8b3caedc031cd5348`

Report content SHA-256: `8b372665947cfd0acc6a8cf857f3da47a1e267a38c2d56592acd3ba056cd9d34`

## Result

All 96 assigned release-mode processes completed without exclusion or Protocol Deviation. Every fresh and recovered Session produced the same 256-Artifact semantic outcome digest, `8c5e4703787465fece63053886929642901c74fdbd02ff4412d0a15fca366130`.

The honest result is that this Bootstrap implementation does not scale with additional workers. On the ordinary filesystem, p50 verifier throughput was 14,849 requests/s with one worker and 14,609 requests/s with eight workers, a 1.6% regression. On RAM-backed storage it was 24,614 requests/s and 24,488 requests/s respectively, a 0.5% regression. The current Runtime Controller performs the measured search and Verification batches serially inside its fixed pool, so extra workers add stack memory without providing Candidate-level parallelism.

Peak accounted resident memory rose from 4,979,411 bytes at one worker to 19,659,475 bytes at eight workers. This nearly fourfold increase is dominated by the explicitly reserved per-worker stacks. The final bundle retained 256 Pareto Artifacts in 574,681 bytes, or 2,244 bytes per Pareto Artifact by integer division. Each fresh Session consumed 770 Verification requests and emitted 769 observer additions.

Checkpoint storage is material at this workload size. One-worker p50 wall time was 51.93 ms on the ordinary filesystem and 31.37 ms on `/dev/shm`, a 39.6% reduction for RAM-backed atomic publication. Completed-bundle recovery showed the same pattern: 29.24 ms versus 19.71 ms, a 32.6% reduction. These paired storage results measure end-to-end checkpoint interference; they do not isolate a single syscall or claim that RAM-backed persistence is durable.

## Frozen comparator

| Workers | Storage | Wall p50 | Wall p95 | CPU p50 | Verifications/s p50 | Peak resident p50 | Recovery wall p50 |
| ---: | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | filesystem | 51.93 ms | 52.69 ms | 33.91 ms | 14,849 | 4.98 MB | 29.24 ms |
| 1 | ramfs | 31.37 ms | 31.59 ms | 31.88 ms | 24,614 | 4.98 MB | 19.71 ms |
| 2 | filesystem | 51.32 ms | 54.06 ms | 34.42 ms | 15,084 | 7.08 MB | 28.69 ms |
| 2 | ramfs | 31.33 ms | 31.46 ms | 31.92 ms | 24,585 | 7.08 MB | 19.68 ms |
| 4 | filesystem | 53.24 ms | 54.31 ms | 34.67 ms | 14,627 | 11.27 MB | 28.92 ms |
| 4 | ramfs | 31.40 ms | 31.57 ms | 32.24 ms | 24,568 | 11.27 MB | 19.81 ms |
| 8 | filesystem | 52.78 ms | 55.09 ms | 35.48 ms | 14,609 | 19.66 MB | 29.21 ms |
| 8 | ramfs | 31.49 ms | 31.70 ms | 32.70 ms | 24,488 | 19.66 MB | 19.86 ms |

The first learned challenger must be compared against the raw process-level records, not only this table. Equal-budget comparisons use one worker as the primary Bootstrap treatment until Candidate-scale parallel execution demonstrates a measured Session-level gain. Additional workers are currently a known memory and CPU cost, not a performance feature.
