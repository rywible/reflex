//! Repository-only experimental seams.
//!
//! This module is absent unless the non-default `internal-experiments` feature
//! is selected. Production consumers and Domain Definitions do not depend on
//! its protocols.

use std::path::Path;
use std::time::Duration;

use crate::{Completion, DomainDefinition, ImprovementRequest, ResourceUsage, SessionError};

/// Repository-facing decomposition of mandatory resident resources before
/// Seed ingestion or search-state allocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DomainResourceInspection {
    pub requested_worker_threads: usize,
    pub external_worker_lanes: usize,
    pub runtime_worker_lanes: usize,
    pub runtime_stack_bytes: u64,
    pub durability_stack_bytes: u64,
    pub external_worker_bytes: u64,
    pub operator_bytes: u64,
    pub maximum_candidate_capacity: usize,
    pub operator_scratch_bytes_per_lane: u64,
    pub operator_scratch_bytes: u64,
    pub fixed_resident_bytes: u64,
}

/// Computes the exact mandatory resident reservation used by the Runtime.
///
/// This excludes dynamic Artifacts, Experience, Frontier, and learning state;
/// callers can subtract it from an experiment envelope before spending search.
#[must_use]
pub fn inspect_domain_resources<D: DomainDefinition>(
    domain: &D,
    requested_worker_threads: usize,
) -> Option<DomainResourceInspection> {
    let plan = crate::runtime::inspect_domain_resources(domain, requested_worker_threads)?;
    Some(DomainResourceInspection {
        requested_worker_threads: plan.requested_worker_threads,
        external_worker_lanes: plan.external_worker_lanes,
        runtime_worker_lanes: plan.runtime_worker_lanes,
        runtime_stack_bytes: plan.runtime_stack_bytes,
        durability_stack_bytes: plan.durability_stack_bytes,
        external_worker_bytes: plan.external_worker_bytes,
        operator_bytes: plan.operator_bytes,
        maximum_candidate_capacity: plan.maximum_candidate_capacity,
        operator_scratch_bytes_per_lane: plan.operator_scratch_bytes_per_lane,
        operator_scratch_bytes: plan.operator_scratch_bytes,
        fixed_resident_bytes: plan.fixed_resident_bytes,
    })
}

/// Repository-facing projection of the durable Session envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionInspection {
    pub completed: bool,
    pub requested: ResourceUsage,
    pub kernel_revision: u64,
    pub environment: Vec<u8>,
    pub completion: Option<Completion>,
    pub usage: ResourceUsage,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionInspectionError;

impl std::fmt::Display for SessionInspectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("malformed Session segment")
    }
}

impl std::error::Error for SessionInspectionError {}

/// Decodes the resource request, terminal disposition, and measured usage from
/// a Session segment without duplicating its durable framing in an experiment.
///
/// # Errors
///
/// Returns an error when the segment framing or fingerprints are malformed.
pub fn inspect_session_segment(bytes: &[u8]) -> Result<SessionInspection, SessionInspectionError> {
    crate::runtime::inspect_session_segment(bytes).map_err(|()| SessionInspectionError)
}

