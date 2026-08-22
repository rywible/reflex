//! Repository-only experimental seams.
//!
//! This module is absent unless the non-default `internal-experiments` feature
//! is selected. Production consumers and Domain Definitions do not depend on
//! its protocols.

use std::path::Path;
use std::time::Duration;

use crate::{DomainDefinition, ImprovementRequest, SessionError};

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
    pub claim_digest: [u8; 32],
    pub canonical_candidate: Vec<u8>,
    pub operator_symbol: Vec<u8>,
    pub verdict: ExperienceVerdictInspection,
    pub allocation_queue: CandidateAllocationQueueInspection,
    pub verification_requests: u32,
    pub epoch: u64,
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
