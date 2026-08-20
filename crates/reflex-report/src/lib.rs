use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter, content_id};
use reflex_economics::EconomicLedgerSummary;
use reflex_eval::{EvaluationReport, SliceMetrics, StateErrorSlice};
use reflex_types::{Digest, ExperimentId, UnitId};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use thiserror::Error;

pub const REPORT_SCHEMA: &str = "reflex.scientific-report.v1";
pub const EVIDENCE_SCHEMA: &str = "reflex.report-evidence.v1";
const MAX_SOURCE_ARTIFACTS: usize = 4096;
const MAX_TEXT_BYTES: usize = 512;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ReportError {
    #[error("missing required evidence: {0}")]
    MissingEvidence(String),
    #[error("report reconstruction mismatch")]
    ReconstructionMismatch,
    #[error("invalid report input: {0}")]
    InvalidInput(String),
    #[error("report serialization failed: {0}")]
    Serialization(String),
}

impl From<CanonicalError> for ReportError {
    fn from(error: CanonicalError) -> Self {
        Self::Serialization(error.to_string())
    }
}

/// Immutable inputs used by the checked queries that reconstructed a report.
/// A report without this manifest is not a scientific report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReportEvidenceManifest {
    pub schema: String,
    pub source_artifacts: Vec<Digest>,
    pub query_plan: Digest,
    pub unit_registry_artifact: Digest,
    pub cell_result_artifact: Digest,
    pub evaluation_result_artifact: Digest,
    pub economics_result_artifact: Digest,
    pub cell_query_id: String,
    pub evaluation_query_id: String,
    pub economics_query_id: String,
    pub cell_population: Digest,
    pub cell_population_size: u64,
    pub evaluation_population: Digest,
    pub evaluation_population_size: u64,
    pub economics_population: Digest,
    pub economics_population_size: u64,
    pub cpu_unit: UnitId,
    pub cpu_unit_symbol: String,
    pub cpu_unit_dimension: String,
    pub cpu_unit_scale: f64,
    pub economics_unit: UnitId,
    pub economics_unit_symbol: String,
    pub economics_unit_dimension: String,
    pub economics_unit_scale: f64,
}

