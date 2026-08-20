//! Segment layout (§8.2) and the footer/index formats (P3.2, P3.4, F-29).
//!
//! ```text
//! SegmentHeader (63 bytes, fixed)
//!   magic "RFXSEG01", schema_id u16, schema_version u16, stream_id u32,
//!   producer_id u32, first_sequence u64, digest algo u8, compatibility digest
//! repeated Block (34-byte header + payload)
//!   stored_length u32, uncompressed_length u32, event_count u32,
//!   first_sequence u64, last_sequence u64, flags u16, crc32c u32
//! optional SegmentFooter (written by finish(); absent on crash)
//!   magic "RFXFTR02", version u16, totals, index, segment digest,
//!   footer crc32c, end magic "RFXEND01"
//! sidecar index file  <segment>.idx (written by finish() and by recovery)
//!   magic "RFXIDX01", version u16, segment_len u64, block_count u64,
//!   entries {offset u64, first_seq u64, last_seq u64, event_count u32,
//!   classes u64}, crc32c
//! ```
//!
//! All multi-byte integers are little-endian; the format is deterministic
//! (identical bytes on every target architecture).
//!
//! The cumulative `segment_digest` is a chained BLAKE3: `h_0 = blake3(header)`,
//! `h_i = blake3(h_{i-1} || block_header_i || payload_i)`. Recovery recomputes
//! it from the scanned blocks and cross-checks the footer.

use crate::codec::payload_classes;
use crate::event::{Event, ResourceSampleEvent, SequencedEvent};
use crate::{LedgerError, MAX_BLOCK_BYTES};
use bytes::BytesMut;
use crc32c::crc32c;
use reflex_types::{Digest, DigestAlgorithm};
use serde::{Deserialize, Serialize};

pub const SEGMENT_MAGIC: &[u8; 8] = b"RFXSEG01";
pub const FOOTER_MAGIC: &[u8; 8] = b"RFXFTR02";
pub const FOOTER_END_MAGIC: &[u8; 8] = b"RFXEND01";
pub const INDEX_MAGIC: &[u8; 8] = b"RFXIDX01";

// ---------------------------------------------------------------------------
// SegmentHeader — §8.2 (unchanged wire format)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentHeader {
    pub magic: [u8; 8],
    pub schema_id: u16,
    pub schema_version: u16,
    pub stream_id: u32,
    pub producer_id: u32,
    pub first_sequence: u64,
    pub compatibility_digest: Digest,
}

impl SegmentHeader {
    pub const SIZE: usize = 8 + 2 + 2 + 4 + 4 + 8 + 33;

    pub fn new(
        schema_id: u16,
        schema_version: u16,
        stream_id: u32,
        producer_id: u32,
        first_sequence: u64,
        compatibility_digest: Digest,
    ) -> Self {
        Self {
            magic: *SEGMENT_MAGIC,
            schema_id,
            schema_version,
            stream_id,
            producer_id,
            first_sequence,
            compatibility_digest,
        }
    }

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        let mut off = 0;
        buf[off..off + 8].copy_from_slice(&self.magic);
        off += 8;
        buf[off..off + 2].copy_from_slice(&self.schema_id.to_le_bytes());
        off += 2;
        buf[off..off + 2].copy_from_slice(&self.schema_version.to_le_bytes());
        off += 2;
        buf[off..off + 4].copy_from_slice(&self.stream_id.to_le_bytes());
        off += 4;
        buf[off..off + 4].copy_from_slice(&self.producer_id.to_le_bytes());
        off += 4;
        buf[off..off + 8].copy_from_slice(&self.first_sequence.to_le_bytes());
        off += 8;
        match self.compatibility_digest.algorithm {
            DigestAlgorithm::Blake3 => buf[off] = 0,
            DigestAlgorithm::Sha256 => buf[off] = 1,
        }
        off += 1;
        buf[off..off + 32].copy_from_slice(&self.compatibility_digest.bytes);
        buf
    }

    pub fn decode(data: &[u8]) -> Result<(Self, usize), LedgerError> {
        if data.len() < Self::SIZE {
            return Err(LedgerError::FileTooShort);
        }
        if &data[0..8] != SEGMENT_MAGIC {
            let mut m = [0u8; 8];
            m.copy_from_slice(&data[0..8]);
            return Err(LedgerError::InvalidMagic(m));
        }
        let mut off = 8;
        let schema_id = u16::from_le_bytes(data[off..off + 2].try_into().unwrap());
        off += 2;
        let schema_version = u16::from_le_bytes(data[off..off + 2].try_into().unwrap());
        off += 2;
        let stream_id = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
        off += 4;
        let producer_id = u32::from_le_bytes(data[off..off + 4].try_into().unwrap());
        off += 4;
        let first_sequence = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let algo = match data[off] {
            0 => DigestAlgorithm::Blake3,
            1 => DigestAlgorithm::Sha256,
            _ => {
                return Err(LedgerError::Encoding(
                    "invalid digest algo in header".to_string(),
                ));
            }
        };
        off += 1;
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&data[off..off + 32]);
        off += 32;

        Ok((
            Self {
                magic: *SEGMENT_MAGIC,
                schema_id,
                schema_version,
                stream_id,
                producer_id,
                first_sequence,
                compatibility_digest: Digest {
                    algorithm: algo,
                    bytes,
                },
            },
            off,
        ))
    }
}

