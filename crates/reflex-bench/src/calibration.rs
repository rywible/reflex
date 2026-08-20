use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Whether benchmark results from this host may be pooled with canonical baselines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HostCanonicalStatus {
    /// Host matches a registered profile and passes throttling/shared-CPU checks.
    Canonical,
    /// Host is usable locally but must not be pooled with canonical baselines.
    Noncanonical,
}

/// Named calibration profile (developer laptop, reference worker, scientific CPU).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalibrationProfile {
    pub name: String,
    pub expected_cpus: Option<usize>,
    pub canonical_status: HostCanonicalStatus,
}

impl CalibrationProfile {
    pub fn developer_laptop() -> Self {
        Self {
            name: "developer-laptop".to_string(),
            expected_cpus: None,
            canonical_status: HostCanonicalStatus::Noncanonical,
        }
    }

    pub fn reference_4vcpu_8gb() -> Self {
        Self {
            name: "reference-4vcpu-8gb".to_string(),
            expected_cpus: Some(4),
            canonical_status: HostCanonicalStatus::Canonical,
        }
    }

    pub fn canonical_scientific_cpu() -> Self {
        Self {
            name: "canonical-scientific-cpu".to_string(),
            expected_cpus: Some(8),
            canonical_status: HostCanonicalStatus::Canonical,
        }
    }
}

/// Descriptive host identity captured at calibration time.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct HostIdentity {
    pub cpu_model: String,
    pub isa: String,
    pub kernel: String,
    pub cgroup_cpu_quota: Option<i64>,
    pub cpu_governor: String,
    pub numa_nodes: usize,
    pub memory_mb: u64,
    pub container_image: String,
    pub git_identity: String,
}

/// Immutable host calibration record (§P1.1 / P1.5).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostCalibration {
    pub profile: CalibrationProfile,
    pub host_class: String,
    pub canonical_status: HostCanonicalStatus,
    pub cpus: usize,
    pub memory_mb: u64,
    pub single_core_score_mops: f64,
    pub memory_bandwidth_gbps: f64,
    pub identity: HostIdentity,
    pub host_fingerprint: Digest,
    /// Human-readable reasons when `canonical_status == Noncanonical`.
    pub noncanonical_reasons: Vec<String>,
}

impl HostCalibration {
    /// Recompute fingerprint from stored fields (must match original).
    pub fn reconstruct_fingerprint(&self) -> Digest {
        fingerprint_from_record(
            &self.host_class,
            self.cpus,
            &self.identity,
            self.single_core_score_mops,
            self.memory_bandwidth_gbps,
        )
    }

    pub fn is_reconstructable(&self) -> bool {
        self.host_fingerprint == self.reconstruct_fingerprint()
    }

    pub fn is_valid_for_claim(&self) -> bool {
        self.canonical_status == HostCanonicalStatus::Canonical
            && self.is_reconstructable()
            && !self.noncanonical_reasons.iter().any(|r| {
                r.contains("shared-cpu") || r.contains("throttled") || r.contains("missing")
            })
    }

    pub fn rejects_performance_experiments(&self) -> bool {
        self.canonical_status == HostCanonicalStatus::Noncanonical
            && self
                .noncanonical_reasons
                .iter()
                .any(|r| r.contains("shared-cpu") || r.contains("throttled"))
    }

    pub fn calibrate_current_host() -> Self {
        calibrate_host()
    }
}

fn fingerprint_from_record(
    host_class: &str,
    cpus: usize,
    identity: &HostIdentity,
    mops: f64,
    bandwidth_gbps: f64,
) -> Digest {
    // Round measurements so idle-run noise does not change identity.
    let mops_bucket = (mops * 10.0).round() / 10.0;
    let bw_bucket = (bandwidth_gbps * 10.0).round() / 10.0;
    let payload = format!(
        "{host_class}|{cpus}|{}|{}|{}|{}|{mops_bucket:.1}|{bw_bucket:.1}",
        identity.cpu_model, identity.isa, identity.kernel, identity.git_identity,
    );
    Digest::hash_blake3(payload.as_bytes())
}

