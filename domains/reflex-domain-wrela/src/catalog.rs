//! Immutable Wrela optimization catalog — editions, lockfile, telemetry.

use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter, content_id};
use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub entry_id: String,
    pub source_pattern: String,
    pub replacement_pattern: String,
    pub preconditions: Vec<String>,
    pub certificate_digest: Digest,
    pub target_cost_delta: i64,
    pub workload_hits: u64,
    pub discovery_provenance: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEdition {
    pub edition_id: String,
    pub wrela_commit: String,
    pub entries: Vec<CatalogEntry>,
    pub conformance_digest: Digest,
    pub cost_corpus_digest: Digest,
}

impl CanonicalEncode for CatalogEdition {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.edition_id)?;
        out.write_str(&self.wrela_commit)?;
        out.write_u32(self.entries.len() as u32)?;
        for e in &self.entries {
            out.write_str(&e.entry_id)?;
            out.write_str(&e.source_pattern)?;
            out.write_str(&e.replacement_pattern)?;
            out.write_u32(e.preconditions.len() as u32)?;
            for precondition in &e.preconditions {
                out.write_str(precondition)?;
            }
            out.write_digest(&e.certificate_digest)?;
            out.write_i64(e.target_cost_delta)?;
            out.write_u64(e.workload_hits)?;
            out.write_str(&e.discovery_provenance)?;
        }
        out.write_digest(&self.conformance_digest)?;
        out.write_digest(&self.cost_corpus_digest)?;
        Ok(())
    }
}

impl CatalogEdition {
    pub fn digest(&self) -> Result<Digest, CanonicalError> {
        content_id(b"wrela.catalog.edition.v1", self)
    }

    #[cfg(test)]
    fn synthetic_test_fixture() -> Self {
        Self {
            edition_id: "synthetic-test-catalog-v1".to_string(),
            wrela_commit: "synthetic-test-commit".to_string(),
            entries: vec![CatalogEntry {
                entry_id: "bernstein-default".to_string(),
                source_pattern: "interval_enclosure".to_string(),
                replacement_pattern: "bernstein_polynomial".to_string(),
                preconditions: vec!["bounded_domain".to_string()],
                certificate_digest: Digest::hash_blake3(b"synthetic-test-bernstein-cert"),
                target_cost_delta: -45_000,
                workload_hits: 128,
                discovery_provenance: "synthetic-test-fixture".to_string(),
            }],
            conformance_digest: Digest::hash_blake3(b"conformance-v1"),
            cost_corpus_digest: Digest::hash_blake3(b"cost-corpus-v1"),
        }
    }

    /// Entries are immutable once published — mutation returns a new edition id.
    pub fn with_entry(&self, entry: CatalogEntry) -> Self {
        let mut next = self.clone();
        next.entries.push(entry);
        next.edition_id = format!("{}+{}", self.edition_id, next.entries.len());
        next
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogLockfile {
    pub pinned_edition_digest: Digest,
    pub wrela_commit: String,
    pub rollback_edition_digest: Option<Digest>,
}

impl CatalogLockfile {
    pub fn pin(edition: &CatalogEdition) -> Result<Self, CanonicalError> {
        Ok(Self {
            pinned_edition_digest: edition.digest()?,
            wrela_commit: edition.wrela_commit.clone(),
            rollback_edition_digest: None,
        })
    }

    pub fn rollback(&self, prior: Digest) -> Self {
        Self {
            pinned_edition_digest: prior,
            wrela_commit: self.wrela_commit.clone(),
            rollback_edition_digest: Some(self.pinned_edition_digest),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ApplicationTelemetry {
    pub entries_fired: HashMap<String, u64>,
    /// Signed: regressions remain negative instead of wrapping or saturating to zero.
    pub measured_savings_cycles: i64,
}

impl ApplicationTelemetry {
    /// Telemetry records only — never changes application decisions.
    pub fn record(&mut self, entry_id: &str, savings: i64) -> Result<(), &'static str> {
        let count = self.entries_fired.entry(entry_id.to_string()).or_insert(0);
        *count = count.checked_add(1).ok_or("telemetry count overflow")?;
        self.measured_savings_cycles = self
            .measured_savings_cycles
            .checked_add(savings)
            .ok_or("telemetry savings overflow")?;
        Ok(())
    }
}

pub fn apply_edition_entry<'a>(
    edition: &'a CatalogEdition,
    entry_id: &str,
) -> Option<&'a CatalogEntry> {
    edition.entries.iter().find(|e| e.entry_id == entry_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_edition_deterministic() {
        let e1 = CatalogEdition::synthetic_test_fixture();
        let e2 = CatalogEdition::synthetic_test_fixture();
        assert_eq!(e1.digest().unwrap(), e2.digest().unwrap());
    }

    #[test]
    fn test_lockfile_rollback() {
        let e = CatalogEdition::synthetic_test_fixture();
        let lock = CatalogLockfile::pin(&e).unwrap();
        let prior = Digest::hash_blake3(b"prior-edition");
        let rolled = lock.rollback(prior);
        assert_eq!(rolled.pinned_edition_digest, prior);
    }

    #[test]
    fn test_telemetry_does_not_mutate_edition() {
        let edition = CatalogEdition::synthetic_test_fixture();
        let digest_before = edition.digest().unwrap();
        let mut telem = ApplicationTelemetry::default();
        telem.record("bernstein-default", 1000).unwrap();
        assert_eq!(edition.digest().unwrap(), digest_before);
        assert_eq!(telem.measured_savings_cycles, 1000);
    }

    #[test]
    fn test_negative_savings_remain_negative() {
        let mut telemetry = ApplicationTelemetry::default();
        telemetry.record("regression", -17).unwrap();
        assert_eq!(telemetry.measured_savings_cycles, -17);
    }
}
