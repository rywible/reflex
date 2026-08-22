# CPU architecture and implementation language for Reflex

Date: 2026-08-21

## Decision

Build Reflex **entirely in stable Rust**.

- Keep the public API, Runtime Controller, search state, provenance, persistence, learned models, and optimized kernels in stable Rust.
- Write a safe, portable Rust reference implementation for every hot kernel.
- Optimize through Rust data layout, LLVM autovectorization, host-native code generation, LTO, and PGO while keeping Reflex-owned source under `unsafe_code = "forbid"`. Do not use C/C++, assembly, foreign runtimes, or native-library FFI to close performance gaps.
- Do not make a general tensor framework foundational. Start with purpose-built FP32 online linear/ranking models and a tiny fused MLP challenger behind a versioned model interface. Evaluate only Rust-native framework implementations for possible adoption.
- Use a hybrid search portfolio: exact enumerative/best-first search, stochastic mutation, and bounded equality-saturation/consolidation components. Learned components rank batched Opportunities, select Operators, forecast structured Potential, and allocate search; they never replace Verification or the deterministic controller.

The evidence shows that C++ currently has easier explicit SVE2 access; the project deliberately accepts that constraint to preserve a single-language implementation. The benchmark gates below choose among Rust implementations and reveal performance gaps without creating a foreign kernel layer.

## Evidence and inference

Statements labelled **Evidence** below are supported by primary documentation, source repositories, or original papers. Statements labelled **Inference** are architectural conclusions for Reflex.

## Target machine

Local inventory on 2026-08-21 reports:

| Property | Observed value |
|---|---|
| ISA / CPU | AArch64, 8 single-threaded Neoverse-V2 cores |
| Vector features | Neon, SVE, SVE2, BF16, I8MM; default SVE vector length 16 bytes (128 bits) |
| Cache | 64 KiB L1 data and 2 MiB L2 per core; 80 MiB shared L3 |
| Memory | 62 GiB usable RAM, one NUMA node, no swap |
| Toolchains | rustc/cargo 1.97.1 (LLVM 22), Clang 19, GCC 14; Julia absent |

