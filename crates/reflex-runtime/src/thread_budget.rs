//! Global thread-budget broker with named compute pools (P1.2).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tracing::{debug, info_span};

// Loom must instrument the registry's synchronization primitives in the deep
// lane. The semaphore remains Tokio-owned; the model covers the bookkeeping
// that decides whether a cell may publish terminal state.
#[cfg(reflex_loom)]
use loom::sync::atomic::{AtomicU64, Ordering};
#[cfg(reflex_loom)]
use loom::sync::{Arc as RegistryArc, RwLock as RegistryRwLock};
#[cfg(not(reflex_loom))]
use std::sync::atomic::{AtomicU64, Ordering};
#[cfg(not(reflex_loom))]
use std::sync::{Arc as RegistryArc, RwLock as RegistryRwLock};

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum BudgetError {
    #[error("invalid thread budget request: count {count} exceeds total {total}")]
    InvalidRequest { count: usize, total: usize },
    #[error("thread budget pool closed or exhausted")]
    Closed,
    #[error("permit leak detected at shutdown for owner {0}")]
    PermitLeak(String),
    #[error("nested pool acquire would exceed budget — deferred")]
    WouldDefer,
}

/// Named compute pools (§P1.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComputePool {
    Search,
    Verifier,
    Training,
    Compaction,
    Analytics,
}

impl ComputePool {
    pub const ALL: [ComputePool; 5] = [
        ComputePool::Search,
        ComputePool::Verifier,
        ComputePool::Training,
        ComputePool::Compaction,
        ComputePool::Analytics,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ComputePool::Search => "search",
            ComputePool::Verifier => "verifier",
            ComputePool::Training => "training",
            ComputePool::Compaction => "compaction",
            ComputePool::Analytics => "analytics",
        }
    }
}

#[derive(Default)]
pub struct PoolDiagnostics {
    pub queue_depth: AtomicU64,
    pub busy_time_ns: AtomicU64,
    pub blocked_duration_ns: AtomicU64,
    pub steals: AtomicU64,
}

