# Promote Models between Verification Cohorts

An Improvement Session may train and Operationally Promote a Model Revision after a completed Verification Cohort. The promoted revision becomes the active allocator only after the same atomic cohort checkpoint has durably recorded its Experience, Admission consequences, corpus roles, and complete Model Revision. A recovered Session therefore makes the same next-cohort allocation decision as an uninterrupted Session.

This supersedes the Model half of the Architecture's former Campaign-wide revision pin: a Model Revision is now pinned for one complete Verification Cohort and may change only at its durable boundary. The Knowledge Revision remains pinned Campaign-wide. No in-flight Candidate generation, selection, or Verification observes a hot swap.

Online training is attempted when retained Candidate Experience first reaches 32 examples and once more after sixteenfold growth to 512. Final Session training is attempted only when Experience has advanced beyond the last durably recorded training watermark. These two online comparisons leave the third and final permitted Selection use available for the mature end-of-Session challenger. The watermark and completed-online-comparison count are restart-complete state. A failed optional checkpoint advances neither, so one delayed retry cannot silently consume two scheduled stages. The sparse deterministic schedule bounds repeated target derivation and training work while still closing the search-learning-search feedback loop early in a long Session. Operational Promotion continues to require disjoint Replay and Selection Corpora, observed policy-prefix improvement, protected-loss tolerances, and bounded Selection reuse. Verification remains the only source of correctness.

The active Knowledge Revision remains pinned for a Session because consolidation changes the available Operator algebra and recovery tail, while a Model Revision only reorders already generated Candidates. This distinction permits safe online ranking improvement without changing Candidate semantics or silently introducing new search operations.

## Trade-offs

Later Experience is gathered under an earlier promoted policy and is therefore operationally on-policy rather than an independent scientific estimate. Candidate Fates retain Bootstrap and learned counterfactual ranks, and Scientific Confirmation still requires a frozen external treatment. Training at every cohort would react sooner but repeatedly derive targets over a growing ledger; end-only training wastes the entire current Session. Logarithmic checkpoints bound that cost to a small number of attempts.

Runtime revision 20 persists the last training Experience watermark and completed-online-comparison count, validates both against the exact trained Experience prefix, and rejects older interrupted recovery state. Lean Development schema v27 identifies the corrected allocation protocol. The public Improvement Session interface is unchanged.

## Development evidence

The [v26 32×8 run](../experiments/lean-public-optimizer-development-v26.md) confirms that the promoted Model Revision serves later cohorts: Learned produced 27 Accepted Candidates in 464 checks while the cooperative Bootstrap queue produced seven in 496. It also falsifies the original power-of-two cadence. Three early comparisons exhausted the three-use Selection Corpus, leaving zero Selection claims and preventing the complete-Experience challenger from replacing generation one. Runtime revision 20 spreads the unchanged bounded comparison budget across Experience growth; it does not relax disjointness or silently reuse retired Selection cases.
