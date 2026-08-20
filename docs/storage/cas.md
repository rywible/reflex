# Content-Addressed Artifact Store (CAS)

> **Superseded for the required native v1 path by ADR 0014 and
> [`arena.md`](arena.md).** Filesystem and remote implementations are legacy
> compatibility adapters, not registered-run or release-gate dependencies.

Legacy compatibility specification for filesystem and remote implementations
still present in `reflex-cas`. It is non-normative for registered v1 runs.

## Object Model

- Every object is addressed by `Digest` (BLAKE3, 32 bytes).
- Objects are immutable once published (INV-RFX-9): publication is
  atomic — write to a temp path, fsync, rename into place; readers never
  observe partial objects.
- Read paths verify the digest on retrieval; a mismatch is a hard error
  (`StoreError::DigestMismatch`), never a silent fallback (INV-RFX-21).

## Implementations

| Implementation | Use |
|---|---|
| `FsArtifactStore` | Legacy local compatibility store |
| `MemoryArtifactStore` | Test compatibility store; replaced natively by bounded `ArtifactArena` |
| `ObjectArtifactStore` | Legacy remote compatibility store |

All implement the `ArtifactStore` trait surface:

- `put_stream` / `put_bytes` — atomic publication (INV-RFX-9).
- `head` — existence + size check without transfer.
- `get_bytes` — verified read.

## Retention

Objects carry a `RetentionClass`. GC only removes objects whose class
permits it and that are not referenced by ledger segments or knowledge
editions. Bulk data lives here — never in metadata rows (INV-RFX-13).

## Tests

- `test_inv_9_no_partial_artifact_publication` — a torn write must never
  be observable as a complete object.
- Unit tests cover digest-mismatch rejection and retention-class GC
  behavior.
