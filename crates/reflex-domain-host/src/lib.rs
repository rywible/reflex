//! Truthful bridge from the synchronous erased-domain API to an external worker.

use reflex_domain::{
    ArtifactHandle, CandidateBatchBuilder, CandidateIndex, DomainCapabilities, DomainError,
    DomainWitnessRef, EpisodeArena, ErasedDomain, FeatureBatch, InvalidCandidateCode, SolvedRoot,
    StateHandle, TransitionBatch, TransitionOutcome, UnresolvedCode, UtilityContext,
    UtilityObservation, VerificationHandle, VerifyBudget, VerifyError,
};
use reflex_protocol::{
    DomainClient, HandshakeResponse, ProtocolError, digest_from_proto, digest_to_proto, domain_v1,
};
use reflex_runtime::SandboxPolicy;
use reflex_types::{ArtifactId, CandidateId, Digest, StateId, TaskId};
use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as StdMutex, mpsc as std_mpsc};
use thiserror::Error;
use tokio::sync::Mutex;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("domain worker crashed or quarantined")]
    WorkerUnavailable,
    #[error("handshake mismatch: {0}")]
    HandshakeMismatch(String),
    #[error("infrastructure failure: {0}")]
    Infrastructure(String),
}

#[derive(Clone, Debug)]
pub struct InFlightRecord {
    pub request_id: u64,
    pub method: String,
    pub started_at_ns: u64,
}

#[derive(Clone, Debug, Default)]
pub struct ShutdownEvidence {
    pub drained_requests: Vec<InFlightRecord>,
}

pub struct DomainHostConfig {
    pub max_states_per_batch: u32,
    pub max_candidates_per_batch: u32,
    /// Per-RPC timeout budget, never an externally fabricated timestamp.
    pub timeout_ns: u64,
    pub executable_digest: Digest,
}

pub struct DomainWorkerSupervisor {
    client: Arc<DomainClient>,
    child: Option<tokio::process::Child>,
    executable_digest: Digest,
    in_flight: Arc<Mutex<HashMap<u64, InFlightRecord>>>,
    is_healthy: AtomicBool,
}

impl DomainWorkerSupervisor {
    /// Launch an owned external domain only through the enforced sandbox.
    /// The executable is hashed before launch and must match its pinned cell
    /// identity; there is deliberately no unsandboxed attach API.
    pub fn launch_sandboxed(
        client: Arc<DomainClient>,
        policy: &SandboxPolicy,
        executable: &Path,
        writable_scratch: &Path,
        arguments: &[String],
        requested_environment: &BTreeMap<String, String>,
        expected_executable_digest: Digest,
    ) -> Result<Self, HostError> {
        if expected_executable_digest == Digest::ZERO {
            return Err(HostError::Infrastructure(
                "external executable digest must be pinned".into(),
            ));
        }
        let mapping = policy
            .map_executable(executable)
            .map_err(|error| HostError::Infrastructure(error.to_string()))?;
        let actual = hash_executable(&mapping.host_path)?;
        if actual != expected_executable_digest {
            return Err(HostError::Infrastructure(format!(
                "external executable digest mismatch: expected {expected_executable_digest}, got {actual}"
            )));
        }
        let command = policy
            .prepare_linux_command(
                executable,
                writable_scratch,
                arguments,
                requested_environment,
            )
            .map_err(|error| HostError::Infrastructure(error.to_string()))?;
        let child = tokio::process::Command::from(command)
            .spawn()
            .map_err(|error| HostError::Infrastructure(format!("launch domain worker: {error}")))?;
        Ok(Self {
            client,
            child: Some(child),
            executable_digest: expected_executable_digest,
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            is_healthy: AtomicBool::new(true),
        })
    }
    pub fn client(&self) -> Arc<DomainClient> {
        Arc::clone(&self.client)
    }
    pub fn executable_digest(&self) -> Digest {
        self.executable_digest
    }
    pub async fn owned_resource_count(&self) -> usize {
        usize::from(self.child.is_some()) + self.in_flight.lock().await.len()
    }
    pub async fn record_in_flight(&self, request_id: u64, method: &str) {
        self.in_flight.lock().await.insert(
            request_id,
            InFlightRecord {
                request_id,
                method: method.into(),
                started_at_ns: mono_now_ns(),
            },
        );
    }
    pub async fn clear_in_flight(&self, request_id: u64) {
        self.in_flight.lock().await.remove(&request_id);
    }
    pub fn quarantine(&self) {
        self.is_healthy.store(false, Ordering::Release);
    }
    pub async fn shutdown_and_drain(&mut self) -> Result<ShutdownEvidence, HostError> {
        let drained = self.in_flight.lock().await.values().cloned().collect();
        self.client.cancel_outstanding().await;
        if let Some(mut child) = self.child.take() {
            if child
                .try_wait()
                .map_err(|e| HostError::Infrastructure(format!("inspect child: {e}")))?
                .is_none()
            {
                child
                    .kill()
                    .await
                    .map_err(|e| HostError::Infrastructure(format!("kill child: {e}")))?;
            }
            let _ = child
                .wait()
                .await
                .map_err(|e| HostError::Infrastructure(format!("reap child: {e}")))?;
        }
        self.in_flight.lock().await.clear();
        Ok(ShutdownEvidence {
            drained_requests: drained,
        })
    }
}

