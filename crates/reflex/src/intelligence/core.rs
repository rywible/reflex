use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::Arc;

use super::arena::{
    Allocation, BidRecord, MarketFrame, OpportunityKind, OpportunitySpec, PortfolioBuffer,
    PortfolioReceipt,
};
#[cfg(test)]
use super::causal::ShadowCampaignId;
use super::causal::{
    CausalDelta, CausalEvidence, CausalExperienceView, CausalLedger, CheckpointDigest,
    ConsequenceEdge, DecisionId, InvestmentOutcome, InvestmentReceipt, InvestmentSettlement,
    ShadowArm, ShadowArmOutcome, ShadowCampaignLifecycle, ShadowUpdate, decision_id,
};
use super::codec::Decoder;
#[cfg(any(test, feature = "internal-experiments"))]
use super::ecology::ModelEcologyManifest;
use super::ecology::{EcologyEdit, ModelEcology, SpecialistMandate, SpecialistRevision};
use super::forecast::{Forecast, ResourceForecast, compare_forecast_vectors};
#[cfg(test)]
use super::knowledge::KnowledgeCompilerView;
use super::knowledge::{
    KnowledgeCompiler, KnowledgePlanningFailure, KnowledgeRecoveryManifest,
    KnowledgeShadowArmReport, KnowledgeShadowOpened, KnowledgeShadowPlan, KnowledgeShadowReport,
    KnowledgeShadowRootFact, KnowledgeShadowSupportFact, KnowledgeUpdate,
    KnowledgeVerificationReport, KnowledgeWork, OpenedKnowledgeWork,
};
use super::model::{CompactModel, IMPORTED_FTRL_AXES};
use super::operations::{OperationalAction, OperationalActionSpec};
use super::trainer::NativeEcologyPlan;
use super::trainer::{self, NativeTrainingBudget};
use super::types::{
    IntelligenceError, IntelligenceLimits, InvestmentId, OpportunityId, ResourceVector, RoleId,
    SubjectId,
};
use crate::knowledge::{
    ConsolidationChallenger, ConsolidationObligation, ConsolidationProduct, DerivationObservation,
    DerivedOperator, KnowledgeRevision, KnowledgeState, MAX_ACTIVE_ARTIFACTS,
    MAX_DERIVED_OPERATORS,
};
use crate::learning::FtrlConversionView;
use crate::policy::{
    PolicyUpdate, RuntimePolicyDecision, RuntimePolicyRevision, RuntimePolicyState,
};

#[derive(Clone, Copy)]
enum CheckpointWireFormat {
    V6,
    V7,
    V8,
    V9,
}

