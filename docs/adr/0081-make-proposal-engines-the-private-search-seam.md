# Make Proposal Engines the private search seam

Candidate generation varies behind one private `ProposalEngine` interface. The first two adapters are the Structured Rewrite Engine, which pages deterministic Primitive Operator applications, and the Derived Operator Engine, which expands verified Knowledge into bounded multi-step proposals. Each adapter consumes eligible parents and engine-specific restart progress, appends Candidates, and reports its bounded resident demand and whether work was truncated. Verification, Candidate ranking, Admission, resource authority, durability, and publication remain outside the seam.

This change is deliberately internal and incremental. `OperatorAlgebra` remains a required Domain Definition capability and continues to implement the existing structured adapter. The Runtime does not expose Proposal Engines to consumers or ask Domain authors to own search orchestration. A later Semantic Identity revision may make structural, constraint, proof, abstraction, or stochastic capabilities optional only after at least one independent non-rewrite engine proves that the broader public contract is real.

## Considered Options

Keeping primitive and Derived-Operator generation as unrelated functions in the central Runtime made every new search method reopen scheduling and lifecycle code. Making Proposal Engines public immediately would widen the Domain Definition before a second capability family exists and would shift autonomous orchestration back onto domain authors. Replacing `OperatorAlgebra` in one incompatible change would also discard the statically dispatched, batched, replayable path already exercised by both production domains.

## Consequences

- Search methods now have one private seam where generation behavior can vary without acquiring correctness or allocation authority.
- The Structured Rewrite and Derived Operator adapters preserve the current candidate order, cursor semantics, features, provenance, and resource accounting.
- Engine selection and resource allocation can become hierarchical without making one engine's cursor or grammar universal.
- Making the minimum public Domain Definition thinner remains future work and requires a real adapter that does not depend on `OperatorAlgebra`.
