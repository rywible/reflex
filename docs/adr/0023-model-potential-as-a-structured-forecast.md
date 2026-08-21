# Model Potential as a structured forecast

Potential is a calibrated, uncertainty-bearing forecast over multiple horizons and outcomes, including immediate improvement, reusable descendants, cross-goal leverage, compression, novelty, expected verification cost, and dead-end risk. The Runtime Controller interprets these predictions in Campaign context rather than training or acting on a universal scalar target, allowing learned taste to emerge from observed reuse and generalization.

## Considered Options

A scalar Potential score would make ranking simple but hide incomparable kinds of value, collapse uncertainty, and bake one resource economy into the weights. Treating Potential as entirely domain-authored heuristics would avoid learning errors but prevent Reflex from acquiring better judgment through experience.
