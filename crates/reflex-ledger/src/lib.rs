//! Reflex evidence ledger (§8) — compact binary event segments.
//!
//! Public surface (stable for consumers):
//! - event families (`Event` + per-family structs, `SequencedEvent`)
//! - segment binary codecs (`SegmentHeader`, `BlockHeader`, `SegmentFooter`,
//!   `IndexFile`, `BlockIndexEntry`, `IndexEntry`)
//! - writer: `LedgerWriter`, `spawn_ledger_writer`, `EventSink`, `LedgerCommand`
//! - recovery: `recover_segment`, `RecoveredSegment`, `SegmentReader`,
//!   `quarantine_corrupt_segment`, `merge_streams`
//! - encoding: `EventEncoder`, `EncodedBlock`, `BufferPool`
//!
//! The on-disk format is compact binary (no NDJSON/JSON); JSON is available
//! only as a debug/tooling export via `Event::to_json`/`Event::from_json`.

#![forbid(unsafe_code)]

mod codec;
mod encoder;
mod event;
mod recovery;
mod segment;
mod writer;

pub use codec::candidate_row_bytes;
pub use encoder::{EncodedBlock, EventEncoder};
pub use event::{
    ArtifactConstructionEvent, AttemptLifecycleEvent, CacheObservationEvent, CancellationEvent,
    CancellationTarget, CandidateApplicationEvent, CandidateBatchEvent, CellLifecycleEvent,
    EpisodeEndEvent, EpisodeStartEvent, Event, ExperimentLifecycleEvent, FeatureRefEvent,
    GenerationLifecycleEvent, IncidentEvent, LineageEdgeEvent, ModelShadowScoreEvent,
    PolicyScoreEvent, ResourceSampleEvent, SequencedEvent, StateDiscoveryEvent, TaskEndEvent,
    TaskStartEvent, UtilityObservationEvent, VerificationReceiptEvent, WorkerSessionEvent,
};
pub use recovery::{
    CorruptionKind, MergeReport, QuarantineReport, RecoveredSegment, RecoveryReport, SegmentReader,
    SequenceGap, merge_streams, quarantine_corrupt_segment, recover_segment,
};
pub use segment::{
    BlockHeader, BlockIndexEntry, IndexEntry, IndexFile, SegmentFooter, SegmentHeader,
    build_segment_index, read_segment_footer, read_segment_index, sidecar_index_path,
};
pub use writer::{
    DurableSync, EventSink, LedgerWriter, spawn_ledger_writer, spawn_ledger_writer_over,
    spawn_ledger_writer_with,
};

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;
use tokio::sync::oneshot;

pub const SEGMENT_MAGIC: &[u8; 8] = segment::SEGMENT_MAGIC;
pub const FOOTER_MAGIC: &[u8; 8] = segment::FOOTER_MAGIC;
pub const FOOTER_END_MAGIC: &[u8; 8] = segment::FOOTER_END_MAGIC;
pub const INDEX_MAGIC: &[u8; 8] = segment::INDEX_MAGIC;
pub const MAX_BLOCK_BYTES: u32 = 16 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum LedgerError {
    #[error("io error: {0}")]
    Io(String),
    #[error("invalid segment magic: {0:?}")]
    InvalidMagic([u8; 8]),
    #[error("unsupported schema version: {0}")]
    UnsupportedVersion(u32),
    #[error("oversized block at offset {offset}: {size} bytes")]
    OversizedBlock { offset: u64, size: u32 },
    #[error("mid-file corruption at block offset {offset}")]
    MidFileCorruption { offset: u64 },
    #[error("sequence gap or overlap at offset {offset}: expected {expected}, found {found}")]
    SequenceMismatch {
        offset: u64,
        expected: u64,
        found: u64,
    },
    #[error("writer closed")]
    WriterClosed,
    #[error("encoding error: {0}")]
    Encoding(String),
    #[error("segment file too short")]
    FileTooShort,
    #[error("truncated block payload at offset {offset}")]
    TruncatedPayload { offset: u64 },
}

