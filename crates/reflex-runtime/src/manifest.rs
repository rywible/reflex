//! Immutable, identity-bearing inputs for one cell execution.

use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use reflex_types::{
    CellId, Digest, DigestAlgorithm, DomainId, KnowledgeEditionId, ModelCheckpointId, TaskId,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use thiserror::Error;

pub const CELL_EXECUTION_MANIFEST_SCHEMA: &str = "reflex.cell-execution.v1";
pub const CELL_EXECUTION_MANIFEST_VERSION: u32 = 1;
pub const MAX_CELL_EXECUTION_MANIFEST_BYTES: usize = 64 * 1024;
const MIN_CELL_MEMORY_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchExecutionConfig {
    algorithm: String,
    action_budget: u64,
    node_budget: u64,
    cpu_budget_ns: u64,
}

impl SearchExecutionConfig {
    pub fn new(
        algorithm: impl Into<String>,
        action_budget: u64,
        node_budget: u64,
        cpu_budget_ns: u64,
    ) -> Self {
        Self {
            algorithm: algorithm.into(),
            action_budget,
            node_budget,
            cpu_budget_ns,
        }
    }

    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    pub fn action_budget(&self) -> u64 {
        self.action_budget
    }

    pub fn node_budget(&self) -> u64 {
        self.node_budget
    }

    pub fn cpu_budget_ns(&self) -> u64 {
        self.cpu_budget_ns
    }

    fn validate(&self) -> Result<(), ManifestValidationError> {
        validate_stable_name("search algorithm", &self.algorithm)?;
        if self.action_budget == 0 || self.node_budget == 0 || self.cpu_budget_ns == 0 {
            return Err(ManifestValidationError::InvalidSearchBudget);
        }
        Ok(())
    }
}

impl CanonicalEncode for SearchExecutionConfig {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.algorithm)?;
        out.write_u64(self.action_budget)?;
        out.write_u64(self.node_budget)?;
        out.write_u64(self.cpu_budget_ns)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceAllocation {
    class: String,
    cpu_permits: u32,
    memory_bytes: u64,
    scratch_bytes: u64,
}

impl ResourceAllocation {
    pub fn new(
        class: impl Into<String>,
        cpu_permits: u32,
        memory_bytes: u64,
        scratch_bytes: u64,
    ) -> Self {
        Self {
            class: class.into(),
            cpu_permits,
            memory_bytes,
            scratch_bytes,
        }
    }

    pub fn class(&self) -> &str {
        &self.class
    }

    pub fn cpu_permits(&self) -> u32 {
        self.cpu_permits
    }

    pub fn memory_bytes(&self) -> u64 {
        self.memory_bytes
    }

    pub fn scratch_bytes(&self) -> u64 {
        self.scratch_bytes
    }

    fn validate(&self) -> Result<(), ManifestValidationError> {
        validate_stable_name("resource class", &self.class)?;
        if self.cpu_permits == 0 || self.memory_bytes < MIN_CELL_MEMORY_BYTES {
            return Err(ManifestValidationError::InvalidResourceAllocation);
        }
        Ok(())
    }
}

impl CanonicalEncode for ResourceAllocation {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.class)?;
        out.write_u32(self.cpu_permits)?;
        out.write_u64(self.memory_bytes)?;
        out.write_u64(self.scratch_bytes)
    }
}

/// Complete semantic input set consumed by a worker before cell execution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CellExecutionManifestWire")]
pub struct CellExecutionManifest {
    schema_version: u32,
    /// Scheduler-assigned identity derived from the scheduler's canonical
    /// `CellManifest`. This is an input to execution; it is intentionally not
    /// derived from this execution manifest's own digest.
    cell_id: CellId,
    task_id: TaskId,
    domain_id: DomainId,
    search: SearchExecutionConfig,
    seed: u64,
    resource: ResourceAllocation,
    model_checkpoint: Option<ModelCheckpointId>,
    knowledge_edition: Option<KnowledgeEditionId>,
    evidence_inputs: BTreeMap<String, Digest>,
}

/// Serde-only shape. Conversion validates before an untrusted decoded value can
/// become a `CellExecutionManifest`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CellExecutionManifestWire {
    schema_version: u32,
    cell_id: CellId,
    task_id: TaskId,
    domain_id: DomainId,
    search: SearchExecutionConfig,
    seed: u64,
    resource: ResourceAllocation,
    model_checkpoint: Option<ModelCheckpointId>,
    knowledge_edition: Option<KnowledgeEditionId>,
    evidence_inputs: BTreeMap<String, Digest>,
}

