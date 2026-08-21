use std::collections::HashSet;
use std::io::Write;
use std::ops::ControlFlow;
use std::sync::Arc;

use atomic_write_file::AtomicWriteFile;
use sha2::{Digest, Sha256};

use crate::bundle::DomainBundle;
use crate::domain::{
    ApplicationWriter, Candidate, CandidateWriter, DomainDefinition, OperatorAlgebra,
    OperatorEnumerationBatch, ReplayVerdictWriter, Seed, SeedSource, SeedWriter,
    StructuralProtocol, Verdict, VerdictWriter, VerificationBatch, VerificationKernel,
    VerificationRecord, VerificationReplayBatch, VerificationReplayRequest, VerificationRequest,
};
use crate::durability::{self, Segment, SegmentKind};
use crate::goal::{Direction, GoalSet, OptimizationGoal, ThresholdRelation};
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
type OriginatedStoredArtifact<D> = (StoredArtifact<D>, usize);
struct RecoveredBundle<D: DomainDefinition> {
    artifacts: Vec<StoredArtifact<D>>,
    pareto_keys: Vec<ArtifactKey>,
    experience: Vec<ExperienceEntry>,
    revisions: Option<RevisionIds>,
}

impl<D: DomainDefinition> Default for RecoveredBundle<D> {
    fn default() -> Self {
        Self {
            artifacts: Vec::new(),
            pareto_keys: Vec::new(),
            experience: Vec::new(),
            revisions: None,
        }
    }
}

#[derive(Clone, Copy)]
struct RevisionIds {
    knowledge: [u8; 32],
    model: [u8; 32],
}

#[derive(Clone, Copy)]
enum ExperienceVerdict {
    Accepted = 1,
    Refuted = 2,
    Unknown = 3,
}

#[derive(Clone)]
struct ExperienceEntry {
    candidate_key: ArtifactKey,
    origin_key: ArtifactKey,
    parent_key: ArtifactKey,
    canonical_candidate: Vec<u8>,
    verdict: ExperienceVerdict,
}

struct VerificationOutcome<D: DomainDefinition> {
    accepted: Vec<OriginatedStoredArtifact<D>>,
    experience: Vec<ExperienceEntry>,
}

