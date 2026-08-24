# Lean historical pin for the pre-2025 experiment

Date: 2026-08-22

## Exact pin

Interpret “the final mathlib4 commit strictly before 2025-01-01 UTC” as the last commit on mathlib4's default-branch history by **committer timestamp**, with the cutoff `2025-01-01T00:00:00Z`.

| Item | Exact value |
|---|---|
| mathlib4 commit | `7178aee7a431bb7527da15c3507836d8dfefcda4` |
| Author and committer timestamp | `2024-12-31T21:01:59Z` |
| Commit subject | `chore(RingTheory/Ideal/Colon): generalize to semiring (#18595)` |
| `lean-toolchain` bytes | `leanprover/lean4:v4.15.0-rc1\n` |
| Lean 4 commit named by that tag | `ffac974dba799956a97d63ffcb13a774f700149c` |

The mathlib commit, both timestamps, tree, and parent are recorded by the [official repository commit response](https://api.github.com/repos/leanprover-community/mathlib4/commits/7178aee7a431bb7527da15c3507836d8dfefcda4). Its one-line [`lean-toolchain` file](https://github.com/leanprover-community/mathlib4/blob/7178aee7a431bb7527da15c3507836d8dfefcda4/lean-toolchain#L1) pins `leanprover/lean4:v4.15.0-rc1`; the [official Lean tag ref](https://api.github.com/repos/leanprover/lean4/git/ref/tags/v4.15.0-rc1) resolves that tag directly to Lean commit `ffac974dba799956a97d63ffcb13a774f700149c`.

### Why this is the last eligible commit

1. GitHub's official “list repository commits” endpoint defaults to the repository's default branch when `sha` is omitted, and its `until` parameter filters at an ISO-8601 timestamp ([API parameter contract](https://docs.github.com/en/rest/commits/commits?apiVersion=2022-11-28#list-commits)). Querying it through the last whole Git second of 2024 returns `7178aee…` first: [exact cutoff query, one result](https://api.github.com/repos/leanprover-community/mathlib4/commits?until=2024-12-31T23%3A59%3A59Z&per_page=1).
2. The earliest default-branch commit on 2025-01-01 is `bd681bc33a22201938feb3796dfad236496726dd` at `2025-01-01T11:13:53Z`, and its sole parent is exactly `7178aee…` ([official commit response](https://api.github.com/repos/leanprover-community/mathlib4/commits/bd681bc33a22201938feb3796dfad236496726dd)). Thus there is no intervening default-branch commit at the boundary.

Git commit timestamps have whole-second precision, so `until=2024-12-31T23:59:59Z` implements the strict midnight cutoff without admitting an exactly-midnight commit.

## Reproducible local installation and cache facts

The smallest reproducible setup is:

```sh
git clone https://github.com/leanprover-community/mathlib4.git
cd mathlib4
git checkout --detach 7178aee7a431bb7527da15c3507836d8dfefcda4
elan toolchain install leanprover/lean4:v4.15.0-rc1
lean --githash
lake exe cache get
```

The expected `lean --githash` output is `ffac974dba799956a97d63ffcb13a774f700149c`. Elan's official documentation says its `lean` and `lake` proxies automatically select—and, when necessary, download—the toolchain named by a project's `lean-toolchain` file ([Elan README](https://github.com/leanprover/elan#elan-lean-version-manager)). Explicit installation is still useful for fail-fast provisioning and an auditable log.

At the pinned commit, mathlib's own instructions say to run `lake exe cache get` for precompiled `.olean` files before `lake build`, because skipping the cache makes the build very slow ([historical README](https://github.com/leanprover-community/mathlib4/blob/7178aee7a431bb7527da15c3507836d8dfefcda4/README.md#L62-L65)). The historical cache CLI defines `get` as downloading missing linked files and decompressing them; path arguments restrict the download to those files and their dependencies ([cache command source](https://github.com/leanprover-community/mathlib4/blob/7178aee7a431bb7527da15c3507836d8dfefcda4/Cache/Main.lean#L9-L47)). It installs build products under `.lake/build`, while compressed downloads live under `$XDG_CACHE_HOME/mathlib` or, by default, `$HOME/.cache/mathlib` ([cache I/O source](https://github.com/leanprover-community/mathlib4/blob/7178aee7a431bb7527da15c3507836d8dfefcda4/Cache/IO.lean#L37-L57)).

For a worker file, `lake lean WorkerInput.lean -- <lean-args>` is preferable to manually reconstructing search paths: this command builds imports and invokes `lean` in the workspace environment, including `LEAN_PATH` and the toolchain paths ([Lake 4.15 CLI help source](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/lake/Lake/CLI/Help.lean#L326-L375)).

## Official-kernel checking facts for a minimal batch worker

Lean's environment documentation states that the kernel type-checks declarations, refuses ill-typed declarations and declarations with metavariables/free variables, and protects the environment constructor from bypassing the kernel ([`Environment` documentation](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/Lean/Environment.lean#L109-L140)). The checked insertion primitive is `Environment.addDeclCore`; Lean separately exposes `addDeclWithoutChecking` with an explicit warning that it compromises soundness and lets buggy tactic output escape kernel checking ([checked and unchecked APIs](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/Lean/Environment.lean#L245-L261)). A Reflex worker must never use the unchecked path.

For a process-backed first implementation, use the pinned binary and a fixed, worker-generated module:

```sh
lake lean WorkerInput.lean -- \
  --trust=0 \
  --threads=1 \
  -DwarningAsError=true \
  --json
```

These flags have first-party semantics:

- `--trust=0` tells Lean not to trust macros and to type-check all imported modules. The default is maximum trust, so omitting this flag is materially weaker ([Lean 4.15 command help source](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/util/shell.cpp#L193-L225)).
- `--threads=1` gives the external process one Lean worker thread; the same CLI also exposes memory and allocation-count limits. Its “timeout” is a maximum allocation count per task, **not** a wall-clock deadline, so Reflex must still enforce elapsed-time/CPU/RSS limits and terminate an over-budget child externally ([same CLI contract](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/util/shell.cpp#L210-L221)).
- `-DwarningAsError=true` converts warnings to errors ([Lean logging option](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/Lean/Log.lean#L50-L64)). Lean logs a warning whenever an inserted declaration contains `sorry` ([declaration insertion path](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/Lean/AddDecl.lean#L29-L36)), so this makes an explicit or synthetic `sorry` fail the process instead of merely warning.
- `--json` emits machine-readable diagnostics. The command driver returns success only when the frontend reports no errors ([frontend result](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/Lean/Elab/Frontend.lean#L136-L172), [process exit path](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/util/shell.cpp#L706-L755)).

Kernel success is necessary but not by itself Reflex's complete definition of “verified.” `sorry` expands to the axiom `sorryAx`, which can prove anything and is accepted by the kernel as an axiom ([Lean prelude](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/Init/Prelude.lean#L647-L664)). Likewise, a candidate could declare a fresh axiom whose *type* is valid. Therefore the worker should accept a structured proof term rendered inside a fixed declaration template—not arbitrary candidate-controlled Lean commands—and enforce an explicit allowed-axiom policy.

Lean provides the first-party mechanism for that policy: `collectAxioms` recursively gathers the axioms on which a declaration depends ([implementation](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/Lean/Util/CollectAxioms.lean#L15-L43)), and `#print axioms theoremName` exposes it as a command ([command implementation](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/Lean/Elab/Print.lean#L155-L165)). For machine checking, compare `collectAxioms` output to a pinned domain allowlist rather than parsing pretty-printed text; at minimum reject `sorryAx` and every candidate-introduced axiom.

### Minimal worker contract implied by the sources

1. Pin and report both the mathlib SHA and Lean `--githash`; refuse mismatches.
2. Import the pinned corpus once per batch or long-lived worker to amortize `.olean` loading and trust-level-zero rechecking.
3. Render each candidate only into a fixed theorem body with a fresh deterministic name.
4. Insert through the checked declaration path, reject every error/warning, and never call `addDeclWithoutChecking`.
5. Collect transitive axioms and require the pinned allowlist; record the resulting theorem type, axiom set, toolchain identities, diagnostics, and resource usage as verification evidence.
6. Keep candidate verdicts independent by starting each check from the same immutable base environment. Lean documents environments as never destructively updated, which makes this isolation model natural ([environment semantics](https://github.com/leanprover/lean4/blob/ffac974dba799956a97d63ffcb13a774f700149c/src/Lean/Environment.lean#L109-L125)).

The six points above are design inferences from the cited first-party behavior; they are not claims that Lean supplies this Reflex-specific worker protocol out of the box.
