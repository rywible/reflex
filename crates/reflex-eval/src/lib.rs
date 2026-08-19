use reflex_dataset::{CandidateKnowledge, DecisionGroup};
use reflex_domain::FeatureBatch;
use reflex_ml_core::{InferenceTelemetry, Ranker};
use reflex_types::{Digest, StateId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EvaluationReport {
    pub total_groups: usize,
    pub top1_viable_rate: f32,
    pub top3_viable_recall: f32,
    pub mrr_cheapest_route: f32,
    pub pairwise_viable_accuracy: f32,
    pub score_entropy: f32,
    pub feature_collisions_detected: usize,
    pub oracle_ceiling: f32,
}

pub fn evaluate_model_offline(
    ranker: &dyn Ranker,
    groups: &[DecisionGroup],
    features_by_state: &HashMap<StateId, Vec<f32>>,
    feature_dim: usize,
) -> EvaluationReport {
    let mut report = EvaluationReport::default();
    if groups.is_empty() {
        return report;
    }

    let mut top1_viable_count = 0;
    let mut top3_viable_count = 0;
    let mut mrr_sum = 0.0;
    let mut pairwise_correct = 0;
    let mut pairwise_total = 0;
    let mut evaluated_groups = 0;

    for group in groups {
        let Some(feat_vals) = features_by_state.get(&group.state_id) else {
            continue;
        };

        let num_candidates = group.candidate_ids.len();
        if num_candidates == 0 {
            continue;
        }

        let mut fb = FeatureBatch::new(
            num_candidates,
            feature_dim,
            reflex_types::FeatureSchemaId::from_digest(Digest::ZERO),
        );
        fb.values.copy_from_slice(feat_vals);

        let mut scores = vec![0.0f32; num_candidates];
        let mut telemetry = InferenceTelemetry::default();
        if ranker
            .score_batch(&fb, &mut scores, &mut telemetry)
            .is_err()
        {
            continue;
        }

        // Rank candidates descending by score
        let mut ranked_indices: Vec<usize> = (0..num_candidates).collect();
        ranked_indices.sort_by(|&a, &b| {
            scores[b]
                .partial_cmp(&scores[a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Top 1 viable
        if let Some(&top_idx) = ranked_indices.first()
            && matches!(group.labels[top_idx], CandidateKnowledge::Viable { .. })
        {
            top1_viable_count += 1;
        }

        // Top 3 viable recall
        let top3 = &ranked_indices[..ranked_indices.len().min(3)];
        if top3
            .iter()
            .any(|&idx| matches!(group.labels[idx], CandidateKnowledge::Viable { .. }))
        {
            top3_viable_count += 1;
        }

        // MRR of cheapest known route
        let mut min_cost = u32::MAX;
        let mut best_cand_idx = None;
        for (idx, label) in group.labels.iter().enumerate() {
            if let CandidateKnowledge::Viable {
                best_actions_to_go, ..
            } = label
                && *best_actions_to_go < min_cost
            {
                min_cost = *best_actions_to_go;
                best_cand_idx = Some(idx);
            }
        }

        if let Some(target_idx) = best_cand_idx
            && let Some(rank) = ranked_indices.iter().position(|&idx| idx == target_idx)
        {
            mrr_sum += 1.0 / (rank as f32 + 1.0);
        }

        // Pairwise accuracy
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
                    (
                        CandidateKnowledge::Viable { .. },
                        CandidateKnowledge::KnownDead { .. } | CandidateKnowledge::Unknown,
                    ) => {
                        pairwise_total += 1;
                        if scores[i] > scores[j] {
                            pairwise_correct += 1;
                        }
                    }
                    _ => {}
                }
            }
        }

        evaluated_groups += 1;
    }

    if evaluated_groups > 0 {
        report.total_groups = evaluated_groups;
        report.top1_viable_rate = top1_viable_count as f32 / evaluated_groups as f32;
        report.top3_viable_recall = top3_viable_count as f32 / evaluated_groups as f32;
        report.mrr_cheapest_route = mrr_sum / evaluated_groups as f32;
        report.pairwise_viable_accuracy = if pairwise_total > 0 {
            pairwise_correct as f32 / pairwise_total as f32
        } else {
            1.0
        };
        report.oracle_ceiling = 1.0;
    }

    report
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
                    receipts: smallvec![Digest::ZERO],
                },
                CandidateKnowledge::Viable {
                    best_actions_to_go: 5,
                    receipts: smallvec![Digest::ZERO],
                },
            ],
            feature_ref: Digest::ZERO,
            source_episodes: Vec::new(),
            coverage: "test".to_string(),
        };

        let mut features_by_state = HashMap::new();
        features_by_state.insert(s1, vec![1.0, 0.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0]);

        let report = evaluate_model_offline(&mlp, &[group], &features_by_state, 4);
        assert_eq!(report.total_groups, 1);
        assert!(report.top1_viable_rate >= 0.0 && report.top1_viable_rate <= 1.0);
    }
}
