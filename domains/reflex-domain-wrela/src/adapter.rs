//! External domain adapter — protocol bridge with replay/restart support.

use crate::certificate::CertificateStrategy;
use crate::kernel_package::KernelPackage;
use reflex_protocol::{
    DomainClient, HandshakeRequest, HandshakeResponse, PROTOCOL_VERSION, ProtocolCapability,
    ProtocolError, negotiate_handshake,
};
use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrelaReplayState {
    pub kernel_id: String,
    pub package_digest: Digest,
    pub selected_strategy: CertificateStrategy,
    pub proof_step: u32,
}

#[derive(Clone, Debug)]
pub struct WrelaAdapterState {
    replay_log: Arc<Mutex<Vec<WrelaReplayState>>>,
}

impl WrelaAdapterState {
    pub fn new() -> Self {
        Self {
            replay_log: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn record(&self, state: WrelaReplayState) -> Result<(), String> {
        self.replay_log
            .lock()
            .map_err(|_| "Wrela replay journal lock was poisoned".to_string())?
            .push(state);
        Ok(())
    }

    pub fn replay_identical(&self) -> Result<bool, String> {
        let log = self
            .replay_log
            .lock()
            .map_err(|_| "Wrela replay journal lock was poisoned".to_string())?;
        if log.len() < 2 {
            return Ok(true);
        }
        Ok(log.windows(2).all(|window| window[0] == window[1]))
    }

    pub fn restart_replay(
        &self,
        pkg: &KernelPackage,
        strategy: CertificateStrategy,
    ) -> Result<WrelaReplayState, String> {
        let package_digest = pkg.identity().map_err(|error| error.to_string())?;
        if package_digest == Digest::ZERO {
            return Err("Wrela replay package identity must be non-zero".into());
        }
        let state = WrelaReplayState {
            kernel_id: pkg.kernel_id.clone(),
            package_digest,
            selected_strategy: strategy,
            proof_step: 0,
        };
        self.record(state.clone())?;
        Ok(state)
    }
}

impl Default for WrelaAdapterState {
    fn default() -> Self {
        Self::new()
    }
}

/// Protocol handshake for external Wrela worker — fails closed without transport.
pub async fn adapter_handshake(
    client: &DomainClient,
    expected_domain: Digest,
) -> Result<HandshakeResponse, ProtocolError> {
    client.handshake(expected_domain).await
}

pub fn validate_protocol_before_search(
    request: &HandshakeRequest,
    response: &HandshakeResponse,
    expected_domain: Digest,
) -> Result<u32, ProtocolError> {
    negotiate_handshake(
        request,
        response,
        expected_domain,
        ProtocolCapability::all(),
    )
}

pub fn build_handshake_request() -> HandshakeRequest {
    HandshakeRequest {
        min_protocol: PROTOCOL_VERSION,
        max_protocol: PROTOCOL_VERSION,
        framework_build: Digest::hash_blake3(b"reflex-framework"),
        requested_max_frame_bytes: reflex_protocol::DEFAULT_MAX_FRAME_BYTES as u32,
        required_capabilities: vec![ProtocolCapability::Cancellation],
        optional_capabilities: ProtocolCapability::all().to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_restart_replay_identical() {
        let adapter = WrelaAdapterState::new();
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let s1 = adapter
            .restart_replay(&pkg, CertificateStrategy::BernsteinPolynomial)
            .unwrap();
        let s2 = adapter
            .restart_replay(&pkg, CertificateStrategy::BernsteinPolynomial)
            .unwrap();
        assert_eq!(s1, s2);
        assert!(adapter.replay_identical().unwrap());
    }

    #[test]
    fn test_protocol_mismatch_fails() {
        let req = build_handshake_request();
        let resp = HandshakeResponse {
            selected_protocol: PROTOCOL_VERSION,
            domain: Digest::hash_blake3(b"wrong-domain"),
            action_schema: Digest::ZERO,
            feature_schema: Digest::ZERO,
            verifier: Digest::ZERO,
            max_states_per_batch: 32,
            max_candidates_per_batch: 128,
            max_inline_bytes: 4096,
            supports_cancellation: true,
            supports_artifact_replay: true,
            negotiated_capabilities: ProtocolCapability::all().to_vec(),
        };
        let expected = Digest::hash_blake3(b"wrela-v1-domain");
        assert!(validate_protocol_before_search(&req, &resp, expected).is_err());
    }
}
