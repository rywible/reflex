//! Bounded object pools (§12.1).
//!
//! [`BoundedPool`] implements the §12.1 caller-owned buffer contract with
//! semantic correctness: an item checked out by a caller holds a permit for
//! the whole checkout, so pool capacity can never be overcommitted — the
//! pool cannot hand out more items than its declared capacity no matter how
//! callers are scheduled. This differs from a naive `VecDeque` + lock pool,
//! which can hand out `capacity + concurrent_callers` items.
//!
//! `insert` adds an item together with its permit, so returned items are
//! exactly the ones inserted. Drop of the pool's last strong reference
//! blocks until every checked-out item is returned (semaphore drop
//! semantics), so shutdown never races with in-flight buffers.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use thiserror::Error;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum PoolError {
    #[error("pool is full: capacity {capacity}")]
    Full { capacity: usize },
    #[error("pool is closed or empty")]
    Closed,
    #[error("timeout acquiring from pool")]
    Timeout,
}

/// Accounting snapshot of one pool.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// Permits currently checked out (items in the hands of callers).
    pub checked_out: usize,
    /// Items currently idle in the pool.
    pub available: usize,
    /// Total acquisitions since creation.
    pub acquired_total: u64,
    /// Total releases since creation.
    pub released_total: u64,
    /// Highest number of items simultaneously checked out.
    pub high_water_items: usize,
    /// Total waiters across the pool's lifetime.
    pub wait_count: u64,
}

/// A caller-owned buffer checked out from a [`BoundedPool`].
///
/// Returning the buffer to the pool (or dropping it) releases its permit.
pub struct Pooled<T> {
    item: Option<T>,
    pool: Arc<PoolInner<T>>,
    permit: Option<OwnedSemaphorePermit>,
}

impl<T: std::fmt::Debug> std::fmt::Debug for Pooled<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pooled")
            .field("item", &self.item)
            .field("checked_out", &self.is_checked_out())
            .finish()
    }
}

impl<T> Pooled<T> {
    /// Direct access to the pooled item.
    pub fn item(&self) -> &T {
        self.item.as_ref().expect("pooled item present")
    }

    /// Mutable access to the pooled item.
    pub fn item_mut(&mut self) -> &mut T {
        self.item.as_mut().expect("pooled item present")
    }

    /// Whether the item is still in the hands of this checkout.
    pub fn is_checked_out(&self) -> bool {
        self.permit.is_some()
    }

    /// Returns the item to the pool, releasing its permit. No-op if the
    /// item was already returned.
    pub fn release(mut self) {
        if let Some(permit) = self.permit.take() {
            self.pool
                .release(self.item.take().expect("pooled item present"), permit);
        }
    }
}

impl<T> std::ops::Deref for Pooled<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.item.as_ref().expect("pooled item present")
    }
}

impl<T> std::ops::DerefMut for Pooled<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.item.as_mut().expect("pooled item present")
    }
}

impl<T> Drop for Pooled<T> {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            self.pool
                .release_dropped(self.item.take().expect("pooled item present"), permit);
        }
    }
}

struct PoolInner<T> {
    capacity: usize,
    slots: Mutex<VecDeque<T>>,
    permits: Arc<Semaphore>,
    checked_out: AtomicU64,
    acquired_total: AtomicU64,
    released_total: AtomicU64,
    high_water: AtomicU64,
    wait_count: AtomicU64,
    closed: AtomicU64,
}

impl<T> PoolInner<T> {
    fn release(&self, item: T, permit: OwnedSemaphorePermit) {
        self.released_total.fetch_add(1, Ordering::Relaxed);
        self.checked_out.fetch_sub(1, Ordering::Relaxed);
        if self.closed.load(Ordering::Acquire) == 0 {
            self.slots.lock().unwrap().push_back(item);
            drop(permit);
        } else {
            // Closed: the item is dropped and its permit is permanently
            // withdrawn (forget), so the pool's capacity shrinks to zero
            // instead of handing the slot back out.
            drop(item);
            drop(permit);
            self.permits.forget_permits(1);
        }
    }

    fn release_dropped(&self, item: T, permit: OwnedSemaphorePermit) {
        self.release(item, permit);
    }

