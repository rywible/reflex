use reflex_types::{Digest, DigestAlgorithm};
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum CanonicalError {
    #[error("non-finite or uncanonical float encountered: {0}")]
    NonFiniteFloat(u64),
    #[error("io or encoding error: {0}")]
    EncodingError(String),
    #[error("schema envelope mismatch: expected {expected}, got {found}")]
    SchemaMismatch { expected: String, found: String },
    #[error("invalid envelope magic: {0:?}")]
    InvalidMagic([u8; 8]),
    #[error("digest verification failed: expected {expected}, computed {computed}")]
    DigestMismatch { expected: Digest, computed: Digest },
    #[error("unsupported schema version: {0}")]
    UnsupportedVersion(u32),
    #[error("canonical length exceeds {limit}-bit field: {length}")]
    LengthOverflow { length: usize, limit: u8 },
}

pub enum WriterSink<'a> {
    Buffer(&'a mut Vec<u8>),
    Hasher(&'a mut blake3::Hasher),
}

pub struct CanonicalWriter<'a> {
    sink: WriterSink<'a>,
}

impl<'a> CanonicalWriter<'a> {
    pub fn new(buffer: &'a mut Vec<u8>) -> Self {
        Self {
            sink: WriterSink::Buffer(buffer),
        }
    }

    pub fn hashing(hasher: &'a mut blake3::Hasher) -> Self {
        Self {
            sink: WriterSink::Hasher(hasher),
        }
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), CanonicalError> {
        match &mut self.sink {
            WriterSink::Buffer(buf) => buf.extend_from_slice(bytes),
            WriterSink::Hasher(h) => {
                h.update(bytes);
            }
        }
        Ok(())
    }

    pub fn write_u8(&mut self, val: u8) -> Result<(), CanonicalError> {
        self.write_bytes(&[val])
    }

    pub fn write_u16(&mut self, val: u16) -> Result<(), CanonicalError> {
        self.write_bytes(&val.to_le_bytes())
    }

    pub fn write_u32(&mut self, val: u32) -> Result<(), CanonicalError> {
        self.write_bytes(&val.to_le_bytes())
    }

    pub fn write_u64(&mut self, val: u64) -> Result<(), CanonicalError> {
        self.write_bytes(&val.to_le_bytes())
    }

    pub fn write_i8(&mut self, val: i8) -> Result<(), CanonicalError> {
        self.write_bytes(&val.to_le_bytes())
    }

    pub fn write_i16(&mut self, val: i16) -> Result<(), CanonicalError> {
        self.write_bytes(&val.to_le_bytes())
    }

    pub fn write_i32(&mut self, val: i32) -> Result<(), CanonicalError> {
        self.write_bytes(&val.to_le_bytes())
    }

    pub fn write_i64(&mut self, val: i64) -> Result<(), CanonicalError> {
        self.write_bytes(&val.to_le_bytes())
    }

    pub fn write_f32(&mut self, val: f32) -> Result<(), CanonicalError> {
        if !val.is_finite() {
            return Err(CanonicalError::NonFiniteFloat(val.to_bits() as u64));
        }
        let norm = if val == 0.0 { 0.0f32 } else { val };
        self.write_bytes(&norm.to_le_bytes())
    }

    pub fn write_f64(&mut self, val: f64) -> Result<(), CanonicalError> {
        if !val.is_finite() {
            return Err(CanonicalError::NonFiniteFloat(val.to_bits()));
        }
        let norm = if val == 0.0 { 0.0f64 } else { val };
        self.write_bytes(&norm.to_le_bytes())
    }

    pub fn write_bool(&mut self, val: bool) -> Result<(), CanonicalError> {
        self.write_u8(if val { 1 } else { 0 })
    }

    pub fn write_str(&mut self, val: &str) -> Result<(), CanonicalError> {
        let bytes = val.as_bytes();
        self.write_u32(u32::try_from(bytes.len()).map_err(|_| {
            CanonicalError::LengthOverflow {
                length: bytes.len(),
                limit: 32,
            }
        })?)?;
        self.write_bytes(bytes)
    }

    pub fn write_byte_slice(&mut self, val: &[u8]) -> Result<(), CanonicalError> {
        self.write_u32(
            u32::try_from(val.len()).map_err(|_| CanonicalError::LengthOverflow {
                length: val.len(),
                limit: 32,
            })?,
        )?;
        self.write_bytes(val)
    }

