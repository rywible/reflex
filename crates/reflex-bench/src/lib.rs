use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq)]
pub enum BenchmarkError {
    #[error("benchmark regression exceeded budget: {name} regressed by {percent:.2}%")]
    RegressionExceeded { name: String, percent: f64 },
    #[error("host calibration mismatch: expected {expected}, detected {detected}")]
    HostMismatch { expected: String, detected: String },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HostCalibration {
    pub host_class: String,
    pub cpus: usize,
    pub memory_mb: u64,
    pub single_core_score_mops: f64,
    pub memory_bandwidth_gbps: f64,
    pub host_fingerprint: Digest,
}

impl HostCalibration {
    pub fn calibrate_current_host() -> Self {
        let cpus = num_cpus();
        let start = std::time::Instant::now();
        let mut x = 1u64;
        for i in 0..10_000_000 {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(i);
        }
        let elapsed = start.elapsed().as_secs_f64();
        let mops = 10.0 / elapsed;

        let host_class = if cpus >= 4 {
            "performance-4x".to_string()
        } else {
            "local-dev".to_string()
        };

        let data = format!("{host_class}:{cpus}:{mops:.2}");
        let host_fingerprint = Digest::hash_blake3(data.as_bytes());

        Self {
            host_class,
            cpus,
            memory_mb: 8192,
            single_core_score_mops: mops,
            memory_bandwidth_gbps: 25.0,
            host_fingerprint,
        }
    }
}

fn num_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkRecord {
    pub name: String,
    pub p50_ns: f64,
    pub p95_ns: f64,
    pub throughput_units_per_sec: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerformanceBudgetRegistry {
    budgets: HashMap<String, f64>, // name -> max_p95_ns
}

impl PerformanceBudgetRegistry {
    pub fn new() -> Self {
        let mut budgets = HashMap::new();
        // Section 3.3 Initial quantitative gates
        budgets.insert("micro_mlp_2607_batch64".to_string(), 100_000.0); // p95 <= 100 µs
        budgets.insert("micro_mlp_2607_single".to_string(), 5_000.0); // p95 <= 5 µs
        budgets.insert("postgres_claim_p95".to_string(), 20_000_000.0); // p95 < 20 ms
        budgets.insert("cli_startup_p95".to_string(), 50_000_000.0); // p95 < 50 ms
        Self { budgets }
    }

    pub fn check_regression(
        &self,
        record: &BenchmarkRecord,
        max_regression_ratio: f64,
    ) -> Result<(), BenchmarkError> {
        if let Some(&budget_p95) = self.budgets.get(&record.name) {
            let limit = budget_p95 * (1.0 + max_regression_ratio);
            if record.p95_ns > limit {
                let percent = (record.p95_ns - budget_p95) / budget_p95 * 100.0;
                return Err(BenchmarkError::RegressionExceeded {
                    name: record.name.clone(),
                    percent,
                });
            }
        }
        Ok(())
    }
}

impl Default for PerformanceBudgetRegistry {
    fn default() -> Self {
        Self::new()
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
    }

    #[test]
    fn test_budget_regression_check() {
        let registry = PerformanceBudgetRegistry::new();
        let pass_record = BenchmarkRecord {
            name: "micro_mlp_2607_batch64".to_string(),
            p50_ns: 40_000.0,
            p95_ns: 90_000.0, // within 100 µs budget
            throughput_units_per_sec: 1_000_000.0,
        };
        assert!(registry.check_regression(&pass_record, 0.05).is_ok());

        let fail_record = BenchmarkRecord {
            name: "micro_mlp_2607_batch64".to_string(),
            p50_ns: 120_000.0,
            p95_ns: 150_000.0, // exceeds budget by 50%
            throughput_units_per_sec: 500_000.0,
        };
        assert!(registry.check_regression(&fail_record, 0.05).is_err());
    }
}