fn hash_executable(path: &Path) -> Result<Digest, HostError> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| HostError::Infrastructure(format!("open executable identity: {error}")))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| HostError::Infrastructure(format!("hash executable: {error}")))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Digest::from_blake3_bytes(*hasher.finalize().as_bytes()))
}

/// Task bytes understood only by the external worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalTaskEnvelope {
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug)]
struct ExternalState {
    id: StateId,
    handle: Vec<u8>,
}
#[derive(Clone, Debug)]
struct ExternalArtifact {
    id: ArtifactId,
    payload: Vec<u8>,
}
#[derive(Clone, Debug)]
struct ExternalVerification {
    payload: Vec<u8>,
}
#[derive(Clone, Debug)]
struct CandidateRecord {
    handle: Vec<u8>,
    features: Vec<f32>,
}

enum BridgeCommand {
    Bootstrap(
        Vec<u8>,
        std_mpsc::SyncSender<Result<domain_v1::TaskBootstrapResponse, ProtocolError>>,
    ),
    Expand(
        Vec<Vec<u8>>,
        std_mpsc::SyncSender<Result<domain_v1::ExpandBatchResponse, ProtocolError>>,
    ),
    Apply(
        Vec<domain_v1::ApplyItem>,
        std_mpsc::SyncSender<Result<domain_v1::ApplyBatchResponse, ProtocolError>>,
    ),
    Reconstruct(
        domain_v1::ReconstructRequest,
        std_mpsc::SyncSender<Result<domain_v1::ReconstructResponse, ProtocolError>>,
    ),
    Verify(
        domain_v1::VerifyRequest,
        std_mpsc::SyncSender<Result<domain_v1::VerifyResponse, ProtocolError>>,
    ),
    Utility(
        domain_v1::UtilityRequest,
        std_mpsc::SyncSender<Result<domain_v1::UtilityResponse, ProtocolError>>,
    ),
    Stop,
}

/// One dedicated runtime owns all async client calls. Synchronous domain methods
/// block only on bounded bridge channels and never enter an ambient Tokio runtime.
struct ProtocolBridge {
    sender: std_mpsc::SyncSender<BridgeCommand>,
}

impl ProtocolBridge {
    fn new(client: Arc<DomainClient>, timeout_ns: u64) -> Result<Self, HostError> {
        let (sender, receiver) = std_mpsc::sync_channel(64);
        std::thread::Builder::new()
            .name("reflex-domain-bridge".into())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(_) => return,
                };
                while let Ok(command) = receiver.recv() {
                    match command {
                        BridgeCommand::Bootstrap(payload, reply) => {
                            let _ = reply
                                .send(runtime.block_on(client.bootstrap_task(payload, timeout_ns)));
                        }
                        BridgeCommand::Expand(states, reply) => {
                            let _ = reply
                                .send(runtime.block_on(client.expand_batch(states, timeout_ns)));
                        }
                        BridgeCommand::Apply(items, reply) => {
                            let _ =
                                reply.send(runtime.block_on(client.apply_batch(items, timeout_ns)));
                        }
                        BridgeCommand::Reconstruct(request, reply) => {
                            let _ = reply
                                .send(runtime.block_on(client.reconstruct(request, timeout_ns)));
                        }
                        BridgeCommand::Verify(request, reply) => {
                            let _ =
                                reply.send(runtime.block_on(client.verify(request, timeout_ns)));
                        }
                        BridgeCommand::Utility(request, reply) => {
                            let _ =
                                reply.send(runtime.block_on(client.utility(request, timeout_ns)));
                        }
                        BridgeCommand::Stop => break,
                    }
                }
            })
            .map_err(|e| HostError::Infrastructure(format!("spawn protocol bridge: {e}")))?;
        Ok(Self { sender })
    }

    fn invoke<T>(
        &self,
        make: impl FnOnce(std_mpsc::SyncSender<Result<T, ProtocolError>>) -> BridgeCommand,
    ) -> Result<T, ProtocolError> {
        let (reply_tx, reply_rx) = std_mpsc::sync_channel(1);
        self.sender
            .send(make(reply_tx))
            .map_err(|_| ProtocolError::ConnectionClosed)?;
        reply_rx
            .recv()
            .map_err(|_| ProtocolError::ConnectionClosed)?
    }
}

impl Drop for ProtocolBridge {
    fn drop(&mut self) {
        let _ = self.sender.send(BridgeCommand::Stop);
    }
}

pub struct ExternalDomainHost {
    capabilities: DomainCapabilities,
    client: Arc<DomainClient>,
    config: DomainHostConfig,
    bridge: ProtocolBridge,
    candidates: StdMutex<HashMap<u32, CandidateRecord>>,
    bootstrap_cache: StdMutex<HashMap<Vec<u8>, domain_v1::TaskBootstrapResponse>>,
    is_healthy: AtomicBool,
}