impl ReportEvidenceManifest {
    pub fn validate(&self) -> Result<(), ReportError> {
        if self.schema != EVIDENCE_SCHEMA {
            return Err(ReportError::InvalidInput(format!(
                "evidence schema must be {EVIDENCE_SCHEMA}"
            )));
        }
        if self.source_artifacts.is_empty() {
            return Err(ReportError::MissingEvidence(
                "at least one immutable source artifact is required".into(),
            ));
        }
        if self.source_artifacts.len() > MAX_SOURCE_ARTIFACTS {
            return Err(ReportError::InvalidInput(
                "source artifact list exceeds 4096".into(),
            ));
        }
        let mut unique = BTreeSet::new();
        let mut previous = None;
        for digest in &self.source_artifacts {
            require_digest("source_artifacts", digest)?;
            if previous.is_some_and(|previous| previous >= *digest) || !unique.insert(*digest) {
                return Err(ReportError::InvalidInput(
                    "source artifact list must be strictly sorted and unique".into(),
                ));
            }
            previous = Some(*digest);
        }
        require_digest("query_plan", &self.query_plan)?;
        require_digest("unit_registry_artifact", &self.unit_registry_artifact)?;
        for (field, digest) in [
            ("query_plan", self.query_plan),
            ("unit_registry_artifact", self.unit_registry_artifact),
            ("cell_result_artifact", self.cell_result_artifact),
            (
                "evaluation_result_artifact",
                self.evaluation_result_artifact,
            ),
            ("economics_result_artifact", self.economics_result_artifact),
        ] {
            require_digest(field, &digest)?;
            if !unique.contains(&digest) {
                return Err(ReportError::MissingEvidence(format!(
                    "{field} is not present in source_artifacts"
                )));
            }
        }
        require_digest("cell_population", &self.cell_population)?;
        require_digest("evaluation_population", &self.evaluation_population)?;
        require_digest("economics_population", &self.economics_population)?;
        if self.cell_population_size == 0
            || self.evaluation_population_size == 0
            || self.economics_population_size == 0
        {
            return Err(ReportError::MissingEvidence(
                "cell, evaluation, and economics populations must be nonempty".into(),
            ));
        }
        require_digest("cpu_unit", self.cpu_unit.digest())?;
        require_digest("economics_unit", self.economics_unit.digest())?;
        for (field, value) in [
            ("cell_query_id", self.cell_query_id.as_str()),
            ("evaluation_query_id", self.evaluation_query_id.as_str()),
            ("economics_query_id", self.economics_query_id.as_str()),
            ("cpu_unit_symbol", self.cpu_unit_symbol.as_str()),
            ("cpu_unit_dimension", self.cpu_unit_dimension.as_str()),
            ("economics_unit_symbol", self.economics_unit_symbol.as_str()),
            (
                "economics_unit_dimension",
                self.economics_unit_dimension.as_str(),
            ),
        ] {
            require_text(field, value)?;
        }
        if BTreeSet::from([
            self.cell_query_id.as_str(),
            self.evaluation_query_id.as_str(),
            self.economics_query_id.as_str(),
        ])
        .len()
            != 3
        {
            return Err(ReportError::InvalidInput(
                "cell, evaluation, and economics query IDs must be distinct".into(),
            ));
        }
        if !self.cpu_unit_scale.is_finite()
            || self.cpu_unit_scale <= 0.0
            || !self.economics_unit_scale.is_finite()
            || self.economics_unit_scale <= 0.0
        {
            return Err(ReportError::InvalidInput(
                "unit scales must be finite and strictly positive".into(),
            ));
        }
        let cycles = UnitId::from_digest(Digest::hash_blake3(b"cycles"));
        if self.economics_unit != cycles
            || self.economics_unit_symbol != "cycles"
            || self.economics_unit_dimension != "compute"
            || self.economics_unit_scale.to_bits() != 1.0_f64.to_bits()
        {
            return Err(ReportError::InvalidInput(
                "EconomicLedgerSummary values are defined in the canonical cycles unit".into(),
            ));
        }
        Ok(())
    }

    pub fn identity(&self) -> Result<Digest, ReportError> {
        self.validate()?;
        Ok(content_id(b"reflex.report-evidence.v1", self)?)
    }
}

impl CanonicalEncode for ReportEvidenceManifest {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.schema)?;
        out.write_vec(&self.source_artifacts)?;
        out.write_digest(&self.query_plan)?;
        out.write_digest(&self.unit_registry_artifact)?;
        out.write_digest(&self.cell_result_artifact)?;
        out.write_digest(&self.evaluation_result_artifact)?;
        out.write_digest(&self.economics_result_artifact)?;
        out.write_str(&self.cell_query_id)?;
        out.write_str(&self.evaluation_query_id)?;
        out.write_str(&self.economics_query_id)?;
        out.write_digest(&self.cell_population)?;
        out.write_u64(self.cell_population_size)?;
        out.write_digest(&self.evaluation_population)?;
        out.write_u64(self.evaluation_population_size)?;
        out.write_digest(&self.economics_population)?;
        out.write_u64(self.economics_population_size)?;
        out.write_digest(self.cpu_unit.digest())?;
        out.write_str(&self.cpu_unit_symbol)?;
        out.write_str(&self.cpu_unit_dimension)?;
        out.write_f64(self.cpu_unit_scale)?;
        out.write_digest(self.economics_unit.digest())?;
        out.write_str(&self.economics_unit_symbol)?;
        out.write_str(&self.economics_unit_dimension)?;
        out.write_f64(self.economics_unit_scale)
    }
}

/// Mutually exclusive reconstructed cell outcomes. Censored and incomplete
/// cells remain explicit and are never collapsed into negative labels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellOutcomeCounts {
    pub solved: u64,
    pub completed_without_solution: u64,
    pub censored: u64,
    pub incomplete: u64,
}

