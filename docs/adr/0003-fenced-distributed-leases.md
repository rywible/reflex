# ADR 0003: Fenced Distributed Leases

## Status
Accepted

## Context
Stateless worker fleets claiming cells across PostgreSQL can experience network partitions, delayed finalizations, or worker restarts.

## Decision
Combine `SELECT ... FOR UPDATE SKIP LOCKED` with monotonic fencing tokens. Every heartbeat, artifact publication, and finalization includes the active token.

## Consequences
- At-most-one accepted attempt per cell.
- Stale workers cannot commit after lease expiration or replacement.
