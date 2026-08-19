use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::Digest as Sha2Digest;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum TypeError {
    #[error("invalid digest format: {0}")]
    InvalidDigestFormat(String),
    #[error("invalid hex string: {0}")]
    InvalidHex(String),
    #[error("unknown digest algorithm: {0}")]
    UnknownAlgorithm(String),
    #[error("invalid ID prefix or schema")]
    InvalidId,
}

// ---------------------------------------------------------------------------
// Digest model (§7.1)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DigestAlgorithm {
    Blake3,
    Sha256,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct Digest {
    pub algorithm: DigestAlgorithm,
    pub bytes: [u8; 32],
}

impl Digest {
    pub const ZERO: Self = Self {
        algorithm: DigestAlgorithm::Blake3,
        bytes: [0u8; 32],
    };

    pub fn from_blake3_bytes(bytes: [u8; 32]) -> Self {
        Self {
            algorithm: DigestAlgorithm::Blake3,
            bytes,
        }
    }

    pub fn from_sha256_bytes(bytes: [u8; 32]) -> Self {
        Self {
            algorithm: DigestAlgorithm::Sha256,
            bytes,
        }
    }

    pub fn hash_blake3(data: &[u8]) -> Self {
        let hash = blake3::hash(data);
        Self {
            algorithm: DigestAlgorithm::Blake3,
            bytes: *hash.as_bytes(),
        }
    }

    pub fn hash_sha256(data: &[u8]) -> Self {
        let mut hasher = sha2::Sha256::new();
        hasher.update(data);
        let result = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&result);
        Self {
            algorithm: DigestAlgorithm::Sha256,
            bytes,
        }
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.bytes
    }

    pub fn to_hex(&self) -> String {
        let algo_str = match self.algorithm {
            DigestAlgorithm::Blake3 => "blake3",
            DigestAlgorithm::Sha256 => "sha256",
        };
        format!("{}:{}", algo_str, hex::encode(self.bytes))
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({})", self.to_hex())
    }
}

impl FromStr for Digest {
    type Err = TypeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(TypeError::InvalidDigestFormat(s.to_string()));
        }
        let algorithm = match parts[0] {
            "blake3" => DigestAlgorithm::Blake3,
            "sha256" => DigestAlgorithm::Sha256,
            other => return Err(TypeError::UnknownAlgorithm(other.to_string())),
        };
        let hex_str = parts[1];
        if hex_str.len() != 64 {
            return Err(TypeError::InvalidHex(format!(
                "expected 64 hex chars, got {}",
                hex_str.len()
            )));
        }
        let decoded = hex::decode(hex_str).map_err(|e| TypeError::InvalidHex(e.to_string()))?;
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&decoded);
        Ok(Digest { algorithm, bytes })
    }
}

impl Serialize for Digest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        Digest::from_str(&s).map_err(serde::de::Error::custom)
    }
}

// ---------------------------------------------------------------------------
// Canonical encoding trait (§7.2)
// ---------------------------------------------------------------------------

pub trait CanonicalEncode {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError>;
}

pub struct CanonicalWriter;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum CanonicalError {
    #[error("io error: {0}")]
    Io(String),
    #[error("unsupported type: {0}")]
    Unsupported(String),
}

// ---------------------------------------------------------------------------
// typed_id! macro (§7.1)
// ---------------------------------------------------------------------------

#[macro_export]
macro_rules! typed_id {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
        pub struct $name(pub $crate::Digest);

        impl $name {
            pub fn from_digest(digest: $crate::Digest) -> Self {
                Self(digest)
            }

            pub fn from_blake3_bytes(bytes: [u8; 32]) -> Self {
                Self($crate::Digest::from_blake3_bytes(bytes))
            }

            pub fn from_sha256_bytes(bytes: [u8; 32]) -> Self {
                Self($crate::Digest::from_sha256_bytes(bytes))
            }

            pub fn digest(&self) -> &$crate::Digest {
                &self.0
            }

            pub fn to_hex(&self) -> String {
                self.0.to_hex()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0.to_hex())
            }
        }

        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0.to_hex())
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::TypeError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let trimmed = s.trim();
                let inner =
                    if let Some(stripped) = trimmed.strip_prefix(concat!(stringify!($name), "(")) {
                        stripped.strip_suffix(')').unwrap_or(stripped)
                    } else {
                        trimmed
                    };
                let digest = $crate::Digest::from_str(inner)?;
                Ok(Self(digest))
            }
        }

        impl serde::Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(&self.0.to_hex())
            }
        }

        impl<'de> serde::Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let s = String::deserialize(deserializer)?;
                let digest = $crate::Digest::from_str(&s).map_err(serde::de::Error::custom)?;
                Ok(Self(digest))
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Typed IDs (§7.1)
// ---------------------------------------------------------------------------

