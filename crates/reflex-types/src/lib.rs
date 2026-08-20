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
        if hex_str.bytes().any(|byte| matches!(byte, b'A'..=b'F')) {
            return Err(TypeError::InvalidHex(
                "uppercase hexadecimal is not canonical".to_string(),
            ));
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
                let digest = $crate::Digest::from_str(s)?;
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

/// Stable typed handle into an [`EpisodeArena`] slot.
///
/// A state handle carries the arena generation it was issued in. The arena
/// stamps every handle with its current generation, and bumps the generation
/// whenever it is reset (`clear`). A handle whose generation no longer matches
/// the arena's current generation is stale: it must be rejected by every arena
/// accessor (P5.2 AC "stale handles are detected by arena generation").
///
/// The first tuple element is the slot index (kept as `.0` for compatibility),
/// the second is the issuing generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StateHandle(pub u32, pub u64);

impl StateHandle {
    /// Builds a handle for slot `index` issued in generation `generation`.
    pub const fn new(index: u32, generation: u64) -> Self {
        Self(index, generation)
    }

    /// Slot index inside the arena.
    #[inline]
    pub const fn index(self) -> u32 {
        self.0
    }

    /// Arena generation this handle was issued in.
    #[inline]
    pub const fn generation(self) -> u64 {
        self.1
    }
}

/// A handle issued by an arena in generation 0.
///
/// Only valid while the arena has never been reset; resetting an arena
/// invalidates every previously issued handle.
impl From<u32> for StateHandle {
    fn from(index: u32) -> Self {
        Self(index, 0)
    }
}

/// Batch-local payload cursor for a candidate inside a [`CandidateBatch`].
///
/// Candidate handles address a candidate within a single enumeration batch
/// (row in the structure-of-arrays columns); they are not arena-resident and
/// carry no generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateHandle(pub u32);

/// Selection index into a [`CandidateBatch`] (row position).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CandidateIndex(pub usize);

// ---------------------------------------------------------------------------
// Transition contract (§10.3)
// ---------------------------------------------------------------------------

/// Semantic/legality result code for a rejected candidate (§10.3).
///
/// `Invalid` is a *semantic* verdict about the candidate itself: the candidate
/// violates the domain's legality rules. It is not a resource-exhaustion
/// result (that is [`UnresolvedCode`]) and it does not by itself make the
/// candidate a training negative.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InvalidCandidateCode {
    /// The candidate violates the domain's declared legality rules.
    Illegal,
    /// The candidate class is not declared in the domain's action schema.
    UnknownClass,
    /// The candidate id is reused for different semantics within one
    /// enumeration (P5.6 AC: "a domain that reuses a candidate ID for
    /// different semantics fails").
    DuplicateId,
    /// The candidate is legal in general but not applicable to this state.
    Unsupported,
    /// A domain-internal invariant failed while checking legality.
    Internal,
    /// Domain-specific code, kept for migration from bare `u32` codes.
    Other(u32),
}

impl InvalidCandidateCode {
    /// Stable discriminator for compact outcome summaries.
    pub fn index(&self) -> u32 {
        match self {
            InvalidCandidateCode::Illegal => 0,
            InvalidCandidateCode::UnknownClass => 1,
            InvalidCandidateCode::DuplicateId => 2,
            InvalidCandidateCode::Unsupported => 3,
            InvalidCandidateCode::Internal => 4,
            InvalidCandidateCode::Other(code) => 100 + code,
        }
    }
}

/// Reason a domain could not determine an outcome within declared limits
/// (§10.3, §10.2).
///
/// `Unresolved` is censored: the outcome is unknown, never falsity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnresolvedCode {
    /// The domain exhausted its registered generation/verification budget.
    BudgetExhausted,
    /// The domain hit a wall-clock deadline.
    Timeout,
    /// The verifier was unavailable (failed closed).
    VerifierUnavailable,
    /// Candidate enumeration was explicitly capped by its registered
    /// generation budget (§10.2).
    CandidateCountCapped,
    /// Domain-specific code, kept for migration from bare `u32` codes.
    Other(u32),
}

impl UnresolvedCode {
    /// Stable discriminator for compact outcome summaries.
    pub fn index(&self) -> u32 {
        match self {
            UnresolvedCode::BudgetExhausted => 0,
            UnresolvedCode::Timeout => 1,
            UnresolvedCode::VerifierUnavailable => 2,
            UnresolvedCode::CandidateCountCapped => 3,
            UnresolvedCode::Other(code) => 100 + code,
        }
    }
}

