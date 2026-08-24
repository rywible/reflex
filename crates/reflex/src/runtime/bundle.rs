use std::path::Path;

use reflex_bundle::CanonicalBundle;

use crate::domain::{DomainDefinition, SeedSource, SemanticIdentity};
use crate::resource::{ResidentReservation, ResourceEnvelopeGuard};
use crate::session::{ImprovementRequest, SessionError};

use super::{RecoveredBundle, RestartBundleState, SessionSeal};

const FILE_FRAMING_BYTES: u64 = 8 + 8 + 4 + 5 * (1 + 4 + 8 + 32) + 32;

#[derive(Clone, Copy)]
pub(super) struct SealAdmission {
    pub(super) resident_overlap: u64,
    pub(super) additional_transient: u64,
    pub(super) pending_durability: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct BundleSealPlan {
    peak_transient_bytes: u64,
    maximum_output_capacity: usize,
}

impl BundleSealPlan {
    pub(super) const fn peak_transient_bytes(self) -> u64 {
        self.peak_transient_bytes
    }

    const fn accepts_output_capacity(self, capacity: usize) -> bool {
        capacity <= self.maximum_output_capacity
    }
}

/// Semantic Domain Bundle encoding and recovery behind one restart-complete
/// interface. Canonical framing is delegated to the private `reflex-bundle`
/// format module; this module owns how Runtime state maps to that format.
pub(super) struct RestartBundleCodec<'a, D: DomainDefinition> {
    domain: &'a D,
    request: &'a ImprovementRequest<D>,
    semantic_identity: SemanticIdentity,
    encoded_scope: Vec<u8>,
    goals_digest: [u8; 32],
    environment: Box<[u8]>,
}

impl<'a, D: DomainDefinition> RestartBundleCodec<'a, D> {
    pub(super) fn new(
        domain: &'a D,
        request: &'a ImprovementRequest<D>,
    ) -> Result<Self, SessionError<D::Error>> {
        use sha2::Digest;

        let mut encoded_scope = Vec::new();
        domain
            .seeds()
            .encode_scope(&request.seeds, &mut encoded_scope)
            .map_err(SessionError::Domain)?;
        let goals = super::GoalEvaluator::encode_set(domain, &request.goals)?;
        let environment = crate::MeasurementEnvironment::local_process()
            .identity()
            .as_bytes()
            .into();
        Ok(Self {
            domain,
            request,
            semantic_identity: domain.semantic_identity(),
            encoded_scope,
            goals_digest: sha2::Sha256::digest(goals).into(),
            environment,
        })
    }

    pub(super) fn resident_bytes(&self) -> u64 {
        u64::try_from(self.encoded_scope.capacity())
            .unwrap_or(u64::MAX)
            .saturating_add(self.semantic_identity.resident_bytes())
            .saturating_add(u64::try_from(self.environment.len()).unwrap_or(u64::MAX))
    }

    pub(super) const fn semantic_identity(&self) -> &SemanticIdentity {
        &self.semantic_identity
    }

    pub(super) fn recover(
        &self,
        source: &Path,
        resources: &ResourceEnvelopeGuard,
        resident_before_bundle: u64,
    ) -> Result<RecoveredBundle<D>, SessionError<D::Error>> {
        super::decode_bundle(
            self.domain,
            self.request,
            source,
            resources,
            resident_before_bundle,
        )
    }

    pub(super) fn replacement_transient_bytes(
        &self,
        encoded_bundle_bytes: u64,
        seed_cursor_len: usize,
        input_is_live: bool,
    ) -> u64 {
        let session = super::session_encoded_len(
            self.encoded_scope.len(),
            seed_cursor_len,
            self.environment.len(),
        );
        u64::from(!input_is_live)
            .saturating_mul(encoded_bundle_bytes)
            .saturating_add(session)
            .saturating_add(encoded_bundle_bytes.saturating_add(session))
    }

