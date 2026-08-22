use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use sha2::{Digest, Sha256};

pub(crate) const FEATURE_COUNT: usize = 16;
pub(crate) const HEAD_COUNT: usize = 7;
const FEATURE_REVISION: u32 = 1;
const TARGET_REVISION: u32 = 1;
const CALIBRATION_REVISION: u32 = 1;
const MAX_REPLAY_BATCH: usize = 16_384;
const TRAINING_EPOCHS: usize = 8;
const MAX_SELECTION_USES: u8 = 3;
const MIN_SELECTION_CASES: usize = 8;
const MIN_REPLAY_CASES: usize = 8;
const MAX_SPECIALISTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Features(pub(crate) [f32; FEATURE_COUNT]);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Targets(pub(crate) [f32; HEAD_COUNT]);

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Forecast {
    pub(crate) estimate: f32,
    pub(crate) calibration_error: f32,
    pub(crate) uncertainty: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PotentialForecast(pub(crate) [Forecast; HEAD_COUNT]);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VerdictTarget {
    Accepted,
    Refuted,
    Unknown,
}

#[derive(Clone, Debug)]
pub(crate) struct AttemptObservation {
    pub(crate) id: [u8; 32],
    pub(crate) artifact: [u8; 32],
    pub(crate) claim: [u8; 32],
    pub(crate) parent: [u8; 32],
    pub(crate) features: Features,
    pub(crate) verdict: VerdictTarget,
    pub(crate) verification_cost: f32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsequenceKind {
    Admitted,
    ParetoImprovement,
    CrossGoalUse,
    Compression,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConsequenceObservation {
    pub(crate) subject: [u8; 32],
    pub(crate) kind: ConsequenceKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CorpusRole {
    Replay,
    Selection { uses: u8 },
}

#[derive(Clone, Debug)]
pub(crate) struct TrainingExample {
    pub(crate) key: [u8; 32],
    pub(crate) corpus_key: [u8; 32],
    pub(crate) features: Features,
    active_features: u16,
    pub(crate) targets: Targets,
    pub(crate) role: CorpusRole,
}

#[derive(Clone, Debug)]
pub(crate) struct FtrlModel {
    z: [[f32; HEAD_COUNT]; FEATURE_COUNT],
    n: [[f32; HEAD_COUNT]; FEATURE_COUNT],
    sqrt_n: [[f32; HEAD_COUNT]; FEATURE_COUNT],
    weights: [[f32; HEAD_COUNT]; FEATURE_COUNT],
    calibration_count: [u64; HEAD_COUNT],
    calibration_error: [f32; HEAD_COUNT],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromotionDecision {
    Promote,
    Specialist,
    Reject,
    Rollback,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LearningState {
    generation: u64,
    champion: Option<FtrlModel>,
    predecessor: Option<FtrlModel>,
    predecessor_is_bootstrap: bool,
    specialists: Vec<FtrlModel>,
    roles: BTreeMap<[u8; 32], CorpusRole>,
}

#[cfg(feature = "internal-experiments")]
pub(crate) struct PairedModelComparison {
    pub(crate) replay_claims: usize,
    pub(crate) selection_claims: usize,
    pub(crate) baseline_loss: [f32; HEAD_COUNT],
    pub(crate) structural_loss: [f32; HEAD_COUNT],
    pub(crate) baseline_training_cpu: std::time::Duration,
    pub(crate) structural_training_cpu: std::time::Duration,
    pub(crate) baseline_revision: [u8; 32],
    pub(crate) structural_revision: [u8; 32],
    pub(crate) model_bytes: usize,
    pub(crate) baseline_reproduces_champion: bool,
    pub(crate) structural_promotes: bool,
    pub(crate) ranking_budgets: [usize; 7],
    pub(crate) baseline_accepted_at_k: [usize; 7],
    pub(crate) structural_accepted_at_k: [usize; 7],
    pub(crate) evaluated_at_k: [usize; 7],
    pub(crate) selection_accepted: usize,
}

impl LearningState {
    pub(crate) fn pinned_model(&self) -> Option<&FtrlModel> {
        self.champion.as_ref()
    }

    pub(crate) fn resident_bytes(&self) -> u64 {
        let inline = std::mem::size_of_val(self) as u64;
        let specialists = (self.specialists.capacity() as u64)
            .saturating_mul(std::mem::size_of::<FtrlModel>() as u64);
        let corpus_index = (self.roles.len() as u64).saturating_mul(128);
        inline
            .saturating_add(specialists)
            .saturating_add(corpus_index)
    }

    pub(crate) fn corpus_is_valid(&self, attempts: &[([u8; 32], [u8; 32])]) -> bool {
        self.roles
            .keys()
            .all(|key| attempts.iter().any(|(_, corpus)| corpus == key))
    }

    pub(crate) fn training_scratch_bytes(example_count: usize) -> u64 {
        let total = example_count as u64;
        let retained = example_count.min(MAX_REPLAY_BATCH) as u64;
        let example = std::mem::size_of::<TrainingExample>() as u64;
        total
            .saturating_mul(example)
            .saturating_add(retained.saturating_mul(example).saturating_mul(2))
            .saturating_add(retained.saturating_mul(192))
    }

    #[cfg(test)]
    fn generation(&self) -> u64 {
        self.generation
    }

    #[cfg(test)]
    fn specialist_count(&self) -> usize {
        self.specialists.len()
    }

    pub(crate) fn learn(&mut self, examples: &mut [TrainingExample]) -> PromotionDecision {
        self.assign_new_corpus_roles(examples);
        let selection = bounded_corpus(examples, true);
        if self.champion.is_some() {
            let predecessor = if self.predecessor_is_bootstrap {
                Some(FtrlModel::zero())
            } else {
                self.predecessor.clone()
            };
            if predecessor.as_ref().is_some_and(|predecessor| {
                compare_models(
                    self.champion.as_ref().expect("the champion exists"),
                    predecessor,
                    &selection,
                ) == PromotionDecision::Promote
            }) {
                let decision = self.rollback();
                self.finish_selection(examples);
                return decision;
            }
        }
        let replay = bounded_corpus(examples, false);
        let mut challenger = FtrlModel::zero();
        for _ in 0..TRAINING_EPOCHS {
            for example in &replay {
                challenger.update(example);
            }
        }
        let champion = self.champion.clone().unwrap_or_else(FtrlModel::zero);
        let decision = compare_models(&champion, &challenger, &selection);
        match decision {
            PromotionDecision::Promote => {
                self.predecessor = self.champion.replace(challenger);
                self.predecessor_is_bootstrap = self.predecessor.is_none();
                self.generation = self.generation.saturating_add(1);
            }
            PromotionDecision::Specialist => {
                self.retain_specialist(challenger);
            }
            PromotionDecision::Reject | PromotionDecision::Rollback => {}
        }
        self.finish_selection(examples);
        decision
    }

    #[cfg(feature = "internal-experiments")]
    pub(crate) fn compare_feature_sets(
        &self,
        baseline_attempts: &[AttemptObservation],
        structural_attempts: &[AttemptObservation],
        consequences: &[ConsequenceObservation],
    ) -> Result<PairedModelComparison, ()> {
        if baseline_attempts.len() != structural_attempts.len() || self.champion.is_none() {
            return Err(());
        }
        let mut baseline = derive_targets(baseline_attempts, consequences);
        let mut structural = derive_targets(structural_attempts, consequences);
        for (baseline, structural) in baseline.iter_mut().zip(&mut structural) {
            if baseline.key != structural.key
                || baseline.corpus_key != structural.corpus_key
                || baseline.targets != structural.targets
            {
                return Err(());
            }
            let role = self.roles.get(&baseline.corpus_key).copied().ok_or(())?;
            baseline.role = role;
            structural.role = role;
        }
        let baseline_replay = bounded_corpus(&baseline, false);
        let structural_replay = bounded_corpus(&structural, false);
        let baseline_selection = bounded_corpus(&baseline, true);
        let structural_selection = bounded_corpus(&structural, true);
        let train = |replay: &[&TrainingExample]| {
            let started = cpu_time::ProcessTime::now();
            let mut model = FtrlModel::zero();
            for _ in 0..TRAINING_EPOCHS {
                for example in replay {
                    model.update(example);
                }
            }
            (model, started.elapsed())
        };
        let (baseline_model, baseline_training_cpu) = train(&baseline_replay);
        let (structural_model, structural_training_cpu) = train(&structural_replay);
        let baseline_loss = losses(&baseline_model, &baseline_selection);
        let structural_loss = losses(&structural_model, &structural_selection);
        let baseline_ranking = accepted_ranking(&baseline_model, baseline_attempts, &self.roles);
        let structural_ranking =
            accepted_ranking(&structural_model, structural_attempts, &self.roles);
        if baseline_ranking.evaluated_at_k != structural_ranking.evaluated_at_k
            || baseline_ranking.total_accepted != structural_ranking.total_accepted
        {
            return Err(());
        }
        let replay_claims = baseline_replay
            .iter()
            .map(|example| example.corpus_key)
            .collect::<BTreeSet<_>>()
            .len();
        let selection_claims = baseline_selection
            .iter()
            .map(|example| example.corpus_key)
            .collect::<BTreeSet<_>>()
            .len();
        Ok(PairedModelComparison {
            replay_claims,
            selection_claims,
            baseline_loss,
            structural_loss,
            baseline_training_cpu,
            structural_training_cpu,
            baseline_revision: revision_digest(&baseline_model),
            structural_revision: revision_digest(&structural_model),
            model_bytes: baseline_model.encode().len(),
            baseline_reproduces_champion: self
                .champion
                .as_ref()
                .is_some_and(|champion| champion.encode() == baseline_model.encode()),
            structural_promotes: promotion_from_losses(baseline_loss, structural_loss)
                == PromotionDecision::Promote,
            ranking_budgets: RANKING_BUDGETS,
            baseline_accepted_at_k: baseline_ranking.accepted_at_k,
            structural_accepted_at_k: structural_ranking.accepted_at_k,
            evaluated_at_k: baseline_ranking.evaluated_at_k,
            selection_accepted: baseline_ranking.total_accepted,
        })
    }

    fn assign_new_corpus_roles(&mut self, examples: &mut [TrainingExample]) {
        let mut unseen = examples
            .iter()
            .map(|example| example.corpus_key)
            .filter(|key| !self.roles.contains_key(key))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        unseen.sort_unstable_by_key(|key| {
            (
                !matches!(assign_role(*key), CorpusRole::Selection { .. }),
                *key,
            )
        });
        let current_selection = self
            .roles
            .values()
            .filter(|role| matches!(role, CorpusRole::Selection { .. }))
            .count();
        let total_cases = self.roles.len().saturating_add(unseen.len());
        let minimum_replay = if self.champion.is_some() {
            0
        } else {
            MIN_REPLAY_CASES
        };
        let maximum_selection = total_cases.saturating_sub(minimum_replay);
        let target_selection = total_cases
            .div_ceil(5)
            .max(MIN_SELECTION_CASES)
            .min(maximum_selection);
        let new_selection = target_selection
            .saturating_sub(current_selection)
            .min(unseen.len());
        for (index, key) in unseen.into_iter().enumerate() {
            self.roles.insert(
                key,
                if index < new_selection {
                    CorpusRole::Selection { uses: 0 }
                } else {
                    CorpusRole::Replay
                },
            );
        }
        for example in examples {
            example.role = self.roles[&example.corpus_key];
        }
    }

    fn finish_selection(&mut self, examples: &mut [TrainingExample]) {
        rotate_selection(examples, MAX_SELECTION_USES);
        for example in examples {
            self.roles.insert(example.corpus_key, example.role);
        }
    }

    pub(crate) fn rollback(&mut self) -> PromotionDecision {
        if self.predecessor_is_bootstrap {
            if let Some(regressed) = self.champion.take() {
                self.retain_specialist(regressed);
            }
            self.predecessor_is_bootstrap = false;
            self.generation = self.generation.saturating_add(1);
            return PromotionDecision::Rollback;
        }
        let Some(predecessor) = self.predecessor.take() else {
            return PromotionDecision::Reject;
        };
        if let Some(regressed) = self.champion.replace(predecessor) {
            self.retain_specialist(regressed);
        }
        self.generation = self.generation.saturating_add(1);
        PromotionDecision::Rollback
    }

    fn retain_specialist(&mut self, specialist: FtrlModel) {
        if !self
            .specialists
            .iter()
            .any(|model| revision_digest(model) == revision_digest(&specialist))
        {
            self.specialists.push(specialist);
            if self.specialists.len() > MAX_SPECIALISTS {
                self.specialists.remove(0);
            }
        }
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"RFLS\x02");
        output.extend_from_slice(&self.generation.to_le_bytes());
        push_model(&mut output, self.champion.as_ref());
        match (&self.predecessor, self.predecessor_is_bootstrap) {
            (None, false) => output.push(0),
            (None, true) => output.push(1),
            (Some(model), false) => {
                output.push(2);
                push_bytes(&mut output, &model.encode());
            }
            (Some(_), true) => unreachable!("a predecessor cannot be Bootstrap and learned"),
        }
        output.extend_from_slice(&(self.specialists.len() as u64).to_le_bytes());
        for specialist in &self.specialists {
            push_bytes(&mut output, &specialist.encode());
        }
        output.extend_from_slice(&(self.roles.len() as u64).to_le_bytes());
        for (key, role) in &self.roles {
            output.extend_from_slice(key);
            match role {
                CorpusRole::Replay => output.push(0),
                CorpusRole::Selection { uses } => {
                    output.push(1);
                    output.push(*uses);
                }
            }
        }
        output
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, ()> {
        let mut input = bytes;
        if take(&mut input, 5)? != b"RFLS\x02" {
            return Err(());
        }
        let generation = read_u64(&mut input)?;
        let champion = read_model(&mut input)?;
        let (predecessor, predecessor_is_bootstrap) = match take(&mut input, 1)?[0] {
            0 => (None, false),
            1 => (None, true),
            2 => (Some(FtrlModel::decode(take_sized(&mut input)?)?), false),
            _ => return Err(()),
        };
        let specialist_count = usize::try_from(read_u64(&mut input)?).map_err(|_| ())?;
        if specialist_count > MAX_SPECIALISTS
            || specialist_count
                > input
                    .len()
                    .saturating_div(5 + HEAD_COUNT * FEATURE_COUNT * 8 + 8)
        {
            return Err(());
        }
        let mut specialists = Vec::with_capacity(specialist_count);
        for _ in 0..specialist_count {
            specialists.push(FtrlModel::decode(take_sized(&mut input)?)?);
        }
        let role_count = usize::try_from(read_u64(&mut input)?).map_err(|_| ())?;
        if role_count > input.len().saturating_div(33) {
            return Err(());
        }
        let mut roles = BTreeMap::new();
        for _ in 0..role_count {
            let key: [u8; 32] = take(&mut input, 32)?.try_into().unwrap();
            let role = match take(&mut input, 1)?[0] {
                0 => CorpusRole::Replay,
                1 => {
                    let uses = take(&mut input, 1)?[0];
                    if uses >= 3 {
                        return Err(());
                    }
                    CorpusRole::Selection { uses }
                }
                _ => return Err(()),
            };
            if roles.insert(key, role).is_some() {
                return Err(());
            }
        }
        if !input.is_empty()
            || generation == 0
                && (champion.is_some()
                    || predecessor.is_some()
                    || predecessor_is_bootstrap
                    || !specialists.is_empty())
            || generation > 0 && champion.is_none() && specialists.is_empty()
            || champion.is_none() && (predecessor.is_some() || predecessor_is_bootstrap)
            || champion.as_ref().is_some_and(|champion| {
                predecessor.as_ref().is_some_and(|predecessor| {
                    revision_digest(champion) == revision_digest(predecessor)
                })
            })
            || specialists.iter().any(|specialist| {
                champion.as_ref().is_some_and(|champion| {
                    revision_digest(champion) == revision_digest(specialist)
                }) || predecessor.as_ref().is_some_and(|predecessor| {
                    revision_digest(predecessor) == revision_digest(specialist)
                })
            })
            || specialists.iter().enumerate().any(|(index, specialist)| {
                specialists[index + 1..]
                    .iter()
                    .any(|other| revision_digest(specialist) == revision_digest(other))
            })
        {
            return Err(());
        }
        Ok(Self {
            generation,
            champion,
            predecessor,
            predecessor_is_bootstrap,
            specialists,
            roles,
        })
    }

    pub(crate) fn revision_digest(&self, semantic_identity: &str) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"reflex-model-revision-v2\0");
        digest.update((semantic_identity.len() as u64).to_le_bytes());
        digest.update(semantic_identity.as_bytes());
        match &self.champion {
            Some(champion) => {
                digest.update([1]);
                digest.update(champion.encode());
            }
            None => digest.update([0]),
        }
        digest.finalize().into()
    }
}

#[cfg(feature = "internal-experiments")]
const RANKING_BUDGETS: [usize; 7] = [1, 2, 4, 8, 16, 32, 64];

#[cfg(feature = "internal-experiments")]
struct AcceptedRanking {
    accepted_at_k: [usize; 7],
    evaluated_at_k: [usize; 7],
    total_accepted: usize,
}

#[cfg(feature = "internal-experiments")]
fn accepted_ranking(
    model: &FtrlModel,
    attempts: &[AttemptObservation],
    roles: &BTreeMap<[u8; 32], CorpusRole>,
) -> AcceptedRanking {
    let mut groups = BTreeMap::<[u8; 32], Vec<(&AttemptObservation, PotentialForecast)>>::new();
    for attempt in attempts.iter().filter(|attempt| {
        matches!(
            roles.get(&attempt.claim),
            Some(CorpusRole::Selection { .. })
        )
    }) {
        groups
            .entry(attempt.claim)
            .or_default()
            .push((attempt, model.forecast(attempt.features)));
    }
    let mut accepted_at_k = [0_usize; 7];
    let mut evaluated_at_k = [0_usize; 7];
    let mut total_accepted = 0_usize;
    for attempts in groups.values_mut() {
        attempts.sort_unstable_by(|(left, left_forecast), (right, right_forecast)| {
            compare_forecasts(*left_forecast, *right_forecast).then_with(|| left.id.cmp(&right.id))
        });
        total_accepted = total_accepted.saturating_add(
            attempts
                .iter()
                .filter(|(attempt, _)| attempt.verdict == VerdictTarget::Accepted)
                .count(),
        );
        for (index, budget) in RANKING_BUDGETS.into_iter().enumerate() {
            let count = attempts.len().min(budget);
            evaluated_at_k[index] = evaluated_at_k[index].saturating_add(count);
            accepted_at_k[index] = accepted_at_k[index].saturating_add(
                attempts[..count]
                    .iter()
                    .filter(|(attempt, _)| attempt.verdict == VerdictTarget::Accepted)
                    .count(),
            );
        }
    }
    AcceptedRanking {
        accepted_at_k,
        evaluated_at_k,
        total_accepted,
    }
}

pub(crate) fn compare_forecasts(left: PotentialForecast, right: PotentialForecast) -> Ordering {
    for head in [0, 1, 2, 3, 4] {
        let left_value = left.0[head].estimate
            - left.0[head].uncertainty
            - left.0[head].calibration_error * 0.25;
        let right_value = right.0[head].estimate
            - right.0[head].uncertainty
            - right.0[head].calibration_error * 0.25;
        let ordering = right_value.total_cmp(&left_value);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    for head in [6, 5] {
        let left_value = left.0[head].estimate
            + left.0[head].uncertainty
            + left.0[head].calibration_error * 0.25;
        let right_value = right.0[head].estimate
            + right.0[head].uncertainty
            + right.0[head].calibration_error * 0.25;
        let ordering = left_value.total_cmp(&right_value);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

impl FtrlModel {
    pub(crate) fn zero() -> Self {
        Self {
            z: [[0.0; HEAD_COUNT]; FEATURE_COUNT],
            n: [[0.0; HEAD_COUNT]; FEATURE_COUNT],
            sqrt_n: [[0.0; HEAD_COUNT]; FEATURE_COUNT],
            weights: [[0.0; HEAD_COUNT]; FEATURE_COUNT],
            calibration_count: [0; HEAD_COUNT],
            calibration_error: [0.0; HEAD_COUNT],
        }
    }

    fn predict(&self, features: Features) -> Targets {
        let mut linear = [0.0_f32; HEAD_COUNT];
        for (feature_index, feature) in features.0.iter().copied().enumerate() {
            for (head, value) in linear.iter_mut().enumerate() {
                *value += self.weights[feature_index][head] * feature;
            }
        }
        sigmoid_heads(linear)
    }

    fn predict_active(&self, features: Features, mut active_features: u16) -> Targets {
        let mut linear = [0.0_f32; HEAD_COUNT];
        while active_features != 0 {
            let feature_index = active_features.trailing_zeros() as usize;
            active_features &= active_features - 1;
            let feature = features.0[feature_index];
            for (head, value) in linear.iter_mut().enumerate() {
                *value += self.weights[feature_index][head] * feature;
            }
        }
        sigmoid_heads(linear)
    }

    pub(crate) fn forecast(&self, features: Features) -> PotentialForecast {
        let mut linear = [0.0_f32; HEAD_COUNT];
        let mut support = [0.0_f32; HEAD_COUNT];
        for (feature_index, feature) in features.0.iter().copied().enumerate() {
            for head in 0..HEAD_COUNT {
                linear[head] += self.weights[feature_index][head] * feature;
                support[head] += self.n[feature_index][head] * feature * feature;
            }
        }
        let estimates = sigmoid_heads(linear);
        PotentialForecast(std::array::from_fn(|head| {
            let epistemic = (1.0 / (1.0 + support[head].max(0.0))).sqrt();
            let calibration_error = if self.calibration_count[head] == 0 {
                1.0
            } else {
                self.calibration_error[head]
            };
            Forecast {
                estimate: estimates.0[head],
                calibration_error,
                uncertainty: epistemic.max(calibration_error).clamp(0.0, 1.0),
            }
        }))
    }

    pub(crate) fn forecast_batch(
        &self,
        features: &[Features],
        output: &mut Vec<PotentialForecast>,
    ) {
        output.clear();
        output.reserve(features.len());
        let mut unique = HashMap::<[u32; FEATURE_COUNT], PotentialForecast>::with_capacity(
            features.len().min(1_024),
        );
        for features in features {
            let key = features.0.map(f32::to_bits);
            let forecast = *unique
                .entry(key)
                .or_insert_with(|| self.forecast(*features));
            output.push(forecast);
        }
    }

    pub(crate) fn update(&mut self, example: &TrainingExample) {
        const ALPHA: f32 = 0.1;
        let predictions = self.predict_active(example.features, example.active_features);
        let errors: [f32; HEAD_COUNT] =
            std::array::from_fn(|head| predictions.0[head] - example.targets.0[head]);
        let mut active_features = example.active_features;
        while active_features != 0 {
            let index = active_features.trailing_zeros() as usize;
            active_features &= active_features - 1;
            let feature = example.features.0[index];
            let previous_n = self.n[index];
            let previous_sqrt_n = self.sqrt_n[index];
            let previous_z = self.z[index];
            let previous_weights = self.weights[index];
            let gradients = errors.map(|error| error * feature);
            let next_n =
                std::array::from_fn(|head| previous_n[head] + gradients[head] * gradients[head]);
            let next_sqrt_n = next_n.map(f32::sqrt);
            let next_z = std::array::from_fn(|head| {
                let sigma = (next_sqrt_n[head] - previous_sqrt_n[head]) / ALPHA;
                previous_z[head] + (gradients[head] - sigma * previous_weights[head])
            });
            self.n[index] = next_n;
            self.sqrt_n[index] = next_sqrt_n;
            self.z[index] = next_z;
            self.weights[index] =
                std::array::from_fn(|head| weight_from_state(next_z[head], next_sqrt_n[head]));
        }
        self.calibration_count = self.calibration_count.map(|count| count.saturating_add(1));
        if self
            .calibration_count
            .iter()
            .all(|count| *count == self.calibration_count[0])
        {
            let count = f32::from(u16::try_from(self.calibration_count[0]).unwrap_or(u16::MAX));
            self.calibration_error = std::array::from_fn(|head| {
                let absolute_error = (predictions.0[head] - example.targets.0[head]).abs();
                self.calibration_error[head]
                    + (absolute_error - self.calibration_error[head]) / count
            });
        } else {
            for head in 0..HEAD_COUNT {
                let count =
                    f32::from(u16::try_from(self.calibration_count[head]).unwrap_or(u16::MAX));
                let absolute_error = (predictions.0[head] - example.targets.0[head]).abs();
                self.calibration_error[head] +=
                    (absolute_error - self.calibration_error[head]) / count;
            }
        }
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(17 + HEAD_COUNT * (FEATURE_COUNT * 8 + 12));
        output.extend_from_slice(b"RFLM\x02");
        output.extend_from_slice(&FEATURE_REVISION.to_le_bytes());
        output.extend_from_slice(&TARGET_REVISION.to_le_bytes());
        output.extend_from_slice(&CALIBRATION_REVISION.to_le_bytes());
        for values in [&self.z, &self.n] {
            for head in 0..HEAD_COUNT {
                for feature in values {
                    output.extend_from_slice(&feature[head].to_bits().to_le_bytes());
                }
            }
        }
        for count in self.calibration_count {
            output.extend_from_slice(&count.to_le_bytes());
        }
        for error in self.calibration_error {
            output.extend_from_slice(&error.to_bits().to_le_bytes());
        }
        output
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, ()> {
        let expected = 17 + HEAD_COUNT * (FEATURE_COUNT * 8 + 12);
        if bytes.len() != expected || &bytes[..5] != b"RFLM\x02" {
            return Err(());
        }
        let mut input = &bytes[5..];
        if read_u32(&mut input)? != FEATURE_REVISION
            || read_u32(&mut input)? != TARGET_REVISION
            || read_u32(&mut input)? != CALIBRATION_REVISION
        {
            return Err(());
        }
        let mut model = Self::zero();
        for (state_index, values) in [&mut model.z, &mut model.n].into_iter().enumerate() {
            for head in 0..HEAD_COUNT {
                for feature in values.iter_mut() {
                    let (encoded, remainder) = input.split_at(4);
                    input = remainder;
                    feature[head] = f32::from_bits(u32::from_le_bytes(encoded.try_into().unwrap()));
                    if !feature[head].is_finite() || state_index == 1 && feature[head] < 0.0 {
                        return Err(());
                    }
                }
            }
        }
        for count in &mut model.calibration_count {
            *count = read_u64(&mut input)?;
        }
        for (count, error) in model
            .calibration_count
            .iter()
            .zip(&mut model.calibration_error)
        {
            *error = f32::from_bits(read_u32(&mut input)?);
            if !error.is_finite() || !(0.0..=1.0).contains(error) || *count == 0 && *error != 0.0 {
                return Err(());
            }
        }
        if !input.is_empty() {
            return Err(());
        }
        model.rebuild_cache();
        Ok(model)
    }

    fn rebuild_cache(&mut self) {
        for index in 0..FEATURE_COUNT {
            for head in 0..HEAD_COUNT {
                let sqrt_n = self.n[index][head].sqrt();
                self.sqrt_n[index][head] = sqrt_n;
                self.weights[index][head] = weight_from_state(self.z[index][head], sqrt_n);
            }
        }
    }
}

fn weight_from_state(z: f32, sqrt_n: f32) -> f32 {
    const ALPHA: f32 = 0.1;
    const BETA: f32 = 1.0;
    const L2: f32 = 1.0;
    -z / ((BETA + sqrt_n) / ALPHA + L2)
}

pub(crate) fn derive_targets(
    attempts: &[AttemptObservation],
    consequences: &[ConsequenceObservation],
) -> Vec<TrainingExample> {
    let mut consequence_flags = HashMap::<[u8; 32], u8>::new();
    for consequence in consequences {
        let flag = match consequence.kind {
            ConsequenceKind::Admitted => 1,
            ConsequenceKind::ParetoImprovement => 2,
            ConsequenceKind::CrossGoalUse => 4,
            ConsequenceKind::Compression => 8,
        };
        *consequence_flags.entry(consequence.subject).or_default() |= flag;
    }
    let mut accepted_children = HashMap::<([u8; 32], [u8; 32]), usize>::new();
    for attempt in attempts
        .iter()
        .filter(|attempt| attempt.verdict == VerdictTarget::Accepted)
    {
        *accepted_children
            .entry((attempt.parent, attempt.claim))
            .or_default() += 1;
    }
    attempts
        .iter()
        .map(|attempt| {
            let accepted = attempt.verdict == VerdictTarget::Accepted;
            let flags = consequence_flags.get(&attempt.id).copied().unwrap_or(0);
            let has = |kind| {
                flags
                    & match kind {
                        ConsequenceKind::Admitted => 1,
                        ConsequenceKind::ParetoImprovement => 2,
                        ConsequenceKind::CrossGoalUse => 4,
                        ConsequenceKind::Compression => 8,
                    }
                    != 0
            };
            let descendants = accepted_children
                .get(&(attempt.artifact, attempt.claim))
                .copied()
                .unwrap_or(0);
            let immediate = accepted
                && (has(ConsequenceKind::Admitted) || has(ConsequenceKind::ParetoImprovement));
            let dead_end = !accepted || (descendants == 0 && !immediate);
            TrainingExample {
                key: attempt.id,
                corpus_key: attempt.claim,
                features: attempt.features,
                targets: Targets([
                    f32::from(immediate),
                    f32::from(u8::try_from(descendants.min(4)).unwrap()) / 4.0,
                    f32::from(has(ConsequenceKind::CrossGoalUse)),
                    f32::from(has(ConsequenceKind::Compression)),
                    f32::from(accepted),
                    (attempt.verification_cost / 16.0).clamp(0.0, 1.0),
                    f32::from(dead_end),
                ]),
                active_features: active_feature_mask(attempt.features),
                role: assign_role(attempt.claim),
            }
        })
        .collect()
}

fn active_feature_mask(features: Features) -> u16 {
    features
        .0
        .iter()
        .enumerate()
        .fold(0_u16, |mask, (index, feature)| {
            if *feature == 0.0 {
                mask
            } else {
                mask | (1_u16 << index)
            }
        })
}

pub(crate) fn assign_role(key: [u8; 32]) -> CorpusRole {
    if key[0].is_multiple_of(5) {
        CorpusRole::Selection { uses: 0 }
    } else {
        CorpusRole::Replay
    }
}

pub(crate) fn rotate_selection(examples: &mut [TrainingExample], maximum_uses: u8) {
    for example in examples {
        if let CorpusRole::Selection { uses } = &mut example.role {
            *uses = uses.saturating_add(1);
            if *uses >= maximum_uses {
                example.role = CorpusRole::Replay;
            }
        }
    }
}

fn bounded_corpus(examples: &[TrainingExample], selection: bool) -> Vec<&TrainingExample> {
    let eligible = |example: &&TrainingExample| {
        matches!(example.role, CorpusRole::Selection { .. }) == selection
    };
    let capacity = MAX_REPLAY_BATCH.min(examples.len());
    let mut groups = HashSet::<[u8; 32]>::with_capacity(capacity);
    let mut selected_keys = HashSet::<[u8; 32]>::with_capacity(capacity);
    let mut corpus = Vec::with_capacity(capacity);
    for example in examples.iter().filter(eligible) {
        if groups.insert(example.corpus_key) {
            selected_keys.insert(example.key);
            corpus.push(example);
            if corpus.len() == MAX_REPLAY_BATCH {
                break;
            }
        }
    }
    if corpus.len() < MAX_REPLAY_BATCH {
        for example in examples.iter().filter(eligible) {
            if groups.contains(&example.corpus_key) && selected_keys.insert(example.key) {
                corpus.push(example);
                if corpus.len() == MAX_REPLAY_BATCH {
                    break;
                }
            }
        }
    }
    corpus.sort_unstable_by_key(|example| example.key);
    corpus
}

pub(crate) fn compare_models(
    champion: &FtrlModel,
    challenger: &FtrlModel,
    selection: &[&TrainingExample],
) -> PromotionDecision {
    if selection
        .iter()
        .map(|example| example.corpus_key)
        .collect::<BTreeSet<_>>()
        .len()
        < MIN_SELECTION_CASES
    {
        return PromotionDecision::Reject;
    }
    let champion_loss = losses(champion, selection);
    let challenger_loss = losses(challenger, selection);
    promotion_from_losses(champion_loss, challenger_loss)
}

fn promotion_from_losses(
    champion_loss: [f32; HEAD_COUNT],
    challenger_loss: [f32; HEAD_COUNT],
) -> PromotionDecision {
    let improved = challenger_loss
        .iter()
        .zip(champion_loss)
        .filter(|(challenger, champion)| **challenger < *champion * 0.98)
        .count();
    let protected_regression = challenger_loss
        .iter()
        .zip(champion_loss)
        .any(|(challenger, champion)| *challenger > champion * 1.05 + f32::EPSILON);
    let champion_total = champion_loss.iter().sum::<f32>();
    let challenger_total = challenger_loss.iter().sum::<f32>();
    if !protected_regression && challenger_total < champion_total * 0.99 {
        PromotionDecision::Promote
    } else if improved > 0 {
        PromotionDecision::Specialist
    } else {
        PromotionDecision::Reject
    }
}

pub(crate) fn revision_digest(model: &FtrlModel) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-ftrl-model-revision-v2\0");
    digest.update(model.encode());
    digest.finalize().into()
}

fn losses(model: &FtrlModel, selection: &[&TrainingExample]) -> [f32; HEAD_COUNT] {
    let mut groups = BTreeMap::<[u8; 32], ([f32; HEAD_COUNT], u32)>::new();
    for example in selection {
        let prediction = model.predict(example.features);
        let group = groups
            .entry(example.corpus_key)
            .or_insert(([0.0; HEAD_COUNT], 0));
        group.1 = group.1.saturating_add(1);
        for (head, loss) in group.0.iter_mut().enumerate() {
            let error = prediction.0[head] - example.targets.0[head];
            *loss += error * error;
        }
    }
    let mut losses = [0.0; HEAD_COUNT];
    for (group_losses, count) in groups.values() {
        let count = f32::from(u16::try_from(*count).unwrap_or(u16::MAX));
        for (loss, group_loss) in losses.iter_mut().zip(group_losses) {
            *loss += group_loss / count;
        }
    }
    let group_count = f32::from(u16::try_from(groups.len()).unwrap_or(u16::MAX));
    for loss in &mut losses {
        *loss /= group_count;
    }
    losses
}

fn sigmoid(value: f32) -> f32 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exponential = value.exp();
        exponential / (1.0 + exponential)
    }
}

fn sigmoid_heads(linear: [f32; HEAD_COUNT]) -> Targets {
    let mut predictions = [0.0_f32; HEAD_COUNT];
    for head in 0..HEAD_COUNT {
        predictions[head] = (0..head)
            .find(|previous| linear[*previous].to_bits() == linear[head].to_bits())
            .map_or_else(|| sigmoid(linear[head]), |previous| predictions[previous]);
    }
    Targets(predictions)
}

fn push_model(output: &mut Vec<u8>, model: Option<&FtrlModel>) {
    match model {
        Some(model) => {
            output.push(1);
            push_bytes(output, &model.encode());
        }
        None => output.push(0),
    }
}

fn read_model(input: &mut &[u8]) -> Result<Option<FtrlModel>, ()> {
    match take(input, 1)?[0] {
        0 => Ok(None),
        1 => Ok(Some(FtrlModel::decode(take_sized(input)?)?)),
        _ => Err(()),
    }
}

fn push_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    output.extend_from_slice(bytes);
}

fn read_u64(input: &mut &[u8]) -> Result<u64, ()> {
    Ok(u64::from_le_bytes(take(input, 8)?.try_into().unwrap()))
}

fn read_u32(input: &mut &[u8]) -> Result<u32, ()> {
    Ok(u32::from_le_bytes(take(input, 4)?.try_into().unwrap()))
}

fn take_sized<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], ()> {
    let length = usize::try_from(read_u64(input)?).map_err(|_| ())?;
    take(input, length)
}

fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], ()> {
    if input.len() < count {
        return Err(());
    }
    let (value, remainder) = input.split_at(count);
    *input = remainder;
    Ok(value)
}

#[cfg(test)]
fn target_map(examples: &[TrainingExample]) -> BTreeMap<[u8; 32], Targets> {
    examples
        .iter()
        .map(|example| (example.key, example.targets))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UncachedFtrlModel {
        z: [[f32; FEATURE_COUNT]; HEAD_COUNT],
        n: [[f32; FEATURE_COUNT]; HEAD_COUNT],
        calibration_count: [u64; HEAD_COUNT],
        calibration_error: [f32; HEAD_COUNT],
    }

    impl UncachedFtrlModel {
        fn zero() -> Self {
            Self {
                z: [[0.0; FEATURE_COUNT]; HEAD_COUNT],
                n: [[0.0; FEATURE_COUNT]; HEAD_COUNT],
                calibration_count: [0; HEAD_COUNT],
                calibration_error: [0.0; HEAD_COUNT],
            }
        }

        fn weights(&self, head: usize) -> [f32; FEATURE_COUNT] {
            const ALPHA: f32 = 0.1;
            const BETA: f32 = 1.0;
            const L1: f32 = 0.0;
            const L2: f32 = 1.0;
            std::array::from_fn(|index| {
                let z = self.z[head][index];
                if z.abs() <= L1 {
                    0.0
                } else {
                    -(z - z.signum() * L1) / ((BETA + self.n[head][index].sqrt()) / ALPHA + L2)
                }
            })
        }

        fn predict(&self, features: Features) -> Targets {
            let mut predictions = [0.0; HEAD_COUNT];
            for (head, prediction) in predictions.iter_mut().enumerate() {
                *prediction = sigmoid(
                    self.weights(head)
                        .iter()
                        .zip(features.0)
                        .map(|(weight, feature)| weight * feature)
                        .sum(),
                );
            }
            Targets(predictions)
        }

        fn update(&mut self, example: &TrainingExample) {
            const ALPHA: f32 = 0.1;
            let predictions = self.predict(example.features);
            for head in 0..HEAD_COUNT {
                let weights = self.weights(head);
                for (index, (feature, weight)) in
                    example.features.0.iter().copied().zip(weights).enumerate()
                {
                    let gradient = (predictions.0[head] - example.targets.0[head]) * feature;
                    let sigma = ((self.n[head][index] + gradient * gradient).sqrt()
                        - self.n[head][index].sqrt())
                        / ALPHA;
                    self.z[head][index] += gradient - sigma * weight;
                    self.n[head][index] += gradient * gradient;
                }
                self.calibration_count[head] = self.calibration_count[head].saturating_add(1);
                let count =
                    f32::from(u16::try_from(self.calibration_count[head]).unwrap_or(u16::MAX));
                let absolute_error = (predictions.0[head] - example.targets.0[head]).abs();
                self.calibration_error[head] +=
                    (absolute_error - self.calibration_error[head]) / count;
            }
        }

        fn forecast(&self, features: Features) -> PotentialForecast {
            let estimates = self.predict(features);
            PotentialForecast(std::array::from_fn(|head| {
                let support = self.n[head]
                    .iter()
                    .zip(features.0)
                    .map(|(accumulated_gradient, feature)| accumulated_gradient * feature * feature)
                    .sum::<f32>();
                let epistemic = (1.0 / (1.0 + support.max(0.0))).sqrt();
                let calibration_error = if self.calibration_count[head] == 0 {
                    1.0
                } else {
                    self.calibration_error[head]
                };
                Forecast {
                    estimate: estimates.0[head],
                    calibration_error,
                    uncertainty: epistemic.max(calibration_error).clamp(0.0, 1.0),
                }
            }))
        }
    }

    fn reference_weights(model: &FtrlModel, head: usize) -> [f32; FEATURE_COUNT] {
        const ALPHA: f32 = 0.1;
        const BETA: f32 = 1.0;
        const L1: f32 = 0.0;
        const L2: f32 = 1.0;
        std::array::from_fn(|index| {
            let z = model.z[index][head];
            if z.abs() <= L1 {
                0.0
            } else {
                -(z - z.signum() * L1) / ((BETA + model.n[index][head].sqrt()) / ALPHA + L2)
            }
        })
    }

    fn reference_predict(model: &FtrlModel, features: Features) -> Targets {
        let mut predictions = [0.0; HEAD_COUNT];
        for (head, prediction) in predictions.iter_mut().enumerate() {
            *prediction = sigmoid(
                reference_weights(model, head)
                    .iter()
                    .zip(features.0)
                    .map(|(weight, feature)| weight * feature)
                    .sum(),
            );
        }
        Targets(predictions)
    }

    fn reference_forecast(model: &FtrlModel, features: Features) -> PotentialForecast {
        let estimates = reference_predict(model, features);
        PotentialForecast(std::array::from_fn(|head| {
            let support = features
                .0
                .iter()
                .enumerate()
                .map(|(index, feature)| model.n[index][head] * feature * feature)
                .sum::<f32>();
            let epistemic = (1.0 / (1.0 + support.max(0.0))).sqrt();
            let calibration_error = if model.calibration_count[head] == 0 {
                1.0
            } else {
                model.calibration_error[head]
            };
            Forecast {
                estimate: estimates.0[head],
                calibration_error,
                uncertainty: epistemic.max(calibration_error).clamp(0.0, 1.0),
            }
        }))
    }

    fn reference_update(model: &mut FtrlModel, example: &TrainingExample) {
        const ALPHA: f32 = 0.1;
        let predictions = reference_predict(model, example.features);
        for head in 0..HEAD_COUNT {
            let weights = reference_weights(model, head);
            for (index, (feature, weight)) in
                example.features.0.iter().copied().zip(weights).enumerate()
            {
                let gradient = (predictions.0[head] - example.targets.0[head]) * feature;
                let sigma = ((model.n[index][head] + gradient * gradient).sqrt()
                    - model.n[index][head].sqrt())
                    / ALPHA;
                model.z[index][head] += gradient - sigma * weight;
                model.n[index][head] += gradient * gradient;
            }
            model.calibration_count[head] = model.calibration_count[head].saturating_add(1);
            let count = f32::from(u16::try_from(model.calibration_count[head]).unwrap_or(u16::MAX));
            let absolute_error = (predictions.0[head] - example.targets.0[head]).abs();
            model.calibration_error[head] +=
                (absolute_error - model.calibration_error[head]) / count;
        }
    }

    fn features(operator_bucket: usize) -> Features {
        let mut values = [0.0; FEATURE_COUNT];
        values[0] = 1.0;
        values[4 + operator_bucket] = 1.0;
        Features(values)
    }

    fn example(key: u8, bucket: usize, accepted: bool, role: CorpusRole) -> TrainingExample {
        let mut targets = [0.0; HEAD_COUNT];
        targets[0] = f32::from(accepted);
        targets[6] = f32::from(!accepted);
        TrainingExample {
            key: [key; 32],
            corpus_key: [key; 32],
            features: features(bucket),
            active_features: active_feature_mask(features(bucket)),
            targets: Targets(targets),
            role,
        }
    }

    #[test]
    fn ftrl_updates_raise_observed_probability_and_reject_non_finite_state() {
        let sample = example(1, 0, true, CorpusRole::Replay);
        let mut model = FtrlModel::zero();
        let before = model.predict(sample.features).0[0];
        model.update(&sample);
        let after = model.predict(sample.features).0[0];
        let forecast = model.forecast(sample.features).0[0];
        let encoded = model.encode();

        assert!(
            (before - 0.5).abs() < f32::EPSILON
                && after > before
                && (model.z[0][0] + 0.5).abs() < f32::EPSILON
                && (model.n[0][0] - 0.25).abs() < f32::EPSILON
                && forecast.calibration_error.is_finite()
                && forecast.uncertainty < 1.0
                && FtrlModel::decode(&encoded)
                    .unwrap()
                    .predict(sample.features)
                    == model.predict(sample.features)
        );
        let mut malformed = encoded;
        malformed[17..21].copy_from_slice(&f32::NAN.to_bits().to_le_bytes());
        assert!(FtrlModel::decode(&malformed).is_err());
        let mut incompatible = model.encode();
        incompatible[5..9].copy_from_slice(&2_u32.to_le_bytes());
        assert!(FtrlModel::decode(&incompatible).is_err());
    }

    #[test]
    fn cached_ftrl_preserves_the_uncached_numerical_and_canonical_result() {
        let examples = (0..64_u8)
            .map(|key| {
                example(
                    key,
                    usize::from(key % 8),
                    key.is_multiple_of(3),
                    CorpusRole::Replay,
                )
            })
            .collect::<Vec<_>>();
        let mut cached = FtrlModel::zero();
        let mut reference = FtrlModel::zero();
        for _ in 0..8 {
            for sample in &examples {
                cached.update(sample);
                reference_update(&mut reference, sample);
            }
        }
        reference.rebuild_cache();
        assert_eq!(cached.encode(), reference.encode());
        for sample in &examples {
            assert_eq!(
                cached.predict(sample.features),
                reference_predict(&reference, sample.features)
            );
            assert_eq!(
                cached.forecast(sample.features),
                reference_forecast(&reference, sample.features)
            );
        }
    }

    #[test]
    #[ignore = "release-mode development microbenchmark"]
    fn cached_ftrl_training_and_forecasting_are_four_times_faster() {
        const SAMPLES: usize = 5;
        const TRAINING_UPDATES: usize = 20_000;
        const FORECASTS: usize = 100_000;
        let examples = (0..64_u8)
            .map(|key| {
                let mut example = example(
                    key,
                    usize::from(key % 8),
                    key.is_multiple_of(3),
                    CorpusRole::Replay,
                );
                example.features.0[1] = f32::from(key) / 255.0;
                example.features.0[2] = f32::from(key.saturating_add(1)) / 255.0;
                example.features.0[3] = f32::from(key) / 128.0 - 0.25;
                example.features.0[4] = f32::from(key) / 1024.0;
                example.features.0[13] = example.features.0[3];
                example.features.0[14] = f32::from(key.is_multiple_of(2));
                example.features.0[15] = f32::from(key) / 256.0;
                example.active_features = active_feature_mask(example.features);
                example
            })
            .collect::<Vec<_>>();
        let mut trained = FtrlModel::zero();
        let mut uncached_trained = UncachedFtrlModel::zero();
        for sample in &examples {
            trained.update(sample);
            uncached_trained.update(sample);
        }
        let forecast_features = (0..FORECASTS)
            .map(|index| examples[index % examples.len()].features)
            .collect::<Vec<_>>();
        let measure = |work: &mut dyn FnMut()| {
            let started = std::time::Instant::now();
            work();
            u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX)
        };
        let mut cached_training = Vec::new();
        let mut reference_training = Vec::new();
        let mut cached_forecasting = Vec::new();
        let mut reference_forecasting = Vec::new();
        for sample in 0..SAMPLES {
            let mut run_cached_training = || {
                let mut model = FtrlModel::zero();
                for index in 0..TRAINING_UPDATES {
                    model.update(std::hint::black_box(&examples[index % examples.len()]));
                }
                std::hint::black_box(model.encode());
            };
            let mut run_reference_training = || {
                let mut model = UncachedFtrlModel::zero();
                for index in 0..TRAINING_UPDATES {
                    model.update(std::hint::black_box(&examples[index % examples.len()]));
                }
                std::hint::black_box(model);
            };
            let mut run_cached_forecasting = || {
                let mut output = Vec::with_capacity(forecast_features.len());
                trained.forecast_batch(std::hint::black_box(&forecast_features), &mut output);
                std::hint::black_box(output);
            };
            let mut run_reference_forecasting = || {
                for features in &forecast_features {
                    std::hint::black_box(
                        uncached_trained.forecast(std::hint::black_box(*features)),
                    );
                }
            };
            if sample.is_multiple_of(2) {
                cached_training.push(measure(&mut run_cached_training));
                reference_training.push(measure(&mut run_reference_training));
                cached_forecasting.push(measure(&mut run_cached_forecasting));
                reference_forecasting.push(measure(&mut run_reference_forecasting));
            } else {
                reference_training.push(measure(&mut run_reference_training));
                cached_training.push(measure(&mut run_cached_training));
                reference_forecasting.push(measure(&mut run_reference_forecasting));
                cached_forecasting.push(measure(&mut run_cached_forecasting));
            }
        }
        let median = |values: &mut Vec<u64>| {
            values.sort_unstable();
            values[values.len() / 2]
        };
        let cached_training = median(&mut cached_training);
        let reference_training = median(&mut reference_training);
        let cached_forecasting = median(&mut cached_forecasting);
        let reference_forecasting = median(&mut reference_forecasting);
        eprintln!(
            "training: reference={reference_training}ns cached={cached_training}ns; forecasting: reference={reference_forecasting}ns cached={cached_forecasting}ns"
        );
        assert!(reference_training >= cached_training.saturating_mul(4));
        assert!(reference_forecasting >= cached_forecasting.saturating_mul(4));
    }

    #[test]
    fn delayed_descendants_revise_targets_without_mutating_attempts() {
        let parent = AttemptObservation {
            id: [1; 32],
            artifact: [11; 32],
            claim: [21; 32],
            parent: [0; 32],
            features: features(0),
            verdict: VerdictTarget::Accepted,
            verification_cost: 1.0,
        };
        let child = AttemptObservation {
            id: [2; 32],
            artifact: [12; 32],
            claim: parent.claim,
            parent: parent.artifact,
            features: features(0),
            verdict: VerdictTarget::Accepted,
            verification_cost: 1.0,
        };
        let before = derive_targets(std::slice::from_ref(&parent), &[]);
        let after = derive_targets(
            &[parent.clone(), child],
            &[
                ConsequenceObservation {
                    subject: parent.id,
                    kind: ConsequenceKind::Admitted,
                },
                ConsequenceObservation {
                    subject: parent.id,
                    kind: ConsequenceKind::ParetoImprovement,
                },
            ],
        );
        let revised = target_map(&after)[&parent.id];

        assert!(
            before[0].targets.0[1] == 0.0
                && (revised.0[0] - 1.0).abs() < f32::EPSILON
                && revised.0[1] > 0.0
                && revised.0[2] == 0.0
                && parent.verdict == VerdictTarget::Accepted
        );
    }

    #[test]
    fn selection_is_disjoint_rotates_and_gates_promotion() {
        let mut examples = (0..50)
            .map(|key| {
                example(
                    key,
                    usize::from(key % 2),
                    key % 2 == 0,
                    assign_role([key; 32]),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            examples
                .iter()
                .any(|example| matches!(example.role, CorpusRole::Replay))
        );
        assert!(
            examples
                .iter()
                .any(|example| matches!(example.role, CorpusRole::Selection { .. }))
        );
        let mut challenger = FtrlModel::zero();
        for _ in 0..8 {
            for sample in examples
                .iter()
                .filter(|sample| matches!(sample.role, CorpusRole::Replay))
            {
                challenger.update(sample);
            }
        }
        let selection = examples
            .iter()
            .filter(|sample| matches!(sample.role, CorpusRole::Selection { .. }))
            .collect::<Vec<_>>();
        assert_eq!(
            compare_models(&FtrlModel::zero(), &challenger, &selection),
            PromotionDecision::Promote
        );
        for _ in 0..3 {
            rotate_selection(&mut examples, 3);
        }
        assert!(
            examples
                .iter()
                .all(|example| matches!(example.role, CorpusRole::Replay))
        );
        assert_ne!(
            revision_digest(&challenger),
            revision_digest(&FtrlModel::zero())
        );
    }

    #[test]
    fn initial_learning_reserves_exact_disjoint_minimum_corpora() {
        let examples = |count: u8| {
            (0..count)
                .map(|key| example(key, usize::from(key % 2), key % 2 == 0, CorpusRole::Replay))
                .collect::<Vec<_>>()
        };
        let mut sufficient = examples(16);
        let mut state = LearningState::default();
        state.assign_new_corpus_roles(&mut sufficient);
        let selection = sufficient
            .iter()
            .filter(|example| matches!(example.role, CorpusRole::Selection { .. }))
            .count();
        let replay = sufficient.len() - selection;
        assert_eq!((selection, replay), (8, 8));

        let mut insufficient = examples(15);
        let mut state = LearningState::default();
        state.assign_new_corpus_roles(&mut insufficient);
        let selection = insufficient
            .iter()
            .filter(|example| matches!(example.role, CorpusRole::Selection { .. }))
            .count();
        assert_eq!((selection, insufficient.len() - selection), (7, 8));
        assert_eq!(
            state.learn(&mut insufficient),
            PromotionDecision::Reject,
            "fewer than sixteen claims cannot satisfy both disjoint minimum cohorts"
        );
    }

    #[test]
    fn model_state_round_trips_promotions_and_rolls_back_to_bootstrap() {
        let mut examples = (0..50)
            .map(|key| {
                example(
                    key,
                    usize::from(key % 2),
                    key % 2 == 0,
                    assign_role([key; 32]),
                )
            })
            .collect::<Vec<_>>();
        let mut state = LearningState::default();
        assert_eq!(state.learn(&mut examples), PromotionDecision::Promote);
        let digest = state.revision_digest("domain");
        let encoded = state.encode();
        let mut recovered = LearningState::decode(&encoded).unwrap();

        assert!(
            recovered.generation() == 1
                && recovered.pinned_model().is_some()
                && recovered.revision_digest("domain") == digest
                && recovered.encode() == encoded
                && recovered.specialist_count() == 0
        );
        assert_eq!(recovered.rollback(), PromotionDecision::Rollback);
        assert!(recovered.pinned_model().is_none() && recovered.specialist_count() == 1);
    }

    #[test]
    fn fresh_selection_evidence_automatically_rolls_back_a_regressed_champion() {
        let mut regressed = FtrlModel::zero();
        regressed.z[0][0] = 10.0;
        regressed.n[0][0] = 1.0;
        regressed.z[0][6] = -10.0;
        regressed.n[0][6] = 1.0;
        regressed.rebuild_cache();
        let mut state = LearningState {
            generation: 1,
            champion: Some(regressed),
            predecessor: None,
            predecessor_is_bootstrap: true,
            specialists: Vec::new(),
            roles: BTreeMap::new(),
        };
        let mut examples = (0..8)
            .map(|index| {
                let key = u8::try_from(index * 5).unwrap();
                example(key, 0, true, CorpusRole::Selection { uses: 0 })
            })
            .collect::<Vec<_>>();

        assert_eq!(state.learn(&mut examples), PromotionDecision::Rollback);
        assert!(state.pinned_model().is_none() && state.specialist_count() == 1);
    }

    #[test]
    fn comparison_retains_incomparable_specialists_and_rejects_non_improvements() {
        let selection_examples = (0..8)
            .map(|key| example(key, 0, true, CorpusRole::Selection { uses: 0 }))
            .collect::<Vec<_>>();
        let selection = selection_examples.iter().collect::<Vec<_>>();
        let champion = FtrlModel::zero();
        let mut incomparable = FtrlModel::zero();
        incomparable.z[0][0] = -10.0;
        incomparable.n[0][0] = 1.0;
        incomparable.z[0][1] = -10.0;
        incomparable.n[0][1] = 1.0;
        incomparable.rebuild_cache();

        assert_eq!(
            compare_models(&champion, &incomparable, &selection),
            PromotionDecision::Specialist
        );
        assert_eq!(
            compare_models(&champion, &champion, &selection),
            PromotionDecision::Reject
        );
    }

    #[test]
    fn learning_state_rejects_corpus_overlap_and_invalid_revision_ancestry() {
        let mut roles = BTreeMap::new();
        roles.insert([7; 32], CorpusRole::Replay);
        let state = LearningState {
            roles,
            ..LearningState::default()
        };
        assert!(state.corpus_is_valid(&[([1; 32], [7; 32])]));
        assert!(!state.corpus_is_valid(&[([1; 32], [9; 32])]));
        let encoded = state.encode();
        let mut duplicate = encoded.clone();
        duplicate[23..31].copy_from_slice(&2_u64.to_le_bytes());
        duplicate.extend_from_slice(&encoded[31..]);
        assert!(LearningState::decode(&duplicate).is_err());

        let mut invalid_ancestry = LearningState::default().encode();
        invalid_ancestry[14] = 1;
        assert!(LearningState::decode(&invalid_ancestry).is_err());
    }

    #[test]
    fn learning_state_rejects_more_specialists_than_the_runtime_can_retain() {
        let specialist = FtrlModel::zero();
        let state = LearningState {
            generation: 1,
            champion: Some(FtrlModel::zero()),
            predecessor: None,
            predecessor_is_bootstrap: true,
            specialists: vec![specialist; MAX_SPECIALISTS + 1],
            roles: BTreeMap::new(),
        };

        assert!(LearningState::decode(&state.encode()).is_err());
    }
}
