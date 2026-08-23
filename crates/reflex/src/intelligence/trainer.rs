use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use sha2::{Digest, Sha256};

use super::arena::{MAXIMUM_FEATURES_PER_OPPORTUNITY, OpportunityKind};
use super::causal::{
    AttributionKind, CausalDelta, CausalEvidence, CausalLedger, CausalSubject, ConsequenceEdge,
    ConsequenceKind, DecisionId, InvestmentOutcome, InvestmentReceipt, InvestmentSettlement,
};
use super::core::InvestmentTag;
use super::ecology::{EcologyEdit, ModelEcology, SpecialistMandate, SpecialistRevision};
use super::forecast::ForecastAxis;
use super::model::{CompactModel, LinearHead};
use super::types::{FeatureSchemaId, IntelligenceError, RoleId, RoutingFamilyId};

const MINIMUM_EXAMPLES: usize = 4;
const MINIMUM_REPLAY_CASES: usize = 8;
const MINIMUM_SELECTION_CASES: usize = 8;
const MAXIMUM_EXAMPLES: usize = 256;
const MAXIMUM_EPOCHS: u16 = 64;
const SCAN_MULTIPLIER: usize = 4;
const NICHE_VIEWS_PER_EXAMPLE: usize = 2;
const MINIMUM_RETIREMENT_CAMPAIGNS: usize = 2;
const MAXIMUM_SELECTION_USES: u8 = 3;
const DEAD_END_MATURITY_EPOCHS: u64 = 8;
const MAXIMUM_PLAN_SPECIALISTS: usize = 2;
const OPERATIONAL_FORECAST_AXES: [ForecastAxis; 7] = [
    ForecastAxis::ImmediateImprovement,
    ForecastAxis::UsefulDescendants,
    ForecastAxis::CrossGoalLeverage,
    ForecastAxis::CompressionValue,
    ForecastAxis::KernelAcceptance,
    ForecastAxis::DeadEndRisk,
    ForecastAxis::VerificationCost,
];
type AxisMask = [bool; OPERATIONAL_FORECAST_AXES.len()];

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct NativeTrainingBudget {
    maximum_examples: usize,
    epochs: u16,
    learning_rate: f32,
}

impl NativeTrainingBudget {
    pub(crate) fn new(
        maximum_examples: usize,
        epochs: u16,
        learning_rate: f32,
    ) -> Result<Self, IntelligenceError> {
        if !(MINIMUM_EXAMPLES..=MAXIMUM_EXAMPLES).contains(&maximum_examples)
            || epochs == 0
            || epochs > MAXIMUM_EPOCHS
            || !learning_rate.is_finite()
            || !(0.0..=1.0).contains(&learning_rate)
            || learning_rate == 0.0
        {
            return Err(IntelligenceError::InvalidModel);
        }
        Ok(Self {
            maximum_examples,
            epochs,
            learning_rate,
        })
    }

    #[cfg(test)]
    pub(crate) const fn scratch_bytes(self) -> u64 {
        self.scratch_bytes_for_history(self.scan_limit())
    }

