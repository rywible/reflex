# Local Run State

Native v1 has no metadata database. `reflex-engine::LocalRunState` is the one
mutable coordination authority for a running process (ADR 0014).

## Representation

- Experiments, generations, and cells occupy capacity-bounded dense vectors.
- Content identities map to vector slots only at API boundaries.
- A deterministic heap selects the highest-priority ready cell, breaking ties
  by ascending canonical `CellId`.
- One owner mutates the state; there are no locks, transactions, leases,
  heartbeats, SQL encoders, or backend dispatch on the coordination path.

## Attempt authority

`LocalRunState::claim_next` mints a private-field `AttemptTicket` containing
the immutable cell identity, attempt number, epoch, and manifest digest.
Retry and cancellation advance the epoch. `LocalRunState::finalize` accepts a
result only when every ticket field still matches, so delayed work cannot
publish (INV-RFX-10).

Finalization also requires `reflex-cas::CommittedEvidence`. Only
`LocalEvidenceBundleStore` can mint that capability after an evidence bundle
has been fully written, reopened, and digest-verified (INV-RFX-9/11).

## Recovery

The process restores the last valid `CURRENT` evidence bundle, increments the
local execution epoch, and deterministically re-executes work after that
barrier. Volatile post-barrier work is never inferred to have completed.

The persisted compatibility line is `arena:1 bundle:1 ledger:1 proto:1`.
