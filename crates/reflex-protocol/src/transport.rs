//! Bounded framed transports with a single correlation authority.

use crate::{
    DEFAULT_MAX_FRAME_BYTES, LengthDelimitedFrameCodec, MessageTag, ProtocolError, decode_tag,
    response_request_id,
};
use bytes::{Bytes, BytesMut};
use prost::Message;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_util::codec::{Decoder, Encoder};

const CHANNEL_CAPACITY: usize = 64;
const MAX_IN_FLIGHT: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DomainTransportKind {
    Uds,
    Stdio,
}

pub struct DomainConnectionSupervisor {
    transport: DomainTransportKind,
    outgoing_tx: mpsc::Sender<Bytes>,
    control_rx: Arc<Mutex<mpsc::Receiver<Bytes>>>,
    control_lock: Mutex<()>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<Bytes>>>>,
    healthy: Arc<AtomicBool>,
}

impl DomainConnectionSupervisor {
    pub fn transport_kind(&self) -> DomainTransportKind {
        self.transport
    }
    pub fn next_request_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }
    pub fn quarantine(&self) {
        self.healthy.store(false, Ordering::Release);
    }

    pub async fn send_frame(&self, frame: Bytes) -> Result<(), ProtocolError> {
        if !self.is_healthy() {
            return Err(ProtocolError::ConnectionClosed);
        }
        self.outgoing_tx
            .send(frame)
            .await
            .map_err(|_| ProtocolError::ConnectionClosed)
    }

    /// Serializes uncorrelated control exchanges (currently handshake only).
    pub async fn exchange_control(
        &self,
        frame: Bytes,
        timeout: Duration,
    ) -> Result<Bytes, ProtocolError> {
        let _guard = self.control_lock.lock().await;
        self.send_frame(frame).await?;
        tokio::time::timeout(timeout, self.control_rx.lock().await.recv())
            .await
            .map_err(|_| ProtocolError::Timeout(0))?
            .ok_or(ProtocolError::ConnectionClosed)
    }

    pub async fn request(
        &self,
        request_id: u64,
        frame: Bytes,
        timeout: Duration,
    ) -> Result<Bytes, ProtocolError> {
        if !self.is_healthy() {
            return Err(ProtocolError::ConnectionClosed);
        }
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            if pending.len() >= MAX_IN_FLIGHT {
                return Err(ProtocolError::Io(format!(
                    "in-flight request limit {MAX_IN_FLIGHT} exceeded"
                )));
            }
            if pending.insert(request_id, sender).is_some() {
                self.quarantine();
                return Err(ProtocolError::DuplicateRequest(request_id));
            }
        }
        if let Err(error) = self.send_frame(frame).await {
            self.pending.lock().await.remove(&request_id);
            return Err(error);
        }
        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(frame)) => Ok(frame),
            Ok(Err(_)) => Err(ProtocolError::ConnectionClosed),
            Err(_) => {
                self.pending.lock().await.remove(&request_id);
                // A reply after this removal is a protocol violation and quarantines the peer.
                Err(ProtocolError::Timeout(request_id))
            }
        }
    }

    pub async fn cancel_all(&self) {
        let ids: Vec<u64> = self.pending.lock().await.keys().copied().collect();
        self.pending.lock().await.clear();
        if !ids.is_empty() {
            let cancel = crate::domain_v1::CancelRequest { request_ids: ids };
            let _ = self
                .send_frame(encode_message(MessageTag::CancelRequest, &cancel))
                .await;
        }
    }
}

pub async fn connect_uds(
    path: impl AsRef<Path>,
) -> Result<DomainConnectionSupervisor, ProtocolError> {
    let stream = std::os::unix::net::UnixStream::connect(path.as_ref())
        .map_err(|e| ProtocolError::Io(format!("uds connect: {e}")))?;
    stream.set_nonblocking(true)?;
    let reader = stream.try_clone()?;
    Ok(spawn_framed_supervisor(
        move || UnixStream::from_std(reader),
        move || UnixStream::from_std(stream),
        DomainTransportKind::Uds,
    ))
}

pub async fn connect_stdio() -> Result<DomainConnectionSupervisor, ProtocolError> {
    Ok(spawn_framed_supervisor(
        || Ok::<_, std::io::Error>(tokio::io::stdin()),
        || Ok::<_, std::io::Error>(tokio::io::stdout()),
        DomainTransportKind::Stdio,
    ))
}

