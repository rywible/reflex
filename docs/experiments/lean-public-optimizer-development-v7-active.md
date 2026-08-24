# Lean public-path optimizer Development v7 active-model result

## Result

The first genuinely active v7 causal run rejects the current 16-feature linear model as an improvement over Bootstrap under the constrained 128-request evaluation envelope. Bootstrap found three kernel-verified strict Proof Collapses and removed 9,164 proof nodes in 43.114 seconds of process CPU. Full found two collapses and removed 998 nodes in 49.261 CPU seconds. Thus Bootstrap produced 0.0696 strict discoveries per CPU-second versus Full's 0.0406, and 212.55 removed proof nodes per CPU-second versus Full's 20.26.

Full and no-model found the same two strict improvements in the same observer order. Bootstrap additionally found the valuable `hfdifferential_apply` collapse, replacing an 8,167-node proof with a one-node pre-2025 library proof. No model-driven discovery benefit is observable at this budget. Full also used 2.5% more CPU than no-model, although this single sequential Development run is not a performance replicate.

The complete machine-readable report is [lean-public-optimizer-development-v7-active.json](./lean-public-optimizer-development-v7-active.json). Its content hash is `4435807b5bc3eab155514378ce7d7d5399d9f37af0d20e822c686457770c09fd`; the repository file SHA-256 is `47c8c5e3be8fd47408796e33c7466e9a8d0b2db176e32f13201c2ffcb61d18c7` (the repository copy adds a terminal newline to the harness output).

## Activation evidence

The run used git revision `722f93e969c3bfc295dbc75b3a1ce3684dc42f02`, 16 pre-2025 training opportunities, four disjoint held-out opportunities, a 1,024-request training envelope, and equal 128-request treatment envelopes. It ran in the release profile with `opt-level=3`, fat LTO, one codegen unit, and AArch64 NEON available. The hard supervisor allowed CPUs 0–6 and 40 GiB while reserving CPU 7 and 16 GiB for the host. No 2026 data was accessed.

The retained training Domain Bundle has SHA-256 `7ba3e9a199a261b770d686060dc5a48d77fb1fad745885afe1991c15aa6797d3`. It contains 1,008 kernel-labeled Experience entries across all 16 correctness claims: 13 Accepted entries across 13 claims, 995 Refuted, and no Unknown. Learning promoted generation one with Model Revision `4392351e2c789edca5caef6f0cc988774f36a8f1fdf812fb873b8214e9799d90`. This proves that the prior v7 null was repaired without increasing feature or model capacity.

## Decision

Do not increase parameter count. The audited representation saturates large proofs, duplicates features, learns epoch order, and omits structural shape. The next treatment is the same 16-dimensional structural representation trained from this exact retained Experience and fixed claim roles. The 32- and 64-feature treatments remain locked until structural-16 beats baseline-16 on paired offline Selection outcomes and in the constrained public-path causal run.
