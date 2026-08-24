use crate::intelligence::{
    CheckpointDigest, ForecastAxis, IntelligenceError, ResourceVector, ShadowArm, ShadowArmOutcome,
    ShadowCampaignSpec, ShadowInvalidationReason, ShadowUpdate, SubjectId, TypedOutcome,
};
use crate::policy::{PolicyUpdate, RuntimePolicyDecision, RuntimePolicyRevision};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SubjectMask(SubjectId);

impl SubjectMask {
    pub(crate) const fn new(subject: SubjectId) -> Self {
        Self(subject)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SchedulingToken([u8; 32]);

impl SchedulingToken {
    pub(crate) const fn new(identity: [u8; 32]) -> Self {
        Self(identity)
    }
}

pub(super) struct ShadowCampaignControls {
    checkpoint: CheckpointDigest,
    random_stream: SubjectId,
    scheduling: SchedulingToken,
    axes: Vec<ForecastAxis>,
}

impl ShadowCampaignControls {
    pub(super) fn new(
        checkpoint: CheckpointDigest,
        random_stream: SubjectId,
        scheduling: SchedulingToken,
        axes: impl IntoIterator<Item = ForecastAxis>,
    ) -> Self {
        Self {
            checkpoint,
            random_stream,
            scheduling,
            axes: axes.into_iter().collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NestedShadowPolicy {
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShadowExecutionPlan {
    subject: SubjectMask,
    resources: ResourceVector,
    random_stream: SubjectId,
    scheduling: SchedulingToken,
}

impl ShadowExecutionPlan {
    pub(crate) const fn new(
        subject: SubjectMask,
        resources: ResourceVector,
        random_stream: SubjectId,
        scheduling: SchedulingToken,
    ) -> Self {
        Self {
            subject,
            resources,
            random_stream,
            scheduling,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ShadowArmContext {
    arm: ShadowArm,
    resources: ResourceVector,
    random_stream: SubjectId,
    scheduling: SchedulingToken,
    nested_shadows: NestedShadowPolicy,
}

impl ShadowArmContext {
    const fn new(arm: ShadowArm, plan: ShadowExecutionPlan) -> Self {
        Self {
            arm,
            resources: plan.resources,
            random_stream: plan.random_stream,
            scheduling: plan.scheduling,
            nested_shadows: NestedShadowPolicy::Disabled,
        }
    }

    pub(crate) const fn arm(self) -> ShadowArm {
        self.arm
    }

    pub(crate) const fn resources(self) -> ResourceVector {
        self.resources
    }

    pub(crate) const fn random_stream(self) -> SubjectId {
        self.random_stream
    }

    pub(crate) const fn scheduling_token(self) -> SchedulingToken {
        self.scheduling
    }

    pub(crate) const fn nested_shadow_policy(self) -> NestedShadowPolicy {
        self.nested_shadows
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ArmExecution<O> {
    Completed(O),
    Invalidated(ShadowInvalidationReason),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PairedArmOutput<O> {
    treatment: O,
    control: O,
}

impl<O> PairedArmOutput<O> {
    pub(crate) fn into_arms(self) -> (O, O) {
        (self.treatment, self.control)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ShadowRunResult<O> {
    Completed(PairedArmOutput<O>),
    Invalidated(ShadowInvalidationReason),
}

pub(crate) struct ShadowRunner;

impl ShadowRunner {
    pub(crate) fn run<C, O>(
        checkpoint: C,
        plan: ShadowExecutionPlan,
        apply_mask: impl Fn(&mut C, &SubjectMask) -> Result<(), ShadowInvalidationReason>,
        mut execute: impl FnMut(C, ShadowArmContext) -> ArmExecution<O>,
    ) -> ShadowRunResult<O>
    where
        C: Clone,
    {
        let mut control = checkpoint.clone();
        if let Err(reason) = apply_mask(&mut control, &plan.subject) {
            return ShadowRunResult::Invalidated(reason);
        }
        let treatment_context = ShadowArmContext::new(ShadowArm::Treatment, plan);
        let control_context = ShadowArmContext::new(ShadowArm::Control, plan);
        debug_assert_eq!(treatment_context.random_stream(), plan.random_stream);
        debug_assert_eq!(control_context.random_stream(), plan.random_stream);
        debug_assert_eq!(treatment_context.scheduling_token(), plan.scheduling);
        debug_assert_eq!(control_context.scheduling_token(), plan.scheduling);
        debug_assert_eq!(
            treatment_context.nested_shadow_policy(),
            NestedShadowPolicy::Disabled
        );
        debug_assert_eq!(
            control_context.nested_shadow_policy(),
            NestedShadowPolicy::Disabled
        );
        let treatment = execute(checkpoint, treatment_context);
        let control = execute(control, control_context);
        match (treatment, control) {
            (ArmExecution::Completed(treatment), ArmExecution::Completed(control)) => {
                ShadowRunResult::Completed(PairedArmOutput { treatment, control })
            }
            (ArmExecution::Invalidated(treatment), ArmExecution::Invalidated(control))
                if treatment == control =>
            {
                ShadowRunResult::Invalidated(treatment)
            }
            _ => ShadowRunResult::Invalidated(ShadowInvalidationReason::AsymmetricArms),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct VerificationSubEnvelope {
    protected_total: ResourceVector,
    per_arm: ResourceVector,
    treatment: Option<ResourceVector>,
    control: Option<ResourceVector>,
}

impl VerificationSubEnvelope {
    pub(super) fn protected(remaining: ResourceVector, per_mille: u16) -> Option<Self> {
        if per_mille == 0 || per_mille > 1_000 {
            return None;
        }
        let scale = |value: u64| {
            u64::try_from((u128::from(value) * u128::from(per_mille)) / 1_000).unwrap_or(u64::MAX)
        };
        let protected_total = ResourceVector::new(
            scale(remaining.cpu_time_ns),
            scale(remaining.resident_bytes),
            scale(remaining.durable_bytes),
            scale(remaining.elapsed_time_ns),
            scale(remaining.verification_requests),
        );
        let per_arm = ResourceVector::new(
            protected_total.cpu_time_ns / 2,
            protected_total.resident_bytes,
            protected_total.durable_bytes / 2,
            protected_total.elapsed_time_ns / 2,
            protected_total.verification_requests / 2,
        );
        if per_arm.cpu_time_ns == 0
            || per_arm.resident_bytes == 0
            || per_arm.elapsed_time_ns == 0
            || per_arm.verification_requests == 0
        {
            return None;
        }
        Some(Self {
            protected_total,
            per_arm,
            treatment: None,
            control: None,
        })
    }

    pub(super) const fn per_arm(self) -> ResourceVector {
        self.per_arm
    }

    pub(super) const fn protected_total(self) -> ResourceVector {
        self.protected_total
    }

    pub(super) const fn reserved_verification_requests(self) -> u64 {
        self.per_arm.verification_requests.saturating_mul(2)
    }

    pub(super) fn record(
        &mut self,
        arm: ShadowArm,
        usage: ResourceVector,
    ) -> Result<(), ShadowInvalidationReason> {
        if !resource_fits(usage, self.per_arm) {
            return Err(ShadowInvalidationReason::ResourceMismatch);
        }
        let slot = match arm {
            ShadowArm::Treatment => &mut self.treatment,
            ShadowArm::Control => &mut self.control,
        };
        if slot.replace(usage).is_some() {
            return Err(ShadowInvalidationReason::DuplicateArm(arm));
        }
        let Some(treatment) = self.treatment else {
            return Ok(());
        };
        let Some(control) = self.control else {
            return Ok(());
        };
        let combined = ResourceVector::new(
            treatment.cpu_time_ns.saturating_add(control.cpu_time_ns),
            treatment.resident_bytes.max(control.resident_bytes),
            treatment
                .durable_bytes
                .saturating_add(control.durable_bytes),
            treatment
                .elapsed_time_ns
                .saturating_add(control.elapsed_time_ns),
            treatment
                .verification_requests
                .saturating_add(control.verification_requests),
        );
        if resource_fits(combined, self.protected_total) {
            Ok(())
        } else {
            Err(ShadowInvalidationReason::ResourceMismatch)
        }
    }
}

const fn resource_fits(usage: ResourceVector, allowance: ResourceVector) -> bool {
    usage.cpu_time_ns <= allowance.cpu_time_ns
        && usage.resident_bytes <= allowance.resident_bytes
        && usage.durable_bytes <= allowance.durable_bytes
        && usage.elapsed_time_ns <= allowance.elapsed_time_ns
        && usage.verification_requests <= allowance.verification_requests
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct RuntimePolicyArmTransaction<T> {
    transaction: T,
    usage: ResourceVector,
    outcomes: Vec<TypedOutcome>,
}

impl<T> RuntimePolicyArmTransaction<T> {
    pub(super) const fn new(
        transaction: T,
        usage: ResourceVector,
        outcomes: Vec<TypedOutcome>,
    ) -> Self {
        Self {
            transaction,
            usage,
            outcomes,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RuntimePolicyArmCheckpoint<C> {
    restart: C,
    incumbent: RuntimePolicyRevision,
    applied: RuntimePolicyRevision,
}

pub(super) struct ScheduledRuntimePolicyTrial<C> {
    challenger: RuntimePolicyRevision,
    checkpoint: RuntimePolicyArmCheckpoint<C>,
    plan: ShadowExecutionPlan,
    resources: VerificationSubEnvelope,
    campaign: ShadowCampaignSpec,
}

impl<C> ScheduledRuntimePolicyTrial<C> {
    pub(super) fn schedule(
        incumbent: RuntimePolicyRevision,
        challenger: RuntimePolicyRevision,
        restart_complete_checkpoint: C,
        resources: VerificationSubEnvelope,
        controls: ShadowCampaignControls,
    ) -> Result<Option<Self>, IntelligenceError> {
        let subject = SubjectId::new(challenger.identity());
        let campaign = ShadowCampaignSpec::new(
            controls.checkpoint,
            subject,
            resources.per_arm(),
            controls.random_stream,
            controls.axes,
        )?;
        Ok(Some(Self {
            challenger,
            checkpoint: RuntimePolicyArmCheckpoint {
                restart: restart_complete_checkpoint,
                incumbent,
                applied: challenger,
            },
            plan: ShadowExecutionPlan::new(
                SubjectMask::new(subject),
                resources.per_arm(),
                controls.random_stream,
                controls.scheduling,
            ),
            resources,
            campaign,
        }))
    }

    pub(super) const fn reserved_verification_requests(&self) -> u64 {
        self.resources.reserved_verification_requests()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) enum RuntimePolicyTrialResult<T> {
    Applied {
        decision: RuntimePolicyDecision,
        transaction: T,
    },
    Invalidated(ShadowInvalidationReason),
}

#[derive(Debug)]
pub(super) enum RuntimePolicyTrialError<E> {
    Causal(E),
    InvalidOutcome(IntelligenceError),
}

pub(super) fn run_scheduled_runtime_policy_trial<C, T, E>(
    active: RuntimePolicyRevision,
    trial: ScheduledRuntimePolicyTrial<C>,
    mut execute: impl FnMut(
        C,
        RuntimePolicyRevision,
        ShadowArmContext,
    ) -> ArmExecution<RuntimePolicyArmTransaction<T>>,
    mut record_causal: impl FnMut(
        &[ShadowUpdate],
        Option<&PolicyUpdate>,
    ) -> Result<Option<RuntimePolicyDecision>, E>,
) -> Result<RuntimePolicyTrialResult<T>, RuntimePolicyTrialError<E>>
where
    C: Clone,
{
    record_causal(&[ShadowUpdate::Open(trial.campaign.clone())], None)
        .map_err(RuntimePolicyTrialError::Causal)?;
    if active != trial.checkpoint.incumbent {
        record_invalidation(
            &trial.campaign,
            ShadowInvalidationReason::CheckpointMismatch,
            &mut record_causal,
        )?;
        return Ok(RuntimePolicyTrialResult::Invalidated(
            ShadowInvalidationReason::CheckpointMismatch,
        ));
    }
    let challenger = trial.challenger;
    let mut resources = trial.resources;
    let run = ShadowRunner::run(
        trial.checkpoint,
        trial.plan,
        |checkpoint, subject| {
            if *subject == SubjectMask::new(SubjectId::new(challenger.identity()))
                && checkpoint.applied == challenger
            {
                checkpoint.applied = checkpoint.incumbent;
                Ok(())
            } else {
                Err(ShadowInvalidationReason::CheckpointMismatch)
            }
        },
        |checkpoint, context| execute(checkpoint.restart, checkpoint.applied, context),
    );
    let output = match run {
        ShadowRunResult::Completed(output) => output,
        ShadowRunResult::Invalidated(reason) => {
            record_invalidation(&trial.campaign, reason, &mut record_causal)?;
            return Ok(RuntimePolicyTrialResult::Invalidated(reason));
        }
    };
    let (challenger_transaction, incumbent_transaction) = output.into_arms();
    if let Err(reason) = resources.record(ShadowArm::Treatment, challenger_transaction.usage) {
        record_invalidation(&trial.campaign, reason, &mut record_causal)?;
        return Ok(RuntimePolicyTrialResult::Invalidated(reason));
    }
    if let Err(reason) = resources.record(ShadowArm::Control, incumbent_transaction.usage) {
        record_invalidation(&trial.campaign, reason, &mut record_causal)?;
        return Ok(RuntimePolicyTrialResult::Invalidated(reason));
    }
    let campaign = &trial.campaign;
    let updates = [
        ShadowUpdate::Outcome(
            ShadowArmOutcome::new(
                campaign.id(),
                ShadowArm::Treatment,
                campaign.checkpoint(),
                challenger_transaction.usage,
                campaign.random_stream(),
                challenger_transaction.outcomes.clone(),
            )
            .map_err(RuntimePolicyTrialError::InvalidOutcome)?,
        ),
        ShadowUpdate::Outcome(
            ShadowArmOutcome::new(
                campaign.id(),
                ShadowArm::Control,
                campaign.checkpoint(),
                incumbent_transaction.usage,
                campaign.random_stream(),
                incumbent_transaction.outcomes.clone(),
            )
            .map_err(RuntimePolicyTrialError::InvalidOutcome)?,
        ),
    ];
    let policy_update = PolicyUpdate::comparison(campaign.id(), challenger);
    let decision = record_causal(&updates, Some(&policy_update))
        .map_err(RuntimePolicyTrialError::Causal)?
        .ok_or(RuntimePolicyTrialError::InvalidOutcome(
            IntelligenceError::InvalidPolicy,
        ))?;
    let transaction = match decision {
        RuntimePolicyDecision::Promote => challenger_transaction.transaction,
        RuntimePolicyDecision::RetainSpecialist | RuntimePolicyDecision::Reject => {
            incumbent_transaction.transaction
        }
    };
    Ok(RuntimePolicyTrialResult::Applied {
        decision,
        transaction,
    })
}

fn record_invalidation<E>(
    campaign: &ShadowCampaignSpec,
    reason: ShadowInvalidationReason,
    record: &mut impl FnMut(
        &[ShadowUpdate],
        Option<&PolicyUpdate>,
    ) -> Result<Option<RuntimePolicyDecision>, E>,
) -> Result<(), RuntimePolicyTrialError<E>> {
    record(&[ShadowUpdate::invalidate(campaign.id(), reason)], None)
        .map(|_| ())
        .map_err(RuntimePolicyTrialError::Causal)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::sync::Arc;

    use crate::intelligence::{
        CheckpointDigest, ForecastAxis, IntelligenceCore, IntelligenceLimits, ResourceVector,
        SettlementFrame, ShadowArm, ShadowInvalidationReason, ShadowUpdate, SubjectId,
        TypedOutcome,
    };
    use crate::policy::{
        RuntimePolicyDecision, RuntimePolicyKernel, RuntimePolicyRevision, RuntimePolicyState,
    };

    use super::{
        ArmExecution, NestedShadowPolicy, PairedArmOutput, RuntimePolicyArmTransaction,
        RuntimePolicyTrialError, RuntimePolicyTrialResult, ScheduledRuntimePolicyTrial,
        SchedulingToken, ShadowCampaignControls, ShadowExecutionPlan, ShadowRunResult,
        ShadowRunner, SubjectMask, VerificationSubEnvelope, run_scheduled_runtime_policy_trial,
    };

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct RestartCompleteState {
        permitted_subject: Option<SubjectMask>,
    }

    #[derive(Clone)]
    struct DownstreamCampaignState {
        permitted_subject: Option<SubjectMask>,
        proposals: Vec<u8>,
    }

    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    struct ArmObservation {
        arm: ShadowArm,
        permitted_subject: Option<SubjectMask>,
        resources: ResourceVector,
        random_stream: SubjectId,
        scheduling: SchedulingToken,
        nested_shadows: NestedShadowPolicy,
    }

    #[test]
    fn paired_campaign_arms_preserve_proposal_order_absent_the_subject_mask() {
        let subject = SubjectMask::new(SubjectId::new([1; 32]));
        let result = ShadowRunner::run(
            DownstreamCampaignState {
                permitted_subject: Some(subject),
                proposals: vec![3, 1, 4, 2],
            },
            plan(),
            |state, mask| {
                assert_eq!(*mask, subject);
                state.permitted_subject = None;
                Ok(())
            },
            |state, _| ArmExecution::Completed(state.proposals),
        );

        let ShadowRunResult::Completed(output) = result else {
            panic!("resource-matched deterministic arms must complete");
        };
        let (treatment, control) = output.into_arms();
        assert_eq!(treatment, control);
        assert_eq!(treatment, vec![3, 1, 4, 2]);
    }

    #[test]
    fn one_interrupted_campaign_arm_invalidates_the_asymmetric_pair() {
        let result = ShadowRunner::run(
            RestartCompleteState {
                permitted_subject: Some(SubjectMask::new(SubjectId::new([1; 32]))),
            },
            plan(),
            |state, _| {
                state.permitted_subject = None;
                Ok(())
            },
            |_, context| match context.arm() {
                ShadowArm::Treatment => ArmExecution::Completed(()),
                ShadowArm::Control => {
                    ArmExecution::Invalidated(ShadowInvalidationReason::Interrupted)
                }
            },
        );

        assert_eq!(
            result,
            ShadowRunResult::Invalidated(ShadowInvalidationReason::AsymmetricArms)
        );
    }

    #[test]
    fn two_interrupted_campaign_arms_retain_the_interruption_invalidation() {
        let result = ShadowRunner::run(
            RestartCompleteState {
                permitted_subject: Some(SubjectMask::new(SubjectId::new([1; 32]))),
            },
            plan(),
            |state, _| {
                state.permitted_subject = None;
                Ok(())
            },
            |_, _| ArmExecution::<()>::Invalidated(ShadowInvalidationReason::Interrupted),
        );

        assert_eq!(
            result,
            ShadowRunResult::Invalidated(ShadowInvalidationReason::Interrupted)
        );
    }

    #[test]
    fn completed_arms_share_controls_and_only_control_masks_the_subject() {
        let resources = ResourceVector::new(100, 200, 0, 300, 4);
        let random_stream = SubjectId::new([7; 32]);
        let scheduling = SchedulingToken::new([8; 32]);
        for subject in [
            SubjectMask::new(SubjectId::new([1; 32])),
            SubjectMask::new(SubjectId::new([2; 32])),
            SubjectMask::new(SubjectId::new([3; 32])),
        ] {
            let plan = ShadowExecutionPlan::new(subject, resources, random_stream, scheduling);
            let checkpoint = RestartCompleteState {
                permitted_subject: Some(subject),
            };

            let result = ShadowRunner::run(
                checkpoint,
                plan,
                |state, mask| {
                    if state.permitted_subject == Some(*mask) {
                        state.permitted_subject = None;
                        Ok(())
                    } else {
                        Err(ShadowInvalidationReason::CheckpointMismatch)
                    }
                },
                |state, context| {
                    ArmExecution::Completed(ArmObservation {
                        arm: context.arm(),
                        permitted_subject: state.permitted_subject,
                        resources: context.resources(),
                        random_stream: context.random_stream(),
                        scheduling: context.scheduling_token(),
                        nested_shadows: context.nested_shadow_policy(),
                    })
                },
            );

            assert_eq!(
                result,
                ShadowRunResult::Completed(PairedArmOutput {
                    treatment: ArmObservation {
                        arm: ShadowArm::Treatment,
                        permitted_subject: Some(subject),
                        resources,
                        random_stream,
                        scheduling,
                        nested_shadows: NestedShadowPolicy::Disabled,
                    },
                    control: ArmObservation {
                        arm: ShadowArm::Control,
                        permitted_subject: None,
                        resources,
                        random_stream,
                        scheduling,
                        nested_shadows: NestedShadowPolicy::Disabled,
                    },
                })
            );
        }
    }

    #[test]
    fn large_restart_checkpoint_is_shared_between_policy_arms() {
        let checkpoint = Arc::new(vec![7_u8; 1024 * 1024]);
        let allocation = Arc::as_ptr(&checkpoint) as usize;
        let result = ShadowRunner::run(
            checkpoint,
            plan(),
            |_, _| Ok(()),
            |restart, _| ArmExecution::Completed(Arc::as_ptr(&restart) as usize),
        );
        let ShadowRunResult::Completed(pair) = result else {
            panic!("both shared policy arms must complete")
        };

        assert_eq!(pair.into_arms(), (allocation, allocation));
    }

    #[test]
    fn a_one_sided_completion_is_explicitly_invalidated_without_partial_output() {
        let result = ShadowRunner::run(
            RestartCompleteState {
                permitted_subject: Some(SubjectMask::new(SubjectId::new([1; 32]))),
            },
            plan(),
            |state, _| {
                state.permitted_subject = None;
                Ok(())
            },
            |_, context| match context.arm() {
                ShadowArm::Treatment => ArmExecution::Completed(1),
                ShadowArm::Control => {
                    ArmExecution::Invalidated(ShadowInvalidationReason::Interrupted)
                }
            },
        );

        assert_eq!(
            result,
            ShadowRunResult::Invalidated(ShadowInvalidationReason::AsymmetricArms)
        );
    }

    #[test]
    fn matching_arm_failures_preserve_their_explicit_invalidation_reason() {
        let result = ShadowRunner::run(
            RestartCompleteState {
                permitted_subject: Some(SubjectMask::new(SubjectId::new([1; 32]))),
            },
            plan(),
            |state, _| {
                state.permitted_subject = None;
                Ok(())
            },
            |_, _| ArmExecution::<u8>::Invalidated(ShadowInvalidationReason::Interrupted),
        );

        assert_eq!(
            result,
            ShadowRunResult::Invalidated(ShadowInvalidationReason::Interrupted)
        );
    }

    #[test]
    fn completed_pair_exposes_both_authoritative_arm_outputs() {
        let result = ShadowRunner::run(
            RestartCompleteState {
                permitted_subject: Some(SubjectMask::new(SubjectId::new([1; 32]))),
            },
            plan(),
            |state, _| {
                state.permitted_subject = None;
                Ok(())
            },
            |state, _| ArmExecution::Completed(u8::from(state.permitted_subject.is_some())),
        );
        let ShadowRunResult::Completed(pair) = result else {
            panic!("both completed arms must remain a completed pair")
        };

        assert_eq!(pair.into_arms(), (1, 0));
    }

    #[test]
    fn runtime_policy_trial_masks_only_the_challenger_and_applies_dual_completion() {
        let limits = IntelligenceLimits::new(8, 16, 16, 64, 4, 64 * 1024).unwrap();
        let mut intelligence = IntelligenceCore::fresh(limits);
        let incumbent = intelligence.runtime_policy_revision();
        let challenger = intelligence.runtime_policy_challenger(7).unwrap();
        let trial = ScheduledRuntimePolicyTrial::schedule(
            incumbent,
            challenger,
            vec![1, 2, 3],
            trial_resources(),
            trial_controls(),
        )
        .unwrap()
        .unwrap();
        let observations = RefCell::new(Vec::new());
        let causal_updates = RefCell::new(Vec::new());

        let result = run_scheduled_runtime_policy_trial(
            incumbent,
            trial,
            |restart, policy, context| {
                observations
                    .borrow_mut()
                    .push((restart, policy, context.arm()));
                ArmExecution::Completed(RuntimePolicyArmTransaction::new(
                    context.arm(),
                    match context.arm() {
                        ShadowArm::Treatment => ResourceVector::new(90, 100, 90, 100, 9),
                        ShadowArm::Control => ResourceVector::new(100, 100, 100, 100, 10),
                    },
                    trial_outcomes(context.arm()),
                ))
            },
            |updates, policy_update| {
                let mut frame = SettlementFrame::observations(&[], &[], &[], updates);
                if let Some(update) = policy_update {
                    frame = frame.with_policy_update(update);
                }
                let transition = intelligence.stage(frame).unwrap();
                let decision = transition.policy_decision();
                transition.commit(&mut intelligence).unwrap();
                causal_updates
                    .borrow_mut()
                    .push((updates.to_vec(), intelligence.runtime_policy_revision()));
                Ok::<_, ()>(decision)
            },
        )
        .unwrap();

        assert_eq!(
            result,
            RuntimePolicyTrialResult::Applied {
                decision: RuntimePolicyDecision::Promote,
                transaction: ShadowArm::Treatment,
            }
        );
        assert_eq!(intelligence.runtime_policy_revision(), challenger);
        let causal_updates = causal_updates.into_inner();
        assert_eq!(causal_updates.len(), 2);
        assert!(matches!(
            causal_updates[0].0.as_slice(),
            [ShadowUpdate::Open(_)]
        ));
        assert_eq!(causal_updates[0].1, incumbent);
        assert!(matches!(
            causal_updates[1].0.as_slice(),
            [ShadowUpdate::Outcome(_), ShadowUpdate::Outcome(_)]
        ));
        assert_eq!(causal_updates[1].1, challenger);
        assert_eq!(
            observations.into_inner(),
            vec![
                (vec![1, 2, 3], challenger, ShadowArm::Treatment),
                (vec![1, 2, 3], incumbent, ShadowArm::Control),
            ]
        );
    }

    #[test]
    fn deterministic_challenger_rotation_reaches_every_verified_policy_dimension() {
        let state = RuntimePolicyState::bootstrap();
        let expected = RuntimePolicyKernel::neighborhood(state.active())
            .into_iter()
            .map(RuntimePolicyRevision::identity)
            .collect::<std::collections::BTreeSet<_>>();
        let selected = (0..u64::try_from(expected.len()).unwrap())
            .filter_map(|sequence| state.challenger_at(sequence))
            .map(RuntimePolicyRevision::identity)
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(selected, expected);
    }

    #[test]
    fn every_eligible_policy_challenger_changes_the_exercised_cohort_field() {
        let state = RuntimePolicyState::bootstrap();
        let incumbent = state.active();
        assert!(
            RuntimePolicyKernel::neighborhood(incumbent)
                .into_iter()
                .all(|challenger| challenger.verification_cohort()
                    != incumbent.verification_cohort())
        );
    }

    #[test]
    fn incomplete_runtime_policy_pair_is_invalidated_without_state_change() {
        let state = RuntimePolicyState::bootstrap();
        let original = state.clone();
        let trial = ScheduledRuntimePolicyTrial::schedule(
            state.active(),
            state.challenger_at(0).unwrap(),
            [9; 32],
            trial_resources(),
            trial_controls(),
        )
        .unwrap()
        .unwrap();

        let causal_updates = RefCell::new(Vec::new());
        let result = run_scheduled_runtime_policy_trial(
            state.active(),
            trial,
            |_, _, context| match context.arm() {
                ShadowArm::Treatment => ArmExecution::Completed(RuntimePolicyArmTransaction::new(
                    (),
                    ResourceVector::new(90, 100, 0, 100, 9),
                    trial_outcomes(context.arm()),
                )),
                ShadowArm::Control => {
                    ArmExecution::Invalidated(ShadowInvalidationReason::Interrupted)
                }
            },
            |updates, policy_update| {
                causal_updates
                    .borrow_mut()
                    .push((updates.to_vec(), policy_update.is_some()));
                Ok::<_, ()>(None)
            },
        )
        .unwrap();

        assert_eq!(
            result,
            RuntimePolicyTrialResult::Invalidated(ShadowInvalidationReason::AsymmetricArms)
        );
        assert_eq!(state, original);
        let causal_updates = causal_updates.into_inner();
        assert_eq!(causal_updates.len(), 2);
        assert!(matches!(
            causal_updates[0].0.as_slice(),
            [ShadowUpdate::Open(_)]
        ));
        assert!(matches!(
            causal_updates[1].0.as_slice(),
            [ShadowUpdate::Invalidate { .. }]
        ));
        assert!(causal_updates.iter().all(|(_, has_policy)| !has_policy));
    }

    #[test]
    fn runtime_policy_trial_requires_a_bounded_nonzero_sub_envelope() {
        assert!(
            VerificationSubEnvelope::protected(ResourceVector::new(0, 200, 0, 300, 4), 1_000,)
                .is_none()
        );
    }

    #[test]
    fn runtime_policy_trial_reserves_both_arm_request_allowances_before_open() {
        let state = RuntimePolicyState::bootstrap();
        let resources = VerificationSubEnvelope::protected(
            ResourceVector::new(2_000, 200, 0, 3_000, 18),
            1_000,
        )
        .unwrap();
        let trial = ScheduledRuntimePolicyTrial::schedule(
            state.active(),
            state.challenger_at(0).unwrap(),
            (),
            resources,
            trial_controls(),
        )
        .unwrap()
        .unwrap();

        assert_eq!(trial.reserved_verification_requests(), 18);
    }

    #[test]
    fn policy_shadow_open_publication_failure_dispatches_neither_arm() {
        let state = RuntimePolicyState::bootstrap();
        let resources = VerificationSubEnvelope::protected(
            ResourceVector::new(2_000, 200, 0, 3_000, 18),
            1_000,
        )
        .unwrap();
        let trial = ScheduledRuntimePolicyTrial::schedule(
            state.active(),
            state.challenger_at(0).unwrap(),
            (),
            resources,
            trial_controls(),
        )
        .unwrap()
        .unwrap();
        let mut opened = None;

        let result: Result<RuntimePolicyTrialResult<()>, RuntimePolicyTrialError<&str>> =
            run_scheduled_runtime_policy_trial(
                state.active(),
                trial,
                |(), _, _| panic!("an arm cannot dispatch before Shadow Open is durable"),
                |updates, _| {
                    let [ShadowUpdate::Open(campaign)] = updates else {
                        panic!("the first publication must be the exact Shadow Open")
                    };
                    opened = Some(campaign.resources().verification_requests.saturating_mul(2));
                    Err::<Option<RuntimePolicyDecision>, _>("durability barrier failed")
                },
            );

        assert!(matches!(result, Err(RuntimePolicyTrialError::Causal(_))));
        assert_eq!(opened, Some(18));
    }

    #[test]
    fn resource_mismatch_discards_both_transactions_and_preserves_policy_state() {
        let state = RuntimePolicyState::bootstrap();
        let original = state.clone();
        let trial = ScheduledRuntimePolicyTrial::schedule(
            state.active(),
            state.challenger_at(0).unwrap(),
            (),
            trial_resources(),
            trial_controls(),
        )
        .unwrap()
        .unwrap();

        let result = run_scheduled_runtime_policy_trial(
            state.active(),
            trial,
            |(), _, context| {
                let usage = match context.arm() {
                    ShadowArm::Treatment => ResourceVector::new(1_001, 100, 0, 100, 9),
                    ShadowArm::Control => ResourceVector::new(90, 100, 0, 100, 9),
                };
                ArmExecution::Completed(RuntimePolicyArmTransaction::new(
                    context.arm(),
                    usage,
                    trial_outcomes(context.arm()),
                ))
            },
            |_, _| Ok::<_, ()>(None),
        )
        .unwrap();

        assert_eq!(
            result,
            RuntimePolicyTrialResult::Invalidated(ShadowInvalidationReason::ResourceMismatch)
        );
        assert_eq!(state, original);
    }

    fn plan() -> ShadowExecutionPlan {
        ShadowExecutionPlan::new(
            SubjectMask::new(SubjectId::new([1; 32])),
            ResourceVector::new(10, 20, 0, 30, 1),
            SubjectId::new([4; 32]),
            SchedulingToken::new([5; 32]),
        )
    }

    fn trial_resources() -> VerificationSubEnvelope {
        VerificationSubEnvelope::protected(ResourceVector::new(2_000, 200, 200, 3_000, 200), 1_000)
            .unwrap()
    }

    fn trial_axes() -> [ForecastAxis; 5] {
        [
            ForecastAxis::KernelAcceptance,
            ForecastAxis::ImmediateImprovement,
            ForecastAxis::UsefulDescendants,
            ForecastAxis::CrossGoalLeverage,
            ForecastAxis::VerificationCost,
        ]
    }

    fn trial_controls() -> ShadowCampaignControls {
        ShadowCampaignControls::new(
            CheckpointDigest::new([3; 32]),
            SubjectId::new([4; 32]),
            SchedulingToken::new([5; 32]),
            trial_axes(),
        )
    }

    fn trial_outcomes(arm: ShadowArm) -> Vec<TypedOutcome> {
        let _ = arm;
        vec![
            TypedOutcome::new(ForecastAxis::KernelAcceptance, 0.0).unwrap(),
            TypedOutcome::new(ForecastAxis::ImmediateImprovement, 1.0).unwrap(),
            TypedOutcome::new(ForecastAxis::UsefulDescendants, 0.0).unwrap(),
            TypedOutcome::new(ForecastAxis::CrossGoalLeverage, 1.0).unwrap(),
            TypedOutcome::new(ForecastAxis::VerificationCost, 9.0).unwrap(),
        ]
    }
}
