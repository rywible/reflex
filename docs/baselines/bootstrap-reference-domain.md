# Bootstrap Reference Domain Baseline

Status: exploratory baseline; not Scientific Confirmation

Protocol: [`bootstrap-reference-domain-protocol.md`](./bootstrap-reference-domain-protocol.md)

Raw report: [`bootstrap-reference-domain.json`](./bootstrap-reference-domain.json)

Runtime revision: `ce586314e3ae9e88669ae8a6592d1cd399994c4c`

Report content SHA-256: `2d44b1b5e93354960eea975140060508d81ee5a0c83a54800823d37482efcaa5`

## Result

All 96 assigned release-mode processes completed without exclusion or Protocol Deviation. Every fresh and recovered Session produced the same 256-Artifact semantic outcome digest, `8c5e4703787465fece63053886929642901c74fdbd02ff4412d0a15fca366130`.

The honest result is that this Bootstrap implementation does not scale with additional workers. On the ordinary filesystem, p50 verifier throughput was 15,627 requests/s with one worker and 15,071 requests/s with eight workers, a 3.6% regression. On RAM-backed storage it was 24,957 requests/s and 24,659 requests/s respectively, a 1.2% regression. The current Runtime Controller performs the measured search and Verification batches serially inside its fixed pool, so extra workers add stack memory without providing Candidate-level parallelism.

Peak accounted resident memory rose from 4,979,411 bytes at one worker to 19,659,475 bytes at eight workers. This nearly fourfold increase is dominated by the explicitly reserved per-worker stacks. The final bundle retained 256 Pareto Artifacts in 574,681 bytes, or 2,244 bytes per Pareto Artifact by integer division. Each fresh Session consumed 770 Verification requests and emitted 769 observer additions.

Checkpoint storage is material at this workload size. One-worker p50 wall time was 50.06 ms on the ordinary filesystem and 30.86 ms on `/dev/shm`, a 38.4% reduction for RAM-backed atomic publication. Completed-bundle recovery showed the same pattern: 28.32 ms versus 19.52 ms, a 31.1% reduction. These paired storage results measure end-to-end checkpoint interference; they do not isolate a single syscall or claim that RAM-backed persistence is durable.

## Frozen comparator

| Workers | Storage | Wall p50 | Wall p95 | CPU p50 | Verifications/s p50 | Peak resident p50 | Recovery wall p50 |
| ---: | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | filesystem | 50.06 ms | 52.05 ms | 32.84 ms | 15,627 | 4.98 MB | 28.32 ms |
| 1 | ramfs | 30.86 ms | 31.15 ms | 31.38 ms | 24,957 | 4.98 MB | 19.52 ms |
| 2 | filesystem | 50.31 ms | 52.98 ms | 33.75 ms | 15,332 | 7.08 MB | 28.72 ms |
| 2 | ramfs | 31.00 ms | 31.37 ms | 31.63 ms | 24,852 | 7.08 MB | 19.54 ms |
| 4 | filesystem | 50.78 ms | 55.46 ms | 34.02 ms | 15,219 | 11.27 MB | 28.98 ms |
| 4 | ramfs | 31.03 ms | 31.19 ms | 31.82 ms | 24,827 | 11.27 MB | 19.62 ms |
| 8 | filesystem | 51.69 ms | 54.81 ms | 34.68 ms | 15,071 | 19.66 MB | 29.05 ms |
| 8 | ramfs | 31.23 ms | 31.42 ms | 32.41 ms | 24,659 | 19.66 MB | 19.68 ms |

The first learned challenger must be compared against the raw process-level records, not only this table. Equal-budget comparisons use one worker as the primary Bootstrap treatment until Candidate-scale parallel execution demonstrates a measured Session-level gain. Additional workers are currently a known memory and CPU cost, not a performance feature.