impl From<std::io::Error> for LedgerError {
    fn from(e: std::io::Error) -> Self {
        LedgerError::Io(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// How the writer treats non-contiguous sequences (F-30 / P3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GapPolicy {
    /// Reject gaps, duplicates, and backwards moves (default).
    #[default]
    Reject,
    /// Accept gaps, recording them for later inspection.
    Record,
}

/// Writer configuration (§8.3).
#[derive(Clone, Debug)]
pub struct LedgerConfig {
    /// Target block payload size in bytes (blocks are packed until the next
    /// event would exceed this).
    pub block_target_bytes: usize,
    /// Maximum events per block.
    pub block_max_events: usize,
    /// Immediate retries per block write before the block is re-queued.
    pub write_retries: u32,
    /// Delay between immediate retries.
    pub retry_delay: std::time::Duration,
    /// Sequence validation policy at write time.
    pub gap_policy: GapPolicy,
    /// Write the sidecar `<segment>.idx` index at finish().
    pub write_index: bool,
}

impl Default for LedgerConfig {
    fn default() -> Self {
        Self {
            block_target_bytes: 64 * 1024,
            block_max_events: 512,
            write_retries: 2,
            retry_delay: std::time::Duration::ZERO,
            gap_policy: GapPolicy::Reject,
            write_index: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Positions and closed-segment metadata
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerPosition {
    pub offset: u64,
    pub sequence: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClosedSegment {
    pub header: SegmentHeader,
    pub total_blocks: u64,
    pub total_events: u64,
    pub total_bytes: u64,
    pub segment_digest: reflex_types::Digest,
}

impl fmt::Display for ClosedSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "segment stream={} blocks={} events={} bytes={} digest={}",
            self.header.stream_id,
            self.total_blocks,
            self.total_events,
            self.total_bytes,
            self.segment_digest
        )
    }
}

// ---------------------------------------------------------------------------
// Writer commands (§8.3)
// ---------------------------------------------------------------------------

pub enum LedgerCommand {
    Block(EncodedBlock),
    Barrier {
        reply: oneshot::Sender<Result<LedgerPosition, LedgerError>>,
    },
    Rotate,
    Finish {
        reply: oneshot::Sender<Result<ClosedSegment, LedgerError>>,
    },
}

// ---------------------------------------------------------------------------
// BufferPool — reusable buffers (API kept for compatibility; the encoder now
// reuses a single BytesMut internally and takes no heap allocations per event)
// ---------------------------------------------------------------------------

pub struct BufferPool {
    buffers: Vec<Vec<u8>>,
    capacity: usize,
}

impl BufferPool {
    pub fn new(capacity: usize) -> Self {
        Self {
            buffers: Vec::new(),
            capacity,
        }
    }

    pub fn acquire(&mut self) -> Vec<u8> {
        self.buffers
            .pop()
            .unwrap_or_else(|| Vec::with_capacity(self.capacity))
    }

    pub fn release(&mut self, mut buf: Vec<u8>) {
        buf.clear();
        if self.buffers.len() < 64 {
            self.buffers.push(buf);
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::ResourceSampleEvent;
    use bytes::BytesMut;

    fn make_header() -> SegmentHeader {
        SegmentHeader::new(1, 1, 42, 7, 0, reflex_types::Digest::hash_blake3(b"compat"))
    }

    fn make_event(n: u64) -> Event {
        Event::ExperimentLifecycle(crate::event::ExperimentLifecycleEvent {
            experiment_id: reflex_types::ExperimentId::from_digest(
                reflex_types::Digest::hash_blake3(&n.to_le_bytes()),
            ),
            action: format!("test-{n}"),
            timestamp_ns: n * 1000,
        })
    }

    #[test]
    fn test_buffer_pool_reuse() {
        let mut pool = BufferPool::new(1024);
        let buf1 = pool.acquire();
        assert!(buf1.is_empty());
        pool.release(buf1);
        let buf2 = pool.acquire();
        assert!(buf2.is_empty());
        pool.release(buf2);
    }

    #[tokio::test]
    async fn test_async_event_sink() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("async.segment");
        let header = make_header();

        let mut sink = spawn_ledger_writer(path.clone(), header, 64).unwrap();

        for i in 0..5 {
            sink.write_event(i, &make_event(i)).await.unwrap();
        }

        let closed = sink.finish().await.unwrap();
        assert_eq!(closed.total_events, 5);

        let recovered = recover_segment(&path).unwrap();
        let events = recovered.read_events(&path).unwrap();
        assert_eq!(events.len(), 5);
    }

    #[tokio::test]
    async fn test_flush_scientific() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("sci.segment");
        let header = make_header();

        let mut sink = spawn_ledger_writer(path.clone(), header, 64).unwrap();

        sink.write_event(0, &make_event(0)).await.unwrap();
        sink.flush_scientific().await.unwrap();

        let closed = sink.finish().await.unwrap();
        assert_eq!(closed.total_events, 1);
    }

    #[tokio::test]
    async fn test_gap_rejected_at_sink() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("gap_sink.segment");
        let mut sink = spawn_ledger_writer(path, make_header(), 64).unwrap();
        sink.write_event(0, &make_event(0)).await.unwrap();
        let err = sink.write_event(2, &make_event(2)).await.unwrap_err();
        assert!(matches!(err, LedgerError::SequenceMismatch { .. }));
    }

    #[test]
    fn test_golden_segment_digest() {
        // P3.2: deterministic format — identical input produces identical
        // bytes on every architecture (all integers are little-endian, IDs are
        // raw 32-byte blake3 digests, framing is fixed). This pins the exact
        // wire bytes of a fixed event set; changing the format breaks it.
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("golden.segment");
        let header = make_header();
        let mut writer = LedgerWriter::create(path.clone(), header).unwrap();
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        // 10 candidate score events behind one batch (representative hot path).
        let batch = crate::event::tests::make_all_events()
            .into_iter()
            .find_map(|e| match e {
                Event::CandidateBatch(b) => Some(Event::CandidateBatch(b)),
                _ => None,
            })
            .unwrap();
        enc.push_event(0, &batch, &mut pool).unwrap();
        let score = crate::event::tests::make_all_events()
            .into_iter()
            .find_map(|e| match e {
                Event::PolicyScore(p) => Some(p),
                _ => None,
            })
            .unwrap();
        for i in 0..10u64 {
            let mut ev = score.clone();
            ev.scores = vec![0.5 + i as f32 * 0.05, 0.5 - i as f32 * 0.05];
            enc.push_event(1 + i, &Event::PolicyScore(ev), &mut pool)
                .unwrap();
        }
        let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        writer.write_block(&block).unwrap();
        let _closed = writer.finish().unwrap();

        let file_bytes = std::fs::read(&path).unwrap();
        let digest = reflex_types::Digest::hash_blake3(&file_bytes);
        // The chain digest covers the header and blocks only, not the footer;
        // the file-level digest is the format fingerprint for this golden
        // segment (pinned: change only with an ADR revising the wire format).
        assert_eq!(
            digest.to_hex(),
            "blake3:47ace47985d62ef3ecae8e7d373278da4dd9d0c821a9bfd8317847cf70f3e16c"
        );
    }

    #[test]
    fn test_decoder_never_panics_on_garbage() {
        // Fuzz-shaped: arbitrary bytes must never panic the decoders.
        let mut seed = 0x9E3779B97F4A7C15u64;
        for _ in 0..2000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let len = (seed % 512) as usize;
            let mut data = vec![0u8; len];
            for b in data.iter_mut() {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                *b = (seed >> 33) as u8;
            }
            let buf = BytesMut::from(&data[..]);
            let mut dec = codec::Dec::new(&buf);
            if !buf.is_empty() {
                let tag = buf[0];
                let mut out = Event::ResourceSample(ResourceSampleEvent::default());
                let _ = crate::event::decode_event(&mut dec, tag, &mut out);
            }
        }
    }

    // P3.1: transition outcome first-class encoding (manifest locates test in lib.rs).
    #[test]
    fn test_transition_outcome_and_group_semantics() {
        use reflex_types::{AndGroupRef, StateHandle, TransitionOutcome};
        let group = AndGroupRef {
            group_id: 1,
            children: vec![StateHandle::new(2, 0)],
        };
        assert!(!group.children.is_empty());
        let outcome = TransitionOutcome::Obligations { group };
        assert!(matches!(
            &outcome,
            TransitionOutcome::Obligations { group: g } if g.group_id == 1
        ));
        assert_eq!(outcome.group_id().expect("obligations carry a group"), 1);
    }

    #[test]
    fn test_transition_outcome_roundtrip() {
        use reflex_types::{
            ArtifactId, InvalidCandidateCode, StateHandle, TransitionOutcome, UnresolvedCode,
        };
        let states = vec![StateHandle::new(0, 1), StateHandle::new(1, 1)];
        let outcomes = vec![
            TransitionOutcome::closed(ArtifactId::from_digest(reflex_types::Digest::hash_blake3(
                b"a",
            ))),
            TransitionOutcome::obligations(7, states.clone()),
            TransitionOutcome::contradiction(),
            TransitionOutcome::invalid(InvalidCandidateCode::Illegal),
            TransitionOutcome::unresolved(UnresolvedCode::BudgetExhausted),
        ];
        for outcome in &outcomes {
            let json = serde_json::to_string(outcome).unwrap();
            let decoded: TransitionOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(*outcome, decoded);
        }
    }

    // P3.2 / P3.4 manifest greps — thin wrappers over segment/recovery module tests.
    #[test]
    fn test_segment_header_roundtrip() {
        let header = make_header();
        let encoded = header.encode();
        let (decoded, consumed) = SegmentHeader::decode(&encoded).unwrap();
        assert_eq!(consumed, SegmentHeader::SIZE);
        assert_eq!(decoded, header);
    }

    #[test]
    fn test_block_header_roundtrip() {
        let bh = BlockHeader {
            stored_length: 1234,
            uncompressed_length: 5678,
            event_count: 42,
            first_sequence: 100,
            last_sequence: 141,
            flags: 0x01,
            crc32c: 0xDEADBEEF,
        };
        let encoded = bh.encode();
        let decoded = BlockHeader::decode(&encoded).unwrap();
        assert_eq!(decoded, bh);
    }

    #[test]
    fn test_recover_crc_mismatch() {
        use crate::event::ResourceSampleEvent;
        use std::fs::OpenOptions;
        use std::io::{Seek, SeekFrom, Write};

        fn make_resource(n: u64) -> Event {
            Event::ResourceSample(ResourceSampleEvent {
                user_cpu_ns: n,
                sys_cpu_ns: n,
                rss_bytes: n * 1024,
                timestamp_ns: n * 1000,
            })
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("crc.segment");
        let header = make_header();
        let mut writer = LedgerWriter::create(path.clone(), header).unwrap();
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        enc.push_event(0, &make_resource(0), &mut pool).unwrap();
        let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        writer.write_block(&block).unwrap();
        writer.finish().unwrap();

        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let header_size = SegmentHeader::SIZE as u64;
        let block_hdr_size = BlockHeader::SIZE as u64;
        f.seek(SeekFrom::Start(header_size + block_hdr_size + 2))
            .unwrap();
        f.write_all(b"XX").unwrap();
        drop(f);

        let result = recover_segment(&path);
        assert!(matches!(result, Err(LedgerError::MidFileCorruption { .. })));
    }

    #[test]
    fn test_recover_torn_tail() {
        use crate::event::ResourceSampleEvent;
        use std::fs::OpenOptions;

        fn make_resource(n: u64) -> Event {
            Event::ResourceSample(ResourceSampleEvent {
                user_cpu_ns: n,
                sys_cpu_ns: n,
                rss_bytes: n * 1024,
                timestamp_ns: n * 1000,
            })
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("torn.segment");
        let header = make_header();
        let mut writer = LedgerWriter::create(path.clone(), header).unwrap();
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        for i in 0..4u64 {
            enc.push_event(i, &make_resource(i), &mut pool).unwrap();
        }
        let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        writer.write_block(&block).unwrap();
        writer.finish().unwrap();

        let block_offset = {
            let r = recover_segment(&path).unwrap();
            r.blocks[0].offset
        };
        let truncate_at = block_offset + BlockHeader::SIZE as u64 + 5;
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.set_len(truncate_at).unwrap();
        drop(f);

        let recovered = recover_segment(&path).unwrap();
        assert!(recovered.report.torn_tail_truncated);
        let events = recovered.read_events(&path).unwrap();
        assert!(events.len() < 4);
    }

    #[test]
    fn test_segment_writer_and_recovery() {
        use crate::event::ResourceSampleEvent;

        fn make_resource(n: u64) -> Event {
            Event::ResourceSample(ResourceSampleEvent {
                user_cpu_ns: n,
                sys_cpu_ns: n,
                rss_bytes: n * 1024,
                timestamp_ns: n * 1000,
            })
        }

        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("writer.segment");
        let header = make_header();
        let mut writer = LedgerWriter::create(path.clone(), header.clone()).unwrap();
        for i in 0..10u64 {
            let mut enc = EventEncoder::new();
            let mut pool = BufferPool::new(1024);
            enc.push_event(i, &make_resource(i), &mut pool).unwrap();
            let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
            writer.write_block(&block).unwrap();
        }
        let closed = writer.finish().unwrap();
        assert_eq!(closed.total_events, 10);
        let recovered = recover_segment(&path).unwrap();
        assert_eq!(recovered.header, header);
        assert_eq!(recovered.blocks.len(), 10);
    }
}
