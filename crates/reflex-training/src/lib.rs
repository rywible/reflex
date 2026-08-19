use burn::nn;
use burn::optim::{AdamWConfig, GradientsParams, ModuleOptimizer};
use burn::prelude::Module;
use burn::tensor::{Device, Int, Tensor, TensorData};
use reflex_dataset::{CandidateKnowledge, DecisionGroup};
use reflex_ml_burn::{BurnRankMlp, RankMlp};
use reflex_ml_micro::{MicroMlp, MicroTrainer};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum TrainingError {
    #[error("non-finite loss encountered at epoch {epoch}, batch {batch}")]
    NonFiniteLoss { epoch: usize, batch: usize },
    #[error("dataset empty or insufficient labels")]
    InsufficientData,
    #[error("training cancelled")]
    Cancelled,
    #[error("training config error: {0}")]
    Config(String),
}

pub fn pairwise_logistic(scores: &[f32], pairs: &[(usize, usize, f32)]) -> f32 {
    let mut loss = 0.0;
    let mut weight_sum = 0.0;
    for &(better, worse, weight) in pairs {
        let margin = scores[better] - scores[worse];
        loss += weight * (1.0 + (-margin).exp()).ln();
        weight_sum += weight;
    }
    if weight_sum == 0.0 {
        0.0
    } else {
        loss / weight_sum
    }
}

pub fn viable_target(
    labels: &[CandidateKnowledge],
    temperature: f32,
    out: &mut [f32],
) -> Result<bool, TrainingError> {
    if out.len() < labels.len() {
        return Err(TrainingError::Config(format!(
            "output buffer length {} is smaller than candidate count {}",
            out.len(),
            labels.len()
        )));
    }
    out.fill(0.0);
    let mut max_logit = f32::NEG_INFINITY;
    for label in labels {
        if let CandidateKnowledge::Viable {
            best_actions_to_go, ..
        } = label
        {
            max_logit = max_logit.max(-(*best_actions_to_go as f32) / temperature);
        }
    }
    if !max_logit.is_finite() {
        return Ok(false);
    }
    let mut sum = 0.0;
    for (index, label) in labels.iter().enumerate() {
        if let CandidateKnowledge::Viable {
            best_actions_to_go, ..
        } = label
        {
            let weight = ((-(*best_actions_to_go as f32) / temperature) - max_logit).exp();
            out[index] = weight;
            sum += weight;
        }
    }
    if sum > 0.0 {
        for value in out.iter_mut() {
            *value /= sum;
        }
        Ok(true)
    } else {
        Ok(false)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrainingConfig {
    pub epochs: usize,
    pub learning_rate: f32,
    pub weight_decay: f32,
    pub temperature: f32,
    pub seed: u64,
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            epochs: 10,
            learning_rate: 0.001,
            weight_decay: 0.0001,
            temperature: 1.0,
            seed: 42,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrainingMetrics {
    pub epoch_losses: Vec<f32>,
    pub final_loss: f32,
    pub total_steps: usize,
}

pub fn train_micro_model(
    mut trainer: MicroTrainer,
    groups: &[DecisionGroup],
    features_by_state: &HashMap<reflex_types::StateId, Vec<f32>>,
    config: &TrainingConfig,
) -> Result<(MicroMlp, TrainingMetrics), TrainingError> {
    if groups.is_empty() {
        return Err(TrainingError::InsufficientData);
    }

    let mut epoch_losses = Vec::new();
    let mut total_steps = 0;

    for epoch in 0..config.epochs {
        let mut epoch_loss_sum = 0.0;
        let mut step_count = 0;

        for group in groups {
            let Some(feats) = features_by_state.get(&group.state_id) else {
                continue;
            };

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
                        ) if c1 < c2 => {
                            pairs.push((i, j));
                        }
                        (
                            CandidateKnowledge::Viable { .. },
                            CandidateKnowledge::KnownDead { .. } | CandidateKnowledge::Unknown,
                        ) => {
                            pairs.push((i, j));
                        }
                        _ => {}
                    }
                }
            }

            for (better, worse) in pairs {
                let loss = trainer.train_step_pairwise(feats, better, worse);
                if !loss.is_finite() {
                    return Err(TrainingError::NonFiniteLoss {
                        epoch,
                        batch: total_steps,
                    });
                }
                epoch_loss_sum += loss;
                step_count += 1;
                total_steps += 1;
            }
        }

        let avg_epoch_loss = if step_count > 0 {
            epoch_loss_sum / step_count as f32
        } else {
            0.0
        };
        epoch_losses.push(avg_epoch_loss);
    }

    let final_loss = epoch_losses.last().copied().unwrap_or(0.0);
    Ok((
        trainer.model,
        TrainingMetrics {
            epoch_losses,
            final_loss,
            total_steps,
        },
    ))
}

