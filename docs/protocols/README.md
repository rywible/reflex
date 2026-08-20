# Protocols

Wire-format specifications for Reflex. These documents are normative:
implementations must match them, and schema bumps follow ADR 0009.

## Contents

- `domain-v1.md` — `reflex.domain.v1` protobuf protocol (handshake,
  batch expansion, digest framing).
- `framing.md` — length-delimited frame codec and size limits.

## Overview

`reflex-protocol` defines the client-server protocol between the reflex
daemon/trainer and domain workers, and between the fleet controller and
workers (ADR 0003). All frames are length-delimited and carry a
`reflex.domain.v1.Digest` envelope; the protocol version is
`PROTOCOL_VERSION = 1` and the maximum frame size is
`DEFAULT_MAX_FRAME_BYTES = 16 MiB`.

Invariants in force:

- Batch expansion never spawns per-candidate processes (INV-RFX-14):
  `ExpandBatchRequest` carries candidate groups, and the server returns
  `ExpandBatchResponse` with scores within one RPC.
- A frame that exceeds the negotiated limit is a protocol error; buffers
  are bounded (INV-RFX-15).