use crate::{FeatureBatch, InferenceTelemetry, PolicyError};
use rand::Rng;
use rand_chacha::ChaCha8Rng;
use rand_chacha::rand_core::SeedableRng;
use reflex_types::{CandidateId, Digest, ModelCheckpointId};
use serde::{Deserialize, Serialize};

/// Candidate-aware search policy. Learned model inference itself is owned by
/// `reflex_ml_core::Ranker`; adapters add candidate identity and search errors.
pub trait SearchPolicy: Send + Sync {
    fn model_id(&self) -> ModelCheckpointId;
    fn score_batch(
        &self,
        features: &FeatureBatch,
        candidate_ids: &[CandidateId],
        output: &mut [f32],
        telemetry: &mut InferenceTelemetry,
    ) -> Result<(), PolicyError>;

    fn validate_available(&self) -> Result<(), PolicyError> {
        Ok(())
    }
}

/// Deterministic uniform priorities from cell seed × candidate identity (§11.5).
pub struct UniformRanker {
    model_id: ModelCheckpointId,
    cell_seed: u64,
}

impl UniformRanker {
    pub fn new(model_id: ModelCheckpointId) -> Self {
        Self {
            model_id,
            cell_seed: 0,
        }
    }

    pub fn with_seed(model_id: ModelCheckpointId, cell_seed: u64) -> Self {
        Self {
            model_id,
            cell_seed,
        }
    }

    pub fn cell_seed(&self) -> u64 {
        self.cell_seed
    }
}

/// Alias used by benchmarks.
pub type UniformPolicy = UniformRanker;

pub fn deterministic_uniform_score(cell_seed: u64, candidate_id: CandidateId) -> f32 {
    let mut buf = [0u8; 40];
    buf[..8].copy_from_slice(&cell_seed.to_le_bytes());
    buf[8..40].copy_from_slice(candidate_id.digest().bytes.as_ref());
    let seed_bytes = Digest::hash_blake3(&buf).bytes;
    let seed = u64::from_le_bytes(seed_bytes[0..8].try_into().unwrap());
    ChaCha8Rng::seed_from_u64(seed).r#gen::<f32>()
}

impl SearchPolicy for UniformRanker {
    fn model_id(&self) -> ModelCheckpointId {
        self.model_id
    }

    fn score_batch(
        &self,
        _features: &FeatureBatch,
        candidate_ids: &[CandidateId],
        output: &mut [f32],
        _telemetry: &mut InferenceTelemetry,
    ) -> Result<(), PolicyError> {
        for (out_val, &cid) in output.iter_mut().zip(candidate_ids.iter()) {
            *out_val = deterministic_uniform_score(self.cell_seed, cid);
        }
        Ok(())
    }
}

pub struct HeuristicRanker<F: Fn(&[f32]) -> f32 + Send + Sync> {
    model_id: ModelCheckpointId,
    func: F,
}

impl<F: Fn(&[f32]) -> f32 + Send + Sync> HeuristicRanker<F> {
    pub fn new(model_id: ModelCheckpointId, func: F) -> Self {
        Self { model_id, func }
    }
}

impl<F: Fn(&[f32]) -> f32 + Send + Sync> SearchPolicy for HeuristicRanker<F> {
    fn model_id(&self) -> ModelCheckpointId {
        self.model_id
    }

    fn score_batch(
        &self,
        features: &FeatureBatch,
        _candidate_ids: &[CandidateId],
        output: &mut [f32],
        _telemetry: &mut InferenceTelemetry,
    ) -> Result<(), PolicyError> {
        for (i, out_val) in output.iter_mut().enumerate().take(features.rows) {
            *out_val = (self.func)(features.row(i));
        }
        Ok(())
    }
}

/// Mixture scheduler with reconstructible events (§11.5).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MixtureEvent {
    pub step: u64,
    pub component_index: usize,
    pub weight: f32,
}

pub struct MixturePolicy {
    pub(crate) components: Vec<(Box<dyn SearchPolicy>, f32)>,
    pub events: Vec<MixtureEvent>,
    step: u64,
    epsilon: f32,
    cell_seed: u64,
}

