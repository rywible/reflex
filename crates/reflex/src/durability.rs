use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::thread::JoinHandle;

use atomic_write_file::AtomicWriteFile;
use sha2::{Digest, Sha256};

pub(crate) const MAGIC: &[u8; 8] = b"REFLEX\0\x03";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub(crate) enum SegmentKind {
    Session = 1,
    Revisions = 2,
    Artifacts = 3,
    Experience = 4,
    Recovery = 5,
}

impl SegmentKind {
    fn decode(value: u8) -> Result<Self, CodecError> {
        match value {
            1 => Ok(Self::Session),
            2 => Ok(Self::Revisions),
            3 => Ok(Self::Artifacts),
            4 => Ok(Self::Experience),
            5 => Ok(Self::Recovery),
            _ => Err(CodecError),
        }
    }

    const fn current_version(self) -> u32 {
        match self {
            Self::Artifacts | Self::Experience => 2,
            Self::Session | Self::Recovery => 1,
            Self::Revisions => 4,
        }
    }
}

pub(crate) struct Segment {
    pub(crate) kind: SegmentKind,
    pub(crate) payload: Vec<u8>,
}

pub(crate) struct DecodedBundle {
    pub(crate) identity: Vec<u8>,
    segments: Vec<Segment>,
}

impl DecodedBundle {
    pub(crate) fn segment(&self, kind: SegmentKind) -> Result<&[u8], CodecError> {
        self.segments
            .iter()
            .find(|segment| segment.kind == kind)
            .map(|segment| segment.payload.as_slice())
            .ok_or(CodecError)
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct CodecError;

type PublicationResult = Result<u64, std::io::Error>;

struct Publication {
    bytes: Vec<u8>,
    result: SyncSender<PublicationResult>,
}

pub(crate) struct CheckpointWriter {
    sender: SyncSender<Option<Publication>>,
    pending: Option<Receiver<PublicationResult>>,
    pending_bytes: u64,
    worker: Option<JoinHandle<()>>,
}

impl CheckpointWriter {
    pub(crate) fn start(target: PathBuf) -> Result<Self, std::io::Error> {
        let (sender, receiver) = sync_channel::<Option<Publication>>(1);
        let worker = std::thread::Builder::new()
            .name("reflex-durability".into())
            .stack_size(512 * 1024)
            .spawn(move || {
                while let Ok(Some(publication)) = receiver.recv() {
                    let result = publish(&target, &publication.bytes);
                    let _ = publication.result.send(result);
                }
            })?;
        Ok(Self {
            sender,
            pending: None,
            pending_bytes: 0,
            worker: Some(worker),
        })
    }

    pub(crate) fn submit(&mut self, bytes: Vec<u8>) -> Result<(), std::io::Error> {
        self.barrier()?;
        self.pending_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        let (result, pending) = sync_channel(1);
        self.sender
            .send(Some(Publication { bytes, result }))
            .map_err(|_| worker_stopped())?;
        self.pending = Some(pending);
        Ok(())
    }

    pub(crate) fn barrier(&mut self) -> Result<u64, std::io::Error> {
        let Some(pending) = self.pending.take() else {
            return Ok(0);
        };
        let result = pending.recv().map_err(|_| worker_stopped())?;
        self.pending_bytes = 0;
        result
    }

    pub(crate) fn pending_bytes(&self) -> u64 {
        self.pending_bytes
    }

    pub(crate) fn finish(mut self) -> Result<(), std::io::Error> {
        self.barrier()?;
        self.sender.send(None).map_err(|_| worker_stopped())?;
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        }
        Ok(())
    }
}

impl Drop for CheckpointWriter {
    fn drop(&mut self) {
        if self.worker.is_none() {
            return;
        }
        let _ = self.barrier();
        let _ = self.sender.send(None);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
        }
    }
}

fn worker_stopped() -> std::io::Error {
    std::io::Error::other("Reflex durability worker stopped")
}

