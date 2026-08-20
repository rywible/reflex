//! Domain capability declaration (serde for config/manifest interchange).

use reflex_types::{ActionSchemaId, Digest, FeatureSchemaId};
use serde::{Deserialize, Serialize};

/// Declared capabilities and schema fingerprints of a domain (§10.1).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DomainCapabilities {
    pub domain_id: String,
    pub domain_digest: Digest,
    pub action_schema: ActionSchemaId,
    pub feature_schema: FeatureSchemaId,
    pub feature_dimension: usize,
    pub max_candidates_per_state: u32,
    pub deterministic_generation: bool,
    pub supports_exact_cache: bool,
}
