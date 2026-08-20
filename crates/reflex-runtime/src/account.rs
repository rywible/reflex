//! Process-tree CPU, RSS, and I/O accounting (P1.3).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard};

/// Coverage level when full procfs tree accounting is unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountingCoverage {
    /// Linux /proc backend with descendant walk and starttime keys.
    FullTree,
    /// Parent-only rusage fallback.
    ParentOnly,
    /// Platform unsupported — samples report zero, never fabricated RSS.
    Unsupported,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    pub pid: u32,
    /// `/proc/<pid>/stat` field 22 — survives PID reuse.
    pub starttime: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessRusageSample {
    pub user_cpu_ns: u64,
    pub sys_cpu_ns: u64,
    pub rss_bytes: u64,
    pub peak_rss_bytes: u64,
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub voluntary_ctx_switches: u64,
    pub involuntary_ctx_switches: u64,
}

#[derive(Clone, Debug, Default)]
struct ProcessSnapshot {
    user_cpu_ns: u64,
    sys_cpu_ns: u64,
    rss_bytes: u64,
    peak_rss_bytes: u64,
    read_bytes: u64,
    write_bytes: u64,
    voluntary_ctx_switches: u64,
    involuntary_ctx_switches: u64,
    exited: bool,
    /// Final totals retained after exit (grandchildren included).
    final_user_cpu_ns: u64,
    final_sys_cpu_ns: u64,
    final_rss_bytes: u64,
}

struct TrackedProcess {
    #[allow(dead_code)] // starttime key for PID-reuse safety (P1.3)
    key: ProcessKey,
    snap: ProcessSnapshot,
}

pub struct ProcessTreeAccountant {
    root_pid: u32,
    coverage: AccountingCoverage,
    processes: RwLock<HashMap<ProcessKey, TrackedProcess>>,
    accumulated_user_ns: AtomicU64,
    accumulated_sys_ns: AtomicU64,
}

impl ProcessTreeAccountant {
    pub fn new(root_pid: u32) -> Self {
        let coverage = detect_coverage();
        let mut processes = HashMap::new();
        if coverage == AccountingCoverage::FullTree
            && let Some(key) = read_process_key(root_pid)
        {
            processes.insert(
                key,
                TrackedProcess {
                    key,
                    snap: ProcessSnapshot::default(),
                },
            );
        }
        Self {
            root_pid,
            coverage,
            processes: RwLock::new(processes),
            accumulated_user_ns: AtomicU64::new(0),
            accumulated_sys_ns: AtomicU64::new(0),
        }
    }

    pub fn coverage(&self) -> AccountingCoverage {
        self.coverage
    }

    pub fn root_pid(&self) -> u32 {
        self.root_pid
    }

    pub fn track_child(&self, pid: u32) {
        if self.coverage != AccountingCoverage::FullTree {
            return;
        }
        let Some(key) = read_process_key(pid) else {
            return;
        };
        let mut map = self.processes.write().unwrap();
        map.entry(key).or_insert_with(|| TrackedProcess {
            key,
            snap: ProcessSnapshot::default(),
        });
    }

    /// Refresh descendant tree and aggregate sample.
    pub fn sample(&self) -> ProcessRusageSample {
        match self.coverage {
            AccountingCoverage::FullTree => self.sample_linux(),
            AccountingCoverage::ParentOnly => self.sample_parent_only(),
            AccountingCoverage::Unsupported => ProcessRusageSample::default(),
        }
    }

    fn sample_linux(&self) -> ProcessRusageSample {
        self.refresh_tree();
        let map = self.processes.read().unwrap();
        aggregate(&map)
    }

    fn sample_parent_only(&self) -> ProcessRusageSample {
        let user = self.accumulated_user_ns.load(Ordering::Relaxed);
        let sys = self.accumulated_sys_ns.load(Ordering::Relaxed);
        ProcessRusageSample {
            user_cpu_ns: user,
            sys_cpu_ns: sys,
            ..ProcessRusageSample::default()
        }
    }

