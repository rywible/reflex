# Require caller-supplied Optimization Goals

Reflex requires at least one caller-supplied Optimization Goal before autonomous improvement begins. A Domain Definition may provide reusable Goal templates, but the Runtime does not infer value from available Measurements or start work merely because a domain and Resource Envelope exist; it owns execution and decomposition, not intent.

## Considered Options

Automatically treating every directed Measurement as an Objective would make startup effortless but silently choose values and constraints the caller did not authorize. Domain-defined default Goals would keep intent external to the learned system but still allow improvement to begin without an explicit caller decision.
