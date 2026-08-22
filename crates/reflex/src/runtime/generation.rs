use std::collections::HashMap;

use crate::StructuralLocation;

pub(super) struct ClaimCompleteBatch<A> {
    pub(super) groups: Vec<Vec<A>>,
    pub(super) truncated: bool,
    pub(super) working_metadata_bytes: u64,
}

pub(super) fn group_locations(
    locations: &[StructuralLocation],
    claim_by_artifact: &[[u8; 32]],
) -> Vec<Vec<StructuralLocation>> {
    let mut indexes = HashMap::new();
    let mut groups = Vec::<Vec<StructuralLocation>>::new();
    for location in locations {
        let claim = *claim_by_artifact
            .get(location.artifact_index())
            .expect("generated structural locations refer to the Search Frontier");
        let next_index = indexes.len();
        let index = *indexes.entry(claim).or_insert(next_index);
        if index == groups.len() {
            groups.push(Vec::new());
        }
        groups[index].push(*location);
    }
    groups
}

pub(super) fn fill_claim_complete<G, A, E>(
    input_groups: &[G],
    limit: usize,
    mut fill: impl FnMut(&G, usize, &mut Vec<A>) -> Result<bool, E>,
) -> Result<ClaimCompleteBatch<A>, E> {
    assert!(!input_groups.is_empty());
    assert!(input_groups.len() <= limit);

    let base = limit / input_groups.len();
    let remainder = limit % input_groups.len();
    let mut groups = Vec::with_capacity(input_groups.len());
    for (index, input) in input_groups.iter().enumerate() {
        let group_limit = base + usize::from(index < remainder);
        let (values, truncated) = fill_bounded(input, group_limit, &mut fill)?;
        groups.push(GroupFill {
            values,
            truncated,
            limit: group_limit,
        });
    }

    loop {
        let used = groups.iter().map(|group| group.values.len()).sum::<usize>();
        let remaining = limit - used;
        if remaining == 0 {
            break;
        }
        let expandable = groups
            .iter()
            .filter(|group| group.values.len() == group.limit)
            .count();
        if expandable == 0 {
            break;
        }
        let base_increase = remaining / expandable;
        let increase_remainder = remaining % expandable;
        let mut position = 0_usize;
        for (index, group) in groups.iter_mut().enumerate() {
            if group.values.len() != group.limit {
                continue;
            }
            let increase = base_increase + usize::from(position < increase_remainder);
            position += 1;
            if increase == 0 {
                continue;
            }
            let previous_len = group.values.len();
            let expanded_limit = group.limit + increase;
            group.values = Vec::new();
            let (values, truncated) =
                fill_bounded(&input_groups[index], expanded_limit, &mut fill)?;
            assert!(values.len() >= previous_len);
            group.values = values;
            group.truncated = truncated;
            group.limit = expanded_limit;
        }
    }

    let working_metadata_bytes =
        (groups.capacity() as u64).saturating_mul(std::mem::size_of::<GroupFill<A>>() as u64);
    Ok(ClaimCompleteBatch {
        truncated: groups.iter().any(|group| group.truncated),
        groups: groups.into_iter().map(|group| group.values).collect(),
        working_metadata_bytes,
    })
}

fn fill_bounded<G, A, E>(
    input: &G,
    limit: usize,
    fill: &mut impl FnMut(&G, usize, &mut Vec<A>) -> Result<bool, E>,
) -> Result<(Vec<A>, bool), E> {
    let mut values = Vec::with_capacity(limit);
    let mut truncated = fill(input, limit, &mut values)?;
    assert!(values.len() <= limit);
    if values.len() == limit {
        return Ok((values, truncated));
    }

    let exact_len = values.len();
    drop(values);
    let mut exact = Vec::with_capacity(exact_len);
    if exact_len != 0 {
        truncated |= fill(input, exact_len, &mut exact)?;
        assert_eq!(exact.len(), exact_len);
    }
    Ok((exact, truncated))
}

struct GroupFill<A> {
    values: Vec<A>,
    truncated: bool,
    limit: usize,
}

#[cfg(test)]
mod tests {
    use super::{fill_claim_complete, group_locations};
    use crate::StructuralLocation;

    #[test]
    fn unused_claim_capacity_is_redistributed_without_starving_any_claim() {
        let groups = [0_usize, 1, 2];
        let supplies = [10_usize, 1, 10];

        let result = fill_claim_complete(&groups, 9, |claim, limit, output| {
            let emitted = supplies[*claim].min(limit);
            output.extend(std::iter::repeat_n(*claim, emitted));
            Ok::<_, ()>(emitted < supplies[*claim])
        })
        .unwrap();

        assert_eq!(
            result.groups.iter().map(Vec::len).collect::<Vec<_>>(),
            [4, 1, 4]
        );
        assert!(result.groups.iter().map(Vec::capacity).sum::<usize>() <= 9);
        assert!(result.truncated);
    }

    #[test]
    fn candidate_fanout_remains_claim_complete_after_application_enumeration() {
        let application_groups = vec![vec![0_usize], vec![1], vec![2]];
        let candidate_supplies = [12_usize, 1, 6];

        let result = fill_claim_complete(&application_groups, 9, |applications, limit, output| {
            let claim = applications[0];
            let emitted = candidate_supplies[claim].min(limit);
            output.extend(std::iter::repeat_n(claim, emitted));
            Ok::<_, ()>(emitted < candidate_supplies[claim])
        })
        .unwrap();

        assert_eq!(
            result.groups.iter().map(Vec::len).collect::<Vec<_>>(),
            [4, 1, 4]
        );
        assert!(result.groups.iter().map(Vec::capacity).sum::<usize>() <= 9);
    }

    #[test]
    fn structural_locations_are_grouped_by_claim_in_first_occurrence_order() {
        let claims = [[1; 32], [2; 32], [1; 32]];
        let locations = [
            StructuralLocation::new(0, 5),
            StructuralLocation::new(1, 6),
            StructuralLocation::new(2, 7),
        ];

        let groups = group_locations(&locations, &claims);

        assert_eq!(
            groups
                .iter()
                .map(|group| {
                    group
                        .iter()
                        .map(|location| location.artifact_index())
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>(),
            [vec![0, 2], vec![1]]
        );
    }
}
