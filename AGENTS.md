# AGENTS.md — Agent Engineering Guidelines

## Invariants
All work in this repository must strictly adhere to the constitutional invariants defined in `docs/invariants.md` and `docs/reflex-framework-master-plan-rust.md`.

## Workflow
1. Implement in the owning crate; do not expand scope without an ADR.
2. Do not weaken, skip, or silently work around an invariant (INV-RFX-21).
3. Verify with `cargo xtask check`. Performance gates live in `reflex-bench::PerformanceBudgetRegistry` and `docs/performance/`.
4. Scientific artifacts (ledgers, CAS objects, experiment reports) belong under `evidence/` by experiment name — not per-task stamp files.
