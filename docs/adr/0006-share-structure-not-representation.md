# Share structure, not representation

Every Domain Definition satisfies one Structural Protocol describing types, composition, binding, and canonical structure, while retaining a domain-specialized physical representation compiled into the Reflex runtime. This gives generic search and learning structural access without forcing Lean terms, expressions, and control-flow graphs into one boxed universal object model or hiding them behind opaque native values.

## Considered Options

A universal graph representation would simplify generic traversal but sacrifice semantic precision and compact domain-specific layout. Opaque Rust types exposed only through callbacks would preserve native performance but make autonomous structural mutation, mining, and learned representation substantially weaker.

## Consequences

Generic Reflex modules operate through the Structural Protocol, while domain implementations may use monomorphized Rust types, packed arenas, and integer identifiers. Domain integration is normally compiled into the runtime; dynamic plugin loading and a stable Rust ABI are not architectural requirements.
