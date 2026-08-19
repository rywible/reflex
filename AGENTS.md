# AGENTS.md — Agent Engineering Guidelines

## Invariants
All work in this repository must strictly adhere to the constitutional invariants defined in `docs/invariants.md` and `docs/reflex-framework-master-plan-rust.md`.

## Workflow
1. Read the task card and inspect dependencies.
2. Implement only owned crates and modules unless explicitly cross-cutting.
3. Preserve all deterministic verification and evidence requirements.
4. Run task verification through `cargo xtask task verify <ID>`.
5. Capture evidence under `evidence/tasks/<ID>/`.
