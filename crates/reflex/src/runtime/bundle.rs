use std::path::Path;

use crate::domain::DomainDefinition;
use crate::resource::ResourceEnvelopeGuard;
use crate::session::{ImprovementRequest, SessionError};

use super::{RecoveredBundle, RestartBundleState, SessionSeal};

/// Semantic Domain Bundle encoding and recovery behind one restart-complete
/// interface. Canonical framing is delegated to the private `reflex-bundle`
/// format module; this module owns how Runtime state maps to that format.
pub(super) struct RestartBundleCodec<'a, D: DomainDefinition> {
    domain: &'a D,
    request: &'a ImprovementRequest<D>,
}

impl<'a, D: DomainDefinition> RestartBundleCodec<'a, D> {
    pub(super) const fn new(domain: &'a D, request: &'a ImprovementRequest<D>) -> Self {
        Self { domain, request }
    }

    pub(super) fn recover(
        &self,
        source: &Path,
        resources: &ResourceEnvelopeGuard,
        resident_before_bundle: u64,
    ) -> Result<RecoveredBundle<D>, SessionError<D::Error>> {
        super::decode_bundle(
            self.domain,
            self.request,
            source,
            resources,
            resident_before_bundle,
        )
    }

    pub(super) fn seal(
        &self,
        seed_cursor: &[u8],
        state: RestartBundleState<'_, D>,
        session_seal: SessionSeal,
    ) -> Result<Vec<u8>, SessionError<D::Error>> {
        super::encode_bundle(self.domain, self.request, seed_cursor, state, session_seal)
    }
}
