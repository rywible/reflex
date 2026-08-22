use std::collections::{BTreeSet, HashMap};

use sha2::Digest;

use crate::domain::{DomainDefinition, ProposalFeatures, ProposalProvenance};
use crate::knowledge::{DerivationObservation, KnowledgeRevision};
use crate::learning::{
    AttemptObservation, ConsequenceKind, ConsequenceObservation, Features, VerdictTarget,
};
use crate::measurement::MeasurementSpace;
use crate::session::{ArtifactKey, SessionError, VerifiedArtifact};

use crate::policy::AllocationQueue;

use super::{push_bytes, push_u64};

const DIGEST_BYTES: usize = 32;
const SIZED_LENGTH_BYTES: usize = 8;
const FLOAT_BYTES: usize = 4;
const VERDICT_BYTES: usize = 1;
const VERIFICATION_REQUEST_BYTES: usize = 4;
const EPOCH_BYTES: usize = 8;
const FIXED_ENTRY_BYTES: usize = DIGEST_BYTES * 5
    + SIZED_LENGTH_BYTES * 2
    + VERDICT_BYTES
    + 1
    + crate::domain::PROPOSAL_FEATURE_COUNT * FLOAT_BYTES
    + 1
    + crate::learning::FEATURE_COUNT * FLOAT_BYTES
    + VERIFICATION_REQUEST_BYTES
    + EPOCH_BYTES;
const FIXED_CONSEQUENCE_BYTES: usize = DIGEST_BYTES + 1;
const MINIMUM_MEASUREMENT_BYTES: usize = DIGEST_BYTES + SIZED_LENGTH_BYTES * 2;
const FIXED_CANDIDATE_FATE_BYTES: usize = DIGEST_BYTES * 4 + 8 * 2 + 4 * 6 + 1 + 1 + 1;

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum ExperienceVerdict {
    Accepted = 1,
    Refuted = 2,
    Unknown = 3,
}

#[derive(Clone, PartialEq)]
pub(super) struct ExperienceEntry {
    pub(super) attempt_id: [u8; 32],
    pub(super) candidate_key: ArtifactKey,
    pub(super) claim_digest: [u8; 32],
    pub(super) origin_key: ArtifactKey,
    pub(super) parent_key: ArtifactKey,
    pub(super) canonical_candidate: Vec<u8>,
    pub(super) verdict: ExperienceVerdict,
    pub(super) allocation_queue: AllocationQueue,
    pub(super) operator_symbol: Vec<u8>,
    pub(super) proposal_features: ProposalFeatures,
    pub(super) proposal_provenance: Option<ProposalProvenance>,
    pub(super) features: Features,
    pub(super) verification_requests: u32,
    pub(super) epoch: u64,
}

impl ExperienceEntry {
    pub(super) fn candidate_fate_key(&self) -> CandidateFateKey {
        CandidateFateKey {
            candidate_key: self.candidate_key,
            claim_digest: self.claim_digest,
            parent_key: self.parent_key,
            operator_digest: sha2::Sha256::digest(&self.operator_symbol).into(),
            proposal_provenance: self.proposal_provenance,
            epoch: self.epoch,
        }
    }
}

#[derive(Clone, PartialEq)]
pub(super) struct EncodedMeasurement {
    pub(super) metric_symbol: Vec<u8>,
    pub(super) observation: Vec<u8>,
}

#[derive(Clone, PartialEq)]
pub(super) struct MeasurementObservation {
    pub(super) subject: [u8; 32],
    pub(super) environment: Vec<u8>,
    pub(super) values: Vec<EncodedMeasurement>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum CandidateFateDisposition {
    KnownArtifact = 1,
    DuplicateCandidate = 2,
    PriorNegativeExperience = 3,
    PolicyDeferred = 4,
    VerificationInterrupted = 5,
    VerifiedAccepted = 6,
    VerifiedRefuted = 7,
    VerifiedUnknown = 8,
}

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) struct CandidateRank(Option<u32>);

impl CandidateRank {
    pub(super) const fn absent() -> Self {
        Self(None)
    }

    pub(super) const fn present(rank: u32) -> Self {
        Self(Some(rank))
    }

    const fn decode(rank: u32) -> Self {
        if rank == u32::MAX {
            Self::absent()
        } else {
            Self::present(rank)
        }
    }

    pub(super) fn value(self) -> Option<u32> {
        self.0
    }

    fn encode(self) -> u32 {
        self.0.unwrap_or(u32::MAX)
    }
}

