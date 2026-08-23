# Unary `u8` causal confirmation v6

Status: frozen before v6 audit exposure

Executable specification SHA-256: `a6afd9585d5c406488d29401373f25fdff979c2d36424d63f1dc3875e7fef166`

V6 is the unconsumed successor to the immutable v5 confirmation. It runs only
against Semantic Identity
`reflex-bitvec/u8/unary/full-ops/masked-shifts/select-nonzero/canonical-dag/v4`;
the harness fails closed before bundle construction, audit generation, or an
evaluation child if the installed identity differs.

The audit uses ten new fixed seed streams derived from
`SHA-256("reflex-u8-causal-confirmation-v6-seed\0" || decimal replicate index)`.
Rejection sampling excludes the development corpus and every semantic group in
the consumed v1 through v5 audits. V5 files, constants, and results remain
immutable Development Corpus and are not rewritten or reused as v6 evidence.

The four equal-envelope treatments are Full, no-Model, no-Derived, and a fresh
isolated Bootstrap Runtime. The no-Model treatment replaces only the Model
Ecology with the exact empty Bootstrap ecology. The no-Derived treatment removes
executable and promotable Derived Operator Knowledge, including compiler records
and pending verification, while preserving Model Ecology, causal Experience,
Runtime Policy, and the active Artifact scheduling index. Production-codec tests
check canonical component roots and byte identities for every component that an
ablation must preserve.

The fresh in-protocol Bootstrap arm is the authoritative v4 comparator. The
pinned `reflex-bootstrap-baseline-v7` report was produced under the earlier v3
Semantic Identity and is retained only as historical performance calibration;
the protocol does not claim that it is an exact current semantic comparator.

V6 has no standalone audit-materialization command. Its corpus is generated
only inside `causal-confirm`. Full first passes production validation; only then
are both one-factor ablation Bundles created and production-validated. Audit
generation requires the resulting in-process exposure authority; the complete corpus is persisted before
the first assignment and execution follows immediately. This prevents an
exposed corpus from later being represented as fresh confirmation evidence.
After the fixed work directory is created but before audit generation, the
report parent is resolved to its filesystem identity and rejected if that
identity is inside the work directory. The report itself is created without
replacement and synced before cleanup, so path aliases cannot erase or hide the
only report.

All remaining budgets, paired treatment order, protected outcomes, uncertainty
rules, multiplicity correction, recovery requirement, and no-exclusion rule are
the values frozen in the executable `ExperimentSpec`. The default report path is
`docs/experiments/u8-causal-confirmation-v6.json`; it must not overwrite v5.
