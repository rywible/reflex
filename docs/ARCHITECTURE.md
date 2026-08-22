# Reflex Architecture

Status: accepted architecture for initial implementation

This document defines the intended Rust interfaces, ownership model, Runtime shape, performance contracts, and first production implementation slice for Reflex. Domain language is defined in [`CONTEXT.md`](../CONTEXT.md); architectural rationale lives in [`docs/adr/`](./adr/).

## Architectural thesis

Reflex has two public seams:

1. Consumers invoke one deep Improvement Session operation.
2. Domain authors implement five explicit semantic capability Modules.

Everything concerned with autonomous execution remains private Runtime implementation. In particular, callers cannot select or orchestrate Campaigns, search engines, learned models, training cadence, Admission, Knowledge Consolidation, Operational Promotion, scheduling, or persistence internals.

The design is asymmetric by intent. Consumer use is extremely small. Domain integration is explicit and demanding because a small number of domain authors serve potentially billions of hot Candidate operations.

## Workspace shape

```text
reflex/
├── Cargo.toml
├── crates/
│   ├── reflex/          # Published library and deep Runtime Module
│   ├── reflex-bitvec/   # Production u8 Reference Domain Adapter
│   └── reflex-bundle/   # Unpublished canonical Domain Bundle framing
├── xtask/               # Unpublished Rust repository commands
└── docs/
```

There is no `reflex-core`, `reflex-types`, `reflex-runtime`, `reflex-model`, or `reflex-storage` crate. Those divisions would create shallow cross-crate interfaces and expose implementation coupling. The `reflex` crate owns them as private Modules. `reflex-bundle` is a deliberately narrow exception: it is unpublished and owns only canonical data framing shared by the production Runtime and internal Experimental Harness, preventing either adapter from carrying a second format implementation.

The initial `reflex` crate exposes only domain construction, goals, session execution, Measurements, verified results, and Domain Bundle handles:

```rust
pub mod bundle;
pub mod domain;
pub mod goal;
pub mod measurement;
pub mod session;

pub use session::{improve, ImprovementRequest, SessionOutcome};
```

Its search, controller, Campaign, frontier, knowledge, learning, memory, and durability Modules remain private.

## Consumer interface

### One operation

```rust
pub fn improve<D, O>(
    domain: D,
    request: ImprovementRequest<D>,
    observer: O,
) -> Result<SessionOutcome<D>, SessionError<D::Error>>
where
    D: DomainDefinition,
    O: for<'a> FnMut(ParetoUpdate<'a, D>) -> ControlFlow<()>;
```

`improve` is blocking and consumes the Domain Definition and request. It owns all CPU threads granted by the Resource Envelope until it returns. Reflex does not impose an asynchronous runtime; a caller that wants background execution places this operation on its own thread.

The observer is invoked serially by the Runtime Controller only after an atomic change to the Pareto Frontier. It never observes Candidate traffic or partially admitted state. `ControlFlow::Break(())` requests an orderly stop; Reflex seals a restart-complete Domain Bundle before returning. Observer time counts against the elapsed-time limit.

### Request

```rust
pub struct ImprovementRequest<D: DomainDefinition> {
    goals: GoalSet<D>,
    seeds: D::SeedScope,
    resources: ResourceEnvelope,
    bundle: BundlePlan,
}

impl<D: DomainDefinition> ImprovementRequest<D> {
    pub fn new(
        goals: GoalSet<D>,
        seeds: D::SeedScope,
        resources: ResourceEnvelope,
        bundle: BundlePlan,
    ) -> Result<Self, RequestError>;
}
```

Fields are private. Construction establishes structural invariants such as nonempty Goals and nonzero required resources. Semantic checks—Metric compatibility, Seed validity, Semantic Identity, Verification replay, and bundle integrity—occur inside `improve` before search begins.

There is no typestate builder. Calling setters in an order cannot prove the semantic conditions that matter.

`GoalSet<D>` is nonempty by construction and contains caller-supplied Optimization Goals only. Reflex never infers value criteria from available Measurements.

```rust
pub struct GoalSet<D: DomainDefinition> {
    goals: Vec<OptimizationGoal<D>>,
}

impl<D: DomainDefinition> GoalSet<D> {
    pub fn one(goal: OptimizationGoal<D>) -> Self;

    pub fn try_from_iter(
        goals: impl IntoIterator<Item = OptimizationGoal<D>>,
    ) -> Result<Self, EmptyGoalSet>;
}
```

An Optimization Goal is structured data, not an evaluator callback and not a scalar score:

