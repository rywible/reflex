# Unary `u8` causal confirmation v1 result

Result: **Null Result caused by a Protocol Deviation**

The preregistered run at revision `8ef030cafcd4b6be01bbc88c22aadc7ffd8e6b88` executed all 40 assigned processes and retained their raw outputs. Every evaluation consumed its declared one-worker, 10,500-verifier envelope, but every child exited during the independent recovery check. The v1 harness assumed that returning `Break` from the observer would stop a completed bundle immediately after import replay. A completed bundle whose persisted Pareto set is unchanged emits no initial observer delta, so the Runtime legitimately began another search epoch before the observer could stop it. That changed the Pareto result and triggered the preregistered recovery failure.

Consequently, v1 has no analyzable paired treatment results and makes no empirical performance claim. Its audit corpus is consumed and becomes Development Corpus; it will never be reused for confirmation.

- Experiment Specification SHA-256: `543bf37e7524f9a19ae805fb86530f2f1b0230a5a39e5e57a1c2f4fc7ba3a112`
- Audit corpus SHA-256: `7c8d87d90691502a55396e3cb70561bbd63cc7179d213879f93d6c5e9bb1a81c`
- Report content SHA-256: `11c23cd1695d15543cf92597505faff6da8c53222d8a451582fd08cc57674561`
- Report file SHA-256: `cd319141f985c982dd14281ce24e8aff3cd146c402abb51a289eb510cca3d208`
- Protocol Deviations: 40 recovery mismatches, followed by incomplete paired data for all three contrasts

The recovery protocol will be corrected and exercised only on Development Corpus before a v2 specification with new audit seeds is frozen.
