use crate::domain::{
    ApplicationWriter, Candidate, CandidateWriter, DomainDefinition, OperatorAlgebra,
    OperatorEnumerationBatch, StructuralLocation,
};
use crate::knowledge::KnowledgeRevision;
use crate::session::SessionError;

use super::{
    ENUMERATION_COMPLETE, ProposedCandidate, StructuralSummary, operator_feature_values,
    opportunity_features, root_locations, structural_node_count, vector_bytes,
};

pub(super) struct ProposalParents<'a, D: DomainDefinition> {
    pub(super) artifacts: &'a [&'a D::Artifact],
    pub(super) frontier_indexes: &'a [usize],
}

impl<D: DomainDefinition> Copy for ProposalParents<'_, D> {}

impl<D: DomainDefinition> Clone for ProposalParents<'_, D> {
    fn clone(&self) -> Self {
        *self
    }
}

pub(super) struct StructuredRewriteRequest<'a, D: DomainDefinition> {
    pub(super) parents: ProposalParents<'a, D>,
    pub(super) operator_offsets: &'a mut [u64],
    pub(super) limit: usize,
    pub(super) epoch: u64,
    pub(super) permitted_operators: Option<&'a [usize]>,
}

pub(super) struct DerivedOperatorRequest<'a, D: DomainDefinition> {
    pub(super) parents: ProposalParents<'a, D>,
    pub(super) knowledge: &'a KnowledgeRevision,
    pub(super) limit: usize,
    pub(super) epoch: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct ProposalBatch {
    resident_bytes: u64,
    truncated: bool,
}

impl ProposalBatch {
    pub(super) const fn resident_bytes(self) -> u64 {
        self.resident_bytes
    }

    pub(super) const fn truncated(self) -> bool {
        self.truncated
    }
}

/// Private Runtime seam for one bounded candidate-generation strategy.
///
/// Engines may propose and report bounded scratch demand. They never verify,
/// admit, schedule, or publish Artifacts.
pub(super) trait ProposalEngine<D: DomainDefinition, R> {
    fn propose(
        &self,
        domain: &D,
        request: &mut R,
        scratch: &mut <D::Operators as OperatorAlgebra<D>>::Scratch,
        output: &mut Vec<ProposedCandidate<D>>,
    ) -> Result<ProposalBatch, SessionError<D::Error>>;
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct StructuredRewriteEngine;

impl<D: DomainDefinition> ProposalEngine<D, StructuredRewriteRequest<'_, D>>
    for StructuredRewriteEngine
{
    fn propose(
        &self,
        domain: &D,
        request: &mut StructuredRewriteRequest<'_, D>,
        scratch: &mut <D::Operators as OperatorAlgebra<D>>::Scratch,
        output: &mut Vec<ProposedCandidate<D>>,
    ) -> Result<ProposalBatch, SessionError<D::Error>> {
        let mut remaining = request.limit;
        let catalog = domain.operators().catalog();
        assert_eq!(
            request.operator_offsets.len(),
            catalog.len(),
            "pending structured-rewrite cursor must match the installed Operator catalog"
        );
        let locations = root_locations(domain, request.parents.artifacts);
        let mut application_bytes = vector_bytes(&locations);
        for (operator_index, descriptor) in catalog.iter().enumerate() {
            if request
                .permitted_operators
                .is_some_and(|permitted| !permitted.contains(&operator_index))
            {
                continue;
            }
            if remaining == 0 {
                break;
            }
            let offset = request.operator_offsets[operator_index];
            if offset == ENUMERATION_COMPLETE {
                continue;
            }
            let incomplete_operators = request
                .operator_offsets
                .iter()
                .enumerate()
                .skip(operator_index)
                .filter(|(index, offset)| {
                    **offset != ENUMERATION_COMPLETE
                        && request
                            .permitted_operators
                            .is_none_or(|permitted| permitted.contains(index))
                })
                .count();
            let operator_limit = remaining.div_ceil(incomplete_operators);
            let before = output.len();
            application_bytes = application_bytes.max(append_operator_page(
                domain,
                &mut StructuredOperatorPage {
                    parents: request.parents,
                    locations: &locations,
                    descriptor,
                    offset: &mut request.operator_offsets[operator_index],
                    limit: operator_limit,
                    epoch: request.epoch,
                },
                scratch,
                output,
            )?);
            remaining = remaining.saturating_sub(output.len().saturating_sub(before));
        }
        Ok(ProposalBatch {
            resident_bytes: application_bytes,
            truncated: request
                .operator_offsets
                .iter()
                .any(|offset| *offset != ENUMERATION_COMPLETE),
        })
    }
}

struct StructuredOperatorPage<'a, D: DomainDefinition> {
    parents: ProposalParents<'a, D>,
    locations: &'a [StructuralLocation],
    descriptor:
        &'a crate::domain::OperatorDescriptor<<D::Operators as OperatorAlgebra<D>>::Operator>,
    offset: &'a mut u64,
    limit: usize,
    epoch: u64,
}

