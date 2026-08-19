# Junior Coding Agent Execution Protocol

This protocol defines the step-by-step procedure for implementing any task card from `reflex-framework-task-manifest.yaml`.

## Step 1: Read and Inspect
1. Read the exact task card and all acceptance criteria.
2. Read `docs/invariants.md` and ensure no constitutional invariant is weakened.
3. Check dependencies in the task manifest.

## Step 2: Implement
1. Modify only the listed owner crates and files.
2. Follow zero-allocation hot-path rules.
3. Wrap third-party APIs behind internal abstractions.

## Step 3: Verify and Capture Evidence
1. Run `cargo test -p <owned_crate>`
2. Run `cargo xtask task verify <TASK_ID>`
3. Run `cargo xtask task benchmark <TASK_ID>`
4. Run `cargo xtask task evidence-check <TASK_ID>`
5. Confirm the 4 required evidence files exist under `evidence/tasks/<TASK_ID>/`:
   - `result.json`
   - `commands.txt`
   - `tests.txt`
   - `benchmarks.json`

## Step 4: Stop on Failure
If any acceptance criterion or performance threshold fails, stop and investigate. Do not mark partial work complete.
