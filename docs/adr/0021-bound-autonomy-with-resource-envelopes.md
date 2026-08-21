# Bound autonomy with Resource Envelopes

The caller grants Reflex a hard Resource Envelope for CPU concurrency, memory, durable storage, and elapsed or compute time. Reflex allocates those resources autonomously across Campaigns, search, Verification, training, and Knowledge Consolidation, and stops when the envelope or an explicit stop condition is reached rather than claiming that uncertain Potential has been exhausted.

## Considered Options

Requiring callers to schedule each activity would prevent autonomous operation. Stopping when learned Potential appears depleted would mistake a fallible prediction for evidence that no valuable discovery remains and could make resource use neither controllable nor reproducible.