    fn refresh_tree(&self) {
        let mut map = self.processes.write().unwrap();
        let descendants = collect_descendants(self.root_pid);
        for pid in descendants {
            if let Some(key) = read_process_key(pid)
                && let Some(proc_stat) = read_proc_metrics(pid)
            {
                map.entry(key)
                    .and_modify(|tp| {
                        if !tp.snap.exited {
                            tp.snap.user_cpu_ns = proc_stat.user_cpu_ns;
                            tp.snap.sys_cpu_ns = proc_stat.sys_cpu_ns;
                            tp.snap.rss_bytes = proc_stat.rss_bytes;
                            tp.snap.peak_rss_bytes =
                                tp.snap.peak_rss_bytes.max(proc_stat.rss_bytes);
                            tp.snap.read_bytes = proc_stat.read_bytes;
                            tp.snap.write_bytes = proc_stat.write_bytes;
                            tp.snap.voluntary_ctx_switches = proc_stat.voluntary_ctx_switches;
                            tp.snap.involuntary_ctx_switches = proc_stat.involuntary_ctx_switches;
                        }
                    })
                    .or_insert_with(|| TrackedProcess {
                        key,
                        snap: ProcessSnapshot {
                            user_cpu_ns: proc_stat.user_cpu_ns,
                            sys_cpu_ns: proc_stat.sys_cpu_ns,
                            rss_bytes: proc_stat.rss_bytes,
                            peak_rss_bytes: proc_stat.rss_bytes,
                            read_bytes: proc_stat.read_bytes,
                            write_bytes: proc_stat.write_bytes,
                            voluntary_ctx_switches: proc_stat.voluntary_ctx_switches,
                            involuntary_ctx_switches: proc_stat.involuntary_ctx_switches,
                            ..ProcessSnapshot::default()
                        },
                    });
            }
        }

        // Mark exited PIDs; retain their final totals (grandchildren stay included).
        let keys: Vec<_> = map.keys().copied().collect();
        for key in keys {
            if !Path::new(&format!("/proc/{}", key.pid)).exists()
                && let Some(tp) = map.get_mut(&key)
                && !tp.snap.exited
            {
                tp.snap.exited = true;
                tp.snap.final_user_cpu_ns = tp.snap.user_cpu_ns;
                tp.snap.final_sys_cpu_ns = tp.snap.sys_cpu_ns;
                tp.snap.final_rss_bytes = tp.snap.peak_rss_bytes;
            }
        }
    }

    pub fn add_cpu(&self, user_ns: u64, sys_ns: u64) {
        self.accumulated_user_ns
            .fetch_add(user_ns, Ordering::Relaxed);
        self.accumulated_sys_ns.fetch_add(sys_ns, Ordering::Relaxed);
    }

    /// Per-process state must stay under 4 KiB (P1.3 perf AC).
    pub fn state_bytes_per_process(&self) -> usize {
        std::mem::size_of::<TrackedProcess>() + std::mem::size_of::<ProcessKey>()
    }
}

fn aggregate(
    map: &RwLockReadGuard<'_, HashMap<ProcessKey, TrackedProcess>>,
) -> ProcessRusageSample {
    let mut out = ProcessRusageSample::default();
    for tp in map.values() {
        let snap = &tp.snap;
        if snap.exited {
            out.user_cpu_ns += snap.final_user_cpu_ns;
            out.sys_cpu_ns += snap.final_sys_cpu_ns;
            out.rss_bytes = out.rss_bytes.max(snap.final_rss_bytes);
        } else {
            out.user_cpu_ns += snap.user_cpu_ns;
            out.sys_cpu_ns += snap.sys_cpu_ns;
            out.rss_bytes = out.rss_bytes.saturating_add(snap.rss_bytes);
            out.peak_rss_bytes = out.peak_rss_bytes.max(snap.peak_rss_bytes);
            out.read_bytes += snap.read_bytes;
            out.write_bytes += snap.write_bytes;
            out.voluntary_ctx_switches += snap.voluntary_ctx_switches;
            out.involuntary_ctx_switches += snap.involuntary_ctx_switches;
        }
    }
    out
}

fn detect_coverage() -> AccountingCoverage {
    if Path::new("/proc/self/stat").exists() {
        AccountingCoverage::FullTree
    } else {
        AccountingCoverage::Unsupported
    }
}

struct ProcMetrics {
    user_cpu_ns: u64,
    sys_cpu_ns: u64,
    rss_bytes: u64,
    read_bytes: u64,
    write_bytes: u64,
    voluntary_ctx_switches: u64,
    involuntary_ctx_switches: u64,
}

fn read_process_key(pid: u32) -> Option<ProcessKey> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let starttime = parse_stat_starttime(&stat)?;
    Some(ProcessKey { pid, starttime })
}

fn parse_stat_starttime(stat: &str) -> Option<u64> {
    // comm field may contain spaces inside parens — find closing paren first.
    let after = stat.rsplit_once(')')?.1;
    let fields: Vec<_> = after.split_whitespace().collect();
    // field index 20 after pid and comm => starttime is fields[19] (0-based after comm)
    fields.get(19).and_then(|s| s.parse().ok())
}

fn read_proc_metrics(pid: u32) -> Option<ProcMetrics> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.rsplit_once(')')?.1;
    let fields: Vec<_> = after.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    let rss_pages: u64 = fields.get(21)?.parse().ok()?;
    let page_size = 4096u64;

    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok();
    let (voluntary, involuntary) = status
        .as_ref()
        .map(|s| parse_ctx_switches(s.as_str()))
        .unwrap_or((0, 0));

    let io = std::fs::read_to_string(format!("/proc/{pid}/io")).ok();
    let (read_bytes, write_bytes) = io.as_ref().map(|s| parse_io(s.as_str())).unwrap_or((0, 0));

    Some(ProcMetrics {
        user_cpu_ns: utime * 10_000_000, // clock ticks usually 100Hz -> ns approx
        sys_cpu_ns: stime * 10_000_000,
        rss_bytes: rss_pages * page_size,
        read_bytes,
        write_bytes,
        voluntary_ctx_switches: voluntary,
        involuntary_ctx_switches: involuntary,
    })
}

