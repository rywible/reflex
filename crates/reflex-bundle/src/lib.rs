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
            _ => Err(CodecError::Invalid),
        }
    }

    const fn current_version(self) -> u32 {
        match self {
            Self::Artifacts | Self::Experience => 3,
            Self::Recovery => 2,
            Self::Session => 1,
            Self::Revisions => 4,
        }
    }

    const fn legacy_version(self) -> u32 {
        match self {
            Self::Artifacts | Self::Experience => 2,
            Self::Recovery => 1,
            _ => self.current_version(),
        }
    }

    const fn is_compressed(self, version: u32) -> bool {
        matches!(self, Self::Artifacts | Self::Experience) && version == 3
    }

    const fn accepts(self, version: u32) -> bool {
        version == self.current_version() || version == self.legacy_version()
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

    pub fn decode(bytes: &[u8], maximum_logical_bytes: u64) -> Result<Self, CodecError> {
        if bytes.len() < 32 {
            return Err(CodecError::Invalid);
        }
        let content_len = bytes.len() - 32;
        let (content, checksum) = bytes.split_at(content_len);
        if Sha256::digest(content)[..] != *checksum {
            return Err(CodecError::Invalid);
        }
        let mut input = content;
        if take(&mut input, MAGIC.len())? != MAGIC {
            return Err(CodecError::Invalid);
        }
        let identity = take_sized(&mut input)?.to_vec();
        let mut logical_bytes = u64::try_from(identity.len()).map_err(|_| CodecError::Invalid)?;
        if logical_bytes > maximum_logical_bytes {
            return Err(CodecError::LogicalSizeLimit);
        }
        if read_u32(&mut input)? != ENCODED_SEGMENT_COUNT {
            return Err(CodecError::Invalid);
        }
        let mut segments: [Vec<u8>; SEGMENT_COUNT] = std::array::from_fn(|_| Vec::new());
        for expected in SegmentKind::ALL {
            let kind = SegmentKind::decode(take(&mut input, 1)?[0])?;
            let version = read_u32(&mut input)?;
            if kind != expected || !kind.accepts(version) {
                return Err(CodecError::Invalid);
            }
            let stored = take_sized(&mut input)?;
            let remaining = maximum_logical_bytes.saturating_sub(logical_bytes);
            let payload = if kind.is_compressed(version) {
                decompress_segment(stored, remaining)?
            } else {
                if u64::try_from(stored.len()).map_err(|_| CodecError::Invalid)? > remaining {
                    return Err(CodecError::LogicalSizeLimit);
                }
                stored.to_vec()
            };
            logical_bytes = logical_bytes
                .checked_add(u64::try_from(payload.len()).map_err(|_| CodecError::Invalid)?)
                .ok_or(CodecError::Invalid)?;
            if Sha256::digest(&payload)[..] != *take(&mut input, 32)? {
                return Err(CodecError::Invalid);
            }
            segments[kind.index()] = payload;
        }
        if !input.is_empty() {
            return Err(CodecError::Invalid);
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

    #[must_use]
    pub fn logical_bytes(&self) -> u64 {
        self.segments.iter().fold(
            u64::try_from(self.identity.len()).unwrap_or(u64::MAX),
            |total, segment| total.saturating_add(u64::try_from(segment.len()).unwrap_or(u64::MAX)),
        )
    }

    pub fn replace_segment(&mut self, kind: SegmentKind, payload: Vec<u8>) {
        self.segments[kind.index()] = payload;
    }

    pub fn replace_session(
        bytes: &[u8],
        expected_identity: &[u8],
        session: &[u8],
    ) -> Result<Vec<u8>, CodecError> {
        if bytes.len() < 32 {
            return Err(CodecError::Invalid);
        }
        let content_len = bytes.len() - 32;
        let (content, checksum) = bytes.split_at(content_len);
        if Sha256::digest(content)[..] != *checksum {
            return Err(CodecError::Invalid);
        }
        let mut input = content;
        if take(&mut input, MAGIC.len())? != MAGIC {
            return Err(CodecError::Invalid);
        }
        let identity = take_sized(&mut input)?;
        if identity != expected_identity {
            return Err(CodecError::IdentityMismatch);
        }
        if read_u32(&mut input)? != ENCODED_SEGMENT_COUNT {
            return Err(CodecError::Invalid);
        }

        let mut output = Vec::with_capacity(bytes.len().saturating_add(session.len()));
        output.extend_from_slice(MAGIC);
        push_bytes(&mut output, identity);
        output.extend_from_slice(&ENCODED_SEGMENT_COUNT.to_le_bytes());
        for expected in SegmentKind::ALL {
            let kind = SegmentKind::decode(take(&mut input, 1)?[0])?;
            let version = read_u32(&mut input)?;
            if kind != expected || !kind.accepts(version) {
                return Err(CodecError::Invalid);
            }
            let stored = take_sized(&mut input)?;
            let logical_checksum = take(&mut input, 32)?;
            output.push(kind as u8);
            if kind == SegmentKind::Session {
                output.extend_from_slice(&kind.current_version().to_le_bytes());
                push_bytes(&mut output, session);
                output.extend_from_slice(&Sha256::digest(session));
            } else {
                output.extend_from_slice(&version.to_le_bytes());
                push_bytes(&mut output, stored);
                output.extend_from_slice(logical_checksum);
            }
        }
        if !input.is_empty() {
            return Err(CodecError::Invalid);
        }
        let checksum = Sha256::digest(&output);
        output.extend_from_slice(&checksum);
        Ok(output)
    }

    /// Returns a conservative encoded-size bound after replacing logical segments.
    ///
    /// Unchanged segments retain their exact stored size. Compressed replacements
    /// use the compressor's published maximum output size, so callers can enforce
    /// a durable limit without encoding a speculative bundle. This parses framing
    /// but deliberately does not rehash a bundle already validated by its owner.
    pub fn replacement_size_bound(
        bytes: &[u8],
        replacements: &[(SegmentKind, u64)],
    ) -> Result<u64, CodecError> {
        if replacements.iter().enumerate().any(|(index, (kind, _))| {
            replacements[index + 1..]
                .iter()
                .any(|(later, _)| later == kind)
        }) {
            return Err(CodecError::Invalid);
        }
        if bytes.len() < 32 {
            return Err(CodecError::Invalid);
        }
        let content_len = bytes.len() - 32;
        let (content, _checksum) = bytes.split_at(content_len);
        let mut input = content;
        if take(&mut input, MAGIC.len())? != MAGIC {
            return Err(CodecError::Invalid);
        }
        let identity = take_sized(&mut input)?;
        if read_u32(&mut input)? != ENCODED_SEGMENT_COUNT {
            return Err(CodecError::Invalid);
        }
        let mut bound = MAGIC
            .len()
            .saturating_add(8)
            .saturating_add(identity.len())
            .saturating_add(4);
        for expected in SegmentKind::ALL {
            let kind = SegmentKind::decode(take(&mut input, 1)?[0])?;
            let version = read_u32(&mut input)?;
            if kind != expected || !kind.accepts(version) {
                return Err(CodecError::Invalid);
            }
            let stored = take_sized(&mut input)?;
            let _logical_checksum = take(&mut input, 32)?;
            let replacement = replacements
                .iter()
                .find_map(|(replacement_kind, logical_bytes)| {
                    (*replacement_kind == kind).then_some(*logical_bytes)
                });
            let stored_bound = if let Some(logical_bytes) = replacement {
                let logical_bytes =
                    usize::try_from(logical_bytes).map_err(|_| CodecError::Invalid)?;
                if matches!(kind, SegmentKind::Artifacts | SegmentKind::Experience) {
                    8_usize.saturating_add(lz4_flex::block::get_maximum_output_size(logical_bytes))
                } else {
                    logical_bytes
                }
            } else {
                stored.len()
            };
            bound = bound
                .saturating_add(1)
                .saturating_add(4)
                .saturating_add(8)
                .saturating_add(stored_bound)
                .saturating_add(32);
        }
        if !input.is_empty() {
            return Err(CodecError::Invalid);
        }
        bound = bound.saturating_add(32);
        u64::try_from(bound).map_err(|_| CodecError::Invalid)
    }

    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        self.encode_with_compression(true)
    }

    fn encode_with_compression(&self, compress: bool) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        push_bytes(&mut output, &self.identity);
        output.extend_from_slice(&ENCODED_SEGMENT_COUNT.to_le_bytes());
        for kind in SegmentKind::ALL {
            let payload = self.segment(kind);
            let compressed =
                compress && matches!(kind, SegmentKind::Artifacts | SegmentKind::Experience);
            let version = if compressed {
                kind.current_version()
            } else if matches!(kind, SegmentKind::Artifacts | SegmentKind::Experience) {
                kind.legacy_version()
            } else {
                kind.current_version()
            };
            let stored = if compressed {
                compress_segment(payload)
            } else {
                payload.to_vec()
            };
            output.push(kind as u8);
            output.extend_from_slice(&version.to_le_bytes());
            push_bytes(&mut output, &stored);
            output.extend_from_slice(&Sha256::digest(payload));
        }
        let checksum = Sha256::digest(&output);
        output.extend_from_slice(&checksum);
        output
    }
}

