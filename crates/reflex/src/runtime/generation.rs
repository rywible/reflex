pub(super) fn select_pending_parents<K: Copy + Eq>(
    pending_indexes: &[usize],
    claim_by_artifact: &[K],
    limit: usize,
) -> Vec<usize> {
    let claims = ordered_distinct_claims(pending_indexes, claim_by_artifact);
    if claims.len() > limit {
        return pending_indexes[..pending_indexes.len().min(limit)].to_vec();
    }
    let mut selected = Vec::with_capacity(pending_indexes.len().min(limit));
    for claim in claims {
        if let Some(index) = pending_indexes
            .iter()
            .find(|index| claim_by_artifact[**index] == claim)
        {
            selected.push(*index);
        }
    }
    for index in pending_indexes {
        if selected.len() == limit {
            break;
        }
        if !selected.contains(index) {
            selected.push(*index);
        }
    }
    selected
}

pub(super) fn parent_capacity<K: Copy + Eq>(
    choice_budget: usize,
    operator_count: usize,
    pending_indexes: &[usize],
    claim_by_artifact: &[K],
) -> usize {
    const COMPLETE_CLAIM_FLOOR: usize = 8;
    if choice_budget == 0 || operator_count == 0 {
        return usize::MAX;
    }
    let ordinary = (choice_budget / operator_count).max(1);
    let claims = ordered_distinct_claims(pending_indexes, claim_by_artifact);
    let complete_claims = if claims.len() <= COMPLETE_CLAIM_FLOOR && claims.len() <= choice_budget {
        claims.len()
    } else {
        0
    };
    ordinary.max(complete_claims)
}

fn ordered_distinct_claims<K: Copy + Eq>(
    pending_indexes: &[usize],
    claim_by_artifact: &[K],
) -> Vec<K> {
    let mut claims = Vec::new();
    for index in pending_indexes {
        let claim = claim_by_artifact[*index];
        if !claims.contains(&claim) {
            claims.push(claim);
        }
    }
    claims
}

#[cfg(test)]
mod tests {
    use super::{parent_capacity, select_pending_parents};

    #[test]
    fn pending_parent_selection_covers_claims_and_leaves_the_unprocessed_tail() {
        let claims = [[1; 32], [1; 32], [2; 32], [1; 32]];
        let pending = [0, 1, 2, 3];

        assert_eq!(select_pending_parents(&pending, &claims, 3), [0, 2, 1]);
        assert_eq!(select_pending_parents(&pending, &claims, 1), [0]);
    }

    #[test]
    fn parent_capacity_covers_small_claim_sets_without_spreading_large_sets_too_thin() {
        let two_claims = [[1; 32], [2; 32]];
        let many_claims = (0_u8..32).map(|value| [value; 32]).collect::<Vec<_>>();
        let many_indexes = (0..many_claims.len()).collect::<Vec<_>>();

        assert_eq!(parent_capacity(8, 8, &[0, 1], &two_claims), 2);
        assert_eq!(parent_capacity(60, 8, &many_indexes, &many_claims), 7);
    }
}