const CURRENT_MAGIC: [u8; 5] = *b"RFIC\x11";
const LEGACY_V16_SEGMENTED_MAGIC: [u8; 5] = *b"RFIC\x10";
const LEGACY_V15_SEGMENTED_MAGIC: [u8; 5] = *b"RFIC\x0F";
const LEGACY_V14_SEGMENTED_MAGIC: [u8; 5] = *b"RFIC\x0E";
const LEGACY_V13_SEGMENTED_MAGIC: [u8; 5] = *b"RFIC\x0D";
const LEGACY_V12_SEGMENTED_MAGIC: [u8; 5] = *b"RFIC\x0C";
const LEGACY_V11_SEGMENTED_MAGIC: [u8; 5] = *b"RFIC\x0B";
const LEGACY_V10_SEGMENTED_MAGIC: [u8; 5] = *b"RFIC\x0A";
const CHECKPOINT_FOOTER_BYTES: usize = 1 + 8 + 7 * 32;
const LEGACY_CHECKSUM_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ComponentRoots {
    ecology: [u8; 32],
    experience: [u8; 32],
    knowledge: [u8; 32],
    policy: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InvestmentKind {
    Generate {
        operator: SubjectId,
        applications: NonZeroU32,
    },
    Verify,
    Repair {
        rejection: SubjectId,
        operator: SubjectId,
    },
    Explore,
    TrainSpecialist {
        mandate: SubjectId,
    },
    CompareRevision {
        challenger: SubjectId,
    },
    Consolidate {
        compiler: SubjectId,
    },
    RunShadowCampaign {
        specification: SubjectId,
    },
    ProposeRuntimePolicy {
        parent: SubjectId,
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub(crate) enum InvestmentTag {
    Generate = 1,
    Verify = 2,
    Repair = 3,
    Explore = 4,
    TrainSpecialist = 5,
    CompareRevision = 6,
    Consolidate = 7,
    RunShadowCampaign = 8,
    ProposeRuntimePolicy = 9,
}

impl InvestmentTag {
    pub(super) fn decode(value: u8) -> Result<Self, ()> {
        match value {
            1 => Ok(Self::Generate),
            2 => Ok(Self::Verify),
            3 => Ok(Self::Repair),
            4 => Ok(Self::Explore),
            5 => Ok(Self::TrainSpecialist),
            6 => Ok(Self::CompareRevision),
            7 => Ok(Self::Consolidate),
            8 => Ok(Self::RunShadowCampaign),
            9 => Ok(Self::ProposeRuntimePolicy),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InvestmentSpec {
    pub(super) opportunity: OpportunityId,
    pub(super) kind: InvestmentKind,
    pub(super) resources: ResourceForecast,
    pub(super) preference_priority: u32,
    pub(super) bootstrap_priority: u32,
}

impl InvestmentSpec {
    pub(crate) const fn generate(
        opportunity: OpportunityId,
        operator: SubjectId,
        applications: NonZeroU32,
        resources: ResourceVector,
        bootstrap_priority: u32,
    ) -> Self {
        Self::new(
            opportunity,
            InvestmentKind::Generate {
                operator,
                applications,
            },
            resources,
            bootstrap_priority,
        )
    }

    pub(crate) const fn verify(
        opportunity: OpportunityId,
        resources: ResourceVector,
        bootstrap_priority: u32,
    ) -> Self {
        Self::new(
            opportunity,
            InvestmentKind::Verify,
            resources,
            bootstrap_priority,
        )
    }

    #[cfg(test)]
    pub(crate) const fn verify_candidate(
        opportunity: OpportunityId,
        resident_bytes: u64,
        durable_bytes: u64,
        bootstrap_priority: u32,
    ) -> Self {
        Self {
            opportunity,
            kind: InvestmentKind::Verify,
            resources: ResourceForecast::verification(resident_bytes, durable_bytes),
            preference_priority: 0,
            bootstrap_priority,
        }
    }

    pub(crate) const fn verify_candidate_with_preference(
        opportunity: OpportunityId,
        resident_bytes: u64,
        durable_bytes: u64,
        preference_priority: u32,
        bootstrap_priority: u32,
    ) -> Self {
        Self {
            opportunity,
            kind: InvestmentKind::Verify,
            resources: ResourceForecast::verification(resident_bytes, durable_bytes),
            preference_priority,
            bootstrap_priority,
        }
    }

    pub(crate) const fn repair(
        opportunity: OpportunityId,
        rejection: SubjectId,
        operator: SubjectId,
        resources: ResourceVector,
        bootstrap_priority: u32,
    ) -> Self {
        Self::new(
            opportunity,
            InvestmentKind::Repair {
                rejection,
                operator,
            },
            resources,
            bootstrap_priority,
        )
    }

    pub(crate) const fn explore(
        opportunity: OpportunityId,
        resources: ResourceVector,
        bootstrap_priority: u32,
    ) -> Self {
        Self::new(
            opportunity,
            InvestmentKind::Explore,
            resources,
            bootstrap_priority,
        )
    }

    pub(crate) const fn train_specialist(
        opportunity: OpportunityId,
        mandate: SubjectId,
        resources: ResourceVector,
        bootstrap_priority: u32,
    ) -> Self {
        Self::new(
            opportunity,
            InvestmentKind::TrainSpecialist { mandate },
            resources,
            bootstrap_priority,
        )
    }

    pub(crate) const fn compare_revision(
        opportunity: OpportunityId,
        challenger: SubjectId,
        resources: ResourceVector,
        bootstrap_priority: u32,
    ) -> Self {
        Self::new(
            opportunity,
            InvestmentKind::CompareRevision { challenger },
            resources,
            bootstrap_priority,
        )
    }

    pub(crate) const fn consolidate(
        opportunity: OpportunityId,
        compiler: SubjectId,
        resources: ResourceVector,
        bootstrap_priority: u32,
    ) -> Self {
        Self::new(
            opportunity,
            InvestmentKind::Consolidate { compiler },
            resources,
            bootstrap_priority,
        )
    }

    pub(crate) const fn run_shadow_campaign(
        opportunity: OpportunityId,
        specification: SubjectId,
        resources: ResourceVector,
        bootstrap_priority: u32,
    ) -> Self {
        Self::new(
            opportunity,
            InvestmentKind::RunShadowCampaign { specification },
            resources,
            bootstrap_priority,
        )
    }

    pub(crate) const fn propose_runtime_policy(
        opportunity: OpportunityId,
        parent: SubjectId,
        resources: ResourceVector,
        bootstrap_priority: u32,
    ) -> Self {
        Self::new(
            opportunity,
            InvestmentKind::ProposeRuntimePolicy { parent },
            resources,
            bootstrap_priority,
        )
    }

    const fn new(
        opportunity: OpportunityId,
        kind: InvestmentKind,
        resources: ResourceVector,
        bootstrap_priority: u32,
    ) -> Self {
        Self {
            opportunity,
            kind,
            resources: ResourceForecast::exact(resources),
            preference_priority: 0,
            bootstrap_priority,
        }
    }

    pub(crate) const fn tag(self) -> InvestmentTag {
        match self.kind {
            InvestmentKind::Generate { .. } => InvestmentTag::Generate,
            InvestmentKind::Verify => InvestmentTag::Verify,
            InvestmentKind::Repair { .. } => InvestmentTag::Repair,
            InvestmentKind::Explore => InvestmentTag::Explore,
            InvestmentKind::TrainSpecialist { .. } => InvestmentTag::TrainSpecialist,
            InvestmentKind::CompareRevision { .. } => InvestmentTag::CompareRevision,
            InvestmentKind::Consolidate { .. } => InvestmentTag::Consolidate,
            InvestmentKind::RunShadowCampaign { .. } => InvestmentTag::RunShadowCampaign,
            InvestmentKind::ProposeRuntimePolicy { .. } => InvestmentTag::ProposeRuntimePolicy,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct IntelligenceCheckpoint {
    bytes: Arc<Vec<u8>>,
}

impl IntelligenceCheckpoint {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[cfg(test)]
    pub(crate) fn digest(&self) -> CheckpointDigest {
        CheckpointDigest::new(self.identity())
    }

    pub(crate) fn identity(&self) -> [u8; 32] {
        let start = self
            .bytes
            .len()
            .checked_sub(32)
            .expect("an Intelligence Checkpoint always carries its checksum");
        self.bytes[start..]
            .try_into()
            .expect("an Intelligence Checkpoint checksum is exactly 32 bytes")
    }

    #[cfg(test)]
    pub(super) fn shares_storage_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.bytes, &other.bytes)
    }

    #[cfg(test)]
    pub(super) fn allocation_capacity(&self) -> usize {
        self.bytes.capacity()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct IntelligenceCore {
    limits: IntelligenceLimits,
    ecology: ModelEcology,
    experience: CausalLedger,
    knowledge: KnowledgeCompiler,
    policy: RuntimePolicyState,
    policy_import_eligible: bool,
    legacy_ftrl_import_eligible: bool,
    legacy_knowledge_import_eligible: bool,
    roots: ComponentRoots,
    revision: [u8; 32],
    checkpoint_bytes: Arc<Vec<u8>>,
    checkpoint_log_root: [u8; 32],
    checkpoint_records: u64,
}

pub(crate) struct LegacyCoreImport {
    core: IntelligenceCore,
}

impl IntelligenceCore {
    pub(crate) fn fresh(limits: IntelligenceLimits) -> Self {
        let ecology = ModelEcology::default();
        let experience = CausalLedger::default();
        let knowledge = KnowledgeCompiler::default();
        let policy = RuntimePolicyState::bootstrap();
        Self::from_components(
            limits, ecology, experience, knowledge, policy, false, false, false,
        )
    }

    pub(crate) fn fresh_for_legacy_import(
        limits: IntelligenceLimits,
    ) -> Result<LegacyCoreImport, IntelligenceError> {
        let limits = limits.validated()?;
        Ok(LegacyCoreImport {
            core: Self::from_components(
                limits,
                ModelEcology::default(),
                CausalLedger::default(),
                KnowledgeCompiler::default(),
                RuntimePolicyState::bootstrap(),
                true,
                true,
                true,
            ),
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the canonical Core constructor gathers its five authenticated components and three one-time legacy import gates"
    )]
    fn from_components(
        limits: IntelligenceLimits,
        ecology: ModelEcology,
        experience: CausalLedger,
        knowledge: KnowledgeCompiler,
        policy: RuntimePolicyState,
        policy_import_eligible: bool,
        legacy_ftrl_import_eligible: bool,
        legacy_knowledge_import_eligible: bool,
    ) -> Self {
        let roots = component_roots(&ecology, &experience, &knowledge, &policy);
        let revision = revision_digest(limits, roots);
        let (checkpoint_bytes, checkpoint_log_root) = snapshot_checkpoint(
            limits,
            &ecology,
            &experience,
            &knowledge,
            &policy,
            roots,
            revision,
        );
        Self {
            limits,
            ecology,
            experience,
            knowledge,
            policy,
            policy_import_eligible,
            legacy_ftrl_import_eligible,
            legacy_knowledge_import_eligible,
            roots,
            revision,
            checkpoint_bytes: Arc::new(checkpoint_bytes),
            checkpoint_log_root,
            checkpoint_records: 1,
        }
    }

    #[cfg(test)]
    pub(crate) fn fork_with_limits(
        &self,
        limits: IntelligenceLimits,
    ) -> Result<Self, IntelligenceError> {
        self.clone().into_fork_with_limits(limits)
    }

    pub(crate) fn into_fork_with_limits(
        self,
        limits: IntelligenceLimits,
    ) -> Result<Self, IntelligenceError> {
        let limits = limits.validated()?;
        if !self.ecology.fits_limits(limits)
            || !self.experience.fits_limits(limits)
            || !self.knowledge.fits_limits(limits)
        {
            return Err(IntelligenceError::CapacityExceeded);
        }
        Ok(Self::from_components(
            limits,
            self.ecology,
            self.experience,
            self.knowledge,
            self.policy,
            self.policy_import_eligible,
            self.legacy_ftrl_import_eligible,
            self.legacy_knowledge_import_eligible,
        ))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one contiguous decoder makes version migration, hostile-limit validation, canonical reconstruction, and revision authentication auditable as a single transaction"
    )]
    pub(crate) fn restore(bytes: &[u8]) -> Result<Self, IntelligenceError> {
        if bytes.starts_with(&CURRENT_MAGIC) {
            return Self::restore_segmented(
                bytes,
                CURRENT_MAGIC,
                true,
                true,
                true,
                true,
                true,
                true,
            );
        }
        if bytes.starts_with(&LEGACY_V16_SEGMENTED_MAGIC) {
            return Self::restore_segmented(
                bytes,
                LEGACY_V16_SEGMENTED_MAGIC,
                true,
                true,
                true,
                true,
                true,
                false,
            );
        }
        if bytes.starts_with(&LEGACY_V15_SEGMENTED_MAGIC) {
            return Self::restore_segmented(
                bytes,
                LEGACY_V15_SEGMENTED_MAGIC,
                true,
                false,
                true,
                true,
                true,
                false,
            );
        }
        if bytes.starts_with(&LEGACY_V14_SEGMENTED_MAGIC) {
            return Self::restore_segmented(
                bytes,
                LEGACY_V14_SEGMENTED_MAGIC,
                true,
                false,
                true,
                true,
                false,
                false,
            );
        }
        if bytes.starts_with(&LEGACY_V13_SEGMENTED_MAGIC) {
            return Self::restore_segmented(
                bytes,
                LEGACY_V13_SEGMENTED_MAGIC,
                true,
                false,
                true,
                false,
                false,
                false,
            );
        }
        if bytes.starts_with(&LEGACY_V12_SEGMENTED_MAGIC) {
            return Self::restore_segmented(
                bytes,
                LEGACY_V12_SEGMENTED_MAGIC,
                true,
                false,
                false,
                false,
                false,
                false,
            );
        }
        if bytes.starts_with(&LEGACY_V11_SEGMENTED_MAGIC) {
            return Self::restore_segmented(
                bytes,
                LEGACY_V11_SEGMENTED_MAGIC,
                false,
                false,
                false,
                false,
                false,
                false,
            );
        }
        if bytes.starts_with(&LEGACY_V10_SEGMENTED_MAGIC) {
            return Self::restore_segmented(
                bytes,
                LEGACY_V10_SEGMENTED_MAGIC,
                false,
                false,
                false,
                false,
                false,
                false,
            );
        }
        let payload_length = bytes
            .len()
            .checked_sub(LEGACY_CHECKSUM_BYTES)
            .ok_or(IntelligenceError::CorruptState)?;
        let (payload, checksum) = bytes.split_at(payload_length);
        let actual_checksum: [u8; 32] = Sha256::digest(payload).into();
        if checksum != actual_checksum {
            return Err(IntelligenceError::CorruptState);
        }
        let mut input = Decoder::new(payload);
        let format = match input
            .take(5)
            .map_err(|()| IntelligenceError::CorruptState)?
        {
            b"RFIC\x09" => CheckpointWireFormat::V9,
            b"RFIC\x08" => CheckpointWireFormat::V8,
            b"RFIC\x07" => CheckpointWireFormat::V7,
            b"RFIC\x06" => CheckpointWireFormat::V6,
            _ => return Err(IntelligenceError::IncompatibleRevision),
        };
        let encoded_revision = input
            .read_digest()
            .map_err(|()| IntelligenceError::CorruptState)?;
        let mut values = [0_usize; 7];
        let value_count = if matches!(format, CheckpointWireFormat::V9) {
            7
        } else {
            6
        };
        for value in &mut values[..value_count] {
            *value = usize::try_from(
                input
                    .read_u64()
                    .map_err(|()| IntelligenceError::CorruptState)?,
            )
            .map_err(|_| IntelligenceError::CorruptState)?;
        }
        let base_limits = IntelligenceLimits::new(
            values[0], values[1], values[2], values[3], values[4], values[5],
        )
        .map_err(|_| IntelligenceError::CorruptState)?;
        let decode_limits = base_limits
            .with_maximum_specialists_per_route(if value_count == 7 {
                values[6]
            } else {
                values[4]
            })
            .map_err(|_| IntelligenceError::CorruptState)?;
        let ecology = ModelEcology::decode_canonical(&mut input, decode_limits)
            .map_err(|()| IntelligenceError::CorruptState)?;
        let limits = if value_count == 7 {
            decode_limits
        } else {
            base_limits
                .with_maximum_specialists_per_route(
                    base_limits
                        .maximum_specialists_per_route
                        .max(ecology.maximum_route_fanout()),
                )
                .map_err(|_| IntelligenceError::CorruptState)?
        };
        if !ecology.fits_limits(limits) {
            return Err(IntelligenceError::CorruptState);
        }
        let experience = CausalLedger::decode_legacy_v11(&mut input, limits)
            .map_err(|()| IntelligenceError::CorruptState)?;
        let knowledge = match format {
            CheckpointWireFormat::V6 => {
                KnowledgeCompiler::decode_legacy_v6(&mut input, &experience, limits)
            }
            CheckpointWireFormat::V7 => {
                KnowledgeCompiler::decode_legacy_v7(&mut input, &experience, limits)
            }
            CheckpointWireFormat::V8 | CheckpointWireFormat::V9 => {
                KnowledgeCompiler::decode_legacy_v13(&mut input, &experience, limits)
            }
        }
        .map_err(|()| IntelligenceError::CorruptState)?;
        if !input.is_finished() {
            return Err(IntelligenceError::CorruptState);
        }
        let persisted_revision = match format {
            CheckpointWireFormat::V6 => {
                legacy_v6_revision_digest(limits, &ecology, &experience, &knowledge)
                    .map_err(|()| IntelligenceError::CorruptState)?
            }
            CheckpointWireFormat::V7 => {
                legacy_v7_revision_digest(limits, &ecology, &experience, &knowledge)
                    .map_err(|()| IntelligenceError::CorruptState)?
            }
            CheckpointWireFormat::V8 => {
                legacy_v8_revision_digest(limits, &ecology, &experience, &knowledge)
            }
            CheckpointWireFormat::V9 => {
                legacy_v9_revision_digest(limits, &ecology, &experience, &knowledge)
            }
        };
        if persisted_revision != encoded_revision {
            return Err(IntelligenceError::CorruptState);
        }
        Ok(Self::from_components(
            limits,
            ecology,
            experience,
            knowledge,
            RuntimePolicyState::bootstrap(),
            true,
            true,
            true,
        ))
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the segmented replay decoder keeps record reconstruction, component-root validation, revision validation, and footer authentication in one auditable transaction"
    )]
    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "each historical RFIC wire version explicitly declares the component fields it authenticates"
    )]
    #[expect(
        clippy::too_many_arguments,
        reason = "the migration boundary names the wire magic and each independently introduced authenticated field"
    )]
    fn restore_segmented(
        bytes: &[u8],
        expected_magic: [u8; 5],
        include_preference: bool,
        include_corpus_key: bool,
        include_policy: bool,
        include_active_knowledge: bool,
        include_pending_verification: bool,
        include_bound_policy_updates: bool,
    ) -> Result<Self, IntelligenceError> {
        let mut input = Decoder::new(bytes);
        let magic = input
            .take(5)
            .map_err(|()| IntelligenceError::CorruptState)?;
        if magic != expected_magic {
            return Err(IntelligenceError::IncompatibleRevision);
        }
        let mut values = [0_usize; 7];
        for value in &mut values {
            *value = usize::try_from(
                input
                    .read_u64()
                    .map_err(|()| IntelligenceError::CorruptState)?,
            )
            .map_err(|_| IntelligenceError::CorruptState)?;
        }
        let limits = IntelligenceLimits::new(
            values[0], values[1], values[2], values[3], values[4], values[5],
        )
        .and_then(|limits| limits.with_maximum_specialists_per_route(values[6]))
        .map_err(|_| IntelligenceError::CorruptState)?;
        if input
            .read_u8()
            .map_err(|()| IntelligenceError::CorruptState)?
            != 1
        {
            return Err(IntelligenceError::CorruptState);
        }
        let ecology_bytes = read_record_bytes(&mut input)?;
        let mut ecology_input = Decoder::new(ecology_bytes);
        let mut ecology = ModelEcology::decode_canonical(&mut ecology_input, limits)
            .map_err(|()| IntelligenceError::CorruptState)?;
        if !ecology_input.is_finished() {
            return Err(IntelligenceError::CorruptState);
        }
        let experience_bytes = read_record_bytes(&mut input)?;
        let mut experience_input = Decoder::new(experience_bytes);
        let mut experience = if include_corpus_key {
            CausalLedger::decode_canonical(&mut experience_input, limits)
        } else if include_preference {
            CausalLedger::decode_legacy_v15(&mut experience_input, limits)
        } else {
            CausalLedger::decode_legacy_v11(&mut experience_input, limits)
        }
        .map_err(|()| IntelligenceError::CorruptState)?;
        if !experience_input.is_finished() {
            return Err(IntelligenceError::CorruptState);
        }
        let knowledge_bytes = read_record_bytes(&mut input)?;
        let mut knowledge_input = Decoder::new(knowledge_bytes);
        let mut knowledge = if include_pending_verification {
            KnowledgeCompiler::decode_canonical(&mut knowledge_input, &experience, limits)
        } else if include_active_knowledge {
            KnowledgeCompiler::decode_legacy_v14(&mut knowledge_input, &experience, limits)
        } else {
            KnowledgeCompiler::decode_legacy_v13(&mut knowledge_input, &experience, limits)
        }
        .map_err(|()| IntelligenceError::CorruptState)?;
        if !knowledge_input.is_finished() {
            return Err(IntelligenceError::CorruptState);
        }
        let policy_bytes = if include_policy {
            Some(read_record_bytes(&mut input)?)
        } else {
            None
        };
        let mut policy = policy_bytes.map_or_else(
            || Ok(RuntimePolicyState::bootstrap()),
            |encoded| {
                RuntimePolicyState::decode(encoded).map_err(|_| IntelligenceError::CorruptState)
            },
        )?;
        let mut snapshot_record = vec![1];
        push_record_bytes(&mut snapshot_record, ecology_bytes);
        push_record_bytes(&mut snapshot_record, experience_bytes);
        push_record_bytes(&mut snapshot_record, knowledge_bytes);
        if let Some(encoded) = policy_bytes {
            push_record_bytes(&mut snapshot_record, encoded);
        }
        let mut reconstructed_records = 1_u64;
        let mut reconstructed_log_root =
            checkpoint_log_root(checkpoint_header_root(&bytes[..61]), &snapshot_record);
        let mut roots = component_roots(&ecology, &experience, &knowledge, &policy);
        let mut validation_roots = if include_corpus_key {
            roots
        } else if include_pending_verification {
            legacy_v15_component_roots(&ecology, &experience, &knowledge, &policy)
        } else if include_active_knowledge {
            legacy_v14_component_roots(&ecology, &experience, &knowledge, &policy)
        } else if include_policy {
            legacy_v13_component_roots(&ecology, &experience, &knowledge, &policy)
        } else if include_preference {
            legacy_v12_component_roots(&ecology, &experience, &knowledge)
        } else {
            legacy_v11_component_roots(&ecology, &experience, &knowledge)
        };
        loop {
            match input
                .read_u8()
                .map_err(|()| IntelligenceError::CorruptState)?
            {
                0 => break,
                2 => {
                    let flags = input
                        .read_u8()
                        .map_err(|()| IntelligenceError::CorruptState)?;
                    let allowed_flags = if include_policy { 7 } else { 3 };
                    if flags & !allowed_flags != 0 {
                        return Err(IntelligenceError::CorruptState);
                    }
                    let mut record = vec![2, flags];
                    if flags & 1 != 0 {
                        let encoded = read_record_bytes(&mut input)?;
                        let mut component_input = Decoder::new(encoded);
                        let edits = ModelEcology::decode_edits(&mut component_input, limits)
                            .map_err(|()| IntelligenceError::CorruptState)?;
                        if edits.is_empty() || !component_input.is_finished() {
                            return Err(IntelligenceError::CorruptState);
                        }
                        for edit in &edits {
                            ecology
                                .apply(edit, limits)
                                .map_err(|_| IntelligenceError::CorruptState)?;
                        }
                        roots.ecology = extend_component_root(b"ecology", roots.ecology, encoded);
                        validation_roots.ecology =
                            extend_component_root(b"ecology", validation_roots.ecology, encoded);
                        push_record_bytes(&mut record, encoded);
                    }
                    let encoded_delta = read_record_bytes(&mut input)?;
                    let mut delta_input = Decoder::new(encoded_delta);
                    let delta = if include_corpus_key {
                        CausalDelta::decode_canonical(&mut delta_input, &experience, limits)
                    } else if include_preference {
                        CausalDelta::decode_legacy_v15(&mut delta_input, &experience, limits)
                    } else {
                        CausalDelta::decode_legacy_v11(&mut delta_input, &experience, limits)
                    }
                    .map_err(|()| IntelligenceError::CorruptState)?;
                    if !delta_input.is_finished() {
                        return Err(IntelligenceError::CorruptState);
                    }
                    let shadows_changed = delta.has_shadow_updates();
                    experience
                        .commit_delta(delta)
                        .map_err(|_| IntelligenceError::CorruptState)?;
                    roots.experience = experience.canonical_root();
                    validation_roots.experience = if include_corpus_key {
                        roots.experience
                    } else if include_preference {
                        experience.legacy_v15_canonical_root()
                    } else {
                        experience.legacy_v11_canonical_root()
                    };
                    push_record_bytes(&mut record, encoded_delta);
                    let reconciled = if shadows_changed {
                        knowledge
                            .reconcile_promotions(&experience)
                            .map_err(|_| IntelligenceError::CorruptState)?
                    } else {
                        false
                    };
                    if flags & 2 != 0 {
                        let encoded = read_record_bytes(&mut input)?;
                        let mut component_input = Decoder::new(encoded);
                        let updates =
                            KnowledgeCompiler::decode_updates(&mut component_input, limits)
                                .map_err(|()| IntelligenceError::CorruptState)?;
                        if !component_input.is_finished() {
                            return Err(IntelligenceError::CorruptState);
                        }
                        let applied = knowledge
                            .apply(&updates, &experience, limits)
                            .map_err(|_| IntelligenceError::CorruptState)?;
                        if !reconciled && !applied {
                            return Err(IntelligenceError::CorruptState);
                        }
                        roots.knowledge =
                            extend_component_root(b"knowledge", roots.knowledge, encoded);
                        validation_roots.knowledge = extend_component_root(
                            b"knowledge",
                            validation_roots.knowledge,
                            encoded,
                        );
                        push_record_bytes(&mut record, encoded);
                    } else if reconciled {
                        return Err(IntelligenceError::CorruptState);
                    }
                    if flags & 4 != 0 {
                        let encoded = read_record_bytes(&mut input)?;
                        let mut policy_input = encoded;
                        if include_bound_policy_updates {
                            let update = PolicyUpdate::decode_canonical(&mut policy_input)
                                .map_err(|_| IntelligenceError::CorruptState)?;
                            let (incumbent_evidence, challenger_evidence) = experience
                                .runtime_policy_evidence(
                                    update.campaign(),
                                    SubjectId::new(update.challenger().identity()),
                                )
                                .map_err(|_| IntelligenceError::CorruptState)?;
                            update
                                .apply_to(&mut policy, incumbent_evidence, challenger_evidence)
                                .map_err(|_| IntelligenceError::CorruptState)?;
                        } else {
                            let (challenger, incumbent_evidence, challenger_evidence) =
                                PolicyUpdate::decode_legacy_unbound(&mut policy_input)
                                    .map_err(|_| IntelligenceError::CorruptState)?;
                            policy
                                .apply_comparison(
                                    challenger,
                                    incumbent_evidence,
                                    challenger_evidence,
                                )
                                .map_err(|_| IntelligenceError::CorruptState)?;
                        }
                        if !policy_input.is_empty() {
                            return Err(IntelligenceError::CorruptState);
                        }
                        roots.policy = extend_component_root(b"policy", roots.policy, encoded);
                        validation_roots.policy = roots.policy;
                        push_record_bytes(&mut record, encoded);
                    }
                    reconstructed_records = reconstructed_records
                        .checked_add(1)
                        .ok_or(IntelligenceError::CorruptState)?;
                    reconstructed_log_root = checkpoint_log_root(reconstructed_log_root, &record);
                }
                _ => return Err(IntelligenceError::CorruptState),
            }
        }
        let records = input
            .read_u64()
            .map_err(|()| IntelligenceError::CorruptState)?;
        let encoded_roots = ComponentRoots {
            ecology: input
                .read_digest()
                .map_err(|()| IntelligenceError::CorruptState)?,
            experience: input
                .read_digest()
                .map_err(|()| IntelligenceError::CorruptState)?,
            knowledge: input
                .read_digest()
                .map_err(|()| IntelligenceError::CorruptState)?,
            policy: if include_policy {
                input
                    .read_digest()
                    .map_err(|()| IntelligenceError::CorruptState)?
            } else {
                RuntimePolicyState::bootstrap().canonical_root()
            },
        };
        let encoded_revision = input
            .read_digest()
            .map_err(|()| IntelligenceError::CorruptState)?;
        let encoded_log_root = input
            .read_digest()
            .map_err(|()| IntelligenceError::CorruptState)?;
        let encoded_checksum = input
            .read_digest()
            .map_err(|()| IntelligenceError::CorruptState)?;
        if !input.is_finished() || records != reconstructed_records {
            return Err(IntelligenceError::CorruptState);
        }
        let revision = if include_policy {
            revision_digest(limits, validation_roots)
        } else {
            legacy_v12_revision_digest(limits, validation_roots)
        };
        let log_root = reconstructed_log_root;
        let mut footer = Vec::new();
        if include_policy {
            append_checkpoint_footer(&mut footer, records, validation_roots, revision, log_root);
        } else {
            append_legacy_segmented_checkpoint_footer(
                &mut footer,
                records,
                validation_roots,
                revision,
                log_root,
            );
        }
        let expected_checksum: [u8; 32] = footer[footer.len() - 32..]
            .try_into()
            .map_err(|_| IntelligenceError::CorruptState)?;
        if validation_roots != encoded_roots
            || revision != encoded_revision
            || log_root != encoded_log_root
            || expected_checksum != encoded_checksum
        {
            return Err(IntelligenceError::CorruptState);
        }
        if !include_policy {
            return Ok(Self::from_components(
                limits, ecology, experience, knowledge, policy, true, true, true,
            ));
        }
        if !include_bound_policy_updates {
            // A legacy segmented log is authoritative only under its own delta
            // decoder. Canonicalize it into one current snapshot before any
            // caller can append a v17 bound PolicyUpdate; otherwise the x10
            // header would make that new record decode as legacy-unbound on
            // the next Resume.
            return Ok(Self::from_components(
                limits,
                ecology,
                experience,
                knowledge,
                policy,
                false,
                false,
                !include_active_knowledge,
            ));
        }
        Ok(Self {
            limits,
            ecology,
            experience,
            knowledge,
            policy,
            policy_import_eligible: false,
            legacy_ftrl_import_eligible: false,
            legacy_knowledge_import_eligible: !include_active_knowledge,
            roots,
            revision,
            checkpoint_bytes: Arc::new(bytes.to_vec()),
            checkpoint_log_root: log_root,
            checkpoint_records: records,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the allocation transaction stays contiguous so permanent Bootstrap protection, specialist bidding, resource reservation, and immutable receipt issuance share one auditable ordering"
    )]
    pub(crate) fn allocate<'a>(
        &self,
        frame: MarketFrame<'_>,
        limit: usize,
        output: &'a mut PortfolioBuffer,
    ) -> Result<PortfolioReceipt<'a>, IntelligenceError> {
        output.clear();
        output.selected.resize(frame.market.investments.len(), 0);
        output.order.extend(0..frame.market.investments.len());
        output.order.sort_unstable_by_key(|index| {
            let specification = frame.market.investments[*index].specification;
            (specification.bootstrap_priority, *index)
        });
        let mut reserved = ResourceVector::default();
        for index in output.order.iter().copied() {
            if output.allocations.len() == limit.min(1) {
                break;
            }
            let specification = frame.market.investments[index].specification;
            let Some(prospective) = reserved.checked_add(specification.resources.declared_upper())
            else {
                return Err(IntelligenceError::ResourceOverflow);
            };
            if !prospective.fits_within(frame.allowance) {
                continue;
            }
            let opportunity = frame
                .market
                .opportunity(specification.opportunity)
                .ok_or(IntelligenceError::InvalidReference)?;
            let _features = frame.market.features(*opportunity);
            let investment = super::types::InvestmentId(
                u32::try_from(index).map_err(|_| IntelligenceError::CapacityExceeded)?,
            );
            output.allocations.push(Allocation::bootstrap(
                investment,
                specification.bootstrap_priority,
                specification.resources.declared_upper(),
            ));
            output.selected[index] = 1;
            reserved = prospective;
        }
        for (index, investment) in frame.market.investments.iter().enumerate() {
            let opportunity = frame
                .market
                .opportunity(investment.specification.opportunity)
                .ok_or(IntelligenceError::InvalidReference)?;
            let features = frame.market.features(*opportunity);
            for specialist in self
                .ecology
                .routed(opportunity.specification, features.len())
            {
                output.record_specialist_evaluation();
                debug_assert!(
                    specialist.permits_opportunity(opportunity.specification, features.len()),
                    "the derived Routing Index may only return compatible Specialists"
                );
                if output.bids.len() >= self.limits.maximum_bids {
                    return Err(IntelligenceError::CapacityExceeded);
                }
                let forecast_start = output.forecasts.len();
                let forecast_end = forecast_start
                    .checked_add(specialist.model().forecast_count())
                    .ok_or(IntelligenceError::ResourceOverflow)?;
                if forecast_end > self.limits.maximum_forecast_cells {
                    return Err(IntelligenceError::CapacityExceeded);
                }
                let (start, count) = specialist
                    .model()
                    .forecast_into(features, &mut output.forecasts)?;
                debug_assert_eq!(output.forecasts.len(), forecast_end);
                if output.forecasts[forecast_start..]
                    .iter()
                    .any(|forecast| !specialist.permits(forecast.axis()))
                {
                    output.forecasts.truncate(forecast_start);
                    return Err(IntelligenceError::InvalidMandate);
                }
                output.bids.push(BidRecord {
                    investment: InvestmentId(
                        u32::try_from(index).map_err(|_| IntelligenceError::CapacityExceeded)?,
                    ),
                    specialist: specialist.id(),
                    forecast_start: start,
                    forecast_count: count,
                });
            }
        }
        output.order.clear();
        output.order.extend(0..output.bids.len());
        output.order.sort_unstable_by(|left, right| {
            let left_specification =
                frame.market.investments[output.bids[*left].investment.0 as usize].specification;
            let right_specification =
                frame.market.investments[output.bids[*right].investment.0 as usize].specification;
            let left_forecasts = bid_forecasts(&output.bids[*left], &output.forecasts);
            let right_forecasts = bid_forecasts(&output.bids[*right], &output.forecasts);
            left_specification
                .preference_priority
                .cmp(&right_specification.preference_priority)
                .then_with(|| compare_forecast_vectors(left_forecasts, right_forecasts))
                .then_with(|| {
                    resource_values(left_specification.resources.declared_upper()).cmp(
                        &resource_values(right_specification.resources.declared_upper()),
                    )
                })
                .then_with(|| {
                    let left_investment = output.bids[*left].investment.0;
                    let right_investment = output.bids[*right].investment.0;
                    left_investment.cmp(&right_investment)
                })
                .then_with(|| {
                    output.bids[*left]
                        .specialist
                        .cmp(&output.bids[*right].specialist)
                })
        });
        for bid_index in output.order.iter().copied() {
            if output.allocations.len() == limit {
                break;
            }
            let bid = output.bids[bid_index];
            let investment_index = bid.investment.0 as usize;
            if output.selected[investment_index] != 0 {
                continue;
            }
            let specification = frame.market.investments[investment_index].specification;
            let Some(prospective) = reserved.checked_add(specification.resources.declared_upper())
            else {
                return Err(IntelligenceError::ResourceOverflow);
            };
            if !prospective.fits_within(frame.allowance) {
                continue;
            }
            output.allocations.push(Allocation::specialist(
                bid.investment,
                bid.specialist,
                specification.bootstrap_priority,
                specification.resources.declared_upper(),
                representative_forecast(&bid, &output.forecasts),
                bid.forecast_start,
                bid.forecast_count,
            ));
            output.selected[investment_index] = 1;
            reserved = prospective;
        }
        if output.allocations.len() < limit {
            output.order.clear();
            output.order.extend(0..frame.market.investments.len());
            output.order.sort_unstable_by_key(|index| {
                let specification = frame.market.investments[*index].specification;
                (
                    specification.preference_priority,
                    resource_values(specification.resources.declared_upper()),
                    specification.bootstrap_priority,
                    *index,
                )
            });
            for index in output.order.iter().copied() {
                if output.allocations.len() == limit || output.selected[index] != 0 {
                    continue;
                }
                let specification = frame.market.investments[index].specification;
                let Some(prospective) =
                    reserved.checked_add(specification.resources.declared_upper())
                else {
                    return Err(IntelligenceError::ResourceOverflow);
                };
                if !prospective.fits_within(frame.allowance) {
                    continue;
                }
                output.allocations.push(Allocation::bootstrap(
                    InvestmentId(
                        u32::try_from(index).map_err(|_| IntelligenceError::CapacityExceeded)?,
                    ),
                    specification.bootstrap_priority,
                    specification.resources.declared_upper(),
                ));
                output.selected[index] = 1;
                reserved = prospective;
            }
        }
        for (rank, allocation) in output.allocations.iter().enumerate() {
            let specification = frame
                .market
                .investment(allocation.investment)
                .ok_or(IntelligenceError::InvalidReference)?;
            let opportunity = frame
                .market
                .opportunity(specification.opportunity)
                .ok_or(IntelligenceError::InvalidReference)?;
            let features = frame.market.features(*opportunity);
            let investment = investment_digest(specification, opportunity.specification, features);
            let policy_rank =
                u32::try_from(rank).map_err(|_| IntelligenceError::CapacityExceeded)?;
            output.receipts.push(InvestmentReceipt::new(
                decision_id(
                    self.revision,
                    frame.epoch,
                    policy_rank,
                    investment,
                    allocation.source,
                ),
                frame.epoch,
                investment,
                opportunity.specification.identity,
                opportunity.specification.corpus_key,
                opportunity.specification.kind,
                opportunity.specification.feature_schema,
                opportunity.specification.routing_family,
                features,
                specification.tag(),
                allocation.source,
                self.revision,
                policy_rank,
                specification.preference_priority,
                allocation.bootstrap_priority,
                allocation.forecasts(&output.forecasts),
                allocation.resources,
            )?);
        }
        Ok(PortfolioReceipt {
            allocations: &output.allocations,
            receipts: &output.receipts,
        })
    }

    #[cfg(test)]
    pub(crate) fn propose_native_ecology_edits(
        &self,
        budget: NativeTrainingBudget,
    ) -> Result<Option<NativeEcologyPlan>, IntelligenceError> {
        trainer::propose(&self.experience, &self.ecology, budget)
    }

    pub(crate) fn prepare_native_training(
        &self,
        evidence: SettlementFrame<'_>,
        budget: NativeTrainingBudget,
    ) -> Result<Option<PreparedNativeEcologyPlan>, IntelligenceError> {
        if !evidence.edits.is_empty()
            || !evidence.knowledge.is_empty()
            || evidence.policy.is_some()
            || evidence.native_training.is_some()
            || evidence.prepared_native_training.is_some()
            || evidence.opened_verification.is_some()
        {
            return Err(IntelligenceError::InvalidModel);
        }
        let delta = self.experience.prepare_delta(
            evidence.receipts,
            evidence.settlements,
            evidence.consequences,
            evidence.shadows,
            self.limits,
        )?;
        Ok(
            trainer::propose_staged(&self.experience, &delta, &self.ecology, budget)?.map(|plan| {
                PreparedNativeEcologyPlan {
                    base_revision: self.revision,
                    evidence_root: delta.canonical_root(),
                    receipt_count: evidence.receipts.len(),
                    settlement_count: evidence.settlements.len(),
                    consequence_count: evidence.consequences.len(),
                    shadow_count: evidence.shadows.len(),
                    budget,
                    scratch_bytes: budget.scratch_bytes_for_history(
                        self.experience
                            .receipt_count()
                            .saturating_add(evidence.receipts.len()),
                    ),
                    plan,
                }
            }),
        )
    }

    pub(crate) fn reproduce_prepared_native_training(
        &self,
        evidence: SettlementFrame<'_>,
        prepared: &PreparedNativeEcologyPlan,
    ) -> Result<bool, IntelligenceError> {
        if prepared.base_revision != self.revision
            || prepared.receipt_count != evidence.receipts.len()
            || prepared.settlement_count != evidence.settlements.len()
            || prepared.consequence_count != evidence.consequences.len()
            || prepared.shadow_count != evidence.shadows.len()
        {
            return Err(IntelligenceError::StaleTransition);
        }
        let Some(reproduced) = self.prepare_native_training(evidence, prepared.budget)? else {
            return Ok(false);
        };
        Ok(reproduced.evidence_root == prepared.evidence_root
            && reproduced.plan.identity() == prepared.plan.identity())
    }

    #[cfg(test)]
    pub(crate) fn train_native(
        &self,
        budget: NativeTrainingBudget,
    ) -> Result<Option<IntelligenceTransition>, IntelligenceError> {
        if self.propose_native_ecology_edits(budget)?.is_none() {
            return Ok(None);
        }
        self.stage(SettlementFrame::empty().with_native_training(budget))
            .map(Some)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the atomic Intelligence transition keeps causal preparation, optional Ecology and Knowledge copy-on-write, authenticated operation framing, and the revision commitment in one auditable transaction"
    )]
    pub(crate) fn stage(
        &self,
        settlement: SettlementFrame<'_>,
    ) -> Result<IntelligenceTransition, IntelligenceError> {
        let has_stale_receipt = settlement
            .receipts
            .iter()
            .any(|receipt| receipt.issued_under() != self.revision);
        let valid_opened_receipts = settlement.opened_verification.is_some_and(|opened| {
            opened.opened_revision == self.revision
                && self.knowledge.matches_opened_verification(opened)
                && settlement.receipts == opened.receipts()
        });
        if has_stale_receipt && !valid_opened_receipts {
            return Err(IntelligenceError::StaleTransition);
        }
        let mut causal_delta = self.experience.prepare_delta(
            settlement.receipts,
            settlement.settlements,
            settlement.consequences,
            settlement.shadows,
            self.limits,
        )?;
        let mut ecology_edits = settlement.edits.to_vec();
        let mut ecology = if ecology_edits.is_empty() {
            None
        } else {
            Some(self.ecology.clone())
        };
        for edit in &ecology_edits {
            ecology
                .as_mut()
                .expect("an Ecology edit materializes the proposed Ecology")
                .apply(edit, self.limits)?;
        }
        if settlement.prepared_native_training.is_some() && settlement.native_training.is_some() {
            return Err(IntelligenceError::InvalidModel);
        }
        let prepared_plan = settlement
            .prepared_native_training
            .map(|prepared| {
                if prepared.base_revision == self.revision {
                    let evidence = self.experience.prepare_delta(
                        settlement
                            .receipts
                            .get(..prepared.receipt_count)
                            .ok_or(IntelligenceError::StaleTransition)?,
                        settlement
                            .settlements
                            .get(..prepared.settlement_count)
                            .ok_or(IntelligenceError::StaleTransition)?,
                        settlement
                            .consequences
                            .get(..prepared.consequence_count)
                            .ok_or(IntelligenceError::StaleTransition)?,
                        settlement
                            .shadows
                            .get(..prepared.shadow_count)
                            .ok_or(IntelligenceError::StaleTransition)?,
                        self.limits,
                    )?;
                    if evidence.canonical_root() != prepared.evidence_root {
                        return Err(IntelligenceError::StaleTransition);
                    }
                    Ok(&prepared.plan)
                } else {
                    Err(IntelligenceError::StaleTransition)
                }
            })
            .transpose()?;
        let staged_plan = if prepared_plan.is_some() {
            None
        } else if let Some(budget) = settlement.native_training {
            trainer::propose_staged(
                &self.experience,
                &causal_delta,
                ecology.as_ref().unwrap_or(&self.ecology),
                budget,
            )?
        } else {
            None
        };
        if let Some(plan) = prepared_plan.or(staged_plan.as_ref()) {
            causal_delta.record_selection_uses(
                &self.experience,
                &plan.selection_cases,
                self.limits,
            )?;
            if !plan.edits.is_empty() {
                let proposed = ecology.get_or_insert_with(|| self.ecology.clone());
                for edit in &plan.edits {
                    proposed.apply(edit, self.limits)?;
                }
                ecology_edits.extend_from_slice(&plan.edits);
            }
        }
        let overlay = self.experience.overlay(&causal_delta);
        let knowledge_may_change =
            !settlement.knowledge.is_empty() || !settlement.shadows.is_empty();
        let mut knowledge = knowledge_may_change.then(|| self.knowledge.clone());
        if let Some(proposed) = &mut knowledge {
            let reconciled = proposed.reconcile_promotions(&overlay)?;
            let applied = proposed.apply(settlement.knowledge, &overlay, self.limits)?;
            if !reconciled && !applied {
                knowledge = None;
            }
        }
        let (policy, policy_decision, policy_delta) = if let Some(update) = settlement.policy {
            let mut proposed = self.policy.clone();
            let (incumbent_evidence, challenger_evidence) = overlay.runtime_policy_evidence(
                update.campaign(),
                SubjectId::new(update.challenger().identity()),
            )?;
            let decision = update
                .apply_to(&mut proposed, incumbent_evidence, challenger_evidence)
                .map_err(|_| IntelligenceError::InvalidPolicy)?;
            let mut encoded = Vec::new();
            update.encode_canonical(&mut encoded);
            (Some(proposed), Some(decision), Some(encoded))
        } else {
            (None, None, None)
        };
        let ecology_delta = ecology.as_ref().map(|_| {
            let mut encoded = Vec::new();
            ModelEcology::encode_edits(&ecology_edits, &mut encoded);
            encoded
        });
        let knowledge_delta = knowledge.as_ref().map(|_| {
            let mut encoded = Vec::new();
            KnowledgeCompiler::encode_updates(settlement.knowledge, &mut encoded);
            encoded
        });
        let roots = ComponentRoots {
            ecology: ecology_delta.as_ref().map_or(self.roots.ecology, |delta| {
                extend_component_root(b"ecology", self.roots.ecology, delta)
            }),
            experience: causal_delta.canonical_root(),
            knowledge: knowledge_delta
                .as_ref()
                .map_or(self.roots.knowledge, |delta| {
                    extend_component_root(b"knowledge", self.roots.knowledge, delta)
                }),
            policy: self.roots.policy,
        };
        let roots = ComponentRoots {
            policy: policy_delta.as_ref().map_or(roots.policy, |delta| {
                extend_component_root(b"policy", roots.policy, delta)
            }),
            ..roots
        };
        let revision = revision_digest(self.limits, roots);
        let checkpoint_record = if causal_delta.is_empty()
            && ecology_delta.is_none()
            && knowledge_delta.is_none()
            && policy_delta.is_none()
        {
            Vec::new()
        } else {
            encode_delta_record(
                ecology_delta.as_deref(),
                &causal_delta,
                knowledge_delta.as_deref(),
                policy_delta.as_deref(),
            )
        };
        let (checkpoint_log_root, checkpoint_records) = if checkpoint_record.is_empty() {
            (self.checkpoint_log_root, self.checkpoint_records)
        } else {
            (
                checkpoint_log_root(self.checkpoint_log_root, &checkpoint_record),
                self.checkpoint_records
                    .checked_add(1)
                    .ok_or(IntelligenceError::ResourceOverflow)?,
            )
        };
        #[cfg(test)]
        let ecology_items_cloned = ecology
            .as_ref()
            .map_or(0, |_| self.ecology.retained_item_count());
        #[cfg(test)]
        let knowledge_items_cloned = knowledge
            .as_ref()
            .map_or(0, |_| self.knowledge.retained_item_count());
        Ok(IntelligenceTransition {
            base_revision: self.revision,
            ecology,
            experience: causal_delta,
            knowledge,
            policy,
            policy_decision,
            roots,
            revision,
            base_checkpoint: Arc::clone(&self.checkpoint_bytes),
            checkpoint_record,
            checkpoint_log_root,
            checkpoint_records,
            #[cfg(test)]
            ecology_items_cloned,
            #[cfg(test)]
            knowledge_items_cloned,
        })
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    pub(crate) const fn manifest(&self) -> ModelEcologyManifest<'_> {
        self.ecology.manifest()
    }

    pub(crate) const fn model_ecology_identity(&self) -> [u8; 32] {
        self.roots.ecology
    }

    #[cfg(feature = "internal-experiments")]
    pub(crate) fn treatment_component_inspection(
        &self,
    ) -> ([[u8; 32]; 4], [[u8; 32]; 4], usize, bool, usize) {
        let digest = |bytes: Vec<u8>| <[u8; 32]>::from(Sha256::digest(bytes));
        let mut ecology = Vec::new();
        self.ecology.encode_canonical(&mut ecology);
        let mut experience = Vec::new();
        self.experience.encode_canonical(&mut experience);
        let mut knowledge = Vec::new();
        self.knowledge.encode_canonical(&mut knowledge);
        let statistics = self.knowledge.statistics();
        let canonical_roots = component_roots(
            &self.ecology,
            &self.experience,
            &self.knowledge,
            &self.policy,
        );
        (
            [
                canonical_roots.ecology,
                canonical_roots.experience,
                canonical_roots.knowledge,
                canonical_roots.policy,
            ],
            [
                digest(ecology),
                digest(experience),
                digest(knowledge),
                digest(self.policy.encode()),
            ],
            statistics
                .provisional
                .saturating_add(statistics.verified)
                .saturating_add(statistics.promoted)
                .saturating_add(statistics.invalidated),
            self.knowledge.opened_verification(self.revision).is_some(),
            self.knowledge.pinned_revision().operators().len(),
        )
    }

    pub(crate) const fn experience(&self) -> CausalExperienceView<'_> {
        self.experience.view()
    }

    #[cfg(test)]
    pub(crate) const fn knowledge(&self) -> KnowledgeCompilerView<'_> {
        self.knowledge.view()
    }

    pub(crate) fn pinned_knowledge_revision(&self) -> &KnowledgeRevision {
        self.knowledge.pinned_revision()
    }

    pub(crate) fn knowledge_product(&self) -> ConsolidationProduct {
        self.knowledge.product()
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    pub(crate) const fn knowledge_generation(&self) -> u64 {
        self.knowledge.generation()
    }

    pub(crate) fn validate_active_knowledge(
        &self,
        artifact_keys: &BTreeSet<[u8; 32]>,
        observations: &[DerivationObservation],
        primitive_symbols: &BTreeSet<Vec<u8>>,
    ) -> bool {
        self.knowledge
            .validate_active(artifact_keys, observations, primitive_symbols)
    }

    pub(crate) fn knowledge_recovery_manifest_resident_bytes(
        &self,
    ) -> Result<usize, IntelligenceError> {
        self.knowledge.recovery_manifest_resident_bytes(self.limits)
    }

    pub(crate) fn knowledge_recovery_manifest(
        &self,
    ) -> Result<KnowledgeRecoveryManifest, IntelligenceError> {
        self.knowledge
            .recovery_manifest(self.revision, self.roots.knowledge, self.limits)
    }

    pub(crate) fn knowledge_recovery_manifest_matches(
        &self,
        manifest: &KnowledgeRecoveryManifest,
    ) -> bool {
        manifest.matches(self.revision, self.roots.knowledge)
    }

    #[cfg(feature = "internal-experiments")]
    pub(crate) fn knowledge_statistics(&self) -> super::knowledge::KnowledgeStatistics {
        self.knowledge.statistics()
    }

    #[cfg(test)]
    pub(crate) fn propose_knowledge_consolidation(
        &self,
        observations: &[DerivationObservation],
        roots: impl IntoIterator<Item = [u8; 32]>,
        pareto: impl IntoIterator<Item = [u8; 32]>,
    ) -> Option<ConsolidationChallenger> {
        self.knowledge
            .propose_consolidation(observations, roots, pareto)
    }

    #[cfg(test)]
    pub(crate) fn plan_knowledge_verification(
        &self,
        observations: &[DerivationObservation],
        roots: impl IntoIterator<Item = [u8; 32]>,
        pareto: impl IntoIterator<Item = [u8; 32]>,
    ) -> Result<Option<KnowledgeWork>, IntelligenceError> {
        let Some(consolidation) = self
            .knowledge
            .propose_consolidation(observations, roots, pareto)
        else {
            return Ok(None);
        };
        if consolidation.obligations().is_empty() {
            return Ok(None);
        }
        let work = KnowledgeWork::verification(self.revision, &consolidation)?;
        if self.knowledge.contains(work.id()) {
            return Ok(None);
        }
        self.knowledge
            .ensure_proposal_capacity(&work.challenger, self.limits)?;
        Ok(Some(work))
    }

    pub(crate) fn prepare_knowledge_compilation(
        &self,
        resources: ResourceVector,
    ) -> PreparedKnowledgeCompilation {
        let broad_compiler = knowledge_compiler_subject(b"broad-roots");
        let focused_compiler = knowledge_compiler_subject(b"pareto-roots");
        PreparedKnowledgeCompilation {
            base_revision: self.revision,
            broad_compiler,
            focused_compiler,
            specifications: [
                OperationalActionSpec::new(
                    OperationalAction::Consolidate {
                        compiler: broad_compiler,
                    },
                    resources,
                    0,
                    [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0],
                ),
                OperationalActionSpec::new(
                    OperationalAction::Consolidate {
                        compiler: focused_compiler,
                    },
                    resources,
                    1,
                    [0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 1.0, 0.0],
                ),
            ],
        }
    }

    pub(crate) fn knowledge_compilation_resident_bound(
        &self,
        shape: KnowledgeCompilationShape,
        root_count: usize,
    ) -> Result<u64, IntelligenceError> {
        let observations = shape.observations;
        let roots = u64::try_from(root_count).map_err(|_| IntelligenceError::ResourceOverflow)?;
        let source = vector_capacity_bytes::<DerivationObservation>(observations)?
            .checked_add(shape.identity_bytes)
            .and_then(|bytes| {
                bytes.checked_add(vector_capacity_bytes::<Vec<u8>>(shape.steps).ok()?)
            })
            .and_then(|bytes| bytes.checked_add(shape.step_bytes))
            .ok_or(IntelligenceError::ResourceOverflow)?;

        // Every accepted child may retain one independently allocated expanded
        // operator sequence. The full observed step payload per child is a
        // conservative bound even when one parent owns nearly every step.
        let expanded_sequences = vector_capacity_bytes::<Vec<u8>>(shape.steps)?
            .checked_add(shape.step_bytes)
            .and_then(|bytes| bytes.checked_mul(observations.max(1)))
            .ok_or(IntelligenceError::ResourceOverflow)?;
        let accepted_index =
            btree_capacity_bytes::<(([u8; 32], [u8; 32]), &DerivationObservation)>(observations)?;
        let discovered_index = btree_capacity_bytes::<(
            Vec<Vec<u8>>,
            (BTreeSet<[u8; 32]>, BTreeSet<[u8; 32]>),
        )>(observations)?;
        let discovered_support = btree_capacity_bytes::<[u8; 32]>(observations)?
            .checked_mul(2)
            .ok_or(IntelligenceError::ResourceOverflow)?;
        let root_sets = btree_capacity_bytes::<[u8; 32]>(roots)?
            .checked_mul(2)
            .ok_or(IntelligenceError::ResourceOverflow)?;
        let scored_artifacts = observations
            .checked_mul(2)
            .ok_or(IntelligenceError::ResourceOverflow)?;
        let score_index = btree_capacity_bytes::<([u8; 32], u64)>(scored_artifacts)?;
        let score_order = vector_capacity_bytes::<([u8; 32], u64)>(scored_artifacts)?;

        let maximum_operators = u64::try_from(MAX_DERIVED_OPERATORS)
            .map_err(|_| IntelligenceError::ResourceOverflow)?;
        let maximum_active =
            u64::try_from(MAX_ACTIVE_ARTIFACTS).map_err(|_| IntelligenceError::ResourceOverflow)?;
        let active_output = vector_capacity_bytes::<[u8; 32]>(maximum_active)?;
        let operator_headers = vector_capacity_bytes::<DerivedOperator>(maximum_operators)?;
        let operator_symbols = maximum_operators
            .checked_mul(u64::try_from(b"derived:".len() + 64).unwrap_or(u64::MAX))
            .ok_or(IntelligenceError::ResourceOverflow)?;
        let operator_support = vector_capacity_bytes::<[u8; 32]>(observations)?;
        let output_revision = u64::try_from(std::mem::size_of::<KnowledgeRevision>())
            .map_err(|_| IntelligenceError::ResourceOverflow)?
            .checked_add(active_output)
            .and_then(|bytes| bytes.checked_add(operator_headers))
            .and_then(|bytes| bytes.checked_add(operator_symbols))
            .and_then(|bytes| bytes.checked_add(expanded_sequences))
            .and_then(|bytes| bytes.checked_add(operator_support))
            .ok_or(IntelligenceError::ResourceOverflow)?;
        let obligation_output =
            vector_capacity_bytes::<ConsolidationObligation>(maximum_operators)?;
        let challenger_output = u64::try_from(std::mem::size_of::<ConsolidationChallenger>())
            .map_err(|_| IntelligenceError::ResourceOverflow)?
            .checked_add(output_revision)
            .and_then(|bytes| bytes.checked_add(obligation_output))
            .ok_or(IntelligenceError::ResourceOverflow)?;
        // Product construction encodes the challenger and the Core cache
        // decodes one independently owned challenger before publication.
        let challenger_encode_and_cache = challenger_output
            .checked_mul(2)
            .ok_or(IntelligenceError::ResourceOverflow)?;

        [
            source,
            expanded_sequences,
            accepted_index,
            discovered_index,
            discovered_support,
            root_sets,
            score_index,
            score_order,
            self.knowledge.active_revision_resident_bytes(),
            challenger_output,
            challenger_encode_and_cache,
        ]
        .into_iter()
        .try_fold(0_u64, |total, bytes| {
            total
                .checked_add(bytes)
                .ok_or(IntelligenceError::ResourceOverflow)
        })
    }

    pub(crate) fn stage_completed_knowledge_compilation(
        &self,
        compilation: ExecutedKnowledgeCompilation,
        receipt: &InvestmentReceipt,
        settlement: InvestmentSettlement,
    ) -> Result<IntelligenceTransition, IntelligenceError> {
        if compilation.base_revision != self.revision
            || receipt.decision() != compilation.producer
            || settlement.decision() != compilation.producer
            || receipt.tag() != InvestmentTag::Consolidate
            || receipt.issued_under() != self.revision
            || settlement.outcome() != InvestmentOutcome::Completed
        {
            return Err(IntelligenceError::InvalidReference);
        }
        let mut knowledge = Vec::with_capacity(usize::from(compilation.challenger.is_some()));
        if let Some(challenger) = compilation.challenger {
            let work = KnowledgeWork::verification(self.revision, &challenger)?;
            if !self.knowledge.contains(work.id()) {
                self.knowledge
                    .ensure_proposal_capacity(&work.challenger, self.limits)?;
                knowledge.push(KnowledgeUpdate::Propose(work.challenger));
            }
        }
        self.stage(
            SettlementFrame::observations(std::slice::from_ref(receipt), &[settlement], &[], &[])
                .with_knowledge_updates(&knowledge),
        )
    }

    pub(crate) fn pending_knowledge_compilation(
        &self,
    ) -> Result<Option<(KnowledgeWork, DecisionId)>, IntelligenceError> {
        let Some(challenger) = self.knowledge.pending_compilation() else {
            return Ok(None);
        };
        let producer = self
            .experience
            .latest_completed_consolidation()
            .ok_or(IntelligenceError::InvalidReference)?;
        let work = KnowledgeWork::from_challenger(self.revision, challenger)?;
        Ok((!work.obligations().is_empty()).then_some((work, producer)))
    }

    pub(crate) fn open_knowledge_verification(
        &self,
        work: KnowledgeWork,
        receipts: &[InvestmentReceipt],
        reserved: ResourceVector,
    ) -> Result<(IntelligenceTransition, OpenedKnowledgeWork), IntelligenceError> {
        if work.base_revision != self.revision {
            return Err(IntelligenceError::StaleTransition);
        }
        if !valid_verification_settlement_batch(&work, receipts, reserved, None) {
            return Err(IntelligenceError::InvalidSettlement);
        }
        let update = KnowledgeUpdate::OpenVerification {
            work,
            receipts: receipts.to_vec().into_boxed_slice(),
            reserved,
        };
        let transition = self.stage(SettlementFrame::knowledge(&[update]))?;
        let opened = transition
            .knowledge
            .as_ref()
            .and_then(|knowledge| knowledge.opened_verification(transition.revision))
            .ok_or(IntelligenceError::InvalidKnowledge)?;
        Ok((transition, opened))
    }

    pub(crate) fn reject_knowledge_work(
        &self,
        work: KnowledgeWork,
        failure: KnowledgePlanningFailure,
    ) -> Result<IntelligenceTransition, IntelligenceError> {
        if work.base_revision != self.revision {
            return Err(IntelligenceError::StaleTransition);
        }
        let KnowledgePlanningFailure::WitnessUnavailable(obligation) = failure;
        if !work
            .obligations()
            .iter()
            .any(|expected| expected.subject() == obligation)
        {
            return Err(IntelligenceError::InvalidReference);
        }
        let id = work.id();
        let invalidation = KnowledgeUpdate::Invalidate {
            challenger: id,
            reason: super::knowledge::KnowledgeInvalidationReason::Witness,
        };
        if self.knowledge.contains(id) {
            self.stage(SettlementFrame::knowledge(&[invalidation]))
        } else {
            self.stage(SettlementFrame::knowledge(&[
                KnowledgeUpdate::Propose(work.challenger),
                invalidation,
            ]))
        }
    }

    pub(crate) fn opened_knowledge_verification(&self) -> Option<OpenedKnowledgeWork> {
        self.knowledge.opened_verification(self.revision)
    }

    pub(crate) fn settle_opened_knowledge_verification(
        &self,
        opened: &OpenedKnowledgeWork,
        report: KnowledgeVerificationReport<'_>,
    ) -> Result<IntelligenceTransition, IntelligenceError> {
        if opened.opened_revision != self.revision
            || !self.knowledge.matches_opened_verification(opened)
        {
            return Err(IntelligenceError::StaleTransition);
        }
        match report {
            KnowledgeVerificationReport::Settled(settlements) => {
                if !valid_verification_settlement_batch(
                    opened.work(),
                    opened.receipts(),
                    opened.reserved_resources(),
                    Some(settlements),
                ) {
                    return Err(IntelligenceError::InvalidSettlement);
                }
                self.stage(
                    SettlementFrame::observations(opened.receipts(), settlements, &[], &[])
                        .with_knowledge_updates(&[KnowledgeUpdate::CompleteVerification(
                            opened.id(),
                        )])
                        .with_opened_verification(opened),
                )
            }
            KnowledgeVerificationReport::Interrupted => {
                let settlements = opened
                    .receipts()
                    .iter()
                    .map(|receipt| {
                        InvestmentSettlement::new(
                            receipt.decision(),
                            super::causal::InvestmentOutcome::VerifiedUnknown,
                            receipt.resources(),
                        )
                    })
                    .collect::<Vec<_>>();
                self.stage(
                    SettlementFrame::observations(opened.receipts(), &settlements, &[], &[])
                        .with_knowledge_updates(&[KnowledgeUpdate::InterruptVerification(
                            opened.id(),
                        )])
                        .with_opened_verification(opened),
                )
            }
        }
    }

    pub(super) fn validate_knowledge_shadow_opened(
        &self,
        opened: &KnowledgeShadowOpened,
    ) -> Result<(), IntelligenceError> {
        if self.revision != opened.opened_revision
            || !self
                .experience()
                .shadow_campaign(opened.campaign())
                .is_some_and(|campaign| {
                    campaign.lifecycle() == super::causal::ShadowCampaignLifecycle::Open
                        && campaign.specification() == opened.campaign_specification()
                })
        {
            return Err(IntelligenceError::StaleTransition);
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the pure shadow planner keeps all paired-counterfactual bindings in one auditable construction"
    )]
    pub(crate) fn plan_knowledge_shadow(
        &self,
        restart_checkpoint: CheckpointDigest,
        context_digest: SubjectId,
        roots: &[KnowledgeShadowRootFact],
        support_facts: &[KnowledgeShadowSupportFact],
        per_arm_allowance: ResourceVector,
    ) -> Result<Option<KnowledgeShadowPlan>, IntelligenceError> {
        let support_limit = self.limits.maximum_investments.saturating_mul(64);
        if roots.is_empty()
            || roots.len() > self.limits.maximum_opportunities
            || support_facts.len() > support_limit
            || per_arm_allowance.verification_requests == 0
            || roots.iter().enumerate().any(|(index, fact)| {
                roots[..index]
                    .iter()
                    .any(|prior| prior.root() == fact.root())
            })
            || support_facts.iter().enumerate().any(|(index, fact)| {
                support_facts[..index]
                    .iter()
                    .any(|prior| prior.attempt() == fact.attempt())
            })
        {
            return Err(IntelligenceError::InvalidKnowledge);
        }
        if self
            .experience()
            .shadow_campaigns()
            .any(|campaign| campaign.lifecycle() == ShadowCampaignLifecycle::Open)
        {
            return Ok(None);
        }
        let Some((challenger_id, challenger)) = self.knowledge.eligible_shadow() else {
            return Ok(None);
        };
        let subject_obligation = challenger
            .obligations()
            .first()
            .copied()
            .ok_or(IntelligenceError::InvalidKnowledge)?;
        let subject_operator = challenger
            .operator_for(subject_obligation)
            .ok_or(IntelligenceError::InvalidKnowledge)?
            .symbol()
            .to_vec()
            .into_boxed_slice();
        let supporting_attempts = challenger
            .obligations()
            .iter()
            .flat_map(|obligation| {
                challenger
                    .supporting_attempts(*obligation)
                    .unwrap_or_default()
                    .iter()
                    .copied()
            })
            .collect::<BTreeSet<_>>();
        let support_claims = support_facts
            .iter()
            .copied()
            .filter(|fact| supporting_attempts.contains(&fact.attempt()))
            .map(KnowledgeShadowSupportFact::claim)
            .collect::<BTreeSet<_>>();
        let Some((root_index, root)) = roots
            .iter()
            .copied()
            .enumerate()
            .filter(|(_, fact)| !support_claims.contains(&fact.claim()))
            .map(|(index, fact)| (fact.selection_key(index), index, fact))
            .min_by_key(|(key, _, _)| *key)
            .map(|(_, index, fact)| (index, fact.root()))
        else {
            return Ok(None);
        };
        let product = SubjectId::new(challenger.product().identity());
        let random_stream = knowledge_shadow_token(
            b"reflex-knowledge-shadow-random-v2\0",
            self.revision,
            restart_checkpoint,
            context_digest,
            product,
            root,
            &subject_operator,
            &supporting_attempts,
            per_arm_allowance,
            u64::try_from(root_index).map_err(|_| IntelligenceError::CapacityExceeded)?,
        );
        let scheduling_token = knowledge_shadow_token(
            b"reflex-knowledge-shadow-scheduling-v2\0",
            self.revision,
            restart_checkpoint,
            context_digest,
            product,
            root,
            &subject_operator,
            &supporting_attempts,
            per_arm_allowance,
            u64::try_from(root_index).map_err(|_| IntelligenceError::CapacityExceeded)?,
        );
        let campaign = super::causal::ShadowCampaignSpec::new(
            restart_checkpoint,
            product,
            per_arm_allowance,
            random_stream,
            [
                super::forecast::ForecastAxis::UsefulDescendants,
                super::forecast::ForecastAxis::CompressionValue,
            ],
        )?;
        Ok(Some(KnowledgeShadowPlan {
            base_revision: self.revision,
            challenger: challenger_id,
            campaign,
            root,
            scheduling_token,
            subject_operator,
            supporting_attempts: supporting_attempts
                .into_iter()
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            treatment: challenger.pinned_revision(),
            control: self.knowledge.pinned_revision_arc(),
        }))
    }

    pub(crate) fn open_knowledge_shadow(
        &self,
        plan: KnowledgeShadowPlan,
    ) -> Result<(IntelligenceTransition, KnowledgeShadowOpened), IntelligenceError> {
        if plan.base_revision != self.revision
            || self
                .experience()
                .shadow_campaigns()
                .any(|campaign| campaign.lifecycle() == ShadowCampaignLifecycle::Open)
            || self
                .knowledge
                .eligible_shadow()
                .is_none_or(|(id, _)| id != plan.challenger)
        {
            return Err(IntelligenceError::StaleTransition);
        }
        let transition = self.stage(SettlementFrame::observations(
            &[],
            &[],
            &[],
            &[ShadowUpdate::Open(plan.campaign.clone())],
        ))?;
        let opened = plan.into_opened(transition.revision);
        Ok((transition, opened))
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "consuming the opened token makes one-shot shadow settlement explicit"
    )]
    pub(crate) fn settle_knowledge_shadow(
        &self,
        opened: KnowledgeShadowOpened,
        report: KnowledgeShadowReport,
    ) -> Result<IntelligenceTransition, IntelligenceError> {
        self.validate_knowledge_shadow_opened(&opened)?;
        match report {
            KnowledgeShadowReport::Invalidated(reason) => {
                self.stage(SettlementFrame::empty().with_observations(
                    &[],
                    &[],
                    &[],
                    &[ShadowUpdate::invalidate(opened.campaign(), reason)],
                ))
            }
            KnowledgeShadowReport::Paired { treatment, control } => {
                let allowance = opened.campaign.resources();
                if treatment.arm() != ShadowArm::Treatment
                    || control.arm() != ShadowArm::Control
                    || !treatment.actual_resources().fits_within(allowance)
                    || !control.actual_resources().fits_within(allowance)
                {
                    return Err(IntelligenceError::InvalidSettlement);
                }
                let updates = [
                    shadow_arm_outcome(&opened, treatment)?,
                    shadow_arm_outcome(&opened, control)?,
                ];
                self.stage(
                    SettlementFrame::empty()
                        .with_observations(&[], &[], &[], &updates)
                        .with_knowledge_updates(&[KnowledgeUpdate::EvaluatePromotion(
                            opened.challenger,
                        )]),
                )
            }
        }
    }

    pub(crate) const fn runtime_policy_revision(&self) -> RuntimePolicyRevision {
        self.policy.active()
    }

    pub(crate) fn completed_candidate_production(&self, decision: DecisionId) -> bool {
        self.experience.completed_candidate_production(decision)
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    pub(crate) fn runtime_policy_identity(&self) -> [u8; 32] {
        self.policy.identity()
    }

    pub(crate) fn runtime_policy_challenger(&self, sequence: u64) -> Option<RuntimePolicyRevision> {
        self.policy.challenger_at(sequence)
    }

    pub(crate) fn into_legacy_runtime_policy(
        self,
        policy: &RuntimePolicyState,
    ) -> Result<Self, IntelligenceError> {
        if !self.policy_import_eligible || self.policy != RuntimePolicyState::bootstrap() {
            return Err(IntelligenceError::InvalidPolicy);
        }
        let policy = RuntimePolicyState::decode(&policy.encode())
            .map_err(|_| IntelligenceError::InvalidPolicy)?;
        Ok(Self::from_components(
            self.limits,
            self.ecology,
            self.experience,
            self.knowledge,
            policy,
            false,
            self.legacy_ftrl_import_eligible,
            self.legacy_knowledge_import_eligible,
        ))
    }

    pub(crate) fn into_legacy_ftrl(
        self,
        conversion: FtrlConversionView<'_>,
    ) -> Result<Self, IntelligenceError> {
        if !self.legacy_ftrl_import_eligible {
            return Err(IntelligenceError::IncompatibleRevision);
        }
        if !self.ecology.is_empty() {
            return Err(IntelligenceError::InvalidMandate);
        }
        let source_lineage = conversion.content_identity();
        let model = CompactModel::import_ftrl(conversion)?;
        let role = imported_ftrl_role(source_lineage, model.content_identity());
        let mandate = SpecialistMandate::for_routing_families(
            role,
            IMPORTED_FTRL_AXES,
            [OpportunityKind::Candidate],
            [OpportunitySpec::candidate_schema()],
            std::iter::empty(),
        )?;
        let specialist = SpecialistRevision::new(mandate, model)?;
        let mut ecology = self.ecology;
        ecology.apply(&EcologyEdit::Spawn(specialist), self.limits)?;
        Ok(Self::from_components(
            self.limits,
            ecology,
            self.experience,
            self.knowledge,
            self.policy,
            self.policy_import_eligible,
            false,
            self.legacy_knowledge_import_eligible,
        ))
    }

    pub(crate) fn into_legacy_learning(
        self,
        conversion: Option<FtrlConversionView<'_>>,
    ) -> Result<Self, IntelligenceError> {
        match conversion {
            Some(conversion) => self.into_legacy_ftrl(conversion),
            None => self.without_legacy_ftrl(),
        }
    }

    fn without_legacy_ftrl(self) -> Result<Self, IntelligenceError> {
        if !self.legacy_ftrl_import_eligible || !self.ecology.is_empty() {
            return Err(IntelligenceError::IncompatibleRevision);
        }
        Ok(Self::from_components(
            self.limits,
            self.ecology,
            self.experience,
            self.knowledge,
            self.policy,
            self.policy_import_eligible,
            false,
            self.legacy_knowledge_import_eligible,
        ))
    }

    pub(crate) fn into_legacy_knowledge(
        self,
        legacy: &KnowledgeState,
    ) -> Result<Self, IntelligenceError> {
        if !self.legacy_knowledge_import_eligible {
            return Err(IntelligenceError::IncompatibleRevision);
        }
        let mut knowledge = self.knowledge;
        knowledge.import_legacy_active(legacy, self.limits)?;
        Ok(Self::from_components(
            self.limits,
            self.ecology,
            self.experience,
            knowledge,
            self.policy,
            self.policy_import_eligible,
            self.legacy_ftrl_import_eligible,
            false,
        ))
    }

    pub(crate) const fn limits(&self) -> IntelligenceLimits {
        self.limits
    }

    pub(crate) const fn active_revision(&self) -> [u8; 32] {
        self.revision
    }

    pub(crate) fn resident_bytes(&self) -> u64 {
        let resident = std::mem::size_of::<Self>()
            .saturating_add(self.ecology.heap_bytes())
            .saturating_add(self.experience.heap_bytes())
            .saturating_add(self.knowledge.heap_bytes())
            .saturating_add(self.policy.heap_bytes())
            .saturating_add(self.checkpoint_bytes.capacity());
        u64::try_from(resident).unwrap_or(u64::MAX)
    }

    pub(crate) fn transition_resident_bytes(
        &self,
        maximum_receipts: usize,
        maximum_settlements: usize,
        maximum_consequences: usize,
        training: Option<NativeTrainingBudget>,
    ) -> u64 {
        let receipt_bytes =
            maximum_receipts.saturating_mul(std::mem::size_of::<InvestmentReceipt>());
        let settlement_bytes =
            maximum_settlements.saturating_mul(std::mem::size_of::<InvestmentSettlement>());
        let consequence_bytes =
            maximum_consequences.saturating_mul(std::mem::size_of::<ConsequenceEdge>());
        let observation_bytes = receipt_bytes
            .saturating_add(settlement_bytes)
            .saturating_add(consequence_bytes);
        let causal_delta_and_checkpoint = observation_bytes.saturating_mul(4);
        let decision_index = maximum_receipts
            .saturating_mul(std::mem::size_of::<DecisionId>())
            .saturating_mul(2);
        let training_bytes = training.map_or(0, |budget| {
            budget
                .scratch_bytes_for_history(
                    self.experience
                        .receipt_count()
                        .saturating_add(maximum_receipts),
                )
                .saturating_add(NativeTrainingBudget::maximum_output_bytes().saturating_mul(3))
                .saturating_add(
                    u64::try_from(self.ecology.staging_clone_bytes()).unwrap_or(u64::MAX),
                )
        });
        let policy_bytes = RuntimePolicyState::maximum_encoded_len()
            .saturating_add(std::mem::size_of::<RuntimePolicyState>());
        u64::try_from(causal_delta_and_checkpoint)
            .unwrap_or(u64::MAX)
            .saturating_add(u64::try_from(decision_index).unwrap_or(u64::MAX))
            .saturating_add(training_bytes)
            .saturating_add(u64::try_from(policy_bytes).unwrap_or(u64::MAX))
            .saturating_add(16 * 1024)
    }

    pub(crate) fn native_training_scratch_bytes(
        &self,
        budget: NativeTrainingBudget,
        additional_receipts: usize,
    ) -> u64 {
        budget.scratch_bytes_for_history(
            self.experience
                .receipt_count()
                .saturating_add(additional_receipts),
        )
    }

    pub(crate) fn shadow_interruption_transition_resident_bytes(&self, update_count: usize) -> u64 {
        self.transition_resident_bytes(0, 0, 0, None)
            .saturating_add(u64::try_from(self.experience.heap_bytes()).unwrap_or(u64::MAX))
            .saturating_add(
                u64::try_from(update_count)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(std::mem::size_of::<ShadowUpdate>() as u64),
            )
    }

    pub(crate) fn knowledge_open_transition_resident_bytes(
        &self,
        work: &KnowledgeWork,
        receipts: &[InvestmentReceipt],
        reserved: ResourceVector,
    ) -> Result<u64, IntelligenceError> {
        if work.base_revision != self.revision
            || !valid_verification_settlement_batch(work, receipts, reserved, None)
        {
            return Err(IntelligenceError::InvalidSettlement);
        }
        let encoded_len = 8_usize
            .saturating_add(1)
            .saturating_add(32)
            .saturating_add(work.challenger.canonical_encoded_len())
            .saturating_add(8)
            .saturating_add(receipts.iter().fold(0_usize, |bytes, receipt| {
                bytes.saturating_add(receipt.canonical_encoded_len())
            }))
            .saturating_add(5 * 8);
        Ok(self
            .transition_resident_bytes(0, 0, 0, None)
            .saturating_add(u64::try_from(self.knowledge.heap_bytes()).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(work.heap_bytes()).unwrap_or(u64::MAX))
            .saturating_add(
                u64::try_from(
                    receipts
                        .len()
                        .saturating_mul(std::mem::size_of::<InvestmentReceipt>()),
                )
                .unwrap_or(u64::MAX),
            )
            .saturating_add(
                u64::try_from(encoded_len)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(3),
            ))
    }

    pub(crate) fn knowledge_settlement_transition_resident_bytes(
        &self,
        opened: &OpenedKnowledgeWork,
        settlement_count: usize,
    ) -> Result<u64, IntelligenceError> {
        if opened.opened_revision != self.revision
            || !self.knowledge.matches_opened_verification(opened)
            || settlement_count != opened.receipts().len()
        {
            return Err(IntelligenceError::StaleTransition);
        }
        Ok(self
            .transition_resident_bytes(opened.receipts().len(), settlement_count, 0, None)
            .saturating_add(u64::try_from(self.knowledge.heap_bytes()).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(opened.heap_bytes()).unwrap_or(u64::MAX))
            .saturating_add(3 * 128))
    }

    pub(crate) fn knowledge_shadow_settlement_transition_resident_bytes(
        &self,
        opened: &KnowledgeShadowOpened,
    ) -> Result<u64, IntelligenceError> {
        self.validate_knowledge_shadow_opened(opened)?;
        let rebase_bytes = self.knowledge.promotion_rebase_bytes(opened.challenger)?;
        Ok(self
            .transition_resident_bytes(0, 0, 0, None)
            .saturating_add(u64::try_from(self.knowledge.heap_bytes()).unwrap_or(u64::MAX))
            .saturating_add(u64::try_from(rebase_bytes).unwrap_or(u64::MAX)))
    }

    pub(crate) fn checkpoint(&self) -> IntelligenceCheckpoint {
        IntelligenceCheckpoint {
            bytes: Arc::clone(&self.checkpoint_bytes),
        }
    }

    pub(crate) fn checkpoint_bytes(&self) -> &[u8] {
        &self.checkpoint_bytes
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    pub(crate) fn legacy_v13_checkpoint_for_test(&self) -> IntelligenceCheckpoint {
        let roots = legacy_v13_component_roots(
            &self.ecology,
            &self.experience,
            &self.knowledge,
            &self.policy,
        );
        let revision = revision_digest(self.limits, roots);
        let (bytes, _) = segmented_checkpoint(
            CheckpointWire::legacy_v13(),
            self.limits,
            &self.ecology,
            &self.experience,
            &self.knowledge,
            &self.policy,
            roots,
            revision,
        );
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }

    #[cfg(test)]
    pub(crate) fn legacy_v14_checkpoint_for_test(&self) -> IntelligenceCheckpoint {
        assert!(
            self.knowledge.opened_verification(self.revision).is_none(),
            "v14 migration fixtures cannot erase pending Verification work"
        );
        let roots = legacy_v14_component_roots(
            &self.ecology,
            &self.experience,
            &self.knowledge,
            &self.policy,
        );
        let revision = revision_digest(self.limits, roots);
        let (bytes, _) = segmented_checkpoint(
            CheckpointWire::legacy_v14(),
            self.limits,
            &self.ecology,
            &self.experience,
            &self.knowledge,
            &self.policy,
            roots,
            revision,
        );
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }

    #[cfg(test)]
    pub(crate) fn legacy_v15_checkpoint_for_test(&self) -> IntelligenceCheckpoint {
        let roots = legacy_v15_component_roots(
            &self.ecology,
            &self.experience,
            &self.knowledge,
            &self.policy,
        );
        let revision = revision_digest(self.limits, roots);
        let (bytes, _) = segmented_checkpoint(
            CheckpointWire::legacy_v15(),
            self.limits,
            &self.ecology,
            &self.experience,
            &self.knowledge,
            &self.policy,
            roots,
            revision,
        );
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }

    #[cfg(test)]
    pub(crate) fn legacy_v16_checkpoint_for_test(&self) -> IntelligenceCheckpoint {
        let (bytes, _) = segmented_checkpoint(
            CheckpointWire::legacy_v16(),
            self.limits,
            &self.ecology,
            &self.experience,
            &self.knowledge,
            &self.policy,
            self.roots,
            self.revision,
        );
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }

    #[cfg(feature = "internal-experiments")]
    pub(crate) fn hostile_domain_invalid_knowledge_checkpoint_for_test(
        &self,
    ) -> Result<IntelligenceCheckpoint, IntelligenceError> {
        let mut knowledge = self.knowledge.clone();
        if !knowledge.poison_predecessor_artifact_for_test() {
            return Err(IntelligenceError::InvalidKnowledge);
        }
        let hostile = Self::from_components(
            self.limits,
            self.ecology.clone(),
            self.experience.clone(),
            knowledge,
            self.policy.clone(),
            false,
            false,
            false,
        );
        Self::restore(hostile.checkpoint().as_bytes())?;
        Ok(hostile.checkpoint())
    }

    /// Builds one authenticated current Core for the repository's frozen
    /// learned-system ablations. The treatment changes only the selected
    /// component: Model Ecology comes from an exact empty Bootstrap Core with
    /// the same Domain semantic identity, or executable/promotable Derived
    /// Operator knowledge is removed.
    #[cfg(feature = "internal-experiments")]
    pub(crate) fn treatment_ablation_for_experiment(
        &self,
        semantic_identity: &[u8],
        model_template: Option<(&Self, &[u8])>,
        without_derived_operators: bool,
    ) -> Result<Self, IntelligenceError> {
        let has_open_work = |core: &Self| {
            core.knowledge.opened_verification(core.revision).is_some()
                || core
                    .experience
                    .view()
                    .shadow_campaigns()
                    .any(|campaign| campaign.lifecycle() == ShadowCampaignLifecycle::Open)
        };
        if has_open_work(self)
            || model_template.is_some_and(|(template, _)| has_open_work(template))
        {
            return Err(IntelligenceError::StaleTransition);
        }
        if model_template.is_some_and(|(template, template_identity)| {
            template_identity != semantic_identity || !template.ecology.is_empty()
        }) {
            return Err(IntelligenceError::InvalidModel);
        }
        let ecology =
            model_template.map_or_else(|| self.ecology.clone(), |(core, _)| core.ecology.clone());
        if !ecology.fits_limits(self.limits) {
            return Err(IntelligenceError::InvalidModel);
        }
        let knowledge = if without_derived_operators {
            self.knowledge.without_derived_operators_for_experiment()
        } else {
            self.knowledge.clone()
        };
        let ablated = Self::from_components(
            self.limits,
            ecology,
            self.experience.clone(),
            knowledge,
            self.policy.clone(),
            false,
            false,
            false,
        );
        Self::restore(ablated.checkpoint().as_bytes())
    }

    #[cfg(test)]
    pub(super) fn legacy_v10_checkpoint_for_test(&self) -> IntelligenceCheckpoint {
        assert!(
            self.experience
                .aligned()
                .all(|(receipt, _)| receipt.forecasts().len() <= 1),
            "the v10 wire format retained at most one forecast per receipt"
        );
        let roots = legacy_v11_component_roots(&self.ecology, &self.experience, &self.knowledge);
        let revision = legacy_v12_revision_digest(self.limits, roots);
        let (bytes, _) = segmented_checkpoint(
            CheckpointWire::legacy(LEGACY_V10_SEGMENTED_MAGIC),
            self.limits,
            &self.ecology,
            &self.experience,
            &self.knowledge,
            &self.policy,
            roots,
            revision,
        );
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }

    #[cfg(test)]
    pub(super) fn legacy_v11_checkpoint_for_test(&self) -> IntelligenceCheckpoint {
        assert!(
            self.experience
                .aligned()
                .all(|(receipt, _)| receipt.preference_priority() == 0),
            "v11 migration fixtures cannot erase an explicit caller preference"
        );
        let roots = legacy_v11_component_roots(&self.ecology, &self.experience, &self.knowledge);
        let revision = legacy_v12_revision_digest(self.limits, roots);
        let (bytes, _) = segmented_checkpoint(
            CheckpointWire::legacy(LEGACY_V11_SEGMENTED_MAGIC),
            self.limits,
            &self.ecology,
            &self.experience,
            &self.knowledge,
            &self.policy,
            roots,
            revision,
        );
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    pub(crate) fn legacy_v12_checkpoint_for_test(&self) -> IntelligenceCheckpoint {
        assert_eq!(
            self.policy,
            RuntimePolicyState::bootstrap(),
            "v12 migration fixtures cannot erase retained Runtime Policy state"
        );
        let roots = legacy_v12_component_roots(&self.ecology, &self.experience, &self.knowledge);
        let revision = legacy_v12_revision_digest(self.limits, roots);
        let (bytes, _) = segmented_checkpoint(
            CheckpointWire::legacy_v12(),
            self.limits,
            &self.ecology,
            &self.experience,
            &self.knowledge,
            &self.policy,
            roots,
            revision,
        );
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }

    #[cfg(test)]
    pub(super) fn legacy_v9_checkpoint_for_test(&self) -> IntelligenceCheckpoint {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RFIC\x09");
        bytes.extend_from_slice(&legacy_v9_revision_digest(
            self.limits,
            &self.ecology,
            &self.experience,
            &self.knowledge,
        ));
        for value in current_limit_values(self.limits) {
            bytes.extend_from_slice(&(value as u64).to_le_bytes());
        }
        self.ecology.encode_canonical(&mut bytes);
        self.experience.encode_legacy_v11(&mut bytes);
        self.knowledge.encode_legacy_v13(&mut bytes);
        let checksum: [u8; 32] = Sha256::digest(&bytes).into();
        bytes.extend_from_slice(&checksum);
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }

    #[cfg(test)]
    pub(super) fn legacy_v8_checkpoint_for_test(&self) -> IntelligenceCheckpoint {
        let mut bytes = Vec::with_capacity(6 * std::mem::size_of::<u64>() + 69);
        bytes.extend_from_slice(b"RFIC\x08");
        bytes.extend_from_slice(&legacy_v8_revision_digest(
            self.limits,
            &self.ecology,
            &self.experience,
            &self.knowledge,
        ));
        for value in legacy_limit_values(self.limits) {
            bytes.extend_from_slice(&(value as u64).to_le_bytes());
        }
        self.ecology.encode_canonical(&mut bytes);
        self.experience.encode_legacy_v11(&mut bytes);
        self.knowledge.encode_legacy_v13(&mut bytes);
        let checksum: [u8; 32] = Sha256::digest(&bytes).into();
        bytes.extend_from_slice(&checksum);
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }

    #[cfg(test)]
    pub(super) fn legacy_v7_checkpoint_for_test(&self) -> IntelligenceCheckpoint {
        let mut bytes = Vec::with_capacity(6 * std::mem::size_of::<u64>() + 69);
        bytes.extend_from_slice(b"RFIC\x07");
        bytes.extend_from_slice(
            &legacy_v7_revision_digest(
                self.limits,
                &self.ecology,
                &self.experience,
                &self.knowledge,
            )
            .expect("a v7 test fixture cannot contain v8-only Knowledge state"),
        );
        for value in [
            self.limits.maximum_opportunities,
            self.limits.maximum_investments,
            self.limits.maximum_bids,
            self.limits.maximum_forecast_cells,
            self.limits.maximum_specialists,
            self.limits.maximum_model_bytes,
        ] {
            bytes.extend_from_slice(&(value as u64).to_le_bytes());
        }
        self.ecology.encode_canonical(&mut bytes);
        self.experience.encode_legacy_v11(&mut bytes);
        self.knowledge
            .encode_legacy_v7(&mut bytes)
            .expect("a v7 test fixture cannot contain v8-only Knowledge state");
        let checksum: [u8; 32] = Sha256::digest(&bytes).into();
        bytes.extend_from_slice(&checksum);
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }

    #[cfg(test)]
    pub(super) fn legacy_v6_checkpoint_for_test(&self) -> IntelligenceCheckpoint {
        let mut bytes = Vec::with_capacity(6 * std::mem::size_of::<u64>() + 69);
        bytes.extend_from_slice(b"RFIC\x06");
        bytes.extend_from_slice(
            &legacy_v6_revision_digest(
                self.limits,
                &self.ecology,
                &self.experience,
                &self.knowledge,
            )
            .expect("a v6 test fixture cannot contain opaque product bytes"),
        );
        for value in [
            self.limits.maximum_opportunities,
            self.limits.maximum_investments,
            self.limits.maximum_bids,
            self.limits.maximum_forecast_cells,
            self.limits.maximum_specialists,
            self.limits.maximum_model_bytes,
        ] {
            bytes.extend_from_slice(&(value as u64).to_le_bytes());
        }
        self.ecology.encode_canonical(&mut bytes);
        self.experience.encode_legacy_v11(&mut bytes);
        self.knowledge
            .encode_legacy_v6(&mut bytes)
            .expect("a v6 test fixture cannot contain opaque product bytes");
        let checksum: [u8; 32] = Sha256::digest(&bytes).into();
        bytes.extend_from_slice(&checksum);
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }
}

fn knowledge_compiler_subject(recipe: &[u8]) -> SubjectId {
    let semantic_identity = b"reflex-knowledge-compiler-action-v1";
    let mut digest = Sha256::new();
    digest.update(b"reflex-artifact-v1\0");
    digest.update((semantic_identity.len() as u64).to_le_bytes());
    digest.update(semantic_identity);
    digest.update(recipe);
    SubjectId::new(digest.finalize().into())
}

pub(crate) struct PreparedNativeEcologyPlan {
    base_revision: [u8; 32],
    evidence_root: [u8; 32],
    receipt_count: usize,
    settlement_count: usize,
    consequence_count: usize,
    shadow_count: usize,
    budget: NativeTrainingBudget,
    scratch_bytes: u64,
    plan: NativeEcologyPlan,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum KnowledgeCompilationError<E> {
    Source(E),
    Intelligence(IntelligenceError),
}

pub(crate) struct PreparedKnowledgeCompilation {
    base_revision: [u8; 32],
    broad_compiler: SubjectId,
    focused_compiler: SubjectId,
    specifications: [OperationalActionSpec; 2],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct KnowledgeCompilationShape {
    observations: u64,
    identity_bytes: u64,
    steps: u64,
    step_bytes: u64,
}

impl KnowledgeCompilationShape {
    pub(crate) fn observe(
        &mut self,
        identity: &[u8],
        steps: &[Vec<u8>],
    ) -> Result<(), IntelligenceError> {
        self.observations = self
            .observations
            .checked_add(1)
            .ok_or(IntelligenceError::ResourceOverflow)?;
        self.identity_bytes = self
            .identity_bytes
            .checked_add(
                u64::try_from(identity.len()).map_err(|_| IntelligenceError::ResourceOverflow)?,
            )
            .ok_or(IntelligenceError::ResourceOverflow)?;
        self.steps = self
            .steps
            .checked_add(
                u64::try_from(steps.len()).map_err(|_| IntelligenceError::ResourceOverflow)?,
            )
            .ok_or(IntelligenceError::ResourceOverflow)?;
        for step in steps {
            self.step_bytes = self
                .step_bytes
                .checked_add(
                    u64::try_from(step.len()).map_err(|_| IntelligenceError::ResourceOverflow)?,
                )
                .ok_or(IntelligenceError::ResourceOverflow)?;
        }
        Ok(())
    }
}

fn vector_capacity_bytes<T>(count: u64) -> Result<u64, IntelligenceError> {
    let header = u64::try_from(std::mem::size_of::<Vec<T>>())
        .map_err(|_| IntelligenceError::ResourceOverflow)?;
    let element =
        u64::try_from(std::mem::size_of::<T>()).map_err(|_| IntelligenceError::ResourceOverflow)?;
    count
        .checked_mul(element)
        .and_then(|bytes| bytes.checked_add(header))
        .ok_or(IntelligenceError::ResourceOverflow)
}

fn btree_capacity_bytes<T>(count: u64) -> Result<u64, IntelligenceError> {
    let header = u64::try_from(std::mem::size_of::<BTreeMap<T, u8>>())
        .map_err(|_| IntelligenceError::ResourceOverflow)?;
    let entry = u64::try_from(std::mem::size_of::<T>())
        .map_err(|_| IntelligenceError::ResourceOverflow)?
        // Parent, left/right child, and allocator/padding allowance per entry
        // conservatively dominate the amortized B-tree node metadata.
        .checked_add(
            u64::try_from(4 * std::mem::size_of::<usize>())
                .map_err(|_| IntelligenceError::ResourceOverflow)?,
        )
        .ok_or(IntelligenceError::ResourceOverflow)?;
    count
        .checked_mul(entry)
        .and_then(|bytes| bytes.checked_add(header))
        .ok_or(IntelligenceError::ResourceOverflow)
}

pub(crate) struct ExecutedKnowledgeCompilation {
    base_revision: [u8; 32],
    producer: DecisionId,
    challenger: Option<ConsolidationChallenger>,
}

impl PreparedKnowledgeCompilation {
    pub(crate) const fn specifications(&self) -> &[OperationalActionSpec] {
        &self.specifications
    }

    pub(crate) fn execute<E>(
        &self,
        intelligence: &IntelligenceCore,
        producer: DecisionId,
        compiler: SubjectId,
        roots: &[[u8; 32]],
        pareto: &[[u8; 32]],
        mine_derivations: impl FnOnce() -> Result<Vec<DerivationObservation>, E>,
    ) -> Result<ExecutedKnowledgeCompilation, KnowledgeCompilationError<E>> {
        if intelligence.active_revision() != self.base_revision {
            return Err(KnowledgeCompilationError::Intelligence(
                IntelligenceError::StaleTransition,
            ));
        }
        let consolidation_roots = if compiler == self.focused_compiler {
            pareto
        } else if compiler == self.broad_compiler {
            roots
        } else {
            return Err(KnowledgeCompilationError::Intelligence(
                IntelligenceError::InvalidReference,
            ));
        };
        let observations = mine_derivations().map_err(KnowledgeCompilationError::Source)?;
        let challenger = intelligence
            .knowledge
            .propose_consolidation(
                &observations,
                consolidation_roots.iter().copied(),
                pareto.iter().copied(),
            )
            .filter(|challenger| !challenger.obligations().is_empty());
        Ok(ExecutedKnowledgeCompilation {
            base_revision: self.base_revision,
            producer,
            challenger,
        })
    }
}

impl PreparedNativeEcologyPlan {
    pub(crate) fn identity(&self) -> [u8; 32] {
        self.plan.identity()
    }

    pub(crate) fn resident_bytes(&self) -> u64 {
        u64::try_from(self.plan.resident_bytes()).unwrap_or(u64::MAX)
    }

    pub(crate) fn comparison_resident_bytes(&self) -> u64 {
        self.resident_bytes()
            .saturating_add(self.scratch_bytes)
            .saturating_add(NativeTrainingBudget::maximum_output_bytes())
    }

    pub(crate) const fn scratch_bytes(&self) -> u64 {
        self.scratch_bytes
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SettlementFrame<'a> {
    edits: &'a [EcologyEdit],
    receipts: &'a [InvestmentReceipt],
    settlements: &'a [InvestmentSettlement],
    consequences: &'a [ConsequenceEdge],
    shadows: &'a [ShadowUpdate],
    knowledge: &'a [KnowledgeUpdate],
    policy: Option<&'a PolicyUpdate>,
    native_training: Option<NativeTrainingBudget>,
    prepared_native_training: Option<&'a PreparedNativeEcologyPlan>,
    opened_verification: Option<&'a OpenedKnowledgeWork>,
}

impl<'a> SettlementFrame<'a> {
    pub(crate) const fn empty() -> Self {
        Self {
            edits: &[],
            receipts: &[],
            settlements: &[],
            consequences: &[],
            shadows: &[],
            knowledge: &[],
            policy: None,
            native_training: None,
            prepared_native_training: None,
            opened_verification: None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn edits(edits: &'a [EcologyEdit]) -> Self {
        Self {
            edits,
            receipts: &[],
            settlements: &[],
            consequences: &[],
            shadows: &[],
            knowledge: &[],
            policy: None,
            native_training: None,
            prepared_native_training: None,
            opened_verification: None,
        }
    }

    pub(crate) const fn observations(
        receipts: &'a [InvestmentReceipt],
        settlements: &'a [InvestmentSettlement],
        consequences: &'a [ConsequenceEdge],
        shadows: &'a [ShadowUpdate],
    ) -> Self {
        Self {
            edits: &[],
            receipts,
            settlements,
            consequences,
            shadows,
            knowledge: &[],
            policy: None,
            native_training: None,
            prepared_native_training: None,
            opened_verification: None,
        }
    }

    pub(crate) const fn knowledge(knowledge: &'a [KnowledgeUpdate]) -> Self {
        Self {
            edits: &[],
            receipts: &[],
            settlements: &[],
            consequences: &[],
            shadows: &[],
            knowledge,
            policy: None,
            native_training: None,
            prepared_native_training: None,
            opened_verification: None,
        }
    }

    pub(crate) const fn with_observations(
        mut self,
        receipts: &'a [InvestmentReceipt],
        settlements: &'a [InvestmentSettlement],
        consequences: &'a [ConsequenceEdge],
        shadows: &'a [ShadowUpdate],
    ) -> Self {
        self.receipts = receipts;
        self.settlements = settlements;
        self.consequences = consequences;
        self.shadows = shadows;
        self
    }

    pub(crate) const fn with_knowledge_updates(mut self, knowledge: &'a [KnowledgeUpdate]) -> Self {
        self.knowledge = knowledge;
        self
    }

    const fn with_opened_verification(mut self, opened: &'a OpenedKnowledgeWork) -> Self {
        self.opened_verification = Some(opened);
        self
    }

    #[cfg(test)]
    pub(crate) const fn with_native_training(mut self, budget: NativeTrainingBudget) -> Self {
        self.native_training = Some(budget);
        self
    }

    pub(crate) const fn with_prepared_native_training(
        mut self,
        prepared: &'a PreparedNativeEcologyPlan,
    ) -> Self {
        self.prepared_native_training = Some(prepared);
        self
    }

    pub(crate) const fn with_policy_update(mut self, update: &'a PolicyUpdate) -> Self {
        self.policy = Some(update);
        self
    }
}

#[derive(Debug)]
pub(crate) struct IntelligenceTransition {
    base_revision: [u8; 32],
    ecology: Option<ModelEcology>,
    experience: CausalDelta,
    knowledge: Option<KnowledgeCompiler>,
    policy: Option<RuntimePolicyState>,
    policy_decision: Option<RuntimePolicyDecision>,
    roots: ComponentRoots,
    revision: [u8; 32],
    base_checkpoint: Arc<Vec<u8>>,
    checkpoint_record: Vec<u8>,
    checkpoint_log_root: [u8; 32],
    checkpoint_records: u64,
    #[cfg(test)]
    ecology_items_cloned: usize,
    #[cfg(test)]
    knowledge_items_cloned: usize,
}

pub(crate) struct PreparedIntelligenceCommit {
    transition: IntelligenceTransition,
    checkpoint: IntelligenceCheckpoint,
}

#[derive(Clone, Copy)]
pub(crate) struct ProposedIntelligenceView<'a> {
    transition: &'a IntelligenceTransition,
    base: &'a IntelligenceCore,
    checkpoint: &'a IntelligenceCheckpoint,
}

impl ProposedIntelligenceView<'_> {
    pub(crate) fn checkpoint_bytes(&self) -> &[u8] {
        self.checkpoint.as_bytes()
    }

    #[cfg(test)]
    pub(crate) const fn revision(&self) -> [u8; 32] {
        self.transition.revision
    }

    pub(crate) const fn model_ecology_identity(&self) -> [u8; 32] {
        self.transition.roots.ecology
    }

    pub(crate) fn knowledge_product_identity(&self) -> [u8; 32] {
        self.transition
            .knowledge
            .as_ref()
            .unwrap_or(&self.base.knowledge)
            .product()
            .identity()
    }

    pub(crate) fn runtime_policy_revision(&self) -> RuntimePolicyRevision {
        self.transition
            .policy
            .as_ref()
            .unwrap_or(&self.base.policy)
            .active()
    }
}

impl PreparedIntelligenceCommit {
    pub(crate) const fn proposed<'a>(
        &'a self,
        base: &'a IntelligenceCore,
    ) -> ProposedIntelligenceView<'a> {
        ProposedIntelligenceView {
            transition: &self.transition,
            base,
            checkpoint: &self.checkpoint,
        }
    }

    pub(crate) fn commit(mut self, core: &mut IntelligenceCore) {
        assert_eq!(
            core.revision, self.transition.base_revision,
            "a prepared Intelligence commit remains bound to its staged base"
        );
        core.experience
            .commit_reserved_delta(self.transition.experience);
        if let Some(ecology) = self.transition.ecology.take() {
            core.ecology = ecology;
        }
        if let Some(knowledge) = self.transition.knowledge.take() {
            core.knowledge = knowledge;
        }
        if let Some(policy) = self.transition.policy.take() {
            core.policy = policy;
            core.policy_import_eligible = false;
        }
        core.roots = self.transition.roots;
        core.revision = self.transition.revision;
        core.checkpoint_bytes = self.checkpoint.bytes;
        core.checkpoint_log_root = self.transition.checkpoint_log_root;
        core.checkpoint_records = self.transition.checkpoint_records;
    }
}

#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct StageWork {
    pub(super) causal_items: usize,
    pub(super) ecology_recomputed: bool,
    pub(super) knowledge_recomputed: bool,
    pub(super) checkpoint_delta_bytes: usize,
    pub(super) historical_items_reencoded: usize,
    pub(super) ecology_items_cloned: usize,
    pub(super) knowledge_items_cloned: usize,
}

impl IntelligenceTransition {
    pub(crate) const fn policy_decision(&self) -> Option<RuntimePolicyDecision> {
        self.policy_decision
    }

    #[cfg(test)]
    pub(super) fn stage_work(&self) -> StageWork {
        StageWork {
            causal_items: self.experience.staged_item_count(),
            ecology_recomputed: self.ecology.is_some(),
            knowledge_recomputed: self.knowledge.is_some(),
            checkpoint_delta_bytes: self.checkpoint_record.len(),
            historical_items_reencoded: 0,
            ecology_items_cloned: self.ecology_items_cloned,
            knowledge_items_cloned: self.knowledge_items_cloned,
        }
    }
    #[cfg(test)]
    pub(crate) fn checkpoint(&self) -> IntelligenceCheckpoint {
        if self.checkpoint_record.is_empty() {
            return IntelligenceCheckpoint {
                bytes: Arc::clone(&self.base_checkpoint),
            };
        }
        let bytes = build_delta_checkpoint(
            &self.base_checkpoint,
            &self.checkpoint_record,
            self.checkpoint_records,
            self.roots,
            self.revision,
            self.checkpoint_log_root,
        )
        .expect("a staged Intelligence Checkpoint fits its declared capacity");
        IntelligenceCheckpoint {
            bytes: Arc::new(bytes),
        }
    }

    pub(crate) fn prepare(
        self,
        core: &mut IntelligenceCore,
    ) -> Result<PreparedIntelligenceCommit, IntelligenceError> {
        if core.revision != self.base_revision {
            return Err(IntelligenceError::StaleTransition);
        }
        core.experience.reserve_for_delta(&self.experience)?;
        let checkpoint = if self.checkpoint_record.is_empty() {
            IntelligenceCheckpoint {
                bytes: Arc::clone(&self.base_checkpoint),
            }
        } else {
            IntelligenceCheckpoint {
                bytes: Arc::new(build_delta_checkpoint(
                    &self.base_checkpoint,
                    &self.checkpoint_record,
                    self.checkpoint_records,
                    self.roots,
                    self.revision,
                    self.checkpoint_log_root,
                )?),
            }
        };
        Ok(PreparedIntelligenceCommit {
            transition: self,
            checkpoint,
        })
    }

    pub(crate) fn checkpoint_encoded_len(&self) -> usize {
        self.base_checkpoint
            .len()
            .saturating_add(self.checkpoint_record.len())
    }

    pub(crate) fn materialization_resident_bytes(&self, core: &IntelligenceCore) -> u64 {
        let checkpoint = if self.checkpoint_record.is_empty() {
            0
        } else {
            u64::try_from(self.checkpoint_encoded_len())
                .unwrap_or(u64::MAX)
                .saturating_add(arc_vec_allocation_bytes())
        };
        checkpoint.saturating_add(core.experience.reservation_resident_bytes(&self.experience))
    }

    #[cfg(test)]
    pub(crate) fn commit(self, core: &mut IntelligenceCore) -> Result<(), IntelligenceError> {
        let prepared = self.prepare(core)?;
        prepared.commit(core);
        Ok(())
    }
}

impl LegacyCoreImport {
    pub(crate) fn into_legacy_learning(
        self,
        conversion: Option<FtrlConversionView<'_>>,
    ) -> Result<Self, IntelligenceError> {
        Ok(Self {
            core: self.core.into_legacy_learning(conversion)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn into_legacy_ftrl(
        self,
        conversion: FtrlConversionView<'_>,
    ) -> Result<Self, IntelligenceError> {
        Ok(Self {
            core: self.core.into_legacy_ftrl(conversion)?,
        })
    }

    pub(crate) fn into_legacy_runtime_policy(
        self,
        policy: &RuntimePolicyState,
    ) -> Result<Self, IntelligenceError> {
        Ok(Self {
            core: self.core.into_legacy_runtime_policy(policy)?,
        })
    }

    pub(crate) fn into_legacy_knowledge(
        self,
        knowledge: &KnowledgeState,
    ) -> Result<Self, IntelligenceError> {
        Ok(Self {
            core: self.core.into_legacy_knowledge(knowledge)?,
        })
    }

    pub(crate) fn finalize(mut self) -> Result<IntelligenceCore, IntelligenceError> {
        if self.core.legacy_ftrl_import_eligible || self.core.legacy_knowledge_import_eligible {
            return Err(IntelligenceError::IncompatibleRevision);
        }
        if self.core.policy_import_eligible {
            self = self.into_legacy_runtime_policy(&RuntimePolicyState::bootstrap())?;
        }
        let IntelligenceCore {
            limits,
            ecology,
            experience,
            knowledge,
            policy,
            ..
        } = self.core;
        Ok(IntelligenceCore::from_components(
            limits, ecology, experience, knowledge, policy, false, false, false,
        ))
    }
}

fn bid_forecasts<'a>(bid: &BidRecord, forecasts: &'a [Forecast]) -> &'a [Forecast] {
    let start = bid.forecast_start as usize;
    let end = start + usize::from(bid.forecast_count);
    &forecasts[start..end]
}

fn representative_forecast(bid: &BidRecord, forecasts: &[Forecast]) -> Forecast {
    *bid_forecasts(bid, forecasts)
        .first()
        .expect("a valid specialist has at least one typed forecast")
}

fn valid_verification_settlement_batch(
    work: &KnowledgeWork,
    receipts: &[InvestmentReceipt],
    reserved: ResourceVector,
    settlements: Option<&[InvestmentSettlement]>,
) -> bool {
    if receipts.is_empty()
        || receipts.len() != work.obligations().len()
        || settlements.is_some_and(|values| values.len() != receipts.len())
    {
        return false;
    }
    let mut declared_total = ResourceVector::default();
    let mut actual_total = ResourceVector::default();
    for (index, (obligation, receipt)) in work.obligations().iter().zip(receipts).enumerate() {
        let declared = receipt.resources();
        if receipt.issued_under() != work.base_revision
            || receipt.opportunity_kind() != OpportunityKind::Candidate
            || receipt.tag() != InvestmentTag::Verify
            || receipt.opportunity_identity() != obligation.subject().identity()
            || declared.verification_requests != 1
            || index > 0 && (declared.resident_bytes != 0 || declared.elapsed_time_ns != 0)
            || receipts[..index]
                .iter()
                .any(|prior| prior.decision() == receipt.decision())
        {
            return false;
        }
        let Some(next_declared) = declared_total.checked_add(declared) else {
            return false;
        };
        declared_total = next_declared;
        if let Some(settlement) = settlements.map(|values| values[index]) {
            let actual = settlement.actual_resources();
            if settlement.decision() != receipt.decision()
                || !actual.fits_within(declared)
                || actual.verification_requests != 1
                || index > 0 && (actual.resident_bytes != 0 || actual.elapsed_time_ns != 0)
            {
                return false;
            }
            let Some(next_actual) = actual_total.checked_add(actual) else {
                return false;
            };
            actual_total = next_actual;
        }
    }
    declared_total == reserved && settlements.is_none_or(|_| actual_total.fits_within(reserved))
}

fn imported_ftrl_role(source_lineage: [u8; 32], canonical_model: [u8; 32]) -> RoleId {
    let mut digest = Sha256::new();
    digest.update(b"reflex-imported-ftrl-generalist-role-v1\0");
    digest.update(source_lineage);
    digest.update(canonical_model);
    RoleId::from_identity(digest.finalize().into())
}

fn component_roots(
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
    policy: &RuntimePolicyState,
) -> ComponentRoots {
    ComponentRoots {
        ecology: ecology.canonical_root(),
        experience: experience.canonical_root(),
        knowledge: knowledge.canonical_root(),
        policy: policy.canonical_root(),
    }
}

fn legacy_v15_component_roots(
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
    policy: &RuntimePolicyState,
) -> ComponentRoots {
    ComponentRoots {
        ecology: ecology.canonical_root(),
        experience: experience.legacy_v15_canonical_root(),
        knowledge: knowledge.canonical_root(),
        policy: policy.canonical_root(),
    }
}

fn legacy_v11_component_roots(
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
) -> ComponentRoots {
    ComponentRoots {
        ecology: ecology.canonical_root(),
        experience: experience.legacy_v11_canonical_root(),
        knowledge: knowledge.legacy_v13_canonical_root(),
        policy: RuntimePolicyState::bootstrap().canonical_root(),
    }
}

fn legacy_v13_component_roots(
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
    policy: &RuntimePolicyState,
) -> ComponentRoots {
    ComponentRoots {
        ecology: ecology.canonical_root(),
        experience: experience.canonical_root(),
        knowledge: knowledge.legacy_v13_canonical_root(),
        policy: policy.canonical_root(),
    }
}

fn legacy_v14_component_roots(
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
    policy: &RuntimePolicyState,
) -> ComponentRoots {
    ComponentRoots {
        ecology: ecology.canonical_root(),
        experience: experience.canonical_root(),
        knowledge: knowledge.legacy_v14_canonical_root(),
        policy: policy.canonical_root(),
    }
}

fn legacy_v12_component_roots(
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
) -> ComponentRoots {
    ComponentRoots {
        ecology: ecology.canonical_root(),
        experience: experience.canonical_root(),
        knowledge: knowledge.legacy_v13_canonical_root(),
        policy: RuntimePolicyState::bootstrap().canonical_root(),
    }
}

fn revision_digest(limits: IntelligenceLimits, roots: ComponentRoots) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-intelligence-revision-v5\0");
    for value in [
        limits.maximum_opportunities,
        limits.maximum_investments,
        limits.maximum_bids,
        limits.maximum_forecast_cells,
        limits.maximum_specialists,
        limits.maximum_model_bytes,
        limits.maximum_specialists_per_route,
    ] {
        digest.update((value as u64).to_le_bytes());
    }
    digest.update(roots.ecology);
    digest.update(roots.experience);
    digest.update(roots.knowledge);
    digest.update(roots.policy);
    digest.finalize().into()
}

fn legacy_v12_revision_digest(limits: IntelligenceLimits, roots: ComponentRoots) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-intelligence-revision-v4\0");
    for value in current_limit_values(limits) {
        digest.update((value as u64).to_le_bytes());
    }
    digest.update(roots.ecology);
    digest.update(roots.experience);
    digest.update(roots.knowledge);
    digest.finalize().into()
}

fn legacy_v9_revision_digest(
    limits: IntelligenceLimits,
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
) -> [u8; 32] {
    let mut encoded = Vec::new();
    ecology.encode_canonical(&mut encoded);
    experience.encode_canonical(&mut encoded);
    knowledge.encode_legacy_v13(&mut encoded);
    let mut digest = Sha256::new();
    digest.update(b"reflex-intelligence-revision-v3\0");
    for value in current_limit_values(limits) {
        digest.update((value as u64).to_le_bytes());
    }
    digest.update(encoded);
    digest.finalize().into()
}

const fn current_limit_values(limits: IntelligenceLimits) -> [usize; 7] {
    [
        limits.maximum_opportunities,
        limits.maximum_investments,
        limits.maximum_bids,
        limits.maximum_forecast_cells,
        limits.maximum_specialists,
        limits.maximum_model_bytes,
        limits.maximum_specialists_per_route,
    ]
}

fn snapshot_checkpoint(
    limits: IntelligenceLimits,
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
    policy: &RuntimePolicyState,
    roots: ComponentRoots,
    revision: [u8; 32],
) -> (Vec<u8>, [u8; 32]) {
    segmented_checkpoint(
        CheckpointWire::current(),
        limits,
        ecology,
        experience,
        knowledge,
        policy,
        roots,
        revision,
    )
}

#[derive(Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the bounded migration descriptor names presence of four independently introduced canonical components"
)]
struct CheckpointWire {
    magic: [u8; 5],
    includes_preference: bool,
    includes_corpus_key: bool,
    includes_policy: bool,
    includes_active_knowledge: bool,
    includes_pending_verification: bool,
}

