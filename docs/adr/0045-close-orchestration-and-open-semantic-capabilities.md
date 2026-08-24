# Close orchestration and open semantic capabilities

Consumers invoke one blocking, generic Improvement Session operation with a Domain Definition, request, and serialized Pareto observer; the result contains completion, Pareto state, resource usage, and a restart-complete Domain Bundle. Domain authors implement five explicit, statically dispatched, batch-oriented capability Modules—Structural Protocol, Seed Source, Operator Algebra, Verification Kernel, and Measurement Space—while Campaigns, search engines, learning, Admission, consolidation, promotion, scheduling, and durability remain private Runtime implementation.

Only the Runtime Controller may construct a Verified Artifact from an accepting Verification Kernel verdict. Capability methods reuse caller-owned buffers and per-worker scratch, domain code may not create its own worker pool, and no asynchronous runtime, boxed universal Artifact, dynamic Domain Definition dispatch, or per-Candidate callback appears at either public seam.

ADR 0048 later permits one narrow exception: a Verification Kernel may use pinned local domain-native worker processes when the actual trusted authority cannot be safely embedded in Rust. Those workers receive Runtime allowances, report resource usage, and have no search or orchestration authority.

## Considered Options

A single domain `execute` method over a closed command enum minimized method count but created a universal protocol with poor semantic locality and broad breakage when the Runtime evolves. A flat callback-bag Domain Definition made the common adapter look simpler but risked hiding structure, encoding, and kernel identity in shallow descriptors. Exposing session builders, search engines, trainers, or persistence ports would give callers flexibility by leaking the autonomous stack Reflex exists to own.