fn spawn_framed_supervisor<R, W, RF, WF>(
    reader_factory: RF,
    writer_factory: WF,
    kind: DomainTransportKind,
) -> DomainConnectionSupervisor
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
    RF: FnOnce() -> Result<R, std::io::Error> + Send + 'static,
    WF: FnOnce() -> Result<W, std::io::Error> + Send + 'static,
{
    let (outgoing_tx, mut outgoing_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let (control_tx, control_rx) = mpsc::channel(CHANNEL_CAPACITY);
    let pending = Arc::new(Mutex::new(HashMap::<u64, oneshot::Sender<Bytes>>::new()));
    let healthy = Arc::new(AtomicBool::new(true));

    let reader_pending = Arc::clone(&pending);
    let reader_health = Arc::clone(&healthy);
    spawn_io_thread(
        "reflex-domain-reader",
        reader_health.clone(),
        move || async move {
            let mut reader = match reader_factory() {
                Ok(reader) => reader,
                Err(_) => {
                    reader_health.store(false, Ordering::Release);
                    return;
                }
            };
            let mut codec = LengthDelimitedFrameCodec::new(DEFAULT_MAX_FRAME_BYTES);
            let mut buffer = BytesMut::new();
            while let Ok(count) = reader.read_buf(&mut buffer).await {
                if count == 0 {
                    break;
                }
                loop {
                    let frame = match codec.decode(&mut buffer) {
                        Ok(Some(frame)) => frame,
                        Ok(None) => break,
                        Err(_) => {
                            reader_health.store(false, Ordering::Release);
                            return;
                        }
                    };
                    match response_request_id(&frame) {
                        Ok(Some(id)) => {
                            let sender = reader_pending.lock().await.remove(&id);
                            match sender {
                                Some(sender) => {
                                    let _ = sender.send(frame);
                                }
                                None => {
                                    reader_health.store(false, Ordering::Release);
                                    return;
                                }
                            }
                        }
                        Ok(None)
                            if matches!(decode_tag(&frame), Ok(MessageTag::HandshakeResponse)) =>
                        {
                            if control_tx.send(frame).await.is_err() {
                                reader_health.store(false, Ordering::Release);
                                return;
                            }
                        }
                        _ => {
                            reader_health.store(false, Ordering::Release);
                            return;
                        }
                    }
                }
            }
            reader_pending.lock().await.clear();
        },
    );

    let writer_health = Arc::clone(&healthy);
    spawn_io_thread(
        "reflex-domain-writer",
        writer_health.clone(),
        move || async move {
            let mut writer = match writer_factory() {
                Ok(writer) => writer,
                Err(_) => {
                    writer_health.store(false, Ordering::Release);
                    return;
                }
            };
            let mut codec = LengthDelimitedFrameCodec::new(DEFAULT_MAX_FRAME_BYTES);
            while let Some(frame) = outgoing_rx.recv().await {
                let mut output = BytesMut::new();
                if codec.encode(frame, &mut output).is_err()
                    || writer.write_all(&output).await.is_err()
                {
                    writer_health.store(false, Ordering::Release);
                    break;
                }
            }
        },
    );

    DomainConnectionSupervisor {
        transport: kind,
        outgoing_tx,
        control_rx: Arc::new(Mutex::new(control_rx)),
        control_lock: Mutex::new(()),
        next_id: AtomicU64::new(1),
        pending,
        healthy,
    }
}

fn spawn_io_thread<M, F>(name: &str, healthy: Arc<AtomicBool>, make_future: M)
where
    M: FnOnce() -> F + Send + 'static,
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let thread_health = Arc::clone(&healthy);
    if std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => {
                    let future = {
                        let _entered = runtime.enter();
                        make_future()
                    };
                    runtime.block_on(future);
                }
                Err(_) => thread_health.store(false, Ordering::Release),
            }
        })
        .is_err()
    {
        healthy.store(false, Ordering::Release);
    }
}

pub fn encode_message<M: Message>(tag: MessageTag, message: &M) -> Bytes {
    let mut bytes = Vec::with_capacity(1 + message.encoded_len());
    bytes.push(tag as u8);
    message
        .encode(&mut bytes)
        .expect("encoding into Vec cannot fail");
    Bytes::from(bytes)
}