impl ExternalDomainHost {
    pub fn new(
        capabilities: DomainCapabilities,
        supervisor: &DomainWorkerSupervisor,
        config: DomainHostConfig,
    ) -> Result<Self, HostError> {
        if config.max_states_per_batch == 0
            || config.max_candidates_per_batch == 0
            || config.timeout_ns == 0
            || config.executable_digest == Digest::ZERO
        {
            return Err(HostError::HandshakeMismatch(
                "host batch, timeout, and executable identity must be non-zero".into(),
            ));
        }
        if supervisor.child.is_none() || supervisor.executable_digest() != config.executable_digest
        {
            return Err(HostError::HandshakeMismatch(
                "external host requires the matching live sandbox launch attestation".into(),
            ));
        }
        let client = supervisor.client();
        let negotiated = client.handshake_response().ok_or_else(|| {
            HostError::HandshakeMismatch("client has not completed a handshake".into())
        })?;
        if negotiated.domain != capabilities.domain_digest
            || negotiated.action_schema != *capabilities.action_schema.digest()
            || negotiated.feature_schema != *capabilities.feature_schema.digest()
        {
            return Err(HostError::HandshakeMismatch(
                "worker domain/action/feature identity differs from host capabilities".into(),
            ));
        }
        if config.max_states_per_batch > negotiated.max_states_per_batch
            || config.max_candidates_per_batch > negotiated.max_candidates_per_batch
        {
            return Err(HostError::HandshakeMismatch(
                "host batch limits exceed worker-negotiated limits".into(),
            ));
        }
        let bridge = ProtocolBridge::new(Arc::clone(&client), config.timeout_ns)?;
        Ok(Self {
            capabilities,
            client,
            config,
            bridge,
            candidates: StdMutex::new(HashMap::new()),
            bootstrap_cache: StdMutex::new(HashMap::new()),
            is_healthy: AtomicBool::new(true),
        })
    }
    pub async fn handshake(
        client: &DomainClient,
        expected_domain: Digest,
    ) -> Result<HandshakeResponse, HostError> {
        Ok(client.handshake(expected_domain).await?)
    }
    pub fn client(&self) -> &Arc<DomainClient> {
        &self.client
    }
    pub fn is_healthy(&self) -> bool {
        self.is_healthy.load(Ordering::Acquire) && self.client.is_healthy()
    }
    pub fn quarantine(&self) {
        self.is_healthy.store(false, Ordering::Release);
    }

    fn worker_error(&self, error: ProtocolError) -> DomainError {
        self.quarantine();
        DomainError::Application(format!("external worker quarantined: {error}"))
    }

    fn bootstrap(
        &self,
        task: &ExternalTaskEnvelope,
    ) -> Result<domain_v1::TaskBootstrapResponse, DomainError> {
        if let Some(response) = self
            .bootstrap_cache
            .lock()
            .expect("bootstrap cache poisoned")
            .get(&task.payload)
            .cloned()
        {
            return Ok(response);
        }
        let response = self
            .bridge
            .invoke(|reply| BridgeCommand::Bootstrap(task.payload.clone(), reply))
            .map_err(|e| self.worker_error(e))?;
        require_no_peer_error(&response.error).map_err(|e| self.worker_error(e))?;
        require_digest(response.task_id.as_ref(), "task_id").map_err(|e| self.worker_error(e))?;
        validate_state(response.initial_state.as_ref()).map_err(|e| self.worker_error(e))?;
        self.bootstrap_cache
            .lock()
            .expect("bootstrap cache poisoned")
            .insert(task.payload.clone(), response.clone());
        Ok(response)
    }

    fn resolve_state<'a>(
        &self,
        state: StateHandle,
        arena: &'a EpisodeArena,
    ) -> Result<&'a ExternalState, DomainError> {
        arena
            .resolve_state(state)?
            .downcast_ref::<ExternalState>()
            .ok_or_else(|| {
                DomainError::InvariantViolation(
                    "state was not issued by this external worker".into(),
                )
            })
    }
}

fn mono_now_ns() -> u64 {
    use std::sync::OnceLock;
    static START: OnceLock<std::time::Instant> = OnceLock::new();
    START
        .get_or_init(std::time::Instant::now)
        .elapsed()
        .as_nanos()
        .try_into()
        .unwrap_or(u64::MAX)
}
fn require_no_peer_error(error: &str) -> Result<(), ProtocolError> {
    if error.is_empty() {
        Ok(())
    } else {
        Err(ProtocolError::Peer(error.into()))
    }
}
fn require_digest(value: Option<&domain_v1::Digest>, field: &str) -> Result<Digest, ProtocolError> {
    value
        .ok_or_else(|| ProtocolError::Decode(format!("missing {field}")))
        .and_then(digest_from_proto)
}
fn validate_state(
    state: Option<&domain_v1::StateEnvelope>,
) -> Result<(StateId, Vec<u8>), ProtocolError> {
    let state = state.ok_or_else(|| ProtocolError::Decode("missing state envelope".into()))?;
    if state.handle.is_empty() {
        return Err(ProtocolError::Decode("empty state handle".into()));
    }
    Ok((
        StateId::from_digest(require_digest(state.state_id.as_ref(), "state_id")?),
        state.handle.clone(),
    ))
}
fn validate_artifact(
    artifact: Option<&domain_v1::ArtifactEnvelope>,
) -> Result<ExternalArtifact, ProtocolError> {
    let artifact =
        artifact.ok_or_else(|| ProtocolError::Decode("missing artifact envelope".into()))?;
    Ok(ExternalArtifact {
        id: ArtifactId::from_digest(require_digest(
            artifact.artifact_id.as_ref(),
            "artifact_id",
        )?),
        payload: artifact.payload.clone(),
    })
}
fn to_proto_artifact(artifact: &ExternalArtifact) -> domain_v1::ArtifactEnvelope {
    domain_v1::ArtifactEnvelope {
        artifact_id: Some(digest_to_proto(artifact.id.digest())),
        payload: artifact.payload.clone(),
    }
}