// ---------------------------------------------------------------------------
// BlockHeader — §8.2 (unchanged wire format)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockHeader {
    pub stored_length: u32,
    pub uncompressed_length: u32,
    pub event_count: u32,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub flags: u16,
    pub crc32c: u32,
}

impl BlockHeader {
    pub const SIZE: usize = 4 + 4 + 4 + 8 + 8 + 2 + 4;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..4].copy_from_slice(&self.stored_length.to_le_bytes());
        buf[4..8].copy_from_slice(&self.uncompressed_length.to_le_bytes());
        buf[8..12].copy_from_slice(&self.event_count.to_le_bytes());
        buf[12..20].copy_from_slice(&self.first_sequence.to_le_bytes());
        buf[20..28].copy_from_slice(&self.last_sequence.to_le_bytes());
        buf[28..30].copy_from_slice(&self.flags.to_le_bytes());
        buf[30..34].copy_from_slice(&self.crc32c.to_le_bytes());
        buf
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, LedgerError> {
        if bytes.len() < Self::SIZE {
            return Err(LedgerError::Encoding("block header too short".to_string()));
        }
        Ok(Self {
            stored_length: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            uncompressed_length: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            event_count: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            first_sequence: u64::from_le_bytes(bytes[12..20].try_into().unwrap()),
            last_sequence: u64::from_le_bytes(bytes[20..28].try_into().unwrap()),
            flags: u16::from_le_bytes(bytes[28..30].try_into().unwrap()),
            crc32c: u32::from_le_bytes(bytes[30..34].try_into().unwrap()),
        })
    }

    /// First/last sequence sanity check used by scan and writer paths.
    pub fn is_sane(&self) -> bool {
        self.stored_length as u64 <= u64::from(MAX_BLOCK_BYTES)
            && self.first_sequence <= self.last_sequence
            && self.stored_length == self.uncompressed_length
    }
}

// ---------------------------------------------------------------------------
// Block index entry (footer and in-memory recovery)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockIndexEntry {
    pub offset: u64,
    pub header: BlockHeader,
}

impl BlockIndexEntry {
    pub const SIZE: usize = 8 + BlockHeader::SIZE;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..8].copy_from_slice(&self.offset.to_le_bytes());
        buf[8..].copy_from_slice(&self.header.encode());
        buf
    }

    pub fn decode(data: &[u8]) -> Result<Self, LedgerError> {
        if data.len() < Self::SIZE {
            return Err(LedgerError::Encoding("index entry too short".to_string()));
        }
        Ok(Self {
            offset: u64::from_le_bytes(data[0..8].try_into().unwrap()),
            header: BlockHeader::decode(&data[8..])?,
        })
    }
}

