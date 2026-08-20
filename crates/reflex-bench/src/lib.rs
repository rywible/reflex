//! Benchmark harness, host calibration, and performance budget registry (P1.1, P1.5, P1.6).
//!
//! Hosts are marked **canonical** or **noncanonical** via [`HostCanonicalStatus`].
//! [`HostCalibration`] records `host_class`, `host_fingerprint`, and reconstructable identity.
//! Regression gates: `test_budget_regression_check`, `test_host_calibration`.
//! Test-only allocation instrumentation is re-exported from `reflex-runtime` (P1.4).

mod benchmark;
mod budget;
mod calibration;
mod model;

pub use benchmark::{
    BenchmarkError, BenchmarkRecord, BenchmarkSummary, NoiseEnvelopeRegistry, RawBenchmarkSamples,
    compare_metric,
};
pub use budget::{
    BudgetDirection, BudgetEntry, BudgetMetric, BudgetRegistration, BudgetStatistic, BudgetUnit,
    MetricMeasurement, OpenBudgetEntry, PerformanceBudgetRegistry, RawMetricSamples,
};
pub use calibration::{CalibrationProfile, HostCalibration, HostCanonicalStatus, HostIdentity};
pub use model::{
    MODEL_BATCH_SIZES, ModelBackendRecommendation, ModelBackendSample, ModelBenchmarkReport,
    ModelNumericsContract, UnavailableBackend, run_model_backend_sweep,
};

/// Counting allocator for allocation budget tests (P1.4).
pub mod alloc {
    pub use reflex_runtime::alloc::*;
}

/// Copy counters for domain vs framework copies (P1.4).
pub mod copy {
    pub use reflex_runtime::copy::*;
}
