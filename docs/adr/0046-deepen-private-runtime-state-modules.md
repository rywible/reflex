# Deepen private Runtime state modules

Reflex keeps the public Improvement Session and Domain Definition interfaces unchanged while concentrating private Runtime invariants behind six deep Modules. Admission uses an atomic Epoch Transition; restart-complete semantic recovery and sealing use one codec over a single unpublished canonical framing crate; the Experience Ledger owns attempts, delayed consequences, Measurement observations, persistence, accounting, and learning/consolidation projections; Resource Envelope enforcement accepts typed live, transient, and pending-durability reservations; the internal Experimental Harness owns guards, child supervision, evidence capture, host metadata, and report primitives; and the Goal Evaluator owns the operational interpretation of caller Goals.

These Modules are private implementation seams. Domain Bundles remain data-only, Experimental Harness language does not enter consumer interfaces, Preference is never scalarized, and Search Frontier allocation remains distinct from per-Goal Pareto retention. Existing bundle bytes and the public library interface remain compatible.

## Considered options

Leaving invariants in the Runtime Controller made every change coordinate parallel collections, duplicated rollback, repeated resource arithmetic, and two experimental process implementations. Adding public extension interfaces would move that complexity to consumers and violate the closed-orchestration decision. Compatibility wrappers around the old paths would create two implementations of each invariant, so the scattered paths are replaced once characterization and recovery tests pass.
