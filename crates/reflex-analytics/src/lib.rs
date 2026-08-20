//! DataFusion-backed analytics over published Parquet datasets (P4.7, ADR-0007).

use datafusion::execution::config::SessionConfig;
use datafusion::execution::runtime_env::RuntimeEnvBuilder;
use datafusion::prelude::*;
use reflex_dataset::DatasetManifest;
use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum AnalyticsError {
    #[error("query error: {0}")]
    Query(String),
    #[error("dataset table not found: {0}")]
    TableNotFound(String),
    #[error("memory limit exceeded during analytical query")]
    OutOfMemory,
    #[error("catalog error: {0}")]
    Catalog(String),
    #[error("invalid analytics configuration: {0}")]
    InvalidConfiguration(String),
    #[error("query result exceeded configured row or byte bounds")]
    ResultLimit,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
    pub query_digest: Digest,
    pub sql_digest: Option<Digest>,
}

/// DataFusion session with dataset catalog, spill directory, and memory limits.
pub struct AnalyticsSession {
    ctx: SessionContext,
    pub memory_limit_bytes: usize,
    pub spill_dir: PathBuf,
    versioned_sql: HashMap<String, Digest>,
    max_result_rows: usize,
    max_result_bytes: usize,
}

impl AnalyticsSession {
    pub fn new(memory_limit_bytes: usize, spill_dir: PathBuf) -> Result<Self, AnalyticsError> {
        if memory_limit_bytes < 1024 * 1024 {
            return Err(AnalyticsError::InvalidConfiguration(
                "memory limit must be at least 1 MiB".into(),
            ));
        }
        std::fs::create_dir_all(&spill_dir)
            .map_err(|error| AnalyticsError::Catalog(format!("create spill directory: {error}")))?;
        let runtime = RuntimeEnvBuilder::new()
            .with_temp_file_path(spill_dir.clone())
            .with_memory_limit(memory_limit_bytes, 1.0)
            .build_arc()
            .map_err(|error| AnalyticsError::InvalidConfiguration(error.to_string()))?;
        let config = SessionConfig::new().with_target_partitions(4);
        let ctx = SessionContext::new_with_config_rt(config, runtime);
        Ok(Self {
            ctx,
            memory_limit_bytes,
            spill_dir,
            versioned_sql: HashMap::new(),
            max_result_rows: (memory_limit_bytes / 256).clamp(1, 1_000_000),
            max_result_bytes: memory_limit_bytes / 2,
        })
    }

    /// Register a versioned SQL report; its digest is stamped into query results.
    pub fn register_sql_report(&mut self, name: &str, sql: &str) -> Digest {
        let digest = Digest::hash_blake3(sql.as_bytes());
        self.versioned_sql.insert(name.to_string(), digest);
        digest
    }

    pub fn load_sql_file(&mut self, name: &str, path: &Path) -> Result<Digest, AnalyticsError> {
        let sql = std::fs::read_to_string(path)
            .map_err(|e| AnalyticsError::Catalog(format!("read sql {}: {e}", path.display())))?;
        Ok(self.register_sql_report(name, &sql))
    }

    /// Register a local Parquet shard directory from a published dataset manifest.
    pub async fn register_dataset_manifest(
        &self,
        table: &str,
        manifest: &DatasetManifest,
        base_path: &Path,
    ) -> Result<(), AnalyticsError> {
        validate_table_name(table)?;
        let files = discover_parquet_files(base_path)?;
        validate_dataset_tree(manifest, &files)?;
        self.register_parquet_files(table, &files).await
    }

    /// Register a local Parquet tree. Remote providers are outside native v1.
    pub async fn register_local_parquet_table(
        &self,
        table: &str,
        base_path: &Path,
    ) -> Result<(), AnalyticsError> {
        validate_table_name(table)?;
        self.register_local_parquet_tree(table, base_path).await
    }

    async fn register_local_parquet_tree(
        &self,
        table: &str,
        base_path: &Path,
    ) -> Result<(), AnalyticsError> {
        let files = discover_parquet_files(base_path)?;
        if files.is_empty() {
            return Err(AnalyticsError::Catalog(format!(
                "no parquet files under {}",
                base_path.display()
            )));
        }
        self.register_parquet_files(table, &files).await
    }

