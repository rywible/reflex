use std::cmp::Ordering;

use sha2::{Digest, Sha256};

use crate::domain::{DomainDefinition, VerificationKernel};
use crate::goal::{Direction, GoalSet, OptimizationGoal, ThresholdRelation};
use crate::measurement::{MeasurementSpace, MetricOrdering};
use crate::session::{GoalId, SessionError, VerifiedArtifact};

use super::ProposedCandidate;

/// The operational interpretation of a caller-supplied Goal Set.
///
/// Runtime phases use this single interface for validation, stable identity,
/// Preference ordering, eligibility, Pareto membership, affected-Goal
/// reporting, and Success Conditions. Preference is never scalarized.
pub(super) struct GoalEvaluator<'a, D: DomainDefinition> {
    domain: &'a D,
    goals: &'a GoalSet<D>,
    ids: Vec<GoalId>,
}

impl<'a, D: DomainDefinition> GoalEvaluator<'a, D> {
    pub(super) fn new(
        domain: &'a D,
        goals: &'a GoalSet<D>,
    ) -> Result<Self, SessionError<D::Error>> {
        validate_goals(domain, goals)?;
        let ids = goal_ids(domain, goals)?;
        Ok(Self { domain, goals, ids })
    }

    pub(super) fn frontiers(&self, known: &[VerifiedArtifact<D>]) -> Vec<Vec<VerifiedArtifact<D>>> {
        self.goals
            .goals
            .iter()
            .map(|goal| retain_pareto(self.domain, goal, known.to_vec()))
            .collect()
    }

    pub(super) fn pareto(&self, known: &[VerifiedArtifact<D>]) -> Vec<VerifiedArtifact<D>> {
        let mut pareto = Vec::new();
        for frontier in self.frontiers(known) {
            extend_unique(&mut pareto, frontier);
        }
        pareto
    }

    pub(super) fn affected(
        &self,
        previous: &[Vec<VerifiedArtifact<D>>],
        current: &[Vec<VerifiedArtifact<D>>],
    ) -> Vec<GoalId> {
        self.ids
            .iter()
            .zip(previous.iter().zip(current))
            .filter_map(|(id, (previous, current))| {
                let changed = previous.len() != current.len()
                    || previous
                        .iter()
                        .any(|artifact| !current.iter().any(|item| item.key() == artifact.key()));
                changed.then_some(*id)
            })
            .collect()
    }

    pub(super) fn success_satisfied(&self, frontiers: &[Vec<VerifiedArtifact<D>>]) -> bool {
        self.goals
            .goals
            .iter()
            .zip(frontiers)
            .all(|(goal, frontier)| {
                let Some(success) = goal.success.as_ref() else {
                    return false;
                };
                frontier.iter().any(|artifact| {
                    success
                        .thresholds
                        .iter()
                        .all(|threshold| threshold_satisfied(self.domain, artifact, threshold))
                })
            })
    }

    pub(super) fn compare_parents(
        &self,
        frontier: &[(VerifiedArtifact<D>, usize)],
        left: &ProposedCandidate<D>,
        right: &ProposedCandidate<D>,
    ) -> Ordering {
        let Some(left_parent) = frontier
            .get(left.candidate.source_index)
            .map(|parent| &parent.0)
        else {
            return Ordering::Equal;
        };
        let Some(right_parent) = frontier
            .get(right.candidate.source_index)
            .map(|parent| &parent.0)
        else {
            return Ordering::Equal;
        };
        let mut left_better = false;
        let mut right_better = false;
        for goal in self.goals.goals.iter() {
            match compare_preference(self.domain, goal, left_parent, right_parent) {
                Ordering::Less => left_better = true,
                Ordering::Greater => right_better = true,
                Ordering::Equal => {}
            }
        }
        match (left_better, right_better) {
            (true, false) => Ordering::Less,
            (false, true) => Ordering::Greater,
            _ => Ordering::Equal,
        }
    }

    pub(super) fn encode_set(
        domain: &D,
        goals: &GoalSet<D>,
    ) -> Result<Vec<u8>, SessionError<D::Error>> {
        let mut output = Vec::new();
        super::push_u64(&mut output, goals.goals.as_slice().len() as u64);
        for goal in goals.goals.iter() {
            encode_goal(domain, goal, &mut output)?;
        }
        Ok(output)
    }
}

