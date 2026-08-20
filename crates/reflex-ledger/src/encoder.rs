//! Bounded, zero-allocation block encoder (§8.3).
//!
//! `EventEncoder` accumulates typed events into a reusable `BytesMut`. Block
//! extraction (`take_nonempty_block`) uses `BytesMut::split`, which keeps the
//! allocation with the encoder — steady-state block generation performs zero
//! heap allocations (verified by `test_encode_reuses_allocation`).

use crate::codec::Enc;
use crate::event::{Event, event_tag};
use crate::segment::BlockHeader;
use crate::{BufferPool, GapPolicy, LedgerError, SequenceGap};
use bytes::BytesMut;
use crc32c::crc32c;

/// Appends a framed event (uvarint seq delta + tag + body) to a block payload.
/// Used by the encoder and by test/index helpers.
#[cfg(test)]
pub(crate) fn append_event_to_block(buf: &mut BytesMut, last_delta: &mut u64, event: &Event) {
    let mut enc = Enc { buf };
    enc.varint(*last_delta);
    crate::event::encode_event(&mut enc, event);
    *last_delta += 1;
}

#[derive(Clone, Debug)]
pub struct EncodedBlock {
    pub header: BlockHeader,
    pub payload: BytesMut,
    /// Bitmask of event families present in this block (sidecar index data).
    pub classes: u64,
}

pub struct EventEncoder {
    buf: BytesMut,
    next_expected: u64,
    first_seq: u64,
    prev_seq: u64,
    count: u32,
    initialized: bool,
    classes: u64,
    gap_policy: GapPolicy,
    gaps: Vec<SequenceGap>,
}

impl EventEncoder {
    /// New encoder expecting the first pushed sequence to be 0.
    pub fn new() -> Self {
        Self::with_first_sequence(0, GapPolicy::Reject)
    }

    /// New encoder expecting `first_sequence` (the segment header's value).
    pub fn with_first_sequence(first_sequence: u64, gap_policy: GapPolicy) -> Self {
        Self {
            buf: BytesMut::with_capacity(64 * 1024),
            next_expected: first_sequence,
            first_seq: 0,
            prev_seq: 0,
            count: 0,
            initialized: false,
            classes: 0,
            gap_policy,
            gaps: Vec::new(),
        }
    }

    /// Appends `event` at sequence `seq`.
    ///
    /// Ordering validation (F-30 / P3.1): sequences must be strictly
    /// increasing and contiguous. Duplicates and backwards moves are always
    /// rejected; gaps are rejected by default (`GapPolicy::Reject`) or
    /// recorded (`GapPolicy::Record`).
    pub fn push_event(
        &mut self,
        seq: u64,
        event: &Event,
        _pool: &mut BufferPool,
    ) -> Result<(), LedgerError> {
        event.validate()?;
        if self.initialized {
            if seq <= self.prev_seq {
                return Err(LedgerError::SequenceMismatch {
                    offset: self.buf.len() as u64,
                    expected: self.next_expected,
                    found: seq,
                });
            }
            if seq != self.next_expected {
                match self.gap_policy {
                    GapPolicy::Reject => {
                        return Err(LedgerError::SequenceMismatch {
                            offset: self.buf.len() as u64,
                            expected: self.next_expected,
                            found: seq,
                        });
                    }
                    GapPolicy::Record => {
                        self.gaps.push(SequenceGap {
                            expected: self.next_expected,
                            found: seq,
                        });
                    }
                }
            }
            let mut enc = Enc { buf: &mut self.buf };
            enc.varint(seq - self.prev_seq);
        } else {
            self.first_seq = seq;
            self.initialized = true;
            let mut enc = Enc { buf: &mut self.buf };
            enc.varint(0);
        }
        crate::event::encode_event(&mut Enc { buf: &mut self.buf }, event);
        self.prev_seq = seq;
        self.next_expected = seq.wrapping_add(1);
        self.count += 1;
        self.classes |= 1u64 << event_tag(event);
        Ok(())
    }

    /// Extracts the accumulated events as one block, leaving the encoder empty
    /// with its allocation retained (zero-alloc steady state).
    pub fn take_nonempty_block(
        &mut self,
        _pool: &mut BufferPool,
    ) -> Result<Option<EncodedBlock>, LedgerError> {
        if self.count == 0 {
            return Ok(None);
        }
        let payload = self.buf.split();
        let stored = payload.len() as u32;
        let block = EncodedBlock {
            header: BlockHeader {
                stored_length: stored,
                uncompressed_length: stored,
                event_count: self.count,
                first_sequence: self.first_seq,
                last_sequence: self.prev_seq,
                flags: 0,
                crc32c: crc32c(&payload),
            },
            payload,
            classes: self.classes,
        };
        self.count = 0;
        self.initialized = false;
        self.classes = 0;
        Ok(Some(block))
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn event_count(&self) -> u32 {
        self.count
    }

    /// Number of encoded bytes buffered so far (for the byte-based block
    /// target).
    pub fn bytes_len(&self) -> usize {
        self.buf.len()
    }

    /// The next sequence the encoder will accept.
    pub fn next_sequence(&self) -> u64 {
        if self.initialized {
            self.next_expected
        } else {
            0
        }
    }

    /// Gaps observed under `GapPolicy::Record`.
    pub fn take_gaps(&mut self) -> Vec<SequenceGap> {
        std::mem::take(&mut self.gaps)
    }

    pub fn clear(&mut self, _pool: &mut BufferPool) {
        self.buf.clear();
        self.count = 0;
        self.initialized = false;
        self.classes = 0;
    }
}

impl Default for EventEncoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::tests::make_all_events;
    use crate::{BufferPool, LedgerError, SegmentHeader, event::ResourceSampleEvent};

