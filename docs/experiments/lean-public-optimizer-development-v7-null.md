# Lean public-path optimizer Development v7 activation null

## Result

The first v7 activation run stopped before causal evaluation because training did not promote a Model Revision. This is the intended fail-closed behavior: Full, no-model, no-derived, and Bootstrap were not run when Full would not actually contain a learned model.

The run used git revision `8ab711cc3b5ecc36b5add85c7ad620b0352bc0e0`, 16 pre-2025 training opportunities, four held-out opportunities, a 1,024-request training envelope, and a planned 128-request evaluation envelope. It ran in release mode under the registered 40 GiB, seven-CPU supervisor with CPU 7 and 16 GiB reserved for the host. No 2026 data was accessed.

The retained training Domain Bundle has SHA-256 `49ed43f7bf805a449d2bd2f9a247ca25632d227f7f1fcba47bcb03fdcaf19332`. Its canonical summary contains 25 Artifact records and 1,008 Experience entries across only 13 correctness claims: nine Accepted entries across nine claims, 999 Refuted entries, and no Unknown entries. The other three training claims received no Candidate Verification before the envelope ended. Learning generation remained zero with no champion. The logical Experience segment is 144,385,298 bytes; the compressed Bundle is 5.4 MiB.

This null has two causes to separate before another training run. First, Candidate allocation did not protect exploration across Seed-relative correctness claims, so three of sixteen claims never entered either learning corpus. Second, the current 16-feature representation is lossy and internally redundant, as recorded in [the feature audit](./lean-candidate-feature-audit-v1.md). The next run must keep the same model capacity, protect claim coverage, and test the genuinely active linear model under the already declared constrained evaluation budget.

The command failed with: `Lean training produced no promoted Model Revision after 16 Artifacts and 1024 Verification requests; a Full versus no-model treatment would be a placebo`.
