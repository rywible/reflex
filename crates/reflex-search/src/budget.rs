use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Instant;

/// Process CPU/RSS sample for budget checks (§11.4 / P1.3 integration point).
#[derive(Clone, Debug, Default)]
pub struct ProcessSample {
    pub user_cpu_ns: u64,
    pub sys_cpu_ns: u64,
    pub rss_bytes: u64,
}

/// Pluggable OS sampler; wire [`reflex_runtime::ProcessTreeAccountant`] at the cell layer.
pub trait ProcessSampler: Send + Sync {
    fn sample(&self) -> ProcessSample;
}

/// Which budget limit fired first (§11.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BudgetKind {
    VerifiedActions,
    SuccessorGoals,
    NodeCount,
    VerifierCalls,
    ProcessCpu,
    WallClock,
    RssBytes,
    ArtifactBytes,
}

impl BudgetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetKind::VerifiedActions => "verified_actions",
            BudgetKind::SuccessorGoals => "successor_goals",
            BudgetKind::NodeCount => "node_count",
            BudgetKind::VerifierCalls => "verifier_calls",
            BudgetKind::ProcessCpu => "process_cpu",
            BudgetKind::WallClock => "wall_clock",
            BudgetKind::RssBytes => "rss_bytes",
            BudgetKind::ArtifactBytes => "artifact_bytes",
        }
    }
}

/// Observed value when a budget fires.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetFired {
    pub kind: BudgetKind,
    pub observed: u64,
    pub limit: u64,
}

/// Configurable limits for all search budgets (§11.4).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetLimits {
    pub verified_actions: u32,
    pub successor_goals: u32,
    pub node_count: u32,
    pub verifier_calls: u32,
    pub process_cpu_seconds: f64,
    pub wall_seconds: f64,
    pub rss_bytes: u64,
    pub artifact_bytes: u64,
}

impl BudgetLimits {
    pub fn default_for_test() -> Self {
        Self {
            verified_actions: 10_000,
            successor_goals: 10_000,
            node_count: 50_000,
            verifier_calls: 1_000,
            process_cpu_seconds: 30.0,
            wall_seconds: 30.0,
            rss_bytes: 8 * 1024 * 1024 * 1024,
            artifact_bytes: 512 * 1024 * 1024,
        }
    }
}

/// Hot logical counters checked every action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetCounters {
    pub verified_actions: u32,
    pub successor_goals: u32,
    pub node_count: u32,
    pub verifier_calls: u32,
    pub artifact_bytes: u64,
}

/// Legacy three-scalar budget (converted into [`BudgetSet`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchBudget {
    pub action_budget: u32,
    pub node_budget: u32,
    pub cpu_seconds: f64,
}

impl SearchBudget {
    pub fn default_for_test() -> Self {
        Self {
            action_budget: 10_000,
            node_budget: 50_000,
            cpu_seconds: 30.0,
        }
    }
}

impl From<SearchBudget> for BudgetSet {
    fn from(b: SearchBudget) -> Self {
        BudgetSet::new(
            BudgetLimits {
                verified_actions: b.action_budget,
                node_count: b.node_budget,
                wall_seconds: b.cpu_seconds,
                process_cpu_seconds: b.cpu_seconds,
                ..BudgetLimits::default_for_test()
            },
            None,
        )
    }
}

/// Full budget accounting with logical/CPU/wall/RSS/verifier counters (§11.4).
pub struct BudgetSet {
    pub limits: BudgetLimits,
    counters: BudgetCounters,
    pub first_fired: Option<BudgetFired>,
    wall_start: Instant,
    os_sample_cadence: u32,
    actions_since_sample: u32,
    last_rss_bytes: u64,
    last_process_cpu_ns: u64,
    sampler: Option<Arc<dyn ProcessSampler>>,
}

impl Clone for BudgetSet {
    fn clone(&self) -> Self {
        Self {
            limits: self.limits.clone(),
            counters: self.counters.clone(),
            first_fired: self.first_fired.clone(),
            wall_start: self.wall_start,
            os_sample_cadence: self.os_sample_cadence,
            actions_since_sample: self.actions_since_sample,
            last_rss_bytes: self.last_rss_bytes,
            last_process_cpu_ns: self.last_process_cpu_ns,
            sampler: self.sampler.clone(),
        }
    }
}

impl BudgetSet {
    pub fn new(limits: BudgetLimits, sampler: Option<Arc<dyn ProcessSampler>>) -> Self {
        Self {
            limits,
            counters: BudgetCounters::default(),
            first_fired: None,
            wall_start: Instant::now(),
            os_sample_cadence: 64,
            actions_since_sample: 0,
            last_rss_bytes: 0,
            last_process_cpu_ns: 0,
            sampler,
        }
    }

    pub fn default_for_test() -> Self {
        Self::new(BudgetLimits::default_for_test(), None)
    }