    pub(crate) const fn scratch_bytes_for_history(self, history_len: usize) -> u64 {
        let model_scratch = MAXIMUM_FEATURES_PER_OPPORTUNITY
            .saturating_mul(std::mem::size_of::<[f32; OPERATIONAL_FORECAST_AXES.len()]>());
        let scan_limit = self.scan_limit();
        let aligned_capacity = scan_limit.saturating_mul(NICHE_VIEWS_PER_EXAMPLE);
        let table_capacity = niche_table_capacity(aligned_capacity);
        let aligned_examples =
            aligned_capacity.saturating_mul(std::mem::size_of::<AlignedExample<'static>>());
        let niche_table = table_capacity.saturating_mul(std::mem::size_of::<NicheSlot>());
        let niche_order = aligned_capacity.saturating_mul(std::mem::size_of::<u16>());
        let lifecycle_groups = aligned_capacity.saturating_mul(
            std::mem::size_of::<NicheGroup>()
                .saturating_add(std::mem::size_of::<&'static SpecialistRevision>()),
        );
        let forecast_scratch = super::model::MAXIMUM_MODEL_HEADS
            .saturating_mul(std::mem::size_of::<super::forecast::Forecast>());
        let retained_pairs = scan_limit.saturating_mul(std::mem::size_of::<(
            &'static InvestmentReceipt,
            InvestmentSettlement,
        )>());
        let corpus_cases = scan_limit.saturating_mul(std::mem::size_of::<[u8; 32]>());
        let target_effects =
            scan_limit.saturating_mul(std::mem::size_of::<(DecisionId, TrainingEffects)>());
        let selection_uses = scan_limit.saturating_mul(std::mem::size_of::<([u8; 32], u8)>());
        let assignment_buckets = history_len
            .saturating_mul(NICHE_VIEWS_PER_EXAMPLE)
            .saturating_mul(2)
            .next_power_of_two();
        let corpus_assignments = assignment_buckets.saturating_mul(
            std::mem::size_of::<((NicheKey, [u8; 32]), bool)>()
                .saturating_add(std::mem::size_of::<usize>()),
        );
        let niche_ordinals = assignment_buckets.saturating_mul(
            std::mem::size_of::<(NicheKey, usize)>().saturating_add(std::mem::size_of::<usize>()),
        );
        (model_scratch as u64)
            .saturating_add(aligned_examples as u64)
            .saturating_add(niche_table as u64)
            .saturating_add(niche_order as u64)
            .saturating_add(lifecycle_groups as u64)
            .saturating_add(forecast_scratch as u64)
            .saturating_add(retained_pairs as u64)
            .saturating_add(corpus_cases as u64)
            .saturating_add(target_effects as u64)
            .saturating_add(selection_uses as u64)
            .saturating_add(corpus_assignments as u64)
            .saturating_add(niche_ordinals as u64)
    }

    pub(crate) const fn maximum_examples(self) -> usize {
        self.maximum_examples
    }

    pub(crate) fn maximum_output_bytes() -> u64 {
        let weights = MAXIMUM_FEATURES_PER_OPPORTUNITY
            .saturating_mul(std::mem::size_of::<f32>())
            .saturating_mul(OPERATIONAL_FORECAST_AXES.len())
            .saturating_mul(MAXIMUM_PLAN_SPECIALISTS);
        let heads = std::mem::size_of::<LinearHead>()
            .saturating_mul(OPERATIONAL_FORECAST_AXES.len())
            .saturating_mul(MAXIMUM_PLAN_SPECIALISTS);
        let mandate_members = std::mem::size_of::<ForecastAxis>()
            .saturating_mul(OPERATIONAL_FORECAST_AXES.len())
            .saturating_add(std::mem::size_of::<OpportunityKind>())
            .saturating_add(std::mem::size_of::<FeatureSchemaId>())
            .saturating_add(std::mem::size_of::<RoutingFamilyId>().saturating_mul(2))
            .saturating_mul(MAXIMUM_PLAN_SPECIALISTS);
        let selection_cases = MAXIMUM_EXAMPLES
            .saturating_mul(MAXIMUM_PLAN_SPECIALISTS)
            .saturating_mul(std::mem::size_of::<[u8; 32]>());
        u64::try_from(
            std::mem::size_of::<NativeEcologyPlan>()
                .saturating_add(std::mem::size_of::<EcologyEdit>().saturating_mul(2))
                .saturating_add(
                    std::mem::size_of::<SpecialistRevision>()
                        .saturating_mul(MAXIMUM_PLAN_SPECIALISTS),
                )
                .saturating_add(
                    std::mem::size_of::<CompactModel>().saturating_mul(MAXIMUM_PLAN_SPECIALISTS),
                )
                .saturating_add(heads)
                .saturating_add(weights)
                .saturating_add(mandate_members)
                .saturating_add(selection_cases)
                .saturating_add(
                    std::mem::size_of::<super::types::SpecialistRevisionId>().saturating_mul(2),
                ),
        )
        .unwrap_or(u64::MAX)
    }

    const fn scan_limit(self) -> usize {
        self.maximum_examples.saturating_mul(SCAN_MULTIPLIER)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct NativeEcologyPlan {
    pub(super) edits: Vec<EcologyEdit>,
    pub(super) selection_cases: Vec<[u8; 32]>,
}

impl NativeEcologyPlan {
    pub(super) fn resident_bytes(&self) -> usize {
        let edit_heap = self.edits.iter().fold(0_usize, |bytes, edit| {
            let nested = match edit {
                EcologyEdit::Spawn(specialist) => specialist.resident_bytes(),
                EcologyEdit::Retire(_) => 0,
                EcologyEdit::Split { children, .. } => children.iter().fold(
                    children
                        .capacity()
                        .saturating_mul(std::mem::size_of::<SpecialistRevision>()),
                    |bytes, child| bytes.saturating_add(child.resident_bytes()),
                ),
                EcologyEdit::Merge { parents, merged } => parents
                    .capacity()
                    .saturating_mul(std::mem::size_of::<super::types::SpecialistRevisionId>())
                    .saturating_add(merged.resident_bytes()),
                EcologyEdit::Distill { teachers, student } => teachers
                    .capacity()
                    .saturating_mul(std::mem::size_of::<super::types::SpecialistRevisionId>())
                    .saturating_add(student.resident_bytes()),
            };
            bytes.saturating_add(nested)
        });
        std::mem::size_of::<Self>()
            .saturating_add(
                self.edits
                    .capacity()
                    .saturating_mul(std::mem::size_of::<EcologyEdit>()),
            )
            .saturating_add(
                self.selection_cases
                    .capacity()
                    .saturating_mul(std::mem::size_of::<[u8; 32]>()),
            )
            .saturating_add(edit_heap)
    }

    pub(super) fn identity(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"reflex-native-ecology-plan-v1\0");
        digest.update((self.edits.len() as u64).to_le_bytes());
        for edit in &self.edits {
            match edit {
                EcologyEdit::Spawn(specialist) => {
                    digest.update([1]);
                    digest.update(specialist.id().0);
                }
                EcologyEdit::Retire(specialist) => {
                    digest.update([2]);
                    digest.update(specialist.0);
                }
                EcologyEdit::Split { parent, children } => {
                    digest.update([3]);
                    digest.update(parent.0);
                    digest.update((children.len() as u64).to_le_bytes());
                    for child in children {
                        digest.update(child.id().0);
                    }
                }
                EcologyEdit::Merge { parents, merged } => {
                    digest.update([4]);
                    digest.update((parents.len() as u64).to_le_bytes());
                    for parent in parents {
                        digest.update(parent.0);
                    }
                    digest.update(merged.id().0);
                }
                EcologyEdit::Distill { teachers, student } => {
                    digest.update([5]);
                    digest.update((teachers.len() as u64).to_le_bytes());
                    for teacher in teachers {
                        digest.update(teacher.0);
                    }
                    digest.update(student.id().0);
                }
            }
        }
        digest.update((self.selection_cases.len() as u64).to_le_bytes());
        for case in &self.selection_cases {
            digest.update(case);
        }
        digest.finalize().into()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct NicheKey {
    kind: OpportunityKind,
    schema: FeatureSchemaId,
    family: Option<RoutingFamilyId>,
    feature_count: u8,
    investment: InvestmentTag,
}

const NO_EXAMPLE: u16 = u16::MAX;

#[derive(Clone, Copy)]
struct AlignedExample<'a> {
    receipt: &'a InvestmentReceipt,
    targets: [Option<f32>; OPERATIONAL_FORECAST_AXES.len()],
    selection: bool,
    next: u16,
}

#[derive(Clone, Copy)]
struct NicheGroup {
    key: NicheKey,
    first: u16,
    last: u16,
    examples: u16,
    positives: u16,
    negatives: u16,
    selection_examples: u16,
}

#[derive(Clone, Copy, Default)]
struct TrainingEffects(u8);

impl TrainingEffects {
    const IMMEDIATE: u8 = 1;
    const DESCENDANTS: u8 = 1 << 1;
    const CROSS_GOAL: u8 = 1 << 2;
    const COMPRESSION: u8 = 1 << 3;
    const COST_SAVED: u8 = 1 << 4;

    const fn contains(self, effect: u8) -> bool {
        self.0 & effect != 0
    }

    fn observe(&mut self, consequence: ConsequenceEdge) {
        match consequence.kind() {
            ConsequenceKind::Admitted | ConsequenceKind::ParetoImprovement => {
                self.0 |= Self::IMMEDIATE;
            }
            ConsequenceKind::UsefulDescendant | ConsequenceKind::OperatorEnabled
                if consequence.attribution() != AttributionKind::Observed =>
            {
                self.0 |= Self::DESCENDANTS;
            }
            ConsequenceKind::CrossGoalUse
                if consequence.attribution() != AttributionKind::Observed =>
            {
                self.0 |= Self::CROSS_GOAL;
            }
            ConsequenceKind::Compression
                if consequence.attribution() != AttributionKind::Observed =>
            {
                self.0 |= Self::COMPRESSION;
            }
            ConsequenceKind::CpuSaved if consequence.attribution() != AttributionKind::Observed => {
                self.0 |= Self::COST_SAVED;
            }
            ConsequenceKind::UsefulDescendant
            | ConsequenceKind::CrossGoalUse
            | ConsequenceKind::Compression
            | ConsequenceKind::CpuSaved
            | ConsequenceKind::OperatorEnabled
            | ConsequenceKind::SelectionUse => {}
        }
    }

    fn observe_contrast(&mut self, axis: ForecastAxis, advantage: f32) {
        if advantage <= 0.0 {
            return;
        }
        self.0 |= match axis {
            ForecastAxis::ImmediateImprovement => Self::IMMEDIATE,
            ForecastAxis::UsefulDescendants => Self::DESCENDANTS,
            ForecastAxis::CrossGoalLeverage => Self::CROSS_GOAL,
            ForecastAxis::CompressionValue => Self::COMPRESSION,
            ForecastAxis::VerificationCost => Self::COST_SAVED,
            ForecastAxis::KernelAcceptance
            | ForecastAxis::InformationValue
            | ForecastAxis::Novelty
            | ForecastAxis::DeadEndRisk => 0,
        };
    }
}

#[derive(Clone, Copy)]
enum NicheSlot {
    Empty,
    Occupied(NicheGroup),
}

struct TrainingScratch<'a> {
    examples: Vec<AlignedExample<'a>>,
    niches: Vec<NicheSlot>,
    order: Vec<u16>,
    #[cfg(test)]
    operations: TrainingOperations,
}

struct TrainingContext {
    effects: Vec<(DecisionId, TrainingEffects)>,
    selection_uses: Vec<([u8; 32], u8)>,
    corpus_assignments: HashMap<(NicheKey, [u8; 32]), bool>,
    maximum_epoch: u64,
}

impl TrainingContext {
    #[expect(
        clippy::too_many_lines,
        reason = "one causal-frame scan binds settlements, mechanically induced consequences, terminal contrasts, and corpus roles without recomputation"
    )]
    fn build<'a>(
        retained: &[(&InvestmentReceipt, InvestmentSettlement)],
        assignment_history: impl Iterator<Item = (&'a InvestmentReceipt, InvestmentSettlement)>,
        evidence: &impl CausalEvidence,
    ) -> Result<Self, IntelligenceError> {
        let mut cases = Vec::new();
        cases
            .try_reserve_exact(retained.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        for case in retained
            .iter()
            .rev()
            .map(|(receipt, _)| receipt.corpus_key())
        {
            if !cases.contains(&case) {
                cases.push(case);
            }
        }
        let mut effects = Vec::new();
        effects
            .try_reserve_exact(retained.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        effects.extend(
            retained
                .iter()
                .map(|(receipt, _)| (receipt.decision(), TrainingEffects::default())),
        );
        effects.sort_unstable_by_key(|(decision, _)| *decision);
        effects.dedup_by_key(|(decision, _)| *decision);
        let mut selection_uses = Vec::new();
        selection_uses
            .try_reserve_exact(cases.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        selection_uses.extend(cases.iter().copied().map(|case| (case, 0_u8)));
        selection_uses.sort_unstable_by_key(|(case, _)| *case);
        let assignment_capacity = assignment_history
            .size_hint()
            .1
            .ok_or(IntelligenceError::CapacityExceeded)?
            .saturating_mul(NICHE_VIEWS_PER_EXAMPLE);
        let mut corpus_assignments = HashMap::new();
        corpus_assignments
            .try_reserve(assignment_capacity)
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        let mut niche_ordinals = HashMap::<NicheKey, usize>::new();
        niche_ordinals
            .try_reserve(assignment_capacity)
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        for (receipt, _) in assignment_history {
            let Some(exact_niche) = niche_key(receipt) else {
                continue;
            };
            let corpus = receipt.corpus_key();
            let generalist_niche = NicheKey {
                family: None,
                ..exact_niche
            };
            for niche in [generalist_niche, exact_niche] {
                if corpus_assignments.contains_key(&(niche, corpus)) {
                    continue;
                }
                let position = *niche_ordinals.get(&niche).unwrap_or(&0);
                corpus_assignments.insert((niche, corpus), corpus_role_is_selection(position));
                niche_ordinals.insert(niche, position.saturating_add(1));
            }
        }
        for consequence in evidence
            .consequence_slices()
            .0
            .iter()
            .chain(evidence.consequence_slices().1)
            .copied()
        {
            if consequence.kind() == ConsequenceKind::SelectionUse
                && let CausalSubject::Artifact(subject) = consequence.subject()
            {
                if let Ok(index) =
                    selection_uses.binary_search_by_key(&subject.identity(), |(case, _)| *case)
                {
                    selection_uses[index].1 = selection_uses[index].1.saturating_add(1);
                }
                continue;
            }
            let CausalSubject::Decision(decision) = consequence.subject() else {
                continue;
            };
            if let Ok(index) = effects.binary_search_by_key(&decision, |(decision, _)| *decision) {
                effects[index].1.observe(consequence);
            }
        }
        for contrast in evidence.contrasts().iter().copied() {
            for (receipt, _) in retained.iter().copied().filter(|(receipt, _)| {
                receipt.opportunity_identity() == contrast.subject().identity()
            }) {
                if let Ok(index) =
                    effects.binary_search_by_key(&receipt.decision(), |(decision, _)| *decision)
                {
                    effects[index]
                        .1
                        .observe_contrast(contrast.axis(), contrast.advantage());
                }
            }
        }
        Ok(Self {
            effects,
            selection_uses,
            corpus_assignments,
            maximum_epoch: retained
                .iter()
                .map(|(receipt, _)| receipt.epoch())
                .max()
                .unwrap_or_default(),
        })
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct TrainingOperations {
    aligned_pairs: usize,
    outcome_reads: usize,
}

impl<'a> TrainingScratch<'a> {
    #[cfg(test)]
    fn build(
        experience: &'a CausalLedger,
        budget: NativeTrainingBudget,
    ) -> Result<Self, IntelligenceError> {
        Self::build_aligned(
            experience.aligned().rev(),
            experience.aligned(),
            experience,
            budget,
        )
    }

    fn build_staged(
        experience: &'a CausalLedger,
        staged: &'a CausalDelta,
        budget: NativeTrainingBudget,
    ) -> Result<Self, IntelligenceError> {
        let evidence = experience.overlay(staged);
        Self::build_aligned(
            staged
                .recent_aligned()
                .rev()
                .chain(experience.aligned().rev()),
            experience.aligned().chain(staged.recent_aligned()),
            &evidence,
            budget,
        )
    }

    fn build_aligned(
        aligned: impl Iterator<Item = (&'a InvestmentReceipt, InvestmentSettlement)>,
        assignment_history: impl Iterator<Item = (&'a InvestmentReceipt, InvestmentSettlement)>,
        evidence: &impl CausalEvidence,
        budget: NativeTrainingBudget,
    ) -> Result<Self, IntelligenceError> {
        let scan_limit = budget.scan_limit();
        let mut scratch = Self::with_scan_capacity(scan_limit)?;
        let mut retained = Vec::new();
        retained
            .try_reserve_exact(scan_limit)
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        retained.extend(aligned.take(scan_limit));
        let context = TrainingContext::build(&retained, assignment_history, evidence)?;
        for (receipt, settlement) in retained {
            #[cfg(test)]
            {
                scratch.operations.aligned_pairs += 1;
            }
            let Some(exact_key) = niche_key(receipt) else {
                continue;
            };
            #[cfg(test)]
            {
                scratch.operations.outcome_reads += 1;
            }
            let effect = context
                .effects
                .binary_search_by_key(&receipt.decision(), |(decision, _)| *decision)
                .ok()
                .map(|index| context.effects[index].1)
                .unwrap_or_default();
            let targets = operational_targets(receipt, settlement, effect, context.maximum_epoch);
            if targets.iter().all(Option::is_none) {
                continue;
            }
            // First occurrence order is restart-complete ledger order. The exact
            // opening cohort reserves eight Replay then eight Selection cases;
            // subsequent cases use one Selection slot in five.
            let generalist_key = NicheKey {
                family: None,
                ..exact_key
            };
            for key in [generalist_key, exact_key] {
                let initially_selected = context
                    .corpus_assignments
                    .get(&(key, receipt.corpus_key()))
                    .copied()
                    .ok_or(IntelligenceError::InvalidModel)?;
                let selection = initially_selected
                    && context
                        .selection_uses
                        .binary_search_by_key(&receipt.corpus_key(), |(case, _)| *case)
                        .ok()
                        .map(|index| context.selection_uses[index].1)
                        .unwrap_or_default()
                        < MAXIMUM_SELECTION_USES;
                let niche = scratch.find_or_insert_niche(key)?;
                scratch.push_example(
                    niche,
                    receipt,
                    targets,
                    selection,
                    budget.maximum_examples,
                )?;
            }
        }
        Ok(scratch)
    }

    fn with_scan_capacity(scan_limit: usize) -> Result<Self, IntelligenceError> {
        let aligned_capacity = scan_limit
            .checked_mul(NICHE_VIEWS_PER_EXAMPLE)
            .ok_or(IntelligenceError::ResourceOverflow)?;
        let mut examples = Vec::new();
        examples
            .try_reserve_exact(aligned_capacity)
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        let mut niches = Vec::new();
        niches
            .try_reserve_exact(niche_table_capacity(aligned_capacity))
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        niches.resize(niche_table_capacity(aligned_capacity), NicheSlot::Empty);
        let mut order = Vec::new();
        order
            .try_reserve_exact(aligned_capacity)
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        Ok(Self {
            examples,
            niches,
            order,
            #[cfg(test)]
            operations: TrainingOperations::default(),
        })
    }

    fn find_or_insert_niche(&mut self, key: NicheKey) -> Result<usize, IntelligenceError> {
        let mask = self.niches.len().saturating_sub(1);
        let mut slot = niche_hash(key) & mask;
        for _ in 0..self.niches.len() {
            match self.niches[slot] {
                NicheSlot::Empty => {
                    let order_slot =
                        u16::try_from(slot).map_err(|_| IntelligenceError::CapacityExceeded)?;
                    self.niches[slot] = NicheSlot::Occupied(NicheGroup {
                        key,
                        first: NO_EXAMPLE,
                        last: NO_EXAMPLE,
                        examples: 0,
                        positives: 0,
                        negatives: 0,
                        selection_examples: 0,
                    });
                    self.order.push(order_slot);
                    return Ok(slot);
                }
                NicheSlot::Occupied(group) if group.key == key => return Ok(slot),
                NicheSlot::Occupied(_) => slot = slot.wrapping_add(1) & mask,
            }
        }
        Err(IntelligenceError::CapacityExceeded)
    }

    fn push_example(
        &mut self,
        niche: usize,
        receipt: &'a InvestmentReceipt,
        targets: [Option<f32>; OPERATIONAL_FORECAST_AXES.len()],
        selection: bool,
        maximum_examples: usize,
    ) -> Result<(), IntelligenceError> {
        let NicheSlot::Occupied(group) = self.niches[niche] else {
            return Err(IntelligenceError::InvalidModel);
        };
        let role_examples = if selection {
            group.selection_examples
        } else {
            group.examples
        };
        if usize::from(role_examples) == maximum_examples {
            return Ok(());
        }
        let index =
            u16::try_from(self.examples.len()).map_err(|_| IntelligenceError::CapacityExceeded)?;
        if group.last != NO_EXAMPLE {
            self.examples[usize::from(group.last)].next = index;
        }
        self.examples.push(AlignedExample {
            receipt,
            targets,
            selection,
            next: NO_EXAMPLE,
        });
        let NicheSlot::Occupied(group) = &mut self.niches[niche] else {
            return Err(IntelligenceError::InvalidModel);
        };
        if group.first == NO_EXAMPLE {
            group.first = index;
        }
        group.last = index;
        if selection {
            group.selection_examples = group.selection_examples.saturating_add(1);
        } else {
            group.examples = group.examples.saturating_add(1);
            if let Some(correct) = targets[4] {
                if correct >= 0.5 {
                    group.positives = group.positives.saturating_add(1);
                } else {
                    group.negatives = group.negatives.saturating_add(1);
                }
            }
        }
        Ok(())
    }

    fn trainable_niche(&self, ecology: &ModelEcology) -> Option<NicheGroup> {
        self.order.iter().find_map(|slot| {
            let NicheSlot::Occupied(group) = self.niches[usize::from(*slot)] else {
                return None;
            };
            (!ecology.has_active_role(niche_role(group.key))
                && self
                    .supported_axes(&[group])
                    .iter()
                    .any(|supported| *supported)
                && self.selection_has_target(group, self.supported_axes(&[group])))
            .then_some(group)
        })
    }

    fn niche_examples(&self, group: NicheGroup) -> NicheExamples<'_, 'a> {
        NicheExamples {
            scratch: self,
            next: group.first,
        }
    }

    fn replay_examples(&self, group: NicheGroup) -> impl Iterator<Item = AlignedExample<'a>> + '_ {
        self.niche_examples(group)
            .filter(|example| !example.selection)
    }

    fn selection_examples(
        &self,
        group: NicheGroup,
    ) -> impl Iterator<Item = AlignedExample<'a>> + '_ {
        self.niche_examples(group)
            .filter(|example| example.selection)
    }

    fn selection_cases(&self, niches: &[NicheGroup]) -> Result<Vec<[u8; 32]>, IntelligenceError> {
        let capacity = niches.iter().fold(0_usize, |count, niche| {
            count.saturating_add(usize::from(niche.selection_examples))
        });
        let mut cases = Vec::new();
        cases
            .try_reserve_exact(capacity)
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        cases.extend(
            niches
                .iter()
                .flat_map(|niche| self.selection_examples(*niche))
                .map(|example| example.receipt.corpus_key()),
        );
        cases.sort_unstable();
        cases.dedup();
        Ok(cases)
    }

    fn supported_axes(&self, niches: &[NicheGroup]) -> AxisMask {
        std::array::from_fn(|axis_index| {
            let mut count = 0_usize;
            let mut minimum = f32::INFINITY;
            let mut maximum = f32::NEG_INFINITY;
            for target in niches
                .iter()
                .flat_map(|niche| self.replay_examples(*niche))
                .filter_map(|example| example.targets[axis_index])
            {
                count = count.saturating_add(1);
                minimum = minimum.min(target);
                maximum = maximum.max(target);
            }
            count >= MINIMUM_EXAMPLES && minimum < maximum
        })
    }

    fn selection_has_target(&self, niche: NicheGroup, axes: AxisMask) -> bool {
        usize::from(niche.examples) >= MINIMUM_REPLAY_CASES
            && usize::from(niche.selection_examples) >= MINIMUM_SELECTION_CASES
            && self.selection_examples(niche).any(|example| {
                axes.iter()
                    .zip(example.targets)
                    .any(|(supported, target)| *supported && target.is_some())
            })
    }
}

fn corpus_role_is_selection(ordinal: usize) -> bool {
    let opening = MINIMUM_REPLAY_CASES.saturating_add(MINIMUM_SELECTION_CASES);
    (ordinal >= MINIMUM_REPLAY_CASES && ordinal < opening)
        || (ordinal >= opening && (ordinal - opening).is_multiple_of(5))
}

struct NicheExamples<'scratch, 'experience> {
    scratch: &'scratch TrainingScratch<'experience>,
    next: u16,
}

impl<'experience> Iterator for NicheExamples<'_, 'experience> {
    type Item = AlignedExample<'experience>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == NO_EXAMPLE {
            return None;
        }
        let example = self.scratch.examples[usize::from(self.next)];
        self.next = example.next;
        Some(example)
    }
}

const fn niche_table_capacity(scan_limit: usize) -> usize {
    scan_limit.saturating_mul(2).next_power_of_two()
}

fn niche_hash(key: NicheKey) -> usize {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    let hash = hasher.finish();
    usize::try_from(hash)
        .unwrap_or_else(|_| usize::try_from(hash & u64::from(u32::MAX)).unwrap_or_default())
}

#[cfg(test)]
pub(super) fn propose(
    experience: &CausalLedger,
    ecology: &ModelEcology,
    budget: NativeTrainingBudget,
) -> Result<Option<NativeEcologyPlan>, IntelligenceError> {
    let scratch = TrainingScratch::build(experience, budget)?;
    propose_from_scratch(&scratch, ecology, budget, experience)
}

pub(super) fn propose_staged(
    experience: &CausalLedger,
    staged: &CausalDelta,
    ecology: &ModelEcology,
    budget: NativeTrainingBudget,
) -> Result<Option<NativeEcologyPlan>, IntelligenceError> {
    let scratch = TrainingScratch::build_staged(experience, staged, budget)?;
    let evidence = experience.overlay(staged);
    propose_from_scratch(&scratch, ecology, budget, &evidence)
}

fn propose_from_scratch(
    scratch: &TrainingScratch<'_>,
    ecology: &ModelEcology,
    budget: NativeTrainingBudget,
    evidence: &impl CausalEvidence,
) -> Result<Option<NativeEcologyPlan>, IntelligenceError> {
    if let Some(specialist) = retirement_candidate(ecology, evidence) {
        return Ok(Some(NativeEcologyPlan {
            edits: vec![EcologyEdit::Retire(specialist.id())],
            selection_cases: Vec::new(),
        }));
    }
    if let Some(plan) = propose_split(scratch, ecology, budget)? {
        return Ok(Some(plan));
    }
    if let Some(plan) = propose_merge(scratch, ecology, budget)? {
        return Ok(Some(plan));
    }
    if let Some(niche) = scratch.trainable_niche(ecology) {
        let role = niche_role(niche.key);
        let specialist = train_specialist(scratch, niche, role, budget)?;
        if empirical_loss(specialist.model(), scratch.selection_examples(niche))?
            >= bootstrap_loss(scratch, niche)
        {
            return Ok(Some(NativeEcologyPlan {
                edits: Vec::new(),
                selection_cases: scratch.selection_cases(&[niche])?,
            }));
        }
        return Ok(Some(NativeEcologyPlan {
            edits: vec![EcologyEdit::Spawn(specialist)],
            selection_cases: scratch.selection_cases(&[niche])?,
        }));
    }
    for slot in &scratch.order {
        let NicheSlot::Occupied(niche) = scratch.niches[usize::from(*slot)] else {
            continue;
        };
        if usize::from(niche.examples) < MINIMUM_EXAMPLES
            || !scratch
                .supported_axes(&[niche])
                .iter()
                .any(|supported| *supported)
            || !scratch.selection_has_target(niche, scratch.supported_axes(&[niche]))
        {
            continue;
        }
        let role = niche_role(niche.key);
        let Some(incumbent) = ecology.active_role(role) else {
            continue;
        };
        if u32::from(niche.examples) <= incumbent.support() {
            continue;
        }
        let challenger = train_specialist(scratch, niche, role, budget)?;
        if challenger.id() == incumbent.id()
            || empirical_loss(challenger.model(), scratch.selection_examples(niche))?
                >= empirical_loss(incumbent.model(), scratch.selection_examples(niche))?
        {
            continue;
        }
        return Ok(Some(NativeEcologyPlan {
            edits: vec![
                EcologyEdit::Retire(incumbent.id()),
                EcologyEdit::Spawn(challenger),
            ],
            selection_cases: scratch.selection_cases(&[niche])?,
        }));
    }
    Ok(None)
}

fn propose_split(
    scratch: &TrainingScratch<'_>,
    ecology: &ModelEcology,
    budget: NativeTrainingBudget,
) -> Result<Option<NativeEcologyPlan>, IntelligenceError> {
    for aggregate in supported_niches(scratch).filter(|niche| niche.key.family.is_none()) {
        let Some(parent) = ecology.active_role(niche_role(aggregate.key)) else {
            continue;
        };
        if !parent.routes_all_families() {
            continue;
        }
        let mut residuals = [None; MAXIMUM_PLAN_SPECIALISTS];
        let mut residual_count = 0_usize;
        for residual in compatible_exact_niches(scratch, aggregate) {
            if residual_count == residuals.len() {
                residual_count = residual_count.saturating_add(1);
                break;
            }
            residuals[residual_count] = Some(residual);
            residual_count += 1;
        }
        if residual_count != residuals.len() {
            continue;
        }
        let mut residuals = residuals.map(|residual| residual.expect("two residuals were counted"));
        residuals.sort_unstable_by_key(|niche| niche.key.family);
        let mut children = Vec::new();
        children
            .try_reserve_exact(MAXIMUM_PLAN_SPECIALISTS)
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        let mut improves_every_family = true;
        for residual in &residuals {
            let child = train_specialist(scratch, *residual, niche_role(residual.key), budget)?;
            if empirical_loss(child.model(), scratch.selection_examples(*residual))?
                >= empirical_loss(parent.model(), scratch.selection_examples(*residual))?
            {
                improves_every_family = false;
                break;
            }
            children.push(child);
        }
        if improves_every_family {
            return Ok(Some(NativeEcologyPlan {
                edits: vec![EcologyEdit::Split {
                    parent: parent.id(),
                    children,
                }],
                selection_cases: scratch.selection_cases(&residuals)?,
            }));
        }
    }
    Ok(None)
}

fn propose_merge(
    scratch: &TrainingScratch<'_>,
    ecology: &ModelEcology,
    budget: NativeTrainingBudget,
) -> Result<Option<NativeEcologyPlan>, IntelligenceError> {
    for aggregate in supported_niches(scratch).filter(|niche| niche.key.family.is_none()) {
        if ecology.active_role(niche_role(aggregate.key)).is_some() {
            continue;
        }
        let mut compatible = [None; MAXIMUM_PLAN_SPECIALISTS];
        let mut compatible_count = 0_usize;
        for pair in compatible_exact_niches(scratch, aggregate).filter_map(|niche| {
            ecology
                .active_role(niche_role(niche.key))
                .map(|specialist| (niche, specialist))
        }) {
            if compatible_count == compatible.len() {
                compatible_count = compatible_count.saturating_add(1);
                break;
            }
            compatible[compatible_count] = Some(pair);
            compatible_count += 1;
        }
        if compatible_count != compatible.len() {
            continue;
        }
        let mut compatible =
            compatible.map(|pair| pair.expect("two compatible niches were counted"));
        compatible.sort_unstable_by_key(|(niche, _)| niche.key.family);
        let [(left_niche, left), (right_niche, right)] = compatible;
        let families = [
            left_niche.key.family.expect("exact niche has a family"),
            right_niche.key.family.expect("exact niche has a family"),
        ];
        let student = train_specialist_groups(
            scratch,
            &[left_niche, right_niche],
            niche_role(aggregate.key),
            &families,
            budget,
        )?;
        let left_redundant =
            empirical_loss(student.model(), scratch.selection_examples(left_niche))?
                <= empirical_loss(left.model(), scratch.selection_examples(left_niche))?;
        let right_redundant =
            empirical_loss(student.model(), scratch.selection_examples(right_niche))?
                <= empirical_loss(right.model(), scratch.selection_examples(right_niche))?;
        if left_redundant && right_redundant {
            return Ok(Some(NativeEcologyPlan {
                edits: vec![EcologyEdit::Merge {
                    parents: Vec::from([left.id(), right.id()]),
                    merged: student,
                }],
                selection_cases: scratch.selection_cases(&[left_niche, right_niche])?,
            }));
        }
    }
    Ok(None)
}

fn supported_niches<'scratch>(
    scratch: &'scratch TrainingScratch<'_>,
) -> impl Iterator<Item = NicheGroup> + 'scratch {
    scratch.order.iter().filter_map(|slot| {
        let NicheSlot::Occupied(niche) = scratch.niches[usize::from(*slot)] else {
            return None;
        };
        let axes = scratch.supported_axes(&[niche]);
        (axes.iter().any(|supported| *supported) && scratch.selection_has_target(niche, axes))
            .then_some(niche)
    })
}

fn compatible_exact_niches<'scratch>(
    scratch: &'scratch TrainingScratch<'_>,
    aggregate: NicheGroup,
) -> impl Iterator<Item = NicheGroup> + 'scratch {
    supported_niches(scratch).filter(move |niche| {
        niche.key.family.is_some()
            && niche.key.kind == aggregate.key.kind
            && niche.key.schema == aggregate.key.schema
            && niche.key.feature_count == aggregate.key.feature_count
            && niche.key.investment == aggregate.key.investment
    })
}

fn retirement_candidate<'a>(
    ecology: &'a ModelEcology,
    evidence: &impl CausalEvidence,
) -> Option<&'a SpecialistRevision> {
    ecology.active().find(|specialist| {
        let subject = super::types::SubjectId::new(specialist.id().0);
        let mut campaigns = [None; MINIMUM_RETIREMENT_CAMPAIGNS];
        let mut campaign_count = 0_usize;
        let mut observed = false;
        for contrast in evidence
            .contrasts()
            .iter()
            .copied()
            .filter(|contrast| contrast.subject() == subject)
        {
            observed = true;
            if contrast.advantage() > 0.0 {
                return false;
            }
            if campaigns[..campaign_count]
                .iter()
                .flatten()
                .any(|campaign| *campaign == contrast.campaign())
            {
                continue;
            }
            if campaign_count < campaigns.len() {
                campaigns[campaign_count] = Some(contrast.campaign());
                campaign_count += 1;
            }
        }
        observed && campaign_count >= MINIMUM_RETIREMENT_CAMPAIGNS
    })
}

