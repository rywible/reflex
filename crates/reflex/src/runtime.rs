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
    OperatorAlgebra, OperatorEnumerationBatch, Seed, SeedSource, SeedWriter, StructuralLocation,
    StructuralProtocol, StructuralView, Verdict, VerificationBatchReport, VerificationKernel,
    VerificationRecord, VerificationReplayRequest, VerificationWorkerRequirements,
};
use crate::durability;
use crate::instrumentation::{Phase, Recorder};
use crate::knowledge::{DerivationObservation, KnowledgeRevision, KnowledgeState};
use crate::learning::{
    AttemptObservation, ConsequenceKind, ConsequenceObservation, Features, FtrlModel,
    LearningState, PotentialForecast, derive_targets,
};
use crate::measurement::{Measurement, MeasurementSpace, MeasurementWriter, VerifiedBatch};
use crate::resource::{ResidentReservation, ResourceEnvelopeGuard};
use crate::session::{
    ArtifactKey, Completion, GoalId, ImprovementRequest, ParetoSnapshot, ParetoUpdate,
    ResourceUsage, SessionError, SessionOutcome, VerifiedArtifact, VerifiedArtifactRecord,
};

mod bundle;
mod epoch;
mod experience;
mod goals;
mod scheduler;

use bundle::RestartBundleCodec;
use epoch::EpochTransition;
use experience::{
    EncodedMeasurement, ExperienceEntry, ExperienceLedger, ExperienceVerdict,
    MeasurementObservation,
};
use goals::GoalEvaluator;
use scheduler::{ClaimVerificationRequest, ScheduleError, Scheduler};

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
    operator_symbol: Vec<u8>,
    features: Features,
    epoch: u64,
    protected_derived: bool,
}