fn append_operator_page<D: DomainDefinition>(
    domain: &D,
    page: &mut StructuredOperatorPage<'_, D>,
    scratch: &mut <D::Operators as OperatorAlgebra<D>>::Scratch,
    output: &mut Vec<ProposedCandidate<D>>,
) -> Result<u64, SessionError<D::Error>> {
    let skip = usize::try_from(*page.offset).map_err(|_| SessionError::CorruptBundle)?;
    let mut applications = Vec::new();
    let mut application_writer =
        ApplicationWriter::with_window(&mut applications, skip, page.limit);
    domain
        .operators()
        .enumerate_legal(
            OperatorEnumerationBatch::new(
                page.parents.artifacts,
                page.locations,
                std::slice::from_ref(&page.descriptor.operator()),
            ),
            &mut application_writer,
            scratch,
        )
        .map_err(SessionError::Domain)?;
    assert!(
        application_writer.consumed_prefix(),
        "OperatorAlgebra::enumerate_legal ended before the retained Structured Rewrite cursor"
    );
    *page.offset = if application_writer.overflowed() {
        page.offset
            .checked_add(u64::try_from(applications.len()).map_err(|_| SessionError::Resource)?)
            .ok_or(SessionError::Resource)?
    } else {
        ENUMERATION_COMPLETE
    };
    let mut operator_candidates = Vec::new();
    let mut candidate_writer =
        CandidateWriter::with_limit(&mut operator_candidates, applications.len());
    domain
        .operators()
        .apply_batch(&applications, &mut candidate_writer, scratch)
        .map_err(SessionError::Domain)?;
    assert!(
        !candidate_writer.overflowed() && operator_candidates.len() == applications.len(),
        "OperatorAlgebra::apply_batch must emit exactly one Candidate per legal Application"
    );
    let resident = vector_bytes(&applications).max(vector_bytes(&operator_candidates));
    let operator_features = operator_feature_values(page.descriptor.symbol().as_str());
    for mut candidate in operator_candidates {
        let parent = page
            .parents
            .artifacts
            .get(candidate.source_index)
            .ok_or(SessionError::InvalidSeed)?;
        candidate.source_index = *page
            .parents
            .frontier_indexes
            .get(candidate.source_index)
            .ok_or(SessionError::InvalidSeed)?;
        let features = opportunity_features(
            domain,
            StructuralSummary {
                node_count: structural_node_count(domain, parent),
            },
            &candidate.artifact,
            operator_features,
            page.epoch,
            candidate.proposal_features,
        );
        output.push(ProposedCandidate::generated(
            candidate,
            page.descriptor.symbol().as_str().as_bytes().to_vec(),
            features,
            page.epoch,
            page.limit,
            false,
        ));
    }
    Ok(resident)
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct DerivedOperatorEngine;

impl<D: DomainDefinition> ProposalEngine<D, DerivedOperatorRequest<'_, D>>
    for DerivedOperatorEngine
{
    #[expect(
        clippy::too_many_lines,
        reason = "Derived Operator expansion keeps its bounded multi-step provenance in one search transaction"
    )]
    fn propose(
        &self,
        domain: &D,
        request: &mut DerivedOperatorRequest<'_, D>,
        scratch: &mut <D::Operators as OperatorAlgebra<D>>::Scratch,
        output: &mut Vec<ProposedCandidate<D>>,
    ) -> Result<ProposalBatch, SessionError<D::Error>> {
        const MAX_DERIVED_CANDIDATES_PER_OPERATOR: usize = 1_024;

        let mut application_bytes = 0_u64;
        let mut truncated = false;
        let limit = request.limit.min(MAX_DERIVED_CANDIDATES_PER_OPERATOR);
        if limit == 0 {
            return Ok(ProposalBatch::default());
        }
        let mut emitted = 0_usize;
        for derived in request
            .knowledge
            .operators()
            .iter()
            .filter(|operator| operator.active())
        {
            let operator_limit = limit.saturating_sub(emitted);
            if operator_limit == 0 {
                break;
            }
            let mut current = Vec::<Candidate<D>>::new();
            for (step_index, step) in derived.steps().iter().enumerate() {
                let Some(descriptor) = domain
                    .operators()
                    .catalog()
                    .iter()
                    .find(|descriptor| descriptor.symbol().as_str().as_bytes() == step)
                else {
                    return Err(SessionError::CorruptBundle);
                };
                let stage_parents = if step_index == 0 {
                    request.parents.artifacts[..request.parents.artifacts.len().min(operator_limit)]
                        .to_vec()
                } else {
                    current
                        .iter()
                        .map(|candidate| &candidate.artifact)
                        .collect()
                };
                let locations = root_locations(domain, &stage_parents);
                let mut applications = Vec::new();
                let mut application_writer =
                    ApplicationWriter::with_limit(&mut applications, operator_limit);
                domain
                    .operators()
                    .enumerate_legal(
                        OperatorEnumerationBatch::new(
                            &stage_parents,
                            &locations,
                            std::slice::from_ref(&descriptor.operator()),
                        ),
                        &mut application_writer,
                        scratch,
                    )
                    .map_err(SessionError::Domain)?;
                truncated |= application_writer.overflowed();
                application_bytes = application_bytes.saturating_add(vector_bytes(&applications));
                let mut next = Vec::new();
                let mut candidate_writer = CandidateWriter::with_limit(&mut next, operator_limit);
                domain
                    .operators()
                    .apply_batch(&applications, &mut candidate_writer, scratch)
                    .map_err(SessionError::Domain)?;
                assert!(
                    !candidate_writer.overflowed() && next.len() == applications.len(),
                    "OperatorAlgebra::apply_batch must emit exactly one Candidate per legal Application"
                );
                if step_index > 0 {
                    for candidate in &mut next {
                        let Some(parent) = current.get(candidate.source_index) else {
                            return Err(SessionError::CorruptBundle);
                        };
                        candidate.source_index = parent.source_index;
                    }
                }
                current = next;
                if current.is_empty() {
                    break;
                }
            }
            let symbol = std::str::from_utf8(derived.symbol())
                .expect("canonical Derived Operator symbols are UTF-8");
            let operator_features = operator_feature_values(symbol);
            emitted = emitted.saturating_add(current.len());
            for mut candidate in current {
                let parent = request
                    .parents
                    .artifacts
                    .get(candidate.source_index)
                    .ok_or(SessionError::CorruptBundle)?;
                candidate.source_index = *request
                    .parents
                    .frontier_indexes
                    .get(candidate.source_index)
                    .ok_or(SessionError::CorruptBundle)?;
                let features = opportunity_features(
                    domain,
                    StructuralSummary {
                        node_count: structural_node_count(domain, parent),
                    },
                    &candidate.artifact,
                    operator_features,
                    request.epoch,
                    candidate.proposal_features,
                );
                output.push(ProposedCandidate::generated(
                    candidate,
                    derived.symbol().to_vec(),
                    features,
                    request.epoch,
                    operator_limit,
                    derived.protected_exploration(),
                ));
            }
        }
        Ok(ProposalBatch {
            resident_bytes: application_bytes,
            truncated,
        })
    }
}