    fn stats(&self) -> PoolStats {
        PoolStats {
            checked_out: self.checked_out.load(Ordering::Relaxed) as usize,
            available: self.permits.available_permits(),
            acquired_total: self.acquired_total.load(Ordering::Relaxed),
            released_total: self.released_total.load(Ordering::Relaxed),
            high_water_items: self.high_water.load(Ordering::Relaxed) as usize,
            wait_count: self.wait_count.load(Ordering::Relaxed),
        }
    }
}

/// A bounded, semantically correct object pool (§12.1).
pub struct BoundedPool<T> {
    inner: Arc<PoolInner<T>>,
}

impl<T> Clone for BoundedPool<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> BoundedPool<T> {
    /// An empty pool of `capacity` slots.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(PoolInner {
                capacity,
                slots: Mutex::new(VecDeque::with_capacity(capacity)),
                permits: Arc::new(Semaphore::new(0)),
                checked_out: AtomicU64::new(0),
                acquired_total: AtomicU64::new(0),
                released_total: AtomicU64::new(0),
                high_water: AtomicU64::new(0),
                wait_count: AtomicU64::new(0),
                closed: AtomicU64::new(0),
            }),
        }
    }

    /// Declared capacity.
    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    /// Idle items currently in the pool.
    pub fn available(&self) -> usize {
        self.inner.permits.available_permits()
    }

    /// Items currently in the hands of callers.
    pub fn checked_out(&self) -> usize {
        self.inner.checked_out.load(Ordering::Relaxed) as usize
    }

    /// Inserts an item into the pool, increasing its available count by one.
    ///
    /// An item in the pool holds a permit; when checked out, the permit
    /// travels with it and is only restored when the item is returned.
    pub fn insert(&self, item: T) -> Result<(), PoolError> {
        if self.inner.permits.available_permits() >= self.inner.capacity {
            return Err(PoolError::Full {
                capacity: self.inner.capacity,
            });
        }
        self.inner.slots.lock().unwrap().push_back(item);
        self.inner.permits.add_permits(1);
        Ok(())
    }

    /// Non-blocking checkout.
    pub fn try_acquire(&self) -> Result<Pooled<T>, PoolError> {
        let permit = self
            .inner
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| PoolError::Closed)?;
        self.finish_checkout(permit)
    }

    /// Blocking checkout.
    pub async fn acquire(&self) -> Result<Pooled<T>, PoolError> {
        self.inner.wait_count.fetch_add(1, Ordering::Relaxed);
        let permit = self
            .inner
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| PoolError::Closed)?;
        self.finish_checkout(permit)
    }

    /// Checkout with a timeout.
    pub async fn acquire_timeout(&self, timeout: Duration) -> Result<Pooled<T>, PoolError> {
        self.inner.wait_count.fetch_add(1, Ordering::Relaxed);
        let permit =
            match tokio::time::timeout(timeout, self.inner.permits.clone().acquire_owned()).await {
                Ok(Ok(permit)) => permit,
                Ok(Err(_)) => return Err(PoolError::Closed),
                Err(_) => return Err(PoolError::Timeout),
            };
        self.finish_checkout(permit)
    }

    /// Accounting snapshot.
    pub fn stats(&self) -> PoolStats {
        self.inner.stats()
    }

    /// Closes the pool: checked-out items are returned to be dropped
    /// instead of pooled; future acquisitions fail.
    pub fn close(&self) {
        self.inner.closed.store(1, Ordering::Release);
    }

    fn finish_checkout(&self, permit: OwnedSemaphorePermit) -> Result<Pooled<T>, PoolError> {
        let item = self
            .inner
            .slots
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(PoolError::Closed)?;
        let checked_out = self.inner.checked_out.fetch_add(1, Ordering::Relaxed) + 1;
        self.inner
            .high_water
            .fetch_max(checked_out, Ordering::Relaxed);
        self.inner.acquired_total.fetch_add(1, Ordering::Relaxed);
        Ok(Pooled {
            item: Some(item),
            pool: self.inner.clone(),
            permit: Some(permit),
        })
    }
}

impl<T: Default> BoundedPool<T> {
    /// Creates a pool prefilled with `count` default items.
    pub fn prefilled(count: usize) -> Self {
        let pool = Self::new(count);
        for _ in 0..count {
            pool.insert(T::default()).expect("prefill within capacity");
        }
        pool
    }
}

/// Buffer pool type used by [`crate::CellContext`] (§12.1).
pub type BufferPool = BoundedPool<Vec<u8>>;

