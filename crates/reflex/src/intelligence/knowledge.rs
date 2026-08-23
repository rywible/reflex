use std::collections::BTreeSet;
use std::sync::Arc;

use sha2::{Digest, Sha256};

use super::arena::OpportunityKind;
use super::causal::{
    CausalEvidence, CausalLedger, ContextualContrast, DecisionId, InvestmentOutcome,
    InvestmentReceipt, InvestmentSettlement, ShadowArm, ShadowCampaignId, ShadowCampaignSpec,
    ShadowInvalidationReason, TypedOutcome,
};
use super::codec::Decoder;
use super::forecast::ForecastAxis;
use super::types::{IntelligenceError, IntelligenceLimits, ResourceVector, SubjectId};
use crate::knowledge::{
    ConsolidationActivationError, ConsolidationChallenger, ConsolidationProduct,
    ConsolidationRollbackError, DerivationObservation, DerivedOperator, KnowledgeRevision,
    KnowledgeState,
};

const MAXIMUM_RECIPE_MEMBERS: usize = 64;
const MAXIMUM_PROMOTION_REQUIREMENTS: usize = 32;
const MAXIMUM_KNOWLEDGE_PRODUCT_BYTES: usize = 64 * 1024 * 1024;
const MINIMUM_KNOWLEDGE_RECORD_BYTES: usize = 148;
const MINIMUM_INVESTMENT_RECEIPT_BYTES: usize = 1;

#[derive(Clone, Copy)]
enum KnowledgeWireFormat {
    LegacyV6,
    LegacyV7,
    V8,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct KnowledgeChallengerId([u8; 32]);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KnowledgeObligationWork {
    subject: SubjectId,
    operator_steps: Box<[Box<[u8]>]>,
    supporting_attempts: Box<[[u8; 32]]>,
}

impl KnowledgeObligationWork {
    pub(crate) const fn subject(&self) -> SubjectId {
        self.subject
    }

    pub(crate) fn operator_steps(&self) -> &[Box<[u8]>] {
        &self.operator_steps
    }

