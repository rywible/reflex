use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use reflex_eval::EvaluationReport;
pub use reflex_meta::DurableGenerationState as GenerationState;
use reflex_meta::{
    CancellationMode, GenerationArtifactRef, GenerationArtifactRole, GenerationCancellationRequest,
    GenerationCancellationResult, GenerationPromotionRequest, GenerationPromotionResult,
    GenerationReconcileRequest, GenerationReconcileResult, GenerationRecord,
    GenerationTransitionCommand, MetaError, MetaStore, NewGenerationRecord, PromotionRequest,
};
use reflex_types::{
    ActionSchemaId, CandidateId, CellId, CompatibilityDigest, Digest, EvaluatorId, ExperimentId,
    FeatureSchemaId, GenerationId, KnowledgeEditionId, ModelCheckpointId, ModelRole, TaskId,
    VerifierId,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use thiserror::Error;

mod turmoil_tests;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum SchedulerError {
    #[error("meta error: {0}")]
    Meta(#[from] MetaError),
    #[error("invalid state transition from {from:?} to {to:?}")]
    InvalidTransition {
        from: GenerationState,
        to: GenerationState,
    },
    #[error("stop condition reached: {0}")]
    Stopped(String),
    #[error("invalid orchestration input: {0}")]
    InvalidInput(String),
    #[error("promotion rejected: {0}")]
    PromotionRejected(String),
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    #[error("canonical manifest encoding failed: {0}")]
    Canonical(#[from] CanonicalError),
    #[error("invalid immutable manifest field `{field}`: {reason}")]
    InvalidField { field: &'static str, reason: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchConfig {
    pub algorithm: String,
    pub action_budget: u32,
    pub node_budget: u32,
    pub cpu_seconds: f64,
    pub exploration_uniform: f32,
}

impl CanonicalEncode for SearchConfig {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.algorithm)?;
        out.write_u32(self.action_budget)?;
        out.write_u32(self.node_budget)?;
        out.write_f64(self.cpu_seconds)?;
        out.write_f32(self.exploration_uniform)?;
        Ok(())
    }
}

impl SearchConfig {
    fn validate(&self) -> Result<(), ManifestError> {
        validate_pinned_name("search.algorithm", &self.algorithm)?;
        if self.action_budget == 0 {
            return Err(invalid_field("search.action_budget", "must be non-zero"));
        }
        if self.node_budget == 0 {
            return Err(invalid_field("search.node_budget", "must be non-zero"));
        }
        if !self.cpu_seconds.is_finite() || self.cpu_seconds <= 0.0 {
            return Err(invalid_field(
                "search.cpu_seconds",
                "must be finite and positive",
            ));
        }
        if !self.exploration_uniform.is_finite() || !(0.0..=1.0).contains(&self.exploration_uniform)
        {
            return Err(invalid_field(
                "search.exploration_uniform",
                "must be finite and between zero and one",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentMode {
    Registered,
    Exploratory,
}

impl CanonicalEncode for ExperimentMode {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_u8(match self {
            Self::Registered => 0,
            Self::Exploratory => 1,
        })
    }
}

/// Human-facing metadata. These fields never participate in experiment or cell identity.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestLabels {
    pub name: String,
    pub comment: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceRequirements {
    pub class: String,
    pub cpu_permits: u32,
    pub memory_bytes: u64,
    pub scratch_bytes: u64,
}

impl ResourceRequirements {
    fn validate(&self) -> Result<(), ManifestError> {
        validate_pinned_name("resource.class", &self.class)?;
        if self.cpu_permits == 0 {
            return Err(invalid_field("resource.cpu_permits", "must be non-zero"));
        }
        if self.memory_bytes == 0 {
            return Err(invalid_field("resource.memory_bytes", "must be non-zero"));
        }
        if self.scratch_bytes > self.memory_bytes {
            return Err(invalid_field(
                "resource.scratch_bytes",
                "must not exceed the cell memory budget",
            ));
        }
        Ok(())
    }
}

impl CanonicalEncode for ResourceRequirements {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.class)?;
        out.write_u32(self.cpu_permits)?;
        out.write_u64(self.memory_bytes)?;
        out.write_u64(self.scratch_bytes)
    }
}

/// Complete, immutable inputs shared by experiment and cell manifests.
///
/// Every field which may affect execution, verification, utility, or analysis is either a
/// content digest, a typed content ID, or a versioned value encoded directly into the manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImmutableInputs {
    pub code: Digest,
    pub image: Option<Digest>,
    pub domain: Digest,
    pub toolchain: Digest,
    pub corpus: Digest,
    pub split: Digest,
    pub feature_schema: FeatureSchemaId,
    pub action_schema: ActionSchemaId,
    pub policy: Digest,
    pub model_checkpoint: Option<ModelCheckpointId>,
    pub knowledge_edition: Option<KnowledgeEditionId>,
    pub cache_snapshot: Option<Digest>,
    pub search: SearchConfig,
    pub resource: ResourceRequirements,
    pub verifier: VerifierId,
    pub utility_evaluators: Vec<EvaluatorId>,
    pub analysis_plan: Digest,
}

impl ImmutableInputs {
    pub fn validate(&self) -> Result<(), ManifestError> {
        validate_digest("code", &self.code)?;
        validate_optional_digest("image", self.image.as_ref())?;
        validate_digest("domain", &self.domain)?;
        validate_digest("toolchain", &self.toolchain)?;
        validate_digest("corpus", &self.corpus)?;
        validate_digest("split", &self.split)?;
        validate_digest("feature_schema", self.feature_schema.digest())?;
        validate_digest("action_schema", self.action_schema.digest())?;
        validate_digest("policy", &self.policy)?;
        validate_optional_digest(
            "model_checkpoint",
            self.model_checkpoint
                .as_ref()
                .map(ModelCheckpointId::digest),
        )?;
        validate_optional_digest(
            "knowledge_edition",
            self.knowledge_edition
                .as_ref()
                .map(KnowledgeEditionId::digest),
        )?;
        validate_optional_digest("cache_snapshot", self.cache_snapshot.as_ref())?;
        self.search.validate()?;
        self.resource.validate()?;
        validate_digest("verifier", self.verifier.digest())?;
        if self.utility_evaluators.is_empty() {
            return Err(invalid_field(
                "utility_evaluators",
                "at least one evaluator must be pinned",
            ));
        }
        for evaluator in &self.utility_evaluators {
            validate_digest("utility_evaluators", evaluator.digest())?;
        }
        validate_digest("analysis_plan", &self.analysis_plan)
    }

    pub fn compatibility_digest(&self) -> Result<CompatibilityDigest, ManifestError> {
        self.validate()?;
        reflex_canonical::content_id(b"reflex.compatibility.v1", &CompatibilityInputs(self))
            .map(CompatibilityDigest::from_digest)
            .map_err(ManifestError::from)
    }
}

impl CanonicalEncode for ImmutableInputs {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_digest(&self.code)?;
        out.write_option(self.image.as_ref())?;
        out.write_digest(&self.domain)?;
        out.write_digest(&self.toolchain)?;
        out.write_digest(&self.corpus)?;
        out.write_digest(&self.split)?;
        out.write_digest(self.feature_schema.digest())?;
        out.write_digest(self.action_schema.digest())?;
        out.write_digest(&self.policy)?;
        out.write_option(
            self.model_checkpoint
                .as_ref()
                .map(ModelCheckpointId::digest),
        )?;
        out.write_option(
            self.knowledge_edition
                .as_ref()
                .map(KnowledgeEditionId::digest),
        )?;
        out.write_option(self.cache_snapshot.as_ref())?;
        self.search.encode_canonical(out)?;
        self.resource.encode_canonical(out)?;
        out.write_digest(self.verifier.digest())?;
        out.write_u32(u32::try_from(self.utility_evaluators.len()).map_err(|_| {
            CanonicalError::LengthOverflow {
                length: self.utility_evaluators.len(),
                limit: 32,
            }
        })?)?;
        for evaluator in &self.utility_evaluators {
            out.write_digest(evaluator.digest())?;
        }
        out.write_digest(&self.analysis_plan)
    }
}

struct CompatibilityInputs<'a>(&'a ImmutableInputs);

impl CanonicalEncode for CompatibilityInputs<'_> {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        let inputs = self.0;
        out.write_digest(&inputs.code)?;
        out.write_option(inputs.image.as_ref())?;
        out.write_digest(&inputs.domain)?;
        out.write_digest(&inputs.toolchain)?;
        out.write_digest(inputs.feature_schema.digest())?;
        out.write_digest(inputs.action_schema.digest())?;
        out.write_digest(inputs.verifier.digest())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExperimentManifest {
    pub schema: String,
    pub labels: ManifestLabels,
    pub mode: ExperimentMode,
    pub inputs: ImmutableInputs,
    pub seeds: Vec<u64>,
}

impl CanonicalEncode for ExperimentManifest {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.schema)?;
        self.mode.encode_canonical(out)?;
        self.inputs.encode_canonical(out)?;
        out.write_vec(&self.seeds)
    }
}

impl ExperimentManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        validate_schema("schema", &self.schema, "reflex.experiment.v1")?;
        self.inputs.validate()?;
        if self.seeds.is_empty() {
            return Err(invalid_field("seeds", "at least one seed must be pinned"));
        }
        Ok(())
    }

    pub fn manifest_digest(&self) -> Result<Digest, ManifestError> {
        self.validate()?;
        reflex_canonical::content_id(b"reflex.experiment.v1", self).map_err(ManifestError::from)
    }

    /// Canonical experiment identity shared by planners, metadata backends, and
    /// operator clients. Labels are intentionally excluded through the
    /// manifest's canonical encoding; semantic inputs and seeds are not.
    pub fn experiment_id(&self) -> Result<ExperimentId, ManifestError> {
        let manifest = self.manifest_digest()?;
        let digest = reflex_canonical::content_id(b"reflex.experiment-id.v1", &manifest)?;
        Ok(ExperimentId::from_digest(digest))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CellManifest {
    pub schema: String,
    pub experiment_id: ExperimentId,
    pub generation_id: GenerationId,
    pub task_id: TaskId,
    pub seed: u64,
    pub inputs: ImmutableInputs,
}

impl CanonicalEncode for CellManifest {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_str(&self.schema)?;
        out.write_digest(self.experiment_id.digest())?;
        out.write_digest(self.generation_id.digest())?;
        out.write_digest(self.task_id.digest())?;
        out.write_u64(self.seed)?;
        self.inputs.encode_canonical(out)
    }
}

impl CellManifest {
    pub fn validate(&self) -> Result<(), ManifestError> {
        validate_schema("schema", &self.schema, "reflex.cell.v1")?;
        validate_digest("experiment_id", self.experiment_id.digest())?;
        validate_digest("generation_id", self.generation_id.digest())?;
        validate_digest("task_id", self.task_id.digest())?;
        self.inputs.validate()
    }

    pub fn cell_id(&self) -> Result<CellId, ManifestError> {
        self.validate()?;
        reflex_canonical::content_id(b"reflex.cell.v1", self)
            .map(CellId::from_digest)
            .map_err(ManifestError::from)
    }

    pub fn compatibility_digest(&self) -> Result<CompatibilityDigest, ManifestError> {
        self.inputs.compatibility_digest()
    }