#[derive(Clone, PartialEq)]
pub(super) struct CandidateFateObservation {
    pub(super) candidate_key: ArtifactKey,
    pub(super) claim_digest: [u8; 32],
    pub(super) parent_key: ArtifactKey,
    pub(super) operator_digest: [u8; 32],
    pub(super) proposal_provenance: Option<ProposalProvenance>,
    pub(super) epoch: u64,
    pub(super) generation_rank: u32,
    pub(super) proposal_limit: u32,
    pub(super) policy_rank: CandidateRank,
    pub(super) bootstrap_rank: CandidateRank,
    pub(super) learned_rank: CandidateRank,
    pub(super) verification_batch_cpu_ns: u64,
    pub(super) verification_batch_size: u32,
    pub(super) disposition: CandidateFateDisposition,
    pub(super) allocation_queue: Option<AllocationQueue>,
}

impl CandidateFateObservation {
    pub(super) fn key(&self) -> CandidateFateKey {
        CandidateFateKey {
            candidate_key: self.candidate_key,
            claim_digest: self.claim_digest,
            parent_key: self.parent_key,
            operator_digest: self.operator_digest,
            proposal_provenance: self.proposal_provenance,
            epoch: self.epoch,
        }
    }

    pub(super) fn attribution_is_valid(&self) -> bool {
        let deferred = matches!(
            self.disposition,
            CandidateFateDisposition::KnownArtifact
                | CandidateFateDisposition::DuplicateCandidate
                | CandidateFateDisposition::PriorNegativeExperience
                | CandidateFateDisposition::PolicyDeferred
        );
        let queue_is_valid = if deferred {
            self.allocation_queue.is_none() && self.policy_rank.value().is_none()
        } else {
            self.allocation_queue.is_some() && self.policy_rank.value().is_some()
        };
        let verified = self.expected_verdict().is_some();
        queue_is_valid
            && verified == (self.verification_batch_size != 0)
            && (verified || self.verification_batch_cpu_ns == 0)
    }

    pub(super) fn expected_verdict(&self) -> Option<ExperienceVerdict> {
        match self.disposition {
            CandidateFateDisposition::VerifiedAccepted => Some(ExperienceVerdict::Accepted),
            CandidateFateDisposition::VerifiedRefuted => Some(ExperienceVerdict::Refuted),
            CandidateFateDisposition::VerifiedUnknown => Some(ExperienceVerdict::Unknown),
            CandidateFateDisposition::KnownArtifact
            | CandidateFateDisposition::DuplicateCandidate
            | CandidateFateDisposition::PriorNegativeExperience
            | CandidateFateDisposition::PolicyDeferred
            | CandidateFateDisposition::VerificationInterrupted => None,
        }
    }
}

pub(super) fn candidate_fate_batches_are_valid(fates: &[CandidateFateObservation]) -> bool {
    let mut batches = HashMap::<u64, (u32, u64, u32)>::new();
    let mut selected_ranks = HashMap::<u64, BTreeSet<u32>>::new();
    for fate in fates {
        if let Some(policy_rank) = fate.policy_rank.value()
            && !selected_ranks
                .entry(fate.epoch)
                .or_default()
                .insert(policy_rank)
        {
            return false;
        }
        if fate.expected_verdict().is_none() {
            continue;
        }
        let batch = batches.entry(fate.epoch).or_insert((
            fate.verification_batch_size,
            fate.verification_batch_cpu_ns,
            0,
        ));
        if batch.0 != fate.verification_batch_size || batch.1 != fate.verification_batch_cpu_ns {
            return false;
        }
        let Some(count) = batch.2.checked_add(1) else {
            return false;
        };
        batch.2 = count;
    }
    batches
        .values()
        .all(|(declared_size, _, retained_count)| declared_size == retained_count)
        && selected_ranks.values().all(|ranks| {
            ranks
                .iter()
                .copied()
                .eq(0..u32::try_from(ranks.len()).unwrap_or(u32::MAX))
        })
}

#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub(super) struct CandidateFateKey {
    candidate_key: ArtifactKey,
    claim_digest: [u8; 32],
    parent_key: ArtifactKey,
    operator_digest: [u8; 32],
    proposal_provenance: Option<ProposalProvenance>,
    epoch: u64,
}

#[derive(Default)]
pub(super) struct ExperienceLedger {
    entries: Vec<ExperienceEntry>,
    consequences: Vec<ConsequenceObservation>,
    measurements: Vec<MeasurementObservation>,
    candidate_fates: Vec<CandidateFateObservation>,
}

