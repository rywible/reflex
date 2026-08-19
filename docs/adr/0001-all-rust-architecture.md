# ADR 0001: All-Rust Architecture

## Status
Accepted

## Context
Reflex requires low-overhead search, model inference, verification, and persistence. Process boundaries between Rust and Python caused measurable overhead in earlier iterations.

## Decision
Implement Reflex entirely in Rust, eliminating mandatory Python runtimes and dependencies. Training and inference run via Burn and reflex-ml-micro.

## Consequences
- Single memory and thread accounting model.
- Zero-allocation hot-path candidate scoring.
- Streamlined deployment on Fly.io performance Machines.
