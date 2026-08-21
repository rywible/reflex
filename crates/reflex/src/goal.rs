use std::fmt;

use crate::DomainDefinition;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NonEmpty<T> {
    values: Vec<T>,
}

impl<T> NonEmpty<T> {
    #[must_use]
    pub fn one(value: T) -> Self {
        Self {
            values: vec![value],
        }
    }

    pub fn try_from_iter(values: impl IntoIterator<Item = T>) -> Result<Self, EmptyCollection> {
        let values = values.into_iter().collect::<Vec<_>>();
        if values.is_empty() {
            Err(EmptyCollection)
        } else {
            Ok(Self { values })
        }
    }

    #[must_use]
    pub fn as_slice(&self) -> &[T] {
        &self.values
    }

    #[must_use]
    pub fn into_vec(self) -> Vec<T> {
        self.values
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.values.iter()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyCollection;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EmptyGoalSet;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Minimize,
    Maximize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ThresholdRelation {
    AtMost,
    AtLeast,
}

pub struct MeasurementConstraint<D: DomainDefinition> {
    pub(crate) metric: D::Metric,
    pub(crate) relation: ThresholdRelation,
    pub(crate) threshold: D::Observation,
}

impl<D: DomainDefinition> MeasurementConstraint<D> {
    #[must_use]
    pub fn new(metric: D::Metric, relation: ThresholdRelation, threshold: D::Observation) -> Self {
        Self {
            metric,
            relation,
            threshold,
        }
    }

    #[must_use]
    pub fn metric(&self) -> D::Metric {
        self.metric
    }

    #[must_use]
    pub fn relation(&self) -> ThresholdRelation {
        self.relation
    }

    #[must_use]
    pub fn threshold(&self) -> &D::Observation {
        &self.threshold
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Objective<D: DomainDefinition> {
    pub(crate) metric: D::Metric,
    pub(crate) direction: Direction,
}

impl<D: DomainDefinition> Objective<D> {
    #[must_use]
    pub fn new(metric: D::Metric, direction: Direction) -> Self {
        Self { metric, direction }
    }
}

pub struct MeasurementTolerance<D: DomainDefinition> {
    pub(crate) metric: D::Metric,
    pub(crate) amount: D::Observation,
}

impl<D: DomainDefinition> MeasurementTolerance<D> {
    #[must_use]
    pub fn new(metric: D::Metric, amount: D::Observation) -> Self {
        Self { metric, amount }
    }

    #[must_use]
    pub fn metric(&self) -> D::Metric {
        self.metric
    }

    #[must_use]
    pub fn amount(&self) -> &D::Observation {
        &self.amount
    }
}

pub struct Preference<D: DomainDefinition> {
    pub(crate) priority_tiers: NonEmpty<NonEmpty<D::Metric>>,
    pub(crate) tolerances: Vec<MeasurementTolerance<D>>,
}

impl<D: DomainDefinition> Preference<D> {
    pub fn tiered(
        priority_tiers: NonEmpty<NonEmpty<D::Metric>>,
        tolerances: impl IntoIterator<Item = MeasurementTolerance<D>>,
    ) -> Result<Self, GoalError> {
        let mut seen = Vec::new();
        for metric in priority_tiers.iter().flat_map(NonEmpty::iter) {
            if seen.contains(metric) {
                return Err(GoalError::DuplicatePreferenceMetric);
            }
            seen.push(*metric);
        }
        Ok(Self {
            priority_tiers,
            tolerances: tolerances.into_iter().collect(),
        })
    }

    #[must_use]
    pub fn priority_tiers(&self) -> &NonEmpty<NonEmpty<D::Metric>> {
        &self.priority_tiers
    }

    #[must_use]
    pub fn tolerances(&self) -> &[MeasurementTolerance<D>] {
        &self.tolerances
    }
}

pub struct SuccessCondition<D: DomainDefinition> {
    pub(crate) thresholds: NonEmpty<MeasurementConstraint<D>>,
}

impl<D: DomainDefinition> SuccessCondition<D> {
    #[must_use]
    pub fn all(thresholds: NonEmpty<MeasurementConstraint<D>>) -> Self {
        Self { thresholds }
    }

    #[must_use]
    pub fn thresholds(&self) -> &NonEmpty<MeasurementConstraint<D>> {
        &self.thresholds
    }
}

pub struct OptimizationGoal<D: DomainDefinition> {
    pub(crate) constraints: Vec<MeasurementConstraint<D>>,
    pub(crate) objectives: NonEmpty<Objective<D>>,
    pub(crate) preference: Preference<D>,
    pub(crate) success: Option<SuccessCondition<D>>,
}

impl<D: DomainDefinition> OptimizationGoal<D> {
    pub fn new(
        constraints: impl IntoIterator<Item = MeasurementConstraint<D>>,
        objectives: NonEmpty<Objective<D>>,
        preference: Preference<D>,
        success: Option<SuccessCondition<D>>,
    ) -> Result<Self, GoalError> {
        let mut seen = Vec::new();
        for objective in objectives.iter() {
            if seen.contains(&objective.metric) {
                return Err(GoalError::DuplicateObjective);
            }
            seen.push(objective.metric);
        }
        let preferred = preference
            .priority_tiers
            .iter()
            .flat_map(NonEmpty::iter)
            .copied()
            .collect::<Vec<_>>();
        if preferred.len() != seen.len() || seen.iter().any(|metric| !preferred.contains(metric)) {
            return Err(GoalError::PreferenceDoesNotCoverObjectives);
        }
        Ok(Self {
            constraints: constraints.into_iter().collect(),
            objectives,
            preference,
            success,
        })
    }

    #[must_use]
    pub fn constraints(&self) -> &[MeasurementConstraint<D>] {
        &self.constraints
    }

    #[must_use]
    pub fn objectives(&self) -> &NonEmpty<Objective<D>> {
        &self.objectives
    }

    #[must_use]
    pub fn preference(&self) -> &Preference<D> {
        &self.preference
    }

    #[must_use]
    pub fn success(&self) -> Option<&SuccessCondition<D>> {
        self.success.as_ref()
    }
}

pub struct GoalSet<D: DomainDefinition> {
    pub(crate) goals: NonEmpty<OptimizationGoal<D>>,
}

impl<D: DomainDefinition> GoalSet<D> {
    #[must_use]
    pub fn one(goal: OptimizationGoal<D>) -> Self {
        Self {
            goals: NonEmpty::one(goal),
        }
    }

    pub fn try_from_iter(
        goals: impl IntoIterator<Item = OptimizationGoal<D>>,
    ) -> Result<Self, EmptyGoalSet> {
        NonEmpty::try_from_iter(goals)
            .map(|goals| Self { goals })
            .map_err(|_| EmptyGoalSet)
    }

    #[must_use]
    pub fn goals(&self) -> &NonEmpty<OptimizationGoal<D>> {
        &self.goals
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GoalError {
    DuplicateObjective,
    DuplicatePreferenceMetric,
    PreferenceDoesNotCoverObjectives,
    UnknownMetric,
    InvalidObservation,
}

impl fmt::Display for GoalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid Optimization Goal: {self:?}")
    }
}

impl std::error::Error for GoalError {}