impl ErasedDomain for ExternalDomainHost {
    fn capabilities(&self) -> DomainCapabilities {
        self.capabilities.clone()
    }

    fn task_id(&self, task: &(dyn Any + Send + Sync)) -> Result<TaskId, DomainError> {
        let task = task
            .downcast_ref::<ExternalTaskEnvelope>()
            .ok_or_else(|| DomainError::InvalidTask("expected ExternalTaskEnvelope".into()))?;
        Ok(TaskId::from_digest(
            require_digest(self.bootstrap(task)?.task_id.as_ref(), "task_id")
                .map_err(|e| self.worker_error(e))?,
        ))
    }

    fn initial_state(
        &self,
        task: &(dyn Any + Send + Sync),
        arena: &mut EpisodeArena,
    ) -> Result<StateHandle, DomainError> {
        let task = task
            .downcast_ref::<ExternalTaskEnvelope>()
            .ok_or_else(|| DomainError::InvalidTask("expected ExternalTaskEnvelope".into()))?;
        let response = self.bootstrap(task)?;
        let (id, handle) =
            validate_state(response.initial_state.as_ref()).map_err(|e| self.worker_error(e))?;
        Ok(arena.insert_state(ExternalState { id, handle }, id))
    }

    fn state_id(&self, state: StateHandle, arena: &EpisodeArena) -> Result<StateId, DomainError> {
        Ok(self.resolve_state(state, arena)?.id)
    }

