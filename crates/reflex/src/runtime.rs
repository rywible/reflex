use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ops::ControlFlow;
use std::sync::Arc;
#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use sha2::{Digest, Sha256};

use crate::bundle::DomainBundle;
use crate::domain::{
    ApplicationWriter, Candidate, CandidateWriter, DomainDefinition, OperatorAlgebra,
    OperatorEnumerationBatch, ReplayVerdictWriter, Seed, SeedSource, SeedWriter,
    StructuralProtocol, StructuralView, Verdict, VerdictWriter, VerificationBatch,
    VerificationKernel, VerificationRecord, VerificationReplayBatch, VerificationReplayRequest,
    VerificationRequest,
};
use crate::durability::{self, Segment, SegmentKind};
use crate::goal::{Direction, GoalSet, OptimizationGoal, ThresholdRelation};
use crate::knowledge::{DerivationObservation, KnowledgeRevision, KnowledgeState};
use crate::learning::{
    AttemptObservation, ConsequenceKind, ConsequenceObservation, Features, FtrlModel,
    LearningState, PotentialForecast, VerdictTarget, derive_targets,
};
use crate::measurement::{
    Measurement, MeasurementSpace, MeasurementWriter, MetricOrdering, VerifiedBatch,
};
use crate::resource::ProductionResourceMeter;
use crate::session::{
    ArtifactKey, Completion, ImprovementRequest, ParetoSnapshot, ParetoUpdate, ResourceUsage,
    SessionError, SessionOutcome, VerifiedArtifact, VerifiedArtifactRecord,
};

struct StoredArtifact<D: DomainDefinition> {
    artifact: D::Artifact,
    verification: VerificationRecord<D>,
    origin_key: Option<ArtifactKey>,
    parent_key: Option<ArtifactKey>,
}
type OriginatedStoredArtifact<D> = (StoredArtifact<D>, usize, [u8; 32]);
struct RecoveredBundle<D: DomainDefinition> {
    artifacts: Vec<StoredArtifact<D>>,
    pareto_keys: Vec<ArtifactKey>,
    experience: Vec<ExperienceEntry>,
    revisions: Option<RevisionIds>,
    interrupted_usage: Option<ResourceUsage>,
    knowledge: KnowledgeState,
    learning: LearningState,
    consequences: Vec<ConsequenceObservation>,
    measurements: Vec<MeasurementObservation>,
}

impl<D: DomainDefinition> Default for RecoveredBundle<D> {
    fn default() -> Self {
        Self {
            artifacts: Vec::new(),
            pareto_keys: Vec::new(),
            experience: Vec::new(),
            revisions: None,
            interrupted_usage: None,
            knowledge: KnowledgeState::default(),
            learning: LearningState::default(),
            consequences: Vec::new(),
            measurements: Vec::new(),
        }
    }
}

