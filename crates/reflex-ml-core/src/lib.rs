use arc_swap::ArcSwap;
use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use reflex_domain::FeatureBatch;
use reflex_types::{Digest, FeatureSchemaId, ModelCheckpointId, ModelOutputSchemaId};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use thiserror::Error;

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
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelRole {
    Ranker,
    TasteCritic,
    ProposalModel,
}

impl CanonicalEncode for ModelRole {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        match self {
            ModelRole::Ranker => out.write_u8(0),
            ModelRole::TasteCritic => out.write_u8(1),
            ModelRole::ProposalModel => out.write_u8(2),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Activation {
    ReLU,
    GELU,
    Tanh,
    Identity,
}

impl CanonicalEncode for Activation {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        match self {
            Activation::ReLU => out.write_u8(0),
            Activation::GELU => out.write_u8(1),
            Activation::Tanh => out.write_u8(2),
            Activation::Identity => out.write_u8(3),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ModelArchitecture {
    Linear {
        in_features: usize,
        out_features: usize,
        bias: bool,
    },
    Mlp {
        in_features: usize,
        hidden: Vec<usize>,
        out_features: usize,
        activation: Activation,
        bias: bool,
    },
    BottleneckMlp {
        in_features: usize,
        bottleneck: usize,
        hidden: usize,
        out_features: usize,
        activation: Activation,
    },
    BurnCustom {
        factory: String,
        config: String,
    },
}

impl CanonicalEncode for ModelArchitecture {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        match self {
            ModelArchitecture::Linear {
                in_features,
                out_features,
                bias,
            } => {
                out.write_u8(0)?;
                out.write_u64(*in_features as u64)?;
                out.write_u64(*out_features as u64)?;
                out.write_bool(*bias)?;
            }
            ModelArchitecture::Mlp {
                in_features,
                hidden,
                out_features,
                activation,
                bias,
            } => {
                out.write_u8(1)?;
                out.write_u64(*in_features as u64)?;
                out.write_u32(hidden.len() as u32)?;
                for &h in hidden {
                    out.write_u64(h as u64)?;
                }
                out.write_u64(*out_features as u64)?;
                activation.encode_canonical(out)?;
                out.write_bool(*bias)?;
            }
            ModelArchitecture::BottleneckMlp {
                in_features,
                bottleneck,
                hidden,
                out_features,
                activation,
            } => {
                out.write_u8(2)?;
                out.write_u64(*in_features as u64)?;
                out.write_u64(*bottleneck as u64)?;
                out.write_u64(*hidden as u64)?;
                out.write_u64(*out_features as u64)?;
                activation.encode_canonical(out)?;
            }
            ModelArchitecture::BurnCustom { factory, config } => {
                out.write_u8(3)?;
                out.write_str(factory)?;
                out.write_str(config)?;
            }
        }
        Ok(())
    }
}

impl ModelArchitecture {
    pub fn parameter_count(&self) -> usize {
        match self {
            ModelArchitecture::Linear {
                in_features,
                out_features,
                bias,
            } => {
                let weights = in_features * out_features;
                let biases = if *bias { *out_features } else { 0 };
                weights + biases
            }
            ModelArchitecture::Mlp {
                in_features,
                hidden,
                out_features,
                bias,
                ..
            } => {
                let mut total = 0;
                let mut prev = *in_features;
                for &h in hidden {
                    total += prev * h;
                    if *bias {
                        total += h;
                    }
                    prev = h;
                }
                total += prev * out_features;
                if *bias {
                    total += out_features;
                }
                total
            }
            ModelArchitecture::BottleneckMlp {
                in_features,
                bottleneck,
                hidden,
                out_features,
                ..
            } => {
                let w1 = in_features * bottleneck + bottleneck;
                let w2 = bottleneck * hidden + hidden;
                let w3 = hidden * out_features + out_features;
                w1 + w2 + w3
            }
            ModelArchitecture::BurnCustom { .. } => 2607,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelSpec {
    pub role: ModelRole,
    pub architecture: ModelArchitecture,
    pub input_schema: FeatureSchemaId,
    pub output_schema: ModelOutputSchemaId,
    pub parameter_budget: usize,
    pub backend_class: String,
}

impl CanonicalEncode for ModelSpec {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        self.role.encode_canonical(out)?;
        self.architecture.encode_canonical(out)?;
        out.write_digest(self.input_schema.digest())?;
        out.write_digest(self.output_schema.digest())?;
        out.write_u64(self.parameter_budget as u64)?;
        out.write_str(&self.backend_class)?;
        Ok(())
    }
}

impl ModelSpec {
    pub fn compatibility_digest(&self) -> Digest {
        reflex_canonical::content_id(b"reflex.model.spec.v1", self)
            .unwrap_or_else(|_| Digest::hash_blake3(b"model_spec_fallback"))
    }
}

#[derive(Clone, Debug, Default)]
pub struct InferenceTelemetry {
    pub forward_cpu_ns: u64,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TasteEstimate {
    pub immediate_utility: f32,
    pub long_horizon_utility: f32,
    pub descendant_value: f32,
    pub option_value: f32,
    pub uncertainty: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposalBatch {
    pub candidate_payloads: Vec<Vec<u8>>,
    pub prior_scores: Vec<f32>,
    pub metadata: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelCheckpointManifest {
    pub model_id: ModelCheckpointId,
    pub spec: ModelSpec,
    pub exact_parameter_count: usize,
    pub weights_digest: Digest,
    pub optimizer_digest: Option<Digest>,
    pub step: u64,
    pub metrics: String,
}

pub struct ModelBundle {
    pub ranker: Arc<dyn Ranker>,
}

pub struct ModelRegistry {
    active: ArcSwap<ModelBundle>,
}

impl ModelRegistry {
    pub fn new(bundle: ModelBundle) -> Self {
        Self {
            active: ArcSwap::from_pointee(bundle),
        }
    }

    pub fn pin(&self) -> Arc<ModelBundle> {
        self.active.load_full()
    }

    pub fn promote_between_generations(&self, next: Arc<ModelBundle>) {
        self.active.store(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parameter_count_exact() {
        // M1.5 reference MLP: in=64, hidden=[32, 16], out=1, with bias
        // Layer 1: 64*32 + 32 = 2080
        // Layer 2: 32*16 + 16 = 528
        // Layer 3: 16*1 + 1 = 17
        // Total = 2080 + 528 + 17 = 2625
        let arch = ModelArchitecture::Mlp {
            in_features: 64,
            hidden: vec![32, 16],
            out_features: 1,
            activation: Activation::ReLU,
            bias: true,
        };
        assert_eq!(arch.parameter_count(), 2625);
    }
}