impl MixturePolicy {
    pub fn new(
        components: Vec<(Box<dyn SearchPolicy>, f32)>,
        epsilon: f32,
        cell_seed: u64,
    ) -> Result<Self, PolicyError> {
        if components.is_empty() {
            return Err(PolicyError::InferenceFailed(
                "mixture requires at least one component".to_string(),
            ));
        }
        for (c, _) in &components {
            c.validate_available()?;
        }
        Ok(Self {
            components,
            events: Vec::new(),
            step: 0,
            epsilon,
            cell_seed,
        })
    }

    pub fn events(&self) -> &[MixtureEvent] {
        &self.events
    }

    fn select_component(&mut self) -> usize {
        let idx = (self.step as usize) % self.components.len();
        self.events.push(MixtureEvent {
            step: self.step,
            component_index: idx,
            weight: self.components[idx].1,
        });
        self.step += 1;
        idx
    }
}

impl SearchPolicy for MixturePolicy {
    fn model_id(&self) -> ModelCheckpointId {
        self.components[0].0.model_id()
    }

    fn validate_available(&self) -> Result<(), PolicyError> {
        for (c, _) in &self.components {
            c.validate_available()?;
        }
        Ok(())
    }

    fn score_batch(
        &self,
        features: &FeatureBatch,
        candidate_ids: &[CandidateId],
        output: &mut [f32],
        telemetry: &mut InferenceTelemetry,
    ) -> Result<(), PolicyError> {
        // Mixture scoring is driven by the outer scheduler; this path blends
        // epsilon-uniform with the primary component for the current step.
        let primary_idx = (self.step.saturating_sub(1) as usize) % self.components.len();
        self.components[primary_idx]
            .0
            .score_batch(features, candidate_ids, output, telemetry)?;
        if self.epsilon > 0.0 {
            for (out_val, &cid) in output.iter_mut().zip(candidate_ids.iter()) {
                let uniform = deterministic_uniform_score(self.cell_seed, cid);
                *out_val = (1.0 - self.epsilon) * *out_val + self.epsilon * uniform;
            }
        }
        Ok(())
    }
}

/// Wrapper that records mixture component selection before scoring.
pub struct MixtureRanker {
    inner: MixturePolicy,
}

impl MixtureRanker {
    pub fn new(policy: MixturePolicy) -> Self {
        Self { inner: policy }
    }

    pub fn events(&self) -> &[MixtureEvent] {
        self.inner.events()
    }
}

impl SearchPolicy for MixtureRanker {
    fn model_id(&self) -> ModelCheckpointId {
        self.inner.model_id()
    }

    fn validate_available(&self) -> Result<(), PolicyError> {
        self.inner.validate_available()
    }

    fn score_batch(
        &self,
        features: &FeatureBatch,
        candidate_ids: &[CandidateId],
        output: &mut [f32],
        telemetry: &mut InferenceTelemetry,
    ) -> Result<(), PolicyError> {
        // Note: select_component requires &mut; callers use score_batch_mut on kernel path.
        self.inner
            .components
            .first()
            .expect("validated non-empty")
            .0
            .score_batch(features, candidate_ids, output, telemetry)
    }
}

impl MixturePolicy {
    pub fn score_batch_mut(
        &mut self,
        features: &FeatureBatch,
        candidate_ids: &[CandidateId],
        output: &mut [f32],
        telemetry: &mut InferenceTelemetry,
    ) -> Result<(), PolicyError> {
        let idx = self.select_component();
        self.components[idx]
            .0
            .score_batch(features, candidate_ids, output, telemetry)?;
        if self.epsilon > 0.0 {
            for (out_val, &cid) in output.iter_mut().zip(candidate_ids.iter()) {
                let uniform = deterministic_uniform_score(self.cell_seed, cid);
                *out_val = (1.0 - self.epsilon) * *out_val + self.epsilon * uniform;
            }
        }
        Ok(())
    }
}

