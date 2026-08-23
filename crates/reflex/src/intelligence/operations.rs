use std::num::NonZeroU32;

use sha2::{Digest, Sha256};

use super::arena::{MarketArena, MarketFrame, OpportunitySpec, PortfolioBuffer};
use super::causal::{InvestmentOutcome, InvestmentReceipt, InvestmentSettlement};
use super::core::{IntelligenceCore, InvestmentSpec};
use super::types::{
    AllocationSource, IntelligenceError, ResourceVector, RoutingFamilyId, SubjectId,
};

const MAXIMUM_OPERATIONAL_ACTIONS: usize = 9;
const OPERATIONAL_FEATURE_COUNT: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OperationalAction {
    Generate {
        operator: SubjectId,
        applications: NonZeroU32,
        routing_family: RoutingFamilyId,
    },
    Repair {
        rejection: SubjectId,
        parent: SubjectId,
        advisory: SubjectId,
        operator: SubjectId,
        applications: NonZeroU32,
        routing_family: RoutingFamilyId,
    },
    Explore {
        question: SubjectId,
        operator: SubjectId,
        applications: NonZeroU32,
        routing_family: RoutingFamilyId,
    },
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct OperationalActionSpec {
    action: OperationalAction,
    resources: ResourceVector,
    bootstrap_priority: u32,
    features: [f32; OPERATIONAL_FEATURE_COUNT],
}

impl OperationalActionSpec {
    pub(crate) const fn new(
        action: OperationalAction,
        resources: ResourceVector,
        bootstrap_priority: u32,
        features: [f32; OPERATIONAL_FEATURE_COUNT],
    ) -> Self {
        Self {
            action,
            resources,
            bootstrap_priority,
            features,
        }
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct AllocatedOperationalAction {
    action: OperationalAction,
    receipt: InvestmentReceipt,
    source: AllocationSource,
    issued_under: [u8; 32],
}

impl AllocatedOperationalAction {
    pub(crate) const fn action(&self) -> OperationalAction {
        self.action
    }

    pub(crate) const fn decision(&self) -> super::causal::DecisionId {
        self.receipt.decision()
    }

    #[cfg(test)]
    pub(crate) const fn source(&self) -> AllocationSource {
        self.source
    }

    pub(crate) fn settle(
        self,
        intelligence: &IntelligenceCore,
        outcome: super::causal::InvestmentOutcome,
        actual_resources: ResourceVector,
    ) -> Result<(InvestmentReceipt, super::causal::InvestmentSettlement), IntelligenceError> {
        if intelligence.active_revision() != self.issued_under {
            return Err(IntelligenceError::StaleTransition);
        }
        if !actual_resources.fits_within(self.receipt.resources()) {
            return Err(IntelligenceError::CapacityExceeded);
        }
        Ok((
            self.receipt,
            super::causal::InvestmentSettlement::new(
                self.receipt.decision(),
                outcome,
                actual_resources,
            ),
        ))
    }

    pub(crate) fn authorize_shadow(
        self,
        intelligence: &IntelligenceCore,
        specification: SubjectId,
        protected_resources: ResourceVector,
    ) -> Result<(InvestmentReceipt, InvestmentSettlement), IntelligenceError> {
        if intelligence.active_revision() != self.issued_under
            || self.receipt.resources() != protected_resources
            || !matches!(
                self.action,
                OperationalAction::RunShadowCampaign {
                    specification: authorized,
                } if authorized == specification
            )
        {
            return Err(IntelligenceError::StaleTransition);
        }
        Ok((
            self.receipt,
            InvestmentSettlement::new(
                self.receipt.decision(),
                InvestmentOutcome::Completed,
                ResourceVector::new(0, 0, 0, 0, 0),
            ),
        ))
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct OperationalAllocations {
    actions: [Option<AllocatedOperationalAction>; MAXIMUM_OPERATIONAL_ACTIONS],
    count: u8,
}

impl OperationalAllocations {
    pub(crate) const fn len(&self) -> usize {
        self.count as usize
    }

    pub(crate) const fn is_empty(&self) -> bool {
        self.count == 0
    }
}

impl OperationalAllocations {
    pub(crate) fn into_iter(self) -> impl Iterator<Item = AllocatedOperationalAction> {
        self.actions
            .into_iter()
            .take(usize::from(self.count))
            .flatten()
    }
}

#[derive(Debug, PartialEq)]
pub(crate) struct PreparedOperationalMarket {
    actions: [Option<OperationalAction>; MAXIMUM_OPERATIONAL_ACTIONS],
    count: u8,
}

impl PreparedOperationalMarket {
    pub(crate) fn prepare(
        specifications: &[OperationalActionSpec],
        market: &mut MarketArena,
    ) -> Result<Self, IntelligenceError> {
        if specifications.is_empty() || specifications.len() > MAXIMUM_OPERATIONAL_ACTIONS {
            return Err(IntelligenceError::CapacityExceeded);
        }
        market.clear();
        let mut actions = std::array::from_fn(|_| None);
        for (index, specification) in specifications.iter().copied().enumerate() {
            let (opportunity, investment) = action_investment(specification, market)?;
            debug_assert_eq!(usize::try_from(investment.0).ok(), Some(index));
            debug_assert_eq!(usize::try_from(opportunity.0).ok(), Some(index));
            actions[index] = Some(specification.action);
        }
        Ok(Self {
            actions,
            count: u8::try_from(specifications.len())
                .map_err(|_| IntelligenceError::CapacityExceeded)?,
        })
    }

    pub(crate) fn allocate(
        self,
        intelligence: &IntelligenceCore,
        market: &MarketArena,
        allowance: ResourceVector,
        epoch: u64,
        limit: usize,
        portfolio: &mut PortfolioBuffer,
    ) -> Result<OperationalAllocations, IntelligenceError> {
        if limit > usize::from(self.count) {
            return Err(IntelligenceError::CapacityExceeded);
        }
        let portfolio = intelligence.allocate(
            MarketFrame::new(market, allowance).at_epoch(epoch),
            limit,
            portfolio,
        )?;
        let mut actions = std::array::from_fn(|_| None);
        for (output, (allocation, receipt)) in actions
            .iter_mut()
            .zip(portfolio.allocations().iter().zip(portfolio.receipts()))
        {
            let index = usize::try_from(allocation.investment_id().0)
                .map_err(|_| IntelligenceError::InvalidReference)?;
            let action = self
                .actions
                .get(index)
                .copied()
                .flatten()
                .ok_or(IntelligenceError::InvalidReference)?;
            *output = Some(AllocatedOperationalAction {
                action,
                receipt: *receipt,
                source: allocation.source(),
                issued_under: intelligence.active_revision(),
            });
        }
        Ok(OperationalAllocations {
            actions,
            count: u8::try_from(portfolio.allocations().len())
                .map_err(|_| IntelligenceError::CapacityExceeded)?,
        })
    }
}

fn action_investment(
    specification: OperationalActionSpec,
    market: &mut MarketArena,
) -> Result<(super::types::OpportunityId, super::types::InvestmentId), IntelligenceError> {
    let identity = action_identity(specification.action);
    let opportunity = match specification.action {
        OperationalAction::Generate { routing_family, .. } => {
            OpportunitySpec::operator_application(identity, routing_family)
        }
        OperationalAction::Repair { routing_family, .. } => OpportunitySpec::repair_in_family(
            identity,
            super::types::FeatureSchemaId::built_in(3),
            routing_family,
        ),
        OperationalAction::Explore { routing_family, .. } => {
            OpportunitySpec::explore_in_family(identity, routing_family)
        }
        OperationalAction::TrainSpecialist { .. } | OperationalAction::CompareRevision { .. } => {
            OpportunitySpec::specialist_niche(identity)
        }
        OperationalAction::Consolidate { .. } => OpportunitySpec::knowledge_pattern(identity),
        OperationalAction::RunShadowCampaign { specification } => {
            OpportunitySpec::shadow_question(specification.identity())
        }
        OperationalAction::ProposeRuntimePolicy { .. } => OpportunitySpec::runtime_policy(identity),
    };
    let opportunity = market.push_opportunity(opportunity, &specification.features)?;
    let investment = match specification.action {
        OperationalAction::Generate {
            operator,
            applications,
            ..
        } => InvestmentSpec::generate(
            opportunity,
            operator,
            applications,
            specification.resources,
            specification.bootstrap_priority,
        ),
        OperationalAction::Repair {
            rejection,
            operator,
            ..
        } => InvestmentSpec::repair(
            opportunity,
            rejection,
            operator,
            specification.resources,
            specification.bootstrap_priority,
        ),
        OperationalAction::Explore { .. } => InvestmentSpec::explore(
            opportunity,
            specification.resources,
            specification.bootstrap_priority,
        ),
        OperationalAction::TrainSpecialist { mandate } => InvestmentSpec::train_specialist(
            opportunity,
            mandate,
            specification.resources,
            specification.bootstrap_priority,
        ),
        OperationalAction::CompareRevision { challenger } => InvestmentSpec::compare_revision(
            opportunity,
            challenger,
            specification.resources,
            specification.bootstrap_priority,
        ),
        OperationalAction::Consolidate { compiler } => InvestmentSpec::consolidate(
            opportunity,
            compiler,
            specification.resources,
            specification.bootstrap_priority,
        ),
        OperationalAction::RunShadowCampaign {
            specification: shadow,
        } => InvestmentSpec::run_shadow_campaign(
            opportunity,
            shadow,
            specification.resources,
            specification.bootstrap_priority,
        ),
        OperationalAction::ProposeRuntimePolicy { parent } => {
            InvestmentSpec::propose_runtime_policy(
                opportunity,
                parent,
                specification.resources,
                specification.bootstrap_priority,
            )
        }
    };
    let investment = market.push_investment(investment)?;
    Ok((opportunity, investment))
}

fn action_identity(action: OperationalAction) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-operational-action-v1\0");
    match action {
        OperationalAction::Generate {
            operator,
            applications,
            routing_family,
        } => {
            digest.update([1]);
            digest.update(operator.identity());
            digest.update(applications.get().to_le_bytes());
            digest.update(routing_family.0);
        }
        OperationalAction::Repair {
            rejection,
            parent,
            advisory,
            operator,
            applications,
            routing_family,
        } => {
            digest.update([2]);
            digest.update(rejection.identity());
            digest.update(parent.identity());
            digest.update(advisory.identity());
            digest.update(operator.identity());
            digest.update(applications.get().to_le_bytes());
            digest.update(routing_family.0);
        }
        OperationalAction::Explore {
            question,
            operator,
            applications,
            routing_family,
        } => {
            digest.update([3]);
            digest.update(question.identity());
            digest.update(operator.identity());
            digest.update(applications.get().to_le_bytes());
            digest.update(routing_family.0);
        }
        OperationalAction::TrainSpecialist { mandate } => {
            digest.update([4]);
            digest.update(mandate.identity());
        }
        OperationalAction::CompareRevision { challenger } => {
            digest.update([5]);
            digest.update(challenger.identity());
        }
        OperationalAction::Consolidate { compiler } => {
            digest.update([6]);
            digest.update(compiler.identity());
        }
        OperationalAction::RunShadowCampaign { specification } => {
            digest.update([7]);
            digest.update(specification.identity());
        }
        OperationalAction::ProposeRuntimePolicy { parent } => {
            digest.update([8]);
            digest.update(parent.identity());
        }
    }
    digest.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intelligence::{
        CompactModel, EcologyEdit, ForecastAxis, IntelligenceLimits, LinearHead, RoleId,
        SettlementFrame, SpecialistMandate, SpecialistRevision,
    };

    #[test]
    fn specialist_changes_the_discretionary_generation_lane_under_fixed_budget() {
        let limits = IntelligenceLimits::new(8, 8, 16, 64, 4, 64 * 1024).unwrap();
        let actions = generation_actions();
        let mut market = MarketArena::with_capacity(limits);
        let prepared = PreparedOperationalMarket::prepare(&actions, &mut market).unwrap();
        let allowance = ResourceVector::new(3, 3, 0, 3, 0);
        let mut portfolio = PortfolioBuffer::with_capacity(limits);
        let bootstrap = prepared
            .allocate(
                &IntelligenceCore::fresh(limits),
                &market,
                allowance,
                1,
                2,
                &mut portfolio,
            )
            .unwrap();
        let bootstrap_actions = bootstrap
            .into_iter()
            .map(|action| action.action())
            .collect::<Vec<_>>();

        let routed_family = RoutingFamilyId::new([3; 32]);
        let specialist = SpecialistRevision::new(
            SpecialistMandate::for_routing_families(
                RoleId::new("generation-lane-specialist").unwrap(),
                [ForecastAxis::ImmediateImprovement],
                [super::super::arena::OpportunityKind::OperatorApplication],
                [super::super::types::FeatureSchemaId::built_in(2)],
                [routed_family],
            )
            .unwrap(),
            CompactModel::linear(
                OPERATIONAL_FEATURE_COUNT,
                [LinearHead::new(
                    ForecastAxis::ImmediateImprovement,
                    0.0,
                    [1.0; OPERATIONAL_FEATURE_COUNT],
                    0.1,
                    32,
                )
                .unwrap()],
            )
            .unwrap(),
        )
        .unwrap();
        let mut learned = IntelligenceCore::fresh(limits);
        learned
            .stage(SettlementFrame::edits(&[EcologyEdit::Spawn(specialist)]))
            .unwrap()
            .commit(&mut learned)
            .unwrap();
        let prepared = PreparedOperationalMarket::prepare(&actions, &mut market).unwrap();
        let learned_actions = prepared
            .allocate(&learned, &market, allowance, 1, 2, &mut portfolio)
            .unwrap();
        let learned_actions = learned_actions
            .into_iter()
            .map(|action| (action.action(), action.source()))
            .collect::<Vec<_>>();

        assert_eq!(
            bootstrap_actions,
            actions[..2]
                .iter()
                .map(|spec| spec.action)
                .collect::<Vec<_>>()
        );
        assert_eq!(learned_actions[0].0, actions[0].action);
        assert_eq!(learned_actions[1].0, actions[2].action);
        assert_eq!(
            learned_actions
                .iter()
                .filter(|(_, source)| matches!(source, AllocationSource::Specialist(_)))
                .count(),
            1
        );
    }

    #[test]
    fn action_tokens_are_revision_bound_and_duplicate_settlement_is_rejected() {
        let limits = IntelligenceLimits::new(4, 4, 8, 32, 4, 64 * 1024).unwrap();
        let actions = generation_actions();
        let mut market = MarketArena::with_capacity(limits);
        let prepared = PreparedOperationalMarket::prepare(&actions[..1], &mut market).unwrap();
        let allowance = ResourceVector::new(1, 1, 0, 1, 0);
        let mut portfolio = PortfolioBuffer::with_capacity(limits);
        let core = IntelligenceCore::fresh(limits);
        let token = prepared
            .allocate(&core, &market, allowance, 7, 1, &mut portfolio)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let mut advanced = core.clone();
        advanced
            .stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
                crate::intelligence::trainer::broad_test_specialist(
                    super::super::types::FeatureSchemaId::built_in(2),
                ),
            )]))
            .unwrap()
            .commit(&mut advanced)
            .unwrap();
        assert_eq!(
            token
                .settle(
                    &advanced,
                    super::super::causal::InvestmentOutcome::Completed,
                    ResourceVector::new(1, 1, 0, 1, 0),
                )
                .unwrap_err(),
            IntelligenceError::StaleTransition
        );

        let prepared = PreparedOperationalMarket::prepare(&actions[..1], &mut market).unwrap();
        let token = prepared
            .allocate(&core, &market, allowance, 7, 1, &mut portfolio)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        let (receipt, settlement) = token
            .settle(
                &core,
                super::super::causal::InvestmentOutcome::Completed,
                ResourceVector::new(1, 1, 0, 1, 0),
            )
            .unwrap();
        assert_eq!(
            core.stage(SettlementFrame::observations(
                &[receipt, receipt],
                &[settlement, settlement],
                &[],
                &[],
            ))
            .unwrap_err(),
            IntelligenceError::DuplicateExperience
        );

        let prepared = PreparedOperationalMarket::prepare(&actions[..1], &mut market).unwrap();
        let token = prepared
            .allocate(&core, &market, allowance, 8, 1, &mut portfolio)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            token
                .settle(
                    &core,
                    super::super::causal::InvestmentOutcome::Completed,
                    ResourceVector::new(2, 1, 0, 1, 0),
                )
                .unwrap_err(),
            IntelligenceError::CapacityExceeded
        );
    }

    #[test]
    fn shadow_authorization_consumes_the_allocation_and_binds_subject_and_envelope() {
        let limits = IntelligenceLimits::new(4, 4, 8, 32, 4, 64 * 1024).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let subject = SubjectId::new([31; 32]);
        let resources = ResourceVector::new(20, 30, 0, 40, 2);
        let specifications = [OperationalActionSpec::new(
            OperationalAction::RunShadowCampaign {
                specification: subject,
            },
            resources,
            0,
            [0.0; OPERATIONAL_FEATURE_COUNT],
        )];
        let mut market = MarketArena::with_capacity(limits);
        let mut portfolio = PortfolioBuffer::with_capacity(limits);
        let authorization = PreparedOperationalMarket::prepare(&specifications, &mut market)
            .unwrap()
            .allocate(&core, &market, resources, 1, 1, &mut portfolio)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            authorization
                .authorize_shadow(&core, SubjectId::new([32; 32]), resources)
                .unwrap_err(),
            IntelligenceError::StaleTransition
        );

        let authorization = PreparedOperationalMarket::prepare(&specifications, &mut market)
            .unwrap()
            .allocate(&core, &market, resources, 2, 1, &mut portfolio)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(
            authorization.receipt.opportunity_identity(),
            subject.identity()
        );
        authorization
            .authorize_shadow(&core, subject, resources)
            .unwrap();
        assert!(core.experience().receipts().is_empty());
    }

    #[test]
    fn completed_operational_action_matrix_is_restart_exact() {
        let limits = IntelligenceLimits::new(8, 8, 16, 64, 8, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let specifications = operational_action_matrix();
        let mut market = MarketArena::with_capacity(limits);
        let mut portfolio = PortfolioBuffer::with_capacity(limits);
        let resources = ResourceVector::new(1, 1, 0, 1, 0);
        let prepared = PreparedOperationalMarket::prepare(&specifications, &mut market).unwrap();
        let allocations = prepared
            .allocate(
                &core,
                &market,
                ResourceVector::new(7, 7, 0, 7, 0),
                19,
                specifications.len(),
                &mut portfolio,
            )
            .unwrap();
        let (receipts, settlements): (Vec<_>, Vec<_>) = allocations
            .into_iter()
            .map(|allocation| {
                allocation
                    .settle(
                        &core,
                        super::super::causal::InvestmentOutcome::Completed,
                        resources,
                    )
                    .unwrap()
            })
            .unzip();
        core.stage(SettlementFrame::observations(
            &receipts,
            &settlements,
            &[],
            &[],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();

        let restored = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        assert_eq!(restored.checkpoint(), core.checkpoint());
        let mut restored_tags = restored
            .experience()
            .receipts()
            .iter()
            .copied()
            .map(super::super::causal::InvestmentReceipt::tag)
            .collect::<Vec<_>>();
        restored_tags.sort_unstable();
        assert_eq!(restored_tags, ordinary_action_tags());
    }

    fn generation_actions() -> [OperationalActionSpec; 3] {
        std::array::from_fn(|index| {
            let identity = u8::try_from(index + 1).unwrap();
            OperationalActionSpec::new(
                OperationalAction::Generate {
                    operator: SubjectId::new([identity; 32]),
                    applications: NonZeroU32::new(1).unwrap(),
                    routing_family: RoutingFamilyId::new([identity; 32]),
                },
                ResourceVector::new(1, 1, 0, 1, 0),
                u32::try_from(index).unwrap(),
                [f32::from(identity); OPERATIONAL_FEATURE_COUNT],
            )
        })
    }

    fn operational_action_matrix() -> [OperationalActionSpec; 7] {
        let subject = |value| SubjectId::new([value; 32]);
        let family = |value| RoutingFamilyId::new([value; 32]);
        let applications = NonZeroU32::new(1).unwrap();
        let resources = ResourceVector::new(1, 1, 0, 1, 0);
        [
            OperationalAction::Generate {
                operator: subject(1),
                applications,
                routing_family: family(1),
            },
            OperationalAction::Repair {
                rejection: subject(2),
                parent: subject(3),
                advisory: subject(4),
                operator: subject(5),
                applications,
                routing_family: family(2),
            },
            OperationalAction::Explore {
                question: subject(6),
                operator: subject(7),
                applications,
                routing_family: family(3),
            },
            OperationalAction::TrainSpecialist {
                mandate: subject(8),
            },
            OperationalAction::CompareRevision {
                challenger: subject(9),
            },
            OperationalAction::Consolidate {
                compiler: subject(10),
            },
            OperationalAction::ProposeRuntimePolicy {
                parent: subject(11),
            },
        ]
        .map(|action| OperationalActionSpec::new(action, resources, 0, [0.0; 8]))
    }

    fn ordinary_action_tags() -> Vec<super::super::core::InvestmentTag> {
        use super::super::core::InvestmentTag;

        vec![
            InvestmentTag::Generate,
            InvestmentTag::Repair,
            InvestmentTag::Explore,
            InvestmentTag::TrainSpecialist,
            InvestmentTag::CompareRevision,
            InvestmentTag::Consolidate,
            InvestmentTag::ProposeRuntimePolicy,
        ]
    }
}
