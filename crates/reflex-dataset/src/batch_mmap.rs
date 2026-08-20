//! Bounded file-backed RFXBATCH reader with deterministic shuffle and prefetch (P4.6).
//!
//! The workspace forbids unsafe code, while operating-system memory mapping requires an
//! unsafe mapping constructor. This module therefore uses bounded positioned reads through
//! the page cache. It never reads the complete RFXBATCH payload into the Rust heap.

use crate::{
    CandidateKnowledge, DatasetError, RFXBATCH_HEADER_BYTES, RFXBATCH_MAGIC, RFXBATCH_VERSION,
    RfxBatch,
};
use crc32c::{crc32c, crc32c_append};
use rand::SeedableRng;
use rand::seq::SliceRandom;
use rand_chacha::ChaCha8Rng;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

const STATUS_UNKNOWN: u8 = 0;
const STATUS_VIABLE: u8 = 1;
const STATUS_DEAD: u8 = 2;
const STATUS_INVALID: u8 = 3;
const CRC_BUFFER_BYTES: usize = 64 * 1024;
const DEFAULT_MAX_BATCH_BYTES: usize = 64 * 1024 * 1024;

/// Extended on-disk layout: packed status masks and source state digests.
#[derive(Clone, Debug)]
pub struct RfxBatchV2 {
    pub batch: RfxBatch,
    pub status_mask: Vec<u8>,
    pub source_state_ids: Vec<[u8; 32]>,
}

impl RfxBatchV2 {
    pub fn from_batch(batch: RfxBatch) -> Self {
        let status_mask: Vec<u8> = batch
            .labels
            .iter()
            .map(|label| match label {
                CandidateKnowledge::Viable { .. } => STATUS_VIABLE,
                CandidateKnowledge::KnownDead { .. } => STATUS_DEAD,
                CandidateKnowledge::Unknown => STATUS_UNKNOWN,
                CandidateKnowledge::Invalid { .. } => STATUS_INVALID,
            })
            .collect();
        let source_state_ids = vec![batch.source_dataset_digest.bytes; batch.total_groups.max(1)];
        Self {
            batch,
            status_mask,
            source_state_ids,
        }
    }

    pub fn write_to_file(&self, path: &Path) -> Result<(), DatasetError> {
        if self.status_mask.len() != self.batch.labels.len()
            || self
                .status_mask
                .iter()
                .zip(&self.batch.labels)
                .any(|(code, label)| {
                    *code
                        != match label {
                            CandidateKnowledge::Viable { .. } => STATUS_VIABLE,
                            CandidateKnowledge::KnownDead { .. } => STATUS_DEAD,
                            CandidateKnowledge::Unknown => STATUS_UNKNOWN,
                            CandidateKnowledge::Invalid { .. } => STATUS_INVALID,
                        }
                })
            || self.source_state_ids.len() != self.batch.total_groups.max(1)
        {
            return Err(DatasetError::Compilation(
                "RFXBATCH v2 metadata disagrees with the authoritative batch".into(),
            ));
        }
        self.batch.write_to_file(path)?;
        let mut file = std::fs::OpenOptions::new().append(true).open(path)?;
        for id in &self.source_state_ids {
            file.write_all(id)?;
        }
        let mask_crc = crc32c(&self.status_mask);
        file.write_all(&mask_crc.to_le_bytes())?;
        file.write_all(&(self.status_mask.len() as u32).to_le_bytes())?;
        file.write_all(&self.status_mask)?;
        Ok(())
    }
}

/// Page-cache-friendly RFXBATCH index backed by bounded positioned file reads.
pub struct FileBatchCache {
    file: Arc<Mutex<File>>,
    feature_dim: usize,
    total_groups: usize,
    total_candidates: usize,
    group_offsets: Vec<u32>,
    shuffle_seed: u64,
    group_order: Vec<usize>,
    features_offset: u64,
    max_batch_bytes: usize,
}

impl FileBatchCache {
    pub fn open(path: &Path, shuffle_seed: u64) -> Result<Self, DatasetError> {
        Self::open_with_max_batch_bytes(path, shuffle_seed, DEFAULT_MAX_BATCH_BYTES)
    }

