use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write;
use std::ops::ControlFlow;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};

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
type OriginatedStoredArtifact<D> = (StoredArtifact<D>, usize);

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
    let recovered_stored = request
        .bundle
        .source()
        .map(|source| load_bundle(domain, source))
        .transpose()?
        .unwrap_or_default();
    let recovered_replays = recovered_stored.len();
    let mut seeds = read_seeds(domain, &request.seeds)?;
    let seed_replays = seeds.len();
    replay_seeds(domain, &seeds)?;
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
    let environment = crate::MeasurementEnvironment::local_process();
    let recovered = materialize(domain, recovered_stored, &environment)?;
    let recovered_keys = recovered
        .iter()
        .map(VerifiedArtifact::key)
        .collect::<Vec<_>>();
    let seed_stored = seeds
        .drain(..)
        .map(|seed| (seed.artifact, seed.verification))
        .collect::<Vec<_>>();
    let roots = materialize(domain, seed_stored, &environment)?;
    let mut known = recovered;
    extend_unique(&mut known, roots.iter().cloned());
    let mut pareto = pareto_union(domain, &request.goals, &known);
    let mut sequence = 0_u64;
    let mut stopped_by_observer =
        deliver_delta(&mut observer, &mut sequence, &recovered_keys, &pareto);
    let mut success_conditions_satisfied = all_success_conditions_satisfied(
        domain,
        &request.goals,
        &goal_frontiers(domain, &request.goals, &known),
    );
    let mut verification_requests = required_replays;
    let mut verification_budget_exhausted = false;
    let mut frontier = roots
        .iter()
        .cloned()
        .enumerate()
        .map(|(origin, artifact)| (artifact, origin))
        .collect::<Vec<_>>();
    let operators = domain
        .operators()
        .catalog()
        .iter()
        .map(crate::OperatorDescriptor::operator)
        .collect::<Vec<_>>();
    let mut operator_scratch = <D::Operators as OperatorAlgebra<D>>::Scratch::default();

    while !stopped_by_observer && !success_conditions_satisfied && !frontier.is_empty() {
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
        let remaining = verification_budget
            .checked_sub(verification_requests)
            .and_then(|remaining| usize::try_from(remaining).ok())
            .unwrap_or(0);
        if candidates.len() > remaining {
            candidates.truncate(remaining);
            verification_budget_exhausted = true;
        }
        if candidates.is_empty() {
            break;
        }
        verification_requests +=
            u64::try_from(candidates.len()).map_err(|_| SessionError::Resource)?;
        let accepted_stored = verify_candidates(domain, &roots, &origins, candidates)?;
        let accepted_origins = accepted_stored
            .iter()
            .map(|(_, origin)| *origin)
            .collect::<Vec<_>>();
        let accepted = materialize(
            domain,
            accepted_stored
                .into_iter()
                .map(|(artifact, _)| artifact)
                .collect(),
            &environment,
        )?;
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
        pareto = pareto_union(domain, &request.goals, &known);
        stopped_by_observer = deliver_delta(&mut observer, &mut sequence, &previous_keys, &pareto);
        success_conditions_satisfied = all_success_conditions_satisfied(
            domain,
            &request.goals,
            &goal_frontiers(domain, &request.goals, &known),
        );
        if verification_budget_exhausted {
            break;
        }
    }

    let completion = if stopped_by_observer {
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
        .map(|(artifact, _)| artifact)
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
        .map(|((artifact, verification), measurements)| {
            let mut canonical = Vec::new();
            domain
                .structure()
                .encode_canonical(&artifact, &mut canonical, &mut structure_scratch)
                .map_err(SessionError::Domain)?;
            Ok(VerifiedArtifact {
                inner: Arc::new(VerifiedArtifactRecord {
                    key: ArtifactKey(stable_digest(identity.as_str(), &canonical)),
                    artifact,
                    verification,
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

fn deliver_delta<D, O>(
    observer: &mut O,
    sequence: &mut u64,
    previous: &[ArtifactKey],
    current: &[VerifiedArtifact<D>],
) -> bool
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
        return false;
    }
    *sequence += 1;
    observer(ParetoUpdate {
        sequence: *sequence,
        added: &added,
        removed: &removed,
    })
    .is_break()
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
    if bytes.len() < 32 {
        return Err(SessionError::CorruptBundle);
    }
    let content_len = bytes.len() - 32;
    let (content, stored_checksum) = bytes.split_at(content_len);
    if Sha256::digest(content)[..] != *stored_checksum {
        return Err(SessionError::CorruptBundle);
    }
    let mut input = content;
    if take_bundle(&mut input, 8)? != b"REFLEX\0\x02" {
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
    roots: &[VerifiedArtifact<D>],
    parent_origins: &[usize],
    candidates: Vec<Candidate<D>>,
) -> Result<Vec<OriginatedStoredArtifact<D>>, SessionError<D::Error>> {
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
    Ok(candidates
        .into_iter()
        .zip(claims)
        .zip(verdicts)
        .filter_map(|((candidate, claim), verdict)| {
            let origin = parent_origins[candidate.source_index];
            match verdict {
                Verdict::Accepted { evidence } => Some((
                    (
                        candidate.artifact,
                        VerificationRecord {
                            claim,
                            evidence,
                            kernel_revision: revision,
                        },
                    ),
                    origin,
                )),
                Verdict::Refuted | Verdict::Unknown => None,
            }
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
    bytes.extend_from_slice(b"REFLEX\0\x02");
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
    let checksum = Sha256::digest(&bytes);
    bytes.extend_from_slice(&checksum);
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

fn stable_digest(semantic_identity: &str, canonical_artifact: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-artifact-v1\0");
    digest.update((semantic_identity.len() as u64).to_le_bytes());
    digest.update(semantic_identity.as_bytes());
    digest.update(canonical_artifact);
    digest.finalize().into()
}
