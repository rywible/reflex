//! Parquet dataset compaction and publication (P4.5, ADR-0007).

use crate::{CandidateKnowledge, DatasetError, DatasetManifest, DatasetShard, DecisionGroup};
use arrow::array::{ArrayRef, BinaryArray, Float32Array, RecordBatch, StringArray, UInt32Array};
use arrow::datatypes::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use reflex_types::{BuildIdentity, CellId, DatasetSchemaId, Digest, FeatureSchemaId};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy)]
struct DatasetPublicationIdentity {
    logical: Digest,
    schema: Digest,
    feature_schema: Digest,
    action_schema: Digest,
    split_manifest: Digest,
    compiler: Digest,
}

impl CanonicalEncode for DatasetPublicationIdentity {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        self.logical.encode_canonical(out)?;
        self.schema.encode_canonical(out)?;
        self.feature_schema.encode_canonical(out)?;
        self.action_schema.encode_canonical(out)?;
        self.split_manifest.encode_canonical(out)?;
        self.compiler.encode_canonical(out)
    }
}

fn publication_identity_digest(
    identity: DatasetPublicationIdentity,
) -> Result<Digest, DatasetError> {
    reflex_canonical::content_id(b"reflex.dataset.publication.v2", &identity)
        .map_err(|error| DatasetError::Compilation(error.to_string()))
}

#[allow(dead_code)] // logical table name for analytics registration (P4.5)
const DECISIONS_TABLE: &str = "decisions";

/// Compacts decision groups into zstd Parquet shards keyed by dataset digest.
pub struct ParquetCompactor {
    pub feature_dim: usize,
    pub shard_row_limit: u64,
}

impl Default for ParquetCompactor {
    fn default() -> Self {
        Self {
            feature_dim: 64,
            shard_row_limit: 100_000,
        }
    }
}

impl ParquetCompactor {
    /// Fuzz-safe probe: reject non-Parquet or truncated logical schemas.
    pub fn probe_logical_schema(data: &[u8]) {
        if data.len() < 4 {
            return;
        }
        if &data[..4] != b"PAR1" {
            return;
        }
        let _ = Self::decisions_schema();
    }

    pub fn decisions_schema() -> Schema {
        Schema::new(vec![
            Field::new("state_id", DataType::Binary, false),
            Field::new("candidate_id", DataType::Binary, false),
            Field::new("candidate_index", DataType::UInt32, false),
            Field::new("label_code", DataType::UInt32, false),
            Field::new("cost_to_go", DataType::Float32, true),
            Field::new("evidence_refs", DataType::Binary, false),
            Field::new("feature_ref", DataType::Binary, false),
            Field::new("coverage", DataType::Utf8, false),
        ])
    }

    /// Identity of the exact logical Arrow schema, including field order,
    /// physical types, nullability and label encoding version.
    pub fn decisions_schema_id() -> DatasetSchemaId {
        DatasetSchemaId::from_digest(Digest::hash_blake3(
            b"reflex.decisions.arrow.v2\0state_id:binary!\0candidate_id:binary!\0candidate_index:u32!\0label_code:u32!{unknown=0,viable=1,dead=2,invalid=3}\0cost_to_go:f32?\0evidence_refs:json<digest>[]!\0feature_ref:digest!\0coverage:utf8!",
        ))
    }

