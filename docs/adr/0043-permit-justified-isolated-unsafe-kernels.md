# Permit justified, isolated `unsafe` kernels

Reflex exposes a safe Rust API while permitting narrowly isolated internal Rust `unsafe` kernels when benchmarks demonstrate material performance value. Each such kernel requires documented invariants, a safe Rust reference implementation, differential and fuzz testing, and Miri-compatible coverage where applicable.

## Considered Options

Banning `unsafe` would simplify the safety story but could prevent necessary SIMD or memory-layout optimizations. Allowing it broadly would trade away auditability and confidence without proving that the risk buys meaningful performance.
