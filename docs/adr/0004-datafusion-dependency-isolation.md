# ADR 0004: DataFusion Dependency Isolation

## Status
Accepted

## Context
DataFusion brings large Arrow and Parquet dependencies. Linking DataFusion into hot worker binaries increases binary size and dependency churn.

## Decision
Isolate DataFusion into a separate crate and binary (`reflex-analytics`). Workers write Parquet shards to CAS; analytics reads Parquet directly from storage.

## Consequences
- Worker binaries remain lean and fast to boot.
- Vectorized SQL analytics available without coupling to worker runtimes.
