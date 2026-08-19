use burn::nn;
use burn::prelude::Module;
use burn::tensor::{Device, Tensor, TensorData};
use reflex_domain::FeatureBatch;
use reflex_ml_core::{InferenceTelemetry, MlError, Ranker};
use reflex_types::{Digest, ModelCheckpointId};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub type BurnBackend = burn::backend::NdArray;

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

    pub fn save(&self, model: &RankMlp, model_id: &ModelCheckpointId) -> Result<(), MlError> {
        let path = self.checkpoint_path(model_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| MlError::CheckpointVerification(e.to_string()))?;
        }
        let record = model.clone().into_record();
        record
            .save(&path)
            .map_err(|e| MlError::CheckpointVerification(e.to_string()))?;
        Ok(())
    }

    pub fn load(
        &self,
        model_id: &ModelCheckpointId,
        input_dim: usize,
        hidden_dim: usize,
    ) -> Result<RankMlp, MlError> {
        let path = self.checkpoint_path(model_id);
        if !path.exists() {
            return Err(MlError::CheckpointVerification(format!(
                "checkpoint not found: {}",
                path.display()
            )));
        }
        let record = burn::store::ModuleRecord::load(&path)
            .map_err(|e| MlError::CheckpointVerification(e.to_string()))?;
        let model = RankMlp::new(input_dim, hidden_dim, 1);
        Ok(model.load_record(record))
    }
}

pub struct InferenceEngine {
    model: Arc<RankMlp>,
}

impl InferenceEngine {
    pub fn new(model: Arc<RankMlp>) -> Self {
        Self { model }
    }

    pub fn score_batch(
        &self,
        features: &FeatureBatch,
        output: &mut [f32],
    ) -> Result<(), MlError> {
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
}

impl TrainingSession {
    pub fn new(input_dim: usize, hidden_dim: usize, lr: f64) -> Self {
        let device = Device::autodiff(Device::default());
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
        let optimizer: burn::optim::ModuleOptimizer = optim_config.init().into();
        Self {
            model,
            optimizer,
            lr,
        }
    }

    pub fn train_step(
        &mut self,
        features: &FeatureBatch,
        targets: &[f32],
    ) -> Result<f32, MlError> {
        if features.cols != self.model.input_dim() {
            return Err(MlError::DimensionMismatch {
                expected: self.model.input_dim(),
                found: features.cols,
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

        Ok(loss_value)
    }

    pub fn model(&self) -> &RankMlp {
        &self.model
    }

    pub fn model_mut(&mut self) -> &mut RankMlp {
        &mut self.model
    }
}

pub struct BurnRankMlp {
    model: Arc<RankMlp>,
    pub model_id: ModelCheckpointId,
}

impl BurnRankMlp {
    pub fn new(
        input_dim: usize,
        hidden_dim: usize,
        model_id: ModelCheckpointId,
    ) -> Self {
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
        let input: Tensor<2> = Tensor::from_data(
            TensorData::new(vec![0.5f32; 128], [8, 16]),
            &device,
        );
        let output = mlp.forward(input);
        assert_eq!(output.dims(), [8, 1]);
        assert!(output.into_data().as_slice::<f32>().unwrap().iter().all(|x| x.is_finite()));
    }

    #[test]
    fn test_burn_rank_mlp_random() {
        let mlp = BurnRankMlp::random(16, 32, 42);
        let features = make_features(4, 16);
        let mut output = vec![0.0f32; 4];
        let mut telemetry = InferenceTelemetry::default();
        mlp.score_batch(&features, &mut output, &mut telemetry).unwrap();
        assert!(output.iter().all(|x| x.is_finite()));
        assert_eq!(telemetry.rows_scored, 4);
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
    fn test_model_loader_save_load() {
        let tmp = tempfile::tempdir().unwrap();
        let loader = ModelLoader::new(tmp.path());
        let model = RankMlp::new(16, 32, 1);
        let model_id = ModelCheckpointId::from_digest(Digest::hash_blake3(b"test-model"));
        loader.save(&model, &model_id).unwrap();
        let loaded = loader.load(&model_id, 16, 32).unwrap();
        let features = make_features(4, 16);
        let scores_orig = model.forward_batch(&features).to_data().as_slice::<f32>().unwrap().to_vec();
        let scores_loaded = loaded.forward_batch(&features).to_data().as_slice::<f32>().unwrap().to_vec();
        for (a, b) in scores_orig.iter().zip(scores_loaded.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }
}
