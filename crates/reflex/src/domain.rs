use std::error::Error;
use std::hash::Hash;

use crate::measurement::MeasurementSpace;

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
    type Measurements: MeasurementSpace<Self, Metric = Self::Metric, Observation = Self::Observation>;

    fn semantic_identity(&self) -> SemanticIdentity;
    fn structure(&self) -> &Self::Structure;
    fn seeds(&self) -> &Self::Seeds;
    fn operators(&self) -> &Self::Operators;
    fn kernel(&self) -> &Self::Kernel;
    fn measurements(&self) -> &Self::Measurements;
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SemanticIdentity(String);

impl SemanticIdentity {
    #[must_use]
    pub fn new(identity: impl Into<String>) -> Self {
        Self(identity.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct KernelRevision(pub u64);

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SymbolId(String);

impl SymbolId {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub trait StructuralView {
    type Sort: Copy + Eq + Hash;
    type Constructor: Copy + Eq + Hash;

    fn root_sort(&self) -> Self::Sort;
    fn node_count(&self) -> usize;
    fn node_sort(&self, node: usize) -> Option<Self::Sort>;
    fn node_constructor(&self, node: usize) -> Option<Self::Constructor>;
    /// Replaces `output` with the child node indexes for `node`.
    fn write_children(&self, node: usize, output: &mut Vec<usize>) -> bool;
    /// Replaces `output` with the canonical unsigned immediates for `node`.
    fn write_immediates(&self, node: usize, output: &mut Vec<u64>) -> bool;
    /// Heap storage owned by the artifact but not included in its inline size.
    fn dynamic_resident_bytes(&self) -> u64;
}

#[derive(Clone, Debug)]
pub struct ConstructorDescriptor<S, C> {
    constructor: C,
    symbol: SymbolId,
    result_sort: S,
    child_sorts: Vec<S>,
    immediate_count: usize,
    child_binding_depths: Vec<u32>,
}

impl<S, C> ConstructorDescriptor<S, C> {
    /// Creates a structural constructor descriptor.
    ///
    /// # Panics
    ///
    /// Panics when `child_sorts` and `child_binding_depths` do not describe the
    /// same number of child positions.
    #[must_use]
    pub fn new(
        constructor: C,
        symbol: SymbolId,
        result_sort: S,
        child_sorts: Vec<S>,
        immediate_count: usize,
        child_binding_depths: Vec<u32>,
    ) -> Self {
        assert_eq!(child_sorts.len(), child_binding_depths.len());
        Self {
            constructor,
            symbol,
            result_sort,
            child_sorts,
            immediate_count,
            child_binding_depths,
        }
    }

    pub fn constructor(&self) -> &C {
        &self.constructor
    }

    pub fn symbol(&self) -> &SymbolId {
        &self.symbol
    }

    pub fn result_sort(&self) -> &S {
        &self.result_sort
    }

    pub fn child_sorts(&self) -> &[S] {
        &self.child_sorts
    }

    pub fn immediate_count(&self) -> usize {
        self.immediate_count
    }

    pub fn child_binding_depths(&self) -> &[u32] {
        &self.child_binding_depths
    }
}

#[derive(Clone, Debug)]
pub struct StructuralSchema<S, C> {
    pub sorts: Vec<(S, SymbolId)>,
    pub constructors: Vec<ConstructorDescriptor<S, C>>,
}

pub trait StructuralProtocol<D: DomainDefinition>: Send + Sync + 'static {
    type Sort: Copy + Eq + Hash + Send + Sync + 'static;
    type Constructor: Copy + Eq + Hash + Send + Sync + 'static;
    type View<'a>: StructuralView<Sort = Self::Sort, Constructor = Self::Constructor>
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

pub type ClaimOf<D> = <<D as DomainDefinition>::Kernel as VerificationKernel<D>>::Claim;
pub type EvidenceOf<D> = <<D as DomainDefinition>::Kernel as VerificationKernel<D>>::Evidence;

pub struct VerificationRecord<D: DomainDefinition> {
    pub claim: ClaimOf<D>,
    pub evidence: EvidenceOf<D>,
    pub kernel_revision: KernelRevision,
}

pub struct Seed<D: DomainDefinition> {
    pub artifact: D::Artifact,
    pub verification: VerificationRecord<D>,
    pub provenance: Vec<u8>,
}

pub struct SeedWriter<'a, D: DomainDefinition> {
    output: &'a mut Vec<Seed<D>>,
    remaining: usize,
    overflowed: bool,
}

impl<'a, D: DomainDefinition> SeedWriter<'a, D> {
    pub fn new(output: &'a mut Vec<Seed<D>>) -> Self {
        Self {
            output,
            remaining: usize::MAX,
            overflowed: false,
        }
    }

    pub(crate) fn with_limit(output: &'a mut Vec<Seed<D>>, limit: usize) -> Self {
        Self {
            output,
            remaining: limit,
            overflowed: false,
        }
    }

    pub fn push(&mut self, seed: Seed<D>) {
        if self.remaining == 0 {
            self.overflowed = true;
        } else {
            self.remaining -= 1;
            self.output.push(seed);
        }
    }

    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SeedPage {
    pub emitted: usize,
    pub exhausted: bool,
}

pub trait SeedSource<D: DomainDefinition>: Send + Sync + 'static {
    type Cursor: Send + 'static;
    type Scratch: Default + Send + 'static;

    fn open(&self, scope: &D::SeedScope) -> Result<Self::Cursor, D::Error>;
    fn read_batch(
        &self,
        cursor: &mut Self::Cursor,
        limit: usize,
        output: &mut SeedWriter<'_, D>,
        scratch: &mut Self::Scratch,
    ) -> Result<SeedPage, D::Error>;
    fn encode_scope(&self, scope: &D::SeedScope, output: &mut Vec<u8>) -> Result<(), D::Error>;
    fn decode_scope(&self, bytes: &[u8]) -> Result<D::SeedScope, D::Error>;
    fn encode_cursor(&self, cursor: &Self::Cursor, output: &mut Vec<u8>) -> Result<(), D::Error>;
    fn decode_cursor(&self, bytes: &[u8]) -> Result<Self::Cursor, D::Error>;
}

#[derive(Clone, Debug)]
pub struct OperatorDescriptor<O> {
    operator: O,
    symbol: SymbolId,
}

impl<O: Copy> OperatorDescriptor<O> {
    #[must_use]
    pub fn new(operator: O, symbol: SymbolId) -> Self {
        Self { operator, symbol }
    }

    #[must_use]
    pub fn operator(&self) -> O {
        self.operator
    }

    #[must_use]
    pub fn symbol(&self) -> &SymbolId {
        &self.symbol
    }
}

pub struct OperatorEnumerationBatch<'a, D: DomainDefinition, O> {
    artifacts: &'a [&'a D::Artifact],
    locations: &'a [StructuralLocation],
    operators: &'a [O],
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StructuralLocation {
    artifact_index: usize,
    node_index: usize,
}

impl StructuralLocation {
    #[must_use]
    pub fn new(artifact_index: usize, node_index: usize) -> Self {
        Self {
            artifact_index,
            node_index,
        }
    }

    #[must_use]
    pub fn artifact_index(self) -> usize {
        self.artifact_index
    }

    #[must_use]
    pub fn node_index(self) -> usize {
        self.node_index
    }
}

impl<'a, D: DomainDefinition, O> OperatorEnumerationBatch<'a, D, O> {
    #[must_use]
    pub fn new(
        artifacts: &'a [&'a D::Artifact],
        locations: &'a [StructuralLocation],
        operators: &'a [O],
    ) -> Self {
        Self {
            artifacts,
            locations,
            operators,
        }
    }

    #[must_use]
    pub fn artifacts(&self) -> &'a [&'a D::Artifact] {
        self.artifacts
    }

