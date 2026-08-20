# ADR 0005: SQLite WAL Single Writer Actor

> **Superseded for the required v1 path by
> [ADR 0014](ADR-0014-memory-primary-runtime-and-evidence-bundles.md).**
> Native v1 coordination is single-owner and memory-primary; SQLite is not a
> release dependency.

## Status
Superseded by ADR 0014 for v1

## Context
SQLite WAL allows concurrent readers but only one writer, and requires all clients to share the same host filesystem.

## Decision
Route all SQLite writes through a single dedicated actor thread. Reads use short-lived read-only connections. Multi-host distributed coordination uses PostgreSQL.

## Consequences
- Zero-service local developer installation.
- No SQLite multi-host locking contention or corruption.
