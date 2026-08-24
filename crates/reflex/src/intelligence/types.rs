#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[expect(
    clippy::struct_field_names,
    reason = "the repeated maximum prefix makes every persisted Resource Envelope bound explicit at call sites"
)]
pub(crate) struct IntelligenceLimits {
    pub(crate) maximum_opportunities: usize,
    pub(crate) maximum_investments: usize,
    pub(crate) maximum_bids: usize,
    pub(crate) maximum_forecast_cells: usize,
    pub(crate) maximum_specialists: usize,
    pub(crate) maximum_model_bytes: usize,
    pub(crate) maximum_specialists_per_route: usize,
}

impl IntelligenceLimits {
    pub(crate) const MAXIMUM_OPPORTUNITIES: usize = 4_096;
    pub(crate) const MAXIMUM_INVESTMENTS: usize = 4_096;
    pub(crate) const MAXIMUM_BIDS: usize = 4_194_304;
    pub(crate) const MAXIMUM_FORECAST_CELLS: usize = 37_748_736;
    pub(crate) const MAXIMUM_SPECIALISTS: usize = 1_024;
    pub(crate) const MAXIMUM_MODEL_BYTES: usize = 64 * 1024 * 1024;
    pub(crate) const MAXIMUM_SPECIALISTS_PER_ROUTE: usize = 1_024;
    pub(crate) const DEFAULT_SPECIALISTS_PER_ROUTE: usize = 8;

    pub(crate) fn new(
        maximum_opportunities: usize,
        maximum_investments: usize,
        maximum_bids: usize,
        maximum_forecast_cells: usize,
        maximum_specialists: usize,
        maximum_model_bytes: usize,
    ) -> Result<Self, IntelligenceError> {
        Self {
            maximum_opportunities,
            maximum_investments,
            maximum_bids,
            maximum_forecast_cells,
            maximum_specialists,
            maximum_model_bytes,
            maximum_specialists_per_route: maximum_specialists
                .min(Self::DEFAULT_SPECIALISTS_PER_ROUTE),
        }
        .validated()
    }

    pub(crate) fn with_maximum_specialists_per_route(
        mut self,
        maximum_specialists_per_route: usize,
    ) -> Result<Self, IntelligenceError> {
        self.maximum_specialists_per_route = maximum_specialists_per_route;
        self.validated()
    }

