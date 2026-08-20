use crate::{BenchmarkError, HostCalibration};
use reflex_domain::FeatureBatch;
use reflex_ml_burn::{BackendAvailability, backend_availability, verify_micro_flex_conversion};
use reflex_ml_micro::MicroMlp;
use reflex_types::{Digest, FeatureSchemaId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

pub const MODEL_BATCH_SIZES: [usize; 6] = [1, 8, 32, 64, 128, 512];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelNumericsContract {
    pub dtype: String,
    pub activation: String,
    pub output_dim: usize,
    pub abs_tolerance_bits: u32,
}

impl ModelNumericsContract {
    pub fn canonical_f32_relu() -> Self {
        Self {
            dtype: "f32".to_string(),
            activation: "relu".to_string(),
            output_dim: 1,
            abs_tolerance_bits: 1e-5f32.to_bits(),
        }
    }

    pub fn digest(&self) -> Digest {
        let mut payload = Vec::new();
        payload.extend_from_slice(b"reflex.model.numerics.v1\0");
        payload.extend_from_slice(self.dtype.as_bytes());
        payload.push(0);
        payload.extend_from_slice(self.activation.as_bytes());
        payload.extend_from_slice(&(self.output_dim as u64).to_le_bytes());
        payload.extend_from_slice(&self.abs_tolerance_bits.to_le_bytes());
        Digest::hash_blake3(&payload)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelBackendSample {
    pub backend: String,
    pub profile: String,
    pub input_dim: usize,
    pub batch_size: usize,
    pub thread_budget: usize,
    pub numerics_digest: Digest,
    pub forward_times_ns: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnavailableBackend {
    pub backend: String,
    pub reason: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ModelBackendRecommendation {
    pub backend: String,
    pub total_p95_ns: f64,
    pub compared_backends: Vec<String>,
    pub micro_ownership_margin: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ModelBenchmarkReport {
    pub schema: String,
    pub host: HostCalibration,
    pub numerics: ModelNumericsContract,
    pub thread_budget: usize,
    pub samples: Vec<ModelBackendSample>,
    pub unavailable: Vec<UnavailableBackend>,
    pub recommendation: ModelBackendRecommendation,
    pub evidence_digest: Digest,
}

impl ModelBenchmarkReport {
    pub fn validate(&self) -> Result<(), BenchmarkError> {
        if self.schema != "reflex.model-backend-benchmark.v1" {
            return Err(BenchmarkError::InvalidEvidence {
                name: "model-backend-sweep".to_string(),
                reason: "unsupported model benchmark evidence schema".to_string(),
            });
        }
        let recommendation = self.recompute_recommendation()?;
        if recommendation != self.recommendation {
            return Err(BenchmarkError::InvalidEvidence {
                name: "model-backend-sweep".to_string(),
                reason: "stored recommendation does not match raw samples".to_string(),
            });
        }
        if self.recompute_evidence_digest()? != self.evidence_digest {
            return Err(BenchmarkError::InvalidEvidence {
                name: "model-backend-sweep".to_string(),
                reason: "model benchmark evidence identity mismatch".to_string(),
            });
        }
        Ok(())
    }

    pub fn recompute_recommendation(&self) -> Result<ModelBackendRecommendation, BenchmarkError> {
        validate_and_recommend(
            &self.host,
            &self.numerics,
            self.thread_budget,
            &self.samples,
        )
    }

    pub fn recompute_evidence_digest(&self) -> Result<Digest, BenchmarkError> {
        evidence_digest(
            &self.host,
            &self.numerics,
            self.thread_budget,
            &self.samples,
            &self.unavailable,
        )
    }
}

pub fn run_model_backend_sweep(
    iterations: usize,
    thread_budget: usize,
) -> Result<ModelBenchmarkReport, BenchmarkError> {
    if iterations == 0 || thread_budget != 1 {
        return Err(BenchmarkError::InvalidEvidence {
            name: "model-backend-sweep".to_string(),
            reason: if iterations == 0 {
                "iteration count must be positive".to_string()
            } else {
                "current micro/Flex adapters are single-threaded; refusing a mismatched thread budget"
                    .to_string()
            },
        });
    }
    let host = HostCalibration::calibrate_current_host();
    let numerics = ModelNumericsContract::canonical_f32_relu();
    let numerics_digest = numerics.digest();
    let mut samples = Vec::new();
    // M1.5's registered 2,607-parameter layout and M2A's actual 8-column
    // feature schema are both measured.
    for (profile, input_dim, hidden) in [
        ("m15-2607", 64usize, vec![22usize, 49usize]),
        ("m2a-feature-8", 8usize, vec![22usize, 49usize]),
    ] {
        let micro = MicroMlp::random_with_hidden(input_dim, hidden, 0x5246_5837);
        for &batch_size in &MODEL_BATCH_SIZES {
            let features = deterministic_features(profile, input_dim, batch_size);
            let burn = verify_micro_flex_conversion(
                &micro,
                &features,
                f32::from_bits(numerics.abs_tolerance_bits),
            )
            .map_err(|error| BenchmarkError::InvalidEvidence {
                name: "micro-burn-conversion".to_string(),
                reason: error.to_string(),
            })?;
            let mut micro_output = vec![0.0f32; batch_size];
            let mut micro_scratch = vec![
                0.0f32;
                micro.scratch_len(batch_size).map_err(|error| {
                    BenchmarkError::InvalidEvidence {
                        name: "micro-scratch".to_string(),
                        reason: error.to_string(),
                    }
                })?
            ];
            let mut burn_output = vec![0.0f32; batch_size];
            // Warm both adapters before retaining samples.
            micro
                .try_score_rows(
                    &features.values,
                    batch_size,
                    &mut micro_output,
                    &mut micro_scratch,
                )
                .map_err(model_error("micro-warmup"))?;
            burn.score_batch(&features, &mut burn_output)
                .map_err(model_error("burn-flex-warmup"))?;

            let mut micro_times = Vec::with_capacity(iterations);
            let mut burn_times = Vec::with_capacity(iterations);
            for _ in 0..iterations {
                let start = Instant::now();
                micro
                    .try_score_rows(
                        &features.values,
                        batch_size,
                        &mut micro_output,
                        &mut micro_scratch,
                    )
                    .map_err(model_error("micro-forward"))?;
                micro_times.push(start.elapsed().as_nanos() as f64);

                let start = Instant::now();
                burn.score_batch(&features, &mut burn_output)
                    .map_err(model_error("burn-flex-forward"))?;
                burn_times.push(start.elapsed().as_nanos() as f64);
            }
            samples.push(ModelBackendSample {
                backend: "micro".to_string(),
                profile: profile.to_string(),
                input_dim,
                batch_size,
                thread_budget,
                numerics_digest,
                forward_times_ns: micro_times,
            });
            samples.push(ModelBackendSample {
                backend: "burn-flex".to_string(),
                profile: profile.to_string(),
                input_dim,
                batch_size,
                thread_budget,
                numerics_digest,
                forward_times_ns: burn_times,
            });
        }
    }
    let unavailable = backend_availability()
        .into_iter()
        .filter_map(|(backend, status)| match status {
            BackendAvailability::Available => None,
            BackendAvailability::Unavailable { reason } => Some(UnavailableBackend {
                backend: backend.to_string(),
                reason,
            }),
        })
        .collect::<Vec<_>>();
    let recommendation = validate_and_recommend(&host, &numerics, thread_budget, &samples)?;
    let evidence_digest = evidence_digest(&host, &numerics, thread_budget, &samples, &unavailable)?;
    Ok(ModelBenchmarkReport {
        schema: "reflex.model-backend-benchmark.v1".to_string(),
        host,
        numerics,
        thread_budget,
        samples,
        unavailable,
        recommendation,
        evidence_digest,
    })
}

fn model_error(name: &'static str) -> impl FnOnce(reflex_ml_core::MlError) -> BenchmarkError {
    move |error| BenchmarkError::InvalidEvidence {
        name: name.to_string(),
        reason: error.to_string(),
    }
}

fn deterministic_features(profile: &str, input_dim: usize, rows: usize) -> FeatureBatch {
    let schema = FeatureSchemaId::from_digest(Digest::hash_blake3(profile.as_bytes()));
    let mut features = FeatureBatch::new(rows, input_dim, schema);
    for (index, value) in features.values.iter_mut().enumerate() {
        *value = ((index as f32 + 1.0) * 0.017).sin();
    }
    features
}

fn p95(samples: &[f64]) -> Result<f64, BenchmarkError> {
    if samples.is_empty()
        || samples
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return Err(BenchmarkError::InvalidEvidence {
            name: "model-backend-sweep".to_string(),
            reason: "raw timing samples must be finite, positive, and non-empty".to_string(),
        });
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    Ok(sorted[((sorted.len() - 1) as f64 * 0.95).round() as usize])
}

fn validate_and_recommend(
    host: &HostCalibration,
    numerics: &ModelNumericsContract,
    thread_budget: usize,
    samples: &[ModelBackendSample],
) -> Result<ModelBackendRecommendation, BenchmarkError> {
    if !host.is_reconstructable() || thread_budget == 0 || samples.is_empty() {
        return Err(BenchmarkError::InvalidEvidence {
            name: "model-backend-sweep".to_string(),
            reason: "reconstructable host, positive thread budget, and samples are required"
                .to_string(),
        });
    }
    let expected_numerics = numerics.digest();
    let expected_batches = BTreeSet::from(MODEL_BATCH_SIZES);
    let expected_profiles = BTreeMap::from([
        ("m15-2607".to_string(), 64usize),
        ("m2a-feature-8".to_string(), 8usize),
    ]);
    let expected_coverage = BTreeSet::from([
        ("micro".to_string(), "m15-2607".to_string()),
        ("micro".to_string(), "m2a-feature-8".to_string()),
        ("burn-flex".to_string(), "m15-2607".to_string()),
        ("burn-flex".to_string(), "m2a-feature-8".to_string()),
    ]);
    let mut coverage = BTreeMap::<(String, String), BTreeSet<usize>>::new();
    let mut totals = BTreeMap::<String, f64>::new();
    let mut expected_iterations = None;
    for sample in samples {
        if sample.thread_budget != thread_budget || sample.numerics_digest != expected_numerics {
            return Err(BenchmarkError::InvalidEvidence {
                name: "model-backend-sweep".to_string(),
                reason: "refusing mismatched numerics or thread budgets".to_string(),
            });
        }
        if expected_profiles.get(&sample.profile) != Some(&sample.input_dim)
            || !expected_coverage.contains(&(sample.backend.clone(), sample.profile.clone()))
        {
            return Err(BenchmarkError::InvalidEvidence {
                name: "model-backend-sweep".to_string(),
                reason: "sample backend/profile or actual feature dimension is not registered"
                    .to_string(),
            });
        }
        match expected_iterations {
            Some(iterations) if iterations != sample.forward_times_ns.len() => {
                return Err(BenchmarkError::InvalidEvidence {
                    name: "model-backend-sweep".to_string(),
                    reason: "raw sample counts differ across benchmark cells".to_string(),
                });
            }
            None => expected_iterations = Some(sample.forward_times_ns.len()),
            Some(_) => {}
        }
        let inserted = coverage
            .entry((sample.backend.clone(), sample.profile.clone()))
            .or_default()
            .insert(sample.batch_size);
        if !inserted {
            return Err(BenchmarkError::InvalidEvidence {
                name: "model-backend-sweep".to_string(),
                reason: "duplicate backend/profile/batch sample".to_string(),
            });
        }
        *totals.entry(sample.backend.clone()).or_default() += p95(&sample.forward_times_ns)?;
    }
    if coverage.keys().cloned().collect::<BTreeSet<_>>() != expected_coverage
        || coverage
            .values()
            .any(|batches| batches != &expected_batches)
    {
        return Err(BenchmarkError::InvalidEvidence {
            name: "model-backend-sweep".to_string(),
            reason: "every backend/profile must contain batches 1,8,32,64,128,512".to_string(),
        });
    }
    let micro = *totals
        .get("micro")
        .ok_or_else(|| BenchmarkError::InvalidEvidence {
            name: "model-backend-sweep".to_string(),
            reason: "micro evidence is missing".to_string(),
        })?;
    let flex = *totals
        .get("burn-flex")
        .ok_or_else(|| BenchmarkError::InvalidEvidence {
            name: "model-backend-sweep".to_string(),
            reason: "Burn/Flex evidence is missing".to_string(),
        })?;
    let margin = 0.10;
    let (backend, total) = if micro <= flex * (1.0 - margin) {
        ("micro", micro)
    } else {
        ("burn-flex", flex)
    };
    Ok(ModelBackendRecommendation {
        backend: backend.to_string(),
        total_p95_ns: total,
        compared_backends: totals.keys().cloned().collect(),
        micro_ownership_margin: margin,
    })
}

fn evidence_digest(
    host: &HostCalibration,
    numerics: &ModelNumericsContract,
    thread_budget: usize,
    samples: &[ModelBackendSample],
    unavailable: &[UnavailableBackend],
) -> Result<Digest, BenchmarkError> {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"reflex.model-backend-evidence.v1\0");
    payload.extend_from_slice(host.host_fingerprint.as_bytes());
    payload.extend_from_slice(numerics.digest().as_bytes());
    payload.extend_from_slice(&(thread_budget as u64).to_le_bytes());
    for sample in samples {
        payload.extend_from_slice(sample.backend.as_bytes());
        payload.push(0);
        payload.extend_from_slice(sample.profile.as_bytes());
        payload.extend_from_slice(&(sample.input_dim as u64).to_le_bytes());
        payload.extend_from_slice(&(sample.batch_size as u64).to_le_bytes());
        payload.extend_from_slice(&(sample.thread_budget as u64).to_le_bytes());
        payload.extend_from_slice(sample.numerics_digest.as_bytes());
        payload.extend_from_slice(&(sample.forward_times_ns.len() as u64).to_le_bytes());
        for value in &sample.forward_times_ns {
            if !value.is_finite() || *value <= 0.0 {
                return Err(BenchmarkError::InvalidEvidence {
                    name: "model-backend-sweep".to_string(),
                    reason: "invalid raw sample in evidence identity".to_string(),
                });
            }
            payload.extend_from_slice(&value.to_bits().to_le_bytes());
        }
    }
    for backend in unavailable {
        payload.extend_from_slice(backend.backend.as_bytes());
        payload.push(0);
        payload.extend_from_slice(backend.reason.as_bytes());
    }
    Ok(Digest::hash_blake3(&payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_mismatched_numerics_and_threads() {
        let report = run_model_backend_sweep(2, 1).unwrap();
        let mut bad = report.samples.clone();
        bad[0].thread_budget = 2;
        assert!(validate_and_recommend(&report.host, &report.numerics, 1, &bad).is_err());
        bad[0].thread_budget = 1;
        bad[0].numerics_digest = Digest::hash_blake3(b"different");
        assert!(validate_and_recommend(&report.host, &report.numerics, 1, &bad).is_err());

        let mut incomplete = report.samples.clone();
        incomplete.retain(|sample| sample.profile != "m2a-feature-8");
        assert!(validate_and_recommend(&report.host, &report.numerics, 1, &incomplete).is_err());
    }

    #[test]
    fn raw_samples_regenerate_identity_and_recommendation() {
        let report = run_model_backend_sweep(2, 1).unwrap();
        report.validate().unwrap();
        assert_eq!(
            report.recompute_evidence_digest().unwrap(),
            report.evidence_digest
        );
        assert_eq!(
            report.recompute_recommendation().unwrap().backend,
            report.recommendation.backend
        );
        assert!(
            report
                .unavailable
                .iter()
                .any(|backend| backend.backend == "burn-cubecl-cpu")
        );
        let mut tampered = report.clone();
        tampered.recommendation.backend = if report.recommendation.backend == "micro" {
            "burn-flex".to_string()
        } else {
            "micro".to_string()
        };
        assert!(tampered.validate().is_err());
    }
}