/// Reference to the durable evidence that closed a state (§10.3).
///
/// A witness names the artifact (by content id) that closed the state and,
/// once verification has accepted it, the digest of the accepted
/// [`VerificationReceipt`]. The reference travels in the in-process outcome;
/// the objects it names live in CAS by digest.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DomainWitnessRef {
    /// Content id of the artifact that closed the state.
    pub artifact: ArtifactId,
    /// Digest of the accepted verification receipt, once verification exists.
    pub verification: Option<Digest>,
}

/// Reference to an AND group of child obligations (§10.3, §11.1).
///
/// AND semantics: every child in `children` must close for the group to close.
/// Search succeeds when *any* OR alternative produces a completely solved
/// artifact; it never succeeds when only one child of an AND group closes
/// (§11.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AndGroupRef {
    /// Group id, unique within the producing episode.
    pub group_id: u64,
    /// Child states that must ALL close.
    pub children: Vec<StateHandle>,
}

/// Result of applying one candidate to one state (§10.3).
///
/// `Invalid` is a semantic/legality verdict about the candidate; `Unresolved`
/// means the domain could not decide within declared limits. Neither
/// automatically makes the candidate a training negative.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TransitionOutcome {
    /// The state is closed; `witness` names the closing artifact.
    Closed { witness: DomainWitnessRef },
    /// The candidate opened an AND group of child obligations.
    Obligations { group: AndGroupRef },
    /// The candidate is certified dead; `certificate` names the counter-
    /// evidence when one exists.
    Contradiction {
        certificate: Option<DomainWitnessRef>,
    },
    /// The candidate is semantically invalid for this state.
    Invalid { code: InvalidCandidateCode },
    /// The domain could not determine the outcome within declared limits.
    Unresolved { code: UnresolvedCode },
}

impl TransitionOutcome {
    /// `Closed` referencing the artifact (by content id) that closes the state.
    pub fn closed(artifact: ArtifactId) -> Self {
        Self::Closed {
            witness: DomainWitnessRef {
                artifact,
                verification: None,
            },
        }
    }

    /// `Closed` with a fully formed witness.
    pub fn closed_with(witness: DomainWitnessRef) -> Self {
        Self::Closed { witness }
    }

    /// `Obligations` over an AND group (all children must close).
    pub fn obligations(group_id: u64, children: Vec<StateHandle>) -> Self {
        Self::Obligations {
            group: AndGroupRef { group_id, children },
        }
    }

    /// `Contradiction` without a certificate reference.
    pub fn contradiction() -> Self {
        Self::Contradiction { certificate: None }
    }

    /// `Contradiction` naming its counter-evidence.
    pub fn contradiction_with(certificate: DomainWitnessRef) -> Self {
        Self::Contradiction {
            certificate: Some(certificate),
        }
    }

    /// `Invalid` with a semantic legality code.
    pub fn invalid(code: InvalidCandidateCode) -> Self {
        Self::Invalid { code }
    }

    /// `Unresolved` with a censored reason.
    pub fn unresolved(code: UnresolvedCode) -> Self {
        Self::Unresolved { code }
    }

    /// Whether this outcome closed the state.
    pub fn is_closed(&self) -> bool {
        matches!(self, TransitionOutcome::Closed { .. })
    }

    /// Whether this outcome is censored (domain could not decide).
    pub fn is_unresolved(&self) -> bool {
        matches!(self, TransitionOutcome::Unresolved { .. })
    }

    /// The AND group of child obligations, if this outcome opened one.
    pub fn as_obligations(&self) -> Option<&AndGroupRef> {
        match self {
            TransitionOutcome::Obligations { group } => Some(group),
            _ => None,
        }
    }

    /// Group id of the AND group, if this outcome opened one.
    pub fn group_id(&self) -> Option<u64> {
        self.as_obligations().map(|g| g.group_id)
    }

    /// The closing artifact reference, if this outcome closed the state.
    pub fn as_witness(&self) -> Option<&DomainWitnessRef> {
        match self {
            TransitionOutcome::Closed { witness } => Some(witness),
            _ => None,
        }
    }

