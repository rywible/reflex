# v1 release acceptance

`cargo xtask acceptance` is the authoritative, read-only evaluator for the P16
release gate. It exits unsuccessfully until every gate is present and valid.
`cargo xtask acceptance --write` additionally records the result at
`evidence/v1/acceptance.json`; that single closure report is not per-task
bookkeeping.

Acceptance records a dirty worktree as the blocking `clean_exact_head` gate,
continues evaluating every other blocker, and exits unsuccessfully. Fast,
deep, invariant, scientific, and performance evidence must all name the exact
full Git commit being released; a short or stale commit is rejected.

The evaluator requires successful, unskipped fast and deep check reports, the
invariant closure report, and a byte-current CycloneDX SBOM. Scientific gates
must be reconstructable reports from their experiment directories:

| Gate | Required report |
|---|---|
| Bit-vector dogfood | `evidence/bitvec-v1/report.json` |
| Local fault matrix | `evidence/fault-matrix/report.json` |
| Wrela campaign | `evidence/wrela-campaigns/report.json` |
| Lean M1.5 | `evidence/lean-m1.5/reconstruction.json` |
| Lean M2A | `evidence/lean-m2a/reconstruction.json` |
| Lean M2B | `evidence/lean-m2b/report.json` |
| Knowledge economy | `evidence/knowledge-economy/report.json` |
| Security fault fixtures | `evidence/security-audit/report.json` |
| New-domain handoff | `evidence/new-domain-handoff/report.json` |

Each scientific report uses the gate-specific schema enforced by the
evaluator. It must state `status: "passed"`, `reconstructed: true`, the exact
commit, distinct nonzero experiment-manifest/query-plan/reconstruction receipt
identities, named nonempty populations, and named checked queries that bind a
population and unit. Those three reconstruction identities must occur in a
sorted, exhaustive list of source files with BLAKE3 digests. Sources must
remain within that experiment's evidence directory; undeclared files,
symlinks, and mutated bytes fail closure. The report's `evidence_digest` must
reconstruct from the complete report body. Domain-specific reconstructors remain responsible
for producing those reports from verified evidence bundles containing immutable
ledgers, arena artifacts, receipts,
populations, and checked query plans. The evaluator never creates a passing
scientific artifact and never treats a missing external tool or campaign as a
pass.

Performance closure is evaluated separately from code-health and deep-suite
duration. Every initial v1 quantitative gate in master-plan §3.3 plus the P16
operator/release gates has a named entry in the evaluator. A gate is
`open` unless its name exists in `PerformanceBudgetRegistry` and
`evidence/performance/<gate>.json` contains a self-digested
`reflex.performance-evidence.v2` envelope. The enclosed typed
`MetricMeasurement` must exactly match the registry's metric, direction, unit,
and statistic and reconstruct its scalar from retained numerator/denominator
observations (latency percentiles require at least 20 samples). Its
`HostCalibration` must be canonical, reconstructable, tied to the exact commit
and registered host class; and its source files are rehashed like scientific
evidence. Thus an unregistered arena, bundle, protocol, runtime, search,
training, reporting, API, observability, sandbox, or
startup budget remains visibly open even when `cargo xtask check-deep` passes.

Generate and verify the supply-chain artifact with:

```bash
cargo xtask sbom
cargo xtask sbom --verify
```

The committed CycloneDX document is derived from `Cargo.lock`. SQL and remote
object-store crates are not native v1 dependencies; if legacy compatibility
adapters are still workspace members, the SBOM must label them non-default and
acceptance must prove the registered native pipeline did not select them.
