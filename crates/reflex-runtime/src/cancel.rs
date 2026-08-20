//! Structured cancellation tree (F-46).
//!
//! A [`CancellationToken`] is a node in a cancellation tree:
//!
//! - cancelling a node cancels the whole subtree beneath it (parent
//!   cancellation propagates to children);
//! - a child's cancellation never leaks to its parent or siblings
//!   (independent cancellation domains);
//! - each node has a bounded fan-out; creating a child past the limit fails
//!   with [`CancellationError::FanOutLimit`] instead of silently degrading.
//!
//! The default fan-out limit is 64. `is_cancelled` consults the chain up to
//! the root, so a child observes an ancestor's cancellation immediately even
//! if it was created before the ancestor was cancelled.

use std::any::Any;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use thiserror::Error;

/// Default maximum number of children a token may create.
pub const DEFAULT_FAN_OUT_LIMIT: usize = 64;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum CancellationError {
    #[error("cancelled")]
    Cancelled,
    #[error("cancellation fan-out limit reached: {current} children, max {max}")]
    FanOutLimit { current: usize, max: usize },
}

struct CancellationState {
    cancelled: AtomicBool,
    max_children: usize,
    parent: Mutex<Option<Weak<CancellationState>>>,
    children: Mutex<Vec<Weak<CancellationState>>>,
    callbacks: Mutex<Vec<Box<dyn FnOnce() + Send + 'static>>>,
}

impl CancellationState {
    fn new() -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            max_children: DEFAULT_FAN_OUT_LIMIT,
            parent: Mutex::new(None),
            children: Mutex::new(Vec::new()),
            callbacks: Mutex::new(Vec::new()),
        }
    }

    fn with_fan_out(max_children: usize) -> Self {
        Self {
            cancelled: AtomicBool::new(false),
            max_children,
            parent: Mutex::new(None),
            children: Mutex::new(Vec::new()),
            callbacks: Mutex::new(Vec::new()),
        }
    }

    fn is_cancelled(&self) -> bool {
        if self.cancelled.load(Ordering::Acquire) {
            return true;
        }
        let parent = self.parent.lock().unwrap().clone();
        match parent {
            Some(parent) => match parent.upgrade() {
                Some(parent) => parent.is_cancelled(),
                None => false,
            },
            None => false,
        }
    }

    /// Number of live (non-dropped) children.
    fn live_children(&self) -> usize {
        let mut children = self.children.lock().unwrap();
        children.retain(|c| c.strong_count() > 0);
        children.len()
    }

    fn cancel(&self) {
        if self.cancelled.swap(true, Ordering::AcqRel) {
            return;
        }
        let callbacks = std::mem::take(&mut *self.callbacks.lock().unwrap());
        for callback in callbacks {
            callback();
        }
        let children: Vec<Arc<CancellationState>> = {
            let mut guard = self.children.lock().unwrap();
            guard.retain(|c| c.strong_count() > 0);
            guard.iter().filter_map(|c| c.upgrade()).collect::<Vec<_>>()
        };
        for child in children {
            child.cancel();
        }
    }
}

/// A node in the cell's cancellation tree.
#[derive(Clone)]
pub struct CancellationToken {
    inner: Arc<CancellationState>,
}

impl PartialEq for CancellationToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl std::fmt::Debug for CancellationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CancellationToken")
            .field("cancelled", &self.is_cancelled())
            .field("live_children", &self.live_children())
            .finish()
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancellationToken {
    /// A root token (no parent), with the default fan-out limit.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(CancellationState::new()),
        }
    }

    /// A root token with a custom fan-out limit.
    pub fn with_fan_out(max_children: usize) -> Self {
        Self {
            inner: Arc::new(CancellationState::with_fan_out(max_children)),
        }
    }

    /// Creates a child token. Cancelling `self` cancels the child; the
    /// child's cancellation does not affect `self`.
    pub fn child(&self) -> Result<Self, CancellationError> {
        if self.inner.is_cancelled() {
            return Err(CancellationError::Cancelled);
        }
        let mut children = self.inner.children.lock().unwrap();
        children.retain(|c| c.strong_count() > 0);
        if children.len() >= self.inner.max_children {
            return Err(CancellationError::FanOutLimit {
                current: children.len(),
                max: self.inner.max_children,
            });
        }
        let child = Arc::new(CancellationState::with_fan_out(self.inner.max_children));
        {
            let mut parent = child.parent.lock().unwrap();
            *parent = Some(Arc::downgrade(&self.inner));
        }
        children.push(Arc::downgrade(&child));
        Ok(Self { inner: child })
    }

    /// Whether this token (or any ancestor) has been cancelled.
    pub fn is_cancelled(&self) -> bool {
        self.inner.is_cancelled()
    }

    /// Returns `Err(Cancelled)` if this token (or an ancestor) is cancelled.
    pub fn check_cancelled(&self) -> Result<(), CancellationError> {
        if self.is_cancelled() {
            Err(CancellationError::Cancelled)
        } else {
            Ok(())
        }
    }

    /// Registers a callback to run when this token is cancelled.
    /// If the token is already cancelled the callback runs immediately.
    pub fn on_cancel(&self, callback: Box<dyn FnOnce() + Send + 'static>) {
        if self.inner.cancelled.load(Ordering::Acquire) {
            callback();
            return;
        }
        self.inner.callbacks.lock().unwrap().push(callback);
    }

    /// Number of live children.
    pub fn live_children(&self) -> usize {
        self.inner.live_children()
    }

    /// The fan-out limit of this token.
    pub fn fan_out_limit(&self) -> usize {
        self.inner.max_children
    }

    /// Cancels this token and its whole subtree.
    pub fn cancel(&self) {
        self.inner.cancel();
    }
}

