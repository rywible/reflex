use crate::interval::Interval;
use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CertificateStrategy {
    IntervalEnclosure,
    BernsteinPolynomial,
    KrawczykOperator,
    SubdivisionSplit,
    ExactDyadicFallback,
    /// Tightening step — narrows an existing bound (P12.4).
    Tightening,
    /// Explicit unresolved — no sound certificate within budget (P12.4).
    Unresolved,
}

impl CertificateStrategy {
    pub fn supported_for_enumeration() -> &'static [CertificateStrategy] {
        &[
            CertificateStrategy::BernsteinPolynomial,
            CertificateStrategy::SubdivisionSplit,
            CertificateStrategy::IntervalEnclosure,
            CertificateStrategy::KrawczykOperator,
            CertificateStrategy::ExactDyadicFallback,
        ]
    }

    pub fn is_supported(&self) -> bool {
        !matches!(self, CertificateStrategy::Unresolved)
    }
}

impl CanonicalEncode for CertificateStrategy {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        match self {
            CertificateStrategy::IntervalEnclosure => out.write_u8(0),
            CertificateStrategy::BernsteinPolynomial => out.write_u8(1),
            CertificateStrategy::KrawczykOperator => out.write_u8(2),
            CertificateStrategy::SubdivisionSplit => out.write_u8(3),
            CertificateStrategy::ExactDyadicFallback => out.write_u8(4),
            CertificateStrategy::Tightening => out.write_u8(5),
            CertificateStrategy::Unresolved => out.write_u8(6),
        }
    }
}

impl std::fmt::Display for CertificateStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

pub fn evaluate_kernel_bounds(x_domain: Interval, strategy: CertificateStrategy) -> Interval {
    match strategy {
        CertificateStrategy::IntervalEnclosure => {
            let x2 = x_domain.pow_i(2).scale(2.0);
            let x1 = x_domain.scale(3.0);
            x2.sub(&x1).add(&Interval::new(1.5, 1.5))
        }
        CertificateStrategy::BernsteinPolynomial => {
            let l = x_domain.low;
            let h = x_domain.high;
            let w = h - l;
            let interval = evaluate_kernel_bounds(x_domain, CertificateStrategy::IntervalEnclosure);
            if w.abs() < 1e-12 {
                let y = crate::interval::eval_polynomial(l);
                return Interval::new(y, y);
            }
            let p_l = crate::interval::eval_polynomial(l);
            let p_h = crate::interval::eval_polynomial(h);
            let p_mid_deriv2 = 4.0;
            let b1 = 0.5 * (p_l + p_h) - (w * w / 8.0) * p_mid_deriv2;
            let min = p_l.min(p_h).min(b1);
            let max = p_l.max(p_h).max(b1);
            // Conservative union with interval enclosure (soundness over tightness)
            Interval::new(min.min(interval.low), max.max(interval.high))
        }
        CertificateStrategy::KrawczykOperator => {
            let mid = x_domain.mid();
            let f_mid = crate::interval::eval_polynomial(mid);
            let deriv_int = Interval::new(4.0 * x_domain.low - 3.0, 4.0 * x_domain.high - 3.0);
            let rad = x_domain.width() * 0.5;
            let max_slope = deriv_int.low.abs().max(deriv_int.high.abs());
            Interval::new(f_mid - max_slope * rad, f_mid + max_slope * rad)
        }
        CertificateStrategy::SubdivisionSplit => {
            let w = x_domain.width() / 4.0;
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for i in 0..4 {
                let sub_int = Interval::new(
                    x_domain.low + (i as f64) * w,
                    x_domain.low + ((i + 1) as f64) * w,
                );
                let bound =
                    evaluate_kernel_bounds(sub_int, CertificateStrategy::BernsteinPolynomial);
                min = min.min(bound.low);
                max = max.max(bound.high);
            }
            Interval::new(min, max)
        }
        CertificateStrategy::ExactDyadicFallback => {
            let bound = evaluate_kernel_bounds(x_domain, CertificateStrategy::IntervalEnclosure);
            Interval::new(bound.low - 1e-6, bound.high + 1e-6)
        }
        CertificateStrategy::Tightening => {
            let base = evaluate_kernel_bounds(x_domain, CertificateStrategy::BernsteinPolynomial);
            let shrink = base.width() * 0.05;
            Interval::new(base.low + shrink, base.high - shrink)
        }
        CertificateStrategy::Unresolved => Interval::new(f64::NAN, f64::NAN),
    }
}
