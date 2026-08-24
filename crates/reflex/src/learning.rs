#[cfg(any(test, feature = "internal-experiments"))]
use std::cmp::Ordering;
#[cfg(feature = "internal-experiments")]
use std::collections::HashSet;
#[cfg(not(any(test, feature = "internal-experiments")))]
use std::collections::{BTreeMap, BTreeSet};
#[cfg(any(test, feature = "internal-experiments"))]
use std::collections::{BTreeMap, BTreeSet, HashMap};

use sha2::{Digest, Sha256};

#[cfg(any(test, feature = "internal-experiments"))]
use crate::policy::{AllocationQueue, OperationalPartition, operational_ranked_selections};

pub(crate) const BASE_FEATURE_COUNT: usize = 16;
pub(crate) const FEATURE_COUNT: usize = BASE_FEATURE_COUNT + crate::domain::PROPOSAL_FEATURE_COUNT;
pub(crate) const HEAD_COUNT: usize = 7;
const FEATURE_REVISION: u32 = 2;
const TARGET_REVISION: u32 = 1;
const CALIBRATION_REVISION: u32 = 1;
#[cfg(feature = "internal-experiments")]
const MAX_REPLAY_BATCH: usize = 16_384;
#[cfg(feature = "internal-experiments")]
const TRAINING_EPOCHS: usize = 8;
const MAX_SELECTION_USES: u8 = 3;
const MAX_SPECIALISTS: usize = 8;
pub(crate) const MIN_ONLINE_TRAINING_EXAMPLES: usize = 32;
pub(crate) const ONLINE_TRAINING_GROWTH: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Features(pub(crate) [f32; FEATURE_COUNT]);

#[cfg(any(test, feature = "internal-experiments"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Targets(pub(crate) [f32; HEAD_COUNT]);

#[cfg(any(test, feature = "internal-experiments"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Forecast {
    pub(crate) estimate: f32,
    pub(crate) calibration_error: f32,
    pub(crate) uncertainty: f32,
}

#[cfg(any(test, feature = "internal-experiments"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PotentialForecast(pub(crate) [Forecast; HEAD_COUNT]);

#[cfg(any(test, feature = "internal-experiments"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum VerdictTarget {
    Accepted,
    #[cfg(feature = "internal-experiments")]
    Refuted,
    #[cfg(feature = "internal-experiments")]
    Unknown,
}

#[cfg(any(test, feature = "internal-experiments"))]
#[derive(Clone, Debug)]
pub(crate) struct AttemptObservation {
    pub(crate) id: [u8; 32],
    pub(crate) artifact: [u8; 32],
    pub(crate) claim: [u8; 32],
    pub(crate) parent: [u8; 32],
    pub(crate) features: Features,
    pub(crate) verdict: VerdictTarget,
    pub(crate) verification_cost: f32,
    pub(crate) allocation_queue: AllocationQueue,
    pub(crate) bootstrap_rank: u32,
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

#[cfg(any(test, feature = "internal-experiments"))]
#[derive(Clone, Debug)]
pub(crate) struct TrainingExample {
    pub(crate) key: [u8; 32],
    #[cfg(feature = "internal-experiments")]
    pub(crate) corpus_key: [u8; 32],
    behavior_rank: u32,
    behavior_sequence: u32,
    allocation_queue: AllocationQueue,
    bootstrap_rank: u32,
    pub(crate) features: Features,
    active_features: u32,
    pub(crate) targets: Targets,
    #[cfg(feature = "internal-experiments")]
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
#[repr(u8)]
pub(crate) enum LegacyFtrlHead {
    ImmediateImprovement = 1,
    UsefulDescendants = 2,
    CrossGoalLeverage = 3,
    CompressionValue = 4,
    KernelAcceptance = 5,
    VerificationCost = 6,
    DeadEndRisk = 7,
}

impl LegacyFtrlHead {
    const fn tag(self) -> u8 {
        self as u8
    }
}

const LEGACY_FTRL_HEADS: [LegacyFtrlHead; HEAD_COUNT] = [
    LegacyFtrlHead::ImmediateImprovement,
    LegacyFtrlHead::UsefulDescendants,
    LegacyFtrlHead::CrossGoalLeverage,
    LegacyFtrlHead::CompressionValue,
    LegacyFtrlHead::KernelAcceptance,
    LegacyFtrlHead::VerificationCost,
    LegacyFtrlHead::DeadEndRisk,
];

#[derive(Clone, Copy)]
pub(crate) struct FtrlConversionView<'a> {
    weights: &'a [[f32; HEAD_COUNT]; FEATURE_COUNT],
    second_moments: &'a [[f32; HEAD_COUNT]; FEATURE_COUNT],
    calibration_counts: &'a [u64; HEAD_COUNT],
    calibration_errors: &'a [f32; HEAD_COUNT],
}

impl<'a> FtrlConversionView<'a> {
    pub(crate) const fn semantic_heads() -> &'static [LegacyFtrlHead; HEAD_COUNT] {
        &LEGACY_FTRL_HEADS
    }

