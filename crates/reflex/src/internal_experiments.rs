//! Repository-only experimental seams.
//!
//! This module is absent unless the non-default `internal-experiments` feature
//! is selected. Production consumers and Domain Definitions do not depend on
//! its protocols.

use std::path::Path;
use std::time::Duration;

use crate::{DomainDefinition, ImprovementRequest, SessionError};

/// Paired fixed-Experience comparison of two equal-capacity Candidate feature families.
#[derive(Clone, Debug)]
pub struct CandidateFeatureComparison {
    pub examples: usize,
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
    pub baseline_accepted_at_k: [usize; 7],
    pub structural_accepted_at_k: [usize; 7],
    pub balanced_structural_accepted_at_k: [usize; 7],
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