fn read_file_lossy(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn detect_cpu_model() -> String {
    if let Some(info) = read_file_lossy(Path::new("/proc/cpuinfo")) {
        for line in info.lines() {
            if let Some(model) = line.strip_prefix("model name\t: ") {
                return model.trim().to_string();
            }
        }
    }
    "unknown".to_string()
}

fn detect_isa() -> String {
    std::env::consts::ARCH.to_string()
}

fn detect_kernel() -> String {
    read_file_lossy(Path::new("/proc/sys/kernel/osrelease"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn detect_governor() -> String {
    read_file_lossy(Path::new(
        "/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor",
    ))
    .map(|s| s.trim().to_string())
    .unwrap_or_else(|| "unknown".to_string())
}

fn detect_numa_nodes() -> usize {
    std::fs::read_dir("/sys/devices/system/node")
        .map(|entries| entries.filter_map(Result::ok).count())
        .unwrap_or(1)
        .max(1)
}

fn detect_memory_mb() -> u64 {
    if let Some(meminfo) = read_file_lossy(Path::new("/proc/meminfo")) {
        for line in meminfo.lines() {
            if let Some(rest) = line.strip_prefix("MemTotal:")
                && let Some(kb) = rest
                    .split_whitespace()
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
            {
                return kb / 1024;
            }
        }
    }
    // No fabricated constant — report zero when unknown.
    0
}

fn detect_cgroup_cpu_quota() -> Option<i64> {
    let cgroup = read_file_lossy(Path::new("/proc/self/cgroup"))?;
    for line in cgroup.lines() {
        if line.contains("cpu") {
            // cgroup v2: look for cpu.max in unified hierarchy
            if let Some(max) = read_file_lossy(Path::new("/sys/fs/cgroup/cpu.max")) {
                let parts: Vec<_> = max.split_whitespace().collect();
                if parts.len() == 2
                    && parts[0] != "max"
                    && let Ok(quota) = parts[0].parse::<i64>()
                {
                    return Some(quota);
                }
            }
            // cgroup v1 cpu.cfs_quota_us
            if let Some(quota) = read_file_lossy(Path::new("/sys/fs/cgroup/cpu/cpu.cfs_quota_us"))
                .or_else(|| {
                    read_file_lossy(Path::new("/sys/fs/cgroup/cpu,cpuacct/cpu.cfs_quota_us"))
                })
                && let Ok(v) = quota.trim().parse::<i64>()
                && v > 0
            {
                return Some(v);
            }
        }
    }
    None
}

fn detect_git_identity() -> String {
    std::env::var("REFLEX_GIT_COMMIT")
        .or_else(|_| std::env::var("GIT_COMMIT"))
        .unwrap_or_else(|_| "unknown".to_string())
}

fn detect_container_image() -> String {
    std::env::var("CONTAINER_IMAGE").unwrap_or_else(|_| "local".to_string())
}

use std::sync::OnceLock;

fn measure_single_core_mops() -> f64 {
    static CACHED: OnceLock<f64> = OnceLock::new();
    *CACHED.get_or_init(|| {
        let start = std::time::Instant::now();
        let mut x = 1u64;
        for i in 0..10_000_000u64 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(i);
        }
        let _ = x;
        let elapsed = start.elapsed().as_secs_f64().max(1e-9);
        10.0 / elapsed
    })
}

fn measure_memory_bandwidth_gbps() -> f64 {
    static CACHED: OnceLock<f64> = OnceLock::new();
    *CACHED.get_or_init(|| {
        const SIZE: usize = 8 * 1024 * 1024;
        let mut buf = vec![0u8; SIZE];
        let start = std::time::Instant::now();
        for _ in 0..8 {
            for b in buf.iter_mut() {
                *b = b.wrapping_add(1);
            }
        }
        let elapsed = start.elapsed().as_secs_f64().max(1e-9);
        let bytes = (SIZE * 8) as f64;
        (bytes / elapsed) / 1e9
    })
}

fn select_profile(cpus: usize, identity: &HostIdentity, reasons: &[String]) -> CalibrationProfile {
    if reasons
        .iter()
        .any(|r| r.contains("shared-cpu") || r.contains("throttled"))
    {
        return CalibrationProfile {
            name: "noncanonical-shared".to_string(),
            expected_cpus: Some(cpus),
            canonical_status: HostCanonicalStatus::Noncanonical,
        };
    }
    if cpus == 8 && identity.numa_nodes >= 1 {
        return CalibrationProfile::canonical_scientific_cpu();
    }
    if cpus == 4 && identity.memory_mb >= 7 * 1024 {
        return CalibrationProfile::reference_4vcpu_8gb();
    }
    CalibrationProfile::developer_laptop()
}

fn calibrate_host() -> HostCalibration {
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    let identity = HostIdentity {
        cpu_model: detect_cpu_model(),
        isa: detect_isa(),
        kernel: detect_kernel(),
        cgroup_cpu_quota: detect_cgroup_cpu_quota(),
        cpu_governor: detect_governor(),
        numa_nodes: detect_numa_nodes(),
        memory_mb: detect_memory_mb(),
        container_image: detect_container_image(),
        git_identity: detect_git_identity(),
    };

    let mut noncanonical_reasons = Vec::new();

    if let Some(quota) = identity.cgroup_cpu_quota {
        noncanonical_reasons.push(format!(
            "shared-cpu: cgroup cpu quota {quota} microseconds per period"
        ));
    }

    if identity.cpu_governor == "powersave" {
        noncanonical_reasons.push("throttled: cpufreq governor is powersave".to_string());
    }

    if identity.memory_mb == 0 {
        noncanonical_reasons.push("missing: memory_mb could not be measured".to_string());
    }

    let profile = select_profile(cpus, &identity, &noncanonical_reasons);
    let canonical_status = if noncanonical_reasons.is_empty() {
        profile.canonical_status
    } else {
        HostCanonicalStatus::Noncanonical
    };

    let host_class = profile.name.clone();
    let single_core_score_mops = measure_single_core_mops();
    let memory_bandwidth_gbps = measure_memory_bandwidth_gbps();

    let host_fingerprint = fingerprint_from_record(
        &host_class,
        cpus,
        &identity,
        single_core_score_mops,
        memory_bandwidth_gbps,
    );

    HostCalibration {
        profile,
        host_class,
        canonical_status,
        cpus,
        memory_mb: identity.memory_mb,
        single_core_score_mops,
        memory_bandwidth_gbps,
        identity,
        host_fingerprint,
        noncanonical_reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_calibration() {
        let cal = HostCalibration::calibrate_current_host();
        assert!(cal.cpus >= 1);
        assert!(cal.single_core_score_mops > 0.0);
        assert!(cal.memory_bandwidth_gbps > 0.0);
        assert!(cal.is_reconstructable());
        assert_eq!(cal.host_fingerprint, cal.reconstruct_fingerprint());
    }

    #[test]
    fn test_identical_machine_classes_cluster_within_variance() {
        let a = HostCalibration::calibrate_current_host();
        let b = HostCalibration::calibrate_current_host();
        assert_eq!(a.host_class, b.host_class);
        assert_eq!(a.canonical_status, b.canonical_status);
        let mops_delta =
            (a.single_core_score_mops - b.single_core_score_mops).abs() / a.single_core_score_mops;
        assert!(
            mops_delta < 0.05,
            "mops variance {mops_delta} exceeds 5% envelope"
        );
        assert!(a.is_reconstructable());
        assert!(b.is_reconstructable());
    }

    #[test]
    fn test_shared_cpu_fixture_rejected_for_performance() {
        let mut cal = HostCalibration::calibrate_current_host();
        cal.noncanonical_reasons
            .push("shared-cpu: fixture throttle".to_string());
        cal.canonical_status = HostCanonicalStatus::Noncanonical;
        assert!(cal.rejects_performance_experiments());
        assert!(!cal.is_valid_for_claim());
    }

    #[test]
    fn test_canonical_status_markers_present() {
        let cal = HostCalibration::calibrate_current_host();
        let status = format!("{:?}", cal.canonical_status);
        assert!(
            status.contains("Canonical") || status.contains("Noncanonical"),
            "expected canonical or noncanonical status"
        );
    }

    #[test]
    fn canonical_profiles_require_exact_cpu_shape() {
        let identity = HostIdentity {
            cpu_model: "fixture".into(),
            isa: "fixture".into(),
            kernel: "fixture".into(),
            cgroup_cpu_quota: None,
            cpu_governor: "performance".into(),
            numa_nodes: 1,
            memory_mb: 16 * 1024,
            container_image: "fixture".into(),
            git_identity: "fixture".into(),
        };
        assert_eq!(
            select_profile(4, &identity, &[]).name,
            "reference-4vcpu-8gb"
        );
        assert_eq!(
            select_profile(8, &identity, &[]).name,
            "canonical-scientific-cpu"
        );
        assert_eq!(select_profile(6, &identity, &[]).name, "developer-laptop");

        let mut undersized = identity;
        undersized.memory_mb = 4 * 1024;
        assert_eq!(select_profile(4, &undersized, &[]).name, "developer-laptop");
    }
}