    pub(crate) const fn weights(self) -> &'a [[f32; HEAD_COUNT]; FEATURE_COUNT] {
        self.weights
    }

    pub(crate) const fn second_moments(self) -> &'a [[f32; HEAD_COUNT]; FEATURE_COUNT] {
        self.second_moments
    }

    pub(crate) const fn calibration_counts(self) -> &'a [u64; HEAD_COUNT] {
        self.calibration_counts
    }

    pub(crate) const fn calibration_errors(self) -> &'a [f32; HEAD_COUNT] {
        self.calibration_errors
    }

    pub(crate) fn content_identity(self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"reflex-ftrl-conversion-view-v1\0");
        digest.update(FEATURE_REVISION.to_le_bytes());
        digest.update(TARGET_REVISION.to_le_bytes());
        digest.update(CALIBRATION_REVISION.to_le_bytes());
        digest.update(
            u64::try_from(FEATURE_COUNT)
                .expect("the fixed FTRL feature count fits u64")
                .to_le_bytes(),
        );
        digest.update(
            u64::try_from(HEAD_COUNT)
                .expect("the fixed FTRL head count fits u64")
                .to_le_bytes(),
        );
        for (head, semantic_head) in LEGACY_FTRL_HEADS.into_iter().enumerate() {
            digest.update([semantic_head.tag()]);
            for feature in self.weights {
                digest.update(feature[head].to_bits().to_le_bytes());
            }
            for feature in self.second_moments {
                digest.update(feature[head].to_bits().to_le_bytes());
            }
            digest.update(self.calibration_counts[head].to_le_bytes());
            digest.update(self.calibration_errors[head].to_bits().to_le_bytes());
        }
        digest.finalize().into()
    }
}

#[cfg(feature = "internal-experiments")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromotionDecision {
    Promote,
    Specialist,
    Reject,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LearningState {
    #[cfg(any(test, feature = "internal-experiments"))]
    generation: u64,
    last_training_examples: u64,
    online_comparisons: u8,
    champion: Option<FtrlModel>,
    #[cfg(any(test, feature = "internal-experiments"))]
    predecessor: Option<FtrlModel>,
    #[cfg(any(test, feature = "internal-experiments"))]
    predecessor_is_bootstrap: bool,
    #[cfg(any(test, feature = "internal-experiments"))]
    specialists: Vec<FtrlModel>,
    roles: BTreeMap<[u8; 32], CorpusRole>,
}

#[cfg(feature = "internal-experiments")]
pub(crate) struct PairedModelComparison {
    pub(crate) replay_claims: usize,
    pub(crate) selection_claims: usize,
    pub(crate) baseline_loss: [f32; HEAD_COUNT],
    pub(crate) structural_loss: [f32; HEAD_COUNT],
    pub(crate) balanced_structural_loss: [f32; HEAD_COUNT],
    pub(crate) baseline_training_cpu: std::time::Duration,
    pub(crate) structural_training_cpu: std::time::Duration,
    pub(crate) balanced_structural_training_cpu: std::time::Duration,
    pub(crate) baseline_revision: [u8; 32],
    pub(crate) structural_revision: [u8; 32],
    pub(crate) balanced_structural_revision: [u8; 32],
    pub(crate) model_bytes: usize,
    pub(crate) baseline_reproduces_champion: bool,
    pub(crate) structural_promotes: bool,
    pub(crate) ranking_budgets: [usize; 7],
    pub(crate) bootstrap_accepted_at_k: [usize; 7],
    pub(crate) baseline_accepted_at_k: [usize; 7],
    pub(crate) structural_accepted_at_k: [usize; 7],
    pub(crate) balanced_structural_accepted_at_k: [usize; 7],
    pub(crate) global_bootstrap_accepted_at_k: [usize; 7],
    pub(crate) global_baseline_accepted_at_k: [usize; 7],
    pub(crate) global_structural_accepted_at_k: [usize; 7],
    pub(crate) global_balanced_structural_accepted_at_k: [usize; 7],
    pub(crate) evaluated_at_k: [usize; 7],
    pub(crate) selection_accepted: usize,
}

impl LearningState {
    pub(crate) fn pinned_model(&self) -> Option<&FtrlModel> {
        self.champion.as_ref()
    }