```rust
pub struct OptimizationGoal<D: DomainDefinition> {
    constraints: Vec<MeasurementConstraint<D>>,
    objectives: NonEmpty<Objective<D>>,
    preference: Preference<D>,
    success: Option<SuccessCondition<D>>,
}

pub struct MeasurementConstraint<D: DomainDefinition> {
    metric: D::Metric,
    relation: ThresholdRelation,
    threshold: D::Observation,
}

pub struct Objective<D: DomainDefinition> {
    metric: D::Metric,
    direction: Direction,
}

pub enum Direction {
    Minimize,
    Maximize,
}

pub enum ThresholdRelation {
    AtMost,
    AtLeast,
}

pub struct Preference<D: DomainDefinition> {
    priority_tiers: NonEmpty<NonEmpty<D::Metric>>,
    tolerances: Vec<MeasurementTolerance<D>>,
}

pub struct MeasurementTolerance<D: DomainDefinition> {
    metric: D::Metric,
    amount: D::Observation,
}

pub struct SuccessCondition<D: DomainDefinition> {
    thresholds: NonEmpty<MeasurementConstraint<D>>,
}

impl<D: DomainDefinition> OptimizationGoal<D> {
    pub fn new(
        constraints: impl IntoIterator<Item = MeasurementConstraint<D>>,
        objectives: NonEmpty<Objective<D>>,
        preference: Preference<D>,
        success: Option<SuccessCondition<D>>,
    ) -> Result<Self, GoalError>;
}

impl<D: DomainDefinition> MeasurementConstraint<D> {
    pub fn new(
        metric: D::Metric,
        relation: ThresholdRelation,
        threshold: D::Observation,
    ) -> Self;
}

impl<D: DomainDefinition> Objective<D> {
    pub fn new(metric: D::Metric, direction: Direction) -> Self;
}

impl<D: DomainDefinition> Preference<D> {
    pub fn tiered(
        priority_tiers: NonEmpty<NonEmpty<D::Metric>>,
        tolerances: impl IntoIterator<Item = MeasurementTolerance<D>>,
    ) -> Result<Self, GoalError>;
}

impl<D: DomainDefinition> MeasurementTolerance<D> {
    pub fn new(metric: D::Metric, amount: D::Observation) -> Self;
}

impl<D: DomainDefinition> SuccessCondition<D> {
    pub fn all(
        thresholds: NonEmpty<MeasurementConstraint<D>>,
    ) -> Self;
}

```

Goal construction validates purely structural relationships. Because Metric catalogs and domain-defined Observation semantics require the installed Domain Definition, `improve` validates that referenced Metrics exist, values have the right representation, Objectives are unique, Preference tiers mention Objectives exactly once, tolerances are valid, and Success Conditions are compatible with Constraints before opening a Campaign. `NonEmpty<T>` is a small Reflex-owned collection with private storage, not a third-party public type.

Constraints decide run-specific result eligibility. Objectives define every axis of the per-Goal Pareto relation. Preference tiers and tolerances guide allocation among incomparable opportunities but never remove a nondominated result merely because a preferred scalarization would have lost it. A Success Condition is an optional sufficient stopping threshold; absence means the Resource Envelope or observer stop ends the Session.

The Resource Envelope is likewise explicit and fully bounded:

```rust
pub struct ResourceEnvelope {
    worker_threads: NonZeroUsize,
    resident_bytes: NonZeroU64,
    durable_bytes: NonZeroU64,
    elapsed_time: NonZeroDuration,
    cpu_time: NonZeroDuration,
    verification_requests: NonZeroU64,
}

impl ResourceEnvelope {
    pub fn new(
        worker_threads: NonZeroUsize,
        resident_bytes: NonZeroU64,
        durable_bytes: NonZeroU64,
        elapsed_time: NonZeroDuration,
        cpu_time: NonZeroDuration,
        verification_requests: NonZeroU64,
    ) -> Self;
}
```

The Runtime stops scheduling before any one budget would be exceeded. Protected Allocations are private policy within these totals; callers grant resources but do not orchestrate their distribution.

Internally, the Resource Envelope Guard receives typed resident reservations that distinguish retained live inventory, scoped transient overlap, and bytes awaiting durability publication. It alone applies resident, durable, verification, elapsed, and CPU limits and records peak usage; Runtime phases describe a reservation but do not interpret limits independently.

### Bundle plan

```rust
pub enum BundlePlan {
    Fresh {
        target: PathBuf,
    },
    Resume {
        source: PathBuf,
        target: PathBuf,
    },
}
```

Every public Improvement Session produces a durable Domain Bundle. `source` and `target` may be equal; publication still uses a new sealed file followed by atomic replacement. Ephemeral Campaigns may exist privately, but ephemeral Sessions do not weaken the public durability contract.

If `source` contains an interrupted Session, the Goal Set, Seed Scope, Semantic Identity, and original Resource Envelope must canonically match the request; already consumed resources remain consumed and recovery continues from the tail. If `source` contains a completed Session, its retained knowledge becomes the starting revision for a new request and the new Resource Envelope starts at zero usage. A mismatch never silently discards interrupted work.

### Outcome and updates

```rust
pub struct SessionOutcome<D: DomainDefinition> {
    completion: Completion,
    pareto: ParetoSnapshot<D>,
    usage: ResourceUsage,
    bundle: DomainBundle,
}

pub enum Completion {
    ResourceEnvelopeExhausted,
    SuccessConditionsSatisfied,
    StoppedByObserver,
    NoEligibleWork,
}

pub struct ParetoUpdate<'a, D: DomainDefinition> {
    sequence: u64,
    added: &'a [VerifiedArtifact<D>],
    removed: &'a [ArtifactKey],
    affected_goals: &'a [GoalId],
}
```

