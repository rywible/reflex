use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use reflex_types::{
    CompatibilityDigest, CuratorSpec, Digest, KnowledgeEditionId, KnowledgeRecordId, RetrieverSpec,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use thiserror::Error;
use tokio::sync::RwLock;

pub const MAX_RETRIEVAL_TOP_K: usize = 1_024;
pub const MAX_POSTINGS_PER_TERM: usize = 65_536;
pub const MAX_POSTINGS_PER_QUERY: usize = 65_536;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum KnowledgeError {
    #[error("knowledge edition not found: {0}")]
    EditionNotFound(KnowledgeEditionId),
    #[error("record not found: {0}")]
    RecordNotFound(KnowledgeRecordId),
    #[error("schema or compatibility mismatch")]
    CompatibilityMismatch,
    #[error("curation error: {0}")]
    Curation(String),
    #[error("verification required: record lacks accepted receipt")]
    VerificationRequired,
    #[error("record is archived")]
    Archived,
    #[error("cycle detected in acyclic lineage edge")]
    CycleDetected,
    #[error("knowledge utility arithmetic overflow")]
    ArithmeticOverflow,
    #[error("non-finite knowledge utility")]
    NonFiniteUtility,
    #[error("knowledge identity mismatch: expected {expected}, supplied {supplied}")]
    IdentityMismatch { expected: String, supplied: String },
    #[error("knowledge storage error: {0}")]
    Storage(String),
    #[error("knowledge proof/certificate digest must be non-zero")]
    ProofRequired,
    #[error("invalid knowledge record: {0}")]
    InvalidRecord(String),
    #[error("retrieval top-k {requested} exceeds the registered maximum {maximum}")]
    QueryLimitExceeded { requested: usize, maximum: usize },
    #[error("knowledge overlay not found: {0}")]
    OverlayNotFound(Digest),
    #[error("knowledge overlay is not pinned to the requested parent edition")]
    OverlayParentMismatch,
    #[error("knowledge edition is not registered or differs from its registered manifest")]
    UnregisteredEdition,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeClass {
    ExactMemory,
    TheoremFact,
    ProofMacro,
    RewriteRule,
    ResearchArtifact,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeRecord {
    pub id: KnowledgeRecordId,
    pub class: KnowledgeClass,
    pub statement: String,
    pub proof_or_cert_digest: Digest,
    pub structural_tags: Vec<String>,
    pub canonical_key: Option<String>,
    pub discovery_generation: u32,
    pub utility_score: f64,
    pub verification_receipt: Digest,
    pub archived: bool,
}

impl KnowledgeRecord {
    /// Derive the only valid record ID from the immutable record payload.
    pub fn canonical_id(&self) -> Result<KnowledgeRecordId, KnowledgeError> {
        let mut bytes = Vec::new();
        let mut writer = CanonicalWriter::new(&mut bytes);
        self.encode_identity(&mut writer)
            .map_err(|error| KnowledgeError::InvalidRecord(error.to_string()))?;
        Ok(KnowledgeRecordId::from_digest(Digest::hash_blake3(&bytes)))
    }

    fn encode_identity(&self, out: &mut CanonicalWriter<'_>) -> Result<(), CanonicalError> {
        out.write_str("reflex.knowledge-record.v1")?;
        out.write_u8(match self.class {
            KnowledgeClass::ExactMemory => 0,
            KnowledgeClass::TheoremFact => 1,
            KnowledgeClass::ProofMacro => 2,
            KnowledgeClass::RewriteRule => 3,
            KnowledgeClass::ResearchArtifact => 4,
        })?;
        out.write_str(self.statement.trim())?;
        out.write_digest(&self.proof_or_cert_digest)?;
        let tags: BTreeSet<&str> = self
            .structural_tags
            .iter()
            .map(|tag| tag.trim())
            .filter(|tag| !tag.is_empty())
            .collect();
        out.write_u32(
            tags.len()
                .try_into()
                .map_err(|_| CanonicalError::LengthOverflow {
                    length: tags.len(),
                    limit: 32,
                })?,
        )?;
        for tag in tags {
            out.write_str(tag)?;
        }
        match self
            .canonical_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            Some(key) => {
                out.write_u8(1)?;
                out.write_str(key)?;
            }
            None => out.write_u8(0)?,
        }
        out.write_u32(self.discovery_generation)?;
        out.write_f64(self.utility_score)?;
        out.write_digest(&self.verification_receipt)
    }

    fn normalize(&mut self) {
        self.statement = self.statement.trim().to_owned();
        self.structural_tags = self
            .structural_tags
            .drain(..)
            .map(|tag| tag.trim().to_owned())
            .filter(|tag| !tag.is_empty())
            .collect();
        self.structural_tags.sort();
        self.structural_tags.dedup();
        self.canonical_key = self
            .canonical_key
            .take()
            .map(|key| key.trim().to_owned())
            .filter(|key| !key.is_empty());
    }

    fn validate(&self) -> Result<(), KnowledgeError> {
        if self.statement.is_empty() {
            return Err(KnowledgeError::InvalidRecord(
                "statement must be non-empty".to_owned(),
            ));
        }
        if self.proof_or_cert_digest == Digest::ZERO {
            return Err(KnowledgeError::ProofRequired);
        }
        if self.verification_receipt == Digest::ZERO {
            return Err(KnowledgeError::VerificationRequired);
        }
        if !self.utility_score.is_finite() {
            return Err(KnowledgeError::NonFiniteUtility);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnowledgeEdition {
    pub id: KnowledgeEditionId,
    pub domain_id: String,
    pub compatibility: CompatibilityDigest,
    pub records: Vec<KnowledgeRecordId>,
    pub parent_edition: Option<KnowledgeEditionId>,
    pub retriever: RetrieverSpec,
    pub curator: CuratorSpec,
    pub curation_policy: Digest,
}

impl KnowledgeEdition {
    pub fn canonical_id(&self) -> Result<KnowledgeEditionId, KnowledgeError> {
        let mut bytes = Vec::new();
        let mut writer = CanonicalWriter::new(&mut bytes);
        self.encode_identity(&mut writer)
            .map_err(|error| KnowledgeError::Curation(error.to_string()))?;
        Ok(KnowledgeEditionId::from_digest(Digest::hash_blake3(&bytes)))
    }

    fn encode_identity(&self, out: &mut CanonicalWriter<'_>) -> Result<(), CanonicalError> {
        out.write_str("reflex.knowledge-edition.v1")?;
        out.write_str(&self.domain_id)?;
        out.write_digest(self.compatibility.digest())?;
        out.write_u32(self.records.len().try_into().map_err(|_| {
            CanonicalError::LengthOverflow {
                length: self.records.len(),
                limit: 32,
            }
        })?)?;
        for record in &self.records {
            out.write_digest(record.digest())?;
        }
        match self.parent_edition {
            Some(parent) => {
                out.write_u8(1)?;
                out.write_digest(parent.digest())?;
            }
            None => out.write_u8(0)?,
        }
        out.write_digest(self.retriever.digest())?;
        out.write_digest(self.curator.digest())?;
        out.write_digest(&self.curation_policy)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeUseLevel {
    Retrieved,
    Attempted,
    TransitionSucceeded,
    PresentInFinalProof,
    NecessaryLeaveOneOut,
    ReducedWorkMatchedRerun,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeUseRecord {
    pub record_id: KnowledgeRecordId,
    pub level: KnowledgeUseLevel,
    pub retrieval_rank: Option<u32>,
    pub retrieval_score: Option<f64>,
    pub actions_saved: i64,
    pub cpu_saved_ns: i64,
    pub causal_confidence: ConfidenceLevel,
    pub proof_receipt: Option<Digest>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfidenceLevel {
    Observational,
    MatchedControl,
    LeaveOneOut,
    AmbiguousAlternative,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageTier {
    Hot,
    Warm,
    Archive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurationPolicy {
    pub max_hot_records: usize,
    pub max_warm_records: usize,
    pub min_utility_threshold: f64,
    pub baseline: CurationBaseline,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CurationBaseline {
    NoCuration,
    Frequency,
    Compression,
    MarginalUtility,
    HumanSeeded,
}

impl Default for CurationPolicy {
    fn default() -> Self {
        Self {
            max_hot_records: 10_000,
            max_warm_records: 100_000,
            min_utility_threshold: 0.01,
            baseline: CurationBaseline::MarginalUtility,
        }
    }
}

impl CurationPolicy {
    pub fn identity(&self) -> Result<Digest, KnowledgeError> {
        if !self.min_utility_threshold.is_finite() {
            return Err(KnowledgeError::NonFiniteUtility);
        }
        self.max_hot_records
            .checked_add(self.max_warm_records)
            .ok_or(KnowledgeError::ArithmeticOverflow)?;
        let mut bytes = Vec::new();
        let mut writer = CanonicalWriter::new(&mut bytes);
        writer
            .write_str("reflex.curation-policy.v1")
            .and_then(|()| writer.write_u64(self.max_hot_records as u64))
            .and_then(|()| writer.write_u64(self.max_warm_records as u64))
            .and_then(|()| writer.write_f64(self.min_utility_threshold))
            .and_then(|()| {
                writer.write_u8(match self.baseline {
                    CurationBaseline::NoCuration => 0,
                    CurationBaseline::Frequency => 1,
                    CurationBaseline::Compression => 2,
                    CurationBaseline::MarginalUtility => 3,
                    CurationBaseline::HumanSeeded => 4,
                })
            })
            .map_err(|error| KnowledgeError::Curation(error.to_string()))?;
        Ok(Digest::hash_blake3(&bytes))
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RecordUtilityLedger {
    pub query_count: u64,
    pub proof_use_count: u64,
    /// Signed: negative values mean the record increased verified work.
    pub marginal_work_saved: i64,
    /// Signed: negative values mean the record consumed additional CPU.
    pub cpu_saved_ns: i64,
    pub compression_contribution: f64,
    pub descendant_value: f64,
    pub discovery_cost: f64,
    pub retrieval_tax_ns: u64,
    pub has_observed_utility: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetrievalQuery {
    pub tag: Option<String>,
    pub canonical_key: Option<String>,
    pub structural_features: Vec<u32>,
    pub include_archived: bool,
    pub top_k: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetrievalHit {
    pub record_id: KnowledgeRecordId,
    pub score: f64,
    pub reason: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RetrievalWork {
    pub postings_scanned: u64,
    pub records_examined: u64,
    pub cpu_ns: u64,
    /// True when a registered posting or per-query work cap omitted candidates.
    pub truncated: bool,
}

#[async_trait::async_trait]
pub trait Retriever: Send + Sync {
    async fn retrieve(
        &self,
        edition: &KnowledgeEdition,
        query: &RetrievalQuery,
    ) -> Result<(Vec<RetrievalHit>, RetrievalWork), KnowledgeError>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverlaySnapshot {
    pub digest: Digest,
    pub record_ids: Vec<KnowledgeRecordId>,
    pub parent_edition: KnowledgeEditionId,
}

impl OverlaySnapshot {
    fn canonical_digest(&self) -> Digest {
        let mut bytes = Vec::with_capacity(64 + self.record_ids.len() * 32);
        bytes.extend_from_slice(b"reflex.knowledge-overlay.v1\0");
        bytes.extend_from_slice(&self.parent_edition.digest().bytes);
        for record in &self.record_ids {
            bytes.extend_from_slice(&record.digest().bytes);
        }
        Digest::hash_blake3(&bytes)
    }
}

#[derive(Clone, Debug, Default)]
struct BoundedPostingList {
    ids: Vec<KnowledgeRecordId>,
    omitted: u64,
}

impl BoundedPostingList {
    fn insert(&mut self, id: KnowledgeRecordId) {
        match self.ids.binary_search(&id) {
            Ok(_) => (),
            Err(index) if self.ids.len() < MAX_POSTINGS_PER_TERM => self.ids.insert(index, id),
            Err(index) if index < MAX_POSTINGS_PER_TERM => {
                self.ids.insert(index, id);
                self.ids.pop();
                self.omitted = self.omitted.saturating_add(1);
            }
            Err(_) => self.omitted = self.omitted.saturating_add(1),
        }
    }
}

pub struct KnowledgeStore {
    records: RwLock<HashMap<KnowledgeRecordId, KnowledgeRecord>>,
    editions: RwLock<HashMap<KnowledgeEditionId, KnowledgeEdition>>,
    record_tiers: RwLock<HashMap<KnowledgeRecordId, StorageTier>>,
    ledgers: RwLock<HashMap<KnowledgeRecordId, RecordUtilityLedger>>,
    exact_index: RwLock<BTreeMap<String, BoundedPostingList>>,
    inverted_index: RwLock<BTreeMap<String, BoundedPostingList>>,
    #[allow(dead_code)] // reserved for structural-feature retrieval (P8.x)
    structural_index: RwLock<BTreeMap<u32, Vec<KnowledgeRecordId>>>,
    overlay_snapshots: RwLock<BTreeMap<Digest, OverlaySnapshot>>,
    current_curation_policy: RwLock<Digest>,
    cas_root: PathBuf,
}

impl KnowledgeStore {
    pub fn new() -> Result<Self, KnowledgeError> {
        Self::with_cas_root(PathBuf::from(".reflex/knowledge-cas"))
    }

    pub fn with_cas_root(cas_root: PathBuf) -> Result<Self, KnowledgeError> {
        std::fs::create_dir_all(&cas_root)
            .map_err(|error| KnowledgeError::Storage(error.to_string()))?;
        Ok(Self {
            records: RwLock::new(HashMap::new()),
            editions: RwLock::new(HashMap::new()),
            record_tiers: RwLock::new(HashMap::new()),
            ledgers: RwLock::new(HashMap::new()),
            exact_index: RwLock::new(BTreeMap::new()),
            inverted_index: RwLock::new(BTreeMap::new()),
            structural_index: RwLock::new(BTreeMap::new()),
            overlay_snapshots: RwLock::new(BTreeMap::new()),
            current_curation_policy: RwLock::new(
                CurationPolicy::default()
                    .identity()
                    .expect("built-in curation policy is valid"),
            ),
            cas_root,
        })
    }

    pub async fn add_verified_record(
        &self,
        mut record: KnowledgeRecord,
    ) -> Result<(), KnowledgeError> {
        let supplied_id = record.id;
        record.normalize();
        record.validate()?;
        let expected_id = record.canonical_id()?;
        if supplied_id != expected_id {
            return Err(KnowledgeError::IdentityMismatch {
                expected: expected_id.to_hex(),
                supplied: supplied_id.to_hex(),
            });
        }
        let id = record.id;
        {
            let records = self.records.read().await;
            if let Some(existing) = records.get(&id) {
                let existing_bytes = serde_json::to_vec(existing)
                    .map_err(|error| KnowledgeError::Curation(error.to_string()))?;
                let new_bytes = serde_json::to_vec(&record)
                    .map_err(|error| KnowledgeError::Curation(error.to_string()))?;
                if existing_bytes != new_bytes {
                    return Err(KnowledgeError::IdentityMismatch {
                        expected: id.to_hex(),
                        supplied: "same ID with different payload".to_owned(),
                    });
                }
                return Ok(());
            }
        }
        self.persist_record_cas(&record).await?;
        self.records.write().await.insert(id, record.clone());
        if let Some(key) = &record.canonical_key {
            self.exact_index
                .write()
                .await
                .entry(key.clone())
                .or_default()
                .insert(id);
        }
        let mut inverted = self.inverted_index.write().await;
        for tag in &record.structural_tags {
            inverted.entry(tag.clone()).or_default().insert(id);
        }
        drop(inverted);
        self.record_tiers.write().await.insert(
            id,
            if record.archived {
                StorageTier::Archive
            } else {
                StorageTier::Hot
            },
        );
        self.ledgers.write().await.entry(id).or_default();
        Ok(())
    }

    async fn persist_record_cas(&self, record: &KnowledgeRecord) -> Result<(), KnowledgeError> {
        let bytes =
            serde_json::to_vec(record).map_err(|e| KnowledgeError::Curation(e.to_string()))?;
        let digest = Digest::hash_blake3(&bytes);
        let path = self.cas_root.join(format!("{}.json", digest.to_hex()));
        if !path.exists() {
            tokio::fs::write(path, bytes)
                .await
                .map_err(|e| KnowledgeError::Curation(e.to_string()))?;
        }
        Ok(())
    }

    pub async fn edit_record(
        &self,
        old_id: KnowledgeRecordId,
        mut updated: KnowledgeRecord,
        domain_id: &str,
        compatibility: CompatibilityDigest,
    ) -> Result<KnowledgeEdition, KnowledgeError> {
        if !self.records.read().await.contains_key(&old_id) {
            return Err(KnowledgeError::RecordNotFound(old_id));
        }
        updated.normalize();
        updated.id = updated.canonical_id()?;
        self.add_verified_record(updated).await?;
        self.record_tiers
            .write()
            .await
            .insert(old_id, StorageTier::Archive);
        self.create_edition(domain_id, compatibility, None).await
    }

    pub async fn create_edition(
        &self,
        domain_id: &str,
        compatibility: CompatibilityDigest,
        parent: Option<KnowledgeEditionId>,
    ) -> Result<KnowledgeEdition, KnowledgeError> {
        let curation_policy = *self.current_curation_policy.read().await;
        self.create_edition_with_identity(
            domain_id,
            compatibility,
            parent,
            RetrieverSpec::from_digest(Digest::hash_blake3(b"reflex.retriever.sparse-v1")),
            CuratorSpec::from_digest(Digest::hash_blake3(b"reflex.curator.baselines-v1")),
            curation_policy,
        )
        .await
    }

    pub async fn create_edition_with_identity(
        &self,
        domain_id: &str,
        compatibility: CompatibilityDigest,
        parent: Option<KnowledgeEditionId>,
        retriever: RetrieverSpec,
        curator: CuratorSpec,
        curation_policy: Digest,
    ) -> Result<KnowledgeEdition, KnowledgeError> {
        let domain_id = domain_id.trim();
        if domain_id.trim().is_empty()
            || *retriever.digest() == Digest::ZERO
            || *curator.digest() == Digest::ZERO
            || curation_policy == Digest::ZERO
        {
            return Err(KnowledgeError::Curation(
                "edition identities must be non-empty and non-zero".to_owned(),
            ));
        }
        if let Some(parent_id) = parent {
            let editions = self.editions.read().await;
            let parent_edition = editions
                .get(&parent_id)
                .ok_or(KnowledgeError::EditionNotFound(parent_id))?;
            if parent_edition.canonical_id()? != parent_id
                || parent_edition.domain_id != domain_id
                || parent_edition.compatibility != compatibility
            {
                return Err(KnowledgeError::CompatibilityMismatch);
            }
        }
        let records_map = self.records.read().await;
        let tiers = self.record_tiers.read().await;
        let mut records: Vec<KnowledgeRecordId> = records_map
            .values()
            .filter(|record| tiers.get(&record.id) != Some(&StorageTier::Archive))
            .map(|r| r.id)
            .collect();
        records.sort();
        records.dedup();

        let mut edition = KnowledgeEdition {
            id: KnowledgeEditionId::from_digest(Digest::ZERO),
            domain_id: domain_id.to_string(),
            compatibility,
            records,
            parent_edition: parent,
            retriever,
            curator,
            curation_policy,
        };
        edition.id = edition.canonical_id()?;
        let id = edition.id;
        self.editions.write().await.insert(id, edition.clone());
        Ok(edition)
    }

    pub async fn load_edition(
        &self,
        edition_id: KnowledgeEditionId,
        expected: CompatibilityDigest,
    ) -> Result<KnowledgeEdition, KnowledgeError> {
        let edition = self
            .editions
            .read()
            .await
            .get(&edition_id)
            .cloned()
            .ok_or(KnowledgeError::EditionNotFound(edition_id))?;
        if edition.compatibility != expected {
            return Err(KnowledgeError::CompatibilityMismatch);
        }
        if edition.canonical_id()? != edition.id {
            return Err(KnowledgeError::UnregisteredEdition);
        }
        Ok(edition)
    }

    pub async fn activate_overlay(
        &self,
        record_ids: Vec<KnowledgeRecordId>,
        parent: KnowledgeEditionId,
    ) -> Result<OverlaySnapshot, KnowledgeError> {
        let editions = self.editions.read().await;
        let parent_manifest = editions
            .get(&parent)
            .ok_or(KnowledgeError::EditionNotFound(parent))?;
        if parent_manifest.canonical_id()? != parent {
            return Err(KnowledgeError::UnregisteredEdition);
        }
        drop(editions);
        let mut sorted = record_ids;
        sorted.sort();
        sorted.dedup();
        let records = self.records.read().await;
        for id in &sorted {
            let record = records.get(id).ok_or(KnowledgeError::RecordNotFound(*id))?;
            record.validate()?;
            if record.canonical_id()? != *id {
                return Err(KnowledgeError::IdentityMismatch {
                    expected: record.canonical_id()?.to_hex(),
                    supplied: id.to_hex(),
                });
            }
        }
        drop(records);
        let mut snap = OverlaySnapshot {
            digest: Digest::ZERO,
            record_ids: sorted,
            parent_edition: parent,
        };
        snap.digest = snap.canonical_digest();
        self.overlay_snapshots
            .write()
            .await
            .insert(snap.digest, snap.clone());
        Ok(snap)
    }

    /// Resolve an immutable overlay by its registered content identity.
    pub async fn pin_overlay(
        &self,
        digest: Digest,
        expected_parent: KnowledgeEditionId,
    ) -> Result<OverlaySnapshot, KnowledgeError> {
        let snapshot = self
            .overlay_snapshots
            .read()
            .await
            .get(&digest)
            .cloned()
            .ok_or(KnowledgeError::OverlayNotFound(digest))?;
        if snapshot.parent_edition != expected_parent {
            return Err(KnowledgeError::OverlayParentMismatch);
        }
        if snapshot.canonical_digest() != digest {
            return Err(KnowledgeError::IdentityMismatch {
                expected: snapshot.canonical_digest().to_hex(),
                supplied: digest.to_hex(),
            });
        }
        Ok(snapshot)
    }

    pub async fn compact_overlay(
        &self,
        domain_id: &str,
        compatibility: CompatibilityDigest,
    ) -> Result<KnowledgeEdition, KnowledgeError> {
        let snapshots = self.overlay_snapshots.read().await;
        let parent = snapshots
            .values()
            .map(|snapshot| snapshot.parent_edition)
            .next();
        if snapshots
            .values()
            .any(|snapshot| Some(snapshot.parent_edition) != parent)
        {
            return Err(KnowledgeError::OverlayParentMismatch);
        }
        drop(snapshots);
        let edition = self
            .create_edition(domain_id, compatibility, parent)
            .await?;
        self.overlay_snapshots.write().await.clear();
        Ok(edition)
    }

    pub async fn record_knowledge_use(
        &self,
        use_record: KnowledgeUseRecord,
    ) -> Result<(), KnowledgeError> {
        if !self
            .records
            .read()
            .await
            .contains_key(&use_record.record_id)
        {
            return Err(KnowledgeError::RecordNotFound(use_record.record_id));
        }
        if use_record
            .retrieval_score
            .is_some_and(|score| !score.is_finite())
        {
            return Err(KnowledgeError::NonFiniteUtility);
        }
        let requires_proof_receipt = matches!(
            use_record.level,
            KnowledgeUseLevel::PresentInFinalProof
                | KnowledgeUseLevel::NecessaryLeaveOneOut
                | KnowledgeUseLevel::ReducedWorkMatchedRerun
        );
        if requires_proof_receipt
            && use_record
                .proof_receipt
                .is_none_or(|receipt| receipt == Digest::ZERO)
        {
            return Err(KnowledgeError::VerificationRequired);
        }
        let mut ledgers = self.ledgers.write().await;
        let ledger = ledgers.entry(use_record.record_id).or_default();
        if use_record.level == KnowledgeUseLevel::Retrieved {
            ledger.query_count = ledger
                .query_count
                .checked_add(1)
                .ok_or(KnowledgeError::ArithmeticOverflow)?;
            return Ok(());
        }
        ledger.has_observed_utility = true;
        if requires_proof_receipt {
            ledger.proof_use_count = ledger
                .proof_use_count
                .checked_add(1)
                .ok_or(KnowledgeError::ArithmeticOverflow)?;
        }
        ledger.marginal_work_saved = ledger
            .marginal_work_saved
            .checked_add(use_record.actions_saved)
            .ok_or(KnowledgeError::ArithmeticOverflow)?;
        ledger.cpu_saved_ns = ledger
            .cpu_saved_ns
            .checked_add(use_record.cpu_saved_ns)
            .ok_or(KnowledgeError::ArithmeticOverflow)?;
        Ok(())
    }

    pub async fn curate_library(&self, policy: &CurationPolicy) -> Result<Digest, KnowledgeError> {
        let policy_id = policy.identity()?;
        let records = self.records.read().await;
        let ledgers = self.ledgers.read().await;
        let current_tiers = self.record_tiers.read().await;
        let mut scored: Vec<(KnowledgeRecordId, Option<f64>, StorageTier)> = records
            .values()
            .map(|r| {
                let ledger = ledgers.get(&r.id);
                let score = match policy.baseline {
                    CurationBaseline::NoCuration => Some(0.0),
                    CurationBaseline::HumanSeeded => Some(r.utility_score),
                    CurationBaseline::Frequency => ledger
                        .filter(|ledger| ledger.has_observed_utility || ledger.query_count > 0)
                        .map(|ledger| ledger.query_count as f64),
                    CurationBaseline::Compression => ledger
                        .filter(|ledger| ledger.has_observed_utility)
                        .map(|ledger| ledger.compression_contribution),
                    CurationBaseline::MarginalUtility => ledger
                        .filter(|ledger| ledger.has_observed_utility)
                        .map(|ledger| {
                            ledger.marginal_work_saved as f64
                                + ledger.cpu_saved_ns as f64
                                + ledger.descendant_value
                                - ledger.discovery_cost
                                - ledger.retrieval_tax_ns as f64
                        }),
                };
                (
                    r.id,
                    score,
                    current_tiers
                        .get(&r.id)
                        .copied()
                        .unwrap_or(StorageTier::Archive),
                )
            })
            .collect();
        scored.sort_by(|a, b| match (a.1, b.1) {
            (Some(left), Some(right)) => right.total_cmp(&left).then_with(|| a.0.cmp(&b.0)),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.0.cmp(&b.0),
        });
        drop(current_tiers);

        let mut tiers = self.record_tiers.write().await;
        let mut ranked = 0usize;
        let mut receipt_bytes = policy_id.bytes.to_vec();
        for (id, score, previous_tier) in scored {
            let new_tier = if policy.baseline == CurationBaseline::NoCuration {
                StorageTier::Hot
            } else if let Some(score) = score {
                let position = ranked;
                ranked = ranked
                    .checked_add(1)
                    .ok_or(KnowledgeError::ArithmeticOverflow)?;
                if score < policy.min_utility_threshold {
                    StorageTier::Archive
                } else if position < policy.max_hot_records {
                    StorageTier::Hot
                } else if position < policy.max_hot_records + policy.max_warm_records {
                    StorageTier::Warm
                } else {
                    StorageTier::Archive
                }
            } else {
                // No observation means censored, not worthless: preserve the prior tier.
                previous_tier
            };
            tiers.insert(id, new_tier);
            receipt_bytes.extend_from_slice(&id.digest().bytes);
            receipt_bytes.push(match previous_tier {
                StorageTier::Hot => 0,
                StorageTier::Warm => 1,
                StorageTier::Archive => 2,
            });
            receipt_bytes.push(match new_tier {
                StorageTier::Hot => 0,
                StorageTier::Warm => 1,
                StorageTier::Archive => 2,
            });
        }
        *self.current_curation_policy.write().await = policy_id;
        Ok(Digest::hash_blake3(&receipt_bytes))
    }

    /// Add a record without inventing verification provenance.
    pub async fn add_record(&self, record: KnowledgeRecord) -> Result<(), KnowledgeError> {
        self.add_verified_record(record).await
    }

    pub async fn retrieve_by_tag(
        &self,
        tag: &str,
        limit: usize,
    ) -> Result<Vec<KnowledgeRecord>, KnowledgeError> {
        let edition = self
            .create_edition(
                "default",
                CompatibilityDigest::from_digest(Digest::ZERO),
                None,
            )
            .await?;
        let query = RetrievalQuery {
            tag: Some(tag.to_string()),
            canonical_key: None,
            structural_features: vec![],
            include_archived: false,
            top_k: limit,
        };
        let (hits, _) = self.retrieve_async(&edition, &query).await?;
        let records = self.records.read().await;
        Ok(hits
            .iter()
            .filter_map(|hit| records.get(&hit.record_id).cloned())
            .collect())
    }
}

#[async_trait::async_trait]
impl Retriever for KnowledgeStore {
    async fn retrieve(
        &self,
        edition: &KnowledgeEdition,
        query: &RetrievalQuery,
    ) -> Result<(Vec<RetrievalHit>, RetrievalWork), KnowledgeError> {
        self.retrieve_async(edition, query).await
    }
}

impl KnowledgeStore {
    pub async fn retrieve_async(
        &self,
        edition: &KnowledgeEdition,
        query: &RetrievalQuery,
    ) -> Result<(Vec<RetrievalHit>, RetrievalWork), KnowledgeError> {
        if query.top_k > MAX_RETRIEVAL_TOP_K {
            return Err(KnowledgeError::QueryLimitExceeded {
                requested: query.top_k,
                maximum: MAX_RETRIEVAL_TOP_K,
            });
        }
        self.validate_pinned_edition(edition).await?;
        let records = self.records.read().await;
        let exact = self.exact_index.read().await;
        let inverted = self.inverted_index.read().await;
        let mut work = RetrievalWork::default();
        let start = std::time::Instant::now();
        let mut hits = BTreeMap::new();
        let edition_contains = |id: &KnowledgeRecordId| edition.records.binary_search(id).is_ok();

        if let Some(key) = &query.canonical_key
            && let Some(posting) = exact.get(key)
        {
            work.truncated |= posting.omitted > 0;
            for id in &posting.ids {
                if work.records_examined as usize >= MAX_POSTINGS_PER_QUERY {
                    work.truncated = true;
                    break;
                }
                work.postings_scanned = work.postings_scanned.saturating_add(1);
                work.records_examined = work.records_examined.saturating_add(1);
                if edition_contains(id) && records.contains_key(id) {
                    hits.entry(*id).or_insert_with(|| RetrievalHit {
                        record_id: *id,
                        score: 1.0,
                        reason: "exact_canonical".into(),
                    });
                }
            }
        }

        if let Some(tag) = &query.tag
            && let Some(posting) = inverted.get(tag)
        {
            work.truncated |= posting.omitted > 0;
            for id in &posting.ids {
                if work.records_examined as usize >= MAX_POSTINGS_PER_QUERY {
                    work.truncated = true;
                    break;
                }
                work.postings_scanned = work.postings_scanned.saturating_add(1);
                work.records_examined = work.records_examined.saturating_add(1);
                if !edition_contains(id) {
                    continue;
                }
                let Some(rec) = records.get(id) else { continue };
                hits.entry(*id).or_insert_with(|| RetrievalHit {
                    record_id: *id,
                    score: rec.utility_score,
                    reason: format!("tag:{tag}"),
                });
            }
        }

        let mut hits: Vec<_> = hits.into_values().collect();
        hits.sort_by(|a, b| {
            b.score
                .total_cmp(&a.score)
                .then_with(|| a.record_id.to_hex().cmp(&b.record_id.to_hex()))
        });
        hits.truncate(query.top_k);
        work.cpu_ns = u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX);
        drop(inverted);
        drop(exact);
        drop(records);

        if !hits.is_empty() {
            let per_hit_tax = work.cpu_ns / hits.len() as u64;
            let mut ledgers = self.ledgers.write().await;
            for hit in &hits {
                let ledger = ledgers.entry(hit.record_id).or_default();
                ledger.query_count = ledger
                    .query_count
                    .checked_add(1)
                    .ok_or(KnowledgeError::ArithmeticOverflow)?;
                ledger.retrieval_tax_ns = ledger
                    .retrieval_tax_ns
                    .checked_add(per_hit_tax)
                    .ok_or(KnowledgeError::ArithmeticOverflow)?;
            }
        }
        Ok((hits, work))
    }

    async fn validate_pinned_edition(
        &self,
        edition: &KnowledgeEdition,
    ) -> Result<(), KnowledgeError> {
        if edition.canonical_id()? != edition.id {
            return Err(KnowledgeError::UnregisteredEdition);
        }
        let editions = self.editions.read().await;
        match editions.get(&edition.id) {
            Some(registered) if registered == edition => Ok(()),
            _ => Err(KnowledgeError::UnregisteredEdition),
        }
    }
}

/// Back-compat alias used across the workspace.
pub type KnowledgeBase = KnowledgeStore;

impl CanonicalEncode for KnowledgeEdition {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        self.encode_identity(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_record(id_seed: &[u8], tag: &str) -> KnowledgeRecord {
        let mut record = KnowledgeRecord {
            id: KnowledgeRecordId::from_digest(Digest::ZERO),
            class: KnowledgeClass::TheoremFact,
            statement: format!("stmt-{}", tag),
            proof_or_cert_digest: Digest::hash_blake3(id_seed),
            structural_tags: vec![tag.to_string(), "logic".to_string()],
            canonical_key: Some(format!("key:{}", tag)),
            discovery_generation: 1,
            utility_score: 10.0,
            verification_receipt: Digest::hash_blake3(b"receipt"),
            archived: false,
        };
        record.normalize();
        record.id = record.canonical_id().unwrap();
        record
    }

    #[test]
    fn knowledge_store_rejects_unusable_storage_root() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let error = match KnowledgeStore::with_cas_root(file.path().to_path_buf()) {
            Ok(_) => panic!("a regular file cannot be a knowledge CAS root"),
            Err(error) => error,
        };
        assert!(matches!(error, KnowledgeError::Storage(_)));
    }

    #[tokio::test]
    async fn test_knowledge_edition_creation_and_retrieval() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        let rec = sample_record(b"rec1", "and_comm");
        kb.add_verified_record(rec).await.unwrap();

        let edition = kb
            .create_edition(
                "bitvec",
                CompatibilityDigest::from_digest(Digest::ZERO),
                None,
            )
            .await
            .unwrap();
        assert_eq!(edition.records.len(), 1);

        let query = RetrievalQuery {
            tag: Some("logic".into()),
            canonical_key: None,
            structural_features: vec![],
            include_archived: false,
            top_k: 5,
        };
        let (hits, work) = kb.retrieve_async(&edition, &query).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(work.cpu_ns > 0);
    }

    #[tokio::test]
    async fn test_edit_creates_new_edition() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        let rec = sample_record(b"rec1", "comm");
        kb.add_verified_record(rec.clone()).await.unwrap();
        let e1 = kb
            .create_edition(
                "bitvec",
                CompatibilityDigest::from_digest(Digest::ZERO),
                None,
            )
            .await
            .unwrap();

        let mut updated = rec.clone();
        updated.statement = "revised".into();
        let e2 = kb
            .edit_record(
                rec.id,
                updated,
                "bitvec",
                CompatibilityDigest::from_digest(Digest::ZERO),
            )
            .await
            .unwrap();
        assert_ne!(e1.id, e2.id);
    }

    #[tokio::test]
    async fn test_unverified_record_rejected() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        let mut rec = sample_record(b"bad", "x");
        rec.verification_receipt = Digest::ZERO;
        assert!(matches!(
            kb.add_verified_record(rec).await,
            Err(KnowledgeError::VerificationRequired)
        ));
    }

    #[tokio::test]
    async fn test_zero_proof_and_noncanonical_id_are_rejected() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        let mut no_proof = sample_record(b"no-proof", "x");
        no_proof.proof_or_cert_digest = Digest::ZERO;
        no_proof.id = no_proof.canonical_id().unwrap();
        assert_eq!(
            kb.add_verified_record(no_proof).await,
            Err(KnowledgeError::ProofRequired)
        );

        let mut wrong_id = sample_record(b"wrong-id", "x");
        wrong_id.id = KnowledgeRecordId::from_digest(Digest::hash_blake3(b"caller-id"));
        assert!(matches!(
            kb.add_verified_record(wrong_id).await,
            Err(KnowledgeError::IdentityMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn test_edition_identity_covers_parent_retriever_curator_and_policy() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        kb.add_verified_record(sample_record(b"edition-id", "x"))
            .await
            .unwrap();
        let compatibility = CompatibilityDigest::from_digest(Digest::hash_blake3(b"compat"));
        let base = kb
            .create_edition("bitvec", compatibility, None)
            .await
            .unwrap();
        let child = kb
            .create_edition("bitvec", compatibility, Some(base.id))
            .await
            .unwrap();
        assert_ne!(base.id, child.id);

        let alternate = kb
            .create_edition_with_identity(
                "bitvec",
                compatibility,
                Some(base.id),
                RetrieverSpec::from_digest(Digest::hash_blake3(b"alternate-retriever")),
                CuratorSpec::from_digest(Digest::hash_blake3(b"alternate-curator")),
                Digest::hash_blake3(b"alternate-policy"),
            )
            .await
            .unwrap();
        assert_ne!(child.id, alternate.id);
    }

    #[tokio::test]
    async fn test_overlay_requires_registered_parent_and_records() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        let record = sample_record(b"overlay", "x");
        let record_id = record.id;
        kb.add_verified_record(record).await.unwrap();
        let unknown_parent = KnowledgeEditionId::from_digest(Digest::hash_blake3(b"unknown"));
        assert!(matches!(
            kb.activate_overlay(vec![record_id], unknown_parent).await,
            Err(KnowledgeError::EditionNotFound(_))
        ));
        let base = kb
            .create_edition(
                "bitvec",
                CompatibilityDigest::from_digest(Digest::hash_blake3(b"compat")),
                None,
            )
            .await
            .unwrap();
        let unknown_record = KnowledgeRecordId::from_digest(Digest::hash_blake3(b"unknown"));
        assert!(matches!(
            kb.activate_overlay(vec![unknown_record], base.id).await,
            Err(KnowledgeError::RecordNotFound(_))
        ));
        let snapshot = kb.activate_overlay(vec![record_id], base.id).await.unwrap();
        assert_eq!(
            kb.pin_overlay(snapshot.digest, base.id).await.unwrap(),
            snapshot
        );
    }

    #[tokio::test]
    async fn test_query_limits_fail_closed() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        kb.add_verified_record(sample_record(b"limit", "x"))
            .await
            .unwrap();
        let edition = kb
            .create_edition(
                "bitvec",
                CompatibilityDigest::from_digest(Digest::hash_blake3(b"compat")),
                None,
            )
            .await
            .unwrap();
        let query = RetrievalQuery {
            tag: Some("x".into()),
            canonical_key: None,
            structural_features: vec![],
            include_archived: false,
            top_k: MAX_RETRIEVAL_TOP_K + 1,
        };
        assert!(matches!(
            kb.retrieve_async(&edition, &query).await,
            Err(KnowledgeError::QueryLimitExceeded { .. })
        ));
    }

    #[tokio::test]
    async fn test_final_proof_use_requires_receipt() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        let record = sample_record(b"proof-use", "x");
        let record_id = record.id;
        kb.add_verified_record(record).await.unwrap();
        let use_record = KnowledgeUseRecord {
            record_id,
            level: KnowledgeUseLevel::PresentInFinalProof,
            retrieval_rank: Some(0),
            retrieval_score: Some(1.0),
            actions_saved: 1,
            cpu_saved_ns: 1,
            causal_confidence: ConfidenceLevel::Observational,
            proof_receipt: Some(Digest::ZERO),
        };
        assert_eq!(
            kb.record_knowledge_use(use_record).await,
            Err(KnowledgeError::VerificationRequired)
        );
    }

    #[tokio::test]
    async fn test_curation_baselines_preserve_censored_records_and_publish_policy() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        let observed = sample_record(b"observed", "observed");
        let censored = sample_record(b"censored", "censored");
        let observed_id = observed.id;
        let censored_id = censored.id;
        kb.add_verified_record(observed).await.unwrap();
        kb.add_verified_record(censored).await.unwrap();
        let compatibility = CompatibilityDigest::from_digest(Digest::hash_blake3(b"compat"));
        let base = kb
            .create_edition("bitvec", compatibility, None)
            .await
            .unwrap();
        let query = RetrievalQuery {
            tag: Some("observed".into()),
            canonical_key: None,
            structural_features: vec![],
            include_archived: false,
            top_k: 1,
        };
        kb.retrieve_async(&base, &query).await.unwrap();

        let frequency = CurationPolicy {
            max_hot_records: 0,
            max_warm_records: 0,
            min_utility_threshold: 0.0,
            baseline: CurationBaseline::Frequency,
        };
        kb.curate_library(&frequency).await.unwrap();
        let tiers = kb.record_tiers.read().await;
        assert_eq!(tiers.get(&observed_id), Some(&StorageTier::Archive));
        assert_eq!(tiers.get(&censored_id), Some(&StorageTier::Hot));
        drop(tiers);
        let curated = kb
            .create_edition("bitvec", compatibility, Some(base.id))
            .await
            .unwrap();
        assert_eq!(curated.curation_policy, frequency.identity().unwrap());
        assert!(!curated.records.contains(&observed_id));
        assert!(curated.records.contains(&censored_id));

        let no_curation = CurationPolicy {
            baseline: CurationBaseline::NoCuration,
            ..CurationPolicy::default()
        };
        kb.curate_library(&no_curation).await.unwrap();
        assert!(
            kb.record_tiers
                .read()
                .await
                .values()
                .all(|tier| *tier == StorageTier::Hot)
        );
    }

    #[tokio::test]
    async fn test_add_record_never_fabricates_receipt() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        let mut rec = sample_record(b"unverified", "x");
        rec.verification_receipt = Digest::ZERO;
        assert_eq!(
            kb.add_record(rec).await,
            Err(KnowledgeError::VerificationRequired)
        );
        assert!(kb.records.read().await.is_empty());
    }

    #[tokio::test]
    async fn test_negative_savings_remain_signed() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        let rec = sample_record(b"negative-savings", "x");
        let id = rec.id;
        kb.add_verified_record(rec).await.unwrap();
        kb.record_knowledge_use(KnowledgeUseRecord {
            record_id: id,
            level: KnowledgeUseLevel::ReducedWorkMatchedRerun,
            retrieval_rank: Some(0),
            retrieval_score: Some(1.0),
            actions_saved: -7,
            cpu_saved_ns: -11,
            causal_confidence: ConfidenceLevel::MatchedControl,
            proof_receipt: Some(Digest::hash_blake3(b"matched-proof")),
        })
        .await
        .unwrap();
        let ledgers = kb.ledgers.read().await;
        let ledger = ledgers.get(&id).unwrap();
        assert_eq!(ledger.marginal_work_saved, -7);
        assert_eq!(ledger.cpu_saved_ns, -11);
    }

    #[tokio::test]
    async fn test_pinned_edition_is_unchanged_by_later_archival() {
        let dir = tempdir().unwrap();
        let kb = KnowledgeStore::with_cas_root(dir.path().to_path_buf()).unwrap();
        let rec = sample_record(b"rec1", "hidden");
        kb.add_verified_record(rec.clone()).await.unwrap();
        let edition = kb
            .create_edition(
                "bitvec",
                CompatibilityDigest::from_digest(Digest::ZERO),
                None,
            )
            .await
            .unwrap();
        kb.record_tiers
            .write()
            .await
            .insert(rec.id, StorageTier::Archive);
        let query = RetrievalQuery {
            tag: Some("hidden".into()),
            canonical_key: None,
            structural_features: vec![],
            include_archived: false,
            top_k: 5,
        };
        let (hits, _) = kb.retrieve_async(&edition, &query).await.unwrap();
        assert_eq!(hits.len(), 1);
        let next = kb
            .create_edition(
                "bitvec",
                CompatibilityDigest::from_digest(Digest::ZERO),
                Some(edition.id),
            )
            .await
            .unwrap();
        assert!(kb.retrieve_async(&next, &query).await.unwrap().0.is_empty());
    }
}