fn goal_ids<D: DomainDefinition>(
    domain: &D,
    goals: &GoalSet<D>,
) -> Result<Vec<GoalId>, SessionError<D::Error>> {
    goals
        .goals
        .iter()
        .map(|goal| {
            let mut encoded = Vec::new();
            encode_goal(domain, goal, &mut encoded)?;
            let mut digest = Sha256::new();
            digest.update(b"reflex-goal-v1\0");
            digest.update(encoded);
            Ok(GoalId(digest.finalize().into()))
        })
        .collect()
}

fn validate_goals<D: DomainDefinition>(
    domain: &D,
    goals: &GoalSet<D>,
) -> Result<(), SessionError<D::Error>> {
    let known = domain.measurements().schema();
    for goal in goals.goals.iter() {
        for metric in goal
            .objectives
            .iter()
            .map(|objective| objective.metric)
            .chain(goal.constraints.iter().map(|constraint| constraint.metric))
            .chain(
                goal.preference
                    .tolerances
                    .iter()
                    .map(|tolerance| tolerance.metric),
            )
            .chain(
                goal.success
                    .iter()
                    .flat_map(|success| success.thresholds.iter())
                    .map(|threshold| threshold.metric),
            )
        {
            if !known.iter().any(|descriptor| descriptor.metric() == metric) {
                return Err(SessionError::InvalidGoal(crate::GoalError::UnknownMetric));
            }
        }
        for constraint in goal.constraints.iter().chain(
            goal.success
                .iter()
                .flat_map(|success| success.thresholds.iter()),
        ) {
            validate_observation(domain, constraint.metric, &constraint.threshold)?;
        }
        for tolerance in &goal.preference.tolerances {
            validate_observation(domain, tolerance.metric, &tolerance.amount)?;
            domain
                .measurements()
                .within_tolerance(
                    tolerance.metric,
                    &tolerance.amount,
                    &tolerance.amount,
                    &tolerance.amount,
                )
                .map_err(|_| SessionError::InvalidGoal(crate::GoalError::InvalidObservation))?;
        }
        let thresholds = goal
            .constraints
            .iter()
            .map(|threshold| (threshold, false))
            .chain(
                goal.success
                    .iter()
                    .flat_map(|success| success.thresholds.iter())
                    .map(|threshold| (threshold, true)),
            )
            .collect::<Vec<_>>();
        for (index, (left, left_is_success)) in thresholds.iter().enumerate() {
            for (right, right_is_success) in &thresholds[index + 1..] {
                if left.metric != right.metric || left.relation == right.relation {
                    continue;
                }
                let ordering = domain
                    .measurements()
                    .compare(left.metric, &left.threshold, &right.threshold)
                    .map_err(|_| SessionError::InvalidGoal(crate::GoalError::InvalidObservation))?;
                let incompatible = matches!(
                    (left.relation, right.relation, ordering),
                    (
                        ThresholdRelation::AtMost,
                        ThresholdRelation::AtLeast,
                        MetricOrdering::Less
                    ) | (
                        ThresholdRelation::AtLeast,
                        ThresholdRelation::AtMost,
                        MetricOrdering::Greater
                    )
                );
                if incompatible {
                    return Err(SessionError::InvalidGoal(
                        if *left_is_success || *right_is_success {
                            crate::GoalError::IncompatibleSuccessCondition
                        } else {
                            crate::GoalError::IncompatibleConstraints
                        },
                    ));
                }
            }
        }
    }
    Ok(())
}

fn validate_observation<D: DomainDefinition>(
    domain: &D,
    metric: D::Metric,
    observation: &D::Observation,
) -> Result<(), SessionError<D::Error>> {
    let mut encoded = Vec::new();
    domain
        .measurements()
        .encode_observation(metric, observation, &mut encoded)
        .map_err(SessionError::Domain)?;
    let decoded = domain
        .measurements()
        .decode_observation(metric, &encoded)
        .map_err(SessionError::Domain)?;
    let mut canonical = Vec::new();
    domain
        .measurements()
        .encode_observation(metric, &decoded, &mut canonical)
        .map_err(SessionError::Domain)?;
    if encoded != canonical
        || domain.measurements().compare(metric, observation, &decoded) != Ok(MetricOrdering::Equal)
    {
        return Err(SessionError::InvalidGoal(
            crate::GoalError::InvalidObservation,
        ));
    }
    Ok(())
}

