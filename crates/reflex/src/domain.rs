use std::error::Error;
use std::hash::Hash;
use std::num::{NonZeroU64, NonZeroUsize};
use std::time::Duration;

use crate::measurement::MeasurementSpace;

pub const PROPOSAL_FEATURE_COUNT: usize = 8;

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

/// A canonical post-order structural view.
///
/// Every child index must be smaller than its parent index, and a non-empty
/// view's root is always `node_count() - 1`. The Runtime relies on this ordering
/// for root-only primitive and Derived Operator applications.
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
    immediate_arity: ImmediateArity,
    child_binding_depths: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ImmediateArity {
    Exact(usize),
    Variable,
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
            immediate_arity: ImmediateArity::Exact(immediate_count),
            child_binding_depths,
        }
    }

    /// Creates a constructor whose canonical immediate payload has variable length.
    ///
    /// # Panics
    ///
    /// Panics when `child_sorts` and `child_binding_depths` do not describe the
    /// same number of child positions.
    #[must_use]
    pub fn new_variable_immediates(
        constructor: C,
        symbol: SymbolId,
        result_sort: S,
        child_sorts: Vec<S>,
        child_binding_depths: Vec<u32>,
    ) -> Self {
        assert_eq!(child_sorts.len(), child_binding_depths.len());
        Self {
            constructor,
            symbol,
            result_sort,
            child_sorts,
            immediate_arity: ImmediateArity::Variable,
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

    pub fn immediate_arity(&self) -> ImmediateArity {
        self.immediate_arity
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
    skip: usize,
    remaining: usize,
    overflowed: bool,
}

impl<'a, A> ApplicationWriter<'a, A> {
    pub fn new(output: &'a mut Vec<A>) -> Self {
        Self {
            output,
            skip: 0,
            remaining: usize::MAX,
            overflowed: false,
        }
    }

    pub(crate) fn with_limit(output: &'a mut Vec<A>, limit: usize) -> Self {
        Self {
            output,
            skip: 0,
            remaining: limit,
            overflowed: false,
        }
    }

    pub(crate) fn with_window(output: &'a mut Vec<A>, skip: usize, limit: usize) -> Self {
        Self {
            output,
            skip,
            remaining: limit,
            overflowed: false,
        }
    }

    pub fn push(&mut self, application: A) {
        if self.skip != 0 {
            self.skip -= 1;
        } else if self.remaining == 0 {
            self.overflowed = true;
        } else {
            self.remaining -= 1;
            self.output.push(application);
        }
    }

    #[must_use]
    /// Reports that bounded enumeration has supplied the one-Application
    /// lookahead proving that this page is not final.
    pub fn is_full(&self) -> bool {
        self.overflowed
    }

    /// Maximum additional legal Applications needed to skip the retained
    /// prefix, fill this bounded page, and probe whether another page exists.
    #[must_use]
    pub fn remaining_capacity(&self) -> usize {
        if self.remaining == usize::MAX {
            usize::MAX
        } else {
            self.skip.saturating_add(self.remaining).saturating_add(1)
        }
    }

    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed
    }

    pub(crate) fn consumed_prefix(&self) -> bool {
        self.skip == 0
    }
}

pub struct Candidate<D: DomainDefinition> {
    pub source_index: usize,
    pub artifact: D::Artifact,
    pub proposal_features: ProposalFeatures,
    pub proposal_provenance: Option<ProposalProvenance>,
}

/// Stable domain-owned identity for the verified support behind a proposal.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ProposalProvenance {
    support_key: [u8; 32],
}

impl ProposalProvenance {
    #[must_use]
    pub const fn new(support_key: [u8; 32]) -> Self {
        Self { support_key }
    }

    #[must_use]
    pub const fn support_key(self) -> [u8; 32] {
        self.support_key
    }
}

/// A bounded advisory description of how an Operator formed a Candidate.
///
/// Values are scoped by the Domain Definition's Semantic Identity and must be
/// finite. They guide learned search but never establish correctness.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProposalFeatures([f32; PROPOSAL_FEATURE_COUNT]);

impl ProposalFeatures {
    /// Creates a domain-scoped proposal feature channel.
    ///
    /// # Panics
    ///
    /// Panics when any feature is not finite.
    #[must_use]
    pub fn new(values: [f32; PROPOSAL_FEATURE_COUNT]) -> Self {
        assert!(
            values.iter().all(|value| value.is_finite()),
            "proposal features must be finite"
        );
        Self(values)
    }

    #[must_use]
    pub fn as_array(self) -> [f32; PROPOSAL_FEATURE_COUNT] {
        self.0
    }
}

