# Lean temporal taste Development v3

Status: **successful Operational Promotion on pre-cutoff Development/Selection evidence; not Scientific Confirmation**. No declaration at or after the 2025-01-01 cutoff was accessed. The 2026 Temporal Audit Corpus remained sealed.

## Result

The conservative multi-head portfolio clears the pre-audit Pareto gate. Against the virtual best of every registered baseline at 256 selections per goal context, Full matches six protected heads and is strictly better on both compression and dead-end avoidance.

| Potential head | Full | Virtual best | Direction | Full strategy |
| --- | ---: | ---: | --- | --- |
| anticipation | 0.9102 | 0.9102 | higher | historical reuse |
| descendants | 0.8592 | 0.8592 | higher | historical reuse |
| reuse | 0.9542 | 0.9542 | higher | historical reuse |
| compression | 0.2916 | 0.2651 | higher | learned |
| migration survival | 0.9883 | 0.9883 | higher | historical reuse |
| verification cost | 0.2170 | 0.2170 | lower | dependency light |
| dead-end risk | 0.0039 | 0.0078 | lower | learned |

This is deliberately a portfolio, not a hidden scalar score. Each Potential head independently retains the strongest specialist supported by internal pre-cutoff selection. The learned ranker is promoted only where it adds held-out value; a structural specialist remains the verification-cost safety floor.

The ablations remain causal and directional. Removing Knowledge Consolidation cuts compression precision from 0.2916 to 0.0291. Immediate-only training cuts descendants from 0.8592 to 0.0239 and reuse from 0.9542 to 0.0210. The kernel sample certifies 43 of 48 attempted relationships, spanning exact, definitional, specialization, derivation, family-collapse, and corpus-compression cases, and retains all five rejections. Semantic Migration replays 17 of 32 June proof bodies in September without repair and retains all 15 failures.

## Economics and provenance

The clean `a1c400aa511baad5a069f59efcfdc2a26dfdb108` revision completed in 117.46 seconds wall and 36.98 controller CPU-seconds. Full training used 3.630 controller CPU-seconds; ranking seven goal contexts over 168,104 held-out declarations used 184 milliseconds wall. Individual baselines ranked in 4.8–8.9 milliseconds, so this result establishes quality gating, not the preregistered 10× time-to-matched-utility claim.

The controller high-water mark was 8.78 GB and the conservative combined controller-plus-worker resident upper bound was 43.14 GB, below the planned 48 GiB envelope. Catalogs occupy 192,026,067 durable bytes, encoded models 142,284 bytes, and the complete report 9,416 bytes. Forty-nine kernel calls used 12.49 seconds wall/CPU upper bound; worker-lifetime CPU is separately bounded at 161.77 seconds.

The protocol SHA-256 is `ca2c2b6da2155fb434c7b59105554a4b9c63e7c2a4972f2340569066537acd80`; the complete result SHA-256 is `c6a6cdf6d74fa3bd0327336d8da6bd96fb6fa31e2b84dc9d50976f5790ed8cf0`. Exact inputs, hashes, selected strategies, timings, failures, and environment are retained in [`lean-taste-development-v3.json`](./lean-taste-development-v3.json). The preceding corrected-label regression remains retained as v2.

## Decision

Operationally promote this portfolio. Before unsealing 2026, freeze a separate immutable audit implementation and analysis specification that adds equal-CPU anytime checkpoints, semantic-family/module statistical units, paired partitions, 99% intervals, recovery/no-regression gates, and blinded mathematical critique. The current short-run harness must not be relabeled as confirmation.
