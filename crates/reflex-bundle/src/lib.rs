//! Canonical framing for Reflex Domain Bundles.
//!
//! This private workspace crate is the single format owner shared by the
//! production Runtime and its internal Experimental Harness. It deliberately
//! knows nothing about domain semantics or experimental treatments.

use std::fmt;

use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"REFLEX\0\x03";
const SEGMENT_COUNT: usize = 5;
const ENCODED_SEGMENT_COUNT: u32 = 5;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub enum SegmentKind {
    Session = 1,
    Revisions = 2,
    Artifacts = 3,
    Experience = 4,
    Recovery = 5,
}

impl SegmentKind {
    const ALL: [Self; SEGMENT_COUNT] = [
        Self::Session,
        Self::Revisions,
        Self::Artifacts,
        Self::Experience,
        Self::Recovery,
    ];

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

    const fn index(self) -> usize {
        self as usize - 1
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalBundle {
    identity: Vec<u8>,
    segments: [Vec<u8>; SEGMENT_COUNT],
}

impl CanonicalBundle {
    #[must_use]
    pub fn new(
        identity: Vec<u8>,
        session: Vec<u8>,
        revisions: Vec<u8>,
        artifacts: Vec<u8>,
        experience: Vec<u8>,
        recovery: Vec<u8>,
    ) -> Self {
        Self {
            identity,
            segments: [session, revisions, artifacts, experience, recovery],
        }
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, CodecError> {
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
        if read_u32(&mut input)? != ENCODED_SEGMENT_COUNT {
            return Err(CodecError);
        }
        let mut segments: [Vec<u8>; SEGMENT_COUNT] = std::array::from_fn(|_| Vec::new());
        for expected in SegmentKind::ALL {
            let kind = SegmentKind::decode(take(&mut input, 1)?[0])?;
            if kind != expected || read_u32(&mut input)? != kind.current_version() {
                return Err(CodecError);
            }
            let payload = take_sized(&mut input)?.to_vec();
            if Sha256::digest(&payload)[..] != *take(&mut input, 32)? {
                return Err(CodecError);
            }
            segments[kind.index()] = payload;
        }
        if !input.is_empty() {
            return Err(CodecError);
        }
        Ok(Self { identity, segments })
    }

    #[must_use]
    pub fn identity(&self) -> &[u8] {
        &self.identity
    }

    #[must_use]
    pub fn segment(&self, kind: SegmentKind) -> &[u8] {
        &self.segments[kind.index()]
    }

    pub fn replace_segment(&mut self, kind: SegmentKind, payload: Vec<u8>) {
        self.segments[kind.index()] = payload;
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        push_bytes(&mut output, &self.identity);
        output.extend_from_slice(&ENCODED_SEGMENT_COUNT.to_le_bytes());
        for kind in SegmentKind::ALL {
            let payload = self.segment(kind);
            output.push(kind as u8);
            output.extend_from_slice(&kind.current_version().to_le_bytes());
            push_bytes(&mut output, payload);
            output.extend_from_slice(&Sha256::digest(payload));
        }
        let checksum = Sha256::digest(&output);
        output.extend_from_slice(&checksum);
        output
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CodecError;

impl fmt::Display for CodecError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("invalid Reflex Domain Bundle")
    }
}

impl std::error::Error for CodecError {}

fn push_bytes(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
}

fn read_u32(input: &mut &[u8]) -> Result<u32, CodecError> {
    Ok(u32::from_le_bytes(
        take(input, 4)?
            .try_into()
            .expect("exactly four bytes taken"),
    ))
}

fn read_u64(input: &mut &[u8]) -> Result<u64, CodecError> {
    Ok(u64::from_le_bytes(
        take(input, 8)?
            .try_into()
            .expect("exactly eight bytes taken"),
    ))
}

fn take_sized<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], CodecError> {
    let count = usize::try_from(read_u64(input)?).map_err(|_| CodecError)?;
    take(input, count)
}

fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], CodecError> {
    if input.len() < count {
        return Err(CodecError);
    }
    let (value, remainder) = input.split_at(count);
    *input = remainder;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::{CanonicalBundle, SegmentKind};

    #[test]
    fn canonical_round_trip_and_replacement() {
        let mut bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session".to_vec(),
            b"revisions".to_vec(),
            b"artifacts".to_vec(),
            b"experience".to_vec(),
            b"recovery".to_vec(),
        );
        bundle.replace_segment(SegmentKind::Revisions, b"changed".to_vec());
        let decoded = CanonicalBundle::decode(&bundle.encode()).unwrap();
        assert_eq!(decoded, bundle);
        assert_eq!(decoded.segment(SegmentKind::Revisions), b"changed");
    }

    #[test]
    fn checksum_corruption_is_rejected() {
        let bundle = CanonicalBundle::new(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        let mut bytes = bundle.encode();
        bytes[0] ^= 1;
        assert!(CanonicalBundle::decode(&bytes).is_err());
    }
}
