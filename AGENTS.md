# AGENTS.md — Engineering Guidelines

Reflex is a local, production-grade system for verified learning and search.
Its architecture evolves through working code, precise types, executable tests,
and measured performance—not approval documents or completion bookkeeping.

## Design

1. Organize around bounded contexts and their ubiquitous language.  A concept
   has one owner and one canonical representation.
2. Keep domain semantics independent of infrastructure.  Search, verification,
   evidence, experiment control, and learning communicate through narrow typed
   boundaries.
3. Make invalid and ambiguous states unrepresentable.  Digests are identities,
   not proof; only verification creates verified facts.
4. Prefer deletion and consolidation over adapters, compatibility layers,
   parallel control planes, and abstract capabilities with no implementation.
5. The native product is local and single-process.  Do not add CI, cloud
   storage, remote coordination, service deployment, or non-local assumptions.

## Quality

1. Correctness is demonstrated through production-path tests, property tests,
   differential references, concurrency tests, fault injection, and replay.
2. Bounded resources, cleanup, allocation behavior, and end-to-end throughput
   are product behavior.  Measure them on representative workloads.
3. Keep the hot path typed, synchronous, batched, reusable, and free of
   serialization, filesystem work, dynamic allocation per action, and hidden
   thread pools.
4. Do not preserve backward compatibility unless it simplifies the resulting
   design.  Migrate call sites and delete the obsolete surface.
5. Comments explain irreducible constraints and safety reasoning.  Names,
   types, module boundaries, and tests document the rest.

## Workflow

1. Work in the owning bounded context; make cross-context changes only when the
   domain boundary itself is being improved.
2. Run focused tests while iterating, then `cargo xtask check` before handoff.
3. `cargo xtask check` validates code and behavior, not generated reports,
   document indexes, schema stamps, SBOM stamps, or other repository paperwork.
4. Genuine experiment outputs belong under `evidence/<experiment>/`.  Never
   create task stamps, completion ledgers, or manual status artifacts.
