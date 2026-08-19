use reflex_economics::EconomicLedgerSummary;
use reflex_eval::EvaluationReport;
use reflex_types::{Digest, ExperimentId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    #[error("missing required evidence: {0}")]
    MissingEvidence(String),
    #[error("report reconstruction mismatch")]
    ReconstructionMismatch,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScientificReport {
    pub experiment_id: ExperimentId,
    pub title: String,
    pub domain: String,
    pub total_cells: usize,
    pub solved_cells: usize,
    pub solve_rate: f64,
    pub total_cpu_seconds: f64,
    pub total_ml_overhead_seconds: f64,
    pub ml_overhead_share: f64,
    pub evaluation: EvaluationReport,
    pub economics: EconomicLedgerSummary,
    pub report_digest: Digest,
}

#[derive(Clone, Debug)]
pub struct ScientificReportParams<'a> {
    pub experiment_id: ExperimentId,
    pub title: &'a str,
    pub domain: &'a str,
    pub total_cells: usize,
    pub solved_cells: usize,
    pub total_cpu_seconds: f64,
    pub total_ml_overhead_seconds: f64,
    pub evaluation: EvaluationReport,
    pub economics: EconomicLedgerSummary,
}

impl ScientificReport {
    pub fn build(params: ScientificReportParams<'_>) -> Self {
        let solve_rate = if params.total_cells > 0 {
            params.solved_cells as f64 / params.total_cells as f64
        } else {
            0.0
        };
        let ml_overhead_share = if params.total_cpu_seconds > 0.0 {
            params.total_ml_overhead_seconds / params.total_cpu_seconds
        } else {
            0.0
        };

        let mut data = Vec::new();
        data.extend_from_slice(params.experiment_id.digest().as_bytes());
        data.extend_from_slice(params.title.as_bytes());
        data.extend_from_slice(params.domain.as_bytes());
        data.extend_from_slice(&params.total_cells.to_le_bytes());
        data.extend_from_slice(&params.solved_cells.to_le_bytes());
        data.extend_from_slice(&params.total_cpu_seconds.to_le_bytes());
        data.extend_from_slice(&params.total_ml_overhead_seconds.to_le_bytes());
        data.extend_from_slice(&params.evaluation.top1_viable_rate.to_le_bytes());
        data.extend_from_slice(&params.evaluation.mrr_cheapest_route.to_le_bytes());
        data.extend_from_slice(&params.economics.net_utility_value.to_le_bytes());

        let report_digest = Digest::hash_blake3(&data);

        Self {
            experiment_id: params.experiment_id,
            title: params.title.to_string(),
            domain: params.domain.to_string(),
            total_cells: params.total_cells,
            solved_cells: params.solved_cells,
            solve_rate,
            total_cpu_seconds: params.total_cpu_seconds,
            total_ml_overhead_seconds: params.total_ml_overhead_seconds,
            ml_overhead_share,
            evaluation: params.evaluation,
            economics: params.economics,
            report_digest,
        }
    }

    pub fn to_markdown(&self) -> String {
        format!(
            r#"# Reflex Scientific Report: {}

- **Experiment ID:** {}
- **Domain:** {}
- **Solve Rate:** {:.2}% ({}/{})
- **Total CPU:** {:.2}s (ML Overhead: {:.2}% / {:.2}s)
- **Top-1 Viable Rate:** {:.2}%
- **MRR Cheapest Route:** {:.3}
- **Net Economics Savings:** {:.2} cycles
- **Report Digest:** {}
"#,
            self.title,
            self.experiment_id.to_hex(),
            self.domain,
            self.solve_rate * 100.0,
            self.solved_cells,
            self.total_cells,
            self.total_cpu_seconds,
            self.ml_overhead_share * 100.0,
            self.total_ml_overhead_seconds,
            self.evaluation.top1_viable_rate * 100.0,
            self.evaluation.mrr_cheapest_route,
            self.economics.net_utility_value,
            self.report_digest.to_hex()
        )
    }

    pub fn to_canonical_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scientific_report_generation() {
        let exp_id = ExperimentId::from_digest(Digest::hash_blake3(b"exp"));
        let report = ScientificReport::build(ScientificReportParams {
            experiment_id: exp_id,
            title: "Bitvec Generation 1",
            domain: "bitvec",
            total_cells: 100,
            solved_cells: 75,
            total_cpu_seconds: 120.0,
            total_ml_overhead_seconds: 6.0,
            evaluation: EvaluationReport {
                top1_viable_rate: 0.85,
                mrr_cheapest_route: 0.90,
                ..Default::default()
            },
            economics: EconomicLedgerSummary {
                net_utility_value: 50000.0,
                ..Default::default()
            },
        });

        assert_eq!(report.solve_rate, 0.75);
        assert_eq!(report.ml_overhead_share, 0.05);
        let md = report.to_markdown();
        assert!(md.contains("Bitvec Generation 1"));
    }
}
