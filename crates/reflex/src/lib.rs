//! Autonomous optimization for mechanically verifiable domains.

pub mod bundle;
pub mod domain;
pub mod goal;
pub mod measurement;
pub mod session;

mod durability;
mod instrumentation;
mod knowledge;
mod learning;
mod resource;
mod runtime;

pub use bundle::{BundlePlan, DomainBundle};
pub use domain::{
    ApplicationWriter, CandidateWriter, ConstructorDescriptor, DomainDefinition, KernelRevision,
    OperatorAlgebra, OperatorDescriptor, OperatorEnumerationBatch, Seed, SeedPage, SeedSource,
    SeedWriter, SemanticIdentity, StructuralLocation, StructuralProtocol, StructuralView, Verdict,
    VerdictWriter, VerificationBatch, VerificationKernel, VerificationRecord,
    VerificationReplayBatch,
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
