# Reflex Audit Report — `impl` Branch vs `docs/reflex-framework-master-plan-rust.md`

**Date:** 2026-08-18
**Repo:** `/Users/ryanwible/projects/reflex`
**Branch:** `impl` (HEAD `9e02f26`, only 2 commits)
**Audited against:** `docs/reflex-framework-master-plan-rust.md` (9,329 lines; sections 0–29, task cards P0.1–P16.8)
**Method:** Deep file-by-file audit of all 28 crates, 4 bins, 3 domains, xtask, tests, benches, fuzz, deploy, config, sql, evidence, docs. `cargo check --workspace --locked` and `cargo test --workspace` both pass; the failures below are semantic, not syntactic.

---

## 1. Executive summary

The repository contains a coherent, compiling, single-file-per-crate skeleton of the Reflex framework. The search kernel, canonical encoding, local CAS, SQLite single-writer actor, fencing primitives, and typed-ID layer are genuine and well-formed. **However, the repository as committed does not meet the acceptance criteria of a single one of its 120 task cards**, and several constitutional invariants are violated or unenforceable. The dominant problem is not missing code but **fabricated evidence**: every one of the 120 `evidence/tasks/<ID>/` packages is stamped "passed" by a generic `cargo test --workspace` run in `xtask`, with no task-specific acceptance criteria, no performance gates, and fake host calibration.

Three plan pillars are effectively absent in substance:
1. **Verification/evidence integrity** (P0.2/P0.5, §22, INV-11) — the xtask "verify" pipeline is a placeholder that stamps pass.
2. **Analytical stack** (P4.5/P4.6/P4.7, §9) — no Parquet, no DataFusion, no mmap RFXBATCH; a hand-rolled string-matching SQL interpreter stands in for DataFusion.
3. **Real domain verification** (P10/P12/P13) — the bit-vector, Wrela, and Lean domain "verifiers" all fabricate acceptance (`kernel_certified: true`, `is_sound: true` hardcoded).

The fast lane (`cargo xtask check`) **fails today** on a clean checkout (`cargo fmt --check` has diffs; `clippy -D warnings` fails in 4 crates). The dependency baseline deviates from §25 without the required ADRs (Burn `0.22.0-pre.2` vs `0.21.x`, Arrow/Parquet `59.2` vs `58.3.x`, DataFusion `55.0` vs `54.1.x`). The P0–P16 release gates (A–G) cannot be claimed.

---

## 2. What is genuinely solid

| Area | Evidence |
|---|---|
| Toolchain/edition | `rust-toolchain.toml` 1.97.1, edition 2024, resolver 3, rust-version 1.97.1, `MIT OR Apache-2.0` — all match §4.1 |
| Build/test | `cargo check --workspace --locked` and `cargo test --workspace` green; 187 unit + 24 integration tests |
| Search kernel | `reflex-search`: indexed-arena `SearchNode`/`AndGroup`, deterministic `FrontierKey` + `OrderedScore` (rejects NaN, normalizes −0.0), `BinaryHeap`, AND-group propagation, censored `BudgetExhausted` (never negative labels), `ProofDag::mark_verified_route` never marks untouched candidates dead (§11, §28.5) |
| Local CAS | `reflex-cas` `FsArtifactStore`: temp-file → hash → `sync_all` → atomic rename → parent fsync, digest-verified collision handling, two-phase GC skeleton + `RetentionClass` (§7.3) |
| SQLite writer | `reflex-meta-sqlite`: dedicated `reflex-sqlite-writer` thread, bounded mpsc, `configure()` pragmas exactly per §9.2/28.2, fenced claim/finalize with artifact `EXISTS` check |
| Fencing core | Postgres claim SQL (`FOR UPDATE SKIP LOCKED` CTE + fencing-token increment) and fenced finalize SQL semantically match §9.3/§28.3 |
| Canonical identity | `reflex-types` tagged `Digest` (BLAKE3/SHA-256), typed-ID newtypes, `content_id` envelope, `reflex-canonical` golden tests |
| Thread budget | `reflex-runtime` `ThreadBudget` (semaphore permits + `PoolRegistry`) matches §3.5; permit-leak detection at shutdown |
| Micro MLP | `reflex-ml-micro` `MicroMlp::score_rows` (mul_add + ReLU, reference §13.2) + AdamW micro trainer with real m/v state |
| Governance artifacts | 5 ADRs (0001–0005), `docs/invariants.md` with INV-RFX-1..24, task manifest YAML with 120 IDs matching the plan |
| Safety | Zero `unsafe` in the entire workspace; no search/runtime/scheduler/domain crate imports Burn (§4.2 isolation holds except `reflex-training`, see F-23) |