impl CheckpointWire {
    const fn current() -> Self {
        Self {
            magic: CURRENT_MAGIC,
            includes_preference: true,
            includes_corpus_key: true,
            includes_policy: true,
            includes_active_knowledge: true,
            includes_pending_verification: true,
        }
    }

    #[cfg(test)]
    const fn legacy_v16() -> Self {
        Self {
            magic: LEGACY_V16_SEGMENTED_MAGIC,
            includes_preference: true,
            includes_corpus_key: true,
            includes_policy: true,
            includes_active_knowledge: true,
            includes_pending_verification: true,
        }
    }

    #[cfg(test)]
    const fn legacy_v15() -> Self {
        Self {
            magic: LEGACY_V15_SEGMENTED_MAGIC,
            includes_preference: true,
            includes_corpus_key: false,
            includes_policy: true,
            includes_active_knowledge: true,
            includes_pending_verification: true,
        }
    }

    #[cfg(test)]
    const fn legacy(magic: [u8; 5]) -> Self {
        Self {
            magic,
            includes_preference: false,
            includes_corpus_key: false,
            includes_policy: false,
            includes_active_knowledge: false,
            includes_pending_verification: false,
        }
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    const fn legacy_v12() -> Self {
        Self {
            magic: LEGACY_V12_SEGMENTED_MAGIC,
            includes_preference: true,
            includes_corpus_key: false,
            includes_policy: false,
            includes_active_knowledge: false,
            includes_pending_verification: false,
        }
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    const fn legacy_v13() -> Self {
        Self {
            magic: LEGACY_V13_SEGMENTED_MAGIC,
            includes_preference: true,
            includes_corpus_key: false,
            includes_policy: true,
            includes_active_knowledge: false,
            includes_pending_verification: false,
        }
    }

    #[cfg(test)]
    const fn legacy_v14() -> Self {
        Self {
            magic: LEGACY_V14_SEGMENTED_MAGIC,
            includes_preference: true,
            includes_corpus_key: false,
            includes_policy: true,
            includes_active_knowledge: true,
            includes_pending_verification: false,
        }
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "the snapshot encoder receives the four independently authenticated components beside its wire, limits, roots, and revision seal"
)]
fn segmented_checkpoint(
    wire: CheckpointWire,
    limits: IntelligenceLimits,
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
    policy: &RuntimePolicyState,
    roots: ComponentRoots,
    revision: [u8; 32],
) -> (Vec<u8>, [u8; 32]) {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&wire.magic);
    for value in current_limit_values(limits) {
        bytes.extend_from_slice(&(value as u64).to_le_bytes());
    }
    let mut record = vec![1];
    let mut encoded = Vec::new();
    ecology.encode_canonical(&mut encoded);
    push_record_bytes(&mut record, &encoded);
    encoded.clear();
    if wire.includes_corpus_key {
        experience.encode_canonical(&mut encoded);
    } else if wire.includes_preference {
        experience.encode_legacy_v15(&mut encoded);
    } else {
        experience.encode_legacy_v11(&mut encoded);
    }
    push_record_bytes(&mut record, &encoded);
    encoded.clear();
    if wire.includes_pending_verification {
        knowledge.encode_canonical(&mut encoded);
    } else if wire.includes_active_knowledge {
        knowledge.encode_legacy_v14(&mut encoded);
    } else {
        knowledge.encode_legacy_v13(&mut encoded);
    }
    push_record_bytes(&mut record, &encoded);
    if wire.includes_policy {
        push_record_bytes(&mut record, &policy.encode());
    }
    bytes.extend_from_slice(&record);
    let log_root = checkpoint_log_root(checkpoint_header_root(&bytes[..61]), &record);
    if wire.includes_policy {
        append_checkpoint_footer(&mut bytes, 1, roots, revision, log_root);
    } else {
        append_legacy_segmented_checkpoint_footer(&mut bytes, 1, roots, revision, log_root);
    }
    (bytes, log_root)
}

fn checkpoint_header_root(header: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-intelligence-checkpoint-header-v1\0");
    digest.update(header);
    digest.finalize().into()
}

fn checkpoint_log_root(previous: [u8; 32], record: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-intelligence-checkpoint-log-v1\0");
    digest.update(previous);
    digest.update((record.len() as u64).to_le_bytes());
    digest.update(record);
    digest.finalize().into()
}

fn append_checkpoint_footer(
    bytes: &mut Vec<u8>,
    records: u64,
    roots: ComponentRoots,
    revision: [u8; 32],
    log_root: [u8; 32],
) {
    bytes.push(0);
    bytes.extend_from_slice(&records.to_le_bytes());
    bytes.extend_from_slice(&roots.ecology);
    bytes.extend_from_slice(&roots.experience);
    bytes.extend_from_slice(&roots.knowledge);
    bytes.extend_from_slice(&roots.policy);
    bytes.extend_from_slice(&revision);
    bytes.extend_from_slice(&log_root);
    let mut digest = Sha256::new();
    digest.update(b"reflex-intelligence-checkpoint-footer-v2\0");
    digest.update(records.to_le_bytes());
    digest.update(roots.ecology);
    digest.update(roots.experience);
    digest.update(roots.knowledge);
    digest.update(roots.policy);
    digest.update(revision);
    digest.update(log_root);
    let checksum: [u8; 32] = digest.finalize().into();
    bytes.extend_from_slice(&checksum);
}

fn append_legacy_segmented_checkpoint_footer(
    bytes: &mut Vec<u8>,
    records: u64,
    roots: ComponentRoots,
    revision: [u8; 32],
    log_root: [u8; 32],
) {
    bytes.push(0);
    bytes.extend_from_slice(&records.to_le_bytes());
    bytes.extend_from_slice(&roots.ecology);
    bytes.extend_from_slice(&roots.experience);
    bytes.extend_from_slice(&roots.knowledge);
    bytes.extend_from_slice(&revision);
    bytes.extend_from_slice(&log_root);
    let mut digest = Sha256::new();
    digest.update(b"reflex-intelligence-checkpoint-footer-v1\0");
    digest.update(records.to_le_bytes());
    digest.update(roots.ecology);
    digest.update(roots.experience);
    digest.update(roots.knowledge);
    digest.update(revision);
    digest.update(log_root);
    bytes.extend_from_slice(&<[u8; 32]>::from(digest.finalize()));
}

fn push_record_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    output.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
    output.extend_from_slice(bytes);
}

