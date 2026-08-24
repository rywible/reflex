# Reserve Verification for correctness

Reflex uses Verified only for a domain-defined Correctness Claim that has been mechanically established under explicit formal semantics and assumptions. A domain supplies the claim and its Verifier, while tests, sampling, benchmarks, and learned confidence may guide search or produce Measurements but cannot confer Verified status; this keeps archived knowledge sound across domains without pretending that empirical performance is formally provable.

## Consequences

Verification has distinct Verified, Refuted, and Unknown outcomes so failure or exhaustion cannot masquerade as evidence. A Verified result records replayable evidence where available, the Verifier and semantics versions, and its assumptions; domain adapters that cannot provide a formal checking mechanism cannot produce Verified Candidates.
