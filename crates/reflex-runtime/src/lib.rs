use reflex_types::{CellId, Digest, EpisodeId, KnowledgeEditionId, ModelCheckpointId};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum BudgetError {
    #[error("invalid thread budget request: count {count} exceeds total {total}")]
    InvalidRequest { count: usize, total: usize },
    #[error("thread budget pool closed or exhausted")]
    Closed,
    #[error("permit leak detected at shutdown for owner {0}")]
    PermitLeak(String),
}

#[derive(Default)]
pub struct PoolRegistry {
    active_permits: std::sync::RwLock<HashMap<&'static str, usize>>,
    total_acquired: AtomicU64,
}

impl PoolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_acquire(&self, owner: &'static str, count: usize) {
        let mut map = self.active_permits.write().unwrap();
        *map.entry(owner).or_insert(0) += count;
        self.total_acquired
            .fetch_add(count as u64, Ordering::Relaxed);
    }

    pub fn on_release(&self, owner: &'static str, count: usize) {
        let mut map = self.active_permits.write().unwrap();
        if let Some(entry) = map.get_mut(owner) {
            if *entry >= count {
                *entry -= count;
            } else {
                *entry = 0;
            }
        }
    }

    pub fn get_active(&self, owner: &'static str) -> usize {
        let map = self.active_permits.read().unwrap();
        map.get(owner).copied().unwrap_or(0)
    }

    pub fn check_clean_shutdown(&self) -> Result<(), BudgetError> {
        let map = self.active_permits.read().unwrap();
        for (owner, count) in map.iter() {
            if *count > 0 {
                return Err(BudgetError::PermitLeak(owner.to_string()));
            }
        }
        Ok(())
    }
}

pub struct ComputeLease {
    pub owner: &'static str,
    pub count: usize,
    _permit: OwnedSemaphorePermit,
    registry: Arc<PoolRegistry>,
}

impl Drop for ComputeLease {
    fn drop(&mut self) {
        self.registry.on_release(self.owner, self.count);
    }
}

#[derive(Clone)]
pub struct ThreadBudget {
    total: usize,
    permits: Arc<Semaphore>,
    registry: Arc<PoolRegistry>,
}

impl ThreadBudget {
    pub fn new(total: usize) -> Self {
        Self {
            total,
            permits: Arc::new(Semaphore::new(total)),
            registry: Arc::new(PoolRegistry::new()),
        }
    }

    pub fn total(&self) -> usize {
        self.total
    }

    pub fn available(&self) -> usize {
        self.permits.available_permits()
    }

    pub fn registry(&self) -> &Arc<PoolRegistry> {
        &self.registry
    }

    pub async fn acquire(
        &self,
        owner: &'static str,
        count: usize,
    ) -> Result<ComputeLease, BudgetError> {
        if count == 0 || count > self.total {
            return Err(BudgetError::InvalidRequest {
                count,
                total: self.total,
            });
        }
        let permit = self
            .permits
            .clone()
            .acquire_many_owned(count as u32)
            .await
            .map_err(|_| BudgetError::Closed)?;
        self.registry.on_acquire(owner, count);
        Ok(ComputeLease {
            owner,
            count,
            _permit: permit,
            registry: self.registry.clone(),
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProcessRusageSample {
    pub user_cpu_ns: u64,
    pub sys_cpu_ns: u64,
    pub rss_bytes: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
}

pub struct ProcessTreeAccountant {
    root_pid: u32,
    tracked_pids: std::sync::RwLock<Vec<u32>>,
    accumulated_user_ns: AtomicU64,
    accumulated_sys_ns: AtomicU64,
}

impl ProcessTreeAccountant {
    pub fn new(root_pid: u32) -> Self {
        Self {
            root_pid,
            tracked_pids: std::sync::RwLock::new(vec![root_pid]),
            accumulated_user_ns: AtomicU64::new(0),
            accumulated_sys_ns: AtomicU64::new(0),
        }
    }

    pub fn root_pid(&self) -> u32 {
        self.root_pid
    }

    pub fn track_child(&self, pid: u32) {
        let mut pids = self.tracked_pids.write().unwrap();
        if !pids.contains(&pid) {
            pids.push(pid);
        }
    }

    pub fn sample(&self) -> ProcessRusageSample {
        ProcessRusageSample {
            user_cpu_ns: self.accumulated_user_ns.load(Ordering::Relaxed),
            sys_cpu_ns: self.accumulated_sys_ns.load(Ordering::Relaxed),
            rss_bytes: 64 * 1024 * 1024,
            read_bytes: 0,
            write_bytes: 0,
        }
    }

    pub fn add_cpu(&self, user_ns: u64, sys_ns: u64) {
        self.accumulated_user_ns
            .fetch_add(user_ns, Ordering::Relaxed);
        self.accumulated_sys_ns.fetch_add(sys_ns, Ordering::Relaxed);
    }
}

pub struct CellContext {
    pub cell_id: CellId,
    pub manifest_digest: Digest,
    pub model_checkpoint: ModelCheckpointId,
    pub knowledge_edition: Option<KnowledgeEditionId>,
    pub thread_budget: ThreadBudget,
    pub accountant: Arc<ProcessTreeAccountant>,
    cancelled: AtomicBool,
}

impl CellContext {
    pub fn new(
        cell_id: CellId,
        manifest_digest: Digest,
        model_checkpoint: ModelCheckpointId,
        knowledge_edition: Option<KnowledgeEditionId>,
        thread_budget: ThreadBudget,
    ) -> Self {
        Self {
            cell_id,
            manifest_digest,
            model_checkpoint,
            knowledge_edition,
            thread_budget,
            accountant: Arc::new(ProcessTreeAccountant::new(std::process::id())),
            cancelled: AtomicBool::new(false),
        }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
}

pub struct EpisodeContext {
    pub episode_id: EpisodeId,
    pub cell: Arc<CellContext>,
}

impl EpisodeContext {
    pub fn new(episode_id: EpisodeId, cell: Arc<CellContext>) -> Self {
        Self { episode_id, cell }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_thread_budget_enforcement() {
        let budget = ThreadBudget::new(4);
        assert_eq!(budget.available(), 4);

        let lease1 = budget.acquire("search", 3).await.unwrap();
        assert_eq!(budget.available(), 1);
        assert_eq!(budget.registry().get_active("search"), 3);

        let lease2 = budget.acquire("io", 1).await.unwrap();
        assert_eq!(budget.available(), 0);

        drop(lease1);
        assert_eq!(budget.available(), 3);
        assert_eq!(budget.registry().get_active("search"), 0);

        drop(lease2);
        assert_eq!(budget.available(), 4);
        budget.registry().check_clean_shutdown().unwrap();
    }
}
