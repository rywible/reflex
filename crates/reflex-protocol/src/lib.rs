//! External-domain protocol: protobuf messages over bounded length-delimited frames.

mod transport;

pub use transport::{
    DomainConnectionSupervisor, DomainTransportKind, connect_stdio, connect_uds, encode_message,
    negotiate_protocol_version,
};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use prost::Message;
use reflex_types::{Digest, DigestAlgorithm};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use thiserror::Error;
use tokio_util::codec::{Decoder, Encoder};

pub mod domain_v1 {
    include!(concat!(env!("OUT_DIR"), "/reflex.domain.v1.rs"));
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("io error: {0}")]
    Io(String),
    #[error("frame too large: {size} bytes (limit: {limit})")]
    FrameTooLarge { size: usize, limit: usize },
    #[error("handshake failed: {reason}")]
    HandshakeFailed { reason: String },
    #[error(
        "protocol version mismatch: client {client_min}..={client_max}, server {server_min}..={server_max}"
    )]
    VersionMismatch {
        client_min: u32,
        client_max: u32,
        server_min: u32,
        server_max: u32,
    },
    #[error("unknown required capability: {0:?}")]
    UnknownRequiredCapability(ProtocolCapability),
    #[error("digest mismatch on {field}: expected {expected}, got {actual}")]
    DigestMismatch {
        field: String,
        expected: Digest,
        actual: Digest,
    },
    #[error("request {0} timed out")]
    Timeout(u64),
    #[error("duplicate request id {0}")]
    DuplicateRequest(u64),
    #[error("unexpected or late reply for request {0}")]
    UnexpectedReply(u64),
    #[error("connection is closed or quarantined")]
    ConnectionClosed,
    #[error("decode error: {0}")]
    Decode(String),
    #[error("peer error: {0}")]
    Peer(String),
}

impl From<std::io::Error> for ProtocolError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u32)]
pub enum ProtocolCapability {
    Cancellation = 1,
    ArtifactReplay = 2,
    BatchedApply = 3,
    InlineArtifacts = 4,
}

impl ProtocolCapability {
    pub fn all() -> &'static [Self] {
        &[
            Self::Cancellation,
            Self::ArtifactReplay,
            Self::BatchedApply,
            Self::InlineArtifacts,
        ]
    }

    fn from_wire(value: u32) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Cancellation),
            2 => Ok(Self::ArtifactReplay),
            3 => Ok(Self::BatchedApply),
            4 => Ok(Self::InlineArtifacts),
            _ => Err(ProtocolError::Decode(format!(
                "unknown protocol capability {value}"
            ))),
        }
    }
}