impl CellOutcomeCounts {
    pub fn total(self) -> Result<u64, ReportError> {
        self.solved
            .checked_add(self.completed_without_solution)
            .and_then(|value| value.checked_add(self.censored))
            .and_then(|value| value.checked_add(self.incomplete))
            .ok_or_else(|| ReportError::InvalidInput("cell outcome count overflow".into()))
    }
}

impl CanonicalEncode for CellOutcomeCounts {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u64(self.solved)?;
        out.write_u64(self.completed_without_solution)?;
        out.write_u64(self.censored)?;
        out.write_u64(self.incomplete)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScientificReport {
    pub schema: String,
    pub evidence: ReportEvidenceManifest,
    pub evidence_manifest_digest: Digest,
    pub experiment_id: ExperimentId,
    pub title: String,
    pub domain: String,
    pub outcomes: CellOutcomeCounts,
    pub total_cells: u64,
    pub solved_cells: u64,
    pub censored_cells: u64,
    pub incomplete_cells: u64,
    pub solve_rate: f64,
    pub total_cpu: f64,
    pub total_ml_overhead: f64,
    pub ml_overhead_share: f64,
    pub evaluation: EvaluationReport,
    pub economics: EconomicLedgerSummary,
    pub report_digest: Digest,
}

#[derive(Clone, Debug)]
pub struct ScientificReportParams<'a> {
    pub evidence: ReportEvidenceManifest,
    pub experiment_id: ExperimentId,
    pub title: &'a str,
    pub domain: &'a str,
    pub outcomes: CellOutcomeCounts,
    pub total_cpu: f64,
    pub total_ml_overhead: f64,
    pub evaluation: EvaluationReport,
    pub economics: EconomicLedgerSummary,
}

impl ScientificReport {
    pub fn rebuild_from_evidence(params: ScientificReportParams<'_>) -> Result<Self, ReportError> {
        Self::build(params)
    }

    pub fn verify_reconstruction(
        &self,
        params: ScientificReportParams<'_>,
    ) -> Result<(), ReportError> {
        if self.compute_digest()? != self.report_digest
            || self.evidence.identity()? != self.evidence_manifest_digest
        {
            return Err(ReportError::ReconstructionMismatch);
        }
        let rebuilt = Self::build(params)?;
        let current = serde_json::to_vec(self)
            .map_err(|error| ReportError::Serialization(error.to_string()))?;
        let expected = serde_json::to_vec(&rebuilt)
            .map_err(|error| ReportError::Serialization(error.to_string()))?;
        if current != expected {
            return Err(ReportError::ReconstructionMismatch);
        }
        Ok(())
    }

    pub fn build(params: ScientificReportParams<'_>) -> Result<Self, ReportError> {
        params.evidence.validate()?;
        require_digest("experiment_id", params.experiment_id.digest())?;
        require_text("title", params.title)?;
        require_text("domain", params.domain)?;
        let total_cells = params.outcomes.total()?;
        if total_cells == 0 {
            return Err(ReportError::MissingEvidence(
                "cell outcome population is empty".into(),
            ));
        }
        if params.evidence.cell_population_size != total_cells
            || params.evidence.evaluation_population_size
                != u64::try_from(params.evaluation.total_groups).map_err(|_| {
                    ReportError::InvalidInput("evaluation population exceeds u64".into())
                })?
        {
            return Err(ReportError::InvalidInput(
                "reconstructed outcome/evaluation counts do not match their declared populations"
                    .into(),
            ));
        }
        validate_measures(params.total_cpu, params.total_ml_overhead)?;
        validate_evaluation(&params.evaluation)?;
        validate_economics(&params.economics)?;
        validate_reconstructed_outputs(&params)?;
        let solve_rate = params.outcomes.solved as f64 / total_cells as f64;
        let ml_overhead_share = if params.total_cpu > 0.0 {
            params.total_ml_overhead / params.total_cpu
        } else {
            0.0
        };
        let evidence_manifest_digest = params.evidence.identity()?;
        let mut report = Self {
            schema: REPORT_SCHEMA.into(),
            evidence: params.evidence,
            evidence_manifest_digest,
            experiment_id: params.experiment_id,
            title: params.title.to_string(),
            domain: params.domain.to_string(),
            outcomes: params.outcomes,
            total_cells,
            solved_cells: params.outcomes.solved,
            censored_cells: params.outcomes.censored,
            incomplete_cells: params.outcomes.incomplete,
            solve_rate,
            total_cpu: params.total_cpu,
            total_ml_overhead: params.total_ml_overhead,
            ml_overhead_share,
            evaluation: params.evaluation,
            economics: params.economics,
            report_digest: Digest::ZERO,
        };
        report.report_digest = report.compute_digest()?;
        Ok(report)
    }

