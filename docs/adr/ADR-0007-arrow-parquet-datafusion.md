# ADR 0007: Arrow 58.3 / Parquet 58.3 / DataFusion 54.1

## Status
Accepted

## Date
2026-08-19

## Context
Reflex stores analytics shards as Parquet and evaluates them with
DataFusion, isolated in `reflex-analytics` (ADR 0004). Version drift
between Arrow, Parquet, and DataFusion is the most frequent source of
transitive `multiple-versions` splits in the workspace; the arrow/parquet
line is used by both the analytics tier and ML checkpointing paths.

DataFusion 54.1.0 declares `arrow ^58.3.0`, `parquet ^58.3.0`,
`object_store ^0.13.2`, and `tokio ^1.52` on crates.io; aligning the
workspace to that resolution removes split versions across the graph.

## Decision
Pin `arrow = 58.3`, `parquet = 58.3` (with default-features=false where
compression is unused), `datafusion = 54.1`, and keep the DataFusion
isolation boundary from ADR 0004: workers write Parquet shards to CAS and
never link DataFusion. These versions are the workspace's single Arrow /
Parquet / DataFusion line; any second semver-major of `arrow`, `parquet`,
or `datafusion` in the graph is a `deny.toml` bans failure (see ADR 0010).

## Consequences
- Single Arrow/Parquet line across analytics and ML checkpointing.
- Binary size and dependency churn stay inside `reflex-analytics`
  (ADR 0004).
- An Arrow/Parquet major upgrade is a cross-cutting change requiring a
  new ADR because it touches checkpoint readers, CAS artifacts, and
  analytics ingestion simultaneously.

## Related
- Master-plan sections: §4.5, §22.3
- Invariants: INV-RFX-13, INV-RFX-22
- ADRs: 0004, 0008, 0010