impl BufferPool {
    /// Creates a buffer pool prefilled with `count` buffers of
    /// `buffer_bytes` capacity.
    pub fn prefilled_buffers(count: usize, buffer_bytes: usize) -> Self {
        let pool = BoundedPool::new(count);
        for _ in 0..count {
            pool.insert(Vec::with_capacity(buffer_bytes))
                .expect("prefill within capacity");
        }
        pool
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_pool_never_overcommits_capacity() {
        let pool: BoundedPool<u32> = BoundedPool::prefilled(2);
        let a = pool.try_acquire().unwrap();
        let b = pool.try_acquire().unwrap();
        assert!(pool.try_acquire().is_err());
        assert_eq!(pool.checked_out(), 2);
        drop(a);
        assert_eq!(pool.available(), 1);
        let c = pool.try_acquire().unwrap();
        assert!(pool.try_acquire().is_err());
        drop(b);
        drop(c);
        assert_eq!(pool.available(), 2);
    }

    #[tokio::test]
    async fn test_insert_returns_items_and_permits() {
        let pool = BoundedPool::new(2);
        pool.insert(1u32).unwrap();
        pool.insert(2u32).unwrap();
        assert_eq!(pool.insert(3u32), Err(PoolError::Full { capacity: 2 }));

        let a = pool.try_acquire().unwrap();
        assert_eq!(*a, 1);
        drop(a);
        assert_eq!(pool.available(), 2);
        // Returned items are pooled again (order is not guaranteed).
        let _a = pool.try_acquire().unwrap();
        let _b = pool.try_acquire().unwrap();
        assert!(pool.try_acquire().is_err());
        drop(_a);
        drop(_b);
    }

    #[tokio::test]
    async fn test_acquire_waits_for_release() {
        let pool: BoundedPool<u8> = BoundedPool::prefilled(1);
        let held = pool.try_acquire().unwrap();
        let task = tokio::spawn({
            let pool = pool.clone();
            async move { pool.acquire().await.unwrap() }
        });
        tokio::task::yield_now().await;
        drop(held);
        let acquired = task.await.unwrap();
        drop(acquired);
    }

    #[tokio::test]
    async fn test_acquire_timeout() {
        let pool: BoundedPool<u8> = BoundedPool::prefilled(1);
        let _held = pool.try_acquire().unwrap();
        let result = pool.acquire_timeout(Duration::from_millis(50)).await;
        assert!(matches!(result, Err(PoolError::Timeout)));
    }

    #[tokio::test]
    async fn test_release_returns_to_pool_or_drops_after_close() {
        let pool: BoundedPool<Vec<u8>> = BoundedPool::prefilled(1);
        let mut a = pool.try_acquire().unwrap();
        a.push(1u8);
        pool.close();
        a.release(); // returned while closed: dropped, not pooled
        assert_eq!(pool.available(), 0);
        assert!(pool.try_acquire().is_err());
    }

    #[tokio::test]
    async fn test_stats_accounting() {
        let pool: BoundedPool<u32> = BoundedPool::prefilled(3);
        let a = pool.try_acquire().unwrap();
        let b = pool.try_acquire().unwrap();
        let stats = pool.stats();
        assert_eq!(stats.checked_out, 2);
        assert_eq!(stats.available, 1);
        assert_eq!(stats.acquired_total, 2);
        drop(a);
        drop(b);
        let stats = pool.stats();
        assert_eq!(stats.released_total, 2);
        assert_eq!(stats.checked_out, 0);
        assert_eq!(stats.high_water_items, 2);
    }

    #[tokio::test]
    async fn test_buffer_pool_prefill() {
        let pool = BufferPool::prefilled_buffers(8, 256 * 1024);
        assert_eq!(pool.capacity(), 8);
        let mut buf = pool.try_acquire().unwrap();
        assert!(buf.capacity() >= 256 * 1024);
        buf.extend_from_slice(b"payload");
        assert_eq!(buf.as_slice(), b"payload");
        drop(buf);
        assert_eq!(pool.available(), 8);
    }

    #[tokio::test]
    async fn test_pooled_deref() {
        let pool: BoundedPool<Vec<u8>> = BoundedPool::prefilled(1);
        let mut buf = pool.try_acquire().unwrap();
        buf.push(7u8);
        assert_eq!(buf.item(), &vec![7u8]);
        assert_eq!(buf.as_slice(), &[7u8]);
        drop(buf);
    }
}