    pub(super) fn replace_session_prepared(
        &self,
        seed_cursor: &[u8],
        encoded_bundle: &[u8],
        session_seal: SessionSeal,
    ) -> Result<Vec<u8>, SessionError<D::Error>> {
        let identity = &self.semantic_identity;
        let restart_state_root = CanonicalBundle::encoded_restart_state_root(
            encoded_bundle,
            identity.as_str().as_bytes(),
        )
        .map_err(|error| {
            if error.is_identity_mismatch() {
                SessionError::IncompatibleBundle
            } else {
                SessionError::CorruptBundle
            }
        })?;
        let session = super::encode_session_prepared(&super::PreparedSessionInput {
            domain: self.domain,
            request: self.request,
            scope: &self.encoded_scope,
            goals_digest: self.goals_digest,
            environment: &self.environment,
            seed_cursor,
            restart_state_root,
            session_seal,
        });
        let maximum = usize::try_from(
            CanonicalBundle::replacement_size_bound(
                encoded_bundle,
                &[(reflex_bundle::SegmentKind::Session, session.len() as u64)],
            )
            .map_err(|_| SessionError::CorruptBundle)?,
        )
        .map_err(|_| SessionError::Resource)?;
        if maximum
            > usize::try_from(self.request.resources.durable_bytes.get()).unwrap_or(usize::MAX)
        {
            return Err(SessionError::Resource);
        }
        let output = CanonicalBundle::replace_session_bounded(
            encoded_bundle,
            identity.as_str().as_bytes(),
            &session,
            maximum,
        )
        .map_err(|error| {
            if error.is_identity_mismatch() {
                SessionError::IncompatibleBundle
            } else {
                SessionError::CorruptBundle
            }
        })?;
        if output.capacity() > maximum {
            return Err(SessionError::Resource);
        }
        Ok(output)
    }

    pub(super) fn seal_plan(
        &self,
        seed_cursor: &[u8],
        state: &RestartBundleState<'_, D>,
        intelligence_checkpoint_len: usize,
    ) -> BundleSealPlan {
        self.seal_plan_for_parts(
            seed_cursor,
            state.artifacts,
            state.pareto,
            &state.search_tail,
            state.ledger,
            intelligence_checkpoint_len,
        )
    }

    pub(super) fn seal_plan_for_parts(
        &self,
        seed_cursor: &[u8],
        artifacts: &[super::VerifiedArtifact<D>],
        pareto: &[super::VerifiedArtifact<D>],
        search_tail: &super::SearchTailView<'_, D>,
        ledger: &super::ExperienceLedger,
        intelligence_checkpoint_len: usize,
    ) -> BundleSealPlan {
        let artifact_bytes = artifacts.iter().fold(8_u64, |bytes, artifact| {
            bytes.saturating_add(artifact.inner.bundle_record.len() as u64)
        });
        let revisions = 4_u64
            .saturating_mul(32)
            .saturating_add(8)
            .saturating_add(u64::try_from(intelligence_checkpoint_len).unwrap_or(u64::MAX));
        let experience = ledger.encoded_len();
        let recovery = super::recovery_encoded_len(pareto, search_tail);
        let session = super::session_encoded_len(
            self.encoded_scope.len(),
            seed_cursor.len(),
            self.environment.len(),
        );
        let identity = u64::try_from(self.semantic_identity.as_str().len()).unwrap_or(u64::MAX);
        let logical = artifact_bytes
            .saturating_add(revisions)
            .saturating_add(experience)
            .saturating_add(recovery)
            .saturating_add(session)
            .saturating_add(identity);
        let artifact_index = u64::try_from(artifacts.len())
            .unwrap_or(u64::MAX)
            .saturating_mul(std::mem::size_of::<&super::VerifiedArtifact<D>>() as u64);
        let compressed_artifacts = compressed_capacity(artifact_bytes);
        let compressed_experience = compressed_capacity(experience);
        let maximum_encoded = FILE_FRAMING_BYTES
            .saturating_add(identity)
            .saturating_add(session)
            .saturating_add(revisions)
            .saturating_add(compressed_artifacts)
            .saturating_add(compressed_experience)
            .saturating_add(recovery);
        let output =
            bounded_output_capacity(maximum_encoded, self.request.resources.durable_bytes.get());
        let output_bytes = u64::try_from(output).unwrap_or(u64::MAX);
        // Materialization caches each canonical Artifact record. Sealing only
        // owns the complete payload and its canonical pointer index.
        let artifact_build_peak = artifact_bytes.saturating_add(artifact_index);
        let build_peak = artifact_build_peak
            .max(
                artifact_bytes
                    .saturating_add(revisions)
                    .saturating_add(recovery)
                    .saturating_add(experience),
            )
            .max(
                artifact_bytes
                    .saturating_add(revisions)
                    .saturating_add(artifact_index),
            );
        let encode_peak = logical.saturating_add(
            compressed_artifacts
                .saturating_mul(2)
                .max(compressed_artifacts.saturating_add(compressed_experience.saturating_mul(2)))
                .max(
                    compressed_artifacts
                        .saturating_add(compressed_experience)
                        .saturating_add(output_bytes),
                ),
        );
        BundleSealPlan {
            peak_transient_bytes: build_peak.max(encode_peak),
            maximum_output_capacity: output,
        }
    }

