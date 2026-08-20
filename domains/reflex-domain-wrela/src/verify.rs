//! Sound certificate verification — authority is computed, never fabricated.

use crate::certificate::CertificateStrategy;
use crate::interval::Interval;
use reflex_domain::VerifyError;
use reflex_types::Digest;

use crate::kernel_package::KernelPackage;

#[derive(Clone, Debug, PartialEq)]
pub struct CertificatePayload {
    pub kernel_id: String,
    pub package_digest: Digest,
    pub strategy: CertificateStrategy,
    pub input_low: f64,
    pub input_high: f64,
    pub claimed_bounds: Interval,
    pub proxy_cycles_saved: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedCertificate {
    pub is_sound: bool,
    pub certificate_digest: Digest,
    pub verification_cpu_ns: u64,
}

/// Verify a certificate bound to `package` identity (INV-RFX-1).
///
/// Requires `payload.package_digest == package.identity()` and includes the
/// digest in the certificate hash so soundness is package-bound.
pub fn verify_certificate(
    package: &KernelPackage,
    payload: &CertificatePayload,
) -> Result<VerifiedCertificate, VerifyError> {
    let expected_digest = package
        .identity()
        .map_err(|e| VerifyError::Failed(format!("cannot compute KernelPackage identity: {e}")))?;
    if payload.package_digest != expected_digest {
        return Err(VerifyError::Failed(format!(
            "package_digest mismatch: certificate {} != KernelPackage::identity() {}",
            payload.package_digest.to_hex(),
            expected_digest.to_hex()
        )));
    }
    if payload.kernel_id != package.kernel_id {
        return Err(VerifyError::Failed(format!(
            "kernel_id mismatch: certificate '{}' != package '{}'",
            payload.kernel_id, package.kernel_id
        )));
    }
    if payload.kernel_id.is_empty() {
        return Err(VerifyError::Failed(
            "empty kernel_id in Wrela certificate".to_string(),
        ));
    }
    if payload.package_digest == Digest::ZERO {
        return Err(VerifyError::Failed(
            "unbound package_digest (ZERO) rejected".to_string(),
        ));
    }
    if !payload.strategy.is_supported() {
        return Err(VerifyError::Failed(format!(
            "unsupported certificate strategy {:?}",
            payload.strategy
        )));
    }
    if !payload.claimed_bounds.low.is_finite() || !payload.claimed_bounds.high.is_finite() {
        return Err(VerifyError::Failed(
            "non-finite claimed certificate bounds".to_string(),
        ));
    }

    Err(VerifyError::Unresolved(
        "VerifierUnavailable: no external Wrela checker receipt was supplied; internal test kernels are not verification authority"
            .to_string(),
    ))
}

#[allow(clippy::too_many_arguments)]
pub fn verify_artifact_fields(
    package: &KernelPackage,
    package_digest: Digest,
    strategy: CertificateStrategy,
    input_low: f64,
    input_high: f64,
    bounds_low: f64,
    bounds_high: f64,
    proxy_cycles_saved: i64,
) -> Result<VerifiedCertificate, VerifyError> {
    verify_certificate(
        package,
        &CertificatePayload {
            kernel_id: package.kernel_id.clone(),
            package_digest,
            strategy,
            input_low,
            input_high,
            claimed_bounds: Interval::new(bounds_low, bounds_high),
            proxy_cycles_saved,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certificate::evaluate_kernel_bounds;

    #[test]
    fn test_synthetic_certificate_is_not_accepted_without_external_checker() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let digest = pkg.identity().unwrap();
        let x = Interval::new(0.0, 1.0);
        let bounds = evaluate_kernel_bounds(x, CertificateStrategy::BernsteinPolynomial);
        let result = verify_artifact_fields(
            &pkg,
            digest,
            CertificateStrategy::BernsteinPolynomial,
            0.0,
            1.0,
            bounds.low,
            bounds.high,
            50_000,
        );
        assert!(matches!(
            result,
            Err(VerifyError::Unresolved(message)) if message.contains("VerifierUnavailable")
        ));
    }

    #[test]
    fn test_package_digest_mismatch_rejected() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let x = Interval::new(0.0, 1.0);
        let bounds = evaluate_kernel_bounds(x, CertificateStrategy::BernsteinPolynomial);
        let res = verify_artifact_fields(
            &pkg,
            Digest::hash_blake3(b"wrong-package"),
            CertificateStrategy::BernsteinPolynomial,
            0.0,
            1.0,
            bounds.low,
            bounds.high,
            50_000,
        );
        assert!(res.is_err());
        let msg = format!("{}", res.unwrap_err());
        assert!(msg.contains("package_digest mismatch"));
    }

    #[test]
    fn test_unsound_synthetic_claim_never_produces_receipt() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let digest = pkg.identity().unwrap();
        let res = verify_artifact_fields(
            &pkg,
            digest,
            CertificateStrategy::BernsteinPolynomial,
            0.0,
            1.0,
            -999.0,
            -998.0,
            50_000,
        );
        assert!(res.is_err());
    }

    #[test]
    fn test_no_fabricated_verification_timing() {
        let pkg = KernelPackage::synthetic_test_fixture_quadratic();
        let digest = pkg.identity().unwrap();
        let bounds = evaluate_kernel_bounds(
            Interval::new(0.0, 1.0),
            CertificateStrategy::IntervalEnclosure,
        );
        let result = verify_artifact_fields(
            &pkg,
            digest,
            CertificateStrategy::IntervalEnclosure,
            0.0,
            1.0,
            bounds.low,
            bounds.high,
            10_000,
        );
        assert!(result.is_err());
    }
}
