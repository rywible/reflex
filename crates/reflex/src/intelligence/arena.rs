use super::causal::InvestmentReceipt;
use super::core::InvestmentSpec;
use super::forecast::Forecast;
use super::model::MAXIMUM_MODEL_HEADS;
use super::types::{
    AllocationSource, FeatureSchemaId, IntelligenceError, IntelligenceLimits, InvestmentId,
    OpportunityId, ResourceVector, RoutingFamilyId, SpecialistRevisionId,
};

pub(super) const MAXIMUM_FEATURES_PER_OPPORTUNITY: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[repr(u8)]
pub(crate) enum OpportunityKind {
    Candidate = 1,
    OperatorApplication = 2,
    Repair = 3,
    ArtifactPotential = 4,
    Emergent = 5,
    KnowledgePattern = 6,
    SpecialistNiche = 7,
    RuntimePolicy = 8,
    ShadowQuestion = 9,
}

impl OpportunityKind {
    pub(super) fn decode(value: u8) -> Result<Self, ()> {
        match value {
            1 => Ok(Self::Candidate),
            2 => Ok(Self::OperatorApplication),
            3 => Ok(Self::Repair),
            4 => Ok(Self::ArtifactPotential),
            5 => Ok(Self::Emergent),
            6 => Ok(Self::KnowledgePattern),
            7 => Ok(Self::SpecialistNiche),
            8 => Ok(Self::RuntimePolicy),
            9 => Ok(Self::ShadowQuestion),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OpportunitySpec {
    pub(super) kind: OpportunityKind,
    pub(super) identity: [u8; 32],
    pub(super) corpus_key: [u8; 32],
    pub(super) feature_schema: FeatureSchemaId,
    pub(super) routing_family: RoutingFamilyId,
}

impl OpportunitySpec {
    pub(crate) const fn candidate(identity: [u8; 32]) -> Self {
        Self::candidate_with_schema(identity, FeatureSchemaId::built_in(1))
    }

    pub(crate) const fn candidate_with_schema(
        identity: [u8; 32],
        feature_schema: FeatureSchemaId,
    ) -> Self {
        Self::candidate_in_family(identity, feature_schema, RoutingFamilyId::generic())
    }

    pub(crate) const fn candidate_in_family(
        identity: [u8; 32],
        feature_schema: FeatureSchemaId,
        routing_family: RoutingFamilyId,
    ) -> Self {
        Self {
            kind: OpportunityKind::Candidate,
            identity,
            corpus_key: identity,
            feature_schema,
            routing_family,
        }
    }

    pub(crate) const fn candidate_in_corpus(
        identity: [u8; 32],
        corpus_key: [u8; 32],
        feature_schema: FeatureSchemaId,
        routing_family: RoutingFamilyId,
    ) -> Self {
        Self {
            kind: OpportunityKind::Candidate,
            identity,
            corpus_key,
            feature_schema,
            routing_family,
        }
    }

    pub(crate) const fn candidate_schema() -> FeatureSchemaId {
        FeatureSchemaId::built_in(1)
    }

    pub(crate) const fn operator_application(
        identity: [u8; 32],
        routing_family: RoutingFamilyId,
    ) -> Self {
        Self::typed(
            OpportunityKind::OperatorApplication,
            identity,
            FeatureSchemaId::built_in(2),
            routing_family,
        )
    }

    pub(crate) const fn specialist_niche(identity: [u8; 32]) -> Self {
        Self::typed(
            OpportunityKind::SpecialistNiche,
            identity,
            FeatureSchemaId::built_in(7),
            RoutingFamilyId::generic(),
        )
    }

    pub(crate) const fn knowledge_pattern(identity: [u8; 32]) -> Self {
        Self::typed(
            OpportunityKind::KnowledgePattern,
            identity,
            FeatureSchemaId::built_in(6),
            RoutingFamilyId::generic(),
        )
    }

    pub(crate) const fn shadow_question(identity: [u8; 32]) -> Self {
        Self::typed(
            OpportunityKind::ShadowQuestion,
            identity,
            FeatureSchemaId::built_in(9),
            RoutingFamilyId::generic(),
        )
    }

    pub(crate) const fn runtime_policy(identity: [u8; 32]) -> Self {
        Self::typed(
            OpportunityKind::RuntimePolicy,
            identity,
            FeatureSchemaId::built_in(8),
            RoutingFamilyId::generic(),
        )
    }

    const fn typed(
        kind: OpportunityKind,
        identity: [u8; 32],
        feature_schema: FeatureSchemaId,
        routing_family: RoutingFamilyId,
    ) -> Self {
        Self {
            kind,
            identity,
            corpus_key: identity,
            feature_schema,
            routing_family,
        }
    }

    #[cfg(test)]
    pub(crate) const fn repair_with_schema(
        identity: [u8; 32],
        feature_schema: FeatureSchemaId,
    ) -> Self {
        Self::repair_in_family(identity, feature_schema, RoutingFamilyId::generic())
    }

    pub(crate) const fn repair_in_family(
        identity: [u8; 32],
        feature_schema: FeatureSchemaId,
        routing_family: RoutingFamilyId,
    ) -> Self {
        Self {
            kind: OpportunityKind::Repair,
            identity,
            corpus_key: identity,
            feature_schema,
            routing_family,
        }
    }

    pub(crate) const fn explore_in_family(
        identity: [u8; 32],
        routing_family: RoutingFamilyId,
    ) -> Self {
        Self::typed(
            OpportunityKind::Emergent,
            identity,
            FeatureSchemaId::built_in(5),
            routing_family,
        )
    }

    #[cfg(test)]
    pub(crate) const fn emergent(identity: [u8; 32]) -> Self {
        Self {
            kind: OpportunityKind::Emergent,
            identity,
            corpus_key: identity,
            feature_schema: FeatureSchemaId::built_in(5),
            routing_family: RoutingFamilyId::generic(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct OpportunityRecord {
    pub(super) specification: OpportunitySpec,
    pub(super) feature_start: u32,
    pub(super) feature_count: u8,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct InvestmentRecord {
    pub(super) specification: InvestmentSpec,
}

#[derive(Debug)]
pub(crate) struct MarketArena {
    limits: IntelligenceLimits,
    pub(super) opportunities: Vec<OpportunityRecord>,
    pub(super) investments: Vec<InvestmentRecord>,
    features: Vec<f32>,
}

impl MarketArena {
    #[cfg(test)]
    pub(crate) fn with_capacity(limits: IntelligenceLimits) -> Self {
        Self::try_with_capacity(limits)
            .expect("validated Intelligence limits must fit the host address space")
    }

    pub(crate) fn try_with_capacity(limits: IntelligenceLimits) -> Result<Self, IntelligenceError> {
        let limits = limits.validated()?;
        let feature_capacity = limits
            .maximum_opportunities
            .checked_mul(MAXIMUM_FEATURES_PER_OPPORTUNITY)
            .ok_or(IntelligenceError::ResourceOverflow)?;
        let mut opportunities = Vec::new();
        opportunities
            .try_reserve_exact(limits.maximum_opportunities)
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        let mut investments = Vec::new();
        investments
            .try_reserve_exact(limits.maximum_investments)
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        let mut features = Vec::new();
        features
            .try_reserve_exact(feature_capacity)
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        Ok(Self {
            limits,
            opportunities,
            investments,
            features,
        })
    }

    pub(crate) fn clear(&mut self) {
        self.opportunities.clear();
        self.investments.clear();
        self.features.clear();
    }

    pub(crate) fn resident_bytes(&self) -> u64 {
        resident_u64(
            std::mem::size_of::<Self>()
                .saturating_add(
                    self.opportunities
                        .capacity()
                        .saturating_mul(std::mem::size_of::<OpportunityRecord>()),
                )
                .saturating_add(
                    self.investments
                        .capacity()
                        .saturating_mul(std::mem::size_of::<InvestmentRecord>()),
                )
                .saturating_add(
                    self.features
                        .capacity()
                        .saturating_mul(std::mem::size_of::<f32>()),
                ),
        )
    }

    #[cfg(test)]
    pub(super) fn capacities(&self) -> (usize, usize, usize) {
        (
            self.opportunities.capacity(),
            self.investments.capacity(),
            self.features.capacity(),
        )
    }

    pub(crate) fn push_opportunity(
        &mut self,
        specification: OpportunitySpec,
        features: &[f32],
    ) -> Result<OpportunityId, IntelligenceError> {
        if self.opportunities.len() == self.limits.maximum_opportunities
            || features.len() > MAXIMUM_FEATURES_PER_OPPORTUNITY
        {
            return Err(IntelligenceError::CapacityExceeded);
        }
        if features.iter().any(|feature| !feature.is_finite()) {
            return Err(IntelligenceError::InvalidFeature);
        }
        let feature_start =
            u32::try_from(self.features.len()).map_err(|_| IntelligenceError::CapacityExceeded)?;
        let feature_count =
            u8::try_from(features.len()).map_err(|_| IntelligenceError::CapacityExceeded)?;
        let index = u32::try_from(self.opportunities.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        self.features.extend_from_slice(features);
        self.opportunities.push(OpportunityRecord {
            specification,
            feature_start,
            feature_count,
        });
        Ok(OpportunityId(index))
    }

    pub(crate) fn push_investment(
        &mut self,
        specification: InvestmentSpec,
    ) -> Result<InvestmentId, IntelligenceError> {
        if self.investments.len() == self.limits.maximum_investments {
            return Err(IntelligenceError::CapacityExceeded);
        }
        let Ok(opportunity_index) = usize::try_from(specification.opportunity.0) else {
            return Err(IntelligenceError::InvalidReference);
        };
        if opportunity_index >= self.opportunities.len() {
            return Err(IntelligenceError::InvalidReference);
        }
        let index = u32::try_from(self.investments.len())
            .map_err(|_| IntelligenceError::CapacityExceeded)?;
        self.investments.push(InvestmentRecord { specification });
        Ok(InvestmentId(index))
    }

    pub(super) fn opportunity(&self, id: OpportunityId) -> Option<&OpportunityRecord> {
        self.opportunities.get(usize::try_from(id.0).ok()?)
    }

    pub(crate) fn investment(&self, id: InvestmentId) -> Option<InvestmentSpec> {
        self.investments
            .get(usize::try_from(id.0).ok()?)
            .map(|record| record.specification)
    }

    pub(super) fn features(&self, record: OpportunityRecord) -> &[f32] {
        let start = record.feature_start as usize;
        let end = start + usize::from(record.feature_count);
        &self.features[start..end]
    }
}

#[derive(Clone, Copy)]
pub(crate) struct MarketFrame<'a> {
    pub(super) market: &'a MarketArena,
    pub(super) allowance: ResourceVector,
    pub(super) epoch: u64,
}

impl<'a> MarketFrame<'a> {
    pub(crate) const fn new(market: &'a MarketArena, allowance: ResourceVector) -> Self {
        Self {
            market,
            allowance,
            epoch: 0,
        }
    }

    pub(crate) const fn at_epoch(mut self, epoch: u64) -> Self {
        self.epoch = epoch;
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Allocation {
    pub(super) investment: InvestmentId,
    pub(super) source: AllocationSource,
    pub(super) bootstrap_priority: u32,
    pub(super) resources: ResourceVector,
    pub(super) forecast: Option<Forecast>,
    pub(super) forecast_start: u32,
    pub(super) forecast_count: u16,
}

impl Allocation {
    pub(crate) const fn investment_id(&self) -> InvestmentId {
        self.investment
    }

    pub(super) const fn bootstrap(
        investment: InvestmentId,
        bootstrap_priority: u32,
        resources: ResourceVector,
    ) -> Self {
        Self {
            investment,
            source: AllocationSource::Bootstrap,
            bootstrap_priority,
            resources,
            forecast: None,
            forecast_start: 0,
            forecast_count: 0,
        }
    }

    pub(super) const fn specialist(
        investment: InvestmentId,
        specialist: SpecialistRevisionId,
        bootstrap_priority: u32,
        resources: ResourceVector,
        forecast: Forecast,
        forecast_start: u32,
        forecast_count: u16,
    ) -> Self {
        Self {
            investment,
            source: AllocationSource::Specialist(specialist),
            bootstrap_priority,
            resources,
            forecast: Some(forecast),
            forecast_start,
            forecast_count,
        }
    }

    pub(crate) const fn source(&self) -> AllocationSource {
        self.source
    }

    pub(crate) const fn bootstrap_priority(&self) -> u32 {
        self.bootstrap_priority
    }

    #[cfg(test)]
    pub(crate) const fn forecast(&self) -> Option<Forecast> {
        self.forecast
    }

    pub(super) fn forecasts<'a>(&self, forecasts: &'a [Forecast]) -> &'a [Forecast] {
        let start = self.forecast_start as usize;
        let end = start + usize::from(self.forecast_count);
        &forecasts[start..end]
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct BidRecord {
    pub(super) investment: InvestmentId,
    pub(super) specialist: SpecialistRevisionId,
    pub(super) forecast_start: u32,
    pub(super) forecast_count: u16,
}

#[derive(Debug)]
pub(crate) struct PortfolioBuffer {
    pub(super) allocations: Vec<Allocation>,
    pub(super) order: Vec<usize>,
    pub(super) bids: Vec<BidRecord>,
    pub(super) forecasts: Vec<Forecast>,
    pub(super) selected: Vec<u8>,
    pub(super) receipts: Vec<InvestmentReceipt>,
    specialists_evaluated: usize,
}

impl PortfolioBuffer {
    #[cfg(test)]
    pub(crate) fn with_capacity(limits: IntelligenceLimits) -> Self {
        Self::try_with_capacity(limits)
            .expect("validated Intelligence limits must fit the host address space")
    }

    pub(crate) fn try_with_capacity(limits: IntelligenceLimits) -> Result<Self, IntelligenceError> {
        let limits = limits.validated()?;
        let routed_bid_capacity = limits.routed_bid_capacity()?;
        let routed_forecast_capacity = limits.routed_forecast_capacity()?;
        Ok(Self {
            allocations: reserved(limits.maximum_investments)?,
            order: reserved(limits.maximum_investments.max(routed_bid_capacity))?,
            bids: reserved(routed_bid_capacity)?,
            forecasts: reserved(routed_forecast_capacity)?,
            selected: reserved(limits.maximum_investments)?,
            receipts: reserved(limits.maximum_investments)?,
            specialists_evaluated: 0,
        })
    }

    pub(crate) fn clear(&mut self) {
        self.allocations.clear();
        self.order.clear();
        self.bids.clear();
        self.forecasts.clear();
        self.selected.clear();
        self.receipts.clear();
        self.specialists_evaluated = 0;
    }

    #[cfg(test)]
    pub(crate) const fn specialists_evaluated(&self) -> usize {
        self.specialists_evaluated
    }

    pub(super) fn record_specialist_evaluation(&mut self) {
        self.specialists_evaluated = self.specialists_evaluated.saturating_add(1);
    }

    pub(crate) fn resident_bytes(&self) -> u64 {
        resident_u64(
            std::mem::size_of::<Self>()
                .saturating_add(
                    self.allocations
                        .capacity()
                        .saturating_mul(std::mem::size_of::<Allocation>()),
                )
                .saturating_add(
                    self.order
                        .capacity()
                        .saturating_mul(std::mem::size_of::<usize>()),
                )
                .saturating_add(
                    self.bids
                        .capacity()
                        .saturating_mul(std::mem::size_of::<BidRecord>()),
                )
                .saturating_add(
                    self.forecasts
                        .capacity()
                        .saturating_mul(std::mem::size_of::<Forecast>()),
                )
                .saturating_add(
                    self.selected
                        .capacity()
                        .saturating_mul(std::mem::size_of::<u8>()),
                )
                .saturating_add(
                    self.receipts
                        .capacity()
                        .saturating_mul(std::mem::size_of::<InvestmentReceipt>()),
                ),
        )
    }

    #[cfg(test)]
    pub(super) fn capacities(&self) -> [usize; 6] {
        [
            self.allocations.capacity(),
            self.order.capacity(),
            self.bids.capacity(),
            self.forecasts.capacity(),
            self.selected.capacity(),
            self.receipts.capacity(),
        ]
    }
}

fn resident_u64(bytes: usize) -> u64 {
    u64::try_from(bytes).unwrap_or(u64::MAX)
}

fn reserved<T>(capacity: usize) -> Result<Vec<T>, IntelligenceError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| IntelligenceError::CapacityExceeded)?;
    Ok(values)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct IntelligenceScratchLayout {
    market_bytes: u64,
    portfolio_bytes: u64,
}

impl IntelligenceScratchLayout {
    #[cfg(test)]
    pub(crate) const fn market_bytes(self) -> u64 {
        self.market_bytes
    }

    #[cfg(test)]
    pub(crate) const fn portfolio_bytes(self) -> u64 {
        self.portfolio_bytes
    }

    pub(crate) const fn total_bytes(self) -> u64 {
        self.market_bytes.saturating_add(self.portfolio_bytes)
    }
}

impl IntelligenceLimits {
    fn routed_bid_capacity(self) -> Result<usize, IntelligenceError> {
        Ok(self
            .maximum_investments
            .checked_mul(self.maximum_specialists_per_route)
            .ok_or(IntelligenceError::ResourceOverflow)?
            .min(self.maximum_bids))
    }

    fn routed_forecast_capacity(self) -> Result<usize, IntelligenceError> {
        Ok(self
            .routed_bid_capacity()?
            .checked_mul(MAXIMUM_MODEL_HEADS)
            .ok_or(IntelligenceError::ResourceOverflow)?
            .min(self.maximum_forecast_cells))
    }

    pub(crate) fn scratch_layout(self) -> Result<IntelligenceScratchLayout, IntelligenceError> {
        let this = self.validated()?;
        let feature_capacity = this
            .maximum_opportunities
            .checked_mul(MAXIMUM_FEATURES_PER_OPPORTUNITY)
            .ok_or(IntelligenceError::ResourceOverflow)?;
        let market_bytes = checked_layout_bytes(
            std::mem::size_of::<MarketArena>(),
            &[
                (
                    this.maximum_opportunities,
                    std::mem::size_of::<OpportunityRecord>(),
                ),
                (
                    this.maximum_investments,
                    std::mem::size_of::<InvestmentRecord>(),
                ),
                (feature_capacity, std::mem::size_of::<f32>()),
            ],
        )?;
        let routed_bid_capacity = this.routed_bid_capacity()?;
        let routed_forecast_capacity = this.routed_forecast_capacity()?;
        let portfolio_bytes = checked_layout_bytes(
            std::mem::size_of::<PortfolioBuffer>(),
            &[
                (this.maximum_investments, std::mem::size_of::<Allocation>()),
                (
                    this.maximum_investments.max(routed_bid_capacity),
                    std::mem::size_of::<usize>(),
                ),
                (routed_bid_capacity, std::mem::size_of::<BidRecord>()),
                (routed_forecast_capacity, std::mem::size_of::<Forecast>()),
                (this.maximum_investments, std::mem::size_of::<u8>()),
                (
                    this.maximum_investments,
                    std::mem::size_of::<InvestmentReceipt>(),
                ),
            ],
        )?;
        Ok(IntelligenceScratchLayout {
            market_bytes,
            portfolio_bytes,
        })
    }
}

fn checked_layout_bytes(
    inline: usize,
    allocations: &[(usize, usize)],
) -> Result<u64, IntelligenceError> {
    let bytes = allocations
        .iter()
        .try_fold(inline, |bytes, (count, width)| {
            bytes
                .checked_add(
                    count
                        .checked_mul(*width)
                        .ok_or(IntelligenceError::ResourceOverflow)?,
                )
                .ok_or(IntelligenceError::ResourceOverflow)
        })?;
    u64::try_from(bytes).map_err(|_| IntelligenceError::ResourceOverflow)
}

#[derive(Clone, Copy)]
pub(crate) struct PortfolioReceipt<'a> {
    pub(super) allocations: &'a [Allocation],
    pub(super) receipts: &'a [InvestmentReceipt],
}

impl<'a> PortfolioReceipt<'a> {
    pub(crate) const fn allocations(&self) -> &'a [Allocation] {
        self.allocations
    }

    pub(crate) const fn receipts(&self) -> &'a [InvestmentReceipt] {
        self.receipts
    }

    #[cfg(test)]
    pub(crate) fn investment_ids(&self) -> impl ExactSizeIterator<Item = InvestmentId> + 'a {
        self.allocations
            .iter()
            .map(|allocation| allocation.investment)
    }

    #[cfg(test)]
    pub(crate) fn resources(&self) -> ResourceVector {
        self.allocations
            .iter()
            .fold(ResourceVector::default(), |total, allocation| {
                total
                    .checked_add(allocation.resources)
                    .expect("an allocated portfolio was checked against a bounded allowance")
            })
    }
}