impl Default for ProposalFeatures {
    fn default() -> Self {
        Self([0.0; PROPOSAL_FEATURE_COUNT])
    }
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
        self.push_with_provenance(source_index, artifact, ProposalFeatures::default(), None);
    }

    pub fn push_with_features(
        &mut self,
        source_index: usize,
        artifact: D::Artifact,
        proposal_features: ProposalFeatures,
    ) {
        self.push_with_provenance(source_index, artifact, proposal_features, None);
    }

    pub fn push_with_provenance(
        &mut self,
        source_index: usize,
        artifact: D::Artifact,
        proposal_features: ProposalFeatures,
        proposal_provenance: Option<ProposalProvenance>,
    ) {
        if self.remaining == 0 {
            self.overflowed = true;
        } else {
            self.remaining -= 1;
            self.output.push(Candidate {
                source_index,
                artifact,
                proposal_features,
                proposal_provenance,
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
    /// Resident bytes retained by this Operator implementation.
    fn resident_bytes(&self) -> u64;
    /// Maximum scratch bytes needed for a batch with this output capacity.
    fn scratch_resident_bytes(&self, output_capacity: usize) -> u64;
    /// Enumerates legal Applications in deterministic order for identical
    /// requests under one Semantic Identity.
    ///
    /// Bounded Runtime pages may replay and discard an already-consumed prefix,
    /// so an implementation must not reorder that prefix between calls.
    fn enumerate_legal(
        &self,
        requests: OperatorEnumerationBatch<'_, D, Self::Operator>,
        output: &mut ApplicationWriter<'_, Self::Application>,
        scratch: &mut Self::Scratch,
    ) -> Result<(), D::Error>;
    /// Materializes exactly one Candidate for every supplied legal Application.
    ///
    /// The Candidate's `source_index` continues to address the Artifact batch
    /// from which its Application was enumerated. Violating this cardinality
    /// contract is a protocol defect and the Runtime terminates immediately.
    fn apply_batch(
        &self,
        applications: &[Self::Application],
        output: &mut CandidateWriter<'_, D>,
        scratch: &mut Self::Scratch,
    ) -> Result<(), D::Error>;
}

#[cfg(test)]
mod application_writer_tests {
    use super::ApplicationWriter;

    #[test]
    fn bounded_window_skips_a_prefix_and_probes_for_a_later_page() {
        let mut output = Vec::new();
        let overflowed = {
            let mut writer = ApplicationWriter::with_window(&mut output, 2, 2);
            for value in 0..5 {
                if writer.is_full() {
                    break;
                }
                writer.push(value);
            }
            writer.overflowed()
        };

        assert_eq!(output, [2, 3]);
        assert!(overflowed);
    }

    #[test]
    fn bounded_window_identifies_an_exact_final_page() {
        let mut output = Vec::new();
        let overflowed = {
            let mut writer = ApplicationWriter::with_window(&mut output, 2, 2);
            for value in 0..4 {
                if writer.is_full() {
                    break;
                }
                writer.push(value);
            }
            writer.overflowed()
        };

        assert_eq!(output, [2, 3]);
        assert!(!overflowed);
    }

    #[test]
    fn bounded_window_detects_a_missing_retained_prefix() {
        let mut output = Vec::new();
        let consumed_prefix = {
            let mut writer = ApplicationWriter::with_window(&mut output, 3, 2);
            for value in 0..2 {
                writer.push(value);
            }
            writer.consumed_prefix()
        };

        assert!(output.is_empty());
        assert!(!consumed_prefix);
    }
}

pub struct VerificationRequest<'a, D: DomainDefinition, C> {
    pub seed: &'a D::Artifact,
    pub candidate: &'a D::Artifact,
    pub claim: &'a C,
}

pub struct VerificationBatch<'a, D: DomainDefinition, C> {
    requests: &'a [VerificationRequest<'a, D, C>],
    allowance: VerificationAllowance,
}