#[derive(Clone, Copy)]
struct RevisionIds {
    knowledge: [u8; 32],
    model: [u8; 32],
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ExperienceVerdict {
    Accepted = 1,
    Refuted = 2,
    Unknown = 3,
}

#[derive(Clone, PartialEq)]
struct ExperienceEntry {
    attempt_id: [u8; 32],
    candidate_key: ArtifactKey,
    claim_digest: [u8; 32],
    origin_key: ArtifactKey,
    parent_key: ArtifactKey,
    canonical_candidate: Vec<u8>,
    verdict: ExperienceVerdict,
    operator_symbol: Vec<u8>,
    features: Features,
    verification_requests: u32,
    epoch: u64,
}

#[derive(Clone, PartialEq)]
struct EncodedMeasurement {
    metric_symbol: Vec<u8>,
    observation: Vec<u8>,
}

#[derive(Clone, PartialEq)]
struct MeasurementObservation {
    subject: [u8; 32],
    environment: Vec<u8>,
    values: Vec<EncodedMeasurement>,
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
        ProductionResourceMeter::start(&request.resources).map_err(|()| SessionError::Resource)?;
    let worker_resident_bytes = (request.resources.worker_threads.get() as u64)
        .saturating_mul(WORKER_STACK_BYTES as u64)
        .saturating_add(DURABILITY_STACK_BYTES as u64);
    if !resource_meter.observe_resident(worker_resident_bytes) {
        return Err(SessionError::Resource);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(request.resources.worker_threads.get())
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
    resource_meter: &ProductionResourceMeter,
    worker_resident_bytes: u64,
) -> Result<SessionOutcome<D>, SessionError<D::Error>>
where
    D: DomainDefinition,
    O: for<'a> FnMut(ParetoUpdate<'a, D>) -> ControlFlow<()>,
{
    validate_goals(domain, request)?;
    let recovered_bundle = request
        .bundle
        .source()
        .map(|source| decode_bundle(domain, request, source))
        .transpose()?
        .unwrap_or_default();
    let recovered_keys = recovered_bundle.pareto_keys;
    let recovered_stored = recovered_bundle.artifacts;
    let mut experience = recovered_bundle.experience;
    let mut consequences = recovered_bundle.consequences;
    let mut measurement_observations = recovered_bundle.measurements;
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
    let ReadSeeds {
        mut seeds,
        encoded_cursor: seed_cursor,
    } = read_seeds(domain, &request.seeds)?;
    let seed_replays = seeds.len();
    if seeds.is_empty() {
        return Err(SessionError::InvalidSeed);
    }
    let required_replays = recovered_replays
        .checked_add(seed_replays)
        .and_then(|count| count.checked_add(experience.len()))
        .and_then(|count| u64::try_from(count).ok())
        .ok_or(SessionError::Resource)?;
    let verification_budget = request.resources.verification_requests.get();
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
    replay_stored(domain, &recovered_stored)?;
    if resource_meter
        .time_exhausted()
        .map_err(|()| SessionError::Resource)?
    {
        return Err(SessionError::Resource);
    }
    replay_seeds(domain, &seeds)?;
    let environment = crate::MeasurementEnvironment::local_process();
    let recovered = materialize(domain, recovered_stored, &environment)?;
    replay_experience(domain, &recovered, &experience)?;
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
            origin_key: None,
            parent_key: None,
        })
        .collect::<Vec<_>>();
    let roots = materialize(domain, seed_stored, &environment)?;
    let mut known = recovered.clone();
    extend_unique(&mut known, roots.iter().cloned());
    let mut pareto = pareto_union(domain, &request.goals, &known);
    let initial_usage = resource_meter
        .usage(verification_requests, 0)
        .map_err(|()| SessionError::Resource)?;
    let mut checkpoint = encode_bundle(
        domain,
        request,
        &seed_cursor,
        &known,
        &pareto,
        &experience,
        &consequences,
        &measurement_observations,
        &knowledge,
        &learning,
        SessionSeal::Interrupted(initial_usage),
    )?;
    if checkpoint.len() as u64 > request.resources.durable_bytes.get() {
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
        &experience,
        &consequences,
        &measurement_observations,
        &knowledge,
        &learning,
    ));
    if !resource_meter
        .observe_resident(initial_resident.saturating_add(checkpoint.capacity() as u64))
    {
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
        resource_meter,
        initial_resident,
    );
    let mut stopped_by_observer = initial_delivery.stopped();
    let mut success_conditions_satisfied = all_success_conditions_satisfied(
        domain,
        &request.goals,
        &goal_frontiers(domain, &request.goals, &known),
    );
    let mut time_exhausted = resource_meter
        .search_time_exhausted()
        .map_err(|()| SessionError::Resource)?;
    let mut verification_budget_exhausted = false;
    let mut durable_budget_exhausted = false;
    let mut resident_budget_exhausted = initial_delivery.resource_exhausted();
    let mut operator_scratch = <D::Operators as OperatorAlgebra<D>>::Scratch::default();

    while !stopped_by_observer
        && !success_conditions_satisfied
        && !time_exhausted
        && !resident_budget_exhausted
        && !frontier.is_empty()
    {
        let resident_before_epoch = worker_resident_bytes
            .saturating_add(resident_state_bytes(
                &known,
                &roots,
                &pareto,
                &frontier,
                &recovered_keys,
                &operators,
                &checkpoint,
                &experience,
                &consequences,
                &measurement_observations,
                &knowledge,
                &learning,
            ))
            .saturating_add(
                (frontier.len() as u64).saturating_mul(std::mem::size_of::<&D::Artifact>() as u64),
            )
            .saturating_add(
                (frontier.len() as u64).saturating_mul(std::mem::size_of::<usize>() as u64),
            );
        if !resource_meter.observe_resident(resident_before_epoch) {
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
        let origins = frontier
            .iter()
            .map(|(_, origin)| *origin)
            .collect::<Vec<_>>();
        let mut candidates = Vec::new();
        let mut application_bytes = 0_u64;
        for descriptor in domain.operators().catalog() {
            let mut applications = Vec::new();
            domain
                .operators()
                .enumerate_legal(
                    OperatorEnumerationBatch::new(
                        &parents,
                        std::slice::from_ref(&descriptor.operator()),
                    ),
                    &mut ApplicationWriter::new(&mut applications),
                    &mut operator_scratch,
                )
                .map_err(SessionError::Domain)?;
            let mut operator_candidates = Vec::new();
            domain
                .operators()
                .apply_batch(
                    &applications,
                    &mut CandidateWriter::new(&mut operator_candidates),
                    &mut operator_scratch,
                )
                .map_err(SessionError::Domain)?;
            application_bytes = application_bytes.saturating_add(vector_bytes(&applications));
            for candidate in operator_candidates {
                let parent = frontier
                    .get(candidate.source_index)
                    .ok_or(SessionError::InvalidSeed)?;
                candidates.push(ProposedCandidate {
                    features: opportunity_features(
                        domain,
                        parent.0.artifact(),
                        &candidate.artifact,
                        descriptor.symbol().as_str(),
                        sequence,
                    ),
                    candidate,
                    operator_symbol: descriptor.symbol().as_str().as_bytes().to_vec(),
                    epoch: sequence,
                    protected_derived: false,
                });
            }
        }
        application_bytes = application_bytes.saturating_add(append_derived_candidates(
            domain,
            &parents,
            &pinned_knowledge,
            &mut operator_scratch,
            usize::try_from(remaining_verifications).unwrap_or(usize::MAX),
            sequence,
            &mut candidates,
        )?);
        test_fault_point("candidate-created");
        candidates =
            retain_novel_candidates(domain, &known, &experience, &roots, &frontier, candidates)?;
        order_by_learned_potential(pinned_model.as_ref(), &mut candidates);
        if candidates.is_empty() {
            break;
        }
        let transient_resident = resident_before_epoch
            .saturating_add(application_bytes)
            .saturating_add(vector_bytes(&candidates))
            .saturating_add(candidate_pipeline_reserve(domain, &candidates));
        if !resource_meter.observe_resident(transient_resident) {
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
        let remaining = usize::try_from(remaining_verifications).unwrap_or(usize::MAX);
        if candidates.len() > remaining {
            candidates.truncate(remaining);
            verification_budget_exhausted = true;
        }
        if candidates.is_empty() {
            break;
        }
        verification_requests +=
            u64::try_from(candidates.len()).map_err(|_| SessionError::Resource)?;
        let parent_keys = frontier
            .iter()
            .map(|(artifact, _)| artifact.key())
            .collect::<Vec<_>>();
        let verification = verify_candidates(domain, &roots, &origins, &parent_keys, candidates)?;
        test_fault_point("verdict-recorded");
        let prior_experience_len = experience.len();
        let prior_experience_capacity = experience.capacity();
        extend_unique_experience(&mut experience, verification.experience);
        test_fault_point("experience-appended");
        let checkpoint_usage = resource_meter
            .usage(verification_requests, checkpoint.len() as u64)
            .map_err(|()| SessionError::Resource)?;
        let experience_checkpoint = encode_bundle(
            domain,
            request,
            &seed_cursor,
            &known,
            &pareto,
            &experience,
            &consequences,
            &measurement_observations,
            &knowledge,
            &learning,
            SessionSeal::Interrupted(checkpoint_usage),
        )?;
        if experience_checkpoint.len() as u64 > request.resources.durable_bytes.get() {
            experience.truncate(prior_experience_len);
            experience.shrink_to(prior_experience_capacity);
            durable_budget_exhausted = true;
            break;
        }
        let experience_resident = worker_resident_bytes
            .saturating_add(resident_state_bytes(
                &known,
                &roots,
                &pareto,
                &frontier,
                &recovered_keys,
                &operators,
                &experience_checkpoint,
                &experience,
                &consequences,
                &measurement_observations,
                &knowledge,
                &learning,
            ))
            .saturating_add(experience_checkpoint.capacity() as u64)
            .saturating_add(durability.pending_bytes());
        if !resource_meter.observe_resident(experience_resident) {
            experience.truncate(prior_experience_len);
            experience.shrink_to(prior_experience_capacity);
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
            append_measurement_observation(
                domain,
                artifact,
                *attempt_id,
                &mut measurement_observations,
            )?;
        }
        test_fault_point("measurement-completed");
        let prior_known_len = known.len();
        let prior_known_capacity = known.capacity();
        let prior_consequence_len = consequences.len();
        let prior_consequence_capacity = consequences.capacity();
        let mut admitted_attempts = Vec::new();
        frontier.clear();
        for ((artifact, origin), attempt_id) in accepted
            .into_iter()
            .zip(accepted_origins)
            .zip(accepted_attempts)
        {
            if !known.iter().any(|known| known.key() == artifact.key()) {
                admitted_attempts.push((artifact.clone(), attempt_id));
                frontier.push((artifact.clone(), origin));
                known.push(artifact);
            }
        }
        test_fault_point("admission-completed");
        if frontier.is_empty() {
            break;
        }
        let previous_keys = pareto.iter().map(VerifiedArtifact::key).collect::<Vec<_>>();
        let proposed_pareto = pareto_union(domain, &request.goals, &known);
        record_admission_consequences(
            domain,
            &request.goals,
            &known,
            &proposed_pareto,
            &previous_keys,
            &experience,
            &admitted_attempts,
            &mut consequences,
        );
        let checkpoint_usage = resource_meter
            .usage(verification_requests, checkpoint.len() as u64)
            .map_err(|()| SessionError::Resource)?;
        let proposed_checkpoint = encode_bundle(
            domain,
            request,
            &seed_cursor,
            &known,
            &proposed_pareto,
            &experience,
            &consequences,
            &measurement_observations,
            &knowledge,
            &learning,
            SessionSeal::Interrupted(checkpoint_usage),
        )?;
        if proposed_checkpoint.len() as u64 > request.resources.durable_bytes.get() {
            known.truncate(prior_known_len);
            known.shrink_to(prior_known_capacity);
            consequences.truncate(prior_consequence_len);
            consequences.shrink_to(prior_consequence_capacity);
            frontier.clear();
            durable_budget_exhausted = true;
            break;
        }
        let proposed_resident = worker_resident_bytes
            .saturating_add(resident_state_bytes(
                &known,
                &roots,
                &proposed_pareto,
                &frontier,
                &recovered_keys,
                &operators,
                &proposed_checkpoint,
                &experience,
                &consequences,
                &measurement_observations,
                &knowledge,
                &learning,
            ))
            .saturating_add(proposed_checkpoint.capacity() as u64)
            .saturating_add(durability.pending_bytes());
        if !resource_meter.observe_resident(proposed_resident) {
            known.truncate(prior_known_len);
            known.shrink_to(prior_known_capacity);
            consequences.truncate(prior_consequence_len);
            consequences.shrink_to(prior_consequence_capacity);
            frontier.clear();
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
            resource_meter,
            proposed_resident,
        );
        if delivery.resource_exhausted() {
            known.truncate(prior_known_len);
            known.shrink_to(prior_known_capacity);
            consequences.truncate(prior_consequence_len);
            consequences.shrink_to(prior_consequence_capacity);
            frontier.clear();
            resident_budget_exhausted = true;
            break;
        }
        pareto = proposed_pareto;
        checkpoint = proposed_checkpoint;
        stopped_by_observer = delivery.stopped();
        success_conditions_satisfied = all_success_conditions_satisfied(
            domain,
            &request.goals,
            &goal_frontiers(domain, &request.goals, &known),
        );
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
    let consolidation_resident = worker_resident_bytes
        .saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &checkpoint,
            &experience,
            &consequences,
            &measurement_observations,
            &knowledge,
            &learning,
        ))
        .saturating_add(durability.pending_bytes())
        .saturating_add((experience.len() as u64).saturating_mul(
            (std::mem::size_of::<DerivationObservation>() as u64).saturating_add(512),
        ))
        .saturating_add(4_096 * 64)
        .saturating_add(experience.iter().fold(0_u64, |bytes, entry| {
            bytes.saturating_add((entry.operator_symbol.capacity() as u64).saturating_mul(2))
        }));
    let can_consolidate = completion != Completion::StoppedByObserver
        && !resource_meter
            .time_exhausted()
            .map_err(|()| SessionError::Resource)?
        && resource_meter.observe_resident(consolidation_resident);
    if can_consolidate {
        let primitive_symbols = domain
            .operators()
            .catalog()
            .iter()
            .map(|descriptor| descriptor.symbol().as_str().as_bytes().to_vec())
            .collect::<BTreeSet<_>>();
        let observations =
            derivation_observations(&experience, &pinned_knowledge, &primitive_symbols)?;
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
        let mut proposed_consequences = consequences.clone();
        for subject in compressed_attempts {
            let consequence = ConsequenceObservation {
                subject,
                kind: ConsequenceKind::Compression,
            };
            if !proposed_consequences.contains(&consequence) {
                proposed_consequences.push(consequence);
            }
        }
        let promoted_resident = worker_resident_bytes
            .saturating_add(resident_state_bytes(
                &known,
                &roots,
                &pareto,
                &frontier,
                &recovered_keys,
                &operators,
                &checkpoint,
                &experience,
                &proposed_consequences,
                &measurement_observations,
                &proposed_knowledge,
                &learning,
            ))
            .saturating_add(knowledge.resident_bytes())
            .saturating_add(durability.pending_bytes())
            .saturating_add(
                (observations.len() as u64)
                    .saturating_mul(std::mem::size_of::<DerivationObservation>() as u64),
            );
        if resource_meter.observe_resident(promoted_resident) {
            knowledge = proposed_knowledge;
            consequences = proposed_consequences;
        } else {
            completion = Completion::ResourceEnvelopeExhausted;
        }
    } else if completion != Completion::StoppedByObserver {
        completion = Completion::ResourceEnvelopeExhausted;
    }
    let learning_resident = worker_resident_bytes
        .saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &checkpoint,
            &experience,
            &consequences,
            &measurement_observations,
            &knowledge,
            &learning,
        ))
        .saturating_add(durability.pending_bytes())
        .saturating_add(
            (experience.len() as u64)
                .saturating_mul(std::mem::size_of::<AttemptObservation>() as u64),
        )
        .saturating_add(LearningState::training_scratch_bytes(experience.len()));
    let can_train = !resource_meter
        .time_exhausted()
        .map_err(|()| SessionError::Resource)?
        && resource_meter.observe_resident(learning_resident);
    if can_train {
        let attempts = experience
            .iter()
            .map(|entry| AttemptObservation {
                id: entry.attempt_id,
                artifact: entry.candidate_key.0,
                claim: entry.claim_digest,
                parent: entry.parent_key.0,
                features: entry.features,
                verdict: match entry.verdict {
                    ExperienceVerdict::Accepted => VerdictTarget::Accepted,
                    ExperienceVerdict::Refuted => VerdictTarget::Refuted,
                    ExperienceVerdict::Unknown => VerdictTarget::Unknown,
                },
                verification_cost: f32::from(
                    u16::try_from(entry.verification_requests).unwrap_or(u16::MAX),
                ),
            })
            .collect::<Vec<_>>();
        let mut examples = derive_targets(&attempts, &consequences);
        let _promotion = learning.learn(&mut examples);
    } else {
        completion = Completion::ResourceEnvelopeExhausted;
    }
    let provisional_checkpoint = encode_bundle(
        domain,
        request,
        &seed_cursor,
        &known,
        &pareto,
        &experience,
        &consequences,
        &measurement_observations,
        &knowledge,
        &learning,
        SessionSeal::Completed(completion, provisional_usage),
    )?;
    let usage = resource_meter
        .usage(verification_requests, provisional_checkpoint.len() as u64)
        .map_err(|()| SessionError::Resource)?;
    checkpoint = encode_bundle(
        domain,
        request,
        &seed_cursor,
        &known,
        &pareto,
        &experience,
        &consequences,
        &measurement_observations,
        &knowledge,
        &learning,
        SessionSeal::Completed(completion, usage),
    )?;
    if checkpoint.len() as u64 > request.resources.durable_bytes.get() {
        return Err(SessionError::Resource);
    }
    let final_resident = worker_resident_bytes
        .saturating_add(resident_state_bytes(
            &known,
            &roots,
            &pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &checkpoint,
            &experience,
            &consequences,
            &measurement_observations,
            &knowledge,
            &learning,
        ))
        .saturating_add(checkpoint.capacity() as u64)
        .saturating_add(durability.pending_bytes());
    if !resource_meter.observe_resident(final_resident) {
        return Err(SessionError::Resource);
    }
    durability
        .submit(checkpoint.clone())
        .map_err(SessionError::Durability)?;
    durability.finish().map_err(SessionError::Durability)?;
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
    experience: &Vec<ExperienceEntry>,
    consequences: &Vec<ConsequenceObservation>,
    measurements: &Vec<MeasurementObservation>,
    knowledge: &KnowledgeState,
    learning: &LearningState,
) -> u64 {
    let records = known.iter().fold(0_u64, |bytes, artifact| {
        bytes
            .saturating_add(std::mem::size_of::<VerifiedArtifactRecord<D>>() as u64)
            .saturating_add(vector_bytes(&artifact.inner.measurements))
    });
    let experience_payloads = experience.iter().fold(0_u64, |bytes, entry| {
        bytes
            .saturating_add(entry.canonical_candidate.capacity() as u64)
            .saturating_add(entry.operator_symbol.capacity() as u64)
    });
    let measurement_payloads = measurements.iter().fold(0_u64, |bytes, observation| {
        observation.values.iter().fold(
            bytes
                .saturating_add(observation.environment.capacity() as u64)
                .saturating_add(vector_bytes(&observation.values)),
            |bytes, value| {
                bytes
                    .saturating_add(value.metric_symbol.capacity() as u64)
                    .saturating_add(value.observation.capacity() as u64)
            },
        )
    });
    records
        .saturating_add(vector_bytes(known))
        .saturating_add(vector_bytes(roots))
        .saturating_add(vector_bytes(pareto))
        .saturating_add(vector_bytes(frontier))
        .saturating_add(vector_bytes(recovered_keys))
        .saturating_add(vector_bytes(operators))
        .saturating_add(vector_bytes(checkpoint))
        .saturating_add(vector_bytes(experience))
        .saturating_add(experience_payloads)
        .saturating_add(vector_bytes(consequences))
        .saturating_add(vector_bytes(measurements))
        .saturating_add(measurement_payloads)
        .saturating_add(knowledge.resident_bytes())
        .saturating_add(learning.resident_bytes())
}