    async fn register_parquet_files(
        &self,
        table: &str,
        files: &[PathBuf],
    ) -> Result<(), AnalyticsError> {
        if files.len() == 1 {
            return self
                .ctx
                .register_parquet(
                    table,
                    &parquet_file_url(&files[0]),
                    ParquetReadOptions::default(),
                )
                .await
                .map_err(|error| AnalyticsError::Catalog(error.to_string()));
        }
        for (index, path) in files.iter().enumerate() {
            let shard_name = format!("{table}_shard_{index}");
            self.ctx
                .register_parquet(
                    &shard_name,
                    &parquet_file_url(path),
                    ParquetReadOptions::default(),
                )
                .await
                .map_err(|error| AnalyticsError::Catalog(error.to_string()))?;
        }
        let union = (0..files.len())
            .map(|index| format!("SELECT * FROM {table}_shard_{index}"))
            .collect::<Vec<_>>()
            .join(" UNION ALL ");
        self.ctx
            .sql(&format!("CREATE OR REPLACE VIEW {table} AS {union}"))
            .await
            .map_err(|error| AnalyticsError::Catalog(error.to_string()))?;
        Ok(())
    }

    pub async fn execute_query_async(&self, sql: &str) -> Result<QueryResult, AnalyticsError> {
        if sql.len() > self.memory_limit_bytes || sql.trim().is_empty() {
            return Err(AnalyticsError::InvalidConfiguration(
                "query must be non-empty and fit the session memory bound".into(),
            ));
        }
        let query_digest = Digest::hash_blake3(sql.as_bytes());
        let sql_digest = self
            .versioned_sql
            .values()
            .copied()
            .find(|digest| *digest == query_digest);

        let df = self
            .ctx
            .sql(sql)
            .await
            .map_err(|e| AnalyticsError::Query(e.to_string()))?;
        let batches = df.collect().await.map_err(|e| {
            if e.to_string().contains("Resources exhausted") {
                AnalyticsError::OutOfMemory
            } else {
                AnalyticsError::Query(e.to_string())
            }
        })?;

        let mut columns = Vec::new();
        let mut rows = Vec::new();
        let mut result_bytes = 0usize;
        for batch in batches {
            if columns.is_empty() {
                columns = batch
                    .schema()
                    .fields()
                    .iter()
                    .map(|f| f.name().clone())
                    .collect();
            }
            for row_idx in 0..batch.num_rows() {
                if rows.len() >= self.max_result_rows {
                    return Err(AnalyticsError::ResultLimit);
                }
                let mut row = Vec::with_capacity(columns.len());
                for col_idx in 0..batch.num_columns() {
                    let array = batch.column(col_idx);
                    let value = arrow_array_string_at(array, row_idx);
                    result_bytes = result_bytes
                        .checked_add(value.len())
                        .ok_or(AnalyticsError::ResultLimit)?;
                    if result_bytes > self.max_result_bytes {
                        return Err(AnalyticsError::ResultLimit);
                    }
                    row.push(value);
                }
                rows.push(row);
            }
        }

        Ok(QueryResult {
            columns,
            rows,
            query_digest,
            sql_digest,
        })
    }
}

fn arrow_array_string_at(array: &dyn arrow::array::Array, idx: usize) -> String {
    use arrow::array::{Int64Array, StringArray, UInt32Array, UInt64Array};
    if let Some(a) = array.as_any().downcast_ref::<StringArray>() {
        return a.value(idx).to_string();
    }
    if let Some(a) = array.as_any().downcast_ref::<Int64Array>() {
        return a.value(idx).to_string();
    }
    if let Some(a) = array.as_any().downcast_ref::<UInt32Array>() {
        return a.value(idx).to_string();
    }
    if let Some(a) = array.as_any().downcast_ref::<UInt64Array>() {
        return a.value(idx).to_string();
    }
    format!("{array:?}")
}

fn parquet_file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

fn validate_table_name(table: &str) -> Result<(), AnalyticsError> {
    let mut chars = table.chars();
    let starts_valid = chars
        .next()
        .is_some_and(|character| character.is_ascii_alphabetic() || character == '_');
    if !starts_valid
        || table.len() > 128
        || !chars.all(|character| character.is_ascii_alphanumeric() || character == '_')
    {
        return Err(AnalyticsError::Catalog(
            "table name must be a bounded ASCII SQL identifier".into(),
        ));
    }
    Ok(())
}

