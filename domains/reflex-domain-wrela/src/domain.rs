use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter, content_id};
use reflex_domain::{
    CandidateBatch, CandidateBatchBuilder, CandidateHandle, CandidateIndex, Domain,
    DomainCapabilities, DomainError, EpisodeArena, FeatureBatch, SolvedRoot, StateHandle,
    TransitionBatch, UtilityContext, UtilityObservation, VerifyBudget, VerifyError,
};
use reflex_economics::{BetterDirection, ConfidenceClass};
use reflex_types::{
    ActionSchemaId, CandidateId, Digest, EvaluatorId, FeatureSchemaId, MetricId, StateId, TaskId,
    UnitId,
};
use serde::{Deserialize, Serialize};

use crate::certificate::{CertificateStrategy, evaluate_kernel_bounds};
use crate::interval::Interval;
use crate::kernel_package::{KernelPackage, ValidateOutcome, validate_package};
use crate::verify::verify_artifact_fields;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrelaTask {
    pub kernel_id: String,
    pub input_range_start: i64,
    pub input_range_end: i64,
    pub precision_bits: u32,
    pub package: KernelPackage,
}

impl CanonicalEncode for WrelaTask {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.kernel_id)?;
        out.write_i64(self.input_range_start)?;
        out.write_i64(self.input_range_end)?;
        out.write_u32(self.precision_bits)?;
        self.package.encode_canonical(out)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WrelaArtifact {
    pub kernel_id: String,
    pub package_digest: Digest,
    /// Package retained so verify can rebind `package_digest` to identity (INV-RFX-1).
    pub package: KernelPackage,
    pub selected_strategy: CertificateStrategy,
    /// Signed modelled delta from the package's declared target cost table.
    pub proxy_cycles_saved: i64,
    pub certificate_low: f64,
    pub certificate_high: f64,
    pub input_low: f64,
    pub input_high: f64,
    /// Exact equivalence vs sound strengthening (P12.5).
    pub receipt_class: WrelaReceiptClass,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WrelaReceiptClass {
    ExactEquivalence,
    SoundStrengthening,
}

impl CanonicalEncode for WrelaArtifact {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.kernel_id)?;
        out.write_digest(&self.package_digest)?;
        self.package.encode_canonical(out)?;
        self.selected_strategy.encode_canonical(out)?;
        out.write_i64(self.proxy_cycles_saved)?;
        out.write_f64(self.certificate_low)?;
        out.write_f64(self.certificate_high)?;
        out.write_f64(self.input_low)?;
        out.write_f64(self.input_high)?;
        match self.receipt_class {
            WrelaReceiptClass::ExactEquivalence => out.write_u8(0),
            WrelaReceiptClass::SoundStrengthening => out.write_u8(1),
        }?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WrelaVerificationReceipt {
    pub is_sound: bool,
    pub certificate_digest: Digest,
    pub verification_cpu_ns: u64,
}

impl CanonicalEncode for WrelaVerificationReceipt {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_bool(self.is_sound)?;
        out.write_digest(&self.certificate_digest)?;
        out.write_u64(self.verification_cpu_ns)?;
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

    pub fn evaluate_kernel_bounds(
        &self,
        x_domain: Interval,
        strategy: CertificateStrategy,
    ) -> Interval {
        evaluate_kernel_bounds(x_domain, strategy)
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
        let digest = content_id(b"wrela.task.v1", task)
            .map_err(|e| DomainError::InvalidTask(e.to_string()))?;
        Ok(TaskId::from_digest(digest))
    }

    fn initial_state(
        &self,
        task: &Self::Task,
        arena: &mut EpisodeArena,
    ) -> Result<StateHandle, DomainError> {
        match validate_package(&task.package) {
            ValidateOutcome::Valid(_) => {}
            ValidateOutcome::Rejected(d) => {
                return Err(DomainError::InvalidTask(format!(
                    "unsupported kernel package: {d:?}"
                )));
            }
        }
        let digest = content_id(b"wrela.state.v1", task)
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
        state: StateHandle,
        arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError> {
        let _task: &WrelaTask = arena
            .get_state(state)
            .ok_or(DomainError::InvalidStateHandle(state.0))?;

        for (idx, s) in CertificateStrategy::supported_for_enumeration()
            .iter()
            .enumerate()
        {
            let digest = content_id(b"wrela.strat.v1", s)
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
        let state_handle = states.first().copied().unwrap_or(StateHandle(0, 0));
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
                0 => 10.0,
                1 => 8.0,
                2 => 5.0,
                3 => 4.0,
                _ => 1.0,
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
        _selection: &[CandidateIndex],
        arena: &mut EpisodeArena,
        _output: &mut TransitionBatch,
    ) -> Result<(), DomainError> {
        let _: &WrelaTask = arena
            .get_state(state)
            .ok_or(DomainError::InvalidStateHandle(state.0))?;
        Err(DomainError::Application(
            "VerifierUnavailable: candidate application requires an accepted external Wrela checker result"
                .to_string(),
        ))
    }

    fn reconstruct_artifact(
        &self,
        solved: SolvedRoot,
        arena: &EpisodeArena,
    ) -> Result<Self::Artifact, DomainError> {
        let task: &WrelaTask = arena
            .get_state(solved.root_state)
            .ok_or(DomainError::InvalidStateHandle(solved.root_state.0))?;

        let selected_strategy = solved
            .solved_edges
            .last()
            .map(|(_, handle, _)| handle.0 as usize)
            .and_then(|idx| CertificateStrategy::supported_for_enumeration().get(idx))
            .copied()
            .unwrap_or(CertificateStrategy::BernsteinPolynomial);

        let x_domain = Interval::new(
            task.input_range_start as f64 * 0.1,
            task.input_range_end as f64 * 0.1,
        );
        let bounds = self.evaluate_kernel_bounds(x_domain, selected_strategy);
        let package_digest = task
            .package
            .identity()
            .map_err(|e| DomainError::Application(e.to_string()))?;
        let baseline = task
            .package
            .target_cost_table
            .iter()
            .find(|(target, _)| target == "baseline_scalar")
            .map(|(_, cost)| *cost)
            .ok_or_else(|| {
                DomainError::Application(
                    "economic evidence unavailable: missing baseline_scalar target cost"
                        .to_string(),
                )
            })?;
        let strategy_key = match selected_strategy {
            CertificateStrategy::IntervalEnclosure => "interval",
            CertificateStrategy::BernsteinPolynomial => "bernstein",
            CertificateStrategy::KrawczykOperator => "krawczyk",
            CertificateStrategy::SubdivisionSplit => "subdivision",
            CertificateStrategy::ExactDyadicFallback => "exact_dyadic",
            CertificateStrategy::Tightening => "tightening",
            CertificateStrategy::Unresolved => {
                return Err(DomainError::Application(
                    "economic evidence unavailable for unresolved strategy".to_string(),
                ));
            }
        };
        let candidate = task
            .package
            .target_cost_table
            .iter()
            .find(|(target, _)| target == strategy_key)
            .map(|(_, cost)| *cost)
            .ok_or_else(|| {
                DomainError::Application(format!(
                    "economic evidence unavailable: missing declared cost for {strategy_key}"
                ))
            })?;
        let proxy_cycles_saved = i64::try_from(i128::from(baseline) - i128::from(candidate))
            .map_err(|_| DomainError::Application("declared cost delta overflow".to_string()))?;

        let receipt_class = if selected_strategy == CertificateStrategy::ExactDyadicFallback {
            WrelaReceiptClass::ExactEquivalence
        } else {
            WrelaReceiptClass::SoundStrengthening
        };

        Ok(WrelaArtifact {
            kernel_id: task.kernel_id.clone(),
            package_digest,
            package: task.package.clone(),
            selected_strategy,
            proxy_cycles_saved,
            certificate_low: bounds.low,
            certificate_high: bounds.high,
            input_low: x_domain.low,
            input_high: x_domain.high,
            receipt_class,
        })
    }

    fn verify(
        &self,
        artifact: &Self::Artifact,
        _budget: VerifyBudget,
    ) -> Result<Self::Verification, VerifyError> {
        let verified = verify_artifact_fields(
            &artifact.package,
            artifact.package_digest,
            artifact.selected_strategy,
            artifact.input_low,
            artifact.input_high,
            artifact.certificate_low,
            artifact.certificate_high,
            artifact.proxy_cycles_saved,
        )?;
        Ok(WrelaVerificationReceipt {
            is_sound: verified.is_sound,
            certificate_digest: verified.certificate_digest,
            verification_cpu_ns: verified.verification_cpu_ns,
        })
    }

    fn evaluate_utility(
        &self,
        artifact: &Self::Artifact,
        verification: &Self::Verification,
        context: &UtilityContext,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError> {
        context.validate()?;
        if verification.is_sound {
            if artifact.package_digest == Digest::ZERO
                || verification.certificate_digest == Digest::ZERO
            {
                return Err(DomainError::UtilityEvaluation(
                    "Wrela utility requires non-zero package and certificate evidence".into(),
                ));
            }
            let mut evidence = context.accepted_evidence.clone();
            for digest in [artifact.package_digest, verification.certificate_digest] {
                if !evidence.contains(&digest) {
                    evidence.push(digest);
                }
            }
            output.push(UtilityObservation {
                subject: context.subject,
                evaluator: EvaluatorId::from_digest(Digest::hash_blake3(b"wrela-evaluator")),
                metric: MetricId::from_digest(Digest::hash_blake3(b"cycles_saved")),
                value: reflex_economics::RationalOrFloat::Float(artifact.proxy_cycles_saved as f64),
                unit: UnitId::from_digest(Digest::hash_blake3(b"cycles")),
                direction: BetterDirection::HigherIsBetter,
                population: context.population,
                evidence,
                confidence: ConfidenceClass::EstimatedModel,
                observed_at_generation: context.observed_at_generation,
                restricted_work: false,
            });
        }
        Ok(())
    }
}

#[cfg(test)]
fn synthetic_test_fixture_task() -> WrelaTask {
    let pkg = KernelPackage::synthetic_test_fixture_quadratic();
    WrelaTask {
        kernel_id: pkg.kernel_id.clone(),
        input_range_start: 0,
        input_range_end: 10,
        precision_bits: 64,
        package: pkg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wrela_domain_soundness() {
        let domain = WrelaDomain::new();
        let task = synthetic_test_fixture_task();
        let x = Interval::new(0.0, 1.0);
        let bounds = domain.evaluate_kernel_bounds(x, CertificateStrategy::IntervalEnclosure);
        let digest = task.package.identity().unwrap();
        let artifact = WrelaArtifact {
            kernel_id: task.kernel_id,
            package_digest: digest,
            package: task.package.clone(),
            selected_strategy: CertificateStrategy::IntervalEnclosure,
            proxy_cycles_saved: 50_000,
            certificate_low: bounds.low,
            certificate_high: bounds.high,
            input_low: 0.0,
            input_high: 1.0,
            receipt_class: WrelaReceiptClass::SoundStrengthening,
        };
        let result = domain.verify(
            &artifact,
            VerifyBudget {
                max_cpu_ns: 1_000_000,
                max_wall_ns: 1_000_000,
                max_memory_bytes: 1024,
            },
        );
        assert!(matches!(
            result,
            Err(VerifyError::Unresolved(message)) if message.contains("VerifierUnavailable")
        ));
    }

    #[test]
    fn test_wrela_verifier_rejects_zero_savings() {
        let domain = WrelaDomain::new();
        let task = synthetic_test_fixture_task();
        let artifact = WrelaArtifact {
            kernel_id: task.kernel_id,
            package_digest: task.package.identity().unwrap(),
            package: task.package.clone(),
            selected_strategy: CertificateStrategy::IntervalEnclosure,
            proxy_cycles_saved: 0,
            certificate_low: 0.0,
            certificate_high: 1.0,
            input_low: 0.0,
            input_high: 1.0,
            receipt_class: WrelaReceiptClass::SoundStrengthening,
        };
        assert!(
            domain
                .verify(
                    &artifact,
                    VerifyBudget {
                        max_cpu_ns: 1000,
                        max_wall_ns: 1000,
                        max_memory_bytes: 1000,
                    },
                )
                .is_err()
        );
    }

    #[test]
    fn test_wrela_bernstein_bounds_tightness() {
        let domain = WrelaDomain::new();
        let x = Interval::new(0.0, 1.0);
        let b_bernstein =
            domain.evaluate_kernel_bounds(x, CertificateStrategy::BernsteinPolynomial);
        let b_interval = domain.evaluate_kernel_bounds(x, CertificateStrategy::IntervalEnclosure);
        assert!(b_bernstein.width() <= b_interval.width());
    }

    #[test]
    fn test_strategy_order_same_authority() {
        let domain = WrelaDomain::new();
        let x = Interval::new(0.0, 1.0);
        let strategies = CertificateStrategy::supported_for_enumeration();
        let mut all_sound = true;
        for s in strategies {
            let b = domain.evaluate_kernel_bounds(x, *s);
            let true_r = crate::interval::true_range_on_domain(0.0, 1.0);
            if !b.contains_interval(&true_r) {
                all_sound = false;
            }
        }
        assert!(all_sound);
    }
}
