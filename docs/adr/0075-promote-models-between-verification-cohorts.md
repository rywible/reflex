# Promote Models between Verification Cohorts

An Improvement Session may train and Operationally Promote a Model Revision after a completed Verification Cohort. The promoted revision becomes the active allocator only after the same atomic cohort checkpoint has durably recorded its Experience, Admission consequences, corpus roles, and complete Model Revision. A recovered Session therefore makes the same next-cohort allocation decision as an uninterrupted Session.

Online training is attempted when retained Candidate Experience first reaches 32 examples and whenever its size crosses another power-of-two boundary. This deterministic logarithmic schedule bounds repeated target derivation and training work while still closing the search-learning-search feedback loop early in a long Session. A Resource Envelope that cannot accommodate the challenger and training scratch skips that online attempt; final Session training remains available. Operational Promotion continues to require disjoint Replay and Selection Corpora, observed policy-prefix improvement, protected-loss tolerances, and bounded Selection reuse. Verification remains the only source of correctness.

The active Knowledge Revision remains pinned for a Session because consolidation changes the available Operator algebra and recovery tail, while a Model Revision only reorders already generated Candidates. This distinction permits safe online ranking improvement without changing Candidate semantics or silently introducing new search operations.

## Trade-offs

Later Experience is gathered under an earlier promoted policy and is therefore operationally on-policy rather than an independent scientific estimate. Candidate Fates retain Bootstrap and learned counterfactual ranks, and Scientific Confirmation still requires a frozen external treatment. Training at every cohort would react sooner but repeatedly derive targets over a growing ledger; end-only training wastes the entire current Session. Logarithmic checkpoints bound that cost to a small number of attempts.

Runtime revision 19 rejects older interrupted recovery state because model activation can now change between Verification Cohorts. Lean Development schema v26 identifies the changed allocation protocol. The public Improvement Session interface is unchanged.
