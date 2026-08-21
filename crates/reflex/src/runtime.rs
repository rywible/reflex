use std::fs::OpenOptions;
use std::io::Write;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::bundle::DomainBundle;
use crate::domain::{
    ApplicationWriter, Candidate, CandidateWriter, DomainDefinition, OperatorAlgebra,
    OperatorEnumerationBatch, ReplayVerdictWriter, Seed, SeedSource, SeedWriter,
    StructuralProtocol, Verdict, VerdictWriter, VerificationBatch, VerificationKernel,
    VerificationRecord, VerificationReplayBatch, VerificationReplayRequest, VerificationRequest,
};
use crate::goal::{Direction, GoalSet, OptimizationGoal, ThresholdRelation};
use crate::measurement::{
    Measurement, MeasurementSpace, MeasurementWriter, MetricOrdering, VerifiedBatch,
};
use crate::session::{
    ArtifactKey, Completion, ImprovementRequest, ParetoSnapshot, ParetoUpdate, ResourceUsage,
    SessionError, SessionOutcome, VerifiedArtifact, VerifiedArtifactRecord,
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

type StoredArtifact<D> = (<D as DomainDefinition>::Artifact, VerificationRecord<D>);

#[expect(
    clippy::too_many_lines,
    reason = "the Runtime Controller keeps one Session epoch legible as a single orchestration path"
)]
pub(crate) fn improve<D, O>(
    domain: &D,
    request: &ImprovementRequest<D>,
    mut observer: O,
) -> Result<SessionOutcome<D>, SessionError<D::Error>>
where
    D: DomainDefinition,
    O: for<'a> FnMut(ParetoUpdate<'a, D>) -> ControlFlow<()>,
{
    validate_goals(domain, request)?;

    let recovered = request
        .bundle
        .source()
        .map(|source| load_bundle(domain, source))
        .transpose()?
        .unwrap_or_default();
    let recovered_replays = recovered.len();

    let mut seeds = read_seeds(domain, &request.seeds)?;
    let seed_replays = seeds.len();
    replay_seeds(domain, &seeds)?;
    if seeds.is_empty() {
        return Err(SessionError::InvalidSeed);
    }

    let mut applications = Vec::new();
    let seed_artifacts = seeds.iter().map(|seed| &seed.artifact).collect::<Vec<_>>();
    let operators = domain
        .operators()
        .catalog()
        .iter()
        .map(crate::OperatorDescriptor::operator)
        .collect::<Vec<_>>();
    let mut operator_scratch = <D::Operators as OperatorAlgebra<D>>::Scratch::default();
    domain
        .operators()
        .enumerate_legal(
            OperatorEnumerationBatch::new(&seed_artifacts, &operators),
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

    let required_replays = recovered_replays
        .checked_add(seed_replays)
        .and_then(|count| u64::try_from(count).ok())
        .ok_or(SessionError::Resource)?;
    let verification_budget = request.resources.verification_requests.get();
    if required_replays > verification_budget {
        return Err(SessionError::Resource);
    }
    let available_candidate_requests =
        usize::try_from(verification_budget - required_replays).unwrap_or(usize::MAX);
    let verification_budget_exhausted = candidates.len() > available_candidate_requests;
    candidates.truncate(available_candidate_requests);
    let candidate_requests = u64::try_from(candidates.len()).map_err(|_| SessionError::Resource)?;
    let verification_requests = required_replays + candidate_requests;
    let accepted = verify_candidates(domain, &seeds, candidates)?;

    let mut pending = recovered;
    pending.extend(
        seeds
            .drain(..)
            .map(|seed| (seed.artifact, seed.verification)),
    );
    pending.extend(accepted);

    let environment = crate::MeasurementEnvironment::local_process();
    let artifact_refs = pending
        .iter()
        .map(|(artifact, _)| artifact)
        .collect::<Vec<_>>();
    let mut measured = Vec::new();
    let mut measurement_scratch = <D::Measurements as MeasurementSpace<D>>::Scratch::default();
    domain
        .measurements()
        .measure_batch(
            VerifiedBatch::new(&artifact_refs),
            &environment,
            &mut MeasurementWriter::new(&mut measured),
            &mut measurement_scratch,
        )
        .map_err(SessionError::Domain)?;

    let mut measurements_by_artifact = (0..pending.len())
        .map(|_| Vec::new())
        .collect::<Vec<Vec<Measurement<D::Metric, D::Observation>>>>();
    for measurement in measured {
        let Some(output) = measurements_by_artifact.get_mut(measurement.artifact_index) else {
            return Err(SessionError::InvalidSeed);
        };
        output.push(measurement);
    }

    let mut structure_scratch = <D::Structure as crate::StructuralProtocol<D>>::Scratch::default();
    let mut verified = Vec::with_capacity(pending.len());
    for ((artifact, verification), measurements) in
        pending.into_iter().zip(measurements_by_artifact)
    {
        let mut canonical = Vec::new();
        domain
            .structure()
            .encode_canonical(&artifact, &mut canonical, &mut structure_scratch)
            .map_err(SessionError::Domain)?;
        verified.push(VerifiedArtifact {
            inner: Arc::new(VerifiedArtifactRecord {
                key: ArtifactKey(stable_digest(&canonical)),
                artifact,
                verification,
                measurements,
                environment: environment.clone(),
            }),
        });
    }
    let mut unique = Vec::with_capacity(verified.len());
    for artifact in verified {
        if !unique
            .iter()
            .any(|retained: &VerifiedArtifact<D>| retained.key() == artifact.key())
        {
            unique.push(artifact);
        }
    }

    let goal_frontiers = request
        .goals
        .goals
        .iter()
        .map(|goal| retain_pareto(domain, goal, unique.clone()))
        .collect::<Vec<_>>();
    let mut pareto = Vec::new();
    for frontier in &goal_frontiers {
        for artifact in frontier {
            if !pareto
                .iter()
                .any(|retained: &VerifiedArtifact<D>| retained.key() == artifact.key())
            {
                pareto.push(artifact.clone());
            }
        }
    }
    let success_conditions_satisfied =
        all_success_conditions_satisfied(domain, &request.goals, &goal_frontiers);
    let completion = if observer(ParetoUpdate {
        sequence: 1,
        added: &pareto,
        removed: &[],
    })
    .is_break()
    {
        Completion::StoppedByObserver
    } else if success_conditions_satisfied {
        Completion::SuccessConditionsSatisfied
    } else if verification_budget_exhausted {
        Completion::ResourceEnvelopeExhausted
    } else {
        Completion::NoEligibleWork
    };

    let durable_bytes = publish_bundle(domain, request, &pareto)?;
    let target = request.bundle.target().to_path_buf();
    Ok(SessionOutcome {
        completion,
        pareto: ParetoSnapshot { artifacts: pareto },
        usage: ResourceUsage {
            verification_requests,
            durable_bytes,
        },
        bundle: DomainBundle::published(target),
    })
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

fn load_bundle<D: DomainDefinition>(
    domain: &D,
    source: &std::path::Path,
) -> Result<Vec<StoredArtifact<D>>, SessionError<D::Error>> {
    let bytes = std::fs::read(source).map_err(SessionError::Durability)?;
    let mut input = bytes.as_slice();
    if take_bundle(&mut input, 8)? != b"REFLEX\0\x01" {
        return Err(SessionError::CorruptBundle);
    }
    let identity = take_sized(&mut input)?;
    if identity != domain.semantic_identity().as_str().as_bytes() {
        return Err(SessionError::IncompatibleBundle);
    }
    let count =
        usize::try_from(read_bundle_u64(&mut input)?).map_err(|_| SessionError::CorruptBundle)?;
    let minimum_record_bytes = 32_usize;
    if count > input.len().saturating_div(minimum_record_bytes) {
        return Err(SessionError::CorruptBundle);
    }

    let mut structure_scratch = <D::Structure as StructuralProtocol<D>>::Scratch::default();
    let mut recovered = Vec::with_capacity(count);
    for _ in 0..count {
        let artifact = domain
            .structure()
            .decode_canonical(take_sized(&mut input)?, &mut structure_scratch)
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
        recovered.push((
            artifact,
            VerificationRecord {
                claim,
                evidence,
                kernel_revision,
            },
        ));
    }
    if !input.is_empty() {
        return Err(SessionError::CorruptBundle);
    }

    let replay_requests = recovered
        .iter()
        .map(|(artifact, verification)| VerificationReplayRequest {
            artifact,
            claim: &verification.claim,
            evidence: &verification.evidence,
            kernel_revision: verification.kernel_revision,
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
    if replayed.len() != recovered.len() || replayed.iter().any(|accepted| !accepted) {
        return Err(SessionError::InvalidSeed);
    }
    Ok(recovered)
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
) -> Result<Vec<Seed<D>>, SessionError<D::Error>> {
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
    Ok(seeds)
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
    seeds: &[Seed<D>],
    candidates: Vec<Candidate<D>>,
) -> Result<Vec<StoredArtifact<D>>, SessionError<D::Error>> {
    let mut claims = Vec::with_capacity(candidates.len());
    for candidate in &candidates {
        let seed = seeds
            .get(candidate.source_index)
            .ok_or(SessionError::InvalidSeed)?;
        claims.push(
            domain
                .kernel()
                .claim_for_candidate(&seed.artifact, &candidate.artifact)
                .map_err(SessionError::Domain)?,
        );
    }
    let requests = candidates
        .iter()
        .zip(&claims)
        .map(|(candidate, claim)| VerificationRequest {
            seed: &seeds[candidate.source_index].artifact,
            candidate: &candidate.artifact,
            claim,
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
    Ok(candidates
        .into_iter()
        .zip(claims)
        .zip(verdicts)
        .filter_map(|((candidate, claim), verdict)| match verdict {
            Verdict::Accepted { evidence } => Some((
                candidate.artifact,
                VerificationRecord {
                    claim,
                    evidence,
                    kernel_revision: revision,
                },
            )),
            Verdict::Refuted | Verdict::Unknown => None,
        })
        .collect())
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

fn publish_bundle<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    artifacts: &[VerifiedArtifact<D>],
) -> Result<u64, SessionError<D::Error>> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"REFLEX\0\x01");
    push_bytes(&mut bytes, domain.semantic_identity().as_str().as_bytes());
    push_u64(&mut bytes, artifacts.len() as u64);
    let mut structure_scratch = <D::Structure as crate::StructuralProtocol<D>>::Scratch::default();
    for artifact in artifacts {
        let mut encoded = Vec::new();
        domain
            .structure()
            .encode_canonical(
                &artifact.inner.artifact,
                &mut encoded,
                &mut structure_scratch,
            )
            .map_err(SessionError::Domain)?;
        push_bytes(&mut bytes, &encoded);
        encoded.clear();
        domain
            .kernel()
            .encode_claim(&artifact.inner.verification.claim, &mut encoded)
            .map_err(SessionError::Domain)?;
        push_bytes(&mut bytes, &encoded);
        encoded.clear();
        domain
            .kernel()
            .encode_evidence(&artifact.inner.verification.evidence, &mut encoded)
            .map_err(SessionError::Domain)?;
        push_bytes(&mut bytes, &encoded);
        push_u64(&mut bytes, artifact.inner.verification.kernel_revision.0);
    }
    let durable_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if durable_bytes > request.resources.durable_bytes.get() {
        return Err(SessionError::Resource);
    }

    let target = request.bundle.target();
    let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = target.with_extension(format!("reflex-tmp-{}-{sequence}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(SessionError::Durability)?;
    if let Err(error) = file.write_all(&bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&temp);
        return Err(SessionError::Durability(error));
    }
    drop(file);
    if let Err(error) = std::fs::rename(&temp, target) {
        let _ = std::fs::remove_file(&temp);
        return Err(SessionError::Durability(error));
    }
    Ok(durable_bytes)
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_bytes(output: &mut Vec<u8>, value: &[u8]) {
    push_u64(output, value.len() as u64);
    output.extend_from_slice(value);
}

fn stable_digest(bytes: &[u8]) -> [u8; 32] {
    let mut output = [0_u8; 32];
    for lane in 0..4_u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64 ^ lane.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let start = usize::try_from(lane * 8).expect("four digest lanes fit usize");
        output[start..start + 8].copy_from_slice(&hash.to_le_bytes());
    }
    output
}
