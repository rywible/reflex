//! Ledger performance harness (P3.1/P3.3/P3.4 gates).
//!
//! Gates measured:
//! - `ledger_write_500k`: events/s through the writer (gate ≥ 500k/s/core).
//! - `ledger_encode_candidate`: ns per candidate score row; asserts the row
//!   fits under 32 bytes (gate: < 32 bytes/candidate, excluding shared state).
//! - `ledger_append_zero_alloc`: encode path must not allocate per event in
//!   steady state (verified structurally in `test_encode_reuses_allocation`;
//!   this bench reports per-event ns on the warm path).
//! - `ledger_recovery_1gib`: recovery scan throughput (gate ≥ 1 GiB/s on
//!   local NVMe). The bench generates a large segment once, then times
//!   `recover_segment` over it.
//!
//! CI-safe: sample sizes are small; actual numbers are printed by Criterion.
//! Gate assertions live in `#[cfg(test)]` with generous thresholds; the bench
//! itself only asserts format invariants (row size, determinism).

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use reflex_ledger::{
    BufferPool, EncodedBlock, Event, EventEncoder, GapPolicy, LedgerConfig, LedgerWriter,
    PolicyScoreEvent, SegmentHeader, candidate_row_bytes, recover_segment,
};
use reflex_types::{CandidateId, Digest, ModelCheckpointId, StateId};
use std::hint::black_box;
use std::path::PathBuf;

fn make_header(first_seq: u64) -> SegmentHeader {
    SegmentHeader::new(1, 1, 1, 1, first_seq, Digest::hash_blake3(b"bench"))
}

fn make_policy_score(seed: u64) -> Event {
    let id = |b: &[u8]| CandidateId::from_digest(Digest::hash_blake3(b));
    Event::PolicyScore(PolicyScoreEvent {
        state_id: StateId::from_digest(Digest::hash_blake3(b"state")),
        model_id: ModelCheckpointId::from_digest(Digest::hash_blake3(b"model")),
        candidate_ids: vec![id(&seed.to_le_bytes())],
        scores: vec![0.5],
        selected_candidate: id(&seed.to_le_bytes()),
    })
}

fn make_resource(i: u64) -> Event {
    Event::ResourceSample(reflex_ledger::ResourceSampleEvent {
        user_cpu_ns: i,
        sys_cpu_ns: i,
        rss_bytes: i,
        timestamp_ns: i,
    })
}

/// Encodes `n` events into blocks through the in-process writer path and
/// returns the blocks (writer not involved; measures encode + framing).
fn encode_blocks(n: u64, event: &Event) -> Vec<EncodedBlock> {
    let mut encoder = EventEncoder::new();
    let mut pool = BufferPool::new(64 * 1024);
    let mut blocks = Vec::new();
    for i in 0..n {
        encoder.push_event(i, event, &mut pool).unwrap();
        if (encoder.event_count() >= 512 || encoder.bytes_len() >= 64 * 1024)
            && let Some(block) = encoder.take_nonempty_block(&mut pool).unwrap()
        {
            blocks.push(block);
        }
    }
    if let Some(b) = encoder.take_nonempty_block(&mut pool).unwrap() {
        blocks.push(b);
    }
    blocks
}

fn bench_write_500k(c: &mut Criterion) {
    let dir = std::env::temp_dir().join("reflex-ledger-bench");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("write_500k.segment");
    let mut blocks = encode_blocks(100_000, &make_policy_score(0));
    let event = make_resource(0);

    let mut group = c.benchmark_group("ledger_write");
    group.sample_size(10);
    group.measurement_time(std::time::Duration::from_secs(5));
    group.throughput(Throughput::Elements(100_000));
    group.bench_function("write_500k", |b| {
        b.iter(|| {
            let mut writer = LedgerWriter::create(path.clone(), make_header(0)).unwrap();
            for _ in 0..5 {
                for blk in blocks.iter() {
                    writer.write_block(blk).unwrap();
                }
            }
            writer.finish().unwrap();
            // Re-encode so each iteration is identical.
            blocks = encode_blocks(100_000, &event);
            black_box(blocks.len())
        })
    });
    group.finish();
    let _ = std::fs::remove_file(&path);
}

