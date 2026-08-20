# Reflex Framework Master Plan — Rust-Native, Performance-First Edition

## A platform for verified self-improving search, learning, and discovery

**Status:** Greenfield implementation authority  
**Evidence cutoff:** 2026-08-18 UTC  
**Supersedes:** `reflex-framework-master-plan.md` and `reflex-framework-task-manifest.yaml`  
**Proposed repository:** `reflex`  
**License:** `MIT OR Apache-2.0`  
**Primary deployment:** one local, single-process, memory-primary runtime on a 64 GiB workstation
**Default implementation language:** Rust only  
**Optional interoperability:** post-v1 adapters; never required by the native path

This document defines the product, architecture, algorithms, schemas, toolchain,
repository layout, performance budgets, and acceptance criteria. Evidence is
scientific output, not task-completion bookkeeping: only reconstructable
experiment artifacts and canonical release checks count.

A later accepted architecture decision record may replace an individual technical choice. It may not silently weaken a constitutional invariant, verifier boundary, scientific identity, or performance gate.

### 0.5 Governing v1 architecture amendment

[ADR 0014](adr/ADR-0014-memory-primary-runtime-and-evidence-bundles.md)
is the governing authority wherever this plan still describes SQLite,
PostgreSQL, remote/S3 object storage, wall-clock worker leases, fleets, or a
portable multi-process topology. Those requirements are superseded, including
the conflicting portions of §§3–5, §7.3–7.4, §9, §18, §24.2–24.3 and task cards
P2.4, P4.2, P4.3, P11, and P16.

The required v1 path is one process with a single-owner in-memory coordinator,
a bounded content-addressed `ArtifactArena`, monotonic attempt-epoch fencing,
and atomic local evidence bundles. Content identity, verifier authority,
immutable cell inputs, replay, stale-attempt rejection, snapshot verification,
and zero-resource cleanup remain release-blocking. The native schema line is
`arena:1 bundle:1 ledger:1 proto:1`. Legacy adapters may remain as
non-default compatibility code, but registered v1 experiments and release
acceptance may not select or depend on them.

---

## 0. Executive decision

Build Reflex in a new repository. Preserve `project-reflex` as the Lean research laboratory and historical scientific authority. Attach it later as a domain integration and regression target.

Reflex is:

> **A high-performance Rust framework that learns search and discovery policies from self-generated, externally verified experience.**

The framework repeatedly performs this loop:

```text
structured candidate generation
            ↓
search and resource allocation
            ↓
external verification
            ↓
immutable artifacts and experience
            ↓
dataset compilation
            ↓
all-Rust training
            ↓
frozen evaluation and promotion
            ↓
better search, richer knowledge, better proposals
            ↺
```

The core is general, but the models are initially domain-specific. Lean proof search, Wrela optimization, circuit synthesis, query optimization, and other domains can use the same architecture when they provide:

1. structured tasks and states;
2. bounded candidate generation or proposal validation;
3. objective verification;
4. measurable utility;
5. repeatable experiments.

### 0.1 The cognitive decomposition

Reflex deliberately places different kinds of capability in different components:

| Component | Responsibility |
|---|---|
| Learned weights | Statistical judgment and uncertainty |
| Verified library | Explicit accumulated knowledge |
| Search | Deliberation and counterfactual exploration |
| Verifier | Truth, equivalence, or soundness authority |
| Utility ledger | Measured realized economics |
| Research graph | Ancestry and delayed causal memory |
| Scheduler | Attention and compute allocation |
| Artifact store | Immutable evidence and replay inputs |

The model does not certify itself. The utility model does not define economics. The library does not mutate a running registered cell. A search failure does not become a negative theorem label.

### 0.2 Why the rewrite is necessary

The original plan used Rust for systems work and Python/PyTorch for training. The revised design removes Python from the required architecture for four reasons:

1. Reflex calls small models frequently inside search; language and process boundaries can dominate useful compute.
2. Burn now provides Rust-native training, autodiff, optimizers, and multiple CPU/GPU backends, including a low-overhead eager CPU backend intended for small models.
3. One language simplifies ownership, batching, resource accounting, deployment, restart, model promotion, and developer experience.
4. The initial model classes are small enough that specialized Rust kernels and trainers are tractable when benchmarks justify them.

All-Rust is not assumed to beat PyTorch on every tensor workload. Reflex chooses model backends through measured end-to-end benchmarks. The value is primarily low-overhead integration, predictability, portability, and one resource model.

### 0.3 M2A changes the framework requirements

The historical M2A record describes a strong negative result with the following
reported values:

- at 4× search budget, uniform solved 75.0% overall;
- the 2,607-parameter MLP solved 60.5%;
- the 99,902-parameter MLP solved 37.1%;
- all twenty neural lineage/budget registered comparisons failed;
- the larger network often consumed less CPU only because it failed earlier.

These numbers are design inputs, not accepted Reflex evidence. They remain
unverified until P13 imports the authoritative manifests, incidents, replay
objects, and lineage registrations and reconstructs them under the release
commit. Missing inputs keep the M2A gate open; implementations and tests must
not synthesize substitutes.

This result does **not** say that compositional search lacked headroom. It says the M1.5 learner failed to exploit it. The framework therefore treats these as first-class requirements:

- proof DAGs rather than one selected proof;
- multiple viable actions per decision state;
- known-dead and unknown candidates as different labels;
- bounded failures as censored episode evidence;
- interchangeable losses and model families;
- oracle ranking and first-action diagnostics;
- feature collision audits;
- exact inference overhead accounting.

A framework that stores only `(state, chosen_action, reward)` is rejected by design.

### 0.4 First release promise

Version 1 is accepted only after the same core completes four vertical slices:

1. a tiny exhaustively verified bit-vector optimizer;
2. an economically grounded Wrela optimization loop;
3. Lean M1.5 and M2A scientific reconstruction plus M2B follow-up;
4. a hermetic single-process fault matrix covering cancellation, stale attempt
   epochs, snapshot interruption, external verifier failure, strict evidence
   reconstruction, and zero leaked cell-owned resources.

The release is not blocked on learned taste or proposal generation. Those are later capabilities built on the grounded core.

## 1. Product definition

### 1.1 User promise

A domain author provides:

- task generation or imported tasks;
- canonical state and candidate identities;
- candidate enumeration or proposal validation;
- state transition semantics;
- artifact construction;
- one or more external verifiers;
- vector-valued utility evaluators;
- structural feature schemas;
- baseline policies;
- model configurations and parameter budgets;
- evaluation, promotion, and stopping rules.

Reflex provides:

- allocation-accounted single-process execution on the local host;
- deterministic AND-OR search;
- complete process and logical accounting;
- high-rate append-only evidence;
- content-addressed artifacts, datasets, and models;
- proof-DAG and multi-positive dataset compilation;
- Rust-native training and inference;
- model evaluation, promotion, shadowing, and rollback;
- immutable knowledge editions and retrieval;
- economic and delayed-utility accounting;
- strict reports and replay;
- attempt-epoch-fenced execution, atomic local snapshots, and owned-resource
  cleanup.

### 1.2 Primary persisted object: research experience

The central object is richer than an RL transition:

```text
Experiment
  └── Cell (immutable inputs)
       └── Attempt
            └── Episode
                 ├── State
                 │    └── CandidateBatch
                 │         └── Decision
                 │              └── AppliedCandidate
                 │                   └── AND child obligations
                 ├── Artifact
                 │    └── VerificationReceipt
                 ├── UtilityObservation[]
                 ├── KnowledgeUse[]
                 └── ResearchLineageEdge[]
```

One immutable evidence history can later produce several datasets:

- local action ranking;
- value estimation;
- taste prediction;
- proposal learning;
- curation;
- retrieval evaluation;
- delayed credit assignment;
- scientific reports.

### 1.3 Model roles

Reflex separates learned components initially:

**Local ranker**

```text
(state, legal candidates) → candidate priorities
```

Dense supervision. Called frequently. Usually tiny.

**Taste critic**

```text
(candidate/research branch at birth) → vector of expected future utilities
```

Sparse, delayed, censored supervision. Called at proposal/allocation boundaries.

**Proposal model**

```text
(current verified world and frontier) → new typed proposals
```

Enabled last. Never authoritative.

Shared trunks are allowed only after transfer and interference experiments demonstrate value.

### 1.4 Online knowledge and near-online weights

Knowledge and weights evolve on different clocks:

```text
milliseconds–seconds   inference and search
seconds–minutes        verify and append knowledge overlay
minutes–hours          compile data, train candidate, evaluate, promote
hours–days             consolidate knowledge, mine proof DAGs, assign delayed credit
```

Verified knowledge may become available to newly started exploratory episodes quickly. Running cells remain pinned. Weights are never updated in place. Reflex trains immutable descendant checkpoints, evaluates them, promotes qualified candidates, and swaps the active checkpoint only at a declared boundary.

### 1.5 Non-goals for v1

Reflex v1 does not provide:

- a general-purpose deep-learning framework;
- a generic autograd implementation;
- arbitrary Python model execution in the native hot path;
- cross-machine shared SQLite;
- SQL or remote object storage in the native v1 execution path;
- recovery of an unfinished, unsnapshotted in-memory run;
- cross-machine worker fleets or live migration;
- event-by-event database persistence;
- unconstrained learned theorem or program generation;
- verifier replacement by statistical confidence;
- distributed synchronous SGD;
- one universal model across all domains;
- automatic proof that a utility function captures human value.

### 1.6 Domains that fit

A domain is a strong fit when candidate generation is cheap, search ordering matters, verification is objective, utility is measurable, problems share structure, and repeated trials are affordable.

Examples:

| Domain | Authority | Economics |
|---|---|---|
| Lean proof search | Lean kernel | solve rate, actions, CPU, reuse |
| Wrela optimization | equivalence/soundness + conformance | cycles, words, memory, workload cost |
| Circuit rewriting | equivalence checker | delay, area, power |
| SQL optimization | relational equivalence/test oracle | latency, I/O, memory |
| Cryptographic kernels | functional proof + side-channel contract | cycles, code size |
| Bit-vector synthesis | exhaustive or SMT proof | operation cost, depth |

## 2. Constitutional invariants

These are release-blocking. Each receives an `INV-RFX-*` ID in the implementation matrix.

1. **Verifier authority.** A model score, search path, benchmark, or test suite never substitutes for the domain’s declared verifier.
2. **Immutable inputs.** Registered cells pin every semantic and experimental input by digest.
3. **No mutable model inside a cell.** A cell observes exactly one checkpoint per model role.
4. **No mutable knowledge inside a cell.** A cell observes exactly one base edition and optional immutable overlay snapshot.
5. **Censored means unknown.** Bounded failure is never converted to theorem falsehood or candidate deadness.
6. **Multiple valid routes survive.** Dataset compilation may not label every unchosen candidate negative.
7. **Raw utility is immutable.** Reward scalarization is derived and versioned; measured facts keep units and provenance.
8. **Search cost includes ML.** Feature extraction, retrieval, inference, batching, and framework CPU count in economics.
9. **No partial evidence publication.** Only a completely written, re-opened,
   digest-verified, atomically renamed evidence bundle is published.
10. **One accepted attempt.** Retries remain immutable; a monotonic attempt
   epoch determines the accepted attempt and rejects delayed work.
11. **Replayable claims.** Scientific and economic reports reconstruct from immutable evidence, not trusted summaries.
12. **Single-process coordination authority.** One in-memory state machine owns
   cell transitions; registered native runs cannot delegate authority to SQL or
   a remote coordinator.
13. **Bounded artifact arena.** Proofs, datasets, checkpoints, traces, and other
   immutable bulk bytes occupy a capacity-accounted content-addressed arena;
   exhaustion fails closed and pinned data is never silently evicted or spilled.
14. **No per-candidate process boundary.** Native scoring is in-process; external domain calls are batched.
15. **Bounded memory and queues.** Every channel, frontier, cache, batch, upload, and process pool has a declared capacity or budget.
16. **One CPU budget.** Tokio, search, verification, training, and analytics cannot oversubscribe hidden pools.
17. **Deterministic registered mode.** Candidate order, tie breaks, seeds, and compatibility inputs are explicit.
18. **Exploratory mode is labeled.** Dynamic overlays, adaptive portfolios, and experimental models cannot masquerade as confirmatory evidence.
19. **Knowledge is explicit.** New theorems and algorithms normally enter the library, not implicit weight memory.
20. **No learned proposal before taste gate.** Learned generation receives significant compute only after fixed-pool critic evaluation.
21. **No silent fallback.** Backend, verifier, model, knowledge, or resource substitutions require manifest authority.
22. **Performance is correctness for the framework.** A hot-path regression beyond the accepted budget blocks promotion/release.
23. **Cleanup is part of completion.** A cell is incomplete until its child processes, permits, buffers, scratch resources, and in-flight requests reconcile.
24. **Historical science remains historical.** The new framework does not rewrite accepted Project Reflex artifacts or claims.

## 3. Performance constitution

Performance is a design input, not a late optimization phase.

### 3.1 Reference hardware classes

**Canonical local reference:** the 64 GiB workstation running one Reflex
process. Every performance record names the exact CPU, core count, memory,
kernel, build profile, power policy, and calibration digest. Measurements from
other machines remain valid within their own registered host class and are
never pooled without calibration.

The default `ArtifactArena` cap is 48 GiB, leaving at least 16 GiB for code,
stacks, external verifiers, page cache, snapshot staging, and the operating
system. A registered manifest may choose a lower cap. Raising it requires a
measured whole-process RSS budget and must still leave explicit headroom; no
subsystem infers spare memory dynamically.

### 3.2 Hot-path rules

- No heap allocation per candidate after warmup.
- No string construction, JSON, serde, Protobuf, SQL, logging, artifact hashing,
  or bundle I/O per candidate.
- Candidate metadata and features use structure-of-arrays buffers.
- Search owns reusable arenas and buffer pools.
- Tiny-model inference consumes caller-owned contiguous slices.
- External processes receive state/candidate batches.
- Event producers append to thread-local/preallocated buffers.
- Artifact insertion deduplicates by digest and uses caller-owned or pooled
  buffers; immutable bytes are shared rather than copied between stages.
- Evidence-bundle I/O occurs only at declared snapshot barriers and uses
  bounded staging buffers.
- I/O uses bounded asynchronous queues; CPU work uses bounded dedicated pools.
- Every cache declares size, eviction, and accounting.
- Every background task has a shutdown and evidence-flush contract.

### 3.3 Initial v1 quantitative gates

The implementation may replace a threshold only through an ADR backed by measurements on the named host class.

| Gate | Initial requirement |
|---|---:|
| Candidate metadata construction | ≥ 5 million candidates/s/core |
| Dense f32 feature packing | ≥ 10 GiB/s/core |
| Uniform/no-op scoring | ≥ 10 million candidates/s/core |
| 2,607-param MLP, 64 candidates | p95 ≤ 100 µs on one performance vCPU |
| 2,607-param MLP, one candidate | p95 ≤ 5 µs |
| Search policy + feature overhead | ≤ 10% of search CPU for a promoted model unless net economics qualify |
| Event append | ≥ 500,000 representative events/s/core |
| Evidence overhead | ≤ 3% of cell CPU on dogfood workloads |
| Ledger recovery | ≥ 1 GiB/s on local NVMe |
| Dataset compaction | ≥ 500,000 candidate rows/s/core |
| Training batch delivery | loader CPU ≤ 10% of one core |
| 3K MLP, 1M rows, one epoch | ≤ 60 s on the registered canonical local host |
| ArtifactArena insertion | ≥ 1 GiB/s/core including BLAKE3 identity |
| ArtifactArena resident bytes | ≤ manifest cap; default 48 GiB |
| Whole-process peak RSS | < 60 GiB on the 64 GiB canonical local host |
| Local all-core utilization | ≥ 85%, excluding verifier waits and snapshot barriers |
| In-memory coordination overhead | < 2% of cell CPU; claim/finalize p95 ≤ 10 µs |
| Atomic bundle snapshot | ≥ 500 MiB/s for a ≥ 1 GiB bundle before the final fsync barrier |
| Local CLI startup | p95 < 50 ms |

### 3.4 Regression policy

- More than 5% regression on a registered hot benchmark fails automatically.
- A 2–5% regression requires an accepted ADR with end-to-end evidence and an expiry/replacement gate.
- Lower training loss never waives inference or search overhead.
- A model that appears cheaper only because it fails earlier does not qualify as efficient.
- Memory regressions are tracked independently from latency.

### 3.5 Resource ownership

The process creates one `ThreadBudget` from its cgroup/manifest allocation. Tokio receives only I/O threads. Search, verifier children, training, compaction, and analytics request permits from the same broker.

```rust
pub struct ThreadBudget {
    total: usize,
    permits: Arc<Semaphore>,
    registry: Arc<PoolRegistry>,
}

impl ThreadBudget {
    pub async fn acquire(
        &self,
        owner: &'static str,
        count: usize,
    ) -> Result<ComputeLease, BudgetError> {
        if count == 0 || count > self.total {
            return Err(BudgetError::InvalidRequest { count, total: self.total });
        }
        let permit = self.permits.clone().acquire_many_owned(count as u32).await
            .map_err(|_| BudgetError::Closed)?;
        self.registry.on_acquire(owner, count);
        Ok(ComputeLease { owner, count, permit, registry: self.registry.clone() })
    }
}
```

No subsystem creates Rayon’s global pool, hidden Burn threads, or unmanaged worker threads.

## 4. Technology decisions

### 4.1 Required stack

| Layer | Choice | Reason |
|---|---|---|
| Language | Rust 1.97.1, edition 2024 | Single ownership/performance model; current stable point release at cutoff |
| Async I/O | Tokio 1.52.x | Mature I/O runtime; used only for I/O/control |
| HTTP/operator API | Axum 0.8.x | Thin Tokio-native API layer |
| External protocol | Prost 0.14.x + `LengthDelimitedCodec` | Compact typed batches without mandatory gRPC |
| ML framework | Burn 0.21.x | Rust-native training/inference/autodiff/backends |
| Canonical CPU ML backend | Burn Flex | Low-overhead eager CPU path for small models |
| Accelerated ML candidate | Burn CubeCL CPU | Benchmark-selected for larger batches/models |
| Tiny-model ceiling | `reflex-ml-micro` | Specialized allocation-free linear/MLP path, only if benchmark-qualified |
| Runtime coordination | `reflex-meta::MemoryMetaStore` | Single-owner state transitions and attempt epochs without serialization |
| Runtime artifacts | bounded `ArtifactArena` | Content identity, immutable sharing, and explicit resident-memory ownership |
| Durable evidence | atomic local evidence bundle | One verified replay root with crash-safe publication |
| Durable analytical data | Arrow/Parquet 58.3.x | Columnar portable datasets |
| Rust analytics | DataFusion 54.1.x | Vectorized, streaming, multithreaded Rust query engine |
| Internal hashing | BLAKE3 | Fast content IDs |
| External/import hashing | SHA-256 | Compatibility with existing evidence/ecosystems |
| Logging/tracing | `tracing` + OpenTelemetry | Structured spans and optional export |
| Benchmarks | Criterion 0.8.x + custom end-to-end harness | Statistical micro and system benchmarking |
| Tests | cargo-nextest, proptest, cargo-fuzz, Loom, Turmoil | Fast suites plus concurrency/fault models |
| Profiling | pprof-rs and Linux perf where available | CPU/heap evidence |

Exact versions live in `Cargo.lock`. The plan records version families because
patch releases may land before implementation. Changing Burn, a persisted
format, the arena/bundle contract, or Arrow/Parquet compatibility requires an
ADR and full parity/performance gates.

### 4.2 Burn policy

Burn is wrapped behind `reflex-ml-burn`. No search, scheduler, domain, or storage crate imports Burn types. This protects Reflex from framework API churn and allows backend replacement.

Use:

- Flex for canonical CPU training/inference and small-model scientific runs;
- CubeCL CPU only after measured total-cost qualification;
- optional CUDA/Metal/WGPU features for exploratory larger models;
- Burnpack/Burn Store for native records;
- safetensors export for interoperability.

Do not implement general autograd. `reflex-ml-micro` supports only explicitly registered tiny architectures and exists only when it beats Burn by the ownership threshold.

### 4.3 DataFusion dependency isolation

DataFusion remains a separate crate boundary and never enters hot runtime
crates. It reads local bundle materializations and exchanges paths, digests,
schemas, and Arrow/Parquet data with the native pipeline. Any transitive
`object_store` dependency is an analytics implementation detail and does not
create a remote-storage capability or native runtime contract.

### 4.4 Memory-primary coordination policy

One coordinator owns the mutable experiment state. Read-heavy views use
immutable snapshots; writes are typed commands and never SQL. Each claim
increments the cell's attempt number and nonzero epoch with checked arithmetic.
Every publication and finalization compares the complete attempt identity.
Cancellation or retry first advances the epoch, making outstanding handles
stale before their resources are drained.

### 4.5 Artifact and durability policy

The `ArtifactArena` holds immutable, digest-addressed runtime bytes below its
manifest cap. Evidence durability is explicit: a snapshot barrier writes the
reachable root set, ledger, receipts, and canonical manifest into one local
bundle and publishes it atomically. An unsnapshotted run is never reported as
durable. SQL databases, filesystem CAS trees, and remote object stores are not
fallbacks for arena pressure.

### 4.6 Tools deliberately not foundational

| Tool | Decision |
|---|---|
| Python/PyTorch | Optional post-v1 interoperability, not native path |
| PyO3/Maturin | Optional adapter only |
| ONNX Runtime | Optional imported inference comparison |
| tch-rs/libtorch | Benchmark/reference backend only |
| Ray/RLlib/Reverb | Not used; semantics and evidence are domain-specific |
| DuckDB | Replaced by DataFusion for all-Rust analytics |
| SQLite/PostgreSQL | Removed from the native workspace |
| Remote object storage | Rejected in the native path |
| gRPC/tonic | Not mandatory; length-framed Prost is enough locally |
| Generic ORM | Rejected; storage APIs expose Reflex concepts and explicit SQL |

## 5. High-level architecture

```text
┌──────────────────── one Rust process / one ThreadBudget ────────────────────┐
│ reflex CLI                                                                  │
│      │                                                                      │
│      ▼                                                                      │
│ single-owner coordinator ── attempt epochs ── cell runtime/search/ML       │
│      │                                              │                       │
│      │ typed state                                   ├─ native domains       │
│      ▼                                              └─ batched external     │
│ immutable views + bounded ArtifactArena                verifier processes   │
│      │                         │                                            │
│      ├─ dataset/analytics ─────┼─ Rust training/evaluation/promotion         │
│      │                         │                                            │
│      └──────── snapshot barrier: frozen roots + ledger + receipts           │
└────────────────────────────────┬─────────────────────────────────────────────┘
                                 ▼
                     atomic local evidence bundle
                     (the durable replay boundary)
```

### 5.1 Local topology

A local user gets:

```text
.reflex/
  evidence/                 digest-named atomic evidence bundles
  staging/                  bounded, ignored until atomic rename
  config.toml
```

`reflex run` owns the coordinator, arena, runtime, training, and snapshot
barrier in one process. External verifier/domain processes are semantic
authorities, not Reflex workers; their protocol is batched and their process
trees remain cell-owned.

### 5.2 Post-v1 topology boundary

Fleet coordination, SQL persistence, remote object storage, and portable Reflex
workers are outside v1. A future adapter must consume and produce the same
immutable manifests and evidence bundles, but cannot add backend dispatch or
network checks to the native hot path. It requires a separate ADR and
qualification suite.

### 5.3 Native versus external domains

**Native Rust domain:** best throughput, in-process candidate generation/application, direct feature buffers.

**External-process domain:** existing engine retains semantic ownership;
batched Protobuf over UDS; immutable bulk bytes use negotiated bounded frames or
arena digest references.

The framework must fit both without `if domain == lean` branches.

## 6. Repository layout

```text
reflex/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── AGENTS.md
├── README.md
├── deny.toml
├── docs/
│   ├── architecture/
│   ├── adr/
│   ├── invariants.md
│   ├── protocols/
│   ├── storage/
│   ├── performance/
│   ├── tutorials/
│   └── implementation/
├── proto/
├── crates/
│   ├── reflex-types/
│   ├── reflex-canonical/
│   ├── reflex-cas/
│   ├── reflex-ledger/
│   ├── reflex-meta/
│   ├── reflex-engine/
│   ├── reflex-dataset/
│   ├── reflex-domain/
│   ├── reflex-protocol/
│   ├── reflex-domain-host/
│   ├── reflex-runtime/
│   ├── reflex-search/
│   ├── reflex-ml-core/
│   ├── reflex-ml-micro/
│   ├── reflex-ml-burn/
│   ├── reflex-training/
│   ├── reflex-eval/
│   ├── reflex-knowledge/
│   ├── reflex-economics/
│   ├── reflex-research-graph/
│   ├── reflex-scheduler/
│   ├── reflex-analytics/
│   ├── reflex-report/
│   ├── reflex-observability/
│   ├── reflex-bench/
│   └── xtask/
├── bins/
│   ├── reflex/
│   └── reflex-analytics/
├── domains/
│   ├── reflex-domain-bitvec/
│   ├── reflex-domain-wrela/
│   └── reflex-domain-lean/
├── sql/reports/
├── benches/
├── fuzz/
├── tests/
└── evidence/
```

### 6.1 Dependency direction

```text
reflex-types
   ↑
canonical / cas / ledger / economics
   ↑
domain / protocol / meta / dataset / ml-core
   ↑
runtime / search / training / knowledge
   ↑
scheduler / worker / CLI / domain integrations
```

Hot worker binaries must not depend on DataFusion. Domain crates may depend on SDK crates, never on scheduler internals. Optional interoperability crates are not workspace-default members until after v1.

## 7. Durable identity and content-addressed storage

### 7.1 Digest model

Every identity-bearing object is hashed over:

```text
schema domain tag
+ schema version
+ compatibility digest(s)
+ canonical payload
```

Internal objects use BLAKE3. Imported evidence may retain SHA-256. Digests are tagged, never bare `[u8; 32]` at APIs.

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub enum DigestAlgorithm {
    Blake3,
    Sha256,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct Digest {
    pub algorithm: DigestAlgorithm,
    pub bytes: [u8; 32],
}

macro_rules! typed_id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
        pub struct $name(pub Digest);
    };
}

typed_id!(CellId);
typed_id!(StateId);
typed_id!(CandidateId);
typed_id!(ArtifactId);
typed_id!(ModelCheckpointId);
```

### 7.2 Canonical encoding

Do not derive identity from JSON, arbitrary serde map order, `Debug`, source filenames, array position, hash-table iteration, or pointer addresses.

Canonical rules:

- little-endian fixed-width integers unless schema says varint;
- floats encoded by exact bits after schema-defined normalization;
- NaN rejected for identity-bearing scientific values unless a schema defines one canonical NaN;
- strings UTF-8 with explicit byte length and declared normalization policy;
- maps sorted by canonical key encoding;
- sets sorted and duplicate-rejected;
- enums encoded by stable semantic IDs, never Rust discriminant;
- optional fields have explicit presence byte;
- schema field order is written by hand and golden-tested.

```rust
pub trait CanonicalEncode {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError>;
}