    pub(crate) fn supporting_attempts(&self) -> &[[u8; 32]] {
        &self.supporting_attempts
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KnowledgeRecoveryObligation {
    binding: SubjectId,
    work: KnowledgeObligationWork,
}

impl KnowledgeRecoveryObligation {
    pub(crate) const fn work(&self) -> &KnowledgeObligationWork {
        &self.work
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KnowledgeRecoveryManifest {
    core_revision: [u8; 32],
    knowledge_root: [u8; 32],
    identity: SubjectId,
    obligations: Box<[KnowledgeRecoveryObligation]>,
}

impl KnowledgeRecoveryManifest {
    pub(crate) const fn obligation_count(&self) -> usize {
        self.obligations.len()
    }

    pub(crate) const fn obligations(&self) -> &[KnowledgeRecoveryObligation] {
        &self.obligations
    }

    pub(super) fn matches(&self, core_revision: [u8; 32], knowledge_root: [u8; 32]) -> bool {
        self.core_revision == core_revision
            && self.knowledge_root == knowledge_root
            && self.identity
                == recovery_manifest_identity(core_revision, knowledge_root, &self.obligations)
    }

    #[cfg(test)]
    pub(super) fn heap_bytes(&self) -> usize {
        self.obligations
            .len()
            .saturating_mul(std::mem::size_of::<KnowledgeRecoveryObligation>())
            .saturating_add(self.obligations.iter().fold(0_usize, |bytes, obligation| {
                bytes
                    .saturating_add(
                        obligation
                            .work
                            .operator_steps
                            .len()
                            .saturating_mul(std::mem::size_of::<Box<[u8]>>()),
                    )
                    .saturating_add(
                        obligation
                            .work
                            .operator_steps
                            .iter()
                            .map(|step| step.len())
                            .sum::<usize>(),
                    )
                    .saturating_add(
                        obligation
                            .work
                            .supporting_attempts
                            .len()
                            .saturating_mul(std::mem::size_of::<[u8; 32]>()),
                    )
            }))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KnowledgeWork {
    pub(super) base_revision: [u8; 32],
    pub(super) challenger: KnowledgeChallenger,
    obligations: Box<[KnowledgeObligationWork]>,
}

impl KnowledgeWork {
    pub(super) fn verification(
        base_revision: [u8; 32],
        consolidation: &ConsolidationChallenger,
    ) -> Result<Self, IntelligenceError> {
        let obligations = recovery_obligations(consolidation)?;
        let product = SubjectId::new(consolidation.product().identity());
        let mut sources = vec![
            SubjectId::new(consolidation.source().digest()),
            SubjectId::new(consolidation.support().digest()),
        ];
        sources.sort_unstable();
        sources.dedup();
        let promotion = PromotionGate::all([
            ContextualRequirement::new(ForecastAxis::UsefulDescendants, 0.01)?,
            ContextualRequirement::new(ForecastAxis::CompressionValue, 0.0)?,
        ])?;
        let challenger = KnowledgeChallenger::new(
            CompilerRecipe::DeriveOperator,
            sources,
            KnowledgeProduct::derived_operator(product).with_bytes(consolidation.encode())?,
            obligations.iter().map(KnowledgeObligationWork::subject),
            promotion,
        )?;
        Ok(Self {
            base_revision,
            challenger,
            obligations: obligations.into_boxed_slice(),
        })
    }

    pub(crate) const fn id(&self) -> KnowledgeChallengerId {
        self.challenger.id
    }

    #[cfg(test)]
    pub(crate) const fn product(&self) -> SubjectId {
        self.challenger.product.subject
    }

    pub(crate) fn obligations(&self) -> &[KnowledgeObligationWork] {
        &self.obligations
    }

    pub(super) fn heap_bytes(&self) -> usize {
        self.challenger
            .heap_bytes()
            .saturating_add(self.obligations.iter().fold(0_usize, |bytes, obligation| {
                bytes
                    .saturating_add(std::mem::size_of::<KnowledgeObligationWork>())
                    .saturating_add(obligation.operator_steps.iter().fold(
                        0_usize,
                        |steps, step| {
                            steps
                                .saturating_add(std::mem::size_of::<Box<[u8]>>())
                                .saturating_add(step.len())
                        },
                    ))
                    .saturating_add(
                        obligation
                            .supporting_attempts
                            .len()
                            .saturating_mul(std::mem::size_of::<[u8; 32]>()),
                    )
            }))
    }

    pub(super) fn from_challenger(
        base_revision: [u8; 32],
        challenger: &KnowledgeChallenger,
    ) -> Result<Self, IntelligenceError> {
        let consolidation = decode_consolidation_product(challenger)?;
        let work = Self::verification(base_revision, &consolidation)?;
        if work.challenger != *challenger {
            return Err(IntelligenceError::InvalidKnowledge);
        }
        Ok(work)
    }
}

fn recovery_obligations(
    consolidation: &ConsolidationChallenger,
) -> Result<Vec<KnowledgeObligationWork>, IntelligenceError> {
    consolidation
        .obligations()
        .iter()
        .map(|obligation| recovery_obligation(consolidation, *obligation))
        .collect()
}

fn recovery_obligation(
    consolidation: &ConsolidationChallenger,
    obligation: crate::knowledge::ConsolidationObligation,
) -> Result<KnowledgeObligationWork, IntelligenceError> {
    let operator = consolidation
        .operator_for(obligation)
        .ok_or(IntelligenceError::InvalidKnowledge)?;
    let supporting_attempts = consolidation
        .supporting_attempts(obligation)
        .ok_or(IntelligenceError::InvalidKnowledge)?;
    Ok(KnowledgeObligationWork {
        subject: SubjectId::new(obligation.id()),
        operator_steps: operator
            .steps()
            .iter()
            .map(|step| step.clone().into_boxed_slice())
            .collect(),
        supporting_attempts: supporting_attempts.into(),
    })
}

fn retained_revision_obligation(
    product: ConsolidationProduct,
    operator: &DerivedOperator,
) -> KnowledgeObligationWork {
    KnowledgeObligationWork {
        subject: SubjectId::new(product.obligation_identity(operator.id())),
        operator_steps: operator
            .steps()
            .iter()
            .map(|step| step.clone().into_boxed_slice())
            .collect(),
        supporting_attempts: operator.support().into(),
    }
}

fn recovery_obligation_binding(
    core_revision: [u8; 32],
    knowledge_root: [u8; 32],
    challenger: KnowledgeChallengerId,
    record_index: usize,
    obligation_index: usize,
    work: &KnowledgeObligationWork,
) -> Result<SubjectId, IntelligenceError> {
    let record_index =
        u64::try_from(record_index).map_err(|_| IntelligenceError::ResourceOverflow)?;
    let obligation_index =
        u64::try_from(obligation_index).map_err(|_| IntelligenceError::ResourceOverflow)?;
    let mut digest = Sha256::new();
    digest.update(b"reflex-knowledge-recovery-obligation-v1\0");
    digest.update(core_revision);
    digest.update(knowledge_root);
    digest.update(challenger.0);
    digest.update(record_index.to_le_bytes());
    digest.update(obligation_index.to_le_bytes());
    digest.update(work.subject.identity());
    digest.update((work.operator_steps.len() as u64).to_le_bytes());
    for step in &work.operator_steps {
        digest.update((step.len() as u64).to_le_bytes());
        digest.update(step);
    }
    digest.update((work.supporting_attempts.len() as u64).to_le_bytes());
    for attempt in &work.supporting_attempts {
        digest.update(attempt);
    }
    Ok(SubjectId::new(digest.finalize().into()))
}

fn recovery_manifest_identity(
    core_revision: [u8; 32],
    knowledge_root: [u8; 32],
    obligations: &[KnowledgeRecoveryObligation],
) -> SubjectId {
    let mut digest = Sha256::new();
    digest.update(b"reflex-knowledge-recovery-manifest-v1\0");
    digest.update(core_revision);
    digest.update(knowledge_root);
    digest.update((obligations.len() as u64).to_le_bytes());
    for obligation in obligations {
        digest.update(obligation.binding.identity());
    }
    SubjectId::new(digest.finalize().into())
}

#[derive(Clone, Debug, PartialEq)]
struct PendingKnowledgeVerification {
    work: KnowledgeWork,
    receipts: Box<[InvestmentReceipt]>,
    reserved: ResourceVector,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct OpenedKnowledgeWork {
    pub(super) opened_revision: [u8; 32],
    work: KnowledgeWork,
    receipts: Box<[InvestmentReceipt]>,
    reserved: ResourceVector,
}

impl OpenedKnowledgeWork {
    pub(super) const fn work(&self) -> &KnowledgeWork {
        &self.work
    }

    pub(crate) fn receipts(&self) -> &[InvestmentReceipt] {
        &self.receipts
    }

    pub(crate) const fn reserved_resources(&self) -> ResourceVector {
        self.reserved
    }

    #[cfg(test)]
    pub(crate) const fn reserved_verification_requests(&self) -> u64 {
        self.reserved.verification_requests
    }

    pub(crate) const fn id(&self) -> KnowledgeChallengerId {
        self.work.id()
    }

    pub(super) fn heap_bytes(&self) -> usize {
        self.work.heap_bytes().saturating_add(
            self.receipts
                .len()
                .saturating_mul(std::mem::size_of::<InvestmentReceipt>()),
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) enum KnowledgeVerificationReport<'a> {
    Settled(&'a [InvestmentSettlement]),
    Interrupted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KnowledgePlanningFailure {
    WitnessUnavailable(SubjectId),
}

impl<'a> KnowledgeVerificationReport<'a> {
    pub(crate) const fn settled(settlements: &'a [InvestmentSettlement]) -> Self {
        Self::Settled(settlements)
    }

    pub(crate) const fn interrupted() -> Self {
        Self::Interrupted
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KnowledgeShadowRootFact {
    root: SubjectId,
    claim: SubjectId,
    preference_rank: u32,
}

impl KnowledgeShadowRootFact {
    pub(crate) const fn new(root: SubjectId, claim: SubjectId, preference_rank: u32) -> Self {
        Self {
            root,
            claim,
            preference_rank,
        }
    }

    pub(super) const fn selection_key(self, ordinal: usize) -> (u32, usize) {
        (self.preference_rank, ordinal)
    }

    pub(super) const fn root(self) -> SubjectId {
        self.root
    }

    pub(super) const fn claim(self) -> SubjectId {
        self.claim
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KnowledgeShadowSupportFact {
    attempt: [u8; 32],
    claim: SubjectId,
}

impl KnowledgeShadowSupportFact {
    pub(crate) const fn new(attempt: [u8; 32], claim: SubjectId) -> Self {
        Self { attempt, claim }
    }

    pub(super) const fn attempt(self) -> [u8; 32] {
        self.attempt
    }

    pub(super) const fn claim(self) -> SubjectId {
        self.claim
    }
}

#[derive(Clone, Debug)]
pub(crate) struct KnowledgeShadowPlan {
    pub(super) base_revision: [u8; 32],
    pub(super) challenger: KnowledgeChallengerId,
    pub(super) campaign: ShadowCampaignSpec,
    pub(super) root: SubjectId,
    pub(super) scheduling_token: SubjectId,
    pub(super) subject_operator: Box<[u8]>,
    pub(super) supporting_attempts: Box<[[u8; 32]]>,
    pub(super) treatment: Arc<KnowledgeRevision>,
    pub(super) control: Arc<KnowledgeRevision>,
}

impl KnowledgeShadowPlan {
    #[cfg(test)]
    pub(crate) const fn product(&self) -> SubjectId {
        self.campaign.subject()
    }

    pub(crate) const fn root(&self) -> SubjectId {
        self.root
    }

    pub(crate) const fn per_arm_allowance(&self) -> ResourceVector {
        self.campaign.resources()
    }

    pub(super) fn into_opened(self, opened_revision: [u8; 32]) -> KnowledgeShadowOpened {
        KnowledgeShadowOpened {
            opened_revision,
            challenger: self.challenger,
            campaign: self.campaign,
            scheduling_token: self.scheduling_token,
            subject_operator: self.subject_operator,
            supporting_attempts: self.supporting_attempts,
            treatment: self.treatment,
            control: self.control,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct KnowledgeShadowOpened {
    pub(super) opened_revision: [u8; 32],
    pub(super) challenger: KnowledgeChallengerId,
    pub(super) campaign: ShadowCampaignSpec,
    pub(super) scheduling_token: SubjectId,
    pub(super) subject_operator: Box<[u8]>,
    pub(super) supporting_attempts: Box<[[u8; 32]]>,
    pub(super) treatment: Arc<KnowledgeRevision>,
    pub(super) control: Arc<KnowledgeRevision>,
}

impl KnowledgeShadowOpened {
    pub(crate) const fn campaign(&self) -> ShadowCampaignId {
        self.campaign.id()
    }

    pub(super) const fn campaign_specification(&self) -> &ShadowCampaignSpec {
        &self.campaign
    }

    pub(crate) fn execution<'a>(
        &'a self,
        core: &super::core::IntelligenceCore,
    ) -> Result<KnowledgeShadowExecution<'a>, IntelligenceError> {
        core.validate_knowledge_shadow_opened(self)?;
        Ok(KnowledgeShadowExecution { opened: self })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct KnowledgeShadowExecution<'a> {
    opened: &'a KnowledgeShadowOpened,
}

impl<'a> KnowledgeShadowExecution<'a> {
    pub(crate) const fn product(self) -> SubjectId {
        self.opened.campaign.subject()
    }

    pub(crate) const fn per_arm_allowance(self) -> ResourceVector {
        self.opened.campaign.resources()
    }

    pub(crate) const fn random_stream(self) -> SubjectId {
        self.opened.campaign.random_stream()
    }

    pub(crate) const fn scheduling_token(self) -> SubjectId {
        self.opened.scheduling_token
    }

    pub(crate) fn subject_operator(self) -> &'a [u8] {
        &self.opened.subject_operator
    }

    pub(crate) fn supporting_attempts(self) -> &'a [[u8; 32]] {
        &self.opened.supporting_attempts
    }

    pub(crate) fn pinned_revision(self, arm: ShadowArm) -> &'a KnowledgeRevision {
        match arm {
            ShadowArm::Treatment => self.opened.treatment.as_ref(),
            ShadowArm::Control => self.opened.control.as_ref(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct KnowledgeShadowArmReport {
    arm: ShadowArm,
    actual_resources: ResourceVector,
    useful_descendants: f32,
    compression_value: f32,
}

impl KnowledgeShadowArmReport {
    pub(crate) fn new(
        arm: ShadowArm,
        actual_resources: ResourceVector,
        useful_descendants: f32,
        compression_value: f32,
    ) -> Result<Self, IntelligenceError> {
        if !useful_descendants.is_finite() || !compression_value.is_finite() {
            return Err(IntelligenceError::InvalidForecast);
        }
        Ok(Self {
            arm,
            actual_resources,
            useful_descendants,
            compression_value,
        })
    }

    pub(super) const fn arm(self) -> ShadowArm {
        self.arm
    }

    pub(super) const fn actual_resources(self) -> ResourceVector {
        self.actual_resources
    }

    pub(super) fn outcomes(self) -> Result<[TypedOutcome; 2], IntelligenceError> {
        Ok([
            TypedOutcome::new(ForecastAxis::UsefulDescendants, self.useful_descendants)?,
            TypedOutcome::new(ForecastAxis::CompressionValue, self.compression_value)?,
        ])
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum KnowledgeShadowReport {
    Paired {
        treatment: KnowledgeShadowArmReport,
        control: KnowledgeShadowArmReport,
    },
    Invalidated(ShadowInvalidationReason),
}

impl KnowledgeShadowReport {
    pub(crate) const fn paired(
        treatment: KnowledgeShadowArmReport,
        control: KnowledgeShadowArmReport,
    ) -> Self {
        Self::Paired { treatment, control }
    }

    pub(crate) const fn invalidated(reason: ShadowInvalidationReason) -> Self {
        Self::Invalidated(reason)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum CompilerRecipe {
    Deduplicate = 1,
    FactorSharedDerivation = 2,
    GeneralizeArtifact = 3,
    DeriveOperator = 4,
    RepairFailureFamily = 5,
    ComposeOperators = 6,
}

impl CompilerRecipe {
    fn decode(value: u8) -> Result<Self, ()> {
        match value {
            1 => Ok(Self::Deduplicate),
            2 => Ok(Self::FactorSharedDerivation),
            3 => Ok(Self::GeneralizeArtifact),
            4 => Ok(Self::DeriveOperator),
            5 => Ok(Self::RepairFailureFamily),
            6 => Ok(Self::ComposeOperators),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum KnowledgeProductKind {
    Artifact = 1,
    DerivedOperator = 2,
    KnowledgeIndex = 3,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum KnowledgeProductMeaning {
    Semantic = 1,
    NonSemanticIndex = 2,
}

impl KnowledgeProductMeaning {
    fn decode(value: u8) -> Result<Self, ()> {
        match value {
            1 => Ok(Self::Semantic),
            2 => Ok(Self::NonSemanticIndex),
            _ => Err(()),
        }
    }
}

impl KnowledgeProductKind {
    fn decode(value: u8) -> Result<Self, ()> {
        match value {
            1 => Ok(Self::Artifact),
            2 => Ok(Self::DerivedOperator),
            3 => Ok(Self::KnowledgeIndex),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KnowledgeProduct {
    kind: KnowledgeProductKind,
    subject: SubjectId,
    meaning: KnowledgeProductMeaning,
    bytes: Option<Arc<[u8]>>,
}

impl KnowledgeProduct {
    #[cfg(test)]
    pub(crate) const fn artifact(subject: SubjectId) -> Self {
        Self {
            kind: KnowledgeProductKind::Artifact,
            subject,
            meaning: KnowledgeProductMeaning::Semantic,
            bytes: None,
        }
    }

    pub(crate) const fn derived_operator(subject: SubjectId) -> Self {
        Self {
            kind: KnowledgeProductKind::DerivedOperator,
            subject,
            meaning: KnowledgeProductMeaning::Semantic,
            bytes: None,
        }
    }

    #[cfg(test)]
    pub(crate) const fn knowledge_index(subject: SubjectId) -> Self {
        Self {
            kind: KnowledgeProductKind::KnowledgeIndex,
            subject,
            meaning: KnowledgeProductMeaning::Semantic,
            bytes: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn nonsemantic_index(
        subject: SubjectId,
        bytes: impl Into<Box<[u8]>>,
    ) -> Result<Self, IntelligenceError> {
        let bytes = Arc::from(validate_product_bytes(bytes.into())?);
        Ok(Self {
            kind: KnowledgeProductKind::KnowledgeIndex,
            subject,
            meaning: KnowledgeProductMeaning::NonSemanticIndex,
            bytes: Some(bytes),
        })
    }

    pub(crate) fn with_bytes(
        mut self,
        bytes: impl Into<Box<[u8]>>,
    ) -> Result<Self, IntelligenceError> {
        self.bytes = Some(Arc::from(validate_product_bytes(bytes.into())?));
        Ok(self)
    }

    #[cfg(test)]
    pub(crate) const fn kind(&self) -> KnowledgeProductKind {
        self.kind
    }

    #[cfg(test)]
    pub(crate) const fn subject(&self) -> SubjectId {
        self.subject
    }

    #[cfg(test)]
    pub(crate) const fn meaning(&self) -> KnowledgeProductMeaning {
        self.meaning
    }

    pub(crate) fn bytes(&self) -> Option<&[u8]> {
        self.bytes.as_deref()
    }

    fn heap_bytes(&self) -> usize {
        self.bytes.as_ref().map_or(0, |bytes| bytes.len())
    }

    fn encode_canonical(&self, output: &mut Vec<u8>) {
        output.push(self.kind as u8);
        output.extend_from_slice(&self.subject.identity());
        output.push(self.meaning as u8);
        encode_optional_product_bytes(self.bytes.as_deref(), output);
    }

    fn encode_legacy_v7(&self, output: &mut Vec<u8>) -> Result<(), ()> {
        if self.meaning != KnowledgeProductMeaning::Semantic {
            return Err(());
        }
        output.push(self.kind as u8);
        output.extend_from_slice(&self.subject.identity());
        encode_optional_product_bytes(self.bytes.as_deref(), output);
        Ok(())
    }

    fn encode_legacy_v6(&self, output: &mut Vec<u8>) -> Result<(), ()> {
        if self.meaning != KnowledgeProductMeaning::Semantic || self.bytes.is_some() {
            return Err(());
        }
        output.push(self.kind as u8);
        output.extend_from_slice(&self.subject.identity());
        Ok(())
    }

    fn decode_canonical(input: &mut Decoder<'_>, maximum_bytes: usize) -> Result<Self, ()> {
        let product = Self {
            kind: KnowledgeProductKind::decode(input.read_u8()?)?,
            subject: SubjectId::new(input.read_digest()?),
            meaning: KnowledgeProductMeaning::decode(input.read_u8()?)?,
            bytes: decode_optional_product_bytes(input, maximum_bytes)?,
        };
        if !product.has_valid_meaning() {
            return Err(());
        }
        Ok(product)
    }

    fn decode_legacy_v7(input: &mut Decoder<'_>, maximum_bytes: usize) -> Result<Self, ()> {
        Ok(Self {
            kind: KnowledgeProductKind::decode(input.read_u8()?)?,
            subject: SubjectId::new(input.read_digest()?),
            meaning: KnowledgeProductMeaning::Semantic,
            bytes: decode_optional_product_bytes(input, maximum_bytes)?,
        })
    }

    fn decode_legacy_v6(input: &mut Decoder<'_>) -> Result<Self, ()> {
        Ok(Self {
            kind: KnowledgeProductKind::decode(input.read_u8()?)?,
            subject: SubjectId::new(input.read_digest()?),
            meaning: KnowledgeProductMeaning::Semantic,
            bytes: None,
        })
    }

    fn has_valid_meaning(&self) -> bool {
        match self.meaning {
            KnowledgeProductMeaning::Semantic => true,
            KnowledgeProductMeaning::NonSemanticIndex => {
                self.kind == KnowledgeProductKind::KnowledgeIndex && self.bytes.is_some()
            }
        }
    }

    fn is_payload_bound_nonsemantic_index(&self) -> bool {
        self.meaning == KnowledgeProductMeaning::NonSemanticIndex && self.has_valid_meaning()
    }
}

fn encode_optional_product_bytes(bytes: Option<&[u8]>, output: &mut Vec<u8>) {
    match bytes {
        Some(bytes) => {
            output.push(1);
            output.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
            output.extend_from_slice(bytes);
        }
        None => output.push(0),
    }
}

fn decode_optional_product_bytes(
    input: &mut Decoder<'_>,
    maximum_bytes: usize,
) -> Result<Option<Arc<[u8]>>, ()> {
    match input.read_u8()? {
        0 => Ok(None),
        1 => {
            let count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
            if count == 0
                || count > maximum_bytes.min(MAXIMUM_KNOWLEDGE_PRODUCT_BYTES)
                || count > input.remaining()
            {
                return Err(());
            }
            Ok(Some(Arc::from(input.take(count)?)))
        }
        _ => Err(()),
    }
}

fn validate_product_bytes(bytes: Box<[u8]>) -> Result<Box<[u8]>, IntelligenceError> {
    if bytes.is_empty() || bytes.len() > MAXIMUM_KNOWLEDGE_PRODUCT_BYTES {
        return Err(IntelligenceError::InvalidKnowledge);
    }
    Ok(bytes)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ContextualRequirement {
    axis: ForecastAxis,
    minimum_advantage: f32,
}

impl ContextualRequirement {
    pub(crate) fn new(
        axis: ForecastAxis,
        minimum_advantage: f32,
    ) -> Result<Self, IntelligenceError> {
        if !minimum_advantage.is_finite() || minimum_advantage < 0.0 {
            return Err(IntelligenceError::InvalidKnowledge);
        }
        Ok(Self {
            axis,
            minimum_advantage,
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PromotionGate {
    requirements: Vec<ContextualRequirement>,
}

impl PromotionGate {
    pub(crate) fn all(
        requirements: impl IntoIterator<Item = ContextualRequirement>,
    ) -> Result<Self, IntelligenceError> {
        let requirements = requirements.into_iter().collect::<Vec<_>>();
        let unique = requirements
            .iter()
            .map(|requirement| requirement.axis)
            .collect::<BTreeSet<_>>();
        let has_downstream_value = requirements.iter().any(|requirement| {
            matches!(
                requirement.axis,
                ForecastAxis::UsefulDescendants | ForecastAxis::CrossGoalLeverage
            ) && requirement.minimum_advantage > 0.0
        });
        if requirements.is_empty()
            || requirements.len() > MAXIMUM_PROMOTION_REQUIREMENTS
            || unique.len() != requirements.len()
            || !has_downstream_value
        {
            return Err(IntelligenceError::InvalidKnowledge);
        }
        Ok(Self { requirements })
    }

    fn satisfying_campaign(
        &self,
        subject: SubjectId,
        contrasts: &[ContextualContrast],
    ) -> Option<ShadowCampaignId> {
        contrasts
            .iter()
            .copied()
            .filter(|contrast| contrast.subject() == subject)
            .map(ContextualContrast::campaign)
            .find(|campaign| self.campaign_satisfies(*campaign, subject, contrasts))
    }

    fn campaign_satisfies(
        &self,
        campaign: ShadowCampaignId,
        subject: SubjectId,
        contrasts: &[ContextualContrast],
    ) -> bool {
        self.requirements.iter().all(|requirement| {
            contrasts.iter().copied().any(|contrast| {
                contrast.campaign() == campaign
                    && contrast.subject() == subject
                    && contrast.axis() == requirement.axis
                    && contrast.advantage() >= requirement.minimum_advantage
            })
        })
    }

    fn encode_canonical(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&(self.requirements.len() as u64).to_le_bytes());
        for requirement in &self.requirements {
            output.push(requirement.axis as u8);
            output.extend_from_slice(&requirement.minimum_advantage.to_bits().to_le_bytes());
        }
    }

    fn decode_canonical(input: &mut Decoder<'_>) -> Result<Self, ()> {
        let count = read_count(input, MAXIMUM_PROMOTION_REQUIREMENTS, 5)?;
        let mut requirements = Vec::with_capacity(count);
        for _ in 0..count {
            requirements.push(
                ContextualRequirement::new(
                    ForecastAxis::decode(input.read_u8()?)?,
                    input.read_f32()?,
                )
                .map_err(|_| ())?,
            );
        }
        Self::all(requirements).map_err(|_| ())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct KnowledgeChallenger {
    id: KnowledgeChallengerId,
    recipe: CompilerRecipe,
    sources: Vec<SubjectId>,
    product: KnowledgeProduct,
    obligations: Vec<SubjectId>,
    promotion: PromotionGate,
}

impl KnowledgeChallenger {
    pub(crate) fn new(
        recipe: CompilerRecipe,
        sources: impl IntoIterator<Item = SubjectId>,
        product: KnowledgeProduct,
        obligations: impl IntoIterator<Item = SubjectId>,
        promotion: PromotionGate,
    ) -> Result<Self, IntelligenceError> {
        let sources = sources.into_iter().collect::<Vec<_>>();
        let obligations = obligations.into_iter().collect::<Vec<_>>();
        let valid_obligations = valid_members(&obligations)
            || (obligations.is_empty() && product.is_payload_bound_nonsemantic_index());
        if !valid_members(&sources) || !valid_obligations {
            return Err(IntelligenceError::InvalidKnowledge);
        }
        let mut challenger = Self {
            id: KnowledgeChallengerId([0; 32]),
            recipe,
            sources,
            product,
            obligations,
            promotion,
        };
        challenger.id = challenger.identity();
        Ok(challenger)
    }

    #[cfg(test)]
    pub(crate) const fn id(&self) -> KnowledgeChallengerId {
        self.id
    }

    fn identity(&self) -> KnowledgeChallengerId {
        let mut encoded = Vec::new();
        self.encode_content(&mut encoded);
        let mut digest = Sha256::new();
        digest.update(b"reflex-knowledge-challenger-v3\0");
        digest.update(encoded);
        KnowledgeChallengerId(digest.finalize().into())
    }

    fn legacy_v7_identity(&self) -> Result<KnowledgeChallengerId, ()> {
        let mut encoded = Vec::new();
        self.encode_legacy_v7_content(&mut encoded)?;
        let mut digest = Sha256::new();
        digest.update(b"reflex-knowledge-challenger-v2\0");
        digest.update(encoded);
        Ok(KnowledgeChallengerId(digest.finalize().into()))
    }

    fn legacy_v6_identity(&self) -> Result<KnowledgeChallengerId, ()> {
        let mut encoded = Vec::new();
        self.encode_legacy_v6_content(&mut encoded)?;
        let mut digest = Sha256::new();
        digest.update(b"reflex-knowledge-challenger-v1\0");
        digest.update(encoded);
        Ok(KnowledgeChallengerId(digest.finalize().into()))
    }

    fn encode_content(&self, output: &mut Vec<u8>) {
        output.push(self.recipe as u8);
        encode_subjects(&self.sources, output);
        self.product.encode_canonical(output);
        encode_subjects(&self.obligations, output);
        self.promotion.encode_canonical(output);
    }

    fn encode_legacy_v7_content(&self, output: &mut Vec<u8>) -> Result<(), ()> {
        output.push(self.recipe as u8);
        encode_subjects(&self.sources, output);
        self.product.encode_legacy_v7(output)?;
        encode_subjects(&self.obligations, output);
        self.promotion.encode_canonical(output);
        Ok(())
    }

    fn encode_legacy_v6_content(&self, output: &mut Vec<u8>) -> Result<(), ()> {
        output.push(self.recipe as u8);
        encode_subjects(&self.sources, output);
        self.product.encode_legacy_v6(output)?;
        encode_subjects(&self.obligations, output);
        self.promotion.encode_canonical(output);
        Ok(())
    }

    fn encode_canonical(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.id.0);
        self.encode_content(output);
    }

    pub(super) fn canonical_encoded_len(&self) -> usize {
        let product_bytes = 1_usize
            .saturating_add(32)
            .saturating_add(1)
            .saturating_add(1)
            .saturating_add(
                self.product
                    .bytes
                    .as_ref()
                    .map_or(0, |bytes| 8_usize.saturating_add(bytes.len())),
            );
        32_usize
            .saturating_add(1)
            .saturating_add(8)
            .saturating_add(self.sources.len().saturating_mul(32))
            .saturating_add(product_bytes)
            .saturating_add(8)
            .saturating_add(self.obligations.len().saturating_mul(32))
            .saturating_add(8)
            .saturating_add(self.promotion.requirements.len().saturating_mul(5))
    }

    fn encode_legacy_v7(&self, output: &mut Vec<u8>) -> Result<(), ()> {
        output.extend_from_slice(&self.legacy_v7_identity()?.0);
        self.encode_legacy_v7_content(output)
    }

    fn encode_legacy_v6(&self, output: &mut Vec<u8>) -> Result<(), ()> {
        output.extend_from_slice(&self.legacy_v6_identity()?.0);
        self.encode_legacy_v6_content(output)
    }

    fn decode_canonical(input: &mut Decoder<'_>, maximum_product_bytes: usize) -> Result<Self, ()> {
        let expected_id = KnowledgeChallengerId(input.read_digest()?);
        let recipe = CompilerRecipe::decode(input.read_u8()?)?;
        let sources = decode_subjects(input)?;
        let product = KnowledgeProduct::decode_canonical(input, maximum_product_bytes)?;
        let obligations = decode_obligations(input, product.is_payload_bound_nonsemantic_index())?;
        let promotion = PromotionGate::decode_canonical(input)?;
        let challenger =
            Self::new(recipe, sources, product, obligations, promotion).map_err(|_| ())?;
        if challenger.id != expected_id {
            return Err(());
        }
        Ok(challenger)
    }

    fn decode_legacy_v7(input: &mut Decoder<'_>, maximum_product_bytes: usize) -> Result<Self, ()> {
        let expected_id = KnowledgeChallengerId(input.read_digest()?);
        let recipe = CompilerRecipe::decode(input.read_u8()?)?;
        let sources = decode_subjects(input)?;
        let product = KnowledgeProduct::decode_legacy_v7(input, maximum_product_bytes)?;
        let obligations = decode_obligations(input, false)?;
        let promotion = PromotionGate::decode_canonical(input)?;
        let challenger =
            Self::new(recipe, sources, product, obligations, promotion).map_err(|_| ())?;
        if challenger.legacy_v7_identity()? != expected_id {
            return Err(());
        }
        Ok(challenger)
    }

    fn decode_legacy_v6(input: &mut Decoder<'_>) -> Result<Self, ()> {
        let expected_id = KnowledgeChallengerId(input.read_digest()?);
        let recipe = CompilerRecipe::decode(input.read_u8()?)?;
        let sources = decode_subjects(input)?;
        let product = KnowledgeProduct::decode_legacy_v6(input)?;
        let obligations = decode_obligations(input, false)?;
        let promotion = PromotionGate::decode_canonical(input)?;
        let challenger =
            Self::new(recipe, sources, product, obligations, promotion).map_err(|_| ())?;
        if challenger.legacy_v6_identity()? != expected_id {
            return Err(());
        }
        Ok(challenger)
    }

    fn heap_bytes(&self) -> usize {
        self.sources
            .capacity()
            .saturating_mul(std::mem::size_of::<SubjectId>())
            .saturating_add(
                self.obligations
                    .capacity()
                    .saturating_mul(std::mem::size_of::<SubjectId>()),
            )
            .saturating_add(
                self.promotion
                    .requirements
                    .capacity()
                    .saturating_mul(std::mem::size_of::<ContextualRequirement>()),
            )
            .saturating_add(self.product.heap_bytes())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct KnowledgeReview {
    challenger: KnowledgeChallengerId,
    obligation: SubjectId,
    decision: DecisionId,
}

impl KnowledgeReview {
    pub(crate) const fn new(
        challenger: KnowledgeChallengerId,
        obligation: SubjectId,
        decision: DecisionId,
    ) -> Self {
        Self {
            challenger,
            obligation,
            decision,
        }
    }

    #[cfg(test)]
    pub(crate) const fn decision(&self) -> DecisionId {
        self.decision
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum KnowledgeUpdate {
    Propose(KnowledgeChallenger),
    Review(KnowledgeReview),
    Invalidate {
        challenger: KnowledgeChallengerId,
        reason: KnowledgeInvalidationReason,
    },
    EvaluatePromotion(KnowledgeChallengerId),
    OpenVerification {
        work: KnowledgeWork,
        receipts: Box<[InvestmentReceipt]>,
        reserved: ResourceVector,
    },
    CompleteVerification(KnowledgeChallengerId),
    InterruptVerification(KnowledgeChallengerId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum KnowledgeInvalidationReason {
    Structural = 1,
    Product = 2,
    Witness = 3,
}

impl KnowledgeInvalidationReason {
    fn decode(value: u8) -> Result<Self, ()> {
        match value {
            1 => Ok(Self::Structural),
            2 => Ok(Self::Product),
            3 => Ok(Self::Witness),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum KnowledgeStatus {
    Provisional,
    Refuted,
    Verified,
    Promoted,
    Invalidated(KnowledgeInvalidationReason),
}

#[cfg(feature = "internal-experiments")]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct KnowledgeStatistics {
    pub(crate) provisional: usize,
    pub(crate) verified: usize,
    pub(crate) promoted: usize,
    pub(crate) invalidated: usize,
}

#[derive(Clone, Debug)]
struct KnowledgeRecord {
    challenger: KnowledgeChallenger,
    consolidation: Option<Arc<ConsolidationChallenger>>,
    reviews: Vec<KnowledgeReview>,
    promoted_by: Option<ShadowCampaignId>,
    status: KnowledgeStatus,
}

#[derive(Clone, Debug, Default)]
pub(super) struct KnowledgeCompiler {
    active: KnowledgeState,
    records: Vec<KnowledgeRecord>,
    pending_verification: Option<PendingKnowledgeVerification>,
}

#[derive(Clone, Copy)]
#[cfg(test)]
pub(crate) struct KnowledgeCompilerView<'a> {
    compiler: &'a KnowledgeCompiler,
}

#[derive(Clone, Copy)]
#[cfg(test)]
pub(crate) struct KnowledgeRecordView<'a> {
    record: &'a KnowledgeRecord,
}

#[cfg(test)]
impl<'a> KnowledgeRecordView<'a> {
    pub(crate) const fn status(self) -> KnowledgeStatus {
        self.record.status
    }

    pub(crate) const fn recipe(self) -> CompilerRecipe {
        self.record.challenger.recipe
    }

    pub(crate) fn sources(self) -> &'a [SubjectId] {
        &self.record.challenger.sources
    }

    pub(crate) const fn product(self) -> &'a KnowledgeProduct {
        &self.record.challenger.product
    }

    pub(crate) fn obligations(self) -> &'a [SubjectId] {
        &self.record.challenger.obligations
    }

    pub(crate) fn reviews(self) -> &'a [KnowledgeReview] {
        &self.record.reviews
    }

    #[cfg(test)]
    pub(crate) const fn promoted_by(self) -> Option<ShadowCampaignId> {
        self.record.promoted_by
    }

    #[cfg(test)]
    pub(crate) const fn invalidation_reason(self) -> Option<KnowledgeInvalidationReason> {
        match self.record.status {
            KnowledgeStatus::Invalidated(reason) => Some(reason),
            KnowledgeStatus::Provisional
            | KnowledgeStatus::Refuted
            | KnowledgeStatus::Verified
            | KnowledgeStatus::Promoted => None,
        }
    }
}

#[cfg(test)]
impl<'a> KnowledgeCompilerView<'a> {
    pub(crate) fn status(self, id: KnowledgeChallengerId) -> Option<KnowledgeStatus> {
        self.record(id).map(KnowledgeRecordView::status)
    }

    pub(crate) fn record(self, id: KnowledgeChallengerId) -> Option<KnowledgeRecordView<'a>> {
        self.compiler
            .records
            .iter()
            .find(|record| record.challenger.id == id)
            .map(|record| KnowledgeRecordView { record })
    }

    pub(crate) fn records(self) -> impl ExactSizeIterator<Item = KnowledgeRecordView<'a>> + 'a {
        self.compiler
            .records
            .iter()
            .map(|record| KnowledgeRecordView { record })
    }
}

impl KnowledgeCompiler {
    #[cfg(feature = "internal-experiments")]
    pub(super) fn without_derived_operators_for_experiment(&self) -> Self {
        Self {
            active: self.active.without_derived_operators_for_experiment(),
            // Retaining compiler records would let the no-Derived-Operator
            // treatment reactivate the very products it masks. The frozen
            // treatment retains verified Experience and active Artifact
            // scheduling, but starts with no executable or promotable Derived
            // Operator knowledge.
            records: Vec::new(),
            pending_verification: None,
        }
    }

    #[cfg(test)]
    pub(super) const fn view(&self) -> KnowledgeCompilerView<'_> {
        KnowledgeCompilerView { compiler: self }
    }

    pub(super) fn pinned_revision(&self) -> &KnowledgeRevision {
        self.active.pinned_revision()
    }

    pub(super) fn pinned_revision_arc(&self) -> Arc<KnowledgeRevision> {
        self.active.pinned_revision_arc()
    }

    pub(super) fn product(&self) -> ConsolidationProduct {
        self.active.product()
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    pub(super) const fn generation(&self) -> u64 {
        self.active.generation()
    }

    pub(super) fn validate_active(
        &self,
        artifact_keys: &BTreeSet<[u8; 32]>,
        observations: &[DerivationObservation],
        primitive_symbols: &BTreeSet<Vec<u8>>,
    ) -> bool {
        self.active
            .validate(artifact_keys, observations, primitive_symbols)
    }

    pub(super) fn recovery_manifest(
        &self,
        core_revision: [u8; 32],
        knowledge_root: [u8; 32],
        limits: IntelligenceLimits,
    ) -> Result<KnowledgeRecoveryManifest, IntelligenceError> {
        let obligation_count = self.recovery_obligation_count(limits)?;
        let mut obligations = Vec::with_capacity(obligation_count);
        for (record_index, record) in self.records.iter().enumerate() {
            if !matches!(
                record.status,
                KnowledgeStatus::Verified | KnowledgeStatus::Promoted
            ) {
                continue;
            }
            let Some(consolidation) = record.consolidation.as_deref() else {
                continue;
            };
            if consolidation.obligations().len() != record.challenger.obligations.len() {
                return Err(IntelligenceError::InvalidKnowledge);
            }
            for (obligation_index, (obligation, expected_subject)) in consolidation
                .obligations()
                .iter()
                .zip(&record.challenger.obligations)
                .enumerate()
            {
                let work = recovery_obligation(consolidation, *obligation)?;
                if work.subject() != *expected_subject {
                    return Err(IntelligenceError::InvalidKnowledge);
                }
                let binding = recovery_obligation_binding(
                    core_revision,
                    knowledge_root,
                    record.challenger.id,
                    record_index,
                    obligation_index,
                    &work,
                )?;
                obligations.push(KnowledgeRecoveryObligation { binding, work });
            }
        }
        for (revision_index, (product, revision)) in self.active.retained_revisions().enumerate() {
            if self.records.iter().any(|record| {
                matches!(
                    record.status,
                    KnowledgeStatus::Verified | KnowledgeStatus::Promoted
                ) && record
                    .consolidation
                    .as_ref()
                    .is_some_and(|challenger| challenger.product() == product)
            }) {
                continue;
            }
            let record_index = self
                .records
                .len()
                .checked_add(revision_index)
                .ok_or(IntelligenceError::ResourceOverflow)?;
            for (obligation_index, operator) in revision.operators().iter().enumerate() {
                let work = retained_revision_obligation(product, operator);
                let binding = recovery_obligation_binding(
                    core_revision,
                    knowledge_root,
                    KnowledgeChallengerId(product.identity()),
                    record_index,
                    obligation_index,
                    &work,
                )?;
                obligations.push(KnowledgeRecoveryObligation { binding, work });
            }
        }
        if obligations.len() != obligation_count {
            return Err(IntelligenceError::InvalidKnowledge);
        }
        let identity = recovery_manifest_identity(core_revision, knowledge_root, &obligations);
        Ok(KnowledgeRecoveryManifest {
            core_revision,
            knowledge_root,
            identity,
            obligations: obligations.into_boxed_slice(),
        })
    }

    pub(super) fn recovery_manifest_resident_bytes(
        &self,
        limits: IntelligenceLimits,
    ) -> Result<usize, IntelligenceError> {
        let obligation_count = self.recovery_obligation_count(limits)?;
        let mut bytes = obligation_count
            .checked_mul(std::mem::size_of::<KnowledgeRecoveryObligation>())
            .ok_or(IntelligenceError::ResourceOverflow)?;
        for record in &self.records {
            if !matches!(
                record.status,
                KnowledgeStatus::Verified | KnowledgeStatus::Promoted
            ) {
                continue;
            }
            let Some(consolidation) = record.consolidation.as_deref() else {
                continue;
            };
            for obligation in consolidation.obligations() {
                let operator = consolidation
                    .operator_for(*obligation)
                    .ok_or(IntelligenceError::InvalidKnowledge)?;
                let supporting_attempts = consolidation
                    .supporting_attempts(*obligation)
                    .ok_or(IntelligenceError::InvalidKnowledge)?;
                bytes = bytes
                    .checked_add(
                        operator
                            .steps()
                            .len()
                            .checked_mul(std::mem::size_of::<Box<[u8]>>())
                            .ok_or(IntelligenceError::ResourceOverflow)?,
                    )
                    .and_then(|total| {
                        operator
                            .steps()
                            .iter()
                            .try_fold(total, |total, step| total.checked_add(step.len()))
                    })
                    .and_then(|total| {
                        total.checked_add(
                            supporting_attempts
                                .len()
                                .checked_mul(std::mem::size_of::<[u8; 32]>())?,
                        )
                    })
                    .ok_or(IntelligenceError::ResourceOverflow)?;
            }
        }
        for (product, revision) in self.active.retained_revisions() {
            if self.records.iter().any(|record| {
                matches!(
                    record.status,
                    KnowledgeStatus::Verified | KnowledgeStatus::Promoted
                ) && record
                    .consolidation
                    .as_ref()
                    .is_some_and(|challenger| challenger.product() == product)
            }) {
                continue;
            }
            for operator in revision.operators() {
                bytes = bytes
                    .checked_add(
                        operator
                            .steps()
                            .len()
                            .checked_mul(std::mem::size_of::<Box<[u8]>>())
                            .ok_or(IntelligenceError::ResourceOverflow)?,
                    )
                    .and_then(|total| {
                        operator
                            .steps()
                            .iter()
                            .try_fold(total, |total, step| total.checked_add(step.len()))
                    })
                    .and_then(|total| {
                        total.checked_add(
                            operator
                                .support()
                                .len()
                                .checked_mul(std::mem::size_of::<[u8; 32]>())?,
                        )
                    })
                    .ok_or(IntelligenceError::ResourceOverflow)?;
            }
        }
        Ok(bytes)
    }

    fn recovery_obligation_count(
        &self,
        limits: IntelligenceLimits,
    ) -> Result<usize, IntelligenceError> {
        let maximum = limits
            .maximum_opportunities
            .checked_mul(16)
            .and_then(|records| records.checked_mul(MAXIMUM_RECIPE_MEMBERS))
            .ok_or(IntelligenceError::ResourceOverflow)?;
        let mut count = self.records.iter().try_fold(0_usize, |count, record| {
            if matches!(
                record.status,
                KnowledgeStatus::Verified | KnowledgeStatus::Promoted
            ) {
                record
                    .consolidation
                    .as_deref()
                    .map_or(Ok(count), |consolidation| {
                        count
                            .checked_add(consolidation.obligations().len())
                            .ok_or(IntelligenceError::ResourceOverflow)
                    })
            } else {
                Ok(count)
            }
        })?;
        for (product, revision) in self.active.retained_revisions() {
            if self.records.iter().any(|record| {
                matches!(
                    record.status,
                    KnowledgeStatus::Verified | KnowledgeStatus::Promoted
                ) && record
                    .consolidation
                    .as_ref()
                    .is_some_and(|challenger| challenger.product() == product)
            }) {
                continue;
            }
            count = count
                .checked_add(revision.operators().len())
                .ok_or(IntelligenceError::ResourceOverflow)?;
        }
        if count > maximum {
            return Err(IntelligenceError::CapacityExceeded);
        }
        Ok(count)
    }

    #[cfg(feature = "internal-experiments")]
    pub(super) fn poison_predecessor_artifact_for_test(&mut self) -> bool {
        self.active.poison_predecessor_artifact_for_test()
    }

    #[cfg(feature = "internal-experiments")]
    pub(super) fn statistics(&self) -> KnowledgeStatistics {
        let mut statistics = KnowledgeStatistics::default();
        for record in &self.records {
            match record.status {
                KnowledgeStatus::Provisional | KnowledgeStatus::Refuted => {
                    statistics.provisional = statistics.provisional.saturating_add(1);
                }
                KnowledgeStatus::Verified => {
                    statistics.verified = statistics.verified.saturating_add(1);
                }
                KnowledgeStatus::Promoted => {
                    statistics.promoted = statistics.promoted.saturating_add(1);
                }
                KnowledgeStatus::Invalidated(_) => {
                    statistics.invalidated = statistics.invalidated.saturating_add(1);
                }
            }
        }
        statistics
    }

    pub(super) fn import_legacy_active(
        &mut self,
        legacy: &KnowledgeState,
        limits: IntelligenceLimits,
    ) -> Result<(), IntelligenceError> {
        let last_promoted = self.last_promoted_consolidation()?;
        if last_promoted
            .as_ref()
            .is_some_and(|challenger| challenger.product() != legacy.product())
        {
            return Err(IntelligenceError::InvalidKnowledge);
        }
        let encoded = legacy.encode();
        if encoded.len() > limits.maximum_model_bytes {
            return Err(IntelligenceError::CapacityExceeded);
        }
        let active =
            KnowledgeState::decode(&encoded).map_err(|()| IntelligenceError::InvalidKnowledge)?;
        self.active = active;
        if !self.fits_limits(limits) {
            return Err(IntelligenceError::CapacityExceeded);
        }
        Ok(())
    }

    fn last_promoted_consolidation(
        &self,
    ) -> Result<Option<ConsolidationChallenger>, IntelligenceError> {
        self.records
            .iter()
            .filter(|record| {
                record.status == KnowledgeStatus::Promoted
                    && record.challenger.recipe == CompilerRecipe::DeriveOperator
            })
            .map(|record| decode_consolidation_product(&record.challenger))
            .next_back()
            .transpose()
    }

    fn active_aligns_with_promotions(&self) -> bool {
        let promoted = self
            .records
            .iter()
            .filter(|record| record.status == KnowledgeStatus::Promoted)
            .count();
        let mut product = self.active.product();
        let mut aligned = 0_usize;
        while let Some(consolidation) = self
            .records
            .iter()
            .filter(|record| record.status == KnowledgeStatus::Promoted)
            .filter_map(|record| record.consolidation.as_ref())
            .find(|consolidation| consolidation.product() == product)
        {
            aligned = aligned.saturating_add(1);
            product = consolidation.base_product();
            if aligned > promoted {
                return false;
            }
        }
        aligned == promoted
    }

    pub(super) fn eligible_shadow(
        &self,
    ) -> Option<(KnowledgeChallengerId, Arc<ConsolidationChallenger>)> {
        for record in &self.records {
            if record.status != KnowledgeStatus::Verified {
                continue;
            }
            let Some(challenger) = record.consolidation.as_ref() else {
                continue;
            };
            if challenger.base_product() == self.active.product() {
                return Some((record.challenger.id, Arc::clone(challenger)));
            }
        }
        None
    }

    pub(super) fn propose_consolidation(
        &self,
        observations: &[DerivationObservation],
        roots: impl IntoIterator<Item = [u8; 32]>,
        pareto: impl IntoIterator<Item = [u8; 32]>,
    ) -> Option<ConsolidationChallenger> {
        self.active
            .propose_consolidation(observations, roots, pareto)
    }

    pub(super) fn contains(&self, id: KnowledgeChallengerId) -> bool {
        self.records.iter().any(|record| record.challenger.id == id)
    }

    pub(super) fn pending_compilation(&self) -> Option<&KnowledgeChallenger> {
        self.records
            .iter()
            .rev()
            .find(|record| {
                record.status == KnowledgeStatus::Provisional && record.consolidation.is_some()
            })
            .map(|record| &record.challenger)
    }

    pub(super) fn opened_verification(
        &self,
        opened_revision: [u8; 32],
    ) -> Option<OpenedKnowledgeWork> {
        self.pending_verification
            .as_ref()
            .map(|pending| OpenedKnowledgeWork {
                opened_revision,
                work: pending.work.clone(),
                receipts: pending.receipts.clone(),
                reserved: pending.reserved,
            })
    }

    pub(super) fn matches_opened_verification(&self, opened: &OpenedKnowledgeWork) -> bool {
        self.pending_verification.as_ref().is_some_and(|pending| {
            pending.work == opened.work
                && pending.receipts == opened.receipts
                && pending.reserved == opened.reserved
        })
    }

    pub(super) fn ensure_proposal_capacity(
        &self,
        challenger: &KnowledgeChallenger,
        limits: IntelligenceLimits,
    ) -> Result<(), IntelligenceError> {
        let record_limit = limits.maximum_opportunities.saturating_mul(16);
        if self.records.len() >= record_limit {
            return Err(IntelligenceError::CapacityExceeded);
        }
        let retained_bytes = self
            .active
            .encode()
            .len()
            .checked_add(self.product_bytes())
            .and_then(|bytes| bytes.checked_add(challenger.product.heap_bytes()))
            .ok_or(IntelligenceError::ResourceOverflow)?;
        if retained_bytes > limits.maximum_model_bytes {
            return Err(IntelligenceError::CapacityExceeded);
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) const fn retained_item_count(&self) -> usize {
        self.records.len()
    }

    pub(super) fn encode_updates(updates: &[KnowledgeUpdate], output: &mut Vec<u8>) {
        output.extend_from_slice(&(updates.len() as u64).to_le_bytes());
        for update in updates {
            match update {
                KnowledgeUpdate::Propose(challenger) => {
                    output.push(1);
                    challenger.encode_canonical(output);
                }
                KnowledgeUpdate::Review(review) => {
                    output.push(2);
                    output.extend_from_slice(&review.challenger.0);
                    output.extend_from_slice(&review.obligation.identity());
                    output.extend_from_slice(&review.decision.identity());
                }
                KnowledgeUpdate::Invalidate { challenger, reason } => {
                    output.push(3);
                    output.extend_from_slice(&challenger.0);
                    output.push(*reason as u8);
                }
                KnowledgeUpdate::EvaluatePromotion(challenger) => {
                    output.push(4);
                    output.extend_from_slice(&challenger.0);
                }
                KnowledgeUpdate::OpenVerification {
                    work,
                    receipts,
                    reserved,
                } => {
                    output.push(5);
                    output.extend_from_slice(&work.base_revision);
                    work.challenger.encode_canonical(output);
                    output.extend_from_slice(&(receipts.len() as u64).to_le_bytes());
                    for receipt in receipts {
                        receipt.encode_canonical(output);
                    }
                    encode_resource_vector(*reserved, output);
                }
                KnowledgeUpdate::CompleteVerification(challenger) => {
                    output.push(6);
                    output.extend_from_slice(&challenger.0);
                }
                KnowledgeUpdate::InterruptVerification(challenger) => {
                    output.push(7);
                    output.extend_from_slice(&challenger.0);
                }
            }
        }
    }

    pub(super) fn decode_updates(
        input: &mut Decoder<'_>,
        limits: IntelligenceLimits,
    ) -> Result<Vec<KnowledgeUpdate>, ()> {
        let record_limit = limits.maximum_opportunities.saturating_mul(16);
        let review_limit = limits.maximum_investments.saturating_mul(64);
        let maximum_updates = record_limit.saturating_mul(3).saturating_add(review_limit);
        let count = read_count(input, maximum_updates, 1)?;
        let mut remaining_product_bytes = limits.maximum_model_bytes;
        let mut updates = Vec::with_capacity(count);
        for _ in 0..count {
            updates.push(match input.read_u8()? {
                1 => {
                    let challenger =
                        KnowledgeChallenger::decode_canonical(input, remaining_product_bytes)?;
                    remaining_product_bytes = remaining_product_bytes
                        .checked_sub(challenger.product.heap_bytes())
                        .ok_or(())?;
                    KnowledgeUpdate::Propose(challenger)
                }
                2 => KnowledgeUpdate::Review(KnowledgeReview::new(
                    KnowledgeChallengerId(input.read_digest()?),
                    SubjectId::new(input.read_digest()?),
                    DecisionId::from_identity(input.read_digest()?),
                )),
                3 => KnowledgeUpdate::Invalidate {
                    challenger: KnowledgeChallengerId(input.read_digest()?),
                    reason: KnowledgeInvalidationReason::decode(input.read_u8()?)?,
                },
                4 => {
                    KnowledgeUpdate::EvaluatePromotion(KnowledgeChallengerId(input.read_digest()?))
                }
                5 => {
                    let base_revision = input.read_digest()?;
                    let challenger =
                        KnowledgeChallenger::decode_canonical(input, remaining_product_bytes)?;
                    remaining_product_bytes = remaining_product_bytes
                        .checked_sub(challenger.product.heap_bytes())
                        .ok_or(())?;
                    let work = KnowledgeWork::from_challenger(base_revision, &challenger)
                        .map_err(|_| ())?;
                    let receipt_count = read_count(
                        input,
                        limits.maximum_investments,
                        MINIMUM_INVESTMENT_RECEIPT_BYTES,
                    )?;
                    let mut receipts = Vec::with_capacity(receipt_count);
                    for _ in 0..receipt_count {
                        receipts.push(InvestmentReceipt::decode_canonical(input)?);
                    }
                    KnowledgeUpdate::OpenVerification {
                        work,
                        receipts: receipts.into_boxed_slice(),
                        reserved: decode_resource_vector(input)?,
                    }
                }
                6 => KnowledgeUpdate::CompleteVerification(KnowledgeChallengerId(
                    input.read_digest()?,
                )),
                7 => KnowledgeUpdate::InterruptVerification(KnowledgeChallengerId(
                    input.read_digest()?,
                )),
                _ => return Err(()),
            });
        }
        Ok(updates)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the Knowledge compiler state machine keeps every canonical update and invariant in one auditable transition"
    )]
    pub(super) fn apply(
        &mut self,
        updates: &[KnowledgeUpdate],
        experience: &impl CausalEvidence,
        limits: IntelligenceLimits,
    ) -> Result<bool, IntelligenceError> {
        let mut changed = false;
        for update in updates {
            match update {
                KnowledgeUpdate::Propose(challenger) => {
                    if self.contains(challenger.id) {
                        return Err(IntelligenceError::DuplicateKnowledge);
                    }
                    self.ensure_proposal_capacity(challenger, limits)?;
                    let mut record = KnowledgeRecord {
                        consolidation: cached_consolidation(challenger)?,
                        challenger: challenger.clone(),
                        reviews: Vec::with_capacity(challenger.obligations.len()),
                        promoted_by: None,
                        status: KnowledgeStatus::Provisional,
                    };
                    record.status = verified_status(&record, experience)?;
                    self.records.push(record);
                    changed = true;
                }
                KnowledgeUpdate::Review(review) => {
                    let record = self
                        .records
                        .iter_mut()
                        .find(|record| record.challenger.id == review.challenger)
                        .ok_or(IntelligenceError::InvalidReference)?;
                    if matches!(record.status, KnowledgeStatus::Invalidated(_)) {
                        return Err(IntelligenceError::InvalidKnowledge);
                    }
                    if !record.challenger.obligations.contains(&review.obligation) {
                        return Err(IntelligenceError::InvalidReference);
                    }
                    if record
                        .reviews
                        .iter()
                        .any(|existing| existing.decision == review.decision)
                    {
                        return Err(IntelligenceError::DuplicateKnowledge);
                    }
                    experience.authoritative_verification(review.decision, review.obligation)?;
                    record.reviews.push(*review);
                    record.status = verified_status(record, experience)?;
                    changed = true;
                }
                KnowledgeUpdate::Invalidate { challenger, reason } => {
                    let record = self
                        .records
                        .iter_mut()
                        .find(|record| record.challenger.id == *challenger)
                        .ok_or(IntelligenceError::InvalidReference)?;
                    if matches!(record.status, KnowledgeStatus::Invalidated(_)) {
                        return Err(IntelligenceError::DuplicateKnowledge);
                    }
                    record.promoted_by = None;
                    record.status = KnowledgeStatus::Invalidated(*reason);
                    changed = true;
                }
                KnowledgeUpdate::EvaluatePromotion(id) => {
                    let record = self
                        .records
                        .iter_mut()
                        .find(|record| record.challenger.id == *id)
                        .ok_or(IntelligenceError::InvalidReference)?;
                    if record.status != KnowledgeStatus::Verified {
                        continue;
                    }
                    if let Some(campaign) = record.challenger.promotion.satisfying_campaign(
                        record.challenger.product.subject,
                        experience.contrasts(),
                    ) {
                        let consolidation = record
                            .consolidation
                            .as_deref()
                            .ok_or(IntelligenceError::InvalidKnowledge)?;
                        activate_product(&mut self.active, &record.challenger, consolidation)?;
                        record.promoted_by = Some(campaign);
                        record.status = KnowledgeStatus::Promoted;
                        changed = true;
                    }
                }
                KnowledgeUpdate::OpenVerification {
                    work,
                    receipts,
                    reserved,
                } => {
                    if self.pending_verification.is_some()
                        || !valid_open_verification(work, receipts, *reserved)
                    {
                        return Err(IntelligenceError::InvalidKnowledge);
                    }
                    if let Some(record) = self
                        .records
                        .iter()
                        .find(|record| record.challenger.id == work.id())
                    {
                        if record.status != KnowledgeStatus::Provisional
                            || record.challenger != work.challenger
                            || !record.reviews.is_empty()
                        {
                            return Err(IntelligenceError::InvalidKnowledge);
                        }
                    } else {
                        self.ensure_proposal_capacity(&work.challenger, limits)?;
                    }
                    self.pending_verification = Some(PendingKnowledgeVerification {
                        work: work.clone(),
                        receipts: receipts.clone(),
                        reserved: *reserved,
                    });
                    changed = true;
                }
                KnowledgeUpdate::CompleteVerification(id) => {
                    let pending = self
                        .pending_verification
                        .as_ref()
                        .filter(|pending| pending.work.id() == *id)
                        .ok_or(IntelligenceError::InvalidReference)?;
                    let mut reviews = Vec::with_capacity(pending.receipts.len());
                    for (obligation, receipt) in
                        pending.work.obligations().iter().zip(&pending.receipts)
                    {
                        experience
                            .authoritative_verification(receipt.decision(), obligation.subject())?;
                        reviews.push(KnowledgeReview::new(
                            *id,
                            obligation.subject(),
                            receipt.decision(),
                        ));
                    }
                    let challenger = pending.work.challenger.clone();
                    if let Some(record) = self
                        .records
                        .iter_mut()
                        .find(|record| record.challenger.id == *id)
                    {
                        if record.status != KnowledgeStatus::Provisional
                            || record.challenger != challenger
                            || !record.reviews.is_empty()
                        {
                            return Err(IntelligenceError::InvalidKnowledge);
                        }
                        record.reviews = reviews;
                        record.status = verified_status(record, experience)?;
                    } else {
                        let mut record = KnowledgeRecord {
                            consolidation: cached_consolidation(&challenger)?,
                            challenger,
                            reviews,
                            promoted_by: None,
                            status: KnowledgeStatus::Provisional,
                        };
                        record.status = verified_status(&record, experience)?;
                        self.records.push(record);
                    }
                    self.pending_verification = None;
                    changed = true;
                }
                KnowledgeUpdate::InterruptVerification(id) => {
                    let pending = self
                        .pending_verification
                        .as_ref()
                        .filter(|pending| pending.work.id() == *id)
                        .ok_or(IntelligenceError::InvalidReference)?;
                    let challenger = pending.work.challenger.clone();
                    if let Some(record) = self
                        .records
                        .iter_mut()
                        .find(|record| record.challenger.id == *id)
                    {
                        if record.status != KnowledgeStatus::Provisional
                            || record.challenger != challenger
                        {
                            return Err(IntelligenceError::InvalidKnowledge);
                        }
                        record.status =
                            KnowledgeStatus::Invalidated(KnowledgeInvalidationReason::Witness);
                    } else {
                        self.records.push(KnowledgeRecord {
                            consolidation: cached_consolidation(&challenger)?,
                            challenger,
                            reviews: Vec::new(),
                            promoted_by: None,
                            status: KnowledgeStatus::Invalidated(
                                KnowledgeInvalidationReason::Witness,
                            ),
                        });
                    }
                    self.pending_verification = None;
                    changed = true;
                }
            }
        }
        Ok(changed)
    }

    pub(super) fn reconcile_promotions(
        &mut self,
        experience: &impl CausalEvidence,
    ) -> Result<bool, IntelligenceError> {
        let mut active_product = self.active.product();
        let mut active_chain = Vec::new();
        while let Some((index, consolidation)) = self
            .records
            .iter()
            .enumerate()
            .filter(|(_, record)| record.status == KnowledgeStatus::Promoted)
            .filter_map(|(index, record)| {
                record
                    .consolidation
                    .as_ref()
                    .map(|consolidation| (index, consolidation))
            })
            .find(|(_, consolidation)| consolidation.product() == active_product)
        {
            active_chain.push(index);
            active_product = consolidation.base_product();
        }
        let Some(invalidated_position) = active_chain.iter().rposition(|index| {
            let record = &self.records[*index];
            record.promoted_by.is_none_or(|campaign| {
                !record.challenger.promotion.campaign_satisfies(
                    campaign,
                    record.challenger.product.subject,
                    experience.contrasts(),
                )
            })
        }) else {
            return Ok(false);
        };
        for index in active_chain.into_iter().take(invalidated_position + 1) {
            let record = &mut self.records[index];
            let consolidation = record
                .consolidation
                .as_ref()
                .ok_or(IntelligenceError::InvalidKnowledge)?;
            self.active
                .rollback_promoted_product(consolidation.product())
                .map_err(|error| match error {
                    ConsolidationRollbackError::StaleProduct => IntelligenceError::StaleTransition,
                    ConsolidationRollbackError::MissingPredecessor
                    | ConsolidationRollbackError::GenerationOverflow => {
                        IntelligenceError::InvalidKnowledge
                    }
                })?;
            record.promoted_by = None;
            record.status = verified_status(record, experience)?;
        }
        Ok(true)
    }

    pub(super) fn promotion_rebase_bytes(
        &self,
        challenger: KnowledgeChallengerId,
    ) -> Result<usize, IntelligenceError> {
        let record = self
            .records
            .iter()
            .find(|record| record.challenger.id == challenger)
            .ok_or(IntelligenceError::InvalidReference)?;
        let consolidation = record
            .consolidation
            .as_deref()
            .ok_or(IntelligenceError::InvalidKnowledge)?;
        consolidation
            .obligations()
            .len()
            .checked_mul(std::mem::size_of::<crate::knowledge::ConsolidationObligation>())
            .ok_or(IntelligenceError::ResourceOverflow)
    }

    pub(super) fn heap_bytes(&self) -> usize {
        let pending_bytes = self.pending_verification.as_ref().map_or(0, |pending| {
            pending.work.challenger.heap_bytes().saturating_add(
                pending
                    .receipts
                    .len()
                    .saturating_mul(std::mem::size_of::<InvestmentReceipt>()),
            )
        });
        usize::try_from(self.active.resident_bytes())
            .unwrap_or(usize::MAX)
            .saturating_add(
                self.records
                    .capacity()
                    .saturating_sub(self.records.len())
                    .saturating_mul(std::mem::size_of::<KnowledgeRecord>())
                    .saturating_add(self.records.iter().fold(0_usize, |bytes, record| {
                        bytes
                            .saturating_add(std::mem::size_of::<KnowledgeRecord>())
                            .saturating_add(record.challenger.heap_bytes())
                            .saturating_add(record.consolidation.as_ref().map_or(0, |value| {
                                usize::try_from(value.resident_bytes()).unwrap_or(usize::MAX)
                            }))
                            .saturating_add(
                                record
                                    .reviews
                                    .capacity()
                                    .saturating_mul(std::mem::size_of::<KnowledgeReview>()),
                            )
                    })),
            )
            .saturating_add(pending_bytes)
    }

    pub(super) fn fits_limits(&self, limits: IntelligenceLimits) -> bool {
        let Some(record_limit) = limits.maximum_opportunities.checked_mul(16) else {
            return false;
        };
        let Some(review_limit) = limits.maximum_investments.checked_mul(64) else {
            return false;
        };
        let pending_product_bytes = self
            .pending_verification
            .as_ref()
            .map_or(0, |pending| pending.work.challenger.product.heap_bytes());
        self.records.len() <= record_limit
            && self
                .product_bytes()
                .checked_add(self.active.encode().len())
                .and_then(|bytes| bytes.checked_add(pending_product_bytes))
                .is_some_and(|bytes| bytes <= limits.maximum_model_bytes)
            && self
                .records
                .iter()
                .all(|record| record.reviews.len() <= review_limit)
            && self.pending_verification.as_ref().is_none_or(|pending| {
                pending.receipts.len() <= limits.maximum_investments
                    && valid_open_verification(&pending.work, &pending.receipts, pending.reserved)
            })
    }

    pub(super) fn encode_canonical(&self, output: &mut Vec<u8>) {
        let active = self.active.encode();
        output.extend_from_slice(&(active.len() as u64).to_le_bytes());
        output.extend_from_slice(&active);
        self.encode_records(output);
        match &self.pending_verification {
            Some(pending) => {
                output.push(1);
                output.extend_from_slice(&pending.work.base_revision);
                pending.work.challenger.encode_canonical(output);
                output.extend_from_slice(&(pending.receipts.len() as u64).to_le_bytes());
                for receipt in &pending.receipts {
                    receipt.encode_canonical(output);
                }
                encode_resource_vector(pending.reserved, output);
            }
            None => output.push(0),
        }
    }

    pub(super) fn encode_legacy_v14(&self, output: &mut Vec<u8>) {
        let active = self.active.encode();
        output.extend_from_slice(&(active.len() as u64).to_le_bytes());
        output.extend_from_slice(&active);
        self.encode_records(output);
    }

    pub(super) fn encode_legacy_v13(&self, output: &mut Vec<u8>) {
        self.encode_records(output);
    }

    fn encode_records(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&(self.records.len() as u64).to_le_bytes());
        for record in &self.records {
            record.challenger.encode_canonical(output);
            output.extend_from_slice(&(record.reviews.len() as u64).to_le_bytes());
            for review in &record.reviews {
                output.extend_from_slice(&review.obligation.identity());
                output.extend_from_slice(&review.decision.identity());
            }
            match record.status {
                KnowledgeStatus::Invalidated(reason) => {
                    output.push(1);
                    output.push(reason as u8);
                }
                KnowledgeStatus::Provisional
                | KnowledgeStatus::Refuted
                | KnowledgeStatus::Verified
                | KnowledgeStatus::Promoted => output.push(0),
            }
            match record.promoted_by {
                Some(campaign) => {
                    output.push(1);
                    output.extend_from_slice(&campaign.identity());
                }
                None => output.push(0),
            }
        }
    }

    pub(super) fn canonical_root(&self) -> [u8; 32] {
        let mut encoded = Vec::new();
        self.encode_canonical(&mut encoded);
        let mut digest = Sha256::new();
        digest.update(b"reflex-knowledge-compiler-root-v1\0");
        digest.update(encoded);
        digest.finalize().into()
    }

    pub(super) fn legacy_v13_canonical_root(&self) -> [u8; 32] {
        let mut encoded = Vec::new();
        self.encode_legacy_v13(&mut encoded);
        let mut digest = Sha256::new();
        digest.update(b"reflex-knowledge-compiler-root-v1\0");
        digest.update(encoded);
        digest.finalize().into()
    }

    pub(super) fn legacy_v14_canonical_root(&self) -> [u8; 32] {
        let mut encoded = Vec::new();
        self.encode_legacy_v14(&mut encoded);
        let mut digest = Sha256::new();
        digest.update(b"reflex-knowledge-compiler-root-v1\0");
        digest.update(encoded);
        digest.finalize().into()
    }

    pub(super) fn encode_legacy_v7(&self, output: &mut Vec<u8>) -> Result<(), ()> {
        output.extend_from_slice(&(self.records.len() as u64).to_le_bytes());
        for record in &self.records {
            if matches!(record.status, KnowledgeStatus::Invalidated(_)) {
                return Err(());
            }
            record.challenger.encode_legacy_v7(output)?;
            output.extend_from_slice(&(record.reviews.len() as u64).to_le_bytes());
            for review in &record.reviews {
                output.extend_from_slice(&review.obligation.identity());
                output.extend_from_slice(&review.decision.identity());
            }
            match record.promoted_by {
                Some(campaign) => {
                    output.push(1);
                    output.extend_from_slice(&campaign.identity());
                }
                None => output.push(0),
            }
        }
        Ok(())
    }

    pub(super) fn encode_legacy_v6(&self, output: &mut Vec<u8>) -> Result<(), ()> {
        output.extend_from_slice(&(self.records.len() as u64).to_le_bytes());
        for record in &self.records {
            if matches!(record.status, KnowledgeStatus::Invalidated(_)) {
                return Err(());
            }
            record.challenger.encode_legacy_v6(output)?;
            output.extend_from_slice(&(record.reviews.len() as u64).to_le_bytes());
            for review in &record.reviews {
                output.extend_from_slice(&review.obligation.identity());
                output.extend_from_slice(&review.decision.identity());
            }
            match record.promoted_by {
                Some(campaign) => {
                    output.push(1);
                    output.extend_from_slice(&campaign.identity());
                }
                None => output.push(0),
            }
        }
        Ok(())
    }

    pub(super) fn decode_canonical(
        input: &mut Decoder<'_>,
        experience: &CausalLedger,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        let active_bytes = read_sized(input, limits.maximum_model_bytes)?;
        let active = KnowledgeState::decode(active_bytes)?;
        let mut compiler = Self::decode(input, experience, limits, KnowledgeWireFormat::V8)?;
        compiler.active = active;
        compiler.pending_verification = match input.read_u8()? {
            0 => None,
            1 => {
                let base_revision = input.read_digest()?;
                let remaining_product_bytes = limits
                    .maximum_model_bytes
                    .checked_sub(compiler.active.encode().len())
                    .and_then(|bytes| bytes.checked_sub(compiler.product_bytes()))
                    .ok_or(())?;
                let challenger =
                    KnowledgeChallenger::decode_canonical(input, remaining_product_bytes)?;
                let work =
                    KnowledgeWork::from_challenger(base_revision, &challenger).map_err(|_| ())?;
                if compiler.contains(work.id()) {
                    return Err(());
                }
                let count = read_count(
                    input,
                    limits.maximum_investments,
                    MINIMUM_INVESTMENT_RECEIPT_BYTES,
                )?;
                let mut receipts = Vec::with_capacity(count);
                for _ in 0..count {
                    receipts.push(InvestmentReceipt::decode_canonical(input)?);
                }
                let reserved = decode_resource_vector(input)?;
                if !valid_open_verification(&work, &receipts, reserved) {
                    return Err(());
                }
                Some(PendingKnowledgeVerification {
                    work,
                    receipts: receipts.into_boxed_slice(),
                    reserved,
                })
            }
            _ => return Err(()),
        };
        if !compiler.fits_limits(limits) || !compiler.active_aligns_with_promotions() {
            return Err(());
        }
        Ok(compiler)
    }

    pub(super) fn decode_legacy_v14(
        input: &mut Decoder<'_>,
        experience: &CausalLedger,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        let active_bytes = read_sized(input, limits.maximum_model_bytes)?;
        let active = KnowledgeState::decode(active_bytes)?;
        let mut compiler = Self::decode(input, experience, limits, KnowledgeWireFormat::V8)?;
        compiler.active = active;
        if !compiler.fits_limits(limits) || !compiler.active_aligns_with_promotions() {
            return Err(());
        }
        Ok(compiler)
    }

    pub(super) fn decode_legacy_v13(
        input: &mut Decoder<'_>,
        experience: &CausalLedger,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        Self::decode(input, experience, limits, KnowledgeWireFormat::V8)
    }

    pub(super) fn decode_legacy_v7(
        input: &mut Decoder<'_>,
        experience: &CausalLedger,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        Self::decode(input, experience, limits, KnowledgeWireFormat::LegacyV7)
    }

    pub(super) fn decode_legacy_v6(
        input: &mut Decoder<'_>,
        experience: &CausalLedger,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        Self::decode(input, experience, limits, KnowledgeWireFormat::LegacyV6)
    }

    fn decode(
        input: &mut Decoder<'_>,
        experience: &CausalLedger,
        limits: IntelligenceLimits,
        format: KnowledgeWireFormat,
    ) -> Result<Self, ()> {
        let record_limit = limits.maximum_opportunities.saturating_mul(16);
        let record_count = read_count(input, record_limit, MINIMUM_KNOWLEDGE_RECORD_BYTES)?;
        let mut compiler = Self {
            active: KnowledgeState::default(),
            records: Vec::with_capacity(record_count),
            pending_verification: None,
        };
        let mut remaining_product_bytes = limits.maximum_model_bytes;
        for _ in 0..record_count {
            let challenger = match format {
                KnowledgeWireFormat::LegacyV6 => KnowledgeChallenger::decode_legacy_v6(input)?,
                KnowledgeWireFormat::LegacyV7 => {
                    KnowledgeChallenger::decode_legacy_v7(input, remaining_product_bytes)?
                }
                KnowledgeWireFormat::V8 => {
                    KnowledgeChallenger::decode_canonical(input, remaining_product_bytes)?
                }
            };
            remaining_product_bytes = remaining_product_bytes
                .checked_sub(challenger.product.heap_bytes())
                .ok_or(())?;
            if compiler
                .records
                .iter()
                .any(|record| record.challenger.id == challenger.id)
            {
                return Err(());
            }
            let review_count =
                read_count(input, limits.maximum_investments.saturating_mul(64), 64)?;
            let mut reviews = Vec::with_capacity(review_count);
            for _ in 0..review_count {
                let review = KnowledgeReview::new(
                    challenger.id,
                    SubjectId::new(input.read_digest()?),
                    DecisionId::from_identity(input.read_digest()?),
                );
                if !challenger.obligations.contains(&review.obligation)
                    || reviews
                        .iter()
                        .any(|existing: &KnowledgeReview| existing.decision == review.decision)
                    || experience
                        .authoritative_verification(review.decision, review.obligation)
                        .is_err()
                {
                    return Err(());
                }
                reviews.push(review);
            }
            let invalidation = match format {
                KnowledgeWireFormat::V8 => match input.read_u8()? {
                    0 => None,
                    1 => Some(KnowledgeInvalidationReason::decode(input.read_u8()?)?),
                    _ => return Err(()),
                },
                KnowledgeWireFormat::LegacyV6 | KnowledgeWireFormat::LegacyV7 => None,
            };
            let promoted_by = match input.read_u8()? {
                0 => None,
                1 => Some(ShadowCampaignId::from_identity(input.read_digest()?)),
                _ => return Err(()),
            };
            let mut record = KnowledgeRecord {
                consolidation: cached_consolidation(&challenger).map_err(|_| ())?,
                challenger,
                reviews,
                promoted_by,
                status: KnowledgeStatus::Provisional,
            };
            if let Some(reason) = invalidation {
                if record.promoted_by.is_some() {
                    return Err(());
                }
                record.status = KnowledgeStatus::Invalidated(reason);
            } else {
                record.status = verified_status(&record, experience).map_err(|_| ())?;
                if let Some(campaign) = record.promoted_by {
                    if record.status != KnowledgeStatus::Verified
                        || !record.challenger.promotion.campaign_satisfies(
                            campaign,
                            record.challenger.product.subject,
                            experience.contrasts(),
                        )
                    {
                        return Err(());
                    }
                    record.status = KnowledgeStatus::Promoted;
                }
            }
            compiler.records.push(record);
        }
        Ok(compiler)
    }

    fn product_bytes(&self) -> usize {
        self.records.iter().fold(0_usize, |bytes, record| {
            bytes.saturating_add(record.challenger.product.heap_bytes())
        })
    }
}

fn verified_status(
    record: &KnowledgeRecord,
    experience: &impl CausalEvidence,
) -> Result<KnowledgeStatus, IntelligenceError> {
    let mut accepted = BTreeSet::new();
    for review in &record.reviews {
        match experience.authoritative_verification(review.decision, review.obligation)? {
            InvestmentOutcome::VerifiedAccepted { .. } => {
                accepted.insert(review.obligation);
            }
            InvestmentOutcome::VerifiedRefuted => return Ok(KnowledgeStatus::Refuted),
            InvestmentOutcome::VerifiedUnknown => {}
            InvestmentOutcome::Completed | InvestmentOutcome::Failed => {
                return Err(IntelligenceError::InvalidKnowledge);
            }
        }
    }
    if record
        .challenger
        .obligations
        .iter()
        .all(|obligation| accepted.contains(obligation))
    {
        Ok(KnowledgeStatus::Verified)
    } else {
        Ok(KnowledgeStatus::Provisional)
    }
}

fn activate_product(
    active: &mut KnowledgeState,
    challenger: &KnowledgeChallenger,
    consolidation: &ConsolidationChallenger,
) -> Result<(), IntelligenceError> {
    let product = consolidation.product();
    if challenger.product.subject.identity() != product.identity() {
        return Err(IntelligenceError::InvalidKnowledge);
    }
    active
        .activate_reverified_challenger(consolidation, product)
        .map_err(|error| match error {
            #[cfg(test)]
            ConsolidationActivationError::StaleBase => IntelligenceError::StaleTransition,
            #[cfg(test)]
            ConsolidationActivationError::WrongProduct => IntelligenceError::InvalidKnowledge,
            ConsolidationActivationError::InvalidChallenger
            | ConsolidationActivationError::Unchanged
            | ConsolidationActivationError::GenerationOverflow => {
                IntelligenceError::InvalidKnowledge
            }
        })?;
    Ok(())
}

fn decode_consolidation_product(
    challenger: &KnowledgeChallenger,
) -> Result<ConsolidationChallenger, IntelligenceError> {
    if challenger.product.kind != KnowledgeProductKind::DerivedOperator
        || challenger.product.meaning != KnowledgeProductMeaning::Semantic
    {
        return Err(IntelligenceError::InvalidKnowledge);
    }
    let bytes = challenger
        .product
        .bytes()
        .ok_or(IntelligenceError::InvalidKnowledge)?;
    let consolidation =
        ConsolidationChallenger::decode(bytes).map_err(|()| IntelligenceError::InvalidKnowledge)?;
    if challenger.product.subject.identity() != consolidation.product().identity() {
        return Err(IntelligenceError::InvalidKnowledge);
    }
    Ok(consolidation)
}

fn cached_consolidation(
    challenger: &KnowledgeChallenger,
) -> Result<Option<Arc<ConsolidationChallenger>>, IntelligenceError> {
    if challenger.product.kind != KnowledgeProductKind::DerivedOperator {
        return Ok(None);
    }
    let Some(bytes) = challenger.product.bytes() else {
        return Ok(None);
    };
    if !bytes.starts_with(b"RFKC\x01") {
        return Ok(None);
    }
    decode_consolidation_product(challenger)
        .map(Arc::new)
        .map(Some)
}

fn valid_open_verification(
    work: &KnowledgeWork,
    receipts: &[InvestmentReceipt],
    reserved: ResourceVector,
) -> bool {
    if receipts.len() != work.obligations().len() || receipts.is_empty() {
        return false;
    }
    let mut aggregate = ResourceVector::default();
    for (index, (obligation, receipt)) in work.obligations().iter().zip(receipts).enumerate() {
        let resources = receipt.resources();
        if receipt.issued_under() != work.base_revision
            || receipt.opportunity_kind() != OpportunityKind::Candidate
            || receipt.tag() != super::core::InvestmentTag::Verify
            || receipt.opportunity_identity() != obligation.subject().identity()
            || resources.verification_requests != 1
            || index > 0 && (resources.resident_bytes != 0 || resources.elapsed_time_ns != 0)
            || receipts[..index]
                .iter()
                .any(|prior| prior.decision() == receipt.decision())
        {
            return false;
        }
        let Some(next) = aggregate.checked_add(resources) else {
            return false;
        };
        aggregate = next;
    }
    aggregate == reserved
}

fn valid_members(members: &[SubjectId]) -> bool {
    !members.is_empty()
        && members.len() <= MAXIMUM_RECIPE_MEMBERS
        && members.iter().copied().collect::<BTreeSet<_>>().len() == members.len()
}

fn encode_subjects(subjects: &[SubjectId], output: &mut Vec<u8>) {
    output.extend_from_slice(&(subjects.len() as u64).to_le_bytes());
    for subject in subjects {
        output.extend_from_slice(&subject.identity());
    }
}

fn encode_resource_vector(resources: ResourceVector, output: &mut Vec<u8>) {
    for value in [
        resources.cpu_time_ns,
        resources.resident_bytes,
        resources.durable_bytes,
        resources.elapsed_time_ns,
        resources.verification_requests,
    ] {
        output.extend_from_slice(&value.to_le_bytes());
    }
}

fn decode_resource_vector(input: &mut Decoder<'_>) -> Result<ResourceVector, ()> {
    Ok(ResourceVector::new(
        input.read_u64()?,
        input.read_u64()?,
        input.read_u64()?,
        input.read_u64()?,
        input.read_u64()?,
    ))
}

fn decode_subjects(input: &mut Decoder<'_>) -> Result<Vec<SubjectId>, ()> {
    decode_members(input, false)
}

fn decode_obligations(input: &mut Decoder<'_>, allow_empty: bool) -> Result<Vec<SubjectId>, ()> {
    decode_members(input, allow_empty)
}

fn decode_members(input: &mut Decoder<'_>, allow_empty: bool) -> Result<Vec<SubjectId>, ()> {
    let count = read_count(input, MAXIMUM_RECIPE_MEMBERS, 32)?;
    let mut subjects = Vec::with_capacity(count);
    for _ in 0..count {
        subjects.push(SubjectId::new(input.read_digest()?));
    }
    if !(valid_members(&subjects) || allow_empty && subjects.is_empty()) {
        return Err(());
    }
    Ok(subjects)
}

fn read_count(input: &mut Decoder<'_>, limit: usize, minimum_bytes: usize) -> Result<usize, ()> {
    let count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
    if count > limit || count > input.remaining().checked_div(minimum_bytes).unwrap_or(0) {
        return Err(());
    }
    Ok(count)
}

fn read_sized<'a>(input: &mut Decoder<'a>, limit: usize) -> Result<&'a [u8], ()> {
    let count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
    if count == 0 || count > limit || count > input.remaining() {
        return Err(());
    }
    input.take(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cloning_a_knowledge_product_shares_its_opaque_payload() {
        let product = KnowledgeProduct::derived_operator(SubjectId::new([7; 32]))
            .with_bytes(vec![0x5a; 1024 * 1024])
            .unwrap();
        let cloned = product.clone();

        assert!(Arc::ptr_eq(
            product.bytes.as_ref().unwrap(),
            cloned.bytes.as_ref().unwrap()
        ));
    }
}
