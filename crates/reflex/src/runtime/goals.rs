use std::cmp::Ordering;
use std::collections::HashSet;

use sha2::{Digest, Sha256};

use crate::domain::DomainDefinition;
use crate::goal::{Direction, GoalSet, OptimizationGoal, ThresholdRelation};
use crate::measurement::{MeasurementSpace, MetricOrdering};
use crate::session::{GoalId, SessionError, VerifiedArtifact};

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

pub(super) struct ParentPreferenceRanks {
    keys: Vec<crate::ArtifactKey>,
    dominated_by: Vec<usize>,
    ranks: Vec<usize>,
}

impl ParentPreferenceRanks {
    pub(super) fn as_slice(&self) -> &[usize] {
        &self.ranks
    }

    fn rebuild(&mut self) {
        let mut order = (0..self.keys.len()).collect::<Vec<_>>();
        order.sort_unstable_by_key(|index| (self.dominated_by[*index], *index));
        self.ranks.resize(self.keys.len(), 0);
        for (rank, index) in order.into_iter().enumerate() {
            self.ranks[index] = rank;
        }
    }
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
            .map(|goal| retain_pareto(self.domain, goal, known))
            .collect()
    }

    pub(super) fn pareto_from_frontiers(
        frontiers: &[Vec<VerifiedArtifact<D>>],
    ) -> Vec<VerifiedArtifact<D>> {
        let mut pareto = Vec::new();
        let mut keys = HashSet::new();
        for frontier in frontiers {
            for artifact in frontier {
                if keys.insert(artifact.key()) {
                    pareto.push(artifact.clone());
                }
            }
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
                let current_keys = current
                    .iter()
                    .map(VerifiedArtifact::key)
                    .collect::<HashSet<_>>();
                let changed = previous.len() != current.len()
                    || previous
                        .iter()
                        .any(|artifact| !current_keys.contains(&artifact.key()));
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

    pub(super) fn parent_ranks(
        &self,
        frontier: &[(VerifiedArtifact<D>, usize)],
    ) -> ParentPreferenceRanks {
        let mut ranks = ParentPreferenceRanks {
            keys: Vec::new(),
            dominated_by: Vec::new(),
            ranks: Vec::new(),
        };
        self.extend_parent_ranks(frontier, &mut ranks);
        ranks
    }

    pub(super) fn extend_parent_ranks(
        &self,
        frontier: &[(VerifiedArtifact<D>, usize)],
        ranks: &mut ParentPreferenceRanks,
    ) {
        assert!(
            ranks.keys.len() <= frontier.len(),
            "Search Frontier cannot shrink while parent preference ranks are live"
        );
        let retained = ranks.keys.len();
        assert!(
            ranks
                .keys
                .iter()
                .zip(frontier)
                .all(|(key, (artifact, _))| *key == artifact.key()),
            "Search Frontier prefix cannot change while parent preference ranks are live"
        );
        ranks.keys.extend(
            frontier[retained..]
                .iter()
                .map(|(artifact, _)| artifact.key()),
        );
        ranks.dominated_by.resize(frontier.len(), 0);
        for right in retained..frontier.len() {
            for left in 0..right {
                match self.compare_parent_artifacts(&frontier[left].0, &frontier[right].0) {
                    Ordering::Less => ranks.dominated_by[right] += 1,
                    Ordering::Greater => ranks.dominated_by[left] += 1,
                    Ordering::Equal => {}
                }
            }
        }
        ranks.rebuild();
    }

    fn compare_parent_artifacts(
        &self,
        left_parent: &VerifiedArtifact<D>,
        right_parent: &VerifiedArtifact<D>,
    ) -> Ordering {
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

#[cfg(test)]
fn linearize_partial_order(
    count: usize,
    mut compare: impl FnMut(usize, usize) -> Ordering,
) -> Vec<usize> {
    let dominated_by = (0..count)
        .map(|right| {
            (0..count)
                .filter(|left| *left != right && compare(*left, right) == Ordering::Less)
                .count()
        })
        .collect::<Vec<_>>();
    let mut order = (0..count).collect::<Vec<_>>();
    order.sort_unstable_by_key(|index| (dominated_by[*index], *index));
    let mut ranks = vec![0; count];
    for (rank, index) in order.into_iter().enumerate() {
        ranks[index] = rank;
    }
    ranks
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
    artifacts: &[VerifiedArtifact<D>],
) -> Vec<VerifiedArtifact<D>> {
    let artifacts = artifacts
        .iter()
        .filter(|artifact| {
            goal.constraints
                .iter()
                .all(|constraint| threshold_satisfied(domain, artifact, constraint))
        })
        .cloned()
        .collect::<Vec<_>>();
    let groups = artifacts
        .iter()
        .map(|artifact| artifact.inner.claim_digest)
        .collect::<Vec<_>>();
    let retained = retain_nondominated(&groups, |left, right| {
        dominates(domain, goal, &artifacts[left], &artifacts[right])
    });
    artifacts
        .into_iter()
        .zip(retained)
        .filter_map(|(artifact, retained)| retained.then_some(artifact))
        .collect()
}

fn retain_nondominated(
    groups: &[[u8; 32]],
    mut dominates: impl FnMut(usize, usize) -> bool,
) -> Vec<bool> {
    let mut retained = vec![true; groups.len()];
    let mut indices = (0..groups.len()).collect::<Vec<_>>();
    indices.sort_unstable_by_key(|index| groups[*index]);
    for group in indices.chunk_by(|left, right| groups[*left] == groups[*right]) {
        for right in group.iter().copied() {
            retained[right] = !group
                .iter()
                .copied()
                .any(|left| left != right && dominates(left, right));
        }
    }
    retained
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
    _domain: &D,
    left: &VerifiedArtifact<D>,
    right: &VerifiedArtifact<D>,
) -> bool {
    left.inner.claim_canonical == right.inner.claim_canonical
}

#[cfg(test)]
mod tests {
    use super::{linearize_partial_order, retain_nondominated};

    #[test]
    fn partial_preference_is_linearized_before_it_reaches_a_sort_comparator() {
        let partial = |left: usize, right: usize| match (left, right) {
            (0, 1) => std::cmp::Ordering::Less,
            (1, 0) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        };

        let ranks = linearize_partial_order(3, partial);

        assert_eq!(
            ranks
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>()
                .len(),
            3
        );
        assert!(
            ranks[0] < ranks[1],
            "strict preference must survive linearization"
        );
    }

    #[test]
    fn claim_grouped_retention_matches_the_quadratic_definition_in_input_order() {
        let groups = (0..257_u16)
            .map(|index| {
                let mut group = [0_u8; 32];
                group[0] = u8::try_from(index % 17).unwrap();
                group
            })
            .collect::<Vec<_>>();
        let measurements = (0..groups.len())
            .map(|index| {
                let first = (index * 37 + index / 3) % 101;
                let second = (index * 19 + index / 7) % 89;
                (first, second)
            })
            .collect::<Vec<_>>();
        let dominates = |left: usize, right: usize| {
            groups[left] == groups[right]
                && measurements[left].0 <= measurements[right].0
                && measurements[left].1 <= measurements[right].1
                && measurements[left] != measurements[right]
        };
        let expected = (0..groups.len())
            .map(|right| (0..groups.len()).any(|left| left != right && dominates(left, right)))
            .map(|dominated| !dominated)
            .collect::<Vec<_>>();
        assert_eq!(retain_nondominated(&groups, dominates), expected);
    }
}
