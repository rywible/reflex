use reflex_dataset::{CandidateKnowledge, DecisionGroup, viable_target};
use reflex_domain::FeatureBatch;
use reflex_ml_burn::{
    BurnRankMlp, BurnTrainingCheckpoint, BurnTrainingConfig,
    TrainingSession as BurnTrainingSession, train_burn_model,
};
use reflex_ml_micro::{MicroCheckpointBundle, MicroMlp, MicroTrainer};
use reflex_types::{Digest, FeatureSchemaId, StateId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
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
    #[error("invalid supervision in group {group}, candidate {candidate}: {reason}")]
    InvalidSupervision {
        group: usize,
        candidate: usize,
        reason: String,
    },
    #[error(
        "feature dimensions do not match group {group}: expected at least {expected}, found {found}"
    )]
    FeatureDimension {
        group: usize,
        expected: usize,
        found: usize,
    },
    #[error("missing feature payload for group {group} state {state}")]
    MissingFeatures { group: usize, state: StateId },
    #[error("batch plan references group {index}, but dataset has {group_count} groups")]
    InvalidBatchPosition { index: usize, group_count: usize },
    #[error("sampling error: {0}")]
    Sampling(String),
    #[error("sweep error: {0}")]
    Sweep(String),
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

pub fn collect_supervised_pairs(group: &DecisionGroup) -> Vec<(usize, usize)> {
    reflex_ml_burn::collect_supervised_pairs(group)
}

pub fn masked_listwise_loss(scores: &[f32], targets: &[f32]) -> f32 {
    if scores.len() != targets.len()
        || scores.iter().any(|score| !score.is_finite())
        || targets
            .iter()
            .any(|target| !target.is_finite() || *target < 0.0)
    {
        return f32::NAN;
    }
    let log_partition = log_sum_exp_masked(scores, targets);
    let mut loss = 0.0;
    let mut weight = 0.0;
    for (&score, &target) in scores.iter().zip(targets.iter()) {
        if target > 0.0 {
            loss += target * (log_partition - score);
            weight += target;
        }
    }
    if weight > 0.0 { loss / weight } else { 0.0 }
}