    pub fn with_sampler(sampler: Arc<dyn ProcessSampler>) -> Self {
        Self::new(BudgetLimits::default_for_test(), Some(sampler))
    }

    pub fn counters(&self) -> &BudgetCounters {
        &self.counters
    }

    /// Returns an attributable process-tree CPU sample when the cell wired a
    /// sampler. Absence is preserved as unknown; callers must not substitute
    /// wall time or zero.
    pub fn sample_process_cpu_ns(&self) -> Option<u64> {
        self.sampler.as_ref().map(|sampler| {
            let sample = sampler.sample();
            sample.user_cpu_ns.saturating_add(sample.sys_cpu_ns)
        })
    }

    /// Start a fresh accounting interval while preserving configured limits
    /// and the process sampler. A `BudgetSet` belongs to the kernel, but its
    /// counters belong to one run.
    pub fn reset(&mut self) {
        self.counters = BudgetCounters::default();
        self.first_fired = None;
        self.wall_start = Instant::now();
        self.actions_since_sample = 0;
        self.last_rss_bytes = 0;
        self.last_process_cpu_ns = 0;
    }

    pub fn record_verified_action(&mut self) {
        self.counters.verified_actions = self.counters.verified_actions.saturating_add(1);
    }

    pub fn record_successor_goal(&mut self) {
        self.counters.successor_goals = self.counters.successor_goals.saturating_add(1);
    }

    pub fn record_node(&mut self) {
        self.counters.node_count = self.counters.node_count.saturating_add(1);
    }

    pub fn record_verifier_call(&mut self) {
        self.counters.verifier_calls = self.counters.verifier_calls.saturating_add(1);
    }

    pub fn record_artifact_bytes(&mut self, bytes: u64) {
        self.counters.artifact_bytes = self.counters.artifact_bytes.saturating_add(bytes);
    }

    /// Cheap logical check (< 2 ns/action target in release).
    pub fn check_logical(&mut self) -> Option<BudgetFired> {
        if self.first_fired.is_some() {
            return self.first_fired.clone();
        }
        if self.counters.verified_actions >= self.limits.verified_actions {
            return self.fire(
                BudgetKind::VerifiedActions,
                u64::from(self.counters.verified_actions),
                u64::from(self.limits.verified_actions),
            );
        }
        if self.counters.successor_goals >= self.limits.successor_goals {
            return self.fire(
                BudgetKind::SuccessorGoals,
                u64::from(self.counters.successor_goals),
                u64::from(self.limits.successor_goals),
            );
        }
        if self.counters.node_count >= self.limits.node_count {
            return self.fire(
                BudgetKind::NodeCount,
                u64::from(self.counters.node_count),
                u64::from(self.limits.node_count),
            );
        }
        if self.counters.verifier_calls >= self.limits.verifier_calls {
            return self.fire(
                BudgetKind::VerifierCalls,
                u64::from(self.counters.verifier_calls),
                u64::from(self.limits.verifier_calls),
            );
        }
        if self.counters.artifact_bytes >= self.limits.artifact_bytes {
            return self.fire(
                BudgetKind::ArtifactBytes,
                self.counters.artifact_bytes,
                self.limits.artifact_bytes,
            );
        }
        None
    }

    /// Sample OS counters at configured cadence and check wall/CPU/RSS.
    pub fn check_expensive(&mut self) -> Option<BudgetFired> {
        if self.first_fired.is_some() {
            return self.first_fired.clone();
        }

        self.actions_since_sample = self.actions_since_sample.saturating_add(1);
        if self.actions_since_sample >= self.os_sample_cadence {
            self.actions_since_sample = 0;
            self.sample_os();
        }

        let wall_elapsed = self.wall_start.elapsed().as_secs_f64();
        if wall_elapsed >= self.limits.wall_seconds {
            return self.fire(
                BudgetKind::WallClock,
                (wall_elapsed * 1_000_000.0) as u64,
                (self.limits.wall_seconds * 1_000_000.0) as u64,
            );
        }

        let cpu_limit_ns = (self.limits.process_cpu_seconds * 1_000_000_000.0) as u64;
        if self.last_process_cpu_ns >= cpu_limit_ns && cpu_limit_ns > 0 {
            return self.fire(
                BudgetKind::ProcessCpu,
                self.last_process_cpu_ns,
                cpu_limit_ns,
            );
        }

        if self.last_rss_bytes >= self.limits.rss_bytes && self.limits.rss_bytes > 0 {
            return self.fire(
                BudgetKind::RssBytes,
                self.last_rss_bytes,
                self.limits.rss_bytes,
            );
        }

        None
    }

    pub fn check_all(&mut self) -> Option<BudgetFired> {
        self.check_logical().or_else(|| self.check_expensive())
    }