    const fn online_comparison_budget() -> u32 {
        (MAX_SELECTION_USES - 1) as u32
    }

    pub(crate) fn corpus_is_valid(&self, attempts: &[([u8; 32], [u8; 32])]) -> bool {
        let Ok(watermark) = usize::try_from(self.last_training_examples) else {
            return false;
        };
        let Some(trained_prefix) = attempts.get(..watermark) else {
            return false;
        };
        let trained_claims = trained_prefix
            .iter()
            .map(|(_, corpus)| *corpus)
            .collect::<BTreeSet<_>>();
        let minimum_online_examples = if self.online_comparisons == 0 {
            0
        } else {
            (1..self.online_comparisons).fold(MIN_ONLINE_TRAINING_EXAMPLES, |value, _| {
                value.saturating_mul(ONLINE_TRAINING_GROWTH)
            })
        };
        watermark >= minimum_online_examples
            && self.roles.len() == trained_claims.len()
            && self.roles.keys().all(|key| trained_claims.contains(key))
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
        let train = |replay: &[&TrainingExample], balancing: Option<[[f32; 2]; HEAD_COUNT]>| {
            let started = cpu_time::ProcessTime::now();
            let mut model = FtrlModel::zero();
            for _ in 0..TRAINING_EPOCHS {
                for example in replay {
                    if let Some(balancing) = balancing {
                        let weights = std::array::from_fn(|head| {
                            balancing[head][usize::from(example.targets.0[head] > 0.0)]
                        });
                        model.update_weighted(example, weights);
                    } else {
                        model.update(example);
                    }
                }
            }
            (model, started.elapsed())
        };
        let (baseline_model, baseline_training_cpu) = train(&baseline_replay, None);
        let (structural_model, structural_training_cpu) = train(&structural_replay, None);
        let (balanced_structural_model, balanced_structural_training_cpu) = train(
            &structural_replay,
            Some(outcome_balancing(&structural_replay)),
        );
        let baseline_loss = losses(&baseline_model, &baseline_selection);
        let structural_loss = losses(&structural_model, &structural_selection);
        let balanced_structural_loss = losses(&balanced_structural_model, &structural_selection);
        let baseline_ranking = accepted_ranking(&baseline_model, &baseline);
        let structural_ranking = accepted_ranking(&structural_model, &structural);
        let balanced_structural_ranking = accepted_ranking(&balanced_structural_model, &structural);
        if !rankings_are_paired(
            &baseline_ranking,
            &structural_ranking,
            &balanced_structural_ranking,
        ) {
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
            balanced_structural_loss,
            baseline_training_cpu,
            structural_training_cpu,
            balanced_structural_training_cpu,
            baseline_revision: revision_digest(&baseline_model),
            structural_revision: revision_digest(&structural_model),
            balanced_structural_revision: revision_digest(&balanced_structural_model),
            model_bytes: baseline_model.encode().len(),
            baseline_reproduces_champion: self
                .champion
                .as_ref()
                .is_some_and(|champion| champion.encode() == baseline_model.encode()),
            structural_promotes: promotion_from_losses(baseline_loss, structural_loss)
                == PromotionDecision::Promote,
            ranking_budgets: RANKING_BUDGETS,
            bootstrap_accepted_at_k: baseline_ranking.bootstrap_accepted_at_k,
            baseline_accepted_at_k: baseline_ranking.accepted_at_k,
            structural_accepted_at_k: structural_ranking.accepted_at_k,
            balanced_structural_accepted_at_k: balanced_structural_ranking.accepted_at_k,
            global_bootstrap_accepted_at_k: baseline_ranking.global_bootstrap_accepted_at_k,
            global_baseline_accepted_at_k: baseline_ranking.global_accepted_at_k,
            global_structural_accepted_at_k: structural_ranking.global_accepted_at_k,
            global_balanced_structural_accepted_at_k: balanced_structural_ranking
                .global_accepted_at_k,
            evaluated_at_k: baseline_ranking.evaluated_at_k,
            selection_accepted: baseline_ranking.total_accepted,
        })
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut output = Vec::new();
        output.extend_from_slice(b"RFLS\x03");
        output.extend_from_slice(&self.generation.to_le_bytes());
        output.extend_from_slice(&self.last_training_examples.to_le_bytes());
        output.push(self.online_comparisons);
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
        if take(&mut input, 5)? != b"RFLS\x03" {
            return Err(());
        }
        let generation = read_u64(&mut input)?;
        let last_training_examples = read_u64(&mut input)?;
        let online_comparisons = take(&mut input, 1)?[0];
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
                    if uses >= MAX_SELECTION_USES {
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
            || u32::from(online_comparisons) > Self::online_comparison_budget()
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
            #[cfg(any(test, feature = "internal-experiments"))]
            generation,
            last_training_examples,
            online_comparisons,
            champion,
            #[cfg(any(test, feature = "internal-experiments"))]
            predecessor,
            #[cfg(any(test, feature = "internal-experiments"))]
            predecessor_is_bootstrap,
            #[cfg(any(test, feature = "internal-experiments"))]
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
    bootstrap_accepted_at_k: [usize; 7],
    accepted_at_k: [usize; 7],
    evaluated_at_k: [usize; 7],
    total_accepted: usize,
    global_bootstrap_accepted_at_k: [usize; 7],
    global_accepted_at_k: [usize; 7],
}

#[cfg(feature = "internal-experiments")]
fn rankings_are_paired(
    baseline: &AcceptedRanking,
    structural: &AcceptedRanking,
    balanced_structural: &AcceptedRanking,
) -> bool {
    [structural, balanced_structural]
        .into_iter()
        .all(|ranking| {
            baseline.evaluated_at_k == ranking.evaluated_at_k
                && baseline.total_accepted == ranking.total_accepted
                && baseline.bootstrap_accepted_at_k == ranking.bootstrap_accepted_at_k
                && baseline.global_bootstrap_accepted_at_k == ranking.global_bootstrap_accepted_at_k
        })
}

#[cfg(feature = "internal-experiments")]
fn accepted_ranking(model: &FtrlModel, examples: &[TrainingExample]) -> AcceptedRanking {
    let selected_examples = examples
        .iter()
        .filter(|example| matches!(example.role, CorpusRole::Selection { .. }))
        .collect::<Vec<_>>();
    let mut groups = BTreeMap::<[u8; 32], Vec<&TrainingExample>>::new();
    for example in &selected_examples {
        groups.entry(example.corpus_key).or_default().push(example);
    }
    let mut bootstrap_accepted_at_k = [0_usize; 7];
    let mut accepted_at_k = [0_usize; 7];
    let mut evaluated_at_k = [0_usize; 7];
    let mut total_accepted = 0_usize;
    for examples in groups.values() {
        let bootstrap = observed_policy_examples(None, examples);
        let operational = observed_policy_examples(Some(model), examples);
        total_accepted = total_accepted.saturating_add(
            examples
                .iter()
                .filter(|example| example.targets.0[4] > 0.0)
                .count(),
        );
        for (index, budget) in RANKING_BUDGETS.into_iter().enumerate() {
            let count = examples.len().min(budget);
            evaluated_at_k[index] = evaluated_at_k[index].saturating_add(count);
            bootstrap_accepted_at_k[index] = bootstrap_accepted_at_k[index].saturating_add(
                bootstrap[..count]
                    .iter()
                    .filter(|example| example.targets.0[4] > 0.0)
                    .count(),
            );
            accepted_at_k[index] = accepted_at_k[index].saturating_add(
                operational[..count]
                    .iter()
                    .filter(|example| example.targets.0[4] > 0.0)
                    .count(),
            );
        }
    }
    let global_bootstrap = observed_policy_examples(None, &selected_examples);
    let global_operational = observed_policy_examples(Some(model), &selected_examples);
    let mut global_bootstrap_accepted_at_k = [0_usize; 7];
    let mut global_accepted_at_k = [0_usize; 7];
    for (index, budget) in RANKING_BUDGETS.into_iter().enumerate() {
        let count = selected_examples.len().min(budget);
        global_bootstrap_accepted_at_k[index] = global_bootstrap[..count]
            .iter()
            .filter(|example| example.targets.0[4] > 0.0)
            .count();
        global_accepted_at_k[index] = global_operational[..count]
            .iter()
            .filter(|example| example.targets.0[4] > 0.0)
            .count();
    }
    AcceptedRanking {
        bootstrap_accepted_at_k,
        accepted_at_k,
        evaluated_at_k,
        total_accepted,
        global_bootstrap_accepted_at_k,
        global_accepted_at_k,
    }
}

#[cfg(any(test, feature = "internal-experiments"))]
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