#[derive(Clone, Copy)]
struct StructuralSummary {
    node_count: f32,
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
const MIN_CHOICE_RESIDENT_BYTES: u64 = 4 * 1024;
const MAX_CANDIDATE_CHOICES: u64 = 16_384;
const RUNTIME_REVISION: u64 = 3;
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
    let requirements = domain.kernel().worker_requirements();
    let runtime_lanes = request
        .resources
        .worker_threads
        .get()
        .checked_sub(requirements.worker_lanes())
        .filter(|lanes| *lanes != 0)
        .ok_or(SessionError::Resource)?;
    let scheduler =
        Scheduler::from_environment(runtime_lanes).map_err(|()| SessionError::Resource)?;
    let worker_resident_bytes = (runtime_lanes as u64)
        .saturating_mul(WORKER_STACK_BYTES as u64)
        .saturating_add(DURABILITY_STACK_BYTES as u64)
        .saturating_add(requirements.resident_bytes());
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
    let recovered_bundle = request
        .bundle
        .source()
        .map(|source| bundle_codec.recover(source, resource_meter, worker_resident_bytes))
        .transpose()?
        .unwrap_or_default();
    let recovered_resident_bytes = recovered_bundle.resident_bytes;
    let recovered_keys = recovered_bundle.pareto_keys;
    let recovered_stored = recovered_bundle.artifacts;
    let mut ledger = recovered_bundle.ledger;
    let mut knowledge = recovered_bundle.knowledge;
    let pinned_knowledge = knowledge.pinned_revision().clone();
    let mut learning = recovered_bundle.learning;
    let pinned_model = learning.pinned_model().cloned();
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
            &[],
            &[],
            &ledger,
            &knowledge,
            &learning,
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
    let initial_usage = resource_meter
        .usage(verification_requests, 0)
        .map_err(|()| SessionError::Resource)?;
    let mut checkpoint = bundle_codec.seal(
        &seed_cursor,
        &known,
        &pareto,
        &ledger,
        &knowledge,
        &learning,
        SessionSeal::Interrupted(initial_usage),
    )?;
    if !resource_meter.checkpoint_fits(checkpoint.len() as u64) {
        return Err(SessionError::Resource);
    }
    let mut frontier = roots
        .iter()
        .cloned()
        .enumerate()
        .map(|(origin, artifact)| (artifact, origin))
        .collect::<Vec<_>>();
    let resuming_interrupted = recovered_bundle.interrupted_usage.is_some();
    for artifact in recovered {
        if let Some(origin) = roots
            .iter()
            .position(|root| root.key() == artifact.inner.origin_key)
            && (resuming_interrupted || pinned_knowledge.schedules(artifact.key().0))
            && !frontier
                .iter()
                .any(|(scheduled, _)| scheduled.key() == artifact.key())
        {
            frontier.push((artifact, origin));
        }
    }
    let operators = domain
        .operators()
        .catalog()
        .iter()
        .map(crate::OperatorDescriptor::operator)
        .collect::<Vec<_>>();
    let initial_resident = worker_resident_bytes.saturating_add(resident_state_bytes(
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
            );
        if !resource_meter.reserve(ResidentReservation::live(resident_before_epoch)) {
            resident_budget_exhausted = true;
            break;
        }
        let remaining_verifications = verification_budget.saturating_sub(verification_requests);
        if remaining_verifications == 0 {
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
        let origins = frontier
            .iter()
            .map(|(_, origin)| *origin)
            .collect::<Vec<_>>();
        let mut candidates = Vec::new();
        let mut application_bytes = 0_u64;
        let mut choice_window_exhausted = false;
        let available_resident = resource_meter.available_resident(resident_before_epoch);
        let generation_limit = usize::try_from(
            remaining_verifications
                .saturating_mul(CHOICES_PER_VERIFICATION)
                .min(available_resident / MIN_CHOICE_RESIDENT_BYTES)
                .min(MAX_CANDIDATE_CHOICES),
        )
        .unwrap_or(usize::MAX);
        if generation_limit == 0 {
            resident_budget_exhausted = true;
            break;
        }
        let has_derived = pinned_knowledge
            .operators()
            .iter()
            .any(crate::knowledge::DerivedOperator::active);
        let derived_budget = if has_derived {
            generation_limit.div_ceil(4)
        } else {
            0
        };
        let mut primitive_budget = generation_limit.saturating_sub(derived_budget);
        let catalog = domain.operators().catalog();
        for (operator_index, descriptor) in catalog.iter().enumerate() {
            if resource_meter
                .search_time_exhausted()
                .map_err(|()| SessionError::Resource)?
            {
                time_exhausted = true;
                break;
            }
            if primitive_budget == 0 {
                break;
            }
            let operators_left = catalog.len() - operator_index;
            let operator_limit = primitive_budget.div_ceil(operators_left);
            let locations = root_locations(domain, &parents);
            let mut applications = Vec::new();
            let mut application_writer =
                ApplicationWriter::with_limit(&mut applications, operator_limit);
            domain
                .operators()
                .enumerate_legal(
                    OperatorEnumerationBatch::new(
                        &parents,
                        &locations,
                        std::slice::from_ref(&descriptor.operator()),
                    ),
                    &mut application_writer,
                    &mut operator_scratch,
                )
                .map_err(SessionError::Domain)?;
            choice_window_exhausted |= application_writer.overflowed();
            if resource_meter
                .search_time_exhausted()
                .map_err(|()| SessionError::Resource)?
            {
                time_exhausted = true;
                break;
            }
            let mut operator_candidates = Vec::new();
            let mut candidate_writer =
                CandidateWriter::with_limit(&mut operator_candidates, operator_limit);
            domain
                .operators()
                .apply_batch(&applications, &mut candidate_writer, &mut operator_scratch)
                .map_err(SessionError::Domain)?;
            choice_window_exhausted |= candidate_writer.overflowed();
            primitive_budget = primitive_budget.saturating_sub(operator_candidates.len());
            application_bytes = application_bytes
                .saturating_add(vector_bytes(&locations))
                .saturating_add(vector_bytes(&applications));
            let operator_bucket = operator_feature_bucket(descriptor.symbol().as_str());
            for candidate in operator_candidates {
                let parent = parent_summaries
                    .get(candidate.source_index)
                    .ok_or(SessionError::InvalidSeed)?;
                candidates.push(ProposedCandidate {
                    features: opportunity_features(
                        domain,
                        *parent,
                        &candidate.artifact,
                        operator_bucket,
                        sequence,
                    ),
                    candidate,
                    operator_symbol: descriptor.symbol().as_str().as_bytes().to_vec(),
                    epoch: sequence,
                    protected_derived: false,
                });
            }
        }
        if time_exhausted {
            break;
        }
        if resource_meter
            .search_time_exhausted()
            .map_err(|()| SessionError::Resource)?
        {
            time_exhausted = true;
            break;
        }
        let (derived_bytes, derived_truncated) = append_derived_candidates(
            domain,
            &parents,
            &pinned_knowledge,
            &mut operator_scratch,
            derived_budget,
            sequence,
            &mut candidates,
        )?;
        application_bytes = application_bytes.saturating_add(derived_bytes);
        choice_window_exhausted |= derived_truncated;
        instrumentation.generated(candidates.len());
        instrumentation.finish(Phase::Generation, generation_started);
        test_fault_point("candidate-created");
        if resource_meter
            .search_time_exhausted()
            .map_err(|()| SessionError::Resource)?
        {
            time_exhausted = true;
            break;
        }
        let selection_started = instrumentation.start();
        candidates = retain_novel_candidates(
            domain,
            &known,
            ledger.entries(),
            &roots,
            &frontier,
            candidates,
        )?;
        let remaining = usize::try_from(remaining_verifications).unwrap_or(usize::MAX);
        order_by_learned_potential(
            &goal_evaluator,
            &frontier,
            pinned_model.as_ref(),
            remaining,
            &mut candidates,
        );
        instrumentation.selected(candidates.len());
        instrumentation.finish(Phase::Selection, selection_started);
        if candidates.is_empty() {
            resident_budget_exhausted |= choice_window_exhausted;
            break;
        }
        let transient_bytes = application_bytes
            .saturating_add(vector_bytes(&candidates))
            .saturating_add(candidate_pipeline_reserve(domain, &candidates));
        let verification_resident = ResidentReservation::live(resident_before_epoch)
            .with_transient(transient_bytes)
            .peak_bytes();
        if !resource_meter.reserve(
            ResidentReservation::live(verification_resident)
                .with_transient(checkpoint.capacity() as u64)
                .with_pending_durability(durability.pending_bytes()),
        ) {
            resident_budget_exhausted = true;
            break;
        }
        if resource_meter
            .search_time_exhausted()
            .map_err(|()| SessionError::Resource)?
        {
            time_exhausted = true;
            break;
        }
        if candidates.len() > remaining {
            candidates.truncate(remaining);
            verification_budget_exhausted = true;
        }
        if candidates.is_empty() {
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
        let experience_checkpoint_state = ledger.checkpoint_entries();
        ledger.append_entries(verification.experience);
        test_fault_point("experience-appended");
        let checkpoint_usage = resource_meter
            .usage(verification_requests, checkpoint.len() as u64)
            .map_err(|()| SessionError::Resource)?;
        let experience_checkpoint = bundle_codec.seal(
            &seed_cursor,
            &known,
            &pareto,
            &ledger,
            &knowledge,
            &learning,
            SessionSeal::Interrupted(checkpoint_usage),
        )?;
        if !resource_meter.checkpoint_fits(experience_checkpoint.len() as u64) {
            ledger.rollback_entries(experience_checkpoint_state);
            durable_budget_exhausted = true;
            break;
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
        if !resource_meter.reserve(
            ResidentReservation::live(experience_live)
                .with_transient(experience_checkpoint.capacity() as u64)
                .with_pending_durability(durability.pending_bytes()),
        ) {
            ledger.rollback_entries(experience_checkpoint_state);
            resident_budget_exhausted = true;
            break;
        }
        checkpoint = experience_checkpoint;
        durability
            .submit(checkpoint.clone())
            .map_err(SessionError::Durability)?;
        if resource_meter
            .search_time_exhausted()
            .map_err(|()| SessionError::Resource)?
        {
            time_exhausted = true;
            break;
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
            frontier.clear();
            resident_budget_exhausted |= choice_window_exhausted;
            instrumentation.finish(Phase::MeasurementAdmission, measurement_admission_started);
            break;
        }
        instrumentation.admitted(admitted.len());
        let mut epoch = EpochTransition::begin(&mut known, &mut frontier, &mut ledger);
        for (artifact, origin) in admitted {
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
        let checkpoint_usage = resource_meter
            .usage(verification_requests, checkpoint.len() as u64)
            .map_err(|()| SessionError::Resource)?;
        let proposed_checkpoint = bundle_codec.seal(
            &seed_cursor,
            epoch.known(),
            &proposed_pareto,
            epoch.ledger(),
            &knowledge,
            &learning,
            SessionSeal::Interrupted(checkpoint_usage),
        )?;
        if !resource_meter.checkpoint_fits(proposed_checkpoint.len() as u64) {
            durable_budget_exhausted = true;
            break;
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
            &learning,
        ));
        let proposed_reservation = ResidentReservation::live(proposed_live)
            .with_transient(proposed_checkpoint.capacity() as u64)
            .with_pending_durability(durability.pending_bytes());
        if !resource_meter.reserve(proposed_reservation) {
            resident_budget_exhausted = true;
            break;
        }
        durability
            .submit(proposed_checkpoint.clone())
            .and_then(|()| durability.barrier().map(|_| ()))
            .map_err(SessionError::Durability)?;
        test_fault_point("pareto-published");
        let delivery = deliver_delta(
            &mut observer,
            &mut sequence,
            &previous_keys,
            &proposed_pareto,
            &goal_evaluator.affected(&previous_goal_frontiers, &proposed_goal_frontiers),
            resource_meter,
            proposed_reservation.peak_bytes(),
        );
        if delivery.resource_exhausted() {
            resident_budget_exhausted = true;
            break;
        }
        epoch.commit();
        instrumentation.finish(Phase::MeasurementAdmission, measurement_admission_started);
        pareto = proposed_pareto;
        checkpoint = proposed_checkpoint;
        stopped_by_observer = delivery.stopped();
        success_conditions_satisfied =
            goal_evaluator.success_satisfied(&goal_evaluator.frontiers(&known));
        time_exhausted = resource_meter
            .search_time_exhausted()
            .map_err(|()| SessionError::Resource)?;
        if verification_budget_exhausted {
            break;
        }
        if choice_window_exhausted {
            resident_budget_exhausted = true;
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
    let consolidation_live = worker_resident_bytes.saturating_add(resident_state_bytes(
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
    let consolidation_transient = (ledger.len() as u64)
        .saturating_mul((std::mem::size_of::<DerivationObservation>() as u64).saturating_add(512))
        .saturating_add(4_096 * 64)
        .saturating_add(ledger.entries().iter().fold(0_u64, |bytes, entry| {
            bytes.saturating_add((entry.operator_symbol.capacity() as u64).saturating_mul(2))
        }));
    let can_consolidate = completion != Completion::StoppedByObserver
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
            .saturating_add(knowledge.resident_bytes());
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
    instrumentation.finish(Phase::Consolidation, consolidation_started);
    let training_started = instrumentation.start();
    let learning_live = worker_resident_bytes.saturating_add(resident_state_bytes(
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
    let learning_transient = (ledger.len() as u64)
        .saturating_mul(std::mem::size_of::<AttemptObservation>() as u64)
        .saturating_add(LearningState::training_scratch_bytes(ledger.len()));
    let can_train = !resource_meter
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
    let provisional_checkpoint = bundle_codec.seal(
        &seed_cursor,
        &known,
        &pareto,
        &ledger,
        &knowledge,
        &learning,
        SessionSeal::Completed(completion, provisional_usage),
    )?;
    let usage = resource_meter
        .usage(verification_requests, provisional_checkpoint.len() as u64)
        .map_err(|()| SessionError::Resource)?;
    checkpoint = bundle_codec.seal(
        &seed_cursor,
        &known,
        &pareto,
        &ledger,
        &knowledge,
        &learning,
        SessionSeal::Completed(completion, usage),
    )?;
    if !resource_meter.checkpoint_fits(checkpoint.len() as u64) {
        return Err(SessionError::Resource);
    }
    let final_live = worker_resident_bytes.saturating_add(resident_state_bytes(
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
            .saturating_add(structural)
            .saturating_mul(2);
        bytes.saturating_add(ledger.max(4 * 1024))
    })
}

fn opportunity_features<D: DomainDefinition>(
    domain: &D,
    parent: StructuralSummary,
    candidate: &D::Artifact,
    operator_bucket: usize,
    epoch: u64,
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
    values[operator_bucket] = 1.0;
    values[13] = reduction * values[operator_bucket];
    values[14] = f32::from(candidate_nodes > parent_nodes);
    values[15] = (candidate_nodes / parent_nodes.max(1.0)).min(4.0) / 4.0;
    Features(values)
}

fn structural_node_count<D: DomainDefinition>(domain: &D, artifact: &D::Artifact) -> f32 {
    f32::from(u16::try_from(domain.structure().view(artifact).node_count()).unwrap_or(u16::MAX))
}

fn operator_feature_bucket(operator_symbol: &str) -> usize {
    let operator_digest = Sha256::digest(operator_symbol.as_bytes());
    5 + usize::from(operator_digest[0] % 8)
}

fn append_derived_candidates<D: DomainDefinition>(
    domain: &D,
    parents: &[&D::Artifact],
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
                parents[..parents.len().min(operator_limit)].to_vec()
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
            truncated |= candidate_writer.overflowed();
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
        let operator_bucket = operator_feature_bucket(symbol);
        emitted = emitted.saturating_add(current.len());
        for candidate in current {
            let parent = parents
                .get(candidate.source_index)
                .ok_or(SessionError::CorruptBundle)?;
            output.push(ProposedCandidate {
                features: opportunity_features(
                    domain,
                    StructuralSummary {
                        node_count: structural_node_count(domain, parent),
                    },
                    &candidate.artifact,
                    operator_bucket,
                    epoch,
                ),
                candidate,
                operator_symbol: derived.symbol().to_vec(),
                epoch,
                protected_derived: derived.protected_exploration(),
            });
        }
    }
    Ok((application_bytes, truncated))
}

fn order_by_learned_potential<D: DomainDefinition>(
    goals: &GoalEvaluator<'_, D>,
    frontier: &[(VerifiedArtifact<D>, usize)],
    model: Option<&FtrlModel>,
    limit: usize,
    candidates: &mut Vec<ProposedCandidate<D>>,
) {
    if model.is_none() {
        let (mut derived, mut ordinary): (Vec<_>, Vec<_>) = std::mem::take(candidates)
            .into_iter()
            .partition(|candidate| candidate.protected_derived);
        let compare = |left: &ProposedCandidate<D>, right: &ProposedCandidate<D>| {
            goals.compare_parents(frontier, left, right).then_with(|| {
                left.operator_symbol
                    .cmp(&right.operator_symbol)
                    .then_with(|| {
                        left.candidate
                            .source_index
                            .cmp(&right.candidate.source_index)
                    })
            })
        };
        sort_prefix_by(&mut derived, limit, compare);
        sort_prefix_by(&mut ordinary, limit, compare);
        let mut derived = derived.into_iter();
        let mut ordinary = ordinary.into_iter();
        loop {
            let before = candidates.len();
            candidates.extend(derived.by_ref().take(2));
            candidates.extend(ordinary.by_ref().take(6));
            if candidates.len() == before || candidates.len() >= limit {
                break;
            }
        }
        candidates.truncate(limit);
        return;
    }
    let model = model.expect("the learned ordering branch requires a Model Revision");
    let mut derived_exploration = Vec::new();
    let mut ranked = Vec::new();
    let mut exploration = Vec::new();
    for (index, candidate) in std::mem::take(candidates).into_iter().enumerate() {
        if candidate.protected_derived {
            derived_exploration.push(candidate);
        } else if index.is_multiple_of(8) {
            exploration.push(candidate);
        } else {
            ranked.push(candidate);
        }
    }
    let ranked_features = ranked
        .iter()
        .map(|candidate| candidate.features)
        .collect::<Vec<_>>();
    let mut forecasts = Vec::with_capacity(ranked_features.len());
    model.forecast_batch(&ranked_features, &mut forecasts);
    let mut ranked = ranked.into_iter().zip(forecasts).collect::<Vec<_>>();
    let compare =
        |(left, left_forecast): &(ProposedCandidate<D>, PotentialForecast),
         (right, right_forecast): &(ProposedCandidate<D>, PotentialForecast)| {
            compare_forecasts(*left_forecast, *right_forecast)
                .then_with(|| goals.compare_parents(frontier, left, right))
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
    sort_prefix_by(&mut ranked, limit, compare);
    let mut ranked = ranked.into_iter().map(|(candidate, _)| candidate);
    let mut exploration = exploration.into_iter();
    let mut derived_exploration = derived_exploration.into_iter();
    loop {
        let before = candidates.len();
        candidates.extend(derived_exploration.by_ref().take(2));
        if let Some(candidate) = exploration.next() {
            candidates.push(candidate);
        }
        candidates.extend(ranked.by_ref().take(5));
        if candidates.len() == before || candidates.len() >= limit {
            break;
        }
    }
    candidates.truncate(limit);
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

fn compare_forecasts(left: PotentialForecast, right: PotentialForecast) -> Ordering {
    for head in [0, 1, 2, 3, 4] {
        let left_value = left.0[head].estimate
            - left.0[head].uncertainty
            - left.0[head].calibration_error * 0.25;
        let right_value = right.0[head].estimate
            - right.0[head].uncertainty
            - right.0[head].calibration_error * 0.25;
        let ordering = right_value.total_cmp(&left_value);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    for head in [6, 5] {
        let left_value = left.0[head].estimate
            + left.0[head].uncertainty
            + left.0[head].calibration_error * 0.25;
        let right_value = right.0[head].estimate
            + right.0[head].uncertainty
            + right.0[head].calibration_error * 0.25;
        let ordering = left_value.total_cmp(&right_value);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
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
) -> Result<Vec<ProposedCandidate<D>>, SessionError<D::Error>> {
    let mut keys = known
        .iter()
        .map(|artifact| Ok((artifact.key(), claim_digest(domain, artifact)?)))
        .collect::<Result<HashSet<_>, SessionError<D::Error>>>()?;
    let root_claims = roots
        .iter()
        .map(|root| claim_digest(domain, root))
        .collect::<Result<Vec<_>, _>>()?;
    let mut scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    let identity = domain.semantic_identity();
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let mut canonical = Vec::new();
            match domain.structure().encode_canonical(
                &candidate.candidate.artifact,
                &mut canonical,
                &mut scratch,
            ) {
                Ok(()) => {
                    let key = ArtifactKey(stable_digest(identity.as_str(), &canonical));
                    let origin = frontier[candidate.candidate.source_index].1;
                    if !keys.insert((key, root_claims[origin])) {
                        return None;
                    }
                    (!experience.iter().any(|entry| {
                        entry.candidate_key == key
                            && entry.claim_digest == root_claims[origin]
                            && matches!(
                                entry.verdict,
                                ExperienceVerdict::Refuted | ExperienceVerdict::Unknown
                            )
                    }))
                    .then_some(Ok(candidate))
                }
                Err(error) => Some(Err(SessionError::Domain(error))),
            }
        })
        .collect()
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
    if !recovery.is_empty() {
        return Err(SessionError::CorruptBundle);
    }
    let mut encoded_experience = decoded.segment(SegmentKind::Experience);
    let experience_count = usize::try_from(read_bundle_u64(&mut encoded_experience)?)
        .map_err(|_| SessionError::CorruptBundle)?;
    if experience_count > encoded_experience.len().saturating_div(181) {
        return Err(SessionError::CorruptBundle);
    }
    let mut experience = Vec::with_capacity(experience_count);
    for _ in 0..experience_count {
        let attempt_id: [u8; 32] = take_bundle(&mut encoded_experience, 32)?
            .try_into()
            .expect("exactly 32 Experience attempt ID bytes were taken");
        let candidate_key = read_artifact_key(&mut encoded_experience)?;
        let claim_digest: [u8; 32] = take_bundle(&mut encoded_experience, 32)?
            .try_into()
            .expect("exactly 32 Experience claim digest bytes were taken");
        let origin_key = read_artifact_key(&mut encoded_experience)?;
        let parent_key = read_artifact_key(&mut encoded_experience)?;
        let canonical_candidate = take_sized(&mut encoded_experience)?.to_vec();
        if ArtifactKey(stable_digest(identity.as_str(), &canonical_candidate)) != candidate_key {
            return Err(SessionError::CorruptBundle);
        }
        let candidate_artifact = domain
            .structure()
            .decode_canonical(&canonical_candidate, &mut structure_scratch)
            .map_err(|_| SessionError::CorruptBundle)?;
        let verdict = match take_bundle(&mut encoded_experience, 1)?[0] {
            1 => ExperienceVerdict::Accepted,
            2 => ExperienceVerdict::Refuted,
            3 => ExperienceVerdict::Unknown,
            _ => return Err(SessionError::CorruptBundle),
        };
        let operator_symbol = take_sized(&mut encoded_experience)?.to_vec();
        let operator = std::str::from_utf8(&operator_symbol)
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
                .resolve_operator(&operator_symbol)
                .is_none()
        {
            return Err(SessionError::CorruptBundle);
        }
        let Some(origin_index) = recovered_index.get(&origin_key).copied() else {
            return Err(SessionError::CorruptBundle);
        };
        let Some(parent_index) = recovered_index.get(&parent_key).copied() else {
            return Err(SessionError::CorruptBundle);
        };
        let origin = &recovered[origin_index];
        let mut encoded_claim = Vec::new();
        domain
            .kernel()
            .encode_claim(&origin.verification.claim, &mut encoded_claim)
            .map_err(SessionError::Domain)?;
        if <[u8; 32]>::from(Sha256::digest(encoded_claim)) != claim_digest {
            return Err(SessionError::CorruptBundle);
        }
        let mut feature_values = [0.0; crate::learning::FEATURE_COUNT];
        for value in &mut feature_values {
            *value = f32::from_bits(u32::from_le_bytes(
                take_bundle(&mut encoded_experience, 4)?
                    .try_into()
                    .expect("exactly four feature bytes were taken"),
            ));
            if !value.is_finite() {
                return Err(SessionError::CorruptBundle);
            }
        }
        let verification_requests = u32::from_le_bytes(
            take_bundle(&mut encoded_experience, 4)?
                .try_into()
                .expect("exactly four verification-request bytes were taken"),
        );
        let epoch = read_bundle_u64(&mut encoded_experience)?;
        let features = Features(feature_values);
        if verification_requests != 1
            || attempt_digest(
                candidate_key,
                origin_key,
                parent_key,
                &operator_symbol,
                epoch,
            ) != attempt_id
            || opportunity_features(
                domain,
                StructuralSummary {
                    node_count: structural_node_count(domain, &recovered[parent_index].artifact),
                },
                &candidate_artifact,
                operator_feature_bucket(operator),
                epoch,
            ) != features
        {
            return Err(SessionError::CorruptBundle);
        }
        let entry = ExperienceEntry {
            attempt_id,
            candidate_key,
            claim_digest,
            origin_key,
            parent_key,
            canonical_candidate,
            verdict,
            operator_symbol,
            features,
            verification_requests,
            epoch,
        };
        if experience
            .iter()
            .any(|known: &ExperienceEntry| known.attempt_id == entry.attempt_id)
        {
            return Err(SessionError::CorruptBundle);
        }
        experience.push(entry);
    }
    let consequence_count = usize::try_from(read_bundle_u64(&mut encoded_experience)?)
        .map_err(|_| SessionError::CorruptBundle)?;
    if consequence_count > encoded_experience.len().saturating_div(33) {
        return Err(SessionError::CorruptBundle);
    }
    let mut consequences = Vec::with_capacity(consequence_count);
    for _ in 0..consequence_count {
        let subject: [u8; 32] = take_bundle(&mut encoded_experience, 32)?
            .try_into()
            .expect("exactly 32 consequence-subject bytes were taken");
        let kind = match take_bundle(&mut encoded_experience, 1)?[0] {
            1 => ConsequenceKind::Admitted,
            2 => ConsequenceKind::ParetoImprovement,
            3 => ConsequenceKind::CrossGoalUse,
            4 => ConsequenceKind::Compression,
            _ => return Err(SessionError::CorruptBundle),
        };
        let consequence = ConsequenceObservation { subject, kind };
        if !experience.iter().any(|entry| entry.attempt_id == subject)
            || consequences.contains(&consequence)
        {
            return Err(SessionError::CorruptBundle);
        }
        consequences.push(consequence);
    }
    let measurement_count = usize::try_from(read_bundle_u64(&mut encoded_experience)?)
        .map_err(|_| SessionError::CorruptBundle)?;
    if measurement_count > encoded_experience.len().saturating_div(48) {
        return Err(SessionError::CorruptBundle);
    }
    let mut measurements = Vec::with_capacity(measurement_count);
    for _ in 0..measurement_count {
        let subject: [u8; 32] = take_bundle(&mut encoded_experience, 32)?
            .try_into()
            .expect("exactly 32 measurement-subject bytes were taken");
        if !experience.iter().any(|entry| {
            entry.attempt_id == subject && entry.verdict == ExperienceVerdict::Accepted
        }) || measurements
            .iter()
            .any(|observation: &MeasurementObservation| observation.subject == subject)
        {
            return Err(SessionError::CorruptBundle);
        }
        let environment = take_sized(&mut encoded_experience)?.to_vec();
        if environment.is_empty() || std::str::from_utf8(&environment).is_err() {
            return Err(SessionError::CorruptBundle);
        }
        let value_count = usize::try_from(read_bundle_u64(&mut encoded_experience)?)
            .map_err(|_| SessionError::CorruptBundle)?;
        if value_count == 0 || value_count > domain.measurements().schema().len() {
            return Err(SessionError::CorruptBundle);
        }
        let mut values = Vec::with_capacity(value_count);
        for _ in 0..value_count {
            let metric_symbol = take_sized(&mut encoded_experience)?.to_vec();
            let observation = take_sized(&mut encoded_experience)?.to_vec();
            let Some(descriptor) = domain
                .measurements()
                .schema()
                .iter()
                .find(|descriptor| descriptor.symbol().as_str().as_bytes() == metric_symbol)
            else {
                return Err(SessionError::CorruptBundle);
            };
            if values
                .iter()
                .any(|value: &EncodedMeasurement| value.metric_symbol == metric_symbol)
                || domain
                    .measurements()
                    .decode_observation(descriptor.metric(), &observation)
                    .is_err()
            {
                return Err(SessionError::CorruptBundle);
            }
            values.push(EncodedMeasurement {
                metric_symbol,
                observation,
            });
        }
        measurements.push(MeasurementObservation {
            subject,
            environment,
            values,
        });
    }
    let ledger = ExperienceLedger::from_parts(experience, consequences, measurements);
    let corpus_assignments = ledger
        .entries()
        .iter()
        .map(|entry| (entry.attempt_id, entry.claim_digest))
        .collect::<Vec<_>>();
    let artifact_keys = recovered_index
        .keys()
        .map(|key| key.0)
        .collect::<BTreeSet<_>>();
    let primitive_symbols = domain
        .operators()
        .catalog()
        .iter()
        .map(|descriptor| descriptor.symbol().as_str().as_bytes().to_vec())
        .collect::<BTreeSet<_>>();
    let derivations = ledger.derivations(knowledge.pinned_revision(), &primitive_symbols)?;
    if !encoded_experience.is_empty()
        || !learning.corpus_is_valid(&corpus_assignments)
        || !knowledge.validate(&artifact_keys, &derivations, &primitive_symbols)
    {
        return Err(SessionError::CorruptBundle);
    }
    Ok(RecoveredBundle {
        artifacts: recovered,
        pareto_keys,
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
    mut input: &[u8],
) -> Result<Option<ResourceUsage>, SessionError<D::Error>> {
    let encoded_session = input;
    let disposition = take_bundle(&mut input, 1)?[0];
    if !matches!(disposition, 0 | 1) {
        return Err(SessionError::CorruptBundle);
    }
    if read_bundle_u64(&mut input)? != RUNTIME_REVISION {
        return Err(SessionError::IncompatibleBundle);
    }
    let _goal_fingerprint = take_bundle(&mut input, 32)?;
    let scope_fingerprint = take_bundle(&mut input, 32)?;
    let encoded_scope = take_sized(&mut input)?;
    if Sha256::digest(encoded_scope)[..] != *scope_fingerprint {
        return Err(SessionError::CorruptBundle);
    }
    let scope = domain
        .seeds()
        .decode_scope(encoded_scope)
        .map_err(|_| SessionError::CorruptBundle)?;
    let mut canonical_scope = Vec::new();
    domain
        .seeds()
        .encode_scope(&scope, &mut canonical_scope)
        .map_err(|_| SessionError::CorruptBundle)?;
    if canonical_scope != encoded_scope {
        return Err(SessionError::CorruptBundle);
    }
    let cursor_fingerprint = take_bundle(&mut input, 32)?;
    let encoded_cursor = take_sized(&mut input)?;
    if Sha256::digest(encoded_cursor)[..] != *cursor_fingerprint {
        return Err(SessionError::CorruptBundle);
    }
    let cursor = domain
        .seeds()
        .decode_cursor(encoded_cursor)
        .map_err(|_| SessionError::CorruptBundle)?;
    let mut canonical_cursor = Vec::new();
    domain
        .seeds()
        .encode_cursor(&cursor, &mut canonical_cursor)
        .map_err(|_| SessionError::CorruptBundle)?;
    if canonical_cursor != encoded_cursor {
        return Err(SessionError::CorruptBundle);
    }
    let _worker_threads = read_bundle_u64(&mut input)?;
    let _resident_bytes = read_bundle_u64(&mut input)?;
    let _durable_bytes = read_bundle_u64(&mut input)?;
    take_bundle(&mut input, 12)?;
    take_bundle(&mut input, 12)?;
    let _verification_requests = read_bundle_u64(&mut input)?;
    if read_bundle_u64(&mut input)? != domain.kernel().revision().0 {
        return Err(SessionError::IncompatibleBundle);
    }
    let compatibility_prefix_len = encoded_session.len() - input.len();
    let environment = take_sized(&mut input)?;
    let completion = take_bundle(&mut input, 1)?[0];
    if environment.is_empty() || !matches!((disposition, completion), (0, 0) | (1, 1..=4)) {
        return Err(SessionError::CorruptBundle);
    }
    let worker_threads =
        usize::try_from(read_bundle_u64(&mut input)?).map_err(|_| SessionError::CorruptBundle)?;
    let resident_bytes = read_bundle_u64(&mut input)?;
    let verification_requests = read_bundle_u64(&mut input)?;
    let durable_bytes = read_bundle_u64(&mut input)?;
    let elapsed_time = read_bundle_duration(&mut input)?;
    let cpu_time = read_bundle_duration(&mut input)?;
    if !input.is_empty() {
        return Err(SessionError::CorruptBundle);
    }
    let usage = ResourceUsage {
        worker_threads,
        resident_bytes,
        verification_requests,
        durable_bytes,
        elapsed_time,
        cpu_time,
    };
    if disposition == 0 {
        let expected = encode_session(
            domain,
            request,
            encoded_cursor,
            SessionSeal::Interrupted(usage),
        )?;
        if expected.get(..compatibility_prefix_len)
            != encoded_session.get(..compatibility_prefix_len)
        {
            return Err(SessionError::IncompatibleBundle);
        }
    }
    Ok((disposition == 0).then_some(usage))
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

fn read_artifact_key<E>(input: &mut &[u8]) -> Result<ArtifactKey, SessionError<E>> {
    Ok(ArtifactKey(
        take_bundle(input, 32)?
            .try_into()
            .expect("exactly 32 ArtifactKey bytes were taken"),
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
) -> Result<VerificationOutcome<D>, SessionError<D::Error>> {
    let mut structure_scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    let identity = domain.semantic_identity();
    let candidate_encodings = candidates
        .iter()
        .map(|candidate| {
            let mut canonical = Vec::new();
            domain
                .structure()
                .encode_canonical(
                    &candidate.candidate.artifact,
                    &mut canonical,
                    &mut structure_scratch,
                )
                .map_err(SessionError::Domain)?;
            Ok((
                ArtifactKey(stable_digest(identity.as_str(), &canonical)),
                canonical,
            ))
        })
        .collect::<Result<Vec<_>, _>>()?;
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
    let claims_and_verdicts = scheduled_verify(
        domain,
        scheduler,
        resource_meter,
        requirements,
        resident_overlap,
        &requests,
        false,
    );
    instrumentation.finish(Phase::VerificationKernel, kernel_started);
    let claims_and_verdicts = claims_and_verdicts?;
    if claims_and_verdicts.len() != candidates.len() {
        return Err(SessionError::InvalidSeed);
    }
    let revision = domain.kernel().revision();
    let mut accepted = Vec::new();
    let mut experience = Vec::with_capacity(candidates.len());
    for ((candidate, (candidate_key, canonical_candidate)), (claim, verdict)) in candidates
        .into_iter()
        .zip(candidate_encodings)
        .zip(claims_and_verdicts)
    {
        let origin = parent_origins[candidate.candidate.source_index];
        let origin_key = roots[origin].key();
        let parent_key = parent_keys[candidate.candidate.source_index];
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
        );
        let experience_verdict = match verdict {
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
                ExperienceVerdict::Accepted
            }
            Verdict::Refuted => ExperienceVerdict::Refuted,
            Verdict::Unknown => ExperienceVerdict::Unknown,
        };
        experience.push(ExperienceEntry {
            attempt_id,
            candidate_key,
            claim_digest,
            origin_key,
            parent_key,
            canonical_candidate,
            verdict: experience_verdict,
            operator_symbol: candidate.operator_symbol,
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
        &[],
        &[],
        ledger,
        knowledge,
        learning,
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

#[expect(
    clippy::too_many_arguments,
    reason = "the canonical bundle encoder explicitly receives every restart-complete state segment"
)]
fn encode_bundle<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    seed_cursor: &[u8],
    artifacts: &[VerifiedArtifact<D>],
    pareto: &[VerifiedArtifact<D>],
    ledger: &ExperienceLedger,
    knowledge: &KnowledgeState,
    learning: &LearningState,
    session_seal: SessionSeal,
) -> Result<Vec<u8>, SessionError<D::Error>> {
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
    let mut recovery = Vec::with_capacity(8 + pareto.len() * 32);
    push_u64(&mut recovery, pareto.len() as u64);
    for artifact in pareto {
        recovery.extend_from_slice(artifact.key().as_bytes());
    }
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
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-attempt-observation-v1\0");
    digest.update(candidate_key.as_bytes());
    digest.update(origin_key.as_bytes());
    digest.update(parent_key.as_bytes());
    digest.update((operator_symbol.len() as u64).to_le_bytes());
    digest.update(operator_symbol);
    digest.update(epoch.to_le_bytes());
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
    use super::sort_prefix_by;

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
}