    fn compute_digest(&self) -> Result<Digest, ReportError> {
        Ok(content_id(
            b"reflex.scientific-report.v1",
            &ReportIdentity(self),
        )?)
    }

    pub fn to_markdown(&self) -> String {
        format!(
            "# Reflex Scientific Report: {}\n\n- **Experiment ID:** {}\n- **Domain:** {}\n- **Cell Population:** {}\n- **Evaluation Population:** {}\n- **Economics Population:** {}\n- **Solve Rate:** {:.2}% ({}/{})\n- **Censored / Incomplete:** {} / {}\n- **Total CPU:** {:.2} {} (ML Overhead: {:.2}% / {:.2} {})\n- **Top-1 Viable Rate:** {:.2}%\n- **MRR Cheapest Route:** {:.3}\n- **Net Economics:** {:.2} {}\n- **Evidence Manifest:** {}\n- **Report Digest:** {}\n",
            self.title,
            self.experiment_id.to_hex(),
            self.domain,
            self.evidence.cell_population,
            self.evidence.evaluation_population,
            self.evidence.economics_population,
            self.solve_rate * 100.0,
            self.solved_cells,
            self.total_cells,
            self.censored_cells,
            self.incomplete_cells,
            self.total_cpu,
            self.evidence.cpu_unit_symbol,
            self.ml_overhead_share * 100.0,
            self.total_ml_overhead,
            self.evidence.cpu_unit_symbol,
            self.evaluation.top1_viable_rate * 100.0,
            self.evaluation.mrr_cheapest_route,
            self.economics.net_utility_value,
            self.evidence.economics_unit_symbol,
            self.evidence_manifest_digest,
            self.report_digest,
        )
    }

    pub fn to_canonical_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

struct ReportIdentity<'a>(&'a ScientificReport);

impl CanonicalEncode for ReportIdentity<'_> {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        let report = self.0;
        out.write_str(&report.schema)?;
        report.evidence.encode_canonical(out)?;
        out.write_digest(&report.evidence_manifest_digest)?;
        out.write_digest(report.experiment_id.digest())?;
        out.write_str(&report.title)?;
        out.write_str(&report.domain)?;
        report.outcomes.encode_canonical(out)?;
        out.write_u64(report.total_cells)?;
        out.write_u64(report.solved_cells)?;
        out.write_u64(report.censored_cells)?;
        out.write_u64(report.incomplete_cells)?;
        out.write_f64(report.solve_rate)?;
        out.write_f64(report.total_cpu)?;
        out.write_f64(report.total_ml_overhead)?;
        out.write_f64(report.ml_overhead_share)?;
        encode_evaluation(&report.evaluation, out)?;
        encode_economics(&report.economics, out)
    }
}

fn encode_evaluation(
    report: &EvaluationReport,
    out: &mut CanonicalWriter,
) -> Result<(), CanonicalError> {
    out.write_u64(report.total_groups as u64)?;
    for value in [
        report.top1_viable_rate,
        report.top3_viable_recall,
        report.mrr_cheapest_route,
        report.pairwise_viable_accuracy,
        report.ndcg,
        report.score_entropy,
        report.mean_score_margin,
        report.calibration_brier,
        report.candidate_diversity,
        report.oracle_ceiling,
    ] {
        out.write_f32(value)?;
    }
    out.write_u64(report.feature_collisions_detected as u64)?;
    out.write_u64(report.semantic_groups as u64)?;
    out.write_u32(report.coverage_breakdown.len() as u32)?;
    for (name, metrics) in &report.coverage_breakdown {
        out.write_str(name)?;
        encode_slice_metrics(metrics, out)?;
    }
    out.write_u32(report.state_slices.len() as u32)?;
    for slice in &report.state_slices {
        encode_state_slice(slice, out)?;
    }
    Ok(())
}