    fn sample_os(&mut self) {
        if let Some(ref sampler) = self.sampler {
            let sample = sampler.sample();
            self.last_process_cpu_ns = sample.user_cpu_ns.saturating_add(sample.sys_cpu_ns);
            self.last_rss_bytes = sample.rss_bytes;
        } else {
            self.last_rss_bytes = read_self_rss_bytes().unwrap_or(0);
        }
    }

    fn fire(&mut self, kind: BudgetKind, observed: u64, limit: u64) -> Option<BudgetFired> {
        let fired = BudgetFired {
            kind,
            observed,
            limit,
        };
        if self.first_fired.is_none() {
            self.first_fired = Some(fired.clone());
        }
        Some(fired)
    }
}

impl Serialize for BudgetSet {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct BudgetSetSnapshot<'a> {
            limits: &'a BudgetLimits,
            counters: &'a BudgetCounters,
            first_fired: &'a Option<BudgetFired>,
        }
        BudgetSetSnapshot {
            limits: &self.limits,
            counters: &self.counters,
            first_fired: &self.first_fired,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for BudgetSet {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct BudgetSetSnapshot {
            limits: BudgetLimits,
            counters: BudgetCounters,
            first_fired: Option<BudgetFired>,
        }
        let snap = BudgetSetSnapshot::deserialize(deserializer)?;
        Ok(BudgetSet {
            limits: snap.limits,
            counters: snap.counters,
            first_fired: snap.first_fired,
            wall_start: Instant::now(),
            os_sample_cadence: 64,
            actions_since_sample: 0,
            last_rss_bytes: 0,
            last_process_cpu_ns: 0,
            sampler: None,
        })
    }
}

/// Read RSS from `/proc/self/status` on Linux; returns None elsewhere.
pub fn read_self_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("VmRSS:") {
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedSampler {
        sample: ProcessSample,
    }

    impl ProcessSampler for FixedSampler {
        fn sample(&self) -> ProcessSample {
            self.sample.clone()
        }
    }

    #[test]
    fn test_budget_default() {
        let b = SearchBudget::default_for_test();
        assert_eq!(b.action_budget, 10_000);
        assert_eq!(b.node_budget, 50_000);
        assert!((b.cpu_seconds - 30.0).abs() < f64::EPSILON);

        let set: BudgetSet = b.into();
        assert_eq!(set.limits.verified_actions, 10_000);
        assert_eq!(set.limits.node_count, 50_000);
    }

    #[test]
    fn test_budget_exhaustion_names_budget() {
        let mut set = BudgetSet::new(
            BudgetLimits {
                verified_actions: 2,
                ..BudgetLimits::default_for_test()
            },
            None,
        );
        set.record_verified_action();
        set.record_verified_action();
        let fired = set.check_logical().expect("should fire");
        assert_eq!(fired.kind, BudgetKind::VerifiedActions);
        assert_eq!(fired.observed, 2);
    }

    #[test]
    fn test_logical_budget_deterministic() {
        let mut a = BudgetSet::new(
            BudgetLimits {
                verified_actions: 5,
                ..BudgetLimits::default_for_test()
            },
            None,
        );
        let mut b = a.clone();
        for _ in 0..5 {
            a.record_verified_action();
            b.record_verified_action();
        }
        assert_eq!(a.check_logical(), b.check_logical());
    }

    #[test]
    fn test_rss_memory_budget() {
        let mut set = BudgetSet::new(
            BudgetLimits {
                rss_bytes: 1,
                ..BudgetLimits::default_for_test()
            },
            None,
        );
        set.last_rss_bytes = 1024;
        let fired = set.check_expensive().expect("rss should fire");
        assert_eq!(fired.kind, BudgetKind::RssBytes);
    }

    #[test]
    fn test_cpu_budget_includes_descendants() {
        let sampler = Arc::new(FixedSampler {
            sample: ProcessSample {
                user_cpu_ns: 2_000_000_000,
                sys_cpu_ns: 500_000_000,
                rss_bytes: 0,
            },
        });
        let mut set = BudgetSet::new(
            BudgetLimits {
                process_cpu_seconds: 1.0,
                ..BudgetLimits::default_for_test()
            },
            Some(sampler),
        );
        set.sample_os();
        let fired = set.check_expensive().expect("cpu should fire");
        assert_eq!(fired.kind, BudgetKind::ProcessCpu);
        assert!(fired.observed >= 2_000_000_000);
    }

    #[test]
    fn test_reset_starts_a_fresh_run() {
        let mut set = BudgetSet::new(
            BudgetLimits {
                verified_actions: 1,
                ..BudgetLimits::default_for_test()
            },
            None,
        );
        set.record_verified_action();
        assert!(set.check_logical().is_some());

        set.reset();
        assert_eq!(set.counters().verified_actions, 0);
        assert!(set.first_fired.is_none());
        assert!(set.check_logical().is_none());
    }
}