    /// The counter-evidence certificate, if this outcome is a contradiction.
    pub fn certificate(&self) -> Option<&DomainWitnessRef> {
        match self {
            TransitionOutcome::Contradiction { certificate } => certificate.as_ref(),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Structure-of-arrays columns (§12.1)
// ---------------------------------------------------------------------------

/// Contiguous column container for candidate/feature batches (§12.1).
///
/// The plan calls for cache-line-aligned storage for the hot structure-of-
/// arrays columns. True aligned allocation requires `allocator_api` or
/// `unsafe`; this workspace forbids `unsafe` (`#![forbid(unsafe_code)]`), so
/// `AlignedVec` currently wraps `Vec` with natural element alignment and is
/// the designated replacement point once an aligned-allocator ADR exists.
///
/// The vector is opaque to layout so the allocation strategy can change
/// without touching callers; the element API is identical to `Vec`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(transparent)]
pub struct AlignedVec<T>(Vec<T>);

impl<T> AlignedVec<T> {
    /// Empty vector.
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// Wraps an existing allocation.
    pub fn from_vec(vec: Vec<T>) -> Self {
        Self(vec)
    }

    /// Unwraps to the backing `Vec`.
    pub fn into_vec(self) -> Vec<T> {
        self.0
    }

    /// Number of elements.
    #[inline]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the column is empty.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Contiguous element slice.
    #[inline]
    pub fn as_slice(&self) -> &[T] {
        &self.0
    }

    /// Contiguous mutable element slice.
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [T] {
        &mut self.0
    }
}

impl<T> std::ops::Deref for AlignedVec<T> {
    type Target = Vec<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for AlignedVec<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl<T> From<Vec<T>> for AlignedVec<T> {
    fn from(vec: Vec<T>) -> Self {
        Self(vec)
    }
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
        assert!(debug_str.parse::<CellId>().is_err());
    }

    #[test]
    fn digest_parser_rejects_noncanonical_uppercase() {
        let canonical = Digest::hash_blake3(b"uppercase").to_hex();
        let (algorithm, hex) = canonical.split_once(':').unwrap();
        let text = format!("{algorithm}:{}", hex.to_uppercase());
        assert!(Digest::from_str(&text).is_err());
    }

    #[test]
    fn test_digest_zero() {
        assert_eq!(Digest::ZERO.algorithm, DigestAlgorithm::Blake3);
        assert_eq!(Digest::ZERO.bytes, [0u8; 32]);
    }

    #[test]
    fn test_transition_outcome_roundtrip() {
        let states = vec![StateHandle::new(0, 1), StateHandle::new(1, 1)];
        let outcomes = vec![
            TransitionOutcome::closed(ArtifactId::from_digest(Digest::hash_blake3(b"a"))),
            TransitionOutcome::obligations(7, states.clone()),
            TransitionOutcome::contradiction(),
            TransitionOutcome::invalid(InvalidCandidateCode::Illegal),
            TransitionOutcome::invalid(InvalidCandidateCode::DuplicateId),
            TransitionOutcome::invalid(InvalidCandidateCode::Other(17)),
            TransitionOutcome::unresolved(UnresolvedCode::BudgetExhausted),
            TransitionOutcome::unresolved(UnresolvedCode::Timeout),
            TransitionOutcome::unresolved(UnresolvedCode::VerifierUnavailable),
            TransitionOutcome::unresolved(UnresolvedCode::Other(3)),
        ];
        for outcome in &outcomes {
            let json = serde_json::to_string(outcome).unwrap();
            let decoded: TransitionOutcome = serde_json::from_str(&json).unwrap();
            assert_eq!(*outcome, decoded);
        }
    }

    #[test]
    fn test_transition_outcome_and_group_semantics() {
        let group = AndGroupRef {
            group_id: 1,
            children: vec![StateHandle::new(2, 0)],
        };
        // AND semantics: the group is only closed when every child closes.
        assert!(!group.children.is_empty());
        let outcome = TransitionOutcome::Obligations { group };
        assert!(matches!(
            &outcome,
            TransitionOutcome::Obligations { group: g } if g.group_id == 1
        ));
        assert_eq!(outcome.group_id().expect("obligations carry a group"), 1);
    }

    #[test]
    fn test_state_handle_generation() {
        let h = StateHandle::new(3, 42);
        assert_eq!(h.index(), 3);
        assert_eq!(h.generation(), 42);
        assert_eq!(h.0, 3);
        assert_eq!(h.1, 42);
        assert_ne!(h, StateHandle::new(3, 43));
        assert_eq!(StateHandle::from(0), StateHandle::new(0, 0));

        let json = serde_json::to_string(&h).unwrap();
        let decoded: StateHandle = serde_json::from_str(&json).unwrap();
        assert_eq!(h, decoded);
    }

    #[test]
    fn test_aligned_vec_wraps_vec() {
        let mut v: AlignedVec<u32> = AlignedVec::from_vec(vec![1, 2, 3]);
        v.push(4);
        assert_eq!(v.len(), 4);
        assert_eq!(v.as_slice(), &[1, 2, 3, 4]);
        assert_eq!(v[0], 1);
        v.as_mut_slice()[0] = 9;
        assert_eq!(v[0], 9);
        assert_eq!(v.into_vec(), vec![9, 2, 3, 4]);

        let empty: AlignedVec<u16> = AlignedVec::new();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_aligned_vec_serde_roundtrip() {
        let v: AlignedVec<u64> = vec![1, 2, 3].into();
        let json = serde_json::to_string(&v).unwrap();
        let decoded: AlignedVec<u64> = serde_json::from_str(&json).unwrap();
        assert_eq!(v, decoded);
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
