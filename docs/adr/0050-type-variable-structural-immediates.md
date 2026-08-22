# Type variable-length Structural Protocol immediates

A Structural Protocol constructor declares its immediate arity as either `Exact(n)` or `Variable`; it must not encode variable-length payloads behind a sentinel fixed count. Exact arity remains the default constructor-descriptor path.

## Considered Options

Forcing Lean names, universe data, and literals into fixed-width immediates would either truncate semantic data or require a universal boxed representation that contradicts the specialized-representation decision in ADR 0006. Modeling every UTF-8 byte as a structural child would make the generic tree describe serialization mechanics instead of meaningful proof structure. A typed variable arity preserves canonical round trips while keeping the shared schema honest and domain structure meaningful.
