pub fn fuzz_envelope(data: &[u8]) {
    if data.len() > 10 * 1024 * 1024 {
        return; // Size limit
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
    }
}