    #[cfg(test)]
    pub(crate) fn deterministic_conversion_fixture() -> Self {
        let mut encoded = Vec::new();
        encoded.extend_from_slice(b"RFLM\x02");
        for revision in [FEATURE_REVISION, TARGET_REVISION, CALIBRATION_REVISION] {
            encoded.extend_from_slice(&revision.to_le_bytes());
        }
        for state in 0..2 {
            for head in 0..HEAD_COUNT {
                for feature in 0..FEATURE_COUNT {
                    let ordinal = u16::try_from((head + 1) * (feature + 1))
                        .expect("the fixed fixture ordinal fits u16");
                    let value = if state == 0 {
                        let magnitude = f32::from(ordinal) / 4096.0;
                        if (head + feature).is_multiple_of(2) {
                            magnitude
                        } else {
                            -magnitude
                        }
                    } else {
                        f32::from(ordinal) / 2048.0
                    };
                    encoded.extend_from_slice(&value.to_bits().to_le_bytes());
                }
            }
        }
        for head in 0..HEAD_COUNT {
            let count = if head == 5 {
                0
            } else {
                u64::try_from(head + 1).expect("the fixed head index fits u64") * 17
            };
            encoded.extend_from_slice(&count.to_le_bytes());
        }
        for head in 0..HEAD_COUNT {
            let error = if head == 5 {
                0.0
            } else {
                f32::from(u16::try_from(head + 1).expect("the fixed head index fits u16")) / 32.0
            };
            encoded.extend_from_slice(&error.to_bits().to_le_bytes());
        }
        Self::decode(&encoded).expect("the deterministic conversion fixture is a valid FTRL model")
    }

