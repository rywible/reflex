use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use reflex_eval::EvaluationReport;
use reflex_meta::MetaError;
use reflex_types::{
    CellId, Digest, ExperimentId, GenerationId, KnowledgeEditionId, ModelCheckpointId, TaskId,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum SchedulerError {
    #[error("meta error: {0}")]
    Meta(#[from] MetaError),
    #[error("invalid state transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: GenerationState,
        to: GenerationState,
    },
    #[error("stop condition reached: {0}")]
    Stopped(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GenerationState {
    Bootstrap,
    Collecting,
    Verifying,
    CompilingDataset,
    Training,
    Evaluating,
    PromotionPending,
    Promoted,
    Rejected,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchConfig {
    pub algorithm: String,
    pub action_budget: u32,
    pub node_budget: u32,
    pub cpu_seconds: f64,
    pub exploration_uniform: f32,
}

impl CanonicalEncode for SearchConfig {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.algorithm)?;
        out.write_u32(self.action_budget)?;
        out.write_u32(self.node_budget)?;
        out.write_f64(self.cpu_seconds)?;
        out.write_f32(self.exploration_uniform)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentManifest {
    pub schema: String,
    pub name: String,
    pub domain: String,
    pub search: SearchConfig,
    pub initial_checkpoint: Option<ModelCheckpointId>,
    pub initial_knowledge: Option<KnowledgeEditionId>,
    pub resource_class: String,
}

impl CanonicalEncode for ExperimentManifest {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.schema)?;
        out.write_str(&self.name)?;
        out.write_str(&self.domain)?;
        self.search.encode_canonical(out)?;
        out.write_option(self.initial_checkpoint.as_ref().map(|c| c.digest()))?;
        out.write_option(self.initial_knowledge.as_ref().map(|k| k.digest()))?;
        out.write_str(&self.resource_class)?;
        Ok(())
    }
}

impl ExperimentManifest {
    pub fn manifest_digest(&self) -> Digest {
        reflex_canonical::content_id(b"reflex.experiment.v1", self)
            .unwrap_or_else(|_| Digest::hash_blake3(b"experiment_manifest_fallback"))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CellManifest {
    pub experiment_id: ExperimentId,
    pub generation_id: GenerationId,
    pub task_id: TaskId,
    pub seed: u64,
    pub search: SearchConfig,
    pub model_checkpoint: Option<ModelCheckpointId>,
    pub knowledge_edition: Option<KnowledgeEditionId>,
    pub resource_class: String,
}

impl CanonicalEncode for CellManifest {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_digest(self.experiment_id.digest())?;
        out.write_digest(self.generation_id.digest())?;
        out.write_digest(self.task_id.digest())?;
        out.write_u64(self.seed)?;
        self.search.encode_canonical(out)?;
        out.write_option(self.model_checkpoint.as_ref().map(|c| c.digest()))?;
        out.write_option(self.knowledge_edition.as_ref().map(|k| k.digest()))?;
        out.write_str(&self.resource_class)?;
        Ok(())
    }
}

impl CellManifest {
    pub fn cell_id(&self) -> CellId {
        let digest = reflex_canonical::content_id(b"reflex.cell.v1", self)
            .unwrap_or_else(|_| Digest::hash_blake3(b"cell_manifest_fallback"));
        CellId::from_digest(digest)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromotionPolicy {
    pub required_lineages: usize,
    pub max_inference_share: f32,
    pub min_top1_viable_rate: f32,
    pub min_mrr: f32,
}

impl Default for PromotionPolicy {
    fn default() -> Self {
        Self {
            required_lineages: 4,
            max_inference_share: 0.10,
            min_top1_viable_rate: 0.50,
            min_mrr: 0.50,
        }
    }
}

impl PromotionPolicy {
    pub fn evaluate_promotion(&self, report: &EvaluationReport) -> bool {
        report.top1_viable_rate >= self.min_top1_viable_rate
            && report.mrr_cheapest_route >= self.min_mrr
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StopPolicy {
    pub max_generations: u32,
    pub max_cpu_hours: f64,
    pub no_promotion_generations: u32,
}

impl Default for StopPolicy {
    fn default() -> Self {
        Self {
            max_generations: 10,
            max_cpu_hours: 50.0,
            no_promotion_generations: 3,
        }
    }
}

pub struct GenerationCoordinator {
    pub state: GenerationState,
    pub current_generation: u32,
    pub no_promotion_streak: u32,
    pub active_model: Option<ModelCheckpointId>,
    pub active_knowledge: Option<KnowledgeEditionId>,
    pub stop_policy: StopPolicy,
    pub promotion_policy: PromotionPolicy,
}

impl GenerationCoordinator {
    pub fn new(
        stop_policy: StopPolicy,
        promotion_policy: PromotionPolicy,
        initial_model: Option<ModelCheckpointId>,
        initial_knowledge: Option<KnowledgeEditionId>,
    ) -> Self {
        Self {
            state: GenerationState::Bootstrap,
            current_generation: 1,
            no_promotion_streak: 0,
            active_model: initial_model,
            active_knowledge: initial_knowledge,
            stop_policy,
            promotion_policy,
        }
    }

    pub fn transition_to(&mut self, next: GenerationState) -> Result<(), SchedulerError> {
        let valid = match (self.state, next) {
            (GenerationState::Bootstrap, GenerationState::Collecting) => true,
            (GenerationState::Collecting, GenerationState::Verifying) => true,
            (GenerationState::Verifying, GenerationState::CompilingDataset) => true,
            (GenerationState::CompilingDataset, GenerationState::Training) => true,
            (GenerationState::Training, GenerationState::Evaluating) => true,
            (GenerationState::Evaluating, GenerationState::PromotionPending) => true,
            (GenerationState::Evaluating, GenerationState::Promoted) => true,
            (GenerationState::Evaluating, GenerationState::Rejected) => true,
            (GenerationState::PromotionPending, GenerationState::Promoted) => true,
            (GenerationState::PromotionPending, GenerationState::Rejected) => true,
            (GenerationState::Promoted, GenerationState::Collecting) => true,
            (GenerationState::Promoted, GenerationState::Stopped) => true,
            (GenerationState::Rejected, GenerationState::Collecting) => true,
            (GenerationState::Rejected, GenerationState::Stopped) => true,
            (_, GenerationState::Failed) => true,
            (_, GenerationState::Stopped) => true,
            (from, to) if from == to => true,
            _ => false,
        };

        if !valid {
            return Err(SchedulerError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }

        self.state = next;
        Ok(())
    }

    pub fn handle_evaluation_result(
        &mut self,
        report: &EvaluationReport,
        candidate_checkpoint: ModelCheckpointId,
    ) -> GenerationState {
        if self.promotion_policy.evaluate_promotion(report) {
            self.active_model = Some(candidate_checkpoint);
            self.no_promotion_streak = 0;
            self.state = GenerationState::Promoted;
        } else {
            self.no_promotion_streak += 1;
            self.state = GenerationState::Rejected;
        }

        self.current_generation += 1;

        if self.current_generation > self.stop_policy.max_generations
            || self.no_promotion_streak >= self.stop_policy.no_promotion_generations
        {
            self.state = GenerationState::Stopped;
        }

        self.state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generation_coordinator_lifecycle() {
        let mut coord = GenerationCoordinator::new(
            StopPolicy {
                max_generations: 3,
                max_cpu_hours: 10.0,
                no_promotion_generations: 2,
            },
            PromotionPolicy {
                required_lineages: 1,
                max_inference_share: 0.1,
                min_top1_viable_rate: 0.6,
                min_mrr: 0.5,
            },
            None,
            None,
        );

        assert_eq!(coord.state, GenerationState::Bootstrap);
        coord.transition_to(GenerationState::Collecting).unwrap();
        assert_eq!(coord.state, GenerationState::Collecting);

        let report_pass = EvaluationReport {
            top1_viable_rate: 0.75,
            mrr_cheapest_route: 0.8,
            ..Default::default()
        };

        let cand_id = ModelCheckpointId::from_digest(Digest::hash_blake3(b"cand1"));
        let next_state = coord.handle_evaluation_result(&report_pass, cand_id);
        assert_eq!(next_state, GenerationState::Promoted);
        assert_eq!(coord.active_model, Some(cand_id));

        // Test that illegal transitions are rejected
        let mut coord2 = GenerationCoordinator::new(
            StopPolicy::default(),
            PromotionPolicy::default(),
            None,
            None,
        );
        let err = coord2.transition_to(GenerationState::Evaluating);
        assert!(err.is_err());
    }
}
