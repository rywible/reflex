# Compress replay-complete Bundle segments

Domain Bundles compress their canonical Artifact and Experience segments with deterministic LZ4 blocks while checksums and all Runtime identities remain over the uncompressed logical bytes. Replay-complete Experience must retain refuted Candidates, but large formal Artifacts made raw checkpoints dominate storage and publication time; the framing layer owns compression so Domain Definitions and the public interface remain unaware of the physical encoding.

## Consequences

The safe pure-Rust encoder and decoder are part of the private Bundle format authority. Segment versions distinguish compressed payloads, legacy uncompressed v2 Artifact and Experience segments remain readable, and malformed expansion ratios are rejected before allocation. The decoder also accepts an explicit logical-byte ceiling; the Runtime derives that ceiling from the Resource Envelope and conservatively charges twelve resident bytes per expanded canonical byte while decoded structures overlap the framing buffers.

Changing only the Session seal preserves the already checksummed stored segments without expanding them. This keeps interruption publication proportional to the physical checkpoint instead of forcing replay-complete formal Artifacts back into memory during a failure path.
