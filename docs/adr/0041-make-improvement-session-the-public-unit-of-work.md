# Make Improvement Session the public unit of work

Consumers start an Improvement Session with one installed Domain Definition, one or more caller-supplied Optimization Goals, a Seed Scope selecting a Seed or reproducible Seed Source, a Resource Envelope, and optionally an existing Domain Bundle. The Session owns internal Campaigns, streams Pareto improvements, and returns a restart-complete updated bundle rather than exposing internal orchestration as the public workflow.

## Considered Options

Exposing individual search, training, and consolidation loops would offer fine control but require consumers to reconstruct Reflex's autonomous stack. A single blocking best-result call would hide multiobjective progress, durable learning, and continuation state.
