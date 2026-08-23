//! Canonical framing for Reflex Domain Bundles.
//!
//! This private workspace crate is the single format owner shared by the
//! production Runtime and its internal Experimental Harness. It deliberately
//! knows nothing about domain semantics or experimental treatments.

use std::fmt;

#[cfg(test)]
use std::cell::Cell;

use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"REFLEX\0\x03";
const SEGMENT_COUNT: usize = 5;
const ENCODED_SEGMENT_COUNT: u32 = 5;

#[cfg(test)]
thread_local! {
    static LOGICAL_CHECKSUM_CALLS: Cell<usize> = const { Cell::new(0) };
}

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
            Self::Revisions => 7,
        }
    }

    const fn legacy_version(self) -> u32 {
        match self {
            Self::Artifacts | Self::Experience => 2,
            Self::Recovery => 1,
            Self::Revisions => 4,
            Self::Session => self.current_version(),
        }
    }

    const fn is_compressed(self, version: u32) -> bool {
        matches!(self, Self::Artifacts | Self::Experience) && version == 3
    }

    const fn accepts(self, version: u32) -> bool {
        if matches!(self, Self::Revisions) {
            matches!(version, 4..=7)
        } else {
            version == self.current_version() || version == self.legacy_version()
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
    versions: [u32; SEGMENT_COUNT],
    logical_checksums: [[u8; 32]; SEGMENT_COUNT],
}

impl CanonicalBundle {
    #[must_use]
    pub fn compressed_segment_capacity(logical_bytes: usize) -> usize {
        8_usize.saturating_add(lz4_flex::block::get_maximum_output_size(logical_bytes))
    }

    #[must_use]
    pub fn new(
        identity: Vec<u8>,
        session: Vec<u8>,
        revisions: Vec<u8>,
        artifacts: Vec<u8>,
        experience: Vec<u8>,
        recovery: Vec<u8>,
    ) -> Self {
        let segments = [session, revisions, artifacts, experience, recovery];
        Self {
            identity,
            logical_checksums: std::array::from_fn(|index| logical_checksum(&segments[index])),
            segments,
            versions: std::array::from_fn(|index| SegmentKind::ALL[index].current_version()),
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
        let mut versions = [0_u32; SEGMENT_COUNT];
        let mut logical_checksums = [[0_u8; 32]; SEGMENT_COUNT];
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
            let encoded_logical_checksum: [u8; 32] = take(&mut input, 32)?
                .try_into()
                .map_err(|_| CodecError::Invalid)?;
            if logical_checksum(&payload) != encoded_logical_checksum {
                return Err(CodecError::Invalid);
            }
            segments[kind.index()] = payload;
            versions[kind.index()] = version;
            logical_checksums[kind.index()] = encoded_logical_checksum;
        }
        if !input.is_empty() {
            return Err(CodecError::Invalid);
        }
        Ok(Self {
            identity,
            segments,
            versions,
            logical_checksums,
        })
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
    pub fn segment_version(&self, kind: SegmentKind) -> u32 {
        self.versions[kind.index()]
    }

    #[must_use]
    pub fn restart_state_root(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        begin_restart_state_root(&mut digest, &self.identity);
        for kind in [
            SegmentKind::Revisions,
            SegmentKind::Artifacts,
            SegmentKind::Experience,
            SegmentKind::Recovery,
        ] {
            extend_restart_state_root(
                &mut digest,
                kind,
                self.segment_version(kind),
                &self.logical_checksums[kind.index()],
            );
        }
        digest.finalize().into()
    }

    pub fn encoded_restart_state_root(
        bytes: &[u8],
        expected_identity: &[u8],
    ) -> Result<[u8; 32], CodecError> {
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
        let mut digest = Sha256::new();
        begin_restart_state_root(&mut digest, identity);
        for expected in SegmentKind::ALL {
            let kind = SegmentKind::decode(take(&mut input, 1)?[0])?;
            let version = read_u32(&mut input)?;
            if kind != expected || !kind.accepts(version) {
                return Err(CodecError::Invalid);
            }
            let _stored = take_sized(&mut input)?;
            let logical_checksum = take(&mut input, 32)?;
            if kind != SegmentKind::Session {
                extend_restart_state_root(&mut digest, kind, version, logical_checksum);
            }
        }
        if !input.is_empty() {
            return Err(CodecError::Invalid);
        }
        Ok(digest.finalize().into())
    }

    #[must_use]
    pub fn logical_bytes(&self) -> u64 {
        self.segments.iter().fold(
            u64::try_from(self.identity.len()).unwrap_or(u64::MAX),
            |total, segment| total.saturating_add(u64::try_from(segment.len()).unwrap_or(u64::MAX)),
        )
    }

    pub fn replace_segment(&mut self, kind: SegmentKind, payload: Vec<u8>) {
        self.logical_checksums[kind.index()] = logical_checksum(&payload);
        self.segments[kind.index()] = payload;
    }

    pub fn replace_session(
        bytes: &[u8],
        expected_identity: &[u8],
        session: &[u8],
    ) -> Result<Vec<u8>, CodecError> {
        Self::replace_session_bounded(bytes, expected_identity, session, usize::MAX)
    }

    /// Replaces the Session without allowing the returned allocation to exceed
    /// `maximum_capacity`. The framing is scanned before the output allocation.
    pub fn replace_session_bounded(
        bytes: &[u8],
        expected_identity: &[u8],
        session: &[u8],
        maximum_capacity: usize,
    ) -> Result<Vec<u8>, CodecError> {
        let encoded_len = usize::try_from(Self::replacement_size_bound(
            bytes,
            &[(SegmentKind::Session, session.len() as u64)],
        )?)
        .map_err(|_| CodecError::OutputLimit)?;
        if encoded_len > maximum_capacity {
            return Err(CodecError::OutputLimit);
        }
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

        let mut output = Vec::with_capacity(encoded_len);
        if output.capacity() > maximum_capacity {
            return Err(CodecError::OutputLimit);
        }
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
        debug_assert_eq!(output.len(), encoded_len);
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

    /// Reads one logical segment length from canonical framing without allocating.
    ///
    /// The caller must already have authenticated `bytes`; this validates the
    /// framing and version of every segment but deliberately avoids rehashing the
    /// bundle. Compressed segments carry their logical length in their stored
    /// prefix, while uncompressed segments use their stored length directly.
    pub fn logical_segment_len(bytes: &[u8], target: SegmentKind) -> Result<u64, CodecError> {
        if bytes.len() < 32 {
            return Err(CodecError::Invalid);
        }
        let mut input = &bytes[..bytes.len() - 32];
        if take(&mut input, MAGIC.len())? != MAGIC {
            return Err(CodecError::Invalid);
        }
        let _identity = take_sized(&mut input)?;
        if read_u32(&mut input)? != ENCODED_SEGMENT_COUNT {
            return Err(CodecError::Invalid);
        }
        let mut target_len = None;
        for expected in SegmentKind::ALL {
            let kind = SegmentKind::decode(take(&mut input, 1)?[0])?;
            let version = read_u32(&mut input)?;
            if kind != expected || !kind.accepts(version) {
                return Err(CodecError::Invalid);
            }
            let stored = take_sized(&mut input)?;
            let _logical_checksum = take(&mut input, 32)?;
            if kind == target {
                target_len = Some(if kind.is_compressed(version) {
                    let mut compressed = stored;
                    read_u64(&mut compressed)?
                } else {
                    u64::try_from(stored.len()).map_err(|_| CodecError::Invalid)?
                });
            }
        }
        if !input.is_empty() {
            return Err(CodecError::Invalid);
        }
        target_len.ok_or(CodecError::Invalid)
    }

    #[must_use]
    /// Encodes without a caller-supplied output limit.
    ///
    /// # Panics
    ///
    /// Panics only if canonical length arithmetic violates the encoder's
    /// internal unbounded-output invariant.
    pub fn encode(&self) -> Vec<u8> {
        self.encode_bounded(usize::MAX)
            .expect("an unbounded canonical Bundle encoding cannot exceed its output limit")
    }

    /// Encodes the Bundle without allowing the returned allocation to exceed
    /// `maximum_capacity`.
    ///
    /// Compressed logical segments are prepared before the final allocation, so
    /// callers can reject the exact stored size before a durability handoff.
    pub fn encode_bounded(&self, maximum_capacity: usize) -> Result<Vec<u8>, CodecError> {
        let artifacts = SegmentKind::Artifacts
            .is_compressed(self.segment_version(SegmentKind::Artifacts))
            .then(|| compress_segment(self.segment(SegmentKind::Artifacts)));
        let experience = SegmentKind::Experience
            .is_compressed(self.segment_version(SegmentKind::Experience))
            .then(|| compress_segment(self.segment(SegmentKind::Experience)));
        let stored_len = |kind| match kind {
            SegmentKind::Artifacts => artifacts
                .as_deref()
                .map_or_else(|| self.segment(kind).len(), <[u8]>::len),
            SegmentKind::Experience => experience
                .as_deref()
                .map_or_else(|| self.segment(kind).len(), <[u8]>::len),
            _ => self.segment(kind).len(),
        };
        let encoded_len = MAGIC
            .len()
            .saturating_add(8)
            .saturating_add(self.identity.len())
            .saturating_add(4)
            .saturating_add(SegmentKind::ALL.into_iter().fold(0_usize, |bytes, kind| {
                bytes
                    .saturating_add(1 + 4 + 8 + 32)
                    .saturating_add(stored_len(kind))
            }))
            .saturating_add(32);
        if encoded_len > maximum_capacity {
            return Err(CodecError::OutputLimit);
        }
        let mut output = Vec::with_capacity(encoded_len);
        if output.capacity() > maximum_capacity {
            return Err(CodecError::OutputLimit);
        }
        output.extend_from_slice(MAGIC);
        push_bytes(&mut output, &self.identity);
        output.extend_from_slice(&ENCODED_SEGMENT_COUNT.to_le_bytes());
        for kind in SegmentKind::ALL {
            let stored = match kind {
                SegmentKind::Artifacts => artifacts.as_deref().unwrap_or(self.segment(kind)),
                SegmentKind::Experience => experience.as_deref().unwrap_or(self.segment(kind)),
                _ => self.segment(kind),
            };
            output.push(kind as u8);
            output.extend_from_slice(&self.segment_version(kind).to_le_bytes());
            push_bytes(&mut output, stored);
            output.extend_from_slice(&self.logical_checksums[kind.index()]);
        }
        let checksum = Sha256::digest(&output);
        output.extend_from_slice(&checksum);
        debug_assert_eq!(output.len(), encoded_len);
        if output.capacity() > maximum_capacity {
            return Err(CodecError::OutputLimit);
        }
        Ok(output)
    }

    #[cfg(test)]
    fn encode_with_compression(&self, compress: bool) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(MAGIC);
        push_bytes(&mut output, &self.identity);
        output.extend_from_slice(&ENCODED_SEGMENT_COUNT.to_le_bytes());
        for kind in SegmentKind::ALL {
            let payload = self.segment(kind);
            let version =
                if !compress && matches!(kind, SegmentKind::Artifacts | SegmentKind::Experience) {
                    kind.legacy_version()
                } else {
                    self.segment_version(kind)
                };
            let compressed = kind.is_compressed(version);
            let stored = if compressed {
                compress_segment(payload)
            } else {
                Box::<[u8]>::from(payload)
            };
            output.push(kind as u8);
            output.extend_from_slice(&version.to_le_bytes());
            push_bytes(&mut output, &stored);
            output.extend_from_slice(&self.logical_checksums[kind.index()]);
        }
        let checksum = Sha256::digest(&output);
        output.extend_from_slice(&checksum);
        output
    }
}

fn logical_checksum(payload: &[u8]) -> [u8; 32] {
    #[cfg(test)]
    LOGICAL_CHECKSUM_CALLS.set(LOGICAL_CHECKSUM_CALLS.get().saturating_add(1));
    Sha256::digest(payload).into()
}

#[cfg(test)]
fn take_logical_checksum_call_count() -> usize {
    LOGICAL_CHECKSUM_CALLS.replace(0)
}

fn compress_segment(payload: &[u8]) -> Box<[u8]> {
    let mut stored = vec![0; CanonicalBundle::compressed_segment_capacity(payload.len())];
    stored[..8].copy_from_slice(&(payload.len() as u64).to_le_bytes());
    let compressed = lz4_flex::block::compress_into(payload, &mut stored[8..])
        .expect("the published maximum LZ4 output capacity must accept the payload");
    stored.truncate(8 + compressed);
    stored.into_boxed_slice()
}

fn begin_restart_state_root(digest: &mut Sha256, identity: &[u8]) {
    digest.update(b"reflex-restart-state-v1\0");
    digest.update((identity.len() as u64).to_le_bytes());
    digest.update(identity);
}

fn extend_restart_state_root(
    digest: &mut Sha256,
    kind: SegmentKind,
    version: u32,
    logical_checksum: &[u8],
) {
    digest.update([kind as u8]);
    if matches!(kind, SegmentKind::Revisions | SegmentKind::Recovery) {
        digest.update(version.to_le_bytes());
    }
    digest.update(logical_checksum);
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
    OutputLimit,
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

    #[must_use]
    pub const fn is_output_limit(self) -> bool {
        matches!(self, Self::OutputLimit)
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
    use sha2::{Digest, Sha256};

    use super::{CanonicalBundle, CodecError, SegmentKind, take_logical_checksum_call_count};

    #[test]
    fn decoded_segment_versions_survive_a_logical_round_trip() {
        let bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session".to_vec(),
            b"legacy-revisions".to_vec(),
            b"artifacts".to_vec(),
            b"experience".to_vec(),
            b"recovery".to_vec(),
        );
        let legacy = relabel_segment_version(bundle.encode(), SegmentKind::Revisions, 4);

        let decoded = CanonicalBundle::decode(&legacy, u64::MAX).unwrap();
        assert_eq!(decoded.segment_version(SegmentKind::Revisions), 4);

        let reencoded = CanonicalBundle::decode(&decoded.encode(), u64::MAX).unwrap();
        assert_eq!(reencoded.segment_version(SegmentKind::Revisions), 4);
        assert_eq!(
            reencoded.segment(SegmentKind::Revisions),
            b"legacy-revisions"
        );
    }

    #[test]
    fn current_revisions_use_v7_while_frozen_v4_through_v6_remain_decodable() {
        let bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session".to_vec(),
            b"revisions".to_vec(),
            b"artifacts".to_vec(),
            b"experience".to_vec(),
            b"recovery".to_vec(),
        );

        assert_eq!(bundle.segment_version(SegmentKind::Revisions), 7);
        for version in 4..=6 {
            let legacy = relabel_segment_version(bundle.encode(), SegmentKind::Revisions, version);
            assert_eq!(
                CanonicalBundle::decode(&legacy, u64::MAX)
                    .unwrap()
                    .segment_version(SegmentKind::Revisions),
                version
            );
        }
    }

    #[test]
    fn logical_segment_lengths_are_read_from_framing_without_expansion() {
        let revisions = b"revisions".repeat(31);
        let experience = b"experience".repeat(10_000);
        let bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session".to_vec(),
            revisions.clone(),
            b"artifacts".repeat(10_000),
            experience.clone(),
            b"recovery".to_vec(),
        )
        .encode();

        assert_eq!(
            CanonicalBundle::logical_segment_len(&bundle, SegmentKind::Revisions).unwrap(),
            revisions.len() as u64
        );
        assert_eq!(
            CanonicalBundle::logical_segment_len(&bundle, SegmentKind::Experience).unwrap(),
            experience.len() as u64
        );
    }

    #[test]
    fn restart_state_root_binds_every_non_session_segment_but_not_session_usage() {
        let bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session-a".to_vec(),
            b"revisions".to_vec(),
            b"artifacts".to_vec(),
            b"experience".to_vec(),
            b"recovery".to_vec(),
        );
        let expected = bundle.restart_state_root();

        let mut changed_session = bundle.clone();
        changed_session.replace_segment(SegmentKind::Session, b"session-b".to_vec());
        assert_eq!(changed_session.restart_state_root(), expected);

        for kind in [
            SegmentKind::Revisions,
            SegmentKind::Artifacts,
            SegmentKind::Experience,
            SegmentKind::Recovery,
        ] {
            let mut changed_state = bundle.clone();
            changed_state.replace_segment(kind, vec![kind as u8, 0xff]);
            assert_ne!(changed_state.restart_state_root(), expected);
        }
    }

    #[test]
    fn encoded_restart_state_root_uses_logical_checksums_without_expansion() {
        let bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session".to_vec(),
            b"revisions".to_vec(),
            b"artifact".repeat(100_000),
            b"experience".repeat(100_000),
            b"recovery".to_vec(),
        );
        let encoded = bundle.encode();

        assert_eq!(
            CanonicalBundle::encoded_restart_state_root(&encoded, b"domain").unwrap(),
            bundle.restart_state_root()
        );
        assert!(CanonicalBundle::encoded_restart_state_root(&encoded, b"other").is_err());
    }

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
    fn logical_checksum_cache_is_content_derived_and_survives_clone_decode_and_replacement() {
        let mut bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session".to_vec(),
            b"revisions".to_vec(),
            b"artifacts".to_vec(),
            b"experience".to_vec(),
            b"recovery".to_vec(),
        );
        let session_checksum = [
            0x3f, 0x3a, 0xf1, 0xec, 0xeb, 0xbd, 0x14, 0x10, 0xab, 0x41, 0x7e, 0xc0, 0xd2, 0x7b,
            0xbf, 0xcb, 0x5d, 0x34, 0x0e, 0x17, 0x7a, 0xe1, 0x59, 0xb5, 0x9f, 0xc8, 0x62, 0x6c,
            0x2d, 0xfd, 0x91, 0x75,
        ];
        let changed_revisions_checksum = [
            0xd6, 0x7e, 0x2e, 0x94, 0x49, 0x94, 0x49, 0x6c, 0x8d, 0x8e, 0xc7, 0x6e, 0xed, 0x0c,
            0xf9, 0xf0, 0x96, 0x79, 0x44, 0x8d, 0x58, 0x4b, 0x53, 0x2b, 0xeb, 0xf9, 0x41, 0x85,
            0x2a, 0x37, 0xf5, 0xed,
        ];

        assert_eq!(
            bundle.logical_checksums[SegmentKind::Session.index()],
            session_checksum
        );
        assert_eq!(bundle.clone(), bundle);
        assert_eq!(
            CanonicalBundle::decode(&bundle.encode(), u64::MAX).unwrap(),
            bundle
        );

        bundle.replace_segment(SegmentKind::Revisions, b"changed".to_vec());
        assert_eq!(
            bundle.logical_checksums[SegmentKind::Revisions.index()],
            changed_revisions_checksum
        );
        assert_eq!(
            CanonicalBundle::decode(&bundle.encode(), u64::MAX).unwrap(),
            bundle
        );
    }

    #[test]
    fn restart_roots_and_encoding_reuse_validated_logical_checksums() {
        take_logical_checksum_call_count();
        let mut bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session".to_vec(),
            b"revisions".to_vec(),
            b"artifacts".to_vec(),
            b"experience".to_vec(),
            b"recovery".to_vec(),
        );
        assert_eq!(take_logical_checksum_call_count(), 5);

        let clone = bundle.clone();
        let expected_root = bundle.restart_state_root();
        assert_eq!(clone.restart_state_root(), expected_root);
        let encoded = bundle.encode();
        assert_eq!(take_logical_checksum_call_count(), 0);

        bundle.replace_segment(SegmentKind::Recovery, b"changed".to_vec());
        assert_ne!(bundle.restart_state_root(), expected_root);
        let replaced = bundle.encode();
        assert_eq!(take_logical_checksum_call_count(), 1);

        assert_eq!(CanonicalBundle::decode(&encoded, u64::MAX).unwrap(), clone);
        assert_eq!(take_logical_checksum_call_count(), 5);
        assert_eq!(
            CanonicalBundle::decode(&replaced, u64::MAX).unwrap(),
            bundle
        );
        assert_eq!(take_logical_checksum_call_count(), 5);
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

        let decoded =
            CanonicalBundle::decode(&bundle.encode_with_compression(false), u64::MAX).unwrap();

        assert_eq!(decoded.identity(), bundle.identity());
        for kind in SegmentKind::ALL {
            assert_eq!(decoded.segment(kind), bundle.segment(kind));
        }
        assert_eq!(decoded.segment_version(SegmentKind::Artifacts), 2);
        assert_eq!(decoded.segment_version(SegmentKind::Experience), 2);
        assert_eq!(
            decoded.restart_state_root(),
            bundle.restart_state_root(),
            "physical compression cannot change restart-complete state identity"
        );
        let reencoded = decoded.encode();
        let restored = CanonicalBundle::decode(&reencoded, u64::MAX).unwrap();
        assert_eq!(restored.segment_version(SegmentKind::Artifacts), 2);
        assert_eq!(restored.segment_version(SegmentKind::Experience), 2);
        assert_eq!(restored, decoded);
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

        let exact = usize::try_from(
            CanonicalBundle::replacement_size_bound(
                &encoded,
                &[(SegmentKind::Session, b"new-session".len() as u64)],
            )
            .unwrap(),
        )
        .unwrap();
        assert!(matches!(
            CanonicalBundle::replace_session_bounded(
                &encoded,
                b"domain",
                b"new-session",
                exact - 1
            ),
            Err(CodecError::OutputLimit)
        ));
        let bounded =
            CanonicalBundle::replace_session_bounded(&encoded, b"domain", b"new-session", exact)
                .unwrap();
        assert_eq!(bounded.capacity(), exact);
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

    #[test]
    fn bounded_encoding_admits_high_compression_by_exact_stored_capacity() {
        let bundle = CanonicalBundle::new(
            b"domain".to_vec(),
            b"session".to_vec(),
            b"revisions".to_vec(),
            vec![0; 4 * 1024 * 1024],
            vec![1; 4 * 1024 * 1024],
            b"recovery".to_vec(),
        );
        let encoded = bundle.encode();
        assert!(encoded.len() < usize::try_from(bundle.logical_bytes()).unwrap() / 100);

        let exact = bundle.encode_bounded(encoded.len()).unwrap();
        assert_eq!(exact, encoded);
        assert!(exact.capacity() <= encoded.len());
        assert_eq!(
            bundle.encode_bounded(encoded.len() - 1),
            Err(CodecError::OutputLimit)
        );
    }

    fn relabel_segment_version(mut bytes: Vec<u8>, target: SegmentKind, version: u32) -> Vec<u8> {
        let content_len = bytes.len() - 32;
        let mut offset = 8;
        let identity_len = usize::try_from(u64::from_le_bytes(
            bytes[offset..offset + 8].try_into().unwrap(),
        ))
        .unwrap();
        offset += 8 + identity_len + 4;
        for _ in 0..5 {
            let kind = bytes[offset];
            if kind == target as u8 {
                bytes[offset + 1..offset + 5].copy_from_slice(&version.to_le_bytes());
                let checksum: [u8; 32] = Sha256::digest(&bytes[..content_len]).into();
                bytes[content_len..].copy_from_slice(&checksum);
                return bytes;
            }
            let stored_len = usize::try_from(u64::from_le_bytes(
                bytes[offset + 5..offset + 13].try_into().unwrap(),
            ))
            .unwrap();
            offset += 13 + stored_len + 32;
        }
        panic!("encoded Bundle contains every canonical Segment")
    }
}