---

## 3. CRITICAL findings (cross-cutting)

### C-1. All 120 evidence packages are boilerplate fakes — the verification system is a lie
- `xtask/src/main.rs:49–121` `verify_task` and `:161–227` `verify-all` run only `cargo test --workspace`, write the **byte-identical** `tests.txt` (md5 `76d121e0f04aa5be642dc9ce8c709d39`) into every task dir, and stamp `result.json` with `"Full AC for <ID>" → "passed"`, `benchmarks.json` with `{<ID>_conformance → passed}`, `commit: 9e02f26`, empty `dependencies`/`deliverables`.
- `evidence-check` (`:123–130`) only asserts the four files exist; it never validates content, AC coverage, or freshness.
- `benchmarks.json` host calibration is fabricated by `HostCalibration::calibrate_current_host()` (`reflex-bench/src/lib.rs:25–53`), which hardcodes `memory_mb: 8192`, `memory_bandwidth_gbps: 25.0`, and derives `host_class` from CPU count (a 10-core Mac is labeled `performance-4x`).
- Evidence is not reconstructible from the repository (§22.1, INV-11). Example: `evidence/tasks/P3.1/tests.txt` cites `test_spawn_ledger_writer_async`, which does not exist; P4.5/P4.7 are "passed" with no Parquet/DataFusion code anywhere.

### C-2. `cargo xtask check` (P0 fast lane) FAILS on a clean checkout
- `cargo fmt --check` diffs: `bins/reflex/src/main.rs:293` and `:571`.
- `cargo clippy --workspace -- -D warnings` fails in 4 crates: `reflex-cas:515` (collapsible if), `reflex-meta:403` (iterating map values), `reflex-ledger:486` (`new_without_default` on `EventEncoder`), `reflex-ml-burn` (useless `ModuleOptimizer` conversion).
- P0 exit gate and P0.2 AC ("A formatting error, clippy warning … each fail the fast lane") are therefore broken even as the check is defined.

### C-3. `check-deep` is identical to `check`
- `xtask/src/main.rs:144–151` runs the same four commands as `check`. No Loom, no Turmoil, no fuzz smoke, no corruption-recovery / distributed integration runs (§20, P0.2). The deep lane is absent.

### C-4. No task-manifest / DAG validation exists
- `xtask` never parses `docs/implementation/reflex-framework-task-manifest.yaml` (no YAML dependency). No cycle/missing-dependency/empty-AC/scope-expansion validation; no ready/blocked/completed views (P0.5). The YAML itself has no `acceptance`/`performance_acceptance` fields required by P0.5.

### C-5. Domain verifiers fabricate acceptance
- **Lean** `domains/reflex-domain-lean/src/lib.rs:350` hardcodes `kernel_certified: true`; proof acceptance is a `"valid tactic"` substring check (P13.1/INV-1).
- **Wrela** `domains/reflex-domain-wrela/src/lib.rs:431–433` hardcodes `is_sound: true` and `cycles_measured: 45_000`; no kernel package, no adapter, no conformance (P12.1–12.8/INV-1).
- **Bitvec** `domains/reflex-domain-bitvec` verifier checks only 256 input combos over 4 env values instead of exhaustive u8 truth tables; `BvExpr` lacks Shift and Select, no bounded depth (P10.1/P10.2, §10.2 completeness).
- A search "success" backed by these receipts cannot be reported solved under INV-1 ("A successful search without an accepted receipt is not a solved task").

### C-6. The evidence ledger serializes events as JSON, not compact binary
- `reflex-ledger/src/lib.rs:517` `serde_json::to_vec(&events)` per block; `:500` clones every event per append.
- This defeats the entire rationale of §8.1/P3.1/P3.3 (no NDJSON, <32 bytes/candidate, 500k events/s/core, zero alloc per event). The plan's performance gates are physically unreachable on this design. It also caps dataset-compaction throughput (`reflex-dataset` decodes the same JSON).

### C-7. The analytics stack is a fake (DataFusion/Arrow/Parquet absent)
- `reflex-analytics/src/lib.rs:43–139` is a substring-based SQL interpreter (`sql_lower.contains("from cells")`) returning `HashMap<String,String>` rows; `memory_limit_bytes`/`spill_dir` are stored but never enforced.
- No member crate depends on `datafusion`/`arrow`/`parquet` (workspace deps at `Cargo.toml:92–95` are unused). No Parquet publication (P4.5), no RFXBATCH mmap/prefetch/status-mask/source-ID sections (P4.6, `memmap2` declared but unused), no DataFusion session/catalog/views/query CLI (P4.7, §19.2).