fn encode_goal<D: DomainDefinition>(
    domain: &D,
    goal: &OptimizationGoal<D>,
    output: &mut Vec<u8>,
) -> Result<(), SessionError<D::Error>> {
    super::push_u64(output, goal.constraints.len() as u64);
    for constraint in &goal.constraints {
        encode_metric(domain, constraint.metric, output)?;
        output.push(match constraint.relation {
            ThresholdRelation::AtMost => 0,
            ThresholdRelation::AtLeast => 1,
        });
        encode_observation(domain, constraint.metric, &constraint.threshold, output)?;
    }
    super::push_u64(output, goal.objectives.as_slice().len() as u64);
    for objective in goal.objectives.iter() {
        encode_metric(domain, objective.metric, output)?;
        output.push(match objective.direction {
            Direction::Minimize => 0,
            Direction::Maximize => 1,
        });
    }
    super::push_u64(
        output,
        goal.preference.priority_tiers.as_slice().len() as u64,
    );
    for tier in goal.preference.priority_tiers.iter() {
        super::push_u64(output, tier.as_slice().len() as u64);
        for metric in tier.iter() {
            encode_metric(domain, *metric, output)?;
        }
    }
    super::push_u64(output, goal.preference.tolerances.len() as u64);
    for tolerance in &goal.preference.tolerances {
        encode_metric(domain, tolerance.metric, output)?;
        encode_observation(domain, tolerance.metric, &tolerance.amount, output)?;
    }
    match &goal.success {
        Some(success) => {
            output.push(1);
            super::push_u64(output, success.thresholds.as_slice().len() as u64);
            for threshold in success.thresholds.iter() {
                encode_metric(domain, threshold.metric, output)?;
                output.push(match threshold.relation {
                    ThresholdRelation::AtMost => 0,
                    ThresholdRelation::AtLeast => 1,
                });
                encode_observation(domain, threshold.metric, &threshold.threshold, output)?;
            }
        }
        None => output.push(0),
    }
    Ok(())
}

fn encode_metric<D: DomainDefinition>(
    domain: &D,
    metric: D::Metric,
    output: &mut Vec<u8>,
) -> Result<(), SessionError<D::Error>> {
    let descriptor = domain
        .measurements()
        .schema()
        .iter()
        .find(|descriptor| descriptor.metric() == metric)
        .ok_or(SessionError::InvalidGoal(crate::GoalError::UnknownMetric))?;
    super::push_bytes(output, descriptor.symbol().as_str().as_bytes());
    Ok(())
}

fn encode_observation<D: DomainDefinition>(
    domain: &D,
    metric: D::Metric,
    observation: &D::Observation,
    output: &mut Vec<u8>,
) -> Result<(), SessionError<D::Error>> {
    let mut encoded = Vec::new();
    domain
        .measurements()
        .encode_observation(metric, observation, &mut encoded)
        .map_err(SessionError::Domain)?;
    super::push_bytes(output, &encoded);
    Ok(())
}

fn compare_preference<D: DomainDefinition>(
    domain: &D,
    goal: &OptimizationGoal<D>,
    left: &VerifiedArtifact<D>,
    right: &VerifiedArtifact<D>,
) -> Ordering {
    if !goal
        .constraints
        .iter()
        .all(|constraint| threshold_satisfied(domain, left, constraint))
        || !goal
            .constraints
            .iter()
            .all(|constraint| threshold_satisfied(domain, right, constraint))
    {
        return Ordering::Equal;
    }
    for tier in goal.preference.priority_tiers.iter() {
        let mut left_better = false;
        let mut right_better = false;
        for metric in tier.iter() {
            let Some(objective) = goal
                .objectives
                .iter()
                .find(|objective| objective.metric == *metric)
            else {
                return Ordering::Equal;
            };
            let Some(left_measurement) = left
                .inner
                .measurements
                .iter()
                .find(|measurement| measurement.metric == *metric)
            else {
                return Ordering::Equal;
            };
            let Some(right_measurement) = right
                .inner
                .measurements
                .iter()
                .find(|measurement| measurement.metric == *metric)
            else {
                return Ordering::Equal;
            };
            if !domain.measurements().environments_compatible(
                *metric,
                &left.inner.environment,
                &right.inner.environment,
            ) {
                return Ordering::Equal;
            }
            if let Some(tolerance) = goal
                .preference
                .tolerances
                .iter()
                .find(|tolerance| tolerance.metric == *metric)
                && domain
                    .measurements()
                    .within_tolerance(
                        *metric,
                        &left_measurement.observation,
                        &right_measurement.observation,
                        &tolerance.amount,
                    )
                    .unwrap_or(false)
            {
                continue;
            }
            let Ok(ordering) = domain.measurements().compare(
                *metric,
                &left_measurement.observation,
                &right_measurement.observation,
            ) else {
                return Ordering::Equal;
            };
            let ordering = match (objective.direction, ordering) {
                (Direction::Minimize, ordering) => ordering,
                (Direction::Maximize, MetricOrdering::Equal) => MetricOrdering::Equal,
                (Direction::Maximize, MetricOrdering::Less) => MetricOrdering::Greater,
                (Direction::Maximize, MetricOrdering::Greater) => MetricOrdering::Less,
            };
            left_better |= ordering == MetricOrdering::Less;
            right_better |= ordering == MetricOrdering::Greater;
        }
        match (left_better, right_better) {
            (true, false) => return Ordering::Less,
            (false, true) => return Ordering::Greater,
            (true, true) => return Ordering::Equal,
            (false, false) => {}
        }
    }
    Ordering::Equal
}

