use reflex_types::{CompatibilityDigest, Digest, KnowledgeEditionId, KnowledgeRecordId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;
use tokio::sync::RwLock;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum KnowledgeError {
    #[error("knowledge edition not found: {0}")]
    EditionNotFound(KnowledgeEditionId),
    #[error("record not found: {0}")]
    RecordNotFound(KnowledgeRecordId),
    #[error("schema or compatibility mismatch")]
    CompatibilityMismatch,
    #[error("curation error: {0}")]
    Curation(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeClass {
    ExactMemory,
    TheoremFact,
    ProofMacro,
    RewriteRule,
    ResearchArtifact,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeRecord {
    pub id: KnowledgeRecordId,
    pub class: KnowledgeClass,
    pub statement: String,
    pub proof_or_cert_digest: Digest,
    pub structural_tags: Vec<String>,
    pub discovery_generation: u32,
    pub utility_score: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeEdition {
    pub id: KnowledgeEditionId,
    pub domain_id: String,
    pub compatibility: CompatibilityDigest,
    pub records: Vec<KnowledgeRecordId>,
    pub parent_edition: Option<KnowledgeEditionId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KnowledgeUseLevel {
    Retrieved,
    Attempted,
    TransitionSucceeded,
    PresentInFinalProof,
    NecessaryLeaveOneOut,
    ReducedWorkMatchedRerun,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnowledgeUseRecord {
    pub record_id: KnowledgeRecordId,
    pub level: KnowledgeUseLevel,
    pub actions_saved: i64,
    pub cpu_saved_ns: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StorageTier {
    Hot,
    Warm,
    Archive,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CurationPolicy {
    pub max_hot_records: usize,
    pub max_warm_records: usize,
    pub min_utility_threshold: f64,
}

impl Default for CurationPolicy {
    fn default() -> Self {
        Self {
            max_hot_records: 10_000,
            max_warm_records: 100_000,
            min_utility_threshold: 0.01,
        }
    }
}

pub struct KnowledgeBase {
    records: RwLock<HashMap<KnowledgeRecordId, KnowledgeRecord>>,
    editions: RwLock<HashMap<KnowledgeEditionId, KnowledgeEdition>>,
    record_tiers: RwLock<HashMap<KnowledgeRecordId, StorageTier>>,
    overlay_records: RwLock<Vec<KnowledgeRecordId>>,
}

impl KnowledgeBase {
    pub fn new() -> Self {
        Self {
            records: RwLock::new(HashMap::new()),
            editions: RwLock::new(HashMap::new()),
            record_tiers: RwLock::new(HashMap::new()),
            overlay_records: RwLock::new(Vec::new()),
        }
    }

    pub async fn add_record(&self, record: KnowledgeRecord) {
        let id = record.id;
        self.records.write().await.insert(id, record);
        self.record_tiers.write().await.insert(id, StorageTier::Hot);
        self.overlay_records.write().await.push(id);
    }

    pub async fn create_edition(
        &self,
        domain_id: &str,
        compatibility: CompatibilityDigest,
        parent: Option<KnowledgeEditionId>,
    ) -> KnowledgeEdition {
        let mut records: Vec<KnowledgeRecordId> =
            self.records.read().await.keys().copied().collect();
        records.sort();

        let mut edition_bytes = Vec::new();
        edition_bytes.extend_from_slice(domain_id.as_bytes());
        edition_bytes.extend_from_slice(&compatibility.digest().bytes);
        if let Some(p) = parent {
            edition_bytes.push(1);
            edition_bytes.extend_from_slice(&p.digest().bytes);
        } else {
            edition_bytes.push(0);
        }
        for r in &records {
            edition_bytes.extend_from_slice(&r.digest().bytes);
        }

        let edition_digest = Digest::hash_blake3(&edition_bytes);
        let id = KnowledgeEditionId::from_digest(edition_digest);
        let edition = KnowledgeEdition {
            id,
            domain_id: domain_id.to_string(),
            compatibility,
            records,
            parent_edition: parent,
        };
        self.editions.write().await.insert(id, edition.clone());
        edition
    }

    pub async fn retrieve_by_tag(&self, tag: &str, limit: usize) -> Vec<KnowledgeRecord> {
        let records = self.records.read().await;
        let mut matching: Vec<KnowledgeRecord> = records
            .values()
            .filter(|r| r.structural_tags.iter().any(|t| t == tag))
            .cloned()
            .collect();
        matching.sort_by(|a, b| {
            b.utility_score
                .partial_cmp(&a.utility_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        matching.truncate(limit);
        matching
    }

    pub async fn curate_library(&self, policy: &CurationPolicy) {
        let records = self.records.read().await;
        let mut sorted: Vec<_> = records.values().collect();
        sorted.sort_by(|a, b| {
            b.utility_score
                .partial_cmp(&a.utility_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut tiers = self.record_tiers.write().await;
        for (i, r) in sorted.iter().enumerate() {
            if i < policy.max_hot_records {
                tiers.insert(r.id, StorageTier::Hot);
            } else if i < policy.max_hot_records + policy.max_warm_records {
                tiers.insert(r.id, StorageTier::Warm);
            } else {
                tiers.insert(r.id, StorageTier::Archive);
            }
        }
    }
}

impl Default for KnowledgeBase {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_knowledge_edition_creation_and_retrieval() {
        let kb = KnowledgeBase::new();
        let rec = KnowledgeRecord {
            id: KnowledgeRecordId::from_digest(Digest::hash_blake3(b"rec1")),
            class: KnowledgeClass::TheoremFact,
            statement: "A and B -> B and A".to_string(),
            proof_or_cert_digest: Digest::ZERO,
            structural_tags: vec!["logic".to_string(), "and_comm".to_string()],
            discovery_generation: 1,
            utility_score: 10.0,
        };
        kb.add_record(rec).await;

        let results = kb.retrieve_by_tag("logic", 5).await;
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].statement, "A and B -> B and A");

        let ed = kb
            .create_edition(
                "bitvec",
                CompatibilityDigest::from_digest(Digest::ZERO),
                None,
            )
            .await;
        assert_eq!(ed.records.len(), 1);
    }
}
