//! Decision-group compilation, Parquet publication (P4.5), and RFXBATCH mmap cache (P4.6).
//!
//! The mmap cache honors a registered shuffle seed for deterministic group orders and supports
//! restart at an exact batch position during training. Corrupt headers, offsets, masks, and CRCs
//! are rejected on load.

//! Decision-group compilation, Parquet publication (P4.5), and RFXBATCH mmap cache (P4.6).
//!
//! The mmap cache honors a registered shuffle seed for deterministic group orders and supports
//! restart at an exact batch position during training. Corrupt headers, offsets, masks, and CRCs
//! are rejected on load.

use crc32c::crc32c;
use reflex_ledger::{Event, RecoveredSegment};
use reflex_types::{
    ActionSchemaId, ArtifactId, BuildIdentity, CandidateId, CellId, DatasetSchemaId, Digest,
    DigestAlgorithm, EpisodeId, FeatureSchemaId, StateId,
};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write as IoWrite};
use std::path::Path;
use thiserror::Error;

mod batch_mmap;
mod parquet;

pub use batch_mmap::{MmapBatchCache, PrefetchingBatchLoader, RfxBatchV2};
pub use parquet::ParquetCompactor;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum DatasetError {
    #[error("io error: {0}")]
    Io(String),
    #[error("invalid RFXBATCH magic: {0:?}")]
    InvalidMagic([u8; 8]),
    #[error("dataset schema mismatch: expected {expected}, found {found}")]
    SchemaMismatch { expected: String, found: String },
    #[error("crc mismatch in RFXBATCH")]
    CrcMismatch,
    #[error("empty dataset or missing supervision")]
    EmptyDataset,
    #[error("compilation error: {0}")]
    Compilation(String),
}

