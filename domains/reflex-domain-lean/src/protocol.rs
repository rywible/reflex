//! Protocol V2 bridge for external Lean worker attachment.

use reflex_protocol::{
    DomainClient, HandshakeRequest, HandshakeResponse, PROTOCOL_VERSION, ProtocolCapability,
    ProtocolError, negotiate_handshake,
};
use reflex_types::Digest;

pub fn lean_domain_digest() -> Digest {
    Digest::hash_blake3(b"lean4-reflex-v2")
}

#[derive(Clone, Debug)]
pub struct LeanProtocolBridge {
    pub expected_domain: Digest,
}

impl LeanProtocolBridge {
    pub fn new() -> Self {
        Self {
            expected_domain: lean_domain_digest(),
        }
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

    /// Protocol mismatches fail before proof search (P13.1 AC).
    pub fn validate_handshake(
        &self,
        request: &HandshakeRequest,
        response: &HandshakeResponse,
    ) -> Result<u32, ProtocolError> {
        negotiate_handshake(
            request,
            response,
            self.expected_domain,
            ProtocolCapability::all(),
        )
    }

    pub async fn handshake(
        &self,
        client: &DomainClient,
    ) -> Result<HandshakeResponse, ProtocolError> {
        client.handshake(self.expected_domain).await
    }
}

impl Default for LeanProtocolBridge {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_mismatch_fails_before_search() {
        let bridge = LeanProtocolBridge::new();
        let req = LeanProtocolBridge::build_handshake_request();
        let resp = HandshakeResponse {
            selected_protocol: PROTOCOL_VERSION,
            domain: Digest::hash_blake3(b"wrong"),
            action_schema: Digest::ZERO,
            feature_schema: Digest::ZERO,
            verifier: Digest::ZERO,
            max_states_per_batch: 64,
            max_candidates_per_batch: 128,
            max_inline_bytes: 4096,
            supports_cancellation: true,
            supports_artifact_replay: true,
            negotiated_capabilities: ProtocolCapability::all().to_vec(),
        };
        assert!(bridge.validate_handshake(&req, &resp).is_err());
    }
}