/// Test-facing projection decoded through the production Experience codec.
#[derive(Clone, Debug)]
pub struct ExperienceInspection {
    pub attempts: Vec<ExperienceAttemptInspection>,
    pub candidate_fates: Vec<CandidateFateInspection>,
    pub consequence_count: usize,
    pub measurements: Vec<ExperienceMeasurementInspection>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CandidateFateInspection {
    pub candidate_key: [u8; 32],
    pub claim_digest: [u8; 32],
    pub parent_key: [u8; 32],
    pub operator_digest: [u8; 32],
    pub support_key: Option<[u8; 32]>,
    pub epoch: u64,
    pub generation_rank: u32,
    pub proposal_limit: u32,
    pub policy_rank: Option<u32>,
    pub verification_batch_cpu_ns: Option<u64>,
    pub verification_batch_size: Option<u32>,
    pub outcome: CandidateFateOutcomeInspection,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateFateOutcomeInspection {
    NoveltyFiltered {
        reason: CandidateNoveltyFilterReasonInspection,
    },
    PolicyDeferred {
        bootstrap_rank: Option<u32>,
        learned_rank: Option<u32>,
    },
    VerificationInterrupted {
        allocation_queue: CandidateAllocationQueueInspection,
        bootstrap_rank: Option<u32>,
        learned_rank: Option<u32>,
    },
    Verified {
        verdict: ExperienceVerdictInspection,
        allocation_queue: CandidateAllocationQueueInspection,
        bootstrap_rank: Option<u32>,
        learned_rank: Option<u32>,
        admitted: bool,
        strict_improvement: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateNoveltyFilterReasonInspection {
    KnownArtifact,
    DuplicateCandidate,
    PriorNegativeExperience,
}

#[derive(Clone, Debug)]
pub struct ExperienceAttemptInspection {
    pub candidate_key: [u8; 32],
    pub claim_digest: [u8; 32],
    pub canonical_candidate: Vec<u8>,
    pub operator_symbol: Vec<u8>,
    pub support_key: Option<[u8; 32]>,
    pub verdict: ExperienceVerdictInspection,
    pub allocation_queue: CandidateAllocationQueueInspection,
    pub verification_requests: u32,
    pub epoch: u64,
    pub feature_bits: Vec<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CandidateAllocationQueueInspection {
    ProtectedOrigin,
    ProtectedDerived,
    Learned,
    Bootstrap,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExperienceVerdictInspection {
    Accepted,
    Refuted,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ExperienceInspectionError;

impl std::fmt::Display for ExperienceInspectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("malformed Experience segment")
    }
}

impl std::error::Error for ExperienceInspectionError {}

#[derive(Clone, Debug)]
pub struct ExperienceMeasurementInspection {
    pub environment: Vec<u8>,
    pub value_count: usize,
}

/// Decodes an Experience segment without duplicating its durable layout.
///
/// # Errors
///
/// Returns `Err(())` when the segment framing is malformed.
pub fn inspect_experience_segment(
    bytes: &[u8],
) -> Result<ExperienceInspection, ExperienceInspectionError> {
    crate::runtime::inspect_experience_segment(bytes).map_err(|()| ExperienceInspectionError)
}

/// Repository-facing projection of a private autonomous-intelligence checkpoint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntelligenceInspection {
    pub active_specialists: usize,
    pub receipts: usize,
    pub settlements: usize,
    pub consequences: usize,
    pub contextual_contrasts: usize,
    pub provisional_knowledge: usize,
    pub verified_knowledge: usize,
    pub promoted_knowledge: usize,
    pub invalidated_knowledge: usize,
    pub open_shadows: usize,
    pub completed_shadows: usize,
    pub interrupted_shadows: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntelligenceInspectionError;

impl std::fmt::Display for IntelligenceInspectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("malformed Intelligence checkpoint")
    }
}

impl std::error::Error for IntelligenceInspectionError {}

/// Authenticated current-Core state for one frozen causal-ablation treatment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IntelligenceTreatmentCheckpoint {
    pub checkpoint: Vec<u8>,
    pub knowledge_product: [u8; 32],
    pub model_revision: [u8; 32],
    pub runtime_policy_revision: [u8; 32],
    pub intelligence_revision: [u8; 32],
}

/// Canonical root and byte identity for one Intelligence component.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntelligenceComponentIdentity {
    pub root: [u8; 32],
    pub bytes: [u8; 32],
}

/// Repository-only causal-ablation inspection of one authenticated Core.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntelligenceComponentInspection {
    pub model_ecology: IntelligenceComponentIdentity,
    pub causal_experience: IntelligenceComponentIdentity,
    pub knowledge_compiler: IntelligenceComponentIdentity,
    pub runtime_policy: IntelligenceComponentIdentity,
    pub knowledge_records: usize,
    pub pending_knowledge_verification: bool,
    pub active_derived_operators: usize,
}

/// One Domain-bound Intelligence checkpoint supplied to the repository's
/// causal-ablation adapter.
#[derive(Clone, Copy, Debug)]
pub struct IntelligenceTreatmentSource<'a> {
    semantic_identity: &'a [u8],
    checkpoint: &'a [u8],
}

impl<'a> IntelligenceTreatmentSource<'a> {
    #[must_use]
    pub const fn new(semantic_identity: &'a [u8], checkpoint: &'a [u8]) -> Self {
        Self {
            semantic_identity,
            checkpoint,
        }
    }
}

