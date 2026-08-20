# External Domain Protocol

Native Reflex is one local process. The wire protocol exists only for
persistent external domain/verifier children such as Lean or Wrela; there is
no coordinator daemon or general worker fleet (ADR 0014).

The protocol is `reflex.domain.v1` (`proto/reflex/domain/v1/domain.proto`).
Frames are length-delimited, correlated, capacity-bounded, and carry an opaque
payload envelope. A fail-closed handshake negotiates the exact schema,
capabilities, batch limits, and maximum frame size before semantic requests are
accepted.

Candidate expansion and application are batched. Per-candidate IPC is forbidden
by INV-RFX-14. The host quarantines duplicate, late, mismatched, and oversized
responses; a child cannot publish accepted evidence directly.

Verify framing and the full semantic RPC surface with:

```bash
cargo test -p reflex-integration-tests --test protocol_test
cargo test -p reflex-protocol
cargo test -p reflex-domain-host
```

Completion is local and explicit:

- `CellContext::check_clean_shutdown` rejects live permits and checked-out
  buffers.
- `DomainWorkerSupervisor::shutdown_and_drain` terminates the child and
  reconciles in-flight requests.
- `LocalRunState::finalize` requires the current attempt ticket and a committed
  evidence capability.
