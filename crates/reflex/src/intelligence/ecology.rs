use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use super::arena::{OpportunityKind, OpportunitySpec};
use super::codec::Decoder;
use super::forecast::ForecastAxis;
use super::model::CompactModel;
use super::routing::RoutingIndex;
use super::types::{
    FeatureSchemaId, IntelligenceError, IntelligenceLimits, RoleId, RoutingFamilyId,
    SpecialistRevisionId,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SpecialistMandate {
    role: RoleId,
    axes: Vec<ForecastAxis>,
    opportunity_kinds: Vec<OpportunityKind>,
    feature_schemas: Vec<FeatureSchemaId>,
    routing_families: Vec<RoutingFamilyId>,
}

impl SpecialistMandate {
    #[cfg(test)]
    pub(crate) fn all(
        role: RoleId,
        axes: impl IntoIterator<Item = ForecastAxis>,
    ) -> Result<Self, IntelligenceError> {
        Self::for_opportunities(
            role,
            axes,
            [OpportunityKind::Candidate],
            [OpportunitySpec::candidate_schema()],
        )
    }

    #[cfg(test)]
    pub(crate) fn for_opportunities(
        role: RoleId,
        axes: impl IntoIterator<Item = ForecastAxis>,
        opportunity_kinds: impl IntoIterator<Item = OpportunityKind>,
        feature_schemas: impl IntoIterator<Item = FeatureSchemaId>,
    ) -> Result<Self, IntelligenceError> {
        Self::for_routing_families(
            role,
            axes,
            opportunity_kinds,
            feature_schemas,
            [RoutingFamilyId::generic()],
        )
    }

    pub(crate) fn for_routing_families(
        role: RoleId,
        axes: impl IntoIterator<Item = ForecastAxis>,
        opportunity_kinds: impl IntoIterator<Item = OpportunityKind>,
        feature_schemas: impl IntoIterator<Item = FeatureSchemaId>,
        routing_families: impl IntoIterator<Item = RoutingFamilyId>,
    ) -> Result<Self, IntelligenceError> {
        let mut axes = axes.into_iter().collect::<Vec<_>>();
        let mut opportunity_kinds = opportunity_kinds.into_iter().collect::<Vec<_>>();
        let mut feature_schemas = feature_schemas.into_iter().collect::<Vec<_>>();
        let mut routing_families = routing_families.into_iter().collect::<Vec<_>>();
        if !unique_nonempty(&axes)
            || !unique_nonempty(&opportunity_kinds)
            || !unique_nonempty(&feature_schemas)
            || (!routing_families.is_empty() && !unique_nonempty(&routing_families))
            || axes.len() > 32
            || opportunity_kinds.len() > 16
            || feature_schemas.len() > 16
            || routing_families.len() > 16
        {
            return Err(IntelligenceError::InvalidMandate);
        }
        axes.sort_unstable();
        opportunity_kinds.sort_unstable();
        feature_schemas.sort_unstable();
        routing_families.sort_unstable();
        Ok(Self {
            role,
            axes,
            opportunity_kinds,
            feature_schemas,
            routing_families,
        })
    }

    pub(super) fn permits(&self, axis: ForecastAxis) -> bool {
        self.axes.contains(&axis)
    }

    fn permits_opportunity(&self, opportunity: OpportunitySpec) -> bool {
        self.opportunity_kinds.contains(&opportunity.kind)
            && self.feature_schemas.contains(&opportunity.feature_schema)
            && (self.routing_families.is_empty()
                || self.routing_families.contains(&opportunity.routing_family))
    }

    fn encode_canonical(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.role.0);
        output.extend_from_slice(&(self.opportunity_kinds.len() as u64).to_le_bytes());
        output.extend(self.opportunity_kinds.iter().map(|kind| *kind as u8));
        output.extend_from_slice(&(self.feature_schemas.len() as u64).to_le_bytes());
        for schema in &self.feature_schemas {
            output.extend_from_slice(&schema.0);
        }
        output.extend_from_slice(&(self.routing_families.len() as u64).to_le_bytes());
        for family in &self.routing_families {
            output.extend_from_slice(&family.0);
        }
        output.extend_from_slice(&(self.axes.len() as u64).to_le_bytes());
        output.extend(self.axes.iter().map(|axis| *axis as u8));
    }

    fn decode_canonical(input: &mut Decoder<'_>) -> Result<Self, ()> {
        let role = RoleId(input.read_digest()?);
        let kind_count = read_count(input, 16, 1)?;
        let mut opportunity_kinds = Vec::with_capacity(kind_count);
        for _ in 0..kind_count {
            opportunity_kinds.push(OpportunityKind::decode(input.read_u8()?)?);
        }
        let schema_count = read_count(input, 16, 32)?;
        let mut feature_schemas = Vec::with_capacity(schema_count);
        for _ in 0..schema_count {
            feature_schemas.push(FeatureSchemaId(input.read_digest()?));
        }
        let family_count = read_optional_count(input, 16, 32)?;
        let mut routing_families = Vec::with_capacity(family_count);
        for _ in 0..family_count {
            routing_families.push(RoutingFamilyId(input.read_digest()?));
        }
        let axis_count = read_count(input, 32, 1)?;
        let mut axes = Vec::with_capacity(axis_count);
        for _ in 0..axis_count {
            axes.push(ForecastAxis::decode(input.read_u8()?)?);
        }
        Self::for_routing_families(
            role,
            axes,
            opportunity_kinds,
            feature_schemas,
            routing_families,
        )
        .map_err(|_| ())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SpecialistStatus {
    Active,
    Retired,
}

#[cfg(any(test, feature = "internal-experiments"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SpecialistLifecycle {
    Spawn,
    Split,
    Merge,
    Distill,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SpecialistOrigin {
    Spawn,
    Split(SpecialistRevisionId),
    Merge(Vec<SpecialistRevisionId>),
    Distill(Vec<SpecialistRevisionId>),
}

impl SpecialistOrigin {
    #[cfg(any(test, feature = "internal-experiments"))]
    const fn lifecycle(&self) -> SpecialistLifecycle {
        match self {
            Self::Spawn => SpecialistLifecycle::Spawn,
            Self::Split(_) => SpecialistLifecycle::Split,
            Self::Merge(_) => SpecialistLifecycle::Merge,
            Self::Distill(_) => SpecialistLifecycle::Distill,
        }
    }

    fn encode_canonical(&self, output: &mut Vec<u8>) {
        match self {
            Self::Spawn => output.push(1),
            Self::Split(parent) => {
                output.push(2);
                output.extend_from_slice(&parent.0);
            }
            Self::Merge(parents) | Self::Distill(parents) => {
                output.push(if matches!(self, Self::Merge(_)) { 3 } else { 4 });
                output.extend_from_slice(&(parents.len() as u64).to_le_bytes());
                for parent in parents {
                    output.extend_from_slice(&parent.0);
                }
            }
        }
    }

    fn decode_canonical(input: &mut Decoder<'_>) -> Result<Self, ()> {
        match input.read_u8()? {
            1 => Ok(Self::Spawn),
            2 => Ok(Self::Split(SpecialistRevisionId(input.read_digest()?))),
            tag @ (3 | 4) => {
                let count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
                if count < 2 || count > input.remaining().saturating_div(32) {
                    return Err(());
                }
                let mut parents = Vec::with_capacity(count);
                for _ in 0..count {
                    parents.push(SpecialistRevisionId(input.read_digest()?));
                }
                if tag == 3 {
                    Ok(Self::Merge(parents))
                } else {
                    Ok(Self::Distill(parents))
                }
            }
            _ => Err(()),
        }
    }

    fn heap_bytes(&self) -> usize {
        match self {
            Self::Merge(parents) | Self::Distill(parents) => parents
                .capacity()
                .saturating_mul(std::mem::size_of::<SpecialistRevisionId>()),
            Self::Spawn | Self::Split(_) => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SpecialistRevision {
    id: SpecialistRevisionId,
    mandate: SpecialistMandate,
    model: CompactModel,
    status: SpecialistStatus,
    origin: SpecialistOrigin,
}

impl SpecialistRevision {
    #[expect(
        clippy::unnecessary_wraps,
        reason = "specialist construction is intentionally fallible at the private Ecology boundary so future compact model families can add validation without widening every caller"
    )]
    pub(crate) fn new(
        mandate: SpecialistMandate,
        model: CompactModel,
    ) -> Result<Self, IntelligenceError> {
        let mut encoded = Vec::new();
        mandate.encode_canonical(&mut encoded);
        model.encode_canonical(&mut encoded);
        let mut digest = Sha256::new();
        digest.update(b"reflex-specialist-revision-v1\0");
        digest.update(encoded);
        Ok(Self {
            id: SpecialistRevisionId(digest.finalize().into()),
            mandate,
            model,
            status: SpecialistStatus::Active,
            origin: SpecialistOrigin::Spawn,
        })
    }

    pub(crate) const fn id(&self) -> SpecialistRevisionId {
        self.id
    }

    pub(super) const fn role(&self) -> RoleId {
        self.mandate.role
    }

    pub(super) const fn active(&self) -> bool {
        matches!(self.status, SpecialistStatus::Active)
    }

    pub(super) const fn model(&self) -> &CompactModel {
        &self.model
    }

    pub(super) fn support(&self) -> u32 {
        self.model.minimum_support()
    }

    pub(super) fn opportunity_kinds(&self) -> &[OpportunityKind] {
        &self.mandate.opportunity_kinds
    }

    pub(super) fn feature_schemas(&self) -> &[FeatureSchemaId] {
        &self.mandate.feature_schemas
    }

    pub(super) fn routing_families(&self) -> &[RoutingFamilyId] {
        &self.mandate.routing_families
    }

    pub(super) const fn routes_all_families(&self) -> bool {
        self.mandate.routing_families.is_empty()
    }

    pub(super) fn route_membership_count(&self) -> Result<usize, IntelligenceError> {
        let feature_counts = (0..=super::arena::MAXIMUM_FEATURES_PER_OPPORTUNITY)
            .filter(|feature_count| self.model.accepts_feature_count(*feature_count))
            .count();
        self.mandate
            .routing_families
            .len()
            .max(1)
            .checked_mul(self.mandate.opportunity_kinds.len())
            .and_then(|count| count.checked_mul(self.mandate.feature_schemas.len()))
            .and_then(|count| count.checked_mul(feature_counts))
            .ok_or(IntelligenceError::ResourceOverflow)
    }

    pub(super) fn permits(&self, axis: ForecastAxis) -> bool {
        self.mandate.permits(axis)
    }

    pub(super) fn permits_opportunity(
        &self,
        opportunity: OpportunitySpec,
        feature_count: usize,
    ) -> bool {
        self.mandate.permits_opportunity(opportunity)
            && self.model.accepts_feature_count(feature_count)
    }

    pub(super) fn resident_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(
                self.mandate
                    .axes
                    .capacity()
                    .saturating_mul(std::mem::size_of::<ForecastAxis>()),
            )
            .saturating_add(
                self.mandate
                    .opportunity_kinds
                    .capacity()
                    .saturating_mul(std::mem::size_of::<OpportunityKind>()),
            )
            .saturating_add(
                self.mandate
                    .feature_schemas
                    .capacity()
                    .saturating_mul(std::mem::size_of::<FeatureSchemaId>()),
            )
            .saturating_add(
                self.mandate
                    .routing_families
                    .capacity()
                    .saturating_mul(std::mem::size_of::<RoutingFamilyId>()),
            )
            .saturating_add(self.model.resident_bytes())
            .saturating_add(self.origin.heap_bytes())
    }

    pub(super) fn encode_canonical(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.id.0);
        output.push(match self.status {
            SpecialistStatus::Active => 1,
            SpecialistStatus::Retired => 2,
        });
        self.origin.encode_canonical(output);
        self.mandate.encode_canonical(output);
        self.model.encode_canonical(output);
    }

    fn decode_canonical(input: &mut Decoder<'_>) -> Result<Self, ()> {
        let expected_id = SpecialistRevisionId(input.read_digest()?);
        let status = match input.read_u8()? {
            1 => SpecialistStatus::Active,
            2 => SpecialistStatus::Retired,
            _ => return Err(()),
        };
        let origin = SpecialistOrigin::decode_canonical(input)?;
        let mandate = SpecialistMandate::decode_canonical(input)?;
        let model = CompactModel::decode_canonical(input)?;
        let mut revision = Self::new(mandate, model).map_err(|_| ())?;
        if revision.id != expected_id {
            return Err(());
        }
        revision.status = status;
        revision.origin = origin;
        Ok(revision)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EcologyEdit {
    Spawn(SpecialistRevision),
    Retire(SpecialistRevisionId),
    Split {
        parent: SpecialistRevisionId,
        children: Vec<SpecialistRevision>,
    },
    Merge {
        parents: Vec<SpecialistRevisionId>,
        merged: SpecialistRevision,
    },
    Distill {
        teachers: Vec<SpecialistRevisionId>,
        student: SpecialistRevision,
    },
}

#[cfg(any(test, feature = "internal-experiments"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SpecialistManifest {
    id: SpecialistRevisionId,
    status: SpecialistStatus,
    lifecycle: SpecialistLifecycle,
}

#[cfg(any(test, feature = "internal-experiments"))]
impl SpecialistManifest {
    pub(crate) const fn id(self) -> SpecialistRevisionId {
        self.id
    }

    pub(crate) const fn active(self) -> bool {
        matches!(self.status, SpecialistStatus::Active)
    }

    #[cfg(test)]
    pub(crate) const fn lifecycle(self) -> SpecialistLifecycle {
        self.lifecycle
    }
}

#[cfg(any(test, feature = "internal-experiments"))]
#[derive(Clone, Copy)]
pub(crate) struct ModelEcologyManifest<'a> {
    ecology: &'a ModelEcology,
}

#[cfg(any(test, feature = "internal-experiments"))]
impl<'a> ModelEcologyManifest<'a> {
    pub(crate) fn specialists(self) -> impl Iterator<Item = SpecialistManifest> + 'a {
        self.ecology
            .specialists
            .iter()
            .map(|specialist| SpecialistManifest {
                id: specialist.id,
                status: specialist.status,
                lifecycle: specialist.origin.lifecycle(),
            })
    }

    pub(crate) fn active_ids(self) -> impl Iterator<Item = SpecialistRevisionId> + 'a {
        self.specialists()
            .filter(|specialist| specialist.active())
            .map(SpecialistManifest::id)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct ModelEcology {
    generation: u64,
    specialists: Vec<SpecialistRevision>,
    routing: RoutingIndex,
}

impl ModelEcology {
    pub(super) const fn is_empty(&self) -> bool {
        self.specialists.is_empty()
    }

    #[cfg(any(test, feature = "internal-experiments"))]
    pub(super) const fn manifest(&self) -> ModelEcologyManifest<'_> {
        ModelEcologyManifest { ecology: self }
    }

    pub(super) fn active(&self) -> impl Iterator<Item = &SpecialistRevision> {
        self.specialists
            .iter()
            .filter(|specialist| specialist.active())
    }

    pub(super) fn routed(
        &self,
        opportunity: OpportunitySpec,
        feature_count: usize,
    ) -> impl ExactSizeIterator<Item = &SpecialistRevision> {
        self.routing
            .route(opportunity, feature_count)
            .map(|index| &self.specialists[index])
    }

    pub(super) const fn maximum_route_fanout(&self) -> usize {
        self.routing.maximum_fanout()
    }

    pub(super) fn canonical_root(&self) -> [u8; 32] {
        let mut encoded = Vec::new();
        self.encode_canonical(&mut encoded);
        let mut digest = Sha256::new();
        digest.update(b"reflex-model-ecology-root-v1\0");
        digest.update(encoded);
        digest.finalize().into()
    }

    #[cfg(test)]
    pub(super) const fn retained_item_count(&self) -> usize {
        self.specialists.len()
    }

    pub(super) fn has_active_role(&self, role: RoleId) -> bool {
        self.active().any(|specialist| specialist.role() == role)
    }

    pub(super) fn active_role(&self, role: RoleId) -> Option<&SpecialistRevision> {
        self.active().find(|specialist| specialist.role() == role)
    }

    pub(super) fn fits_limits(&self, limits: IntelligenceLimits) -> bool {
        self.specialists.len() <= limits.maximum_specialists
            && self.routing.maximum_fanout() <= limits.maximum_specialists_per_route
            && self.heap_bytes() <= limits.maximum_model_bytes
    }

    pub(super) fn heap_bytes(&self) -> usize {
        self.specialists
            .capacity()
            .saturating_sub(self.specialists.len())
            .saturating_mul(std::mem::size_of::<SpecialistRevision>())
            .saturating_add(self.specialists.iter().fold(0_usize, |bytes, specialist| {
                bytes.saturating_add(specialist.resident_bytes())
            }))
            .saturating_add(self.routing.heap_bytes())
    }

    pub(super) fn staging_clone_bytes(&self) -> usize {
        self.heap_bytes().saturating_add(self.routing.heap_bytes())
    }

    pub(super) fn apply(
        &mut self,
        edit: &EcologyEdit,
        limits: IntelligenceLimits,
    ) -> Result<(), IntelligenceError> {
        match edit {
            EcologyEdit::Spawn(specialist) => {
                self.push(specialist.clone(), SpecialistOrigin::Spawn, limits)?;
            }
            EcologyEdit::Retire(id) => {
                self.retire(&[*id])?;
            }
            EcologyEdit::Split { parent, children } => {
                if children.len() < 2 {
                    return Err(IntelligenceError::InvalidMandate);
                }
                self.retire(&[*parent])?;
                for child in children {
                    self.push(child.clone(), SpecialistOrigin::Split(*parent), limits)?;
                }
            }
            EcologyEdit::Merge { parents, merged } => {
                validate_parents(parents)?;
                self.retire(parents)?;
                self.push(
                    merged.clone(),
                    SpecialistOrigin::Merge(parents.clone()),
                    limits,
                )?;
            }
            EcologyEdit::Distill { teachers, student } => {
                validate_parents(teachers)?;
                self.retire(teachers)?;
                self.push(
                    student.clone(),
                    SpecialistOrigin::Distill(teachers.clone()),
                    limits,
                )?;
            }
        }
        self.routing = RoutingIndex::build(&self.specialists, limits)?;
        if !self.fits_limits(limits) {
            return Err(IntelligenceError::CapacityExceeded);
        }
        self.generation = self.generation.saturating_add(1);
        Ok(())
    }

    pub(super) fn encode_edits(edits: &[EcologyEdit], output: &mut Vec<u8>) {
        output.extend_from_slice(&(edits.len() as u64).to_le_bytes());
        for edit in edits {
            match edit {
                EcologyEdit::Spawn(specialist) => {
                    output.push(1);
                    specialist.encode_canonical(output);
                }
                EcologyEdit::Retire(id) => {
                    output.push(2);
                    output.extend_from_slice(&id.0);
                }
                EcologyEdit::Split { parent, children } => {
                    output.push(3);
                    output.extend_from_slice(&parent.0);
                    output.extend_from_slice(&(children.len() as u64).to_le_bytes());
                    for child in children {
                        child.encode_canonical(output);
                    }
                }
                EcologyEdit::Merge { parents, merged } => {
                    output.push(4);
                    encode_specialist_ids(parents, output);
                    merged.encode_canonical(output);
                }
                EcologyEdit::Distill { teachers, student } => {
                    output.push(5);
                    encode_specialist_ids(teachers, output);
                    student.encode_canonical(output);
                }
            }
        }
    }

    pub(super) fn decode_edits(
        input: &mut Decoder<'_>,
        limits: IntelligenceLimits,
    ) -> Result<Vec<EcologyEdit>, ()> {
        let maximum_edits = limits.maximum_specialists.saturating_mul(4);
        let count = read_optional_count(input, maximum_edits, 1)?;
        let mut edits = Vec::with_capacity(count);
        for _ in 0..count {
            edits.push(match input.read_u8()? {
                1 => EcologyEdit::Spawn(SpecialistRevision::decode_canonical(input)?),
                2 => EcologyEdit::Retire(SpecialistRevisionId(input.read_digest()?)),
                3 => {
                    let parent = SpecialistRevisionId(input.read_digest()?);
                    let child_count = read_count(input, limits.maximum_specialists, 46)?;
                    let mut children = Vec::with_capacity(child_count);
                    for _ in 0..child_count {
                        children.push(SpecialistRevision::decode_canonical(input)?);
                    }
                    EcologyEdit::Split { parent, children }
                }
                4 => EcologyEdit::Merge {
                    parents: decode_specialist_ids(input, limits.maximum_specialists)?,
                    merged: SpecialistRevision::decode_canonical(input)?,
                },
                5 => EcologyEdit::Distill {
                    teachers: decode_specialist_ids(input, limits.maximum_specialists)?,
                    student: SpecialistRevision::decode_canonical(input)?,
                },
                _ => return Err(()),
            });
        }
        Ok(edits)
    }

    fn retire(&mut self, ids: &[SpecialistRevisionId]) -> Result<(), IntelligenceError> {
        for id in ids {
            if !self
                .specialists
                .iter()
                .any(|specialist| specialist.id == *id && specialist.active())
            {
                return Err(IntelligenceError::MissingSpecialist);
            }
        }
        for specialist in &mut self.specialists {
            if ids.contains(&specialist.id) {
                specialist.status = SpecialistStatus::Retired;
            }
        }
        Ok(())
    }

    fn push(
        &mut self,
        mut specialist: SpecialistRevision,
        origin: SpecialistOrigin,
        limits: IntelligenceLimits,
    ) -> Result<(), IntelligenceError> {
        if self.specialists.len() == limits.maximum_specialists {
            return Err(IntelligenceError::CapacityExceeded);
        }
        if self
            .specialists
            .iter()
            .any(|existing| existing.id == specialist.id)
        {
            return Err(IntelligenceError::DuplicateSpecialist);
        }
        let proposed_bytes = self
            .specialists
            .iter()
            .fold(specialist.resident_bytes(), |bytes, existing| {
                bytes.saturating_add(existing.resident_bytes())
            });
        if proposed_bytes > limits.maximum_model_bytes {
            return Err(IntelligenceError::CapacityExceeded);
        }
        specialist.origin = origin;
        specialist.status = SpecialistStatus::Active;
        self.specialists.push(specialist);
        Ok(())
    }

    pub(super) fn encode_canonical(&self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.generation.to_le_bytes());
        output.extend_from_slice(&(self.specialists.len() as u64).to_le_bytes());
        for specialist in &self.specialists {
            specialist.encode_canonical(output);
        }
    }

    pub(super) fn decode_canonical(
        input: &mut Decoder<'_>,
        limits: IntelligenceLimits,
    ) -> Result<Self, ()> {
        let generation = input.read_u64()?;
        let count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
        if count > limits.maximum_specialists || count > input.remaining().saturating_div(46) {
            return Err(());
        }
        let mut specialists = Vec::with_capacity(count);
        let mut ids = BTreeSet::new();
        for _ in 0..count {
            let specialist = SpecialistRevision::decode_canonical(input)?;
            if specialist.resident_bytes() > limits.maximum_model_bytes
                || !ids.insert(specialist.id)
            {
                return Err(());
            }
            specialists.push(specialist);
        }
        if generation == 0 && !specialists.is_empty() {
            return Err(());
        }
        for (index, specialist) in specialists.iter().enumerate() {
            let prior = &specialists[..index];
            let valid_parent =
                |id: &SpecialistRevisionId| prior.iter().any(|candidate| candidate.id == *id);
            let valid = match &specialist.origin {
                SpecialistOrigin::Spawn => true,
                SpecialistOrigin::Split(parent) => valid_parent(parent),
                SpecialistOrigin::Merge(parents) | SpecialistOrigin::Distill(parents) => {
                    parents.iter().all(valid_parent)
                }
            };
            if !valid {
                return Err(());
            }
        }
        let mut ecology = Self {
            generation,
            specialists,
            routing: RoutingIndex::default(),
        };
        ecology.routing = RoutingIndex::build(&ecology.specialists, limits).map_err(|_| ())?;
        if !ecology.fits_limits(limits) {
            return Err(());
        }
        Ok(ecology)
    }
}

