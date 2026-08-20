use crate::{NodeIndex, NodeStatus};
use reflex_types::{Digest, StateId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Configuration contributing to compatibility identity (§11.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheConfig {
    pub shard_count: usize,
    pub enable_dominance: bool,
    pub enable_reflex: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            shard_count: 32,
            enable_dominance: true,
            enable_reflex: true,
        }
    }
}

impl CacheConfig {
    pub fn compatibility_digest(&self) -> Digest {
        let mut buf = Vec::with_capacity(16);
        buf.extend_from_slice(&(self.shard_count as u64).to_le_bytes());
        buf.push(u8::from(self.enable_dominance));
        buf.push(u8::from(self.enable_reflex));
        Digest::hash_blake3(&buf)
    }
}

/// Compact visit record for transposition lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisitRecord {
    pub node: NodeIndex,
    pub best_remaining_budget: u32,
    pub visit_budget: u32,
    pub status: NodeStatus,
}

/// Result of a transposition lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranspositionHit {
    /// Reuse existing node; higher-budget visit is not dominated.
    Reuse(NodeIndex),
    /// Lower-budget failure must not dominate this visit.
    Blocked,
}

/// Sharded state table with dominance semantics (§11.5).
pub struct TranspositionTable {
    shards: Vec<HashMap<StateId, VisitRecord>>,
    config: CacheConfig,
}

impl TranspositionTable {
    pub fn new(config: CacheConfig) -> Self {
        let shard_count = config.shard_count.max(1);
        Self {
            shards: (0..shard_count).map(|_| HashMap::new()).collect(),
            config,
        }
    }

    pub fn config(&self) -> &CacheConfig {
        &self.config
    }

    fn shard_index(&self, state_id: StateId) -> usize {
        let hash = u64::from_le_bytes(state_id.digest().bytes[0..8].try_into().unwrap());
        (hash as usize) % self.shards.len()
    }

    pub fn lookup(&self, state_id: StateId, remaining_budget: u32) -> Option<TranspositionHit> {
        let shard = &self.shards[self.shard_index(state_id)];
        let record = shard.get(&state_id)?;
        if self.config.enable_dominance
            && record.status == NodeStatus::Failed
            && record.best_remaining_budget < remaining_budget
        {
            return Some(TranspositionHit::Blocked);
        }
        Some(TranspositionHit::Reuse(record.node))
    }

    pub fn insert(&mut self, state_id: StateId, record: VisitRecord) {
        let idx = self.shard_index(state_id);
        let shard = &mut self.shards[idx];
        if let Some(existing) = shard.get(&state_id).copied()
            && existing.node == record.node
        {
            shard.insert(
                state_id,
                VisitRecord {
                    node: record.node,
                    best_remaining_budget: existing
                        .best_remaining_budget
                        .max(record.best_remaining_budget),
                    visit_budget: existing.visit_budget.min(record.visit_budget),
                    status: if status_strength(record.status) >= status_strength(existing.status) {
                        record.status
                    } else {
                        existing.status
                    },
                },
            );
            return;
        }
        match shard.get(&state_id) {
            // More remaining budget is the stronger visit. At equal budget,
            // retain a terminal semantic result over an in-progress marker.
            Some(existing) if existing.best_remaining_budget > record.best_remaining_budget => {}
            Some(existing)
                if existing.best_remaining_budget == record.best_remaining_budget
                    && status_strength(existing.status) >= status_strength(record.status) => {}
            _ => {
                shard.insert(state_id, record);
            }
        }
    }

    pub fn clear(&mut self) {
        for shard in &mut self.shards {
            shard.clear();
        }
    }

    pub fn len(&self) -> usize {
        self.shards.iter().map(|s| s.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn compatibility_digest(&self) -> Digest {
        self.config.compatibility_digest()
    }
}

fn status_strength(status: NodeStatus) -> u8 {
    match status {
        NodeStatus::Closed | NodeStatus::ObligationComplete | NodeStatus::Failed => 2,
        NodeStatus::Open | NodeStatus::ObligationPending => 1,
        // Censored is evidence about a run, never a semantic cache result.
        NodeStatus::Censored => 0,
    }
}

impl Default for TranspositionTable {
    fn default() -> Self {
        Self::new(CacheConfig::default())
    }
}

/// Cached reflex/closure result, separate from policy-dependent frontier (§11.5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CachedReflexResult {
    Closed { receipt: Digest },
    Failed { certificate: Option<Digest> },
}

pub struct ReflexCache {
    entries: HashMap<(StateId, Digest), CachedReflexResult>,
    overlay_generation: u64,
}

impl ReflexCache {
    pub fn new(overlay_generation: u64) -> Self {
        Self {
            entries: HashMap::new(),
            overlay_generation,
        }
    }

