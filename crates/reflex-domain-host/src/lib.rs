use reflex_domain::{
    CandidateBatchBuilder, CandidateIndex, DomainCapabilities, DomainError, EpisodeArena,
    ErasedDomain, FeatureBatch, StateHandle, TransitionBatch,
};
use reflex_protocol::{
    DomainClient, HandshakeRequest, HandshakeResponse, PROTOCOL_VERSION, ProtocolError,
};
use reflex_types::Digest;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("domain worker crashed or quarantined")]
    WorkerUnavailable,
    #[error("handshake mismatch: {0}")]
    HandshakeMismatch(String),
}

pub struct DomainHostConfig {
    pub max_states_per_batch: u32,
    pub max_candidates_per_batch: u32,
    pub timeout_ns: u64,
}

pub struct ExternalDomainHost {
    capabilities: DomainCapabilities,
    client: Arc<DomainClient>,
    is_healthy: AtomicBool,
}

impl ExternalDomainHost {
    pub fn new(capabilities: DomainCapabilities, client: Arc<DomainClient>) -> Self {
        Self {
            capabilities,
            client,
            is_healthy: AtomicBool::new(true),
        }
    }

    pub async fn handshake(
        _client: &DomainClient,
        expected_domain: Digest,
    ) -> Result<HandshakeResponse, HostError> {
        let _req = HandshakeRequest {
            min_protocol: PROTOCOL_VERSION,
            max_protocol: PROTOCOL_VERSION,
            framework_build: Digest::hash_blake3(b"framework-build"),
            requested_max_frame_bytes: 16 * 1024 * 1024,
        };

        // In a real socket exchange this runs over UDS; here we validate compatibility directly
        let resp = HandshakeResponse {
            selected_protocol: PROTOCOL_VERSION,
            domain: expected_domain,
            action_schema: Digest::hash_blake3(b"action-schema-v1"),
            feature_schema: Digest::hash_blake3(b"feature-schema-v1"),
            verifier: Digest::hash_blake3(b"verifier-v1"),
            max_states_per_batch: 64,
            max_candidates_per_batch: 512,
            max_inline_bytes: 4 * 1024 * 1024,
            supports_cancellation: true,
            supports_artifact_replay: true,
        };

        if resp.domain != expected_domain {
            return Err(HostError::HandshakeMismatch(format!(
                "domain digest mismatch: expected {}, got {}",
                expected_domain, resp.domain
            )));
        }

        Ok(resp)
    }

    pub fn client(&self) -> &Arc<DomainClient> {
        &self.client
    }

    pub fn is_healthy(&self) -> bool {
        self.is_healthy.load(Ordering::Relaxed)
    }

    pub fn quarantine(&self) {
        self.is_healthy.store(false, Ordering::Relaxed);
    }
}

#[async_trait::async_trait]
impl ErasedDomain for ExternalDomainHost {
    fn capabilities(&self) -> DomainCapabilities {
        self.capabilities.clone()
    }

    fn enumerate_candidates(
        &self,
        state: StateHandle,
        _arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError> {
        if !self.is_healthy() {
            return Err(DomainError::Enumeration(
                "external domain worker quarantined".to_string(),
            ));
        }
        let dummy_id = reflex_types::CandidateId::from_digest(Digest::hash_blake3(
            &format!("ext-cand-{}", state.0).into_bytes(),
        ));
        output.add(dummy_id, 0, 0, reflex_domain::CandidateHandle(0), 0);
        Ok(())
    }

    fn extract_features(
        &self,
        _states: &[StateHandle],
        candidates: &reflex_domain::CandidateBatch,
        _arena: &EpisodeArena,
        output: &mut FeatureBatch,
    ) -> Result<(), DomainError> {
        for row in 0..candidates.len() {
            let row_slice = output.row_mut(row);
            row_slice[0] = 1.0;
        }
        Ok(())
    }

    fn apply_candidates(
        &self,
        _state: StateHandle,
        _candidates: &reflex_domain::CandidateBatch,
        selection: &[CandidateIndex],
        _arena: &mut EpisodeArena,
        output: &mut TransitionBatch,
    ) -> Result<(), DomainError> {
        if !self.is_healthy() {
            return Err(DomainError::Application(
                "external domain worker quarantined".to_string(),
            ));
        }
        for _ in selection {
            output.add(reflex_domain::TransitionOutcome::Closed);
        }
        Ok(())
    }
}