    pub(super) fn admit(
        plan: BundleSealPlan,
        resources: &ResourceEnvelopeGuard,
        resident_overlap: u64,
        additional_transient: u64,
        pending_durability: u64,
    ) -> Result<(), SessionError<D::Error>> {
        if !resources.reserve(
            ResidentReservation::live(resident_overlap)
                .with_transient(
                    plan.peak_transient_bytes
                        .saturating_add(additional_transient),
                )
                .with_pending_durability(pending_durability),
        ) {
            return Err(SessionError::Resource);
        }
        Ok(())
    }

    pub(super) fn seal_prepared(
        &self,
        seed_cursor: &[u8],
        state: RestartBundleState<'_, D>,
        session_seal: SessionSeal,
        plan: BundleSealPlan,
    ) -> Result<Vec<u8>, SessionError<D::Error>> {
        let encoded = super::encode_bundle(super::BundleEncodingInput {
            domain: self.domain,
            request: self.request,
            identity: &self.semantic_identity,
            encoded_scope: &self.encoded_scope,
            goals_digest: self.goals_digest,
            environment: &self.environment,
            seed_cursor,
            state,
            session_seal,
            maximum_output_capacity: plan.maximum_output_capacity,
        })?;
        if !plan.accepts_output_capacity(encoded.capacity()) {
            return Err(SessionError::Resource);
        }
        Ok(encoded)
    }

    pub(super) fn seal_admitted(
        &self,
        seed_cursor: &[u8],
        state: RestartBundleState<'_, D>,
        session_seal: SessionSeal,
        resources: &ResourceEnvelopeGuard,
        admission: SealAdmission,
    ) -> Result<Vec<u8>, SessionError<D::Error>> {
        let plan = self.seal_plan(
            seed_cursor,
            &state,
            state.intelligence.checkpoint_bytes().len(),
        );
        Self::admit(
            plan,
            resources,
            admission.resident_overlap,
            admission.additional_transient,
            admission.pending_durability,
        )?;
        self.seal_prepared(seed_cursor, state, session_seal, plan)
    }
}

fn compressed_capacity(logical: u64) -> u64 {
    usize::try_from(logical).map_or(u64::MAX, |logical| {
        u64::try_from(reflex_bundle::CanonicalBundle::compressed_segment_capacity(
            logical,
        ))
        .unwrap_or(u64::MAX)
    })
}

fn bounded_output_capacity(maximum_encoded: u64, durable_limit: u64) -> usize {
    usize::try_from(maximum_encoded.min(durable_limit)).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::{BundleSealPlan, bounded_output_capacity};

    #[test]
    fn admitted_output_capacity_rejects_vec_overcapacity() {
        let plan = BundleSealPlan {
            peak_transient_bytes: 1_000,
            maximum_output_capacity: 99,
        };

        assert!(plan.accepts_output_capacity(99));
        assert!(!plan.accepts_output_capacity(100));
    }

    #[test]
    fn tiny_bundle_does_not_charge_the_entire_large_durable_envelope() {
        assert_eq!(
            bounded_output_capacity(4_096, 4 * 1024 * 1024 * 1024),
            4_096
        );
    }
}
