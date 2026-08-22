#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AllocationQueue {
    ProtectedOrigin = 1,
    ProtectedDerived = 2,
    Learned = 3,
    Bootstrap = 4,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CooperativeSelection {
    pub(crate) index: usize,
    pub(crate) queue: AllocationQueue,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OperationalPartition {
    ProtectedOrigin,
    ProtectedDerived,
    Unprotected,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OperationalSelection {
    pub(crate) partition: OperationalPartition,
    pub(crate) index: usize,
    pub(crate) queue: AllocationQueue,
}

pub(crate) fn operational_ranked_selections(
    protected_origin_count: usize,
    protected_derived_count: usize,
    bootstrap: &[usize],
    learned: Option<&[usize]>,
    limit: usize,
) -> Vec<OperationalSelection> {
    let mut output = Vec::with_capacity(
        limit.min(
            protected_origin_count
                .saturating_add(protected_derived_count)
                .saturating_add(bootstrap.len()),
        ),
    );
    output.extend(
        (0..protected_origin_count)
            .take(limit)
            .map(|index| OperationalSelection {
                partition: OperationalPartition::ProtectedOrigin,
                index,
                queue: AllocationQueue::ProtectedOrigin,
            }),
    );
    let unprotected = learned.map_or_else(
        || {
            bootstrap
                .iter()
                .copied()
                .map(|index| CooperativeSelection {
                    index,
                    queue: AllocationQueue::Bootstrap,
                })
                .collect::<Vec<_>>()
        },
        |learned| cooperative_ranked_selections(bootstrap, learned, bootstrap.len()),
    );
    let mut derived_cursor = 0;
    let mut unprotected_cursor = 0;
    while output.len() < limit {
        let before = output.len();
        for _ in 0..2 {
            if output.len() == limit || derived_cursor == protected_derived_count {
                break;
            }
            output.push(OperationalSelection {
                partition: OperationalPartition::ProtectedDerived,
                index: derived_cursor,
                queue: AllocationQueue::ProtectedDerived,
            });
            derived_cursor += 1;
        }
        for _ in 0..6 {
            if output.len() == limit || unprotected_cursor == unprotected.len() {
                break;
            }
            let selection = unprotected[unprotected_cursor];
            output.push(OperationalSelection {
                partition: OperationalPartition::Unprotected,
                index: selection.index,
                queue: selection.queue,
            });
            unprotected_cursor += 1;
        }
        if output.len() == before {
            break;
        }
    }
    output
}

pub(crate) fn cooperative_ranked_selections(
    bootstrap: &[usize],
    learned: &[usize],
    limit: usize,
) -> Vec<CooperativeSelection> {
    assert_eq!(
        bootstrap.len(),
        learned.len(),
        "cooperative policy rankings must cover the same candidates"
    );
    let candidate_count = bootstrap.len();
    debug_assert!({
        let mut bootstrap_seen = vec![false; candidate_count];
        let mut learned_seen = vec![false; candidate_count];
        bootstrap.iter().all(|index| {
            *index < candidate_count && !std::mem::replace(&mut bootstrap_seen[*index], true)
        }) && learned.iter().all(|index| {
            *index < candidate_count && !std::mem::replace(&mut learned_seen[*index], true)
        })
    });
    let limit = limit.min(candidate_count);
    let mut selected = vec![false; candidate_count];
    let mut output = Vec::with_capacity(limit);
    let mut bootstrap_cursor = 0;
    let mut learned_cursor = 0;
    let take_unique = |ranking: &[usize],
                       cursor: &mut usize,
                       count: usize,
                       queue: AllocationQueue,
                       output: &mut Vec<CooperativeSelection>,
                       selected: &mut [bool]| {
        let target = output.len().saturating_add(count);
        while output.len() < target && *cursor < ranking.len() {
            let index = ranking[*cursor];
            *cursor += 1;
            if !selected[index] {
                selected[index] = true;
                output.push(CooperativeSelection { index, queue });
            }
        }
    };
    while output.len() < limit {
        let before = output.len();
        take_unique(
            learned,
            &mut learned_cursor,
            usize::from(output.len() < limit),
            AllocationQueue::Learned,
            &mut output,
            &mut selected,
        );
        take_unique(
            bootstrap,
            &mut bootstrap_cursor,
            usize::from(output.len() < limit),
            AllocationQueue::Bootstrap,
            &mut output,
            &mut selected,
        );
        if output.len() == before {
            break;
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operational_policy_keeps_protection_and_distinguishes_bootstrap_from_cooperation() {
        let bootstrap = [0, 1, 2, 3];
        let learned = [3, 2, 1, 0];
        let bootstrap_only = operational_ranked_selections(2, 3, &bootstrap, None, 8);
        let cooperative = operational_ranked_selections(2, 3, &bootstrap, Some(&learned), 8);

        assert_eq!(
            bootstrap_only
                .iter()
                .map(|selection| (selection.partition, selection.index, selection.queue))
                .collect::<Vec<_>>(),
            vec![
                (
                    OperationalPartition::ProtectedOrigin,
                    0,
                    AllocationQueue::ProtectedOrigin
                ),
                (
                    OperationalPartition::ProtectedOrigin,
                    1,
                    AllocationQueue::ProtectedOrigin
                ),
                (
                    OperationalPartition::ProtectedDerived,
                    0,
                    AllocationQueue::ProtectedDerived
                ),
                (
                    OperationalPartition::ProtectedDerived,
                    1,
                    AllocationQueue::ProtectedDerived
                ),
                (
                    OperationalPartition::Unprotected,
                    0,
                    AllocationQueue::Bootstrap
                ),
                (
                    OperationalPartition::Unprotected,
                    1,
                    AllocationQueue::Bootstrap
                ),
                (
                    OperationalPartition::Unprotected,
                    2,
                    AllocationQueue::Bootstrap
                ),
                (
                    OperationalPartition::Unprotected,
                    3,
                    AllocationQueue::Bootstrap
                ),
            ]
        );
        assert_eq!(
            cooperative[4..]
                .iter()
                .map(|selection| (selection.index, selection.queue))
                .collect::<Vec<_>>(),
            vec![
                (3, AllocationQueue::Learned),
                (0, AllocationQueue::Bootstrap),
                (2, AllocationQueue::Learned),
                (1, AllocationQueue::Bootstrap),
            ]
        );
    }

    #[test]
    fn zero_information_cooperation_preserves_candidates_but_not_bootstrap_attribution() {
        let bootstrap = [0, 1, 2, 3];
        let bootstrap_only = operational_ranked_selections(1, 1, &bootstrap, None, 6);
        let zero_information = operational_ranked_selections(1, 1, &bootstrap, Some(&bootstrap), 6);

        assert_eq!(
            bootstrap_only
                .iter()
                .map(|selection| (selection.partition, selection.index))
                .collect::<Vec<_>>(),
            zero_information
                .iter()
                .map(|selection| (selection.partition, selection.index))
                .collect::<Vec<_>>(),
            "zero information must not perturb the selected Candidate prefix"
        );
        assert_eq!(
            bootstrap_only[2].queue,
            AllocationQueue::Bootstrap,
            "the real Bootstrap branch attributes every unprotected Candidate to Bootstrap"
        );
        assert_eq!(
            zero_information[2].queue,
            AllocationQueue::Learned,
            "cooperation diverges at the first unprotected slot by applying its 1:1 learned-first rule"
        );
    }
}