`ArtifactKey` is an opaque public wrapper around the canonical Artifact digest, not a live arena index. `GoalId` is derived from the canonical Goal declaration and is stable across recovery. Updates borrow immutable Runtime storage and are valid only for the observer call. The final snapshot owns its result handles. Accessors expose Artifacts, Measurements, provenance, Correctness Claims, and Verification Records without exposing Runtime indexes.

Resource exhaustion, an unmet Success Condition, observer stop, and the absence of eligible work are successful completions, not errors. `SuccessConditionsSatisfied` means every Goal has a Success Condition and every one is satisfied; a Goal without one continues until another successful completion reason applies. `NoEligibleWork` means every active Campaign is at a fixed point and the Runtime has no schedulable search, Verification, training, or Knowledge Consolidation work; it must never be used as a guess that uncertain Potential has been exhausted.

## Domain Definition seam

### Capability family

```rust
pub trait DomainDefinition: Sized + Send + Sync + 'static {
    type Artifact: Send + Sync + 'static;
    type Error: Error + Send + Sync + 'static;
    type SeedScope: Send + Sync + 'static;
    type Metric: Copy + Eq + Hash + Send + Sync + 'static;
    type Observation: Send + Sync + 'static;

    type Structure: StructuralProtocol<Self>;
    type Seeds: SeedSource<Self>;
    type Operators: OperatorAlgebra<Self>;
    type Kernel: VerificationKernel<Self>;
    type Measurements: MeasurementSpace<
        Self,
        Metric = Self::Metric,
        Observation = Self::Observation,
    >;

    fn semantic_identity(&self) -> SemanticIdentity;
    fn structure(&self) -> &Self::Structure;
    fn seeds(&self) -> &Self::Seeds;
    fn operators(&self) -> &Self::Operators;
    fn kernel(&self) -> &Self::Kernel;
    fn measurements(&self) -> &Self::Measurements;
}
```

`D` is always statically dispatched. Reflex does not accept `dyn DomainDefinition`, load dynamic domain plugins, or box Artifacts into a universal representation. A Runtime is monomorphized for its installed domain.

The capability Modules expose semantic facts and efficient operations. They do not expose `search`, `rank`, `allocate`, `train`, `admit`, `promote`, or `consolidate` methods.

### Structural Protocol

```rust
pub trait StructuralView {
    type Sort: Copy + Eq + Hash;
    type Constructor: Copy + Eq + Hash;

    fn root_sort(&self) -> Self::Sort;
    fn node_count(&self) -> usize;
    fn node_sort(&self, node: usize) -> Option<Self::Sort>;
    fn node_constructor(&self, node: usize) -> Option<Self::Constructor>;
    fn write_children(&self, node: usize, output: &mut Vec<usize>) -> bool;
    fn write_immediates(&self, node: usize, output: &mut Vec<u64>) -> bool;
    fn dynamic_resident_bytes(&self) -> u64;
}

pub trait StructuralProtocol<D: DomainDefinition>: Send + Sync + 'static {
    type Sort: Copy + Eq + Hash + Send + Sync + 'static;
    type Constructor: Copy + Eq + Hash + Send + Sync + 'static;
    type View<'a>: StructuralView<
        Sort = Self::Sort,
        Constructor = Self::Constructor,
    >
    where
        Self: 'a,
        D: 'a;
    type Scratch: Default + Send + 'static;

    fn schema(&self) -> &StructuralSchema<Self::Sort, Self::Constructor>;

    fn view<'a>(&'a self, artifact: &'a D::Artifact) -> Self::View<'a>;

    fn compose(
        &self,
        constructor: Self::Constructor,
        children: &[&D::Artifact],
        immediates: &[u64],
        scratch: &mut Self::Scratch,
    ) -> Result<D::Artifact, D::Error>;

    fn extract(
        &self,
        artifact: &D::Artifact,
        node: usize,
        scratch: &mut Self::Scratch,
    ) -> Result<D::Artifact, D::Error>;

    fn replace(
        &self,
        artifact: &D::Artifact,
        node: usize,
        replacement: &D::Artifact,
        scratch: &mut Self::Scratch,
    ) -> Result<D::Artifact, D::Error>;

    fn encode_canonical(
        &self,
        artifact: &D::Artifact,
        output: &mut Vec<u8>,
        scratch: &mut Self::Scratch,
    ) -> Result<(), D::Error>;

    fn decode_canonical(
        &self,
        bytes: &[u8],
        scratch: &mut Self::Scratch,
    ) -> Result<D::Artifact, D::Error>;
}
```

Views borrow the domain-specialized Artifact without allocation. Canonical encoding is used at ingress, recovery, checkpointing, and export, not in ordinary hot search.

The schema describes types, composition, binding, and constructor shapes. It is not a universal stored graph. The physical representation remains owned by the Adapter.

