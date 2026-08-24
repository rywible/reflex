# Rotate operational Selection Corpora

Operational Selection Corpora are replenished with untouched Seeds and have bounded reuse across promotion decisions. Retired cases move into the Replay Corpus and are replaced by fresh withheld cases, limiting adaptive overfitting while preserving the experience for later training; Scientific Corpora remain sealed outside this rotation.

## Considered Options

A permanent operational holdout would be simple and comparable over time, but repeated revision selection would gradually overfit it. Discarding cases after selection would avoid training leakage while wasting useful operational evidence.
