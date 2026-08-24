# Unary `u8` causal confirmation v2: interrupted run

Status: **Protocol Deviation — no confirmatory claim**

The preregistered v2 run was manually interrupted on 2026-08-21 after three of forty assignments completed. The active fourth assignment and the remaining thirty-six assignments were not completed. The stopping rule required every assignment exactly once, so neither a causal contrast nor a null effect may be inferred from this run.

The run used git revision `382e5dec1eaa4e1420339e3effdeee55834bd0e3` and frozen specification SHA-256 `1a5b592bdb7e74c98deebefa2a35ff5c196fcbfb94e9a1df86b1f6e906cfbb5e`.

## Consumed audit corpus

All ten exposed corpora are retained in [`u8-causal-confirmation-v2-consumed-audit.json`](./u8-causal-confirmation-v2-consumed-audit.json). They contain 81,900 globally unique semantic groups and are development data for every later experiment.

- Canonical corpus-content SHA-256: `ad7b01320496b67cecabd97aea949c7e0a198945eee45faad07d811e31b2e081`
- Artifact file SHA-256: `4552d1f26756edadfd7c6a4df8200edf78188bca54480e7262c613076a0e35ef`

## Completed assignments

All three completed assignments were replicate 0, consumed the exact 10,500-request verifier envelope, retained all 8,190 audit-origin Pareto artifacts, and passed the preregistered completed-recovery identity check.

| Treatment | Order | Node count | Depth | Encoded bytes | Evaluator operations | Wall ns | Process CPU ns | Peak resident bytes | Durable bytes | Result bundle SHA-256 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---|
| full | 0 | 74,707 | 50,527 | 421,203 | 74,707 | 18,451,213,769 | 18,436,853,878 | 65,529,574 | 8,617,377 | `1bb8600aec50075387fc6beb6eba4edba898c3ed5d3899fe42a111c1badbf938` |
| no-model | 1 | 75,368 | 51,188 | 427,152 | 75,368 | 16,225,649,923 | 16,209,062,364 | 65,490,256 | 7,614,793 | `d4121bc7a96b5b430d276667bae4002ee34cdb4b83656699b1c6e9e10f19fabd` |
| no-derived | 2 | 75,106 | 50,585 | 422,407 | 75,106 | 18,474,337,561 | 18,458,543,474 | 62,959,710 | 8,569,319 | `2bc580a36b5851bc8f6a2cb9be720ff80904b525e52453e77f1aed31b8e1549d` |

The bootstrap assignment was interrupted and has no retained result. The completed values above are disclosed for provenance only; they are not analyzed because the preregistered paired design is incomplete.

## Review consequence

This interruption exposed two harness defects before another confirmation attempt: corpora after replicate 0 had not yet been persisted, and an otherwise completed evaluation could be lost if its subsequent recovery check failed. The next harness revision persists every corpus before the first assignment and serializes evaluation results independently of recovery validity.
