use reflex_domain::FeatureBatch;
use reflex_ml_core::{
    InferenceTelemetry, MlError, Ranker, m15_hidden_layers, parameter_count, validate_ranker_spec,
};
use reflex_types::{
    Activation, BackendClass, Digest, FeatureSchemaId, ModelArchitecture, ModelCheckpointId,
    ModelDType, ModelOutputSchemaId, ModelRole, ModelSpec, ParameterBudget,
};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;

pub const WARM_BATCH_ROWS: usize = 64;

thread_local! {
    /// The object-safe `Ranker` API cannot accept caller-owned scratch. Reuse one
    /// buffer per calling thread instead of serializing every scorer behind a
    /// model-wide mutex. Callers with a fixed latency budget should use
    /// `score_rows` and own the scratch buffer explicitly.
    static RANKER_SCRATCH: RefCell<Vec<f32>> = const { RefCell::new(Vec::new()) };
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MicroMlp {
    pub input_dim: usize,
    pub hidden_dims: Vec<usize>,
    pub weights: Box<[f32]>,
    pub model_id: ModelCheckpointId,
}

impl MicroMlp {
    pub fn new(
        input_dim: usize,
        hidden_dims: Vec<usize>,
        weights: Box<[f32]>,
        model_id: ModelCheckpointId,
    ) -> Self {
        Self::try_new(input_dim, hidden_dims, weights, model_id)
            .expect("invalid micro MLP configuration")
    }

    pub fn try_new(
        input_dim: usize,
        hidden_dims: Vec<usize>,
        weights: Box<[f32]>,
        model_id: ModelCheckpointId,
    ) -> Result<Self, MlError> {
        if input_dim == 0 || hidden_dims.contains(&0) {
            return Err(MlError::UnsupportedArchitecture(
                "MLP dimensions must be non-zero".into(),
            ));
        }
        let arch = ModelArchitecture::Mlp {
            hidden: hidden_dims.clone(),
            activation: Activation::ReLU,
            bias: true,
        };
        let expected = checked_parameter_count(input_dim, &hidden_dims)?;
        debug_assert_eq!(expected, parameter_count(input_dim, 1, &arch)?);
        if weights.len() != expected {
            return Err(MlError::DimensionMismatch {
                expected,
                found: weights.len(),
            });
        }
        if weights.iter().any(|weight| !weight.is_finite()) {
            return Err(MlError::NonFiniteValue);
        }
        Ok(Self {
            input_dim,
            hidden_dims,
            weights,
            model_id,
        })
    }

    pub fn output_dim(&self) -> usize {
        1
    }

    pub fn parameter_count(&self) -> usize {
        self.weights.len()
    }

    pub fn reference_mlp_2607(seed: u64) -> Self {
        Self::random_with_hidden(64, m15_hidden_layers(2607, 64).expect("2607 layers"), seed)
    }

    pub fn random(input_dim: usize, hidden_dim: usize, seed: u64) -> Self {
        Self::random_with_hidden(input_dim, vec![hidden_dim], seed)
    }

    pub fn random_with_hidden(input_dim: usize, hidden_dims: Vec<usize>, seed: u64) -> Self {
        use rand::Rng;
        use rand::SeedableRng;
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
        let arch = ModelArchitecture::Mlp {
            hidden: hidden_dims.clone(),
            activation: Activation::ReLU,
            bias: true,
        };
        let count = parameter_count(input_dim, 1, &arch)
            .expect("generated micro MLP dimensions must have a finite parameter count");
        let scale = (2.0 / input_dim as f32).sqrt();
        let weights: Box<[f32]> = (0..count)
            .map(|_| rng.gen_range(-scale..scale))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let model_id = ModelCheckpointId::from_digest(Digest::hash_blake3(
            &format!("micro-mlp-{input_dim}-{hidden_dims:?}-{seed}").into_bytes(),
        ));
        Self::new(input_dim, hidden_dims, weights, model_id)
    }

    #[inline]
    pub fn score_rows(&self, features: &[f32], rows: usize, out: &mut [f32], scratch: &mut [f32]) {
        self.try_score_rows(features, rows, out, scratch)
            .expect("invalid micro MLP inference buffers");
    }

    /// Scores rows without allocating. `scratch_len(rows)` floats are required.
    #[inline]
    pub fn try_score_rows(
        &self,
        features: &[f32],
        rows: usize,
        out: &mut [f32],
        scratch: &mut [f32],
    ) -> Result<(), MlError> {
        let feature_count = rows
            .checked_mul(self.input_dim)
            .ok_or(MlError::DimensionMismatch {
                expected: usize::MAX,
                found: features.len(),
            })?;
        if features.len() != feature_count {
            return Err(MlError::DimensionMismatch {
                expected: feature_count,
                found: features.len(),
            });
        }
        if out.len() < rows {
            return Err(MlError::DimensionMismatch {
                expected: rows,
                found: out.len(),
            });
        }
        let scratch_len = self.scratch_len(rows)?;
        if scratch.len() < scratch_len {
            return Err(MlError::DimensionMismatch {
                expected: scratch_len,
                found: scratch.len(),
            });
        }
        if self.weights.iter().any(|weight| !weight.is_finite())
            || features.iter().any(|feature| !feature.is_finite())
        {
            return Err(MlError::NonFiniteValue);
        }

        // A hidden-less MLP is a linear layer and needs no scratch.
        if self.hidden_dims.is_empty() {
            let bias = self.weights[self.input_dim];
            for (row, out_slot) in out.iter_mut().take(rows).enumerate() {
                let inputs = &features[row * self.input_dim..(row + 1) * self.input_dim];
                let mut score = bias;
                for (weight, input) in self.weights[..self.input_dim].iter().zip(inputs) {
                    score = input.mul_add(*weight, score);
                }
                *out_slot = score;
            }
            return Ok(());
        }

        let max_hidden = self.max_hidden();
        for (row, out_slot) in out.iter_mut().take(rows).enumerate() {
            let inputs = &features[row * self.input_dim..(row + 1) * self.input_dim];
            let row_scratch = &mut scratch[row * max_hidden * 2..(row + 1) * max_hidden * 2];
            let (act_a, act_b) = row_scratch.split_at_mut(max_hidden);
            let mut weight_offset = 0;
            let mut input_dim = self.input_dim;
            let mut use_a = true;

            for (layer_index, &output_dim) in self.hidden_dims.iter().enumerate() {
                let weight_count = input_dim * output_dim;
                let weights = &self.weights[weight_offset..weight_offset + weight_count];
                let biases = &self.weights
                    [weight_offset + weight_count..weight_offset + weight_count + output_dim];
                for output_index in 0..output_dim {
                    let row_weights =
                        &weights[output_index * input_dim..(output_index + 1) * input_dim];
                    let mut sum = biases[output_index];
                    if layer_index == 0 {
                        for (&input, &weight) in inputs.iter().zip(row_weights) {
                            sum = input.mul_add(weight, sum);
                        }
                    } else {
                        let previous = if use_a {
                            &act_a[..input_dim]
                        } else {
                            &act_b[..input_dim]
                        };
                        for (&input, &weight) in previous.iter().zip(row_weights) {
                            sum = input.mul_add(weight, sum);
                        }
                    }
                    if use_a {
                        act_b[output_index] = sum.max(0.0);
                    } else {
                        act_a[output_index] = sum.max(0.0);
                    }
                }
                use_a = !use_a;
                weight_offset += weight_count + output_dim;
                input_dim = output_dim;
            }

            let previous = if use_a {
                &act_a[..input_dim]
            } else {
                &act_b[..input_dim]
            };
            let mut score = self.weights[weight_offset + input_dim];
            for (&activation, &weight) in previous
                .iter()
                .zip(&self.weights[weight_offset..weight_offset + input_dim])
            {
                score = activation.mul_add(weight, score);
            }
            if !score.is_finite() {
                return Err(MlError::NonFiniteValue);
            }
            *out_slot = score;
        }
        Ok(())
    }

    pub fn scratch_len(&self, rows: usize) -> Result<usize, MlError> {
        rows.checked_mul(self.max_hidden())
            .and_then(|value| value.checked_mul(2))
            .ok_or(MlError::DimensionMismatch {
                expected: usize::MAX,
                found: rows,
            })
    }

    fn max_hidden(&self) -> usize {
        self.hidden_dims.iter().copied().max().unwrap_or(0)
    }

    pub fn to_manifest(&self) -> MicroCheckpointBundle {
        let mut manifest = reflex_ml_core::ModelCheckpointManifest {
            model_id: ModelCheckpointId::from_digest(Digest::ZERO),
            spec: ModelSpec {
                role: ModelRole::Ranker,
                architecture: ModelArchitecture::Mlp {
                    hidden: self.hidden_dims.clone(),
                    activation: Activation::ReLU,
                    bias: true,
                },
                input_schema: FeatureSchemaId::from_digest(Digest::hash_blake3(b"micro-in")),
                output_schema: ModelOutputSchemaId::from_digest(Digest::hash_blake3(b"micro-out")),
                parameter_budget: ParameterBudget {
                    max_parameters: self.parameter_count() as u64,
                    max_unique_parameters: None,
                },
                dtype: ModelDType::F32,
                canonical_backend: BackendClass::Flex,
            },
            input_dim: self.input_dim,
            exact_parameter_count: self.parameter_count(),
            weights_digest: digest_f32(self.weights.as_ref()),
            optimizer_digest: None,
            rng_digest: None,
            normalization_digest: None,
            dataset_digest: None,
            sampler_digest: None,
            code_identity: None,
            provenance_digest: None,
            backend: BackendClass::Flex,
            device: "cpu-micro".to_string(),
            step: 0,
            epoch: 0,
            batch_position: 0,
            metrics_digest: Digest::hash_blake3(b"{}"),
            inference_only: true,
        };
        manifest
            .seal()
            .expect("valid micro inference checkpoint manifest");
        MicroCheckpointBundle {
            manifest,
            weights: self.weights.to_vec(),
        }
    }

    pub fn from_checkpoint(bundle: &MicroCheckpointBundle) -> Result<Self, MlError> {
        bundle.manifest.validate()?;
        validate_ranker_spec(&bundle.manifest.spec, bundle.manifest.input_dim)?;
        let digest = digest_f32(&bundle.weights);
        if digest != bundle.manifest.weights_digest {
            return Err(MlError::CheckpointVerification(
                "weights digest mismatch".into(),
            ));
        }
        Self::try_new(
            bundle.manifest.input_dim,
            match &bundle.manifest.spec.architecture {
                ModelArchitecture::Mlp { hidden, .. } => hidden.clone(),
                _ => {
                    return Err(MlError::UnsupportedArchitecture(
                        "micro checkpoint must be MLP".into(),
                    ));
                }
            },
            bundle.weights.clone().into_boxed_slice(),
            bundle.manifest.model_id,
        )
    }
}

fn checked_parameter_count(input_dim: usize, hidden_dims: &[usize]) -> Result<usize, MlError> {
    let mut count = 0usize;
    let mut previous = input_dim;
    for &hidden in hidden_dims {
        count = count
            .checked_add(previous.checked_mul(hidden).ok_or_else(|| {
                MlError::UnsupportedArchitecture("MLP parameter count overflow".into())
            })?)
            .and_then(|value| value.checked_add(hidden))
            .ok_or_else(|| {
                MlError::UnsupportedArchitecture("MLP parameter count overflow".into())
            })?;
        previous = hidden;
    }
    count
        .checked_add(previous)
        .and_then(|value| value.checked_add(1))
        .ok_or_else(|| MlError::UnsupportedArchitecture("MLP parameter count overflow".into()))
}

fn digest_f32(values: &[f32]) -> Digest {
    let mut bytes = Vec::with_capacity(values.len() * 4);
    for v in values {
        bytes.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    Digest::hash_blake3(&bytes)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MicroCheckpointBundle {
    pub manifest: reflex_ml_core::ModelCheckpointManifest,
    pub weights: Vec<f32>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MicroTrainingCheckpoint {
    pub inference: MicroCheckpointBundle,
    pub first_moment: Vec<f32>,
    pub second_moment: Vec<f32>,
    pub learning_rate: f32,
    pub weight_decay: f32,
    pub beta1: f32,
    pub beta2: f32,
    pub epsilon: f32,
    pub rng_seed: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrainingCheckpointProvenance {
    pub dataset_digest: Digest,
    pub sampler_digest: Digest,
    pub code_identity: Digest,
    pub metrics_digest: Digest,
    pub provenance_digest: Digest,
    pub epoch: u64,
    pub batch_position: u64,
}

impl Ranker for MicroMlp {
    fn model_id(&self) -> ModelCheckpointId {
        self.model_id
    }

    fn score_batch(
        &self,
        features: &FeatureBatch,
        output: &mut [f32],
        telemetry: &mut InferenceTelemetry,
    ) -> Result<(), MlError> {
        if features.cols != self.input_dim {
            return Err(MlError::DimensionMismatch {
                expected: self.input_dim,
                found: features.cols,
            });
        }
        if output.len() < features.rows {
            return Err(MlError::DimensionMismatch {
                expected: features.rows,
                found: output.len(),
            });
        }

        let start = std::time::Instant::now();
        let need = self.scratch_len(features.rows)?;
        RANKER_SCRATCH.with(|storage| {
            let mut scratch = storage.borrow_mut();
            if scratch.len() < need {
                scratch.resize(need, 0.0);
            }
            self.try_score_rows(
                &features.values,
                features.rows,
                output,
                &mut scratch[..need],
            )
        })?;
        telemetry.forward_cpu_ns += start.elapsed().as_nanos() as u64;
        telemetry.rows_scored += features.rows;
        Ok(())
    }
}

pub struct MicroTrainer {
    pub model: MicroMlp,
    pub m_weights: Vec<f32>,
    pub v_weights: Vec<f32>,
    pub step: u64,
    pub lr: f32,
    pub weight_decay: f32,
    pub beta1: f32,
    pub beta2: f32,
    pub eps: f32,
    pub rng_seed: u64,
}

impl MicroTrainer {
    pub fn new(model: MicroMlp, lr: f32, weight_decay: f32) -> Self {
        Self::try_new(model, lr, weight_decay).expect("invalid AdamW configuration")
    }

    pub fn try_new(model: MicroMlp, lr: f32, weight_decay: f32) -> Result<Self, MlError> {
        let count = model.weights.len();
        let trainer = Self {
            model,
            m_weights: vec![0.0; count],
            v_weights: vec![0.0; count],
            step: 0,
            lr,
            weight_decay,
            beta1: 0.9,
            beta2: 0.999,
            eps: 1e-8,
            rng_seed: 42,
        };
        trainer.validate()?;
        Ok(trainer)
    }

    pub fn optimizer_state_digest(&self) -> Digest {
        let mut bytes = Vec::with_capacity(
            32 + (self.m_weights.len() + self.v_weights.len()) * std::mem::size_of::<f32>(),
        );
        bytes.extend_from_slice(b"reflex.micro.adamw.v1\0");
        bytes.extend_from_slice(&self.step.to_le_bytes());
        bytes.extend_from_slice(&self.lr.to_bits().to_le_bytes());
        bytes.extend_from_slice(&self.weight_decay.to_bits().to_le_bytes());
        bytes.extend_from_slice(&self.beta1.to_bits().to_le_bytes());
        bytes.extend_from_slice(&self.beta2.to_bits().to_le_bytes());
        bytes.extend_from_slice(&self.eps.to_bits().to_le_bytes());
        bytes.extend_from_slice(&self.rng_seed.to_le_bytes());
        bytes.extend_from_slice(&(self.m_weights.len() as u64).to_le_bytes());
        append_f32_bytes(&mut bytes, &self.m_weights);
        append_f32_bytes(&mut bytes, &self.v_weights);
        Digest::hash_blake3(&bytes)
    }

    pub fn rng_state_digest(&self) -> Digest {
        let mut bytes = Vec::with_capacity(40);
        bytes.extend_from_slice(b"reflex.micro.rng.v1\0");
        bytes.extend_from_slice(&self.rng_seed.to_le_bytes());
        Digest::hash_blake3(&bytes)
    }

    pub fn to_training_checkpoint(
        &self,
        provenance: TrainingCheckpointProvenance,
    ) -> Result<MicroTrainingCheckpoint, MlError> {
        self.validate()?;
        if [
            provenance.dataset_digest,
            provenance.sampler_digest,
            provenance.code_identity,
            provenance.metrics_digest,
            provenance.provenance_digest,
        ]
        .contains(&Digest::ZERO)
        {
            return Err(MlError::CheckpointVerification(
                "training checkpoint provenance digests must be non-zero".into(),
            ));
        }
        let mut inference = self.model.to_manifest();
        inference.manifest.optimizer_digest = Some(self.optimizer_state_digest());
        inference.manifest.rng_digest = Some(self.rng_state_digest());
        inference.manifest.dataset_digest = Some(provenance.dataset_digest);
        inference.manifest.sampler_digest = Some(provenance.sampler_digest);
        inference.manifest.code_identity = Some(provenance.code_identity);
        inference.manifest.provenance_digest = Some(provenance.provenance_digest);
        inference.manifest.step = self.step;
        inference.manifest.epoch = provenance.epoch;
        inference.manifest.batch_position = provenance.batch_position;
        inference.manifest.metrics_digest = provenance.metrics_digest;
        inference.manifest.seal()?;
        inference.manifest.require_resumable()?;
        Ok(MicroTrainingCheckpoint {
            inference,
            first_moment: self.m_weights.clone(),
            second_moment: self.v_weights.clone(),
            learning_rate: self.lr,
            weight_decay: self.weight_decay,
            beta1: self.beta1,
            beta2: self.beta2,
            epsilon: self.eps,
            rng_seed: self.rng_seed,
        })
    }

    pub fn from_training_checkpoint(checkpoint: &MicroTrainingCheckpoint) -> Result<Self, MlError> {
        checkpoint.inference.manifest.require_resumable()?;
        let model = MicroMlp::from_checkpoint(&checkpoint.inference)?;
        let trainer = Self {
            model,
            m_weights: checkpoint.first_moment.clone(),
            v_weights: checkpoint.second_moment.clone(),
            step: checkpoint.inference.manifest.step,
            lr: checkpoint.learning_rate,
            weight_decay: checkpoint.weight_decay,
            beta1: checkpoint.beta1,
            beta2: checkpoint.beta2,
            eps: checkpoint.epsilon,
            rng_seed: checkpoint.rng_seed,
        };
        trainer.validate()?;
        if Some(trainer.optimizer_state_digest()) != checkpoint.inference.manifest.optimizer_digest
            || Some(trainer.rng_state_digest()) != checkpoint.inference.manifest.rng_digest
        {
            return Err(MlError::CheckpointVerification(
                "optimizer or RNG state digest mismatch".into(),
            ));
        }
        Ok(trainer)
    }

    pub fn validate(&self) -> Result<(), MlError> {
        if !self.lr.is_finite()
            || self.lr <= 0.0
            || !self.weight_decay.is_finite()
            || self.weight_decay < 0.0
            || !self.beta1.is_finite()
            || !(0.0..1.0).contains(&self.beta1)
            || !self.beta2.is_finite()
            || !(0.0..1.0).contains(&self.beta2)
            || !self.eps.is_finite()
            || self.eps <= 0.0
        {
            return Err(MlError::CheckpointVerification(
                "invalid AdamW hyperparameters".into(),
            ));
        }
        if self.m_weights.len() != self.model.weights.len()
            || self.v_weights.len() != self.model.weights.len()
        {
            return Err(MlError::CheckpointVerification(
                "optimizer state dimension mismatch".into(),
            ));
        }
        if self.model.weights.iter().any(|value| !value.is_finite())
            || self.m_weights.iter().any(|value| !value.is_finite())
            || self.v_weights.iter().any(|value| !value.is_finite())
        {
            return Err(MlError::NonFiniteValue);
        }
        Ok(())
    }

    /// Compatibility wrapper. New code should use `try_train_step_pairwise` so
    /// malformed supervision is reported rather than represented by NaN.
    pub fn train_step_pairwise(
        &mut self,
        features: &[f32],
        better_idx: usize,
        worse_idx: usize,
    ) -> f32 {
        self.try_train_step_pairwise(features, better_idx, worse_idx)
            .unwrap_or(f32::NAN)
    }

    pub fn try_train_step_pairwise(
        &mut self,
        features: &[f32],
        better_idx: usize,
        worse_idx: usize,
    ) -> Result<f32, MlError> {
        self.validate()?;
        if better_idx == worse_idx {
            return Err(MlError::CheckpointVerification(
                "pairwise supervision must reference two distinct rows".into(),
            ));
        }
        let rows = features.len() / self.model.input_dim;
        if !features.len().is_multiple_of(self.model.input_dim)
            || better_idx >= rows
            || worse_idx >= rows
        {
            return Err(MlError::DimensionMismatch {
                expected: (better_idx.max(worse_idx) + 1) * self.model.input_dim,
                found: features.len(),
            });
        }
        if features.iter().any(|value| !value.is_finite()) {
            return Err(MlError::NonFiniteValue);
        }

        let better =
            &features[better_idx * self.model.input_dim..(better_idx + 1) * self.model.input_dim];
        let worse =
            &features[worse_idx * self.model.input_dim..(worse_idx + 1) * self.model.input_dim];
        let (loss, gradients) = pairwise_gradients(&self.model, better, worse)?;

        let next_step = self
            .step
            .checked_add(1)
            .ok_or_else(|| MlError::CheckpointVerification("optimizer step overflow".into()))?;
        let beta1_correction = 1.0 - self.beta1.powf(next_step as f32);
        let beta2_correction = 1.0 - self.beta2.powf(next_step as f32);
        let mut next_weights = self.model.weights.to_vec();
        let mut next_first_moment = self.m_weights.clone();
        let mut next_second_moment = self.v_weights.clone();
        for index in 0..next_weights.len() {
            let gradient = gradients[index];
            next_first_moment[index] = self
                .beta1
                .mul_add(next_first_moment[index], (1.0 - self.beta1) * gradient);
            next_second_moment[index] = self.beta2.mul_add(
                next_second_moment[index],
                (1.0 - self.beta2) * gradient * gradient,
            );
            let corrected_moment = next_first_moment[index] / beta1_correction;
            let corrected_variance = next_second_moment[index] / beta2_correction;
            let adam_update = corrected_moment / (corrected_variance.sqrt() + self.eps);
            next_weights[index] -=
                self.lr * (adam_update + self.weight_decay * next_weights[index]);
            if !next_weights[index].is_finite()
                || !next_first_moment[index].is_finite()
                || !next_second_moment[index].is_finite()
            {
                return Err(MlError::NonFiniteValue);
            }
        }
        self.model.weights.copy_from_slice(&next_weights);
        self.m_weights = next_first_moment;
        self.v_weights = next_second_moment;
        self.step = next_step;

        let mut identity = Vec::with_capacity(72);
        identity.extend_from_slice(b"reflex.micro.checkpoint.v1\0");
        identity.extend_from_slice(digest_f32(&self.model.weights).as_bytes());
        identity.extend_from_slice(self.optimizer_state_digest().as_bytes());
        identity.extend_from_slice(&self.step.to_le_bytes());
        self.model.model_id = ModelCheckpointId::from_digest(Digest::hash_blake3(&identity));
        Ok(loss)
    }
}

fn append_f32_bytes(bytes: &mut Vec<u8>, values: &[f32]) {
    for value in values {
        bytes.extend_from_slice(&value.to_bits().to_le_bytes());
    }
}

#[derive(Debug)]
struct ForwardPass {
    activations: Vec<Vec<f32>>,
    score: f32,
}

fn forward_training(model: &MicroMlp, input: &[f32]) -> Result<ForwardPass, MlError> {
    if input.len() != model.input_dim {
        return Err(MlError::DimensionMismatch {
            expected: model.input_dim,
            found: input.len(),
        });
    }
    let mut activations = Vec::with_capacity(model.hidden_dims.len() + 1);
    activations.push(input.to_vec());
    let mut offset = 0;
    let mut input_dim = model.input_dim;
    for &output_dim in &model.hidden_dims {
        let weight_count = input_dim * output_dim;
        let weights = &model.weights[offset..offset + weight_count];
        let biases = &model.weights[offset + weight_count..offset + weight_count + output_dim];
        let previous = activations.last().expect("input activation exists");
        let mut output = vec![0.0; output_dim];
        for output_index in 0..output_dim {
            let mut sum = biases[output_index];
            let row_weights = &weights[output_index * input_dim..(output_index + 1) * input_dim];
            for (&activation, &weight) in previous.iter().zip(row_weights) {
                sum = activation.mul_add(weight, sum);
            }
            output[output_index] = sum.max(0.0);
        }
        activations.push(output);
        offset += weight_count + output_dim;
        input_dim = output_dim;
    }
    let previous = activations.last().expect("input activation exists");
    let mut score = model.weights[offset + input_dim];
    for (&activation, &weight) in previous
        .iter()
        .zip(&model.weights[offset..offset + input_dim])
    {
        score = activation.mul_add(weight, score);
    }
    if !score.is_finite() {
        return Err(MlError::NonFiniteValue);
    }
    Ok(ForwardPass { activations, score })
}

fn accumulate_score_gradient(
    model: &MicroMlp,
    pass: &ForwardPass,
    score_gradient: f32,
    gradients: &mut [f32],
) {
    let mut offsets = Vec::with_capacity(model.hidden_dims.len());
    let mut offset = 0;
    let mut input_dim = model.input_dim;
    for &output_dim in &model.hidden_dims {
        offsets.push(offset);
        offset += input_dim * output_dim + output_dim;
        input_dim = output_dim;
    }

    let last_activation = pass.activations.last().expect("input activation exists");
    for (index, &activation) in last_activation.iter().enumerate() {
        gradients[offset + index] += score_gradient * activation;
    }
    gradients[offset + input_dim] += score_gradient;

    if model.hidden_dims.is_empty() {
        return;
    }
    let mut delta: Vec<f32> = model.weights[offset..offset + input_dim]
        .iter()
        .zip(last_activation)
        .map(|(&weight, &activation)| {
            if activation > 0.0 {
                score_gradient * weight
            } else {
                0.0
            }
        })
        .collect();

    for layer_index in (0..model.hidden_dims.len()).rev() {
        let output_dim = model.hidden_dims[layer_index];
        let input_dim = if layer_index == 0 {
            model.input_dim
        } else {
            model.hidden_dims[layer_index - 1]
        };
        let layer_offset = offsets[layer_index];
        let previous = &pass.activations[layer_index];
        for (output_index, &output_delta) in delta.iter().enumerate().take(output_dim) {
            for input_index in 0..input_dim {
                gradients[layer_offset + output_index * input_dim + input_index] +=
                    output_delta * previous[input_index];
            }
            gradients[layer_offset + input_dim * output_dim + output_index] += output_delta;
        }

        if layer_index > 0 {
            let mut previous_delta = vec![0.0; input_dim];
            for input_index in 0..input_dim {
                let mut sum = 0.0;
                for (output_index, &output_delta) in delta.iter().enumerate().take(output_dim) {
                    sum = model.weights[layer_offset + output_index * input_dim + input_index]
                        .mul_add(output_delta, sum);
                }
                previous_delta[input_index] = if previous[input_index] > 0.0 {
                    sum
                } else {
                    0.0
                };
            }
            delta = previous_delta;
        }
    }
}

fn pairwise_gradients(
    model: &MicroMlp,
    better: &[f32],
    worse: &[f32],
) -> Result<(f32, Vec<f32>), MlError> {
    let better_pass = forward_training(model, better)?;
    let worse_pass = forward_training(model, worse)?;
    let margin = better_pass.score - worse_pass.score;
    if !margin.is_finite() {
        return Err(MlError::NonFiniteValue);
    }
    let negative_margin = -margin;
    let loss = if negative_margin > 0.0 {
        negative_margin + (-negative_margin).exp().ln_1p()
    } else {
        negative_margin.exp().ln_1p()
    };
    let margin_gradient = if margin >= 0.0 {
        let exp_negative = (-margin).exp();
        -exp_negative / (1.0 + exp_negative)
    } else {
        -1.0 / (1.0 + margin.exp())
    };
    let mut gradients = vec![0.0; model.weights.len()];
    accumulate_score_gradient(model, &better_pass, margin_gradient, &mut gradients);
    accumulate_score_gradient(model, &worse_pass, -margin_gradient, &mut gradients);
    if !loss.is_finite() || gradients.iter().any(|gradient| !gradient.is_finite()) {
        return Err(MlError::NonFiniteValue);
    }
    Ok((loss, gradients))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_types::FeatureSchemaId;

    #[test]
    fn test_micro_mlp_inference() {
        let mlp = MicroMlp::reference_mlp_2607(42);
        assert_eq!(mlp.parameter_count(), 2607);
        let features = vec![0.5f32; 64 * 4];
        let mut out = vec![0.0f32; 4];
        let mut scratch = vec![0.0f32; 4 * 49 * 2];
        mlp.score_rows(&features, 4, &mut out, &mut scratch);
        for &s in &out {
            assert!(s.is_finite());
        }
    }

    #[test]
    fn test_dimension_mismatch() {
        let mlp = MicroMlp::random(16, 8, 1);
        let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(b"t"));
        let fb = FeatureBatch::new(2, 32, schema);
        let mut out = vec![0.0; 2];
        let mut tel = InferenceTelemetry::default();
        let err = mlp.score_batch(&fb, &mut out, &mut tel);
        assert!(matches!(err, Err(MlError::DimensionMismatch { .. })));
    }

    #[test]
    fn test_inference_engine() {
        let mlp = MicroMlp::reference_mlp_2607(7);
        let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(b"t"));
        let mut fb = FeatureBatch::new(8, 64, schema);
        fb.values.fill(0.25);
        let mut out = vec![0.0; 8];
        let mut tel = InferenceTelemetry::default();
        mlp.score_batch(&fb, &mut out, &mut tel).unwrap();
        assert!(out.iter().all(|v| v.is_finite()));
        assert_eq!(tel.rows_scored, 8);
    }

    #[test]
    fn test_micro_mlp_training_convergence() {
        let mlp = MicroMlp::random(4, 8, 123);
        let mut trainer = MicroTrainer::new(mlp, 0.05, 0.001);
        let features = vec![1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0];
        let initial = trainer.try_train_step_pairwise(&features, 0, 1).unwrap();
        let mut final_loss = initial;
        for _ in 0..99 {
            final_loss = trainer.try_train_step_pairwise(&features, 0, 1).unwrap();
        }
        assert!(initial.is_finite());
        assert!(final_loss.is_finite());
        assert!(final_loss < initial * 0.2, "{initial} -> {final_loss}");
        assert!(trainer.m_weights.iter().any(|moment| *moment != 0.0));
        assert!(trainer.v_weights.iter().any(|moment| *moment > 0.0));
    }

    #[test]
    fn test_checkpoint_roundtrip() {
        let mlp = MicroMlp::reference_mlp_2607(99);
        let bundle = mlp.to_manifest();
        let loaded = MicroMlp::from_checkpoint(&bundle).unwrap();
        assert_eq!(loaded.weights.len(), mlp.weights.len());
        for (a, b) in loaded.weights.iter().zip(mlp.weights.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn hiddenless_model_is_linear_and_needs_no_scratch() {
        let model_id = ModelCheckpointId::from_digest(Digest::hash_blake3(b"linear"));
        let model = MicroMlp::try_new(2, vec![], vec![2.0, -3.0, 0.5].into(), model_id).unwrap();
        let mut output = [0.0];
        model
            .try_score_rows(&[4.0, 2.0], 1, &mut output, &mut [])
            .unwrap();
        assert_eq!(output[0], 2.5);
    }

    #[test]
    fn pairwise_backprop_matches_finite_difference_for_deep_mlp() {
        let model = MicroMlp::random_with_hidden(3, vec![4, 3, 2], 91);
        let better = [0.8, -0.2, 0.5];
        let worse = [-0.4, 0.7, -0.1];
        let (_, analytic) = pairwise_gradients(&model, &better, &worse).unwrap();
        let epsilon = 1e-3;
        for (index, &analytic_gradient) in analytic.iter().enumerate() {
            let mut plus = model.clone();
            plus.weights[index] += epsilon;
            let plus_loss = pairwise_gradients(&plus, &better, &worse).unwrap().0;
            let mut minus = model.clone();
            minus.weights[index] -= epsilon;
            let minus_loss = pairwise_gradients(&minus, &better, &worse).unwrap().0;
            let numeric = (plus_loss - minus_loss) / (2.0 * epsilon);
            assert!(
                (analytic_gradient - numeric).abs() < 3e-3,
                "gradient {index}: analytic={}, numeric={numeric}",
                analytic_gradient
            );
        }
    }

    #[test]
    fn optimizer_digest_commits_to_all_state() {
        let model = MicroMlp::random(2, 3, 5);
        let mut trainer = MicroTrainer::new(model, 0.01, 0.001);
        let initial = trainer.optimizer_state_digest();
        trainer.v_weights[0] = 1.0;
        assert_ne!(initial, trainer.optimizer_state_digest());
        let with_variance = trainer.optimizer_state_digest();
        trainer.step = 1;
        assert_ne!(with_variance, trainer.optimizer_state_digest());
    }

    #[test]
    fn first_adamw_step_uses_bias_corrected_gradient() {
        let model_id = ModelCheckpointId::from_digest(Digest::hash_blake3(b"adam-step"));
        let model = MicroMlp::try_new(2, vec![], vec![0.0, 0.0, 0.0].into(), model_id).unwrap();
        let mut trainer = MicroTrainer::new(model, 0.01, 0.0);
        trainer
            .try_train_step_pairwise(&[1.0, 0.0, 0.0, 0.0], 0, 1)
            .unwrap();
        assert!((trainer.model.weights[0] - 0.01).abs() < 1e-6);
        assert_eq!(trainer.model.weights[1], 0.0);
        // The shared output bias has equal-and-opposite pairwise gradients.
        assert_eq!(trainer.model.weights[2], 0.0);
        assert!((trainer.m_weights[0] + 0.05).abs() < 1e-7);
        assert!((trainer.v_weights[0] - 0.000_25).abs() < 1e-8);
    }

    #[test]
    fn seeded_training_is_bit_deterministic() {
        let features = [0.7, -0.2, 0.4, -0.5, 0.1, 0.9];
        let mut first = MicroTrainer::new(
            MicroMlp::random_with_hidden(3, vec![5, 4], 777),
            0.003,
            0.0001,
        );
        let mut second = MicroTrainer::new(
            MicroMlp::random_with_hidden(3, vec![5, 4], 777),
            0.003,
            0.0001,
        );
        for _ in 0..32 {
            assert_eq!(
                first.try_train_step_pairwise(&features, 0, 1).unwrap(),
                second.try_train_step_pairwise(&features, 0, 1).unwrap()
            );
        }
        assert_eq!(first.model.weights, second.model.weights);
        assert_eq!(first.m_weights, second.m_weights);
        assert_eq!(first.v_weights, second.v_weights);
        assert_eq!(
            first.optimizer_state_digest(),
            second.optimizer_state_digest()
        );
        assert_eq!(first.model.model_id, second.model.model_id);
    }

    #[test]
    fn invalid_model_and_optimizer_configuration_fail_closed() {
        let id = ModelCheckpointId::from_digest(Digest::hash_blake3(b"invalid"));
        assert!(MicroMlp::try_new(0, vec![2], vec![0.0; 5].into(), id).is_err());
        let model = MicroMlp::random(2, 3, 1);
        assert!(MicroTrainer::try_new(model, f32::NAN, 0.0).is_err());
    }

    #[test]
    fn training_checkpoint_restores_optimizer_rng_and_next_step_bits() {
        let features = [0.7, -0.2, 0.4, -0.5, 0.1, 0.9];
        let mut uninterrupted = MicroTrainer::new(
            MicroMlp::random_with_hidden(3, vec![5, 4], 777),
            0.003,
            0.0001,
        );
        for _ in 0..8 {
            uninterrupted
                .try_train_step_pairwise(&features, 0, 1)
                .unwrap();
        }
        let checkpoint = uninterrupted
            .to_training_checkpoint(TrainingCheckpointProvenance {
                dataset_digest: Digest::hash_blake3(b"dataset"),
                sampler_digest: Digest::hash_blake3(b"sampler"),
                code_identity: Digest::hash_blake3(b"code"),
                metrics_digest: Digest::hash_blake3(b"metrics"),
                provenance_digest: Digest::hash_blake3(b"provenance"),
                epoch: 2,
                batch_position: 17,
            })
            .unwrap();
        let mut resumed = MicroTrainer::from_training_checkpoint(&checkpoint).unwrap();
        assert_eq!(
            uninterrupted
                .try_train_step_pairwise(&features, 0, 1)
                .unwrap(),
            resumed.try_train_step_pairwise(&features, 0, 1).unwrap()
        );
        assert_eq!(uninterrupted.model.weights, resumed.model.weights);
        assert_eq!(uninterrupted.m_weights, resumed.m_weights);
        assert_eq!(uninterrupted.v_weights, resumed.v_weights);

        let mut tampered = checkpoint;
        tampered.first_moment[0] += 1.0;
        assert!(MicroTrainer::from_training_checkpoint(&tampered).is_err());
    }

    #[test]
    fn concurrent_ranker_batches_match_single_threaded_results() {
        use std::sync::{Arc, Barrier};

        let model = Arc::new(MicroMlp::random_with_hidden(16, vec![12, 7], 1234));
        let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(b"concurrent"));
        let mut batch = FeatureBatch::new(32, 16, schema);
        for (index, value) in batch.values.iter_mut().enumerate() {
            *value = (index as f32 * 0.017).sin();
        }
        let mut expected = vec![0.0; batch.rows];
        model
            .score_batch(&batch, &mut expected, &mut InferenceTelemetry::default())
            .unwrap();

        let batch = Arc::new(batch);
        let barrier = Arc::new(Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let model = Arc::clone(&model);
                let batch = Arc::clone(&batch);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    let mut output = vec![0.0; batch.rows];
                    let mut telemetry = InferenceTelemetry::default();
                    for _ in 0..16 {
                        model
                            .score_batch(&batch, &mut output, &mut telemetry)
                            .unwrap();
                    }
                    (output, telemetry.rows_scored)
                })
            })
            .collect();
        for handle in handles {
            let (actual, rows_scored) = handle.join().unwrap();
            assert_eq!(actual, expected);
            assert_eq!(rows_scored, 16 * batch.rows);
        }
    }
}