    /// Opens and validates an RFXBATCH while bounding every staged batch.
    pub fn open_with_max_batch_bytes(
        path: &Path,
        shuffle_seed: u64,
        max_batch_bytes: usize,
    ) -> Result<Self, DatasetError> {
        if max_batch_bytes == 0 {
            return Err(DatasetError::Compilation(
                "max_batch_bytes must be greater than zero".into(),
            ));
        }
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let mut header = [0u8; RFXBATCH_HEADER_BYTES];
        if file_len < RFXBATCH_HEADER_BYTES as u64 {
            let mut short = vec![0u8; usize::try_from(file_len).unwrap_or(0).min(8)];
            file.read_exact(&mut short)?;
            let mut observed = [0; 8];
            observed[..short.len()].copy_from_slice(&short);
            return Err(DatasetError::InvalidMagic(observed));
        }
        file.read_exact(&mut header)?;
        if &header[0..8] != RFXBATCH_MAGIC {
            return Err(DatasetError::InvalidMagic(header[0..8].try_into().unwrap()));
        }
        if header[8] != RFXBATCH_VERSION || !matches!(header[9], 0 | 1) {
            return Err(DatasetError::SchemaMismatch {
                expected: format!("RFXBATCH v{RFXBATCH_VERSION}"),
                found: format!("RFXBATCH v{}", header[8]),
            });
        }
        let feature_dim = u32::from_le_bytes(header[42..46].try_into().unwrap()) as usize;
        let total_groups = u32::from_le_bytes(header[46..50].try_into().unwrap()) as usize;
        let total_candidates = u32::from_le_bytes(header[50..54].try_into().unwrap()) as usize;
        let payload_len = usize::try_from(u64::from_le_bytes(header[54..62].try_into().unwrap()))
            .map_err(|_| {
            DatasetError::Compilation("RFXBATCH payload length overflows usize".into())
        })?;
        let expected_crc = u32::from_le_bytes(header[62..66].try_into().unwrap());
        let payload_end = RFXBATCH_HEADER_BYTES
            .checked_add(payload_len)
            .ok_or_else(|| DatasetError::Compilation("payload end overflow".into()))?;
        if file_len < payload_end as u64 {
            return Err(DatasetError::Compilation("truncated RFXBATCH".into()));
        }

        // Validate the complete payload in fixed memory. This preserves the authoritative
        // whole-payload CRC without materializing the payload in the Rust heap.
        let mut crc = 0;
        let mut remaining = payload_len;
        let mut crc_buffer = vec![0u8; CRC_BUFFER_BYTES.min(remaining.max(1))];
        while remaining != 0 {
            let count = remaining.min(crc_buffer.len());
            file.read_exact(&mut crc_buffer[..count])?;
            crc = crc32c_append(crc, &crc_buffer[..count]);
            remaining -= count;
        }
        if crc != expected_crc {
            return Err(DatasetError::CrcMismatch);
        }
        let offsets_bytes = total_groups
            .checked_add(1)
            .and_then(|count| count.checked_mul(4))
            .ok_or_else(|| DatasetError::Compilation("group offset length overflow".into()))?;
        if offsets_bytes > payload_len {
            return Err(DatasetError::Compilation(
                "truncated RFXBATCH group offsets".into(),
            ));
        }
        file.seek(SeekFrom::Start(RFXBATCH_HEADER_BYTES as u64))?;
        let mut offsets = vec![0u8; offsets_bytes];
        file.read_exact(&mut offsets)?;
        let mut group_offsets = Vec::with_capacity(total_groups + 1);
        for offset in (0..offsets_bytes).step_by(4) {
            group_offsets.push(u32::from_le_bytes(
                offsets[offset..offset + 4].try_into().unwrap(),
            ));
        }
        if group_offsets.first().copied() != Some(0)
            || group_offsets.last().copied().map(|value| value as usize) != Some(total_candidates)
            || group_offsets.windows(2).any(|pair| pair[0] > pair[1])
        {
            return Err(DatasetError::Compilation(
                "invalid RFXBATCH group offset geometry".into(),
            ));
        }

        let feature_bytes = total_candidates
            .checked_mul(feature_dim)
            .and_then(|count| count.checked_mul(std::mem::size_of::<f32>()))
            .ok_or_else(|| DatasetError::Compilation("RFXBATCH feature shape overflow".into()))?;
        if offsets_bytes
            .checked_add(feature_bytes)
            .is_none_or(|required| required > payload_len)
        {
            return Err(DatasetError::Compilation(
                "truncated RFXBATCH feature column".into(),
            ));
        }
        for pair in group_offsets.windows(2) {
            let candidates = (pair[1] - pair[0]) as usize;
            let bytes = candidates
                .checked_mul(feature_dim)
                .and_then(|count| count.checked_mul(std::mem::size_of::<f32>()))
                .ok_or_else(|| DatasetError::Compilation("batch byte size overflow".into()))?;
            if bytes > max_batch_bytes {
                return Err(DatasetError::Compilation(format!(
                    "batch requires {bytes} bytes, exceeding configured limit {max_batch_bytes}"
                )));
            }
        }
        let mut group_order: Vec<usize> = (0..total_groups).collect();
        let mut rng = ChaCha8Rng::seed_from_u64(shuffle_seed);
        group_order.shuffle(&mut rng);

        Ok(Self {
            file: Arc::new(Mutex::new(file)),
            feature_dim,
            total_groups,
            total_candidates,
            group_offsets,
            shuffle_seed,
            group_order,
            features_offset: (RFXBATCH_HEADER_BYTES + offsets_bytes) as u64,
            max_batch_bytes,
        })
    }

