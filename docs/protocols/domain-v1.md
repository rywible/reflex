# Domain Protocol v1 (`reflex.domain.v1`)

Normative specification of the Reflex domain wire protocol.

## Package and Source

`proto/reflex/domain/v1/domain.proto`, edition `2023`, package
`reflex.domain.v1`. Compiled with `prost` (0.14.x) in `reflex-protocol`.

## Envelope

Every frame carries a `Digest`:

```proto
message Digest {
  bytes digest = 1;   // 32-byte BLAKE3 digest of the payload
}
```

The digest covers the canonical encoding of the message body. A frame
whose digest does not match its payload is rejected before deserialization
— this is the transport-level integrity guarantee that underpins
INV-RFX-1 receipts.

## Messages

### `HandshakeRequest`

Sent by the client on connection establishment. Carries the client
`PROTOCOL_VERSION` and capability flags. A version mismatch is answered
with a compatibility error; the connection is closed (ADR 0009: a
mixed-version fleet must not silently interop).

### `HandshakeResponse`

Sent by the server. Carries the server protocol version and the accepted
frame-size limit (≤ `DEFAULT_MAX_FRAME_BYTES`).

### `ExpandBatchRequest`

```proto
message ExpandBatchRequest {
  CandidateGroup group = 1;
}
```

`CandidateGroup` carries state identifiers and candidate identifiers to
expand in one RPC (INV-RFX-14: no per-candidate process/IPC boundary).

### `ExpandBatchResponse`

```proto
message ExpandBatchResponse {
  repeated ScoredTransition transitions = 1;
}
```

One response for the whole batch. Partial results are not defined; a
batch either expands fully or fails.

## Flow

1. Client connects, sends `HandshakeRequest` (client version).
2. Server replies `HandshakeResponse` (server version, frame limit) or
   closes on mismatch.
3. Client sends `ExpandBatchRequest`; server replies
   `ExpandBatchResponse`.
4. Either side may close; the ledger records the attempt outcome at the
   metadata layer, not in the protocol.

## Versioning

The schema component `proto:1` (ADR 0009) corresponds to this document.
A wire-format change that older binaries cannot read bumps the component
and requires an ADR plus a migration plan.