fn encode_slice_metrics(
    metrics: &SliceMetrics,
    out: &mut CanonicalWriter,
) -> Result<(), CanonicalError> {
    out.write_u64(metrics.groups as u64)?;
    out.write_u64(metrics.groups_with_viable_route as u64)?;
    out.write_u64(metrics.semantic_groups as u64)?;
    out.write_f32(metrics.top1_viable_rate)?;
    out.write_f32(metrics.ndcg)?;
    out.write_f32(metrics.score_entropy)
}

fn encode_state_slice(
    slice: &StateErrorSlice,
    out: &mut CanonicalWriter,
) -> Result<(), CanonicalError> {
    out.write_digest(slice.state_id.digest())?;
    out.write_str(&slice.coverage)?;
    out.write_u64(slice.candidate_count as u64)?;
    out.write_u64(slice.known_candidate_count as u64)?;
    out.write_u64(slice.unknown_candidate_count as u64)?;
    out.write_bool(slice.has_viable_route)?;
    out.write_option(slice.top1_viable.as_ref())?;
    out.write_option(slice.ndcg.as_ref())?;
    out.write_f32(slice.score_margin)?;
    out.write_f32(slice.score_entropy)
}

fn encode_economics(
    summary: &EconomicLedgerSummary,
    out: &mut CanonicalWriter,
) -> Result<(), CanonicalError> {
    out.write_f64(summary.gross_savings_proxy)?;
    out.write_u64(summary.ml_inference_tax_cpu_ns)?;
    out.write_u64(summary.feature_extraction_tax_cpu_ns)?;
    out.write_u64(summary.retrieval_tax_cpu_ns)?;
    out.write_f64(summary.net_utility_value)?;
    out.write_i64(summary.verified_actions_saved)
}

fn validate_measures(total_cpu: f64, ml_overhead: f64) -> Result<(), ReportError> {
    if !total_cpu.is_finite()
        || !ml_overhead.is_finite()
        || total_cpu < 0.0
        || ml_overhead < 0.0
        || ml_overhead > total_cpu
    {
        return Err(ReportError::InvalidInput(
            "CPU totals must be finite, nonnegative, and internally consistent".into(),
        ));
    }
    Ok(())
}

fn validate_evaluation(report: &EvaluationReport) -> Result<(), ReportError> {
    if report.total_groups == 0 || report.semantic_groups > report.total_groups {
        return Err(ReportError::MissingEvidence(
            "evaluation report must name a nonempty, internally consistent held-out population"
                .into(),
        ));
    }
    reflex_canonical::encode_to_vec(&EvaluationIdentity(report))?;
    Ok(())
}

struct EvaluationIdentity<'a>(&'a EvaluationReport);
impl CanonicalEncode for EvaluationIdentity<'_> {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        encode_evaluation(self.0, out)
    }
}

fn validate_economics(summary: &EconomicLedgerSummary) -> Result<(), ReportError> {
    if !summary.gross_savings_proxy.is_finite() || !summary.net_utility_value.is_finite() {
        return Err(ReportError::InvalidInput(
            "economics values must be finite".into(),
        ));
    }
    Ok(())
}

fn validate_reconstructed_outputs(params: &ScientificReportParams<'_>) -> Result<(), ReportError> {
    let expected_cell =
        cell_query_result_digest(params.outcomes, params.total_cpu, params.total_ml_overhead)?;
    let expected_evaluation = evaluation_query_result_digest(&params.evaluation)?;
    let expected_economics = economics_query_result_digest(&params.economics)?;
    if params.evidence.cell_result_artifact != expected_cell
        || params.evidence.evaluation_result_artifact != expected_evaluation
        || params.evidence.economics_result_artifact != expected_economics
    {
        return Err(ReportError::ReconstructionMismatch);
    }
    Ok(())
}

