use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use crate::DomainDefinition;
use crate::domain::{Candidate, ProposalFeatures, ProposalProvenance, StructuralProtocol};
use crate::learning::Features;
use crate::policy::{AllocationQueue, RuntimePolicyRevision};

use super::experience::CandidateRank;
use super::{PendingParent, ProposedCandidate};
use crate::intelligence::DecisionId;
use crate::session::ArtifactKey;

const MIN_COHORT: usize = 8;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CanonicalArtifact {
    bytes: Vec<u8>,
}

impl CanonicalArtifact {
    pub(super) fn new(bytes: Vec<u8>) -> Self {
        Self { bytes }
    }

    pub(super) fn decode_with<T, E>(
        &self,
        decode: impl FnOnce(&[u8]) -> Result<T, E>,
    ) -> Result<T, E> {
        decode(&self.bytes)
    }

    pub(super) fn resident_bytes(&self) -> u64 {
        self.bytes.capacity() as u64
    }
}

#[derive(Clone)]
pub(super) struct CohortProposal {
    canonical_artifact: CanonicalArtifact,
    source_index: usize,
    proposal_features: ProposalFeatures,
    proposal_provenance: Option<ProposalProvenance>,
    operator_symbol: Vec<u8>,
    operator_digest: [u8; 32],
    features: Features,
    epoch: u64,
    proposal_limit: u32,
    protected_derived: bool,
    allocation_queue: AllocationQueue,
    fate_index: usize,
    bootstrap_rank: CandidateRank,
    learned_rank: CandidateRank,
    generated_in_epoch: bool,
    published_in_epoch: bool,
    action_decision: Option<DecisionId>,
    causal_parent_key: Option<ArtifactKey>,
}

impl CohortProposal {
    pub(super) fn capture<D: DomainDefinition>(candidate: ProposedCandidate<D>) -> Self {
        Self {
            canonical_artifact: CanonicalArtifact::new(candidate.canonical_candidate),
            source_index: candidate.candidate.source_index,
            proposal_features: candidate.candidate.proposal_features,
            proposal_provenance: candidate.candidate.proposal_provenance,
            operator_symbol: candidate.operator_symbol,
            operator_digest: candidate.operator_digest,
            features: candidate.features,
            epoch: candidate.epoch,
            proposal_limit: candidate.proposal_limit,
            protected_derived: candidate.protected_derived,
            allocation_queue: candidate.allocation_queue,
            fate_index: candidate.fate_index,
            bootstrap_rank: candidate.bootstrap_rank,
            learned_rank: candidate.learned_rank,
            generated_in_epoch: candidate.generated_in_epoch,
            published_in_epoch: candidate.published_in_epoch,
            action_decision: candidate.action_decision,
            causal_parent_key: candidate.causal_parent_key,
        }
    }

    pub(super) fn replay<D: DomainDefinition>(
        &self,
        domain: &D,
        scratch: &mut <D::Structure as StructuralProtocol<D>>::Scratch,
    ) -> Result<ProposedCandidate<D>, D::Error> {
        let artifact = self
            .canonical_artifact
            .decode_with(|bytes| domain.structure().decode_canonical(bytes, scratch))?;
        Ok(ProposedCandidate {
            candidate: Candidate {
                source_index: self.source_index,
                artifact,
                proposal_features: self.proposal_features,
                proposal_provenance: self.proposal_provenance,
            },
            canonical_candidate: self.canonical_artifact.bytes.clone(),
            operator_symbol: self.operator_symbol.clone(),
            operator_digest: self.operator_digest,
            features: self.features,
            epoch: self.epoch,
            proposal_limit: self.proposal_limit,
            protected_derived: self.protected_derived,
            allocation_queue: self.allocation_queue,
            fate_index: self.fate_index,
            bootstrap_rank: self.bootstrap_rank,
            learned_rank: self.learned_rank,
            generated_in_epoch: self.generated_in_epoch,
            published_in_epoch: self.published_in_epoch,
            // Opening the durable policy shadow advances the Core revision, so
            // a receipt minted before the trial cannot be settled by either arm.
            intelligence_receipt: None,
            action_decision: self.action_decision,
            causal_parent_key: self.causal_parent_key,
        })
    }

    pub(super) fn resident_bytes(&self) -> u64 {
        self.canonical_artifact
            .resident_bytes()
            .saturating_add(self.operator_symbol.capacity() as u64)
    }