// ---------------------------------------------------------------------------
// SegmentFooter — written by finish() (F-29)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SegmentFooter {
    pub version: u16,
    pub total_blocks: u64,
    pub total_events: u64,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub stored_bytes: u64,
    pub uncompressed_bytes: u64,
    pub block_index: Vec<BlockIndexEntry>,
    pub segment_digest: Digest,
}

impl SegmentFooter {
    /// Fixed portion (everything except the per-block index entries).
    pub const FIXED_SIZE: usize = 8 + 2 + 8 + 8 + 8 + 8 + 8 + 8 + 4 + 33 + 4 + 8;

    pub const fn size_for(blocks: usize) -> usize {
        Self::FIXED_SIZE + blocks * BlockIndexEntry::SIZE
    }

    /// Encodes body + footer crc32c + end magic (the complete on-disk tail).
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(Self::size_for(self.block_index.len()));
        body.extend_from_slice(FOOTER_MAGIC);
        body.extend_from_slice(&self.version.to_le_bytes());
        body.extend_from_slice(&self.total_blocks.to_le_bytes());
        body.extend_from_slice(&self.total_events.to_le_bytes());
        body.extend_from_slice(&self.first_sequence.to_le_bytes());
        body.extend_from_slice(&self.last_sequence.to_le_bytes());
        body.extend_from_slice(&self.stored_bytes.to_le_bytes());
        body.extend_from_slice(&self.uncompressed_bytes.to_le_bytes());
        body.extend_from_slice(&(self.block_index.len() as u32).to_le_bytes());
        for entry in &self.block_index {
            body.extend_from_slice(&entry.encode());
        }
        body.push(match self.segment_digest.algorithm {
            DigestAlgorithm::Blake3 => 0,
            DigestAlgorithm::Sha256 => 1,
        });
        body.extend_from_slice(&self.segment_digest.bytes);
        let crc = crc32c(&body);
        body.extend_from_slice(&crc.to_le_bytes());
        body.extend_from_slice(FOOTER_END_MAGIC);
        body
    }

    /// Decodes a complete footer (including crc and end magic) and validates
    /// the footer crc32c.
    pub fn decode(data: &[u8]) -> Result<Self, LedgerError> {
        if data.len() < Self::FIXED_SIZE {
            return Err(LedgerError::Encoding("footer too short".to_string()));
        }
        let body_len = data.len() - 4 - 8;
        if &data[data.len() - 8..] != FOOTER_END_MAGIC {
            return Err(LedgerError::Encoding("footer end magic mismatch".into()));
        }
        if &data[0..8] != FOOTER_MAGIC {
            return Err(LedgerError::Encoding("footer magic mismatch".into()));
        }
        let expected_crc = u32::from_le_bytes(data[body_len..body_len + 4].try_into().unwrap());
        if crc32c(&data[..body_len]) != expected_crc {
            return Err(LedgerError::Encoding("footer crc mismatch".into()));
        }
        let mut off = 8;
        let version = u16::from_le_bytes(data[off..off + 2].try_into().unwrap());
        off += 2;
        let total_blocks = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let total_events = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let first_sequence = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let last_sequence = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let stored_bytes = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let uncompressed_bytes = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let index_len = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        let mut block_index = Vec::with_capacity(index_len);
        for _ in 0..index_len {
            if off + BlockIndexEntry::SIZE > body_len {
                return Err(LedgerError::Encoding("footer index truncated".into()));
            }
            block_index.push(BlockIndexEntry::decode(
                &data[off..off + BlockIndexEntry::SIZE],
            )?);
            off += BlockIndexEntry::SIZE;
        }
        let algo = match data[off] {
            0 => DigestAlgorithm::Blake3,
            1 => DigestAlgorithm::Sha256,
            _ => return Err(LedgerError::Encoding("bad digest algo".into())),
        };
        off += 1;
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&data[off..off + 32]);
        off += 32;
        if off != body_len {
            return Err(LedgerError::Encoding("footer trailing bytes".into()));
        }
        Ok(Self {
            version,
            total_blocks,
            total_events,
            first_sequence,
            last_sequence,
            stored_bytes,
            uncompressed_bytes,
            block_index,
            segment_digest: Digest {
                algorithm: algo,
                bytes,
            },
        })
    }
}