fn empirical_loss<'a>(
    model: &CompactModel,
    mut examples: impl Iterator<Item = AlignedExample<'a>>,
) -> Result<f32, IntelligenceError> {
    let mut forecasts = Vec::with_capacity(model.forecast_count());
    let (loss, labels) = examples.try_fold((0.0_f32, 0_u32), |(loss, labels), example| {
        forecasts.clear();
        model.forecast_into(example.receipt.features(), &mut forecasts)?;
        forecasts
            .iter()
            .try_fold((loss, labels), |(loss, labels), forecast| {
                let target_index = OPERATIONAL_FORECAST_AXES
                    .iter()
                    .position(|axis| *axis == forecast.axis())
                    .ok_or(IntelligenceError::InvalidModel)?;
                let Some(target) = example.targets[target_index] else {
                    return Ok((loss, labels));
                };
                let residual = target - forecast.estimate();
                Ok((residual.mul_add(residual, loss), labels.saturating_add(1)))
            })
    })?;
    if labels == 0 {
        return Ok(f32::INFINITY);
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "the bounded target count normalizes operational comparison loss"
    )]
    let labels = labels as f32;
    Ok(loss / labels)
}

fn bootstrap_loss(scratch: &TrainingScratch<'_>, niche: NicheGroup) -> f32 {
    let mut sums = [0.0_f32; OPERATIONAL_FORECAST_AXES.len()];
    let mut counts = [0_u16; OPERATIONAL_FORECAST_AXES.len()];
    for example in scratch.replay_examples(niche) {
        for ((sum, count), target) in sums.iter_mut().zip(&mut counts).zip(example.targets) {
            if let Some(target) = target {
                *sum += target;
                *count = count.saturating_add(1);
            }
        }
    }
    let means: [Option<f32>; OPERATIONAL_FORECAST_AXES.len()] = std::array::from_fn(|axis| {
        (counts[axis] != 0).then(|| sums[axis] / f32::from(counts[axis]))
    });
    let (loss, labels) =
        scratch
            .selection_examples(niche)
            .fold((0.0_f32, 0_u32), |(loss, labels), example| {
                example.targets.iter().zip(means).fold(
                    (loss, labels),
                    |(loss, labels), (target, mean)| {
                        let (Some(target), Some(mean)) = (target, mean) else {
                            return (loss, labels);
                        };
                        let residual = target - mean;
                        (residual.mul_add(residual, loss), labels.saturating_add(1))
                    },
                )
            });
    if labels == 0 {
        f32::INFINITY
    } else {
        #[expect(
            clippy::cast_precision_loss,
            reason = "the bounded target count normalizes Bootstrap comparison loss"
        )]
        let labels = labels as f32;
        loss / labels
    }
}

