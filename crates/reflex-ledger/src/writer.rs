//! Bounded segment writer (§8.3) with never-drop semantics (F-27).
//!
//! Design:
//! - `EventSink` buffers typed events into a reusable encoder and hands full
//!   blocks to a dedicated writer thread through a bounded mpsc channel
//!   (backpressure = awaiting `send` when the channel is full).
//! - The writer performs sequential I/O. A failed block write is *never
//!   dropped*: the block is kept in a bounded pending queue and retried on the
//!   next command (limited immediate retries, then re-queued). The durable
//!   watermark (`durable_bytes`) only advances after a successful fsync.
//! - Failures surface three ways: `Barrier`/`Finish` replies carry the error,
//!   `EventSink::write_event` returns the latest error before buffering, and
//!   `EventSink::take_error()` exposes it. A later successful barrier clears
//!   the error (transient failures self-heal; blocks are never lost).
//! - `finish()` writes the §8.2 footer and the sidecar index (F-29) and
//!   verifies both by reading them back.

use crate::encoder::EncodedBlock;
use crate::segment::{
    BlockHeader, BlockIndexEntry, IndexEntry, IndexFile, SegmentFooter, SegmentHeader,
    sidecar_index_path,
};
use crate::{
    ClosedSegment, GapPolicy, LedgerCommand, LedgerConfig, LedgerError, LedgerPosition, SequenceGap,
};
use reflex_types::Digest;
use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write as IoWrite};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Abstraction over the durable-sync operation so tests can inject failures.
pub trait DurableSync {
    fn sync_durable(&mut self) -> std::io::Result<()>;
}

impl DurableSync for File {
    fn sync_durable(&mut self) -> std::io::Result<()> {
        self.sync_all()
    }
}

/// Shared error channel between the writer thread and the `EventSink`.
#[derive(Debug, Default)]
pub(crate) struct WriterState {
    pub error: Mutex<Option<LedgerError>>,
}

pub struct LedgerWriter<F: IoWrite + Read + Seek + DurableSync = File> {
    path: PathBuf,
    header: SegmentHeader,
    config: LedgerConfig,
    file: F,
    pub next_sequence: u64,
    total_blocks: u64,
    total_events: u64,
    total_bytes: u64,
    durable_bytes: u64,
    pending: VecDeque<EncodedBlock>,
    index: Vec<BlockIndexEntry>,
    index_classes: Vec<u64>,
    chained_digest: [u8; 32],
    error: Option<LedgerError>,
}

impl LedgerWriter<File> {
    pub fn create(path: PathBuf, header: SegmentHeader) -> Result<Self, LedgerError> {
        Self::create_with(path, header, LedgerConfig::default())
    }

    pub fn create_with(
        path: PathBuf,
        header: SegmentHeader,
        config: LedgerConfig,
    ) -> Result<Self, LedgerError> {
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;
        Self::with_file(file, path, header, config)
    }
}