fn encode_specialist_ids(ids: &[SpecialistRevisionId], output: &mut Vec<u8>) {
    output.extend_from_slice(&(ids.len() as u64).to_le_bytes());
    for id in ids {
        output.extend_from_slice(&id.0);
    }
}

fn decode_specialist_ids(
    input: &mut Decoder<'_>,
    limit: usize,
) -> Result<Vec<SpecialistRevisionId>, ()> {
    let count = read_count(input, limit, 32)?;
    let mut ids = Vec::with_capacity(count);
    for _ in 0..count {
        ids.push(SpecialistRevisionId(input.read_digest()?));
    }
    Ok(ids)
}

fn validate_parents(parents: &[SpecialistRevisionId]) -> Result<(), IntelligenceError> {
    if parents.len() < 2 || parents.iter().copied().collect::<BTreeSet<_>>().len() != parents.len()
    {
        return Err(IntelligenceError::InvalidMandate);
    }
    Ok(())
}

fn unique_nonempty<T: Copy + Ord>(values: &[T]) -> bool {
    !values.is_empty() && values.iter().copied().collect::<BTreeSet<_>>().len() == values.len()
}

fn read_count(input: &mut Decoder<'_>, limit: usize, minimum_bytes: usize) -> Result<usize, ()> {
    let count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
    if count == 0
        || count > limit
        || count > input.remaining().checked_div(minimum_bytes).unwrap_or(0)
    {
        return Err(());
    }
    Ok(count)
}

