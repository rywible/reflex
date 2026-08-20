# Performance

Performance budgets for Reflex. Every measurement below is tied to an
invariant row in `docs/invariants.md` (performance-measurement column)
and is enforced by `reflex-bench::PerformanceBudgetRegistry`
(INV-RFX-22).

## Budgets

Budgets are typed contracts, not an assumption that every measurement is a
p95 latency. A registry entry declares:

- the measured quantity (`latency`, `throughput`, CPU ratio, utilization,
  resident bytes, scratch amplification, or artifact bytes);
- direction and strictness (`at_most`, `less_than`, `at_least`, or
  `greater_than`);
- unit and reduction (`p95`, `p99`, rate/ratio of sums, or maximum); and
- host class, owner, threshold, and normative authority.

`MetricMeasurement` retains numerator/denominator observations. Gate values
must reconstruct bit-for-bit from those observations before comparison. This
prevents a throughput field from being treated as latency, a percent from
being treated as bytes, or a hand-written summary from passing as evidence.

The exact quantitative local gates currently encoded from the master plan
include candidate construction, dense feature packing, uniform scoring,
policy/evidence CPU ratios, ledger recovery, dataset compaction/compilation,
training-loader CPU, the 3K/1M training epoch, thread-permit latency, micro-MLP
latencies, checkpoint scratch amplification, CLI startup, local operator API
reads, and observability overhead. `PerformanceBudgetRegistry::registration`
also contains an explicit `open` entry and reason for every named release gate
whose requested statistic has no master-plan threshold. An open registration
never passes through `get`, `check_regression`, or `check_measurement`.

ADR 0014 supersedes PostgreSQL, SQLite, multi-worker coordination, the 8 GiB
worker RSS class, remote object-store, and deployable worker-image gates. They
must be removed from the v1 acceptance inventory rather than reinterpreted.
The replacement measurements are arena insertion and resident bytes,
whole-process RSS, local all-core utilization, in-memory claim/finalize,
evidence-bundle snapshot throughput, and local CLI startup. A replacement
remains open until its typed registry entry and raw observations exist.

### Search (INV-RFX-8, INV-RFX-15, INV-RFX-16, INV-RFX-17)

| Metric | Budget | Notes |
|---|---|---|
| Search CPU per node | ≤ budget per node (§3.1) | Includes ML overhead: `SearchStats::ml_overhead_ns` must be accounted (INV-RFX-8) |
| Frontier memory | bounded per `BufferPool` capacity | Overflow must backpressure, never unbounded allocation (INV-RFX-15) |
| Thread-permit acquisition | bounded per `ThreadBudget` | Single CPU budget broker (INV-RFX-16) |
| Frontier ordering | deterministic; cost per node tracked | `FrontierKey` `Ord` is the single ordering authority (INV-RFX-17) |

### Memory and durability (INV-RFX-9, INV-RFX-12, INV-RFX-13)

| Metric | Budget | Notes |
|---|---|---|
| Arena insertion throughput | ≥ 1 GiB/s/core | Includes BLAKE3 identity and unique-byte accounting |
| Arena resident bytes | ≤ manifest cap | Default 48 GiB; overflow fails before publication (INV-RFX-13) |
| Whole-process peak RSS | < 60 GiB | Canonical 64 GiB local host retains OS/verifier headroom (INV-RFX-15) |
| Atomic bundle snapshot | ≥ 500 MiB/s | ≥1 GiB bundle before final fsync; interrupted staging never publishes (INV-RFX-9) |
| Ledger append latency | ≤ budget p99 | CRC + buffer-pool reuse on the hot path |
| In-memory claim/finalize | p95 ≤ 10 µs | Single-owner state and monotonic attempt epochs (INV-RFX-10/12) |

### Protocol (INV-RFX-14, INV-RFX-15)

| Metric | Budget | Notes |
|---|---|---|
| Batch expansion latency p99 | ≤ budget | One RPC per batch; no per-candidate process (INV-RFX-14) |
| Frame processing | bounded | 16 MiB max frame; oversized prefixes rejected pre-allocation (INV-RFX-15) |

### Completion cleanup (INV-RFX-23)

| Metric | Budget | Notes |
|---|---|---|
| Cell cleanup | zero owned resources at completion | `CellContext::check_clean_shutdown` rejects live compute permits or checked-out buffers before completion is recorded (INV-RFX-23) |

## Enforcement

- `PerformanceBudgetRegistry` gates benchmarks; a regression beyond an
  accepted budget fails the promotion gate (INV-RFX-22).
- `BenchmarkRecord` remains the compatibility envelope for p95 latency and
  rate benchmarks. Ratios, utilization, RSS, artifact size, and scratch
  amplification use `MetricMeasurement`; attempting to force those through a
  timing record fails closed.
- The same registry owns the canonical-scientific deep-suite baselines for
  Loom, Turmoil, fuzz smoke, and recovery. `cargo xtask check-deep` rejects a
  host-class mismatch and any suite duration more than 20% above its accepted
  baseline.
- Benchmarks live under `benches/` and run via `cargo bench`.
- Budget changes are performance-is-correctness changes: they require an
  ADR and a re-run of the affected invariant tests.

Deep-suite timings remain transient CI telemetry; only the reviewed baselines
and normalized pass/fail report are versioned.