    pub fn shuffle_seed(&self) -> u64 {
        self.shuffle_seed
    }

    pub fn batch_count(&self) -> usize {
        self.total_groups
    }

    /// Restart training at an exact batch position in the shuffled order.
    pub fn batch_at_position(&self, position: usize) -> Result<FileBatchView<'_>, DatasetError> {
        if position >= self.total_groups {
            return Err(DatasetError::Compilation(format!(
                "batch position {position} >= batch count {}",
                self.total_groups
            )));
        }
        let group_idx = self.group_order[position];
        Ok(FileBatchView {
            cache: self,
            group_idx,
            position,
        })
    }

    fn read_batch(&self, position: usize) -> Result<PrefetchedBatch, DatasetError> {
        let view = self.batch_at_position(position)?;
        let start_candidate = self.group_offsets[view.group_idx] as usize;
        let end_candidate = self.group_offsets[view.group_idx + 1] as usize;
        let byte_start = start_candidate
            .checked_mul(self.feature_dim)
            .and_then(|count| count.checked_mul(std::mem::size_of::<f32>()))
            .ok_or_else(|| DatasetError::Compilation("batch offset overflow".into()))?;
        let byte_len = (end_candidate - start_candidate)
            .checked_mul(self.feature_dim)
            .and_then(|count| count.checked_mul(std::mem::size_of::<f32>()))
            .ok_or_else(|| DatasetError::Compilation("batch length overflow".into()))?;
        if byte_len > self.max_batch_bytes {
            return Err(DatasetError::Compilation(
                "batch exceeds configured staging limit".into(),
            ));
        }
        let mut feature_bytes = vec![0u8; byte_len];
        let mut file = self
            .file
            .lock()
            .map_err(|_| DatasetError::Compilation("batch file lock poisoned".into()))?;
        file.seek(SeekFrom::Start(
            self.features_offset
                .checked_add(byte_start as u64)
                .ok_or_else(|| DatasetError::Compilation("batch file offset overflow".into()))?,
        ))?;
        file.read_exact(&mut feature_bytes)?;
        Ok(PrefetchedBatch {
            position,
            group_index: view.group_idx,
            candidate_range: start_candidate..end_candidate,
            feature_bytes,
        })
    }

    pub fn corrupt_check(&self) -> Result<(), DatasetError> {
        if self.total_candidates == 0 {
            return Err(DatasetError::EmptyDataset);
        }
        Ok(())
    }
}

pub struct FileBatchView<'a> {
    cache: &'a FileBatchCache,
    group_idx: usize,
    position: usize,
}