/// Inspects authenticated Core component identities for repository ablation
/// invariants without exposing them through the normal consumer interface.
///
/// # Errors
///
/// Returns an error when `checkpoint` is not a current authenticated Core.
pub fn inspect_intelligence_components(
    checkpoint: &[u8],
) -> Result<IntelligenceComponentInspection, IntelligenceInspectionError> {
    let core = crate::intelligence::IntelligenceCore::restore(checkpoint)
        .map_err(|_| IntelligenceInspectionError)?;
    let (roots, bytes, knowledge_records, pending, active_derived_operators) =
        core.treatment_component_inspection();
    let identity = |index| IntelligenceComponentIdentity {
        root: roots[index],
        bytes: bytes[index],
    };
    Ok(IntelligenceComponentInspection {
        model_ecology: identity(0),
        causal_experience: identity(1),
        knowledge_compiler: identity(2),
        runtime_policy: identity(3),
        knowledge_records,
        pending_knowledge_verification: pending,
        active_derived_operators,
    })
}

/// Applies exactly one repository causal-ablation treatment through the
/// production Intelligence checkpoint codec.
///
/// A `model_template` replaces only the Model Ecology. Setting
/// `without_derived_operators` removes executable and promotable Derived
/// Operator knowledge while retaining causal Experience, Runtime Policy, and
/// the active Artifact scheduling index.
///
/// # Errors
///
/// Returns an error when either checkpoint is invalid or contains open work,
/// when the template belongs to a different semantic identity, when its Model
/// Ecology is not the exact empty Bootstrap ecology, or when that ecology does
/// not fit the source Core's limits.
pub fn ablate_intelligence_checkpoint(
    source: IntelligenceTreatmentSource<'_>,
    model_template: Option<IntelligenceTreatmentSource<'_>>,
    without_derived_operators: bool,
) -> Result<IntelligenceTreatmentCheckpoint, IntelligenceInspectionError> {
    if model_template.is_some() == without_derived_operators {
        return Err(IntelligenceInspectionError);
    }
    let core = crate::intelligence::IntelligenceCore::restore(source.checkpoint)
        .map_err(|_| IntelligenceInspectionError)?;
    let template = model_template
        .map(|template| {
            crate::intelligence::IntelligenceCore::restore(template.checkpoint)
                .map(|core| (core, template.semantic_identity))
        })
        .transpose()
        .map_err(|_| IntelligenceInspectionError)?;
    let ablated = core
        .treatment_ablation_for_experiment(
            source.semantic_identity,
            template
                .as_ref()
                .map(|(core, semantic_identity)| (core, *semantic_identity)),
            without_derived_operators,
        )
        .map_err(|_| IntelligenceInspectionError)?;
    let checkpoint = ablated.checkpoint();
    Ok(IntelligenceTreatmentCheckpoint {
        checkpoint: checkpoint.as_bytes().to_vec(),
        knowledge_product: ablated.knowledge_product().identity(),
        model_revision: ablated.model_ecology_identity(),
        runtime_policy_revision: ablated.runtime_policy_revision().identity(),
        intelligence_revision: checkpoint.identity(),
    })
}

/// Re-encodes a Bootstrap-policy Intelligence checkpoint using the legacy v12
/// wire format for repository migration fixtures.
///
/// # Errors
///
/// Returns an error when the input checkpoint is invalid or already contains
/// retained Runtime Policy state that v12 could not represent.
pub fn legacy_v12_intelligence_checkpoint(
    bytes: &[u8],
) -> Result<Vec<u8>, IntelligenceInspectionError> {
    let core = crate::intelligence::IntelligenceCore::restore(bytes)
        .map_err(|_| IntelligenceInspectionError)?;
    if core.runtime_policy_identity() != crate::policy::RuntimePolicyState::bootstrap().identity() {
        return Err(IntelligenceInspectionError);
    }
    Ok(core.legacy_v12_checkpoint_for_test().as_bytes().to_vec())
}