fn encode_delta_record(
    ecology: Option<&[u8]>,
    experience: &CausalDelta,
    knowledge: Option<&[u8]>,
    policy: Option<&[u8]>,
) -> Vec<u8> {
    let mut record = vec![2];
    let mut flags = 0_u8;
    if ecology.is_some() {
        flags |= 1;
    }
    if knowledge.is_some() {
        flags |= 2;
    }
    if policy.is_some() {
        flags |= 4;
    }
    record.push(flags);
    if let Some(encoded) = ecology {
        push_record_bytes(&mut record, encoded);
    }
    let mut encoded = Vec::new();
    experience.encode_canonical(&mut encoded);
    push_record_bytes(&mut record, &encoded);
    if let Some(encoded) = knowledge {
        push_record_bytes(&mut record, encoded);
    }
    if let Some(encoded) = policy {
        push_record_bytes(&mut record, encoded);
    }
    record
}

fn extend_component_root(label: &[u8], previous: [u8; 32], delta: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-intelligence-component-delta-v1\0");
    digest.update((label.len() as u64).to_le_bytes());
    digest.update(label);
    digest.update(previous);
    digest.update((delta.len() as u64).to_le_bytes());
    digest.update(delta);
    digest.finalize().into()
}

#[cfg(test)]
fn append_delta_checkpoint(
    checkpoint: &mut Vec<u8>,
    record: &[u8],
    records: u64,
    roots: ComponentRoots,
    revision: [u8; 32],
    log_root: [u8; 32],
) {
    let payload_length = checkpoint
        .len()
        .checked_sub(CHECKPOINT_FOOTER_BYTES)
        .expect("a current checkpoint always carries its fixed footer");
    checkpoint.truncate(payload_length);
    checkpoint.extend_from_slice(record);
    append_checkpoint_footer(checkpoint, records, roots, revision, log_root);
}