### Seed Source

```rust
pub trait SeedSource<D: DomainDefinition>: Send + Sync + 'static {
    type Cursor: Send + 'static;
    type Scratch: Default + Send + 'static;

    fn open(
        &self,
        scope: &D::SeedScope,
    ) -> Result<Self::Cursor, D::Error>;

    fn read_batch(
        &self,
        cursor: &mut Self::Cursor,
        limit: usize,
        output: &mut SeedWriter<'_, D>,
        scratch: &mut Self::Scratch,
    ) -> Result<SeedPage, D::Error>;

    fn encode_scope(
        &self,
        scope: &D::SeedScope,
        output: &mut Vec<u8>,
    ) -> Result<(), D::Error>;

    fn decode_scope(&self, bytes: &[u8]) -> Result<D::SeedScope, D::Error>;

    fn encode_cursor(
        &self,
        cursor: &Self::Cursor,
        output: &mut Vec<u8>,
    ) -> Result<(), D::Error>;

    fn decode_cursor(&self, bytes: &[u8]) -> Result<Self::Cursor, D::Error>;
}
```

Identical scope and cursor state must produce identical Seeds and provenance. A Seed Source may be finite or unbounded. Each emitted Seed includes the prior Verification Record that Reflex must replay before eligibility.

### Operator Algebra

```rust
pub trait OperatorAlgebra<D: DomainDefinition>: Send + Sync + 'static {
    type Operator: Copy + Eq + Hash + Send + Sync + 'static;
    type Application: Send + 'static;
    type Scratch: Default + Send + 'static;

    fn catalog(&self) -> &[OperatorDescriptor<Self::Operator>];

    fn enumerate_legal(
        &self,
        requests: OperatorEnumerationBatch<'_, D, Self::Operator>,
        output: &mut ApplicationWriter<'_, Self::Application>,
        scratch: &mut Self::Scratch,
    ) -> Result<(), D::Error>;

    fn apply_batch(
        &self,
        applications: &[Self::Application],
        output: &mut CandidateWriter<'_, D>,
        scratch: &mut Self::Scratch,
    ) -> Result<(), D::Error>;
}
```

`OperatorEnumerationBatch` contains the Runtime-selected Artifacts, explicit structural locations, and primitive Operators. `enumerate_legal` describes deterministic legal parameterizations only at those locations. It may not rank, prune for predicted value, allocate resources, or create its own search loop. The Runtime chooses which Artifacts, locations, and Operators receive attention and how returned applications are scheduled.

Primitive and Derived Operators produce Candidates only.

### Verification Kernel

```rust
pub trait VerificationKernel<D: DomainDefinition>: Send + Sync + 'static {
    type Claim: Send + Sync + 'static;
    type Evidence: Send + Sync + 'static;
    type Scratch: Default + Send + 'static;

    fn revision(&self) -> KernelRevision;

    fn claim_for_candidate(
        &self,
        seed: &D::Artifact,
        candidate: &D::Artifact,
    ) -> Result<Self::Claim, D::Error>;

    fn verify_batch(
        &self,
        requests: VerificationBatch<'_, D, Self::Claim>,
        output: &mut VerdictWriter<'_, Self::Evidence>,
        scratch: &mut Self::Scratch,
    ) -> Result<(), D::Error>;

    fn replay_batch(
        &self,
        records: VerificationReplayBatch<'_, D, Self::Claim, Self::Evidence>,
        output: &mut ReplayVerdictWriter<'_>,
        scratch: &mut Self::Scratch,
    ) -> Result<(), D::Error>;

    fn encode_claim(
        &self,
        claim: &Self::Claim,
        output: &mut Vec<u8>,
    ) -> Result<(), D::Error>;

    fn decode_claim(&self, bytes: &[u8]) -> Result<Self::Claim, D::Error>;

    fn encode_evidence(
        &self,
        evidence: &Self::Evidence,
        output: &mut Vec<u8>,
    ) -> Result<(), D::Error>;

    fn decode_evidence(&self, bytes: &[u8]) -> Result<Self::Evidence, D::Error>;
}

pub enum Verdict<E> {
    Accepted { evidence: E },
    Refuted,
    Unknown,
}
```

Only the Runtime Controller can convert `Accepted` into `VerifiedCandidate<D>` or `VerifiedArtifact<D>`; their constructors are private to the `reflex` crate. No Operator, Measurement, model, or caller can manufacture verified state.

`Refuted` and `Unknown` are ordinary results. They are recorded in the Experience Ledger and do not abort a Session.

### Measurement Space

