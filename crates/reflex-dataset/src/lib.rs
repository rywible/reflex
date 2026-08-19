use crc32c::crc32c;
use reflex_ledger::{Event, RecoveredSegment};
use reflex_types::{
    ActionSchemaId, BuildIdentity, CandidateId, CellId, DatasetSchemaId, Digest, EpisodeId,
    FeatureSchemaId, StateId,
};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write as IoWrite};
use std::path::Path;
use thiserror::Error;

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

pub struct DatasetCompiler {
    groups: BTreeMap<StateId, DecisionGroup>,
    viable_routes: HashMap<(StateId, CandidateId), (u32, Digest)>,
    dead_certificates: HashMap<(StateId, CandidateId), Digest>,
}

impl DatasetCompiler {
    pub fn new() -> Self {
        Self {
            groups: BTreeMap::new(),
            viable_routes: HashMap::new(),
            dead_certificates: HashMap::new(),
        }
    }

    pub fn record_verified_route(
        &mut self,
        state_id: StateId,
        candidate_id: CandidateId,
        actions_to_go: u32,
        receipt: Digest,
    ) {
        let entry = self
            .viable_routes
            .entry((state_id, candidate_id))
            .or_insert((actions_to_go, receipt));
        if actions_to_go < entry.0 {
            *entry = (actions_to_go, receipt);
        }
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

    pub fn ingest_segment(&mut self, segment: &RecoveredSegment, segment_path: &Path) {
        let events = match segment.read_events(segment_path) {
            Ok(events) => events,
            Err(_) => return,
        };

        let mut current_episode = None;
        let mut last_constructed_artifact_id: Option<reflex_types::ArtifactId> = None;

        for seq_event in &events {
            match &seq_event.event {
                Event::EpisodeStart(ep) => {
                    current_episode = Some(ep.episode_id);
                }
                Event::CandidateBatch(cb) => {
                    let ep = current_episode.unwrap_or_else(|| {
                        EpisodeId::from_digest(Digest::hash_blake3(b"default-ep"))
                    });
                    let entry = self
                        .groups
                        .entry(cb.state_id)
                        .or_insert_with(|| DecisionGroup {
                            state_id: cb.state_id,
                            candidate_ids: cb.candidate_ids.clone(),
                            labels: vec![CandidateKnowledge::Unknown; cb.candidate_ids.len()],
                            feature_ref: Digest::hash_blake3(&cb.state_id.digest().bytes),
                            source_episodes: Vec::new(),
                            coverage: "observed".to_string(),
                        });
                    if !entry.source_episodes.contains(&ep) {
                        entry.source_episodes.push(ep);
                    }
                }
                Event::ArtifactConstruction(ac) => {
                    last_constructed_artifact_id = Some(ac.artifact_id);
                }
                Event::CandidateApplication(ca) if ca.outcome == "Closed" => {
                    let receipt = last_constructed_artifact_id
                        .map(|a| Digest::hash_blake3(&a.digest().bytes))
                        .unwrap_or_else(|| Digest::hash_blake3(b"default-receipt"));
                    self.record_verified_route(ca.state_id, ca.candidate_id, 1, receipt);
                }
                _ => {}
            }
        }
    }

    pub fn finalize_labels(&mut self) -> Vec<DecisionGroup> {
        for (_state_id, group) in self.groups.iter_mut() {
            for (idx, cand_id) in group.candidate_ids.iter().enumerate() {
                if let Some((cost, receipt)) = self.viable_routes.get(&(group.state_id, *cand_id))
                {
                    let mut receipts = SmallVec::new();
                    receipts.push(*receipt);
                    group.labels[idx] = CandidateKnowledge::Viable {
                        best_actions_to_go: *cost,
                        receipts,
                    };
                } else if let Some(cert) =
                    self.dead_certificates.get(&(group.state_id, *cand_id))
                {
                    group.labels[idx] = CandidateKnowledge::KnownDead {
                        certificate: *cert,
                    };
                } else {
                    group.labels[idx] = CandidateKnowledge::Unknown;
                }
            }
        }
        self.groups.values().cloned().collect()
    }
}

impl Default for DatasetCompiler {
    fn default() -> Self {
        Self::new()
    }
}

pub const RFXBATCH_MAGIC: &[u8; 8] = b"RFXBATCH";

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
    pub fn write_to_file(&self, path: &Path) -> Result<(), DatasetError> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?;

