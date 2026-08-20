use arc_swap::ArcSwap;
use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use reflex_domain::FeatureBatch;
use reflex_types::{Digest, ModelCheckpointId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

pub use reflex_types::{
    Activation, BackendClass, FeatureSchemaId, ModelArchitecture, ModelDType, ModelOutputSchemaId,
    ModelRole, ModelSpec, ParameterBudget,
};

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum MlError {
    #[error("model dimension mismatch: expected input {expected}, found {found}")]
    DimensionMismatch { expected: usize, found: usize },
    #[error("non-finite model score or weight")]
    NonFiniteValue,
    #[error("unsupported model architecture: {0}")]
    UnsupportedArchitecture(String),
    #[error("checkpoint verification failed: {0}")]
    CheckpointVerification(String),
    #[error("schema compatibility mismatch")]
    SchemaMismatch,
    #[error("role mismatch: expected {expected:?}, found {found:?}")]
    RoleMismatch {
        expected: ModelRole,
        found: ModelRole,
    },
}

/// M1.5 reference parameter budgets for rankers with 64-dim inputs.
pub const M15_PARAMETER_BUDGETS: [usize; 6] = [519, 1026, 2607, 9614, 29_538, 99_902];

pub fn m15_hidden_layers(budget: usize, input_dim: usize) -> Option<Vec<usize>> {
    if input_dim != 64 {
        return None;
    }
    match budget {
        519 => Some(vec![7, 7]),
        1026 => Some(vec![13, 12]),
        2607 => Some(vec![22, 49]),
        9614 => Some(vec![5, 48, 180]),
        29_538 => Some(vec![35, 156, 137]),
        99_902 => Some(vec![50, 51, 261, 306]),
        _ => None,
    }
}

pub fn parameter_count(
    input_dim: usize,
    output_dim: usize,
    arch: &ModelArchitecture,
) -> Result<usize, MlError> {
    let layer = |input: usize, output: usize, bias: bool| {
        input
            .checked_mul(output)
            .and_then(|count| count.checked_add(if bias { output } else { 0 }))
            .ok_or_else(|| MlError::UnsupportedArchitecture("parameter count overflow".into()))
    };
    let count = match arch {
        ModelArchitecture::Linear => layer(input_dim, output_dim, true)?,
        ModelArchitecture::Mlp { hidden, bias, .. } => {
            let mut total: usize = 0;
            let mut prev = input_dim;
            for &h in hidden {
                total = total.checked_add(layer(prev, h, *bias)?).ok_or_else(|| {
                    MlError::UnsupportedArchitecture("parameter count overflow".into())
                })?;
                prev = h;
            }
            total = total
                .checked_add(layer(prev, output_dim, *bias)?)
                .ok_or_else(|| {
                    MlError::UnsupportedArchitecture("parameter count overflow".into())
                })?;
            total
        }
        ModelArchitecture::BottleneckMlp {
            bottleneck, hidden, ..
        } => layer(input_dim, *bottleneck, true)?
            .checked_add(layer(*bottleneck, *hidden, true)?)
            .and_then(|count| count.checked_add(layer(*hidden, output_dim, true).ok()?))
            .ok_or_else(|| MlError::UnsupportedArchitecture("parameter count overflow".into()))?,
        ModelArchitecture::BurnCustom { config, .. } => {
            if config.0.len() >= 8 {
                usize::try_from(u64::from_le_bytes(config.0[..8].try_into().map_err(
                    |_| MlError::UnsupportedArchitecture("invalid factory config".into()),
                )?))
                .map_err(|_| {
                    MlError::UnsupportedArchitecture("custom parameter count exceeds usize".into())
                })?
            } else {
                return Err(MlError::UnsupportedArchitecture(
                    "custom factory config does not declare an exact parameter count".into(),
                ));
            }
        }
    };
    Ok(count)
}

struct ModelSpecIdentity<'a>(&'a ModelSpec, usize);