    fn make_resource(i: u64) -> Event {
        Event::ResourceSample(ResourceSampleEvent {
            user_cpu_ns: i,
            sys_cpu_ns: i * 2,
            rss_bytes: i * 4,
            timestamp_ns: i * 1000,
        })
    }

    #[test]
    fn test_event_encoder_batch() {
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        for i in 0..3 {
            enc.push_event(i, &make_all_events()[0], &mut pool).unwrap();
        }
        assert_eq!(enc.event_count(), 3);
        let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        assert_eq!(block.header.event_count, 3);
        assert_eq!(block.header.first_sequence, 0);
        assert_eq!(block.header.last_sequence, 2);
        assert_eq!(block.header.stored_length, block.payload.len() as u32);
        assert_eq!(block.header.crc32c, crc32c(&block.payload));
        assert!(enc.is_empty());
    }

    #[test]
    fn test_sequence_gap_rejected_by_default() {
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        enc.push_event(0, &make_resource(0), &mut pool).unwrap();
        let err = enc.push_event(5, &make_resource(5), &mut pool).unwrap_err();
        assert!(matches!(err, LedgerError::SequenceMismatch { .. }));
    }

    #[test]
    fn test_sequence_duplicate_rejected() {
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        enc.push_event(3, &make_resource(3), &mut pool).unwrap();
        let err = enc.push_event(3, &make_resource(3), &mut pool).unwrap_err();
        assert!(matches!(err, LedgerError::SequenceMismatch { .. }));
    }

    #[test]
    fn test_first_sequence_from_header() {
        let header = SegmentHeader::new(1, 1, 1, 1, 42, reflex_types::Digest::hash_blake3(b"c"));
        let mut enc = EventEncoder::with_first_sequence(header.first_sequence, GapPolicy::Reject);
        let mut pool = BufferPool::new(1024);
        enc.push_event(42, &make_resource(42), &mut pool).unwrap();
        let block = enc.take_nonempty_block(&mut pool).unwrap().unwrap();
        assert_eq!(block.header.first_sequence, 42);
    }

    #[test]
    fn test_gap_record_policy() {
        let mut enc = EventEncoder::with_first_sequence(0, GapPolicy::Record);
        let mut pool = BufferPool::new(1024);
        enc.push_event(0, &make_resource(0), &mut pool).unwrap();
        enc.push_event(5, &make_resource(5), &mut pool).unwrap();
        let gaps = enc.take_gaps();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].expected, 1);
        assert_eq!(gaps[0].found, 5);
    }

    #[test]
    fn test_encode_reuses_allocation() {
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        for i in 0..100_000u64 {
            enc.push_event(i, &make_resource(i), &mut pool).unwrap();
            if enc.event_count() >= 512 {
                enc.take_nonempty_block(&mut pool).unwrap();
            }
        }
        let capacity_before = enc.buf.capacity();
        for i in 100_000..200_000u64 {
            enc.push_event(i, &make_resource(i), &mut pool).unwrap();
            if enc.event_count() >= 512 {
                enc.take_nonempty_block(&mut pool).unwrap();
            }
        }
        assert_eq!(enc.buf.capacity(), capacity_before, "encoder reallocated");
    }

    #[test]
    fn test_throughput_smoke_ci_safe() {
        // Generous CI-safe guard: the plan gate is 500k events/s/core; this
        // only protects against catastrophic regressions (10x headroom).
        let mut enc = EventEncoder::new();
        let mut pool = BufferPool::new(1024);
        let n = 200_000u64;
        let start = std::time::Instant::now();
        for i in 0..n {
            enc.push_event(i, &make_resource(i), &mut pool).unwrap();
            if enc.event_count() >= 512 {
                enc.take_nonempty_block(&mut pool).unwrap();
            }
        }
        let elapsed = start.elapsed().as_secs_f64();
        let rate = n as f64 / elapsed;
        assert!(
            rate >= 100_000.0,
            "encode rate {rate:.0} events/s below CI-safe floor"
        );
    }
}