#[derive(Clone, Copy)]
pub(super) struct EntryCheckpoint {
    len: usize,
    capacity: usize,
}

#[derive(Clone, Copy)]
pub(super) struct ConsequenceCheckpoint {
    len: usize,
    capacity: usize,
}

#[derive(Clone, Copy)]
pub(super) struct CandidateFateCheckpoint {
    len: usize,
    capacity: usize,
}

#[derive(Clone, Copy)]
pub(super) struct MeasurementCheckpoint {
    len: usize,
    capacity: usize,
}

impl ExperienceLedger {
    pub(super) fn from_parts(
        entries: Vec<ExperienceEntry>,
        consequences: Vec<ConsequenceObservation>,
        measurements: Vec<MeasurementObservation>,
        candidate_fates: Vec<CandidateFateObservation>,
    ) -> Self {
        Self {
            entries,
            consequences,
            measurements,
            candidate_fates,
        }
    }

    pub(super) fn len(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn accepted_len(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.verdict == ExperienceVerdict::Accepted)
            .count()
    }

    pub(super) fn entries(&self) -> &[ExperienceEntry] {
        &self.entries
    }

    pub(super) fn consequences(&self) -> &[ConsequenceObservation] {
        &self.consequences
    }

    pub(super) fn measurements(&self) -> &[MeasurementObservation] {
        &self.measurements
    }

    pub(super) fn candidate_fates(&self) -> &[CandidateFateObservation] {
        &self.candidate_fates
    }

    pub(super) fn checkpoint_candidate_fates(&self) -> CandidateFateCheckpoint {
        CandidateFateCheckpoint {
            len: self.candidate_fates.len(),
            capacity: self.candidate_fates.capacity(),
        }
    }

    pub(super) fn rollback_candidate_fates(&mut self, checkpoint: CandidateFateCheckpoint) {
        self.candidate_fates.truncate(checkpoint.len);
        self.candidate_fates.shrink_to(checkpoint.capacity);
    }

    pub(super) fn append_candidate_fates(
        &mut self,
        fates: impl IntoIterator<Item = CandidateFateObservation>,
    ) {
        self.candidate_fates.extend(fates);
    }

    pub(super) fn checkpoint_measurements(&self) -> MeasurementCheckpoint {
        MeasurementCheckpoint {
            len: self.measurements.len(),
            capacity: self.measurements.capacity(),
        }
    }

    pub(super) fn rollback_measurements(&mut self, checkpoint: MeasurementCheckpoint) {
        self.measurements.truncate(checkpoint.len);
        self.measurements.shrink_to(checkpoint.capacity);
    }

    #[cfg(feature = "internal-experiments")]
    pub(super) fn force_first_accepted_for_test(&mut self) -> Result<(), ()> {
        self.entries.first_mut().ok_or(()).map(|entry| {
            entry.verdict = ExperienceVerdict::Accepted;
        })
    }

    /// Decodes the complete private Experience segment framing.
    ///
    /// Domain-dependent semantic validation deliberately remains with restart,
    /// but no caller needs to know byte offsets or record widths.
    pub(super) fn decode(mut input: &[u8]) -> Result<Self, ()> {
        let entry_count = read_usize(&mut input)?;
        if entry_count > input.len().saturating_div(FIXED_ENTRY_BYTES) {
            return Err(());
        }
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            let attempt_id = read_digest(&mut input)?;
            let candidate_key = ArtifactKey(read_digest(&mut input)?);
            let claim_digest = read_digest(&mut input)?;
            let origin_key = ArtifactKey(read_digest(&mut input)?);
            let parent_key = ArtifactKey(read_digest(&mut input)?);
            let canonical_candidate = read_sized(&mut input)?.to_vec();
            let verdict = match take(&mut input, 1)?[0] {
                1 => ExperienceVerdict::Accepted,
                2 => ExperienceVerdict::Refuted,
                3 => ExperienceVerdict::Unknown,
                _ => return Err(()),
            };
            let allocation_queue = match take(&mut input, 1)?[0] {
                1 => AllocationQueue::ProtectedOrigin,
                2 => AllocationQueue::ProtectedDerived,
                3 => AllocationQueue::Learned,
                4 => AllocationQueue::Bootstrap,
                _ => return Err(()),
            };
            let operator_symbol = read_sized(&mut input)?.to_vec();
            let mut proposal_values = [0.0; crate::domain::PROPOSAL_FEATURE_COUNT];
            read_finite_features(&mut input, &mut proposal_values)?;
            let proposal_provenance = match take(&mut input, 1)?[0] {
                0 => None,
                1 => Some(ProposalProvenance::new(read_digest(&mut input)?)),
                _ => return Err(()),
            };
            let mut feature_values = [0.0; crate::learning::FEATURE_COUNT];
            read_finite_features(&mut input, &mut feature_values)?;
            let verification_requests = u32::from_le_bytes(
                take(&mut input, VERIFICATION_REQUEST_BYTES)?
                    .try_into()
                    .map_err(|_| ())?,
            );
            let epoch = read_u64(&mut input)?;
            entries.push(ExperienceEntry {
                attempt_id,
                candidate_key,
                claim_digest,
                origin_key,
                parent_key,
                canonical_candidate,
                verdict,
                allocation_queue,
                operator_symbol,
                proposal_features: ProposalFeatures::new(proposal_values),
                proposal_provenance,
                features: Features(feature_values),
                verification_requests,
                epoch,
            });
        }

