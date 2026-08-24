# Scope learned state to a domain

Every Model Revision is scoped to the exact Domain Definition whose semantics and structure produced its training experience. Reflex shares its learning architecture and algorithms across domains, but reusing weights across domains requires an explicit compatibility or initialization path; weights never transfer merely because both domains implement the Structural Protocol.

## Considered Options

A universal cross-domain model could appear to maximize transfer, but structurally similar inputs can carry incompatible semantics and poison both reproducibility and learned judgment. Fully unrelated implementations would be safer but would discard the shared learning machinery that makes Reflex a library.