pub fn content_id<T: CanonicalEncode>(domain: &'static [u8], value: &T) -> Result<Digest, CanonicalError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"RFXID\0");
    hasher.update(&(domain.len() as u32).to_le_bytes());
    hasher.update(domain);
    let mut writer = CanonicalWriter::hashing(&mut hasher);
    value.encode_canonical(&mut writer)?;
    Ok(Digest { algorithm: DigestAlgorithm::Blake3, bytes: *hasher.finalize().as_bytes() })
}
```

### 7.3 ArtifactArena and evidence-bundle contract

```rust
pub trait ArtifactArena: Send + Sync {
    fn insert(&self, bytes: PooledBytes, expected: Option<Digest>)
        -> Result<ArtifactHandle, ArenaError>;
    fn get(&self, digest: Digest) -> Result<ArtifactHandle, ArenaError>;
    fn pin(&self, digest: Digest, owner: OwnerId) -> Result<ArtifactPin, ArenaError>;
    fn usage(&self) -> ArenaUsage;
}
```

Insertion hashes and counts bytes before publication, rejects a supplied digest
mismatch, deduplicates an existing immutable object, and charges unique resident
bytes to the arena. The arena rejects capacity overflow before making an object
visible. Handles never expose mutable backing storage. Pinned objects cannot be
evicted; registered mode neither spills nor switches storage implementations.

Durable publication freezes the accepted-attempt view and reachable artifact
roots, drains ledger barriers, and writes a canonical bundle to a sibling
temporary path. The writer verifies the manifest, member lengths, and member
digests, fsyncs the staged tree, renames it atomically to the digest-derived
final path, fsyncs the parent, then reopens and verifies the result. Temporary,
incomplete, extra, duplicate, or corrupt members are never evidence.

### 7.4 Object classes

| Class | Examples | Retention |
|---|---|---|
| Evidence root | accepted ledgers, receipts, reports, registrations | pinned until a verified bundle succeeds |
| Release | promoted models and knowledge editions | pinned while active or snapshotted |
| Active | current cell inputs and outputs | pinned for the owning cell |
| Cache | regenerated indexes and derived views | evictable when unpinned |
| Ephemeral | failed attempts and scratch buffers | reclaimed after owner reconciliation |

Reclamation is arena-root reachability plus explicit ownership. It never scans
filenames or guesses importance, never reclaims a pin, and must reconcile to
the manifest budget at cell and experiment completion.

## 8. Evidence ledger

### 8.1 Why not database rows or NDJSON

Reflex can emit millions of candidate-level observations. Per-event SQL and
textual JSON would dominate CPU and allocation. The hot path writes compact
binary blocks into bounded reusable buffers; a snapshot barrier serializes
complete segments into the evidence bundle.

### 8.2 Segment layout

```text
SegmentHeader
  magic                "RFXSEG01"
  schema_id
  schema_version
  stream_id
  producer_id
  first_sequence
  compatibility_digest

repeated Block
  stored_length
  uncompressed_length
  event_count
  first_sequence
  last_sequence
  flags
  crc32c
  payload

optional SegmentFooter
  block index
  segment digest
```

A crash may remove or tear the trailing block. Every earlier complete block remains valid. Mid-file corruption fails reconstruction.

### 8.3 Writer design

Each search thread writes typed events into a reusable local encoder buffer. When full or at a barrier, it sends ownership of the buffer to one segment writer through a bounded channel. The writer assigns block framing and performs sequential I/O.

```rust
pub enum LedgerCommand {
    Block(EncodedBlock),
    Barrier { reply: oneshot::Sender<Result<LedgerPosition, LedgerError>> },
    Rotate,
    Finish { reply: oneshot::Sender<Result<ClosedSegment, LedgerError>> },
}

pub struct EventSink {
    tx: mpsc::Sender<LedgerCommand>,
    pool: BufferPool,
    encoder: EventEncoder,
}

impl EventSink {
    pub async fn flush_scientific(&mut self) -> Result<(), LedgerError> {
        if let Some(block) = self.encoder.take_nonempty_block(&self.pool)? {
            self.tx.send(LedgerCommand::Block(block)).await
                .map_err(|_| LedgerError::WriterClosed)?;
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx.send(LedgerCommand::Barrier { reply: reply_tx }).await
            .map_err(|_| LedgerError::WriterClosed)?;
        reply_rx.await.map_err(|_| LedgerError::WriterClosed)??;
        Ok(())
    }
}
```

Scientific events are never dropped. High-cardinality debug telemetry may be sampled only when its schema declares it non-authoritative.

### 8.4 Required event families

- experiment/generation/cell/attempt lifecycle;
- worker session and calibration;
- task and episode start/end;
- state discovery;
- candidate batch and feature reference;
- policy scores and selected frontier key;
- candidate application and AND obligations;
- cache/retrieval observations;
- artifact construction;
- verification receipt;
- utility observations;
- model shadow score;
- resource samples;
- lineage edges;
- incidents and cancellation.

### 8.5 Reconstruction

A report is authoritative only if it can be regenerated from:

```text
immutable cell manifest
+ ledger segments
+ referenced arena artifacts
+ versioned report queries/code
```

Summary files are caches. Deleting them must not destroy the claim.

## 9. Storage architecture

Reflex exposes three semantic stores: memory-primary coordination, the bounded
artifact arena, and immutable dataset manifests. Durability is the evidence
bundle boundary, not a mutable database.

### 9.1 MetaStore

```rust
#[async_trait::async_trait]
pub trait MetaStore: Send + Sync {
    async fn create_experiment(&self, record: NewExperiment) -> Result<ExperimentId, MetaError>;
    async fn enqueue_cells(&self, cells: &[NewCell]) -> Result<(), MetaError>;
    async fn claim_cell(&self, request: ClaimRequest) -> Result<Option<AttemptHandle>, MetaError>;
    async fn cancel_attempt(&self, attempt: &AttemptHandle) -> Result<(), MetaError>;
    async fn publish_attempt_artifacts(
        &self,
        attempt: &AttemptHandle,
        artifacts: &[ArtifactRef],
    ) -> Result<(), MetaError>;
    async fn finalize_attempt(
        &self,
        attempt: &AttemptHandle,
        result: FinalAttempt,
    ) -> Result<FinalizeResult, MetaError>;
    async fn compare_and_promote(&self, request: PromotionRequest) -> Result<PromotionResult, MetaError>;
}
```

### 9.2 Single-owner in-memory coordination

`MemoryMetaStore` is the native implementation. It uses bounded indexed
collections behind one mutation authority and publishes immutable read views.
Claim increments `attempt_no` and a nonzero monotonic epoch with checked
arithmetic. Every artifact reference and finalization presents the exact
`(cell_id, attempt_no, epoch)` handle; mismatch is a terminal stale-attempt
result, not a retry. Advancing the epoch precedes cancellation and resource
drain, so delayed tasks cannot race back into acceptance.

### 9.3 Atomic experiment snapshot

The snapshot barrier captures the coordination view, reachable arena digests,
ledger cut, verifier receipts, model/knowledge identities, query plans, and
compatibility line in one manifest. The final bundle digest covers the
canonical manifest and an exhaustive, sorted member inventory. Publication
follows §7.3 and is the only transition from ephemeral runtime state to durable
evidence.

### 9.4 DatasetStore

Datasets are immutable manifests over Parquet shards and optional RFXBATCH caches.

```rust
pub struct DatasetManifest {
    pub schema: DatasetSchemaId,
    pub source_cells: Vec<CellId>,
    pub source_ledgers: Vec<Digest>,
    pub shards: Vec<DatasetShard>,
    pub logical_rows: u64,
    pub decision_groups: u64,
    pub feature_schema: FeatureSchemaId,
    pub action_schema: ActionSchemaId,
    pub split_manifest: Digest,
    pub compiler: BuildIdentity,
}
```

### 9.5 Analytics

DataFusion reads local Parquet materialized from the active arena or a verified
evidence bundle. Canonical SQL files live in `sql/reports`. Every report records
query digests and DataFusion/Arrow versions.

The analytics binary is not linked into workers. Large queries receive memory/spill budgets from configuration.

## 10. Domain SDK

### 10.1 Typed native interface

```rust
pub trait Domain: Send + Sync + 'static {
    type Task: CanonicalEncode + Send + Sync;
    type State: Send + Sync;
    type Candidate: Send + Sync;
    type Transition: Send + Sync;
    type Artifact: CanonicalEncode + Send + Sync;
    type Verification: CanonicalEncode + Send + Sync;

    fn capabilities(&self) -> DomainCapabilities;
    fn task_id(&self, task: &Self::Task) -> Result<TaskId, DomainError>;
    fn initial_state(&self, task: &Self::Task, arena: &mut EpisodeArena) -> Result<StateHandle, DomainError>;
    fn state_id(&self, state: StateHandle, arena: &EpisodeArena) -> Result<StateId, DomainError>;