// ---------------------------------------------------------------------------
// Sidecar index — P3.4 ("mapping sequence ranges and event classes to block
// offsets"; <1% of segment size for representative workloads)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexEntry {
    pub offset: u64,
    pub first_sequence: u64,
    pub last_sequence: u64,
    pub event_count: u32,
    pub classes: u64,
}

impl IndexEntry {
    pub const SIZE: usize = 8 + 8 + 8 + 4 + 8;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut buf = [0u8; Self::SIZE];
        buf[0..8].copy_from_slice(&self.offset.to_le_bytes());
        buf[8..16].copy_from_slice(&self.first_sequence.to_le_bytes());
        buf[16..24].copy_from_slice(&self.last_sequence.to_le_bytes());
        buf[24..28].copy_from_slice(&self.event_count.to_le_bytes());
        buf[28..36].copy_from_slice(&self.classes.to_le_bytes());
        buf
    }

    pub fn decode(data: &[u8]) -> Result<Self, LedgerError> {
        if data.len() < Self::SIZE {
            return Err(LedgerError::Encoding("index entry too short".into()));
        }
        Ok(Self {
            offset: u64::from_le_bytes(data[0..8].try_into().unwrap()),
            first_sequence: u64::from_le_bytes(data[8..16].try_into().unwrap()),
            last_sequence: u64::from_le_bytes(data[16..24].try_into().unwrap()),
            event_count: u32::from_le_bytes(data[24..28].try_into().unwrap()),
            classes: u64::from_le_bytes(data[28..36].try_into().unwrap()),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexFile {
    pub version: u16,
    pub segment_len: u64,
    pub entries: Vec<IndexEntry>,
}

impl IndexFile {
    pub const FIXED_SIZE: usize = 8 + 2 + 8 + 8 + 4;

    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::with_capacity(Self::FIXED_SIZE + self.entries.len() * IndexEntry::SIZE);
        body.extend_from_slice(INDEX_MAGIC);
        body.extend_from_slice(&self.version.to_le_bytes());
        body.extend_from_slice(&self.segment_len.to_le_bytes());
        body.extend_from_slice(&(self.entries.len() as u64).to_le_bytes());
        for e in &self.entries {
            body.extend_from_slice(&e.encode());
        }
        let crc = crc32c(&body);
        body.extend_from_slice(&crc.to_le_bytes());
        body
    }

    pub fn decode(data: &[u8]) -> Result<Self, LedgerError> {
        if data.len() < Self::FIXED_SIZE {
            return Err(LedgerError::Encoding("index file too short".into()));
        }
        if &data[0..8] != INDEX_MAGIC {
            return Err(LedgerError::Encoding("index magic mismatch".into()));
        }
        let body_len = data.len() - 4;
        let expected_crc = u32::from_le_bytes(data[body_len..body_len + 4].try_into().unwrap());
        if crc32c(&data[..body_len]) != expected_crc {
            return Err(LedgerError::Encoding("index crc mismatch".into()));
        }
        let mut off = 8;
        let version = u16::from_le_bytes(data[off..off + 2].try_into().unwrap());
        off += 2;
        let segment_len = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let count = u64::from_le_bytes(data[off..off + 8].try_into().unwrap()) as usize;
        off += 8;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            entries.push(IndexEntry::decode(&data[off..off + IndexEntry::SIZE])?);
            off += IndexEntry::SIZE;
        }
        if off != body_len {
            return Err(LedgerError::Encoding("index trailing bytes".into()));
        }
        Ok(Self {
            version,
            segment_len,
            entries,
        })
    }
}

/// Path of the sidecar index for a segment: `<segment>.idx`.
pub fn sidecar_index_path(path: &std::path::Path) -> std::path::PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".idx");
    std::path::PathBuf::from(s)
}