/// Convenience: observe cancellation as a task payload.
pub type CancellationObserver = Box<dyn Fn() -> bool + Send + Sync>;

/// Builds a cancellation observer from a token.
pub fn observer(token: &CancellationToken) -> CancellationObserver {
    let token = token.clone();
    Box::new(move || token.is_cancelled())
}

/// Downcast helper: cancellation errors as `dyn Any` for task payloads.
pub fn as_any_error(error: &CancellationError) -> &(dyn Any + Send + Sync) {
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn test_parent_cancellation_propagates_to_children() {
        let root = CancellationToken::new();
        let child = root.child().unwrap();
        let grandchild = child.child().unwrap();
        assert!(!root.is_cancelled());
        assert!(!grandchild.is_cancelled());

        root.cancel();
        assert!(root.is_cancelled());
        assert!(child.is_cancelled());
        assert!(grandchild.is_cancelled());
        assert_eq!(child.check_cancelled(), Err(CancellationError::Cancelled));
    }

    #[test]
    fn test_child_cancellation_does_not_leak_upwards() {
        let root = CancellationToken::new();
        let child_a = root.child().unwrap();
        let child_b = root.child().unwrap();
        child_a.cancel();
        assert!(child_a.is_cancelled());
        assert!(!root.is_cancelled());
        assert!(!child_b.is_cancelled());
        // Creating more children stays possible after a sibling cancelled.
        let child_c = root.child().unwrap();
        assert!(!child_c.is_cancelled());
    }

    #[test]
    fn test_ancestor_cancellation_visible_to_existing_children() {
        let root = CancellationToken::with_fan_out(2);
        let child = root.child().unwrap();
        let grandchild = child.child().unwrap();
        // grandchild was created before root cancelled.
        root.cancel();
        assert!(grandchild.is_cancelled());
    }

    #[test]
    fn test_fan_out_limit_enforced() {
        let root = CancellationToken::with_fan_out(2);
        assert_eq!(root.fan_out_limit(), 2);
        let _a = root.child().unwrap();
        let _b = root.child().unwrap();
        match root.child() {
            Err(CancellationError::FanOutLimit { current, max }) => {
                assert_eq!(current, 2);
                assert_eq!(max, 2);
            }
            other => panic!("expected FanOutLimit, got {other:?}"),
        }
    }

    #[test]
    fn test_dropped_children_free_slots() {
        let root = CancellationToken::with_fan_out(1);
        {
            let _child = root.child().unwrap();
            assert_eq!(root.live_children(), 1);
        }
        assert_eq!(root.live_children(), 0);
        let _child2 = root.child().unwrap();
        assert_eq!(root.live_children(), 1);
    }

    #[test]
    fn test_child_creation_after_cancellation_fails() {
        let root = CancellationToken::new();
        root.cancel();
        assert_eq!(root.child(), Err(CancellationError::Cancelled));
    }

    #[test]
    fn test_callbacks_run_exactly_once_on_cancel() {
        let root = CancellationToken::new();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        root.on_cancel(Box::new(move || {
            calls2.fetch_add(1, Ordering::SeqCst);
        }));
        root.cancel();
        root.cancel();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_callback_on_already_cancelled_runs_immediately() {
        let root = CancellationToken::new();
        root.cancel();
        let calls = Arc::new(AtomicUsize::new(0));
        let calls2 = calls.clone();
        root.on_cancel(Box::new(move || {
            calls2.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn test_observer_helper() {
        let root = CancellationToken::new();
        let observe = observer(&root);
        assert!(!observe());
        root.cancel();
        assert!(observe());
    }

    #[test]
    fn test_default_fan_out_is_64() {
        let root = CancellationToken::new();
        assert_eq!(root.fan_out_limit(), DEFAULT_FAN_OUT_LIMIT);
    }
}