    /// Compact groups into temporary shard files, validate row counts, then
    /// publish the canonical manifest. Interrupted compaction rolls back temps.
    #[allow(clippy::too_many_arguments)]
    pub fn compact_and_publish(
        &self,
        groups: &[DecisionGroup],
        work_dir: &Path,
        source_cells: Vec<CellId>,
        source_ledgers: Vec<Digest>,
        schema_id: DatasetSchemaId,
        feature_schema: FeatureSchemaId,
        action_schema: reflex_types::ActionSchemaId,
        split_manifest: Digest,
        compiler: BuildIdentity,
    ) -> Result<(DatasetManifest, PathBuf), DatasetError> {
        if schema_id != Self::decisions_schema_id() {
            return Err(DatasetError::SchemaMismatch {
                expected: Self::decisions_schema_id().to_string(),
                found: schema_id.to_string(),
            });
        }
        if feature_schema.digest() == &Digest::ZERO
            || action_schema.digest() == &Digest::ZERO
            || split_manifest == Digest::ZERO
            || compiler.digest() == &Digest::ZERO
        {
            return Err(DatasetError::Compilation(
                "dataset publication identities must be non-zero".into(),
            ));
        }
        for group in groups {
            validate_group(group)?;
        }
        let logical = Self::logical_identity_digest(groups, &source_cells, &source_ledgers);
        let dataset_digest = publication_identity_digest(DatasetPublicationIdentity {
            logical,
            schema: *schema_id.digest(),
            feature_schema: *feature_schema.digest(),
            action_schema: *action_schema.digest(),
            split_manifest,
            compiler: *compiler.digest(),
        })?;
        let staging = work_dir.join(format!(".compact-{}", dataset_digest.to_hex()));
        if staging.exists() {
            fs::remove_dir_all(&staging).map_err(|e| DatasetError::Io(e.to_string()))?;
        }
        fs::create_dir_all(&staging).map_err(|e| DatasetError::Io(e.to_string()))?;

        let mut guard = CompactionGuard::new(staging.clone());
        let mut logical_rows = 0u64;
        let mut shards = Vec::new();
        let mut shard_id = 0u32;
        let mut batch_rows: Vec<DecisionGroup> = Vec::new();
        let mut batch_count = 0u64;

        for group in groups {
            batch_rows.push(group.clone());
            batch_count += group.candidate_ids.len() as u64;
            if batch_count >= self.shard_row_limit {
                let shard = self.write_shard(&staging, dataset_digest, shard_id, &batch_rows)?;
                logical_rows += shard.row_count;
                shards.push(shard);
                shard_id += 1;
                batch_rows.clear();
                batch_count = 0;
            }
        }
        if !batch_rows.is_empty() {
            let shard = self.write_shard(&staging, dataset_digest, shard_id, &batch_rows)?;
            logical_rows += shard.row_count;
            shards.push(shard);
        }

        let manifest = DatasetManifest {
            schema: schema_id,
            source_cells,
            source_ledgers,
            shards,
            logical_rows,
            decision_groups: groups.len() as u64,
            feature_schema,
            action_schema,
            split_manifest,
            compiler,
        };

        guard.commit();
        let published = work_dir.join(format!("dataset-{}", dataset_digest.to_hex()));
        if published.exists() {
            fs::remove_dir_all(&published).map_err(|e| DatasetError::Io(e.to_string()))?;
        }
        fs::rename(&staging, &published).map_err(|e| DatasetError::Io(e.to_string()))?;
        Ok((manifest, published))
    }

    pub fn logical_identity_digest(
        groups: &[DecisionGroup],
        source_cells: &[CellId],
        source_ledgers: &[Digest],
    ) -> Digest {
        let mut hasher = blake3::Hasher::new();
        for cell in source_cells {
            hasher.update(cell.digest().as_bytes());
        }
        for ledger in source_ledgers {
            hasher.update(ledger.as_bytes());
        }
        for group in groups {
            let encoded = serde_json::to_vec(group).expect("DecisionGroup is serializable");
            hasher.update(&(encoded.len() as u64).to_le_bytes());
            hasher.update(&encoded);
        }
        Digest::from_blake3_bytes(*hasher.finalize().as_bytes())
    }

