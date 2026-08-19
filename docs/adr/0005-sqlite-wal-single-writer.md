# ADR 0005: SQLite WAL Single Writer Actor

## Status
Accepted

## Context
SQLite WAL allows concurrent readers but only one writer, and requires all clients to share the same host filesystem.

## Decision
Route all SQLite writes through a single dedicated actor thread. Reads use short-lived read-only connections. Multi-host distributed coordination uses PostgreSQL.

## Consequences
- Zero-service local developer installation.
- No SQLite multi-host locking contention or corruption.
