use std::collections::{BTreeSet, HashSet};

use sha2::{Digest, Sha256};

use super::arena::{MAXIMUM_FEATURES_PER_OPPORTUNITY, OpportunityKind};
use super::codec::Decoder;
use super::core::InvestmentTag;
use super::forecast::{Forecast, ForecastAxis, MAXIMUM_TYPED_FORECASTS};
use super::types::{
    AllocationSource, FeatureSchemaId, IntelligenceError, IntelligenceLimits, ResourceVector,
    RoutingFamilyId, SpecialistRevisionId, SubjectId,
};
use crate::policy::OperationalEvidence;

const MAXIMUM_SHADOW_AXES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct DecisionId([u8; 32]);

impl DecisionId {
    pub(crate) const fn from_identity(identity: [u8; 32]) -> Self {
        Self(identity)
    }

    pub(crate) const fn identity(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct CheckpointDigest([u8; 32]);

impl CheckpointDigest {
    pub(crate) const fn new(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub(crate) const fn identity(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ShadowCampaignId([u8; 32]);

impl ShadowCampaignId {
    pub(crate) const fn from_identity(identity: [u8; 32]) -> Self {
        Self(identity)
    }

    pub(crate) const fn identity(self) -> [u8; 32] {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct InvestmentReceipt {
    decision: DecisionId,
    epoch: u64,
    investment: [u8; 32],
    opportunity: [u8; 32],
    corpus_key: [u8; 32],
    opportunity_kind: OpportunityKind,
    feature_schema: FeatureSchemaId,
    routing_family: RoutingFamilyId,
    feature_count: u8,
    features: [f32; MAXIMUM_FEATURES_PER_OPPORTUNITY],
    tag: InvestmentTag,
    source: AllocationSource,
    active_revision: [u8; 32],
    policy_rank: u32,
    preference_priority: u32,
    bootstrap_priority: u32,
    forecast_count: u8,
    forecasts: [Forecast; MAXIMUM_TYPED_FORECASTS],
    resources: ResourceVector,
}

impl InvestmentReceipt {
    #[expect(
        clippy::too_many_arguments,
        reason = "the immutable receipt names each causally relevant allocation fact"
    )]
    pub(super) fn new(
        decision: DecisionId,
        epoch: u64,
        investment: [u8; 32],
        opportunity: [u8; 32],
        corpus_key: [u8; 32],
        opportunity_kind: OpportunityKind,
        feature_schema: FeatureSchemaId,
        routing_family: RoutingFamilyId,
        features: &[f32],
        tag: InvestmentTag,
        source: AllocationSource,
        active_revision: [u8; 32],
        policy_rank: u32,
        preference_priority: u32,
        bootstrap_priority: u32,
        forecasts: &[Forecast],
        resources: ResourceVector,
    ) -> Result<Self, IntelligenceError> {
        let feature_count = u8::try_from(features.len())
            .ok()
            .filter(|count| usize::from(*count) <= MAXIMUM_FEATURES_PER_OPPORTUNITY)
            .ok_or(IntelligenceError::InvalidFeature)?;
        if features.iter().any(|feature| !feature.is_finite()) {
            return Err(IntelligenceError::InvalidFeature);
        }
        let forecast_count = u8::try_from(forecasts.len())
            .ok()
            .filter(|count| usize::from(*count) <= MAXIMUM_TYPED_FORECASTS)
            .ok_or(IntelligenceError::InvalidForecast)?;
        if forecasts
            .iter()
            .map(|forecast| forecast.axis())
            .collect::<BTreeSet<_>>()
            .len()
            != forecasts.len()
        {
            return Err(IntelligenceError::InvalidForecast);
        }
        let mut observed_features = [0.0; MAXIMUM_FEATURES_PER_OPPORTUNITY];
        observed_features[..features.len()].copy_from_slice(features);
        let mut observed_forecasts = [Forecast::storage_placeholder(); MAXIMUM_TYPED_FORECASTS];
        for (destination, forecast) in observed_forecasts.iter_mut().zip(forecasts) {
            *destination = *forecast;
        }
        Ok(Self {
            decision,
            epoch,
            investment,
            opportunity,
            corpus_key,
            opportunity_kind,
            feature_schema,
            routing_family,
            feature_count,
            features: observed_features,
            tag,
            source,
            active_revision,
            policy_rank,
            preference_priority,
            bootstrap_priority,
            forecast_count,
            forecasts: observed_forecasts,
            resources,
        })
    }

    pub(crate) const fn decision(self) -> DecisionId {
        self.decision
    }

    pub(super) const fn epoch(self) -> u64 {
        self.epoch
    }

    pub(crate) const fn opportunity_kind(self) -> OpportunityKind {
        self.opportunity_kind
    }

    pub(super) const fn opportunity_identity(self) -> [u8; 32] {
        self.opportunity
    }

    pub(super) const fn corpus_key(self) -> [u8; 32] {
        self.corpus_key
    }

    pub(crate) const fn feature_schema(self) -> FeatureSchemaId {
        self.feature_schema
    }

    pub(crate) const fn routing_family(self) -> RoutingFamilyId {
        self.routing_family
    }

    pub(crate) fn features(&self) -> &[f32] {
        &self.features[..usize::from(self.feature_count)]
    }

    pub(crate) fn forecasts(&self) -> &[Forecast] {
        &self.forecasts[..usize::from(self.forecast_count)]
    }

    pub(crate) const fn resources(self) -> ResourceVector {
        self.resources
    }

    #[cfg(test)]
    pub(crate) const fn preference_priority(self) -> u32 {
        self.preference_priority
    }

    pub(super) const fn tag(self) -> InvestmentTag {
        self.tag
    }

    pub(super) const fn issued_under(self) -> [u8; 32] {
        self.active_revision
    }

    #[cfg(test)]
    pub(crate) fn canonical_identity(self) -> [u8; 32] {
        let mut encoded = Vec::new();
        self.encode_canonical(&mut encoded);
        let mut digest = Sha256::new();
        digest.update(b"reflex-investment-receipt-v1\0");
        digest.update(encoded);
        digest.finalize().into()
    }

    pub(super) fn encode_canonical(self, output: &mut Vec<u8>) {
        self.encode_with_fields(output, true, true);
    }

    pub(super) fn canonical_encoded_len(self) -> usize {
        let source_bytes = match self.source {
            AllocationSource::Bootstrap => 1,
            AllocationSource::Specialist(_) => 33,
        };
        32_usize
            .saturating_add(8)
            .saturating_add(32)
            .saturating_add(32)
            .saturating_add(32)
            .saturating_add(1)
            .saturating_add(32)
            .saturating_add(32)
            .saturating_add(1)
            .saturating_add(self.features().len().saturating_mul(4))
            .saturating_add(1)
            .saturating_add(source_bytes)
            .saturating_add(32)
            .saturating_add(4)
            .saturating_add(4)
            .saturating_add(4)
            .saturating_add(1)
            .saturating_add(self.forecasts().len().saturating_mul(17))
            .saturating_add(5 * 8)
    }

    fn encode_legacy_v11(self, output: &mut Vec<u8>) {
        self.encode_with_fields(output, false, false);
    }

    pub(super) fn encode_legacy_v15(self, output: &mut Vec<u8>) {
        self.encode_with_fields(output, true, false);
    }

    fn encode_with_fields(
        self,
        output: &mut Vec<u8>,
        include_preference: bool,
        include_corpus_key: bool,
    ) {
        output.extend_from_slice(&self.decision.0);
        output.extend_from_slice(&self.epoch.to_le_bytes());
        output.extend_from_slice(&self.investment);
        output.extend_from_slice(&self.opportunity);
        if include_corpus_key {
            output.extend_from_slice(&self.corpus_key);
        }
        output.push(self.opportunity_kind as u8);
        output.extend_from_slice(&self.feature_schema.0);
        output.extend_from_slice(&self.routing_family.0);
        output.push(self.feature_count);
        for feature in self.features() {
            output.extend_from_slice(&feature.to_bits().to_le_bytes());
        }
        output.push(self.tag as u8);
        encode_source(self.source, output);
        output.extend_from_slice(&self.active_revision);
        output.extend_from_slice(&self.policy_rank.to_le_bytes());
        if include_preference {
            output.extend_from_slice(&self.preference_priority.to_le_bytes());
        }
        output.extend_from_slice(&self.bootstrap_priority.to_le_bytes());
        output.push(self.forecast_count);
        for forecast in self.forecasts().iter().copied() {
            forecast.encode_canonical(output);
        }
        encode_resources(self.resources, output);
    }

    pub(super) fn decode_canonical(input: &mut Decoder<'_>) -> Result<Self, ()> {
        Self::decode_with_fields(input, true, true)
    }

    fn decode_legacy_v11(input: &mut Decoder<'_>) -> Result<Self, ()> {
        Self::decode_with_fields(input, false, false)
    }

    pub(super) fn decode_legacy_v15(input: &mut Decoder<'_>) -> Result<Self, ()> {
        Self::decode_with_fields(input, true, false)
    }

    fn decode_with_fields(
        input: &mut Decoder<'_>,
        include_preference: bool,
        include_corpus_key: bool,
    ) -> Result<Self, ()> {
        let decision = DecisionId(input.read_digest()?);
        let epoch = input.read_u64()?;
        let investment = input.read_digest()?;
        let opportunity = input.read_digest()?;
        let corpus_key = if include_corpus_key {
            input.read_digest()?
        } else {
            opportunity
        };
        let opportunity_kind = OpportunityKind::decode(input.read_u8()?)?;
        let feature_schema = FeatureSchemaId(input.read_digest()?);
        let routing_family = RoutingFamilyId(input.read_digest()?);
        let feature_count = usize::from(input.read_u8()?);
        if feature_count > MAXIMUM_FEATURES_PER_OPPORTUNITY
            || feature_count > input.remaining() / std::mem::size_of::<f32>()
        {
            return Err(());
        }
        let mut features = [0.0; MAXIMUM_FEATURES_PER_OPPORTUNITY];
        for feature in &mut features[..feature_count] {
            *feature = input.read_f32()?;
            if !feature.is_finite() {
                return Err(());
            }
        }
        let tag = InvestmentTag::decode(input.read_u8()?)?;
        let source = decode_source(input)?;
        let active_revision = input.read_digest()?;
        let policy_rank = input.read_u32()?;
        let preference_priority = if include_preference {
            input.read_u32()?
        } else {
            0
        };
        let bootstrap_priority = input.read_u32()?;
        let forecast_count = usize::from(input.read_u8()?);
        if forecast_count > MAXIMUM_TYPED_FORECASTS || forecast_count > input.remaining() / 17 {
            return Err(());
        }
        let mut forecasts = Vec::with_capacity(forecast_count);
        for _ in 0..forecast_count {
            forecasts.push(Forecast::decode_canonical(input)?);
        }
        let resources = decode_resources(input)?;
        let receipt = Self::new(
            decision,
            epoch,
            investment,
            opportunity,
            corpus_key,
            opportunity_kind,
            feature_schema,
            routing_family,
            &features[..feature_count],
            tag,
            source,
            active_revision,
            policy_rank,
            preference_priority,
            bootstrap_priority,
            &forecasts,
            resources,
        )
        .map_err(|_| ())?;
        if decision_id(
            receipt.active_revision,
            receipt.epoch,
            receipt.policy_rank,
            receipt.investment,
            receipt.source,
        ) != receipt.decision
        {
            return Err(());
        }
        Ok(receipt)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InvestmentOutcome {
    VerifiedAccepted { verification_record: SubjectId },
    VerifiedRefuted,
    VerifiedUnknown,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InvestmentSettlement {
    decision: DecisionId,
    outcome: InvestmentOutcome,
    actual_resources: ResourceVector,
}

impl InvestmentSettlement {
    pub(crate) const fn new(
        decision: DecisionId,
        outcome: InvestmentOutcome,
        actual_resources: ResourceVector,
    ) -> Self {
        Self {
            decision,
            outcome,
            actual_resources,
        }
    }

    pub(super) const fn decision(self) -> DecisionId {
        self.decision
    }

    pub(super) const fn actual_resources(self) -> ResourceVector {
        self.actual_resources
    }

    pub(super) const fn outcome(self) -> InvestmentOutcome {
        self.outcome
    }

    fn encode_canonical(self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.decision.0);
        match self.outcome {
            InvestmentOutcome::VerifiedAccepted {
                verification_record,
            } => {
                output.push(1);
                output.extend_from_slice(&verification_record.identity());
            }
            InvestmentOutcome::VerifiedRefuted => output.push(2),
            InvestmentOutcome::VerifiedUnknown => output.push(3),
            InvestmentOutcome::Completed => output.push(4),
            InvestmentOutcome::Failed => output.push(5),
        }
        encode_resources(self.actual_resources, output);
    }

    fn decode_canonical(input: &mut Decoder<'_>) -> Result<Self, ()> {
        let decision = DecisionId(input.read_digest()?);
        let outcome = match input.read_u8()? {
            1 => InvestmentOutcome::VerifiedAccepted {
                verification_record: SubjectId::new(input.read_digest()?),
            },
            2 => InvestmentOutcome::VerifiedRefuted,
            3 => InvestmentOutcome::VerifiedUnknown,
            4 => InvestmentOutcome::Completed,
            5 => InvestmentOutcome::Failed,
            _ => return Err(()),
        };
        Ok(Self {
            decision,
            outcome,
            actual_resources: decode_resources(input)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CausalSubject {
    Decision(DecisionId),
    Artifact(SubjectId),
    Specialist(SpecialistRevisionId),
    Knowledge(SubjectId),
    RuntimePolicy(SubjectId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ConsequenceKind {
    Admitted,
    ParetoImprovement,
    UsefulDescendant,
    CrossGoalUse,
    Compression,
    CpuSaved,
    OperatorEnabled,
    SelectionUse,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AttributionKind {
    Observed,
    MechanicallyInduced,
    PairedShadow(ShadowCampaignId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ConsequenceEdge {
    subject: CausalSubject,
    kind: ConsequenceKind,
    attribution: AttributionKind,
}

impl ConsequenceEdge {
    pub(crate) const fn observed(subject: CausalSubject, kind: ConsequenceKind) -> Self {
        Self {
            subject,
            kind,
            attribution: AttributionKind::Observed,
        }
    }

    pub(super) const fn subject(self) -> CausalSubject {
        self.subject
    }

    pub(super) const fn kind(self) -> ConsequenceKind {
        self.kind
    }

    pub(super) const fn attribution(self) -> AttributionKind {
        self.attribution
    }

    pub(super) const fn selection_use(corpus_key: [u8; 32]) -> Self {
        Self {
            subject: CausalSubject::Artifact(SubjectId::new(corpus_key)),
            kind: ConsequenceKind::SelectionUse,
            attribution: AttributionKind::MechanicallyInduced,
        }
    }

    pub(crate) const fn mechanically_induced(
        subject: CausalSubject,
        kind: ConsequenceKind,
    ) -> Self {
        Self {
            subject,
            kind,
            attribution: AttributionKind::MechanicallyInduced,
        }
    }

    fn encode_canonical(self, output: &mut Vec<u8>) {
        encode_subject(self.subject, output);
        output.push(encode_consequence(self.kind));
        match self.attribution {
            AttributionKind::Observed => output.push(1),
            AttributionKind::MechanicallyInduced => output.push(2),
            AttributionKind::PairedShadow(campaign) => {
                output.push(3);
                output.extend_from_slice(&campaign.0);
            }
        }
    }

    fn decode_canonical(input: &mut Decoder<'_>) -> Result<Self, ()> {
        let subject = decode_subject(input)?;
        let kind = decode_consequence(input.read_u8()?)?;
        let attribution = match input.read_u8()? {
            1 => AttributionKind::Observed,
            2 => AttributionKind::MechanicallyInduced,
            3 => AttributionKind::PairedShadow(ShadowCampaignId(input.read_digest()?)),
            _ => return Err(()),
        };
        Ok(Self {
            subject,
            kind,
            attribution,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShadowArm {
    Treatment,
    Control,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TypedOutcome {
    axis: ForecastAxis,
    value: f32,
}

impl TypedOutcome {
    pub(crate) fn new(axis: ForecastAxis, value: f32) -> Result<Self, IntelligenceError> {
        if !value.is_finite() {
            return Err(IntelligenceError::InvalidForecast);
        }
        Ok(Self { axis, value })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShadowCampaignSpec {
    id: ShadowCampaignId,
    checkpoint: CheckpointDigest,
    subject: SubjectId,
    resources: ResourceVector,
    random_stream: SubjectId,
    axes: Vec<ForecastAxis>,
}

impl ShadowCampaignSpec {
    pub(crate) fn new(
        checkpoint: CheckpointDigest,
        subject: SubjectId,
        resources: ResourceVector,
        random_stream: SubjectId,
        axes: impl IntoIterator<Item = ForecastAxis>,
    ) -> Result<Self, IntelligenceError> {
        let axes = axes.into_iter().collect::<Vec<_>>();
        if axes.is_empty()
            || axes.len() > MAXIMUM_SHADOW_AXES
            || axes.iter().copied().collect::<BTreeSet<_>>().len() != axes.len()
        {
            return Err(IntelligenceError::InvalidForecast);
        }
        let id = shadow_campaign_id(checkpoint, subject, resources, random_stream, &axes);
        Ok(Self {
            id,
            checkpoint,
            subject,
            resources,
            random_stream,
            axes,
        })
    }

    pub(crate) const fn id(&self) -> ShadowCampaignId {
        self.id
    }

    pub(crate) const fn checkpoint(&self) -> CheckpointDigest {
        self.checkpoint
    }

    pub(crate) const fn subject(&self) -> SubjectId {
        self.subject
    }

    pub(crate) const fn resources(&self) -> ResourceVector {
        self.resources
    }

    pub(crate) const fn random_stream(&self) -> SubjectId {
        self.random_stream
    }

    fn encode_canonical(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.id.0);
        output.extend_from_slice(&self.checkpoint.0);
        output.extend_from_slice(&self.subject.identity());
        encode_resources(self.resources, output);
        output.extend_from_slice(&self.random_stream.identity());
        output.extend_from_slice(&(self.axes.len() as u64).to_le_bytes());
        output.extend(self.axes.iter().map(|axis| *axis as u8));
    }

    fn decode_canonical(input: &mut Decoder<'_>) -> Result<Self, ()> {
        let expected_id = ShadowCampaignId(input.read_digest()?);
        let checkpoint = CheckpointDigest(input.read_digest()?);
        let subject = SubjectId::new(input.read_digest()?);
        let resources = decode_resources(input)?;
        let random_stream = SubjectId::new(input.read_digest()?);
        let count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
        if count == 0 || count > MAXIMUM_SHADOW_AXES || count > input.remaining() {
            return Err(());
        }
        let mut axes = Vec::with_capacity(count);
        for _ in 0..count {
            axes.push(ForecastAxis::decode(input.read_u8()?)?);
        }
        let specification =
            Self::new(checkpoint, subject, resources, random_stream, axes).map_err(|_| ())?;
        if specification.id != expected_id {
            return Err(());
        }
        Ok(specification)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ShadowArmOutcome {
    campaign: ShadowCampaignId,
    arm: ShadowArm,
    checkpoint: CheckpointDigest,
    resources: ResourceVector,
    random_stream: SubjectId,
    outcomes: Vec<TypedOutcome>,
}

impl ShadowArmOutcome {
    pub(crate) fn new(
        campaign: ShadowCampaignId,
        arm: ShadowArm,
        checkpoint: CheckpointDigest,
        resources: ResourceVector,
        random_stream: SubjectId,
        outcomes: impl IntoIterator<Item = TypedOutcome>,
    ) -> Result<Self, IntelligenceError> {
        let outcomes = outcomes.into_iter().collect::<Vec<_>>();
        if outcomes.is_empty()
            || outcomes.len() > MAXIMUM_SHADOW_AXES
            || outcomes
                .iter()
                .map(|outcome| outcome.axis)
                .collect::<BTreeSet<_>>()
                .len()
                != outcomes.len()
        {
            return Err(IntelligenceError::InvalidForecast);
        }
        Ok(Self {
            campaign,
            arm,
            checkpoint,
            resources,
            random_stream,
            outcomes,
        })
    }

    fn encode_canonical(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.campaign.0);
        output.push(match self.arm {
            ShadowArm::Treatment => 1,
            ShadowArm::Control => 2,
        });
        output.extend_from_slice(&self.checkpoint.0);
        encode_resources(self.resources, output);
        output.extend_from_slice(&self.random_stream.identity());
        output.extend_from_slice(&(self.outcomes.len() as u64).to_le_bytes());
        for outcome in &self.outcomes {
            output.push(outcome.axis as u8);
            output.extend_from_slice(&outcome.value.to_bits().to_le_bytes());
        }
    }

    fn decode_canonical(input: &mut Decoder<'_>) -> Result<Self, ()> {
        let campaign = ShadowCampaignId(input.read_digest()?);
        let arm = match input.read_u8()? {
            1 => ShadowArm::Treatment,
            2 => ShadowArm::Control,
            _ => return Err(()),
        };
        let checkpoint = CheckpointDigest(input.read_digest()?);
        let resources = decode_resources(input)?;
        let random_stream = SubjectId::new(input.read_digest()?);
        let count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
        if count == 0 || count > MAXIMUM_SHADOW_AXES || count > input.remaining() / 5 {
            return Err(());
        }
        let mut outcomes = Vec::with_capacity(count);
        for _ in 0..count {
            outcomes.push(
                TypedOutcome::new(ForecastAxis::decode(input.read_u8()?)?, input.read_f32()?)
                    .map_err(|_| ())?,
            );
        }
        Self::new(
            campaign,
            arm,
            checkpoint,
            resources,
            random_stream,
            outcomes,
        )
        .map_err(|_| ())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum ShadowUpdate {
    Open(ShadowCampaignSpec),
    Outcome(ShadowArmOutcome),
    Close(ShadowCampaignId),
    Invalidate {
        campaign: ShadowCampaignId,
        reason: ShadowInvalidationReason,
    },
}

impl ShadowUpdate {
    #[cfg(test)]
    pub(crate) const fn close(campaign: ShadowCampaignId) -> Self {
        Self::Close(campaign)
    }

    pub(crate) const fn interrupted(campaign: ShadowCampaignId) -> Self {
        Self::invalidate(campaign, ShadowInvalidationReason::Interrupted)
    }

    pub(crate) const fn invalidate(
        campaign: ShadowCampaignId,
        reason: ShadowInvalidationReason,
    ) -> Self {
        Self::Invalidate { campaign, reason }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShadowInvalidationReason {
    DuplicateOpen,
    DuplicateArm(ShadowArm),
    CheckpointMismatch,
    ResourceMismatch,
    RandomStreamMismatch,
    OutcomeAxesMismatch,
    AsymmetricArms,
    Interrupted,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShadowCampaignLifecycle {
    Open,
    Completed,
    Invalidated(ShadowInvalidationReason),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ContextualContrast {
    campaign: ShadowCampaignId,
    subject: SubjectId,
    axis: ForecastAxis,
    treatment: f32,
    control: f32,
    delta: f32,
}

impl ContextualContrast {
    pub(super) const fn campaign(self) -> ShadowCampaignId {
        self.campaign
    }

    pub(super) const fn subject(self) -> SubjectId {
        self.subject
    }

    pub(crate) const fn axis(self) -> ForecastAxis {
        self.axis
    }

    pub(super) fn advantage(self) -> f32 {
        if self.axis.higher_is_better() {
            self.delta
        } else {
            -self.delta
        }
    }
}

#[derive(Clone, Debug)]
struct ShadowCampaignState {
    specification: ShadowCampaignSpec,
    treatment: Option<ShadowArmOutcome>,
    control: Option<ShadowArmOutcome>,
    lifecycle: ShadowCampaignLifecycle,
}

#[derive(Clone, Copy)]
pub(crate) struct ShadowCampaignView<'a> {
    state: &'a ShadowCampaignState,
}

impl<'a> ShadowCampaignView<'a> {
    pub(crate) const fn specification(self) -> &'a ShadowCampaignSpec {
        &self.state.specification
    }

    pub(crate) const fn lifecycle(self) -> ShadowCampaignLifecycle {
        self.state.lifecycle
    }

    #[cfg(test)]
    pub(crate) const fn has_outcome(self, arm: ShadowArm) -> bool {
        match arm {
            ShadowArm::Treatment => self.state.treatment.is_some(),
            ShadowArm::Control => self.state.control.is_some(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AppendRoot {
    count: u64,
    digest: [u8; 32],
}

impl AppendRoot {
    fn empty(domain: &[u8]) -> Self {
        let mut digest = Sha256::new();
        digest.update(domain);
        digest.update(b"empty\0");
        Self {
            count: 0,
            digest: digest.finalize().into(),
        }
    }

    fn append(self, domain: &[u8], encoded: &[u8]) -> Self {
        let count = self.count.saturating_add(1);
        let mut digest = Sha256::new();
        digest.update(domain);
        digest.update(self.digest);
        digest.update(count.to_le_bytes());
        digest.update((encoded.len() as u64).to_le_bytes());
        digest.update(encoded);
        Self {
            count,
            digest: digest.finalize().into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CausalRootState {
    receipts: AppendRoot,
    settlements: AppendRoot,
    consequences: AppendRoot,
    shadows: [u8; 32],
}

impl Default for CausalRootState {
    fn default() -> Self {
        Self {
            receipts: AppendRoot::empty(b"reflex-causal-receipts-v1\0"),
            settlements: AppendRoot::empty(b"reflex-causal-settlements-v1\0"),
            consequences: AppendRoot::empty(b"reflex-causal-consequences-v1\0"),
            shadows: empty_shadow_root(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct CausalLedger {
    receipts: Vec<InvestmentReceipt>,
    settlements: Vec<InvestmentSettlement>,
    consequences: Vec<ConsequenceEdge>,
    shadows: Vec<ShadowCampaignState>,
    contrasts: Vec<ContextualContrast>,
    decisions: HashSet<DecisionId>,
    roots: CausalRootState,
}

#[derive(Clone, Debug)]
pub(super) struct CausalDelta {
    receipts: Vec<InvestmentReceipt>,
    settlements: Vec<InvestmentSettlement>,
    consequences: Vec<ConsequenceEdge>,
    shadow_updates: Vec<ShadowUpdate>,
    proposed_shadows: Option<Vec<ShadowCampaignState>>,
    proposed_contrasts: Option<Vec<ContextualContrast>>,
    roots: CausalRootState,
}

pub(super) struct CausalOverlay<'a> {
    base: &'a CausalLedger,
    delta: &'a CausalDelta,
}

pub(super) trait CausalEvidence {
    fn authoritative_verification(
        &self,
        decision: DecisionId,
        obligation: SubjectId,
    ) -> Result<InvestmentOutcome, IntelligenceError>;

    fn contrasts(&self) -> &[ContextualContrast];

    fn consequence_slices(&self) -> (&[ConsequenceEdge], &[ConsequenceEdge]);

    fn runtime_policy_evidence(
        &self,
        campaign: ShadowCampaignId,
        challenger: SubjectId,
    ) -> Result<(OperationalEvidence, OperationalEvidence), IntelligenceError>;
}

impl CausalEvidence for CausalLedger {
    fn authoritative_verification(
        &self,
        decision: DecisionId,
        obligation: SubjectId,
    ) -> Result<InvestmentOutcome, IntelligenceError> {
        CausalLedger::authoritative_verification(self, decision, obligation)
    }

    fn contrasts(&self) -> &[ContextualContrast] {
        &self.contrasts
    }

    fn consequence_slices(&self) -> (&[ConsequenceEdge], &[ConsequenceEdge]) {
        (&self.consequences, &[])
    }

    fn runtime_policy_evidence(
        &self,
        campaign: ShadowCampaignId,
        challenger: SubjectId,
    ) -> Result<(OperationalEvidence, OperationalEvidence), IntelligenceError> {
        runtime_policy_evidence(&self.shadows, campaign, challenger)
    }
}

impl CausalEvidence for CausalOverlay<'_> {
    fn authoritative_verification(
        &self,
        decision: DecisionId,
        obligation: SubjectId,
    ) -> Result<InvestmentOutcome, IntelligenceError> {
        if let Some((receipt, settlement)) = self
            .delta
            .receipts
            .iter()
            .zip(&self.delta.settlements)
            .find(|(receipt, _)| receipt.decision == decision)
        {
            return authoritative_verification_pair(receipt, settlement, obligation);
        }
        self.base.authoritative_verification(decision, obligation)
    }

    fn contrasts(&self) -> &[ContextualContrast] {
        self.delta
            .proposed_contrasts
            .as_deref()
            .unwrap_or(&self.base.contrasts)
    }

    fn consequence_slices(&self) -> (&[ConsequenceEdge], &[ConsequenceEdge]) {
        (&self.base.consequences, &self.delta.consequences)
    }

    fn runtime_policy_evidence(
        &self,
        campaign: ShadowCampaignId,
        challenger: SubjectId,
    ) -> Result<(OperationalEvidence, OperationalEvidence), IntelligenceError> {
        runtime_policy_evidence(
            self.delta
                .proposed_shadows
                .as_deref()
                .unwrap_or(&self.base.shadows),
            campaign,
            challenger,
        )
    }
}

fn runtime_policy_evidence(
    shadows: &[ShadowCampaignState],
    campaign: ShadowCampaignId,
    challenger: SubjectId,
) -> Result<(OperationalEvidence, OperationalEvidence), IntelligenceError> {
    let state = shadows
        .iter()
        .find(|state| state.specification.id == campaign)
        .filter(|state| {
            state.lifecycle == ShadowCampaignLifecycle::Completed
                && state.specification.subject == challenger
        })
        .ok_or(IntelligenceError::InvalidPolicy)?;
    let treatment = state
        .treatment
        .as_ref()
        .ok_or(IntelligenceError::InvalidPolicy)?;
    let control = state
        .control
        .as_ref()
        .ok_or(IntelligenceError::InvalidPolicy)?;
    Ok((
        operational_evidence_from_shadow(control)?,
        operational_evidence_from_shadow(treatment)?,
    ))
}

fn operational_evidence_from_shadow(
    outcome: &ShadowArmOutcome,
) -> Result<OperationalEvidence, IntelligenceError> {
    let count = |axis| {
        let value = outcome
            .outcomes
            .iter()
            .find(|outcome| outcome.axis == axis)
            .map(|outcome| outcome.value)
            .ok_or(IntelligenceError::InvalidPolicy)?;
        if value < 0.0 || value.fract() != 0.0 || value > f32::from(u16::MAX) {
            return Err(IntelligenceError::InvalidPolicy);
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the finite integral Shadow count was bounded to u16 immediately above"
        )]
        let bounded = value as u16;
        Ok(u64::from(bounded))
    };
    Ok(OperationalEvidence::new(
        count(ForecastAxis::KernelAcceptance)?,
        count(ForecastAxis::ImmediateImprovement)?,
        count(ForecastAxis::CrossGoalLeverage)?,
        count(ForecastAxis::UsefulDescendants)?,
        outcome.resources.cpu_time_ns,
        outcome.resources.verification_requests,
        outcome.resources.durable_bytes,
    ))
}

impl CausalDelta {
    pub(super) const fn is_empty(&self) -> bool {
        self.receipts.is_empty()
            && self.settlements.is_empty()
            && self.consequences.is_empty()
            && self.shadow_updates.is_empty()
    }

    pub(super) fn recent_aligned(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&InvestmentReceipt, InvestmentSettlement)> {
        self.receipts
            .iter()
            .zip(&self.settlements)
            .map(|(receipt, settlement)| (receipt, *settlement))
    }

    pub(super) fn canonical_root(&self) -> [u8; 32] {
        combined_causal_root(self.roots)
    }

    pub(super) fn record_selection_uses(
        &mut self,
        base: &CausalLedger,
        cases: &[[u8; 32]],
        limits: IntelligenceLimits,
    ) -> Result<(), IntelligenceError> {
        if cases.is_empty() {
            return Ok(());
        }
        let consequence_limit = limits.maximum_forecast_cells.saturating_mul(64);
        if base
            .consequences
            .len()
            .saturating_add(self.consequences.len())
            .saturating_add(cases.len())
            > consequence_limit
        {
            return Err(IntelligenceError::CapacityExceeded);
        }
        let first_use = self.consequences.len();
        self.consequences
            .try_reserve_exact(cases.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        self.consequences
            .extend(cases.iter().copied().map(ConsequenceEdge::selection_use));
        let uses = &self.consequences[first_use..];
        self.roots =
            extend_causal_roots(self.roots, &[], &[], uses, self.proposed_shadows.as_deref());
        Ok(())
    }

    pub(super) const fn has_shadow_updates(&self) -> bool {
        !self.shadow_updates.is_empty()
    }

    #[cfg(test)]
    pub(super) fn staged_item_count(&self) -> usize {
        self.receipts
            .len()
            .saturating_add(self.settlements.len())
            .saturating_add(self.consequences.len())
            .saturating_add(self.shadow_updates.len())
    }

    pub(super) fn encode_canonical(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&(self.receipts.len() as u64).to_le_bytes());
        for receipt in self.receipts.iter().copied() {
            receipt.encode_canonical(output);
        }
        output.extend_from_slice(&(self.settlements.len() as u64).to_le_bytes());
        for settlement in self.settlements.iter().copied() {
            settlement.encode_canonical(output);
        }
        output.extend_from_slice(&(self.consequences.len() as u64).to_le_bytes());
        for consequence in self.consequences.iter().copied() {
            consequence.encode_canonical(output);
        }
        output.extend_from_slice(&(self.shadow_updates.len() as u64).to_le_bytes());
        for update in &self.shadow_updates {
            encode_shadow_update(update, output);
        }
    }

    pub(super) fn decode_canonical(
        input: &mut Decoder<'_>,
        base: &CausalLedger,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        Self::decode_with_fields(input, base, limits, true, true)
    }

    pub(super) fn decode_legacy_v11(
        input: &mut Decoder<'_>,
        base: &CausalLedger,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        Self::decode_with_fields(input, base, limits, false, false)
    }

    pub(super) fn decode_legacy_v15(
        input: &mut Decoder<'_>,
        base: &CausalLedger,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        Self::decode_with_fields(input, base, limits, true, false)
    }

    fn decode_with_fields(
        input: &mut Decoder<'_>,
        base: &CausalLedger,
        limits: IntelligenceLimits,
        include_preference: bool,
        include_corpus_key: bool,
    ) -> Result<Self, ()> {
        let entry_limit = limits.maximum_investments.saturating_mul(64);
        let consequence_limit = limits.maximum_forecast_cells.saturating_mul(64);
        let shadow_limit = limits.maximum_opportunities.saturating_mul(16);
        let receipt_count = read_count(input, entry_limit, 221)?;
        let mut receipts = Vec::with_capacity(receipt_count);
        for _ in 0..receipt_count {
            receipts.push(match (include_preference, include_corpus_key) {
                (_, true) => InvestmentReceipt::decode_canonical(input)?,
                (true, false) => InvestmentReceipt::decode_legacy_v15(input)?,
                (false, false) => InvestmentReceipt::decode_legacy_v11(input)?,
            });
        }
        let settlement_count = read_count(input, entry_limit, 73)?;
        let mut settlements = Vec::with_capacity(settlement_count);
        for _ in 0..settlement_count {
            settlements.push(InvestmentSettlement::decode_canonical(input)?);
        }
        let consequence_count = read_count(input, consequence_limit, 34)?;
        let mut consequences = Vec::with_capacity(consequence_count);
        for _ in 0..consequence_count {
            consequences.push(ConsequenceEdge::decode_canonical(input)?);
        }
        let update_count = read_count(input, shadow_limit.saturating_mul(4), 1)?;
        let mut shadows = Vec::with_capacity(update_count);
        for _ in 0..update_count {
            shadows.push(decode_shadow_update(input)?);
        }
        base.prepare_delta(&receipts, &settlements, &consequences, &shadows, limits)
            .map_err(|_| ())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct CausalExperienceView<'a> {
    ledger: &'a CausalLedger,
}

impl<'a> CausalExperienceView<'a> {
    #[cfg(any(test, feature = "internal-experiments"))]
    pub(crate) fn receipts(self) -> &'a [InvestmentReceipt] {
        &self.ledger.receipts
    }

    pub(crate) fn settlements(self) -> &'a [InvestmentSettlement] {
        &self.ledger.settlements
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    pub(crate) fn consequences(self) -> &'a [ConsequenceEdge] {
        &self.ledger.consequences
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    pub(crate) fn contrasts(self) -> &'a [ContextualContrast] {
        &self.ledger.contrasts
    }

    pub(crate) fn shadow_campaign(
        self,
        campaign: ShadowCampaignId,
    ) -> Option<ShadowCampaignView<'a>> {
        self.ledger
            .shadows
            .iter()
            .find(|state| state.specification.id == campaign)
            .map(|state| ShadowCampaignView { state })
    }

    pub(crate) fn shadow_campaigns(
        self,
    ) -> impl ExactSizeIterator<Item = ShadowCampaignView<'a>> + 'a {
        self.ledger
            .shadows
            .iter()
            .map(|state| ShadowCampaignView { state })
    }

    #[cfg(test)]
    pub(crate) fn authoritative_verification_decision(
        self,
        subject: SubjectId,
    ) -> Option<DecisionId> {
        self.ledger
            .receipts
            .iter()
            .zip(&self.ledger.settlements)
            .rev()
            .find_map(|(receipt, settlement)| {
                (receipt.tag == InvestmentTag::Verify
                    && settlement.decision == receipt.decision
                    && matches!(
                        settlement.outcome,
                        InvestmentOutcome::VerifiedAccepted {
                            verification_record
                        } if verification_record == subject
                    ))
                .then_some(receipt.decision)
            })
    }

    #[cfg(test)]
    pub(crate) fn authoritative_verification_outcome(
        self,
        decision: DecisionId,
        subject: SubjectId,
    ) -> Result<InvestmentOutcome, IntelligenceError> {
        self.ledger.authoritative_verification(decision, subject)
    }
}

impl CausalLedger {
    pub(super) const fn receipt_count(&self) -> usize {
        self.receipts.len()
    }

    pub(super) const fn view(&self) -> CausalExperienceView<'_> {
        CausalExperienceView { ledger: self }
    }

    pub(super) fn heap_bytes(&self) -> usize {
        vector_heap_bytes(&self.receipts)
            .saturating_add(vector_heap_bytes(&self.settlements))
            .saturating_add(vector_heap_bytes(&self.consequences))
            .saturating_add(vector_heap_bytes(&self.contrasts))
            .saturating_add(
                self.shadows
                    .capacity()
                    .saturating_sub(self.shadows.len())
                    .saturating_mul(std::mem::size_of::<ShadowCampaignState>()),
            )
            .saturating_add(self.shadows.iter().fold(0_usize, |bytes, shadow| {
                bytes.saturating_add(shadow.resident_bytes())
            }))
            .saturating_add(
                self.decisions
                    .capacity()
                    .saturating_mul(std::mem::size_of::<DecisionId>()),
            )
    }

    pub(super) fn fits_limits(&self, limits: IntelligenceLimits) -> bool {
        let Some(entry_limit) = limits.maximum_investments.checked_mul(64) else {
            return false;
        };
        let Some(consequence_limit) = limits.maximum_forecast_cells.checked_mul(64) else {
            return false;
        };
        let Some(shadow_limit) = limits.maximum_opportunities.checked_mul(16) else {
            return false;
        };
        self.receipts.len() <= entry_limit
            && self.settlements.len() <= entry_limit
            && self.consequences.len() <= consequence_limit
            && self.shadows.len() <= shadow_limit
    }

    pub(super) fn authoritative_verification(
        &self,
        decision: DecisionId,
        obligation: SubjectId,
    ) -> Result<InvestmentOutcome, IntelligenceError> {
        let receipt = self
            .receipts
            .iter()
            .find(|receipt| receipt.decision == decision)
            .ok_or(IntelligenceError::InvalidReference)?;
        let settlement = self
            .settlements
            .iter()
            .find(|settlement| settlement.decision == decision)
            .ok_or(IntelligenceError::InvalidReference)?;
        authoritative_verification_pair(receipt, settlement, obligation)
    }

    pub(super) fn contrasts(&self) -> &[ContextualContrast] {
        &self.contrasts
    }

    pub(super) fn completed_candidate_production(&self, decision: DecisionId) -> bool {
        self.receipts
            .iter()
            .zip(&self.settlements)
            .any(|(receipt, settlement)| {
                receipt.decision == decision
                    && settlement.decision == decision
                    && matches!(
                        receipt.tag,
                        InvestmentTag::Generate | InvestmentTag::Repair | InvestmentTag::Explore
                    )
                    && settlement.outcome == InvestmentOutcome::Completed
            })
    }

    pub(super) fn latest_completed_consolidation(&self) -> Option<DecisionId> {
        self.receipts
            .iter()
            .zip(&self.settlements)
            .rev()
            .find_map(|(receipt, settlement)| {
                (receipt.tag == InvestmentTag::Consolidate
                    && settlement.outcome == InvestmentOutcome::Completed)
                    .then_some(receipt.decision)
            })
    }

    pub(super) fn canonical_root(&self) -> [u8; 32] {
        combined_causal_root(self.roots)
    }

    pub(super) fn legacy_v11_canonical_root(&self) -> [u8; 32] {
        let roots = extend_causal_roots_with_fields(
            CausalRootState::default(),
            &self.receipts,
            &self.settlements,
            &self.consequences,
            Some(&self.shadows),
            false,
            false,
        );
        combined_causal_root(roots)
    }

    pub(super) fn legacy_v15_canonical_root(&self) -> [u8; 32] {
        let roots = extend_causal_roots_with_fields(
            CausalRootState::default(),
            &self.receipts,
            &self.settlements,
            &self.consequences,
            Some(&self.shadows),
            true,
            false,
        );
        combined_causal_root(roots)
    }

    pub(super) const fn overlay<'a>(&'a self, delta: &'a CausalDelta) -> CausalOverlay<'a> {
        CausalOverlay { base: self, delta }
    }

    pub(super) fn aligned(
        &self,
    ) -> impl DoubleEndedIterator<Item = (&InvestmentReceipt, InvestmentSettlement)> {
        self.receipts
            .iter()
            .zip(&self.settlements)
            .map(|(receipt, settlement)| (receipt, *settlement))
    }

    pub(super) fn prepare_delta(
        &self,
        receipts: &[InvestmentReceipt],
        settlements: &[InvestmentSettlement],
        consequences: &[ConsequenceEdge],
        shadows: &[ShadowUpdate],
        limits: IntelligenceLimits,
    ) -> Result<CausalDelta, IntelligenceError> {
        if receipts.len() != settlements.len()
            || receipts
                .iter()
                .zip(settlements)
                .any(|(receipt, settlement)| {
                    receipt.decision != settlement.decision
                        || !outcome_is_compatible(receipt.tag, settlement.outcome)
                })
        {
            return Err(IntelligenceError::InvalidSettlement);
        }
        let entry_limit = limits.maximum_investments.saturating_mul(64);
        let consequence_limit = limits.maximum_forecast_cells.saturating_mul(64);
        let shadow_limit = limits.maximum_opportunities.saturating_mul(16);
        if self.receipts.len().saturating_add(receipts.len()) > entry_limit
            || self.settlements.len().saturating_add(settlements.len()) > entry_limit
            || self.consequences.len().saturating_add(consequences.len()) > consequence_limit
        {
            return Err(IntelligenceError::CapacityExceeded);
        }
        let mut decisions = HashSet::new();
        decisions
            .try_reserve(receipts.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        for receipt in receipts {
            if self.decisions.contains(&receipt.decision) || !decisions.insert(receipt.decision) {
                return Err(IntelligenceError::DuplicateExperience);
            }
        }
        if consequences.iter().any(|consequence| {
            matches!(
                consequence.subject,
                CausalSubject::Decision(decision)
                    if !self.decisions.contains(&decision) && !decisions.contains(&decision)
            )
        }) {
            return Err(IntelligenceError::InvalidReference);
        }
        let mut proposed_shadows = None;
        let mut proposed_contrasts = None;
        if !shadows.is_empty() {
            let mut shadow_ledger = Self {
                receipts: Vec::new(),
                settlements: Vec::new(),
                consequences: Vec::new(),
                shadows: self.shadows.clone(),
                contrasts: self.contrasts.clone(),
                decisions: HashSet::new(),
                roots: CausalRootState::default(),
            };
            shadow_ledger.apply_shadow_updates(shadows, shadow_limit)?;
            proposed_shadows = Some(shadow_ledger.shadows);
            proposed_contrasts = Some(shadow_ledger.contrasts);
        }
        let mut owned_receipts = Vec::new();
        owned_receipts
            .try_reserve_exact(receipts.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        owned_receipts.extend_from_slice(receipts);
        let mut owned_settlements = Vec::new();
        owned_settlements
            .try_reserve_exact(settlements.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        owned_settlements.extend_from_slice(settlements);
        let mut owned_consequences = Vec::new();
        owned_consequences
            .try_reserve_exact(consequences.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        owned_consequences.extend_from_slice(consequences);
        let mut owned_shadows = Vec::new();
        owned_shadows
            .try_reserve_exact(shadows.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        owned_shadows.extend_from_slice(shadows);
        let roots = extend_causal_roots(
            self.roots,
            &owned_receipts,
            &owned_settlements,
            &owned_consequences,
            proposed_shadows.as_deref(),
        );
        Ok(CausalDelta {
            receipts: owned_receipts,
            settlements: owned_settlements,
            consequences: owned_consequences,
            shadow_updates: owned_shadows,
            proposed_shadows,
            proposed_contrasts,
            roots,
        })
    }

    #[cfg(test)]
    pub(super) fn apply(
        &mut self,
        receipts: &[InvestmentReceipt],
        settlements: &[InvestmentSettlement],
        consequences: &[ConsequenceEdge],
        shadows: &[ShadowUpdate],
        limits: IntelligenceLimits,
    ) -> Result<(), IntelligenceError> {
        let delta = self.prepare_delta(receipts, settlements, consequences, shadows, limits)?;
        self.commit_delta(delta)
    }

    pub(super) fn reserve_for_delta(
        &mut self,
        delta: &CausalDelta,
    ) -> Result<(), IntelligenceError> {
        self.receipts
            .try_reserve(delta.receipts.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        self.settlements
            .try_reserve(delta.settlements.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        self.consequences
            .try_reserve(delta.consequences.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        self.decisions
            .try_reserve(delta.receipts.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        Ok(())
    }

    pub(super) fn reservation_resident_bytes(&self, delta: &CausalDelta) -> u64 {
        fn vector_growth<T>(values: &Vec<T>, additional: usize) -> usize {
            let required = values.len().saturating_add(additional);
            if required <= values.capacity() {
                return 0;
            }
            let minimum = if std::mem::size_of::<T>() == 1 { 8 } else { 4 };
            let target = values
                .capacity()
                .saturating_mul(2)
                .max(required)
                .max(minimum);
            target
                .saturating_sub(values.capacity())
                .saturating_mul(std::mem::size_of::<T>())
        }

        let decisions_required = self.decisions.len().saturating_add(delta.receipts.len());
        let decision_growth = if decisions_required <= self.decisions.capacity() {
            0
        } else {
            decisions_required
                .saturating_mul(2)
                .checked_next_power_of_two()
                .unwrap_or(usize::MAX)
                .saturating_sub(self.decisions.capacity())
                .saturating_mul(std::mem::size_of::<DecisionId>())
        };
        u64::try_from(
            vector_growth(&self.receipts, delta.receipts.len())
                .saturating_add(vector_growth(&self.settlements, delta.settlements.len()))
                .saturating_add(vector_growth(&self.consequences, delta.consequences.len()))
                .saturating_add(decision_growth),
        )
        .unwrap_or(u64::MAX)
    }

    pub(super) fn commit_delta(&mut self, delta: CausalDelta) -> Result<(), IntelligenceError> {
        self.reserve_for_delta(&delta)?;
        self.commit_reserved_delta(delta);
        Ok(())
    }

    pub(super) fn commit_reserved_delta(&mut self, mut delta: CausalDelta) {
        self.decisions
            .extend(delta.receipts.iter().map(|receipt| receipt.decision));
        self.receipts.append(&mut delta.receipts);
        self.settlements.append(&mut delta.settlements);
        self.consequences.append(&mut delta.consequences);
        if let Some(shadows) = delta.proposed_shadows {
            self.shadows = shadows;
            self.contrasts = delta.proposed_contrasts.unwrap_or_default();
        }
        self.roots = delta.roots;
    }

    fn apply_shadow_updates(
        &mut self,
        shadows: &[ShadowUpdate],
        shadow_limit: usize,
    ) -> Result<(), IntelligenceError> {
        for update in shadows {
            match update {
                ShadowUpdate::Open(specification) => {
                    if let Some(index) = self
                        .shadows
                        .iter()
                        .position(|state| state.specification.id == specification.id)
                    {
                        self.invalidate_shadow(index, ShadowInvalidationReason::DuplicateOpen);
                        continue;
                    }
                    if self.shadows.len() == shadow_limit {
                        return Err(IntelligenceError::CapacityExceeded);
                    }
                    self.shadows.push(ShadowCampaignState {
                        specification: specification.clone(),
                        treatment: None,
                        control: None,
                        lifecycle: ShadowCampaignLifecycle::Open,
                    });
                }
                ShadowUpdate::Outcome(outcome) => self.apply_shadow_outcome(outcome)?,
                ShadowUpdate::Close(campaign) => {
                    let Some(index) = self
                        .shadows
                        .iter()
                        .position(|state| state.specification.id == *campaign)
                    else {
                        return Err(IntelligenceError::InvalidReference);
                    };
                    if matches!(self.shadows[index].lifecycle, ShadowCampaignLifecycle::Open) {
                        self.invalidate_shadow(index, ShadowInvalidationReason::AsymmetricArms);
                    }
                }
                ShadowUpdate::Invalidate { campaign, reason } => {
                    let Some(index) = self
                        .shadows
                        .iter()
                        .position(|state| state.specification.id == *campaign)
                    else {
                        return Err(IntelligenceError::InvalidReference);
                    };
                    self.invalidate_shadow(index, *reason);
                }
            }
        }
        Ok(())
    }

    fn apply_shadow_outcome(
        &mut self,
        outcome: &ShadowArmOutcome,
    ) -> Result<(), IntelligenceError> {
        let Some(index) = self
            .shadows
            .iter()
            .position(|state| state.specification.id == outcome.campaign)
        else {
            return Err(IntelligenceError::InvalidReference);
        };
        if matches!(
            self.shadows[index].lifecycle,
            ShadowCampaignLifecycle::Invalidated(_)
        ) {
            return Ok(());
        }
        if matches!(
            self.shadows[index].lifecycle,
            ShadowCampaignLifecycle::Completed
        ) {
            self.invalidate_shadow(index, ShadowInvalidationReason::DuplicateArm(outcome.arm));
            return Ok(());
        }
        if let Some(reason) = shadow_outcome_mismatch(&self.shadows[index].specification, outcome) {
            self.invalidate_shadow(index, reason);
            return Ok(());
        }
        let state = &mut self.shadows[index];
        let target = match outcome.arm {
            ShadowArm::Treatment => &mut state.treatment,
            ShadowArm::Control => &mut state.control,
        };
        if target.is_some() {
            self.invalidate_shadow(index, ShadowInvalidationReason::DuplicateArm(outcome.arm));
            return Ok(());
        }
        *target = Some(outcome.clone());
        if let (Some(treatment), Some(control)) = (&state.treatment, &state.control) {
            state.lifecycle = ShadowCampaignLifecycle::Completed;
            let specification = &state.specification;
            for (treatment, control) in treatment.outcomes.iter().zip(&control.outcomes) {
                self.contrasts.push(ContextualContrast {
                    campaign: specification.id,
                    subject: specification.subject,
                    axis: treatment.axis,
                    treatment: treatment.value,
                    control: control.value,
                    delta: treatment.value - control.value,
                });
            }
        }
        Ok(())
    }

    fn invalidate_shadow(&mut self, index: usize, reason: ShadowInvalidationReason) {
        if matches!(
            self.shadows[index].lifecycle,
            ShadowCampaignLifecycle::Invalidated(_)
        ) {
            return;
        }
        let campaign = self.shadows[index].specification.id;
        self.shadows[index].lifecycle = ShadowCampaignLifecycle::Invalidated(reason);
        self.contrasts
            .retain(|contrast| contrast.campaign != campaign);
    }

    pub(super) fn encode_canonical(&self, output: &mut Vec<u8>) {
        self.encode_with_fields(output, true, true);
    }

    pub(super) fn encode_legacy_v11(&self, output: &mut Vec<u8>) {
        self.encode_with_fields(output, false, false);
    }

    pub(super) fn encode_legacy_v15(&self, output: &mut Vec<u8>) {
        self.encode_with_fields(output, true, false);
    }

    fn encode_with_fields(
        &self,
        output: &mut Vec<u8>,
        include_preference: bool,
        include_corpus_key: bool,
    ) {
        output.extend_from_slice(&(self.receipts.len() as u64).to_le_bytes());
        for receipt in self.receipts.iter().copied() {
            match (include_preference, include_corpus_key) {
                (_, true) => receipt.encode_canonical(output),
                (true, false) => receipt.encode_legacy_v15(output),
                (false, false) => receipt.encode_legacy_v11(output),
            }
        }
        output.extend_from_slice(&(self.settlements.len() as u64).to_le_bytes());
        for settlement in self.settlements.iter().copied() {
            settlement.encode_canonical(output);
        }
        output.extend_from_slice(&(self.consequences.len() as u64).to_le_bytes());
        for consequence in self.consequences.iter().copied() {
            consequence.encode_canonical(output);
        }
        output.extend_from_slice(&(self.shadows.len() as u64).to_le_bytes());
        for shadow in &self.shadows {
            shadow.specification.encode_canonical(output);
            encode_shadow_lifecycle(shadow.lifecycle, output);
            encode_optional_outcome(shadow.treatment.as_ref(), output);
            encode_optional_outcome(shadow.control.as_ref(), output);
        }
    }

    pub(super) fn decode_canonical(
        input: &mut Decoder<'_>,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        Self::decode_with_fields(input, limits, true, true)
    }

    pub(super) fn decode_legacy_v11(
        input: &mut Decoder<'_>,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        Self::decode_with_fields(input, limits, false, false)
    }

    pub(super) fn decode_legacy_v15(
        input: &mut Decoder<'_>,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        Self::decode_with_fields(input, limits, true, false)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the bounded ledger decoder validates each section and its cross-record invariants in wire order"
    )]
    fn decode_with_fields(
        input: &mut Decoder<'_>,
        limits: IntelligenceLimits,
        include_preference: bool,
        include_corpus_key: bool,
    ) -> Result<Self, ()> {
        let entry_limit = limits.maximum_investments.saturating_mul(64);
        let consequence_limit = limits.maximum_forecast_cells.saturating_mul(64);
        let shadow_limit = limits.maximum_opportunities.saturating_mul(16);
        let receipt_count = read_count(input, entry_limit, 221)?;
        let mut receipts = Vec::with_capacity(receipt_count);
        for _ in 0..receipt_count {
            receipts.push(match (include_preference, include_corpus_key) {
                (_, true) => InvestmentReceipt::decode_canonical(input)?,
                (true, false) => InvestmentReceipt::decode_legacy_v15(input)?,
                (false, false) => InvestmentReceipt::decode_legacy_v11(input)?,
            });
        }
        let settlement_count = read_count(input, entry_limit, 73)?;
        let mut settlements = Vec::with_capacity(settlement_count);
        for _ in 0..settlement_count {
            settlements.push(InvestmentSettlement::decode_canonical(input)?);
        }
        let consequence_count = read_count(input, consequence_limit, 34)?;
        let mut consequences = Vec::with_capacity(consequence_count);
        for _ in 0..consequence_count {
            consequences.push(ConsequenceEdge::decode_canonical(input)?);
        }
        let shadow_count = read_count(input, shadow_limit, 179)?;
        let mut ledger = Self {
            receipts,
            settlements,
            consequences,
            shadows: Vec::with_capacity(shadow_count),
            contrasts: Vec::new(),
            decisions: HashSet::new(),
            roots: CausalRootState::default(),
        };
        validate_ledger_references(&ledger)?;
        for _ in 0..shadow_count {
            let specification = ShadowCampaignSpec::decode_canonical(input)?;
            if ledger
                .shadows
                .iter()
                .any(|state| state.specification.id == specification.id)
            {
                return Err(());
            }
            let lifecycle = decode_shadow_lifecycle(input)?;
            let mut treatment = None;
            let mut control = None;
            for expected_arm in [ShadowArm::Treatment, ShadowArm::Control] {
                match input.read_u8()? {
                    0 => {}
                    1 => {
                        let outcome = ShadowArmOutcome::decode_canonical(input)?;
                        if outcome.arm != expected_arm
                            || outcome.campaign != specification.id
                            || shadow_outcome_mismatch(&specification, &outcome).is_some()
                        {
                            return Err(());
                        }
                        match expected_arm {
                            ShadowArm::Treatment => treatment = Some(outcome),
                            ShadowArm::Control => control = Some(outcome),
                        }
                    }
                    _ => return Err(()),
                }
            }
            match lifecycle {
                ShadowCampaignLifecycle::Open if treatment.is_some() && control.is_some() => {
                    return Err(());
                }
                ShadowCampaignLifecycle::Completed if treatment.is_none() || control.is_none() => {
                    return Err(());
                }
                ShadowCampaignLifecycle::Open
                | ShadowCampaignLifecycle::Completed
                | ShadowCampaignLifecycle::Invalidated(_) => {}
            }
            if lifecycle == ShadowCampaignLifecycle::Completed {
                let treatment_outcome = treatment.as_ref().ok_or(())?;
                let control_outcome = control.as_ref().ok_or(())?;
                for (treatment, control) in treatment_outcome
                    .outcomes
                    .iter()
                    .zip(&control_outcome.outcomes)
                {
                    ledger.contrasts.push(ContextualContrast {
                        campaign: specification.id,
                        subject: specification.subject,
                        axis: treatment.axis,
                        treatment: treatment.value,
                        control: control.value,
                        delta: treatment.value - control.value,
                    });
                }
            }
            ledger.shadows.push(ShadowCampaignState {
                specification,
                treatment,
                control,
                lifecycle,
            });
        }
        ledger.rebuild_indexes_and_roots();
        Ok(ledger)
    }

    fn rebuild_indexes_and_roots(&mut self) {
        self.decisions.clear();
        self.decisions
            .extend(self.receipts.iter().map(|receipt| receipt.decision));
        self.roots = extend_causal_roots(
            CausalRootState::default(),
            &self.receipts,
            &self.settlements,
            &self.consequences,
            Some(&self.shadows),
        );
    }
}

pub(super) fn decision_id(
    active_revision: [u8; 32],
    epoch: u64,
    policy_rank: u32,
    investment: [u8; 32],
    source: AllocationSource,
) -> DecisionId {
    let mut digest = Sha256::new();
    digest.update(b"reflex-investment-decision-v1\0");
    digest.update(active_revision);
    digest.update(epoch.to_le_bytes());
    digest.update(policy_rank.to_le_bytes());
    digest.update(investment);
    let mut encoded_source = Vec::with_capacity(33);
    encode_source(source, &mut encoded_source);
    digest.update(encoded_source);
    DecisionId(digest.finalize().into())
}

fn shadow_campaign_id(
    checkpoint: CheckpointDigest,
    subject: SubjectId,
    resources: ResourceVector,
    random_stream: SubjectId,
    axes: &[ForecastAxis],
) -> ShadowCampaignId {
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&checkpoint.0);
    encoded.extend_from_slice(&subject.identity());
    encode_resources(resources, &mut encoded);
    encoded.extend_from_slice(&random_stream.identity());
    encoded.extend_from_slice(&(axes.len() as u64).to_le_bytes());
    encoded.extend(axes.iter().map(|axis| *axis as u8));
    let mut digest = Sha256::new();
    digest.update(b"reflex-shadow-campaign-v1\0");
    digest.update(encoded);
    ShadowCampaignId(digest.finalize().into())
}

fn encode_resources(resources: ResourceVector, output: &mut Vec<u8>) {
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

fn decode_resources(input: &mut Decoder<'_>) -> Result<ResourceVector, ()> {
    Ok(ResourceVector::new(
        input.read_u64()?,
        input.read_u64()?,
        input.read_u64()?,
        input.read_u64()?,
        input.read_u64()?,
    ))
}

fn encode_source(source: AllocationSource, output: &mut Vec<u8>) {
    match source {
        AllocationSource::Bootstrap => output.push(1),
        AllocationSource::Specialist(specialist) => {
            output.push(2);
            output.extend_from_slice(&specialist.0);
        }
    }
}

fn decode_source(input: &mut Decoder<'_>) -> Result<AllocationSource, ()> {
    match input.read_u8()? {
        1 => Ok(AllocationSource::Bootstrap),
        2 => Ok(AllocationSource::Specialist(SpecialistRevisionId(
            input.read_digest()?,
        ))),
        _ => Err(()),
    }
}

fn encode_subject(subject: CausalSubject, output: &mut Vec<u8>) {
    match subject {
        CausalSubject::Decision(decision) => {
            output.push(1);
            output.extend_from_slice(&decision.0);
        }
        CausalSubject::Artifact(subject) => {
            output.push(2);
            output.extend_from_slice(&subject.identity());
        }
        CausalSubject::Specialist(specialist) => {
            output.push(3);
            output.extend_from_slice(&specialist.0);
        }
        CausalSubject::Knowledge(subject) => {
            output.push(4);
            output.extend_from_slice(&subject.identity());
        }
        CausalSubject::RuntimePolicy(subject) => {
            output.push(5);
            output.extend_from_slice(&subject.identity());
        }
    }
}

fn decode_subject(input: &mut Decoder<'_>) -> Result<CausalSubject, ()> {
    let tag = input.read_u8()?;
    let identity = input.read_digest()?;
    match tag {
        1 => Ok(CausalSubject::Decision(DecisionId(identity))),
        2 => Ok(CausalSubject::Artifact(SubjectId::new(identity))),
        3 => Ok(CausalSubject::Specialist(SpecialistRevisionId(identity))),
        4 => Ok(CausalSubject::Knowledge(SubjectId::new(identity))),
        5 => Ok(CausalSubject::RuntimePolicy(SubjectId::new(identity))),
        _ => Err(()),
    }
}

const fn encode_consequence(kind: ConsequenceKind) -> u8 {
    match kind {
        ConsequenceKind::Admitted => 1,
        ConsequenceKind::ParetoImprovement => 2,
        ConsequenceKind::UsefulDescendant => 3,
        ConsequenceKind::CrossGoalUse => 4,
        ConsequenceKind::Compression => 5,
        ConsequenceKind::CpuSaved => 6,
        ConsequenceKind::OperatorEnabled => 7,
        ConsequenceKind::SelectionUse => 8,
    }
}

fn decode_consequence(value: u8) -> Result<ConsequenceKind, ()> {
    match value {
        1 => Ok(ConsequenceKind::Admitted),
        2 => Ok(ConsequenceKind::ParetoImprovement),
        3 => Ok(ConsequenceKind::UsefulDescendant),
        4 => Ok(ConsequenceKind::CrossGoalUse),
        5 => Ok(ConsequenceKind::Compression),
        6 => Ok(ConsequenceKind::CpuSaved),
        7 => Ok(ConsequenceKind::OperatorEnabled),
        8 => Ok(ConsequenceKind::SelectionUse),
        _ => Err(()),
    }
}

fn encode_optional_outcome(outcome: Option<&ShadowArmOutcome>, output: &mut Vec<u8>) {
    match outcome {
        Some(outcome) => {
            output.push(1);
            outcome.encode_canonical(output);
        }
        None => output.push(0),
    }
}

fn encode_shadow_update(update: &ShadowUpdate, output: &mut Vec<u8>) {
    match update {
        ShadowUpdate::Open(specification) => {
            output.push(1);
            specification.encode_canonical(output);
        }
        ShadowUpdate::Outcome(outcome) => {
            output.push(2);
            outcome.encode_canonical(output);
        }
        ShadowUpdate::Close(campaign) => {
            output.push(3);
            output.extend_from_slice(&campaign.0);
        }
        ShadowUpdate::Invalidate { campaign, reason } => {
            output.push(4);
            output.extend_from_slice(&campaign.0);
            encode_shadow_invalidation_reason(*reason, output);
        }
    }
}

fn decode_shadow_update(input: &mut Decoder<'_>) -> Result<ShadowUpdate, ()> {
    match input.read_u8()? {
        1 => Ok(ShadowUpdate::Open(ShadowCampaignSpec::decode_canonical(
            input,
        )?)),
        2 => Ok(ShadowUpdate::Outcome(ShadowArmOutcome::decode_canonical(
            input,
        )?)),
        3 => Ok(ShadowUpdate::Close(ShadowCampaignId(input.read_digest()?))),
        4 => Ok(ShadowUpdate::Invalidate {
            campaign: ShadowCampaignId(input.read_digest()?),
            reason: decode_shadow_invalidation_reason(input)?,
        }),
        _ => Err(()),
    }
}

fn encode_shadow_invalidation_reason(reason: ShadowInvalidationReason, output: &mut Vec<u8>) {
    match reason {
        ShadowInvalidationReason::DuplicateOpen => output.push(1),
        ShadowInvalidationReason::DuplicateArm(arm) => {
            output.push(2);
            output.push(match arm {
                ShadowArm::Treatment => 1,
                ShadowArm::Control => 2,
            });
        }
        ShadowInvalidationReason::CheckpointMismatch => output.push(3),
        ShadowInvalidationReason::ResourceMismatch => output.push(4),
        ShadowInvalidationReason::RandomStreamMismatch => output.push(5),
        ShadowInvalidationReason::OutcomeAxesMismatch => output.push(6),
        ShadowInvalidationReason::AsymmetricArms => output.push(7),
        ShadowInvalidationReason::Interrupted => output.push(8),
    }
}

fn decode_shadow_invalidation_reason(
    input: &mut Decoder<'_>,
) -> Result<ShadowInvalidationReason, ()> {
    match input.read_u8()? {
        1 => Ok(ShadowInvalidationReason::DuplicateOpen),
        2 => Ok(ShadowInvalidationReason::DuplicateArm(
            match input.read_u8()? {
                1 => ShadowArm::Treatment,
                2 => ShadowArm::Control,
                _ => return Err(()),
            },
        )),
        3 => Ok(ShadowInvalidationReason::CheckpointMismatch),
        4 => Ok(ShadowInvalidationReason::ResourceMismatch),
        5 => Ok(ShadowInvalidationReason::RandomStreamMismatch),
        6 => Ok(ShadowInvalidationReason::OutcomeAxesMismatch),
        7 => Ok(ShadowInvalidationReason::AsymmetricArms),
        8 => Ok(ShadowInvalidationReason::Interrupted),
        _ => Err(()),
    }
}

fn shadow_outcome_mismatch(
    specification: &ShadowCampaignSpec,
    outcome: &ShadowArmOutcome,
) -> Option<ShadowInvalidationReason> {
    if outcome.checkpoint != specification.checkpoint {
        Some(ShadowInvalidationReason::CheckpointMismatch)
    } else if !outcome.resources.fits_within(specification.resources) {
        Some(ShadowInvalidationReason::ResourceMismatch)
    } else if outcome.random_stream != specification.random_stream {
        Some(ShadowInvalidationReason::RandomStreamMismatch)
    } else if outcome
        .outcomes
        .iter()
        .map(|item| item.axis)
        .ne(specification.axes.iter().copied())
    {
        Some(ShadowInvalidationReason::OutcomeAxesMismatch)
    } else {
        None
    }
}

fn encode_shadow_lifecycle(lifecycle: ShadowCampaignLifecycle, output: &mut Vec<u8>) {
    match lifecycle {
        ShadowCampaignLifecycle::Open => output.push(1),
        ShadowCampaignLifecycle::Completed => output.push(2),
        ShadowCampaignLifecycle::Invalidated(reason) => {
            output.push(3);
            match reason {
                ShadowInvalidationReason::DuplicateOpen => output.push(1),
                ShadowInvalidationReason::DuplicateArm(arm) => {
                    output.push(2);
                    output.push(match arm {
                        ShadowArm::Treatment => 1,
                        ShadowArm::Control => 2,
                    });
                }
                ShadowInvalidationReason::CheckpointMismatch => output.push(3),
                ShadowInvalidationReason::ResourceMismatch => output.push(4),
                ShadowInvalidationReason::RandomStreamMismatch => output.push(5),
                ShadowInvalidationReason::OutcomeAxesMismatch => output.push(6),
                ShadowInvalidationReason::AsymmetricArms => output.push(7),
                ShadowInvalidationReason::Interrupted => output.push(8),
            }
        }
    }
}

fn decode_shadow_lifecycle(input: &mut Decoder<'_>) -> Result<ShadowCampaignLifecycle, ()> {
    match input.read_u8()? {
        1 => Ok(ShadowCampaignLifecycle::Open),
        2 => Ok(ShadowCampaignLifecycle::Completed),
        3 => {
            let reason = match input.read_u8()? {
                1 => ShadowInvalidationReason::DuplicateOpen,
                2 => ShadowInvalidationReason::DuplicateArm(match input.read_u8()? {
                    1 => ShadowArm::Treatment,
                    2 => ShadowArm::Control,
                    _ => return Err(()),
                }),
                3 => ShadowInvalidationReason::CheckpointMismatch,
                4 => ShadowInvalidationReason::ResourceMismatch,
                5 => ShadowInvalidationReason::RandomStreamMismatch,
                6 => ShadowInvalidationReason::OutcomeAxesMismatch,
                7 => ShadowInvalidationReason::AsymmetricArms,
                8 => ShadowInvalidationReason::Interrupted,
                _ => return Err(()),
            };
            Ok(ShadowCampaignLifecycle::Invalidated(reason))
        }
        _ => Err(()),
    }
}

fn read_count(input: &mut Decoder<'_>, limit: usize, minimum_bytes: usize) -> Result<usize, ()> {
    let count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
    if count > limit || count > input.remaining().checked_div(minimum_bytes).unwrap_or(0) {
        return Err(());
    }
    Ok(count)
}

fn validate_ledger_references(ledger: &CausalLedger) -> Result<(), ()> {
    let decisions = ledger
        .receipts
        .iter()
        .map(|receipt| receipt.decision)
        .collect::<BTreeSet<_>>();
    if decisions.len() != ledger.receipts.len()
        || ledger.receipts.len() != ledger.settlements.len()
        || ledger
            .receipts
            .iter()
            .zip(&ledger.settlements)
            .any(|(receipt, settlement)| {
                receipt.decision != settlement.decision
                    || !outcome_is_compatible(receipt.tag, settlement.outcome)
            })
        || ledger
            .settlements
            .iter()
            .any(|settlement| !decisions.contains(&settlement.decision))
        || ledger
            .settlements
            .iter()
            .map(|settlement| settlement.decision)
            .collect::<BTreeSet<_>>()
            .len()
            != ledger.settlements.len()
        || ledger.consequences.iter().any(|consequence| {
            matches!(
                consequence.subject,
                CausalSubject::Decision(decision) if !decisions.contains(&decision)
            )
        })
    {
        return Err(());
    }
    Ok(())
}

fn authoritative_verification_pair(
    receipt: &InvestmentReceipt,
    settlement: &InvestmentSettlement,
    obligation: SubjectId,
) -> Result<InvestmentOutcome, IntelligenceError> {
    if receipt.tag != InvestmentTag::Verify || receipt.decision != settlement.decision {
        return Err(IntelligenceError::InvalidReference);
    }
    let subject_matches = match settlement.outcome {
        InvestmentOutcome::VerifiedAccepted {
            verification_record,
        } => verification_record == obligation,
        InvestmentOutcome::VerifiedRefuted | InvestmentOutcome::VerifiedUnknown => {
            receipt.opportunity == obligation.identity()
        }
        InvestmentOutcome::Completed | InvestmentOutcome::Failed => false,
    };
    if !subject_matches {
        return Err(IntelligenceError::InvalidReference);
    }
    Ok(settlement.outcome)
}

const fn outcome_is_compatible(tag: InvestmentTag, outcome: InvestmentOutcome) -> bool {
    matches!(
        (tag, outcome),
        (
            InvestmentTag::Verify,
            InvestmentOutcome::VerifiedAccepted { .. }
                | InvestmentOutcome::VerifiedRefuted
                | InvestmentOutcome::VerifiedUnknown
        ) | (
            InvestmentTag::Generate
                | InvestmentTag::Repair
                | InvestmentTag::Explore
                | InvestmentTag::TrainSpecialist
                | InvestmentTag::CompareRevision
                | InvestmentTag::Consolidate
                | InvestmentTag::RunShadowCampaign
                | InvestmentTag::ProposeRuntimePolicy,
            InvestmentOutcome::Completed | InvestmentOutcome::Failed
        )
    )
}

impl ShadowCampaignState {
    fn resident_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(
                self.specification
                    .axes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<ForecastAxis>()),
            )
            .saturating_add(
                self.treatment
                    .as_ref()
                    .map_or(0, ShadowArmOutcome::heap_bytes),
            )
            .saturating_add(
                self.control
                    .as_ref()
                    .map_or(0, ShadowArmOutcome::heap_bytes),
            )
    }
}

impl ShadowArmOutcome {
    fn heap_bytes(&self) -> usize {
        self.outcomes
            .capacity()
            .saturating_mul(std::mem::size_of::<TypedOutcome>())
    }
}

fn vector_heap_bytes<T>(values: &Vec<T>) -> usize {
    values.capacity().saturating_mul(std::mem::size_of::<T>())
}

fn empty_shadow_root() -> [u8; 32] {
    Sha256::digest(b"reflex-causal-shadows-v1\0empty\0").into()
}

fn shadow_root(shadows: &[ShadowCampaignState]) -> [u8; 32] {
    if shadows.is_empty() {
        return empty_shadow_root();
    }
    let mut digest = Sha256::new();
    digest.update(b"reflex-causal-shadows-v1\0");
    digest.update((shadows.len() as u64).to_le_bytes());
    let mut encoded = Vec::new();
    for shadow in shadows {
        encoded.clear();
        shadow.specification.encode_canonical(&mut encoded);
        encode_shadow_lifecycle(shadow.lifecycle, &mut encoded);
        encode_optional_outcome(shadow.treatment.as_ref(), &mut encoded);
        encode_optional_outcome(shadow.control.as_ref(), &mut encoded);
        digest.update((encoded.len() as u64).to_le_bytes());
        digest.update(&encoded);
    }
    digest.finalize().into()
}

fn extend_causal_roots(
    roots: CausalRootState,
    receipts: &[InvestmentReceipt],
    settlements: &[InvestmentSettlement],
    consequences: &[ConsequenceEdge],
    shadows: Option<&[ShadowCampaignState]>,
) -> CausalRootState {
    extend_causal_roots_with_fields(
        roots,
        receipts,
        settlements,
        consequences,
        shadows,
        true,
        true,
    )
}

fn extend_causal_roots_with_fields(
    mut roots: CausalRootState,
    receipts: &[InvestmentReceipt],
    settlements: &[InvestmentSettlement],
    consequences: &[ConsequenceEdge],
    shadows: Option<&[ShadowCampaignState]>,
    include_preference: bool,
    include_corpus_key: bool,
) -> CausalRootState {
    let mut encoded = Vec::new();
    for receipt in receipts.iter().copied() {
        encoded.clear();
        match (include_preference, include_corpus_key) {
            (_, true) => receipt.encode_canonical(&mut encoded),
            (true, false) => receipt.encode_legacy_v15(&mut encoded),
            (false, false) => receipt.encode_legacy_v11(&mut encoded),
        }
        roots.receipts = roots
            .receipts
            .append(b"reflex-causal-receipts-v1\0", &encoded);
    }
    for settlement in settlements.iter().copied() {
        encoded.clear();
        settlement.encode_canonical(&mut encoded);
        roots.settlements = roots
            .settlements
            .append(b"reflex-causal-settlements-v1\0", &encoded);
    }
    for consequence in consequences.iter().copied() {
        encoded.clear();
        consequence.encode_canonical(&mut encoded);
        roots.consequences = roots
            .consequences
            .append(b"reflex-causal-consequences-v1\0", &encoded);
    }
    if let Some(shadows) = shadows {
        roots.shadows = shadow_root(shadows);
    }
    roots
}

fn combined_causal_root(roots: CausalRootState) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-causal-ledger-root-v1\0");
    for root in [roots.receipts, roots.settlements, roots.consequences] {
        digest.update(root.count.to_le_bytes());
        digest.update(root.digest);
    }
    digest.update(roots.shadows);
    digest.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_decoder_rejects_hostile_oversized_forecast_vectors() {
        let active_revision = [17; 32];
        let epoch = 3;
        let investment = [18; 32];
        let source = AllocationSource::Bootstrap;
        let policy_rank = 4;
        let forecast =
            Forecast::calibrated(ForecastAxis::ImmediateImprovement, 0.75, 0.125, 63).unwrap();
        let receipt = InvestmentReceipt::new(
            decision_id(active_revision, epoch, policy_rank, investment, source),
            epoch,
            investment,
            [19; 32],
            [19; 32],
            OpportunityKind::Candidate,
            FeatureSchemaId::built_in(1),
            RoutingFamilyId::generic(),
            &[],
            InvestmentTag::Verify,
            source,
            active_revision,
            policy_rank,
            0,
            5,
            &[forecast],
            ResourceVector::new(1, 2, 3, 4, 5),
        )
        .unwrap();
        let mut encoded = Vec::new();
        receipt.encode_canonical(&mut encoded);
        let forecast_count_offset = encoded.len() - 40 - 17 - 1;
        assert_eq!(encoded[forecast_count_offset], 1);
        encoded[forecast_count_offset] = u8::try_from(MAXIMUM_TYPED_FORECASTS + 1).unwrap();

        assert!(InvestmentReceipt::decode_canonical(&mut Decoder::new(&encoded)).is_err());

        let oversized = vec![forecast; MAXIMUM_TYPED_FORECASTS + 1];
        assert_eq!(
            InvestmentReceipt::new(
                decision_id(active_revision, epoch, policy_rank, investment, source),
                epoch,
                investment,
                [19; 32],
                [19; 32],
                OpportunityKind::Candidate,
                FeatureSchemaId::built_in(1),
                RoutingFamilyId::generic(),
                &[],
                InvestmentTag::Verify,
                source,
                active_revision,
                policy_rank,
                0,
                5,
                &oversized,
                ResourceVector::new(1, 2, 3, 4, 5),
            ),
            Err(IntelligenceError::InvalidForecast)
        );
    }
}