fn build_delta_checkpoint(
    base: &[u8],
    record: &[u8],
    records: u64,
    roots: ComponentRoots,
    revision: [u8; 32],
    log_root: [u8; 32],
) -> Result<Vec<u8>, IntelligenceError> {
    let payload_length = base
        .len()
        .checked_sub(CHECKPOINT_FOOTER_BYTES)
        .ok_or(IntelligenceError::CorruptState)?;
    let final_length = base
        .len()
        .checked_add(record.len())
        .ok_or(IntelligenceError::CapacityExceeded)?;
    let mut checkpoint = Vec::new();
    checkpoint
        .try_reserve_exact(final_length)
        .map_err(|_| IntelligenceError::CapacityExceeded)?;
    if checkpoint.capacity() != final_length {
        return Err(IntelligenceError::CapacityExceeded);
    }
    checkpoint.extend_from_slice(&base[..payload_length]);
    checkpoint.extend_from_slice(record);
    append_checkpoint_footer(&mut checkpoint, records, roots, revision, log_root);
    if checkpoint.len() != final_length || checkpoint.capacity() != final_length {
        return Err(IntelligenceError::CapacityExceeded);
    }
    Ok(checkpoint)
}

const fn arc_vec_allocation_bytes() -> u64 {
    (std::mem::size_of::<Vec<u8>>() + 2 * std::mem::size_of::<usize>()) as u64
}