    pub(super) fn validated(self) -> Result<Self, IntelligenceError> {
        let values = [
            self.maximum_opportunities,
            self.maximum_investments,
            self.maximum_bids,
            self.maximum_forecast_cells,
            self.maximum_specialists,
            self.maximum_model_bytes,
            self.maximum_specialists_per_route,
        ];
        if values.contains(&0)
            || self.maximum_opportunities > Self::MAXIMUM_OPPORTUNITIES
            || self.maximum_investments > Self::MAXIMUM_INVESTMENTS
            || self.maximum_bids > Self::MAXIMUM_BIDS
            || self.maximum_forecast_cells > Self::MAXIMUM_FORECAST_CELLS
            || self.maximum_specialists > Self::MAXIMUM_SPECIALISTS
            || self.maximum_model_bytes > Self::MAXIMUM_MODEL_BYTES
            || self.maximum_specialists_per_route > Self::MAXIMUM_SPECIALISTS_PER_ROUTE
            || self.maximum_specialists_per_route > self.maximum_specialists
            || self.maximum_bids
                > self
                    .maximum_investments
                    .checked_mul(self.maximum_specialists)
                    .ok_or(IntelligenceError::InvalidLimits)?
            || self.maximum_forecast_cells
                > self
                    .maximum_bids
                    .checked_mul(32)
                    .ok_or(IntelligenceError::InvalidLimits)?
        {
            return Err(IntelligenceError::InvalidLimits);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct ResourceVector {
    pub(crate) cpu_time_ns: u64,
    pub(crate) resident_bytes: u64,
    pub(crate) durable_bytes: u64,
    pub(crate) elapsed_time_ns: u64,
    pub(crate) verification_requests: u64,
}

impl ResourceVector {
    pub(crate) const fn new(
        cpu_time_ns: u64,
        resident_bytes: u64,
        durable_bytes: u64,
        elapsed_time_ns: u64,
        verification_requests: u64,
    ) -> Self {
        Self {
            cpu_time_ns,
            resident_bytes,
            durable_bytes,
            elapsed_time_ns,
            verification_requests,
        }
    }

    pub(crate) fn checked_add(self, other: Self) -> Option<Self> {
        Some(Self {
            cpu_time_ns: self.cpu_time_ns.checked_add(other.cpu_time_ns)?,
            resident_bytes: self.resident_bytes.checked_add(other.resident_bytes)?,
            durable_bytes: self.durable_bytes.checked_add(other.durable_bytes)?,
            elapsed_time_ns: self.elapsed_time_ns.checked_add(other.elapsed_time_ns)?,
            verification_requests: self
                .verification_requests
                .checked_add(other.verification_requests)?,
        })
    }

    pub(crate) const fn fits_within(self, allowance: Self) -> bool {
        self.cpu_time_ns <= allowance.cpu_time_ns
            && self.resident_bytes <= allowance.resident_bytes
            && self.durable_bytes <= allowance.durable_bytes
            && self.elapsed_time_ns <= allowance.elapsed_time_ns
            && self.verification_requests <= allowance.verification_requests
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AllocationSource {
    Bootstrap,
    Specialist(SpecialistRevisionId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IntelligenceError {
    InvalidLimits,
    CapacityExceeded,
    InvalidReference,
    InvalidFeature,
    InvalidModel,
    InvalidForecast,
    InvalidMandate,
    DuplicateSpecialist,
    MissingSpecialist,
    StaleTransition,
    IncompatibleRevision,
    CorruptState,
    ResourceOverflow,
    DuplicateExperience,
    InvalidSettlement,
    InvalidKnowledge,
    DuplicateKnowledge,
    InvalidPolicy,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct OpportunityId(pub(super) u32);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct InvestmentId(pub(super) u32);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct FeatureSchemaId(pub(super) [u8; 32]);

impl FeatureSchemaId {
    #[cfg(test)]
    pub(crate) const fn new(identity: [u8; 32]) -> Self {
        Self(identity)
    }

    pub(super) const fn built_in(tag: u8) -> Self {
        let mut identity = [0_u8; 32];
        identity[0] = 0x52;
        identity[1] = 0x46;
        identity[31] = tag;
        Self(identity)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RoutingFamilyId(pub(super) [u8; 32]);

impl RoutingFamilyId {
    pub(crate) const fn new(identity: [u8; 32]) -> Self {
        Self(identity)
    }

    pub(crate) const fn generic() -> Self {
        let mut identity = [0_u8; 32];
        identity[0] = 0x52;
        identity[1] = 0x54;
        identity[31] = 1;
        Self(identity)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SpecialistRevisionId(pub(super) [u8; 32]);

impl SpecialistRevisionId {
    #[cfg(test)]
    pub(crate) const fn new(identity: [u8; 32]) -> Self {
        Self(identity)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct RoleId(pub(super) [u8; 32]);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SubjectId([u8; 32]);

impl SubjectId {
    pub(crate) const fn new(identity: [u8; 32]) -> Self {
        Self(identity)
    }

    pub(crate) const fn identity(self) -> [u8; 32] {
        self.0
    }
}

impl RoleId {
    pub(super) const fn from_identity(identity: [u8; 32]) -> Self {
        Self(identity)
    }

    #[cfg(test)]
    pub(crate) fn new(symbol: &str) -> Result<Self, IntelligenceError> {
        use sha2::{Digest, Sha256};

        if symbol.is_empty() || symbol.len() > 256 {
            return Err(IntelligenceError::InvalidMandate);
        }
        let mut digest = Sha256::new();
        digest.update(b"reflex-specialist-role-v1\0");
        digest.update((symbol.len() as u64).to_le_bytes());
        digest.update(symbol.as_bytes());
        Ok(Self(digest.finalize().into()))
    }
}
