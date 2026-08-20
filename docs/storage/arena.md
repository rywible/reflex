# ArtifactArena

Normative native v1 specification for the `arena:1` compatibility component
(ADR 0014).

`ArtifactArena` is the content-addressed authority while a run is active. It
accepts immutable pooled bytes, derives or verifies their BLAKE3 identity,
deduplicates equal content, and returns read-only handles. Unique resident bytes
are charged before publication against the manifest's hard cap (48 GiB by
default on the canonical 64 GiB host). Capacity exhaustion is an explicit
error. Registered mode never spills, uploads, changes backend, evicts a pinned
object, or substitutes an unverified value.

Pins name their owner: cell, accepted attempt, active model/knowledge edition,
dataset job, or snapshot. Owner completion releases every pin. Cached unpinned
objects may be reclaimed by digest reachability; evidence roots remain pinned
until a verified evidence bundle publishes.

Insertion throughput, resident bytes, deduplication, pin reconciliation, and
whole-process RSS are release measurements. Digest mismatch, checked-size
overflow, capacity overflow, or leaked ownership fails closed.