fn read_record_bytes<'a>(input: &mut Decoder<'a>) -> Result<&'a [u8], IntelligenceError> {
    let length = usize::try_from(
        input
            .read_u64()
            .map_err(|()| IntelligenceError::CorruptState)?,
    )
    .map_err(|_| IntelligenceError::CorruptState)?;
    input
        .take(length)
        .map_err(|()| IntelligenceError::CorruptState)
}

fn legacy_v8_revision_digest(
    limits: IntelligenceLimits,
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
) -> [u8; 32] {
    let mut encoded = Vec::new();
    ecology.encode_canonical(&mut encoded);
    experience.encode_legacy_v11(&mut encoded);
    knowledge.encode_legacy_v13(&mut encoded);
    let mut digest = Sha256::new();
    digest.update(b"reflex-intelligence-revision-v2\0");
    for value in legacy_limit_values(limits) {
        digest.update((value as u64).to_le_bytes());
    }
    digest.update(encoded);
    digest.finalize().into()
}

fn legacy_v7_revision_digest(
    limits: IntelligenceLimits,
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
) -> Result<[u8; 32], ()> {
    let mut encoded = Vec::new();
    ecology.encode_canonical(&mut encoded);
    experience.encode_legacy_v11(&mut encoded);
    knowledge.encode_legacy_v7(&mut encoded)?;
    Ok(legacy_revision_digest(limits, &encoded))
}

fn legacy_v6_revision_digest(
    limits: IntelligenceLimits,
    ecology: &ModelEcology,
    experience: &CausalLedger,
    knowledge: &KnowledgeCompiler,
) -> Result<[u8; 32], ()> {
    let mut encoded = Vec::new();
    ecology.encode_canonical(&mut encoded);
    experience.encode_legacy_v11(&mut encoded);
    knowledge.encode_legacy_v6(&mut encoded)?;
    Ok(legacy_revision_digest(limits, &encoded))
}

fn legacy_revision_digest(limits: IntelligenceLimits, encoded: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-intelligence-revision-v1\0");
    for value in legacy_limit_values(limits) {
        digest.update((value as u64).to_le_bytes());
    }
    digest.update(encoded);
    digest.finalize().into()
}

const fn legacy_limit_values(limits: IntelligenceLimits) -> [usize; 6] {
    [
        limits.maximum_opportunities,
        limits.maximum_investments,
        limits.maximum_bids,
        limits.maximum_forecast_cells,
        limits.maximum_specialists,
        limits.maximum_model_bytes,
    ]
}

fn investment_digest(
    specification: InvestmentSpec,
    opportunity: super::arena::OpportunitySpec,
    features: &[f32],
) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-investment-v2\0");
    digest.update([opportunity_kind_tag(opportunity.kind)]);
    digest.update(opportunity.identity);
    digest.update(opportunity.corpus_key);
    digest.update(opportunity.feature_schema.0);
    digest.update(opportunity.routing_family.0);
    digest.update((features.len() as u64).to_le_bytes());
    for feature in features {
        digest.update(feature.to_bits().to_le_bytes());
    }
    digest.update([specification.tag() as u8]);
    match specification.kind {
        InvestmentKind::Generate {
            operator,
            applications,
        } => {
            digest.update(operator.identity());
            digest.update(applications.get().to_le_bytes());
        }
        InvestmentKind::Verify | InvestmentKind::Explore => {}
        InvestmentKind::Repair {
            rejection,
            operator,
        } => {
            digest.update(rejection.identity());
            digest.update(operator.identity());
        }
        InvestmentKind::TrainSpecialist { mandate } => digest.update(mandate.identity()),
        InvestmentKind::CompareRevision { challenger } => {
            digest.update(challenger.identity());
        }
        InvestmentKind::Consolidate { compiler } => digest.update(compiler.identity()),
        InvestmentKind::RunShadowCampaign { specification } => {
            digest.update(specification.identity());
        }
        InvestmentKind::ProposeRuntimePolicy { parent } => digest.update(parent.identity()),
    }
    for value in resource_values(specification.resources.expected()) {
        digest.update(value.to_le_bytes());
    }
    for value in resource_values(specification.resources.declared_upper()) {
        digest.update(value.to_le_bytes());
    }
    digest.update(specification.resources.bound_tags());
    digest.update(specification.bootstrap_priority.to_le_bytes());
    digest.update(specification.preference_priority.to_le_bytes());
    digest.finalize().into()
}

const fn opportunity_kind_tag(kind: super::arena::OpportunityKind) -> u8 {
    use super::arena::OpportunityKind;
    match kind {
        OpportunityKind::Candidate => 1,
        OpportunityKind::OperatorApplication => 2,
        OpportunityKind::Repair => 3,
        OpportunityKind::ArtifactPotential => 4,
        OpportunityKind::Emergent => 5,
        OpportunityKind::KnowledgePattern => 6,
        OpportunityKind::SpecialistNiche => 7,
        OpportunityKind::RuntimePolicy => 8,
        OpportunityKind::ShadowQuestion => 9,
    }
}

const fn resource_values(resources: ResourceVector) -> [u64; 5] {
    [
        resources.cpu_time_ns,
        resources.resident_bytes,
        resources.durable_bytes,
        resources.elapsed_time_ns,
        resources.verification_requests,
    ]
}

#[expect(
    clippy::too_many_arguments,
    reason = "the shadow token authenticates every independent restart, scheduling, resource, and cohort binding"
)]
fn knowledge_shadow_token(
    domain: &[u8],
    revision: [u8; 32],
    restart_checkpoint: CheckpointDigest,
    context_digest: SubjectId,
    product: SubjectId,
    root: SubjectId,
    subject_operator: &[u8],
    supporting_attempts: &BTreeSet<[u8; 32]>,
    resources: ResourceVector,
    root_index: u64,
) -> SubjectId {
    let mut digest = Sha256::new();
    digest.update(domain);
    digest.update(revision);
    digest.update(restart_checkpoint.identity());
    digest.update(context_digest.identity());
    digest.update(product.identity());
    digest.update(root.identity());
    digest.update((subject_operator.len() as u64).to_le_bytes());
    digest.update(subject_operator);
    digest.update((supporting_attempts.len() as u64).to_le_bytes());
    for attempt in supporting_attempts {
        digest.update(attempt);
    }
    for value in resource_values(resources) {
        digest.update(value.to_le_bytes());
    }
    digest.update(root_index.to_le_bytes());
    SubjectId::new(digest.finalize().into())
}

fn shadow_arm_outcome(
    opened: &KnowledgeShadowOpened,
    report: KnowledgeShadowArmReport,
) -> Result<ShadowUpdate, IntelligenceError> {
    Ok(ShadowUpdate::Outcome(ShadowArmOutcome::new(
        opened.campaign(),
        report.arm(),
        opened.campaign.checkpoint(),
        report.actual_resources(),
        opened.campaign.random_stream(),
        report.outcomes()?,
    )?))
}

#[cfg(test)]
mod checkpoint_tests {
    use super::*;
    use crate::intelligence::{
        AllocationSource, CompactModel, ForecastAxis, InvestmentOutcome, KnowledgeStatus,
        MarketArena, MarketFrame, OpportunitySpec, PortfolioBuffer, PreparedOperationalMarket,
        PriorHead, RoleId, RoutingFamilyId, SpecialistMandate, SpecialistRevision,
        SpecialistRevisionId,
    };
    use crate::learning::{FEATURE_COUNT, Features, FtrlModel, compare_forecasts};

    fn migrated_v12_core(limits: IntelligenceLimits) -> IntelligenceCore {
        let legacy = IntelligenceCore::fresh(limits).legacy_v12_checkpoint_for_test();
        IntelligenceCore::restore(legacy.as_bytes()).unwrap()
    }

    fn legacy_knowledge_fixture() -> crate::knowledge::KnowledgeState {
        let observations = (1_u8..=8)
            .flat_map(|case| {
                [
                    crate::knowledge::DerivationObservation {
                        id: [case; 32],
                        artifact: [case.saturating_add(1); 32],
                        parent: [case.saturating_add(10); 32],
                        claim: [case; 32],
                        operator_identity: b"simplify".to_vec(),
                        operator_steps: vec![b"simplify".to_vec()],
                        accepted: true,
                    },
                    crate::knowledge::DerivationObservation {
                        id: [case.saturating_add(100); 32],
                        artifact: [case.saturating_add(2); 32],
                        parent: [case.saturating_add(1); 32],
                        claim: [case; 32],
                        operator_identity: b"simplify".to_vec(),
                        operator_steps: vec![b"simplify".to_vec()],
                        accepted: true,
                    },
                    crate::knowledge::DerivationObservation {
                        id: [case.saturating_add(40); 32],
                        artifact: [case.saturating_add(41); 32],
                        parent: [case.saturating_add(50); 32],
                        claim: [case.saturating_add(20); 32],
                        operator_identity: b"rewrite".to_vec(),
                        operator_steps: vec![b"rewrite".to_vec()],
                        accepted: true,
                    },
                    crate::knowledge::DerivationObservation {
                        id: [case.saturating_add(140); 32],
                        artifact: [case.saturating_add(42); 32],
                        parent: [case.saturating_add(41); 32],
                        claim: [case.saturating_add(20); 32],
                        operator_identity: b"rewrite".to_vec(),
                        operator_steps: vec![b"rewrite".to_vec()],
                        accepted: true,
                    },
                ]
            })
            .collect::<Vec<_>>();
        let mut knowledge = crate::knowledge::KnowledgeState::default();
        knowledge.consolidate(&observations, [[20; 32]], [[21; 32]]);
        knowledge
    }

    fn verification_pair(
        core: &IntelligenceCore,
        limits: IntelligenceLimits,
        subject: SubjectId,
        epoch: u64,
        declared: ResourceVector,
        actual: ResourceVector,
    ) -> (InvestmentReceipt, InvestmentSettlement) {
        let mut market = MarketArena::with_capacity(limits);
        let opportunity = market
            .push_opportunity(OpportunitySpec::candidate(subject.identity()), &[1.0])
            .unwrap();
        market
            .push_investment(InvestmentSpec::verify(opportunity, declared, 0))
            .unwrap();
        let mut output = PortfolioBuffer::with_capacity(limits);
        let receipt = core
            .allocate(
                MarketFrame::new(&market, declared).at_epoch(epoch),
                1,
                &mut output,
            )
            .unwrap()
            .receipts()[0];
        let settlement = InvestmentSettlement::new(
            receipt.decision(),
            InvestmentOutcome::VerifiedAccepted {
                verification_record: subject,
            },
            actual,
        );
        (receipt, settlement)
    }

    fn verified_shadow_core(
        limits: IntelligenceLimits,
    ) -> (IntelligenceCore, Vec<DerivationObservation>) {
        let observations = (1_u8..=8)
            .flat_map(|case| {
                [
                    DerivationObservation {
                        id: [case; 32],
                        artifact: [case.saturating_add(1); 32],
                        parent: [case.saturating_add(10); 32],
                        claim: [case; 32],
                        operator_identity: b"simplify".to_vec(),
                        operator_steps: vec![b"simplify".to_vec()],
                        accepted: true,
                    },
                    DerivationObservation {
                        id: [case.saturating_add(100); 32],
                        artifact: [case.saturating_add(2); 32],
                        parent: [case.saturating_add(1); 32],
                        claim: [case; 32],
                        operator_identity: b"simplify".to_vec(),
                        operator_steps: vec![b"simplify".to_vec()],
                        accepted: true,
                    },
                ]
            })
            .collect::<Vec<_>>();
        let mut core = IntelligenceCore::fresh(limits);
        let work = core
            .plan_knowledge_verification(&observations, [[20; 32]], [[21; 32]])
            .unwrap()
            .unwrap();
        let (receipts, settlements) = work
            .obligations()
            .iter()
            .enumerate()
            .map(|(index, obligation)| {
                verification_pair(
                    &core,
                    limits,
                    obligation.subject(),
                    u64::try_from(index).unwrap(),
                    if index == 0 {
                        ResourceVector::new(2, 2, 2, 2, 1)
                    } else {
                        ResourceVector::new(2, 0, 2, 0, 1)
                    },
                    if index == 0 {
                        ResourceVector::new(1, 1, 1, 1, 1)
                    } else {
                        ResourceVector::new(1, 0, 1, 0, 1)
                    },
                )
            })
            .unzip::<_, _, Vec<_>, Vec<_>>();
        let reserved = receipts
            .iter()
            .try_fold(ResourceVector::default(), |total, receipt| {
                total.checked_add(receipt.resources())
            })
            .unwrap();
        let (open, opened) = core
            .open_knowledge_verification(work, &receipts, reserved)
            .unwrap();
        open.commit(&mut core).unwrap();
        core.settle_opened_knowledge_verification(
            &opened,
            KnowledgeVerificationReport::settled(&settlements),
        )
        .unwrap()
        .commit(&mut core)
        .unwrap();
        (core, observations)
    }

