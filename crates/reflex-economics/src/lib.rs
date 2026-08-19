use reflex_types::{Digest, EvaluatorId, GenerationId, MetricId, ResearchNodeId, UnitId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum EconomicsError {
    #[error("incompatible units: cannot combine {unit_a} and {unit_b}")]
    IncompatibleUnits { unit_a: String, unit_b: String },
    #[error("division by zero or non-finite utility calculation")]
    NonFiniteUtility,
    #[error("missing economic baseline for subject {0}")]
    MissingBaseline(String),
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UtilityObservation {
    pub subject: ResearchNodeId,
    pub evaluator: EvaluatorId,
    pub metric: MetricId,
    pub value: f64,
    pub unit: UnitId,
    pub direction: BetterDirection,
    pub population: Digest,
    pub evidence: Vec<Digest>,
    pub confidence: ConfidenceClass,
    pub observed_at_generation: GenerationId,
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
    ) -> Self {
        let total_overhead_ns = ml_inference_cpu_ns + feature_cpu_ns + retrieval_cpu_ns;
        let overhead_in_cycles = (total_overhead_ns as f64) * cpu_ns_to_cycle_ratio;
        let net_utility = gross_cycles_saved - overhead_in_cycles;

        Self {
            gross_savings_proxy: gross_cycles_saved,
            ml_inference_tax_cpu_ns: ml_inference_cpu_ns,
            feature_extraction_tax_cpu_ns: feature_cpu_ns,
            retrieval_tax_cpu_ns: retrieval_cpu_ns,
            net_utility_value: net_utility,
            verified_actions_saved: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_economic_accounting_net_savings() {
        let summary = EconomicLedgerSummary::compute_net_savings(
            1_000_000.0, // 1M cycles saved
            1.0,         // 1 ns = 1 cycle
            50_000,      // 50k ns ml inference
            10_000,      // 10k ns features
            5_000,       // 5k ns retrieval
        );

        assert_eq!(summary.gross_savings_proxy, 1_000_000.0);
        assert_eq!(summary.net_utility_value, 935_000.0);
    }
}