```rust
pub trait MeasurementSpace<D: DomainDefinition>: Send + Sync + 'static {
    type Metric: Copy + Eq + Hash + Send + Sync + 'static;
    type Observation: Send + Sync + 'static;
    type Scratch: Default + Send + 'static;

    fn schema(&self) -> &[MeasurementDescriptor<Self::Metric>];

    fn measure_batch(
        &self,
        artifacts: VerifiedBatch<'_, D>,
        environment: &MeasurementEnvironment,
        output: &mut MeasurementWriter<'_, Self::Metric, Self::Observation>,
        scratch: &mut Self::Scratch,
    ) -> Result<(), D::Error>;

    fn compare(
        &self,
        metric: Self::Metric,
        left: &Self::Observation,
        right: &Self::Observation,
    ) -> Result<MetricOrdering, Incomparable>;

    fn environments_compatible(
        &self,
        metric: Self::Metric,
        left: &MeasurementEnvironment,
        right: &MeasurementEnvironment,
    ) -> bool;

    fn within_tolerance(
        &self,
        metric: Self::Metric,
        left: &Self::Observation,
        right: &Self::Observation,
        tolerance: &Self::Observation,
    ) -> Result<bool, Incomparable>;

    fn encode_observation(
        &self,
        metric: Self::Metric,
        observation: &Self::Observation,
        output: &mut Vec<u8>,
    ) -> Result<(), D::Error>;

    fn decode_observation(
        &self,
        metric: Self::Metric,
        bytes: &[u8],
    ) -> Result<Self::Observation, D::Error>;
}

pub enum MetricOrdering {
    Less,
    Equal,
    Greater,
}
```

Measurement occurs after Verification. The writer binds every Observation to its descriptor revision and supplied environment. `environments_compatible` must pass before Reflex calls `compare` or `within_tolerance`; incompatible prior observations are retained but remeasured or excluded from that comparison. Missing, failed, noisy, or environmentally incompatible Measurements remain explicit and cannot alter correctness. `compare` orders raw observations; the Objective's `Direction` determines which is better. `within_tolerance` gives Preference a domain-correct interpretation without requiring Reflex to do arithmetic on opaque Observations. Comparison is partial and the interface does not require a global scalar.

### Capability performance contract

All five capability Modules obey these rules:

- Operations are batch-oriented.
- Input borrows do not escape the call.
- Output is written into Runtime-owned reusable writers.
- Temporary mutable state lives in per-worker scratch.
- Adapters do not spawn threads or retain a separate worker pool. A Verification Kernel may instead own pinned local domain-native worker processes under ADR 0048; each batch receives a Runtime allowance and reports external resource usage.
- Identical inputs and declared environment produce identical semantic outputs; empirical Measurements carry environment provenance.
- Panics are not caught. A panic or hang is a process failure, and restart recovers from durable state.
- A batch-level returned error terminates the Session with a typed error; item-level rejection is represented in output.
- Every domain-defined value that enters a Domain Bundle has a canonical encoding owned by its semantic capability. Rust enum discriminants, memory addresses, and implementation-layout bytes are never persisted as identities.
- Operator, Metric, Sort, and Constructor descriptors carry stable canonical symbol IDs. Bundles persist those IDs, never the in-process Rust values used for fast dispatch.

## Runtime implementation

The external operation deepens the autonomous stack into one Module:

```text
ImprovementRequest
        │
        ▼
validate Goal, Seed Scope, Resource Envelope, and Bundle
        │
        ▼
replay Seed and imported Artifact Verification Records
        │
        ▼
construct Campaigns pinned to Knowledge + Model Revisions
        │
        ▼
Search Engines ──► Opportunities ──► Primitive/Derived Operators
        │                                      │
        │                                      ▼
        │                                  Candidates
        │                                      │
        │                                      ▼
        │                             Verification Kernel
        │                                      │
        │                                      ▼
        │                             Verified Candidates
        │                                      │
        │                                      ▼
        │                              Measurement Space
        │                                      │
        └──────────────────────────────► Admission
                                               │
                         ┌─────────────────────┼────────────────────┐
                         ▼                     ▼                    ▼
                  Pareto Frontier       Search Frontier      Experience Ledger
                         │                     │                    │
                         ▼                     ▼                    ▼
                     observer             allocation       training/consolidation
```

### Private Modules

The initial implementation contains these private Modules inside `reflex`:

- `controller`: epochs, hard invariants, Resource Envelope accounting, completion, observer delivery.
- `campaign`: Agenda Graph construction, Seed portfolio, pinned revision lifetimes.
- `search`: enumerative/best-first, stochastic mutation, and bounded equality-saturation engines.
- `verification`: batching, cache, Verification Record construction and replay.
- `frontier`: Pareto Frontier, Search Frontier, and Admission.
- `knowledge`: Discovery Graph, indexes, Derived Operators, Knowledge Consolidation, revisions.
- `experience`: append-only Experience Ledger and Training Target derivation.
- `learning`: Bootstrap Revision, FTRL/linear heads, challenger training, selection, promotion.
- `memory`: segmented arenas, hot columns, interning, byte accounting, compaction.
- `durability`: sealed checkpoint segments, recovery tail, bundle codec, atomic publication.

These are implementation organizations, not consumer-facing seams. Private traits are introduced only where multiple implementations exist now:

- `SearchEngine`: enumerative, stochastic, and equality-saturation implementations.
- `DecisionModel`: Bootstrap and learned implementations.
- `Scheduler`: throughput and deterministic experimental implementations.
- `BundleStore`: atomic filesystem and in-memory test implementations.
- `ResourceMeter`: production monotonic and deterministic experimental implementations.