impl TryFrom<CellExecutionManifestWire> for CellExecutionManifest {
    type Error = ManifestValidationError;

    fn try_from(wire: CellExecutionManifestWire) -> Result<Self, Self::Error> {
        let manifest = Self {
            schema_version: wire.schema_version,
            cell_id: wire.cell_id,
            task_id: wire.task_id,
            domain_id: wire.domain_id,
            search: wire.search,
            seed: wire.seed,
            resource: wire.resource,
            model_checkpoint: wire.model_checkpoint,
            knowledge_edition: wire.knowledge_edition,
            evidence_inputs: wire.evidence_inputs,
        };
        manifest.validate()?;
        Ok(manifest)
    }
}

impl CellExecutionManifest {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cell_id: CellId,
        task_id: TaskId,
        domain_id: DomainId,
        search: SearchExecutionConfig,
        seed: u64,
        resource: ResourceAllocation,
        model_checkpoint: Option<ModelCheckpointId>,
        knowledge_edition: Option<KnowledgeEditionId>,
        evidence_inputs: BTreeMap<String, Digest>,
    ) -> Result<Self, ManifestValidationError> {
        let manifest = Self {
            schema_version: CELL_EXECUTION_MANIFEST_VERSION,
            cell_id,
            task_id,
            domain_id,
            search,
            seed,
            resource,
            model_checkpoint,
            knowledge_edition,
            evidence_inputs,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestValidationError> {
        if self.schema_version != CELL_EXECUTION_MANIFEST_VERSION {
            return Err(ManifestValidationError::UnsupportedVersion(
                self.schema_version,
            ));
        }
        validate_nonzero_digest("cell ID", self.cell_id.digest())?;
        validate_nonzero_digest("task ID", self.task_id.digest())?;
        validate_nonzero_digest("domain ID", self.domain_id.digest())?;
        self.search.validate()?;
        self.resource.validate()?;
        if let Some(model) = self.model_checkpoint {
            validate_nonzero_digest("model checkpoint", model.digest())?;
        }
        if let Some(knowledge) = self.knowledge_edition {
            validate_nonzero_digest("knowledge edition", knowledge.digest())?;
        }
        for (role, digest) in &self.evidence_inputs {
            validate_stable_name("evidence input role", role)?;
            validate_nonzero_digest(format!("evidence input {role:?}"), digest)?;
        }
        Ok(())
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>, ManifestSerializationError> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)?;
        if bytes.len() > MAX_CELL_EXECUTION_MANIFEST_BYTES {
            return Err(ManifestSerializationError::TooLarge(bytes.len()));
        }
        Ok(bytes)
    }

    pub fn digest(&self) -> Result<Digest, CanonicalError> {
        reflex_canonical::content_id(CELL_EXECUTION_MANIFEST_SCHEMA.as_bytes(), self)
    }

    pub fn cell_id(&self) -> CellId {
        self.cell_id
    }

    pub fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub fn domain_id(&self) -> DomainId {
        self.domain_id
    }

    pub fn search(&self) -> &SearchExecutionConfig {
        &self.search
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }

    pub fn resource(&self) -> &ResourceAllocation {
        &self.resource
    }

    pub fn model_checkpoint(&self) -> Option<ModelCheckpointId> {
        self.model_checkpoint
    }

    pub fn knowledge_edition(&self) -> Option<KnowledgeEditionId> {
        self.knowledge_edition
    }

    pub fn evidence_inputs(&self) -> &BTreeMap<String, Digest> {
        &self.evidence_inputs
    }
}

