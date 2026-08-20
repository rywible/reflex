use crate::calibration::HostCalibration;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq)]
pub enum BenchmarkError {
    #[error("invalid benchmark evidence for {name}: {reason}")]
    InvalidEvidence { name: String, reason: String },
    #[error("benchmark regression exceeded budget: {name} regressed by {percent:.2}%")]
    RegressionExceeded { name: String, percent: f64 },
    #[error("host calibration mismatch: expected {expected}, detected {detected}")]
    HostMismatch { expected: String, detected: String },
    #[error(
        "benchmark comparison failed: {name} delta {percent:.2}% exceeds noise envelope {envelope:.2}%"
    )]
    NoiseEnvelopeExceeded {
        name: String,
        percent: f64,
        envelope: f64,
    },
    #[error("noncanonical host cannot be pooled with canonical baseline")]
    NoncanonicalHost,
}

/// Raw timing samples retained for regeneration (Criterion sample.json shape).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RawBenchmarkSamples {
    pub name: String,
    pub times_ns: Vec<f64>,
}

impl RawBenchmarkSamples {
    pub fn summary(&self) -> Result<BenchmarkSummary, BenchmarkError> {
        BenchmarkSummary::from_samples(&self.name, &self.times_ns)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkSummary {
    pub p50_ns: f64,
    pub p95_ns: f64,
    pub mean_ns: f64,
}

impl BenchmarkSummary {
    pub fn from_samples(name: &str, samples: &[f64]) -> Result<Self, BenchmarkError> {
        if samples.is_empty() {
            return Err(BenchmarkError::InvalidEvidence {
                name: name.to_string(),
                reason: "sample set is empty".to_string(),
            });
        }
        if samples
            .iter()
            .any(|sample| !sample.is_finite() || *sample <= 0.0)
        {
            return Err(BenchmarkError::InvalidEvidence {
                name: name.to_string(),
                reason: "timing samples must be finite and positive".to_string(),
            });
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(f64::total_cmp);
        let p = |q: f64| -> f64 {
            let idx = ((sorted.len() as f64 - 1.0) * q).round() as usize;
            sorted[idx.min(sorted.len() - 1)]
        };
        let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
        Ok(Self {
            p50_ns: p(0.50),
            p95_ns: p(0.95),
            mean_ns: mean,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkRecord {
    pub name: String,
    pub host_class: String,
    pub p50_ns: f64,
    pub p95_ns: f64,
    pub throughput_units_per_sec: f64,
    pub raw_samples: Option<RawBenchmarkSamples>,
}

impl BenchmarkRecord {
    pub fn validate(&self) -> Result<(), BenchmarkError> {
        if self.name.trim().is_empty()
            || self.host_class.trim().is_empty()
            || !self.p50_ns.is_finite()
            || !self.p95_ns.is_finite()
            || !self.throughput_units_per_sec.is_finite()
            || self.p50_ns <= 0.0
            || self.p95_ns <= 0.0
            || self.p50_ns > self.p95_ns
            || self.throughput_units_per_sec <= 0.0
        {
            return Err(BenchmarkError::InvalidEvidence {
                name: self.name.clone(),
                reason:
                    "names and finite positive p50/p95/throughput are required, with p50 <= p95"
                        .to_string(),
            });
        }
        if let Some(raw) = &self.raw_samples {
            if raw.name != self.name {
                return Err(BenchmarkError::InvalidEvidence {
                    name: self.name.clone(),
                    reason: "raw sample name does not match record".to_string(),
                });
            }
            let summary = raw.summary()?;
            if self.p50_ns.to_bits() != summary.p50_ns.to_bits()
                || self.p95_ns.to_bits() != summary.p95_ns.to_bits()
            {
                return Err(BenchmarkError::InvalidEvidence {
                    name: self.name.clone(),
                    reason: "recorded p50/p95 do not reconstruct from raw samples".to_string(),
                });
            }
        }
        Ok(())
    }
}

/// Registered noise envelope per benchmark (fraction, e.g. 0.05 = 5%).
#[derive(Clone, Debug, Default)]
pub struct NoiseEnvelopeRegistry {
    envelopes: std::collections::HashMap<String, f64>,
}

impl NoiseEnvelopeRegistry {
    pub fn new() -> Self {
        let mut envelopes = std::collections::HashMap::new();
        envelopes.insert("micro_mlp_2607_batch64".to_string(), 0.05);
        envelopes.insert("search_bitvec_uniform".to_string(), 0.05);
        envelopes.insert("scorer_p95".to_string(), 0.05);
        Self { envelopes }
    }

    pub fn envelope_for(&self, name: &str) -> Option<f64> {
        self.envelopes.get(name).copied()
    }
}

impl NoiseEnvelopeRegistry {
    /// Compare baseline vs candidate on the same host class within noise envelope.
    pub fn compare_runs(
        &self,
        baseline: &BenchmarkRecord,
        candidate: &BenchmarkRecord,
        host: &HostCalibration,
    ) -> Result<(), BenchmarkError> {
        baseline.validate()?;
        candidate.validate()?;
        if baseline.name != candidate.name {
            return Err(BenchmarkError::InvalidEvidence {
                name: candidate.name.clone(),
                reason: format!("does not match baseline {}", baseline.name),
            });
        }
        if baseline.host_class != candidate.host_class {
            return Err(BenchmarkError::HostMismatch {
                expected: baseline.host_class.clone(),
                detected: candidate.host_class.clone(),
            });
        }
        if host.host_class != baseline.host_class {
            return Err(BenchmarkError::HostMismatch {
                expected: baseline.host_class.clone(),
                detected: host.host_class.clone(),
            });
        }
        if host.canonical_status == crate::calibration::HostCanonicalStatus::Noncanonical {
            return Err(BenchmarkError::NoncanonicalHost);
        }
        let envelope =
            self.envelope_for(&baseline.name)
                .ok_or_else(|| BenchmarkError::InvalidEvidence {
                    name: baseline.name.clone(),
                    reason: "benchmark has no registered noise envelope".to_string(),
                })?;
        compare_metric(&baseline.name, baseline.p95_ns, candidate.p95_ns, envelope)
    }
}

pub fn compare_metric(
    name: &str,
    baseline_ns: f64,
    candidate_ns: f64,
    envelope: f64,
) -> Result<(), BenchmarkError> {
    if !baseline_ns.is_finite()
        || !candidate_ns.is_finite()
        || !envelope.is_finite()
        || baseline_ns <= 0.0
        || candidate_ns <= 0.0
        || envelope < 0.0
    {
        return Err(BenchmarkError::InvalidEvidence {
            name: name.to_string(),
            reason: "baseline, candidate, and envelope must be finite; timings must be positive and the envelope non-negative".to_string(),
        });
    }
    let delta = (candidate_ns - baseline_ns) / baseline_ns;
    if delta > envelope {
        return Err(BenchmarkError::NoiseEnvelopeExceeded {
            name: name.to_string(),
            percent: delta * 100.0,
            envelope: envelope * 100.0,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::calibration::HostCalibration;

    fn sample_record(p95: f64, host_class: &str) -> BenchmarkRecord {
        BenchmarkRecord {
            name: "micro_mlp_2607_batch64".to_string(),
            host_class: host_class.to_string(),
            p50_ns: p95 * 0.8,
            p95_ns: p95,
            throughput_units_per_sec: 1.0,
            raw_samples: None,
        }
    }

    #[test]
    fn test_two_idle_runs_agree_within_noise_envelope() {
        let registry = NoiseEnvelopeRegistry::new();
        let host = HostCalibration::calibrate_current_host();
        let baseline = sample_record(100_000.0, &host.host_class);
        let candidate = sample_record(102_000.0, &host.host_class); // +2%
        let result = registry.compare_runs(&baseline, &candidate, &host);
        if host.canonical_status == crate::calibration::HostCanonicalStatus::Canonical {
            assert!(result.is_ok());
        } else {
            assert_eq!(result, Err(BenchmarkError::NoncanonicalHost));
        }
    }

    #[test]
    fn test_synthetic_10_percent_slowdown_fails_comparison() {
        let registry = NoiseEnvelopeRegistry::new();
        let host = HostCalibration::calibrate_current_host();
        let baseline = sample_record(100_000.0, &host.host_class);
        let candidate = sample_record(110_000.0, &host.host_class); // +10%
        assert!(registry.compare_runs(&baseline, &candidate, &host).is_err());
    }

    #[test]
    fn test_raw_samples_regenerate_summary() {
        let samples = RawBenchmarkSamples {
            name: "fixture".to_string(),
            times_ns: (0..100).map(|i| 50_000.0 + (i as f64) * 100.0).collect(),
        };
        let summary = samples.summary().unwrap();
        let again = BenchmarkSummary::from_samples(&samples.name, &samples.times_ns).unwrap();
        assert_eq!(summary, again);
        assert!(summary.p50_ns > 0.0);
        assert!(summary.p95_ns >= summary.p50_ns);
    }

    #[test]
    fn malformed_evidence_never_passes() {
        assert!(compare_metric("x", 0.0, 1.0, 0.05).is_err());
        assert!(compare_metric("x", 1.0, f64::NAN, 0.05).is_err());
        assert!(
            RawBenchmarkSamples {
                name: "x".into(),
                times_ns: vec![1.0, f64::INFINITY],
            }
            .summary()
            .is_err()
        );
    }
}
