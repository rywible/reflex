use std::collections::BTreeSet;

use crate::domain::DomainDefinition;
use crate::knowledge::{DerivationObservation, KnowledgeRevision};
use crate::learning::{
    AttemptObservation, ConsequenceKind, ConsequenceObservation, Features, VerdictTarget,
};
use crate::measurement::MeasurementSpace;
use crate::session::{ArtifactKey, SessionError, VerifiedArtifact};

use super::{push_bytes, push_u64};

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
    pub(super) operator_symbol: Vec<u8>,
    pub(super) features: Features,
    pub(super) verification_requests: u32,
    pub(super) epoch: u64,
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

#[derive(Default)]
pub(super) struct ExperienceLedger {
    entries: Vec<ExperienceEntry>,
    consequences: Vec<ConsequenceObservation>,
    measurements: Vec<MeasurementObservation>,
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

impl ExperienceLedger {
    pub(super) fn from_parts(
        entries: Vec<ExperienceEntry>,
        consequences: Vec<ConsequenceObservation>,
        measurements: Vec<MeasurementObservation>,
    ) -> Self {
        Self {
            entries,
            consequences,
            measurements,
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
        self.entries
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
            .collect()
    }

    pub(super) fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(8 + self.entries.len() * 97);
        push_u64(&mut output, self.entries.len() as u64);
        for entry in &self.entries {
            output.extend_from_slice(&entry.attempt_id);
            output.extend_from_slice(entry.candidate_key.as_bytes());
            output.extend_from_slice(&entry.claim_digest);
            output.extend_from_slice(entry.origin_key.as_bytes());
            output.extend_from_slice(entry.parent_key.as_bytes());
            push_bytes(&mut output, &entry.canonical_candidate);
            output.push(entry.verdict as u8);
            push_bytes(&mut output, &entry.operator_symbol);
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

fn vector_bytes<T>(values: &Vec<T>) -> u64 {
    (values.capacity() as u64).saturating_mul(std::mem::size_of::<T>() as u64)
}
