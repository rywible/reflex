use std::fmt;
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Duration;

use crate::bundle::{BundlePlan, DomainBundle};
use crate::domain::{DomainDefinition, VerificationRecord};
use crate::goal::{GoalError, GoalSet};
use crate::measurement::{Measurement, MeasurementEnvironment};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct NonZeroDuration(Duration);

impl NonZeroDuration {
    #[must_use]
    pub fn new(value: Duration) -> Option<Self> {
        (!value.is_zero()).then_some(Self(value))
    }

    #[must_use]
    pub fn get(self) -> Duration {
        self.0
    }
}

#[derive(Clone, Debug)]
pub struct ResourceEnvelope {
    pub(crate) worker_threads: NonZeroUsize,
    pub(crate) resident_bytes: NonZeroU64,
    pub(crate) durable_bytes: NonZeroU64,
    pub(crate) elapsed_time: NonZeroDuration,
    pub(crate) cpu_time: NonZeroDuration,
    pub(crate) verification_requests: NonZeroU64,
}

impl ResourceEnvelope {
    #[must_use]
    pub fn new(
        worker_threads: NonZeroUsize,
        resident_bytes: NonZeroU64,
        durable_bytes: NonZeroU64,
        elapsed_time: NonZeroDuration,
        cpu_time: NonZeroDuration,
        verification_requests: NonZeroU64,
    ) -> Self {
        Self {
            worker_threads,
            resident_bytes,
            durable_bytes,
            elapsed_time,
            cpu_time,
            verification_requests,
        }
    }

    #[must_use]
    pub fn worker_threads(&self) -> NonZeroUsize {
        self.worker_threads
    }

    #[must_use]
    pub fn resident_bytes(&self) -> NonZeroU64 {
        self.resident_bytes
    }

    #[must_use]
    pub fn durable_bytes(&self) -> NonZeroU64 {
        self.durable_bytes
    }

    #[must_use]
    pub fn elapsed_time(&self) -> NonZeroDuration {
        self.elapsed_time
    }

    #[must_use]
    pub fn cpu_time(&self) -> NonZeroDuration {
        self.cpu_time
    }

    #[must_use]
    pub fn verification_requests(&self) -> NonZeroU64 {
        self.verification_requests
    }
}

pub struct ImprovementRequest<D: DomainDefinition> {
    pub(crate) goals: GoalSet<D>,
    pub(crate) seeds: D::SeedScope,
    pub(crate) resources: ResourceEnvelope,
    pub(crate) bundle: BundlePlan,
}

impl<D: DomainDefinition> ImprovementRequest<D> {
    pub fn new(
        goals: GoalSet<D>,
        seeds: D::SeedScope,
        resources: ResourceEnvelope,
        bundle: BundlePlan,
    ) -> Result<Self, RequestError> {
        if bundle.target().as_os_str().is_empty() {
            return Err(RequestError::EmptyBundlePath);
        }
        Ok(Self {
            goals,
            seeds,
            resources,
            bundle,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestError {
    EmptyBundlePath,
}

impl fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "invalid Improvement Session request: {self:?}")
    }
}

impl std::error::Error for RequestError {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArtifactKey(pub(crate) [u8; 32]);

impl ArtifactKey {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

pub(crate) struct VerifiedArtifactRecord<D: DomainDefinition> {
    pub key: ArtifactKey,
    pub claim_digest: [u8; 32],
    pub claim_canonical: Box<[u8]>,
    pub artifact: D::Artifact,
    pub verification: VerificationRecord<D>,
    pub origin_key: ArtifactKey,
    pub parent_key: Option<ArtifactKey>,
    pub measurements: Vec<Measurement<D::Metric, D::Observation>>,
    pub environment: MeasurementEnvironment,
    pub provenance: Vec<u8>,
    pub dynamic_resident_bytes: u64,
}

pub struct VerifiedArtifact<D: DomainDefinition> {
    pub(crate) inner: Arc<VerifiedArtifactRecord<D>>,
}

impl<D: DomainDefinition> Clone for VerifiedArtifact<D> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl<D: DomainDefinition> VerifiedArtifact<D> {
    #[must_use]
    pub fn key(&self) -> ArtifactKey {
        self.inner.key
    }

    #[must_use]
    pub fn artifact(&self) -> &D::Artifact {
        &self.inner.artifact
    }

    #[must_use]
    pub fn verification(&self) -> &VerificationRecord<D> {
        &self.inner.verification
    }