    pub(super) const fn generated_in_epoch(&self) -> bool {
        self.generated_in_epoch
    }

    pub(super) fn trial_identity(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"reflex-cohort-proposal-v1\0");
        digest.update((self.canonical_artifact.bytes.len() as u64).to_le_bytes());
        digest.update(&self.canonical_artifact.bytes);
        digest.update((self.source_index as u64).to_le_bytes());
        for feature in self.proposal_features.as_array() {
            digest.update(feature.to_bits().to_le_bytes());
        }
        if let Some(provenance) = self.proposal_provenance {
            digest.update([1]);
            digest.update(provenance.support_key());
        } else {
            digest.update([0]);
        }
        digest.update((self.operator_symbol.len() as u64).to_le_bytes());
        digest.update(&self.operator_symbol);
        digest.update(self.operator_digest);
        for feature in self.features.0 {
            digest.update(feature.to_bits().to_le_bytes());
        }
        digest.update(self.epoch.to_le_bytes());
        digest.update(self.proposal_limit.to_le_bytes());
        digest.update([
            u8::from(self.protected_derived),
            self.allocation_queue as u8,
        ]);
        digest.update((self.fate_index as u64).to_le_bytes());
        digest.update(
            self.bootstrap_rank
                .value()
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        digest.update(self.learned_rank.value().unwrap_or(u32::MAX).to_le_bytes());
        digest.update([u8::from(self.generated_in_epoch)]);
        if let Some(decision) = self.action_decision {
            digest.update([1]);
            digest.update(decision.identity());
        } else {
            digest.update([0]);
        }
        if let Some(parent) = self.causal_parent_key {
            digest.update([1]);
            digest.update(parent.as_bytes());
        } else {
            digest.update([0]);
        }
        digest.finalize().into()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct PolicySelectionPlan {
    selected: std::ops::Range<usize>,
    deferred: std::ops::Range<usize>,
}

impl PolicySelectionPlan {
    pub(super) fn for_policy<K: Copy + Ord>(
        policy: RuntimePolicyRevision,
        proposal_count: usize,
        remaining: usize,
        worker_lanes: usize,
        represented_claims: &[K],
        covered_claims: &[K],
    ) -> Self {
        let selected = limit(
            remaining,
            worker_lanes,
            usize::from(policy.verification_cohort()),
            represented_claims,
            covered_claims,
        )
        .min(proposal_count);
        Self {
            selected: 0..selected,
            deferred: selected..proposal_count,
        }
    }

    pub(super) fn selected(&self) -> std::ops::Range<usize> {
        self.selected.clone()
    }

    pub(super) fn deferred(&self) -> std::ops::Range<usize> {
        self.deferred.clone()
    }

    pub(super) fn trial_identity(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"reflex-policy-selection-plan-v1\0");
        digest.update((self.selected.start as u64).to_le_bytes());
        digest.update((self.selected.end as u64).to_le_bytes());
        digest.update((self.deferred.start as u64).to_le_bytes());
        digest.update((self.deferred.end as u64).to_le_bytes());
        digest.finalize().into()
    }
}

/// Chooses one bounded Verification Cohort without exposing a tuning surface.
///
/// The policy pays complete claim coverage only for claims that have no
/// retained Verification Experience, preserves ordinary slots for policy
/// comparison on large Seed Scopes, and never exceeds the remaining allowance.
pub(super) fn limit<K: Copy + Ord>(
    remaining: usize,
    worker_lanes: usize,
    policy_minimum: usize,
    represented_claims: &[K],
    covered_claims: &[K],
) -> usize {
    let uncovered_claim_count = represented_claims
        .iter()
        .copied()
        .filter(|claim| covered_claims.binary_search(claim).is_err())
        .collect::<BTreeSet<_>>()
        .len();
    let complete_claim_coverage = if uncovered_claim_count <= remaining {
        uncovered_claim_count.saturating_add(
            usize::from(uncovered_claim_count > MIN_COHORT).saturating_mul(MIN_COHORT),
        )
    } else {
        0
    };
    remaining.min(
        MIN_COHORT
            .max(worker_lanes)
            .max(policy_minimum)
            .max(complete_claim_coverage),
    )
}

pub(super) fn recovery_resident_bytes<D: DomainDefinition>(
    domain: &D,
    deferred_candidates: &Vec<ProposedCandidate<D>>,
    pending_parents: &Vec<PendingParent>,
) -> u64 {
    super::vector_bytes(deferred_candidates)
        .saturating_add(super::candidate_pipeline_reserve(
            domain,
            deferred_candidates,
        ))
        .saturating_add(super::vector_bytes(pending_parents))
        .saturating_add(pending_parents.iter().fold(0_u64, |bytes, parent| {
            bytes.saturating_add(parent.resident_bytes())
        }))
}

pub(super) fn rollback_unverified_generation<D: DomainDefinition>(
    selected: Vec<ProposedCandidate<D>>,
    deferred: Vec<ProposedCandidate<D>>,
) -> Vec<ProposedCandidate<D>> {
    selected
        .into_iter()
        .chain(deferred)
        .filter(|candidate| {
            survives_unverified_rollback(candidate.generated_in_epoch, candidate.published_in_epoch)
        })
        .collect()
}

const fn survives_unverified_rollback(generated_in_epoch: bool, published_in_epoch: bool) -> bool {
    !generated_in_epoch || published_in_epoch
}

#[cfg(test)]
mod tests {
    use crate::policy::{
        RuntimePolicyKernel, RuntimePolicyMutation, RuntimePolicyRevision, SignedStep,
    };

