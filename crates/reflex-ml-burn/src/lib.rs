use burn::module::Param;
use burn::nn;
use burn::optim::{AdamWConfig, GradientsParams, ModuleOptimizer};
use burn::prelude::Module;
use burn::tensor::{Device, Int, Tensor, TensorData};
use reflex_dataset::{CandidateKnowledge, DecisionGroup};
use reflex_domain::FeatureBatch;
use reflex_ml_core::{InferenceTelemetry, MlError, Ranker};
use reflex_ml_micro::MicroMlp;
use reflex_types::{Digest, ModelCheckpointId, StateId};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Canonical pure-Rust CPU backend. CubeCL CPU is intentionally not compiled
/// into the default worker until it has independent parity and benchmark
/// evidence.
pub type BurnBackend = burn::backend::Flex;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BackendAvailability {
    Available,
    Unavailable { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FactoryCapability {
    pub factory: String,
    pub inference: bool,
    pub training: bool,
    pub reason: Option<String>,
}

/// Exact factory surface implemented by this adapter. Unsupported master-plan
/// factories are explicit so callers cannot infer support from Burn itself.
pub fn factory_capabilities() -> Vec<FactoryCapability> {
    vec![
        FactoryCapability {
            factory: "fixed-two-hidden-gelu-ranker".to_string(),
            inference: true,
            training: true,
            reason: None,
        },
        FactoryCapability {
            factory: "micro-dense-relu-import".to_string(),
            inference: true,
            training: false,
            reason: Some("conversion adapter is inference/parity only".to_string()),
        },
        FactoryCapability {
            factory: "linear".to_string(),
            inference: false,
            training: false,
            reason: Some("general linear factory is not implemented".to_string()),
        },
        FactoryCapability {
            factory: "bottleneck-mlp".to_string(),
            inference: false,
            training: false,
            reason: Some("general bottleneck factory is not implemented".to_string()),
        },
        FactoryCapability {
            factory: "residual-ranker".to_string(),
            inference: false,
            training: false,
            reason: Some("residual factory is not implemented".to_string()),
        },
    ]
}

pub fn backend_availability() -> Vec<(&'static str, BackendAvailability)> {
    vec![
        ("burn-flex", BackendAvailability::Available),
        (
            "burn-cubecl-cpu",
            BackendAvailability::Unavailable {
                reason: "CubeCL CPU is not compiled or parity-qualified in this build".to_string(),
            },
        ),
    ]
}

/// Burn/Flex module with exactly the same dense-ReLU layout as `MicroMlp`.
/// It is the only model used for cross-backend selection; unrelated models
/// are never compared under a false numerical-equivalence claim.
#[derive(Module, Debug)]
pub struct MicroParityMlp {
    pub layers: Vec<nn::Linear>,
    pub relu: nn::Relu,
}

impl MicroParityMlp {
    pub fn from_micro(model: &MicroMlp) -> Result<Self, MlError> {
        let device = Device::default();
        let mut dimensions = Vec::with_capacity(model.hidden_dims.len() + 2);
        dimensions.push(model.input_dim);
        dimensions.extend(model.hidden_dims.iter().copied());
        dimensions.push(1);
        let mut offset = 0usize;
        let mut layers = Vec::with_capacity(dimensions.len() - 1);
        for pair in dimensions.windows(2) {
            let input = pair[0];
            let output = pair[1];
            let weight_count = input.checked_mul(output).ok_or_else(|| {
                MlError::CheckpointVerification("conversion shape overflow".into())
            })?;
            let row_major = model
                .weights
                .get(offset..offset + weight_count)
                .ok_or_else(|| {
                    MlError::CheckpointVerification("micro weight buffer is truncated".into())
                })?;
            let biases = model
                .weights
                .get(offset + weight_count..offset + weight_count + output)
                .ok_or_else(|| {
                    MlError::CheckpointVerification("micro bias buffer is truncated".into())
                })?;
            let mut burn_layout = vec![0.0f32; weight_count];
            for output_index in 0..output {
                for input_index in 0..input {
                    burn_layout[input_index * output + output_index] =
                        row_major[output_index * input + input_index];
                }
            }
            layers.push(nn::Linear {
                weight: Param::from_tensor(Tensor::from_data(
                    TensorData::new(burn_layout, [input, output]),
                    &device,
                )),
                bias: Some(Param::from_tensor(Tensor::from_data(
                    TensorData::new(biases.to_vec(), [output]),
                    &device,
                ))),
            });
            offset += weight_count + output;
        }
        if offset != model.weights.len() {
            return Err(MlError::CheckpointVerification(
                "micro weight buffer has trailing data".into(),
            ));
        }
        Ok(Self {
            layers,
            relu: nn::Relu::new(),
        })
    }

    pub fn forward(&self, mut input: Tensor<2>) -> Tensor<2> {
        for (index, layer) in self.layers.iter().enumerate() {
            input = layer.forward(input);
            if index + 1 != self.layers.len() {
                input = self.relu.forward(input);
            }
        }
        input
    }

    pub fn score_batch(&self, features: &FeatureBatch, output: &mut [f32]) -> Result<(), MlError> {
        if output.len() < features.rows {
            return Err(MlError::DimensionMismatch {
                expected: features.rows,
                found: output.len(),
            });
        }
        let input_dim = self
            .layers
            .first()
            .map(|layer| layer.weight.val().dims()[0])
            .ok_or_else(|| MlError::UnsupportedArchitecture("empty Burn MLP".into()))?;
        if features.cols != input_dim || features.values.len() != features.rows * features.cols {
            return Err(MlError::DimensionMismatch {
                expected: input_dim,
                found: features.cols,
            });
        }
        let device = Device::default();
        let input = Tensor::from_data(
            TensorData::new(features.values.clone(), [features.rows, features.cols]),
            &device,
        );
        let data = self.forward(input).into_data();
        let scores = data
            .as_slice::<f32>()
            .map_err(|error| MlError::CheckpointVerification(error.to_string()))?;
        output[..features.rows].copy_from_slice(&scores[..features.rows]);
        Ok(())
    }
}

pub fn verify_micro_flex_conversion(
    micro: &MicroMlp,
    features: &FeatureBatch,
    abs_tolerance: f32,
) -> Result<MicroParityMlp, MlError> {
    if !abs_tolerance.is_finite() || abs_tolerance < 0.0 {
        return Err(MlError::CheckpointVerification(
            "conversion tolerance must be finite and non-negative".into(),
        ));
    }
    let burn = MicroParityMlp::from_micro(micro)?;
    let mut micro_output = vec![0.0f32; features.rows];
    let mut scratch = vec![0.0f32; micro.scratch_len(features.rows)?];
    micro.try_score_rows(
        &features.values,
        features.rows,
        &mut micro_output,
        &mut scratch,
    )?;
    let mut burn_output = vec![0.0f32; features.rows];
    burn.score_batch(features, &mut burn_output)?;
    for (index, (&expected, &observed)) in micro_output.iter().zip(&burn_output).enumerate() {
        if (expected - observed).abs() > abs_tolerance {
            return Err(MlError::CheckpointVerification(format!(
                "micro/Flex conversion diverged at row {index}: {expected} vs {observed}"
            )));
        }
    }
    Ok(burn)
}

#[derive(Module, Debug)]
pub struct RankMlp {
    pub input: nn::Linear,
    pub hidden: nn::Linear,
    pub output: nn::Linear,
    pub gelu: nn::Gelu,
}

impl RankMlp {
    pub fn new(input_dim: usize, hidden_dim: usize, output_dim: usize) -> Self {
        let device = Device::default();
        let input = nn::LinearConfig::new(input_dim, hidden_dim).init(&device);
        let hidden = nn::LinearConfig::new(hidden_dim, hidden_dim).init(&device);
        let output = nn::LinearConfig::new(hidden_dim, output_dim).init(&device);
        let gelu = nn::Gelu::new();
        Self {
            input,
            hidden,
            output,
            gelu,
        }
    }

    pub fn forward<const D: usize>(&self, x: Tensor<D>) -> Tensor<D> {
        let x = self.gelu.forward(self.input.forward(x));
        let x = self.gelu.forward(self.hidden.forward(x));
        self.output.forward(x)
    }

    pub fn forward_batch(&self, features: &FeatureBatch) -> Tensor<2> {
        let device = Device::default();
        let data = TensorData::new(features.values.clone(), [features.rows, features.cols]);
        let input_tensor: Tensor<2> = Tensor::from_data(data, &device);
        self.forward(input_tensor)
    }

    pub fn input_dim(&self) -> usize {
        self.input.weight.val().dims()[0]
    }
}

pub struct ModelLoader {
    pub base_dir: PathBuf,
}

static CHECKPOINT_TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

impl ModelLoader {
    pub fn new(base_dir: impl AsRef<Path>) -> Self {
        Self {
            base_dir: base_dir.as_ref().to_path_buf(),
        }
    }

    pub fn checkpoint_path(&self, model_id: &ModelCheckpointId) -> PathBuf {
        let hex_str: String = model_id
            .digest()
            .as_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        self.base_dir.join(format!("{hex_str}.burn"))
    }

    pub fn save(&self, model: &RankMlp, model_id: &ModelCheckpointId) -> Result<Digest, MlError> {
        let bytes = checkpoint_bytes(model)?;
        let digest = Digest::hash_blake3(&bytes);
        if &digest != model_id.digest() {
            return Err(checkpoint_error(format!(
                "checkpoint ID {} does not match canonical Burnpack content {}",
                model_id.to_hex(),
                digest.to_hex()
            )));
        }
        let path = self.checkpoint_path(model_id);
        write_checkpoint_atomically(&path, &bytes)?;
        Ok(digest)
    }

    /// Persist a checkpoint under the identity of its canonical Burnpack bytes.
    pub fn save_content_addressed(&self, model: &RankMlp) -> Result<ModelCheckpointId, MlError> {
        let bytes = checkpoint_bytes(model)?;
        let id = ModelCheckpointId::from_digest(Digest::hash_blake3(&bytes));
        write_checkpoint_atomically(&self.checkpoint_path(&id), &bytes)?;
        Ok(id)
    }

    pub fn load(
        &self,
        model_id: &ModelCheckpointId,
        input_dim: usize,
        hidden_dim: usize,
    ) -> Result<RankMlp, MlError> {
        let path = self.checkpoint_path(model_id);
        let raw = std::fs::read(&path).map_err(|error| {
            checkpoint_error(format!(
                "checkpoint read failed at {}: {error}",
                path.display()
            ))
        })?;
        let actual = Digest::hash_blake3(&raw);
        if &actual != model_id.digest() {
            return Err(checkpoint_error(format!(
                "checkpoint content mismatch: expected {}, found {}",
                model_id.to_hex(),
                actual.to_hex()
            )));
        }
        let bytes = burn::tensor::Bytes::from_bytes_vec(raw);
        let record = burn::store::ModuleRecord::from_bytes(bytes)
            .map_err(|error| checkpoint_error(format!("invalid Burnpack: {error}")))?;
        let model = RankMlp::new(input_dim, hidden_dim, 1);
        let model = model
            .try_load_record(record)
            .map_err(|error| checkpoint_error(format!("checkpoint/spec mismatch: {error}")))?;
        validate_checkpoint_model(&model, input_dim, hidden_dim)?;
        Ok(model)
    }
}

fn checkpoint_error(message: impl Into<String>) -> MlError {
    MlError::CheckpointVerification(message.into())
}

fn checkpoint_bytes(model: &RankMlp) -> Result<burn::tensor::Bytes, MlError> {
    validate_checkpoint_model(
        model,
        model.input_dim(),
        model.hidden.weight.val().dims()[0],
    )?;
    model
        .clone()
        .into_record()
        .into_bytes()
        .map_err(|error| checkpoint_error(format!("Burnpack encoding failed: {error}")))
}

fn write_checkpoint_atomically(path: &Path, bytes: &[u8]) -> Result<(), MlError> {
    let parent = path
        .parent()
        .ok_or_else(|| checkpoint_error("checkpoint path has no parent"))?;
    std::fs::create_dir_all(parent).map_err(|error| checkpoint_error(error.to_string()))?;

    if path.exists() {
        let existing = std::fs::read(path).map_err(|error| checkpoint_error(error.to_string()))?;
        if existing == bytes {
            return Ok(());
        }
        return Err(checkpoint_error(format!(
            "checkpoint identity collision at {}",
            path.display()
        )));
    }

    let sequence = CHECKPOINT_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temp = parent.join(format!(".checkpoint-{}-{sequence}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|error| checkpoint_error(error.to_string()))?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| checkpoint_error(error.to_string()))?;
        std::fs::rename(&temp, path).map_err(|error| checkpoint_error(error.to_string()))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

fn validate_checkpoint_model(
    model: &RankMlp,
    input_dim: usize,
    hidden_dim: usize,
) -> Result<(), MlError> {
    validate_tensor(
        "input.weight",
        &model.input.weight.val(),
        [input_dim, hidden_dim],
    )?;
    validate_bias("input.bias", model.input.bias.as_ref(), hidden_dim)?;
    validate_tensor(
        "hidden.weight",
        &model.hidden.weight.val(),
        [hidden_dim, hidden_dim],
    )?;
    validate_bias("hidden.bias", model.hidden.bias.as_ref(), hidden_dim)?;
    validate_tensor("output.weight", &model.output.weight.val(), [hidden_dim, 1])?;
    validate_bias("output.bias", model.output.bias.as_ref(), 1)
}

fn validate_bias(
    name: &str,
    bias: Option<&burn::module::Param<Tensor<1>>>,
    expected: usize,
) -> Result<(), MlError> {
    let bias = bias.ok_or_else(|| checkpoint_error(format!("{name} is missing")))?;
    validate_tensor(name, &bias.val(), [expected])
}

fn validate_tensor<const D: usize>(
    name: &str,
    tensor: &Tensor<D>,
    expected: [usize; D],
) -> Result<(), MlError> {
    let found = tensor.dims();
    if found != expected {
        return Err(checkpoint_error(format!(
            "{name} shape mismatch: expected {expected:?}, found {found:?}"
        )));
    }
    let data = tensor.to_data();
    let values = data
        .as_slice::<f32>()
        .map_err(|error| checkpoint_error(format!("{name} is not canonical f32: {error}")))?;
    if values.iter().any(|value| !value.is_finite()) {
        return Err(checkpoint_error(format!(
            "{name} contains non-finite values"
        )));
    }
    Ok(())
}

pub struct InferenceEngine {
    model: Arc<RankMlp>,
}

impl InferenceEngine {
    pub fn new(model: Arc<RankMlp>) -> Self {
        Self { model }
    }

    pub fn score_batch(&self, features: &FeatureBatch, output: &mut [f32]) -> Result<(), MlError> {
        if features.cols != self.model.input_dim() {
            return Err(MlError::DimensionMismatch {
                expected: self.model.input_dim(),
                found: features.cols,
            });
        }
        if output.len() < features.rows {
            return Err(MlError::DimensionMismatch {
                expected: features.rows,
                found: output.len(),
            });
        }

        let tensor = self.model.forward_batch(features);
        let scores = tensor.to_data();
        let slice = scores.as_slice::<f32>().unwrap();
        for (i, val) in slice.iter().enumerate().take(features.rows) {
            output[i] = *val;
        }
        Ok(())
    }
}

pub struct TrainingSession {
    model: RankMlp,
    optimizer: burn::optim::ModuleOptimizer,
    lr: f64,
    step: u64,
    rng_seed: u64,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BurnTrainingCheckpoint {
    pub model_burnpack: Vec<u8>,
    pub optimizer_burnpack: Vec<u8>,
    pub model_digest: Digest,
    pub optimizer_digest: Digest,
    pub input_dim: usize,
    pub hidden_dim: usize,
    pub learning_rate: f64,
    pub step: u64,
    pub rng_seed: u64,
}

impl TrainingSession {
    pub fn new(input_dim: usize, hidden_dim: usize, lr: f64) -> Self {
        Self::new_seeded(input_dim, hidden_dim, lr, 0)
    }

    pub fn new_seeded(input_dim: usize, hidden_dim: usize, lr: f64, rng_seed: u64) -> Self {
        let device = Device::autodiff(Device::default());
        device.seed(rng_seed);
        let input = nn::LinearConfig::new(input_dim, hidden_dim).init(&device);
        let hidden = nn::LinearConfig::new(hidden_dim, hidden_dim).init(&device);
        let output = nn::LinearConfig::new(hidden_dim, 1).init(&device);
        let model = RankMlp {
            input,
            hidden,
            output,
            gelu: nn::Gelu::new(),
        };
        let optim_config = burn::optim::AdamWConfig::new();
        let optimizer: burn::optim::ModuleOptimizer = optim_config.init();
        Self {
            model,
            optimizer,
            lr,
            step: 0,
            rng_seed,
        }
    }

    pub fn train_step(&mut self, features: &FeatureBatch, targets: &[f32]) -> Result<f32, MlError> {
        if !self.lr.is_finite() || self.lr <= 0.0 {
            return Err(MlError::CheckpointVerification(
                "training learning rate must be finite and positive".into(),
            ));
        }
        if features.cols != self.model.input_dim() {
            return Err(MlError::DimensionMismatch {
                expected: self.model.input_dim(),
                found: features.cols,
            });
        }
        if targets.len() != features.rows {
            return Err(MlError::DimensionMismatch {
                expected: features.rows,
                found: targets.len(),
            });
        }

        let device = Device::autodiff(Device::default());
        let input_data = TensorData::new(features.values.clone(), [features.rows, features.cols]);
        let input: Tensor<2> = Tensor::from_data(input_data, &device);
        let targets_data = TensorData::new(targets.to_vec(), [features.rows, 1]);
        let targets_tensor: Tensor<2> = Tensor::from_data(targets_data, &device);

        let predictions = self.model.forward(input);
        let loss = (predictions - targets_tensor).powf_scalar(2.0).mean();
        let loss_value: f32 = loss.clone().into_data().as_slice::<f32>().unwrap()[0];

        let grads = loss.backward();
        let grads_params = burn::optim::GradientsParams::from_grads(grads, &self.model);

        self.model = self
            .optimizer
            .step(self.lr, self.model.clone(), grads_params);
        validate_checkpoint_model(
            &self.model,
            self.model.input_dim(),
            self.model.hidden.weight.val().dims()[0],
        )?;
        self.step = self.step.checked_add(1).ok_or_else(|| {
            MlError::CheckpointVerification("training step counter overflow".into())
        })?;

        Ok(loss_value)
    }

    pub fn model(&self) -> &RankMlp {
        &self.model
    }

    pub fn model_mut(&mut self) -> &mut RankMlp {
        &mut self.model
    }

    pub fn step(&self) -> u64 {
        self.step
    }

    pub fn rng_seed(&self) -> u64 {
        self.rng_seed
    }

    pub fn checkpoint(&self) -> Result<BurnTrainingCheckpoint, MlError> {
        if !self.lr.is_finite() || self.lr <= 0.0 {
            return Err(checkpoint_error(
                "training learning rate must be finite and positive",
            ));
        }
        let model_burnpack = checkpoint_bytes(&self.model)?.to_vec();
        let optimizer_burnpack = self
            .optimizer
            .into_bytes()
            .map_err(|error| checkpoint_error(format!("optimizer Burnpack failed: {error}")))?
            .to_vec();
        Ok(BurnTrainingCheckpoint {
            model_digest: Digest::hash_blake3(&model_burnpack),
            optimizer_digest: Digest::hash_blake3(&optimizer_burnpack),
            model_burnpack,
            optimizer_burnpack,
            input_dim: self.model.input_dim(),
            hidden_dim: self.model.hidden.weight.val().dims()[0],
            learning_rate: self.lr,
            step: self.step,
            rng_seed: self.rng_seed,
        })
    }

    pub fn restore(checkpoint: &BurnTrainingCheckpoint) -> Result<Self, MlError> {
        if checkpoint.model_digest != Digest::hash_blake3(&checkpoint.model_burnpack)
            || checkpoint.optimizer_digest != Digest::hash_blake3(&checkpoint.optimizer_burnpack)
            || !checkpoint.learning_rate.is_finite()
            || checkpoint.learning_rate <= 0.0
            || checkpoint.input_dim == 0
            || checkpoint.hidden_dim == 0
        {
            return Err(checkpoint_error(
                "training checkpoint digest or learning rate is invalid",
            ));
        }
        let model_record = burn::store::ModuleRecord::from_bytes(
            burn::tensor::Bytes::from_bytes_vec(checkpoint.model_burnpack.clone()),
        )
        .map_err(|error| checkpoint_error(format!("invalid model Burnpack: {error}")))?;
        let device = Device::autodiff(Device::default());
        device.seed(checkpoint.rng_seed);
        let model = RankMlp {
            input: nn::LinearConfig::new(checkpoint.input_dim, checkpoint.hidden_dim).init(&device),
            hidden: nn::LinearConfig::new(checkpoint.hidden_dim, checkpoint.hidden_dim)
                .init(&device),
            output: nn::LinearConfig::new(checkpoint.hidden_dim, 1).init(&device),
            gelu: nn::Gelu::new(),
        }
        .try_load_record(model_record)
        .map_err(|error| checkpoint_error(format!("training model mismatch: {error}")))?;
        validate_checkpoint_model(&model, checkpoint.input_dim, checkpoint.hidden_dim)?;
        let optimizer: ModuleOptimizer = AdamWConfig::new()
            .init()
            .from_bytes(burn::tensor::Bytes::from_bytes_vec(
                checkpoint.optimizer_burnpack.clone(),
            ))
            .map_err(|error| checkpoint_error(format!("invalid optimizer Burnpack: {error}")))?;
        Ok(Self {
            model,
            optimizer,
            lr: checkpoint.learning_rate,
            step: checkpoint.step,
            rng_seed: checkpoint.rng_seed,
        })
    }

    pub fn export_ranker(&self, model_id: ModelCheckpointId) -> Result<BurnRankMlp, MlError> {
        let record = self.model.clone().into_record();
        let model = RankMlp::new(
            self.model.input_dim(),
            self.model.hidden.weight.val().dims()[0],
            1,
        )
        .try_load_record(record)
        .map_err(|error| checkpoint_error(format!("training export failed: {error}")))?;
        validate_checkpoint_model(
            &model,
            model.input_dim(),
            model.hidden.weight.val().dims()[0],
        )?;
        Ok(BurnRankMlp::from_model(model, model_id))
    }
}

pub struct BurnRankMlp {
    model: Arc<RankMlp>,
    pub model_id: ModelCheckpointId,
}

impl BurnRankMlp {
    pub fn new(input_dim: usize, hidden_dim: usize, model_id: ModelCheckpointId) -> Self {
        let model = RankMlp::new(input_dim, hidden_dim, 1);
        Self {
            model: Arc::new(model),
            model_id,
        }
    }

    pub fn random(input_dim: usize, hidden_dim: usize, seed: u64) -> Self {
        let device = Device::default();
        device.seed(seed);
        let model = RankMlp::new(input_dim, hidden_dim, 1);

        let model_id = ModelCheckpointId::from_digest(Digest::hash_blake3(
            &format!("burn-mlp-{input_dim}-{seed}").into_bytes(),
        ));

        Self {
            model: Arc::new(model),
            model_id,
        }
    }

    pub fn from_model(model: RankMlp, model_id: ModelCheckpointId) -> Self {
        Self {
            model: Arc::new(model),
            model_id,
        }
    }

    pub fn input_dim(&self) -> usize {
        self.model.input_dim()
    }

    pub fn hidden_dim(&self) -> usize {
        self.model.hidden.weight.val().dims()[0]
    }

    pub fn rank_mlp(&self) -> &RankMlp {
        &self.model
    }
}

impl Ranker for BurnRankMlp {
    fn model_id(&self) -> ModelCheckpointId {
        self.model_id
    }

    fn score_batch(
        &self,
        features: &FeatureBatch,
        output: &mut [f32],
        telemetry: &mut InferenceTelemetry,
    ) -> Result<(), MlError> {
        let engine = InferenceEngine::new(self.model.clone());
        let start = std::time::Instant::now();
        engine.score_batch(features, output)?;
        telemetry.forward_cpu_ns += start.elapsed().as_nanos() as u64;
        telemetry.rows_scored += features.rows;
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct BurnTrainingConfig {
    pub epochs: usize,
    pub learning_rate: f32,
}

#[derive(Clone, Debug)]
pub struct BurnTrainingMetrics {
    pub epoch_losses: Vec<f32>,
    pub final_loss: f32,
    pub total_steps: usize,
}

pub fn collect_supervised_pairs(group: &DecisionGroup) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    for i in 0..group.labels.len() {
        for j in 0..group.labels.len() {
            match (&group.labels[i], &group.labels[j]) {
                (
                    CandidateKnowledge::Viable {
                        best_actions_to_go: c1,
                        ..
                    },
                    CandidateKnowledge::Viable {
                        best_actions_to_go: c2,
                        ..
                    },
                ) if c1 < c2 => pairs.push((i, j)),
                (CandidateKnowledge::Viable { .. }, CandidateKnowledge::KnownDead { .. }) => {
                    pairs.push((i, j));
                }
                _ => {}
            }
        }
    }
    pairs
}

pub fn train_burn_model(
    burn_model: BurnRankMlp,
    groups: &[DecisionGroup],
    features_by_state: &HashMap<StateId, Vec<f32>>,
    config: &BurnTrainingConfig,
) -> Result<(BurnRankMlp, BurnTrainingMetrics), MlError> {
    if groups.is_empty() {
        return Err(MlError::UnsupportedArchitecture("empty dataset".into()));
    }

    let input_dim = burn_model.input_dim();
    let hidden_dim = burn_model.hidden_dim();
    let model_id = burn_model.model_id;

    let device = Device::autodiff(Device::default());
    let record = burn_model.rank_mlp().clone().into_record();
    let input = nn::LinearConfig::new(input_dim, hidden_dim).init(&device);
    let hidden = nn::LinearConfig::new(hidden_dim, hidden_dim).init(&device);
    let output = nn::LinearConfig::new(hidden_dim, 1).init(&device);
    let mut model = RankMlp {
        input,
        hidden,
        output,
        gelu: nn::Gelu::new(),
    };
    model = model.load_record(record);

    let optim_config = AdamWConfig::new();
    let mut optimizer: ModuleOptimizer = optim_config.init();

    let mut epoch_losses = Vec::new();
    let mut total_steps = 0usize;

    for _epoch in 0..config.epochs {
        let mut epoch_loss_sum = 0.0f32;
        let mut step_count = 0usize;

        for group in groups {
            let Some(feats) = features_by_state.get(&group.state_id) else {
                continue;
            };
            let num_cand = group.candidate_ids.len();
            if num_cand == 0 {
                continue;
            }
            let pairs = collect_supervised_pairs(group);
            if pairs.is_empty() {
                continue;
            }

            let feat_slice = &feats[..num_cand * input_dim];
            let input_data = TensorData::new(feat_slice.to_vec(), [num_cand, input_dim]);
            let features_tensor: Tensor<2> = Tensor::from_data(input_data, &device);
            let scores = model.forward(features_tensor).squeeze_dim::<1>(1);

            let better_indices: Vec<i64> = pairs.iter().map(|(b, _)| *b as i64).collect();
            let worse_indices: Vec<i64> = pairs.iter().map(|(_, w)| *w as i64).collect();
            let num_pairs = pairs.len();
            let better_idx =
                Tensor::<1, Int>::from_data(TensorData::new(better_indices, [num_pairs]), &device);
            let worse_idx =
                Tensor::<1, Int>::from_data(TensorData::new(worse_indices, [num_pairs]), &device);

            let better_scores = scores.clone().select(0, better_idx);
            let worse_scores = scores.clone().select(0, worse_idx);
            let margins = better_scores - worse_scores;
            let loss = (margins * -1.0f32).exp().add_scalar(1.0f32).log().mean();
            let loss_value: f32 = loss.clone().into_data().as_slice::<f32>().unwrap()[0];
            if !loss_value.is_finite() {
                return Err(MlError::NonFiniteValue);
            }

            let grads = loss.backward();
            let grads_params = GradientsParams::from_grads(grads, &model);
            model = optimizer.step(config.learning_rate as f64, model, grads_params);

            epoch_loss_sum += loss_value;
            step_count += 1;
            total_steps += 1;
        }

        epoch_losses.push(if step_count > 0 {
            epoch_loss_sum / step_count as f32
        } else {
            0.0
        });
    }

    let record = model.into_record();
    let mut final_model = RankMlp::new(input_dim, hidden_dim, 1);
    final_model = final_model.load_record(record);
    let final_loss = epoch_losses.last().copied().unwrap_or(0.0);
    Ok((
        BurnRankMlp::from_model(final_model, model_id),
        BurnTrainingMetrics {
            epoch_losses,
            final_loss,
            total_steps,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_types::FeatureSchemaId;

    fn make_features(rows: usize, cols: usize) -> FeatureBatch {
        let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(b"test"));
        let mut feat = FeatureBatch::new(rows, cols, schema);
        for val in feat.values.iter_mut() {
            *val = 0.5;
        }
        feat
    }

    #[test]
    fn test_burn_mlp_forward() {
        let mlp = RankMlp::new(16, 32, 1);
        let device = Device::default();
        let input: Tensor<2> =
            Tensor::from_data(TensorData::new(vec![0.5f32; 128], [8, 16]), &device);
        let output = mlp.forward(input);
        assert_eq!(output.dims(), [8, 1]);
        assert!(
            output
                .into_data()
                .as_slice::<f32>()
                .unwrap()
                .iter()
                .all(|x| x.is_finite())
        );
    }

    #[test]
    fn test_burn_rank_mlp_random() {
        let mlp = BurnRankMlp::random(16, 32, 42);
        let features = make_features(4, 16);
        let mut output = vec![0.0f32; 4];
        let mut telemetry = InferenceTelemetry::default();
        mlp.score_batch(&features, &mut output, &mut telemetry)
            .unwrap();
        assert!(output.iter().all(|x| x.is_finite()));
        assert_eq!(telemetry.rows_scored, 4);
    }

    #[test]
    fn micro_to_flex_conversion_preserves_outputs() {
        let micro = MicroMlp::reference_mlp_2607(44);
        let features = make_features(32, 64);
        verify_micro_flex_conversion(&micro, &features, 1e-5).unwrap();
    }

    #[test]
    fn capability_audit_fails_closed_for_unimplemented_factories() {
        assert!(factory_capabilities().iter().any(|capability| {
            capability.factory == "fixed-two-hidden-gelu-ranker"
                && capability.inference
                && capability.training
        }));
        assert!(factory_capabilities().iter().any(|capability| {
            capability.factory == "bottleneck-mlp"
                && !capability.inference
                && !capability.training
                && capability.reason.is_some()
        }));
    }

    #[test]
    fn training_checkpoint_restores_model_optimizer_and_step() {
        let features = make_features(4, 3);
        let targets = [0.2, -0.1, 0.7, 0.4];
        let mut uninterrupted = TrainingSession::new_seeded(3, 5, 0.001, 91);
        for _ in 0..3 {
            uninterrupted.train_step(&features, &targets).unwrap();
        }
        let checkpoint = uninterrupted.checkpoint().unwrap();
        let mut resumed = TrainingSession::restore(&checkpoint).unwrap();
        assert_eq!(resumed.step(), 3);
        assert_eq!(resumed.rng_seed(), 91);
        let expected_loss = uninterrupted.train_step(&features, &targets).unwrap();
        let resumed_loss = resumed.train_step(&features, &targets).unwrap();
        assert_eq!(expected_loss.to_bits(), resumed_loss.to_bits());
        let expected = uninterrupted.checkpoint().unwrap();
        let actual = resumed.checkpoint().unwrap();
        assert_eq!(expected.model_digest, actual.model_digest);
        assert_ne!(actual.optimizer_digest, Digest::ZERO);
        // Burnpack rewrites optimizer parameter IDs on restore, so its bytes are
        // not a semantic determinism claim. Prove resumed update state by taking
        // another identical step and comparing the resulting model bytes.
        uninterrupted.train_step(&features, &targets).unwrap();
        resumed.train_step(&features, &targets).unwrap();
        assert_eq!(
            uninterrupted.checkpoint().unwrap().model_digest,
            resumed.checkpoint().unwrap().model_digest
        );

        let mut tampered = checkpoint;
        tampered.optimizer_burnpack[0] ^= 1;
        assert!(TrainingSession::restore(&tampered).is_err());

        let exported = resumed
            .export_ranker(ModelCheckpointId::from_digest(Digest::hash_blake3(
                b"export",
            )))
            .unwrap();
        let mut output = vec![0.0; features.rows];
        exported
            .score_batch(&features, &mut output, &mut InferenceTelemetry::default())
            .unwrap();
        assert!(output.iter().all(|value| value.is_finite()));
    }

    #[test]
    fn cubecl_candidate_is_explicitly_unavailable() {
        assert!(backend_availability().iter().any(|(name, status)| {
            *name == "burn-cubecl-cpu" && matches!(status, BackendAvailability::Unavailable { .. })
        }));
    }

    #[test]
    fn test_inference_engine() {
        let model = Arc::new(RankMlp::new(16, 32, 1));
        let engine = InferenceEngine::new(model);
        let features = make_features(8, 16);
        let mut output = vec![0.0f32; 8];
        engine.score_batch(&features, &mut output).unwrap();
        assert!(output.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn test_training_session() {
        let mut session = TrainingSession::new(16, 32, 0.01);
        let features = make_features(8, 16);
        let targets = vec![1.0f32; 8];
        let loss = session.train_step(&features, &targets).unwrap();
        assert!(loss.is_finite());
    }

    #[test]
    fn test_dimension_mismatch() {
        let model = Arc::new(RankMlp::new(16, 32, 1));
        let engine = InferenceEngine::new(model);
        let features = make_features(4, 32);
        let mut output = vec![0.0f32; 4];
        let result = engine.score_batch(&features, &mut output);
        assert!(matches!(result, Err(MlError::DimensionMismatch { .. })));
    }

    #[test]
    fn test_burn_model_training_convergence() {
        let model = BurnRankMlp::random(4, 8, 42);
        let s1 = reflex_types::StateId::from_digest(Digest::hash_blake3(b"s-burn"));
        let c1 = reflex_types::CandidateId::from_digest(Digest::hash_blake3(b"c1"));
        let c2 = reflex_types::CandidateId::from_digest(Digest::hash_blake3(b"c2"));
        let group = DecisionGroup {
            state_id: s1,
            candidate_ids: vec![c1, c2],
            labels: vec![
                CandidateKnowledge::Viable {
                    best_actions_to_go: 1,
                    receipts: smallvec::smallvec![Digest::hash_blake3(b"receipt")],
                },
                CandidateKnowledge::KnownDead {
                    certificate: Digest::hash_blake3(b"certificate"),
                },
            ],
            feature_ref: Digest::ZERO,
            source_episodes: Vec::new(),
            coverage: "test".to_string(),
        };
        let mut features_by_state = HashMap::new();
        features_by_state.insert(s1, vec![1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0]);
        let config = BurnTrainingConfig {
            epochs: 50,
            learning_rate: 0.05,
        };
        let (_, metrics) = train_burn_model(model, &[group], &features_by_state, &config).unwrap();
        assert!(metrics.final_loss < metrics.epoch_losses[0]);
    }

    #[test]
    fn test_model_loader_save_load() {
        let tmp = tempfile::tempdir().unwrap();
        let loader = ModelLoader::new(tmp.path());
        let model = RankMlp::new(16, 32, 1);
        let model_id = loader.save_content_addressed(&model).unwrap();
        let digest = loader.save(&model, &model_id).unwrap();
        assert_eq!(&digest, model_id.digest());
        let loaded = loader.load(&model_id, 16, 32).unwrap();
        let features = make_features(4, 16);
        let scores_orig = model
            .forward_batch(&features)
            .to_data()
            .as_slice::<f32>()
            .unwrap()
            .to_vec();
        let scores_loaded = loaded
            .forward_batch(&features)
            .to_data()
            .as_slice::<f32>()
            .unwrap()
            .to_vec();
        for (a, b) in scores_orig.iter().zip(scores_loaded.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn test_checkpoint_corruption_fails_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let loader = ModelLoader::new(tmp.path());
        let model = RankMlp::new(4, 8, 1);
        let model_id = loader.save_content_addressed(&model).unwrap();
        let path = loader.checkpoint_path(&model_id);
        let mut bytes = std::fs::read(&path).unwrap();
        let corrupt_at = bytes.len() / 2;
        bytes[corrupt_at] ^= 0x80;
        std::fs::write(path, bytes).unwrap();

        assert!(matches!(
            loader.load(&model_id, 4, 8),
            Err(MlError::CheckpointVerification(_))
        ));
    }

    #[test]
    fn test_checkpoint_identity_survives_base_path_rename() {
        let tmp = tempfile::tempdir().unwrap();
        let original = tmp.path().join("original");
        let renamed = tmp.path().join("renamed");
        let loader = ModelLoader::new(&original);
        let model = RankMlp::new(4, 8, 1);
        let model_id = loader.save_content_addressed(&model).unwrap();
        std::fs::rename(&original, &renamed).unwrap();

        let renamed_loader = ModelLoader::new(&renamed);
        assert!(renamed_loader.load(&model_id, 4, 8).is_ok());
    }

    #[test]
    fn test_checkpoint_wrong_spec_and_nonfinite_save_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let loader = ModelLoader::new(tmp.path());
        let model = RankMlp::new(4, 8, 1);
        let model_id = loader.save_content_addressed(&model).unwrap();
        assert!(matches!(
            loader.load(&model_id, 5, 8),
            Err(MlError::CheckpointVerification(_))
        ));

        let mut nonfinite = RankMlp::new(4, 8, 1);
        nonfinite.output.bias = Some(burn::module::Param::from_data(
            TensorData::new(vec![f32::NAN], [1]),
            &Device::default(),
        ));
        assert!(matches!(
            loader.save_content_addressed(&nonfinite),
            Err(MlError::CheckpointVerification(_))
        ));
    }
}
