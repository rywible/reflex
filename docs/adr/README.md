# Architecture Decision Records

Reflex uses Architecture Decision Records (ADRs) to capture decisions that
change the constitutionally significant surface: invariants
(`docs/invariants.md`), dependency policy, storage formats, protocols, or
performance budgets.

## Process

1. Copy `TEMPLATE.md` to `NNNN-short-title.md` (next number).
2. Fill Context / Decision / Consequences; reference the master plan
   sections and any invariant IDs affected.
3. Status starts as `Proposed`; a decision becomes `Accepted` only after
   review. `Superseded`/`Deprecated` records must link to their
   replacement.
4. Dependency-policy decisions (pins, major-version splits, exceptions)
   must also update `deny.toml` and `deny-exceptions.json` in the same
   change.

## Index

| ADR | Title | Status | Date |
|---|---|---|---|
| 0001 | All-Rust Architecture | Accepted | — |
| 0002 | Burn and Micro Tiers | Accepted | — |
| 0003 | Fenced Distributed Leases | Superseded by 0014 for v1 | — |
| 0004 | DataFusion Dependency Isolation | Accepted | — |
| 0005 | SQLite WAL Single Writer | Superseded by 0014 for v1 | — |
| 0006 | Burn 0.22.0-pre.2 Pinned | Accepted | 2026-08-19 |
| 0007 | Arrow 58.3 / Parquet 58.3 / DataFusion 54.1 | Accepted | 2026-08-19 |
| 0008 | tokio 1.52.4 / rusqlite 0.40.1 Pins | Superseded by 0014 for v1 | 2026-08-19 |
| 0009 | Schema Compatibility Range | Accepted | 2026-08-19 |
| 0010 | Supply-Chain Policy (cargo-deny + Exception Registry) | Accepted | 2026-08-19 |
| 0012 | Local-First Core and Scientific Evidence | Accepted | 2026-08-20 |
| 0013 | Arrow and Parquet 58.4 Resolution | Accepted | 2026-08-20 |
| 0014 | Memory-Primary Runtime and Atomic Evidence Bundles | Accepted | 2026-08-20 |

ADRs 0001–0005 predate the date field in this template and are recorded
without one; their decisions are in force.