    fn enumerate_candidates(
        &self,
        state: StateHandle,
        arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError>;

    fn extract_features(
        &self,
        states: &[StateHandle],
        candidates: &CandidateBatch,
        arena: &EpisodeArena,
        output: &mut FeatureBatch,
    ) -> Result<(), DomainError>;

    fn apply_candidates(
        &self,
        state: StateHandle,
        candidates: &CandidateBatch,
        selection: &[CandidateIndex],
        arena: &mut EpisodeArena,
        output: &mut TransitionBatch,
    ) -> Result<(), DomainError>;

    fn reconstruct_artifact(
        &self,
        solved: SolvedRoot,
        arena: &EpisodeArena,
    ) -> Result<Self::Artifact, DomainError>;

    fn verify(&self, artifact: &Self::Artifact, budget: VerifyBudget)
        -> Result<Self::Verification, VerifyError>;

    fn evaluate_utility(
        &self,
        artifact: &Self::Artifact,
        verification: &Self::Verification,
        context: &UtilityContext,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError>;
}
```

### 10.2 Candidate contract

Candidate enumeration must be:

- complete for the declared action/proposal class;
- deterministic under the canonical state;
- identity-stable;
- finite or explicitly capped with a registered generation budget;
- independent of model scores;
- unable to invoke recursive proof search as a hidden legality test unless declared and charged.

### 10.3 Transition contract

```rust
pub enum TransitionOutcome {
    Closed { witness: DomainWitnessRef },
    Obligations { group: AndGroupRef },
    Contradiction { certificate: Option<DomainWitnessRef> },
    Invalid { code: InvalidCandidateCode },
    Unresolved { code: UnresolvedCode },
}
```

`Invalid` is a semantic/legality result for a candidate. `Unresolved` means the domain could not determine an outcome within declared limits. Neither automatically means the candidate is a training negative.

### 10.4 External process integration

The same semantic methods are exposed as batched messages. A handshake pins:

```text
protocol range
domain digest
action/feature schema digests
verifier implementation digest
maximum inline bytes
maximum states/candidates per batch
cancellation/deadline support
artifact replay support
```

Bulk proof objects and datasets travel by CAS reference. UDS is the default local transport. Stdio exists for portability/testing.

### 10.5 Verifier receipt

```rust
pub struct VerificationReceipt {
    pub verifier: VerifierId,
    pub implementation: Digest,
    pub semantic_anchor: Digest,
    pub artifact: ArtifactId,
    pub status: VerificationStatus,
    pub proof_or_certificate: Option<Digest>,
    pub assumptions: Vec<AssumptionId>,
    pub cpu_ns: u64,
    pub wall_ns: u64,
    pub peak_rss_bytes: u64,
    pub replay_command: ReplayCommand,
}
```

A successful search without an accepted receipt is not a solved task.

## 11. Search kernel

### 11.1 AND-OR semantics

A state is an OR node: search may choose among candidates. Applying one candidate may create an AND group: all child obligations must close.

```text
OR state S
  candidate A → AND {S1, S2}
  candidate B → terminal success
  candidate C → one child S3
```

Search succeeds when any OR alternative produces a completely solved artifact. It does not succeed when one child of an AND group closes.

### 11.2 Compact node storage

Use indexed arenas, not `Arc<Node>` graphs.

```rust
pub struct SearchNode {
    pub state: StateHandle,
    pub state_id: StateId,
    pub status: NodeStatus,
    pub first_parent: Option<EdgeIndex>,
    pub alternative_parents: SmallVec<[EdgeIndex; 2]>,
    pub best_logical_cost: u32,
    pub depth: u32,
}

pub struct AndGroup {
    pub parent_edge: EdgeIndex,
    pub children: Range<NodeIndex>,
    pub remaining: u32,
    pub failed: bool,
}
```

### 11.3 Deterministic frontier

Scores are guidance, not identity. Normalize floating-point edge cases before building a key.

```rust
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct FrontierKey {
    score_key: OrderedScore,
    logical_cost: Reverse<u32>,
    depth: Reverse<u32>,
    state: Reverse<StateId>,
    candidate: Reverse<CandidateId>,
    insertion: Reverse<u64>,
}

pub fn ordered_score(score: f32) -> Result<OrderedScore, PolicyError> {
    if !score.is_finite() {
        return Err(PolicyError::NonFiniteScore(score.to_bits()));
    }
    let normalized = if score == 0.0 { 0.0 } else { score };
    Ok(OrderedScore::from_bits(normalized.to_bits()))
}
```

The exact direction/reversal depends on heap implementation and is golden-tested.

### 11.4 Transpositions and bounded failure

A state table may reuse exact solved states and dominate strictly worse visits. It must not cache “unsolved at budget B” as “unsolvable.”

Safe records include:

- verified solved artifact/continuation;
- exact contradiction certificate;
- best known visit cost;
- active/in-progress marker scoped to one search;
- censored exhaustion with its exact budget, useful for analysis but not semantic pruning.

### 11.5 Search policy API

```rust
pub trait Ranker: Send + Sync {
    fn model_id(&self) -> ModelCheckpointId;
    fn score_batch(
        &self,
        features: &FeatureBatch,
        output: &mut [f32],
        telemetry: &mut InferenceTelemetry,
    ) -> Result<(), PolicyError>;
}
```

Policies may combine learned scores with exploration lanes. Every contribution and RNG seed is recorded.

### 11.6 Proof DAGs

After one or more certified solutions, Reflex merges canonical states and retains every candidate edge that appears in a complete verified route.

```rust
pub enum CandidateKnowledge {
    Viable { best_actions_to_go: u32, receipts: SmallVec<[Digest; 2]> },
    KnownDead { certificate: Digest },
    Unknown,
    Invalid { code: u32 },
}

pub struct DecisionGroup {
    pub state_id: StateId,
    pub candidate_ids: Vec<CandidateId>,
    pub labels: Vec<CandidateKnowledge>,
    pub feature_ref: Digest,
    pub source_episodes: Vec<EpisodeId>,
    pub coverage: CoverageClass,
}
```

An action that did not appear in a found proof remains `Unknown` unless separately proven dead.

### 11.7 Parallel search

V1 supports parallel episodes/cells and optionally parallel independent search lanes. Registered deterministic search does not let racy shared-frontier timing decide outcomes. If a domain needs parallel within-episode search, it must use a deterministic work-partition or record/replay-compatible semantics and qualify it separately.

## 12. Candidate and feature memory model

### 12.1 Structure of arrays

Candidates are grouped by state using offsets, while hot columns are contiguous:

```rust
pub struct CandidateBatch {
    pub group_offsets: Vec<u32>,        // len = states + 1
    pub ids: AlignedVec<CandidateId>,
    pub classes: AlignedVec<u16>,
    pub tie_breaks: AlignedVec<u64>,
    pub payload_handles: AlignedVec<CandidateHandle>,
    pub flags: AlignedVec<u16>,
}

pub struct FeatureBatch {
    pub rows: usize,
    pub cols: usize,
    pub values: AlignedVec<f32>,         // row-major initially
    pub schema: FeatureSchemaId,
}

impl FeatureBatch {
    #[inline]
    pub fn row(&self, index: usize) -> &[f32] {
        let start = index * self.cols;
        &self.values[start..start + self.cols]
    }
}
```

The initial row-major layout matches tiny MLP scoring. Alternative layouts require benchmarks and schema/version isolation.

### 12.2 Buffer lifecycle

- `EpisodeArena` owns domain state/candidate payloads.
- `CandidateBatch` and `FeatureBatch` come from bounded size-class pools.
- Search returns buffers after scoring/application.
- Capacity growth happens outside registered hot loops or fails with a named capacity result.
- Batch capacities and high-water marks are reported.

### 12.3 Feature schema

A feature schema includes:

```text
ordered column names
semantic definitions
dtype
normalization
missing-value behavior
state-shared versus candidate-specific status
version
```

No feature may use theorem-library position, candidate array position, task ID, generator family label, or source filename unless the experiment explicitly studies such leakage and names it.

### 12.4 M2A feature audit

The Lean M2B workflow must inspect whether states requiring different decisions map to identical or nearly identical feature vectors. Candidate model size cannot recover information absent from the representation. Search-context additions such as depth, sibling obligations, AND/OR origin, frontier width, or context growth receive new feature-schema versions and independent ablations.

## 13. Rust-native model system

### 13.1 Model specification

```rust
#[derive(Clone, Serialize, Deserialize)]
pub enum ModelArchitecture {
    Linear,
    Mlp {
        hidden: Vec<usize>,
        activation: Activation,
        bias: bool,
    },
    BottleneckMlp {
        bottleneck: usize,
        hidden: usize,
        activation: Activation,
    },
    BurnCustom {
        factory: RegisteredFactoryId,
        config: CanonicalConfig,
    },
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub role: ModelRole,
    pub architecture: ModelArchitecture,
    pub input_schema: FeatureSchemaId,
    pub output_schema: ModelOutputSchemaId,
    pub parameter_budget: ParameterBudget,
    pub dtype: ModelDType,
    pub canonical_backend: BackendClass,
}
```

Exact trainable parameter count is computed after model construction and appears in the checkpoint.

### 13.2 Two model tiers

#### Burn tier

Use for:

- arbitrary registered MLPs;
- taste critics;
- shared encoders;
- attention/graph models;
- future proposal models;
- accelerated training.

A simplified Burn ranker:

```rust
use burn::{module::Module, nn, tensor::{backend::Backend, Tensor}};

#[derive(Module, Debug)]
pub struct RankMlp<B: Backend> {
    input: nn::Linear<B>,
    hidden: nn::Linear<B>,
    output: nn::Linear<B>,
    gelu: nn::Gelu,
}

impl<B: Backend> RankMlp<B> {
    pub fn forward(&self, x: Tensor<B, 2>) -> Tensor<B, 2> {
        let x = self.gelu.forward(self.input.forward(x));
        let x = self.gelu.forward(self.hidden.forward(x));
        self.output.forward(x)
    }
}
```

Actual construction uses version-pinned Burn APIs and is covered by compile tests.

#### Micro tier

Use only for registered linear and two-layer MLPs. It owns:

- contiguous aligned f32 weights;
- direct slice inference;
- optional manual AdamW training;
- no tensor object allocation in inference;
- exact parity tests against Burn.

```rust
pub struct MicroMlp {
    input: usize,
    hidden: usize,
    w1: Box<[f32]>,
    b1: Box<[f32]>,
    w2: Box<[f32]>,
    b2: f32,
}

impl MicroMlp {
    pub fn score_rows(&self, features: &[f32], rows: usize, out: &mut [f32], scratch: &mut [f32]) {
        assert_eq!(features.len(), rows * self.input);
        assert!(scratch.len() >= rows * self.hidden);
        assert!(out.len() >= rows);

        for row in 0..rows {
            let x = &features[row * self.input..(row + 1) * self.input];
            let h = &mut scratch[row * self.hidden..(row + 1) * self.hidden];
            for j in 0..self.hidden {
                let mut sum = self.b1[j];
                let w = &self.w1[j * self.input..(j + 1) * self.input];
                for k in 0..self.input {
                    sum = x[k].mul_add(w[k], sum);
                }
                h[j] = sum.max(0.0);
            }
            let mut score = self.b2;
            for j in 0..self.hidden {
                score = h[j].mul_add(self.w2[j], score);
            }
            out[row] = score;
        }
    }
}
```

This readable scalar/autovectorized version is the reference. Unsafe SIMD enters only after assembly/benchmark evidence and complete parity testing.

### 13.3 Backend selection

Reflex benchmarks actual model specs on actual host classes:

```text
Burn Flex
Burn CubeCL CPU
micro reference
micro optimized
optional GPU backend
optional libtorch reference
```

Compare:

- batch 1/8/32/64/128/512 forward;
- backward and optimizer step;
- startup and checkpoint load;
- peak RSS;
- thread scaling;
- convergence on a fixed dataset;
- end-to-end search CPU and wall.

Backend choice is part of model runtime identity. Canonical scientific runs begin with f32 CPU.

### 13.4 Checkpoints

Checkpoint manifest fields:

```text
model spec and exact parameter count
weights digest
optimizer state digest
normalization digest
training dataset and sampler digests
training code/build identity
backend and device
step/epoch/batch position
all RNG states
training/dev metrics
calibration metrics
parent checkpoint
```

A checkpoint is immutable. Stable/candidate/experimental are metadata roles pointing at checkpoint IDs.

### 13.5 Atomic model activation

Workers pin the active model at cell start. Exploratory episode-level swaps may use `ArcSwap`, but registered cells forbid them.

```rust
pub struct ModelRegistry {
    active: arc_swap::ArcSwap<ModelBundle>,
}

impl ModelRegistry {
    pub fn pin(&self) -> Arc<ModelBundle> {
        self.active.load_full()
    }

    pub fn promote_between_generations(&self, next: Arc<ModelBundle>) {
        self.active.store(next);
    }
}
```

The metadata promotion transaction is authoritative; in-memory swap follows it and records success.

## 14. Dataset semantics and learning objectives

### 14.1 Honest supervision

For a decision state with candidates A, B, C, D:

```text
A → certified solution, cost-to-go 6
B → certified solution, cost-to-go 8
C → untried
D → certified dead
```

Correct training information is:

```text
A preferred over B
A and B preferred over D
C receives no semantic negative gradient
```

It is not:

```text
A positive; B/C/D negative
```

### 14.2 Pairwise loss

For known comparable pairs `(i, j)` where candidate `i` should rank above `j`:

```rust
fn pairwise_logistic(scores: &[f32], pairs: &[(usize, usize, f32)]) -> f32 {
    let mut loss = 0.0;
    let mut weight_sum = 0.0;
    for &(better, worse, weight) in pairs {
        let margin = scores[better] - scores[worse];
        loss += weight * (1.0 + (-margin).exp()).ln();
        weight_sum += weight;
    }
    if weight_sum == 0.0 { 0.0 } else { loss / weight_sum }
}
```

Production uses Burn tensors, stable softplus, masks, and grouped reduction.

### 14.3 Listwise target

For viable candidates with known cost-to-go `c_i`:


distribution proportional to:

```text
exp(-c_i / temperature)
```

Unknown candidates are masked out. Known-dead candidates may receive explicit floor mass only in a registered objective.

### 14.4 Censored episode evidence

An episode ending because of action, CPU, wall, node, verifier, or memory budget records:

```text
status = exhausted
budget = exact budget identity
observed work
frontier summary
candidate/state coverage
```

It does not produce “all actions seen were bad.” Censored evidence may train separate survival/cost models or influence sampling, but semantic ranking labels require positive/negative evidence defined by the dataset schema.

### 14.5 Proof-DAG compiler

Algorithm:

1. collect all kernel-certified proof artifacts for source episodes;
2. replay to state/candidate identities;
3. merge identical states;
4. retain each candidate edge belonging to a complete proof;
5. propagate minimum known remaining cost backward;
6. attach known-dead certificates when available;
7. leave all other complete-candidate-list entries unknown;
8. emit `DecisionGroup` records.

### 14.6 Training batch cache

Parquet is durable and analytical. Training uses an RFXBATCH cache:

```text
header
  magic/version
  source dataset digest
  feature/action schema
  dtype/dimensions
  group and candidate counts
  section offsets
  CRC table

sections
  group_offsets
  features
  status masks
  cost_to_go
  weights
  source IDs
```

Files are immutable, mmap-able, and page-cache friendly. Deterministic shuffles store index permutations or counter-RNG state.

### 14.7 Custom Burn training loop

```rust
for epoch in resume.epoch..config.epochs {
    for batch in loader.batches_from(resume.batch) {
        budget.check()?;
        let input = batch.features_tensor::<TrainBackend>(&device);
        let scores = model.forward(input);
        let loss = objective.compute(scores, &batch)?;

        if !loss.clone().into_scalar().is_finite() {
            return Err(TrainError::NonFiniteLoss { epoch, batch: batch.index });
        }

        let grads = loss.backward();
        let grads = GradientsParams::from_grads(grads, &model);
        model = optimizer.step(config.learning_rate, model, grads);

        metrics.observe(&batch, &loss, &model)?;
        if checkpoint_policy.should_save(global_step) {
            recorder.save(&model, &optimizer, &loader.state(), &metrics)?;
        }
        global_step += 1;
    }
}
```

Exact Burn API details are version-locked in implementation; the control flow and evidence requirements are normative.

### 14.8 Offline diagnostics before search

Every candidate model receives:

- top-1 known-viable rate;
- top-k viable recall;
- MRR of cheapest known route;
- pairwise ordering among viable actions;
- NDCG/cost-sensitive ranking;
- score entropy and margins;
- top-action diversity;
- family/stratum breakdown;
- feature-collision error slices;
- proof-DAG oracle ceiling.

A model with high training accuracy and low held-out viable recall is overfitting or fitting bad supervision, not “reasoning deeply.”

## 15. Training, evaluation, and continual adaptation

### 15.1 Three rhythms

**Fast loop — search and knowledge:**

```text
search → verify → append artifact/overlay → next exploratory episode may pin it
```

**Slow loop — weights:**

```text
experience threshold → compile dataset → train candidate → frozen evaluation → promote/reject
```

**Sleep loop — consolidation:**

```text
deduplicate → mine proof DAGs → rebalance rare frontier → assign delayed credit → compact library → train larger critic
```

### 15.2 Autonomous generation state machine

```text
BOOTSTRAP
  ↓
COLLECTING
  ↓
VERIFYING
  ↓
COMPILING_DATASET
  ↓
TRAINING
  ↓
EVALUATING
  ↓
PROMOTION_PENDING
  ├──→ PROMOTED → next generation
  └──→ REJECTED → next collection or stop
```

Every state transition is durable and idempotent. There is no background promise to deliver a future result; the coordinator executes the current state until completion, failure, or stop.

### 15.3 Promotion

Example declarative rule:

```toml
[promotion]
required_lineages = 4
lineages_total = 5
max_family_solve_deficit = 0.02
max_stratum_solve_deficit = 0.02
max_cpu_ratio_at_equivalent_solve = 1.10
min_action_compression = 1.25
max_inference_share = 0.10
require_kernel_replay = true
```

Promotion compares matched accepted cells. If solve populations differ, cost compression is computed only under the registered restricted-work or equivalent-solve rule; a model cannot “win” by quitting.

### 15.4 Shadow and experimental models

Workers may score a bounded shadow sample with candidate/experimental models. Shadow output:

- never changes decisions;
- consumes an explicit resource lane;
- produces offline ranking evidence;
- can trigger new training/evaluation, not direct promotion.

### 15.5 Stop conditions

```toml
[stop]
max_generations = 12
max_summed_cpu_hours = 200.0
max_machine_hours = 80.0
no_promotion_generations = 3
min_marginal_utility_per_cpu_hour = 0.01
cleanup_reserve_machine_hours = 2.0
```

Hard compute limits permit at most already-leased bounded cells to finish. Cleanup and report reconstruction retain reserved budget.

## 16. Verified knowledge, retrieval, and economics

### 16.1 Knowledge classes

Do not pool these in one “library size” claim:

1. exact search memory;
2. instantiable theorem/fact;
3. proof macro or plan;
4. optimization/rewrite rule;
5. speculative verified research artifact.

Each has different retrieval and capability meaning.

### 16.2 Edition model

```rust
pub struct KnowledgeEdition {
    pub domain: DomainId,
    pub compatibility: CompatibilityDigest,
    pub records: Vec<KnowledgeRecordId>,
    pub retriever: RetrieverSpec,
    pub curator: CuratorSpec,
    pub parent: Option<KnowledgeEditionId>,
}
```

Records never disappear from archival storage. Curation selects active hot/warm/archive tiers.

### 16.3 Online activation

Exploratory mode may build an immutable overlay snapshot from newly verified records and let new episodes pin it. Registered cells remain unchanged. Periodic compaction creates a new edition.

### 16.4 Behavioral reuse

A knowledge record has increasingly strong use evidence:

```text
retrieved
attempted
transition succeeded
present in final proof/artifact
necessary under leave-one-out
reduced work under matched rerun
produced useful descendants
```

Reports state the strongest supported level.

### 16.5 Utility observation

```rust
pub struct UtilityObservation {
    pub subject: ResearchNodeId,
    pub evaluator: EvaluatorId,
    pub metric: MetricId,
    pub value: RationalOrFloat,
    pub unit: UnitId,
    pub direction: BetterDirection,
    pub population: Digest,
    pub evidence: Vec<Digest>,
    pub confidence: ConfidenceClass,
    pub observed_at_generation: GenerationId,
}
```

Raw observations are immutable. Derived reward functions are named and versioned.

### 16.6 Economic examples

Wrela:

```text
proxy cycles saved
+ physical timing calibration
+ workload frequency
- feature extraction
- inference
- retrieval
- compilation/application overhead
```

Mathematics:

```text
future problems newly solved
+ verified actions saved
+ child CPU saved
+ proof compression
+ cross-family reuse
- retrieval/index tax
- discovery/proof compute
```

### 16.7 Knowledge-model coevolution experiments

Once the system runs, four ordinary comparisons become possible:

```text
weights fixed, knowledge grows
knowledge fixed, weights train
both fixed, search budget grows
both grow together
```

This is the clean way to test whether explicit verified knowledge substitutes for weight capacity or merely complements it.

## 17. Research graph, delayed credit, and taste

This phase is deliberately after Wrela and M2B grounding.

### 17.1 Graph

```text
A theorem
  ├── generalized into B
  │      └── enabled C
  │             └── enabled algorithm D
  │                    └── saved 18% workload cost
  └── directly used in four proofs
```

Edges are typed and evidence-backed. Immediate utility remains distinct from propagated descendant value.

### 17.2 Potentiality

Taste predicts latent future utility from what is visible now. Candidate heads include:

- immediate reuse;
- long-horizon reuse;
- descendant utility;
- cross-domain transfer;
- compression;
- option value;
- discovery cost;
- uncertainty.

Elegance-like structural features are hypotheses about optionality, not rewards by definition.

### 17.3 Credit

A first implementation may compute discounted descendant credit:

```text
credit(parent) += edge_weight × discount^distance × realized_value(descendant)
```

But it must keep:

- direct realized value;
- propagated value;
- causal confidence;
- counterfactual/ablation evidence;

as separate fields.

### 17.4 Portfolio scheduling

A mature scheduler allocates bounded shares among:

```text
exploitation
near-frontier
exploratory/high-potentiality
adversarial/uncertain
speculative
```

It never allows a critic to eliminate all exploration or mandatory baselines.

### 17.5 Learned proposal gate

Learned generation enters only after:

1. deterministic mutation/abstraction/enumeration produce a frozen pool;
2. the taste critic ranks that pool better than strong baselines on held-out delayed utility;
3. critic-guided closed-loop exploration produces a better verified library under equal compute;
4. verifier and proposal queues remain bounded under invalid bursts.

## 18. Local single-process execution

The coordinator, scheduler, search, learning pipeline, metadata state, and
artifact arena run in one process. Native cells are tasks under that ownership
tree. External domain/verifier processes remain allowed because they retain
semantic authority, but they are bounded cell-owned children rather than
portable Reflex workers.

### 18.1 Worker contract

```text
verify build and schema compatibility
load an immutable cell manifest
verify every referenced digest
run host calibration
reserve bounded arena/model/domain resources
claim work with a monotonic attempt epoch
drain evidence and owned resources on every exit
```

Each cell declares CPU, arena, scratch, verifier-process, and queue limits. The
scheduler never changes a registered cell's resource class after observing its
outcome.

### 18.2 Resource ownership and cleanup

The process owns one `ThreadBudget`, bounded buffer and request pools, external
domain child processes, and disposable scratch. Completion requires:

1. the ledger finalization barrier has frozen all prior scientific events for
   the next snapshot;
2. all required arena objects verify by digest and every pin is accounted;
3. no compute permit or buffer remains checked out;
4. external children are drained or terminated as a process group;
5. in-flight requests have explicit terminal outcomes;
6. scratch and abandoned bundle staging paths are removed or the failure is
   recorded.

`CellContext::check_clean_shutdown` and
`DomainWorkerSupervisor::shutdown_and_drain` enforce the in-process and
external-process portions of this contract (INV-RFX-23).

### 18.3 Publication protocol

```text
cell completes search/verifier
  ↓
advance/freeze the accepted attempt view
  ↓
drain ledger buffers and freeze reachable arena roots
  ↓
write and verify a sibling temporary evidence bundle
  ↓
fsync staged members and directory
  ↓
atomically rename bundle and fsync parent
  ↓
reopen/verify bundle and record publication success
```

Delayed work presenting a prior attempt epoch cannot add a root, finalize the
cell, or enter the published bundle.

### 18.4 Hermetic fault matrix

The release suite injects these failures into the in-memory state machine,
snapshot writer, and external verifier process boundary:

- cancellation before claim;
- cancellation after claim before first event;
- cancellation mid-search;
- arena exhaustion and digest mismatch;
- interruption before, during, and after staged-bundle fsync;
- interruption before and after atomic rename;
- external verifier crash, timeout, and malformed reply;
- duplicate/late finalization attempt.

Strict reconstruction must select exactly one accepted attempt or report an
explicit incomplete cell. The final owned-resource inventory must be zero.

## 19. Analytics, reports, and operator UX

### 19.1 Parquet tables

Canonical logical tables:

```text
experiments
cells
attempts
episodes
states
candidate_decisions
transitions
artifacts
verification_receipts
utility_observations
knowledge_uses
research_edges
resource_samples
model_checkpoints
model_evaluations
promotions
incidents
```

Large feature matrices may be separate datasets keyed by `feature_ref`.

### 19.2 DataFusion catalog

```rust
pub async fn session(memory_limit: usize, spill_dir: PathBuf) -> Result<SessionContext> {
    let runtime = RuntimeEnvBuilder::new()
        .with_memory_limit(memory_limit, 0.8)
        .with_temp_file_path(spill_dir)
        .build()?;
    let config = SessionConfig::new()
        .with_target_partitions(4)
        .with_information_schema(true);
    Ok(SessionContext::new_with_config_rt(config, Arc::new(runtime)))
}
```

Actual APIs are pinned by DataFusion version. Queries are checked-in SQL or Rust plans with digests.

### 19.3 CLI

Core commands:

```text
reflex init
reflex doctor
reflex domain new
reflex experiment plan
reflex experiment run
reflex experiment status
reflex experiment stop
reflex experiment resume
reflex cell replay
reflex artifact inspect
reflex dataset compile
reflex train
reflex evaluate
reflex model promote
reflex knowledge build
reflex report
reflex bench
```

Every mutating command supports `--dry-run`. Every human view has stable `--format json` output.

### 19.4 Example experiment configuration

```toml
schema = "reflex.experiment.v1"
name = "bitvec-generation-1"
domain = "bitvec-v1"
corpus = "blake3:..."

[search]
algorithm = "best-first-and-or"
action_budget = 20000
node_budget = 100000
cpu_seconds = 30.0
policy = "stable"
exploration_uniform = 0.10

[model]
role = "ranker"
checkpoint = "blake3:..."
backend = "flex"

[knowledge]
edition = "blake3:..."
overlay = "none"

[collection]
lineages = 5
policies = ["uniform", "heuristic", "stable"]

[training]
architecture = { kind = "mlp", hidden = [32, 16], activation = "relu" }
loss = "masked_listwise_cost"
optimizer = { kind = "adamw", learning_rate = 0.001 }
epochs = 10

[promotion]
required_lineages = 4
max_inference_share = 0.10
```

Identity-bearing values resolve to immutable digests before enqueue.

### 19.5 Reports

Every report contains:

- claim and preregistered criterion;
- exact identities;
- accepted/missing/retried/quarantined cells;
- solve/correctness;
- logical work;
- CPU/wall/RSS/I/O;
- feature/inference/retrieval overhead;
- family/stratum breakdown;
- model and knowledge lineage;
- behavioral use/causal attribution;
- incidents;
- compute and cost proxy;
- cleanup inventory;
- report query/code digests.

## 20. Testing and reproducibility

### 20.1 Test layers

| Layer | Tools | Purpose |
|---|---|---|
| Unit | nextest | Local semantics and errors |
| Golden | checked-in binary/text fixtures | Canonical identities and formats |
| Property | proptest | Algebraic and round-trip invariants |
| Fuzz | cargo-fuzz | Untrusted persisted/protocol inputs |
| Concurrency | Loom | Writer actor, swaps, channels, permits |
| Multi-process | Turmoil + local child processes | Leases, partitions, retries, cleanup |
| Bench | Criterion/custom | Hot path and end-to-end gates |
| Scientific | domain matrices | Claims and negative results |

### 20.2 Mandatory fuzz targets

- canonical envelope decoder;
- digest parser;
- ledger header/block/event decoder;
- protocol frame and message decoder;
- CAS chunk manifest;
- model/checkpoint manifest;
- RFXBATCH reader;
- Parquet logical-schema importer boundaries;
- domain package decoders;
- configuration parser.

Every decoder has an input-size limit before allocation.

### 20.3 Loom models

Model:

- local attempt claim/retry/cancellation/finalization;
- bounded event-buffer ownership;
- model active-pointer promotion;
- thread-budget acquire/release/shutdown;
- atomic evidence capability handoff;
- local duplicate arena publication.

### 20.4 Turmoil scenarios

Simulate:

- cancellation racing a local completion;
- stale attempt finalization after retry;
- duplicate arena insertion;
- interruption at every evidence-bundle write boundary;
- restart from the last valid CURRENT bundle;
- external verifier response loss, delay, duplication, and crash;
- cancellation during each generation phase.

### 20.5 Reproducibility classes

**Logical deterministic:** tasks, candidates, transitions, proof/artifact, solve result, logical work.

**Canonical CPU model deterministic:** dataset order, initialization, checkpoint, metrics under the registered backend policy.

**Performance comparable:** host-calibrated CPU/wall/RSS; not expected byte-identical.

**Exploratory:** dynamic overlays or adaptive scheduling; fully logged but not pooled as confirmatory without freeze.

## 21. Dogfood implementation order

### Slice 0 — all-Rust ML feasibility

Before broad framework work, reproduce the 2,607-parameter reference architecture and benchmark:

```text
Burn Flex
Burn CubeCL CPU
micro scalar/autovectorized
optional libtorch/PyTorch historical reference
```

Measure single/batched inference, training, RSS, startup, scaling, and actual search integration. Burn remains sufficient even if micro ownership does not qualify.

### Slice 1 — bit-vector complete loop

One local command must:

```text
generate frozen tasks
→ run uniform/heuristic search
→ exhaustively verify
→ write/recover ledger
→ compile proof-DAG DecisionGroups
→ train ranker in Rust
→ evaluate and promote/reject
→ run generation 2
→ produce strict report
```

This is the framework reference tutorial.

### Slice 2 — Wrela first real domain

Wrela comes before Lean adapter to avoid recreating Lean-specific assumptions.

1. export one bounded Wrela kernel package;
2. attach persistent Wrela checker/cost worker;
3. rank certificate strategies;
4. train from generated experience;
5. evaluate identical authority decisions and net cost;
6. build immutable catalog edition;
7. run clipped-coverage/fixed-Q campaign.

### Slice 3 — Lean scientific integration

1. external adapter;
2. M1.5 reconstruction;
3. M2A import/reconstruction;
4. multi-proof DAG mining;
5. corrected datasets/losses/features;
6. oracle diagnostics;
7. preregistered M2B matrix.

This slice proves that the generic dataset/model interfaces can explain a real negative transfer result rather than merely reproduce a toy success.

### Slice 4 — knowledge economy

Separate exact cache, theorem, and macro snapshots; run junk injection, curation, behavioral reuse, and fixed-weight/fixed-library interaction studies.

### Slice 5 — taste and proposal

Only after Wrela economics and historical/delayed utility exist:

- research graph;
- option/descendant credit;
- fixed-pool taste critic;
- portfolio scheduling;
- learned proposal gate.

## 22. Implementation rules

For every task:

1. Read this architecture section and the exact task card.
2. Modify the owning crates/files unless the task explicitly requires a
   cross-cutting change.
3. Do not weaken an invariant or performance threshold to make tests pass.
4. When a required API no longer matches a pinned dependency, open an ADR
   with primary-source evidence and a minimal migration—not a silent workaround.
5. Implement the simplest correct/reference version first.
6. Add correctness tests before optimization and the named benchmark before
   changing hot code.
7. Run `cargo xtask check`. Store genuine ledgers, arena artifacts, datasets,
   checkpoints, and reports under `evidence/<experiment>/`; do not create task
   stamps or manual completion checklists.
8. A failed acceptance criterion remains visibly incomplete.

### 22.1 When to create an ADR

Create an ADR before:

- changing a durable schema;
- changing verifier authority;
- replacing a required dependency or backend;
- changing model/dataset label semantics;
- weakening or redefining a performance gate;
- using unsafe code in a hot kernel;
- changing attempt-epoch fencing or atomic bundle publication;
- adding a mandatory non-Rust runtime;
- changing a historical scientific migration claim.

## 23. Release gates

### Gate A — foundation

P0–P5 complete. Native and external fixture domains pass conformance. Storage and ledger recover after injected crashes.

### Gate B — search and ML

P6–P9 complete. The 2,607-parameter numerical reference matches, DecisionGroups preserve unknowns, and one autonomous generation can promote/reject safely.

### Gate C — tutorial

P10 complete. Bit-vector generation 2 improves a frozen verified metric or returns a complete negative result that still demonstrates all lifecycle semantics.

### Gate D — resilience

Attempt-epoch fencing, atomic bundle publication, cancellation, external
verifier failure, retry, and cleanup pass the hermetic local fault matrix with
zero owned resources or staging paths.

### Gate E — real domains

P12 and P13 complete. Wrela produces an economically grounded result and Lean historical evidence reconstructs; M2B answers the primary attribution question.

### Gate F — knowledge

P14 complete. Knowledge editions, retrieval tax, behavioral reuse, and curation are measured rather than assumed.

### Gate G — v1

P16 acceptance closes every blocking invariant and performance threshold. P15 may remain post-v1 except for schemas that other components require.

## 24. Research and tool-selection synthesis

The implementation choices are based on current primary technical sources at the evidence cutoff.

### 24.1 Rust and Burn

Rust 1.97.1 is the pinned language baseline because the point release fixes an LLVM miscompilation and is the current stable release at the cutoff.

Burn 0.21 is selected because it provides:

- model training and inference in Rust;
- autodiff and optimizers;
- CPU backends for x86 and Arm;
- the Flex eager CPU backend aimed at small-model/low-overhead workloads;
- CubeCL-backed accelerated backends;
- backend-generic model code;
- native record and weight loading paths.

Burn explicitly remains under active development. Reflex therefore wraps it, pins it, and makes parity/performance testing mandatory for updates.

The micro tier is not a rejection of Burn. It is a benchmark-controlled specialization for models in the thousands of parameters where general framework dispatch can be meaningful relative to arithmetic.

### 24.2 Memory-primary state and attempt epochs

Single-process ownership removes SQL serialization and distributed lease
timers without weakening stale-work rejection. A checked monotonic attempt
epoch is sufficient because all authoritative mutation occurs in one state
machine. Immutable read views and arena handles give concurrent stages access
without duplicating ownership.

### 24.3 ArtifactArena and evidence bundles

Content addressing is useful independently of object storage: it deduplicates
immutable bytes, binds manifests, and makes replay verifiable. The arena keeps
that identity in memory under a hard resident-byte cap. Atomic local bundles
provide the narrower durability operation Reflex actually needs, with one
digest-verified publication boundary instead of a mutable database plus CAS
coordination protocol.

### 24.4 Arrow, Parquet, and DataFusion

Parquet is selected for durable analytical/training datasets because it supports column projection, compression, statistics, and portable columnar access. DataFusion is selected over DuckDB for the required path because it is a Rust-native streaming, vectorized, multithreaded query engine over Arrow with Parquet and object-store integration.

Parquet is not used as the high-rate event log. A custom append-only segment format has simpler crash semantics and lower hot-path overhead. Compaction converts evidence to Parquet afterward.

### 24.5 Prior systems implications

The general search → verified experience → training → improved search loop has strong precedent in expert iteration, formal theorem proving, algorithm discovery, and evaluator-driven evolution. Reflex does not claim this flywheel as novel.

The framework tests a more specific systems hypothesis:

> Learned local judgment, long-horizon taste, explicit verified knowledge, search, objective economics, and causal research memory can remain separable and reusable across multiple verified domains.

The M1.5 and M2A results make that hypothesis falsifiable. M1.5 showed a tiny ranker can work in a shallow domain and benefit from explicit knowledge. M2A showed the same recipe can fail badly on compositional search. Reflex is designed to make that distinction easy to investigate rather than hiding it behind one monolithic model.

## 25. Dependency baseline

These are the initial version anchors, not permission to float dependencies.

| Dependency | Initial line | Notes |
|---|---:|---|
| Rust | 1.97.1 | `rust-toolchain.toml` exact |
| Burn | 0.21.x | exact Cargo pin/lock; wrap internally |
| Tokio | 1.52.x | I/O/control runtime |
| tokio-util | 0.7.x | length-delimited framing |
| Axum | 0.8.x | thin operator API |
| Prost | 0.14.x | external protocol messages |
| BLAKE3 | current locked | internal hashes |
| sha2 | current locked | SHA-256 imports |
| Arrow/Parquet | 58.3.x | match DataFusion line |
| DataFusion | 54.1.x | analytics binary only |
| Criterion | 0.8.x | microbenchmarks |
| cargo-nextest | current locked tool | fast tests |
| proptest | current locked | properties |
| Loom | current locked | concurrency models |
| Turmoil | current locked | deterministic local fault simulation where useful |
| pprof | 0.15.x | CPU/heap profiles |
| tracing | current locked | spans/events |
| OpenTelemetry Rust | 0.32.x | optional export |

No dependency version is accepted solely because it is newer. Updates run compatibility, numerical, persisted-format, and performance gates appropriate to the dependency.

## 26. Primary technical sources

The following source set guided the stack decisions. The checked-in repository should preserve a dated research memo with exact publication/release identifiers and source digests or archived citations where licensing permits.

1. Rust Release Team, **Announcing Rust 1.97.1**, 2026-07-16.
2. Tracel AI, **Burn v0.21.0 release notes** and Burn repository documentation.
3. Apache DataFusion project, **DataFusion 54.1.0 documentation and release artifacts**.
4. Apache Arrow Rust crates, Arrow/Parquet 58.3.x dependency line.
5. Project Reflex, **Research Findings Through M1.5** and the accepted **M2A Compositional Headroom Result**.
6. Wrela, Pixels language/renderer contract, theorem-to-kernel correspondence, cost model, and P8R evidence.

## 27. Final architectural recommendation

Start the new repository now.

Do not migrate Project Reflex first. Build the performance substrate and bit-vector slice, attach Wrela as the first structurally different real domain, then attach Lean and reproduce historical evidence.

The most important design boundary is not “Rust versus Python.” It is:

```text
statistical judgment in weights
explicit knowledge in verified artifacts
search in a replayable engine
truth in an external verifier
value in measured economics
time in an immutable research graph
```

Rust makes that decomposition operationally clean and fast enough to run
continuously. Burn makes native training plausible. The micro tier ensures tiny
policies do not pay avoidable framework overhead. A bounded in-memory arena and
single-owner coordinator keep the hot pipeline direct; atomic bundles,
Parquet, and DataFusion preserve durable replay and offline analysis locally.

The implementation succeeds when a less capable agent can follow the task graph, build the system without hidden context, run it cheaply, and obtain results whose truth, provenance, performance, and economics can all be reconstructed.

## 28. Reference implementation patterns for tricky subsystems

These examples are normative at the algorithm and failure-semantics level. Exact imported API names may change with pinned crate versions.

### 28.1 External domain protocol

```proto
edition = "2023";
package reflex.domain.v1;

message Digest {
  string algorithm = 1;
  bytes value = 2;
}

message HandshakeRequest {
  uint32 min_protocol = 1;
  uint32 max_protocol = 2;
  Digest framework_build = 3;
  uint32 requested_max_frame_bytes = 4;
}

message HandshakeResponse {
  uint32 selected_protocol = 1;
  Digest domain = 2;
  Digest action_schema = 3;
  Digest feature_schema = 4;
  Digest verifier = 5;
  uint32 max_states_per_batch = 6;
  uint32 max_candidates_per_batch = 7;
  uint32 max_inline_bytes = 8;
  bool supports_cancellation = 9;
  bool supports_artifact_replay = 10;
}

message ExpandBatchRequest {
  uint64 request_id = 1;
  uint64 deadline_mono_ns = 2;
  repeated bytes state_handles = 3;
}

message CandidateGroup {
  bytes state_handle = 1;
  repeated bytes candidate_ids = 2;
  repeated uint32 candidate_classes = 3 [packed = true];
  repeated fixed64 tie_breaks = 4 [packed = true];
  bytes shared_feature_payload = 5;
  bytes candidate_feature_payload = 6;
}

message ExpandBatchResponse {
  uint64 request_id = 1;
  repeated CandidateGroup groups = 2;
}
```

The transport rejects frames larger than the negotiated maximum before allocation. Request IDs are unique per connection. Late responses after cancellation are consumed and recorded but never applied.

### 28.2 Single-owner local coordination

```rust
pub fn drain_ready(
    state: &mut LocalRunState,
    execute: impl Fn(CellJob) -> Result<CommittedEvidence>,
) -> Result<()> {
    while let Some(job) = state.claim_next()? {
        let evidence = execute(job)?;
        state.finalize(job.ticket, AttemptOutcome::Accepted, &evidence)?;
    }
    Ok(())
}
```

One owner mutates dense bounded state. Claims are deterministic and a worker
result cannot finalize until an atomic evidence commit has minted its
capability.

### 28.3 Attempt-epoch finalization

```rust
let job = state.claim_next()?.ok_or(NoReadyCell)?;
let evidence = bundles.commit(schema, roots)?.1;
state.finalize(job.ticket, AttemptOutcome::Accepted, &evidence)?;
```

A mismatched cell, attempt number, epoch, or manifest is rejected. Retry and
cancellation advance the epoch, so a delayed result cannot become accepted.

### 28.4 Ledger recovery

```rust
pub fn recover_segment(file: &mut File) -> Result<RecoveredSegment, LedgerError> {
    let header = read_and_validate_header(file)?;
    let mut blocks = Vec::new();
    loop {
        let offset = file.stream_position()?;
        match try_read_block_header(file)? {
            None => break,
            Some(block_header) => {
                if block_header.stored_length > MAX_BLOCK_BYTES {
                    return Err(LedgerError::OversizedBlock { offset });
                }
                let mut payload = vec![0; block_header.stored_length as usize];
                if let Err(error) = file.read_exact(&mut payload) {
                    if error.kind() == ErrorKind::UnexpectedEof {
                        file.set_len(offset)?; // expected torn tail
                        break;
                    }
                    return Err(error.into());
                }
                if crc32c(&payload) != block_header.crc32c {
                    return Err(LedgerError::MidFileCorruption { offset });
                }
                validate_sequence(&blocks, &block_header)?;
                blocks.push(BlockIndexEntry::from_header(offset, block_header));
            }
        }
    }
    Ok(RecoveredSegment { header, blocks })
}
```

The actual implementation reuses a bounded buffer and distinguishes a torn final block from corruption followed by additional valid bytes.

### 28.5 Proof-DAG viability propagation

```rust
pub fn mark_verified_route(
    dag: &mut ProofDag,
    proof: &ReplayedProof,
    receipt: Digest,
) -> Result<(), DagError> {
    for step in proof.steps.iter().rev() {
        let state = dag.state_mut(step.state_id)?;
        let edge = state.candidate_mut(step.candidate_id)?;
        edge.receipts.insert(receipt);
        edge.status = CandidateStatus::Viable;
        edge.best_actions_to_go = edge.best_actions_to_go.min(step.actions_to_go);
        for child in &step.and_children {
            dag.add_child_edge(step.state_id, step.candidate_id, *child)?;
        }
    }
    Ok(())
}
```

No pass sets untouched candidates to dead. Known-dead status enters only through an explicit certificate or registered exhaustive oracle.

### 28.6 Masked listwise target construction

```rust
pub fn viable_target(
    labels: &[CandidateKnowledge],
    temperature: f32,
    out: &mut [f32],
) -> Result<bool, DatasetError> {
    out.fill(0.0);
    let mut max_logit = f32::NEG_INFINITY;
    for label in labels {
        if let CandidateKnowledge::Viable { best_actions_to_go, .. } = label {
            max_logit = max_logit.max(-(*best_actions_to_go as f32) / temperature);
        }
    }
    if !max_logit.is_finite() {
        return Ok(false); // no known viable supervision
    }
    let mut sum = 0.0;
    for (index, label) in labels.iter().enumerate() {
        if let CandidateKnowledge::Viable { best_actions_to_go, .. } = label {
            let weight = ((-(*best_actions_to_go as f32) / temperature) - max_logit).exp();
            out[index] = weight;
            sum += weight;
        }
    }
    for value in out.iter_mut() {
        *value /= sum;
    }
    Ok(true)
}
```

Unknown, invalid, and dead candidates receive zero target mass here. A separate registered term may compare viable candidates against known-dead candidates.

### 28.7 Atomic checkpoint promotion

```rust
pub async fn promote(
    meta: &dyn MetaStore,
    request: PromotionRequest,
    registry: &ModelRegistry,
    loader: &ModelLoader,
) -> Result<PromotionReceipt, PromotionError> {
    let candidate = loader.load_and_verify(request.candidate).await?;
    let receipt = meta.compare_and_promote(request).await?;
    if receipt.promoted {
        registry.promote_between_generations(candidate);
    }
    Ok(receipt)
}
```

If metadata promotion succeeds but the process dies before the in-memory swap, restart loads the metadata-authoritative stable checkpoint. If loading fails before the transaction, metadata does not change.

### 28.8 Worker claim loop

```rust
loop {
    if shutdown.is_draining() {
        break;
    }
    match meta.claim_cell(claim_request(&worker)).await? {
        Some(lease) => {
            let manifest = load_and_verify_manifest(&cas, lease.manifest_digest).await?;
            let result = run_cell(&worker, &manifest, &lease).await;
            publish_and_finalize(&cas, &meta, lease, result).await?;
        }
        None => {
            if worker_policy.should_exit_idle(idle_since.elapsed()) {
                break;
            }
            tokio::time::sleep(worker_policy.poll_interval).await;
        }
    }
}
```

Cell execution owns the declared compute lease. The claim loop itself never
runs a second ordinary cell concurrently unless the manifest explicitly
partitions the worker budget.

## 29. Implementation task cards

These cards define ownership and acceptance criteria. They are planning
guidance, not completion records; executable checks and reconstructable
scientific artifacts remain authoritative.

## P0 — Repository and engineering authority

**Goal:** Create the greenfield all-Rust workspace and checked-in authority documents.

**Exit gate:** A clean checkout passes the fast gate and no mandatory Python runtime or package exists.

### P0.1 — Create the all-Rust workspace and pin toolchains

**Phase:** P0  
**Dependencies:** None  
**Size:** M  
**Primary skill:** Rust build systems  
**Owned scope:** `workspace`, `xtask`

**Purpose.** Create a greenfield repository whose default build, test, training, and runtime path contains no Python dependency.

**Deliverables**

- Cargo workspace and crate directories from §8
- rust-toolchain.toml pinned to 1.97.1
- Cargo.lock committed
- MIT OR Apache-2.0 licensing and package metadata

**Implementation steps**

1. Create the workspace members exactly as listed in the repository layout.
2. Set resolver = 3, edition = 2024, rust-version = 1.97.1, and deny unsafe code by default.
3. Add the `reflex` binary with version output and keep offline analytics isolated.
4. Commit a lockfile generated on Linux and verify it on macOS arm64.

**Acceptance criteria**

- cargo build --workspace --locked succeeds from a clean checkout.
- cargo metadata contains no mandatory Python, PyO3, libtorch, or ONNX dependency.
- reflex --version prints the git commit, target triple, profile, and schema compatibility range.
- A changed toolchain or unlocked dependency causes the fast gate to fail.

**Performance acceptance**

- Clean release build completes within 12 minutes on the pinned reference-4vcpu-8gb build host after an empty registry cache.
- Incremental no-op cargo check completes within 4 seconds on the same host.

### P0.2 — Implement authoritative fast and deep check lanes

**Phase:** P0  
**Dependencies:** P0.1  
**Size:** M  
**Primary skill:** CI and test engineering  
**Owned scope:** `xtask`, `.github/workflows`

**Purpose.** Ensure every developer and CI invocation runs the same checked-in commands and produces machine-readable evidence.

**Deliverables**

- cargo xtask check
- cargo xtask check-deep
- fast and deep CI workflows
- JSON check report schema

**Implementation steps**

1. Wire rustfmt, clippy -D warnings, cargo nextest, doctests, generated-file freshness, dependency policy, and documentation checks.
2. Make the deep lane add Loom, Turmoil, fuzz smoke, corruption recovery, and distributed integration tests.
3. Write one deterministic report per lane containing commands, normalized
   status, and tool versions. Keep timings in transient CI telemetry.
4. Upload the report and logs even when a lane fails.

**Acceptance criteria**

- A formatting error, clippy warning, stale generated file, broken link, and failing test each fail the fast lane.
- Deep-only fault tests do not run in the fast lane.
- Local and CI reports are byte-identical after normalizing paths and timestamps.
- No workflow duplicates check logic outside xtask.

**Performance acceptance**

- Fast lane p95 is under 8 minutes on a warm reference-4vcpu-8gb CI runner.
- Deep lane exposes per-suite timing and flags any suite growing by more than 20% from its accepted baseline.

### P0.3 — Establish dependency, license, and advisory policy

**Phase:** P0  
**Dependencies:** P0.1  
**Size:** S  
**Primary skill:** Supply-chain security  
**Owned scope:** `xtask`, `deny.toml`

**Purpose.** Prevent silent dependency drift and make the evolving Rust ML ecosystem safe to consume.

**Deliverables**

- cargo-deny configuration
- cargo-audit configuration
- dependency exception registry
- SBOM generator

**Implementation steps**

1. Allow crates.io releases only by default; require exact workspace pins for Burn and persisted-format-critical crates.
2. Deny unknown licenses and duplicate semver-major versions of security-sensitive crates unless listed in the exception registry.
3. Generate SPDX or CycloneDX SBOMs for every release artifact.
4. Record the Burn, Tokio, protocol, arena/bundle, and Arrow/DataFusion compatibility assumptions in ADRs.

**Acceptance criteria**

- A deliberately vulnerable fixture is rejected.
- An unapproved Git source and license are rejected.
- The SBOM exactly reconstructs the native binary dependency graph.
- Dependency exceptions name an owner, rationale, expiry date, and benchmark or compatibility evidence.

**Performance acceptance**

- Supply-chain checks add less than 45 seconds to a warm fast lane.
- The release image dependency inventory is reproducible across two clean builds.

### P0.4 — Create ADR, invariant, and evidence traceability

**Phase:** P0  
**Dependencies:** P0.1  
**Size:** M  
**Primary skill:** Architecture documentation  
**Owned scope:** `docs/adr`, `docs/invariants.md`, `xtask`

**Purpose.** Make every constitutional rule auditable from design authority through implementation, tests, and failure behavior.

**Deliverables**

- ADR template and index
- stable invariant IDs
- invariant ownership matrix
- docs validator

**Implementation steps**

1. Assign an INV-RFX-* identifier to every invariant in §4 and §5.
2. For each invariant record authority section, owner crate, enforcing API, tests, failure mode, and performance measurement.
3. Require an ADR for changes to persisted schemas, compatibility digests, verifier authority, or benchmark gates.
4. Make supersession append-only: old ADRs remain unchanged and point to the successor.

**Acceptance criteria**

- Every invariant has at least one executable test or explicitly accepted deferred gate.
- Duplicate IDs, missing files, and uncovered blocking invariants fail docs validation.
- A schema change without an ADR and migration entry fails the fast lane.
- The generated invariant report links to the exact test evidence paths.

**Performance acceptance**

- Docs validation completes in under 2 seconds on the repository corpus.
- Traceability generation is deterministic and does not read network state.


## P1 — Performance constitution and measurement substrate

**Goal:** Make performance, resource accounting, and regression detection first-class before the hot path exists.

**Exit gate:** Pinned calibration hosts produce reproducible baseline artifacts; allocation, CPU, RSS, and benchmark regressions fail automatically.

### P1.1 — Build the benchmark harness and calibration profiles

**Phase:** P1  
**Dependencies:** P0.2  
**Size:** L  
**Primary skill:** Performance engineering  
**Owned scope:** `reflex-bench`, `xtask`

**Purpose.** Define reproducible micro, component, and end-to-end benchmarks before implementing hot subsystems.

**Deliverables**

- Criterion benchmark workspace
- calibration profile schema
- benchmark JSON exporter
- baseline comparison command

**Implementation steps**

1. Create named profiles for developer laptop, provider-neutral reference-4vcpu-8gb, and canonical scientific CPU.
2. Record CPU model, ISA, kernel, cgroup quota, governor, NUMA, memory, container image, and git identity.
3. Warm up each benchmark, report distributions, and retain raw samples.
4. Implement compare with explicit noise bands and per-benchmark regression thresholds.

**Acceptance criteria**

- Two runs on the same idle host agree within the registered noise envelope.
- A synthetic 10% slowdown fails comparison.
- Unsupported or throttled hosts are marked noncanonical rather than silently pooled.
- Raw benchmark samples can regenerate the summary.

**Performance acceptance**

- Benchmark harness overhead is below 1% for benchmarks longer than 100 ms.
- A full fast benchmark suite finishes in under 5 minutes on reference-4vcpu-8gb.

### P1.2 — Implement the global thread-budget broker

**Phase:** P1  
**Dependencies:** P1.1  
**Size:** L  
**Primary skill:** Rust concurrency  
**Owned scope:** `reflex-runtime`

**Purpose.** Prevent Tokio, search, verification, training, analytics, and BLAS-like backends from oversubscribing the same CPUs.

**Deliverables**

- ThreadBudget API
- scoped permits
- pool registry
- oversubscription diagnostics

**Implementation steps**

1. Represent the process CPU budget as permits assigned to named pools.
2. Create dedicated bounded pools for search, verifier children, training, compaction, and analytics; Tokio remains I/O-only.
3. Reject nested pool creation that exceeds available permits.
4. Expose per-pool queue depth, busy time, steals, and blocked duration.

**Acceptance criteria**

- A 4-vCPU configuration never creates more than four compute workers unless a test explicitly opts into oversubscription.
- Nested training during search blocks or defers rather than spawning hidden threads.
- Permit leaks are detected at shutdown.
- Pool ownership appears in diagnostics and traces.

**Performance acceptance**

- Synthetic mixed workloads sustain at least 90% CPU utilization without run-queue growth beyond 2× vCPU count.
- Broker acquire/release p95 is below 2 microseconds under 64 contending tasks.

### P1.3 — Implement child-process CPU, RSS, and I/O accounting

**Phase:** P1  
**Dependencies:** P1.2  
**Size:** M  
**Primary skill:** Linux systems  
**Owned scope:** `reflex-runtime`

**Purpose.** Preserve summed process CPU as the scientific compute measure and expose memory/I/O regressions.

**Deliverables**

- ProcessTreeAccountant
- Linux procfs backend
- portable fallback
- accounting fixtures

**Implementation steps**

1. Track parent and all descendant PIDs across fork/exec, process exit, and PID reuse using start times.
2. Sample user CPU, system CPU, RSS, peak RSS, read/write bytes, voluntary and involuntary context switches.
3. Reconcile final rusage on child exit so short-lived processes are not missed.
4. Store monotonic samples in the evidence ledger and final totals in the cell result.

**Acceptance criteria**

- Known CPU-burn and memory fixtures report within 3% of independent measurements.
- Exited grandchildren remain included.
- PID reuse does not merge unrelated processes.
- Unsupported platforms report a qualified coverage level.

**Performance acceptance**

- Sampling at 100 ms adds under 0.5% CPU to an 8-worker Lean cell.
- Accounting state stays below 4 KiB per live process.

### P1.4 — Add allocation and copy instrumentation to hot paths

**Phase:** P1  
**Dependencies:** P1.1  
**Size:** M  
**Primary skill:** Rust performance  
**Owned scope:** `reflex-bench`, `reflex-runtime`

**Purpose.** Make zero-allocation and zero-copy claims executable instead of aspirational.

**Deliverables**

- Counting allocator test feature
- copy counters
- allocation budget assertions
- heap-profile integration

**Implementation steps**

1. Provide a test-only global allocator that counts allocations and bytes by scoped operation.
2. Instrument feature packing, scoring, frontier push/pop, event append, and CAS staging boundaries.
3. Add benchmark assertions for warm-state allocations.
4. Integrate pprof heap profiles into deep performance evidence.

**Acceptance criteria**

- A deliberate Vec growth in candidate scoring fails its allocation budget test.
- Warm search expansion, micro-model scoring, and event append meet their declared allocation budgets.
- Instrumentation is compiled out of release workers unless explicitly enabled.
- Copy counters distinguish unavoidable domain payload copies from framework copies.

**Performance acceptance**

- Disabled instrumentation has no measurable effect above a 1% noise floor.
- Enabled scoped counting adds under 5% to microbenchmarks.

### P1.5 — Implement deterministic host calibration

**Phase:** P1  
**Dependencies:** P1.1, P1.3  
**Size:** M  
**Primary skill:** Benchmark operations  
**Owned scope:** `reflex-bench`, `reflex-runtime`

**Purpose.** Measure host capability before accepting scientific or performance-sensitive cells.

**Deliverables**

- Calibration command
- integer/FP/memory/process fixtures
- host class fingerprint
- admission policy

**Implementation steps**

1. Run one- and all-core search-step proxies, memory bandwidth, CAS streaming, process startup, and micro-ML inference.
2. Hash the measured and descriptive host identity into a host-class record.
3. Reject shared-CPU throttling and unexpected vCPU counts for performance cells.
4. Attach calibration to every worker session and cell result.

**Acceptance criteria**

- Identical machine classes cluster within registered variance.
- A throttled or shared-CPU host is rejected for performance experiments.
- A worker with missing calibration cannot claim work.
- Calibration results are immutable and reconstructable.

**Performance acceptance**

- Calibration completes in under 90 seconds on reference-4vcpu-8gb.
- Calibration uses under 256 MiB RSS and leaves no persistent scratch.

### P1.6 — Enforce performance budgets in CI and promotion

**Phase:** P1  
**Dependencies:** P1.1, P1.4, P1.5  
**Size:** M  
**Primary skill:** Performance governance  
**Owned scope:** `xtask`, `reflex-scheduler`

**Purpose.** Make speed and memory release criteria equal in authority to correctness tests.

**Deliverables**

- performance budget registry
- regression gate
- promotion hook
- waiver ADR schema

**Implementation steps**

1. Encode every §5 target with benchmark name, canonical host class, statistic, threshold, and owner.
2. Fail the fast performance lane at more than 5% regression and require an ADR plus evidence for 2–5%.
3. Prevent a model from promotion when inference overhead exceeds its search savings budget.
4. Track memory regressions separately from latency.

**Acceptance criteria**

- Missing baseline or wrong host class fails closed.
- Performance waivers expire and identify the replacement gate.
- Model promotion consumes accepted benchmark evidence rather than rerunning ad hoc commands.
- A deliberate 6% scorer regression blocks promotion and release.

**Performance acceptance**

- Gate evaluation completes in under 1 second for 10,000 benchmark records.
- Budget registry lookup adds no hot-path code or dependency.

## P2 — Canonical identity, ArtifactArena, and durable bundles

**Goal:** Provide stable identities and immutable bytes for every scientific and model artifact.

**Exit gate:** The bounded in-memory arena passes identity, deduplication,
capacity, ownership, and concurrency tests; atomic local bundles pass
interrupted-publication, corruption, and replay tests.

### P2.1 — Implement typed digests and durable IDs

**Phase:** P2  
**Dependencies:** P0.2  
**Size:** M  
**Primary skill:** Rust systems  
**Owned scope:** `reflex-types`

**Purpose.** Prevent identity confusion and ensure every durable object has content-derived provenance.

**Deliverables**

- Digest enum
- BLAKE3 and SHA-256 implementations
- typed ID newtypes
- strict text and binary codecs

**Implementation steps**

1. Use BLAKE3 for internal content IDs and SHA-256 for imported/external compatibility records.
2. Tag digests with algorithm and schema domain before hashing.
3. Create distinct newtypes for task, episode, state, candidate, artifact, model, dataset, knowledge edition, and cell IDs.
4. Reject untagged byte strings at durable boundaries.

**Acceptance criteria**

- Golden vectors match independent implementations.
- Cross-type ID assignment fails at compile time.
- Malformed algorithm, length, uppercase policy, and hex are rejected.
- Serde and binary round trips preserve exact canonical text.

**Performance acceptance**

- Hashing sustains at least 1.5 GiB/s on the calibration reference-4vcpu-8gb host for 64 MiB buffers.
- Typed ID parse/format p95 is below 500 ns for cached-size stack buffers.

### P2.2 — Implement canonical serialization and schema envelopes

**Phase:** P2  
**Dependencies:** P2.1  
**Size:** L  
**Primary skill:** Serialization design  
**Owned scope:** `reflex-canonical`, `reflex-types`

**Purpose.** Make identities independent of map order, process architecture, and incidental serializer behavior.

**Deliverables**

- CanonicalWriter
- schema envelope format
- normalization rules
- golden corpus

**Implementation steps**

1. Define explicit field order, integer width, float-bit policy, string normalization, collection sorting, and option encoding.
2. Wrap every durable payload in magic, schema name, version, compatibility range, payload length, and digest.
3. Forbid generic HashMap serialization in identity-bearing structures.
4. Generate cross-platform golden files and deliberate semantic-change fixtures.

**Acceptance criteria**

- Equivalent values with different construction order hash identically.
- Any semantic field change changes the digest.
- Unknown required fields and incompatible versions fail closed.
- x86_64 Linux and arm64 macOS produce identical goldens.

**Performance acceptance**

- Canonical encoding sustains at least 500 MiB/s for large flat records.
- Encoding a typical decision group allocates at most once into the caller-provided output buffer.

### P2.3 — Implement the bounded content-addressed ArtifactArena

**Phase:** P2  
**Dependencies:** P2.1, P2.2  
**Size:** L  
**Primary skill:** Rust memory systems
**Owned scope:** `reflex-cas`

**Purpose.** Share immutable artifacts across the hot pipeline without I/O,
backend dispatch, unbounded allocation, or identity drift.

**Deliverables**

- ArtifactArena with a manifest hard cap
- pooled immutable handles and explicit owner pins
- checked BLAKE3 insertion and deduplication
- usage/reconciliation inventory

**Implementation steps**

1. Hash and count caller-owned/pooled bytes, verify any expected digest, and
   charge unique bytes before visibility.
2. Deduplicate equal content and return immutable shared handles without a
   second payload copy.
3. Pin every object to an explicit cell, attempt, model/knowledge, dataset, or
   snapshot owner; reclaim only unpinned unreachable caches.
4. Reject checked-size or capacity overflow. Registered mode never spills,
   uploads, changes backend, or evicts a pin.

**Acceptance criteria**

- Concurrent identical inserts converge to one immutable allocation.
- Digest mismatch and arena exhaustion fail before publication.
- Cancellation releases all owner pins; completion rejects a leaked pin.
- Pinned objects survive cache reclamation and every usage counter reconciles.

**Performance acceptance**

- Insertion including BLAKE3 sustains at least 1 GiB/s/core.
- Resident bytes never exceed the manifest cap and whole-process RSS remains
  below 60 GiB on the canonical host.

### P2.4 — Implement atomic local evidence bundles

**Phase:** P2  
**Dependencies:** P2.3  
**Size:** L  
**Primary skill:** Filesystem durability
**Owned scope:** `reflex-cas`

**Purpose.** Convert one frozen in-memory experiment view into a complete,
crash-safe, locally durable replay root.

**Deliverables**

- canonical EvidenceBundleManifest
- bounded staging writer
- verified atomic publish and reopen
- interrupted-publication recovery

**Implementation steps**

1. Freeze accepted attempts, ledger cuts, reachable arena roots, receipts,
   datasets, query identities, and compatibility inputs.
2. Write an exhaustive sorted member inventory and bytes into a sibling
   temporary path through bounded staging buffers.
3. Verify all lengths/digests, fsync files and staged directory, atomically
   rename to the digest-derived destination, and fsync the parent.
4. Reopen and verify the final bundle before publication succeeds; remove or
   report abandoned staging paths during cleanup.

**Acceptance criteria**

- Failure before rename never creates a published bundle.
- Failure after rename leaves a bundle that reconstructs exactly.
- Missing, extra, duplicate, reordered, truncated, or corrupt members fail.
- The bundle reconstructs the accepted attempt and every scientific report
  without mutable runtime state.

**Performance acceptance**

- A bundle of at least 1 GiB stages at 500 MiB/s before the final fsync barrier.
- Staging memory remains within its declared buffer cap.

### P2.5 — Add chunked manifests for very large artifacts

> **Superseded by ADR 0014.** Native v1 divides work into bounded
> arena-resident cells/generations and writes bundle members directly. This
> upload/resume card has no v1 acceptance criteria.

**Phase:** P2  
**Dependencies:** P2.3, P2.4  
**Size:** M  
**Primary skill:** Storage engineering  
**Owned scope:** `reflex-cas`

**Purpose.** Avoid restarting multi-gigabyte uploads and permit deduplication and ranged reconstruction.

**Deliverables**

- ChunkManifest schema
- content-defined or fixed chunk policy
- parallel fetch
- manifest verification

**Implementation steps**

1. Use fixed-size chunks in v1 for deterministic implementation; default to 64 MiB with a final short chunk.
2. Hash and publish chunks independently, then publish a canonical manifest containing order, length, and root digest.
3. Support resumable upload by checking existing chunks.
4. Verify full reconstructed length and root digest before handing bytes to consumers.

**Acceptance criteria**

- A killed 10 GiB upload resumes without resending accepted chunks.
- Chunk reorder, omission, and substitution are detected.
- Single-object and chunked representations have distinct schema IDs but identical logical payload digest metadata.
- Ranged reconstruction returns exact bytes.

**Performance acceptance**

- Parallel reconstruction saturates at least 80% of measured object-store bandwidth with bounded memory.
- Manifest parse time is below 10 ms for 100,000 chunks.

### P2.6 — Implement CAS reachability, retention, and repair

> **Superseded by ADR 0014.** Arena pin/reachability reconciliation is P2.3 and
> durable publication is P2.4. Remote repair and metadata-driven CAS GC have no
> v1 acceptance criteria.

**Phase:** P2  
**Dependencies:** P2.3, P2.5, P4.1  
**Size:** M  
**Primary skill:** Storage operations  
**Owned scope:** `reflex-cas`, `reflex-meta`

**Purpose.** Control storage growth without allowing garbage collection to destroy scientific evidence.

**Deliverables**

- reachability walker
- retention classes
- two-phase GC
- repair report

**Implementation steps**

1. Classify objects as evidence-permanent, release, active, cached, or ephemeral.
2. Build reachability roots from immutable experiment manifests, accepted reports, promoted models, and pinned knowledge editions.
3. Mark in metadata, wait a configured grace period, then delete only unreachable nonpermanent objects.
4. Repair missing local cache objects from remote CAS and quarantine missing permanent objects.

**Acceptance criteria**

- Permanent evidence is never selected for deletion.
- A crash between mark and sweep is safe and idempotent.
- GC dry-run explains every retained and deleted object.
- A missing permanent object makes the audit fail closed.

**Performance acceptance**

- Reachability scans process at least 250,000 metadata references/s/core.
- GC uses bounded memory and streams graphs larger than RAM.

## P3 — Evidence ledger and external protocol

**Goal:** Persist high-rate research experience and support mature external verifiers without per-candidate IPC.

**Exit gate:** The binary ledger recovers exactly after injected crashes and the external domain protocol sustains batched round trips within its latency budget.

### P3.1 — Define versioned experience event schemas

**Phase:** P3  
**Dependencies:** P2.2  
**Size:** L  
**Primary skill:** Data modeling  
**Owned scope:** `reflex-ledger`, `reflex-types`

**Purpose.** Represent every decision, attempt, verifier result, utility observation, and lineage edge without assuming one valid route or one scalar reward.

**Deliverables**

- event enum and payload schemas
- sequence and clock rules
- compatibility tests
- event reference guide

**Implementation steps**

1. Define events for cell lifecycle, task, episode, state, candidate batch, decision scores, transition, AND obligations, artifact, verification, utility, knowledge use, resource sample, and lineage.
2. Use monotonic per-stream sequence numbers and explicit producer IDs; wall time is descriptive only.
3. Store durable IDs and references, not duplicated large payloads.
4. Document which events are required to reconstruct each report.

**Acceptance criteria**

- All required scientific outcomes reconstruct from events plus referenced CAS objects.
- Unknown optional event versions can be skipped; unknown required semantics fail.
- Multiple viable actions and censored outcomes have first-class encodings.
- Event ordering rules reject gaps, duplicates, and cross-stream ambiguity.

**Performance acceptance**

- A typical candidate score event encodes under 32 bytes per candidate excluding shared state features.
- Schema decoding sustains at least 5 million small events/s/core.

### P3.2 — Specify the binary evidence segment format

**Phase:** P3  
**Dependencies:** P3.1  
**Size:** M  
**Primary skill:** Binary formats  
**Owned scope:** `reflex-ledger`

**Purpose.** Persist high-rate append-only evidence without NDJSON overhead or synchronous per-event durability.

**Deliverables**

- segment header
- framed block format
- CRC32C validation
- format golden files

**Implementation steps**

1. Define a fixed header with magic, schema, stream ID, producer ID, first sequence, and creation identity.
2. Frame blocks with stored length, uncompressed length, event count, first/last sequence, flags, and CRC32C.
3. Use raw blocks by default; permit LZ4 only after benchmark evidence and record the codec in flags.
4. Close a segment with a canonical index/footer but make recovery independent of the footer.

**Acceptance criteria**

- Golden segments parse identically on both target architectures.
- Length overflow, CRC failure, sequence overlap, and truncated headers are rejected.
- A reader can scan a segment without allocating per event.
- Footer loss does not lose prior complete blocks.

**Performance acceptance**

- Sequential decode exceeds 750 MiB/s on calibration hardware.
- Per-block framing overhead stays below 0.5% for 1 MiB blocks.

### P3.3 — Implement the bounded event writer

**Phase:** P3  
**Dependencies:** P3.2, P1.2  
**Size:** L  
**Primary skill:** High-throughput I/O  
**Owned scope:** `reflex-ledger`

**Purpose.** Decouple search from persistence using bounded batching and explicit backpressure.

**Deliverables**

- EventSink API
- MPSC block builder
- segment rotation
- flush/barrier commands

**Implementation steps**

1. Accept typed events into preallocated per-thread buffers and transfer full buffers to one writer.
2. Batch to 1–8 MiB or a configured maximum latency, whichever occurs first.
3. Rotate by byte count, event count, or experiment boundary.
4. Implement barriers for checkpoints and finalization; ordinary events never fsync individually.

**Acceptance criteria**

- Producer order is preserved within each stream.
- Backpressure blocks or sheds only explicitly lossy telemetry; scientific events are never dropped.
- A finalization barrier guarantees all prior events are durable.
- Writer errors cancel the cell and prevent success publication.

**Performance acceptance**

- Synthetic append sustains at least 500,000 representative events/s/core.
- Ledger overhead is at most 3% of CPU on the bit-vector and Lean dogfood cells.
- Warm append performs zero heap allocations per event.

### P3.4 — Implement segment recovery, indexing, and compaction input

**Phase:** P3  
**Dependencies:** P3.2, P3.3  
**Size:** L  
**Primary skill:** Reliability engineering  
**Owned scope:** `reflex-ledger`

**Purpose.** Recover the maximal valid prefix after crashes and feed deterministic analytical compaction.

**Deliverables**

- recovery scanner
- segment index builder
- stream merger
- corruption quarantine

**Implementation steps**

1. Scan block by block and stop at the first incomplete or invalid trailing block.
2. Distinguish expected torn tail from mid-file corruption.
3. Build a compact sidecar index mapping sequence ranges and event classes to block offsets.
4. Merge producer streams using explicit causal references, not wall-clock ordering.

**Acceptance criteria**

- Kill injection at every write boundary recovers exactly the accepted prefix.
- Mid-file corruption quarantines the segment and fails scientific reconstruction.
- Repeated recovery produces identical index bytes.
- Compaction input contains no duplicate event identity.

**Performance acceptance**

- Recovery scans at least 1 GiB/s on local NVMe.
- Index size stays below 1% of segment size for representative workloads.

### P3.5 — Define the external-domain Protobuf protocol

**Phase:** P3  
**Dependencies:** P3.1  
**Size:** L  
**Primary skill:** Protocol design  
**Owned scope:** `reflex-protocol`, `proto`

**Purpose.** Let Lean and future mature engines retain their own process while avoiding chatty per-candidate calls.

**Deliverables**

- Protobuf Editions schema
- capability handshake
- batched request/response messages
- generated Rust code

**Implementation steps**

1. Define handshake fields for protocol range, domain digest, action/feature schema, max batch, verifier authority, cancellation, and optional capabilities.
2. Define batched task open, state expansion, candidate application, verification, artifact replay, and shutdown messages.
3. Carry request IDs, deadlines, compatibility digests, and error classes.
4. Keep bulk immutable payloads in CAS and pass references when larger than the negotiated inline limit.

**Acceptance criteria**

- Old and new compatible peers negotiate the highest common version.
- Digest disagreement fails before task execution.
- Unknown required capability fails closed.
- A batch can represent several states and all candidate applications without one RPC per candidate.

**Performance acceptance**

- Encoded control overhead averages below 64 bytes per candidate in a 64-candidate batch.
- Schema generation is deterministic and checked for freshness.

### P3.6 — Implement the framed external-domain transport

**Phase:** P3  
**Dependencies:** P3.5, P1.2  
**Size:** L  
**Primary skill:** Async Rust I/O  
**Owned scope:** `reflex-protocol`, `reflex-runtime`

**Purpose.** Provide low-latency local process transport with strict bounds, cancellation, and restart behavior.

**Deliverables**

- UDS transport
- stdio fallback
- LengthDelimitedCodec framing
- connection supervisor

**Implementation steps**

1. Use Unix domain sockets by default and length-delimited frames with a negotiated maximum.
2. Use one reader task and one writer task with bounded channels; decode off the I/O loop only when payload size warrants.
3. Implement correlated requests, out-of-order replies, deadlines, cancellation, and graceful drain.
4. Restart a failed domain process only at cell boundaries unless the cell manifest explicitly permits replay-safe restart.

**Acceptance criteria**

- Oversized, malformed, duplicate, and late replies are rejected.
- Cancellation closes outstanding requests and leaves no task falsely successful.
- Connection loss yields a named cell failure with durable evidence.
- The same integration suite passes over UDS and stdio.

**Performance acceptance**

- Handshake p95 is below 500 ms including process startup for the bit-vector external fixture.
- A 64-state expansion/apply round trip p95 is below 2 ms excluding domain computation.
- Transport adds under 5% wall time to the Lean dogfood arm.

## P4 — In-memory coordination and analytical data

**Goal:** Separate mutable coordination from immutable evidence and columnar analytical data.

**Exit gate:** `MemoryMetaStore` enforces bounded single-owner transitions and
attempt epochs; a snapshot atomically binds that accepted view to all evidence;
Parquet/DataFusion reconstruct registered local reports.

### P4.1 — Define domain-specific storage interfaces

**Phase:** P4  
**Dependencies:** P2.2  
**Size:** L  
**Primary skill:** Storage architecture  
**Owned scope:** `reflex-meta`, `reflex-cas`, `reflex-dataset`

**Purpose.** Expose Reflex concepts as direct memory-primary operations without
generic SQL, filesystem, or remote-backend dispatch.

**Deliverables**

- MetaStore trait
- ArtifactArena trait
- DatasetStore trait
- conformance test harness

**Implementation steps**

1. Define bounded methods for experiment creation, cell enqueue/claim/finalize,
   artifact-root publication, model promotion, knowledge edition publication,
   and lineage indexing.
2. Require complete attempt identity and monotonic epochs for every state
   transition that delayed work could reach.
3. Keep immutable arena payload APIs separate from mutable coordination.
4. Make the in-memory implementation the conformance authority.

**Acceptance criteria**

- Native framework crates contain no rusqlite, tokio-postgres, or object-store
  types or runtime backend switches.
- Every mutating method defines idempotency and conflict behavior.
- Capacity is checked before enqueue/publication and all counters reconcile.
- An attempt cannot publish a missing or unpinned arena digest.

**Performance acceptance**

- Coordination overhead stays below 2% of cell CPU.
- Bulk metadata APIs support at least 1,000 records per call.

### P4.2 — Implement single-owner in-memory metadata

**Phase:** P4  
**Dependencies:** P4.1, P1.2  
**Size:** L  
**Primary skill:** Rust concurrency
**Owned scope:** `reflex-meta`

**Purpose.** Make cell coordination a direct, bounded in-process state machine
with immutable read views.

**Deliverables**

- MemoryMetaStore
- bounded typed command/state collections
- immutable status/report views
- ownership and capacity inventory

**Implementation steps**

1. Pre-size bounded indexes from the experiment manifest and reject overflow.
2. Serialize authoritative mutation through one owner; expose immutable or
   lock-bounded read snapshots for status and reporting.
3. Use checked counters and nonzero typed identities throughout.
4. Reconcile cells, attempts, roots, promotions, and knowledge/model pins at
   every completion boundary.

**Acceptance criteria**

- Concurrent readers observe a coherent before-or-after view.
- Invalid identity, overflow, missing roots, and stale attempts fail without
  partially mutating state.
- Native registered configuration cannot select SQL or remote coordination.
- Completion rejects nonzero owned state.

**Performance acceptance**

- Claim/finalize p95 is at most 10 µs with 10,000 ready cells.
- Coordination CPU is below 2% of cell CPU and memory stays within its manifest
  allocation.

### P4.3 — Implement monotonic attempt-epoch fencing

**Phase:** P4  
**Dependencies:** P4.2
**Size:** M
**Primary skill:** State-machine correctness
**Owned scope:** `reflex-meta`, `reflex-scheduler`

**Purpose.** Reject delayed work and select exactly one accepted attempt without
wall-clock leases or a database.

**Deliverables**

- nonzero checked attempt epoch
- typed AttemptHandle
- stale publication/finalization rejection
- cancellation/retry transition tests

**Implementation steps**

1. Increment attempt number and epoch with checked arithmetic on every claim.
2. Require `(cell_id, attempt_no, epoch)` for root publication and finalization.
3. Advance the epoch before cancellation drains outstanding work.
4. Preserve immutable prior attempt records in the ledger and snapshot view.

**Acceptance criteria**

- Two handles cannot accept the same attempt epoch.
- A prior handle cannot publish after replacement or cancellation.
- Retry preserves prior evidence and receives distinct identities.
- Exactly one accepted attempt is selected or the cell remains incomplete.

**Performance acceptance**

- Epoch comparison is allocation-free and included in the 10 µs
  claim/finalize budget.

### P4.4 — Bind coordination to the atomic snapshot barrier

**Phase:** P4  
**Dependencies:** P2.4, P4.3
**Size:** L  
**Primary skill:** Crash consistency
**Owned scope:** `reflex-meta`, `reflex-scheduler`, `reflex-cas`

**Purpose.** Make the accepted in-memory state and immutable evidence one
verified, atomically durable replay root.

**Deliverables**

- frozen SnapshotView
- arena root pin set
- ledger barrier coordination
- post-rename reopen verification

**Implementation steps**

1. Freeze accepted attempts and advance/cancel every nonaccepted epoch.
2. Pin the exact arena root set and drain the authoritative ledger to a declared
   cut.
3. Pass the immutable view to the P2.4 bundle writer.
4. Record publication only after final-path reopen verification; always release
   snapshot pins and staging ownership.

**Acceptance criteria**

- A bundle cannot mix coordination views or include a stale attempt.
- Failure before rename leaves no published result; failure after rename
  reconstructs exactly.
- Snapshot cancellation releases all roots, buffers, and staging paths.
- Published reports reconstruct with no mutable store.

**Performance acceptance**

- Snapshot throughput and staging memory meet P2.4; freeze/thaw overhead is
  reported separately and remains below the registered barrier budget.

### P4.5 — Implement Parquet dataset publication

**Phase:** P4  
**Dependencies:** P3.4, P4.1  
**Size:** L  
**Primary skill:** Arrow and Parquet  
**Owned scope:** `reflex-dataset`

**Purpose.** Convert immutable event evidence into versioned columnar datasets without making Parquet the hot append path.

**Deliverables**

- Parquet schemas
- partitioning policy
- statistics and manifest
- streaming writer

**Implementation steps**

1. Use Arrow 58.3-compatible arrays and Parquet with zstd for durable decision, attempt, artifact, utility, and lineage tables.
2. Partition by dataset digest and logical shard, not timestamps alone.
3. Write into temporary CAS objects, validate row counts/statistics, then publish a canonical dataset manifest.
4. Keep feature vectors in fixed-size or large-list columns according to schema; avoid JSON features.

**Acceptance criteria**

- Published manifests reproduce exact row counts, schema fingerprints, and shard digests.
- Interrupted compaction publishes no dataset.
- Column projection and predicate pushdown work on representative queries.
- Repeated compaction from the same event inputs yields identical logical dataset identity.

**Performance acceptance**

- Compaction sustains at least 500,000 representative decision rows/s/core.
- Output is within 1.5× of a hand-written Parquet reference size and uses under 1 GiB streaming memory.

### P4.6 — Implement the mmap-friendly training batch cache

**Phase:** P4  
**Dependencies:** P4.5  
**Size:** L  
**Primary skill:** Data pipelines  
**Owned scope:** `reflex-dataset`, `reflex-training`

**Purpose.** Remove Parquet decoding and variable-shape assembly from the inner training loop.

**Deliverables**

- RFXBATCH binary format
- batch compiler
- mmap reader
- double-buffered prefetcher

**Implementation steps**

1. Project selected Parquet columns and pack contiguous state, candidate, mask, target, and group-offset arrays.
2. Use a canonical header with dtype, dimensions, row/group counts, source dataset digest, and CRCs.
3. Memory-map immutable files and provide deterministic shuffled index views without copying features.
4. Prefetch the next batch on a dedicated permitted thread.

**Acceptance criteria**

- The cache exactly reproduces source decision groups.
- Corrupt headers, offsets, masks, and CRCs are rejected.
- Different shuffle seeds produce registered deterministic orders.
- Training can restart at an exact batch position.

**Performance acceptance**

- Sequential batch delivery exceeds 5 GiB/s effective feature bandwidth from page cache.
- Loader CPU is below 10% of one core during micro-model training.
- Peak staging memory is limited to two configured batches.

### P4.7 — Integrate DataFusion for all-Rust analytics

**Phase:** P4  
**Dependencies:** P4.5  
**Size:** M  
**Primary skill:** Rust analytics  
**Owned scope:** `reflex-analytics`

**Purpose.** Provide fast SQL and DataFrame analysis over local Parquet exports while isolating analytical dependency weight from the native runtime.

**Deliverables**

- DataFusion session builder
- dataset catalog
- registered views
- query CLI

**Implementation steps**

1. Put DataFusion in a separate binary/crate so its dependency graph does not leak into hot runtime APIs.
2. Register digest-verified local dataset manifests as logical tables.
3. Provide views for solve rate, matched work, utility, model lineage, retrieval use, and resource accounting.
4. Configure memory limits, local spill directories, and partition count from the thread/resource broker.

**Acceptance criteria**

- The canonical reports can be regenerated using only published datasets and report SQL.
- Queries work against digest-verified local Parquet exports.
- Out-of-memory analytical queries spill or fail with a named limit rather than killing workers.
- SQL files are versioned and their digests appear in reports.

**Performance acceptance**

- The M2A-style 180-arm report reconstructs in under 30 seconds from warm page cache.
- Analytics is absent from the `reflex` native runtime dependency tree.

## P5 — Domain SDK and local runtime

**Goal:** Define the minimal contracts a verifiable domain must implement and run them with strict resource ownership.

**Exit gate:** A typed Rust domain and an external-process domain pass the same conformance suite without domain-specific framework branches.

### P5.1 — Define the typed Rust Domain trait

**Phase:** P5  
**Dependencies:** P3.1, P4.1  
**Size:** L  
**Primary skill:** API design  
**Owned scope:** `reflex-domain`

**Purpose.** Make verifiable worlds pluggable without reducing them to untyped observation/action blobs.

**Deliverables**

- Domain trait
- associated durable codecs
- capability declaration
- error taxonomy

**Implementation steps**

1. Define associated task, state, candidate, transition, artifact, verification, and utility types.
2. Require canonical identity, batched candidate enumeration, batched application, artifact reconstruction, verification, and utility evaluation.
3. Separate deterministic legality from learned scoring.
4. Make candidate ordering and tie-break keys explicit.

**Acceptance criteria**

- A domain cannot return an accepted artifact without verifier evidence.
- All durable associated types declare schema identity and canonical codecs.
- The trait represents OR choices that create zero or more AND child obligations.
- Error classes distinguish invalid candidate, resource exhaustion, unresolved verification, internal invariant, and infrastructure failure.

**Performance acceptance**

- Candidate enumeration can fill caller-owned buffers without allocation.
- Trait methods support batches large enough to amortize virtual dispatch and IPC.

### P5.2 — Implement object-safe erased domain adapters

**Phase:** P5  
**Dependencies:** P5.1  
**Size:** L  
**Primary skill:** Rust trait systems  
**Owned scope:** `reflex-domain`

**Purpose.** Allow runtime-selected domains while preserving typed implementations and avoiding serialization in the native path.

**Deliverables**

- ErasedDomain trait
- typed adapter
- arena-backed payload handles
- downcast-free runtime calls

**Implementation steps**

1. Use stable typed handles into per-episode arenas for task/state/candidate payloads.
2. Erase through adapter-owned methods that operate on slices and output buffers.
3. Keep canonical serialization only at durable or process boundaries.
4. Validate domain capability and schema fingerprints at registration.

**Acceptance criteria**

- Native typed and erased executions produce identical state/candidate IDs and artifacts.
- No serde or Protobuf call occurs in the native expansion/apply hot path.
- Stale handles are detected by arena generation.
- Domains unload only when no episode holds their handles.

**Performance acceptance**

- Erasure overhead is below 3% on the bit-vector expansion benchmark.
- Warm native expansion performs zero heap allocations inside framework code.

### P5.3 — Build task, episode, and cell runtime ownership

**Phase:** P5  
**Dependencies:** P5.2, P1.2, P3.3  
**Size:** L  
**Primary skill:** Runtime systems  
**Owned scope:** `reflex-runtime`

**Purpose.** Give every execution a clear lifetime, resource budget, model pin, knowledge pin, and evidence stream.

**Deliverables**

- CellContext
- EpisodeContext
- cancellation tree
- resource permit integration

**Implementation steps**

1. Construct a cell from an immutable manifest and verify all referenced inputs before execution.
2. Pin the domain, model checkpoint, knowledge edition, search configuration, seed, and resource class for the entire cell.
3. Create episode arenas, scratch buffers, and event streams from bounded pools.
4. Propagate cancellation and deadline from experiment to cell to episode to domain request.

**Acceptance criteria**

- A model or knowledge publication during a cell cannot change that cell.
- Cancellation leaves an explicit censored or infrastructure outcome.
- All permits, child processes, and buffers are released on every exit path.
- A cell result names every pinned input and final evidence segment.

**Performance acceptance**

- Starting an in-process episode after worker warmup takes under 100 microseconds.
- Cell runtime metadata stays under 64 KiB excluding domain scratch.

### P5.4 — Implement the external-domain host and supervisor

**Phase:** P5  
**Dependencies:** P3.6, P5.3  
**Size:** L  
**Primary skill:** Process supervision  
**Owned scope:** `reflex-domain-host`, `reflex-runtime`

**Purpose.** Attach Lean and other mature engines as persistent, batched processes with the same semantic contract as native domains.

**Deliverables**

- process launcher
- handshake supervisor
- worker pool
- restart/quarantine policy

**Implementation steps**

1. Launch one long-lived domain worker per assigned CPU permit or as declared by domain capability.
2. Validate executable digest, environment, protocol, and domain identity before admitting the worker.
3. Route batched requests by worker affinity while preserving episode ownership.
4. Quarantine workers after malformed output or invariant violation; restart only according to manifest policy.

**Acceptance criteria**

- The host never silently falls back to a different executable or schema.
- Worker crash attributes all in-flight requests and leaves replayable evidence.
- Persistent workers are reused across episodes within a cell.
- Shutdown drains evidence and kills the entire process group.

**Performance acceptance**

- Lean-like worker startup cost is amortized over at least 100 episodes in dogfood.
- Supervisor CPU overhead stays below 1% of cell CPU.

### P5.5 — Define verifier authority and isolated verification workers

**Phase:** P5  
**Dependencies:** P5.1, P5.3  
**Size:** L  
**Primary skill:** Verification architecture  
**Owned scope:** `reflex-domain`, `reflex-runtime`

**Purpose.** Keep the untrusted proposer/search path separate from the authority that admits artifacts.

**Deliverables**

- Verifier trait
- VerificationReceipt
- isolated worker mode
- replay command

**Implementation steps**

1. Require receipts to name verifier implementation digest, input artifact digest, semantic anchor, result, axioms/assumptions, resource use, and output proof/certificate digest.
2. Allow in-process verifiers only for exhaustive trusted fixtures; real domains may use process isolation.
3. Support deterministic replay from CAS inputs.
4. Fail closed on timeout, nonzero exit, malformed receipt, or compatibility mismatch.

**Acceptance criteria**

- A forged or edited receipt is rejected by digest and replay.
- Search success without accepted verification is never reported as solved.
- Verifier timeout is unresolved/censored, not false.
- Replay names the first divergent evidence class.

**Performance acceptance**

- Verification orchestration adds under 2% to verifier CPU-intensive workloads.
- Receipt size stays below 4 KiB excluding referenced proof artifacts.

### P5.6 — Create the domain conformance kit and tutorial skeleton

**Phase:** P5  
**Dependencies:** P5.1, P5.5  
**Size:** M  
**Primary skill:** SDK quality  
**Owned scope:** `reflex-domain`, `reflex-cli`

**Purpose.** Give domain implementers executable guidance and detect semantic omissions early.

**Deliverables**

- conformance test crate
- domain template
- capability checklist
- minimal tutorial source

**Implementation steps**

1. Test canonical IDs, deterministic candidate order, buffer sizing, apply semantics, AND child ownership, artifact reconstruction, verifier failure, utility units, and cancellation.
2. Provide fixtures with zero, one, many, and duplicate candidates.
3. Generate a new domain crate with compile-ready trait stubs and tests.
4. Document native versus external-process selection criteria.

**Acceptance criteria**

- A deliberately nondeterministic candidate enumerator fails.
- A domain that reuses a candidate ID for different semantics fails.
- The generated template builds and its pending conformance tests identify each missing implementation.
- The tutorial uses only public APIs.

**Performance acceptance**

- Conformance suite runs in under 10 seconds for a small native domain.
- Template adds no dependency beyond the domain SDK and chosen verifier.

## P6 — Deterministic AND-OR search kernel

**Goal:** Implement fast, replayable, budgeted search with honest censoring and proof-DAG output.

**Exit gate:** Uniform and heuristic policies solve conformance tasks deterministically; replay reproduces every decision and accounting total.

### P6.1 — Implement the AND-OR search state model

**Phase:** P6  
**Dependencies:** P5.3  
**Size:** L  
**Primary skill:** Search algorithms  
**Owned scope:** `reflex-search`

**Purpose.** Represent proof and synthesis searches where a choice may create several obligations and all must succeed.

**Deliverables**

- SearchNode types
- OR candidate edges
- AND obligation groups
- solution propagation

**Implementation steps**

1. Model OR states, candidate applications, AND groups, terminal success, terminal contradiction, and censored exhaustion.
2. Track parent references and outstanding child counts without recursive heap objects.
3. Propagate success only when all AND children succeed; propagate alternative OR success independently.
4. Retain multiple certified solution parents for proof-DAG mining.

**Acceptance criteria**

- Canonical AND-OR fixtures produce expected solved, failed, and censored outcomes.
- A solved child cannot accidentally close its unsolved sibling.
- Multiple solutions remain representable.
- Deep fixtures avoid call-stack recursion.

**Performance acceptance**

- Node storage overhead averages below 96 bytes per live search node excluding domain payload.
- Propagation processes at least 10 million trivial child completions/s/core.

### P6.2 — Implement structure-of-arrays candidate and feature batches

**Phase:** P6  
**Dependencies:** P5.2, P1.4  
**Size:** L  
**Primary skill:** Data-oriented Rust  
**Owned scope:** `reflex-search`, `reflex-ml-core`

**Purpose.** Make candidate enumeration, feature extraction, scoring, and frontier insertion cache-friendly and allocation-free.

**Deliverables**

- CandidateBatch
- FeatureBatch
- aligned storage
- reusable buffer pools

**Implementation steps**

1. Store candidate IDs, action classes, tie-break keys, offsets, legality flags, and features in separate contiguous arrays.
2. Use 64-byte alignment for feature matrices and pad rows only when benchmarks justify it.
3. Support ragged state batches through group offsets.
4. Preallocate from observed capacity classes and return buffers to bounded pools.

**Acceptance criteria**

- Batch slicing preserves candidate/state grouping.
- Feature schema digest includes column order, dtype, normalization, and missing-value rules.
- Warm expansion/scoring allocates zero framework heap objects.
- Capacity overflow returns a named error or grows only outside registered hot benchmarks.

**Performance acceptance**

- Synthetic candidate construction exceeds 5 million candidate metadata records/s/core.
- Feature packing bandwidth exceeds 10 GiB/s for f32 dense features.

### P6.3 — Implement deterministic frontier ordering

**Phase:** P6  
**Dependencies:** P6.1, P6.2  
**Size:** L  
**Primary skill:** Algorithms  
**Owned scope:** `reflex-search`

**Purpose.** Make policy scores guide search without compromising reproducibility or allowing floating-point ties to drift.

**Deliverables**

- FrontierKey
- binary heap or indexed heap
- score quantization policy
- stable tie-break

**Implementation steps**

1. Define ordering by score class, normalized score bits or fixed quantization, logical cost, depth, state ID, candidate ID, and insertion sequence.
2. Reject NaN scores and normalize signed zero.
3. Provide decrease-key only if a benchmark proves it preferable to duplicate-and-stale skipping.
4. Record the exact selected key in decision events.

**Acceptance criteria**

- Identical inputs produce identical selected action across architectures.
- Equal scores respect canonical candidate order.
- NaN and infinite policy output fail the policy attempt without corrupting search.
- Replay reconstructs every pop order.

**Performance acceptance**

- Push/pop throughput exceeds 8 million operations/s/core at 100,000 live entries.
- Frontier memory is bounded and reported per cell.

### P6.4 — Implement transposition, dominance, and reflex caches

**Phase:** P6  
**Dependencies:** P6.1, P2.1  
**Size:** L  
**Primary skill:** Search optimization  
**Owned scope:** `reflex-search`

**Purpose.** Avoid repeated states while preserving exact cache semantics and per-arm isolation.

**Deliverables**

- state table
- dominance records
- reflex cache
- cache snapshot identity

**Implementation steps**

1. Index canonical state IDs in a sharded table with compact visit records.
2. Store best known remaining budget/cost and parent alternatives; never let a lower-budget failure dominate a higher-budget visit.
3. Separate exact reflex/closure results from policy-dependent frontier state.
4. Make input snapshots immutable and cell overlays private.

**Acceptance criteria**

- Cache hits replay to the same verified result.
- Budget exhaustion is never cached as semantic failure.
- Matched arms cannot observe each other’s overlays.
- Cache content and configuration contribute to compatibility identity.

**Performance acceptance**

- Lookup p95 is below 200 ns on a 1-million-state in-memory table under four search threads.
- Sharding scales to at least 3.2× one-thread throughput on reference-4vcpu-8gb.

### P6.5 — Implement logical, CPU, wall, and memory budgets

**Phase:** P6  
**Dependencies:** P6.1, P1.3  
**Size:** M  
**Primary skill:** Resource accounting  
**Owned scope:** `reflex-search`, `reflex-runtime`

**Purpose.** Bound experiments honestly and retain exhaustion as censored evidence.

**Deliverables**

- BudgetSet
- hot counter checks
- process CPU integration
- outcome mapping

**Implementation steps**

1. Support verified actions, successor goals, node count, verifier calls, child process CPU, wall deadline, RSS, and artifact bytes.
2. Check cheap logical counters every action and expensive operating-system counters at a configured cadence.
3. Record which budget fired first and the observed value.
4. Map every budget exhaustion to censored/unresolved rather than negative candidate labels.

**Acceptance criteria**

- An episode exceeding any budget stops and names that budget.
- Logical budgets are deterministic across host speed.
- CPU budget includes descendants.
- Memory pressure cancels before worker OOM when the OS supplies timely metrics.

**Performance acceptance**

- Hot logical budget checks add under 2 ns/action in optimized builds.
- OS sampling overhead remains within P1.3 limits.

### P6.6 — Implement search policies and exploration mixtures

**Phase:** P6  
**Dependencies:** P6.2, P6.3  
**Size:** L  
**Primary skill:** Search and ML integration  
**Owned scope:** `reflex-search`, `reflex-ml-core`

**Purpose.** Support uniform, heuristic, retrieval, learned, oracle, and mixture policies under one measured interface.

**Deliverables**

- Policy trait
- uniform policy
- heuristic policy hooks
- mixture scheduler

**Implementation steps**

1. Score complete candidate batches and return scores plus policy telemetry.
2. Implement uniform with deterministic pseudorandom priorities derived from cell seed and candidate identity.
3. Allow mixtures such as epsilon-uniform, policy disagreement, novelty, and lane portfolios.
4. Charge feature extraction and scoring to the active policy.

**Acceptance criteria**

- Uniform is invariant to candidate array allocation order.
- Policy overhead appears in CPU and utility reports.
- A mixture’s probability or budget allocation reconstructs from events.
- An unavailable learned model fails before the cell starts.

**Performance acceptance**

- Uniform and no-op heuristic score at least 10 million candidates/s/core.
- Batch interface amortizes learned scoring to meet P7 gates.

### P6.7 — Implement certified proof-DAG extraction

**Phase:** P6  
**Dependencies:** P6.1, P5.5  
**Size:** L  
**Primary skill:** Graph algorithms  
**Owned scope:** `reflex-search`, `reflex-dataset`

**Purpose.** Preserve all known viable routes and cost-to-go evidence instead of flattening one found proof into false negatives.

**Deliverables**

- ProofDag schema
- state merge
- viability propagation
- minimum known cost-to-go

**Implementation steps**

1. Merge nodes by canonical state identity after verifier-backed route reconstruction.
2. Mark candidate edges viable only when they participate in a certified complete artifact.
3. Compute minimum known remaining verified actions and CPU along acyclic proof projections; retain cycles as separately handled strongly connected components.
4. Store unknown candidates unchanged.

**Acceptance criteria**

- Two different certified proofs through one state produce two viable actions.
- An untried action never becomes dead.
- Cost-to-go values match hand-worked DAG fixtures.
- Every viable edge links to at least one verifier receipt.

**Performance acceptance**

- Extract 1 million proof edges/s/core for acyclic fixtures.
- Memory scales linearly and can spill sorted edge runs when the DAG exceeds configured RAM.

### P6.8 — Implement deterministic replay and search audit

**Phase:** P6  
**Dependencies:** P6.3, P6.5, P3.4  
**Size:** L  
**Primary skill:** Reproducibility  
**Owned scope:** `reflex-search`, `reflex-report`

**Purpose.** Reconstruct what the policy saw, chose, spent, and proved without trusting summary files.

**Deliverables**

- replay engine
- first-divergence report
- search transcript audit
- reconstruction command

**Implementation steps**

1. Load cell inputs by digest, re-enumerate candidates, re-score or consume recorded scores according to replay mode, and compare decisions.
2. Support logical replay, policy replay, and full verifier replay as separate costs.
3. Name the first differing state, candidate batch, score, frontier key, transition, budget, or receipt.
4. Recompute final solve and resource summaries from evidence.

**Acceptance criteria**

- Accepted cells pass logical replay.
- Edited candidate order or score produces a precise divergence.
- Summary deletion does not prevent reconstruction.
- Replay never mutates knowledge, cache, or model inputs.

**Performance acceptance**

- Logical replay runs at least 2× faster than original verifier-heavy cells when using accepted receipts.
- Memory stays bounded by configured replay window plus search state.

## P7 — All-Rust model runtime

**Goal:** Support low-overhead tiny models and general Burn models with immutable checkpoints and batched inference.

**Exit gate:** The 2,607-parameter reference network matches numerical goldens; the selected backend satisfies the micro-inference and search-overhead budgets.

### P7.1 — Define model roles, specs, and compatibility identity

**Phase:** P7  
**Dependencies:** P2.2, P6.2  
**Size:** L  
**Primary skill:** ML systems design  
**Owned scope:** `reflex-ml-core`

**Purpose.** Make local rankers, taste critics, and proposal models explicit roles with independent schemas, datasets, and promotion rules.

**Deliverables**

- ModelRole enum
- ModelSpec schemas
- parameter counter
- compatibility digest

**Implementation steps**

1. Define ranker input/output, taste vector outputs, and proposal interfaces separately.
2. Include feature schema, action schema, architecture, parameter shapes, dtype, normalization, backend class, and inference semantics in model identity.
3. Reject parameter budgets by exact counted trainable parameters rather than nominal labels.
4. Define canonical f32 CPU inference as the initial scientific reference class.

**Acceptance criteria**

- A checkpoint cannot load against a mismatched feature/action/model schema.
- Parameter counts match independent tensor-shape calculations.
- Taste and proposal checkpoints cannot be passed to a ranker API.
- Equivalent config key ordering yields identical model identity.

**Performance acceptance**

- Compatibility checks complete in under 100 microseconds.
- Model-spec parsing allocates bounded memory proportional to layer count, not parameter count.

### P7.2 — Integrate Burn behind the reflex-ml abstraction

**Phase:** P7  
**Dependencies:** P7.1, P0.3  
**Size:** L  
**Primary skill:** Burn and Rust ML  
**Owned scope:** `reflex-ml-burn`

**Purpose.** Use Burn for general model authoring, autodiff, optimizers, and backend portability without exposing its unstable API throughout Reflex.

**Deliverables**

- Burn backend adapter
- module factory
- autodiff integration
- version pin and compatibility tests

**Implementation steps**

1. Pin Burn 0.21.x exactly in the workspace lockfile and wrap all Burn-facing types inside reflex-ml-burn.
2. Support Flex as the canonical small CPU backend and CubeCL CPU as an accelerated candidate; compile GPU backends only behind optional features.
3. Implement MLP, bottleneck MLP, linear, and small residual ranker factories.
4. Add numerical goldens and backend parity tolerances.

**Acceptance criteria**

- No non-ML crate imports Burn.
- Flex and CubeCL produce outputs within registered tolerance for the reference models.
- Breaking Burn updates require an ADR, parity suite, and backend benchmarks.
- A model can train and infer in one Rust executable.

**Performance acceptance**

- Release worker without GPU features avoids GPU backend dependency weight.
- Backend adapter overhead is below 2% of raw Burn forward time.

### P7.3 — Implement the micro linear and MLP inference engine

**Phase:** P7  
**Dependencies:** P7.1, P1.4  
**Size:** L  
**Primary skill:** SIMD and numerical Rust  
**Owned scope:** `reflex-ml-micro`

**Purpose.** Provide a minimal, allocation-free ceiling for tiny rankers called millions of times inside search.

**Deliverables**

- contiguous weight layout
- linear and two-layer MLP kernels
- activation kernels
- batch scorer

**Implementation steps**

1. Store weights and biases in one aligned immutable allocation with explicit shape metadata.
2. Implement f32 linear, ReLU, GELU approximation only if numerically specified, and scalar output layers.
3. Use safe slices first and inspect generated assembly; add isolated unsafe SIMD only after benchmark and Miri/fuzz evidence.
4. Score candidate-major and state-batched layouts without tensor object construction.

**Acceptance criteria**

- The 2,607-parameter model matches PyTorch-origin numerical goldens within 1e-5 absolute/relative tolerance.
- Warm score_batch performs zero allocations.
- All bounds and shape errors fail before entering kernels.
- Scalar reference, auto-vectorized, and optional SIMD paths agree.

**Performance acceptance**

- A 2,607-parameter MLP scoring 64 candidates has p95 latency at or below 100 microseconds on one calibrated reference vCPU.
- Single-candidate p95 is below 5 microseconds.
- Four independent batches scale at least 3.2× on reference-4vcpu-8gb.

### P7.4 — Build the model backend benchmark and selection tool

**Phase:** P7  
**Dependencies:** P7.2, P7.3, P1.1  
**Size:** M  
**Primary skill:** ML benchmarking  
**Owned scope:** `reflex-bench`, `reflex-ml-core`

**Purpose.** Choose backends from measured Reflex workloads rather than ecosystem reputation.

**Deliverables**

- reflex bench model command
- Burn Flex/Cube/micro comparison
- batch-size sweep
- selection report

**Implementation steps**

1. Benchmark forward, backward, optimizer step, checkpoint load, startup, RSS, and thread scaling for registered model specs.
2. Use batches 1, 8, 32, 64, 128, 512 and the actual M1.5/M2A feature dimensions.
3. Record convergence parity on a fixed dataset, not only kernel throughput.
4. Select a backend per model role/profile and make the selection an input digest.

**Acceptance criteria**

- The tool refuses to compare mismatched numerics or thread budgets.
- Raw samples and host calibration regenerate the recommendation.
- Micro is owned only when it beats Burn by the accepted total-cost margin.
- A backend regression can change the recommendation without code edits.

**Performance acceptance**

- Full CPU backend sweep completes in under 20 minutes on reference-4vcpu-8gb.
- Recommendation computation completes in under 1 second.

### P7.5 — Implement batched ranker inference in search

**Phase:** P7  
**Dependencies:** P7.3, P6.6  
**Size:** L  
**Primary skill:** ML/search integration  
**Owned scope:** `reflex-ml-core`, `reflex-search`

**Purpose.** Make learned scoring a cheap queueing operation over contiguous candidate batches.

**Deliverables**

- Ranker trait
- synchronous native scorer
- optional inference batcher
- score telemetry

**Implementation steps**

1. Accept FeatureBatch and caller-owned output slices.
2. For tiny native models score synchronously on the search thread when benchmarked cheaper; otherwise submit to a bounded inference batcher.
3. Batch across states only when doing so does not violate deterministic decision semantics.
4. Record feature, queue, forward, and postprocess CPU separately.

**Acceptance criteria**

- Scoring cannot reorder candidates or mutate features.
- Backpressure prevents unbounded inference queues.
- Failed scoring yields a named policy failure and permits a registered fallback only if declared in the manifest.
- Scores included in replay are exact f32 bits or specified quantized values.

**Performance acceptance**

- Promoted model scoring plus feature extraction is at most 10% of total search CPU on its qualification workload unless it yields registered net savings.
- Batching adds under 100 microseconds queue delay at p95 for interactive local runs.

### P7.6 — Define the vector-valued taste critic interface

**Phase:** P7  
**Dependencies:** P7.1  
**Size:** M  
**Primary skill:** ML API design  
**Owned scope:** `reflex-ml-core`

**Purpose.** Represent predicted potentiality without collapsing near-term, long-term, option, uncertainty, and cost into one baked reward.

**Deliverables**

- TasteEstimate schema
- head registry
- uncertainty contract
- scheduler adapter

**Implementation steps**

1. Define optional heads for immediate utility, long-horizon utility, descendant value, cross-domain reuse, compression, option value, discovery cost, and epistemic uncertainty.
2. Require units and calibration metadata for each head.
3. Keep scalar scheduling policy outside the model artifact.
4. Version missing-head and censoring behavior.

**Acceptance criteria**

- Raw head outputs are persisted before scheduler scalarization.
- A scheduler cannot treat unavailable heads as zero without explicit policy.
- Changing scalarization does not change model identity or raw historical economics.
- Calibration datasets and metrics are named in the checkpoint.

**Performance acceptance**

- Scoring overhead scales linearly with head count and adds under 5% to a shared-trunk forward for eight scalar heads.

### P7.7 — Define the proposal-model boundary and safety gate

**Phase:** P7  
**Dependencies:** P7.1, P5.5  
**Size:** M  
**Primary skill:** Generative systems  
**Owned scope:** `reflex-ml-core`, `reflex-domain`

**Purpose.** Allow learned candidate generation later without letting a generator bypass domain legality, falsification, taste, or verification.

**Deliverables**

- ProposalBatch schema
- proposal provenance
- domain validation hook
- compute quota interface

**Implementation steps**

1. A proposal model emits typed domain proposal payloads plus scores and generation metadata.
2. The domain validates syntax, bounds, and cheap falsification before proposals enter search.
3. All proposals consume explicit generation and verification budgets.
4. Disable learned proposals by default until P15 gates pass.

**Acceptance criteria**

- No proposal becomes verified knowledge without the ordinary verifier.
- Invalid proposal rates and costs are reported.
- Proposal payload size and count are bounded by capability negotiation.
- A checkpoint cannot activate proposal authority merely by being promoted as a ranker.

**Performance acceptance**

- Rejected proposals cannot cause unbounded allocation or verifier load.
- Batch validation meets domain-specific throughput gates.

### P7.8 — Implement immutable model checkpoint storage

**Phase:** P7  
**Dependencies:** P7.2, P2.3  
**Size:** L  
**Primary skill:** ML persistence  
**Owned scope:** `reflex-ml-core`, `reflex-ml-burn`

**Purpose.** Persist weights, optimizer state, normalization, metrics, and provenance as content-addressed artifacts.

**Deliverables**

- checkpoint manifest
- Burnpack/Burn Store recorder
- micro-weight codec
- load verifier

**Implementation steps**

1. Store model spec, exact weights, optimizer tensors, step/epoch, RNG states, dataset digest, code identity, backend, and metrics.
2. Use Burnpack for native Burn records and a simple canonical contiguous format for micro models; optionally export safetensors for interoperability.
3. Publish artifacts before metadata references.
4. Verify tensor shapes, finite values, parameter count, and digest on load.

**Acceptance criteria**

- Training resumes bit-for-bit where deterministic backend guarantees permit.
- A checkpoint missing optimizer or RNG state is marked inference-only.
- Edited metrics cannot change checkpoint identity independently of the manifest.
- Micro and Burn checkpoint conversions preserve inference outputs.

**Performance acceptance**

- Loading the 3K model takes under 5 ms from page cache.
- Checkpoint write is streamed and uses less than 1.25× model+optimizer bytes of peak scratch.

### P7.9 — Implement stable, candidate, experimental, and shadow model states

**Phase:** P7  
**Dependencies:** P7.8, P4.1  
**Size:** M  
**Primary skill:** Production ML lifecycle  
**Owned scope:** `reflex-ml-core`, `reflex-meta`

**Purpose.** Permit near-online learning without mutating the active model or contaminating registered cells.

**Deliverables**

- model-state records
- shadow evaluation
- atomic active pointer
- rollback command

**Implementation steps**

1. Represent immutable checkpoints separately from roles stable, candidate, experimental, rejected, and retired.
2. Allow candidate/experimental models to score shadow batches without affecting decisions.
3. Pin active model at cell start and swap only between episodes or generations according to mode.
4. Retain one-command rollback to a prior compatible stable checkpoint.

**Acceptance criteria**

- A cell never observes two model checkpoints.
- Shadow scoring cannot affect frontier order or resource budgets except its explicitly charged shadow budget.
- Promotion and rollback are atomic and audited.
- Rejected checkpoints remain inspectable.

**Performance acceptance**

- Atomic model pointer load adds under 10 ns per episode boundary.
- Shadow mode respects a configured CPU percentage and cannot starve active search.

## P8 — Dataset compilation and training

**Goal:** Turn immutable experience into honest multi-route training data and train entirely in Rust.

**Exit gate:** Decision-group datasets preserve viable, dead, and unknown labels; Burn and micro trainers reproduce deterministic metrics and checkpoint digests.

### P8.1 — Define DecisionGroup and label semantics

**Phase:** P8  
**Dependencies:** P6.7, P4.5  
**Size:** L  
**Primary skill:** ML data modeling  
**Owned scope:** `reflex-dataset`, `reflex-types`

**Purpose.** Encode the lesson from M2A: one found proof does not make every unchosen action wrong.

**Deliverables**

- DecisionGroup schema
- candidate status enum
- known cost-to-go fields
- censoring metadata

**Implementation steps**

1. For each state store the complete candidate list and statuses viable, known-dead, unknown, or invalid.
2. Attach minimum known verified actions/CPU for viable candidates and evidence references.
3. Treat bounded episode failure separately from candidate status.
4. Record how the group was mined and its coverage confidence.

**Acceptance criteria**

- An untried candidate remains unknown.
- Multiple certified routes can all be viable.
- Known-dead requires a named oracle/certificate, not absence from found proofs.
- Every training label traces to durable evidence.

**Performance acceptance**

- A typical 64-candidate group stores under 4 KiB excluding feature arrays.
- Group validation processes at least 500,000 candidates/s/core.

### P8.2 — Implement the event-to-decision-group compiler

**Phase:** P8  
**Dependencies:** P8.1, P3.4  
**Size:** L  
**Primary skill:** Data engineering  
**Owned scope:** `reflex-dataset`

**Purpose.** Derive honest training examples deterministically from immutable search and proof-DAG evidence.

**Deliverables**

- streaming compiler
- state/candidate join
- proof-DAG label join
- dataset counters

**Implementation steps**

1. Read ledger segments in sequence, join state and candidate records by durable IDs, and attach verified route evidence from proof DAGs.
2. Deduplicate identical decision observations by content identity while preserving source episodes.
3. Emit censored episode observations even when no positive route is known.
4. Publish counters for coverage, positives, unknowns, dead labels, families, strata, policies, and sources.

**Acceptance criteria**

- Recompiling the same inputs yields the same logical dataset digest.
- Missing referenced evidence fails rather than silently dropping a row.
- No candidate becomes negative merely because another was chosen.
- Counters reconcile to source manifests.

**Performance acceptance**

- Compiler sustains at least 500,000 candidate rows/s/core.
- Streaming memory remains below 2 GiB on 100-million-row datasets.

### P8.3 — Implement multi-positive pairwise and listwise objectives

**Phase:** P8  
**Dependencies:** P8.1, P7.2  
**Size:** L  
**Primary skill:** ML losses  
**Owned scope:** `reflex-training`

**Purpose.** Train ranking from known relationships while giving unknown candidates no false-negative gradient.

**Deliverables**

- masked pairwise loss
- masked listwise loss
- cost-to-go target transform
- loss tests

**Implementation steps**

1. For pairwise loss compare viable candidates by known cost-to-go and viable versus known-dead candidates; mask unknowns.
2. For listwise loss form a target distribution over viable candidates using temperature-scaled negative cost-to-go; optionally assign known-dead floor mass.
3. Normalize per decision group to prevent large candidate sets from dominating.
4. Report groups with insufficient supervision rather than fabricating labels.

**Acceptance criteria**

- Autograd gradients match finite-difference checks on tiny fixtures.
- Unknown candidates have exactly zero target-driven gradient.
- Multiple equal-cost viable candidates receive equal target weight.
- Loss is finite for empty dead sets, one viable candidate, and extreme cost ranges.

**Performance acceptance**

- Loss computation processes at least 1 million candidates/s/core for 64-candidate groups on Flex.
- Temporary tensor memory is bounded by batch candidate count, not global action space.

### P8.4 — Implement censored sampling and frontier coverage policies

**Phase:** P8  
**Dependencies:** P8.2  
**Size:** M  
**Primary skill:** Dataset sampling  
**Owned scope:** `reflex-training`

**Purpose.** Prevent the active policy from training only on the region it already knows how to explore.

**Deliverables**

- sampler registry
- difficulty/family strata
- policy-disagreement sampler
- failed-frontier sampler

**Implementation steps**

1. Support uniform problems, balanced family/stratum, decision-group supervision density, policy disagreement, rare state shapes, and censored frontier sampling.
2. Cap source-policy contribution and duplicate state frequency.
3. Record sampling probabilities for importance correction where used.
4. Keep evaluation corpora immutable and excluded from all samplers.

**Acceptance criteria**

- The k-NN-success-only ablation can be reproduced as a deliberately narrow sampler.
- Sampling manifests reproduce exact selected group IDs.
- Rare families receive configured minimum coverage.
- No evaluation ID appears in training shards.

**Performance acceptance**

- Generate a 10-million-group sample manifest in under 30 seconds.
- Sampling uses streaming/reservoir methods when indexes exceed RAM.

### P8.5 — Build deterministic training batch assembly

**Phase:** P8  
**Dependencies:** P4.6, P8.3  
**Size:** L  
**Primary skill:** High-performance training input  
**Owned scope:** `reflex-training`

**Purpose.** Feed Burn and micro trainers contiguous batches with deterministic grouping and no hot-path allocation.

**Deliverables**

- BatchPlan
- group-aware packer
- shuffle/restart state
- prefetch pipeline

**Implementation steps**

1. Bucket groups by candidate count where beneficial but preserve declared sampling weights.
2. Pack features, masks, targets, weights, and offsets into preallocated tensors/slices.
3. Use counter-based RNG or persisted permutation state.
4. Double-buffer I/O and compute through the thread-budget broker.

**Acceptance criteria**

- Same seed and dataset produce the same batches and checkpoint position.
- Group boundaries and masks survive padding.
- Restart resumes at the exact next batch.
- No evaluation leakage occurs through shared cache manifests.

**Performance acceptance**

- Batch assembly is below 10% of training step CPU.
- Warm assembly performs zero per-candidate heap allocation.

### P8.6 — Implement the Burn custom training loop

**Phase:** P8  
**Dependencies:** P7.2, P7.8, P8.5  
**Size:** L  
**Primary skill:** Rust ML training  
**Owned scope:** `reflex-training`, `reflex-ml-burn`

**Purpose.** Train rankers and critics in Rust with exact accounting, restartable checkpoints, and promotion-ready metrics.

**Deliverables**

- trainer state machine
- optimizer configuration
- metric reducers
- checkpoint cadence

**Implementation steps**

1. Use a custom loop rather than a hidden high-level learner so batch identity, CPU accounting, censoring, and checkpoints are explicit.
2. Support AdamW and SGD initially with gradient clipping, weight decay, and learning-rate schedules.
3. Emit train/dev loss, ranking metrics, calibration, throughput, CPU, RSS, and backend telemetry.
4. Checkpoint at deterministic step boundaries and on graceful cancellation.

**Acceptance criteria**

- A tiny synthetic problem converges to its known optimum.
- Resume produces matching subsequent metrics within backend determinism policy.
- Nonfinite loss or gradient fails the run and preserves prior checkpoint.
- Optimizer state is included in checkpoint identity.

**Performance acceptance**

- Train one epoch over 1 million 64-candidate decision rows with a 3K MLP in at most 60 seconds on reference-4vcpu-8gb.
- Peak RSS stays below 2 GiB for that benchmark.
- All four CPUs are used without oversubscription.

### P8.7 — Implement the specialized micro-model trainer

**Phase:** P8  
**Dependencies:** P7.3, P8.5  
**Size:** L  
**Primary skill:** Numerical optimization  
**Owned scope:** `reflex-ml-micro`, `reflex-training`

**Purpose.** Provide a transparent and fast reference trainer for linear and two-layer MLP rankers without building general autograd.

**Deliverables**

- manual forward/backward
- AdamW state
- gradient tests
- Burn parity harness

**Implementation steps**

1. Implement batched matrix/vector derivatives for the supported micro architectures.
2. Use f32 accumulation initially and explicit deterministic reduction order for canonical mode.
3. Compare every update against a Burn reference on small random batches.
4. Share the same checkpoint and metric schemas as Burn trainers.

**Acceptance criteria**

- Gradient and parameter updates match Burn within registered tolerance for 1,000 seeded steps.
- Unsupported layers fail at config validation.
- Resume preserves optimizer moments and RNG.
- Micro checkpoints convert to the inference format without numeric change.

**Performance acceptance**

- Micro training beats or matches Burn Flex total epoch time by the accepted 15% ownership threshold; otherwise it remains a benchmark-only implementation.
- Training allocates only at batch and checkpoint boundaries.

### P8.8 — Implement offline ranking and search-proxy evaluation

**Phase:** P8  
**Dependencies:** P8.2, P7.5  
**Size:** M  
**Primary skill:** ML evaluation  
**Owned scope:** `reflex-eval`

**Purpose.** Catch label, representation, and overfitting failures before spending distributed verifier compute.

**Deliverables**

- top-k viable metrics
- MRR and NDCG
- cost-to-go ordering metrics
- score calibration and entropy report

**Implementation steps**

1. Evaluate top-1 viable rate, top-k viable recall, reciprocal rank of cheapest known route, pairwise viable ordering, family/stratum breakdowns, margins, entropy, and candidate diversity.
2. Compare train, development, and held-out construction mechanisms.
3. Add a proof-DAG oracle score ceiling.
4. Persist per-state error slices for feature-collision analysis.

**Acceptance criteria**

- Metrics reproduce from checkpoint and dataset artifacts.
- The report detects a model with high training accuracy and poor held-out viable recall.
- Unknown candidates are excluded from semantic accuracy denominators.
- Oracle ceiling distinguishes proposal coverage from model failure.

**Performance acceptance**

- Evaluate 10 million candidates/s/core with the micro scorer where features are memory-resident.
- Report generation streams per-state slices to Parquet.

### P8.9 — Implement capacity, architecture, loss, and seed sweeps

**Phase:** P8  
**Dependencies:** P8.6, P8.8  
**Size:** L  
**Primary skill:** Experiment orchestration  
**Owned scope:** `reflex-training`, `reflex-scheduler`

**Purpose.** Make model-size and supervision questions ordinary framework operations.

**Deliverables**

- SweepSpec
- exact parameter-budget search
- lineage runner
- selection report

**Implementation steps**

1. Generate exact-size or bounded-size model configs for linear, bottleneck MLP, and MLP families.
2. Cross product model, loss, dataset variant, seed/lineage, and backend into immutable training cells.
3. Apply preregistered selection rules without peeking at evaluation.
4. Reuse content-addressed datasets and initialization artifacts.

**Acceptance criteria**

- The M1.5 519/1,026/2,607/9,614/29,538/99,902 parameter points can be expressed exactly.
- A sweep can compare single-route versus proof-DAG supervision.
- Selection consumes development results only.
- All cells and exclusions reconcile in the final report.

**Performance acceptance**

- Scheduler overhead stays under 1% of total sweep CPU.
- Duplicate configs are deduplicated by identity before execution.

## P9 — Autonomous collect–train–evaluate–promote loop

**Goal:** Make self-improvement a restartable, gated state machine rather than an ad hoc script.

**Exit gate:** A complete generation survives process death, promotes only a qualifying checkpoint, and atomically pins the next generation.

### P9.1 — Define the autonomous generation state machine

**Phase:** P9  
**Dependencies:** P8.9, P4.1  
**Size:** L  
**Primary skill:** Workflow design  
**Owned scope:** `reflex-scheduler`

**Purpose.** Turn self-training into explicit durable states with no invisible background work.

**Deliverables**

- GenerationState enum
- transition rules
- durable commands
- state reconstruction

**Implementation steps**

1. Define bootstrap, collecting, verifying, compiling, training, evaluating, promotion-pending, promoted, rejected, stopped, and failed.
2. Permit transitions only through metadata compare-and-set with required artifact references.
3. Make each state restartable and idempotent.
4. Store the cause and operator identity for manual transitions.

**Acceptance criteria**

- Illegal transitions fail.
- Coordinator restart reconstructs exact generation state.
- No promoted state exists without accepted evaluation and checkpoint artifacts.
- Failed generations remain inspectable and cannot be mistaken for incomplete.

**Performance acceptance**

- State transition transaction p95 is below 20 ms distributed and 2 ms local.
- Reconstruction handles 100,000 generations in bounded memory.

### P9.2 — Implement immutable experiment and cell manifests

**Phase:** P9  
**Dependencies:** P9.1, P2.2  
**Size:** L  
**Primary skill:** Experiment design  
**Owned scope:** `reflex-scheduler`, `reflex-types`

**Purpose.** Pin every input that can affect scientific or economic outcomes.

**Deliverables**

- ExperimentManifest
- CellManifest
- compatibility digest
- manifest diff

**Implementation steps**

1. Include code/image, domain, toolchain, corpus, split, feature/action schemas, policy/model, knowledge/cache snapshots, search budgets, seeds, resource class, verifier, utility evaluators, and analysis plan.
2. Separate descriptive labels from identity-bearing fields.
3. Compute cell IDs from complete canonical manifests.
4. Provide human-readable diff explaining identity changes.

**Acceptance criteria**

- A changed budget, model, knowledge edition, verifier, or utility evaluator changes identity.
- Labels and comments do not.
- Worker verifies every referenced digest before start.
- Manifests reject unresolved mutable tags such as latest.

**Performance acceptance**

- Manifest validation completes under 5 ms for typical cells.
- Diff handles 10,000-cell matrices in under 2 seconds.

### P9.3 — Implement generation collection and dataset triggers

**Phase:** P9  
**Dependencies:** P9.1, P9.2  
**Size:** M  
**Primary skill:** Workflow orchestration  
**Owned scope:** `reflex-scheduler`

**Purpose.** Launch enough diverse experience to improve models without letting the current policy monopolize data.

**Deliverables**

- collection plan
- policy mixture allocation
- completion gate
- dataset trigger

**Implementation steps**

1. Allocate cells among stable policy, uniform, heuristics, disagreement, failure mining, and experimental lanes.
2. Declare minimum experience counts and coverage strata before dataset compilation.
3. Wait for accepted attempts or explicitly registered exclusions.
4. Publish the exact source-cell set into the dataset manifest.

**Acceptance criteria**

- Collection cannot mutate its policy mixture after observing evaluation.
- Missing required strata block training.
- Retries do not double-count episodes.
- The source set reconstructs from metadata and cell evidence.

**Performance acceptance**

- Coordinator handles 100,000 cells with under 1 GiB RSS.
- Plan expansion is under 5 seconds for 1 million logical episodes.

### P9.4 — Implement declarative evaluation and promotion policy

**Phase:** P9  
**Dependencies:** P8.8, P9.1  
**Size:** L  
**Primary skill:** Model governance  
**Owned scope:** `reflex-scheduler`, `reflex-eval`

**Purpose.** Promote models only when they improve frozen goals after inference and feature overhead.

**Deliverables**

- PromotionPolicy schema
- metric expressions
- family/stratum regression rules
- promotion receipt

**Implementation steps**

1. Support solve-rate lift, equivalence margins, verified-action compression, CPU ratios, inference overhead, lineage qualification, calibration, and severe regression vetoes.
2. Compare candidate against stable and cheap baselines using matched cells.
3. Require four-of-five or another explicit lineage rule where registered.
4. Publish a signed/content-addressed receipt naming all inputs and decisions.

**Acceptance criteria**

- Training loss alone cannot promote.
- A cheaper model that fails earlier does not qualify as compression.
- Missing metrics or insufficient equivalent solves fail closed.
- A promotion is reproducible from accepted reports.

**Performance acceptance**

- Policy evaluation over 100,000 metric rows completes in under 100 ms.
- Promotion adds no search hot-path code.

### P9.5 — Implement crash-safe resume, retry, and cancellation

**Phase:** P9  
**Dependencies:** P9.1, P4.4  
**Size:** L  
**Primary skill:** Reliability  
**Owned scope:** `reflex-engine`, `reflex`

**Purpose.** Resume from the last atomic local barrier without ambiguous scientific state.

**Deliverables**

- resume command
- attempt policy
- cancellation semantics
- orphan reconciliation

**Implementation steps**

1. Reopen and verify CURRENT, rebuild its root set in the arena, and discard volatile post-barrier work.
2. Retry infrastructure failures under the manifest’s bounded policy; never retry scientific failures as if they were infrastructure.
3. Cancellation stops new claims, drains or kills active cells according to mode, and publishes partial evidence.
4. Use fencing tokens for all finalization.

**Acceptance criteria**

- Killing coordinator, worker, database connection, and network at injected points yields one accepted or explicit incomplete outcome.
- Stale attempts cannot overwrite accepted evidence.
- Resume is idempotent.
- Cancellation leaves no active unowned Machines.

**Performance acceptance**

- Reconciliation of 10,000 attempts completes under 10 seconds.
- Worker cancellation p95 is under 5 seconds excluding uninterruptible verifier behavior.

### P9.6 — Implement stopping conditions and compute economics

**Phase:** P9  
**Dependencies:** P9.3, P9.4  
**Size:** M  
**Primary skill:** Resource economics  
**Owned scope:** `reflex-scheduler`, `reflex-economics`

**Purpose.** Stop autonomous training at an explicit capability, cost, or convergence boundary.

**Deliverables**

- StopPolicy
- budget ledger
- marginal return calculation
- stop receipt

**Implementation steps**

1. Support CPU-hours, Machine-hours, wall deadline, dollar planning proxy, generations, target capability, target utility, no-promotion patience, and marginal utility per CPU.
2. Use posted accepted evidence through the current generation; do not forecast unrun work as realized.
3. Reserve cleanup and validation budget.
4. Make operator stop an explicit reason.

**Acceptance criteria**

- A run cannot exceed hard compute limits by more than one already-leased bounded cell.
- Stop decisions reconstruct from immutable economics.
- No-improvement patience ignores failed infrastructure generations.
- Planning cost and billing authority are labeled separately.

**Performance acceptance**

- Budget checks add negligible coordinator load.
- Economics aggregation handles 100 million utility rows through Parquet/DataFusion.

## P10 — Verified bit-vector vertical slice

**Goal:** Prove the framework abstraction with a small complete domain before attaching mature systems.

**Exit gate:** One command performs two generations and the promoted ranker improves a frozen metric without weakening exhaustive equivalence.

### P10.1 — Implement the bounded bit-vector expression language

**Phase:** P10  
**Dependencies:** P5.6  
**Size:** M  
**Primary skill:** Compiler frontends  
**Owned scope:** `domains/reflex-domain-bitvec`

**Purpose.** Provide a complete, cheap, exhaustively verifiable domain that exercises the real framework contracts.

**Deliverables**

- typed AST
- canonical codec
- interpreter
- task generator

**Implementation steps**

1. Support u8 inputs, constants, add, sub, xor, and, or, shifts with fixed semantics, select, and a bounded expression depth.
2. Define exact wrapping and shift behavior.
3. Generate source expressions and optimization tasks with deterministic seeds.
4. Reject ill-typed and over-capacity ASTs.

**Acceptance criteria**

- Interpreter matches exhaustive truth-table fixtures.
- Canonical IDs are alpha/name independent where semantics require.
- Generator has zero duplicate source semantics in the frozen corpus after canonicalization.
- Every task fits the declared exhaustive verifier budget.

**Performance acceptance**

- Interpret at least 100 million primitive ops/s/core.
- Generate 100,000 tasks in under 5 seconds.

### P10.2 — Implement candidate rewrites and exhaustive verification

**Phase:** P10  
**Dependencies:** P10.1, P5.5  
**Size:** L  
**Primary skill:** Program synthesis  
**Owned scope:** `domains/reflex-domain-bitvec`

**Purpose.** Exercise proposal, application, artifact, verification, and utility semantics with absolute correctness.

**Deliverables**

- rewrite grammar
- candidate enumerator
- exhaustive verifier
- cost model

**Implementation steps**

1. Enumerate local algebraic rewrites, subtree replacement, constant folding, reassociation, and bounded expression synthesis.
2. Apply candidates to immutable ASTs and canonicalize results.
3. Verify equivalence over every u8 input assignment within task arity.
4. Score weighted operation count, depth, and optional measured runtime as separate utility dimensions.

**Acceptance criteria**

- Every accepted candidate is exhaustively equivalent.
- A known incorrect rewrite is rejected with a counterexample.
- Candidate enumeration is complete for the declared grammar and deterministic.
- Utility never influences correctness.

**Performance acceptance**

- Verify at least 50,000 small candidate pairs/s/core.
- Candidate enumeration and apply meet P6 allocation budgets.

### P10.3 — Add bit-vector features and cheap baselines

**Phase:** P10  
**Dependencies:** P10.2, P6.6  
**Size:** M  
**Primary skill:** Feature engineering  
**Owned scope:** `domains/reflex-domain-bitvec`

**Purpose.** Provide a real learning problem with transparent structural features and strong non-neural controls.

**Deliverables**

- feature schema
- uniform policy
- cost heuristic
- oracle policy

**Implementation steps**

1. Encode operator histograms, subtree sizes, depth, candidate edit class, local cost delta, constant flags, and shared structure.
2. Keep expression/candidate identity out of features.
3. Implement cost-first and simplification-first heuristics.
4. Implement an offline oracle using exhaustive candidate utility for diagnostics only.

**Acceptance criteria**

- Feature goldens are stable.
- Identity permutation does not alter features.
- Oracle is never available to training or confirmatory evaluation.
- Baselines use the same search and accounting path as learned policies.

**Performance acceptance**

- Feature extraction exceeds 5 million candidates/s/core.
- Heuristic scoring exceeds 10 million candidates/s/core.

### P10.4 — Run the first collect and proof-DAG dataset generation

**Phase:** P10  
**Dependencies:** P10.3, P9.3  
**Size:** L  
**Primary skill:** Experiment execution  
**Owned scope:** `domains/reflex-domain-bitvec`, `reflex-scheduler`

**Purpose.** Produce the first end-to-end immutable experience and honest decision-group dataset.

**Deliverables**

- frozen corpus
- uniform/heuristic cells
- verified artifacts
- decision-group dataset

**Implementation steps**

1. Freeze train/dev/eval tasks before collection.
2. Run uniform, heuristic, and randomized lineages with exact verification.
3. Mine multiple optimization routes and cost-to-go.
4. Publish ledger, Parquet, RFXBATCH, and dataset reports.

**Acceptance criteria**

- Every accepted optimization replays.
- Train/eval overlap checks pass.
- Dataset has multiple-positive groups and unknown candidates.
- All cell and row counts reconcile.

**Performance acceptance**

- The entire collection completes in under 15 minutes on one reference-4vcpu-8gb.
- Accepted evidence stays under 2 GiB.

### P10.5 — Train and qualify the first all-Rust ranker

**Phase:** P10  
**Dependencies:** P10.4, P8.7  
**Size:** L  
**Primary skill:** Rust ML experimentation  
**Owned scope:** `domains/reflex-domain-bitvec`, `reflex-training`

**Purpose.** Demonstrate that Rust-native training produces a model that improves framework-owned verified search.

**Deliverables**

- capacity sweep
- selected checkpoint
- offline report
- search evaluation

**Implementation steps**

1. Train linear and small MLP models using pairwise and listwise objectives.
2. Select on development only.
3. Run frozen evaluation against uniform and heuristic baselines at matched budgets.
4. Account for feature and inference CPU.

**Acceptance criteria**

- At least one learned model improves solve/optimization success or compresses verified work under a preregistered gate.
- If no model qualifies, the negative result is complete and blocks P9 promotion semantics acceptance.
- Checkpoint and report reconstruct exactly.
- Selected model stays within its parameter budget.

**Performance acceptance**

- Training and evaluation satisfy P7/P8 throughput budgets.
- Inference overhead remains below 10% of search CPU.

### P10.6 — Complete the second autonomous generation and tutorial

**Phase:** P10  
**Dependencies:** P10.5, P9.6  
**Size:** L  
**Primary skill:** Developer experience  
**Owned scope:** `domains/reflex-domain-bitvec`, `docs/tutorials`

**Purpose.** Prove the autonomous loop and provide a reference implementation a new domain author can follow.

**Deliverables**

- two-generation run
- promotion receipt
- tutorial
- performance report

**Implementation steps**

1. Use the promoted model to collect the second generation while retaining exploration lanes.
2. Train/evaluate the next candidate and stop according to the tutorial policy.
3. Document every public type, command, artifact, and failure mode used.
4. Include expected outputs and evidence digests from a frozen tutorial run.

**Acceptance criteria**

- One command reproduces the tutorial from a clean checkout.
- The second generation uses the pinned promoted checkpoint and knowledge edition.
- The tutorial explains how unknown/censored labels are handled.
- All P0–P10 blocking invariants are covered.

**Performance acceptance**

- Tutorial run completes under 30 minutes on reference-4vcpu-8gb.
- Local setup requires no service or Python installation.


## P12 — Wrela dogfood integration

**Goal:** Complete the first economically grounded real-domain loop from semantic anchor to verified catalog entry.

**Exit gate:** Reflex reduces certified Wrela work or a locked workload cost with identical declared semantics and full overhead accounting.

### P12.1 — Define the Wrela Kernel Package v1

**Phase:** P12
**Dependencies:** P5.6, P2.5
**Size:** L
**Primary skill:** Compiler interfaces
**Owned scope:** `domains/reflex-domain-wrela`, `docs/wrela`

**Purpose.** Create a narrow, content-addressed semantic package for pure bounded Wrela kernels rather than ingesting arbitrary source.

**Deliverables**

- kernel package schema
- semantic IR
- range/precondition records
- package validator

**Implementation steps**

1. Support finite integer/fixed/dyadic/interval values, small tuples, bounded arrays, let/if, and statically bounded loops required by first kernels.
2. Record exact overflow, error, and Result behavior as observable semantics.
3. Include Wrela/compiler commit, source, semantic AST, input ranges, reference adapters, theorem/certificate dependencies, fixtures, target cost table, and workload traces.
4. Reject allocation, actors, concurrency, arbitrary pointers, unbounded loops, and unsupported effects in v1.

**Acceptance criteria**

- Package validates independently of a live Wrela checkout.
- Any semantic, range, cost, or verifier change changes identity.
- Unsupported constructs fail with source-mapped diagnostics.
- Reference package fixtures round-trip through export/import.

**Performance acceptance**

- Validate a 100,000-node kernel package in under 500 ms.
- Package metadata stays compact; large fixtures/traces are CAS references.

### P12.2 — Implement Wrela export, candidate check, and cost commands

**Phase:** P12  
**Dependencies:** P12.1  
**Size:** L  
**Primary skill:** Wrela compiler integration  
**Owned scope:** `wrela8 xtask seam`, `domains/reflex-domain-wrela`

**Purpose.** Give Reflex a stable command surface without coupling its core to Wrela internals.

**Deliverables**

- wrela reflex-export
- wrela reflex-check-candidate
- wrela reflex-cost-candidate
- wrela reflex-conformance

**Implementation steps**

1. Export packages after Wrela sealing and before guest reachability finalization.
2. Accept candidate package plus claimed theorem/precondition and rebuild through pinned Wrela code.
3. Run Rust/Wrela scalar/packet differential fixtures, instruction obligations, and declared cost model.
4. Return a signed/content-addressed receipt with exact inputs and outputs.

**Acceptance criteria**

- No command mutates source or catalog by default.
- Candidate check fails on semantic or failure-behavior divergence.
- Cost reports include feature/inference/compiler overhead when evaluating a Reflex policy.
- Commands are deterministic under pinned inputs.

**Performance acceptance**

- Warm candidate check startup is under 250 ms excluding compilation/verifier work.
- Batch mode handles at least 100 candidates per process invocation.

### P12.3 — Implement the Wrela external domain adapter

**Phase:** P12  
**Dependencies:** P12.2, P5.4  
**Size:** L  
**Primary skill:** Domain integration  
**Owned scope:** `domains/reflex-domain-wrela`

**Purpose.** Map Wrela optimization states and legal transformations into the generic Domain contract.

**Deliverables**

- task/state/candidate schemas
- external worker
- feature extraction
- artifact reconstruction

**Implementation steps**

1. Represent semantic anchor, current candidate, unresolved obligations, target profile, and measured downstream context.
2. Enumerate registered transformations or certificate strategies in deterministic order.
3. Batch apply candidates through persistent Wrela worker processes.
4. Build artifacts only from candidates accepted by the Wrela checker.

**Acceptance criteria**

- Adapter passes domain conformance.
- A candidate ID is independent of array position and source filename.
- Worker restart/replay yields identical states and artifacts.
- No Wrela-specific branch appears in reflex-search/training.

**Performance acceptance**

- Transport/framework overhead is under 5% of total Wrela evaluation wall time.
- Feature extraction meets candidate throughput targets for first campaign.

### P12.4 — Build the certified strategy-scheduling domain

**Phase:** P12  
**Dependencies:** P12.3, P6.6  
**Size:** L  
**Primary skill:** Numerical verification  
**Owned scope:** `domains/reflex-domain-wrela`

**Purpose.** Start with the safest transfer of the existing Reflex skill: rank already-sound certificate and refinement methods.

**Deliverables**

- certificate state schema
- action inventory
- strategy executor
- strategy fixtures

**Implementation steps**

1. Expose interval, Bernstein, Krawczyk, subdivision, tightening, exact fallback, and unresolved actions where supported.
2. Encode degree, coefficient widths, interval widths, derivative signs, box geometry, depth, prior failures, and budget features.
3. Keep dyadic verifier authority unchanged.
4. Collect cases from locked scenes, cuts/whips, parameter sweeps, and adversarial near-degeneracies.

**Acceptance criteria**

- All strategy orders produce identical authority decisions under sufficient budgets.
- Unsupported strategies are not emitted.
- No learned score can bypass a verifier.
- Held-out split groups neighboring scene/camera cases to prevent leakage.

**Performance acceptance**

- State/action feature extraction is under 10% of certificate work.
- Executor supports at least 100,000 strategy attempts/s where verifier kernels are cheap.

### P12.5 — Implement Wrela equivalence, refinement, and economics receipts

**Phase:** P12  
**Dependencies:** P12.2, P14.5  
**Size:** L  
**Primary skill:** Formal/compiler validation  
**Owned scope:** `domains/reflex-domain-wrela`, `reflex-economics`

**Purpose.** Distinguish exact equivalence, sound strengthening, and economic value with complete evidence.

**Deliverables**

- WrelaVerificationReceipt
- exact-equivalence class
- sound-strengthening class
- utility adapters

**Implementation steps**

1. Exact class requires identical output and error behavior over the declared domain.
2. Sound-strengthening requires a theorem that the candidate never accepts an incorrect result and may certify additional cases.
3. Record emitted words, proxy cycles, frame bytes, code bytes, compile cost, workload frequencies, displayed digests, and physical timing calibration separately.
4. Subtract policy/retrieval/feature overhead from net utility.

**Acceptance criteria**

- A locally cheaper candidate that worsens locked workload is not promoted as an economic win.
- A candidate failing more frames is rejected unless semantics explicitly permit and the claim says so.
- Modelled and measured timing are clearly labeled.
- Receipts replay from package and Wrela commit.

**Performance acceptance**

- Receipt generation adds under 2% to the underlying conformance/cost runs.
- Utility aggregation is streaming and bounded.

### P12.6 — Collect, train, and evaluate the Wrela strategy ranker

**Phase:** P12  
**Dependencies:** P12.4, P8.9
**Size:** XL  
**Primary skill:** ML experiment execution  
**Owned scope:** `domains/reflex-domain-wrela`, `reflex-training`

**Purpose.** Demonstrate fresh-domain self-generated training and measure whether local ranking transfers architecturally.

**Deliverables**

- training corpus
- decision-group dataset
- capacity/loss sweep
- frozen search evaluation

**Implementation steps**

1. Bootstrap with current hard-coded order, cost-first, random order, and high-budget portfolio search.
2. Mine successful strategy routes and unknown alternatives.
3. Train fresh micro/Burn rankers; optionally test M1.5 initialization as a transfer arm but not the primary path.
4. Evaluate held-out scenes with all overhead and exact authority parity.

**Acceptance criteria**

- A positive claim requires no additional errors and registered certificate or whole-workload savings.
- A negative result remains complete and attributed with oracle diagnostics.
- Fresh and transferred initialization are reported separately.
- The stable Wrela model is independently promoted from Lean models.

**Performance acceptance**

- Target gate: at least 15% certificate-work reduction or 3% whole-workload reduction with inference included.
- Distributed matrix uses one cell per reference-4vcpu-8gb worker and stays within 8 GiB.

### P12.7 — Implement immutable Wrela optimization catalogs

**Phase:** P12  
**Dependencies:** P12.5, P14.1  
**Size:** L  
**Primary skill:** Compiler deployment  
**Owned scope:** `domains/reflex-domain-wrela`, `wrela8 catalog seam`

**Purpose.** Separate slow discovery from deterministic application through pinned catalog editions.

**Deliverables**

- catalog entry schema
- edition builder
- lockfile
- application telemetry

**Implementation steps**

1. Store source/replacement pattern, preconditions, proof/certificate, target cost deltas, workload hit counts, dependencies, and discovery provenance.
2. Build content-addressed editions that pass Wrela conformance and cost-corpus gates.
3. Pin edition digest per build; no learning or discovery occurs during compilation.
4. Record which entries fired and measured savings for offline feedback.

**Acceptance criteria**

- Same compiler input plus edition yields identical application decisions.
- Entries cannot be edited inside an edition.
- A bad edition rolls back by lockfile only.
- Telemetry never changes current build decisions.

**Performance acceptance**

- Lookup/application meets Wrela compile-time overhead budget.
- Edition load is mmap-friendly and bounded by active entry count.

### P12.8 — Run the clipped-coverage and fixed-Q discovery campaigns

**Phase:** P12  
**Dependencies:** P12.6, P12.7  
**Size:** XL  
**Primary skill:** Algorithm discovery  
**Owned scope:** `domains/reflex-domain-wrela`, `evidence/wrela-campaigns`

**Purpose.** Move from strategy scheduling to real verified candidate algorithms in high-leverage Pixels kernels.

**Deliverables**

- coverage campaign
- fixed-Q campaign
- baselines and oracle studies
- catalog candidate report

**Implementation steps**

1. For coverage compare fixed 256-piece enclosure, adaptive dyadic subdivision, root-isolation plus exact antiderivative, and hybrid selectors.
2. For fixed-Q search domain exponent, reset width, centering, recurrence/reset form, and packet grouping under overflow/error proofs.
3. Use deterministic enumeration/mutation first; learned proposal remains disabled.
4. Evaluate conservative bounds, output singleton rate, downstream refinement, locked displayed bytes, cost sweeps, and physical calibration.

**Acceptance criteria**

- Every accepted algorithm is formally or conservatively certified and conformance-clean.
- Campaign report distinguishes rediscovery, Wrela-new result, and plausible novelty.
- Hindsight oracle identifies whether candidate space or taste/ranking limited outcomes.
- No catalog admission occurs on microbenchmark evidence alone.

**Performance acceptance**

- Coverage success gate is 20% coverage-work or 8% locked-renderer cost reduction; fixed-Q gate is campaign-preregistered.
- Campaign overhead and compute are fully reported.

## P13 — Lean adapter and compositional-learning follow-up

**Goal:** Preserve the existing scientific evidence and use M2A to validate proof-DAG supervision and model flexibility.

**Exit gate:** M1.5 and M2A reconstruct under Reflex; M2B attributes whether supervision, representation, or ranking value caused the negative result.

### P13.1 — Implement the Project Reflex Lean external adapter

**Phase:** P13  
**Dependencies:** P5.4, P3.5  
**Size:** L  
**Primary skill:** Lean integration  
**Owned scope:** `domains/reflex-domain-lean`

**Purpose.** Attach the mature Lean engine without rewriting its trusted search/action semantics inside the new framework.

**Deliverables**

- Protocol V2 bridge
- domain capability mapping
- persistent worker pool
- artifact importer

**Implementation steps**

1. Adapt task opening, candidate enumeration/application, proof replay, cache snapshot, and resource events to the Reflex protocol.
2. Preserve Project Reflex fragment/action/feature digests as compatibility inputs.
3. Use the existing Lean kernel replay as authority.
4. Keep historical Project Reflex repositories and artifacts unchanged.

**Acceptance criteria**

- A known Project Reflex problem produces identical candidate identities and proof result through the adapter.
- Cache and knowledge inputs remain immutable per cell.
- Protocol mismatches fail before proof search.
- No accepted historical artifact is rewritten in place.

**Performance acceptance**

- Adapter overhead is under 5% of representative evaluation wall time.
- Persistent workers retain the previously measured startup amortization.

### P13.2 — Reconstruct the M1.5 reference result under Reflex

**Phase:** P13  
**Dependencies:** P13.1, P8.9
**Size:** XL  
**Primary skill:** Scientific reproduction  
**Owned scope:** `domains/reflex-domain-lean`, `evidence/lean-m1.5`

**Purpose.** Prove that framework abstraction preserves accepted capacity, ablation, certification, and accounting conclusions.

**Deliverables**

- imported frozen inputs
- all-Rust model reproductions
- matched evaluation cells
- strict comparison report

**Implementation steps**

1. Import manifests/checkpoints/datasets by digest or regenerate with documented representation migration.
2. Reimplement the 2,607 and 99,902 parameter models in Burn/micro and validate scores.
3. Run registered cells on the same or qualified compute class.
4. Compare solve rates, actions, CPU, lineage qualification, and proof replays.

**Acceptance criteria**

- The selected capacity floor and training-signal conclusions agree within preregistered tolerances.
- Every solve kernel replays.
- Any representation/performance deviation is explained by named evidence.
- No new result overwrites historical authority.

**Performance acceptance**

- Representative evaluation is no slower than Project Reflex after excluding unavoidable protocol migration cost, or an ADR blocks release.
- Peak RSS stays within 8 GiB.

### P13.3 — Import and reconstruct the complete M2A negative result

**Phase:** P13  
**Dependencies:** P13.1, P4.7  
**Size:** L  
**Primary skill:** Scientific data migration  
**Owned scope:** `domains/reflex-domain-lean`, `evidence/lean-m2a`

**Purpose.** Use M2A as a regression target for matrices, strict evidence, and the framework’s richer supervision abstractions.

**Deliverables**

- M2A manifest importer
- 180-arm reconstruction
- result comparison
- incident history mapping

**Implementation steps**

1. Import all 30 bundles, 180 arms, calibration, attempts, reports, and digests.
2. Reconstruct primary tables from evidence through Reflex analytics.
3. Map the heartbeat and JSON-key-order incidents into explicit infrastructure/analysis events without altering outputs.
4. Publish a comparison to the supplied authoritative M2A result.

**Acceptance criteria**

- Solve rates, action counts, CPU totals, bundle counts, and accepted attempts match exactly or with documented formatting-only differences.
- All reported solves retain replay references.
- Negative registered conclusion remains unchanged.
- No missing cell is hidden by aggregation.

**Performance acceptance**

- Reconstruction completes under 30 seconds from warm local cache.
- Imported evidence remains content-addressed and deduplicated.

### P13.4 — Mine multi-proof DAGs for a bounded M2A subset

**Phase:** P13  
**Dependencies:** P13.3, P6.7  
**Size:** XL  
**Primary skill:** Proof search research  
**Owned scope:** `domains/reflex-domain-lean`, `reflex-dataset`

**Purpose.** Test whether single-route supervision caused the neural failure before changing architectures blindly.

**Deliverables**

- subset registration
- multi-policy/high-budget searches
- proof DAGs
- coverage report

**Implementation steps**

1. Select a frozen representative subset across families and strata.
2. Run uniform, heuristic, k-NN, 3K, 100K, randomized seeds, and high-budget search.
3. Kernel-certify every route and merge canonical states.
4. Report viable-action multiplicity, unknown coverage, and minimum cost-to-go.

**Acceptance criteria**

- Subset selection occurs before new search results.
- Every viable edge has proof evidence.
- Unknown remains distinct from dead.
- Coverage suffices to evaluate oracle ranking on the named subset or the phase reports insufficiency.

**Performance acceptance**

- Use the 20-worker pool with one search cell per worker.
- DAG mining remains within declared compute and storage budget.

### P13.5 — Build the M2B proof-DAG training datasets

**Phase:** P13  
**Dependencies:** P13.4, P8.4  
**Size:** L  
**Primary skill:** ML data research  
**Owned scope:** `domains/reflex-domain-lean`, `reflex-dataset`

**Purpose.** Compare old single-route labels with honest multi-positive/censored labels on identical states and feature schemas.

**Deliverables**

- single-route dataset
- proof-DAG dataset
- feature-collision audit
- dataset comparison report

**Implementation steps**

1. Compile both dataset variants from the same source episodes.
2. Quantify how often old negatives are now known viable.
3. Find state/action feature collisions or near-collisions with different cheapest actions.
4. Add only the smallest search-context features justified by the audit as a separate schema version.

**Acceptance criteria**

- Dataset variants differ only in registered supervision/feature changes.
- All label flips trace to certified routes.
- Feature additions have explicit semantic definitions and no identities.
- Train/dev/eval leakage gates remain zero.

**Performance acceptance**

- Dataset compilation meets P8 throughput and memory gates.
- Collision analysis streams and does not require all feature rows in RAM.

### P13.6 — Train M2B loss, capacity, and feature ablations

**Phase:** P13  
**Dependencies:** P13.5, P8.9  
**Size:** XL  
**Primary skill:** ML experimentation  
**Owned scope:** `domains/reflex-domain-lean`, `reflex-training`

**Purpose.** Determine whether supervision, feature sufficiency, capacity, or optimization explains the M2A neural regression.

**Deliverables**

- loss sweep
- feature sweep
- 3K/100K capacity arms
- offline diagnostics

**Implementation steps**

1. Train old binary imitation, multi-positive binary, masked pairwise, listwise cost-to-go, and censored variants.
2. Run original and justified context feature schemas.
3. Compare 3K and 100K models with equal data and optimization budgets.
4. Report top-k viability, margins, entropy, overfitting, family/stratum metrics, and training convergence.

**Acceptance criteria**

- No evaluation data influences selection.
- The 100K model autopsy distinguishes training fit from held-out viable recall.
- Unknown candidates receive no negative gradient in corrected losses.
- All failed runs remain in the matrix.

**Performance acceptance**

- Training cells fit reference-4vcpu-8gb/8 GiB.
- All-Rust training meets or improves Project Reflex wall/CPU throughput.

### P13.7 — Run first-action and oracle-ranking diagnostics

**Phase:** P13  
**Dependencies:** P13.4, P6.6  
**Size:** L  
**Primary skill:** Causal experiment design  
**Owned scope:** `domains/reflex-domain-lean`, `reflex-eval`

**Purpose.** Separate ranking-value ceiling from model and search-integration failures.

**Deliverables**

- first-action oracle
- known-cost oracle
- feature oracle
- attribution report

**Implementation steps**

1. Force one certified viable first action and then return to each policy.
2. Run a policy that orders known DAG actions by minimum cost-to-go where coverage exists.
3. Compare against uniform at matched budgets.
4. Attribute outcomes to candidate generation, label coverage, feature distinguishability, policy learning, or search integration.

**Acceptance criteria**

- Oracles never contaminate training or confirmatory results.
- Coverage-limited oracle metrics name their denominator.
- If oracle is near uniform, the report explicitly says local ranking has little available value.
- If oracle is strong, neural failure is not blamed on task impossibility.

**Performance acceptance**

- Oracle lookup adds under 5% to diagnostic search.
- Diagnostics reuse mined DAGs without rerunning unnecessary proof search.

### P13.8 — Run the preregistered M2B confirmatory matrix

**Phase:** P13  
**Dependencies:** P13.6, P13.7
**Size:** XL  
**Primary skill:** Scientific experiment execution  
**Owned scope:** `domains/reflex-domain-lean`, `evidence/lean-m2b`

**Purpose.** Answer whether corrected learning recovers compositional guidance or whether the architecture needs richer value/planning models.

**Deliverables**

- registration
- complete matrix
- strict aggregation
- findings update

**Implementation steps**

1. Freeze selected dataset/loss/features from development evidence.
2. Run independent lineages, 1×/4× budgets, all strata, and required baselines.
3. Kernel replay every solve and include scoring overhead.
4. Publish positive or negative conclusion against registered criteria.

**Acceptance criteria**

- Expected, observed, accepted, and excluded cells reconcile exactly.
- No retry or analysis fix mutates matrix output.
- Severe family/stratum regressions veto qualification.
- The result is added to Project Reflex findings without erasing M2A.

**Performance acceptance**

- Matrix uses the ordinary 20-worker class unless measured memory requires promotion.
- Worker and cell performance meet the calibrated budgets.

## P14 — Verified knowledge economy

**Goal:** Allow verified knowledge to grow online while measuring retrieval tax, behavioral reuse, and curation.

**Exit gate:** Immutable editions reproduce exactly; knowledge can activate without weight mutation; junk-injection and leave-one-out reports attribute value.

### P14.1 — Define verified knowledge records and immutable editions

**Phase:** P14  
**Dependencies:** P5.5, P2.5  
**Size:** L  
**Primary skill:** Knowledge systems  
**Owned scope:** `reflex-knowledge`

**Purpose.** Store reusable theorems, proof macros, exact search memory, and optimization rules explicitly instead of forcing models to memorize them.

**Deliverables**

- KnowledgeRecord
- knowledge class enum
- edition manifest
- compatibility rules

**Implementation steps**

1. Separate exact memoized solution, instantiable theorem, proof macro, rewrite/catalog rule, and research artifact classes.
2. Record statement/pattern, proof/certificate, compatibility, structural encoding, discovery provenance, dependencies, and utility references.
3. Build immutable content-addressed editions with deterministic ordering.
4. Keep archived verified records even when excluded from active retrieval.

**Acceptance criteria**

- Every active record has accepted verification.
- Editing one record creates a new edition.
- Knowledge class is explicit in reports; exact memory is not called theorem abstraction.
- Edition load rejects incompatible domain/action/feature schema.

**Performance acceptance**

- Load one million compact records in under 10 seconds from local page cache or stream an index without loading all payloads.
- Manifest memory scales with active index, not proof bytes.

### P14.2 — Implement the retrieval interface and baseline indexes

**Phase:** P14  
**Dependencies:** P14.1, P6.6  
**Size:** L  
**Primary skill:** Information retrieval  
**Owned scope:** `reflex-knowledge`

**Purpose.** Surface a bounded, measurable candidate set from a large explicit library.

**Deliverables**

- Retriever trait
- exact lookup
- inverted structural index
- k-NN baseline

**Implementation steps**

1. Define query features, filters, top-k, score, reason, and work accounting.
2. Implement exact canonical lookup and sparse structural postings before approximate indexes.
3. Cap ubiquitous postings and per-query work.
4. Return stable record IDs in deterministic tie order.

**Acceptance criteria**

- Same query/edition/config returns the same results.
- Retrieval work and CPU are recorded.
- Missing or archived entries do not appear unless explicitly requested.
- No model embedding table is keyed by theorem position.

**Performance acceptance**

- K100K-style retrieval p95 is below 1 ms for hot in-memory index.
- Index build is streaming and supports 1 million records within 8 GiB.

### P14.3 — Implement per-cell knowledge overlays and online activation

**Phase:** P14  
**Dependencies:** P14.1, P14.2, P9.2  
**Size:** L  
**Primary skill:** Continual systems  
**Owned scope:** `reflex-knowledge`, `reflex-runtime`

**Purpose.** Let verified knowledge become available quickly while registered cells remain pinned and reproducible.

**Deliverables**

- overlay store
- activation policy
- edition publication
- cell pinning

**Implementation steps**

1. Append newly verified records to a private experiment overlay.
2. In exploratory mode permit new episodes/generations to pin the latest accepted overlay snapshot; running cells never change.
3. Periodically compact overlays into a new immutable edition.
4. In confirmatory mode require predeclared edition and disable live activation.

**Acceptance criteria**

- A record never appears in an already-running cell.
- Overlay activation is atomic and content-addressed.
- Model weights do not update merely because knowledge activates.
- Compaction preserves query semantics or publishes a new retriever identity.

**Performance acceptance**

- Overlay lookup adds under 20% to base retrieval p95 at configured size.
- Compaction does not block active readers.

### P14.4 — Implement behavioral knowledge-use accounting

**Phase:** P14  
**Dependencies:** P14.2, P3.1  
**Size:** L  
**Primary skill:** Causal instrumentation  
**Owned scope:** `reflex-knowledge`, `reflex-economics`

**Purpose.** Distinguish retrieved, attempted, proof-used, necessary, and cost-reducing knowledge.

**Deliverables**

- KnowledgeUse events
- proof dependency extraction
- leave-one-out runner
- utility attribution

**Implementation steps**

1. Record retrieval rank/score, application attempts, transition result, final proof dependency, and search work.
2. For high-value records run candidate-only, leave-one-out, matched random-library, and shuffled retrieval controls.
3. Subtract retrieval index build/query overhead from economics.
4. Mark causal confidence level on every attributed utility value.

**Acceptance criteria**

- Raw retrieval count is never reported as reuse.
- Final proof use traces to artifact receipts.
- Leave-one-out reruns use identical policy/budget/cache conditions.
- Ambiguous alternative proofs are labeled rather than overclaimed.

**Performance acceptance**

- Use accounting adds under 5% to retrieval/application CPU.
- Ablation scheduler deduplicates matched cells.

### P14.5 — Define immutable vector-valued utility and economics

**Phase:** P14  
**Dependencies:** P3.1, P4.5  
**Size:** L  
**Primary skill:** Metrics and economics  
**Owned scope:** `reflex-economics`

**Purpose.** Separate measured facts from derived reward scalarization and retain units, provenance, and uncertainty.

**Deliverables**

- UtilityObservation
- unit registry
- derived metric expressions
- marginal utility API

**Implementation steps**

1. Support correctness status, verified actions, successor goals, child CPU, wall, RSS, bytes, runtime cycles, code/frame size, solve unlocks, reuse, descendants, retrieval tax, and discovery cost.
2. Store observed values with evaluator identity, population, direction, confidence, and evidence references.
3. Compute scalar rewards only in named policies over raw observations.
4. Support baseline/treatment marginal comparisons and restricted-work metrics.

**Acceptance criteria**

- Changing scalarization does not mutate raw observations.
- Incompatible units cannot be added.
- Unsolved bounded searches use registered restricted-work treatment.
- Cost savings include framework/model/retrieval overhead.

**Performance acceptance**

- Aggregation processes at least 5 million observations/s/core.
- Hot observation append uses compact typed events, not dynamic maps.

### P14.6 — Implement curation, tiering, and utility ledgers

**Phase:** P14  
**Dependencies:** P14.4, P14.5  
**Size:** L  
**Primary skill:** Library learning  
**Owned scope:** `reflex-knowledge`

**Purpose.** Control active retrieval tax while retaining every verified artifact.

**Deliverables**

- per-record utility ledger
- hot/warm/archive tiers
- curation policy interface
- promotion/demotion receipts

**Implementation steps**

1. Track query count, proof use, marginal work saved, CPU saved, compression contribution, descendant value, discovery cost, and maintenance/retrieval tax.
2. Implement no-curation, frequency, compression, marginal-utility, and human-seeded baselines.
3. Demote rather than delete.
4. Require curation policy identity in knowledge edition.

**Acceptance criteria**

- Tier changes reconstruct from ledger and policy.
- Records with no observed utility are censored, not declared worthless.
- Curation never changes theorem truth or proof bytes.
- A restored archived record retains history.

**Performance acceptance**

- Curator scores one million records in under 60 seconds on reference-4vcpu-8gb.
- Hot-set lookup meets P14.2 latency.

### P14.7 — Run junk-injection and knowledge-growth experiments

**Phase:** P14  
**Dependencies:** P14.6
**Size:** XL  
**Primary skill:** Scientific experiment execution  
**Owned scope:** `reflex-knowledge`, `evidence/knowledge-economy`

**Purpose.** Measure whether growing explicit knowledge actually substitutes for search/model capacity or merely adds distractors.

**Deliverables**

- K0–K1M snapshots
- junk classes
- curation matrix
- behavioral reuse report

**Implementation steps**

1. Build exact-memory, theorem, and macro curves separately.
2. Inject exact duplicates, alpha variants, overspecialized instances, structurally plausible irrelevancies, and high-match useless facts.
3. Compare no curation, baselines, and learned curation at fixed policy and compute.
4. Report solve, verified work, CPU, retrieval tax, actual invocation, and capacity interactions.

**Acceptance criteria**

- Knowledge/evaluation overlap and novelty strata are explicit.
- No claim pools knowledge classes.
- Nonmonotone curves are reported as results.
- Learned curation is evaluated on held-out contamination and real growth.

**Performance acceptance**

- Ordinary cells stay within 8 GiB through streaming/index design.
- Portable multi-process execution passes the hermetic resilience gate.

## P15 — Research graph, taste, and proposal learning

**Goal:** Add long-horizon value prediction and proposal learning only after local search and economics are grounded.

**Exit gate:** Taste beats registered baselines on frozen delayed-utility data before any learned proposer can allocate significant compute.

### P15.1 — Implement the research ancestry graph

**Phase:** P15  
**Dependencies:** P14.1, P14.5  
**Size:** L  
**Primary skill:** Graph data systems  
**Owned scope:** `reflex-research-graph`

**Purpose.** Retain how theorems, candidates, abstractions, models, and algorithms were produced so delayed value can be attributed.

**Deliverables**

- ResearchNode
- typed edges
- immutable graph shards
- query API

**Implementation steps**

1. Create edges generated-from, generalized-from, enabled-by, retrieved-by, trained-from, inspired-by, replaced-by, and applied-to.
2. Require evidence references and creator/model/knowledge state.
3. Append graph shards through CAS and maintain small indexes in metadata.
4. Detect impossible cycles for edge classes that must be acyclic while allowing explicit theory cycles where meaningful.

**Acceptance criteria**

- Every verified artifact names its direct provenance.
- Graph reconstruction is independent of summary tables.
- Edge edits create new evidence rather than mutating history.
- Queries can traverse descendants and ancestors with depth/budget limits.

**Performance acceptance**

- Stream at least 1 million edges/s/core into Parquet.
- Bounded traversal of a 100-million-edge graph avoids loading it all in RAM.

### P15.2 — Implement delayed and counterfactual credit assignment

**Phase:** P15  
**Dependencies:** P15.1, P14.4  
**Size:** L  
**Primary skill:** Causal learning systems  
**Owned scope:** `reflex-research-graph`, `reflex-economics`

**Purpose.** Propagate realized descendant value backward without pretending correlation is causal certainty.

**Deliverables**

- credit algorithms
- discount/config schema
- causal confidence
- counterfactual ablation hooks

**Implementation steps**

1. Implement direct-use value, discounted descendant value, and option-value observations as separate measures.
2. Weight paths by edge type, replaceability, and causal confidence.
3. Use leave-one-out or matched reruns for high-value claims where feasible.
4. Never overwrite realized immediate economics with propagated credit.

**Acceptance criteria**

- Hand-worked DAG fixtures receive expected credit.
- Interchangeable parent paths split or qualify credit according to policy.
- Cycles converge under a bounded specified algorithm or are excluded.
- Every propagated value links to source utility observations.

**Performance acceptance**

- Credit computation streams or partitions graphs larger than RAM.
- One million-edge acyclic fixture completes under 10 seconds/core.

### P15.3 — Build delayed-utility and Mathlib-Rewind-compatible taste datasets

**Phase:** P15  
**Dependencies:** P15.2, P8.2  
**Size:** L  
**Primary skill:** Temporal data science  
**Owned scope:** `reflex-dataset`, `reflex-research-graph`

**Purpose.** Train potentiality from features available at artifact birth and utility observed only later.

**Deliverables**

- birth-feature snapshot
- observation horizons
- censoring rules
- temporal split manifest

**Implementation steps**

1. Freeze candidate features, theory/library state, module context, and critic-visible history at birth.
2. Attach immediate, horizon-specific, and descendant utility without leaking future features.
3. Treat no observed utility within horizon as right-censored.
4. Support Wrela economics and historical Mathlib utility through the same generic schema with domain-specific heads.

**Acceptance criteria**

- Future information cannot enter birth features.
- Temporal windows and vocabulary eligibility are explicit.
- Same artifact may have different horizon labels without duplication ambiguity.
- Sociology-only and mathematical-only feature families can be separated.

**Performance acceptance**

- Dataset construction remains streaming over multi-year library histories.
- Feature snapshot storage is deduplicated by content identity.

### P15.4 — Train and calibrate taste critics

**Phase:** P15  
**Dependencies:** P15.3, P7.6, P8.9  
**Size:** XL  
**Primary skill:** Long-horizon ML  
**Owned scope:** `reflex-training`, `reflex-ml-burn`

**Purpose.** Learn a prior over future utility while preserving interpretable heads and uncertainty.

**Deliverables**

- multi-head critic
- censored ranking objectives
- calibration suite
- baseline comparison

**Implementation steps**

1. Train immediate, long-horizon, descendant, option, cost, and uncertainty heads with task-appropriate masked losses.
2. Compare random, simplicity, centrality, compression, recent-activity, intrinsic reuse, and hand-designed elegance baselines.
3. Evaluate fixed candidate pools before closed-loop scheduling.
4. Report calibration and utility concentration in top-ranked fractions.

**Acceptance criteria**

- Critic cannot see future goals or outcomes in inputs.
- A claim requires held-out temporal/domain evaluation.
- Mathematical-only performance is reported separately from sociology-only prediction.
- No proposal model is enabled by a critic that fails the fixed-pool gate.

**Performance acceptance**

- Training uses bounded resources declared per critic class.
- Inference overhead is measured against expected exploration savings.

### P15.5 — Implement the portfolio research scheduler

**Phase:** P15  
**Dependencies:** P15.4, P9.6  
**Size:** L  
**Primary skill:** Bandits and scheduling  
**Owned scope:** `reflex-scheduler`

**Purpose.** Allocate compute across exploitation, near-frontier, exploratory, adversarial, and speculative lanes instead of optimizing one greedy scalar.

**Deliverables**

- PortfolioPolicy
- lane budgets
- uncertainty exploration
- scheduler replay

**Implementation steps**

1. Define lane eligibility, minimum/maximum shares, horizon scalarization, and rebalance cadence.
2. Reserve nonzero exploration and speculative budgets unless an operator disables them.
3. Use critic predictions and uncertainty only as scheduling inputs; hard compute and verifier limits remain external.
4. Record every allocation decision and counterfactual available candidates.

**Acceptance criteria**

- The scheduler cannot starve mandatory baseline/exploration lanes.
- Same inputs/seed produce same allocations.
- Lane economics update only at declared boundaries.
- A taste model does not define truth or realized reward.

**Performance acceptance**

- Schedule 1 million frontier items/s/core in batch mode.
- Allocation overhead is below 1% of exploration compute.

### P15.6 — Gate and integrate learned proposal models

**Phase:** P15  
**Dependencies:** P15.4, P15.5, P7.7  
**Size:** XL  
**Primary skill:** Generative ML  
**Owned scope:** `reflex-ml-burn`, `reflex-scheduler`

**Purpose.** Add learned generation only after deterministic proposal mechanisms and taste can be evaluated independently.

**Deliverables**

- proposal training dataset
- generator checkpoint
- cheap falsification pipeline
- closed-loop gate

**Implementation steps**

1. Train on high-value verified proposals and their birth contexts while retaining invalid/failed outcomes as typed evidence.
2. Compare against mutation, anti-unification, enumeration, and evolutionary baselines under equal proposal/proof compute.
3. Pass generated candidates through domain validation, falsification, taste, search, and verifier.
4. Limit initial learned-proposal lane to a small portfolio share.

**Acceptance criteria**

- The proposer cannot co-train with the confirmatory critic on the same held-out window.
- Invalidity, novelty, proof success, and realized utility are reported separately.
- Promotion requires better verified library economics under equal compute.
- Failure cannot contaminate verified knowledge.

**Performance acceptance**

- Proposal inference respects configured budget and batching.
- Verifier queue remains bounded under invalid-proposal bursts.

## P16 — Operator experience, hardening, and v1 release

**Goal:** Make the system delightful, diagnosable, secure, portable, and safe to hand to less experienced implementers.

**Exit gate:** The release audit closes every invariant, performance gate, security check, tutorial, and dogfood requirement with named evidence.

### P16.1 — Implement the reflex CLI and declarative configuration

**Phase:** P16  
**Dependencies:** P9.6, P4.7  
**Size:** L  
**Primary skill:** CLI and configuration  
**Owned scope:** `reflex-cli`

**Purpose.** Make local and distributed use obvious, scriptable, and fully inspectable.

**Deliverables**

- reflex init/run/status/stop/replay/report/doctor commands
- TOML config schemas
- shell completions
- stable JSON output

**Implementation steps**

1. Use layered config with explicit precedence: compiled defaults, file, environment, CLI; identity-bearing values resolve before manifest creation.
2. Provide dry-run and manifest-diff modes.
3. Make human output concise and JSON output stable/versioned.
4. Never require users to know database tables or CAS paths.

**Acceptance criteria**

- A new user completes the bit-vector tutorial from README commands.
- Invalid config points to exact field and allowed values.
- Dry-run performs no durable mutation.
- JSON output has compatibility tests.

**Performance acceptance**

- CLI startup p95 is under 50 ms for local metadata-only commands.
- Status for 100,000 cells returns in under 2 seconds with pagination.

### P16.2 — Implement the thin operator API and dashboard data endpoints

**Phase:** P16  
**Dependencies:** P16.1, P4.3  
**Size:** M  
**Primary skill:** Rust web services  
**Owned scope:** `reflex`

**Purpose.** Expose remote control and observability without moving search or ML logic into the HTTP layer.

**Deliverables**

- Axum API
- OpenAPI schema
- auth middleware
- SSE/WebSocket status stream

**Implementation steps**

1. Map HTTP commands to the same scheduler services used by CLI.
2. Use pagination, ETags, and bounded response sizes.
3. Stream status/metrics updates; large artifacts use presigned object links or CLI download.
4. Require authenticated, scoped write operations and audit them.

**Acceptance criteria**

- API and CLI produce the same manifest/state transitions.
- Unauthenticated writes fail.
- Slow clients cannot backpressure workers.
- OpenAPI is generated and freshness-checked.

**Performance acceptance**

- Read endpoint p95 is below 100 ms at dogfood scale.
- API CPU is below 1% of fleet compute.

### P16.3 — Build canonical reports and explainability views

**Phase:** P16  
**Dependencies:** P4.7, P14.5  
**Size:** L  
**Primary skill:** Scientific reporting  
**Owned scope:** `reflex-report`, `reflex-analytics`

**Purpose.** Turn immutable evidence into clear scientific, economic, and operational answers.

**Deliverables**

- report templates
- strict reconstruction
- model/knowledge lineage views
- first-divergence explanations

**Implementation steps**

1. Produce solve/capability, matched work, CPU/wall/RSS, model overhead, knowledge use, utility, fleet cost, incidents, and identity sections.
2. Require every number to name its population, unit, and evidence query.
3. Separate modelled from measured and planning from billing.
4. Generate Markdown and canonical JSON.

**Acceptance criteria**

- Deleting summaries and regenerating from evidence yields the same canonical JSON.
- Missing/mutated inputs fail strict mode.
- Reports do not call unresolved/censored outcomes negatives.
- A cheaper failure is never labeled efficiency gain.

**Performance acceptance**

- M2A-style report regenerates under 30 seconds warm.
- Report queries use bounded memory.

### P16.4 — Integrate tracing, OpenTelemetry, metrics, and profiling

**Phase:** P16  
**Dependencies:** P1.1, P5.3
**Size:** L  
**Primary skill:** Observability  
**Owned scope:** `reflex-observability`

**Purpose.** Make every performance and reliability issue attributable without overwhelming the hot path.

**Deliverables**

- tracing spans
- OpenTelemetry exporter
- Prometheus metrics
- pprof integration

**Implementation steps**

1. Instrument experiment/cell/episode/search/domain/verifier/training/CAS/database boundaries with stable fields and IDs.
2. Use sampled traces and counters/histograms; never emit one log line per candidate.
3. Expose queue depths, CPU pools, inference, ledger, CAS, Postgres, retries, and worker lifecycle.
4. Support on-demand CPU/heap profiles attached to evidence.

**Acceptance criteria**

- Sensitive payloads and secrets are redacted.
- Trace IDs link operator events to cell evidence.
- Disabled exporters do not fail experiments.
- Metric names and units are documented and tested.

**Performance acceptance**

- Default observability overhead is under 2% CPU.
- Cardinality limits prevent per-state/candidate metrics.

### P16.5 — Complete property, fuzz, concurrency, and distributed fault testing

**Phase:** P16  
**Dependencies:** P9.5, P13.8
**Size:** XL  
**Primary skill:** Test engineering  
**Owned scope:** `workspace tests`, `fuzz`

**Purpose.** Attack the exact classes of failure that would corrupt evidence, identity, leases, search, or model promotion.

**Deliverables**

- proptest suites
- cargo-fuzz targets
- Loom models
- Turmoil network simulations

**Implementation steps**

1. Property-test canonicalization, IDs, search propagation, DecisionGroup labels, utility units, and manifests.
2. Fuzz ledger/protocol/CAS/checkpoint/dataset decoders.
3. Model writer actor, model swap, bounded channels, and permit broker under Loom.
4. Simulate lease expiry, duplicate messages, network partitions, storage errors, and coordinator restart under Turmoil.

**Acceptance criteria**

- No known panic or data corruption remains in supported input space.
- Every persisted decoder has a fuzz target and size limit.
- Loom explores the declared concurrency models without invariant violation.
- Distributed simulations prove stale fencing cannot finalize.

**Performance acceptance**

- Fast fuzz smoke fits deep CI; extended fuzz runs publish corpora nightly or manually.
- Tests have bounded timeouts and no flakes across 100 repetitions.

### P16.6 — Harden verifier isolation, secrets, and untrusted domains

**Phase:** P16  
**Dependencies:** P5.5, P2.4
**Size:** L  
**Primary skill:** Security engineering  
**Owned scope:** `reflex-runtime`, `reflex-domain-host`

**Purpose.** Assume candidate generators and external domain processes can crash, hang, or behave maliciously without letting them corrupt authority or credentials.

**Deliverables**

- sandbox policy
- process limits
- secret scopes
- security audit

**Implementation steps**

1. Run untrusted external processes with dedicated user, process group, rlimits/cgroups, bounded filesystem, no unnecessary network, and minimal environment.
2. Give external verifier children no credentials and only the minimal declared environment.
3. Validate all lengths and paths before allocation or file access.
4. Document trust boundaries and remaining assumptions.

**Acceptance criteria**

- A fork bomb, memory bomb, oversized frame, path traversal, and secret-print fixture are contained or rejected.
- External verifier children receive no framework or storage credentials.
- Untrusted process cannot write accepted metadata directly.
- Security failures are durable cell outcomes.

**Performance acceptance**

- Sandbox setup adds under 100 ms per persistent worker.
- Limits do not reduce normal verifier throughput beyond accepted 3% overhead.

### P16.7 — Package releases and optional interoperability crates

**Phase:** P16  
**Dependencies:** P16.5, P16.6  
**Size:** L  
**Primary skill:** Release engineering  
**Owned scope:** `release`, `optional/reflex-python`, `optional/reflex-onnx`

**Purpose.** Ship a one-command native installation while keeping non-Rust interoperability outside the default dependency graph.

**Deliverables**

- cargo binaries
- container images
- release checksums/SBOM
- optional interoperability design

**Implementation steps**

1. Publish the `reflex` binary for Linux x86_64/aarch64 and macOS arm64 where supported.
2. Publish checksums and exact native compatibility metadata for each binary.
3. Keep Python, ONNX, and libtorch adapters optional and disabled from v1 critical path; add them only after native release gates.
4. Publish migration/compatibility policy.

**Acceptance criteria**

- Default cargo install path contains no Python runtime.
- Packaged binaries include no build secrets.
- Checksums and SBOM verify.
- Optional crates cannot alter core persisted semantics.

**Performance acceptance**

- Release worker image is kept small enough for fast startup; target under 500 MiB compressed and justify increases.
- Binary startup and image pull appear in canary evidence.

### P16.8 — Run the v1 acceptance and handoff audit

**Phase:** P16  
**Dependencies:** P16.1, P16.3, P16.4, P16.5, P16.6, P16.7, P12.8, P13.8, P14.7  
**Size:** XL  
**Primary skill:** Technical leadership  
**Owned scope:** `docs`, `evidence/v1`

**Purpose.** Demonstrate that the implementation matches this authority and can be maintained by engineers who did not design it.

**Deliverables**

- invariant closure report
- performance closure report
- dogfood results
- new-domain handoff run

**Implementation steps**

1. Close every blocking invariant and acceptance criterion with executable or
   reconstructable evidence.
2. Run local bit-vector, portable multi-process fault, Wrela, Lean M1.5/M2A/M2B, and knowledge-economy gates.
3. Have an engineer or coding agent unfamiliar with internals implement a small new domain using only public docs and record friction.
4. Audit dependencies, security, backups, cleanup, and rollback.

**Acceptance criteria**

- No blocking gap or unexplained performance regression remains.
- All reports reconstruct from immutable evidence.
- The new-domain handoff needs no private architectural explanation.
- The repository can be built, tested, dogfooded, and cleaned from documented commands.

**Performance acceptance**

- All §5 v1 quantitative budgets pass on their named host classes or have accepted nonexpiring ADR replacements.
- The full acceptance suite reports total CPU, wall, RSS, storage, and cost.