impl<'a, D: DomainDefinition, C> VerificationBatch<'a, D, C> {
    #[must_use]
    pub fn new(requests: &'a [VerificationRequest<'a, D, C>]) -> Self {
        Self {
            requests,
            allowance: VerificationAllowance::none(),
        }
    }

    #[must_use]
    pub fn requests(&self) -> &'a [VerificationRequest<'a, D, C>] {
        self.requests
    }

    #[must_use]
    pub fn allowance(&self) -> VerificationAllowance {
        self.allowance
    }

    pub(crate) const fn with_allowance(
        requests: &'a [VerificationRequest<'a, D, C>],
        allowance: VerificationAllowance,
    ) -> Self {
        Self {
            requests,
            allowance,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VerificationAllowance {
    worker_lanes: usize,
    resident_bytes: u64,
    elapsed_time: Duration,
    cpu_time: Duration,
}

impl VerificationAllowance {
    const fn none() -> Self {
        Self {
            worker_lanes: 0,
            resident_bytes: 0,
            elapsed_time: Duration::ZERO,
            cpu_time: Duration::ZERO,
        }
    }

    pub(crate) const fn new(
        worker_lanes: usize,
        resident_bytes: u64,
        elapsed_time: Duration,
        cpu_time: Duration,
    ) -> Self {
        Self {
            worker_lanes,
            resident_bytes,
            elapsed_time,
            cpu_time,
        }
    }

    #[must_use]
    pub fn worker_lanes(self) -> usize {
        self.worker_lanes
    }

    #[must_use]
    pub fn resident_bytes(self) -> u64 {
        self.resident_bytes
    }

    #[must_use]
    pub fn elapsed_time(self) -> Duration {
        self.elapsed_time
    }

    #[must_use]
    pub fn cpu_time(self) -> Duration {
        self.cpu_time
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ExternalVerificationUsage {
    worker_lanes: usize,
    peak_resident_bytes: u64,
    elapsed_time: Duration,
    cpu_time: Duration,
}

impl ExternalVerificationUsage {
    #[must_use]
    pub fn new(
        worker_lanes: usize,
        peak_resident_bytes: u64,
        elapsed_time: Duration,
        cpu_time: Duration,
    ) -> Self {
        Self {
            worker_lanes,
            peak_resident_bytes,
            elapsed_time,
            cpu_time,
        }
    }

    #[must_use]
    pub fn worker_lanes(self) -> usize {
        self.worker_lanes
    }

    #[must_use]
    pub fn peak_resident_bytes(self) -> u64 {
        self.peak_resident_bytes
    }

    #[must_use]
    pub fn elapsed_time(self) -> Duration {
        self.elapsed_time
    }

    #[must_use]
    pub fn cpu_time(self) -> Duration {
        self.cpu_time
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VerificationBatchReport {
    external_usage: ExternalVerificationUsage,
    worker_failed: bool,
}

#[derive(Debug)]
#[must_use = "Verification batch usage and failure must be inspected"]
pub struct VerificationBatchOutcome<E> {
    report: VerificationBatchReport,
    error: Option<E>,
}

impl<E> VerificationBatchOutcome<E> {
    pub const fn completed(report: VerificationBatchReport) -> Self {
        Self {
            report,
            error: None,
        }
    }

    pub const fn domain_error(report: VerificationBatchReport, error: E) -> Self {
        Self {
            report,
            error: Some(error),
        }
    }

    #[must_use]
    pub fn into_parts(self) -> (VerificationBatchReport, Option<E>) {
        (self.report, self.error)
    }
}

impl VerificationBatchReport {
    #[must_use]
    pub const fn in_process() -> Self {
        Self {
            external_usage: ExternalVerificationUsage {
                worker_lanes: 0,
                peak_resident_bytes: 0,
                elapsed_time: Duration::ZERO,
                cpu_time: Duration::ZERO,
            },
            worker_failed: false,
        }
    }

    #[must_use]
    pub const fn external(usage: ExternalVerificationUsage, worker_failed: bool) -> Self {
        Self {
            external_usage: usage,
            worker_failed,
        }
    }

    #[must_use]
    pub fn external_usage(self) -> ExternalVerificationUsage {
        self.external_usage
    }

    #[must_use]
    pub fn worker_failed(self) -> bool {
        self.worker_failed
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VerificationWorkerRequirements {
    worker_lanes: usize,
    resident_bytes: u64,
}

impl VerificationWorkerRequirements {
    #[must_use]
    pub const fn in_process() -> Self {
        Self {
            worker_lanes: 0,
            resident_bytes: 0,
        }
    }

    #[must_use]
    pub fn external(worker_lanes: NonZeroUsize, resident_bytes: NonZeroU64) -> Self {
        Self {
            worker_lanes: worker_lanes.get(),
            resident_bytes: resident_bytes.get(),
        }
    }

    #[must_use]
    pub fn worker_lanes(self) -> usize {
        self.worker_lanes
    }

    #[must_use]
    pub fn resident_bytes(self) -> u64 {
        self.resident_bytes
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
    allowance: VerificationAllowance,
}

impl<'a, D: DomainDefinition, C, E> VerificationReplayBatch<'a, D, C, E> {
    #[must_use]
    pub fn new(requests: &'a [VerificationReplayRequest<'a, D, C, E>]) -> Self {
        Self {
            requests,
            allowance: VerificationAllowance::none(),
        }
    }

    #[must_use]
    pub fn requests(&self) -> &'a [VerificationReplayRequest<'a, D, C, E>] {
        self.requests
    }

    #[must_use]
    pub fn allowance(&self) -> VerificationAllowance {
        self.allowance
    }

    pub(crate) const fn with_allowance(
        requests: &'a [VerificationReplayRequest<'a, D, C, E>],
        allowance: VerificationAllowance,
    ) -> Self {
        Self {
            requests,
            allowance,
        }
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
    fn worker_requirements(&self) -> VerificationWorkerRequirements {
        VerificationWorkerRequirements::in_process()
    }
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
    fn encode_claim(&self, claim: &Self::Claim, output: &mut Vec<u8>) -> Result<(), D::Error>;
    fn decode_claim(&self, bytes: &[u8]) -> Result<Self::Claim, D::Error>;
    fn encode_evidence(
        &self,
        evidence: &Self::Evidence,
        output: &mut Vec<u8>,
    ) -> Result<(), D::Error>;
    fn decode_evidence(&self, bytes: &[u8]) -> Result<Self::Evidence, D::Error>;
}