/// Reads and validates the footer at the end of a cleanly finished segment.
/// Returns `Ok(None)` when the file has no valid footer (crash or torn tail).
///
/// The footer start is `file_len - FIXED_SIZE - 42 * block_count`, so all
/// candidate starts share one residue class modulo 42. The tail of the file
/// (up to 1 MiB — footers beyond that are verified by recovery, which scans
/// from the block boundary) is read once and each candidate start is
/// full-parsed (magic, index length consistency, crc32c, end magic).
pub fn read_segment_footer(path: &std::path::Path) -> Result<Option<SegmentFooter>, LedgerError> {
    use std::io::{Read, Seek};
    let mut file = std::fs::File::open(path)?;
    let file_len = file.seek(std::io::SeekFrom::End(0))? as usize;
    if file_len < SegmentFooter::FIXED_SIZE {
        return Ok(None);
    }
    let tail_len = std::cmp::min(file_len, 1024 * 1024);
    file.seek(std::io::SeekFrom::End(-(tail_len as i64)))?;
    let mut tail = vec![0u8; tail_len];
    file.read_exact(&mut tail)?;

    let residue = (file_len + (42usize - SegmentFooter::FIXED_SIZE % 42usize)) % 42usize;
    let max_blocks = (file_len - SegmentFooter::FIXED_SIZE) / 42usize;
    for bc in (0..=max_blocks).rev() {
        let start_off = bc * 42 + residue;
        if start_off + SegmentFooter::FIXED_SIZE > tail_len {
            continue;
        }
        if &tail[start_off..start_off + 8] == FOOTER_MAGIC
            && let Ok(f) = SegmentFooter::decode(&tail[start_off..])
        {
            return Ok(Some(f));
        }
    }
    Ok(None)
}

/// Reads and validates the sidecar index for a segment. Returns `Ok(None)`
/// when the index file is absent.
pub fn read_segment_index(path: &std::path::Path) -> Result<Option<IndexFile>, LedgerError> {
    use std::io::Read;
    let idx_path = sidecar_index_path(path);
    let mut data = Vec::new();
    let mut f = match std::fs::File::open(&idx_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    f.read_to_end(&mut data)?;
    Ok(Some(IndexFile::decode(&data)?))
}

/// Rebuilds the sidecar index for an existing segment by scanning it
/// (deterministic output; used by recovery and by operators after a manual
/// index loss). Returns the rebuilt index.
pub fn build_segment_index(path: &std::path::Path) -> Result<IndexFile, LedgerError> {
    use std::io::Write;
    let (_, entries) = scan_blocks_for_index(path)?;
    let segment_len = std::fs::metadata(path)?.len();
    let idx = IndexFile {
        version: 1,
        segment_len,
        entries,
    };
    let data = idx.encode();
    let idx_path = sidecar_index_path(path);
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&idx_path)?;
    f.write_all(&data)?;
    f.flush()?;
    f.sync_all()?;
    Ok(idx)
}