    pub(crate) const fn conversion_view(&self) -> FtrlConversionView<'_> {
        FtrlConversionView {
            weights: &self.weights,
            second_moments: &self.n,
            calibration_counts: &self.calibration_count,
            calibration_errors: &self.calibration_error,
        }
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    fn predict(&self, features: Features) -> Targets {
        let mut linear = [0.0_f32; HEAD_COUNT];
        for (feature_index, feature) in features.0.iter().copied().enumerate() {
            for (head, value) in linear.iter_mut().enumerate() {
                *value += self.weights[feature_index][head] * feature;
            }
        }
        sigmoid_heads(linear)
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    fn predict_active(&self, features: Features, mut active_features: u32) -> Targets {
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

    #[cfg(any(test, feature = "internal-experiments"))]
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

    #[cfg(any(test, feature = "internal-experiments"))]
    pub(crate) fn update(&mut self, example: &TrainingExample) {
        self.update_weighted(example, [1.0; HEAD_COUNT]);
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    fn update_weighted(&mut self, example: &TrainingExample, head_weights: [f32; HEAD_COUNT]) {
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
            let gradients: [f32; HEAD_COUNT] =
                std::array::from_fn(|head| errors[head] * feature * head_weights[head]);
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
    let weight = -z / ((BETA + sqrt_n) / ALPHA + L2);
    if weight == 0.0 { 0.0 } else { weight }
}

#[cfg(any(test, feature = "internal-experiments"))]
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
    let mut behavior_ranks = HashMap::<[u8; 32], u32>::new();
    attempts
        .iter()
        .enumerate()
        .map(|(sequence, attempt)| {
            let behavior_rank = behavior_ranks.entry(attempt.claim).or_default();
            let current_behavior_rank = *behavior_rank;
            *behavior_rank = behavior_rank.saturating_add(1);
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
                #[cfg(feature = "internal-experiments")]
                corpus_key: attempt.claim,
                behavior_rank: current_behavior_rank,
                behavior_sequence: u32::try_from(sequence).unwrap_or(u32::MAX),
                allocation_queue: attempt.allocation_queue,
                bootstrap_rank: attempt.bootstrap_rank,
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
                #[cfg(feature = "internal-experiments")]
                role: assign_role(attempt.claim),
            }
        })
        .collect()
}

#[cfg(any(test, feature = "internal-experiments"))]
fn active_feature_mask(features: Features) -> u32 {
    features
        .0
        .iter()
        .enumerate()
        .fold(0_u32, |mask, (index, feature)| {
            if *feature == 0.0 {
                mask
            } else {
                mask | (1_u32 << index)
            }
        })
}

#[cfg(feature = "internal-experiments")]
pub(crate) fn assign_role(key: [u8; 32]) -> CorpusRole {
    if key[0].is_multiple_of(5) {
        CorpusRole::Selection { uses: 0 }
    } else {
        CorpusRole::Replay
    }
}

#[cfg(feature = "internal-experiments")]
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

#[cfg(any(test, feature = "internal-experiments"))]
fn observed_policy_examples<'a>(
    model: Option<&FtrlModel>,
    examples: &[&'a TrainingExample],
) -> Vec<&'a TrainingExample> {
    let behavior_key = |example: &&TrainingExample| {
        (
            example.behavior_sequence,
            example.behavior_rank,
            example.key,
        )
    };
    let mut origin = examples
        .iter()
        .copied()
        .filter(|example| example.allocation_queue == AllocationQueue::ProtectedOrigin)
        .collect::<Vec<_>>();
    let mut derived = examples
        .iter()
        .copied()
        .filter(|example| example.allocation_queue == AllocationQueue::ProtectedDerived)
        .collect::<Vec<_>>();
    let unprotected = examples
        .iter()
        .copied()
        .filter(|example| {
            matches!(
                example.allocation_queue,
                AllocationQueue::Learned | AllocationQueue::Bootstrap
            )
        })
        .collect::<Vec<_>>();
    origin.sort_unstable_by_key(behavior_key);
    derived.sort_unstable_by_key(behavior_key);
    let mut bootstrap = (0..unprotected.len()).collect::<Vec<_>>();
    bootstrap.sort_unstable_by_key(|index| {
        let example = unprotected[*index];
        (
            example.bootstrap_rank,
            example.behavior_sequence,
            example.key,
        )
    });
    let learned = model.map(|model| {
        let forecasts = unprotected
            .iter()
            .map(|example| model.forecast(example.features))
            .collect::<Vec<_>>();
        let mut learned = (0..unprotected.len()).collect::<Vec<_>>();
        learned.sort_unstable_by(|left, right| {
            compare_forecasts(forecasts[*left], forecasts[*right])
                .then_with(|| {
                    unprotected[*left]
                        .bootstrap_rank
                        .cmp(&unprotected[*right].bootstrap_rank)
                })
                .then_with(|| unprotected[*left].key.cmp(&unprotected[*right].key))
        });
        learned
    });
    operational_ranked_selections(
        origin.len(),
        derived.len(),
        &bootstrap,
        learned.as_deref(),
        examples.len(),
    )
    .into_iter()
    .map(|selection| match selection.partition {
        OperationalPartition::ProtectedOrigin => origin[selection.index],
        OperationalPartition::ProtectedDerived => derived[selection.index],
        OperationalPartition::Unprotected => unprotected[selection.index],
    })
    .collect()
}

#[cfg(feature = "internal-experiments")]
fn outcome_balancing(examples: &[&TrainingExample]) -> [[f32; 2]; HEAD_COUNT] {
    let mut counts = [[0_u32; 2]; HEAD_COUNT];
    for example in examples {
        for (head, target) in example.targets.0.iter().enumerate() {
            let class = usize::from(*target > 0.0);
            counts[head][class] = counts[head][class].saturating_add(1);
        }
    }
    std::array::from_fn(|head| {
        let total = counts[head][0].saturating_add(counts[head][1]);
        if counts[head].contains(&0) || total == 0 {
            [1.0; 2]
        } else {
            std::array::from_fn(|class| {
                let denominator = counts[head][class].saturating_mul(2);
                (bounded_u32(total) / bounded_u32(denominator)).min(32.0)
            })
        }
    })
}

#[cfg(feature = "internal-experiments")]
fn bounded_u32(value: u32) -> f32 {
    f32::from(u16::try_from(value).unwrap_or(u16::MAX))
}

#[cfg(feature = "internal-experiments")]
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

#[cfg(feature = "internal-experiments")]
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

#[cfg(any(test, feature = "internal-experiments"))]
fn sigmoid(value: f32) -> f32 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exponential = value.exp();
        exponential / (1.0 + exponential)
    }
}

