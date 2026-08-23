use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::ControlFlow;
use std::sync::Arc;
#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use reflex_bundle::{CanonicalBundle, SegmentKind};
use sha2::{Digest, Sha256};

use crate::bundle::DomainBundle;
use crate::domain::{
    ApplicationWriter, Candidate, CandidateWriter, ClaimOf, DomainDefinition, EvidenceOf,
    OperatorAlgebra, OperatorEnumerationBatch, ProposalFeatures, ProposalProvenance, Seed,
    SeedSource, SeedWriter, StructuralLocation, StructuralProtocol, StructuralView, Verdict,
    VerificationBatchReport, VerificationKernel, VerificationRecord, VerificationReplayRequest,
    VerificationWorkerRequirements,
};
use crate::durability;
use crate::instrumentation::{Phase, Recorder, ResourceRefusal};
use crate::knowledge::{DerivationObservation, KnowledgeRevision, KnowledgeState};
use crate::learning::{
    AttemptObservation, ConsequenceKind, ConsequenceObservation, FEATURE_COUNT, Features,
    FtrlModel, LearningState, compare_forecasts, derive_targets,
};
use crate::measurement::{Measurement, MeasurementSpace, MeasurementWriter, VerifiedBatch};
use crate::policy::{AllocationQueue, OperationalPartition, operational_ranked_selections};
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
mod scheduler;

use bundle::RestartBundleCodec;
use epoch::EpochTransition;
#[cfg(feature = "internal-experiments")]
use experience::CandidateFateKey;
use experience::{
    CandidateFateDisposition, CandidateFateObservation, CandidateRank, ExperienceEntry,
    ExperienceLedger, ExperienceVerdict, candidate_fate_batches_are_valid,
    encoded_candidate_fates_len, encoded_entry_len,
};
use goals::GoalEvaluator;
use scheduler::{ClaimVerificationRequest, ScheduleError, Scheduler};

const OPERATOR_FEATURE_START: usize = 5;
const OPERATOR_FEATURE_END: usize = 13;
const MIN_ONLINE_TRAINING_EXAMPLES: usize = 32;

struct StoredArtifact<D: DomainDefinition> {
    artifact: D::Artifact,
    verification: VerificationRecord<D>,
    provenance: Vec<u8>,
    origin_key: Option<ArtifactKey>,
    parent_key: Option<ArtifactKey>,
}
type OriginatedStoredArtifact<D> = (StoredArtifact<D>, usize, [u8; 32]);
type ClaimedVerdicts<D> = Vec<(ClaimOf<D>, Verdict<EvidenceOf<D>>)>;
struct RecoveredBundle<D: DomainDefinition> {
    artifacts: Vec<StoredArtifact<D>>,
    pareto_keys: Vec<ArtifactKey>,
    frontier_keys: Vec<ArtifactKey>,
    deferred_candidates: Vec<DeferredCandidate<D>>,
    pending_parents: Vec<PendingParent>,
    ledger: ExperienceLedger,
    revisions: Option<RevisionIds>,
    interrupted_usage: Option<ResourceUsage>,
    knowledge: KnowledgeState,
    learning: LearningState,
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
            ledger: ExperienceLedger::default(),
            revisions: None,
            interrupted_usage: None,
            knowledge: KnowledgeState::default(),
            learning: LearningState::default(),
            resident_bytes: 0,
        }
    }
}

#[derive(Clone, Copy)]
struct RevisionIds {
    knowledge: [u8; 32],
    model: [u8; 32],
}

struct VerificationOutcome<D: DomainDefinition> {
    accepted: Vec<OriginatedStoredArtifact<D>>,
    experience: Vec<ExperienceEntry>,
}

struct ProposedCandidate<D: DomainDefinition> {
    candidate: Candidate<D>,
    canonical_candidate: Vec<u8>,
    operator_symbol: Vec<u8>,
    features: Features,
    epoch: u64,
    proposal_limit: u32,
    protected_derived: bool,
    allocation_queue: AllocationQueue,
    fate_index: usize,
    bootstrap_rank: CandidateRank,
    learned_rank: CandidateRank,
    generated_in_epoch: bool,
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
            features,
            epoch,
            proposal_limit: u32::try_from(proposal_limit).unwrap_or(u32::MAX),
            protected_derived,
            allocation_queue: AllocationQueue::Bootstrap,
            fate_index: usize::MAX,
            bootstrap_rank: CandidateRank::absent(),
            learned_rank: CandidateRank::absent(),
            generated_in_epoch: true,
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

struct SearchTailView<'a, D: DomainDefinition> {
    frontier: &'a [(VerifiedArtifact<D>, usize)],
    deferred_candidates: &'a [ProposedCandidate<D>],
    pending_parents: &'a [PendingParent],
}

struct RestartBundleState<'a, D: DomainDefinition> {
    artifacts: &'a [VerifiedArtifact<D>],
    pareto: &'a [VerifiedArtifact<D>],
    search_tail: SearchTailView<'a, D>,
    ledger: &'a ExperienceLedger,
    knowledge: &'a KnowledgeState,
    learning: &'a LearningState,
}

impl<'a, D: DomainDefinition> RestartBundleState<'a, D> {
    const fn new(
        artifacts: &'a [VerifiedArtifact<D>],
        pareto: &'a [VerifiedArtifact<D>],
        search_tail: SearchTailView<'a, D>,
        ledger: &'a ExperienceLedger,
        knowledge: &'a KnowledgeState,
        learning: &'a LearningState,
    ) -> Self {
        Self {
            artifacts,
            pareto,
            search_tail,
            ledger,
            knowledge,
            learning,
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
        }
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
}

struct GenerationParents<'a, D: DomainDefinition> {
    artifacts: &'a [&'a D::Artifact],
    frontier_indexes: &'a [usize],
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
const CHOICES_PER_VERIFICATION: u64 = 8;
const GENERATION_COHORT_LOOKAHEAD: u64 = 2;
const MIN_CHOICE_RESIDENT_BYTES: u64 = 8 * 1024;
const MAX_CANDIDATE_CHOICES: u64 = 16_384;

