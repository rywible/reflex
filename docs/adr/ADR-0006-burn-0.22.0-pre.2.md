# ADR 0006: Burn 0.22.0-pre.2 Pinned

## Status
Accepted

## Date
2026-08-19

## Context
Reflex's ML tier (training, checkpointing, inference) runs on Burn. Burn
has no stable release compatible with the toolchain and feature set Reflex
needs; the last verified release line is `0.22.0-pre.2`. A newer Burn
would change the tensor/ndarray split and the checkpoint format, touching
`reflex-ml-burn` and the stored-model compatibility surface (§13.5,
INV-RFX-3). Upgrading is therefore a planned migration, not an ad-hoc
bump.

`0.22.0-pre.2` pulls two policy-exception dependencies:
- `ndarray 0.17` (transitive split; the rest of the graph resolves 0.16);
- `paste 1.0.15` (unmaintained advisory RUSTSEC-2024-0436, build-time
  proc-macro, no safe upgrade);
- `colored 3.1.1` (MPL-2.0, weak copyleft, transitive via burn-tensor).

## Decision
Pin Burn at `0.22.0-pre.2` until a stable release is evaluated. Do not
adopt a new Burn major/minor without an ADR that verifies checkpoint
compatibility. The `ndarray@0.17.2` split is `skip-tree`-listed in
`deny.toml` and the advisory is ignored with a registry entry; both
expire with this ADR.

## Consequences
- Stable, reproducible ML tier on the pinned pre-release.
- Three exception-registry entries (ndarray-2026-08-19,
  mpl-colored-2026-08-19, paste-unmaintained-2026-08-19) with expiry
  2027-08-19; renewal requires a new ADR.
- Burn upgrade becomes a tracked migration with checkpoint-format
  verification, superseding this ADR.

## Related
- Master-plan sections: §13, §3.2
- Invariants: INV-RFX-3, INV-RFX-8
- ADRs: 0010 (exception registry), 0002