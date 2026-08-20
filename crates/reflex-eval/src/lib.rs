use reflex_dataset::{CandidateKnowledge, DecisionGroup};
use reflex_domain::FeatureBatch;
use reflex_ml_core::{InferenceTelemetry, Ranker};
use reflex_types::{FeatureSchemaId, StateId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum EvaluationError {
    #[error("evaluation dataset is empty")]
    EmptyDataset,
    #[error("invalid feature schema or feature dimension")]
    InvalidFeatureSchema,
    #[error("group {group} has invalid candidate/label geometry")]
    InvalidGroup { group: usize },
    #[error("group {group} has invalid supervision at candidate {candidate}: {reason}")]
    InvalidSupervision {
        group: usize,
        candidate: usize,
        reason: String,
    },
    #[error("missing feature payload for group {group} state {state}")]
    MissingFeatures { group: usize, state: StateId },
    #[error("feature dimensions for group {group}: expected {expected}, found {found}")]
    FeatureDimension {
        group: usize,
        expected: usize,
        found: usize,
    },
    #[error("ranker failed for group {group}: {reason}")]
    Scoring { group: usize, reason: String },
    #[error("ranker produced a non-finite score for group {group}")]
    NonFiniteScore { group: usize },
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SliceMetrics {
    pub groups: usize,
    pub groups_with_viable_route: usize,
    pub semantic_groups: usize,
    pub top1_viable_rate: f32,
    pub ndcg: f32,
    pub score_entropy: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct StateErrorSlice {
    pub state_id: StateId,
    pub coverage: String,
    pub candidate_count: usize,
    pub known_candidate_count: usize,
    pub unknown_candidate_count: usize,
    pub has_viable_route: bool,
    pub top1_viable: Option<bool>,
    pub ndcg: Option<f32>,
    pub score_margin: f32,
    pub score_entropy: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EvaluationReport {
    pub total_groups: usize,
    pub top1_viable_rate: f32,
    pub top3_viable_recall: f32,
    pub mrr_cheapest_route: f32,
    pub pairwise_viable_accuracy: f32,
    pub ndcg: f32,
    pub score_entropy: f32,
    pub mean_score_margin: f32,
    pub calibration_brier: f32,
    pub candidate_diversity: f32,
    pub feature_collisions_detected: usize,
    pub oracle_ceiling: f32,
    pub semantic_groups: usize,
    pub coverage_breakdown: BTreeMap<String, SliceMetrics>,
    pub state_slices: Vec<StateErrorSlice>,
}

pub fn evaluate_model_offline(
    ranker: &dyn Ranker,
    groups: &[DecisionGroup],
    features_by_state: &HashMap<StateId, Vec<f32>>,
    feature_dim: usize,
    feature_schema: FeatureSchemaId,
) -> Result<EvaluationReport, EvaluationError> {
    if groups.is_empty() {
        return Err(EvaluationError::EmptyDataset);
    }
    if feature_dim == 0 || feature_schema.digest() == &reflex_types::Digest::ZERO {
        return Err(EvaluationError::InvalidFeatureSchema);
    }

    let mut report = EvaluationReport::default();
    let mut top1_viable_count = 0;
    let mut top3_viable_count = 0;
    let mut mrr_sum = 0.0;
    let mut pairwise_correct = 0;
    let mut pairwise_total = 0;
    let mut ndcg_sum = 0.0;
    let mut entropy_sum = 0.0;
    let mut margin_sum = 0.0;
    let mut calibration_sum = 0.0;
    let mut calibration_count = 0usize;
    let mut viable_group_count = 0usize;
    let mut unique_candidates = HashSet::new();
    let mut total_candidates = 0usize;
    let mut coverage_accumulators = BTreeMap::<String, CoverageAccumulator>::new();

    for (group_index, group) in groups.iter().enumerate() {
        let num_candidates = group.candidate_ids.len();
        if num_candidates == 0 || group.labels.len() != num_candidates {
            return Err(EvaluationError::InvalidGroup { group: group_index });
        }
        for (candidate_index, label) in group.labels.iter().enumerate() {
            label
                .validate_evidence()
                .map_err(|error| EvaluationError::InvalidSupervision {
                    group: group_index,
                    candidate: candidate_index,
                    reason: error.to_string(),
                })?;
        }
        let feat_vals =
            features_by_state
                .get(&group.state_id)
                .ok_or(EvaluationError::MissingFeatures {
                    group: group_index,
                    state: group.state_id,
                })?;
        let expected_features =
            num_candidates
                .checked_mul(feature_dim)
                .ok_or(EvaluationError::FeatureDimension {
                    group: group_index,
                    expected: usize::MAX,
                    found: feat_vals.len(),
                })?;
        if feat_vals.len() != expected_features {
            return Err(EvaluationError::FeatureDimension {
                group: group_index,
                expected: expected_features,
                found: feat_vals.len(),
            });
        }
        if feat_vals.iter().any(|value| !value.is_finite()) {
            return Err(EvaluationError::NonFiniteScore { group: group_index });
        }

        let mut fb = FeatureBatch::new(num_candidates, feature_dim, feature_schema);
        fb.values.copy_from_slice(feat_vals);

        let mut scores = vec![0.0f32; num_candidates];
        let mut telemetry = InferenceTelemetry::default();
        ranker
            .score_batch(&fb, &mut scores, &mut telemetry)
            .map_err(|error| EvaluationError::Scoring {
                group: group_index,
                reason: error.to_string(),
            })?;
        if scores.iter().any(|score| !score.is_finite()) {
            return Err(EvaluationError::NonFiniteScore { group: group_index });
        }

        let mut ranked_indices: Vec<usize> = (0..num_candidates)
            .filter(|index| {
                matches!(
                    group.labels[*index],
                    CandidateKnowledge::Viable { .. } | CandidateKnowledge::KnownDead { .. }
                )
            })
            .collect();
        ranked_indices.sort_by(|&a, &b| {
            scores[b].total_cmp(&scores[a]).then_with(|| {
                group.candidate_ids[a]
                    .digest()
                    .cmp(group.candidate_ids[b].digest())
            })
        });

        let has_viable = group
            .labels
            .iter()
            .any(|label| matches!(label, CandidateKnowledge::Viable { .. }));
        viable_group_count += usize::from(has_viable);
        total_candidates += num_candidates;
        unique_candidates.extend(group.candidate_ids.iter().copied());
        let entropy = normalized_score_entropy(&scores);
        let margin = score_margin(&scores);
        entropy_sum += entropy;
        margin_sum += margin;

        let group_ndcg = if has_viable && !ranked_indices.is_empty() {
            Some(ndcg(&ranked_indices, &group.labels))
        } else {
            None
        };
        let top1_viable = if has_viable && !ranked_indices.is_empty() {
            Some(matches!(
                group.labels[ranked_indices[0]],
                CandidateKnowledge::Viable { .. }
            ))
        } else {
            None
        };

        if has_viable {
            report.semantic_groups += 1;
            top1_viable_count += usize::from(top1_viable == Some(true));
            let top3 = &ranked_indices[..ranked_indices.len().min(3)];
            top3_viable_count += usize::from(
                top3.iter()
                    .any(|&idx| matches!(group.labels[idx], CandidateKnowledge::Viable { .. })),
            );
            ndcg_sum += group_ndcg.unwrap_or(0.0);

            let min_cost = group
                .labels
                .iter()
                .filter_map(|label| match label {
                    CandidateKnowledge::Viable {
                        best_actions_to_go, ..
                    } => Some(*best_actions_to_go),
                    _ => None,
                })
                .min()
                .expect("has viable route");
            if let Some(rank) = ranked_indices.iter().position(|&idx| {
                matches!(
                    group.labels[idx],
                    CandidateKnowledge::Viable { best_actions_to_go, .. }
                        if best_actions_to_go == min_cost
                )
            }) {
                mrr_sum += 1.0 / (rank as f32 + 1.0);
            }
        }

        for &index in &ranked_indices {
            let target = f32::from(matches!(
                group.labels[index],
                CandidateKnowledge::Viable { .. }
            ));
            let probability = 1.0 / (1.0 + (-scores[index]).exp());
            calibration_sum += (probability - target).powi(2);
            calibration_count += 1;
        }

        for i in 0..num_candidates {
            for j in 0..num_candidates {
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
                        pairwise_total += 1;
                        if scores[i] > scores[j] {
                            pairwise_correct += 1;
                        }
                    }
                    (CandidateKnowledge::Viable { .. }, CandidateKnowledge::KnownDead { .. }) => {
                        pairwise_total += 1;
                        if scores[i] > scores[j] {
                            pairwise_correct += 1;
                        }
                    }
                    _ => {}
                }
            }
        }

        let accumulator = coverage_accumulators
            .entry(group.coverage.clone())
            .or_default();
        accumulator.groups += 1;
        accumulator.viable_groups += usize::from(has_viable);
        accumulator.semantic_groups += usize::from(has_viable);
        accumulator.top1 += usize::from(top1_viable == Some(true));
        accumulator.ndcg += group_ndcg.unwrap_or(0.0);
        accumulator.entropy += entropy;
        report.state_slices.push(StateErrorSlice {
            state_id: group.state_id,
            coverage: group.coverage.clone(),
            candidate_count: num_candidates,
            known_candidate_count: ranked_indices.len(),
            unknown_candidate_count: group
                .labels
                .iter()
                .filter(|label| matches!(label, CandidateKnowledge::Unknown))
                .count(),
            has_viable_route: has_viable,
            top1_viable,
            ndcg: group_ndcg,
            score_margin: margin,
            score_entropy: entropy,
        });
    }

    report.total_groups = groups.len();
    if report.semantic_groups > 0 {
        let denominator = report.semantic_groups as f32;
        report.top1_viable_rate = top1_viable_count as f32 / denominator;
        report.top3_viable_recall = top3_viable_count as f32 / denominator;
        report.mrr_cheapest_route = mrr_sum / denominator;
        report.ndcg = ndcg_sum / denominator;
    }
    report.pairwise_viable_accuracy = if pairwise_total > 0 {
        pairwise_correct as f32 / pairwise_total as f32
    } else {
        0.0
    };
    report.score_entropy = entropy_sum / groups.len() as f32;
    report.mean_score_margin = margin_sum / groups.len() as f32;
    report.calibration_brier = if calibration_count > 0 {
        calibration_sum / calibration_count as f32
    } else {
        0.0
    };
    report.candidate_diversity = unique_candidates.len() as f32 / total_candidates as f32;
    report.oracle_ceiling = viable_group_count as f32 / groups.len() as f32;
    report.feature_collisions_detected = audit_feature_collisions(groups, features_by_state);
    report.coverage_breakdown = coverage_accumulators
        .into_iter()
        .map(|(name, value)| (name, value.finish()))
        .collect();
    Ok(report)
}

#[derive(Default)]
struct CoverageAccumulator {
    groups: usize,
    viable_groups: usize,
    semantic_groups: usize,
    top1: usize,
    ndcg: f32,
    entropy: f32,
}

impl CoverageAccumulator {
    fn finish(self) -> SliceMetrics {
        let semantic = self.semantic_groups.max(1) as f32;
        SliceMetrics {
            groups: self.groups,
            groups_with_viable_route: self.viable_groups,
            semantic_groups: self.semantic_groups,
            top1_viable_rate: self.top1 as f32 / semantic,
            ndcg: self.ndcg / semantic,
            score_entropy: self.entropy / self.groups.max(1) as f32,
        }
    }
}

fn relevance(label: &CandidateKnowledge) -> f32 {
    match label {
        CandidateKnowledge::Viable {
            best_actions_to_go, ..
        } => 1.0 / (1.0 + *best_actions_to_go as f32),
        _ => 0.0,
    }
}

fn discounted_gain(order: &[usize], labels: &[CandidateKnowledge]) -> f32 {
    order
        .iter()
        .enumerate()
        .map(|(rank, &index)| relevance(&labels[index]) / (rank as f32 + 2.0).log2())
        .sum()
}

fn ndcg(ranked: &[usize], labels: &[CandidateKnowledge]) -> f32 {
    let actual = discounted_gain(ranked, labels);
    let mut ideal = ranked.to_vec();
    ideal.sort_by(|&a, &b| relevance(&labels[b]).total_cmp(&relevance(&labels[a])));
    let ceiling = discounted_gain(&ideal, labels);
    if ceiling > 0.0 { actual / ceiling } else { 0.0 }
}

fn normalized_score_entropy(scores: &[f32]) -> f32 {
    if scores.len() <= 1 {
        return 0.0;
    }
    let max = scores.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let partition: f32 = scores.iter().map(|score| (*score - max).exp()).sum();
    let entropy: f32 = scores
        .iter()
        .map(|score| {
            let probability = (*score - max).exp() / partition;
            -probability * probability.ln()
        })
        .sum();
    entropy / (scores.len() as f32).ln()
}

fn score_margin(scores: &[f32]) -> f32 {
    let mut best = f32::NEG_INFINITY;
    let mut second = f32::NEG_INFINITY;
    for score in scores {
        if *score > best {
            second = best;
            best = *score;
        } else if *score > second {
            second = *score;
        }
    }
    if second.is_finite() {
        best - second
    } else {
        0.0
    }
}

pub fn audit_feature_collisions(
    groups: &[DecisionGroup],
    features_by_state: &HashMap<StateId, Vec<f32>>,
) -> usize {
    let mut collisions = 0;
    let mut feature_to_best_action: HashMap<Vec<u32>, (usize, u32)> = HashMap::new();

    for group in groups {
        let Some(feats) = features_by_state.get(&group.state_id) else {
            continue;
        };
        let bit_repr: Vec<u32> = feats.iter().map(|f| f.to_bits()).collect();
        let mut best_viable: Option<(usize, u32)> = None;
        for (idx, label) in group.labels.iter().enumerate() {
            if let CandidateKnowledge::Viable {
                best_actions_to_go, ..
            } = label
                && best_viable.is_none_or(|(_, min_c)| *best_actions_to_go < min_c)
            {
                best_viable = Some((idx, *best_actions_to_go));
            }
        }

        if let Some(viable_target) = best_viable {
            if let Some(&existing) = feature_to_best_action.get(&bit_repr) {
                if existing.0 != viable_target.0 {
                    collisions += 1;
                }
            } else {
                feature_to_best_action.insert(bit_repr, viable_target);
            }
        }
    }

    collisions
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_ml_micro::MicroMlp;
    use reflex_types::{CandidateId, Digest, StateId};
    use smallvec::smallvec;

    #[test]
    fn test_evaluation_report_computation() {
        let mlp = MicroMlp::random(4, 8, 42);
        let s1 = StateId::from_digest(Digest::hash_blake3(b"s1"));
        let c1 = CandidateId::from_digest(Digest::hash_blake3(b"c1"));
        let c2 = CandidateId::from_digest(Digest::hash_blake3(b"c2"));

        let group = DecisionGroup {
            state_id: s1,
            candidate_ids: vec![c1, c2],
            labels: vec![
                CandidateKnowledge::Viable {
                    best_actions_to_go: 2,
                    receipts: smallvec![Digest::hash_blake3(b"receipt-a")],
                },
                CandidateKnowledge::Viable {
                    best_actions_to_go: 5,
                    receipts: smallvec![Digest::hash_blake3(b"receipt-b")],
                },
            ],
            feature_ref: Digest::ZERO,
            source_episodes: Vec::new(),
            coverage: "test".to_string(),
        };

        let mut features_by_state = HashMap::new();
        features_by_state.insert(s1, vec![1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0]);

        let report = evaluate_model_offline(
            &mlp,
            &[group],
            &features_by_state,
            4,
            FeatureSchemaId::from_digest(Digest::hash_blake3(b"features")),
        )
        .unwrap();
        assert_eq!(report.total_groups, 1);
        assert!(report.top1_viable_rate >= 0.0 && report.top1_viable_rate <= 1.0);
        assert!(report.ndcg > 0.0 && report.ndcg <= 1.0);
        assert!(report.score_entropy >= 0.0 && report.score_entropy <= 1.0);
        assert_eq!(report.oracle_ceiling, 1.0);
        assert_eq!(report.state_slices.len(), 1);
    }

    #[test]
    fn missing_features_fail_closed() {
        let mlp = MicroMlp::random(2, 2, 1);
        let group = DecisionGroup {
            state_id: StateId::from_digest(Digest::hash_blake3(b"missing")),
            candidate_ids: vec![CandidateId::from_digest(Digest::hash_blake3(b"c"))],
            labels: vec![CandidateKnowledge::Unknown],
            feature_ref: Digest::hash_blake3(b"features"),
            source_episodes: vec![],
            coverage: "held-out".into(),
        };
        assert!(matches!(
            evaluate_model_offline(
                &mlp,
                &[group],
                &HashMap::new(),
                2,
                FeatureSchemaId::from_digest(Digest::hash_blake3(b"schema")),
            ),
            Err(EvaluationError::MissingFeatures { .. })
        ));
    }
}