/// Re-encodes an Intelligence checkpoint using the legacy v13 wire format,
/// whose Knowledge Compiler did not own the active Knowledge Revision.
///
/// # Errors
///
/// Returns an error when the input checkpoint is invalid.
pub fn legacy_v13_intelligence_checkpoint(
    bytes: &[u8],
) -> Result<Vec<u8>, IntelligenceInspectionError> {
    let core = crate::intelligence::IntelligenceCore::restore(bytes)
        .map_err(|_| IntelligenceInspectionError)?;
    Ok(core.legacy_v13_checkpoint_for_test().as_bytes().to_vec())
}

/// Re-snapshots a current Core with a structurally valid active Knowledge
/// lineage that refers to a foreign Artifact.
///
/// This repository-only fixture proves that Domain Bundle recovery checks the
/// Core-owned Knowledge semantics against the installed Domain Definition
/// instead of trusting authenticated Core framing alone.
///
/// # Errors
///
/// Returns an error when the checkpoint is invalid or has no promoted
/// predecessor that can carry the hostile Artifact reference.
pub fn hostile_current_knowledge_checkpoint(
    bytes: &[u8],
) -> Result<Vec<u8>, IntelligenceInspectionError> {
    let core = crate::intelligence::IntelligenceCore::restore(bytes)
        .map_err(|_| IntelligenceInspectionError)?;
    core.hostile_domain_invalid_knowledge_checkpoint_for_test()
        .map(|checkpoint| checkpoint.as_bytes().to_vec())
        .map_err(|_| IntelligenceInspectionError)
}

/// Counts the correctness-bearing Knowledge obligations that current recovery
/// must replay through the installed Verification Kernel.
///
/// # Errors
///
/// Returns an error when the checkpoint or its authenticated recovery manifest
/// is invalid.
pub fn inspect_knowledge_recovery_obligation_count(
    bytes: &[u8],
) -> Result<usize, IntelligenceInspectionError> {
    let core = crate::intelligence::IntelligenceCore::restore(bytes)
        .map_err(|_| IntelligenceInspectionError)?;
    core.knowledge_recovery_manifest()
        .map(|manifest| manifest.obligation_count())
        .map_err(|_| IntelligenceInspectionError)
}

/// Encodes the empty legacy learner used by repository bundle-migration fixtures.
#[must_use]
pub fn empty_legacy_learning_state() -> Vec<u8> {
    crate::learning::LearningState::default().encode()
}

/// Computes the authenticated legacy learner revision for migration fixtures.
pub fn legacy_learning_revision(
    bytes: &[u8],
    semantic_identity: &str,
) -> Result<[u8; 32], IntelligenceInspectionError> {
    crate::learning::LearningState::decode(bytes)
        .map(|state| state.revision_digest(semantic_identity))
        .map_err(|()| IntelligenceInspectionError)
}

/// Decodes private Intelligence state through its production checkpoint codec.
///
/// # Errors
///
/// Returns an error when canonical framing, limits, references, or checksums are invalid.
pub fn inspect_intelligence_checkpoint(
    bytes: &[u8],
) -> Result<IntelligenceInspection, IntelligenceInspectionError> {
    let inspection = crate::runtime::inspect_intelligence_checkpoint(bytes)
        .map_err(|()| IntelligenceInspectionError)?;
    Ok(intelligence_inspection(inspection))
}

/// Decodes private Intelligence from a current Revisions segment.
///
/// # Errors
///
/// Returns an error when the segment framing or Intelligence checkpoint is malformed.
pub fn inspect_intelligence_revision_segment(
    bytes: &[u8],
) -> Result<IntelligenceInspection, IntelligenceInspectionError> {
    let inspection = crate::runtime::inspect_intelligence_revision_segment(bytes)
        .map_err(|()| IntelligenceInspectionError)?;
    Ok(intelligence_inspection(inspection))
}

fn intelligence_inspection(
    inspection: (
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
    ),
) -> IntelligenceInspection {
    IntelligenceInspection {
        active_specialists: inspection.0,
        receipts: inspection.1,
        settlements: inspection.2,
        consequences: inspection.3,
        contextual_contrasts: inspection.4,
        provisional_knowledge: inspection.5,
        verified_knowledge: inspection.6,
        promoted_knowledge: inspection.7,
        invalidated_knowledge: inspection.8,
        open_shadows: inspection.9,
        completed_shadows: inspection.10,
        interrupted_shadows: inspection.11,
    }
}

