# Keep learned components advisory

Model Revisions produce typed predictions and proposals, while a deterministic Runtime Controller owns Verification routing, Admission rules, Operational Promotion guardrails, and Resource Envelope enforcement. Learned components guide decisions but never become the authority for correctness or operational safety.

## Considered Options

Allowing an end-to-end learned policy to control the entire loop could simplify orchestration, but it would make hard invariants probabilistic and entangle learned behavior with Reflex's trust boundary. Removing learned allocation entirely would preserve control while discarding the judgment Reflex is intended to acquire.
