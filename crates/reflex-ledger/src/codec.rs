//! Primitive field codecs for the binary event format.
//!
//! `Enc` writes into a caller-provided `BytesMut` (no allocation on the encode
//! hot path); `Dec` reads from a slice with strict bounds checking.

use crate::LedgerError;
use crate::event::{CANDIDATE_ROW_BYTES, CancellationTarget, CandidateUniverse, decode_event};
use bytes::BytesMut;
use reflex_types::{
    ArtifactId, CandidateId, CellId, Digest, DigestAlgorithm, EpisodeId, ExperimentId,
    FeatureSchemaId, GenerationId, MetricId, ModelCheckpointId, ResearchNodeId, StateId, TaskId,
    UnitId, VerifierId, WorkerId,
};

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

pub(crate) struct Enc<'a> {
    pub(crate) buf: &'a mut BytesMut,
}

impl Enc<'_> {
    pub fn u8(&mut self, v: u8) {
        self.buf.extend_from_slice(&[v]);
    }

    pub fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn f32(&mut self, v: f32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn f64(&mut self, v: f64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn bool(&mut self, v: bool) {
        self.u8(v as u8);
    }

    pub fn varint(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.buf.extend_from_slice(&[((v as u8) & 0x7f) | 0x80]);
            v >>= 7;
        }
        self.buf.extend_from_slice(&[v as u8]);
    }

    pub fn str(&mut self, s: &str) {
        self.varint(s.len() as u64);
        self.buf.extend_from_slice(s.as_bytes());
    }

    pub fn opt_str(&mut self, o: &Option<String>) {
        match o {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }

    pub fn id32(&mut self, d: &[u8; 32]) {
        self.buf.extend_from_slice(d);
    }

    pub fn digest(&mut self, d: &Digest) {
        self.u8(match d.algorithm {
            DigestAlgorithm::Blake3 => 0,
            DigestAlgorithm::Sha256 => 1,
        });
        self.buf.extend_from_slice(&d.bytes);
    }

    pub fn vec_bytes(&mut self, v: &[u8]) {
        self.varint(v.len() as u64);
        self.buf.extend_from_slice(v);
    }
}

/// Encodes tag + body for `event` into `buf` (used by the block encoder and by
/// tests). Sequence-delta framing is added by `EventEncoder` around this.
#[cfg(test)]
pub(crate) fn encode_event_into(buf: &mut BytesMut, event: &crate::event::Event) {
    let mut enc = Enc { buf };
    crate::event::encode_event(&mut enc, event);
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

pub(crate) struct Dec<'a> {
    pub data: &'a [u8],
    pub pos: usize,
    /// Candidate batch universe for prefix resolution of PolicyScore rows.
    /// `None` when decoding standalone payloads (tests, tooling).
    pub candidates: Option<&'a CandidateUniverse>,
}

const TRUNCATED: &str = "truncated event payload";
const BAD_UTF8: &str = "invalid utf-8 in event payload";

impl<'a> Dec<'a> {
    #[cfg(test)]
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            candidates: None,
        }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], LedgerError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| LedgerError::Encoding(TRUNCATED.into()))?;
        if end > self.data.len() {
            return Err(LedgerError::Encoding(TRUNCATED.into()));
        }
        let s = &self.data[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// Bounds an element count read from the wire against the remaining
    /// input before any allocation, so garbage lengths can never trigger
    /// huge reserves (fuzz/truncation safety).
    fn bound_count(&self, n: usize, bytes_per_entry: usize) -> Result<(), LedgerError> {
        let remaining = self.data.len().saturating_sub(self.pos);
        if n > remaining / bytes_per_entry {
            return Err(LedgerError::Encoding(TRUNCATED.into()));
        }
        Ok(())
    }

    pub fn u8(&mut self) -> Result<u8, LedgerError> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, LedgerError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32, LedgerError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn u64(&mut self) -> Result<u64, LedgerError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn f32(&mut self) -> Result<f32, LedgerError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn f64(&mut self) -> Result<f64, LedgerError> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn bool(&mut self) -> Result<bool, LedgerError> {
        Ok(self.u8()? != 0)
    }

    pub fn varint(&mut self) -> Result<u64, LedgerError> {
        let mut v = 0u64;
        let mut shift = 0u32;
        loop {
            let b = self.u8()?;
            if shift == 63 && (b & 0x7f) > 1 {
                return Err(LedgerError::Encoding("varint overflow".into()));
            }
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
            shift += 7;
        }
    }

    pub fn str_into(&mut self, out: &mut String) -> Result<(), LedgerError> {
        let len = self.varint()? as usize;
        let s = self.take(len)?;
        let s = std::str::from_utf8(s).map_err(|_| LedgerError::Encoding(BAD_UTF8.into()))?;
        out.clear();
        out.push_str(s);
        Ok(())
    }

    pub fn opt_str_into(&mut self, out: &mut Option<String>) -> Result<(), LedgerError> {
        if self.u8()? != 0 {
            let mut s = String::new();
            self.str_into(&mut s)?;
            *out = Some(s);
        } else {
            *out = None;
        }
        Ok(())
    }

    pub fn id32(&mut self) -> Result<[u8; 32], LedgerError> {
        let b = self.take(32)?;
        let mut out = [0u8; 32];
        out.copy_from_slice(b);
        Ok(out)
    }

    pub fn digest(&mut self) -> Result<Digest, LedgerError> {
        match self.u8()? {
            0 => Ok(Digest::from_blake3_bytes(self.id32()?)),
            1 => Ok(Digest::from_sha256_bytes(self.id32()?)),
            other => Err(LedgerError::Encoding(format!(
                "unknown digest algorithm tag {other}"
            ))),
        }
    }

    fn vec_bytes_into(&mut self, out: &mut Vec<u8>) -> Result<(), LedgerError> {
        let n = self.varint()? as usize;
        let data = self.take(n)?;
        out.clear();
        out.extend_from_slice(data);
        Ok(())
    }

    fn resolve_candidate(&self, prefix: u64) -> Result<CandidateId, LedgerError> {
        let universe = self.candidates.ok_or_else(|| {
            LedgerError::Encoding("policy score decode requires a candidate universe".into())
        })?;
        match universe.get(&prefix) {
            Some(v) if v.len() == 1 => Ok(v[0]),
            Some(v) => Err(LedgerError::Encoding(format!(
                "ambiguous candidate prefix {prefix:016x}: {} candidates match",
                v.len()
            ))),
            None => Err(LedgerError::Encoding(format!(
                "unknown candidate prefix {prefix:016x} (CandidateBatch must precede PolicyScore in the same segment)"
            ))),
        }
    }

    // -- per-family decoders (reuse String/Vec storage for zero-alloc scan) --

    pub fn decode_experiment_lifecycle(
        &mut self,
        out: &mut ExperimentLifecycleEvent,
    ) -> Result<(), LedgerError> {
        out.experiment_id = ExperimentId::from_blake3_bytes(self.id32()?);
        self.str_into(&mut out.action)?;
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_cell_lifecycle(
        &mut self,
        out: &mut CellLifecycleEvent,
    ) -> Result<(), LedgerError> {
        out.cell_id = CellId::from_blake3_bytes(self.id32()?);
        out.experiment_id = ExperimentId::from_blake3_bytes(self.id32()?);
        out.generation_id = GenerationId::from_blake3_bytes(self.id32()?);
        self.str_into(&mut out.action)?;
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_generation_lifecycle(
        &mut self,
        out: &mut GenerationLifecycleEvent,
    ) -> Result<(), LedgerError> {
        out.generation_id = GenerationId::from_blake3_bytes(self.id32()?);
        out.cell_id = CellId::from_blake3_bytes(self.id32()?);
        out.experiment_id = ExperimentId::from_blake3_bytes(self.id32()?);
        self.str_into(&mut out.action)?;
        self.opt_str_into(&mut out.outcome)?;
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_attempt_lifecycle(
        &mut self,
        out: &mut AttemptLifecycleEvent,
    ) -> Result<(), LedgerError> {
        out.attempt_id = self.u64()?;
        out.cell_id = CellId::from_blake3_bytes(self.id32()?);
        out.generation_id = GenerationId::from_blake3_bytes(self.id32()?);
        self.str_into(&mut out.action)?;
        self.opt_str_into(&mut out.outcome)?;
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_worker_session(
        &mut self,
        out: &mut WorkerSessionEvent,
    ) -> Result<(), LedgerError> {
        out.worker_id = WorkerId::from_blake3_bytes(self.id32()?);
        self.str_into(&mut out.action)?;
        self.vec_bytes_into(&mut out.calibration_data)?;
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_task_start(&mut self, out: &mut TaskStartEvent) -> Result<(), LedgerError> {
        out.task_id = TaskId::from_blake3_bytes(self.id32()?);
        out.cell_id = CellId::from_blake3_bytes(self.id32()?);
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_task_end(&mut self, out: &mut TaskEndEvent) -> Result<(), LedgerError> {
        out.task_id = TaskId::from_blake3_bytes(self.id32()?);
        out.cell_id = CellId::from_blake3_bytes(self.id32()?);
        self.str_into(&mut out.outcome)?;
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_episode_start(&mut self, out: &mut EpisodeStartEvent) -> Result<(), LedgerError> {
        out.episode_id = EpisodeId::from_blake3_bytes(self.id32()?);
        out.task_id = TaskId::from_blake3_bytes(self.id32()?);
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_episode_end(&mut self, out: &mut EpisodeEndEvent) -> Result<(), LedgerError> {
        out.episode_id = EpisodeId::from_blake3_bytes(self.id32()?);
        self.str_into(&mut out.status)?;
        out.actions_taken = self.u32()?;
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_state_discovery(
        &mut self,
        out: &mut StateDiscoveryEvent,
    ) -> Result<(), LedgerError> {
        out.state_id = StateId::from_blake3_bytes(self.id32()?);
        out.episode_id = EpisodeId::from_blake3_bytes(self.id32()?);
        out.depth = self.u32()?;
        Ok(())
    }

    pub fn decode_candidate_batch(
        &mut self,
        out: &mut CandidateBatchEvent,
    ) -> Result<(), LedgerError> {
        out.state_id = StateId::from_blake3_bytes(self.id32()?);
        let n = self.varint()? as usize;
        self.bound_count(n, 42)?;
        out.candidate_ids.clear();
        out.candidate_ids.reserve(n);
        out.classes.clear();
        out.classes.reserve(n);
        out.tie_breaks.clear();
        out.tie_breaks.reserve(n);
        for _ in 0..n {
            out.candidate_ids
                .push(CandidateId::from_blake3_bytes(self.id32()?));
            out.classes.push(self.u16()?);
            out.tie_breaks.push(self.u64()?);
        }
        Ok(())
    }

    pub fn decode_feature_ref(&mut self, out: &mut FeatureRefEvent) -> Result<(), LedgerError> {
        out.state_id = StateId::from_blake3_bytes(self.id32()?);
        out.feature_schema = FeatureSchemaId::from_blake3_bytes(self.id32()?);
        self.vec_bytes_into(&mut out.payload_handle)?;
        Ok(())
    }

    pub fn decode_policy_score(&mut self, out: &mut PolicyScoreEvent) -> Result<(), LedgerError> {
        out.state_id = StateId::from_blake3_bytes(self.id32()?);
        out.model_id = ModelCheckpointId::from_blake3_bytes(self.id32()?);
        let n = self.u16()? as usize;
        let selected = self.u16()?;
        out.candidate_ids.clear();
        out.candidate_ids.reserve(n);
        out.scores.clear();
        out.scores.reserve(n);
        for _ in 0..n {
            let prefix = self.u64()?;
            let score = self.f32()?;
            out.candidate_ids.push(self.resolve_candidate(prefix)?);
            out.scores.push(score);
        }
        out.selected_candidate = if n == 0 || selected as usize >= n {
            CandidateId::from_digest(Digest::ZERO)
        } else {
            out.candidate_ids[selected as usize]
        };
        Ok(())
    }

    pub fn decode_candidate_application(
        &mut self,
        out: &mut CandidateApplicationEvent,
    ) -> Result<(), LedgerError> {
        out.state_id = StateId::from_blake3_bytes(self.id32()?);
        out.candidate_id = CandidateId::from_blake3_bytes(self.id32()?);
        let n = self.varint()? as usize;
        self.bound_count(n, 32)?;
        out.and_child_states.clear();
        out.and_child_states.reserve(n);
        for _ in 0..n {
            out.and_child_states
                .push(StateId::from_blake3_bytes(self.id32()?));
        }
        self.str_into(&mut out.outcome)?;
        Ok(())
    }

    pub fn decode_cache_observation(
        &mut self,
        out: &mut CacheObservationEvent,
    ) -> Result<(), LedgerError> {
        out.state_id = StateId::from_blake3_bytes(self.id32()?);
        out.hit = self.bool()?;
        out.cache_key = self.digest()?;
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_artifact_construction(
        &mut self,
        out: &mut ArtifactConstructionEvent,
    ) -> Result<(), LedgerError> {
        out.artifact_id = ArtifactId::from_blake3_bytes(self.id32()?);
        out.episode_id = EpisodeId::from_blake3_bytes(self.id32()?);
        out.size_bytes = self.u64()?;
        Ok(())
    }

    pub fn decode_verification_receipt(
        &mut self,
        out: &mut VerificationReceiptEvent,
    ) -> Result<(), LedgerError> {
        out.artifact_id = ArtifactId::from_blake3_bytes(self.id32()?);
        out.verifier = VerifierId::from_blake3_bytes(self.id32()?);
        self.str_into(&mut out.status)?;
        out.cpu_ns = self.u64()?;
        out.wall_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_utility_observation(
        &mut self,
        out: &mut UtilityObservationEvent,
    ) -> Result<(), LedgerError> {
        out.subject = ResearchNodeId::from_blake3_bytes(self.id32()?);
        out.metric = MetricId::from_blake3_bytes(self.id32()?);
        out.value = self.f64()?;
        out.unit = UnitId::from_blake3_bytes(self.id32()?);
        Ok(())
    }

    pub fn decode_model_shadow_score(
        &mut self,
        out: &mut ModelShadowScoreEvent,
    ) -> Result<(), LedgerError> {
        out.model_id = ModelCheckpointId::from_blake3_bytes(self.id32()?);
        out.state_id = StateId::from_blake3_bytes(self.id32()?);
        out.candidate_id = CandidateId::from_blake3_bytes(self.id32()?);
        out.score = self.f32()?;
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_resource_sample(
        &mut self,
        out: &mut ResourceSampleEvent,
    ) -> Result<(), LedgerError> {
        out.user_cpu_ns = self.u64()?;
        out.sys_cpu_ns = self.u64()?;
        out.rss_bytes = self.u64()?;
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_lineage_edge(&mut self, out: &mut LineageEdgeEvent) -> Result<(), LedgerError> {
        out.parent = ResearchNodeId::from_blake3_bytes(self.id32()?);
        out.child = ResearchNodeId::from_blake3_bytes(self.id32()?);
        self.str_into(&mut out.edge_type)?;
        Ok(())
    }

    pub fn decode_incident(&mut self, out: &mut IncidentEvent) -> Result<(), LedgerError> {
        self.str_into(&mut out.severity)?;
        self.str_into(&mut out.description)?;
        if self.u8()? != 0 {
            out.episode_id = Some(EpisodeId::from_blake3_bytes(self.id32()?));
        } else {
            out.episode_id = None;
        }
        out.timestamp_ns = self.u64()?;
        Ok(())
    }

    pub fn decode_cancellation(&mut self, out: &mut CancellationEvent) -> Result<(), LedgerError> {
        match self.u8()? {
            0 => {
                out.target =
                    CancellationTarget::Episode(EpisodeId::from_blake3_bytes(self.id32()?));
            }
            1 => {
                out.target = CancellationTarget::Cell(CellId::from_blake3_bytes(self.id32()?));
            }
            2 => {
                let cell_id = CellId::from_blake3_bytes(self.id32()?);
                let attempt_id = self.u64()?;
                out.target = CancellationTarget::Attempt {
                    cell_id,
                    attempt_id,
                };
            }
            other => {
                return Err(LedgerError::Encoding(format!(
                    "unknown cancellation target tag {other}"
                )));
            }
        }
        self.str_into(&mut out.reason)?;
        out.timestamp_ns = self.u64()?;
        Ok(())
    }
}

#[allow(unused_imports)]
use crate::event::{
    ArtifactConstructionEvent, AttemptLifecycleEvent, CacheObservationEvent, CancellationEvent,
    CandidateApplicationEvent, CandidateBatchEvent, CellLifecycleEvent, EpisodeEndEvent,
    EpisodeStartEvent, ExperimentLifecycleEvent, FeatureRefEvent, GenerationLifecycleEvent,
    IncidentEvent, LineageEdgeEvent, ModelShadowScoreEvent, PolicyScoreEvent, ResourceSampleEvent,
    StateDiscoveryEvent, TaskEndEvent, TaskStartEvent, UtilityObservationEvent,
    VerificationReceiptEvent, WorkerSessionEvent,
};

/// Compute the class bitmask (event families present) for a block payload by
/// fully decoding it into a scratch buffer. Used by the index builder.
pub(crate) fn payload_classes(
    payload: &[u8],
    scratch: &mut crate::event::SequencedEvent,
) -> Result<u64, LedgerError> {
    let mut bits = 0u64;
    let mut pos = 0usize;
    // Candidate batch events earlier in the payload define the universe for
    // prefix resolution of candidate score rows (same rule as the reader).
    let mut universe: CandidateUniverse = Default::default();
    while pos < payload.len() {
        let mut dec = Dec {
            data: &payload[pos..],
            pos: 0,
            candidates: Some(&universe),
        };
        let _delta = dec.varint()?;
        let tag = dec.u8()?;
        bits |= 1u64
            .checked_shl(u32::from(tag))
            .ok_or_else(|| LedgerError::Encoding("invalid event tag".into()))?;
        decode_event(&mut dec, tag, &mut scratch.event)?;
        pos += dec.pos;
        if tag == crate::event::TAG_CANDIDATE_BATCH
            && let crate::event::Event::CandidateBatch(batch) = &scratch.event
        {
            for c in &batch.candidate_ids {
                universe
                    .entry(crate::event::candidate_prefix(c))
                    .or_default()
                    .push(*c);
            }
        }
    }
    Ok(bits)
}

/// Size of the per-candidate wire row; public for bench/tooling assertions.
pub fn candidate_row_bytes() -> usize {
    CANDIDATE_ROW_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::tests::make_all_events;
    use crate::event::{EVENT_TAG_COUNT, Event, PolicyScoreEvent};

    #[test]
    fn test_varint_roundtrip() {
        let values = [
            0u64,
            1,
            127,
            128,
            300,
            65535,
            1 << 20,
            u32::MAX as u64,
            u64::MAX,
        ];
        for v in values {
            let mut buf = BytesMut::new();
            let mut enc = Enc { buf: &mut buf };
            enc.varint(v);
            let mut dec = Dec::new(&buf);
            assert_eq!(dec.varint().unwrap(), v);
            assert_eq!(dec.pos, buf.len());
        }
    }

    #[test]
    fn test_varint_overflow_rejected() {
        let mut dec = Dec::new(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x02]);
        assert!(dec.varint().is_err());
    }

    #[test]
    fn test_policy_score_requires_universe() {
        let mut buf = BytesMut::new();
        let mut enc = Enc { buf: &mut buf };
        let ev = make_all_events()
            .into_iter()
            .find_map(|e| match e {
                Event::PolicyScore(p) => Some(p),
                _ => None,
            })
            .unwrap();
        crate::event::encode_event(&mut enc, &Event::PolicyScore(ev));
        let mut out = PolicyScoreEvent::default();
        let mut dec = Dec::new(&buf);
        assert!(dec.decode_policy_score(&mut out).is_err());
    }

    #[test]
    fn test_ambiguous_prefix_rejected() {
        let mut buf = BytesMut::new();
        let mut enc = Enc { buf: &mut buf };
        let ev = make_all_events()
            .into_iter()
            .find_map(|e| match e {
                Event::PolicyScore(p) => Some(p),
                _ => None,
            })
            .unwrap();
        crate::event::encode_event(&mut enc, &Event::PolicyScore(ev));
        // Two candidates sharing the same 8-byte prefix.
        let id_a = CandidateId::from_digest(Digest::hash_blake3(b"a"));
        let mut universe = CandidateUniverse::new();
        universe.insert(crate::event::candidate_prefix(&id_a), vec![id_a, id_a]);
        let mut out = PolicyScoreEvent::default();
        let mut dec = Dec {
            data: &buf,
            pos: 0,
            candidates: Some(&universe),
        };
        assert!(matches!(
            dec.decode_policy_score(&mut out),
            Err(LedgerError::Encoding(_))
        ));
    }

    #[test]
    fn test_payload_classes() {
        let events = make_all_events();
        let mut buf = BytesMut::new();
        let mut scratch = crate::event::SequencedEvent {
            sequence: 0,
            event: Event::ResourceSample(ResourceSampleEvent::default()),
        };
        let mut last_delta = 0u64;
        for ev in &events {
            crate::encoder::append_event_to_block(&mut buf, &mut last_delta, ev);
        }
        let bits = payload_classes(&buf, &mut scratch).unwrap();
        assert_eq!(bits.count_ones(), EVENT_TAG_COUNT as u32);
    }
}
