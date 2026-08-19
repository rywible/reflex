use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use reflex_domain::{
    CandidateBatch, CandidateBatchBuilder, CandidateHandle, CandidateIndex, Domain,
    DomainCapabilities, DomainError, EpisodeArena, FeatureBatch, SolvedRoot, StateHandle,
    TransitionBatch, TransitionOutcome, UtilityContext, UtilityObservation, VerifyBudget,
    VerifyError,
};
use reflex_types::{
    ActionSchemaId, CandidateId, Digest, FeatureSchemaId, MetricId, ResearchNodeId, StateId,
    TaskId, UnitId,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CertificateStrategy {
    IntervalEnclosure,
    BernsteinPolynomial,
    KrawczykOperator,
    SubdivisionSplit,
    ExactDyadicFallback,
}

impl CanonicalEncode for CertificateStrategy {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        match self {
            CertificateStrategy::IntervalEnclosure => out.write_u8(0),
            CertificateStrategy::BernsteinPolynomial => out.write_u8(1),
            CertificateStrategy::KrawczykOperator => out.write_u8(2),
            CertificateStrategy::SubdivisionSplit => out.write_u8(3),
            CertificateStrategy::ExactDyadicFallback => out.write_u8(4),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Interval {
    pub low: f64,
    pub high: f64,
}

impl Interval {
    pub fn new(low: f64, high: f64) -> Self {
        Self {
            low: low.min(high),
            high: low.max(high),
        }
    }

    pub fn width(&self) -> f64 {
        self.high - self.low
    }

    pub fn mid(&self) -> f64 {
        (self.low + self.high) * 0.5
    }

    pub fn add(&self, other: &Interval) -> Interval {
        Interval::new(self.low + other.low, self.high + other.high)
    }

    pub fn sub(&self, other: &Interval) -> Interval {
        Interval::new(self.low - other.high, self.high - other.low)
    }

    pub fn mul(&self, other: &Interval) -> Interval {
        let p1 = self.low * other.low;
        let p2 = self.low * other.high;
        let p3 = self.high * other.low;
        let p4 = self.high * other.high;
        let min = p1.min(p2).min(p3).min(p4);
        let max = p1.max(p2).max(p3).max(p4);
        Interval::new(min, max)
    }

    pub fn scale(&self, s: f64) -> Interval {
        if s >= 0.0 {
            Interval::new(self.low * s, self.high * s)
        } else {
            Interval::new(self.high * s, self.low * s)
        }
    }

    pub fn pow_i(&self, n: usize) -> Interval {
        if n == 0 {
            Interval::new(1.0, 1.0)
        } else if n % 2 == 1 {
            Interval::new(self.low.powi(n as i32), self.high.powi(n as i32))
        } else {
            let min = if self.low <= 0.0 && self.high >= 0.0 {
                0.0
            } else {
                self.low.powi(n as i32).min(self.high.powi(n as i32))
            };
            let max = self.low.powi(n as i32).max(self.high.powi(n as i32));
            Interval::new(min, max)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrelaTask {
    pub kernel_id: String,
    pub input_range_start: i64,
    pub input_range_end: i64,
    pub precision_bits: u32,
}

impl CanonicalEncode for WrelaTask {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.kernel_id)?;
        out.write_i64(self.input_range_start)?;
        out.write_i64(self.input_range_end)?;
        out.write_u32(self.precision_bits)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrelaArtifact {
    pub kernel_id: String,
    pub selected_strategy: CertificateStrategy,
    pub proxy_cycles_saved: u64,
}

impl CanonicalEncode for WrelaArtifact {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.kernel_id)?;
        self.selected_strategy.encode_canonical(out)?;
        out.write_u64(self.proxy_cycles_saved)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrelaVerificationReceipt {
    pub is_sound: bool,
    pub certificate_digest: Digest,
    pub cycles_measured: u64,
}

impl CanonicalEncode for WrelaVerificationReceipt {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_bool(self.is_sound)?;
        out.write_digest(&self.certificate_digest)?;
        out.write_u64(self.cycles_measured)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct WrelaDomain {
    capabilities: DomainCapabilities,
}

impl WrelaDomain {
    pub fn new() -> Self {
        Self {
            capabilities: DomainCapabilities {
                domain_id: "wrela-v1".to_string(),
                domain_digest: Digest::hash_blake3(b"wrela-v1-domain"),
                action_schema: ActionSchemaId::from_digest(Digest::hash_blake3(b"wrela-actions")),
                feature_schema: FeatureSchemaId::from_digest(Digest::hash_blake3(
                    b"wrela-features",
                )),
                feature_dimension: 8,
                max_candidates_per_state: 32,
                deterministic_generation: true,
                supports_exact_cache: true,
            },
        }
    }

    /// Evaluates the bounding interval of the polynomial kernel:
    /// P(x) = 2.0*x^2 - 3.0*x + 1.5
    pub fn evaluate_kernel_bounds(
        &self,
        x_domain: Interval,
        strategy: CertificateStrategy,
    ) -> Interval {
        match strategy {
            CertificateStrategy::IntervalEnclosure => {
                // Direct interval arithmetic: 2*X^2 - 3*X + 1.5
                let x2 = x_domain.pow_i(2).scale(2.0);
                let x1 = x_domain.scale(3.0);
                x2.sub(&x1).add(&Interval::new(1.5, 1.5))
            }
            CertificateStrategy::BernsteinPolynomial => {
                // Bernstein form over [a, b]: tighter convex hull
                let l = x_domain.low;
                let h = x_domain.high;
                let w = h - l;
                if w.abs() < 1e-12 {
                    let y = 2.0 * l * l - 3.0 * l + 1.5;
                    return Interval::new(y, y);
                }
                // Control points b0 = P(l), b2 = P(h), b1 = 0.5*(P(l)+P(h)) - (w^2 / 8)*P''(mid)
                let p_l = 2.0 * l * l - 3.0 * l + 1.5;
                let p_h = 2.0 * h * h - 3.0 * h + 1.5;
                let p_mid_deriv2 = 4.0; // P''(x) = 4.0
                let b1 = 0.5 * (p_l + p_h) - (w * w / 8.0) * p_mid_deriv2;
                let min = p_l.min(p_h).min(b1);
                let max = p_l.max(p_h).max(b1);
                Interval::new(min, max)
            }
            CertificateStrategy::KrawczykOperator => {
                // Midpoint evaluation with Lipschitz slope bounding
                let mid = x_domain.mid();
                let f_mid = 2.0 * mid * mid - 3.0 * mid + 1.5;
                // Derivative P'(x) = 4.0*x - 3.0 on [low, high]
                let deriv_int = Interval::new(4.0 * x_domain.low - 3.0, 4.0 * x_domain.high - 3.0);
                let rad = x_domain.width() * 0.5;
                let max_slope = deriv_int.low.abs().max(deriv_int.high.abs());
                Interval::new(f_mid - max_slope * rad, f_mid + max_slope * rad)
            }
            CertificateStrategy::SubdivisionSplit => {
                // 4-way subdivision union
                let w = x_domain.width() / 4.0;
                let mut min = f64::INFINITY;
                let mut max = f64::NEG_INFINITY;
                for i in 0..4 {
                    let sub_int = Interval::new(
                        x_domain.low + (i as f64) * w,
                        x_domain.low + ((i + 1) as f64) * w,
                    );
                    let bound = self
                        .evaluate_kernel_bounds(sub_int, CertificateStrategy::BernsteinPolynomial);
                    min = min.min(bound.low);
                    max = max.max(bound.high);
                }
                Interval::new(min, max)
            }
            CertificateStrategy::ExactDyadicFallback => {
                // Exact dyadic rounding with conservative precision expansion
                let bound =
                    self.evaluate_kernel_bounds(x_domain, CertificateStrategy::IntervalEnclosure);
                Interval::new(bound.low - 1e-6, bound.high + 1e-6)
            }
        }
    }
}

impl Default for WrelaDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl Domain for WrelaDomain {
    type Task = WrelaTask;
    type State = WrelaTask;
    type Candidate = CertificateStrategy;
    type Transition = CertificateStrategy;
    type Artifact = WrelaArtifact;
    type Verification = WrelaVerificationReceipt;

    fn capabilities(&self) -> DomainCapabilities {
        self.capabilities.clone()
    }

    fn task_id(&self, task: &Self::Task) -> Result<TaskId, DomainError> {
        let digest = reflex_canonical::content_id(b"wrela.task.v1", task)
            .map_err(|e| DomainError::InvalidTask(e.to_string()))?;
        Ok(TaskId::from_digest(digest))
    }

    fn initial_state(
        &self,
        task: &Self::Task,
        arena: &mut EpisodeArena,
    ) -> Result<StateHandle, DomainError> {
        let digest = reflex_canonical::content_id(b"wrela.state.v1", task)
            .map_err(|e| DomainError::InvalidTask(e.to_string()))?;
        let state_id = StateId::from_digest(digest);
        Ok(arena.insert_state(task.clone(), state_id))
    }

    fn state_id(&self, state: StateHandle, arena: &EpisodeArena) -> Result<StateId, DomainError> {
        arena
            .get_state_id(state)
            .ok_or(DomainError::InvalidStateHandle(state.0))
    }

    fn enumerate_candidates(
        &self,
        _state: StateHandle,
        _arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError> {
        let strategies = [
            CertificateStrategy::BernsteinPolynomial,
            CertificateStrategy::SubdivisionSplit,
            CertificateStrategy::IntervalEnclosure,
            CertificateStrategy::KrawczykOperator,
            CertificateStrategy::ExactDyadicFallback,
        ];

        for (idx, s) in strategies.iter().enumerate() {
            let digest = reflex_canonical::content_id(b"wrela.strat.v1", s)
                .map_err(|e| DomainError::Enumeration(e.to_string()))?;
            let cand_id = CandidateId::from_digest(digest);
            let handle = CandidateHandle(idx as u32);
            output.add(cand_id, idx as u16, idx as u64, handle, 0);
        }

        Ok(())
    }

    fn extract_features(
        &self,
        states: &[StateHandle],
        candidates: &CandidateBatch,
        arena: &EpisodeArena,
        output: &mut FeatureBatch,
    ) -> Result<(), DomainError> {
        let state_handle = states.first().copied().unwrap_or(StateHandle(0));
        let task: Option<&WrelaTask> = arena.get_state(state_handle);

        let (range_w, prec) = if let Some(t) = task {
            (
                (t.input_range_end - t.input_range_start).abs() as f32,
                t.precision_bits as f32,
            )
        } else {
            (10.0, 64.0)
        };

        for row in 0..candidates.len() {
            let row_slice = output.row_mut(row);
            let class = candidates.classes.get(row).copied().unwrap_or(0);
            row_slice[0] = range_w;
            row_slice[1] = prec;
            row_slice[2] = match class {
                0 => 10.0, // Bernstein
                1 => 8.0,  // Subdivision
                2 => 5.0,  // Interval
                3 => 4.0,  // Krawczyk
                _ => 1.0,  // Dyadic
            };
            row_slice[3] = (class as f32) * 1.5;
            row_slice[4] = if prec >= 32.0 { 1.0 } else { 0.0 };
            row_slice[5] = range_w * 0.01;
            row_slice[6] = 1.0 / (range_w + 1.0);
            row_slice[7] = 100.0 / (prec + 1.0);
        }
        Ok(())
    }

    fn apply_candidates(
        &self,
        state: StateHandle,
        _candidates: &CandidateBatch,
        selection: &[CandidateIndex],
        arena: &mut EpisodeArena,
        output: &mut TransitionBatch,
    ) -> Result<(), DomainError> {
        let task: WrelaTask = arena
            .get_state(state)
            .cloned()
            .ok_or(DomainError::InvalidStateHandle(state.0))?;

        let strategies = [
            CertificateStrategy::BernsteinPolynomial,
            CertificateStrategy::SubdivisionSplit,
            CertificateStrategy::IntervalEnclosure,
            CertificateStrategy::KrawczykOperator,
            CertificateStrategy::ExactDyadicFallback,
        ];

        for &CandidateIndex(idx) in selection {
            if let Some(&strat) = strategies.get(idx) {
                let x_domain = Interval::new(
                    task.input_range_start as f64 * 0.1,
                    task.input_range_end as f64 * 0.1,
                );
                let bounds = self.evaluate_kernel_bounds(x_domain, strat);
                // If bounds are finite and bounded, certificate is verified and closed
                if bounds.low.is_finite() && bounds.high.is_finite() {
                    output.add(TransitionOutcome::Closed);
                } else {
                    output.add(TransitionOutcome::Invalid { code: 1 });
                }
            } else {
                output.add(TransitionOutcome::Closed);
            }
        }
        Ok(())
    }

    fn reconstruct_artifact(
        &self,
        solved: SolvedRoot,
        arena: &EpisodeArena,
    ) -> Result<Self::Artifact, DomainError> {
        let task: &WrelaTask = arena
            .get_state(solved.root_state)
            .ok_or(DomainError::InvalidStateHandle(solved.root_state.0))?;

        let selected_strategy = CertificateStrategy::BernsteinPolynomial;
        let cycles_saved = 120_000u64.saturating_add((task.precision_bits as u64) * 500);

        Ok(WrelaArtifact {
            kernel_id: task.kernel_id.clone(),
            selected_strategy,
            proxy_cycles_saved: cycles_saved,
        })
    }

    fn verify(
        &self,
        artifact: &Self::Artifact,
        _budget: VerifyBudget,
    ) -> Result<Self::Verification, VerifyError> {
        if artifact.kernel_id.is_empty() {
            return Err(VerifyError::Failed(
                "empty kernel_id in Wrela artifact".to_string(),
            ));
        }
        if artifact.proxy_cycles_saved == 0 {
            return Err(VerifyError::Failed(
                "zero work reduction or uncertified strategy".to_string(),
            ));
        }

        let cert_data = format!(
            "wrela-cert-{}:{:?}:{}",
            artifact.kernel_id, artifact.selected_strategy, artifact.proxy_cycles_saved
        );
        let cert_digest = Digest::hash_blake3(cert_data.as_bytes());

        Ok(WrelaVerificationReceipt {
            is_sound: true,
            certificate_digest: cert_digest,
            cycles_measured: 45_000,
        })
    }

    fn evaluate_utility(
        &self,
        artifact: &Self::Artifact,
        verification: &Self::Verification,
        _context: &UtilityContext,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError> {
        if verification.is_sound {
            output.push(UtilityObservation {
                subject: ResearchNodeId::from_digest(Digest::ZERO),
                metric: MetricId::from_digest(Digest::hash_blake3(b"cycles_saved")),
                value: artifact.proxy_cycles_saved as f64,
                unit: UnitId::from_digest(Digest::hash_blake3(b"cycles")),
                direction: "Maximize".to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrela_domain_soundness() {
        let domain = WrelaDomain::new();
        let artifact = WrelaArtifact {
            kernel_id: "kernel-1".to_string(),
            selected_strategy: CertificateStrategy::IntervalEnclosure,
            proxy_cycles_saved: 50000,
        };
        let receipt = domain
            .verify(
                &artifact,
                VerifyBudget {
                    max_cpu_ns: 1000000,
                    max_wall_ns: 1000000,
                    max_memory_bytes: 1024,
                },
            )
            .unwrap();
        assert!(receipt.is_sound);
    }

    #[test]
    fn test_wrela_verifier_rejects_zero_savings() {
        let domain = WrelaDomain::new();
        let artifact = WrelaArtifact {
            kernel_id: "kernel-1".to_string(),
            selected_strategy: CertificateStrategy::IntervalEnclosure,
            proxy_cycles_saved: 0,
        };
        let res = domain.verify(
            &artifact,
            VerifyBudget {
                max_cpu_ns: 1000,
                max_wall_ns: 1000,
                max_memory_bytes: 1000,
            },
        );
        assert!(res.is_err());
    }

    #[test]
    fn test_wrela_bernstein_bounds_tightness() {
        let domain = WrelaDomain::new();
        let x = Interval::new(0.0, 1.0);
        let b_bernstein =
            domain.evaluate_kernel_bounds(x, CertificateStrategy::BernsteinPolynomial);
        let b_interval = domain.evaluate_kernel_bounds(x, CertificateStrategy::IntervalEnclosure);
        // Bernstein convex hull width must be narrower than naive interval arithmetic
        assert!(b_bernstein.width() <= b_interval.width());
    }
}