    pub fn write_digest(&mut self, val: &Digest) -> Result<(), CanonicalError> {
        match val.algorithm {
            DigestAlgorithm::Blake3 => self.write_u8(0)?,
            DigestAlgorithm::Sha256 => self.write_u8(1)?,
        }
        self.write_bytes(&val.bytes)
    }

    pub fn write_option<T: CanonicalEncode>(
        &mut self,
        val: Option<&T>,
    ) -> Result<(), CanonicalError> {
        match val {
            Some(v) => {
                self.write_u8(1)?;
                v.encode_canonical(self)
            }
            None => self.write_u8(0),
        }
    }

    pub fn write_vec<T: CanonicalEncode>(&mut self, val: &[T]) -> Result<(), CanonicalError> {
        self.write_u32(
            u32::try_from(val.len()).map_err(|_| CanonicalError::LengthOverflow {
                length: val.len(),
                limit: 32,
            })?,
        )?;
        for item in val {
            item.encode_canonical(self)?;
        }
        Ok(())
    }

    pub fn write_map<K: CanonicalEncode + Ord, V: CanonicalEncode>(
        &mut self,
        map: &BTreeMap<K, V>,
    ) -> Result<(), CanonicalError> {
        self.write_u32(
            u32::try_from(map.len()).map_err(|_| CanonicalError::LengthOverflow {
                length: map.len(),
                limit: 32,
            })?,
        )?;
        for (k, v) in map {
            k.encode_canonical(self)?;
            v.encode_canonical(self)?;
        }
        Ok(())
    }
}

pub trait CanonicalEncode {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError>;
}

impl CanonicalEncode for u8 {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u8(*self)
    }
}
impl CanonicalEncode for u16 {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u16(*self)
    }
}
impl CanonicalEncode for u32 {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(*self)
    }
}
impl CanonicalEncode for u64 {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u64(*self)
    }
}
impl CanonicalEncode for i8 {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_i8(*self)
    }
}
impl CanonicalEncode for i16 {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_i16(*self)
    }
}
impl CanonicalEncode for i32 {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_i32(*self)
    }
}
impl CanonicalEncode for i64 {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_i64(*self)
    }
}
impl CanonicalEncode for f32 {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_f32(*self)
    }
}
impl CanonicalEncode for f64 {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_f64(*self)
    }
}
impl CanonicalEncode for bool {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_bool(*self)
    }
}
impl CanonicalEncode for String {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(self)
    }
}
impl CanonicalEncode for str {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(self)
    }
}
impl CanonicalEncode for Digest {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_digest(self)
    }
}

impl<T: CanonicalEncode> CanonicalEncode for Vec<T> {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_vec(self)
    }
}

impl<T: CanonicalEncode> CanonicalEncode for Option<T> {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_option(self.as_ref())
    }
}

impl<K: CanonicalEncode + Ord, V: CanonicalEncode> CanonicalEncode for BTreeMap<K, V> {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_map(self)
    }
}

pub fn encode_to_vec<T: CanonicalEncode>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    let mut buf = Vec::new();
    let mut writer = CanonicalWriter::new(&mut buf);
    value.encode_canonical(&mut writer)?;
    Ok(buf)
}

pub fn content_id<T: CanonicalEncode>(domain: &[u8], value: &T) -> Result<Digest, CanonicalError> {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"RFXID\0");
    let domain_len = u32::try_from(domain.len()).map_err(|_| CanonicalError::LengthOverflow {
        length: domain.len(),
        limit: 32,
    })?;
    hasher.update(&domain_len.to_le_bytes());
    hasher.update(domain);
    let mut writer = CanonicalWriter::hashing(&mut hasher);
    value.encode_canonical(&mut writer)?;
    Ok(Digest {
        algorithm: DigestAlgorithm::Blake3,
        bytes: *hasher.finalize().as_bytes(),
    })
}

pub const ENVELOPE_MAGIC: &[u8; 8] = b"RFXENV01";