fn discover_parquet_files(base: &Path) -> Result<Vec<PathBuf>, AnalyticsError> {
    let base = base
        .canonicalize()
        .map_err(|error| AnalyticsError::Catalog(format!("canonicalize dataset root: {error}")))?;
    let mut files = Vec::new();
    collect_parquet_recursive(&base, &base, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_parquet_recursive(
    root: &Path,
    dir: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), AnalyticsError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| AnalyticsError::Catalog(format!("read_dir {}: {e}", dir.display())))?;
    for entry in entries {
        let entry = entry.map_err(|e| AnalyticsError::Catalog(e.to_string()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| AnalyticsError::Catalog(error.to_string()))?;
        if file_type.is_symlink() {
            return Err(AnalyticsError::Catalog(format!(
                "dataset tree contains a symlink: {}",
                path.display()
            )));
        }
        if file_type.is_dir() {
            let canonical = path
                .canonicalize()
                .map_err(|error| AnalyticsError::Catalog(error.to_string()))?;
            if !canonical.starts_with(root) {
                return Err(AnalyticsError::Catalog(
                    "dataset directory escaped its canonical root".into(),
                ));
            }
            collect_parquet_recursive(root, &canonical, out)?;
        } else if path.extension().is_some_and(|ext| ext == "parquet") {
            out.push(path);
        }
    }
    Ok(())
}

fn validate_dataset_tree(
    manifest: &DatasetManifest,
    files: &[PathBuf],
) -> Result<(), AnalyticsError> {
    use std::collections::{BTreeMap, BTreeSet};

    if manifest.schema.digest() == &Digest::ZERO
        || manifest.feature_schema.digest() == &Digest::ZERO
        || manifest.action_schema.digest() == &Digest::ZERO
        || manifest.split_manifest == Digest::ZERO
        || manifest.compiler.digest() == &Digest::ZERO
        || manifest.shards.is_empty()
        || files.len() != manifest.shards.len()
        || manifest
            .source_cells
            .iter()
            .any(|id| id.digest() == &Digest::ZERO)
        || manifest.source_ledgers.contains(&Digest::ZERO)
    {
        return Err(AnalyticsError::Catalog(
            "dataset manifest identities or shard geometry are invalid".into(),
        ));
    }
    let source_cells: BTreeSet<_> = manifest.source_cells.iter().copied().collect();
    let source_ledgers: BTreeSet<_> = manifest.source_ledgers.iter().copied().collect();
    if source_cells.len() != manifest.source_cells.len()
        || source_ledgers.len() != manifest.source_ledgers.len()
    {
        return Err(AnalyticsError::Catalog(
            "dataset manifest contains duplicate source identities".into(),
        ));
    }
    let logical_rows = manifest
        .shards
        .iter()
        .try_fold(0u64, |sum, shard| {
            if shard.digest == Digest::ZERO || shard.row_count == 0 || shard.group_count == 0 {
                return None;
            }
            sum.checked_add(shard.row_count)
        })
        .ok_or_else(|| AnalyticsError::Catalog("dataset row count overflow".into()))?;
    let decision_groups = manifest
        .shards
        .iter()
        .try_fold(0u64, |sum, shard| sum.checked_add(shard.group_count))
        .ok_or_else(|| AnalyticsError::Catalog("dataset group count overflow".into()))?;
    if logical_rows != manifest.logical_rows || decision_groups != manifest.decision_groups {
        return Err(AnalyticsError::Catalog(
            "dataset manifest totals do not reconcile with its shards".into(),
        ));
    }
    let manifest_shard_ids: BTreeSet<_> =
        manifest.shards.iter().map(|shard| shard.shard_id).collect();
    if manifest_shard_ids.len() != manifest.shards.len() {
        return Err(AnalyticsError::Catalog(
            "dataset manifest contains duplicate shard identities".into(),
        ));
    }
    let mut actual = BTreeMap::new();
    for path in files {
        let shard_id = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_prefix("shard="))
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or_else(|| {
                AnalyticsError::Catalog(format!(
                    "cannot derive shard identity from {}",
                    path.display()
                ))
            })?;
        if actual.insert(shard_id, hash_file(path)?).is_some() {
            return Err(AnalyticsError::Catalog(format!(
                "duplicate parquet file for shard {shard_id}"
            )));
        }
    }
    for shard in &manifest.shards {
        if actual.get(&shard.shard_id) != Some(&shard.digest) {
            return Err(AnalyticsError::Catalog(format!(
                "parquet shard {} is missing or failed digest verification",
                shard.shard_id
            )));
        }
    }
    Ok(())
}