    #[test]
    fn completed_compiler_recipe_atomically_retains_its_exact_restart_work() {
        let limits = IntelligenceLimits::new(16, 64, 64, 512, 8, 128 * 1024).unwrap();
        let (_, observations) = verified_shadow_core(limits);
        let mut core = IntelligenceCore::fresh(limits);
        let resources = ResourceVector::new(10_000, 64 * 1024, 0, 10_000, 0);
        let plan = core.prepare_knowledge_compilation(resources);
        assert_eq!(plan.specifications().len(), 2);

        let mut market = MarketArena::with_capacity(limits);
        let prepared = PreparedOperationalMarket::prepare(plan.specifications(), &mut market)
            .expect("the deep module emits a valid bounded compiler market");
        let mut portfolio = PortfolioBuffer::with_capacity(limits);
        let action = prepared
            .allocate(&core, &market, resources, 0, 1, &mut portfolio)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let OperationalAction::Consolidate { compiler } = action.action() else {
            panic!("the compiler plan only exposes consolidation recipes");
        };
        let producer = action.decision();
        let executed = plan
            .execute(&core, producer, compiler, &[[20; 32]], &[[21; 32]], || {
                Ok::<_, ()>(observations)
            })
            .unwrap();
        let (receipt, settlement) = action
            .settle(
                &core,
                InvestmentOutcome::Completed,
                ResourceVector::new(1, 1, 0, 1, 0),
            )
            .unwrap();

        core.stage_completed_knowledge_compilation(executed, &receipt, settlement)
            .unwrap()
            .commit(&mut core)
            .unwrap();
        let expected = core
            .pending_knowledge_compilation()
            .unwrap()
            .expect("the exact challenger is retained with its producer");
        let restored = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        let resumed = restored
            .pending_knowledge_compilation()
            .unwrap()
            .expect("restart resumes the retained challenger without mining");
        assert_eq!(resumed.0.id(), expected.0.id());
        assert_eq!(resumed.1, producer);
        assert_eq!(expected.1, producer);

        let unavailable = restored
            .reject_knowledge_work(
                resumed.0.clone(),
                KnowledgePlanningFailure::WitnessUnavailable(resumed.0.obligations()[0].subject()),
            )
            .expect("retained work can be terminally rejected without reproposing it");
        let invalidated = IntelligenceCore::restore(unavailable.checkpoint().as_bytes()).unwrap();
        assert_eq!(
            invalidated.knowledge().status(resumed.0.id()),
            Some(KnowledgeStatus::Invalidated(
                super::super::knowledge::KnowledgeInvalidationReason::Witness
            ))
        );
    }

