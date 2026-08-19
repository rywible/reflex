use async_trait::async_trait;
use bytes::Bytes;
use futures::Stream;
use futures_util::StreamExt;
use reflex_types::{Digest, DigestAlgorithm};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write as IoWrite};
use std::ops::Range;
use std::path::PathBuf;
use std::pin::Pin;
use thiserror::Error;
use tokio::sync::RwLock;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    #[error("io error: {0}")]
    Io(String),
    #[error("digest mismatch: expected {expected}, calculated {calculated}")]
    DigestMismatch {
        expected: Digest,
        calculated: Digest,
    },
    #[error("object not found: {0}")]
    NotFound(Digest),
    #[error("gc token required")]
    GcTokenRequired,
    #[error("store closed")]
    Closed,
    #[error("corrupt object at digest {0}")]
    CorruptObject(Digest),
    #[error("invalid range {start}..{end} for size {size}")]
    InvalidRange { start: u64, end: u64, size: u64 },
    #[error("unauthorized delete for retention class {0:?}")]
    UnauthorizedDelete(RetentionClass),
    #[error("manifest error: {0}")]
    ManifestError(String),
}

impl From<std::io::Error> for StoreError {
    fn from(e: std::io::Error) -> Self {
        StoreError::Io(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Read verification
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReadVerification {
    None,
    Expected { digest: Digest },
}

// ---------------------------------------------------------------------------
// Retention classes  (§7.4)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RetentionClass {
    /// Never automatically deleted – accepted ledgers, receipts, reports.
    EvidencePermanent,
    /// Retained while supported/pinned – promoted models, knowledge editions.
    Release,
    /// Retained while reachable – current experiment inputs/outputs.
    Active,
    /// Evictable – regenerated indexes, local mirrors.
    Cache,
    /// Grace-period GC – failed upload parts, scratch bundles.
    Ephemeral,
}

// ---------------------------------------------------------------------------
// Object metadata and stored-object handle
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectMeta {
    pub digest: Digest,
    pub size_bytes: u64,
    pub retention: RetentionClass,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredObject {
    pub digest: Digest,
    pub size_bytes: u64,
}

// ---------------------------------------------------------------------------
// GC token
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcToken(pub String);

// ---------------------------------------------------------------------------
// ArtifactStore trait  (§7.3)
// ---------------------------------------------------------------------------

#[async_trait]
pub trait ArtifactStore: Send + Sync {
    async fn put_stream(
        &self,
        expected: Option<Digest>,
        stream: Pin<Box<dyn Stream<Item = Result<Bytes, StoreError>> + Send>>,
        retention: RetentionClass,
    ) -> Result<StoredObject, StoreError>;

    async fn put_bytes(
        &self,
        expected: Option<Digest>,
        bytes: Bytes,
        retention: RetentionClass,
    ) -> Result<StoredObject, StoreError> {
        let stream = Box::pin(futures::stream::once(async move { Ok(bytes) }));
        self.put_stream(expected, stream, retention).await
    }

    async fn head(&self, digest: Digest) -> Result<Option<ObjectMeta>, StoreError>;

    async fn get_range(
        &self,
        digest: Digest,
        range: Range<u64>,
        verification: ReadVerification,
    ) -> Result<Bytes, StoreError>;

    async fn get_bytes(&self, digest: Digest) -> Result<Bytes, StoreError> {
        let meta = self
            .head(digest)
            .await?
            .ok_or(StoreError::NotFound(digest))?;
        self.get_range(digest, 0..meta.size_bytes, ReadVerification::None)
            .await
    }

    async fn delete_ephemeral(&self, digest: Digest, token: GcToken) -> Result<(), StoreError>;
}

// ---------------------------------------------------------------------------
// FsArtifactStore  (§7.3 – local publication algorithm)
//
// Path layout: {root}/objects/{hex[0..2]}/{hex[2..]}
// Meta   file: {root}/objects/{hex[0..2]}/{hex[2..]}.meta
// ---------------------------------------------------------------------------

pub struct FsArtifactStore {
    root: PathBuf,
    meta_cache: RwLock<HashMap<Digest, ObjectMeta>>,
}

impl FsArtifactStore {
    pub fn new(root: PathBuf) -> Result<Self, StoreError> {
        fs::create_dir_all(root.join("objects"))?;
        fs::create_dir_all(root.join("scratch"))?;
        Ok(Self {
            root,
            meta_cache: RwLock::new(HashMap::new()),
        })
    }

    pub fn digest_path(&self, digest: &Digest) -> PathBuf {
        let hex = digest_hex(digest);
        let prefix = &hex[0..2];
        let rest = &hex[2..];
        self.root.join("objects").join(prefix).join(rest)
    }

    pub fn digest_meta_path(&self, digest: &Digest) -> PathBuf {
        let hex = digest_hex(digest);
        let prefix = &hex[0..2];
        let rest = &hex[2..];
        self.root
            .join("objects")
            .join(prefix)
            .join(format!("{rest}.meta"))
    }

    fn parent_dir(&self, digest: &Digest) -> PathBuf {
        let hex = digest_hex(digest);
        let prefix = &hex[0..2];
        self.root.join("objects").join(prefix)
    }

    fn scratch_path(&self) -> PathBuf {
        self.root
            .join("scratch")
            .join(format!("upload-{}", uuid::Uuid::new_v4()))
    }

    /// Compute the digest for the given algorithm from accumulated bytes.
    fn finalize_digest(algorithm: DigestAlgorithm, blake3: &blake3::Hasher, sha256: &sha2::Sha256) -> Digest {
        match algorithm {
            DigestAlgorithm::Blake3 => Digest::from_blake3_bytes(*blake3.finalize().as_bytes()),
            DigestAlgorithm::Sha256 => {
                use sha2::Digest as Sha2Digest;
                let res = sha256.clone().finalize();
                let mut b = [0u8; 32];
                b.copy_from_slice(&res);
                Digest::from_sha256_bytes(b)
            }
        }
    }

    /// Read the .meta file for an existing object, if present.
    fn read_meta_file(&self, digest: &Digest) -> Option<ObjectMeta> {
        let meta_path = self.digest_meta_path(digest);
        fs::read(&meta_path)
            .ok()
            .and_then(|b| serde_json::from_slice::<ObjectMeta>(&b).ok())
    }

    /// Write the .meta file for an object.
    fn write_meta_file(&self, meta: &ObjectMeta) {
        if let Ok(meta_json) = serde_json::to_vec(meta) {
            let _ = fs::write(&self.digest_meta_path(&meta.digest), meta_json);
        }
    }

    /// Read full file contents for verification.
    fn read_full_file(path: &std::path::Path) -> Result<Vec<u8>, StoreError> {
        let mut file = File::open(path)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok(buf)
    }

    /// Verify the destination already exists and matches the expected digest.
    /// Returns Ok(true) if verified, Ok(false) if destination does not exist.
    fn verify_existing_destination(
        &self,
        final_path: &std::path::Path,
        calculated: Digest,
    ) -> Result<bool, StoreError> {
        if !final_path.exists() {
            return Ok(false);
        }

        let contents = Self::read_full_file(final_path)?;
        let existing = match calculated.algorithm {
            DigestAlgorithm::Sha256 => Digest::hash_sha256(&contents),
            DigestAlgorithm::Blake3 => Digest::hash_blake3(&contents),
        };

        if existing == calculated {
            Ok(true)
        } else {
            // Destination is corrupt or different content; overwrite it.
            Ok(false)
        }
    }
}

/// Hex-encode the raw digest bytes.
fn digest_hex(digest: &Digest) -> String {
    hex::encode(digest.bytes)
}

#[async_trait]
impl ArtifactStore for FsArtifactStore {
    /// §7.3 local publication algorithm:
    /// 1. create random temp file
    /// 2. stream bytes while hashing (BLAKE3) and counting
    /// 3. flush + fsync temp file
    /// 4. compare expected digest when supplied
    /// 5. create parent fanout directories
    /// 6. atomically rename to digest path
    /// 7. fsync parent directory
    /// 8. if destination already exists, verify size/digest and discard temp
    async fn put_stream(
        &self,
        expected: Option<Digest>,
        mut stream: Pin<Box<dyn Stream<Item = Result<Bytes, StoreError>> + Send>>,
        retention: RetentionClass,
    ) -> Result<StoredObject, StoreError> {
        // Step 1: create random temp file.
        let temp_path = self.scratch_path();
        let mut temp_file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp_path)?;

        // Determine algorithm from expected digest.
        let algorithm = expected
            .map(|d| d.algorithm)
            .unwrap_or(DigestAlgorithm::Blake3);

        let mut blake3_hasher = blake3::Hasher::new();
        let mut sha256_hasher = sha2::Sha256::new();
        let use_sha256 = algorithm == DigestAlgorithm::Sha256;
        let mut total_bytes = 0u64;

        // Step 2: stream bytes while hashing and counting.
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if use_sha256 {
                use sha2::Digest as Sha2Digest;
                sha256_hasher.update(&chunk);
            } else {
                blake3_hasher.update(&chunk);
            }
            temp_file.write_all(&chunk)?;
            total_bytes += chunk.len() as u64;
        }

        // Step 3: flush and fsync.
        temp_file.flush()?;
        temp_file.sync_all()?;
        drop(temp_file);

        // Compute the digest.
        let calculated = Self::finalize_digest(algorithm, &blake3_hasher, &sha256_hasher);

        // Step 4: compare expected digest when supplied.
        if let Some(exp) = expected {
            if exp != calculated {
                let _ = fs::remove_file(&temp_path);
                return Err(StoreError::DigestMismatch {
                    expected: exp,
                    calculated,
                });
            }
        }

        // Determine final retention – existing EvidencePermanent/Release wins.
        let mut final_retention = retention;
        if let Some(existing_meta) = self.read_meta_file(&calculated) {
            if matches!(
                existing_meta.retention,
                RetentionClass::EvidencePermanent | RetentionClass::Release
            ) {
                final_retention = existing_meta.retention;
            }
        }

        let final_path = self.digest_path(&calculated);

        // Step 5 + 8: create fanout dirs; if destination exists verify & discard temp.
        // Step 8 first – check if destination already exists.
        if self.verify_existing_destination(&final_path, calculated)? {
            // Destination verified – discard temp.
            let _ = fs::remove_file(&temp_path);

            // Still update meta if needed.
            let meta = ObjectMeta {
                digest: calculated,
                size_bytes: total_bytes,
                retention: final_retention,
            };
            self.write_meta_file(&meta);
            self.meta_cache.write().await.insert(calculated, meta.clone());

            return Ok(StoredObject {
                digest: calculated,
                size_bytes: total_bytes,
            });
        }

        // Step 5: create parent fanout directories.
        let target_dir = self.parent_dir(&calculated);
        fs::create_dir_all(&target_dir)?;

        // Step 6: atomically rename to the digest path.
        fs::rename(&temp_path, &final_path)?;

        // Step 7: fsync the parent directory.
        if let Ok(dir_file) = File::open(&target_dir) {
            let _ = dir_file.sync_all();
        }

        // Persist metadata.
        let meta = ObjectMeta {
            digest: calculated,
            size_bytes: total_bytes,
            retention: final_retention,
        };
        self.write_meta_file(&meta);
        self.meta_cache.write().await.insert(calculated, meta);

        Ok(StoredObject {
            digest: calculated,
            size_bytes: total_bytes,
        })
    }

    async fn head(&self, digest: Digest) -> Result<Option<ObjectMeta>, StoreError> {
        // Fast path: in-memory cache.
        if let Some(meta) = self.meta_cache.read().await.get(&digest) {
            return Ok(Some(meta.clone()));
        }

        let path = self.digest_path(&digest);
        if !path.exists() {
            return Ok(None);
        }

        // Try to read persisted metadata.
        if let Some(obj_meta) = self.read_meta_file(&digest) {
            self.meta_cache
                .write()
                .await
                .insert(digest, obj_meta.clone());
            return Ok(Some(obj_meta));
        }

        // Fall back to filesystem metadata.
        let file_meta = fs::metadata(&path)?;
        let obj_meta = ObjectMeta {
            digest,
            size_bytes: file_meta.len(),
            retention: RetentionClass::Active,
        };
        self.meta_cache
            .write()
            .await
            .insert(digest, obj_meta.clone());
        Ok(Some(obj_meta))
    }

    async fn get_range(
        &self,
        digest: Digest,
        range: Range<u64>,
        verification: ReadVerification,
    ) -> Result<Bytes, StoreError> {
        let path = self.digest_path(&digest);
        if !path.exists() {
            return Err(StoreError::NotFound(digest));
        }

        // Verify against expected digest if requested.
        match verification {
            ReadVerification::None => {}
            ReadVerification::Expected { digest: expected } => {
                let contents = Self::read_full_file(&path)?;
                let computed = match expected.algorithm {
                    DigestAlgorithm::Blake3 => Digest::hash_blake3(&contents),
                    DigestAlgorithm::Sha256 => Digest::hash_sha256(&contents),
                };
                if computed != expected {
                    return Err(StoreError::DigestMismatch {
                        expected,
                        calculated: computed,
                    });
                }
                // Verified – return the requested range from the buffer we already
                // have in memory.
                if range.start > range.end || range.end > contents.len() as u64 {
                    return Err(StoreError::InvalidRange {
                        start: range.start,
                        end: range.end,
                        size: contents.len() as u64,
                    });
                }
                return Ok(Bytes::from(
                    contents[(range.start as usize)..(range.end as usize)].to_vec(),
                ));
            }
        }

        // Normal path without full-file verification.
        let mut file = File::open(&path)?;
        let total_size = file.metadata()?.len();

        if range.start > range.end || range.end > total_size {
            return Err(StoreError::InvalidRange {
                start: range.start,
                end: range.end,
                size: total_size,
            });
        }

        let len = (range.end - range.start) as usize;
        let mut buffer = vec![0u8; len];
        file.seek(SeekFrom::Start(range.start))?;
        file.read_exact(&mut buffer)?;

        Ok(Bytes::from(buffer))
    }

    async fn delete_ephemeral(&self, digest: Digest, _token: GcToken) -> Result<(), StoreError> {
        // Check retention class via metadata.
        match self.head(digest).await? {
            Some(meta)
                if matches!(
                    meta.retention,
                    RetentionClass::EvidencePermanent | RetentionClass::Release
                ) =>
            {
                return Err(StoreError::UnauthorizedDelete(meta.retention));
            }
            _ => {}
        }

        let path = self.digest_path(&digest);
        let meta_path = self.digest_meta_path(&digest);

        if path.exists() {
            fs::remove_file(path)?;
        }
        if meta_path.exists() {
            let _ = fs::remove_file(meta_path);
        }
        self.meta_cache.write().await.remove(&digest);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MemoryArtifactStore
// ---------------------------------------------------------------------------

pub struct MemoryArtifactStore {
    objects: RwLock<HashMap<Digest, (Bytes, ObjectMeta)>>,
}

impl MemoryArtifactStore {
    pub fn new() -> Self {
        Self {
            objects: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for MemoryArtifactStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ArtifactStore for MemoryArtifactStore {
    async fn put_stream(
        &self,
        expected: Option<Digest>,
        mut stream: Pin<Box<dyn Stream<Item = Result<Bytes, StoreError>> + Send>>,
        retention: RetentionClass,
    ) -> Result<StoredObject, StoreError> {
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk?);
        }
        let calculated = match expected {
            Some(exp) if exp.algorithm == DigestAlgorithm::Sha256 => Digest::hash_sha256(&buf),
            _ => Digest::hash_blake3(&buf),
        };
        match expected {
            Some(exp) if exp != calculated => {
                return Err(StoreError::DigestMismatch {
                    expected: exp,
                    calculated,
                });
            }
            _ => {}
        }
        let bytes = Bytes::from(buf);
        let size_bytes = bytes.len() as u64;
        let meta = ObjectMeta {
            digest: calculated,
            size_bytes,
            retention,
        };
        self.objects.write().await.insert(calculated, (bytes, meta));
        Ok(StoredObject {
            digest: calculated,
            size_bytes,
        })
    }

    async fn head(&self, digest: Digest) -> Result<Option<ObjectMeta>, StoreError> {
        Ok(self
            .objects
            .read()
            .await
            .get(&digest)
            .map(|(_, m)| m.clone()))
    }

    async fn get_range(
        &self,
        digest: Digest,
        range: Range<u64>,
        verification: ReadVerification,
    ) -> Result<Bytes, StoreError> {
        let guard = self.objects.read().await;
        let (bytes, _) = guard.get(&digest).ok_or(StoreError::NotFound(digest))?;

        match verification {
            ReadVerification::None => {}
            ReadVerification::Expected { digest: expected } => {
                let computed = match expected.algorithm {
                    DigestAlgorithm::Blake3 => Digest::hash_blake3(bytes),
                    DigestAlgorithm::Sha256 => Digest::hash_sha256(bytes),
                };
                if computed != expected {
                    return Err(StoreError::DigestMismatch {
                        expected,
                        calculated: computed,
                    });
                }
            }
        }

        if range.start > range.end || range.end > bytes.len() as u64 {
            return Err(StoreError::InvalidRange {
                start: range.start,
                end: range.end,
                size: bytes.len() as u64,
            });
        }
        Ok(bytes.slice((range.start as usize)..(range.end as usize)))
    }

    async fn delete_ephemeral(&self, digest: Digest, _token: GcToken) -> Result<(), StoreError> {
        let mut guard = self.objects.write().await;
        match guard.get(&digest) {
            Some((_, meta))
                if matches!(
                    meta.retention,
                    RetentionClass::EvidencePermanent | RetentionClass::Release
                ) =>
            {
                return Err(StoreError::UnauthorizedDelete(meta.retention));
            }
            _ => {}
        }
        guard.remove(&digest);
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ObjectStoreArtifactStore  (thin wrapper over memory for now)
// ---------------------------------------------------------------------------

pub struct ObjectStoreArtifactStore {
    endpoint: String,
    bucket: String,
    region: String,
    inner: MemoryArtifactStore,
}

impl ObjectStoreArtifactStore {
    pub fn new(endpoint: String, bucket: String, region: String) -> Self {
        Self {
            endpoint,
            bucket,
            region,
            inner: MemoryArtifactStore::new(),
        }
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn bucket(&self) -> &str {
        &self.bucket
    }

    pub fn region(&self) -> &str {
        &self.region
    }
}

#[async_trait]
impl ArtifactStore for ObjectStoreArtifactStore {
    async fn put_stream(
        &self,
        expected: Option<Digest>,
        stream: Pin<Box<dyn Stream<Item = Result<Bytes, StoreError>> + Send>>,
        retention: RetentionClass,
    ) -> Result<StoredObject, StoreError> {
        self.inner.put_stream(expected, stream, retention).await
    }

    async fn head(&self, digest: Digest) -> Result<Option<ObjectMeta>, StoreError> {
        self.inner.head(digest).await
    }

    async fn get_range(
        &self,
        digest: Digest,
        range: Range<u64>,
        verification: ReadVerification,
    ) -> Result<Bytes, StoreError> {
        self.inner.get_range(digest, range, verification).await
    }

    async fn delete_ephemeral(&self, digest: Digest, token: GcToken) -> Result<(), StoreError> {
        self.inner.delete_ephemeral(digest, token).await
    }
}

// ---------------------------------------------------------------------------
// ChunkManifest  (§7.3 – fixed 64 MiB chunks for large objects)
// ---------------------------------------------------------------------------

pub const CHUNK_SIZE_BYTES: u64 = 64 * 1024 * 1024; // 64 MiB

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkEntry {
    pub index: u32,
    pub offset: u64,
    pub length: u64,
    pub digest: Digest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkManifest {
    pub schema: String,
    pub total_bytes: u64,
    pub chunk_size: u64,
    pub root_digest: Digest,
    pub chunks: Vec<ChunkEntry>,
}

impl ChunkManifest {
    pub fn build(total_bytes: u64, chunk_size: u64, chunks: Vec<ChunkEntry>) -> Self {
        let mut manifest_bytes = Vec::new();
        manifest_bytes.extend_from_slice(&total_bytes.to_le_bytes());
        manifest_bytes.extend_from_slice(&chunk_size.to_le_bytes());
        for c in &chunks {
            manifest_bytes.extend_from_slice(&c.index.to_le_bytes());
            manifest_bytes.extend_from_slice(&c.offset.to_le_bytes());
            manifest_bytes.extend_from_slice(&c.length.to_le_bytes());
            manifest_bytes.extend_from_slice(&c.digest.bytes);
        }
        let root_digest = Digest::hash_blake3(&manifest_bytes);
        Self {
            schema: "reflex.chunk_manifest.v1".to_string(),
            total_bytes,
            chunk_size,
            root_digest,
            chunks,
        }
    }

    pub fn verify(&self) -> bool {
        let mut manifest_bytes = Vec::new();
        manifest_bytes.extend_from_slice(&self.total_bytes.to_le_bytes());
        manifest_bytes.extend_from_slice(&self.chunk_size.to_le_bytes());
        for c in &self.chunks {
            manifest_bytes.extend_from_slice(&c.index.to_le_bytes());
            manifest_bytes.extend_from_slice(&c.offset.to_le_bytes());
            manifest_bytes.extend_from_slice(&c.length.to_le_bytes());
            manifest_bytes.extend_from_slice(&c.digest.bytes);
        }
        let computed = Digest::hash_blake3(&manifest_bytes);
        computed == self.root_digest
    }
}

// ---------------------------------------------------------------------------
// Reachability walker (used by GC)
// ---------------------------------------------------------------------------

pub struct ReachabilityWalker {
    roots: HashSet<Digest>,
    edges: HashMap<Digest, Vec<Digest>>,
}

impl ReachabilityWalker {
    pub fn new(roots: HashSet<Digest>) -> Self {
        Self {
            roots,
            edges: HashMap::new(),
        }
    }

    pub fn add_reference(&mut self, source: Digest, target: Digest) {
        self.edges.entry(source).or_default().push(target);
    }

    pub fn mark_all_reachable(&self) -> HashSet<Digest> {
        let mut reachable = self.roots.clone();
        let mut queue: Vec<Digest> = self.roots.iter().copied().collect();

        while let Some(curr) = queue.pop() {
            if let Some(targets) = self.edges.get(&curr) {
                for &target in targets {
                    if reachable.insert(target) {
                        queue.push(target);
                    }
                }
            }
        }
        reachable
    }
}

// ---------------------------------------------------------------------------
// Two-phase garbage collection  (§7.4)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GcReport {
    pub scanned: usize,
    pub retained: usize,
    pub deleted: usize,
    pub permanent_preserved: usize,
    pub errors: Vec<String>,
}

/// Metadata-reachability based two-phase GC.
///
/// Phase 1 – scan: classify every object by retention class and reachability.
/// Phase 2 – sweep: delete only Cache/Ephemeral objects not in the reachable set.
pub async fn run_two_phase_gc(
    store: &dyn ArtifactStore,
    all_objects: &[ObjectMeta],
    reachable: &HashSet<Digest>,
    dry_run: bool,
) -> GcReport {
    let mut report = GcReport::default();
    let token = GcToken("gc-cycle".to_string());

    for obj in all_objects {
        report.scanned += 1;

        // EvidencePermanent and Release are never swept.
        if obj.retention == RetentionClass::EvidencePermanent
            || obj.retention == RetentionClass::Release
        {
            report.permanent_preserved += 1;
            report.retained += 1;
            continue;
        }

        if reachable.contains(&obj.digest) {
            // Active or reachable Cache/Ephemeral – keep.
            report.retained += 1;
        } else if !dry_run {
            match store.delete_ephemeral(obj.digest, token.clone()).await {
                Ok(()) => report.deleted += 1,
                Err(e) => report.errors.push(e.to_string()),
            }
        } else {
            report.deleted += 1;
        }
    }

    report
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_fs_artifact_store_put_get() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = FsArtifactStore::new(temp_dir.path().to_path_buf()).unwrap();

        let payload = Bytes::from_static(b"Hello CAS world from reflex!");
        let stored = store
            .put_bytes(None, payload.clone(), RetentionClass::Active)
            .await
            .unwrap();

        let read_bytes = store.get_bytes(stored.digest).await.unwrap();
        assert_eq!(payload, read_bytes);

        let head = store.head(stored.digest).await.unwrap().unwrap();
        assert_eq!(head.size_bytes, payload.len() as u64);
    }

    #[tokio::test]
    async fn test_fs_artifact_store_expected_digest() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = FsArtifactStore::new(temp_dir.path().to_path_buf()).unwrap();

        let payload = Bytes::from_static(b"verify me");
        let expected = Digest::hash_blake3(&payload);

        let stored = store
            .put_bytes(Some(expected), payload.clone(), RetentionClass::Active)
            .await
            .unwrap();
        assert_eq!(stored.digest, expected);

        let read_back = store.get_bytes(expected).await.unwrap();
        assert_eq!(read_back, payload);
    }

    #[tokio::test]
    async fn test_fs_artifact_store_digest_mismatch() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = FsArtifactStore::new(temp_dir.path().to_path_buf()).unwrap();

        let payload = Bytes::from_static(b"wrong digest");
        let wrong_digest = Digest::hash_blake3(b"something else entirely");

        let err = store
            .put_bytes(Some(wrong_digest), payload, RetentionClass::Active)
            .await
            .unwrap_err();

        assert!(matches!(err, StoreError::DigestMismatch { .. }));
    }

    #[tokio::test]
    async fn test_fs_artifact_store_read_verification() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = FsArtifactStore::new(temp_dir.path().to_path_buf()).unwrap();

        let payload = Bytes::from_static(b"verify read range");
        let stored = store
            .put_bytes(None, payload.clone(), RetentionClass::Active)
            .await
            .unwrap();

        // Read with verification: "verify read range"[7..17] = "read range"
        let range_bytes = store
            .get_range(
                stored.digest,
                7..17,
                ReadVerification::Expected {
                    digest: stored.digest,
                },
            )
            .await
            .unwrap();
        assert_eq!(&range_bytes[..], b"read range");

        // Wrong verification digest should fail.
        let wrong = Digest::hash_blake3(b"nope");
        let err = store
            .get_range(
                stored.digest,
                0..4,
                ReadVerification::Expected { digest: wrong },
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::DigestMismatch { .. }));
    }

    #[tokio::test]
    async fn test_fs_artifact_store_idempotent_put() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = FsArtifactStore::new(temp_dir.path().to_path_buf()).unwrap();

        let payload = Bytes::from_static(b"identical content");
        let s1 = store
            .put_bytes(None, payload.clone(), RetentionClass::Active)
            .await
            .unwrap();
        let s2 = store
            .put_bytes(None, payload.clone(), RetentionClass::Cache)
            .await
            .unwrap();

        assert_eq!(s1.digest, s2.digest);
        assert_eq!(s1.size_bytes, s2.size_bytes);

        // Object still exists and is readable.
        let read_back = store.get_bytes(s1.digest).await.unwrap();
        assert_eq!(read_back, payload);
    }

    #[tokio::test]
    async fn test_fs_artifact_store_retention_escalation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = FsArtifactStore::new(temp_dir.path().to_path_buf()).unwrap();

        let payload = Bytes::from_static(b"permanent evidence");

        // First put as Cache.
        let s1 = store
            .put_bytes(None, payload.clone(), RetentionClass::Cache)
            .await
            .unwrap();
        let meta1 = store.head(s1.digest).await.unwrap().unwrap();
        assert_eq!(meta1.retention, RetentionClass::Cache);

        // Re-put as EvidencePermanent – should escalate.
        let s2 = store
            .put_bytes(None, payload.clone(), RetentionClass::EvidencePermanent)
            .await
            .unwrap();
        assert_eq!(s1.digest, s2.digest);
        let meta2 = store.head(s2.digest).await.unwrap().unwrap();
        assert_eq!(meta2.retention, RetentionClass::EvidencePermanent);
    }

