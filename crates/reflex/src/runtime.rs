use std::cell::Cell;
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::num::NonZeroU32;
use std::ops::ControlFlow;
use std::sync::Arc;
#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use reflex_bundle::{CanonicalBundle, SegmentKind};
use sha2::{Digest, Sha256};

use crate::bundle::DomainBundle;
use crate::domain::{
    ApplicationWriter, Candidate, CandidateWriter, ClaimOf, DomainDefinition, EvidenceOf,
    OperatorAlgebra, OperatorEnumerationBatch, ProposalFeatures, ProposalProvenance,
    RejectionAdvisory, Seed, SeedSource, SeedWriter, SemanticIdentity, StructuralLocation,
    StructuralProtocol, StructuralView, Verdict, VerificationAllowance, VerificationBatchReport,
    VerificationKernel, VerificationRecord, VerificationReplayRequest,
    VerificationWorkerRequirements,
};
use crate::durability;
use crate::instrumentation::{Phase, Recorder, ResourceRefusal};
use crate::intelligence::{
    AllocationSource, CausalSubject, CheckpointDigest, ConsequenceEdge,
    ConsequenceKind as IntelligenceConsequenceKind, DecisionId, ForecastAxis, IntelligenceCore,
    IntelligenceError, IntelligenceLimits, IntelligenceTransition, InvestmentOutcome,
    InvestmentReceipt, InvestmentSettlement, InvestmentSpec, KnowledgeCompilationError,
    KnowledgeObligationWork, KnowledgePlanningFailure, KnowledgeRecoveryObligation,
    KnowledgeShadowArmReport, KnowledgeShadowReport, KnowledgeShadowRootFact,
    KnowledgeShadowSupportFact, KnowledgeVerificationReport, KnowledgeWork, MarketArena,
    MarketFrame, NativeTrainingBudget, OpenedKnowledgeWork, OperationalAction,
    OperationalActionSpec, OpportunitySpec, PortfolioBuffer, PreparedNativeEcologyPlan,
    PreparedOperationalMarket, ProposedIntelligenceView, ResourceVector, RoutingFamilyId,
    SettlementFrame, ShadowArm, ShadowUpdate, SubjectId, TypedOutcome,
};
use crate::knowledge::{KnowledgeRevision, KnowledgeState};
#[cfg(feature = "internal-experiments")]
use crate::learning::AttemptObservation;
use crate::learning::{
    ConsequenceKind, ConsequenceObservation, FEATURE_COUNT, Features, LearningState,
};
use crate::measurement::{Measurement, MeasurementSpace, MeasurementWriter, VerifiedBatch};
use crate::policy::{
    AllocationQueue, OperationalPartition, RuntimePolicyKernel, RuntimePolicyRevision,
    RuntimePolicyState, cooperative_ranked_selections, operational_ranked_selections,
};
use crate::resource::{ResidentReservation, ResourceEnvelopeGuard};
use crate::session::{
    ArtifactKey, Completion, GoalId, ImprovementRequest, ParetoSnapshot, ParetoUpdate,
    ResourceUsage, SessionError, SessionOutcome, VerifiedArtifact, VerifiedArtifactRecord,
};

mod bundle;
mod cohort;
mod epoch;
mod experience;
mod generation;
mod goals;
mod proposal;
mod scheduler;
mod shadow;

use bundle::{RestartBundleCodec, SealAdmission};
use epoch::EpochTransition;
#[cfg(feature = "internal-experiments")]
use experience::CandidateFateKey;
use experience::{
    CandidateFateDisposition, CandidateFateObservation, CandidateRank, ExperienceEntry,
    ExperienceLedger, ExperienceVerdict, candidate_fate_batches_are_valid,
    encoded_candidate_fates_len, encoded_entry_len,
};
use goals::GoalEvaluator;
use proposal::{
    DerivedOperatorEngine, DerivedOperatorRequest, ProposalEngine, ProposalParents,
    StructuredRewriteEngine, StructuredRewriteRequest,
};
use scheduler::{ClaimVerificationRequest, ScheduleError, Scheduler};

const OPERATOR_FEATURE_START: usize = 5;
const OPERATOR_FEATURE_END: usize = 13;

struct StoredArtifact<D: DomainDefinition> {
    artifact: D::Artifact,
    verification: VerificationRecord<D>,
    provenance: Vec<u8>,
    origin_key: Option<ArtifactKey>,
    parent_key: Option<ArtifactKey>,
}
type OriginatedStoredArtifact<D> = (StoredArtifact<D>, usize, [u8; 32]);
type ClaimedVerdicts<D> = Vec<(
    ClaimOf<D>,
    Verdict<EvidenceOf<D>>,
    Option<RejectionAdvisory>,
)>;
struct RecoveredBundle<D: DomainDefinition> {
    artifacts: Vec<StoredArtifact<D>>,
    pareto_keys: Vec<ArtifactKey>,
    frontier_keys: Vec<ArtifactKey>,
    deferred_candidates: Vec<DeferredCandidate<D>>,
    pending_parents: Vec<PendingParent>,
    generation_complete: bool,
    ledger: ExperienceLedger,
    revisions: Option<RevisionIds>,
    interrupted_usage: Option<ResourceUsage>,
    legacy_knowledge: Option<KnowledgeState>,
    legacy_learning: Option<LearningState>,
    legacy_runtime_policy: Option<RuntimePolicyState>,
    intelligence: Option<IntelligenceCore>,
    authenticated_intelligence_identity: Option<[u8; 32]>,
    resident_bytes: u64,
}

impl<D: DomainDefinition> Default for RecoveredBundle<D> {
    fn default() -> Self {
        Self {
            artifacts: Vec::new(),
            pareto_keys: Vec::new(),
            frontier_keys: Vec::new(),
            deferred_candidates: Vec::new(),
            pending_parents: Vec::new(),
            generation_complete: false,
            ledger: ExperienceLedger::default(),
            revisions: None,
            interrupted_usage: None,
            legacy_knowledge: None,
            legacy_learning: None,
            legacy_runtime_policy: None,
            intelligence: None,
            authenticated_intelligence_identity: None,
            resident_bytes: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct RevisionIds {
    knowledge: [u8; 32],
    model: [u8; 32],
    runtime_policy: [u8; 32],
    intelligence: [u8; 32],
}

struct VerificationOutcome<D: DomainDefinition> {
    accepted: Vec<OriginatedStoredArtifact<D>>,
    experience: Vec<ExperienceEntry>,
    intelligence_receipts: Vec<InvestmentReceipt>,
    intelligence_settlements: Vec<InvestmentSettlement>,
    verification_decisions: Vec<([u8; 32], DecisionId)>,
    action_decisions: Vec<([u8; 32], DecisionId)>,
    kernel_usage: ResourceVector,
    external_worker_cpu_ns: u64,
}

struct ProposedCandidate<D: DomainDefinition> {
    candidate: Candidate<D>,
    canonical_candidate: Vec<u8>,
    operator_symbol: Vec<u8>,
    operator_digest: [u8; 32],
    features: Features,
    epoch: u64,
    proposal_limit: u32,
    protected_derived: bool,
    allocation_queue: AllocationQueue,
    fate_index: usize,
    bootstrap_rank: CandidateRank,
    learned_rank: CandidateRank,
    generated_in_epoch: bool,
    published_in_epoch: bool,
    intelligence_receipt: Option<InvestmentReceipt>,
    action_decision: Option<DecisionId>,
    causal_parent_key: Option<ArtifactKey>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CandidateVerificationDemand {
    resident_bytes: u64,
    durable_bytes: u64,
}

impl<D: DomainDefinition> ProposedCandidate<D> {
    fn generated(
        candidate: Candidate<D>,
        canonical_operator: Vec<u8>,
        features: Features,
        epoch: u64,
        proposal_limit: usize,
        protected_derived: bool,
    ) -> Self {
        Self {
            candidate,
            canonical_candidate: Vec::new(),
            operator_symbol: canonical_operator,
            operator_digest: [0; 32],
            features,
            epoch,
            proposal_limit: u32::try_from(proposal_limit).unwrap_or(u32::MAX),
            protected_derived,
            allocation_queue: AllocationQueue::Bootstrap,
            fate_index: usize::MAX,
            bootstrap_rank: CandidateRank::absent(),
            learned_rank: CandidateRank::absent(),
            generated_in_epoch: true,
            published_in_epoch: false,
            intelligence_receipt: None,
            action_decision: None,
            causal_parent_key: None,
        }
    }
}

const ENUMERATION_COMPLETE: u64 = u64::MAX;

#[derive(Clone)]
struct PendingParent {
    key: ArtifactKey,
    primitive_offsets: Vec<u64>,
    derived_sampled: bool,
}

impl PendingParent {
    fn new(key: ArtifactKey, primitive_operator_count: usize, has_derived: bool) -> Self {
        Self {
            key,
            primitive_offsets: vec![0; primitive_operator_count],
            derived_sampled: !has_derived,
        }
    }

    fn complete(&self) -> bool {
        self.derived_sampled
            && self
                .primitive_offsets
                .iter()
                .all(|offset| *offset == ENUMERATION_COMPLETE)
    }

    fn resident_bytes(&self) -> u64 {
        (self.primitive_offsets.capacity() as u64).saturating_mul(std::mem::size_of::<u64>() as u64)
    }
}

fn pending_parent_vector_resident_bytes(parents: &[PendingParent], vector_capacity: usize) -> u64 {
    u64::try_from(vector_capacity.saturating_mul(std::mem::size_of::<PendingParent>()))
        .unwrap_or(u64::MAX)
        .saturating_add(parents.iter().fold(0_u64, |bytes, parent| {
            bytes.saturating_add(parent.resident_bytes())
        }))
}

#[derive(Clone, Copy)]
struct SearchTailView<'a, D: DomainDefinition> {
    frontier: &'a [(VerifiedArtifact<D>, usize)],
    deferred_candidates: &'a [ProposedCandidate<D>],
    pending_parents: &'a [PendingParent],
    generation_complete: bool,
}

#[derive(Clone, Copy)]
struct RestartBundleState<'a, D: DomainDefinition> {
    artifacts: &'a [VerifiedArtifact<D>],
    pareto: &'a [VerifiedArtifact<D>],
    search_tail: SearchTailView<'a, D>,
    ledger: &'a ExperienceLedger,
    intelligence: &'a dyn IntelligenceBundleView,
}

trait IntelligenceBundleView {
    fn checkpoint_bytes(&self) -> &[u8];
    fn checkpoint_identity(&self) -> [u8; 32] {
        let checkpoint = self.checkpoint_bytes();
        checkpoint[checkpoint.len() - 32..]
            .try_into()
            .expect("an authenticated Intelligence checkpoint ends in a 32-byte checksum")
    }
    fn knowledge_product_identity(&self) -> [u8; 32];
    fn model_ecology_identity(&self) -> [u8; 32];
    fn runtime_policy_revision(&self) -> RuntimePolicyRevision;
}

impl IntelligenceBundleView for IntelligenceCore {
    fn checkpoint_bytes(&self) -> &[u8] {
        self.checkpoint_bytes()
    }

    fn knowledge_product_identity(&self) -> [u8; 32] {
        self.knowledge_product().identity()
    }

    fn model_ecology_identity(&self) -> [u8; 32] {
        self.model_ecology_identity()
    }

    fn runtime_policy_revision(&self) -> RuntimePolicyRevision {
        self.runtime_policy_revision()
    }
}

impl IntelligenceBundleView for ProposedIntelligenceView<'_> {
    fn checkpoint_bytes(&self) -> &[u8] {
        self.checkpoint_bytes()
    }

    fn knowledge_product_identity(&self) -> [u8; 32] {
        self.knowledge_product_identity()
    }

    fn model_ecology_identity(&self) -> [u8; 32] {
        self.model_ecology_identity()
    }

    fn runtime_policy_revision(&self) -> RuntimePolicyRevision {
        self.runtime_policy_revision()
    }
}

#[derive(Clone, Copy)]
struct IntelligencePublicationState<'a, D: DomainDefinition> {
    artifacts: &'a [VerifiedArtifact<D>],
    pareto: &'a [VerifiedArtifact<D>],
    frontier: &'a [(VerifiedArtifact<D>, usize)],
    deferred_candidates: &'a [ProposedCandidate<D>],
    pending_parents: &'a [PendingParent],
    generation_complete: bool,
    ledger: &'a ExperienceLedger,
}

impl<D: DomainDefinition> IntelligencePublicationState<'_, D> {
    const fn restart_state<'b>(
        &'b self,
        intelligence: &'b dyn IntelligenceBundleView,
    ) -> RestartBundleState<'b, D> {
        RestartBundleState::new(
            self.artifacts,
            self.pareto,
            SearchTailView::new(
                self.frontier,
                self.deferred_candidates,
                self.pending_parents,
            )
            .with_generation_complete(self.generation_complete),
            self.ledger,
            intelligence,
        )
    }
}

impl<'a, D: DomainDefinition> RestartBundleState<'a, D> {
    const fn new(
        artifacts: &'a [VerifiedArtifact<D>],
        pareto: &'a [VerifiedArtifact<D>],
        search_tail: SearchTailView<'a, D>,
        ledger: &'a ExperienceLedger,
        intelligence: &'a dyn IntelligenceBundleView,
    ) -> Self {
        Self {
            artifacts,
            pareto,
            search_tail,
            ledger,
            intelligence,
        }
    }
}

impl<'a, D: DomainDefinition> SearchTailView<'a, D> {
    const fn new(
        frontier: &'a [(VerifiedArtifact<D>, usize)],
        deferred_candidates: &'a [ProposedCandidate<D>],
        pending_parents: &'a [PendingParent],
    ) -> Self {
        Self {
            frontier,
            deferred_candidates,
            pending_parents,
            generation_complete: false,
        }
    }

    const fn with_generation_complete(mut self, generation_complete: bool) -> Self {
        self.generation_complete = generation_complete;
        self
    }
}

struct DeferredCandidate<D: DomainDefinition> {
    artifact: D::Artifact,
    canonical_candidate: Vec<u8>,
    parent_key: ArtifactKey,
    operator_symbol: Vec<u8>,
    proposal_features: ProposalFeatures,
    proposal_provenance: Option<ProposalProvenance>,
    proposal_limit: u32,
    protected_derived: bool,
    action_decision: Option<DecisionId>,
    causal_parent_key: Option<ArtifactKey>,
}

struct NovelCandidateBatch<D: DomainDefinition> {
    candidates: Vec<ProposedCandidate<D>>,
    fates: Vec<CandidateFateObservation>,
    fate_is_new_generation: Vec<bool>,
}

#[derive(Clone, Copy)]
struct StructuralSummary {
    node_count: f32,
}

struct PolicyCohortTransaction<D: DomainDefinition> {
    verification: VerificationOutcome<D>,
    candidate_fates: Vec<CandidateFateObservation>,
    plan: cohort::PolicySelectionPlan,
}

#[derive(Clone)]
struct KnowledgeShadowCheckpoint {
    installed: KnowledgeRevision,
    incumbent: KnowledgeRevision,
}

struct KnowledgeCampaignArmOutcome {
    useful_descendants: f32,
    compression_value: f32,
    usage: ResourceVector,
}

struct KnowledgeCampaignResidentView<'a, D: DomainDefinition> {
    known: &'a Vec<VerifiedArtifact<D>>,
    frontier: &'a Vec<(VerifiedArtifact<D>, usize)>,
    experience: &'a Vec<ExperienceEntry>,
    pending: &'a Vec<PendingParent>,
}

impl<D: DomainDefinition> KnowledgeCampaignResidentView<'_, D> {
    fn resident_bytes(&self) -> u64 {
        let known_artifacts = self.known.iter().fold(0_u64, |bytes, artifact| {
            bytes.saturating_add(artifact.inner.dynamic_resident_bytes)
        });
        let frontier_artifacts = self.frontier.iter().fold(0_u64, |bytes, (artifact, _)| {
            bytes.saturating_add(artifact.inner.dynamic_resident_bytes)
        });
        let experience_payloads = self.experience.iter().fold(0_u64, |bytes, entry| {
            bytes
                .saturating_add(entry.canonical_candidate.capacity() as u64)
                .saturating_add(entry.operator_symbol.capacity() as u64)
        });
        vector_bytes(self.known)
            .saturating_add(known_artifacts)
            .saturating_add(vector_bytes(self.frontier))
            .saturating_add(frontier_artifacts)
            .saturating_add(vector_bytes(self.experience))
            .saturating_add(experience_payloads)
            .saturating_add(vector_bytes(self.pending))
            .saturating_add(self.pending.iter().fold(0_u64, |bytes, parent| {
                bytes.saturating_add(parent.resident_bytes())
            }))
    }
}

fn knowledge_campaign_initial_resident_bound<D: DomainDefinition>(
    known: &[VerifiedArtifact<D>],
    frontier: &[(VerifiedArtifact<D>, usize)],
    experience: &[ExperienceEntry],
    primitive_count: usize,
) -> u64 {
    let known_artifacts = known.iter().fold(0_u64, |bytes, artifact| {
        bytes.saturating_add(artifact.inner.dynamic_resident_bytes)
    });
    let frontier_artifacts = frontier.iter().fold(0_u64, |bytes, (artifact, _)| {
        bytes.saturating_add(artifact.inner.dynamic_resident_bytes)
    });
    let experience_payloads = experience.iter().fold(0_u64, |bytes, entry| {
        bytes
            .saturating_add(entry.canonical_candidate.len() as u64)
            .saturating_add(entry.operator_symbol.len() as u64)
    });
    (known.len() as u64)
        .saturating_mul(std::mem::size_of::<VerifiedArtifact<D>>() as u64)
        .saturating_add(known_artifacts)
        .saturating_add(
            (frontier.len() as u64)
                .saturating_mul(std::mem::size_of::<(VerifiedArtifact<D>, usize)>() as u64),
        )
        .saturating_add(frontier_artifacts)
        .saturating_add(
            (experience.len() as u64).saturating_mul(std::mem::size_of::<ExperienceEntry>() as u64),
        )
        .saturating_add(experience_payloads)
        .saturating_add(
            (frontier.len() as u64).saturating_mul(std::mem::size_of::<PendingParent>() as u64),
        )
        .saturating_add(
            (frontier.len() as u64)
                .saturating_mul(primitive_count as u64)
                .saturating_mul(std::mem::size_of::<u64>() as u64),
        )
}

struct VerifiedKnowledgeShadowOutcome {
    compressed_attempts: Vec<[u8; 32]>,
    verification_requests: u64,
}

#[cfg(feature = "internal-experiments")]
struct ShapeSummary<C> {
    node_count: f32,
    depth: f32,
    constructor_frequencies: Vec<f32>,
    root_constructor: Option<C>,
}

#[cfg(feature = "internal-experiments")]
struct CandidateFeatureDiagnostics {
    groups: usize,
    mixed_verdict_groups: usize,
    accepted_in_mixed_groups: usize,
    accepted_examples: usize,
    proposal_informed_examples: usize,
}

struct ReadSeeds<D: DomainDefinition> {
    seeds: Vec<Seed<D>>,
    encoded_cursor: Vec<u8>,
}

#[derive(Clone, Copy)]
enum SessionSeal {
    Interrupted(ResourceUsage),
    Completed(Completion, ResourceUsage),
}
const WORKER_STACK_BYTES: usize = 2 * 1024 * 1024;
const DURABILITY_STACK_BYTES: usize = 512 * 1024;
const MIN_CHOICE_RESIDENT_BYTES: u64 = 8 * 1024;
// Keep one immutable minimum cohort of allocator pages outside ecology sizing.
// Exact checkpoint/candidate payloads are admitted separately; this bound keeps
// their fixed seal and publication scaffolding from being consumed by scratch.
const MIN_SESSION_DYNAMIC_HEADROOM_BYTES: u64 = MIN_CHOICE_RESIDENT_BYTES * 8;
const MAX_CANDIDATE_CHOICES: u64 = 16_384;

pub(crate) struct DomainResourcePlan {
    requirements: VerificationWorkerRequirements,
    #[cfg(feature = "internal-experiments")]
    pub(crate) requested_worker_threads: usize,
    #[cfg(feature = "internal-experiments")]
    pub(crate) external_worker_lanes: usize,
    pub(crate) runtime_worker_lanes: usize,
    #[cfg(feature = "internal-experiments")]
    pub(crate) runtime_stack_bytes: u64,
    #[cfg(feature = "internal-experiments")]
    pub(crate) durability_stack_bytes: u64,
    #[cfg(feature = "internal-experiments")]
    pub(crate) external_worker_bytes: u64,
    #[cfg(feature = "internal-experiments")]
    pub(crate) operator_bytes: u64,
    #[cfg(feature = "internal-experiments")]
    pub(crate) maximum_candidate_capacity: usize,
    #[cfg(feature = "internal-experiments")]
    pub(crate) operator_scratch_bytes_per_lane: u64,
    #[cfg(feature = "internal-experiments")]
    pub(crate) operator_scratch_bytes: u64,
    pub(crate) fixed_resident_bytes: u64,
}

fn fixed_resident_categories(
    runtime_worker_lanes: usize,
    external_worker_bytes: u64,
    operator_bytes: u64,
    operator_scratch_bytes_per_lane: u64,
) -> (u64, u64, u64, u64) {
    let runtime_stack_bytes =
        (runtime_worker_lanes as u64).saturating_mul(WORKER_STACK_BYTES as u64);
    let durability_stack_bytes = DURABILITY_STACK_BYTES as u64;
    let operator_scratch_bytes =
        (runtime_worker_lanes as u64).saturating_mul(operator_scratch_bytes_per_lane);
    let fixed_resident_bytes = runtime_stack_bytes
        .saturating_add(durability_stack_bytes)
        .saturating_add(external_worker_bytes)
        .saturating_add(operator_bytes)
        .saturating_add(operator_scratch_bytes);
    (
        runtime_stack_bytes,
        durability_stack_bytes,
        operator_scratch_bytes,
        fixed_resident_bytes,
    )
}

fn domain_resource_plan<D: DomainDefinition>(
    domain: &D,
    requested_worker_threads: usize,
) -> Option<DomainResourcePlan> {
    let requirements = domain.kernel().worker_requirements();
    let runtime_worker_lanes = requested_worker_threads
        .checked_sub(requirements.worker_lanes())
        .filter(|lanes| *lanes != 0)?;
    let external_worker_bytes = requirements.resident_bytes();
    let operator_bytes = domain.operators().resident_bytes();
    let maximum_candidate_capacity = usize::try_from(MAX_CANDIDATE_CHOICES).ok()?;
    let operator_scratch_bytes_per_lane = domain
        .operators()
        .scratch_resident_bytes(maximum_candidate_capacity);
    let resident_categories = fixed_resident_categories(
        runtime_worker_lanes,
        external_worker_bytes,
        operator_bytes,
        operator_scratch_bytes_per_lane,
    );
    let fixed_resident_bytes = resident_categories.3;
    Some(DomainResourcePlan {
        requirements,
        #[cfg(feature = "internal-experiments")]
        requested_worker_threads,
        #[cfg(feature = "internal-experiments")]
        external_worker_lanes: requirements.worker_lanes(),
        runtime_worker_lanes,
        #[cfg(feature = "internal-experiments")]
        runtime_stack_bytes: resident_categories.0,
        #[cfg(feature = "internal-experiments")]
        durability_stack_bytes: resident_categories.1,
        #[cfg(feature = "internal-experiments")]
        external_worker_bytes,
        #[cfg(feature = "internal-experiments")]
        operator_bytes,
        #[cfg(feature = "internal-experiments")]
        maximum_candidate_capacity,
        #[cfg(feature = "internal-experiments")]
        operator_scratch_bytes_per_lane,
        #[cfg(feature = "internal-experiments")]
        operator_scratch_bytes: resident_categories.2,
        fixed_resident_bytes,
    })
}

#[cfg(feature = "internal-experiments")]
pub(crate) fn inspect_domain_resources<D: DomainDefinition>(
    domain: &D,
    requested_worker_threads: usize,
) -> Option<DomainResourcePlan> {
    domain_resource_plan(domain, requested_worker_threads)
}
const LEGACY_RUNTIME_REVISION: u64 = 20;
const STANDALONE_POLICY_RUNTIME_REVISION: u64 = 21;
const STANDALONE_KNOWLEDGE_RUNTIME_REVISION: u64 = 22;
const PRE_ACTION_PROVENANCE_RUNTIME_REVISION: u64 = 23;
const ACTION_PROVENANCE_RUNTIME_REVISION: u64 = 24;
const RUNTIME_REVISION: u64 = 25;
const LEGACY_REVISIONS_SEGMENT_VERSION: u32 = 4;
const STANDALONE_POLICY_REVISIONS_SEGMENT_VERSION: u32 = 5;
const STANDALONE_KNOWLEDGE_REVISIONS_SEGMENT_VERSION: u32 = 6;
const REVISIONS_SEGMENT_VERSION: u32 = 7;
const BUNDLE_DECODE_RESIDENT_MULTIPLIER: u64 = 12;
#[cfg(debug_assertions)]
static FAULT_OCCURRENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn improve<D, O>(
    domain: &D,
    request: &ImprovementRequest<D>,
    observer: O,
) -> Result<SessionOutcome<D>, SessionError<D::Error>>
where
    D: DomainDefinition,
    O: for<'a> FnMut(ParetoUpdate<'a, D>) -> ControlFlow<()> + Send,
{
    let resource_meter =
        ResourceEnvelopeGuard::start(&request.resources).map_err(|()| SessionError::Resource)?;
    let resource_plan = domain_resource_plan(domain, request.resources.worker_threads.get())
        .ok_or(SessionError::Resource)?;
    let requirements = resource_plan.requirements;
    let runtime_lanes = resource_plan.runtime_worker_lanes;
    let scheduler =
        Scheduler::from_environment(runtime_lanes).map_err(|()| SessionError::Resource)?;
    let worker_resident_bytes = resource_plan.fixed_resident_bytes;
    if !resource_meter.reserve(ResidentReservation::live(worker_resident_bytes)) {
        return Err(SessionError::Resource);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(runtime_lanes)
        .stack_size(WORKER_STACK_BYTES)
        .thread_name(|index| format!("reflex-worker-{index}"))
        .build()
        .map_err(|_| SessionError::Resource)?;
    pool.install(move || {
        improve_on_workers(
            domain,
            request,
            observer,
            &resource_meter,
            worker_resident_bytes,
            &scheduler,
            requirements,
        )
    })
}

struct DecodedSession<'a> {
    completed: bool,
    runtime_revision: u64,
    restart_state_root: Option<[u8; 32]>,
    encoded_scope: &'a [u8],
    encoded_cursor: &'a [u8],
    #[cfg(feature = "internal-experiments")]
    requested: ResourceUsage,
    kernel_revision: u64,
    compatibility_prefix_len: usize,
    #[cfg(feature = "internal-experiments")]
    environment: &'a [u8],
    #[cfg(feature = "internal-experiments")]
    completion: Option<Completion>,
    usage: ResourceUsage,
}

fn decode_session_segment<E>(bytes: &[u8]) -> Result<DecodedSession<'_>, SessionError<E>> {
    let mut input = bytes;
    let disposition = take_bundle(&mut input, 1)?[0];
    if !matches!(disposition, 0 | 1) {
        return Err(SessionError::CorruptBundle);
    }
    let runtime_revision = read_bundle_u64(&mut input)?;
    if runtime_revision != RUNTIME_REVISION
        && !(disposition == 1
            && matches!(
                runtime_revision,
                LEGACY_RUNTIME_REVISION
                    | STANDALONE_POLICY_RUNTIME_REVISION
                    | STANDALONE_KNOWLEDGE_RUNTIME_REVISION
                    | PRE_ACTION_PROVENANCE_RUNTIME_REVISION
                    | ACTION_PROVENANCE_RUNTIME_REVISION
            ))
    {
        return Err(SessionError::IncompatibleBundle);
    }
    let restart_state_root = if runtime_revision == LEGACY_RUNTIME_REVISION {
        None
    } else {
        Some(
            take_bundle(&mut input, 32)?
                .try_into()
                .expect("exactly 32 restart-state-root bytes were taken"),
        )
    };
    take_bundle(&mut input, 32)?;
    let scope_fingerprint = take_bundle(&mut input, 32)?;
    let encoded_scope = take_sized(&mut input)?;
    if Sha256::digest(encoded_scope)[..] != *scope_fingerprint {
        return Err(SessionError::CorruptBundle);
    }
    let cursor_fingerprint = take_bundle(&mut input, 32)?;
    let encoded_cursor = take_sized(&mut input)?;
    if Sha256::digest(encoded_cursor)[..] != *cursor_fingerprint {
        return Err(SessionError::CorruptBundle);
    }
    let requested = ResourceUsage {
        worker_threads: usize::try_from(read_bundle_u64(&mut input)?)
            .map_err(|_| SessionError::CorruptBundle)?,
        resident_bytes: read_bundle_u64(&mut input)?,
        durable_bytes: read_bundle_u64(&mut input)?,
        elapsed_time: read_bundle_duration(&mut input)?,
        cpu_time: read_bundle_duration(&mut input)?,
        verification_requests: read_bundle_u64(&mut input)?,
    };
    let kernel_revision = read_bundle_u64(&mut input)?;
    let compatibility_prefix_len = bytes.len() - input.len();
    let environment = take_sized(&mut input)?;
    let completion_byte = take_bundle(&mut input, 1)?[0];
    let completion = match completion_byte {
        0 => None,
        1 => Some(Completion::ResourceEnvelopeExhausted),
        2 => Some(Completion::SuccessConditionsSatisfied),
        3 => Some(Completion::StoppedByObserver),
        4 => Some(Completion::NoEligibleWork),
        _ => return Err(SessionError::CorruptBundle),
    };
    if environment.is_empty() || !matches!((disposition, completion), (0, None) | (1, Some(_))) {
        return Err(SessionError::CorruptBundle);
    }
    let usage = ResourceUsage {
        worker_threads: usize::try_from(read_bundle_u64(&mut input)?)
            .map_err(|_| SessionError::CorruptBundle)?,
        resident_bytes: read_bundle_u64(&mut input)?,
        verification_requests: read_bundle_u64(&mut input)?,
        durable_bytes: read_bundle_u64(&mut input)?,
        elapsed_time: read_bundle_duration(&mut input)?,
        cpu_time: read_bundle_duration(&mut input)?,
    };
    if !input.is_empty() {
        return Err(SessionError::CorruptBundle);
    }
    #[cfg(not(feature = "internal-experiments"))]
    let _ = requested;
    Ok(DecodedSession {
        completed: disposition == 1,
        runtime_revision,
        restart_state_root,
        encoded_scope,
        encoded_cursor,
        #[cfg(feature = "internal-experiments")]
        requested,
        kernel_revision,
        compatibility_prefix_len,
        #[cfg(feature = "internal-experiments")]
        environment,
        #[cfg(feature = "internal-experiments")]
        completion,
        usage,
    })
}

#[cfg(feature = "internal-experiments")]
pub(crate) fn inspect_session_segment(
    bytes: &[u8],
) -> Result<crate::internal_experiments::SessionInspection, ()> {
    let decoded = decode_session_segment::<()>(bytes).map_err(|_| ())?;
    Ok(crate::internal_experiments::SessionInspection {
        completed: decoded.completed,
        requested: decoded.requested,
        kernel_revision: decoded.kernel_revision,
        environment: decoded.environment.to_vec(),
        completion: decoded.completion,
        usage: decoded.usage,
    })
}

#[cfg(feature = "internal-experiments")]
pub(crate) fn inspect_experience_segment(
    bytes: &[u8],
) -> Result<crate::internal_experiments::ExperienceInspection, ()> {
    let ledger = ExperienceLedger::decode(bytes)?;
    let consequence_candidates = |kind| {
        let attempts = ledger
            .consequences()
            .iter()
            .filter(|consequence| consequence.kind == kind)
            .map(|consequence| consequence.subject)
            .collect::<HashSet<_>>();
        ledger
            .entries()
            .iter()
            .filter(|entry| attempts.contains(&entry.attempt_id))
            .map(ExperienceEntry::candidate_fate_key)
            .collect::<HashSet<_>>()
    };
    let admitted_candidates = consequence_candidates(ConsequenceKind::Admitted);
    let improved_candidates = consequence_candidates(ConsequenceKind::ParetoImprovement);
    Ok(crate::internal_experiments::ExperienceInspection {
        attempts: experience_attempt_inspections(&ledger),
        candidate_fates: candidate_fate_inspections(
            &ledger,
            &admitted_candidates,
            &improved_candidates,
        ),
        consequence_count: ledger.consequences().len(),
        measurements: ledger
            .measurements()
            .iter()
            .map(
                |measurement| crate::internal_experiments::ExperienceMeasurementInspection {
                    environment: measurement.environment.clone(),
                    value_count: measurement.values.len(),
                },
            )
            .collect(),
    })
}

#[cfg(feature = "internal-experiments")]
fn inspect_allocation_queue(
    queue: AllocationQueue,
) -> crate::internal_experiments::CandidateAllocationQueueInspection {
    match queue {
        AllocationQueue::ProtectedOrigin => {
            crate::internal_experiments::CandidateAllocationQueueInspection::ProtectedOrigin
        }
        AllocationQueue::ProtectedDerived => {
            crate::internal_experiments::CandidateAllocationQueueInspection::ProtectedDerived
        }
        AllocationQueue::Learned => {
            crate::internal_experiments::CandidateAllocationQueueInspection::Learned
        }
        AllocationQueue::Bootstrap => {
            crate::internal_experiments::CandidateAllocationQueueInspection::Bootstrap
        }
    }
}

#[cfg(feature = "internal-experiments")]
fn experience_attempt_inspections(
    ledger: &ExperienceLedger,
) -> Vec<crate::internal_experiments::ExperienceAttemptInspection> {
    ledger
        .entries()
        .iter()
        .map(
            |entry| crate::internal_experiments::ExperienceAttemptInspection {
                candidate_key: entry.candidate_key.0,
                claim_digest: entry.claim_digest,
                canonical_candidate: entry.canonical_candidate.clone(),
                operator_symbol: entry.operator_symbol.clone(),
                support_key: entry
                    .proposal_provenance
                    .map(ProposalProvenance::support_key),
                verdict: match entry.verdict {
                    ExperienceVerdict::Accepted => {
                        crate::internal_experiments::ExperienceVerdictInspection::Accepted
                    }
                    ExperienceVerdict::Refuted => {
                        crate::internal_experiments::ExperienceVerdictInspection::Refuted
                    }
                    ExperienceVerdict::Unknown => {
                        crate::internal_experiments::ExperienceVerdictInspection::Unknown
                    }
                },
                allocation_queue: inspect_allocation_queue(entry.allocation_queue),
                verification_requests: entry.verification_requests,
                epoch: entry.epoch,
                feature_bits: entry.features.0.into_iter().map(f32::to_bits).collect(),
            },
        )
        .collect()
}

#[cfg(feature = "internal-experiments")]
fn candidate_fate_inspections(
    ledger: &ExperienceLedger,
    admitted: &HashSet<CandidateFateKey>,
    improved: &HashSet<CandidateFateKey>,
) -> Vec<crate::internal_experiments::CandidateFateInspection> {
    ledger
        .candidate_fates()
        .iter()
        .map(|fate| {
            let queue = fate.allocation_queue.map(inspect_allocation_queue);
            let was_admitted = admitted.contains(&fate.key());
            let was_improved = improved.contains(&fate.key());
            let verified = |verdict| {
                crate::internal_experiments::CandidateFateOutcomeInspection::Verified {
                    verdict,
                    allocation_queue: queue
                        .expect("Verified Candidate Fates retain queue attribution"),
                    bootstrap_rank: fate.bootstrap_rank.value(),
                    learned_rank: fate.learned_rank.value(),
                    admitted: was_admitted,
                    strict_improvement: was_improved,
                }
            };
            let outcome = match fate.disposition {
                CandidateFateDisposition::KnownArtifact => {
                    crate::internal_experiments::CandidateFateOutcomeInspection::NoveltyFiltered {
                        reason: crate::internal_experiments::CandidateNoveltyFilterReasonInspection::KnownArtifact,
                    }
                }
                CandidateFateDisposition::DuplicateCandidate => {
                    crate::internal_experiments::CandidateFateOutcomeInspection::NoveltyFiltered {
                        reason: crate::internal_experiments::CandidateNoveltyFilterReasonInspection::DuplicateCandidate,
                    }
                }
                CandidateFateDisposition::PriorNegativeExperience => {
                    crate::internal_experiments::CandidateFateOutcomeInspection::NoveltyFiltered {
                        reason: crate::internal_experiments::CandidateNoveltyFilterReasonInspection::PriorNegativeExperience,
                    }
                }
                CandidateFateDisposition::PolicyDeferred => {
                    crate::internal_experiments::CandidateFateOutcomeInspection::PolicyDeferred {
                        bootstrap_rank: fate.bootstrap_rank.value(),
                        learned_rank: fate.learned_rank.value(),
                    }
                }
                CandidateFateDisposition::VerificationInterrupted => {
                    crate::internal_experiments::CandidateFateOutcomeInspection::VerificationInterrupted {
                        allocation_queue: queue
                            .expect("selected Candidate Fates retain queue attribution"),
                        bootstrap_rank: fate.bootstrap_rank.value(),
                        learned_rank: fate.learned_rank.value(),
                    }
                }
                CandidateFateDisposition::VerifiedAccepted => {
                    verified(crate::internal_experiments::ExperienceVerdictInspection::Accepted)
                }
                CandidateFateDisposition::VerifiedRefuted => {
                    verified(crate::internal_experiments::ExperienceVerdictInspection::Refuted)
                }
                CandidateFateDisposition::VerifiedUnknown => {
                    verified(crate::internal_experiments::ExperienceVerdictInspection::Unknown)
                }
            };
            crate::internal_experiments::CandidateFateInspection {
                candidate_key: fate.candidate_key.0,
                claim_digest: fate.claim_digest,
                parent_key: fate.parent_key.0,
                operator_digest: fate.operator_digest,
                support_key: fate
                    .proposal_provenance
                    .map(ProposalProvenance::support_key),
                epoch: fate.epoch,
                generation_rank: fate.generation_rank,
                proposal_limit: fate.proposal_limit,
                policy_rank: fate.policy_rank.value(),
                verification_batch_cpu_ns: (fate.verification_batch_size != 0)
                    .then_some(fate.verification_batch_cpu_ns),
                verification_batch_size: (fate.verification_batch_size != 0)
                    .then_some(fate.verification_batch_size),
                outcome,
            }
        })
        .collect()
}

#[cfg(feature = "internal-experiments")]
pub(crate) fn force_first_experience_accepted(bytes: &[u8]) -> Result<Vec<u8>, ()> {
    let mut ledger = ExperienceLedger::decode(bytes)?;
    ledger.force_first_accepted_for_test()?;
    Ok(ledger.encode())
}

#[cfg(feature = "internal-experiments")]
pub(crate) fn pre_action_v23_experience_segment(bytes: &[u8]) -> Result<Vec<u8>, ()> {
    ExperienceLedger::decode(bytes).map(|ledger| ledger.encode_pre_action_v23_for_test())
}

#[cfg(feature = "internal-experiments")]
pub(crate) fn compare_candidate_features<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    source: &std::path::Path,
) -> Result<crate::internal_experiments::CandidateFeatureComparison, SessionError<D::Error>> {
    let resource_meter =
        ResourceEnvelopeGuard::start(&request.resources).map_err(|()| SessionError::Resource)?;
    let recovered = decode_bundle(domain, request, source, &resource_meter, 0)?;
    let feature_started = cpu_time::ProcessTime::now();
    let mut structure_scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    let identity = domain.semantic_identity();
    let mut parent_summaries = HashMap::with_capacity(recovered.artifacts.len());
    for artifact in &recovered.artifacts {
        let mut canonical = Vec::new();
        domain
            .structure()
            .encode_canonical(&artifact.artifact, &mut canonical, &mut structure_scratch)
            .map_err(SessionError::Domain)?;
        let key = ArtifactKey(stable_digest(identity.as_str(), &canonical));
        let summary =
            shape_summary(domain, &artifact.artifact).map_err(|()| SessionError::CorruptBundle)?;
        if parent_summaries.insert(key, summary).is_some() {
            return Err(SessionError::CorruptBundle);
        }
    }
    let baseline_attempts = recovered.ledger.attempts();
    let diagnostics = candidate_feature_diagnostics(&baseline_attempts, recovered.ledger.entries());
    let mut structural_attempts = baseline_attempts.clone();
    for (entry, attempt) in recovered
        .ledger
        .entries()
        .iter()
        .zip(&mut structural_attempts)
    {
        let parent = parent_summaries
            .get(&entry.parent_key)
            .ok_or(SessionError::CorruptBundle)?;
        let candidate = domain
            .structure()
            .decode_canonical(&entry.canonical_candidate, &mut structure_scratch)
            .map_err(|_| SessionError::CorruptBundle)?;
        let candidate =
            shape_summary(domain, &candidate).map_err(|()| SessionError::CorruptBundle)?;
        let operator =
            std::str::from_utf8(&entry.operator_symbol).map_err(|_| SessionError::CorruptBundle)?;
        attempt.features =
            structural_opportunity_features(parent, &candidate, operator, entry.proposal_features);
    }
    let feature_extraction_cpu = feature_started.elapsed();
    let legacy_learning = recovered.legacy_learning.unwrap_or_default();
    let comparison = legacy_learning
        .compare_feature_sets(
            &baseline_attempts,
            &structural_attempts,
            recovered.ledger.consequences(),
        )
        .map_err(|()| SessionError::CorruptBundle)?;
    Ok(crate::internal_experiments::CandidateFeatureComparison {
        examples: baseline_attempts.len(),
        feature_count: crate::learning::FEATURE_COUNT,
        claim_operator_feature_groups: diagnostics.groups,
        mixed_verdict_claim_operator_feature_groups: diagnostics.mixed_verdict_groups,
        accepted_examples_in_mixed_groups: diagnostics.accepted_in_mixed_groups,
        accepted_examples: diagnostics.accepted_examples,
        proposal_informed_examples: diagnostics.proposal_informed_examples,
        replay_claims: comparison.replay_claims,
        selection_claims: comparison.selection_claims,
        baseline_selection_loss: comparison.baseline_loss,
        structural_selection_loss: comparison.structural_loss,
        balanced_structural_selection_loss: comparison.balanced_structural_loss,
        baseline_training_cpu: comparison.baseline_training_cpu,
        structural_training_cpu: comparison.structural_training_cpu,
        balanced_structural_training_cpu: comparison.balanced_structural_training_cpu,
        feature_extraction_cpu,
        baseline_model_revision: comparison.baseline_revision,
        structural_model_revision: comparison.structural_revision,
        balanced_structural_model_revision: comparison.balanced_structural_revision,
        model_bytes: comparison.model_bytes,
        baseline_reproduces_champion: comparison.baseline_reproduces_champion,
        structural_promotes_over_baseline: comparison.structural_promotes,
        ranking_budgets: comparison.ranking_budgets,
        bootstrap_accepted_at_k: comparison.bootstrap_accepted_at_k,
        baseline_accepted_at_k: comparison.baseline_accepted_at_k,
        structural_accepted_at_k: comparison.structural_accepted_at_k,
        balanced_structural_accepted_at_k: comparison.balanced_structural_accepted_at_k,
        global_bootstrap_accepted_at_k: comparison.global_bootstrap_accepted_at_k,
        global_baseline_accepted_at_k: comparison.global_baseline_accepted_at_k,
        global_structural_accepted_at_k: comparison.global_structural_accepted_at_k,
        global_balanced_structural_accepted_at_k: comparison
            .global_balanced_structural_accepted_at_k,
        evaluated_at_k: comparison.evaluated_at_k,
        selection_accepted: comparison.selection_accepted,
    })
}

#[cfg(feature = "internal-experiments")]
fn candidate_feature_diagnostics(
    attempts: &[AttemptObservation],
    entries: &[ExperienceEntry],
) -> CandidateFeatureDiagnostics {
    assert_eq!(
        attempts.len(),
        entries.len(),
        "Experience attempts and durable entries must remain paired"
    );
    let mut outcomes =
        HashMap::<([u8; 32], Vec<u8>, [u32; crate::learning::FEATURE_COUNT]), (u8, usize)>::new();
    let mut accepted_examples = 0;
    for (attempt, entry) in attempts.iter().zip(entries) {
        let verdict = match attempt.verdict {
            crate::learning::VerdictTarget::Accepted => 1,
            crate::learning::VerdictTarget::Refuted => 2,
            crate::learning::VerdictTarget::Unknown => 4,
        };
        let accepted = usize::from(attempt.verdict == crate::learning::VerdictTarget::Accepted);
        accepted_examples += accepted;
        let group = outcomes
            .entry((
                attempt.claim,
                entry.operator_symbol.clone(),
                attempt.features.0.map(f32::to_bits),
            ))
            .or_default();
        group.0 |= verdict;
        group.1 += accepted;
    }
    CandidateFeatureDiagnostics {
        groups: outcomes.len(),
        mixed_verdict_groups: outcomes
            .values()
            .filter(|(verdicts, _)| verdicts.count_ones() > 1)
            .count(),
        accepted_in_mixed_groups: outcomes
            .values()
            .filter(|(verdicts, _)| verdicts.count_ones() > 1)
            .map(|(_, accepted)| *accepted)
            .sum(),
        accepted_examples,
        proposal_informed_examples: entries
            .iter()
            .filter(|entry| {
                entry
                    .proposal_features
                    .as_array()
                    .into_iter()
                    .any(|feature| feature != 0.0)
            })
            .count(),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the Runtime Controller keeps one Session epoch legible as a single orchestration path"
)]
fn improve_on_workers<D, O>(
    domain: &D,
    request: &ImprovementRequest<D>,
    mut observer: O,
    resource_meter: &ResourceEnvelopeGuard,
    worker_resident_bytes: u64,
    scheduler: &Scheduler,
    verification_workers: VerificationWorkerRequirements,
) -> Result<SessionOutcome<D>, SessionError<D::Error>>
where
    D: DomainDefinition,
    O: for<'a> FnMut(ParetoUpdate<'a, D>) -> ControlFlow<()>,
{
    let mut instrumentation = Recorder::from_environment();
    let setup_started = instrumentation.start();
    let goal_evaluator = GoalEvaluator::new(domain, &request.goals)?;
    let bundle_codec = RestartBundleCodec::new(domain, request)?;
    let worker_resident_bytes = worker_resident_bytes.saturating_add(bundle_codec.resident_bytes());
    if !resource_meter.reserve(ResidentReservation::live(worker_resident_bytes)) {
        return Err(SessionError::Resource);
    }
    let mut recovered_bundle = request
        .bundle
        .source()
        .map(|source| bundle_codec.recover(source, resource_meter, worker_resident_bytes))
        .transpose()?
        .unwrap_or_default();
    if request.bundle.forks() {
        if recovered_bundle.interrupted_usage.is_some() {
            return Err(SessionError::IncompatibleBundle);
        }
        recovered_bundle.frontier_keys.clear();
        recovered_bundle.deferred_candidates.clear();
        recovered_bundle.pending_parents.clear();
        recovered_bundle.generation_complete = false;
    }
    let recovered_has_search_tail = !recovered_bundle.deferred_candidates.is_empty()
        || !recovered_bundle.pending_parents.is_empty();
    let recovered_resident_bytes = recovered_bundle.resident_bytes;
    let recovered_keys = recovered_bundle.pareto_keys;
    let recovered_frontier_keys = recovered_bundle.frontier_keys;
    let recovered_deferred = recovered_bundle.deferred_candidates;
    let recovered_pending_parents = recovered_bundle.pending_parents;
    let mut recovered_generation_complete = recovered_bundle.generation_complete;
    let recovered_stored = recovered_bundle.artifacts;
    let mut ledger = recovered_bundle.ledger;
    let legacy_knowledge = recovered_bundle.legacy_knowledge.take();
    let legacy_learning = recovered_bundle.legacy_learning.take();
    let migrated_legacy_learning = legacy_learning.is_some();
    let recovered_policy = recovered_bundle.legacy_runtime_policy.take();
    let migrated_standalone_policy = recovered_policy.is_some();
    let mut recovered_intelligence = recovered_bundle.intelligence.take();
    let authenticated_intelligence_identity = recovered_bundle.authenticated_intelligence_identity;
    let mut runtime_policy = recovered_policy.as_ref().map_or_else(
        || {
            recovered_intelligence.as_ref().map_or_else(
                RuntimePolicyRevision::bootstrap,
                IntelligenceCore::runtime_policy_revision,
            )
        },
        RuntimePolicyState::active,
    );
    RuntimePolicyKernel::verify(&runtime_policy)
        .expect("the built-in Runtime Policy Revision must satisfy the immutable supervisor");
    let intelligence_limits = intelligence_limits(
        request,
        runtime_policy,
        worker_resident_bytes.saturating_add(recovered_resident_bytes),
    );
    let resumes_interrupted_envelope = recovered_bundle.interrupted_usage.is_some();
    let recovered_intelligence_revisions = recovered_intelligence.as_ref().map(|core| {
        (
            core.model_ecology_identity(),
            core.runtime_policy_revision().identity(),
            core.checkpoint().identity(),
        )
    });
    if let Some(policy) = recovered_policy {
        let restored = recovered_intelligence
            .take()
            .ok_or(SessionError::CorruptBundle)?;
        let mut restored = restored
            .into_legacy_runtime_policy(&policy)
            .map_err(|_| SessionError::CorruptBundle)?;
        restored = restored
            .into_legacy_learning(
                legacy_learning
                    .as_ref()
                    .and_then(LearningState::pinned_model)
                    .map(|model| model.conversion_view()),
            )
            .map_err(|_| SessionError::CorruptBundle)?;
        if let Some(knowledge) = legacy_knowledge.as_ref() {
            restored = restored
                .into_legacy_knowledge(knowledge)
                .map_err(|_| SessionError::CorruptBundle)?;
        }
        recovered_intelligence = Some(restored);
    }
    if recovered_intelligence.is_some()
        && !migrated_standalone_policy
        && let Some(knowledge) = legacy_knowledge.as_ref()
    {
        let restored = recovered_intelligence
            .take()
            .expect("the recovered Intelligence Core was just checked");
        recovered_intelligence = Some(
            restored
                .into_legacy_knowledge(knowledge)
                .map_err(|_| SessionError::CorruptBundle)?,
        );
    }
    if recovered_intelligence.is_none() && (legacy_learning.is_some() || legacy_knowledge.is_some())
    {
        let mut import = IntelligenceCore::fresh_for_legacy_import(intelligence_limits)
            .map_err(map_intelligence_error)?;
        import = import
            .into_legacy_learning(
                legacy_learning
                    .as_ref()
                    .and_then(LearningState::pinned_model)
                    .map(|model| model.conversion_view()),
            )
            .map_err(map_intelligence_error)?;
        if let Some(knowledge) = legacy_knowledge.as_ref() {
            import = import
                .into_legacy_knowledge(knowledge)
                .map_err(map_intelligence_error)?;
        }
        recovered_intelligence = Some(import.finalize().map_err(map_intelligence_error)?);
    }
    if !resumes_interrupted_envelope
        && recovered_intelligence
            .as_ref()
            .is_some_and(|core| core.opened_knowledge_verification().is_some())
    {
        return Err(SessionError::CorruptBundle);
    }
    let mut intelligence = match recovered_intelligence {
        Some(restored) if !resumes_interrupted_envelope => restored
            .into_fork_with_limits(intelligence_limits)
            .map_err(|_| SessionError::IncompatibleBundle)?,
        Some(restored) if resumes_interrupted_envelope => restored,
        Some(_) => return Err(SessionError::IncompatibleBundle),
        None => IntelligenceCore::fresh(intelligence_limits),
    };
    let pinned_knowledge = intelligence.pinned_knowledge_revision().clone();
    runtime_policy = intelligence.runtime_policy_revision();
    let interrupted_shadow_count = intelligence
        .experience()
        .shadow_campaigns()
        .filter(|campaign| {
            campaign.lifecycle() == crate::intelligence::ShadowCampaignLifecycle::Open
        })
        .count();
    if interrupted_shadow_count != 0 && !resumes_interrupted_envelope {
        return Err(SessionError::CorruptBundle);
    }
    let knowledge_manifest_bytes = u64::try_from(
        intelligence
            .knowledge_recovery_manifest_resident_bytes()
            .map_err(map_intelligence_error)?,
    )
    .unwrap_or(u64::MAX);
    if !resource_meter.reserve(
        ResidentReservation::live(
            worker_resident_bytes
                .saturating_add(recovered_resident_bytes)
                .saturating_add(intelligence.resident_bytes()),
        )
        .with_transient(knowledge_manifest_bytes),
    ) {
        return Err(SessionError::Resource);
    }
    let knowledge_recovery_manifest = intelligence
        .knowledge_recovery_manifest()
        .map_err(map_intelligence_error)?;
    let knowledge_replays = knowledge_recovery_manifest.obligation_count();
    let knowledge_replays_u64 =
        u64::try_from(knowledge_replays).map_err(|_| SessionError::Resource)?;
    let intelligence_working_bytes = intelligence
        .limits()
        .scratch_layout()
        .map_err(|_| SessionError::Resource)?
        .total_bytes()
        .saturating_add(9_u64.saturating_mul(
            (std::mem::size_of::<InvestmentReceipt>()
                + std::mem::size_of::<InvestmentSettlement>()
                + std::mem::size_of::<OperationalActionSpec>()) as u64,
        ));
    let recovered_revisions = recovered_bundle.revisions;
    let prior_usage = recovered_bundle.interrupted_usage.unwrap_or_default();
    if recovered_bundle.interrupted_usage.is_some() {
        resource_meter
            .resume(prior_usage)
            .map_err(|()| SessionError::Resource)?;
    }
    let recovered_replays = recovered_stored.len();
    let accepted_experience_replays = ledger.accepted_len();
    let verification_budget = resource_meter.verification_limit();
    let replay_budget = verification_budget
        .checked_sub(prior_usage.verification_requests)
        .and_then(|remaining| remaining.checked_sub(u64::try_from(recovered_replays).ok()?))
        .and_then(|remaining| {
            remaining.checked_sub(u64::try_from(accepted_experience_replays).ok()?)
        })
        .and_then(|remaining| remaining.checked_sub(knowledge_replays_u64))
        .ok_or(SessionError::Resource)?;
    let ReadSeeds {
        mut seeds,
        encoded_cursor: seed_cursor,
    } = read_seeds(domain, &request.seeds, replay_budget, resource_meter)?;
    let seed_replays = seeds.len();
    if seeds.is_empty() {
        return Err(SessionError::InvalidSeed);
    }
    let required_replays = recovered_replays
        .checked_add(seed_replays)
        .and_then(|count| count.checked_add(accepted_experience_replays))
        .and_then(|count| count.checked_add(knowledge_replays))
        .and_then(|count| u64::try_from(count).ok())
        .ok_or(SessionError::Resource)?;
    let mut verification_requests = prior_usage
        .verification_requests
        .checked_add(required_replays)
        .ok_or(SessionError::Resource)?;
    if verification_requests > verification_budget {
        return Err(SessionError::Resource);
    }
    if resource_meter
        .time_exhausted()
        .map_err(|()| SessionError::Resource)?
    {
        return Err(SessionError::Resource);
    }
    let recovery_resident = worker_resident_bytes
        .saturating_add(recovered_resident_bytes)
        .saturating_add(intelligence_working_bytes)
        .saturating_add(knowledge_manifest_bytes)
        .saturating_add(vector_bytes(&seeds));
    let failure_publication_bytes = if verification_workers.worker_lanes() == 0 {
        0
    } else if let Some(source) = request.bundle.source() {
        let source_bytes = std::fs::metadata(source)
            .map_err(SessionError::Durability)?
            .len();
        bundle_codec.replacement_transient_bytes(source_bytes, seed_cursor.len(), false)
    } else {
        let state = RestartBundleState::new(
            &[],
            &[],
            SearchTailView::new(&[], &[], &[]),
            &ledger,
            &intelligence,
        );
        bundle_codec
            .seal_plan(
                &seed_cursor,
                &state,
                intelligence.checkpoint().as_bytes().len(),
            )
            .peak_transient_bytes()
    };
    if !resource_meter.reserve(
        ResidentReservation::live(recovery_resident).with_transient(failure_publication_bytes),
    ) {
        return Err(SessionError::Resource);
    }
    let mut intelligence_market = MarketArena::try_with_capacity(intelligence.limits())
        .map_err(|_| SessionError::Resource)?;
    let mut intelligence_portfolio = PortfolioBuffer::try_with_capacity(intelligence.limits())
        .map_err(|_| SessionError::Resource)?;
    let mut operational_receipts = Vec::new();
    operational_receipts
        .try_reserve_exact(9)
        .map_err(|_| SessionError::Resource)?;
    let mut operational_settlements = Vec::new();
    operational_settlements
        .try_reserve_exact(9)
        .map_err(|_| SessionError::Resource)?;
    let mut operational_specs = Vec::new();
    operational_specs
        .try_reserve_exact(9)
        .map_err(|_| SessionError::Resource)?;
    debug_assert!(
        intelligence_market
            .resident_bytes()
            .saturating_add(intelligence_portfolio.resident_bytes())
            <= intelligence_working_bytes
    );
    let stored_replay = replay_stored(
        domain,
        &recovered_stored,
        scheduler,
        resource_meter,
        verification_workers,
        recovery_resident,
    );
    if let Err(error) = stored_replay {
        if charged_external_interruption(&error, verification_workers) {
            let usage = resource_meter
                .usage(verification_requests, 0)
                .map_err(|()| SessionError::Resource)?;
            persist_setup_interruption(
                request,
                &bundle_codec,
                &seed_cursor,
                &ledger,
                &intelligence,
                usage,
                resource_meter,
                recovery_resident,
            )?;
        }
        return Err(error);
    }
    if resource_meter
        .time_exhausted()
        .map_err(|()| SessionError::Resource)?
    {
        return Err(SessionError::Resource);
    }
    let seed_replay = replay_seeds(
        domain,
        &seeds,
        scheduler,
        resource_meter,
        verification_workers,
        recovery_resident,
    );
    if let Err(error) = seed_replay {
        if charged_external_interruption(&error, verification_workers) {
            let usage = resource_meter
                .usage(verification_requests, 0)
                .map_err(|()| SessionError::Resource)?;
            persist_setup_interruption(
                request,
                &bundle_codec,
                &seed_cursor,
                &ledger,
                &intelligence,
                usage,
                resource_meter,
                recovery_resident,
            )?;
        }
        return Err(error);
    }
    let environment = crate::MeasurementEnvironment::local_process();
    let recovered = materialize(
        domain,
        bundle_codec.semantic_identity(),
        recovered_stored,
        &environment,
        resource_meter,
        recovery_resident,
    )?;
    let experience_replay = replay_accepted_experience(
        domain,
        &recovered,
        ledger.entries(),
        scheduler,
        resource_meter,
        verification_workers,
        recovery_resident,
    );
    if let Err(error) = experience_replay {
        if charged_external_interruption(&error, verification_workers) {
            let usage = resource_meter
                .usage(verification_requests, 0)
                .map_err(|()| SessionError::Resource)?;
            persist_setup_interruption(
                request,
                &bundle_codec,
                &seed_cursor,
                &ledger,
                &intelligence,
                usage,
                resource_meter,
                recovery_resident,
            )?;
        }
        return Err(error);
    }
    let knowledge_replay_transient = knowledge_recovery_replay_resident_bytes::<D>(
        domain,
        &recovered,
        &ledger,
        knowledge_recovery_manifest
            .obligations()
            .iter()
            .map(KnowledgeRecoveryObligation::work),
    )?;
    if !resource_meter.reserve(
        ResidentReservation::live(recovery_resident)
            .with_transient(knowledge_replay_transient)
            .with_pending_durability(0),
    ) {
        return Err(SessionError::Resource);
    }
    let knowledge_replay = replay_knowledge_obligations(
        domain,
        &recovered,
        &ledger,
        knowledge_recovery_manifest
            .obligations()
            .iter()
            .map(KnowledgeRecoveryObligation::work),
        scheduler,
        resource_meter,
        verification_workers,
        recovery_resident,
    );
    if let Err(error) = knowledge_replay {
        if charged_external_interruption(&error, verification_workers) {
            let usage = resource_meter
                .usage(verification_requests, 0)
                .map_err(|()| SessionError::Resource)?;
            persist_setup_interruption(
                request,
                &bundle_codec,
                &seed_cursor,
                &ledger,
                &intelligence,
                usage,
                resource_meter,
                recovery_resident,
            )?;
        }
        return Err(error);
    }
    if !intelligence.knowledge_recovery_manifest_matches(&knowledge_recovery_manifest) {
        return Err(SessionError::CorruptBundle);
    }
    drop(knowledge_recovery_manifest);
    if recovered_revisions.is_some_and(|revisions| {
        let expected = revision_ids(
            domain.semantic_identity().as_str(),
            &recovered,
            &intelligence,
            recovered_intelligence_revisions.map_or_else(
                || intelligence.checkpoint().identity(),
                |(_, _, identity)| identity,
            ),
        );
        let expected_knowledge =
            legacy_knowledge
                .as_ref()
                .map_or(expected.knowledge, |knowledge| {
                    legacy_knowledge_revision_id(
                        domain.semantic_identity().as_str(),
                        &recovered,
                        knowledge,
                    )
                });
        let knowledge_bad = revisions.knowledge != expected_knowledge;
        let model_bad = !migrated_legacy_learning
            && revisions.model
                != recovered_intelligence_revisions.map_or(expected.model, |(model, _, _)| model);
        let policy_bad = !migrated_standalone_policy
            && revisions.runtime_policy != [0; 32]
            && revisions.runtime_policy
                != recovered_intelligence_revisions
                    .map_or(expected.runtime_policy, |(_, policy, _)| policy);
        // The raw checkpoint was authenticated against the outer header before
        // restore. Its canonical identity may change during format or
        // standalone-state migration, so never compare the migrated identity
        // with a header that committed the raw bytes.
        let intelligence_bad = !migrated_standalone_policy
            && revisions.intelligence != [0; 32]
            && revisions.intelligence
                != authenticated_intelligence_identity.unwrap_or(expected.intelligence);
        knowledge_bad || model_bad || policy_bad || intelligence_bad
    }) {
        return Err(SessionError::CorruptBundle);
    }
    if recovered.iter().any(|artifact| {
        !recovered
            .iter()
            .any(|known| known.key() == artifact.inner.origin_key)
            || artifact
                .inner
                .parent_key
                .is_some_and(|parent| !recovered.iter().any(|known| known.key() == parent))
    }) {
        return Err(SessionError::CorruptBundle);
    }
    if recovered_keys
        .iter()
        .any(|key| !recovered.iter().any(|artifact| artifact.key() == *key))
    {
        return Err(SessionError::CorruptBundle);
    }
    let seed_stored = seeds
        .drain(..)
        .map(|seed| StoredArtifact {
            artifact: seed.artifact,
            verification: seed.verification,
            provenance: seed.provenance,
            origin_key: None,
            parent_key: None,
        })
        .collect::<Vec<_>>();
    let roots = materialize(
        domain,
        bundle_codec.semantic_identity(),
        seed_stored,
        &environment,
        resource_meter,
        recovery_resident,
    )?;
    let recovered_goal_frontiers = goal_evaluator.frontiers(&recovered);
    let mut known = recovered.clone();
    extend_unique(&mut known, roots.iter().cloned());
    let initial_goal_frontiers = goal_evaluator.frontiers(&known);
    let mut pareto = GoalEvaluator::<D>::pareto_from_frontiers(&initial_goal_frontiers);
    let resuming_interrupted = recovered_bundle.interrupted_usage.is_some();
    let restoring_search_tail = resuming_interrupted || recovered_has_search_tail;
    let mut frontier = if restoring_search_tail {
        recovered_frontier_keys
            .into_iter()
            .map(|key| {
                let artifact = known
                    .iter()
                    .find(|artifact| artifact.key() == key)
                    .cloned()
                    .ok_or(SessionError::CorruptBundle)?;
                let origin = roots
                    .iter()
                    .position(|root| root.key() == artifact.inner.origin_key)
                    .ok_or(SessionError::CorruptBundle)?;
                Ok((artifact, origin))
            })
            .collect::<Result<Vec<_>, SessionError<D::Error>>>()?
    } else {
        let mut selected_frontier = roots
            .iter()
            .cloned()
            .enumerate()
            .map(|(origin, artifact)| (artifact, origin))
            .collect::<Vec<_>>();
        for artifact in recovered {
            if let Some(origin) = roots
                .iter()
                .position(|root| root.key() == artifact.inner.origin_key)
                && pinned_knowledge.schedules(artifact.key().0)
                && !selected_frontier
                    .iter()
                    .any(|(scheduled, _)| scheduled.key() == artifact.key())
            {
                selected_frontier.push((artifact, origin));
            }
        }
        selected_frontier
    };
    let mut deferred_candidates = recovered_deferred
        .into_iter()
        .map(|deferred| {
            let source_index = frontier
                .iter()
                .position(|(parent, _)| parent.key() == deferred.parent_key)
                .ok_or(SessionError::CorruptBundle)?;
            let operator_is_known = domain.operators().catalog().iter().any(|descriptor| {
                descriptor.symbol().as_str().as_bytes() == deferred.operator_symbol
            }) || pinned_knowledge
                .resolve_operator(&deferred.operator_symbol)
                .is_some();
            if !operator_is_known {
                return Err(SessionError::CorruptBundle);
            }
            Ok(ProposedCandidate {
                candidate: Candidate {
                    source_index,
                    artifact: deferred.artifact,
                    proposal_features: deferred.proposal_features,
                    proposal_provenance: deferred.proposal_provenance,
                },
                canonical_candidate: deferred.canonical_candidate,
                operator_symbol: deferred.operator_symbol,
                operator_digest: [0; 32],
                features: Features([0.0; FEATURE_COUNT]),
                epoch: 0,
                proposal_limit: deferred.proposal_limit,
                protected_derived: deferred.protected_derived,
                allocation_queue: AllocationQueue::Bootstrap,
                fate_index: usize::MAX,
                bootstrap_rank: CandidateRank::absent(),
                learned_rank: CandidateRank::absent(),
                generated_in_epoch: false,
                published_in_epoch: false,
                intelligence_receipt: None,
                action_decision: deferred.action_decision,
                causal_parent_key: deferred.causal_parent_key,
            })
        })
        .collect::<Result<Vec<_>, SessionError<D::Error>>>()?;
    let has_derived_operators = pinned_knowledge
        .operators()
        .iter()
        .any(crate::knowledge::DerivedOperator::active);
    let primitive_operator_count = domain.operators().catalog().len();
    let mut pending_parents = if restoring_search_tail {
        recovered_pending_parents
    } else {
        frontier
            .iter()
            .map(|(artifact, _)| {
                PendingParent::new(
                    artifact.key(),
                    primitive_operator_count,
                    has_derived_operators,
                )
            })
            .collect()
    };
    if pending_parents.iter().any(|pending| {
        pending.primitive_offsets.len() != primitive_operator_count
            || !frontier
                .iter()
                .any(|(artifact, _)| artifact.key() == pending.key)
    }) {
        return Err(SessionError::CorruptBundle);
    }
    if !has_derived_operators {
        for pending in &mut pending_parents {
            pending.derived_sampled = true;
        }
    }
    let initial_usage = resource_meter
        .usage(verification_requests, 0)
        .map_err(|()| SessionError::Resource)?;
    let operators = domain
        .operators()
        .catalog()
        .iter()
        .map(crate::OperatorDescriptor::operator)
        .collect::<Vec<_>>();
    let empty_checkpoint = Vec::new();
    let initial_seal_live = worker_resident_bytes
        .saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &empty_checkpoint,
            &ledger,
            &intelligence,
            intelligence_working_bytes,
        ))
        .saturating_add(cohort::recovery_resident_bytes(
            domain,
            &deferred_candidates,
            &pending_parents,
        ));
    let mut checkpoint = bundle_codec.seal_admitted(
        &seed_cursor,
        RestartBundleState::new(
            &known,
            &pareto,
            SearchTailView::new(&frontier, &deferred_candidates, &pending_parents),
            &ledger,
            &intelligence,
        ),
        SessionSeal::Interrupted(initial_usage),
        resource_meter,
        SealAdmission {
            resident_overlap: initial_seal_live,
            additional_transient: 0,
            pending_durability: 0,
        },
    )?;
    let mut parent_ranks = goal_evaluator.parent_ranks(&frontier);
    let mut initial_resident = worker_resident_bytes
        .saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &checkpoint,
            &ledger,
            &intelligence,
            intelligence_working_bytes,
        ))
        .saturating_add(cohort::recovery_resident_bytes(
            domain,
            &deferred_candidates,
            &pending_parents,
        ));
    if !resource_meter.reserve(ResidentReservation::live(initial_resident)) {
        return Err(SessionError::Resource);
    }
    let mut durability = durability::CheckpointWriter::start(request.bundle.target().to_path_buf())
        .map_err(SessionError::Durability)?;
    durability
        .submit(checkpoint)
        .map_err(SessionError::Durability)?;
    checkpoint = durability
        .barrier_retaining()
        .map(|(_, checkpoint)| checkpoint)
        .map_err(SessionError::Durability)?;
    if interrupted_shadow_count != 0 {
        let update_bytes = allocation_bytes::<ShadowUpdate, _>(interrupted_shadow_count)?;
        let transition_bytes =
            intelligence.shadow_interruption_transition_resident_bytes(interrupted_shadow_count);
        if !resource_meter.reserve(
            ResidentReservation::live(initial_resident)
                .with_transient(update_bytes.saturating_add(transition_bytes))
                .with_pending_durability(durability.pending_bytes()),
        ) {
            return Err(SessionError::Resource);
        }
        let mut interrupted_shadows = exact_vec(interrupted_shadow_count)?;
        interrupted_shadows.extend(
            intelligence
                .experience()
                .shadow_campaigns()
                .filter(|campaign| {
                    campaign.lifecycle() == crate::intelligence::ShadowCampaignLifecycle::Open
                })
                .map(|campaign| ShadowUpdate::interrupted(campaign.specification().id())),
        );
        let transition = intelligence
            .stage(SettlementFrame::observations(
                &[],
                &[],
                &[],
                &interrupted_shadows,
            ))
            .map_err(map_intelligence_error)?;
        let publication_live = initial_resident
            .saturating_add(update_bytes)
            .saturating_add(transition_bytes);
        checkpoint = publish_intelligence_transition(
            transition,
            &mut intelligence,
            &seed_cursor,
            &IntelligencePublicationState {
                artifacts: &known,
                pareto: &pareto,
                frontier: &frontier,
                deferred_candidates: &deferred_candidates,
                pending_parents: &pending_parents,
                generation_complete: false,
                ledger: &ledger,
            },
            &checkpoint,
            verification_requests,
            publication_live,
            &bundle_codec,
            resource_meter,
            &mut durability,
        )?;
        initial_resident = worker_resident_bytes
            .saturating_add(resident_state_bytes(
                &known,
                &roots,
                &pareto,
                &frontier,
                &recovered_keys,
                &operators,
                &checkpoint,
                &ledger,
                &intelligence,
                intelligence_working_bytes,
            ))
            .saturating_add(cohort::recovery_resident_bytes(
                domain,
                &deferred_candidates,
                &pending_parents,
            ));
    }
    if let Some(opened) = intelligence.opened_knowledge_verification() {
        if !resuming_interrupted {
            return Err(SessionError::CorruptBundle);
        }
        let settlement_resident = intelligence
            .knowledge_settlement_transition_resident_bytes(&opened, opened.receipts().len())
            .map_err(map_intelligence_error)?;
        if !resource_meter.reserve(
            ResidentReservation::live(initial_resident)
                .with_transient(settlement_resident)
                .with_pending_durability(durability.pending_bytes()),
        ) {
            return Err(SessionError::Resource);
        }
        let transition = intelligence
            .settle_opened_knowledge_verification(
                &opened,
                KnowledgeVerificationReport::interrupted(),
            )
            .map_err(map_intelligence_error)?;
        test_fault_point("knowledge-verification-recovery-interruption-staged");
        let next_checkpoint = publish_intelligence_transition(
            transition,
            &mut intelligence,
            &seed_cursor,
            &IntelligencePublicationState {
                artifacts: &known,
                pareto: &pareto,
                frontier: &frontier,
                deferred_candidates: &deferred_candidates,
                pending_parents: &pending_parents,
                generation_complete: false,
                ledger: &ledger,
            },
            &checkpoint,
            verification_requests,
            initial_resident,
            &bundle_codec,
            resource_meter,
            &mut durability,
        )?;
        let terminal_resident = worker_resident_bytes
            .saturating_add(resident_state_bytes(
                &known,
                &roots,
                &pareto,
                &frontier,
                &recovered_keys,
                &operators,
                &next_checkpoint,
                &ledger,
                &intelligence,
                intelligence_working_bytes,
            ))
            .saturating_add(cohort::recovery_resident_bytes(
                domain,
                &deferred_candidates,
                &pending_parents,
            ));
        checkpoint = next_checkpoint;
        initial_resident = terminal_resident;
        test_fault_point("knowledge-verification-recovery-interruption-published");
    }
    let mut sequence = 0_u64;
    let initial_delivery = deliver_delta(
        &mut observer,
        &mut sequence,
        &recovered_keys,
        &pareto,
        &goal_evaluator.affected(&recovered_goal_frontiers, &initial_goal_frontiers),
        resource_meter,
        initial_resident,
    );
    let mut stopped_by_observer = initial_delivery.stopped();
    let mut success_conditions_satisfied =
        goal_evaluator.success_satisfied(&initial_goal_frontiers);
    let mut time_exhausted = resource_meter
        .search_time_exhausted()
        .map_err(|()| SessionError::Resource)?;
    let mut verification_budget_exhausted = false;
    let mut durable_budget_exhausted = false;
    let mut resident_budget_exhausted = initial_delivery.resource_exhausted();
    let mut policy_trial_invalidated = false;
    let mut selection_epoch = ledger
        .entries()
        .iter()
        .map(|entry| entry.epoch)
        .chain(ledger.candidate_fates().iter().map(|fate| fate.epoch))
        .max()
        .map_or(Some(0_u64), |epoch| epoch.checked_add(1))
        .ok_or(SessionError::CorruptBundle)?;
    let knowledge_campaign_known_len = known.len();
    let knowledge_campaign_experience_len = ledger.entries().len();
    let knowledge_campaign_epoch = selection_epoch;
    let knowledge_campaign_checkpoint = CheckpointDigest::new(Sha256::digest(&checkpoint).into());
    let knowledge_campaign_intelligence = intelligence.checkpoint();
    let mut operator_scratch = <D::Operators as OperatorAlgebra<D>>::Scratch::default();
    instrumentation.finish(Phase::Setup, setup_started);

    while !stopped_by_observer
        && !success_conditions_satisfied
        && !time_exhausted
        && !resident_budget_exhausted
        && !frontier.is_empty()
    {
        operational_receipts.clear();
        operational_settlements.clear();
        operational_specs.clear();
        runtime_policy = intelligence.runtime_policy_revision();
        instrumentation.epoch();
        let generation_started = instrumentation.start();
        let resident_before_epoch = worker_resident_bytes
            .saturating_add(resident_state_bytes(
                &known,
                &roots,
                &pareto,
                &frontier,
                &recovered_keys,
                &operators,
                &checkpoint,
                &ledger,
                &intelligence,
                intelligence_working_bytes,
            ))
            .saturating_add(
                (frontier.len() as u64).saturating_mul(std::mem::size_of::<&D::Artifact>() as u64),
            )
            .saturating_add(
                (frontier.len() as u64).saturating_mul(std::mem::size_of::<usize>() as u64),
            )
            .saturating_add(cohort::recovery_resident_bytes(
                domain,
                &deferred_candidates,
                &pending_parents,
            ));
        if !resource_meter.reserve(ResidentReservation::live(resident_before_epoch)) {
            instrumentation.refused(ResourceRefusal::ResidentEpoch);
            resident_budget_exhausted = true;
            break;
        }
        let remaining_verifications = verification_budget.saturating_sub(verification_requests);
        if remaining_verifications == 0 {
            instrumentation.refused(ResourceRefusal::VerificationBudget);
            verification_budget_exhausted = true;
            break;
        }
        let parents = frontier
            .iter()
            .map(|(artifact, _)| artifact.artifact())
            .collect::<Vec<_>>();
        let parent_summaries = parents
            .iter()
            .map(|artifact| StructuralSummary {
                node_count: structural_node_count(domain, artifact),
            })
            .collect::<Vec<_>>();
        let parent_claims = frontier
            .iter()
            .map(|(artifact, _)| artifact.inner.claim_digest)
            .collect::<Vec<_>>();
        let origins = frontier
            .iter()
            .map(|(_, origin)| *origin)
            .collect::<Vec<_>>();
        let frontier_index_by_key = frontier
            .iter()
            .enumerate()
            .map(|(index, (artifact, _))| (artifact.key(), index))
            .collect::<HashMap<_, _>>();
        let pending_parent_indexes = pending_parents
            .iter()
            .map(|pending| {
                *frontier_index_by_key
                    .get(&pending.key)
                    .expect("pending parent retains a Search Frontier Artifact")
            })
            .collect::<Vec<_>>();
        let mut candidates = Vec::new();
        let mut application_bytes = vector_bytes(&pending_parent_indexes);
        let mut covered_claims = ledger
            .entries()
            .iter()
            .map(|entry| entry.claim_digest)
            .collect::<Vec<_>>();
        covered_claims.sort_unstable();
        covered_claims.dedup();
        let remaining = usize::try_from(remaining_verifications).unwrap_or(usize::MAX);
        let cohort_limit = cohort::limit(
            remaining,
            verification_workers.worker_lanes(),
            usize::from(runtime_policy.verification_cohort()),
            &parent_claims,
            &covered_claims,
        );
        let policy_trial_sequence =
            runtime_policy_trial_sequence(selection_epoch, runtime_policy.shadow_cadence());
        let policy_trial_due = policy_trial_sequence.is_some();
        let inventory_cohort_limit = cohort_limit
            .saturating_add(usize::from(policy_trial_due))
            .min(remaining);
        let catalog = domain.operators().catalog();
        let available_resident = resource_meter.available_resident(resident_before_epoch);
        let generation_inventory_target = candidate_generation_limit(
            inventory_cohort_limit,
            remaining_verifications,
            pending_parent_indexes.len(),
            catalog.len(),
            available_resident,
            usize::from(runtime_policy.lookahead_depth()),
            usize::from(runtime_policy.shortlist_width()),
        );
        let resume_at_selection = std::mem::take(&mut recovered_generation_complete);
        let generation_limit = if resume_at_selection {
            0
        } else {
            generation_refill_limit(generation_inventory_target, deferred_candidates.len())
        };
        if generation_inventory_target == 0 && deferred_candidates.is_empty() {
            instrumentation.refused(ResourceRefusal::ResidentEpoch);
            resident_budget_exhausted = true;
            break;
        }
        let generation_transient_bound =
            (generation_limit as u64).saturating_mul(MIN_CHOICE_RESIDENT_BYTES);
        if !resource_meter.reserve(
            ResidentReservation::live(resident_before_epoch)
                .with_transient(generation_transient_bound),
        ) {
            instrumentation.refused(ResourceRefusal::ResidentEpoch);
            resident_budget_exhausted = true;
            break;
        }
        let has_derived = has_derived_operators
            && pending_parents
                .iter()
                .any(|pending| !pending.derived_sampled);
        let derived_budget = if has_derived {
            proportional_budget(generation_limit, runtime_policy.exploration_per_mille())
        } else {
            0
        };
        let primitive_budget = generation_limit.saturating_sub(derived_budget);
        let protected_primitive_budget = primitive_budget.min(pending_parent_indexes.len().max(1));
        let discretionary_primitive_budget = if policy_trial_due {
            0
        } else {
            primitive_budget.saturating_sub(protected_primitive_budget)
        };
        let action_limit = usize::from(discretionary_primitive_budget > 0)
            .saturating_add(usize::from(discretionary_primitive_budget > 1))
            .min(catalog.len().min(9));
        let operational_allocations = if action_limit == 0 {
            None
        } else {
            let application_count = discretionary_primitive_budget.div_ceil(action_limit);
            let applications = NonZeroU32::new(
                u32::try_from(application_count).map_err(|_| SessionError::Resource)?,
            )
            .ok_or(SessionError::Resource)?;
            let allowance = remaining_resource_allowance(
                request,
                resource_meter,
                verification_requests,
                resident_before_epoch,
                checkpoint.len() as u64,
            )?;
            let divisor = u64::try_from(action_limit).map_err(|_| SessionError::Resource)?;
            let action_resources = ResourceVector::new(
                allowance.cpu_time_ns / divisor,
                generation_transient_bound / divisor,
                0,
                allowance.elapsed_time_ns / divisor,
                0,
            );
            let rotation = usize::try_from(selection_epoch).unwrap_or(usize::MAX) % catalog.len();
            for slot in 0..operational_spec_limit(
                catalog.len(),
                intelligence.limits().maximum_opportunities,
            ) {
                let index = rotation.saturating_add(slot) % catalog.len();
                let descriptor = &catalog[index];
                let operator = SubjectId::new(stable_digest(
                    "reflex-primitive-operator-action-v1",
                    descriptor.symbol().as_str().as_bytes(),
                ));
                let family = RoutingFamilyId::new(stable_digest(
                    "reflex-primitive-operator-family-v1",
                    descriptor.symbol().as_str().as_bytes(),
                ));
                let mut features = [0.0_f32; 8];
                features[0] = 1.0;
                features[1] = bounded_usize_f32(generation_limit) / 4096.0;
                features[2] = bounded_usize_f32(pending_parent_indexes.len()) / 4096.0;
                features[3] = bounded_usize_f32(catalog.len()) / 4096.0;
                features[4] = bounded_usize_f32(index) / bounded_usize_f32(catalog.len()).max(1.0);
                features[5] = f32::from(has_derived);
                let prior =
                    ledger.entries().iter().rev().find(|entry| {
                        entry.operator_symbol == descriptor.symbol().as_str().as_bytes()
                    });
                let repair = prior.filter(|entry| {
                    entry.verdict == ExperienceVerdict::Refuted
                        && entry.rejection_advisory.is_some()
                        && frontier
                            .iter()
                            .any(|(artifact, _)| artifact.key() == entry.parent_key)
                });
                let action = if let Some(rejection) = repair {
                    let (advisory, class, has_counterexample) = rejection_advisory_profile(
                        rejection
                            .rejection_advisory
                            .expect("a repair action requires a retained advisory"),
                    );
                    features[6] = class;
                    features[7] = has_counterexample;
                    OperationalAction::Repair {
                        rejection: SubjectId::new(rejection.attempt_id),
                        parent: SubjectId::new(rejection.parent_key.0),
                        advisory: SubjectId::new(advisory),
                        operator,
                        applications,
                        routing_family: family,
                    }
                } else if prior.is_none() {
                    OperationalAction::Explore {
                        question: SubjectId::new(stable_digest(
                            "reflex-unseen-operator-question-v1",
                            descriptor.symbol().as_str().as_bytes(),
                        )),
                        operator,
                        applications,
                        routing_family: family,
                    }
                } else {
                    OperationalAction::Generate {
                        operator,
                        applications,
                        routing_family: family,
                    }
                };
                operational_specs.push(OperationalActionSpec::new(
                    action,
                    action_resources,
                    u32::try_from(slot).map_err(|_| SessionError::Resource)?,
                    features,
                ));
            }
            let prepared =
                PreparedOperationalMarket::prepare(&operational_specs, &mut intelligence_market)
                    .map_err(map_intelligence_error)?;
            Some(
                prepared
                    .allocate(
                        &intelligence,
                        &intelligence_market,
                        allowance,
                        selection_epoch,
                        action_limit,
                        &mut intelligence_portfolio,
                    )
                    .map_err(map_intelligence_error)?,
            )
        };
        let primitive_parent_capacity = generation::parent_capacity(
            protected_primitive_budget,
            catalog.len(),
            &pending_parent_indexes,
            &parent_claims,
        );
        let generation_parent_indexes = if generation_limit == 0 {
            Vec::new()
        } else {
            let parent_capacity = generation_limit
                .min(primitive_parent_capacity)
                .max(usize::from(primitive_budget == 0));
            generation::select_pending_parents(
                &pending_parent_indexes,
                &parent_claims,
                parent_capacity,
            )
        };
        let generation_parents = generation_parent_indexes
            .iter()
            .map(|index| parents[*index])
            .collect::<Vec<_>>();
        let mut staged_parent_progress = generation_parent_indexes
            .iter()
            .map(|index| {
                let key = frontier[*index].0.key();
                pending_parents
                    .iter()
                    .find(|pending| pending.key == key)
                    .cloned()
                    .expect("selected pending parent retains enumeration progress")
            })
            .collect::<Vec<_>>();
        application_bytes = application_bytes
            .saturating_add(vector_bytes(&generation_parent_indexes))
            .saturating_add(vector_bytes(&generation_parents))
            .saturating_add(vector_bytes(&staged_parent_progress))
            .saturating_add(staged_parent_progress.iter().fold(0_u64, |bytes, parent| {
                bytes.saturating_add(parent.resident_bytes())
            }));
        let candidate_epoch = selection_epoch;
        selection_epoch = selection_epoch
            .checked_add(1)
            .ok_or(SessionError::CorruptBundle)?;
        candidates.append(&mut deferred_candidates);
        let carried_candidate_count = candidates.len();
        for candidate in &mut candidates {
            candidate.epoch = candidate_epoch;
            let operator = std::str::from_utf8(&candidate.operator_symbol)
                .expect("canonical Operator symbols are UTF-8");
            let parent = parent_summaries
                .get(candidate.candidate.source_index)
                .expect("deferred Candidates retain a Search Frontier parent");
            candidate.features = opportunity_features(
                domain,
                *parent,
                &candidate.candidate.artifact,
                operator_feature_values(operator),
                candidate_epoch,
                candidate.candidate.proposal_features,
            );
            candidate.fate_index = usize::MAX;
            candidate.bootstrap_rank = CandidateRank::absent();
            candidate.learned_rank = CandidateRank::absent();
            candidate.generated_in_epoch = false;
        }
        let mut remaining_primitive_budget = protected_primitive_budget;
        for (parent_position, ((parent, source_index), progress)) in generation_parents
            .iter()
            .zip(&generation_parent_indexes)
            .zip(&mut staged_parent_progress)
            .enumerate()
        {
            if resource_meter
                .search_time_exhausted()
                .map_err(|()| SessionError::Resource)?
            {
                instrumentation.refused(ResourceRefusal::Time);
                time_exhausted = true;
                break;
            }
            let parents_left = generation_parents.len() - parent_position;
            let parent_budget = remaining_primitive_budget.div_ceil(parents_left);
            let before = candidates.len();
            let parent_artifacts = [*parent];
            let parent_indexes = [*source_index];
            let primitive_bytes = StructuredRewriteEngine
                .propose(
                    domain,
                    &mut StructuredRewriteRequest {
                        parents: ProposalParents {
                            artifacts: &parent_artifacts,
                            frontier_indexes: &parent_indexes,
                        },
                        operator_offsets: &mut progress.primitive_offsets,
                        limit: parent_budget,
                        epoch: candidate_epoch,
                        permitted_operators: None,
                    },
                    &mut operator_scratch,
                    &mut candidates,
                )?
                .resident_bytes();
            remaining_primitive_budget =
                remaining_primitive_budget.saturating_sub(candidates.len() - before);
            application_bytes = application_bytes.max(primitive_bytes);
        }
        if time_exhausted {
            deferred_candidates = cohort::rollback_unverified_generation(candidates, Vec::new());
            break;
        }
        let mut remaining_discretionary_budget = discretionary_primitive_budget;
        if let Some(allocations) = operational_allocations {
            for allocation in allocations.into_iter() {
                let action_candidate_start = candidates.len();
                let mut repair_seed = None;
                let (operator, applications, repair_parent, repair_causal_parent) = match allocation
                    .action()
                {
                    OperationalAction::Generate {
                        operator,
                        applications,
                        ..
                    }
                    | OperationalAction::Explore {
                        operator,
                        applications,
                        ..
                    } => (operator, applications, None, None),
                    OperationalAction::Repair {
                        rejection,
                        parent,
                        operator,
                        applications,
                        ..
                    } => {
                        let rejected = ledger
                            .entries()
                            .iter()
                            .find(|entry| entry.attempt_id == rejection.identity())
                            .ok_or(SessionError::CorruptBundle)?;
                        let expected_operator = SubjectId::new(stable_digest(
                            "reflex-primitive-operator-action-v1",
                            &rejected.operator_symbol,
                        ));
                        if rejected.parent_key.0 != parent.identity()
                            || expected_operator != operator
                            || rejected.verdict != ExperienceVerdict::Refuted
                            || rejected.rejection_advisory.is_none()
                        {
                            return Err(SessionError::CorruptBundle);
                        }
                        let source_index = frontier
                            .iter()
                            .position(|(artifact, _)| artifact.key() == rejected.parent_key)
                            .ok_or(SessionError::CorruptBundle)?;
                        let mut structure_scratch =
                            <D::Structure as StructuralProtocol<D>>::Scratch::default();
                        let rejected_artifact = domain
                            .structure()
                            .decode_canonical(&rejected.canonical_candidate, &mut structure_scratch)
                            .map_err(|_| SessionError::CorruptBundle)?;
                        repair_seed = Some((
                            rejected_artifact,
                            source_index,
                            rejected
                                .rejection_advisory
                                .expect("a validated Repair has an advisory"),
                        ));
                        (
                            operator,
                            applications,
                            Some(rejected.parent_key),
                            Some(rejected.candidate_key),
                        )
                    }
                    _ => return Err(SessionError::CorruptBundle),
                };
                let operator_index = catalog
                    .iter()
                    .position(|descriptor| {
                        SubjectId::new(stable_digest(
                            "reflex-primitive-operator-action-v1",
                            descriptor.symbol().as_str().as_bytes(),
                        )) == operator
                    })
                    .ok_or(SessionError::CorruptBundle)?;
                let action_budget = remaining_discretionary_budget
                    .min(usize::try_from(applications.get()).unwrap_or(usize::MAX));
                let started = std::time::Instant::now();
                let cpu_before = resource_meter
                    .current_cpu()
                    .map_err(|()| SessionError::Resource)?;
                let mut action_resident = 0_u64;
                let mut remaining_action_budget = action_budget;
                let mut action_generated = 0_usize;
                if let Some((rejected_artifact, source_index, advisory)) = repair_seed {
                    let mut repair_offsets = vec![0_u64; catalog.len()];
                    let advisory_span = action_budget.max(1);
                    let advisory_limit = advisory_span.saturating_sub(
                        usize::try_from(
                            rejection_advisory_cursor(advisory)
                                % u64::try_from(advisory_span).unwrap_or(u64::MAX),
                        )
                        .unwrap_or(0),
                    );
                    let parent_artifacts = [&rejected_artifact];
                    let parent_indexes = [source_index];
                    let primitive_bytes = StructuredRewriteEngine
                        .propose(
                            domain,
                            &mut StructuredRewriteRequest {
                                parents: ProposalParents {
                                    artifacts: &parent_artifacts,
                                    frontier_indexes: &parent_indexes,
                                },
                                operator_offsets: &mut repair_offsets,
                                limit: remaining_action_budget.min(advisory_limit),
                                epoch: candidate_epoch,
                                permitted_operators: Some(&[operator_index]),
                            },
                            &mut operator_scratch,
                            &mut candidates,
                        )?
                        .resident_bytes();
                    action_generated = candidates.len().saturating_sub(action_candidate_start);
                    action_resident = action_resident.max(primitive_bytes);
                } else {
                    for (parent_position, ((parent, source_index), progress)) in generation_parents
                        .iter()
                        .zip(&generation_parent_indexes)
                        .zip(&mut staged_parent_progress)
                        .enumerate()
                    {
                        if remaining_action_budget == 0 {
                            break;
                        }
                        if !primitive_action_parent_eligible(
                            repair_parent,
                            frontier[*source_index].0.key(),
                        ) {
                            continue;
                        }
                        let parents_left = repair_parent
                            .map_or_else(|| generation_parents.len() - parent_position, |_| 1);
                        let parent_budget = remaining_action_budget.div_ceil(parents_left);
                        let before = candidates.len();
                        let parent_artifacts = [*parent];
                        let parent_indexes = [*source_index];
                        let primitive_bytes = StructuredRewriteEngine
                            .propose(
                                domain,
                                &mut StructuredRewriteRequest {
                                    parents: ProposalParents {
                                        artifacts: &parent_artifacts,
                                        frontier_indexes: &parent_indexes,
                                    },
                                    operator_offsets: &mut progress.primitive_offsets,
                                    limit: parent_budget,
                                    epoch: candidate_epoch,
                                    permitted_operators: Some(&[operator_index]),
                                },
                                &mut operator_scratch,
                                &mut candidates,
                            )?
                            .resident_bytes();
                        let generated = candidates.len().saturating_sub(before);
                        action_generated = action_generated.saturating_add(generated);
                        remaining_action_budget = remaining_action_budget.saturating_sub(generated);
                        action_resident = action_resident.max(primitive_bytes);
                    }
                }
                remaining_discretionary_budget =
                    remaining_discretionary_budget.saturating_sub(action_budget);
                let cpu = resource_meter
                    .current_cpu()
                    .map_err(|()| SessionError::Resource)?
                    .saturating_sub(cpu_before);
                let (receipt, settlement) = allocation
                    .settle(
                        &intelligence,
                        if action_generated == 0 {
                            InvestmentOutcome::Failed
                        } else {
                            InvestmentOutcome::Completed
                        },
                        ResourceVector::new(
                            u64::try_from(cpu.as_nanos()).unwrap_or(u64::MAX),
                            action_resident,
                            0,
                            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                            0,
                        ),
                    )
                    .map_err(map_intelligence_error)?;
                for candidate in &mut candidates[action_candidate_start..] {
                    candidate.action_decision = Some(receipt.decision());
                    candidate.causal_parent_key = repair_causal_parent;
                    if repair_causal_parent.is_some() {
                        let operator = std::str::from_utf8(&candidate.operator_symbol)
                            .expect("canonical Operator symbols are UTF-8");
                        candidate.features = opportunity_features(
                            domain,
                            parent_summaries[candidate.candidate.source_index],
                            &candidate.candidate.artifact,
                            operator_feature_values(operator),
                            candidate_epoch,
                            candidate.candidate.proposal_features,
                        );
                    }
                }
                operational_receipts.push(receipt);
                operational_settlements.push(settlement);
            }
        }
        if resource_meter
            .search_time_exhausted()
            .map_err(|()| SessionError::Resource)?
        {
            deferred_candidates = cohort::rollback_unverified_generation(candidates, Vec::new());
            instrumentation.refused(ResourceRefusal::Time);
            time_exhausted = true;
            break;
        }
        let mut remaining_derived_budget = derived_budget;
        let derived_positions = staged_parent_progress
            .iter()
            .enumerate()
            .filter_map(|(position, progress)| (!progress.derived_sampled).then_some(position))
            .collect::<Vec<_>>();
        for (derived_position, parent_position) in derived_positions.iter().copied().enumerate() {
            let parents_left = derived_positions.len() - derived_position;
            let parent_budget = remaining_derived_budget.div_ceil(parents_left);
            if parent_budget == 0 {
                break;
            }
            let before = candidates.len();
            let parent = generation_parents[parent_position];
            let source_index = generation_parent_indexes[parent_position];
            let parent_artifacts = [parent];
            let parent_indexes = [source_index];
            let derived_batch = DerivedOperatorEngine.propose(
                domain,
                &mut DerivedOperatorRequest {
                    parents: ProposalParents {
                        artifacts: &parent_artifacts,
                        frontier_indexes: &parent_indexes,
                    },
                    knowledge: &pinned_knowledge,
                    limit: parent_budget,
                    epoch: candidate_epoch,
                },
                &mut operator_scratch,
                &mut candidates,
            )?;
            let derived_bytes = derived_batch.resident_bytes();
            let _derived_truncated = derived_batch.truncated();
            remaining_derived_budget =
                remaining_derived_budget.saturating_sub(candidates.len() - before);
            application_bytes = application_bytes.max(derived_bytes);
            staged_parent_progress[parent_position].derived_sampled = true;
        }
        if !operational_receipts.is_empty() {
            // Candidate-producing actions become causal authority before any
            // generated Candidate is ranked, deferred, or exposed to
            // Verification. The checkpoint retains the complete generation
            // prefix and its exact primitive and Derived-Operator progress, so
            // recovery resumes at selection instead of rerunning an action.
            let (canonical_bytes, canonical_scratch) =
                candidate_canonical_materialization_bound(domain, &candidates)?;
            let published_progress_bytes =
                pending_parent_vector_resident_bytes(&pending_parents, pending_parents.capacity());
            let action_materialization_live = worker_resident_bytes
                .saturating_add(resident_state_bytes(
                    &known,
                    &roots,
                    &pareto,
                    &frontier,
                    &recovered_keys,
                    &operators,
                    &checkpoint,
                    &ledger,
                    &intelligence,
                    intelligence_working_bytes,
                ))
                .saturating_add(cohort::recovery_resident_bytes(
                    domain,
                    &candidates,
                    &pending_parents,
                ))
                .saturating_add(canonical_bytes);
            if !resource_meter.reserve(
                ResidentReservation::live(action_materialization_live)
                    .with_transient(canonical_scratch.saturating_add(published_progress_bytes))
                    .with_pending_durability(durability.pending_bytes()),
            ) {
                return Err(SessionError::Resource);
            }
            materialize_candidate_canonical(domain, &mut candidates)?;
            let mut published_pending_parents = pending_parents.clone();
            commit_pending_progress(
                &mut published_pending_parents,
                staged_parent_progress.clone(),
            );
            let action_transition = intelligence
                .stage(SettlementFrame::observations(
                    &operational_receipts,
                    &operational_settlements,
                    &[],
                    &[],
                ))
                .map_err(map_intelligence_error)?;
            test_fault_point("operational-actions-staged");
            let action_live = worker_resident_bytes
                .saturating_add(resident_state_bytes(
                    &known,
                    &roots,
                    &pareto,
                    &frontier,
                    &recovered_keys,
                    &operators,
                    &checkpoint,
                    &ledger,
                    &intelligence,
                    intelligence_working_bytes,
                ))
                .saturating_add(cohort::recovery_resident_bytes(
                    domain,
                    &candidates,
                    &pending_parents,
                ))
                // Publication encodes the progressed clone while the original
                // pending-parent vector remains live until the durability
                // barrier commits. Charge both complete nested capacities.
                .saturating_add(published_progress_bytes);
            let published = publish_intelligence_transition(
                action_transition,
                &mut intelligence,
                &seed_cursor,
                &IntelligencePublicationState {
                    artifacts: &known,
                    pareto: &pareto,
                    frontier: &frontier,
                    deferred_candidates: &candidates,
                    pending_parents: &published_pending_parents,
                    generation_complete: true,
                    ledger: &ledger,
                },
                &checkpoint,
                verification_requests,
                action_live,
                &bundle_codec,
                resource_meter,
                &mut durability,
            );
            let next_checkpoint = match published {
                Ok(checkpoint) => checkpoint,
                Err(SessionError::Resource) => {
                    deferred_candidates =
                        cohort::rollback_unverified_generation(candidates, deferred_candidates);
                    operational_receipts.clear();
                    operational_settlements.clear();
                    instrumentation.refused(ResourceRefusal::DurablePreVerification);
                    durable_budget_exhausted = true;
                    break;
                }
                Err(error) => return Err(error),
            };
            pending_parents = published_pending_parents;
            checkpoint = next_checkpoint;
            for candidate in &mut candidates {
                candidate.published_in_epoch = true;
            }
            operational_receipts.clear();
            operational_settlements.clear();
            test_fault_point("operational-actions-published");
        }
        instrumentation.generated(candidates.len().saturating_sub(carried_candidate_count));
        instrumentation.finish(Phase::Generation, generation_started);
        test_fault_point("candidate-created");
        if resource_meter
            .search_time_exhausted()
            .map_err(|()| SessionError::Resource)?
        {
            deferred_candidates = cohort::rollback_unverified_generation(candidates, Vec::new());
            instrumentation.refused(ResourceRefusal::Time);
            time_exhausted = true;
            break;
        }
        let selection_started = instrumentation.start();
        let novel = retain_novel_candidates(
            domain,
            &known,
            ledger.entries(),
            &roots,
            &frontier,
            candidates,
        )?;
        candidates = novel.candidates;
        let mut candidate_fates = novel.fates;
        let fate_is_new_generation = novel.fate_is_new_generation;
        let intelligence_allowance = remaining_resource_allowance(
            request,
            resource_meter,
            verification_requests,
            resident_before_epoch,
            checkpoint.len() as u64,
        )?;
        deferred_candidates = order_by_learned_potential(
            domain,
            parent_ranks.as_slice(),
            &frontier,
            &covered_claims,
            &intelligence,
            &mut intelligence_market,
            &mut intelligence_portfolio,
            intelligence_allowance,
            inventory_cohort_limit,
            &mut candidates,
            &mut candidate_fates,
        );
        let mut policy_trial = None;
        if policy_trial_due {
            let trial_live = resident_before_epoch
                .saturating_add(vector_bytes(&candidates))
                .saturating_add(candidate_pipeline_reserve(domain, &candidates));
            if let Some(resources) = protected_policy_trial_resources(
                request,
                resource_meter,
                verification_requests,
                trial_live,
                checkpoint.len() as u64,
                runtime_policy,
            )? {
                let paired_arm_bytes = vector_bytes(&candidates)
                    .saturating_add(candidate_pipeline_reserve(domain, &candidates))
                    .saturating_add(vector_bytes(&candidate_fates))
                    .saturating_mul(2);
                let trial_transient = (checkpoint.capacity() as u64)
                    .saturating_add(paired_arm_bytes)
                    .saturating_add(vector_bytes(&candidates));
                if resource_meter.reserve(
                    ResidentReservation::live(trial_live)
                        .with_transient(trial_transient)
                        .with_pending_durability(durability.pending_bytes()),
                ) && let Some(challenger) = intelligence.runtime_policy_challenger(
                    policy_trial_sequence.expect("a due policy trial has a sequence"),
                ) {
                    let sequence =
                        policy_trial_sequence.expect("a due policy trial has a sequence");
                    let current_parent = SubjectId::new(stable_digest(
                        "reflex-runtime-policy-proposal-v1",
                        &sequence.to_le_bytes(),
                    ));
                    let alternate_sequence = sequence.saturating_add(1);
                    let alternate_parent = SubjectId::new(stable_digest(
                        "reflex-runtime-policy-proposal-v1",
                        &alternate_sequence.to_le_bytes(),
                    ));
                    let shadow_subject = SubjectId::new(challenger.identity());
                    let proposal_resources = ResourceVector::new(
                        intelligence_allowance.cpu_time_ns / 16,
                        0,
                        0,
                        intelligence_allowance.elapsed_time_ns / 16,
                        0,
                    );
                    operational_specs.clear();
                    operational_specs.extend_from_slice(&[
                        OperationalActionSpec::new(
                            OperationalAction::ProposeRuntimePolicy {
                                parent: current_parent,
                            },
                            proposal_resources,
                            0,
                            [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0],
                        ),
                        OperationalActionSpec::new(
                            OperationalAction::RunShadowCampaign {
                                specification: shadow_subject,
                            },
                            resources.protected_total(),
                            1,
                            [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0],
                        ),
                        OperationalActionSpec::new(
                            OperationalAction::ProposeRuntimePolicy {
                                parent: alternate_parent,
                            },
                            proposal_resources,
                            2,
                            [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0],
                        ),
                    ]);
                    let prepared = PreparedOperationalMarket::prepare(
                        &operational_specs,
                        &mut intelligence_market,
                    )
                    .map_err(map_intelligence_error)?;
                    let actions = prepared
                        .allocate(
                            &intelligence,
                            &intelligence_market,
                            intelligence_allowance,
                            selection_epoch,
                            2,
                            &mut intelligence_portfolio,
                        )
                        .map_err(map_intelligence_error)?;
                    let mut selected_challenger = None;
                    let mut shadow_selected = false;
                    for action in actions.into_iter() {
                        match action.action() {
                            OperationalAction::ProposeRuntimePolicy { parent }
                                if parent == current_parent =>
                            {
                                let materialized = intelligence
                                    .runtime_policy_challenger(sequence)
                                    .ok_or(SessionError::CorruptBundle)?;
                                if materialized.identity() != shadow_subject.identity() {
                                    return Err(SessionError::CorruptBundle);
                                }
                                selected_challenger = Some(materialized);
                                let (receipt, settlement) = action
                                    .settle(
                                        &intelligence,
                                        InvestmentOutcome::Completed,
                                        ResourceVector::new(0, 0, 0, 0, 0),
                                    )
                                    .map_err(map_intelligence_error)?;
                                operational_receipts.push(receipt);
                                operational_settlements.push(settlement);
                            }
                            OperationalAction::ProposeRuntimePolicy { parent }
                                if parent == alternate_parent =>
                            {
                                intelligence
                                    .runtime_policy_challenger(alternate_sequence)
                                    .ok_or(SessionError::CorruptBundle)?;
                                let (receipt, settlement) = action
                                    .settle(
                                        &intelligence,
                                        InvestmentOutcome::Completed,
                                        ResourceVector::new(0, 0, 0, 0, 0),
                                    )
                                    .map_err(map_intelligence_error)?;
                                operational_receipts.push(receipt);
                                operational_settlements.push(settlement);
                            }
                            OperationalAction::RunShadowCampaign { specification }
                                if specification == shadow_subject =>
                            {
                                let (receipt, settlement) = action
                                    .authorize_shadow(
                                        &intelligence,
                                        shadow_subject,
                                        resources.protected_total(),
                                    )
                                    .map_err(map_intelligence_error)?;
                                operational_receipts.push(receipt);
                                operational_settlements.push(settlement);
                                shadow_selected = true;
                            }
                            _ => return Err(SessionError::CorruptBundle),
                        }
                    }
                    if let Some(challenger) = selected_challenger.filter(|_| shadow_selected) {
                        let proposals = candidates
                            .drain(..)
                            .map(cohort::CohortProposal::capture)
                            .collect::<Vec<_>>();
                        let per_arm_limit =
                            usize::try_from(resources.per_arm().verification_requests)
                                .unwrap_or(usize::MAX);
                        let incumbent_plan = cohort::PolicySelectionPlan::for_policy(
                            runtime_policy,
                            proposals.len(),
                            remaining.min(per_arm_limit),
                            verification_workers.worker_lanes(),
                            &parent_claims,
                            &covered_claims,
                        );
                        let challenger_plan = cohort::PolicySelectionPlan::for_policy(
                            challenger,
                            proposals.len(),
                            remaining.min(per_arm_limit),
                            verification_workers.worker_lanes(),
                            &parent_claims,
                            &covered_claims,
                        );
                        let checkpoint_digest = runtime_policy_trial_checkpoint(
                            &checkpoint,
                            &proposals,
                            &candidate_fates,
                            &fate_is_new_generation,
                            &incumbent_plan,
                            &challenger_plan,
                        );
                        let scheduled_trial = shadow::ScheduledRuntimePolicyTrial::schedule(
                            runtime_policy,
                            challenger,
                            Arc::new(checkpoint.clone()),
                            resources,
                            shadow::ShadowCampaignControls::new(
                                CheckpointDigest::new(checkpoint_digest),
                                SubjectId::new(stable_digest(
                                    domain.semantic_identity().as_str(),
                                    &candidate_epoch.to_le_bytes(),
                                )),
                                shadow::SchedulingToken::new(checkpoint_digest),
                                [
                                    ForecastAxis::KernelAcceptance,
                                    ForecastAxis::ImmediateImprovement,
                                    ForecastAxis::UsefulDescendants,
                                    ForecastAxis::CrossGoalLeverage,
                                    ForecastAxis::VerificationCost,
                                ],
                            ),
                        )
                        .map_err(map_intelligence_error)?
                        .ok_or(SessionError::CorruptBundle)?;
                        let mut structure_scratch =
                            <D::Structure as StructuralProtocol<D>>::Scratch::default();
                        let trial_recovery_deferred = proposals
                            .iter()
                            .filter(|proposal| !proposal.generated_in_epoch())
                            .map(|proposal| {
                                proposal
                                    .replay(domain, &mut structure_scratch)
                                    .map_err(SessionError::Domain)
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        candidates = incumbent_plan
                            .selected()
                            .map(|index| {
                                proposals[index]
                                    .replay(domain, &mut structure_scratch)
                                    .map_err(SessionError::Domain)
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let incumbent_deferred = incumbent_plan
                            .deferred()
                            .map(|index| {
                                proposals[index]
                                    .replay(domain, &mut structure_scratch)
                                    .map_err(SessionError::Domain)
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let incumbent_deferred_count = incumbent_deferred.len();
                        deferred_candidates.splice(0..0, incumbent_deferred);
                        policy_trial = Some((
                            scheduled_trial,
                            proposals,
                            candidate_fates.clone(),
                            fate_is_new_generation.clone(),
                            incumbent_plan,
                            challenger_plan,
                            incumbent_deferred_count,
                            trial_recovery_deferred,
                        ));
                    }
                }
            }
        }
        drop(covered_claims);
        for (policy_rank, candidate) in candidates.iter().enumerate() {
            let fate = candidate_fates
                .get_mut(candidate.fate_index)
                .expect("selected Candidates retain their Candidate Fate index");
            fate.disposition = CandidateFateDisposition::VerificationInterrupted;
            fate.allocation_queue = Some(candidate.allocation_queue);
            fate.policy_rank = CandidateRank::present(
                u32::try_from(policy_rank).expect("Candidate policy rank is bounded"),
            );
            fate.bootstrap_rank = candidate.bootstrap_rank;
            fate.learned_rank = candidate.learned_rank;
        }
        candidate_fates =
            retain_transaction_fates(&mut candidates, candidate_fates, fate_is_new_generation);
        instrumentation.selected(candidates.len());
        instrumentation.finish(Phase::Selection, selection_started);
        if candidates.is_empty() {
            let prospective_experience = ledger
                .encoded_len()
                .saturating_add(encoded_candidate_fates_len(&candidate_fates));
            let prospective_recovery = recovery_encoded_len(
                &pareto,
                &SearchTailView::new(&frontier, &deferred_candidates, &pending_parents),
            );
            let prospective_durable = CanonicalBundle::replacement_size_bound(
                &checkpoint,
                &[
                    (SegmentKind::Experience, prospective_experience),
                    (SegmentKind::Recovery, prospective_recovery),
                ],
            )
            .map_err(|_| SessionError::CorruptBundle)?;
            if !resource_meter.checkpoint_fits(prospective_durable) {
                deferred_candidates =
                    cohort::rollback_unverified_generation(Vec::new(), deferred_candidates);
                instrumentation.refused(ResourceRefusal::DurablePreVerification);
                durable_budget_exhausted = true;
                break;
            }
            commit_pending_progress(&mut pending_parents, staged_parent_progress);
            ledger.append_candidate_fates(candidate_fates);
            if !pending_parents.is_empty() {
                continue;
            }
            break;
        }
        let deferred_resident =
            cohort::recovery_resident_bytes(domain, &deferred_candidates, &pending_parents);
        let epoch_context_bytes = vector_bytes(&parents)
            .saturating_add(vector_bytes(&parent_summaries))
            .saturating_add(vector_bytes(&parent_claims))
            .saturating_add(vector_bytes(&origins))
            .saturating_add(
                (frontier_index_by_key.capacity() as u64)
                    .saturating_mul(std::mem::size_of::<(ArtifactKey, usize)>() as u64)
                    .saturating_mul(2),
            );
        let transaction_bytes = application_bytes
            .saturating_add(epoch_context_bytes)
            .saturating_add(allocation_bytes::<ArtifactKey, _>(frontier.len())?)
            .saturating_add(vector_bytes(&candidates))
            .saturating_add(candidate_pipeline_reserve(domain, &candidates))
            .saturating_add(vector_bytes(&candidate_fates))
            .saturating_add(intelligence_transition_reserve(
                &intelligence,
                candidates.len().saturating_add(action_limit),
                native_training_budget(runtime_policy),
            ));
        let prospective_experience =
            prospective_experience_encoded_len(&ledger, &candidates, &candidate_fates);
        let prospective_recovery = recovery_encoded_len(
            &pareto,
            &SearchTailView::new(&frontier, &deferred_candidates, &pending_parents),
        );
        let prospective_recovery = recovery_after_admission_bound(
            prospective_recovery,
            candidates.len(),
            primitive_operator_count,
        );
        let prospective_durable = CanonicalBundle::replacement_size_bound(
            &checkpoint,
            &[
                (SegmentKind::Experience, prospective_experience),
                (SegmentKind::Recovery, prospective_recovery),
            ],
        )
        .map_err(|_| SessionError::CorruptBundle)?;
        if !resource_meter.checkpoint_fits(prospective_durable) {
            deferred_candidates =
                cohort::rollback_unverified_generation(candidates, deferred_candidates);
            instrumentation.refused(ResourceRefusal::DurablePreVerification);
            durable_budget_exhausted = true;
            break;
        }
        let stable_verification_live = worker_resident_bytes.saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &checkpoint,
            &ledger,
            &intelligence,
            intelligence_working_bytes,
        ));
        let verification_resident = moved_tail_transaction_peak(
            stable_verification_live,
            deferred_resident,
            transaction_bytes,
        );
        if !resource_meter.reserve(
            ResidentReservation::live(verification_resident)
                .with_transient(checkpoint.capacity() as u64)
                .with_pending_durability(durability.pending_bytes()),
        ) {
            deferred_candidates =
                cohort::rollback_unverified_generation(candidates, deferred_candidates);
            instrumentation.refused(ResourceRefusal::ResidentPreVerification);
            resident_budget_exhausted = true;
            break;
        }
        if resource_meter
            .search_time_exhausted()
            .map_err(|()| SessionError::Resource)?
        {
            deferred_candidates =
                cohort::rollback_unverified_generation(candidates, deferred_candidates);
            instrumentation.refused(ResourceRefusal::Time);
            time_exhausted = true;
            break;
        }
        if candidates.len() > remaining {
            candidates.truncate(remaining);
            instrumentation.refused(ResourceRefusal::VerificationBudget);
            verification_budget_exhausted = true;
        }
        if candidates.is_empty() {
            ledger.append_candidate_fates(candidate_fates);
            break;
        }
        let mut parent_keys = exact_vec(frontier.len())?;
        parent_keys.extend(frontier.iter().map(|(artifact, _)| artifact.key()));
        let verification_started = instrumentation.start();
        let verification_result = if let Some((
            scheduled,
            proposals,
            trial_fates,
            trial_fate_is_new,
            incumbent_plan,
            challenger_plan,
            incumbent_deferred_count,
            trial_recovery_deferred,
        )) = policy_trial.take()
        {
            let mut execution_error = None;
            let trial_verification_requests = Cell::new(scheduled.reserved_verification_requests());
            let mut policy_actions_open = true;
            let trial_result = shadow::run_scheduled_runtime_policy_trial(
                intelligence.runtime_policy_revision(),
                scheduled,
                |restart, policy, context| {
                    let plan = if policy == runtime_policy {
                        &incumbent_plan
                    } else {
                        &challenger_plan
                    };
                    if !resource_meter.reserve(
                        ResidentReservation::live(verification_resident)
                            .with_transient(context.resources().resident_bytes),
                    ) {
                        execution_error = Some(SessionError::Resource);
                        return shadow::ArmExecution::Invalidated(
                            crate::intelligence::ShadowInvalidationReason::ResourceMismatch,
                        );
                    }
                    let started = std::time::Instant::now();
                    let Ok(cpu_before) = resource_meter.current_cpu() else {
                        execution_error = Some(SessionError::Resource);
                        return shadow::ArmExecution::Invalidated(
                            crate::intelligence::ShadowInvalidationReason::Interrupted,
                        );
                    };
                    let prepared = replay_policy_plan(
                        domain,
                        &proposals,
                        plan,
                        trial_fates.clone(),
                        trial_fate_is_new.clone(),
                    );
                    let (arm_candidates, mut arm_fates) = match prepared {
                        Ok(prepared) => prepared,
                        Err(error) => {
                            execution_error = Some(error);
                            return shadow::ArmExecution::Invalidated(
                                crate::intelligence::ShadowInvalidationReason::Interrupted,
                            );
                        }
                    };
                    let arm_resources = context.resources();
                    let arm_live = PolicyShadowArmResidentView {
                        restart: restart.as_ref(),
                        proposals: &proposals,
                        retained_fates: &trial_fates,
                        retained_fate_is_new: &trial_fate_is_new,
                        plans: [&incumbent_plan, &challenger_plan],
                        trial_recovery_deferred: &trial_recovery_deferred,
                        candidates: &arm_candidates,
                        candidate_fates: &arm_fates,
                    }
                    .resident_bytes(domain);
                    if arm_live > arm_resources.resident_bytes
                        || !resource_meter.reserve(ResidentReservation::live(
                            verification_resident.saturating_add(arm_live),
                        ))
                    {
                        execution_error = Some(SessionError::Resource);
                        return shadow::ArmExecution::Invalidated(
                            crate::intelligence::ShadowInvalidationReason::ResourceMismatch,
                        );
                    }
                    let controller_cpu_ns = resource_meter.current_cpu().map_or(u64::MAX, |cpu| {
                        u64::try_from(cpu.saturating_sub(cpu_before).as_nanos()).unwrap_or(u64::MAX)
                    });
                    let controller_elapsed_ns =
                        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    if controller_cpu_ns >= arm_resources.cpu_time_ns
                        || controller_elapsed_ns >= arm_resources.elapsed_time_ns
                    {
                        return shadow::ArmExecution::Invalidated(
                            crate::intelligence::ShadowInvalidationReason::ResourceMismatch,
                        );
                    }
                    let allowance = VerificationAllowance::new(
                        verification_workers.worker_lanes().max(1),
                        arm_resources.resident_bytes.saturating_sub(arm_live),
                        std::time::Duration::from_nanos(
                            arm_resources.elapsed_time_ns - controller_elapsed_ns,
                        ),
                        std::time::Duration::from_nanos(
                            arm_resources.cpu_time_ns - controller_cpu_ns,
                        ),
                    );
                    let request_count = arm_candidates.len() as u64;
                    let verification = verify_candidates(
                        domain,
                        &roots,
                        &origins,
                        &parent_keys,
                        arm_candidates,
                        scheduler,
                        resource_meter,
                        verification_workers,
                        verification_resident,
                        &mut instrumentation,
                        &mut arm_fates,
                        Some(allowance),
                    );
                    let verification = match verification {
                        Ok(verification) => verification,
                        Err(error) => {
                            execution_error = Some(error);
                            return shadow::ArmExecution::Invalidated(
                                crate::intelligence::ShadowInvalidationReason::Interrupted,
                            );
                        }
                    };
                    let cpu_time_ns = resource_meter
                        .current_cpu()
                        .map_or(u64::MAX, |cpu| {
                            u64::try_from(cpu.saturating_sub(cpu_before).as_nanos())
                                .unwrap_or(u64::MAX)
                        })
                        .saturating_add(verification.external_worker_cpu_ns);
                    let elapsed_time_ns = shadow_elapsed_ns(
                        started.elapsed(),
                        verification.kernel_usage.elapsed_time_ns,
                    );
                    let (outcomes, durable_bytes) =
                        policy_trial_evidence(&verification, request_count);
                    let arm_peak_resident =
                        arm_live.saturating_add(verification.kernel_usage.resident_bytes);
                    if context.arm() == crate::intelligence::ShadowArm::Treatment {
                        test_fault_point("runtime-policy-shadow-treatment-completed");
                    }
                    shadow::ArmExecution::Completed(shadow::RuntimePolicyArmTransaction::new(
                        PolicyCohortTransaction {
                            verification,
                            candidate_fates: arm_fates,
                            plan: plan.clone(),
                        },
                        ResourceVector::new(
                            cpu_time_ns,
                            arm_peak_resident,
                            durable_bytes,
                            elapsed_time_ns,
                            request_count,
                        ),
                        outcomes,
                    ))
                },
                |updates, policy_update| {
                    (|| {
                        let (receipts, settlements) = if policy_actions_open {
                            (&operational_receipts[..], &operational_settlements[..])
                        } else {
                            (&[][..], &[][..])
                        };
                        let mut frame =
                            SettlementFrame::observations(receipts, settlements, &[], updates);
                        if let Some(update) = policy_update {
                            frame = frame.with_policy_update(update);
                        }
                        let transition =
                            intelligence.stage(frame).map_err(map_intelligence_error)?;
                        let decision = transition.policy_decision();
                        let causal_verification_requests = verification_requests
                            .checked_add(trial_verification_requests.get())
                            .ok_or(SessionError::Resource)?;
                        let causal_live = worker_resident_bytes
                            .saturating_add(resident_state_bytes(
                                &known,
                                &roots,
                                &pareto,
                                &frontier,
                                &recovered_keys,
                                &operators,
                                &checkpoint,
                                &ledger,
                                &intelligence,
                                intelligence_working_bytes,
                            ))
                            .saturating_add(cohort::recovery_resident_bytes(
                                domain,
                                &trial_recovery_deferred,
                                &pending_parents,
                            ));
                        let next_checkpoint = publish_intelligence_transition(
                            transition,
                            &mut intelligence,
                            &seed_cursor,
                            &IntelligencePublicationState {
                                artifacts: &known,
                                pareto: &pareto,
                                frontier: &frontier,
                                deferred_candidates: &trial_recovery_deferred,
                                pending_parents: &pending_parents,
                                generation_complete: false,
                                ledger: &ledger,
                            },
                            &checkpoint,
                            causal_verification_requests,
                            causal_live,
                            &bundle_codec,
                            resource_meter,
                            &mut durability,
                        )?;
                        checkpoint = next_checkpoint;
                        if policy_actions_open {
                            operational_receipts.clear();
                            operational_settlements.clear();
                            policy_actions_open = false;
                        }
                        test_fault_point("runtime-policy-shadow-update-published");
                        Ok::<_, SessionError<D::Error>>(decision)
                    })()
                },
            );
            instrumentation
                .verified(usize::try_from(trial_verification_requests.get()).unwrap_or(usize::MAX));
            verification_requests = verification_requests
                .checked_add(trial_verification_requests.get())
                .ok_or(SessionError::Resource)?;
            if let Some(error) = execution_error {
                Err(error)
            } else {
                let trial_result = trial_result.map_err(|error| match error {
                    shadow::RuntimePolicyTrialError::Causal(error) => error,
                    shadow::RuntimePolicyTrialError::InvalidOutcome(error) => {
                        map_intelligence_error(error)
                    }
                })?;
                let transaction = match trial_result {
                    shadow::RuntimePolicyTrialResult::Applied { transaction, .. } => transaction,
                    shadow::RuntimePolicyTrialResult::Invalidated(_) => {
                        drop(candidates);
                        deferred_candidates = trial_recovery_deferred;
                        policy_trial_invalidated = true;
                        break;
                    }
                };
                runtime_policy = intelligence.runtime_policy_revision();
                drop(candidates);
                let selected_deferred_count = transaction.plan.deferred().len();
                if selected_deferred_count < incumbent_deferred_count {
                    deferred_candidates
                        .drain(0..incumbent_deferred_count - selected_deferred_count);
                }
                candidate_fates = transaction.candidate_fates;
                Ok(transaction.verification)
            }
        } else {
            instrumentation.verified(candidates.len());
            verification_requests +=
                u64::try_from(candidates.len()).map_err(|_| SessionError::Resource)?;
            verify_candidates(
                domain,
                &roots,
                &origins,
                &parent_keys,
                candidates,
                scheduler,
                resource_meter,
                verification_workers,
                verification_resident,
                &mut instrumentation,
                &mut candidate_fates,
                None,
            )
        };
        let verification = match verification_result {
            Ok(verification) => verification,
            Err(error) => {
                if verification_workers.worker_lanes() != 0 {
                    let usage = resource_meter
                        .usage(verification_requests, checkpoint.len() as u64)
                        .map_err(|()| SessionError::Resource)?;
                    persist_active_interruption(
                        &bundle_codec,
                        &seed_cursor,
                        &checkpoint,
                        usage,
                        resource_meter,
                        verification_resident,
                        &mut durability,
                    )?;
                }
                return Err(error);
            }
        };
        instrumentation.finish(Phase::Verification, verification_started);
        test_fault_point("verdict-recorded");
        let intelligence_training_due = crossed_cadence(
            intelligence.experience().settlements().len(),
            operational_settlements
                .len()
                .saturating_add(verification.intelligence_settlements.len()),
            4,
        );
        operational_receipts.extend_from_slice(&verification.intelligence_receipts);
        operational_settlements.extend_from_slice(&verification.intelligence_settlements);
        let mut run_training_market = |training_consequences: &[ConsequenceEdge]| -> Result<
            Option<PreparedNativeEcologyPlan>,
            SessionError<D::Error>,
        > {
            let selected_training = if intelligence_training_due {
                let standard_budget = native_training_budget(runtime_policy);
                let exploratory_budget =
                    NativeTrainingBudget::new(standard_budget.maximum_examples(), 8, 0.125)
                        .expect("the bounded exploratory training budget is valid");
                let training_resident = intelligence
                    .native_training_scratch_bytes(standard_budget, operational_receipts.len())
                    .saturating_add(NativeTrainingBudget::maximum_output_bytes());
                let allowance = remaining_resource_allowance(
                    request,
                    resource_meter,
                    verification_requests,
                    resident_before_epoch,
                    checkpoint.len() as u64,
                )?;
                let training_market_fits = allowance.resident_bytes >= training_resident
                    && allowance.cpu_time_ns >= 4
                    && allowance.elapsed_time_ns >= 4;
                if training_market_fits {
                    operational_specs.clear();
                    let standard_mandate = SubjectId::new(stable_digest(
                        "reflex-native-training-mandate-v1",
                        b"standard",
                    ));
                    let exploratory_mandate = SubjectId::new(stable_digest(
                        "reflex-native-training-mandate-v1",
                        b"exploratory",
                    ));
                    let training_resources = ResourceVector::new(
                        allowance.cpu_time_ns / 4,
                        training_resident,
                        0,
                        allowance.elapsed_time_ns / 4,
                        0,
                    );
                    operational_specs.extend_from_slice(&[
                        OperationalActionSpec::new(
                            OperationalAction::TrainSpecialist {
                                mandate: standard_mandate,
                            },
                            training_resources,
                            0,
                            [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                        ),
                        OperationalActionSpec::new(
                            OperationalAction::TrainSpecialist {
                                mandate: exploratory_mandate,
                            },
                            training_resources,
                            1,
                            [1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                        ),
                    ]);
                    let prepared = PreparedOperationalMarket::prepare(
                        &operational_specs,
                        &mut intelligence_market,
                    )
                    .map_err(map_intelligence_error)?;
                    let allocations = prepared
                        .allocate(
                            &intelligence,
                            &intelligence_market,
                            allowance,
                            selection_epoch,
                            2,
                            &mut intelligence_portfolio,
                        )
                        .map_err(map_intelligence_error)?;
                    let simultaneous_training_resident = training_resident
                        .saturating_mul(u64::try_from(allocations.len()).unwrap_or(u64::MAX));
                    if allocations.is_empty()
                        || !resource_meter.reserve(
                            ResidentReservation::live(resident_before_epoch)
                                .with_transient(simultaneous_training_resident),
                        )
                    {
                        return Ok(None);
                    }
                    let mut standard_plan = None;
                    let mut exploratory_plan = None;
                    let training_receipt_prefix = operational_receipts.len();
                    let training_settlement_prefix = operational_settlements.len();
                    for allocation in allocations.into_iter() {
                        let started = std::time::Instant::now();
                        let cpu_before = resource_meter
                            .current_cpu()
                            .map_err(|()| SessionError::Resource)?;
                        let actual_resident = match allocation.action() {
                            OperationalAction::TrainSpecialist { mandate } => {
                                let budget = if mandate == standard_mandate {
                                    standard_budget
                                } else if mandate == exploratory_mandate {
                                    exploratory_budget
                                } else {
                                    return Err(SessionError::CorruptBundle);
                                };
                                let plan = intelligence
                                    .prepare_native_training(
                                        SettlementFrame::observations(
                                            &operational_receipts[..training_receipt_prefix],
                                            &operational_settlements[..training_settlement_prefix],
                                            training_consequences,
                                            &[],
                                        ),
                                        budget,
                                    )
                                    .map_err(map_intelligence_error)?;
                                let prepared_resident = plan.as_ref().map_or(0, |prepared| {
                                    prepared
                                        .resident_bytes()
                                        .saturating_add(prepared.scratch_bytes())
                                });
                                if mandate == standard_mandate {
                                    standard_plan = plan;
                                } else {
                                    exploratory_plan = plan;
                                }
                                prepared_resident
                            }
                            _ => return Err(SessionError::CorruptBundle),
                        };
                        let cpu = resource_meter
                            .current_cpu()
                            .map_err(|()| SessionError::Resource)?
                            .saturating_sub(cpu_before);
                        let (receipt, settlement) = allocation
                            .settle(
                                &intelligence,
                                InvestmentOutcome::Completed,
                                ResourceVector::new(
                                    u64::try_from(cpu.as_nanos()).unwrap_or(u64::MAX),
                                    actual_resident,
                                    0,
                                    u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                                    0,
                                ),
                            )
                            .map_err(map_intelligence_error)?;
                        operational_receipts.push(receipt);
                        operational_settlements.push(settlement);
                    }
                    operational_specs.clear();
                    let comparison_allowance = remaining_resource_allowance(
                        request,
                        resource_meter,
                        verification_requests,
                        resident_before_epoch,
                        checkpoint.len() as u64,
                    )?;
                    let comparison_resident_bound = standard_plan
                        .as_ref()
                        .into_iter()
                        .chain(exploratory_plan.as_ref())
                        .map(PreparedNativeEcologyPlan::comparison_resident_bytes)
                        .max()
                        .unwrap_or(0);
                    let comparison_resources = ResourceVector::new(
                        comparison_allowance.cpu_time_ns,
                        comparison_resident_bound,
                        0,
                        comparison_allowance.elapsed_time_ns,
                        0,
                    );
                    if let Some(plan) = &standard_plan {
                        operational_specs.push(OperationalActionSpec::new(
                            OperationalAction::CompareRevision {
                                challenger: SubjectId::new(plan.identity()),
                            },
                            comparison_resources,
                            1,
                            [0.0, 1.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0],
                        ));
                    }
                    if let Some(plan) = &exploratory_plan {
                        operational_specs.push(OperationalActionSpec::new(
                            OperationalAction::CompareRevision {
                                challenger: SubjectId::new(plan.identity()),
                            },
                            comparison_resources,
                            2,
                            [0.0, 1.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0],
                        ));
                    }
                    if operational_specs.is_empty()
                        || comparison_allowance.cpu_time_ns == 0
                        || comparison_allowance.elapsed_time_ns == 0
                        || comparison_allowance.resident_bytes < comparison_resident_bound
                    {
                        return Ok(None);
                    }
                    let prepared = PreparedOperationalMarket::prepare(
                        &operational_specs,
                        &mut intelligence_market,
                    )
                    .map_err(map_intelligence_error)?;
                    let mut comparisons = prepared
                        .allocate(
                            &intelligence,
                            &intelligence_market,
                            comparison_allowance,
                            selection_epoch,
                            1,
                            &mut intelligence_portfolio,
                        )
                        .map_err(map_intelligence_error)?
                        .into_iter();
                    let Some(comparison) = comparisons.next() else {
                        return Ok(None);
                    };
                    if comparisons.next().is_some() {
                        return Err(SessionError::CorruptBundle);
                    }
                    let OperationalAction::CompareRevision { challenger } = comparison.action()
                    else {
                        return Err(SessionError::CorruptBundle);
                    };
                    // Selection is authoritative before reproduction. Move the
                    // selected plan out and drop every unselected output so the
                    // reproduction peak contains exactly one retained plan,
                    // one complete scratch arena, and one possible output.
                    let selected_plan = if standard_plan
                        .as_ref()
                        .is_some_and(|plan| plan.identity() == challenger.identity())
                    {
                        let selected = standard_plan.take();
                        drop(exploratory_plan.take());
                        selected
                    } else if exploratory_plan
                        .as_ref()
                        .is_some_and(|plan| plan.identity() == challenger.identity())
                    {
                        let selected = exploratory_plan.take();
                        drop(standard_plan.take());
                        selected
                    } else {
                        return Err(SessionError::CorruptBundle);
                    }
                    .ok_or(SessionError::CorruptBundle)?;
                    let comparison_resident = selected_plan.comparison_resident_bytes();
                    if !resource_meter.reserve(
                        ResidentReservation::live(resident_before_epoch)
                            .with_transient(comparison_resident),
                    ) {
                        return Ok(None);
                    }
                    let started = std::time::Instant::now();
                    let cpu_before = resource_meter
                        .current_cpu()
                        .map_err(|()| SessionError::Resource)?;
                    let reproduced = intelligence
                        .reproduce_prepared_native_training(
                            SettlementFrame::observations(
                                &operational_receipts[..training_receipt_prefix],
                                &operational_settlements[..training_settlement_prefix],
                                training_consequences,
                                &[],
                            ),
                            &selected_plan,
                        )
                        .map_err(map_intelligence_error)?;
                    let cpu = resource_meter
                        .current_cpu()
                        .map_err(|()| SessionError::Resource)?
                        .saturating_sub(cpu_before);
                    let (receipt, settlement) = comparison
                        .settle(
                            &intelligence,
                            if reproduced {
                                InvestmentOutcome::Completed
                            } else {
                                InvestmentOutcome::Failed
                            },
                            ResourceVector::new(
                                u64::try_from(cpu.as_nanos()).unwrap_or(u64::MAX),
                                comparison_resident,
                                0,
                                u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                                0,
                            ),
                        )
                        .map_err(map_intelligence_error)?;
                    operational_receipts.push(receipt);
                    operational_settlements.push(settlement);
                    reproduced.then_some(selected_plan)
                } else {
                    None
                }
            } else {
                None
            };
            Ok(selected_training)
        };
        commit_pending_progress(&mut pending_parents, staged_parent_progress);
        ledger.append_entries(verification.experience);
        ledger.append_candidate_fates(candidate_fates);
        test_fault_point("experience-appended");
        let mut measurement_admission_started = instrumentation.start();
        let accepted_origins = verification
            .accepted
            .iter()
            .map(|(_, origin, _)| *origin)
            .collect::<Vec<_>>();
        let accepted_attempts = verification
            .accepted
            .iter()
            .map(|(_, _, attempt_id)| *attempt_id)
            .collect::<Vec<_>>();
        let accepted = materialize(
            domain,
            bundle_codec.semantic_identity(),
            verification
                .accepted
                .into_iter()
                .map(|(artifact, _, _)| artifact)
                .collect(),
            &environment,
            resource_meter,
            verification_resident,
        )?;
        for (artifact, attempt_id) in accepted.iter().zip(&accepted_attempts) {
            ledger.append_measurement(domain, artifact, *attempt_id)?;
        }
        test_fault_point("measurement-completed");
        let previous_goal_frontiers = goal_evaluator.frontiers(&known);
        let mut admitted_attempts = Vec::new();
        let mut admitted = Vec::new();
        for ((artifact, origin), attempt_id) in accepted
            .into_iter()
            .zip(accepted_origins)
            .zip(accepted_attempts)
        {
            if !known.iter().any(|known| known.key() == artifact.key()) {
                admitted_attempts.push((artifact.clone(), attempt_id));
                admitted.push((artifact, origin));
            }
        }
        test_fault_point("admission-completed");
        if admitted.is_empty() {
            instrumentation.finish(Phase::MeasurementAdmission, measurement_admission_started);
            measurement_admission_started = instrumentation.start();
            let selected_training = run_training_market(&[])?;
            let mut frame = SettlementFrame::observations(
                &operational_receipts,
                &operational_settlements,
                &[],
                &[],
            );
            if let Some(prepared) = &selected_training {
                frame = frame.with_prepared_native_training(prepared);
            }
            let transition = intelligence.stage(frame).map_err(map_intelligence_error)?;
            test_fault_point("intelligence-settled");
            let cohort_live = worker_resident_bytes
                .saturating_add(resident_state_bytes(
                    &known,
                    &roots,
                    &pareto,
                    &frontier,
                    &recovered_keys,
                    &operators,
                    &checkpoint,
                    &ledger,
                    &intelligence,
                    intelligence_working_bytes,
                ))
                .saturating_add(deferred_resident);
            let cohort_checkpoint = publish_intelligence_transition(
                transition,
                &mut intelligence,
                &seed_cursor,
                &IntelligencePublicationState {
                    artifacts: &known,
                    pareto: &pareto,
                    frontier: &frontier,
                    deferred_candidates: &deferred_candidates,
                    pending_parents: &pending_parents,
                    generation_complete: false,
                    ledger: &ledger,
                },
                &checkpoint,
                verification_requests,
                cohort_live,
                &bundle_codec,
                resource_meter,
                &mut durability,
            )?;
            checkpoint = cohort_checkpoint;
            test_fault_point("cohort-published");
            instrumentation.finish(Phase::MeasurementAdmission, measurement_admission_started);
            time_exhausted = resource_meter
                .search_time_exhausted()
                .map_err(|()| SessionError::Resource)?;
            if time_exhausted {
                break;
            }
            if deferred_candidates.is_empty() && pending_parents.is_empty() {
                frontier.clear();
                break;
            }
            continue;
        }
        instrumentation.admitted(admitted.len());
        let mut epoch = EpochTransition::begin(&mut known, &mut frontier, &mut ledger);
        for (artifact, origin) in admitted {
            pending_parents.push(PendingParent::new(
                artifact.key(),
                primitive_operator_count,
                has_derived_operators,
            ));
            epoch.admit(artifact, origin);
        }
        let previous_keys = pareto.iter().map(VerifiedArtifact::key).collect::<Vec<_>>();
        let proposed_goal_frontiers = goal_evaluator.frontiers(epoch.known());
        let proposed_pareto = GoalEvaluator::<D>::pareto_from_frontiers(&proposed_goal_frontiers);
        let (staged_known, staged_entries, staged_consequences) = epoch.admission_state();
        record_admission_consequences(
            &goal_evaluator,
            staged_known,
            &proposed_pareto,
            &previous_keys,
            staged_entries,
            &admitted_attempts,
            staged_consequences,
        );
        let mut intelligence_consequences = Vec::new();
        for (artifact, attempt_id) in &admitted_attempts {
            let Some(decision) = semantic_consequence_decision(
                *attempt_id,
                &verification.action_decisions,
                &verification.verification_decisions,
            ) else {
                continue;
            };
            intelligence_consequences.push(ConsequenceEdge::observed(
                CausalSubject::Decision(decision),
                IntelligenceConsequenceKind::Admitted,
            ));
            if proposed_pareto
                .iter()
                .any(|candidate| candidate.key() == artifact.key())
                && !previous_keys.contains(&artifact.key())
            {
                intelligence_consequences.push(ConsequenceEdge::observed(
                    CausalSubject::Decision(decision),
                    IntelligenceConsequenceKind::ParetoImprovement,
                ));
            }
        }
        let mut proposed_live = worker_resident_bytes
            .saturating_add(resident_state_bytes(
                epoch.known(),
                &roots,
                &proposed_pareto,
                epoch.frontier(),
                &recovered_keys,
                &operators,
                &checkpoint,
                epoch.ledger(),
                &intelligence,
                intelligence_working_bytes,
            ))
            .saturating_add(cohort::recovery_resident_bytes(
                domain,
                &deferred_candidates,
                &pending_parents,
            ));
        let selected_training = run_training_market(&intelligence_consequences)?;
        let training = selected_training
            .as_ref()
            .map(|_| native_training_budget(runtime_policy));
        let consequence_transient = intelligence.transition_resident_bytes(
            operational_receipts.len(),
            operational_settlements.len(),
            intelligence_consequences.len(),
            training,
        );
        if !resource_meter.reserve(
            ResidentReservation::live(proposed_live)
                .with_transient(consequence_transient)
                .with_pending_durability(durability.pending_bytes()),
        ) {
            return Err(SessionError::Resource);
        }
        let mut frame = SettlementFrame::observations(
            &operational_receipts,
            &operational_settlements,
            &intelligence_consequences,
            &[],
        );
        if let Some(prepared) = &selected_training {
            frame = frame.with_prepared_native_training(prepared);
        }
        let intelligence_transition = intelligence.stage(frame).map_err(map_intelligence_error)?;
        test_fault_point("intelligence-consequences-staged");
        instrumentation.finish(Phase::MeasurementAdmission, measurement_admission_started);
        measurement_admission_started = instrumentation.start();
        let proposed_checkpoint = publish_intelligence_transition(
            intelligence_transition,
            &mut intelligence,
            &seed_cursor,
            &IntelligencePublicationState {
                artifacts: epoch.known(),
                pareto: &proposed_pareto,
                frontier: epoch.frontier(),
                deferred_candidates: &deferred_candidates,
                pending_parents: &pending_parents,
                generation_complete: false,
                ledger: epoch.ledger(),
            },
            &checkpoint,
            verification_requests,
            proposed_live,
            &bundle_codec,
            resource_meter,
            &mut durability,
        )?;
        proposed_live = proposed_live
            .saturating_sub(vector_bytes(&checkpoint))
            .saturating_add(vector_bytes(&proposed_checkpoint));
        let proposed_peak = ResidentReservation::live(proposed_live)
            .with_pending_durability(durability.pending_bytes())
            .peak_bytes();
        checkpoint = proposed_checkpoint;
        test_fault_point("pareto-published");
        epoch.commit();
        goal_evaluator.extend_parent_ranks(&frontier, &mut parent_ranks);
        instrumentation.finish(Phase::MeasurementAdmission, measurement_admission_started);
        pareto = proposed_pareto;
        let delivery = deliver_delta(
            &mut observer,
            &mut sequence,
            &previous_keys,
            &pareto,
            &goal_evaluator.affected(&previous_goal_frontiers, &proposed_goal_frontiers),
            resource_meter,
            proposed_peak,
        );
        if delivery.resource_exhausted() {
            instrumentation.refused(ResourceRefusal::ResidentEpoch);
            resident_budget_exhausted = true;
            break;
        }
        stopped_by_observer = delivery.stopped();
        success_conditions_satisfied =
            goal_evaluator.success_satisfied(&goal_evaluator.frontiers(&known));
        time_exhausted = resource_meter
            .search_time_exhausted()
            .map_err(|()| SessionError::Resource)?;
        if verification_budget_exhausted {
            break;
        }
    }

    let mut completion = if stopped_by_observer {
        Completion::StoppedByObserver
    } else if success_conditions_satisfied {
        Completion::SuccessConditionsSatisfied
    } else if verification_budget_exhausted
        || durable_budget_exhausted
        || resident_budget_exhausted
        || policy_trial_invalidated
        || time_exhausted
    {
        Completion::ResourceEnvelopeExhausted
    } else {
        Completion::NoEligibleWork
    };

    let provisional_usage = resource_meter
        .usage(verification_requests, checkpoint.len() as u64)
        .map_err(|()| SessionError::Resource)?;
    if matches!(completion, Completion::NoEligibleWork)
        && (provisional_usage.elapsed_time >= request.resources.elapsed_time.get()
            || provisional_usage.cpu_time >= request.resources.cpu_time.get())
    {
        completion = Completion::ResourceEnvelopeExhausted;
    }
    let consolidation_started = instrumentation.start();
    let mut consolidation_live = worker_resident_bytes
        .saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &checkpoint,
            &ledger,
            &intelligence,
            intelligence_working_bytes,
        ))
        .saturating_add(cohort::recovery_resident_bytes(
            domain,
            &deferred_candidates,
            &pending_parents,
        ));
    let primitive_symbols = domain
        .operators()
        .catalog()
        .iter()
        .map(|descriptor| descriptor.symbol().as_str().as_bytes().to_vec())
        .collect::<BTreeSet<_>>();
    let derivation_shape = ledger.derivation_shape(&pinned_knowledge, &primitive_symbols)?;
    let consolidation_transient = intelligence
        .knowledge_compilation_resident_bound(
            derivation_shape,
            roots.len().saturating_add(pareto.len()),
        )
        .map_err(map_intelligence_error)?
        .saturating_add(
            (MAX_CONSOLIDATION_WITNESS_BRANCHES as u64)
                .saturating_mul(MIN_CHOICE_RESIDENT_BYTES)
                .saturating_mul(2),
        );
    let consolidation_due = completion != Completion::StoppedByObserver
        && ledger.len()
            >= usize::try_from(runtime_policy.consolidation_cadence()).unwrap_or(usize::MAX);
    let can_consolidate = consolidation_due
        && !policy_trial_invalidated
        && !durable_budget_exhausted
        && verification_requests < request.resources.verification_requests().get()
        && !resource_meter
            .time_exhausted()
            .map_err(|()| SessionError::Resource)?
        && resource_meter.reserve(
            ResidentReservation::live(consolidation_live)
                .with_transient(consolidation_transient)
                .with_pending_durability(durability.pending_bytes()),
        );
    if can_consolidate {
        operational_receipts.clear();
        operational_settlements.clear();
        operational_specs.clear();
        let resumed_compilation = intelligence
            .pending_knowledge_compilation()
            .map_err(map_intelligence_error)?;
        let (compiled_work, consolidation_decision) = if let Some((work, producer)) =
            resumed_compilation
        {
            (Some(work), Some(producer))
        } else {
            let consolidation_allowance = remaining_resource_allowance(
                request,
                resource_meter,
                verification_requests,
                consolidation_live,
                checkpoint.len() as u64,
            )?;
            let compilation_plan = intelligence.prepare_knowledge_compilation(ResourceVector::new(
                consolidation_allowance.cpu_time_ns,
                consolidation_transient,
                0,
                consolidation_allowance.elapsed_time_ns,
                0,
            ));
            operational_specs.extend_from_slice(compilation_plan.specifications());
            let prepared =
                PreparedOperationalMarket::prepare(&operational_specs, &mut intelligence_market)
                    .map_err(map_intelligence_error)?;
            let actions = prepared
                .allocate(
                    &intelligence,
                    &intelligence_market,
                    consolidation_allowance,
                    selection_epoch,
                    1,
                    &mut intelligence_portfolio,
                )
                .map_err(map_intelligence_error)?;
            let action = actions
                .into_iter()
                .next()
                .ok_or(SessionError::CorruptBundle)?;
            let OperationalAction::Consolidate { compiler } = action.action() else {
                return Err(SessionError::CorruptBundle);
            };
            let consolidation_decision = action.decision();
            let consolidation_roots = roots
                .iter()
                .map(|artifact| artifact.key().0)
                .collect::<Vec<_>>();
            let pareto_roots = pareto
                .iter()
                .map(|artifact| artifact.key().0)
                .collect::<Vec<_>>();
            let started = std::time::Instant::now();
            let cpu_before = resource_meter
                .current_cpu()
                .map_err(|()| SessionError::Resource)?;
            let executed_compilation = compilation_plan
                .execute(
                    &intelligence,
                    consolidation_decision,
                    compiler,
                    &consolidation_roots,
                    &pareto_roots,
                    || ledger.derivations(&pinned_knowledge, &primitive_symbols),
                )
                .map_err(|error| match error {
                    KnowledgeCompilationError::Source(error) => error,
                    KnowledgeCompilationError::Intelligence(error) => map_intelligence_error(error),
                })?;
            let cpu = resource_meter
                .current_cpu()
                .map_err(|()| SessionError::Resource)?
                .saturating_sub(cpu_before);
            let (receipt, settlement) = action
                .settle(
                    &intelligence,
                    InvestmentOutcome::Completed,
                    ResourceVector::new(
                        u64::try_from(cpu.as_nanos()).unwrap_or(u64::MAX),
                        consolidation_transient,
                        0,
                        u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                        0,
                    ),
                )
                .map_err(map_intelligence_error)?;
            let transition = intelligence
                .stage_completed_knowledge_compilation(executed_compilation, &receipt, settlement)
                .map_err(map_intelligence_error)?;
            let action_checkpoint = publish_intelligence_transition(
                transition,
                &mut intelligence,
                &seed_cursor,
                &IntelligencePublicationState {
                    artifacts: &known,
                    pareto: &pareto,
                    frontier: &frontier,
                    deferred_candidates: &deferred_candidates,
                    pending_parents: &pending_parents,
                    generation_complete: false,
                    ledger: &ledger,
                },
                &checkpoint,
                verification_requests,
                consolidation_live,
                &bundle_codec,
                resource_meter,
                &mut durability,
            )?;
            consolidation_live = consolidation_live
                .saturating_sub(vector_bytes(&checkpoint))
                .saturating_add(vector_bytes(&action_checkpoint));
            checkpoint = action_checkpoint;
            let compiled_work = intelligence
                .pending_knowledge_compilation()
                .map_err(map_intelligence_error)?
                .map(|(work, producer)| {
                    debug_assert_eq!(producer, consolidation_decision);
                    work
                });
            (compiled_work, Some(consolidation_decision))
        };
        let mut promoted_knowledge = false;
        let mut promoted_current_compilation = false;
        if let Some(resources) = protected_policy_trial_resources(
            request,
            resource_meter,
            verification_requests,
            consolidation_live,
            checkpoint.len() as u64,
            runtime_policy,
        )? {
            let prior_product = intelligence.knowledge_product();
            let arm_intelligence =
                IntelligenceCore::restore(knowledge_campaign_intelligence.as_bytes())
                    .map_err(map_intelligence_error)?;
            if let Some(shadow_outcome) = execute_knowledge_shadow(
                domain,
                bundle_codec.semantic_identity(),
                &roots,
                &known[..knowledge_campaign_known_len],
                &goal_evaluator,
                &ledger.entries()[..knowledge_campaign_experience_len],
                &mut intelligence,
                &arm_intelligence,
                runtime_policy,
                knowledge_campaign_epoch,
                knowledge_campaign_checkpoint,
                scheduler,
                resource_meter,
                verification_workers,
                consolidation_live,
                durability.pending_bytes(),
                resources,
                &mut intelligence_market,
                &mut intelligence_portfolio,
                &mut operator_scratch,
                |transition, added_requests, intelligence| {
                    let causal_requests = verification_requests
                        .checked_add(added_requests)
                        .ok_or(SessionError::Resource)?;
                    let next_checkpoint = publish_intelligence_transition(
                        transition,
                        intelligence,
                        &seed_cursor,
                        &IntelligencePublicationState {
                            artifacts: &known,
                            pareto: &pareto,
                            frontier: &frontier,
                            deferred_candidates: &deferred_candidates,
                            pending_parents: &pending_parents,
                            generation_complete: false,
                            ledger: &ledger,
                        },
                        &checkpoint,
                        causal_requests,
                        consolidation_live,
                        &bundle_codec,
                        resource_meter,
                        &mut durability,
                    )?;
                    checkpoint = next_checkpoint;
                    Ok(())
                },
            )? {
                let shadow_requests = shadow_outcome.verification_requests;
                verification_requests = verification_requests
                    .checked_add(shadow_requests)
                    .ok_or(SessionError::Resource)?;
                instrumentation.verified(usize::try_from(shadow_requests).unwrap_or(usize::MAX));
                promoted_knowledge = intelligence.knowledge_product() != prior_product;
                if promoted_knowledge {
                    let mut consequences = ledger.consequences().to_vec();
                    for subject in shadow_outcome.compressed_attempts {
                        let consequence = ConsequenceObservation {
                            subject,
                            kind: ConsequenceKind::Compression,
                        };
                        if !consequences.contains(&consequence) {
                            consequences.push(consequence);
                        }
                    }
                    ledger.replace_consequences(consequences);
                }
            }
        }
        // A Knowledge Shadow publication advances the authoritative Core
        // revision. Reconstruct any still-pending compilation from that Core
        // instead of attempting to settle the pre-shadow `KnowledgeWork`,
        // whose base revision is necessarily stale.
        let work = if !promoted_knowledge && compiled_work.is_some() {
            intelligence
                .pending_knowledge_compilation()
                .map_err(map_intelligence_error)?
                .map(|(work, producer)| {
                    if Some(producer) != consolidation_decision {
                        return Err(SessionError::CorruptBundle);
                    }
                    Ok(work)
                })
                .transpose()?
        } else {
            None
        };
        if let Some(work) = work
            && !work.obligations().is_empty()
        {
            let prior_product = intelligence.knowledge_product();
            let reviewed = propose_and_verify_knowledge_product(
                domain,
                &roots,
                &ledger,
                work,
                scheduler,
                resource_meter,
                verification_workers,
                consolidation_live,
                durability.pending_bytes(),
                request
                    .resources
                    .verification_requests()
                    .get()
                    .saturating_sub(verification_requests),
                selection_epoch,
                &mut operator_scratch,
                &mut intelligence,
                &mut intelligence_market,
                &mut intelligence_portfolio,
                &mut instrumentation,
                |transition, added_requests, intelligence| {
                    let causal_requests = verification_requests
                        .checked_add(added_requests)
                        .ok_or(SessionError::Resource)?;
                    let next_checkpoint = publish_intelligence_transition(
                        transition,
                        intelligence,
                        &seed_cursor,
                        &IntelligencePublicationState {
                            artifacts: &known,
                            pareto: &pareto,
                            frontier: &frontier,
                            deferred_candidates: &deferred_candidates,
                            pending_parents: &pending_parents,
                            generation_complete: false,
                            ledger: &ledger,
                        },
                        &checkpoint,
                        causal_requests,
                        consolidation_live,
                        &bundle_codec,
                        resource_meter,
                        &mut durability,
                    )?;
                    checkpoint = next_checkpoint;
                    Ok(())
                },
            )?;
            verification_requests = verification_requests
                .checked_add(reviewed)
                .ok_or(SessionError::Resource)?;
            promoted_knowledge = intelligence.knowledge_product() != prior_product;
            promoted_current_compilation = promoted_knowledge;
        }
        if promoted_current_compilation && let Some(consolidation_decision) = consolidation_decision
        {
            let consequences = [ConsequenceEdge::mechanically_induced(
                CausalSubject::Decision(consolidation_decision),
                IntelligenceConsequenceKind::Compression,
            )];
            let transition = intelligence
                .stage(SettlementFrame::observations(&[], &[], &consequences, &[]))
                .map_err(map_intelligence_error)?;
            checkpoint = publish_intelligence_transition(
                transition,
                &mut intelligence,
                &seed_cursor,
                &IntelligencePublicationState {
                    artifacts: &known,
                    pareto: &pareto,
                    frontier: &frontier,
                    deferred_candidates: &deferred_candidates,
                    pending_parents: &pending_parents,
                    generation_complete: false,
                    ledger: &ledger,
                },
                &checkpoint,
                verification_requests,
                consolidation_live,
                &bundle_codec,
                resource_meter,
                &mut durability,
            )?;
        }
    }
    reconcile_derived_progress(
        &mut pending_parents,
        &pinned_knowledge,
        intelligence.pinned_knowledge_revision(),
    );
    instrumentation.finish(Phase::Consolidation, consolidation_started);
    let finalization_started = instrumentation.start();
    let (sealed_deferred, sealed_pending) = if completion == Completion::ResourceEnvelopeExhausted {
        (&deferred_candidates[..], &pending_parents[..])
    } else {
        (&[][..], &[][..])
    };
    let finalization_live = worker_resident_bytes
        .saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &checkpoint,
            &ledger,
            &intelligence,
            intelligence_working_bytes,
        ))
        .saturating_add(cohort::recovery_resident_bytes(
            domain,
            &deferred_candidates,
            &pending_parents,
        ));
    let provisional_checkpoint = bundle_codec.seal_admitted(
        &seed_cursor,
        RestartBundleState::new(
            &known,
            &pareto,
            SearchTailView::new(&frontier, sealed_deferred, sealed_pending),
            &ledger,
            &intelligence,
        ),
        SessionSeal::Completed(completion, provisional_usage),
        resource_meter,
        SealAdmission {
            resident_overlap: finalization_live,
            additional_transient: 0,
            pending_durability: durability.pending_bytes(),
        },
    )?;
    let usage = resource_meter
        .usage(verification_requests, provisional_checkpoint.len() as u64)
        .map_err(|()| SessionError::Resource)?;
    drop(provisional_checkpoint);
    checkpoint = bundle_codec.seal_admitted(
        &seed_cursor,
        RestartBundleState::new(
            &known,
            &pareto,
            SearchTailView::new(&frontier, sealed_deferred, sealed_pending),
            &ledger,
            &intelligence,
        ),
        SessionSeal::Completed(completion, usage),
        resource_meter,
        SealAdmission {
            resident_overlap: finalization_live,
            additional_transient: 0,
            pending_durability: durability.pending_bytes(),
        },
    )?;
    durability
        .submit(checkpoint)
        .map_err(SessionError::Durability)?;
    durability.finish().map_err(SessionError::Durability)?;
    instrumentation.finish(Phase::Finalization, finalization_started);
    let target = request.bundle.target().to_path_buf();
    Ok(SessionOutcome {
        completion,
        pareto: ParetoSnapshot { artifacts: pareto },
        usage,
        bundle: DomainBundle::published(target),
    })
}

fn intelligence_limits<D: DomainDefinition>(
    request: &ImprovementRequest<D>,
    runtime_policy: RuntimePolicyRevision,
    resident_overlap: u64,
) -> IntelligenceLimits {
    let maximum_opportunities = usize::try_from(request.resources.verification_requests.get())
        .unwrap_or(4_096)
        .clamp(1, 4_096);
    let scratch_budget = request
        .resources
        .resident_bytes()
        .get()
        .saturating_sub(resident_overlap)
        .saturating_sub(MIN_SESSION_DYNAMIC_HEADROOM_BYTES);
    let mut lower = 1_usize;
    let mut upper = maximum_opportunities;
    while lower < upper {
        let middle = lower + (upper - lower).div_ceil(2);
        let limits = intelligence_limits_for_opportunities(middle, runtime_policy);
        let mandatory_bytes = limits.scratch_layout().ok().map(|layout| {
            layout
                .total_bytes()
                .saturating_add(IntelligenceCore::fresh(limits).resident_bytes())
        });
        if mandatory_bytes.is_some_and(|bytes| bytes <= scratch_budget) {
            lower = middle;
        } else {
            upper = middle - 1;
        }
    }
    intelligence_limits_for_opportunities(lower, runtime_policy)
}

fn operational_spec_limit(catalog_len: usize, maximum_opportunities: usize) -> usize {
    let catalog_cap = if catalog_len < 9 { catalog_len } else { 9 };
    if catalog_cap < maximum_opportunities {
        catalog_cap
    } else {
        maximum_opportunities
    }
}

fn intelligence_limits_for_opportunities(
    opportunities: usize,
    runtime_policy: RuntimePolicyRevision,
) -> IntelligenceLimits {
    let specialists = usize::from(runtime_policy.active_specialist_cap());
    IntelligenceLimits::new(
        opportunities,
        opportunities,
        opportunities.saturating_mul(specialists),
        opportunities.saturating_mul(specialists).saturating_mul(9),
        specialists,
        1 << 20,
    )
    .expect("the built-in Intelligence limits are nonzero and bounded")
}

fn native_training_budget(runtime_policy: RuntimePolicyRevision) -> NativeTrainingBudget {
    let maximum_examples = usize::try_from(runtime_policy.training_cadence())
        .unwrap_or(256)
        .clamp(4, 256);
    NativeTrainingBudget::new(maximum_examples, 12, 0.25)
        .expect("the built-in native Training budget is finite and supervisor-bounded")
}

fn runtime_policy_trial_checkpoint(
    checkpoint: &[u8],
    proposals: &[cohort::CohortProposal],
    fates: &[CandidateFateObservation],
    fate_is_new_generation: &[bool],
    incumbent_plan: &cohort::PolicySelectionPlan,
    challenger_plan: &cohort::PolicySelectionPlan,
) -> [u8; 32] {
    let proposal_identities = proposals
        .iter()
        .map(cohort::CohortProposal::trial_identity)
        .collect::<Vec<_>>();
    let fate_identities = fates
        .iter()
        .map(|fate| {
            let mut digest = Sha256::new();
            digest.update(b"reflex-candidate-fate-v1\0");
            digest.update(fate.candidate_key.as_bytes());
            digest.update(fate.claim_digest);
            digest.update(fate.parent_key.as_bytes());
            digest.update(fate.operator_digest);
            if let Some(provenance) = fate.proposal_provenance {
                digest.update([1]);
                digest.update(provenance.support_key());
            } else {
                digest.update([0]);
            }
            digest.update(fate.epoch.to_le_bytes());
            digest.update(fate.generation_rank.to_le_bytes());
            digest.update(fate.proposal_limit.to_le_bytes());
            digest.update(fate.policy_rank.value().unwrap_or(u32::MAX).to_le_bytes());
            digest.update(
                fate.bootstrap_rank
                    .value()
                    .unwrap_or(u32::MAX)
                    .to_le_bytes(),
            );
            digest.update(fate.learned_rank.value().unwrap_or(u32::MAX).to_le_bytes());
            digest.update(fate.verification_batch_cpu_ns.to_le_bytes());
            digest.update(fate.verification_batch_size.to_le_bytes());
            digest.update([fate.disposition as u8]);
            digest.update([fate.allocation_queue.map_or(0, |queue| queue as u8)]);
            digest.finalize().into()
        })
        .collect::<Vec<_>>();
    bind_runtime_policy_trial_checkpoint(
        Sha256::digest(checkpoint).into(),
        &proposal_identities,
        &fate_identities,
        fate_is_new_generation,
        incumbent_plan.trial_identity(),
        challenger_plan.trial_identity(),
    )
}

fn bind_runtime_policy_trial_checkpoint(
    checkpoint: [u8; 32],
    proposal_identities: &[[u8; 32]],
    fate_identities: &[[u8; 32]],
    fate_is_new_generation: &[bool],
    incumbent_plan: [u8; 32],
    challenger_plan: [u8; 32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-runtime-policy-trial-checkpoint-v1\0");
    digest.update(checkpoint);
    digest.update((proposal_identities.len() as u64).to_le_bytes());
    for identity in proposal_identities {
        digest.update(identity);
    }
    digest.update((fate_identities.len() as u64).to_le_bytes());
    for identity in fate_identities {
        digest.update(identity);
    }
    digest.update((fate_is_new_generation.len() as u64).to_le_bytes());
    for generated in fate_is_new_generation {
        digest.update([u8::from(*generated)]);
    }
    digest.update(incumbent_plan);
    digest.update(challenger_plan);
    digest.finalize().into()
}

fn policy_trial_evidence<D: DomainDefinition>(
    verification: &VerificationOutcome<D>,
    verification_requests: u64,
) -> (Vec<TypedOutcome>, u64) {
    let verified_discoveries = verification
        .experience
        .iter()
        .filter(|entry| entry.verdict == ExperienceVerdict::Accepted)
        .count() as u64;
    let covered_claims = verification
        .experience
        .iter()
        .filter(|entry| entry.verdict == ExperienceVerdict::Accepted)
        .map(|entry| entry.claim_digest)
        .collect::<BTreeSet<_>>()
        .len() as u64;
    let durable_bytes = verification.experience.iter().fold(0_u64, |bytes, entry| {
        bytes.saturating_add(encoded_entry_len(
            entry.canonical_candidate.len(),
            entry.operator_symbol.len(),
            entry.proposal_provenance.is_some(),
        ))
    });
    let measured = |value: u64| f32::from(u16::try_from(value).unwrap_or(u16::MAX));
    let outcomes = vec![
        TypedOutcome::new(ForecastAxis::KernelAcceptance, 0.0)
            .expect("zero correctness failures is finite"),
        TypedOutcome::new(
            ForecastAxis::ImmediateImprovement,
            measured(verified_discoveries),
        )
        .expect("bounded discovery counts are finite"),
        TypedOutcome::new(ForecastAxis::UsefulDescendants, 0.0)
            .expect("zero descendants is finite"),
        TypedOutcome::new(ForecastAxis::CrossGoalLeverage, measured(covered_claims))
            .expect("bounded Claim counts are finite"),
        TypedOutcome::new(
            ForecastAxis::VerificationCost,
            measured(verification_requests),
        )
        .expect("bounded Verification counts are finite"),
    ];
    (outcomes, durable_bytes)
}

fn semantic_consequence_decision(
    attempt: [u8; 32],
    action_decisions: &[([u8; 32], DecisionId)],
    verification_decisions: &[([u8; 32], DecisionId)],
) -> Option<DecisionId> {
    action_decisions
        .iter()
        .find_map(|(observed, decision)| (*observed == attempt).then_some(*decision))
        .or_else(|| {
            verification_decisions
                .iter()
                .find_map(|(observed, decision)| (*observed == attempt).then_some(*decision))
        })
}

type ReplayedPolicyPlan<D> = Result<
    (Vec<ProposedCandidate<D>>, Vec<CandidateFateObservation>),
    SessionError<<D as DomainDefinition>::Error>,
>;

struct PolicyShadowArmResidentView<'a, D: DomainDefinition> {
    restart: &'a Vec<u8>,
    proposals: &'a Vec<cohort::CohortProposal>,
    retained_fates: &'a Vec<CandidateFateObservation>,
    retained_fate_is_new: &'a Vec<bool>,
    plans: [&'a cohort::PolicySelectionPlan; 2],
    trial_recovery_deferred: &'a Vec<ProposedCandidate<D>>,
    candidates: &'a Vec<ProposedCandidate<D>>,
    candidate_fates: &'a Vec<CandidateFateObservation>,
}

impl<D: DomainDefinition> PolicyShadowArmResidentView<'_, D> {
    fn resident_bytes(&self, domain: &D) -> u64 {
        let proposal_graph = policy_proposal_graph_resident_bytes(self.proposals);
        let candidate_graph = policy_candidate_graph_resident_bytes(domain, self.candidates);
        vector_bytes(self.restart)
            .saturating_add(proposal_graph)
            .saturating_add(vector_bytes(self.retained_fates))
            // The retained classification vector and its replay clone coexist
            // until candidate reconstruction is complete.
            .saturating_add(vector_bytes(self.retained_fate_is_new).saturating_mul(2))
            .saturating_add(std::mem::size_of_val(self.plans[0]) as u64)
            .saturating_add(std::mem::size_of_val(self.plans[1]) as u64)
            .saturating_add(policy_candidate_graph_resident_bytes(
                domain,
                self.trial_recovery_deferred,
            ))
            .saturating_add(candidate_pipeline_reserve(
                domain,
                self.trial_recovery_deferred,
            ))
            .saturating_add(candidate_graph)
            .saturating_add(vector_bytes(self.candidate_fates))
            .saturating_add(candidate_pipeline_reserve(domain, self.candidates))
    }
}

fn policy_candidate_graph_resident_bytes<D: DomainDefinition>(
    domain: &D,
    candidates: &Vec<ProposedCandidate<D>>,
) -> u64 {
    candidates
        .iter()
        .fold(vector_bytes(candidates), |bytes, candidate| {
            bytes
                .saturating_add(candidate.canonical_candidate.capacity() as u64)
                .saturating_add(candidate.operator_symbol.capacity() as u64)
                .saturating_add(
                    domain
                        .structure()
                        .view(&candidate.candidate.artifact)
                        .dynamic_resident_bytes(),
                )
        })
}

fn policy_proposal_graph_resident_bytes(proposals: &Vec<cohort::CohortProposal>) -> u64 {
    vector_bytes(proposals).saturating_add(proposals.iter().fold(0_u64, |bytes, proposal| {
        bytes.saturating_add(proposal.resident_bytes())
    }))
}

fn replay_policy_plan<D: DomainDefinition>(
    domain: &D,
    proposals: &[cohort::CohortProposal],
    plan: &cohort::PolicySelectionPlan,
    mut candidate_fates: Vec<CandidateFateObservation>,
    fate_is_new_generation: Vec<bool>,
) -> ReplayedPolicyPlan<D> {
    let mut scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    let mut candidates = plan
        .selected()
        .map(|index| {
            proposals[index]
                .replay(domain, &mut scratch)
                .map_err(SessionError::Domain)
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (policy_rank, candidate) in candidates.iter().enumerate() {
        let fate = candidate_fates
            .get_mut(candidate.fate_index)
            .ok_or(SessionError::CorruptBundle)?;
        fate.disposition = CandidateFateDisposition::VerificationInterrupted;
        fate.allocation_queue = Some(candidate.allocation_queue);
        fate.policy_rank =
            CandidateRank::present(u32::try_from(policy_rank).map_err(|_| SessionError::Resource)?);
        fate.bootstrap_rank = candidate.bootstrap_rank;
        fate.learned_rank = candidate.learned_rank;
    }
    let candidate_fates =
        retain_transaction_fates(&mut candidates, candidate_fates, fate_is_new_generation);
    Ok((candidates, candidate_fates))
}

fn protected_policy_trial_resources<D: DomainDefinition>(
    request: &ImprovementRequest<D>,
    resource_meter: &ResourceEnvelopeGuard,
    verification_requests: u64,
    live_bytes: u64,
    durable_bytes: u64,
    policy: RuntimePolicyRevision,
) -> Result<Option<shadow::VerificationSubEnvelope>, SessionError<D::Error>> {
    let remaining = remaining_resource_allowance(
        request,
        resource_meter,
        verification_requests,
        live_bytes,
        durable_bytes,
    )?;
    Ok(shadow::VerificationSubEnvelope::protected(
        remaining,
        policy.shadow_budget_per_mille(),
    ))
}

fn remaining_resource_allowance<D: DomainDefinition>(
    request: &ImprovementRequest<D>,
    resource_meter: &ResourceEnvelopeGuard,
    verification_requests: u64,
    live_bytes: u64,
    durable_bytes: u64,
) -> Result<ResourceVector, SessionError<D::Error>> {
    let usage = resource_meter
        .usage(verification_requests, durable_bytes)
        .map_err(|()| SessionError::Resource)?;
    let cpu = request
        .resources
        .cpu_time()
        .get()
        .saturating_sub(usage.cpu_time);
    let elapsed = request
        .resources
        .elapsed_time()
        .get()
        .saturating_sub(usage.elapsed_time);
    Ok(ResourceVector::new(
        u64::try_from(cpu.as_nanos()).unwrap_or(u64::MAX),
        request
            .resources
            .resident_bytes()
            .get()
            .saturating_sub(live_bytes),
        request
            .resources
            .durable_bytes()
            .get()
            .saturating_sub(usage.durable_bytes),
        u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
        request
            .resources
            .verification_requests()
            .get()
            .saturating_sub(verification_requests),
    ))
}

fn crossed_cadence(prior: usize, added: usize, cadence: u32) -> bool {
    let cadence = usize::try_from(cadence).unwrap_or(usize::MAX).max(1);
    prior / cadence < prior.saturating_add(added) / cadence
}

fn intelligence_transition_reserve(
    intelligence: &IntelligenceCore,
    maximum_receipts: usize,
    training: NativeTrainingBudget,
) -> u64 {
    intelligence.transition_resident_bytes(maximum_receipts, maximum_receipts, 0, Some(training))
}

fn map_intelligence_error<E>(error: IntelligenceError) -> SessionError<E> {
    match error {
        IntelligenceError::CapacityExceeded | IntelligenceError::ResourceOverflow => {
            SessionError::Resource
        }
        _ => SessionError::CorruptBundle,
    }
}

/// Publishes one staged Intelligence transition without exposing a proposed Core
/// before its restart-complete Bundle is durable. The allocation-free bound is
/// reserved before checkpoint materialization, either Bundle seal, or transfer
/// to the durability worker.
#[expect(
    clippy::too_many_arguments,
    reason = "the private publication seam names each restart and resource authority exactly once"
)]
fn publish_intelligence_transition<D: DomainDefinition>(
    transition: IntelligenceTransition,
    intelligence: &mut IntelligenceCore,
    seed_cursor: &[u8],
    state: &IntelligencePublicationState<'_, D>,
    current_checkpoint: &[u8],
    verification_requests: u64,
    resident_overlap: u64,
    bundle_codec: &RestartBundleCodec<'_, D>,
    resource_meter: &ResourceEnvelopeGuard,
    durability: &mut durability::CheckpointWriter,
) -> Result<Vec<u8>, SessionError<D::Error>> {
    let search_tail = SearchTailView::new(
        state.frontier,
        state.deferred_candidates,
        state.pending_parents,
    );
    let seal_plan = bundle_codec.seal_plan_for_parts(
        seed_cursor,
        state.artifacts,
        state.pareto,
        &search_tail,
        state.ledger,
        transition.checkpoint_encoded_len(),
    );
    RestartBundleCodec::<D>::admit(
        seal_plan,
        resource_meter,
        resident_overlap,
        transition.materialization_resident_bytes(intelligence),
        durability.pending_bytes(),
    )?;

    let prepared = transition
        .prepare(intelligence)
        .map_err(map_intelligence_error)?;

    let provisional_usage = resource_meter
        .usage(verification_requests, current_checkpoint.len() as u64)
        .map_err(|()| SessionError::Resource)?;
    let proposed = prepared.proposed(intelligence);
    let provisional = bundle_codec.seal_prepared(
        seed_cursor,
        state.restart_state(&proposed),
        SessionSeal::Interrupted(provisional_usage),
        seal_plan,
    )?;
    let usage = resource_meter
        .usage(verification_requests, provisional.len() as u64)
        .map_err(|()| SessionError::Resource)?;
    drop(provisional);
    let next_checkpoint = bundle_codec.seal_prepared(
        seed_cursor,
        state.restart_state(&proposed),
        SessionSeal::Interrupted(usage),
        seal_plan,
    );
    let next_checkpoint = next_checkpoint?;
    durability
        .submit(next_checkpoint)
        .map_err(SessionError::Durability)?;
    let (_, next_checkpoint) = durability
        .barrier_retaining()
        .map_err(SessionError::Durability)?;
    prepared.commit(intelligence);
    Ok(next_checkpoint)
}

fn verification_batch_fits(remaining_requests: u64, requested: usize) -> bool {
    u64::try_from(requested).is_ok_and(|requested| requested <= remaining_requests)
}

#[cfg(test)]
fn run_if_verification_fits<T, E>(
    remaining_requests: u64,
    requested: usize,
    run: impl FnOnce() -> Result<T, E>,
) -> Result<Option<T>, E> {
    if !verification_batch_fits(remaining_requests, requested) {
        return Ok(None);
    }
    run().map(Some)
}

fn verification_batch_resources(
    allowance: VerificationAllowance,
    verification_requests: u64,
) -> ResourceVector {
    ResourceVector::new(
        u64::try_from(allowance.cpu_time().as_nanos()).unwrap_or(u64::MAX),
        allowance.resident_bytes(),
        0,
        u64::try_from(allowance.elapsed_time().as_nanos()).unwrap_or(u64::MAX),
        verification_requests,
    )
}

fn verification_obligation_resources(
    batch: ResourceVector,
    index: usize,
    count: usize,
) -> Result<ResourceVector, ()> {
    let count = u64::try_from(count).map_err(|_| ())?;
    let index = u64::try_from(index).map_err(|_| ())?;
    if count == 0 || index >= count || batch.verification_requests != count {
        return Err(());
    }
    let cpu_base = batch.cpu_time_ns / count;
    let cpu_remainder = batch.cpu_time_ns % count;
    Ok(ResourceVector::new(
        cpu_base.saturating_add(u64::from(index < cpu_remainder)),
        if index == 0 { batch.resident_bytes } else { 0 },
        0,
        if index == 0 { batch.elapsed_time_ns } else { 0 },
        1,
    ))
}

fn knowledge_verification_batch_fits(available: ResourceVector, required: ResourceVector) -> bool {
    required.fits_within(available)
}

fn verification_kernel_usage(
    report: VerificationBatchReport,
    in_process_cpu: std::time::Duration,
    in_process_elapsed: std::time::Duration,
    verification_requests: u64,
) -> ResourceVector {
    let external = report.external_usage();
    let uses_external_worker = external.worker_lanes() != 0;
    ResourceVector::new(
        u64::try_from(
            if uses_external_worker {
                external.cpu_time()
            } else {
                in_process_cpu
            }
            .as_nanos(),
        )
        .unwrap_or(u64::MAX),
        external.peak_resident_bytes(),
        0,
        u64::try_from(
            if uses_external_worker {
                external.elapsed_time()
            } else {
                in_process_elapsed
            }
            .as_nanos(),
        )
        .unwrap_or(u64::MAX),
        verification_requests,
    )
}

fn external_worker_cpu_ns(report: VerificationBatchReport) -> u64 {
    let external = report.external_usage();
    if external.worker_lanes() == 0 {
        0
    } else {
        u64::try_from(external.cpu_time().as_nanos()).unwrap_or(u64::MAX)
    }
}

fn shadow_elapsed_ns(controller_elapsed: std::time::Duration, kernel_elapsed_ns: u64) -> u64 {
    u64::try_from(controller_elapsed.as_nanos())
        .unwrap_or(u64::MAX)
        .max(kernel_elapsed_ns)
}

fn interrupt_opened_knowledge<D: DomainDefinition>(
    intelligence: &mut IntelligenceCore,
    opened: &OpenedKnowledgeWork,
    reserved_requests: u64,
    publish: &mut impl FnMut(
        IntelligenceTransition,
        u64,
        &mut IntelligenceCore,
    ) -> Result<(), SessionError<D::Error>>,
) -> Result<(), SessionError<D::Error>> {
    let transition = intelligence
        .settle_opened_knowledge_verification(opened, KnowledgeVerificationReport::interrupted())
        .map_err(map_intelligence_error)?;
    test_fault_point("knowledge-verification-interruption-staged");
    publish(transition, reserved_requests, intelligence)?;
    test_fault_point("knowledge-verification-interruption-published");
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Knowledge compilation names its verifier, resource, and durable Intelligence authorities"
)]
#[expect(
    clippy::too_many_lines,
    reason = "Knowledge compilation keeps proposal, authoritative review, and atomic Intelligence settlement in one audited path"
)]
fn propose_and_verify_knowledge_product<D: DomainDefinition>(
    domain: &D,
    roots: &[VerifiedArtifact<D>],
    ledger: &ExperienceLedger,
    work: KnowledgeWork,
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
    pending_durability_bytes: u64,
    remaining_verification_requests: u64,
    selection_epoch: u64,
    operator_scratch: &mut <D::Operators as OperatorAlgebra<D>>::Scratch,
    intelligence: &mut IntelligenceCore,
    market: &mut MarketArena,
    portfolio: &mut PortfolioBuffer,
    instrumentation: &mut Recorder,
    mut publish: impl FnMut(
        IntelligenceTransition,
        u64,
        &mut IntelligenceCore,
    ) -> Result<(), SessionError<D::Error>>,
) -> Result<u64, SessionError<D::Error>> {
    let obligation_count = work.obligations().len();
    if !verification_batch_fits(remaining_verification_requests, obligation_count) {
        return Ok(0);
    }
    let mut witnesses = Vec::with_capacity(work.obligations().len());
    let mut missing_obligation = None;
    for obligation in work.obligations() {
        let Some((artifact, origin)) =
            reproduce_consolidation_witness(domain, roots, ledger, obligation, operator_scratch)?
        else {
            missing_obligation = Some(obligation.subject());
            break;
        };
        witnesses.push((artifact, origin, obligation.subject()));
    }
    if let Some(obligation) = missing_obligation {
        let transition = intelligence
            .reject_knowledge_work(
                work,
                KnowledgePlanningFailure::WitnessUnavailable(obligation),
            )
            .map_err(map_intelligence_error)?;
        test_fault_point("knowledge-verification-planning-failure-staged");
        publish(transition, 0, intelligence)?;
        test_fault_point("knowledge-verification-planning-failure-published");
        return Ok(0);
    }
    let allowance = resource_meter
        .verification_allowance(requirements.worker_lanes(), resident_overlap)
        .map_err(|()| SessionError::Resource)?;
    let request_count = u64::try_from(obligation_count).map_err(|_| SessionError::Resource)?;
    let reserved = verification_batch_resources(allowance, request_count);
    let available = verification_batch_resources(allowance, remaining_verification_requests);
    if !knowledge_verification_batch_fits(available, reserved) {
        return Ok(0);
    }
    let mut receipts = Vec::with_capacity(witnesses.len());
    for (index, (_, _, obligation)) in witnesses.iter().enumerate() {
        let forecast = verification_obligation_resources(reserved, index, obligation_count)
            .map_err(|()| SessionError::Resource)?;
        market.clear();
        portfolio.clear();
        let opportunity = market
            .push_opportunity(OpportunitySpec::candidate(obligation.identity()), &[1.0])
            .map_err(map_intelligence_error)?;
        market
            .push_investment(InvestmentSpec::verify(opportunity, forecast, 0))
            .map_err(map_intelligence_error)?;
        let epoch = selection_epoch
            .checked_add(u64::try_from(index).map_err(|_| SessionError::Resource)?)
            .ok_or(SessionError::Resource)?;
        let allocation = intelligence
            .allocate(
                MarketFrame::new(market, forecast).at_epoch(epoch),
                1,
                portfolio,
            )
            .map_err(map_intelligence_error)?;
        receipts.push(
            *allocation
                .receipts()
                .first()
                .ok_or(SessionError::CorruptBundle)?,
        );
    }
    let open_resident = intelligence
        .knowledge_open_transition_resident_bytes(&work, &receipts, reserved)
        .map_err(map_intelligence_error)?;
    if !resource_meter.reserve(
        ResidentReservation::live(resident_overlap)
            .with_transient(open_resident)
            .with_pending_durability(pending_durability_bytes),
    ) {
        return Ok(0);
    }
    let (open_transition, _opened) = intelligence
        .open_knowledge_verification(work, &receipts, reserved)
        .map_err(map_intelligence_error)?;
    test_fault_point("knowledge-verification-open-staged");
    publish(open_transition, request_count, intelligence)?;
    test_fault_point("knowledge-verification-open-published");
    let opened = intelligence
        .opened_knowledge_verification()
        .ok_or(SessionError::CorruptBundle)?;
    let requests = witnesses
        .iter()
        .map(|(artifact, origin, _)| ClaimVerificationRequest {
            seed: roots[*origin].artifact(),
            candidate: artifact,
        })
        .collect::<Vec<_>>();
    let kernel_started = std::time::Instant::now();
    let kernel_cpu_before = resource_meter
        .current_cpu()
        .map_err(|()| SessionError::Resource)?;
    let verification = scheduled_verify_with_report(
        domain,
        scheduler,
        resource_meter,
        requirements,
        resident_overlap,
        &requests,
        false,
        None,
    );
    let (verdicts, report) = match verification {
        Ok(verified) => verified,
        Err(error) => {
            interrupt_opened_knowledge::<D>(intelligence, &opened, request_count, &mut publish)?;
            return Err(error);
        }
    };
    let process_cpu_ns = u64::try_from(
        resource_meter
            .current_cpu()
            .map_err(|()| SessionError::Resource)?
            .saturating_sub(kernel_cpu_before)
            .as_nanos(),
    )
    .unwrap_or(u64::MAX);
    if verdicts.len() != witnesses.len() {
        interrupt_opened_knowledge::<D>(intelligence, &opened, request_count, &mut publish)?;
        return Err(SessionError::InvalidSeed);
    }
    let actual = verification_kernel_usage(
        report,
        std::time::Duration::from_nanos(process_cpu_ns),
        kernel_started.elapsed(),
        request_count,
    );
    if !actual.fits_within(reserved) {
        interrupt_opened_knowledge::<D>(intelligence, &opened, request_count, &mut publish)?;
        return Err(SessionError::Resource);
    }
    let mut settlements = Vec::with_capacity(verdicts.len());
    for (index, (((_, verdict, _), (_, _, obligation)), receipt)) in verdicts
        .into_iter()
        .zip(&witnesses)
        .zip(opened.receipts())
        .enumerate()
    {
        let resources = verification_obligation_resources(actual, index, obligation_count)
            .map_err(|()| SessionError::Resource)?;
        let outcome = match verdict {
            Verdict::Accepted { .. } => InvestmentOutcome::VerifiedAccepted {
                verification_record: *obligation,
            },
            Verdict::Refuted => InvestmentOutcome::VerifiedRefuted,
            Verdict::Unknown => InvestmentOutcome::VerifiedUnknown,
        };
        settlements.push(InvestmentSettlement::new(
            receipt.decision(),
            outcome,
            resources,
        ));
    }
    let transition = intelligence
        .settle_opened_knowledge_verification(
            &opened,
            KnowledgeVerificationReport::settled(&settlements),
        )
        .map_err(map_intelligence_error)?;
    test_fault_point("knowledge-verification-settlement-staged");
    publish(transition, request_count, intelligence)?;
    test_fault_point("knowledge-verification-settlement-published");
    instrumentation.verified(witnesses.len());
    Ok(request_count)
}

#[cfg(test)]
fn goal_preferred_eligible_index(
    ranks: &[usize],
    claims: &[[u8; 32]],
    excluded_claims: &BTreeSet<[u8; 32]>,
) -> Option<usize> {
    assert_eq!(ranks.len(), claims.len());
    claims
        .iter()
        .enumerate()
        .filter(|(_, claim)| !excluded_claims.contains(*claim))
        .min_by_key(|(index, _)| (ranks[*index], *index))
        .map(|(index, _)| index)
}

fn knowledge_campaign_request_limit(
    resources: ResourceVector,
    runtime_policy: RuntimePolicyRevision,
) -> u64 {
    resources
        .verification_requests
        .min(u64::from(runtime_policy.verification_cohort()))
}

#[expect(
    clippy::too_many_arguments,
    reason = "a downstream Campaign arm names its pinned search, goal, Intelligence, Kernel, and resource authorities"
)]
#[expect(
    clippy::too_many_lines,
    reason = "one bounded Campaign arm keeps production generation, allocation, Verification, and admission causally local"
)]
fn run_knowledge_campaign_arm<D: DomainDefinition>(
    domain: &D,
    semantic_identity: &SemanticIdentity,
    roots: &[VerifiedArtifact<D>],
    initial_known: &[VerifiedArtifact<D>],
    initial_frontier: &[(VerifiedArtifact<D>, usize)],
    initial_experience: &[ExperienceEntry],
    goals: &GoalEvaluator<'_, D>,
    runtime_policy: RuntimePolicyRevision,
    installed_knowledge: &KnowledgeRevision,
    subject_operator: &[u8],
    initial_intelligence: &IntelligenceCore,
    market: &mut MarketArena,
    portfolio: &mut PortfolioBuffer,
    initial_parent: usize,
    initial_epoch: u64,
    context: shadow::ShadowArmContext,
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
) -> Result<KnowledgeCampaignArmOutcome, SessionError<D::Error>> {
    let started = std::time::Instant::now();
    let cpu_before = resource_meter
        .current_cpu()
        .map_err(|()| SessionError::Resource)?;
    let mut known = initial_known.to_vec();
    let mut frontier = initial_frontier.to_vec();
    let mut experience = initial_experience.to_vec();
    let intelligence = initial_intelligence;
    let primitive_count = domain.operators().catalog().len();
    let has_derived = installed_knowledge
        .operators()
        .iter()
        .any(crate::knowledge::DerivedOperator::active);
    let mut pending = frontier
        .iter()
        .map(|(artifact, _)| PendingParent::new(artifact.key(), primitive_count, has_derived))
        .collect::<Vec<_>>();
    pending.swap(0, initial_parent);
    let mut peak_resident_bytes = KnowledgeCampaignResidentView {
        known: &known,
        frontier: &frontier,
        experience: &experience,
        pending: &pending,
    }
    .resident_bytes();
    if peak_resident_bytes > context.resources().resident_bytes {
        return Err(SessionError::Resource);
    }
    let mut operator_scratch = <D::Operators as OperatorAlgebra<D>>::Scratch::default();
    let mut epoch = initial_epoch;
    let mut request_count = 0_u64;
    let mut kernel_elapsed_ns = 0_u64;
    let mut external_worker_cpu_ns = 0_u64;
    let mut useful_descendants = 0.0_f32;
    let mut compression_value = 0.0_f32;
    let mut subject_lineage = BTreeSet::new();
    let request_limit = knowledge_campaign_request_limit(context.resources(), runtime_policy);
    while request_count < request_limit && !pending.is_empty() {
        let cpu_used = resource_meter
            .current_cpu()
            .map_err(|()| SessionError::Resource)?
            .saturating_sub(cpu_before);
        let cpu_used_ns = u64::try_from(cpu_used.as_nanos())
            .unwrap_or(u64::MAX)
            .saturating_add(external_worker_cpu_ns);
        let elapsed_used = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if cpu_used_ns >= context.resources().cpu_time_ns
            || elapsed_used >= context.resources().elapsed_time_ns
        {
            break;
        }
        let ranks = goals.parent_ranks(&frontier);
        let Some((pending_index, parent_index)) = pending
            .iter()
            .enumerate()
            .filter_map(|(pending_index, progress)| {
                frontier
                    .iter()
                    .position(|(artifact, _)| artifact.key() == progress.key)
                    .map(|parent_index| (pending_index, parent_index))
            })
            .min_by_key(|(_, parent_index)| (ranks.as_slice()[*parent_index], *parent_index))
        else {
            return Err(SessionError::CorruptBundle);
        };
        let remaining =
            usize::try_from(request_limit.saturating_sub(request_count)).unwrap_or(usize::MAX);
        let generation_limit = remaining.min(usize::from(runtime_policy.verification_cohort()));
        if generation_limit == 0 {
            break;
        }
        let progress = &mut pending[pending_index];
        let parent = frontier[parent_index].0.artifact();
        let parents = ProposalParents {
            artifacts: &[parent],
            frontier_indexes: &[parent_index],
        };
        let derived_budget = if !progress.derived_sampled && has_derived {
            proportional_budget(generation_limit, runtime_policy.exploration_per_mille())
                .max(1)
                .min(generation_limit)
        } else {
            0
        };
        let primitive_budget = generation_limit.saturating_sub(derived_budget);
        let mut candidates = Vec::new();
        StructuredRewriteEngine.propose(
            domain,
            &mut StructuredRewriteRequest {
                parents,
                operator_offsets: &mut progress.primitive_offsets,
                limit: primitive_budget,
                epoch,
                permitted_operators: None,
            },
            &mut operator_scratch,
            &mut candidates,
        )?;
        if derived_budget > 0 {
            DerivedOperatorEngine.propose(
                domain,
                &mut DerivedOperatorRequest {
                    parents,
                    knowledge: installed_knowledge,
                    limit: derived_budget,
                    epoch,
                },
                &mut operator_scratch,
                &mut candidates,
            )?;
            progress.derived_sampled = true;
        }
        if progress.complete() {
            pending.remove(pending_index);
        }
        epoch = epoch.checked_add(1).ok_or(SessionError::Resource)?;
        if candidates.is_empty() {
            continue;
        }
        let novel =
            retain_novel_candidates(domain, &known, &experience, roots, &frontier, candidates)?;
        let mut candidates = novel.candidates;
        let mut candidate_fates = novel.fates;
        if candidates.is_empty() {
            continue;
        }
        let mut covered_claims = experience
            .iter()
            .map(|entry| entry.claim_digest)
            .collect::<Vec<_>>();
        covered_claims.sort_unstable();
        covered_claims.dedup();
        let ranks = goals.parent_ranks(&frontier);
        let deferred = order_by_learned_potential(
            domain,
            ranks.as_slice(),
            &frontier,
            &covered_claims,
            intelligence,
            market,
            portfolio,
            context.resources(),
            remaining,
            &mut candidates,
            &mut candidate_fates,
        );
        debug_assert!(
            deferred.is_empty(),
            "Campaign generation is bounded by its cohort"
        );
        for (policy_rank, candidate) in candidates.iter().enumerate() {
            let fate = candidate_fates
                .get_mut(candidate.fate_index)
                .ok_or(SessionError::CorruptBundle)?;
            fate.disposition = CandidateFateDisposition::VerificationInterrupted;
            fate.allocation_queue = Some(candidate.allocation_queue);
            fate.policy_rank =
                CandidateRank::present(u32::try_from(policy_rank).unwrap_or(u32::MAX));
            fate.bootstrap_rank = candidate.bootstrap_rank;
            fate.learned_rank = candidate.learned_rank;
        }
        candidate_fates = retain_transaction_fates(
            &mut candidates,
            candidate_fates,
            novel.fate_is_new_generation,
        );
        let verification_resident = KnowledgeCampaignResidentView {
            known: &known,
            frontier: &frontier,
            experience: &experience,
            pending: &pending,
        }
        .resident_bytes()
        .saturating_add(vector_bytes(&candidates))
        .saturating_add(candidate_pipeline_reserve(domain, &candidates))
        .saturating_add(vector_bytes(&candidate_fates))
        .saturating_add(allocation_bytes::<usize, _>(frontier.len())?)
        .saturating_add(allocation_bytes::<ArtifactKey, _>(frontier.len())?);
        peak_resident_bytes = peak_resident_bytes.max(verification_resident);
        if peak_resident_bytes > context.resources().resident_bytes {
            return Err(SessionError::Resource);
        }
        let batch_requests = u64::try_from(candidates.len()).map_err(|_| SessionError::Resource)?;
        let mut parent_origins = exact_vec(frontier.len())?;
        parent_origins.extend(frontier.iter().map(|(_, origin)| *origin));
        let mut parent_keys = exact_vec(frontier.len())?;
        parent_keys.extend(frontier.iter().map(|(artifact, _)| artifact.key()));
        let cpu_remaining = context.resources().cpu_time_ns.saturating_sub(cpu_used_ns);
        let elapsed_remaining = context
            .resources()
            .elapsed_time_ns
            .saturating_sub(elapsed_used);
        let allowance = VerificationAllowance::new(
            requirements.worker_lanes().max(1),
            context
                .resources()
                .resident_bytes
                .saturating_sub(verification_resident),
            std::time::Duration::from_nanos(elapsed_remaining),
            std::time::Duration::from_nanos(cpu_remaining),
        );
        let mut arm_instrumentation = Recorder::from_environment();
        let verification = verify_candidates(
            domain,
            roots,
            &parent_origins,
            &parent_keys,
            candidates,
            scheduler,
            resource_meter,
            requirements,
            resident_overlap.saturating_add(verification_resident),
            &mut arm_instrumentation,
            &mut candidate_fates,
            Some(allowance),
        )?;
        peak_resident_bytes = peak_resident_bytes
            .max(verification_resident.saturating_add(verification.kernel_usage.resident_bytes));
        external_worker_cpu_ns =
            external_worker_cpu_ns.saturating_add(verification.external_worker_cpu_ns);
        kernel_elapsed_ns =
            kernel_elapsed_ns.saturating_add(verification.kernel_usage.elapsed_time_ns);
        request_count = request_count
            .checked_add(batch_requests)
            .ok_or(SessionError::Resource)?;
        for entry in verification
            .experience
            .iter()
            .filter(|entry| entry.verdict == ExperienceVerdict::Accepted)
        {
            if subject_lineage.contains(&entry.parent_key) {
                useful_descendants += 1.0;
                subject_lineage.insert(entry.candidate_key);
            } else if entry.operator_symbol == subject_operator {
                subject_lineage.insert(entry.candidate_key);
            }
        }
        compression_value += verification
            .experience
            .iter()
            .filter(|entry| entry.verdict == ExperienceVerdict::Accepted)
            .map(|entry| entry.features.0[3].max(0.0))
            .sum::<f32>();
        experience.extend(verification.experience);
        let accepted_origins = verification
            .accepted
            .iter()
            .map(|(_, origin, _)| *origin)
            .collect::<Vec<_>>();
        let accepted = materialize(
            domain,
            semantic_identity,
            verification
                .accepted
                .into_iter()
                .map(|(artifact, _, _)| artifact)
                .collect(),
            &roots[0].inner.environment,
            resource_meter,
            resident_overlap.saturating_add(verification_resident),
        )?;
        for (artifact, origin) in accepted.into_iter().zip(accepted_origins) {
            if known.iter().any(|known| known.key() == artifact.key()) {
                continue;
            }
            known.push(artifact.clone());
            frontier.push((artifact.clone(), origin));
            pending.push(PendingParent::new(
                artifact.key(),
                primitive_count,
                has_derived,
            ));
        }
        peak_resident_bytes = peak_resident_bytes.max(
            KnowledgeCampaignResidentView {
                known: &known,
                frontier: &frontier,
                experience: &experience,
                pending: &pending,
            }
            .resident_bytes(),
        );
        if peak_resident_bytes > context.resources().resident_bytes {
            return Err(SessionError::Resource);
        }
    }
    let cpu_time_ns = resource_meter
        .current_cpu()
        .map_or(u64::MAX, |cpu| {
            u64::try_from(cpu.saturating_sub(cpu_before).as_nanos()).unwrap_or(u64::MAX)
        })
        .saturating_add(external_worker_cpu_ns);
    Ok(KnowledgeCampaignArmOutcome {
        useful_descendants,
        compression_value,
        usage: ResourceVector::new(
            cpu_time_ns,
            peak_resident_bytes,
            0,
            shadow_elapsed_ns(started.elapsed(), kernel_elapsed_ns),
            request_count,
        ),
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the paired Knowledge trial names its pinned restart, Kernel, resource, and compiler authorities"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the authoritative compiler transaction keeps causal publication, paired execution, and atomic promotion together"
)]
fn execute_knowledge_shadow<D: DomainDefinition>(
    domain: &D,
    semantic_identity: &SemanticIdentity,
    roots: &[VerifiedArtifact<D>],
    known: &[VerifiedArtifact<D>],
    goals: &GoalEvaluator<'_, D>,
    experience: &[ExperienceEntry],
    intelligence: &mut IntelligenceCore,
    arm_intelligence: &IntelligenceCore,
    runtime_policy: RuntimePolicyRevision,
    selection_epoch: u64,
    restart_checkpoint: CheckpointDigest,
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
    pending_durability_bytes: u64,
    mut resources: shadow::VerificationSubEnvelope,
    market: &mut MarketArena,
    portfolio: &mut PortfolioBuffer,
    _operator_scratch: &mut <D::Operators as OperatorAlgebra<D>>::Scratch,
    mut publish: impl FnMut(
        IntelligenceTransition,
        u64,
        &mut IntelligenceCore,
    ) -> Result<(), SessionError<D::Error>>,
) -> Result<Option<VerifiedKnowledgeShadowOutcome>, SessionError<D::Error>> {
    let root_frontier = roots
        .iter()
        .enumerate()
        .map(|(origin, root)| (root.clone(), origin))
        .collect::<Vec<_>>();
    let root_ranks = goals.parent_ranks(&root_frontier);
    let root_facts = root_frontier
        .iter()
        .zip(root_ranks.as_slice())
        .map(|((root, _), preference_rank)| {
            KnowledgeShadowRootFact::new(
                SubjectId::new(root.key().0),
                SubjectId::new(root.inner.claim_digest),
                u32::try_from(*preference_rank).unwrap_or(u32::MAX),
            )
        })
        .collect::<Vec<_>>();
    let support_facts = experience
        .iter()
        .map(|entry| {
            KnowledgeShadowSupportFact::new(entry.attempt_id, SubjectId::new(entry.claim_digest))
        })
        .collect::<Vec<_>>();
    let mut context = Sha256::new();
    context.update(b"reflex-knowledge-shadow-context-v1\0");
    context.update(arm_intelligence.checkpoint().identity());
    context.update(runtime_policy.identity());
    context.update(selection_epoch.to_le_bytes());
    let context_digest = SubjectId::new(context.finalize().into());
    let Some(plan) = intelligence
        .plan_knowledge_shadow(
            restart_checkpoint,
            context_digest,
            &root_facts,
            &support_facts,
            resources.per_arm(),
        )
        .map_err(map_intelligence_error)?
    else {
        return Ok(None);
    };
    let reserved_verification_requests = plan
        .per_arm_allowance()
        .verification_requests
        .checked_mul(2)
        .ok_or(SessionError::Resource)?;
    let selected_root_key = ArtifactKey(plan.root().identity());
    let selected_root = root_frontier
        .iter()
        .position(|(root, _)| root.key() == selected_root_key)
        .ok_or(SessionError::CorruptBundle)?;
    let mut campaign_frontier = known
        .iter()
        .filter(|artifact| artifact.inner.origin_key == selected_root_key)
        .map(|artifact| (artifact.clone(), selected_root))
        .collect::<Vec<_>>();
    if !campaign_frontier
        .iter()
        .any(|(artifact, _)| artifact.key() == selected_root_key)
    {
        campaign_frontier.push((root_frontier[selected_root].0.clone(), selected_root));
    }
    campaign_frontier.sort_unstable_by_key(|(artifact, _)| artifact.key());
    let root_index = campaign_frontier
        .iter()
        .position(|(artifact, _)| artifact.key() == selected_root_key)
        .expect("the selected Seed root remains in the Campaign Frontier");
    let primitive_count = domain.operators().catalog().len();
    let minimum_arm_resident = knowledge_campaign_initial_resident_bound(
        known,
        &campaign_frontier,
        experience,
        primitive_count,
    );
    if minimum_arm_resident > resources.per_arm().resident_bytes {
        return Ok(None);
    }
    let (open_transition, opened) = intelligence
        .open_knowledge_shadow(plan)
        .map_err(map_intelligence_error)?;
    test_fault_point("knowledge-shadow-open-staged");
    publish(
        open_transition,
        reserved_verification_requests,
        intelligence,
    )?;
    test_fault_point("knowledge-shadow-open-published");

    let execution = opened
        .execution(intelligence)
        .map_err(map_intelligence_error)?;
    let product_subject = execution.product();
    let subject_operator = execution.subject_operator().to_vec();
    let support = execution
        .supporting_attempts()
        .iter()
        .copied()
        .collect::<BTreeSet<_>>();
    let treatment_knowledge = execution.pinned_revision(ShadowArm::Treatment).clone();
    let incumbent_knowledge = execution.pinned_revision(ShadowArm::Control).clone();
    let random_stream = execution.random_stream();

    let plan = shadow::ShadowExecutionPlan::new(
        shadow::SubjectMask::new(product_subject),
        execution.per_arm_allowance(),
        random_stream,
        shadow::SchedulingToken::new(execution.scheduling_token().identity()),
    );
    let run = shadow::ShadowRunner::run(
        KnowledgeShadowCheckpoint {
            installed: treatment_knowledge,
            incumbent: incumbent_knowledge,
        },
        plan,
        |checkpoint, subject| {
            if *subject != shadow::SubjectMask::new(product_subject) {
                return Err(crate::intelligence::ShadowInvalidationReason::CheckpointMismatch);
            }
            checkpoint.installed = checkpoint.incumbent.clone();
            Ok(())
        },
        |checkpoint, context| {
            if !resource_meter.reserve(
                ResidentReservation::live(resident_overlap)
                    .with_transient(context.resources().resident_bytes),
            ) {
                return shadow::ArmExecution::Invalidated(
                    crate::intelligence::ShadowInvalidationReason::ResourceMismatch,
                );
            }
            let arm = run_knowledge_campaign_arm(
                domain,
                semantic_identity,
                roots,
                known,
                &campaign_frontier,
                experience,
                goals,
                runtime_policy,
                &checkpoint.installed,
                &subject_operator,
                arm_intelligence,
                market,
                portfolio,
                root_index,
                selection_epoch,
                context,
                scheduler,
                resource_meter,
                requirements,
                resident_overlap,
            );
            match arm {
                Ok(output) => {
                    if context.arm() == ShadowArm::Treatment {
                        test_fault_point("knowledge-shadow-treatment-completed");
                    }
                    shadow::ArmExecution::Completed(output)
                }
                Err(_) => shadow::ArmExecution::Invalidated(
                    crate::intelligence::ShadowInvalidationReason::Interrupted,
                ),
            }
        },
    );
    let output = match run {
        shadow::ShadowRunResult::Completed(output) => output,
        shadow::ShadowRunResult::Invalidated(reason) => {
            let settlement_bytes = intelligence
                .knowledge_shadow_settlement_transition_resident_bytes(&opened)
                .map_err(map_intelligence_error)?;
            if !resource_meter.reserve(
                ResidentReservation::live(resident_overlap)
                    .with_transient(settlement_bytes)
                    .with_pending_durability(pending_durability_bytes),
            ) {
                return Err(SessionError::Resource);
            }
            let transition = intelligence
                .settle_knowledge_shadow(opened, KnowledgeShadowReport::invalidated(reason))
                .map_err(map_intelligence_error)?;
            test_fault_point("knowledge-shadow-settlement-staged");
            publish(transition, reserved_verification_requests, intelligence)?;
            test_fault_point("knowledge-shadow-settlement-published");
            return Ok(Some(VerifiedKnowledgeShadowOutcome {
                compressed_attempts: support.into_iter().collect(),
                verification_requests: reserved_verification_requests,
            }));
        }
    };
    let (treatment, control) = output.into_arms();
    if let Err(reason) = resources
        .record(ShadowArm::Treatment, treatment.usage)
        .and_then(|()| resources.record(ShadowArm::Control, control.usage))
    {
        let settlement_bytes = intelligence
            .knowledge_shadow_settlement_transition_resident_bytes(&opened)
            .map_err(map_intelligence_error)?;
        if !resource_meter.reserve(
            ResidentReservation::live(resident_overlap)
                .with_transient(settlement_bytes)
                .with_pending_durability(pending_durability_bytes),
        ) {
            return Err(SessionError::Resource);
        }
        let transition = intelligence
            .settle_knowledge_shadow(opened, KnowledgeShadowReport::invalidated(reason))
            .map_err(map_intelligence_error)?;
        test_fault_point("knowledge-shadow-settlement-staged");
        publish(transition, reserved_verification_requests, intelligence)?;
        test_fault_point("knowledge-shadow-settlement-published");
        return Ok(Some(VerifiedKnowledgeShadowOutcome {
            compressed_attempts: support.into_iter().collect(),
            verification_requests: reserved_verification_requests,
        }));
    }
    let treatment_report = KnowledgeShadowArmReport::new(
        ShadowArm::Treatment,
        treatment.usage,
        treatment.useful_descendants,
        treatment.compression_value,
    )
    .map_err(map_intelligence_error)?;
    let control_report = KnowledgeShadowArmReport::new(
        ShadowArm::Control,
        control.usage,
        control.useful_descendants,
        control.compression_value,
    )
    .map_err(map_intelligence_error)?;
    let settlement_bytes = intelligence
        .knowledge_shadow_settlement_transition_resident_bytes(&opened)
        .map_err(map_intelligence_error)?;
    if !resource_meter.reserve(
        ResidentReservation::live(resident_overlap)
            .with_transient(settlement_bytes)
            .with_pending_durability(pending_durability_bytes),
    ) {
        return Err(SessionError::Resource);
    }
    let transition = intelligence
        .settle_knowledge_shadow(
            opened,
            KnowledgeShadowReport::paired(treatment_report, control_report),
        )
        .map_err(map_intelligence_error)?;
    test_fault_point("knowledge-shadow-settlement-staged");
    publish(transition, reserved_verification_requests, intelligence)?;
    test_fault_point("knowledge-shadow-settlement-published");
    Ok(Some(VerifiedKnowledgeShadowOutcome {
        compressed_attempts: support.into_iter().collect(),
        verification_requests: reserved_verification_requests,
    }))
}

#[cfg(feature = "internal-experiments")]
type IntelligenceInspection = (
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
    usize,
);

#[cfg(feature = "internal-experiments")]
pub(crate) fn inspect_intelligence_checkpoint(bytes: &[u8]) -> Result<IntelligenceInspection, ()> {
    let core = IntelligenceCore::restore(bytes).map_err(|_| ())?;
    let experience = core.experience();
    let knowledge = core.knowledge_statistics();
    let shadows = experience.shadow_campaigns().collect::<Vec<_>>();
    Ok((
        core.manifest().active_ids().count(),
        experience.receipts().len(),
        experience.settlements().len(),
        experience.consequences().len(),
        experience.contrasts().len(),
        knowledge.provisional,
        knowledge.verified,
        knowledge.promoted,
        knowledge.invalidated,
        shadows
            .iter()
            .filter(|campaign| {
                campaign.lifecycle() == crate::intelligence::ShadowCampaignLifecycle::Open
            })
            .count(),
        shadows
            .iter()
            .filter(|campaign| {
                campaign.lifecycle() == crate::intelligence::ShadowCampaignLifecycle::Completed
            })
            .count(),
        shadows
            .iter()
            .filter(|campaign| {
                campaign.lifecycle()
                    == crate::intelligence::ShadowCampaignLifecycle::Invalidated(
                        crate::intelligence::ShadowInvalidationReason::Interrupted,
                    )
            })
            .count(),
    ))
}

#[cfg(feature = "internal-experiments")]
type KnowledgeRevisionInspection = (u64, Vec<(bool, usize, usize)>);

#[cfg(feature = "internal-experiments")]
pub(crate) fn inspect_knowledge_revision_segment(
    bytes: &[u8],
) -> Result<KnowledgeRevisionInspection, ()> {
    const REVISION_ID_BYTES: usize = 4 * 32;
    let mut input = bytes.get(REVISION_ID_BYTES..).ok_or(())?;
    let first = take_sized::<()>(&mut input).map_err(|_| ())?;
    let (generation, revision) = if first.starts_with(b"RFIC") {
        let core = IntelligenceCore::restore(first).map_err(|_| ())?;
        (
            core.knowledge_generation(),
            core.pinned_knowledge_revision().clone(),
        )
    } else {
        let knowledge = KnowledgeState::decode(first)?;
        (knowledge.generation(), knowledge.pinned_revision().clone())
    };
    Ok((
        generation,
        revision
            .operators()
            .iter()
            .map(|operator| {
                (
                    operator.active(),
                    operator.steps().len(),
                    operator.support().len(),
                )
            })
            .collect(),
    ))
}

#[cfg(feature = "internal-experiments")]
pub(crate) fn inspect_intelligence_revision_segment(
    bytes: &[u8],
) -> Result<IntelligenceInspection, ()> {
    const REVISION_ID_BYTES: usize = 4 * 32;
    let mut input = bytes.get(REVISION_ID_BYTES..).ok_or(())?;
    let first = take_sized::<()>(&mut input).map_err(|_| ())?;
    let intelligence = if first.starts_with(b"RFIC") {
        first
    } else {
        take_sized::<()>(&mut input).map_err(|_| ())?
    };
    if !input.is_empty() {
        return Err(());
    }
    inspect_intelligence_checkpoint(intelligence)
}

#[expect(
    clippy::too_many_arguments,
    reason = "resident accounting names each independently owned live Session collection"
)]
fn resident_state_bytes<D: DomainDefinition, O>(
    known: &Vec<VerifiedArtifact<D>>,
    roots: &Vec<VerifiedArtifact<D>>,
    pareto: &Vec<VerifiedArtifact<D>>,
    frontier: &Vec<(VerifiedArtifact<D>, usize)>,
    recovered_keys: &Vec<ArtifactKey>,
    operators: &Vec<O>,
    checkpoint: &Vec<u8>,
    ledger: &ExperienceLedger,
    intelligence: &IntelligenceCore,
    intelligence_working_bytes: u64,
) -> u64 {
    let parent_preference_bytes = (frontier.capacity() as u64).saturating_mul(
        (std::mem::size_of::<ArtifactKey>() + 2 * std::mem::size_of::<usize>()) as u64,
    );
    let records = known.iter().fold(0_u64, |bytes, artifact| {
        bytes
            .saturating_add(std::mem::size_of::<VerifiedArtifactRecord<D>>() as u64)
            .saturating_add((2 * std::mem::size_of::<usize>()) as u64)
            .saturating_add(artifact.inner.dynamic_resident_bytes)
            .saturating_add(artifact.inner.environment.dynamic_resident_bytes())
            .saturating_add(artifact.inner.claim_canonical.len() as u64)
            .saturating_add(artifact.inner.bundle_record.len() as u64)
            .saturating_add(artifact.inner.provenance.capacity() as u64)
            .saturating_add(vector_bytes(&artifact.inner.measurements))
    });
    records
        .saturating_add(vector_bytes(known))
        .saturating_add(vector_bytes(roots))
        .saturating_add(vector_bytes(pareto))
        .saturating_add(vector_bytes(frontier))
        .saturating_add(parent_preference_bytes)
        .saturating_add(vector_bytes(recovered_keys))
        .saturating_add(vector_bytes(operators))
        .saturating_add(vector_bytes(checkpoint))
        .saturating_add(ledger.resident_bytes())
        .saturating_add(intelligence.resident_bytes())
        .saturating_add(intelligence_working_bytes)
}

fn vector_bytes<T>(values: &Vec<T>) -> u64 {
    (values.capacity() as u64).saturating_mul(std::mem::size_of::<T>() as u64)
}

fn root_locations<D: DomainDefinition>(
    domain: &D,
    artifacts: &[&D::Artifact],
) -> Vec<StructuralLocation> {
    artifacts
        .iter()
        .enumerate()
        .filter_map(|(artifact_index, artifact)| {
            domain
                .structure()
                .view(artifact)
                .node_count()
                .checked_sub(1)
                .map(|node_index| StructuralLocation::new(artifact_index, node_index))
        })
        .collect()
}

fn candidate_pipeline_reserve<D: DomainDefinition>(
    domain: &D,
    candidates: &[ProposedCandidate<D>],
) -> u64 {
    candidates.iter().fold(0_u64, |bytes, candidate| {
        let view = domain.structure().view(&candidate.candidate.artifact);
        let structural = view
            .dynamic_resident_bytes()
            .saturating_add((view.node_count() as u64).saturating_mul(256));
        let ledger = (std::mem::size_of::<ExperienceEntry>() as u64)
            .saturating_add(candidate.operator_symbol.capacity() as u64)
            .saturating_add(candidate.canonical_candidate.capacity() as u64)
            .saturating_add(structural)
            .saturating_mul(2);
        bytes.saturating_add(ledger.max(4 * 1024))
    })
}

fn prospective_experience_encoded_len<D: DomainDefinition>(
    ledger: &ExperienceLedger,
    candidates: &[ProposedCandidate<D>],
    candidate_fates: &[CandidateFateObservation],
) -> u64 {
    candidates
        .iter()
        .fold(ledger.encoded_len(), |bytes, candidate| {
            bytes.saturating_add(encoded_entry_len(
                candidate.canonical_candidate.len(),
                candidate.operator_symbol.len(),
                candidate.candidate.proposal_provenance.is_some(),
            ))
        })
        .saturating_add(encoded_candidate_fates_len(candidate_fates))
}

fn recovery_encoded_len<D: DomainDefinition>(
    pareto: &[VerifiedArtifact<D>],
    search_tail: &SearchTailView<'_, D>,
) -> u64 {
    let frontier = search_tail.frontier;
    let deferred = search_tail.deferred_candidates;
    let pending = search_tail.pending_parents;
    let deferred_bytes = deferred.iter().fold(0_u64, |bytes, candidate| {
        bytes
            .saturating_add(86)
            .saturating_add(candidate.canonical_candidate.len() as u64)
            .saturating_add(candidate.operator_symbol.len() as u64)
            .saturating_add(u64::from(candidate.candidate.proposal_provenance.is_some()) * 32)
    });
    let pending_bytes = pending.iter().fold(0_u64, |bytes, parent| {
        bytes
            .saturating_add(32 + 8 + 1)
            .saturating_add((parent.primitive_offsets.len() as u64).saturating_mul(8))
    });
    let action_provenance_bytes = deferred.iter().fold(8_u64, |bytes, candidate| {
        bytes
            .saturating_add(2)
            .saturating_add(u64::from(candidate.action_decision.is_some()).saturating_mul(32))
            .saturating_add(u64::from(candidate.causal_parent_key.is_some()).saturating_mul(32))
    });
    8_u64
        .saturating_add((pareto.len() as u64).saturating_mul(32))
        .saturating_add(8)
        .saturating_add((frontier.len() as u64).saturating_mul(32))
        .saturating_add(8)
        .saturating_add(deferred_bytes)
        .saturating_add(8)
        .saturating_add(pending_bytes)
        .saturating_add(action_provenance_bytes)
        .saturating_add(1)
}

fn recovery_after_admission_bound(
    current_recovery_bytes: u64,
    candidate_count: usize,
    primitive_operator_count: usize,
) -> u64 {
    let pending_parent_bytes = 32_u64
        .saturating_add(8)
        .saturating_add(1)
        .saturating_add((primitive_operator_count as u64).saturating_mul(8));
    let bytes_per_candidate = 32_u64
        .saturating_add(32)
        .saturating_add(pending_parent_bytes)
        .saturating_add(1)
        .saturating_add(66);
    current_recovery_bytes
        .saturating_add((candidate_count as u64).saturating_mul(bytes_per_candidate))
}

fn candidate_generation_limit(
    cohort_limit: usize,
    remaining_verifications: u64,
    pending_parent_count: usize,
    operator_count: usize,
    available_resident: u64,
    lookahead_depth: usize,
    shortlist_width: usize,
) -> usize {
    let cohort_limit = u64::try_from(cohort_limit).unwrap_or(u64::MAX);
    let lookahead_parents = cohort_limit
        .saturating_mul(u64::try_from(lookahead_depth).unwrap_or(u64::MAX))
        .min(remaining_verifications)
        .min(u64::try_from(pending_parent_count).unwrap_or(u64::MAX));
    let cohort_choices =
        cohort_limit.saturating_mul(u64::try_from(shortlist_width).unwrap_or(u64::MAX));
    let operator_breadth =
        lookahead_parents.saturating_mul(u64::try_from(operator_count).unwrap_or(u64::MAX));
    usize::try_from(
        cohort_choices
            .max(operator_breadth)
            .min(available_resident / MIN_CHOICE_RESIDENT_BYTES)
            .min(MAX_CANDIDATE_CHOICES),
    )
    .unwrap_or(usize::MAX)
}

fn generation_refill_limit(inventory_target: usize, deferred_candidates: usize) -> usize {
    inventory_target.saturating_sub(deferred_candidates)
}

fn proportional_budget(total: usize, per_mille: u16) -> usize {
    let numerator = (total as u128).saturating_mul(u128::from(per_mille));
    usize::try_from(numerator.div_ceil(1_000)).unwrap_or(usize::MAX)
}

fn runtime_policy_trial_sequence(selection_epoch: u64, cadence: u32) -> Option<u64> {
    let cadence = u64::from(cadence);
    (selection_epoch != 0 && selection_epoch.is_multiple_of(cadence))
        .then(|| selection_epoch / cadence - 1)
}

fn moved_tail_transaction_peak(stable_live: u64, prospective_tail: u64, transient: u64) -> u64 {
    ResidentReservation::live(stable_live.saturating_add(prospective_tail))
        .with_transient(transient)
        .peak_bytes()
}

fn opportunity_features<D: DomainDefinition>(
    domain: &D,
    parent: StructuralSummary,
    candidate: &D::Artifact,
    operator_features: [f32; 8],
    epoch: u64,
    proposal_features: ProposalFeatures,
) -> Features {
    let parent_nodes = parent.node_count;
    let candidate_nodes = structural_node_count(domain, candidate);
    let reduction = ((parent_nodes - candidate_nodes) / parent_nodes.max(1.0)).clamp(-1.0, 1.0);
    let mut values = [0.0; crate::learning::FEATURE_COUNT];
    values[0] = 1.0;
    values[1] = (parent_nodes / 1024.0).min(1.0);
    values[2] = (candidate_nodes / 1024.0).min(1.0);
    values[3] = reduction;
    values[4] = f32::from(u16::try_from(epoch).unwrap_or(u16::MAX)) / 1024.0;
    values[OPERATOR_FEATURE_START..OPERATOR_FEATURE_END].copy_from_slice(&operator_features);
    values[13] = reduction;
    values[14] = f32::from(candidate_nodes > parent_nodes);
    values[15] = (candidate_nodes / parent_nodes.max(1.0)).min(4.0) / 4.0;
    let mut features = Features(values);
    append_proposal_features(&mut features, proposal_features);
    features
}

fn append_proposal_features(features: &mut Features, proposal: ProposalFeatures) {
    features.0[crate::learning::BASE_FEATURE_COUNT..].copy_from_slice(&proposal.as_array());
}

fn structural_node_count<D: DomainDefinition>(domain: &D, artifact: &D::Artifact) -> f32 {
    f32::from(u16::try_from(domain.structure().view(artifact).node_count()).unwrap_or(u16::MAX))
}

#[cfg(feature = "internal-experiments")]
fn shape_summary<D: DomainDefinition>(
    domain: &D,
    artifact: &D::Artifact,
) -> Result<ShapeSummary<<D::Structure as StructuralProtocol<D>>::Constructor>, ()> {
    let view = domain.structure().view(artifact);
    let node_count = view.node_count();
    let mut depths = Vec::with_capacity(node_count);
    let mut constructor_counts = vec![0.0_f32; domain.structure().schema().constructors.len()];
    let mut children = Vec::new();
    for node in 0..node_count {
        let constructor = view.node_constructor(node).ok_or(())?;
        let constructor_index = domain
            .structure()
            .schema()
            .constructors
            .iter()
            .position(|descriptor| *descriptor.constructor() == constructor)
            .ok_or(())?;
        constructor_counts[constructor_index] += 1.0;
        if !view.write_children(node, &mut children) || children.iter().any(|child| *child >= node)
        {
            return Err(());
        }
        let depth = children
            .iter()
            .filter_map(|child| depths.get(*child))
            .copied()
            .max()
            .unwrap_or(0_u32)
            .saturating_add(1);
        depths.push(depth);
    }
    let node_count_f32 = bounded_usize_f32(node_count);
    if node_count != 0 {
        for count in &mut constructor_counts {
            *count /= node_count_f32;
        }
    }
    Ok(ShapeSummary {
        node_count: node_count_f32,
        depth: depths.last().copied().map_or(0.0, bounded_u32_f32),
        constructor_frequencies: constructor_counts,
        root_constructor: node_count
            .checked_sub(1)
            .and_then(|root| view.node_constructor(root)),
    })
}

#[cfg(feature = "internal-experiments")]
fn structural_opportunity_features<C: Copy + Eq>(
    parent: &ShapeSummary<C>,
    candidate: &ShapeSummary<C>,
    operator_symbol: &str,
    proposal_features: ProposalFeatures,
) -> Features {
    const NODE_SCALE: f32 = 11.512_936;
    const DEPTH_SCALE: f32 = 11.090_37;
    let mut values = [0.0; crate::learning::FEATURE_COUNT];
    let parent_nodes = parent.node_count.min(f32::from(u16::MAX));
    let candidate_nodes = candidate.node_count.min(f32::from(u16::MAX));
    let reduction = ((parent_nodes - candidate_nodes) / parent_nodes.max(1.0)).clamp(-1.0, 1.0);
    values[0] = 1.0;
    values[1] = (parent_nodes / 1024.0).min(1.0);
    values[2] = (candidate_nodes / 1024.0).min(1.0);
    values[3] = reduction;
    values[4] = (candidate.node_count.ln_1p() / NODE_SCALE).min(1.0);
    values[OPERATOR_FEATURE_START..OPERATOR_FEATURE_END]
        .copy_from_slice(&operator_feature_values(operator_symbol));
    values[13] = (candidate.depth.ln_1p() / DEPTH_SCALE).min(1.0);
    values[14] = parent
        .constructor_frequencies
        .iter()
        .zip(&candidate.constructor_frequencies)
        .map(|(parent, candidate)| (parent - candidate).abs())
        .sum::<f32>()
        / 2.0;
    values[15] = f32::from(
        parent.root_constructor.is_some() && parent.root_constructor == candidate.root_constructor,
    );
    let mut features = Features(values);
    append_proposal_features(&mut features, proposal_features);
    features
}

fn bounded_usize_f32(value: usize) -> f32 {
    bounded_u32_f32(u32::try_from(value).unwrap_or(u32::MAX))
}

fn bounded_u32_f32(value: u32) -> f32 {
    let high = u16::try_from(value >> 16).unwrap_or(u16::MAX);
    let low = u16::try_from(value & u32::from(u16::MAX)).unwrap_or(u16::MAX);
    f32::from(high) * 65_536.0 + f32::from(low)
}

fn operator_feature_values(operator_symbol: &str) -> [f32; 8] {
    let operator_digest = Sha256::digest(operator_symbol.as_bytes());
    let mut values = [0.0; 8];
    for (digest_byte, magnitude) in operator_digest.iter().zip([1.0, 0.5, 0.25]) {
        values[usize::from(*digest_byte % 8)] += magnitude;
    }
    values
}

fn commit_pending_progress(pending: &mut Vec<PendingParent>, staged: Vec<PendingParent>) {
    let staged_keys = staged
        .iter()
        .map(|progress| progress.key)
        .collect::<HashSet<_>>();
    assert!(
        staged.iter().all(|progress| progress.complete()
            || pending.iter().any(|retained| retained.key == progress.key)),
        "staged enumeration progress retains every pending parent"
    );
    pending.retain(|retained| !staged_keys.contains(&retained.key));
    for progress in staged {
        if !progress.complete() {
            pending.push(progress);
        }
    }
}

fn reconcile_derived_progress(
    pending: &mut Vec<PendingParent>,
    prior: &KnowledgeRevision,
    current: &KnowledgeRevision,
) {
    let active_ids = |revision: &KnowledgeRevision| {
        revision
            .operators()
            .iter()
            .filter(|operator| operator.active())
            .map(crate::knowledge::DerivedOperator::id)
            .collect::<Vec<_>>()
    };
    let prior_active = active_ids(prior);
    let current_active = active_ids(current);
    if prior_active == current_active {
        return;
    }
    let derived_sampled = current_active.is_empty();
    for parent in pending.iter_mut() {
        parent.derived_sampled = derived_sampled;
    }
    pending.retain(|parent| !parent.complete());
}

const MAX_CONSOLIDATION_WITNESS_BRANCHES: usize = 1_024;

type ReproducedWitness<D> = Result<
    Option<(<D as DomainDefinition>::Artifact, usize)>,
    SessionError<<D as DomainDefinition>::Error>,
>;

fn reproduce_consolidation_witness<D: DomainDefinition>(
    domain: &D,
    roots: &[VerifiedArtifact<D>],
    ledger: &ExperienceLedger,
    obligation: &KnowledgeObligationWork,
    scratch: &mut <D::Operators as OperatorAlgebra<D>>::Scratch,
) -> ReproducedWitness<D> {
    let Some(target) = obligation
        .supporting_attempts()
        .iter()
        .find_map(|attempt| {
            ledger
                .entries()
                .iter()
                .find(|entry| entry.attempt_id == *attempt)
        })
        .filter(|entry| entry.verdict == ExperienceVerdict::Accepted)
    else {
        return Ok(None);
    };
    let Some(origin_index) = roots
        .iter()
        .position(|artifact| artifact.key() == target.origin_key)
    else {
        return Ok(None);
    };
    let mut current = Vec::<Candidate<D>>::new();
    for (step_index, step) in obligation.operator_steps().iter().enumerate() {
        let Some(descriptor) = domain
            .operators()
            .catalog()
            .iter()
            .find(|descriptor| descriptor.symbol().as_str().as_bytes() == step.as_ref())
        else {
            return Ok(None);
        };
        let stage_parents = if step_index == 0 {
            vec![roots[origin_index].artifact()]
        } else {
            current
                .iter()
                .map(|candidate| &candidate.artifact)
                .collect()
        };
        let locations = root_locations(domain, &stage_parents);
        let mut applications = Vec::new();
        let mut application_writer =
            ApplicationWriter::with_limit(&mut applications, MAX_CONSOLIDATION_WITNESS_BRANCHES);
        domain
            .operators()
            .enumerate_legal(
                OperatorEnumerationBatch::new(
                    &stage_parents,
                    &locations,
                    std::slice::from_ref(&descriptor.operator()),
                ),
                &mut application_writer,
                scratch,
            )
            .map_err(SessionError::Domain)?;
        if application_writer.overflowed() {
            return Ok(None);
        }
        let mut next = Vec::new();
        let mut candidate_writer = CandidateWriter::with_limit(&mut next, applications.len());
        domain
            .operators()
            .apply_batch(&applications, &mut candidate_writer, scratch)
            .map_err(SessionError::Domain)?;
        if candidate_writer.overflowed() || next.len() != applications.len() {
            return Err(SessionError::InvalidSeed);
        }
        current = next;
        if current.is_empty() {
            return Ok(None);
        }
    }
    let mut structure_scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    for candidate in current {
        let mut canonical = Vec::new();
        domain
            .structure()
            .encode_canonical(&candidate.artifact, &mut canonical, &mut structure_scratch)
            .map_err(SessionError::Domain)?;
        if canonical == target.canonical_candidate {
            return Ok(Some((candidate.artifact, origin_index)));
        }
    }
    Ok(None)
}

#[expect(
    clippy::too_many_arguments,
    reason = "Candidate ordering keeps the domain, legacy ranking, bounded Intelligence arena, controller allowance, and fate attribution explicit at one allocation boundary"
)]
fn order_by_learned_potential<D: DomainDefinition>(
    domain: &D,
    parent_ranks: &[usize],
    frontier: &[(VerifiedArtifact<D>, usize)],
    covered_claims: &[[u8; 32]],
    intelligence: &IntelligenceCore,
    intelligence_market: &mut MarketArena,
    intelligence_portfolio: &mut PortfolioBuffer,
    intelligence_allowance: ResourceVector,
    limit: usize,
    candidates: &mut Vec<ProposedCandidate<D>>,
    candidate_fates: &mut [CandidateFateObservation],
) -> Vec<ProposedCandidate<D>> {
    rank_all_candidates(parent_ranks, candidates, candidate_fates);
    let (mut origin_exploration, remaining) =
        partition_origin_exploration(frontier, covered_claims, std::mem::take(candidates), limit);
    let (mut derived_exploration, mut unprotected) = partition_derived_exploration(remaining);
    for candidate in &mut origin_exploration {
        candidate.allocation_queue = AllocationQueue::ProtectedOrigin;
    }
    for candidate in &mut derived_exploration {
        candidate.allocation_queue = AllocationQueue::ProtectedDerived;
    }
    let bootstrap_rank = |candidate: &ProposedCandidate<D>| {
        candidate
            .bootstrap_rank
            .value()
            .expect("all novel Candidates retain a Bootstrap rank")
    };
    sort_prefix_by(&mut derived_exploration, limit, |left, right| {
        bootstrap_rank(left).cmp(&bootstrap_rank(right))
    });
    unprotected.sort_unstable_by_key(bootstrap_rank);
    let mut bootstrap = (0..unprotected.len()).collect::<Vec<_>>();
    bootstrap.sort_unstable_by_key(|candidate| {
        unprotected[*candidate]
            .bootstrap_rank
            .value()
            .expect("all novel Candidates retain a Bootstrap rank")
    });
    let (intelligence_order, intelligence_sources, intelligence_receipts) =
        intelligence_candidate_order(
            domain,
            intelligence,
            intelligence_market,
            intelligence_portfolio,
            intelligence_allowance,
            parent_ranks,
            frontier,
            &unprotected,
            &bootstrap,
            None,
        );
    apply_operational_order(
        origin_exploration,
        derived_exploration,
        unprotected,
        &intelligence_order,
        &intelligence_sources,
        &intelligence_receipts,
        limit,
        candidates,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the private allocation adapter explicitly binds its domain-derived demands, reusable arenas, real controller allowance, and both fallback orderings"
)]
fn intelligence_candidate_order<D: DomainDefinition>(
    domain: &D,
    intelligence: &IntelligenceCore,
    market: &mut MarketArena,
    buffer: &mut PortfolioBuffer,
    allowance: ResourceVector,
    parent_ranks: &[usize],
    frontier: &[(VerifiedArtifact<D>, usize)],
    candidates: &[ProposedCandidate<D>],
    bootstrap: &[usize],
    learned: Option<&[usize]>,
) -> (
    Vec<usize>,
    Vec<AllocationQueue>,
    Vec<Option<InvestmentReceipt>>,
) {
    let limits = intelligence.limits();
    let market_count = bootstrap
        .len()
        .min(limits.maximum_opportunities)
        .min(limits.maximum_investments);
    market.clear();
    for (priority, candidate_index) in bootstrap.iter().take(market_count).enumerate() {
        let candidate = &candidates[*candidate_index];
        let demand = candidate_verification_demand(domain, candidate);
        let preference_priority = u32::try_from(
            *parent_ranks
                .get(candidate.candidate.source_index)
                .expect("every Candidate retains a Search Frontier parent preference"),
        )
        .expect("the bounded Search Frontier has fewer than u32::MAX parents");
        let opportunity = market
            .push_opportunity(
                OpportunitySpec::candidate_in_corpus(
                    Sha256::digest(&candidate.canonical_candidate).into(),
                    frontier[candidate.candidate.source_index]
                        .0
                        .inner
                        .claim_digest,
                    OpportunitySpec::candidate_schema(),
                    RoutingFamilyId::new(candidate.operator_digest),
                ),
                &candidate.features.0,
            )
            .expect("the bounded Candidate market fits its declared Intelligence limits");
        market
            .push_investment(InvestmentSpec::verify_candidate_with_preference(
                opportunity,
                demand.resident_bytes,
                demand.durable_bytes,
                preference_priority,
                u32::try_from(priority).expect("the Intelligence market is bounded below u32::MAX"),
            ))
            .expect("each Candidate contributes exactly one bounded Verify Investment");
    }
    buffer.clear();
    let portfolio = intelligence
        .allocate(
            MarketFrame::new(market, allowance)
                .at_epoch(candidates.first().map_or(0, |candidate| candidate.epoch)),
            market_count,
            buffer,
        )
        .expect("a valid private Intelligence checkpoint must allocate its bounded market");
    let allocations = portfolio
        .allocations()
        .iter()
        .zip(portfolio.receipts())
        .map(|(allocation, receipt)| {
            let priority = usize::try_from(allocation.bootstrap_priority())
                .expect("a Bootstrap priority was produced from a bounded usize");
            (bootstrap[priority], allocation.source(), *receipt)
        })
        .collect::<Vec<_>>();
    let allocation_sources = allocations
        .iter()
        .map(|(index, source, _)| (*index, *source))
        .collect::<Vec<_>>();
    let (order, sources) = merge_intelligence_allocations(bootstrap, learned, &allocation_sources);
    let mut receipts = vec![None; candidates.len()];
    for (index, allocation_source, receipt) in allocations {
        let was_executed_by_source = match allocation_source {
            AllocationSource::Bootstrap => sources[index] == AllocationQueue::Bootstrap,
            AllocationSource::Specialist(_) => sources[index] == AllocationQueue::Learned,
        };
        if was_executed_by_source {
            receipts[index] = Some(receipt);
        }
    }
    (order, sources, receipts)
}

fn candidate_verification_demand<D: DomainDefinition>(
    domain: &D,
    candidate: &ProposedCandidate<D>,
) -> CandidateVerificationDemand {
    let feature_bytes = std::mem::size_of_val(&candidate.features) as u64;
    let retained_without_features = (std::mem::size_of_val(candidate) as u64)
        .saturating_sub(feature_bytes)
        .saturating_add(
            domain
                .structure()
                .view(&candidate.candidate.artifact)
                .dynamic_resident_bytes(),
        );
    let resident_bytes = retained_without_features
        .saturating_add(feature_bytes)
        .saturating_add(candidate.canonical_candidate.capacity() as u64)
        .saturating_add(candidate.operator_symbol.capacity() as u64);
    let durable_bytes = encoded_entry_len(
        candidate.canonical_candidate.len(),
        candidate.operator_symbol.len(),
        candidate.candidate.proposal_provenance.is_some(),
    );
    CandidateVerificationDemand {
        resident_bytes,
        durable_bytes,
    }
}

/// Keeps one causally independent Bootstrap decision, admits every actual
/// specialist decision, then fills the remainder from the legacy cooperative
/// policy. Bootstrap fallback allocations emitted by `IntelligenceCore` are
/// deliberately ignored after the first: otherwise an empty ecology would
/// silently erase the active legacy model instead of remaining behaviorally
/// compatible while that model is migrated into a typed Specialist.
fn merge_intelligence_allocations(
    bootstrap: &[usize],
    learned: Option<&[usize]>,
    intelligence: &[(usize, AllocationSource)],
) -> (Vec<usize>, Vec<AllocationQueue>) {
    let candidate_count = bootstrap.len();
    let mut eligible = vec![false; candidate_count];
    for (index, _) in intelligence.iter().copied() {
        eligible[index] = true;
    }
    let baseline = learned.map_or_else(
        || {
            bootstrap
                .iter()
                .copied()
                .filter(|index| eligible[*index])
                .map(|index| crate::policy::CooperativeSelection {
                    index,
                    queue: AllocationQueue::Bootstrap,
                })
                .collect::<Vec<_>>()
        },
        |learned| {
            cooperative_ranked_selections(bootstrap, learned, bootstrap.len())
                .into_iter()
                .filter(|selection| eligible[selection.index])
                .collect()
        },
    );
    let mut selected = vec![false; candidate_count];
    let mut order = Vec::with_capacity(candidate_count);
    let mut sources = vec![AllocationQueue::Bootstrap; candidate_count];
    for (position, (index, source)) in intelligence.iter().copied().enumerate() {
        let keep = position == 0 && source == AllocationSource::Bootstrap
            || matches!(source, AllocationSource::Specialist(_));
        if keep && !std::mem::replace(&mut selected[index], true) {
            order.push(index);
            sources[index] = match source {
                AllocationSource::Bootstrap => AllocationQueue::Bootstrap,
                AllocationSource::Specialist(_) => AllocationQueue::Learned,
            };
        }
    }
    for selection in baseline {
        if !std::mem::replace(&mut selected[selection.index], true) {
            order.push(selection.index);
            sources[selection.index] = selection.queue;
        }
    }
    (order, sources)
}

fn rank_all_candidates<D: DomainDefinition>(
    parent_ranks: &[usize],
    candidates: &mut [ProposedCandidate<D>],
    candidate_fates: &mut [CandidateFateObservation],
) {
    let bootstrap_compare = |left: &ProposedCandidate<D>, right: &ProposedCandidate<D>| {
        parent_ranks[left.candidate.source_index]
            .cmp(&parent_ranks[right.candidate.source_index])
            .then_with(|| {
                left.operator_symbol
                    .cmp(&right.operator_symbol)
                    .then_with(|| {
                        left.candidate
                            .source_index
                            .cmp(&right.candidate.source_index)
                    })
            })
    };
    let mut global_bootstrap = (0..candidates.len()).collect::<Vec<_>>();
    global_bootstrap
        .sort_unstable_by(|left, right| bootstrap_compare(&candidates[*left], &candidates[*right]));
    for (rank, candidate) in global_bootstrap.into_iter().enumerate() {
        let rank = CandidateRank::present(
            u32::try_from(rank).expect("Candidate count is bounded below u32::MAX"),
        );
        candidates[candidate].bootstrap_rank = rank;
        candidate_fates[candidates[candidate].fate_index].bootstrap_rank = rank;
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the final allocation join keeps the three protected partitions, Intelligence attribution, cohort bound, and destination explicit"
)]
fn apply_operational_order<D: DomainDefinition>(
    origin: Vec<ProposedCandidate<D>>,
    derived: Vec<ProposedCandidate<D>>,
    unprotected: Vec<ProposedCandidate<D>>,
    intelligence_order: &[usize],
    intelligence_sources: &[AllocationQueue],
    intelligence_receipts: &[Option<InvestmentReceipt>],
    limit: usize,
    output: &mut Vec<ProposedCandidate<D>>,
) -> Vec<ProposedCandidate<D>> {
    let selections =
        operational_ranked_selections(origin.len(), derived.len(), intelligence_order, None, limit);
    let mut origin = origin.into_iter().map(Some).collect::<Vec<_>>();
    let mut derived = derived.into_iter().map(Some).collect::<Vec<_>>();
    let mut unprotected = unprotected.into_iter().map(Some).collect::<Vec<_>>();
    output.extend(selections.into_iter().map(|selection| {
        let candidate = match selection.partition {
            OperationalPartition::ProtectedOrigin => &mut origin[selection.index],
            OperationalPartition::ProtectedDerived => &mut derived[selection.index],
            OperationalPartition::Unprotected => &mut unprotected[selection.index],
        };
        let mut candidate = candidate
            .take()
            .expect("operational policy emits each Candidate once");
        candidate.allocation_queue = if selection.partition == OperationalPartition::Unprotected {
            intelligence_sources[selection.index]
        } else {
            selection.queue
        };
        if selection.partition == OperationalPartition::Unprotected {
            candidate.intelligence_receipt = intelligence_receipts[selection.index];
        }
        candidate
    }));
    origin
        .into_iter()
        .chain(derived)
        .chain(unprotected)
        .flatten()
        .collect()
}

fn partition_derived_exploration<D: DomainDefinition>(
    candidates: Vec<ProposedCandidate<D>>,
) -> (Vec<ProposedCandidate<D>>, Vec<ProposedCandidate<D>>) {
    candidates
        .into_iter()
        .partition(|candidate| candidate.protected_derived)
}

fn partition_origin_exploration<D: DomainDefinition>(
    frontier: &[(VerifiedArtifact<D>, usize)],
    covered_claims: &[[u8; 32]],
    candidates: Vec<ProposedCandidate<D>>,
    limit: usize,
) -> (Vec<ProposedCandidate<D>>, Vec<ProposedCandidate<D>>) {
    let origins = candidates
        .iter()
        .filter_map(|candidate| {
            frontier
                .get(candidate.candidate.source_index)
                .map(|(artifact, _)| artifact.inner.claim_digest)
        })
        .filter(|claim| covered_claims.binary_search(claim).is_err())
        .collect::<BTreeSet<_>>();
    let protected = protected_origin_keys(origins, limit);
    let mut selected: HashMap<[u8; 32], usize> = HashMap::with_capacity(protected.len());
    for (index, candidate) in candidates.iter().enumerate() {
        let Some(claim) = frontier
            .get(candidate.candidate.source_index)
            .map(|(artifact, _)| artifact.inner.claim_digest)
        else {
            continue;
        };
        if !protected.contains(&claim) {
            continue;
        }
        selected
            .entry(claim)
            .and_modify(|selected_index| {
                let selected_candidate = &candidates[*selected_index];
                if (candidate.protected_derived && !selected_candidate.protected_derived)
                    || (candidate.protected_derived == selected_candidate.protected_derived
                        && candidate.bootstrap_rank.value()
                            < selected_candidate.bootstrap_rank.value())
                {
                    *selected_index = index;
                }
            })
            .or_insert(index);
    }
    let selected = selected.into_values().collect::<HashSet<_>>();
    let mut exploration = Vec::with_capacity(protected.len());
    let mut unprotected = Vec::with_capacity(candidates.len().saturating_sub(protected.len()));
    for (index, candidate) in candidates.into_iter().enumerate() {
        if selected.contains(&index) {
            exploration.push(candidate);
        } else {
            unprotected.push(candidate);
        }
    }
    (exploration, unprotected)
}

fn protected_origin_keys<K: Copy + Eq + std::hash::Hash + Ord>(
    origins: BTreeSet<K>,
    limit: usize,
) -> HashSet<K> {
    if origins.len() <= 1 || origins.len() > limit {
        return HashSet::new();
    }
    origins.into_iter().collect()
}

fn sort_prefix_by<T>(
    values: &mut Vec<T>,
    limit: usize,
    mut compare: impl FnMut(&T, &T) -> Ordering,
) {
    if limit == 0 {
        values.clear();
    } else if values.len() <= limit {
        values.sort_unstable_by(compare);
    } else {
        let (prefix, _, _) = values.select_nth_unstable_by(limit, &mut compare);
        prefix.sort_unstable_by(compare);
        values.truncate(limit);
    }
}

fn claim_digest<D: DomainDefinition>(
    domain: &D,
    artifact: &VerifiedArtifact<D>,
) -> Result<[u8; 32], SessionError<D::Error>> {
    let mut encoded = Vec::new();
    domain
        .kernel()
        .encode_claim(&artifact.inner.verification.claim, &mut encoded)
        .map_err(SessionError::Domain)?;
    Ok(Sha256::digest(encoded).into())
}

fn retain_novel_candidates<D: DomainDefinition>(
    domain: &D,
    known: &[VerifiedArtifact<D>],
    experience: &[ExperienceEntry],
    roots: &[VerifiedArtifact<D>],
    frontier: &[(VerifiedArtifact<D>, usize)],
    candidates: Vec<ProposedCandidate<D>>,
) -> Result<NovelCandidateBatch<D>, SessionError<D::Error>> {
    let known_keys = known
        .iter()
        .map(|artifact| Ok((artifact.key(), claim_digest(domain, artifact)?)))
        .collect::<Result<HashSet<_>, SessionError<D::Error>>>()?;
    let mut generated_keys = HashSet::with_capacity(candidates.len());
    let root_claims = roots
        .iter()
        .map(|root| claim_digest(domain, root))
        .collect::<Result<Vec<_>, _>>()?;
    let mut scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    let identity = domain.semantic_identity();
    let mut retained = Vec::with_capacity(candidates.len());
    let mut fates = Vec::with_capacity(candidates.len());
    let mut fate_is_new_generation = Vec::with_capacity(candidates.len());
    for (generation_rank, mut candidate) in candidates.into_iter().enumerate() {
        let mut canonical = std::mem::take(&mut candidate.canonical_candidate);
        if candidate.generated_in_epoch && canonical.is_empty() {
            domain
                .structure()
                .encode_canonical(&candidate.candidate.artifact, &mut canonical, &mut scratch)
                .map_err(SessionError::Domain)?;
        }
        let candidate_key = ArtifactKey(stable_digest(identity.as_str(), &canonical));
        let origin = frontier[candidate.candidate.source_index].1;
        let claim_digest = root_claims[origin];
        let identity = (candidate_key, claim_digest);
        let prior_negative = experience.iter().any(|entry| {
            entry.candidate_key == candidate_key
                && entry.claim_digest == claim_digest
                && matches!(
                    entry.verdict,
                    ExperienceVerdict::Refuted | ExperienceVerdict::Unknown
                )
        });
        let disposition = if known_keys.contains(&identity) {
            CandidateFateDisposition::KnownArtifact
        } else if !generated_keys.insert(identity) {
            CandidateFateDisposition::DuplicateCandidate
        } else if prior_negative {
            CandidateFateDisposition::PriorNegativeExperience
        } else {
            CandidateFateDisposition::PolicyDeferred
        };
        let novel = disposition == CandidateFateDisposition::PolicyDeferred;
        let fate_index = fates.len();
        let operator_digest: [u8; 32] = Sha256::digest(&candidate.operator_symbol).into();
        candidate.operator_digest = operator_digest;
        fates.push(CandidateFateObservation {
            candidate_key,
            claim_digest,
            parent_key: frontier[candidate.candidate.source_index].0.key(),
            operator_digest,
            proposal_provenance: candidate.candidate.proposal_provenance,
            epoch: candidate.epoch,
            generation_rank: u32::try_from(generation_rank).unwrap_or(u32::MAX),
            proposal_limit: candidate.proposal_limit,
            policy_rank: CandidateRank::absent(),
            bootstrap_rank: CandidateRank::absent(),
            learned_rank: CandidateRank::absent(),
            verification_batch_cpu_ns: 0,
            verification_batch_size: 0,
            disposition,
            allocation_queue: None,
        });
        fate_is_new_generation.push(candidate.generated_in_epoch);
        if novel {
            candidate.canonical_candidate = canonical;
            candidate.fate_index = fate_index;
            retained.push(candidate);
        }
    }
    Ok(NovelCandidateBatch {
        candidates: retained,
        fates,
        fate_is_new_generation,
    })
}

fn candidate_canonical_materialization_bound<D: DomainDefinition>(
    domain: &D,
    candidates: &[ProposedCandidate<D>],
) -> Result<(u64, u64), SessionError<D::Error>> {
    let mut retained = 0_u64;
    let mut scratch = 0_u64;
    for candidate in candidates.iter().filter(|candidate| {
        candidate.generated_in_epoch && candidate.canonical_candidate.is_empty()
    }) {
        let contract = domain
            .structure()
            .canonical_encoding_contract(&candidate.candidate.artifact)
            .map_err(SessionError::Domain)?;
        retained = retained.saturating_add(
            u64::try_from(contract.encoded_bytes()).map_err(|_| SessionError::Resource)?,
        );
        scratch = scratch.max(contract.scratch_resident_bytes());
    }
    Ok((retained, scratch))
}

fn materialize_candidate_canonical<D: DomainDefinition>(
    domain: &D,
    candidates: &mut [ProposedCandidate<D>],
) -> Result<(), SessionError<D::Error>> {
    for candidate in candidates.iter_mut().filter(|candidate| {
        candidate.generated_in_epoch && candidate.canonical_candidate.is_empty()
    }) {
        let contract = domain
            .structure()
            .canonical_encoding_contract(&candidate.candidate.artifact)
            .map_err(SessionError::Domain)?;
        let mut canonical = exact_vec(contract.encoded_bytes())?;
        let mut scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
        domain
            .structure()
            .encode_canonical(&candidate.candidate.artifact, &mut canonical, &mut scratch)
            .map_err(SessionError::Domain)?;
        if canonical.len() != contract.encoded_bytes()
            || canonical.capacity() != contract.encoded_bytes()
            || domain.structure().scratch_dynamic_resident_bytes(&scratch)
                > contract.scratch_resident_bytes()
        {
            return Err(SessionError::InvalidSeed);
        }
        candidate.canonical_candidate = canonical;
    }
    Ok(())
}

fn retain_transaction_fates<D: DomainDefinition>(
    selected: &mut [ProposedCandidate<D>],
    fates: Vec<CandidateFateObservation>,
    mut retained: Vec<bool>,
) -> Vec<CandidateFateObservation> {
    assert_eq!(fates.len(), retained.len());
    for candidate in selected.iter() {
        retained[candidate.fate_index] = true;
    }
    let mut remapped = vec![usize::MAX; fates.len()];
    let mut compact = Vec::with_capacity(retained.iter().filter(|retain| **retain).count());
    for (old_index, (fate, retain)) in fates.into_iter().zip(retained).enumerate() {
        if retain {
            remapped[old_index] = compact.len();
            compact.push(fate);
        }
    }
    for candidate in selected {
        candidate.fate_index = remapped[candidate.fate_index];
    }
    compact
}

fn verified_artifact_bundle_record_len(
    artifact_bytes: usize,
    claim_bytes: usize,
    evidence_bytes: usize,
    has_parent: bool,
    provenance_bytes: usize,
) -> u64 {
    [
        artifact_bytes,
        claim_bytes,
        evidence_bytes,
        provenance_bytes,
    ]
    .into_iter()
    .fold(8_u64 + 32 + 1, |bytes, length| {
        bytes
            .saturating_add(8)
            .saturating_add(u64::try_from(length).unwrap_or(u64::MAX))
    })
    .saturating_add(u64::from(has_parent).saturating_mul(32))
}

struct ArtifactBundleRecordInput<'a> {
    artifact: &'a [u8],
    claim: &'a [u8],
    evidence: &'a [u8],
    kernel_revision: u64,
    origin_key: ArtifactKey,
    parent_key: Option<ArtifactKey>,
    provenance: &'a [u8],
}

fn artifact_bundle_record(input: &ArtifactBundleRecordInput<'_>) -> Result<Box<[u8]>, ()> {
    let encoded_len = verified_artifact_bundle_record_len(
        input.artifact.len(),
        input.claim.len(),
        input.evidence.len(),
        input.parent_key.is_some(),
        input.provenance.len(),
    );
    let mut record = Vec::with_capacity(usize::try_from(encoded_len).map_err(|_| ())?);
    push_bytes(&mut record, input.artifact);
    push_bytes(&mut record, input.claim);
    push_bytes(&mut record, input.evidence);
    push_u64(&mut record, input.kernel_revision);
    record.extend_from_slice(input.origin_key.as_bytes());
    match input.parent_key {
        Some(parent_key) => {
            record.push(1);
            record.extend_from_slice(parent_key.as_bytes());
        }
        None => record.push(0),
    }
    push_bytes(&mut record, input.provenance);
    debug_assert_eq!(record.len() as u64, encoded_len);
    Ok(record.into_boxed_slice())
}

#[expect(
    clippy::too_many_lines,
    reason = "materialization validates one atomic Domain allocation contract from admission through retained records"
)]
fn materialize<D: DomainDefinition>(
    domain: &D,
    identity: &SemanticIdentity,
    stored: Vec<StoredArtifact<D>>,
    environment: &crate::MeasurementEnvironment,
    resource_meter: &ResourceEnvelopeGuard,
    resident_overlap: u64,
) -> Result<Vec<VerifiedArtifact<D>>, SessionError<D::Error>> {
    let artifact_count = stored.len();
    let artifact_reference_bytes = allocation_bytes::<&D::Artifact, _>(artifact_count)?;
    if !resource_meter.reserve(
        ResidentReservation::live(resident_overlap).with_transient(artifact_reference_bytes),
    ) {
        return Err(SessionError::Resource);
    }
    let mut artifact_refs = exact_vec(artifact_count)?;
    artifact_refs.extend(stored.iter().map(|stored| &stored.artifact));
    let mut measurement_scratch = <D::Measurements as MeasurementSpace<D>>::Scratch::default();
    let schema = domain.measurements().schema();
    let expected_measurements = artifact_count
        .checked_mul(domain.measurements().schema().len())
        .ok_or(SessionError::Resource)?;
    let measured_bytes =
        allocation_bytes::<Measurement<D::Metric, D::Observation>, _>(expected_measurements)?;
    let measurement_outer_bytes =
        allocation_bytes::<Vec<Measurement<D::Metric, D::Observation>>, _>(artifact_count)?;
    let measurement_inner_bytes = measured_bytes;
    let measurement_scratch_bound = domain
        .measurements()
        .measurement_scratch_resident_bytes(&artifact_refs);
    let observation_dynamic_bound = stored.iter().fold(0_u64, |bytes, stored| {
        schema.iter().fold(bytes, |bytes, descriptor| {
            bytes.saturating_add(
                domain
                    .measurements()
                    .observation_dynamic_resident_bytes_bound(
                        &stored.artifact,
                        descriptor.metric(),
                    ),
            )
        })
    });
    let result_vector_bytes = allocation_bytes::<VerifiedArtifact<D>, _>(artifact_count)?;
    let measurement_peak = artifact_reference_bytes
        .saturating_add(measured_bytes)
        .saturating_add(measurement_outer_bytes)
        .saturating_add(measurement_inner_bytes)
        .saturating_add(measurement_scratch_bound)
        .saturating_add(observation_dynamic_bound);
    let retained_measurements = measurement_outer_bytes
        .saturating_add(measurement_inner_bytes)
        .saturating_add(observation_dynamic_bound)
        .saturating_add(result_vector_bytes);
    let mut retained_encoding = 0_u64;
    let mut encoding_peak = retained_measurements;
    for stored in &stored {
        let canonical = domain
            .structure()
            .canonical_encoding_contract(&stored.artifact)
            .map_err(SessionError::Domain)?;
        let claim = domain
            .kernel()
            .claim_encoding_contract(&stored.verification.claim)
            .map_err(SessionError::Domain)?;
        let evidence = domain
            .kernel()
            .evidence_encoding_contract(&stored.verification.evidence)
            .map_err(SessionError::Domain)?;
        let bundle = verified_artifact_bundle_record_len(
            canonical.encoded_bytes(),
            claim.encoded_bytes(),
            evidence.encoded_bytes(),
            stored.parent_key.is_some(),
            stored.provenance.len(),
        );
        let canonical_bytes = u64::try_from(canonical.encoded_bytes()).unwrap_or(u64::MAX);
        let claim_bytes = u64::try_from(claim.encoded_bytes()).unwrap_or(u64::MAX);
        let evidence_bytes = u64::try_from(evidence.encoded_bytes()).unwrap_or(u64::MAX);
        let arc_and_environment = arc_record_allocation_bytes::<D>()
            .saturating_add(u64::try_from(environment.identity().len()).unwrap_or(u64::MAX));
        let current_peak = canonical_bytes
            .saturating_add(canonical.scratch_resident_bytes())
            .max(
                canonical_bytes
                    .saturating_add(claim_bytes)
                    .saturating_add(claim.scratch_resident_bytes()),
            )
            .max(
                canonical_bytes
                    .saturating_add(claim_bytes)
                    .saturating_add(evidence_bytes)
                    .saturating_add(evidence.scratch_resident_bytes()),
            )
            .max(
                canonical_bytes
                    .saturating_add(claim_bytes)
                    .saturating_add(evidence_bytes)
                    .saturating_add(bundle)
                    .saturating_add(arc_and_environment),
            );
        let permanent = claim_bytes
            .saturating_add(bundle)
            .saturating_add(arc_and_environment);
        encoding_peak = encoding_peak.max(
            retained_measurements
                .saturating_add(retained_encoding)
                .saturating_add(current_peak),
        );
        retained_encoding = retained_encoding.saturating_add(permanent);
    }
    if !resource_meter.reserve(
        ResidentReservation::live(resident_overlap)
            .with_transient(measurement_peak.max(encoding_peak)),
    ) {
        return Err(SessionError::Resource);
    }
    let mut measured = exact_vec(expected_measurements)?;
    let mut writer = MeasurementWriter::with_limit(&mut measured, expected_measurements);
    domain
        .measurements()
        .measure_batch(
            VerifiedBatch::new(&artifact_refs),
            environment,
            &mut writer,
            &mut measurement_scratch,
        )
        .map_err(SessionError::Domain)?;
    if writer.overflowed() || measured.len() != expected_measurements {
        return Err(SessionError::InvalidSeed);
    }
    if domain
        .measurements()
        .scratch_dynamic_resident_bytes(&measurement_scratch)
        > measurement_scratch_bound
    {
        return Err(SessionError::InvalidSeed);
    }
    let mut by_artifact = exact_vec(artifact_count)?;
    for _ in 0..artifact_count {
        by_artifact.push(exact_vec(schema.len())?);
    }
    for measurement in measured {
        let Some(output) = by_artifact.get_mut(measurement.artifact_index) else {
            return Err(SessionError::InvalidSeed);
        };
        output.push(measurement);
    }
    if by_artifact.iter().any(|measurements| {
        schema.iter().any(|descriptor| {
            measurements
                .iter()
                .filter(|measurement| measurement.metric == descriptor.metric())
                .count()
                != 1
        })
    }) {
        return Err(SessionError::InvalidSeed);
    }
    for (artifact_index, measurements) in by_artifact.iter().enumerate() {
        for measurement in measurements {
            let actual = domain
                .measurements()
                .observation_dynamic_resident_bytes(measurement.metric, &measurement.observation);
            let bound = domain
                .measurements()
                .observation_dynamic_resident_bytes_bound(
                    &stored[artifact_index].artifact,
                    measurement.metric,
                );
            if !observation_resident_fits_bound(actual, bound) {
                return Err(SessionError::InvalidSeed);
            }
        }
    }
    drop(measurement_scratch);
    drop(artifact_refs);
    let mut materialized = exact_vec(artifact_count)?;
    for (stored, measurements) in stored.into_iter().zip(by_artifact) {
        let canonical_contract = domain
            .structure()
            .canonical_encoding_contract(&stored.artifact)
            .map_err(SessionError::Domain)?;
        let mut canonical = exact_vec(canonical_contract.encoded_bytes())?;
        let mut structure_scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
        domain
            .structure()
            .encode_canonical(&stored.artifact, &mut canonical, &mut structure_scratch)
            .map_err(SessionError::Domain)?;
        if canonical.len() != canonical_contract.encoded_bytes()
            || canonical.capacity() != canonical_contract.encoded_bytes()
            || domain
                .structure()
                .scratch_dynamic_resident_bytes(&structure_scratch)
                > canonical_contract.scratch_resident_bytes()
        {
            return Err(SessionError::InvalidSeed);
        }
        drop(structure_scratch);
        let key = ArtifactKey(stable_digest(identity.as_str(), &canonical));
        let claim_contract = domain
            .kernel()
            .claim_encoding_contract(&stored.verification.claim)
            .map_err(SessionError::Domain)?;
        let mut encoded_claim = exact_vec(claim_contract.encoded_bytes())?;
        domain
            .kernel()
            .encode_claim(&stored.verification.claim, &mut encoded_claim)
            .map_err(SessionError::Domain)?;
        if encoded_claim.len() != claim_contract.encoded_bytes()
            || encoded_claim.capacity() != claim_contract.encoded_bytes()
        {
            return Err(SessionError::InvalidSeed);
        }
        let claim_digest = Sha256::digest(&encoded_claim).into();
        let evidence_contract = domain
            .kernel()
            .evidence_encoding_contract(&stored.verification.evidence)
            .map_err(SessionError::Domain)?;
        let mut encoded_evidence = exact_vec(evidence_contract.encoded_bytes())?;
        domain
            .kernel()
            .encode_evidence(&stored.verification.evidence, &mut encoded_evidence)
            .map_err(SessionError::Domain)?;
        if encoded_evidence.len() != evidence_contract.encoded_bytes()
            || encoded_evidence.capacity() != evidence_contract.encoded_bytes()
        {
            return Err(SessionError::InvalidSeed);
        }
        let origin_key = stored.origin_key.unwrap_or(key);
        let bundle_record = artifact_bundle_record(&ArtifactBundleRecordInput {
            artifact: &canonical,
            claim: &encoded_claim,
            evidence: &encoded_evidence,
            kernel_revision: stored.verification.kernel_revision.0,
            origin_key,
            parent_key: stored.parent_key,
            provenance: &stored.provenance,
        })
        .map_err(|()| SessionError::Resource)?;
        let claim_canonical = encoded_claim.into_boxed_slice();
        let dynamic_resident_bytes = domain
            .structure()
            .artifact_dynamic_resident_bytes(&stored.artifact)
            .saturating_add(
                domain
                    .kernel()
                    .claim_dynamic_resident_bytes(&stored.verification.claim),
            )
            .saturating_add(
                domain
                    .kernel()
                    .evidence_dynamic_resident_bytes(&stored.verification.evidence),
            )
            .saturating_add(measurements.iter().fold(0_u64, |bytes, measurement| {
                bytes.saturating_add(domain.measurements().observation_dynamic_resident_bytes(
                    measurement.metric,
                    &measurement.observation,
                ))
            }));
        materialized.push(VerifiedArtifact {
            inner: Arc::new(VerifiedArtifactRecord {
                key,
                claim_digest,
                claim_canonical,
                artifact: stored.artifact,
                verification: stored.verification,
                origin_key,
                parent_key: stored.parent_key,
                measurements,
                environment: environment.clone(),
                provenance: stored.provenance,
                dynamic_resident_bytes,
                bundle_record,
            }),
        });
    }
    Ok(materialized)
}

const fn observation_resident_fits_bound(actual: u64, bound: u64) -> bool {
    actual <= bound
}

fn allocation_bytes<T, E>(capacity: usize) -> Result<u64, SessionError<E>> {
    capacity
        .checked_mul(std::mem::size_of::<T>())
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(SessionError::Resource)
}

fn exact_vec<T, E>(capacity: usize) -> Result<Vec<T>, SessionError<E>> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| SessionError::Resource)?;
    if std::mem::size_of::<T>() != 0 && values.capacity() != capacity {
        return Err(SessionError::Resource);
    }
    Ok(values)
}

const fn arc_record_allocation_bytes<D: DomainDefinition>() -> u64 {
    (std::mem::size_of::<VerifiedArtifactRecord<D>>() + 2 * std::mem::size_of::<usize>()) as u64
}

fn extend_unique<D: DomainDefinition>(
    known: &mut Vec<VerifiedArtifact<D>>,
    artifacts: impl IntoIterator<Item = VerifiedArtifact<D>>,
) {
    for artifact in artifacts {
        if !known.iter().any(|known| known.key() == artifact.key()) {
            known.push(artifact);
        }
    }
}

fn record_admission_consequences<D: DomainDefinition>(
    goals: &GoalEvaluator<'_, D>,
    known: &[VerifiedArtifact<D>],
    proposed_pareto: &[VerifiedArtifact<D>],
    previous_pareto_keys: &[ArtifactKey],
    experience: &[ExperienceEntry],
    admitted: &[(VerifiedArtifact<D>, [u8; 32])],
    consequences: &mut Vec<ConsequenceObservation>,
) {
    let per_goal = goals.frontiers(known);
    for (artifact, attempt_id) in admitted {
        append_unique_consequence(
            consequences,
            ConsequenceObservation {
                subject: *attempt_id,
                kind: ConsequenceKind::Admitted,
            },
        );
        if experience
            .iter()
            .any(|entry| entry.attempt_id == *attempt_id && entry.features.0[3].is_sign_positive())
        {
            append_unique_consequence(
                consequences,
                ConsequenceObservation {
                    subject: *attempt_id,
                    kind: ConsequenceKind::Compression,
                },
            );
        }
        if proposed_pareto
            .iter()
            .any(|candidate| candidate.key() == artifact.key())
            && !previous_pareto_keys.contains(&artifact.key())
        {
            append_unique_consequence(
                consequences,
                ConsequenceObservation {
                    subject: *attempt_id,
                    kind: ConsequenceKind::ParetoImprovement,
                },
            );
        }
        if per_goal
            .iter()
            .filter(|frontier| {
                frontier
                    .iter()
                    .any(|candidate| candidate.key() == artifact.key())
            })
            .count()
            > 1
        {
            append_unique_consequence(
                consequences,
                ConsequenceObservation {
                    subject: *attempt_id,
                    kind: ConsequenceKind::CrossGoalUse,
                },
            );
        }
    }
}

fn append_unique_consequence(
    consequences: &mut Vec<ConsequenceObservation>,
    consequence: ConsequenceObservation,
) {
    if !consequences.contains(&consequence) {
        consequences.push(consequence);
    }
}

#[derive(Clone, Copy)]
enum DeltaDelivery {
    NoChange,
    Delivered { stopped: bool },
    ResourceExhausted,
}

impl DeltaDelivery {
    fn stopped(self) -> bool {
        matches!(self, Self::Delivered { stopped: true })
    }

    fn resource_exhausted(self) -> bool {
        matches!(self, Self::ResourceExhausted)
    }
}

fn deliver_delta<D, O>(
    observer: &mut O,
    sequence: &mut u64,
    previous: &[ArtifactKey],
    current: &[VerifiedArtifact<D>],
    affected_goals: &[GoalId],
    resource_meter: &ResourceEnvelopeGuard,
    resident_state: u64,
) -> DeltaDelivery
where
    D: DomainDefinition,
    O: for<'a> FnMut(ParetoUpdate<'a, D>) -> ControlFlow<()>,
{
    let index_bytes = (previous.len() as u64)
        .saturating_add(current.len() as u64)
        .saturating_mul(128);
    if !resource_meter
        .reserve(ResidentReservation::live(resident_state).with_transient(index_bytes))
    {
        return DeltaDelivery::ResourceExhausted;
    }
    let previous_keys = previous.iter().copied().collect::<HashSet<_>>();
    let current_keys = current
        .iter()
        .map(VerifiedArtifact::key)
        .collect::<HashSet<_>>();
    let added = current
        .iter()
        .filter(|artifact| !previous_keys.contains(&artifact.key()))
        .cloned()
        .collect::<Vec<_>>();
    let removed = previous
        .iter()
        .filter(|key| !current_keys.contains(key))
        .copied()
        .collect::<Vec<_>>();
    if added.is_empty() && removed.is_empty() {
        return DeltaDelivery::NoChange;
    }
    let export_bytes = vector_bytes(&added)
        .saturating_add(vector_bytes(&removed))
        .saturating_add(std::mem::size_of_val(affected_goals) as u64)
        .saturating_add(index_bytes);
    if !resource_meter
        .reserve(ResidentReservation::live(resident_state).with_transient(export_bytes))
    {
        return DeltaDelivery::ResourceExhausted;
    }
    *sequence += 1;
    let stopped = observer(ParetoUpdate {
        sequence: *sequence,
        added: &added,
        removed: &removed,
        affected_goals,
    })
    .is_break();
    DeltaDelivery::Delivered { stopped }
}

#[expect(
    clippy::too_many_lines,
    reason = "the canonical v3 import keeps segment cross-validation in one audit path"
)]
fn decode_bundle<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    source: &std::path::Path,
    resource_meter: &ResourceEnvelopeGuard,
    resident_before_bundle: u64,
) -> Result<RecoveredBundle<D>, SessionError<D::Error>> {
    let mut source_file = std::fs::File::open(source).map_err(SessionError::Durability)?;
    let bundle_bytes = source_file
        .metadata()
        .map_err(SessionError::Durability)?
        .len();
    if !resource_meter.checkpoint_fits(bundle_bytes)
        || !resource_meter.reserve(
            ResidentReservation::live(resident_before_bundle)
                .with_transient(bundle_bytes.saturating_mul(6)),
        )
    {
        return Err(SessionError::Resource);
    }
    let bundle_len = usize::try_from(bundle_bytes).map_err(|_| SessionError::Resource)?;
    let bytes = read_fixed_source(&mut source_file, bundle_len)
        .map_err(|()| SessionError::CorruptBundle)?;
    let maximum_logical_bytes = resource_meter
        .available_resident(resident_before_bundle.saturating_add(bundle_bytes))
        .checked_div(BUNDLE_DECODE_RESIDENT_MULTIPLIER)
        .ok_or(SessionError::Resource)?;
    let decoded = CanonicalBundle::decode(&bytes, maximum_logical_bytes).map_err(|error| {
        if error.is_logical_size_limit() {
            SessionError::Resource
        } else {
            SessionError::CorruptBundle
        }
    })?;
    let decoded_resident_bytes = decoded
        .logical_bytes()
        .saturating_mul(BUNDLE_DECODE_RESIDENT_MULTIPLIER);
    if !resource_meter.reserve(
        ResidentReservation::live(resident_before_bundle.saturating_add(decoded_resident_bytes))
            .with_transient(bundle_bytes),
    ) {
        return Err(SessionError::Resource);
    }
    drop(bytes);
    if decoded.identity() != domain.semantic_identity().as_str().as_bytes() {
        return Err(SessionError::IncompatibleBundle);
    }
    let validated_session =
        validate_session(domain, request, decoded.segment(SegmentKind::Session))?;
    if validated_session.runtime_revision != LEGACY_RUNTIME_REVISION
        && validated_session.restart_state_root != Some(decoded.restart_state_root())
    {
        return Err(SessionError::CorruptBundle);
    }
    let expected_revisions_version = match validated_session.runtime_revision {
        LEGACY_RUNTIME_REVISION => LEGACY_REVISIONS_SEGMENT_VERSION,
        STANDALONE_POLICY_RUNTIME_REVISION => STANDALONE_POLICY_REVISIONS_SEGMENT_VERSION,
        STANDALONE_KNOWLEDGE_RUNTIME_REVISION => STANDALONE_KNOWLEDGE_REVISIONS_SEGMENT_VERSION,
        PRE_ACTION_PROVENANCE_RUNTIME_REVISION
        | ACTION_PROVENANCE_RUNTIME_REVISION
        | RUNTIME_REVISION => REVISIONS_SEGMENT_VERSION,
        _ => return Err(SessionError::IncompatibleBundle),
    };
    if decoded.segment_version(SegmentKind::Revisions) != expected_revisions_version {
        return Err(SessionError::CorruptBundle);
    }
    let encoded_revisions = decoded.segment(SegmentKind::Revisions);
    let revision_header_len: usize =
        if validated_session.runtime_revision == LEGACY_RUNTIME_REVISION {
            64
        } else {
            128
        };
    if encoded_revisions.len() < revision_header_len.saturating_add(16) {
        return Err(SessionError::CorruptBundle);
    }
    let revisions = RevisionIds {
        knowledge: encoded_revisions[..32]
            .try_into()
            .expect("Knowledge Revision ID is exactly 32 bytes"),
        model: encoded_revisions[32..64]
            .try_into()
            .expect("Model Revision ID is exactly 32 bytes"),
        runtime_policy: if revision_header_len == 128 {
            encoded_revisions[64..96]
                .try_into()
                .expect("Runtime Policy Revision ID is exactly 32 bytes")
        } else {
            [0; 32]
        },
        intelligence: if revision_header_len == 128 {
            encoded_revisions[96..128]
                .try_into()
                .expect("Intelligence Revision ID is exactly 32 bytes")
        } else {
            [0; 32]
        },
    };
    let mut encoded_learning = &encoded_revisions[revision_header_len..];
    let legacy_knowledge = if matches!(
        validated_session.runtime_revision,
        PRE_ACTION_PROVENANCE_RUNTIME_REVISION
            | ACTION_PROVENANCE_RUNTIME_REVISION
            | RUNTIME_REVISION
    ) {
        None
    } else {
        Some(
            KnowledgeState::decode(take_sized(&mut encoded_learning)?)
                .map_err(|()| SessionError::CorruptBundle)?,
        )
    };
    let legacy_learning = if matches!(
        validated_session.runtime_revision,
        STANDALONE_KNOWLEDGE_RUNTIME_REVISION
            | PRE_ACTION_PROVENANCE_RUNTIME_REVISION
            | ACTION_PROVENANCE_RUNTIME_REVISION
            | RUNTIME_REVISION
    ) {
        None
    } else {
        Some(
            LearningState::decode(take_sized(&mut encoded_learning)?)
                .map_err(|()| SessionError::CorruptBundle)?,
        )
    };
    let (legacy_runtime_policy, intelligence) = match validated_session.runtime_revision {
        LEGACY_RUNTIME_REVISION => {
            if !encoded_learning.is_empty() {
                return Err(SessionError::CorruptBundle);
            }
            (None, None)
        }
        STANDALONE_POLICY_RUNTIME_REVISION => {
            let policy = RuntimePolicyState::decode(take_sized(&mut encoded_learning)?)
                .map_err(|_| SessionError::CorruptBundle)?;
            let intelligence_bytes = take_sized(&mut encoded_learning)?;
            if intelligence_bytes.get(intelligence_bytes.len().saturating_sub(32)..)
                != Some(revisions.intelligence.as_slice())
            {
                return Err(SessionError::CorruptBundle);
            }
            let intelligence = IntelligenceCore::restore(intelligence_bytes)
                .map_err(|_| SessionError::CorruptBundle)?;
            if !encoded_learning.is_empty() {
                return Err(SessionError::CorruptBundle);
            }
            (Some(policy), Some(intelligence))
        }
        STANDALONE_KNOWLEDGE_RUNTIME_REVISION
        | PRE_ACTION_PROVENANCE_RUNTIME_REVISION
        | ACTION_PROVENANCE_RUNTIME_REVISION
        | RUNTIME_REVISION => {
            let intelligence_bytes = take_sized(&mut encoded_learning)?;
            if intelligence_bytes.get(intelligence_bytes.len().saturating_sub(32)..)
                != Some(revisions.intelligence.as_slice())
            {
                return Err(SessionError::CorruptBundle);
            }
            let intelligence = IntelligenceCore::restore(intelligence_bytes)
                .map_err(|_| SessionError::CorruptBundle)?;
            if !encoded_learning.is_empty() {
                return Err(SessionError::CorruptBundle);
            }
            (None, Some(intelligence))
        }
        _ => return Err(SessionError::IncompatibleBundle),
    };
    let core_owned_model = matches!(
        validated_session.runtime_revision,
        STANDALONE_KNOWLEDGE_RUNTIME_REVISION
            | PRE_ACTION_PROVENANCE_RUNTIME_REVISION
            | ACTION_PROVENANCE_RUNTIME_REVISION
            | RUNTIME_REVISION
    );
    if (!core_owned_model
        && legacy_learning.as_ref().is_none_or(|learning| {
            learning.revision_digest(domain.semantic_identity().as_str()) != revisions.model
        }))
        || (core_owned_model
            && intelligence
                .as_ref()
                .is_none_or(|core| core.model_ecology_identity() != revisions.model))
        || (validated_session.runtime_revision == STANDALONE_POLICY_RUNTIME_REVISION
            && legacy_runtime_policy
                .as_ref()
                .is_none_or(|policy| policy.identity() != revisions.runtime_policy))
        || (core_owned_model
            && intelligence.as_ref().is_none_or(|core| {
                core.runtime_policy_revision().identity() != revisions.runtime_policy
            }))
    {
        return Err(SessionError::CorruptBundle);
    }
    let pinned_knowledge = legacy_knowledge.as_ref().map_or_else(
        || {
            intelligence
                .as_ref()
                .expect("current bundles restore an authenticated Intelligence Core")
                .pinned_knowledge_revision()
        },
        KnowledgeState::pinned_revision,
    );
    let identity = domain.semantic_identity();
    let mut input = decoded.segment(SegmentKind::Artifacts);
    let count =
        usize::try_from(read_bundle_u64(&mut input)?).map_err(|_| SessionError::CorruptBundle)?;
    let minimum_record_bytes = 32_usize;
    if count > input.len().saturating_div(minimum_record_bytes) {
        return Err(SessionError::CorruptBundle);
    }

    let mut structure_scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    let mut recovered = Vec::with_capacity(count);
    let mut recovered_index = HashMap::with_capacity(count);
    let mut previous_key = None;
    for _ in 0..count {
        let canonical = take_sized(&mut input)?;
        let key = ArtifactKey(stable_digest(
            domain.semantic_identity().as_str(),
            canonical,
        ));
        if previous_key.is_some_and(|previous| previous >= key) {
            return Err(SessionError::CorruptBundle);
        }
        previous_key = Some(key);
        let artifact = domain
            .structure()
            .decode_canonical(canonical, &mut structure_scratch)
            .map_err(|_| SessionError::CorruptBundle)?;
        let claim = domain
            .kernel()
            .decode_claim(take_sized(&mut input)?)
            .map_err(|_| SessionError::CorruptBundle)?;
        let evidence = domain
            .kernel()
            .decode_evidence(take_sized(&mut input)?)
            .map_err(|_| SessionError::CorruptBundle)?;
        let kernel_revision = crate::KernelRevision(read_bundle_u64(&mut input)?);
        let origin_key = ArtifactKey(
            take_bundle(&mut input, 32)?
                .try_into()
                .expect("exactly 32 origin-key bytes were taken"),
        );
        let parent_key = match take_bundle(&mut input, 1)?[0] {
            0 => None,
            1 => Some(ArtifactKey(
                take_bundle(&mut input, 32)?
                    .try_into()
                    .expect("exactly 32 parent-key bytes were taken"),
            )),
            _ => return Err(SessionError::CorruptBundle),
        };
        let provenance = take_sized(&mut input)?.to_vec();
        if recovered_index.insert(key, recovered.len()).is_some() {
            return Err(SessionError::CorruptBundle);
        }
        recovered.push(StoredArtifact {
            artifact,
            verification: VerificationRecord {
                claim,
                evidence,
                kernel_revision,
            },
            provenance,
            origin_key: Some(origin_key),
            parent_key,
        });
    }
    if !input.is_empty() {
        return Err(SessionError::CorruptBundle);
    }

    let mut recovery = decoded.segment(SegmentKind::Recovery);
    let pareto_count = usize::try_from(read_bundle_u64(&mut recovery)?)
        .map_err(|_| SessionError::CorruptBundle)?;
    if pareto_count > recovery.len().saturating_div(32) {
        return Err(SessionError::CorruptBundle);
    }
    let mut pareto_keys = Vec::with_capacity(pareto_count);
    for _ in 0..pareto_count {
        let key = ArtifactKey(
            take_bundle(&mut recovery, 32)?
                .try_into()
                .expect("exactly 32 Pareto-key bytes were taken"),
        );
        if pareto_keys.contains(&key) {
            return Err(SessionError::CorruptBundle);
        }
        pareto_keys.push(key);
    }
    let frontier_count = usize::try_from(read_bundle_u64(&mut recovery)?)
        .map_err(|_| SessionError::CorruptBundle)?;
    if frontier_count > recovery.len().saturating_div(32) {
        return Err(SessionError::CorruptBundle);
    }
    let mut frontier_keys = Vec::with_capacity(frontier_count);
    for _ in 0..frontier_count {
        let key = ArtifactKey(
            take_bundle(&mut recovery, 32)?
                .try_into()
                .expect("exactly 32 Search Frontier key bytes were taken"),
        );
        if frontier_keys.contains(&key) {
            return Err(SessionError::CorruptBundle);
        }
        frontier_keys.push(key);
    }
    let deferred_count = usize::try_from(read_bundle_u64(&mut recovery)?)
        .map_err(|_| SessionError::CorruptBundle)?;
    if deferred_count > recovery.len().saturating_div(86) {
        return Err(SessionError::CorruptBundle);
    }
    let mut deferred_candidates = Vec::with_capacity(deferred_count);
    for _ in 0..deferred_count {
        let canonical_candidate = take_sized(&mut recovery)?.to_vec();
        let artifact = domain
            .structure()
            .decode_canonical(&canonical_candidate, &mut structure_scratch)
            .map_err(|_| SessionError::CorruptBundle)?;
        let mut canonical_round_trip = Vec::new();
        domain
            .structure()
            .encode_canonical(&artifact, &mut canonical_round_trip, &mut structure_scratch)
            .map_err(|_| SessionError::CorruptBundle)?;
        if canonical_round_trip != canonical_candidate {
            return Err(SessionError::CorruptBundle);
        }
        let parent_key = ArtifactKey(
            take_bundle(&mut recovery, 32)?
                .try_into()
                .expect("exactly 32 parent-key bytes were taken"),
        );
        let operator_symbol = take_sized(&mut recovery)?.to_vec();
        let proposal_limit = u32::from_le_bytes(
            take_bundle(&mut recovery, 4)?
                .try_into()
                .expect("exactly four proposal-limit bytes were taken"),
        );
        let protected_derived = match take_bundle(&mut recovery, 1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(SessionError::CorruptBundle),
        };
        let mut proposal_features = ProposalFeatures::default().as_array();
        for feature in &mut proposal_features {
            *feature = f32::from_bits(u32::from_le_bytes(
                take_bundle(&mut recovery, 4)?
                    .try_into()
                    .expect("exactly four proposal-feature bytes were taken"),
            ));
        }
        let proposal_provenance = match take_bundle(&mut recovery, 1)?[0] {
            0 => None,
            1 => Some(ProposalProvenance::new(
                take_bundle(&mut recovery, 32)?
                    .try_into()
                    .expect("exactly 32 proposal-provenance bytes were taken"),
            )),
            _ => return Err(SessionError::CorruptBundle),
        };
        if proposal_limit == 0
            || operator_symbol.is_empty()
            || std::str::from_utf8(&operator_symbol).is_err()
            || proposal_features.iter().any(|feature| !feature.is_finite())
        {
            return Err(SessionError::CorruptBundle);
        }
        deferred_candidates.push(DeferredCandidate {
            artifact,
            canonical_candidate,
            parent_key,
            operator_symbol,
            proposal_features: ProposalFeatures::new(proposal_features),
            proposal_provenance,
            proposal_limit,
            protected_derived,
            action_decision: None,
            causal_parent_key: None,
        });
    }
    let pending_count = usize::try_from(read_bundle_u64(&mut recovery)?)
        .map_err(|_| SessionError::CorruptBundle)?;
    if pending_count > recovery.len().saturating_div(41) {
        return Err(SessionError::CorruptBundle);
    }
    let primitive_operator_count = domain.operators().catalog().len();
    let mut pending_parents = Vec::with_capacity(pending_count);
    for _ in 0..pending_count {
        let key = ArtifactKey(
            take_bundle(&mut recovery, 32)?
                .try_into()
                .expect("exactly 32 pending-parent-key bytes were taken"),
        );
        if pending_parents
            .iter()
            .any(|pending: &PendingParent| pending.key == key)
        {
            return Err(SessionError::CorruptBundle);
        }
        let offset_count = usize::try_from(read_bundle_u64(&mut recovery)?)
            .map_err(|_| SessionError::CorruptBundle)?;
        if offset_count != primitive_operator_count
            || offset_count > recovery.len().saturating_div(8)
        {
            return Err(SessionError::CorruptBundle);
        }
        let mut primitive_offsets = Vec::with_capacity(offset_count);
        for _ in 0..offset_count {
            let offset = read_bundle_u64(&mut recovery)?;
            if offset != ENUMERATION_COMPLETE && usize::try_from(offset).is_err() {
                return Err(SessionError::CorruptBundle);
            }
            primitive_offsets.push(offset);
        }
        let derived_sampled = match take_bundle(&mut recovery, 1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(SessionError::CorruptBundle),
        };
        let pending = PendingParent {
            key,
            primitive_offsets,
            derived_sampled,
        };
        if pending.complete() {
            return Err(SessionError::CorruptBundle);
        }
        pending_parents.push(pending);
    }
    if matches!(
        validated_session.runtime_revision,
        ACTION_PROVENANCE_RUNTIME_REVISION | RUNTIME_REVISION
    ) {
        let provenance_count = usize::try_from(read_bundle_u64(&mut recovery)?)
            .map_err(|_| SessionError::CorruptBundle)?;
        if provenance_count != deferred_candidates.len() {
            return Err(SessionError::CorruptBundle);
        }
        for deferred in &mut deferred_candidates {
            deferred.action_decision = match take_bundle(&mut recovery, 1)?[0] {
                0 => None,
                1 => Some(DecisionId::from_identity(
                    take_bundle(&mut recovery, 32)?
                        .try_into()
                        .expect("exactly 32 action-decision bytes were taken"),
                )),
                _ => return Err(SessionError::CorruptBundle),
            };
            deferred.causal_parent_key = match take_bundle(&mut recovery, 1)?[0] {
                0 => None,
                1 => Some(ArtifactKey(
                    take_bundle(&mut recovery, 32)?
                        .try_into()
                        .expect("exactly 32 causal-parent bytes were taken"),
                )),
                _ => return Err(SessionError::CorruptBundle),
            };
        }
    }
    let generation_complete = if validated_session.runtime_revision == RUNTIME_REVISION {
        match take_bundle(&mut recovery, 1)?[0] {
            0 => false,
            1 => true,
            _ => return Err(SessionError::CorruptBundle),
        }
    } else {
        false
    };
    if !recovery.is_empty() {
        return Err(SessionError::CorruptBundle);
    }
    let encoded_experience = decoded.segment(SegmentKind::Experience);
    let ledger = match validated_session.runtime_revision {
        LEGACY_RUNTIME_REVISION => ExperienceLedger::decode_legacy_v20(encoded_experience),
        STANDALONE_POLICY_RUNTIME_REVISION
        | STANDALONE_KNOWLEDGE_RUNTIME_REVISION
        | PRE_ACTION_PROVENANCE_RUNTIME_REVISION => {
            ExperienceLedger::decode_pre_action_v23(encoded_experience)
        }
        _ => ExperienceLedger::decode(encoded_experience),
    }
    .map_err(|()| SessionError::CorruptBundle)?;
    for (entry_index, entry) in ledger.entries().iter().enumerate() {
        if ArtifactKey(stable_digest(identity.as_str(), &entry.canonical_candidate))
            != entry.candidate_key
        {
            return Err(SessionError::CorruptBundle);
        }
        let candidate_artifact = domain
            .structure()
            .decode_canonical(&entry.canonical_candidate, &mut structure_scratch)
            .map_err(|_| SessionError::CorruptBundle)?;
        let operator = std::str::from_utf8(&entry.operator_symbol)
            .ok()
            .filter(|operator| !operator.is_empty())
            .ok_or(SessionError::CorruptBundle)?;
        if !domain
            .operators()
            .catalog()
            .iter()
            .any(|descriptor| descriptor.symbol().as_str() == operator)
            && pinned_knowledge
                .resolve_operator(&entry.operator_symbol)
                .is_none()
        {
            return Err(SessionError::CorruptBundle);
        }
        let Some(origin_index) = recovered_index.get(&entry.origin_key).copied() else {
            return Err(SessionError::CorruptBundle);
        };
        let Some(parent_index) = recovered_index.get(&entry.parent_key).copied() else {
            return Err(SessionError::CorruptBundle);
        };
        if let Some(causal_parent) = entry.causal_parent_key {
            let valid_repair_parent = ledger.entries()[..entry_index].iter().any(|parent| {
                parent.candidate_key == causal_parent
                    && parent.origin_key == entry.origin_key
                    && parent.claim_digest == entry.claim_digest
                    && parent.verdict == ExperienceVerdict::Refuted
                    && parent.rejection_advisory.is_some()
            });
            if !valid_repair_parent {
                return Err(SessionError::CorruptBundle);
            }
        }
        let origin = &recovered[origin_index];
        let mut encoded_claim = Vec::new();
        domain
            .kernel()
            .encode_claim(&origin.verification.claim, &mut encoded_claim)
            .map_err(SessionError::Domain)?;
        if <[u8; 32]>::from(Sha256::digest(encoded_claim)) != entry.claim_digest
            || entry.verification_requests != 1
            || attempt_digest(
                entry.candidate_key,
                entry.origin_key,
                entry.parent_key,
                &entry.operator_symbol,
                entry.epoch,
                entry.proposal_features,
                entry.proposal_provenance,
            ) != entry.attempt_id
            || opportunity_features(
                domain,
                StructuralSummary {
                    node_count: structural_node_count(domain, &recovered[parent_index].artifact),
                },
                &candidate_artifact,
                operator_feature_values(operator),
                entry.epoch,
                entry.proposal_features,
            ) != entry.features
            || ledger.entries()[..entry_index]
                .iter()
                .any(|known| known.attempt_id == entry.attempt_id)
        {
            return Err(SessionError::CorruptBundle);
        }
    }
    for (consequence_index, consequence) in ledger.consequences().iter().enumerate() {
        if !ledger
            .entries()
            .iter()
            .any(|entry| entry.attempt_id == consequence.subject)
            || ledger.consequences()[..consequence_index].contains(consequence)
        {
            return Err(SessionError::CorruptBundle);
        }
    }
    for (measurement_index, measurement) in ledger.measurements().iter().enumerate() {
        if !ledger.entries().iter().any(|entry| {
            entry.attempt_id == measurement.subject && entry.verdict == ExperienceVerdict::Accepted
        }) || ledger.measurements()[..measurement_index]
            .iter()
            .any(|known| known.subject == measurement.subject)
        {
            return Err(SessionError::CorruptBundle);
        }
        if measurement.environment.is_empty()
            || std::str::from_utf8(&measurement.environment).is_err()
        {
            return Err(SessionError::CorruptBundle);
        }
        if measurement.values.is_empty()
            || measurement.values.len() > domain.measurements().schema().len()
        {
            return Err(SessionError::CorruptBundle);
        }
        for (value_index, value) in measurement.values.iter().enumerate() {
            let Some(descriptor) =
                domain.measurements().schema().iter().find(|descriptor| {
                    descriptor.symbol().as_str().as_bytes() == value.metric_symbol
                })
            else {
                return Err(SessionError::CorruptBundle);
            };
            if measurement.values[..value_index]
                .iter()
                .any(|known| known.metric_symbol == value.metric_symbol)
                || domain
                    .measurements()
                    .decode_observation(descriptor.metric(), &value.observation)
                    .is_err()
            {
                return Err(SessionError::CorruptBundle);
            }
        }
    }
    let mut entries_by_fate = HashMap::with_capacity(ledger.entries().len());
    for entry in ledger.entries() {
        if entries_by_fate
            .insert(entry.candidate_fate_key(), entry)
            .is_some()
        {
            return Err(SessionError::CorruptBundle);
        }
    }
    let mut matched_attempts = HashSet::with_capacity(ledger.entries().len());
    if !candidate_fate_batches_are_valid(ledger.candidate_fates()) {
        return Err(SessionError::CorruptBundle);
    }
    for fate in ledger.candidate_fates() {
        if !fate.attribution_is_valid() {
            return Err(SessionError::CorruptBundle);
        }
        if let Some(expected_verdict) = fate.expected_verdict() {
            let Some(entry) = entries_by_fate.get(&fate.key()) else {
                return Err(SessionError::CorruptBundle);
            };
            if entry.verdict != expected_verdict
                || Some(entry.allocation_queue) != fate.allocation_queue
                || !matched_attempts.insert(entry.attempt_id)
            {
                return Err(SessionError::CorruptBundle);
            }
        }
    }
    if !ledger.candidate_fates().is_empty() && matched_attempts.len() != ledger.entries().len() {
        return Err(SessionError::CorruptBundle);
    }
    let corpus_assignments = ledger
        .entries()
        .iter()
        .map(|entry| (entry.attempt_id, entry.claim_digest))
        .collect::<Vec<_>>();
    let artifact_keys = recovered_index
        .keys()
        .map(|key| key.0)
        .collect::<BTreeSet<_>>();
    if frontier_keys
        .iter()
        .any(|key| !recovered_index.contains_key(key))
    {
        return Err(SessionError::CorruptBundle);
    }
    let primitive_symbols = domain
        .operators()
        .catalog()
        .iter()
        .map(|descriptor| descriptor.symbol().as_str().as_bytes().to_vec())
        .collect::<BTreeSet<_>>();
    let derivations = ledger.derivations(pinned_knowledge, &primitive_symbols)?;
    if legacy_learning
        .as_ref()
        .is_some_and(|learning| !learning.corpus_is_valid(&corpus_assignments))
        || legacy_knowledge.as_ref().is_some_and(|knowledge| {
            !knowledge.validate(&artifact_keys, &derivations, &primitive_symbols)
        })
        || legacy_knowledge.is_none()
            && intelligence.as_ref().is_none_or(|core| {
                !core.validate_active_knowledge(&artifact_keys, &derivations, &primitive_symbols)
            })
        || deferred_candidates.iter().any(|candidate| {
            candidate.action_decision.is_some_and(|decision| {
                intelligence
                    .as_ref()
                    .is_none_or(|core| !core.completed_candidate_production(decision))
            })
        })
    {
        return Err(SessionError::CorruptBundle);
    }
    Ok(RecoveredBundle {
        artifacts: recovered,
        pareto_keys,
        frontier_keys,
        deferred_candidates,
        pending_parents,
        generation_complete,
        ledger,
        revisions: Some(revisions),
        interrupted_usage: validated_session.interrupted_usage,
        legacy_knowledge,
        legacy_learning,
        legacy_runtime_policy,
        authenticated_intelligence_identity: intelligence.as_ref().map(|_| revisions.intelligence),
        intelligence,
        resident_bytes: decoded_resident_bytes,
    })
}

struct ValidatedSession {
    interrupted_usage: Option<ResourceUsage>,
    runtime_revision: u64,
    restart_state_root: Option<[u8; 32]>,
}

fn validate_session<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    input: &[u8],
) -> Result<ValidatedSession, SessionError<D::Error>> {
    let decoded = decode_session_segment(input)?;
    let scope = domain
        .seeds()
        .decode_scope(decoded.encoded_scope)
        .map_err(|_| SessionError::CorruptBundle)?;
    let mut canonical_scope = Vec::new();
    domain
        .seeds()
        .encode_scope(&scope, &mut canonical_scope)
        .map_err(|_| SessionError::CorruptBundle)?;
    if canonical_scope != decoded.encoded_scope {
        return Err(SessionError::CorruptBundle);
    }
    let cursor = domain
        .seeds()
        .decode_cursor(decoded.encoded_cursor)
        .map_err(|_| SessionError::CorruptBundle)?;
    let mut canonical_cursor = Vec::new();
    domain
        .seeds()
        .encode_cursor(&cursor, &mut canonical_cursor)
        .map_err(|_| SessionError::CorruptBundle)?;
    if canonical_cursor != decoded.encoded_cursor {
        return Err(SessionError::CorruptBundle);
    }
    if decoded.kernel_revision != domain.kernel().revision().0 {
        return Err(SessionError::IncompatibleBundle);
    }
    if decoded.runtime_revision == LEGACY_RUNTIME_REVISION && !request.bundle.forks() {
        return Err(SessionError::IncompatibleBundle);
    }
    if !decoded.completed {
        let restart_state_root = decoded
            .restart_state_root
            .ok_or(SessionError::CorruptBundle)?;
        let expected = encode_session(
            domain,
            request,
            decoded.encoded_cursor,
            restart_state_root,
            SessionSeal::Interrupted(decoded.usage),
        )?;
        if expected.get(..decoded.compatibility_prefix_len)
            != input.get(..decoded.compatibility_prefix_len)
        {
            return Err(SessionError::IncompatibleBundle);
        }
    }
    Ok(ValidatedSession {
        interrupted_usage: (!decoded.completed).then_some(decoded.usage),
        runtime_revision: decoded.runtime_revision,
        restart_state_root: decoded.restart_state_root,
    })
}

fn finish_scheduled_with_report<D: DomainDefinition, T>(
    result: Result<(T, VerificationBatchReport), ScheduleError<D::Error>>,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    allowance: crate::VerificationAllowance,
    resident_overlap: u64,
    corrupt_contract: bool,
) -> Result<(T, VerificationBatchReport), SessionError<D::Error>> {
    let charge = |report: VerificationBatchReport| {
        resource_meter
            .charge_external_verification(
                requirements,
                allowance,
                report.external_usage(),
                resident_overlap,
            )
            .map_err(|()| SessionError::Resource)?;
        if report.worker_failed() {
            Err(SessionError::VerificationWorker)
        } else {
            Ok(())
        }
    };
    match result {
        Ok((value, report)) => {
            charge(report)?;
            Ok((value, report))
        }
        Err(ScheduleError::Domain { error, report }) => {
            charge(report)?;
            Err(SessionError::Domain(error))
        }
        Err(ScheduleError::Contract { report }) => {
            charge(report)?;
            Err(if corrupt_contract {
                SessionError::CorruptBundle
            } else {
                SessionError::InvalidSeed
            })
        }
    }
}

fn finish_scheduled<D: DomainDefinition, T>(
    result: Result<(T, VerificationBatchReport), ScheduleError<D::Error>>,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    allowance: crate::VerificationAllowance,
    resident_overlap: u64,
    corrupt_contract: bool,
) -> Result<T, SessionError<D::Error>> {
    finish_scheduled_with_report::<D, _>(
        result,
        resource_meter,
        requirements,
        allowance,
        resident_overlap,
        corrupt_contract,
    )
    .map(|(value, _)| value)
}

fn charged_external_interruption<E>(
    error: &SessionError<E>,
    requirements: VerificationWorkerRequirements,
) -> bool {
    requirements.worker_lanes() != 0
        && matches!(
            error,
            SessionError::VerificationWorker | SessionError::Resource
        )
}

#[expect(
    clippy::too_many_arguments,
    reason = "the Kernel dispatch boundary explicitly binds scheduler, meter, worker contract, resident overlap, contract authority, and child allowance"
)]
fn scheduled_verify<D: DomainDefinition>(
    domain: &D,
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
    requests: &[ClaimVerificationRequest<'_, D>],
    corrupt_contract: bool,
    child_allowance: Option<VerificationAllowance>,
) -> Result<ClaimedVerdicts<D>, SessionError<D::Error>> {
    scheduled_verify_with_report(
        domain,
        scheduler,
        resource_meter,
        requirements,
        resident_overlap,
        requests,
        corrupt_contract,
        child_allowance,
    )
    .map(|(verdicts, _)| verdicts)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the reported Kernel dispatch boundary additionally returns authoritative batch economics"
)]
fn scheduled_verify_with_report<D: DomainDefinition>(
    domain: &D,
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
    requests: &[ClaimVerificationRequest<'_, D>],
    corrupt_contract: bool,
    child_allowance: Option<VerificationAllowance>,
) -> Result<(ClaimedVerdicts<D>, VerificationBatchReport), SessionError<D::Error>> {
    let lanes = if requirements.worker_lanes() == 0 {
        scheduler.lanes()
    } else {
        requirements.worker_lanes()
    };
    let global_allowance = resource_meter
        .verification_allowance(
            lanes,
            resident_overlap.saturating_sub(requirements.resident_bytes()),
        )
        .map_err(|()| SessionError::Resource)?;
    let allowance = child_allowance.map_or(global_allowance, |child| {
        VerificationAllowance::new(
            global_allowance.worker_lanes().min(child.worker_lanes()),
            global_allowance
                .resident_bytes()
                .min(child.resident_bytes()),
            global_allowance.elapsed_time().min(child.elapsed_time()),
            global_allowance.cpu_time().min(child.cpu_time()),
        )
    });
    finish_scheduled_with_report::<D, _>(
        scheduler.claim_and_verify(domain, requests, allowance, requirements),
        resource_meter,
        requirements,
        allowance,
        resident_overlap,
        corrupt_contract,
    )
}

fn scheduled_replay<D: DomainDefinition>(
    domain: &D,
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
    requests: &[VerificationReplayRequest<'_, D, ClaimOf<D>, EvidenceOf<D>>],
    corrupt_contract: bool,
) -> Result<Vec<bool>, SessionError<D::Error>> {
    let lanes = if requirements.worker_lanes() == 0 {
        scheduler.lanes()
    } else {
        requirements.worker_lanes()
    };
    let allowance = resource_meter
        .verification_allowance(
            lanes,
            resident_overlap.saturating_sub(requirements.resident_bytes()),
        )
        .map_err(|()| SessionError::Resource)?;
    finish_scheduled::<D, _>(
        scheduler.replay(domain, requests, allowance, requirements),
        resource_meter,
        requirements,
        allowance,
        resident_overlap,
        corrupt_contract,
    )
}

fn replay_stored<D: DomainDefinition>(
    domain: &D,
    stored: &[StoredArtifact<D>],
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
) -> Result<(), SessionError<D::Error>> {
    let replay_requests = stored
        .iter()
        .map(|stored| VerificationReplayRequest {
            artifact: &stored.artifact,
            claim: &stored.verification.claim,
            evidence: &stored.verification.evidence,
            kernel_revision: stored.verification.kernel_revision,
        })
        .collect::<Vec<_>>();
    let replayed = scheduled_replay(
        domain,
        scheduler,
        resource_meter,
        requirements,
        resident_overlap,
        &replay_requests,
        true,
    )?;
    if replayed.len() != stored.len() || replayed.iter().any(|accepted| !accepted) {
        return Err(SessionError::CorruptBundle);
    }
    Ok(())
}

fn replay_accepted_experience<D: DomainDefinition>(
    domain: &D,
    known: &[VerifiedArtifact<D>],
    experience: &[ExperienceEntry],
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
) -> Result<(), SessionError<D::Error>> {
    let mut structure_scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    for entry in experience
        .iter()
        .filter(|entry| entry.verdict == ExperienceVerdict::Accepted)
    {
        let entries = std::slice::from_ref(entry);
        let candidates = entries
            .iter()
            .map(|entry| {
                domain
                    .structure()
                    .decode_canonical(&entry.canonical_candidate, &mut structure_scratch)
                    .map_err(|_| SessionError::CorruptBundle)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let seeds = entries
            .iter()
            .map(|entry| {
                known
                    .iter()
                    .find(|artifact| artifact.key() == entry.origin_key)
                    .map(VerifiedArtifact::artifact)
                    .ok_or(SessionError::CorruptBundle)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let requests = seeds
            .iter()
            .zip(&candidates)
            .map(|(seed, candidate)| ClaimVerificationRequest {
                seed: *seed,
                candidate,
            })
            .collect::<Vec<_>>();
        let claims_and_verdicts = scheduled_verify(
            domain,
            scheduler,
            resource_meter,
            requirements,
            resident_overlap,
            &requests,
            true,
            None,
        )?;
        for (entry, (claim, _, _)) in entries.iter().zip(&claims_and_verdicts) {
            let mut encoded = Vec::new();
            domain
                .kernel()
                .encode_claim(claim, &mut encoded)
                .map_err(SessionError::Domain)?;
            if <[u8; 32]>::from(Sha256::digest(encoded)) != entry.claim_digest {
                return Err(SessionError::CorruptBundle);
            }
        }
        if claims_and_verdicts.len() != entries.len()
            || entries
                .iter()
                .zip(claims_and_verdicts)
                .any(|(entry, (_, verdict, _))| {
                    !matches!(
                        (entry.verdict, verdict),
                        (ExperienceVerdict::Accepted, Verdict::Accepted { .. })
                            | (ExperienceVerdict::Refuted, Verdict::Refuted)
                            | (ExperienceVerdict::Unknown, Verdict::Unknown)
                    )
                })
        {
            return Err(SessionError::CorruptBundle);
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "Knowledge recovery explicitly binds reconstruction, scheduling, and Resource Envelope authorities"
)]
fn replay_knowledge_obligations<'a, D: DomainDefinition>(
    domain: &D,
    known: &[VerifiedArtifact<D>],
    ledger: &ExperienceLedger,
    obligations: impl ExactSizeIterator<Item = &'a KnowledgeObligationWork>,
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
) -> Result<(), SessionError<D::Error>> {
    let obligation_count = obligations.len();
    if obligation_count == 0 {
        return Ok(());
    }
    let roots = known
        .iter()
        .filter(|artifact| artifact.key() == artifact.inner.origin_key)
        .cloned()
        .collect::<Vec<_>>();
    let mut operator_scratch = <D::Operators as OperatorAlgebra<D>>::Scratch::default();
    let mut witnesses = Vec::with_capacity(obligation_count);
    for obligation in obligations {
        let Some((candidate, origin)) = reproduce_consolidation_witness(
            domain,
            &roots,
            ledger,
            obligation,
            &mut operator_scratch,
        )?
        else {
            return Err(SessionError::CorruptBundle);
        };
        witnesses.push((candidate, origin));
    }
    let requests = witnesses
        .iter()
        .map(|(candidate, origin)| ClaimVerificationRequest {
            seed: roots[*origin].artifact(),
            candidate,
        })
        .collect::<Vec<_>>();
    let verdicts = scheduled_verify(
        domain,
        scheduler,
        resource_meter,
        requirements,
        resident_overlap,
        &requests,
        true,
        None,
    )?;
    if verdicts.len() != obligation_count
        || verdicts
            .iter()
            .any(|(_, verdict, _)| !matches!(verdict, Verdict::Accepted { .. }))
    {
        return Err(SessionError::CorruptBundle);
    }
    Ok(())
}

/// Bounds the allocations made while reconstructing and replaying the
/// authenticated Knowledge recovery manifest. Reconstruction is sequential,
/// so only one bounded Operator frontier is live at a time; completed witness
/// artifacts remain live until the single indexed Kernel batch is dispatched.
fn knowledge_recovery_replay_resident_bytes<'a, D: DomainDefinition>(
    domain: &D,
    known: &[VerifiedArtifact<D>],
    ledger: &ExperienceLedger,
    obligations: impl ExactSizeIterator<Item = &'a KnowledgeObligationWork>,
) -> Result<u64, SessionError<D::Error>> {
    let obligation_count = obligations.len();
    if obligation_count == 0 {
        return Ok(0);
    }
    let count = u64::try_from(obligation_count).map_err(|_| SessionError::Resource)?;
    let root_count = u64::try_from(
        known
            .iter()
            .filter(|artifact| artifact.key() == artifact.inner.origin_key)
            .count(),
    )
    .map_err(|_| SessionError::Resource)?;

    let mut retained_witness_payload = 0_u64;
    let mut largest_branch_payload = MIN_CHOICE_RESIDENT_BYTES;
    for obligation in obligations {
        let target = obligation
            .supporting_attempts()
            .iter()
            .find_map(|attempt| {
                ledger
                    .entries()
                    .iter()
                    .find(|entry| entry.attempt_id == *attempt)
            })
            .filter(|entry| entry.verdict == ExperienceVerdict::Accepted)
            .ok_or(SessionError::CorruptBundle)?;
        // The canonical candidate is the only pre-reconstruction payload
        // authority available to Runtime. The factor covers the materialized
        // artifact plus its transient canonical/provenance representation.
        let payload = u64::try_from(target.canonical_candidate.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(2)
            .max(MIN_CHOICE_RESIDENT_BYTES);
        retained_witness_payload = retained_witness_payload.saturating_add(payload);
        largest_branch_payload = largest_branch_payload.max(payload);
    }

    let roots = root_count.saturating_mul(std::mem::size_of::<VerifiedArtifact<D>>() as u64);
    let operator_frontier = (MAX_CONSOLIDATION_WITNESS_BRANCHES as u64)
        .saturating_mul(
            (std::mem::size_of::<<D::Operators as OperatorAlgebra<D>>::Application>() as u64)
                .saturating_add(2 * std::mem::size_of::<Candidate<D>>() as u64)
                .saturating_add(std::mem::size_of::<&D::Artifact>() as u64)
                .saturating_add(std::mem::size_of::<StructuralLocation>() as u64)
                .saturating_add(largest_branch_payload.saturating_mul(2)),
        )
        .saturating_add(
            domain
                .operators()
                .scratch_resident_bytes(MAX_CONSOLIDATION_WITNESS_BRANCHES),
        );
    let retained_witnesses = count
        .saturating_mul((std::mem::size_of::<D::Artifact>() + std::mem::size_of::<usize>()) as u64)
        .saturating_add(retained_witness_payload);
    let requests = count.saturating_mul(
        (std::mem::size_of::<ClaimVerificationRequest<'static, D>>()
            + std::mem::size_of::<crate::domain::VerificationRequest<'static, D, ClaimOf<D>>>())
            as u64,
    );
    // Scheduler owns claims, verdicts, advisories, ordered chunk results, and
    // the final aligned tuple concurrently at the hand-off boundary. The
    // duplicated factor also covers heap-backed claim/evidence payloads using
    // the same per-witness canonical payload authority above.
    let scheduler = count
        .saturating_mul(
            (2 * std::mem::size_of::<ClaimOf<D>>()
                + 2 * std::mem::size_of::<Verdict<EvidenceOf<D>>>()
                + 2 * std::mem::size_of::<Option<RejectionAdvisory>>()) as u64,
        )
        .saturating_add(retained_witness_payload.saturating_mul(2));

    Ok(roots
        .saturating_add(operator_frontier)
        .saturating_add(retained_witnesses)
        .saturating_add(requests)
        .saturating_add(scheduler))
}

fn take_bundle<'a, E>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], SessionError<E>> {
    if input.len() < count {
        return Err(SessionError::CorruptBundle);
    }
    let (value, remainder) = input.split_at(count);
    *input = remainder;
    Ok(value)
}

fn read_bundle_u64<E>(input: &mut &[u8]) -> Result<u64, SessionError<E>> {
    let bytes = take_bundle(input, 8)?;
    Ok(u64::from_le_bytes(
        bytes.try_into().expect("exactly eight bytes were taken"),
    ))
}

fn read_bundle_duration<E>(input: &mut &[u8]) -> Result<std::time::Duration, SessionError<E>> {
    let seconds = read_bundle_u64(input)?;
    let nanoseconds = u32::from_le_bytes(
        take_bundle(input, 4)?
            .try_into()
            .expect("exactly four bytes were taken"),
    );
    if nanoseconds >= 1_000_000_000 {
        return Err(SessionError::CorruptBundle);
    }
    Ok(std::time::Duration::new(seconds, nanoseconds))
}

fn take_sized<'a, E>(input: &mut &'a [u8]) -> Result<&'a [u8], SessionError<E>> {
    let length =
        usize::try_from(read_bundle_u64(input)?).map_err(|_| SessionError::CorruptBundle)?;
    take_bundle(input, length)
}

fn read_seeds<D: DomainDefinition>(
    domain: &D,
    scope: &D::SeedScope,
    maximum: u64,
    resource_meter: &ResourceEnvelopeGuard,
) -> Result<ReadSeeds<D>, SessionError<D::Error>> {
    let mut cursor = domain.seeds().open(scope).map_err(SessionError::Domain)?;
    let mut scratch = <D::Seeds as SeedSource<D>>::Scratch::default();
    let mut seeds = Vec::new();
    loop {
        if resource_meter
            .time_exhausted()
            .map_err(|()| SessionError::Resource)?
        {
            return Err(SessionError::Resource);
        }
        let remaining =
            usize::try_from(maximum.saturating_sub(u64::try_from(seeds.len()).unwrap_or(u64::MAX)))
                .unwrap_or(usize::MAX);
        if remaining == 0 {
            return Err(SessionError::Resource);
        }
        let limit = remaining.min(256);
        let before = seeds.len();
        let mut writer = SeedWriter::with_limit(&mut seeds, limit);
        let page = domain
            .seeds()
            .read_batch(&mut cursor, limit, &mut writer, &mut scratch)
            .map_err(SessionError::Domain)?;
        if writer.overflowed() {
            return Err(SessionError::Resource);
        }
        let accepted = seeds.len() - before;
        if page.emitted != accepted || page.emitted > limit {
            return Err(SessionError::InvalidSeed);
        }
        if page.exhausted {
            break;
        }
        if page.emitted == 0 {
            return Err(SessionError::InvalidSeed);
        }
    }
    let mut encoded_cursor = Vec::new();
    domain
        .seeds()
        .encode_cursor(&cursor, &mut encoded_cursor)
        .map_err(SessionError::Domain)?;
    Ok(ReadSeeds {
        seeds,
        encoded_cursor,
    })
}

fn replay_seeds<D: DomainDefinition>(
    domain: &D,
    seeds: &[Seed<D>],
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
) -> Result<(), SessionError<D::Error>> {
    let requests = seeds
        .iter()
        .map(|seed| VerificationReplayRequest {
            artifact: &seed.artifact,
            claim: &seed.verification.claim,
            evidence: &seed.verification.evidence,
            kernel_revision: seed.verification.kernel_revision,
        })
        .collect::<Vec<_>>();
    let replayed = scheduled_replay(
        domain,
        scheduler,
        resource_meter,
        requirements,
        resident_overlap,
        &requests,
        false,
    )?;
    if replayed.len() != seeds.len() || replayed.iter().any(|accepted| !accepted) {
        return Err(SessionError::InvalidSeed);
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "candidate verification names its immutable roots and resource authority explicitly"
)]
#[expect(
    clippy::too_many_lines,
    reason = "batched Verification and immutable Experience creation share one audited transaction"
)]
fn verify_candidates<D: DomainDefinition>(
    domain: &D,
    roots: &[VerifiedArtifact<D>],
    parent_origins: &[usize],
    parent_keys: &[ArtifactKey],
    candidates: Vec<ProposedCandidate<D>>,
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
    instrumentation: &mut Recorder,
    candidate_fates: &mut [CandidateFateObservation],
    child_allowance: Option<VerificationAllowance>,
) -> Result<VerificationOutcome<D>, SessionError<D::Error>> {
    let requests = candidates
        .iter()
        .map(|candidate| {
            let origin = parent_origins[candidate.candidate.source_index];
            ClaimVerificationRequest {
                seed: roots[origin].artifact(),
                candidate: &candidate.candidate.artifact,
            }
        })
        .collect::<Vec<_>>();
    let kernel_started = instrumentation.start();
    let kernel_cpu_before = resource_meter
        .current_cpu()
        .map_err(|()| SessionError::Resource)?;
    let kernel_started_at = std::time::Instant::now();
    let claims_and_verdicts = scheduled_verify_with_report(
        domain,
        scheduler,
        resource_meter,
        requirements,
        resident_overlap,
        &requests,
        false,
        child_allowance,
    );
    let kernel_cpu = resource_meter
        .current_cpu()
        .map_err(|()| SessionError::Resource)?
        .saturating_sub(kernel_cpu_before);
    instrumentation.finish(Phase::VerificationKernel, kernel_started);
    let (claims_and_verdicts, report) = claims_and_verdicts?;
    let external_worker_cpu_ns = external_worker_cpu_ns(report);
    let kernel_usage = verification_kernel_usage(
        report,
        kernel_cpu,
        kernel_started_at.elapsed(),
        u64::try_from(candidates.len()).unwrap_or(u64::MAX),
    );
    if claims_and_verdicts.len() != candidates.len() {
        return Err(SessionError::InvalidSeed);
    }
    let verification_batch_size = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
    let verification_batch_cpu_ns = kernel_usage.cpu_time_ns;
    for candidate in &candidates {
        let fate = candidate_fates
            .get_mut(candidate.fate_index)
            .ok_or(SessionError::CorruptBundle)?;
        fate.verification_batch_cpu_ns = verification_batch_cpu_ns;
        fate.verification_batch_size = verification_batch_size;
    }
    let revision = domain.kernel().revision();
    let mut accepted = Vec::new();
    let mut experience = Vec::with_capacity(candidates.len());
    let mut intelligence_receipts = Vec::new();
    let mut intelligence_settlements = Vec::new();
    let mut verification_decisions = Vec::new();
    let mut action_decisions = Vec::new();
    for (mut candidate, (claim, verdict, rejection_advisory)) in
        candidates.into_iter().zip(claims_and_verdicts)
    {
        let origin = parent_origins[candidate.candidate.source_index];
        let origin_key = roots[origin].key();
        let parent_key = parent_keys[candidate.candidate.source_index];
        let candidate_key = candidate_fates
            .get(candidate.fate_index)
            .ok_or(SessionError::CorruptBundle)?
            .candidate_key;
        let canonical_candidate = std::mem::take(&mut candidate.canonical_candidate);
        let mut encoded_claim = Vec::new();
        domain
            .kernel()
            .encode_claim(&claim, &mut encoded_claim)
            .map_err(SessionError::Domain)?;
        let claim_digest: [u8; 32] = Sha256::digest(encoded_claim).into();
        let attempt_id = attempt_digest(
            candidate_key,
            origin_key,
            parent_key,
            &candidate.operator_symbol,
            candidate.epoch,
            candidate.candidate.proposal_features,
            candidate.candidate.proposal_provenance,
        );
        let fate_index = candidate.fate_index;
        let (experience_verdict, fate_disposition) = match verdict {
            Verdict::Accepted { evidence } => {
                accepted.push((
                    StoredArtifact {
                        artifact: candidate.candidate.artifact,
                        verification: VerificationRecord {
                            claim,
                            evidence,
                            kernel_revision: revision,
                        },
                        provenance: candidate.operator_symbol.clone(),
                        origin_key: Some(origin_key),
                        parent_key: Some(parent_key),
                    },
                    origin,
                    attempt_id,
                ));
                (
                    ExperienceVerdict::Accepted,
                    CandidateFateDisposition::VerifiedAccepted,
                )
            }
            Verdict::Refuted => (
                ExperienceVerdict::Refuted,
                CandidateFateDisposition::VerifiedRefuted,
            ),
            Verdict::Unknown => (
                ExperienceVerdict::Unknown,
                CandidateFateDisposition::VerifiedUnknown,
            ),
        };
        let fate = candidate_fates
            .get_mut(fate_index)
            .ok_or(SessionError::CorruptBundle)?;
        if fate.candidate_key != candidate_key
            || fate.claim_digest != claim_digest
            || fate.parent_key != parent_key
            || fate.allocation_queue != Some(candidate.allocation_queue)
        {
            return Err(SessionError::CorruptBundle);
        }
        fate.disposition = fate_disposition;
        if let Some(receipt) = candidate.intelligence_receipt {
            let outcome = match experience_verdict {
                ExperienceVerdict::Accepted => InvestmentOutcome::VerifiedAccepted {
                    verification_record: SubjectId::new(attempt_id),
                },
                ExperienceVerdict::Refuted => InvestmentOutcome::VerifiedRefuted,
                ExperienceVerdict::Unknown => InvestmentOutcome::VerifiedUnknown,
            };
            intelligence_receipts.push(receipt);
            verification_decisions.push((attempt_id, receipt.decision()));
            intelligence_settlements.push(InvestmentSettlement::new(
                receipt.decision(),
                outcome,
                // The Kernel reports CPU for the indivisible batch. Candidate
                // Fates retain that one shared batch observation; inventing an
                // equal per-Candidate split would be false causal evidence.
                ResourceVector::new(0, 0, 0, 0, 1),
            ));
        }
        if let Some(decision) = candidate.action_decision {
            action_decisions.push((attempt_id, decision));
        }
        experience.push(ExperienceEntry {
            attempt_id,
            candidate_key,
            claim_digest,
            origin_key,
            parent_key,
            causal_parent_key: candidate.causal_parent_key,
            canonical_candidate,
            verdict: experience_verdict,
            rejection_advisory,
            allocation_queue: candidate.allocation_queue,
            operator_symbol: candidate.operator_symbol,
            proposal_features: candidate.candidate.proposal_features,
            proposal_provenance: candidate.candidate.proposal_provenance,
            features: candidate.features,
            verification_requests: 1,
            epoch: candidate.epoch,
        });
    }
    Ok(VerificationOutcome {
        accepted,
        experience,
        intelligence_receipts,
        intelligence_settlements,
        verification_decisions,
        action_decisions,
        kernel_usage,
        external_worker_cpu_ns,
    })
}

fn publish_recovery_interruption<D: DomainDefinition>(
    request: &ImprovementRequest<D>,
    bundle_codec: &RestartBundleCodec<'_, D>,
    seed_cursor: &[u8],
    source: &std::path::Path,
    usage: ResourceUsage,
    resource_meter: &ResourceEnvelopeGuard,
    resident_overlap: u64,
) -> Result<(), SessionError<D::Error>> {
    let mut source_file = std::fs::File::open(source).map_err(SessionError::Durability)?;
    let bundle_bytes = source_file
        .metadata()
        .map_err(SessionError::Durability)?
        .len();
    if !resource_meter.reserve(ResidentReservation::live(resident_overlap).with_transient(
        bundle_codec.replacement_transient_bytes(bundle_bytes, seed_cursor.len(), false),
    )) {
        return Err(SessionError::Resource);
    }
    let bundle_len = usize::try_from(bundle_bytes).map_err(|_| SessionError::Resource)?;
    let encoded = read_fixed_source(&mut source_file, bundle_len)
        .map_err(|()| SessionError::CorruptBundle)?;
    let interrupted = bundle_codec.replace_session_prepared(
        seed_cursor,
        &encoded,
        SessionSeal::Interrupted(usage),
    )?;
    publish_interrupted_bytes(request, interrupted)
}

fn read_fixed_source(reader: &mut impl Read, length: usize) -> Result<Box<[u8]>, ()> {
    let mut encoded = vec![0_u8; length].into_boxed_slice();
    reader.read_exact(&mut encoded).map_err(|_| ())?;
    let mut trailing = [0_u8; 1];
    if reader.read(&mut trailing).map_err(|_| ())? != 0 {
        return Err(());
    }
    Ok(encoded)
}

#[expect(
    clippy::too_many_arguments,
    reason = "failure publication carries the complete restart boundary explicitly"
)]
fn persist_setup_interruption<D: DomainDefinition>(
    request: &ImprovementRequest<D>,
    bundle_codec: &RestartBundleCodec<'_, D>,
    seed_cursor: &[u8],
    ledger: &ExperienceLedger,
    intelligence: &IntelligenceCore,
    usage: ResourceUsage,
    resource_meter: &ResourceEnvelopeGuard,
    resident_overlap: u64,
) -> Result<(), SessionError<D::Error>> {
    if request.bundle.forks() && request.bundle.source().is_some() {
        // A Fork is not restart-compatible until a complete current-revision
        // checkpoint has been encoded. Preserve its source on setup failure;
        // never splice a current Session onto legacy Revisions and Recovery.
        return Ok(());
    }
    if let Some(source) = request.bundle.source() {
        return publish_recovery_interruption(
            request,
            bundle_codec,
            seed_cursor,
            source,
            usage,
            resource_meter,
            resident_overlap,
        );
    }
    let interrupted = bundle_codec.seal_admitted(
        seed_cursor,
        RestartBundleState::new(
            &[],
            &[],
            SearchTailView::new(&[], &[], &[]),
            ledger,
            intelligence,
        ),
        SessionSeal::Interrupted(usage),
        resource_meter,
        SealAdmission {
            resident_overlap,
            additional_transient: 0,
            pending_durability: 0,
        },
    )?;
    publish_interrupted_bytes(request, interrupted)
}

fn persist_active_interruption<D: DomainDefinition>(
    bundle_codec: &RestartBundleCodec<'_, D>,
    seed_cursor: &[u8],
    checkpoint: &[u8],
    usage: ResourceUsage,
    resource_meter: &ResourceEnvelopeGuard,
    resident_overlap: u64,
    durability: &mut durability::CheckpointWriter,
) -> Result<(), SessionError<D::Error>> {
    if !resource_meter.reserve(
        ResidentReservation::live(resident_overlap)
            .with_transient(bundle_codec.replacement_transient_bytes(
                checkpoint.len() as u64,
                seed_cursor.len(),
                true,
            ))
            .with_pending_durability(durability.pending_bytes()),
    ) {
        return Err(SessionError::Resource);
    }
    let interrupted = bundle_codec.replace_session_prepared(
        seed_cursor,
        checkpoint,
        SessionSeal::Interrupted(usage),
    )?;
    durability
        .submit(interrupted)
        .and_then(|()| durability.barrier().map(|_| ()))
        .map_err(SessionError::Durability)
}

fn publish_interrupted_bytes<D: DomainDefinition>(
    request: &ImprovementRequest<D>,
    interrupted: Vec<u8>,
) -> Result<(), SessionError<D::Error>> {
    let mut durability = durability::CheckpointWriter::start(request.bundle.target().to_path_buf())
        .map_err(SessionError::Durability)?;
    durability
        .submit(interrupted)
        .map_err(SessionError::Durability)?;
    durability.finish().map_err(SessionError::Durability)
}

fn encode_recovery_segment<D: DomainDefinition>(
    _domain: &D,
    pareto: &[VerifiedArtifact<D>],
    search_tail: &SearchTailView<'_, D>,
) -> Result<Vec<u8>, SessionError<D::Error>> {
    let frontier = search_tail.frontier;
    let deferred_candidates = search_tail.deferred_candidates;
    let pending_parents = search_tail.pending_parents;
    let encoded_len = recovery_encoded_len(
        pareto,
        &SearchTailView::new(frontier, deferred_candidates, pending_parents),
    );
    let mut recovery =
        Vec::with_capacity(usize::try_from(encoded_len).map_err(|_| SessionError::Resource)?);
    push_u64(&mut recovery, pareto.len() as u64);
    for artifact in pareto {
        recovery.extend_from_slice(artifact.key().as_bytes());
    }
    push_u64(&mut recovery, frontier.len() as u64);
    for (artifact, _) in frontier {
        recovery.extend_from_slice(artifact.key().as_bytes());
    }
    push_u64(&mut recovery, deferred_candidates.len() as u64);
    for candidate in deferred_candidates {
        let parent_key = frontier
            .get(candidate.candidate.source_index)
            .ok_or(SessionError::CorruptBundle)?
            .0
            .key();
        push_bytes(&mut recovery, &candidate.canonical_candidate);
        recovery.extend_from_slice(parent_key.as_bytes());
        push_bytes(&mut recovery, &candidate.operator_symbol);
        recovery.extend_from_slice(&candidate.proposal_limit.to_le_bytes());
        recovery.push(u8::from(candidate.protected_derived));
        for feature in candidate.candidate.proposal_features.as_array() {
            recovery.extend_from_slice(&feature.to_bits().to_le_bytes());
        }
        if let Some(provenance) = candidate.candidate.proposal_provenance {
            recovery.push(1);
            recovery.extend_from_slice(&provenance.support_key());
        } else {
            recovery.push(0);
        }
    }
    push_u64(&mut recovery, pending_parents.len() as u64);
    for pending in pending_parents {
        recovery.extend_from_slice(pending.key.as_bytes());
        push_u64(&mut recovery, pending.primitive_offsets.len() as u64);
        for offset in &pending.primitive_offsets {
            push_u64(&mut recovery, *offset);
        }
        recovery.push(u8::from(pending.derived_sampled));
    }
    push_u64(&mut recovery, deferred_candidates.len() as u64);
    for candidate in deferred_candidates {
        if let Some(decision) = candidate.action_decision {
            recovery.push(1);
            recovery.extend_from_slice(&decision.identity());
        } else {
            recovery.push(0);
        }
        if let Some(parent) = candidate.causal_parent_key {
            recovery.push(1);
            recovery.extend_from_slice(parent.as_bytes());
        } else {
            recovery.push(0);
        }
    }
    recovery.push(u8::from(search_tail.generation_complete));
    debug_assert_eq!(recovery.len() as u64, encoded_len);
    Ok(recovery)
}

struct BundleEncodingInput<'a, D: DomainDefinition> {
    domain: &'a D,
    request: &'a ImprovementRequest<D>,
    identity: &'a crate::SemanticIdentity,
    encoded_scope: &'a [u8],
    goals_digest: [u8; 32],
    environment: &'a [u8],
    seed_cursor: &'a [u8],
    state: RestartBundleState<'a, D>,
    session_seal: SessionSeal,
    maximum_output_capacity: usize,
}

fn encode_bundle<D: DomainDefinition>(
    input: BundleEncodingInput<'_, D>,
) -> Result<Vec<u8>, SessionError<D::Error>> {
    let BundleEncodingInput {
        domain,
        request,
        identity,
        encoded_scope,
        goals_digest,
        environment,
        seed_cursor,
        state,
        session_seal,
        maximum_output_capacity,
    } = input;
    let RestartBundleState {
        artifacts,
        pareto,
        search_tail,
        ledger,
        intelligence,
    } = state;
    let artifact_payload = encode_artifact_payload(artifacts)?;
    let revision_ids = revision_ids(
        identity.as_str(),
        artifacts,
        intelligence,
        intelligence.checkpoint_identity(),
    );
    let mut revisions = Vec::with_capacity(136 + intelligence.checkpoint_bytes().len());
    revisions.extend_from_slice(&revision_ids.knowledge);
    revisions.extend_from_slice(&revision_ids.model);
    revisions.extend_from_slice(&revision_ids.runtime_policy);
    revisions.extend_from_slice(&revision_ids.intelligence);
    push_bytes(&mut revisions, intelligence.checkpoint_bytes());
    test_fault_point("revision-sealed");
    let recovery = encode_recovery_segment(domain, pareto, &search_tail)?;
    let encoded_experience = ledger.encode();
    let mut bundle = CanonicalBundle::new(
        identity.as_str().as_bytes().to_vec(),
        Vec::new(),
        revisions,
        artifact_payload,
        encoded_experience,
        recovery,
    );
    let session = encode_session_prepared(&PreparedSessionInput {
        domain,
        request,
        scope: encoded_scope,
        goals_digest,
        environment,
        seed_cursor,
        restart_state_root: bundle.restart_state_root(),
        session_seal,
    });
    bundle.replace_segment(SegmentKind::Session, session);
    let bundle = bundle
        .encode_bounded(maximum_output_capacity)
        .map_err(|error| {
            if error.is_output_limit() {
                SessionError::Resource
            } else {
                SessionError::CorruptBundle
            }
        })?;
    test_fault_point("manifest-sealed");
    Ok(bundle)
}

fn encode_artifact_payload<D: DomainDefinition>(
    artifacts: &[VerifiedArtifact<D>],
) -> Result<Vec<u8>, SessionError<D::Error>> {
    let payload_len = artifacts.iter().fold(8_u64, |bytes, artifact| {
        bytes.saturating_add(artifact.inner.bundle_record.len() as u64)
    });
    let mut canonical = Vec::with_capacity(artifacts.len());
    canonical.extend(artifacts);
    canonical.sort_unstable_by_key(|artifact| artifact.key());
    copy_cached_artifact_records(
        canonical
            .into_iter()
            .map(|artifact| artifact.inner.bundle_record.as_ref()),
        artifacts.len(),
        payload_len,
    )
    .map_err(|()| SessionError::Resource)
}

fn copy_cached_artifact_records<'a>(
    records: impl Iterator<Item = &'a [u8]>,
    record_count: usize,
    payload_len: u64,
) -> Result<Vec<u8>, ()> {
    let mut payload = Vec::with_capacity(usize::try_from(payload_len).map_err(|_| ())?);
    push_u64(&mut payload, record_count as u64);
    for record in records {
        payload.extend_from_slice(record);
    }
    debug_assert_eq!(payload.len() as u64, payload_len);
    Ok(payload)
}

fn revision_ids<D: DomainDefinition>(
    semantic_identity: &str,
    artifacts: &[VerifiedArtifact<D>],
    policy_owner: &dyn IntelligenceBundleView,
    intelligence: [u8; 32],
) -> RevisionIds {
    let mut canonical = Vec::with_capacity(artifacts.len());
    canonical.extend(artifacts);
    canonical.sort_unstable_by_key(|artifact| artifact.key());
    let mut knowledge = Sha256::new();
    knowledge.update(b"reflex-knowledge-revision-v1\0");
    knowledge.update((semantic_identity.len() as u64).to_le_bytes());
    knowledge.update(semantic_identity.as_bytes());
    for artifact in canonical {
        knowledge.update(artifact.key().as_bytes());
        knowledge.update(artifact.inner.origin_key.as_bytes());
        match artifact.inner.parent_key {
            Some(parent) => {
                knowledge.update([1]);
                knowledge.update(parent.as_bytes());
            }
            None => knowledge.update([0]),
        }
        knowledge.update((artifact.inner.provenance.len() as u64).to_le_bytes());
        knowledge.update(&artifact.inner.provenance);
    }
    knowledge.update(policy_owner.knowledge_product_identity());
    RevisionIds {
        knowledge: knowledge.finalize().into(),
        model: policy_owner.model_ecology_identity(),
        runtime_policy: policy_owner.runtime_policy_revision().identity(),
        intelligence,
    }
}

fn legacy_knowledge_revision_id<D: DomainDefinition>(
    semantic_identity: &str,
    artifacts: &[VerifiedArtifact<D>],
    state: &KnowledgeState,
) -> [u8; 32] {
    let mut canonical = artifacts.iter().collect::<Vec<_>>();
    canonical.sort_unstable_by_key(|artifact| artifact.key());
    let mut knowledge = Sha256::new();
    knowledge.update(b"reflex-knowledge-revision-v1\0");
    knowledge.update((semantic_identity.len() as u64).to_le_bytes());
    knowledge.update(semantic_identity.as_bytes());
    for artifact in canonical {
        knowledge.update(artifact.key().as_bytes());
        knowledge.update(artifact.inner.origin_key.as_bytes());
        match artifact.inner.parent_key {
            Some(parent) => {
                knowledge.update([1]);
                knowledge.update(parent.as_bytes());
            }
            None => knowledge.update([0]),
        }
        knowledge.update((artifact.inner.provenance.len() as u64).to_le_bytes());
        knowledge.update(&artifact.inner.provenance);
    }
    knowledge.update(state.revision_digest(semantic_identity));
    knowledge.finalize().into()
}

fn encode_session<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    seed_cursor: &[u8],
    restart_state_root: [u8; 32],
    session_seal: SessionSeal,
) -> Result<Vec<u8>, SessionError<D::Error>> {
    let mut scope = Vec::new();
    domain
        .seeds()
        .encode_scope(&request.seeds, &mut scope)
        .map_err(SessionError::Domain)?;
    let goals = GoalEvaluator::encode_set(domain, &request.goals)?;
    let environment = crate::MeasurementEnvironment::local_process();
    Ok(encode_session_prepared(&PreparedSessionInput {
        domain,
        request,
        scope: &scope,
        goals_digest: Sha256::digest(goals).into(),
        environment: environment.identity().as_bytes(),
        seed_cursor,
        restart_state_root,
        session_seal,
    }))
}

fn session_encoded_len(scope_len: usize, cursor_len: usize, environment_len: usize) -> u64 {
    // Disposition, Runtime Revision, restart root, Goal/Scope/Cursor digests,
    // sized Scope/Cursor, requested envelope, Kernel Revision, sized
    // Measurement Environment, completion, and consumed envelope.
    1_u64
        .saturating_add(8)
        .saturating_add(32 * 4)
        .saturating_add(8)
        .saturating_add(u64::try_from(scope_len).unwrap_or(u64::MAX))
        .saturating_add(8)
        .saturating_add(u64::try_from(cursor_len).unwrap_or(u64::MAX))
        .saturating_add(8 * 4 + 12 * 2)
        .saturating_add(8)
        .saturating_add(8)
        .saturating_add(u64::try_from(environment_len).unwrap_or(u64::MAX))
        .saturating_add(1)
        .saturating_add(8 * 4 + 12 * 2)
}

#[derive(Clone, Copy)]
struct PreparedSessionInput<'a, D: DomainDefinition> {
    domain: &'a D,
    request: &'a ImprovementRequest<D>,
    scope: &'a [u8],
    goals_digest: [u8; 32],
    environment: &'a [u8],
    seed_cursor: &'a [u8],
    restart_state_root: [u8; 32],
    session_seal: SessionSeal,
}

fn encode_session_prepared<D: DomainDefinition>(input: &PreparedSessionInput<'_, D>) -> Vec<u8> {
    let domain = input.domain;
    let request = input.request;
    let scope = input.scope;
    let goals_digest = input.goals_digest;
    let environment = input.environment;
    let seed_cursor = input.seed_cursor;
    let restart_state_root = input.restart_state_root;
    let session_seal = input.session_seal;
    let capacity = usize::try_from(session_encoded_len(
        scope.len(),
        seed_cursor.len(),
        environment.len(),
    ))
    .unwrap_or(usize::MAX);
    let mut payload = Vec::with_capacity(capacity);
    payload.push(u8::from(matches!(session_seal, SessionSeal::Completed(..))));
    push_u64(&mut payload, RUNTIME_REVISION);
    payload.extend_from_slice(&restart_state_root);
    payload.extend_from_slice(&goals_digest);
    payload.extend_from_slice(&Sha256::digest(scope));
    push_bytes(&mut payload, scope);
    payload.extend_from_slice(&Sha256::digest(seed_cursor));
    push_bytes(&mut payload, seed_cursor);
    push_u64(&mut payload, request.resources.worker_threads.get() as u64);
    push_u64(&mut payload, request.resources.resident_bytes.get());
    push_u64(&mut payload, request.resources.durable_bytes.get());
    push_duration(&mut payload, request.resources.elapsed_time.get());
    push_duration(&mut payload, request.resources.cpu_time.get());
    push_u64(&mut payload, request.resources.verification_requests.get());
    push_u64(&mut payload, domain.kernel().revision().0);
    push_bytes(&mut payload, environment);
    let (completion, usage) = match session_seal {
        SessionSeal::Interrupted(usage) => (None, usage),
        SessionSeal::Completed(completion, usage) => (Some(completion), usage),
    };
    payload.push(match completion {
        None => 0,
        Some(Completion::ResourceEnvelopeExhausted) => 1,
        Some(Completion::SuccessConditionsSatisfied) => 2,
        Some(Completion::StoppedByObserver) => 3,
        Some(Completion::NoEligibleWork) => 4,
    });
    push_u64(&mut payload, usage.worker_threads as u64);
    push_u64(&mut payload, usage.resident_bytes);
    push_u64(&mut payload, usage.verification_requests);
    push_u64(&mut payload, usage.durable_bytes);
    push_duration(&mut payload, usage.elapsed_time);
    push_duration(&mut payload, usage.cpu_time);
    debug_assert_eq!(payload.len(), capacity);
    payload
}

fn push_duration(output: &mut Vec<u8>, duration: std::time::Duration) {
    push_u64(output, duration.as_secs());
    output.extend_from_slice(&duration.subsec_nanos().to_le_bytes());
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_bytes(output: &mut Vec<u8>, value: &[u8]) {
    push_u64(output, value.len() as u64);
    output.extend_from_slice(value);
}

fn stable_digest(semantic_identity: &str, canonical_artifact: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-artifact-v1\0");
    digest.update((semantic_identity.len() as u64).to_le_bytes());
    digest.update(semantic_identity.as_bytes());
    digest.update(canonical_artifact);
    digest.finalize().into()
}

fn primitive_action_parent_eligible(
    repair_parent: Option<ArtifactKey>,
    candidate_parent: ArtifactKey,
) -> bool {
    repair_parent.is_none_or(|required| required == candidate_parent)
}

fn rejection_advisory_cursor(advisory: RejectionAdvisory) -> u64 {
    match advisory {
        RejectionAdvisory::Counterexample {
            input,
            expected,
            observed,
        } => input ^ expected.rotate_left(21) ^ observed.rotate_left(43),
        RejectionAdvisory::MalformedArtifact => 1,
        RejectionAdvisory::MalformedClaim => 2,
        RejectionAdvisory::KernelCheckFailure => 3,
        RejectionAdvisory::ClaimMismatch => 4,
        RejectionAdvisory::ForbiddenConstruct => 5,
        RejectionAdvisory::ScopeViolation => 6,
        RejectionAdvisory::DependencyViolation => 7,
        RejectionAdvisory::AxiomViolation => 8,
        RejectionAdvisory::DependencyMismatch => 9,
    }
}

fn rejection_advisory_profile(advisory: RejectionAdvisory) -> ([u8; 32], f32, f32) {
    let mut digest = Sha256::new();
    digest.update(b"reflex-rejection-advisory-action-v1\0");
    let (class, counterexample) = match advisory {
        RejectionAdvisory::Counterexample {
            input,
            expected,
            observed,
        } => {
            digest.update([1]);
            digest.update(input.to_le_bytes());
            digest.update(expected.to_le_bytes());
            digest.update(observed.to_le_bytes());
            (1_u8, true)
        }
        RejectionAdvisory::MalformedArtifact => (2, false),
        RejectionAdvisory::MalformedClaim => (3, false),
        RejectionAdvisory::KernelCheckFailure => (4, false),
        RejectionAdvisory::ClaimMismatch => (5, false),
        RejectionAdvisory::ForbiddenConstruct => (6, false),
        RejectionAdvisory::ScopeViolation => (7, false),
        RejectionAdvisory::DependencyViolation => (8, false),
        RejectionAdvisory::AxiomViolation => (9, false),
        RejectionAdvisory::DependencyMismatch => (10, false),
    };
    if !counterexample {
        digest.update([class]);
    }
    (
        digest.finalize().into(),
        f32::from(class) / 10.0,
        f32::from(counterexample),
    )
}

fn attempt_digest(
    candidate_key: ArtifactKey,
    origin_key: ArtifactKey,
    parent_key: ArtifactKey,
    operator_symbol: &[u8],
    epoch: u64,
    proposal_features: ProposalFeatures,
    proposal_provenance: Option<ProposalProvenance>,
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-attempt-observation-v3\0");
    digest.update(candidate_key.as_bytes());
    digest.update(origin_key.as_bytes());
    digest.update(parent_key.as_bytes());
    digest.update((operator_symbol.len() as u64).to_le_bytes());
    digest.update(operator_symbol);
    digest.update(epoch.to_le_bytes());
    for feature in proposal_features.as_array() {
        digest.update(feature.to_bits().to_le_bytes());
    }
    if let Some(provenance) = proposal_provenance {
        digest.update([1]);
        digest.update(provenance.support_key());
    } else {
        digest.update([0]);
    }
    digest.finalize().into()
}

#[cfg(debug_assertions)]
fn test_fault_point(phase: &str) {
    const PHASE_ENV: &str = "REFLEX_INTERNAL_TEST_FAULT_PHASE";
    const OCCURRENCE_ENV: &str = "REFLEX_INTERNAL_TEST_FAULT_OCCURRENCE";
    if std::env::var_os(PHASE_ENV).as_deref() != Some(std::ffi::OsStr::new(phase)) {
        return;
    }
    let expected = std::env::var(OCCURRENCE_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1);
    if FAULT_OCCURRENCE.fetch_add(1, AtomicOrdering::Relaxed) + 1 == expected {
        std::process::abort();
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
fn test_fault_point(_: &str) {}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::{BTreeSet, HashSet};
    use std::io::Cursor;
    #[cfg(feature = "internal-experiments")]
    use std::time::Duration;

    #[cfg(feature = "internal-experiments")]
    use sha2::Digest;

    use super::{
        ENUMERATION_COMPLETE, PendingParent, append_proposal_features,
        bind_runtime_policy_trial_checkpoint, candidate_generation_limit,
        charged_external_interruption, commit_pending_progress, copy_cached_artifact_records,
        external_worker_cpu_ns, fixed_resident_categories, generation_refill_limit,
        goal_preferred_eligible_index, knowledge_campaign_request_limit,
        knowledge_verification_batch_fits, merge_intelligence_allocations,
        moved_tail_transaction_peak, observation_resident_fits_bound, operational_spec_limit,
        operator_feature_values, pending_parent_vector_resident_bytes,
        policy_proposal_graph_resident_bytes, primitive_action_parent_eligible,
        proportional_budget, protected_origin_keys, read_fixed_source,
        recovery_after_admission_bound, run_if_verification_fits, runtime_policy_trial_sequence,
        semantic_consequence_decision, shadow_elapsed_ns, sort_prefix_by,
        verification_kernel_usage, verification_obligation_resources,
    };
    #[cfg(feature = "internal-experiments")]
    use super::{
        LEGACY_RUNTIME_REVISION, RUNTIME_REVISION, decode_session_segment, inspect_session_segment,
        push_bytes, push_duration, push_u64,
    };
    use crate::intelligence::{AllocationSource, DecisionId, ResourceVector, SpecialistRevisionId};
    use crate::learning::{FEATURE_COUNT, Features};
    use crate::policy::RuntimePolicyState;
    use crate::resource::ResidentReservation;
    use crate::{
        ExternalVerificationUsage, ProposalFeatures, VerificationBatchReport,
        VerificationWorkerRequirements,
    };

    #[test]
    fn knowledge_campaign_selects_the_goal_preferred_eligible_root() {
        let excluded = BTreeSet::from([[2; 32]]);

        assert_eq!(
            goal_preferred_eligible_index(&[2, 0, 1], &[[1; 32], [2; 32], [3; 32]], &excluded),
            Some(2)
        );
    }

    #[test]
    fn knowledge_campaign_admits_multiple_candidates_without_exceeding_its_sub_budget() {
        let policy = RuntimePolicyState::bootstrap().active();
        let resources = ResourceVector::new(1_000, 1_000, 0, 1_000, 3);

        assert_eq!(knowledge_campaign_request_limit(resources, policy), 3);
    }

    #[test]
    fn variable_observation_residency_may_be_smaller_than_its_admitted_bound() {
        assert!(observation_resident_fits_bound(7, 8));
        assert!(observation_resident_fits_bound(8, 8));
        assert!(!observation_resident_fits_bound(9, 8));
    }

    #[test]
    fn action_publication_peak_retains_original_and_large_progress_clone() {
        let parents = (0_u8..16)
            .map(|identity| PendingParent::new(crate::ArtifactKey([identity; 32]), 1_024, true))
            .collect::<Vec<_>>();
        let one_progress_vector =
            pending_parent_vector_resident_bytes(&parents, parents.capacity());
        let publication_overlap = one_progress_vector.saturating_mul(2);

        assert!(one_progress_vector > 16 * 1_024 * std::mem::size_of::<u64>() as u64);
        assert_eq!(
            publication_overlap,
            one_progress_vector.saturating_add(one_progress_vector),
            "the original all-selected progress vector and encoded progressed clone coexist until the durability barrier"
        );
        assert_eq!(
            ResidentReservation::live(one_progress_vector)
                .with_transient(one_progress_vector)
                .peak_bytes(),
            publication_overlap,
            "the exact publication reservation admits both vectors without hidden headroom"
        );
        assert!(publication_overlap > one_progress_vector);
    }

    #[test]
    fn knowledge_verification_attributes_one_batch_exactly_across_three_obligations() {
        let batch = ResourceVector::new(101, 1_000, 0, 53, 3);
        let shares = (0..3)
            .map(|index| {
                verification_obligation_resources(batch, index, 3)
                    .expect("three canonical obligations fit the bounded batch")
            })
            .collect::<Vec<_>>();

        assert_eq!(shares[0], ResourceVector::new(34, 1_000, 0, 53, 1));
        assert_eq!(shares[1], ResourceVector::new(34, 0, 0, 0, 1));
        assert_eq!(shares[2], ResourceVector::new(33, 0, 0, 0, 1));
        assert_eq!(
            shares
                .into_iter()
                .try_fold(ResourceVector::default(), ResourceVector::checked_add),
            Some(batch)
        );
    }

    #[test]
    fn external_worker_usage_is_the_shadow_arm_resource_authority() {
        let report = VerificationBatchReport::external(
            ExternalVerificationUsage::new(
                2,
                4_096,
                std::time::Duration::from_nanos(700),
                std::time::Duration::from_nanos(500),
            ),
            false,
        );

        assert_eq!(
            verification_kernel_usage(
                report,
                std::time::Duration::from_nanos(3),
                std::time::Duration::from_nanos(5),
                7,
            ),
            ResourceVector::new(500, 4_096, 0, 700, 7)
        );
        assert_eq!(external_worker_cpu_ns(report), 500);
        assert_eq!(
            external_worker_cpu_ns(VerificationBatchReport::in_process()),
            0
        );
        assert_eq!(
            shadow_elapsed_ns(std::time::Duration::from_nanos(5), 700),
            700
        );
    }

    #[test]
    fn charged_external_overrun_requires_interrupted_publication() {
        let external = VerificationWorkerRequirements::external(
            std::num::NonZeroUsize::new(1).unwrap(),
            std::num::NonZeroU64::new(4_096).unwrap(),
        );
        let in_process = VerificationWorkerRequirements::in_process();

        assert!(charged_external_interruption::<()>(
            &crate::SessionError::Resource,
            external,
        ));
        assert!(charged_external_interruption::<()>(
            &crate::SessionError::VerificationWorker,
            external,
        ));
        assert!(!charged_external_interruption::<()>(
            &crate::SessionError::Resource,
            in_process,
        ));
    }

    #[test]
    fn knowledge_verification_resource_partition_rejects_invalid_batch_shapes() {
        assert!(
            verification_obligation_resources(ResourceVector::new(1, 1, 0, 1, 0), 0, 0).is_err()
        );
        assert!(
            verification_obligation_resources(ResourceVector::new(1, 1, 0, 1, 2), 2, 2).is_err()
        );
        assert!(
            verification_obligation_resources(ResourceVector::new(1, 1, 0, 1, 3), 0, 2).is_err()
        );
    }

    #[test]
    fn knowledge_verification_refuses_each_insufficient_dimension_before_kernel_dispatch() {
        let required = ResourceVector::new(101, 1_000, 1, 53, 3);
        let dispatched = Cell::new(0_u8);
        let preflight = |available| {
            if knowledge_verification_batch_fits(available, required) {
                dispatched.set(dispatched.get().saturating_add(1));
            }
            assert_eq!(
                dispatched.get(),
                0,
                "an insufficient aggregate batch must refuse before Kernel dispatch"
            );
        };

        preflight(ResourceVector::new(100, 1_000, 1, 53, 3));
        preflight(ResourceVector::new(101, 999, 1, 53, 3));
        preflight(ResourceVector::new(101, 1_000, 0, 53, 3));
        preflight(ResourceVector::new(101, 1_000, 1, 52, 3));
        preflight(ResourceVector::new(101, 1_000, 1, 53, 2));
    }

    #[test]
    fn durable_preflight_bounds_every_candidate_as_pareto_frontier_and_pending_parent() {
        assert_eq!(recovery_after_admission_bound(201, 1, 1), 381);
        assert_eq!(recovery_after_admission_bound(201, 3, 2), 765);
    }

    #[test]
    fn runtime_policy_trial_checkpoint_binds_candidate_universe_and_both_plans() {
        let baseline = bind_runtime_policy_trial_checkpoint(
            [1; 32],
            &[[2; 32], [3; 32]],
            &[[4; 32]],
            &[false],
            [5; 32],
            [6; 32],
        );
        for changed in [
            bind_runtime_policy_trial_checkpoint(
                [9; 32],
                &[[2; 32], [3; 32]],
                &[[4; 32]],
                &[false],
                [5; 32],
                [6; 32],
            ),
            bind_runtime_policy_trial_checkpoint(
                [1; 32],
                &[[2; 32], [8; 32]],
                &[[4; 32]],
                &[false],
                [5; 32],
                [6; 32],
            ),
            bind_runtime_policy_trial_checkpoint(
                [1; 32],
                &[[2; 32], [3; 32]],
                &[[7; 32]],
                &[false],
                [5; 32],
                [6; 32],
            ),
            bind_runtime_policy_trial_checkpoint(
                [1; 32],
                &[[2; 32], [3; 32]],
                &[[4; 32]],
                &[true],
                [5; 32],
                [6; 32],
            ),
            bind_runtime_policy_trial_checkpoint(
                [1; 32],
                &[[2; 32], [3; 32]],
                &[[4; 32]],
                &[false],
                [8; 32],
                [6; 32],
            ),
            bind_runtime_policy_trial_checkpoint(
                [1; 32],
                &[[2; 32], [3; 32]],
                &[[4; 32]],
                &[false],
                [5; 32],
                [8; 32],
            ),
        ] {
            assert_ne!(changed, baseline);
        }
    }

    #[cfg(feature = "internal-experiments")]
    #[test]
    fn session_inspection_reports_completion_and_resource_usage() {
        let mut encoded = vec![1];
        push_u64(&mut encoded, RUNTIME_REVISION);
        encoded.extend_from_slice(&[9; 32]);
        encoded.extend_from_slice(&[1; 32]);
        encoded.extend_from_slice(&sha2::Sha256::digest(b"scope"));
        push_bytes(&mut encoded, b"scope");
        encoded.extend_from_slice(&sha2::Sha256::digest(b"cursor"));
        push_bytes(&mut encoded, b"cursor");
        push_u64(&mut encoded, 6);
        push_u64(&mut encoded, 32 << 30);
        push_u64(&mut encoded, 1 << 30);
        push_duration(&mut encoded, Duration::from_mins(10));
        push_duration(&mut encoded, Duration::from_secs(601));
        push_u64(&mut encoded, 1024);
        push_u64(&mut encoded, 7);
        push_bytes(&mut encoded, b"environment");
        encoded.push(1);
        push_u64(&mut encoded, 6);
        push_u64(&mut encoded, 123_456);
        push_u64(&mut encoded, 16);
        push_u64(&mut encoded, 654_321);
        push_duration(&mut encoded, Duration::from_millis(250));
        push_duration(&mut encoded, Duration::from_secs(602));

        let inspection = inspect_session_segment(&encoded).expect("valid Session segment");

        assert!(inspection.completed);
        assert_eq!(
            inspection.completion,
            Some(crate::Completion::ResourceEnvelopeExhausted)
        );
        assert_eq!(inspection.requested.worker_threads, 6);
        assert_eq!(inspection.requested.verification_requests, 1024);
        assert_eq!(inspection.usage.worker_threads, 6);
        assert_eq!(inspection.usage.resident_bytes, 123_456);
        assert_eq!(inspection.usage.verification_requests, 16);
        assert_eq!(inspection.usage.durable_bytes, 654_321);
        assert_eq!(inspection.usage.elapsed_time, Duration::from_millis(250));
        assert_eq!(inspection.usage.cpu_time, Duration::from_secs(602));
    }

    #[cfg(feature = "internal-experiments")]
    #[test]
    fn legacy_session_revision_is_importable_only_when_completed() {
        let encode = |disposition| {
            let mut encoded = vec![disposition];
            push_u64(&mut encoded, LEGACY_RUNTIME_REVISION);
            encoded.extend_from_slice(&[1; 32]);
            encoded.extend_from_slice(&sha2::Sha256::digest(b"scope"));
            push_bytes(&mut encoded, b"scope");
            encoded.extend_from_slice(&sha2::Sha256::digest(b"cursor"));
            push_bytes(&mut encoded, b"cursor");
            push_u64(&mut encoded, 1);
            push_u64(&mut encoded, 1);
            push_u64(&mut encoded, 1);
            push_duration(&mut encoded, Duration::from_nanos(1));
            push_duration(&mut encoded, Duration::from_nanos(1));
            push_u64(&mut encoded, 1);
            push_u64(&mut encoded, 1);
            push_bytes(&mut encoded, b"environment");
            encoded.push(u8::from(disposition == 1));
            for value in [1_u64, 0, 0, 0] {
                push_u64(&mut encoded, value);
            }
            push_duration(&mut encoded, Duration::ZERO);
            push_duration(&mut encoded, Duration::ZERO);
            encoded
        };

        let completed_bytes = encode(1);
        let completed = decode_session_segment::<()>(completed_bytes.as_slice()).unwrap();
        assert!(completed.completed);
        assert_eq!(completed.runtime_revision, LEGACY_RUNTIME_REVISION);
        assert!(matches!(
            decode_session_segment::<()>(encode(0).as_slice()),
            Err(crate::SessionError::IncompatibleBundle)
        ));
    }

    #[test]
    fn candidate_generation_covers_one_cohort_and_two_cohorts_of_operator_breadth() {
        assert_eq!(
            candidate_generation_limit(24, 1_008, 16, 9, u64::MAX, 2, 8),
            192
        );
        assert_eq!(candidate_generation_limit(8, 10, 32, 8, u64::MAX, 2, 8), 80);
        assert_eq!(
            candidate_generation_limit(24, 1_008, 16, 9, 64 * 1024, 2, 8),
            8
        );
        assert_eq!(
            candidate_generation_limit(0, 1_008, 16, 9, u64::MAX, 2, 8),
            0
        );
    }

    #[test]
    fn runtime_policy_controls_generation_lookahead_shortlist_and_exploration() {
        assert_eq!(
            candidate_generation_limit(4, 1_000, 100, 1, u64::MAX, 2, 8),
            32
        );
        assert_eq!(
            candidate_generation_limit(4, 1_000, 100, 1, u64::MAX, 2, 16),
            64
        );
        assert_eq!(
            candidate_generation_limit(4, 1_000, 100, 10, u64::MAX, 20, 8),
            800
        );
        assert_eq!(proportional_budget(9, 250), 3);
        assert_eq!(proportional_budget(9, 0), 0);
        assert_eq!(proportional_budget(9, 1_000), 9);
    }

    #[test]
    fn operational_spec_count_never_overfills_the_exact_market_capacity() {
        assert_eq!(operational_spec_limit(0, 9), 0);
        assert_eq!(operational_spec_limit(9, 1), 1);
        assert_eq!(operational_spec_limit(9, 9), 9);
        assert_eq!(operational_spec_limit(4, 9), 4);
        assert_eq!(operational_spec_limit(12, 12), 9);
    }

    #[test]
    fn admitted_candidate_semantics_credit_the_originating_action_before_verification() {
        let attempt = [41; 32];
        let action = DecisionId::from_identity([42; 32]);
        let verification = DecisionId::from_identity([43; 32]);
        assert_eq!(
            semantic_consequence_decision(
                attempt,
                &[(attempt, action)],
                &[(attempt, verification)],
            ),
            Some(action)
        );
        assert_eq!(
            semantic_consequence_decision(attempt, &[], &[(attempt, verification)]),
            Some(verification)
        );
    }

    #[test]
    fn policy_trial_cadence_advances_one_bounded_challenger_at_a_time() {
        let cadence = 32_u32;
        let sequences = (1..=20_u64)
            .map(|trial| {
                runtime_policy_trial_sequence(trial * u64::from(cadence), cadence).unwrap()
            })
            .collect::<Vec<_>>();

        assert_eq!(sequences, (0..20_u64).collect::<Vec<_>>());
        assert_eq!(runtime_policy_trial_sequence(31, cadence), None);
        assert_eq!(runtime_policy_trial_sequence(33, cadence), None);
    }

    #[test]
    fn multi_obligation_consolidation_does_not_enter_kernel_when_one_request_remains() {
        let calls = Cell::new(0_u8);

        let result = run_if_verification_fits(1, 2, || {
            calls.set(calls.get() + 1);
            Ok::<_, ()>(())
        })
        .unwrap();

        assert!(result.is_none());
        assert_eq!(calls.get(), 0);
    }

    #[test]
    fn consolidation_enters_kernel_once_when_every_obligation_fits() {
        let calls = Cell::new(0_u8);

        let result = run_if_verification_fits(2, 2, || {
            calls.set(calls.get() + 1);
            Ok::<_, ()>(17)
        })
        .unwrap();

        assert_eq!(result, Some(17));
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn intelligence_keeps_true_bootstrap_and_specialists_then_cooperative_tail() {
        let bootstrap = [2, 0, 1, 3];
        let learned = [1, 0, 3, 2];
        let specialist = AllocationSource::Specialist(SpecialistRevisionId::new([7; 32]));
        let allocations = [
            (2, AllocationSource::Bootstrap),
            (3, specialist),
            (0, AllocationSource::Bootstrap),
            (1, AllocationSource::Bootstrap),
        ];

        let (order, sources) =
            merge_intelligence_allocations(&bootstrap, Some(&learned), &allocations);

        assert_eq!(order, [2, 3, 1, 0]);
        assert_eq!(sources[2], crate::policy::AllocationQueue::Bootstrap);
        assert_eq!(sources[3], crate::policy::AllocationQueue::Learned);
        assert_eq!(sources[1], crate::policy::AllocationQueue::Learned);
        assert_eq!(sources[0], crate::policy::AllocationQueue::Learned);
    }

    #[test]
    fn empty_ecology_does_not_erase_the_legacy_learned_order() {
        let bootstrap = [0, 1, 2, 3];
        let learned = [3, 2, 1, 0];
        let allocations = bootstrap
            .iter()
            .copied()
            .map(|index| (index, AllocationSource::Bootstrap))
            .collect::<Vec<_>>();

        let (order, sources) =
            merge_intelligence_allocations(&bootstrap, Some(&learned), &allocations);

        assert_eq!(order, [0, 3, 2, 1]);
        assert_eq!(sources[0], crate::policy::AllocationQueue::Bootstrap);
        assert_eq!(sources[3], crate::policy::AllocationQueue::Learned);
    }

    #[test]
    fn cooperative_tail_cannot_reinsert_a_resource_infeasible_candidate() {
        let bootstrap = [0, 1];
        let allocations = [(1, AllocationSource::Bootstrap)];

        let (order, sources) = merge_intelligence_allocations(&bootstrap, None, &allocations);

        assert_eq!(order, [1]);
        assert_eq!(sources[1], crate::policy::AllocationQueue::Bootstrap);
    }

    #[test]
    fn generation_refills_lookahead_only_after_deferred_inventory_drains() {
        assert_eq!(generation_refill_limit(144, 229), 0);
        assert_eq!(generation_refill_limit(144, 144), 0);
        assert_eq!(generation_refill_limit(144, 136), 8);
        assert_eq!(generation_refill_limit(144, 0), 144);
    }

    #[test]
    fn moved_deferred_payload_is_counted_once_in_the_prospective_transaction() {
        assert_eq!(moved_tail_transaction_peak(100, 30, 20), 150);
    }

    #[test]
    fn truncated_operator_progress_keeps_the_parent_pending() {
        let key = crate::ArtifactKey([7; 32]);
        let mut pending = vec![PendingParent::new(key, 2, false)];
        let mut staged = pending[0].clone();
        staged.primitive_offsets = vec![8, ENUMERATION_COMPLETE];

        commit_pending_progress(&mut pending, vec![staged]);

        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].primitive_offsets, [8, ENUMERATION_COMPLETE]);
    }

    #[test]
    fn completely_enumerated_parent_leaves_the_pending_tail() {
        let key = crate::ArtifactKey([9; 32]);
        let mut pending = vec![PendingParent::new(key, 2, false)];
        let mut staged = pending[0].clone();
        staged.primitive_offsets = vec![ENUMERATION_COMPLETE; 2];

        commit_pending_progress(&mut pending, vec![staged]);

        assert!(pending.is_empty());

        let completed = PendingParent {
            key,
            primitive_offsets: vec![ENUMERATION_COMPLETE; 2],
            derived_sampled: true,
        };
        commit_pending_progress(&mut pending, vec![completed]);
        assert!(
            pending.is_empty(),
            "republication of completion is idempotent"
        );
    }

    #[test]
    fn action_barrier_preserves_derived_sampling_after_primitive_exhaustion() {
        let key = crate::ArtifactKey([10; 32]);
        let mut pending = vec![PendingParent::new(key, 2, true)];
        let mut staged = pending[0].clone();
        staged.primitive_offsets = vec![ENUMERATION_COMPLETE; 2];

        // The action barrier publishes a clone: the live epoch must retain its
        // exact cursor so Derived-Operator sampling follows the primitive page.
        commit_pending_progress(&mut pending, vec![staged.clone()]);
        assert_eq!(pending.len(), 1);
        assert!(!pending[0].derived_sampled);
        assert!(
            pending[0]
                .primitive_offsets
                .iter()
                .all(|offset| *offset == ENUMERATION_COMPLETE)
        );

        staged.derived_sampled = true;
        commit_pending_progress(&mut pending, vec![staged]);
        assert!(
            pending.is_empty(),
            "the completed parent cannot loop forever"
        );
    }

    #[test]
    fn incomplete_parent_page_rotates_behind_unvisited_parents() {
        let first = crate::ArtifactKey([1; 32]);
        let second = crate::ArtifactKey([2; 32]);
        let mut pending = vec![
            PendingParent::new(first, 1, false),
            PendingParent::new(second, 1, false),
        ];
        let mut staged = pending[0].clone();
        staged.primitive_offsets[0] = 8;

        commit_pending_progress(&mut pending, vec![staged]);

        assert_eq!(
            pending.iter().map(|parent| parent.key).collect::<Vec<_>>(),
            [second, first]
        );
    }

    #[test]
    fn mandatory_resident_plan_decomposes_the_runtime_reservation_exactly() {
        let (stacks, durability, scratch, fixed) =
            fixed_resident_categories(5, 16 << 30, 1_000, 250);

        assert_eq!(stacks, 10 << 20);
        assert_eq!(durability, 512 << 10);
        assert_eq!(scratch, 1_250);
        assert_eq!(fixed, (16 << 30) + (10 << 20) + (512 << 10) + 2_250);
    }

    #[test]
    fn proposal_features_occupy_a_disjoint_model_feature_channel() {
        let mut combined = Features([0.0; FEATURE_COUNT]);
        combined.0[15] = 7.0;
        append_proposal_features(
            &mut combined,
            ProposalFeatures::new([1.0, 0.5, 0.25, 0.0, -0.25, -0.5, -0.75, -1.0]),
        );

        assert_eq!(combined.0[15].to_bits(), 7.0_f32.to_bits());
        assert_eq!(
            &combined.0[crate::learning::BASE_FEATURE_COUNT..],
            &[1.0, 0.5, 0.25, 0.0, -0.25, -0.5, -0.75, -1.0]
        );
    }

    #[test]
    fn operator_features_separate_a_known_single_bucket_collision() {
        assert_ne!(
            operator_feature_values("probe-zero").map(f32::to_bits),
            operator_feature_values("simplify-known-identity").map(f32::to_bits)
        );
    }

    #[test]
    fn repair_parent_locality_excludes_every_unrelated_frontier_parent() {
        let rejected_parent = crate::ArtifactKey([41; 32]);
        let unrelated_parent = crate::ArtifactKey([42; 32]);

        assert!(primitive_action_parent_eligible(
            Some(rejected_parent),
            rejected_parent
        ));
        assert!(!primitive_action_parent_eligible(
            Some(rejected_parent),
            unrelated_parent
        ));
        assert!(primitive_action_parent_eligible(None, unrelated_parent));
    }

    #[test]
    fn bounded_selection_matches_the_complete_total_order_prefix() {
        for length in 0..257_usize {
            let values = (0..length)
                .map(|index| (index * 73 + 11) % 263)
                .collect::<Vec<_>>();
            for limit in [0, 1, 3, 8, 31, 128, 512] {
                let mut expected = values.clone();
                expected.sort_unstable();
                expected.truncate(limit);
                let mut actual = values.clone();
                sort_prefix_by(&mut actual, limit, Ord::cmp);
                assert_eq!(actual, expected);
            }
        }
    }

    #[test]
    fn protected_claim_exploration_is_complete_or_defers_to_preference() {
        let origins = BTreeSet::from([0, 1, 2, 3]);
        assert_eq!(
            protected_origin_keys(origins.clone(), 8),
            HashSet::from([0, 1, 2, 3])
        );
        assert!(protected_origin_keys(origins, 2).is_empty());
        assert!(protected_origin_keys(BTreeSet::from([0]), 8).is_empty());
    }

    #[test]
    fn policy_shadow_proposal_graph_charges_vector_overcapacity() {
        let proposals = Vec::<super::cohort::CohortProposal>::with_capacity(37);

        assert_eq!(
            policy_proposal_graph_resident_bytes(&proposals),
            37 * std::mem::size_of::<super::cohort::CohortProposal>() as u64
        );
    }

    #[test]
    fn fixed_source_read_rejects_truncation_and_growth_without_buffer_growth() {
        assert_eq!(
            read_fixed_source(&mut Cursor::new([1_u8, 2, 3]), 3)
                .unwrap()
                .as_ref(),
            &[1, 2, 3]
        );
        assert!(read_fixed_source(&mut Cursor::new([1_u8, 2]), 3).is_err());
        assert!(read_fixed_source(&mut Cursor::new([1_u8, 2, 3, 4]), 3).is_err());
    }

    #[test]
    fn artifact_payload_sealing_is_a_pure_copy_of_cached_records() {
        let records: [&[u8]; 2] = [&[1, 2, 3], &[4, 5]];
        let payload = copy_cached_artifact_records(records.into_iter(), 2, 13).unwrap();

        assert_eq!(&payload[..8], &2_u64.to_le_bytes());
        assert_eq!(&payload[8..], &[1, 2, 3, 4, 5]);
        assert_eq!(payload.capacity(), 13);
    }
}