impl FileBatchView<'_> {
    pub fn position(&self) -> usize {
        self.position
    }

    pub fn group_index(&self) -> usize {
        self.group_idx
    }

    pub fn candidate_count(&self) -> usize {
        let offsets = &self.cache.group_offsets;
        (offsets[self.group_idx + 1] - offsets[self.group_idx]) as usize
    }
}

/// Compatibility name retained for callers; this is deliberately not an unsafe mmap.
pub type MmapBatchCache = FileBatchCache;
/// Compatibility name retained for callers; views are backed by positioned reads.
#[allow(dead_code)]
pub type MmapBatchView<'a> = FileBatchView<'a>;

/// One bounded, file-backed batch prepared by the dedicated prefetch worker.
pub struct PrefetchedBatch {
    pub position: usize,
    pub group_index: usize,
    pub candidate_range: std::ops::Range<usize>,
    pub feature_bytes: Vec<u8>,
}

enum PrefetchCommand {
    Read(usize),
}

/// Double-buffered prefetcher on one persistent, optionally permitted thread.
pub struct PrefetchingBatchLoader {
    command_tx: Option<mpsc::SyncSender<PrefetchCommand>>,
    result_rx: Mutex<mpsc::Receiver<Result<PrefetchedBatch, String>>>,
    next_to_schedule: Mutex<usize>,
    next_expected: Mutex<usize>,
    total_batches: usize,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl PrefetchingBatchLoader {
    pub fn new(cache: MmapBatchCache) -> Self {
        Self::start(cache, None)
    }

    /// Starts the worker while retaining a caller-owned compute-pool permit.
    ///
    /// Passing `reflex_runtime::ComputeLease` here lets the runtime account the
    /// dedicated prefetch worker without introducing a dependency cycle.
    pub fn new_with_permit<P: Send + 'static>(cache: MmapBatchCache, permit: P) -> Self {
        Self::start(cache, Some(Box::new(permit)))
    }

    fn start(cache: MmapBatchCache, permit: Option<Box<dyn Send>>) -> Self {
        let cache = Arc::new(cache);
        let (command_tx, command_rx) = mpsc::sync_channel(2);
        let (result_tx, result_rx) = mpsc::sync_channel(2);
        let batch_count = cache.batch_count();
        let worker = thread::Builder::new()
            .name("reflex-batch-prefetch".into())
            .spawn(move || {
                let _permit = permit;
                while let Ok(PrefetchCommand::Read(position)) = command_rx.recv() {
                    let result = cache
                        .read_batch(position)
                        .map_err(|error| error.to_string());
                    if result_tx.send(result).is_err() {
                        break;
                    }
                }
            })
            .expect("spawn bounded batch prefetch worker");
        let loader = Self {
            command_tx: Some(command_tx),
            result_rx: Mutex::new(result_rx),
            next_to_schedule: Mutex::new(0),
            next_expected: Mutex::new(0),
            total_batches: batch_count,
            worker: Mutex::new(Some(worker)),
        };
        if batch_count != 0 {
            loader.schedule_next().expect("initial prefetch");
        }
        loader
    }

    fn schedule_next(&self) -> Result<(), DatasetError> {
        let mut next = self
            .next_to_schedule
            .lock()
            .map_err(|_| DatasetError::Compilation("prefetch position lock poisoned".into()))?;
        if *next >= self.total_batches {
            return Ok(());
        }
        self.command_tx
            .as_ref()
            .ok_or_else(|| DatasetError::Compilation("prefetch worker closed".into()))?
            .send(PrefetchCommand::Read(*next))
            .map_err(|_| DatasetError::Compilation("prefetch worker closed".into()))?;
        *next += 1;
        Ok(())
    }

    pub fn next_batch(&self) -> Result<PrefetchedBatch, DatasetError> {
        let mut expected = self
            .next_expected
            .lock()
            .map_err(|_| DatasetError::Compilation("prefetch position lock poisoned".into()))?;
        if *expected >= self.total_batches {
            return Err(DatasetError::Compilation("end of epoch".into()));
        }
        let batch = self
            .result_rx
            .lock()
            .map_err(|_| DatasetError::Compilation("prefetch result lock poisoned".into()))?
            .recv()
            .map_err(|_| DatasetError::Compilation("prefetch worker closed".into()))?
            .map_err(DatasetError::Compilation)?;
        if batch.position != *expected {
            return Err(DatasetError::Compilation(
                "prefetch results arrived out of order".into(),
            ));
        }
        *expected += 1;
        // Keep exactly one batch in flight. The two-slot channels bound staging even if
        // callers temporarily stop consuming.
        self.schedule_next()?;
        Ok(batch)
    }