/// Scans a segment file (header + blocks, no footer) and returns per-block
/// index entries with class bitmaps. Used by the index builder.
pub(crate) fn scan_blocks_for_index(
    path: &std::path::Path,
) -> Result<(SegmentHeader, Vec<IndexEntry>), LedgerError> {
    use std::io::{Read, Seek};
    let mut file = std::fs::File::open(path)?;
    let mut header_buf = [0u8; SegmentHeader::SIZE];
    file.read_exact(&mut header_buf)?;
    let (header, _header_size) = SegmentHeader::decode(&header_buf)?;
    let mut payload = BytesMut::with_capacity(256 * 1024);
    let mut entries = Vec::new();
    let mut scratch = SequencedEvent {
        sequence: 0,
        event: Event::ResourceSample(ResourceSampleEvent::default()),
    };
    loop {
        let offset = file.stream_position()?;
        let mut bh_buf = [0u8; BlockHeader::SIZE];
        match file.read_exact(&mut bh_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        if &bh_buf[0..8] == FOOTER_MAGIC {
            break;
        }
        let header = BlockHeader::decode(&bh_buf)?;
        if !header.is_sane() {
            return Err(LedgerError::OversizedBlock {
                offset,
                size: header.stored_length,
            });
        }
        payload.clear();
        payload.resize(header.stored_length as usize, 0);
        file.read_exact(&mut payload)?;
        if crc32c(&payload) != header.crc32c {
            return Err(LedgerError::MidFileCorruption { offset });
        }
        let classes = payload_classes(&payload, &mut scratch)?;
        entries.push(IndexEntry {
            offset,
            first_sequence: header.first_sequence,
            last_sequence: header.last_sequence,
            event_count: header.event_count,
            classes,
        });
    }
    Ok((header, entries))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_header() -> SegmentHeader {
        SegmentHeader::new(1, 1, 42, 7, 0, Digest::hash_blake3(b"compat"))
    }

    #[test]
    fn test_segment_header_roundtrip() {
        let header = make_header();
        let encoded = header.encode();
        assert_eq!(&encoded[0..8], SEGMENT_MAGIC);
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
    fn test_index_entry_roundtrip() {
        let e = IndexEntry {
            offset: 123,
            first_sequence: 4,
            last_sequence: 9,
            event_count: 6,
            classes: 0b101,
        };
        assert_eq!(IndexEntry::decode(&e.encode()).unwrap(), e);
    }

    #[test]
    fn test_index_file_roundtrip() {
        let idx = IndexFile {
            version: 1,
            segment_len: 999,
            entries: vec![
                IndexEntry {
                    offset: 63,
                    first_sequence: 0,
                    last_sequence: 100,
                    event_count: 101,
                    classes: 0b1,
                },
                IndexEntry {
                    offset: 65536,
                    first_sequence: 101,
                    last_sequence: 200,
                    event_count: 100,
                    classes: 0b10,
                },
            ],
        };
        let data = idx.encode();
        let decoded = IndexFile::decode(&data).unwrap();
        assert_eq!(decoded, idx);
    }

    #[test]
    fn test_footer_roundtrip_and_crc() {
        let footer = SegmentFooter {
            version: 1,
            total_blocks: 2,
            total_events: 5,
            first_sequence: 0,
            last_sequence: 4,
            stored_bytes: 123,
            uncompressed_bytes: 123,
            block_index: vec![
                BlockIndexEntry {
                    offset: 63,
                    header: BlockHeader {
                        stored_length: 10,
                        uncompressed_length: 10,
                        event_count: 2,
                        first_sequence: 0,
                        last_sequence: 1,
                        flags: 0,
                        crc32c: 1,
                    },
                },
                BlockIndexEntry {
                    offset: 107,
                    header: BlockHeader {
                        stored_length: 20,
                        uncompressed_length: 20,
                        event_count: 3,
                        first_sequence: 2,
                        last_sequence: 4,
                        flags: 0,
                        crc32c: 2,
                    },
                },
            ],
            segment_digest: Digest::hash_blake3(b"seg"),
        };
        let data = footer.encode();
        assert_eq!(data.len(), SegmentFooter::size_for(2));
        let decoded = SegmentFooter::decode(&data).unwrap();
        assert_eq!(decoded, footer);
        // A flipped byte anywhere must be detected by the footer crc.
        for cut in [0usize, 8, 40, data.len() - 20] {
            let mut corrupt = data.clone();
            corrupt[cut] ^= 0xFF;
            assert!(SegmentFooter::decode(&corrupt).is_err(), "cut {cut}");
        }
    }

    #[test]
    fn test_footer_rejects_truncation() {
        let footer = SegmentFooter {
            version: 1,
            total_blocks: 0,
            total_events: 0,
            first_sequence: 0,
            last_sequence: u64::MAX,
            stored_bytes: 0,
            uncompressed_bytes: 0,
            block_index: Vec::new(),
            segment_digest: Digest::ZERO,
        };
        let data = footer.encode();
        for cut in 0..data.len() {
            assert!(SegmentFooter::decode(&data[..cut]).is_err());
        }
    }
}