impl PoolDiagnostics {
    pub fn snapshot(&self) -> PoolDiagnosticsSnapshot {
        PoolDiagnosticsSnapshot {
            queue_depth: self.queue_depth.load(Ordering::Relaxed),
            busy_time_ns: self.busy_time_ns.load(Ordering::Relaxed),
            blocked_duration_ns: self.blocked_duration_ns.load(Ordering::Relaxed),
            steals: self.steals.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PoolDiagnosticsSnapshot {
    pub queue_depth: u64,
    pub busy_time_ns: u64,
    pub blocked_duration_ns: u64,
    pub steals: u64,
}

#[derive(Default)]
pub struct PoolRegistry {
    active_permits: RegistryRwLock<HashMap<&'static str, usize>>,
    total_acquired: AtomicU64,
    diagnostics: RegistryRwLock<HashMap<&'static str, RegistryArc<PoolDiagnostics>>>,
}

impl PoolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn diag(&self, owner: &'static str) -> RegistryArc<PoolDiagnostics> {
        let mut map = self.diagnostics.write().unwrap();
        map.entry(owner)
            .or_insert_with(|| RegistryArc::new(PoolDiagnostics::default()))
            .clone()
    }

    pub fn on_acquire(&self, owner: &'static str, count: usize) {
        let mut map = self.active_permits.write().unwrap();
        *map.entry(owner).or_insert(0) += count;
        self.total_acquired
            .fetch_add(count as u64, Ordering::Relaxed);
        let diag = self.diag(owner);
        diag.queue_depth.fetch_add(count as u64, Ordering::Relaxed);
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
        let diag = self.diag(owner);
        diag.queue_depth.fetch_sub(count as u64, Ordering::Relaxed);
    }

    pub fn get_active(&self, owner: &'static str) -> usize {
        let map = self.active_permits.read().unwrap();
        map.get(owner).copied().unwrap_or(0)
    }

    pub fn diagnostics(&self, owner: &'static str) -> PoolDiagnosticsSnapshot {
        self.diag(owner).snapshot()
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
    busy_start: Instant,
}

impl Drop for ComputeLease {
    fn drop(&mut self) {
        let elapsed = self.busy_start.elapsed().as_nanos() as u64;
        self.registry
            .diag(self.owner)
            .busy_time_ns
            .fetch_add(elapsed, Ordering::Relaxed);
        self.registry.on_release(self.owner, self.count);
        debug!(
            pool = self.owner,
            count = self.count,
            "released compute lease"
        );
    }
}

#[derive(Clone)]
pub struct ThreadBudget {
    total: usize,
    permits: Arc<Semaphore>,
    registry: Arc<PoolRegistry>,
    allow_oversubscription: bool,
}

impl ThreadBudget {
    pub fn new(total: usize) -> Self {
        Self {
            total,
            permits: Arc::new(Semaphore::new(total)),
            registry: Arc::new(PoolRegistry::new()),
            allow_oversubscription: false,
        }
    }

    #[cfg(test)]
    pub fn with_oversubscription(total: usize) -> Self {
        Self {
            total,
            permits: Arc::new(Semaphore::new(total)),
            registry: Arc::new(PoolRegistry::new()),
            allow_oversubscription: true,
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

    pub fn active_workers(&self) -> usize {
        self.total - self.available()
    }

    pub async fn acquire(
        &self,
        owner: &'static str,
        count: usize,
    ) -> Result<ComputeLease, BudgetError> {
        self.acquire_pool(None, owner, count).await
    }

    /// Acquire from a named pool. Nested acquire waits (defers) rather than
    /// spawning hidden threads when the process budget is exhausted.
    pub async fn acquire_pool(
        &self,
        pool: Option<ComputePool>,
        owner: &'static str,
        count: usize,
    ) -> Result<ComputeLease, BudgetError> {
        let span = info_span!("thread_budget_acquire", pool = owner, count = count);
        let _guard = span.enter();

        if count == 0 || count > self.total {
            return Err(BudgetError::InvalidRequest {
                count,
                total: self.total,
            });
        }

        if !self.allow_oversubscription && count > self.available() {
            let wait_start = Instant::now();
            // Will block until permits free — nested training during search defers here.
            let permit = self
                .permits
                .clone()
                .acquire_many_owned(count as u32)
                .await
                .map_err(|_| BudgetError::Closed)?;
            let blocked = wait_start.elapsed();
            self.registry
                .diag(owner)
                .blocked_duration_ns
                .fetch_add(blocked.as_nanos() as u64, Ordering::Relaxed);
            self.registry.on_acquire(owner, count);
            if let Some(p) = pool {
                debug!(pool = p.name(), owner, count, "acquired after defer");
            }
            return Ok(ComputeLease {
                owner,
                count,
                _permit: permit,
                registry: self.registry.clone(),
                busy_start: Instant::now(),
            });
        }

        let permit = self
            .permits
            .clone()
            .acquire_many_owned(count as u32)
            .await
            .map_err(|_| BudgetError::Closed)?;
        self.registry.on_acquire(owner, count);
        if let Some(p) = pool {
            debug!(pool = p.name(), owner, count, "acquired");
        }
        Ok(ComputeLease {
            owner,
            count,
            _permit: permit,
            registry: self.registry.clone(),
            busy_start: Instant::now(),
        })
    }

    pub async fn acquire_named(
        &self,
        pool: ComputePool,
        count: usize,
    ) -> Result<ComputeLease, BudgetError> {
        self.acquire_pool(Some(pool), pool.name(), count).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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

    #[tokio::test]
    async fn test_four_vcpu_never_exceeds_budget_without_oversubscription() {
        let budget = ThreadBudget::new(4);
        let _l1 = budget.acquire_named(ComputePool::Search, 2).await.unwrap();
        let _l2 = budget
            .acquire_named(ComputePool::Verifier, 2)
            .await
            .unwrap();
        assert_eq!(budget.active_workers(), 4);
        assert_eq!(budget.available(), 0);
    }

    #[tokio::test]
    async fn test_nested_training_defers_during_search() {
        let budget = Arc::new(ThreadBudget::new(4));
        let search_lease = budget.acquire_named(ComputePool::Search, 4).await.unwrap();
        assert_eq!(budget.available(), 0);

        let budget2 = budget.clone();
        let training = tokio::spawn(async move {
            let start = Instant::now();
            let _lease = budget2
                .acquire_named(ComputePool::Training, 1)
                .await
                .unwrap();
            start.elapsed()
        });

        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(search_lease);
        let blocked = training.await.unwrap();
        assert!(
            blocked >= Duration::from_millis(15),
            "training should defer until search releases"
        );
    }

    #[tokio::test]
    async fn test_permit_leak_detected_at_shutdown() {
        let budget = ThreadBudget::new(2);
        let _lease = budget.acquire("search", 1).await.unwrap();
        std::mem::forget(_lease);
        assert!(budget.registry().check_clean_shutdown().is_err());
    }

    #[tokio::test]
    async fn test_pool_diagnostics_tracked() {
        let budget = ThreadBudget::new(2);
        let lease = budget
            .acquire_named(ComputePool::Analytics, 1)
            .await
            .unwrap();
        drop(lease);
        let diag = budget.registry().diagnostics("analytics");
        assert!(diag.busy_time_ns > 0 || diag.queue_depth == 0);
    }
}
