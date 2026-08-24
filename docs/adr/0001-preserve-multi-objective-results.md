# Preserve multi-objective results

Reflex separates hard Verification from typed Measurements and run-specific Preferences. Domains define measurable facts, optimization runs define what they value, and Reflex retains a Pareto Frontier rather than collapsing results into one global scalar; this preserves useful trade-offs for future runs and prevents a temporary preference from becoming permanent training truth.

## Considered Options

A scalar cost would give search and training a convenient total ordering, but weights erase trade-offs and make archived results depend on one optimization context. Domain-owned preferences were also rejected because the same domain can serve deployments with different constraints.

## Consequences

Search and learning cannot assume every pair of candidates is ordered. Measurements must carry enough methodological and environmental provenance to support valid comparisons, and frontier growth must be controlled without silently scalarizing it.