        let consequence_count = read_usize(&mut input)?;
        if consequence_count > input.len().saturating_div(FIXED_CONSEQUENCE_BYTES) {
            return Err(());
        }
        let mut consequences = Vec::with_capacity(consequence_count);
        for _ in 0..consequence_count {
            let subject = read_digest(&mut input)?;
            let kind = match take(&mut input, 1)?[0] {
                1 => ConsequenceKind::Admitted,
                2 => ConsequenceKind::ParetoImprovement,
                3 => ConsequenceKind::CrossGoalUse,
                4 => ConsequenceKind::Compression,
                _ => return Err(()),
            };
            consequences.push(ConsequenceObservation { subject, kind });
        }

        let measurements = decode_measurements(&mut input)?;
        let candidate_fates = decode_candidate_fates(&mut input)?;
        if !input.is_empty() {
            return Err(());
        }
        Ok(Self::from_parts(
            entries,
            consequences,
            measurements,
            candidate_fates,
        ))
    }

    pub(super) fn admission_observations(
        &mut self,
    ) -> (&[ExperienceEntry], &mut Vec<ConsequenceObservation>) {
        (&self.entries, &mut self.consequences)
    }

    pub(super) fn checkpoint_entries(&self) -> EntryCheckpoint {
        EntryCheckpoint {
            len: self.entries.len(),
            capacity: self.entries.capacity(),
        }
    }

    pub(super) fn rollback_entries(&mut self, checkpoint: EntryCheckpoint) {
        self.entries.truncate(checkpoint.len);
        self.entries.shrink_to(checkpoint.capacity);
    }

    pub(super) fn checkpoint_consequences(&self) -> ConsequenceCheckpoint {
        ConsequenceCheckpoint {
            len: self.consequences.len(),
            capacity: self.consequences.capacity(),
        }
    }

    pub(super) fn rollback_consequences(&mut self, checkpoint: ConsequenceCheckpoint) {
        self.consequences.truncate(checkpoint.len);
        self.consequences.shrink_to(checkpoint.capacity);
    }

    pub(super) fn append_entries(&mut self, entries: impl IntoIterator<Item = ExperienceEntry>) {
        for entry in entries {
            if let Some(existing) = self
                .entries
                .iter()
                .find(|existing| existing.attempt_id == entry.attempt_id)
            {
                assert!(
                    existing == &entry,
                    "a stable attempt identity must name exactly one immutable observation"
                );
            } else {
                self.entries.push(entry);
            }
        }
    }

    pub(super) fn replace_consequences(&mut self, consequences: Vec<ConsequenceObservation>) {
        self.consequences = consequences;
    }

    pub(super) fn append_measurement<D: DomainDefinition>(
        &mut self,
        domain: &D,
        artifact: &VerifiedArtifact<D>,
        subject: [u8; 32],
    ) -> Result<(), SessionError<D::Error>> {
        if self
            .measurements
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
        self.measurements.push(MeasurementObservation {
            subject,
            environment: artifact.inner.environment.identity().as_bytes().to_vec(),
            values,
        });
        Ok(())
    }

    pub(super) fn derivations<E>(
        &self,
        knowledge: &KnowledgeRevision,
        primitive_symbols: &BTreeSet<Vec<u8>>,
    ) -> Result<Vec<DerivationObservation>, SessionError<E>> {
        self.entries
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

    pub(super) fn attempts(&self) -> Vec<AttemptObservation> {
        let traces = self
            .candidate_fates
            .iter()
            .filter(|fate| {
                matches!(
                    fate.disposition,
                    CandidateFateDisposition::VerifiedAccepted
                        | CandidateFateDisposition::VerifiedRefuted
                        | CandidateFateDisposition::VerifiedUnknown
                )
            })
            .map(|fate| (fate.key(), fate))
            .collect::<HashMap<_, _>>();
        self.entries
            .iter()
            .map(|entry| {
                let trace = traces.get(&entry.candidate_fate_key());
                AttemptObservation {
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
                    allocation_queue: entry.allocation_queue,
                    bootstrap_rank: trace
                        .and_then(|fate| fate.bootstrap_rank.value())
                        .unwrap_or(u32::MAX),
                }
            })
            .collect()
    }

    pub(super) fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(8 + self.entries.len() * FIXED_ENTRY_BYTES);
        push_u64(&mut output, self.entries.len() as u64);
        for entry in &self.entries {
            output.extend_from_slice(&entry.attempt_id);
            output.extend_from_slice(entry.candidate_key.as_bytes());
            output.extend_from_slice(&entry.claim_digest);
            output.extend_from_slice(entry.origin_key.as_bytes());
            output.extend_from_slice(entry.parent_key.as_bytes());
            push_bytes(&mut output, &entry.canonical_candidate);
            output.push(entry.verdict as u8);
            output.push(entry.allocation_queue as u8);
            push_bytes(&mut output, &entry.operator_symbol);
            for feature in entry.proposal_features.as_array() {
                output.extend_from_slice(&feature.to_bits().to_le_bytes());
            }
            if let Some(provenance) = entry.proposal_provenance {
                output.push(1);
                output.extend_from_slice(&provenance.support_key());
            } else {
                output.push(0);
            }
            for feature in entry.features.0 {
                output.extend_from_slice(&feature.to_bits().to_le_bytes());
            }
            output.extend_from_slice(&entry.verification_requests.to_le_bytes());
            output.extend_from_slice(&entry.epoch.to_le_bytes());
        }
        push_u64(&mut output, self.consequences.len() as u64);
        for consequence in &self.consequences {
            output.extend_from_slice(&consequence.subject);
            output.push(match consequence.kind {
                ConsequenceKind::Admitted => 1,
                ConsequenceKind::ParetoImprovement => 2,
                ConsequenceKind::CrossGoalUse => 3,
                ConsequenceKind::Compression => 4,
            });
        }
        push_u64(&mut output, self.measurements.len() as u64);
        for measurement in &self.measurements {
            output.extend_from_slice(&measurement.subject);
            push_bytes(&mut output, &measurement.environment);
            push_u64(&mut output, measurement.values.len() as u64);
            for value in &measurement.values {
                push_bytes(&mut output, &value.metric_symbol);
                push_bytes(&mut output, &value.observation);
            }
        }
        push_u64(&mut output, self.candidate_fates.len() as u64);
        for fate in &self.candidate_fates {
            output.extend_from_slice(fate.candidate_key.as_bytes());
            output.extend_from_slice(&fate.claim_digest);
            output.extend_from_slice(fate.parent_key.as_bytes());
            output.extend_from_slice(&fate.operator_digest);
            if let Some(provenance) = fate.proposal_provenance {
                output.push(1);
                output.extend_from_slice(&provenance.support_key());
            } else {
                output.push(0);
            }
            output.extend_from_slice(&fate.epoch.to_le_bytes());
            output.extend_from_slice(&fate.generation_rank.to_le_bytes());
            output.extend_from_slice(&fate.proposal_limit.to_le_bytes());
            output.extend_from_slice(&fate.policy_rank.encode().to_le_bytes());
            output.extend_from_slice(&fate.bootstrap_rank.encode().to_le_bytes());
            output.extend_from_slice(&fate.learned_rank.encode().to_le_bytes());
            output.extend_from_slice(&fate.verification_batch_cpu_ns.to_le_bytes());
            output.extend_from_slice(&fate.verification_batch_size.to_le_bytes());
            output.push(fate.disposition as u8);
            output.push(fate.allocation_queue.map_or(0, |queue| queue as u8));
        }
        output
    }

    pub(super) fn resident_bytes(&self) -> u64 {
        let entry_payloads = self.entries.iter().fold(0_u64, |bytes, entry| {
            bytes
                .saturating_add(entry.canonical_candidate.capacity() as u64)
                .saturating_add(entry.operator_symbol.capacity() as u64)
        });
        let measurement_payloads = self.measurements.iter().fold(0_u64, |bytes, observation| {
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
        vector_bytes(&self.entries)
            .saturating_add(entry_payloads)
            .saturating_add(vector_bytes(&self.consequences))
            .saturating_add(vector_bytes(&self.measurements))
            .saturating_add(vector_bytes(&self.candidate_fates))
            .saturating_add(measurement_payloads)
    }

    pub(super) fn resident_bytes_with_consequences(
        &self,
        consequences: &Vec<ConsequenceObservation>,
    ) -> u64 {
        self.resident_bytes()
            .saturating_sub(vector_bytes(&self.consequences))
            .saturating_add(vector_bytes(consequences))
    }
}

fn decode_candidate_fates(input: &mut &[u8]) -> Result<Vec<CandidateFateObservation>, ()> {
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let fate_count = read_usize(input)?;
    if fate_count > input.len().saturating_div(FIXED_CANDIDATE_FATE_BYTES) {
        return Err(());
    }
    let mut fates = Vec::with_capacity(fate_count);
    for _ in 0..fate_count {
        let candidate_key = ArtifactKey(read_digest(input)?);
        let claim_digest = read_digest(input)?;
        let parent_key = ArtifactKey(read_digest(input)?);
        let operator_digest = read_digest(input)?;
        let proposal_provenance = match take(input, 1)?[0] {
            0 => None,
            1 => Some(ProposalProvenance::new(read_digest(input)?)),
            _ => return Err(()),
        };
        let epoch = read_u64(input)?;
        let generation_rank = read_u32(input)?;
        let proposal_limit = read_u32(input)?;
        let policy_rank = CandidateRank::decode(read_u32(input)?);
        let bootstrap_rank = CandidateRank::decode(read_u32(input)?);
        let learned_rank = CandidateRank::decode(read_u32(input)?);
        let verification_batch_cpu_ns = read_u64(input)?;
        let verification_batch_size = read_u32(input)?;
        let disposition = match take(input, 1)?[0] {
            1 => CandidateFateDisposition::KnownArtifact,
            2 => CandidateFateDisposition::DuplicateCandidate,
            3 => CandidateFateDisposition::PriorNegativeExperience,
            4 => CandidateFateDisposition::PolicyDeferred,
            5 => CandidateFateDisposition::VerificationInterrupted,
            6 => CandidateFateDisposition::VerifiedAccepted,
            7 => CandidateFateDisposition::VerifiedRefuted,
            8 => CandidateFateDisposition::VerifiedUnknown,
            _ => return Err(()),
        };
        let allocation_queue = match take(input, 1)?[0] {
            0 => None,
            1 => Some(AllocationQueue::ProtectedOrigin),
            2 => Some(AllocationQueue::ProtectedDerived),
            3 => Some(AllocationQueue::Learned),
            4 => Some(AllocationQueue::Bootstrap),
            _ => return Err(()),
        };
        let fate = CandidateFateObservation {
            candidate_key,
            claim_digest,
            parent_key,
            operator_digest,
            proposal_provenance,
            epoch,
            generation_rank,
            proposal_limit,
            policy_rank,
            bootstrap_rank,
            learned_rank,
            verification_batch_cpu_ns,
            verification_batch_size,
            disposition,
            allocation_queue,
        };
        if !fate.attribution_is_valid() {
            return Err(());
        }
        fates.push(fate);
    }
    if !candidate_fate_batches_are_valid(&fates) {
        return Err(());
    }
    Ok(fates)
}

fn decode_measurements(input: &mut &[u8]) -> Result<Vec<MeasurementObservation>, ()> {
    let measurement_count = read_usize(input)?;
    if measurement_count > input.len().saturating_div(MINIMUM_MEASUREMENT_BYTES) {
        return Err(());
    }
    let mut measurements = Vec::with_capacity(measurement_count);
    for _ in 0..measurement_count {
        let subject = read_digest(input)?;
        let environment = read_sized(input)?.to_vec();
        let value_count = read_usize(input)?;
        if value_count > input.len().saturating_div(SIZED_LENGTH_BYTES * 2) {
            return Err(());
        }
        let mut values = Vec::with_capacity(value_count);
        for _ in 0..value_count {
            values.push(EncodedMeasurement {
                metric_symbol: read_sized(input)?.to_vec(),
                observation: read_sized(input)?.to_vec(),
            });
        }
        measurements.push(MeasurementObservation {
            subject,
            environment,
            values,
        });
    }
    Ok(measurements)
}

fn read_finite_features<const N: usize>(
    input: &mut &[u8],
    output: &mut [f32; N],
) -> Result<(), ()> {
    for value in output {
        *value = f32::from_bits(u32::from_le_bytes(
            take(input, FLOAT_BYTES)?.try_into().map_err(|_| ())?,
        ));
        if !value.is_finite() {
            return Err(());
        }
    }
    Ok(())
}

fn read_digest(input: &mut &[u8]) -> Result<[u8; DIGEST_BYTES], ()> {
    take(input, DIGEST_BYTES)?.try_into().map_err(|_| ())
}

fn read_sized<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], ()> {
    let length = read_usize(input)?;
    take(input, length)
}