impl<F> LedgerWriter<F>
where
    F: IoWrite + Read + Seek + DurableSync,
{
    /// Wraps an existing file. The header is written at offset 0; any previous
    /// contents are overwritten from offset 0 (the caller is responsible for
    /// truncation semantics).
    pub fn with_file(
        mut file: F,
        path: PathBuf,
        header: SegmentHeader,
        config: LedgerConfig,
    ) -> Result<Self, LedgerError> {
        file.seek(SeekFrom::Start(0))?;
        let header_bytes = header.encode();
        let first_sequence = header.first_sequence;
        file.write_all(&header_bytes)?;
        file.flush()?;
        Ok(Self {
            path,
            header,
            config,
            file,
            next_sequence: first_sequence,
            total_blocks: 0,
            total_events: 0,
            total_bytes: SegmentHeader::SIZE as u64,
            durable_bytes: SegmentHeader::SIZE as u64,
            pending: VecDeque::new(),
            index: Vec::new(),
            index_classes: Vec::new(),
            chained_digest: *blake3::hash(&header_bytes).as_bytes(),
            error: None,
        })
    }

    /// Writes one block. Validates stream continuity (F-30: ordering/gap
    /// validation at write time) under `GapPolicy::Reject`. On error the file
    /// position and the byte counters are unchanged, so a retry rewrites the
    /// block from its start.
    pub fn write_block(&mut self, block: &EncodedBlock) -> Result<(), LedgerError> {
        if matches!(self.config.gap_policy, GapPolicy::Reject)
            && block.header.first_sequence != self.next_sequence
        {
            return Err(LedgerError::SequenceMismatch {
                offset: self.total_bytes,
                expected: self.next_sequence,
                found: block.header.first_sequence,
            });
        }
        let offset = self.total_bytes;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(&block.header.encode())?;
        self.file.write_all(&block.payload)?;
        self.file.flush()?;
        self.total_bytes = offset + BlockHeader::SIZE as u64 + block.payload.len() as u64;
        self.total_blocks += 1;
        self.total_events += u64::from(block.header.event_count);
        self.next_sequence = block.header.last_sequence.wrapping_add(1);
        let mut h = blake3::Hasher::new();
        h.update(&self.chained_digest);
        h.update(&block.header.encode());
        h.update(&block.payload);
        self.chained_digest = *h.finalize().as_bytes();
        self.index.push(BlockIndexEntry {
            offset,
            header: block.header.clone(),
        });
        self.index_classes.push(block.classes);
        Ok(())
    }

    fn write_block_retry(&mut self, block: &EncodedBlock) -> Result<(), LedgerError> {
        let mut attempts = 0u32;
        loop {
            match self.write_block(block) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    tracing::warn!(error = %e, "ledger block write failed; will retry");
                    self.error = Some(e.clone());
                    if attempts >= self.config.write_retries {
                        return Err(e);
                    }
                    attempts += 1;
                    if !self.config.retry_delay.is_zero() {
                        std::thread::sleep(self.config.retry_delay);
                    }
                }
            }
        }
    }

    /// Retries all pending blocks; the first persistently failing block (and
    /// everything behind it) stays queued. Blocks are never dropped.
    fn flush_pending(&mut self) -> Result<(), LedgerError> {
        while let Some(block) = self.pending.pop_front() {
            if let Err(e) = self.write_block_retry(&block) {
                self.pending.push_front(block);
                return Err(e);
            }
        }
        Ok(())
    }

    /// Enqueues a block from the sink. Any previously failed blocks are
    /// retried first; on persistent failure the new block is appended to the
    /// pending queue so no event is lost.
    pub fn queue_block(&mut self, block: EncodedBlock) -> Result<(), LedgerError> {
        if let Err(e) = self.flush_pending() {
            self.pending.push_back(block);
            return Err(e);
        }
        match self.write_block_retry(&block) {
            Ok(()) => Ok(()),
            Err(e) => {
                self.pending.push_back(block);
                Err(e)
            }
        }
    }

    /// Flushes all pending blocks and fsyncs. The durable watermark advances
    /// only after a successful fsync.
    pub fn barrier(&mut self) -> Result<LedgerPosition, LedgerError> {
        self.flush_pending()?;
        self.sync_file()?;
        self.durable_bytes = self.total_bytes;
        self.error = None;
        Ok(self.current_position())
    }

    pub fn sync(&mut self) -> Result<(), LedgerError> {
        self.sync_file()
    }

    fn sync_file(&mut self) -> Result<(), LedgerError> {
        self.file.flush()?;
        self.file.sync_durable()?;
        Ok(())
    }

    pub fn current_position(&self) -> LedgerPosition {
        LedgerPosition {
            offset: self.total_bytes,
            sequence: self.next_sequence,
        }
    }

    /// The last error observed by the writer (cleared on a successful barrier).
    pub fn last_error(&self) -> Option<&LedgerError> {
        self.error.as_ref()
    }

    pub fn is_failed(&self) -> bool {
        self.error.is_some()
    }

    fn write_footer(&mut self) -> Result<SegmentFooter, LedgerError> {
        let stored_bytes = self
            .total_bytes
            .saturating_sub(SegmentHeader::SIZE as u64)
            .saturating_sub(self.total_blocks * BlockHeader::SIZE as u64);
        let footer = SegmentFooter {
            version: 1,
            total_blocks: self.total_blocks,
            total_events: self.total_events,
            first_sequence: self.header.first_sequence,
            last_sequence: if self.total_events == 0 {
                self.header.first_sequence.wrapping_sub(1)
            } else {
                self.next_sequence.wrapping_sub(1)
            },
            stored_bytes,
            uncompressed_bytes: stored_bytes,
            block_index: self.index.clone(),
            segment_digest: Digest::from_blake3_bytes(self.chained_digest),
        };
        let data = footer.encode();
        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&data)?;
        self.file.flush()?;
        self.file.sync_durable()?;
        self.total_bytes += data.len() as u64;
        Ok(footer)
    }

    fn write_sidecar_index(&mut self) -> Result<IndexFile, LedgerError> {
        let idx = IndexFile {
            version: 1,
            segment_len: self.total_bytes,
            entries: self
                .index
                .iter()
                .zip(self.index_classes.iter())
                .map(|(e, classes)| IndexEntry {
                    offset: e.offset,
                    first_sequence: e.header.first_sequence,
                    last_sequence: e.header.last_sequence,
                    event_count: e.header.event_count,
                    classes: *classes,
                })
                .collect(),
        };
        let data = idx.encode();
        let idx_path = sidecar_index_path(&self.path);
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&idx_path)?;
        f.write_all(&data)?;
        f.flush()?;
        f.sync_all()?;
        Ok(idx)
    }

    /// Closes the segment: flush pending, fsync, write footer + sidecar index,
    /// then read both back to verify them. On any failure no footer is
    /// written/kept inconsistent and the error is returned; the file itself
    /// remains recoverable up to the last durable block.
    pub fn finish(mut self) -> Result<ClosedSegment, LedgerError> {
        self.flush_pending()?;
        self.sync_file()?;
        let footer = self.write_footer()?;
        if self.config.write_index {
            self.write_sidecar_index()?;
        }
        // Verify what we wrote (footer parse + crc, index parse + crc).
        let mut file = File::open(&self.path)?;
        let file_len = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(file_len - footer.encode().len() as u64))?;
        let mut footer_bytes = vec![0u8; footer.encode().len()];
        file.read_exact(&mut footer_bytes)?;
        SegmentFooter::decode(&footer_bytes)
            .map_err(|_| LedgerError::Encoding("footer verification failed after write".into()))?;
        if self.config.write_index {
            let mut idx_bytes = Vec::new();
            let idx_path = sidecar_index_path(&self.path);
            let mut idx_file = File::open(&idx_path)?;
            idx_file.read_to_end(&mut idx_bytes)?;
            let read = IndexFile::decode(&idx_bytes).map_err(|_| {
                LedgerError::Encoding("index verification failed after write".into())
            })?;
            debug_assert_eq!(read.entries.len(), self.index.len());
        }
        Ok(ClosedSegment {
            header: self.header,
            total_blocks: self.total_blocks,
            total_events: self.total_events,
            total_bytes: self.total_bytes,
            segment_digest: footer.segment_digest,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------
// Async sink (§8.3)
// ---------------------------------------------------------------------------

pub struct EventSink {
    tx: tokio::sync::mpsc::Sender<LedgerCommand>,
    shared: Arc<WriterState>,
    pool: crate::BufferPool,
    encoder: crate::EventEncoder,
    config: LedgerConfig,
}

impl EventSink {
    pub(crate) fn new(
        tx: tokio::sync::mpsc::Sender<LedgerCommand>,
        shared: Arc<WriterState>,
        first_sequence: u64,
        config: LedgerConfig,
    ) -> Self {
        Self {
            tx,
            shared,
            pool: crate::BufferPool::new(64 * 1024),
            encoder: crate::EventEncoder::with_first_sequence(first_sequence, config.gap_policy),
            config,
        }
    }

    /// Latest error reported by the writer (does not clear it; a successful
    /// barrier clears the writer's state).
    pub fn peek_error(&self) -> Option<LedgerError> {
        self.shared.error.lock().unwrap().clone()
    }

    /// Takes (and clears) the latest error reported by the writer.
    pub fn take_error(&self) -> Option<LedgerError> {
        self.shared.error.lock().unwrap().take()
    }

    /// Records a gap observed by the encoder under `GapPolicy::Record`.
    pub fn take_gaps(&mut self) -> Vec<SequenceGap> {
        self.encoder.take_gaps()
    }

    pub async fn write_event(&mut self, seq: u64, event: &crate::Event) -> Result<(), LedgerError> {
        if let Some(e) = self.peek_error() {
            return Err(e);
        }
        self.encoder.push_event(seq, event, &mut self.pool)?;
        if self.encoder.event_count() as usize >= self.config.block_max_events
            || self.encoder.bytes_len() >= self.config.block_target_bytes
        {
            self.drain_block().await?;
        }
        Ok(())
    }

    pub async fn flush_scientific(&mut self) -> Result<(), LedgerError> {
        self.drain_block().await?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(LedgerCommand::Barrier { reply: reply_tx })
            .await
            .map_err(|_| LedgerError::WriterClosed)?;
        reply_rx.await.map_err(|_| LedgerError::WriterClosed)??;
        Ok(())
    }

    pub async fn flush(&self) -> Result<LedgerPosition, LedgerError> {
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(LedgerCommand::Barrier { reply: reply_tx })
            .await
            .map_err(|_| LedgerError::WriterClosed)?;
        reply_rx.await.map_err(|_| LedgerError::WriterClosed)?
    }

    pub async fn rotate(&self) -> Result<(), LedgerError> {
        self.tx
            .send(LedgerCommand::Rotate)
            .await
            .map_err(|_| LedgerError::WriterClosed)
    }

    pub async fn finish(mut self) -> Result<ClosedSegment, LedgerError> {
        self.drain_block().await?;
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        self.tx
            .send(LedgerCommand::Finish { reply: reply_tx })
            .await
            .map_err(|_| LedgerError::WriterClosed)?;
        reply_rx.await.map_err(|_| LedgerError::WriterClosed)?
    }

    async fn drain_block(&mut self) -> Result<(), LedgerError> {
        if let Some(block) = self.encoder.take_nonempty_block(&mut self.pool)? {
            self.tx
                .send(LedgerCommand::Block(block))
                .await
                .map_err(|_| LedgerError::WriterClosed)?;
        }
        Ok(())
    }

    pub fn next_sequence(&self) -> u64 {
        self.encoder.next_sequence()
    }
}

// ---------------------------------------------------------------------------
// Spawn helpers
// ---------------------------------------------------------------------------

pub fn spawn_ledger_writer(
    path: PathBuf,
    header: SegmentHeader,
    capacity: usize,
) -> Result<EventSink, LedgerError> {
    spawn_ledger_writer_with(path, header, capacity, LedgerConfig::default())
}

pub fn spawn_ledger_writer_with(
    path: PathBuf,
    header: SegmentHeader,
    capacity: usize,
    config: LedgerConfig,
) -> Result<EventSink, LedgerError> {
    let writer = LedgerWriter::create_with(path, header.clone(), config.clone())?;
    spawn_ledger_writer_over(writer, capacity, config)
}

/// Spawns the writer thread over an existing writer (used by tests to inject
/// I/O failures through a custom `Write` implementation).
pub fn spawn_ledger_writer_over<F>(
    mut writer: LedgerWriter<F>,
    capacity: usize,
    config: LedgerConfig,
) -> Result<EventSink, LedgerError>
where
    F: IoWrite + Read + Seek + DurableSync + Send + 'static,
{
    let (tx, mut rx) = tokio::sync::mpsc::channel::<LedgerCommand>(capacity);
    let shared = Arc::new(WriterState::default());
    let writer_shared = shared.clone();
    let first_sequence = writer.header.first_sequence;
    std::thread::Builder::new()
        .name("reflex-ledger-writer".into())
        .spawn(move || {
            while let Some(cmd) = rx.blocking_recv() {
                match cmd {
                    LedgerCommand::Block(block) => {
                        if let Err(e) = writer.queue_block(block) {
                            *writer_shared.error.lock().unwrap() = Some(e);
                        }
                    }
                    LedgerCommand::Barrier { reply } => {
                        let res = writer.barrier();
                        if let Err(e) = &res {
                            *writer_shared.error.lock().unwrap() = Some(e.clone());
                        } else {
                            writer_shared.error.lock().unwrap().take();
                        }
                        let _ = reply.send(res);
                    }
                    LedgerCommand::Rotate => {
                        let _ = writer.sync_file();
                    }
                    LedgerCommand::Finish { reply } => {
                        let res = writer.finish();
                        if let Err(e) = &res {
                            *writer_shared.error.lock().unwrap() = Some(e.clone());
                        }
                        let _ = reply.send(res);
                        return;
                    }
                }
            }
            let _ = writer.sync_file();
        })
        .expect("spawn ledger writer thread");

    Ok(EventSink::new(tx, shared, first_sequence, config))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::recovery::recover_segment;
    use crate::segment::read_segment_footer;
    use crate::{BufferPool, Event, EventEncoder, LedgerError, SegmentHeader};

    fn make_header() -> SegmentHeader {
        SegmentHeader::new(1, 1, 42, 7, 0, Digest::hash_blake3(b"compat"))
    }

    fn make_resource(i: u64) -> Event {
        Event::ResourceSample(crate::event::ResourceSampleEvent {
            user_cpu_ns: i,
            sys_cpu_ns: i,
            rss_bytes: i,
            timestamp_ns: i,
        })
    }

    fn push_block(writer: &mut LedgerWriter<File>, seq: u64, event: &Event) {
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        enc.push_event(seq, event, &mut pool).unwrap();
        let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        writer.write_block(&block).unwrap();
    }

    #[test]
    fn test_segment_writer_and_recovery() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("test.segment");
        let header = make_header();
        let mut writer = LedgerWriter::create(path.clone(), header.clone()).unwrap();
        for i in 0..10 {
            let next = writer.next_sequence;
            push_block(&mut writer, next, &make_resource(i));
        }
        let closed = writer.finish().unwrap();
        assert_eq!(closed.total_events, 10);
        let recovered = recover_segment(&path).unwrap();
        assert_eq!(recovered.header, header);
        assert_eq!(recovered.blocks.len(), 10);
        let events = recovered.read_events(&path).unwrap();
        assert_eq!(events.len(), 10);
    }

    #[test]
    fn test_finish_writes_footer_and_index() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("f.segment");
        let header = make_header();
        let mut writer = LedgerWriter::create(path.clone(), header).unwrap();
        // 8 blocks of 1024 small events each (~35 KiB per block, matching
        // production block sizes) so the index-overhead gate is meaningful.
        let mut pool = BufferPool::new(1024);
        for b in 0..8u64 {
            let mut enc = EventEncoder::new();
            for i in 0..1024u64 {
                enc.push_event(b * 1024 + i, &make_resource(b * 1024 + i), &mut pool)
                    .unwrap();
            }
            let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
            writer.write_block(&block).unwrap();
        }
        let closed = writer.finish().unwrap();
        assert_eq!(closed.total_blocks, 8);

        let footer = read_segment_footer(&path).unwrap().expect("footer");
        assert_eq!(footer.total_blocks, 8);
        assert_eq!(footer.total_events, 8 * 1024);
        assert_eq!(footer.block_index.len(), 8);
        assert_eq!(footer.segment_digest, closed.segment_digest);

        let idx = crate::segment::read_segment_index(&path)
            .unwrap()
            .expect("idx");
        assert_eq!(idx.entries.len(), 8);
        // Index size well under 1% of segment size for 64 KiB-class blocks.
        let seg_len = std::fs::metadata(&path).unwrap().len();
        let idx_len = std::fs::metadata(crate::segment::sidecar_index_path(&path))
            .unwrap()
            .len();
        assert!((idx_len as f64) < (seg_len as f64) * 0.01);
    }

    #[test]
    fn test_writer_sequence_validation() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("seq.segment");
        let mut writer = LedgerWriter::create(path, make_header()).unwrap();
        push_block(&mut writer, 0, &make_resource(0));
        // Gap: writer must reject non-contiguous block.
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        enc.push_event(5, &make_resource(5), &mut pool).unwrap();
        let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        let err = writer.write_block(&block).unwrap_err();
        assert!(matches!(
            err,
            LedgerError::SequenceMismatch {
                expected: 1,
                found: 5,
                ..
            }
        ));
    }

    /// Failing Write/Read/Seek double that fails writes once the file length
    /// would exceed a byte budget held in a shared `Arc`, so tests can heal
    /// the failure mid-test while the writer owns the double.
    struct FailAfterShared {
        inner: File,
        budget: Arc<Mutex<usize>>,
    }

    impl FailAfterShared {
        fn new(inner: File, budget: Arc<Mutex<usize>>) -> Self {
            Self { inner, budget }
        }
    }

    impl IoWrite for FailAfterShared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            let budget = *self.budget.lock().unwrap();
            if self.inner.metadata().map(|m| m.len()).unwrap_or(0) + buf.len() as u64
                > budget as u64
            {
                return Err(std::io::Error::other("injected write failure"));
            }
            self.inner.write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    impl Read for FailAfterShared {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inner.read(buf)
        }
    }

    impl Seek for FailAfterShared {
        fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(pos)
        }
    }

    impl DurableSync for FailAfterShared {
        fn sync_durable(&mut self) -> std::io::Result<()> {
            self.inner.sync_all()
        }
    }

    #[test]
    fn test_never_drop_on_write_failure() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("nd.segment");
        let raw = File::create(&path).unwrap();
        // Budget: header (61) + block 0 (68 bytes) fits; block 1 overflows.
        let budget = std::sync::Arc::new(Mutex::new(61 + 68 + 64));
        let fail = FailAfterShared::new(raw, budget.clone());
        let header = make_header();
        let config = LedgerConfig {
            write_retries: 0,
            ..LedgerConfig::default()
        };
        let mut writer = LedgerWriter::with_file(fail, path.clone(), header, config).unwrap();
        let mut pool = BufferPool::new(1024);

        let mut enc = EventEncoder::new();
        enc.push_event(0, &make_resource(0), &mut pool).unwrap();
        let b0 = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        writer.queue_block(b0).unwrap();

        let mut enc = EventEncoder::new();
        enc.push_event(1, &make_resource(1), &mut pool).unwrap();
        let b1 = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        // This write fails (budget exhausted) — the block must not be dropped.
        assert!(writer.queue_block(b1).is_err());
        assert!(writer.is_failed());
        assert!(writer.last_error().is_some());

        // The failed block must still be retried by the next barrier.
        *budget.lock().unwrap() = usize::MAX;
        let pos = writer.barrier().unwrap();
        assert_eq!(pos.sequence, 2);
        assert!(!writer.is_failed());

        // The file must contain both blocks' events after recovery.
        let recovered = recover_segment(&path).unwrap();
        assert_eq!(recovered.report.blocks_valid, 2);
        let events = recovered.read_events(&path).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_never_drop_pending_order_preserved() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("nd2.segment");
        let raw = File::create(&path).unwrap();
        let budget = std::sync::Arc::new(Mutex::new(61 + 68 + 64));
        let fail = FailAfterShared::new(raw, budget.clone());
        let config = LedgerConfig {
            write_retries: 0,
            ..LedgerConfig::default()
        };
        let mut writer =
            LedgerWriter::with_file(fail, path.clone(), make_header(), config).unwrap();
        let mut pool = BufferPool::new(1024);

        let mut enc = EventEncoder::new();
        enc.push_event(0, &make_resource(0), &mut pool).unwrap();
        let b0 = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        writer.queue_block(b0).unwrap();

        let mut enc = EventEncoder::new();
        enc.push_event(1, &make_resource(1), &mut pool).unwrap();
        let b1 = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        assert!(writer.queue_block(b1).is_err());

        // A second failure while a pending block exists: the new block is
        // queued behind the failed one; sequence order is preserved.
        let mut enc = EventEncoder::new();
        enc.push_event(2, &make_resource(2), &mut pool).unwrap();
        let b2 = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        assert!(writer.queue_block(b2).is_err());

        *budget.lock().unwrap() = usize::MAX;
        writer.barrier().unwrap();
        let events = recover_segment(&path).unwrap().read_events(&path).unwrap();
        let seqs: Vec<u64> = events.iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn test_async_sink_error_surfacing() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("async_fail.segment");
        let raw = File::create(&path).unwrap();
        // Header fits (63 bytes); the first block write overflows.
        let budget = std::sync::Arc::new(Mutex::new(61 + 64));
        let fail = FailAfterShared::new(raw, budget.clone());
        let config = LedgerConfig {
            write_retries: 0,
            ..LedgerConfig::default()
        };
        let writer =
            LedgerWriter::with_file(fail, path.clone(), make_header(), config.clone()).unwrap();
        let mut sink = spawn_ledger_writer_over(writer, 64, config).unwrap();

        sink.write_event(0, &make_resource(0)).await.unwrap();
        let err = sink.flush_scientific().await.unwrap_err();
        assert!(matches!(err, LedgerError::Io(_)));
        assert!(sink.peek_error().is_some());
        // The failed block is retained; the next append surfaces the error
        // (the event is not buffered while the writer is failed).
        let err = sink.write_event(1, &make_resource(1)).await.unwrap_err();
        assert!(matches!(err, LedgerError::Io(_)));
        assert!(sink.take_error().is_some());
        // Heal: the pending block drains on the next flush, after which
        // appends are buffered again.
        *budget.lock().unwrap() = usize::MAX;
        sink.flush_scientific().await.unwrap();
        sink.write_event(1, &make_resource(1)).await.unwrap();
        let closed = sink.finish().await.unwrap();
        assert_eq!(closed.total_events, 2);
    }

    #[tokio::test]
    async fn test_async_sink_error_heals_and_finishes() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("async_heal.segment");
        let raw = File::create(&path).unwrap();
        // Keep a handle so we can flip the failure off mid-test.
        let shared_mode = std::sync::Arc::new(Mutex::new(61 + 64));
        let budget = shared_mode.clone();
        let fail = FailAfterShared::new(raw, shared_mode);
        let config = LedgerConfig {
            write_retries: 0,
            ..LedgerConfig::default()
        };
        let writer =
            LedgerWriter::with_file(fail, path.clone(), make_header(), config.clone()).unwrap();
        let mut sink = spawn_ledger_writer_over(writer, 64, config).unwrap();

        sink.write_event(0, &make_resource(0)).await.unwrap();
        assert!(sink.flush_scientific().await.is_err());

        // Heal the writer; the pending block must be retried on the next
        // flush, and finish must succeed with all events durable.
        *budget.lock().unwrap() = usize::MAX;
        sink.flush_scientific().await.unwrap();
        sink.write_event(1, &make_resource(1)).await.unwrap();
        let closed = sink.finish().await.unwrap();
        assert_eq!(closed.total_events, 2);
        let events = recover_segment(&path).unwrap().read_events(&path).unwrap();
        assert_eq!(events.len(), 2);
        let seqs: Vec<u64> = events.iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, vec![0, 1]);
    }
}