Other private Modules use concrete types until a second implementation makes a seam real.

The Runtime never creates another Optimization Goal. Campaign construction expands the Agenda Graph with formal Obligations mechanically induced by Operator applications or the Verification Kernel, and with Emergent Opportunities discovered from verified derivations, shared structure, reuse, or missing links. It connects those nodes back to one or more caller Goals and allocates by predicted Potential. Milestones exist only when supplied by the caller or an explicitly selected domain Goal template. This gives Reflex autonomous intermediate work without allowing it to invent what is valuable.

## Ownership and memory

### IDs and arenas

Hot Runtime relationships use typed `u32` IDs into segmented arenas:

```rust
#[repr(transparent)]
struct ArtifactId(u32);

#[repr(transparent)]
struct OpportunityId(u32);

#[repr(transparent)]
struct ExperienceId(u32);
```

IDs are never reused within one live revision. Knowledge Consolidation may remap compacted indexes only when constructing a new immutable Knowledge Revision, retaining provenance links to canonical content digests.

Variable-sized payloads occupy append-only byte slabs. Frequently scanned fields use separate contiguous columns: domain Operator, structural size, semantic digest prefix, status, feature offset, resource cost, visit count, and prediction moments. Layout is selected by measurement, but pointer-rich object graphs are not the default.

### Stored verified state

The Runtime owns domain Artifacts in arenas. Public verified handles are immutable and cannot expose mutable arena storage:

```rust
pub struct VerifiedArtifact<D: DomainDefinition> {
    inner: Arc<VerifiedArtifactRecord<D>>,
}
```

The private record binds the domain Artifact to its Seed, Correctness Claim, assumptions, Verification Kernel revision, evidence, provenance, and Measurements. `Arc` exists at retained-result granularity, not per Candidate or graph edge.

### Resource accounting

Every Runtime-owned allocation is charged before growth against the Resource Envelope. Arena segments, hash tables, queues, verification caches, model state, scratch buffers, checkpoint staging, and observer exports are included.

Domain work executes in-process by default. A Domain Definition can violate its contract by allocating or running beyond the requested batch, so installed in-process code remains trusted to obey batch and scratch limits. A Verification Kernel using the ADR 0048 process exception receives an explicit remaining allowance; the Runtime reserves its declared overlap before dispatch and charges measured child CPU and peak resident memory afterward. Exceeding the allowance terminates that worker and becomes a Session failure recoverable from durable state.

## Parallel execution

One fixed worker set owns the CPU allocation. Each worker owns:

- a local work deque;
- phase-local arenas and reusable buffers;
- capability scratch values;
- local deduplication staging;
- local Experience and resource counters.

Workers prefer local LIFO work for cache locality and steal batches when idle. Results flush to sharded global indexes in batches. The Runtime avoids an atomic or mutex operation per Candidate.

The Runtime Controller serializes epochs, Admission, Pareto changes, revision publication, and observer calls. It does not execute Candidate-scale work.

Each admission epoch is represented by an internal `EpochTransition`. Artifact, Search Frontier, and Experience Ledger consequence mutations remain staged until the restart-complete checkpoint is durable and observer export is admissible. Dropping an uncommitted transition structurally restores the prior state, so refusal paths cannot omit rollback work.

Search, Verification, training, and Knowledge Consolidation share the same compute-lane budget and obey Protected Allocations. No learned or domain Module owns another pool. Pinned Verification workers authorized by ADR 0048 consume lanes from that same budget rather than creating nested parallelism.

Production scheduling may be nondeterministic. The internal Experimental Harness uses fixed partitions, predetermined random streams, and deterministic reductions through the private Scheduler and ResourceMeter seams while exercising the same Improvement Session implementation.

## Revisions and learning

Each Campaign holds immutable references to exactly one Knowledge Revision and one Model Revision. Campaign execution never observes hot-swapped learned state or indexes.

The private Goal Evaluator is the one operational interpreter of a Goal Set. Validation, stable Goal IDs, Constraint eligibility, tiered Preference ordering, per-Goal Pareto membership, affected-Goal reporting, Success Conditions, and canonical checkpoint encoding cross this seam. It does not scalarize Preference and does not use Pareto membership as Search Frontier investment policy.

The Bootstrap Revision is a valid DecisionModel implementation using deterministic structural cost, novelty, frontier age, and exploration rules. It runs through the same prediction and allocation path as learned revisions.

Potential remains typed inside the model and allocator:

```rust
struct PotentialForecast {
    immediate_improvement: HorizonForecast,
    useful_descendants: HorizonForecast,
    cross_goal_leverage: HorizonForecast,
    compression_value: HorizonForecast,
    novelty: HorizonForecast,
    verification_cost: CostForecast,
    dead_end_risk: ProbabilityForecast,
}
```

