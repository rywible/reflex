use std::hash::Hash;

use crate::domain::{DomainDefinition, SymbolId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeasurementEnvironment {
    identity: String,
}

impl MeasurementEnvironment {
    #[must_use]
    pub fn local_process() -> Self {
        Self {
            identity: format!(
                "{}-{}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH,
                env!("CARGO_PKG_VERSION")
            ),
        }
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }
}

#[derive(Clone, Debug)]
pub struct MeasurementDescriptor<M> {
    metric: M,
    symbol: SymbolId,
}

impl<M: Copy> MeasurementDescriptor<M> {
    #[must_use]
    pub fn new(metric: M, symbol: SymbolId) -> Self {
        Self { metric, symbol }
    }

    #[must_use]
    pub fn metric(&self) -> M {
        self.metric
    }

    #[must_use]
    pub fn symbol(&self) -> &SymbolId {
        &self.symbol
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetricOrdering {
    Less,
    Equal,
    Greater,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Incomparable;

pub struct VerifiedBatch<'a, D: DomainDefinition> {
    artifacts: &'a [&'a D::Artifact],
}

impl<'a, D: DomainDefinition> VerifiedBatch<'a, D> {
    #[must_use]
    pub fn new(artifacts: &'a [&'a D::Artifact]) -> Self {
        Self { artifacts }
    }

    #[must_use]
    pub fn artifacts(&self) -> &'a [&'a D::Artifact] {
        self.artifacts
    }
}

pub struct Measurement<M, O> {
    pub artifact_index: usize,
    pub metric: M,
    pub observation: O,
}

pub struct MeasurementWriter<'a, M, O> {
    output: &'a mut Vec<Measurement<M, O>>,
}

impl<'a, M, O> MeasurementWriter<'a, M, O> {
    pub(crate) fn new(output: &'a mut Vec<Measurement<M, O>>) -> Self {
        Self { output }
    }

    pub fn push(&mut self, artifact_index: usize, metric: M, observation: O) {
        self.output.push(Measurement {
            artifact_index,
            metric,
            observation,
        });
    }
}

pub trait MeasurementSpace<D: DomainDefinition>: Send + Sync + 'static {
    type Metric: Copy + Eq + Hash + Send + Sync + 'static;
    type Observation: Send + Sync + 'static;
    type Scratch: Default + Send + 'static;

    fn schema(&self) -> &[MeasurementDescriptor<Self::Metric>];
    fn measure_batch(
        &self,
        artifacts: VerifiedBatch<'_, D>,
        environment: &MeasurementEnvironment,
        output: &mut MeasurementWriter<'_, Self::Metric, Self::Observation>,
        scratch: &mut Self::Scratch,
    ) -> Result<(), D::Error>;
    fn compare(
        &self,
        metric: Self::Metric,
        left: &Self::Observation,
        right: &Self::Observation,
    ) -> Result<MetricOrdering, Incomparable>;
    fn environments_compatible(
        &self,
        metric: Self::Metric,
        left: &MeasurementEnvironment,
        right: &MeasurementEnvironment,
    ) -> bool;
    fn within_tolerance(
        &self,
        metric: Self::Metric,
        left: &Self::Observation,
        right: &Self::Observation,
        tolerance: &Self::Observation,
    ) -> Result<bool, Incomparable>;
    fn encode_observation(
        &self,
        metric: Self::Metric,
        observation: &Self::Observation,
        output: &mut Vec<u8>,
    ) -> Result<(), D::Error>;
    fn decode_observation(
        &self,
        metric: Self::Metric,
        bytes: &[u8],
    ) -> Result<Self::Observation, D::Error>;
}
