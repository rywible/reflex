# Share immutable Lean expression subtrees

Lean proof terms use persistent `Arc`-linked expression trees so Candidate clones share unchanged subtrees and transformations allocate only their rebuilt structure. Deep `Box` cloning made one bounded generation hold tens of gigabytes, while a centralized arena would couple otherwise portable Artifacts and complicate cross-Artifact composition; canonical encoding still serializes the logical tree and never persists pointer identity or sharing layout.

## Consequences

Reference counting is confined to the Lean Adapter's immutable physical representation. Runtime Artifact identity, hashing, Verification, and Domain Bundle encoding remain content-based, and resident accounting remains conservative against the expanded logical tree.
