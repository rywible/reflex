# Lean temporal taste Development v2

Status: **Development evidence; not Scientific Confirmation**. No declaration at or after the 2025-01-01 cutoff was accessed. This run supersedes v1's collapsed descendant/reuse labels: reuse now counts new theorem dependencies, descendants count all growth in direct inbound declaration dependencies, and verification cost comes from the later snapshot.

## Result

The corrected labels exposed one remaining regression. Full matched the virtual-best baseline on anticipation, descendants, reuse, migration survival, and dead-end risk, and improved compression from 0.2651 to 0.2916. It did not Pareto-dominate because its learned future verification-cost result was 0.2352 versus the dependency-light baseline's 0.2170, where lower is better. This is a negative Operational Promotion gate, so the 2026 Temporal Audit Corpus remained sealed.

| Potential head | Full | Virtual best | Direction |
| --- | ---: | ---: | --- |
| anticipation | 0.9102 | 0.9102 | higher |
| descendants | 0.8592 | 0.8592 | higher |
| reuse | 0.9542 | 0.9542 | higher |
| compression | 0.2916 | 0.2651 | higher |
| migration survival | 0.9883 | 0.9883 | higher |
| verification cost | 0.2352 | 0.2170 | lower |
| dead-end risk | 0.0039 | 0.0078 | lower |

The causal signals remained directional: removing consolidation reduced compression precision to 0.0291, while immediate-only training reduced descendant and reuse precision to 0.0239 and 0.0210. The bounded kernel sample again certified 43 of 48 relationships and retained every rejection. Explicit June→September Semantic Migration replayed 17 of 32 exact proof bodies and retained all 15 failures without repair.

## Economics and provenance

The clean `15d7b397b32b20148e95ca4bb9e75b130791ba3e` revision completed analysis in 116.93 seconds wall and 36.53 controller CPU-seconds. Full training consumed 3.612 controller CPU-seconds and seven-head ranking took 155 milliseconds wall. The measured controller high-water mark was 8.78 GB; adding both hard worker limits gives a conservative combined resident upper bound of 43.14 GB. The three catalogs occupy 192,026,067 durable bytes, encoded treatment models 142,275 bytes, and this report 8,354 bytes. Forty-nine kernel calls consumed 12.41 seconds wall/CPU upper bound. Worker-lifetime CPU is separately bounded at 161.62 seconds.

The protocol SHA-256 is `ca2c2b6da2155fb434c7b59105554a4b9c63e7c2a4972f2340569066537acd80`; the complete result SHA-256 is `e7857243aae5a3e05745f39c2640a63bbcb6c9da2b5cd6867ba1002dcf8cccc1`. Exact inputs, catalog hashes, treatment outputs, failure diagnostics, environment, and accounting are retained in [`lean-taste-development-v2.json`](./lean-taste-development-v2.json).

## Decision

Retain the result as negative evidence. Promote a conservative multi-head portfolio in which the dependency-light structural specialist remains the verification-cost safety floor; learned ranking may displace specialists only on heads where pre-cutoff selection supports it. Re-evaluate that implementation on the still pre-cutoff September→December Selection Corpus before any audit freeze.
