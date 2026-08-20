# Local experiment configuration v1

`reflex experiment plan --config <path>` accepts strict TOML with three
tables: `experiment`, `search`, and `training`. Unknown fields are errors; no
environment or implicit database value may silently change manifest identity.

The planner resolves the user-facing values into
`reflex.experiment.v1`, including pinned code, toolchain, domain, corpus,
split, schemas, policy, verifier, evaluator, resource, and analysis-plan
digests. Human labels do not participate in identity. The resulting manifest
is validated before its content digest is printed.

```toml
[experiment]
name = "bitvec-tutorial"
domain = "bitvec-v1"
generations = 2
seeds = [42]

[search]
algorithm = "best-first-and-or-v1"
action_budget = 100
node_budget = 1000
cpu_seconds = 30.0
exploration_uniform = 0.1

[training]
model_type = "micro-mlp"
input_dim = 24
hidden_dim = 16
epochs = 30
learning_rate = 0.05
weight_decay = 0.0001
```

`experiment plan` never mutates durable state. `experiment run --dry-run`
uses the same resolver and likewise writes no metadata, CAS object, ledger, or
report. A non-dry run is admitted only through the durable generation
coordinator.
