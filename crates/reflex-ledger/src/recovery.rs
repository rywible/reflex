//! Crash recovery (§28.4), streaming reader, corruption quarantine, and the
//! stream merger (P3.4, F-28, F-29).
//!
//! Recovery scans block by block with a single reusable buffer. A torn tail
//! (EOF mid-header or mid-payload) truncates the file to the last valid block
//! boundary and is recorded in `RecoveryReport`. A full-length payload with a
//! bad CRC is `MidFileCorruption` (quarantine territory, never silently
//! swallowed). Sequence gaps between blocks are reported; overlaps are errors.
//! Recovery is independent of the footer: a missing or torn footer never loses
//! prior complete blocks.

use crate::codec::Dec;
use crate::event::{CandidateUniverse, SequencedEvent};
use crate::segment::{
    BlockHeader, BlockIndexEntry, FOOTER_MAGIC, IndexEntry, IndexFile, SegmentFooter,
    SegmentHeader, sidecar_index_path,
};
use crate::{
    BufferPool, Event, EventEncoder, GapPolicy, LedgerConfig, LedgerError, MAX_BLOCK_BYTES,
};
use bytes::BytesMut;
use crc32c::crc32c;
use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Report types
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SequenceGap {
    pub expected: u64,
    pub found: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryReport {
    pub blocks_scanned: u64,
    pub blocks_valid: u64,
    pub torn_tail_truncated: bool,
    pub truncated_bytes: u64,
    pub first_bad_block: Option<u64>,
    pub recovered_events: u64,
    pub max_seq: u64,
    pub gaps: Vec<SequenceGap>,
    pub footer_valid: bool,
    pub index_rebuilt: bool,
}

#[derive(Clone, Debug)]
pub struct RecoveredSegment {
    pub header: SegmentHeader,
    pub blocks: Vec<BlockIndexEntry>,
    pub valid_bytes: u64,
    pub report: RecoveryReport,
    pub footer: Option<SegmentFooter>,
}

impl RecoveredSegment {
    /// Decodes every event in the recovered prefix (tooling path; allocates).
    pub fn read_events(&self, path: &Path) -> Result<Vec<SequencedEvent>, LedgerError> {
        let mut reader = SegmentReader::from_recovered(self.clone(), path)?;
        let capacity: usize = self
            .blocks
            .iter()
            .map(|b| b.header.event_count as usize)
            .sum();
        let mut events = Vec::with_capacity(capacity);
        let mut out = SequencedEvent {
            sequence: 0,
            event: Event::ResourceSample(crate::event::ResourceSampleEvent::default()),
        };
        while reader.next_event(&mut out)? {
            events.push(out.clone());
        }
        Ok(events)
    }
}

// ---------------------------------------------------------------------------
// recover_segment — §28.4
// ---------------------------------------------------------------------------

pub fn recover_segment(path: &Path) -> Result<RecoveredSegment, LedgerError> {
    let mut file = open_for_recovery(path)?;

    let mut header_buf = [0u8; SegmentHeader::SIZE];
    match file.read_exact(&mut header_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(LedgerError::FileTooShort);
        }
        Err(e) => return Err(e.into()),
    }
    let (header, header_size) = SegmentHeader::decode(&header_buf)?;

    let mut payload = BytesMut::with_capacity(256 * 1024);
    let mut blocks: Vec<BlockIndexEntry> = Vec::new();
    let mut index_classes: Vec<u64> = Vec::new();
    let mut expected = header.first_sequence;
    let mut valid_bytes = header_size as u64;
    let mut blocks_scanned = 0u64;
    let mut gaps: Vec<SequenceGap> = Vec::new();
    let mut torn_offset: Option<u64> = None;
    let mut digest = *blake3::hash(&header_buf).as_bytes();
    let mut scratch = SequencedEvent {
        sequence: 0,
        event: Event::ResourceSample(crate::event::ResourceSampleEvent::default()),
    };

    loop {
        let offset = file.stream_position()?;
        let mut bh_buf = [0u8; BlockHeader::SIZE];
        match file.read_exact(&mut bh_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                torn_offset = Some(offset);
                break;
            }
            Err(e) => return Err(e.into()),
        }
        // A footer (clean finish) ends the block scan.
        if &bh_buf[0..8] == FOOTER_MAGIC {
            break;
        }
        let block_header = BlockHeader::decode(&bh_buf)?;
        if block_header.stored_length > MAX_BLOCK_BYTES {
            return Err(LedgerError::OversizedBlock {
                offset,
                size: block_header.stored_length,
            });
        }
        if block_header.first_sequence > block_header.last_sequence {
            return Err(LedgerError::SequenceMismatch {
                offset,
                expected,
                found: block_header.first_sequence,
            });
        }
        blocks_scanned += 1;
        payload.clear();
        payload.resize(block_header.stored_length as usize, 0);
        if let Err(e) = file.read_exact(&mut payload) {
            if e.kind() == std::io::ErrorKind::UnexpectedEof {
                torn_offset = Some(offset);
                break;
            }
            return Err(e.into());
        }
        if crc32c(&payload) != block_header.crc32c {
            return Err(LedgerError::MidFileCorruption { offset });
        }
        if block_header.first_sequence < expected {
            return Err(LedgerError::SequenceMismatch {
                offset,
                expected,
                found: block_header.first_sequence,
            });
        }
        if block_header.first_sequence > expected {
            gaps.push(SequenceGap {
                expected,
                found: block_header.first_sequence,
            });
        }
        expected = block_header.last_sequence.wrapping_add(1);
        valid_bytes = offset + BlockHeader::SIZE as u64 + u64::from(block_header.stored_length);
        let mut h = blake3::Hasher::new();
        h.update(&digest);
        h.update(&bh_buf);
        h.update(&payload);
        digest = *h.finalize().as_bytes();
        index_classes.push(crate::codec::payload_classes(&payload, &mut scratch)?);
        blocks.push(BlockIndexEntry {
            offset,
            header: block_header,
        });
    }

    let mut file_len = file.seek(SeekFrom::End(0))?;
    let mut report = RecoveryReport {
        blocks_scanned,
        blocks_valid: blocks.len() as u64,
        torn_tail_truncated: false,
        truncated_bytes: 0,
        first_bad_block: None,
        recovered_events: blocks.iter().map(|b| u64::from(b.header.event_count)).sum(),
        max_seq: blocks
            .last()
            .map_or(header.first_sequence.wrapping_sub(1), |b| {
                b.header.last_sequence
            }),
        gaps,
        footer_valid: false,
        index_rebuilt: false,
    };

    if let Some(t) = torn_offset {
        report.first_bad_block = Some(t);
        if file_len > valid_bytes {
            file.seek(SeekFrom::Start(valid_bytes))?;
            file.set_len(valid_bytes)?;
            report.torn_tail_truncated = true;
            report.truncated_bytes = file_len - valid_bytes;
            file_len = valid_bytes;
        }
    }

    // Footer validation (independent of block scan; a missing/torn footer
    // never loses prior complete blocks).
    let footer = read_footer(&mut file, valid_bytes, file_len)?;
    match &footer {
        Some(f) => {
            let digest_matches = f.segment_digest.as_bytes() == &digest;
            report.footer_valid = digest_matches
                && f.total_blocks as usize == blocks.len()
                && f.total_events as usize == report.recovered_events as usize;
        }
        None => {
            // Torn or missing footer: drop the partial footer bytes so the
            // segment ends exactly at the last valid block boundary.
            if file_len > valid_bytes {
                file.seek(SeekFrom::Start(valid_bytes))?;
                file.set_len(valid_bytes)?;
                report.torn_tail_truncated = true;
                report.truncated_bytes = file_len - valid_bytes;
                file_len = valid_bytes;
            }
        }
    }

    // Rebuild the sidecar index when the segment was not cleanly finished
    // (missing/torn footer or no index). Deterministic: identical input always
    // produces identical index bytes.
    if report.footer_valid {
        // Rebuild the sidecar index when the footer is valid but the index is
        // missing (operator wiped it, or a tool created the segment directly).
        report.index_rebuilt = if sidecar_index_path(path).exists() {
            false
        } else {
            rebuild_index_file(path, &blocks, &index_classes, file_len)?
        };
    } else {
        report.index_rebuilt = rebuild_index_file(path, &blocks, &index_classes, file_len)?;
    }

    Ok(RecoveredSegment {
        header,
        blocks,
        valid_bytes,
        report,
        footer,
    })
}