pub(crate) struct DomainResourcePlan {
    requirements: VerificationWorkerRequirements,
    pub(crate) requested_worker_threads: usize,
    pub(crate) external_worker_lanes: usize,
    pub(crate) runtime_worker_lanes: usize,
    pub(crate) runtime_stack_bytes: u64,
    pub(crate) durability_stack_bytes: u64,
    pub(crate) external_worker_bytes: u64,
    pub(crate) operator_bytes: u64,
    pub(crate) maximum_candidate_capacity: usize,
    pub(crate) operator_scratch_bytes_per_lane: u64,
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
    let (runtime_stack_bytes, durability_stack_bytes, operator_scratch_bytes, fixed_resident_bytes) =
        fixed_resident_categories(
            runtime_worker_lanes,
            external_worker_bytes,
            operator_bytes,
            operator_scratch_bytes_per_lane,
        );
    Some(DomainResourcePlan {
        requirements,
        requested_worker_threads,
        external_worker_lanes: requirements.worker_lanes(),
        runtime_worker_lanes,
        runtime_stack_bytes,
        durability_stack_bytes,
        external_worker_bytes,
        operator_bytes,
        maximum_candidate_capacity,
        operator_scratch_bytes_per_lane,
        operator_scratch_bytes,
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
const RUNTIME_REVISION: u64 = 19;
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
    if read_bundle_u64(&mut input)? != RUNTIME_REVISION {
        return Err(SessionError::IncompatibleBundle);
    }
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
    let comparison = recovered
        .learning
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
    let bundle_codec = RestartBundleCodec::new(domain, request);
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
    }
    let recovered_has_search_tail = !recovered_bundle.deferred_candidates.is_empty()
        || !recovered_bundle.pending_parents.is_empty();
    let recovered_resident_bytes = recovered_bundle.resident_bytes;
    let recovered_keys = recovered_bundle.pareto_keys;
    let recovered_frontier_keys = recovered_bundle.frontier_keys;
    let recovered_deferred = recovered_bundle.deferred_candidates;
    let recovered_pending_parents = recovered_bundle.pending_parents;
    let recovered_stored = recovered_bundle.artifacts;
    let mut ledger = recovered_bundle.ledger;
    let mut knowledge = recovered_bundle.knowledge;
    let pinned_knowledge = knowledge.pinned_revision().clone();
    let mut learning = recovered_bundle.learning;
    let mut active_model = learning.pinned_model().cloned();
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
        .saturating_add(vector_bytes(&seeds));
    let failure_publication_bytes = if verification_workers.worker_lanes() == 0 {
        0
    } else if let Some(source) = request.bundle.source() {
        std::fs::metadata(source)
            .map_err(SessionError::Durability)?
            .len()
            .saturating_mul(3)
    } else {
        let usage = resource_meter
            .usage(verification_requests, 0)
            .map_err(|()| SessionError::Resource)?;
        let probe = bundle_codec.seal(
            &seed_cursor,
            RestartBundleState::new(
                &[],
                &[],
                SearchTailView::new(&[], &[], &[]),
                &ledger,
                &knowledge,
                &learning,
            ),
            SessionSeal::Interrupted(usage),
        )?;
        if !resource_meter.checkpoint_fits(probe.len() as u64) {
            return Err(SessionError::Resource);
        }
        probe.capacity() as u64
    };
    if !resource_meter.reserve(
        ResidentReservation::live(recovery_resident).with_transient(failure_publication_bytes),
    ) {
        return Err(SessionError::Resource);
    }
    let stored_replay = replay_stored(
        domain,
        &recovered_stored,
        scheduler,
        resource_meter,
        verification_workers,
        recovery_resident,
    );
    if let Err(error) = stored_replay {
        if verification_workers.worker_lanes() != 0 {
            let usage = resource_meter
                .usage(verification_requests, 0)
                .map_err(|()| SessionError::Resource)?;
            persist_setup_interruption(
                domain,
                request,
                &bundle_codec,
                &seed_cursor,
                &ledger,
                &knowledge,
                &learning,
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
        if verification_workers.worker_lanes() != 0 {
            let usage = resource_meter
                .usage(verification_requests, 0)
                .map_err(|()| SessionError::Resource)?;
            persist_setup_interruption(
                domain,
                request,
                &bundle_codec,
                &seed_cursor,
                &ledger,
                &knowledge,
                &learning,
                usage,
                resource_meter,
                recovery_resident,
            )?;
        }
        return Err(error);
    }
    let environment = crate::MeasurementEnvironment::local_process();
    let recovered = materialize(domain, recovered_stored, &environment)?;
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
        if verification_workers.worker_lanes() != 0 {
            let usage = resource_meter
                .usage(verification_requests, 0)
                .map_err(|()| SessionError::Resource)?;
            persist_setup_interruption(
                domain,
                request,
                &bundle_codec,
                &seed_cursor,
                &ledger,
                &knowledge,
                &learning,
                usage,
                resource_meter,
                recovery_resident,
            )?;
        }
        return Err(error);
    }
    if recovered_revisions.is_some_and(|revisions| {
        let expected = revision_ids(
            domain.semantic_identity().as_str(),
            &recovered,
            &knowledge,
            &learning,
        );
        revisions.knowledge != expected.knowledge || revisions.model != expected.model
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
    let roots = materialize(domain, seed_stored, &environment)?;
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
                features: Features([0.0; FEATURE_COUNT]),
                epoch: 0,
                proposal_limit: deferred.proposal_limit,
                protected_derived: deferred.protected_derived,
                allocation_queue: AllocationQueue::Bootstrap,
                fate_index: usize::MAX,
                bootstrap_rank: CandidateRank::absent(),
                learned_rank: CandidateRank::absent(),
                generated_in_epoch: false,
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
    let mut checkpoint = bundle_codec.seal(
        &seed_cursor,
        RestartBundleState::new(
            &known,
            &pareto,
            SearchTailView::new(&frontier, &deferred_candidates, &pending_parents),
            &ledger,
            &knowledge,
            &learning,
        ),
        SessionSeal::Interrupted(initial_usage),
    )?;
    if !resource_meter.checkpoint_fits(checkpoint.len() as u64) {
        return Err(SessionError::Resource);
    }
    let operators = domain
        .operators()
        .catalog()
        .iter()
        .map(crate::OperatorDescriptor::operator)
        .collect::<Vec<_>>();
    let mut parent_ranks = goal_evaluator.parent_ranks(&frontier);
    let initial_resident = worker_resident_bytes
        .saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &checkpoint,
            &ledger,
            &knowledge,
            &learning,
        ))
        .saturating_add(cohort::recovery_resident_bytes(
            domain,
            &deferred_candidates,
            &pending_parents,
        ));
    if !resource_meter.reserve(
        ResidentReservation::live(initial_resident).with_transient(checkpoint.capacity() as u64),
    ) {
        return Err(SessionError::Resource);
    }
    let mut durability = durability::CheckpointWriter::start(request.bundle.target().to_path_buf())
        .map_err(SessionError::Durability)?;
    durability
        .submit(checkpoint.clone())
        .and_then(|()| durability.barrier().map(|_| ()))
        .map_err(SessionError::Durability)?;
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
    let mut selection_epoch = ledger
        .entries()
        .iter()
        .map(|entry| entry.epoch)
        .chain(ledger.candidate_fates().iter().map(|fate| fate.epoch))
        .max()
        .map_or(Some(0_u64), |epoch| epoch.checked_add(1))
        .ok_or(SessionError::CorruptBundle)?;
    let mut operator_scratch = <D::Operators as OperatorAlgebra<D>>::Scratch::default();
    instrumentation.finish(Phase::Setup, setup_started);

    while !stopped_by_observer
        && !success_conditions_satisfied
        && !time_exhausted
        && !resident_budget_exhausted
        && !frontier.is_empty()
    {
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
                &knowledge,
                &learning,
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
            &parent_claims,
            &covered_claims,
        );
        let catalog = domain.operators().catalog();
        let available_resident = resource_meter.available_resident(resident_before_epoch);
        let generation_inventory_target = candidate_generation_limit(
            cohort_limit,
            remaining_verifications,
            pending_parent_indexes.len(),
            catalog.len(),
            available_resident,
        );
        let generation_limit =
            generation_refill_limit(generation_inventory_target, deferred_candidates.len());
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
            generation_limit.div_ceil(4)
        } else {
            0
        };
        let primitive_budget = generation_limit.saturating_sub(derived_budget);
        let primitive_parent_capacity = generation::parent_capacity(
            primitive_budget,
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
        let mut remaining_primitive_budget = primitive_budget;
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
            let primitive_bytes = append_primitive_candidates(
                domain,
                &GenerationParents {
                    artifacts: &parent_artifacts,
                    frontier_indexes: &parent_indexes,
                },
                &mut operator_scratch,
                &mut progress.primitive_offsets,
                parent_budget,
                candidate_epoch,
                &mut candidates,
            )?;
            remaining_primitive_budget =
                remaining_primitive_budget.saturating_sub(candidates.len() - before);
            application_bytes = application_bytes.max(primitive_bytes);
        }
        if time_exhausted {
            deferred_candidates = cohort::rollback_unverified_generation(candidates, Vec::new());
            break;
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
            let (derived_bytes, _derived_truncated) = append_derived_candidates(
                domain,
                &GenerationParents {
                    artifacts: &parent_artifacts,
                    frontier_indexes: &parent_indexes,
                },
                &pinned_knowledge,
                &mut operator_scratch,
                parent_budget,
                candidate_epoch,
                &mut candidates,
            )?;
            remaining_derived_budget =
                remaining_derived_budget.saturating_sub(candidates.len() - before);
            application_bytes = application_bytes.max(derived_bytes);
            staged_parent_progress[parent_position].derived_sampled = true;
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
        deferred_candidates = order_by_learned_potential(
            parent_ranks.as_slice(),
            &frontier,
            &covered_claims,
            active_model.as_ref(),
            cohort_limit,
            &mut candidates,
            &mut candidate_fates,
        );
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
            .saturating_add(vector_bytes(&candidates))
            .saturating_add(candidate_pipeline_reserve(domain, &candidates))
            .saturating_add(vector_bytes(&candidate_fates));
        let prospective_experience =
            prospective_experience_encoded_len(&ledger, &candidates, &candidate_fates);
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
            &knowledge,
            &learning,
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
        instrumentation.verified(candidates.len());
        verification_requests +=
            u64::try_from(candidates.len()).map_err(|_| SessionError::Resource)?;
        let parent_keys = frontier
            .iter()
            .map(|(artifact, _)| artifact.key())
            .collect::<Vec<_>>();
        let verification_started = instrumentation.start();
        let verification_result = verify_candidates(
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
        );
        let verification = match verification_result {
            Ok(verification) => verification,
            Err(error) => {
                if verification_workers.worker_lanes() != 0 {
                    let usage = resource_meter
                        .usage(verification_requests, checkpoint.len() as u64)
                        .map_err(|()| SessionError::Resource)?;
                    persist_active_interruption(
                        domain,
                        request,
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
        let experience_before_cohort = ledger.len();
        let experience_checkpoint_state = ledger.checkpoint_entries();
        let candidate_fate_checkpoint = ledger.checkpoint_candidate_fates();
        let measurement_checkpoint = ledger.checkpoint_measurements();
        commit_pending_progress(&mut pending_parents, staged_parent_progress);
        ledger.append_entries(verification.experience);
        ledger.append_candidate_fates(candidate_fates);
        test_fault_point("experience-appended");
        let checkpoint_usage = resource_meter
            .usage(verification_requests, checkpoint.len() as u64)
            .map_err(|()| SessionError::Resource)?;
        let experience_checkpoint = bundle_codec.seal(
            &seed_cursor,
            RestartBundleState::new(
                &known,
                &pareto,
                SearchTailView::new(&frontier, &deferred_candidates, &pending_parents),
                &ledger,
                &knowledge,
                &learning,
            ),
            SessionSeal::Interrupted(checkpoint_usage),
        )?;
        if !resource_meter.checkpoint_fits(experience_checkpoint.len() as u64) {
            ledger.rollback_entries(experience_checkpoint_state);
            ledger.rollback_candidate_fates(candidate_fate_checkpoint);
            return Err(SessionError::Resource);
        }
        let experience_live = worker_resident_bytes.saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &experience_checkpoint,
            &ledger,
            &knowledge,
            &learning,
        ));
        let experience_live = experience_live.saturating_add(deferred_resident);
        if !resource_meter.reserve(
            ResidentReservation::live(experience_live)
                .with_transient(experience_checkpoint.capacity() as u64)
                .with_pending_durability(durability.pending_bytes()),
        ) {
            ledger.rollback_entries(experience_checkpoint_state);
            ledger.rollback_candidate_fates(candidate_fate_checkpoint);
            return Err(SessionError::Resource);
        }
        let measurement_admission_started = instrumentation.start();
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
            verification
                .accepted
                .into_iter()
                .map(|(artifact, _, _)| artifact)
                .collect(),
            &environment,
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
            let online_learning_live = worker_resident_bytes
                .saturating_add(resident_state_bytes(
                    &known,
                    &roots,
                    &pareto,
                    &frontier,
                    &recovered_keys,
                    &operators,
                    &checkpoint,
                    &ledger,
                    &knowledge,
                    &learning,
                ))
                .saturating_add(deferred_resident);
            let training_started = instrumentation.start();
            let proposed_learning = online_learning_candidate(
                &ledger,
                &learning,
                experience_before_cohort,
                resource_meter,
                online_learning_live,
                durability.pending_bytes(),
            );
            instrumentation.finish(Phase::Training, training_started);
            let checkpoint_learning = proposed_learning.as_ref().unwrap_or(&learning);
            let cohort_usage = resource_meter
                .usage(verification_requests, checkpoint.len() as u64)
                .map_err(|()| SessionError::Resource)?;
            let cohort_checkpoint = bundle_codec.seal(
                &seed_cursor,
                RestartBundleState::new(
                    &known,
                    &pareto,
                    SearchTailView::new(&frontier, &deferred_candidates, &pending_parents),
                    &ledger,
                    &knowledge,
                    checkpoint_learning,
                ),
                SessionSeal::Interrupted(cohort_usage),
            )?;
            let cohort_live = worker_resident_bytes
                .saturating_add(resident_state_bytes(
                    &known,
                    &roots,
                    &pareto,
                    &frontier,
                    &recovered_keys,
                    &operators,
                    &cohort_checkpoint,
                    &ledger,
                    &knowledge,
                    checkpoint_learning,
                ))
                .saturating_add(deferred_resident);
            let replaced_learning_bytes = proposed_learning
                .as_ref()
                .map_or(0, |_| learning.resident_bytes());
            let cohort_reservation = ResidentReservation::live(cohort_live)
                .with_transient(
                    (cohort_checkpoint.capacity() as u64).saturating_add(replaced_learning_bytes),
                )
                .with_pending_durability(durability.pending_bytes());
            if !resource_meter.checkpoint_fits(cohort_checkpoint.len() as u64) {
                ledger.rollback_entries(experience_checkpoint_state);
                ledger.rollback_candidate_fates(candidate_fate_checkpoint);
                ledger.rollback_measurements(measurement_checkpoint);
                return Err(SessionError::Resource);
            }
            if !resource_meter.reserve(cohort_reservation) {
                ledger.rollback_entries(experience_checkpoint_state);
                ledger.rollback_candidate_fates(candidate_fate_checkpoint);
                ledger.rollback_measurements(measurement_checkpoint);
                return Err(SessionError::Resource);
            }
            durability
                .submit(cohort_checkpoint.clone())
                .and_then(|()| durability.barrier().map(|_| ()))
                .map_err(SessionError::Durability)?;
            test_fault_point("cohort-published");
            if let Some(proposed_learning) = proposed_learning {
                learning = proposed_learning;
                active_model = learning.pinned_model().cloned();
            }
            checkpoint = cohort_checkpoint;
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
        let online_learning_live = worker_resident_bytes
            .saturating_add(resident_state_bytes(
                epoch.known(),
                &roots,
                &proposed_pareto,
                epoch.frontier(),
                &recovered_keys,
                &operators,
                &checkpoint,
                epoch.ledger(),
                &knowledge,
                &learning,
            ))
            .saturating_add(cohort::recovery_resident_bytes(
                domain,
                &deferred_candidates,
                &pending_parents,
            ));
        let training_started = instrumentation.start();
        let proposed_learning = online_learning_candidate(
            epoch.ledger(),
            &learning,
            experience_before_cohort,
            resource_meter,
            online_learning_live,
            durability.pending_bytes(),
        );
        instrumentation.finish(Phase::Training, training_started);
        let checkpoint_learning = proposed_learning.as_ref().unwrap_or(&learning);
        let checkpoint_usage = resource_meter
            .usage(verification_requests, checkpoint.len() as u64)
            .map_err(|()| SessionError::Resource)?;
        let proposed_checkpoint = bundle_codec.seal(
            &seed_cursor,
            RestartBundleState::new(
                epoch.known(),
                &proposed_pareto,
                SearchTailView::new(epoch.frontier(), &deferred_candidates, &pending_parents),
                epoch.ledger(),
                &knowledge,
                checkpoint_learning,
            ),
            SessionSeal::Interrupted(checkpoint_usage),
        )?;
        if !resource_meter.checkpoint_fits(proposed_checkpoint.len() as u64) {
            drop(epoch);
            ledger.rollback_entries(experience_checkpoint_state);
            ledger.rollback_candidate_fates(candidate_fate_checkpoint);
            ledger.rollback_measurements(measurement_checkpoint);
            return Err(SessionError::Resource);
        }
        let proposed_live = worker_resident_bytes.saturating_add(resident_state_bytes(
            epoch.known(),
            &roots,
            &proposed_pareto,
            epoch.frontier(),
            &recovered_keys,
            &operators,
            &proposed_checkpoint,
            epoch.ledger(),
            &knowledge,
            checkpoint_learning,
        ));
        let proposed_live = proposed_live.saturating_add(cohort::recovery_resident_bytes(
            domain,
            &deferred_candidates,
            &pending_parents,
        ));
        let replaced_learning_bytes = proposed_learning
            .as_ref()
            .map_or(0, |_| learning.resident_bytes());
        let proposed_reservation = ResidentReservation::live(proposed_live)
            .with_transient(
                (proposed_checkpoint.capacity() as u64).saturating_add(replaced_learning_bytes),
            )
            .with_pending_durability(durability.pending_bytes());
        if !resource_meter.reserve(proposed_reservation) {
            drop(epoch);
            ledger.rollback_entries(experience_checkpoint_state);
            ledger.rollback_candidate_fates(candidate_fate_checkpoint);
            ledger.rollback_measurements(measurement_checkpoint);
            return Err(SessionError::Resource);
        }
        durability
            .submit(proposed_checkpoint.clone())
            .and_then(|()| durability.barrier().map(|_| ()))
            .map_err(SessionError::Durability)?;
        test_fault_point("pareto-published");
        epoch.commit();
        if let Some(proposed_learning) = proposed_learning {
            learning = proposed_learning;
            active_model = learning.pinned_model().cloned();
        }
        goal_evaluator.extend_parent_ranks(&frontier, &mut parent_ranks);
        instrumentation.finish(Phase::MeasurementAdmission, measurement_admission_started);
        pareto = proposed_pareto;
        checkpoint = proposed_checkpoint;
        let delivery = deliver_delta(
            &mut observer,
            &mut sequence,
            &previous_keys,
            &pareto,
            &goal_evaluator.affected(&previous_goal_frontiers, &proposed_goal_frontiers),
            resource_meter,
            proposed_reservation.peak_bytes(),
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
    let consolidation_live = worker_resident_bytes
        .saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &checkpoint,
            &ledger,
            &knowledge,
            &learning,
        ))
        .saturating_add(cohort::recovery_resident_bytes(
            domain,
            &deferred_candidates,
            &pending_parents,
        ));
    let consolidation_transient = (ledger.len() as u64)
        .saturating_mul((std::mem::size_of::<DerivationObservation>() as u64).saturating_add(512))
        .saturating_add(4_096 * 64)
        .saturating_add(ledger.entries().iter().fold(0_u64, |bytes, entry| {
            bytes.saturating_add((entry.operator_symbol.capacity() as u64).saturating_mul(2))
        }));
    let can_consolidate = completion != Completion::StoppedByObserver
        && !durable_budget_exhausted
        && !resource_meter
            .time_exhausted()
            .map_err(|()| SessionError::Resource)?
        && resource_meter.reserve(
            ResidentReservation::live(consolidation_live)
                .with_transient(consolidation_transient)
                .with_pending_durability(durability.pending_bytes()),
        );
    if can_consolidate {
        let primitive_symbols = domain
            .operators()
            .catalog()
            .iter()
            .map(|descriptor| descriptor.symbol().as_str().as_bytes().to_vec())
            .collect::<BTreeSet<_>>();
        let observations = ledger.derivations(&pinned_knowledge, &primitive_symbols)?;
        let prior_operators = knowledge
            .pinned_revision()
            .operators()
            .iter()
            .map(crate::knowledge::DerivedOperator::id)
            .collect::<BTreeSet<_>>();
        let mut proposed_knowledge = knowledge.clone();
        let _decision = proposed_knowledge.consolidate(
            &observations,
            roots.iter().map(|artifact| artifact.key().0),
            pareto.iter().map(|artifact| artifact.key().0),
        );
        let compressed_attempts = proposed_knowledge
            .pinned_revision()
            .operators()
            .iter()
            .filter(|operator| !prior_operators.contains(&operator.id()))
            .flat_map(|operator| operator.support().iter().copied())
            .collect::<Vec<_>>();
        let mut proposed_consequences = ledger.consequences().to_vec();
        for subject in compressed_attempts {
            let consequence = ConsequenceObservation {
                subject,
                kind: ConsequenceKind::Compression,
            };
            if !proposed_consequences.contains(&consequence) {
                proposed_consequences.push(consequence);
            }
        }
        let promoted_live = worker_resident_bytes
            .saturating_add(resident_state_bytes(
                &known,
                &roots,
                &pareto,
                &frontier,
                &recovered_keys,
                &operators,
                &checkpoint,
                &ledger,
                &proposed_knowledge,
                &learning,
            ))
            .saturating_sub(ledger.resident_bytes())
            .saturating_add(ledger.resident_bytes_with_consequences(&proposed_consequences))
            .saturating_add(knowledge.resident_bytes())
            .saturating_add(cohort::recovery_resident_bytes(
                domain,
                &deferred_candidates,
                &pending_parents,
            ));
        let promoted_transient = (observations.len() as u64)
            .saturating_mul(std::mem::size_of::<DerivationObservation>() as u64);
        if resource_meter.reserve(
            ResidentReservation::live(promoted_live)
                .with_transient(promoted_transient)
                .with_pending_durability(durability.pending_bytes()),
        ) {
            knowledge = proposed_knowledge;
            ledger.replace_consequences(proposed_consequences);
        } else {
            completion = Completion::ResourceEnvelopeExhausted;
        }
    } else if completion != Completion::StoppedByObserver {
        completion = Completion::ResourceEnvelopeExhausted;
    }
    reconcile_derived_progress(
        &mut pending_parents,
        &pinned_knowledge,
        knowledge.pinned_revision(),
    );
    instrumentation.finish(Phase::Consolidation, consolidation_started);
    let training_started = instrumentation.start();
    let learning_live = worker_resident_bytes
        .saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &checkpoint,
            &ledger,
            &knowledge,
            &learning,
        ))
        .saturating_add(cohort::recovery_resident_bytes(
            domain,
            &deferred_candidates,
            &pending_parents,
        ));
    let learning_transient = (ledger.len() as u64)
        .saturating_mul(std::mem::size_of::<AttemptObservation>() as u64)
        .saturating_add(LearningState::training_scratch_bytes(ledger.len()));
    let can_train = !durable_budget_exhausted
        && !resource_meter
            .time_exhausted()
            .map_err(|()| SessionError::Resource)?
        && resource_meter.reserve(
            ResidentReservation::live(learning_live)
                .with_transient(learning_transient)
                .with_pending_durability(durability.pending_bytes()),
        );
    if can_train {
        let attempts = ledger.attempts();
        let mut examples = derive_targets(&attempts, ledger.consequences());
        let _promotion = learning.learn(&mut examples);
    } else {
        completion = Completion::ResourceEnvelopeExhausted;
    }
    instrumentation.finish(Phase::Training, training_started);
    let finalization_started = instrumentation.start();
    let (sealed_deferred, sealed_pending) = if completion == Completion::ResourceEnvelopeExhausted {
        (&deferred_candidates[..], &pending_parents[..])
    } else {
        (&[][..], &[][..])
    };
    let provisional_checkpoint = bundle_codec.seal(
        &seed_cursor,
        RestartBundleState::new(
            &known,
            &pareto,
            SearchTailView::new(&frontier, sealed_deferred, sealed_pending),
            &ledger,
            &knowledge,
            &learning,
        ),
        SessionSeal::Completed(completion, provisional_usage),
    )?;
    let usage = resource_meter
        .usage(verification_requests, provisional_checkpoint.len() as u64)
        .map_err(|()| SessionError::Resource)?;
    checkpoint = bundle_codec.seal(
        &seed_cursor,
        RestartBundleState::new(
            &known,
            &pareto,
            SearchTailView::new(&frontier, sealed_deferred, sealed_pending),
            &ledger,
            &knowledge,
            &learning,
        ),
        SessionSeal::Completed(completion, usage),
    )?;
    if !resource_meter.checkpoint_fits(checkpoint.len() as u64) {
        return Err(SessionError::Resource);
    }
    let final_live = worker_resident_bytes
        .saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &checkpoint,
            &ledger,
            &knowledge,
            &learning,
        ))
        .saturating_add(cohort::recovery_resident_bytes(
            domain,
            &deferred_candidates,
            &pending_parents,
        ));
    if !resource_meter.reserve(
        ResidentReservation::live(final_live)
            .with_transient(checkpoint.capacity() as u64)
            .with_pending_durability(durability.pending_bytes()),
    ) {
        return Err(SessionError::Resource);
    }
    durability
        .submit(checkpoint.clone())
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
    knowledge: &KnowledgeState,
    learning: &LearningState,
) -> u64 {
    let parent_preference_bytes = (frontier.capacity() as u64).saturating_mul(
        (std::mem::size_of::<ArtifactKey>() + 2 * std::mem::size_of::<usize>()) as u64,
    );
    let records = known.iter().fold(0_u64, |bytes, artifact| {
        bytes
            .saturating_add(std::mem::size_of::<VerifiedArtifactRecord<D>>() as u64)
            .saturating_add(artifact.inner.dynamic_resident_bytes)
            .saturating_add(artifact.inner.claim_canonical.len() as u64)
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
        .saturating_add(knowledge.resident_bytes())
        .saturating_add(learning.resident_bytes())
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
    8_u64
        .saturating_add((pareto.len() as u64).saturating_mul(32))
        .saturating_add(8)
        .saturating_add((frontier.len() as u64).saturating_mul(32))
        .saturating_add(8)
        .saturating_add(deferred_bytes)
        .saturating_add(8)
        .saturating_add(pending_bytes)
}

fn candidate_generation_limit(
    cohort_limit: usize,
    remaining_verifications: u64,
    pending_parent_count: usize,
    operator_count: usize,
    available_resident: u64,
) -> usize {
    let cohort_limit = u64::try_from(cohort_limit).unwrap_or(u64::MAX);
    let lookahead_parents = cohort_limit
        .saturating_mul(GENERATION_COHORT_LOOKAHEAD)
        .min(remaining_verifications)
        .min(u64::try_from(pending_parent_count).unwrap_or(u64::MAX));
    let cohort_choices = cohort_limit.saturating_mul(CHOICES_PER_VERIFICATION);
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

fn online_training_due(previous_examples: usize, current_examples: usize) -> bool {
    fn checkpoint(examples: usize) -> u32 {
        if examples < MIN_ONLINE_TRAINING_EXAMPLES {
            0
        } else {
            usize::BITS - examples.leading_zeros()
        }
    }

    checkpoint(current_examples) > checkpoint(previous_examples)
}

fn learning_transient_bytes(ledger: &ExperienceLedger, learning: &LearningState) -> u64 {
    (ledger.len() as u64)
        .saturating_mul(std::mem::size_of::<AttemptObservation>() as u64)
        .saturating_add(LearningState::training_scratch_bytes(ledger.len()))
        .saturating_add(learning.resident_bytes())
}

fn online_learning_candidate(
    ledger: &ExperienceLedger,
    learning: &LearningState,
    previous_examples: usize,
    resource_meter: &ResourceEnvelopeGuard,
    live_bytes: u64,
    pending_durability: u64,
) -> Option<LearningState> {
    if !online_training_due(previous_examples, ledger.len())
        || !resource_meter.reserve(
            ResidentReservation::live(live_bytes)
                .with_transient(learning_transient_bytes(ledger, learning))
                .with_pending_durability(pending_durability),
        )
    {
        return None;
    }
    let mut challenger = learning.clone();
    let attempts = ledger.attempts();
    let mut examples = derive_targets(&attempts, ledger.consequences());
    let _promotion = challenger.learn(&mut examples);
    Some(challenger)
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

#[cfg(feature = "internal-experiments")]
fn bounded_usize_f32(value: usize) -> f32 {
    bounded_u32_f32(u32::try_from(value).unwrap_or(u32::MAX))
}

#[cfg(feature = "internal-experiments")]
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
        staged_keys
            .iter()
            .all(|key| pending.iter().any(|retained| retained.key == *key)),
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

fn append_primitive_candidates<D: DomainDefinition>(
    domain: &D,
    parents: &GenerationParents<'_, D>,
    scratch: &mut <D::Operators as OperatorAlgebra<D>>::Scratch,
    operator_offsets: &mut [u64],
    limit: usize,
    epoch: u64,
    output: &mut Vec<ProposedCandidate<D>>,
) -> Result<u64, SessionError<D::Error>> {
    let mut application_bytes = 0_u64;
    let mut remaining = limit;
    let catalog = domain.operators().catalog();
    assert_eq!(
        operator_offsets.len(),
        catalog.len(),
        "pending primitive cursor must match the installed Operator catalog"
    );
    let locations = root_locations(domain, parents.artifacts);
    for (operator_index, descriptor) in catalog.iter().enumerate() {
        if remaining == 0 {
            break;
        }
        let offset = operator_offsets[operator_index];
        if offset == ENUMERATION_COMPLETE {
            continue;
        }
        let incomplete_operators = operator_offsets[operator_index..]
            .iter()
            .filter(|offset| **offset != ENUMERATION_COMPLETE)
            .count();
        let operator_limit = remaining.div_ceil(incomplete_operators);
        let skip = usize::try_from(offset).map_err(|_| SessionError::CorruptBundle)?;
        let mut applications = Vec::new();
        let mut application_writer =
            ApplicationWriter::with_window(&mut applications, skip, operator_limit);
        domain
            .operators()
            .enumerate_legal(
                OperatorEnumerationBatch::new(
                    parents.artifacts,
                    &locations,
                    std::slice::from_ref(&descriptor.operator()),
                ),
                &mut application_writer,
                scratch,
            )
            .map_err(SessionError::Domain)?;
        assert!(
            application_writer.consumed_prefix(),
            "OperatorAlgebra::enumerate_legal ended before the retained Primitive Enumeration Cursor"
        );
        let has_more = application_writer.overflowed();
        operator_offsets[operator_index] = if has_more {
            offset
                .checked_add(u64::try_from(applications.len()).map_err(|_| SessionError::Resource)?)
                .ok_or(SessionError::Resource)?
        } else {
            ENUMERATION_COMPLETE
        };
        let mut operator_candidates = Vec::new();
        let mut candidate_writer =
            CandidateWriter::with_limit(&mut operator_candidates, applications.len());
        domain
            .operators()
            .apply_batch(&applications, &mut candidate_writer, scratch)
            .map_err(SessionError::Domain)?;
        assert!(
            !candidate_writer.overflowed() && operator_candidates.len() == applications.len(),
            "OperatorAlgebra::apply_batch must emit exactly one Candidate per legal Application"
        );
        application_bytes = application_bytes
            .max(vector_bytes(&locations).saturating_add(vector_bytes(&applications)))
            .max(vector_bytes(&operator_candidates));
        remaining = remaining.saturating_sub(operator_candidates.len());
        let operator_features = operator_feature_values(descriptor.symbol().as_str());
        for mut candidate in operator_candidates {
            let parent = parents
                .artifacts
                .get(candidate.source_index)
                .ok_or(SessionError::InvalidSeed)?;
            candidate.source_index = *parents
                .frontier_indexes
                .get(candidate.source_index)
                .ok_or(SessionError::InvalidSeed)?;
            let features = opportunity_features(
                domain,
                StructuralSummary {
                    node_count: structural_node_count(domain, parent),
                },
                &candidate.artifact,
                operator_features,
                epoch,
                candidate.proposal_features,
            );
            output.push(ProposedCandidate::generated(
                candidate,
                descriptor.symbol().as_str().as_bytes().to_vec(),
                features,
                epoch,
                operator_limit,
                false,
            ));
        }
    }
    Ok(application_bytes)
}

#[expect(
    clippy::too_many_lines,
    reason = "Derived Operator expansion keeps its bounded multi-step provenance in one search transaction"
)]
fn append_derived_candidates<D: DomainDefinition>(
    domain: &D,
    parents: &GenerationParents<'_, D>,
    knowledge: &KnowledgeRevision,
    scratch: &mut <D::Operators as OperatorAlgebra<D>>::Scratch,
    remaining_verifications: usize,
    epoch: u64,
    output: &mut Vec<ProposedCandidate<D>>,
) -> Result<(u64, bool), SessionError<D::Error>> {
    const MAX_DERIVED_CANDIDATES_PER_OPERATOR: usize = 1_024;

    let mut application_bytes = 0_u64;
    let mut truncated = false;
    let limit = remaining_verifications.min(MAX_DERIVED_CANDIDATES_PER_OPERATOR);
    if limit == 0 {
        return Ok((0, false));
    }
    let mut emitted = 0_usize;
    for derived in knowledge
        .operators()
        .iter()
        .filter(|operator| operator.active())
    {
        let operator_limit = limit.saturating_sub(emitted);
        if operator_limit == 0 {
            break;
        }
        let mut current = Vec::<Candidate<D>>::new();
        for (step_index, step) in derived.steps().iter().enumerate() {
            let Some(descriptor) = domain
                .operators()
                .catalog()
                .iter()
                .find(|descriptor| descriptor.symbol().as_str().as_bytes() == step)
            else {
                return Err(SessionError::CorruptBundle);
            };
            let stage_parents = if step_index == 0 {
                parents.artifacts[..parents.artifacts.len().min(operator_limit)].to_vec()
            } else {
                current
                    .iter()
                    .map(|candidate| &candidate.artifact)
                    .collect()
            };
            let locations = root_locations(domain, &stage_parents);
            let mut applications = Vec::new();
            let mut application_writer =
                ApplicationWriter::with_limit(&mut applications, operator_limit);
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
            truncated |= application_writer.overflowed();
            application_bytes = application_bytes.saturating_add(vector_bytes(&applications));
            let mut next = Vec::new();
            let mut candidate_writer = CandidateWriter::with_limit(&mut next, operator_limit);
            domain
                .operators()
                .apply_batch(&applications, &mut candidate_writer, scratch)
                .map_err(SessionError::Domain)?;
            assert!(
                !candidate_writer.overflowed() && next.len() == applications.len(),
                "OperatorAlgebra::apply_batch must emit exactly one Candidate per legal Application"
            );
            if step_index > 0 {
                for candidate in &mut next {
                    let Some(parent) = current.get(candidate.source_index) else {
                        return Err(SessionError::CorruptBundle);
                    };
                    candidate.source_index = parent.source_index;
                }
            }
            current = next;
            if current.is_empty() {
                break;
            }
        }
        let symbol = std::str::from_utf8(derived.symbol())
            .expect("canonical Derived Operator symbols are UTF-8");
        let operator_features = operator_feature_values(symbol);
        emitted = emitted.saturating_add(current.len());
        for mut candidate in current {
            let parent = parents
                .artifacts
                .get(candidate.source_index)
                .ok_or(SessionError::CorruptBundle)?;
            candidate.source_index = *parents
                .frontier_indexes
                .get(candidate.source_index)
                .ok_or(SessionError::CorruptBundle)?;
            let features = opportunity_features(
                domain,
                StructuralSummary {
                    node_count: structural_node_count(domain, parent),
                },
                &candidate.artifact,
                operator_features,
                epoch,
                candidate.proposal_features,
            );
            output.push(ProposedCandidate::generated(
                candidate,
                derived.symbol().to_vec(),
                features,
                epoch,
                operator_limit,
                derived.protected_exploration(),
            ));
        }
    }
    Ok((application_bytes, truncated))
}

fn order_by_learned_potential<D: DomainDefinition>(
    parent_ranks: &[usize],
    frontier: &[(VerifiedArtifact<D>, usize)],
    covered_claims: &[[u8; 32]],
    model: Option<&FtrlModel>,
    limit: usize,
    candidates: &mut Vec<ProposedCandidate<D>>,
    candidate_fates: &mut [CandidateFateObservation],
) -> Vec<ProposedCandidate<D>> {
    rank_all_candidates(parent_ranks, model, candidates, candidate_fates);
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
    if model.is_none() {
        unprotected.sort_unstable_by_key(bootstrap_rank);
    }
    let mut bootstrap = (0..unprotected.len()).collect::<Vec<_>>();
    if model.is_some() {
        bootstrap.sort_unstable_by_key(|candidate| {
            unprotected[*candidate]
                .bootstrap_rank
                .value()
                .expect("all novel Candidates retain a Bootstrap rank")
        });
    }
    let Some(_) = model else {
        return apply_operational_order(
            origin_exploration,
            derived_exploration,
            unprotected,
            &bootstrap,
            None,
            limit,
            candidates,
        );
    };
    let mut learned = (0..unprotected.len()).collect::<Vec<_>>();
    learned.sort_unstable_by_key(|candidate| {
        unprotected[*candidate]
            .learned_rank
            .value()
            .expect("active-model Candidates retain a learned rank")
    });
    apply_operational_order(
        origin_exploration,
        derived_exploration,
        unprotected,
        &bootstrap,
        Some(&learned),
        limit,
        candidates,
    )
}

fn rank_all_candidates<D: DomainDefinition>(
    parent_ranks: &[usize],
    model: Option<&FtrlModel>,
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
    if let Some(model) = model {
        let ranked_features = candidates
            .iter()
            .map(|candidate| candidate.features)
            .collect::<Vec<_>>();
        let mut forecasts = Vec::with_capacity(ranked_features.len());
        model.forecast_batch(&ranked_features, &mut forecasts);
        let mut global_learned = (0..candidates.len()).collect::<Vec<_>>();
        global_learned.sort_unstable_by(|left, right| {
            compare_forecasts(forecasts[*left], forecasts[*right])
                .then_with(|| {
                    parent_ranks[candidates[*left].candidate.source_index]
                        .cmp(&parent_ranks[candidates[*right].candidate.source_index])
                })
                .then_with(|| {
                    candidates[*left]
                        .operator_symbol
                        .cmp(&candidates[*right].operator_symbol)
                        .then_with(|| {
                            candidates[*left]
                                .candidate
                                .source_index
                                .cmp(&candidates[*right].candidate.source_index)
                        })
                })
        });
        for (rank, candidate) in global_learned.into_iter().enumerate() {
            let rank = CandidateRank::present(
                u32::try_from(rank).expect("Candidate count is bounded below u32::MAX"),
            );
            candidates[candidate].learned_rank = rank;
            candidate_fates[candidates[candidate].fate_index].learned_rank = rank;
        }
    }
}

fn apply_operational_order<D: DomainDefinition>(
    origin: Vec<ProposedCandidate<D>>,
    derived: Vec<ProposedCandidate<D>>,
    unprotected: Vec<ProposedCandidate<D>>,
    bootstrap: &[usize],
    learned: Option<&[usize]>,
    limit: usize,
    output: &mut Vec<ProposedCandidate<D>>,
) -> Vec<ProposedCandidate<D>> {
    let selections =
        operational_ranked_selections(origin.len(), derived.len(), bootstrap, learned, limit);
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
        candidate.allocation_queue = selection.queue;
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
        if candidate.generated_in_epoch {
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
        fates.push(CandidateFateObservation {
            candidate_key,
            claim_digest,
            parent_key: frontier[candidate.candidate.source_index].0.key(),
            operator_digest: Sha256::digest(&candidate.operator_symbol).into(),
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

fn materialize<D: DomainDefinition>(
    domain: &D,
    stored: Vec<StoredArtifact<D>>,
    environment: &crate::MeasurementEnvironment,
) -> Result<Vec<VerifiedArtifact<D>>, SessionError<D::Error>> {
    let artifact_refs = stored
        .iter()
        .map(|stored| &stored.artifact)
        .collect::<Vec<_>>();
    let mut measured = Vec::new();
    let mut measurement_scratch = <D::Measurements as MeasurementSpace<D>>::Scratch::default();
    let expected_measurements = artifact_refs
        .len()
        .checked_mul(domain.measurements().schema().len())
        .ok_or(SessionError::Resource)?;
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
    let mut by_artifact = (0..stored.len())
        .map(|_| Vec::new())
        .collect::<Vec<Vec<Measurement<D::Metric, D::Observation>>>>();
    for measurement in measured {
        let Some(output) = by_artifact.get_mut(measurement.artifact_index) else {
            return Err(SessionError::InvalidSeed);
        };
        output.push(measurement);
    }
    let schema = domain.measurements().schema();
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
    let mut structure_scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    let identity = domain.semantic_identity();
    stored
        .into_iter()
        .zip(by_artifact)
        .map(|(stored, measurements)| {
            let mut canonical = Vec::new();
            domain
                .structure()
                .encode_canonical(&stored.artifact, &mut canonical, &mut structure_scratch)
                .map_err(SessionError::Domain)?;
            let key = ArtifactKey(stable_digest(identity.as_str(), &canonical));
            let mut encoded_claim = Vec::new();
            domain
                .kernel()
                .encode_claim(&stored.verification.claim, &mut encoded_claim)
                .map_err(SessionError::Domain)?;
            let claim_digest = Sha256::digest(&encoded_claim).into();
            let claim_canonical = encoded_claim.into_boxed_slice();
            let dynamic_resident_bytes = domain
                .structure()
                .view(&stored.artifact)
                .dynamic_resident_bytes();
            Ok(VerifiedArtifact {
                inner: Arc::new(VerifiedArtifactRecord {
                    key,
                    claim_digest,
                    claim_canonical,
                    artifact: stored.artifact,
                    verification: stored.verification,
                    origin_key: stored.origin_key.unwrap_or(key),
                    parent_key: stored.parent_key,
                    measurements,
                    environment: environment.clone(),
                    provenance: stored.provenance,
                    dynamic_resident_bytes,
                }),
            })
        })
        .collect()
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
    let bundle_bytes = std::fs::metadata(source)
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
    let bytes = std::fs::read(source).map_err(SessionError::Durability)?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != bundle_bytes {
        return Err(SessionError::Resource);
    }
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
    let interrupted_usage =
        validate_session(domain, request, decoded.segment(SegmentKind::Session))?;
    let encoded_revisions = decoded.segment(SegmentKind::Revisions);
    if encoded_revisions.len() < 80 {
        return Err(SessionError::CorruptBundle);
    }
    let revisions = RevisionIds {
        knowledge: encoded_revisions[..32]
            .try_into()
            .expect("Knowledge Revision ID is exactly 32 bytes"),
        model: encoded_revisions[32..64]
            .try_into()
            .expect("Model Revision ID is exactly 32 bytes"),
    };
    let mut encoded_learning = &encoded_revisions[64..];
    let knowledge = KnowledgeState::decode(take_sized(&mut encoded_learning)?)
        .map_err(|()| SessionError::CorruptBundle)?;
    let learning = LearningState::decode(take_sized(&mut encoded_learning)?)
        .map_err(|()| SessionError::CorruptBundle)?;
    if !encoded_learning.is_empty()
        || learning.revision_digest(domain.semantic_identity().as_str()) != revisions.model
    {
        return Err(SessionError::CorruptBundle);
    }
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
    if !recovery.is_empty() {
        return Err(SessionError::CorruptBundle);
    }
    let ledger = ExperienceLedger::decode(decoded.segment(SegmentKind::Experience))
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
            && knowledge
                .pinned_revision()
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
    let derivations = ledger.derivations(knowledge.pinned_revision(), &primitive_symbols)?;
    if !learning.corpus_is_valid(&corpus_assignments)
        || !knowledge.validate(&artifact_keys, &derivations, &primitive_symbols)
    {
        return Err(SessionError::CorruptBundle);
    }
    Ok(RecoveredBundle {
        artifacts: recovered,
        pareto_keys,
        frontier_keys,
        deferred_candidates,
        pending_parents,
        ledger,
        revisions: Some(revisions),
        interrupted_usage,
        knowledge,
        learning,
        resident_bytes: decoded_resident_bytes,
    })
}

fn validate_session<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    input: &[u8],
) -> Result<Option<ResourceUsage>, SessionError<D::Error>> {
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
    if !decoded.completed {
        let expected = encode_session(
            domain,
            request,
            decoded.encoded_cursor,
            SessionSeal::Interrupted(decoded.usage),
        )?;
        if expected.get(..decoded.compatibility_prefix_len)
            != input.get(..decoded.compatibility_prefix_len)
        {
            return Err(SessionError::IncompatibleBundle);
        }
    }
    Ok((!decoded.completed).then_some(decoded.usage))
}

fn finish_scheduled<D: DomainDefinition, T>(
    result: Result<(T, VerificationBatchReport), ScheduleError<D::Error>>,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    allowance: crate::VerificationAllowance,
    resident_overlap: u64,
    corrupt_contract: bool,
) -> Result<T, SessionError<D::Error>> {
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
            Ok(value)
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

fn scheduled_verify<D: DomainDefinition>(
    domain: &D,
    scheduler: &Scheduler,
    resource_meter: &ResourceEnvelopeGuard,
    requirements: VerificationWorkerRequirements,
    resident_overlap: u64,
    requests: &[ClaimVerificationRequest<'_, D>],
    corrupt_contract: bool,
) -> Result<ClaimedVerdicts<D>, SessionError<D::Error>> {
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
        false,
    )?;
    if replayed.len() != stored.len() || replayed.iter().any(|accepted| !accepted) {
        return Err(SessionError::InvalidSeed);
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
        )?;
        for (entry, (claim, _)) in entries.iter().zip(&claims_and_verdicts) {
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
                .any(|(entry, (_, verdict))| {
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
    let claims_and_verdicts = scheduled_verify(
        domain,
        scheduler,
        resource_meter,
        requirements,
        resident_overlap,
        &requests,
        false,
    );
    let kernel_cpu = resource_meter
        .current_cpu()
        .map_err(|()| SessionError::Resource)?
        .saturating_sub(kernel_cpu_before);
    instrumentation.finish(Phase::VerificationKernel, kernel_started);
    let claims_and_verdicts = claims_and_verdicts?;
    if claims_and_verdicts.len() != candidates.len() {
        return Err(SessionError::InvalidSeed);
    }
    let verification_batch_size = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
    let verification_batch_cpu_ns = u64::try_from(kernel_cpu.as_nanos()).unwrap_or(u64::MAX);
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
    for (mut candidate, (claim, verdict)) in candidates.into_iter().zip(claims_and_verdicts) {
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
        experience.push(ExperienceEntry {
            attempt_id,
            candidate_key,
            claim_digest,
            origin_key,
            parent_key,
            canonical_candidate,
            verdict: experience_verdict,
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
    })
}

fn replace_interrupted_session<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    seed_cursor: &[u8],
    encoded_bundle: &[u8],
    usage: ResourceUsage,
) -> Result<Vec<u8>, SessionError<D::Error>> {
    let session = encode_session(
        domain,
        request,
        seed_cursor,
        SessionSeal::Interrupted(usage),
    )?;
    CanonicalBundle::replace_session(
        encoded_bundle,
        domain.semantic_identity().as_str().as_bytes(),
        &session,
    )
    .map_err(|error| {
        if error.is_identity_mismatch() {
            SessionError::IncompatibleBundle
        } else {
            SessionError::CorruptBundle
        }
    })
}

fn publish_recovery_interruption<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    seed_cursor: &[u8],
    source: &std::path::Path,
    usage: ResourceUsage,
    resource_meter: &ResourceEnvelopeGuard,
    resident_overlap: u64,
) -> Result<(), SessionError<D::Error>> {
    let bundle_bytes = std::fs::metadata(source)
        .map_err(SessionError::Durability)?
        .len();
    if !resource_meter.reserve(
        ResidentReservation::live(resident_overlap).with_transient(bundle_bytes.saturating_mul(3)),
    ) {
        return Err(SessionError::Resource);
    }
    let encoded = std::fs::read(source).map_err(SessionError::Durability)?;
    let interrupted = replace_interrupted_session(domain, request, seed_cursor, &encoded, usage)?;
    if !resource_meter.checkpoint_fits(interrupted.len() as u64) {
        return Err(SessionError::Resource);
    }
    publish_interrupted_bytes(request, interrupted)
}

#[expect(
    clippy::too_many_arguments,
    reason = "failure publication carries the complete restart boundary explicitly"
)]
fn persist_setup_interruption<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    bundle_codec: &RestartBundleCodec<'_, D>,
    seed_cursor: &[u8],
    ledger: &ExperienceLedger,
    knowledge: &KnowledgeState,
    learning: &LearningState,
    usage: ResourceUsage,
    resource_meter: &ResourceEnvelopeGuard,
    resident_overlap: u64,
) -> Result<(), SessionError<D::Error>> {
    if let Some(source) = request.bundle.source() {
        return publish_recovery_interruption(
            domain,
            request,
            seed_cursor,
            source,
            usage,
            resource_meter,
            resident_overlap,
        );
    }
    let interrupted = bundle_codec.seal(
        seed_cursor,
        RestartBundleState::new(
            &[],
            &[],
            SearchTailView::new(&[], &[], &[]),
            ledger,
            knowledge,
            learning,
        ),
        SessionSeal::Interrupted(usage),
    )?;
    if !resource_meter.checkpoint_fits(interrupted.len() as u64)
        || !resource_meter.reserve(
            ResidentReservation::live(resident_overlap)
                .with_transient(interrupted.capacity() as u64),
        )
    {
        return Err(SessionError::Resource);
    }
    publish_interrupted_bytes(request, interrupted)
}

#[expect(
    clippy::too_many_arguments,
    reason = "active failure publication carries the restart boundary explicitly"
)]
fn persist_active_interruption<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    seed_cursor: &[u8],
    checkpoint: &[u8],
    usage: ResourceUsage,
    resource_meter: &ResourceEnvelopeGuard,
    resident_overlap: u64,
    durability: &mut durability::CheckpointWriter,
) -> Result<(), SessionError<D::Error>> {
    let interrupted = replace_interrupted_session(domain, request, seed_cursor, checkpoint, usage)?;
    if !resource_meter.checkpoint_fits(interrupted.len() as u64)
        || !resource_meter.reserve(
            ResidentReservation::live(resident_overlap)
                .with_transient(interrupted.capacity() as u64)
                .with_pending_durability(durability.pending_bytes()),
        )
    {
        return Err(SessionError::Resource);
    }
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
    let mut recovery = Vec::with_capacity(
        32_usize
            .saturating_add(pareto.len().saturating_mul(32))
            .saturating_add(frontier.len().saturating_mul(32))
            .saturating_add(
                deferred_candidates
                    .len()
                    .saturating_mul(std::mem::size_of::<DeferredCandidate<D>>()),
            ),
    );
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
    debug_assert_eq!(
        recovery.len() as u64,
        recovery_encoded_len(
            pareto,
            &SearchTailView::new(frontier, deferred_candidates, pending_parents)
        )
    );
    Ok(recovery)
}

fn encode_bundle<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    seed_cursor: &[u8],
    state: RestartBundleState<'_, D>,
    session_seal: SessionSeal,
) -> Result<Vec<u8>, SessionError<D::Error>> {
    let RestartBundleState {
        artifacts,
        pareto,
        search_tail,
        ledger,
        knowledge,
        learning,
    } = state;
    let mut artifact_payload = Vec::new();
    push_u64(&mut artifact_payload, artifacts.len() as u64);
    let mut structure_scratch = <D::Structure as crate::StructuralProtocol<D>>::Scratch::default();
    let mut canonical_artifacts = artifacts.iter().collect::<Vec<_>>();
    canonical_artifacts.sort_unstable_by_key(|artifact| artifact.key());
    for artifact in canonical_artifacts {
        let mut encoded = Vec::new();
        domain
            .structure()
            .encode_canonical(
                &artifact.inner.artifact,
                &mut encoded,
                &mut structure_scratch,
            )
            .map_err(SessionError::Domain)?;
        push_bytes(&mut artifact_payload, &encoded);
        encoded.clear();
        domain
            .kernel()
            .encode_claim(&artifact.inner.verification.claim, &mut encoded)
            .map_err(SessionError::Domain)?;
        push_bytes(&mut artifact_payload, &encoded);
        encoded.clear();
        domain
            .kernel()
            .encode_evidence(&artifact.inner.verification.evidence, &mut encoded)
            .map_err(SessionError::Domain)?;
        push_bytes(&mut artifact_payload, &encoded);
        push_u64(
            &mut artifact_payload,
            artifact.inner.verification.kernel_revision.0,
        );
        artifact_payload.extend_from_slice(artifact.inner.origin_key.as_bytes());
        match artifact.inner.parent_key {
            Some(parent_key) => {
                artifact_payload.push(1);
                artifact_payload.extend_from_slice(parent_key.as_bytes());
            }
            None => artifact_payload.push(0),
        }
        push_bytes(&mut artifact_payload, &artifact.inner.provenance);
    }
    let identity = domain.semantic_identity();
    let revision_ids = revision_ids(identity.as_str(), artifacts, knowledge, learning);
    let encoded_knowledge = knowledge.encode();
    let encoded_learning = learning.encode();
    let mut revisions = Vec::with_capacity(80 + encoded_knowledge.len() + encoded_learning.len());
    revisions.extend_from_slice(&revision_ids.knowledge);
    revisions.extend_from_slice(&revision_ids.model);
    push_bytes(&mut revisions, &encoded_knowledge);
    push_bytes(&mut revisions, &encoded_learning);
    test_fault_point("revision-sealed");
    let recovery = encode_recovery_segment(domain, pareto, &search_tail)?;
    let encoded_experience = ledger.encode();
    let session = encode_session(domain, request, seed_cursor, session_seal)?;
    let bundle = CanonicalBundle::new(
        identity.as_str().as_bytes().to_vec(),
        session,
        revisions,
        artifact_payload,
        encoded_experience,
        recovery,
    )
    .encode();
    test_fault_point("manifest-sealed");
    Ok(bundle)
}

fn revision_ids<D: DomainDefinition>(
    semantic_identity: &str,
    artifacts: &[VerifiedArtifact<D>],
    state: &KnowledgeState,
    learning: &LearningState,
) -> RevisionIds {
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
    RevisionIds {
        knowledge: knowledge.finalize().into(),
        model: learning.revision_digest(semantic_identity),
    }
}

fn encode_session<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    seed_cursor: &[u8],
    session_seal: SessionSeal,
) -> Result<Vec<u8>, SessionError<D::Error>> {
    let mut scope = Vec::new();
    domain
        .seeds()
        .encode_scope(&request.seeds, &mut scope)
        .map_err(SessionError::Domain)?;
    let goals = GoalEvaluator::encode_set(domain, &request.goals)?;
    let mut payload = Vec::new();
    payload.push(u8::from(matches!(session_seal, SessionSeal::Completed(..))));
    push_u64(&mut payload, RUNTIME_REVISION);
    payload.extend_from_slice(&Sha256::digest(&goals));
    payload.extend_from_slice(&Sha256::digest(&scope));
    push_bytes(&mut payload, &scope);
    payload.extend_from_slice(&Sha256::digest(seed_cursor));
    push_bytes(&mut payload, seed_cursor);
    push_u64(&mut payload, request.resources.worker_threads.get() as u64);
    push_u64(&mut payload, request.resources.resident_bytes.get());
    push_u64(&mut payload, request.resources.durable_bytes.get());
    push_duration(&mut payload, request.resources.elapsed_time.get());
    push_duration(&mut payload, request.resources.cpu_time.get());
    push_u64(&mut payload, request.resources.verification_requests.get());
    push_u64(&mut payload, domain.kernel().revision().0);
    push_bytes(
        &mut payload,
        crate::MeasurementEnvironment::local_process()
            .identity()
            .as_bytes(),
    );
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
    Ok(payload)
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
    use std::collections::{BTreeSet, HashSet};
    #[cfg(feature = "internal-experiments")]
    use std::time::Duration;

    #[cfg(feature = "internal-experiments")]
    use sha2::Digest;

    use super::{
        ENUMERATION_COMPLETE, PendingParent, append_proposal_features, candidate_generation_limit,
        commit_pending_progress, fixed_resident_categories, generation_refill_limit,
        moved_tail_transaction_peak, online_training_due, operator_feature_values,
        protected_origin_keys, sort_prefix_by,
    };
    #[cfg(feature = "internal-experiments")]
    use super::{RUNTIME_REVISION, inspect_session_segment, push_bytes, push_duration, push_u64};
    use crate::ProposalFeatures;
    use crate::learning::{FEATURE_COUNT, Features};

    #[cfg(feature = "internal-experiments")]
    #[test]
    fn session_inspection_reports_completion_and_resource_usage() {
        let mut encoded = vec![1];
        push_u64(&mut encoded, RUNTIME_REVISION);
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

    #[test]
    fn candidate_generation_covers_one_cohort_and_two_cohorts_of_operator_breadth() {
        assert_eq!(candidate_generation_limit(24, 1_008, 16, 9, u64::MAX), 192);
        assert_eq!(candidate_generation_limit(8, 10, 32, 8, u64::MAX), 80);
        assert_eq!(candidate_generation_limit(24, 1_008, 16, 9, 64 * 1024), 8);
        assert_eq!(candidate_generation_limit(0, 1_008, 16, 9, u64::MAX), 0);
    }

    #[test]
    fn generation_refills_lookahead_only_after_deferred_inventory_drains() {
        assert_eq!(generation_refill_limit(144, 229), 0);
        assert_eq!(generation_refill_limit(144, 144), 0);
        assert_eq!(generation_refill_limit(144, 136), 8);
        assert_eq!(generation_refill_limit(144, 0), 144);
    }

    #[test]
    fn online_training_runs_at_logarithmic_experience_checkpoints() {
        assert!(!online_training_due(0, 31));
        assert!(online_training_due(0, 32));
        assert!(online_training_due(31, 40));
        assert!(!online_training_due(32, 63));
        assert!(online_training_due(63, 64));
        assert!(online_training_due(40, 80));
        assert!(!online_training_due(64, 64));
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
}