fn niche_key(receipt: &InvestmentReceipt) -> Option<NicheKey> {
    let feature_count = u8::try_from(receipt.features().len()).ok()?;
    if feature_count == 0 {
        return None;
    }
    Some(NicheKey {
        kind: receipt.opportunity_kind(),
        schema: receipt.feature_schema(),
        family: Some(receipt.routing_family()),
        feature_count,
        investment: receipt.tag(),
    })
}

fn train_specialist(
    scratch: &TrainingScratch<'_>,
    niche: NicheGroup,
    role: RoleId,
    budget: NativeTrainingBudget,
) -> Result<SpecialistRevision, IntelligenceError> {
    let key = niche.key;
    if let Some(family) = key.family {
        train_specialist_groups(scratch, &[niche], role, &[family], budget)
    } else {
        train_specialist_groups(scratch, &[niche], role, &[], budget)
    }
}

fn train_specialist_groups(
    scratch: &TrainingScratch<'_>,
    niches: &[NicheGroup],
    role: RoleId,
    families: &[RoutingFamilyId],
    budget: NativeTrainingBudget,
) -> Result<SpecialistRevision, IntelligenceError> {
    let key = niches.first().ok_or(IntelligenceError::InvalidModel)?.key;
    let feature_count = usize::from(key.feature_count);
    let axes = scratch.supported_axes(niches);
    if !axes.iter().any(|supported| *supported) {
        return Err(IntelligenceError::InvalidModel);
    }
    let mut weights = Vec::new();
    weights
        .try_reserve_exact(feature_count)
        .map_err(|_| IntelligenceError::CapacityExceeded)?;
    weights.resize(feature_count, [0.0_f32; OPERATIONAL_FORECAST_AXES.len()]);
    let mut bias = [0.0_f32; OPERATIONAL_FORECAST_AXES.len()];
    for epoch in 0..budget.epochs {
        let step = budget.learning_rate / f32::from(epoch.saturating_add(1)).sqrt();
        for niche in niches {
            for example in scratch.replay_examples(*niche) {
                let receipt = example.receipt;
                let mut linear = bias;
                for (weight, feature) in weights.iter().zip(receipt.features()) {
                    let feature = feature.clamp(-16.0, 16.0);
                    for (sum, weight) in linear.iter_mut().zip(weight) {
                        *sum = weight.mul_add(feature, *sum);
                    }
                }
                let errors: [f32; OPERATIONAL_FORECAST_AXES.len()] = std::array::from_fn(|axis| {
                    example.targets[axis].map_or(0.0, |target| target - logistic(linear[axis]))
                });
                for (bias, error) in bias.iter_mut().zip(errors) {
                    *bias = step.mul_add(error, *bias);
                }
                for (weight, feature) in weights.iter_mut().zip(receipt.features()) {
                    let feature = feature.clamp(-16.0, 16.0);
                    for (weight, error) in weight.iter_mut().zip(errors) {
                        *weight = (step * error).mul_add(feature, *weight);
                    }
                }
            }
        }
    }
    let support = niches.iter().fold(0_u32, |support, niche| {
        support.saturating_add(u32::from(niche.examples))
    });
    let head_count = axes.iter().filter(|supported| **supported).count();
    let mut heads = Vec::new();
    heads
        .try_reserve_exact(head_count)
        .map_err(|_| IntelligenceError::CapacityExceeded)?;
    for (axis_index, axis) in OPERATIONAL_FORECAST_AXES.iter().copied().enumerate() {
        if axes[axis_index] {
            heads.push(LinearHead::new(
                axis,
                bias[axis_index],
                weights.iter().map(|weight| weight[axis_index]),
                0.25,
                support,
            )?);
        }
    }
    let model = CompactModel::linear(feature_count, heads)?;
    SpecialistRevision::new(
        SpecialistMandate::for_routing_families(
            role,
            OPERATIONAL_FORECAST_AXES
                .iter()
                .copied()
                .enumerate()
                .filter_map(|(index, axis)| axes[index].then_some(axis)),
            [key.kind],
            [key.schema],
            families.iter().copied(),
        )?,
        model,
    )
}

