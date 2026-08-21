# Reflex owns autonomous search

Domain authors provide a typed, introspectable Domain Definition rather than implementing search strategies around opaque values and verifier callbacks. Reflex owns opportunity generation, generic search, Campaign construction, economic allocation, training, evaluation, and model promotion; requiring structural access is the price of producing strong autonomous behavior from Seeds, Verification, and Measurements alone.

## Considered Options

Opaque domain adapters would be easier to integrate but leave Reflex unable to construct meaningful mutations or reuse structure without domain-authored search logic. Requiring every domain to provide its own strategies would make Reflex an orchestration framework rather than the hands-on optimizer it is intended to be.

## Consequences

Domain integration must expose types, composition, and well-formed structure in a form generic Reflex components can inspect. Specialized domain extensions may exist as optional escape hatches, but the standard path must search, train, evaluate, and improve without them.