/// Canonical identity of the reconstructed cell/resource query output.
pub fn cell_query_result_digest(
    outcomes: CellOutcomeCounts,
    total_cpu: f64,
    total_ml_overhead: f64,
) -> Result<Digest, ReportError> {
    validate_measures(total_cpu, total_ml_overhead)?;
    Ok(content_id(
        b"reflex.report-cell-query-result.v1",
        &CellQueryResultIdentity {
            outcomes,
            total_cpu,
            total_ml_overhead,
        },
    )?)
}

/// Canonical identity of the reconstructed evaluation query output.
pub fn evaluation_query_result_digest(report: &EvaluationReport) -> Result<Digest, ReportError> {
    validate_evaluation(report)?;
    Ok(content_id(
        b"reflex.report-evaluation-query-result.v1",
        &EvaluationIdentity(report),
    )?)
}

/// Canonical identity of the reconstructed economic query output.
pub fn economics_query_result_digest(
    summary: &EconomicLedgerSummary,
) -> Result<Digest, ReportError> {
    validate_economics(summary)?;
    Ok(content_id(
        b"reflex.report-economics-query-result.v1",
        &EconomicsIdentity(summary),
    )?)
}

struct CellQueryResultIdentity {
    outcomes: CellOutcomeCounts,
    total_cpu: f64,
    total_ml_overhead: f64,
}

impl CanonicalEncode for CellQueryResultIdentity {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        self.outcomes.encode_canonical(out)?;
        out.write_f64(self.total_cpu)?;
        out.write_f64(self.total_ml_overhead)
    }
}

struct EconomicsIdentity<'a>(&'a EconomicLedgerSummary);

impl CanonicalEncode for EconomicsIdentity<'_> {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        encode_economics(self.0, out)
    }
}

fn require_digest(field: &str, digest: &Digest) -> Result<(), ReportError> {
    if *digest == Digest::ZERO {
        return Err(ReportError::MissingEvidence(format!(
            "{field} has an unpinned zero digest"
        )));
    }
    Ok(())
}