#[cfg(test)]
pub(super) fn broad_test_specialist(schema: FeatureSchemaId) -> SpecialistRevision {
    let key = NicheKey {
        kind: OpportunityKind::Candidate,
        schema,
        family: None,
        feature_count: 1,
        investment: InvestmentTag::Verify,
    };
    SpecialistRevision::new(
        SpecialistMandate::for_routing_families(
            niche_role(key),
            [ForecastAxis::KernelAcceptance],
            [key.kind],
            [key.schema],
            [],
        )
        .expect("the test Generalist mandate is valid"),
        CompactModel::linear(
            1,
            [
                LinearHead::new(ForecastAxis::KernelAcceptance, 0.0, [0.0], 0.5, 1)
                    .expect("the test Generalist head is finite"),
            ],
        )
        .expect("the test Generalist model is bounded"),
    )
    .expect("the test Generalist is valid")
}

fn operational_targets(
    receipt: &InvestmentReceipt,
    settlement: InvestmentSettlement,
    effects: TrainingEffects,
    maximum_epoch: u64,
) -> [Option<f32>; OPERATIONAL_FORECAST_AXES.len()] {
    let outcome = settlement.outcome();
    let mature = maximum_epoch.saturating_sub(receipt.epoch()) >= DEAD_END_MATURITY_EPOCHS;
    let immediate_improvement = effects.contains(TrainingEffects::IMMEDIATE);
    let useful_descendants = effects.contains(TrainingEffects::DESCENDANTS);
    let cross_goal_leverage = effects.contains(TrainingEffects::CROSS_GOAL);
    let compression_value = effects.contains(TrainingEffects::COMPRESSION);
    let cost_saved = effects.contains(TrainingEffects::COST_SAVED);
    let has_delayed_value = useful_descendants || cross_goal_leverage || compression_value;
    let correctness = (receipt.tag() == InvestmentTag::Verify)
        .then_some(match outcome {
            InvestmentOutcome::VerifiedAccepted { .. } => Some(1.0),
            InvestmentOutcome::VerifiedRefuted => Some(0.0),
            InvestmentOutcome::VerifiedUnknown
            | InvestmentOutcome::Completed
            | InvestmentOutcome::Failed => None,
        })
        .flatten();
    let verification_cost = cost_saved.then_some(0.0).or_else(|| {
        (receipt.tag() == InvestmentTag::Verify
            && settlement.actual_resources().cpu_time_ns != 0)
            .then(|| {
            let cpu = settlement.actual_resources().cpu_time_ns;
            #[expect(
                clippy::cast_precision_loss,
                reason = "bounded nanosecond measurements are normalized into a statistical target"
            )]
            let cpu = cpu as f32;
            cpu / (cpu + 1_000_000.0)
        })
    });
    [
        immediate_improvement
            .then_some(1.0)
            .or_else(|| mature.then_some(0.0)),
        useful_descendants
            .then_some(1.0)
            .or_else(|| mature.then_some(0.0)),
        cross_goal_leverage
            .then_some(1.0)
            .or_else(|| mature.then_some(0.0)),
        compression_value
            .then_some(1.0)
            .or_else(|| mature.then_some(0.0)),
        correctness,
        (immediate_improvement || has_delayed_value)
            .then_some(0.0)
            .or_else(|| mature.then_some(1.0)),
        verification_cost,
    ]
}

fn niche_role(key: NicheKey) -> RoleId {
    let mut digest = Sha256::new();
    digest.update(b"reflex-native-specialist-niche-v1\0");
    digest.update([key.kind as u8]);
    digest.update(key.schema.0);
    if let Some(family) = key.family {
        digest.update([1]);
        digest.update(family.0);
    } else {
        digest.update([0]);
    }
    digest.update([key.feature_count, key.investment as u8]);
    RoleId::from_identity(digest.finalize().into())
}

