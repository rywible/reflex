# Lean CPU model-size scaling Development v1

## Purpose

Measure whether richer Candidate structure or additional linear capacity improves verified discovery efficiency. This is a pre-2025 Development experiment, not Scientific Confirmation. It reuses one immutable Experience corpus and its claim-group assignments for every treatment; it does not generate extra labels per model and does not access the sealed 2026 corpus.

## Entry gate

The existing 16-feature linear model must first be genuinely promoted and causally evaluated against Bootstrap under the 128-request Lean envelope. A zero-generation or absent champion is a failed activation, not a model result. No larger model may run before this gate records verified discoveries, nodes removed, CPU time, and discovery order for Full and all counterfactuals.

## Fixed data and training

The initial sweep corpus is the retained v7 Experience bundle with SHA-256 `49ed43f7bf805a449d2bd2f9a247ca25632d227f7f1fcba47bcb03fdcaf19332`: 1,008 kernel-labeled attempts, nine Accepted and 999 Refuted. A successor corpus may add attempts needed to cover the three starved claims, but once chosen it is content-addressed and every model receives identical canonical Candidates, verdicts, consequences, claim-group Replay/Selection roles, example order, eight training epochs, FTRL hyperparameters, and calibration logic. Results from different corpus identities are never compared as a capacity sweep.

## Treatments

Run in this order:

1. `baseline-16`: the current representation and 112 effective coefficients.
2. `structural-16`: the audited same-capacity v2 representation.
3. `structural-32`: a nested extension adding an eight-bin signed constructor-delta sketch, four shape moments, and four explicit node-ratio × operator interactions; 224 coefficients.
4. `structural-64`: a nested extension separating parent and Candidate constructor sketches and adding bounded immediate and provenance summaries only if their extraction cost passed the CPU gate; 448 coefficients.

The 32- and 64-feature treatments are locked until `structural-16` beats `baseline-16` on the fixed Selection corpus and in the constrained public-path causal run. There is no neural hidden layer in this sweep: it measures representation and linear capacity before architecture complexity.

## Outcomes

Primary operational outcome: cumulative kernel-Accepted discoveries per process CPU-second at fixed Candidate budgets, macro-averaged by correctness claim. Report budgets 1, 4, 8, 16, 32, 64, and 128 rather than only the saturated endpoint.

Also report:

- strict Proof Collapses and verified proof nodes removed at each budget;
- seven-head Selection loss macro-averaged by claim, with no attempt-weighted shortcut;
- accepted precision and recall at each per-claim top-k;
- dead-end avoidance and calibration error by head;
- training CPU time, batch-forecast CPU time, encoded Model Revision bytes, and peak scratch bytes;
- Full, no-model, no-derived, and Bootstrap discovery order through the public observer;
- exact corpus, feature, target, runtime, model, and environment identities.

## Decision rule

Prefer the smallest treatment on the Pareto frontier of verified discoveries per CPU-second, Selection loss, and model bytes. A larger model advances only if its paired per-claim discovery efficiency exceeds the smaller nested treatment and it causes no protected-head regression under the existing promotion tolerances. Parameter count alone, training loss, and saturated 4/4 completion cannot justify advancement.
