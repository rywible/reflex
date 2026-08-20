use crate::features::layout;
use reflex_domain::FeatureBatch;
use reflex_search::{HeuristicRanker, SearchPolicy, UniformRanker};
use reflex_types::{CandidateId, ModelCheckpointId};

/// Uniform baseline using the same search accounting path as learned policies.
pub fn uniform_policy(model_id: ModelCheckpointId, cell_seed: u64) -> UniformRanker {
    UniformRanker::with_seed(model_id, cell_seed)
}

/// Cost-first heuristic: prefer lower resulting expression cost.
pub fn cost_first_policy(
    model_id: ModelCheckpointId,
) -> HeuristicRanker<impl Fn(&[f32]) -> f32 + Send + Sync> {
    HeuristicRanker::new(model_id, |row| {
        // Lower candidate cost => higher score
        1.0 - row[layout::CAND_COST]
    })
}

/// Simplification-first heuristic: prefer larger negative cost delta.
pub fn simplification_first_policy(
    model_id: ModelCheckpointId,
) -> HeuristicRanker<impl Fn(&[f32]) -> f32 + Send + Sync> {
    HeuristicRanker::new(model_id, |row| -row[layout::COST_DELTA])
}

/// Offline oracle for diagnostics only — never wired into training cells.
pub struct BitvecOraclePolicy {
    model_id: ModelCheckpointId,
}

impl BitvecOraclePolicy {
    pub fn new(model_id: ModelCheckpointId) -> Self {
        Self { model_id }
    }

    pub fn model_id(&self) -> ModelCheckpointId {
        self.model_id
    }

    /// Score candidates by verified utility upper bound (cost reduction).
    pub fn score_candidates(
        state_cost: u32,
        rewrites: &[crate::rewrites::BvCandidate],
    ) -> Vec<f32> {
        rewrites
            .iter()
            .map(|c| {
                let saved = state_cost.saturating_sub(c.resulting_expr.cost());
                saved as f32 + if c.cost_delta < 0 { 0.5 } else { 0.0 }
            })
            .collect()
    }
}

impl SearchPolicy for BitvecOraclePolicy {
    fn model_id(&self) -> ModelCheckpointId {
        self.model_id
    }

    fn score_batch(
        &self,
        features: &FeatureBatch,
        candidate_ids: &[CandidateId],
        output: &mut [f32],
        _telemetry: &mut reflex_search::InferenceTelemetry,
    ) -> Result<(), reflex_search::PolicyError> {
        for (i, out) in output.iter_mut().enumerate().take(features.rows) {
            let row = features.row(i);
            let _ = candidate_ids.get(i);
            *out = -row[layout::CAND_COST] + row[layout::COST_DELTA].abs();
        }
        Ok(())
    }
}

#[allow(dead_code)] // exercised by domain policy tests and offline diagnostics
/// Batch heuristic scorer shared by collection diagnostics.
pub fn score_heuristic_batch(features: &FeatureBatch, output: &mut [f32]) {
    for (i, out) in output.iter_mut().enumerate().take(features.rows) {
        let row = features.row(i);
        *out = -row[layout::CAND_COST] - row[layout::COST_DELTA];
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::BvExpr;
    use crate::features::{BITVEC_FEATURE_DIM, extract_row};
    use crate::rewrites::generate_rewrites;
    use reflex_domain::FeatureBatch;
    use reflex_types::{Digest, FeatureSchemaId};

    #[test]
    fn test_heuristic_scoring_throughput_smoke() {
        let state = BvExpr::Xor(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0)));
        let rewrites = generate_rewrites(&state);
        let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(b"bitvec-features"));
        let mut batch = FeatureBatch::new(rewrites.len(), BITVEC_FEATURE_DIM, schema);
        for (i, rw) in rewrites.iter().enumerate() {
            let row = extract_row(&state, Some(rw));
            batch.row_mut(i).copy_from_slice(&row);
        }
        let mut scores = vec![0.0f32; rewrites.len()];
        let start = std::time::Instant::now();
        for _ in 0..10_000 {
            score_heuristic_batch(&batch, &mut scores);
        }
        assert!(start.elapsed().as_millis() < 500);
    }
}