impl CanonicalEncode for ModelSpecIdentity<'_> {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u64(self.1 as u64)?;
        out.write_u8(match self.0.role {
            ModelRole::Ranker => 0,
            ModelRole::TasteCritic => 1,
            ModelRole::Proposal => 2,
        })?;
        match &self.0.architecture {
            ModelArchitecture::Linear => out.write_u8(0)?,
            ModelArchitecture::Mlp {
                hidden,
                activation,
                bias,
            } => {
                out.write_u8(1)?;
                out.write_vec(&hidden.iter().map(|&value| value as u64).collect::<Vec<_>>())?;
                out.write_u8(activation_tag(activation))?;
                out.write_bool(*bias)?;
            }
            ModelArchitecture::BottleneckMlp {
                bottleneck,
                hidden,
                activation,
            } => {
                out.write_u8(2)?;
                out.write_u64(*bottleneck as u64)?;
                out.write_u64(*hidden as u64)?;
                out.write_u8(activation_tag(activation))?;
            }
            ModelArchitecture::BurnCustom { factory, config } => {
                out.write_u8(3)?;
                out.write_str(&factory.0)?;
                out.write_byte_slice(&config.0)?;
            }
        }
        out.write_digest(self.0.input_schema.digest())?;
        out.write_digest(self.0.output_schema.digest())?;
        out.write_u64(self.0.parameter_budget.max_parameters)?;
        match self.0.parameter_budget.max_unique_parameters {
            Some(value) => {
                out.write_u8(1)?;
                out.write_u64(value)?;
            }
            None => out.write_u8(0)?,
        }
        out.write_u8(match self.0.dtype {
            ModelDType::F32 => 0,
            ModelDType::F16 => 1,
        })?;
        out.write_u8(backend_tag(&self.0.canonical_backend))
    }
}

fn activation_tag(activation: &Activation) -> u8 {
    match activation {
        Activation::ReLU => 0,
        Activation::Gelu => 1,
        Activation::Silu => 2,
    }
}

fn backend_tag(backend: &BackendClass) -> u8 {
    match backend {
        BackendClass::Flex => 0,
        BackendClass::CubeCL => 1,
        BackendClass::Cuda => 2,
        BackendClass::Metal => 3,
    }
}

pub fn model_spec_compatibility_digest(
    spec: &ModelSpec,
    input_dim: usize,
) -> Result<Digest, MlError> {
    reflex_canonical::content_id(b"reflex.model.spec.v2", &ModelSpecIdentity(spec, input_dim))
        .map_err(|_| MlError::SchemaMismatch)
}

/// Feature schema identity: column order, dtype, normalization, missing-value rules (P6.2).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FeatureSchemaDocument {
    pub schema_id: FeatureSchemaId,
    pub column_order: Vec<String>,
    pub dtype: ModelDType,
    pub normalization: String,
    pub missing_value_rule: String,
}

struct FeatureSchemaIdentity<'a>(&'a FeatureSchemaDocument);

impl CanonicalEncode for FeatureSchemaIdentity<'_> {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        self.0.column_order.encode_canonical(out)?;
        out.write_u8(match self.0.dtype {
            ModelDType::F32 => 0,
            ModelDType::F16 => 1,
        })?;
        self.0.normalization.encode_canonical(out)?;
        self.0.missing_value_rule.encode_canonical(out)
    }
}

pub fn feature_schema_compatibility_digest(doc: &FeatureSchemaDocument) -> Result<Digest, MlError> {
    if doc.column_order.is_empty()
        || doc.column_order.iter().any(String::is_empty)
        || doc.normalization.is_empty()
        || doc.missing_value_rule.is_empty()
    {
        return Err(MlError::SchemaMismatch);
    }
    reflex_canonical::content_id(b"reflex.feature_schema.v2", &FeatureSchemaIdentity(doc))
        .map_err(|_| MlError::SchemaMismatch)
}

pub fn validate_feature_schema_document(doc: &FeatureSchemaDocument) -> Result<(), MlError> {
    let computed = feature_schema_compatibility_digest(doc)?;
    if doc.schema_id.digest() != &computed {
        return Err(MlError::SchemaMismatch);
    }
    Ok(())
}

pub fn validate_ranker_spec(spec: &ModelSpec, input_dim: usize) -> Result<(), MlError> {
    if spec.role != ModelRole::Ranker {
        return Err(MlError::RoleMismatch {
            expected: ModelRole::Ranker,
            found: spec.role.clone(),
        });
    }
    let exact = parameter_count(input_dim, 1, &spec.architecture)?;
    if exact as u64 > spec.parameter_budget.max_parameters {
        return Err(MlError::UnsupportedArchitecture(format!(
            "exact parameter count {exact} exceeds budget {}",
            spec.parameter_budget.max_parameters
        )));
    }
    Ok(())
}