/// Repository-facing summary of a Derived Operator retained in Knowledge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DerivedKnowledgeInspection {
    pub active: bool,
    pub steps: usize,
    pub support: usize,
}

/// Repository-facing Knowledge projection decoded through the production codec.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KnowledgeInspection {
    pub generation: u64,
    pub derived: Vec<DerivedKnowledgeInspection>,
}

/// Decodes the Knowledge State embedded in a current Revisions segment.
///
/// # Errors
///
/// Returns an error when the segment framing or Knowledge payload is malformed.
pub fn inspect_knowledge_revision_segment(
    bytes: &[u8],
) -> Result<KnowledgeInspection, IntelligenceInspectionError> {
    let (generation, derived) = crate::runtime::inspect_knowledge_revision_segment(bytes)
        .map_err(|()| IntelligenceInspectionError)?;
    Ok(KnowledgeInspection {
        generation,
        derived: derived
            .into_iter()
            .map(|(active, steps, support)| DerivedKnowledgeInspection {
                active,
                steps,
                support,
            })
            .collect(),
    })
}

/// Re-encodes an Experience segment after forcing its first verdict to Accepted.
///
/// This deliberately test-only mutation lets corruption tests exercise semantic
/// replay checks without learning private byte offsets.
///
/// # Errors
///
/// Returns `Err(())` when the segment is malformed or contains no attempts.
pub fn force_first_experience_accepted(bytes: &[u8]) -> Result<Vec<u8>, ExperienceInspectionError> {
    crate::runtime::force_first_experience_accepted(bytes).map_err(|()| ExperienceInspectionError)
}

/// Re-encodes current Experience through the frozen pre-action-provenance v23
/// framing for migration fixtures.
pub fn pre_action_v23_experience_segment(
    bytes: &[u8],
) -> Result<Vec<u8>, ExperienceInspectionError> {
    crate::runtime::pre_action_v23_experience_segment(bytes).map_err(|()| ExperienceInspectionError)
}

/// Paired fixed-Experience comparison of two equal-capacity Candidate feature families.
#[derive(Clone, Debug)]
pub struct CandidateFeatureComparison {
    pub examples: usize,
    pub feature_count: usize,
    pub claim_operator_feature_groups: usize,
    pub mixed_verdict_claim_operator_feature_groups: usize,
    pub accepted_examples_in_mixed_groups: usize,
    pub accepted_examples: usize,
    pub proposal_informed_examples: usize,
    pub replay_claims: usize,
    pub selection_claims: usize,
    pub baseline_selection_loss: [f32; 7],
    pub structural_selection_loss: [f32; 7],
    pub balanced_structural_selection_loss: [f32; 7],
    pub baseline_training_cpu: Duration,
    pub structural_training_cpu: Duration,
    pub balanced_structural_training_cpu: Duration,
    pub feature_extraction_cpu: Duration,
    pub baseline_model_revision: [u8; 32],
    pub structural_model_revision: [u8; 32],
    pub balanced_structural_model_revision: [u8; 32],
    pub model_bytes: usize,
    pub baseline_reproduces_champion: bool,
    pub structural_promotes_over_baseline: bool,
    pub ranking_budgets: [usize; 7],
    pub bootstrap_accepted_at_k: [usize; 7],
    pub baseline_accepted_at_k: [usize; 7],
    pub structural_accepted_at_k: [usize; 7],
    pub balanced_structural_accepted_at_k: [usize; 7],
    pub global_bootstrap_accepted_at_k: [usize; 7],
    pub global_baseline_accepted_at_k: [usize; 7],
    pub global_structural_accepted_at_k: [usize; 7],
    pub global_balanced_structural_accepted_at_k: [usize; 7],
    pub evaluated_at_k: [usize; 7],
    pub selection_accepted: usize,
}

/// Replays canonical Candidate structure from a retained Domain Bundle without
/// requesting new Verification labels.
///
/// # Errors
///
/// Returns the same compatibility, corruption, resource, and domain failures
/// as normal restart decoding.
pub fn compare_candidate_features<D: DomainDefinition>(
    domain: &D,
    request: &ImprovementRequest<D>,
    source: &Path,
) -> Result<CandidateFeatureComparison, SessionError<D::Error>> {
    crate::runtime::compare_candidate_features(domain, request, source)
}