struct ReadSeeds<D: DomainDefinition> {
    seeds: Vec<Seed<D>>,
    encoded_cursor: Vec<u8>,
}
const WORKER_STACK_BYTES: usize = 2 * 1024 * 1024;

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
    let worker_resident_bytes =
        (request.resources.worker_threads.get() as u64).saturating_mul(WORKER_STACK_BYTES as u64);
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
        .map(|source| decode_bundle(domain, source))
        .transpose()?
        .unwrap_or_default();
    let recovered_keys = recovered_bundle.pareto_keys;
    let recovered_stored = recovered_bundle.artifacts;
    let mut experience = recovered_bundle.experience;
    let recovered_revisions = recovered_bundle.revisions;
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
        .and_then(|count| u64::try_from(count).ok())
        .ok_or(SessionError::Resource)?;
    let verification_budget = request.resources.verification_requests.get();
    if required_replays > verification_budget {
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
    if recovered_revisions.is_some_and(|revisions| {
        let expected = revision_ids(domain.semantic_identity().as_str(), &recovered);
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
    let mut checkpoint = encode_bundle(
        domain,
        request,
        &seed_cursor,
        &known,
        &pareto,
        &experience,
        None,
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
    for artifact in recovered {
        if let Some(origin) = roots
            .iter()
            .position(|root| root.key() == artifact.inner.origin_key)
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
    ));
    if !resource_meter.observe_resident(initial_resident) {
        return Err(SessionError::Resource);
    }
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
        .time_exhausted()
        .map_err(|()| SessionError::Resource)?;
    let mut verification_requests = required_replays;
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
        let mut applications = Vec::new();
        domain
            .operators()
            .enumerate_legal(
                OperatorEnumerationBatch::new(&parents, &operators),
                &mut ApplicationWriter::new(&mut applications),
                &mut operator_scratch,
            )
            .map_err(SessionError::Domain)?;
        let mut candidates = Vec::new();
        domain
            .operators()
            .apply_batch(
                &applications,
                &mut CandidateWriter::new(&mut candidates),
                &mut operator_scratch,
            )
            .map_err(SessionError::Domain)?;
        candidates = retain_novel_candidates(domain, &known, candidates)?;
        if candidates.is_empty() {
            break;
        }
        let transient_resident = resident_before_epoch
            .saturating_add(vector_bytes(&applications))
            .saturating_add(vector_bytes(&candidates));
        if !resource_meter.observe_resident(transient_resident) {
            resident_budget_exhausted = true;
            break;
        }
        if resource_meter
            .time_exhausted()
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
        let prior_experience_len = experience.len();
        experience.extend(verification.experience);
        let experience_checkpoint = encode_bundle(
            domain,
            request,
            &seed_cursor,
            &known,
            &pareto,
            &experience,
            None,
        )?;
        if experience_checkpoint.len() as u64 > request.resources.durable_bytes.get() {
            experience.truncate(prior_experience_len);
            durable_budget_exhausted = true;
            break;
        }
        checkpoint = experience_checkpoint;
        if resource_meter
            .time_exhausted()
            .map_err(|()| SessionError::Resource)?
        {
            time_exhausted = true;
            break;
        }
        let accepted_origins = verification
            .accepted
            .iter()
            .map(|(_, origin)| *origin)
            .collect::<Vec<_>>();
        let accepted = materialize(
            domain,
            verification
                .accepted
                .into_iter()
                .map(|(artifact, _)| artifact)
                .collect(),
            &environment,
        )?;
        let prior_known_len = known.len();
        frontier.clear();
        for (artifact, origin) in accepted.into_iter().zip(accepted_origins) {
            if !known.iter().any(|known| known.key() == artifact.key()) {
                frontier.push((artifact.clone(), origin));
                known.push(artifact);
            }
        }
        if frontier.is_empty() {
            break;
        }
        let previous_keys = pareto.iter().map(VerifiedArtifact::key).collect::<Vec<_>>();
        let proposed_pareto = pareto_union(domain, &request.goals, &known);
        let proposed_checkpoint = encode_bundle(
            domain,
            request,
            &seed_cursor,
            &known,
            &proposed_pareto,
            &experience,
            None,
        )?;
        if proposed_checkpoint.len() as u64 > request.resources.durable_bytes.get() {
            known.truncate(prior_known_len);
            frontier.clear();
            durable_budget_exhausted = true;
            break;
        }
        let proposed_resident = worker_resident_bytes.saturating_add(resident_state_bytes(
            &known,
            &roots,
            &proposed_pareto,
            &frontier,
            &recovered_keys,
            &operators,
            &proposed_checkpoint,
        ));
        if !resource_meter.observe_resident(proposed_resident) {
            known.truncate(prior_known_len);
            frontier.clear();
            resident_budget_exhausted = true;
            break;
        }
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
            .time_exhausted()
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
    let provisional_checkpoint = encode_bundle(
        domain,
        request,
        &seed_cursor,
        &known,
        &pareto,
        &experience,
        Some((completion, provisional_usage)),
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
        Some((completion, usage)),
    )?;
    if checkpoint.len() as u64 > request.resources.durable_bytes.get() {
        return Err(SessionError::Resource);
    }
    publish_bundle(request, &checkpoint)?;
    let target = request.bundle.target().to_path_buf();
    Ok(SessionOutcome {
        completion,
        pareto: ParetoSnapshot { artifacts: pareto },
        usage,
        bundle: DomainBundle::published(target),
    })
}

fn resident_state_bytes<D: DomainDefinition, O>(
    known: &Vec<VerifiedArtifact<D>>,
    roots: &Vec<VerifiedArtifact<D>>,
    pareto: &Vec<VerifiedArtifact<D>>,
    frontier: &Vec<(VerifiedArtifact<D>, usize)>,
    recovered_keys: &Vec<ArtifactKey>,
    operators: &Vec<O>,
    checkpoint: &Vec<u8>,
) -> u64 {
    let records = known.iter().fold(0_u64, |bytes, artifact| {
        bytes
            .saturating_add(std::mem::size_of::<VerifiedArtifactRecord<D>>() as u64)
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
}

fn vector_bytes<T>(values: &Vec<T>) -> u64 {
    (values.capacity() as u64).saturating_mul(std::mem::size_of::<T>() as u64)
}

fn retain_novel_candidates<D: DomainDefinition>(
    domain: &D,
    known: &[VerifiedArtifact<D>],
    candidates: Vec<Candidate<D>>,
) -> Result<Vec<Candidate<D>>, SessionError<D::Error>> {
    let mut keys = known
        .iter()
        .map(VerifiedArtifact::key)
        .collect::<HashSet<_>>();
    let mut scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    let identity = domain.semantic_identity();
    candidates
        .into_iter()
        .filter_map(|candidate| {
            let mut canonical = Vec::new();
            match domain.structure().encode_canonical(
                &candidate.artifact,
                &mut canonical,
                &mut scratch,
            ) {
                Ok(()) => {
                    let key = ArtifactKey(stable_digest(identity.as_str(), &canonical));
                    keys.insert(key).then_some(Ok(candidate))
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
    source: &std::path::Path,
) -> Result<RecoveredBundle<D>, SessionError<D::Error>> {
    let bytes = std::fs::read(source).map_err(SessionError::Durability)?;
    let decoded = durability::decode(&bytes).map_err(|_| SessionError::CorruptBundle)?;
    if decoded.identity != domain.semantic_identity().as_str().as_bytes() {
        return Err(SessionError::IncompatibleBundle);
    }
    validate_session(
        domain,
        decoded
            .segment(SegmentKind::Session)
            .map_err(|_| SessionError::CorruptBundle)?,
    )?;
    let encoded_revisions = decoded
        .segment(SegmentKind::Revisions)
        .map_err(|_| SessionError::CorruptBundle)?;
    if encoded_revisions.len() != 64 {
        return Err(SessionError::CorruptBundle);
    }
    let revisions = RevisionIds {
        knowledge: encoded_revisions[..32]
            .try_into()
            .expect("Knowledge Revision ID is exactly 32 bytes"),
        model: encoded_revisions[32..]
            .try_into()
            .expect("Model Revision ID is exactly 32 bytes"),
    };
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
    if experience_count > encoded_experience.len().saturating_div(105) {
        return Err(SessionError::CorruptBundle);
    }
    let mut experience = Vec::with_capacity(experience_count);
    for _ in 0..experience_count {
        let mut read_key = || -> Result<ArtifactKey, SessionError<D::Error>> {
            Ok(ArtifactKey(
                take_bundle(&mut encoded_experience, 32)?
                    .try_into()
                    .expect("exactly 32 Experience key bytes were taken"),
            ))
        };
        let candidate_key = read_key()?;
        let origin_key = read_key()?;
        let parent_key = read_key()?;
        let canonical_candidate = take_sized(&mut encoded_experience)?.to_vec();
        if ArtifactKey(stable_digest(identity.as_str(), &canonical_candidate)) != candidate_key
            || domain
                .structure()
                .decode_canonical(&canonical_candidate, &mut structure_scratch)
                .is_err()
        {
            return Err(SessionError::CorruptBundle);
        }
        let verdict = match take_bundle(&mut encoded_experience, 1)?[0] {
            1 => ExperienceVerdict::Accepted,
            2 => ExperienceVerdict::Refuted,
            3 => ExperienceVerdict::Unknown,
            _ => return Err(SessionError::CorruptBundle),
        };
        experience.push(ExperienceEntry {
            candidate_key,
            origin_key,
            parent_key,
            canonical_candidate,
            verdict,
        });
    }
    if take_bundle(&mut encoded_experience, 1)? != [0] || !encoded_experience.is_empty() {
        return Err(SessionError::CorruptBundle);
    }
    Ok(RecoveredBundle {
        artifacts: recovered,
        pareto_keys,
        experience,
        revisions: Some(revisions),
    })
}

fn validate_session<D: DomainDefinition>(
    domain: &D,
    mut input: &[u8],
) -> Result<(), SessionError<D::Error>> {
    if take_bundle(&mut input, 1)? != [1] {
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
    let environment = take_sized(&mut input)?;
    if environment.is_empty() || !matches!(take_bundle(&mut input, 1)?[0], 1..=4) {
        return Err(SessionError::CorruptBundle);
    }
    for _ in 0..4 {
        read_bundle_u64(&mut input)?;
    }
    take_bundle(&mut input, 12)?;
    take_bundle(&mut input, 12)?;
    if !input.is_empty() {
        return Err(SessionError::CorruptBundle);
    }
    Ok(())
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

fn verify_candidates<D: DomainDefinition>(
    domain: &D,
    roots: &[VerifiedArtifact<D>],
    parent_origins: &[usize],
    parent_keys: &[ArtifactKey],
    candidates: Vec<Candidate<D>>,
) -> Result<VerificationOutcome<D>, SessionError<D::Error>> {
    let mut structure_scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    let identity = domain.semantic_identity();
    let candidate_encodings = candidates
        .iter()
        .map(|candidate| {
            let mut canonical = Vec::new();
            domain
                .structure()
                .encode_canonical(&candidate.artifact, &mut canonical, &mut structure_scratch)
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
            .get(candidate.source_index)
            .ok_or(SessionError::InvalidSeed)?;
        let seed = roots.get(origin).ok_or(SessionError::InvalidSeed)?;
        claims.push(
            domain
                .kernel()
                .claim_for_candidate(seed.artifact(), &candidate.artifact)
                .map_err(SessionError::Domain)?,
        );
    }
    let requests = candidates
        .iter()
        .zip(&claims)
        .map(|(candidate, claim)| {
            let origin = parent_origins[candidate.source_index];
            VerificationRequest {
                seed: roots[origin].artifact(),
                candidate: &candidate.artifact,
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
        let origin = parent_origins[candidate.source_index];
        let origin_key = roots[origin].key();
        let parent_key = parent_keys[candidate.source_index];
        let experience_verdict = match verdict {
            Verdict::Accepted { evidence } => {
                accepted.push((
                    StoredArtifact {
                        artifact: candidate.artifact,
                        verification: VerificationRecord {
                            claim,
                            evidence,
                            kernel_revision: revision,
                        },
                        origin_key: Some(origin_key),
                        parent_key: Some(parent_key),
                    },
                    origin,
                ));
                ExperienceVerdict::Accepted
            }
            Verdict::Refuted => ExperienceVerdict::Refuted,
            Verdict::Unknown => ExperienceVerdict::Unknown,
        };
        experience.push(ExperienceEntry {
            candidate_key,
            origin_key,
            parent_key,
            canonical_candidate,
            verdict: experience_verdict,
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

fn encode_bundle<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    seed_cursor: &[u8],
    artifacts: &[VerifiedArtifact<D>],
    pareto: &[VerifiedArtifact<D>],
    experience: &[ExperienceEntry],
    session_outcome: Option<(Completion, ResourceUsage)>,
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
    let revision_ids = revision_ids(identity.as_str(), artifacts);
    let mut revisions = Vec::with_capacity(64);
    revisions.extend_from_slice(&revision_ids.knowledge);
    revisions.extend_from_slice(&revision_ids.model);
    let mut recovery = Vec::with_capacity(8 + pareto.len() * 32);
    push_u64(&mut recovery, pareto.len() as u64);
    for artifact in pareto {
        recovery.extend_from_slice(artifact.key().as_bytes());
    }
    let mut encoded_experience = Vec::with_capacity(8 + experience.len() * 97);
    push_u64(&mut encoded_experience, experience.len() as u64);
    for entry in experience {
        encoded_experience.extend_from_slice(entry.candidate_key.as_bytes());
        encoded_experience.extend_from_slice(entry.origin_key.as_bytes());
        encoded_experience.extend_from_slice(entry.parent_key.as_bytes());
        push_bytes(
            &mut encoded_experience,
            entry.canonical_candidate.as_slice(),
        );
        encoded_experience.push(entry.verdict as u8);
    }
    encoded_experience.push(0);
    let session = encode_session(domain, request, seed_cursor, session_outcome)?;
    durability::seal(
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
    .map_err(|_| SessionError::Resource)
}

fn revision_ids<D: DomainDefinition>(
    semantic_identity: &str,
    artifacts: &[VerifiedArtifact<D>],
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
    let mut model = Sha256::new();
    model.update(b"reflex-bootstrap-model-revision-v1\0");
    model.update((semantic_identity.len() as u64).to_le_bytes());
    model.update(semantic_identity.as_bytes());
    RevisionIds {
        knowledge: knowledge.finalize().into(),
        model: model.finalize().into(),
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
    session_outcome: Option<(Completion, ResourceUsage)>,
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
    payload.push(1);
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
    let usage = session_outcome.map_or_else(ResourceUsage::default, |(_, usage)| usage);
    payload.push(match session_outcome.map(|(completion, _)| completion) {
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

fn publish_bundle<D: DomainDefinition>(
    request: &ImprovementRequest<D>,
    bytes: &[u8],
) -> Result<u64, SessionError<D::Error>> {
    let target = request.bundle.target();
    let mut file = AtomicWriteFile::open(target).map_err(SessionError::Durability)?;
    if let Err(error) = file.write_all(bytes) {
        let _ = file.discard();
        return Err(SessionError::Durability(error));
    }
    file.commit().map_err(SessionError::Durability)?;
    u64::try_from(bytes.len()).map_err(|_| SessionError::Resource)
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
