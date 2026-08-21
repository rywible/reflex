# Pin Verification to a domain kernel

Every Domain Definition identifies a minimal, versioned Verification Kernel whose judgment is the final authority for Correctness Claims under explicit semantics and assumptions. Each accepted Artifact retains replayable evidence binding its claim and assumptions to that exact semantics and kernel revision. Reflex may schedule, cache, or accelerate verification, but neither a learned model nor the optimizer may substitute for or overrule the pinned kernel.

## Considered Options

Trusting arbitrary optimizer assertions would make domain integration easy but collapse the distinction between candidate confidence and correctness. Requiring one universal proof kernel would strengthen uniformity but exclude verifiable domains whose native correctness procedure is exhaustive checking or another specialized decision procedure.
