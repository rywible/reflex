//! Private autonomous intelligence for Runtime allocation and learning.
mod arena;
mod causal;
mod codec;
mod core;
mod ecology;
mod forecast;
mod knowledge;
mod model;
mod operations;
mod routing;
mod trainer;
mod types;

#[cfg(test)]
use arena::{Allocation, OpportunityKind};
pub(crate) use arena::{MarketArena, MarketFrame, OpportunitySpec, PortfolioBuffer};
pub(crate) use causal::{
    CausalSubject, CheckpointDigest, ConsequenceEdge, ConsequenceKind, DecisionId,
    InvestmentOutcome, InvestmentReceipt, InvestmentSettlement, ShadowArm, ShadowArmOutcome,
    ShadowCampaignId, ShadowCampaignLifecycle, ShadowCampaignSpec, ShadowInvalidationReason,
    ShadowUpdate, TypedOutcome,
};
#[cfg(test)]
use core::InvestmentTag;
pub(crate) use core::{
    IntelligenceCore, IntelligenceTransition, InvestmentSpec, KnowledgeCompilationError,
    KnowledgeCompilationShape, PreparedNativeEcologyPlan, ProposedIntelligenceView,
    SettlementFrame,
};
#[cfg(test)]
use ecology::{EcologyEdit, SpecialistLifecycle, SpecialistMandate, SpecialistRevision};
pub(crate) use forecast::ForecastAxis;
#[cfg(test)]
pub(crate) use knowledge::KnowledgeChallengerId;
#[cfg(test)]
pub(crate) use knowledge::{
    CompilerRecipe, ContextualRequirement, KnowledgeChallenger, KnowledgeInvalidationReason,
    KnowledgeProduct, KnowledgeProductKind, KnowledgeProductMeaning, KnowledgeReview,
    KnowledgeStatus, KnowledgeUpdate, PromotionGate,
};
pub(crate) use knowledge::{
    KnowledgeObligationWork, KnowledgePlanningFailure, KnowledgeRecoveryObligation,
    KnowledgeShadowArmReport, KnowledgeShadowReport, KnowledgeShadowRootFact,
    KnowledgeShadowSupportFact, KnowledgeVerificationReport, KnowledgeWork, OpenedKnowledgeWork,
};
#[cfg(test)]
use model::{CompactModel, LinearHead, PriorHead};
pub(crate) use operations::{OperationalAction, OperationalActionSpec, PreparedOperationalMarket};
pub(crate) use trainer::NativeTrainingBudget;
#[cfg(test)]
pub(crate) use types::SpecialistRevisionId;
pub(crate) use types::{
    AllocationSource, IntelligenceError, IntelligenceLimits, ResourceVector, RoutingFamilyId,
    SubjectId,
};
#[cfg(test)]
use types::{FeatureSchemaId, InvestmentId, RoleId};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{
        OperationalEvidence, PolicyUpdate, RuntimePolicyDecision, RuntimePolicyRevision,
        RuntimePolicyState,
    };

    fn bound_policy_comparison(
        core: &IntelligenceCore,
        challenger: RuntimePolicyRevision,
        incumbent: OperationalEvidence,
        treatment: OperationalEvidence,
    ) -> (Vec<ShadowUpdate>, PolicyUpdate) {
        let resources = |evidence: OperationalEvidence| {
            let (cpu, requests, durable) = evidence.resources();
            ResourceVector::new(cpu, 1, durable, 1, requests)
        };
        let incumbent_resources = resources(incumbent);
        let treatment_resources = resources(treatment);
        let allowance = ResourceVector::new(
            incumbent_resources
                .cpu_time_ns
                .max(treatment_resources.cpu_time_ns),
            1,
            incumbent_resources
                .durable_bytes
                .max(treatment_resources.durable_bytes),
            1,
            incumbent_resources
                .verification_requests
                .max(treatment_resources.verification_requests),
        );
        let axes = [
            ForecastAxis::KernelAcceptance,
            ForecastAxis::ImmediateImprovement,
            ForecastAxis::UsefulDescendants,
            ForecastAxis::CrossGoalLeverage,
        ];
        let outcomes = |evidence: OperationalEvidence| {
            [
                evidence.correctness_failures(),
                evidence.verified_discoveries(),
                evidence.useful_descendants(),
                evidence.covered_claims(),
            ]
            .into_iter()
            .zip(axes)
            .map(|(value, axis)| {
                TypedOutcome::new(axis, f32::from(u16::try_from(value).unwrap())).unwrap()
            })
            .collect::<Vec<_>>()
        };
        let campaign = ShadowCampaignSpec::new(
            core.checkpoint().digest(),
            SubjectId::new(challenger.identity()),
            allowance,
            SubjectId::new(challenger.identity()),
            axes,
        )
        .unwrap();
        let updates = vec![
            ShadowUpdate::Open(campaign.clone()),
            ShadowUpdate::Outcome(
                ShadowArmOutcome::new(
                    campaign.id(),
                    ShadowArm::Treatment,
                    campaign.checkpoint(),
                    treatment_resources,
                    campaign.random_stream(),
                    outcomes(treatment),
                )
                .unwrap(),
            ),
            ShadowUpdate::Outcome(
                ShadowArmOutcome::new(
                    campaign.id(),
                    ShadowArm::Control,
                    campaign.checkpoint(),
                    incumbent_resources,
                    campaign.random_stream(),
                    outcomes(incumbent),
                )
                .unwrap(),
            ),
        ];
        let update = PolicyUpdate::comparison(campaign.id(), challenger);
        (updates, update)
    }

    #[test]
    fn fresh_core_allocates_the_bootstrap_prefix_without_mutating_itself() {
        let limits = IntelligenceLimits::new(8, 32, 32, 128, 8, 64 * 1024).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let before = core.checkpoint();
        let mut market = MarketArena::with_capacity(limits);
        let opportunity = market
            .push_opportunity(OpportunitySpec::candidate([1; 32]), &[1.0, 0.5])
            .unwrap();
        for priority in [30, 10, 20] {
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(priority, 1, 0, 1, 1),
                    u32::try_from(priority).unwrap(),
                ))
                .unwrap();
        }
        let frame = MarketFrame::new(&market, ResourceVector::new(100, 100, 100, 100, 100));
        let mut output = PortfolioBuffer::with_capacity(limits);

        let receipt = core.allocate(frame, 2, &mut output).unwrap();

        assert_eq!(receipt.allocations().len(), 2);
        assert!(
            receipt
                .allocations()
                .iter()
                .all(|allocation| allocation.source() == AllocationSource::Bootstrap)
        );
        assert_eq!(
            receipt
                .allocations()
                .iter()
                .map(Allocation::bootstrap_priority)
                .collect::<Vec<_>>(),
            vec![10, 20]
        );
        assert_eq!(
            receipt.investment_ids().collect::<Vec<_>>(),
            vec![InvestmentId(1), InvestmentId(2)]
        );
        assert_eq!(core.checkpoint(), before, "allocation is side-effect-free");
    }

    #[test]
    fn verification_allocation_rejects_heavy_bytes_and_receipts_preserve_declared_demand() {
        let limits = IntelligenceLimits::new(4, 4, 4, 32, 4, 16 * 1024).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let mut market = MarketArena::with_capacity(limits);
        let heavy_opportunity = market
            .push_opportunity(OpportunitySpec::candidate([1; 32]), &[1.0])
            .unwrap();
        let small_opportunity = market
            .push_opportunity(OpportunitySpec::candidate([2; 32]), &[0.5])
            .unwrap();
        market
            .push_investment(InvestmentSpec::verify_candidate(
                heavy_opportunity,
                4_096,
                2_048,
                0,
            ))
            .unwrap();
        market
            .push_investment(InvestmentSpec::verify_candidate(
                small_opportunity,
                64,
                32,
                1,
            ))
            .unwrap();
        let mut output = PortfolioBuffer::with_capacity(limits);

        let portfolio = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(11, 64, 32, 13, 1)),
                2,
                &mut output,
            )
            .unwrap();

        assert_eq!(
            portfolio.investment_ids().collect::<Vec<_>>(),
            vec![InvestmentId(1)]
        );
        assert_eq!(portfolio.resources(), ResourceVector::new(0, 64, 32, 0, 1));
        assert_eq!(
            portfolio.receipts()[0].resources(),
            ResourceVector::new(0, 64, 32, 0, 1)
        );
        let forecast = market.investment(InvestmentId(1)).unwrap().resources;
        assert!(forecast.cpu_time_is_unknown());
        assert!(forecast.elapsed_time_is_unknown());
    }

    #[test]
    fn market_and_portfolio_storage_reset_without_losing_allocations() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 16 * 1024).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let before = core.checkpoint();
        let mut market = MarketArena::try_with_capacity(limits).unwrap();
        let mut output = PortfolioBuffer::try_with_capacity(limits).unwrap();
        let scratch = limits.scratch_layout().unwrap();
        assert_eq!(market.resident_bytes(), scratch.market_bytes());
        assert_eq!(output.resident_bytes(), scratch.portfolio_bytes());
        assert_eq!(
            scratch.total_bytes(),
            market.resident_bytes() + output.resident_bytes()
        );
        let build_market = |market: &mut MarketArena| {
            let opportunity = market
                .push_opportunity(OpportunitySpec::candidate([41; 32]), &[0.5, 1.0])
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(3, 5, 0, 7, 1),
                    2,
                ))
                .unwrap();
        };
        build_market(&mut market);
        let first = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(3, 5, 0, 7, 1)).at_epoch(9),
                1,
                &mut output,
            )
            .unwrap();
        let first_allocations = first.allocations().to_vec();
        let first_receipts = first.receipts().to_vec();
        let market_capacities = market.capacities();
        let portfolio_capacities = output.capacities();
        let market_resident: u64 = market.resident_bytes();
        let portfolio_resident: u64 = output.resident_bytes();

        market.clear();
        output.clear();
        assert_eq!(market.capacities(), market_capacities);
        assert_eq!(output.capacities(), portfolio_capacities);
        assert_eq!(market.resident_bytes(), market_resident);
        assert_eq!(output.resident_bytes(), portfolio_resident);
        build_market(&mut market);
        let second = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(3, 5, 0, 7, 1)).at_epoch(9),
                1,
                &mut output,
            )
            .unwrap();

        assert_eq!(second.allocations(), first_allocations);
        assert_eq!(second.receipts(), first_receipts);
        assert_eq!(core.checkpoint(), before);
    }

    #[test]
    fn specialist_wins_an_adaptive_slot_without_erasing_bootstrap_protection() {
        let limits = IntelligenceLimits::new(8, 32, 32, 128, 8, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let specialist = SpecialistRevision::new(
            SpecialistMandate::all(
                RoleId::new("candidate-ranker").unwrap(),
                [ForecastAxis::ImmediateImprovement],
            )
            .unwrap(),
            CompactModel::linear(
                1,
                [
                    LinearHead::new(ForecastAxis::ImmediateImprovement, 0.0, [4.0], 0.05, 32)
                        .unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        let edits = [EcologyEdit::Spawn(specialist)];
        let transition = core.stage(SettlementFrame::edits(&edits)).unwrap();
        let incumbent = core.checkpoint();
        assert_ne!(transition.checkpoint(), incumbent);
        assert_eq!(
            core.checkpoint(),
            incumbent,
            "staging cannot mutate the core"
        );
        transition.commit(&mut core).unwrap();

        let mut market = MarketArena::with_capacity(limits);
        for (identity, feature, bootstrap_priority) in [(1, 0.0, 1), (2, 0.5, 2), (3, 1.0, 3)] {
            let opportunity = market
                .push_opportunity(OpportunitySpec::candidate([identity; 32]), &[feature])
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    bootstrap_priority,
                ))
                .unwrap();
        }
        let frame = MarketFrame::new(&market, ResourceVector::new(3, 3, 0, 3, 3));
        let mut output = PortfolioBuffer::with_capacity(limits);

        let receipt = core.allocate(frame, 2, &mut output).unwrap();

        assert_eq!(receipt.allocations().len(), 2);
        assert_eq!(
            receipt.allocations()[0].source(),
            AllocationSource::Bootstrap
        );
        assert_eq!(receipt.allocations()[0].bootstrap_priority(), 1);
        assert!(matches!(
            receipt.allocations()[1].source(),
            AllocationSource::Specialist(_)
        ));
        assert_eq!(receipt.allocations()[1].bootstrap_priority(), 3);
        assert_eq!(
            receipt.allocations()[1].forecast().unwrap().axis(),
            ForecastAxis::ImmediateImprovement
        );
    }

    #[test]
    fn prepared_commit_exposes_the_exact_proposed_core_before_infallible_swap() {
        let limits = IntelligenceLimits::new(8, 32, 32, 128, 8, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let specialist = SpecialistRevision::new(
            SpecialistMandate::all(
                RoleId::new("prepared-commit").unwrap(),
                [ForecastAxis::ImmediateImprovement],
            )
            .unwrap(),
            CompactModel::linear(
                1,
                [
                    LinearHead::new(ForecastAxis::ImmediateImprovement, 0.0, [1.0], 0.05, 32)
                        .unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        let before = core.checkpoint();
        let transition = core
            .stage(SettlementFrame::edits(&[EcologyEdit::Spawn(specialist)]))
            .unwrap();
        let expected = transition.checkpoint();
        let restored = IntelligenceCore::restore(expected.as_bytes()).unwrap();

        let prepared = transition.prepare(&mut core).unwrap();
        let proposed = prepared.proposed(&core);
        assert_eq!(
            core.checkpoint(),
            before,
            "preparation cannot publish state"
        );
        assert_eq!(proposed.checkpoint_bytes(), expected.as_bytes());
        assert_eq!(proposed.revision(), restored.active_revision());
        assert_ne!(
            expected.identity(),
            restored.active_revision(),
            "Bundle checkpoint identity and Core active revision are distinct authorities"
        );
        assert_eq!(
            proposed.model_ecology_identity(),
            restored.model_ecology_identity()
        );

        prepared.commit(&mut core);
        assert_eq!(core.checkpoint(), expected);
        assert_eq!(core.active_revision(), restored.active_revision());
    }

    #[test]
    fn caller_preference_precedes_specialist_forecasts_and_survives_recovery() {
        let limits = IntelligenceLimits::new(8, 32, 32, 128, 8, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
            test_specialist("preference-authority", 4.0),
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        let mut market = MarketArena::with_capacity(limits);
        for (identity, feature, preference, bootstrap) in
            [(1, 0.0, 99, 0), (2, -1.0, 0, 1), (3, 1.0, 1, 2)]
        {
            let opportunity = market
                .push_opportunity(OpportunitySpec::candidate([identity; 32]), &[feature])
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify_candidate_with_preference(
                    opportunity,
                    1,
                    0,
                    preference,
                    bootstrap,
                ))
                .unwrap();
        }
        let mut output = PortfolioBuffer::with_capacity(limits);
        let portfolio = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(2, 2, 0, 2, 2)),
                2,
                &mut output,
            )
            .unwrap();
        assert_eq!(portfolio.allocations()[0].bootstrap_priority(), 0);
        assert_eq!(portfolio.allocations()[1].bootstrap_priority(), 1);
        assert_eq!(portfolio.receipts()[1].preference_priority(), 0);
        let receipt = portfolio.receipts()[1];
        let settlement = InvestmentSettlement::new(
            receipt.decision(),
            InvestmentOutcome::VerifiedAccepted {
                verification_record: SubjectId::new([2; 32]),
            },
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

        let restored = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        assert_eq!(restored.experience().receipts()[0].preference_priority(), 0);
    }

    #[test]
    fn allocator_uses_the_complete_typed_forecast_vector_after_bootstrap() {
        let limits = IntelligenceLimits::new(8, 16, 32, 128, 4, 16 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let specialist = SpecialistRevision::new(
            SpecialistMandate::all(
                RoleId::new("multi-head-candidate-ranker").unwrap(),
                [
                    ForecastAxis::ImmediateImprovement,
                    ForecastAxis::UsefulDescendants,
                ],
            )
            .unwrap(),
            CompactModel::linear(
                1,
                [
                    LinearHead::new(ForecastAxis::ImmediateImprovement, 0.0, [0.0], 0.01, 100)
                        .unwrap(),
                    LinearHead::new(ForecastAxis::UsefulDescendants, 0.0, [4.0], 0.01, 100)
                        .unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(specialist)]))
            .unwrap()
            .commit(&mut core)
            .unwrap();

        let mut market = MarketArena::with_capacity(limits);
        for (identity, feature, priority) in [(1, 0.0, 0), (2, -1.0, 1), (3, 1.0, 2)] {
            let opportunity = market
                .push_opportunity(OpportunitySpec::candidate([identity; 32]), &[feature])
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    priority,
                ))
                .unwrap();
        }
        let mut output = PortfolioBuffer::with_capacity(limits);
        let portfolio = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(2, 2, 0, 2, 2)),
                2,
                &mut output,
            )
            .unwrap();

        assert_eq!(
            portfolio.allocations()[0].source(),
            AllocationSource::Bootstrap
        );
        assert_eq!(portfolio.allocations()[0].bootstrap_priority(), 0);
        assert_eq!(portfolio.allocations()[1].bootstrap_priority(), 2);
        assert!(matches!(
            portfolio.allocations()[1].source(),
            AllocationSource::Specialist(_)
        ));

        let specialist_receipt = portfolio.receipts()[1];
        assert_eq!(
            specialist_receipt
                .forecasts()
                .iter()
                .map(|forecast| forecast.axis())
                .collect::<Vec<_>>(),
            [
                ForecastAxis::ImmediateImprovement,
                ForecastAxis::UsefulDescendants,
            ]
        );
        let settlement = InvestmentSettlement::new(
            specialist_receipt.decision(),
            InvestmentOutcome::VerifiedAccepted {
                verification_record: SubjectId::new([3; 32]),
            },
            ResourceVector::new(1, 1, 0, 1, 1),
        );
        core.stage(SettlementFrame::observations(
            &[specialist_receipt],
            &[settlement],
            &[],
            &[],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();

        let checkpoint = core.checkpoint();
        assert_eq!(&checkpoint.as_bytes()[..5], b"RFIC\x11");
        let restored = IntelligenceCore::restore(checkpoint.as_bytes()).unwrap();
        assert_eq!(restored.checkpoint(), checkpoint);
        assert_eq!(restored.experience().receipts(), &[specialist_receipt]);
        assert_eq!(restored.experience().settlements(), &[settlement]);
    }

    #[test]
    fn specialist_mandates_isolate_opportunity_kind_schema_and_feature_width() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 32 * 1024).unwrap();
        let candidate_schema = FeatureSchemaId::new([51; 32]);
        let unrelated_schema = FeatureSchemaId::new([52; 32]);
        let specialist = SpecialistRevision::new(
            SpecialistMandate::for_opportunities(
                RoleId::new("candidate-v51-ranker").unwrap(),
                [ForecastAxis::ImmediateImprovement],
                [OpportunityKind::Candidate],
                [candidate_schema],
            )
            .unwrap(),
            CompactModel::linear(
                2,
                [
                    LinearHead::new(ForecastAxis::ImmediateImprovement, 0.0, [1.0, 2.0], 0.1, 16)
                        .unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(specialist)]))
            .unwrap()
            .commit(&mut core)
            .unwrap();
        let mut market = MarketArena::with_capacity(limits);
        for (specification, features, priority) in [
            (
                OpportunitySpec::repair_with_schema([53; 32], candidate_schema),
                &[1.0, 1.0][..],
                1,
            ),
            (
                OpportunitySpec::candidate_with_schema([54; 32], unrelated_schema),
                &[1.0, 1.0][..],
                2,
            ),
            (
                OpportunitySpec::candidate_with_schema([55; 32], candidate_schema),
                &[1.0, 1.0][..],
                3,
            ),
            (
                OpportunitySpec::candidate_with_schema([56; 32], candidate_schema),
                &[1.0][..],
                4,
            ),
        ] {
            let opportunity = market.push_opportunity(specification, features).unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    priority,
                ))
                .unwrap();
        }
        let mut output = PortfolioBuffer::with_capacity(limits);
        let portfolio = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(4, 4, 0, 4, 4)),
                4,
                &mut output,
            )
            .unwrap();
        assert_eq!(portfolio.allocations()[0].bootstrap_priority(), 1);
        assert_eq!(
            portfolio.allocations()[0].source(),
            AllocationSource::Bootstrap
        );
        assert_eq!(portfolio.allocations()[1].bootstrap_priority(), 3);
        assert!(matches!(
            portfolio.allocations()[1].source(),
            AllocationSource::Specialist(_)
        ));
        assert!(
            portfolio.allocations()[2..]
                .iter()
                .all(|allocation| allocation.source() == AllocationSource::Bootstrap)
        );

        let recovered = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        let mut recovered_output = PortfolioBuffer::with_capacity(limits);
        let recovered_portfolio = recovered
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(4, 4, 0, 4, 4)),
                4,
                &mut recovered_output,
            )
            .unwrap();
        assert_eq!(recovered_portfolio.allocations(), portfolio.allocations());
    }

    #[test]
    fn routing_families_isolate_coexisting_specialists_with_one_feature_schema() {
        let limits = IntelligenceLimits::new(8, 16, 32, 128, 8, 32 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let schema = FeatureSchemaId::new([61; 32]);
        let family_a = RoutingFamilyId::new([62; 32]);
        let family_b = RoutingFamilyId::new([63; 32]);
        let make_specialist = |role, family, estimate| {
            SpecialistRevision::new(
                SpecialistMandate::for_routing_families(
                    RoleId::new(role).unwrap(),
                    [ForecastAxis::ImmediateImprovement],
                    [OpportunityKind::Candidate],
                    [schema],
                    [family],
                )
                .unwrap(),
                CompactModel::prior([PriorHead::new(
                    ForecastAxis::ImmediateImprovement,
                    estimate,
                    0.01,
                    100,
                )
                .unwrap()])
                .unwrap(),
            )
            .unwrap()
        };
        let specialist_a = make_specialist("family-a", family_a, 0.9);
        let first_revision = specialist_a.id();
        let specialist_b = make_specialist("family-b", family_b, 0.8);
        let second_revision = specialist_b.id();
        core.stage(SettlementFrame::edits(&[
            EcologyEdit::Spawn(specialist_a),
            EcologyEdit::Spawn(specialist_b),
        ]))
        .unwrap()
        .commit(&mut core)
        .unwrap();

        let mut market = MarketArena::with_capacity(limits);
        for (identity, family, priority) in [
            ([60; 32], RoutingFamilyId::generic(), 0),
            ([62; 32], family_a, 1),
            ([63; 32], family_b, 2),
        ] {
            let opportunity = market
                .push_opportunity(
                    OpportunitySpec::candidate_in_family(identity, schema, family),
                    &[1.0],
                )
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    priority,
                ))
                .unwrap();
        }
        let mut output = PortfolioBuffer::with_capacity(limits);
        let portfolio = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(3, 3, 0, 3, 3)),
                3,
                &mut output,
            )
            .unwrap();
        let source_for_priority = |priority| {
            portfolio
                .allocations()
                .iter()
                .find(|allocation| allocation.bootstrap_priority() == priority)
                .unwrap()
                .source()
        };
        assert_eq!(source_for_priority(0), AllocationSource::Bootstrap);
        assert_eq!(
            source_for_priority(1),
            AllocationSource::Specialist(first_revision)
        );
        assert_eq!(
            source_for_priority(2),
            AllocationSource::Specialist(second_revision)
        );
        assert_eq!(
            IntelligenceCore::restore(core.checkpoint().as_bytes())
                .unwrap()
                .checkpoint(),
            core.checkpoint()
        );
    }

    #[test]
    fn checkpoint_round_trip_preserves_the_complete_ecology_and_rejects_corruption() {
        use sha2::Digest;

        let limits = IntelligenceLimits::new(8, 32, 32, 128, 8, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let specialist = SpecialistRevision::new(
            SpecialistMandate::all(
                RoleId::new("potential-curator").unwrap(),
                [
                    ForecastAxis::UsefulDescendants,
                    ForecastAxis::CompressionValue,
                ],
            )
            .unwrap(),
            CompactModel::linear(
                2,
                [
                    LinearHead::new(ForecastAxis::UsefulDescendants, -0.25, [0.5, 1.0], 0.1, 64)
                        .unwrap(),
                    LinearHead::new(ForecastAxis::CompressionValue, 0.25, [1.0, -0.5], 0.2, 32)
                        .unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap();
        let transition = core
            .stage(SettlementFrame::edits(&[EcologyEdit::Spawn(specialist)]))
            .unwrap();
        transition.commit(&mut core).unwrap();
        let checkpoint = core.checkpoint();
        let resident_bytes: u64 = core.resident_bytes();

        let restored = IntelligenceCore::restore(checkpoint.as_bytes()).unwrap();

        assert_eq!(restored.checkpoint(), checkpoint);
        assert_eq!(restored.limits(), limits);
        assert_eq!(
            checkpoint.identity().as_slice(),
            &checkpoint.as_bytes()[checkpoint.as_bytes().len() - 32..]
        );
        assert!(resident_bytes >= std::mem::size_of::<IntelligenceCore>() as u64);
        let mut corrupt = checkpoint.as_bytes().to_vec();
        let corrupt_index = corrupt.len() / 2;
        corrupt[corrupt_index] ^= 0x80;
        assert_eq!(
            IntelligenceCore::restore(&corrupt).unwrap_err(),
            IntelligenceError::CorruptState
        );

        let mut hostile_limits = checkpoint.as_bytes().to_vec();
        let payload_length = hostile_limits.len() - 32;
        hostile_limits[37..45].copy_from_slice(&u64::MAX.to_le_bytes());
        let checksum: [u8; 32] = sha2::Sha256::digest(&hostile_limits[..payload_length]).into();
        hostile_limits[payload_length..].copy_from_slice(&checksum);
        assert_eq!(
            IntelligenceCore::restore(&hostile_limits).unwrap_err(),
            IntelligenceError::CorruptState,
            "checksum-valid hostile limits are rejected before scratch allocation"
        );
    }

    #[test]
    fn limits_fork_rebinds_the_revision_only_when_all_retained_state_fits() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 32 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
            test_specialist("fork-specialist", 1.0),
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        let before = core.checkpoint();
        let expanded = IntelligenceLimits::new(16, 32, 32, 128, 8, 64 * 1024).unwrap();

        let forked = core.fork_with_limits(expanded).unwrap();

        assert_eq!(
            core.checkpoint(),
            before,
            "forking cannot mutate the incumbent"
        );
        assert_eq!(forked.limits(), expanded);
        assert_ne!(forked.checkpoint().identity(), before.identity());
        assert_eq!(
            forked.manifest().specialists().collect::<Vec<_>>(),
            core.manifest().specialists().collect::<Vec<_>>()
        );
        assert_eq!(
            IntelligenceCore::restore(forked.checkpoint().as_bytes())
                .unwrap()
                .checkpoint(),
            forked.checkpoint()
        );

        let too_small = IntelligenceLimits::new(8, 16, 16, 64, 4, 1).unwrap();
        assert_eq!(
            core.fork_with_limits(too_small).unwrap_err(),
            IntelligenceError::CapacityExceeded
        );
        assert_eq!(core.checkpoint(), before);
    }

    #[test]
    fn market_retains_every_typed_investment_kind_and_its_resource_vector() {
        use std::num::NonZeroU32;

        let limits = IntelligenceLimits::new(4, 16, 16, 64, 4, 16 * 1024).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let mut market = MarketArena::with_capacity(limits);
        let opportunity = market
            .push_opportunity(OpportunitySpec::emergent([9; 32]), &[0.25])
            .unwrap();
        let resources = ResourceVector::new(2, 3, 5, 7, 11);
        let specifications = [
            InvestmentSpec::generate(
                opportunity,
                SubjectId::new([1; 32]),
                NonZeroU32::new(3).unwrap(),
                resources,
                0,
            ),
            InvestmentSpec::verify(opportunity, resources, 1),
            InvestmentSpec::repair(
                opportunity,
                SubjectId::new([2; 32]),
                SubjectId::new([3; 32]),
                resources,
                2,
            ),
            InvestmentSpec::explore(opportunity, resources, 3),
            InvestmentSpec::train_specialist(opportunity, SubjectId::new([4; 32]), resources, 4),
            InvestmentSpec::compare_revision(opportunity, SubjectId::new([5; 32]), resources, 5),
            InvestmentSpec::consolidate(opportunity, SubjectId::new([6; 32]), resources, 6),
            InvestmentSpec::run_shadow_campaign(opportunity, SubjectId::new([7; 32]), resources, 7),
            InvestmentSpec::propose_runtime_policy(
                opportunity,
                SubjectId::new([8; 32]),
                resources,
                8,
            ),
        ];
        let expected = [
            InvestmentTag::Generate,
            InvestmentTag::Verify,
            InvestmentTag::Repair,
            InvestmentTag::Explore,
            InvestmentTag::TrainSpecialist,
            InvestmentTag::CompareRevision,
            InvestmentTag::Consolidate,
            InvestmentTag::RunShadowCampaign,
            InvestmentTag::ProposeRuntimePolicy,
        ];
        let ids = specifications
            .into_iter()
            .map(|specification| market.push_investment(specification).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            ids.iter()
                .map(|id| market.investment(*id).unwrap().tag())
                .collect::<Vec<_>>(),
            expected
        );

        let allowance = ResourceVector::new(18, 27, 45, 63, 99);
        let frame = MarketFrame::new(&market, allowance);
        let mut output = PortfolioBuffer::with_capacity(limits);
        let receipt = core.allocate(frame, 9, &mut output).unwrap();

        assert_eq!(receipt.allocations().len(), 9);
        assert_eq!(receipt.resources(), allowance);
    }

    #[test]
    fn ecology_split_distill_merge_and_retirement_are_atomic_and_manifested() {
        let limits = IntelligenceLimits::new(4, 16, 32, 128, 12, 128 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let parent = test_specialist("generalist", 1.0);
        let parent_id = parent.id();
        core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(parent)]))
            .unwrap()
            .commit(&mut core)
            .unwrap();

        let operator = test_specialist("operator-specialist", 2.0);
        let potential = test_specialist("potential-specialist", 3.0);
        let operator_id = operator.id();
        let potential_id = potential.id();
        core.stage(SettlementFrame::edits(&[EcologyEdit::Split {
            parent: parent_id,
            children: vec![operator, potential],
        }]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.manifest().active_ids().collect::<Vec<_>>(),
            vec![operator_id, potential_id]
        );
        assert_eq!(
            core.manifest()
                .specialists()
                .filter(|specialist| specialist.active())
                .filter(|specialist| specialist.lifecycle() == SpecialistLifecycle::Split)
                .count(),
            2
        );

        let distilled = SpecialistRevision::new(
            SpecialistMandate::all(
                RoleId::new("distilled-router").unwrap(),
                [ForecastAxis::ImmediateImprovement],
            )
            .unwrap(),
            CompactModel::prior([PriorHead::new(
                ForecastAxis::ImmediateImprovement,
                0.8,
                0.05,
                128,
            )
            .unwrap()])
            .unwrap(),
        )
        .unwrap();
        let distilled_id = distilled.id();
        core.stage(SettlementFrame::edits(&[EcologyEdit::Distill {
            teachers: vec![operator_id, potential_id],
            student: distilled,
        }]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.manifest().active_ids().collect::<Vec<_>>(),
            vec![distilled_id]
        );

        let peer = test_specialist("repair-specialist", 4.0);
        let peer_id = peer.id();
        core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(peer)]))
            .unwrap()
            .commit(&mut core)
            .unwrap();
        let merged = test_specialist("merged-specialist", 5.0);
        let merged_id = merged.id();
        core.stage(SettlementFrame::edits(&[EcologyEdit::Merge {
            parents: vec![distilled_id, peer_id],
            merged,
        }]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.manifest().active_ids().collect::<Vec<_>>(),
            vec![merged_id]
        );
        let recovered = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        assert_eq!(recovered.checkpoint(), core.checkpoint());

        let before = core.checkpoint();
        let invalid = [
            EcologyEdit::Retire(merged_id),
            EcologyEdit::Retire(SpecialistRevisionId::new([255; 32])),
        ];
        assert_eq!(
            core.stage(SettlementFrame::edits(&invalid)).unwrap_err(),
            IntelligenceError::MissingSpecialist
        );
        assert_eq!(
            core.checkpoint(),
            before,
            "a failed stage is all-or-nothing"
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the integration test exercises every terminal transition of one paired shadow campaign"
    )]
    fn causal_experience_requires_a_complete_symmetric_shadow_pair() {
        let limits = IntelligenceLimits::new(4, 8, 8, 64, 4, 32 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let mut market = MarketArena::with_capacity(limits);
        let opportunity = market
            .push_opportunity(OpportunitySpec::candidate([17; 32]), &[0.5])
            .unwrap();
        market
            .push_investment(InvestmentSpec::verify(
                opportunity,
                ResourceVector::new(10, 20, 30, 40, 1),
                0,
            ))
            .unwrap();
        let mut output = PortfolioBuffer::with_capacity(limits);
        let portfolio = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(10, 20, 30, 40, 1)),
                1,
                &mut output,
            )
            .unwrap();
        let receipts = portfolio.receipts().to_vec();
        assert_eq!(receipts[0].opportunity_kind(), OpportunityKind::Candidate);
        assert_eq!(
            receipts[0].feature_schema(),
            OpportunitySpec::candidate_schema()
        );
        assert_eq!(receipts[0].features(), &[0.5]);
        let settlement = InvestmentSettlement::new(
            receipts[0].decision(),
            InvestmentOutcome::VerifiedAccepted {
                verification_record: SubjectId::new([18; 32]),
            },
            ResourceVector::new(9, 20, 30, 39, 1),
        );
        let consequence = ConsequenceEdge::observed(
            CausalSubject::Decision(receipts[0].decision()),
            ConsequenceKind::ParetoImprovement,
        );
        let checkpoint = core.checkpoint().digest();
        let paired_resources = ResourceVector::new(100, 200, 300, 400, 8);
        let stream = SubjectId::new([19; 32]);
        let campaign = ShadowCampaignSpec::new(
            checkpoint,
            SubjectId::new([20; 32]),
            paired_resources,
            stream,
            [
                ForecastAxis::UsefulDescendants,
                ForecastAxis::CompressionValue,
            ],
        )
        .unwrap();
        let treatment = ShadowArmOutcome::new(
            campaign.id(),
            ShadowArm::Treatment,
            checkpoint,
            paired_resources,
            stream,
            [
                TypedOutcome::new(ForecastAxis::UsefulDescendants, 0.75).unwrap(),
                TypedOutcome::new(ForecastAxis::CompressionValue, 0.5).unwrap(),
            ],
        )
        .unwrap();
        let first_updates = [
            ShadowUpdate::Open(campaign.clone()),
            ShadowUpdate::Outcome(treatment),
        ];
        core.stage(SettlementFrame::observations(
            &receipts,
            &[settlement],
            &[consequence],
            &first_updates,
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(core.experience().receipts().len(), 1);
        assert_eq!(core.experience().settlements().len(), 1);
        assert_eq!(core.experience().consequences().len(), 1);
        assert!(core.experience().contrasts().is_empty());
        let open = core
            .experience()
            .shadow_campaign(campaign.id())
            .expect("the opened campaign is queryable for recovery");
        assert_eq!(open.lifecycle(), ShadowCampaignLifecycle::Open);
        assert!(open.has_outcome(ShadowArm::Treatment));
        assert!(!open.has_outcome(ShadowArm::Control));

        let asymmetric_control = ShadowArmOutcome::new(
            campaign.id(),
            ShadowArm::Control,
            checkpoint,
            ResourceVector::new(101, 200, 300, 400, 8),
            stream,
            [
                TypedOutcome::new(ForecastAxis::UsefulDescendants, 0.25).unwrap(),
                TypedOutcome::new(ForecastAxis::CompressionValue, 0.25).unwrap(),
            ],
        )
        .unwrap();
        core.stage(SettlementFrame::observations(
            &[],
            &[],
            &[],
            &[ShadowUpdate::Outcome(asymmetric_control)],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert!(core.experience().contrasts().is_empty());
        assert_eq!(
            core.experience()
                .shadow_campaign(campaign.id())
                .unwrap()
                .lifecycle(),
            ShadowCampaignLifecycle::Invalidated(ShadowInvalidationReason::ResourceMismatch)
        );

        let control = ShadowArmOutcome::new(
            campaign.id(),
            ShadowArm::Control,
            checkpoint,
            paired_resources,
            stream,
            [
                TypedOutcome::new(ForecastAxis::UsefulDescendants, 0.25).unwrap(),
                TypedOutcome::new(ForecastAxis::CompressionValue, 0.25).unwrap(),
            ],
        )
        .unwrap();
        core.stage(SettlementFrame::observations(
            &[],
            &[],
            &[],
            &[ShadowUpdate::Outcome(control)],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert!(core.experience().contrasts().is_empty());
        assert_eq!(core.experience().shadow_campaigns().len(), 1);
        assert_eq!(
            core.experience()
                .shadow_campaign(campaign.id())
                .unwrap()
                .lifecycle(),
            ShadowCampaignLifecycle::Invalidated(ShadowInvalidationReason::ResourceMismatch)
        );
        let recovered = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        assert_eq!(recovered.checkpoint(), core.checkpoint());
        assert_eq!(
            recovered
                .experience()
                .shadow_campaign(campaign.id())
                .unwrap()
                .lifecycle(),
            ShadowCampaignLifecycle::Invalidated(ShadowInvalidationReason::ResourceMismatch)
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the integration test characterizes all terminal shadow outcomes and their durable recovery"
    )]
    fn shadow_terminal_updates_preserve_only_exact_completed_contrasts() {
        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 16 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let resources = ResourceVector::new(10, 20, 0, 30, 2);
        let stream = SubjectId::new([91; 32]);
        let campaign = ShadowCampaignSpec::new(
            core.checkpoint().digest(),
            SubjectId::new([92; 32]),
            resources,
            stream,
            [ForecastAxis::UsefulDescendants],
        )
        .unwrap();
        let treatment = ShadowArmOutcome::new(
            campaign.id(),
            ShadowArm::Treatment,
            campaign.checkpoint(),
            resources,
            stream,
            [TypedOutcome::new(ForecastAxis::UsefulDescendants, 0.75).unwrap()],
        )
        .unwrap();
        let control = ShadowArmOutcome::new(
            campaign.id(),
            ShadowArm::Control,
            campaign.checkpoint(),
            resources,
            stream,
            [TypedOutcome::new(ForecastAxis::UsefulDescendants, 0.25).unwrap()],
        )
        .unwrap();
        core.stage(SettlementFrame::observations(
            &[],
            &[],
            &[],
            &[
                ShadowUpdate::Open(campaign.clone()),
                ShadowUpdate::Outcome(treatment.clone()),
                ShadowUpdate::Outcome(control),
            ],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.experience()
                .shadow_campaign(campaign.id())
                .unwrap()
                .lifecycle(),
            ShadowCampaignLifecycle::Completed
        );
        assert_eq!(core.experience().contrasts().len(), 1);

        core.stage(SettlementFrame::observations(
            &[],
            &[],
            &[],
            &[ShadowUpdate::Outcome(treatment)],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.experience()
                .shadow_campaign(campaign.id())
                .unwrap()
                .lifecycle(),
            ShadowCampaignLifecycle::Invalidated(ShadowInvalidationReason::DuplicateArm(
                ShadowArm::Treatment
            ))
        );
        assert!(core.experience().contrasts().is_empty());

        let asymmetric = ShadowCampaignSpec::new(
            core.checkpoint().digest(),
            SubjectId::new([93; 32]),
            resources,
            stream,
            [ForecastAxis::CompressionValue],
        )
        .unwrap();
        let asymmetric_treatment = ShadowArmOutcome::new(
            asymmetric.id(),
            ShadowArm::Treatment,
            asymmetric.checkpoint(),
            resources,
            stream,
            [TypedOutcome::new(ForecastAxis::CompressionValue, 0.5).unwrap()],
        )
        .unwrap();
        core.stage(SettlementFrame::observations(
            &[],
            &[],
            &[],
            &[
                ShadowUpdate::Open(asymmetric.clone()),
                ShadowUpdate::Outcome(asymmetric_treatment),
                ShadowUpdate::close(asymmetric.id()),
            ],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.experience()
                .shadow_campaign(asymmetric.id())
                .unwrap()
                .lifecycle(),
            ShadowCampaignLifecycle::Invalidated(ShadowInvalidationReason::AsymmetricArms)
        );

        let interrupted = ShadowCampaignSpec::new(
            core.checkpoint().digest(),
            SubjectId::new([94; 32]),
            resources,
            stream,
            [ForecastAxis::DeadEndRisk],
        )
        .unwrap();
        core.stage(SettlementFrame::observations(
            &[],
            &[],
            &[],
            &[
                ShadowUpdate::Open(interrupted.clone()),
                ShadowUpdate::interrupted(interrupted.id()),
            ],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.experience()
                .shadow_campaign(interrupted.id())
                .unwrap()
                .lifecycle(),
            ShadowCampaignLifecycle::Invalidated(ShadowInvalidationReason::Interrupted)
        );

        let invalidated = ShadowCampaignSpec::new(
            core.checkpoint().digest(),
            SubjectId::new([95; 32]),
            resources,
            stream,
            [ForecastAxis::VerificationCost],
        )
        .unwrap();
        core.stage(SettlementFrame::observations(
            &[],
            &[],
            &[],
            &[
                ShadowUpdate::Open(invalidated.clone()),
                ShadowUpdate::invalidate(
                    invalidated.id(),
                    ShadowInvalidationReason::CheckpointMismatch,
                ),
            ],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.experience()
                .shadow_campaign(invalidated.id())
                .unwrap()
                .lifecycle(),
            ShadowCampaignLifecycle::Invalidated(ShadowInvalidationReason::CheckpointMismatch)
        );

        let recovered = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        assert_eq!(recovered.checkpoint(), core.checkpoint());
        assert_eq!(recovered.experience().shadow_campaigns().len(), 4);
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "the integration test follows a knowledge product through verification, review, promotion, and recovery"
    )]
    fn knowledge_compiler_requires_verified_obligations_and_vector_causal_promotion() {
        let limits = IntelligenceLimits::new(8, 16, 16, 128, 4, 32 * 1024).unwrap();
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
                ]
            })
            .collect::<Vec<_>>();
        let exact = core
            .propose_knowledge_consolidation(&observations, [[20; 32]], [[21; 32]])
            .unwrap();
        let original_product = core.knowledge_product();
        let product = SubjectId::new(exact.product().identity());
        let obligations = exact
            .obligations()
            .iter()
            .map(|obligation| SubjectId::new(obligation.id()))
            .collect::<Vec<_>>();
        let mut sources = vec![
            SubjectId::new(exact.source().digest()),
            SubjectId::new(exact.support().digest()),
        ];
        sources.sort_unstable();
        sources.dedup();
        let gate = PromotionGate::all([
            ContextualRequirement::new(ForecastAxis::UsefulDescendants, 0.25).unwrap(),
            ContextualRequirement::new(ForecastAxis::CompressionValue, 0.25).unwrap(),
        ])
        .unwrap();
        let challenger = KnowledgeChallenger::new(
            CompilerRecipe::DeriveOperator,
            sources,
            KnowledgeProduct::derived_operator(product)
                .with_bytes(exact.encode())
                .unwrap(),
            obligations.iter().copied(),
            gate,
        )
        .unwrap();
        let challenger_id = challenger.id();

        core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Propose(
            challenger,
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.knowledge().status(challenger_id),
            Some(KnowledgeStatus::Provisional)
        );

        for (index, obligation) in obligations.iter().copied().enumerate() {
            record_accepted_obligation(
                &mut core,
                limits,
                challenger_id,
                obligation,
                u64::try_from(index).unwrap().saturating_add(1),
            );
        }
        assert_eq!(
            core.knowledge().status(challenger_id),
            Some(KnowledgeStatus::Verified)
        );

        let paired_resources = ResourceVector::new(100, 200, 300, 400, 8);
        let insufficient_checkpoint = core.checkpoint().digest();
        let insufficient_stream = SubjectId::new([36; 32]);
        let insufficient = ShadowCampaignSpec::new(
            insufficient_checkpoint,
            product,
            paired_resources,
            insufficient_stream,
            [
                ForecastAxis::UsefulDescendants,
                ForecastAxis::CompressionValue,
            ],
        )
        .unwrap();
        let insufficient_updates = [
            ShadowUpdate::Open(insufficient.clone()),
            ShadowUpdate::Outcome(
                ShadowArmOutcome::new(
                    insufficient.id(),
                    ShadowArm::Treatment,
                    insufficient_checkpoint,
                    paired_resources,
                    insufficient_stream,
                    [
                        TypedOutcome::new(ForecastAxis::UsefulDescendants, 0.3).unwrap(),
                        TypedOutcome::new(ForecastAxis::CompressionValue, 0.8).unwrap(),
                    ],
                )
                .unwrap(),
            ),
            ShadowUpdate::Outcome(
                ShadowArmOutcome::new(
                    insufficient.id(),
                    ShadowArm::Control,
                    insufficient_checkpoint,
                    paired_resources,
                    insufficient_stream,
                    [
                        TypedOutcome::new(ForecastAxis::UsefulDescendants, 0.2).unwrap(),
                        TypedOutcome::new(ForecastAxis::CompressionValue, 0.2).unwrap(),
                    ],
                )
                .unwrap(),
            ),
        ];
        core.stage(SettlementFrame::observations(
            &[],
            &[],
            &[],
            &insufficient_updates,
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        core.stage(SettlementFrame::knowledge(&[
            KnowledgeUpdate::EvaluatePromotion(challenger_id),
        ]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.knowledge().status(challenger_id),
            Some(KnowledgeStatus::Verified),
            "compression alone cannot promote a Knowledge Challenger"
        );

        let sufficient_checkpoint = core.checkpoint().digest();
        let sufficient_stream = SubjectId::new([37; 32]);
        let sufficient = ShadowCampaignSpec::new(
            sufficient_checkpoint,
            product,
            paired_resources,
            sufficient_stream,
            [
                ForecastAxis::UsefulDescendants,
                ForecastAxis::CompressionValue,
            ],
        )
        .unwrap();
        let sufficient_updates = [
            ShadowUpdate::Open(sufficient.clone()),
            ShadowUpdate::Outcome(
                ShadowArmOutcome::new(
                    sufficient.id(),
                    ShadowArm::Treatment,
                    sufficient_checkpoint,
                    paired_resources,
                    sufficient_stream,
                    [
                        TypedOutcome::new(ForecastAxis::UsefulDescendants, 0.75).unwrap(),
                        TypedOutcome::new(ForecastAxis::CompressionValue, 0.5).unwrap(),
                    ],
                )
                .unwrap(),
            ),
            ShadowUpdate::Outcome(
                ShadowArmOutcome::new(
                    sufficient.id(),
                    ShadowArm::Control,
                    sufficient_checkpoint,
                    paired_resources,
                    sufficient_stream,
                    [
                        TypedOutcome::new(ForecastAxis::UsefulDescendants, 0.25).unwrap(),
                        TypedOutcome::new(ForecastAxis::CompressionValue, 0.25).unwrap(),
                    ],
                )
                .unwrap(),
            ),
        ];
        core.stage(SettlementFrame::observations(
            &[],
            &[],
            &[],
            &sufficient_updates,
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        core.stage(SettlementFrame::knowledge(&[
            KnowledgeUpdate::EvaluatePromotion(challenger_id),
        ]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.knowledge().status(challenger_id),
            Some(KnowledgeStatus::Promoted)
        );
        assert_eq!(core.knowledge_product(), exact.product());
        let legacy_compiler =
            IntelligenceCore::restore(core.legacy_v13_checkpoint_for_test().as_bytes()).unwrap();
        assert_eq!(
            legacy_compiler
                .into_legacy_knowledge(&crate::knowledge::KnowledgeState::default())
                .unwrap_err(),
            IntelligenceError::InvalidKnowledge,
            "legacy active Knowledge must agree with the latest compiler promotion"
        );

        let restored = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        assert_eq!(restored.checkpoint(), core.checkpoint());
        assert_eq!(
            restored.knowledge().status(challenger_id),
            Some(KnowledgeStatus::Promoted)
        );

        core.stage(SettlementFrame::observations(
            &[],
            &[],
            &[],
            &[ShadowUpdate::invalidate(
                sufficient.id(),
                ShadowInvalidationReason::Interrupted,
            )],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.knowledge().status(challenger_id),
            Some(KnowledgeStatus::Verified),
            "an invalidated causal label cannot leave its Knowledge Challenger promoted"
        );
        assert_eq!(
            core.knowledge_product(),
            original_product,
            "retracting the exact causal campaign rolls back its active Knowledge Revision"
        );
        assert_eq!(
            IntelligenceCore::restore(core.checkpoint().as_bytes())
                .unwrap()
                .checkpoint(),
            core.checkpoint()
        );
    }

    #[test]
    fn knowledge_records_persist_concrete_products_and_resolve_verified_support() {
        let limits = IntelligenceLimits::new(16, 32, 64, 128, 8, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let source = SubjectId::new([111; 32]);
        let product_subject = SubjectId::new([112; 32]);
        let obligation = SubjectId::new([113; 32]);
        let product = KnowledgeProduct::derived_operator(product_subject)
            .with_bytes(vec![0x52, 0x46, 0x58, 0x01])
            .unwrap();
        let challenger = KnowledgeChallenger::new(
            CompilerRecipe::DeriveOperator,
            [source],
            product,
            [obligation],
            PromotionGate::all([
                ContextualRequirement::new(ForecastAxis::UsefulDescendants, 0.1).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        let challenger_id = challenger.id();
        core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Propose(
            challenger,
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        record_accepted_obligation(&mut core, limits, challenger_id, obligation, 1);

        let record = core.knowledge().record(challenger_id).unwrap();
        assert_eq!(record.status(), KnowledgeStatus::Verified);
        assert_eq!(record.recipe(), CompilerRecipe::DeriveOperator);
        assert_eq!(record.sources(), &[source]);
        assert_eq!(record.obligations(), &[obligation]);
        assert_eq!(
            record.product().kind(),
            KnowledgeProductKind::DerivedOperator
        );
        assert_eq!(record.product().subject(), product_subject);
        assert_eq!(
            record.product().bytes(),
            Some(&[0x52, 0x46, 0x58, 0x01][..])
        );
        assert_eq!(core.knowledge().records().len(), 1);
        assert_eq!(
            core.experience()
                .authoritative_verification_decision(obligation),
            record.reviews().first().map(KnowledgeReview::decision)
        );

        let restored = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        let restored_record = restored.knowledge().record(challenger_id).unwrap();
        assert_eq!(restored_record.product().bytes(), record.product().bytes());
        assert_eq!(restored.checkpoint(), core.checkpoint());
    }

    #[test]
    fn routing_index_bounds_specialist_evaluations_and_ignores_unrelated_families() {
        let limits = IntelligenceLimits::new(4, 8, 64, 512, 8, 64 * 1024)
            .unwrap()
            .with_maximum_specialists_per_route(2)
            .unwrap();
        let schema = OpportunitySpec::candidate_schema();
        let family_a = RoutingFamilyId::new([201; 32]);
        let family_b = RoutingFamilyId::new([202; 32]);
        let mut core = IntelligenceCore::fresh(limits);
        for (role, family, weight) in [("route-a-1", family_a, 1.0), ("route-a-2", family_a, 0.5)] {
            core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
                routed_test_specialist(role, family, schema, weight),
            )]))
            .unwrap()
            .commit(&mut core)
            .unwrap();
        }
        let mut market = MarketArena::with_capacity(limits);
        let opportunity = market
            .push_opportunity(
                OpportunitySpec::candidate_in_family([203; 32], schema, family_a),
                &[1.0],
            )
            .unwrap();
        for priority in 0..3 {
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    priority,
                ))
                .unwrap();
        }
        let mut output = PortfolioBuffer::with_capacity(limits);
        core.allocate(
            MarketFrame::new(&market, ResourceVector::new(8, 8, 0, 8, 8)),
            3,
            &mut output,
        )
        .unwrap();
        assert_eq!(output.specialists_evaluated(), 6);
        assert!(
            output.specialists_evaluated()
                <= market.investments.len() * limits.maximum_specialists_per_route
        );

        for (role, weight) in [("route-b-1", -0.5), ("route-b-2", -1.0)] {
            core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
                routed_test_specialist(role, family_b, schema, weight),
            )]))
            .unwrap()
            .commit(&mut core)
            .unwrap();
        }
        core.allocate(
            MarketFrame::new(&market, ResourceVector::new(8, 8, 0, 8, 8)),
            3,
            &mut output,
        )
        .unwrap();
        assert_eq!(
            output.specialists_evaluated(),
            6,
            "unrelated Routing Families add no specialist evaluation work"
        );
        assert_eq!(output.capacities(), [8, 16, 16, 512, 8, 8]);
        assert_eq!(
            output.resident_bytes(),
            limits.scratch_layout().unwrap().portfolio_bytes()
        );

        let before = core.checkpoint();
        assert_eq!(
            core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
                routed_test_specialist("route-a-overfull", family_a, schema, 2.0),
            )]))
            .unwrap_err(),
            IntelligenceError::CapacityExceeded
        );
        assert_eq!(core.checkpoint(), before);
    }

    #[test]
    fn all_family_generalists_share_the_exact_route_bound_without_duplicate_work() {
        let limits = IntelligenceLimits::new(4, 8, 16, 128, 4, 64 * 1024)
            .unwrap()
            .with_maximum_specialists_per_route(2)
            .unwrap();
        let schema = OpportunitySpec::candidate_schema();
        let family_a = RoutingFamilyId::new([206; 32]);
        let family_b = RoutingFamilyId::new([207; 32]);
        let mut core = IntelligenceCore::fresh(limits);
        for specialist in [
            generalist_test_specialist("all-family", schema, 0.25),
            routed_test_specialist("exact-family-a", family_a, schema, 1.0),
        ] {
            core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(specialist)]))
                .unwrap()
                .commit(&mut core)
                .unwrap();
        }
        let checkpoint = core.checkpoint();
        let restored = IntelligenceCore::restore(checkpoint.as_bytes()).unwrap();
        assert_eq!(restored.checkpoint(), checkpoint);

        let mut market = MarketArena::with_capacity(limits);
        for (identity, family, priority) in [([208; 32], family_a, 0), ([209; 32], family_b, 1)] {
            let opportunity = market
                .push_opportunity(
                    OpportunitySpec::candidate_in_family(identity, schema, family),
                    &[1.0],
                )
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    priority,
                ))
                .unwrap();
        }
        let mut output = PortfolioBuffer::with_capacity(limits);
        restored
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(2, 2, 0, 2, 2)),
                2,
                &mut output,
            )
            .unwrap();
        assert_eq!(output.specialists_evaluated(), 3);

        assert_eq!(
            core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
                routed_test_specialist("exact-family-a-overfull", family_a, schema, 2.0),
            )]))
            .unwrap_err(),
            IntelligenceError::CapacityExceeded
        );
        assert_eq!(core.checkpoint(), checkpoint);
    }

    #[test]
    fn legacy_v8_migration_retains_the_observed_route_fanout() {
        let limits = IntelligenceLimits::new(4, 8, 64, 512, 12, 1024 * 1024)
            .unwrap()
            .with_maximum_specialists_per_route(12)
            .unwrap();
        let schema = OpportunitySpec::candidate_schema();
        let family = RoutingFamilyId::new([204; 32]);
        let mut core = IntelligenceCore::fresh(limits);
        for index in 0..9 {
            let role = format!("legacy-route-{index}");
            core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
                routed_test_specialist(&role, family, schema, 1.0),
            )]))
            .unwrap()
            .commit(&mut core)
            .unwrap();
        }

        let legacy = core.legacy_v8_checkpoint_for_test();
        assert_eq!(&legacy.as_bytes()[..5], b"RFIC\x08");
        let restored = IntelligenceCore::restore(legacy.as_bytes()).unwrap();

        assert_eq!(restored.manifest().active_ids().count(), 9);
        assert_eq!(restored.limits().maximum_specialists_per_route, 9);
        assert_eq!(&restored.checkpoint().as_bytes()[..5], b"RFIC\x11");
    }

    #[test]
    fn legacy_v9_checkpoint_migrates_to_authenticated_delta_checkpoints() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        for (epoch, accepted) in [(1, false), (2, true), (3, false), (4, true)] {
            record_training_example(
                &mut core,
                limits,
                epoch,
                f32::from(u16::try_from(epoch).unwrap()),
                accepted,
            );
        }
        let legacy = core.legacy_v9_checkpoint_for_test();
        assert_eq!(&legacy.as_bytes()[..5], b"RFIC\x09");

        let restored = IntelligenceCore::restore(legacy.as_bytes()).unwrap();

        assert_eq!(restored.experience().receipts().len(), 4);
        assert_eq!(restored.experience().settlements().len(), 4);
        assert_eq!(&restored.checkpoint().as_bytes()[..5], b"RFIC\x11");
        IntelligenceCore::restore(restored.checkpoint().as_bytes()).unwrap();
    }

    #[test]
    fn legacy_v10_checkpoint_migrates_single_head_receipts_to_forecast_vectors() {
        let limits = IntelligenceLimits::new(4, 8, 8, 32, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
            test_specialist("legacy-single-head", 2.0),
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        let mut market = MarketArena::with_capacity(limits);
        for (identity, feature, priority) in [(21, 0.0, 0), (22, 1.0, 1)] {
            let opportunity = market
                .push_opportunity(OpportunitySpec::candidate([identity; 32]), &[feature])
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    priority,
                ))
                .unwrap();
        }
        let mut output = PortfolioBuffer::with_capacity(limits);
        let receipt = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(2, 2, 0, 2, 2)),
                2,
                &mut output,
            )
            .unwrap()
            .receipts()[1];
        assert_eq!(receipt.forecasts().len(), 1);
        let settlement = InvestmentSettlement::new(
            receipt.decision(),
            InvestmentOutcome::VerifiedAccepted {
                verification_record: SubjectId::new([22; 32]),
            },
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

        let legacy = core.legacy_v10_checkpoint_for_test();
        assert_eq!(&legacy.as_bytes()[..5], b"RFIC\x0A");
        let restored = IntelligenceCore::restore(legacy.as_bytes()).unwrap();

        assert_eq!(&restored.checkpoint().as_bytes()[..5], b"RFIC\x11");
        assert_eq!(restored.experience().receipts(), &[receipt]);
        assert_eq!(restored.experience().settlements(), &[settlement]);
    }

    #[test]
    fn legacy_v11_checkpoint_migrates_missing_preferences_to_neutral_priority() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        record_training_example(&mut core, limits, 1, -1.0, false);
        record_training_example(&mut core, limits, 2, 1.0, true);
        let legacy = core.legacy_v11_checkpoint_for_test();
        assert_eq!(&legacy.as_bytes()[..5], b"RFIC\x0B");

        let restored = IntelligenceCore::restore(legacy.as_bytes()).unwrap();

        assert_eq!(&restored.checkpoint().as_bytes()[..5], b"RFIC\x11");
        assert!(
            restored
                .experience()
                .receipts()
                .iter()
                .all(|receipt| receipt.preference_priority() == 0)
        );
    }

    #[test]
    fn legacy_v12_checkpoint_migrates_to_bootstrap_policy_under_v13() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        record_training_example(&mut core, limits, 1, -1.0, false);
        let legacy = core.legacy_v12_checkpoint_for_test();
        assert_eq!(&legacy.as_bytes()[..5], b"RFIC\x0C");

        let restored = IntelligenceCore::restore(legacy.as_bytes()).unwrap();

        assert_eq!(&restored.checkpoint().as_bytes()[..5], b"RFIC\x11");
        assert_eq!(
            restored.runtime_policy_revision(),
            crate::policy::RuntimePolicyRevision::bootstrap()
        );
    }

    #[test]
    fn legacy_policy_import_preserves_lineage_and_rejects_current_overwrite() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 64 * 1024).unwrap();
        let legacy = IntelligenceCore::fresh(limits).legacy_v12_checkpoint_for_test();
        let migrated = IntelligenceCore::restore(legacy.as_bytes()).unwrap();
        let mut standalone = RuntimePolicyState::bootstrap();
        let challenger = standalone.challenger_at(0).unwrap();
        standalone
            .apply_comparison(
                challenger,
                OperationalEvidence::new(0, 4, 3, 2, 1_000, 80, 100),
                OperationalEvidence::new(0, 5, 4, 3, 900, 70, 90),
            )
            .unwrap();
        let expected_identity = standalone.identity();

        let imported = migrated.into_legacy_runtime_policy(&standalone).unwrap();
        assert_eq!(imported.runtime_policy_revision(), challenger);
        assert_eq!(imported.runtime_policy_identity(), expected_identity);
        let recovered = IntelligenceCore::restore(imported.checkpoint().as_bytes()).unwrap();
        assert_eq!(recovered.runtime_policy_identity(), expected_identity);
        assert_eq!(
            recovered
                .into_legacy_runtime_policy(&standalone)
                .unwrap_err(),
            IntelligenceError::InvalidPolicy
        );
        assert_eq!(
            IntelligenceCore::fresh(limits)
                .into_legacy_runtime_policy(&RuntimePolicyState::bootstrap())
                .unwrap_err(),
            IntelligenceError::InvalidPolicy
        );
    }

    #[test]
    fn policy_promotion_is_deterministic_authenticated_and_recoverable() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let challenger = core.runtime_policy_challenger(7).unwrap();
        assert_eq!(core.runtime_policy_challenger(7), Some(challenger));
        let incumbent = OperationalEvidence::new(0, 4, 3, 2, 1_000, 80, 100);
        let dominant = OperationalEvidence::new(0, 5, 4, 3, 900, 70, 90);
        let (shadow_updates, update) =
            bound_policy_comparison(&core, challenger, incumbent, dominant);
        let mut market = MarketArena::with_capacity(limits);
        let opportunity = market
            .push_opportunity(OpportunitySpec::candidate([91; 32]), &[1.0])
            .unwrap();
        market
            .push_investment(InvestmentSpec::verify(
                opportunity,
                ResourceVector::new(1, 1, 0, 1, 1),
                0,
            ))
            .unwrap();
        let mut output = PortfolioBuffer::with_capacity(limits);
        let receipt = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(1, 1, 0, 1, 1)),
                1,
                &mut output,
            )
            .unwrap()
            .receipts()[0];
        let settlement = InvestmentSettlement::new(
            receipt.decision(),
            InvestmentOutcome::VerifiedRefuted,
            ResourceVector::new(1, 1, 0, 1, 1),
        );

        let transition = core
            .stage(
                SettlementFrame::observations(&[receipt], &[settlement], &[], &shadow_updates)
                    .with_policy_update(&update),
            )
            .unwrap();
        assert_eq!(
            transition.policy_decision(),
            Some(RuntimePolicyDecision::Promote)
        );
        let recovered = IntelligenceCore::restore(transition.checkpoint().as_bytes()).unwrap();
        assert_eq!(recovered.runtime_policy_revision(), challenger);
        assert_eq!(recovered.experience().receipts(), &[receipt]);
        transition.commit(&mut core).unwrap();
        assert_eq!(core.runtime_policy_revision(), challenger);
        assert_eq!(core.checkpoint(), recovered.checkpoint());
    }

    #[test]
    fn incomparable_and_rejected_policy_comparisons_preserve_authority() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let active = core.runtime_policy_revision();
        let bootstrap_resident = core.resident_bytes();
        let incomparable = core.runtime_policy_challenger(0).unwrap();
        let incumbent = OperationalEvidence::new(0, 4, 3, 2, 1_000, 80, 100);
        let tradeoff = OperationalEvidence::new(0, 5, 3, 2, 1_200, 70, 90);
        let (shadow_updates, update) =
            bound_policy_comparison(&core, incomparable, incumbent, tradeoff);
        let transition = core
            .stage(
                SettlementFrame::observations(&[], &[], &[], &shadow_updates)
                    .with_policy_update(&update),
            )
            .unwrap();
        assert_eq!(
            transition.policy_decision(),
            Some(RuntimePolicyDecision::RetainSpecialist)
        );
        transition.commit(&mut core).unwrap();
        assert_eq!(core.runtime_policy_revision(), active);
        assert!(core.resident_bytes() > bootstrap_resident);
        assert_ne!(core.runtime_policy_challenger(0), Some(incomparable));

        let rejected = core.runtime_policy_challenger(0).unwrap();
        let unsafe_evidence = OperationalEvidence::new(1, 3, 4, 3, 900, 70, 90);
        let (shadow_updates, update) =
            bound_policy_comparison(&core, rejected, incumbent, unsafe_evidence);
        let transition = core
            .stage(
                SettlementFrame::observations(&[], &[], &[], &shadow_updates)
                    .with_policy_update(&update),
            )
            .unwrap();
        assert_eq!(
            transition.policy_decision(),
            Some(RuntimePolicyDecision::Reject)
        );
        transition.commit(&mut core).unwrap();
        assert_eq!(core.runtime_policy_revision(), active);
        assert_eq!(core.runtime_policy_challenger(0), Some(rejected));
    }

    #[test]
    fn invalid_policy_comparison_rejects_the_whole_mixed_transition() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 64 * 1024).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let before = core.checkpoint();
        let (shadow_updates, invalid) = bound_policy_comparison(
            &core,
            core.runtime_policy_revision(),
            OperationalEvidence::new(0, 1, 1, 0, 10, 1, 1),
            OperationalEvidence::new(0, 2, 1, 0, 9, 1, 1),
        );
        let receipts = [];
        let settlements = [];

        assert_eq!(
            core.stage(
                SettlementFrame::observations(&receipts, &settlements, &[], &shadow_updates)
                    .with_policy_update(&invalid)
            )
            .unwrap_err(),
            IntelligenceError::InvalidPolicy
        );
        assert_eq!(core.checkpoint(), before);
    }

    #[test]
    fn policy_update_without_its_exact_terminal_shadow_is_rejected() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 64 * 1024).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let challenger = core.runtime_policy_challenger(0).unwrap();
        let unbound =
            PolicyUpdate::comparison(ShadowCampaignId::from_identity([0xa5; 32]), challenger);

        assert_eq!(
            core.stage(SettlementFrame::empty().with_policy_update(&unbound))
                .unwrap_err(),
            IntelligenceError::InvalidPolicy
        );
    }

    #[test]
    fn legacy_v16_resume_migrates_before_appending_a_bound_policy_update() {
        let limits = IntelligenceLimits::new(8, 32, 32, 128, 8, 64 * 1024).unwrap();
        let legacy = IntelligenceCore::fresh(limits).legacy_v16_checkpoint_for_test();
        assert!(legacy.as_bytes().starts_with(b"RFIC\x10"));
        let mut resumed = IntelligenceCore::restore(legacy.as_bytes()).unwrap();
        assert!(resumed.checkpoint().as_bytes().starts_with(b"RFIC\x11"));

        let challenger = resumed.runtime_policy_challenger(0).unwrap();
        let (shadows, update) = bound_policy_comparison(
            &resumed,
            challenger,
            OperationalEvidence::new(0, 1, 1, 0, 20, 2, 20),
            OperationalEvidence::new(0, 2, 2, 0, 10, 1, 10),
        );
        resumed
            .stage(
                SettlementFrame::observations(&[], &[], &[], &shadows).with_policy_update(&update),
            )
            .unwrap()
            .commit(&mut resumed)
            .unwrap();
        assert!(resumed.checkpoint().as_bytes().starts_with(b"RFIC\x11"));
        let restored = IntelligenceCore::restore(resumed.checkpoint().as_bytes()).unwrap();
        assert_eq!(restored.runtime_policy_revision(), challenger);
    }

    #[test]
    fn checkpoint_rejects_checksum_valid_overfull_routing_index() {
        use sha2::Digest;

        const ROUTE_BOUND_OFFSET: usize = 5 + 32 + 6 * std::mem::size_of::<u64>();

        let limits = IntelligenceLimits::new(4, 8, 32, 512, 4, 64 * 1024)
            .unwrap()
            .with_maximum_specialists_per_route(2)
            .unwrap();
        let schema = OpportunitySpec::candidate_schema();
        let family = RoutingFamilyId::new([205; 32]);
        let mut core = IntelligenceCore::fresh(limits);
        for (role, weight) in [("hostile-route-a", 1.0), ("hostile-route-b", 2.0)] {
            core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
                routed_test_specialist(role, family, schema, weight),
            )]))
            .unwrap()
            .commit(&mut core)
            .unwrap();
        }
        let checkpoint = core.checkpoint();
        assert_eq!(
            IntelligenceCore::restore(checkpoint.as_bytes())
                .unwrap()
                .checkpoint(),
            checkpoint
        );

        let mut hostile = checkpoint.as_bytes().to_vec();
        hostile[ROUTE_BOUND_OFFSET..ROUTE_BOUND_OFFSET + 8].copy_from_slice(&1_u64.to_le_bytes());
        let payload_length = hostile.len() - 32;
        let checksum: [u8; 32] = sha2::Sha256::digest(&hostile[..payload_length]).into();
        hostile[payload_length..].copy_from_slice(&checksum);

        assert_eq!(
            IntelligenceCore::restore(&hostile).unwrap_err(),
            IntelligenceError::CorruptState
        );
    }

    #[test]
    fn knowledge_product_bytes_obey_the_configured_retained_capacity() {
        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 64).unwrap();
        let core = IntelligenceCore::fresh(limits);
        let before = core.checkpoint();
        let challenger = KnowledgeChallenger::new(
            CompilerRecipe::DeriveOperator,
            [SubjectId::new([121; 32])],
            KnowledgeProduct::derived_operator(SubjectId::new([122; 32]))
                .with_bytes(vec![0x5a; 65])
                .unwrap(),
            [SubjectId::new([123; 32])],
            PromotionGate::all([
                ContextualRequirement::new(ForecastAxis::UsefulDescendants, 0.1).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Propose(
                challenger
            )]))
            .unwrap_err(),
            IntelligenceError::CapacityExceeded
        );
        assert_eq!(core.checkpoint(), before);
    }

    #[test]
    fn legacy_v6_knowledge_records_migrate_to_the_bounded_product_format() {
        let limits = IntelligenceLimits::new(16, 32, 64, 128, 8, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let obligation = SubjectId::new([126; 32]);
        let challenger = KnowledgeChallenger::new(
            CompilerRecipe::DeriveOperator,
            [SubjectId::new([124; 32])],
            KnowledgeProduct::derived_operator(SubjectId::new([125; 32])),
            [obligation],
            PromotionGate::all([
                ContextualRequirement::new(ForecastAxis::UsefulDescendants, 0.1).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        let challenger_id = challenger.id();
        core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Propose(
            challenger,
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        record_accepted_obligation(&mut core, limits, challenger_id, obligation, 1);

        let legacy = core.legacy_v6_checkpoint_for_test();
        assert_eq!(&legacy.as_bytes()[..5], b"RFIC\x06");

        let restored = IntelligenceCore::restore(legacy.as_bytes()).unwrap();
        let record = restored.knowledge().records().next().unwrap();
        assert_eq!(record.status(), KnowledgeStatus::Verified);
        assert_eq!(record.product().bytes(), None);
        assert_eq!(
            restored
                .experience()
                .authoritative_verification_decision(obligation),
            record.reviews().first().map(KnowledgeReview::decision)
        );
        assert_eq!(&restored.checkpoint().as_bytes()[..5], b"RFIC\x11");
    }

    #[test]
    fn legacy_v7_payload_products_migrate_to_explicit_semantic_meaning() {
        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let challenger = KnowledgeChallenger::new(
            CompilerRecipe::DeriveOperator,
            [SubjectId::new([143; 32])],
            KnowledgeProduct::derived_operator(SubjectId::new([144; 32]))
                .with_bytes(vec![4, 5, 6])
                .unwrap(),
            [SubjectId::new([145; 32])],
            PromotionGate::all([
                ContextualRequirement::new(ForecastAxis::UsefulDescendants, 0.1).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Propose(
            challenger,
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();

        let legacy = core.legacy_v7_checkpoint_for_test();
        assert_eq!(&legacy.as_bytes()[..5], b"RFIC\x07");
        let restored = IntelligenceCore::restore(legacy.as_bytes()).unwrap();
        let record = restored.knowledge().records().next().unwrap();
        assert_eq!(
            record.product().meaning(),
            KnowledgeProductMeaning::Semantic
        );
        assert_eq!(record.product().bytes(), Some(&[4, 5, 6][..]));
        assert_eq!(record.status(), KnowledgeStatus::Provisional);
        assert_eq!(&restored.checkpoint().as_bytes()[..5], b"RFIC\x11");
    }

    #[test]
    fn accepted_verification_support_resolves_by_authoritative_record_subject() {
        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let candidate = SubjectId::new([127; 32]);
        let verification_record = SubjectId::new([128; 32]);
        let mut market = MarketArena::with_capacity(limits);
        let opportunity = market
            .push_opportunity(OpportunitySpec::candidate(candidate.identity()), &[1.0])
            .unwrap();
        market
            .push_investment(InvestmentSpec::verify(
                opportunity,
                ResourceVector::new(1, 1, 0, 1, 1),
                0,
            ))
            .unwrap();
        let mut scratch = PortfolioBuffer::with_capacity(limits);
        let receipt = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(1, 1, 0, 1, 1)),
                1,
                &mut scratch,
            )
            .unwrap()
            .receipts()[0];
        core.stage(SettlementFrame::observations(
            &[receipt],
            &[InvestmentSettlement::new(
                receipt.decision(),
                InvestmentOutcome::VerifiedAccepted {
                    verification_record,
                },
                ResourceVector::new(1, 1, 0, 1, 1),
            )],
            &[],
            &[],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();

        assert_eq!(
            core.experience()
                .authoritative_verification_decision(verification_record),
            Some(receipt.decision())
        );
        assert_eq!(
            core.experience()
                .authoritative_verification_decision(candidate),
            None,
            "the candidate identity cannot impersonate the verifier's retained record"
        );
    }

    #[test]
    fn settlement_frame_atomically_composes_verification_and_knowledge_review() {
        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let obligation = SubjectId::new([132; 32]);
        let challenger = KnowledgeChallenger::new(
            CompilerRecipe::DeriveOperator,
            [SubjectId::new([133; 32])],
            KnowledgeProduct::derived_operator(SubjectId::new([134; 32])),
            [obligation],
            PromotionGate::all([
                ContextualRequirement::new(ForecastAxis::UsefulDescendants, 0.1).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        let challenger_id = challenger.id();
        core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Propose(
            challenger,
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();

        let mut market = MarketArena::with_capacity(limits);
        let opportunity = market
            .push_opportunity(OpportunitySpec::candidate([135; 32]), &[1.0])
            .unwrap();
        market
            .push_investment(InvestmentSpec::verify(
                opportunity,
                ResourceVector::new(1, 1, 0, 1, 1),
                0,
            ))
            .unwrap();
        let mut scratch = PortfolioBuffer::with_capacity(limits);
        let receipt = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(1, 1, 0, 1, 1)),
                1,
                &mut scratch,
            )
            .unwrap()
            .receipts()[0];
        let review = KnowledgeUpdate::Review(KnowledgeReview::new(
            challenger_id,
            obligation,
            receipt.decision(),
        ));
        let bad_settlement = InvestmentSettlement::new(
            receipt.decision(),
            InvestmentOutcome::VerifiedAccepted {
                verification_record: SubjectId::new([136; 32]),
            },
            ResourceVector::new(1, 1, 0, 1, 1),
        );
        let before = core.checkpoint();
        assert_eq!(
            core.stage(
                SettlementFrame::observations(&[receipt], &[bad_settlement], &[], &[])
                    .with_knowledge_updates(std::slice::from_ref(&review))
            )
            .unwrap_err(),
            IntelligenceError::InvalidReference
        );
        assert_eq!(core.checkpoint(), before);

        let settlement = InvestmentSettlement::new(
            receipt.decision(),
            InvestmentOutcome::VerifiedAccepted {
                verification_record: obligation,
            },
            ResourceVector::new(1, 1, 0, 1, 1),
        );
        core.stage(
            SettlementFrame::observations(&[receipt], &[settlement], &[], &[])
                .with_knowledge_updates(std::slice::from_ref(&review)),
        )
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.experience()
                .authoritative_verification_outcome(receipt.decision(), obligation),
            Ok(InvestmentOutcome::VerifiedAccepted {
                verification_record: obligation
            })
        );
        assert_eq!(
            core.knowledge().status(challenger_id),
            Some(KnowledgeStatus::Verified)
        );
    }

    #[test]
    fn knowledge_invalidation_is_durable_and_never_masquerades_as_refutation() {
        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let obligation = SubjectId::new([137; 32]);
        let challenger = KnowledgeChallenger::new(
            CompilerRecipe::GeneralizeArtifact,
            [SubjectId::new([138; 32])],
            KnowledgeProduct::artifact(SubjectId::new([139; 32])),
            [obligation],
            PromotionGate::all([
                ContextualRequirement::new(ForecastAxis::UsefulDescendants, 0.1).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        let challenger_id = challenger.id();
        core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Propose(
            challenger,
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Invalidate {
            challenger: challenger_id,
            reason: KnowledgeInvalidationReason::Product,
        }]))
        .unwrap()
        .commit(&mut core)
        .unwrap();

        let invalidated = KnowledgeStatus::Invalidated(KnowledgeInvalidationReason::Product);
        let record = core.knowledge().record(challenger_id).unwrap();
        assert_eq!(record.status(), invalidated);
        assert_eq!(
            record.invalidation_reason(),
            Some(KnowledgeInvalidationReason::Product)
        );
        assert_eq!(record.promoted_by(), None);

        core.stage(SettlementFrame::knowledge(&[
            KnowledgeUpdate::EvaluatePromotion(challenger_id),
        ]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(core.knowledge().status(challenger_id), Some(invalidated));
        assert_eq!(
            core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Review(
                KnowledgeReview::new(
                    challenger_id,
                    obligation,
                    DecisionId::from_identity([140; 32]),
                ),
            )]))
            .unwrap_err(),
            IntelligenceError::InvalidKnowledge
        );

        let restored = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        assert_eq!(
            restored.knowledge().status(challenger_id),
            Some(invalidated)
        );
        assert_eq!(restored.checkpoint(), core.checkpoint());
    }

    #[test]
    fn only_payload_bound_explicitly_nonsemantic_indexes_need_no_obligations() {
        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 64 * 1024).unwrap();
        let product_subject = SubjectId::new([141; 32]);
        let source = SubjectId::new([142; 32]);
        let gate = || {
            PromotionGate::all([
                ContextualRequirement::new(ForecastAxis::UsefulDescendants, 0.1).unwrap(),
            ])
            .unwrap()
        };
        assert_eq!(
            KnowledgeChallenger::new(
                CompilerRecipe::Deduplicate,
                [source],
                KnowledgeProduct::knowledge_index(product_subject),
                [],
                gate(),
            )
            .unwrap_err(),
            IntelligenceError::InvalidKnowledge
        );
        assert_eq!(
            KnowledgeChallenger::new(
                CompilerRecipe::Deduplicate,
                [source],
                KnowledgeProduct::knowledge_index(product_subject)
                    .with_bytes(vec![1, 2, 3])
                    .unwrap(),
                [],
                gate(),
            )
            .unwrap_err(),
            IntelligenceError::InvalidKnowledge,
            "payload presence alone does not waive semantic correctness obligations"
        );

        let product = KnowledgeProduct::nonsemantic_index(product_subject, vec![1, 2, 3]).unwrap();
        assert_eq!(product.meaning(), KnowledgeProductMeaning::NonSemanticIndex);
        let challenger =
            KnowledgeChallenger::new(CompilerRecipe::Deduplicate, [source], product, [], gate())
                .unwrap();
        let challenger_id = challenger.id();
        let mut core = IntelligenceCore::fresh(limits);
        core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Propose(
            challenger,
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();

        assert_eq!(
            core.knowledge().status(challenger_id),
            Some(KnowledgeStatus::Verified)
        );
        core.stage(SettlementFrame::knowledge(&[
            KnowledgeUpdate::EvaluatePromotion(challenger_id),
        ]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(
            core.knowledge().status(challenger_id),
            Some(KnowledgeStatus::Verified),
            "a nonsemantic index remains subject to causal shadow promotion"
        );
        let restored = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
        let record = restored.knowledge().record(challenger_id).unwrap();
        assert_eq!(
            record.product().meaning(),
            KnowledgeProductMeaning::NonSemanticIndex
        );
        assert_eq!(record.product().bytes(), Some(&[1, 2, 3][..]));
        assert_eq!(record.status(), KnowledgeStatus::Verified);
    }

    #[test]
    fn checkpoint_rejects_checksum_valid_hostile_knowledge_product_lengths() {
        use sha2::Digest;

        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let marker = b"reflex-opaque-product-boundary";
        let challenger = KnowledgeChallenger::new(
            CompilerRecipe::DeriveOperator,
            [SubjectId::new([129; 32])],
            KnowledgeProduct::derived_operator(SubjectId::new([130; 32]))
                .with_bytes(marker.to_vec())
                .unwrap(),
            [SubjectId::new([131; 32])],
            PromotionGate::all([
                ContextualRequirement::new(ForecastAxis::UsefulDescendants, 0.1).unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Propose(
            challenger,
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        let mut hostile = core.checkpoint().as_bytes().to_vec();
        let marker_offset = hostile
            .windows(marker.len())
            .position(|window| window == marker)
            .unwrap();
        assert_eq!(
            hostile
                .windows(marker.len())
                .filter(|window| *window == marker)
                .count(),
            1
        );
        hostile[marker_offset - 8..marker_offset].copy_from_slice(&u64::MAX.to_le_bytes());
        let payload_length = hostile.len() - 32;
        let checksum: [u8; 32] = sha2::Sha256::digest(&hostile[..payload_length]).into();
        hostile[payload_length..].copy_from_slice(&checksum);

        assert_eq!(
            IntelligenceCore::restore(&hostile).unwrap_err(),
            IntelligenceError::CorruptState
        );
    }

    #[test]
    fn native_training_spawns_one_bounded_specialist_from_authoritative_experience() {
        let limits = IntelligenceLimits::new(16, 32, 64, 128, 8, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        for epoch in 1_u64..=16 {
            let accepted = epoch % 4 >= 2;
            let feature = if accepted { 1.0 } else { -1.0 };
            record_training_example(&mut core, limits, epoch, feature, accepted);
        }
        let budget = NativeTrainingBudget::new(16, 12, 0.25).unwrap();
        core.train_native(budget)
            .unwrap()
            .expect("class-diverse Experience trains a blind first specialist")
            .commit(&mut core)
            .unwrap();
        assert_eq!(core.manifest().active_ids().count(), 1);
        assert_eq!(
            core.manifest().specialists().next().unwrap().lifecycle(),
            SpecialistLifecycle::Spawn
        );
        core.train_native(budget)
            .unwrap()
            .expect("supported exact-family evidence grows beyond the Generalist")
            .commit(&mut core)
            .unwrap();
        assert_eq!(core.manifest().active_ids().count(), 2);
        assert!(core.train_native(budget).unwrap().is_none());

        let mut market = MarketArena::with_capacity(limits);
        for (identity, feature, priority) in [(71, -1.0, 1), (72, 1.0, 2)] {
            let opportunity = market
                .push_opportunity(OpportunitySpec::candidate([identity; 32]), &[feature])
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    priority,
                ))
                .unwrap();
        }
        let mut output = PortfolioBuffer::with_capacity(limits);
        let portfolio = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(2, 2, 0, 2, 2)),
                2,
                &mut output,
            )
            .unwrap();
        assert_eq!(
            portfolio.allocations()[0].source(),
            AllocationSource::Bootstrap
        );
        assert_eq!(portfolio.allocations()[1].bootstrap_priority(), 2);
        assert!(matches!(
            portfolio.allocations()[1].source(),
            AllocationSource::Specialist(_)
        ));
        assert_eq!(
            IntelligenceCore::restore(core.checkpoint().as_bytes())
                .unwrap()
                .checkpoint(),
            core.checkpoint()
        );
    }

    #[test]
    fn production_comparison_rotates_selection_cases_restart_exactly() {
        let limits = IntelligenceLimits::new(32, 64, 128, 512, 8, 128 * 1024).unwrap();
        let budget = NativeTrainingBudget::new(16, 12, 0.25).unwrap();
        let (mut core, receipts, settlements) = comparison_training_cohort(limits);
        let prepared = core
            .prepare_native_training(
                SettlementFrame::observations(&receipts, &settlements, &[], &[]),
                budget,
            )
            .unwrap()
            .expect("the newest completed cohort creates a concrete challenger");
        let mut mismatched = settlements.clone();
        mismatched[0] = InvestmentSettlement::new(
            receipts[0].decision(),
            InvestmentOutcome::VerifiedUnknown,
            ResourceVector::new(1, 1, 0, 1, 1),
        );
        assert!(
            core.reproduce_prepared_native_training(
                SettlementFrame::observations(&receipts, &settlements, &[], &[]),
                &prepared,
            )
            .unwrap(),
            "a selected comparison reruns and binds the exact prepared challenger"
        );
        assert!(
            !core
                .reproduce_prepared_native_training(
                    SettlementFrame::observations(&receipts, &mismatched, &[], &[]),
                    &prepared,
                )
                .unwrap(),
            "different causal evidence cannot complete the prepared comparison"
        );
        assert_eq!(
            core.stage(
                SettlementFrame::observations(&receipts, &mismatched, &[], &[])
                    .with_prepared_native_training(&prepared),
            )
            .unwrap_err(),
            IntelligenceError::StaleTransition
        );
        core.stage(
            SettlementFrame::observations(&receipts, &settlements, &[], &[])
                .with_prepared_native_training(&prepared),
        )
        .unwrap()
        .commit(&mut core)
        .unwrap();
        for expected_uses in [2_usize, 3] {
            core = IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
            core.stage(SettlementFrame::empty().with_native_training(budget))
                .unwrap()
                .commit(&mut core)
                .unwrap();
            assert_eq!(
                core.experience()
                    .consequences()
                    .iter()
                    .filter(|edge| edge.kind() == ConsequenceKind::SelectionUse)
                    .count(),
                8 * expected_uses
            );
        }
        let rotated = core.checkpoint();
        let transition = core
            .stage(SettlementFrame::empty().with_native_training(budget))
            .unwrap();
        assert_eq!(transition.checkpoint(), rotated);
        assert_eq!(core.manifest().active_ids().count(), 0);
    }

    fn comparison_training_cohort(
        limits: IntelligenceLimits,
    ) -> (
        IntelligenceCore,
        Vec<InvestmentReceipt>,
        Vec<InvestmentSettlement>,
    ) {
        let core = IntelligenceCore::fresh(limits);
        let mut market = MarketArena::with_capacity(limits);
        for index in 0_u8..16 {
            let positive_feature = index % 4 >= 2;
            let opportunity = market
                .push_opportunity(
                    OpportunitySpec::candidate_in_corpus(
                        [index.saturating_add(1); 32],
                        [index.saturating_add(64); 32],
                        OpportunitySpec::candidate_schema(),
                        RoutingFamilyId::generic(),
                    ),
                    &[if positive_feature { 1.0 } else { -1.0 }],
                )
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    u32::from(index),
                ))
                .unwrap();
        }
        let mut output = PortfolioBuffer::with_capacity(limits);
        let receipts = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(16, 16, 0, 16, 16)),
                16,
                &mut output,
            )
            .unwrap()
            .receipts()
            .to_vec();
        let settlements = receipts
            .iter()
            .enumerate()
            .map(|(index, receipt)| {
                let replay = index % 2 == 0;
                let positive_feature = index % 4 >= 2;
                let accepted = if replay {
                    positive_feature
                } else {
                    !positive_feature
                };
                InvestmentSettlement::new(
                    receipt.decision(),
                    if accepted {
                        InvestmentOutcome::VerifiedAccepted {
                            verification_record: SubjectId::new(
                                [u8::try_from(index + 1).unwrap(); 32],
                            ),
                        }
                    } else {
                        InvestmentOutcome::VerifiedRefuted
                    },
                    ResourceVector::new(1, 1, 0, 1, 1),
                )
            })
            .collect::<Vec<_>>();
        (core, receipts, settlements)
    }

    #[test]
    fn native_training_grows_a_society_across_routing_families() {
        let limits = IntelligenceLimits::new(16, 32, 64, 128, 8, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let schema = OpportunitySpec::candidate_schema();
        let family_a = RoutingFamilyId::new([101; 32]);
        let family_b = RoutingFamilyId::new([102; 32]);
        let mut epoch = 1_u64;
        for (family, reverse, cases) in [(family_a, false, 24), (family_b, true, 8)] {
            for case in 0..cases {
                let base_acceptance = case % 4 >= 2;
                let accepted = base_acceptance != reverse;
                let feature = if base_acceptance { 1.0 } else { -1.0 };
                record_training_example_in_family(
                    &mut core, limits, epoch, feature, accepted, schema, family,
                );
                epoch += 1;
            }
        }
        core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
            trainer::broad_test_specialist(schema),
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        let budget = NativeTrainingBudget::new(32, 64, 0.25).unwrap();
        core.train_native(budget)
            .unwrap()
            .expect("a supported routing family spawns a Specialist beside the Generalist")
            .commit(&mut core)
            .unwrap();
        assert_eq!(core.manifest().active_ids().count(), 2);
        let mut market = MarketArena::with_capacity(limits);
        for (identity, family, priority) in [
            ([100; 32], RoutingFamilyId::generic(), 0),
            ([101; 32], family_a, 1),
            ([102; 32], family_b, 2),
        ] {
            let opportunity = market
                .push_opportunity(
                    OpportunitySpec::candidate_in_family(identity, schema, family),
                    &[1.0],
                )
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    priority,
                ))
                .unwrap();
        }
        let mut output = PortfolioBuffer::with_capacity(limits);
        let portfolio = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(3, 3, 0, 3, 3)),
                3,
                &mut output,
            )
            .unwrap();
        let adaptive_sources = portfolio.allocations()[1..]
            .iter()
            .map(Allocation::source)
            .collect::<Vec<_>>();
        assert_eq!(adaptive_sources.len(), 2);
        assert_ne!(adaptive_sources[0], adaptive_sources[1]);
        assert!(
            adaptive_sources
                .iter()
                .all(|source| matches!(source, AllocationSource::Specialist(_)))
        );
    }

    #[test]
    fn autonomous_family_split_is_deterministic_recoverable_and_route_bounded() {
        let limits = IntelligenceLimits::new(16, 32, 64, 128, 8, 64 * 1024)
            .unwrap()
            .with_maximum_specialists_per_route(2)
            .unwrap();
        let schema = OpportunitySpec::candidate_schema();
        let first_family = RoutingFamilyId::new([111; 32]);
        let second_family = RoutingFamilyId::new([112; 32]);
        let mut core = IntelligenceCore::fresh(limits);
        let mut epoch = 1_u64;
        for (family, reverse, cases) in [(first_family, false, 16), (second_family, true, 16)] {
            for case in 0..cases {
                let base_acceptance = case % 4 >= 2;
                let feature = if base_acceptance { 1.0 } else { -1.0 };
                record_training_example_in_family(
                    &mut core,
                    limits,
                    epoch,
                    feature,
                    base_acceptance != reverse,
                    schema,
                    family,
                );
                epoch += 1;
            }
        }
        let budget = NativeTrainingBudget::new(16, 64, 0.25).unwrap();
        core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(
            trainer::broad_test_specialist(schema),
        )]))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        let checkpoint = core.checkpoint();
        let mut left = IntelligenceCore::restore(checkpoint.as_bytes()).unwrap();
        let mut right = IntelligenceCore::restore(checkpoint.as_bytes()).unwrap();
        let left_transition = left
            .train_native(budget)
            .unwrap()
            .expect("stable opposed residuals split the Generalist");
        let right_transition = right
            .train_native(budget)
            .unwrap()
            .expect("the same evidence produces the same split");
        assert_eq!(left_transition.checkpoint(), right_transition.checkpoint());
        left_transition.commit(&mut left).unwrap();
        right_transition.commit(&mut right).unwrap();
        assert_eq!(left.checkpoint(), right.checkpoint());

        let recovered = IntelligenceCore::restore(left.checkpoint().as_bytes()).unwrap();
        assert_eq!(recovered.manifest().active_ids().count(), 2);
        assert_eq!(
            recovered
                .manifest()
                .specialists()
                .filter(|specialist| {
                    specialist.active() && specialist.lifecycle() == SpecialistLifecycle::Split
                })
                .count(),
            2
        );
        let mut market = MarketArena::with_capacity(limits);
        for (identity, family) in [([111; 32], first_family), ([112; 32], second_family)] {
            let opportunity = market
                .push_opportunity(
                    OpportunitySpec::candidate_in_family(identity, schema, family),
                    &[1.0],
                )
                .unwrap();
            market
                .push_investment(InvestmentSpec::verify(
                    opportunity,
                    ResourceVector::new(1, 1, 0, 1, 1),
                    0,
                ))
                .unwrap();
        }
        let mut output = PortfolioBuffer::with_capacity(limits);
        recovered
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(2, 2, 0, 2, 2)),
                2,
                &mut output,
            )
            .unwrap();
        assert_eq!(output.specialists_evaluated(), 2);
    }

    #[test]
    fn stage_and_checkpoint_work_scale_with_delta_not_retained_experience() {
        let matrix = [4_u64, 32, 128].map(|retained| {
            let limits = IntelligenceLimits::new(16, 256, 64, 512, 8, 256 * 1024).unwrap();
            let mut core = IntelligenceCore::fresh(limits);
            let retained_feature = f32::from(u16::try_from(retained).unwrap());
            for epoch in 1..=retained {
                let epoch_feature = f32::from(u16::try_from(epoch).unwrap());
                record_training_example(
                    &mut core,
                    limits,
                    epoch,
                    epoch_feature / retained_feature,
                    epoch % 2 == 0,
                );
            }
            let first_checkpoint = core.checkpoint();
            let second_checkpoint = core.checkpoint();
            assert!(first_checkpoint.shares_storage_with(&second_checkpoint));
            drop(first_checkpoint);
            drop(second_checkpoint);
            let checkpoint_bytes_before = core.checkpoint().as_bytes().len();
            let consequence_reserve = core.transition_resident_bytes(0, 0, 2, None);
            let training_reserve = core.transition_resident_bytes(
                8,
                8,
                0,
                Some(NativeTrainingBudget::new(32, 4, 0.1).unwrap()),
            );
            let decision = core
                .experience()
                .receipts()
                .last()
                .copied()
                .map(InvestmentReceipt::decision)
                .unwrap();
            let consequence = ConsequenceEdge::observed(
                CausalSubject::Decision(decision),
                ConsequenceKind::Admitted,
            );
            let transition = core
                .stage(SettlementFrame::observations(&[], &[], &[consequence], &[]))
                .unwrap();
            let work = transition.stage_work();
            transition.commit(&mut core).unwrap();
            let checkpoint_growth = core
                .checkpoint()
                .as_bytes()
                .len()
                .saturating_sub(checkpoint_bytes_before);
            IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
            (
                retained,
                work,
                checkpoint_growth,
                consequence_reserve,
                training_reserve,
            )
        });

        let baseline_work = matrix[0].1;
        let baseline_growth = matrix[0].2;
        let baseline_consequence_reserve = matrix[0].3;
        let baseline_training_reserve = matrix[0].4;
        for (retained, work, checkpoint_growth, consequence_reserve, training_reserve) in matrix {
            assert_eq!(work.causal_items, 1, "retained={retained}");
            assert!(!work.ecology_recomputed, "retained={retained}");
            assert!(!work.knowledge_recomputed, "retained={retained}");
            assert_eq!(work.historical_items_reencoded, 0, "retained={retained}");
            assert_eq!(
                work.checkpoint_delta_bytes, baseline_work.checkpoint_delta_bytes,
                "retained={retained}"
            );
            assert_eq!(checkpoint_growth, baseline_growth, "retained={retained}");
            assert_eq!(
                consequence_reserve, baseline_consequence_reserve,
                "retained={retained}"
            );
            assert!(
                training_reserve >= baseline_training_reserve,
                "the stable corpus-role index charges retained history: retained={retained}"
            );
            if retained > matrix[0].0 {
                assert!(
                    training_reserve > baseline_training_reserve,
                    "larger retained history must not hide role-index memory: retained={retained}"
                );
            }
        }
    }

    #[test]
    fn empty_stage_reuses_the_checkpoint_without_extending_the_log() {
        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 64 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let before = core.checkpoint();
        let before_bytes = core.resident_bytes();
        let transition = core.stage(SettlementFrame::empty()).unwrap();
        assert_eq!(transition.materialization_resident_bytes(&core), 0);
        let proposed = transition.checkpoint();
        assert!(before.shares_storage_with(&proposed));
        assert_eq!(transition.stage_work().checkpoint_delta_bytes, 0);

        transition.commit(&mut core).unwrap();
        let after = core.checkpoint();
        assert!(before.shares_storage_with(&after));
        assert_eq!(core.resident_bytes(), before_bytes);
    }

    #[test]
    fn tiny_delta_over_a_large_checkpoint_uses_one_exact_final_allocation() {
        let limits = IntelligenceLimits::new(8, 64, 64, 512, 64, 1024 * 1024)
            .unwrap()
            .with_maximum_specialists_per_route(64)
            .unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let mut retiring = None;
        for index in 0..32 {
            let specialist = test_specialist(&format!("retained-specialist-{index}"), 0.25);
            retiring = Some(specialist.id());
            core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(specialist)]))
                .unwrap()
                .commit(&mut core)
                .unwrap();
        }
        let base_length = core.checkpoint().as_bytes().len();
        let transition = core
            .stage(SettlementFrame::edits(&[EcologyEdit::Retire(
                retiring.unwrap(),
            )]))
            .unwrap();
        let final_length = transition.checkpoint_encoded_len();
        assert!(final_length > base_length);
        assert!(transition.stage_work().checkpoint_delta_bytes < base_length);
        assert_eq!(
            transition.materialization_resident_bytes(&core),
            u64::try_from(final_length).unwrap()
                + u64::try_from(std::mem::size_of::<Vec<u8>>() + 2 * std::mem::size_of::<usize>(),)
                    .unwrap()
        );
        let checkpoint = transition.checkpoint();
        assert_eq!(checkpoint.as_bytes().len(), final_length);
        assert_eq!(checkpoint.allocation_capacity(), final_length);
        IntelligenceCore::restore(checkpoint.as_bytes()).unwrap();
    }

    #[test]
    fn ecology_and_knowledge_checkpoints_encode_only_authoritative_operations() {
        let ecology_matrix = [1_usize, 8, 32].map(|retained| {
            let limits = IntelligenceLimits::new(8, 64, 64, 512, 64, 1024 * 1024)
                .unwrap()
                .with_maximum_specialists_per_route(64)
                .unwrap();
            let mut core = IntelligenceCore::fresh(limits);
            let mut retiring = None;
            for index in 0..retained {
                let specialist = test_specialist(&format!("delta-specialist-{index}"), 0.25);
                retiring = Some(specialist.id());
                core.stage(SettlementFrame::edits(&[EcologyEdit::Spawn(specialist)]))
                    .unwrap()
                    .commit(&mut core)
                    .unwrap();
            }
            let transition = core
                .stage(SettlementFrame::edits(&[EcologyEdit::Retire(
                    retiring.unwrap(),
                )]))
                .unwrap();
            let work = transition.stage_work();
            transition.commit(&mut core).unwrap();
            IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
            (retained, work)
        });
        let ecology_delta_bytes = ecology_matrix[0].1.checkpoint_delta_bytes;
        for (retained, work) in ecology_matrix {
            assert_eq!(work.historical_items_reencoded, 0, "retained={retained}");
            assert_eq!(work.ecology_items_cloned, retained);
            assert_eq!(work.knowledge_items_cloned, 0);
            assert_eq!(work.checkpoint_delta_bytes, ecology_delta_bytes);
        }

        let knowledge_matrix = [1_usize, 8, 32].map(|retained| {
            let limits = IntelligenceLimits::new(8, 64, 64, 512, 4, 1024 * 1024).unwrap();
            let mut core = IntelligenceCore::fresh(limits);
            let mut invalidating = None;
            for index in 0..retained {
                let identity = [u8::try_from(index + 1).unwrap(); 32];
                let challenger = KnowledgeChallenger::new(
                    CompilerRecipe::Deduplicate,
                    [SubjectId::new([identity[0].wrapping_add(64); 32])],
                    KnowledgeProduct::nonsemantic_index(SubjectId::new(identity), vec![1]).unwrap(),
                    [],
                    PromotionGate::all([ContextualRequirement::new(
                        ForecastAxis::UsefulDescendants,
                        0.1,
                    )
                    .unwrap()])
                    .unwrap(),
                )
                .unwrap();
                invalidating = Some(challenger.id());
                core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Propose(
                    challenger,
                )]))
                .unwrap()
                .commit(&mut core)
                .unwrap();
            }
            let transition = core
                .stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Invalidate {
                    challenger: invalidating.unwrap(),
                    reason: KnowledgeInvalidationReason::Product,
                }]))
                .unwrap();
            let work = transition.stage_work();
            transition.commit(&mut core).unwrap();
            IntelligenceCore::restore(core.checkpoint().as_bytes()).unwrap();
            (retained, work)
        });
        let knowledge_delta_bytes = knowledge_matrix[0].1.checkpoint_delta_bytes;
        for (retained, work) in knowledge_matrix {
            assert_eq!(work.historical_items_reencoded, 0, "retained={retained}");
            assert_eq!(work.ecology_items_cloned, 0);
            assert_eq!(work.knowledge_items_cloned, retained);
            assert_eq!(work.checkpoint_delta_bytes, knowledge_delta_bytes);
        }
    }

    #[test]
    fn causal_experience_requires_complete_kind_compatible_settlements() {
        let limits = IntelligenceLimits::new(8, 8, 8, 32, 4, 16 * 1024).unwrap();
        let mut core = IntelligenceCore::fresh(limits);
        let before = core.checkpoint();
        let mut market = MarketArena::with_capacity(limits);
        let verify_opportunity = market
            .push_opportunity(OpportunitySpec::candidate([81; 32]), &[1.0])
            .unwrap();
        let explore_opportunity = market
            .push_opportunity(OpportunitySpec::emergent([82; 32]), &[0.5])
            .unwrap();
        let resources = ResourceVector::new(1, 1, 0, 1, 1);
        market
            .push_investment(InvestmentSpec::verify(verify_opportunity, resources, 0))
            .unwrap();
        market
            .push_investment(InvestmentSpec::explore(explore_opportunity, resources, 1))
            .unwrap();
        let mut output = PortfolioBuffer::with_capacity(limits);
        let receipts = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(2, 2, 0, 2, 2)),
                2,
                &mut output,
            )
            .unwrap()
            .receipts()
            .to_vec();
        let verified = InvestmentSettlement::new(
            receipts[0].decision(),
            InvestmentOutcome::VerifiedRefuted,
            resources,
        );
        let completed = InvestmentSettlement::new(
            receipts[1].decision(),
            InvestmentOutcome::Completed,
            resources,
        );

        for invalid in [
            SettlementFrame::observations(&receipts[..1], &[], &[], &[]),
            SettlementFrame::observations(&[], &[verified], &[], &[]),
            SettlementFrame::observations(&receipts[..1], &[completed], &[], &[]),
            SettlementFrame::observations(&receipts[1..], &[verified], &[], &[]),
        ] {
            assert_eq!(
                core.stage(invalid).unwrap_err(),
                IntelligenceError::InvalidSettlement
            );
            assert_eq!(core.checkpoint(), before);
        }

        core.stage(SettlementFrame::observations(
            &receipts,
            &[verified, completed],
            &[],
            &[],
        ))
        .unwrap()
        .commit(&mut core)
        .unwrap();
        assert_eq!(core.experience().receipts(), &receipts);
        assert_eq!(core.experience().settlements(), &[verified, completed]);
    }

    fn record_training_example(
        core: &mut IntelligenceCore,
        limits: IntelligenceLimits,
        epoch: u64,
        feature: f32,
        accepted: bool,
    ) {
        record_training_example_in_family(
            core,
            limits,
            epoch,
            feature,
            accepted,
            OpportunitySpec::candidate_schema(),
            RoutingFamilyId::generic(),
        );
    }

    fn record_training_example_in_family(
        core: &mut IntelligenceCore,
        limits: IntelligenceLimits,
        epoch: u64,
        feature: f32,
        accepted: bool,
        schema: FeatureSchemaId,
        family: RoutingFamilyId,
    ) {
        let identity = u8::try_from(epoch).unwrap();
        let mut market = MarketArena::with_capacity(limits);
        let opportunity = market
            .push_opportunity(
                OpportunitySpec::candidate_in_family([identity; 32], schema, family),
                &[feature],
            )
            .unwrap();
        market
            .push_investment(InvestmentSpec::verify(
                opportunity,
                ResourceVector::new(1, 1, 0, 1, 1),
                0,
            ))
            .unwrap();
        let mut output = PortfolioBuffer::with_capacity(limits);
        let receipt = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(1, 1, 0, 1, 1)).at_epoch(epoch),
                1,
                &mut output,
            )
            .unwrap()
            .receipts()[0];
        let outcome = if accepted {
            InvestmentOutcome::VerifiedAccepted {
                verification_record: SubjectId::new([identity; 32]),
            }
        } else {
            InvestmentOutcome::VerifiedRefuted
        };
        core.stage(SettlementFrame::observations(
            &[receipt],
            &[InvestmentSettlement::new(
                receipt.decision(),
                outcome,
                ResourceVector::new(1, 1, 0, 1, 1),
            )],
            &[],
            &[],
        ))
        .unwrap()
        .commit(core)
        .unwrap();
    }

    fn record_accepted_obligation(
        core: &mut IntelligenceCore,
        limits: IntelligenceLimits,
        challenger: KnowledgeChallengerId,
        obligation: SubjectId,
        epoch: u64,
    ) {
        let mut market = MarketArena::with_capacity(limits);
        let opportunity = market
            .push_opportunity(OpportunitySpec::candidate(obligation.identity()), &[1.0])
            .unwrap();
        market
            .push_investment(InvestmentSpec::verify(
                opportunity,
                ResourceVector::new(10, 20, 0, 30, 1),
                0,
            ))
            .unwrap();
        let mut output = PortfolioBuffer::with_capacity(limits);
        let portfolio = core
            .allocate(
                MarketFrame::new(&market, ResourceVector::new(10, 20, 0, 30, 1)).at_epoch(epoch),
                1,
                &mut output,
            )
            .unwrap();
        let receipt = portfolio.receipts()[0];
        let settlement = InvestmentSettlement::new(
            receipt.decision(),
            InvestmentOutcome::VerifiedAccepted {
                verification_record: obligation,
            },
            ResourceVector::new(9, 20, 0, 29, 1),
        );
        core.stage(SettlementFrame::observations(
            &[receipt],
            &[settlement],
            &[],
            &[],
        ))
        .unwrap()
        .commit(core)
        .unwrap();
        core.stage(SettlementFrame::knowledge(&[KnowledgeUpdate::Review(
            KnowledgeReview::new(challenger, obligation, receipt.decision()),
        )]))
        .unwrap()
        .commit(core)
        .unwrap();
    }

    fn test_specialist(role: &str, weight: f32) -> SpecialistRevision {
        SpecialistRevision::new(
            SpecialistMandate::all(
                RoleId::new(role).unwrap(),
                [ForecastAxis::ImmediateImprovement],
            )
            .unwrap(),
            CompactModel::linear(
                1,
                [
                    LinearHead::new(ForecastAxis::ImmediateImprovement, 0.0, [weight], 0.1, 16)
                        .unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn routed_test_specialist(
        role: &str,
        family: RoutingFamilyId,
        schema: FeatureSchemaId,
        weight: f32,
    ) -> SpecialistRevision {
        SpecialistRevision::new(
            SpecialistMandate::for_routing_families(
                RoleId::new(role).unwrap(),
                [ForecastAxis::ImmediateImprovement],
                [OpportunityKind::Candidate],
                [schema],
                [family],
            )
            .unwrap(),
            CompactModel::linear(
                1,
                [
                    LinearHead::new(ForecastAxis::ImmediateImprovement, 0.0, [weight], 0.1, 16)
                        .unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn generalist_test_specialist(
        role: &str,
        schema: FeatureSchemaId,
        weight: f32,
    ) -> SpecialistRevision {
        SpecialistRevision::new(
            SpecialistMandate::for_routing_families(
                RoleId::new(role).unwrap(),
                [ForecastAxis::ImmediateImprovement],
                [OpportunityKind::Candidate],
                [schema],
                std::iter::empty::<RoutingFamilyId>(),
            )
            .unwrap(),
            CompactModel::linear(
                1,
                [
                    LinearHead::new(ForecastAxis::ImmediateImprovement, 0.0, [weight], 0.1, 16)
                        .unwrap(),
                ],
            )
            .unwrap(),
        )
        .unwrap()
    }
}