### C-8. External protocol has no generated code, no transport, no server
- `proto/reflex/domain/v1/domain.proto` matches §28.1 text-for-text but is referenced by nothing: no `build.rs`, no `prost-build`; runtime messages are hand-written serde types.
- `reflex-protocol` has a length-delimited codec and a `DomainClient` only — no UDS, no stdio, no supervisor, no server; `deadline_mono_ns` is carried but never enforced; no cancellation; late replies dropped silently (P3.6, §28.1).
- `reflex-domain-host` is a stub: `handshake()` fabricates a `HandshakeResponse` locally; `enumerate_candidates` returns a hash-derived dummy; `apply_candidates` always returns `Closed`. It cannot attach any real Lean/Wrela process (P5.4).

### C-9. Repository is effectively unversioned and the P0.1 deliverables are unmet
- Only 3 files are tracked (`.gitignore`, `LICENSE`, plan doc); `Cargo.lock` and all source are untracked. No Linux-generated lockfile exists.
- `bins/reflex/src/main.rs:28` version string is hardcoded `"(commit: 9e02f26, profile: release, target: universal)"` — commit won't advance, profile is wrong in debug builds, "universal" is not a target triple, no schema compatibility range. `reflexd` and `reflex-worker` have no version output. No `build.rs` anywhere.

### C-10. Search kernel bug: a Contradiction kills the whole OR node
- `reflex-search/src/lib.rs:670–675`: on `TransitionOutcome::Contradiction` the kernel sets the **entire OR node** `NodeStatus::Failed` and calls `propagate_failed`. Under §11.1 an OR node succeeds when *any* alternative closes; a certified-dead candidate is one dead alternative, not a dead state. Remaining viable candidates are never tried, and parents fail. The bitvec domain never emits `Contradiction`, so tests miss it.

---

## 4. Findings by phase

### P0 — Repository and engineering authority
| # | Sev | Finding | Location |
|---|---|---|---|
| F-1 | CRIT | Evidence pipeline fabricates pass (see C-1) | `xtask/src/main.rs` |
| F-2 | CRIT | Fast lane fails fmt + clippy (see C-2) | `bins/reflex/src/main.rs`, 4 crates |
| F-3 | CRIT | `check-deep` == `check` (see C-3) | `xtask/src/main.rs:144–151` |
| F-4 | CRIT | No DAG/manifest validation; YAML schema incomplete (see C-4) | `xtask`, `docs/implementation/reflex-framework-task-manifest.yaml` |
| F-5 | CRIT | Repo unversioned; version output hardcoded (see C-9) | `bins/reflex/src/main.rs:28` |
| F-6 | MAJ | Dependency drift without ADR: Burn `0.22.0-pre.2` (plan 0.21.x), tokio `1.53` (1.52.x), rusqlite `0.40.2` (0.40.1), arrow/parquet `59.2` (58.3.x), datafusion `55.0` (54.1.x); pprof + OpenTelemetry absent | `Cargo.toml:55–95` |
| F-7 | MAJ | No CI workflows at all; `evidence/checks/` absent (P0.2) | `.github/workflows` missing |
| F-8 | MAJ | No cargo-audit, no SBOM, no exception registry, cargo-deny not wired into xtask; deny.toml has no `[advisories]` / multi-version deny (P0.3) | `deny.toml` |
| F-9 | MAJ | `docs/invariants.md` not trustworthy: no performance-measurement column; some "Enforcing API" names don't exist (`FrontierKey::order`, `ProposalGate::evaluate`, `CellContext::pin_model`); test names mismatched (P0.4) | `docs/invariants.md` |
| F-10 | MAJ | No ADR template/index; empty `docs/architecture`, `docs/protocols`, `docs/storage`, `docs/performance`, `docs/tutorials` (P0.4) | `docs/` |
| F-11 | MAJ | No `#![forbid(unsafe_code)]` and no mechanism enforcing "deny unsafe by default" (P0.1) | all crates |
| F-12 | MIN | xtask at repo root, not `crates/xtask` (§6 layout) | repo root |
| F-13 | MIN | SQLite/Postgres migration drift: `artifacts.created_at` missing in `sql/sqlite/0001_init.sql`; embedded PG schema (`attempt_no/fencing_token BIGINT`) vs `sql/postgres/0001_init.sql` (`INTEGER`) already out of sync | `sql/`, `reflex-meta-postgres` |

