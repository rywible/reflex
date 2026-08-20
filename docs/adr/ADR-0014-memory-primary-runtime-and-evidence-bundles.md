# ADR 0014: Memory-Primary Runtime and Atomic Evidence Bundles

## Status
Accepted

## Date
2026-08-20

## Context

Reflex v1 runs on one 64 GiB development workstation. Requiring SQLite,
PostgreSQL, a remote object store, leases, uploads, and portable worker
deployment adds serialization, copying, synchronization, dependencies, and
failure modes to a workload whose hot state fits in memory. Those seams do not
improve the scientific validity of a local experiment.

The requirements underneath the old storage and fleet design remain
constitutional: identities are content-derived, cells pin immutable inputs,
only one attempt may be accepted, stale work may not publish, evidence must
replay, publication must be atomic, and all cell-owned resources must reconcile
before completion.

This record supersedes ADR 0003, ADR 0005, the database portion of ADR 0008,
the native compatibility components in ADR 0009, and the optional distributed
seams retained by ADR 0012 for the v1 native path. It changes INV-RFX-9,
INV-RFX-10, INV-RFX-12, and INV-RFX-13 without weakening their safety or
reconstruction properties.

## Decision

1. The required v1 topology is one Rust process on one machine. Scheduling,
   metadata, manifests, attempt state, immutable artifacts, datasets, models,
   and knowledge editions are memory-primary. PostgreSQL, SQLite, remote
   object storage, worker fleets, and infrastructure deployment are not native
   v1 dependencies or release gates.
2. `ArtifactArena` is the required artifact authority while a run is active.
   It is content-addressed, immutable after insertion, capacity-bounded,
   allocation-accounted, and shared by digest. A capacity breach fails closed;
   the native path does not silently spill, upload, switch backends, or evict a
   pinned object. Large experiments are divided into independently bounded
   cells or generations.
3. Mutable coordination is a single-owner in-memory state machine. Every
   accepted attempt receives a monotonically increasing, nonzero attempt epoch.
   Publication and finalization compare `(cell_id, attempt_no, epoch)` with the
   current state, so delayed work from a cancelled or replaced attempt cannot
   commit even inside one process. Wall-clock leases and heartbeats are not the
   authority.
4. A run becomes durable only through an atomic local evidence bundle. The
   bundle contains a canonical manifest, ledger segments, all reachable
   immutable artifacts, verifier receipts, query identities, and checksums.
   The writer stages into a sibling temporary path, verifies every length and
   digest, flushes and fsyncs files, fsyncs the staged directory, atomically
   renames it to its digest-derived final name, and fsyncs the parent. Readers
   ignore temporary paths and reject incomplete, undeclared, duplicate, or
   digest-mismatched members.
5. Snapshotting is a barrier, not a background best-effort upload. The
   coordinator freezes an immutable root set and accepted-attempt view, drains
   authoritative ledger buffers, writes the bundle, reopens and verifies its
   manifest, and only then records publication success. A crash before rename
   leaves no published bundle; a crash after rename leaves a complete replay
   root.
6. Cleanup remains part of completion. An experiment cannot report completion
   while it owns compute permits, arena pins, buffers, ledger writers, external
   verifier processes, in-flight requests, or snapshot staging paths.
7. The native persisted compatibility line is
   `arena:1 bundle:1 ledger:1 proto:1`. The removed `sqlite` and `postgres`
   components do not describe the v1 native format. The SQL backends and
   service/worker binaries are removed from the workspace; reintroducing them
   requires a new ADR and cannot alter the native hot path.

## Invariant changes

| Invariant | Previous enforcement | New enforcement | Verification test |
|---|---|---|---|
| INV-RFX-9 | Atomic CAS object publication | Verified atomic evidence-bundle publication | `test_inv_9_no_partial_artifact_publication` |
| INV-RFX-10 | Database lease fencing | In-memory monotonic attempt-epoch compare-and-finalize | `test_inv_10_one_accepted_attempt` |
| INV-RFX-12 | Same-host single-writer SQLite | Single-owner in-memory coordination; no required SQL backend | `test_inv_12_no_shared_sqlite` |
| INV-RFX-13 | Bulk bytes referenced from a metadata database | Bounded bulk bytes live in `ArtifactArena`; evidence bundles preserve digest references | `test_inv_13_bulk_data_in_cas` |

The existing test names remain stable compatibility identifiers; their
assertions move to the new enforcing APIs.

## Consequences

- The hot pipeline avoids SQL, network storage, lease timers, backend dispatch,
  and duplicate serialization. Capacity and CPU ownership are explicit against
  the 64 GiB host instead of hidden in service caches.
- Runtime state is intentionally ephemeral until a snapshot barrier succeeds.
  This is acceptable for local v1: Reflex never claims an unsnapshotted run is
  durable scientific evidence.
- Content identity, verifier authority, replay, stale-attempt rejection,
  atomic publication, and cleanup survive the simplification.
- Cross-machine execution, live migration, remote durability, and recovery of
  an unfinished in-memory run are non-goals. Reintroducing any of them requires
  a new ADR and independent correctness and performance qualification; it must
  not complicate the native path by default.

## Related

- Supersedes: ADR 0003 and ADR 0005 for the required v1 path; ADR 0012 decision
  2.
- Master-plan sections: §0.5, §2, §3, §4, §5, §7, §9, §18, §23, §24, P2, P4,
  P11, and P16.
- Invariants: INV-RFX-2, INV-RFX-9, INV-RFX-10, INV-RFX-11, INV-RFX-12,
  INV-RFX-13, INV-RFX-15, INV-RFX-21, INV-RFX-23.