    pub fn prefetch_next(&self) -> Result<usize, DatasetError> {
        Ok(self.next_batch()?.position)
    }
}

impl Drop for PrefetchingBatchLoader {
    fn drop(&mut self) {
        self.command_tx.take();
        if let Ok(worker) = self.worker.get_mut()
            && let Some(worker) = worker.take()
        {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RfxBatch;
    use reflex_types::Digest;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn sample_batch() -> RfxBatch {
        let total_groups = 10;
        let total_candidates = total_groups * 2;
        RfxBatch {
            source_dataset_digest: Digest::hash_blake3(b"ds1"),
            feature_dim: 2,
            total_groups,
            total_candidates,
            group_offsets: (0..=total_groups).map(|i| (i * 2) as u32).collect(),
            features: vec![1.0; total_candidates * 2],
            labels: vec![CandidateKnowledge::Unknown; total_candidates],
            cost_to_go: vec![1.0; total_candidates],
            weights: vec![1.0; total_candidates],
        }
    }

    #[test]
    fn test_shuffle_seed_deterministic_order() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("batch.rfxbatch");
        RfxBatchV2::from_batch(sample_batch())
            .write_to_file(&path)
            .unwrap();
        let a = MmapBatchCache::open(&path, 42).unwrap();
        let b = MmapBatchCache::open(&path, 42).unwrap();
        let c = MmapBatchCache::open(&path, 99).unwrap();
        assert_eq!(a.group_order, b.group_order);
        assert_ne!(
            a.group_order, c.group_order,
            "shuffle seed must change group order"
        );
    }

    #[test]
    fn test_restart_at_batch_position() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("batch.rfxbatch");
        RfxBatchV2::from_batch(sample_batch())
            .write_to_file(&path)
            .unwrap();
        let cache = MmapBatchCache::open(&path, 7).unwrap();
        let view = cache.batch_at_position(2).unwrap();
        assert_eq!(view.position(), 2);
        assert!(cache.batch_at_position(99).is_err());
    }

    #[test]
    fn test_corrupt_crc_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("batch.rfxbatch");
        sample_batch().write_to_file(&path).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        if let Some(b) = bytes.last_mut() {
            *b ^= 0xFF;
        }
        std::fs::write(&path, bytes).unwrap();
        assert!(MmapBatchCache::open(&path, 0).is_err());
    }

    #[test]
    fn test_positioned_batch_read_is_bounded() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("batch.rfxbatch");
        sample_batch().write_to_file(&path).unwrap();

        // Each fixture group has 2 candidates * 2 features * 4 bytes.
        assert!(FileBatchCache::open_with_max_batch_bytes(&path, 0, 15).is_err());
        let cache = FileBatchCache::open_with_max_batch_bytes(&path, 0, 16).unwrap();
        let batch = cache.read_batch(0).unwrap();
        assert_eq!(batch.feature_bytes.len(), 16);
        assert_eq!(batch.candidate_range.len(), 2);
    }

    #[test]
    fn test_persistent_prefetch_holds_permit_and_ends_exactly() {
        struct Permit(Arc<AtomicBool>);
        impl Drop for Permit {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("batch.rfxbatch");
        sample_batch().write_to_file(&path).unwrap();
        let released = Arc::new(AtomicBool::new(false));
        let loader = PrefetchingBatchLoader::new_with_permit(
            FileBatchCache::open(&path, 7).unwrap(),
            Permit(Arc::clone(&released)),
        );
        for position in 0..10 {
            let batch = loader.next_batch().unwrap();
            assert_eq!(batch.position, position);
            assert_eq!(batch.feature_bytes.len(), 16);
        }
        assert!(loader.next_batch().is_err());
        assert!(!released.load(Ordering::Acquire));
        drop(loader);
        assert!(released.load(Ordering::Acquire));
    }
}
