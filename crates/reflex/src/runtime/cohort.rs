use std::collections::BTreeSet;

use crate::DomainDefinition;

use super::{PendingParent, ProposedCandidate};

const MIN_COHORT: usize = 8;

/// Chooses one bounded Verification Cohort without exposing a tuning surface.
///
/// The policy pays complete claim coverage only for claims that have no
/// retained Verification Experience, preserves ordinary slots for policy
/// comparison on large Seed Scopes, and never exceeds the remaining allowance.
pub(super) fn limit<K: Copy + Ord>(
    remaining: usize,
    worker_lanes: usize,
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
    remaining.min(MIN_COHORT.max(worker_lanes).max(complete_claim_coverage))
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
        .filter(|candidate| !candidate.generated_in_epoch)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::limit;

    #[test]
    fn covers_unobserved_claims_and_parallel_lanes_without_spending_the_envelope() {
        assert_eq!(limit(128, 6, &[0, 1, 2, 3], &[]), 8);
        assert_eq!(limit(128, 6, &(0..16).collect::<Vec<_>>(), &[]), 24);
        assert_eq!(limit(5, 6, &(0..16).collect::<Vec<_>>(), &[]), 5);
        assert_eq!(limit(128, 12, &[0], &[]), 12);
        assert_eq!(
            limit(
                20,
                6,
                &(0..100).collect::<Vec<_>>(),
                &(0..90).collect::<Vec<_>>(),
            ),
            18,
            "covered claims must not disable affordable complete uncovered coverage"
        );
    }
}
