# ADR 0002: Burn and Micro Model Tiers

## Status
Accepted

## Context
Small models (e.g. 2,607 parameter MLPs) are evaluated millions of times in search. Large tensor frameworks introduce tensor object allocation and dispatch overhead.

## Decision
Support two model tiers:
1. `reflex-ml-burn`: generic neural network architectures, autodiff, optimizers.
2. `reflex-ml-micro`: specialized zero-allocation linear and 2-layer MLPs for tiny rankers.

## Consequences
- Tiny models score with zero heap allocation in search.
- Burn handles general model authoring and larger architectures.