    #[must_use]
    pub fn origin_key(&self) -> ArtifactKey {
        self.inner.origin_key
    }

    #[must_use]
    pub fn parent_key(&self) -> Option<ArtifactKey> {
        self.inner.parent_key
    }

    #[must_use]
    pub fn measurements(&self) -> &[Measurement<D::Metric, D::Observation>] {
        &self.inner.measurements
    }

    #[must_use]
    pub fn measurement_environment(&self) -> &MeasurementEnvironment {
        &self.inner.environment
    }

    #[must_use]
    pub fn provenance(&self) -> &[u8] {
        &self.inner.provenance
    }
}

pub struct ParetoSnapshot<D: DomainDefinition> {
    pub(crate) artifacts: Vec<VerifiedArtifact<D>>,
}

impl<D: DomainDefinition> ParetoSnapshot<D> {
    #[must_use]
    pub fn artifacts(&self) -> &[VerifiedArtifact<D>] {
        &self.artifacts
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct GoalId(pub(crate) [u8; 32]);

impl GoalId {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

pub struct ParetoUpdate<'a, D: DomainDefinition> {
    pub(crate) sequence: u64,
    pub(crate) added: &'a [VerifiedArtifact<D>],
    pub(crate) removed: &'a [ArtifactKey],
    pub(crate) affected_goals: &'a [GoalId],
}

impl<'a, D: DomainDefinition> ParetoUpdate<'a, D> {
    #[must_use]
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    #[must_use]
    pub fn added(&self) -> &'a [VerifiedArtifact<D>] {
        self.added
    }

    #[must_use]
    pub fn removed(&self) -> &'a [ArtifactKey] {
        self.removed
    }

    #[must_use]
    pub fn affected_goals(&self) -> &'a [GoalId] {
        self.affected_goals
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Completion {
    ResourceEnvelopeExhausted,
    SuccessConditionsSatisfied,
    StoppedByObserver,
    NoEligibleWork,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ResourceUsage {
    pub worker_threads: usize,
    pub resident_bytes: u64,
    pub verification_requests: u64,
    pub durable_bytes: u64,
    pub elapsed_time: Duration,
    pub cpu_time: Duration,
}

pub struct SessionOutcome<D: DomainDefinition> {
    pub(crate) completion: Completion,
    pub(crate) pareto: ParetoSnapshot<D>,
    pub(crate) usage: ResourceUsage,
    pub(crate) bundle: DomainBundle,
}

impl<D: DomainDefinition> SessionOutcome<D> {
    #[must_use]
    pub fn completion(&self) -> Completion {
        self.completion
    }

    #[must_use]
    pub fn pareto(&self) -> &ParetoSnapshot<D> {
        &self.pareto
    }

    #[must_use]
    pub fn usage(&self) -> ResourceUsage {
        self.usage
    }

    #[must_use]
    pub fn bundle(&self) -> &DomainBundle {
        &self.bundle
    }
}

#[derive(Debug)]
pub enum SessionError<E> {
    InvalidRequest(RequestError),
    InvalidGoal(GoalError),
    InvalidSeed,
    IncompatibleBundle,
    CorruptBundle,
    Domain(E),
    Durability(std::io::Error),
    Resource,
}

impl<E: fmt::Display> fmt::Display for SessionError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(error) => error.fmt(formatter),
            Self::InvalidGoal(error) => error.fmt(formatter),
            Self::InvalidSeed => formatter.write_str("invalid Seed"),
            Self::IncompatibleBundle => formatter.write_str("incompatible Domain Bundle"),
            Self::CorruptBundle => formatter.write_str("corrupt Domain Bundle"),
            Self::Domain(error) => write!(formatter, "domain error: {error}"),
            Self::Durability(error) => write!(formatter, "durability error: {error}"),
            Self::Resource => formatter.write_str("Resource Envelope cannot be honored"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for SessionError<E> {}

#[expect(
    clippy::needless_pass_by_value,
    reason = "the public Session seam deliberately owns its Domain Definition and request"
)]
pub fn improve<D, O>(
    domain: D,
    request: ImprovementRequest<D>,
    observer: O,
) -> Result<SessionOutcome<D>, SessionError<D::Error>>
where
    D: DomainDefinition,
    O: for<'a> FnMut(ParetoUpdate<'a, D>) -> ControlFlow<()> + Send,
{
    crate::runtime::improve(&domain, &request, observer)
}