pub fn validate_required_capabilities(
    required: &[ProtocolCapability],
    supported: &[ProtocolCapability],
) -> Result<(), ProtocolError> {
    for capability in required {
        if !supported.contains(capability) {
            return Err(ProtocolError::UnknownRequiredCapability(*capability));
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandshakeRequest {
    pub min_protocol: u32,
    pub max_protocol: u32,
    pub framework_build: Digest,
    pub requested_max_frame_bytes: u32,
    pub required_capabilities: Vec<ProtocolCapability>,
    pub optional_capabilities: Vec<ProtocolCapability>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandshakeResponse {
    pub selected_protocol: u32,
    pub domain: Digest,
    pub action_schema: Digest,
    pub feature_schema: Digest,
    pub verifier: Digest,
    pub max_states_per_batch: u32,
    pub max_candidates_per_batch: u32,
    pub max_inline_bytes: u32,
    pub supports_cancellation: bool,
    pub supports_artifact_replay: bool,
    pub negotiated_capabilities: Vec<ProtocolCapability>,
}

pub fn digest_to_proto(digest: &Digest) -> domain_v1::Digest {
    domain_v1::Digest {
        algorithm: match digest.algorithm {
            DigestAlgorithm::Blake3 => "blake3",
            DigestAlgorithm::Sha256 => "sha256",
        }
        .into(),
        value: digest.as_bytes().to_vec(),
    }
}

pub fn digest_from_proto(digest: &domain_v1::Digest) -> Result<Digest, ProtocolError> {
    if digest.value.len() != 32 {
        return Err(ProtocolError::Decode(format!(
            "digest is {} bytes, expected 32",
            digest.value.len()
        )));
    }
    let mut bytes = [0; 32];
    bytes.copy_from_slice(&digest.value);
    let algorithm = match digest.algorithm.as_str() {
        "blake3" => DigestAlgorithm::Blake3,
        "sha256" => DigestAlgorithm::Sha256,
        other => {
            return Err(ProtocolError::Decode(format!(
                "unknown digest algorithm {other}"
            )));
        }
    };
    Ok(Digest { algorithm, bytes })
}

fn handshake_request_to_proto(request: &HandshakeRequest) -> domain_v1::HandshakeRequest {
    domain_v1::HandshakeRequest {
        min_protocol: request.min_protocol,
        max_protocol: request.max_protocol,
        framework_build: Some(digest_to_proto(&request.framework_build)),
        requested_max_frame_bytes: request.requested_max_frame_bytes,
        required_capabilities: request
            .required_capabilities
            .iter()
            .map(|c| *c as u32)
            .collect(),
        optional_capabilities: request
            .optional_capabilities
            .iter()
            .map(|c| *c as u32)
            .collect(),
    }
}

fn handshake_response_to_proto(response: &HandshakeResponse) -> domain_v1::HandshakeResponse {
    domain_v1::HandshakeResponse {
        selected_protocol: response.selected_protocol,
        domain: Some(digest_to_proto(&response.domain)),
        action_schema: Some(digest_to_proto(&response.action_schema)),
        feature_schema: Some(digest_to_proto(&response.feature_schema)),
        verifier: Some(digest_to_proto(&response.verifier)),
        max_states_per_batch: response.max_states_per_batch,
        max_candidates_per_batch: response.max_candidates_per_batch,
        max_inline_bytes: response.max_inline_bytes,
        supports_cancellation: response.supports_cancellation,
        supports_artifact_replay: response.supports_artifact_replay,
        negotiated_capabilities: response
            .negotiated_capabilities
            .iter()
            .map(|c| *c as u32)
            .collect(),
    }
}

/// Encodes a server handshake response without exposing conversion details.
pub fn encode_handshake_response(response: &HandshakeResponse) -> Bytes {
    encode_message(
        MessageTag::HandshakeResponse,
        &handshake_response_to_proto(response),
    )
}

fn require_digest(value: Option<&domain_v1::Digest>, field: &str) -> Result<Digest, ProtocolError> {
    value
        .ok_or_else(|| ProtocolError::Decode(format!("missing {field} digest")))
        .and_then(digest_from_proto)
}

fn handshake_response_from_proto(
    response: domain_v1::HandshakeResponse,
) -> Result<HandshakeResponse, ProtocolError> {
    Ok(HandshakeResponse {
        selected_protocol: response.selected_protocol,
        domain: require_digest(response.domain.as_ref(), "domain")?,
        action_schema: require_digest(response.action_schema.as_ref(), "action_schema")?,
        feature_schema: require_digest(response.feature_schema.as_ref(), "feature_schema")?,
        verifier: require_digest(response.verifier.as_ref(), "verifier")?,
        max_states_per_batch: response.max_states_per_batch,
        max_candidates_per_batch: response.max_candidates_per_batch,
        max_inline_bytes: response.max_inline_bytes,
        supports_cancellation: response.supports_cancellation,
        supports_artifact_replay: response.supports_artifact_replay,
        negotiated_capabilities: response
            .negotiated_capabilities
            .into_iter()
            .map(ProtocolCapability::from_wire)
            .collect::<Result<_, _>>()?,
    })
}

pub fn negotiate_handshake(
    request: &HandshakeRequest,
    response: &HandshakeResponse,
    expected_domain: Digest,
    supported: &[ProtocolCapability],
) -> Result<u32, ProtocolError> {
    let selected = negotiate_protocol_version(
        request.min_protocol,
        request.max_protocol,
        response.selected_protocol,
        response.selected_protocol,
    )?;
    validate_required_capabilities(
        &request.required_capabilities,
        &response.negotiated_capabilities,
    )?;
    validate_required_capabilities(&response.negotiated_capabilities, supported)?;
    if response.domain != expected_domain {
        return Err(ProtocolError::DigestMismatch {
            field: "domain".into(),
            expected: expected_domain,
            actual: response.domain,
        });
    }
    Ok(selected)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum MessageTag {
    HandshakeRequest = 1,
    HandshakeResponse = 2,
    TaskBootstrapRequest = 3,
    TaskBootstrapResponse = 4,
    ExpandBatchRequest = 5,
    ExpandBatchResponse = 6,
    ApplyBatchRequest = 7,
    ApplyBatchResponse = 8,
    ReconstructRequest = 9,
    ReconstructResponse = 10,
    VerifyRequest = 11,
    VerifyResponse = 12,
    UtilityRequest = 13,
    UtilityResponse = 14,
    CancelRequest = 15,
    Shutdown = 16,
}

impl MessageTag {
    fn from_byte(byte: u8) -> Result<Self, ProtocolError> {
        match byte {
            1 => Ok(Self::HandshakeRequest),
            2 => Ok(Self::HandshakeResponse),
            3 => Ok(Self::TaskBootstrapRequest),
            4 => Ok(Self::TaskBootstrapResponse),
            5 => Ok(Self::ExpandBatchRequest),
            6 => Ok(Self::ExpandBatchResponse),
            7 => Ok(Self::ApplyBatchRequest),
            8 => Ok(Self::ApplyBatchResponse),
            9 => Ok(Self::ReconstructRequest),
            10 => Ok(Self::ReconstructResponse),
            11 => Ok(Self::VerifyRequest),
            12 => Ok(Self::VerifyResponse),
            13 => Ok(Self::UtilityRequest),
            14 => Ok(Self::UtilityResponse),
            15 => Ok(Self::CancelRequest),
            16 => Ok(Self::Shutdown),
            _ => Err(ProtocolError::Decode(format!("unknown message tag {byte}"))),
        }
    }
}

pub fn decode_tag(frame: &Bytes) -> Result<MessageTag, ProtocolError> {
    frame
        .first()
        .copied()
        .ok_or_else(|| ProtocolError::Decode("empty frame".into()))
        .and_then(MessageTag::from_byte)
}

pub fn response_request_id(frame: &Bytes) -> Result<Option<u64>, ProtocolError> {
    let payload = frame
        .get(1..)
        .ok_or_else(|| ProtocolError::Decode("empty frame".into()))?;
    Ok(match decode_tag(frame)? {
        MessageTag::TaskBootstrapResponse => Some(
            domain_v1::TaskBootstrapResponse::decode(payload)
                .map_err(decode_error)?
                .request_id,
        ),
        MessageTag::ExpandBatchResponse => Some(
            domain_v1::ExpandBatchResponse::decode(payload)
                .map_err(decode_error)?
                .request_id,
        ),
        MessageTag::ApplyBatchResponse => Some(
            domain_v1::ApplyBatchResponse::decode(payload)
                .map_err(decode_error)?
                .request_id,
        ),
        MessageTag::ReconstructResponse => Some(
            domain_v1::ReconstructResponse::decode(payload)
                .map_err(decode_error)?
                .request_id,
        ),
        MessageTag::VerifyResponse => Some(
            domain_v1::VerifyResponse::decode(payload)
                .map_err(decode_error)?
                .request_id,
        ),
        MessageTag::UtilityResponse => Some(
            domain_v1::UtilityResponse::decode(payload)
                .map_err(decode_error)?
                .request_id,
        ),
        _ => None,
    })
}

fn decode_error(error: prost::DecodeError) -> ProtocolError {
    ProtocolError::Decode(error.to_string())
}

pub struct LengthDelimitedFrameCodec {
    max_frame_bytes: usize,
}
impl LengthDelimitedFrameCodec {
    pub fn new(max_frame_bytes: usize) -> Self {
        Self { max_frame_bytes }
    }
}
impl Decoder for LengthDelimitedFrameCodec {
    type Item = Bytes;
    type Error = ProtocolError;
    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Bytes>, ProtocolError> {
        if src.len() < 4 {
            return Ok(None);
        }
        let length = u32::from_le_bytes(src[..4].try_into().expect("four bytes")) as usize;
        if length > self.max_frame_bytes {
            return Err(ProtocolError::FrameTooLarge {
                size: length,
                limit: self.max_frame_bytes,
            });
        }
        if src.len() < 4 + length {
            src.reserve(4 + length - src.len());
            return Ok(None);
        }
        src.advance(4);
        Ok(Some(src.split_to(length).freeze()))
    }
}
impl Encoder<Bytes> for LengthDelimitedFrameCodec {
    type Error = ProtocolError;
    fn encode(&mut self, item: Bytes, dst: &mut BytesMut) -> Result<(), ProtocolError> {
        if item.len() > self.max_frame_bytes {
            return Err(ProtocolError::FrameTooLarge {
                size: item.len(),
                limit: self.max_frame_bytes,
            });
        }
        let length = u32::try_from(item.len()).map_err(|_| ProtocolError::FrameTooLarge {
            size: item.len(),
            limit: self.max_frame_bytes,
        })?;
        dst.reserve(4 + item.len());
        dst.put_u32_le(length);
        dst.put_slice(&item);
        Ok(())
    }
}

/// A client whose request correlation is owned by its framed supervisor.
pub struct DomainClient {
    supervisor: std::sync::Arc<DomainConnectionSupervisor>,
    handshake_complete: std::sync::atomic::AtomicBool,
    handshake: std::sync::RwLock<Option<HandshakeResponse>>,
}

impl DomainClient {
    pub fn with_supervisor(supervisor: std::sync::Arc<DomainConnectionSupervisor>) -> Self {
        Self {
            supervisor,
            handshake_complete: std::sync::atomic::AtomicBool::new(false),
            handshake: std::sync::RwLock::new(None),
        }
    }

    pub fn is_handshake_complete(&self) -> bool {
        self.handshake_complete
            .load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn is_healthy(&self) -> bool {
        self.supervisor.is_healthy()
    }
    pub fn handshake_response(&self) -> Option<HandshakeResponse> {
        self.handshake.read().ok()?.clone()
    }

    pub async fn handshake(
        &self,
        expected_domain: Digest,
    ) -> Result<HandshakeResponse, ProtocolError> {
        let request = HandshakeRequest {
            min_protocol: PROTOCOL_VERSION,
            max_protocol: PROTOCOL_VERSION,
            framework_build: Digest::hash_blake3(b"reflex-framework"),
            requested_max_frame_bytes: DEFAULT_MAX_FRAME_BYTES as u32,
            required_capabilities: vec![ProtocolCapability::BatchedApply],
            optional_capabilities: ProtocolCapability::all().to_vec(),
        };
        let frame = encode_message(
            MessageTag::HandshakeRequest,
            &handshake_request_to_proto(&request),
        );
        let reply = self
            .supervisor
            .exchange_control(frame, Duration::from_secs(10))
            .await?;
        if decode_tag(&reply)? != MessageTag::HandshakeResponse {
            return Err(ProtocolError::HandshakeFailed {
                reason: "expected handshake response".into(),
            });
        }
        let response = handshake_response_from_proto(
            domain_v1::HandshakeResponse::decode(&reply[1..]).map_err(decode_error)?,
        )?;
        negotiate_handshake(
            &request,
            &response,
            expected_domain,
            ProtocolCapability::all(),
        )?;
        *self
            .handshake
            .write()
            .map_err(|_| ProtocolError::HandshakeFailed {
                reason: "handshake state lock poisoned".into(),
            })? = Some(response.clone());
        self.handshake_complete
            .store(true, std::sync::atomic::Ordering::Release);
        Ok(response)
    }

    fn require_ready(&self) -> Result<(), ProtocolError> {
        if !self.is_handshake_complete() {
            return Err(ProtocolError::HandshakeFailed {
                reason: "handshake required before RPC".into(),
            });
        }
        if !self.is_healthy() {
            return Err(ProtocolError::ConnectionClosed);
        }
        Ok(())
    }

    async fn rpc<Req, Resp>(
        &self,
        tag: MessageTag,
        response_tag: MessageTag,
        request_id: u64,
        timeout_ns: u64,
        request: &Req,
    ) -> Result<Resp, ProtocolError>
    where
        Req: Message,
        Resp: Message + Default,
    {
        self.require_ready()?;
        let timeout = Duration::from_nanos(timeout_ns.max(1));
        let reply = self
            .supervisor
            .request(request_id, encode_message(tag, request), timeout)
            .await?;
        if decode_tag(&reply)? != response_tag {
            self.supervisor.quarantine();
            return Err(ProtocolError::Decode(
                "response tag does not match request".into(),
            ));
        }
        Resp::decode(&reply[1..]).map_err(decode_error)
    }

    pub async fn bootstrap_task(
        &self,
        task_payload: Vec<u8>,
        timeout_ns: u64,
    ) -> Result<domain_v1::TaskBootstrapResponse, ProtocolError> {
        let id = self.supervisor.next_request_id();
        let request = domain_v1::TaskBootstrapRequest {
            request_id: id,
            timeout_ns,
            task_payload,
        };
        self.rpc(
            MessageTag::TaskBootstrapRequest,
            MessageTag::TaskBootstrapResponse,
            id,
            timeout_ns,
            &request,
        )
        .await
    }
    pub async fn expand_batch(
        &self,
        state_handles: Vec<Vec<u8>>,
        timeout_ns: u64,
    ) -> Result<domain_v1::ExpandBatchResponse, ProtocolError> {
        let id = self.supervisor.next_request_id();
        let request = domain_v1::ExpandBatchRequest {
            request_id: id,
            timeout_ns,
            state_handles,
        };
        self.rpc(
            MessageTag::ExpandBatchRequest,
            MessageTag::ExpandBatchResponse,
            id,
            timeout_ns,
            &request,
        )
        .await
    }
    pub async fn apply_batch(
        &self,
        items: Vec<domain_v1::ApplyItem>,
        timeout_ns: u64,
    ) -> Result<domain_v1::ApplyBatchResponse, ProtocolError> {
        let id = self.supervisor.next_request_id();
        let request = domain_v1::ApplyBatchRequest {
            request_id: id,
            timeout_ns,
            items,
        };
        self.rpc(
            MessageTag::ApplyBatchRequest,
            MessageTag::ApplyBatchResponse,
            id,
            timeout_ns,
            &request,
        )
        .await
    }
    pub async fn reconstruct(
        &self,
        mut request: domain_v1::ReconstructRequest,
        timeout_ns: u64,
    ) -> Result<domain_v1::ReconstructResponse, ProtocolError> {
        let id = self.supervisor.next_request_id();
        request.request_id = id;
        request.timeout_ns = timeout_ns;
        self.rpc(
            MessageTag::ReconstructRequest,
            MessageTag::ReconstructResponse,
            id,
            timeout_ns,
            &request,
        )
        .await
    }
    pub async fn verify(
        &self,
        mut request: domain_v1::VerifyRequest,
        timeout_ns: u64,
    ) -> Result<domain_v1::VerifyResponse, ProtocolError> {
        let id = self.supervisor.next_request_id();
        request.request_id = id;
        request.timeout_ns = timeout_ns;
        self.rpc(
            MessageTag::VerifyRequest,
            MessageTag::VerifyResponse,
            id,
            timeout_ns,
            &request,
        )
        .await
    }
    pub async fn utility(
        &self,
        mut request: domain_v1::UtilityRequest,
        timeout_ns: u64,
    ) -> Result<domain_v1::UtilityResponse, ProtocolError> {
        let id = self.supervisor.next_request_id();
        request.request_id = id;
        request.timeout_ns = timeout_ns;
        self.rpc(
            MessageTag::UtilityRequest,
            MessageTag::UtilityResponse,
            id,
            timeout_ns,
            &request,
        )
        .await
    }
    pub async fn cancel_outstanding(&self) {
        self.supervisor.cancel_all().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_codec_roundtrip() {
        let mut codec = LengthDelimitedFrameCodec::new(32);
        let mut bytes = BytesMut::new();
        codec
            .encode(Bytes::from_static(b"frame"), &mut bytes)
            .unwrap();
        assert_eq!(
            codec.decode(&mut bytes).unwrap().unwrap(),
            Bytes::from_static(b"frame")
        );
    }
    #[test]
    fn oversized_frame_is_rejected() {
        let mut codec = LengthDelimitedFrameCodec::new(2);
        assert!(matches!(
            codec.encode(Bytes::from_static(b"long"), &mut BytesMut::new()),
            Err(ProtocolError::FrameTooLarge { .. })
        ));
    }
    #[test]
    fn required_capabilities_fail_closed() {
        assert!(matches!(
            validate_required_capabilities(
                &[ProtocolCapability::BatchedApply],
                &[ProtocolCapability::Cancellation]
            ),
            Err(ProtocolError::UnknownRequiredCapability(_))
        ));
    }
}
