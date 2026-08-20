use reflex_canonical::CanonicalWriter;
use reflex_types::{Digest, EvaluatorId, GenerationId, MetricId, ResearchNodeId, UnitId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum EconomicsError {
    #[error("incompatible units: cannot combine {unit_a} and {unit_b}")]
    IncompatibleUnits { unit_a: String, unit_b: String },
    #[error("division by zero or non-finite utility calculation")]
    NonFiniteUtility,
    #[error("missing economic baseline for subject {0}")]
    MissingBaseline(String),
    #[error("unknown unit: {0}")]
    UnknownUnit(String),
    #[error("invalid unit definition for {unit}: {reason}")]
    InvalidUnit { unit: String, reason: String },
    #[error("baseline and treatment differ in {field}")]
    ObservationMismatch { field: &'static str },
    #[error("economic arithmetic overflow")]
    ArithmeticOverflow,
    #[error("unit ID is already registered with a different definition: {0}")]
    UnitIdentityConflict(String),
    #[error("invalid scalarization policy: {0}")]
    InvalidScalarization(String),
}

/// Typed observation value: exact rational or IEEE float (§16.5, INV-RFX-7).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum RationalOrFloat {
    Rational { numerator: i64, denominator: i64 },
    Float(f64),
}

impl RationalOrFloat {
    pub fn as_f64(&self) -> Result<f64, EconomicsError> {
        match self {
            Self::Rational {
                numerator,
                denominator,
            } => {
                if *denominator == 0 {
                    return Err(EconomicsError::NonFiniteUtility);
                }
                Ok(*numerator as f64 / *denominator as f64)
            }
            Self::Float(v) => {
                if !v.is_finite() {
                    return Err(EconomicsError::NonFiniteUtility);
                }
                Ok(*v)
            }
        }
    }

    pub fn is_finite(&self) -> bool {
        match self {
            Self::Rational {
                numerator: _,
                denominator,
            } => *denominator != 0,
            Self::Float(v) => v.is_finite(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BetterDirection {
    LowerIsBetter,
    HigherIsBetter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConfidenceClass {
    ObservedExact,
    EstimatedModel,
    CausalLeaveOneOut,
}

/// Registry of named units with dimensional compatibility (INV-RFX-7).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UnitRegistry {
    units: HashMap<UnitId, UnitSpec>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UnitSpec {
    pub name: String,
    pub dimension: String,
    pub scale: f64,
}

impl UnitRegistry {
    pub fn with_standard_units() -> Self {
        let mut reg = Self::default();
        reg.register(
            UnitId::from_digest(Digest::hash_blake3(b"cycles")),
            "cycles",
            "compute",
            1.0,
        )
        .expect("standard unit is valid");
        reg.register(
            UnitId::from_digest(Digest::hash_blake3(b"cpu_ns")),
            "cpu_ns",
            "time",
            1.0,
        )
        .expect("standard unit is valid");
        reg.register(
            UnitId::from_digest(Digest::hash_blake3(b"actions")),
            "verified_actions",
            "count",
            1.0,
        )
        .expect("standard unit is valid");
        reg.register(
            UnitId::from_digest(Digest::hash_blake3(b"bytes")),
            "bytes",
            "storage",
            1.0,
        )
        .expect("standard unit is valid");
        reg
    }

    /// Register a unit whose `scale` converts values into the dimension's base unit.
    pub fn register(
        &mut self,
        id: UnitId,
        name: &str,
        dimension: &str,
        scale: f64,
    ) -> Result<(), EconomicsError> {
        if name.trim().is_empty() || dimension.trim().is_empty() {
            return Err(EconomicsError::InvalidUnit {
                unit: name.to_string(),
                reason: "name and dimension must be non-empty".to_string(),
            });
        }
        if !scale.is_finite() || scale <= 0.0 {
            return Err(EconomicsError::InvalidUnit {
                unit: name.to_string(),
                reason: "scale must be finite and strictly positive".to_string(),
            });
        }
        let spec = UnitSpec {
            name: name.to_string(),
            dimension: dimension.to_string(),
            scale,
        };
        if let Some(existing) = self.units.get(&id)
            && existing != &spec
        {
            return Err(EconomicsError::UnitIdentityConflict(id.to_hex()));
        }
        self.units.insert(id, spec);
        Ok(())
    }

    pub fn spec(&self, id: UnitId) -> Option<&UnitSpec> {
        self.units.get(&id)
    }

    pub fn compatible(&self, a: UnitId, b: UnitId) -> bool {
        match (self.units.get(&a), self.units.get(&b)) {
            (Some(sa), Some(sb)) => sa.dimension == sb.dimension,
            _ => false,
        }
    }

    fn require_spec(&self, id: UnitId) -> Result<&UnitSpec, EconomicsError> {
        self.spec(id)
            .ok_or_else(|| EconomicsError::UnknownUnit(id.to_hex()))
    }

    /// Convert `value` from `from` into `to`, applying registered scales.
    pub fn convert_value(
        &self,
        from: UnitId,
        value: &RationalOrFloat,
        to: UnitId,
    ) -> Result<RationalOrFloat, EconomicsError> {
        let from_spec = self.require_spec(from)?;
        let to_spec = self.require_spec(to)?;
        if from_spec.dimension != to_spec.dimension {
            return Err(EconomicsError::IncompatibleUnits {
                unit_a: from_spec.name.clone(),
                unit_b: to_spec.name.clone(),
            });
        }
        let converted = value.as_f64()? * from_spec.scale / to_spec.scale;
        if !converted.is_finite() {
            return Err(EconomicsError::NonFiniteUtility);
        }
        Ok(RationalOrFloat::Float(converted))
    }

    pub fn add_values(
        &self,
        a: UnitId,
        va: &RationalOrFloat,
        b: UnitId,
        vb: &RationalOrFloat,
    ) -> Result<RationalOrFloat, EconomicsError> {
        self.require_spec(a)?;
        let vb_in_a = self.convert_value(b, vb, a)?.as_f64()?;
        let sum = va.as_f64()? + vb_in_a;
        if !sum.is_finite() {
            return Err(EconomicsError::NonFiniteUtility);
        }
        Ok(RationalOrFloat::Float(sum))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UtilityObservation {
    pub subject: ResearchNodeId,
    pub evaluator: EvaluatorId,
    pub metric: MetricId,
    pub value: RationalOrFloat,
    pub unit: UnitId,
    pub direction: BetterDirection,
    pub population: Digest,
    pub evidence: Vec<Digest>,
    pub confidence: ConfidenceClass,
    pub observed_at_generation: GenerationId,
    pub restricted_work: bool,
}

/// Named scalarization over immutable observations (does not mutate raw obs).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScalarizationPolicy {
    pub name: String,
    pub ml_weight: f64,
    pub retrieval_weight: f64,
    pub feature_weight: f64,
}

impl Default for ScalarizationPolicy {
    fn default() -> Self {
        Self {
            name: "default_net_cycles".to_string(),
            ml_weight: 1.0,
            retrieval_weight: 1.0,
            feature_weight: 1.0,
        }
    }
}

impl ScalarizationPolicy {
    /// Content identity for a named, immutable reward policy.
    pub fn identity(&self) -> Result<Digest, EconomicsError> {
        if self.name.trim().is_empty()
            || !self.ml_weight.is_finite()
            || !self.retrieval_weight.is_finite()
            || !self.feature_weight.is_finite()
            || self.ml_weight < 0.0
            || self.retrieval_weight < 0.0
            || self.feature_weight < 0.0
        {
            return Err(EconomicsError::InvalidScalarization(
                "name must be non-empty and overhead weights finite and non-negative".to_owned(),
            ));
        }
        let mut bytes = Vec::new();
        let mut writer = CanonicalWriter::new(&mut bytes);
        writer
            .write_str("reflex.scalarization-policy.v1")
            .and_then(|()| writer.write_str(self.name.trim()))
            .and_then(|()| writer.write_f64(self.ml_weight))
            .and_then(|()| writer.write_f64(self.retrieval_weight))
            .and_then(|()| writer.write_f64(self.feature_weight))
            .map_err(|error| EconomicsError::InvalidScalarization(error.to_string()))?;
        Ok(Digest::hash_blake3(&bytes))
    }

    pub fn scalarize(
        &self,
        observations: &[UtilityObservation],
        registry: &UnitRegistry,
        overhead: &EconomicLedgerSummary,
    ) -> Result<f64, EconomicsError> {
        self.identity()?;
        let cycles_unit = UnitId::from_digest(Digest::hash_blake3(b"cycles"));
        let mut total = RationalOrFloat::Float(0.0);
        let mut identity: Option<(&EvaluatorId, &MetricId, &BetterDirection, &Digest, bool)> = None;
        for obs in observations {
            if let Some((evaluator, metric, direction, population, restricted_work)) = identity {
                if &obs.evaluator != evaluator {
                    return Err(EconomicsError::ObservationMismatch { field: "evaluator" });
                }
                if &obs.metric != metric {
                    return Err(EconomicsError::ObservationMismatch { field: "metric" });
                }
                if &obs.direction != direction {
                    return Err(EconomicsError::ObservationMismatch { field: "direction" });
                }
                if &obs.population != population {
                    return Err(EconomicsError::ObservationMismatch {
                        field: "population",
                    });
                }
                if obs.restricted_work != restricted_work {
                    return Err(EconomicsError::ObservationMismatch {
                        field: "restricted_work",
                    });
                }
            } else {
                identity = Some((
                    &obs.evaluator,
                    &obs.metric,
                    &obs.direction,
                    &obs.population,
                    obs.restricted_work,
                ));
            }
            total = registry.add_values(cycles_unit, &total, obs.unit, &obs.value)?;
        }
        let gross = total.as_f64()?;
        let overhead_cycles = (overhead.ml_inference_tax_cpu_ns as f64) * self.ml_weight
            + (overhead.feature_extraction_tax_cpu_ns as f64) * self.feature_weight
            + (overhead.retrieval_tax_cpu_ns as f64) * self.retrieval_weight;
        Ok(gross - overhead_cycles)
    }
}

/// Marginal utility: treatment minus baseline under matched work.
pub fn marginal_utility(
    baseline: &UtilityObservation,
    treatment: &UtilityObservation,
    registry: &UnitRegistry,
) -> Result<RationalOrFloat, EconomicsError> {
    if baseline.evaluator != treatment.evaluator {
        return Err(EconomicsError::ObservationMismatch { field: "evaluator" });
    }
    if baseline.metric != treatment.metric {
        return Err(EconomicsError::ObservationMismatch { field: "metric" });
    }
    if baseline.direction != treatment.direction {
        return Err(EconomicsError::ObservationMismatch { field: "direction" });
    }
    if baseline.population != treatment.population {
        return Err(EconomicsError::ObservationMismatch {
            field: "population",
        });
    }
    if baseline.restricted_work != treatment.restricted_work {
        return Err(EconomicsError::ObservationMismatch {
            field: "restricted_work",
        });
    }
    let b = baseline.value.as_f64()?;
    let t = registry
        .convert_value(treatment.unit, &treatment.value, baseline.unit)?
        .as_f64()?;
    let improvement = match baseline.direction {
        BetterDirection::HigherIsBetter => t - b,
        BetterDirection::LowerIsBetter => b - t,
    };
    if !improvement.is_finite() {
        return Err(EconomicsError::NonFiniteUtility);
    }
    Ok(RationalOrFloat::Float(improvement))
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct EconomicLedgerSummary {
    pub gross_savings_proxy: f64,
    pub ml_inference_tax_cpu_ns: u64,
    pub feature_extraction_tax_cpu_ns: u64,
    pub retrieval_tax_cpu_ns: u64,
    pub net_utility_value: f64,
    pub verified_actions_saved: i64,
}

impl EconomicLedgerSummary {
    pub fn compute_net_savings(
        gross_cycles_saved: f64,
        cpu_ns_to_cycle_ratio: f64,
        ml_inference_cpu_ns: u64,
        feature_cpu_ns: u64,
        retrieval_cpu_ns: u64,
    ) -> Result<Self, EconomicsError> {
        if !gross_cycles_saved.is_finite()
            || !cpu_ns_to_cycle_ratio.is_finite()
            || cpu_ns_to_cycle_ratio < 0.0
        {
            return Err(EconomicsError::NonFiniteUtility);
        }
        let total_overhead_ns = ml_inference_cpu_ns
            .checked_add(feature_cpu_ns)
            .and_then(|value| value.checked_add(retrieval_cpu_ns))
            .ok_or(EconomicsError::ArithmeticOverflow)?;
        let overhead_in_cycles = (total_overhead_ns as f64) * cpu_ns_to_cycle_ratio;
        let net_utility = gross_cycles_saved - overhead_in_cycles;
        if !overhead_in_cycles.is_finite() || !net_utility.is_finite() {
            return Err(EconomicsError::NonFiniteUtility);
        }

        Ok(Self {
            gross_savings_proxy: gross_cycles_saved,
            ml_inference_tax_cpu_ns: ml_inference_cpu_ns,
            feature_extraction_tax_cpu_ns: feature_cpu_ns,
            retrieval_tax_cpu_ns: retrieval_cpu_ns,
            net_utility_value: net_utility,
            verified_actions_saved: 0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_economic_accounting_net_savings() {
        let summary =
            EconomicLedgerSummary::compute_net_savings(1_000_000.0, 1.0, 50_000, 10_000, 5_000)
                .unwrap();

        assert_eq!(summary.gross_savings_proxy, 1_000_000.0);
        assert_eq!(summary.net_utility_value, 935_000.0);
    }

    #[test]
    fn test_incompatible_units_rejected() {
        let reg = UnitRegistry::with_standard_units();
        let cycles = UnitId::from_digest(Digest::hash_blake3(b"cycles"));
        let bytes = UnitId::from_digest(Digest::hash_blake3(b"bytes"));
        let err = reg
            .add_values(
                cycles,
                &RationalOrFloat::Float(1.0),
                bytes,
                &RationalOrFloat::Float(2.0),
            )
            .unwrap_err();
        assert!(matches!(err, EconomicsError::IncompatibleUnits { .. }));
    }

    #[test]
    fn test_scalarization_does_not_mutate_observations() {
        let reg = UnitRegistry::with_standard_units();
        let cycles = UnitId::from_digest(Digest::hash_blake3(b"cycles"));
        let obs = UtilityObservation {
            subject: ResearchNodeId::from_digest(Digest::ZERO),
            evaluator: EvaluatorId::from_digest(Digest::ZERO),
            metric: MetricId::from_digest(Digest::hash_blake3(b"cycles")),
            value: RationalOrFloat::Rational {
                numerator: 1250,
                denominator: 1,
            },
            unit: cycles,
            direction: BetterDirection::HigherIsBetter,
            population: Digest::ZERO,
            evidence: Vec::new(),
            confidence: ConfidenceClass::ObservedExact,
            observed_at_generation: GenerationId::from_digest(Digest::ZERO),
            restricted_work: false,
        };
        let original = obs.value.clone();
        let policy = ScalarizationPolicy::default();
        let overhead = EconomicLedgerSummary::default();
        let _ = policy
            .scalarize(std::slice::from_ref(&obs), &reg, &overhead)
            .unwrap();
        assert_eq!(obs.value, original);
    }

    #[test]
    fn test_marginal_utility() {
        let reg = UnitRegistry::with_standard_units();
        let unit = UnitId::from_digest(Digest::hash_blake3(b"cycles"));
        let base = UtilityObservation {
            subject: ResearchNodeId::from_digest(Digest::ZERO),
            evaluator: EvaluatorId::from_digest(Digest::ZERO),
            metric: MetricId::from_digest(Digest::hash_blake3(b"cycles")),
            value: RationalOrFloat::Float(100.0),
            unit,
            direction: BetterDirection::HigherIsBetter,
            population: Digest::ZERO,
            evidence: Vec::new(),
            confidence: ConfidenceClass::ObservedExact,
            observed_at_generation: GenerationId::from_digest(Digest::ZERO),
            restricted_work: false,
        };
        let treatment = UtilityObservation {
            value: RationalOrFloat::Float(150.0),
            ..base.clone()
        };
        let delta = marginal_utility(&base, &treatment, &reg).unwrap();
        assert_eq!(delta.as_f64().unwrap(), 50.0);
    }

    #[test]
    fn test_unit_conversion_is_applied() {
        let mut reg = UnitRegistry::default();
        let seconds = UnitId::from_digest(Digest::hash_blake3(b"seconds"));
        let milliseconds = UnitId::from_digest(Digest::hash_blake3(b"milliseconds"));
        reg.register(seconds, "seconds", "time", 1.0).unwrap();
        reg.register(milliseconds, "milliseconds", "time", 0.001)
            .unwrap();
        let sum = reg
            .add_values(
                seconds,
                &RationalOrFloat::Float(1.0),
                milliseconds,
                &RationalOrFloat::Float(500.0),
            )
            .unwrap();
        assert_eq!(sum.as_f64().unwrap(), 1.5);
    }

    #[test]
    fn test_unknown_unit_rejected_even_when_ids_match() {
        let reg = UnitRegistry::default();
        let unknown = UnitId::from_digest(Digest::hash_blake3(b"unknown"));
        assert!(matches!(
            reg.add_values(
                unknown,
                &RationalOrFloat::Float(1.0),
                unknown,
                &RationalOrFloat::Float(1.0),
            ),
            Err(EconomicsError::UnknownUnit(_))
        ));
    }

    #[test]
    fn test_marginal_utility_requires_matched_population() {
        let reg = UnitRegistry::with_standard_units();
        let unit = UnitId::from_digest(Digest::hash_blake3(b"cycles"));
        let base = UtilityObservation {
            subject: ResearchNodeId::from_digest(Digest::ZERO),
            evaluator: EvaluatorId::from_digest(Digest::hash_blake3(b"eval")),
            metric: MetricId::from_digest(Digest::hash_blake3(b"cycles")),
            value: RationalOrFloat::Float(100.0),
            unit,
            direction: BetterDirection::LowerIsBetter,
            population: Digest::hash_blake3(b"population-a"),
            evidence: vec![],
            confidence: ConfidenceClass::ObservedExact,
            observed_at_generation: GenerationId::from_digest(Digest::ZERO),
            restricted_work: false,
        };
        let treatment = UtilityObservation {
            population: Digest::hash_blake3(b"population-b"),
            value: RationalOrFloat::Float(90.0),
            ..base.clone()
        };
        assert!(matches!(
            marginal_utility(&base, &treatment, &reg),
            Err(EconomicsError::ObservationMismatch {
                field: "population"
            })
        ));
    }

    #[test]
    fn test_lower_is_better_returns_positive_savings() {
        let reg = UnitRegistry::with_standard_units();
        let unit = UnitId::from_digest(Digest::hash_blake3(b"cycles"));
        let base = UtilityObservation {
            subject: ResearchNodeId::from_digest(Digest::ZERO),
            evaluator: EvaluatorId::from_digest(Digest::ZERO),
            metric: MetricId::from_digest(Digest::hash_blake3(b"cycles")),
            value: RationalOrFloat::Float(100.0),
            unit,
            direction: BetterDirection::LowerIsBetter,
            population: Digest::ZERO,
            evidence: vec![],
            confidence: ConfidenceClass::ObservedExact,
            observed_at_generation: GenerationId::from_digest(Digest::ZERO),
            restricted_work: false,
        };
        let treatment = UtilityObservation {
            value: RationalOrFloat::Float(90.0),
            ..base.clone()
        };
        assert_eq!(
            marginal_utility(&base, &treatment, &reg)
                .unwrap()
                .as_f64()
                .unwrap(),
            10.0
        );
    }

    #[test]
    fn test_overhead_overflow_fails_closed() {
        assert!(matches!(
            EconomicLedgerSummary::compute_net_savings(1.0, 1.0, u64::MAX, 1, 0),
            Err(EconomicsError::ArithmeticOverflow)
        ));
    }

    #[test]
    fn test_scalarization_rejects_invalid_policy_and_mixed_restricted_work() {
        let invalid = ScalarizationPolicy {
            ml_weight: f64::NAN,
            ..ScalarizationPolicy::default()
        };
        assert!(matches!(
            invalid.identity(),
            Err(EconomicsError::InvalidScalarization(_))
        ));

        let registry = UnitRegistry::with_standard_units();
        let cycles = UnitId::from_digest(Digest::hash_blake3(b"cycles"));
        let base = UtilityObservation {
            subject: ResearchNodeId::from_digest(Digest::hash_blake3(b"subject")),
            evaluator: EvaluatorId::from_digest(Digest::hash_blake3(b"evaluator")),
            metric: MetricId::from_digest(Digest::hash_blake3(b"metric")),
            value: RationalOrFloat::Float(1.0),
            unit: cycles,
            direction: BetterDirection::HigherIsBetter,
            population: Digest::hash_blake3(b"population"),
            evidence: vec![Digest::hash_blake3(b"evidence")],
            confidence: ConfidenceClass::ObservedExact,
            observed_at_generation: GenerationId::from_digest(Digest::hash_blake3(b"generation")),
            restricted_work: false,
        };
        let restricted = UtilityObservation {
            restricted_work: true,
            ..base.clone()
        };
        assert!(matches!(
            ScalarizationPolicy::default().scalarize(
                &[base, restricted],
                &registry,
                &EconomicLedgerSummary::default()
            ),
            Err(EconomicsError::ObservationMismatch {
                field: "restricted_work"
            })
        ));
    }

    #[test]
    fn test_unit_identity_cannot_be_redefined() {
        let mut registry = UnitRegistry::default();
        let unit = UnitId::from_digest(Digest::hash_blake3(b"fixed-unit"));
        registry.register(unit, "ticks", "compute", 1.0).unwrap();
        assert!(matches!(
            registry.register(unit, "seconds", "time", 1.0),
            Err(EconomicsError::UnitIdentityConflict(_))
        ));
    }
}
