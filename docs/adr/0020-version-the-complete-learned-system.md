# Version the complete learned decision system

A Model Revision captures the complete domain-scoped learned decision system rather than a single neural-network parameter array. Predictor components, feature interpretation, calibration, and normalization evolve and reproduce together, allowing Reflex to use multiple small CPU-efficient decision components without weakening revision identity.

## Considered Options

Treating one weight tensor as the model would simplify checkpointing but leave behavior dependent on unversioned preprocessing and calibration. Fixing Model Revision to one network topology would also turn an early empirical choice into an architectural constraint.