    #[must_use]
    pub fn locations(&self) -> &'a [StructuralLocation] {
        self.locations
    }

    #[must_use]
    pub fn operators(&self) -> &'a [O] {
        self.operators
    }
}

pub struct ApplicationWriter<'a, A> {
    output: &'a mut Vec<A>,
    remaining: usize,
    overflowed: bool,
}

impl<'a, A> ApplicationWriter<'a, A> {
    pub fn new(output: &'a mut Vec<A>) -> Self {
        Self {
            output,
            remaining: usize::MAX,
            overflowed: false,
        }
    }

    pub(crate) fn with_limit(output: &'a mut Vec<A>, limit: usize) -> Self {
        Self {
            output,
            remaining: limit,
            overflowed: false,
        }
    }

    pub fn push(&mut self, application: A) {
        if self.remaining == 0 {
            self.overflowed = true;
        } else {
            self.remaining -= 1;
            self.output.push(application);
        }
    }

    #[must_use]
    pub fn is_full(&self) -> bool {
        self.remaining == 0
    }

    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed
    }
}

pub struct Candidate<D: DomainDefinition> {
    pub source_index: usize,
    pub artifact: D::Artifact,
}

pub struct CandidateWriter<'a, D: DomainDefinition> {
    output: &'a mut Vec<Candidate<D>>,
    remaining: usize,
    overflowed: bool,
}