fn logistic(value: f32) -> f32 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exponential = value.exp();
        exponential / (1.0 + exponential)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intelligence::causal::{
        CheckpointDigest, DecisionId, InvestmentOutcome, InvestmentSettlement, ShadowArm,
        ShadowArmOutcome, ShadowCampaignSpec, ShadowUpdate, TypedOutcome,
    };
    use crate::intelligence::types::{
        AllocationSource, IntelligenceLimits, ResourceVector, SubjectId,
    };

    #[test]
    fn aligned_training_scan_work_is_linear_in_the_bounded_retained_prefix() {
        let budget = NativeTrainingBudget::new(32, 1, 0.25).unwrap();
        let matrix = [4_usize, 8, 16, 32, 64, 128, 192].map(|retained| {
            let ledger = ledger_with_examples(retained);
            let scratch = TrainingScratch::build(&ledger, budget).unwrap();
            (
                retained,
                scratch.operations.aligned_pairs,
                scratch.operations.outcome_reads,
            )
        });

        assert_eq!(
            matrix,
            [
                (4, 4, 4),
                (8, 8, 8),
                (16, 16, 16),
                (32, 32, 32),
                (64, 64, 64),
                (128, 128, 128),
                (192, 128, 128),
            ]
        );
    }

    #[test]
    fn declared_training_bounds_cover_a_two_niche_seven_axis_plan() {
        fn maximum_specialist(identity: u8) -> SpecialistRevision {
            let heads = OPERATIONAL_FORECAST_AXES.map(|axis| {
                LinearHead::new(axis, 0.0, [0.0; MAXIMUM_FEATURES_PER_OPPORTUNITY], 0.25, 64)
                    .unwrap()
            });
            SpecialistRevision::new(
                SpecialistMandate::for_routing_families(
                    RoleId::from_identity([identity; 32]),
                    OPERATIONAL_FORECAST_AXES,
                    [OpportunityKind::Candidate],
                    [FeatureSchemaId::new([identity; 32])],
                    [
                        RoutingFamilyId::new([identity; 32]),
                        RoutingFamilyId::new([identity.saturating_add(1); 32]),
                    ],
                )
                .unwrap(),
                CompactModel::linear(MAXIMUM_FEATURES_PER_OPPORTUNITY, heads).unwrap(),
            )
            .unwrap()
        }

        let children = Vec::from([maximum_specialist(1), maximum_specialist(3)]);
        let mut selection_cases = Vec::new();
        selection_cases
            .try_reserve_exact(MAXIMUM_EXAMPLES * MAXIMUM_PLAN_SPECIALISTS)
            .unwrap();
        selection_cases.resize(MAXIMUM_EXAMPLES * MAXIMUM_PLAN_SPECIALISTS, [7; 32]);
        let plan = NativeEcologyPlan {
            edits: vec![EcologyEdit::Split {
                parent: super::super::types::SpecialistRevisionId::new([9; 32]),
                children,
            }],
            selection_cases,
        };
        let mut encoded = Vec::new();
        ModelEcology::encode_edits(&plan.edits, &mut encoded);
        let declared = usize::try_from(NativeTrainingBudget::maximum_output_bytes()).unwrap();

        assert!(
            plan.resident_bytes() <= declared,
            "resident={} declared={declared}",
            plan.resident_bytes()
        );
        assert!(
            encoded.len() <= declared,
            "encoded={} declared={declared}",
            encoded.len()
        );
        let maximum_budget = NativeTrainingBudget::new(MAXIMUM_EXAMPLES, 1, 0.25).unwrap();
        assert!(
            maximum_budget.scratch_bytes()
                >= u64::try_from(
                    MAXIMUM_FEATURES_PER_OPPORTUNITY
                        * std::mem::size_of::<[f32; OPERATIONAL_FORECAST_AXES.len()]>()
                )
                .unwrap()
        );
    }

    #[test]
    fn aligned_scratch_preserves_recent_first_examples_and_causal_eligibility() {
        let budget = NativeTrainingBudget::new(16, 1, 0.25).unwrap();
        let mut ledger = ledger_with_examples(18);
        append_example(&mut ledger, 19, 19.0, InvestmentOutcome::VerifiedUnknown);
        let scratch = TrainingScratch::build(&ledger, budget).unwrap();
        let niche = scratch
            .trainable_niche(&ModelEcology::default())
            .expect("accepted and refuted authoritative outcomes form a niche");
        let observed = scratch
            .niche_examples(niche)
            .map(|example| {
                (
                    example.receipt.features()[0],
                    example.targets[4].is_some_and(|target| target >= 0.5),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(observed.len(), 19);
        assert!((observed[0].0 - 19.0).abs() <= f32::EPSILON);
        assert!(
            observed
                .iter()
                .any(|(feature, correct)| (*feature - 19.0).abs() <= f32::EPSILON && !correct),
            "an Unknown Verification remains eligible for its measured cost target"
        );
        assert_eq!(
            usize::from(niche.examples) + usize::from(niche.selection_examples),
            19
        );
    }

    #[test]
    fn corpus_schedule_reserves_eight_disjoint_cases_then_approaches_one_fifth_selection() {
        let budget = NativeTrainingBudget::new(64, 1, 0.25).unwrap();
        let initial = ledger_with_examples(16);
        let initial_scratch = TrainingScratch::build(&initial, budget).unwrap();
        let initial_niche = initial_scratch
            .order
            .iter()
            .find_map(|slot| match initial_scratch.niches[usize::from(*slot)] {
                NicheSlot::Occupied(group) if group.key.family.is_some() => Some(group),
                NicheSlot::Empty | NicheSlot::Occupied(_) => None,
            })
            .unwrap();
        assert_eq!(initial_niche.examples, 8);
        assert_eq!(initial_niche.selection_examples, 8);

        let larger = ledger_with_examples(36);
        let larger_scratch = TrainingScratch::build(&larger, budget).unwrap();
        let larger_niche = larger_scratch
            .order
            .iter()
            .find_map(|slot| match larger_scratch.niches[usize::from(*slot)] {
                NicheSlot::Occupied(group) if group.key.family.is_some() => Some(group),
                NicheSlot::Empty | NicheSlot::Occupied(_) => None,
            })
            .unwrap();
        assert_eq!(larger_niche.examples, 24);
        assert_eq!(larger_niche.selection_examples, 12);
    }

    #[test]
    fn opening_selection_cases_never_leak_into_replay_before_the_cohort_is_complete() {
        let budget = NativeTrainingBudget::new(64, 1, 0.25).unwrap();
        for retained in 9_usize..=15 {
            let ledger = ledger_with_examples(retained);
            let scratch = TrainingScratch::build(&ledger, budget).unwrap();
            let niche = scratch
                .order
                .iter()
                .find_map(|slot| match scratch.niches[usize::from(*slot)] {
                    NicheSlot::Occupied(group) if group.key.family.is_some() => Some(group),
                    NicheSlot::Empty | NicheSlot::Occupied(_) => None,
                })
                .unwrap();

            assert_eq!(niche.examples, 8, "retained={retained}");
            assert_eq!(
                usize::from(niche.selection_examples),
                retained - MINIMUM_REPLAY_CASES,
                "retained={retained}"
            );
        }
    }

    #[test]
    fn corpus_roles_follow_persistent_ordinals_across_sliding_scan_windows() {
        let budget = NativeTrainingBudget::new(4, 1, 0.25).unwrap();
        let roles = |retained| {
            let ledger = ledger_with_examples(retained);
            let scratch = TrainingScratch::build(&ledger, budget).unwrap();
            let niche = scratch
                .order
                .iter()
                .find_map(|slot| match scratch.niches[usize::from(*slot)] {
                    NicheSlot::Occupied(group) if group.key.family.is_some() => Some(group),
                    NicheSlot::Empty | NicheSlot::Occupied(_) => None,
                })
                .unwrap();
            scratch
                .niche_examples(niche)
                .map(|example| (example.receipt.corpus_key()[0], example.selection))
                .collect::<Vec<_>>()
        };
        let first_window = roles(40);
        let shifted_window = roles(41);

        assert_eq!(
            first_window,
            vec![
                (40, false),
                (39, false),
                (38, false),
                (37, true),
                (36, false),
                (32, true),
                (27, true),
            ]
        );
        for corpus in [40_u8, 39, 38, 37, 32, 27] {
            let first_role = first_window
                .iter()
                .find_map(|(case, role)| (*case == corpus).then_some(*role))
                .unwrap();
            let shifted_role = shifted_window
                .iter()
                .find_map(|(case, role)| (*case == corpus).then_some(*role))
                .unwrap();
            assert_eq!(first_role, shifted_role, "corpus={corpus}");
        }
    }

    #[test]
    fn production_causal_experience_trains_all_operational_forecast_axes() {
        let limits = IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap();
        let budget = NativeTrainingBudget::new(16, 2, 0.25).unwrap();
        let mut ledger = CausalLedger::default();
        for index in 0_u8..16 {
            let identity = index.saturating_add(1);
            let accepted = index % 4 >= 2;
            let feature = if accepted { 1.0 } else { -1.0 };
            let receipt =
                training_receipt_in_corpus(identity, feature, [index.saturating_add(96); 32]);
            let consequences = accepted.then(|| {
                [
                    ConsequenceKind::ParetoImprovement,
                    ConsequenceKind::UsefulDescendant,
                    ConsequenceKind::CrossGoalUse,
                    ConsequenceKind::Compression,
                ]
                .map(|kind| {
                    ConsequenceEdge::mechanically_induced(
                        CausalSubject::Decision(receipt.decision()),
                        kind,
                    )
                })
            });
            ledger
                .apply(
                    &[receipt],
                    &[InvestmentSettlement::new(
                        receipt.decision(),
                        if accepted {
                            InvestmentOutcome::VerifiedAccepted {
                                verification_record: SubjectId::new([identity; 32]),
                            }
                        } else {
                            InvestmentOutcome::VerifiedRefuted
                        },
                        ResourceVector::new(u64::from(identity), 1, 0, 1, 1),
                    )],
                    consequences.as_ref().map_or(&[], |edges| edges.as_slice()),
                    &[],
                    limits,
                )
                .unwrap();
        }

        let plan = propose(&ledger, &ModelEcology::default(), budget)
            .unwrap()
            .expect("production causal settlements are trainable Experience");
        let [EcologyEdit::Spawn(specialist)] = plan.edits.as_slice() else {
            panic!("one supported generation niche spawns one Specialist");
        };
        let mut forecasts = Vec::new();
        specialist
            .model()
            .forecast_into(&[0.25], &mut forecasts)
            .unwrap();
        let axes = forecasts
            .iter()
            .map(|forecast| forecast.axis())
            .collect::<Vec<_>>();

        assert_eq!(
            axes,
            [
                ForecastAxis::ImmediateImprovement,
                ForecastAxis::UsefulDescendants,
                ForecastAxis::CrossGoalLeverage,
                ForecastAxis::CompressionValue,
                ForecastAxis::KernelAcceptance,
                ForecastAxis::DeadEndRisk,
                ForecastAxis::VerificationCost,
            ]
        );
    }

    #[test]
    fn replay_fitting_challenger_cannot_promote_against_reversed_selection_claims() {
        let limits = IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap();
        let budget = NativeTrainingBudget::new(16, 12, 0.25).unwrap();
        let mut ledger = CausalLedger::default();
        for index in 0_u8..16 {
            let replay = index % 2 == 0;
            let positive_feature = index % 4 >= 2;
            let accepted = if replay {
                positive_feature
            } else {
                !positive_feature
            };
            let receipt = training_receipt_in_corpus(
                index.saturating_add(1),
                if positive_feature { 1.0 } else { -1.0 },
                [index.saturating_add(64); 32],
            );
            ledger
                .apply(
                    &[receipt],
                    &[InvestmentSettlement::new(
                        receipt.decision(),
                        if accepted {
                            InvestmentOutcome::VerifiedAccepted {
                                verification_record: SubjectId::new([index.saturating_add(1); 32]),
                            }
                        } else {
                            InvestmentOutcome::VerifiedRefuted
                        },
                        ResourceVector::new(1, 1, 0, 1, 1),
                    )],
                    &[],
                    &[],
                    limits,
                )
                .unwrap();
        }

        let plan = propose(&ledger, &ModelEcology::default(), budget)
            .unwrap()
            .expect("a completed operational comparison consumes its Selection cases");
        assert!(
            plan.edits.is_empty(),
            "Replay loss cannot promote a challenger that loses on disjoint Selection claims"
        );
        assert_eq!(plan.selection_cases.len(), 8);
    }

    #[test]
    fn operational_targets_mask_inapplicable_and_unmatured_axes() {
        let generated = training_receipt_with_outcome_tag(41, InvestmentTag::Generate, 0);
        let generated_targets = operational_targets(
            &generated,
            InvestmentSettlement::new(
                generated.decision(),
                InvestmentOutcome::Completed,
                ResourceVector::new(9_000_000, 1, 0, 1, 0),
            ),
            TrainingEffects::default(),
            generated.epoch(),
        );
        assert_eq!(generated_targets, [None; OPERATIONAL_FORECAST_AXES.len()]);

        let verified = training_receipt_with_outcome_tag(42, InvestmentTag::Verify, 1);
        let verified_targets = operational_targets(
            &verified,
            InvestmentSettlement::new(
                verified.decision(),
                InvestmentOutcome::VerifiedAccepted {
                    verification_record: SubjectId::new([42; 32]),
                },
                ResourceVector::new(1_000_000, 1, 0, 1, 1),
            ),
            TrainingEffects::default(),
            verified.epoch(),
        );
        assert_eq!(verified_targets[0], None, "acceptance is not improvement");
        assert_eq!(verified_targets[4], Some(1.0));
        assert_eq!(verified_targets[5], None, "dead-end waits for maturity");
        assert_eq!(verified_targets[6], Some(0.5));

        let refuted = training_receipt_with_outcome_tag(43, InvestmentTag::Verify, 1);
        let mature_targets = operational_targets(
            &refuted,
            InvestmentSettlement::new(
                refuted.decision(),
                InvestmentOutcome::VerifiedRefuted,
                ResourceVector::new(0, 1, 0, 1, 1),
            ),
            TrainingEffects::default(),
            refuted.epoch().saturating_add(DEAD_END_MATURITY_EPOCHS),
        );
        assert_eq!(
            mature_targets[..6],
            [
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(0.0),
                Some(1.0),
            ]
        );
        assert_eq!(
            mature_targets[6], None,
            "an unmeasured shared batch cost is not fabricated per Candidate"
        );
    }

    #[test]
    fn completed_shadow_contrasts_train_the_exact_causal_subject() {
        let receipt = training_receipt_with_outcome_tag(45, InvestmentTag::RunShadowCampaign, 0);
        let settlement = InvestmentSettlement::new(
            receipt.decision(),
            InvestmentOutcome::Completed,
            ResourceVector::new(1, 1, 0, 1, 0),
        );
        let mut ledger = CausalLedger::default();
        ledger
            .apply(
                &[receipt],
                &[settlement],
                &[],
                &[],
                IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap(),
            )
            .unwrap();
        append_shadow_contrast(
            &mut ledger,
            SubjectId::new(receipt.opportunity_identity()),
            45,
            1.0,
            0.0,
        );

        let context =
            TrainingContext::build(&[(&receipt, settlement)], ledger.aligned(), &ledger).unwrap();
        let effects = context.effects[0].1;
        let targets = operational_targets(&receipt, settlement, effects, receipt.epoch());

        assert_eq!(targets[0], Some(1.0));
        assert_eq!(targets[5], Some(0.0));
    }

    #[test]
    fn mechanically_induced_operator_enablement_and_cpu_savings_train_potential() {
        let receipt = training_receipt_with_outcome_tag(46, InvestmentTag::Consolidate, 0);
        let settlement = InvestmentSettlement::new(
            receipt.decision(),
            InvestmentOutcome::Completed,
            ResourceVector::new(1, 1, 0, 1, 0),
        );
        let mut effects = TrainingEffects::default();
        effects.observe(ConsequenceEdge::mechanically_induced(
            CausalSubject::Decision(receipt.decision()),
            ConsequenceKind::OperatorEnabled,
        ));
        effects.observe(ConsequenceEdge::mechanically_induced(
            CausalSubject::Decision(receipt.decision()),
            ConsequenceKind::CpuSaved,
        ));

        let targets = operational_targets(&receipt, settlement, effects, receipt.epoch());

        assert_eq!(targets[1], Some(1.0));
        assert_eq!(targets[5], Some(0.0));
        assert_eq!(targets[6], Some(0.0));
    }

    #[test]
    fn mixed_family_evidence_bootstraps_one_all_family_generalist() {
        let budget = NativeTrainingBudget::new(16, 2, 0.25).unwrap();
        let mut ledger = CausalLedger::default();
        for identity in 21_u8..37 {
            let accepted = identity % 4 >= 2;
            let feature = if accepted { 1.0 } else { -1.0 };
            let decision = DecisionId::from_identity([identity; 32]);
            let receipt =
                training_receipt_in_family(identity, feature, RoutingFamilyId::new([identity; 32]));
            let outcome = if accepted {
                InvestmentOutcome::VerifiedAccepted {
                    verification_record: SubjectId::new([identity; 32]),
                }
            } else {
                InvestmentOutcome::VerifiedRefuted
            };
            ledger
                .apply(
                    &[receipt],
                    &[InvestmentSettlement::new(
                        decision,
                        outcome,
                        ResourceVector::new(1, 1, 0, 1, 1),
                    )],
                    &[],
                    &[],
                    IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap(),
                )
                .unwrap();
        }

        let plan = propose(&ledger, &ModelEcology::default(), budget)
            .unwrap()
            .expect("aggregate class-diverse evidence trains a Generalist");
        let [EcologyEdit::Spawn(specialist)] = plan.edits.as_slice() else {
            panic!("blind aggregate training emits exactly one Spawn edit");
        };
        assert!(specialist.routes_all_families());
    }

    #[test]
    fn receipt_canonical_identity_is_deterministic_and_content_sensitive() {
        let receipt = training_receipt(1, 1.0);

        assert_eq!(receipt.canonical_identity(), receipt.canonical_identity());
        assert_ne!(
            receipt.canonical_identity(),
            training_receipt(2, 1.0).canonical_identity()
        );
    }

    #[test]
    fn newer_causal_support_revises_one_same_niche_specialist_atomically() {
        let limits = IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap();
        let budget = NativeTrainingBudget::new(16, 2, 0.25).unwrap();
        let mut ledger = ledger_with_examples(16);
        let mut ecology = ModelEcology::default();
        for _ in 0..2 {
            let plan = propose(&ledger, &ecology, budget)
                .unwrap()
                .expect("aggregate and exact niches bootstrap independently");
            for edit in &plan.edits {
                ecology.apply(edit, limits).unwrap();
            }
        }
        let before = ecology
            .active()
            .map(SpecialistRevision::id)
            .collect::<Vec<_>>();
        append_example(&mut ledger, 17, -3.0, InvestmentOutcome::VerifiedRefuted);
        append_example(
            &mut ledger,
            18,
            3.0,
            InvestmentOutcome::VerifiedAccepted {
                verification_record: SubjectId::new([18; 32]),
            },
        );
        append_example(&mut ledger, 19, -4.0, InvestmentOutcome::VerifiedRefuted);
        append_example(
            &mut ledger,
            20,
            4.0,
            InvestmentOutcome::VerifiedAccepted {
                verification_record: SubjectId::new([20; 32]),
            },
        );

        let plan = propose(&ledger, &ecology, budget)
            .unwrap()
            .expect("strictly newer causal support proposes a replacement revision");
        let [EcologyEdit::Retire(parent), EcologyEdit::Spawn(challenger)] = plan.edits.as_slice()
        else {
            panic!("same-niche revision is one atomic retirement and spawn");
        };

        assert!(before.contains(parent));
        assert!(!before.contains(&challenger.id()));
    }

    #[test]
    fn two_nonpositive_paired_shadow_campaigns_retire_a_specialist() {
        let limits = IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap();
        let budget = NativeTrainingBudget::new(16, 2, 0.25).unwrap();
        let mut ledger = ledger_with_examples(16);
        let mut ecology = ModelEcology::default();
        let initial = propose(&ledger, &ecology, budget).unwrap().unwrap();
        for edit in &initial.edits {
            ecology.apply(edit, limits).unwrap();
        }
        let specialist = ecology.active().next().unwrap().id();
        for campaign_index in [31_u8, 32] {
            append_shadow_contrast(
                &mut ledger,
                SubjectId::new(specialist.0),
                campaign_index,
                0.0,
                1.0,
            );
        }

        let plan = propose(&ledger, &ecology, budget)
            .unwrap()
            .expect("repeatable nonpositive causal contribution is retirement evidence");
        assert_eq!(plan.edits, [EcologyEdit::Retire(specialist)]);
    }

    #[test]
    fn stable_family_residuals_split_a_broad_generalist() {
        let limits = IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap();
        let budget = NativeTrainingBudget::new(16, 8, 0.25).unwrap();
        let first_family = RoutingFamilyId::new([41; 32]);
        let second_family = RoutingFamilyId::new([42; 32]);
        let ledger = two_family_ledger(first_family, second_family, true);
        let mut ecology = ModelEcology::default();
        ecology
            .apply(&EcologyEdit::Spawn(broad_baseline_specialist()), limits)
            .unwrap();
        let parent = ecology.active().next().unwrap().id();

        let plan = propose(&ledger, &ecology, budget)
            .unwrap()
            .expect("opposed stable family residuals justify specialization");
        let [
            EcologyEdit::Split {
                parent: proposed_parent,
                children,
            },
        ] = plan.edits.as_slice()
        else {
            panic!("one broad parent becomes its two causally supported family children");
        };

        assert_eq!(*proposed_parent, parent);
        assert_eq!(children.len(), 2);
        assert!(children.iter().all(|child| !child.routes_all_families()));
    }

    #[test]
    fn redundant_compatible_family_specialists_merge_without_widening_their_mandate() {
        let limits = IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap();
        let budget = NativeTrainingBudget::new(16, 8, 0.25).unwrap();
        let first_family = RoutingFamilyId::new([51; 32]);
        let second_family = RoutingFamilyId::new([52; 32]);
        let ledger = two_family_ledger(first_family, second_family, false);
        let scratch = TrainingScratch::build(&ledger, budget).unwrap();
        let aggregate = supported_niches(&scratch)
            .find(|niche| niche.key.family.is_none())
            .unwrap();
        let mut ecology = ModelEcology::default();
        for niche in compatible_exact_niches(&scratch, aggregate) {
            let specialist =
                train_specialist(&scratch, niche, niche_role(niche.key), budget).unwrap();
            ecology
                .apply(&EcologyEdit::Spawn(specialist), limits)
                .unwrap();
        }
        let mut parents = ecology
            .active()
            .map(SpecialistRevision::id)
            .collect::<Vec<_>>();
        parents.sort_unstable();

        let plan = propose(&ledger, &ecology, budget)
            .unwrap()
            .expect("redundant compatible specialists compress into one routed revision");
        let [
            EcologyEdit::Merge {
                parents: proposed_parents,
                merged,
            },
        ] = plan.edits.as_slice()
        else {
            panic!("redundancy produces one Merge edit");
        };

        let mut proposed_parents = proposed_parents.clone();
        proposed_parents.sort_unstable();
        assert_eq!(proposed_parents, parents);
        assert_eq!(merged.routing_families(), [first_family, second_family]);
        assert!(!merged.routes_all_families());
    }

    #[test]
    fn incomparable_family_specialists_coexist_instead_of_being_distilled() {
        let limits = IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap();
        let budget = NativeTrainingBudget::new(16, 8, 0.25).unwrap();
        let ledger = two_family_ledger(
            RoutingFamilyId::new([61; 32]),
            RoutingFamilyId::new([62; 32]),
            true,
        );
        let scratch = TrainingScratch::build(&ledger, budget).unwrap();
        let aggregate = supported_niches(&scratch)
            .find(|niche| niche.key.family.is_none())
            .unwrap();
        let mut ecology = ModelEcology::default();
        for niche in compatible_exact_niches(&scratch, aggregate) {
            let specialist =
                train_specialist(&scratch, niche, niche_role(niche.key), budget).unwrap();
            ecology
                .apply(&EcologyEdit::Spawn(specialist), limits)
                .unwrap();
        }
        let incumbent_ids = ecology
            .active()
            .map(SpecialistRevision::id)
            .collect::<Vec<_>>();

        let plan = propose(&ledger, &ecology, budget).unwrap();

        assert!(plan.as_ref().is_none_or(|plan| {
            plan.edits.iter().all(|edit| {
                !matches!(
                    edit,
                    EcologyEdit::Merge { .. } | EcologyEdit::Distill { .. }
                )
            })
        }));
        assert!(
            incumbent_ids
                .iter()
                .all(|id| ecology.active().any(|specialist| specialist.id() == *id))
        );
    }

    #[test]
    fn a_positive_paired_contrast_protects_an_incomparable_specialist_from_retirement() {
        let limits = IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap();
        let budget = NativeTrainingBudget::new(16, 2, 0.25).unwrap();
        let mut ledger = ledger_with_examples(16);
        let mut ecology = ModelEcology::default();
        let initial = propose(&ledger, &ecology, budget).unwrap().unwrap();
        for edit in &initial.edits {
            ecology.apply(edit, limits).unwrap();
        }
        let specialist = ecology.active().next().unwrap().id();
        append_shadow_contrast(&mut ledger, SubjectId::new(specialist.0), 71, 0.0, 1.0);
        append_shadow_contrast(&mut ledger, SubjectId::new(specialist.0), 72, 2.0, 1.0);

        let plan = propose(&ledger, &ecology, budget).unwrap().unwrap();

        assert!(
            !plan
                .edits
                .iter()
                .any(|edit| matches!(edit, EcologyEdit::Retire(id) if *id == specialist))
        );
    }

    fn ledger_with_examples(count: usize) -> CausalLedger {
        let mut ledger = CausalLedger::default();
        for index in 1..=count {
            let identity = u8::try_from(index).unwrap();
            let accepted = index % 4 >= 2;
            append_example(
                &mut ledger,
                identity,
                if accepted { 1.0 } else { -1.0 },
                if accepted {
                    InvestmentOutcome::VerifiedAccepted {
                        verification_record: SubjectId::new([identity; 32]),
                    }
                } else {
                    InvestmentOutcome::VerifiedRefuted
                },
            );
        }
        ledger
    }

    fn broad_baseline_specialist() -> SpecialistRevision {
        let role = niche_role(NicheKey {
            kind: OpportunityKind::Candidate,
            schema: FeatureSchemaId::built_in(1),
            family: None,
            feature_count: 1,
            investment: InvestmentTag::Verify,
        });
        SpecialistRevision::new(
            SpecialistMandate::for_routing_families(
                role,
                [ForecastAxis::KernelAcceptance],
                [OpportunityKind::Candidate],
                [FeatureSchemaId::built_in(1)],
                [],
            )
            .unwrap(),
            CompactModel::linear(
                1,
                [LinearHead::new(ForecastAxis::KernelAcceptance, 0.0, [0.0], 0.5, 1).unwrap()],
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn append_example(
        ledger: &mut CausalLedger,
        identity: u8,
        feature: f32,
        outcome: InvestmentOutcome,
    ) {
        append_example_in_family(
            ledger,
            identity,
            feature,
            outcome,
            RoutingFamilyId::generic(),
        );
    }

    fn append_example_in_family(
        ledger: &mut CausalLedger,
        identity: u8,
        feature: f32,
        outcome: InvestmentOutcome,
        family: RoutingFamilyId,
    ) {
        let decision = DecisionId::from_identity([identity; 32]);
        let receipt = training_receipt_in_family(identity, feature, family);
        let settlement =
            InvestmentSettlement::new(decision, outcome, ResourceVector::new(1, 1, 0, 1, 1));
        ledger
            .apply(
                &[receipt],
                &[settlement],
                &[],
                &[],
                IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap(),
            )
            .unwrap();
    }

    fn two_family_ledger(
        first_family: RoutingFamilyId,
        second_family: RoutingFamilyId,
        oppose_second: bool,
    ) -> CausalLedger {
        let mut ledger = CausalLedger::default();
        for (offset, family) in [(0_u8, first_family), (32, second_family)] {
            for index in 0_usize..16 {
                let identity = offset
                    .saturating_add(u8::try_from(index).unwrap())
                    .saturating_add(1);
                let accepted = index % 4 >= 2;
                let feature = if accepted { 1.0 } else { -1.0 };
                let accepted = if offset == 0 || !oppose_second {
                    accepted
                } else {
                    !accepted
                };
                let outcome = if accepted {
                    InvestmentOutcome::VerifiedAccepted {
                        verification_record: SubjectId::new([identity; 32]),
                    }
                } else {
                    InvestmentOutcome::VerifiedRefuted
                };
                append_example_in_family(&mut ledger, identity, feature, outcome, family);
            }
        }
        ledger
    }

    fn append_shadow_contrast(
        ledger: &mut CausalLedger,
        subject: SubjectId,
        identity: u8,
        treatment: f32,
        control: f32,
    ) {
        let checkpoint = CheckpointDigest::new([identity; 32]);
        let resources = ResourceVector::new(1, 1, 0, 1, 1);
        let stream = SubjectId::new([identity.wrapping_add(64); 32]);
        let campaign = ShadowCampaignSpec::new(
            checkpoint,
            subject,
            resources,
            stream,
            [ForecastAxis::ImmediateImprovement],
        )
        .unwrap();
        let outcome = |arm, value| {
            ShadowArmOutcome::new(
                campaign.id(),
                arm,
                checkpoint,
                resources,
                stream,
                [TypedOutcome::new(ForecastAxis::ImmediateImprovement, value).unwrap()],
            )
            .unwrap()
        };
        ledger
            .apply(
                &[],
                &[],
                &[],
                &[
                    ShadowUpdate::Open(campaign.clone()),
                    ShadowUpdate::Outcome(outcome(ShadowArm::Treatment, treatment)),
                    ShadowUpdate::Outcome(outcome(ShadowArm::Control, control)),
                    ShadowUpdate::close(campaign.id()),
                ],
                IntelligenceLimits::new(64, 64, 64, 256, 8, 64 * 1024).unwrap(),
            )
            .unwrap();
    }

    fn training_receipt(identity: u8, feature: f32) -> InvestmentReceipt {
        training_receipt_in_family(identity, feature, RoutingFamilyId::generic())
    }

    fn training_receipt_in_family(
        identity: u8,
        feature: f32,
        family: RoutingFamilyId,
    ) -> InvestmentReceipt {
        InvestmentReceipt::new(
            DecisionId::from_identity([identity; 32]),
            u64::from(identity),
            [identity; 32],
            [identity; 32],
            [identity; 32],
            OpportunityKind::Candidate,
            FeatureSchemaId::built_in(1),
            family,
            &[feature],
            InvestmentTag::Verify,
            AllocationSource::Bootstrap,
            [0; 32],
            u32::from(identity),
            0,
            u32::from(identity),
            &[],
            ResourceVector::new(1, 1, 0, 1, 1),
        )
        .unwrap()
    }

    fn training_receipt_in_corpus(
        identity: u8,
        feature: f32,
        corpus_key: [u8; 32],
    ) -> InvestmentReceipt {
        InvestmentReceipt::new(
            DecisionId::from_identity([identity; 32]),
            u64::from(identity),
            [identity; 32],
            [identity; 32],
            corpus_key,
            OpportunityKind::Candidate,
            FeatureSchemaId::built_in(1),
            RoutingFamilyId::generic(),
            &[feature],
            InvestmentTag::Verify,
            AllocationSource::Bootstrap,
            [0; 32],
            u32::from(identity),
            0,
            u32::from(identity),
            &[],
            ResourceVector::new(1, 1, 0, 1, 1),
        )
        .unwrap()
    }

    fn training_receipt_with_outcome_tag(
        identity: u8,
        tag: InvestmentTag,
        verification_requests: u64,
    ) -> InvestmentReceipt {
        InvestmentReceipt::new(
            DecisionId::from_identity([identity; 32]),
            u64::from(identity),
            [identity; 32],
            [identity; 32],
            [identity; 32],
            OpportunityKind::OperatorApplication,
            FeatureSchemaId::built_in(2),
            RoutingFamilyId::generic(),
            &[1.0],
            tag,
            AllocationSource::Bootstrap,
            [0; 32],
            u32::from(identity),
            0,
            u32::from(identity),
            &[],
            ResourceVector::new(0, 1, 0, 0, verification_requests),
        )
        .unwrap()
    }
}