        let mut header = Vec::new();
        header.extend_from_slice(RFXBATCH_MAGIC);
        header.extend_from_slice(&self.source_dataset_digest.bytes);
        header.extend_from_slice(&(self.feature_dim as u32).to_le_bytes());
        header.extend_from_slice(&(self.total_groups as u32).to_le_bytes());
        header.extend_from_slice(&(self.total_candidates as u32).to_le_bytes());

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
        payload.extend_from_slice(&(labels_json.len() as u32).to_le_bytes());
        payload.extend_from_slice(&labels_json);

        let crc = crc32c(&payload);
        header.extend_from_slice(&(payload.len() as u64).to_le_bytes());
        header.extend_from_slice(&crc.to_le_bytes());

        file.write_all(&header)?;
        file.write_all(&payload)?;
        file.flush()?;
        Ok(())
    }

    pub fn read_from_file(path: &Path) -> Result<Self, DatasetError> {
        let mut file = File::open(path)?;
        let mut header_buf = [0u8; 8 + 32 + 4 + 4 + 4 + 8 + 4];
        file.read_exact(&mut header_buf)?;

        if &header_buf[0..8] != RFXBATCH_MAGIC {
            let mut m = [0u8; 8];
            m.copy_from_slice(&header_buf[0..8]);
            return Err(DatasetError::InvalidMagic(m));
        }

        let mut digest_bytes = [0u8; 32];
        digest_bytes.copy_from_slice(&header_buf[8..40]);
        let source_dataset_digest = Digest::from_blake3_bytes(digest_bytes);

        let feature_dim = u32::from_le_bytes(header_buf[40..44].try_into().unwrap()) as usize;
        let total_groups = u32::from_le_bytes(header_buf[44..48].try_into().unwrap()) as usize;
        let total_candidates =
            u32::from_le_bytes(header_buf[48..52].try_into().unwrap()) as usize;
        let payload_len = u64::from_le_bytes(header_buf[52..60].try_into().unwrap()) as usize;
        let expected_crc = u32::from_le_bytes(header_buf[60..64].try_into().unwrap());

        let mut payload = vec![0u8; payload_len];
        file.read_exact(&mut payload)?;

        if crc32c(&payload) != expected_crc {
            return Err(DatasetError::CrcMismatch);
        }

        let required_bytes = (total_groups + 1) * 4
            + (total_candidates * feature_dim) * 4
            + total_candidates * 4
            + total_candidates * 4;
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

        let labels = if offset + 4 <= payload.len() {
            let l_len =
                u32::from_le_bytes(payload[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            if offset + l_len <= payload.len() {
                serde_json::from_slice::<Vec<CandidateKnowledge>>(&payload[offset..offset + l_len])
                    .unwrap_or_else(|_| vec![CandidateKnowledge::Unknown; total_candidates])
            } else {
                vec![CandidateKnowledge::Unknown; total_candidates]
            }
        } else {
            vec![CandidateKnowledge::Unknown; total_candidates]
        };

        Ok(Self {
            source_dataset_digest,
            feature_dim,
            total_groups,
            total_candidates,
            group_offsets,
            features,
            labels,
            cost_to_go,
            weights,
        })
    }
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
                    receipts: SmallVec::new(),
                },
                CandidateKnowledge::KnownDead {
                    certificate: Digest::ZERO,
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
        compiler.record_verified_route(s1, c1, 4, Digest::hash_blake3(b"receipt1"));

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
}