impl CanonicalEncode for CellExecutionManifest {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u32(self.schema_version)?;
        out.write_digest(self.cell_id.digest())?;
        out.write_digest(self.task_id.digest())?;
        out.write_digest(self.domain_id.digest())?;
        self.search.encode_canonical(out)?;
        out.write_u64(self.seed)?;
        self.resource.encode_canonical(out)?;
        out.write_option(self.model_checkpoint.as_ref().map(|id| id.digest()))?;
        out.write_option(self.knowledge_edition.as_ref().map(|id| id.digest()))?;
        out.write_map(&self.evidence_inputs)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ManifestValidationError {
    #[error("unsupported cell execution manifest version {0}")]
    UnsupportedVersion(u32),
    #[error("{field} must be a non-empty stable name")]
    InvalidName { field: &'static str },
    #[error("{field} contains unresolved mutable tag {value:?}")]
    MutableTag { field: &'static str, value: String },
    #[error("search budgets must all be non-zero")]
    InvalidSearchBudget,
    #[error("resource allocation requires CPU permits and at least 2 MiB memory")]
    InvalidResourceAllocation,
    #[error("{field} must not use the all-zero digest sentinel")]
    ZeroDigest { field: String },
}

#[derive(Debug, Error)]
pub enum ManifestSerializationError {
    #[error(transparent)]
    Validation(#[from] ManifestValidationError),
    #[error("cell execution manifest is {0} bytes; maximum is 65536")]
    TooLarge(usize),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Error)]
pub enum ManifestVerificationError {
    #[error("cell execution manifest is {0} bytes; maximum is 65536")]
    TooLarge(usize),
    #[error("cell execution manifest is not its canonical serde encoding")]
    NonCanonicalEncoding,
    #[error("cell execution manifests require a BLAKE3 identity, got {0:?}")]
    UnsupportedDigestAlgorithm(DigestAlgorithm),
    #[error("cell manifest digest mismatch: expected {expected}, observed {observed}")]
    DigestMismatch { expected: Digest, observed: Digest },
    #[error(transparent)]
    Validation(#[from] ManifestValidationError),
    #[error(transparent)]
    Canonical(#[from] CanonicalError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// Parsed, canonical, digest-verified manifest capability (INV-RFX-2/21).
#[derive(Clone, Debug)]
pub struct VerifiedCellManifest {
    digest: Digest,
    manifest: Arc<CellExecutionManifest>,
    bytes: Arc<[u8]>,
}

impl VerifiedCellManifest {
    pub fn verify(bytes: &[u8], expected: Digest) -> Result<Self, ManifestVerificationError> {
        if bytes.len() > MAX_CELL_EXECUTION_MANIFEST_BYTES {
            return Err(ManifestVerificationError::TooLarge(bytes.len()));
        }
        if expected.algorithm != DigestAlgorithm::Blake3 {
            return Err(ManifestVerificationError::UnsupportedDigestAlgorithm(
                expected.algorithm,
            ));
        }
        let manifest: CellExecutionManifest = serde_json::from_slice(bytes)?;
        manifest.validate()?;
        let canonical_bytes = manifest.to_bytes().map_err(|error| match error {
            ManifestSerializationError::Validation(error) => error.into(),
            ManifestSerializationError::TooLarge(length) => {
                ManifestVerificationError::TooLarge(length)
            }
            ManifestSerializationError::Json(error) => error.into(),
        })?;
        if canonical_bytes != bytes {
            return Err(ManifestVerificationError::NonCanonicalEncoding);
        }
        let observed = manifest.digest()?;
        if observed != expected {
            return Err(ManifestVerificationError::DigestMismatch { expected, observed });
        }
        Ok(Self {
            digest: expected,
            manifest: Arc::new(manifest),
            bytes: Arc::from(bytes),
        })
    }

    pub fn digest(&self) -> Digest {
        self.digest
    }

    pub fn manifest(&self) -> &CellExecutionManifest {
        &self.manifest
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

fn validate_stable_name(field: &'static str, value: &str) -> Result<(), ManifestValidationError> {
    if value.is_empty() || value.len() > 256 || value.trim() != value {
        return Err(ManifestValidationError::InvalidName { field });
    }
    let lower = value.to_ascii_lowercase();
    let mutable = lower == "latest"
        || lower == "head"
        || lower.ends_with(":latest")
        || lower.ends_with("@latest")
        || lower.ends_with("/latest");
    if mutable {
        return Err(ManifestValidationError::MutableTag {
            field,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_nonzero_digest(
    field: impl Into<String>,
    digest: &Digest,
) -> Result<(), ManifestValidationError> {
    if digest.bytes == [0; 32] {
        return Err(ManifestValidationError::ZeroDigest {
            field: field.into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> CellExecutionManifest {
        CellExecutionManifest::new(
            CellId::from_digest(Digest::hash_blake3(b"cell")),
            TaskId::from_digest(Digest::hash_blake3(b"task")),
            DomainId::from_digest(Digest::hash_blake3(b"domain")),
            SearchExecutionConfig::new("best-first", 100, 1_000, 5_000_000),
            42,
            ResourceAllocation::new("reference-4vcpu-8gb", 4, 8 * 1024 * 1024, 1024),
            Some(ModelCheckpointId::from_digest(Digest::hash_blake3(
                b"model",
            ))),
            Some(KnowledgeEditionId::from_digest(Digest::hash_blake3(
                b"knowledge",
            ))),
            BTreeMap::from([
                ("corpus".to_string(), Digest::hash_blake3(b"corpus")),
                ("verifier".to_string(), Digest::hash_blake3(b"verifier")),
            ]),
        )
        .unwrap()
    }

    #[test]
    fn canonical_serde_roundtrip_retains_typed_manifest() {
        let manifest = manifest();
        let bytes = manifest.to_bytes().unwrap();
        let verified = VerifiedCellManifest::verify(&bytes, manifest.digest().unwrap()).unwrap();
        assert_eq!(verified.manifest(), &manifest);
        assert_eq!(verified.bytes(), bytes);
    }

    #[test]
    fn semantic_changes_change_identity() {
        let base = manifest();
        let mut variants = Vec::new();
        let mut changed = base.clone();
        changed.seed += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.search.node_budget += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.resource.memory_bytes += 1;
        variants.push(changed);
        let mut changed = base.clone();
        changed.model_checkpoint = None;
        variants.push(changed);
        let mut changed = base.clone();
        changed.knowledge_edition = None;
        variants.push(changed);
        let mut changed = base.clone();
        changed
            .evidence_inputs
            .insert("corpus".to_string(), Digest::hash_blake3(b"other"));
        variants.push(changed);

        let base_digest = base.digest().unwrap();
        assert!(
            variants
                .iter()
                .all(|variant| variant.digest().unwrap() != base_digest)
        );
    }

    #[test]
    fn mutated_and_noncanonical_bytes_fail_closed() {
        let manifest = manifest();
        let expected = manifest.digest().unwrap();
        let mut semantic_mutation = manifest.clone();
        semantic_mutation.seed += 1;
        assert!(matches!(
            VerifiedCellManifest::verify(&semantic_mutation.to_bytes().unwrap(), expected),
            Err(ManifestVerificationError::DigestMismatch { .. })
        ));

        let mut noncanonical = manifest.to_bytes().unwrap();
        noncanonical.push(b'\n');
        assert!(matches!(
            VerifiedCellManifest::verify(&noncanonical, expected),
            Err(ManifestVerificationError::NonCanonicalEncoding)
        ));
    }

    #[test]
    fn unresolved_mutable_tags_are_rejected() {
        let error = CellExecutionManifest::new(
            CellId::from_digest(Digest::hash_blake3(b"cell")),
            TaskId::from_digest(Digest::hash_blake3(b"task")),
            DomainId::from_digest(Digest::hash_blake3(b"domain")),
            SearchExecutionConfig::new("policy:latest", 1, 1, 1),
            0,
            ResourceAllocation::new("local", 1, MIN_CELL_MEMORY_BYTES, 0),
            None,
            None,
            BTreeMap::new(),
        )
        .unwrap_err();
        assert!(matches!(error, ManifestValidationError::MutableTag { .. }));
    }

    #[test]
    fn every_identity_field_rejects_zero_digest() {
        let base = manifest();
        let mut variants = Vec::new();
        let mut changed = base.clone();
        changed.cell_id = CellId::from_digest(Digest::ZERO);
        variants.push(changed);
        let mut changed = base.clone();
        changed.task_id = TaskId::from_digest(Digest::ZERO);
        variants.push(changed);
        let mut changed = base.clone();
        changed.domain_id = DomainId::from_digest(Digest::ZERO);
        variants.push(changed);
        let mut changed = base.clone();
        changed.model_checkpoint = Some(ModelCheckpointId::from_digest(Digest::ZERO));
        variants.push(changed);
        let mut changed = base.clone();
        changed.knowledge_edition = Some(KnowledgeEditionId::from_digest(Digest::ZERO));
        variants.push(changed);
        let mut changed = base;
        changed
            .evidence_inputs
            .insert("corpus".to_string(), Digest::ZERO);
        variants.push(changed);

        for variant in variants {
            assert!(matches!(
                variant.validate(),
                Err(ManifestValidationError::ZeroDigest { .. })
            ));
            let unchecked_json = serde_json::to_vec(&variant).unwrap();
            assert!(serde_json::from_slice::<CellExecutionManifest>(&unchecked_json).is_err());
        }
    }
}