fn publish(target: &std::path::Path, bytes: &[u8]) -> PublicationResult {
    let mut file = AtomicWriteFile::open(target)?;
    if let Err(error) = file.write_all(bytes) {
        let _ = file.discard();
        return Err(error);
    }
    file.commit()?;
    test_fault_point("atomic-rename");
    u64::try_from(bytes.len()).map_err(|_| std::io::Error::other("bundle exceeds u64 bytes"))
}

#[cfg(debug_assertions)]
fn test_fault_point(phase: &str) {
    const PHASE_ENV: &str = "REFLEX_INTERNAL_TEST_FAULT_PHASE";
    const OCCURRENCE_ENV: &str = "REFLEX_INTERNAL_TEST_FAULT_OCCURRENCE";
    static OCCURRENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if std::env::var_os(PHASE_ENV).as_deref() != Some(std::ffi::OsStr::new(phase)) {
        return;
    }
    let expected = std::env::var(OCCURRENCE_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(1);
    if OCCURRENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1 == expected {
        std::process::abort();
    }
}

#[cfg(not(debug_assertions))]
#[inline(always)]
fn test_fault_point(_: &str) {}

pub(crate) fn seal(identity: &[u8], segments: &[Segment]) -> Result<Vec<u8>, CodecError> {
    if segments.len() != 5 || !segments.windows(2).all(|pair| pair[0].kind < pair[1].kind) {
        return Err(CodecError);
    }
    let mut output = Vec::new();
    output.extend_from_slice(MAGIC);
    push_bytes(&mut output, identity);
    push_u32(
        &mut output,
        u32::try_from(segments.len()).map_err(|_| CodecError)?,
    );
    for segment in segments {
        output.push(segment.kind as u8);
        push_u32(&mut output, segment.kind.current_version());
        push_bytes(&mut output, &segment.payload);
        output.extend_from_slice(&Sha256::digest(&segment.payload));
    }
    let checksum = Sha256::digest(&output);
    output.extend_from_slice(&checksum);
    Ok(output)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<DecodedBundle, CodecError> {
    if bytes.len() < 32 {
        return Err(CodecError);
    }
    let content_len = bytes.len() - 32;
    let (content, checksum) = bytes.split_at(content_len);
    if Sha256::digest(content)[..] != *checksum {
        return Err(CodecError);
    }
    let mut input = content;
    if take(&mut input, MAGIC.len())? != MAGIC {
        return Err(CodecError);
    }
    let identity = take_sized(&mut input)?.to_vec();
    let count = read_u32(&mut input)? as usize;
    if count != 5 {
        return Err(CodecError);
    }
    let mut segments = Vec::with_capacity(count);
    let mut prior = None;
    for _ in 0..count {
        let kind = SegmentKind::decode(take(&mut input, 1)?[0])?;
        if prior.is_some_and(|prior| prior >= kind)
            || read_u32(&mut input)? != kind.current_version()
        {
            return Err(CodecError);
        }
        prior = Some(kind);
        let payload = take_sized(&mut input)?.to_vec();
        if Sha256::digest(&payload)[..] != *take(&mut input, 32)? {
            return Err(CodecError);
        }
        segments.push(Segment { kind, payload });
    }
    if !input.is_empty() {
        return Err(CodecError);
    }
    Ok(DecodedBundle { identity, segments })
}

fn push_u32(output: &mut Vec<u8>, value: u32) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_le_bytes());
}

fn push_bytes(output: &mut Vec<u8>, value: &[u8]) {
    push_u64(output, value.len() as u64);
    output.extend_from_slice(value);
}

fn read_u32(input: &mut &[u8]) -> Result<u32, CodecError> {
    Ok(u32::from_le_bytes(
        take(input, 4)?.try_into().map_err(|_| CodecError)?,
    ))
}

fn read_u64(input: &mut &[u8]) -> Result<u64, CodecError> {
    Ok(u64::from_le_bytes(
        take(input, 8)?.try_into().map_err(|_| CodecError)?,
    ))
}

fn take_sized<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], CodecError> {
    let length = usize::try_from(read_u64(input)?).map_err(|_| CodecError)?;
    take(input, length)
}

fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], CodecError> {
    if input.len() < count {
        return Err(CodecError);
    }
    let (value, remainder) = input.split_at(count);
    *input = remainder;
    Ok(value)
}