    #[test]
    fn completed_compiler_recipe_without_verification_obligations_retains_only_its_receipt() {
        let limits = IntelligenceLimits::new(16, 64, 64, 512, 8, 128 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let resources = ResourceVector::new(10_000, 64 * 1024, 0, 10_000, 0);
        let plan = core.prepare_knowledge_compilation(resources);
        let mut market = MarketArena::with_capacity(limits);
        let prepared = PreparedOperationalMarket::prepare(plan.specifications(), &mut market)
            .expect("the deep module emits a valid bounded compiler market");
        let mut portfolio = PortfolioBuffer::with_capacity(limits);
        let action = prepared
            .allocate(&core, &market, resources, 0, 1, &mut portfolio)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let OperationalAction::Consolidate { compiler } = action.action() else {
            panic!("the compiler plan only exposes consolidation recipes");
        };
        let producer = action.decision();
        let executed = plan
            .execute(&core, producer, compiler, &[[20; 32]], &[[21; 32]], || {
                Ok::<_, ()>(Vec::new())
            })
            .unwrap();
        let (receipt, settlement) = action
            .settle(
                &core,
                InvestmentOutcome::Completed,
                ResourceVector::new(1, 1, 0, 1, 0),
            )
            .unwrap();

        core.stage_completed_knowledge_compilation(executed, &receipt, settlement)
            .unwrap()
            .commit(&mut core)
            .unwrap();
        assert_eq!(core.pending_knowledge_compilation().unwrap(), None);
        assert_eq!(
            core.experience.latest_completed_consolidation(),
            Some(producer),
            "the executed compiler action remains causal Experience even when it emitted no verifiable challenger"
        );
        IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
    }

    #[test]
    fn compiler_reservation_covers_large_symbols_and_expanded_derived_steps() {
        let limits = IntelligenceLimits::new(16, 64, 64, 512, 8, 128 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let mut small = KnowledgeCompilationShape::default();
        small.observe(b"p", &[b"p".to_vec()]).unwrap();
        let mut large = KnowledgeCompilationShape::default();
        let identity = vec![7_u8; 64 * 1024];
        let steps = (0..8)
            .map(|step| vec![u8::try_from(step).unwrap(); 32 * 1024])
            .collect::<Vec<_>>();
        for _ in 0..16 {
            large.observe(&identity, &steps).unwrap();
        }

        let small_bound = core.knowledge_compilation_resident_bound(small, 2).unwrap();
        let large_bound = core.knowledge_compilation_resident_bound(large, 2).unwrap();
        assert!(large_bound > small_bound.saturating_mul(100));
        assert!(large_bound > 64 * 1024 * 16 + 32 * 1024 * 8 * 16);

        let maximum_active = KnowledgeState::maximum_resident_test_state(4 * 1024, 4 * 1024);
        let active_resident = maximum_active.resident_bytes();
        core.knowledge.replace_active_for_test(maximum_active);
        let maximum_bound = core
            .knowledge_compilation_resident_bound(large, MAX_ACTIVE_ARTIFACTS)
            .unwrap();
        assert!(
            maximum_bound >= large_bound.saturating_add(active_resident),
            "max operators, max steps, large symbols, and the complete active revision must all be resident in the declared compiler peak"
        );
    }

    #[test]
    fn refuted_only_consolidation_is_not_executable_verification_work() {
        let limits = IntelligenceLimits::new(8, 16, 16, 128, 4, 64 * 1024).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let observations = [DerivationObservation {
            id: [1; 32],
            artifact: [2; 32],
            parent: [3; 32],
            claim: [4; 32],
            operator_identity: b"refuted".to_vec(),
            operator_steps: vec![b"refuted".to_vec()],
            accepted: false,
        }];

        assert!(
            core.plan_knowledge_verification(&observations, [[2; 32]], [[2; 32]])
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn recovery_manifest_is_complete_stable_and_preflight_bounded() {
        let limits = IntelligenceLimits::new(8, 16, 16, 128, 4, 64 * 1024).unwrap();
        let (core, _) = verified_shadow_core(limits);
        let preflight = core.knowledge_recovery_manifest_resident_bytes().unwrap();
        let manifest = core.knowledge_recovery_manifest().unwrap();

        assert!(manifest.obligation_count() > 0);
        assert_eq!(preflight, manifest.heap_bytes());
        assert!(core.knowledge_recovery_manifest_matches(&manifest));
        assert!(
            manifest
                .obligations()
                .iter()
                .all(|entry| !entry.work().operator_steps().is_empty())
        );

        let restored = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        let recovered = restored.knowledge_recovery_manifest().unwrap();
        assert_eq!(manifest, recovered);
        assert!(restored.knowledge_recovery_manifest_matches(&manifest));
        assert!(!IntelligenceCore::fresh(limits).knowledge_recovery_manifest_matches(&manifest));
    }

    #[test]
    fn opened_verification_is_authenticated_recoverable_and_interrupts_monotonically() {
        let limits = IntelligenceLimits::new(32, 32, 32, 256, 4, 256 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let observations = (1_u8..=8)
            .flat_map(|case| {
                [
                    DerivationObservation {
                        id: [case; 32],
                        artifact: [case.saturating_add(1); 32],
                        parent: [case.saturating_add(10); 32],
                        claim: [case; 32],
                        operator_identity: b"simplify".to_vec(),
                        operator_steps: vec![b"simplify".to_vec()],
                        accepted: true,
                    },
                    DerivationObservation {
                        id: [case.saturating_add(100); 32],
                        artifact: [case.saturating_add(2); 32],
                        parent: [case.saturating_add(1); 32],
                        claim: [case; 32],
                        operator_identity: b"simplify".to_vec(),
                        operator_steps: vec![b"simplify".to_vec()],
                        accepted: true,
                    },
                ]
            })
            .collect::<Vec<_>>();
        let work = core
            .plan_knowledge_verification(&observations, [[20; 32]], [[21; 32]])
            .unwrap()
            .unwrap();
        assert!(!work.obligations().is_empty());
        let receipts = work
            .obligations()
            .iter()
            .enumerate()
            .map(|(index, obligation)| {
                let declared = if index == 0 {
                    ResourceVector::new(3, 5, 7, 11, 1)
                } else {
                    ResourceVector::new(3, 0, 7, 0, 1)
                };
                verification_pair(
                    &core,
                    limits,
                    obligation.subject(),
                    u64::try_from(index).unwrap(),
                    declared,
                    declared,
                )
                .0
            })
            .collect::<Vec<_>>();
        let reserved = receipts
            .iter()
            .try_fold(ResourceVector::default(), |total, receipt| {
                total.checked_add(receipt.resources())
            })
            .unwrap();
        let initial_receipts = core.experience().receipts().len();

        let (open, opened) = core
            .open_knowledge_verification(work, &receipts, reserved)
            .unwrap();
        assert!(core.opened_knowledge_verification().is_none());
        assert_eq!(
            opened.reserved_verification_requests(),
            receipts.len() as u64
        );
        assert_eq!(opened.receipts(), receipts);
        let recovered_open = IntelligenceCore::restore(open.checkpoint().as_bytes()).unwrap();
        assert_eq!(
            recovered_open.opened_knowledge_verification().unwrap(),
            opened
        );
        open.commit(&mut core).unwrap();
        assert_eq!(core.opened_knowledge_verification().unwrap(), opened);

        let interrupted = core
            .settle_opened_knowledge_verification(
                &opened,
                KnowledgeVerificationReport::interrupted(),
            )
            .unwrap();
        interrupted.commit(&mut core).unwrap();
        assert!(core.opened_knowledge_verification().is_none());
        assert_eq!(
            core.experience().receipts().len(),
            initial_receipts + receipts.len()
        );
        let restored = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        assert_eq!(restored.checkpoint(), core.checkpoint());
        assert!(restored.opened_knowledge_verification().is_none());
    }

    #[test]
    fn legacy_v14_checkpoint_migrates_to_current_without_pending_work() {
        let limits = IntelligenceLimits::new(8, 8, 8, 64, 4, 64 * 1024).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let legacy = core.legacy_v14_checkpoint_for_test();
        assert_eq!(&legacy.as_bytes()[..5], b"RFIC\x0E");

        let migrated = IntelligenceCore::restore(legacy.as_bytes()).unwrap();
        assert!(migrated.opened_knowledge_verification().is_none());
        assert_eq!(&migrated.checkpoint().as_bytes()[..5], b"RFIC\x11");
        let resealed = migrated.into_fork_with_limits(limits).unwrap();
        assert_eq!(&resealed.checkpoint().as_bytes()[..5], b"RFIC\x11");
        assert!(resealed.opened_knowledge_verification().is_none());
    }

    #[test]
    fn legacy_v15_receipts_recover_semantic_corpus_identity_and_reseal_to_v17() {
        let limits = IntelligenceLimits::new(8, 8, 8, 64, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let (receipt, settlement) = verification_pair(
            &core,
            limits,
            SubjectId::new([71; 32]),
            1,
            ResourceVector::new(1, 1, 0, 1, 1),
            ResourceVector::new(1, 1, 0, 1, 1),
        );
        core.stage(SettlementFrame::observations(
            &[receipt],
            &[settlement],
            &[],
            &[],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        let legacy = core.legacy_v15_checkpoint_for_test();
        assert_eq!(&legacy.as_bytes()[..5], b"RFIC\x0F");

        let migrated = IntelligenceCore::restore(legacy.as_bytes()).unwrap();
        let receipt = migrated.experience().receipts()[0];
        assert_eq!(receipt.corpus_key(), receipt.opportunity_identity());
        let resealed = migrated.into_fork_with_limits(limits).unwrap();
        assert_eq!(&resealed.checkpoint().as_bytes()[..5], b"RFIC\x11");
    }

    #[test]
    fn legacy_v13_knowledge_import_is_exact_consuming_and_restart_closed() {
        let limits = IntelligenceLimits::new(8, 8, 32, 256, 4, 64 * 1024).unwrap();
        let legacy = legacy_knowledge_fixture();
        let legacy_checkpoint = IntelligenceCore::fresh(limits).legacy_v13_checkpoint_for_test();
        let migrated = IntelligenceCore::restore(legacy_checkpoint.as_bytes()).unwrap();

        let imported = migrated.into_legacy_knowledge(&legacy).unwrap();
        assert_eq!(imported.knowledge_generation(), legacy.generation());
        assert_eq!(imported.knowledge_product(), legacy.product());
        assert_eq!(
            imported.pinned_knowledge_revision(),
            legacy.pinned_revision()
        );
        assert_eq!(&imported.checkpoint().as_bytes()[..5], b"RFIC\x11");

        let restored = IntelligenceCore::restore(imported.checkpoint().as_bytes()).unwrap();
        assert_eq!(restored.checkpoint(), imported.checkpoint());
        assert_eq!(restored.knowledge_product(), legacy.product());
        assert_eq!(
            restored.into_legacy_knowledge(&legacy).unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );
        assert_eq!(
            IntelligenceCore::fresh(limits)
                .into_legacy_knowledge(&legacy)
                .unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the hostile verification reactor scenario exercises one atomic protocol from plan through durable settlement"
    )]
    fn knowledge_verification_reactor_plans_deterministically_and_settles_through_core() {
        let limits = IntelligenceLimits::new(8, 16, 16, 128, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let observations = (1_u8..=8)
            .flat_map(|case| {
                [
                    crate::knowledge::DerivationObservation {
                        id: [case; 32],
                        artifact: [case.saturating_add(1); 32],
                        parent: [case.saturating_add(10); 32],
                        claim: [case; 32],
                        operator_identity: b"simplify".to_vec(),
                        operator_steps: vec![b"simplify".to_vec()],
                        accepted: true,
                    },
                    crate::knowledge::DerivationObservation {
                        id: [case.saturating_add(100); 32],
                        artifact: [case.saturating_add(2); 32],
                        parent: [case.saturating_add(1); 32],
                        claim: [case; 32],
                        operator_identity: b"simplify".to_vec(),
                        operator_steps: vec![b"simplify".to_vec()],
                        accepted: true,
                    },
                    crate::knowledge::DerivationObservation {
                        id: [case.saturating_add(40); 32],
                        artifact: [case.saturating_add(41); 32],
                        parent: [case.saturating_add(50); 32],
                        claim: [case.saturating_add(20); 32],
                        operator_identity: b"rewrite".to_vec(),
                        operator_steps: vec![b"rewrite".to_vec()],
                        accepted: true,
                    },
                    crate::knowledge::DerivationObservation {
                        id: [case.saturating_add(140); 32],
                        artifact: [case.saturating_add(42); 32],
                        parent: [case.saturating_add(41); 32],
                        claim: [case.saturating_add(20); 32],
                        operator_identity: b"rewrite".to_vec(),
                        operator_steps: vec![b"rewrite".to_vec()],
                        accepted: true,
                    },
                ]
            })
            .collect::<Vec<_>>();
        let first = core
            .plan_knowledge_verification(&observations, [[20; 32]], [[21; 32]])
            .unwrap()
            .unwrap();
        let second = core
            .plan_knowledge_verification(&observations, [[20; 32]], [[21; 32]])
            .unwrap()
            .unwrap();
        assert_eq!(first.id(), second.id());
        assert_eq!(first.product(), second.product());
        assert_eq!(first.obligations(), second.obligations());
        assert!(first.obligations().iter().all(|obligation| {
            !obligation.operator_steps().is_empty() && !obligation.supporting_attempts().is_empty()
        }));
        assert_eq!(core.knowledge().records().len(), 0, "planning is pure");

        let untouched = core.checkpoint();
        assert_eq!(
            core.reject_knowledge_work(
                first.clone(),
                KnowledgePlanningFailure::WitnessUnavailable(SubjectId::new([0xfe; 32])),
            )
            .unwrap_err(),
            IntelligenceError::InvalidReference
        );
        let unavailable = core
            .reject_knowledge_work(
                first.clone(),
                KnowledgePlanningFailure::WitnessUnavailable(first.obligations()[0].subject()),
            )
            .unwrap();
        let invalidated = IntelligenceCore::restore(unavailable.checkpoint().as_bytes()).unwrap();
        assert_eq!(
            invalidated.knowledge().status(first.id()),
            Some(KnowledgeStatus::Invalidated(
                super::super::knowledge::KnowledgeInvalidationReason::Witness
            ))
        );
        assert_eq!(core.checkpoint(), untouched, "invalidation is staged");

        let mut receipts = Vec::new();
        let mut settlements = Vec::new();
        for (index, obligation) in first.obligations().iter().enumerate() {
            let (receipt, settlement) = verification_pair(
                &core,
                limits,
                obligation.subject(),
                u64::try_from(index).unwrap(),
                if index == 0 {
                    ResourceVector::new(1, 1, 0, 1, 1)
                } else {
                    ResourceVector::new(1, 0, 0, 0, 1)
                },
                if index == 0 {
                    ResourceVector::new(1, 1, 0, 1, 1)
                } else {
                    ResourceVector::new(1, 0, 0, 0, 1)
                },
            );
            receipts.push(receipt);
            settlements.push(settlement);
        }
        assert!(receipts.len() > 1, "the fixture must exercise ordering");
        let unchanged = core.checkpoint();
        let reserved = receipts
            .iter()
            .try_fold(ResourceVector::default(), |total, receipt| {
                total.checked_add(receipt.resources())
            })
            .unwrap();

        let mut swapped_receipts = receipts.clone();
        swapped_receipts.swap(0, 1);
        assert_eq!(
            core.open_knowledge_verification(first.clone(), &swapped_receipts, reserved,)
                .unwrap_err(),
            IntelligenceError::InvalidSettlement
        );
        assert_eq!(core.checkpoint(), unchanged);

        let (foreign_receipt, _) = verification_pair(
            &core,
            limits,
            SubjectId::new([0xee; 32]),
            100,
            ResourceVector::new(1, 1, 0, 1, 1),
            ResourceVector::new(1, 1, 0, 1, 1),
        );
        let mut foreign_receipts = receipts.clone();
        foreign_receipts[0] = foreign_receipt;
        assert_eq!(
            core.open_knowledge_verification(first.clone(), &foreign_receipts, reserved,)
                .unwrap_err(),
            IntelligenceError::InvalidSettlement
        );
        assert_eq!(core.checkpoint(), unchanged);

        let duplicate_receipts = vec![receipts[0]; receipts.len()];
        assert_eq!(
            core.open_knowledge_verification(first.clone(), &duplicate_receipts, reserved,)
                .unwrap_err(),
            IntelligenceError::InvalidSettlement
        );
        assert_eq!(core.checkpoint(), unchanged);

        let (open, opened) = core
            .open_knowledge_verification(first.clone(), &receipts, reserved)
            .unwrap();
        open.commit(&mut core).unwrap();
        let opened_checkpoint = core.checkpoint();

        let mut oversized_settlements = settlements.clone();
        oversized_settlements[0] = InvestmentSettlement::new(
            receipts[0].decision(),
            InvestmentOutcome::VerifiedAccepted {
                verification_record: first.obligations()[0].subject(),
            },
            ResourceVector::new(1, 2, 0, 1, 1),
        );
        assert_eq!(
            core.settle_opened_knowledge_verification(
                &opened,
                KnowledgeVerificationReport::settled(&oversized_settlements),
            )
            .unwrap_err(),
            IntelligenceError::InvalidSettlement
        );
        assert_eq!(core.checkpoint(), opened_checkpoint);

        let transition = core
            .settle_opened_knowledge_verification(
                &opened,
                KnowledgeVerificationReport::settled(&settlements),
            )
            .unwrap();
        let proposed_checkpoint = transition.checkpoint();
        let proposed = IntelligenceCore::restore(proposed_checkpoint.as_bytes()).unwrap();
        assert_eq!(
            proposed.knowledge().status(second.id()),
            Some(KnowledgeStatus::Verified)
        );
        assert_eq!(core.knowledge().records().len(), 0, "settlement is staged");
        transition.commit(&mut core).unwrap();
        let committed = core.checkpoint();
        assert_eq!(committed, proposed_checkpoint);
        assert!(
            core.plan_knowledge_verification(&observations, [[20; 32]], [[21; 32]])
                .unwrap()
                .is_none(),
            "the active compiler must not reissue retained Knowledge"
        );
        assert_eq!(
            core.open_knowledge_verification(second, &receipts, reserved,)
                .unwrap_err(),
            IntelligenceError::StaleTransition
        );
        assert_eq!(core.checkpoint(), committed);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the shadow reactor scenario audits restart binding, symmetry, promotion, and cascading rollback together"
    )]
    fn knowledge_shadow_reactor_is_restart_bound_symmetric_and_promotes_atomically() {
        let limits = IntelligenceLimits::new(8, 16, 16, 128, 4, 64 * 1024).unwrap();
        let allowance = ResourceVector::new(10, 10, 10, 10, 2);
        let restart = CheckpointDigest::new([0x71; 32]);
        let context = SubjectId::new([0x72; 32]);
        let roots = [
            KnowledgeShadowRootFact::new(SubjectId::new([0x10; 32]), SubjectId::new([0x20; 32]), 0),
            KnowledgeShadowRootFact::new(SubjectId::new([0x11; 32]), SubjectId::new([0x21; 32]), 1),
        ];

        let (mut core, observations) = verified_shadow_core(limits);
        let retained_before_planning = core.resident_bytes();
        let verified_work = KnowledgeWork::verification(
            core.revision,
            &KnowledgeState::default()
                .propose_consolidation(&observations, [[20; 32]], [[21; 32]])
                .unwrap(),
        )
        .unwrap();
        let supported_attempt = verified_work.obligations()[0].supporting_attempts()[0];
        let support_facts = [KnowledgeShadowSupportFact::new(
            supported_attempt,
            roots[0].claim(),
        )];
        let plan = core
            .plan_knowledge_shadow(restart, context, &roots, &support_facts, allowance)
            .unwrap()
            .unwrap();
        assert_eq!(plan.root(), roots[1].root(), "support claims are excluded");
        let challenger = plan.challenger;
        let product = plan.product();
        let incumbent = core.knowledge_product();
        let differently_bound = core
            .plan_knowledge_shadow(
                CheckpointDigest::new([0x73; 32]),
                context,
                &roots,
                &support_facts,
                allowance,
            )
            .unwrap()
            .unwrap();
        assert_ne!(plan.campaign.id(), differently_bound.campaign.id());
        assert!(Arc::ptr_eq(&plan.treatment, &differently_bound.treatment));
        assert!(Arc::ptr_eq(&plan.control, &differently_bound.control));
        assert_eq!(core.resident_bytes(), retained_before_planning);

        let (open, opened) = core.open_knowledge_shadow(plan).unwrap();
        assert_eq!(
            opened.execution(&core).unwrap_err(),
            IntelligenceError::StaleTransition,
            "execution is unavailable until the open transition commits"
        );
        let recovered_open = IntelligenceCore::restore(open.checkpoint().as_bytes()).unwrap();
        let execution = opened.execution(&recovered_open).unwrap();
        assert_eq!(execution.product(), product);
        assert_eq!(execution.per_arm_allowance(), allowance);
        assert!(!execution.subject_operator().is_empty());
        assert!(execution.supporting_attempts().contains(&supported_attempt));
        assert_ne!(execution.random_stream(), execution.scheduling_token());
        assert_ne!(
            execution.pinned_revision(ShadowArm::Treatment),
            execution.pinned_revision(ShadowArm::Control)
        );
        assert!(
            recovered_open
                .plan_knowledge_shadow(restart, context, &roots, &support_facts, allowance)
                .unwrap()
                .is_none(),
            "an open campaign excludes nesting after recovery"
        );
        open.commit(&mut core).unwrap();
        let shadow_settlement_bound = core
            .knowledge_shadow_settlement_transition_resident_bytes(&opened)
            .unwrap();
        assert!(shadow_settlement_bound >= core.transition_resident_bytes(0, 0, 0, None));

        let treatment = KnowledgeShadowArmReport::new(
            ShadowArm::Treatment,
            ResourceVector::new(2, 3, 4, 5, 1),
            0.0,
            2.0,
        )
        .unwrap();
        let control = KnowledgeShadowArmReport::new(
            ShadowArm::Control,
            ResourceVector::new(1, 2, 3, 4, 1),
            0.0,
            0.0,
        )
        .unwrap();
        assert_eq!(
            core.settle_knowledge_shadow(
                opened.clone(),
                KnowledgeShadowReport::paired(control, treatment),
            )
            .unwrap_err(),
            IntelligenceError::InvalidSettlement
        );
        let oversized = KnowledgeShadowArmReport::new(
            ShadowArm::Treatment,
            ResourceVector::new(11, 3, 4, 5, 1),
            0.0,
            2.0,
        )
        .unwrap();
        assert_eq!(
            core.settle_knowledge_shadow(
                opened.clone(),
                KnowledgeShadowReport::paired(oversized, control),
            )
            .unwrap_err(),
            IntelligenceError::InvalidSettlement
        );
        let non_promoting = core
            .settle_knowledge_shadow(opened, KnowledgeShadowReport::paired(treatment, control))
            .unwrap();
        let non_promoted =
            IntelligenceCore::restore(non_promoting.checkpoint().as_bytes()).unwrap();
        assert_eq!(non_promoted.knowledge_product(), incumbent);
        assert_eq!(
            non_promoted.knowledge().status(challenger),
            Some(KnowledgeStatus::Verified),
            "compression without downstream advantage cannot promote"
        );

        let (mut promoting, mut promoted_observations) = verified_shadow_core(limits);
        let plan = promoting
            .plan_knowledge_shadow(restart, context, &roots, &support_facts, allowance)
            .unwrap()
            .unwrap();
        let promoted_product = plan.product();
        let predecessor = promoting.knowledge_product();
        let (open, opened) = promoting.open_knowledge_shadow(plan).unwrap();
        open.commit(&mut promoting).unwrap();
        let campaign = opened.campaign();
        let treatment = KnowledgeShadowArmReport::new(
            ShadowArm::Treatment,
            ResourceVector::new(2, 2, 2, 2, 1),
            1.0,
            1.0,
        )
        .unwrap();
        let control = KnowledgeShadowArmReport::new(
            ShadowArm::Control,
            ResourceVector::new(1, 1, 1, 1, 1),
            0.0,
            0.0,
        )
        .unwrap();
        promoting
            .settle_knowledge_shadow(opened, KnowledgeShadowReport::paired(treatment, control))
            .unwrap()
            .commit(&mut promoting)
            .unwrap();
        assert_eq!(
            promoting.knowledge_product().identity(),
            promoted_product.identity()
        );

        promoted_observations.extend((20_u8..=27).flat_map(|case| {
            [
                DerivationObservation {
                    id: [case; 32],
                    artifact: [case.saturating_add(1); 32],
                    parent: [case.saturating_add(10); 32],
                    claim: [case; 32],
                    operator_identity: b"rewrite".to_vec(),
                    operator_steps: vec![b"rewrite".to_vec()],
                    accepted: true,
                },
                DerivationObservation {
                    id: [case.saturating_add(100); 32],
                    artifact: [case.saturating_add(2); 32],
                    parent: [case.saturating_add(1); 32],
                    claim: [case; 32],
                    operator_identity: b"rewrite".to_vec(),
                    operator_steps: vec![b"rewrite".to_vec()],
                    accepted: true,
                },
            ]
        }));
        let second_work = promoting
            .plan_knowledge_verification(&promoted_observations, [[30; 32]], [[31; 32]])
            .unwrap()
            .unwrap();
        let (second_receipts, second_settlements) = second_work
            .obligations()
            .iter()
            .enumerate()
            .map(|(index, obligation)| {
                verification_pair(
                    &promoting,
                    limits,
                    obligation.subject(),
                    100 + u64::try_from(index).unwrap(),
                    if index == 0 {
                        ResourceVector::new(2, 2, 2, 2, 1)
                    } else {
                        ResourceVector::new(2, 0, 2, 0, 1)
                    },
                    if index == 0 {
                        ResourceVector::new(1, 1, 1, 1, 1)
                    } else {
                        ResourceVector::new(1, 0, 1, 0, 1)
                    },
                )
            })
            .unzip::<_, _, Vec<_>, Vec<_>>();
        let second_reserved = second_receipts
            .iter()
            .try_fold(ResourceVector::default(), |total, receipt| {
                total.checked_add(receipt.resources())
            })
            .unwrap();
        let (second_open, second_opened) = promoting
            .open_knowledge_verification(second_work, &second_receipts, second_reserved)
            .unwrap();
        second_open.commit(&mut promoting).unwrap();
        promoting
            .settle_opened_knowledge_verification(
                &second_opened,
                KnowledgeVerificationReport::settled(&second_settlements),
            )
            .unwrap()
            .commit(&mut promoting)
            .unwrap();
        let second_plan = promoting
            .plan_knowledge_shadow(
                CheckpointDigest::new([0x81; 32]),
                SubjectId::new([0x82; 32]),
                &roots,
                &[],
                allowance,
            )
            .unwrap()
            .unwrap();
        let second_product = second_plan.product();
        let (second_shadow_open, second_shadow_opened) =
            promoting.open_knowledge_shadow(second_plan).unwrap();
        let second_campaign = second_shadow_opened.campaign();
        second_shadow_open.commit(&mut promoting).unwrap();
        promoting
            .settle_knowledge_shadow(
                second_shadow_opened,
                KnowledgeShadowReport::paired(treatment, control),
            )
            .unwrap()
            .commit(&mut promoting)
            .unwrap();
        assert_eq!(
            promoting.knowledge_product().identity(),
            second_product.identity()
        );

        let before_invalidation = promoting.checkpoint();
        let invalid_a = ShadowUpdate::invalidate(
            campaign,
            super::super::causal::ShadowInvalidationReason::CheckpointMismatch,
        );
        let invalid_b = ShadowUpdate::invalidate(
            second_campaign,
            super::super::causal::ShadowInvalidationReason::CheckpointMismatch,
        );
        promoting
            .stage(SettlementFrame::observations(
                &[],
                &[],
                &[],
                &[invalid_b.clone(), invalid_a.clone()],
            ))
            .unwrap()
            .commit(&mut promoting)
            .unwrap();
        assert_eq!(promoting.knowledge_product(), predecessor);
        assert_eq!(promoting.knowledge_generation(), 4);

        let mut reverse = IntelligenceCore::restore(before_invalidation.as_bytes()).unwrap();
        reverse
            .stage(SettlementFrame::observations(
                &[],
                &[],
                &[],
                &[invalid_a, invalid_b],
            ))
            .unwrap()
            .commit(&mut reverse)
            .unwrap();
        assert_eq!(reverse.knowledge_product(), predecessor);
        assert_eq!(reverse.knowledge_generation(), 4);
        let roundtrip = IntelligenceCore::restore(reverse.checkpoint().as_bytes()).unwrap();
        assert_eq!(roundtrip.knowledge_product(), predecessor);
        assert_eq!(roundtrip.knowledge_generation(), 4);

        let first_repromotion = promoting
            .plan_knowledge_shadow(
                CheckpointDigest::new([0x91; 32]),
                SubjectId::new([0x92; 32]),
                &roots,
                &support_facts,
                allowance,
            )
            .unwrap()
            .unwrap();
        assert_eq!(first_repromotion.product(), promoted_product);
        let (first_reopen, first_reopened) =
            promoting.open_knowledge_shadow(first_repromotion).unwrap();
        first_reopen.commit(&mut promoting).unwrap();
        promoting
            .settle_knowledge_shadow(
                first_reopened,
                KnowledgeShadowReport::paired(treatment, control),
            )
            .unwrap()
            .commit(&mut promoting)
            .unwrap();
        assert_eq!(
            promoting.knowledge_product().identity(),
            promoted_product.identity()
        );

        let second_repromotion = promoting
            .plan_knowledge_shadow(
                CheckpointDigest::new([0xa1; 32]),
                SubjectId::new([0xa2; 32]),
                &roots,
                &[],
                allowance,
            )
            .unwrap()
            .unwrap();
        assert_eq!(second_repromotion.product(), second_product);
        let (second_reopen, second_reopened) =
            promoting.open_knowledge_shadow(second_repromotion).unwrap();
        second_reopen.commit(&mut promoting).unwrap();
        promoting
            .settle_knowledge_shadow(
                second_reopened,
                KnowledgeShadowReport::paired(treatment, control),
            )
            .unwrap()
            .commit(&mut promoting)
            .unwrap();
        assert_eq!(
            promoting.knowledge_product().identity(),
            second_product.identity()
        );
        assert_eq!(promoting.knowledge_generation(), 6);
    }

    fn legacy_head(axis: ForecastAxis) -> usize {
        match axis {
            ForecastAxis::ImmediateImprovement => 0,
            ForecastAxis::UsefulDescendants => 1,
            ForecastAxis::CrossGoalLeverage => 2,
            ForecastAxis::CompressionValue => 3,
            ForecastAxis::KernelAcceptance => 4,
            ForecastAxis::VerificationCost => 5,
            ForecastAxis::DeadEndRisk => 6,
            ForecastAxis::InformationValue | ForecastAxis::Novelty => {
                panic!("the migrated legacy model has no {axis:?} head")
            }
        }
    }

    fn legacy_ftrl_feature_vectors() -> Vec<[f32; FEATURE_COUNT]> {
        (0..4)
            .map(|case| {
                std::array::from_fn(|feature| {
                    let magnitude = f32::from(
                        u16::try_from((case + 1) * (feature + 3)).expect("fixture value fits u16"),
                    ) / 16.0;
                    if (case + feature).is_multiple_of(3) {
                        -magnitude
                    } else {
                        magnitude
                    }
                })
            })
            .collect()
    }

    #[test]
    fn legacy_ftrl_import_preserves_all_forecasts_ordering_lineage_and_restart_identity() {
        let limits = IntelligenceLimits::new(8, 8, 32, 256, 4, 64 * 1024).unwrap();
        let legacy_model = FtrlModel::deterministic_conversion_fixture();
        let migrated = migrated_v12_core(limits);
        let empty_ecology_identity = migrated.model_ecology_identity();
        let imported = migrated
            .into_legacy_ftrl(legacy_model.conversion_view())
            .unwrap();
        let imported_checkpoint = imported.checkpoint();
        let imported_id = imported.manifest().active_ids().next().unwrap();
        assert_ne!(imported.model_ecology_identity(), empty_ecology_identity);
        assert_eq!(imported.manifest().active_ids().count(), 1);
        assert_eq!(&imported_checkpoint.as_bytes()[..5], b"RFIC\x11");

        let family = RoutingFamilyId::new([0x6a; 32]);
        let feature_vectors = legacy_ftrl_feature_vectors();
        let mut market = MarketArena::with_capacity(limits);
        for (index, features) in feature_vectors.iter().enumerate() {
            let opportunity = market
                .push_opportunity(
                    OpportunitySpec::candidate_in_family(
                        [u8::try_from(index).unwrap(); 32],
                        OpportunitySpec::candidate_schema(),
                        family,
                    ),
                    features,
                )
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    u32::try_from(index).unwrap(),
                ))
                .unwrap();
        }
        let expected_adaptive = (1..feature_vectors.len())
            .min_by(|left, right| {
                compare_forecasts(
                    legacy_model.forecast(Features(feature_vectors[*left])),
                    legacy_model.forecast(Features(feature_vectors[*right])),
                )
                .then_with(|| left.cmp(right))
            })
            .unwrap();
        let mut output = PortfolioBuffer::with_capacity(limits);
        let portfolio = imported
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(2, 2, 0, 2, 2)),
                2,
                &mut output,
            )
            .unwrap();
        assert_eq!(
            portfolio.investment_ids().collect::<Vec<_>>(),
            [
                InvestmentId(0),
                InvestmentId(u32::try_from(expected_adaptive).unwrap())
            ]
        );
        assert_eq!(
            portfolio.allocations()[1].source(),
            AllocationSource::Specialist(imported_id)
        );
        let expected = legacy_model.forecast(Features(feature_vectors[expected_adaptive]));
        assert_eq!(portfolio.receipts()[1].forecasts().len(), 7);
        for forecast in portfolio.receipts()[1].forecasts() {
            let expected = expected.0[legacy_head(forecast.axis())];
            assert_eq!(forecast.estimate().to_bits(), expected.estimate.to_bits());
            assert_eq!(
                forecast.calibration_error().to_bits(),
                expected.calibration_error.to_bits()
            );
            assert_eq!(
                forecast.uncertainty().to_bits(),
                expected.uncertainty.to_bits()
            );
        }

        let recovered = IntelligenceCore::restore(imported_checkpoint.as_bytes()).unwrap();
        assert_eq!(recovered.checkpoint(), imported_checkpoint);
        assert_eq!(
            recovered.model_ecology_identity(),
            imported.model_ecology_identity()
        );
        assert_eq!(
            recovered.manifest().active_ids().collect::<Vec<_>>(),
            [imported_id]
        );
        let independently_imported = migrated_v12_core(limits)
            .into_legacy_ftrl(legacy_model.conversion_view())
            .unwrap();
        assert_eq!(independently_imported.checkpoint(), imported_checkpoint);
        assert_eq!(
            independently_imported
                .manifest()
                .active_ids()
                .collect::<Vec<_>>(),
            [imported_id]
        );
    }

    #[test]
    fn legacy_ftrl_import_rejects_fresh_duplicate_nonempty_and_over_limit_cores() {
        let limits = IntelligenceLimits::new(8, 8, 32, 256, 4, 64 * 1024).unwrap();
        let model = FtrlModel::deterministic_conversion_fixture();
        assert_eq!(
            IntelligenceCore::fresh(limits)
                .into_legacy_ftrl(model.conversion_view())
                .unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );
        let imported = migrated_v12_core(limits)
            .into_legacy_ftrl(model.conversion_view())
            .unwrap();
        assert_eq!(
            IntelligenceCore::restore(imported.checkpoint().as_bytes())
                .unwrap()
                .into_legacy_ftrl(model.conversion_view())
                .unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );

        let mut nonempty = IntelligenceCore::fresh(limits);
        let retained = SpecialistRevision::new(
            SpecialistMandate::all(
                RoleId::new("legacy-nonempty").unwrap(),
                [ForecastAxis::KernelAcceptance],
            )
            .unwrap(),
            CompactModel::prior([
                PriorHead::new(ForecastAxis::KernelAcceptance, 0.5, 0.25, 1).unwrap()
            ])
            .unwrap(),
        )
        .unwrap();
        nonempty
            .stage(SettlementFrame::edits(&[EcologyEdit::Spawn(retained)]))
            .unwrap()
            .commit(&mut nonempty)
            .unwrap();
        let legacy_nonempty = nonempty.legacy_v12_checkpoint_for_test();
        assert_eq!(
            IntelligenceCore::restore(legacy_nonempty.as_bytes())
                .unwrap()
                .into_legacy_ftrl(model.conversion_view())
                .unwrap_err(),
            IntelligenceError::InvalidMandate
        );

        let constrained = IntelligenceLimits::new(8, 8, 8, 256, 1, 1).unwrap();
        assert_eq!(
            migrated_v12_core(constrained)
                .into_legacy_ftrl(model.conversion_view())
                .unwrap_err(),
            IntelligenceError::CapacityExceeded
        );
    }

    #[test]
    fn restored_legacy_core_consumes_authenticated_absent_learning_once() {
        let limits = IntelligenceLimits::new(8, 8, 32, 256, 4, 64 * 1024).unwrap();
        let legacy = migrated_v12_core(limits);
        let bootstrap_ecology = legacy.model_ecology_identity();
        let closed = legacy.into_legacy_learning(None).unwrap();
        assert_eq!(closed.model_ecology_identity(), bootstrap_ecology);
        assert_eq!(closed.manifest().active_ids().count(), 0);
        assert_eq!(
            closed.clone().into_legacy_learning(None).unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );
        assert_eq!(
            IntelligenceCore::fresh(limits)
                .into_legacy_learning(None)
                .unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );
        let resealed = closed
            .into_legacy_knowledge(&KnowledgeState::default())
            .unwrap();
        assert_eq!(
            IntelligenceCore::restore(resealed.checkpoint().as_bytes())
                .unwrap()
                .into_legacy_learning(None)
                .unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );
    }

    #[test]
    fn legacy_ftrl_and_policy_imports_preserve_independent_one_time_provenance() {
        let limits = IntelligenceLimits::new(8, 8, 32, 256, 4, 64 * 1024).unwrap();
        let model = FtrlModel::deterministic_conversion_fixture();
        let migrated = migrated_v12_core(limits);

        let model_then_policy = migrated
            .clone()
            .into_legacy_ftrl(model.conversion_view())
            .unwrap()
            .into_legacy_runtime_policy(&RuntimePolicyState::bootstrap())
            .unwrap();
        let policy_then_model = migrated
            .into_legacy_runtime_policy(&RuntimePolicyState::bootstrap())
            .unwrap()
            .into_legacy_ftrl(model.conversion_view())
            .unwrap();

        assert_eq!(
            model_then_policy.checkpoint(),
            policy_then_model.checkpoint()
        );
    }

    #[test]
    fn v20_import_core_requires_model_and_knowledge_then_closes_all_import_eligibility() {
        let limits = IntelligenceLimits::new(8, 8, 32, 256, 4, 64 * 1024).unwrap();
        let model = FtrlModel::deterministic_conversion_fixture();
        let knowledge = KnowledgeState::default();
        let bootstrap_ecology = IntelligenceCore::fresh(limits).model_ecology_identity();
        assert_eq!(
            IntelligenceCore::fresh_for_legacy_import(limits)
                .unwrap()
                .finalize()
                .unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );
        assert_eq!(
            IntelligenceCore::fresh_for_legacy_import(limits)
                .unwrap()
                .into_legacy_runtime_policy(&RuntimePolicyState::bootstrap())
                .unwrap()
                .finalize()
                .unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );

        let model_then_policy = IntelligenceCore::fresh_for_legacy_import(limits)
            .unwrap()
            .into_legacy_learning(Some(model.conversion_view()))
            .unwrap()
            .into_legacy_runtime_policy(&RuntimePolicyState::bootstrap())
            .unwrap()
            .into_legacy_knowledge(&knowledge)
            .unwrap()
            .finalize()
            .unwrap();
        let policy_then_model = IntelligenceCore::fresh_for_legacy_import(limits)
            .unwrap()
            .into_legacy_knowledge(&knowledge)
            .unwrap()
            .into_legacy_runtime_policy(&RuntimePolicyState::bootstrap())
            .unwrap()
            .into_legacy_ftrl(model.conversion_view())
            .unwrap()
            .finalize()
            .unwrap();
        assert_eq!(
            model_then_policy.checkpoint(),
            policy_then_model.checkpoint()
        );

        let no_model = IntelligenceCore::fresh_for_legacy_import(limits)
            .unwrap()
            .into_legacy_learning(None)
            .unwrap();
        let Err(duplicate_close) = no_model.into_legacy_learning(None) else {
            panic!("legacy model absence can only be consumed once");
        };
        assert_eq!(duplicate_close, IntelligenceError::IncompatibleRevision);
        let no_model = IntelligenceCore::fresh_for_legacy_import(limits)
            .unwrap()
            .into_legacy_learning(None)
            .unwrap()
            .into_legacy_knowledge(&knowledge)
            .unwrap()
            .finalize()
            .unwrap();
        assert_eq!(no_model.model_ecology_identity(), bootstrap_ecology);
        assert_eq!(no_model.manifest().active_ids().count(), 0);
        assert_eq!(
            IntelligenceCore::restore(no_model.checkpoint().as_bytes())
                .unwrap()
                .checkpoint(),
            no_model.checkpoint()
        );
        assert_eq!(
            model_then_policy
                .clone()
                .into_legacy_ftrl(model.conversion_view())
                .unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );
        assert_eq!(
            model_then_policy
                .clone()
                .into_legacy_runtime_policy(&RuntimePolicyState::bootstrap())
                .unwrap_err(),
            IntelligenceError::InvalidPolicy
        );
        assert_eq!(
            model_then_policy
                .into_legacy_knowledge(&knowledge)
                .unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );

        let recovered = IntelligenceCore::restore(policy_then_model.checkpoint().as_bytes())
            .expect("the finalized v20 import is a current authenticated checkpoint");
        assert_eq!(
            recovered
                .into_legacy_ftrl(model.conversion_view())
                .unwrap_err(),
            IntelligenceError::IncompatibleRevision
        );
    }

    #[test]
    fn authenticated_delta_replay_rejects_an_invalid_intermediate_ecology_edit() {
        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 64 * 1024).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let causal_delta = core
            .experience
            .prepare_delta(&[], &[], &[], &[], limits)
            .unwrap();
        let mut ecology_delta = Vec::new();
        ModelEcology::encode_edits(
            &[EcologyEdit::Retire(SpecialistRevisionId::new([0x5a; 32]))],
            &mut ecology_delta,
        );
        let record = encode_delta_record(Some(&ecology_delta), &causal_delta, None, None);
        let roots = ComponentRoots {
            ecology: extend_component_root(b"ecology", core.roots.ecology, &ecology_delta),
            experience: causal_delta.canonical_root(),
            knowledge: core.roots.knowledge,
            policy: core.roots.policy,
        };
        let revision = revision_digest(limits, roots);
        let log_root = checkpoint_log_root(core.checkpoint_log_root, &record);
        let mut hostile = core.checkpoint_bytes.as_ref().clone();
        append_delta_checkpoint(&mut hostile, &record, 2, roots, revision, log_root);

        assert_eq!(
            IntelligenceCore::restore(&hostile).unwrap_err(),
            IntelligenceError::CorruptState
        );
    }

    #[test]
    fn authenticated_delta_replay_rejects_an_invalid_policy_comparison() {
        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 64 * 1024).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let causal_delta = core
            .experience
            .prepare_delta(&[], &[], &[], &[], limits)
            .unwrap();
        let update = PolicyUpdate::comparison(
            ShadowCampaignId::from_identity([99; 32]),
            core.policy.active(),
        );
        let mut policy_delta = Vec::new();
        update.encode_canonical(&mut policy_delta);
        let record = encode_delta_record(None, &causal_delta, None, Some(&policy_delta));
        let roots = ComponentRoots {
            ecology: core.roots.ecology,
            experience: causal_delta.canonical_root(),
            knowledge: core.roots.knowledge,
            policy: extend_component_root(b"policy", core.roots.policy, &policy_delta),
        };
        let revision = revision_digest(limits, roots);
        let log_root = checkpoint_log_root(core.checkpoint_log_root, &record);
        let mut hostile = core.checkpoint_bytes.as_ref().clone();
        append_delta_checkpoint(&mut hostile, &record, 2, roots, revision, log_root);

        assert_eq!(
            IntelligenceCore::restore(&hostile).unwrap_err(),
            IntelligenceError::CorruptState
        );
    }
}