fn hash_file(path: &Path) -> Result<Digest, AnalyticsError> {
    use std::io::Read as _;

    let mut file = std::fs::File::open(path)
        .map_err(|error| AnalyticsError::Catalog(format!("open {}: {error}", path.display())))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            AnalyticsError::Catalog(format!("read {}: {error}", path.display()))
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Digest::from_blake3_bytes(*hasher.finalize().as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_dataset::{DecisionGroup, ParquetCompactor};
    use reflex_types::{
        ActionSchemaId, BuildIdentity, CandidateId, CellId, FeatureSchemaId, StateId,
    };

    #[tokio::test]
    async fn test_analytics_query_execution() {
        let temp = tempfile::tempdir().unwrap();
        let compactor = ParquetCompactor::default();
        let s1 = StateId::from_digest(Digest::hash_blake3(b"s1"));
        let c1 = CandidateId::from_digest(Digest::hash_blake3(b"c1"));
        let groups = vec![DecisionGroup {
            state_id: s1,
            candidate_ids: vec![c1],
            labels: vec![reflex_dataset::CandidateKnowledge::Unknown],
            feature_ref: Digest::hash_blake3(b"f"),
            source_episodes: vec![],
            coverage: "observed".into(),
        }];
        let (manifest, published) = compactor
            .compact_and_publish(
                &groups,
                temp.path(),
                vec![CellId::from_digest(Digest::hash_blake3(b"cell"))],
                vec![Digest::hash_blake3(b"ledger")],
                ParquetCompactor::decisions_schema_id(),
                FeatureSchemaId::from_digest(Digest::hash_blake3(b"feat")),
                ActionSchemaId::from_digest(Digest::hash_blake3(b"act")),
                Digest::hash_blake3(b"split"),
                BuildIdentity::from_digest(Digest::hash_blake3(b"compiler")),
            )
            .unwrap();

        let mut session =
            AnalyticsSession::new(512 * 1024 * 1024, temp.path().join("spill")).unwrap();
        session
            .register_dataset_manifest("decisions", &manifest, &published)
            .await
            .unwrap();
        session.register_sql_report(
            "0001_solve_rates",
            "SELECT label_code FROM decisions LIMIT 10",
        );
        let local = session
            .execute_query_async("SELECT label_code FROM decisions LIMIT 10")
            .await
            .unwrap();
        assert!(!local.rows.is_empty());
        assert!(local.sql_digest.is_some());

        let unrelated = session
            .execute_query_async("SELECT COUNT(*) FROM decisions")
            .await
            .unwrap();
        assert!(unrelated.sql_digest.is_none());

        session
            .register_local_parquet_table("decisions_local", &published)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn rejects_corrupted_published_shard() {
        use std::io::Write as _;

        let temp = tempfile::tempdir().unwrap();
        let compactor = ParquetCompactor::default();
        let groups = vec![DecisionGroup {
            state_id: StateId::from_digest(Digest::hash_blake3(b"state")),
            candidate_ids: vec![CandidateId::from_digest(Digest::hash_blake3(b"candidate"))],
            labels: vec![reflex_dataset::CandidateKnowledge::Unknown],
            feature_ref: Digest::hash_blake3(b"feature"),
            source_episodes: vec![],
            coverage: "observed".into(),
        }];
        let (manifest, published) = compactor
            .compact_and_publish(
                &groups,
                temp.path(),
                vec![CellId::from_digest(Digest::hash_blake3(b"cell"))],
                vec![Digest::hash_blake3(b"ledger")],
                ParquetCompactor::decisions_schema_id(),
                FeatureSchemaId::from_digest(Digest::hash_blake3(b"features")),
                ActionSchemaId::from_digest(Digest::hash_blake3(b"actions")),
                Digest::hash_blake3(b"split"),
                BuildIdentity::from_digest(Digest::hash_blake3(b"compiler")),
            )
            .unwrap();
        let shard = discover_parquet_files(&published).unwrap().remove(0);
        std::fs::OpenOptions::new()
            .append(true)
            .open(shard)
            .unwrap()
            .write_all(b"corrupt")
            .unwrap();

        let session = AnalyticsSession::new(64 * 1024 * 1024, temp.path().join("spill")).unwrap();
        let error = session
            .register_dataset_manifest("decisions", &manifest, &published)
            .await
            .unwrap_err();
        assert!(matches!(error, AnalyticsError::Catalog(_)));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_in_dataset_tree() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        symlink(outside.path(), temp.path().join("part.parquet")).unwrap();
        let error = discover_parquet_files(temp.path()).unwrap_err();
        assert!(matches!(error, AnalyticsError::Catalog(_)));
    }

    #[test]
    fn rejects_unquoted_sql_table_injection() {
        assert!(validate_table_name("decisions; DROP TABLE cells").is_err());
        assert!(validate_table_name("9decisions").is_err());
        assert!(validate_table_name("decisions_v1").is_ok());
    }
}
