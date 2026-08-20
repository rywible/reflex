# Ledger and CAS

Inspect what a run persisted. Formats are normative in
`docs/storage/` (`ledger.md`, `cas.md`).

## Ledger segments

Segments are append-only files starting with the magic `b"RFXSEG01"`,
blocked at `MAX_BLOCK_BYTES` (16 MiB) with `BLOCK_BATCH_LIMIT` (512)
events per block, CRC-protected per block (`crc32c`), and sealed by a
`SegmentFooter { block_index, segment_digest }`.

```bash
# Locate segments under the run's data directory, then verify a segment:
# digest must match the block sequence (self-verifying footer).
```

A torn tail (power loss mid-write) is detected at read time and truncated
at the last intact block; this is tested by
`test_ledger_torn_tail_recovery`.

## CAS objects

Objects are addressed by BLAKE3 digest and published atomically
(INV-RFX-9):

```bash
# FsArtifactStore layout: one file per digest under the store root.
# Retrieval verifies the digest; a mismatch is a hard error, never a
# silent fallback (INV-RFX-21).
```

## Reconstruction

Strict reports rebuild from CAS + ledger only (`ScientificReport`),
producing canonical JSON that is byte-for-byte replayable
(INV-RFX-11). If an object is missing, reconstruction fails loudly —
it never substitutes another object.

## Verify

```bash
cargo test -p reflex-integration-tests --test recovery_test
cargo test -p reflex-integration-tests --test e2e_test
```