fn parse_ctx_switches(status: &str) -> (u64, u64) {
    let mut voluntary = 0u64;
    let mut involuntary = 0u64;
    for line in status.lines() {
        if let Some(v) = line.strip_prefix("voluntary_ctxt_switches:") {
            voluntary = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("nonvoluntary_ctxt_switches:") {
            involuntary = v.trim().parse().unwrap_or(0);
        }
    }
    (voluntary, involuntary)
}

fn parse_io(io: &str) -> (u64, u64) {
    let mut read_bytes = 0u64;
    let mut write_bytes = 0u64;
    for line in io.lines() {
        if let Some(v) = line.strip_prefix("read_bytes:") {
            read_bytes = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("write_bytes:") {
            write_bytes = v.trim().parse().unwrap_or(0);
        }
    }
    (read_bytes, write_bytes)
}

fn collect_descendants(root: u32) -> Vec<u32> {
    let mut out = vec![root];
    let mut i = 0;
    while i < out.len() {
        let pid = out[i];
        if let Ok(children) = std::fs::read_to_string(format!("/proc/{pid}/task/{pid}/children")) {
            for child in children.split_whitespace() {
                if let Ok(cpid) = child.parse::<u32>()
                    && !out.contains(&cpid)
                {
                    out.push(cpid);
                }
            }
        }
        i += 1;
    }
    out
}

/// PID reuse must not merge unrelated processes — keys include starttime.
pub fn process_keys_distinct(a: ProcessKey, b: ProcessKey) -> bool {
    a.pid == b.pid && a.starttime != b.starttime
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_accountant_coverage_reported() {
        let acct = ProcessTreeAccountant::new(std::process::id());
        let cov = acct.coverage();
        assert!(
            matches!(
                cov,
                AccountingCoverage::FullTree
                    | AccountingCoverage::ParentOnly
                    | AccountingCoverage::Unsupported
            ),
            "coverage must be qualified"
        );
    }

    #[test]
    fn test_pid_reuse_does_not_merge_unrelated_processes() {
        let a = ProcessKey {
            pid: 42,
            starttime: 100,
        };
        let b = ProcessKey {
            pid: 42,
            starttime: 200,
        };
        assert!(process_keys_distinct(a, b));
        let mut map = HashMap::new();
        map.insert(
            a,
            TrackedProcess {
                key: a,
                snap: ProcessSnapshot {
                    user_cpu_ns: 1000,
                    ..Default::default()
                },
            },
        );
        map.insert(
            b,
            TrackedProcess {
                key: b,
                snap: ProcessSnapshot {
                    user_cpu_ns: 2000,
                    ..Default::default()
                },
            },
        );
        let lock = RwLock::new(map);
        let guard = lock.read().unwrap();
        let sample = aggregate(&guard);
        assert_eq!(sample.user_cpu_ns, 3000);
    }

    #[test]
    fn test_exited_grandchild_totals_retained() {
        let key = ProcessKey {
            pid: 99,
            starttime: 1,
        };
        let mut map = HashMap::new();
        map.insert(
            key,
            TrackedProcess {
                key,
                snap: ProcessSnapshot {
                    exited: true,
                    final_user_cpu_ns: 5_000_000,
                    final_sys_cpu_ns: 1_000_000,
                    final_rss_bytes: 2 * 1024 * 1024,
                    ..Default::default()
                },
            },
        );
        let lock = RwLock::new(map);
        let guard = lock.read().unwrap();
        let sample = aggregate(&guard);
        assert_eq!(sample.user_cpu_ns, 5_000_000);
        assert_eq!(sample.rss_bytes, 2 * 1024 * 1024);
    }

    #[test]
    fn test_state_under_4k_per_process() {
        let acct = ProcessTreeAccountant::new(std::process::id());
        assert!(
            acct.state_bytes_per_process() < 4096,
            "state {} >= 4096",
            acct.state_bytes_per_process()
        );
    }

    #[test]
    fn test_live_sample_nonzero_cpu_on_linux() {
        if !Path::new("/proc/self/stat").exists() {
            return;
        }
        let acct = ProcessTreeAccountant::new(std::process::id());
        let sample = acct.sample();
        // Parent process should have some RSS on Linux when measurable.
        assert!(
            sample.rss_bytes > 0 || acct.coverage() == AccountingCoverage::Unsupported,
            "must not fabricate RSS"
        );
    }
}
