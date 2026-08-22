# Keep Reflex source `unsafe`-free

All Reflex-owned Rust source remains under `unsafe_code = "forbid"`, including optimized search, learning, memory, and numerical kernels. Performance work uses data layout, fused loops, LLVM autovectorization, host-native code generation, LTO, PGO, and safe portable abstractions; handwritten intrinsics or an unsafe SIMD dependency require a future explicit reversal backed by end-to-end evidence rather than an implementation-local exception.

## Considered Options

Isolated `unsafe` could expose architecture intrinsics and specialized layouts, but the current workloads have not shown that this risk is necessary. Preserving one safe implementation and proving the remaining gap with benchmarks gives stronger portability, differential testing, and maintenance guarantees.