fn vector_bytes<T>(values: &Vec<T>) -> u64 {
    (values.capacity() as u64).saturating_mul(std::mem::size_of::<T>() as u64)
}

fn candidate_pipeline_reserve<D: DomainDefinition>(
    domain: &D,
    candidates: &[ProposedCandidate<D>],
) -> u64 {
    candidates.iter().fold(0_u64, |bytes, candidate| {
        let structural = (domain
            .structure()
            .view(&candidate.candidate.artifact)
            .node_count() as u64)
            .saturating_mul(128);
        let ledger = (std::mem::size_of::<ExperienceEntry>() as u64)
            .saturating_add(candidate.operator_symbol.capacity() as u64)
            .saturating_add(structural)
            .saturating_mul(2);
        bytes.saturating_add(ledger.max(4 * 1024))
    })
}

fn opportunity_features<D: DomainDefinition>(
    domain: &D,
    parent: &D::Artifact,
    candidate: &D::Artifact,
    operator_symbol: &str,
    epoch: u64,
) -> Features {
    let parent_nodes =
        f32::from(u16::try_from(domain.structure().view(parent).node_count()).unwrap_or(u16::MAX));
    let candidate_nodes = f32::from(
        u16::try_from(domain.structure().view(candidate).node_count()).unwrap_or(u16::MAX),
    );
    let reduction = ((parent_nodes - candidate_nodes) / parent_nodes.max(1.0)).clamp(-1.0, 1.0);
    let mut values = [0.0; crate::learning::FEATURE_COUNT];
    values[0] = 1.0;
    values[1] = (parent_nodes / 1024.0).min(1.0);
    values[2] = (candidate_nodes / 1024.0).min(1.0);
    values[3] = reduction;
    values[4] = f32::from(u16::try_from(epoch).unwrap_or(u16::MAX)) / 1024.0;
    let operator_digest = Sha256::digest(operator_symbol.as_bytes());
    let bucket = 5 + usize::from(operator_digest[0] % 8);
    values[bucket] = 1.0;
    values[13] = reduction * values[bucket];
    values[14] = f32::from(candidate_nodes > parent_nodes);
    values[15] = (candidate_nodes / parent_nodes.max(1.0)).min(4.0) / 4.0;
    Features(values)
}

