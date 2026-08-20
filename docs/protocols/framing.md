# Framing (Length-Delimited Frames)

Normative specification of the Reflex frame codec
(`reflex-protocol::LengthDelimitedFrameCodec`).

## Format

Each frame on the wire is:

```
[ 4-byte big-endian length ][ payload bytes ]
```

- The length prefix counts the payload bytes only.
- The payload is the canonical encoding of the protobuf message
  (see `domain-v1.md`), preceded by its `Digest` envelope.
- Length prefixes larger than `DEFAULT_MAX_FRAME_BYTES` (16 MiB) are a
  protocol error: the codec rejects the frame without allocating a buffer
  of that size (INV-RFX-15 bounded memory).

## Limits

| Constant | Value |
|---|---|
| `PROTOCOL_VERSION` | 1 |
| `DEFAULT_MAX_FRAME_BYTES` | 16 MiB |

The negotiated limit from `HandshakeResponse` is authoritative for a
connection; a sender exceeding it is disconnected.

## Codec Tests

- `test_protocol_frame_roundtrip` — encode/decode round-trip stability.
- Fuzz targets under `reflex-fuzz` exercise malformed prefixes and
  truncated payloads; the codec must reject without panic or
  unbounded allocation.

## Versioning

Framing is part of the `proto:1` schema component (ADR 0009). Changing
the prefix width or adding framing options bumps the component.