impl From<std::io::Error> for DatasetError {
    fn from(e: std::io::Error) -> Self {
        DatasetError::Io(e.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CandidateKnowledge {
    Viable {
        best_actions_to_go: u32,
        receipts: SmallVec<[Digest; 2]>,
    },
    KnownDead {
        certificate: Digest,
    },
    Unknown,
    Invalid {
        code: u32,
    },
}

impl CandidateKnowledge {
    pub fn validate_evidence(&self) -> Result<(), DatasetError> {
        match self {
            Self::Viable { receipts, .. }
                if receipts.is_empty() || receipts.contains(&Digest::ZERO) =>
            {
                Err(DatasetError::Compilation(
                    "viable label requires non-zero verification receipts".into(),
                ))
            }
            Self::KnownDead { certificate } if *certificate == Digest::ZERO => {
                Err(DatasetError::Compilation(
                    "known-dead label requires a non-zero certificate".into(),
                ))
            }
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DecisionGroup {
    pub state_id: StateId,
    pub candidate_ids: Vec<CandidateId>,
    pub labels: Vec<CandidateKnowledge>,
    pub feature_ref: Digest,
    pub source_episodes: Vec<EpisodeId>,
    pub coverage: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatasetShard {
    pub shard_id: u32,
    pub digest: Digest,
    pub row_count: u64,
    pub group_count: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub schema: DatasetSchemaId,
    pub source_cells: Vec<CellId>,
    pub source_ledgers: Vec<Digest>,
    pub shards: Vec<DatasetShard>,
    pub logical_rows: u64,
    pub decision_groups: u64,
    pub feature_schema: FeatureSchemaId,
    pub action_schema: ActionSchemaId,
    pub split_manifest: Digest,
    pub compiler: BuildIdentity,
}

/// Birth-feature snapshot for taste critics (P15.3): features frozen at artifact birth.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BirthFeatureSnapshot {
    pub artifact_digest: Digest,
    pub birth_generation: u32,
    pub mathematical_features: Vec<f32>,
    pub sociology_features: Vec<f32>,
    pub module_context_digest: Digest,
    pub vocabulary_eligible: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObservationHorizon {
    Immediate,
    HorizonHours(u32),
    Descendant,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HorizonLabel {
    pub horizon: ObservationHorizon,
    pub utility_digest: Digest,
    pub censored: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TasteDatasetRow {
    pub snapshot: BirthFeatureSnapshot,
    pub labels: Vec<HorizonLabel>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemporalSplitManifest {
    pub train_end_generation: u32,
    pub val_end_generation: u32,
    pub test_end_generation: u32,
    pub censoring_rule: String,
}

pub struct TasteDatasetCompiler {
    rows: HashMap<Digest, TasteDatasetRow>,
    #[allow(dead_code)] // consumed by temporal split export (P4.x)
    split: TemporalSplitManifest,
}

impl TasteDatasetCompiler {
    pub fn new(split: TemporalSplitManifest) -> Self {
        Self {
            rows: HashMap::new(),
            split,
        }
    }

    pub fn record_birth(&mut self, snapshot: BirthFeatureSnapshot) {
        self.rows.insert(
            snapshot.artifact_digest,
            TasteDatasetRow {
                snapshot,
                labels: Vec::new(),
            },
        );
    }

    pub fn attach_horizon_label(
        &mut self,
        artifact: Digest,
        label: HorizonLabel,
        observed_generation: u32,
    ) -> Result<(), DatasetError> {
        let row = self
            .rows
            .get_mut(&artifact)
            .ok_or_else(|| DatasetError::Compilation("unknown artifact".into()))?;
        if observed_generation < row.snapshot.birth_generation {
            return Err(DatasetError::Compilation(
                "future leak: utility observed before birth".into(),
            ));
        }
        row.labels.push(label);
        Ok(())
    }

    pub fn finalize(&self) -> Vec<TasteDatasetRow> {
        self.rows.values().cloned().collect()
    }
}

pub struct DatasetCompiler {
    groups: BTreeMap<StateId, DecisionGroup>,
    viable_routes: HashMap<(StateId, CandidateId), ViableRouteEvidence>,
    dead_certificates: HashMap<(StateId, CandidateId), Digest>,
    counters: CompilationCounters,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompilationCounters {
    pub source_segments: u64,
    pub source_events: u64,
    pub candidate_batches: u64,
    pub candidate_observations: u64,
    pub accepted_receipts: u64,
    pub decision_groups: u64,
    pub logical_rows: u64,
    pub viable_rows: u64,
    pub known_dead_rows: u64,
    pub unknown_rows: u64,
    pub invalid_rows: u64,
}

#[derive(Clone, Debug)]
struct ViableRouteEvidence {
    best_actions_to_go: u32,
    receipts: BTreeSet<Digest>,
}

impl DatasetCompiler {
    pub fn new() -> Self {
        Self {
            groups: BTreeMap::new(),
            viable_routes: HashMap::new(),
            dead_certificates: HashMap::new(),
            counters: CompilationCounters::default(),
        }
    }

    pub fn record_verified_route(
        &mut self,
        state_id: StateId,
        candidate_id: CandidateId,
        actions_to_go: u32,
        receipt: Digest,
    ) -> Result<(), DatasetError> {
        if receipt == Digest::ZERO {
            return Err(DatasetError::Compilation(
                "verified route requires a non-zero receipt digest".into(),
            ));
        }
        let entry = self
            .viable_routes
            .entry((state_id, candidate_id))
            .or_insert_with(|| ViableRouteEvidence {
                best_actions_to_go: actions_to_go,
                receipts: BTreeSet::new(),
            });
        entry.best_actions_to_go = entry.best_actions_to_go.min(actions_to_go);
        entry.receipts.insert(receipt);
        Ok(())
    }

    pub fn record_known_dead(
        &mut self,
        state_id: StateId,
        candidate_id: CandidateId,
        certificate: Digest,
    ) -> Result<(), DatasetError> {
        if certificate == Digest::ZERO {
            return Err(DatasetError::Compilation(
                "known-dead label requires a non-zero certificate digest".into(),
            ));
        }
        self.dead_certificates
            .insert((state_id, candidate_id), certificate);
        Ok(())
    }

    pub fn record_state_candidates(
        &mut self,
        state_id: StateId,
        candidate_ids: Vec<CandidateId>,
        coverage: &str,
    ) {
        let entry = self
            .groups
            .entry(state_id)
            .or_insert_with(|| DecisionGroup {
                state_id,
                candidate_ids: candidate_ids.clone(),
                labels: vec![CandidateKnowledge::Unknown; candidate_ids.len()],
                feature_ref: Digest::hash_blake3(&state_id.digest().bytes),
                source_episodes: Vec::new(),
                coverage: coverage.to_string(),
            });
        for c in candidate_ids {
            if !entry.candidate_ids.contains(&c) {
                entry.candidate_ids.push(c);
                entry.labels.push(CandidateKnowledge::Unknown);
            }
        }
    }

    pub fn add_group(&mut self, group: DecisionGroup) {
        self.groups.insert(group.state_id, group);
    }

    pub fn counters(&self) -> &CompilationCounters {
        &self.counters
    }

    pub fn ingest_segment(
        &mut self,
        segment: &RecoveredSegment,
        segment_path: &Path,
    ) -> Result<(), DatasetError> {
        let events = segment
            .read_events(segment_path)
            .map_err(|error| DatasetError::Compilation(error.to_string()))?;

        if events.len() as u64 != segment.report.recovered_events {
            return Err(DatasetError::Compilation(format!(
                "segment manifest reports {} events but decoder produced {}",
                segment.report.recovered_events,
                events.len()
            )));
        }

        let mut current_episode = None;
        let mut state_depths = HashMap::<(EpisodeId, StateId), u32>::new();
        let mut pending_closed = HashMap::<EpisodeId, (StateId, CandidateId)>::new();
        let mut artifact_routes = HashMap::<ArtifactId, (EpisodeId, StateId, CandidateId)>::new();
        let mut accepted_receipts = HashMap::<ArtifactId, Digest>::new();
        let mut episode_actions = HashMap::<EpisodeId, u32>::new();

        for seq_event in &events {
            match &seq_event.event {
                Event::EpisodeStart(ep) => {
                    current_episode = Some(ep.episode_id);
                }
                Event::StateDiscovery(state) => {
                    state_depths.insert((state.episode_id, state.state_id), state.depth);
                }
                Event::CandidateBatch(cb) => {
                    let ep = current_episode.ok_or_else(|| {
                        DatasetError::Compilation(
                            "CandidateBatch appeared before EpisodeStart".into(),
                        )
                    })?;
                    if cb.candidate_ids.len() != cb.classes.len()
                        || cb.candidate_ids.len() != cb.tie_breaks.len()
                    {
                        return Err(DatasetError::Compilation(
                            "CandidateBatch column lengths disagree".into(),
                        ));
                    }
                    self.record_state_candidates(cb.state_id, cb.candidate_ids.clone(), "observed");
                    let entry = self.groups.get_mut(&cb.state_id).expect("group inserted");
                    if !entry.source_episodes.contains(&ep) {
                        entry.source_episodes.push(ep);
                    }
                    self.counters.candidate_batches += 1;
                    self.counters.candidate_observations += cb.candidate_ids.len() as u64;
                }
                Event::FeatureRef(feature) => {
                    if feature.feature_schema.digest() == &Digest::ZERO
                        || feature.payload_handle.is_empty()
                    {
                        return Err(DatasetError::Compilation(
                            "feature reference is missing schema identity or payload handle".into(),
                        ));
                    }
                    let group = self.groups.get_mut(&feature.state_id).ok_or_else(|| {
                        DatasetError::Compilation(
                            "FeatureRef references a state without a CandidateBatch".into(),
                        )
                    })?;
                    group.feature_ref = Digest::hash_blake3(&feature.payload_handle);
                }
                Event::ArtifactConstruction(ac) => {
                    if let Some(route) = pending_closed.remove(&ac.episode_id) {
                        artifact_routes.insert(ac.artifact_id, (ac.episode_id, route.0, route.1));
                    }
                }
                Event::CandidateApplication(ca) if ca.outcome == "Closed" => {
                    let episode = current_episode.ok_or_else(|| {
                        DatasetError::Compilation(
                            "closed CandidateApplication appeared before EpisodeStart".into(),
                        )
                    })?;
                    pending_closed.insert(episode, (ca.state_id, ca.candidate_id));
                }
                Event::VerificationReceipt(receipt) if receipt.status == "accepted" => {
                    if receipt.verifier.digest() == &Digest::ZERO {
                        return Err(DatasetError::Compilation(
                            "accepted verification names a zero verifier".into(),
                        ));
                    }
                    if accepted_receipts.contains_key(&receipt.artifact_id) {
                        return Err(DatasetError::Compilation(
                            "duplicate accepted receipt for one artifact".into(),
                        ));
                    }
                    let encoded = serde_json::to_vec(receipt)
                        .map_err(|error| DatasetError::Compilation(error.to_string()))?;
                    accepted_receipts.insert(receipt.artifact_id, Digest::hash_blake3(&encoded));
                }
                Event::EpisodeEnd(end) => {
                    episode_actions.insert(end.episode_id, end.actions_taken);
                    if current_episode == Some(end.episode_id) {
                        current_episode = None;
                    }
                }
                _ => {}
            }
        }

        for (artifact, receipt) in accepted_receipts {
            let (episode, state_id, candidate_id) =
                artifact_routes.remove(&artifact).ok_or_else(|| {
                    DatasetError::Compilation(
                        "accepted receipt references an artifact without a closed candidate route"
                            .into(),
                    )
                })?;
            let depth = state_depths.get(&(episode, state_id)).ok_or_else(|| {
                DatasetError::Compilation(
                    "verified route is missing its state-depth evidence".into(),
                )
            })?;
            let actions_taken = episode_actions.get(&episode).ok_or_else(|| {
                DatasetError::Compilation(
                    "verified route is missing its EpisodeEnd action count".into(),
                )
            })?;
            let actions_to_go = actions_taken.checked_sub(*depth).ok_or_else(|| {
                DatasetError::Compilation(
                    "episode action count precedes the verified state depth".into(),
                )
            })?;
            if actions_to_go == 0 {
                return Err(DatasetError::Compilation(
                    "closed candidate route has zero actions-to-go".into(),
                ));
            }
            self.record_verified_route(state_id, candidate_id, actions_to_go, receipt)?;
            self.counters.accepted_receipts += 1;
        }
        self.counters.source_segments += 1;
        self.counters.source_events += events.len() as u64;
        Ok(())
    }

    pub fn finalize_labels(&mut self) -> Vec<DecisionGroup> {
        for group in self.groups.values_mut() {
            for (idx, cand_id) in group.candidate_ids.iter().enumerate() {
                if let Some(route) = self.viable_routes.get(&(group.state_id, *cand_id)) {
                    let receipts = route.receipts.iter().copied().collect();
                    group.labels[idx] = CandidateKnowledge::Viable {
                        best_actions_to_go: route.best_actions_to_go,
                        receipts,
                    };
                } else if let Some(cert) = self.dead_certificates.get(&(group.state_id, *cand_id)) {
                    group.labels[idx] = CandidateKnowledge::KnownDead { certificate: *cert };
                } else {
                    group.labels[idx] = CandidateKnowledge::Unknown;
                }
            }
        }
        let groups: Vec<_> = self.groups.values().cloned().collect();
        self.counters.decision_groups = groups.len() as u64;
        self.counters.logical_rows = 0;
        self.counters.viable_rows = 0;
        self.counters.known_dead_rows = 0;
        self.counters.unknown_rows = 0;
        self.counters.invalid_rows = 0;
        for group in &groups {
            self.counters.logical_rows += group.labels.len() as u64;
            for label in &group.labels {
                match label {
                    CandidateKnowledge::Viable { .. } => self.counters.viable_rows += 1,
                    CandidateKnowledge::KnownDead { .. } => self.counters.known_dead_rows += 1,
                    CandidateKnowledge::Unknown => self.counters.unknown_rows += 1,
                    CandidateKnowledge::Invalid { .. } => self.counters.invalid_rows += 1,
                }
            }
        }
        groups
    }
}

impl Default for DatasetCompiler {
    fn default() -> Self {
        Self::new()
    }
}

pub const RFXBATCH_MAGIC: &[u8; 8] = b"RFXBATCH";
pub const RFXBATCH_VERSION: u8 = 2;
pub const RFXBATCH_HEADER_BYTES: usize = 66;
pub const RFXBATCH_MAX_DECODE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct RfxBatch {
    pub source_dataset_digest: Digest,
    pub feature_dim: usize,
    pub total_groups: usize,
    pub total_candidates: usize,
    pub group_offsets: Vec<u32>,
    pub features: Vec<f32>,
    pub labels: Vec<CandidateKnowledge>,
    pub cost_to_go: Vec<f32>,
    pub weights: Vec<f32>,
}

impl RfxBatch {
    pub fn validate(&self) -> Result<(), DatasetError> {
        if self.source_dataset_digest == Digest::ZERO {
            return Err(DatasetError::Compilation(
                "RFXBATCH source dataset identity must be non-zero".into(),
            ));
        }
        let expected_features = self
            .total_candidates
            .checked_mul(self.feature_dim)
            .ok_or_else(|| DatasetError::Compilation("feature shape overflow".into()))?;
        if self.group_offsets.len() != self.total_groups.saturating_add(1)
            || self.group_offsets.first().copied() != Some(0)
            || self
                .group_offsets
                .last()
                .copied()
                .map(|value| value as usize)
                != Some(self.total_candidates)
            || self.group_offsets.windows(2).any(|pair| pair[0] > pair[1])
        {
            return Err(DatasetError::Compilation(
                "invalid decision-group offset geometry".into(),
            ));
        }
        if self.features.len() != expected_features
            || self.labels.len() != self.total_candidates
            || self.cost_to_go.len() != self.total_candidates
            || self.weights.len() != self.total_candidates
        {
            return Err(DatasetError::Compilation(
                "RFXBATCH column lengths disagree with header shape".into(),
            ));
        }
        if self
            .features
            .iter()
            .chain(&self.cost_to_go)
            .chain(&self.weights)
            .any(|value| !value.is_finite())
        {
            return Err(DatasetError::Compilation(
                "RFXBATCH numeric columns contain non-finite values".into(),
            ));
        }
        for label in &self.labels {
            label.validate_evidence()?;
        }
        u32::try_from(self.feature_dim)
            .and_then(|_| u32::try_from(self.total_groups))
            .and_then(|_| u32::try_from(self.total_candidates))
            .map_err(|_| DatasetError::Compilation("RFXBATCH header exceeds u32 limits".into()))?;
        Ok(())
    }

    pub fn write_to_file(&self, path: &Path) -> Result<(), DatasetError> {
        self.validate()?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;

        let mut header = Vec::new();
        header.extend_from_slice(RFXBATCH_MAGIC);
        header.push(RFXBATCH_VERSION);
        header.push(match self.source_dataset_digest.algorithm {
            DigestAlgorithm::Blake3 => 0,
            DigestAlgorithm::Sha256 => 1,
        });
        header.extend_from_slice(&self.source_dataset_digest.bytes);
        header.extend_from_slice(
            &u32::try_from(self.feature_dim)
                .map_err(|_| DatasetError::Compilation("feature_dim exceeds u32".into()))?
                .to_le_bytes(),
        );
        header.extend_from_slice(
            &u32::try_from(self.total_groups)
                .map_err(|_| DatasetError::Compilation("total_groups exceeds u32".into()))?
                .to_le_bytes(),
        );
        header.extend_from_slice(
            &u32::try_from(self.total_candidates)
                .map_err(|_| DatasetError::Compilation("total_candidates exceeds u32".into()))?
                .to_le_bytes(),
        );

        let mut payload = Vec::new();
        for off in &self.group_offsets {
            payload.extend_from_slice(&off.to_le_bytes());
        }
        for f in &self.features {
            payload.extend_from_slice(&f.to_le_bytes());
        }
        for c in &self.cost_to_go {
            payload.extend_from_slice(&c.to_le_bytes());
        }
        for w in &self.weights {
            payload.extend_from_slice(&w.to_le_bytes());
        }
        let labels_json = serde_json::to_vec(&self.labels)
            .map_err(|e| DatasetError::Compilation(e.to_string()))?;
        payload.extend_from_slice(
            &u32::try_from(labels_json.len())
                .map_err(|_| DatasetError::Compilation("label column exceeds u32".into()))?
                .to_le_bytes(),
        );
        payload.extend_from_slice(&labels_json);

        let crc = crc32c(&payload);
        header.extend_from_slice(
            &u64::try_from(payload.len())
                .map_err(|_| DatasetError::Compilation("payload length exceeds u64".into()))?
                .to_le_bytes(),
        );
        header.extend_from_slice(&crc.to_le_bytes());

        file.write_all(&header)?;
        file.write_all(&payload)?;
        file.flush()?;
        Ok(())
    }

    pub fn read_from_file(path: &Path) -> Result<Self, DatasetError> {
        Self::read_from_reader_with_limit(File::open(path)?, RFXBATCH_MAX_DECODE_BYTES)
    }

    fn read_from_reader(mut reader: impl Read) -> Result<Self, DatasetError> {
        Self::read_from_reader_with_limit(&mut reader, RFXBATCH_MAX_DECODE_BYTES)
    }

    fn read_from_reader_with_limit(
        mut reader: impl Read,
        max_payload_bytes: usize,
    ) -> Result<Self, DatasetError> {
        if max_payload_bytes == 0 {
            return Err(DatasetError::Compilation(
                "RFXBATCH decode limit must be non-zero".into(),
            ));
        }
        let mut header_buf = [0u8; RFXBATCH_HEADER_BYTES];
        reader.read_exact(&mut header_buf)?;

        if &header_buf[0..8] != RFXBATCH_MAGIC {
            let mut m = [0u8; 8];
            m.copy_from_slice(&header_buf[0..8]);
            return Err(DatasetError::InvalidMagic(m));
        }

        if header_buf[8] != RFXBATCH_VERSION {
            return Err(DatasetError::SchemaMismatch {
                expected: format!("RFXBATCH v{RFXBATCH_VERSION}"),
                found: format!("RFXBATCH v{}", header_buf[8]),
            });
        }
        let algorithm = match header_buf[9] {
            0 => DigestAlgorithm::Blake3,
            1 => DigestAlgorithm::Sha256,
            value => {
                return Err(DatasetError::Compilation(format!(
                    "unknown source digest algorithm tag {value}"
                )));
            }
        };
        let mut digest_bytes = [0u8; 32];
        digest_bytes.copy_from_slice(&header_buf[10..42]);
        let source_dataset_digest = Digest {
            algorithm,
            bytes: digest_bytes,
        };

        let feature_dim = u32::from_le_bytes(header_buf[42..46].try_into().unwrap()) as usize;
        let total_groups = u32::from_le_bytes(header_buf[46..50].try_into().unwrap()) as usize;
        let total_candidates = u32::from_le_bytes(header_buf[50..54].try_into().unwrap()) as usize;
        let payload_len =
            usize::try_from(u64::from_le_bytes(header_buf[54..62].try_into().unwrap()))
                .map_err(|_| DatasetError::Compilation("payload length overflows usize".into()))?;
        if payload_len > max_payload_bytes {
            return Err(DatasetError::Compilation(format!(
                "RFXBATCH payload length {payload_len} exceeds decode limit {max_payload_bytes}; use the bounded file-backed batch cache for large datasets"
            )));
        }
        let expected_crc = u32::from_le_bytes(header_buf[62..66].try_into().unwrap());

        let mut payload = vec![0u8; payload_len];
        reader.read_exact(&mut payload)?;

        if crc32c(&payload) != expected_crc {
            return Err(DatasetError::CrcMismatch);
        }

        let required_bytes = total_groups
            .checked_add(1)
            .and_then(|value| value.checked_mul(4))
            .and_then(|value| {
                total_candidates
                    .checked_mul(feature_dim)
                    .and_then(|count| count.checked_mul(4))
                    .and_then(|bytes| value.checked_add(bytes))
            })
            .and_then(|value| {
                total_candidates
                    .checked_mul(8)
                    .and_then(|bytes| value.checked_add(bytes))
            })
            .ok_or_else(|| DatasetError::Compilation("RFXBATCH shape overflow".into()))?;
        if payload_len < required_bytes {
            return Err(DatasetError::Compilation(format!(
                "RFXBATCH payload truncated: expected at least {required_bytes} bytes, found {payload_len}"
            )));
        }

        let mut offset = 0;
        let mut group_offsets = Vec::with_capacity(total_groups + 1);
        for _ in 0..=total_groups {
            let val = u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap());
            group_offsets.push(val);
            offset += 4;
        }

        let mut features = Vec::with_capacity(total_candidates * feature_dim);
        for _ in 0..(total_candidates * feature_dim) {
            let val = f32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap());
            features.push(val);
            offset += 4;
        }

        let mut cost_to_go = Vec::with_capacity(total_candidates);
        for _ in 0..total_candidates {
            let val = f32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap());
            cost_to_go.push(val);
            offset += 4;
        }

        let mut weights = Vec::with_capacity(total_candidates);
        for _ in 0..total_candidates {
            let val = f32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap());
            weights.push(val);
            offset += 4;
        }

        if offset.checked_add(4).is_none_or(|end| end > payload.len()) {
            return Err(DatasetError::Compilation(
                "RFXBATCH is missing its label column".into(),
            ));
        }
        let label_len =
            u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
        offset += 4;
        let label_end = offset
            .checked_add(label_len)
            .ok_or_else(|| DatasetError::Compilation("label length overflow".into()))?;
        if label_end != payload.len() {
            return Err(DatasetError::Compilation(
                "RFXBATCH label column is truncated or has trailing bytes".into(),
            ));
        }
        let labels = serde_json::from_slice::<Vec<CandidateKnowledge>>(&payload[offset..label_end])
            .map_err(|error| DatasetError::Compilation(format!("invalid label column: {error}")))?;

        let batch = Self {
            source_dataset_digest,
            feature_dim,
            total_groups,
            total_candidates,
            group_offsets,
            features,
            labels,
            cost_to_go,
            weights,
        };
        batch.validate()?;
        Ok(batch)
    }

    /// Decode RFXBATCH from an in-memory buffer (fuzz / streaming).
    pub fn read_from_bytes(data: &[u8]) -> Result<Self, DatasetError> {
        Self::read_from_reader(std::io::Cursor::new(data))
    }
}

/// Softmax targets over viable cost-to-go labels (mirrors training supervision).
pub fn viable_target(
    labels: &[CandidateKnowledge],
    temperature: f32,
    out: &mut [f32],
) -> Result<bool, DatasetError> {
    if !temperature.is_finite() || temperature <= 0.0 {
        return Err(DatasetError::Compilation(
            "target temperature must be finite and positive".into(),
        ));
    }
    if out.len() < labels.len() {
        return Err(DatasetError::Compilation(format!(
            "output buffer length {} < candidate count {}",
            out.len(),
            labels.len()
        )));
    }
    for label in labels {
        label.validate_evidence()?;
    }
    out.fill(0.0);
    let mut max_logit = f32::NEG_INFINITY;
    for label in labels {
        if let CandidateKnowledge::Viable {
            best_actions_to_go, ..
        } = label
        {
            max_logit = max_logit.max(-(*best_actions_to_go as f32) / temperature);
        }
    }
    if !max_logit.is_finite() {
        return Ok(false);
    }
    let mut sum = 0.0;
    for (index, label) in labels.iter().enumerate() {
        if let CandidateKnowledge::Viable {
            best_actions_to_go, ..
        } = label
        {
            let logit = -(*best_actions_to_go as f32) / temperature;
            let prob = (logit - max_logit).exp();
            out[index] = prob;
            sum += prob;
        }
    }
    if sum > 0.0 {
        for value in out.iter_mut().take(labels.len()) {
            if *value > 0.0 {
                *value /= sum;
            }
        }
    }
    Ok(sum > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rfxbatch_serialization_roundtrip() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("test.rfxbatch");

        let batch = RfxBatch {
            source_dataset_digest: Digest::hash_blake3(b"ds1"),
            feature_dim: 2,
            total_groups: 1,
            total_candidates: 2,
            group_offsets: vec![0, 2],
            features: vec![1.0, 2.0, 3.0, 4.0],
            labels: vec![
                CandidateKnowledge::Viable {
                    best_actions_to_go: 3,
                    receipts: smallvec::smallvec![Digest::hash_blake3(b"receipt")],
                },
                CandidateKnowledge::KnownDead {
                    certificate: Digest::hash_blake3(b"certificate"),
                },
            ],
            cost_to_go: vec![3.0, 99.0],
            weights: vec![1.0, 1.0],
        };

        batch.write_to_file(&path).unwrap();
        let read_batch = RfxBatch::read_from_file(&path).unwrap();

        assert_eq!(read_batch.total_groups, 1);
        assert_eq!(read_batch.total_candidates, 2);
        assert_eq!(read_batch.labels.len(), 2);
        match &read_batch.labels[0] {
            CandidateKnowledge::Viable {
                best_actions_to_go, ..
            } => assert_eq!(*best_actions_to_go, 3),
            _ => panic!("expected viable"),
        }
        match &read_batch.labels[1] {
            CandidateKnowledge::KnownDead { .. } => {}
            _ => panic!("expected dead"),
        }
    }

    #[test]
    fn rfxbatch_preserves_digest_algorithm() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("sha256.rfxbatch");
        let batch = RfxBatch {
            source_dataset_digest: Digest::hash_sha256(b"source-dataset"),
            feature_dim: 1,
            total_groups: 1,
            total_candidates: 1,
            group_offsets: vec![0, 1],
            features: vec![1.0],
            labels: vec![CandidateKnowledge::Unknown],
            cost_to_go: vec![0.0],
            weights: vec![0.0],
        };
        batch.write_to_file(&path).unwrap();
        let decoded = RfxBatch::read_from_file(&path).unwrap();
        assert_eq!(decoded.source_dataset_digest, batch.source_dataset_digest);
    }

    #[test]
    fn malformed_label_json_never_decodes_as_unknown() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("bad-label.rfxbatch");
        let batch = RfxBatch {
            source_dataset_digest: Digest::hash_blake3(b"source-dataset"),
            feature_dim: 1,
            total_groups: 1,
            total_candidates: 1,
            group_offsets: vec![0, 1],
            features: vec![1.0],
            labels: vec![CandidateKnowledge::Unknown],
            cost_to_go: vec![0.0],
            weights: vec![0.0],
        };
        batch.write_to_file(&path).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let label_offset = RFXBATCH_HEADER_BYTES + 8 + 4 + 4 + 4 + 4;
        bytes[label_offset] = b'{';
        let payload_len =
            usize::try_from(u64::from_le_bytes(bytes[54..62].try_into().unwrap())).unwrap();
        let crc = crc32c(&bytes[RFXBATCH_HEADER_BYTES..RFXBATCH_HEADER_BYTES + payload_len]);
        bytes[62..66].copy_from_slice(&crc.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert!(matches!(
            RfxBatch::read_from_file(&path),
            Err(DatasetError::Compilation(message)) if message.contains("label")
        ));
    }

    #[test]
    fn rfxbatch_rejects_declared_payload_before_allocation() {
        let mut header = [0u8; RFXBATCH_HEADER_BYTES];
        header[..8].copy_from_slice(RFXBATCH_MAGIC);
        header[8] = RFXBATCH_VERSION;
        header[9] = 0;
        header[10..42].copy_from_slice(Digest::hash_blake3(b"dataset").as_bytes());
        header[54..62].copy_from_slice(&1025u64.to_le_bytes());
        let error = RfxBatch::read_from_reader_with_limit(&header[..], 1024).unwrap_err();
        assert!(
            matches!(error, DatasetError::Compilation(message) if message.contains("decode limit"))
        );
    }

    #[test]
    fn rfxbatch_rejects_zero_source_identity() {
        let batch = RfxBatch {
            source_dataset_digest: Digest::ZERO,
            feature_dim: 1,
            total_groups: 1,
            total_candidates: 1,
            group_offsets: vec![0, 1],
            features: vec![1.0],
            labels: vec![CandidateKnowledge::Unknown],
            cost_to_go: vec![0.0],
            weights: vec![0.0],
        };
        assert!(
            matches!(batch.validate(), Err(DatasetError::Compilation(message)) if message.contains("identity"))
        );
    }

    #[test]
    fn test_candidate_knowledge_variants() {
        let viable = CandidateKnowledge::Viable {
            best_actions_to_go: 3,
            receipts: smallvec::smallvec![Digest::hash_blake3(b"receipt")],
        };
        let dead = CandidateKnowledge::KnownDead {
            certificate: Digest::hash_blake3(b"cert"),
        };
        let unknown = CandidateKnowledge::Unknown;
        assert!(matches!(viable, CandidateKnowledge::Viable { .. }));
        assert!(matches!(dead, CandidateKnowledge::KnownDead { .. }));
        assert_eq!(unknown, CandidateKnowledge::Unknown);
    }

    #[test]
    fn compiler_preserves_all_receipts_and_reconciles_labels() {
        let state = StateId::from_digest(Digest::hash_blake3(b"state-routes"));
        let candidate = CandidateId::from_digest(Digest::hash_blake3(b"candidate-routes"));
        let receipt_a = Digest::hash_blake3(b"receipt-a");
        let receipt_b = Digest::hash_blake3(b"receipt-b");
        let mut compiler = DatasetCompiler::new();
        compiler.record_state_candidates(state, vec![candidate], "proof-dag");
        compiler
            .record_verified_route(state, candidate, 8, receipt_a)
            .unwrap();
        compiler
            .record_verified_route(state, candidate, 5, receipt_b)
            .unwrap();
        let groups = compiler.finalize_labels();
        assert_eq!(groups.len(), 1);
        match &groups[0].labels[0] {
            CandidateKnowledge::Viable {
                best_actions_to_go,
                receipts,
            } => {
                assert_eq!(*best_actions_to_go, 5);
                assert_eq!(receipts.len(), 2);
                assert!(receipts.contains(&receipt_a));
                assert!(receipts.contains(&receipt_b));
            }
            other => panic!("expected viable label, found {other:?}"),
        }
        assert_eq!(compiler.counters().decision_groups, 1);
        assert_eq!(compiler.counters().logical_rows, 1);
        assert_eq!(compiler.counters().viable_rows, 1);
        assert_eq!(
            compiler.counters().viable_rows
                + compiler.counters().known_dead_rows
                + compiler.counters().unknown_rows
                + compiler.counters().invalid_rows,
            compiler.counters().logical_rows
        );
    }

    #[test]
    fn test_decision_group_compilation() {
        let mut compiler = DatasetCompiler::new();
        let s1 = StateId::from_digest(Digest::hash_blake3(b"s1"));
        let c1 = CandidateId::from_digest(Digest::hash_blake3(b"c1"));
        let c2 = CandidateId::from_digest(Digest::hash_blake3(b"c2"));
        let ep1 = EpisodeId::from_digest(Digest::hash_blake3(b"ep1"));

        compiler.record_state_candidates(s1, vec![c1, c2], "observed");
        if let Some(group) = compiler.groups.get_mut(&s1) {
            group.source_episodes.push(ep1);
        }
        compiler
            .record_verified_route(s1, c1, 4, Digest::hash_blake3(b"receipt1"))
            .unwrap();

        let groups = compiler.finalize_labels();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].candidate_ids.len(), 2);
        match &groups[0].labels[0] {
            CandidateKnowledge::Viable {
                best_actions_to_go, ..
            } => assert_eq!(*best_actions_to_go, 4),
            _ => panic!("expected viable"),
        }
        assert_eq!(groups[0].labels[1], CandidateKnowledge::Unknown);
    }

    #[test]
    fn test_viable_target_calculation() {
        let labels = vec![
            CandidateKnowledge::Viable {
                best_actions_to_go: 2,
                receipts: smallvec::smallvec![Digest::hash_blake3(b"receipt-2")],
            },
            CandidateKnowledge::Viable {
                best_actions_to_go: 4,
                receipts: smallvec::smallvec![Digest::hash_blake3(b"receipt-4")],
            },
            CandidateKnowledge::Unknown,
            CandidateKnowledge::KnownDead {
                certificate: Digest::hash_blake3(b"dead-certificate"),
            },
        ];

        let mut targets = vec![0.0f32; 4];
        let has_supervision = viable_target(&labels, 1.0, &mut targets).unwrap();
        assert!(has_supervision);
        assert!(targets[0] > targets[1]);
        assert_eq!(targets[2], 0.0);
        assert_eq!(targets[3], 0.0);
        assert!((targets[0] + targets[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_no_default_ep_or_receipt_placeholders() {
        // INV-RFX-11 / INV-RFX-21: Closed without ArtifactConstruction → Unknown, not Viable.
        let mut compiler = DatasetCompiler::new();
        let s1 = StateId::from_digest(Digest::hash_blake3(b"s-closed"));
        let c1 = CandidateId::from_digest(Digest::hash_blake3(b"c-closed"));
        compiler.record_state_candidates(s1, vec![c1], "observed");
        let groups = compiler.finalize_labels();
        assert_eq!(groups[0].labels[0], CandidateKnowledge::Unknown);

        // Placeholder literal digests must not appear as Viable receipts.
        let forbidden_ep = Digest::hash_blake3(b"default-ep");
        let forbidden_receipt = Digest::hash_blake3(b"default-receipt");
        assert_ne!(
            groups[0].source_episodes,
            vec![EpisodeId::from_digest(forbidden_ep)]
        );
        for label in &groups[0].labels {
            if let CandidateKnowledge::Viable { receipts, .. } = label {
                assert!(!receipts.contains(&forbidden_receipt));
            }
        }
    }
}
