# Storage

Storage specifications for Reflex's memory-primary native runtime and local
durable evidence. These documents are normative for the `arena:<n>`,
`bundle:<n>`, and `ledger:<n>` compatibility components (ADRs 0009 and 0014).

## Contents

- `arena.md` — bounded content-addressed runtime arena.
- `evidence-bundle.md` — atomic local durable publication.
- `ledger.md` — append-only segment ledger.
- `cas.md` — superseded filesystem/remote CAS compatibility notes.
- `metadata.md` — superseded SQL metadata compatibility notes.

## Overview

Reflex manages three kinds of data:

1. **Active artifacts** (proofs, checkpoints, shards) in a capacity-accounted
   `ArtifactArena`, keyed by digest (INV-RFX-13).
2. **Events** (transition outcomes, verifier receipts) in the append-only
   ledger; datasets and reports reconstruct from bundle members + ledger
   (INV-RFX-11).
3. **Mutable coordination** (cells, attempt epochs, promotions, knowledge
   editions) in one in-memory state machine. An atomic evidence bundle captures
   the accepted view and all reachable immutable bytes (INV-RFX-9/10/12).