    pub fn overlay_generation(&self) -> u64 {
        self.overlay_generation
    }

    pub fn get(&self, state_id: StateId, input_digest: Digest) -> Option<&CachedReflexResult> {
        self.entries
            .get(&(state_id, input_digest))
            .filter(|result| result.has_evidence())
    }

    pub fn insert(&mut self, state_id: StateId, input_digest: Digest, result: CachedReflexResult) {
        if result.has_evidence() {
            self.entries.insert((state_id, input_digest), result);
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl CachedReflexResult {
    fn has_evidence(&self) -> bool {
        match self {
            Self::Closed { receipt } => *receipt != Digest::ZERO,
            Self::Failed {
                certificate: Some(certificate),
            } => *certificate != Digest::ZERO,
            Self::Failed { certificate: None } => false,
        }
    }
}

impl Default for ReflexCache {
    fn default() -> Self {
        Self::new(0)
    }
}

/// Combined search caches.
pub struct SearchCaches {
    pub transposition: TranspositionTable,
    pub reflex: ReflexCache,
}

impl SearchCaches {
    pub fn new(config: CacheConfig, overlay_generation: u64) -> Self {
        Self {
            transposition: TranspositionTable::new(config),
            reflex: ReflexCache::new(overlay_generation),
        }
    }

    pub fn compatibility_digest(&self) -> Digest {
        let mut buf = Vec::new();
        buf.extend_from_slice(self.transposition.compatibility_digest().bytes.as_ref());
        buf.extend_from_slice(&self.reflex.overlay_generation().to_le_bytes());
        Digest::hash_blake3(&buf)
    }
}

impl Default for SearchCaches {
    fn default() -> Self {
        Self::new(CacheConfig::default(), 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dominance_blocks_lower_budget_failure() {
        let mut table = TranspositionTable::default();
        let state = StateId::from_digest(Digest::hash_blake3(b"s1"));
        table.insert(
            state,
            VisitRecord {
                node: NodeIndex(1),
                best_remaining_budget: 5,
                visit_budget: 3,
                status: NodeStatus::Failed,
            },
        );
        assert_eq!(table.lookup(state, 10), Some(TranspositionHit::Blocked));
        assert_eq!(
            table.lookup(state, 3),
            Some(TranspositionHit::Reuse(NodeIndex(1)))
        );
    }

    #[test]
    fn test_stronger_visit_replaces_weaker_visit() {
        let mut table = TranspositionTable::default();
        let state = StateId::from_digest(Digest::hash_blake3(b"stronger"));
        table.insert(
            state,
            VisitRecord {
                node: NodeIndex(1),
                best_remaining_budget: 2,
                visit_budget: 8,
                status: NodeStatus::Failed,
            },
        );
        table.insert(
            state,
            VisitRecord {
                node: NodeIndex(2),
                best_remaining_budget: 8,
                visit_budget: 2,
                status: NodeStatus::Open,
            },
        );
        assert_eq!(
            table.lookup(state, 8),
            Some(TranspositionHit::Reuse(NodeIndex(2)))
        );
    }

    #[test]
    fn test_cache_config_digest() {
        let a = CacheConfig::default();
        let b = CacheConfig {
            shard_count: 64,
            ..Default::default()
        };
        assert_ne!(a.compatibility_digest(), b.compatibility_digest());
    }

    #[test]
    fn test_reflex_cache_isolation() {
        let mut cache = ReflexCache::new(1);
        let state = StateId::from_digest(Digest::hash_blake3(b"s"));
        let input = Digest::hash_blake3(b"input");
        cache.insert(
            state,
            input,
            CachedReflexResult::Closed {
                receipt: Digest::hash_blake3(b"receipt"),
            },
        );
        assert!(cache.get(state, input).is_some());
        assert!(cache.get(state, Digest::hash_blake3(b"other")).is_none());
    }

    #[test]
    fn test_reflex_cache_rejects_evidence_free_results() {
        let mut cache = ReflexCache::new(1);
        let state = StateId::from_digest(Digest::hash_blake3(b"s"));
        let input = Digest::hash_blake3(b"input");
        cache.insert(
            state,
            input,
            CachedReflexResult::Closed {
                receipt: Digest::ZERO,
            },
        );
        cache.insert(
            state,
            input,
            CachedReflexResult::Failed { certificate: None },
        );
        assert!(cache.is_empty());
    }
}