fn open_for_recovery(path: &Path) -> Result<File, LedgerError> {
    match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => Ok(f),
        Err(_) => Ok(File::open(path)?),
    }
}

/// Parses and validates the footer located at `valid_bytes..file_len`, if any.
fn read_footer(
    file: &mut File,
    valid_bytes: u64,
    file_len: u64,
) -> Result<Option<SegmentFooter>, LedgerError> {
    let tail = file_len.saturating_sub(valid_bytes);
    if tail < SegmentFooter::FIXED_SIZE as u64 {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(valid_bytes))?;
    let mut buf = vec![0u8; tail as usize];
    file.read_exact(&mut buf)?;
    match SegmentFooter::decode(&buf) {
        Ok(f) => Ok(Some(f)),
        Err(_) => Ok(None),
    }
}

fn rebuild_index_file(
    path: &Path,
    blocks: &[BlockIndexEntry],
    classes: &[u64],
    segment_len: u64,
) -> Result<bool, LedgerError> {
    let idx = IndexFile {
        version: 1,
        segment_len,
        entries: blocks
            .iter()
            .zip(classes.iter())
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
    let idx_path = sidecar_index_path(path);
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&idx_path)?;
    f.write_all(&data)?;
    f.flush()?;
    f.sync_all()?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// SegmentReader — zero-alloc-per-event iteration
// ---------------------------------------------------------------------------

pub struct SegmentReader {
    file: File,
    header: SegmentHeader,
    blocks: Vec<BlockIndexEntry>,
    next_block: usize,
    payload: BytesMut,
    payload_pos: usize,
    events_left: u32,
    prev_seq: u64,
    candidates: CandidateUniverse,
    done: bool,
}

impl SegmentReader {
    /// Opens a segment, running recovery first (truncation and index rebuild
    /// included, per §28.4 reopen semantics).
    pub fn open(path: &Path) -> Result<Self, LedgerError> {
        let recovered = recover_segment(path)?;
        Self::from_recovered(recovered, path)
    }

    pub fn from_recovered(recovered: RecoveredSegment, path: &Path) -> Result<Self, LedgerError> {
        let file = File::open(path)?;
        Ok(Self {
            file,
            header: recovered.header,
            blocks: recovered.blocks,
            next_block: 0,
            payload: BytesMut::with_capacity(256 * 1024),
            payload_pos: 0,
            events_left: 0,
            prev_seq: 0,
            candidates: CandidateUniverse::new(),
            done: false,
        })
    }

    pub fn header(&self) -> &SegmentHeader {
        &self.header
    }

    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// Decodes the next event into `out` (reusing its String/Vec storage;
    /// zero allocation per event in steady state). Returns `Ok(false)` when
    /// the segment is exhausted.
    pub fn next_event(&mut self, out: &mut SequencedEvent) -> Result<bool, LedgerError> {
        if self.done {
            return Ok(false);
        }
        if self.events_left == 0 {
            if self.next_block >= self.blocks.len() {
                self.done = true;
                return Ok(false);
            }
            let entry = &self.blocks[self.next_block];
            self.next_block += 1;
            self.file
                .seek(SeekFrom::Start(entry.offset + BlockHeader::SIZE as u64))?;
            self.payload.clear();
            self.payload.resize(entry.header.stored_length as usize, 0);
            self.file.read_exact(&mut self.payload)?;
            if crc32c(&self.payload) != entry.header.crc32c {
                return Err(LedgerError::MidFileCorruption {
                    offset: entry.offset,
                });
            }
            self.payload_pos = 0;
            self.events_left = entry.header.event_count;
            self.prev_seq = entry.header.first_sequence;
        }

        let mut dec = Dec {
            data: &self.payload[self.payload_pos..],
            pos: 0,
            candidates: Some(&self.candidates),
        };
        let delta = dec.varint()?;
        let tag = dec.u8()?;
        let seq = self.prev_seq.wrapping_add(delta);
        crate::event::decode_event(&mut dec, tag, &mut out.event)?;
        self.payload_pos += dec.pos;
        self.prev_seq = seq;
        self.events_left -= 1;
        out.sequence = seq;

        if tag == crate::event::TAG_CANDIDATE_BATCH
            && let Event::CandidateBatch(batch) = &out.event
        {
            for c in &batch.candidate_ids {
                self.candidates
                    .entry(crate::event::candidate_prefix(c))
                    .or_default()
                    .push(*c);
            }
        }
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Corruption quarantine — P3.4
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum CorruptionKind {
    /// Corrupt bytes at EOF; nothing valid follows.
    TornTail,
    /// Corrupt block followed by additional bytes that are not a valid block.
    MidFileOnly,
    /// Corrupt block followed by at least one valid block.
    MidFileWithValidBlocks,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct QuarantineReport {
    pub original_path: PathBuf,
    pub quarantine_path: PathBuf,
    pub report_path: PathBuf,
    pub kind: CorruptionKind,
    pub first_bad_block: u64,
    pub reason: String,
    pub blocks_before_failure: u64,
    pub valid_bytes: u64,
}

/// Quarantines a corrupt segment instead of silently stopping: the whole file
/// is moved to `quarantine/<segment>.corrupt` (same directory tree) and a JSON
/// report is written next to it. Returns `Ok(None)` when the segment is clean.
pub fn quarantine_corrupt_segment(path: &Path) -> Result<Option<QuarantineReport>, LedgerError> {
    let result = recover_segment(path);
    let err = match result {
        Ok(_) => return Ok(None),
        Err(e) => e,
    };
    let (offset, reason) = match &err {
        LedgerError::MidFileCorruption { offset } => {
            (*offset, "mid-file corruption: crc32c mismatch".to_string())
        }
        LedgerError::OversizedBlock { offset, .. } => (
            *offset,
            "mid-file corruption: oversized block length".to_string(),
        ),
        LedgerError::SequenceMismatch { offset, .. } => {
            (*offset, "mid-file corruption: sequence overlap".to_string())
        }
        other => return Err(other.clone()),
    };

    let kind = classify_corruption(path, offset);
    let dir = path.parent().unwrap_or(Path::new("."));
    let quarantine_dir = dir.join("quarantine");
    std::fs::create_dir_all(&quarantine_dir)?;
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "segment".to_string());
    let quarantine_path = quarantine_dir.join(format!("{file_name}.corrupt"));
    let report_path = quarantine_path.with_extension("corrupt.report.json");

    let valid_bytes = offset;
    let report = QuarantineReport {
        original_path: path.to_path_buf(),
        quarantine_path: quarantine_path.clone(),
        report_path: report_path.clone(),
        kind: kind.clone(),
        first_bad_block: offset,
        reason,
        blocks_before_failure: 0,
        valid_bytes,
    };
    std::fs::rename(path, &quarantine_path)?;
    let json =
        serde_json::to_string_pretty(&report).map_err(|e| LedgerError::Encoding(e.to_string()))?;
    std::fs::write(&report_path, json)?;
    Ok(Some(report))
}

fn classify_corruption(path: &Path, bad_offset: u64) -> CorruptionKind {
    let mut file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return CorruptionKind::TornTail,
    };
    let mut pos = bad_offset;
    // Skip the corrupted block (header + declared payload length if parseable).
    if let Ok(()) = (|| {
        let mut bh_buf = [0u8; BlockHeader::SIZE];
        file.seek(SeekFrom::Start(pos))?;
        file.read_exact(&mut bh_buf)?;
        let bh = BlockHeader::decode(&bh_buf)?;
        pos += BlockHeader::SIZE as u64 + u64::from(bh.stored_length);
        Ok::<(), LedgerError>(())
    })() {
        // Check for a valid block after the corrupt one.
        let mut bh_buf = [0u8; BlockHeader::SIZE];
        if file.seek(SeekFrom::Start(pos)).is_ok() {
            match file.read_exact(&mut bh_buf) {
                Ok(()) => {
                    if let Ok(bh) = BlockHeader::decode(&bh_buf)
                        && bh.stored_length <= MAX_BLOCK_BYTES
                    {
                        return CorruptionKind::MidFileWithValidBlocks;
                    }
                    CorruptionKind::MidFileOnly
                }
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => CorruptionKind::TornTail,
                Err(_) => CorruptionKind::MidFileOnly,
            }
        } else {
            CorruptionKind::MidFileOnly
        }
    } else {
        CorruptionKind::MidFileOnly
    }
}

// ---------------------------------------------------------------------------
// Stream merger — P3.4
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MergeReport {
    pub input_segments: Vec<PathBuf>,
    pub output: PathBuf,
    pub stream_id: u32,
    pub total_events: u64,
    pub gaps: Vec<SequenceGap>,
    /// Sequences present in more than one input segment (kept once).
    pub duplicates: Vec<u64>,
    pub merged_digest: Digest,
}

/// Merges segments from the same stream into one ordered segment. Ordering is
/// by sequence number (producers are merged using explicit causal references,
/// not wall-clock ordering). The merged stream is re-sequenced contiguously;
/// duplicates (same sequence in two inputs) are kept once and reported, and
/// holes in the input union are reported as gaps. Output is written with a
/// footer + sidecar index via the standard writer.
pub fn merge_streams(
    inputs: &[PathBuf],
    output: PathBuf,
    producer_id: u32,
) -> Result<MergeReport, LedgerError> {
    if inputs.is_empty() {
        return Err(LedgerError::Encoding(
            "merge_streams requires at least one input segment".into(),
        ));
    }

    let mut first_header: Option<SegmentHeader> = None;
    let mut by_seq: BTreeMap<u64, Vec<Event>> = BTreeMap::new();
    let mut duplicates = Vec::new();
    let mut scratch = SequencedEvent {
        sequence: 0,
        event: Event::ResourceSample(crate::event::ResourceSampleEvent::default()),
    };

    for path in inputs {
        let recovered = recover_segment(path)?;
        let h = &recovered.header;
        match &first_header {
            None => first_header = Some(recovered.header.clone()),
            Some(fh) => {
                if h.stream_id != fh.stream_id
                    || h.schema_id != fh.schema_id
                    || h.schema_version != fh.schema_version
                {
                    return Err(LedgerError::Encoding(format!(
                        "cannot merge segments with different stream/schema identity \
                         ({} vs {})",
                        path.display(),
                        fh.stream_id
                    )));
                }
            }
        }
        let mut reader = SegmentReader::from_recovered(recovered, path)?;
        while reader.next_event(&mut scratch)? {
            let seq = scratch.sequence;
            // Duplicate sequences across inputs are kept once (first input
            // wins) and reported.
            match by_seq.entry(seq) {
                std::collections::btree_map::Entry::Vacant(e) => {
                    e.insert(vec![scratch.event.clone()]);
                }
                std::collections::btree_map::Entry::Occupied(_) => {
                    duplicates.push(seq);
                }
            }
        }
    }

    let header = first_header.unwrap();
    let start_seq = by_seq.keys().next().copied().unwrap_or(0);
    let mut out_header = header.clone();
    out_header.first_sequence = start_seq;
    out_header.producer_id = producer_id;

    let config = LedgerConfig {
        gap_policy: GapPolicy::Reject,
        ..LedgerConfig::default()
    };
    let mut writer = crate::writer::LedgerWriter::create_with(output.clone(), out_header, config)?;
    let mut encoder = EventEncoder::new();
    let mut pool = BufferPool::new(64 * 1024);
    let mut next = start_seq;
    for events in by_seq.values() {
        for ev in events {
            encoder.push_event(next, ev, &mut pool)?;
            next = next.wrapping_add(1);
            if (encoder.event_count() as usize >= 512 || encoder.bytes_len() >= 64 * 1024)
                && let Some(block) = encoder.take_nonempty_block(&mut pool)?
            {
                writer.write_block(&block)?;
            }
        }
    }
    if let Some(block) = encoder.take_nonempty_block(&mut pool)? {
        writer.write_block(&block)?;
    }
    let closed = writer.finish()?;

    // Gap report over the input union.
    let mut gaps = Vec::new();
    let mut expected = start_seq;
    for seq in by_seq.keys() {
        if *seq > expected {
            gaps.push(SequenceGap {
                expected,
                found: *seq,
            });
        }
        expected = seq.wrapping_add(1);
    }

    Ok(MergeReport {
        input_segments: inputs.to_vec(),
        output,
        stream_id: header.stream_id,
        total_events: closed.total_events,
        gaps,
        duplicates,
        merged_digest: closed.segment_digest,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SegmentHeader;
    use crate::event::tests::make_all_events;
    use crate::segment::read_segment_footer;

    fn make_header(first_seq: u64) -> SegmentHeader {
        SegmentHeader::new(1, 1, 42, 7, first_seq, Digest::hash_blake3(b"compat"))
    }

    fn make_resource(i: u64) -> Event {
        Event::ResourceSample(crate::event::ResourceSampleEvent {
            user_cpu_ns: i,
            sys_cpu_ns: i,
            rss_bytes: i,
            timestamp_ns: i,
        })
    }

    fn write_segment(path: &Path, events: &[(u64, Event)]) -> crate::ClosedSegment {
        write_segment_with(path, events, LedgerConfig::default())
    }

    fn write_segment_with(
        path: &Path,
        events: &[(u64, Event)],
        config: LedgerConfig,
    ) -> crate::ClosedSegment {
        let header = make_header(events.first().map(|(s, _)| *s).unwrap_or(0));
        let first_sequence = header.first_sequence;
        let mut writer =
            crate::writer::LedgerWriter::create_with(path.to_path_buf(), header, config.clone())
                .unwrap();
        let mut enc = EventEncoder::with_first_sequence(first_sequence, config.gap_policy);
        let mut pool = BufferPool::new(1024);
        for (seq, ev) in events {
            enc.push_event(*seq, ev, &mut pool).unwrap();
            if enc.event_count() >= 64 {
                let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
                writer.write_block(&block).unwrap();
            }
        }
        if let Some(block) = enc.take_nonempty_block(&mut pool).unwrap() {
            writer.write_block(&block).unwrap();
        }
        writer.finish().unwrap()
    }

    #[test]
    fn test_recover_torn_tail_truncates() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("torn.segment");
        // Two blocks: 64 events then 4, so a torn tail loses only block 2.
        let mut evs = vec![];
        for i in 0..64u64 {
            evs.push((i, make_resource(i)));
        }
        for i in 64..68u64 {
            evs.push((i, make_resource(i)));
        }
        write_segment(&path, &evs);
        let block2_offset = {
            let r = recover_segment(&path).unwrap();
            r.blocks[1].offset
        };

        // Simulate a crash mid-block-write: truncate into block 2's payload.
        let truncate_at = block2_offset + BlockHeader::SIZE as u64 + 5;
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.set_len(truncate_at).unwrap();
        drop(f);

        let recovered = recover_segment(&path).unwrap();
        assert!(recovered.report.torn_tail_truncated);
        assert_eq!(recovered.report.first_bad_block, Some(block2_offset));
        assert_eq!(recovered.report.blocks_valid, 1);
        assert_eq!(recovered.report.recovered_events, 64);
        // The torn bytes are dropped; the file ends at the last valid block.
        assert_eq!(std::fs::metadata(&path).unwrap().len(), block2_offset);
        let events = recovered.read_events(&path).unwrap();
        assert_eq!(events.len(), 64);
    }

    #[test]
    fn test_recover_torn_footer_truncates_blocks_preserved() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("tornfooter.segment");
        write_segment(&path, &[(0, make_resource(0)), (1, make_resource(1))]);
        let blocks_end = {
            let r = recover_segment(&path).unwrap();
            r.valid_bytes
        };

        // Truncate into the footer: the single block must survive; the
        // partial footer bytes are dropped and the index rebuilt.
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        f.set_len(blocks_end + 10).unwrap();
        drop(f);
        let recovered = recover_segment(&path).unwrap();
        assert!(recovered.report.torn_tail_truncated);
        assert!(recovered.report.index_rebuilt);
        assert_eq!(recovered.report.blocks_valid, 1);
        assert_eq!(recovered.report.recovered_events, 2);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), blocks_end);
        let events = recovered.read_events(&path).unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_recover_crc_mismatch_errors() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("corrupt.segment");
        write_segment(&path, &[(0, make_resource(0))]);
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
    fn test_quarantine_moves_corrupt_segment() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("quarantine_me.segment");
        write_segment(&path, &[(0, make_resource(0)), (1, make_resource(1))]);
        // Corrupt the block's payload (flip two bytes mid-payload).
        let mut f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let payload_start = SegmentHeader::SIZE as u64 + BlockHeader::SIZE as u64;
        f.seek(SeekFrom::Start(payload_start + 4)).unwrap();
        f.write_all(b"XX").unwrap();
        drop(f);

        let report = quarantine_corrupt_segment(&path).unwrap().expect("report");
        assert_eq!(report.first_bad_block, SegmentHeader::SIZE as u64);
        assert!(
            report
                .quarantine_path
                .ends_with("quarantine_me.segment.corrupt")
        );
        assert!(!path.exists());
        assert!(report.quarantine_path.exists());
        assert!(report.report_path.exists());
        let json = std::fs::read_to_string(&report.report_path).unwrap();
        assert!(json.contains("crc32c"));
    }

    #[test]
    fn test_quarantine_clean_segment_returns_none() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("clean.segment");
        write_segment(&path, &[(0, make_resource(0))]);
        let report = quarantine_corrupt_segment(&path).unwrap();
        assert!(report.is_none());
    }

    #[test]
    fn test_recovery_reports_gaps() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("gap.segment");
        // Craft a segment whose blocks skip sequences 1..=4 (block 1 covers
        // seq 0, block 2 starts at seq 5). The writer is configured with
        // GapPolicy::Record so the cross-block gap is tolerated at write time
        // and must be reported by recovery.
        let config = LedgerConfig {
            gap_policy: GapPolicy::Record,
            ..LedgerConfig::default()
        };
        let header = make_header(0);
        let mut writer =
            crate::writer::LedgerWriter::create_with(path.clone(), header, config).unwrap();
        let mut pool = BufferPool::new(1024);

        let mut enc = EventEncoder::new();
        enc.push_event(0, &make_resource(0), &mut pool).unwrap();
        let b0 = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        writer.write_block(&b0).unwrap();

        let mut enc = EventEncoder::new();
        enc.push_event(5, &make_resource(5), &mut pool).unwrap();
        let b1 = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        writer.write_block(&b1).unwrap();
        writer.finish().unwrap();

        let recovered = recover_segment(&path).unwrap();
        assert_eq!(recovered.report.gaps.len(), 1);
        assert_eq!(recovered.report.gaps[0].expected, 1);
        assert_eq!(recovered.report.gaps[0].found, 5);
    }

    #[test]
    fn test_repeated_recovery_identical_index() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("det.segment");
        write_segment(&path, &[(0, make_resource(0)), (1, make_resource(1))]);
        let idx_path = sidecar_index_path(&path);
        // Wipe the index so recovery rebuilds it.
        std::fs::remove_file(&idx_path).unwrap();
        recover_segment(&path).unwrap();
        let first = std::fs::read(&idx_path).unwrap();
        std::fs::remove_file(&idx_path).unwrap();
        recover_segment(&path).unwrap();
        let second = std::fs::read(&idx_path).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn test_reader_iterates_events() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("reader.segment");
        let mut events = vec![];
        for i in 0..100 {
            events.push((i, make_resource(i)));
        }
        write_segment(&path, &events);

        let mut reader = SegmentReader::open(&path).unwrap();
        let mut out = SequencedEvent {
            sequence: 0,
            event: Event::ResourceSample(crate::event::ResourceSampleEvent::default()),
        };
        let mut seen = 0;
        while reader.next_event(&mut out).unwrap() {
            assert_eq!(out.sequence, seen);
            seen += 1;
        }
        assert_eq!(seen, 100);
    }

    #[test]
    fn test_reader_resolves_policy_score_batches() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("scores.segment");
        let events = make_all_events();
        let mut evs = vec![];
        for (i, e) in events.iter().enumerate() {
            evs.push((i as u64, e.clone()));
        }
        write_segment(&path, &evs);
        let mut reader = SegmentReader::open(&path).unwrap();
        let mut out = SequencedEvent {
            sequence: 0,
            event: Event::ResourceSample(crate::event::ResourceSampleEvent::default()),
        };
        let mut score_seen = false;
        while reader.next_event(&mut out).unwrap() {
            if let Event::PolicyScore(p) = &out.event {
                score_seen = true;
                assert_eq!(p.candidate_ids.len(), 2);
                assert_eq!(
                    p.selected_candidate, p.candidate_ids[0],
                    "selected candidate must resolve to a full ID"
                );
            }
        }
        assert!(score_seen);
    }

    #[test]
    fn test_merge_streams() {
        let temp_dir = tempfile::tempdir().unwrap();
        let a = temp_dir.path().join("a.segment");
        let b = temp_dir.path().join("b.segment");
        let out = temp_dir.path().join("merged.segment");
        write_segment(&a, &[(0, make_resource(0)), (1, make_resource(1))]);
        write_segment(&b, &[(2, make_resource(2)), (3, make_resource(3))]);

        let report = merge_streams(&[a, b], out.clone(), 99).unwrap();
        assert_eq!(report.total_events, 4);
        assert!(report.gaps.is_empty());
        assert!(report.duplicates.is_empty());

        let recovered = recover_segment(&out).unwrap();
        let events = recovered.read_events(&out).unwrap();
        let seqs: Vec<u64> = events.iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, vec![0, 1, 2, 3]);
        let footer = read_segment_footer(&out).unwrap().expect("footer");
        assert_eq!(footer.segment_digest, report.merged_digest);
        assert_eq!(recovered.header.producer_id, 99);
    }

    #[test]
    fn test_merge_duplicates_and_gaps_reported() {
        let temp_dir = tempfile::tempdir().unwrap();
        let a = temp_dir.path().join("a2.segment");
        let b = temp_dir.path().join("b2.segment");
        let out = temp_dir.path().join("merged2.segment");
        // b starts at seq 1, duplicating a's seq 1, and skips seq 3. Record
        // policy: the encoder tolerates the internal gap (the merger is what
        // reports it).
        write_segment(&a, &[(0, make_resource(0)), (1, make_resource(1))]);
        write_segment_with(
            &b,
            &[(1, make_resource(1)), (4, make_resource(4))],
            LedgerConfig {
                gap_policy: GapPolicy::Record,
                ..LedgerConfig::default()
            },
        );

        let report = merge_streams(&[a, b], out.clone(), 1).unwrap();
        assert_eq!(report.duplicates, vec![1]);
        assert_eq!(report.gaps.len(), 1);
        assert_eq!(report.gaps[0].expected, 2);
        assert_eq!(report.gaps[0].found, 4);

        // Output is contiguous; duplicate identity kept once (first input wins).
        let events = recover_segment(&out).unwrap().read_events(&out).unwrap();
        let seqs: Vec<u64> = events.iter().map(|e| e.sequence).collect();
        assert_eq!(seqs, vec![0, 1, 2]);
        assert_eq!(events.len(), 3);
    }

    #[test]
    fn test_merge_rejects_cross_stream() {
        let temp_dir = tempfile::tempdir().unwrap();
        let a = temp_dir.path().join("a3.segment");
        let b = temp_dir.path().join("b3.segment");
        write_segment(&a, &[(0, make_resource(0))]);
        // Different stream id.
        let mut header = make_header(0);
        header.stream_id = 43;
        let mut writer = crate::writer::LedgerWriter::create(b.clone(), header).unwrap();
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        enc.push_event(0, &make_resource(0), &mut pool).unwrap();
        let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        writer.write_block(&block).unwrap();
        writer.finish().unwrap();

        let out = temp_dir.path().join("merged3.segment");
        let err = merge_streams(&[a, b], out, 1).unwrap_err();
        assert!(matches!(err, LedgerError::Encoding(_)));
    }
}
