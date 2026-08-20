# Wrela Kernel Package v1

Content-addressed semantic package for pure bounded Wrela kernels.

## Schema

- `version`: package format version (currently `1`)
- `kernel_id`: stable kernel identifier
- `wrela_commit`: pinned Wrela compiler commit
- `source_digest`: digest of sealed source
- `semantic_ir`: bounded AST nodes (const dyadic, var, add/sub/mul, let, if, bounded loop, tuple, bounded array)
- `input_ranges`: declared input domains with precision
- `overflow`: observable overflow/error/Result behavior
- `target_cost_table`: baseline and strategy cost targets
- `fixture_refs` / `trace_refs`: CAS references for large artifacts

## Validation

Packages validate independently of a live Wrela checkout. Unsupported constructs
(allocation, actors, concurrency, pointers, unbounded loops) fail with source-mapped
diagnostics.

## Identity

Any semantic, range, cost, or verifier change changes the package digest via
`content_id(b"wrela.package.v1", package)`.

## Commands

| Command | Purpose |
|---------|---------|
| `reflex-export` | Export sealed package JSON |
| `reflex-check-candidate` | Verify candidate certificate soundness |
| `reflex-cost-candidate` | Measured cycles with overhead breakdown |
| `reflex-conformance` | Scalar/packet differential fixtures |

Commands do not mutate source or catalog by default.
