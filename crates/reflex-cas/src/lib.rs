use async_trait::async_trait;
use bytes::{Buf, Bytes, BytesMut};
use futures::Stream;
use futures_util::StreamExt;
use reflex_types::{Digest, DigestAlgorithm};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write as IoWrite};
use std::num::NonZeroU64;
use std::ops::Range;
use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
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
    #[error("artifact capacity exceeded: {0}")]
    CapacityExceeded(String),
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
// Path layout: {root}/objects/{algorithm}/{hex[0..2]}/{hex[2..]}
// Meta   file: {root}/objects/{algorithm}/{hex[0..2]}/{hex[2..]}.meta
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
        self.root
            .join("objects")
            .join(digest_algorithm_name(digest.algorithm))
            .join(prefix)
            .join(rest)
    }

    /// Exercise the local store's required write, sync, and cleanup path.
    ///
    /// A lookup for a sentinel digest is not a readiness check: absence is a
    /// valid CAS result.  This probe creates no published object and removes
    /// its scratch file before returning.
    pub fn check_readiness(&self) -> Result<(), StoreError> {
        static PROBE_SEQUENCE: AtomicU64 = AtomicU64::new(1);
        let sequence = PROBE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = self
            .root
            .join("scratch")
            .join(format!(".readiness-{}-{sequence}", std::process::id()));
        let result = (|| -> Result<(), StoreError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(b"reflex-cas-readiness-v1")?;
            file.sync_all()?;
            drop(file);
            fs::remove_file(&path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&path);
        }
        result
    }

    pub fn digest_meta_path(&self, digest: &Digest) -> PathBuf {
        let hex = digest_hex(digest);
        let prefix = &hex[0..2];
        let rest = &hex[2..];
        self.root
            .join("objects")
            .join(digest_algorithm_name(digest.algorithm))
            .join(prefix)
            .join(format!("{rest}.meta"))
    }

    fn parent_dir(&self, digest: &Digest) -> PathBuf {
        let hex = digest_hex(digest);
        let prefix = &hex[0..2];
        self.root
            .join("objects")
            .join(digest_algorithm_name(digest.algorithm))
            .join(prefix)
    }

    fn scratch_path(&self) -> PathBuf {
        self.root
            .join("scratch")
            .join(format!("upload-{}", uuid::Uuid::new_v4()))
    }

    /// Compute the digest for the given algorithm from accumulated bytes.
    fn finalize_digest(
        algorithm: DigestAlgorithm,
        blake3: &blake3::Hasher,
        sha256: &sha2::Sha256,
    ) -> Digest {
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

    /// Read and validate the metadata for an existing object, if present.
    fn read_meta_file(&self, digest: &Digest) -> Result<Option<ObjectMeta>, StoreError> {
        let meta_path = self.digest_meta_path(digest);
        let bytes = match fs::read(&meta_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let meta: ObjectMeta = serde_json::from_slice(&bytes).map_err(|error| {
            StoreError::ManifestError(format!("invalid object metadata: {error}"))
        })?;
        if meta.digest != *digest {
            return Err(StoreError::ManifestError(format!(
                "metadata digest {} does not match object {digest}",
                meta.digest
            )));
        }
        Ok(Some(meta))
    }

    /// Atomically and durably write the metadata for an object.
    fn write_meta_file(&self, meta: &ObjectMeta) -> Result<(), StoreError> {
        let meta_json =
            serde_json::to_vec(meta).map_err(|e| StoreError::Io(format!("meta json: {e}")))?;
        let final_path = self.digest_meta_path(&meta.digest);
        let parent = final_path
            .parent()
            .ok_or_else(|| StoreError::Io("object metadata path has no parent directory".into()))?;
        fs::create_dir_all(parent)?;
        let temp_path = parent.join(format!(".meta-{}.tmp", uuid::Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp_path)?;
        if let Err(error) = (|| -> Result<(), std::io::Error> {
            file.write_all(&meta_json)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temp_path, &final_path)?;
            File::open(parent)?.sync_all()
        })() {
            let _ = fs::remove_file(&temp_path);
            return Err(error.into());
        }
        Ok(())
    }

    /// Hash a file with bounded memory.
    fn hash_file(
        path: &std::path::Path,
        algorithm: DigestAlgorithm,
    ) -> Result<(Digest, u64), StoreError> {
        let mut file = File::open(path)?;
        let mut buffer = [0u8; 64 * 1024];
        let mut blake3 = blake3::Hasher::new();
        let mut sha256 = sha2::Sha256::new();
        let mut size = 0u64;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            size = size
                .checked_add(read as u64)
                .ok_or_else(|| StoreError::Io("object size overflow".to_string()))?;
            match algorithm {
                DigestAlgorithm::Blake3 => {
                    blake3.update(&buffer[..read]);
                }
                DigestAlgorithm::Sha256 => {
                    sha2::Digest::update(&mut sha256, &buffer[..read]);
                }
            }
        }
        Ok((Self::finalize_digest(algorithm, &blake3, &sha256), size))
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

        let (existing, _) = Self::hash_file(final_path, calculated.algorithm)?;

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

fn digest_algorithm_name(algorithm: DigestAlgorithm) -> &'static str {
    match algorithm {
        DigestAlgorithm::Blake3 => "blake3",
        DigestAlgorithm::Sha256 => "sha256",
    }
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
        if let Some(exp) = expected
            && exp != calculated
        {
            let _ = fs::remove_file(&temp_path);
            return Err(StoreError::DigestMismatch {
                expected: exp,
                calculated,
            });
        }

        // Determine final retention – existing EvidencePermanent/Release wins.
        let mut final_retention = retention;
        if let Some(existing_meta) = self.read_meta_file(&calculated)?
            && matches!(
                existing_meta.retention,
                RetentionClass::EvidencePermanent | RetentionClass::Release
            )
        {
            final_retention = existing_meta.retention;
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
            self.write_meta_file(&meta)?;
            self.meta_cache
                .write()
                .await
                .insert(calculated, meta.clone());

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
        let dir_file = File::open(&target_dir)?;
        dir_file.sync_all()?;

        // Persist metadata.
        let meta = ObjectMeta {
            digest: calculated,
            size_bytes: total_bytes,
            retention: final_retention,
        };
        self.write_meta_file(&meta)?;
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
        if let Some(obj_meta) = self.read_meta_file(&digest)? {
            let file_size = fs::metadata(&path)?.len();
            if obj_meta.size_bytes != file_size {
                return Err(StoreError::ManifestError(format!(
                    "metadata size {} does not match object size {file_size}",
                    obj_meta.size_bytes
                )));
            }
            self.meta_cache
                .write()
                .await
                .insert(digest, obj_meta.clone());
            return Ok(Some(obj_meta));
        }

        Err(StoreError::ManifestError(format!(
            "object {digest} is missing durable retention metadata"
        )))
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
                let (computed, size) = Self::hash_file(&path, expected.algorithm)?;
                if computed != expected {
                    return Err(StoreError::DigestMismatch {
                        expected,
                        calculated: computed,
                    });
                }
                if range.start > range.end || range.end > size {
                    return Err(StoreError::InvalidRange {
                        start: range.start,
                        end: range.end,
                        size,
                    });
                }
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
// Bounded immutable in-memory artifact arena
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactArenaLimits {
    pub total_bytes: u64,
    pub per_object_bytes: u64,
    pub evidence_permanent_bytes: u64,
    pub release_bytes: u64,
    pub active_bytes: u64,
    pub cache_bytes: u64,
    pub ephemeral_bytes: u64,
}

impl ArtifactArenaLimits {
    pub fn validate(self) -> Result<Self, StoreError> {
        if self.total_bytes == 0
            || self.per_object_bytes == 0
            || self.per_object_bytes > self.total_bytes
            || [
                self.evidence_permanent_bytes,
                self.release_bytes,
                self.active_bytes,
                self.cache_bytes,
                self.ephemeral_bytes,
            ]
            .into_iter()
            .any(|cap| cap == 0 || cap > self.total_bytes)
        {
            return Err(StoreError::CapacityExceeded(
                "arena caps must be non-zero and no larger than total_bytes".into(),
            ));
        }
        Ok(self)
    }

    fn retention_cap(self, retention: RetentionClass) -> u64 {
        match retention {
            RetentionClass::EvidencePermanent => self.evidence_permanent_bytes,
            RetentionClass::Release => self.release_bytes,
            RetentionClass::Active => self.active_bytes,
            RetentionClass::Cache => self.cache_bytes,
            RetentionClass::Ephemeral => self.ephemeral_bytes,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArtifactArenaUsage {
    pub total_bytes: u64,
    pub evidence_permanent_bytes: u64,
    pub release_bytes: u64,
    pub active_bytes: u64,
    pub cache_bytes: u64,
    pub ephemeral_bytes: u64,
    pub objects: usize,
}

impl ArtifactArenaUsage {
    fn retention_bytes(&self, retention: RetentionClass) -> u64 {
        match retention {
            RetentionClass::EvidencePermanent => self.evidence_permanent_bytes,
            RetentionClass::Release => self.release_bytes,
            RetentionClass::Active => self.active_bytes,
            RetentionClass::Cache => self.cache_bytes,
            RetentionClass::Ephemeral => self.ephemeral_bytes,
        }
    }

    fn retention_bytes_mut(&mut self, retention: RetentionClass) -> &mut u64 {
        match retention {
            RetentionClass::EvidencePermanent => &mut self.evidence_permanent_bytes,
            RetentionClass::Release => &mut self.release_bytes,
            RetentionClass::Active => &mut self.active_bytes,
            RetentionClass::Cache => &mut self.cache_bytes,
            RetentionClass::Ephemeral => &mut self.ephemeral_bytes,
        }
    }
}

struct ArenaEntry {
    bytes: Arc<[u8]>,
    meta: ObjectMeta,
}

#[derive(Default)]
struct ArenaState {
    objects: HashMap<Digest, ArenaEntry>,
    usage: ArtifactArenaUsage,
}

pub struct ArtifactArena {
    limits: ArtifactArenaLimits,
    state: RwLock<ArenaState>,
}

impl ArtifactArena {
    pub fn new(limits: ArtifactArenaLimits) -> Result<Self, StoreError> {
        Ok(Self {
            limits: limits.validate()?,
            state: RwLock::new(ArenaState::default()),
        })
    }

    pub fn limits(&self) -> ArtifactArenaLimits {
        self.limits
    }

    pub async fn usage(&self) -> ArtifactArenaUsage {
        self.state.read().await.usage
    }

    /// Returns the immutable arena allocation without copying.
    pub async fn get_arc(&self, digest: Digest) -> Result<Arc<[u8]>, StoreError> {
        self.state
            .read()
            .await
            .objects
            .get(&digest)
            .map(|entry| Arc::clone(&entry.bytes))
            .ok_or(StoreError::NotFound(digest))
    }

    /// Inserts an immutable allocation without copying its payload.
    pub async fn put_arc(
        &self,
        expected: Option<Digest>,
        bytes: Arc<[u8]>,
        retention: RetentionClass,
    ) -> Result<StoredObject, StoreError> {
        let size_bytes = u64::try_from(bytes.len()).map_err(|_| {
            Self::capacity_error("object", 0, u64::MAX, self.limits.per_object_bytes)
        })?;
        if size_bytes > self.limits.per_object_bytes {
            return Err(Self::capacity_error(
                "object",
                0,
                size_bytes,
                self.limits.per_object_bytes,
            ));
        }
        let digest = match expected {
            Some(value) if value.algorithm == DigestAlgorithm::Sha256 => {
                Digest::hash_sha256(&bytes)
            }
            _ => Digest::hash_blake3(&bytes),
        };
        if let Some(expected) = expected
            && expected != digest
        {
            return Err(StoreError::DigestMismatch {
                expected,
                calculated: digest,
            });
        }
        let mut state = self.state.write().await;
        if let Some(entry) = state.objects.get(&digest) {
            let old = entry.meta.retention;
            let upgraded = Self::stronger(old, retention);
            if upgraded != old {
                let used = state.usage.retention_bytes(upgraded);
                let cap = self.limits.retention_cap(upgraded);
                if used.checked_add(size_bytes).is_none_or(|value| value > cap) {
                    return Err(Self::capacity_error("retention", used, size_bytes, cap));
                }
                *state.usage.retention_bytes_mut(old) -= size_bytes;
                *state.usage.retention_bytes_mut(upgraded) += size_bytes;
                state
                    .objects
                    .get_mut(&digest)
                    .expect("entry exists")
                    .meta
                    .retention = upgraded;
            }
            return Ok(StoredObject { digest, size_bytes });
        }
        let total = state.usage.total_bytes;
        if total
            .checked_add(size_bytes)
            .is_none_or(|value| value > self.limits.total_bytes)
        {
            return Err(Self::capacity_error(
                "arena",
                total,
                size_bytes,
                self.limits.total_bytes,
            ));
        }
        let retained = state.usage.retention_bytes(retention);
        let retention_cap = self.limits.retention_cap(retention);
        if retained
            .checked_add(size_bytes)
            .is_none_or(|value| value > retention_cap)
        {
            return Err(Self::capacity_error(
                "retention",
                retained,
                size_bytes,
                retention_cap,
            ));
        }
        state.usage.total_bytes += size_bytes;
        *state.usage.retention_bytes_mut(retention) += size_bytes;
        state.usage.objects += 1;
        state.objects.insert(
            digest,
            ArenaEntry {
                bytes,
                meta: ObjectMeta {
                    digest,
                    size_bytes,
                    retention,
                },
            },
        );
        Ok(StoredObject { digest, size_bytes })
    }

    fn stronger(a: RetentionClass, b: RetentionClass) -> RetentionClass {
        fn rank(retention: RetentionClass) -> u8 {
            match retention {
                RetentionClass::EvidencePermanent => 4,
                RetentionClass::Release => 3,
                RetentionClass::Active => 2,
                RetentionClass::Cache => 1,
                RetentionClass::Ephemeral => 0,
            }
        }
        if rank(a) >= rank(b) { a } else { b }
    }

    fn capacity_error(label: &str, used: u64, requested: u64, cap: u64) -> StoreError {
        StoreError::CapacityExceeded(format!(
            "{label}: used={used}, requested={requested}, cap={cap}"
        ))
    }
}

#[async_trait]
impl ArtifactStore for ArtifactArena {
    async fn put_stream(
        &self,
        expected: Option<Digest>,
        mut stream: Pin<Box<dyn Stream<Item = Result<Bytes, StoreError>> + Send>>,
        retention: RetentionClass,
    ) -> Result<StoredObject, StoreError> {
        let mut buffer = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            let next = buffer.len().checked_add(chunk.len()).ok_or_else(|| {
                Self::capacity_error(
                    "object",
                    buffer.len() as u64,
                    chunk.len() as u64,
                    self.limits.per_object_bytes,
                )
            })?;
            if next as u64 > self.limits.per_object_bytes {
                return Err(Self::capacity_error(
                    "object",
                    buffer.len() as u64,
                    chunk.len() as u64,
                    self.limits.per_object_bytes,
                ));
            }
            buffer.extend_from_slice(&chunk);
        }
        let bytes: Arc<[u8]> = buffer.into();
        self.put_arc(expected, bytes, retention).await
    }

    async fn head(&self, digest: Digest) -> Result<Option<ObjectMeta>, StoreError> {
        Ok(self
            .state
            .read()
            .await
            .objects
            .get(&digest)
            .map(|entry| entry.meta.clone()))
    }

    async fn get_range(
        &self,
        digest: Digest,
        range: Range<u64>,
        verification: ReadVerification,
    ) -> Result<Bytes, StoreError> {
        let bytes = self.get_arc(digest).await?;
        if range.start > range.end || range.end > bytes.len() as u64 {
            return Err(StoreError::InvalidRange {
                start: range.start,
                end: range.end,
                size: bytes.len() as u64,
            });
        }
        if let ReadVerification::Expected { digest: expected } = verification {
            let calculated = match expected.algorithm {
                DigestAlgorithm::Blake3 => Digest::hash_blake3(&bytes),
                DigestAlgorithm::Sha256 => Digest::hash_sha256(&bytes),
            };
            if calculated != expected {
                return Err(StoreError::DigestMismatch {
                    expected,
                    calculated,
                });
            }
        }
        Ok(Bytes::copy_from_slice(
            &bytes[range.start as usize..range.end as usize],
        ))
    }

    async fn delete_ephemeral(&self, digest: Digest, _token: GcToken) -> Result<(), StoreError> {
        let mut state = self.state.write().await;
        let Some(entry) = state.objects.get(&digest) else {
            return Ok(());
        };
        if matches!(
            entry.meta.retention,
            RetentionClass::EvidencePermanent | RetentionClass::Release
        ) {
            return Err(StoreError::UnauthorizedDelete(entry.meta.retention));
        }
        let entry = state.objects.remove(&digest).expect("entry exists");
        state.usage.total_bytes -= entry.meta.size_bytes;
        *state.usage.retention_bytes_mut(entry.meta.retention) -= entry.meta.size_bytes;
        state.usage.objects -= 1;
        Ok(())
    }
}

// Compatibility store. New hot paths should construct `ArtifactArena` with
// workload-specific limits instead of relying on this conservative default.
// ---------------------------------------------------------------------------
// MemoryArtifactStore
// ---------------------------------------------------------------------------

/// Compatibility facade with a finite 256 MiB ceiling. New code should use
/// `ArtifactArena::new` and choose workload-specific caps explicitly.
pub struct MemoryArtifactStore {
    arena: ArtifactArena,
}

impl MemoryArtifactStore {
    pub fn new() -> Self {
        const CAP: u64 = 256 * 1024 * 1024;
        Self {
            arena: ArtifactArena::new(ArtifactArenaLimits {
                total_bytes: CAP,
                per_object_bytes: CAP,
                evidence_permanent_bytes: CAP,
                release_bytes: CAP,
                active_bytes: CAP,
                cache_bytes: CAP,
                ephemeral_bytes: CAP,
            })
            .expect("constant compatibility arena limits are valid"),
        }
    }

    pub fn arena(&self) -> &ArtifactArena {
        &self.arena
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
        stream: Pin<Box<dyn Stream<Item = Result<Bytes, StoreError>> + Send>>,
        retention: RetentionClass,
    ) -> Result<StoredObject, StoreError> {
        self.arena.put_stream(expected, stream, retention).await
    }

    async fn head(&self, digest: Digest) -> Result<Option<ObjectMeta>, StoreError> {
        self.arena.head(digest).await
    }

    async fn get_range(
        &self,
        digest: Digest,
        range: Range<u64>,
        verification: ReadVerification,
    ) -> Result<Bytes, StoreError> {
        self.arena.get_range(digest, range, verification).await
    }

    async fn delete_ephemeral(&self, digest: Digest, token: GcToken) -> Result<(), StoreError> {
        self.arena.delete_ephemeral(digest, token).await
    }
}

// ---------------------------------------------------------------------------
// Atomic local evidence bundles
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommittedEvidence {
    digest: Digest,
    archive_generation: NonZeroU64,
}

impl CommittedEvidence {
    pub fn digest(&self) -> Digest {
        self.digest
    }

    pub fn archive_generation(&self) -> NonZeroU64 {
        self.archive_generation
    }
}

#[derive(Clone, Debug)]
pub struct EvidenceArtifact {
    pub name: String,
    pub bytes: Arc<[u8]>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceBundleEntry {
    pub name: String,
    pub digest: Digest,
    pub length: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceBundleManifest {
    pub schema: String,
    pub archive_generation: NonZeroU64,
    pub total_bytes: u64,
    pub artifacts: Vec<EvidenceBundleEntry>,
    pub digest: Digest,
}

#[derive(Clone, Debug)]
pub struct EvidenceBundle {
    pub manifest: EvidenceBundleManifest,
    pub artifacts: BTreeMap<String, Arc<[u8]>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CurrentEvidence {
    schema: String,
    digest: Digest,
    archive_generation: NonZeroU64,
}

pub struct LocalEvidenceBundleStore {
    root: PathBuf,
    max_bundle_bytes: u64,
    max_artifact_bytes: u64,
    max_artifacts: usize,
    commit_lock: std::sync::Mutex<()>,
}

impl LocalEvidenceBundleStore {
    pub fn new(
        root: PathBuf,
        max_bundle_bytes: u64,
        max_artifact_bytes: u64,
        max_artifacts: usize,
    ) -> Result<Self, StoreError> {
        if max_bundle_bytes == 0
            || max_artifact_bytes == 0
            || max_artifact_bytes > max_bundle_bytes
            || max_artifacts == 0
        {
            return Err(StoreError::CapacityExceeded(
                "bundle caps must be non-zero and per-artifact <= total".into(),
            ));
        }
        fs::create_dir_all(root.join("bundles"))?;
        sync_directory(&root)?;
        Ok(Self {
            root,
            max_bundle_bytes,
            max_artifact_bytes,
            max_artifacts,
            commit_lock: std::sync::Mutex::new(()),
        })
    }

    pub fn commit(
        &self,
        schema: &str,
        mut artifacts: Vec<EvidenceArtifact>,
    ) -> Result<(EvidenceBundle, CommittedEvidence), StoreError> {
        let _guard = self.commit_lock.lock().map_err(|_| StoreError::Closed)?;
        validate_schema(schema)?;
        if artifacts.is_empty() || artifacts.len() > self.max_artifacts {
            return Err(StoreError::CapacityExceeded(format!(
                "artifact count must be in 1..={}",
                self.max_artifacts
            )));
        }
        artifacts.sort_by(|left, right| left.name.cmp(&right.name));
        let mut entries = Vec::with_capacity(artifacts.len());
        let mut total_bytes = 0u64;
        let mut previous = None::<&str>;
        for artifact in &artifacts {
            validate_artifact_name(&artifact.name)?;
            if previous == Some(artifact.name.as_str()) {
                return Err(StoreError::ManifestError(format!(
                    "duplicate artifact name {}",
                    artifact.name
                )));
            }
            previous = Some(&artifact.name);
            let length = artifact.bytes.len() as u64;
            if length == 0 || length > self.max_artifact_bytes {
                return Err(StoreError::CapacityExceeded(format!(
                    "artifact {} length {length} exceeds cap {}",
                    artifact.name, self.max_artifact_bytes
                )));
            }
            total_bytes = total_bytes
                .checked_add(length)
                .ok_or_else(|| StoreError::CapacityExceeded("bundle length overflow".into()))?;
            if total_bytes > self.max_bundle_bytes {
                return Err(StoreError::CapacityExceeded(format!(
                    "bundle length {total_bytes} exceeds cap {}",
                    self.max_bundle_bytes
                )));
            }
            entries.push(EvidenceBundleEntry {
                name: artifact.name.clone(),
                digest: Digest::hash_blake3(&artifact.bytes),
                length,
            });
        }
        let generation = self.next_generation()?;
        let digest = bundle_digest(schema, generation, total_bytes, &entries)?;
        let manifest = EvidenceBundleManifest {
            schema: schema.into(),
            archive_generation: generation,
            total_bytes,
            artifacts: entries,
            digest,
        };
        let bundle_name = digest_hex(&digest);
        let bundles = self.root.join("bundles");
        let destination = bundles.join(&bundle_name);
        let partial = bundles.join(format!("{bundle_name}.partial"));
        if destination.exists() {
            let bundle = self.read(digest, schema)?;
            self.publish_current(&manifest)?;
            return Ok((bundle, committed(&manifest)));
        }
        if partial.exists() {
            fs::remove_dir_all(&partial)?;
        }
        fs::create_dir(&partial)?;
        for artifact in &artifacts {
            write_new_synced(&partial.join(&artifact.name), &artifact.bytes)?;
        }
        let manifest_bytes = serde_json::to_vec(&manifest)
            .map_err(|error| StoreError::ManifestError(error.to_string()))?;
        write_new_synced(&partial.join("manifest.json.partial"), &manifest_bytes)?;
        fs::rename(
            partial.join("manifest.json.partial"),
            partial.join("manifest.json"),
        )?;
        sync_directory(&partial)?;
        fs::rename(&partial, &destination)?;
        sync_directory(&bundles)?;
        self.publish_current(&manifest)?;
        let bundle = EvidenceBundle {
            manifest: manifest.clone(),
            artifacts: artifacts
                .into_iter()
                .map(|artifact| (artifact.name, artifact.bytes))
                .collect(),
        };
        Ok((bundle, committed(&manifest)))
    }

    pub fn read_current(
        &self,
        expected_schema: &str,
    ) -> Result<(EvidenceBundle, CommittedEvidence), StoreError> {
        validate_schema(expected_schema)?;
        let bytes = read_bounded(&self.root.join("CURRENT"), 4096)?;
        let current: CurrentEvidence = serde_json::from_slice(&bytes)
            .map_err(|error| StoreError::ManifestError(format!("invalid CURRENT: {error}")))?;
        if current.schema != "reflex.evidence-current.v1" {
            return Err(StoreError::ManifestError(
                "unsupported CURRENT schema".into(),
            ));
        }
        let bundle = self.read(current.digest, expected_schema)?;
        if bundle.manifest.archive_generation != current.archive_generation {
            return Err(StoreError::ManifestError(
                "CURRENT generation does not match bundle".into(),
            ));
        }
        let capability = committed(&bundle.manifest);
        Ok((bundle, capability))
    }

    pub fn read(
        &self,
        digest: Digest,
        expected_schema: &str,
    ) -> Result<EvidenceBundle, StoreError> {
        validate_schema(expected_schema)?;
        if digest.algorithm != DigestAlgorithm::Blake3 {
            return Err(StoreError::ManifestError(
                "bundle identity must use blake3".into(),
            ));
        }
        let directory = self.root.join("bundles").join(digest_hex(&digest));
        let metadata = fs::symlink_metadata(&directory)?;
        if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
            return Err(StoreError::ManifestError(
                "bundle path is not a real directory".into(),
            ));
        }
        let manifest_bytes = read_bounded(&directory.join("manifest.json"), 1024 * 1024)?;
        let manifest: EvidenceBundleManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|error| StoreError::ManifestError(format!("invalid manifest: {error}")))?;
        if manifest.schema != expected_schema
            || manifest.digest != digest
            || manifest.artifacts.is_empty()
            || manifest.artifacts.len() > self.max_artifacts
            || bundle_digest(
                &manifest.schema,
                manifest.archive_generation,
                manifest.total_bytes,
                &manifest.artifacts,
            )? != digest
        {
            return Err(StoreError::ManifestError(
                "manifest schema, identity, or shape is invalid".into(),
            ));
        }
        let mut total = 0u64;
        let mut artifacts = BTreeMap::new();
        let mut previous = None::<&str>;
        for entry in &manifest.artifacts {
            validate_artifact_name(&entry.name)?;
            if previous.is_some_and(|name| name >= entry.name.as_str())
                || entry.length == 0
                || entry.length > self.max_artifact_bytes
            {
                return Err(StoreError::ManifestError(
                    "manifest entries are not canonical or bounded".into(),
                ));
            }
            previous = Some(&entry.name);
            let path = directory.join(&entry.name);
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != entry.length
            {
                return Err(StoreError::CorruptObject(entry.digest));
            }
            let bytes = read_bounded(&path, self.max_artifact_bytes)?;
            if Digest::hash_blake3(&bytes) != entry.digest {
                return Err(StoreError::CorruptObject(entry.digest));
            }
            total = total
                .checked_add(entry.length)
                .ok_or_else(|| StoreError::ManifestError("bundle length overflow".into()))?;
            artifacts.insert(entry.name.clone(), Arc::<[u8]>::from(bytes));
        }
        if total != manifest.total_bytes || total > self.max_bundle_bytes {
            return Err(StoreError::ManifestError(
                "manifest total length does not reconcile".into(),
            ));
        }
        Ok(EvidenceBundle {
            manifest,
            artifacts,
        })
    }

    fn next_generation(&self) -> Result<NonZeroU64, StoreError> {
        let path = self.root.join("CURRENT");
        let previous = match fs::symlink_metadata(&path) {
            Ok(_) => {
                let bytes = read_bounded(&path, 4096)?;
                let current: CurrentEvidence = serde_json::from_slice(&bytes).map_err(|error| {
                    StoreError::ManifestError(format!("invalid CURRENT: {error}"))
                })?;
                current.archive_generation.get()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        };
        NonZeroU64::new(
            previous
                .checked_add(1)
                .ok_or_else(|| StoreError::ManifestError("archive generation overflow".into()))?,
        )
        .ok_or_else(|| StoreError::ManifestError("archive generation is zero".into()))
    }

    fn publish_current(&self, manifest: &EvidenceBundleManifest) -> Result<(), StoreError> {
        let current = CurrentEvidence {
            schema: "reflex.evidence-current.v1".into(),
            digest: manifest.digest,
            archive_generation: manifest.archive_generation,
        };
        let bytes = serde_json::to_vec(&current)
            .map_err(|error| StoreError::ManifestError(error.to_string()))?;
        let partial = self.root.join("CURRENT.partial");
        if partial.exists() {
            fs::remove_file(&partial)?;
        }
        write_new_synced(&partial, &bytes)?;
        fs::rename(partial, self.root.join("CURRENT"))?;
        sync_directory(&self.root)
    }
}

fn committed(manifest: &EvidenceBundleManifest) -> CommittedEvidence {
    CommittedEvidence {
        digest: manifest.digest,
        archive_generation: manifest.archive_generation,
    }
}

fn validate_schema(schema: &str) -> Result<(), StoreError> {
    if schema.is_empty()
        || schema.len() > 128
        || !schema
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err(StoreError::ManifestError("invalid evidence schema".into()));
    }
    Ok(())
}

fn validate_artifact_name(name: &str) -> Result<(), StoreError> {
    let path = Path::new(name);
    if name.is_empty()
        || name.len() > 255
        || name == "manifest.json"
        || path.components().count() != 1
        || !matches!(
            path.components().next(),
            Some(std::path::Component::Normal(_))
        )
    {
        return Err(StoreError::ManifestError(format!(
            "invalid artifact name {name:?}"
        )));
    }
    Ok(())
}

fn bundle_digest(
    schema: &str,
    generation: NonZeroU64,
    total_bytes: u64,
    artifacts: &[EvidenceBundleEntry],
) -> Result<Digest, StoreError> {
    let bytes = serde_json::to_vec(&(schema, generation, total_bytes, artifacts))
        .map_err(|error| StoreError::ManifestError(error.to_string()))?;
    Ok(Digest::hash_blake3(&bytes))
}

fn write_new_synced(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn read_bounded(path: &Path, cap: u64) -> Result<Vec<u8>, StoreError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() || metadata.len() > cap
    {
        return Err(StoreError::ManifestError(format!(
            "file {} is not a bounded regular file",
            path.display()
        )));
    }
    fs::read(path).map_err(Into::into)
}

fn sync_directory(path: &Path) -> Result<(), StoreError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Integrity scanner — walk fanout tree and verify digests
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct IntegrityScanReport {
    pub scanned: usize,
    pub valid: usize,
    pub corrupt: Vec<Digest>,
    pub missing_meta: Vec<Digest>,
}

pub struct IntegrityScanner<'a> {
    store: &'a FsArtifactStore,
}

impl<'a> IntegrityScanner<'a> {
    pub fn new(store: &'a FsArtifactStore) -> Self {
        Self { store }
    }

    pub fn scan(&self) -> Result<IntegrityScanReport, StoreError> {
        let objects_dir = self.store.root.join("objects");
        let mut report = IntegrityScanReport::default();
        if !objects_dir.exists() {
            return Ok(report);
        }
        for algorithm_entry in fs::read_dir(&objects_dir)? {
            let algorithm_entry = algorithm_entry?;
            if !algorithm_entry.file_type()?.is_dir() {
                continue;
            }
            let algorithm = match algorithm_entry.file_name().to_str() {
                Some("blake3") => DigestAlgorithm::Blake3,
                Some("sha256") => DigestAlgorithm::Sha256,
                _ => {
                    return Err(StoreError::ManifestError(format!(
                        "unknown CAS algorithm directory {}",
                        algorithm_entry.path().display()
                    )));
                }
            };
            for prefix_entry in fs::read_dir(algorithm_entry.path())? {
                let prefix_entry = prefix_entry?;
                if !prefix_entry.file_type()?.is_dir() {
                    return Err(StoreError::ManifestError(format!(
                        "unexpected file in CAS algorithm directory: {}",
                        prefix_entry.path().display()
                    )));
                }
                let prefix = prefix_entry.file_name().to_string_lossy().into_owned();
                if prefix.len() != 2 || !prefix.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Err(StoreError::ManifestError(format!(
                        "invalid CAS fanout prefix `{prefix}`"
                    )));
                }
                for obj_entry in fs::read_dir(prefix_entry.path())? {
                    let obj_entry = obj_entry?;
                    let path = obj_entry.path();
                    if path.extension().is_some_and(|e| e == "meta") {
                        continue;
                    }
                    if !obj_entry.file_type()?.is_file() {
                        return Err(StoreError::ManifestError(format!(
                            "unexpected non-file CAS object {}",
                            path.display()
                        )));
                    }
                    report.scanned += 1;
                    let hex_name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .ok_or_else(|| {
                            StoreError::ManifestError(format!(
                                "CAS object name is not UTF-8: {}",
                                path.display()
                            ))
                        })?
                        .to_string();
                    let full_hex = format!("{prefix}{hex_name}");
                    let bytes = match hex::decode(&full_hex) {
                        Ok(b) if b.len() == 32 => b,
                        _ => {
                            return Err(StoreError::ManifestError(format!(
                                "invalid CAS object name `{full_hex}`"
                            )));
                        }
                    };
                    let mut digest_bytes = [0u8; 32];
                    digest_bytes.copy_from_slice(&bytes);
                    let digest = match algorithm {
                        DigestAlgorithm::Blake3 => Digest::from_blake3_bytes(digest_bytes),
                        DigestAlgorithm::Sha256 => Digest::from_sha256_bytes(digest_bytes),
                    };
                    let (calculated, _) = FsArtifactStore::hash_file(&path, algorithm)?;
                    if calculated != digest {
                        report.corrupt.push(digest);
                        continue;
                    }
                    report.valid += 1;
                    if self.store.read_meta_file(&digest)?.is_none() {
                        report.missing_meta.push(digest);
                    }
                }
            }
        }
        Ok(report)
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChunkManifest {
    pub schema: String,
    pub total_bytes: u64,
    pub chunk_size: u64,
    pub root_digest: Digest,
    pub chunks: Vec<ChunkEntry>,
}

impl<'de> Deserialize<'de> for ChunkManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct WireManifest {
            schema: String,
            total_bytes: u64,
            chunk_size: u64,
            root_digest: Digest,
            chunks: Vec<ChunkEntry>,
        }

        let wire = WireManifest::deserialize(deserializer)?;
        let manifest = Self {
            schema: wire.schema,
            total_bytes: wire.total_bytes,
            chunk_size: wire.chunk_size,
            root_digest: wire.root_digest,
            chunks: wire.chunks,
        };
        manifest.validate().map_err(serde::de::Error::custom)?;
        Ok(manifest)
    }
}

impl ChunkManifest {
    pub fn build(total_bytes: u64, chunk_size: u64, chunks: Vec<ChunkEntry>) -> Self {
        let root_digest = Self::calculate_root(total_bytes, chunk_size, &chunks);
        Self {
            schema: "reflex.chunk_manifest.v1".to_string(),
            total_bytes,
            chunk_size,
            root_digest,
            chunks,
        }
    }

    pub fn try_build(
        total_bytes: u64,
        chunk_size: u64,
        chunks: Vec<ChunkEntry>,
    ) -> Result<Self, StoreError> {
        let manifest = Self::build(total_bytes, chunk_size, chunks);
        manifest.validate()?;
        Ok(manifest)
    }

    fn calculate_root(total_bytes: u64, chunk_size: u64, chunks: &[ChunkEntry]) -> Digest {
        let mut manifest_bytes = Vec::new();
        manifest_bytes.extend_from_slice(b"reflex.chunk_manifest.v1\0");
        manifest_bytes.extend_from_slice(&total_bytes.to_le_bytes());
        manifest_bytes.extend_from_slice(&chunk_size.to_le_bytes());
        manifest_bytes.extend_from_slice(&(chunks.len() as u64).to_le_bytes());
        for c in chunks {
            manifest_bytes.extend_from_slice(&c.index.to_le_bytes());
            manifest_bytes.extend_from_slice(&c.offset.to_le_bytes());
            manifest_bytes.extend_from_slice(&c.length.to_le_bytes());
            manifest_bytes.push(match c.digest.algorithm {
                DigestAlgorithm::Blake3 => 0,
                DigestAlgorithm::Sha256 => 1,
            });
            manifest_bytes.extend_from_slice(&c.digest.bytes);
        }
        Digest::hash_blake3(&manifest_bytes)
    }

    pub fn verify(&self) -> bool {
        self.validate().is_ok()
    }

    /// Validate both the manifest checksum and its complete chunk geometry.
    pub fn validate(&self) -> Result<(), StoreError> {
        if self.schema != "reflex.chunk_manifest.v1" {
            return Err(StoreError::ManifestError(format!(
                "unsupported chunk manifest schema {}",
                self.schema
            )));
        }
        if self.chunk_size == 0 {
            return Err(StoreError::ManifestError(
                "chunk_size must be greater than zero".to_string(),
            ));
        }
        if self.total_bytes == 0 && !self.chunks.is_empty() {
            return Err(StoreError::ManifestError(
                "empty object must not contain chunks".to_string(),
            ));
        }
        if self.total_bytes != 0 && self.chunks.is_empty() {
            return Err(StoreError::ManifestError(
                "non-empty object must contain chunks".to_string(),
            ));
        }

        let mut expected_offset = 0u64;
        for (position, chunk) in self.chunks.iter().enumerate() {
            let expected_index = u32::try_from(position).map_err(|_| {
                StoreError::ManifestError("too many chunks for u32 indices".to_string())
            })?;
            if chunk.index != expected_index {
                return Err(StoreError::ManifestError(format!(
                    "chunk at position {position} has index {}",
                    chunk.index
                )));
            }
            if chunk.offset != expected_offset {
                return Err(StoreError::ManifestError(format!(
                    "chunk {position} starts at {}, expected {expected_offset}",
                    chunk.offset
                )));
            }
            if chunk.length == 0 || chunk.length > self.chunk_size {
                return Err(StoreError::ManifestError(format!(
                    "chunk {position} has invalid length {}",
                    chunk.length
                )));
            }
            if position + 1 != self.chunks.len() && chunk.length != self.chunk_size {
                return Err(StoreError::ManifestError(format!(
                    "non-final chunk {position} is not exactly chunk_size"
                )));
            }
            expected_offset = expected_offset.checked_add(chunk.length).ok_or_else(|| {
                StoreError::ManifestError("chunk offsets overflow u64".to_string())
            })?;
        }
        if expected_offset != self.total_bytes {
            return Err(StoreError::ManifestError(format!(
                "chunk lengths total {expected_offset}, expected {}",
                self.total_bytes
            )));
        }

        let computed = Self::calculate_root(self.total_bytes, self.chunk_size, &self.chunks);
        if computed != self.root_digest {
            return Err(StoreError::DigestMismatch {
                expected: self.root_digest,
                calculated: computed,
            });
        }
        Ok(())
    }
}

async fn publish_chunk(
    store: &dyn ArtifactStore,
    resume: Option<&ChunkManifest>,
    index: u32,
    offset: u64,
    bytes: Bytes,
    retention: RetentionClass,
) -> Result<ChunkEntry, StoreError> {
    let digest = Digest::hash_blake3(&bytes);
    let entry = ChunkEntry {
        index,
        offset,
        length: bytes.len() as u64,
        digest,
    };

    if let Some(expected) = resume.and_then(|manifest| manifest.chunks.get(index as usize))
        && expected != &entry
    {
        return Err(StoreError::ManifestError(format!(
            "resume chunk {index} does not match input"
        )));
    }

    match store.head(digest).await? {
        Some(meta) if meta.size_bytes == entry.length => {
            store
                .get_range(
                    digest,
                    0..entry.length,
                    ReadVerification::Expected { digest },
                )
                .await?;
        }
        Some(_) => return Err(StoreError::CorruptObject(digest)),
        None => {
            store.put_bytes(Some(digest), bytes, retention).await?;
        }
    }
    Ok(entry)
}

/// Upload a stream as independently addressable fixed-size chunks.
///
/// At most one logical chunk is assembled locally. When `resume` is supplied,
/// already-present chunks are verified and reused; every manifest field must
/// still match the input stream before the returned manifest is accepted.
pub async fn upload_chunked_stream(
    store: &dyn ArtifactStore,
    total_bytes: u64,
    chunk_size: u64,
    mut stream: Pin<Box<dyn Stream<Item = Result<Bytes, StoreError>> + Send>>,
    retention: RetentionClass,
    resume: Option<&ChunkManifest>,
) -> Result<ChunkManifest, StoreError> {
    if chunk_size == 0 || chunk_size > usize::MAX as u64 {
        return Err(StoreError::ManifestError(
            "chunk_size is zero or exceeds addressable memory".to_string(),
        ));
    }
    if let Some(manifest) = resume {
        manifest.validate()?;
        if manifest.total_bytes != total_bytes || manifest.chunk_size != chunk_size {
            return Err(StoreError::ManifestError(
                "resume manifest dimensions do not match upload".to_string(),
            ));
        }
    }

    let chunk_capacity = chunk_size as usize;
    let mut buffer = BytesMut::with_capacity(chunk_capacity);
    let mut entries = Vec::new();
    let mut uploaded_bytes = 0u64;

    while let Some(part) = stream.next().await {
        let mut part = part?;
        while !part.is_empty() {
            let take = (chunk_capacity - buffer.len()).min(part.len());
            buffer.extend_from_slice(&part[..take]);
            part.advance(take);
            if buffer.len() == chunk_capacity {
                let index = u32::try_from(entries.len()).map_err(|_| {
                    StoreError::ManifestError("too many chunks for u32 indices".to_string())
                })?;
                let chunk = buffer.split().freeze();
                let entry =
                    publish_chunk(store, resume, index, uploaded_bytes, chunk, retention).await?;
                uploaded_bytes = uploaded_bytes.checked_add(entry.length).ok_or_else(|| {
                    StoreError::ManifestError("uploaded byte count overflow".to_string())
                })?;
                entries.push(entry);
            }
        }
    }

    if !buffer.is_empty() {
        let index = u32::try_from(entries.len()).map_err(|_| {
            StoreError::ManifestError("too many chunks for u32 indices".to_string())
        })?;
        let entry = publish_chunk(
            store,
            resume,
            index,
            uploaded_bytes,
            buffer.freeze(),
            retention,
        )
        .await?;
        uploaded_bytes = uploaded_bytes
            .checked_add(entry.length)
            .ok_or_else(|| StoreError::ManifestError("uploaded byte count overflow".to_string()))?;
        entries.push(entry);
    }

    if uploaded_bytes != total_bytes {
        return Err(StoreError::ManifestError(format!(
            "stream contained {uploaded_bytes} bytes, expected {total_bytes}"
        )));
    }
    let manifest = ChunkManifest::try_build(total_bytes, chunk_size, entries)?;
    if let Some(resume) = resume
        && resume != &manifest
    {
        return Err(StoreError::ManifestError(
            "resume manifest does not describe the complete input".to_string(),
        ));
    }
    Ok(manifest)
}

/// Reconstruct a byte range from a validated chunk manifest.
pub async fn get_chunked_range(
    store: &dyn ArtifactStore,
    manifest: &ChunkManifest,
    range: Range<u64>,
) -> Result<Bytes, StoreError> {
    manifest.validate()?;
    if range.start > range.end || range.end > manifest.total_bytes {
        return Err(StoreError::InvalidRange {
            start: range.start,
            end: range.end,
            size: manifest.total_bytes,
        });
    }
    let output_len = usize::try_from(range.end - range.start).map_err(|_| {
        StoreError::ManifestError("requested range exceeds addressable memory".to_string())
    })?;
    let mut output = BytesMut::with_capacity(output_len);
    for chunk in &manifest.chunks {
        let chunk_end = chunk.offset + chunk.length;
        let overlap_start = range.start.max(chunk.offset);
        let overlap_end = range.end.min(chunk_end);
        if overlap_start >= overlap_end {
            continue;
        }
        let local_start = overlap_start - chunk.offset;
        let local_end = overlap_end - chunk.offset;
        let bytes = store
            .get_range(
                chunk.digest,
                local_start..local_end,
                ReadVerification::Expected {
                    digest: chunk.digest,
                },
            )
            .await?;
        if bytes.len() as u64 != local_end - local_start {
            return Err(StoreError::CorruptObject(chunk.digest));
        }
        output.extend_from_slice(&bytes);
    }
    if output.len() != output_len {
        return Err(StoreError::ManifestError(
            "chunk reconstruction produced the wrong length".to_string(),
        ));
    }
    Ok(output.freeze())
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
    async fn fs_store_fails_closed_on_corrupt_retention_metadata() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = FsArtifactStore::new(temp_dir.path().to_path_buf()).unwrap();
        let stored = store
            .put_bytes(
                None,
                Bytes::from_static(b"permanent evidence"),
                RetentionClass::EvidencePermanent,
            )
            .await
            .unwrap();
        store.meta_cache.write().await.clear();
        std::fs::write(store.digest_meta_path(&stored.digest), b"not-json").unwrap();

        let error = store.head(stored.digest).await.unwrap_err();
        assert!(matches!(error, StoreError::ManifestError(_)));
    }

    #[tokio::test]
    async fn integrity_scanner_rejects_malformed_object_names() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = FsArtifactStore::new(temp_dir.path().to_path_buf()).unwrap();
        let stored = store
            .put_bytes(None, Bytes::from_static(b"object"), RetentionClass::Active)
            .await
            .unwrap();
        let malformed = store.parent_dir(&stored.digest).join("not-a-digest");
        std::fs::write(malformed, b"junk").unwrap();

        let error = IntegrityScanner::new(&store).scan().unwrap_err();
        assert!(matches!(error, StoreError::ManifestError(_)));
    }

    #[test]
    fn fs_readiness_exercises_scratch_without_publishing_an_object() {
        let temp_dir = tempfile::tempdir().unwrap();
        let store = FsArtifactStore::new(temp_dir.path().to_path_buf()).unwrap();
        store.check_readiness().unwrap();
        assert_eq!(
            std::fs::read_dir(temp_dir.path().join("scratch"))
                .unwrap()
                .count(),
            0
        );
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
    async fn test_chunk_manifest_rejects_invalid_geometry() {
        let mut manifest = ChunkManifest::try_build(
            8,
            4,
            vec![
                ChunkEntry {
                    index: 0,
                    offset: 0,
                    length: 4,
                    digest: Digest::hash_blake3(b"abcd"),
                },
                ChunkEntry {
                    index: 1,
                    offset: 4,
                    length: 4,
                    digest: Digest::hash_blake3(b"efgh"),
                },
            ],
        )
        .unwrap();
        manifest.chunks[1].offset = 5;
        assert!(matches!(
            manifest.validate(),
            Err(StoreError::ManifestError(_))
        ));
        let encoded = serde_json::to_vec(&manifest).unwrap();
        assert!(serde_json::from_slice::<ChunkManifest>(&encoded).is_err());
    }

    #[tokio::test]
    async fn test_chunk_upload_resume_and_range_reconstruction() {
        let store = MemoryArtifactStore::new();
        let input = Bytes::from_static(b"abcdefghijklmnopq");
        let parts = vec![
            Ok(input.slice(0..3)),
            Ok(input.slice(3..12)),
            Ok(input.slice(12..)),
        ];
        let manifest = upload_chunked_stream(
            &store,
            input.len() as u64,
            4,
            Box::pin(futures::stream::iter(parts)),
            RetentionClass::Active,
            None,
        )
        .await
        .unwrap();
        assert_eq!(manifest.chunks.len(), 5);
        assert!(manifest.verify());

        let range = get_chunked_range(&store, &manifest, 3..14).await.unwrap();
        assert_eq!(range, input.slice(3..14));

        let resumed = upload_chunked_stream(
            &store,
            input.len() as u64,
            4,
            Box::pin(futures::stream::iter(vec![Ok(input.clone())])),
            RetentionClass::Active,
            Some(&manifest),
        )
        .await
        .unwrap();
        assert_eq!(resumed, manifest);
    }

    #[tokio::test]
    async fn test_chunk_resume_rejects_different_input() {
        let store = MemoryArtifactStore::new();
        let original = Bytes::from_static(b"abcdefgh");
        let manifest = upload_chunked_stream(
            &store,
            original.len() as u64,
            4,
            Box::pin(futures::stream::iter(vec![Ok(original)])),
            RetentionClass::Active,
            None,
        )
        .await
        .unwrap();

        let error = upload_chunked_stream(
            &store,
            8,
            4,
            Box::pin(futures::stream::iter(vec![Ok(Bytes::from_static(
                b"abcdWXYZ",
            ))])),
            RetentionClass::Active,
            Some(&manifest),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, StoreError::ManifestError(_)));
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
            .put_bytes(
                None,
                Bytes::from_static(b"ephemeral"),
                RetentionClass::Ephemeral,
            )
            .await
            .unwrap();
        let permanent = store
            .put_bytes(
                None,
                Bytes::from_static(b"permanent"),
                RetentionClass::EvidencePermanent,
            )
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

    fn arena_limits() -> ArtifactArenaLimits {
        ArtifactArenaLimits {
            total_bytes: 16,
            per_object_bytes: 8,
            evidence_permanent_bytes: 16,
            release_bytes: 16,
            active_bytes: 16,
            cache_bytes: 16,
            ephemeral_bytes: 16,
        }
    }

    #[tokio::test]
    async fn artifact_arena_caps_are_atomic_and_dedup_never_downgrades_retention() {
        let arena = ArtifactArena::new(arena_limits()).unwrap();
        let first = arena
            .put_bytes(
                None,
                Bytes::from_static(b"evidence"),
                RetentionClass::EvidencePermanent,
            )
            .await
            .unwrap();
        let duplicate = arena
            .put_bytes(
                None,
                Bytes::from_static(b"evidence"),
                RetentionClass::Ephemeral,
            )
            .await
            .unwrap();
        assert_eq!(first, duplicate);
        assert_eq!(arena.usage().await.total_bytes, 8);
        assert_eq!(arena.usage().await.objects, 1);
        assert_eq!(
            arena.head(first.digest).await.unwrap().unwrap().retention,
            RetentionClass::EvidencePermanent
        );

        let before = arena.usage().await;
        assert!(matches!(
            arena
                .put_bytes(
                    None,
                    Bytes::from_static(b"too-large"),
                    RetentionClass::Active
                )
                .await,
            Err(StoreError::CapacityExceeded(_))
        ));
        assert_eq!(arena.usage().await, before);
        let allocation = arena.get_arc(first.digest).await.unwrap();
        assert_eq!(&*allocation, b"evidence");
    }

    #[test]
    fn evidence_bundle_commit_is_atomic_bounded_and_mints_capability() {
        let temp = tempfile::tempdir().unwrap();
        let store = LocalEvidenceBundleStore::new(temp.path().into(), 32, 16, 4).unwrap();
        let (bundle, capability) = store
            .commit(
                "reflex.test-evidence.v1",
                vec![EvidenceArtifact {
                    name: "ledger.bin".into(),
                    bytes: Arc::from(&b"verified"[..]),
                }],
            )
            .unwrap();
        assert_eq!(capability.digest(), bundle.manifest.digest);
        assert_eq!(capability.archive_generation().get(), 1);
        assert!(!temp.path().join("CURRENT.partial").exists());
        let (read, read_capability) = store.read_current("reflex.test-evidence.v1").unwrap();
        assert_eq!(read.manifest, bundle.manifest);
        assert_eq!(read_capability, capability);
        assert_eq!(&**read.artifacts.get("ledger.bin").unwrap(), b"verified");

        assert!(matches!(
            store.commit(
                "reflex.test-evidence.v1",
                vec![EvidenceArtifact {
                    name: "oversized.bin".into(),
                    bytes: Arc::from(vec![0_u8; 17]),
                }],
            ),
            Err(StoreError::CapacityExceeded(_))
        ));
    }

    #[test]
    fn evidence_bundle_reader_rejects_corruption_and_ignores_partial_publish() {
        let temp = tempfile::tempdir().unwrap();
        let store = LocalEvidenceBundleStore::new(temp.path().into(), 64, 32, 4).unwrap();
        let (bundle, _) = store
            .commit(
                "reflex.test-evidence.v1",
                vec![EvidenceArtifact {
                    name: "events.bin".into(),
                    bytes: Arc::from(&b"events"[..]),
                }],
            )
            .unwrap();
        let stale_partial = temp.path().join("bundles/stale.partial");
        fs::create_dir(&stale_partial).unwrap();
        fs::write(stale_partial.join("manifest.json.partial"), b"truncated").unwrap();
        assert!(store.read_current("reflex.test-evidence.v1").is_ok());

        let artifact = temp
            .path()
            .join("bundles")
            .join(digest_hex(&bundle.manifest.digest))
            .join("events.bin");
        fs::write(artifact, b"damage").unwrap();
        assert!(matches!(
            store.read_current("reflex.test-evidence.v1"),
            Err(StoreError::CorruptObject(_))
        ));
    }
}
