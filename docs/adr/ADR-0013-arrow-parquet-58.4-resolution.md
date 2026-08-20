# ADR 0013: Arrow and Parquet 58.4 Resolution

## Status

Accepted

## Date

2026-08-20

## Context

ADR 0007 selected the Arrow/Parquet 58.3 line used by DataFusion 54.1. Cargo's
current registry resolution for DataFusion 54.1's `^58.3.0` requirement
provides Arrow and Parquet 58.4.0; the workspace's former `58.3` requirements
therefore resolved silently to 58.4.0. Leaving the manifest loose while the
ADR named another version made the dependency authority non-reconstructable.

The change remains within the same Arrow and Parquet semver-major and does not
alter Reflex's logical dataset schema. It does affect binary codecs and must be
qualified as one workspace-wide analytics/storage change.

## Decision

Pin `arrow = =58.4.0`, `parquet = =58.4.0`, and `datafusion = =54.1.0` in the
workspace. Keep a single Arrow/Parquet major in the resolved graph. Dataset
schema fingerprints, Parquet round trips, analytics queries, fuzz decoders,
and the full verification lane must pass before the resolution is accepted as
release evidence.

Any later Arrow, Parquet, or DataFusion version change requires an ADR because
the types cross dataset, analytics, and checkpoint validation boundaries.

## Consequences

- Cargo resolution now matches the documented and SBOM-recorded versions
  exactly.
- Patch upgrades are deliberate instead of silently selected by a caret range.
- Persisted logical schema IDs remain unchanged; byte-level shard digests
  naturally change if the encoder output changes.

## Related

- Supersedes the version selection in ADR 0007 while retaining its isolation
  and single-major decisions.
- Master-plan sections: §4.5, §22.3, P8, P16.7.
- Invariants: INV-RFX-13, INV-RFX-22.
- ADRs: 0004, 0007, 0010.