### P1 — Performance constitution and measurement substrate
| # | Sev | Finding | Location |
|---|---|---|---|
| F-14 | MAJ | `HostCalibration` hardcodes memory/bandwidth/host-class; no calibration profile schema, no JSON exporter, no baseline-comparison with noise bands (P1.1) | `reflex-bench/src/lib.rs:25–53` |
| F-15 | MAJ | `ProcessTreeAccountant::sample` returns hardcoded `rss_bytes = 64 MiB`; no procfs, no PID-reuse/start-time tracking, no rusage reconcile (P1.3) | `reflex-runtime/src/lib.rs:172` |
| F-16 | MAJ | No counting allocator / allocation-budget assertions (P1.4); `ThreadBudget`/`PoolRegistry` exist (solid) but nothing enforces zero-alloc claims | workspace |
| F-17 | MAJ | `task benchmark <ID>` only prints the fabricated calibration, never runs Criterion benches; regression gate not wired to CI or promotion; no waiver-ADR schema (P1.6) | `xtask/src/main.rs:230–237` |
| F-18 | MAJ | §3.3 performance gates have no benchmarks anywhere; `benches/search_bench.rs` imports nonexistent `BudgetSet`/`UniformPolicy` (stale, won't compile as a bench target) | `benches/` |
| F-19 | MIN | `HostCalibration` performance-4x label from CPU count alone | `reflex-bench` |

### P2 — Canonical identity and content-addressed storage
| # | Sev | Finding | Location |
|---|---|---|---|
| F-20 | MAJ | `ObjectStoreArtifactStore` is a thin wrapper over `MemoryArtifactStore`; ignores endpoint/bucket/region; no object_store dependency, no multipart/resumable upload, no retry/abort, no MinIO/Tigris conformance (P2.4, §18.5) | `reflex-cas/src/lib.rs:645–702` |
| F-21 | MAJ | No integrity scanner; no chunked put/get (only a `ChunkManifest` type); GC lacks mark-in-metadata grace period and `repair` (P2.3/P2.5/P2.6) | `reflex-cas` |
| F-22 | MAJ | `ArtifactStore::put_stream` adds a `retention` param not in §7.3; `delete_ephemeral` ignores the GC token; `head` falls back to `Active` on missing meta; `get_range` with verification reads the whole file; `Digest::as_path` ignores algorithm tag (collision-prone) | `reflex-cas/src/lib.rs:110–124` |
| F-23 | MAJ | `content_id` takes `&[u8]` not `&'static [u8]`; envelope lacks compatibility-range field; no `HashMap` canonical impl; pending rewrite `PLAN-reflex-canonical-rewrite.md` unfinished (P2.2, §7.2) | `reflex-canonical/src/lib.rs:258` |
| F-24 | MIN | Empty dead files `reflex-cas/src/store/{mod,fs,memory,object}.rs` not declared in lib.rs | `reflex-cas/src/store/` |

### P3 — Evidence ledger and external protocol
| # | Sev | Finding | Location |
|---|---|---|---|
| F-25 | CRIT | Ledger payloads are JSON; per-event clone (see C-6) | `reflex-ledger/src/lib.rs:500,517` |
| F-26 | CRIT | Proto dead code; no generated Rust; transport/server/cancellation absent (see C-8) | `proto/`, `reflex-protocol` |
| F-27 | MAJ | Writer silently drops blocks on I/O error (`let _ = writer.write_block(...)`) — blocks lost between a failed write and the next barrier; scientific events can vanish (P3.3 "never dropped") | `reflex-ledger/src/lib.rs:647` |
| F-28 | MAJ | Recovery never truncates torn tails and allocates a fresh `Vec` per block; 1 GiB/s gate unmeasured (P3.4, §28.4) | `reflex-ledger/src/lib.rs:818–824` |
| F-29 | MAJ | No segment footer/index is ever written; `finish()` computes a digest but writes nothing; no sidecar index, no stream merger, no corruption quarantine (P3.2/P3.4) | `reflex-ledger/src/lib.rs:602–623,764–774` |
| F-30 | MAJ | Missing event families: `GenerationLifecycle`, `AttemptLifecycle`, `TaskEnd`, cancellation; no ordering/gap validation at write time (P3.1, §8.4) | `reflex-ledger/src/lib.rs:370–390` |

### P4 — Metadata and analytical storage
| # | Sev | Finding | Location |
|---|---|---|---|
| F-31 | CRIT | Analytics/Parquet/DataFusion/RFXBATCH all absent (see C-7) | `reflex-analytics`, `reflex-dataset` |
| F-32 | MAJ | Postgres `publish_attempt_artifacts` has **no fence check** — a stale worker can insert artifact rows (INV-10, §9.3). SQLite backend does check | `reflex-meta-postgres/src/lib.rs:247–269` |
| F-33 | MAJ | Postgres claim SQL/schema omit `available_at` and `image_digest` from §9.3; no lease-expiry reclamation path (abandoned `running` cells deadlock) | `reflex-meta-postgres/src/lib.rs:54–74` |
| F-34 | MAJ | Postgres schema missing `attempts`, `worker_sessions`, `leases`, `reports`, `knowledge_editions` tables — retry evidence and sessions cannot be recorded (P4.3/P4.4) | `reflex-meta-postgres`, `sql/postgres/0001_init.sql` |
| F-35 | MAJ | Heartbeat omits `attempt_no`, hardcodes 60s, opens a fresh connection per call — no batching, no pool (P4.4 perf: <5 q/s/worker) | `reflex-meta-postgres/src/lib.rs:225–245` |
| F-36 | MAJ | `MetaStore` trait lacks worker-session / knowledge-edition / lineage methods; **no `DatasetStore` trait exists** (P4.1) | `reflex-meta/src/lib.rs:147–172` |
| F-37 | MAJ | SQLite writer actor does not batch (no 256-`try_recv` loop) and never checkpoints WAL on exit (§28.2); no runtime WAL-reset-fix assertion (P4.2) | `reflex-meta-sqlite/src/lib.rs:403–449,414–416` |
| F-38 | MAJ | `LeaseStatus::Expired` is never produced by any backend — the expiry→reclaim path is dead (P4.4) | all meta backends |
| F-39 | MAJ | Promotion receipts are constant digests (`hash_blake3(b"sqlite-promotion-receipt")`, `b"postgres-promotion-receipt"`, `b"promotion-receipt"`) — not derived from promotion lineage (P9.4) | `reflex-meta-sqlite:383`, `reflex-meta-postgres:378`, `reflex-meta:337` |
| F-40 | MAJ | `MemoryMetaStore` conformance fixture hardcodes `lease_expires_at = 9999999999` and finalizes without verifying artifact publication — the fixture itself violates P4.1 fencing semantics | `reflex-meta/src/lib.rs:246,289–321` |
| F-41 | MAJ | No connection pooling/prepared-statement pinning; `enqueue_cells` is row-by-row (P4.3 step 4, P4.1 bulk) | `reflex-meta-postgres` |
| F-42 | MIN | Finalize conflates conflict causes into one `StaleFence` (§28.3 wants exact conflict) | `reflex-meta-postgres/src/lib.rs:323–329` |
| F-43 | MIN | `QueryResult` records query digest but no DataFusion/Arrow versions; only one report SQL file (out of 17 canonical tables) | `reflex-analytics`, `sql/reports/` |

### P5 — Domain SDK and local runtime
| # | Sev | Finding | Location |
|---|---|---|---|
| F-44 | MAJ | `ErasedDomain` exposes only 4 of ~10 semantic methods (no `reconstruct_artifact`/`verify`/`evaluate_utility`); `EpisodeArena` has no generation counter, so stale-handle detection (P5.2 AC) is absent | `reflex-domain/src/lib.rs:376–403,66–108` |
| F-45 | MAJ | `reflex-domain-host` is a stub (see C-8) | `reflex-domain-host/src/lib.rs` |
| F-46 | MAJ | `CellContext` pins only IDs, not `Arc`-pinned objects — INV-3/INV-4 (one checkpoint/knowledge per cell) unenforceable by design; cancellation is a single `AtomicBool`, no cancellation tree; no bounded pools (P5.3) | `reflex-runtime/src/lib.rs:185–221` |
| F-47 | MAJ | `TransitionOutcome` duplicated and divergent: `reflex-domain/src/lib.rs:220–227` (unit `Closed`, no witness, `Obligations{Vec<StateHandle>}`) vs `reflex-types/src/lib.rs:301–308` (`Obligations{group_id:u64}`); codes are bare `u32` not `InvalidCandidateCode`/`UnresolvedCode` (§10.3) | two crates |
| F-48 | MAJ | `VerificationReceipt.status` is a free `String`, not a `VerificationStatus` enum (§10.5) | `reflex-domain/src/lib.rs:277` |
| F-49 | MIN | Domain conformance kit / template / tutorial skeleton (P5.6) absent | workspace |

### P6 — Deterministic AND-OR search kernel
| # | Sev | Finding | Location |
|---|---|---|---|
| F-50 | CRIT | Contradiction kills the whole OR node (see C-10) | `reflex-search/src/lib.rs:670–675` |
| F-51 | MAJ | No `AlignedVec`/aligned allocator; batches use `Vec` (§12.1, P6.2) | `reflex-types`, `reflex-domain` |
| F-52 | MAJ | No transposition/dominance table or reflex/closure cache — only `state_to_node: HashMap` (P6.4) | `reflex-search/src/lib.rs:98–203` |
| F-53 | MAJ | No `BudgetSet`; budget is 3 scalars (`action/node/cpu_seconds`); no verifier-call/wall/RSS/artifact-byte counters, no "which budget fired first", no sub-2ns logical checks (P6.5) | `reflex-search/src/lib.rs:287–302` |
| F-54 | MAJ | No replay engine or first-divergence report; CLI `cell replay` re-runs a fresh search and prints "0 divergences" without comparison (P6.8) | `reflex-search`, `bins/reflex/src/main.rs:289–302` |
| F-55 | MAJ | `UniformRanker` emits constant `1.0`; `rand`/`rand_chacha` declared but unused; no epsilon-uniform/mixture/portfolio; `exploration_uniform` config field never read (P6.6) | `reflex-search/src/lib.rs:334–360`, `bins/reflex/src/main.rs:460` |
| F-56 | MAJ | Search-side proof-DAG `mark_viable` stores empty `receipts` and `mark_verified_route` takes no `Digest` — viable edges are never linked to receipts (P6.7 AC) | `reflex-search/src/lib.rs:423–483` |
| F-57 | MIN | Two incompatible `Ranker` traits (`reflex-search` vs `reflex-ml-core`) with duplicated `InferenceTelemetry`; `ordered_score` name/error-type deviates; no golden frontier fixtures | `reflex-search/src/lib.rs:322–330`, `reflex-ml-core/src/lib.rs:226–234` |

### P7 — All-Rust model runtime
| # | Sev | Finding | Location |
|---|---|---|---|
| F-58 | MAJ | `MicroMlp::score_batch` allocates `vec![0.0; rows*hidden]` per call — violates P7.3 zero-allocation AC; weights are `Vec<f32>` not `Box<[f32]>` (§13.2) | `reflex-ml-micro/src/lib.rs:116–118,6–15` |
| F-59 | MAJ | Two divergent `ModelSpec`s: `reflex-ml-core/src/lib.rs:191–199` (used by checkpoints) drops `dtype` and typed `BackendClass`, uses `String` backend — checkpoint/identity omits dtype (P7.1) | `reflex-ml-core`, `reflex-types/src/lib.rs:412–421` |
| F-60 | MAJ | Checkpoint manifest missing normalization digest, dataset/sampler digests, build identity, backend/device, RNG states, dev/calibration metrics, parent; `metrics` is a `String` (§13.4) | `reflex-ml-core/src/lib.rs:252–261` |
| F-61 | MAJ | Burn pinned at `0.22.0-pre.2` vs plan `0.21.x` with **no ADR**; §4.1 mandates ADR + parity/performance gates for any Burn change | `Cargo.toml:90` |
| F-62 | MAJ | `reflex-training` imports `burn::*` directly — violates §4.2 "Burn only behind reflex-ml-burn" | `reflex-training/src/lib.rs:1–4` |
| F-63 | MIN | `TasteEstimate` missing cross-domain/compression/discovery-cost heads + calibration/unit metadata (P7.6); `ProposalBatch` has no validation hook or quota (P7.7); `BurnCustom` parameter count hardcoded to 2607 | `reflex-ml-core` |

### P8 — Dataset compilation and training
| # | Sev | Finding | Location |
|---|---|---|---|
| F-64 | MAJ | Only pairwise loss; no masked listwise/multi-positive objective (P8.3); **training builds pairs `(Viable, Unknown)`, giving unknown candidates negative gradient — directly violating §14.1** | `reflex-training/src/lib.rs:151–157,24` |
| F-65 | MAJ | No `BatchPlan`, no shuffle state, no prefetch pipeline (P8.5) | `reflex-training` |
| F-66 | MAJ | Burn checkpoint/optimizer state not persisted — `ModelLoader` saves weights only; resume cannot reproduce metrics (P8.6, §13.4) | `reflex-ml-burn/src/lib.rs:74–104,144–210` |
| F-67 | MAJ | No `SweepSpec`; exact parameter budgets (519/1026/2607/9614/29538/99902) not expressible; `ml_bench` mislabels a 2113-param MLP as `micro_mlp_2607_batch64` (P8.9) | `benches/ml_bench.rs:5` |
| F-68 | MAJ | Evaluation metrics missing NDCG, family/stratum breakdowns, calibration; `mrr_cheapest_route` is a rank-1 heuristic, not MRR (P8.8) | `reflex-eval/src/lib.rs:9–18` |
| F-69 | MAJ | No micro/Burn parity harness (P8.7) | `reflex-ml-micro` |
| F-70 | MAJ | `DatasetCompiler` uses placeholder `feature_ref` = hash of state_id and a `b"default-receipt"` fallback — labels not traceable to evidence (P8.1/P8.2) | `reflex-dataset/src/lib.rs:128,183` |

### P9 — Autonomous loop
| # | Sev | Finding | Location |
|---|---|---|---|
| F-71 | MAJ | Worker never verifies manifests: ignores `model_checkpoint`, hardcodes `UniformRanker`, hardcodes a `BitvecTask` ignoring `task_id`; no digest/compatibility/resource-class check (P9.2 AC) | `bins/reflex-worker/src/main.rs:131–147` |
| F-72 | MAJ | Promotion receipts are constant digests (see F-39); `model promote` CLI digests a candidate string, not CAS-loaded checkpoints | `bins/reflex/src/main.rs` |
| F-73 | MIN | `GenerationCoordinator`/`StopPolicy` exist but claim-loop transition validation lives in the worker and never checks manifest authority (P9.1) | `reflex-scheduler`, `bins/reflex-worker` |

### P10 — Bit-vector slice
| # | Sev | Finding | Location |
|---|---|---|---|
| F-74 | CRIT | Verifier not exhaustive; AST missing Shift/Select; no bounded depth (see C-5) | `domains/reflex-domain-bitvec/src/lib.rs:15–23` |
| F-75 | MAJ | Features are 8-dim stubs: no operator histograms, subtree sizes, candidate edit class, local cost delta (P10.3) | same crate |

### P11 — Fly.io distributed execution
| # | Sev | Finding | Location |
|---|---|---|---|
| F-76 | MAJ | Resource classes are string literals only (`performance_4x_8gb`); `safe_rss_mb`/`max_search_threads`/`max_verifier_workers` appear nowhere; no resource-class registry (§18.2) | `reflex-meta:449,459`, `reflexd:164`, `config/` |
| F-77 | MAJ | Postgres connection hygiene: fresh `tokio_postgres::connect` + `NoTls` per operation (P11.5) | `reflex-meta-postgres/src/lib.rs:118–130` |
| F-78 | MAJ | `FlyApiClient` + `FleetController` exist but no on-wire verification; no Tigris config; `deploy/fly` has `fly.toml`/`Dockerfile` only, no Postgres/Tigris provisioning (P11.1/P11.4/P11.5) | `reflex-fly`, `deploy/fly` |
| F-79 | MIN | `fly inventory` CLI prints `"0 leaked volumes"` hardcoded; worker has no health endpoint (P11.3) | `bins/reflex`, `bins/reflex-worker` |

### P12/P13 — Wrela and Lean
| # | Sev | Finding | Location |
|---|---|---|---|
| F-80 | CRIT | Wrela verification is fictional (see C-5); no kernel package schema, no export/check/cost commands, no catalog, no campaigns | `domains/reflex-domain-wrela` |
| F-81 | CRIT | Lean verification is fictional (see C-5); no protocol bridge, no M1.5/M2A reconstruction (hardcoded `reconstruct_m2a_result`) | `domains/reflex-domain-lean` |

### P14 — Knowledge economy
| # | Sev | Finding | Location |
|---|---|---|---|
| F-82 | MAJ | `RationalOrFloat` does not exist — `UtilityObservation.value` is `f64`; no unit registry, no derived-metric expressions, no marginal-utility API; INV-RFX-7 (raw utility immutable) unenforceable as specified | `reflex-economics/src/lib.rs:33`, `reflex-domain/src/lib.rs:298` |
| F-83 | MIN | Knowledge classes/use-levels/overlays exist but no retrieval experiment harness, no leave-one-out runner (P14.4/P14.7) | `reflex-knowledge` |

### P15 — Research graph / taste / proposal
| # | Sev | Finding | Location |
|---|---|---|---|
| F-84 | MIN | `ResearchNode` has only `direct_utility` + `propagated_credit`; plan requires separate direct/propagated/causal-confidence/counterfactual fields; 8 edge types exist (P15.1/P15.2) | `reflex-research-graph/src/lib.rs:27–33` |

### P16 — Operator experience and release
| # | Sev | Finding | Location |
|---|---|---|---|
| F-85 | MAJ | `reflexd` has no auth middleware, no SSE stream, no ETag/pagination; OpenAPI is a hand-written static blob (P16.2) | `bins/reflexd/src/main.rs` |
| F-86 | MAJ | Fuzz is a stub: 7 plain functions in a lib crate, no `fuzz_targets/`, no `#![no_main]`; 10 mandatory targets missing (P16.5, §20.2) | `fuzz/` |
| F-87 | MAJ | Benchmarks assert no performance gates (p95 ≤100µs @64-cand, ≤5µs @1-cand, 5M metadata/s, 10M no-op/s, 10GiB packing/s, 500k events/s) (§3.3) | `benches/` |
| F-88 | MAJ | 24 invariant tests exist by name but most are shallow value-holds; only inv_1/10/15 substantive (P16.5) | `tests/invariants_test.rs` |
| F-89 | MIN | CLI `--dry-run` declared but unused; `evaluate` ignores `--checkpoint`; `experiment status/stop/resume`, `report`, `fly inventory` print hardcoded output | `bins/reflex/src/main.rs:36` |

---

## 5. Constitutional invariant status

| Invariant | Status |
|---|---|
| INV-RFX-1 Verifier authority | **VIOLATED** — Lean/Wrela verifiers hardcode acceptance; a fake receipt becomes "solved" |
| INV-RFX-2 Immutable inputs | Partial — manifests pin IDs, but worker ignores them (F-71) |
| INV-RFX-3/4 No mutable model/knowledge in cell | Unenforceable by design — `CellContext` pins IDs only (F-46) |
| INV-RFX-5 Censored means unknown | OK in search (`BudgetExhausted` censored) |
| INV-RFX-6 Multiple valid routes survive | OK in `CandidateKnowledge`/`ProofDag`; **undermined** by training `(Viable, Unknown)` pairs (F-64) and by the Contradiction bug (C-10) |
| INV-RFX-7 Raw utility immutable | Not enforceable — no `RationalOrFloat`/unit registry (F-82) |
| INV-RFX-8 Search cost includes ML | Partial — `ml_overhead_ns` tracked |
| INV-RFX-9 No partial artifact publication | OK in CAS/metadata backends |
| INV-RFX-10 One accepted attempt | Partial — Postgres artifact publication is **unfenced** (F-32) |
| INV-RFX-11 Replayable claims | **VIOLATED** — evidence pipeline fabricates pass; replay engine absent (C-1, F-54) |
| INV-RFX-12 No shared SQLite | OK — local-only actor |
| INV-RFX-13 Bulk data out of metadata DBs | OK by design |
| INV-RFX-14 No per-candidate process boundary | OK — batched |
| INV-RFX-15 Bounded memory/queues | Partial — bounded channels exist |
| INV-RFX-16 One CPU budget | OK — `ThreadBudget` exists |
| INV-RFX-17 Deterministic registered mode | Partial — frontier deterministic; uniform policy is constant, no seed-derived priorities (F-55) |
| INV-RFX-18 Exploratory mode labeled | Not implemented |
| INV-RFX-19 Knowledge explicit | Partial |
| INV-RFX-20 No learned proposal before taste gate | OK by absence |
| INV-RFX-21 No silent fallback | **VIOLATED** — domain-host fabricates handshake/enumerate (C-8) |
| INV-RFX-22 Performance is correctness | **VIOLATED** — no gates enforced (F-87) |
| INV-RFX-23 Cleanup is part of completion | Partial — inventory audit exists, output hardcoded (F-79) |
| INV-RFX-24 Historical science remains historical | N/A — no historical artifacts touched |

---

## 6. Priority remediation order

1. **Stop stamping evidence.** Replace `xtask task verify` with a real per-task verifier that runs each task card's named commands and ACs; make `evidence-check` validate content against the manifest (P0.5, P0.2). This is the precondition for every "passed" claim.
2. **Make the fast lane green** (fmt + clippy) and give `check-deep` real content (Loom/Turmoil/fuzz smoke).
3. **Replace fake verifiers** in bitvec (exhaustive u8 tables, Shift/Select AST, bounded depth), Wrela, and Lean with real authority per INV-1; a "solved" claim must trace to an accepted receipt.
4. **Fix the Contradiction OR-node bug** (`reflex-search:670`) — dead alternative ≠ dead state.
5. **Fix training supervision** — remove `(Viable, Unknown)` pairs (§14.1); add the masked listwise objective; persist optimizer state in checkpoints; add micro/Burn parity.
6. **Reconcile dependencies with §25 or write the ADRs** (Burn 0.21.x or ADR for 0.22-pre; Arrow/Parquet/DataFusion lines; add pprof + OTel).
7. **Fence Postgres artifact publication; add lease-expiry reclamation; add missing coordination tables** (P4.4).
8. **Implement Parquet + DataFusion analytics and mmap RFXBATCH, or explicitly defer P4.5–P4.7 via ADR** rather than leaving a string-matching fake.
9. **Implement the external protocol transport + generated code, or the domain host** so Wrela/Lean can attach for real.
10. **Commit the repository** (Cargo.lock, sources, evidence) and add CI + cargo-deny/audit/SBOM.