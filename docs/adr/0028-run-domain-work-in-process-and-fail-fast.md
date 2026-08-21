# Run domain work in-process and fail fast

Domain Operators and Verification Kernels execute in the Reflex process without a worker-process isolation layer. Fatal domain failures terminate the Runtime, which recovers from its durable state on restart; this accepts a larger failure boundary in exchange for the lowest dispatch overhead and simplest high-performance memory access.

## Considered Options

Supervised worker processes could contain panics, hangs, and memory faults, but batching, serialization, shared-memory coordination, and worker lifecycle management would impose complexity and overhead on the core local execution path.
