use crate::benchmark::{BenchmarkError, BenchmarkRecord};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetMetric {
    Latency,
    Throughput,
    CpuRatio,
    Utilization,
    ResidentBytes,
    ScratchAmplification,
    ArtifactBytes,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDirection {
    AtMost,
    LessThan,
    AtLeast,
    GreaterThan,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetUnit {
    Nanoseconds,
    UnitsPerSecond,
    BytesPerSecond,
    Ratio,
    Bytes,
}

/// The exact reduction used to reconstruct a reported value from raw data.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetStatistic {
    P95,
    P99,
    RateOfSums,
    RatioOfSums,
    Maximum,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BudgetEntry {
    pub name: String,
    pub host_class: String,
    pub metric: BudgetMetric,
    pub direction: BudgetDirection,
    pub unit: BudgetUnit,
    pub statistic: BudgetStatistic,
    pub threshold: f64,
    pub owner: String,
    pub authority: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpenBudgetEntry {
    pub name: String,
    pub owner: String,
    pub reason: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum BudgetRegistration {
    Budget(BudgetEntry),
    Open(OpenBudgetEntry),
}

/// Latency/size samples use `numerators`. Rates and ratios use paired vectors
/// and reconstruct as `sum(numerators) / sum(denominators)`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RawMetricSamples {
    pub numerators: Vec<f64>,
    #[serde(default)]
    pub denominators: Vec<f64>,
}

impl RawMetricSamples {
    pub fn reconstruct(
        &self,
        name: &str,
        statistic: BudgetStatistic,
    ) -> Result<f64, BenchmarkError> {
        if self.numerators.is_empty() || self.numerators.len() > 1_000_000 {
            return Err(invalid(
                name,
                "raw observation count must be in 1..=1,000,000",
            ));
        }
        if self
            .numerators
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err(invalid(
                name,
                "raw numerators must be finite and non-negative",
            ));
        }
        match statistic {
            BudgetStatistic::P95 | BudgetStatistic::P99 | BudgetStatistic::Maximum => {
                if !self.denominators.is_empty()
                    || self.numerators.iter().any(|value| *value <= 0.0)
                {
                    return Err(invalid(
                        name,
                        "latency/size samples must be positive and denominator-free",
                    ));
                }
                let mut sorted = self.numerators.clone();
                sorted.sort_by(f64::total_cmp);
                if statistic == BudgetStatistic::Maximum {
                    return Ok(*sorted.last().expect("non-empty samples"));
                }
                let quantile = if statistic == BudgetStatistic::P95 {
                    0.95
                } else {
                    0.99
                };
                let index = ((sorted.len() as f64 - 1.0) * quantile).round() as usize;
                Ok(sorted[index.min(sorted.len() - 1)])
            }
            BudgetStatistic::RateOfSums | BudgetStatistic::RatioOfSums => {
                if self.denominators.len() != self.numerators.len()
                    || self
                        .denominators
                        .iter()
                        .any(|value| !value.is_finite() || *value <= 0.0)
                {
                    return Err(invalid(
                        name,
                        "rates/ratios require one finite positive denominator per numerator",
                    ));
                }
                let numerator = self.numerators.iter().sum::<f64>();
                let denominator = self.denominators.iter().sum::<f64>();
                let value = numerator / denominator;
                if !numerator.is_finite() || !denominator.is_finite() || !value.is_finite() {
                    return Err(invalid(name, "raw observation sums overflowed"));
                }
                Ok(value)
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MetricMeasurement {
    pub name: String,
    pub host_class: String,
    pub metric: BudgetMetric,
    pub unit: BudgetUnit,
    pub statistic: BudgetStatistic,
    pub value: f64,
    pub raw_samples: RawMetricSamples,
}

impl MetricMeasurement {
    pub fn validate(&self) -> Result<(), BenchmarkError> {
        if self.name.trim().is_empty()
            || self.host_class.trim().is_empty()
            || !self.value.is_finite()
            || self.value < 0.0
        {
            return Err(invalid(
                &self.name,
                "measurement identity and finite non-negative value are required",
            ));
        }
        if self
            .raw_samples
            .reconstruct(&self.name, self.statistic)?
            .to_bits()
            != self.value.to_bits()
        {
            return Err(invalid(
                &self.name,
                "measurement does not reconstruct exactly from raw observations",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerformanceBudgetRegistry {
    registrations: HashMap<String, BudgetRegistration>,
}

impl PerformanceBudgetRegistry {
    pub fn new() -> Self {
        let reference = "reference-4vcpu-8gb";
        let section = "docs/reflex-framework-master-plan-rust.md §3.3";
        let mut registrations = HashMap::new();
        let budgets = [
            b(
                "candidate_metadata_per_core",
                reference,
                BudgetMetric::Throughput,
                BudgetDirection::AtLeast,
                BudgetUnit::UnitsPerSecond,
                BudgetStatistic::RateOfSums,
                5_000_000.0,
                "reflex-search",
                section,
            ),
            b(
                "dense_f32_feature_packing",
                reference,
                BudgetMetric::Throughput,
                BudgetDirection::AtLeast,
                BudgetUnit::BytesPerSecond,
                BudgetStatistic::RateOfSums,
                10.0 * 1024.0 * 1024.0 * 1024.0,
                "reflex-search",
                section,
            ),
            b(
                "uniform_scoring_per_core",
                reference,
                BudgetMetric::Throughput,
                BudgetDirection::AtLeast,
                BudgetUnit::UnitsPerSecond,
                BudgetStatistic::RateOfSums,
                10_000_000.0,
                "reflex-search",
                section,
            ),
            b(
                "search_policy_feature_overhead_ratio",
                reference,
                BudgetMetric::CpuRatio,
                BudgetDirection::AtMost,
                BudgetUnit::Ratio,
                BudgetStatistic::RatioOfSums,
                0.10,
                "reflex-search",
                section,
            ),
            b(
                "event_append_per_core",
                reference,
                BudgetMetric::Throughput,
                BudgetDirection::AtLeast,
                BudgetUnit::UnitsPerSecond,
                BudgetStatistic::RateOfSums,
                500_000.0,
                "reflex-ledger",
                section,
            ),
            b(
                "evidence_overhead_ratio",
                reference,
                BudgetMetric::CpuRatio,
                BudgetDirection::AtMost,
                BudgetUnit::Ratio,
                BudgetStatistic::RatioOfSums,
                0.03,
                "reflex-ledger",
                section,
            ),
            b(
                "ledger_recovery_1gib",
                reference,
                BudgetMetric::Throughput,
                BudgetDirection::AtLeast,
                BudgetUnit::BytesPerSecond,
                BudgetStatistic::RateOfSums,
                1024.0 * 1024.0 * 1024.0,
                "reflex-ledger",
                section,
            ),
            b(
                "dataset_compaction_rows_per_core",
                reference,
                BudgetMetric::Throughput,
                BudgetDirection::AtLeast,
                BudgetUnit::UnitsPerSecond,
                BudgetStatistic::RateOfSums,
                500_000.0,
                "reflex-dataset",
                section,
            ),
            b(
                "training_loader_cpu_ratio",
                reference,
                BudgetMetric::CpuRatio,
                BudgetDirection::AtMost,
                BudgetUnit::Ratio,
                BudgetStatistic::RatioOfSums,
                0.10,
                "reflex-training",
                section,
            ),
            b(
                "mlp_3k_epoch_1m",
                reference,
                BudgetMetric::Latency,
                BudgetDirection::AtMost,
                BudgetUnit::Nanoseconds,
                BudgetStatistic::Maximum,
                60_000_000_000.0,
                "reflex-training",
                section,
            ),
            b(
                "micro_mlp_2607_batch64",
                reference,
                BudgetMetric::Latency,
                BudgetDirection::AtMost,
                BudgetUnit::Nanoseconds,
                BudgetStatistic::P95,
                100_000.0,
                "reflex-ml-micro",
                section,
            ),
            b(
                "micro_mlp_2607_single",
                reference,
                BudgetMetric::Latency,
                BudgetDirection::AtMost,
                BudgetUnit::Nanoseconds,
                BudgetStatistic::P95,
                5_000.0,
                "reflex-ml-micro",
                section,
            ),
            b(
                "dataset_compile_rows_per_sec",
                reference,
                BudgetMetric::Throughput,
                BudgetDirection::AtLeast,
                BudgetUnit::UnitsPerSecond,
                BudgetStatistic::RateOfSums,
                500_000.0,
                "reflex-dataset",
                "master plan P8.4",
            ),
            b(
                "checkpoint_stream_peak_bytes",
                reference,
                BudgetMetric::ScratchAmplification,
                BudgetDirection::LessThan,
                BudgetUnit::Ratio,
                BudgetStatistic::RatioOfSums,
                1.25,
                "reflex-training",
                "master plan P8.2",
            ),
            b(
                "thread_permit_p95",
                reference,
                BudgetMetric::Latency,
                BudgetDirection::LessThan,
                BudgetUnit::Nanoseconds,
                BudgetStatistic::P95,
                2_000.0,
                "reflex-runtime",
                "master plan P1.2",
            ),
            b(
                "cli_startup_p95",
                reference,
                BudgetMetric::Latency,
                BudgetDirection::LessThan,
                BudgetUnit::Nanoseconds,
                BudgetStatistic::P95,
                50_000_000.0,
                "bins/reflex",
                section,
            ),
            b(
                "operator_api_read_p95",
                reference,
                BudgetMetric::Latency,
                BudgetDirection::LessThan,
                BudgetUnit::Nanoseconds,
                BudgetStatistic::P95,
                100_000_000.0,
                "reflex-api",
                "master plan P16.3",
            ),
            b(
                "observability_overhead_ratio",
                reference,
                BudgetMetric::CpuRatio,
                BudgetDirection::LessThan,
                BudgetUnit::Ratio,
                BudgetStatistic::RatioOfSums,
                0.02,
                "reflex-observability",
                "master plan P16.5",
            ),
            b(
                "deep_loom",
                "canonical-scientific-cpu",
                BudgetMetric::Latency,
                BudgetDirection::AtMost,
                BudgetUnit::Nanoseconds,
                BudgetStatistic::P95,
                23_701_000_000.0,
                "reflex-runtime",
                "reviewed deep-suite baseline",
            ),
            b(
                "deep_turmoil",
                "canonical-scientific-cpu",
                BudgetMetric::Latency,
                BudgetDirection::AtMost,
                BudgetUnit::Nanoseconds,
                BudgetStatistic::P95,
                25_629_000_000.0,
                "reflex-scheduler",
                "reviewed deep-suite baseline",
            ),
            b(
                "deep_fuzz_smoke",
                "canonical-scientific-cpu",
                BudgetMetric::Latency,
                BudgetDirection::AtMost,
                BudgetUnit::Nanoseconds,
                BudgetStatistic::P95,
                14_802_000_000.0,
                "reflex-fuzz",
                "reviewed deep-suite baseline",
            ),
            b(
                "deep_recovery",
                "canonical-scientific-cpu",
                BudgetMetric::Latency,
                BudgetDirection::AtMost,
                BudgetUnit::Nanoseconds,
                BudgetStatistic::P95,
                123_916_000_000.0,
                "tests/recovery",
                "reviewed deep-suite baseline",
            ),
        ];
        for entry in budgets {
            registrations.insert(entry.name.clone(), BudgetRegistration::Budget(entry));
        }
        let open_entries = [
            o(
                "canonical_digest_1k_p95",
                "reflex-types",
                "plan specifies encoding throughput, not 1 KiB digest p95",
            ),
            o(
                "ledger_append_p99",
                "reflex-ledger",
                "plan specifies append throughput/overhead, not append p99",
            ),
            o(
                "cas_ingest_mib_per_sec",
                "reflex-cas",
                "plan specifies throughput relative to direct file write, not an absolute rate",
            ),
            o(
                "cas_fsync_p99",
                "reflex-cas",
                "plan specifies 1 MiB put p95, not fsync p99",
            ),
            o(
                "protocol_expand_batch_p99",
                "reflex-domain-protocol",
                "plan specifies 64-state round-trip p95, not p99",
            ),
            o(
                "cell_launch_p95",
                "reflex-runtime",
                "plan sets no cell-launch latency threshold",
            ),
            o(
                "cell_cleanup_p95",
                "reflex-runtime",
                "plan requires zero owned resources, not cleanup latency",
            ),
            o(
                "search_cpu_per_node_p95",
                "reflex-search",
                "plan requires accounting but sets no absolute CPU-per-node threshold",
            ),
            o(
                "frontier_ordering_p95",
                "reflex-search",
                "plan requires deterministic ordering but sets no latency threshold",
            ),
            o(
                "burn_training_epoch_p95",
                "reflex-ml-burn",
                "plan sets the specific 3K/1M epoch gate, not generic Burn p95",
            ),
            o(
                "evaluation_groups_per_sec",
                "reflex-eval",
                "plan sets candidate throughput, not groups per second",
            ),
            o(
                "report_rebuild_p95",
                "reflex-report",
                "plan sets no report rebuild threshold",
            ),
            o(
                "cli_status_100k_p95",
                "bins/reflex",
                "plan sets CLI startup p95, not 100K-cell status p95",
            ),
            o(
                "acceptance_suite_resource_accounting",
                "xtask",
                "plan requires complete reporting but sets no scalar threshold",
            ),
            o(
                "scorer_p95",
                "reflex-search",
                "legacy name has no independent master-plan latency threshold",
            ),
            o(
                "coordinator_overhead_ratio",
                "reflex-scheduler",
                "the local coordinator overhead threshold is not yet specified",
            ),
        ];
        for entry in open_entries {
            registrations.insert(entry.name.clone(), BudgetRegistration::Open(entry));
        }
        Self { registrations }
    }

    pub fn get(&self, name: &str) -> Option<&BudgetEntry> {
        match self.registrations.get(name) {
            Some(BudgetRegistration::Budget(entry)) => Some(entry),
            _ => None,
        }
    }

    pub fn registration(&self, name: &str) -> Option<&BudgetRegistration> {
        self.registrations.get(name)
    }

    pub fn check_measurement(&self, measurement: &MetricMeasurement) -> Result<(), BenchmarkError> {
        measurement.validate()?;
        let entry = self.required_budget(&measurement.name)?;
        if measurement.host_class != entry.host_class {
            return Err(BenchmarkError::HostMismatch {
                expected: entry.host_class.clone(),
                detected: measurement.host_class.clone(),
            });
        }
        if (measurement.metric, measurement.unit, measurement.statistic)
            != (entry.metric, entry.unit, entry.statistic)
        {
            return Err(invalid(
                &measurement.name,
                "metric, unit, or statistic does not match budget",
            ));
        }
        check_threshold(entry, measurement.value, 0.0)
    }

    pub fn check_regression(
        &self,
        record: &BenchmarkRecord,
        ratio: f64,
    ) -> Result<(), BenchmarkError> {
        record.validate()?;
        validate_ratio(&record.name, ratio)?;
        let entry = self.required_budget(&record.name)?;
        if record.host_class != entry.host_class {
            return Err(BenchmarkError::HostMismatch {
                expected: entry.host_class.clone(),
                detected: record.host_class.clone(),
            });
        }
        check_threshold(entry, record_value(entry, record)?, ratio)
    }

    pub fn check_against_baseline(
        &self,
        baseline: &BenchmarkRecord,
        candidate: &BenchmarkRecord,
        ratio: f64,
    ) -> Result<(), BenchmarkError> {
        baseline.validate()?;
        candidate.validate()?;
        validate_ratio(&candidate.name, ratio)?;
        let entry = self.required_budget(&candidate.name)?;
        if baseline.name != candidate.name
            || baseline.host_class != candidate.host_class
            || candidate.host_class != entry.host_class
        {
            return Err(BenchmarkError::HostMismatch {
                expected: format!("{}@{}", baseline.name, baseline.host_class),
                detected: format!("{}@{}", candidate.name, candidate.host_class),
            });
        }
        let old = record_value(entry, baseline)?;
        let new = record_value(entry, candidate)?;
        let regression = match entry.direction {
            BudgetDirection::AtMost | BudgetDirection::LessThan => (new - old) / old,
            BudgetDirection::AtLeast | BudgetDirection::GreaterThan => (old - new) / old,
        };
        if regression > ratio {
            return Err(BenchmarkError::RegressionExceeded {
                name: candidate.name.clone(),
                percent: regression * 100.0,
            });
        }
        Ok(())
    }

    fn required_budget(&self, name: &str) -> Result<&BudgetEntry, BenchmarkError> {
        match self.registrations.get(name) {
            Some(BudgetRegistration::Budget(entry)) => Ok(entry),
            Some(BudgetRegistration::Open(entry)) => Err(invalid(name, &entry.reason)),
            None => Err(invalid(name, "benchmark is not registered")),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn b(
    name: &str,
    host: &str,
    metric: BudgetMetric,
    direction: BudgetDirection,
    unit: BudgetUnit,
    statistic: BudgetStatistic,
    threshold: f64,
    owner: &str,
    authority: &str,
) -> BudgetEntry {
    BudgetEntry {
        name: name.into(),
        host_class: host.into(),
        metric,
        direction,
        unit,
        statistic,
        threshold,
        owner: owner.into(),
        authority: authority.into(),
    }
}
fn o(name: &str, owner: &str, reason: &str) -> OpenBudgetEntry {
    OpenBudgetEntry {
        name: name.into(),
        owner: owner.into(),
        reason: reason.into(),
    }
}
fn invalid(name: &str, reason: &str) -> BenchmarkError {
    BenchmarkError::InvalidEvidence {
        name: name.into(),
        reason: reason.into(),
    }
}

fn record_value(entry: &BudgetEntry, record: &BenchmarkRecord) -> Result<f64, BenchmarkError> {
    match (entry.metric, entry.unit, entry.statistic) {
        (BudgetMetric::Latency, BudgetUnit::Nanoseconds, BudgetStatistic::P95) => Ok(record.p95_ns),
        (
            BudgetMetric::Throughput,
            BudgetUnit::UnitsPerSecond | BudgetUnit::BytesPerSecond,
            BudgetStatistic::RateOfSums,
        ) => Ok(record.throughput_units_per_sec),
        _ => Err(invalid(
            &record.name,
            "BenchmarkRecord cannot represent this metric; use MetricMeasurement",
        )),
    }
}

fn check_threshold(entry: &BudgetEntry, value: f64, ratio: f64) -> Result<(), BenchmarkError> {
    if !value.is_finite() || value < 0.0 || !entry.threshold.is_finite() || entry.threshold <= 0.0 {
        return Err(invalid(
            &entry.name,
            "value and threshold must be finite and threshold positive",
        ));
    }
    let limit = match entry.direction {
        BudgetDirection::AtMost | BudgetDirection::LessThan => entry.threshold * (1.0 + ratio),
        BudgetDirection::AtLeast | BudgetDirection::GreaterThan => entry.threshold * (1.0 - ratio),
    };
    let pass = match entry.direction {
        BudgetDirection::AtMost => value <= limit,
        BudgetDirection::LessThan => value < limit,
        BudgetDirection::AtLeast => value >= limit,
        BudgetDirection::GreaterThan => value > limit,
    };
    if pass {
        return Ok(());
    }
    let percent = match entry.direction {
        BudgetDirection::AtMost | BudgetDirection::LessThan => {
            (value - entry.threshold) / entry.threshold * 100.0
        }
        BudgetDirection::AtLeast | BudgetDirection::GreaterThan => {
            (entry.threshold - value) / entry.threshold * 100.0
        }
    };
    Err(BenchmarkError::RegressionExceeded {
        name: entry.name.clone(),
        percent,
    })
}

fn validate_ratio(name: &str, ratio: f64) -> Result<(), BenchmarkError> {
    if ratio.is_finite() && (0.0..1.0).contains(&ratio) {
        Ok(())
    } else {
        Err(invalid(
            name,
            "regression ratio must be finite and in [0, 1)",
        ))
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
    fn record(name: &str, host: &str, p95: f64) -> BenchmarkRecord {
        BenchmarkRecord {
            name: name.into(),
            host_class: host.into(),
            p50_ns: p95 * 0.8,
            p95_ns: p95,
            throughput_units_per_sec: 1.0,
            raw_samples: None,
        }
    }

    #[test]
    fn latency_and_baseline_regression_work() {
        let registry = PerformanceBudgetRegistry::new();
        let base = record("micro_mlp_2607_batch64", "reference-4vcpu-8gb", 90_000.0);
        assert!(registry.check_regression(&base, 0.05).is_ok());
        assert!(
            registry
                .check_against_baseline(
                    &base,
                    &record("micro_mlp_2607_batch64", "reference-4vcpu-8gb", 95_000.0),
                    0.05
                )
                .is_err()
        );
    }

    #[test]
    fn typed_rate_and_ratio_reconstruct() {
        let registry = PerformanceBudgetRegistry::new();
        let rate = MetricMeasurement {
            name: "candidate_metadata_per_core".into(),
            host_class: "reference-4vcpu-8gb".into(),
            metric: BudgetMetric::Throughput,
            unit: BudgetUnit::UnitsPerSecond,
            statistic: BudgetStatistic::RateOfSums,
            value: 5_000_000.0,
            raw_samples: RawMetricSamples {
                numerators: vec![10_000_000.0],
                denominators: vec![2.0],
            },
        };
        assert!(registry.check_measurement(&rate).is_ok());
        let ratio = MetricMeasurement {
            name: "observability_overhead_ratio".into(),
            host_class: "reference-4vcpu-8gb".into(),
            metric: BudgetMetric::CpuRatio,
            unit: BudgetUnit::Ratio,
            statistic: BudgetStatistic::RatioOfSums,
            value: 0.02,
            raw_samples: RawMetricSamples {
                numerators: vec![2.0],
                denominators: vec![100.0],
            },
        };
        assert!(registry.check_measurement(&ratio).is_err());
    }

    #[test]
    fn malformed_and_open_fail_closed() {
        let registry = PerformanceBudgetRegistry::new();
        assert!(registry.get("report_rebuild_p95").is_none());
        assert!(matches!(
            registry.registration("report_rebuild_p95"),
            Some(BudgetRegistration::Open(_))
        ));
        assert!(
            registry
                .check_regression(&record("unknown", "reference-4vcpu-8gb", 1.0), 0.05)
                .is_err()
        );
        assert!(
            registry
                .check_regression(&record("micro_mlp_2607_batch64", "local-dev", 1.0), 0.05)
                .is_err()
        );
    }
}
