#![forbid(unsafe_code)]

use reflex_ledger::{
    BufferPool, CellLifecycleEvent, Event, EventEncoder, SegmentHeader, recover_segment,
};
use reflex_types::{CellId, Digest, ExperimentId, GenerationId};
use std::fs::{File, OpenOptions};
use std::io::Write;

#[test]
fn test_ledger_torn_tail_recovery() {
    let temp_dir = tempfile::tempdir().unwrap();
    let path = temp_dir.path().join("crash.segment");

    let header = SegmentHeader::new(1, 1, 1, 1, 0, Digest::hash_blake3(b"compat"));

    let mut file = File::create(&path).unwrap();
    file.write_all(&header.encode()).unwrap();

    let cell_id = CellId::from_digest(Digest::hash_blake3(b"cell-1"));
    let exp_id = ExperimentId::from_digest(Digest::hash_blake3(b"exp-1"));
    let gen_id = GenerationId::from_digest(Digest::hash_blake3(b"gen-1"));

    let event = Event::CellLifecycle(CellLifecycleEvent {
        cell_id,
        experiment_id: exp_id,
        generation_id: gen_id,
        action: "started".to_string(),
        timestamp_ns: 100,
    });

    let mut encoder = EventEncoder::new();
    let mut pool = BufferPool::new(64 * 1024);
    encoder.push_event(0, &event, &mut pool).unwrap();
    let block = encoder.take_nonempty_block(&mut pool).unwrap().unwrap();
    file.write_all(&block.header.encode()).unwrap();
    file.write_all(&block.payload).unwrap();
    file.sync_all().unwrap();
    drop(file);

    // Simulate partial/torn bytes appended at EOF
    let mut f = OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(&[0xFF, 0xFE, 0xFD]).unwrap();
    f.sync_all().unwrap();
    drop(f);

    // Recovery must gracefully recover the valid complete block prefix and ignore torn tail
    let recovered = recover_segment(&path).unwrap();
    let events = recovered.read_events(&path).unwrap();
    assert_eq!(events.len(), 1);
}
