# Keep experimental rigor outside the public interface

Reflex applies a strict internal experimental standard to claims about the library while keeping research terminology and workflow out of the consumer-facing Runtime interface. An internal Experimental Harness treats the deep Runtime module as a black box through the same interface consumers use, enforcing sealed corpora, equal budgets, replication, analysis, and reproducibility without making domain authors orchestrate experiments.

## Consequences

Research tooling is not a dependency of the core library, and public types do not expose Experiment Specifications, corpus seals, statistical tests, or promotion terminology. General operational capabilities needed by both consumers and experiments—deterministic configuration, resource limits, provenance, observability, and cold recovery—remain part of the Runtime interface on their own merits.
