use reflex_domain::FeatureBatch;
use reflex_ml_core::{InferenceTelemetry, MlError, Ranker};
use reflex_types::{Digest, ModelCheckpointId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MicroMlp {
    pub input_dim: usize,
    pub hidden_dim: usize,
    pub w1: Vec<f32>, // shape: (hidden_dim, input_dim)
    pub b1: Vec<f32>, // shape: (hidden_dim)
    pub w2: Vec<f32>, // shape: (hidden_dim)
    pub b2: f32,
    pub model_id: ModelCheckpointId,
}

impl MicroMlp {
    pub fn new(
        input_dim: usize,
        hidden_dim: usize,
        w1: Vec<f32>,
        b1: Vec<f32>,
        w2: Vec<f32>,
        b2: f32,
        model_id: ModelCheckpointId,
    ) -> Self {
        assert_eq!(w1.len(), hidden_dim * input_dim);
        assert_eq!(b1.len(), hidden_dim);
        assert_eq!(w2.len(), hidden_dim);
        Self {
            input_dim,
            hidden_dim,
            w1,
            b1,
            w2,
            b2,
            model_id,
        }
    }

    pub fn random(input_dim: usize, hidden_dim: usize, seed: u64) -> Self {
        use rand::Rng;
        use rand::SeedableRng;
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);

        let scale1 = (2.0 / input_dim as f32).sqrt();
        let w1: Vec<f32> = (0..(hidden_dim * input_dim))
            .map(|_| rng.gen_range(-scale1..scale1))
            .collect();
        let b1 = vec![0.0f32; hidden_dim];

        let scale2 = (2.0 / hidden_dim as f32).sqrt();
        let w2: Vec<f32> = (0..hidden_dim)
            .map(|_| rng.gen_range(-scale2..scale2))
            .collect();
        let b2 = 0.0f32;

        let model_id = ModelCheckpointId::from_digest(Digest::hash_blake3(
            &format!("micro-mlp-{input_dim}-{hidden_dim}-{seed}").into_bytes(),
        ));

        Self::new(input_dim, hidden_dim, w1, b1, w2, b2, model_id)
    }

    #[inline]
    pub fn score_rows(&self, features: &[f32], rows: usize, out: &mut [f32], scratch: &mut [f32]) {
        assert_eq!(features.len(), rows * self.input_dim);
        assert!(scratch.len() >= rows * self.hidden_dim);
        assert!(out.len() >= rows);

        for row in 0..rows {
            let x = &features[row * self.input_dim..(row + 1) * self.input_dim];
            let h = &mut scratch[row * self.hidden_dim..(row + 1) * self.hidden_dim];
            for (j, h_val) in h.iter_mut().enumerate().take(self.hidden_dim) {
                let mut sum = self.b1[j];
                let w = &self.w1[j * self.input_dim..(j + 1) * self.input_dim];
                for (k, &x_k) in x.iter().enumerate().take(self.input_dim) {
                    sum = x_k.mul_add(w[k], sum);
                }
                // ReLU
                *h_val = sum.max(0.0);
            }
            let mut score = self.b2;
            for (j, &h_j) in h.iter().enumerate().take(self.hidden_dim) {
                score = h_j.mul_add(self.w2[j], score);
            }
            out[row] = score;
        }
    }
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
        let mut scratch = vec![0.0f32; features.rows * self.hidden_dim];
        self.score_rows(&features.values, features.rows, output, &mut scratch);
        telemetry.forward_cpu_ns += start.elapsed().as_nanos() as u64;
        telemetry.rows_scored += features.rows;

        Ok(())
    }
}

pub struct MicroTrainer {
    pub model: MicroMlp,
    pub m_w1: Vec<f32>,
    pub v_w1: Vec<f32>,
    pub m_b1: Vec<f32>,
    pub v_b1: Vec<f32>,
    pub m_w2: Vec<f32>,
    pub v_w2: Vec<f32>,
    pub m_b2: f32,
    pub v_b2: f32,
    pub step: u64,
    pub lr: f32,
    pub weight_decay: f32,
    pub beta1: f32,
    pub beta2: f32,
    pub eps: f32,
}

impl MicroTrainer {
    pub fn new(model: MicroMlp, lr: f32, weight_decay: f32) -> Self {
        let h = model.hidden_dim;
        let in_d = model.input_dim;
        Self {
            model,
            m_w1: vec![0.0; h * in_d],
            v_w1: vec![0.0; h * in_d],
            m_b1: vec![0.0; h],
            v_b1: vec![0.0; h],
            m_w2: vec![0.0; h],
            v_w2: vec![0.0; h],
            m_b2: 0.0,
            v_b2: 0.0,
            step: 0,
            lr,
            weight_decay,
            beta1: 0.9,
            beta2: 0.999,
            eps: 1e-8,
        }
    }

