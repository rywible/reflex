//! Runtime context for one research cell (§12.1).
//!
//! Allocation instrumentation and copy counters are compiled out of release
//! workers unless the `counting-allocator` feature is explicitly enabled (P1.4).
//! See [`alloc`] for counting-allocator hooks and copy tracking.

pub mod account;
pub mod cancel;
pub mod pool;
pub mod sandbox;
pub mod thread_budget;

pub mod alloc;
pub mod copy;
mod loom_tests;
pub mod manifest;

use reflex_types::{CellId, Digest, EpisodeId, KnowledgeEditionId, ModelCheckpointId};
use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

pub use account::{
    AccountingCoverage, ProcessKey, ProcessRusageSample, ProcessTreeAccountant,
    process_keys_distinct,
};
// procfs backend uses (pid, starttime) keys — PID reuse does not merge unrelated processes.
// Unsupported platforms report AccountingCoverage::Unsupported fallback.
pub use cancel::{CancellationError, CancellationToken};
pub use manifest::{
    CELL_EXECUTION_MANIFEST_SCHEMA, CELL_EXECUTION_MANIFEST_VERSION, CellExecutionManifest,
    ManifestSerializationError, ManifestValidationError, ManifestVerificationError,
    ResourceAllocation, SearchExecutionConfig, VerifiedCellManifest,
};
pub use pool::{BoundedPool, BufferPool, PoolError, PoolStats};
pub use sandbox::{SandboxError, SandboxExecutableMapping, SandboxLimits, SandboxPolicy};
pub use thread_budget::{
    BudgetError, ComputeLease, ComputePool, PoolDiagnosticsSnapshot, PoolRegistry, ThreadBudget,
};

