//! Copy counters distinguishing domain payload vs framework copies (P1.4).

use std::sync::atomic::{AtomicU64, Ordering};

static DOMAIN_COPIES: AtomicU64 = AtomicU64::new(0);
static DOMAIN_BYTES: AtomicU64 = AtomicU64::new(0);
static FRAMEWORK_COPIES: AtomicU64 = AtomicU64::new(0);
static FRAMEWORK_BYTES: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyKind {
    /// Unavoidable domain payload (candidate/state bytes).
    DomainPayload,
    /// Framework-internal bookkeeping copies.
    Framework,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CopyCounterSnapshot {
    pub domain_copies: u64,
    pub domain_bytes: u64,
    pub framework_copies: u64,
    pub framework_bytes: u64,
}

pub fn record_copy(kind: CopyKind, bytes: u64) {
    match kind {
        CopyKind::DomainPayload => {
            DOMAIN_COPIES.fetch_add(1, Ordering::Relaxed);
            DOMAIN_BYTES.fetch_add(bytes, Ordering::Relaxed);
        }
        CopyKind::Framework => {
            FRAMEWORK_COPIES.fetch_add(1, Ordering::Relaxed);
            FRAMEWORK_BYTES.fetch_add(bytes, Ordering::Relaxed);
        }
    }
}

pub fn snapshot() -> CopyCounterSnapshot {
    CopyCounterSnapshot {
        domain_copies: DOMAIN_COPIES.load(Ordering::Relaxed),
        domain_bytes: DOMAIN_BYTES.load(Ordering::Relaxed),
        framework_copies: FRAMEWORK_COPIES.load(Ordering::Relaxed),
        framework_bytes: FRAMEWORK_BYTES.load(Ordering::Relaxed),
    }
}

pub fn reset() {
    DOMAIN_COPIES.store(0, Ordering::Relaxed);
    DOMAIN_BYTES.store(0, Ordering::Relaxed);
    FRAMEWORK_COPIES.store(0, Ordering::Relaxed);
    FRAMEWORK_BYTES.store(0, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_copy_counters_distinguish_domain_and_framework() {
        reset();
        record_copy(CopyKind::DomainPayload, 128);
        record_copy(CopyKind::Framework, 32);
        let snap = snapshot();
        assert_eq!(snap.domain_copies, 1);
        assert_eq!(snap.domain_bytes, 128);
        assert_eq!(snap.framework_copies, 1);
        assert_eq!(snap.framework_bytes, 32);
    }
}
