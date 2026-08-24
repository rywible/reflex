# Keep Domain Bundles data-only

A Domain Bundle contains restart-complete data but no executable domain code. Import requires an installed Domain Definition whose Semantic Identity matches the bundle, preserving portability across compatible local Runtimes without turning bundles into platform-specific executable packages.

## Considered Options

Bundling Rust domain code could make one file operationally self-contained, but it would introduce platform coupling, code-loading and trust concerns, and a second distribution mechanism for domain implementations.
