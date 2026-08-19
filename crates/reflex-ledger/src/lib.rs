use crc32c::crc32c;
use reflex_types::{
    ArtifactId, CandidateId, CellId, Digest, DigestAlgorithm, EpisodeId, MetricId,
    ModelCheckpointId, ResearchNodeId, StateId, TaskId, UnitId, VerifierId,
    WorkerId,
};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write as IoWrite};
use std::path::{Path, PathBuf};
use thiserror::Error;
use tokio::sync::{mpsc, oneshot};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const SEGMENT_MAGIC: &[u8; 8] = b"RFXSEG01";
pub const MAX_BLOCK_BYTES: u32 = 16 * 1024 * 1024;
const BLOCK_BATCH_LIMIT: usize = 512;

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
// SegmentHeader — §8.2
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
                compatibility_digest: Digest { algorithm: algo, bytes },
            },
            off,
        ))
    }
}

// ---------------------------------------------------------------------------
// BlockHeader — §8.2
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
}

// ---------------------------------------------------------------------------
// Event families — §8.4
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExperimentLifecycleEvent {
    pub experiment_id: reflex_types::ExperimentId,
    pub action: String,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellLifecycleEvent {
    pub cell_id: CellId,
    pub experiment_id: reflex_types::ExperimentId,
    pub generation_id: reflex_types::GenerationId,
    pub action: String,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WorkerSessionEvent {
    pub worker_id: WorkerId,
    pub action: String,
    pub calibration_data: Vec<u8>,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskStartEvent {
    pub task_id: TaskId,
    pub cell_id: CellId,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EpisodeStartEvent {
    pub episode_id: EpisodeId,
    pub task_id: TaskId,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EpisodeEndEvent {
    pub episode_id: EpisodeId,
    pub status: String,
    pub actions_taken: u32,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateDiscoveryEvent {
    pub state_id: StateId,
    pub episode_id: EpisodeId,
    pub depth: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateBatchEvent {
    pub state_id: StateId,
    pub candidate_ids: Vec<CandidateId>,
    pub classes: Vec<u16>,
    pub tie_breaks: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureRefEvent {
    pub state_id: StateId,
    pub feature_schema: reflex_types::FeatureSchemaId,
    pub payload_handle: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PolicyScoreEvent {
    pub state_id: StateId,
    pub model_id: ModelCheckpointId,
    pub candidate_ids: Vec<CandidateId>,
    pub scores: Vec<f32>,
    pub selected_candidate: CandidateId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CandidateApplicationEvent {
    pub state_id: StateId,
    pub candidate_id: CandidateId,
    pub and_child_states: Vec<StateId>,
    pub outcome: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CacheObservationEvent {
    pub state_id: StateId,
    pub hit: bool,
    pub cache_key: Digest,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArtifactConstructionEvent {
    pub artifact_id: ArtifactId,
    pub episode_id: EpisodeId,
    pub size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VerificationReceiptEvent {
    pub artifact_id: ArtifactId,
    pub verifier: VerifierId,
    pub status: String,
    pub cpu_ns: u64,
    pub wall_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UtilityObservationEvent {
    pub subject: ResearchNodeId,
    pub metric: MetricId,
    pub value: f64,
    pub unit: UnitId,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelShadowScoreEvent {
    pub model_id: ModelCheckpointId,
    pub state_id: StateId,
    pub candidate_id: CandidateId,
    pub score: f32,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceSampleEvent {
    pub user_cpu_ns: u64,
    pub sys_cpu_ns: u64,
    pub rss_bytes: u64,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LineageEdgeEvent {
    pub parent: ResearchNodeId,
    pub child: ResearchNodeId,
    pub edge_type: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct IncidentEvent {
    pub severity: String,
    pub description: String,
    pub episode_id: Option<EpisodeId>,
    pub timestamp_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Event {
    ExperimentLifecycle(ExperimentLifecycleEvent),
    CellLifecycle(CellLifecycleEvent),
    WorkerSession(WorkerSessionEvent),
    TaskStart(TaskStartEvent),
    EpisodeStart(EpisodeStartEvent),
    EpisodeEnd(EpisodeEndEvent),
    StateDiscovery(StateDiscoveryEvent),
    CandidateBatch(CandidateBatchEvent),
    FeatureRef(FeatureRefEvent),
    PolicyScore(PolicyScoreEvent),
    CandidateApplication(CandidateApplicationEvent),
    CacheObservation(CacheObservationEvent),
    ArtifactConstruction(ArtifactConstructionEvent),
    VerificationReceipt(VerificationReceiptEvent),
    UtilityObservation(UtilityObservationEvent),
    ModelShadowScore(ModelShadowScoreEvent),
    ResourceSample(ResourceSampleEvent),
    LineageEdge(LineageEdgeEvent),
    Incident(IncidentEvent),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SequencedEvent {
    pub sequence: u64,
    pub event: Event,
}

// ---------------------------------------------------------------------------
// EncodedBlock
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct EncodedBlock {
    pub header: BlockHeader,
    pub payload: Vec<u8>,
}

// ---------------------------------------------------------------------------
// LedgerPosition — returned by Barrier
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerPosition {
    pub offset: u64,
    pub sequence: u64,
}

// ---------------------------------------------------------------------------
// ClosedSegment — returned by Finish
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClosedSegment {
    pub header: SegmentHeader,
    pub total_blocks: u64,
    pub total_events: u64,
    pub total_bytes: u64,
    pub segment_digest: Digest,
}

// ---------------------------------------------------------------------------
// LedgerCommand — §8.3
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
// BufferPool — reusable buffers for event encoding
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
// EventEncoder — encodes typed events into blocks
// ---------------------------------------------------------------------------

pub struct EventEncoder {
    events: Vec<SequencedEvent>,
}

impl EventEncoder {
    pub fn new() -> Self {
        Self {
            events: Vec::with_capacity(512),
        }
    }

    pub fn push_event(
        &mut self,
        seq: u64,
        event: &Event,
        _pool: &mut BufferPool,
    ) -> Result<(), LedgerError> {
        self.events.push(SequencedEvent {
            sequence: seq,
            event: event.clone(),
        });
        Ok(())
    }

    pub fn take_nonempty_block(
        &mut self,
        _pool: &mut BufferPool,
    ) -> Result<Option<EncodedBlock>, LedgerError> {
        if self.events.is_empty() {
            return Ok(None);
        }
        let events = std::mem::replace(&mut self.events, Vec::with_capacity(512));
        let first_seq = events.first().unwrap().sequence;
        let last_seq = events.last().unwrap().sequence;
        let count = events.len() as u32;

        let payload = serde_json::to_vec(&events)
            .map_err(|e| LedgerError::Encoding(e.to_string()))?;
        let crc = crc32c(&payload);

        Ok(Some(EncodedBlock {
            header: BlockHeader {
                stored_length: payload.len() as u32,
                uncompressed_length: payload.len() as u32,
                event_count: count,
                first_sequence: first_seq,
                last_sequence: last_seq,
                flags: 0,
                crc32c: crc,
            },
            payload,
        }))
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    pub fn event_count(&self) -> u32 {
        self.events.len() as u32
    }

    pub fn clear(&mut self, _pool: &mut BufferPool) {
        self.events.clear();
    }
}

// ---------------------------------------------------------------------------
// LedgerWriter — runs on a dedicated thread, writes blocks sequentially
// ---------------------------------------------------------------------------

pub struct LedgerWriter {
    path: PathBuf,
    header: SegmentHeader,
    next_sequence: u64,
    total_blocks: u64,
    total_events: u64,
    total_bytes: u64,
    file: File,
}

impl LedgerWriter {
    pub fn create(path: PathBuf, header: SegmentHeader) -> Result<Self, LedgerError> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)?;

        let header_bytes = header.encode();
        file.write_all(&header_bytes)?;
        let total_bytes = header_bytes.len() as u64;
        let next_seq = header.first_sequence;

        Ok(Self {
            path,
            header,
            next_sequence: next_seq,
            total_blocks: 0,
            total_events: 0,
            total_bytes,
            file,
        })
    }

    fn write_block(&mut self, block: &EncodedBlock) -> Result<(), LedgerError> {
        self.file.write_all(&block.header.encode())?;
        self.file.write_all(&block.payload)?;
        self.total_bytes += BlockHeader::SIZE as u64 + block.payload.len() as u64;
        self.total_blocks += 1;
        self.total_events += block.header.event_count as u64;
        self.next_sequence = block.header.last_sequence + 1;
        Ok(())
    }

    fn sync(&mut self) -> Result<(), LedgerError> {
        self.file.flush()?;
        self.file.sync_all()?;
        Ok(())
    }

    fn finish(mut self) -> Result<ClosedSegment, LedgerError> {
        self.sync()?;
        let mut file = File::open(&self.path)?;
        let mut hasher = blake3::Hasher::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        let segment_digest = Digest::from_blake3_bytes(*hasher.finalize().as_bytes());

        Ok(ClosedSegment {
            header: self.header,
            total_blocks: self.total_blocks,
            total_events: self.total_events,
            total_bytes: self.total_bytes,
            segment_digest,
        })
    }

    fn current_position(&self) -> LedgerPosition {
        LedgerPosition {
            offset: self.total_bytes,
            sequence: self.next_sequence,
        }
    }
}

pub fn spawn_ledger_writer(
    path: PathBuf,
    header: SegmentHeader,
    capacity: usize,
) -> Result<EventSink, LedgerError> {
    let (tx, mut rx) = mpsc::channel::<LedgerCommand>(capacity);
    let mut writer = LedgerWriter::create(path, header)?;

    std::thread::Builder::new()
        .name("reflex-ledger-writer".into())
        .spawn(move || {
            while let Some(cmd) = rx.blocking_recv() {
                match cmd {
                    LedgerCommand::Block(block) => {
                        let _ = writer.write_block(&block);
                    }
                    LedgerCommand::Barrier { reply } => {
                        let res = writer.sync().map(|()| writer.current_position());
                        let _ = reply.send(res);
                    }
                    LedgerCommand::Rotate => {
                        let _ = writer.sync();
                    }
                    LedgerCommand::Finish { reply } => {
                        let res = writer.finish();
                        let _ = reply.send(res);
                        return;
                    }
                }
            }
            let _ = writer.sync();
        })
        .expect("spawn ledger writer thread");

    Ok(EventSink::new(tx))
}

// ---------------------------------------------------------------------------
// EventSink — §8.3
// ---------------------------------------------------------------------------

pub struct EventSink {
    tx: mpsc::Sender<LedgerCommand>,
    pool: BufferPool,
    encoder: EventEncoder,
}

impl EventSink {
    pub fn new(tx: mpsc::Sender<LedgerCommand>) -> Self {
        Self {
            tx,
            pool: BufferPool::new(64 * 1024),
            encoder: EventEncoder::new(),
        }
    }

    pub async fn write_event(
        &mut self,
        seq: u64,
        event: &Event,
    ) -> Result<(), LedgerError> {
        self.encoder.push_event(seq, event, &mut self.pool)?;
        if self.encoder.event_count() >= BLOCK_BATCH_LIMIT as u32 {
            self.drain_block().await?;
        }
        Ok(())
    }

    pub async fn flush_scientific(&mut self) -> Result<(), LedgerError> {
        if let Some(block) = self.encoder.take_nonempty_block(&mut self.pool)? {
            self.tx
                .send(LedgerCommand::Block(block))
                .await
                .map_err(|_| LedgerError::WriterClosed)?;
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(LedgerCommand::Barrier { reply: reply_tx })
            .await
            .map_err(|_| LedgerError::WriterClosed)?;
        reply_rx.await.map_err(|_| LedgerError::WriterClosed)??;
        Ok(())
    }

    pub async fn flush(&self) -> Result<LedgerPosition, LedgerError> {
        let (reply_tx, reply_rx) = oneshot::channel();
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
        if let Some(block) = self.encoder.take_nonempty_block(&mut self.pool)? {
            let _ = self.tx.send(LedgerCommand::Block(block)).await;
        }
        let (reply_tx, reply_rx) = oneshot::channel();
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
        self.encoder.events.last().map_or(0, |e| e.sequence + 1)
    }
}

// ---------------------------------------------------------------------------
// Segment footer helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BlockIndexEntry {
    pub offset: u64,
    pub header: BlockHeader,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SegmentFooter {
    pub block_index: Vec<BlockIndexEntry>,
    pub segment_digest: Digest,
}

// ---------------------------------------------------------------------------
// recover_segment — §28.4 crash recovery
// ---------------------------------------------------------------------------

pub fn recover_segment(path: &Path) -> Result<RecoveredSegment, LedgerError> {
    let mut file = File::open(path)?;

    let mut header_buf = [0u8; SegmentHeader::SIZE];
    match file.read_exact(&mut header_buf) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(LedgerError::FileTooShort);
        }
        Err(e) => return Err(e.into()),
    }
    let (header, header_size) = SegmentHeader::decode(&header_buf)?;

    file.seek(SeekFrom::Start(header_size as u64))?;

    let mut blocks = Vec::new();
    let mut expected_seq = header.first_sequence;
    let mut valid_bytes = header_size as u64;

    loop {
        let offset = file.stream_position()?;
        let mut bh_buf = [0u8; BlockHeader::SIZE];
        match file.read_exact(&mut bh_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(e) => return Err(e.into()),
        }

        let block_header = BlockHeader::decode(&bh_buf)?;
        if block_header.stored_length > MAX_BLOCK_BYTES {
            return Err(LedgerError::OversizedBlock {
                offset,
                size: block_header.stored_length,
            });
        }

        let mut payload = vec![0u8; block_header.stored_length as usize];
        match file.read_exact(&mut payload) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break;
            }
            Err(e) => return Err(e.into()),
        }

        if crc32c(&payload) != block_header.crc32c {
            return Err(LedgerError::MidFileCorruption { offset });
        }

        if block_header.first_sequence != expected_seq {
            return Err(LedgerError::SequenceMismatch {
                offset,
                expected: expected_seq,
                found: block_header.first_sequence,
            });
        }

        expected_seq = block_header.last_sequence + 1;
        valid_bytes = offset + BlockHeader::SIZE as u64 + payload.len() as u64;
        blocks.push(BlockIndexEntry {
            offset,
            header: block_header,
        });
    }

    Ok(RecoveredSegment {
        header,
        blocks,
        valid_bytes,
    })
}

#[derive(Clone, Debug)]
pub struct RecoveredSegment {
    pub header: SegmentHeader,
    pub blocks: Vec<BlockIndexEntry>,
    pub valid_bytes: u64,
}

impl RecoveredSegment {
    pub fn read_events(&self, path: &Path) -> Result<Vec<SequencedEvent>, LedgerError> {
        let mut file = File::open(path)?;
        let (_, header_size) = SegmentHeader::decode(&{
            let mut buf = [0u8; SegmentHeader::SIZE];
            file.read_exact(&mut buf)?;
            buf
        })?;
        file.seek(SeekFrom::Start(header_size as u64))?;

        let mut all_events = Vec::new();
        for entry in &self.blocks {
            file.seek(SeekFrom::Start(entry.offset + BlockHeader::SIZE as u64))?;
            let mut payload = vec![0u8; entry.header.stored_length as usize];
            file.read_exact(&mut payload)?;

            let events: Vec<SequencedEvent> = serde_json::from_slice(&payload)
                .map_err(|e| LedgerError::Encoding(e.to_string()))?;
            all_events.extend(events);
        }
        Ok(all_events)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_header() -> SegmentHeader {
        SegmentHeader::new(
            1,
            1,
            42,
            7,
            0,
            Digest::hash_blake3(b"compat"),
        )
    }

    fn make_event(n: u64) -> Event {
        Event::ExperimentLifecycle(ExperimentLifecycleEvent {
            experiment_id: reflex_types::ExperimentId::from_digest(
                Digest::hash_blake3(&n.to_le_bytes()),
            ),
            action: format!("test-{n}"),
            timestamp_ns: n * 1000,
        })
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
    fn test_segment_writer_and_recovery() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("test.segment");
        let header = make_header();

        let mut writer = LedgerWriter::create(path.clone(), header.clone()).unwrap();

        for i in 0..10 {
            let event = make_event(i);
            let seq = writer.next_sequence;
            let mut enc = EventEncoder::new();
            let mut pool = BufferPool::new(1024);
            enc.push_event(seq, &event, &mut pool).unwrap();
            if let Some(block) = enc.take_nonempty_block(&mut pool).unwrap() {
                writer.write_block(&block).unwrap();
            }
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
    fn test_recover_torn_tail() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("torn.segment");
        let header = make_header();

        let mut writer = LedgerWriter::create(path.clone(), header).unwrap();

        for i in 0..5 {
            let event = make_event(i);
            let seq = writer.next_sequence;
            let mut enc = EventEncoder::new();
            let mut pool = BufferPool::new(1024);
            enc.push_event(seq, &event, &mut pool).unwrap();
            if let Some(block) = enc.take_nonempty_block(&mut pool).unwrap() {
                writer.write_block(&block).unwrap();
            }
        }
        writer.sync().unwrap();

        let mut f = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        let pos = f.seek(SeekFrom::End(0)).unwrap();
        f.set_len(pos - 5).unwrap();
        drop(f);

        let recovered = recover_segment(&path).unwrap();
        assert!(recovered.blocks.len() <= 5);
        let events = recovered.read_events(&path).unwrap();
        assert!(events.len() <= 5);
    }

    #[test]
    fn test_recover_crc_mismatch() {
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("corrupt.segment");
        let header = make_header();

        let mut writer = LedgerWriter::create(path.clone(), header).unwrap();
        let event = make_event(0);
        let seq = writer.next_sequence;
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        enc.push_event(seq, &event, &mut pool).unwrap();
        if let Some(block) = enc.take_nonempty_block(&mut pool).unwrap() {
            writer.write_block(&block).unwrap();
        }
        writer.sync().unwrap();

        let mut f = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        let header_size = SegmentHeader::SIZE as u64;
        let block_hdr_size = BlockHeader::SIZE as u64;
        f.seek(SeekFrom::Start(header_size + block_hdr_size + 2)).unwrap();
        f.write_all(b"XX").unwrap();
        drop(f);

        let result = recover_segment(&path);
        assert!(matches!(result, Err(LedgerError::MidFileCorruption { .. })));
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

    #[test]
    fn test_event_encoder_batch() {
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);

        for i in 0..3 {
            enc.push_event(i, &make_event(i), &mut pool).unwrap();
        }
        assert_eq!(enc.event_count(), 3);

        let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        assert_eq!(block.header.event_count, 3);
        assert_eq!(block.header.first_sequence, 0);
        assert_eq!(block.header.last_sequence, 2);
        assert!(enc.is_empty());
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
}
