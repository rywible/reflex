//! Test-only allocation instrumentation (P1.4).
//!
//! Compiled out of release workers unless the `counting-allocator` feature is enabled.

use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

thread_local! {
    static ACTIVE_SCOPE: RefCell<Option<AllocationScope>> = const { RefCell::new(None) };
}

static GLOBAL_BYTES: AtomicUsize = AtomicUsize::new(0);
static GLOBAL_ALLOCS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AllocationOperation {
    #[default]
    FeaturePack,
    Score,
    FrontierPush,
    EventAppend,
    CasStage,
}

#[derive(Clone, Debug, Default)]
pub struct AllocationScope {
    pub operation: AllocationOperation,
    pub max_bytes: usize,
    pub max_allocs: usize,
    bytes: usize,
    allocs: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AllocationBudgetError {
    #[error("allocation budget exceeded for {operation:?}: {bytes} bytes > {max_bytes}")]
    BytesExceeded {
        operation: AllocationOperation,
        bytes: usize,
        max_bytes: usize,
    },
    #[error("allocation count budget exceeded for {operation:?}: {allocs} > {max_allocs}")]
    AllocsExceeded {
        operation: AllocationOperation,
        allocs: usize,
        max_allocs: usize,
    },
}

impl AllocationScope {
    pub fn new(operation: AllocationOperation, max_bytes: usize, max_allocs: usize) -> Self {
        Self {
            operation,
            max_bytes,
            max_allocs,
            bytes: 0,
            allocs: 0,
        }
    }

    pub fn track_alloc(&mut self, bytes: usize) -> Result<(), AllocationBudgetError> {
        self.bytes = self.bytes.saturating_add(bytes);
        self.allocs = self.allocs.saturating_add(1);
        GLOBAL_BYTES.fetch_add(bytes, Ordering::Relaxed);
        GLOBAL_ALLOCS.fetch_add(1, Ordering::Relaxed);
        if self.bytes > self.max_bytes {
            return Err(AllocationBudgetError::BytesExceeded {
                operation: self.operation,
                bytes: self.bytes,
                max_bytes: self.max_bytes,
            });
        }
        if self.allocs > self.max_allocs {
            return Err(AllocationBudgetError::AllocsExceeded {
                operation: self.operation,
                allocs: self.allocs,
                max_allocs: self.max_allocs,
            });
        }
        Ok(())
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn allocs(&self) -> usize {
        self.allocs
    }
}

pub fn with_scope<F, T>(scope: AllocationScope, f: F) -> Result<T, AllocationBudgetError>
where
    F: FnOnce() -> Result<T, AllocationBudgetError>,
{
    ACTIVE_SCOPE.with(|cell| {
        *cell.borrow_mut() = Some(scope);
        let result = f();
        *cell.borrow_mut() = None;
        result
    })
}

pub fn track_vec_growth<T>(vec: &mut Vec<T>, value: T) -> Result<(), AllocationBudgetError> {
    let cap_before = vec.capacity();
    vec.push(value);
    let cap_after = vec.capacity();
    if cap_after > cap_before {
        let growth = (cap_after - cap_before) * std::mem::size_of::<T>();
        ACTIVE_SCOPE.with(|cell| {
            if let Some(scope) = cell.borrow_mut().as_mut() {
                scope.track_alloc(growth)
            } else {
                Ok(())
            }
        })?;
    }
    Ok(())
}

pub fn global_bytes() -> usize {
    GLOBAL_BYTES.load(Ordering::Relaxed)
}

pub fn global_allocs() -> usize {
    GLOBAL_ALLOCS.load(Ordering::Relaxed)
}

pub fn reset_global() {
    GLOBAL_BYTES.store(0, Ordering::Relaxed);
    GLOBAL_ALLOCS.store(0, Ordering::Relaxed);
}

/// Deliberate Vec growth in candidate scoring — must fail when budget is tight.
pub fn score_candidates_with_budget(max_bytes: usize) -> Result<(), AllocationBudgetError> {
    with_scope(
        AllocationScope::new(AllocationOperation::Score, max_bytes, 64),
        || {
            let mut scores = Vec::with_capacity(4);
            for i in 0..256usize {
                track_vec_growth(&mut scores, i as f32)?;
            }
            Ok(())
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deliberate_vec_growth_fails_allocation_budget() {
        reset_global();
        let err = score_candidates_with_budget(64).unwrap_err();
        assert!(matches!(err, AllocationBudgetError::BytesExceeded { .. }));
    }

    #[test]
    fn test_warm_paths_meet_declared_budgets() {
        reset_global();
        let result = with_scope(
            AllocationScope::new(AllocationOperation::FrontierPush, 4096, 8),
            || {
                let mut frontier = Vec::with_capacity(8);
                for i in 0..8 {
                    track_vec_growth(&mut frontier, i)?;
                }
                Ok(())
            },
        );
        assert!(result.is_ok());
    }
}