impl<'a, D: DomainDefinition> CandidateWriter<'a, D> {
    pub fn new(output: &'a mut Vec<Candidate<D>>) -> Self {
        Self {
            output,
            remaining: usize::MAX,
            overflowed: false,
        }
    }

    pub(crate) fn with_limit(output: &'a mut Vec<Candidate<D>>, limit: usize) -> Self {
        Self {
            output,
            remaining: limit,
            overflowed: false,
        }
    }

    pub fn push(&mut self, source_index: usize, artifact: D::Artifact) {
        if self.remaining == 0 {
            self.overflowed = true;
        } else {
            self.remaining -= 1;
            self.output.push(Candidate {
                source_index,
                artifact,
            });
        }
    }

    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed
    }
}

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

pub struct VerificationRequest<'a, D: DomainDefinition, C> {
    pub seed: &'a D::Artifact,
    pub candidate: &'a D::Artifact,
    pub claim: &'a C,
}

pub struct VerificationBatch<'a, D: DomainDefinition, C> {
    requests: &'a [VerificationRequest<'a, D, C>],
}

impl<'a, D: DomainDefinition, C> VerificationBatch<'a, D, C> {
    #[must_use]
    pub fn new(requests: &'a [VerificationRequest<'a, D, C>]) -> Self {
        Self { requests }
    }

    #[must_use]
    pub fn requests(&self) -> &'a [VerificationRequest<'a, D, C>] {
        self.requests
    }
}

pub struct VerificationReplayRequest<'a, D: DomainDefinition, C, E> {
    pub artifact: &'a D::Artifact,
    pub claim: &'a C,
    pub evidence: &'a E,
    pub kernel_revision: KernelRevision,
}

pub struct VerificationReplayBatch<'a, D: DomainDefinition, C, E> {
    requests: &'a [VerificationReplayRequest<'a, D, C, E>],
}

impl<'a, D: DomainDefinition, C, E> VerificationReplayBatch<'a, D, C, E> {
    #[must_use]
    pub fn new(requests: &'a [VerificationReplayRequest<'a, D, C, E>]) -> Self {
        Self { requests }
    }

    #[must_use]
    pub fn requests(&self) -> &'a [VerificationReplayRequest<'a, D, C, E>] {
        self.requests
    }
}

pub enum Verdict<E> {
    Accepted { evidence: E },
    Refuted,
    Unknown,
}

pub struct VerdictWriter<'a, E> {
    output: &'a mut Vec<Verdict<E>>,
    remaining: usize,
    overflowed: bool,
}

impl<'a, E> VerdictWriter<'a, E> {
    pub fn new(output: &'a mut Vec<Verdict<E>>) -> Self {
        Self {
            output,
            remaining: usize::MAX,
            overflowed: false,
        }
    }

    pub(crate) fn with_limit(output: &'a mut Vec<Verdict<E>>, limit: usize) -> Self {
        Self {
            output,
            remaining: limit,
            overflowed: false,
        }
    }

    pub fn push(&mut self, verdict: Verdict<E>) {
        if self.remaining == 0 {
            self.overflowed = true;
        } else {
            self.remaining -= 1;
            self.output.push(verdict);
        }
    }

    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed
    }
}

pub struct ReplayVerdictWriter<'a> {
    output: &'a mut Vec<bool>,
    remaining: usize,
    overflowed: bool,
}

impl<'a> ReplayVerdictWriter<'a> {
    pub fn new(output: &'a mut Vec<bool>) -> Self {
        Self {
            output,
            remaining: usize::MAX,
            overflowed: false,
        }
    }

    pub(crate) fn with_limit(output: &'a mut Vec<bool>, limit: usize) -> Self {
        Self {
            output,
            remaining: limit,
            overflowed: false,
        }
    }

    pub fn push(&mut self, accepted: bool) {
        if self.remaining == 0 {
            self.overflowed = true;
        } else {
            self.remaining -= 1;
            self.output.push(accepted);
        }
    }

    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed
    }
}

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
    fn encode_claim(&self, claim: &Self::Claim, output: &mut Vec<u8>) -> Result<(), D::Error>;
    fn decode_claim(&self, bytes: &[u8]) -> Result<Self::Claim, D::Error>;
    fn encode_evidence(
        &self,
        evidence: &Self::Evidence,
        output: &mut Vec<u8>,
    ) -> Result<(), D::Error>;
    fn decode_evidence(&self, bytes: &[u8]) -> Result<Self::Evidence, D::Error>;
}