pub fn negotiate_protocol_version(
    client_min: u32,
    client_max: u32,
    server_min: u32,
    server_max: u32,
) -> Result<u32, ProtocolError> {
    let lower = client_min.max(server_min);
    let upper = client_max.min(server_max);
    if lower > upper {
        return Err(ProtocolError::VersionMismatch {
            client_min,
            client_max,
            server_min,
            server_max,
        });
    }
    Ok(upper)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DomainClient, HandshakeResponse, ProtocolCapability, digest_to_proto, domain_v1};
    use prost::Message;
    use reflex_types::Digest;
    use tokio::net::UnixListener;
    #[test]
    fn version_negotiation_uses_highest_common_version() {
        assert_eq!(negotiate_protocol_version(1, 3, 2, 4).unwrap(), 3);
        assert!(negotiate_protocol_version(5, 6, 1, 2).is_err());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn uds_correlates_out_of_order_batched_replies() {
        let path = std::env::temp_dir().join(format!(
            "reflex-protocol-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = UnixListener::bind(&path).unwrap();
        let domain = Digest::hash_blake3(b"uds-domain");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let handshake = read_wire_frame(&mut stream).await;
            assert_eq!(
                decode_tag(&handshake).unwrap(),
                MessageTag::HandshakeRequest
            );
            let response = HandshakeResponse {
                selected_protocol: 1,
                domain,
                action_schema: Digest::hash_blake3(b"action"),
                feature_schema: Digest::hash_blake3(b"feature"),
                verifier: Digest::hash_blake3(b"verifier"),
                max_states_per_batch: 8,
                max_candidates_per_batch: 16,
                max_inline_bytes: 4096,
                supports_cancellation: true,
                supports_artifact_replay: true,
                negotiated_capabilities: vec![ProtocolCapability::BatchedApply],
            };
            write_wire_frame(&mut stream, crate::encode_handshake_response(&response)).await;

            let first = read_wire_frame(&mut stream).await;
            let second = read_wire_frame(&mut stream).await;
            let first = domain_v1::ExpandBatchRequest::decode(&first[1..]).unwrap();
            let second = domain_v1::ExpandBatchRequest::decode(&second[1..]).unwrap();
            for request in [second, first] {
                let response = domain_v1::ExpandBatchResponse {
                    request_id: request.request_id,
                    groups: vec![domain_v1::CandidateGroup {
                        state_handle: request.state_handles[0].clone(),
                        candidates: vec![domain_v1::CandidateEnvelope {
                            candidate_id: Some(digest_to_proto(&Digest::hash_blake3(
                                &request.state_handles[0],
                            ))),
                            handle: request.state_handles[0].clone(),
                            candidate_class: 1,
                            tie_break: request.request_id,
                            features: vec![1.0],
                        }],
                    }],
                    error: String::new(),
                };
                write_wire_frame(
                    &mut stream,
                    encode_message(MessageTag::ExpandBatchResponse, &response),
                )
                .await;
            }
        });

        let supervisor = Arc::new(connect_uds(&path).await.unwrap());
        let client = Arc::new(DomainClient::with_supervisor(supervisor));
        client.handshake(domain).await.unwrap();
        let a = {
            let client = Arc::clone(&client);
            tokio::spawn(async move {
                client
                    .expand_batch(vec![b"a".to_vec()], 1_000_000_000)
                    .await
                    .unwrap()
            })
        };
        let b = {
            let client = Arc::clone(&client);
            tokio::spawn(async move {
                client
                    .expand_batch(vec![b"b".to_vec()], 1_000_000_000)
                    .await
                    .unwrap()
            })
        };
        let (a, b) = tokio::join!(a, b);
        assert_eq!(a.unwrap().groups[0].state_handle, b"a");
        assert_eq!(b.unwrap().groups[0].state_handle, b"b");
        server.await.unwrap();
        let _ = std::fs::remove_file(path);
    }

    async fn read_wire_frame(stream: &mut UnixStream) -> Bytes {
        let mut length = [0; 4];
        stream.read_exact(&mut length).await.unwrap();
        let mut payload = vec![0; u32::from_le_bytes(length) as usize];
        stream.read_exact(&mut payload).await.unwrap();
        Bytes::from(payload)
    }

    async fn write_wire_frame(stream: &mut UnixStream, frame: Bytes) {
        stream
            .write_all(&(frame.len() as u32).to_le_bytes())
            .await
            .unwrap();
        stream.write_all(&frame).await.unwrap();
    }
}