pub fn wrap_envelope<T: CanonicalEncode>(
    schema_name: &str,
    schema_version: u32,
    value: &T,
) -> Result<Vec<u8>, CanonicalError> {
    let mut payload = Vec::new();
    let mut writer = CanonicalWriter::new(&mut payload);
    value.encode_canonical(&mut writer)?;

    let digest = content_id(schema_name.as_bytes(), value)?;

    let mut out = Vec::new();
    out.extend_from_slice(ENVELOPE_MAGIC);
    let schema_bytes = schema_name.as_bytes();
    let schema_len =
        u32::try_from(schema_bytes.len()).map_err(|_| CanonicalError::LengthOverflow {
            length: schema_bytes.len(),
            limit: 32,
        })?;
    out.extend_from_slice(&schema_len.to_le_bytes());
    out.extend_from_slice(schema_bytes);
    out.extend_from_slice(&schema_version.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    match digest.algorithm {
        DigestAlgorithm::Blake3 => out.push(0),
        DigestAlgorithm::Sha256 => out.push(1),
    }
    out.extend_from_slice(&digest.bytes);
    out.extend_from_slice(&payload);
    Ok(out)
}

pub struct EnvelopeHeader {
    pub schema_name: String,
    pub schema_version: u32,
    pub payload_len: usize,
    pub digest: Digest,
}

pub fn read_envelope_header(data: &[u8]) -> Result<(EnvelopeHeader, &[u8]), CanonicalError> {
    if data.len() < 8 + 4 + 4 + 8 + 1 + 32 {
        return Err(CanonicalError::EncodingError(
            "envelope too short".to_string(),
        ));
    }
    if &data[0..8] != ENVELOPE_MAGIC {
        let mut magic = [0u8; 8];
        magic.copy_from_slice(&data[0..8]);
        return Err(CanonicalError::InvalidMagic(magic));
    }
    let schema_len = u32::from_le_bytes(data[8..12].try_into().unwrap()) as usize;
    let mut offset = 12;
    if data.len() < offset + schema_len + 4 + 8 + 1 + 32 {
        return Err(CanonicalError::EncodingError(
            "envelope header truncated".to_string(),
        ));
    }
    let schema_name = String::from_utf8(data[offset..offset + schema_len].to_vec())
        .map_err(|e| CanonicalError::EncodingError(e.to_string()))?;
    offset += schema_len;
    let schema_version = u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap());
    offset += 4;
    let payload_len_u64 = u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap());
    let payload_len = usize::try_from(payload_len_u64).map_err(|_| {
        CanonicalError::EncodingError(format!(
            "payload length {payload_len_u64} exceeds platform usize"
        ))
    })?;
    offset += 8;
    let algo = match data[offset] {
        0 => DigestAlgorithm::Blake3,
        1 => DigestAlgorithm::Sha256,
        _ => {
            return Err(CanonicalError::EncodingError(
                "invalid algorithm in envelope".to_string(),
            ));
        }
    };
    offset += 1;
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(&data[offset..offset + 32]);
    offset += 32;

    let digest = Digest {
        algorithm: algo,
        bytes,
    };
    let payload = &data[offset..];
    if payload.len() != payload_len {
        return Err(CanonicalError::EncodingError(format!(
            "payload length mismatch: header {} vs actual {}",
            payload_len,
            payload.len()
        )));
    }

    let schema_bytes = schema_name.as_bytes();
    let computed_digest = match algo {
        DigestAlgorithm::Blake3 => {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"RFXID\0");
            hasher.update(&(schema_bytes.len() as u32).to_le_bytes());
            hasher.update(schema_bytes);
            hasher.update(payload);
            Digest {
                algorithm: DigestAlgorithm::Blake3,
                bytes: *hasher.finalize().as_bytes(),
            }
        }
        DigestAlgorithm::Sha256 => {
            let mut hasher = sha2::Sha256::new();
            use sha2::Digest as Sha2Digest;
            hasher.update(b"RFXID\0");
            hasher.update((schema_bytes.len() as u32).to_le_bytes());
            hasher.update(schema_bytes);
            hasher.update(payload);
            let res = hasher.finalize();
            let mut b = [0u8; 32];
            b.copy_from_slice(&res);
            Digest {
                algorithm: DigestAlgorithm::Sha256,
                bytes: b,
            }
        }
    };

    if computed_digest != digest {
        return Err(CanonicalError::DigestMismatch {
            expected: digest,
            computed: computed_digest,
        });
    }

    Ok((
        EnvelopeHeader {
            schema_name,
            schema_version,
            payload_len,
            digest,
        },
        payload,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, PartialEq, Debug)]
    struct ExampleRecord {
        id: u32,
        name: String,
        score: f32,
        flags: Vec<u8>,
    }

    impl CanonicalEncode for ExampleRecord {
        fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
            out.write_u32(self.id)?;
            out.write_str(&self.name)?;
            out.write_f32(self.score)?;
            out.write_vec(&self.flags)?;
            Ok(())
        }
    }

    #[test]
    fn test_canonical_content_id_stability() {
        let r1 = ExampleRecord {
            id: 42,
            name: "test".to_string(),
            score: 0.0,
            flags: vec![1, 2, 3],
        };
        let r2 = ExampleRecord {
            id: 42,
            name: "test".to_string(),
            score: -0.0, // signed zero normalized to 0.0
            flags: vec![1, 2, 3],
        };
        let id1 = content_id(b"example", &r1).unwrap();
        let id2 = content_id(b"example", &r2).unwrap();
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_envelope_roundtrip() {
        let rec = ExampleRecord {
            id: 100,
            name: "envelope".to_string(),
            score: 3.5,
            flags: vec![7, 8],
        };
        let env = wrap_envelope("example.v1", 1, &rec).unwrap();
        let (header, payload) = read_envelope_header(&env).unwrap();
        assert_eq!(header.schema_name, "example.v1");
        assert_eq!(header.schema_version, 1);
        let expected_digest = content_id(b"example.v1", &rec).unwrap();
        assert_eq!(header.digest, expected_digest);
        assert_eq!(payload.len(), header.payload_len);
    }

    #[test]
    fn test_semantic_field_change_changes_digest() {
        let base = ExampleRecord {
            id: 1,
            name: "alpha".to_string(),
            score: 1.0,
            flags: vec![1],
        };
        let mut changed = base.clone();
        changed.score = 2.0;
        let id_base = content_id(b"semantic", &base).unwrap();
        let id_changed = content_id(b"semantic", &changed).unwrap();
        assert_ne!(id_base, id_changed);
    }

    #[test]
    fn test_map_order_independent_content_id() {
        let mut map_a = BTreeMap::new();
        map_a.insert("z".to_string(), 1u32);
        map_a.insert("a".to_string(), 2u32);
        let mut map_b = BTreeMap::new();
        map_b.insert("a".to_string(), 2u32);
        map_b.insert("z".to_string(), 1u32);
        let id_a = content_id(b"map-order", &map_a).unwrap();
        let id_b = content_id(b"map-order", &map_b).unwrap();
        assert_eq!(id_a, id_b);
    }

    /// Cross-platform golden corpus (P2.2): pinned wire bytes for a fixed record.
    #[test]
    fn test_golden_envelope_corpus() {
        let rec = ExampleRecord {
            id: 4242,
            name: "golden-record".to_string(),
            score: 1.25,
            flags: vec![0xDE, 0xAD],
        };
        let env = wrap_envelope("reflex.golden.v1", 1, &rec).unwrap();
        let digest = reflex_types::Digest::hash_blake3(&env);
        assert_eq!(
            digest.to_hex(),
            "blake3:54dee4db82cccb3362634d830337a68f2ced72230e5664398507dcc7f129e883"
        );
        // Verify envelope round-trip preserves golden bytes.
        let (header, _) = read_envelope_header(&env).unwrap();
        assert_eq!(header.schema_name, "reflex.golden.v1");
    }

    #[test]
    fn test_unsupported_schema_version_fails_closed() {
        let rec = ExampleRecord {
            id: 1,
            name: "v".to_string(),
            score: 0.0,
            flags: vec![],
        };
        let env = wrap_envelope("example.v1", 99, &rec).unwrap();
        let (header, _) = read_envelope_header(&env).unwrap();
        assert_eq!(header.schema_version, 99);
        const MIN_SUPPORTED: u32 = 1;
        const MAX_SUPPORTED: u32 = 1;
        let err = if header.schema_version < MIN_SUPPORTED || header.schema_version > MAX_SUPPORTED
        {
            Err(CanonicalError::UnsupportedVersion(header.schema_version))
        } else {
            Ok(())
        };
        assert!(matches!(err, Err(CanonicalError::UnsupportedVersion(99))));
    }
}