typed_id!(CellId);
typed_id!(StateId);
typed_id!(CandidateId);
typed_id!(ArtifactId);
typed_id!(ModelCheckpointId);
typed_id!(ExperimentId);
typed_id!(WorkerId);
typed_id!(EpisodeId);
typed_id!(TaskId);
typed_id!(GenerationId);
typed_id!(KnowledgeEditionId);
typed_id!(KnowledgeRecordId);
typed_id!(ResearchNodeId);
typed_id!(EvaluatorId);
typed_id!(MetricId);
typed_id!(UnitId);
typed_id!(FeatureSchemaId);
typed_id!(ActionSchemaId);
typed_id!(ModelOutputSchemaId);
typed_id!(VerifierId);
typed_id!(AssumptionId);
typed_id!(DatasetId);
typed_id!(DatasetSchemaId);
typed_id!(DomainId);
typed_id!(RetrieverSpec);
typed_id!(CuratorSpec);
typed_id!(CompatibilityDigest);
typed_id!(BuildIdentity);

// ---------------------------------------------------------------------------
// Arena handles (§10.1)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateHandle(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateHandle(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateIndex(pub u32);

// ---------------------------------------------------------------------------
// Transition types (§10.3)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransitionOutcome {
    Closed,
    Obligations { group_id: u64 },
    Contradiction,
    Invalid { code: u32 },
    Unresolved { code: u32 },
}

// ---------------------------------------------------------------------------
// Candidate and feature memory model (§12.1)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CandidateBatch {
    pub group_offsets: Vec<u32>,
    pub ids: Vec<CandidateId>,
    pub classes: Vec<u16>,
    pub tie_breaks: Vec<u64>,
    pub payload_handles: Vec<CandidateHandle>,
    pub flags: Vec<u16>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FeatureBatch {
    pub rows: usize,
    pub cols: usize,
    pub values: Vec<f32>,
    pub schema: FeatureSchemaId,
}

impl FeatureBatch {
    pub fn new(rows: usize, cols: usize, schema: FeatureSchemaId) -> Self {
        Self {
            rows,
            cols,
            values: vec![0.0; rows * cols],
            schema,
        }
    }

    #[inline]
    pub fn row(&self, index: usize) -> &[f32] {
        let start = index * self.cols;
        &self.values[start..start + self.cols]
    }

    #[inline]
    pub fn row_mut(&mut self, index: usize) -> &mut [f32] {
        let start = index * self.cols;
        &mut self.values[start..start + self.cols]
    }
}

