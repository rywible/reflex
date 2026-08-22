# Rank protected Candidates counterfactually

Before partitioning a selection transaction into protected and unprotected work, the Runtime computes complete Bootstrap and, when active, learned rankings over every novel Candidate. A protected Candidate retains those counterfactual ranks even though protection, rather than either ranker, determines its executed policy rank and Allocation Queue. Removing protected Candidates from the operational queues continues to preserve their relative ordering, so this observation change does not change which Candidates are selected.

Protected exploration recovered every held-out Proof Collapse in the Lean v11 Development run, but the corresponding Candidate Fates had absent Bootstrap and learned ranks. The trace could establish that protection caused selection while being unable to answer where either comparator would have placed the same discoveries. This made the protected lane scientifically necessary and causally opaque at the same time.

The counterfactual ranks are deterministic observations of the exact considered Candidate set, Model Revision, Bootstrap Revision, Goals, and active Preference. They do not claim that an unexecuted policy would have produced the same future Search Frontier, and therefore support only within-transaction attribution and diagnostics. Online equal-envelope treatments remain necessary for causal policy claims.

Candidate Fate meaning is restart-complete Experience semantics. Runtime revision 8 rejects older bundles rather than mixing absent-by-policy ranks with ranks that are absent because no learned model existed. Internal Lean Development schemas advance with the changed trace meaning. The public Improvement Session and Domain Definition interfaces remain unchanged.