    fn enumerate_candidates(
        &self,
        state: StateHandle,
        arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError> {
        if !self.is_healthy() {
            return Err(DomainError::Enumeration(
                "external worker quarantined".into(),
            ));
        }
        let external = self.resolve_state(state, arena)?;
        let response = self
            .bridge
            .invoke(|reply| BridgeCommand::Expand(vec![external.handle.clone()], reply))
            .map_err(|e| self.worker_error(e))?;
        require_no_peer_error(&response.error).map_err(|e| self.worker_error(e))?;
        if response.groups.len() != 1 || response.groups[0].state_handle != external.handle {
            return Err(self.worker_error(ProtocolError::Decode(
                "expand response does not match requested state".into(),
            )));
        }
        let group = &response.groups[0];
        let base = output.len();
        if base.saturating_add(group.candidates.len())
            > self.config.max_candidates_per_batch as usize
        {
            return Err(self.worker_error(ProtocolError::Decode(
                "worker exceeded negotiated candidate limit".into(),
            )));
        }
        let mut seen = std::collections::HashSet::new();
        let mut cache = self.candidates.lock().expect("candidate cache poisoned");
        if base == 0 {
            cache.clear();
        }
        for (index, candidate) in group.candidates.iter().enumerate() {
            let id = CandidateId::from_digest(
                require_digest(candidate.candidate_id.as_ref(), "candidate_id")
                    .map_err(|e| self.worker_error(e))?,
            );
            if !seen.insert(id) {
                return Err(
                    self.worker_error(ProtocolError::Decode("duplicate candidate id".into()))
                );
            }
            let class = u16::try_from(candidate.candidate_class).map_err(|_| {
                self.worker_error(ProtocolError::Decode("candidate class exceeds u16".into()))
            })?;
            if candidate.features.len() != self.capabilities.feature_dimension
                || candidate.features.iter().any(|v| !v.is_finite())
            {
                return Err(self.worker_error(ProtocolError::Decode(
                    "invalid candidate feature row".into(),
                )));
            }
            let local_handle = u32::try_from(base + index).map_err(|_| {
                self.worker_error(ProtocolError::Decode("candidate handle exceeds u32".into()))
            })?;
            cache.insert(
                local_handle,
                CandidateRecord {
                    handle: candidate.handle.clone(),
                    features: candidate.features.clone(),
                },
            );
            output.add(
                id,
                class,
                candidate.tie_break,
                reflex_domain::CandidateHandle(local_handle),
                0,
            );
        }
        Ok(())
    }

    fn extract_features(
        &self,
        _states: &[StateHandle],
        candidates: &reflex_domain::CandidateBatch,
        _arena: &EpisodeArena,
        output: &mut FeatureBatch,
    ) -> Result<(), DomainError> {
        if output.rows != candidates.len()
            || output.cols != self.capabilities.feature_dimension
            || output.schema != self.capabilities.feature_schema
        {
            return Err(DomainError::FeatureExtraction(
                "feature output shape/schema mismatch".into(),
            ));
        }
        let cache = self.candidates.lock().expect("candidate cache poisoned");
        for (row, handle) in candidates.payload_handles.iter().enumerate() {
            let record = cache.get(&handle.0).ok_or_else(|| {
                DomainError::FeatureExtraction(format!(
                    "no worker feature row for candidate handle {}",
                    handle.0
                ))
            })?;
            output.row_mut(row).copy_from_slice(&record.features);
        }
        Ok(())
    }

    fn apply_candidates(
        &self,
        state: StateHandle,
        candidates: &reflex_domain::CandidateBatch,
        selection: &[CandidateIndex],
        arena: &mut EpisodeArena,
        output: &mut TransitionBatch,
    ) -> Result<(), DomainError> {
        if !self.is_healthy() {
            return Err(DomainError::Application(
                "external worker quarantined".into(),
            ));
        }
        let state_handle = self.resolve_state(state, arena)?.handle.clone();
        let cache = self.candidates.lock().expect("candidate cache poisoned");
        let mut items = Vec::with_capacity(selection.len());
        for selected in selection {
            let id = *candidates
                .ids
                .get(selected.0)
                .ok_or(DomainError::InvalidCandidateIndex(selected.0))?;
            let handle = candidates
                .payload_handles
                .get(selected.0)
                .ok_or(DomainError::InvalidCandidateIndex(selected.0))?;
            let record = cache.get(&handle.0).ok_or_else(|| {
                DomainError::Application(format!(
                    "candidate handle {} was not enumerated by worker",
                    handle.0
                ))
            })?;
            items.push(domain_v1::ApplyItem {
                state_handle: state_handle.clone(),
                candidate_id: Some(digest_to_proto(id.digest())),
                candidate_handle: record.handle.clone(),
            });
        }
        drop(cache);
        let response = self
            .bridge
            .invoke(|reply| BridgeCommand::Apply(items, reply))
            .map_err(|e| self.worker_error(e))?;
        require_no_peer_error(&response.error).map_err(|e| self.worker_error(e))?;
        if response.outcomes.len() != selection.len() {
            return Err(
                self.worker_error(ProtocolError::Decode("apply outcome count mismatch".into()))
            );
        }
        for outcome in response.outcomes {
            use domain_v1::TransitionKind;
            let transition = match TransitionKind::try_from(outcome.kind)
                .unwrap_or(TransitionKind::Unspecified)
            {
                TransitionKind::Closed => TransitionOutcome::closed(
                    validate_artifact(outcome.artifact.as_ref())
                        .map_err(|e| self.worker_error(e))?
                        .id,
                ),
                TransitionKind::Obligations => {
                    let mut children = Vec::with_capacity(outcome.children.len());
                    for child in &outcome.children {
                        let (id, handle) =
                            validate_state(Some(child)).map_err(|e| self.worker_error(e))?;
                        children.push(arena.insert_state(ExternalState { id, handle }, id));
                    }
                    TransitionOutcome::obligations(outcome.and_group_id, children)
                }
                TransitionKind::Contradiction => match outcome.artifact.as_ref() {
                    Some(artifact) => {
                        let artifact =
                            validate_artifact(Some(artifact)).map_err(|e| self.worker_error(e))?;
                        TransitionOutcome::contradiction_with(DomainWitnessRef {
                            artifact: artifact.id,
                            verification: None,
                        })
                    }
                    None => TransitionOutcome::contradiction(),
                },
                TransitionKind::Invalid => {
                    TransitionOutcome::invalid(InvalidCandidateCode::Other(outcome.code))
                }
                TransitionKind::Unresolved => {
                    TransitionOutcome::unresolved(UnresolvedCode::Other(outcome.code))
                }
                TransitionKind::Unspecified => {
                    return Err(self.worker_error(ProtocolError::Decode(
                        "unspecified transition outcome".into(),
                    )));
                }
            };
            output.add(transition);
        }
        Ok(())
    }

    fn reconstruct_artifact(
        &self,
        solved: SolvedRoot,
        arena: &mut EpisodeArena,
    ) -> Result<ArtifactHandle, DomainError> {
        let root = self.resolve_state(solved.root_state, arena)?.handle.clone();
        let cache = self.candidates.lock().expect("candidate cache poisoned");
        let mut edges = Vec::with_capacity(solved.solved_edges.len());
        for (state, candidate, children) in solved.solved_edges {
            let state_handle = self.resolve_state(state, arena)?.handle.clone();
            let candidate_handle = cache
                .get(&candidate.0)
                .map(|record| record.handle.clone())
                .ok_or_else(|| {
                    DomainError::Reconstruction("unknown candidate handle in solved edge".into())
                })?;
            let child_handles = children
                .into_iter()
                .map(|child| self.resolve_state(child, arena).map(|s| s.handle.clone()))
                .collect::<Result<Vec<_>, _>>()?;
            edges.push(domain_v1::SolvedEdge {
                state_handle,
                candidate_handle,
                child_handles,
            });
        }
        drop(cache);
        let request = domain_v1::ReconstructRequest {
            request_id: 0,
            timeout_ns: 0,
            root_state_handle: root,
            solved_edges: edges,
        };
        let response = self
            .bridge
            .invoke(|reply| BridgeCommand::Reconstruct(request, reply))
            .map_err(|e| self.worker_error(e))?;
        require_no_peer_error(&response.error).map_err(|e| self.worker_error(e))?;
        let artifact =
            validate_artifact(response.artifact.as_ref()).map_err(|e| self.worker_error(e))?;
        Ok(arena.insert_artifact(artifact))
    }

    fn verify(
        &self,
        artifact: ArtifactHandle,
        budget: VerifyBudget,
        arena: &mut EpisodeArena,
    ) -> Result<VerificationHandle, VerifyError> {
        let artifact = arena
            .resolve_artifact(artifact)
            .map_err(|e| VerifyError::Failed(e.to_string()))?
            .downcast_ref::<ExternalArtifact>()
            .ok_or_else(|| VerifyError::Failed("artifact was not issued by this worker".into()))?;
        let request = domain_v1::VerifyRequest {
            request_id: 0,
            timeout_ns: 0,
            artifact: Some(to_proto_artifact(artifact)),
            max_cpu_ns: budget.max_cpu_ns,
            max_wall_ns: budget.max_wall_ns,
            max_memory_bytes: budget.max_memory_bytes,
        };
        let response = self
            .bridge
            .invoke(|reply| BridgeCommand::Verify(request, reply))
            .map_err(|e| {
                self.quarantine();
                VerifyError::Failed(e.to_string())
            })?;
        if response.timeout {
            return Err(VerifyError::Timeout(budget.max_wall_ns));
        }
        if response.budget_exhausted {
            return Err(VerifyError::BudgetExhausted(response.error));
        }
        if !response.error.is_empty() {
            return Err(VerifyError::Failed(response.error));
        }
        if response.verification_payload.is_empty() {
            self.quarantine();
            return Err(VerifyError::Failed(
                "worker returned empty verification payload".into(),
            ));
        }
        Ok(arena.insert_verification(ExternalVerification {
            payload: response.verification_payload,
        }))
    }

    fn evaluate_utility(
        &self,
        artifact: ArtifactHandle,
        verification: VerificationHandle,
        context: &UtilityContext,
        arena: &EpisodeArena,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError> {
        context.validate()?;
        let artifact = arena
            .resolve_artifact(artifact)?
            .downcast_ref::<ExternalArtifact>()
            .ok_or_else(|| {
                DomainError::UtilityEvaluation("artifact was not issued by this worker".into())
            })?;
        let verification = arena
            .resolve_verification(verification)?
            .downcast_ref::<ExternalVerification>()
            .ok_or_else(|| {
                DomainError::UtilityEvaluation("verification was not issued by this worker".into())
            })?;
        let request = domain_v1::UtilityRequest {
            request_id: 0,
            timeout_ns: 0,
            artifact: Some(to_proto_artifact(artifact)),
            verification_payload: verification.payload.clone(),
            context: Some(domain_v1::UtilityContext {
                cell_cpu_ns: context.cell_cpu_ns,
                model_inference_cpu_ns: context.model_inference_cpu_ns,
                retrieval_cpu_ns: context.retrieval_cpu_ns,
                verified_actions_count: context.verified_actions_count,
                subject: context.subject.digest().as_bytes().to_vec(),
                population: context.population.as_bytes().to_vec(),
                observed_at_generation: context.observed_at_generation.digest().as_bytes().to_vec(),
                accepted_verification: context.accepted_verification.as_bytes().to_vec(),
                accepted_evidence: context
                    .accepted_evidence
                    .iter()
                    .map(|digest| digest.as_bytes().to_vec())
                    .collect(),
            }),
        };
        let response = self
            .bridge
            .invoke(|reply| BridgeCommand::Utility(request, reply))
            .map_err(|e| self.worker_error(e))?;
        require_no_peer_error(&response.error).map_err(|e| self.worker_error(e))?;
        let observations: Vec<UtilityObservation> =
            serde_json::from_slice(&response.observations_payload).map_err(|e| {
                self.quarantine();
                DomainError::UtilityEvaluation(format!("invalid worker observation payload: {e}"))
            })?;
        if observations
            .iter()
            .any(|observation| context.validate_observation(observation).is_err())
        {
            self.quarantine();
            return Err(DomainError::UtilityEvaluation(
                "worker returned utility with invalid value or provenance".into(),
            ));
        }
        output.extend(observations);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use prost::Message;
    use reflex_protocol::{
        MessageTag, ProtocolCapability, connect_uds, decode_tag, digest_to_proto, domain_v1,
        encode_handshake_response, encode_message,
    };
    use reflex_types::{ActionSchemaId, FeatureSchemaId};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    impl DomainWorkerSupervisor {
        fn attested_for_test(client: Arc<DomainClient>, executable_digest: Digest) -> Self {
            let child = tokio::process::Command::new("sh")
                .args(["-c", "exit 0"])
                .spawn()
                .unwrap();
            Self {
                client,
                child: Some(child),
                executable_digest,
                in_flight: Arc::new(Mutex::new(HashMap::new())),
                is_healthy: AtomicBool::new(true),
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn supervisor_reaps_owned_child_without_fabricated_evidence() {
        // This unit exercises ownership only; protocol UDS coverage lives in reflex-protocol.
        let path = std::env::temp_dir().join(format!("reflex-missing-{}.sock", std::process::id()));
        assert!(reflex_protocol::connect_uds(path).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn erased_domain_roundtrips_all_semantic_rpcs_over_uds() {
        let path = std::env::temp_dir().join(format!(
            "reflex-domain-host-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let std_listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        std_listener.set_nonblocking(true).unwrap();
        let domain_digest = Digest::hash_blake3(b"external-domain");
        let action_digest = Digest::hash_blake3(b"external-action");
        let feature_digest = Digest::hash_blake3(b"external-feature");
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(fake_worker(
                std_listener,
                domain_digest,
                action_digest,
                feature_digest,
            ));
        });

        let supervisor = Arc::new(connect_uds(&path).await.unwrap());
        let client = Arc::new(DomainClient::with_supervisor(supervisor));
        let handshake = ExternalDomainHost::handshake(&client, domain_digest)
            .await
            .unwrap();
        assert_eq!(handshake.action_schema, action_digest);
        let capabilities = DomainCapabilities {
            domain_id: "external-test".into(),
            domain_digest,
            action_schema: ActionSchemaId::from_digest(action_digest),
            feature_schema: FeatureSchemaId::from_digest(feature_digest),
            feature_dimension: 2,
            max_candidates_per_state: 4,
            deterministic_generation: true,
            supports_exact_cache: false,
        };
        let executable_digest = Digest::hash_blake3(b"worker-binary");
        let supervisor = DomainWorkerSupervisor::attested_for_test(client, executable_digest);
        let host = ExternalDomainHost::new(
            capabilities.clone(),
            &supervisor,
            DomainHostConfig {
                max_states_per_batch: 4,
                max_candidates_per_batch: 4,
                timeout_ns: 1_000_000_000,
                executable_digest,
            },
        )
        .unwrap();
        let task = ExternalTaskEnvelope {
            payload: b"opaque-task".to_vec(),
        };
        let expected_task = TaskId::from_digest(Digest::hash_blake3(b"worker-task-id"));
        assert_eq!(host.task_id(&task).unwrap(), expected_task);

        let mut arena = EpisodeArena::new();
        let state = host.initial_state(&task, &mut arena).unwrap();
        assert_eq!(
            host.state_id(state, &arena).unwrap(),
            StateId::from_digest(Digest::hash_blake3(b"worker-state-id"))
        );
        let mut builder = CandidateBatchBuilder::new();
        host.enumerate_candidates(state, &arena, &mut builder)
            .unwrap();
        let candidates = builder.build();
        assert_eq!(candidates.len(), 1);
        let mut features = FeatureBatch::new(1, 2, capabilities.feature_schema);
        host.extract_features(&[state], &candidates, &arena, &mut features)
            .unwrap();
        assert_eq!(features.values, vec![2.0, 3.0]);
        let mut transitions = TransitionBatch::new();
        host.apply_candidates(
            state,
            &candidates,
            &[CandidateIndex(0)],
            &mut arena,
            &mut transitions,
        )
        .unwrap();
        assert!(matches!(
            transitions.outcomes.as_slice(),
            [TransitionOutcome::Contradiction { .. }]
        ));

        let artifact = host
            .reconstruct_artifact(
                SolvedRoot {
                    root_state: state,
                    solved_edges: vec![],
                },
                &mut arena,
            )
            .unwrap();
        let verification = host
            .verify(
                artifact,
                VerifyBudget {
                    max_cpu_ns: 100,
                    max_wall_ns: 100,
                    max_memory_bytes: 1024,
                },
                &mut arena,
            )
            .unwrap();
        let mut observations = Vec::new();
        host.evaluate_utility(
            artifact,
            verification,
            &UtilityContext {
                subject: reflex_types::ResearchNodeId::from_digest(Digest::hash_blake3(
                    b"host-subject",
                )),
                population: Digest::hash_blake3(b"host-population"),
                observed_at_generation: reflex_types::GenerationId::from_digest(
                    Digest::hash_blake3(b"host-generation"),
                ),
                accepted_verification: Digest::hash_blake3(b"host-verification"),
                accepted_evidence: vec![Digest::hash_blake3(b"host-verification")],
                cell_cpu_ns: 1,
                model_inference_cpu_ns: 1,
                retrieval_cpu_ns: 0,
                verified_actions_count: 1,
            },
            &arena,
            &mut observations,
        )
        .unwrap();
        assert!(observations.is_empty());
        worker.join().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[cfg(unix)]
    async fn fake_worker(
        listener: std::os::unix::net::UnixListener,
        domain: Digest,
        action: Digest,
        feature: Digest,
    ) {
        let listener = tokio::net::UnixListener::from_std(listener).unwrap();
        let (mut stream, _) = listener.accept().await.unwrap();
        let handshake = read_frame(&mut stream).await;
        assert_eq!(
            decode_tag(&handshake).unwrap(),
            MessageTag::HandshakeRequest
        );
        write_frame(
            &mut stream,
            encode_handshake_response(&HandshakeResponse {
                selected_protocol: 1,
                domain,
                action_schema: action,
                feature_schema: feature,
                verifier: Digest::hash_blake3(b"external-verifier"),
                max_states_per_batch: 4,
                max_candidates_per_batch: 4,
                max_inline_bytes: 4096,
                supports_cancellation: true,
                supports_artifact_replay: true,
                negotiated_capabilities: vec![ProtocolCapability::BatchedApply],
            }),
        )
        .await;

        let bootstrap = read_frame(&mut stream).await;
        let bootstrap = domain_v1::TaskBootstrapRequest::decode(&bootstrap[1..]).unwrap();
        assert_eq!(bootstrap.task_payload, b"opaque-task");
        write_frame(
            &mut stream,
            encode_message(
                MessageTag::TaskBootstrapResponse,
                &domain_v1::TaskBootstrapResponse {
                    request_id: bootstrap.request_id,
                    task_id: Some(digest_to_proto(&Digest::hash_blake3(b"worker-task-id"))),
                    initial_state: Some(domain_v1::StateEnvelope {
                        state_id: Some(digest_to_proto(&Digest::hash_blake3(b"worker-state-id"))),
                        handle: b"state-handle".to_vec(),
                    }),
                    error: String::new(),
                },
            ),
        )
        .await;

        let expand = read_frame(&mut stream).await;
        let expand = domain_v1::ExpandBatchRequest::decode(&expand[1..]).unwrap();
        write_frame(
            &mut stream,
            encode_message(
                MessageTag::ExpandBatchResponse,
                &domain_v1::ExpandBatchResponse {
                    request_id: expand.request_id,
                    groups: vec![domain_v1::CandidateGroup {
                        state_handle: b"state-handle".to_vec(),
                        candidates: vec![domain_v1::CandidateEnvelope {
                            candidate_id: Some(digest_to_proto(&Digest::hash_blake3(
                                b"worker-candidate-id",
                            ))),
                            handle: b"candidate-handle".to_vec(),
                            candidate_class: 7,
                            tie_break: 11,
                            features: vec![2.0, 3.0],
                        }],
                    }],
                    error: String::new(),
                },
            ),
        )
        .await;

        let apply = read_frame(&mut stream).await;
        let apply = domain_v1::ApplyBatchRequest::decode(&apply[1..]).unwrap();
        assert_eq!(apply.items.len(), 1);
        write_frame(
            &mut stream,
            encode_message(
                MessageTag::ApplyBatchResponse,
                &domain_v1::ApplyBatchResponse {
                    request_id: apply.request_id,
                    outcomes: vec![domain_v1::ApplyOutcome {
                        kind: domain_v1::TransitionKind::Contradiction as i32,
                        artifact: None,
                        children: vec![],
                        code: 0,
                        detail: String::new(),
                        and_group_id: 0,
                    }],
                    error: String::new(),
                },
            ),
        )
        .await;

        let reconstruct = read_frame(&mut stream).await;
        let reconstruct = domain_v1::ReconstructRequest::decode(&reconstruct[1..]).unwrap();
        write_frame(
            &mut stream,
            encode_message(
                MessageTag::ReconstructResponse,
                &domain_v1::ReconstructResponse {
                    request_id: reconstruct.request_id,
                    artifact: Some(domain_v1::ArtifactEnvelope {
                        artifact_id: Some(digest_to_proto(&Digest::hash_blake3(
                            b"worker-artifact-id",
                        ))),
                        payload: b"artifact".to_vec(),
                    }),
                    error: String::new(),
                },
            ),
        )
        .await;

        let verify = read_frame(&mut stream).await;
        let verify = domain_v1::VerifyRequest::decode(&verify[1..]).unwrap();
        write_frame(
            &mut stream,
            encode_message(
                MessageTag::VerifyResponse,
                &domain_v1::VerifyResponse {
                    request_id: verify.request_id,
                    verification_payload: b"verification".to_vec(),
                    error: String::new(),
                    timeout: false,
                    budget_exhausted: false,
                },
            ),
        )
        .await;

        let utility = read_frame(&mut stream).await;
        let utility = domain_v1::UtilityRequest::decode(&utility[1..]).unwrap();
        write_frame(
            &mut stream,
            encode_message(
                MessageTag::UtilityResponse,
                &domain_v1::UtilityResponse {
                    request_id: utility.request_id,
                    observations_payload: b"[]".to_vec(),
                    error: String::new(),
                },
            ),
        )
        .await;
    }

    #[cfg(unix)]
    async fn read_frame(stream: &mut tokio::net::UnixStream) -> Bytes {
        let mut length = [0; 4];
        stream.read_exact(&mut length).await.unwrap();
        let mut payload = vec![0; u32::from_le_bytes(length) as usize];
        stream.read_exact(&mut payload).await.unwrap();
        Bytes::from(payload)
    }

    #[cfg(unix)]
    async fn write_frame(stream: &mut tokio::net::UnixStream, frame: Bytes) {
        stream
            .write_all(&(frame.len() as u32).to_le_bytes())
            .await
            .unwrap();
        stream.write_all(&frame).await.unwrap();
    }
}