fn log_sum_exp_masked(scores: &[f32], targets: &[f32]) -> f32 {
    let mut max_score = f32::NEG_INFINITY;
    for (&score, &target) in scores.iter().zip(targets.iter()) {
        if target > 0.0 {
            max_score = max_score.max(score);
        }
    }
    if !max_score.is_finite() {
        return 0.0;
    }
    let mut sum = 0.0;
    for (&score, &target) in scores.iter().zip(targets.iter()) {
        if target > 0.0 {
            sum += (score - max_score).exp();
        }
    }
    max_score + sum.ln()
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BatchPlan {
    pub seed: u64,
    pub group_indices: Vec<usize>,
    pub next_batch: usize,
    pub eval_excluded: BTreeSet<Digest>,
    /// Source corpus identity for every entry in `group_indices`. Legacy plans may omit
    /// these only when there are no evaluation exclusions to enforce.
    #[serde(default)]
    pub source_identities: Vec<Digest>,
}

impl BatchPlan {
    pub fn new(seed: u64, group_count: usize, eval_ids: HashSet<Digest>) -> Self {
        use rand::SeedableRng;
        use rand::seq::index::sample;
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(seed);
        let indices: Vec<usize> = sample(&mut rng, group_count, group_count).into_vec();
        Self {
            seed,
            group_indices: indices,
            next_batch: 0,
            eval_excluded: eval_ids.into_iter().collect(),
            source_identities: Vec::new(),
        }
    }

    pub fn next_group(&mut self) -> Result<Option<usize>, TrainingError> {
        if !self.eval_excluded.is_empty()
            && self.source_identities.len() != self.group_indices.len()
        {
            return Err(TrainingError::Sampling(
                "batch plan lacks source identities needed to enforce evaluation exclusion".into(),
            ));
        }
        if self.next_batch >= self.group_indices.len() {
            return Ok(None);
        }
        if self
            .source_identities
            .get(self.next_batch)
            .is_some_and(|source| self.eval_excluded.contains(source))
        {
            return Err(TrainingError::Sampling(
                "batch plan reached an excluded evaluation corpus".into(),
            ));
        }
        let idx = self.group_indices[self.next_batch];
        self.next_batch += 1;
        Ok(Some(idx))
    }

    pub fn peek_group(&self) -> Option<usize> {
        self.group_indices.get(self.next_batch).copied()
    }

    pub fn commit_group(&mut self) -> Result<(), TrainingError> {
        if self.next_batch >= self.group_indices.len() {
            return Err(TrainingError::Config(
                "cannot commit beyond the batch plan".into(),
            ));
        }
        self.next_batch += 1;
        Ok(())
    }

    pub fn from_sampling_manifest(
        manifest: &SamplingManifest,
        mut eval_ids: BTreeSet<Digest>,
    ) -> Result<Self, TrainingError> {
        manifest.validate()?;
        eval_ids.extend(
            manifest
                .excluded_evaluation_corpora
                .iter()
                .map(|corpus| corpus.identity),
        );
        if manifest
            .selected
            .iter()
            .any(|entry| eval_ids.contains(&entry.source_identity))
        {
            return Err(TrainingError::Sampling(
                "sampling manifest contains an excluded evaluation identity".into(),
            ));
        }
        Ok(Self {
            seed: manifest.seed,
            group_indices: manifest
                .selected
                .iter()
                .map(|entry| entry.group_index)
                .collect(),
            next_batch: 0,
            eval_excluded: eval_ids,
            source_identities: manifest
                .selected
                .iter()
                .map(|entry| entry.source_identity)
                .collect(),
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrainingBatch {
    pub features: Vec<f32>,
    pub targets: Vec<f32>,
    pub masks: Vec<f32>,
    pub candidate_count: usize,
}

/// Reusable contiguous storage for a group-aware batch. `group_offsets` has one
/// sentinel entry, so group `i` occupies `offsets[i]..offsets[i + 1]`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PackedTrainingBatch {
    pub features: Vec<f32>,
    pub targets: Vec<f32>,
    pub masks: Vec<f32>,
    pub weights: Vec<f32>,
    pub group_offsets: Vec<usize>,
    pub group_indices: Vec<usize>,
    pub feature_dim: usize,
}

impl PackedTrainingBatch {
    pub fn candidate_count(&self) -> usize {
        self.targets.len()
    }

    fn clear_preserving_capacity(&mut self, feature_dim: usize) {
        self.features.clear();
        self.targets.clear();
        self.masks.clear();
        self.weights.clear();
        self.group_offsets.clear();
        self.group_indices.clear();
        self.feature_dim = feature_dim;
        self.group_offsets.push(0);
    }
}

/// A packer owns and reuses its buffers. A failed pack leaves the durable plan
/// position unchanged; successful packing commits the exact number of groups.
pub struct GroupBatchPacker {
    batch: PackedTrainingBatch,
    target_scratch: Vec<f32>,
}

impl GroupBatchPacker {
    pub fn with_capacity(groups: usize, candidates: usize, feature_dim: usize) -> Self {
        Self {
            batch: PackedTrainingBatch {
                features: Vec::with_capacity(candidates.saturating_mul(feature_dim)),
                targets: Vec::with_capacity(candidates),
                masks: Vec::with_capacity(candidates),
                weights: Vec::with_capacity(candidates),
                group_offsets: Vec::with_capacity(groups.saturating_add(1)),
                group_indices: Vec::with_capacity(groups),
                feature_dim,
            },
            target_scratch: Vec::new(),
        }
    }

    pub fn pack<'a>(
        &'a mut self,
        plan: &mut BatchPlan,
        groups: &[DecisionGroup],
        features_by_state: &HashMap<StateId, Vec<f32>>,
        feature_dim: usize,
        max_groups: usize,
        temperature: f32,
    ) -> Result<Option<&'a PackedTrainingBatch>, TrainingError> {
        if max_groups == 0 || feature_dim == 0 || !temperature.is_finite() || temperature <= 0.0 {
            return Err(TrainingError::Config(
                "batch group count, feature dimension, and temperature must be positive".into(),
            ));
        }
        if !plan.eval_excluded.is_empty()
            && plan.source_identities.len() != plan.group_indices.len()
        {
            return Err(TrainingError::Sampling(
                "batch plan lacks source identities needed to enforce evaluation exclusion".into(),
            ));
        }
        let start = plan.next_batch;
        if start >= plan.group_indices.len() {
            return Ok(None);
        }
        let end = start
            .saturating_add(max_groups)
            .min(plan.group_indices.len());
        self.batch.clear_preserving_capacity(feature_dim);

        for position in start..end {
            if let Some(source) = plan.source_identities.get(position)
                && plan.eval_excluded.contains(source)
            {
                self.batch.clear_preserving_capacity(feature_dim);
                return Err(TrainingError::Sampling(format!(
                    "batch position {position} belongs to an excluded evaluation corpus"
                )));
            }
            let group_idx = plan.group_indices[position];
            let group = groups
                .get(group_idx)
                .ok_or(TrainingError::InvalidBatchPosition {
                    index: group_idx,
                    group_count: groups.len(),
                })?;
            validate_group_supervision(group_idx, group)?;
            let candidate_count = group.candidate_ids.len();
            let feature_count = candidate_count
                .checked_mul(feature_dim)
                .ok_or_else(|| TrainingError::Config("feature dimension overflow".into()))?;
            let features =
                features_by_state
                    .get(&group.state_id)
                    .ok_or(TrainingError::MissingFeatures {
                        group: group_idx,
                        state: group.state_id,
                    })?;
            if features.len() != feature_count {
                return Err(TrainingError::FeatureDimension {
                    group: group_idx,
                    expected: feature_count,
                    found: features.len(),
                });
            }
            if features.iter().any(|value| !value.is_finite()) {
                return Err(TrainingError::Config(format!(
                    "non-finite feature in group {group_idx}"
                )));
            }
            self.target_scratch.resize(candidate_count, 0.0);
            self.target_scratch.fill(0.0);
            if !viable_target(&group.labels, temperature, &mut self.target_scratch).map_err(
                |error| TrainingError::InvalidSupervision {
                    group: group_idx,
                    candidate: 0,
                    reason: error.to_string(),
                },
            )? {
                return Err(TrainingError::InsufficientData);
            }
            self.batch.features.extend_from_slice(features);
            self.batch.targets.extend_from_slice(&self.target_scratch);
            self.batch.masks.extend(group.labels.iter().map(|label| {
                if matches!(label, CandidateKnowledge::Unknown) {
                    0.0
                } else {
                    1.0
                }
            }));
            // Per-group weights keep differently sized decision groups from
            // silently dominating the loss. Unknown candidates remain weight zero.
            let known = group
                .labels
                .iter()
                .filter(|label| !matches!(label, CandidateKnowledge::Unknown))
                .count();
            let known_weight = 1.0 / known as f32;
            self.batch.weights.extend(group.labels.iter().map(|label| {
                if matches!(label, CandidateKnowledge::Unknown) {
                    0.0
                } else {
                    known_weight
                }
            }));
            self.batch.group_indices.push(group_idx);
            self.batch.group_offsets.push(self.batch.targets.len());
        }
        // Commit only after the whole batch has been validated and packed.
        plan.next_batch = end;
        Ok(Some(&self.batch))
    }
}

pub struct Trainer {
    pub config: TrainingConfig,
    pub plan: BatchPlan,
    pub last_checkpoint_step: u64,
}

impl Trainer {
    pub fn new(config: TrainingConfig, plan: BatchPlan) -> Self {
        Self {
            config,
            plan,
            last_checkpoint_step: 0,
        }
    }

    pub fn assemble_batch(
        &mut self,
        groups: &[DecisionGroup],
        features_by_state: &HashMap<StateId, Vec<f32>>,
        feature_dim: usize,
    ) -> Result<Option<TrainingBatch>, TrainingError> {
        if !self.plan.eval_excluded.is_empty()
            && self.plan.source_identities.len() != self.plan.group_indices.len()
        {
            return Err(TrainingError::Sampling(
                "batch plan lacks source identities needed to enforce evaluation exclusion".into(),
            ));
        }
        if self
            .plan
            .source_identities
            .get(self.plan.next_batch)
            .is_some_and(|source| self.plan.eval_excluded.contains(source))
        {
            return Err(TrainingError::Sampling(
                "batch plan reached an excluded evaluation corpus".into(),
            ));
        }
        let Some(group_idx) = self.plan.peek_group() else {
            return Ok(None);
        };
        let group = groups
            .get(group_idx)
            .ok_or(TrainingError::InvalidBatchPosition {
                index: group_idx,
                group_count: groups.len(),
            })?;
        validate_group_supervision(group_idx, group)?;
        let feats =
            features_by_state
                .get(&group.state_id)
                .ok_or(TrainingError::MissingFeatures {
                    group: group_idx,
                    state: group.state_id,
                })?;
        let n = group.candidate_ids.len();
        let feature_count = n
            .checked_mul(feature_dim)
            .ok_or_else(|| TrainingError::Config("feature dimension overflow".into()))?;
        if feats.len() != feature_count {
            return Err(TrainingError::FeatureDimension {
                group: group_idx,
                expected: feature_count,
                found: feats.len(),
            });
        }
        if feats.iter().any(|feature| !feature.is_finite()) {
            return Err(TrainingError::Config(format!(
                "non-finite feature in group {group_idx}"
            )));
        }
        let mut targets = vec![0.0; n];
        if !viable_target(&group.labels, self.config.temperature, &mut targets).map_err(
            |error| TrainingError::InvalidSupervision {
                group: group_idx,
                candidate: 0,
                reason: error.to_string(),
            },
        )? {
            return Err(TrainingError::InsufficientData);
        }
        let batch = TrainingBatch {
            features: feats.to_vec(),
            targets,
            masks: group
                .labels
                .iter()
                .map(|l| match l {
                    CandidateKnowledge::Unknown => 0.0,
                    _ => 1.0,
                })
                .collect(),
            candidate_count: n,
        };
        self.plan.commit_group()?;
        Ok(Some(batch))
    }
}

pub struct TrainingSession {
    pub micro_trainer: Option<MicroTrainer>,
    pub burn_model: Option<BurnRankMlp>,
    pub step: u64,
    pub plan: BatchPlan,
    pub epoch: usize,
    pub status: TrainingRunStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrainingRunStatus {
    Running,
    Cancelled,
    Completed,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MicroTrainingCheckpoint {
    pub format_version: u32,
    pub dataset_digest: Digest,
    pub model: MicroCheckpointBundle,
    pub first_moment: Vec<f32>,
    pub second_moment: Vec<f32>,
    pub optimizer_step: u64,
    pub learning_rate: f32,
    pub weight_decay: f32,
    pub beta1: f32,
    pub beta2: f32,
    pub epsilon: f32,
    pub rng_seed: u64,
    pub plan: BatchPlan,
    pub epoch: usize,
    pub status: TrainingRunStatus,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct TrainingCheckpointProvenance {
    pub dataset: Digest,
    pub sampler: Digest,
    pub code: Digest,
    pub provenance: Digest,
    pub metrics: Digest,
}

impl TrainingCheckpointProvenance {
    fn validate(self) -> Result<(), TrainingError> {
        if [
            self.dataset,
            self.sampler,
            self.code,
            self.provenance,
            self.metrics,
        ]
        .contains(&Digest::ZERO)
        {
            return Err(TrainingError::Config(
                "checkpoint provenance identities must be non-zero".into(),
            ));
        }
        Ok(())
    }
}

impl MicroTrainingCheckpoint {
    pub const FORMAT_VERSION: u32 = 1;

    pub fn identity(&self) -> Result<Digest, TrainingError> {
        let encoded =
            serde_json::to_vec(self).map_err(|error| TrainingError::Config(error.to_string()))?;
        let mut bytes = b"reflex.micro.training.checkpoint.v1\0".to_vec();
        bytes.extend_from_slice(&encoded);
        Ok(Digest::hash_blake3(&bytes))
    }

    pub fn persist(&self, path: &Path) -> Result<Digest, TrainingError> {
        self.validate()?;
        let encoded =
            serde_json::to_vec(self).map_err(|error| TrainingError::Config(error.to_string()))?;
        let parent = path.parent().ok_or_else(|| {
            TrainingError::Config("checkpoint path has no parent directory".into())
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        let temporary = path.with_extension("rfx-checkpoint.tmp");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        file.write_all(&encoded)
            .and_then(|()| file.sync_all())
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        std::fs::rename(&temporary, path)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        self.identity()
    }

    pub fn load(path: &Path, expected_identity: Digest) -> Result<Self, TrainingError> {
        let bytes =
            std::fs::read(path).map_err(|error| TrainingError::Config(error.to_string()))?;
        let checkpoint: Self = serde_json::from_slice(&bytes)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        checkpoint.validate()?;
        let actual = checkpoint.identity()?;
        if actual != expected_identity {
            return Err(TrainingError::Config(format!(
                "training checkpoint identity mismatch: expected {expected_identity}, found {actual}"
            )));
        }
        Ok(checkpoint)
    }

    fn validate(&self) -> Result<(), TrainingError> {
        if self.format_version != Self::FORMAT_VERSION || self.dataset_digest == Digest::ZERO {
            return Err(TrainingError::Config(
                "invalid checkpoint version or dataset identity".into(),
            ));
        }
        let model = MicroMlp::from_checkpoint(&self.model)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        let trainer = MicroTrainer {
            model,
            m_weights: self.first_moment.clone(),
            v_weights: self.second_moment.clone(),
            step: self.optimizer_step,
            lr: self.learning_rate,
            weight_decay: self.weight_decay,
            beta1: self.beta1,
            beta2: self.beta2,
            eps: self.epsilon,
            rng_seed: self.rng_seed,
        };
        trainer
            .validate()
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        if self.model.manifest.optimizer_digest != Some(trainer.optimizer_state_digest())
            || self.model.manifest.rng_digest
                != Some(Digest::hash_blake3(&self.rng_seed.to_le_bytes()))
            || self.model.manifest.step != self.optimizer_step
            || self.model.manifest.inference_only
        {
            return Err(TrainingError::Config(
                "checkpoint manifest does not commit to optimizer, RNG, and step state".into(),
            ));
        }
        self.model
            .manifest
            .require_resumable()
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        if self.model.manifest.dataset_digest != Some(self.dataset_digest)
            || self.model.manifest.epoch != self.epoch as u64
            || self.model.manifest.batch_position != self.plan.next_batch as u64
        {
            return Err(TrainingError::Config(
                "checkpoint manifest disagrees with training restart state".into(),
            ));
        }
        if self.plan.next_batch > self.plan.group_indices.len() {
            return Err(TrainingError::Config(
                "checkpoint batch position exceeds its deterministic plan".into(),
            ));
        }
        if !self.plan.eval_excluded.is_empty()
            && (self.plan.source_identities.len() != self.plan.group_indices.len()
                || self
                    .plan
                    .source_identities
                    .iter()
                    .any(|source| self.plan.eval_excluded.contains(source)))
        {
            return Err(TrainingError::Config(
                "checkpoint batch plan cannot prove evaluation exclusion".into(),
            ));
        }
        Ok(())
    }

    fn into_trainer(self) -> Result<MicroTrainer, TrainingError> {
        self.validate()?;
        let model = MicroMlp::from_checkpoint(&self.model)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        Ok(MicroTrainer {
            model,
            m_weights: self.first_moment,
            v_weights: self.second_moment,
            step: self.optimizer_step,
            lr: self.learning_rate,
            weight_decay: self.weight_decay,
            beta1: self.beta1,
            beta2: self.beta2,
            eps: self.epsilon,
            rng_seed: self.rng_seed,
        })
    }
}

impl TrainingSession {
    pub fn new_micro(trainer: MicroTrainer, plan: BatchPlan) -> Self {
        let step = trainer.step;
        Self {
            micro_trainer: Some(trainer),
            burn_model: None,
            step,
            plan,
            epoch: 0,
            status: TrainingRunStatus::Running,
        }
    }

    pub fn resume_from_plan(trainer: MicroTrainer, plan: BatchPlan) -> Self {
        Self::new_micro(trainer, plan)
    }

    pub fn checkpoint(
        &self,
        checkpoint_provenance: TrainingCheckpointProvenance,
    ) -> Result<MicroTrainingCheckpoint, TrainingError> {
        checkpoint_provenance.validate()?;
        let trainer = self
            .micro_trainer
            .as_ref()
            .ok_or_else(|| TrainingError::Config("session is not a micro trainer".into()))?;
        trainer
            .validate()
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        if self.step != trainer.step {
            return Err(TrainingError::Config(
                "session and micro optimizer step disagree".into(),
            ));
        }
        let mut model = trainer.model.to_manifest();
        model.manifest.optimizer_digest = Some(trainer.optimizer_state_digest());
        model.manifest.rng_digest = Some(Digest::hash_blake3(&trainer.rng_seed.to_le_bytes()));
        model.manifest.step = trainer.step;
        model.manifest.epoch = self.epoch as u64;
        model.manifest.batch_position = self.plan.next_batch as u64;
        model.manifest.dataset_digest = Some(checkpoint_provenance.dataset);
        model.manifest.sampler_digest = Some(checkpoint_provenance.sampler);
        model.manifest.code_identity = Some(checkpoint_provenance.code);
        model.manifest.provenance_digest = Some(checkpoint_provenance.provenance);
        model.manifest.metrics_digest = checkpoint_provenance.metrics;
        model.manifest.inference_only = false;
        model
            .manifest
            .seal()
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        Ok(MicroTrainingCheckpoint {
            format_version: MicroTrainingCheckpoint::FORMAT_VERSION,
            dataset_digest: checkpoint_provenance.dataset,
            model,
            first_moment: trainer.m_weights.clone(),
            second_moment: trainer.v_weights.clone(),
            optimizer_step: trainer.step,
            learning_rate: trainer.lr,
            weight_decay: trainer.weight_decay,
            beta1: trainer.beta1,
            beta2: trainer.beta2,
            epsilon: trainer.eps,
            rng_seed: trainer.rng_seed,
            plan: self.plan.clone(),
            epoch: self.epoch,
            status: self.status,
        })
    }

    pub fn resume(checkpoint: MicroTrainingCheckpoint) -> Result<Self, TrainingError> {
        let plan = checkpoint.plan.clone();
        let epoch = checkpoint.epoch;
        let status = checkpoint.status;
        let step = checkpoint.optimizer_step;
        let trainer = checkpoint.into_trainer()?;
        Ok(Self {
            micro_trainer: Some(trainer),
            burn_model: None,
            step,
            plan,
            epoch,
            status,
        })
    }

    pub fn cancel_and_checkpoint(
        &mut self,
        checkpoint_provenance: TrainingCheckpointProvenance,
        path: &Path,
    ) -> Result<Digest, TrainingError> {
        self.status = TrainingRunStatus::Cancelled;
        self.checkpoint(checkpoint_provenance)?.persist(path)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SamplerKind {
    Uniform,
    BalancedFamilyStratum,
    SupervisionDensity,
    PolicyDisagreement,
    RareStateShape,
    CensoredFrontier,
    KnnSuccessOnly,
}

impl SamplerKind {
    fn tag(self) -> u8 {
        match self {
            Self::Uniform => 0,
            Self::BalancedFamilyStratum => 1,
            Self::SupervisionDensity => 2,
            Self::PolicyDisagreement => 3,
            Self::RareStateShape => 4,
            Self::CensoredFrontier => 5,
            Self::KnnSuccessOnly => 6,
        }
    }
}

/// Metadata used for sampling. The dataset compiler owns these facts; the
/// sampler never infers scientific labels from features or policy names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SamplingRecord {
    pub group_index: usize,
    pub group_identity: Digest,
    pub source_identity: Digest,
    pub family: String,
    pub stratum: String,
    pub state_shape: String,
    pub source_policy: String,
    pub duplicate_identity: Digest,
    pub supervision_density: f32,
    pub policy_disagreement: f32,
    pub censored_frontier: bool,
    pub knn_success: bool,
}

impl SamplingRecord {
    fn validate(&self) -> Result<(), TrainingError> {
        if self.group_identity == Digest::ZERO
            || self.source_identity == Digest::ZERO
            || self.duplicate_identity == Digest::ZERO
            || self.family.trim().is_empty()
            || self.stratum.trim().is_empty()
            || self.state_shape.trim().is_empty()
            || self.source_policy.trim().is_empty()
            || !self.supervision_density.is_finite()
            || !(0.0..=1.0).contains(&self.supervision_density)
            || !self.policy_disagreement.is_finite()
            || !(0.0..=1.0).contains(&self.policy_disagreement)
        {
            return Err(TrainingError::Sampling(format!(
                "invalid metadata for group {}",
                self.group_index
            )));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SamplingSpec {
    pub name: String,
    pub kind: SamplerKind,
    pub seed: u64,
    pub sample_count: usize,
    pub source_policy_cap: usize,
    pub duplicate_cap: usize,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SampledGroup {
    pub group_index: usize,
    pub group_identity: Digest,
    pub source_identity: Digest,
    /// Exact marginal inclusion probability when the policy admits a simple
    /// closed form (currently uniform sampling); absent for weighted policies.
    pub inclusion_probability: Option<f32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvaluationCorpusIdentity {
    pub name: String,
    pub identity: Digest,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SamplingManifest {
    pub format_version: u32,
    pub sampler_name: String,
    pub kind: SamplerKind,
    pub seed: u64,
    pub eligible_count: u64,
    pub selected: Vec<SampledGroup>,
    pub excluded_evaluation_corpora: Vec<EvaluationCorpusIdentity>,
}

impl SamplingManifest {
    pub const FORMAT_VERSION: u32 = 1;

    pub fn validate(&self) -> Result<(), TrainingError> {
        if self.format_version != Self::FORMAT_VERSION || self.sampler_name.trim().is_empty() {
            return Err(TrainingError::Sampling(
                "invalid sampling manifest header".into(),
            ));
        }
        if self.selected.len() as u64 > self.eligible_count {
            return Err(TrainingError::Sampling(
                "selected count exceeds eligible count".into(),
            ));
        }
        let excluded: BTreeSet<_> = self
            .excluded_evaluation_corpora
            .iter()
            .map(|corpus| corpus.identity)
            .collect();
        let names: BTreeSet<_> = self
            .excluded_evaluation_corpora
            .iter()
            .map(|corpus| corpus.name.as_str())
            .collect();
        if excluded.len() != self.excluded_evaluation_corpora.len()
            || names.len() != self.excluded_evaluation_corpora.len()
            || excluded.contains(&Digest::ZERO)
            || self
                .excluded_evaluation_corpora
                .iter()
                .any(|corpus| corpus.name.trim().is_empty())
            || !self
                .excluded_evaluation_corpora
                .windows(2)
                .all(|pair| pair[0].name < pair[1].name)
        {
            return Err(TrainingError::Sampling(
                "evaluation identities must be unique and non-zero".into(),
            ));
        }
        let mut groups = BTreeSet::new();
        for entry in &self.selected {
            if entry.group_identity == Digest::ZERO
                || entry.source_identity == Digest::ZERO
                || excluded.contains(&entry.source_identity)
                || !groups.insert(entry.group_identity)
                || entry.inclusion_probability.is_some_and(|probability| {
                    !probability.is_finite() || probability <= 0.0 || probability > 1.0
                })
            {
                return Err(TrainingError::Sampling(
                    "invalid, duplicate, or evaluation-owned selected group".into(),
                ));
            }
        }
        Ok(())
    }

    /// Stable byte encoding used for content identity. All variable-length
    /// collections have explicit lengths and evaluation identities are sorted.
    pub fn canonical_bytes(&self) -> Result<Vec<u8>, TrainingError> {
        self.validate()?;
        let mut bytes = b"reflex.sampling-manifest.v1\0".to_vec();
        bytes.extend_from_slice(&self.format_version.to_le_bytes());
        append_string(&mut bytes, &self.sampler_name)?;
        bytes.push(self.kind.tag());
        bytes.extend_from_slice(&self.seed.to_le_bytes());
        bytes.extend_from_slice(&self.eligible_count.to_le_bytes());
        append_len(&mut bytes, self.selected.len())?;
        for entry in &self.selected {
            bytes.extend_from_slice(&(entry.group_index as u64).to_le_bytes());
            append_digest(&mut bytes, entry.group_identity);
            append_digest(&mut bytes, entry.source_identity);
            match entry.inclusion_probability {
                Some(probability) => {
                    bytes.push(1);
                    bytes.extend_from_slice(&probability.to_bits().to_le_bytes());
                }
                None => bytes.push(0),
            }
        }
        append_len(&mut bytes, self.excluded_evaluation_corpora.len())?;
        for corpus in &self.excluded_evaluation_corpora {
            append_string(&mut bytes, &corpus.name)?;
            append_digest(&mut bytes, corpus.identity);
        }
        Ok(bytes)
    }

    pub fn identity(&self) -> Result<Digest, TrainingError> {
        Ok(Digest::hash_blake3(&self.canonical_bytes()?))
    }
}

pub struct SamplerRegistry {
    pub eval_ids: BTreeSet<Digest>,
    pub evaluation_corpora: BTreeMap<String, Digest>,
    pub min_family_coverage: usize,
}

impl SamplerRegistry {
    pub fn new(eval_ids: HashSet<Digest>) -> Self {
        let eval_ids: BTreeSet<_> = eval_ids.into_iter().collect();
        Self {
            evaluation_corpora: eval_ids
                .iter()
                .map(|identity| (format!("evaluation-{identity}"), *identity))
                .collect(),
            eval_ids,
            min_family_coverage: 1,
        }
    }

    pub fn with_evaluation_corpora(
        corpora: Vec<EvaluationCorpusIdentity>,
    ) -> Result<Self, TrainingError> {
        let mut evaluation_corpora = BTreeMap::new();
        let mut eval_ids = BTreeSet::new();
        for corpus in corpora {
            if corpus.name.trim().is_empty()
                || corpus.identity == Digest::ZERO
                || evaluation_corpora
                    .insert(corpus.name, corpus.identity)
                    .is_some()
                || !eval_ids.insert(corpus.identity)
            {
                return Err(TrainingError::Sampling(
                    "evaluation corpus names and identities must be unique and non-zero".into(),
                ));
            }
        }
        Ok(Self {
            eval_ids,
            evaluation_corpora,
            min_family_coverage: 1,
        })
    }

    pub fn sample(
        &self,
        records: &[SamplingRecord],
        spec: &SamplingSpec,
    ) -> Result<SamplingManifest, TrainingError> {
        if spec.name.trim().is_empty()
            || spec.sample_count == 0
            || spec.source_policy_cap == 0
            || spec.duplicate_cap == 0
            || self.eval_ids.contains(&Digest::ZERO)
        {
            return Err(TrainingError::Sampling(
                "invalid sampling specification".into(),
            ));
        }
        let mut group_ids = BTreeSet::new();
        let mut shape_counts = BTreeMap::<&str, usize>::new();
        let mut candidates = Vec::new();
        for record in records {
            record.validate()?;
            if !group_ids.insert(record.group_identity) {
                return Err(TrainingError::Sampling(format!(
                    "duplicate group identity {}",
                    record.group_identity
                )));
            }
            if self.eval_ids.contains(&record.source_identity) {
                continue;
            }
            if matches!(spec.kind, SamplerKind::CensoredFrontier) && !record.censored_frontier {
                continue;
            }
            if matches!(spec.kind, SamplerKind::KnnSuccessOnly) && !record.knn_success {
                continue;
            }
            *shape_counts.entry(&record.state_shape).or_default() += 1;
            candidates.push(record);
        }
        let eligible_count = candidates.len();
        if eligible_count == 0 {
            return Err(TrainingError::InsufficientData);
        }
        let mut eligible_sources = BTreeMap::<&str, usize>::new();
        let mut eligible_duplicates = BTreeMap::<Digest, usize>::new();
        for record in &candidates {
            *eligible_sources.entry(&record.source_policy).or_default() += 1;
            *eligible_duplicates
                .entry(record.duplicate_identity)
                .or_default() += 1;
        }
        let caps_did_not_shape_selection = eligible_sources
            .values()
            .all(|count| *count <= spec.source_policy_cap)
            && eligible_duplicates
                .values()
                .all(|count| *count <= spec.duplicate_cap);
        candidates
            .sort_by_key(|record| sampling_priority(record, spec, &shape_counts, eligible_count));

        let mut selected = Vec::with_capacity(spec.sample_count.min(eligible_count));
        let mut selected_ids = BTreeSet::new();
        let mut source_counts = BTreeMap::<&str, usize>::new();
        let mut duplicate_counts = BTreeMap::<Digest, usize>::new();
        if matches!(spec.kind, SamplerKind::BalancedFamilyStratum) {
            let mut buckets = BTreeMap::<(&str, &str), Vec<&SamplingRecord>>::new();
            for record in &candidates {
                buckets
                    .entry((&record.family, &record.stratum))
                    .or_default()
                    .push(record);
            }
            let required = buckets.len().saturating_mul(self.min_family_coverage);
            if required > spec.sample_count {
                return Err(TrainingError::Sampling(format!(
                    "sample count {} cannot provide configured coverage {required}",
                    spec.sample_count
                )));
            }
            for bucket in buckets.values() {
                let mut covered = 0;
                for record in bucket {
                    if try_select(
                        record,
                        spec,
                        &mut selected_ids,
                        &mut source_counts,
                        &mut duplicate_counts,
                    ) {
                        selected.push(*record);
                        covered += 1;
                        if covered == self.min_family_coverage {
                            break;
                        }
                    }
                }
                if covered != self.min_family_coverage {
                    return Err(TrainingError::Sampling(
                        "source-policy or duplicate caps make minimum coverage impossible".into(),
                    ));
                }
            }
        }
        for record in candidates {
            if selected.len() == spec.sample_count {
                break;
            }
            if try_select(
                record,
                spec,
                &mut selected_ids,
                &mut source_counts,
                &mut duplicate_counts,
            ) {
                selected.push(record);
            }
        }
        if selected.len() < spec.sample_count.min(eligible_count) {
            return Err(TrainingError::Sampling(format!(
                "caps permit only {} of {} requested groups",
                selected.len(),
                spec.sample_count
            )));
        }
        let uniform_probability = (matches!(spec.kind, SamplerKind::Uniform)
            && caps_did_not_shape_selection)
            .then_some((spec.sample_count.min(eligible_count) as f32) / eligible_count as f32);
        let excluded_evaluation_corpora = self
            .evaluation_corpora
            .iter()
            .map(|(name, identity)| EvaluationCorpusIdentity {
                name: name.clone(),
                identity: *identity,
            })
            .collect();
        let manifest = SamplingManifest {
            format_version: SamplingManifest::FORMAT_VERSION,
            sampler_name: spec.name.clone(),
            kind: spec.kind,
            seed: spec.seed,
            eligible_count: eligible_count as u64,
            selected: selected
                .into_iter()
                .map(|record| SampledGroup {
                    group_index: record.group_index,
                    group_identity: record.group_identity,
                    source_identity: record.source_identity,
                    inclusion_probability: uniform_probability,
                })
                .collect(),
            excluded_evaluation_corpora,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn knn_success_only(&self) -> SamplerKind {
        SamplerKind::KnnSuccessOnly
    }
}

fn try_select<'a>(
    record: &'a SamplingRecord,
    spec: &SamplingSpec,
    selected_ids: &mut BTreeSet<Digest>,
    source_counts: &mut BTreeMap<&'a str, usize>,
    duplicate_counts: &mut BTreeMap<Digest, usize>,
) -> bool {
    if selected_ids.contains(&record.group_identity)
        || source_counts
            .get(record.source_policy.as_str())
            .copied()
            .unwrap_or(0)
            >= spec.source_policy_cap
        || duplicate_counts
            .get(&record.duplicate_identity)
            .copied()
            .unwrap_or(0)
            >= spec.duplicate_cap
    {
        return false;
    }
    selected_ids.insert(record.group_identity);
    *source_counts.entry(&record.source_policy).or_default() += 1;
    *duplicate_counts
        .entry(record.duplicate_identity)
        .or_default() += 1;
    true
}

fn sampling_priority(
    record: &SamplingRecord,
    spec: &SamplingSpec,
    shape_counts: &BTreeMap<&str, usize>,
    eligible_count: usize,
) -> (u128, Digest) {
    let mut bytes = b"reflex.sampler.priority.v1\0".to_vec();
    bytes.extend_from_slice(&spec.seed.to_le_bytes());
    bytes.push(spec.kind.tag());
    append_digest(&mut bytes, record.group_identity);
    let digest = Digest::hash_blake3(&bytes);
    let raw = u64::from_le_bytes(digest.bytes[..8].try_into().expect("digest width"));
    let weight: f64 = match spec.kind {
        SamplerKind::Uniform | SamplerKind::CensoredFrontier | SamplerKind::KnnSuccessOnly => 1.0,
        SamplerKind::BalancedFamilyStratum => 1.0,
        SamplerKind::SupervisionDensity => f64::from(record.supervision_density.max(1.0e-6)),
        SamplerKind::PolicyDisagreement => f64::from(record.policy_disagreement.max(1.0e-6)),
        SamplerKind::RareStateShape => {
            eligible_count as f64
                / *shape_counts
                    .get(record.state_shape.as_str())
                    .unwrap_or(&eligible_count) as f64
        }
    };
    let scaled_weight = (weight * 1_000_000.0).max(1.0) as u128;
    (
        u128::from(raw).saturating_mul(1_000_000) / scaled_weight,
        digest,
    )
}

fn append_len(bytes: &mut Vec<u8>, length: usize) -> Result<(), TrainingError> {
    let length = u32::try_from(length)
        .map_err(|_| TrainingError::Config("canonical collection is too large".into()))?;
    bytes.extend_from_slice(&length.to_le_bytes());
    Ok(())
}

fn append_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), TrainingError> {
    append_len(bytes, value.len())?;
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn append_digest(bytes: &mut Vec<u8>, digest: Digest) {
    bytes.push(match digest.algorithm {
        reflex_types::DigestAlgorithm::Blake3 => 0,
        reflex_types::DigestAlgorithm::Sha256 => 1,
    });
    bytes.extend_from_slice(&digest.bytes);
}

pub const M1_5_PARAMETER_POINTS: [u64; 6] = [519, 1_026, 2_607, 9_614, 29_538, 99_902];

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ArchitectureSpec {
    Linear { input_dim: u32 },
    BottleneckMlp { input_dim: u32, bottleneck: u32 },
    Mlp { input_dim: u32, hidden: Vec<u32> },
}

impl ArchitectureSpec {
    pub fn exact_linear(parameter_count: u64) -> Result<Self, TrainingError> {
        let input_dim = parameter_count
            .checked_sub(1)
            .and_then(|value| u32::try_from(value).ok())
            .filter(|value| *value > 0)
            .ok_or_else(|| TrainingError::Sweep("invalid exact linear parameter count".into()))?;
        Ok(Self::Linear { input_dim })
    }

    pub fn parameter_count(&self) -> Result<u64, TrainingError> {
        let dimensions: Vec<u64> = match self {
            Self::Linear { input_dim } => vec![u64::from(*input_dim), 1],
            Self::BottleneckMlp {
                input_dim,
                bottleneck,
            } => vec![u64::from(*input_dim), u64::from(*bottleneck), 1],
            Self::Mlp { input_dim, hidden } => {
                if hidden.is_empty() {
                    return Err(TrainingError::Sweep(
                        "MLP architecture requires at least one hidden layer".into(),
                    ));
                }
                let mut dimensions = Vec::with_capacity(hidden.len() + 2);
                dimensions.push(u64::from(*input_dim));
                dimensions.extend(hidden.iter().copied().map(u64::from));
                dimensions.push(1);
                dimensions
            }
        };
        if dimensions.contains(&0) {
            return Err(TrainingError::Sweep(
                "architecture dimensions must be non-zero".into(),
            ));
        }
        dimensions.windows(2).try_fold(0_u64, |sum, pair| {
            let layer = pair[0]
                .checked_mul(pair[1])
                .and_then(|weights| weights.checked_add(pair[1]))
                .ok_or_else(|| TrainingError::Sweep("parameter count overflow".into()))?;
            sum.checked_add(layer)
                .ok_or_else(|| TrainingError::Sweep("parameter count overflow".into()))
        })
    }

    fn encode(&self, bytes: &mut Vec<u8>) -> Result<(), TrainingError> {
        match self {
            Self::Linear { input_dim } => {
                bytes.push(0);
                bytes.extend_from_slice(&input_dim.to_le_bytes());
            }
            Self::BottleneckMlp {
                input_dim,
                bottleneck,
            } => {
                bytes.push(1);
                bytes.extend_from_slice(&input_dim.to_le_bytes());
                bytes.extend_from_slice(&bottleneck.to_le_bytes());
            }
            Self::Mlp { input_dim, hidden } => {
                bytes.push(2);
                bytes.extend_from_slice(&input_dim.to_le_bytes());
                append_len(bytes, hidden.len())?;
                for width in hidden {
                    bytes.extend_from_slice(&width.to_le_bytes());
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LossSpec {
    PairwiseLogistic,
    MaskedListwise { temperature_millis: u32 },
    SingleRoute,
    ProofDag,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum TrainingBackend {
    Micro,
    BurnNdArray,
    BurnFlex,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectionDirection {
    Minimize,
    Maximize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DevelopmentSelectionRule {
    pub metric: String,
    pub direction: SelectionDirection,
}

/// Immutable sweep input. Construction validates every axis before a cell can
/// be materialized, and the selection rule is explicitly development-only.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepSpec {
    name: String,
    architectures: Vec<ArchitectureSpec>,
    losses: Vec<LossSpec>,
    datasets: Vec<Digest>,
    seeds: Vec<u64>,
    lineages: Vec<Digest>,
    backends: Vec<TrainingBackend>,
    selection: DevelopmentSelectionRule,
    exclusions: BTreeMap<Digest, String>,
}

impl SweepSpec {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        name: String,
        architectures: Vec<ArchitectureSpec>,
        losses: Vec<LossSpec>,
        datasets: Vec<Digest>,
        seeds: Vec<u64>,
        lineages: Vec<Digest>,
        backends: Vec<TrainingBackend>,
        selection: DevelopmentSelectionRule,
        exclusions: BTreeMap<Digest, String>,
    ) -> Result<Self, TrainingError> {
        let spec = Self {
            name,
            architectures,
            losses,
            datasets,
            seeds,
            lineages,
            backends,
            selection,
            exclusions,
        };
        spec.validate()?;
        Ok(spec)
    }

    pub fn validate(&self) -> Result<(), TrainingError> {
        if self.name.trim().is_empty()
            || self.architectures.is_empty()
            || self.losses.is_empty()
            || self.datasets.is_empty()
            || self.seeds.is_empty()
            || self.lineages.is_empty()
            || self.backends.is_empty()
            || self.selection.metric.trim().is_empty()
            || self.datasets.contains(&Digest::ZERO)
            || self.lineages.contains(&Digest::ZERO)
            || self
                .exclusions
                .iter()
                .any(|(identity, reason)| *identity == Digest::ZERO || reason.trim().is_empty())
        {
            return Err(TrainingError::Sweep("invalid sweep specification".into()));
        }
        for architecture in &self.architectures {
            architecture.parameter_count()?;
        }
        Ok(())
    }

    pub fn cells(&self) -> Result<SweepPlan, TrainingError> {
        self.validate()?;
        let mut unique = BTreeMap::new();
        let mut generated = 0_usize;
        for architecture in &self.architectures {
            for loss in &self.losses {
                for dataset in &self.datasets {
                    for seed in &self.seeds {
                        for lineage in &self.lineages {
                            for backend in &self.backends {
                                generated = generated.checked_add(1).ok_or_else(|| {
                                    TrainingError::Sweep("sweep cross-product overflow".into())
                                })?;
                                let cell = SweepCell {
                                    architecture: architecture.clone(),
                                    loss: loss.clone(),
                                    dataset: *dataset,
                                    seed: *seed,
                                    lineage: *lineage,
                                    backend: backend.clone(),
                                };
                                let identity = cell.identity()?;
                                unique.entry(identity).or_insert(cell);
                            }
                        }
                    }
                }
            }
        }
        if self
            .exclusions
            .keys()
            .any(|identity| !unique.contains_key(identity))
        {
            return Err(TrainingError::Sweep(
                "sweep exclusion references a cell outside the cross-product".into(),
            ));
        }
        let mut runnable = Vec::new();
        let mut exclusions = BTreeMap::new();
        for (identity, cell) in unique {
            if let Some(reason) = self.exclusions.get(&identity) {
                exclusions.insert(identity, reason.clone());
            } else {
                runnable.push(IdentifiedSweepCell { identity, cell });
            }
        }
        Ok(SweepPlan {
            sweep_identity: self.identity()?,
            generated_cells: generated,
            deduplicated_cells: runnable.len() + exclusions.len(),
            runnable,
            exclusions,
            selection: self.selection.clone(),
        })
    }

    pub fn identity(&self) -> Result<Digest, TrainingError> {
        self.validate()?;
        let bytes =
            serde_json::to_vec(self).map_err(|error| TrainingError::Sweep(error.to_string()))?;
        let mut canonical = b"reflex.sweep-spec.v1\0".to_vec();
        canonical.extend_from_slice(&bytes);
        Ok(Digest::hash_blake3(&canonical))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepCell {
    pub architecture: ArchitectureSpec,
    pub loss: LossSpec,
    pub dataset: Digest,
    pub seed: u64,
    pub lineage: Digest,
    pub backend: TrainingBackend,
}

impl SweepCell {
    pub fn identity(&self) -> Result<Digest, TrainingError> {
        if self.dataset == Digest::ZERO || self.lineage == Digest::ZERO {
            return Err(TrainingError::Sweep(
                "cell dataset and lineage identities must be non-zero".into(),
            ));
        }
        let mut bytes = b"reflex.sweep-cell.v1\0".to_vec();
        self.architecture.encode(&mut bytes)?;
        bytes.push(match &self.loss {
            LossSpec::PairwiseLogistic => 0,
            LossSpec::MaskedListwise { .. } => 1,
            LossSpec::SingleRoute => 2,
            LossSpec::ProofDag => 3,
        });
        if let LossSpec::MaskedListwise { temperature_millis } = &self.loss {
            if *temperature_millis == 0 {
                return Err(TrainingError::Sweep(
                    "listwise temperature must be non-zero".into(),
                ));
            }
            bytes.extend_from_slice(&temperature_millis.to_le_bytes());
        }
        append_digest(&mut bytes, self.dataset);
        bytes.extend_from_slice(&self.seed.to_le_bytes());
        append_digest(&mut bytes, self.lineage);
        bytes.push(match self.backend {
            TrainingBackend::Micro => 0,
            TrainingBackend::BurnNdArray => 1,
            TrainingBackend::BurnFlex => 2,
        });
        Ok(Digest::hash_blake3(&bytes))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdentifiedSweepCell {
    pub identity: Digest,
    pub cell: SweepCell,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepPlan {
    pub sweep_identity: Digest,
    pub generated_cells: usize,
    pub deduplicated_cells: usize,
    pub runnable: Vec<IdentifiedSweepCell>,
    pub exclusions: BTreeMap<Digest, String>,
    pub selection: DevelopmentSelectionRule,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultCorpus {
    Train,
    Development,
    Evaluation,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SweepMetricResult {
    pub cell_identity: Digest,
    pub corpus: ResultCorpus,
    pub metric: String,
    pub value: f64,
}

impl SweepPlan {
    pub fn select_development(
        &self,
        results: &[SweepMetricResult],
    ) -> Result<Digest, TrainingError> {
        let runnable: BTreeSet<_> = self.runnable.iter().map(|cell| cell.identity).collect();
        let mut eligible = BTreeMap::new();
        for result in results {
            if result.metric != self.selection.metric {
                continue;
            }
            if result.corpus != ResultCorpus::Development {
                return Err(TrainingError::Sweep(
                    "selection metric may consume development results only".into(),
                ));
            }
            if !runnable.contains(&result.cell_identity) || !result.value.is_finite() {
                return Err(TrainingError::Sweep(
                    "selection result is non-finite or references a non-runnable cell".into(),
                ));
            }
            if eligible
                .insert(result.cell_identity, result.value)
                .is_some()
            {
                return Err(TrainingError::Sweep(
                    "selection has duplicate metric results for a cell".into(),
                ));
            }
        }
        if eligible.len() != runnable.len() {
            return Err(TrainingError::Sweep(
                "selection requires one development result for every runnable cell".into(),
            ));
        }
        eligible
            .into_iter()
            .min_by(|a, b| {
                let ordering = a.1.total_cmp(&b.1);
                let ordering = match self.selection.direction {
                    SelectionDirection::Minimize => ordering,
                    SelectionDirection::Maximize => ordering.reverse(),
                };
                ordering.then_with(|| a.0.cmp(&b.0))
            })
            .map(|(identity, _)| identity)
            .ok_or_else(|| TrainingError::Sweep("no development result for selection".into()))
    }

    pub fn reconcile(&self, completed: &[Digest]) -> Result<SweepReconciliation, TrainingError> {
        let expected: BTreeSet<_> = self.runnable.iter().map(|cell| cell.identity).collect();
        let actual: BTreeSet<_> = completed.iter().copied().collect();
        if actual.len() != completed.len() || !actual.is_subset(&expected) {
            return Err(TrainingError::Sweep(
                "completed cells contain duplicates or unexpected identities".into(),
            ));
        }
        Ok(SweepReconciliation {
            completed: actual.iter().copied().collect(),
            missing: expected.difference(&actual).copied().collect(),
            exclusions: self.exclusions.clone(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SweepReconciliation {
    pub completed: Vec<Digest>,
    pub missing: Vec<Digest>,
    pub exclusions: BTreeMap<Digest, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BurnLoopStatus {
    Pending,
    Running,
    Cancelled,
    Completed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BurnLoopMetrics {
    pub epoch_losses: Vec<f64>,
    pub total_steps: u64,
    pub supervised_candidates: u64,
    pub train_cpu_ns: u64,
    pub backend: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BurnLoopState {
    pub status: BurnLoopStatus,
    pub epoch: usize,
    pub plan: BatchPlan,
    pub metrics: BurnLoopMetrics,
    pub current_epoch_loss_sum: f64,
    pub current_epoch_steps: u64,
    pub last_checkpoint_step: u64,
}

impl BurnLoopState {
    pub fn new(plan: BatchPlan) -> Self {
        Self {
            status: BurnLoopStatus::Pending,
            epoch: 0,
            plan,
            metrics: BurnLoopMetrics {
                epoch_losses: Vec::new(),
                total_steps: 0,
                supervised_candidates: 0,
                train_cpu_ns: 0,
                backend: "burn-flex".into(),
            },
            current_epoch_loss_sum: 0.0,
            current_epoch_steps: 0,
            last_checkpoint_step: 0,
        }
    }

    pub fn resume_cancelled(&mut self) -> Result<(), TrainingError> {
        if self.status != BurnLoopStatus::Cancelled {
            return Err(TrainingError::Config(
                "only a cancelled Burn loop can be explicitly resumed".into(),
            ));
        }
        self.status = BurnLoopStatus::Running;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BurnLoopCheckpoint {
    pub format_version: u32,
    pub training: BurnTrainingCheckpoint,
    pub state: BurnLoopState,
    pub checkpoint_provenance: TrainingCheckpointProvenance,
}

impl BurnLoopCheckpoint {
    pub const FORMAT_VERSION: u32 = 1;

    pub fn validate(&self) -> Result<(), TrainingError> {
        if self.format_version != Self::FORMAT_VERSION {
            return Err(TrainingError::Config(
                "unsupported Burn loop checkpoint version".into(),
            ));
        }
        self.checkpoint_provenance.validate()?;
        if self.training.step != self.state.metrics.total_steps
            || self.state.plan.next_batch > self.state.plan.group_indices.len()
            || self.state.metrics.backend != "burn-flex"
            || !self.state.current_epoch_loss_sum.is_finite()
            || self
                .state
                .metrics
                .epoch_losses
                .iter()
                .any(|loss| !loss.is_finite())
        {
            return Err(TrainingError::Config(
                "Burn checkpoint disagrees with loop state or contains non-finite metrics".into(),
            ));
        }
        // Restore performs the owning crate's full digest, shape, optimizer,
        // learning-rate, and tensor validation.
        BurnTrainingSession::restore(&self.training)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        Ok(())
    }

    pub fn identity(&self) -> Result<Digest, TrainingError> {
        self.validate()?;
        let encoded =
            serde_json::to_vec(self).map_err(|error| TrainingError::Config(error.to_string()))?;
        let mut bytes = b"reflex.burn-loop-checkpoint.v1\0".to_vec();
        bytes.extend_from_slice(&encoded);
        Ok(Digest::hash_blake3(&bytes))
    }

    pub fn persist(&self, path: &Path) -> Result<Digest, TrainingError> {
        let identity = self.identity()?;
        let encoded =
            serde_json::to_vec(self).map_err(|error| TrainingError::Config(error.to_string()))?;
        let parent = path.parent().ok_or_else(|| {
            TrainingError::Config("checkpoint path has no parent directory".into())
        })?;
        std::fs::create_dir_all(parent)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        let temporary = path.with_extension("burn-loop.tmp");
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        file.write_all(&encoded)
            .and_then(|()| file.sync_all())
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        std::fs::rename(temporary, path)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        Ok(identity)
    }

    pub fn load(path: &Path, expected: Digest) -> Result<Self, TrainingError> {
        let bytes =
            std::fs::read(path).map_err(|error| TrainingError::Config(error.to_string()))?;
        let checkpoint: Self = serde_json::from_slice(&bytes)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        let actual = checkpoint.identity()?;
        if actual != expected {
            return Err(TrainingError::Config(format!(
                "Burn loop checkpoint identity mismatch: expected {expected}, found {actual}"
            )));
        }
        Ok(checkpoint)
    }

    pub fn restore_session_and_state(
        &self,
    ) -> Result<(BurnTrainingSession, BurnLoopState), TrainingError> {
        self.validate()?;
        let session = BurnTrainingSession::restore(&self.training)
            .map_err(|error| TrainingError::Config(error.to_string()))?;
        Ok((session, self.state.clone()))
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct BurnLoopConfig {
    pub epochs: usize,
    pub groups_per_batch: usize,
    pub checkpoint_every_steps: u64,
    pub temperature: f32,
}

#[allow(clippy::too_many_arguments)]
pub fn run_burn_loop(
    session: &mut BurnTrainingSession,
    state: &mut BurnLoopState,
    groups: &[DecisionGroup],
    features_by_state: &HashMap<StateId, Vec<f32>>,
    feature_dim: usize,
    feature_schema: FeatureSchemaId,
    config: BurnLoopConfig,
    checkpoint_provenance: TrainingCheckpointProvenance,
    cancel: &AtomicBool,
    latest_checkpoint: &mut Option<BurnLoopCheckpoint>,
) -> Result<(), TrainingError> {
    checkpoint_provenance.validate()?;
    if config.epochs == 0
        || config.groups_per_batch == 0
        || config.checkpoint_every_steps == 0
        || feature_dim == 0
        || !config.temperature.is_finite()
        || config.temperature <= 0.0
        || state.epoch > config.epochs
        || state.metrics.total_steps != session.step()
        || matches!(
            state.status,
            BurnLoopStatus::Completed | BurnLoopStatus::Failed
        )
    {
        return Err(TrainingError::Config(
            "invalid Burn loop configuration or restart state".into(),
        ));
    }
    if state.status == BurnLoopStatus::Cancelled {
        return Err(TrainingError::Cancelled);
    }
    state.status = BurnLoopStatus::Running;
    let max_candidates = groups
        .iter()
        .map(|group| group.candidate_ids.len())
        .max()
        .unwrap_or(0)
        .saturating_mul(config.groups_per_batch);
    let mut packer =
        GroupBatchPacker::with_capacity(config.groups_per_batch, max_candidates, feature_dim);
    let mut burn_features = FeatureBatch {
        rows: 0,
        cols: feature_dim,
        values: Vec::with_capacity(max_candidates.saturating_mul(feature_dim)),
        schema: feature_schema,
    };
    let mut burn_targets = Vec::with_capacity(max_candidates);

    while state.epoch < config.epochs {
        if cancel.load(Ordering::Acquire) {
            state.status = BurnLoopStatus::Cancelled;
            state.last_checkpoint_step = state.metrics.total_steps;
            *latest_checkpoint = Some(capture_burn_checkpoint(
                session,
                state,
                checkpoint_provenance,
            )?);
            return Ok(());
        }
        let Some(batch) = packer.pack(
            &mut state.plan,
            groups,
            features_by_state,
            feature_dim,
            config.groups_per_batch,
            config.temperature,
        )?
        else {
            if state.current_epoch_steps == 0 {
                state.status = BurnLoopStatus::Failed;
                return Err(TrainingError::InsufficientData);
            }
            state
                .metrics
                .epoch_losses
                .push(state.current_epoch_loss_sum / state.current_epoch_steps as f64);
            state.current_epoch_loss_sum = 0.0;
            state.current_epoch_steps = 0;
            state.epoch += 1;
            if state.epoch < config.epochs {
                state.plan.next_batch = 0;
            }
            continue;
        };

        burn_features.values.clear();
        burn_targets.clear();
        for candidate in 0..batch.candidate_count() {
            if batch.masks[candidate] == 0.0 {
                continue;
            }
            let start = candidate * feature_dim;
            burn_features
                .values
                .extend_from_slice(&batch.features[start..start + feature_dim]);
            burn_targets.push(batch.targets[candidate]);
        }
        if burn_targets.is_empty() {
            state.status = BurnLoopStatus::Failed;
            return Err(TrainingError::InsufficientData);
        }
        burn_features.rows = burn_targets.len();
        let started = Instant::now();
        let loss = match session.train_step(&burn_features, &burn_targets) {
            Ok(loss) if loss.is_finite() => loss,
            Ok(_) => {
                state.status = BurnLoopStatus::Failed;
                return Err(TrainingError::NonFiniteLoss {
                    epoch: state.epoch,
                    batch: state.metrics.total_steps as usize,
                });
            }
            Err(error) => {
                state.status = BurnLoopStatus::Failed;
                return Err(TrainingError::Config(error.to_string()));
            }
        };
        state.metrics.train_cpu_ns = state
            .metrics
            .train_cpu_ns
            .saturating_add(started.elapsed().as_nanos() as u64);
        state.current_epoch_loss_sum += f64::from(loss);
        state.current_epoch_steps += 1;
        state.metrics.total_steps += 1;
        state.metrics.supervised_candidates = state
            .metrics
            .supervised_candidates
            .saturating_add(burn_targets.len() as u64);
        if state
            .metrics
            .total_steps
            .is_multiple_of(config.checkpoint_every_steps)
        {
            state.last_checkpoint_step = state.metrics.total_steps;
            let checkpoint = capture_burn_checkpoint(session, state, checkpoint_provenance);
            match checkpoint {
                Ok(checkpoint) => *latest_checkpoint = Some(checkpoint),
                Err(error) => {
                    state.status = BurnLoopStatus::Failed;
                    return Err(error);
                }
            }
        }
    }
    state.status = BurnLoopStatus::Completed;
    state.last_checkpoint_step = state.metrics.total_steps;
    let checkpoint = capture_burn_checkpoint(session, state, checkpoint_provenance);
    match checkpoint {
        Ok(checkpoint) => *latest_checkpoint = Some(checkpoint),
        Err(error) => {
            state.status = BurnLoopStatus::Failed;
            return Err(error);
        }
    }
    Ok(())
}

fn capture_burn_checkpoint(
    session: &BurnTrainingSession,
    state: &BurnLoopState,
    checkpoint_provenance: TrainingCheckpointProvenance,
) -> Result<BurnLoopCheckpoint, TrainingError> {
    let checkpoint = BurnLoopCheckpoint {
        format_version: BurnLoopCheckpoint::FORMAT_VERSION,
        training: session
            .checkpoint()
            .map_err(|error| TrainingError::Config(error.to_string()))?,
        state: state.clone(),
        checkpoint_provenance,
    };
    checkpoint.validate()?;
    Ok(checkpoint)
}

pub fn train_micro_model(
    mut trainer: MicroTrainer,
    groups: &[DecisionGroup],
    features_by_state: &HashMap<StateId, Vec<f32>>,
    config: &TrainingConfig,
) -> Result<(MicroMlp, TrainingMetrics), TrainingError> {
    validate_training_config(config)?;
    validate_supervision(groups)?;
    if groups.is_empty() {
        return Err(TrainingError::InsufficientData);
    }
    trainer.lr = config.learning_rate;
    trainer.weight_decay = config.weight_decay;
    trainer
        .validate()
        .map_err(|error| TrainingError::Config(error.to_string()))?;

    let mut epoch_losses = Vec::new();
    let mut total_steps = 0;

    for epoch in 0..config.epochs {
        let mut epoch_loss_sum = 0.0;
        let mut step_count = 0;

        for (group_index, group) in groups.iter().enumerate() {
            let feats =
                features_by_state
                    .get(&group.state_id)
                    .ok_or(TrainingError::MissingFeatures {
                        group: group_index,
                        state: group.state_id,
                    })?;
            let expected = group
                .candidate_ids
                .len()
                .checked_mul(trainer.model.input_dim)
                .ok_or_else(|| TrainingError::Config("feature dimension overflow".into()))?;
            if feats.len() != expected {
                return Err(TrainingError::FeatureDimension {
                    group: group_index,
                    expected,
                    found: feats.len(),
                });
            }
            if feats[..expected].iter().any(|feature| !feature.is_finite()) {
                return Err(TrainingError::Config(format!(
                    "non-finite feature in group {group_index}"
                )));
            }
            let pairs = collect_supervised_pairs(group);
            for (better, worse) in pairs {
                let loss = trainer
                    .try_train_step_pairwise(&feats[..expected], better, worse)
                    .map_err(|_| TrainingError::NonFiniteLoss {
                        epoch,
                        batch: total_steps,
                    })?;
                epoch_loss_sum += loss;
                step_count += 1;
                total_steps += 1;
            }
        }

        epoch_losses.push(if step_count > 0 {
            epoch_loss_sum / step_count as f32
        } else {
            0.0
        });
    }

    if total_steps == 0 {
        return Err(TrainingError::InsufficientData);
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

pub fn train_burn_model_wrapper(
    model: BurnRankMlp,
    groups: &[DecisionGroup],
    features_by_state: &HashMap<StateId, Vec<f32>>,
    config: &TrainingConfig,
) -> Result<(BurnRankMlp, TrainingMetrics), TrainingError> {
    validate_training_config(config)?;
    validate_supervision(groups)?;
    validate_feature_payloads(groups, features_by_state, model.input_dim())?;
    let burn_config = BurnTrainingConfig {
        epochs: config.epochs,
        learning_rate: config.learning_rate,
    };
    train_burn_model(model, groups, features_by_state, &burn_config)
        .map(|(m, metrics)| {
            (
                m,
                TrainingMetrics {
                    epoch_losses: metrics.epoch_losses,
                    final_loss: metrics.final_loss,
                    total_steps: metrics.total_steps,
                },
            )
        })
        .map_err(|e| TrainingError::Config(e.to_string()))
}

fn validate_training_config(config: &TrainingConfig) -> Result<(), TrainingError> {
    if config.epochs == 0
        || !config.learning_rate.is_finite()
        || config.learning_rate <= 0.0
        || !config.weight_decay.is_finite()
        || config.weight_decay < 0.0
        || !config.temperature.is_finite()
        || config.temperature <= 0.0
    {
        return Err(TrainingError::Config(
            "epochs, learning rate, weight decay, and temperature are invalid".into(),
        ));
    }
    Ok(())
}

fn validate_supervision(groups: &[DecisionGroup]) -> Result<(), TrainingError> {
    for (group_index, group) in groups.iter().enumerate() {
        if group.labels.len() != group.candidate_ids.len() {
            return Err(TrainingError::InvalidSupervision {
                group: group_index,
                candidate: group.labels.len().min(group.candidate_ids.len()),
                reason: "candidate and label counts differ".into(),
            });
        }
        for (candidate_index, label) in group.labels.iter().enumerate() {
            if let Err(error) = label.validate_evidence() {
                return Err(TrainingError::InvalidSupervision {
                    group: group_index,
                    candidate: candidate_index,
                    reason: error.to_string(),
                });
            }
        }
    }
    Ok(())
}

fn validate_group_supervision(
    group_index: usize,
    group: &DecisionGroup,
) -> Result<(), TrainingError> {
    if group.labels.len() != group.candidate_ids.len() || group.candidate_ids.is_empty() {
        return Err(TrainingError::InvalidSupervision {
            group: group_index,
            candidate: group.labels.len().min(group.candidate_ids.len()),
            reason: "candidate and label counts differ or group is empty".into(),
        });
    }
    for (candidate_index, label) in group.labels.iter().enumerate() {
        label
            .validate_evidence()
            .map_err(|error| TrainingError::InvalidSupervision {
                group: group_index,
                candidate: candidate_index,
                reason: error.to_string(),
            })?;
    }
    Ok(())
}

fn validate_feature_payloads(
    groups: &[DecisionGroup],
    features_by_state: &HashMap<StateId, Vec<f32>>,
    feature_dim: usize,
) -> Result<(), TrainingError> {
    for (group_index, group) in groups.iter().enumerate() {
        let expected = group
            .candidate_ids
            .len()
            .checked_mul(feature_dim)
            .ok_or_else(|| TrainingError::Config("feature dimension overflow".into()))?;
        let features =
            features_by_state
                .get(&group.state_id)
                .ok_or(TrainingError::MissingFeatures {
                    group: group_index,
                    state: group.state_id,
                })?;
        if features.len() != expected {
            return Err(TrainingError::FeatureDimension {
                group: group_index,
                expected,
                found: features.len(),
            });
        }
        if features.iter().any(|feature| !feature.is_finite()) {
            return Err(TrainingError::Config(format!(
                "non-finite feature in group {group_index}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_types::{CandidateId, Digest, StateId};
    use smallvec::smallvec;

    fn sampling_record(index: usize, source: Digest) -> SamplingRecord {
        SamplingRecord {
            group_index: index,
            group_identity: Digest::hash_blake3(format!("group-{index}").as_bytes()),
            source_identity: source,
            family: format!("family-{}", index % 2),
            stratum: format!("stratum-{}", index % 2),
            state_shape: format!("shape-{}", index % 3),
            source_policy: format!("policy-{}", index % 2),
            duplicate_identity: Digest::hash_blake3(format!("duplicate-{index}").as_bytes()),
            supervision_density: ((index + 1) as f32 / 10.0).min(1.0),
            policy_disagreement: (10_usize.saturating_sub(index)) as f32 / 10.0,
            censored_frontier: index.is_multiple_of(2),
            knn_success: index.is_multiple_of(3),
        }
    }

    #[test]
    fn test_training_session() {
        let mlp = MicroMlp::random(4, 8, 1);
        let trainer = MicroTrainer::new(mlp, 0.01, 0.0001);
        let plan = BatchPlan::new(42, 1, HashSet::new());
        let session = TrainingSession::new_micro(trainer, plan);
        assert!(session.micro_trainer.is_some());
    }

    #[test]
    fn test_viable_target_equal_cost() {
        let receipt = Digest::hash_blake3(b"verified-route");
        let labels = vec![
            CandidateKnowledge::Viable {
                best_actions_to_go: 3,
                receipts: smallvec![receipt],
            },
            CandidateKnowledge::Viable {
                best_actions_to_go: 3,
                receipts: smallvec![receipt],
            },
            CandidateKnowledge::Unknown,
        ];
        let mut targets = vec![0.0; 3];
        assert!(viable_target(&labels, 1.0, &mut targets).unwrap());
        assert!((targets[0] - 0.5).abs() < 1e-5);
        assert!((targets[1] - 0.5).abs() < 1e-5);
        assert_eq!(targets[2], 0.0);
    }

    #[test]
    fn test_unknown_has_zero_pairwise_supervision() {
        let group = DecisionGroup {
            state_id: StateId::from_digest(Digest::hash_blake3(b"s")),
            candidate_ids: vec![
                CandidateId::from_digest(Digest::hash_blake3(b"a")),
                CandidateId::from_digest(Digest::hash_blake3(b"b")),
            ],
            labels: vec![
                CandidateKnowledge::Viable {
                    best_actions_to_go: 1,
                    receipts: smallvec![Digest::hash_blake3(b"verified-route")],
                },
                CandidateKnowledge::Unknown,
            ],
            feature_ref: Digest::ZERO,
            source_episodes: Vec::new(),
            coverage: "test".to_string(),
        };
        let pairs = collect_supervised_pairs(&group);
        assert!(pairs.is_empty());
    }

    #[test]
    fn test_batch_plan_deterministic() {
        let a = BatchPlan::new(7, 10, HashSet::new());
        let b = BatchPlan::new(7, 10, HashSet::new());
        assert_eq!(a.group_indices, b.group_indices);
    }

    #[test]
    fn test_eval_exclusion_in_sampler() {
        let eval = Digest::hash_blake3(b"eval-shard");
        let registry = SamplerRegistry::new(HashSet::from([eval]));
        assert!(registry.eval_ids.contains(&eval));
    }

    #[test]
    fn sampler_registry_is_reproducible_and_excludes_evaluation() {
        let train = Digest::hash_blake3(b"train-corpus");
        let eval = Digest::hash_blake3(b"evaluation-corpus");
        let mut records: Vec<_> = (0..10).map(|index| sampling_record(index, train)).collect();
        records.push(sampling_record(10, eval));
        let registry = SamplerRegistry::new(HashSet::from([eval]));
        let spec = SamplingSpec {
            name: "uniform-reference".into(),
            kind: SamplerKind::Uniform,
            seed: 91,
            sample_count: 5,
            source_policy_cap: 5,
            duplicate_cap: 1,
        };
        let first = registry.sample(&records, &spec).unwrap();
        let second = registry.sample(&records, &spec).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.identity().unwrap(), second.identity().unwrap());
        assert_eq!(first.selected.len(), 5);
        assert!(
            first
                .selected
                .iter()
                .all(|entry| entry.source_identity != eval)
        );
        assert!(
            first
                .selected
                .iter()
                .all(|entry| entry.inclusion_probability == Some(0.5))
        );
    }

    #[test]
    fn narrow_and_balanced_samplers_enforce_declared_semantics() {
        let train = Digest::hash_blake3(b"train");
        let records: Vec<_> = (0..10).map(|index| sampling_record(index, train)).collect();
        let mut registry = SamplerRegistry::new(HashSet::new());
        registry.min_family_coverage = 1;
        let narrow = registry
            .sample(
                &records,
                &SamplingSpec {
                    name: "knn-success-ablation".into(),
                    kind: SamplerKind::KnnSuccessOnly,
                    seed: 3,
                    sample_count: 4,
                    source_policy_cap: 4,
                    duplicate_cap: 1,
                },
            )
            .unwrap();
        assert!(
            narrow
                .selected
                .iter()
                .all(|selected| records[selected.group_index].knn_success)
        );

        let balanced = registry
            .sample(
                &records,
                &SamplingSpec {
                    name: "balanced".into(),
                    kind: SamplerKind::BalancedFamilyStratum,
                    seed: 3,
                    sample_count: 4,
                    source_policy_cap: 4,
                    duplicate_cap: 1,
                },
            )
            .unwrap();
        let covered: BTreeSet<_> = balanced
            .selected
            .iter()
            .map(|selected| {
                let record = &records[selected.group_index];
                (record.family.as_str(), record.stratum.as_str())
            })
            .collect();
        assert_eq!(covered.len(), 2);
    }

    #[test]
    fn group_batch_packer_preserves_boundaries_masks_and_restart_position() {
        let source = Digest::hash_blake3(b"train-source");
        let receipt = Digest::hash_blake3(b"receipt");
        let make_group = |name: &'static [u8], candidates: usize| {
            let state = StateId::from_digest(Digest::hash_blake3(name));
            let mut labels = vec![CandidateKnowledge::Unknown; candidates];
            labels[0] = CandidateKnowledge::Viable {
                best_actions_to_go: 1,
                receipts: smallvec![receipt],
            };
            DecisionGroup {
                state_id: state,
                candidate_ids: (0..candidates)
                    .map(|index| {
                        CandidateId::from_digest(Digest::hash_blake3(
                            format!("{}-{index}", String::from_utf8_lossy(name)).as_bytes(),
                        ))
                    })
                    .collect(),
                labels,
                feature_ref: Digest::hash_blake3(b"features"),
                source_episodes: Vec::new(),
                coverage: "train".into(),
            }
        };
        let groups = vec![make_group(b"a", 2), make_group(b"b", 3)];
        let features = HashMap::from([
            (groups[0].state_id, vec![1.0; 4]),
            (groups[1].state_id, vec![2.0; 6]),
        ]);
        let manifest = SamplingManifest {
            format_version: 1,
            sampler_name: "fixed".into(),
            kind: SamplerKind::Uniform,
            seed: 8,
            eligible_count: 2,
            selected: vec![
                SampledGroup {
                    group_index: 0,
                    group_identity: Digest::hash_blake3(b"g0"),
                    source_identity: source,
                    inclusion_probability: Some(1.0),
                },
                SampledGroup {
                    group_index: 1,
                    group_identity: Digest::hash_blake3(b"g1"),
                    source_identity: source,
                    inclusion_probability: Some(1.0),
                },
            ],
            excluded_evaluation_corpora: Vec::new(),
        };
        let mut plan = BatchPlan::from_sampling_manifest(&manifest, BTreeSet::new()).unwrap();
        let mut packer = GroupBatchPacker::with_capacity(2, 5, 2);
        let batch = packer
            .pack(&mut plan, &groups, &features, 2, 2, 1.0)
            .unwrap()
            .unwrap();
        assert_eq!(batch.group_offsets, vec![0, 2, 5]);
        assert_eq!(batch.masks, vec![1.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(batch.weights, vec![1.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(plan.next_batch, 2);

        plan.next_batch = 0;
        let mut incomplete = features;
        incomplete.remove(&groups[1].state_id);
        assert!(
            packer
                .pack(&mut plan, &groups, &incomplete, 2, 2, 1.0)
                .is_err()
        );
        assert_eq!(plan.next_batch, 0);
    }

    #[test]
    fn burn_loop_cancels_checkpoints_and_resumes_exact_position() {
        let state_id = StateId::from_digest(Digest::hash_blake3(b"burn-state"));
        let group = DecisionGroup {
            state_id,
            candidate_ids: vec![
                CandidateId::from_digest(Digest::hash_blake3(b"viable")),
                CandidateId::from_digest(Digest::hash_blake3(b"dead")),
                CandidateId::from_digest(Digest::hash_blake3(b"unknown")),
            ],
            labels: vec![
                CandidateKnowledge::Viable {
                    best_actions_to_go: 1,
                    receipts: smallvec![Digest::hash_blake3(b"route")],
                },
                CandidateKnowledge::KnownDead {
                    certificate: Digest::hash_blake3(b"dead-certificate"),
                },
                CandidateKnowledge::Unknown,
            ],
            feature_ref: Digest::hash_blake3(b"burn-features"),
            source_episodes: Vec::new(),
            coverage: "train".into(),
        };
        let features = HashMap::from([(state_id, vec![1.0, 0.0, -1.0, 0.0, 9.0, 9.0])]);
        let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(b"schema"));
        let provenance = TrainingCheckpointProvenance {
            dataset: Digest::hash_blake3(b"burn-dataset"),
            sampler: Digest::hash_blake3(b"burn-sampler"),
            code: Digest::hash_blake3(b"burn-code"),
            provenance: Digest::hash_blake3(b"burn-provenance"),
            metrics: Digest::hash_blake3(b"burn-metrics"),
        };
        let config = BurnLoopConfig {
            epochs: 2,
            groups_per_batch: 1,
            checkpoint_every_steps: 1,
            temperature: 1.0,
        };
        let mut session = BurnTrainingSession::new_seeded(2, 3, 0.01, 44);
        let mut state = BurnLoopState::new(BatchPlan::new(4, 1, HashSet::new()));
        let cancel = AtomicBool::new(true);
        let mut latest = None;
        run_burn_loop(
            &mut session,
            &mut state,
            std::slice::from_ref(&group),
            &features,
            2,
            schema,
            config,
            provenance,
            &cancel,
            &mut latest,
        )
        .unwrap();
        assert_eq!(state.status, BurnLoopStatus::Cancelled);
        assert_eq!(state.plan.next_batch, 0);
        let (mut resumed_session, mut resumed_state) = latest
            .as_ref()
            .unwrap()
            .restore_session_and_state()
            .unwrap();
        resumed_state.resume_cancelled().unwrap();
        cancel.store(false, Ordering::Release);
        run_burn_loop(
            &mut resumed_session,
            &mut resumed_state,
            &[group],
            &features,
            2,
            schema,
            config,
            provenance,
            &cancel,
            &mut latest,
        )
        .unwrap();
        assert_eq!(resumed_state.status, BurnLoopStatus::Completed);
        assert_eq!(resumed_state.metrics.total_steps, 2);
        assert_eq!(resumed_state.metrics.supervised_candidates, 4);
        assert_eq!(latest.as_ref().unwrap().training.step, 2);
        assert_eq!(latest.as_ref().unwrap().state.plan.next_batch, 1);
    }

    #[test]
    fn sweep_cells_are_exact_deduplicated_and_dev_selected() {
        let architectures: Vec<_> = M1_5_PARAMETER_POINTS
            .into_iter()
            .map(|count| ArchitectureSpec::exact_linear(count).unwrap())
            .collect();
        assert_eq!(
            architectures
                .iter()
                .map(ArchitectureSpec::parameter_count)
                .collect::<Result<Vec<_>, _>>()
                .unwrap(),
            M1_5_PARAMETER_POINTS
        );
        let dataset = Digest::hash_blake3(b"dataset");
        let lineage = Digest::hash_blake3(b"lineage");
        let spec = SweepSpec::new(
            "m1.5".into(),
            vec![architectures[0].clone(), architectures[0].clone()],
            vec![LossSpec::SingleRoute, LossSpec::ProofDag],
            vec![dataset],
            vec![7, 7],
            vec![lineage],
            vec![TrainingBackend::Micro],
            DevelopmentSelectionRule {
                metric: "dev_loss".into(),
                direction: SelectionDirection::Minimize,
            },
            BTreeMap::new(),
        )
        .unwrap();
        let plan = spec.cells().unwrap();
        assert_eq!(plan.generated_cells, 8);
        assert_eq!(plan.deduplicated_cells, 2);
        assert_eq!(plan.runnable.len(), 2);
        let results = vec![
            SweepMetricResult {
                cell_identity: plan.runnable[0].identity,
                corpus: ResultCorpus::Development,
                metric: "dev_loss".into(),
                value: 0.4,
            },
            SweepMetricResult {
                cell_identity: plan.runnable[1].identity,
                corpus: ResultCorpus::Development,
                metric: "dev_loss".into(),
                value: 0.2,
            },
        ];
        assert_eq!(
            plan.select_development(&results).unwrap(),
            plan.runnable[1].identity
        );
        let mut leaked = results;
        leaked[0].corpus = ResultCorpus::Evaluation;
        assert!(plan.select_development(&leaked).is_err());
        let reconciliation = plan.reconcile(&[plan.runnable[0].identity]).unwrap();
        assert_eq!(reconciliation.completed.len(), 1);
        assert_eq!(reconciliation.missing.len(), 1);
    }

    #[test]
    fn training_rejects_zero_receipt_supervision() {
        let state = StateId::from_digest(Digest::hash_blake3(b"state"));
        let group = DecisionGroup {
            state_id: state,
            candidate_ids: vec![
                CandidateId::from_digest(Digest::hash_blake3(b"better")),
                CandidateId::from_digest(Digest::hash_blake3(b"worse")),
            ],
            labels: vec![
                CandidateKnowledge::Viable {
                    best_actions_to_go: 1,
                    receipts: smallvec![Digest::ZERO],
                },
                CandidateKnowledge::KnownDead {
                    certificate: Digest::hash_blake3(b"certificate"),
                },
            ],
            feature_ref: Digest::hash_blake3(b"features"),
            source_episodes: Vec::new(),
            coverage: "test".into(),
        };
        let trainer = MicroTrainer::new(MicroMlp::random(2, 3, 9), 0.01, 0.0);
        let features = HashMap::from([(state, vec![1.0, 1.0, -1.0, -1.0])]);
        assert!(matches!(
            train_micro_model(trainer, &[group], &features, &TrainingConfig::default()),
            Err(TrainingError::InvalidSupervision { .. })
        ));
    }

    #[test]
    fn training_rejects_dataset_without_pairs() {
        let state = StateId::from_digest(Digest::hash_blake3(b"unlabelled"));
        let group = DecisionGroup {
            state_id: state,
            candidate_ids: vec![CandidateId::from_digest(Digest::hash_blake3(b"candidate"))],
            labels: vec![CandidateKnowledge::Unknown],
            feature_ref: Digest::hash_blake3(b"features"),
            source_episodes: Vec::new(),
            coverage: "test".into(),
        };
        let trainer = MicroTrainer::new(MicroMlp::random(2, 3, 10), 0.01, 0.0);
        let features = HashMap::from([(state, vec![0.0, 0.0])]);
        assert!(matches!(
            train_micro_model(trainer, &[group], &features, &TrainingConfig::default()),
            Err(TrainingError::InsufficientData)
        ));
    }

    #[test]
    fn batch_assembly_failure_does_not_advance_resume_position() {
        let state = StateId::from_digest(Digest::hash_blake3(b"missing-batch-features"));
        let group = DecisionGroup {
            state_id: state,
            candidate_ids: vec![CandidateId::from_digest(Digest::hash_blake3(b"candidate"))],
            labels: vec![CandidateKnowledge::Viable {
                best_actions_to_go: 1,
                receipts: smallvec![Digest::hash_blake3(b"receipt")],
            }],
            feature_ref: Digest::hash_blake3(b"features"),
            source_episodes: vec![],
            coverage: "train".into(),
        };
        let mut trainer = Trainer::new(
            TrainingConfig::default(),
            BatchPlan::new(1, 1, HashSet::new()),
        );
        assert!(matches!(
            trainer.assemble_batch(&[group], &HashMap::new(), 2),
            Err(TrainingError::MissingFeatures { .. })
        ));
        assert_eq!(trainer.plan.next_batch, 0);
    }

    #[test]
    fn micro_checkpoint_roundtrip_preserves_resume_and_cancel_state() {
        let mut trainer = MicroTrainer::new(MicroMlp::random(2, 3, 77), 0.01, 0.001);
        trainer
            .try_train_step_pairwise(&[1.0, 0.0, 0.0, 1.0], 0, 1)
            .unwrap();
        let mut plan = BatchPlan::new(9, 4, HashSet::new());
        let _ = plan.next_group();
        let mut session = TrainingSession::new_micro(trainer, plan);
        session.epoch = 2;
        session.step = 1;
        let dataset = Digest::hash_blake3(b"dataset");
        let checkpoint_provenance = TrainingCheckpointProvenance {
            dataset,
            sampler: Digest::hash_blake3(b"sampler"),
            code: Digest::hash_blake3(b"code"),
            provenance: Digest::hash_blake3(b"training provenance"),
            metrics: Digest::hash_blake3(b"metrics"),
        };
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("trainer.json");
        let identity = session
            .cancel_and_checkpoint(checkpoint_provenance, &path)
            .unwrap();
        let stored = MicroTrainingCheckpoint::load(&path, identity).unwrap();
        let resumed = TrainingSession::resume(stored).unwrap();
        assert_eq!(resumed.status, TrainingRunStatus::Cancelled);
        assert_eq!(resumed.epoch, 2);
        assert_eq!(resumed.plan.next_batch, 1);
        let resumed_trainer = resumed.micro_trainer.unwrap();
        assert_eq!(resumed_trainer.step, 1);
        assert_eq!(
            resumed_trainer.optimizer_state_digest(),
            session
                .micro_trainer
                .as_ref()
                .unwrap()
                .optimizer_state_digest()
        );
    }
}
