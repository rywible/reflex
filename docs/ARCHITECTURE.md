# Reflex Architecture

Status: accepted architecture for initial implementation

This document defines the intended Rust interfaces, ownership model, Runtime shape, performance contracts, and first production implementation slice for Reflex. Domain language is defined in [`CONTEXT.md`](../CONTEXT.md); architectural rationale lives in [`docs/adr/`](./adr/).

## Architectural thesis

Reflex has two public seams:

1. Consumers invoke one deep Improvement Session operation.
2. Domain authors implement five explicit semantic capability Modules.

Everything concerned with autonomous execution remains private Runtime implementation. In particular, callers cannot select or orchestrate Campaigns, search engines, learned models, training cadence, Admission, Knowledge Consolidation, Operational Promotion, scheduling, or persistence internals.

The Runtime Controller delegates economic judgment to one private `IntelligenceCore`. That Module owns the Model Ecology, typed Investment market, causal attribution, Knowledge Compiler and active Knowledge Revision, and bounded Runtime Policy challengers. It can propose a complete next transition but cannot establish correctness, spend unreserved resources, or publish durability; those authorities remain in the Runtime Controller. A Core transition may select the next active revision, but no Campaign or Cohort observes it until Runtime commits and durably publishes that transition at a legal boundary.

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
    Fork {
        source: PathBuf,
        target: PathBuf,
    },
}
```

Every public Improvement Session produces a durable Domain Bundle. `source` and `target` may be equal; publication still uses a new sealed file followed by atomic replacement. Ephemeral Campaigns may exist privately, but ephemeral Sessions do not weaken the public durability contract.

`Resume` continues exact retained search state. If its `source` contains an interrupted Session, the Goal Set, Seed Scope, Semantic Identity, and original Resource Envelope must canonically match the request; already consumed resources remain consumed and recovery continues from the tail. A mismatch never silently discards interrupted work.

`Fork` starts new search from the verified Artifacts, Experience, Knowledge Revision, and Model Revision in a completed `source`, under the new request's Seed Scope, goals, and fresh Resource Envelope. It deliberately discards source Search Frontier, deferred Candidates, and pending-parent tail. Forking an interrupted Bundle is incompatible rather than an implicit loss of restart-complete work.

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

Canonical materialization is governed by explicit Domain allocation contracts. The Structure declares each Artifact's exact canonical output length, encoding scratch bound, and retained dynamic ownership; the Kernel does the same for Claims and Evidence; the Measurement Space declares batch scratch and both the expected and observed dynamic ownership of every Observation. Runtime admits those bytes before allocating, preallocates exact output capacities, and rejects a Domain whose actual output, scratch, or retained Observation ownership differs from its declaration. Claim, Evidence, Observation, environment, record-`Arc`, and canonical payload ownership remain part of resident state after admission.

Views borrow the domain-specialized Artifact without allocation. Canonical encoding is used at ingress, recovery, and export, not in ordinary hot search. Materialization caches each verified Artifact's exact immutable canonical Bundle record. Restart sealing therefore sorts borrowed record pointers and copies those bounded bytes; it never invokes domain Structure or Kernel encoding and cannot acquire unreported adapter scratch.

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
    fn resident_bytes(&self) -> u64;
    fn scratch_resident_bytes(&self, output_capacity: usize) -> u64;

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

`OperatorEnumerationBatch` contains the Runtime-selected Artifacts, explicit structural locations, and primitive Operators. `enumerate_legal` describes deterministic legal parameterizations only at those locations. It may not rank or prune for predicted value, allocate resources, or create its own search loop; a domain may use goal-independent retrieval to make the legal prefix relevant under a bounded writer. `resident_bytes` and `scratch_resident_bytes` make retained indexes and maximum per-batch workspace for every Runtime lane part of the Resource Envelope before workers start. The Runtime chooses which Artifacts, locations, and Operators receive attention and how returned applications are scheduled.

Primitive and Derived Operators produce Candidates only. A Candidate may retain Proposal Provenance identifying a verified supporting Artifact, plus bounded Proposal Features describing the support relationship. Both survive into Experience for attribution but establish neither correctness nor value.

### Private Proposal Engine portfolio

The Runtime reaches candidate generation through one private `ProposalEngine` interface. A Proposal Engine consumes eligible parent Artifacts, a bounded request, and engine-specific restart progress; it appends Candidates and reports peak resident demand and truncation. It cannot Verification-check, rank, admit, spend beyond the Runtime allowance, or publish state.

The initial adapters are a Structured Rewrite Engine over the public Operator Algebra and a Derived Operator Engine over the active Knowledge Revision. Their cursors and grammars remain engine-specific rather than being flattened into a universal public protocol. This internal seam permits later solver, proof-state, stochastic, or retrieval engines without making those mechanisms part of the consumer interface. The public Domain Definition remains unchanged until an independent non-rewrite adapter demonstrates which semantic capabilities must actually become optional.

### Verification Kernel

```rust
pub trait VerificationKernel<D: DomainDefinition>: Send + Sync + 'static {
    type Claim: Send + Sync + 'static;
    type Evidence: Send + Sync + 'static;
    type Scratch: Default + Send + 'static;

    fn revision(&self) -> KernelRevision;
    fn worker_requirements(&self) -> VerificationWorkerRequirements;

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
    ) -> VerificationBatchOutcome<D::Error>;

    fn replay_batch(
        &self,
        records: VerificationReplayBatch<'_, D, Self::Claim, Self::Evidence>,
        output: &mut ReplayVerdictWriter<'_>,
        scratch: &mut Self::Scratch,
    ) -> VerificationBatchOutcome<D::Error>;

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