Each forecast carries calibration and uncertainty, not only a point estimate. The Runtime Controller interprets the fields against Goal Preference, current frontier state, protected allocations, and remaining resources. Neither training nor allocation collapses this record into a permanent universal reward.

Training consumes a versioned Replay Corpus and produces an immutable challenger. The first learned challenger uses purpose-built Rust FP32 FTRL/linear heads for Operator selection, cost prediction, and typed Potential outcomes. No generic tensor framework is foundational.

Operational Selection uses a rotating Selection Corpus. Passing challengers are atomically promoted for future Campaigns; incomparable challengers may become Specialist Revisions. Scientific Corpora are inaccessible to production search, training, tuning, and promotion code paths.

Knowledge Consolidation similarly produces immutable challenger Knowledge Revisions. Derived Operators become immediately eligible for initial exploration after verified consolidation, then ordinary evidence governs later allocation or deactivation.

## Durability and Domain Bundles

All hot work is memory-resident. Search workers never await filesystem operations.

Mutable Runtime state appends to generation-local segments. A short checkpoint barrier seals completed segments and hands them to the durability thread; workers continue on new segments. The durability implementation writes content-addressed canonical records and a recovery tail, not a byte dump of process memory.

A Domain Bundle contains:

- Semantic Identity and format versions;
- canonical domain Artifacts and Verification Records;
- Knowledge and Model Revisions;
- Experience Ledger and corpus-role state;
- provenance and derivation relationships;
- Resource and Measurement environment metadata;
- recovery state required to resume autonomous improvement.

It is data-only and never contains executable Rust code.

Bundle ownership is split by depth, not duplicated by caller. The unpublished canonical-format Module owns magic, versions, segment ordering, framing, and checksums. The private restart-complete codec owns the semantic mapping between Runtime state and those segments, including recovery validation and revision identity. The Runtime Controller asks that codec only to recover or seal; the Experimental Harness may replace data segments for ablation through the same canonical framing without gaining access to Runtime orchestration.

The Experience Ledger owns immutable attempt observations, delayed consequences, Measurement observations, canonical encoding, resident accounting, and reproducible projections for Model Revision learning and Knowledge Consolidation. Runtime phases do not maintain parallel representations of an attempt.

Hardware-specific packed weights, hash-table capacity, pointer values, worker queues, and transient indexes are rebuildable caches and do not enter the portable canonical state.

Import first validates format and Semantic Identity. Verification replay may be incremental, but an imported Artifact remains ineligible for search, result retention, or training until its record has replayed successfully through the installed Verification Kernel.

At Session completion, the Runtime synchronously seals all required state and publishes the target file atomically. A publication failure returns an error and leaves the previously published bundle intact.

## Failure semantics

```rust
pub enum SessionError<E> {
    InvalidRequest(RequestError),
    InvalidGoal(GoalError),
    InvalidSeed(SeedError),
    IncompatibleBundle(BundleCompatibilityError),
    CorruptBundle(BundleCorruptionError),
    Domain(E),
    Durability(DurabilityError),
    Resource(ResourceError),
}
```

Expected Candidate rejection, `Unknown` Verification, unavailable Measurement, search exhaustion within a Campaign, and failed challenger promotion are recorded outcomes rather than errors.

Domain or Runtime panics are not converted into `SessionError`. Panics, hangs, allocator aborts, and memory corruption fail the process. The next process reconstructs from the last complete checkpoint and recovery tail.

No error path may publish a partially written Domain Bundle or mark a Candidate verified without an accepting kernel verdict.

## Test surfaces

The Improvement Session interface is the primary test surface. Integration tests run real small Domain Definition Adapters through `improve` and assert on verified results, observer ordering, resource usage, bundle contents, and recovery—not private Runtime state.

The Domain Definition seam is exercised first by `reflex-bitvec`; it is not mocked into dozens of callback tests. Each capability implementation retains focused tests for its own semantic laws:

- canonical encode/decode round trips;
- deterministic Seed replay;
- Structural Protocol consistency;
- Operator application well-formedness;
- Verification acceptance/refutation and evidence replay;
- Measurement comparison, tolerance laws, and environment compatibility.

The `reflex` crate uses the private in-memory BundleStore and deterministic Scheduler only in internal tests. Public integration tests use temporary local files and the production atomic filesystem implementation.

Performance tests measure complete Improvement Sessions in addition to kernels. A faster evaluator or model is not an improvement if feature extraction, cache disruption, scheduling, or training reduces equal-budget Pareto progress.

Scientific claims about Reflex use the same public Improvement Session path under the private Experimental Harness. Experiment Specifications, sealed corpora, equal Resource Envelopes, independent process-level replicates, competitive baselines, causal ablations, predeclared analysis, and complete reporting follow [`docs/EXPERIMENTS.md`](./EXPERIMENTS.md). None of those controls appear in the consumer or Domain Definition interfaces.

Isolated Rust `unsafe` kernels require:

- a safe Rust reference implementation;
- differential tests over generated inputs;
- fuzzing;
- Miri-compatible coverage where applicable;
- a measured material end-to-end gain.

## `u8` Reference Domain