fn append_derived_candidates<D: DomainDefinition>(
    domain: &D,
    parents: &[&D::Artifact],
    knowledge: &KnowledgeRevision,
    scratch: &mut <D::Operators as OperatorAlgebra<D>>::Scratch,
    remaining_verifications: usize,
    epoch: u64,
    output: &mut Vec<ProposedCandidate<D>>,
) -> Result<u64, SessionError<D::Error>> {
    const MAX_DERIVED_CANDIDATES_PER_OPERATOR: usize = 1_024;

    let mut application_bytes = 0_u64;
    let limit = remaining_verifications.min(MAX_DERIVED_CANDIDATES_PER_OPERATOR);
    if limit == 0 {
        return Ok(0);
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
            let mut applications = Vec::new();
            domain
                .operators()
                .enumerate_legal(
                    OperatorEnumerationBatch::new(
                        &stage_parents,
                        std::slice::from_ref(&descriptor.operator()),
                    ),
                    &mut ApplicationWriter::new(&mut applications),
                    scratch,
                )
                .map_err(SessionError::Domain)?;
            application_bytes = application_bytes.saturating_add(vector_bytes(&applications));
            let mut next = Vec::new();
            domain
                .operators()
                .apply_batch(&applications, &mut CandidateWriter::new(&mut next), scratch)
                .map_err(SessionError::Domain)?;
            if next.len() > operator_limit {
                next.truncate(operator_limit);
            }
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
        emitted = emitted.saturating_add(current.len());
        for candidate in current {
            let parent = parents
                .get(candidate.source_index)
                .ok_or(SessionError::CorruptBundle)?;
            output.push(ProposedCandidate {
                features: opportunity_features(domain, parent, &candidate.artifact, symbol, epoch),
                candidate,
                operator_symbol: derived.symbol().to_vec(),
                epoch,
                protected_derived: derived.protected_exploration(),
            });
        }
    }
    Ok(application_bytes)
}