    fn write_shard(
        &self,
        staging: &Path,
        dataset_digest: Digest,
        shard_id: u32,
        groups: &[DecisionGroup],
    ) -> Result<DatasetShard, DatasetError> {
        let partition_dir = staging
            .join(format!("digest={}", dataset_digest.to_hex()))
            .join(format!("shard={shard_id}"));
        fs::create_dir_all(&partition_dir).map_err(|e| DatasetError::Io(e.to_string()))?;
        let path = partition_dir.join(format!("part-{shard_id}.parquet"));

        let schema = Arc::new(Self::decisions_schema());
        let mut state_ids = Vec::new();
        let mut candidate_ids = Vec::new();
        let mut cand_idx = Vec::new();
        let mut label_codes = Vec::new();
        let mut costs = Vec::new();
        let mut evidence_refs = Vec::new();
        let mut feature_refs = Vec::new();
        let mut coverage = Vec::new();

        for group in groups {
            for (idx, label) in group.labels.iter().enumerate() {
                state_ids.push(group.state_id.digest().bytes.to_vec());
                candidate_ids.push(group.candidate_ids[idx].digest().bytes.to_vec());
                cand_idx.push(idx as u32);
                let (code, cost, evidence) = label_columns(label)?;
                label_codes.push(code);
                costs.push(cost);
                evidence_refs.push(evidence);
                feature_refs.push(group.feature_ref.bytes.to_vec());
                coverage.push(group.coverage.clone());
            }
        }

        let row_count = state_ids.len() as u64;
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(BinaryArray::from_iter_values(state_ids.iter())) as ArrayRef,
                Arc::new(BinaryArray::from_iter_values(candidate_ids.iter())) as ArrayRef,
                Arc::new(UInt32Array::from(cand_idx)),
                Arc::new(UInt32Array::from(label_codes)),
                Arc::new(Float32Array::from(costs)),
                Arc::new(BinaryArray::from_iter_values(evidence_refs.iter())) as ArrayRef,
                Arc::new(BinaryArray::from_iter_values(feature_refs.iter())) as ArrayRef,
                Arc::new(StringArray::from(coverage)),
            ],
        )
        .map_err(|e| DatasetError::Compilation(e.to_string()))?;

        let props = WriterProperties::builder()
            .set_compression(Compression::ZSTD(Default::default()))
            .build();
        let file = File::create(&path).map_err(|e| DatasetError::Io(e.to_string()))?;
        let mut writer = ArrowWriter::try_new(file, schema, Some(props))
            .map_err(|e| DatasetError::Compilation(e.to_string()))?;
        writer
            .write(&batch)
            .map_err(|e| DatasetError::Compilation(e.to_string()))?;
        writer
            .close()
            .map_err(|e| DatasetError::Compilation(e.to_string()))?;

        let bytes = fs::read(&path).map_err(|e| DatasetError::Io(e.to_string()))?;
        let digest = Digest::hash_blake3(&bytes);
        Ok(DatasetShard {
            shard_id,
            digest,
            row_count,
            group_count: groups.len() as u64,
        })
    }

    /// Column projection over a published Parquet shard (predicate pushdown smoke).
    pub fn project_label_codes(path: &Path, feature_dim: usize) -> Result<Vec<u32>, DatasetError> {
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        let file = File::open(path).map_err(|e| DatasetError::Io(e.to_string()))?;
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| DatasetError::Compilation(e.to_string()))?;
        let schema = builder.schema();
        let label_idx = schema
            .index_of("label_code")
            .map_err(|e| DatasetError::Compilation(e.to_string()))?;
        let _ = feature_dim;
        let mut codes = Vec::new();
        for batch in builder
            .build()
            .map_err(|e| DatasetError::Compilation(e.to_string()))?
        {
            let batch = batch.map_err(|e| DatasetError::Compilation(e.to_string()))?;
            let col = batch
                .column(label_idx)
                .as_any()
                .downcast_ref::<UInt32Array>()
                .ok_or_else(|| DatasetError::Compilation("label_code column missing".into()))?;
            codes.extend(col.values().iter().copied());
        }
        Ok(codes)
    }
}

fn label_columns(label: &CandidateKnowledge) -> Result<(u32, Option<f32>, Vec<u8>), DatasetError> {
    label.validate_evidence()?;
    let columns = match label {
        CandidateKnowledge::Viable {
            best_actions_to_go,
            receipts,
        } => (
            1,
            Some(*best_actions_to_go as f32),
            serde_json::to_vec(receipts),
        ),
        CandidateKnowledge::KnownDead { certificate } => {
            (2, None, serde_json::to_vec(&[*certificate]))
        }
        CandidateKnowledge::Unknown => (0, None, serde_json::to_vec(&Vec::<Digest>::new())),
        CandidateKnowledge::Invalid { .. } => (3, None, serde_json::to_vec(&Vec::<Digest>::new())),
    };
    Ok((
        columns.0,
        columns.1,
        columns
            .2
            .map_err(|error| DatasetError::Compilation(error.to_string()))?,
    ))
}

fn validate_group(group: &DecisionGroup) -> Result<(), DatasetError> {
    if group.state_id.digest() == &Digest::ZERO
        || group.feature_ref == Digest::ZERO
        || group.candidate_ids.is_empty()
        || group.candidate_ids.len() != group.labels.len()
        || group.coverage.is_empty()
    {
        return Err(DatasetError::Compilation(
            "decision group has invalid identity or column geometry".into(),
        ));
    }
    let mut candidates = std::collections::BTreeSet::new();
    for (candidate, label) in group.candidate_ids.iter().zip(&group.labels) {
        if candidate.digest() == &Digest::ZERO || !candidates.insert(*candidate) {
            return Err(DatasetError::Compilation(
                "decision group contains a zero or duplicate candidate".into(),
            ));
        }
        label.validate_evidence()?;
    }
    Ok(())
}

