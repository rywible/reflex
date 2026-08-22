//! Autonomous optimization for mechanically verifiable domains.

pub mod bundle;
pub mod domain;
pub mod goal;
pub mod measurement;
pub mod session;

mod durability;
mod instrumentation;
#[cfg(feature = "internal-experiments")]
#[doc(hidden)]
pub mod internal_experiments;
mod knowledge;
mod learning;
mod policy;
mod resource;
mod runtime;

pub use bundle::{BundlePlan, DomainBundle};
pub use domain::{
    ApplicationWriter, CandidateWriter, ConstructorDescriptor, DomainDefinition,
    ExternalVerificationUsage, ImmediateArity, KernelRevision, OperatorAlgebra, OperatorDescriptor,
    OperatorEnumerationBatch, ProposalFeatures, ProposalProvenance, Seed, SeedPage, SeedSource,
    SeedWriter, SemanticIdentity, StructuralLocation, StructuralProtocol, StructuralView, Verdict,
    VerdictWriter, VerificationAllowance, VerificationBatch, VerificationBatchOutcome,
    VerificationBatchReport, VerificationKernel, VerificationRecord, VerificationReplayBatch,
    VerificationWorkerRequirements,
};
pub use goal::{
    Direction, EmptyGoalSet, GoalError, GoalSet, MeasurementConstraint, MeasurementTolerance,
    NonEmpty, Objective, OptimizationGoal, Preference, SuccessCondition, ThresholdRelation,
};
pub use measurement::{
    Incomparable, MeasurementDescriptor, MeasurementEnvironment, MeasurementSpace,
    MeasurementWriter, MetricOrdering, VerifiedBatch,
};
pub use session::{
    ArtifactKey, Completion, GoalId, ImprovementRequest, NonZeroDuration, ParetoSnapshot,
    ParetoUpdate, RequestError, ResourceEnvelope, ResourceUsage, SessionError, SessionOutcome,
    VerifiedArtifact, improve,
};