/// A live cell-owned resource prevents terminal publication (INV-RFX-23).
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CellShutdownError {
    #[error(transparent)]
    Compute(#[from] BudgetError),
    #[error("{0} cell buffer(s) are still checked out")]
    BufferLeak(usize),
}

// ---------------------------------------------------------------------------
// Cell context (§12.1, INV-RFX-3, INV-RFX-4)
// ---------------------------------------------------------------------------

/// Opaque pinned model checkpoint (INV-RFX-3).
#[derive(Clone, Debug)]
pub struct PinnedModelCheckpoint {
    id: ModelCheckpointId,
    payload: Arc<dyn Any + Send + Sync>,
}

impl PinnedModelCheckpoint {
    fn new(id: ModelCheckpointId, payload: Arc<dyn Any + Send + Sync>) -> Self {
        Self { id, payload }
    }

    pub fn id(&self) -> &ModelCheckpointId {
        &self.id
    }

    pub fn payload<T: Any>(&self) -> Option<&T> {
        self.payload.downcast_ref()
    }
}

impl PartialEq<ModelCheckpointId> for PinnedModelCheckpoint {
    fn eq(&self, other: &ModelCheckpointId) -> bool {
        &self.id == other
    }
}

impl PartialEq<PinnedModelCheckpoint> for ModelCheckpointId {
    fn eq(&self, other: &PinnedModelCheckpoint) -> bool {
        *self == other.id
    }
}

/// Opaque pinned knowledge edition (INV-RFX-4).
#[derive(Clone, Debug)]
pub struct PinnedKnowledgeEdition {
    id: KnowledgeEditionId,
    payload: Arc<dyn Any + Send + Sync>,
}

impl PinnedKnowledgeEdition {
    fn new(id: KnowledgeEditionId, payload: Arc<dyn Any + Send + Sync>) -> Self {
        Self { id, payload }
    }

    pub fn id(&self) -> &KnowledgeEditionId {
        &self.id
    }

    pub fn payload<T: Any>(&self) -> Option<&T> {
        self.payload.downcast_ref()
    }
}

impl PartialEq<KnowledgeEditionId> for PinnedKnowledgeEdition {
    fn eq(&self, other: &KnowledgeEditionId) -> bool {
        &self.id == other
    }
}

impl PartialEq<PinnedKnowledgeEdition> for KnowledgeEditionId {
    fn eq(&self, other: &PinnedKnowledgeEdition) -> bool {
        *self == other.id
    }
}

/// Inputs resolved and loaded before a cell may start.
#[derive(Default)]
pub struct ResolvedCellInputs {
    model: Option<(ModelCheckpointId, Arc<dyn Any + Send + Sync>)>,
    knowledge: Option<(KnowledgeEditionId, Arc<dyn Any + Send + Sync>)>,
    evidence: BTreeMap<String, Digest>,
}

impl ResolvedCellInputs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_model(
        mut self,
        id: ModelCheckpointId,
        payload: Arc<dyn Any + Send + Sync>,
    ) -> Self {
        self.model = Some((id, payload));
        self
    }

    pub fn with_knowledge(
        mut self,
        id: KnowledgeEditionId,
        payload: Arc<dyn Any + Send + Sync>,
    ) -> Self {
        self.knowledge = Some((id, payload));
        self
    }

    pub fn with_evidence(mut self, role: impl Into<String>, digest: Digest) -> Self {
        self.evidence.insert(role.into(), digest);
        self
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum CellContextError {
    #[error("manifest references model checkpoint {0}, but no matching payload was resolved")]
    MissingModel(ModelCheckpointId),
    #[error("resolved model checkpoint {resolved} does not match manifest checkpoint {expected}")]
    ModelMismatch {
        expected: ModelCheckpointId,
        resolved: ModelCheckpointId,
    },
    #[error("a model payload was resolved but the manifest references no model")]
    UnexpectedModel,
    #[error("manifest references knowledge edition {0}, but no matching payload was resolved")]
    MissingKnowledge(KnowledgeEditionId),
    #[error("resolved knowledge edition {resolved} does not match manifest edition {expected}")]
    KnowledgeMismatch {
        expected: KnowledgeEditionId,
        resolved: KnowledgeEditionId,
    },
    #[error("a knowledge payload was resolved but the manifest references no knowledge edition")]
    UnexpectedKnowledge,
    #[error("resolved evidence inputs do not exactly match the manifest")]
    EvidenceMismatch,
    #[error("thread budget has {actual} permits, but manifest requires {expected}")]
    ThreadBudgetMismatch { expected: usize, actual: usize },
}

/// Per-cell context (§12.1): identity anchor, pinned model/knowledge,
/// cancellation root, thread budget, and buffer pool.
pub struct CellContext {
    cell_id: CellId,
    manifest: VerifiedCellManifest,
    model_checkpoint: Option<PinnedModelCheckpoint>,
    knowledge_edition: Option<PinnedKnowledgeEdition>,
    thread_budget: ThreadBudget,
    accountant: Arc<ProcessTreeAccountant>,
    cancellation: CancellationToken,
    buffer_pool: BufferPool,
}

impl CellContext {
    pub fn new(
        manifest: VerifiedCellManifest,
        resolved: ResolvedCellInputs,
        thread_budget: ThreadBudget,
    ) -> Result<Self, CellContextError> {
        let expected_model = manifest.manifest().model_checkpoint();
        let model_checkpoint = match (expected_model, resolved.model) {
            (Some(expected), Some((resolved, payload))) if expected == resolved => {
                Some(PinnedModelCheckpoint::new(expected, payload))
            }
            (Some(expected), Some((resolved, _))) => {
                return Err(CellContextError::ModelMismatch { expected, resolved });
            }
            (Some(expected), None) => return Err(CellContextError::MissingModel(expected)),
            (None, Some(_)) => return Err(CellContextError::UnexpectedModel),
            (None, None) => None,
        };
        let expected_knowledge = manifest.manifest().knowledge_edition();
        let knowledge_edition = match (expected_knowledge, resolved.knowledge) {
            (Some(expected), Some((resolved, payload))) if expected == resolved => {
                Some(PinnedKnowledgeEdition::new(expected, payload))
            }
            (Some(expected), Some((resolved, _))) => {
                return Err(CellContextError::KnowledgeMismatch { expected, resolved });
            }
            (Some(expected), None) => return Err(CellContextError::MissingKnowledge(expected)),
            (None, Some(_)) => return Err(CellContextError::UnexpectedKnowledge),
            (None, None) => None,
        };
        if resolved.evidence != *manifest.manifest().evidence_inputs() {
            return Err(CellContextError::EvidenceMismatch);
        }
        let expected_permits = manifest.manifest().resource().cpu_permits() as usize;
        if thread_budget.total() != expected_permits {
            return Err(CellContextError::ThreadBudgetMismatch {
                expected: expected_permits,
                actual: thread_budget.total(),
            });
        }
        let buffer_pool = BufferPool::prefilled_buffers(8, 256 * 1024);
        Ok(Self {
            cell_id: manifest.manifest().cell_id(),
            manifest,
            model_checkpoint,
            knowledge_edition,
            thread_budget,
            accountant: Arc::new(ProcessTreeAccountant::new(std::process::id())),
            cancellation: CancellationToken::new(),
            buffer_pool,
        })
    }

    pub fn cell_id(&self) -> CellId {
        self.cell_id
    }

    pub fn manifest_digest(&self) -> Digest {
        self.manifest.digest()
    }

    pub fn manifest(&self) -> &CellExecutionManifest {
        self.manifest.manifest()
    }

    pub fn model_checkpoint(&self) -> Option<&PinnedModelCheckpoint> {
        self.model_checkpoint.as_ref()
    }

    pub fn knowledge_edition(&self) -> Option<&PinnedKnowledgeEdition> {
        self.knowledge_edition.as_ref()
    }

    pub fn thread_budget(&self) -> &ThreadBudget {
        &self.thread_budget
    }

    pub fn accountant(&self) -> &Arc<ProcessTreeAccountant> {
        &self.accountant
    }

    pub fn cancellation(&self) -> &CancellationToken {
        &self.cancellation
    }

    pub fn buffer_pool(&self) -> &BufferPool {
        &self.buffer_pool
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Shutdown leak detection (P1.2).
    pub fn check_clean_shutdown(&self) -> Result<(), CellShutdownError> {
        self.thread_budget.registry().check_clean_shutdown()?;
        let checked_out = self.buffer_pool.checked_out();
        if checked_out != 0 {
            return Err(CellShutdownError::BufferLeak(checked_out));
        }
        Ok(())
    }
}

impl Drop for CellContext {
    fn drop(&mut self) {
        if let Err(error) = self.check_clean_shutdown() {
            tracing::error!(%error, "cell-owned resource leak detected at shutdown");
        }
    }
}

pub struct EpisodeContext {
    pub episode_id: EpisodeId,
    pub cell: Arc<CellContext>,
}

impl EpisodeContext {
    pub fn new(episode_id: EpisodeId, cell: Arc<CellContext>) -> Self {
        Self { episode_id, cell }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_types::{DomainId, TaskId};

    fn verified_manifest(
        model: Option<ModelCheckpointId>,
        knowledge: Option<KnowledgeEditionId>,
        evidence: BTreeMap<String, Digest>,
        cpu_permits: u32,
    ) -> VerifiedCellManifest {
        let manifest = CellExecutionManifest::new(
            CellId::from_digest(Digest::hash_blake3(b"cell")),
            TaskId::from_digest(Digest::hash_blake3(b"task")),
            DomainId::from_digest(Digest::hash_blake3(b"domain")),
            SearchExecutionConfig::new("best-first", 100, 1_000, 5_000_000),
            42,
            ResourceAllocation::new("reference", cpu_permits, 8 * 1024 * 1024, 1024),
            model,
            knowledge,
            evidence,
        )
        .unwrap();
        let bytes = manifest.to_bytes().unwrap();
        VerifiedCellManifest::verify(&bytes, manifest.digest().unwrap()).unwrap()
    }

    #[tokio::test]
    async fn test_thread_budget_enforcement() {
        let budget = ThreadBudget::new(4);
        assert_eq!(budget.available(), 4);

        let lease1 = budget.acquire("search", 3).await.unwrap();
        assert_eq!(budget.available(), 1);
        assert_eq!(budget.registry().get_active("search"), 3);

        let lease2 = budget.acquire("io", 1).await.unwrap();
        assert_eq!(budget.available(), 0);

        drop(lease1);
        assert_eq!(budget.available(), 3);
        assert_eq!(budget.registry().get_active("search"), 0);

        drop(lease2);
        assert_eq!(budget.available(), 4);
        budget.registry().check_clean_shutdown().unwrap();
    }

    #[tokio::test]
    async fn test_cell_context_pins_identity() {
        let checkpoint = ModelCheckpointId::from_digest(Digest::hash_blake3(b"mc"));
        let knowledge = KnowledgeEditionId::from_digest(Digest::hash_blake3(b"kn"));
        let evidence = BTreeMap::from([("corpus".to_string(), Digest::hash_blake3(b"corpus"))]);
        let manifest = verified_manifest(Some(checkpoint), Some(knowledge), evidence.clone(), 2);
        let resolved = ResolvedCellInputs::new()
            .with_model(checkpoint, Arc::new(vec![1u8, 2, 3]))
            .with_knowledge(knowledge, Arc::new("knowledge".to_string()))
            .with_evidence("corpus", evidence["corpus"]);
        let cell = CellContext::new(manifest, resolved, ThreadBudget::new(2)).unwrap();

        assert_eq!(cell.cell_id(), cell.manifest().cell_id());
        assert_eq!(cell.model_checkpoint().unwrap().id(), &checkpoint);
        assert_eq!(cell.knowledge_edition().unwrap().id(), &knowledge);
        assert_eq!(
            cell.model_checkpoint()
                .unwrap()
                .payload::<Vec<u8>>()
                .map(Vec::len),
            Some(3)
        );
        assert_eq!(
            cell.knowledge_edition()
                .unwrap()
                .payload::<String>()
                .map(String::as_str),
            Some("knowledge")
        );

        let child = cell.cancellation().child().unwrap();
        cell.cancel();
        assert!(child.is_cancelled());
    }

    #[test]
    fn manifest_digest_mismatch_fails_closed() {
        let manifest = verified_manifest(None, None, BTreeMap::new(), 1);
        let error = VerifiedCellManifest::verify(
            manifest.bytes(),
            Digest::hash_blake3(b"different canonical identity"),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            ManifestVerificationError::DigestMismatch { .. }
        ));
    }

    #[test]
    fn cell_context_rejects_missing_or_substituted_inputs() {
        let checkpoint = ModelCheckpointId::from_digest(Digest::hash_blake3(b"model"));
        let knowledge = KnowledgeEditionId::from_digest(Digest::hash_blake3(b"knowledge"));
        let manifest = verified_manifest(
            Some(checkpoint),
            Some(knowledge),
            BTreeMap::from([("corpus".to_string(), Digest::hash_blake3(b"corpus"))]),
            2,
        );

        assert!(matches!(
            CellContext::new(
                manifest.clone(),
                ResolvedCellInputs::new(),
                ThreadBudget::new(2)
            ),
            Err(CellContextError::MissingModel(_))
        ));
        assert!(matches!(
            CellContext::new(
                manifest.clone(),
                ResolvedCellInputs::new()
                    .with_model(
                        ModelCheckpointId::from_digest(Digest::hash_blake3(b"other")),
                        Arc::new(())
                    )
                    .with_knowledge(knowledge, Arc::new(()))
                    .with_evidence("corpus", Digest::hash_blake3(b"corpus")),
                ThreadBudget::new(2)
            ),
            Err(CellContextError::ModelMismatch { .. })
        ));
        assert!(matches!(
            CellContext::new(
                manifest.clone(),
                ResolvedCellInputs::new()
                    .with_model(checkpoint, Arc::new(()))
                    .with_knowledge(knowledge, Arc::new(())),
                ThreadBudget::new(2)
            ),
            Err(CellContextError::EvidenceMismatch)
        ));
        assert!(matches!(
            CellContext::new(
                manifest,
                ResolvedCellInputs::new()
                    .with_model(checkpoint, Arc::new(()))
                    .with_knowledge(knowledge, Arc::new(()))
                    .with_evidence("corpus", Digest::hash_blake3(b"corpus")),
                ThreadBudget::new(1)
            ),
            Err(CellContextError::ThreadBudgetMismatch { .. })
        ));
    }

    #[test]
    fn test_pinned_payload_identity_equality() {
        let checkpoint = ModelCheckpointId::from_digest(Digest::hash_blake3(b"mc"));
        let a = PinnedModelCheckpoint::new(checkpoint, Arc::new(1u8));
        let b = PinnedModelCheckpoint::new(checkpoint, Arc::new(2u8));
        assert_eq!(a, checkpoint);
        assert_eq!(b, checkpoint);
        assert_eq!(checkpoint, a);
        let _ = b;
    }

    #[test]
    fn test_buffer_pool_roundtrip() {
        let pool = BufferPool::prefilled_buffers(2, 4);
        let mut buf = pool.try_acquire().unwrap();
        buf.extend_from_slice(&[1, 2, 3]);
        assert_eq!(buf.as_slice(), &[1, 2, 3]);
        drop(buf);
        assert_eq!(pool.available(), 2);
    }
}