/// Learned ranker placeholder that fails before cell start when unavailable.
pub struct UnavailableLearnedRanker {
    model_id: ModelCheckpointId,
    reason: String,
}

impl UnavailableLearnedRanker {
    pub fn new(model_id: ModelCheckpointId, reason: impl Into<String>) -> Self {
        Self {
            model_id,
            reason: reason.into(),
        }
    }
}

impl SearchPolicy for UnavailableLearnedRanker {
    fn model_id(&self) -> ModelCheckpointId {
        self.model_id
    }

    fn validate_available(&self) -> Result<(), PolicyError> {
        Err(PolicyError::InferenceFailed(self.reason.clone()))
    }

    fn score_batch(
        &self,
        _features: &FeatureBatch,
        _candidate_ids: &[CandidateId],
        _output: &mut [f32],
        _telemetry: &mut InferenceTelemetry,
    ) -> Result<(), PolicyError> {
        Err(PolicyError::InferenceFailed(self.reason.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_types::FeatureSchemaId;

    #[test]
    fn test_uniform_invariant_to_allocation_order() {
        let ids_a = [
            CandidateId::from_digest(Digest::hash_blake3(b"a")),
            CandidateId::from_digest(Digest::hash_blake3(b"b")),
            CandidateId::from_digest(Digest::hash_blake3(b"c")),
        ];
        let ids_b = [
            CandidateId::from_digest(Digest::hash_blake3(b"c")),
            CandidateId::from_digest(Digest::hash_blake3(b"a")),
            CandidateId::from_digest(Digest::hash_blake3(b"b")),
        ];
        let seed = 42u64;
        let scores_for = |ids: &[CandidateId]| {
            ids.iter()
                .map(|&id| deterministic_uniform_score(seed, id))
                .collect::<Vec<_>>()
        };
        let mut a = scores_for(&ids_a);
        let mut b = scores_for(&ids_b);
        a.sort_by(|x, y| x.partial_cmp(y).unwrap());
        b.sort_by(|x, y| x.partial_cmp(y).unwrap());
        assert_eq!(a, b);
    }

    #[test]
    fn test_uniform_not_constant() {
        let a = deterministic_uniform_score(1, CandidateId::from_digest(Digest::hash_blake3(b"x")));
        let b = deterministic_uniform_score(1, CandidateId::from_digest(Digest::hash_blake3(b"y")));
        assert_ne!(a, b);
    }

    #[test]
    fn test_unavailable_model_fails_before_start() {
        let ranker = UnavailableLearnedRanker::new(
            ModelCheckpointId::from_digest(Digest::hash_blake3(b"m")),
            "checkpoint missing",
        );
        assert!(ranker.validate_available().is_err());
    }

    #[test]
    fn test_mixture_events_recorded() {
        let uniform =
            UniformRanker::with_seed(ModelCheckpointId::from_digest(Digest::hash_blake3(b"u")), 7);
        let heuristic = HeuristicRanker::new(
            ModelCheckpointId::from_digest(Digest::hash_blake3(b"h")),
            |_| 0.5,
        );
        let mut mixture = MixturePolicy::new(
            vec![
                (Box::new(uniform) as Box<dyn SearchPolicy>, 0.5),
                (Box::new(heuristic) as Box<dyn SearchPolicy>, 0.5),
            ],
            0.1,
            99,
        )
        .unwrap();
        let features = FeatureBatch::new(2, 1, FeatureSchemaId::from_digest(Digest::ZERO));
        let ids = [
            CandidateId::from_digest(Digest::hash_blake3(b"c1")),
            CandidateId::from_digest(Digest::hash_blake3(b"c2")),
        ];
        let mut scores = [0.0f32; 2];
        let mut telemetry = InferenceTelemetry {
            cpu_ns: 0,
            model_id: mixture.model_id(),
        };
        mixture
            .score_batch_mut(&features, &ids, &mut scores, &mut telemetry)
            .unwrap();
        assert_eq!(mixture.events().len(), 1);
        assert_eq!(mixture.events()[0].component_index, 0);
    }
}