// ---------------------------------------------------------------------------
// Model system (§13.1)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Activation {
    ReLU,
    Gelu,
    Silu,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelArchitecture {
    Linear,
    Mlp {
        hidden: Vec<usize>,
        activation: Activation,
        bias: bool,
    },
    BottleneckMlp {
        bottleneck: usize,
        hidden: usize,
        activation: Activation,
    },
    BurnCustom {
        factory: RegisteredFactoryId,
        config: CanonicalConfig,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelRole {
    Ranker,
    TasteCritic,
    Proposal,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ModelDType {
    F32,
    F16,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BackendClass {
    Flex,
    CubeCL,
    Cuda,
    Metal,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ParameterBudget {
    pub max_parameters: u64,
    pub max_unique_parameters: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ModelSpec {
    pub role: ModelRole,
    pub architecture: ModelArchitecture,
    pub input_schema: FeatureSchemaId,
    pub output_schema: ModelOutputSchemaId,
    pub parameter_budget: ParameterBudget,
    pub dtype: ModelDType,
    pub canonical_backend: BackendClass,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RegisteredFactoryId(pub String);

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanonicalConfig(pub Vec<u8>);

impl CanonicalConfig {
    pub fn empty() -> Self {
        Self(Vec::new())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_digest_blake3_and_sha256() {
        let data = b"reflex verification test";
        let d1 = Digest::hash_blake3(data);
        let d2 = Digest::hash_sha256(data);
        assert_eq!(d1.algorithm, DigestAlgorithm::Blake3);
        assert_eq!(d2.algorithm, DigestAlgorithm::Sha256);
        assert_ne!(d1.bytes, d2.bytes);

        let hex1 = d1.to_hex();
        assert!(hex1.starts_with("blake3:"));
        let parsed1 = Digest::from_str(&hex1).unwrap();
        assert_eq!(d1, parsed1);

        let hex2 = d2.to_hex();
        assert!(hex2.starts_with("sha256:"));
        let parsed2 = Digest::from_str(&hex2).unwrap();
        assert_eq!(d2, parsed2);
    }

    #[test]
    fn test_typed_id_serialization() {
        let cell_id = CellId::from_digest(Digest::hash_blake3(b"cell-1"));
        let json = serde_json::to_string(&cell_id).unwrap();
        let deserialized: CellId = serde_json::from_str(&json).unwrap();
        assert_eq!(cell_id, deserialized);

        let disp = cell_id.to_string();
        let parsed: CellId = disp.parse().unwrap();
        assert_eq!(cell_id, parsed);

        let debug_str = format!("{cell_id:?}");
        let parsed_debug: CellId = debug_str.parse().unwrap();
        assert_eq!(cell_id, parsed_debug);
    }

    #[test]
    fn test_digest_zero() {
        assert_eq!(Digest::ZERO.algorithm, DigestAlgorithm::Blake3);
        assert_eq!(Digest::ZERO.bytes, [0u8; 32]);
    }

    #[test]
    fn test_transition_outcome_roundtrip() {
        let outcomes = vec![
            TransitionOutcome::Closed,
            TransitionOutcome::Obligations { group_id: 42 },
            TransitionOutcome::Contradiction,
            TransitionOutcome::Invalid { code: 100 },
            TransitionOutcome::Unresolved { code: 200 },
        ];
        for outcome in &outcomes {
            let json = serde_json::to_string(outcome).unwrap();
            let decoded: TransitionOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(*outcome, decoded);
        }
    }

    #[test]
    fn test_candidate_batch_default() {
        let batch = CandidateBatch::default();
        assert!(batch.group_offsets.is_empty());
        assert!(batch.ids.is_empty());
    }

    #[test]
    fn test_feature_batch_row_access() {
        let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(b"schema"));
        let mut fb = FeatureBatch::new(2, 3, schema);
        fb.values[0] = 1.0;
        fb.values[1] = 2.0;
        fb.values[2] = 3.0;
        fb.values[3] = 4.0;
        fb.values[4] = 5.0;
        fb.values[5] = 6.0;
        assert_eq!(fb.row(0), &[1.0, 2.0, 3.0]);
        assert_eq!(fb.row(1), &[4.0, 5.0, 6.0]);
    }

    #[test]
    fn test_model_spec_roundtrip() {
        let spec = ModelSpec {
            role: ModelRole::Ranker,
            architecture: ModelArchitecture::Mlp {
                hidden: vec![64, 32],
                activation: Activation::Gelu,
                bias: true,
            },
            input_schema: FeatureSchemaId::from_digest(Digest::hash_blake3(b"in")),
            output_schema: ModelOutputSchemaId::from_digest(Digest::hash_blake3(b"out")),
            parameter_budget: ParameterBudget {
                max_parameters: 1_000_000,
                max_unique_parameters: None,
            },
            dtype: ModelDType::F32,
            canonical_backend: BackendClass::Flex,
        };
        let json = serde_json::to_string(&spec).unwrap();
        let decoded: ModelSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec.role, decoded.role);
        assert_eq!(spec.architecture, decoded.architecture);
    }
}