#[cfg(any(test, feature = "internal-experiments"))]
fn sigmoid_heads(linear: [f32; HEAD_COUNT]) -> Targets {
    let mut predictions = [0.0_f32; HEAD_COUNT];
    for head in 0..HEAD_COUNT {
        predictions[head] = (0..head)
            .find(|previous| linear[*previous].to_bits() == linear[head].to_bits())
            .map_or_else(|| sigmoid(linear[head]), |previous| predictions[previous]);
    }
    Targets(predictions)
}

#[cfg(any(test, feature = "internal-experiments"))]
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

#[cfg(any(test, feature = "internal-experiments"))]
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

    fn forecast_from_conversion(
        conversion: FtrlConversionView<'_>,
        features: Features,
    ) -> PotentialForecast {
        let mut linear = [0.0_f32; HEAD_COUNT];
        let mut support = [0.0_f32; HEAD_COUNT];
        for (feature_index, feature) in features.0.iter().copied().enumerate() {
            for head in 0..HEAD_COUNT {
                linear[head] += conversion.weights()[feature_index][head] * feature;
                support[head] +=
                    conversion.second_moments()[feature_index][head] * feature * feature;
            }
        }
        let estimates = sigmoid_heads(linear);
        PotentialForecast(std::array::from_fn(|head| {
            let epistemic = (1.0 / (1.0 + support[head].max(0.0))).sqrt();
            let calibration_error = if conversion.calibration_counts()[head] == 0 {
                1.0
            } else {
                conversion.calibration_errors()[head]
            };
            Forecast {
                estimate: estimates.0[head],
                calibration_error,
                uncertainty: epistemic.max(calibration_error).clamp(0.0, 1.0),
            }
        }))
    }

    fn assert_forecast_bits_eq(left: PotentialForecast, right: PotentialForecast) {
        for (left, right) in left.0.into_iter().zip(right.0) {
            assert_eq!(left.estimate.to_bits(), right.estimate.to_bits());
            assert_eq!(
                left.calibration_error.to_bits(),
                right.calibration_error.to_bits()
            );
            assert_eq!(left.uncertainty.to_bits(), right.uncertainty.to_bits());
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
        #[cfg(not(feature = "internal-experiments"))]
        let _ = role;
        let mut targets = [0.0; HEAD_COUNT];
        targets[0] = f32::from(accepted);
        targets[6] = f32::from(!accepted);
        TrainingExample {
            key: [key; 32],
            #[cfg(feature = "internal-experiments")]
            corpus_key: [key; 32],
            behavior_rank: 0,
            behavior_sequence: u32::from(key),
            allocation_queue: AllocationQueue::Bootstrap,
            bootstrap_rank: u32::from(key),
            features: features(bucket),
            active_features: active_feature_mask(features(bucket)),
            targets: Targets(targets),
            #[cfg(feature = "internal-experiments")]
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
        incompatible[5..9].copy_from_slice(&FEATURE_REVISION.saturating_add(1).to_le_bytes());
        assert!(FtrlModel::decode(&incompatible).is_err());
    }

    #[test]
    fn zero_ftrl_preserves_the_bootstrap_candidate_prefix() {
        let mut examples = (0..8_u8)
            .map(|key| example(key, usize::from(key % 2), key % 3 == 0, CorpusRole::Replay))
            .collect::<Vec<_>>();
        examples[0].allocation_queue = AllocationQueue::ProtectedOrigin;
        examples[1].allocation_queue = AllocationQueue::ProtectedOrigin;
        examples[2].allocation_queue = AllocationQueue::ProtectedDerived;
        examples[3].allocation_queue = AllocationQueue::ProtectedDerived;
        let frozen = examples.iter().collect::<Vec<_>>();
        let bootstrap = observed_policy_examples(None, &frozen)
            .into_iter()
            .map(|example| example.key)
            .collect::<Vec<_>>();
        let zero = observed_policy_examples(Some(&FtrlModel::zero()), &frozen)
            .into_iter()
            .map(|example| example.key)
            .collect::<Vec<_>>();

        assert_eq!(
            zero, bootstrap,
            "zero information preserves Candidate order; the Policy module separately locks its distinct cooperative queue attribution"
        );
    }

    #[test]
    fn sparse_training_mask_covers_every_model_feature() {
        let mut features = Features([0.0; FEATURE_COUNT]);
        features.0[FEATURE_COUNT - 1] = 1.0;

        assert_eq!(active_feature_mask(features).count_ones(), 1);
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
    fn conversion_view_preserves_every_semantic_forecast_bit_exactly() {
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
        let mut model = FtrlModel::zero();
        for _ in 0..5 {
            for sample in &examples {
                model.update(sample);
            }
        }
        let conversion = model.conversion_view();
        assert_eq!(
            FtrlConversionView::semantic_heads(),
            &[
                LegacyFtrlHead::ImmediateImprovement,
                LegacyFtrlHead::UsefulDescendants,
                LegacyFtrlHead::CrossGoalLeverage,
                LegacyFtrlHead::CompressionValue,
                LegacyFtrlHead::KernelAcceptance,
                LegacyFtrlHead::VerificationCost,
                LegacyFtrlHead::DeadEndRisk,
            ]
        );
        let adversarial = [
            Features([0.0; FEATURE_COUNT]),
            Features(std::array::from_fn(|index| {
                if index.is_multiple_of(2) { -0.0 } else { 0.0 }
            })),
            Features(std::array::from_fn(|index| {
                let magnitude = f32::from_bits(
                    1_u32.saturating_add(u32::try_from(index).expect("feature index fits u32")),
                );
                if index.is_multiple_of(2) {
                    magnitude
                } else {
                    -magnitude
                }
            })),
            Features(std::array::from_fn(|index| {
                let magnitude = (f32::from(u16::try_from(index).expect("feature index fits u16"))
                    + 1.0)
                    * 1_000_000.0;
                if index.is_multiple_of(3) {
                    -magnitude
                } else {
                    magnitude
                }
            })),
            Features(std::array::from_fn(|index| {
                const VALUES: [f32; 8] = [
                    f32::MIN_POSITIVE,
                    -f32::MIN_POSITIVE,
                    1.0,
                    -1.0,
                    core::f32::consts::PI,
                    -core::f32::consts::E,
                    65_504.0,
                    -65_504.0,
                ];
                VALUES[index % VALUES.len()]
            })),
        ];

        for features in adversarial {
            assert_forecast_bits_eq(
                model.forecast(features),
                forecast_from_conversion(conversion, features),
            );
        }
    }

    #[test]
    fn conversion_view_has_a_stable_inference_content_identity() {
        let examples = (0..16_u8)
            .map(|key| {
                example(
                    key,
                    usize::from(key % 8),
                    key.is_multiple_of(3),
                    CorpusRole::Replay,
                )
            })
            .collect::<Vec<_>>();
        let mut model = FtrlModel::zero();
        for sample in &examples {
            model.update(sample);
        }

        let identity = model.conversion_view().content_identity();
        let restored = FtrlModel::decode(&model.encode()).unwrap();
        let restored_conversion = restored.conversion_view();
        for feature in 0..FEATURE_COUNT {
            for head in 0..HEAD_COUNT {
                assert_eq!(
                    restored_conversion.weights()[feature][head].to_bits(),
                    model.conversion_view().weights()[feature][head].to_bits(),
                    "weight mismatch at feature {feature}, head {head}"
                );
                assert_eq!(
                    restored_conversion.second_moments()[feature][head].to_bits(),
                    model.conversion_view().second_moments()[feature][head].to_bits(),
                    "second-moment mismatch at feature {feature}, head {head}"
                );
            }
        }

        assert_eq!(
            identity,
            [
                239, 6, 195, 225, 233, 76, 126, 141, 74, 247, 124, 216, 72, 244, 173, 132, 174,
                228, 57, 252, 135, 128, 71, 68, 103, 254, 253, 193, 208, 209, 35, 162,
            ]
        );
        assert_eq!(restored_conversion.content_identity(), identity);
        model.update(&examples[0]);
        assert_ne!(model.conversion_view().content_identity(), identity);
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
            allocation_queue: AllocationQueue::Bootstrap,
            bootstrap_rank: 0,
        };
        let child = AttemptObservation {
            id: [2; 32],
            artifact: [12; 32],
            claim: parent.claim,
            parent: parent.artifact,
            features: features(0),
            verdict: VerdictTarget::Accepted,
            verification_cost: 1.0,
            allocation_queue: AllocationQueue::Bootstrap,
            bootstrap_rank: 1,
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
    fn learning_state_rejects_corpus_overlap_and_invalid_revision_ancestry() {
        let mut roles = BTreeMap::new();
        roles.insert([7; 32], CorpusRole::Replay);
        let mut state = LearningState {
            last_training_examples: 1,
            roles,
            ..LearningState::default()
        };
        assert!(state.corpus_is_valid(&[([1; 32], [7; 32])]));
        assert!(!state.corpus_is_valid(&[([1; 32], [9; 32])]));
        state.last_training_examples = 2;
        assert!(!state.corpus_is_valid(&[([1; 32], [7; 32])]));
        state.last_training_examples = 1;
        state.online_comparisons = 1;
        assert!(!state.corpus_is_valid(&[([1; 32], [7; 32])]));
        state.online_comparisons = 0;
        state.roles.clear();
        assert!(!state.corpus_is_valid(&[([1; 32], [7; 32])]));
        state.roles.insert([7; 32], CorpusRole::Replay);
        let encoded = state.encode();
        let mut duplicate = encoded.clone();
        duplicate[32..40].copy_from_slice(&2_u64.to_le_bytes());
        duplicate.extend_from_slice(&encoded[40..]);
        assert!(LearningState::decode(&duplicate).is_err());

        let mut invalid_ancestry = LearningState::default().encode();
        invalid_ancestry[23] = 1;
        assert!(LearningState::decode(&invalid_ancestry).is_err());
    }

    #[test]
    fn learning_state_rejects_more_specialists_than_the_runtime_can_retain() {
        let specialist = FtrlModel::zero();
        let state = LearningState {
            generation: 1,
            last_training_examples: 0,
            online_comparisons: 0,
            champion: Some(FtrlModel::zero()),
            predecessor: None,
            predecessor_is_bootstrap: true,
            specialists: vec![specialist; MAX_SPECIALISTS + 1],
            roles: BTreeMap::new(),
        };

        assert!(LearningState::decode(&state.encode()).is_err());
    }
}
