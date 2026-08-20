# ADR 0003: Fenced Distributed Leases

> **Superseded for the required v1 path by
> [ADR 0014](ADR-0014-memory-primary-runtime-and-evidence-bundles.md).**
> Monotonic attempt-epoch fencing remains required; PostgreSQL and
> wall-clock leases do not.

## Status
Superseded by ADR 0014 for v1

## Context
Stateless worker fleets claiming cells across PostgreSQL can experience network partitions, delayed finalizations, or worker restarts.

## Decision
Combine `SELECT ... FOR UPDATE SKIP LOCKED` with monotonic fencing tokens. Every heartbeat, artifact publication, and finalization includes the active token.

## Consequences
- At-most-one accepted attempt per cell.
- Stale workers cannot commit after lease expiration or replacement.