fn read_usize(input: &mut &[u8]) -> Result<usize, ()> {
    usize::try_from(read_u64(input)?).map_err(|_| ())
}

fn read_u64(input: &mut &[u8]) -> Result<u64, ()> {
    Ok(u64::from_le_bytes(
        take(input, SIZED_LENGTH_BYTES)?
            .try_into()
            .map_err(|_| ())?,
    ))
}

fn read_u32(input: &mut &[u8]) -> Result<u32, ()> {
    Ok(u32::from_le_bytes(
        take(input, 4)?.try_into().map_err(|_| ())?,
    ))
}

fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], ()> {
    if input.len() < count {
        return Err(());
    }
    let (value, remainder) = input.split_at(count);
    *input = remainder;
    Ok(value)
}

fn vector_bytes<T>(values: &Vec<T>) -> u64 {
    (values.capacity() as u64).saturating_mul(std::mem::size_of::<T>() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "one codec contract covers framing, verdicts, fate states, and hostile counts"
    )]
    fn experience_codec_round_trips_every_verdict_and_rejects_bad_framing() {
        let verdicts = [
            ExperienceVerdict::Accepted,
            ExperienceVerdict::Refuted,
            ExperienceVerdict::Unknown,
        ];
        let entries = verdicts
            .into_iter()
            .enumerate()
            .map(|(index, verdict)| {
                let marker = u8::try_from(index + 1).unwrap();
                ExperienceEntry {
                    attempt_id: [marker; 32],
                    candidate_key: ArtifactKey([marker; 32]),
                    claim_digest: [marker; 32],
                    origin_key: ArtifactKey([marker; 32]),
                    parent_key: ArtifactKey([marker; 32]),
                    canonical_candidate: vec![marker],
                    verdict,
                    allocation_queue: AllocationQueue::Bootstrap,
                    operator_symbol: vec![marker],
                    proposal_features: ProposalFeatures::default(),
                    proposal_provenance: (index == 0)
                        .then_some(ProposalProvenance::new([0xa5; 32])),
                    features: Features([0.0; crate::learning::FEATURE_COUNT]),
                    verification_requests: 1,
                    epoch: u64::try_from(index).unwrap(),
                }
            })
            .collect();
        let dispositions = [
            CandidateFateDisposition::KnownArtifact,
            CandidateFateDisposition::DuplicateCandidate,
            CandidateFateDisposition::PriorNegativeExperience,
            CandidateFateDisposition::PolicyDeferred,
            CandidateFateDisposition::VerificationInterrupted,
            CandidateFateDisposition::VerifiedAccepted,
            CandidateFateDisposition::VerifiedRefuted,
            CandidateFateDisposition::VerifiedUnknown,
        ];
        let candidate_fates = dispositions
            .into_iter()
            .enumerate()
            .map(|(index, disposition)| {
                let marker = u8::try_from(index + 1).unwrap();
                let rank = u32::try_from(index).unwrap();
                let deferred = matches!(
                    disposition,
                    CandidateFateDisposition::KnownArtifact
                        | CandidateFateDisposition::DuplicateCandidate
                        | CandidateFateDisposition::PriorNegativeExperience
                        | CandidateFateDisposition::PolicyDeferred
                );
                CandidateFateObservation {
                    candidate_key: ArtifactKey([marker; 32]),
                    claim_digest: [marker; 32],
                    parent_key: ArtifactKey([marker; 32]),
                    operator_digest: [marker; 32],
                    proposal_provenance: (index == 0)
                        .then_some(ProposalProvenance::new([0x5a; 32])),
                    epoch: if matches!(
                        disposition,
                        CandidateFateDisposition::VerifiedAccepted
                            | CandidateFateDisposition::VerifiedRefuted
                            | CandidateFateDisposition::VerifiedUnknown
                    ) {
                        8
                    } else {
                        index as u64
                    },
                    generation_rank: rank,
                    proposal_limit: 64,
                    policy_rank: if deferred {
                        CandidateRank::absent()
                    } else if disposition == CandidateFateDisposition::VerificationInterrupted {
                        CandidateRank::present(0)
                    } else {
                        CandidateRank::present(rank - 5)
                    },
                    bootstrap_rank: CandidateRank::present(rank),
                    learned_rank: CandidateRank::present(rank),
                    verification_batch_cpu_ns: if matches!(
                        disposition,
                        CandidateFateDisposition::VerifiedAccepted
                            | CandidateFateDisposition::VerifiedRefuted
                            | CandidateFateDisposition::VerifiedUnknown
                    ) {
                        1_000
                    } else {
                        0
                    },
                    verification_batch_size: if matches!(
                        disposition,
                        CandidateFateDisposition::VerifiedAccepted
                            | CandidateFateDisposition::VerifiedRefuted
                            | CandidateFateDisposition::VerifiedUnknown
                    ) {
                        3
                    } else {
                        0
                    },
                    disposition,
                    allocation_queue: (!deferred).then_some(AllocationQueue::Bootstrap),
                }
            })
            .collect();
        let ledger = ExperienceLedger::from_parts(entries, Vec::new(), Vec::new(), candidate_fates);
        let encoded = ledger.encode();
        let decoded = ExperienceLedger::decode(&encoded).unwrap();
        assert!(decoded.entries() == ledger.entries());
        assert!(decoded.candidate_fates() == ledger.candidate_fates());
        assert_eq!(decoded.encode(), encoded);
        assert!(ExperienceLedger::decode(&encoded[..encoded.len() - 1]).is_err());

        let mut bad_verdict = encoded;
        let first_verdict = 8 + 32 * 5 + 8 + 1;
        bad_verdict[first_verdict] = 0;
        assert!(ExperienceLedger::decode(&bad_verdict).is_err());

        let mut missing_fate_queue = ledger.encode();
        *missing_fate_queue.last_mut().unwrap() = 0;
        assert!(ExperienceLedger::decode(&missing_fate_queue).is_err());

        let mut hostile_value_count = Vec::new();
        push_u64(&mut hostile_value_count, 0);
        push_u64(&mut hostile_value_count, 0);
        push_u64(&mut hostile_value_count, 1);
        hostile_value_count.extend_from_slice(&[0; DIGEST_BYTES]);
        push_bytes(&mut hostile_value_count, b"environment");
        push_u64(&mut hostile_value_count, u64::MAX);
        assert!(ExperienceLedger::decode(&hostile_value_count).is_err());
    }

    #[test]
    fn verification_batch_attribution_requires_one_complete_consistent_batch() {
        let verified = |marker, batch_size, batch_cpu| CandidateFateObservation {
            candidate_key: ArtifactKey([marker; 32]),
            claim_digest: [marker; 32],
            parent_key: ArtifactKey([marker; 32]),
            operator_digest: [marker; 32],
            proposal_provenance: None,
            epoch: 7,
            generation_rank: u32::from(marker),
            proposal_limit: 8,
            policy_rank: CandidateRank::present(u32::from(marker - 1)),
            bootstrap_rank: CandidateRank::present(u32::from(marker)),
            learned_rank: CandidateRank::present(u32::from(marker)),
            verification_batch_cpu_ns: batch_cpu,
            verification_batch_size: batch_size,
            disposition: CandidateFateDisposition::VerifiedAccepted,
            allocation_queue: Some(AllocationQueue::Learned),
        };
        let complete = vec![verified(1, 2, 1_000), verified(2, 2, 1_000)];
        assert!(candidate_fate_batches_are_valid(&complete));

        let incomplete = vec![verified(1, 2, 1_000)];
        assert!(!candidate_fate_batches_are_valid(&incomplete));

        let inconsistent_cpu = vec![verified(1, 2, 1_000), verified(2, 2, 1_001)];
        assert!(!candidate_fate_batches_are_valid(&inconsistent_cpu));

        let duplicate_rank = vec![verified(1, 2, 1_000), verified(1, 2, 1_000)];
        assert!(!candidate_fate_batches_are_valid(&duplicate_rank));

        let mut missing_first_rank = complete;
        missing_first_rank[0].policy_rank = CandidateRank::present(2);
        assert!(!candidate_fate_batches_are_valid(&missing_first_rank));
    }
}