fn retain_pareto<D: DomainDefinition>(
    domain: &D,
    goal: &OptimizationGoal<D>,
    artifacts: Vec<VerifiedArtifact<D>>,
) -> Vec<VerifiedArtifact<D>> {
    let artifacts = artifacts
        .into_iter()
        .filter(|artifact| {
            goal.constraints
                .iter()
                .all(|constraint| threshold_satisfied(domain, artifact, constraint))
        })
        .collect::<Vec<_>>();
    let dominated = (0..artifacts.len())
        .map(|right| {
            (0..artifacts.len()).any(|left| {
                left != right && dominates(domain, goal, &artifacts[left], &artifacts[right])
            })
        })
        .collect::<Vec<_>>();
    artifacts
        .into_iter()
        .zip(dominated)
        .filter_map(|(artifact, dominated)| (!dominated).then_some(artifact))
        .collect()
}

fn threshold_satisfied<D: DomainDefinition>(
    domain: &D,
    artifact: &VerifiedArtifact<D>,
    threshold: &crate::MeasurementConstraint<D>,
) -> bool {
    let Some(measurement) = artifact
        .inner
        .measurements
        .iter()
        .find(|measurement| measurement.metric == threshold.metric)
    else {
        return false;
    };
    let Ok(ordering) = domain.measurements().compare(
        threshold.metric,
        &measurement.observation,
        &threshold.threshold,
    ) else {
        return false;
    };
    matches!(
        (threshold.relation, ordering),
        (
            ThresholdRelation::AtMost,
            MetricOrdering::Less | MetricOrdering::Equal
        ) | (
            ThresholdRelation::AtLeast,
            MetricOrdering::Greater | MetricOrdering::Equal
        )
    )
}

fn dominates<D: DomainDefinition>(
    domain: &D,
    goal: &OptimizationGoal<D>,
    left: &VerifiedArtifact<D>,
    right: &VerifiedArtifact<D>,
) -> bool {
    if !same_correctness_claim(domain, left, right) {
        return false;
    }
    let mut strictly_better = false;
    for objective in goal.objectives.iter() {
        let Some(left_value) = left
            .inner
            .measurements
            .iter()
            .find(|measurement| measurement.metric == objective.metric)
        else {
            return false;
        };
        let Some(right_value) = right
            .inner
            .measurements
            .iter()
            .find(|measurement| measurement.metric == objective.metric)
        else {
            return false;
        };
        let Ok(ordering) = domain.measurements().compare(
            objective.metric,
            &left_value.observation,
            &right_value.observation,
        ) else {
            return false;
        };
        let better = matches!(
            (objective.direction, ordering),
            (Direction::Minimize, MetricOrdering::Less)
                | (Direction::Maximize, MetricOrdering::Greater)
        );
        let worse = matches!(
            (objective.direction, ordering),
            (Direction::Minimize, MetricOrdering::Greater)
                | (Direction::Maximize, MetricOrdering::Less)
        );
        if worse {
            return false;
        }
        strictly_better |= better;
    }
    strictly_better
}

fn same_correctness_claim<D: DomainDefinition>(
    domain: &D,
    left: &VerifiedArtifact<D>,
    right: &VerifiedArtifact<D>,
) -> bool {
    let mut left_claim = Vec::new();
    let mut right_claim = Vec::new();
    domain
        .kernel()
        .encode_claim(&left.inner.verification.claim, &mut left_claim)
        .is_ok()
        && domain
            .kernel()
            .encode_claim(&right.inner.verification.claim, &mut right_claim)
            .is_ok()
        && left_claim == right_claim
}

fn extend_unique<D: DomainDefinition>(
    known: &mut Vec<VerifiedArtifact<D>>,
    artifacts: impl IntoIterator<Item = VerifiedArtifact<D>>,
) {
    for artifact in artifacts {
        if !known.iter().any(|known| known.key() == artifact.key()) {
            known.push(artifact);
        }
    }
}