    use super::{CanonicalArtifact, PolicySelectionPlan, limit, survives_unverified_rollback};

    #[test]
    fn action_published_candidates_survive_every_unverified_rollback() {
        assert!(survives_unverified_rollback(false, false));
        assert!(survives_unverified_rollback(true, true));
        assert!(!survives_unverified_rollback(true, false));
    }

    #[test]
    fn covers_unobserved_claims_and_parallel_lanes_without_spending_the_envelope() {
        assert_eq!(limit(128, 6, 8, &[0, 1, 2, 3], &[]), 8);
        assert_eq!(limit(128, 6, 8, &(0..16).collect::<Vec<_>>(), &[]), 24);
        assert_eq!(limit(5, 6, 8, &(0..16).collect::<Vec<_>>(), &[]), 5);
        assert_eq!(limit(128, 12, 8, &[0], &[]), 12);
        assert_eq!(
            limit(
                20,
                6,
                8,
                &(0..100).collect::<Vec<_>>(),
                &(0..90).collect::<Vec<_>>(),
            ),
            18,
            "covered claims must not disable affordable complete uncovered coverage"
        );
    }

    #[test]
    fn runtime_policy_can_raise_but_not_lower_the_safe_cohort_floor() {
        assert_eq!(limit(128, 6, 12, &[0, 1, 2], &[]), 12);
        assert_eq!(limit(128, 12, 4, &[0, 1, 2], &[]), 12);
    }

    #[test]
    fn canonical_artifact_replays_without_requiring_the_artifact_to_clone() {
        struct NotClone(u8);

        let canonical = CanonicalArtifact::new(vec![3, 5, 8]);
        let replayed = canonical
            .decode_with(|bytes| Ok::<_, ()>(NotClone(bytes.iter().copied().sum())))
            .unwrap();

        assert_eq!(replayed.0, 16);
        assert_eq!(canonical.resident_bytes(), 3);
    }

    #[test]
    fn policy_selection_plan_is_pure_bounded_and_preserves_the_deferred_suffix() {
        let incumbent = RuntimePolicyRevision::bootstrap();
        let challenger = RuntimePolicyKernel::mutate(
            incumbent,
            RuntimePolicyMutation::VerificationCohort(SignedStep::Increase),
        )
        .unwrap();

        let incumbent_plan = PolicySelectionPlan::for_policy(incumbent, 20, 20, 1, &[0_u8], &[0]);
        let challenger_plan = PolicySelectionPlan::for_policy(challenger, 20, 20, 1, &[0_u8], &[0]);

        assert_eq!(incumbent_plan.selected(), 0..8);
        assert_eq!(incumbent_plan.deferred(), 8..20);
        assert_eq!(challenger_plan.selected(), 0..9);
        assert_eq!(challenger_plan.deferred(), 9..20);
        assert_eq!(
            PolicySelectionPlan::for_policy(incumbent, 20, 4, 1, &[0_u8], &[0]).selected(),
            0..4
        );
    }
}
