# Keep the production implementation entirely Rust

All Reflex-owned production code, including optimized search, Verification, and learned-model kernels, is implemented in stable Rust. Reflex does not link project-owned C/C++, assembly, foreign-language runtimes, or native ML libraries; it accepts the current gap in explicit stable-Rust SVE2 access to preserve one toolchain, safety model, build graph, and debugging surface.

## Considered Options

A narrow C ABI could expose mature SVE2, BF16, I8MM, and ML kernels while keeping most of Reflex in Rust, but it would make performance depend on a second language and toolchain. A whole-system C++ implementation would improve direct access to Arm ACLE while enlarging the memory-safety and concurrency audit surface.