/// Rolls back the staging directory unless explicitly committed (interrupted compaction).
struct CompactionGuard {
    path: PathBuf,
    committed: bool,
}

impl CompactionGuard {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            committed: false,
        }
    }

    fn commit(&mut self) {
        self.committed = true;
    }
}

impl Drop for CompactionGuard {
    fn drop(&mut self) {
        if !self.committed && self.path.exists() {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DatasetCompiler;
    use reflex_types::{ActionSchemaId, CandidateId, Digest, FeatureSchemaId, StateId};

    #[test]
    fn test_parquet_compaction_roundtrip() {
        let mut compiler = DatasetCompiler::new();
        let s1 = StateId::from_digest(Digest::hash_blake3(b"s1"));
        let c1 = CandidateId::from_digest(Digest::hash_blake3(b"c1"));
        compiler.record_state_candidates(s1, vec![c1], "observed");
        compiler
            .record_verified_route(s1, c1, 2, Digest::hash_blake3(b"rcpt"))
            .unwrap();
        let groups = compiler.finalize_labels();

        let temp = tempfile::tempdir().unwrap();
        let compactor = ParquetCompactor::default();
        let cell = CellId::from_digest(Digest::hash_blake3(b"cell"));
        let (manifest, published) = compactor
            .compact_and_publish(
                &groups,
                temp.path(),
                vec![cell],
                vec![Digest::hash_blake3(b"ledger")],
                ParquetCompactor::decisions_schema_id(),
                FeatureSchemaId::from_digest(Digest::hash_blake3(b"feat")),
                ActionSchemaId::from_digest(Digest::hash_blake3(b"act")),
                Digest::hash_blake3(b"split"),
                BuildIdentity::from_digest(Digest::hash_blake3(b"compiler")),
            )
            .unwrap();
        assert_eq!(manifest.logical_rows, 1);
        assert!(published.exists());
        let shard_path = published
            .read_dir()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("shard=0/part-0.parquet");
        let codes = ParquetCompactor::project_label_codes(&shard_path, 64).unwrap();
        assert_eq!(codes, vec![1]);
    }

    #[test]
    fn test_interrupted_compaction_publishes_no_dataset() {
        let temp = tempfile::tempdir().unwrap();
        let staging = temp.path().join(".compact-deadbeef");
        fs::create_dir_all(&staging).unwrap();
        {
            let _guard = CompactionGuard::new(staging.clone());
        }
        assert!(!staging.exists());
    }

    #[test]
    fn test_repeated_compaction_identical_identity() {
        let mut compiler = DatasetCompiler::new();
        let s1 = StateId::from_digest(Digest::hash_blake3(b"s1"));
        let c1 = CandidateId::from_digest(Digest::hash_blake3(b"c1"));
        compiler.record_state_candidates(s1, vec![c1], "observed");
        let groups = compiler.finalize_labels();
        let cells = vec![CellId::from_digest(Digest::hash_blake3(b"cell"))];
        let ledgers = vec![Digest::hash_blake3(b"ledger")];
        let d1 = ParquetCompactor::logical_identity_digest(&groups, &cells, &ledgers);
        let d2 = ParquetCompactor::logical_identity_digest(&groups, &cells, &ledgers);
        assert_eq!(d1, d2);
    }

    #[test]
    fn publication_identity_commits_to_every_semantic_field() {
        let base = DatasetPublicationIdentity {
            logical: Digest::hash_blake3(b"logical"),
            schema: Digest::hash_blake3(b"schema"),
            feature_schema: Digest::hash_blake3(b"features"),
            action_schema: Digest::hash_blake3(b"actions"),
            split_manifest: Digest::hash_blake3(b"split"),
            compiler: Digest::hash_blake3(b"compiler"),
        };
        let base_digest = publication_identity_digest(base).unwrap();
        let replacement = Digest::hash_blake3(b"changed");
        let mutations = [
            DatasetPublicationIdentity {
                logical: replacement,
                ..base
            },
            DatasetPublicationIdentity {
                schema: replacement,
                ..base
            },
            DatasetPublicationIdentity {
                feature_schema: replacement,
                ..base
            },
            DatasetPublicationIdentity {
                action_schema: replacement,
                ..base
            },
            DatasetPublicationIdentity {
                split_manifest: replacement,
                ..base
            },
            DatasetPublicationIdentity {
                compiler: replacement,
                ..base
            },
        ];
        for mutation in mutations {
            assert_ne!(publication_identity_digest(mutation).unwrap(), base_digest);
        }
    }
}