fn require_text(field: &str, value: &str) -> Result<(), ReportError> {
    if value.trim().is_empty()
        || value.len() > MAX_TEXT_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(ReportError::InvalidInput(format!(
            "{field} must be non-empty, bounded, and contain no control characters"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> ReportEvidenceManifest {
        let cell_result = cell_query_result_digest(
            CellOutcomeCounts {
                solved: 75,
                completed_without_solution: 20,
                censored: 3,
                incomplete: 2,
            },
            120.0,
            6.0,
        )
        .unwrap();
        let evaluation_result = evaluation_query_result_digest(&EvaluationReport {
            total_groups: 100,
            top1_viable_rate: 0.85,
            mrr_cheapest_route: 0.90,
            ..Default::default()
        })
        .unwrap();
        let economics_result = economics_query_result_digest(&EconomicLedgerSummary {
            net_utility_value: 50_000.0,
            ..Default::default()
        })
        .unwrap();
        let query_plan = Digest::hash_blake3(b"checked-query-plan");
        let unit_registry = Digest::hash_blake3(b"unit-registry");
        let mut source_artifacts = vec![
            cell_result,
            evaluation_result,
            economics_result,
            query_plan,
            unit_registry,
        ];
        source_artifacts.sort_unstable();
        ReportEvidenceManifest {
            schema: EVIDENCE_SCHEMA.into(),
            source_artifacts,
            query_plan,
            unit_registry_artifact: unit_registry,
            cell_result_artifact: cell_result,
            evaluation_result_artifact: evaluation_result,
            economics_result_artifact: economics_result,
            cell_query_id: "cells-v1".into(),
            evaluation_query_id: "heldout-evaluation-v1".into(),
            economics_query_id: "matched-economics-v1".into(),
            cell_population: Digest::hash_blake3(b"cell-population"),
            cell_population_size: 100,
            evaluation_population: Digest::hash_blake3(b"heldout-population"),
            evaluation_population_size: 100,
            economics_population: Digest::hash_blake3(b"matched-population"),
            economics_population_size: 100,
            cpu_unit: UnitId::from_digest(Digest::hash_blake3(b"cpu-seconds")),
            cpu_unit_symbol: "s".into(),
            cpu_unit_dimension: "time".into(),
            cpu_unit_scale: 1.0,
            economics_unit: UnitId::from_digest(Digest::hash_blake3(b"cycles")),
            economics_unit_symbol: "cycles".into(),
            economics_unit_dimension: "compute".into(),
            economics_unit_scale: 1.0,
        }
    }

    fn params() -> ScientificReportParams<'static> {
        ScientificReportParams {
            evidence: evidence(),
            experiment_id: ExperimentId::from_digest(Digest::hash_blake3(b"exp")),
            title: "Bitvec Generation 1",
            domain: "bitvec",
            outcomes: CellOutcomeCounts {
                solved: 75,
                completed_without_solution: 20,
                censored: 3,
                incomplete: 2,
            },
            total_cpu: 120.0,
            total_ml_overhead: 6.0,
            evaluation: EvaluationReport {
                total_groups: 100,
                top1_viable_rate: 0.85,
                mrr_cheapest_route: 0.90,
                ..Default::default()
            },
            economics: EconomicLedgerSummary {
                net_utility_value: 50_000.0,
                ..Default::default()
            },
        }
    }

    #[test]
    fn scientific_report_is_evidence_bound_and_unit_correct() {
        let report = ScientificReport::build(params()).unwrap();
        assert_eq!(report.solve_rate, 0.75);
        assert_eq!(report.censored_cells, 3);
        assert_eq!(report.incomplete_cells, 2);
        assert_eq!(report.ml_overhead_share, 0.05);
        let markdown = report.to_markdown();
        assert!(markdown.contains("50000.00 cycles"));
        report.verify_reconstruction(params()).unwrap();
    }

    #[test]
    fn missing_or_mutated_evidence_fails_closed() {
        let mut invalid = params();
        invalid.evidence.source_artifacts.clear();
        assert!(matches!(
            ScientificReport::build(invalid),
            Err(ReportError::MissingEvidence(_))
        ));
        let mut report = ScientificReport::build(params()).unwrap();
        report.evaluation.oracle_ceiling = 0.75;
        assert_eq!(
            report.verify_reconstruction(params()),
            Err(ReportError::ReconstructionMismatch)
        );
    }

    #[test]
    fn evidence_identity_changes_for_query_population_unit_and_source() {
        let baseline = evidence();
        let baseline_id = baseline.identity().unwrap();
        let mut variants = Vec::new();
        let mut changed = baseline.clone();
        changed.query_plan = Digest::hash_blake3(b"different-query");
        let position = changed
            .source_artifacts
            .iter()
            .position(|digest| *digest == baseline.query_plan)
            .unwrap();
        changed.source_artifacts[position] = changed.query_plan;
        changed.source_artifacts.sort_unstable();
        variants.push(changed);
        let mut changed = baseline.clone();
        changed.evaluation_population = Digest::hash_blake3(b"different-population");
        variants.push(changed);
        let mut changed = baseline.clone();
        changed.cpu_unit = UnitId::from_digest(Digest::hash_blake3(b"different-unit"));
        variants.push(changed);
        let mut changed = baseline.clone();
        let position = changed
            .source_artifacts
            .iter()
            .position(|digest| *digest == baseline.cell_result_artifact)
            .unwrap();
        changed.source_artifacts[position] = Digest::hash_blake3(b"different-source");
        changed.cell_result_artifact = changed.source_artifacts[position];
        changed.source_artifacts.sort_unstable();
        variants.push(changed);
        assert!(
            variants
                .iter()
                .all(|variant| variant.identity().unwrap() != baseline_id)
        );
        let mut mislabeled = baseline;
        mislabeled.economics_unit_symbol = "actions".into();
        assert!(mislabeled.validate().is_err());
    }
}