pub fn train_burn_model(
    burn_model: BurnRankMlp,
    groups: &[DecisionGroup],
    features_by_state: &HashMap<reflex_types::StateId, Vec<f32>>,
    config: &TrainingConfig,
) -> Result<(BurnRankMlp, TrainingMetrics), TrainingError> {
    if groups.is_empty() {
        return Err(TrainingError::InsufficientData);
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
    let mut optimizer: ModuleOptimizer = optim_config.init().into();

    let mut epoch_losses = Vec::new();
    let mut total_steps = 0;

    for epoch in 0..config.epochs {
        let mut epoch_loss_sum = 0.0f32;
        let mut step_count = 0;

        for group in groups {
            let Some(feats) = features_by_state.get(&group.state_id) else {
                continue;
            };

            let num_cand = group.candidate_ids.len();
            if num_cand == 0 {
                continue;
            }

            let feat_slice = &feats[..num_cand * input_dim];
            let input_data = TensorData::new(feat_slice.to_vec(), [num_cand, input_dim]);
            let features_tensor: Tensor<2> = Tensor::from_data(input_data, &device);

            let scores: Tensor<2> = model.forward(features_tensor);
            let scores = scores.squeeze_dim::<1>(1);

            let mut better_indices = Vec::new();
            let mut worse_indices = Vec::new();
            let mut pair_weights = Vec::new();

            for i in 0..num_cand {
                for j in 0..num_cand {
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
                        ) if c1 < c2 => {
                            better_indices.push(i as i64);
                            worse_indices.push(j as i64);
                            pair_weights.push(1.0f32);
                        }
                        (
                            CandidateKnowledge::Viable { .. },
                            CandidateKnowledge::KnownDead { .. } | CandidateKnowledge::Unknown,
                        ) => {
                            better_indices.push(i as i64);
                            worse_indices.push(j as i64);
                            pair_weights.push(1.0f32);
                        }
                        _ => {}
                    }
                }
            }

            if better_indices.is_empty() {
                continue;
            }

            let num_pairs = better_indices.len();
            let better_idx = Tensor::<1, Int>::from_data(
                TensorData::new(better_indices, [num_pairs]),
                &device,
            );
            let worse_idx = Tensor::<1, Int>::from_data(
                TensorData::new(worse_indices, [num_pairs]),
                &device,
            );
            let weights = Tensor::from_data(
                TensorData::new(pair_weights.clone(), [num_pairs]),
                &device,
            );

            let better_scores = scores.clone().select(0, better_idx);
            let worse_scores = scores.clone().select(0, worse_idx);

            let margins = better_scores - worse_scores;
            let pair_losses = (margins * -1.0f32).exp().add_scalar(1.0f32).log();
            let weight_sum: f32 = pair_weights.iter().sum();
            let loss = (pair_losses * weights).sum() / weight_sum;

            let loss_value: f32 = loss.clone().into_data().as_slice::<f32>().unwrap()[0];
            if !loss_value.is_finite() {
                return Err(TrainingError::NonFiniteLoss {
                    epoch,
                    batch: total_steps,
                });
            }

            let grads = loss.backward();
            let grads_params = GradientsParams::from_grads(grads, &model);
            model = optimizer.step(config.learning_rate as f64, model, grads_params);

            epoch_loss_sum += loss_value;
            step_count += 1;
            total_steps += 1;
        }

        let avg_loss = if step_count > 0 {
            epoch_loss_sum / step_count as f32
        } else {
            0.0
        };
        epoch_losses.push(avg_loss);
    }

    let record = model.into_record();
    let mut final_model = RankMlp::new(input_dim, hidden_dim, 1);
    final_model = final_model.load_record(record);

    let final_loss = epoch_losses.last().copied().unwrap_or(0.0);
    Ok((
        BurnRankMlp::from_model(final_model, model_id),
        TrainingMetrics {
            epoch_losses,
            final_loss,
            total_steps,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_types::Digest;
    use smallvec::smallvec;

    #[test]
    fn test_viable_target_calculation() {
        let labels = vec![
            CandidateKnowledge::Viable {
                best_actions_to_go: 2,
                receipts: smallvec![Digest::ZERO],
            },
            CandidateKnowledge::Viable {
                best_actions_to_go: 4,
                receipts: smallvec![Digest::ZERO],
            },
            CandidateKnowledge::Unknown,
            CandidateKnowledge::KnownDead {
                certificate: Digest::ZERO,
            },
        ];

        let mut targets = vec![0.0f32; 4];
        let has_supervision = viable_target(&labels, 1.0, &mut targets).unwrap();
        assert!(has_supervision);

        assert!(targets[0] > targets[1]);
        assert_eq!(targets[2], 0.0);
        assert_eq!(targets[3], 0.0);
        assert!((targets[0] + targets[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_burn_model_training_convergence() {
        let model = reflex_ml_burn::BurnRankMlp::random(4, 8, 42);
        let s1 = reflex_types::StateId::from_digest(Digest::hash_blake3(b"s-burn"));
        let c1 = reflex_types::CandidateId::from_digest(Digest::hash_blake3(b"c1"));
        let c2 = reflex_types::CandidateId::from_digest(Digest::hash_blake3(b"c2"));

        let group = DecisionGroup {
            state_id: s1,
            candidate_ids: vec![c1, c2],
            labels: vec![
                CandidateKnowledge::Viable {
                    best_actions_to_go: 1,
                    receipts: smallvec::smallvec![Digest::ZERO],
                },
                CandidateKnowledge::KnownDead {
                    certificate: Digest::ZERO,
                },
            ],
            feature_ref: Digest::ZERO,
            source_episodes: Vec::new(),
            coverage: "test".to_string(),
        };

        let mut features_by_state = HashMap::new();
        features_by_state.insert(s1, vec![1.0, 1.0, 1.0, 1.0, -1.0, -1.0, -1.0, -1.0]);

        let config = TrainingConfig {
            epochs: 50,
            learning_rate: 0.05,
            weight_decay: 0.0001,
            temperature: 1.0,
            seed: 42,
        };

        let (_, metrics) = train_burn_model(model, &[group], &features_by_state, &config).unwrap();
        assert!(metrics.final_loss < metrics.epoch_losses[0]);
    }
}
