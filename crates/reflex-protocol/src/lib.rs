use bytes::{Buf, BufMut, Bytes, BytesMut};
use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use thiserror::Error;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::codec::{Decoder, Encoder};

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("io error: {0}")]
    Io(String),
    #[error("frame too large: {size} bytes (limit: {limit})")]
    FrameTooLarge { size: usize, limit: usize },
    #[error("handshake failed: {reason}")]
    HandshakeFailed { reason: String },
    #[error(
        "protocol version mismatch: client supported {client_min}..={client_max}, server supported {server_min}..={server_max}"
    )]
    VersionMismatch {
        client_min: u32,
        client_max: u32,
        server_min: u32,
        server_max: u32,
    },
    #[error("request timed out: {0}")]
    Timeout(u64),
    #[error("connection closed")]
    ConnectionClosed,
    #[error("decode error: {0}")]
    Decode(String),
}

impl From<std::io::Error> for ProtocolError {
    fn from(e: std::io::Error) -> Self {
        ProtocolError::Io(e.to_string())
    }
}

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_MAX_FRAME_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandshakeRequest {
    pub min_protocol: u32,
    pub max_protocol: u32,
    pub framework_build: Digest,
    pub requested_max_frame_bytes: u32,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateGroupMsg {
    pub state_handle: Vec<u8>,
    pub candidate_ids: Vec<Vec<u8>>,
    pub candidate_classes: Vec<u32>,
    pub tie_breaks: Vec<u64>,
    pub shared_feature_payload: Vec<u8>,
    pub candidate_feature_payload: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExpandBatchRequest {
    pub request_id: u64,
    pub deadline_mono_ns: u64,
    pub state_handles: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExpandBatchResponse {
    pub request_id: u64,
    pub groups: Vec<CandidateGroupMsg>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplyCandidateRequest {
    pub request_id: u64,
    pub state_handle: Vec<u8>,
    pub candidate_id: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplyCandidateResponse {
    pub request_id: u64,
    pub outcome: String, // "Closed", "Obligations", "Contradiction", "Invalid", "Unresolved"
    pub and_child_handles: Vec<Vec<u8>>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DomainMessage {
    HandshakeReq(HandshakeRequest),
    HandshakeResp(HandshakeResponse),
    ExpandReq(ExpandBatchRequest),
    ExpandResp(ExpandBatchResponse),
    ApplyReq(ApplyCandidateRequest),
    ApplyResp(ApplyCandidateResponse),
    Shutdown,
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

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }

        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[0..4]);
        let length = u32::from_le_bytes(length_bytes) as usize;

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
        let data = src.split_to(length).freeze();
        Ok(Some(data))
    }
}

impl Encoder<Bytes> for LengthDelimitedFrameCodec {
    type Error = ProtocolError;

    fn encode(&mut self, item: Bytes, dst: &mut BytesMut) -> Result<(), Self::Error> {
        if item.len() > self.max_frame_bytes {
            return Err(ProtocolError::FrameTooLarge {
                size: item.len(),
                limit: self.max_frame_bytes,
            });
        }
        dst.reserve(4 + item.len());
        dst.put_u32_le(item.len() as u32);
        dst.put_slice(&item);
        Ok(())
    }
}

pub struct DomainClient {
    next_req_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<DomainMessage>>>>,
    outgoing_tx: mpsc::Sender<DomainMessage>,
}

impl DomainClient {
    pub fn new(outgoing_tx: mpsc::Sender<DomainMessage>) -> Self {
        Self {
            next_req_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            outgoing_tx,
        }
    }

    pub async fn expand_batch(
        &self,
        state_handles: Vec<Vec<u8>>,
        deadline_mono_ns: u64,
    ) -> Result<ExpandBatchResponse, ProtocolError> {
        let req_id = self.next_req_id.fetch_add(1, Ordering::SeqCst);
        let req = ExpandBatchRequest {
            request_id: req_id,
            deadline_mono_ns,
            state_handles,
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending.lock().await.insert(req_id, reply_tx);

        self.outgoing_tx
            .send(DomainMessage::ExpandReq(req))
            .await
            .map_err(|_| ProtocolError::ConnectionClosed)?;

        let resp = reply_rx
            .await
            .map_err(|_| ProtocolError::ConnectionClosed)?;
        match resp {
            DomainMessage::ExpandResp(r) => Ok(r),
            _ => Err(ProtocolError::Decode("unexpected response message".into())),
        }
    }

    pub async fn apply_candidate(
        &self,
        state_handle: Vec<u8>,
        candidate_id: Vec<u8>,
    ) -> Result<ApplyCandidateResponse, ProtocolError> {
        let req_id = self.next_req_id.fetch_add(1, Ordering::SeqCst);
        let req = ApplyCandidateRequest {
            request_id: req_id,
            state_handle,
            candidate_id,
        };

        let (reply_tx, reply_rx) = oneshot::channel();
        self.pending.lock().await.insert(req_id, reply_tx);

        self.outgoing_tx
            .send(DomainMessage::ApplyReq(req))
            .await
            .map_err(|_| ProtocolError::ConnectionClosed)?;

        let resp = reply_rx
            .await
            .map_err(|_| ProtocolError::ConnectionClosed)?;
        match resp {
            DomainMessage::ApplyResp(r) => Ok(r),
            _ => Err(ProtocolError::Decode("unexpected response message".into())),
        }
    }

    pub async fn handle_incoming(&self, msg: DomainMessage) {
        let req_id = match &msg {
            DomainMessage::ExpandResp(r) => r.request_id,
            DomainMessage::ApplyResp(r) => r.request_id,
            _ => return,
        };

        if let Some(tx) = self.pending.lock().await.remove(&req_id) {
            let _ = tx.send(msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_length_delimited_codec() {
        let mut codec = LengthDelimitedFrameCodec::new(1024);
        let mut buffer = BytesMut::new();

        let payload = Bytes::from_static(b"test message frame");
        codec.encode(payload.clone(), &mut buffer).unwrap();

        let decoded = codec.decode(&mut buffer).unwrap().unwrap();
        assert_eq!(decoded, payload);
    }
}