fn read_optional_count(
    input: &mut Decoder<'_>,
    limit: usize,
    minimum_bytes: usize,
) -> Result<usize, ()> {
    let count = usize::try_from(input.read_u64()?).map_err(|_| ())?;
    if count > limit || count > input.remaining().checked_div(minimum_bytes).unwrap_or(0) {
        return Err(());
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mandate_decoder_rejects_empty_and_unknown_opportunity_filters() {
        let mandate = SpecialistMandate::for_opportunities(
            RoleId::new("decode-adversary").unwrap(),
            [ForecastAxis::ImmediateImprovement],
            [OpportunityKind::Candidate],
            [FeatureSchemaId::new([61; 32])],
        )
        .unwrap();
        let mut encoded = Vec::new();
        mandate.encode_canonical(&mut encoded);
        assert_eq!(
            SpecialistMandate::decode_canonical(&mut Decoder::new(&encoded)).unwrap(),
            mandate
        );

        let mut empty_kinds = encoded.clone();
        empty_kinds[32..40].copy_from_slice(&0_u64.to_le_bytes());
        assert!(SpecialistMandate::decode_canonical(&mut Decoder::new(&empty_kinds)).is_err());

        let mut unknown_kind = encoded.clone();
        unknown_kind[40] = u8::MAX;
        assert!(SpecialistMandate::decode_canonical(&mut Decoder::new(&unknown_kind)).is_err());

        let mut empty_schemas = encoded;
        empty_schemas[41..49].copy_from_slice(&0_u64.to_le_bytes());
        assert!(SpecialistMandate::decode_canonical(&mut Decoder::new(&empty_schemas)).is_err());
    }
}
