# Compress replay-complete Bundle segments

Domain Bundles compress their canonical Artifact and Experience segments with deterministic LZ4 blocks while checksums and all Runtime identities remain over the uncompressed logical bytes. Replay-complete Experience must retain refuted Candidates, but large formal Artifacts made raw checkpoints dominate storage and publication time; the framing layer owns compression so Domain Definitions and the public interface remain unaware of the physical encoding.

## Consequences

The safe pure-Rust encoder and decoder are part of the private Bundle format authority. Segment versions distinguish compressed payloads, legacy uncompressed v2 Artifact and Experience segments remain readable, and malformed expansion ratios are rejected before allocation. The decoder also accepts an explicit logical-byte ceiling; the Runtime derives that ceiling from the Resource Envelope and conservatively charges twelve resident bytes per expanded canonical byte while decoded structures overlap the framing buffers.

Encoding has a separate codec-owned admission plan. It derives the maximum stored output from canonical framing and the compressor's published bounds rather than from the durable envelope, and accounts for simultaneously live logical payloads, canonical indexes, compression/shrink overlap, final output capacity, and durability ownership before any of those allocations. The bounded encoder observes the actual compressed lengths and rejects an oversized output before allocating the final Bundle.

Verified Artifact materialization retains its exact immutable canonical Bundle record. Every later seal orders borrowed record pointers and performs only bounded copies; it does not call domain Structure, claim, or evidence encoders. Domain encoding scratch is therefore charged at ingress where it is actually used, never hidden inside restart publication.

Changing only the Session seal preserves the already checksummed stored segments without expanding them. The framing scan computes and admits the exact replacement size before output allocation. This keeps interruption publication proportional to the physical checkpoint instead of forcing replay-complete formal Artifacts back into memory during a failure path.
