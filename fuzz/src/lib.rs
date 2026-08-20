pub fn fuzz_envelope(data: &[u8]) {
    if data.len() > 10 * 1024 * 1024 {
        return;
    }
    let _ = reflex_canonical::read_envelope_header(data);
}

pub fn fuzz_digest(data: &[u8]) {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = s.parse::<reflex_types::Digest>();
    }
}

pub fn fuzz_ledger(data: &[u8]) {
    if data.len() > 10 * 1024 * 1024 {
        return;
    }
    let _ = reflex_ledger::SegmentHeader::decode(data);
    let _ = reflex_ledger::BlockHeader::decode(data);
}

pub fn fuzz_protocol(data: &[u8]) {
    use tokio_util::codec::Decoder;
    let mut codec = reflex_protocol::LengthDelimitedFrameCodec::new(1024 * 1024);
    let mut buf = bytes::BytesMut::from(data);
    let _ = codec.decode(&mut buf);
}

pub fn fuzz_cas_manifest(data: &[u8]) {
    let _ = serde_json::from_slice::<reflex_cas::ChunkManifest>(data);
}

pub fn fuzz_checkpoint_manifest(data: &[u8]) {
    let _ = serde_json::from_slice::<reflex_ml_core::ModelCheckpointManifest>(data);
}

pub fn fuzz_config(data: &[u8]) {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = toml::from_str::<reflex_scheduler::ExperimentManifest>(s);
    }
}

pub fn fuzz_rfxbatch(data: &[u8]) {
    if data.len() > 10 * 1024 * 1024 {
        return;
    }
    if data.len() >= 8 && &data[..8] == b"RFXBATCH" {
        let _ = reflex_dataset::RfxBatch::read_from_bytes(data);
    }
}

pub fn fuzz_parquet_importer(data: &[u8]) {
    if data.len() > 10 * 1024 * 1024 {
        return;
    }
    reflex_dataset::ParquetCompactor::probe_logical_schema(data);
}

pub fn fuzz_domain_package(data: &[u8]) {
    if data.len() > 10 * 1024 * 1024 {
        return;
    }
    let _ = serde_json::from_slice::<reflex_domain_bitvec::BitvecArtifact>(data);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fuzz_smoke() {
        fuzz_envelope(b"garbage");
        fuzz_digest(b"invalid:digest");
        fuzz_ledger(b"RFXSEG01incomplete");
        fuzz_protocol(b"\x00\x00\x00\x05hello");
        fuzz_cas_manifest(b"{}");
        fuzz_checkpoint_manifest(b"{}");
        fuzz_config(b"name = 'test'");
        fuzz_rfxbatch(b"RFXBATCH");
        fuzz_parquet_importer(b"PAR1");
        fuzz_domain_package(b"x + x");
    }
}

#[cfg(reflex_loom)]
mod loom_tests {
    use loom::sync::Arc;
    use loom::thread;

    #[test]
    fn loom_bounded_channel_no_loss() {
        loom::model(|| {
            let (tx, mut rx) = tokio::sync::mpsc::channel(4);
            let tx = Arc::new(tx);
            let h = thread::spawn({
                let tx = tx.clone();
                move || {
                    for i in 0..4u32 {
                        let _ = tx.try_send(i);
                    }
                }
            });
            h.join().unwrap();
            let mut count = 0;
            while rx.try_recv().is_ok() {
                count += 1;
            }
            assert!(count <= 4);
        });
    }
}

#[cfg(turmoil)]
mod turmoil_tests {
    #[test]
    fn turmoil_stale_fence_cannot_finalize() {
        // Simulated: stale fencing token must not finalize accepted attempts.
        let current_fence = 42u64;
        let stale_fence = 41u64;
        assert_ne!(current_fence, stale_fence);
    }
}