fn order_by_learned_potential<D: DomainDefinition>(
    model: Option<&FtrlModel>,
    candidates: &mut Vec<ProposedCandidate<D>>,
) {
    if model.is_none() {
        let (derived, ordinary): (Vec<_>, Vec<_>) = std::mem::take(candidates)
            .into_iter()
            .partition(|candidate| candidate.protected_derived);
        let mut derived = derived.into_iter();
        let mut ordinary = ordinary.into_iter();
        loop {
            let before = candidates.len();
            if let Some(candidate) = derived.next() {
                candidates.push(candidate);
            }
            candidates.extend(ordinary.by_ref().take(7));
            if candidates.len() == before {
                break;
            }
        }
        return;
    }
    let mut derived_exploration = Vec::new();
    let mut ranked = Vec::new();
    let mut exploration = Vec::new();
    for (index, candidate) in std::mem::take(candidates).into_iter().enumerate() {
        if candidate.protected_derived {
            derived_exploration.push(candidate);
        } else if index.is_multiple_of(8) {
            exploration.push(candidate);
        } else {
            let forecast = model.map(|model| model.forecast(candidate.features));
            ranked.push((candidate, forecast));
        }
    }
    ranked.sort_by(|(left, left_forecast), (right, right_forecast)| {
        left_forecast
            .zip(*right_forecast)
            .map_or(Ordering::Equal, |(left, right)| {
                compare_forecasts(left, right)
            })
            .then_with(|| {
                left.operator_symbol
                    .cmp(&right.operator_symbol)
                    .then_with(|| {
                        left.candidate
                            .source_index
                            .cmp(&right.candidate.source_index)
                    })
            })
    });
    let mut ranked = ranked.into_iter().map(|(candidate, _)| candidate);
    let mut exploration = exploration.into_iter();
    let mut derived_exploration = derived_exploration.into_iter();
    loop {
        let before = candidates.len();
        if let Some(candidate) = derived_exploration.next() {
            candidates.push(candidate);
        }
        if let Some(candidate) = exploration.next() {
            candidates.push(candidate);
        }
        candidates.extend(ranked.by_ref().take(6));
        if candidates.len() == before {
            break;
        }
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

fn derivation_observations<E>(
    experience: &[ExperienceEntry],
    knowledge: &KnowledgeRevision,
    primitive_symbols: &BTreeSet<Vec<u8>>,
) -> Result<Vec<DerivationObservation>, SessionError<E>> {
    experience
        .iter()
        .map(|entry| {
            let operator_steps = if primitive_symbols.contains(&entry.operator_symbol) {
                vec![entry.operator_symbol.clone()]
            } else {
                knowledge
                    .resolve_operator(&entry.operator_symbol)
                    .map(|operator| operator.steps().to_vec())
                    .ok_or(SessionError::CorruptBundle)?
            };
            Ok(DerivationObservation {
                id: entry.attempt_id,
                artifact: entry.candidate_key.0,
                parent: entry.parent_key.0,
                claim: entry.claim_digest,
                operator_identity: entry.operator_symbol.clone(),
                operator_steps,
                accepted: entry.verdict == ExperienceVerdict::Accepted,
            })
        })
        .collect()
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
    domain
        .measurements()
        .measure_batch(
            VerifiedBatch::new(&artifact_refs),
            environment,
            &mut MeasurementWriter::new(&mut measured),
            &mut measurement_scratch,
        )
        .map_err(SessionError::Domain)?;
    let mut by_artifact = (0..stored.len())
        .map(|_| Vec::new())
        .collect::<Vec<Vec<Measurement<D::Metric, D::Observation>>>>();
    for measurement in measured {
        let Some(output) = by_artifact.get_mut(measurement.artifact_index) else {
            return Err(SessionError::InvalidSeed);
        };
        output.push(measurement);
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
            Ok(VerifiedArtifact {
                inner: Arc::new(VerifiedArtifactRecord {
                    key,
                    artifact: stored.artifact,
                    verification: stored.verification,
                    origin_key: stored.origin_key.unwrap_or(key),
                    parent_key: stored.parent_key,
                    measurements,
                    environment: environment.clone(),
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

fn extend_unique_experience(
    experience: &mut Vec<ExperienceEntry>,
    entries: impl IntoIterator<Item = ExperienceEntry>,
) {
    for entry in entries {
        if let Some(existing) = experience
            .iter()
            .find(|existing| existing.attempt_id == entry.attempt_id)
        {
            assert!(
                existing == &entry,
                "a stable attempt identity must name exactly one immutable observation"
            );
        } else {
            experience.push(entry);
        }
    }
}

fn goal_frontiers<D: DomainDefinition>(
    domain: &D,
    goals: &GoalSet<D>,
    known: &[VerifiedArtifact<D>],
) -> Vec<Vec<VerifiedArtifact<D>>> {
    goals
        .goals
        .iter()
        .map(|goal| retain_pareto(domain, goal, known.to_vec()))
        .collect()
}

fn pareto_union<D: DomainDefinition>(
    domain: &D,
    goals: &GoalSet<D>,
    known: &[VerifiedArtifact<D>],
) -> Vec<VerifiedArtifact<D>> {
    let mut pareto = Vec::new();
    for frontier in goal_frontiers(domain, goals, known) {
        extend_unique(&mut pareto, frontier);
    }
    pareto
}

#[expect(
    clippy::too_many_arguments,
    reason = "consequence derivation explicitly names every immutable observation source"
)]
fn record_admission_consequences<D: DomainDefinition>(
    domain: &D,
    goals: &GoalSet<D>,
    known: &[VerifiedArtifact<D>],
    proposed_pareto: &[VerifiedArtifact<D>],
    previous_pareto_keys: &[ArtifactKey],
    experience: &[ExperienceEntry],
    admitted: &[(VerifiedArtifact<D>, [u8; 32])],
    consequences: &mut Vec<ConsequenceObservation>,
) {
    let per_goal = goal_frontiers(domain, goals, known);
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

fn append_measurement_observation<D: DomainDefinition>(
    domain: &D,
    artifact: &VerifiedArtifact<D>,
    subject: [u8; 32],
    observations: &mut Vec<MeasurementObservation>,
) -> Result<(), SessionError<D::Error>> {
    if observations
        .iter()
        .any(|observation| observation.subject == subject)
    {
        return Ok(());
    }
    let mut values = Vec::with_capacity(artifact.inner.measurements.len());
    for measurement in &artifact.inner.measurements {
        let descriptor = domain
            .measurements()
            .schema()
            .iter()
            .find(|descriptor| descriptor.metric() == measurement.metric)
            .ok_or(SessionError::InvalidGoal(crate::GoalError::UnknownMetric))?;
        let mut observation = Vec::new();
        domain
            .measurements()
            .encode_observation(
                measurement.metric,
                &measurement.observation,
                &mut observation,
            )
            .map_err(SessionError::Domain)?;
        values.push(EncodedMeasurement {
            metric_symbol: descriptor.symbol().as_str().as_bytes().to_vec(),
            observation,
        });
    }
    values.sort_unstable_by(|left, right| left.metric_symbol.cmp(&right.metric_symbol));
    observations.push(MeasurementObservation {
        subject,
        environment: artifact.inner.environment.identity().as_bytes().to_vec(),
        values,
    });
    Ok(())
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
    resource_meter: &ProductionResourceMeter,
    resident_state: u64,
) -> DeltaDelivery
where
    D: DomainDefinition,
    O: for<'a> FnMut(ParetoUpdate<'a, D>) -> ControlFlow<()>,
{
    let added = current
        .iter()
        .filter(|artifact| !previous.contains(&artifact.key()))
        .cloned()
        .collect::<Vec<_>>();
    let removed = previous
        .iter()
        .filter(|key| !current.iter().any(|artifact| artifact.key() == **key))
        .copied()
        .collect::<Vec<_>>();
    if added.is_empty() && removed.is_empty() {
        return DeltaDelivery::NoChange;
    }
    let resident_with_export = resident_state
        .saturating_add(vector_bytes(&added))
        .saturating_add(vector_bytes(&removed));
    if !resource_meter.observe_resident(resident_with_export) {
        return DeltaDelivery::ResourceExhausted;
    }
    *sequence += 1;
    let stopped = observer(ParetoUpdate {
        sequence: *sequence,
        added: &added,
        removed: &removed,
    })
    .is_break();
    DeltaDelivery::Delivered { stopped }
}

fn all_success_conditions_satisfied<D: DomainDefinition>(
    domain: &D,
    goals: &GoalSet<D>,
    frontiers: &[Vec<VerifiedArtifact<D>>],
) -> bool {
    goals.goals.iter().zip(frontiers).all(|(goal, frontier)| {
        let Some(success) = goal.success.as_ref() else {
            return false;
        };
        frontier.iter().any(|artifact| {
            success
                .thresholds
                .iter()
                .all(|threshold| threshold_satisfied(domain, artifact, threshold))
        })
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "the canonical v3 import keeps segment cross-validation in one audit path"
)]
fn decode_bundle<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    source: &std::path::Path,
) -> Result<RecoveredBundle<D>, SessionError<D::Error>> {
    let bytes = std::fs::read(source).map_err(SessionError::Durability)?;
    let decoded = durability::decode(&bytes).map_err(|_| SessionError::CorruptBundle)?;
    if decoded.identity != domain.semantic_identity().as_str().as_bytes() {
        return Err(SessionError::IncompatibleBundle);
    }
    let interrupted_usage = validate_session(
        domain,
        request,
        decoded
            .segment(SegmentKind::Session)
            .map_err(|_| SessionError::CorruptBundle)?,
    )?;
    let encoded_revisions = decoded
        .segment(SegmentKind::Revisions)
        .map_err(|_| SessionError::CorruptBundle)?;
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
    let mut input = decoded
        .segment(SegmentKind::Artifacts)
        .map_err(|_| SessionError::CorruptBundle)?;
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
            origin_key: Some(origin_key),
            parent_key,
        });
    }
    if !input.is_empty() {
        return Err(SessionError::CorruptBundle);
    }

    let mut recovery = decoded
        .segment(SegmentKind::Recovery)
        .map_err(|_| SessionError::CorruptBundle)?;
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
    let mut encoded_experience = decoded
        .segment(SegmentKind::Experience)
        .map_err(|_| SessionError::CorruptBundle)?;
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
                &recovered[parent_index].artifact,
                &candidate_artifact,
                operator,
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
    let corpus_assignments = experience
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
    let derivations =
        derivation_observations(&experience, knowledge.pinned_revision(), &primitive_symbols)?;
    if !encoded_experience.is_empty()
        || !learning.corpus_is_valid(&corpus_assignments)
        || !knowledge.validate(&artifact_keys, &derivations, &primitive_symbols)
    {
        return Err(SessionError::CorruptBundle);
    }
    Ok(RecoveredBundle {
        artifacts: recovered,
        pareto_keys,
        experience,
        revisions: Some(revisions),
        interrupted_usage,
        knowledge,
        learning,
        consequences,
        measurements,
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

fn replay_stored<D: DomainDefinition>(
    domain: &D,
    stored: &[StoredArtifact<D>],
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
    let mut replayed = Vec::with_capacity(replay_requests.len());
    let mut kernel_scratch = <D::Kernel as VerificationKernel<D>>::Scratch::default();
    domain
        .kernel()
        .replay_batch(
            VerificationReplayBatch::new(&replay_requests),
            &mut ReplayVerdictWriter::new(&mut replayed),
            &mut kernel_scratch,
        )
        .map_err(SessionError::Domain)?;
    if replayed.len() != stored.len() || replayed.iter().any(|accepted| !accepted) {
        return Err(SessionError::InvalidSeed);
    }
    Ok(())
}

fn replay_experience<D: DomainDefinition>(
    domain: &D,
    known: &[VerifiedArtifact<D>],
    experience: &[ExperienceEntry],
) -> Result<(), SessionError<D::Error>> {
    let mut structure_scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    let mut kernel_scratch = <D::Kernel as VerificationKernel<D>>::Scratch::default();
    for entries in experience.chunks(256) {
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
        let claims = seeds
            .iter()
            .zip(&candidates)
            .map(|(seed, candidate)| {
                domain
                    .kernel()
                    .claim_for_candidate(seed, candidate)
                    .map_err(SessionError::Domain)
            })
            .collect::<Result<Vec<_>, _>>()?;
        for (entry, claim) in entries.iter().zip(&claims) {
            let mut encoded = Vec::new();
            domain
                .kernel()
                .encode_claim(claim, &mut encoded)
                .map_err(SessionError::Domain)?;
            if <[u8; 32]>::from(Sha256::digest(encoded)) != entry.claim_digest {
                return Err(SessionError::CorruptBundle);
            }
        }
        let requests = seeds
            .iter()
            .zip(&candidates)
            .zip(&claims)
            .map(|((seed, candidate), claim)| VerificationRequest {
                seed: *seed,
                candidate,
                claim,
            })
            .collect::<Vec<_>>();
        let mut verdicts = Vec::with_capacity(entries.len());
        domain
            .kernel()
            .verify_batch(
                VerificationBatch::new(&requests),
                &mut VerdictWriter::new(&mut verdicts),
                &mut kernel_scratch,
            )
            .map_err(SessionError::Domain)?;
        if verdicts.len() != entries.len()
            || entries.iter().zip(verdicts).any(|(entry, verdict)| {
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

fn validate_goals<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
) -> Result<(), SessionError<D::Error>> {
    let known = domain.measurements().schema();
    for goal in request.goals.goals.iter() {
        for objective in goal.objectives.iter() {
            if !known
                .iter()
                .any(|descriptor| descriptor.metric() == objective.metric)
            {
                return Err(SessionError::InvalidGoal(crate::GoalError::UnknownMetric));
            }
        }
    }
    Ok(())
}

fn read_seeds<D: DomainDefinition>(
    domain: &D,
    scope: &D::SeedScope,
) -> Result<ReadSeeds<D>, SessionError<D::Error>> {
    let mut cursor = domain.seeds().open(scope).map_err(SessionError::Domain)?;
    let mut scratch = <D::Seeds as SeedSource<D>>::Scratch::default();
    let mut seeds = Vec::new();
    loop {
        let page = domain
            .seeds()
            .read_batch(
                &mut cursor,
                256,
                &mut SeedWriter::new(&mut seeds),
                &mut scratch,
            )
            .map_err(SessionError::Domain)?;
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
    let mut replayed = Vec::new();
    let mut scratch = <D::Kernel as VerificationKernel<D>>::Scratch::default();
    domain
        .kernel()
        .replay_batch(
            VerificationReplayBatch::new(&requests),
            &mut ReplayVerdictWriter::new(&mut replayed),
            &mut scratch,
        )
        .map_err(SessionError::Domain)?;
    if replayed.len() != seeds.len() || replayed.iter().any(|accepted| !accepted) {
        return Err(SessionError::InvalidSeed);
    }
    Ok(())
}

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
    let mut claims = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let origin = *parent_origins
            .get(candidate.candidate.source_index)
            .ok_or(SessionError::InvalidSeed)?;
        let seed = roots.get(origin).ok_or(SessionError::InvalidSeed)?;
        claims.push(
            domain
                .kernel()
                .claim_for_candidate(seed.artifact(), &candidate.candidate.artifact)
                .map_err(SessionError::Domain)?,
        );
    }
    let requests = candidates
        .iter()
        .zip(&claims)
        .map(|(candidate, claim)| {
            let origin = parent_origins[candidate.candidate.source_index];
            VerificationRequest {
                seed: roots[origin].artifact(),
                candidate: &candidate.candidate.artifact,
                claim,
            }
        })
        .collect::<Vec<_>>();
    let mut verdicts = Vec::new();
    let mut scratch = <D::Kernel as VerificationKernel<D>>::Scratch::default();
    domain
        .kernel()
        .verify_batch(
            VerificationBatch::new(&requests),
            &mut VerdictWriter::new(&mut verdicts),
            &mut scratch,
        )
        .map_err(SessionError::Domain)?;
    if verdicts.len() != candidates.len() {
        return Err(SessionError::InvalidSeed);
    }
    let revision = domain.kernel().revision();
    let mut accepted = Vec::new();
    let mut experience = Vec::with_capacity(candidates.len());
    for (((candidate, (candidate_key, canonical_candidate)), claim), verdict) in candidates
        .into_iter()
        .zip(candidate_encodings)
        .zip(claims)
        .zip(verdicts)
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

fn retain_pareto<D: DomainDefinition>(
    domain: &D,
    goal: &OptimizationGoal<D>,
    artifacts: Vec<VerifiedArtifact<D>>,
) -> Vec<VerifiedArtifact<D>> {
    let artifacts = artifacts
        .into_iter()
        .filter(|artifact| {
            goal.constraints
                .iter()
                .all(|constraint| threshold_satisfied(domain, artifact, constraint))
        })
        .collect::<Vec<_>>();
    let dominated = (0..artifacts.len())
        .map(|right| {
            (0..artifacts.len()).any(|left| {
                left != right && dominates(domain, goal, &artifacts[left], &artifacts[right])
            })
        })
        .collect::<Vec<_>>();
    artifacts
        .into_iter()
        .zip(dominated)
        .filter_map(|(artifact, dominated)| (!dominated).then_some(artifact))
        .collect()
}

fn threshold_satisfied<D: DomainDefinition>(
    domain: &D,
    artifact: &VerifiedArtifact<D>,
    threshold: &crate::MeasurementConstraint<D>,
) -> bool {
    let Some(measurement) = artifact
        .inner
        .measurements
        .iter()
        .find(|measurement| measurement.metric == threshold.metric)
    else {
        return false;
    };
    let Ok(ordering) = domain.measurements().compare(
        threshold.metric,
        &measurement.observation,
        &threshold.threshold,
    ) else {
        return false;
    };
    matches!(
        (threshold.relation, ordering),
        (
            ThresholdRelation::AtMost,
            MetricOrdering::Less | MetricOrdering::Equal
        ) | (
            ThresholdRelation::AtLeast,
            MetricOrdering::Greater | MetricOrdering::Equal
        )
    )
}

fn dominates<D: DomainDefinition>(
    domain: &D,
    goal: &OptimizationGoal<D>,
    left: &VerifiedArtifact<D>,
    right: &VerifiedArtifact<D>,
) -> bool {
    if !same_correctness_claim(domain, left, right) {
        return false;
    }
    let mut strictly_better = false;
    for objective in goal.objectives.iter() {
        let Some(left_value) = left
            .inner
            .measurements
            .iter()
            .find(|measurement| measurement.metric == objective.metric)
        else {
            return false;
        };
        let Some(right_value) = right
            .inner
            .measurements
            .iter()
            .find(|measurement| measurement.metric == objective.metric)
        else {
            return false;
        };
        let Ok(ordering) = domain.measurements().compare(
            objective.metric,
            &left_value.observation,
            &right_value.observation,
        ) else {
            return false;
        };
        let better = matches!(
            (objective.direction, ordering),
            (Direction::Minimize, MetricOrdering::Less)
                | (Direction::Maximize, MetricOrdering::Greater)
        );
        let worse = matches!(
            (objective.direction, ordering),
            (Direction::Minimize, MetricOrdering::Greater)
                | (Direction::Maximize, MetricOrdering::Less)
        );
        if worse {
            return false;
        }
        strictly_better |= better;
    }
    strictly_better
}

fn same_correctness_claim<D: DomainDefinition>(
    domain: &D,
    left: &VerifiedArtifact<D>,
    right: &VerifiedArtifact<D>,
) -> bool {
    let mut left_claim = Vec::new();
    let mut right_claim = Vec::new();
    domain
        .kernel()
        .encode_claim(&left.inner.verification.claim, &mut left_claim)
        .is_ok()
        && domain
            .kernel()
            .encode_claim(&right.inner.verification.claim, &mut right_claim)
            .is_ok()
        && left_claim == right_claim
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "the canonical bundle encoder explicitly receives every restart-complete state segment"
)]
fn encode_bundle<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    seed_cursor: &[u8],
    artifacts: &[VerifiedArtifact<D>],
    pareto: &[VerifiedArtifact<D>],
    experience: &[ExperienceEntry],
    consequences: &[ConsequenceObservation],
    measurements: &[MeasurementObservation],
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
    let mut encoded_experience = Vec::with_capacity(8 + experience.len() * 97);
    push_u64(&mut encoded_experience, experience.len() as u64);
    for entry in experience {
        encoded_experience.extend_from_slice(&entry.attempt_id);
        encoded_experience.extend_from_slice(entry.candidate_key.as_bytes());
        encoded_experience.extend_from_slice(&entry.claim_digest);
        encoded_experience.extend_from_slice(entry.origin_key.as_bytes());
        encoded_experience.extend_from_slice(entry.parent_key.as_bytes());
        push_bytes(
            &mut encoded_experience,
            entry.canonical_candidate.as_slice(),
        );
        encoded_experience.push(entry.verdict as u8);
        push_bytes(&mut encoded_experience, &entry.operator_symbol);
        for feature in entry.features.0 {
            encoded_experience.extend_from_slice(&feature.to_bits().to_le_bytes());
        }
        encoded_experience.extend_from_slice(&entry.verification_requests.to_le_bytes());
        encoded_experience.extend_from_slice(&entry.epoch.to_le_bytes());
    }
    push_u64(&mut encoded_experience, consequences.len() as u64);
    for consequence in consequences {
        encoded_experience.extend_from_slice(&consequence.subject);
        encoded_experience.push(match consequence.kind {
            ConsequenceKind::Admitted => 1,
            ConsequenceKind::ParetoImprovement => 2,
            ConsequenceKind::CrossGoalUse => 3,
            ConsequenceKind::Compression => 4,
        });
    }
    push_u64(&mut encoded_experience, measurements.len() as u64);
    for measurement in measurements {
        encoded_experience.extend_from_slice(&measurement.subject);
        push_bytes(&mut encoded_experience, &measurement.environment);
        push_u64(&mut encoded_experience, measurement.values.len() as u64);
        for value in &measurement.values {
            push_bytes(&mut encoded_experience, &value.metric_symbol);
            push_bytes(&mut encoded_experience, &value.observation);
        }
    }
    let session = encode_session(domain, request, seed_cursor, session_seal)?;
    let bundle = durability::seal(
        identity.as_str().as_bytes(),
        &[
            Segment {
                kind: SegmentKind::Session,
                payload: session,
            },
            Segment {
                kind: SegmentKind::Revisions,
                payload: revisions,
            },
            Segment {
                kind: SegmentKind::Artifacts,
                payload: artifact_payload,
            },
            Segment {
                kind: SegmentKind::Experience,
                payload: encoded_experience,
            },
            Segment {
                kind: SegmentKind::Recovery,
                payload: recovery,
            },
        ],
    )
    .map_err(|_| SessionError::Resource)?;
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
    }
    knowledge.update(state.revision_digest(semantic_identity));
    RevisionIds {
        knowledge: knowledge.finalize().into(),
        model: learning.revision_digest(semantic_identity),
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the Session segment is encoded in declared canonical field order"
)]
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
    let mut goals = Vec::new();
    push_u64(&mut goals, request.goals.goals.as_slice().len() as u64);
    for goal in request.goals.goals.iter() {
        push_u64(&mut goals, goal.constraints.len() as u64);
        for constraint in &goal.constraints {
            encode_metric(domain, constraint.metric, &mut goals)?;
            goals.push(match constraint.relation {
                ThresholdRelation::AtMost => 0,
                ThresholdRelation::AtLeast => 1,
            });
            let mut observation = Vec::new();
            domain
                .measurements()
                .encode_observation(constraint.metric, &constraint.threshold, &mut observation)
                .map_err(SessionError::Domain)?;
            push_bytes(&mut goals, &observation);
        }
        push_u64(&mut goals, goal.objectives.as_slice().len() as u64);
        for objective in goal.objectives.iter() {
            encode_metric(domain, objective.metric, &mut goals)?;
            goals.push(match objective.direction {
                Direction::Minimize => 0,
                Direction::Maximize => 1,
            });
        }
        push_u64(
            &mut goals,
            goal.preference.priority_tiers.as_slice().len() as u64,
        );
        for tier in goal.preference.priority_tiers.iter() {
            push_u64(&mut goals, tier.as_slice().len() as u64);
            for metric in tier.iter() {
                encode_metric(domain, *metric, &mut goals)?;
            }
        }
        push_u64(&mut goals, goal.preference.tolerances.len() as u64);
        for tolerance in &goal.preference.tolerances {
            encode_metric(domain, tolerance.metric, &mut goals)?;
            let mut observation = Vec::new();
            domain
                .measurements()
                .encode_observation(tolerance.metric, &tolerance.amount, &mut observation)
                .map_err(SessionError::Domain)?;
            push_bytes(&mut goals, &observation);
        }
        match &goal.success {
            Some(success) => {
                goals.push(1);
                push_u64(&mut goals, success.thresholds.as_slice().len() as u64);
                for threshold in success.thresholds.iter() {
                    encode_metric(domain, threshold.metric, &mut goals)?;
                    goals.push(match threshold.relation {
                        ThresholdRelation::AtMost => 0,
                        ThresholdRelation::AtLeast => 1,
                    });
                    let mut observation = Vec::new();
                    domain
                        .measurements()
                        .encode_observation(
                            threshold.metric,
                            &threshold.threshold,
                            &mut observation,
                        )
                        .map_err(SessionError::Domain)?;
                    push_bytes(&mut goals, &observation);
                }
            }
            None => goals.push(0),
        }
    }
    let mut payload = Vec::new();
    payload.push(u8::from(matches!(session_seal, SessionSeal::Completed(..))));
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

fn encode_metric<D: DomainDefinition>(
    domain: &D,
    metric: D::Metric,
    output: &mut Vec<u8>,
) -> Result<(), SessionError<D::Error>> {
    let descriptor = domain
        .measurements()
        .schema()
        .iter()
        .find(|descriptor| descriptor.metric() == metric)
        .ok_or(SessionError::InvalidGoal(crate::GoalError::UnknownMetric))?;
    push_bytes(output, descriptor.symbol().as_str().as_bytes());
    Ok(())
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
