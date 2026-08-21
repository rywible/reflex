# Protect long-horizon resource allocations

Each Resource Envelope includes minimum allocations for exploration, Verification, training, and Knowledge Consolidation, with strong library defaults; the Runtime Controller adapts the remaining capacity. This prevents an immature or self-reinforcing learned allocator from starving the activities that expose new knowledge, correct its predictions, and preserve trustworthy operation.

## Considered Options

Fully adaptive allocation could exploit current knowledge efficiently but may permanently suppress activities whose benefits arrive later. A fixed schedule would prevent starvation but discard the contextual economics that learned allocation is meant to improve.
