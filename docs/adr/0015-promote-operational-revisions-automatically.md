# Promote operational revisions automatically

When a challenger Knowledge Revision or Model Revision satisfies the configured selection guardrails, the Runtime promotes it atomically without per-revision human approval. Promotion retains the previous compatible revision and enough state to roll back. Operational Promotion governs the live improvement loop; Scientific Confirmation remains a separate internal process for establishing empirical claims about Reflex itself.

## Considered Options

Manual approval for every revision would interrupt the turn-it-on autonomy Reflex promises. Unchecked replacement would preserve autonomy but allow regressions to poison subsequent Campaigns, so automatic promotion requires explicit comparison, protected constraints, atomic adoption, and rollback.
