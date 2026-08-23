use super::arena::{MAXIMUM_FEATURES_PER_OPPORTUNITY, OpportunityKind, OpportunitySpec};
use super::ecology::SpecialistRevision;
use super::types::{FeatureSchemaId, IntelligenceError, IntelligenceLimits, RoutingFamilyId};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct GeneralistRoutingKey {
    kind: OpportunityKind,
    schema: FeatureSchemaId,
    feature_count: u8,
}

impl GeneralistRoutingKey {
    fn new(opportunity: OpportunitySpec, feature_count: usize) -> Option<Self> {
        Some(Self {
            kind: opportunity.kind,
            schema: opportunity.feature_schema,
            feature_count: u8::try_from(feature_count).ok()?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct RoutingKey {
    family: RoutingFamilyId,
    route: GeneralistRoutingKey,
}

impl RoutingKey {
    fn new(opportunity: OpportunitySpec, feature_count: usize) -> Option<Self> {
        Some(Self {
            family: opportunity.routing_family,
            route: GeneralistRoutingKey::new(opportunity, feature_count)?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct RoutingEntry<Key> {
    key: Key,
    specialist_index: u32,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct RoutingIndex {
    exact_entries: Vec<RoutingEntry<RoutingKey>>,
    generalist_entries: Vec<RoutingEntry<GeneralistRoutingKey>>,
    maximum_fanout: usize,
}

pub(super) struct RoutedIndices<'a> {
    exact_entries: &'a [RoutingEntry<RoutingKey>],
    generalist_entries: &'a [RoutingEntry<GeneralistRoutingKey>],
    exact_cursor: usize,
    generalist_cursor: usize,
}

impl Iterator for RoutedIndices<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        let specialist_index = if let Some(entry) = self.exact_entries.get(self.exact_cursor) {
            self.exact_cursor += 1;
            entry.specialist_index
        } else {
            let entry = self.generalist_entries.get(self.generalist_cursor)?;
            self.generalist_cursor += 1;
            entry.specialist_index
        };
        Some(
            usize::try_from(specialist_index).expect("a stored Specialist index always fits usize"),
        )
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let exact = self.exact_entries.len().saturating_sub(self.exact_cursor);
        let generalist = self
            .generalist_entries
            .len()
            .saturating_sub(self.generalist_cursor);
        let remaining = exact.saturating_add(generalist);
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for RoutedIndices<'_> {}

impl RoutingIndex {
    pub(super) fn build(
        specialists: &[SpecialistRevision],
        limits: IntelligenceLimits,
    ) -> Result<Self, IntelligenceError> {
        let (exact_count, generalist_count) = specialists
            .iter()
            .filter(|specialist| specialist.active())
            .try_fold((0_usize, 0_usize), |(exact, generalist), specialist| {
                let memberships = specialist.route_membership_count()?;
                if specialist.routes_all_families() {
                    Ok((
                        exact,
                        generalist
                            .checked_add(memberships)
                            .ok_or(IntelligenceError::ResourceOverflow)?,
                    ))
                } else {
                    Ok((
                        exact
                            .checked_add(memberships)
                            .ok_or(IntelligenceError::ResourceOverflow)?,
                        generalist,
                    ))
                }
            })?;
        let retained_bytes = exact_count
            .checked_mul(std::mem::size_of::<RoutingEntry<RoutingKey>>())
            .and_then(|bytes| {
                generalist_count
                    .checked_mul(std::mem::size_of::<RoutingEntry<GeneralistRoutingKey>>())
                    .and_then(|generalist_bytes| bytes.checked_add(generalist_bytes))
            })
            .ok_or(IntelligenceError::ResourceOverflow)?;
        if retained_bytes > limits.maximum_model_bytes {
            return Err(IntelligenceError::CapacityExceeded);
        }
        let mut exact_entries = reserved(exact_count)?;
        let mut generalist_entries = reserved(generalist_count)?;
        for (specialist_index, specialist) in specialists.iter().enumerate() {
            if !specialist.active() {
                continue;
            }
            let specialist_index =
                u32::try_from(specialist_index).map_err(|_| IntelligenceError::CapacityExceeded)?;
            for kind in specialist.opportunity_kinds() {
                for schema in specialist.feature_schemas() {
                    for feature_count in 0..=MAXIMUM_FEATURES_PER_OPPORTUNITY {
                        if !specialist.model().accepts_feature_count(feature_count) {
                            continue;
                        }
                        let route = GeneralistRoutingKey {
                            kind: *kind,
                            schema: *schema,
                            feature_count: u8::try_from(feature_count)
                                .expect("the bounded Opportunity feature count always fits u8"),
                        };
                        if specialist.routes_all_families() {
                            generalist_entries.push(RoutingEntry {
                                key: route,
                                specialist_index,
                            });
                        } else {
                            for family in specialist.routing_families() {
                                exact_entries.push(RoutingEntry {
                                    key: RoutingKey {
                                        family: *family,
                                        route,
                                    },
                                    specialist_index,
                                });
                            }
                        }
                    }
                }
            }
        }
        debug_assert_eq!(exact_entries.len(), exact_count);
        debug_assert_eq!(generalist_entries.len(), generalist_count);
        exact_entries.sort_unstable_by_key(|entry| (entry.key, entry.specialist_index));
        generalist_entries.sort_unstable_by_key(|entry| (entry.key, entry.specialist_index));
        let maximum_fanout = validate_fanout(
            &exact_entries,
            &generalist_entries,
            limits.maximum_specialists_per_route,
        )?;
        Ok(Self {
            exact_entries,
            generalist_entries,
            maximum_fanout,
        })
    }

    pub(super) fn route(
        &self,
        opportunity: OpportunitySpec,
        feature_count: usize,
    ) -> RoutedIndices<'_> {
        let Some(exact_key) = RoutingKey::new(opportunity, feature_count) else {
            return RoutedIndices::empty();
        };
        RoutedIndices {
            exact_entries: matching_entries(&self.exact_entries, exact_key),
            generalist_entries: matching_entries(&self.generalist_entries, exact_key.route),
            exact_cursor: 0,
            generalist_cursor: 0,
        }
    }

    pub(super) const fn maximum_fanout(&self) -> usize {
        self.maximum_fanout
    }

    pub(super) fn heap_bytes(&self) -> usize {
        self.exact_entries
            .capacity()
            .saturating_mul(std::mem::size_of::<RoutingEntry<RoutingKey>>())
            .saturating_add(
                self.generalist_entries
                    .capacity()
                    .saturating_mul(std::mem::size_of::<RoutingEntry<GeneralistRoutingKey>>()),
            )
    }
}

impl RoutedIndices<'_> {
    const fn empty() -> Self {
        Self {
            exact_entries: &[],
            generalist_entries: &[],
            exact_cursor: 0,
            generalist_cursor: 0,
        }
    }
}

fn matching_entries<Key: Copy + Ord>(
    entries: &[RoutingEntry<Key>],
    key: Key,
) -> &[RoutingEntry<Key>] {
    let start = entries.partition_point(|entry| entry.key < key);
    let end = entries[start..]
        .partition_point(|entry| entry.key == key)
        .saturating_add(start);
    &entries[start..end]
}

fn validate_fanout(
    exact_entries: &[RoutingEntry<RoutingKey>],
    generalist_entries: &[RoutingEntry<GeneralistRoutingKey>],
    maximum_specialists_per_route: usize,
) -> Result<usize, IntelligenceError> {
    let mut maximum_fanout =
        validate_group_bound(generalist_entries, maximum_specialists_per_route)?;
    let mut start = 0_usize;
    while start < exact_entries.len() {
        let key = exact_entries[start].key;
        let exact_count = exact_entries[start..].partition_point(|entry| entry.key == key);
        let generalist_count = matching_entries(generalist_entries, key.route).len();
        let combined = exact_count
            .checked_add(generalist_count)
            .ok_or(IntelligenceError::ResourceOverflow)?;
        if combined > maximum_specialists_per_route {
            return Err(IntelligenceError::CapacityExceeded);
        }
        maximum_fanout = maximum_fanout.max(combined);
        start = start.saturating_add(exact_count);
    }
    Ok(maximum_fanout)
}

fn validate_group_bound<Key: Copy + Ord>(
    entries: &[RoutingEntry<Key>],
    limit: usize,
) -> Result<usize, IntelligenceError> {
    let mut maximum = 0_usize;
    let mut start = 0_usize;
    while start < entries.len() {
        let key = entries[start].key;
        let count = entries[start..].partition_point(|entry| entry.key == key);
        if count > limit {
            return Err(IntelligenceError::CapacityExceeded);
        }
        maximum = maximum.max(count);
        start = start.saturating_add(count);
    }
    Ok(maximum)
}

fn reserved<T>(capacity: usize) -> Result<Vec<T>, IntelligenceError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| IntelligenceError::CapacityExceeded)?;
    Ok(values)
}
