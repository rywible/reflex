# Require reproducible Seed Sources

A Domain Definition supplies one or more reproducible Seed Sources that emit Verified Artifacts with provenance. A source may enumerate a finite corpus or generate an unbounded deterministic stream, allowing Reflex to construct Campaigns autonomously while reproducing the exact inputs used by prior runs.

## Considered Options

Requiring callers to submit every Seed manually would make individual runs simple but prevent turn-it-on operation. Allowing an opaque or nondeterministic generator would expand coverage but make recovery, comparison, and Scientific Confirmation unreliable.