fn compress_segment(payload: &[u8]) -> Vec<u8> {
    let compressed = lz4_flex::block::compress(payload);
    let mut stored = Vec::with_capacity(8 + compressed.len());
    stored.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    stored.extend_from_slice(&compressed);
    stored
}

fn decompress_segment(stored: &[u8], maximum_logical_bytes: u64) -> Result<Vec<u8>, CodecError> {
    let mut input = stored;
    let uncompressed = usize::try_from(read_u64(&mut input)?).map_err(|_| CodecError::Invalid)?;
    let maximum = input.len().saturating_mul(1024).saturating_add(1024 * 1024);
    if uncompressed > maximum {
        return Err(CodecError::Invalid);
    }
    if u64::try_from(uncompressed).map_err(|_| CodecError::Invalid)? > maximum_logical_bytes {
        return Err(CodecError::LogicalSizeLimit);
    }
    lz4_flex::block::decompress(input, uncompressed).map_err(|_| CodecError::Invalid)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CodecError {
    Invalid,
    LogicalSizeLimit,
    IdentityMismatch,
}

impl CodecError {
    #[must_use]
    pub const fn is_logical_size_limit(self) -> bool {
        matches!(self, Self::LogicalSizeLimit)
    }

    #[must_use]
    pub const fn is_identity_mismatch(self) -> bool {
        matches!(self, Self::IdentityMismatch)
    }
}

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
    let count = usize::try_from(read_u64(input)?).map_err(|_| CodecError::Invalid)?;
    take(input, count)
}

fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], CodecError> {
    if input.len() < count {
        return Err(CodecError::Invalid);
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
        let decoded = CanonicalBundle::decode(&bundle.encode(), u64::MAX).unwrap();
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
        assert!(CanonicalBundle::decode(&bytes, u64::MAX).is_err());
    }

    #[test]
    fn repetitive_artifact_and_experience_segments_are_compressed() {
        let artifacts = b"persistent-proof-subtree".repeat(100_000);
        let experience = b"replayed-candidate-proof".repeat(100_000);
        let logical_bytes = artifacts.len() + experience.len();
        let bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session".to_vec(),
            b"revisions".to_vec(),
            artifacts,
            experience,
            b"recovery".to_vec(),
        );
        let encoded = bundle.encode();

        assert!(encoded.len() < logical_bytes / 10);
        assert_eq!(CanonicalBundle::decode(&encoded, u64::MAX).unwrap(), bundle);
        assert_eq!(bundle.logical_bytes(), logical_bytes as u64 + 30);
    }

    #[test]
    fn legacy_uncompressed_segments_remain_readable() {
        let bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session".to_vec(),
            b"revisions".to_vec(),
            b"artifacts".to_vec(),
            b"experience".to_vec(),
            b"recovery".to_vec(),
        );

        assert_eq!(
            CanonicalBundle::decode(&bundle.encode_with_compression(false), u64::MAX).unwrap(),
            bundle
        );
    }

    #[test]
    fn logical_expansion_is_bounded_before_decompression() {
        let bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            Vec::new(),
            Vec::new(),
            b"large-artifact".repeat(100_000),
            Vec::new(),
            Vec::new(),
        );
        let encoded = bundle.encode();

        assert!(CanonicalBundle::decode(&encoded, bundle.logical_bytes() - 1).is_err());
        assert_eq!(
            CanonicalBundle::decode(&encoded, bundle.logical_bytes()).unwrap(),
            bundle
        );
    }

    #[test]
    fn session_replacement_preserves_compressed_segments_without_expansion() {
        let bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"old-session".to_vec(),
            b"revisions".to_vec(),
            b"large-artifact".repeat(100_000),
            b"large-experience".repeat(100_000),
            b"recovery".to_vec(),
        );
        let encoded = bundle.encode();
        let replaced = CanonicalBundle::replace_session(&encoded, b"domain", b"new-session")
            .expect("a valid encoded bundle can replace its Session segment");
        let decoded = CanonicalBundle::decode(&replaced, u64::MAX).unwrap();

        assert_eq!(decoded.segment(SegmentKind::Session), b"new-session");
        assert_eq!(
            decoded.segment(SegmentKind::Artifacts),
            bundle.segment(SegmentKind::Artifacts)
        );
        assert_eq!(
            decoded.segment(SegmentKind::Experience),
            bundle.segment(SegmentKind::Experience)
        );
        assert!(CanonicalBundle::replace_session(&encoded, b"other-domain", b"new").is_err());
    }

    #[test]
    fn replacement_bound_covers_every_encoded_replacement() {
        let bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session".to_vec(),
            b"revisions".to_vec(),
            b"artifact".repeat(100),
            b"experience".repeat(100),
            b"recovery".to_vec(),
        );
        let encoded = bundle.encode();
        let new_experience = b"incompressible-ish-experience-0123456789".repeat(137);
        let new_recovery = b"candidate-tail-9876543210".repeat(91);
        let bound = CanonicalBundle::replacement_size_bound(
            &encoded,
            &[
                (SegmentKind::Experience, new_experience.len() as u64),
                (SegmentKind::Recovery, new_recovery.len() as u64),
            ],
        )
        .unwrap();
        let mut replaced = bundle;
        replaced.replace_segment(SegmentKind::Experience, new_experience);
        replaced.replace_segment(SegmentKind::Recovery, new_recovery);

        assert!(replaced.encode().len() as u64 <= bound);
        assert!(
            CanonicalBundle::replacement_size_bound(
                &encoded,
                &[(SegmentKind::Recovery, 1), (SegmentKind::Recovery, 2)]
            )
            .is_err()
        );
    }
}