    pub fn train_step_pairwise(
        &mut self,
        features: &[f32],
        better_idx: usize,
        worse_idx: usize,
    ) -> f32 {
        let in_d = self.model.input_dim;
        let h_d = self.model.hidden_dim;

        // Forward better
        let x_b = &features[better_idx * in_d..(better_idx + 1) * in_d];
        let mut h_b = vec![0.0; h_d];
        let mut pre_b = vec![0.0; h_d];
        for (j, h_b_val) in h_b.iter_mut().enumerate().take(h_d) {
            let mut sum = self.model.b1[j];
            for (k, &x_k) in x_b.iter().enumerate().take(in_d) {
                sum += x_k * self.model.w1[j * in_d + k];
            }
            pre_b[j] = sum;
            *h_b_val = sum.max(0.0);
        }
        let mut s_b = self.model.b2;
        for (j, &h_val) in h_b.iter().enumerate().take(h_d) {
            s_b += h_val * self.model.w2[j];
        }

        // Forward worse
        let x_w = &features[worse_idx * in_d..(worse_idx + 1) * in_d];
        let mut h_w = vec![0.0; h_d];
        let mut pre_w = vec![0.0; h_d];
        for (j, h_w_val) in h_w.iter_mut().enumerate().take(h_d) {
            let mut sum = self.model.b1[j];
            for (k, &x_k) in x_w.iter().enumerate().take(in_d) {
                sum += x_k * self.model.w1[j * in_d + k];
            }
            pre_w[j] = sum;
            *h_w_val = sum.max(0.0);
        }
        let mut s_w = self.model.b2;
        for (j, &h_val) in h_w.iter().enumerate().take(h_d) {
            s_w += h_val * self.model.w2[j];
        }

        let margin = s_b - s_w;
        let loss = (1.0 + (-margin).exp()).ln();
        let grad_loss = -1.0 / (1.0 + margin.exp()); // dLoss/dMargin

        // Gradients w.r.t s_b is grad_loss, w.r.t s_w is -grad_loss
        let mut grad_w2 = vec![0.0; h_d];
        let mut grad_w1 = vec![0.0; h_d * in_d];
        let mut grad_b1 = vec![0.0; h_d];

        for j in 0..h_d {
            grad_w2[j] += grad_loss * h_b[j] - grad_loss * h_w[j];
        }

        for j in 0..h_d {
            let d_hb = if pre_b[j] > 0.0 {
                grad_loss * self.model.w2[j]
            } else {
                0.0
            };
            let d_hw = if pre_w[j] > 0.0 {
                -grad_loss * self.model.w2[j]
            } else {
                0.0
            };

            grad_b1[j] += d_hb + d_hw;
            for k in 0..in_d {
                grad_w1[j * in_d + k] += d_hb * x_b[k] + d_hw * x_w[k];
            }
        }

        // AdamW update
        self.step += 1;
        let t = self.step as f32;
        let lr_t = self.lr * (1.0 - self.beta2.powf(t)).sqrt() / (1.0 - self.beta1.powf(t));

        for (i, w1_val) in self.model.w1.iter_mut().enumerate() {
            let g = grad_w1[i] + self.weight_decay * *w1_val;
            self.m_w1[i] = self.beta1 * self.m_w1[i] + (1.0 - self.beta1) * g;
            self.v_w1[i] = self.beta2 * self.v_w1[i] + (1.0 - self.beta2) * g * g;
            *w1_val -= lr_t * self.m_w1[i] / (self.v_w1[i].sqrt() + self.eps);
        }

        for (i, b1_val) in self.model.b1.iter_mut().enumerate() {
            let g = grad_b1[i];
            self.m_b1[i] = self.beta1 * self.m_b1[i] + (1.0 - self.beta1) * g;
            self.v_b1[i] = self.beta2 * self.v_b1[i] + (1.0 - self.beta2) * g * g;
            *b1_val -= lr_t * self.m_b1[i] / (self.v_b1[i].sqrt() + self.eps);
        }

        for (i, w2_val) in self.model.w2.iter_mut().enumerate() {
            let g = grad_w2[i] + self.weight_decay * *w2_val;
            self.m_w2[i] = self.beta1 * self.m_w2[i] + (1.0 - self.beta1) * g;
            self.v_w2[i] = self.beta2 * self.v_w2[i] + (1.0 - self.beta2) * g * g;
            *w2_val -= lr_t * self.m_w2[i] / (self.v_w2[i].sqrt() + self.eps);
        }

        loss
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_micro_mlp_inference() {
        let mlp = MicroMlp::random(8, 16, 42);
        let features = vec![0.5f32; 8 * 4];
        let mut out = vec![0.0f32; 4];
        let mut scratch = vec![0.0f32; 4 * 16];

        mlp.score_rows(&features, 4, &mut out, &mut scratch);
        for &s in &out {
            assert!(s.is_finite());
        }
    }

    #[test]
    fn test_micro_mlp_training_convergence() {
        let mlp = MicroMlp::random(4, 8, 123);
        let mut trainer = MicroTrainer::new(mlp, 0.05, 0.001);

        // Feature vector 0 should rank above feature vector 1
        let features = vec![
            1.0, 1.0, 1.0, 1.0, // better (index 0)
            -1.0, -1.0, -1.0, -1.0, // worse (index 1)
        ];

        let mut initial_loss = 0.0;
        let mut final_loss = 0.0;

        for step in 0..100 {
            let l = trainer.train_step_pairwise(&features, 0, 1);
            if step == 0 {
                initial_loss = l;
            }
            final_loss = l;
        }

        assert!(final_loss < initial_loss);
    }
}
