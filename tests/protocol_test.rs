use bytes::BytesMut;
use reflex_protocol::{HandshakeRequest, LengthDelimitedFrameCodec, PROTOCOL_VERSION};
use reflex_types::Digest;
use tokio_util::codec::{Decoder, Encoder};

#[test]
fn test_protocol_frame_roundtrip() {
    let mut codec = LengthDelimitedFrameCodec::new(1024 * 1024);
    let mut buf = BytesMut::new();

    let req = HandshakeRequest {
        min_protocol: PROTOCOL_VERSION,
        max_protocol: PROTOCOL_VERSION,
        framework_build: Digest::hash_blake3(b"build-1"),
        requested_max_frame_bytes: 4096,
    };
    let json_bytes = serde_json::to_vec(&req).unwrap();
    codec
        .encode(bytes::Bytes::from(json_bytes.clone()), &mut buf)
        .unwrap();

    let decoded = codec.decode(&mut buf).unwrap().unwrap();
    assert_eq!(&decoded[..], &json_bytes[..]);

    let decoded_req: HandshakeRequest = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(decoded_req.min_protocol, PROTOCOL_VERSION);
}