pub struct VerificationAllowance {
    worker_lanes: usize,
    resident_bytes: u64,
    elapsed_time: Duration,
    cpu_time: Duration,
}

pub struct VerificationBatchOutcome<E> {
    report: VerificationBatchReport,
    error: Option<E>,
}

pub struct VerificationBatchReport {
    external_usage: ExternalVerificationUsage,
    worker_failed: bool,
}

pub enum Verdict<E> {
    Accepted { evidence: E },
    Refuted,
    Unknown,
}
```

Only the Runtime Controller can convert `Accepted` into `VerifiedCandidate<D>` or `VerifiedArtifact<D>`; their constructors are private to the `reflex` crate. No Operator, Measurement, model, or caller can manufacture verified state.

`Refuted` and `Unknown` are ordinary results. They are recorded in the Experience Ledger and do not abort a Session.

The default worker requirements are in-process and consume no external lanes. An external Kernel declares pinned worker lanes and resident bytes before the Runtime pool is built. Every batch receives its remaining allowance and returns an outcome even on a domain error, so observed child CPU, wall time, peak resident memory, and worker failure cannot disappear down an error path. Shadow arm reports attribute the worker's measured CPU, elapsed time, and peak resident memory to the arm that dispatched it; paired resource matching therefore compares the complete cost rather than only the controller process. A failed or over-budget external batch terminates the Session only after the charged interrupted checkpoint is durable.

`claim_for_candidate` is deterministic in-process claim derivation and cannot invoke the external authority because it has no allowance. Only `verify_batch` and `replay_batch` may dispatch the pinned workers.

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
- `intelligence`: routed Model Ecology, typed Investment allocation, causal credit, Shadow Campaigns, verified knowledge compilation, and bounded Runtime Policy Revisions.
- `memory`: segmented arenas, hot columns, interning, byte accounting, compaction.
- `durability`: sealed checkpoint segments, recovery tail, bundle codec, atomic publication.

These are implementation organizations, not consumer-facing seams. Private traits are introduced only where multiple implementations exist now:

- `SearchEngine`: enumerative, stochastic, and equality-saturation implementations.
- `Scheduler`: throughput and deterministic experimental implementations.

Other private Modules use concrete types until a second implementation makes a seam real.

`IntelligenceCore` is a deep concrete Module rather than a public extension point. Its small private Interface restores one canonical state, allocates a bounded portfolio without mutation, stages authoritative settlements into an uncommitted transition, and exposes restart-complete checkpoint data. The Runtime Controller knows none of the mechanics of specialist routing, training, causal credit, consolidation, or policy search.

The current restart format carries one authenticated `IntelligenceCore` checkpoint. Its Model Ecology, Knowledge Compiler, and Runtime Policy components are the Model, active Knowledge, and Runtime Policy Revision authorities; a current bundle contains no second learner, Knowledge, or policy payload. Completed legacy bundles may import their standalone FTRL model, Knowledge state, and Runtime Policy exactly once into the Core, after which those legacy payloads are discarded. Interrupted legacy sessions remain incompatible because partial authority migration is not restart-complete.

One cohort follows this authority order:

1. pin the active Knowledge Revision, Model Ecology, and Runtime Policy Revision;
2. construct a bounded Opportunity inventory and shared Feature Bank;
3. let applicable specialists forecast typed Investments;
4. apply immutable Protected Allocations, Goal Preference, and Resource Envelope feasibility;
5. execute one Verification Cohort and bounded auxiliary Investments;
6. route correctness exclusively through the Verification Kernel, then Measurement and Admission;
7. append immutable settlements and later consequences to Experience;
8. stage model, knowledge, causal, and Runtime Policy consequences in one restart-complete transition;
9. pass the staged Core transition to one private prepared-publication primitive, which computes and reserves its allocation-free peak bound before Experience capacity reservation, checkpoint materialization, and both Bundle seals. A proposed read-only Core view supplies the exact checkpoint checksum and component identities without decode/restore. The private codec's bound covers cached canonical payload capacities, pointer indexes, segment compression and shrink overlap, framing ownership, the bounded final output allocation, and the durability handoff; the encoder enforces the admitted output capacity before publication. The primitive transfers the final Bundle allocation to the writer without cloning, crosses the durability barrier, then performs an infallible move-only Core commit; and
10. activate eligible revisions only at their legal boundary.

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

The private record binds the domain Artifact to its Seed, Correctness Claim, assumptions, Verification Kernel revision, evidence, provenance, and Measurements. For Runtime-owned records, `Arc` exists at retained-result granularity, not per Candidate or Discovery Graph edge. A Domain's specialized immutable Artifact representation may share internal structure when its Semantic Identity and canonical encoding remain content-based; ADR 0053 applies that exception to Lean expression subtrees.

### Resource accounting

Every Runtime-owned allocation is charged before growth against the Resource Envelope. Arena segments, hash tables, queues, verification caches, model state, scratch buffers, checkpoint staging, and observer exports are included. Model-Ecology capacity is chosen only after subtracting the fresh Core residency and eight fixed 8-KiB allocator pages (one immutable minimum cohort) for seal/publication scaffolding; exact variable checkpoint and Candidate payloads still pass their separate encoded-byte and live-graph admissions.

Domain work executes in-process by default. A Domain Definition can violate its contract by allocating or running beyond the requested batch, so installed in-process code remains trusted to obey batch and scratch limits. A Verification Kernel using the ADR 0048 process exception receives an explicit remaining allowance; the Runtime reserves its declared overlap before dispatch and charges measured child CPU and peak resident memory afterward. Exceeding the allowance terminates that worker and becomes a Session failure recoverable from durable state.

## Parallel execution

One fixed worker set owns the CPU allocation. Each worker owns:

- a local work deque;
- phase-local arenas and reusable buffers;
- capability scratch values;
- local deduplication staging;
- local Experience and resource counters.

Workers prefer local LIFO work for cache locality and steal batches when idle. Results flush to sharded global indexes in batches. The Runtime avoids an atomic or mutex operation per Candidate.

The Runtime Controller serializes epochs, Admission, Pareto changes, revision publication, and observer calls. It does not execute Candidate-scale work. Candidate Verification is divided into Runtime-sized Verification Cohorts: each cohort is selected, kernel-checked, admitted, and synchronously made restart-complete before allocation is reconsidered. Exact Search Frontier order, deferred Candidates, and newly admitted pending parents are canonical recovery state; a parent is removed from pending state only in the durable transaction that retains its materialized work. Completed Generate, Repair, and Explore actions likewise publish their exact Candidate prefix and represented enumeration progress together. That prefix remains restart-reachable through later refusal paths, while the live epoch continues from the same progress rather than resampling or skipping a Derived-Operator window. A Resource-exhausted completed Session carries that unfinished tail into the next Session's fresh envelope. Thus feedback neither discards unfinished opportunities nor waits for the complete Verification allowance to be spent, and a restart does not infer the frontier or deferred tail from a smaller remaining budget.

Each admission epoch is represented by an internal `EpochTransition`. Artifact, Search Frontier, and Experience Ledger consequence mutations remain staged until the restart-complete checkpoint is durable and observer export is admissible. Dropping an uncommitted transition structurally restores the prior state, so refusal paths cannot omit rollback work.

Search, Verification, training, and Knowledge Consolidation share the same compute-lane budget and obey Protected Allocations. No learned or domain Module owns another pool. Pinned Verification workers authorized by ADR 0048 consume lanes from that same budget rather than creating nested parallelism.

On a multi-claim Search Frontier, bounded primitive-Operator generation gives selected pending parents isolated windows, covers every represented Seed-relative correctness claim when a small window can afford the complete set, and retains every unprocessed parent for the next transaction. Candidate allocation separately reserves one exploration slot for every represented claim without retained Verification Experience when the remaining allowance can afford the complete set. A protected Derived Operator Candidate satisfies that allocation obligation preferentially because it can combine coverage with a verified discovery; learned ranking, Bootstrap ranking, and remaining Derived Operator protection allocate the other cohort slots. Thus an early or prolific Seed cannot prevent another claim from producing any Candidate or erase every observation from that claim, while already covered claims do not repeatedly tax every cohort. Bootstrap and active learned rankings are computed over the complete novel Candidate set before this partition, so protected Candidate Fates retain their counterfactual within-transaction ranks even though protection determines their executed queue and policy rank. When either bounded capacity cannot cover every uncovered claim, active Preference and ordinary enumeration or ranking retain authority instead of an arbitrary partial reservation overriding caller economics.

When fresh Experience first becomes eligible for learning, the Learning Module assigns whole correctness-claim groups to disjoint Replay and Selection Corpora within each training niche. The first eight distinct claims are Replay, the next eight are Selection, and later claims send every fifth case to Selection. A niche must retain at least eight of each before its challenger is eligible. Persisted roles never move from Replay back into Selection; Selection cases rotate into Replay after bounded use and only fresh claims replenish the operational gate.

The private Scheduler is the only owner of Candidate-scale Verification parallelism. In-process kernels receive ordered contiguous sub-batches on the Runtime pool; they do not create nested pools. `REFLEX_INTERNAL_SCHEDULER=throughput` uses surplus ordered chunks and Rayon's local-deque work stealing, while `deterministic` fixes one contiguous partition per Runtime lane. Both reductions preserve request order. External pinned workers receive one whole batch because their adapter owns process dispatch, but their lanes are first subtracted from the same caller-declared worker budget and at least one Runtime controller lane must remain.

Production scheduling may be nondeterministic. The internal Experimental Harness uses fixed partitions, predetermined random streams, and deterministic reductions through the private Scheduler and ResourceMeter seams while exercising the same Improvement Session implementation. Scheduling semantics are pinned by `RUNTIME_REVISION`; a change makes an interrupted Domain Bundle explicitly incompatible instead of silently changing its resumed execution.

## Revisions and learning

Each Campaign holds an immutable Knowledge Revision. One Verification Cohort pins exactly one immutable Model Revision; after the cohort and its learning evidence are restart-complete, an Operational Promotion may atomically change the Model Revision used by the next cohort in the same Campaign. No Candidate-scale work observes a hot-swapped model or index.

The private Goal Evaluator is the one operational interpreter of a Goal Set. Validation, stable Goal IDs, Constraint eligibility, tiered Preference ordering, per-Goal Pareto membership, affected-Goal reporting, Success Conditions, and canonical checkpoint encoding cross this seam. It does not scalarize Preference and does not use Pareto membership as Search Frontier investment policy.

The Bootstrap Revision is a permanent deterministic institution using structural cost, novelty, frontier age, and exploration rules. It runs through the same allocation path as learned revisions, submits an independent complete ranking, and retains a protected allocation that neither a Model Ecology nor Runtime Policy Revision may weaken.

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

A complete Model Revision is a Model Ecology rather than one parameter array. It contains a broad generalist, routed Operator or structural specialists, Verification-cost and repairability critics, Potential and consolidation curators, a router, calibration, competence scopes, evidence lineage, and Bootstrap. Specialists may split on stable residual niches, merge or distill when redundant, retire when they add no causal value, or coexist when incomparable. A canonical Routing Family identifies semantic scope independently from the Feature Schema, so representation-compatible Operators can train and coexist without falsely sharing competence. Ordinary Candidate inference evaluates only a bounded applicable subset; ecology size cannot make every hot prediction scan every retained specialist.

The allocator buys typed Investments, including generation, Verification, repair, Operator exploration, specialist training, revision comparison, Knowledge Consolidation, Shadow Campaigns, and bounded Runtime Policy trials. Bids carry typed outcome, uncertainty, calibration, correlation, and resource vectors. The Runtime applies hard eligibility, Protected Allocations, goal-relative Pareto filtering, Preference tiers, resource efficiency, and deterministic tie-breaking without persisting a universal scalar reward. Every completed action must execute its concrete bounded work: selected generation enumerates its Operator, Repair starts from the exact retained Refuted Candidate and advisory while its verified parent is active, selected training materializes its prepared plan under simultaneous-residency accounting, consolidation executes its selected compiler strategy, and policy allocation materializes the exact selected challenger.

The Experience Ledger retains Investment decisions and settlements as well as Candidate Fates. Action Decision IDs follow generated Candidates through Verification and Admission, while Repair separately retains the Refuted causal parent without replacing the verified parent that governs claim, feature, and Artifact recovery. Later consequence edges may record observed descendants, reuse, compression, enabled Operators, or resources saved. Shared Verification-batch CPU remains batch-attributed on Candidate Fates; an individual settlement does not fabricate a divided CPU duration. Observational counterfactual ranks remain diagnostics. Causal Training Targets require a mechanically induced intervention or a valid paired Shadow Campaign.

A Shadow Campaign forks treatment and control from one restart-complete state with identical Goals, semantic revisions, deterministic streams, scheduling semantics, and resource sub-envelopes; treatment permits one subject and control masks it. Open publication reserves and charges the complete two-arm Verification-request sub-envelope before either arm dispatches. Each arm charges its live graph before growth and removes that graph from the allowance passed to the Verification Kernel. Asymmetric or interrupted pairs are invalidated, and shadow work remains a bounded information Investment rather than Scientific Confirmation. Only a terminal symmetric Contextual Contrast bound to the exact challenged subject can train typed Potential, including downstream descendants, compression, enabled Operators, and CPU saved.

Knowledge Consolidation operates as a verified compiler owned by `IntelligenceCore`: it constructs strategy identities, selects compiler roots, interprets broad and focused mining, mines recurring derivations, failure families, structural abstractions, and successful Operator sequences; emits opaque bounded work plus explicit Obligations; routes every semantic addition through Runtime's installed Verification Kernel; and requires controlled downstream usefulness before promotion. Core derives a conservative peak bound from the exact observation identities and fully expanded primitive or Derived-Operator steps before Runtime reserves execution; Runtime owns no compiler scratch recipe or magic allowance. Runtime returns authoritative typed Investment receipts and settlements through the private reactor seam; it never constructs compiler recipes, gates, reviews, or activation updates. A Completed Consolidate receipt and its exact provisional challenger cross the barrier in one transition, so crash recovery resumes the retained product rather than mining again; later compression credit remains bound to that producing decision. Verification receipt issuance is one authenticated Open transition that is durably published before Kernel dispatch; the terminal settlement is a second published transition. Both cross the same prepared-publication primitive used by ordinary Cohort settlements and consequences, so no proposed Core becomes authoritative before its restart-complete Bundle barrier. Recovery invalidates a retained Open batch without replaying it or releasing its reserved requests. Compression alone is insufficient.

A Runtime Policy Revision is bounded canonical data interpreted below an immutable supervisor. It may eventually advise cohort sizing, routing, exploration above protected floors, shortlist widths, and training, consolidation, shadow, stopping, or reinvestment cadence. Autonomous challengers are restricted to fields that the paired trial actually executes; today that is Verification-cohort size across bounded restart-complete arms, with Bootstrap and Verification floors preserved. It cannot modify Verification authority, Admission, Resource Envelope accounting, durability, corpus isolation, Optimization Goals, or executable Rust. Safe challengers still require operational comparison; source or binary improvements remain External Adoption.

Operational Selection uses a rotating Selection Corpus. Passing Model challengers are atomically promoted for future Verification Cohorts, including later cohorts in the active Campaign; passing Knowledge challengers remain eligible for future Campaigns. Incomparable challengers may become Specialist Revisions. Scientific Corpora are inaccessible to production search, training, tuning, and promotion code paths.

Knowledge Consolidation similarly produces immutable challenger Knowledge Revisions. A concrete consolidation product remains unavailable while provisional, becomes Verified only after every explicit Obligation is discharged by authoritative decisions, and is promoted only after a valid paired Shadow Campaign demonstrates its required downstream consequences. Invalidated causal evidence retracts promotion instead of silently preserving it.

## Durability and Domain Bundles

All hot work is memory-resident. Search workers never await filesystem operations.

Mutable Runtime state appends to generation-local segments. A short checkpoint barrier seals completed segments and hands them to the durability thread; workers continue on new segments. The durability implementation writes content-addressed canonical records and a recovery tail, not a byte dump of process memory.

The private Bundle framing layer deterministically compresses replay-complete Artifact and Experience segments under ADR 0054. Segment checksums, Artifact identities, and revision identities remain defined over the expanded canonical bytes; compression is a physical storage decision and cannot change Verification or Semantic Identity. Recovery bounds expansion from the Resource Envelope before allocation and charges the expanded representation rather than the physical file. Runtime v24 added action provenance to the recovery tail and a canonical causal-repair parent to Experience. Runtime v25 additionally authenticates whether retained Candidate inventory is the complete generation prefix after the action barrier; Resume consumes that phase marker exactly once and proceeds to Selection. Completed older bundles use frozen decoders and migrate absent fields conservatively, while interrupted legacy execution remains incompatible. Every imported Artifact replays through the Verification Kernel. Accepted Experience outcomes additionally replay one Candidate at a time before they may remain positive training evidence; Refuted and Unknown observations are canonically validated but do not consume Verification because they cannot establish correctness or bypass future Candidate Verification. Current Core-owned Verified and Promoted Knowledge also supplies an authenticated, obligation-ordered recovery manifest. Runtime reserves its complete reconstruction and Kernel-batch working set before allocation, charges its exact request count before Seed reading, reconstructs every witness from retained support, and requires indexed evidence-bound acceptance under the installed Kernel before Resume or Fork can observe that Knowledge.

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

At Session completion, the Runtime reserves the codec-owned resident seal plan before materializing any logical segment or final output, synchronously seals all required state, and publishes the target file atomically. Session-only interruption replacement performs the same allocation-free admission scan before allocating its bounded output. Once bytes cross the durability barrier, resource admission is complete: a later resource refusal cannot strand a published transition. A publication failure returns an error and leaves the previously published bundle intact.

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

Before any resource-intensive development Campaign, a private Directional Harness must pass the same production implementation through progressively stronger cheap gates: exhaustive cohort-transition and crash schedules; finite synthetic verified worlds with known specialist niches, causal value, valid and invalid abstractions, and safe and unsafe Runtime Policy challengers; fixed-Experience determinism and promotion checks; repository-tool and Reference Domain unit suites; default-production and all-feature strict linting; exact checkpoint, Knowledge Consolidation, current Knowledge, and legacy-v20 migration gates; BitVec microcampaigns; and a tiny domain-native Verification smoke run. Its passed receipt content-addresses every Git-visible tracked or untracked source path, normalized mode, symlink target or file contents, and tracked deletion. It also binds the canonical commands, controlled environment, per-gate timeouts, resident limits, and global deadline; a successful receipt requires consistent zero exits and no timeout or resident-limit breach. The Harness passes each child only the smaller of its gate timeout and the remaining global deadline, and a run beyond that deadline cannot pass. Confirmation and large-development parent entrypoints reject an absent, stale, malformed, or plan-mismatched receipt before launching children. These gates may falsify the implementation but cannot establish a substantive capability claim. Only a frozen equal-budget real Campaign may do that.

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

The non-default `internal-experiments` build feature exposes repository tooling to private retained-Experience analysis without extending the normal consumer surface. The model-feature gate decodes a completed Domain Bundle through the same compatibility and integrity checks as recovery, replays canonical parent and Candidate structure in-process, preserves claim-level Replay/Selection roles, consequences, example order, FTRL hyperparameters, and training epochs, and requests no new Verification labels. It must reproduce the persisted baseline champion before a paired representation result is admissible.

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

## Lean proof-optimization domain

`reflex-lean` is the second production Domain Definition. It pins the final pre-2025 mathlib default-branch commit and its exact Lean compiler, imports only that environment at trust level zero, and keeps search, ranking, learning, persistence, and resource control in safe Rust. A persistent local Lean process owns only domain-native indexing and kernel Verification.

A Lean Artifact is a replay-complete elaborated core declaration: declaration name and universe parameters, proposition, actual proof body, direct statement-and-proof dependencies, allowed axioms, and the complete environment and kernel-contract identity. Candidates retain the Seed declaration identity. Verification checks that both claims are well-formed, asks the official kernel for definitional equality between Candidate and Seed propositions, kernel-checks the proof, and rejects sorry, new axioms, unsafe or partial dependencies, unknown constants, and unknown universe parameters. Under ADR 0055, a Lean corpus independently identifies Seed entries and Verified library Artifacts available to Primitive Operators; merely installing an evaluation Seed never exposes its proof as reusable library knowledge.

The binary declaration catalog includes the type, dependency edges, declaration kind, and local eligibility of every declaration exposed by the imported environment. A declaration enters the eligible subgraph only when all transitive dependencies are present and eligible. The compact catalog is checksummed and environment-bound; normal restoration loads it into RAM rather than rescanning Mathlib.

Lean structural views follow the Runtime's canonical post-order convention: every child precedes its parent and the root is last. Primitive Operators cover indexed proof substitution, directed application and composition, local rewriting, common-subproof factoring, structural anti-unification transfer, eta abstraction, beta/zeta normalization, and syntactic instantiation of general theorems. Proof substitution indexes the verified Operator library in RAM: exact proposition matches precede a deterministic symbol-independent structural retrieval tier, with corpus order as the complete fallback. Contiguous sparse postings are merged by bounded cursors into a top-k heap, so query workspace scales with the Candidate window and structural-token bound rather than library size; the index and per-lane worst-case scratch are charged to the Resource Envelope. Retrieval is goal-independent proposal enumeration, not Runtime ranking, and never establishes equivalence; every result remains a Candidate until the Lean kernel accepts it. Stable donor provenance and relation features survive every Candidate Fate into Experience.

The worker's configured memory is a hard Linux address-space ceiling, not an estimate. Because resident memory cannot exceed that ceiling, the Runtime conservatively charges the ceiling to the Resource Envelope on success and failure. Formal Artifacts can differ by millions of expression nodes, so fetch and Verification traffic is paged into single-item protocol transactions; request order and the batch deadline remain unchanged while the worker never retains a batch-wide decoded payload. The fixed Development suite separately gates cold restore and clean replay at 60 seconds, warm first kernel-certified Proof Collapse at 1 second p50 and 5 seconds p95, and exact proof-node improvement on five historical cases. Development timing never constitutes Scientific Confirmation.

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

Resource-intensive experimental children run inside a host-safety envelope outside the public Resource Envelope. On the supported Linux harness this is a kernel-enforced memory scope that includes descendant processes and memory-backed files, plus explicit CPU affinity that leaves registered host resources unavailable to the experiment. The child authenticates and records the effective boundaries before constructing a Domain Definition. A harness refuses to start unless both its experimental allocation and its host reserve are currently available; process-tree sampling remains evidence and an early stop, not the hard isolation authority.

`causal-development-performance` replays one consumed v5 corpus through Full and Bootstrap for bounded regression work. It rejects changes unless deterministic Measurements, Pareto artifact identity, Knowledge Revision, Model Revision, and recovery remain exact, Full is at least twice as fast as historical v5 Full, and Full beats historical v5 Bootstrap wall time. Consumed data remains Development Corpus: passing this gate is never confirmation evidence.

`verification-scaling` is the bounded multicore development gate. It runs the production Improvement Session and Runtime scheduler over a fixed candidate-heavy BitVec corpus in isolated one-lane and seven-lane processes, leaving the eighth logical CPU outside the experiment for host control and recovery. It records the private Verification-Kernel phase plus whole-process CPU and requires identical semantic outcomes, at least `6x` median phase-wall speedup, and no more than `20%` excess median aggregate CPU. Its report freezes the `[1, 7]` treatments, dirty-tree state, and host state and is performance evidence, never Scientific Confirmation.

Mechanical hot-path changes cache only derivable private state. Cached correctness-claim bytes remain the exact equality authority while their digest is only a grouping index; cached FTRL weights and square roots remain excluded from canonical Model Revision encoding. Bounded selection must reproduce the exact prefix of the complete total order. This keeps speedups from silently weakening correctness or changing durable identity.

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