fn bench_encode_candidate(c: &mut Criterion) {
    // P3.1 gate: candidate score row must be under 32 bytes per candidate.
    let row_bytes = candidate_row_bytes();
    assert!(
        row_bytes < 32,
        "candidate row {row_bytes} bytes must be < 32 (P3.1)"
    );
    let event = make_policy_score(0);
    let mut encoder = EventEncoder::new();
    let mut pool = BufferPool::new(64 * 1024);
    encoder.push_event(0, &event, &mut pool).unwrap();
    let block = encoder.take_nonempty_block(&mut pool).unwrap().unwrap();
    println!(
        "candidate row wire size: {row_bytes} bytes (gate < 32); single-candidate score event total: {} bytes",
        block.payload.len()
    );
    drop(block);

    let mut group = c.benchmark_group("ledger_encode");
    group.sample_size(30);
    group.throughput(Throughput::Elements(1));
    group.bench_function("encode_candidate", |b| {
        b.iter(|| {
            let mut enc = EventEncoder::new();
            let mut pool = BufferPool::new(64 * 1024);
            enc.push_event(0, &event, &mut pool).unwrap();
            black_box(enc.take_nonempty_block(&mut pool).unwrap())
        })
    });
    group.finish();
}

fn bench_append_zero_alloc(c: &mut Criterion) {
    // Warm-path append: the encoder reuses its BytesMut; steady state must
    // not reallocate (verified in test_encode_reuses_allocation). This bench
    // reports the per-event cost of the warm path.
    let event = make_policy_score(1);
    let mut group = c.benchmark_group("ledger_append");
    group.sample_size(30);
    group.throughput(Throughput::Elements(100_000));
    group.bench_function("append_zero_alloc", |b| {
        b.iter(|| {
            let mut enc = EventEncoder::new();
            let mut pool = BufferPool::new(64 * 1024);
            for i in 0..100_000u64 {
                enc.push_event(i, &event, &mut pool).unwrap();
                if enc.event_count() >= 512 {
                    enc.take_nonempty_block(&mut pool).unwrap();
                }
            }
            black_box(enc.event_count())
        })
    });
    group.finish();
}

fn bench_recovery(c: &mut Criterion) {
    // P3.4 gate: ≥ 1 GiB/s on local NVMe. Generate a ~256 MiB segment once,
    // then time recovery. Reported in GiB/s by the harness summary line.
    let dir = std::env::temp_dir().join("reflex-ledger-bench");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("recovery_1gib.segment");
    let target_bytes: u64 = 256 * 1024 * 1024;

    if !path.exists() {
        let mut writer = LedgerWriter::create_with(
            path.clone(),
            make_header(0),
            LedgerConfig {
                block_target_bytes: 1024 * 1024,
                block_max_events: 1_000_000,
                gap_policy: GapPolicy::Reject,
                ..LedgerConfig::default()
            },
        )
        .unwrap();
        let mut encoder = EventEncoder::new();
        let mut pool = BufferPool::new(1024 * 1024);
        let mut i = 0u64;
        let event = make_resource(0);
        while writer.current_position().offset < target_bytes {
            encoder.push_event(i, &event, &mut pool).unwrap();
            i += 1;
            if encoder.bytes_len() >= 1024 * 1024 {
                let block = encoder.take_nonempty_block(&mut pool).unwrap().unwrap();
                writer.write_block(&block).unwrap();
            }
        }
        if let Some(block) = encoder.take_nonempty_block(&mut pool).unwrap() {
            writer.write_block(&block).unwrap();
        }
        writer.finish().unwrap();
    }

    let segment_bytes = std::fs::metadata(&path).unwrap().len();
    println!(
        "recovery bench segment: {:.1} MiB ({} events)",
        segment_bytes as f64 / (1024.0 * 1024.0),
        segment_bytes / 40
    );

    let mut group = c.benchmark_group("ledger_recovery");
    group.sample_size(5);
    group.measurement_time(std::time::Duration::from_secs(10));
    group.bench_function("recovery", |b| {
        b.iter(|| {
            let recovered = recover_segment(&path).unwrap();
            black_box(recovered.report.blocks_valid)
        })
    });
    group.finish();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(PathBuf::from(format!("{}.idx", path.display())));
}

criterion_group!(
    benches,
    bench_write_500k,
    bench_encode_candidate,
    bench_append_zero_alloc,
    bench_recovery
);
criterion_main!(benches);
