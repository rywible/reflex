use sha2::{Digest, Sha256};

use crate::intelligence::ShadowCampaignId;

const MAX_POLICY_COHORT: u16 = 4_096;
const MAX_ACTIVE_SPECIALISTS: u16 = 1_024;
const MAX_SHORTLIST_WIDTH: u16 = 4_096;
const MAX_LOOKAHEAD_DEPTH: u16 = 64;
const MAX_CADENCE: u32 = 1_000_000;
const MAX_RETAINED_INCOMPARABLE_POLICIES: usize = 32;
const ENCODED_RUNTIME_POLICY_REVISION_LEN: usize = 71;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AllocationQueue {
    ProtectedOrigin = 1,
    ProtectedDerived = 2,
    Learned = 3,
    Bootstrap = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CooperativeSelection {
    pub(crate) index: usize,
    pub(crate) queue: AllocationQueue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OperationalPartition {
    ProtectedOrigin,
    ProtectedDerived,
    Unprotected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OperationalSelection {
    pub(crate) partition: OperationalPartition,
    pub(crate) index: usize,
    pub(crate) queue: AllocationQueue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimePolicyProgram {
    pub(crate) verification_cohort: u16,
    pub(crate) bootstrap_slots: u16,
    pub(crate) uncovered_claim_slots: u16,
    pub(crate) new_operator_slots: u16,
    pub(crate) lookahead_depth: u16,
    pub(crate) shortlist_width: u16,
    pub(crate) active_specialist_cap: u16,
    pub(crate) training_cadence: u32,
    pub(crate) consolidation_cadence: u32,
    pub(crate) shadow_cadence: u32,
    pub(crate) stagnation_patience: u32,
    pub(crate) exploration_per_mille: u16,
    pub(crate) shadow_budget_per_mille: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RuntimePolicyRevision {
    program: RuntimePolicyProgram,
}

impl RuntimePolicyRevision {
    pub(crate) const fn bootstrap() -> Self {
        Self {
            program: RuntimePolicyProgram {
                verification_cohort: 8,
                bootstrap_slots: 1,
                uncovered_claim_slots: 1,
                new_operator_slots: 1,
                lookahead_depth: 2,
                shortlist_width: 8,
                active_specialist_cap: 8,
                training_cadence: 32,
                consolidation_cadence: 8,
                shadow_cadence: 32,
                stagnation_patience: 128,
                exploration_per_mille: 250,
                shadow_budget_per_mille: 50,
            },
        }
    }

    pub(crate) const fn verification_cohort(self) -> u16 {
        self.program.verification_cohort
    }

    pub(crate) const fn lookahead_depth(self) -> u16 {
        self.program.lookahead_depth
    }

    pub(crate) const fn shortlist_width(self) -> u16 {
        self.program.shortlist_width
    }

    pub(crate) const fn active_specialist_cap(self) -> u16 {
        self.program.active_specialist_cap
    }

    pub(crate) const fn training_cadence(self) -> u32 {
        self.program.training_cadence
    }

    pub(crate) const fn consolidation_cadence(self) -> u32 {
        self.program.consolidation_cadence
    }

    pub(crate) const fn shadow_cadence(self) -> u32 {
        self.program.shadow_cadence
    }

    pub(crate) const fn exploration_per_mille(self) -> u16 {
        self.program.exploration_per_mille
    }

    pub(crate) const fn shadow_budget_per_mille(self) -> u16 {
        self.program.shadow_budget_per_mille
    }

    pub(crate) fn identity(self) -> [u8; 32] {
        Sha256::digest(self.encode()).into()
    }

    pub(crate) fn encode(self) -> Vec<u8> {
        let mut output = Vec::with_capacity(71);
        output.extend_from_slice(b"RFPR\x02");
        for value in [
            self.program.verification_cohort,
            self.program.bootstrap_slots,
            self.program.uncovered_claim_slots,
            self.program.new_operator_slots,
            self.program.lookahead_depth,
            self.program.shortlist_width,
            self.program.active_specialist_cap,
        ] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        for value in [
            self.program.training_cadence,
            self.program.consolidation_cadence,
            self.program.shadow_cadence,
            self.program.stagnation_patience,
        ] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        for value in [
            self.program.exploration_per_mille,
            self.program.shadow_budget_per_mille,
        ] {
            output.extend_from_slice(&value.to_le_bytes());
        }
        let checksum: [u8; 32] = Sha256::digest(&output).into();
        output.extend_from_slice(&checksum);
        output
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, RuntimePolicyViolation> {
        const PAYLOAD_LEN: usize = 39;
        if bytes.len() != PAYLOAD_LEN + 32 || &bytes[..5] != b"RFPR\x02" {
            return Err(RuntimePolicyViolation::CorruptEncoding);
        }
        let expected: [u8; 32] = Sha256::digest(&bytes[..PAYLOAD_LEN]).into();
        if bytes[PAYLOAD_LEN..] != expected {
            return Err(RuntimePolicyViolation::CorruptEncoding);
        }
        let read_u16 = |offset: usize| u16::from_le_bytes([bytes[offset], bytes[offset + 1]]);
        let read_u32 = |offset: usize| {
            u32::from_le_bytes([
                bytes[offset],
                bytes[offset + 1],
                bytes[offset + 2],
                bytes[offset + 3],
            ])
        };
        let revision = Self {
            program: RuntimePolicyProgram {
                verification_cohort: read_u16(5),
                bootstrap_slots: read_u16(7),
                uncovered_claim_slots: read_u16(9),
                new_operator_slots: read_u16(11),
                lookahead_depth: read_u16(13),
                shortlist_width: read_u16(15),
                active_specialist_cap: read_u16(17),
                training_cadence: read_u32(19),
                consolidation_cadence: read_u32(23),
                shadow_cadence: read_u32(27),
                stagnation_patience: read_u32(31),
                exploration_per_mille: read_u16(35),
                shadow_budget_per_mille: read_u16(37),
            },
        };
        RuntimePolicyKernel::verify(&revision)?;
        Ok(revision)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimePolicyState {
    generation: u64,
    active: RuntimePolicyRevision,
    predecessor: Option<RuntimePolicyRevision>,
    retained_incomparable: Vec<RuntimePolicyRevision>,
}

impl RuntimePolicyState {
    pub(crate) const fn bootstrap() -> Self {
        Self {
            generation: 0,
            active: RuntimePolicyRevision::bootstrap(),
            predecessor: None,
            retained_incomparable: Vec::new(),
        }
    }

    pub(crate) const fn active(&self) -> RuntimePolicyRevision {
        self.active
    }

    #[cfg(test)]
    pub(crate) const fn predecessor(&self) -> Option<RuntimePolicyRevision> {
        self.predecessor
    }

    #[cfg(test)]
    pub(crate) fn retained_incomparable(&self) -> &[RuntimePolicyRevision] {
        &self.retained_incomparable
    }

    pub(crate) fn identity(&self) -> [u8; 32] {
        Sha256::digest(self.encode()).into()
    }

    pub(crate) fn canonical_root(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"reflex-runtime-policy-root-v1\0");
        digest.update(self.encode());
        digest.finalize().into()
    }

    pub(crate) fn heap_bytes(&self) -> usize {
        self.retained_incomparable
            .capacity()
            .saturating_mul(std::mem::size_of::<RuntimePolicyRevision>())
    }

    pub(crate) const fn maximum_encoded_len() -> usize {
        5 + 8
            + ENCODED_RUNTIME_POLICY_REVISION_LEN
            + 1
            + ENCODED_RUNTIME_POLICY_REVISION_LEN
            + 2
            + MAX_RETAINED_INCOMPARABLE_POLICIES * ENCODED_RUNTIME_POLICY_REVISION_LEN
            + 32
    }

    pub(crate) fn challenger_at(&self, sequence: u64) -> Option<RuntimePolicyRevision> {
        let mut candidates = RuntimePolicyKernel::neighborhood(self.active);
        candidates.retain(|candidate| {
            self.predecessor != Some(*candidate) && !self.retained_incomparable.contains(candidate)
        });
        candidates.sort_unstable_by_key(|candidate| candidate.identity());
        let count = u64::try_from(candidates.len())
            .ok()
            .filter(|count| *count != 0)?;
        let index = usize::try_from(sequence % count).ok()?;
        candidates.get(index).copied()
    }

    pub(crate) fn apply_comparison(
        &mut self,
        challenger: RuntimePolicyRevision,
        incumbent_evidence: OperationalEvidence,
        challenger_evidence: OperationalEvidence,
    ) -> Result<RuntimePolicyDecision, RuntimePolicyViolation> {
        if !RuntimePolicyKernel::neighborhood(self.active).contains(&challenger)
            || self.predecessor == Some(challenger)
            || self.retained_incomparable.contains(&challenger)
        {
            return Err(RuntimePolicyViolation::InvalidChallenger);
        }
        let decision = RuntimePolicyKernel::compare(incumbent_evidence, challenger_evidence);
        match decision {
            RuntimePolicyDecision::Promote => {
                self.generation = self
                    .generation
                    .checked_add(1)
                    .ok_or(RuntimePolicyViolation::GenerationOverflow)?;
                self.predecessor = Some(std::mem::replace(&mut self.active, challenger));
            }
            RuntimePolicyDecision::RetainSpecialist => {
                if self.retained_incomparable.len() == MAX_RETAINED_INCOMPARABLE_POLICIES {
                    return Err(RuntimePolicyViolation::RetainedCapacity);
                }
                self.generation = self
                    .generation
                    .checked_add(1)
                    .ok_or(RuntimePolicyViolation::GenerationOverflow)?;
                self.retained_incomparable.push(challenger);
                self.retained_incomparable
                    .sort_unstable_by_key(|revision| revision.identity());
            }
            RuntimePolicyDecision::Reject => {}
        }
        debug_assert!(self.validate().is_ok());
        Ok(decision)
    }

    #[cfg(test)]
    pub(crate) fn rollback(&mut self) -> Result<(), RuntimePolicyViolation> {
        let predecessor = self
            .predecessor
            .ok_or(RuntimePolicyViolation::MissingPredecessor)?;
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(RuntimePolicyViolation::GenerationOverflow)?;
        self.generation = generation;
        self.predecessor = None;
        self.active = predecessor;
        debug_assert!(self.validate().is_ok());
        Ok(())
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(
            5 + 8
                + ENCODED_RUNTIME_POLICY_REVISION_LEN
                + 1
                + usize::from(self.predecessor.is_some()) * ENCODED_RUNTIME_POLICY_REVISION_LEN
                + 2
                + self.retained_incomparable.len() * ENCODED_RUNTIME_POLICY_REVISION_LEN
                + 32,
        );
        output.extend_from_slice(b"RFPS\x01");
        output.extend_from_slice(&self.generation.to_le_bytes());
        output.extend_from_slice(&self.active.encode());
        match self.predecessor {
            Some(predecessor) => {
                output.push(1);
                output.extend_from_slice(&predecessor.encode());
            }
            None => output.push(0),
        }
        output.extend_from_slice(
            &u16::try_from(self.retained_incomparable.len())
                .expect("retained Runtime Policy capacity is below u16::MAX")
                .to_le_bytes(),
        );
        for revision in &self.retained_incomparable {
            output.extend_from_slice(&revision.encode());
        }
        let checksum: [u8; 32] = Sha256::digest(&output).into();
        output.extend_from_slice(&checksum);
        output
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, RuntimePolicyViolation> {
        const MINIMUM_PAYLOAD_LEN: usize = 5 + 8 + ENCODED_RUNTIME_POLICY_REVISION_LEN + 1 + 2;
        let maximum_len = MINIMUM_PAYLOAD_LEN
            + ENCODED_RUNTIME_POLICY_REVISION_LEN
            + MAX_RETAINED_INCOMPARABLE_POLICIES * ENCODED_RUNTIME_POLICY_REVISION_LEN
            + 32;
        if bytes.len() < MINIMUM_PAYLOAD_LEN + 32 || bytes.len() > maximum_len {
            return Err(RuntimePolicyViolation::CorruptEncoding);
        }
        let payload_len = bytes
            .len()
            .checked_sub(32)
            .ok_or(RuntimePolicyViolation::CorruptEncoding)?;
        let (payload, checksum) = bytes.split_at(payload_len);
        let expected: [u8; 32] = Sha256::digest(payload).into();
        if checksum != expected {
            return Err(RuntimePolicyViolation::CorruptEncoding);
        }
        let mut input = payload;
        if take_policy_bytes(&mut input, 5)? != b"RFPS\x01" {
            return Err(RuntimePolicyViolation::CorruptEncoding);
        }
        let generation = u64::from_le_bytes(
            take_policy_bytes(&mut input, 8)?
                .try_into()
                .map_err(|_| RuntimePolicyViolation::CorruptEncoding)?,
        );
        let active = RuntimePolicyRevision::decode(take_policy_bytes(
            &mut input,
            ENCODED_RUNTIME_POLICY_REVISION_LEN,
        )?)?;
        let predecessor = match take_policy_bytes(&mut input, 1)?[0] {
            0 => None,
            1 => Some(RuntimePolicyRevision::decode(take_policy_bytes(
                &mut input,
                ENCODED_RUNTIME_POLICY_REVISION_LEN,
            )?)?),
            _ => return Err(RuntimePolicyViolation::CorruptEncoding),
        };
        let retained_count = usize::from(u16::from_le_bytes(
            take_policy_bytes(&mut input, 2)?
                .try_into()
                .map_err(|_| RuntimePolicyViolation::CorruptEncoding)?,
        ));
        if retained_count > MAX_RETAINED_INCOMPARABLE_POLICIES
            || input.len() != retained_count * ENCODED_RUNTIME_POLICY_REVISION_LEN
        {
            return Err(RuntimePolicyViolation::CorruptEncoding);
        }
        let mut retained_incomparable = Vec::with_capacity(retained_count);
        for _ in 0..retained_count {
            retained_incomparable.push(RuntimePolicyRevision::decode(take_policy_bytes(
                &mut input,
                ENCODED_RUNTIME_POLICY_REVISION_LEN,
            )?)?);
        }
        let state = Self {
            generation,
            active,
            predecessor,
            retained_incomparable,
        };
        state.validate()?;
        Ok(state)
    }

    fn validate(&self) -> Result<(), RuntimePolicyViolation> {
        RuntimePolicyKernel::verify(&self.active)?;
        if self.predecessor == Some(self.active)
            || self.retained_incomparable.len() > MAX_RETAINED_INCOMPARABLE_POLICIES
        {
            return Err(RuntimePolicyViolation::CorruptEncoding);
        }
        if self.generation == 0
            && (self.active != RuntimePolicyRevision::bootstrap()
                || self.predecessor.is_some()
                || !self.retained_incomparable.is_empty())
        {
            return Err(RuntimePolicyViolation::CorruptEncoding);
        }
        if let Some(predecessor) = self.predecessor {
            RuntimePolicyKernel::verify(&predecessor)?;
        }
        let mut prior_identity = None;
        for revision in &self.retained_incomparable {
            RuntimePolicyKernel::verify(revision)?;
            let identity = revision.identity();
            if *revision == self.active
                || self.predecessor == Some(*revision)
                || prior_identity.is_some_and(|prior| prior >= identity)
            {
                return Err(RuntimePolicyViolation::CorruptEncoding);
            }
            prior_identity = Some(identity);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PolicyUpdate {
    campaign: ShadowCampaignId,
    challenger: RuntimePolicyRevision,
}

impl PolicyUpdate {
    pub(crate) const fn comparison(
        campaign: ShadowCampaignId,
        challenger: RuntimePolicyRevision,
    ) -> Self {
        Self {
            campaign,
            challenger,
        }
    }

    pub(crate) fn apply_to(
        self,
        state: &mut RuntimePolicyState,
        incumbent_evidence: OperationalEvidence,
        challenger_evidence: OperationalEvidence,
    ) -> Result<RuntimePolicyDecision, RuntimePolicyViolation> {
        state.apply_comparison(self.challenger, incumbent_evidence, challenger_evidence)
    }

    pub(crate) const fn campaign(self) -> ShadowCampaignId {
        self.campaign
    }

    pub(crate) const fn challenger(self) -> RuntimePolicyRevision {
        self.challenger
    }

    pub(crate) fn encode_canonical(self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.campaign.identity());
        output.extend_from_slice(&self.challenger.encode());
    }

    pub(crate) fn decode_canonical(input: &mut &[u8]) -> Result<Self, RuntimePolicyViolation> {
        let campaign = ShadowCampaignId::from_identity(
            take_policy_bytes(input, 32)?
                .try_into()
                .map_err(|_| RuntimePolicyViolation::CorruptEncoding)?,
        );
        let challenger = RuntimePolicyRevision::decode(take_policy_bytes(
            input,
            ENCODED_RUNTIME_POLICY_REVISION_LEN,
        )?)?;
        Ok(Self::comparison(campaign, challenger))
    }

    pub(crate) fn decode_legacy_unbound(
        input: &mut &[u8],
    ) -> Result<
        (
            RuntimePolicyRevision,
            OperationalEvidence,
            OperationalEvidence,
        ),
        RuntimePolicyViolation,
    > {
        let challenger = RuntimePolicyRevision::decode(take_policy_bytes(
            input,
            ENCODED_RUNTIME_POLICY_REVISION_LEN,
        )?)?;
        let incumbent = OperationalEvidence::decode_canonical(input)?;
        let challenger_evidence = OperationalEvidence::decode_canonical(input)?;
        Ok((challenger, incumbent, challenger_evidence))
    }
}

fn take_policy_bytes<'a>(
    input: &mut &'a [u8],
    count: usize,
) -> Result<&'a [u8], RuntimePolicyViolation> {
    if input.len() < count {
        return Err(RuntimePolicyViolation::CorruptEncoding);
    }
    let (taken, remaining) = input.split_at(count);
    *input = remaining;
    Ok(taken)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimePolicyViolation {
    CorruptEncoding,
    InvalidCohort,
    BootstrapStarvation,
    ClaimStarvation,
    OperatorStarvation,
    ProtectedAllocationOverflow,
    InvalidLookahead,
    InvalidShortlist,
    InvalidSpecialistCap,
    InvalidCadence,
    InvalidFraction,
    InvalidChallenger,
    #[cfg(test)]
    MissingPredecessor,
    RetainedCapacity,
    GenerationOverflow,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SignedStep {
    Decrease,
    Increase,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[allow(
    dead_code,
    reason = "the mutation kernel retains verified field mutators while causal trials admit only exercised fields"
)]
pub(crate) enum RuntimePolicyMutation {
    VerificationCohort(SignedStep),
    LookaheadDepth(SignedStep),
    ShortlistWidth(SignedStep),
    ActiveSpecialistCap(SignedStep),
    TrainingCadence(SignedStep),
    ConsolidationCadence(SignedStep),
    ShadowCadence(SignedStep),
    StagnationPatience(SignedStep),
    ExplorationFraction(SignedStep),
    ShadowBudgetFraction(SignedStep),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OperationalEvidence {
    correctness_failures: u64,
    verified_discoveries: u64,
    covered_claims: u64,
    useful_descendants: u64,
    cpu_time_ns: u64,
    verification_requests: u64,
    durable_bytes: u64,
}

impl OperationalEvidence {
    pub(crate) const fn new(
        correctness_failures: u64,
        verified_discoveries: u64,
        covered_claims: u64,
        useful_descendants: u64,
        cpu_time_ns: u64,
        verification_requests: u64,
        durable_bytes: u64,
    ) -> Self {
        Self {
            correctness_failures,
            verified_discoveries,
            covered_claims,
            useful_descendants,
            cpu_time_ns,
            verification_requests,
            durable_bytes,
        }
    }

    #[cfg(test)]
    pub(crate) const fn correctness_failures(self) -> u64 {
        self.correctness_failures
    }

    #[cfg(test)]
    pub(crate) const fn verified_discoveries(self) -> u64 {
        self.verified_discoveries
    }

    #[cfg(test)]
    pub(crate) const fn covered_claims(self) -> u64 {
        self.covered_claims
    }

    #[cfg(test)]
    pub(crate) const fn useful_descendants(self) -> u64 {
        self.useful_descendants
    }

    #[cfg(test)]
    pub(crate) const fn resources(self) -> (u64, u64, u64) {
        (
            self.cpu_time_ns,
            self.verification_requests,
            self.durable_bytes,
        )
    }

    #[cfg(test)]
    fn encode_canonical(self, output: &mut Vec<u8>) {
        for value in [
            self.correctness_failures,
            self.verified_discoveries,
            self.covered_claims,
            self.useful_descendants,
            self.cpu_time_ns,
            self.verification_requests,
            self.durable_bytes,
        ] {
            output.extend_from_slice(&value.to_le_bytes());
        }
    }

    fn decode_canonical(input: &mut &[u8]) -> Result<Self, RuntimePolicyViolation> {
        let mut values = [0_u64; 7];
        for value in &mut values {
            *value = u64::from_le_bytes(
                take_policy_bytes(input, 8)?
                    .try_into()
                    .map_err(|_| RuntimePolicyViolation::CorruptEncoding)?,
            );
        }
        Ok(Self::new(
            values[0], values[1], values[2], values[3], values[4], values[5], values[6],
        ))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RuntimePolicyDecision {
    Promote,
    RetainSpecialist,
    Reject,
}

pub(crate) struct RuntimePolicyKernel;

impl RuntimePolicyKernel {
    pub(crate) fn neighborhood(parent: RuntimePolicyRevision) -> Vec<RuntimePolicyRevision> {
        use RuntimePolicyMutation::VerificationCohort;
        let mutations = [
            VerificationCohort(SignedStep::Decrease),
            VerificationCohort(SignedStep::Increase),
        ];
        let mut identities = std::collections::BTreeSet::new();
        mutations
            .into_iter()
            .filter_map(|mutation| Self::mutate(parent, mutation).ok())
            .filter(|candidate| *candidate != parent)
            .filter(|candidate| identities.insert(candidate.identity()))
            .collect()
    }

    pub(crate) const fn verify(
        revision: &RuntimePolicyRevision,
    ) -> Result<(), RuntimePolicyViolation> {
        if revision.program.verification_cohort == 0
            || revision.program.verification_cohort > MAX_POLICY_COHORT
        {
            Err(RuntimePolicyViolation::InvalidCohort)
        } else if revision.program.bootstrap_slots == 0 {
            Err(RuntimePolicyViolation::BootstrapStarvation)
        } else if revision.program.uncovered_claim_slots == 0 {
            Err(RuntimePolicyViolation::ClaimStarvation)
        } else if revision.program.new_operator_slots == 0 {
            Err(RuntimePolicyViolation::OperatorStarvation)
        } else if revision
            .program
            .bootstrap_slots
            .saturating_add(revision.program.uncovered_claim_slots)
            .saturating_add(revision.program.new_operator_slots)
            > revision.program.verification_cohort
        {
            Err(RuntimePolicyViolation::ProtectedAllocationOverflow)
        } else if revision.program.lookahead_depth == 0
            || revision.program.lookahead_depth > MAX_LOOKAHEAD_DEPTH
        {
            Err(RuntimePolicyViolation::InvalidLookahead)
        } else if revision.program.shortlist_width == 0
            || revision.program.shortlist_width > MAX_SHORTLIST_WIDTH
        {
            Err(RuntimePolicyViolation::InvalidShortlist)
        } else if revision.program.active_specialist_cap == 0
            || revision.program.active_specialist_cap > MAX_ACTIVE_SPECIALISTS
        {
            Err(RuntimePolicyViolation::InvalidSpecialistCap)
        } else if invalid_cadence(revision.program.training_cadence)
            || invalid_cadence(revision.program.consolidation_cadence)
            || invalid_cadence(revision.program.shadow_cadence)
            || invalid_cadence(revision.program.stagnation_patience)
        {
            Err(RuntimePolicyViolation::InvalidCadence)
        } else if revision.program.exploration_per_mille == 0
            || revision.program.exploration_per_mille > 1_000
            || revision.program.shadow_budget_per_mille == 0
            || revision.program.shadow_budget_per_mille > 1_000
        {
            Err(RuntimePolicyViolation::InvalidFraction)
        } else {
            Ok(())
        }
    }

    pub(crate) fn mutate(
        parent: RuntimePolicyRevision,
        mutation: RuntimePolicyMutation,
    ) -> Result<RuntimePolicyRevision, RuntimePolicyViolation> {
        let mut program = parent.program;
        match mutation {
            RuntimePolicyMutation::VerificationCohort(step) => {
                program.verification_cohort = step_u16(program.verification_cohort, step);
            }
            RuntimePolicyMutation::LookaheadDepth(step) => {
                program.lookahead_depth = step_u16(program.lookahead_depth, step);
            }
            RuntimePolicyMutation::ShortlistWidth(step) => {
                program.shortlist_width = step_u16(program.shortlist_width, step);
            }
            RuntimePolicyMutation::ActiveSpecialistCap(step) => {
                program.active_specialist_cap = step_u16(program.active_specialist_cap, step);
            }
            RuntimePolicyMutation::TrainingCadence(step) => {
                program.training_cadence = step_u32(program.training_cadence, step);
            }
            RuntimePolicyMutation::ConsolidationCadence(step) => {
                program.consolidation_cadence = step_u32(program.consolidation_cadence, step);
            }
            RuntimePolicyMutation::ShadowCadence(step) => {
                program.shadow_cadence = step_u32(program.shadow_cadence, step);
            }
            RuntimePolicyMutation::StagnationPatience(step) => {
                program.stagnation_patience = step_u32(program.stagnation_patience, step);
            }
            RuntimePolicyMutation::ExplorationFraction(step) => {
                program.exploration_per_mille = step_u16(program.exploration_per_mille, step);
            }
            RuntimePolicyMutation::ShadowBudgetFraction(step) => {
                program.shadow_budget_per_mille = step_u16(program.shadow_budget_per_mille, step);
            }
        }
        let revision = RuntimePolicyRevision { program };
        Self::verify(&revision)?;
        Ok(revision)
    }

    pub(crate) const fn compare(
        incumbent: OperationalEvidence,
        challenger: OperationalEvidence,
    ) -> RuntimePolicyDecision {
        if challenger.correctness_failures > incumbent.correctness_failures
            || challenger.verified_discoveries < incumbent.verified_discoveries
            || challenger.covered_claims < incumbent.covered_claims
        {
            return RuntimePolicyDecision::Reject;
        }
        let no_worse = challenger.useful_descendants >= incumbent.useful_descendants
            && challenger.cpu_time_ns <= incumbent.cpu_time_ns
            && challenger.verification_requests <= incumbent.verification_requests
            && challenger.durable_bytes <= incumbent.durable_bytes;
        let strictly_better = challenger.verified_discoveries > incumbent.verified_discoveries
            || challenger.covered_claims > incumbent.covered_claims
            || challenger.useful_descendants > incumbent.useful_descendants
            || challenger.cpu_time_ns < incumbent.cpu_time_ns
            || challenger.verification_requests < incumbent.verification_requests
            || challenger.durable_bytes < incumbent.durable_bytes;
        if no_worse && strictly_better {
            RuntimePolicyDecision::Promote
        } else if challenger.verified_discoveries > incumbent.verified_discoveries
            || challenger.covered_claims > incumbent.covered_claims
            || challenger.useful_descendants > incumbent.useful_descendants
            || challenger.cpu_time_ns < incumbent.cpu_time_ns
            || challenger.verification_requests < incumbent.verification_requests
        {
            RuntimePolicyDecision::RetainSpecialist
        } else {
            RuntimePolicyDecision::Reject
        }
    }
}

const fn step_u16(value: u16, step: SignedStep) -> u16 {
    match step {
        SignedStep::Decrease => value.saturating_sub(1),
        SignedStep::Increase => value.saturating_add(1),
    }
}

const fn invalid_cadence(value: u32) -> bool {
    value == 0 || value > MAX_CADENCE
}

const fn step_u32(value: u32, step: SignedStep) -> u32 {
    match step {
        SignedStep::Decrease => value.saturating_sub(1),
        SignedStep::Increase => value.saturating_add(1),
    }
}

pub(crate) fn operational_ranked_selections(
    protected_origin_count: usize,
    protected_derived_count: usize,
    bootstrap: &[usize],
    learned: Option<&[usize]>,
    limit: usize,
) -> Vec<OperationalSelection> {
    let mut output = Vec::with_capacity(
        limit.min(
            protected_origin_count
                .saturating_add(protected_derived_count)
                .saturating_add(bootstrap.len()),
        ),
    );
    output.extend(
        (0..protected_origin_count)
            .take(limit)
            .map(|index| OperationalSelection {
                partition: OperationalPartition::ProtectedOrigin,
                index,
                queue: AllocationQueue::ProtectedOrigin,
            }),
    );
    let unprotected = learned.map_or_else(
        || {
            bootstrap
                .iter()
                .copied()
                .map(|index| CooperativeSelection {
                    index,
                    queue: AllocationQueue::Bootstrap,
                })
                .collect::<Vec<_>>()
        },
        |learned| cooperative_ranked_selections(bootstrap, learned, bootstrap.len()),
    );
    let mut derived_cursor = 0;
    let mut unprotected_cursor = 0;
    while output.len() < limit {
        let before = output.len();
        for _ in 0..2 {
            if output.len() == limit || derived_cursor == protected_derived_count {
                break;
            }
            output.push(OperationalSelection {
                partition: OperationalPartition::ProtectedDerived,
                index: derived_cursor,
                queue: AllocationQueue::ProtectedDerived,
            });
            derived_cursor += 1;
        }
        for _ in 0..6 {
            if output.len() == limit || unprotected_cursor == unprotected.len() {
                break;
            }
            let selection = unprotected[unprotected_cursor];
            output.push(OperationalSelection {
                partition: OperationalPartition::Unprotected,
                index: selection.index,
                queue: selection.queue,
            });
            unprotected_cursor += 1;
        }
        if output.len() == before {
            break;
        }
    }
    output
}

pub(crate) fn cooperative_ranked_selections(
    bootstrap: &[usize],
    learned: &[usize],
    limit: usize,
) -> Vec<CooperativeSelection> {
    assert_eq!(
        bootstrap.len(),
        learned.len(),
        "cooperative policy rankings must cover the same candidates"
    );
    let candidate_count = bootstrap.len();
    debug_assert!({
        let mut bootstrap_seen = vec![false; candidate_count];
        let mut learned_seen = vec![false; candidate_count];
        bootstrap.iter().all(|index| {
            *index < candidate_count && !std::mem::replace(&mut bootstrap_seen[*index], true)
        }) && learned.iter().all(|index| {
            *index < candidate_count && !std::mem::replace(&mut learned_seen[*index], true)
        })
    });
    let limit = limit.min(candidate_count);
    let mut selected = vec![false; candidate_count];
    let mut output = Vec::with_capacity(limit);
    let mut bootstrap_cursor = 0;
    let mut learned_cursor = 0;
    let take_unique = |ranking: &[usize],
                       cursor: &mut usize,
                       count: usize,
                       queue: AllocationQueue,
                       output: &mut Vec<CooperativeSelection>,
                       selected: &mut [bool]| {
        let target = output.len().saturating_add(count);
        while output.len() < target && *cursor < ranking.len() {
            let index = ranking[*cursor];
            *cursor += 1;
            if !selected[index] {
                selected[index] = true;
                output.push(CooperativeSelection { index, queue });
            }
        }
    };
    while output.len() < limit {
        let before = output.len();
        take_unique(
            learned,
            &mut learned_cursor,
            usize::from(output.len() < limit),
            AllocationQueue::Learned,
            &mut output,
            &mut selected,
        );
        take_unique(
            bootstrap,
            &mut bootstrap_cursor,
            usize::from(output.len() < limit),
            AllocationQueue::Bootstrap,
            &mut output,
            &mut selected,
        );
        if output.len() == before {
            break;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operational_policy_keeps_protection_and_distinguishes_bootstrap_from_cooperation() {
        let bootstrap = [0, 1, 2, 3];
        let learned = [3, 2, 1, 0];
        let bootstrap_only = operational_ranked_selections(2, 3, &bootstrap, None, 8);
        let cooperative = operational_ranked_selections(2, 3, &bootstrap, Some(&learned), 8);

        assert_eq!(
            bootstrap_only
                .iter()
                .map(|selection| (selection.partition, selection.index, selection.queue))
                .collect::<Vec<_>>(),
            vec![
                (
                    OperationalPartition::ProtectedOrigin,
                    0,
                    AllocationQueue::ProtectedOrigin
                ),
                (
                    OperationalPartition::ProtectedOrigin,
                    1,
                    AllocationQueue::ProtectedOrigin
                ),
                (
                    OperationalPartition::ProtectedDerived,
                    0,
                    AllocationQueue::ProtectedDerived
                ),
                (
                    OperationalPartition::ProtectedDerived,
                    1,
                    AllocationQueue::ProtectedDerived
                ),
                (
                    OperationalPartition::Unprotected,
                    0,
                    AllocationQueue::Bootstrap
                ),
                (
                    OperationalPartition::Unprotected,
                    1,
                    AllocationQueue::Bootstrap
                ),
                (
                    OperationalPartition::Unprotected,
                    2,
                    AllocationQueue::Bootstrap
                ),
                (
                    OperationalPartition::Unprotected,
                    3,
                    AllocationQueue::Bootstrap
                ),
            ]
        );
        assert_eq!(
            cooperative[4..]
                .iter()
                .map(|selection| (selection.index, selection.queue))
                .collect::<Vec<_>>(),
            vec![
                (3, AllocationQueue::Learned),
                (0, AllocationQueue::Bootstrap),
                (2, AllocationQueue::Learned),
                (1, AllocationQueue::Bootstrap),
            ]
        );
    }

    #[test]
    fn zero_information_cooperation_preserves_candidates_but_not_bootstrap_attribution() {
        let bootstrap = [0, 1, 2, 3];
        let bootstrap_only = operational_ranked_selections(1, 1, &bootstrap, None, 6);
        let zero_information = operational_ranked_selections(1, 1, &bootstrap, Some(&bootstrap), 6);

        assert_eq!(
            bootstrap_only
                .iter()
                .map(|selection| (selection.partition, selection.index))
                .collect::<Vec<_>>(),
            zero_information
                .iter()
                .map(|selection| (selection.partition, selection.index))
                .collect::<Vec<_>>(),
            "zero information must not perturb the selected Candidate prefix"
        );
        assert_eq!(
            bootstrap_only[2].queue,
            AllocationQueue::Bootstrap,
            "the real Bootstrap branch attributes every unprotected Candidate to Bootstrap"
        );
        assert_eq!(
            zero_information[2].queue,
            AllocationQueue::Learned,
            "cooperation diverges at the first unprotected slot by applying its 1:1 learned-first rule"
        );
    }

    #[test]
    fn runtime_policy_cannot_weaken_immutable_protected_allocations() {
        let mut challenger = RuntimePolicyRevision::bootstrap();
        challenger.program.bootstrap_slots = 0;

        assert_eq!(
            RuntimePolicyKernel::verify(&challenger),
            Err(RuntimePolicyViolation::BootstrapStarvation)
        );
    }

    #[test]
    fn runtime_policy_cannot_starve_an_uncovered_correctness_claim() {
        let mut challenger = RuntimePolicyRevision::bootstrap();
        challenger.program.uncovered_claim_slots = 0;

        assert_eq!(
            RuntimePolicyKernel::verify(&challenger),
            Err(RuntimePolicyViolation::ClaimStarvation)
        );
    }

    #[test]
    fn runtime_policy_cannot_starve_a_new_operator() {
        let mut challenger = RuntimePolicyRevision::bootstrap();
        challenger.program.new_operator_slots = 0;

        assert_eq!(
            RuntimePolicyKernel::verify(&challenger),
            Err(RuntimePolicyViolation::OperatorStarvation)
        );
    }

    #[test]
    fn runtime_policy_requires_a_bounded_nonempty_verification_cohort() {
        let mut challenger = RuntimePolicyRevision::bootstrap();
        challenger.program.verification_cohort = 0;

        assert_eq!(
            RuntimePolicyKernel::verify(&challenger),
            Err(RuntimePolicyViolation::InvalidCohort)
        );
    }

    #[test]
    fn runtime_policy_checkpoint_is_canonical_and_rejects_corruption() {
        let revision = RuntimePolicyRevision::bootstrap();
        let encoded = revision.encode();

        assert_eq!(RuntimePolicyRevision::decode(&encoded), Ok(revision));
        let mut corrupt = encoded;
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(
            RuntimePolicyRevision::decode(&corrupt),
            Err(RuntimePolicyViolation::CorruptEncoding)
        );
    }

    #[test]
    fn runtime_policy_protected_slots_must_fit_its_verification_cohort() {
        let mut challenger = RuntimePolicyRevision::bootstrap();
        challenger.program.verification_cohort = 2;

        assert_eq!(
            RuntimePolicyKernel::verify(&challenger),
            Err(RuntimePolicyViolation::ProtectedAllocationOverflow)
        );
    }

    #[test]
    fn runtime_policy_bounds_every_self_optimizable_control() {
        let mut challenger = RuntimePolicyRevision::bootstrap();
        challenger.program.shortlist_width = 0;
        assert_eq!(
            RuntimePolicyKernel::verify(&challenger),
            Err(RuntimePolicyViolation::InvalidShortlist)
        );

        let mut challenger = RuntimePolicyRevision::bootstrap();
        challenger.program.active_specialist_cap = MAX_ACTIVE_SPECIALISTS + 1;
        assert_eq!(
            RuntimePolicyKernel::verify(&challenger),
            Err(RuntimePolicyViolation::InvalidSpecialistCap)
        );

        let mut challenger = RuntimePolicyRevision::bootstrap();
        challenger.program.exploration_per_mille = 1_001;
        assert_eq!(
            RuntimePolicyKernel::verify(&challenger),
            Err(RuntimePolicyViolation::InvalidFraction)
        );

        let mut challenger = RuntimePolicyRevision::bootstrap();
        challenger.program.shadow_budget_per_mille = 0;
        assert_eq!(
            RuntimePolicyKernel::verify(&challenger),
            Err(RuntimePolicyViolation::InvalidFraction)
        );

        let mut challenger = RuntimePolicyRevision::bootstrap();
        challenger.program.training_cadence = 0;
        assert_eq!(
            RuntimePolicyKernel::verify(&challenger),
            Err(RuntimePolicyViolation::InvalidCadence)
        );
    }

    #[test]
    fn runtime_policy_mutations_are_data_only_bounded_and_deterministic() {
        let parent = RuntimePolicyRevision::bootstrap();
        let first = RuntimePolicyKernel::mutate(
            parent,
            RuntimePolicyMutation::ShortlistWidth(SignedStep::Increase),
        )
        .unwrap();
        let second = RuntimePolicyKernel::mutate(
            parent,
            RuntimePolicyMutation::ShortlistWidth(SignedStep::Increase),
        )
        .unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first.program.shortlist_width,
            parent.program.shortlist_width + 1
        );
        assert_eq!(RuntimePolicyKernel::verify(&first), Ok(()));
    }

    #[test]
    fn runtime_policy_comparison_is_vector_valued_and_protects_correctness() {
        let incumbent = OperationalEvidence::new(5, 7, 3, 2, 1_000, 80, 100);
        let unsafe_challenger = OperationalEvidence::new(6, 6, 4, 2, 900, 70, 90);
        assert_eq!(
            RuntimePolicyKernel::compare(incumbent, unsafe_challenger),
            RuntimePolicyDecision::Reject
        );

        let dominant = OperationalEvidence::new(5, 8, 4, 3, 900, 70, 90);
        assert_eq!(
            RuntimePolicyKernel::compare(incumbent, dominant),
            RuntimePolicyDecision::Promote
        );

        let tradeoff = OperationalEvidence::new(5, 8, 3, 2, 1_200, 70, 90);
        assert_eq!(
            RuntimePolicyKernel::compare(incumbent, tradeoff),
            RuntimePolicyDecision::RetainSpecialist
        );
    }

    #[test]
    fn legacy_unbound_policy_delta_remains_migration_decodable_only() {
        let challenger = RuntimePolicyKernel::neighborhood(RuntimePolicyRevision::bootstrap())[0];
        let incumbent = OperationalEvidence::new(0, 4, 3, 2, 1_000, 80, 100);
        let treatment = OperationalEvidence::new(0, 5, 4, 3, 900, 70, 90);
        let mut encoded = challenger.encode();
        incumbent.encode_canonical(&mut encoded);
        treatment.encode_canonical(&mut encoded);
        let mut input = encoded.as_slice();

        assert_eq!(
            PolicyUpdate::decode_legacy_unbound(&mut input),
            Ok((challenger, incumbent, treatment))
        );
        assert!(input.is_empty());
    }

    #[test]
    fn extended_runtime_policy_checkpoint_remains_canonical() {
        let revision = RuntimePolicyKernel::mutate(
            RuntimePolicyRevision::bootstrap(),
            RuntimePolicyMutation::ShadowCadence(SignedStep::Decrease),
        )
        .unwrap();

        let encoded = revision.encode();
        assert_eq!(RuntimePolicyRevision::decode(&encoded), Ok(revision));
        assert_eq!(revision.encode(), encoded);
    }

    #[test]
    fn runtime_policy_neighborhood_contains_only_fields_exercised_by_shadow_trials() {
        let parent = RuntimePolicyRevision::bootstrap();
        let candidates = RuntimePolicyKernel::neighborhood(parent);
        let identities = candidates
            .iter()
            .map(|candidate| candidate.identity())
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(candidates.len(), identities.len());
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| RuntimePolicyKernel::verify(candidate).is_ok())
        );
        assert!(!candidates.contains(&parent));
        assert!(candidates.iter().all(|candidate| {
            candidate.program.verification_cohort != parent.program.verification_cohort
                && RuntimePolicyProgram {
                    verification_cohort: parent.program.verification_cohort,
                    ..candidate.program
                } == parent.program
        }));
    }

    #[test]
    fn runtime_policy_state_checkpoint_is_canonical() {
        let state = RuntimePolicyState::bootstrap();
        let encoded = state.encode();

        assert_eq!(RuntimePolicyState::decode(&encoded), Ok(state.clone()));
        assert_eq!(state.active(), RuntimePolicyRevision::bootstrap());
        assert_eq!(state.active().shadow_cadence(), 32);
        assert_eq!(state.active().shadow_budget_per_mille(), 50);
        assert_eq!(state.predecessor(), None);
        assert!(state.retained_incomparable().is_empty());
        let expected_identity: [u8; 32] = Sha256::digest(&encoded).into();
        assert_eq!(state.identity(), expected_identity);
        assert_eq!(state.encode(), encoded);
    }

    #[test]
    fn runtime_policy_state_selects_a_bounded_deterministic_neighbor() {
        let state = RuntimePolicyState::bootstrap();
        let first = state.challenger_at(0).unwrap();
        let cycle = RuntimePolicyKernel::neighborhood(state.active()).len() as u64;

        assert_eq!(state.challenger_at(0), Some(first));
        assert_eq!(state.challenger_at(cycle), Some(first));
        assert_ne!(first, state.active());
        assert_eq!(RuntimePolicyKernel::verify(&first), Ok(()));
    }

    #[test]
    fn dominant_runtime_policy_is_promoted_with_one_step_rollback_lineage() {
        let mut state = RuntimePolicyState::bootstrap();
        let bootstrap = state.active();
        let challenger = state.challenger_at(0).unwrap();
        let incumbent = OperationalEvidence::new(0, 7, 3, 2, 1_000, 80, 100);
        let dominant = OperationalEvidence::new(0, 8, 4, 3, 900, 70, 90);

        assert_eq!(
            state.apply_comparison(challenger, incumbent, dominant),
            Ok(RuntimePolicyDecision::Promote)
        );
        assert_eq!(state.active(), challenger);
        assert_eq!(state.predecessor(), Some(bootstrap));
        assert!(state.retained_incomparable().is_empty());
        assert_eq!(
            RuntimePolicyState::decode(&state.encode()),
            Ok(state.clone())
        );

        state.rollback().unwrap();
        assert_eq!(state.active(), bootstrap);
        assert_eq!(state.predecessor(), None);
    }

    #[test]
    fn incomparable_runtime_policy_is_retained_without_replacing_the_active_revision() {
        let mut state = RuntimePolicyState::bootstrap();
        let bootstrap = state.active();
        let challenger = state.challenger_at(0).unwrap();
        let incumbent = OperationalEvidence::new(0, 7, 3, 2, 1_000, 80, 100);
        let tradeoff = OperationalEvidence::new(0, 8, 3, 2, 1_200, 70, 90);

        assert_eq!(
            state.apply_comparison(challenger, incumbent, tradeoff),
            Ok(RuntimePolicyDecision::RetainSpecialist)
        );
        assert_eq!(state.active(), bootstrap);
        assert_eq!(state.predecessor(), None);
        assert_eq!(state.retained_incomparable(), &[challenger]);
        assert_ne!(state.challenger_at(0), Some(challenger));
        assert_eq!(
            RuntimePolicyState::decode(&state.encode()),
            Ok(state.clone())
        );
    }

    #[test]
    fn rejected_runtime_policy_leaves_the_state_byte_exact() {
        let mut state = RuntimePolicyState::bootstrap();
        let challenger = state.challenger_at(0).unwrap();
        let before = state.encode();
        let incumbent = OperationalEvidence::new(0, 7, 3, 2, 1_000, 80, 100);
        let unsafe_challenger = OperationalEvidence::new(1, 8, 4, 3, 900, 70, 90);

        assert_eq!(
            state.apply_comparison(challenger, incumbent, unsafe_challenger),
            Ok(RuntimePolicyDecision::Reject)
        );
        assert_eq!(state.encode(), before);
    }

    #[test]
    fn runtime_policy_state_decoder_rejects_hostile_semantic_encodings() {
        let bootstrap = RuntimePolicyRevision::bootstrap();
        let candidates = RuntimePolicyKernel::neighborhood(bootstrap);
        let duplicate_active = RuntimePolicyState {
            generation: 1,
            active: bootstrap,
            predecessor: Some(bootstrap),
            retained_incomparable: Vec::new(),
        };
        assert_eq!(
            RuntimePolicyState::decode(&duplicate_active.encode()),
            Err(RuntimePolicyViolation::CorruptEncoding)
        );

        let duplicate_retained = RuntimePolicyState {
            generation: 1,
            active: bootstrap,
            predecessor: None,
            retained_incomparable: vec![candidates[0], candidates[0]],
        };
        assert_eq!(
            RuntimePolicyState::decode(&duplicate_retained.encode()),
            Err(RuntimePolicyViolation::CorruptEncoding)
        );

        let mut unsorted_revisions = [candidates[0], candidates[1]];
        unsorted_revisions.sort_unstable_by_key(|revision| revision.identity());
        unsorted_revisions.reverse();
        let unsorted_retained = RuntimePolicyState {
            generation: 1,
            active: bootstrap,
            predecessor: None,
            retained_incomparable: unsorted_revisions.to_vec(),
        };
        assert_eq!(
            RuntimePolicyState::decode(&unsorted_retained.encode()),
            Err(RuntimePolicyViolation::CorruptEncoding)
        );

        let forged_generation_zero = RuntimePolicyState {
            generation: 0,
            active: candidates[0],
            predecessor: None,
            retained_incomparable: Vec::new(),
        };
        assert_eq!(
            RuntimePolicyState::decode(&forged_generation_zero.encode()),
            Err(RuntimePolicyViolation::CorruptEncoding)
        );

        let mut corrupt_nested_revision = RuntimePolicyState::bootstrap().encode();
        corrupt_nested_revision[20] ^= 1;
        let payload_len = corrupt_nested_revision.len() - 32;
        let checksum: [u8; 32] = Sha256::digest(&corrupt_nested_revision[..payload_len]).into();
        corrupt_nested_revision[payload_len..].copy_from_slice(&checksum);
        assert_eq!(
            RuntimePolicyState::decode(&corrupt_nested_revision),
            Err(RuntimePolicyViolation::CorruptEncoding)
        );

        let mut oversized_count = RuntimePolicyState::bootstrap().encode();
        oversized_count[85..87].copy_from_slice(&33_u16.to_le_bytes());
        let payload_len = oversized_count.len() - 32;
        let checksum: [u8; 32] = Sha256::digest(&oversized_count[..payload_len]).into();
        oversized_count[payload_len..].copy_from_slice(&checksum);
        assert_eq!(
            RuntimePolicyState::decode(&oversized_count),
            Err(RuntimePolicyViolation::CorruptEncoding)
        );
    }
}
