# Lean temporal taste Development v1

Status: **Development evidence; not Scientific Confirmation**. No declaration at or after the 2025-01-01 cutoff was accessed. The 2026 Temporal Audit Corpus remains sealed.

## Result

Reflex learned seven separate Potential heads from the June→September 2024 Temporal Snapshot Pair and ranked semantic groups absent from that training set against September→December consequences. The full CPU model trained over 254,225 declarations for eight epochs in 1.907 seconds of process CPU, occupied 35,124 encoded bytes, and ranked all seven goal contexts over 168,104 held-out declarations in 152 milliseconds.

At 256 declarations per goal context, Full achieved:

| Potential head | Full | Virtual-best baseline | Direction |
| --- | ---: | ---: | --- |
| anticipation | 0.9609 | 0.9883 | higher |
| descendants | 0.9475 | 0.9542 | higher |
| reuse | 0.9475 | 0.9542 | higher |
| compression | 0.2916 | 0.2651 | higher |
| migration survival | 0.9961 | 0.9883 | higher |
| verification cost | 0.1988 | 0.1967 | lower |
| dead-end risk | 0.0039 | 0.0078 | lower |

This is a strong but deliberately negative gate result: Full greatly beat uniform allocation and exposed substantial long-horizon effects, but it did **not** Pareto-dominate the virtual-best baseline portfolio. The audit must remain sealed. The next model revision needs to close the small anticipation/reuse/cost gaps without surrendering its compression, migration, and dead-end advantages.

The ablations behaved directionally:

- removing consolidation reduced compression precision from 0.2916 to 0.0291;
- immediate-only training reduced descendant and reuse precision from 0.9475 to 0.0210;
- Full anticipation was 15.4× uniform (0.9609 versus 0.0625);
- Full dead-end risk was one sixth of uniform (0.0039 versus 0.0234).

The bounded kernel sample certified 43 of 48 relationships: one exact, 26 definitional, eight specialization, eight derivation, five family-collapse, and two corpus-compression certificates. Certified proof collapses removed 1,448 proof nodes. Explicit June→September Semantic Migration replayed 17 of 32 sampled theorem bodies without repair and retained all 15 failures with diagnostics; a migration failure is rejection, never silent compatibility.

## Frozen inputs

| Snapshot | mathlib commit | Lean toolchain | Catalog declarations | Eligible | Catalog SHA-256 |
| --- | --- | --- | ---: | ---: | --- |
| 2024-06-30 | `454c40501feacb5aef56e707d6b348fc68897dce` | `v4.9.0-rc3` | 928,494 | 392,081 | `9deacea7faeb8edcd400dd61348ec08a76fcbbd1aa85e08f349a3dfb63df4f10` |
| 2024-09-30 | `37814caf0b1b93a00743c1dd7af97ceb6b092b40` | `v4.12.0-rc1` | 957,816 | 424,936 | `9c027cede0fb0cb75a33a2afeabca1c9726df60c92c40f65fff00ee1bceb18ac` |
| 2024-12-31 | `7178aee7a431bb7527da15c3507836d8dfefcda4` | `v4.15.0-rc1` | 1,007,619 | 458,619 | `021e90ae7d642aa87f65108d5d8d5984a77a11b35ed9636b7ed6c48be3069090` |

The protocol SHA-256 is `2afbeadf0624bfc421f722afbee0e27148ed5f1c032efb5674c6ed3446f470f3`; the complete result SHA-256 is `7b255c00df890f39e63e7592c16cb94f1b64731f0060de9f05133ce5fedfb877`. Raw treatment results, failures, environment, resource upper bounds, and protocol fields are retained in [`lean-taste-development-v1.json`](./lean-taste-development-v1.json).

## Reproduction

Build each catalog with `cargo run -p xtask --release -- lean-catalog`, the matching `--snapshot` (`2024-06-30`, `2024-09-30`, or `2024-12-31`), and its exact local mathlib checkout. Then run:

```text
cargo run -p xtask --release -- lean-taste-development \
  --lake /home/ryanwible/.elan/bin/lake \
  --june-root /home/ryanwible/.cache/reflex/mathlib4-454c40501feacb5aef56e707d6b348fc68897dce \
  --september-root /home/ryanwible/.cache/reflex/mathlib4-37814caf0b1b93a00743c1dd7af97ceb6b092b40 \
  --december-root /home/ryanwible/.cache/reflex/mathlib4-7178aee7a431bb7527da15c3507836d8dfefcda4 \
  --june-catalog /tmp/reflex-lean-catalog-june-v1.bin \
  --september-catalog /tmp/reflex-lean-catalog-september-v1.bin \
  --december-catalog /tmp/reflex-lean-catalog-december-v1.bin \
  --certificate-limit 48 --migration-limit 32 \
  --output /tmp/reflex-lean-taste-development-v1.json
```