    pub fn diff_identity(&self, other: &Self) -> Result<Vec<ManifestChange>, ManifestError> {
        self.validate()?;
        other.validate()?;
        let mut changes = Vec::new();
        record_change(&mut changes, "schema", &self.schema, &other.schema);
        record_change(
            &mut changes,
            "experiment_id",
            &self.experiment_id.to_string(),
            &other.experiment_id.to_string(),
        );
        record_change(
            &mut changes,
            "generation_id",
            &self.generation_id.to_string(),
            &other.generation_id.to_string(),
        );
        record_change(
            &mut changes,
            "task_id",
            &self.task_id.to_string(),
            &other.task_id.to_string(),
        );
        record_change(
            &mut changes,
            "seed",
            &self.seed.to_string(),
            &other.seed.to_string(),
        );
        if self.inputs != other.inputs {
            record_input_changes(&mut changes, &self.inputs, &other.inputs)?;
        }
        Ok(changes)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestChange {
    pub field: String,
    pub before: String,
    pub after: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionLane {
    Stable,
    Uniform,
    Heuristic,
    Disagreement,
    FailureMining,
    Experimental,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneAllocation {
    pub lane: CollectionLane,
    pub logical_episodes: u64,
}

/// Immutable collection policy. Its digest is stored before any cell result is
/// observed, so later outcomes cannot change the exploration mixture.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionPlan {
    pub plan_digest: Digest,
    pub lanes: Vec<LaneAllocation>,
    pub minimum_accepted_experiences: u64,
    pub required_strata: BTreeMap<String, u64>,
    pub max_infrastructure_attempts: u32,
}

impl CollectionPlan {
    pub fn validate(&self) -> Result<(), SchedulerError> {
        if self.plan_digest == Digest::ZERO
            || self.minimum_accepted_experiences == 0
            || self.max_infrastructure_attempts == 0
        {
            return Err(SchedulerError::InvalidInput(
                "collection digest, minimum experience count, and retry bound must be non-zero"
                    .into(),
            ));
        }
        let mut seen = HashSet::new();
        for allocation in &self.lanes {
            if allocation.logical_episodes == 0 || !seen.insert(allocation.lane) {
                return Err(SchedulerError::InvalidInput(
                    "collection lanes must be unique and non-empty".into(),
                ));
            }
        }
        for mandatory in [
            CollectionLane::Stable,
            CollectionLane::Uniform,
            CollectionLane::Heuristic,
        ] {
            if !seen.contains(&mandatory) {
                return Err(SchedulerError::InvalidInput(format!(
                    "mandatory {mandatory:?} collection lane is absent"
                )));
            }
        }
        if self
            .required_strata
            .iter()
            .any(|(name, count)| name.trim().is_empty() || *count == 0)
        {
            return Err(SchedulerError::InvalidInput(
                "required strata must have names and positive counts".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionObservation {
    pub cell_id: CellId,
    pub accepted_attempt_no: Option<u32>,
    pub stratum: String,
    /// An exclusion is a registered terminal reason, never a negative label.
    pub exclusion: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionCompletion {
    pub accepted_cells: Vec<CellId>,
    pub excluded_cells: Vec<CellId>,
    pub source_set_digest: Digest,
}

#[derive(Serialize)]
struct CollectionSourceSetManifest<'a> {
    schema: &'static str,
    observations: Vec<&'a CollectionObservation>,
}

/// Canonical bytes committed by `CollectionCompletion::source_set_digest`.
/// Callers that persist the source set in CAS must publish these exact bytes,
/// making the scheduler's transition artifact independently reconstructable.
pub fn collection_source_set_bytes(
    observations: &[CollectionObservation],
) -> Result<Vec<u8>, SchedulerError> {
    let mut ordered: BTreeMap<CellId, &CollectionObservation> = BTreeMap::new();
    for observation in observations {
        if ordered.insert(observation.cell_id, observation).is_some() {
            return Err(SchedulerError::InvalidInput(
                "collection source cells must be unique; retries cannot double-count".into(),
            ));
        }
    }
    serde_json::to_vec(&CollectionSourceSetManifest {
        schema: "reflex.collection-source-set.v1",
        observations: ordered.into_values().collect(),
    })
    .map_err(|error| SchedulerError::InvalidInput(error.to_string()))
}

pub fn evaluate_collection_completion(
    plan: &CollectionPlan,
    observations: &[CollectionObservation],
) -> Result<CollectionCompletion, SchedulerError> {
    plan.validate()?;
    let mut by_cell = BTreeMap::new();
    for observation in observations {
        if observation.cell_id == CellId::from_digest(Digest::ZERO)
            || observation.stratum.trim().is_empty()
            || observation
                .exclusion
                .as_ref()
                .is_some_and(|reason| reason.trim().is_empty())
            || observation.accepted_attempt_no == Some(0)
            || (observation.accepted_attempt_no.is_some() == observation.exclusion.is_some())
        {
            return Err(SchedulerError::InvalidInput(
                "each collection cell needs exactly one accepted attempt or explicit exclusion"
                    .into(),
            ));
        }
        if by_cell.insert(observation.cell_id, observation).is_some() {
            return Err(SchedulerError::InvalidInput(
                "collection source cells must be unique; retries cannot double-count".into(),
            ));
        }
    }
    let accepted: Vec<_> = by_cell
        .values()
        .filter(|item| item.accepted_attempt_no.is_some())
        .collect();
    if accepted.len() < plan.minimum_accepted_experiences as usize {
        return Err(SchedulerError::InvalidInput(format!(
            "accepted experience count {} is below required {}",
            accepted.len(),
            plan.minimum_accepted_experiences
        )));
    }
    for (stratum, minimum) in &plan.required_strata {
        let count = accepted
            .iter()
            .filter(|item| item.stratum == *stratum)
            .count() as u64;
        if count < *minimum {
            return Err(SchedulerError::InvalidInput(format!(
                "stratum {stratum} has {count} accepted experiences; requires {minimum}"
            )));
        }
    }
    let accepted_cells: Vec<_> = accepted.iter().map(|item| item.cell_id).collect();
    let excluded_cells: Vec<_> = by_cell
        .values()
        .filter(|item| item.exclusion.is_some())
        .map(|item| item.cell_id)
        .collect();
    let source_bytes = collection_source_set_bytes(observations)?;
    Ok(CollectionCompletion {
        accepted_cells,
        excluded_cells,
        source_set_digest: Digest::hash_blake3(&source_bytes),
    })
}

fn invalid_field(field: &'static str, reason: impl Into<String>) -> ManifestError {
    ManifestError::InvalidField {
        field,
        reason: reason.into(),
    }
}

fn validate_digest(field: &'static str, digest: &Digest) -> Result<(), ManifestError> {
    if digest == &Digest::ZERO {
        Err(invalid_field(field, "zero digest is not a resolved input"))
    } else {
        Ok(())
    }
}

fn validate_optional_digest(
    field: &'static str,
    digest: Option<&Digest>,
) -> Result<(), ManifestError> {
    if let Some(digest) = digest {
        validate_digest(field, digest)?;
    }
    Ok(())
}

fn validate_schema(
    field: &'static str,
    value: &str,
    expected: &'static str,
) -> Result<(), ManifestError> {
    if value == expected {
        Ok(())
    } else {
        Err(invalid_field(field, format!("expected `{expected}`")))
    }
}

fn validate_pinned_name(field: &'static str, value: &str) -> Result<(), ManifestError> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(invalid_field(field, "must not be empty"));
    }
    if normalized == "latest" || normalized.ends_with(":latest") || normalized.ends_with("/latest")
    {
        return Err(invalid_field(field, "mutable `latest` tag is forbidden"));
    }
    Ok(())
}

fn record_change(changes: &mut Vec<ManifestChange>, field: &str, before: &str, after: &str) {
    if before != after {
        changes.push(ManifestChange {
            field: field.to_string(),
            before: before.to_string(),
            after: after.to_string(),
        });
    }
}

fn record_input_changes(
    changes: &mut Vec<ManifestChange>,
    before: &ImmutableInputs,
    after: &ImmutableInputs,
) -> Result<(), ManifestError> {
    let before_value =
        serde_json::to_value(before).map_err(|error| invalid_field("inputs", error.to_string()))?;
    let after_value =
        serde_json::to_value(after).map_err(|error| invalid_field("inputs", error.to_string()))?;
    let (Some(before_map), Some(after_map)) = (before_value.as_object(), after_value.as_object())
    else {
        return Err(invalid_field("inputs", "must serialize as an object"));
    };
    for (field, before) in before_map {
        let after = &after_map[field];
        if before != after {
            changes.push(ManifestChange {
                field: format!("inputs.{field}"),
                before: before.to_string(),
                after: after.to_string(),
            });
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BenchmarkEvidenceRecord {
    pub name: String,
    pub host_class: String,
    pub p95_ns: f64,
}

#[derive(Error, Debug, Clone, PartialEq)]
pub enum PromotionBenchmarkError {
    #[error("benchmark regression: {name} regressed {percent:.2}%")]
    Regression { name: String, percent: f64 },
    #[error("missing accepted benchmark evidence for {0}")]
    MissingEvidence(String),
    #[error("invalid accepted benchmark evidence: {0}")]
    InvalidEvidence(String),
}

fn check_regression_against_baseline(
    baseline: &BenchmarkEvidenceRecord,
    candidate: &BenchmarkEvidenceRecord,
    max_ratio: f64,
) -> Result<(), PromotionBenchmarkError> {
    if !baseline.p95_ns.is_finite()
        || !candidate.p95_ns.is_finite()
        || !max_ratio.is_finite()
        || baseline.p95_ns <= 0.0
        || candidate.p95_ns <= 0.0
        || max_ratio < 0.0
    {
        return Err(PromotionBenchmarkError::InvalidEvidence(
            "benchmark timings must be finite and positive and the regression ratio non-negative"
                .into(),
        ));
    }
    if baseline.name != candidate.name || baseline.host_class != candidate.host_class {
        return Err(PromotionBenchmarkError::MissingEvidence(
            baseline.name.clone(),
        ));
    }
    let limit = baseline.p95_ns * (1.0 + max_ratio);
    if candidate.p95_ns > limit {
        let percent = (candidate.p95_ns - baseline.p95_ns) / baseline.p95_ns * 100.0;
        return Err(PromotionBenchmarkError::Regression {
            name: candidate.name.clone(),
            percent,
        });
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcceptedBenchmarkEvidence {
    pub benchmark: String,
    pub host_class: String,
    /// Accepted baseline p95 from prior evidence run.
    pub baseline_p95_ns: f64,
    /// Current candidate p95 under evaluation.
    pub candidate_p95_ns: f64,
    /// Content-addressed raw samples for the accepted baseline and candidate.
    pub baseline_evidence: Digest,
    pub candidate_evidence: Digest,
    /// Canonical host calibration used for both matched runs.
    pub host_calibration: Digest,
    /// CPU spent in feature extraction, queueing, inference, and postprocess.
    pub inference_cpu_ns: u64,
    /// Total search CPU for the matched evaluation cells, including inference.
    pub total_search_cpu_ns: u64,
    /// Explicit independent evaluation lineage identities.
    pub lineages: Vec<Digest>,
    /// Receipt issued by the registered benchmark acceptance policy.
    pub acceptance_receipt: Digest,
}

impl AcceptedBenchmarkEvidence {
    fn validate(&self) -> Result<(), PromotionBenchmarkError> {
        let unique: HashSet<_> = self.lineages.iter().copied().collect();
        if self.benchmark.trim().is_empty()
            || self.host_class.trim().is_empty()
            || !self.baseline_p95_ns.is_finite()
            || !self.candidate_p95_ns.is_finite()
            || self.baseline_p95_ns <= 0.0
            || self.candidate_p95_ns <= 0.0
            || self.baseline_evidence == Digest::ZERO
            || self.candidate_evidence == Digest::ZERO
            || self.host_calibration == Digest::ZERO
            || self.acceptance_receipt == Digest::ZERO
            || self.lineages.is_empty()
            || unique.len() != self.lineages.len()
            || unique.contains(&Digest::ZERO)
            || self.total_search_cpu_ns == 0
            || self.inference_cpu_ns > self.total_search_cpu_ns
        {
            return Err(PromotionBenchmarkError::InvalidEvidence(
                "benchmark evidence requires finite timings, non-zero raw/calibration/acceptance identities, unique non-zero lineages, and consistent CPU totals"
                    .into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PromotionPolicy {
    pub required_lineages: usize,
    pub max_inference_share: f32,
    pub min_top1_viable_rate: f32,
    pub min_mrr: f32,
}

impl Default for PromotionPolicy {
    fn default() -> Self {
        Self {
            required_lineages: 4,
            max_inference_share: 0.10,
            min_top1_viable_rate: 0.50,
            min_mrr: 0.50,
        }
    }
}

impl PromotionPolicy {
    /// Tutorial/bootstrap gate for the frozen bitvec corpus (5 decision groups).
    /// Production cells use [`Default`] (0.50/0.50) plus P1.6 benchmark evidence.
    pub fn bootstrap_tutorial() -> Self {
        Self {
            required_lineages: 1,
            max_inference_share: 0.10,
            min_top1_viable_rate: 0.50,
            min_mrr: 0.50,
        }
    }

    /// Evaluates promotion using accepted benchmark evidence (P1.6) — no ad-hoc reruns.
    pub fn evaluate_promotion(
        &self,
        report: &EvaluationReport,
        benchmark_evidence: &[AcceptedBenchmarkEvidence],
    ) -> Result<bool, PromotionBenchmarkError> {
        if report.top1_viable_rate < self.min_top1_viable_rate
            || report.mrr_cheapest_route < self.min_mrr
        {
            return Ok(false);
        }

        let scorer = benchmark_evidence
            .iter()
            .find(|e| e.benchmark == "scorer_p95");
        let Some(scorer_ev) = scorer else {
            return Err(PromotionBenchmarkError::MissingEvidence(
                "scorer_p95".to_string(),
            ));
        };
        scorer_ev.validate()?;
        if scorer_ev.lineages.len() < self.required_lineages {
            return Err(PromotionBenchmarkError::InvalidEvidence(format!(
                "requires {} independent lineages, got {}",
                self.required_lineages,
                scorer_ev.lineages.len()
            )));
        }

        let baseline = BenchmarkEvidenceRecord {
            name: scorer_ev.benchmark.clone(),
            host_class: scorer_ev.host_class.clone(),
            p95_ns: scorer_ev.baseline_p95_ns,
        };
        let candidate = BenchmarkEvidenceRecord {
            name: scorer_ev.benchmark.clone(),
            host_class: scorer_ev.host_class.clone(),
            p95_ns: scorer_ev.candidate_p95_ns,
        };

        check_regression_against_baseline(&baseline, &candidate, 0.05)?;

        let inference_share =
            scorer_ev.inference_cpu_ns as f64 / scorer_ev.total_search_cpu_ns as f64;
        if inference_share > f64::from(self.max_inference_share) {
            return Ok(false);
        }

        Ok(true)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationSliceKind {
    Overall,
    Family,
    Stratum,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchedEvaluationSlice {
    pub kind: EvaluationSliceKind,
    pub name: String,
    pub matched_population: Digest,
    pub matched_cells: u64,
    pub candidate_solved: u64,
    pub stable_solved: u64,
    pub cheap_baseline_solved: u64,
    pub candidate_cpu_ns: u64,
    pub stable_cpu_ns: u64,
    pub cheap_baseline_cpu_ns: u64,
    pub candidate_verified_actions: u64,
    pub stable_verified_actions: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchedPromotionEvidence {
    pub evaluation_report_digest: Digest,
    pub candidate: ModelCheckpointId,
    pub stable: Option<ModelCheckpointId>,
    pub lineages: Vec<Digest>,
    pub benchmark_evidence: Vec<AcceptedBenchmarkEvidence>,
    pub slices: Vec<MatchedEvaluationSlice>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisteredPromotionPolicy {
    pub offline: PromotionPolicy,
    pub max_family_solve_deficit: f64,
    pub max_stratum_solve_deficit: f64,
    pub max_cpu_ratio_at_equivalent_solve: f64,
    pub min_action_compression: f64,
}

impl Default for RegisteredPromotionPolicy {
    fn default() -> Self {
        Self {
            offline: PromotionPolicy::default(),
            max_family_solve_deficit: 0.02,
            max_stratum_solve_deficit: 0.02,
            max_cpu_ratio_at_equivalent_solve: 1.10,
            min_action_compression: 1.25,
        }
    }
}

impl RegisteredPromotionPolicy {
    pub fn digest(&self) -> Result<Digest, SchedulerError> {
        let encoded = serde_json::to_vec(self)
            .map_err(|error| SchedulerError::InvalidInput(error.to_string()))?;
        let mut identity = b"reflex.registered-promotion-policy.v1\0".to_vec();
        identity.extend_from_slice(&encoded);
        Ok(Digest::hash_blake3(&identity))
    }

    /// Evaluate only accepted, matched evidence. Every family and stratum row
    /// is a veto; aggregate improvements cannot hide severe regressions.
    pub fn evaluate(
        &self,
        report: &EvaluationReport,
        evidence: &MatchedPromotionEvidence,
    ) -> Result<bool, SchedulerError> {
        if ![
            self.max_family_solve_deficit,
            self.max_stratum_solve_deficit,
            self.max_cpu_ratio_at_equivalent_solve,
            self.min_action_compression,
        ]
        .into_iter()
        .all(|value| value.is_finite() && value >= 0.0)
            || self.max_family_solve_deficit > 1.0
            || self.max_stratum_solve_deficit > 1.0
            || self.max_cpu_ratio_at_equivalent_solve == 0.0
            || self.min_action_compression == 0.0
        {
            return Err(SchedulerError::InvalidInput(
                "registered promotion thresholds are invalid".into(),
            ));
        }
        if evidence.evaluation_report_digest == Digest::ZERO
            || *evidence.candidate.digest() == Digest::ZERO
        {
            return Err(SchedulerError::InvalidInput(
                "promotion evidence identities must be non-zero".into(),
            ));
        }
        let unique_lineages: HashSet<_> = evidence.lineages.iter().copied().collect();
        if unique_lineages.len() != evidence.lineages.len()
            || unique_lineages.contains(&Digest::ZERO)
            || unique_lineages.len() < self.offline.required_lineages
        {
            return Err(SchedulerError::InvalidInput(format!(
                "promotion requires {} unique non-zero lineages",
                self.offline.required_lineages
            )));
        }
        for benchmark in &evidence.benchmark_evidence {
            benchmark
                .validate()
                .map_err(|error| SchedulerError::InvalidInput(error.to_string()))?;
            let benchmark_lineages: HashSet<_> = benchmark.lineages.iter().copied().collect();
            if benchmark_lineages != unique_lineages {
                return Err(SchedulerError::InvalidInput(
                    "benchmark and evaluation lineage identities disagree".into(),
                ));
            }
        }
        if !self
            .offline
            .evaluate_promotion(report, &evidence.benchmark_evidence)
            .map_err(|error| SchedulerError::PromotionRejected(error.to_string()))?
        {
            return Ok(false);
        }
        if evidence.slices.is_empty()
            || !evidence
                .slices
                .iter()
                .any(|slice| slice.kind == EvaluationSliceKind::Overall)
            || !evidence
                .slices
                .iter()
                .any(|slice| slice.kind == EvaluationSliceKind::Family)
            || !evidence
                .slices
                .iter()
                .any(|slice| slice.kind == EvaluationSliceKind::Stratum)
        {
            return Err(SchedulerError::InvalidInput(
                "matched evaluation requires overall, family, and stratum slices".into(),
            ));
        }
        let mut slice_keys = HashSet::new();
        for slice in &evidence.slices {
            if slice.name.trim().is_empty()
                || !slice_keys.insert((slice.kind, slice.name.as_str()))
                || slice.matched_population == Digest::ZERO
                || slice.matched_cells == 0
                || slice.candidate_solved > slice.matched_cells
                || slice.stable_solved > slice.matched_cells
                || slice.cheap_baseline_solved > slice.matched_cells
                || slice.stable_cpu_ns == 0
                || slice.cheap_baseline_cpu_ns == 0
            {
                return Err(SchedulerError::InvalidInput(format!(
                    "invalid matched evaluation slice {}",
                    slice.name
                )));
            }
            let candidate_rate = slice.candidate_solved as f64 / slice.matched_cells as f64;
            let stable_rate = slice.stable_solved as f64 / slice.matched_cells as f64;
            let cheap_rate = slice.cheap_baseline_solved as f64 / slice.matched_cells as f64;
            let allowed_deficit = match slice.kind {
                EvaluationSliceKind::Family => self.max_family_solve_deficit,
                EvaluationSliceKind::Stratum => self.max_stratum_solve_deficit,
                EvaluationSliceKind::Overall => self
                    .max_family_solve_deficit
                    .min(self.max_stratum_solve_deficit),
            };
            if candidate_rate + allowed_deficit < stable_rate
                || candidate_rate + allowed_deficit < cheap_rate
            {
                return Ok(false);
            }
            // Cost comparisons are valid only on equivalent solve populations.
            if candidate_rate < stable_rate {
                continue;
            }
            let cpu_ratio = slice.candidate_cpu_ns as f64 / slice.stable_cpu_ns as f64;
            if !cpu_ratio.is_finite() || cpu_ratio > self.max_cpu_ratio_at_equivalent_solve {
                return Ok(false);
            }
            if candidate_rate >= cheap_rate {
                let cheap_cpu_ratio =
                    slice.candidate_cpu_ns as f64 / slice.cheap_baseline_cpu_ns as f64;
                if !cheap_cpu_ratio.is_finite()
                    || cheap_cpu_ratio > self.max_cpu_ratio_at_equivalent_solve
                {
                    return Ok(false);
                }
            }
            if slice.candidate_verified_actions == 0 {
                return Err(SchedulerError::InvalidInput(format!(
                    "slice {} has no candidate verified-action accounting",
                    slice.name
                )));
            }
            let compression =
                slice.stable_verified_actions as f64 / slice.candidate_verified_actions as f64;
            if !compression.is_finite() || compression < self.min_action_compression {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostAuthority {
    PlanningProxy,
    BillingAuthority,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RegisteredStopPolicy {
    pub max_generations: u32,
    pub max_summed_cpu_hours: f64,
    pub max_machine_hours: f64,
    pub max_wall_seconds: u64,
    pub max_planning_dollars: f64,
    pub no_promotion_generations: u32,
    pub min_marginal_utility_per_cpu_hour: f64,
    pub target_capability: Option<f64>,
    pub target_utility: Option<f64>,
    pub cleanup_reserve_machine_hours: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StopEvidence {
    pub economics_digest: Digest,
    pub generations_completed: u32,
    /// Counts only completed scientific rejections; infrastructure failures do not increment it.
    pub consecutive_scientific_rejections: u32,
    pub summed_cpu_hours: f64,
    pub machine_hours: f64,
    pub wall_seconds: u64,
    pub cost_dollars: f64,
    pub cost_authority: CostAuthority,
    pub capability: f64,
    pub utility: f64,
    pub marginal_utility_per_cpu_hour: f64,
    /// Maximum CPU exposure of one already-leased bounded cell.
    pub active_cell_bound_cpu_hours: f64,
    pub operator_stop: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    Operator,
    GenerationLimit,
    CpuLimit,
    MachineLimit,
    WallDeadline,
    PlanningCostLimit,
    TargetCapability,
    TargetUtility,
    NoPromotionPatience,
    MarginalUtility,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StopDecision {
    pub reason: StopReason,
    pub economics_digest: Digest,
    pub receipt_digest: Digest,
}

impl RegisteredStopPolicy {
    pub fn evaluate(
        &self,
        evidence: &StopEvidence,
    ) -> Result<Option<StopDecision>, SchedulerError> {
        let finite_nonnegative = |value: f64| value.is_finite() && value >= 0.0;
        if evidence.economics_digest == Digest::ZERO
            || ![
                evidence.summed_cpu_hours,
                evidence.machine_hours,
                evidence.cost_dollars,
                evidence.capability,
                evidence.active_cell_bound_cpu_hours,
            ]
            .into_iter()
            .all(finite_nonnegative)
            || !evidence.marginal_utility_per_cpu_hour.is_finite()
            || !evidence.utility.is_finite()
            || ![
                self.max_summed_cpu_hours,
                self.max_machine_hours,
                self.max_planning_dollars,
                self.cleanup_reserve_machine_hours,
            ]
            .into_iter()
            .all(finite_nonnegative)
            || !self.min_marginal_utility_per_cpu_hour.is_finite()
            || self.max_generations == 0
            || self.max_summed_cpu_hours == 0.0
            || self.max_machine_hours == 0.0
            || self.max_wall_seconds == 0
            || self.no_promotion_generations == 0
            || self.cleanup_reserve_machine_hours > self.max_machine_hours
            || self
                .target_capability
                .is_some_and(|target| !target.is_finite())
            || self
                .target_utility
                .is_some_and(|target| !target.is_finite())
        {
            return Err(SchedulerError::InvalidInput(
                "stop policy and realized economics must be finite, non-negative evidence".into(),
            ));
        }
        let reason = if evidence.operator_stop {
            Some(StopReason::Operator)
        } else if evidence.generations_completed >= self.max_generations {
            Some(StopReason::GenerationLimit)
        } else if evidence.summed_cpu_hours + evidence.active_cell_bound_cpu_hours
            >= self.max_summed_cpu_hours
        {
            Some(StopReason::CpuLimit)
        } else if evidence.machine_hours + self.cleanup_reserve_machine_hours
            >= self.max_machine_hours
        {
            Some(StopReason::MachineLimit)
        } else if evidence.wall_seconds >= self.max_wall_seconds {
            Some(StopReason::WallDeadline)
        } else if evidence.cost_authority == CostAuthority::PlanningProxy
            && evidence.cost_dollars >= self.max_planning_dollars
        {
            Some(StopReason::PlanningCostLimit)
        } else if self
            .target_capability
            .is_some_and(|target| evidence.capability >= target)
        {
            Some(StopReason::TargetCapability)
        } else if self
            .target_utility
            .is_some_and(|target| evidence.utility >= target)
        {
            Some(StopReason::TargetUtility)
        } else if evidence.consecutive_scientific_rejections >= self.no_promotion_generations {
            Some(StopReason::NoPromotionPatience)
        } else if evidence.generations_completed > 0
            && evidence.marginal_utility_per_cpu_hour < self.min_marginal_utility_per_cpu_hour
        {
            Some(StopReason::MarginalUtility)
        } else {
            None
        };
        let Some(reason) = reason else {
            return Ok(None);
        };
        let payload = serde_json::to_vec(&(self, evidence, &reason))
            .map_err(|error| SchedulerError::InvalidInput(error.to_string()))?;
        Ok(Some(StopDecision {
            reason,
            economics_digest: evidence.economics_digest,
            receipt_digest: Digest::hash_blake3(&payload),
        }))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StopPolicy {
    pub max_generations: u32,
    pub max_cpu_hours: f64,
    pub no_promotion_generations: u32,
}

impl Default for StopPolicy {
    fn default() -> Self {
        Self {
            max_generations: 10,
            max_cpu_hours: 50.0,
            no_promotion_generations: 3,
        }
    }
}

/// Pure policy projection for tests and planning. It cannot publish or authorize
/// a generation transition; production state changes use [`DurableGenerationCoordinator`].
pub struct GenerationCoordinator {
    state: GenerationState,
    current_generation: u32,
    no_promotion_streak: u32,
    active_model: Option<ModelCheckpointId>,
    active_knowledge: Option<KnowledgeEditionId>,
    stop_policy: StopPolicy,
    promotion_policy: PromotionPolicy,
}

impl GenerationCoordinator {
    pub fn new(
        stop_policy: StopPolicy,
        promotion_policy: PromotionPolicy,
        initial_model: Option<ModelCheckpointId>,
        initial_knowledge: Option<KnowledgeEditionId>,
    ) -> Self {
        Self {
            state: GenerationState::Bootstrap,
            current_generation: 1,
            no_promotion_streak: 0,
            active_model: initial_model,
            active_knowledge: initial_knowledge,
            stop_policy,
            promotion_policy,
        }
    }

    pub fn transition_to(&mut self, next: GenerationState) -> Result<(), SchedulerError> {
        if !self.state.permits(next) {
            return Err(SchedulerError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }

        self.state = next;
        Ok(())
    }

    pub fn state(&self) -> GenerationState {
        self.state
    }

    pub fn active_model(&self) -> Option<ModelCheckpointId> {
        self.active_model
    }

    pub fn active_knowledge(&self) -> Option<KnowledgeEditionId> {
        self.active_knowledge
    }

    pub fn simulate_evaluation_result(
        &mut self,
        report: &EvaluationReport,
        benchmark_evidence: &[AcceptedBenchmarkEvidence],
        candidate_checkpoint: ModelCheckpointId,
    ) -> Result<GenerationState, PromotionBenchmarkError> {
        if self
            .promotion_policy
            .evaluate_promotion(report, benchmark_evidence)?
        {
            self.active_model = Some(candidate_checkpoint);
            self.no_promotion_streak = 0;
            self.state = GenerationState::Promoted;
        } else {
            self.no_promotion_streak += 1;
            self.state = GenerationState::Rejected;
        }

        self.current_generation += 1;

        if self.current_generation > self.stop_policy.max_generations
            || self.no_promotion_streak >= self.stop_policy.no_promotion_generations
        {
            self.state = GenerationState::Stopped;
        }

        Ok(self.state)
    }
}

/// Scheduler facade over the metadata compare-and-set journal.
///
/// This is the stateful coordinator used by production workflows. Its cached
/// snapshot is replaced only by a committed metadata transition, and a fresh
/// process reconstructs the same state with [`Self::reconstruct`].
pub struct DurableGenerationCoordinator<'a, M: MetaStore + ?Sized> {
    meta: &'a M,
    record: GenerationRecord,
}

impl<'a, M: MetaStore + ?Sized> DurableGenerationCoordinator<'a, M> {
    pub async fn create(
        meta: &'a M,
        generation: NewGenerationRecord,
    ) -> Result<Self, SchedulerError> {
        let record = meta.create_generation(generation).await?;
        Ok(Self { meta, record })
    }

    pub async fn reconstruct(
        meta: &'a M,
        generation_id: GenerationId,
    ) -> Result<Self, SchedulerError> {
        let record = meta
            .get_generation(generation_id)
            .await?
            .ok_or(MetaError::GenerationNotFound(generation_id))?;
        Ok(Self { meta, record })
    }

    pub fn record(&self) -> &GenerationRecord {
        &self.record
    }

    pub async fn transition(
        &mut self,
        command: GenerationTransitionCommand,
    ) -> Result<&GenerationRecord, SchedulerError> {
        if command.generation_id != self.record.id || command.expected != self.record.state {
            return Err(SchedulerError::InvalidTransition {
                from: self.record.state,
                to: command.next,
            });
        }
        self.record = self.meta.compare_and_set_generation(command).await?;
        Ok(&self.record)
    }

    async fn advance(
        &mut self,
        next: GenerationState,
        artifacts: Vec<GenerationArtifactRef>,
        cause: impl Into<String>,
        operator_id: impl Into<String>,
    ) -> Result<&GenerationRecord, SchedulerError> {
        let cause = cause.into();
        let operator_id = operator_id.into();
        if cause.trim().is_empty() || operator_id.trim().is_empty() {
            return Err(SchedulerError::InvalidInput(
                "transition cause and operator must be non-empty".into(),
            ));
        }
        let mut identity = Vec::new();
        identity.extend_from_slice(self.record.id.digest().as_bytes());
        identity.extend_from_slice(&self.record.revision.to_le_bytes());
        identity.extend_from_slice(next.as_str().as_bytes());
        identity.extend_from_slice(cause.as_bytes());
        identity.extend_from_slice(operator_id.as_bytes());
        for artifact in &artifacts {
            identity.extend_from_slice(artifact.role.as_str().as_bytes());
            identity.extend_from_slice(artifact.digest.as_bytes());
        }
        self.transition(GenerationTransitionCommand {
            command_id: Digest::hash_blake3(&identity),
            generation_id: self.record.id,
            expected: self.record.state,
            next,
            artifacts,
            cause,
            operator_id,
            failure: None,
        })
        .await
    }

    pub async fn start_collection(
        &mut self,
        plan: &CollectionPlan,
        operator_id: impl Into<String>,
    ) -> Result<&GenerationRecord, SchedulerError> {
        plan.validate()?;
        self.advance(
            GenerationState::Collecting,
            vec![GenerationArtifactRef {
                digest: plan.plan_digest,
                role: GenerationArtifactRole::CollectionManifest,
            }],
            "immutable collection plan accepted",
            operator_id,
        )
        .await
    }

    pub async fn seal_collection(
        &mut self,
        completion: &CollectionCompletion,
        operator_id: impl Into<String>,
    ) -> Result<&GenerationRecord, SchedulerError> {
        if completion.source_set_digest == Digest::ZERO || completion.accepted_cells.is_empty() {
            return Err(SchedulerError::InvalidInput(
                "sealed collection requires an accepted, content-addressed source set".into(),
            ));
        }
        self.advance(
            GenerationState::Verifying,
            vec![GenerationArtifactRef {
                digest: completion.source_set_digest,
                role: GenerationArtifactRole::Other("collection_source_set".into()),
            }],
            "collection source set sealed",
            operator_id,
        )
        .await
    }

    pub async fn publish_accepted_evidence(
        &mut self,
        accepted_evidence: Digest,
        operator_id: impl Into<String>,
    ) -> Result<&GenerationRecord, SchedulerError> {
        self.advance(
            GenerationState::CompilingDataset,
            vec![nonzero_artifact(
                accepted_evidence,
                GenerationArtifactRole::AcceptedEvidence,
            )?],
            "accepted evidence verified",
            operator_id,
        )
        .await
    }

    pub async fn publish_dataset(
        &mut self,
        dataset_manifest: Digest,
        operator_id: impl Into<String>,
    ) -> Result<&GenerationRecord, SchedulerError> {
        self.advance(
            GenerationState::Training,
            vec![nonzero_artifact(
                dataset_manifest,
                GenerationArtifactRole::Dataset,
            )?],
            "dataset compiled from sealed source set",
            operator_id,
        )
        .await
    }

    pub async fn publish_checkpoint(
        &mut self,
        checkpoint: ModelCheckpointId,
        operator_id: impl Into<String>,
    ) -> Result<&GenerationRecord, SchedulerError> {
        self.advance(
            GenerationState::Evaluating,
            vec![nonzero_artifact(
                *checkpoint.digest(),
                GenerationArtifactRole::Checkpoint,
            )?],
            "training checkpoint published",
            operator_id,
        )
        .await
    }

    async fn publish_evaluation(
        &mut self,
        evaluation_report: Digest,
        accepted: bool,
        policy_digest: Digest,
        operator_id: impl Into<String>,
    ) -> Result<&GenerationRecord, SchedulerError> {
        let mut artifacts = vec![
            nonzero_artifact(evaluation_report, GenerationArtifactRole::EvaluationReport)?,
            nonzero_artifact(
                policy_digest,
                GenerationArtifactRole::Other("evaluation_policy".into()),
            )?,
        ];
        let next = if accepted {
            artifacts.push(GenerationArtifactRef {
                digest: evaluation_report,
                role: GenerationArtifactRole::AcceptedEvaluation,
            });
            GenerationState::PromotionPending
        } else {
            GenerationState::Rejected
        };
        self.advance(
            next,
            artifacts,
            if accepted {
                "evaluation accepted by registered policy"
            } else {
                "evaluation rejected by registered policy"
            },
            operator_id,
        )
        .await
    }

    /// Persist a fail-closed evaluation decision when the registered evidence
    /// package is incomplete or the candidate misses its thresholds. The
    /// caller must publish the report and policy artifacts first; this method
    /// only advances the durable state machine and can never promote.
    pub async fn reject_evaluation(
        &mut self,
        evaluation_report: Digest,
        policy_digest: Digest,
        operator_id: impl Into<String>,
    ) -> Result<&GenerationRecord, SchedulerError> {
        self.publish_evaluation(evaluation_report, false, policy_digest, operator_id)
            .await
    }

    pub async fn evaluate_candidate(
        &mut self,
        report: &EvaluationReport,
        evidence: &MatchedPromotionEvidence,
        policy: &RegisteredPromotionPolicy,
        operator_id: impl Into<String>,
    ) -> Result<bool, SchedulerError> {
        let accepted = policy.evaluate(report, evidence)?;
        self.publish_evaluation(
            evidence.evaluation_report_digest,
            accepted,
            policy.digest()?,
            operator_id,
        )
        .await?;
        Ok(accepted)
    }

    pub async fn promote(
        &mut self,
        candidate: ModelCheckpointId,
        active_stable: Option<ModelCheckpointId>,
        evaluation_report: Digest,
        policy: &RegisteredPromotionPolicy,
        operator_id: impl Into<String>,
    ) -> Result<GenerationPromotionResult, SchedulerError> {
        if evaluation_report == Digest::ZERO {
            return Err(SchedulerError::InvalidInput(
                "promotion policy and evaluation digests must be non-zero".into(),
            ));
        }
        let policy_digest = policy.digest()?;
        if !self.record.artifacts.iter().any(|artifact| {
            artifact.role == GenerationArtifactRole::Other("evaluation_policy".into())
                && artifact.digest == policy_digest
        }) {
            return Err(SchedulerError::PromotionRejected(
                "promotion policy differs from the policy that accepted evaluation".into(),
            ));
        }
        let receipt_digest = reflex_ml_core::promotion_receipt_digest(
            &candidate,
            active_stable.as_ref(),
            &evaluation_report,
            self.record.id.digest(),
            &policy_digest,
        );
        let artifacts = vec![
            GenerationArtifactRef {
                digest: *candidate.digest(),
                role: GenerationArtifactRole::Checkpoint,
            },
            GenerationArtifactRef {
                digest: evaluation_report,
                role: GenerationArtifactRole::AcceptedEvaluation,
            },
            GenerationArtifactRef {
                digest: receipt_digest,
                role: GenerationArtifactRole::PromotionReceipt,
            },
        ];
        let operator_id = operator_id.into();
        let mut identity = Vec::new();
        identity.extend_from_slice(self.record.id.digest().as_bytes());
        identity.extend_from_slice(&self.record.revision.to_le_bytes());
        identity.extend_from_slice(receipt_digest.as_bytes());
        identity.extend_from_slice(operator_id.as_bytes());
        let result = self
            .meta
            .commit_generation_promotion(GenerationPromotionRequest {
                transition: GenerationTransitionCommand {
                    command_id: Digest::hash_blake3(&identity),
                    generation_id: self.record.id,
                    expected: self.record.state,
                    next: GenerationState::Promoted,
                    artifacts,
                    cause: "candidate atomically promoted".into(),
                    operator_id,
                    failure: None,
                },
                promotion: PromotionRequest {
                    candidate,
                    active_stable,
                    evaluation_report_digest: evaluation_report,
                    generation_id: self.record.id,
                },
                policy_digest,
            })
            .await?;
        self.record = result.generation.clone();
        Ok(result)
    }

    /// Apply an operator or externally registered stop receipt through the
    /// durable state machine. The receipt must already exist in the caller's
    /// artifact store; the coordinator never fabricates one.
    pub async fn stop_with_receipt(
        &mut self,
        stop_receipt: Digest,
        reason: impl Into<String>,
        operator_id: impl Into<String>,
    ) -> Result<&GenerationRecord, SchedulerError> {
        self.advance(
            GenerationState::Stopped,
            vec![nonzero_artifact(
                stop_receipt,
                GenerationArtifactRole::StopReceipt,
            )?],
            reason,
            operator_id,
        )
        .await
    }

    pub async fn apply_stop_policy(
        &mut self,
        policy: &RegisteredStopPolicy,
        evidence: &StopEvidence,
        operator_id: impl Into<String>,
    ) -> Result<Option<StopDecision>, SchedulerError> {
        let Some(decision) = policy.evaluate(evidence)? else {
            return Ok(None);
        };
        self.stop_with_receipt(
            decision.receipt_digest,
            format!("registered stop condition: {:?}", decision.reason),
            operator_id,
        )
        .await?;
        Ok(Some(decision))
    }

    /// Recover only expired leases. Completed scientific failures are never
    /// converted back to infrastructure retries.
    pub async fn reconcile_expired_attempts(
        &self,
        observed_at_timestamp: u64,
        max_infrastructure_attempts: u32,
    ) -> Result<GenerationReconcileResult, SchedulerError> {
        Ok(self
            .meta
            .reconcile_generation_cells(GenerationReconcileRequest {
                generation_id: self.record.id,
                observed_at_timestamp,
                max_infrastructure_attempts,
            })
            .await?)
    }

    pub async fn cancel_cells(
        &self,
        mode: CancellationMode,
        cause: impl Into<String>,
        operator_id: impl Into<String>,
    ) -> Result<GenerationCancellationResult, SchedulerError> {
        Ok(self
            .meta
            .cancel_generation_cells(GenerationCancellationRequest {
                generation_id: self.record.id,
                mode,
                cause: cause.into(),
                operator_id: operator_id.into(),
            })
            .await?)
    }
}

fn nonzero_artifact(
    digest: Digest,
    role: GenerationArtifactRole,
) -> Result<GenerationArtifactRef, SchedulerError> {
    if digest == Digest::ZERO {
        return Err(SchedulerError::InvalidInput(format!(
            "{} artifact digest must be non-zero",
            role.as_str()
        )));
    }
    Ok(GenerationArtifactRef { digest, role })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortfolioLane {
    Exploitation,
    NearFrontier,
    Exploratory,
    Adversarial,
    Speculative,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortfolioPolicy {
    pub min_lane_share: f64,
    pub max_lane_share: f64,
    pub rebalance_cadence_generations: u32,
    pub lanes_enabled: Vec<PortfolioLane>,
}

impl Default for PortfolioPolicy {
    fn default() -> Self {
        Self {
            min_lane_share: 0.05,
            max_lane_share: 0.60,
            rebalance_cadence_generations: 1,
            lanes_enabled: vec![
                PortfolioLane::Exploitation,
                PortfolioLane::NearFrontier,
                PortfolioLane::Exploratory,
                PortfolioLane::Adversarial,
                PortfolioLane::Speculative,
            ],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortfolioAllocation {
    pub seed: u64,
    pub generation: u32,
    pub lane_shares: BTreeMap<PortfolioLane, f64>,
    pub counterfactual_candidates: Vec<CandidateId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PortfolioCandidate {
    pub id: CandidateId,
    pub critic_score: f32,
    pub uncertainty: f32,
    pub eligible_lanes: BTreeSet<PortfolioLane>,
}

impl PortfolioPolicy {
    pub fn validate(&self) -> Result<(), SchedulerError> {
        if self.lanes_enabled.is_empty() {
            return Err(SchedulerError::InvalidInput(
                "portfolio requires at least one enabled lane".into(),
            ));
        }
        let lanes: BTreeSet<_> = self.lanes_enabled.iter().copied().collect();
        if lanes.len() != self.lanes_enabled.len() {
            return Err(SchedulerError::InvalidInput(
                "portfolio lanes must be unique".into(),
            ));
        }
        if !self.min_lane_share.is_finite()
            || !self.max_lane_share.is_finite()
            || self.min_lane_share <= 0.0
            || self.max_lane_share > 1.0
            || self.min_lane_share > self.max_lane_share
        {
            return Err(SchedulerError::InvalidInput(
                "portfolio shares must be finite, positive, ordered, and at most one".into(),
            ));
        }
        let lane_count = self.lanes_enabled.len() as f64;
        if self.min_lane_share * lane_count > 1.0 + f64::EPSILON
            || self.max_lane_share * lane_count < 1.0 - f64::EPSILON
        {
            return Err(SchedulerError::InvalidInput(
                "portfolio lane bounds cannot sum to one".into(),
            ));
        }
        if self.rebalance_cadence_generations == 0 {
            return Err(SchedulerError::InvalidInput(
                "portfolio rebalance cadence must be non-zero".into(),
            ));
        }
        Ok(())
    }

    pub fn allocate(
        &self,
        seed: u64,
        generation: u32,
        frontier: &[PortfolioCandidate],
    ) -> Result<PortfolioAllocation, SchedulerError> {
        self.validate()?;
        if !generation.is_multiple_of(self.rebalance_cadence_generations) {
            return Err(SchedulerError::InvalidInput(
                "portfolio allocation requested outside a declared rebalance boundary".into(),
            ));
        }
        let enabled: BTreeSet<_> = self.lanes_enabled.iter().copied().collect();
        let mut signals: BTreeMap<_, f64> = enabled.iter().map(|lane| (*lane, 0.0)).collect();
        let mut counterfactual_candidates = Vec::with_capacity(frontier.len());
        let mut seen_candidates = BTreeSet::new();
        for item in frontier {
            if item.id.digest() == &Digest::ZERO {
                return Err(SchedulerError::InvalidInput(
                    "portfolio candidate id must be non-zero".into(),
                ));
            }
            if !item.critic_score.is_finite()
                || !item.uncertainty.is_finite()
                || !(0.0..=1.0).contains(&item.uncertainty)
            {
                return Err(SchedulerError::InvalidInput(
                    "critic scores must be finite and uncertainty must be in [0, 1]".into(),
                ));
            }
            if item.eligible_lanes.is_empty() || !item.eligible_lanes.is_subset(&enabled) {
                return Err(SchedulerError::InvalidInput(
                    "candidate lane eligibility must be non-empty and enabled".into(),
                ));
            }
            if !seen_candidates.insert(item.id) {
                return Err(SchedulerError::InvalidInput(
                    "portfolio frontier contains a duplicate candidate".into(),
                ));
            }
            counterfactual_candidates.push(item.id);
            let score = f64::from(item.critic_score.tanh());
            let uncertainty = f64::from(item.uncertainty);
            for lane in &item.eligible_lanes {
                let signal = match lane {
                    PortfolioLane::Exploitation => (1.0 + score) * 0.5,
                    PortfolioLane::NearFrontier => 1.0 - score.abs(),
                    PortfolioLane::Exploratory => uncertainty,
                    PortfolioLane::Adversarial => (1.0 - score) * 0.5,
                    PortfolioLane::Speculative => uncertainty * (1.0 + score) * 0.5,
                };
                *signals.get_mut(lane).expect("eligibility was validated") += signal;
            }
        }

        counterfactual_candidates.sort_unstable();
        counterfactual_candidates.dedup();
        let mut lane_shares: BTreeMap<_, _> = enabled
            .iter()
            .map(|lane| (*lane, self.min_lane_share))
            .collect();
        let mut remaining = 1.0 - self.min_lane_share * enabled.len() as f64;
        let mut active = enabled;
        while remaining > 1e-12 && !active.is_empty() {
            let signal_total: f64 = active.iter().map(|lane| signals[lane] + f64::EPSILON).sum();
            let before = remaining;
            let mut distributed = 0.0;
            let mut saturated = Vec::new();
            for lane in &active {
                let requested = before * (signals[lane] + f64::EPSILON) / signal_total;
                let share = lane_shares.get_mut(lane).expect("enabled lane has a share");
                let available = (self.max_lane_share - *share).max(0.0);
                let increment = requested.min(available);
                *share += increment;
                distributed += increment;
                if available - increment <= 1e-12 {
                    saturated.push(*lane);
                }
            }
            remaining -= distributed;
            for lane in saturated {
                active.remove(&lane);
            }
            if distributed <= 1e-15 {
                return Err(SchedulerError::InvalidInput(
                    "portfolio bounds left unallocatable share".into(),
                ));
            }
        }
        // Correct accumulated floating-point error without violating a lane cap.
        let total: f64 = lane_shares.values().sum();
        let correction = 1.0 - total;
        if correction.abs() > 1e-12 {
            let lane = lane_shares
                .iter()
                .find(|(_, share)| {
                    **share + correction >= self.min_lane_share
                        && **share + correction <= self.max_lane_share
                })
                .map(|(lane, _)| *lane)
                .ok_or_else(|| {
                    SchedulerError::InvalidInput(
                        "portfolio numerical correction violates lane bounds".into(),
                    )
                })?;
            *lane_shares.get_mut(&lane).expect("selected lane exists") += correction;
        }

        Ok(PortfolioAllocation {
            seed,
            generation,
            lane_shares,
            counterfactual_candidates,
        })
    }
}

/// Evidence required before a proposal-role checkpoint can receive any compute.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposalActivationEvidence {
    pub checkpoint: ModelCheckpointId,
    pub checkpoint_role: ModelRole,
    pub fixed_pool: FixedPoolTasteEvaluation,
    pub temporal_split_manifest: Digest,
    pub deterministic_baseline_report: Digest,
    pub training_window_end: u64,
    pub confirmatory_window_start: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FixedPoolTasteEvaluation {
    pub report: Digest,
    pub candidate_registered_utility: f64,
    pub best_baseline_registered_utility: f64,
    pub candidate_compute_units: u64,
    pub baseline_compute_units: u64,
    pub held_out_items: u64,
}

impl FixedPoolTasteEvaluation {
    fn validate_pass(&self) -> Result<(), SchedulerError> {
        if self.report == Digest::ZERO
            || !self.candidate_registered_utility.is_finite()
            || !self.best_baseline_registered_utility.is_finite()
            || self.candidate_compute_units == 0
            || self.candidate_compute_units != self.baseline_compute_units
            || self.held_out_items == 0
        {
            return Err(SchedulerError::InvalidInput(
                "taste gate requires finite held-out utility under equal non-zero compute".into(),
            ));
        }
        if self.candidate_registered_utility <= self.best_baseline_registered_utility {
            return Err(SchedulerError::InvalidInput(
                "taste critic did not beat the best registered fixed-pool baseline".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposerSafetyGate {
    enabled_checkpoint: Option<ModelCheckpointId>,
    pub max_payload_bytes: usize,
    pub max_batch_count: usize,
    pub max_pending_verifications: usize,
    pub generation_budget: u64,
    pub verification_budget: u64,
    pub max_portfolio_share: f64,
}

impl Default for ProposerSafetyGate {
    fn default() -> Self {
        Self {
            enabled_checkpoint: None,
            max_payload_bytes: 4096,
            max_batch_count: 64,
            max_pending_verifications: 64,
            generation_budget: 1024,
            verification_budget: 256,
            max_portfolio_share: 0.05,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProposalRejection {
    InvalidSyntax,
    OutOfBounds,
    CheapFalsification,
    Duplicate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProposalValidation {
    Valid {
        evidence: Digest,
    },
    Rejected {
        reason: ProposalRejection,
        evidence: Digest,
    },
}

impl ProposalValidation {
    fn evidence(&self) -> Digest {
        match self {
            Self::Valid { evidence } | Self::Rejected { evidence, .. } => *evidence,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProposalBudgetUsage {
    pub generated: u64,
    pub queued_for_verification: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProposalAdmission {
    pub checkpoint: ModelCheckpointId,
    pub requires_ordinary_verification: Vec<usize>,
    pub rejected: Vec<(usize, ProposalRejection)>,
}

impl ProposerSafetyGate {
    pub fn enabled_checkpoint(&self) -> Option<ModelCheckpointId> {
        self.enabled_checkpoint
    }

    pub fn activate(
        &mut self,
        evidence: &ProposalActivationEvidence,
    ) -> Result<(), SchedulerError> {
        if evidence.checkpoint.digest() == &Digest::ZERO
            || evidence.temporal_split_manifest == Digest::ZERO
            || evidence.deterministic_baseline_report == Digest::ZERO
        {
            return Err(SchedulerError::InvalidInput(
                "proposal activation requires non-zero checkpoint and evaluation evidence".into(),
            ));
        }
        if evidence.checkpoint_role != ModelRole::Proposal {
            return Err(SchedulerError::InvalidInput(
                "only a proposal-role checkpoint can activate the proposer".into(),
            ));
        }
        evidence.fixed_pool.validate_pass()?;
        if evidence.training_window_end >= evidence.confirmatory_window_start {
            return Err(SchedulerError::InvalidInput(
                "proposal and confirmatory critic windows must be temporally disjoint".into(),
            ));
        }
        if self.max_payload_bytes == 0
            || self.max_batch_count == 0
            || self.max_pending_verifications == 0
            || self.generation_budget == 0
            || self.verification_budget == 0
            || !self.max_portfolio_share.is_finite()
            || self.max_portfolio_share <= 0.0
            || self.max_portfolio_share > 0.05
        {
            return Err(SchedulerError::InvalidInput(
                "proposal safety limits must be bounded and learned share cannot exceed 5%".into(),
            ));
        }
        self.enabled_checkpoint = Some(evidence.checkpoint);
        Ok(())
    }

    pub fn admit(
        &self,
        batch: &reflex_ml_core::ProposalBatch,
        validations: &[ProposalValidation],
        usage: &ProposalBudgetUsage,
        pending_verifications: usize,
    ) -> Result<ProposalAdmission, SchedulerError> {
        let checkpoint = self
            .enabled_checkpoint
            .ok_or_else(|| SchedulerError::InvalidInput("learned proposer is disabled".into()))?;
        if batch.provenance.model_id != checkpoint {
            return Err(SchedulerError::InvalidInput(
                "proposal batch checkpoint does not match activated checkpoint".into(),
            ));
        }
        let count = batch.candidate_payloads.len();
        if count == 0
            || count > self.max_batch_count
            || batch.prior_scores.len() != count
            || validations.len() != count
            || batch
                .candidate_payloads
                .iter()
                .any(|payload| payload.is_empty() || payload.len() > self.max_payload_bytes)
            || batch.prior_scores.iter().any(|score| !score.is_finite())
        {
            return Err(SchedulerError::InvalidInput(
                "proposal batch violates count, payload, validation, or score bounds".into(),
            ));
        }
        if validations
            .iter()
            .any(|validation| validation.evidence() == Digest::ZERO)
        {
            return Err(SchedulerError::InvalidInput(
                "proposal validation requires non-zero evidence".into(),
            ));
        }
        let count_u64 = u64::try_from(count)
            .map_err(|_| SchedulerError::InvalidInput("proposal count exceeds u64".into()))?;
        if usage.generated.saturating_add(count_u64) > self.generation_budget {
            return Err(SchedulerError::InvalidInput(
                "proposal generation budget exhausted".into(),
            ));
        }
        let valid_count = validations
            .iter()
            .filter(|validation| matches!(validation, ProposalValidation::Valid { .. }))
            .count();
        let valid_u64 = u64::try_from(valid_count)
            .map_err(|_| SchedulerError::InvalidInput("valid proposal count exceeds u64".into()))?;
        if usage.queued_for_verification.saturating_add(valid_u64) > self.verification_budget
            || pending_verifications.saturating_add(valid_count) > self.max_pending_verifications
        {
            return Err(SchedulerError::InvalidInput(
                "proposal verifier queue or budget exhausted".into(),
            ));
        }

        let mut requires_ordinary_verification = Vec::with_capacity(valid_count);
        let mut rejected = Vec::with_capacity(count - valid_count);
        for (index, validation) in validations.iter().enumerate() {
            match validation {
                ProposalValidation::Valid { .. } => {
                    requires_ordinary_verification.push(index);
                }
                ProposalValidation::Rejected { reason, .. } => rejected.push((index, *reason)),
            }
        }
        Ok(ProposalAdmission {
            checkpoint,
            requires_ordinary_verification,
            rejected,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_inputs() -> ImmutableInputs {
        ImmutableInputs {
            code: Digest::hash_blake3(b"code"),
            image: Some(Digest::hash_sha256(b"image")),
            domain: Digest::hash_blake3(b"domain"),
            toolchain: Digest::hash_blake3(b"toolchain"),
            corpus: Digest::hash_blake3(b"corpus"),
            split: Digest::hash_blake3(b"split"),
            feature_schema: FeatureSchemaId::from_digest(Digest::hash_blake3(b"features")),
            action_schema: ActionSchemaId::from_digest(Digest::hash_blake3(b"actions")),
            policy: Digest::hash_blake3(b"policy"),
            model_checkpoint: None,
            knowledge_edition: None,
            cache_snapshot: Some(Digest::hash_blake3(b"cache")),
            search: SearchConfig {
                algorithm: "best-first-v1".to_string(),
                action_budget: 1,
                node_budget: 1,
                cpu_seconds: 1.0,
                exploration_uniform: 0.0,
            },
            resource: ResourceRequirements {
                class: "reference-4vcpu-8gb".to_string(),
                cpu_permits: 4,
                memory_bytes: 8 * 1024 * 1024,
                scratch_bytes: 1024,
            },
            verifier: VerifierId::from_digest(Digest::hash_blake3(b"verifier")),
            utility_evaluators: vec![EvaluatorId::from_digest(Digest::hash_blake3(b"utility"))],
            analysis_plan: Digest::hash_blake3(b"analysis"),
        }
    }

    #[test]
    fn test_generation_coordinator_lifecycle() {
        let mut coord = GenerationCoordinator::new(
            StopPolicy {
                max_generations: 3,
                max_cpu_hours: 10.0,
                no_promotion_generations: 2,
            },
            PromotionPolicy {
                required_lineages: 1,
                max_inference_share: 0.1,
                min_top1_viable_rate: 0.6,
                min_mrr: 0.5,
            },
            None,
            None,
        );

        assert_eq!(coord.state, GenerationState::Bootstrap);
        coord.transition_to(GenerationState::Collecting).unwrap();
        assert_eq!(coord.state, GenerationState::Collecting);

        let report_pass = EvaluationReport {
            top1_viable_rate: 0.75,
            mrr_cheapest_route: 0.8,
            ..Default::default()
        };

        let cand_id = ModelCheckpointId::from_digest(Digest::hash_blake3(b"cand1"));
        let evidence = AcceptedBenchmarkEvidence {
            benchmark: "scorer_p95".to_string(),
            host_class: "reference-4vcpu-8gb".to_string(),
            baseline_p95_ns: 100_000.0,
            candidate_p95_ns: 100_000.0,
            baseline_evidence: Digest::hash_blake3(b"baseline-samples"),
            candidate_evidence: Digest::hash_blake3(b"candidate-samples"),
            host_calibration: Digest::hash_blake3(b"host-calibration"),
            inference_cpu_ns: 5,
            total_search_cpu_ns: 100,
            lineages: vec![Digest::hash_blake3(b"lineage-1")],
            acceptance_receipt: Digest::hash_blake3(b"benchmark-acceptance"),
        };
        let next_state = coord
            .simulate_evaluation_result(&report_pass, &[evidence], cand_id)
            .unwrap();
        assert_eq!(next_state, GenerationState::Promoted);
        assert_eq!(coord.active_model(), Some(cand_id));

        // Test that illegal transitions are rejected
        let mut coord2 = GenerationCoordinator::new(
            StopPolicy::default(),
            PromotionPolicy::default(),
            None,
            None,
        );
        let err = coord2.transition_to(GenerationState::Evaluating);
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn durable_coordinator_reconstructs_committed_state() {
        let meta = reflex_meta::MemoryMetaStore::new();
        let experiment_id = ExperimentId::from_digest(Digest::hash_blake3(b"durable-exp"));
        let generation_id = GenerationId::from_digest(Digest::hash_blake3(b"durable-gen"));
        meta.create_experiment(reflex_meta::NewExperiment {
            id: experiment_id,
            name: "durable".to_string(),
            domain: "fixture".to_string(),
            manifest_digest: Digest::hash_blake3(b"durable-manifest"),
        })
        .await
        .unwrap();
        let mut coordinator = DurableGenerationCoordinator::create(
            &meta,
            NewGenerationRecord {
                id: generation_id,
                experiment_id,
                ordinal: 1,
                cause: "experiment start".to_string(),
                operator_id: "scheduler".to_string(),
            },
        )
        .await
        .unwrap();
        coordinator
            .transition(GenerationTransitionCommand {
                command_id: Digest::hash_blake3(b"start-collection"),
                generation_id,
                expected: GenerationState::Bootstrap,
                next: GenerationState::Collecting,
                artifacts: Vec::new(),
                cause: "collection scheduled".to_string(),
                operator_id: "scheduler".to_string(),
                failure: None,
            })
            .await
            .unwrap();
        drop(coordinator);

        let reconstructed = DurableGenerationCoordinator::reconstruct(&meta, generation_id)
            .await
            .unwrap();
        assert_eq!(reconstructed.record().state, GenerationState::Collecting);
        assert_eq!(reconstructed.record().revision, 1);
        assert_eq!(reconstructed.record().cause, "collection scheduled");
    }

    #[test]
    fn test_six_percent_scorer_regression_blocks_promotion() {
        let policy = PromotionPolicy::default();
        let report = EvaluationReport {
            top1_viable_rate: 0.9,
            mrr_cheapest_route: 0.9,
            score_entropy: 0.05,
            ..Default::default()
        };
        let ok_ev = AcceptedBenchmarkEvidence {
            benchmark: "scorer_p95".to_string(),
            host_class: "reference-4vcpu-8gb".to_string(),
            baseline_p95_ns: 100_000.0,
            candidate_p95_ns: 104_000.0,
            baseline_evidence: Digest::hash_blake3(b"baseline-samples"),
            candidate_evidence: Digest::hash_blake3(b"candidate-samples"),
            host_calibration: Digest::hash_blake3(b"host-calibration"),
            inference_cpu_ns: 5,
            total_search_cpu_ns: 100,
            lineages: (0..4).map(|index| Digest::hash_blake3(&[index])).collect(),
            acceptance_receipt: Digest::hash_blake3(b"benchmark-acceptance"),
        };
        assert!(
            policy
                .evaluate_promotion(&report, std::slice::from_ref(&ok_ev))
                .unwrap()
        );

        let regressed_ev = AcceptedBenchmarkEvidence {
            candidate_p95_ns: 106_000.0,
            ..ok_ev
        };
        assert!(policy.evaluate_promotion(&report, &[regressed_ev]).is_err());
    }

    #[test]
    fn benchmark_acceptance_requires_reconstructable_authority() {
        let report = EvaluationReport {
            top1_viable_rate: 1.0,
            mrr_cheapest_route: 1.0,
            ..Default::default()
        };
        let mut evidence = accepted_benchmark();
        evidence.baseline_evidence = Digest::ZERO;
        assert!(
            PromotionPolicy::default()
                .evaluate_promotion(&report, &[evidence])
                .is_err()
        );
    }

    #[test]
    fn manifest_identity_failure_never_falls_back() {
        let mut inputs = test_inputs();
        inputs.search.cpu_seconds = f64::NAN;
        let manifest = ExperimentManifest {
            schema: "reflex.experiment.v1".to_string(),
            labels: ManifestLabels {
                name: "invalid".to_string(),
                comment: None,
            },
            mode: ExperimentMode::Registered,
            inputs,
            seeds: vec![1],
        };
        assert!(manifest.manifest_digest().is_err());
    }

    #[test]
    fn labels_do_not_change_identity_but_semantic_inputs_do() {
        let first = ExperimentManifest {
            schema: "reflex.experiment.v1".to_string(),
            labels: ManifestLabels {
                name: "first label".to_string(),
                comment: None,
            },
            mode: ExperimentMode::Registered,
            inputs: test_inputs(),
            seeds: vec![7],
        };
        let mut relabeled = first.clone();
        relabeled.labels.name = "renamed".to_string();
        relabeled.labels.comment = Some("descriptive only".to_string());
        assert_eq!(
            first.manifest_digest().unwrap(),
            relabeled.manifest_digest().unwrap()
        );

        let mut changed = first.clone();
        changed.inputs.verifier =
            VerifierId::from_digest(Digest::hash_blake3(b"different-verifier"));
        assert_ne!(
            first.manifest_digest().unwrap(),
            changed.manifest_digest().unwrap()
        );
    }

    #[test]
    fn cell_manifest_rejects_mutable_tags_and_reports_identity_diff() {
        let first = CellManifest {
            schema: "reflex.cell.v1".to_string(),
            experiment_id: ExperimentId::from_digest(Digest::hash_blake3(b"experiment")),
            generation_id: GenerationId::from_digest(Digest::hash_blake3(b"generation")),
            task_id: TaskId::from_digest(Digest::hash_blake3(b"task")),
            seed: 9,
            inputs: test_inputs(),
        };
        let mut changed = first.clone();
        changed.inputs.search.node_budget += 1;
        assert_ne!(first.cell_id().unwrap(), changed.cell_id().unwrap());
        assert_eq!(
            changed.diff_identity(&first).unwrap()[0].field,
            "inputs.search"
        );

        changed.inputs.resource.class = "worker:latest".to_string();
        assert!(changed.cell_id().is_err());
    }

    #[test]
    fn compatibility_digest_contains_exact_execution_abi_fields() {
        let base = test_inputs();
        let digest = base.compatibility_digest().unwrap();
        type InputMutation = fn(&mut ImmutableInputs);
        let mutations: [InputMutation; 7] = [
            |inputs| inputs.code = Digest::hash_blake3(b"code-2"),
            |inputs| inputs.image = Some(Digest::hash_blake3(b"image-2")),
            |inputs| inputs.domain = Digest::hash_blake3(b"domain-2"),
            |inputs| inputs.toolchain = Digest::hash_blake3(b"toolchain-2"),
            |inputs| {
                inputs.feature_schema =
                    FeatureSchemaId::from_digest(Digest::hash_blake3(b"features-2"));
            },
            |inputs| {
                inputs.action_schema =
                    ActionSchemaId::from_digest(Digest::hash_blake3(b"actions-2"));
            },
            |inputs| {
                inputs.verifier = VerifierId::from_digest(Digest::hash_blake3(b"verifier-2"));
            },
        ];
        for mutate in mutations {
            let mut changed = base.clone();
            mutate(&mut changed);
            assert_ne!(changed.compatibility_digest().unwrap(), digest);
        }

        let mut scientific_only = base.clone();
        scientific_only.corpus = Digest::hash_blake3(b"corpus-2");
        scientific_only.split = Digest::hash_blake3(b"split-2");
        scientific_only.policy = Digest::hash_blake3(b"policy-2");
        scientific_only.analysis_plan = Digest::hash_blake3(b"analysis-2");
        scientific_only.search.node_budget += 1;
        assert_eq!(scientific_only.compatibility_digest().unwrap(), digest);
    }

    fn collection_plan() -> CollectionPlan {
        CollectionPlan {
            plan_digest: Digest::hash_blake3(b"collection-plan"),
            lanes: vec![
                LaneAllocation {
                    lane: CollectionLane::Stable,
                    logical_episodes: 2,
                },
                LaneAllocation {
                    lane: CollectionLane::Uniform,
                    logical_episodes: 1,
                },
                LaneAllocation {
                    lane: CollectionLane::Heuristic,
                    logical_episodes: 1,
                },
            ],
            minimum_accepted_experiences: 2,
            required_strata: BTreeMap::from([("hard".into(), 1), ("easy".into(), 1)]),
            max_infrastructure_attempts: 3,
        }
    }

    #[test]
    fn collection_gate_rejects_retry_double_count_and_missing_strata() {
        let cell = CellId::from_digest(Digest::hash_blake3(b"cell-a"));
        let duplicate = vec![
            CollectionObservation {
                cell_id: cell,
                accepted_attempt_no: Some(1),
                stratum: "hard".into(),
                exclusion: None,
            },
            CollectionObservation {
                cell_id: cell,
                accepted_attempt_no: Some(2),
                stratum: "hard".into(),
                exclusion: None,
            },
        ];
        assert!(evaluate_collection_completion(&collection_plan(), &duplicate).is_err());
        let missing = vec![CollectionObservation {
            cell_id: cell,
            accepted_attempt_no: Some(2),
            stratum: "hard".into(),
            exclusion: None,
        }];
        assert!(evaluate_collection_completion(&collection_plan(), &missing).is_err());
    }

    fn accepted_benchmark() -> AcceptedBenchmarkEvidence {
        AcceptedBenchmarkEvidence {
            benchmark: "scorer_p95".into(),
            host_class: "reference-4vcpu-8gb".into(),
            baseline_p95_ns: 100_000.0,
            candidate_p95_ns: 100_000.0,
            baseline_evidence: Digest::hash_blake3(b"baseline-samples"),
            candidate_evidence: Digest::hash_blake3(b"candidate-samples"),
            host_calibration: Digest::hash_blake3(b"host-calibration"),
            inference_cpu_ns: 5,
            total_search_cpu_ns: 100,
            lineages: (0..4).map(|index| Digest::hash_blake3(&[index])).collect(),
            acceptance_receipt: Digest::hash_blake3(b"benchmark-acceptance"),
        }
    }

    fn matched_slice(kind: EvaluationSliceKind, name: &str) -> MatchedEvaluationSlice {
        MatchedEvaluationSlice {
            kind,
            name: name.into(),
            matched_population: Digest::hash_blake3(name.as_bytes()),
            matched_cells: 100,
            candidate_solved: 90,
            stable_solved: 90,
            cheap_baseline_solved: 80,
            candidate_cpu_ns: 90,
            stable_cpu_ns: 100,
            cheap_baseline_cpu_ns: 100,
            candidate_verified_actions: 70,
            stable_verified_actions: 100,
        }
    }

    #[test]
    fn matched_stratum_regression_vetoes_promotion() {
        let report = EvaluationReport {
            top1_viable_rate: 0.9,
            mrr_cheapest_route: 0.9,
            ..Default::default()
        };
        let mut evidence = MatchedPromotionEvidence {
            evaluation_report_digest: Digest::hash_blake3(b"accepted-report"),
            candidate: ModelCheckpointId::from_digest(Digest::hash_blake3(b"candidate")),
            stable: Some(ModelCheckpointId::from_digest(Digest::hash_blake3(
                b"stable",
            ))),
            lineages: (0..4).map(|index| Digest::hash_blake3(&[index])).collect(),
            benchmark_evidence: vec![accepted_benchmark()],
            slices: vec![
                matched_slice(EvaluationSliceKind::Overall, "all"),
                matched_slice(EvaluationSliceKind::Family, "arithmetic"),
                matched_slice(EvaluationSliceKind::Stratum, "hard"),
            ],
        };
        let policy = RegisteredPromotionPolicy::default();
        assert!(policy.evaluate(&report, &evidence).unwrap());
        evidence.slices[2].candidate_solved = 80;
        assert!(!policy.evaluate(&report, &evidence).unwrap());
    }

    #[test]
    fn stop_receipt_uses_realized_economics_and_active_cell_bound() {
        let policy = RegisteredStopPolicy {
            max_generations: 10,
            max_summed_cpu_hours: 10.0,
            max_machine_hours: 20.0,
            max_wall_seconds: 1000,
            max_planning_dollars: 50.0,
            no_promotion_generations: 3,
            min_marginal_utility_per_cpu_hour: 0.1,
            target_capability: None,
            target_utility: None,
            cleanup_reserve_machine_hours: 1.0,
        };
        let evidence = StopEvidence {
            economics_digest: Digest::hash_blake3(b"economics"),
            generations_completed: 2,
            consecutive_scientific_rejections: 0,
            summed_cpu_hours: 9.5,
            machine_hours: 3.0,
            wall_seconds: 100,
            cost_dollars: 2.0,
            cost_authority: CostAuthority::PlanningProxy,
            capability: 0.4,
            utility: 1.0,
            marginal_utility_per_cpu_hour: 0.5,
            active_cell_bound_cpu_hours: 0.5,
            operator_stop: false,
        };
        let decision = policy.evaluate(&evidence).unwrap().unwrap();
        assert_eq!(decision.reason, StopReason::CpuLimit);
        assert_ne!(decision.receipt_digest, Digest::ZERO);
    }

    #[tokio::test]
    async fn durable_stage_bundle_promotes_atomically() {
        let meta = reflex_meta::MemoryMetaStore::new();
        let experiment_id = ExperimentId::from_digest(Digest::hash_blake3(b"bundle-exp"));
        let generation_id = GenerationId::from_digest(Digest::hash_blake3(b"bundle-gen"));
        meta.create_experiment(reflex_meta::NewExperiment {
            id: experiment_id,
            name: "bundle".into(),
            domain: "fixture".into(),
            manifest_digest: Digest::hash_blake3(b"manifest"),
        })
        .await
        .unwrap();
        let mut coordinator = DurableGenerationCoordinator::create(
            &meta,
            NewGenerationRecord {
                id: generation_id,
                experiment_id,
                ordinal: 1,
                cause: "start".into(),
                operator_id: "test".into(),
            },
        )
        .await
        .unwrap();
        let plan = collection_plan();
        coordinator.start_collection(&plan, "test").await.unwrap();
        let observations = vec![
            CollectionObservation {
                cell_id: CellId::from_digest(Digest::hash_blake3(b"accepted-hard")),
                accepted_attempt_no: Some(1),
                stratum: "hard".into(),
                exclusion: None,
            },
            CollectionObservation {
                cell_id: CellId::from_digest(Digest::hash_blake3(b"accepted-easy")),
                accepted_attempt_no: Some(2),
                stratum: "easy".into(),
                exclusion: None,
            },
        ];
        let completion = evaluate_collection_completion(&plan, &observations).unwrap();
        coordinator
            .seal_collection(&completion, "test")
            .await
            .unwrap();
        coordinator
            .publish_accepted_evidence(Digest::hash_blake3(b"evidence"), "test")
            .await
            .unwrap();
        coordinator
            .publish_dataset(Digest::hash_blake3(b"dataset"), "test")
            .await
            .unwrap();
        let candidate = ModelCheckpointId::from_digest(Digest::hash_blake3(b"candidate"));
        coordinator
            .publish_checkpoint(candidate, "test")
            .await
            .unwrap();
        let evaluation = Digest::hash_blake3(b"evaluation");
        coordinator
            .publish_evaluation(
                evaluation,
                true,
                RegisteredPromotionPolicy::default().digest().unwrap(),
                "test",
            )
            .await
            .unwrap();
        let result = coordinator
            .promote(
                candidate,
                None,
                evaluation,
                &RegisteredPromotionPolicy::default(),
                "test",
            )
            .await
            .unwrap();
        assert_eq!(result.generation.state, GenerationState::Promoted);
        assert_eq!(
            meta.list_active_models().await.unwrap()[0].checkpoint_id,
            candidate.to_hex()
        );
    }

    #[test]
    fn passing_metrics_without_benchmark_evidence_fail_closed() {
        let report = EvaluationReport {
            top1_viable_rate: 1.0,
            mrr_cheapest_route: 1.0,
            ..Default::default()
        };
        assert!(
            PromotionPolicy::default()
                .evaluate_promotion(&report, &[])
                .is_err()
        );
    }

    fn portfolio_candidate(seed: &[u8], score: f32, uncertainty: f32) -> PortfolioCandidate {
        PortfolioCandidate {
            id: CandidateId::from_digest(Digest::hash_blake3(seed)),
            critic_score: score,
            uncertainty,
            eligible_lanes: PortfolioPolicy::default()
                .lanes_enabled
                .into_iter()
                .collect(),
        }
    }

    #[test]
    fn portfolio_consumes_real_signals_without_starving_lanes() {
        let policy = PortfolioPolicy::default();
        let exploit = policy
            .allocate(
                7,
                3,
                &[
                    portfolio_candidate(b"high", 8.0, 0.0),
                    portfolio_candidate(b"high-2", 7.0, 0.0),
                ],
            )
            .unwrap();
        let explore = policy
            .allocate(
                7,
                3,
                &[
                    portfolio_candidate(b"uncertain", 0.0, 1.0),
                    portfolio_candidate(b"uncertain-2", 0.0, 1.0),
                ],
            )
            .unwrap();
        assert!(
            exploit.lane_shares[&PortfolioLane::Exploitation]
                > explore.lane_shares[&PortfolioLane::Exploitation]
        );
        assert!(
            explore.lane_shares[&PortfolioLane::Exploratory]
                > exploit.lane_shares[&PortfolioLane::Exploratory]
        );
        assert!(
            exploit
                .lane_shares
                .values()
                .all(|share| *share >= policy.min_lane_share)
        );
        assert!((exploit.lane_shares.values().sum::<f64>() - 1.0).abs() < 1e-9);
        assert_eq!(
            exploit.lane_shares,
            policy
                .allocate(
                    7,
                    3,
                    &[
                        portfolio_candidate(b"high", 8.0, 0.0),
                        portfolio_candidate(b"high-2", 7.0, 0.0),
                    ],
                )
                .unwrap()
                .lane_shares
        );
        assert_eq!(exploit.counterfactual_candidates.len(), 2);
    }

    #[test]
    fn portfolio_rejects_invalid_bounds_and_scores() {
        let invalid = PortfolioPolicy {
            min_lane_share: 0.3,
            ..PortfolioPolicy::default()
        };
        assert!(invalid.allocate(0, 0, &[]).is_err());
        assert!(
            PortfolioPolicy::default()
                .allocate(0, 0, &[portfolio_candidate(b"nan", f32::NAN, 0.0)])
                .is_err()
        );
    }

    fn activation(checkpoint: ModelCheckpointId) -> ProposalActivationEvidence {
        ProposalActivationEvidence {
            checkpoint,
            checkpoint_role: ModelRole::Proposal,
            fixed_pool: FixedPoolTasteEvaluation {
                report: Digest::hash_blake3(b"fixed-pool"),
                candidate_registered_utility: 1.1,
                best_baseline_registered_utility: 1.0,
                candidate_compute_units: 100,
                baseline_compute_units: 100,
                held_out_items: 50,
            },
            temporal_split_manifest: Digest::hash_blake3(b"split"),
            deterministic_baseline_report: Digest::hash_blake3(b"baselines"),
            training_window_end: 10,
            confirmatory_window_start: 11,
        }
    }

    fn proposal_batch(checkpoint: ModelCheckpointId) -> reflex_ml_core::ProposalBatch {
        reflex_ml_core::ProposalBatch {
            candidate_payloads: vec![vec![1], vec![2]],
            prior_scores: vec![0.9, 0.1],
            provenance: reflex_ml_core::ProposalProvenance {
                model_id: checkpoint,
                generation_step: 1,
                seed: 2,
            },
            metadata: "fixture".into(),
        }
    }

    #[test]
    fn proposer_fails_closed_until_taste_and_role_gates_pass() {
        let checkpoint = ModelCheckpointId::from_digest(Digest::hash_blake3(b"proposal"));
        let batch = proposal_batch(checkpoint);
        let validations = vec![
            ProposalValidation::Valid {
                evidence: Digest::hash_blake3(b"syntax"),
            },
            ProposalValidation::Rejected {
                reason: ProposalRejection::CheapFalsification,
                evidence: Digest::hash_blake3(b"falsification"),
            },
        ];
        let mut gate = ProposerSafetyGate::default();
        assert!(
            gate.admit(&batch, &validations, &ProposalBudgetUsage::default(), 0)
                .is_err()
        );
        let mut wrong_role = activation(checkpoint);
        wrong_role.checkpoint_role = ModelRole::Ranker;
        assert!(gate.activate(&wrong_role).is_err());
        let mut leaked_window = activation(checkpoint);
        leaked_window.confirmatory_window_start = leaked_window.training_window_end;
        assert!(gate.activate(&leaked_window).is_err());
        let mut losing_critic = activation(checkpoint);
        losing_critic.fixed_pool.candidate_registered_utility = 0.9;
        assert!(gate.activate(&losing_critic).is_err());

        gate.activate(&activation(checkpoint)).unwrap();
        let admitted = gate
            .admit(&batch, &validations, &ProposalBudgetUsage::default(), 0)
            .unwrap();
        assert_eq!(admitted.requires_ordinary_verification, vec![0]);
        assert_eq!(
            admitted.rejected,
            vec![(1, ProposalRejection::CheapFalsification)]
        );
    }

    #[test]
    fn proposer_bounds_invalid_bursts_before_verifier_queue() {
        let checkpoint = ModelCheckpointId::from_digest(Digest::hash_blake3(b"proposal"));
        let mut gate = ProposerSafetyGate {
            max_pending_verifications: 1,
            ..ProposerSafetyGate::default()
        };
        gate.activate(&activation(checkpoint)).unwrap();
        let batch = proposal_batch(checkpoint);
        let validations = vec![
            ProposalValidation::Valid {
                evidence: Digest::hash_blake3(b"one"),
            },
            ProposalValidation::Valid {
                evidence: Digest::hash_blake3(b"two"),
            },
        ];
        assert!(
            gate.admit(&batch, &validations, &ProposalBudgetUsage::default(), 0)
                .is_err()
        );
    }
}