pub fn validate_checkpoint_load(
    spec: &ModelSpec,
    input_dim: usize,
    output_dim: usize,
    exact_count: usize,
) -> Result<(), MlError> {
    validate_ranker_spec(spec, input_dim)?;
    let expected = parameter_count(input_dim, output_dim, &spec.architecture)?;
    if expected != exact_count {
        return Err(MlError::CheckpointVerification(format!(
            "parameter count mismatch: spec expects {expected}, checkpoint has {exact_count}"
        )));
    }
    Ok(())
}

#[derive(Clone, Debug, Default)]
pub struct InferenceTelemetry {
    pub feature_cpu_ns: u64,
    pub queue_cpu_ns: u64,
    pub forward_cpu_ns: u64,
    pub postprocess_cpu_ns: u64,
    pub rows_scored: usize,
}

pub trait Ranker: Send + Sync {
    fn model_id(&self) -> ModelCheckpointId;
    fn score_batch(
        &self,
        features: &FeatureBatch,
        output: &mut [f32],
        telemetry: &mut InferenceTelemetry,
    ) -> Result<(), MlError>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TasteHead {
    ImmediateUtility,
    LongHorizonUtility,
    DescendantValue,
    CrossDomainReuse,
    Compression,
    OptionValue,
    DiscoveryCost,
    EpistemicUncertainty,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalibrationMetadata {
    pub dataset_digest: Digest,
    pub metric_name: String,
    pub calibration_version: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TasteHeadRegistry {
    pub enabled_heads: Vec<TasteHead>,
    pub units: std::collections::BTreeMap<String, String>,
    pub calibration: CalibrationMetadata,
    pub missing_head_policy: String,
}

impl Default for TasteHeadRegistry {
    fn default() -> Self {
        Self {
            enabled_heads: vec![
                TasteHead::ImmediateUtility,
                TasteHead::LongHorizonUtility,
                TasteHead::DescendantValue,
                TasteHead::OptionValue,
                TasteHead::EpistemicUncertainty,
            ],
            units: std::collections::BTreeMap::from([
                ("immediate_utility".to_string(), "normalized_q".to_string()),
                (
                    "long_horizon_utility".to_string(),
                    "normalized_q".to_string(),
                ),
            ]),
            calibration: CalibrationMetadata {
                dataset_digest: Digest::ZERO,
                metric_name: "isotonic_v1".to_string(),
                calibration_version: 1,
            },
            missing_head_policy: "censor".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TasteEstimate {
    pub immediate_utility: Option<f32>,
    pub long_horizon_utility: Option<f32>,
    pub descendant_value: Option<f32>,
    pub cross_domain_reuse: Option<f32>,
    pub compression: Option<f32>,
    pub option_value: Option<f32>,
    pub discovery_cost: Option<f32>,
    pub uncertainty: Option<f32>,
    pub registry: TasteHeadRegistry,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposalProvenance {
    pub model_id: ModelCheckpointId,
    pub generation_step: u64,
    pub seed: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposalBatch {
    pub candidate_payloads: Vec<Vec<u8>>,
    pub prior_scores: Vec<f32>,
    pub provenance: ProposalProvenance,
    pub metadata: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposalGate {
    pub learned_proposals_enabled: bool,
    pub max_payload_bytes: usize,
    pub max_batch_count: usize,
    pub invalid_proposal_count: u64,
    pub invalid_proposal_cost_ns: u64,
}

impl Default for ProposalGate {
    fn default() -> Self {
        Self {
            learned_proposals_enabled: false,
            max_payload_bytes: 4096,
            max_batch_count: 64,
            invalid_proposal_count: 0,
            invalid_proposal_cost_ns: 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelLifecycleState {
    Stable,
    Candidate,
    Experimental,
    Rejected,
    Retired,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShadowScoringConfig {
    pub enabled: bool,
    pub cpu_budget_pct: f32,
    pub checkpoint_id: Option<ModelCheckpointId>,
}

impl Default for ShadowScoringConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cpu_budget_pct: 5.0,
            checkpoint_id: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelStateRecord {
    pub checkpoint_id: ModelCheckpointId,
    pub lifecycle: ModelLifecycleState,
    pub spec_digest: Digest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelCheckpointManifest {
    pub model_id: ModelCheckpointId,
    pub spec: ModelSpec,
    pub input_dim: usize,
    pub exact_parameter_count: usize,
    pub weights_digest: Digest,
    pub optimizer_digest: Option<Digest>,
    pub rng_digest: Option<Digest>,
    pub normalization_digest: Option<Digest>,
    pub dataset_digest: Option<Digest>,
    pub sampler_digest: Option<Digest>,
    pub code_identity: Option<Digest>,
    pub provenance_digest: Option<Digest>,
    pub backend: BackendClass,
    pub device: String,
    pub step: u64,
    pub epoch: u64,
    pub batch_position: u64,
    pub metrics_digest: Digest,
    pub inference_only: bool,
}

impl ModelCheckpointManifest {
    pub fn classify_inference_only(&mut self) {
        self.inference_only = self.optimizer_digest.is_none() || self.rng_digest.is_none();
    }

    pub fn checkpoint_identity(&self) -> Result<ModelCheckpointId, MlError> {
        let spec = model_spec_compatibility_digest(&self.spec, self.input_dim)?;
        let mut payload = Vec::with_capacity(320);
        payload.extend_from_slice(b"reflex.model.checkpoint.v2\0");
        payload.extend_from_slice(spec.as_bytes());
        payload.extend_from_slice(&(self.exact_parameter_count as u64).to_le_bytes());
        payload.extend_from_slice(self.weights_digest.as_bytes());
        append_optional_digest(&mut payload, self.optimizer_digest);
        append_optional_digest(&mut payload, self.rng_digest);
        append_optional_digest(&mut payload, self.normalization_digest);
        append_optional_digest(&mut payload, self.dataset_digest);
        append_optional_digest(&mut payload, self.sampler_digest);
        append_optional_digest(&mut payload, self.code_identity);
        append_optional_digest(&mut payload, self.provenance_digest);
        payload.push(backend_tag(&self.backend));
        payload.extend_from_slice(&(self.device.len() as u64).to_le_bytes());
        payload.extend_from_slice(self.device.as_bytes());
        payload.extend_from_slice(&self.step.to_le_bytes());
        payload.extend_from_slice(&self.epoch.to_le_bytes());
        payload.extend_from_slice(&self.batch_position.to_le_bytes());
        payload.extend_from_slice(self.metrics_digest.as_bytes());
        Ok(ModelCheckpointId::from_digest(Digest::hash_blake3(
            &payload,
        )))
    }

    pub fn seal(&mut self) -> Result<(), MlError> {
        self.classify_inference_only();
        self.model_id = self.checkpoint_identity()?;
        Ok(())
    }

    pub fn validate(&self) -> Result<(), MlError> {
        validate_checkpoint_load(&self.spec, self.input_dim, 1, self.exact_parameter_count)?;
        if self.backend != self.spec.canonical_backend {
            return Err(MlError::CheckpointVerification(
                "manifest backend differs from model spec".into(),
            ));
        }
        if self.device.trim().is_empty() {
            return Err(MlError::CheckpointVerification(
                "checkpoint device identity is empty".into(),
            ));
        }
        if self.weights_digest == Digest::ZERO || self.metrics_digest == Digest::ZERO {
            return Err(MlError::CheckpointVerification(
                "checkpoint has zero weights or metrics digest".into(),
            ));
        }
        if [
            self.optimizer_digest,
            self.rng_digest,
            self.normalization_digest,
            self.dataset_digest,
            self.sampler_digest,
            self.code_identity,
            self.provenance_digest,
        ]
        .into_iter()
        .flatten()
        .any(|digest| digest == Digest::ZERO)
        {
            return Err(MlError::CheckpointVerification(
                "checkpoint contains a present-but-zero artifact digest".into(),
            ));
        }
        let classified = self.optimizer_digest.is_none() || self.rng_digest.is_none();
        if classified != self.inference_only {
            return Err(MlError::CheckpointVerification(
                "inference-only classification does not match optimizer/RNG state".into(),
            ));
        }
        if self.checkpoint_identity()? != self.model_id {
            return Err(MlError::CheckpointVerification(
                "checkpoint manifest identity mismatch".into(),
            ));
        }
        Ok(())
    }

    pub fn require_resumable(&self) -> Result<(), MlError> {
        self.validate()?;
        if self.inference_only {
            return Err(MlError::CheckpointVerification(
                "checkpoint is inference-only; optimizer and RNG state are required".into(),
            ));
        }
        for (name, digest) in [
            ("dataset", self.dataset_digest),
            ("sampler", self.sampler_digest),
            ("code", self.code_identity),
            ("provenance", self.provenance_digest),
        ] {
            if digest.is_none() || digest == Some(Digest::ZERO) {
                return Err(MlError::CheckpointVerification(format!(
                    "resumable checkpoint is missing {name} identity"
                )));
            }
        }
        Ok(())
    }
}

fn append_optional_digest(payload: &mut Vec<u8>, digest: Option<Digest>) {
    match digest {
        Some(digest) => {
            payload.push(1);
            payload.extend_from_slice(digest.as_bytes());
        }
        None => payload.push(0),
    }
}

pub struct ModelBundle {
    pub checkpoint_id: ModelCheckpointId,
    pub spec_digest: Digest,
    pub ranker: Arc<dyn Ranker>,
    pub shadow: Option<Arc<dyn Ranker>>,
    pub shadow_budget: ShadowScoringConfig,
}

pub struct ModelRegistry {
    active: ArcSwap<ModelBundle>,
    state: std::sync::Mutex<ModelRegistryState>,
}

struct ModelRegistryState {
    records: Vec<ModelStateRecord>,
    bundles: std::collections::HashMap<ModelCheckpointId, Arc<ModelBundle>>,
    audit: Vec<ModelTransitionAudit>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelTransitionAudit {
    pub sequence: u64,
    pub from: ModelCheckpointId,
    pub to: ModelCheckpointId,
    pub action: String,
}

impl ModelRegistry {
    pub fn new(bundle: ModelBundle) -> Self {
        assert_eq!(
            bundle.checkpoint_id,
            bundle.ranker.model_id(),
            "model bundle identity must match ranker identity"
        );
        assert_ne!(
            bundle.spec_digest,
            Digest::ZERO,
            "model bundle spec identity must be non-zero"
        );
        let checkpoint_id = bundle.checkpoint_id;
        let spec_digest = bundle.spec_digest;
        let bundle = Arc::new(bundle);
        Self {
            active: ArcSwap::from(bundle.clone()),
            state: std::sync::Mutex::new(ModelRegistryState {
                records: vec![ModelStateRecord {
                    checkpoint_id,
                    lifecycle: ModelLifecycleState::Stable,
                    spec_digest,
                }],
                bundles: std::collections::HashMap::from([(checkpoint_id, bundle)]),
                audit: Vec::new(),
            }),
        }
    }

    pub fn pin(&self) -> Arc<ModelBundle> {
        self.active.load_full()
    }

    pub fn promote_between_generations(
        &self,
        next: Arc<ModelBundle>,
        record: ModelStateRecord,
    ) -> Result<(), MlError> {
        if next.checkpoint_id != next.ranker.model_id()
            || record.checkpoint_id != next.checkpoint_id
            || record.spec_digest != next.spec_digest
            || record.lifecycle != ModelLifecycleState::Stable
            || record.spec_digest == Digest::ZERO
        {
            return Err(MlError::CheckpointVerification(
                "invalid promotion bundle or state record".into(),
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| MlError::CheckpointVerification("model registry lock poisoned".into()))?;
        let current = self.active.load_full();
        if current.spec_digest != next.spec_digest {
            return Err(MlError::SchemaMismatch);
        }
        state.bundles.insert(next.checkpoint_id, next.clone());
        state.records.push(record);
        let sequence = state.audit.len() as u64;
        state.audit.push(ModelTransitionAudit {
            sequence,
            from: current.checkpoint_id,
            to: next.checkpoint_id,
            action: "promote".to_string(),
        });
        self.active.store(next);
        Ok(())
    }

    pub fn rollback_to(&self, checkpoint_id: ModelCheckpointId) -> Result<(), MlError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| MlError::CheckpointVerification("model registry lock poisoned".into()))?;
        let current = self.active.load_full();
        let bundle = state.bundles.get(&checkpoint_id).cloned().ok_or_else(|| {
            MlError::CheckpointVerification("rollback checkpoint is not retained".into())
        })?;
        if bundle.spec_digest != current.spec_digest {
            return Err(MlError::SchemaMismatch);
        }
        state.records.push(ModelStateRecord {
            checkpoint_id,
            lifecycle: ModelLifecycleState::Stable,
            spec_digest: bundle.spec_digest,
        });
        let sequence = state.audit.len() as u64;
        state.audit.push(ModelTransitionAudit {
            sequence,
            from: current.checkpoint_id,
            to: checkpoint_id,
            action: "rollback".to_string(),
        });
        self.active.store(bundle);
        Ok(())
    }

    /// Record the lifecycle of an immutable checkpoint without making it active.
    /// Stable transitions must use promotion or rollback so the pointer swap and
    /// audit entry remain one serialized operation.
    pub fn record_checkpoint_state(&self, record: ModelStateRecord) -> Result<(), MlError> {
        if record.lifecycle == ModelLifecycleState::Stable {
            return Err(MlError::CheckpointVerification(
                "stable state must be established by promotion or rollback".into(),
            ));
        }
        if record.spec_digest == Digest::ZERO {
            return Err(MlError::CheckpointVerification(
                "model state has zero spec identity".into(),
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| MlError::CheckpointVerification("model registry lock poisoned".into()))?;
        if let Some(bundle) = state.bundles.get(&record.checkpoint_id)
            && bundle.spec_digest != record.spec_digest
        {
            return Err(MlError::SchemaMismatch);
        }
        state.records.push(record);
        Ok(())
    }

    pub fn state_records(&self) -> Result<Vec<ModelStateRecord>, MlError> {
        self.state
            .lock()
            .map(|state| state.records.clone())
            .map_err(|_| MlError::CheckpointVerification("model registry lock poisoned".into()))
    }

    pub fn rejected_checkpoints(&self) -> Result<Vec<ModelStateRecord>, MlError> {
        let state = self
            .state
            .lock()
            .map_err(|_| MlError::CheckpointVerification("model registry lock poisoned".into()))?;
        Ok(state
            .records
            .iter()
            .filter(|record| record.lifecycle == ModelLifecycleState::Rejected)
            .cloned()
            .collect())
    }

    pub fn audit_log(&self) -> Result<Vec<ModelTransitionAudit>, MlError> {
        self.state
            .lock()
            .map(|state| state.audit.clone())
            .map_err(|_| MlError::CheckpointVerification("model registry lock poisoned".into()))
    }
}

pub fn promotion_receipt_digest(
    candidate: &ModelCheckpointId,
    active_stable: Option<&ModelCheckpointId>,
    evaluation_report_digest: &Digest,
    generation_id: &Digest,
    policy_digest: &Digest,
) -> Digest {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"reflex.promotion.receipt.v1");
    payload.extend_from_slice(candidate.digest().as_bytes());
    if let Some(stable) = active_stable {
        payload.extend_from_slice(stable.digest().as_bytes());
    }
    payload.extend_from_slice(evaluation_report_digest.as_bytes());
    payload.extend_from_slice(generation_id.as_bytes());
    payload.extend_from_slice(policy_digest.as_bytes());
    Digest::hash_blake3(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ranker_spec() -> ModelSpec {
        ModelSpec {
            role: ModelRole::Ranker,
            architecture: ModelArchitecture::Mlp {
                hidden: vec![22, 49],
                activation: Activation::Gelu,
                bias: true,
            },
            input_schema: FeatureSchemaId::from_digest(Digest::hash_blake3(b"in")),
            output_schema: ModelOutputSchemaId::from_digest(Digest::hash_blake3(b"out")),
            parameter_budget: ParameterBudget {
                max_parameters: 2607,
                max_unique_parameters: None,
            },
            dtype: ModelDType::F32,
            canonical_backend: BackendClass::Flex,
        }
    }

    #[test]
    fn test_feature_schema_compatibility_digest() {
        let mut doc = FeatureSchemaDocument {
            schema_id: FeatureSchemaId::from_digest(Digest::ZERO),
            column_order: vec!["x".into(), "y".into()],
            dtype: ModelDType::F32,
            normalization: "zscore".into(),
            missing_value_rule: "zero".into(),
        };
        let d1 = feature_schema_compatibility_digest(&doc).unwrap();
        doc.schema_id = FeatureSchemaId::from_digest(d1);
        let d2 = feature_schema_compatibility_digest(&doc).unwrap();
        assert_eq!(d1, d2);
        validate_feature_schema_document(&doc).unwrap();
        doc.column_order.swap(0, 1);
        assert!(matches!(
            validate_feature_schema_document(&doc),
            Err(MlError::SchemaMismatch)
        ));
    }

    #[test]
    fn test_model_spec_roundtrip() {
        let spec = sample_ranker_spec();
        let json = serde_json::to_string(&spec).unwrap();
        let decoded: ModelSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, decoded);
        let d1 = model_spec_compatibility_digest(&spec, 64).unwrap();
        let d2 = model_spec_compatibility_digest(&decoded, 64).unwrap();
        assert_eq!(d1, d2);
    }

    #[test]
    fn test_parameter_count_exact() {
        for &budget in &M15_PARAMETER_BUDGETS {
            let hidden = m15_hidden_layers(budget, 64).expect("m15 layer map");
            let arch = ModelArchitecture::Mlp {
                hidden,
                activation: Activation::ReLU,
                bias: true,
            };
            assert_eq!(
                parameter_count(64, 1, &arch).unwrap(),
                budget,
                "budget {budget}"
            );
        }
    }

    #[test]
    fn custom_parameter_count_never_fabricates_a_default() {
        let architecture = ModelArchitecture::BurnCustom {
            factory: reflex_types::RegisteredFactoryId("custom".into()),
            config: reflex_types::CanonicalConfig::empty(),
        };
        assert!(parameter_count(64, 1, &architecture).is_err());
    }

    #[test]
    fn model_spec_identity_commits_to_semantics() {
        let spec = sample_ranker_spec();
        let original = model_spec_compatibility_digest(&spec, 64).unwrap();
        let mut changed = spec.clone();
        changed.canonical_backend = BackendClass::CubeCL;
        assert_ne!(
            original,
            model_spec_compatibility_digest(&changed, 64).unwrap()
        );
        changed = spec.clone();
        changed.parameter_budget.max_parameters += 1;
        assert_ne!(
            original,
            model_spec_compatibility_digest(&changed, 64).unwrap()
        );
    }

    #[test]
    fn test_model_loader_save_load() {
        let spec = sample_ranker_spec();
        let mut manifest = ModelCheckpointManifest {
            model_id: ModelCheckpointId::from_digest(Digest::hash_blake3(b"test-model")),
            spec,
            input_dim: 64,
            exact_parameter_count: 2607,
            weights_digest: Digest::hash_blake3(b"weights"),
            optimizer_digest: Some(Digest::hash_blake3(b"optim")),
            rng_digest: Some(Digest::hash_blake3(b"rng")),
            normalization_digest: None,
            dataset_digest: Some(Digest::hash_blake3(b"dataset")),
            sampler_digest: Some(Digest::hash_blake3(b"sampler")),
            code_identity: Some(Digest::hash_blake3(b"code")),
            provenance_digest: Some(Digest::hash_blake3(b"provenance")),
            backend: BackendClass::Flex,
            device: "cpu".to_string(),
            step: 100,
            epoch: 4,
            batch_position: 9,
            metrics_digest: Digest::hash_blake3(b"metrics"),
            inference_only: false,
        };
        manifest.seal().unwrap();
        assert!(!manifest.inference_only);
        manifest.validate().unwrap();
        let mut edited_metrics = manifest.clone();
        edited_metrics.metrics_digest = Digest::hash_blake3(b"edited");
        assert!(edited_metrics.validate().is_err());

        let mut inference_only = manifest.clone();
        inference_only.optimizer_digest = None;
        inference_only.rng_digest = None;
        inference_only.seal().unwrap();
        assert!(inference_only.inference_only);
        inference_only.validate().unwrap();
        assert!(inference_only.require_resumable().is_err());

        let mut missing_sampler = manifest.clone();
        missing_sampler.sampler_digest = None;
        missing_sampler.seal().unwrap();
        assert!(missing_sampler.validate().is_ok());
        assert!(missing_sampler.require_resumable().is_err());

        let mut zero_optimizer = manifest.clone();
        zero_optimizer.optimizer_digest = Some(Digest::ZERO);
        zero_optimizer.seal().unwrap();
        assert!(zero_optimizer.validate().is_err());

        let json = serde_json::to_string(&manifest).unwrap();
        let loaded: ModelCheckpointManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(manifest.model_id, loaded.model_id);
        assert_eq!(manifest.exact_parameter_count, loaded.exact_parameter_count);
        assert_eq!(manifest.weights_digest, loaded.weights_digest);
        assert_eq!(manifest.optimizer_digest, loaded.optimizer_digest);
    }

    #[test]
    fn test_role_firewall() {
        let taste = ModelSpec {
            role: ModelRole::TasteCritic,
            ..sample_ranker_spec()
        };
        assert!(validate_ranker_spec(&taste, 64).is_err());

        let proposal = ModelSpec {
            role: ModelRole::Proposal,
            ..sample_ranker_spec()
        };
        assert!(validate_ranker_spec(&proposal, 64).is_err());
    }

    #[test]
    fn test_taste_calibration_metadata() {
        let est = TasteEstimate {
            immediate_utility: Some(0.5),
            long_horizon_utility: None,
            descendant_value: None,
            cross_domain_reuse: None,
            compression: None,
            option_value: None,
            discovery_cost: None,
            uncertainty: Some(0.1),
            registry: TasteHeadRegistry::default(),
        };
        assert_eq!(est.registry.calibration.metric_name, "isotonic_v1");
        assert_eq!(est.registry.missing_head_policy, "censor");
    }

    #[test]
    fn test_shadow_scoring_config_default_off() {
        let cfg = ShadowScoringConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.cpu_budget_pct > 0.0);
    }

    struct DummyRanker(ModelCheckpointId);

    impl Ranker for DummyRanker {
        fn model_id(&self) -> ModelCheckpointId {
            self.0
        }

        fn score_batch(
            &self,
            features: &FeatureBatch,
            output: &mut [f32],
            _telemetry: &mut InferenceTelemetry,
        ) -> Result<(), MlError> {
            output[..features.rows].fill(0.0);
            Ok(())
        }
    }

    fn bundle(name: &[u8], spec_digest: Digest) -> ModelBundle {
        let checkpoint_id = ModelCheckpointId::from_digest(Digest::hash_blake3(name));
        ModelBundle {
            checkpoint_id,
            spec_digest,
            ranker: Arc::new(DummyRanker(checkpoint_id)),
            shadow: None,
            shadow_budget: ShadowScoringConfig::default(),
        }
    }

    #[test]
    fn model_registry_promotion_and_rollback_are_compatible_and_audited() {
        let spec_digest = Digest::hash_blake3(b"spec");
        let stable = bundle(b"stable", spec_digest);
        let stable_id = stable.checkpoint_id;
        let registry = ModelRegistry::new(stable);
        let candidate = Arc::new(bundle(b"candidate", spec_digest));
        registry
            .promote_between_generations(
                candidate.clone(),
                ModelStateRecord {
                    checkpoint_id: candidate.checkpoint_id,
                    lifecycle: ModelLifecycleState::Stable,
                    spec_digest,
                },
            )
            .unwrap();
        assert_eq!(registry.pin().checkpoint_id, candidate.checkpoint_id);
        registry.rollback_to(stable_id).unwrap();
        assert_eq!(registry.pin().checkpoint_id, stable_id);
        registry
            .record_checkpoint_state(ModelStateRecord {
                checkpoint_id: candidate.checkpoint_id,
                lifecycle: ModelLifecycleState::Rejected,
                spec_digest,
            })
            .unwrap();
        assert_eq!(registry.rejected_checkpoints().unwrap().len(), 1);
        assert!(
            registry
                .record_checkpoint_state(ModelStateRecord {
                    checkpoint_id: candidate.checkpoint_id,
                    lifecycle: ModelLifecycleState::Stable,
                    spec_digest,
                })
                .is_err()
        );
        let audit = registry.audit_log().unwrap();
        assert_eq!(audit.len(), 2);
        assert_eq!(audit[0].action, "promote");
        assert_eq!(audit[1].action, "rollback");

        let incompatible = Arc::new(bundle(b"bad", Digest::hash_blake3(b"other-spec")));
        assert!(
            registry
                .promote_between_generations(
                    incompatible.clone(),
                    ModelStateRecord {
                        checkpoint_id: incompatible.checkpoint_id,
                        lifecycle: ModelLifecycleState::Stable,
                        spec_digest: incompatible.spec_digest,
                    },
                )
                .is_err()
        );
    }
}