**Evidence.** Arm identifies SVE2 and Neon as Neoverse-V2 features. Its optimization guide describes four 128-bit vector pipelines, so SVE2 on this host chiefly expands operations and predication rather than increasing lane width ([Neoverse V2 product page](https://developer.arm.com/compute-ip/neoverse-v2), [Neoverse V2 optimization guide](https://documentation-service.arm.com/static/668bc0a369e89f01e39c4668)). GCC supports both vector-length-agnostic and fixed-length SVE code generation and recommends `-mcpu`/`-mtune` for performance ([GCC AArch64 options](https://gcc.gnu.org/onlinedocs/gcc/AArch64-Options.html)).

**Inference.** Reflex should target private-L2 locality, contiguous batches, and low allocation/branch overhead before pursuing exotic matrix kernels. An 80 MiB L3 and 64 GiB-class RAM make a large in-memory knowledge system reasonable, but they do not make pointer-rich or randomly accessed layouts free. Eight real cores also make oversubscription by nested OpenMP/BLAS runtimes especially costly.

Build two artifacts in performance work:

1. A portable AArch64/x86-64 Rust build with scalar and stable SIMD/autovectorized paths.
2. A host-tuned Rust build (`-C target-cpu=native`) selected at installation or deployment.

Hardware-specific prepacked weights and indexes should be rebuildable caches. The canonical FP32 model state and portable data remain in the Domain Bundle.

## Language comparison

Scores are architectural judgments, not benchmark results: `best`, `strong`, `mixed`, or `weak` for this project.

| Criterion | Rust | C++ | Julia |
|---|---|---|---|
| Search-loop throughput ceiling | best | best | strong when type-stable and allocation-free |
| Predictable layout and allocation | best | best | mixed; possible for bits types/arrays, but GC-managed runtime remains |
| Safe parallel state | best | mixed | mixed |
| Explicit Arm SVE/SVE2 today | mixed | best | mixed |
| Tiny CPU training/inference ecosystem | mixed | best | strong interactively |
| In-process verifier/domain integration | best | strong | weak for a foreign multithreaded host |
| Embeddable reusable library | best | strong | weak |
| Portable deployment | best | strong | mixed |
| Reproducible build/runtime control | strong | strong | mixed-to-strong |
| Operational complexity | best | mixed | weak as an embedded runtime |

### Rust

**Evidence.** Safe Rust cannot cause undefined behavior, while unsafe contracts are isolated at explicit boundaries; ownership and `Send`/`Sync` move many concurrency errors to compile time ([Rustonomicon](https://doc.rust-lang.org/stable/nomicon/safe-unsafe-meaning.html), [Rust Book: concurrency](https://doc.rust-lang.org/stable/book/ch16-00-concurrency.html)). Rust offers `repr(C)` for interoperable layouts, C ABI exports, `staticlib`/`cdylib` outputs, target-feature-specific code generation, LTO, and panic-abort production profiles ([Rust type layout](https://doc.rust-lang.org/reference/type-layout.html), [Rust linkage](https://doc.rust-lang.org/beta/reference/linkage.html), [target features](https://doc.rust-lang.org/stable/reference/attributes/codegen.html), [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html)). AArch64 Linux with glibc is a Tier-1 host target ([rustc platform support](https://doc.rust-lang.org/rustc/platform-support/aarch64-unknown-linux-gnu.html)). Rayon provides a mature fixed-pool work-stealing scheduler, and Crossbeam exposes local deques and batch stealing for custom scheduling ([Rayon thread pool](https://docs.rs/rayon/latest/rayon/struct.ThreadPool.html), [Crossbeam deque](https://docs.rs/crossbeam/latest/crossbeam/deque/)). The high-performance `egg` equality-saturation library is itself Rust, reducing integration risk for a relevant search baseline ([egg repository](https://github.com/egraphs-good/egg), [egg paper](https://homes.cs.washington.edu/~cnandi/docs/popl21-cr.pdf)).

The important limitation is SIMD maturity. Stable Rust exposes AArch64 Neon intrinsics and permits `sve`/`sve2` target features, so LLVM may autovectorize suitable code. Explicit SVE/SVE2 types and intrinsics are still nightly-only under `stdarch_aarch64_sve` ([Rust stdarch source](https://doc.rust-lang.org/nightly/src/core/stdarch/crates/core_arch/src/aarch64/mod.rs.html), [LLVM vectorizers](https://llvm.org/docs/Vectorizers.html)).

**Inference.** Rust provides the required control over scalar code, memory layout, and LLVM autovectorization while materially reducing risk in a long-lived concurrent graph system. Its explicit stable-SVE gap is real. Reflex accepts that limitation and will optimize within stable Rust rather than introduce a second implementation language.

### C++

**Evidence.** Arm ACLE gives C/C++ standardized access to Neon, SVE, SVE2, BF16, and related feature macros and intrinsics ([Arm C Language Extensions](https://arm-software.github.io/acle/main/acle.html)). GCC and Clang have mature Neoverse and SVE tuning. The C++ ecosystem includes allocation resources such as `std::pmr::monotonic_buffer_resource`, parallel algorithms, OpenMP/TBB, Vowpal Wabbit, XGBoost, LightGBM, oneDNN, Arm Compute Library, and KleidiAI. C++ parallel execution may legally fall back to sequential execution, and unsequenced policies impose non-obvious restrictions on synchronization and library calls ([C++ draft: parallel algorithms](https://eel.is/c++draft/algorithms.parallel)).

Arm's KleidiAI is especially relevant: it supplies dependency-free, stateless C microkernels with no allocation, memory management, or scheduling, including SVE and I8MM variants designed for caller-controlled threading ([KleidiAI repository](https://github.com/ARM-software/kleidiai)). oneDNN supports AArch64 and can integrate Arm Compute Library, but brings its own primitive and threading-runtime choices ([oneDNN README](https://github.com/uxlfoundation/oneDNN/blob/main/README.md), [oneDNN build options](https://uxlfoundation.github.io/oneDNN/v3.5/dev_guide_build_options.html)).

**Inference.** C++ is presently the easiest route to explicit Arm SVE2 kernels, but adopting it even at leaf boundaries would create a second toolchain, safety model, build path, and testing surface. Reflex rejects that operational split and treats C++ implementations only as external evidence about possible hardware ceilings.

### Julia

**Evidence.** Julia can generate fast native code when functions are type-stable, hot loops avoid allocation, and buffers are preallocated. Its own performance guide warns that unexpected allocations and garbage collection are substantial bottlenecks and recommends function barriers and preallocation ([Julia performance tips](https://docs.julialang.org/en/v1/manual/performance-tips/)). Julia supports threads, atomics, SIMD annotations, C calls, manifests, and AArch64 CPU targeting. It also states that the task schedule is nondeterministic, shared mutable collections need manual locking, compute-bound tasks can delay GC safepoints, and data races can destroy memory safety ([Julia multithreading](https://docs.julialang.org/en/v1/manual/multi-threading/)).

The embedding constraint is decisive. Julia's C API is not fully thread-safe: API calls may originate only from the `jl_init` thread or threads started by Julia; calling it from host-created worker threads is unsupported and may crash ([Julia embedding: thread safety](https://docs.julialang.org/en/v1/manual/embedding/#Thread-safety)). PackageCompiler reduces startup latency, but sysimages are generally machine-specific and relocatable apps require all dependencies to be relocatable ([PackageCompiler](https://julialang.github.io/PackageCompiler.jl/dev/), [relocatable apps](https://julialang.github.io/PackageCompiler.jl/stable/apps.html)).

**Inference.** Julia is excellent for offline analysis and rapid algorithm experiments, and a pure-Julia Reflex could be fast. It is a poor foundation for the required embeddable, in-process, fail-fast library whose worker pool is owned by Reflex and whose hot-state lifetime must be tightly bounded. Julia is not installed on the target host, but installation cost is not the reason for rejection; the runtime and embedding model are.

### Why no fourth system language

No examined alternative supplied a whole-system combination that surpassed Rust for Reflex. C would offer ACLE access with less abstraction and safety; Python would make native extensions and a second runtime fundamental; Zig would add a less mature library/ML ecosystem. The project additionally rejects a mixed-language kernel boundary, choosing one build, ownership model, and performance discipline throughout.

## Proposed production architecture

### 1. One deterministic authority, several replaceable engines

The Runtime Controller owns all hard decisions: Resource Envelope accounting, Verification routing, Admission, revision pinning, and Operational Promotion. Search and learned components return typed proposals only.

Internally, define deep interfaces around batches rather than individual objects:

- `DomainKernel`: canonicalize, apply Operators, evaluate/Verify, measure, and extract structural features.
- `SearchEngine`: consume an immutable Campaign view and produce batches of Opportunities/Candidates.
- `DecisionModel`: batched typed predictions plus uncertainty; no correctness or promotion authority.
- `Trainer`: consume versioned Training Targets and produce an immutable challenger Model Revision.
- `KnowledgeConsolidator`: propose a challenger Knowledge Revision and Derived Operators.
- `KernelDispatch`: select portable or hardware-specialized Rust implementations once, outside hot loops.

**Inference.** Batch-oriented seams allow domain-specialized physical representations, make SIMD and cache locality available, amortize model/scheduler calls, and let benchmarks replace one engine without reopening the public Domain Definition.

### 2. Hot memory: indexed arenas plus scan-oriented columns

Use 32-bit generational IDs into segmented arenas for Artifacts, derivations, Opportunities, and ledger records while a configured live-state limit remains below four billion entries per object class. Store variable-sized payloads in append-only byte slabs. Keep frequently scanned fields—operator ID, structural size, semantic digest, costs, status, feature offsets, visit counts, prediction moments—in separate contiguous arrays.

Use structural interning/hash-consing before expensive downstream work and semantic interning after exact evaluation. The `egg` design demonstrates hash-consed e-nodes and compact IDs in a high-performance optimizer; it also documents the risk of e-graph growth, reinforcing bounded use rather than universal saturation ([egg paper](https://homes.cs.washington.edu/~cnandi/docs/popl21-cr.pdf)). Contiguous columnar buffers improve locality and enable vectorization ([Apache Arrow format introduction](https://arrow.apache.org/docs/format/Intro.html)). Phase-local temporary objects fit bump allocation, whose trade-off is fast pointer-bump allocation against bulk-only reclamation ([bumpalo documentation](https://docs.rs/bumpalo/latest/bumpalo/)).

**Inference.** Do not expose arena references across revisions or serialization. Persist IDs plus canonical content, and rebuild transient indexes on load. Use 64-byte-aligned/padded blocks where measurement shows scan kernels benefit, but do not force Arrow or any external columnar format into the core.

### 3. Parallelism: one owner of the core budget

Create one fixed worker set sized by the Resource Envelope. Each worker owns scratch buffers, temporary arenas, local proposal queues, local dedup staging, and metric counters. Use local LIFO work for locality and steal batches when idle. Crossbeam explicitly supports batch stealing; Rayon is a strong initial implementation when its abstraction fits ([Crossbeam deque](https://docs.rs/crossbeam/latest/crossbeam/deque/), [Rayon work stealing](https://docs.rs/rayon/latest/rayon/struct.ThreadPool.html)).

Flush local results to sharded global indexes in batches. A controller epoch aggregates measurements, performs Pareto/Search Frontier Admission, accounts resources, and publishes immutable read views. Avoid a mutex or atomic reference-count update per Candidate.

Any Rust-native learning framework must run within the Runtime-owned resource budget rather than create an unbudgeted second pool. Foreign learners and BLAS libraries are not production dependencies.

Production work stealing may be nondeterministic, as already allowed by ADR 0035. The Experimental Harness should instead use fixed partitions, predetermined random streams, deterministic reductions, and recorded compiler/CPU metadata.

### 4. Immutable revisions and asynchronous durability

A live Campaign pins immutable `Arc`-owned Knowledge and Model Revisions. Mutable search state appends to generation-local segments. At a checkpoint barrier, Reflex seals completed segments and hands them to the durability thread; workers continue on new segments. Publishing a new revision is an atomic pointer swap, but only newly constructed Campaigns adopt it.

Rust's `Arc` provides shared immutable ownership and clone-on-write facilities; `arc-swap` is optimized for read-mostly snapshots that must update without stopping readers ([Rust `Arc`](https://doc.rust-lang.org/std/sync/struct.Arc.html), [arc-swap documentation](https://docs.rs/arc-swap/latest/arc_swap/)).

**Inference.** Checkpoint data should be content-addressed canonical records plus a short recovery tail, not a byte dump of pointer-rich RAM. This preserves restart completeness and portability while keeping storage off the search path.

## Search architecture

No single search method should define Reflex. The first Reference Domain should run a portfolio through the same SearchEngine interface:

1. **Enumerative/best-first:** size- or cost-stratified expression construction with structural and semantic interning. This is the cold-start workhorse and strongest exact baseline for small `u8` spaces.
2. **Stochastic local search:** mutate verified expressions and retain stepping stones through the Search Frontier. STOKE established stochastic superoptimization as a credible alternative to systematic enumeration ([STOKE paper](https://theory.stanford.edu/~aiken/publications/papers/asplos13.pdf)).
3. **Bounded equality saturation:** use known and Derived Operators for local equivalence closure, extraction, and Knowledge Consolidation. `egg` is a reusable implementation, but its paper explicitly discusses e-graph explosion, so saturation needs hard node/iteration budgets.
4. **Solver-backed or constraint-guided proposals:** optional domain engines may propose Candidates, as Souper does for LLVM peepholes using SMT; the pinned Verification Kernel remains final ([Souper repository](https://github.com/google/souper), [Souper paper](https://research.google/pubs/souper-a-synthesizing-superoptimizer/)).

AlphaDev confirms that learned search can discover measured algorithm improvements, but its large AlphaZero-style training system is evidence for the problem class, not for adopting that architecture on eight CPU cores ([AlphaDev paper](https://www.nature.com/articles/s41586-023-06004-9)).

For exhaustive `u8` equivalence, represent input lanes in bit-sliced or packed contiguous blocks, evaluate batches without per-node allocation, and cache the complete semantic function/digest. The exact representation depends critically on expression arity: one unary `u8` function has only 256 inputs, whereas each additional variable multiplies the exhaustive space by 256. Arity and truth-table layout are therefore mandatory prototype measurements, not minor implementation details.

## Learned decision system

### Start smaller than a neural framework

The first complete Model Revision should contain several typed components, all behind one stable interface:

- normalized structural/domain features and their schema;
- per-Operator and per-search-engine online linear heads;
- structured Potential heads predicting immediate improvement, descendant/reuse/compression outcomes, verification cost, and dead-end probability at several horizons;
- calibrated uncertainty state;
- optional shared tiny MLP encoder and pairwise ranker;
- the exact Training Target derivation and optimizer state required for continuation.

The mutable trainer creates challengers; active Campaigns never observe in-place weight changes.

### Recommended model sequence

1. **Bootstrap heuristic:** deterministic counts, novelty, cost, and frontier-age rules. This is both required cold-start behavior and an ablation.
2. **Online linear/FTRL models:** one sparse or dense feature vector can feed typed regression/logistic heads with per-coordinate state. FTRL-Proximal has strong online, sparsity, calibration, and memory precedent at far larger scale than Reflex ([Google's FTRL production paper](https://research.google/pubs/ad-click-prediction-a-view-from-the-trenches/)).
3. **Linear contextual bandit:** use context/action features and explicit uncertainty for allocating exploration among Operators or engines. LinUCB is computationally efficient and designed for changing action pools ([LinUCB paper](https://www.microsoft.com/en-us/research/wp-content/uploads/2016/02/p661.pdf)). Treat this as an allocator, not a scalar definition of Potential.
4. **Pairwise/listwise ranker:** derive comparisons only within compatible decision contexts and Optimization Goals. RankNet/LambdaMART provide primary precedents for pairwise/listwise learning; XGBoost implements ranking objectives and a stable C API ([LambdaMART overview](https://www.microsoft.com/en-us/research/publication/from-ranknet-to-lambdarank-to-lambdamart-an-overview/), [XGBoost learning to rank](https://xgboost.readthedocs.io/en/stable/tutorials/learning_to_rank.html), [XGBoost C API](https://xgboost.readthedocs.io/en/stable/c.html)). Trees are periodic challengers, not the continuously updated default.
5. **Tiny MLP ensemble:** only after linear baselines saturate. Use a shallow shared encoder with typed heads, train from Replay batches, and evaluate a small ensemble for uncertainty. Deep ensembles are simple and parallelizable and have empirical calibration evidence, but their value here must be demonstrated against their extra compute ([deep ensembles paper](https://papers.nips.cc/paper/2017/hash/9ef2ed4b7fd2c810847ffa5fa85bce38-Abstract.html)).

Vowpal Wabbit is strong prior art for online learning, bounded memory, feature hashing, contextual bandits, and learning-to-search ([Vowpal Wabbit repository](https://github.com/VowpalWabbit/vowpal_wabbit), [Vowpal Wabbit implementation paper](https://www.jmlr.org/papers/volume10/langford09a/langford09a.pdf)). Its algorithms should inform Reflex's Rust implementations, but its C++ runtime is not a production dependency.

### CPU kernels and precision

Implement FP32 reference inference and training directly for the initial linear models and tiny MLP. For small fixed dimensions, fused loops can combine normalization, affine layers, activation, typed heads, loss, and optimizer updates without constructing a generic tensor graph.

Benchmark three Rust-native representations:

- FP32 packed/autovectorized Rust;
- BF16 operands with FP32 accumulation for batched training/inference;
- I8 prepacked inference with FP32 output/calibration.

Arm documents BF16 instructions and Arm libraries can dispatch them, but oneDNN's default math mode remains strict and warns that BF16 down-conversion may reduce accuracy ([Arm BF16 overview](https://developer.arm.com/community/arm-community-blogs/b/ai-blog/posts/bfloat16-processing-for-neural-networks-on-armv8_2d00_a), [oneDNN floating-point modes](https://uxlfoundation.github.io/oneDNN/dev_guide_attributes_fpmath_mode.html)). Promote reduced precision only on campaign-level ranking agreement, calibration, Pareto outcomes, and end-to-end CPU savings—not tensor error alone.

KleidiAI and oneDNN/ACL remain useful evidence for hardware capabilities and kernel shapes, but Reflex will not link them. A reduced-precision path is adopted only when a Rust implementation passes the same end-to-end promotion criteria.

### Framework assessment

- **Candle:** supports Rust CPU training and is intentionally minimalist, but its documented optimized CPU features emphasize Intel MKL and Apple Accelerate, not Linux Arm SVE2 ([Candle repository](https://github.com/huggingface/candle), [Candle installation](https://huggingface.github.io/candle/guide/installation.html)). Only its Rust-native CPU configuration is eligible, initially as a correctness/productivity baseline.
- **Burn:** advertises training and Arm CPU backends, but it is a broad abstraction and an open August 2026 issue reports incorrect gradients in several operations of the new CPU backend ([Burn repository](https://github.com/Tracel-AI/burn), [Burn CPU gradient issue](https://github.com/Tracel-AI/burn/issues/5296)). Only a Rust-native backend could be eligible, and not until the required path passes differential tests.
- **XGBoost/LightGBM/Vowpal Wabbit:** useful algorithmic and scientific reference points, but their C++ runtimes and C APIs exclude them from the production dependency graph.
- **oneDNN/ACL/KleidiAI:** credible native CPU kernels and useful performance references, but excluded by the Rust-only boundary.

## Benchmarks that must precede lock-in

### A. Search and Verification kernel bake-off

Implement the same packed `u8` evaluator/verifier in:

1. safe scalar Rust;
2. safe Rust written for LLVM autovectorization;
3. host-tuned safe Rust;
4. a portable x86-64 safe Rust counterpart.

Vary AST encoding, arity, expression depth, batch size, data layout, and cache working-set size. Record Candidates/s, verified semantic lanes/s, cycles, instructions, branches/misses, L1/L2/L3 misses, memory bandwidth, peak RSS, and energy if exposed. Inspect compiler vectorization remarks and generated machine code. Test cold and warm caches. An optimized Rust kernel earns adoption only if its end-to-end Campaign gain exceeds its complexity and maintenance cost.

### B. Scheduler and memory bake-off

Compare:

- Rayon custom pools versus a Crossbeam batched-deque pool;
- global queue versus per-worker local queues;
- AoS versus hot-field SoA;
- ordinary allocation versus per-worker phase arenas;
- lock-per-result versus batched sharded Admission;
- batch sizes across L1/L2/L3 boundaries.

Measure throughput, p50/p99 task latency, steal rate, synchronization time, bytes per retained Artifact, and checkpoint interference. Use 1, 2, 4, and 8 cores. The architecture should remain valid if Rayon wins; only the engine changes.

### C. Learned-model bake-off

On identical logged decisions and replay splits, compare:

- Bootstrap heuristic;
- FTRL linear heads;
- LinUCB or another linear uncertainty allocator;
- custom tiny FP32 MLP and ensemble;
- Rust-native Candle/Burn implementation of the identical MLP where reliable.

Measure prediction/training CPU time, resident bytes, update latency, calibration per typed head, pairwise ranking accuracy, exploration regret, and—most importantly—equal-budget Campaign Pareto improvement. Include feature extraction and training in the Resource Envelope. A more accurate predictor that lowers end-to-end discovery is a regression.

### D. Rust precision-kernel bake-off

For the winning MLP shapes compare Rust-native custom FP32, BF16, and I8 paths. Test single-Opportunity latency and realistic batches. Promotion requires preserved calibration and Campaign outcomes on a Selection Corpus plus a material CPU or energy reduction.

### E. Reproducibility and recovery

For each contender:

- freeze toolchain, dependency, target-feature, and random-stream metadata;
- replay accepted Artifacts through the Verification Kernel;
- compare deterministic Experimental Harness outputs across repeated processes;
- kill the process at randomized checkpoint phases and resume from the Domain Bundle;
- load portable bundles on AArch64 and x86-64, rebuilding only derived native caches.

## Rejected alternatives

### Whole-system C++

Rejected because its first-class ACLE and native ML ecosystem do not establish a whole-system throughput advantage, while C++ would increase the audit burden for concurrent graph memory and domain callbacks.

### Mixed-language leaf kernels

Rejected despite easier access to SVE2 and mature native libraries. A foreign kernel boundary would create a second toolchain, safety model, build path, and debugging surface; Reflex accepts the possible local performance cost to keep one Rust implementation.

### Julia as the embedded production runtime

Rejected because the host-thread restrictions, GC integration, JIT/sysimage operations, and deployment footprint conflict with Reflex-owned in-process workers and portable local embedding. Julia remains welcome for offline research that consumes exported Experience data.

### A heavyweight neural framework from day one

Rejected because the initial models are small, structured, and continuously updated; generic tensor dispatch, graph construction, nested threading, and broad dependencies are costs that must justify themselves. Keep the interface broad enough to adopt a framework when model complexity earns it.

### Nightly Rust for explicit SVE

Rejected for the production foundation. Stable Rust can autovectorize and detect target features. Reflex will revisit explicit SIMD only through a future architectural decision if safe Rust cannot meet an observed end-to-end requirement.

### One learned policy or one search algorithm

Rejected because the settled design requires a Bootstrap Revision, protected exploration, structured Potential, multiple Objectives, and causal ablations. A portfolio permits learning which engine works where and prevents an early architecture choice from becoming the definition of optimization.

## Principal risks and mitigations

| Risk | Consequence | Mitigation |
|---|---|---|
| Search is memory-bandwidth or hash-bound, not model-bound | SIMD/ML work yields no end-to-end gain | Profile full Campaigns; prioritize compact IDs, interning, SoA scans, and batching |
| Semantic dedup tables exceed the live envelope | Throughput collapse or OOM on a no-swap host | Explicit byte accounting, sharding, load-factor limits, consolidation, and cold compaction |
| Work stealing destroys locality | Eight cores scale poorly | Steal batches; preserve per-worker Seed/engine affinity; measure LLC misses |
| Rust-only kernels trail tuned native libraries | Lower peak throughput on some shapes | Optimize layout first; use PGO/LTO/Neon; measure the end-to-end gap; revisit when stable Rust gains SVE |
| Reduced precision changes ranking/taste | Faster model finds worse Artifacts | Canonical FP32 state; promotion on calibration and Campaign outcomes, not tensor distance |
| Online feedback becomes self-confirming | Potential collapses around early biases | protected exploration, replay diversity, revisable delayed targets, rotating Selection Corpora |
| Framework thread pools oversubscribe eight cores | Erratic throughput and invalid budgets | one Runtime-owned pool; admit only Rust-native backends that respect it |
| Production nondeterminism contaminates scientific claims | Irreproducible performance conclusions | separate deterministic Experimental Harness, fixed reductions, full provenance |
| Rust's SVE support changes | Current optimized paths become stale | keep portable reference kernels and benchmark stable Rust SVE when it lands |

## Bottom line

The decision is **Rust everywhere in the Reflex implementation**: ownership, orchestration, search, verification integration, learning, persistence, and optimized kernels. On this Neoverse-V2 machine, optimize contiguous exact evaluation and graph locality first, deploy lightweight online models before tiny neural ensembles, and treat reduced precision and target-specific SIMD as measured Rust accelerators rather than architectural premises.

This gives up the option to import a tuned foreign kernel when Rust trails it, but preserves one toolchain, one safety model, one build graph, and one performance discipline. Julia and C++ systems remain research references, not linked components.
