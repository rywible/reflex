//! Wrela command surface — export, check-candidate, cost-candidate, conformance.
//!
//! In-process reference implementation; does not mutate source or catalog by default.

use crate::certificate::CertificateStrategy;
use crate::interval::Interval;
use crate::kernel_package::{
    KernelPackage, ValidateOutcome, export_package_json, validate_package,
};
use crate::verify::{CertificatePayload, verify_certificate};
use reflex_types::Digest;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReceipt {
    pub command: String,
    pub input_digest: Digest,
    pub output_digest: Digest,
    pub success: bool,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CostReport {
    pub baseline_cycles: u64,
    pub candidate_cycles: u64,
    pub proxy_cycles_saved: i64,
    pub feature_overhead_ns: u64,
    pub inference_overhead_ns: u64,
    pub compiler_overhead_ns: u64,
    pub net_cycles: i64,
    pub labelled_measured: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConformanceReport {
    pub scalar_pass: bool,
    pub packet_pass: bool,
    pub instruction_obligations_pass: bool,
    pub divergence: Option<String>,
}

/// `wrela reflex-export` — seal package to JSON without mutating source.
pub fn reflex_export(pkg: &KernelPackage) -> Result<(String, CommandReceipt), String> {
    let input_digest = pkg.identity().map_err(|e| e.to_string())?;
    let json = export_package_json(pkg).map_err(|e| e.to_string())?;
    let output_digest = Digest::hash_blake3(json.as_bytes());
    Ok((
        json,
        CommandReceipt {
            command: "reflex-export".to_string(),
            input_digest,
            output_digest,
            success: true,
            message: "exported without mutation".to_string(),
        },
    ))
}

/// `wrela reflex-check-candidate` — rebuild and verify candidate semantics.
pub fn reflex_check_candidate(
    pkg: &KernelPackage,
    candidate_strategy: CertificateStrategy,
    claimed_low: f64,
    claimed_high: f64,
) -> Result<CommandReceipt, String> {
    let input_digest = pkg.identity().map_err(|e| e.to_string())?;
    let range = pkg
        .input_ranges
        .first()
        .ok_or_else(|| "missing input range".to_string())?;
    let input_low = range.start as f64 * 0.1;
    let input_high = range.end as f64 * 0.1;

    let payload = CertificatePayload {
        kernel_id: pkg.kernel_id.clone(),
        package_digest: input_digest,
        strategy: candidate_strategy,
        input_low,
        input_high,
        claimed_bounds: Interval::new(claimed_low, claimed_high),
        proxy_cycles_saved: 0,
    };

    match verify_certificate(pkg, &payload) {
        Ok(v) => {
            let output_digest = v.certificate_digest;
            Ok(CommandReceipt {
                command: "reflex-check-candidate".to_string(),
                input_digest,
                output_digest,
                success: true,
                message: "candidate check passed".to_string(),
            })
        }
        Err(e) => Err(e.to_string()),
    }
}

/// `wrela reflex-cost-candidate` — measured cycles with overhead breakdown.
pub fn reflex_cost_candidate(
    pkg: &KernelPackage,
    candidate_strategy: CertificateStrategy,
) -> Result<(CostReport, CommandReceipt), String> {
    let _ = (pkg, candidate_strategy);
    Err(
        "CostEvidenceUnavailable: an external Wrela cost worker and measured overhead inputs are required"
            .to_string(),
    )
}

/// `wrela reflex-conformance` — scalar/packet differential fixtures.
pub fn reflex_conformance(
    pkg: &KernelPackage,
) -> Result<(ConformanceReport, CommandReceipt), String> {
    match validate_package(pkg) {
        ValidateOutcome::Valid(_) => {}
        ValidateOutcome::Rejected(d) => {
            return Err(format!("package validation failed: {d:?}"));
        }
    }
    Err(
        "ConformanceUnavailable: external Rust/Wrela scalar, packet, and instruction checks are required"
            .to_string(),
    )
}

/// Batch check up to 100 candidates in one invocation.
pub fn reflex_check_candidate_batch(
    pkg: &KernelPackage,
    candidates: &[(CertificateStrategy, f64, f64)],
) -> Result<Vec<CommandReceipt>, String> {
    if candidates.is_empty() || candidates.len() > 100 {
        return Err("candidate batch size must be in 1..=100".into());
    }
    candidates
        .iter()
        .map(|(strategy, low, high)| reflex_check_candidate(pkg, *strategy, *low, *high))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certificate::evaluate_kernel_bounds;
    use crate::kernel_package::{KernelPackage, import_package_json};

    #[test]
    fn test_export_deterministic() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let (j1, r1) = reflex_export(&pkg).unwrap();
        let (j2, r2) = reflex_export(&pkg).unwrap();
        assert_eq!(j1, j2);
        assert_eq!(r1.output_digest, r2.output_digest);
    }

    #[test]
    fn test_check_rejects_divergence() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let error = reflex_check_candidate(
            &pkg,
            CertificateStrategy::BernsteinPolynomial,
            -100.0,
            -99.0,
        )
        .unwrap_err();
        assert!(!error.is_empty());
    }

    #[test]
    fn test_cost_is_unavailable_without_external_measurements() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let error =
            reflex_cost_candidate(&pkg, CertificateStrategy::BernsteinPolynomial).unwrap_err();
        assert!(error.contains("CostEvidenceUnavailable"));
    }

    #[test]
    fn test_batch_100_candidates() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let bounds = evaluate_kernel_bounds(
            Interval::new(0.0, 1.0),
            CertificateStrategy::BernsteinPolynomial,
        );
        let cands: Vec<_> = (0..100)
            .map(|_| {
                (
                    CertificateStrategy::BernsteinPolynomial,
                    bounds.low,
                    bounds.high,
                )
            })
            .collect();
        let error = reflex_check_candidate_batch(&pkg, &cands).unwrap_err();
        assert!(error.contains("VerifierUnavailable"));
        let oversized = vec![cands[0]; 101];
        assert!(reflex_check_candidate_batch(&pkg, &oversized).is_err());
    }

    #[test]
    fn test_import_export_roundtrip() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let (json, _) = reflex_export(&pkg).unwrap();
        let imported = import_package_json(&json).unwrap();
        assert_eq!(pkg.kernel_id, imported.kernel_id);
    }

    #[test]
    fn test_conformance_is_unavailable_without_external_worker() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let error = reflex_conformance(&pkg).unwrap_err();
        assert!(error.contains("ConformanceUnavailable"));
    }
}