    #[tokio::test]
    async fn test_fs_artifact_store_delete_ephemeral_protection() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = FsArtifactStore::new(temp_dir.path().to_path_buf()).unwrap();

        let payload = Bytes::from_static(b"protected data");
        let stored = store
            .put_bytes(None, payload, RetentionClass::EvidencePermanent)
            .await
            .unwrap();

        let err = store
            .delete_ephemeral(stored.digest, GcToken("tok".to_string()))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            StoreError::UnauthorizedDelete(RetentionClass::EvidencePermanent)
        );
    }

    #[tokio::test]
    async fn test_chunk_manifest_verification() {
        let c1 = ChunkEntry {
            index: 0,
            offset: 0,
            length: 1024,
            digest: Digest::hash_blake3(b"chunk1"),
        };
        let c2 = ChunkEntry {
            index: 1,
            offset: 1024,
            length: 512,
            digest: Digest::hash_blake3(b"chunk2"),
        };
        let manifest = ChunkManifest::build(1536, 1024, vec![c1, c2]);
        assert!(manifest.verify());
    }

    #[tokio::test]
    async fn test_memory_artifact_store_put_get() {
        let store = MemoryArtifactStore::new();
        let payload = Bytes::from_static(b"memory store test");
        let stored = store
            .put_bytes(None, payload.clone(), RetentionClass::Active)
            .await
            .unwrap();

        let read_back = store.get_bytes(stored.digest).await.unwrap();
        assert_eq!(payload, read_back);

        let head = store.head(stored.digest).await.unwrap().unwrap();
        assert_eq!(head.size_bytes, payload.len() as u64);
    }

    #[tokio::test]
    async fn test_memory_artifact_store_read_verification() {
        let store = MemoryArtifactStore::new();
        let payload = Bytes::from_static(b"verify memory read");
        let stored = store
            .put_bytes(None, payload.clone(), RetentionClass::Active)
            .await
            .unwrap();

        // "verify memory read"[7..16] = "memory re"
        let range_bytes = store
            .get_range(
                stored.digest,
                7..16,
                ReadVerification::Expected {
                    digest: stored.digest,
                },
            )
            .await
            .unwrap();
        assert_eq!(&range_bytes[..], b"memory re");
    }

    #[tokio::test]
    async fn test_reachability_walker() {
        let root = Digest::hash_blake3(b"root");
        let a = Digest::hash_blake3(b"a");
        let b = Digest::hash_blake3(b"b");
        let orphan = Digest::hash_blake3(b"orphan");

        let mut walker = ReachabilityWalker::new(HashSet::from([root]));
        walker.add_reference(root, a);
        walker.add_reference(a, b);

        let reachable = walker.mark_all_reachable();
        assert!(reachable.contains(&root));
        assert!(reachable.contains(&a));
        assert!(reachable.contains(&b));
        assert!(!reachable.contains(&orphan));
    }

    #[tokio::test]
    async fn test_two_phase_gc() {
        let store = MemoryArtifactStore::new();

        let active = store
            .put_bytes(None, Bytes::from_static(b"active"), RetentionClass::Active)
            .await
            .unwrap();
        let cache = store
            .put_bytes(None, Bytes::from_static(b"cache"), RetentionClass::Cache)
            .await
            .unwrap();
        let ephemeral = store
            .put_bytes(None, Bytes::from_static(b"ephemeral"), RetentionClass::Ephemeral)
            .await
            .unwrap();
        let permanent = store
            .put_bytes(None, Bytes::from_static(b"permanent"), RetentionClass::EvidencePermanent)
            .await
            .unwrap();

        let all_objects = vec![
            store.head(active.digest).await.unwrap().unwrap(),
            store.head(cache.digest).await.unwrap().unwrap(),
            store.head(ephemeral.digest).await.unwrap().unwrap(),
            store.head(permanent.digest).await.unwrap().unwrap(),
        ];

        // Only active is reachable.
        let reachable = HashSet::from([active.digest]);

        let report = run_two_phase_gc(&store, &all_objects, &reachable, false).await;
        assert_eq!(report.scanned, 4);
        assert_eq!(report.retained, 2); // active + permanent
        assert_eq!(report.permanent_preserved, 1);
        assert_eq!(report.deleted, 2); // cache + ephemeral

        // Verify deleted objects are gone.
        assert!(store.head(cache.digest).await.unwrap().is_none());
        assert!(store.head(ephemeral.digest).await.unwrap().is_none());
        assert!(store.head(active.digest).await.unwrap().is_some());
        assert!(store.head(permanent.digest).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_two_phase_gc_dry_run() {
        let store = MemoryArtifactStore::new();

        let cache = store
            .put_bytes(None, Bytes::from_static(b"cache"), RetentionClass::Cache)
            .await
            .unwrap();
        let all_objects = vec![store.head(cache.digest).await.unwrap().unwrap()];
        let reachable = HashSet::new();

        let report = run_two_phase_gc(&store, &all_objects, &reachable, true).await;
        assert_eq!(report.deleted, 1);
        // Dry run – object should still exist.
        assert!(store.head(cache.digest).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn test_permanent_evidence_retention_protection() {
        let store = MemoryArtifactStore::new();
        let data = Bytes::from_static(b"precious permanent evidence");
        let stored = store
            .put_bytes(None, data, RetentionClass::EvidencePermanent)
            .await
            .unwrap();

        let err = store
            .delete_ephemeral(stored.digest, GcToken("tok".to_string()))
            .await
            .unwrap_err();
        assert_eq!(
            err,
            StoreError::UnauthorizedDelete(RetentionClass::EvidencePermanent)
        );
    }
}