`reflex-bitvec` is a production Adapter, not a toy interface. Its initial semantic family is fixed-width expression DAGs parameterized by bit width and input arity. The first confirmed corpus uses unary `u8` functions so complete equivalence requires only 256 inputs; binary arity is admitted only after truth-table layout and throughput benchmarks establish a viable Campaign budget.

The Adapter supplies:

- a compact indexed expression DAG Artifact;
- sorts for values and typed immediates;
- constructors for inputs, constants, wrapping arithmetic, bitwise operations, shifts, rotates, and selection operations;
- Primitive Operators for typed construction, replacement, reassociation, constant transformation, and known rewrites;
- deterministic generated and curated Seed Sources;
- exhaustive packed evaluation as its Verification Kernel;
- canonical expression and Verification Record encoding;
- Measurements including node count, depth, live temporaries, encoded bytes, evaluator operations, and environment-pinned elapsed performance.

Structural Measurements are exact. Elapsed performance is empirical and never part of Verification.

The Semantic Identity includes bit width, arity, operator semantics, shift behavior, canonical encoding, and Verification Kernel revision.

## First production vertical slice

The first implementation slice is deliberately end to end:

1. Create the workspace and the final public `improve` and Domain Definition interfaces.
2. Implement the unary `u8` Adapter, canonical encoding, exhaustive Verification Kernel, and Measurements.
3. Implement the Bootstrap Revision and one bounded enumerative/best-first SearchEngine through the private search seam.
4. Build Candidate → Verification → Measurement → Admission → Pareto update flow.
5. Enforce CPU, memory, elapsed, verifier-call, and durable-byte accounting.
6. Write the Experience Ledger, immutable initial Knowledge/Model Revisions, checkpoint segments, and restart-complete Domain Bundle.
7. Kill at randomized checkpoint phases, resume, replay Verification, and continue improvement.
8. Establish session-level throughput and memory baselines before adding another search engine or learned challenger.
9. Add online FTRL heads, revisable Training Targets, Replay and rotating Selection Corpora, challenger comparison, automatic promotion, rollback, and Specialist Revisions.
10. Add Knowledge Consolidation, verified Derived Operators, immediate bounded exploration, evidence-governed allocation, and live-working-set compaction.
11. Run the preregistered causal gate: on untouched Seeds and equal CPU and memory budgets, learned Reflex must beat its Bootstrap baseline, with separate ablations for cross-Seed knowledge, learned allocation, and Derived Operators.

The early steps form a runnable vertical spine, but the production Reference Domain is not complete until all eleven steps pass. Every step uses final seams and durable formats. Temporarily inactive behavior is represented by valid production states—such as the Bootstrap Revision—not alternative interfaces or disposable implementations.

## Production build and measurement profiles

Production binaries use optimization level 3, fat link-time optimization, one code-generation unit, abort-on-panic behavior, and no incremental compilation. `production` strips symbols; `profiling` retains line tables with otherwise identical optimization settings. Host-native builds are explicit and reproducible on the recorded host through `cargo run --release -p xtask -- build-native`, which writes only to `target/native` and applies `-Ctarget-cpu=native`. Reflex source remains safe Rust; the compiler may autovectorize for the selected target.

`perf-smoke` is the bounded development diagnostic. It records the embedded build profile and flags, session wall and process CPU time, externally sampled process-tree CPU time and peak resident memory, semantic outcome, and recovery validity. It permits a dirty tree and labels its report development-only. It must never be presented as confirmation evidence or replace a frozen equal-budget protocol.

The private Runtime phase recorder is disabled by default and cannot be selected through the consumer or Domain Definition interfaces. Repository tooling activates it in child processes only through an internal environment variable. It takes coarse timestamps around setup, generation, selection, Verification, Measurement/Admission, consolidation, training, and finalization, while hot loops contribute only batch aggregate counters. `instrumentation-overhead` alternates disabled and enabled assignments in paired order and rejects median session-wall or process-CPU overhead of 1% or more.

## Performance gates before expansion

Before adding binary arity, Lean, Wrela, a tensor framework, reduced precision, or a custom scheduler, measure:

- safe scalar versus autovectorized portable and host-native Rust evaluation;
- AST/DAG representations and hot-field layouts;
- batch sizes across L1/L2/L3 working sets;
- ordinary allocation versus per-worker arenas;
- global versus local/batched work queues;
- sharded versus centralized interning and Admission;
- Bootstrap versus FTRL/linear decision models;
- checkpoint interference and recovery cost;
- one-, two-, four-, and eight-core scaling.

Adoption requires material equal-budget Improvement Session gains, not isolated microbenchmark wins.

## Explicit non-goals

- No synthesis from natural-language or unsolved specifications.
- No externally invented Optimization Goals.
- No global scalar fitness.
- No learned correctness authority.
- No dynamic domain plugins or stable Rust ABI.
- No foreign-language code, runtime, or native ML dependency.
- No network process or remote registry.
- No automatic mutation of consumer projects.
- No public search-engine, trainer, scheduler, persistence, or experimental-control interface.
