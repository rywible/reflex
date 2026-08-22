# Preserve domain proposal features

Operators may emit a small fixed-width Proposal Features channel alongside each Candidate so domain-significant proposal relationships survive generic Runtime batching, Experience retention, and training. The values are advisory, scoped by Semantic Identity, and default to zero; the Verification Kernel remains the sole correctness authority. This preserves useful facts such as a Lean library proof's syntactic proposition match without adding domain-specific orchestration to Reflex or forcing every domain author to implement learned search.

## Considered Options

Deriving every feature from the Candidate alone loses relationships to supporting library Artifacts. Persisting opaque variable metadata would enlarge the public and durable protocols without giving the Runtime a stable learning representation, while hard-coding Lean donor concepts in Reflex would violate domain independence.
