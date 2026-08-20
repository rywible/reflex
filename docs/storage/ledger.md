# Ledger Segment Format

Normative specification of the append-only event ledger
(`reflex-ledger`). This document defines the `ledger:<n>` schema
component (ADR 0009).

## Segment File

A segment file starts with the magic `b"RFXSEG01"` and contains an
append-only sequence of blocks. Segments are written once and never
rewritten; torn tails are detected and recovered at read time.

## Limits

| Constant | Value |
|---|---|
| `MAX_BLOCK_BYTES` | 16 MiB |
| `BLOCK_BATCH_LIMIT` | 512 events per block |

## Event Encoding

- Each event payload is serialized with `serde_json` (canonical field
  order) and integrity-checked with `crc32c` (`EventEncoder`).
- CRC mismatch marks the block as torn; recovery truncates at the last
  intact block boundary (`test_ledger_torn_tail_recovery`).
- The encoder is incremental and reuses buffers from `BufferPool`
  (INV-RFX-15) — encoding never allocates unbounded scratch memory.

## Segment Footer

```rust
pub struct SegmentFooter {
    pub block_index: u64,
    pub segment_digest: Digest,   // BLAKE3 over the block sequence
}
```

The footer binds the segment to its blocks; a segment whose digest does
not match is rejected. Footers make segments self-verifying and are the
basis for replayable reports (INV-RFX-11).

## Tests

- `test_block_header_roundtrip` — block header encode/decode stability.
- `test_event_encoder_batch` — batch encode/decode round-trip.
- `test_ledger_torn_tail_recovery` — torn-tail detection and truncation.
- `test_protocol_frame_roundtrip` (protocol crate) — frame codec
  interop with ledger event payloads.

## Versioning

Changing the magic, block layout, CRC, or footer structure bumps the
`ledger` component to 2 with a migration ADR; old segments must remain
readable (INV-RFX-24 spirit: history stays readable).