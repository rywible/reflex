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
                "{}-{}-{}-{}-workers-{}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH,
                env!("CARGO_PKG_VERSION"),
                if cfg!(debug_assertions) {
                    "debug"
                } else {
                    "release"
                },
                rayon::current_num_threads(),
                cpu_feature_identity(),
            ),
        }
    }

    #[must_use]
    pub fn identity(&self) -> &str {
        &self.identity
    }

    #[must_use]
    pub(crate) fn dynamic_resident_bytes(&self) -> u64 {
        u64::try_from(self.identity.capacity()).unwrap_or(u64::MAX)
    }
}

fn cpu_feature_identity() -> String {
    let mut features = Vec::new();
    #[cfg(target_arch = "x86_64")]
    for (name, present) in [
        ("sse4.2", std::arch::is_x86_feature_detected!("sse4.2")),
        ("avx", std::arch::is_x86_feature_detected!("avx")),
        ("avx2", std::arch::is_x86_feature_detected!("avx2")),
        ("avx512f", std::arch::is_x86_feature_detected!("avx512f")),
    ] {
        if present {
            features.push(name);
        }
    }
    #[cfg(target_arch = "aarch64")]
    if std::arch::is_aarch64_feature_detected!("neon") {
        features.push("neon");
    }
    if features.is_empty() {
        "scalar".into()
    } else {
        features.join("+")
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
    remaining: usize,
    overflowed: bool,
}

impl<'a, M, O> MeasurementWriter<'a, M, O> {
    pub fn new(output: &'a mut Vec<Measurement<M, O>>) -> Self {
        Self {
            output,
            remaining: usize::MAX,
            overflowed: false,
        }
    }

    pub(crate) fn with_limit(output: &'a mut Vec<Measurement<M, O>>, limit: usize) -> Self {
        Self {
            output,
            remaining: limit,
            overflowed: false,
        }
    }

    pub fn push(&mut self, artifact_index: usize, metric: M, observation: O) {
        if self.remaining == 0 {
            self.overflowed = true;
        } else {
            self.remaining -= 1;
            self.output.push(Measurement {
                artifact_index,
                metric,
                observation,
            });
        }
    }

    pub(crate) fn overflowed(&self) -> bool {
        self.overflowed
    }
}

pub trait MeasurementSpace<D: DomainDefinition>: Send + Sync + 'static {
    type Metric: Copy + Eq + Hash + Send + Sync + 'static;
    type Observation: Send + Sync + 'static;
    type Scratch: Default + Send + 'static;

    fn schema(&self) -> &[MeasurementDescriptor<Self::Metric>];
    fn measurement_scratch_resident_bytes(&self, artifacts: &[&D::Artifact]) -> u64;
    fn scratch_dynamic_resident_bytes(&self, scratch: &Self::Scratch) -> u64;
    fn observation_dynamic_resident_bytes_bound(
        &self,
        artifact: &D::Artifact,
        metric: Self::Metric,
    ) -> u64;
    fn observation_dynamic_resident_bytes(
        &self,
        metric: Self::Metric,
        observation: &Self::Observation,
    ) -> u64;
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
