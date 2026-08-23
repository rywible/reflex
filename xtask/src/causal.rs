use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::Write;
use std::num::{NonZeroU64, NonZeroUsize};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use atomic_write_file::AtomicWriteFile;
use cpu_time::ProcessTime;
#[cfg(test)]
use reflex::internal_experiments::{
    IntelligenceComponentInspection, inspect_intelligence_components,
    inspect_knowledge_revision_segment,
};
use reflex::internal_experiments::{IntelligenceTreatmentSource, ablate_intelligence_checkpoint};
use reflex::{
    BundlePlan, Completion, Direction, DomainDefinition, GoalSet, ImprovementRequest,
    MeasurementConstraint, NonEmpty, NonZeroDuration, Objective, OptimizationGoal, Preference,
    ResourceEnvelope, StructuralProtocol, SuccessCondition, ThresholdRelation, improve,
};
use reflex_bitvec::{BitVecDomain, Expression, Metric, SeedScope};
use reflex_bundle::{CanonicalBundle, SegmentKind};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::harness::{
    AnyError, HostEnvironment, capture_large_campaign_child, completion_name, duration_ns,
    environment, hash_file, hash_json, hex, require_absent, require_clean, require_release,
};

const SPEC_VERSION: &str = "reflex-u8-causal-confirmation-v6";
const DOMAIN_IDENTITY: &str =
    "reflex-bitvec/u8/unary/full-ops/masked-shifts/select-nonzero/canonical-dag/v4";
const EXPECTED_SPEC_SHA256: &str =
    "a6afd9585d5c406488d29401373f25fdff979c2d36424d63f1dc3875e7fef166";
const BOOTSTRAP_COMPARATOR_REPORT: &str = "docs/baselines/bootstrap-reference-domain-v7.json";
const BOOTSTRAP_COMPARATOR_FILE_SHA256: &str =
    "b8a5e2c8ef87a88c4576d934e8d9e234961942cc599973d789a5fe4e29ad6daa";
const BOOTSTRAP_COMPARATOR_CONTENT_SHA256: &str =
    "18af93061337a068e4c883dd149632d87b8d197d2ff33ad87e1c074aedf1f117";
const BOOTSTRAP_COMPARATOR_PROTOCOL_SHA256: &str =
    "14b108b35f336c134fa94139de5430adba0904f8471ffa44de4fb7c55fc0b880";
const BOOTSTRAP_COMPARATOR_SEMANTIC_SHA256: &str =
    "ecaf1feba1d8d45511b2b3b01fd85d0a9be829ac48814f3bc29e9daa1b8d5582";
const CONSUMED_V1_REPORT: &str = "docs/experiments/u8-causal-confirmation-v1.json";
const CONSUMED_V1_AUDIT_SHA256: &str =
    "7c8d87d90691502a55396e3cb70561bbd63cc7179d213879f93d6c5e9bb1a81c";
const CONSUMED_V2_REPORT: &str = "docs/experiments/u8-causal-confirmation-v2-consumed-audit.json";
const CONSUMED_V2_AUDIT_SHA256: &str =
    "ad7b01320496b67cecabd97aea949c7e0a198945eee45faad07d811e31b2e081";
const CONSUMED_V3_REPORT: &str = "docs/experiments/u8-causal-confirmation-v3-consumed-audit.json";
const CONSUMED_V3_AUDIT_SHA256: &str =
    "585c9e7ec1f2c64fb34fb2d9a300e72d5d29c2ea3ff34fca250807c4d990aaaf";
const CONSUMED_V4_REPORT: &str = "docs/experiments/u8-causal-confirmation-v4-consumed-audit.json";
const CONSUMED_V4_AUDIT_SHA256: &str =
    "0eae44e4ca7a2ba050f02ab87c0e2de27fecde457d8744636e29afdd6d404787";
const CONSUMED_V5_REPORT: &str = "docs/experiments/u8-causal-confirmation-v5-consumed-audit.json";
const CONSUMED_V5_AUDIT_SHA256: &str =
    "5e1e3ad15fb06719b79855fbd18ce057c8f536f9d5e53d8575e13c9c4671a2ce";
const CASES_PER_REPLICATE: usize = 8_190;
const REPLICATES: usize = 10;
const VERIFICATION_REQUESTS: u64 = 10_500;
const RESIDENT_BYTES: u64 = 256 * 1024 * 1024;
const DURABLE_BYTES: u64 = 256 * 1024 * 1024;
const TIME_SECONDS: u64 = 60;
const CHILD_TIMEOUT_SECONDS: u64 = 120;
const HISTORICAL_V5_FULL_WALL_NS: u64 = 24_756_073_867;
const HISTORICAL_V5_BOOTSTRAP_WALL_NS: u64 = 19_340_946_572;
const BOOTSTRAP_THRESHOLD: i64 = 500;
const MODEL_THRESHOLD: i64 = 250;
const DERIVED_THRESHOLD: i64 = 200;
const RESAMPLES: usize = 10_000;
const PILOT_EXCLUSION_CASES: usize = 8_190;
const AUDIT_SEEDS: [&str; REPLICATES] = [
    "1f9944407c25de385e09287d6dd98f96ff160a9144d1f25ad692f07ac20e5a52",
    "f847251d171b938af061eb406675aed7fbaa43d5d30676114a7534a09528e898",
    "6e671b8a2443560f4f737f47bfe30207292eafdc30c1de424717dca4d2986335",
    "763b23a856a3f3c875639c4e687c1ecb7a278367e52f8b686570884f5071a903",
    "f99c8f5845fcee12e37a4019e30b37249d58b57417ad67568477bbe06591cf09",
    "5ecb3f43906d95a81b5f8e2b68cf2660f7b8d4256292fe6498b75b7b4c331ca7",
    "13a93f05e712adcf5151c2ae4008776823e6b706bd3be3e827ff957fb54ce26c",
    "058bdd4545bd85d53338c5567b6fe8fb41abcc94c7b7fd722d21cded1a4fb81b",
    "df4e80c1bf19a3bc6d7aa1bba5fdad046f756b69c91966ef934da1e443a7c164",
    "a557f8887aa3bfbcb9358f9707b70fc062ba238d26bd3d9c698b968f0c1cb023",
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Treatment {
    Full,
    NoModel,
    NoDerived,
    Bootstrap,
}

impl Treatment {
    const ALL: [Self; 4] = [Self::Full, Self::NoModel, Self::NoDerived, Self::Bootstrap];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::NoModel => "no-model",
            Self::NoDerived => "no-derived",
            Self::Bootstrap => "bootstrap",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CorpusRecord {
    first_constant: u8,
    first_rotation: u8,
    second_constant: u8,
    second_rotation: u8,
    category: u8,
    semantic_sha256: String,
}

#[derive(Deserialize)]
struct ConsumedReport {
    audit_corpus_sha256: String,
    audit_corpora: Vec<Vec<CorpusRecord>>,
}

#[derive(Debug, Serialize)]
struct ExperimentSpec {
    version: &'static str,
    hypothesis: &'static str,
    domain_identity: &'static str,
    development_corpus: &'static str,
    consumed_audit_corpus: &'static str,
    historical_bootstrap_calibration: &'static str,
    training_generator: &'static str,
    training_verification_requests: u64,
    pilot_exclusion_cases: usize,
    pilot_generator: &'static str,
    audit_generator: &'static str,
    audit_surface_categories: &'static str,
    semantic_group_digest: &'static str,
    semantic_split: &'static str,
    audit_exposure: &'static str,
    audit_seeds: Vec<&'static str>,
    independent_replicates: usize,
    cases_per_replicate: usize,
    treatments: Vec<&'static str>,
    ablations: &'static str,
    treatment_order: &'static str,
    worker_threads: usize,
    resident_bytes: u64,
    durable_bytes: u64,
    elapsed_seconds: u64,
    cpu_seconds: u64,
    verification_requests: u64,
    child_timeout_seconds: u64,
    primary_outcome: &'static str,
    protected_outcomes: Vec<&'static str>,
    diagnostic_outcomes: Vec<&'static str>,
    practical_thresholds: BTreeMap<&'static str, i64>,
    uncertainty: &'static str,
    bootstrap_algorithm: &'static str,
    multiplicity: &'static str,
    stopping: &'static str,
    exclusions: &'static str,
    decision: &'static str,
    mode: &'static str,
    pareto_scope: &'static str,
    anytime_trace: &'static str,
    recovery: &'static str,
}

#[derive(Deserialize)]
struct BootstrapComparatorReport {
    schema: String,
    protocol_sha256: String,
    environment: BootstrapComparatorEnvironment,
    protocol_deviations: Vec<String>,
    semantic_outcome_sha256: Option<String>,
    runs: Vec<BootstrapComparatorRun>,
    content_sha256: String,
}

#[derive(Deserialize)]
struct BootstrapComparatorEnvironment {
    git_dirty: bool,
}

#[derive(Deserialize)]
struct BootstrapComparatorRun {
    result: Option<BootstrapComparatorChild>,
    failure: Option<String>,
}

#[derive(Deserialize)]
struct BootstrapComparatorChild {
    semantic_outcome_sha256: String,
    recovery_valid: bool,
    recovery_semantic_outcome_sha256: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
struct Aggregates {
    node_count: u64,
    depth: u64,
    peak_live_temporaries: u64,
    encoded_bytes: u64,
    evaluator_operations: u64,
    evaluation_nanoseconds: u64,
}

impl Aggregates {
    fn same_deterministic_measurements(self, other: Self) -> bool {
        self.node_count == other.node_count
            && self.depth == other.depth
            && self.peak_live_temporaries == other.peak_live_temporaries
            && self.encoded_bytes == other.encoded_bytes
            && self.evaluator_operations == other.evaluator_operations
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AnytimePoint {
    observer_sequence: u64,
    pareto_artifacts: usize,
    aggregates: Aggregates,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ParetoArtifactResult {
    artifact_key: String,
    origin_key: String,
    measurements: Aggregates,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ChildResult {
    replicate: usize,
    treatment: Treatment,
    completion: String,
    wall_ns: u64,
    process_cpu_ns: u64,
    reported_elapsed_ns: u64,
    reported_cpu_ns: u64,
    resident_bytes: u64,
    durable_bytes: u64,
    verification_requests: u64,
    pareto_artifacts: usize,
    pareto_results: Vec<ParetoArtifactResult>,
    aggregates: Aggregates,
    anytime_curve: Vec<AnytimePoint>,
    audit_artifact_sha256: String,
    bundle_sha256: String,
    knowledge_revision: String,
    model_revision: String,
    evaluation_valid: bool,
    evaluation_failure: Option<String>,
    recovery_valid: bool,
    recovery_failure: Option<String>,
    recovery_artifact_sha256: Option<String>,
    recovery_verification_requests: Option<u64>,
}

struct ChildAssignment {
    replicate: usize,
    treatment: Treatment,
    corpus: PathBuf,
    target: PathBuf,
}

#[derive(Debug, Serialize)]
struct RecordedRun {
    replicate: usize,
    treatment: Treatment,
    order: usize,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
    result: Option<ChildResult>,
    failure: Option<String>,
}

#[derive(Debug, Serialize)]
struct Contrast {
    comparison: Treatment,
    threshold: i64,
    paired_effects: Vec<i64>,
    mean_effect: f64,
    familywise_99_percent_lower: i64,
    all_positive: bool,
    protected_nonregression: bool,
    passed: bool,
}

#[derive(Debug, Serialize)]
struct Report {
    schema: &'static str,
    specification_sha256: String,
    specification: ExperimentSpec,
    environment: HostEnvironment,
    audit_corpus_sha256: String,
    audit_corpora: Vec<Vec<CorpusRecord>>,
    training_bundle_sha256: String,
    full_knowledge_revision: String,
    full_model_revision: String,
    no_model_bundle_sha256: String,
    no_derived_bundle_sha256: String,
    protocol_deviations: Vec<String>,
    runs: Vec<RecordedRun>,
    contrasts: Vec<Contrast>,
    confirmed: bool,
    reproduction_commands: Vec<&'static str>,
    content_sha256: String,
}

#[derive(Debug, Serialize)]
struct DevelopmentPerformanceReport {
    schema: &'static str,
    status: &'static str,
    warning: &'static str,
    environment: HostEnvironment,
    source_audit_sha256: &'static str,
    historical_full_wall_ns: u64,
    historical_bootstrap_wall_ns: u64,
    full_is_twice_as_fast_as_v5: bool,
    full_is_faster_than_v5_bootstrap: bool,
    outcomes_match_v5: bool,
    phase_reports: BTreeMap<String, Vec<String>>,
    runs: Vec<RecordedRun>,
}

pub(super) fn run_development_performance(arguments: &[String]) -> Result<(), AnyError> {
    require_release("causal-development-performance")?;
    let output = match arguments {
        [flag, path] if flag == "--output" => PathBuf::from(path),
        _ => return Err("causal-development-performance requires --output PATH".into()),
    };
    require_absent(&output, "development performance report")?;
    let environment = environment()?;
    let work = std::env::current_dir()?.join(format!(
        "target/reflex-causal-development-performance-{}",
        std::process::id()
    ));
    if work.exists() {
        return Err(format!(
            "development performance work already exists: {}",
            work.display()
        )
        .into());
    }
    std::fs::create_dir(&work)?;
    let full = work.join("full.bundle");
    let bootstrap_revision = work.join("bootstrap-revision.bundle");
    build_training_bundles(&full, &bootstrap_revision)?;
    std::fs::remove_file(bootstrap_revision)?;
    let consumed = load_consumed_report(CONSUMED_V5_REPORT, CONSUMED_V5_AUDIT_SHA256)?;
    let corpus = consumed
        .audit_corpora
        .into_iter()
        .next()
        .ok_or("consumed v5 audit has no replicate")?;
    let corpus_path = work.join("consumed-v5-replicate-0.json");
    std::fs::write(&corpus_path, serde_json::to_vec(&corpus)?)?;
    let executable = std::env::current_exe()?;
    let mut runs = Vec::new();
    let mut phase_reports = BTreeMap::new();
    for (order, treatment) in [Treatment::Full, Treatment::Bootstrap]
        .into_iter()
        .enumerate()
    {
        let target = work.join(format!("result-{}.bundle", treatment.as_str()));
        if treatment == Treatment::Full {
            std::fs::copy(&full, &target)?;
        }
        let phase_prefix = work.join(format!("phase-{}", treatment.as_str()));
        let (run, phases) = run_assignment_capture(
            &executable,
            0,
            treatment,
            order,
            &corpus_path,
            &target,
            Some(&phase_prefix),
        )?;
        phase_reports.insert(treatment.as_str().into(), phases);
        runs.push(run);
        if target.is_file() {
            std::fs::remove_file(target)?;
        }
    }
    let full_result = runs
        .iter()
        .find(|run| run.treatment == Treatment::Full)
        .and_then(|run| run.result.as_ref());
    let full_is_twice_as_fast_as_v5 = full_result
        .is_some_and(|result| result.wall_ns.saturating_mul(2) <= HISTORICAL_V5_FULL_WALL_NS);
    let full_is_faster_than_v5_bootstrap =
        full_result.is_some_and(|result| result.wall_ns < HISTORICAL_V5_BOOTSTRAP_WALL_NS);
    let outcomes_match_v5 = runs.iter().all(v5_outcome_matches);
    let report = DevelopmentPerformanceReport {
        schema: "reflex-u8-development-performance-v1",
        status: "development-only",
        warning: "Consumed v5 data is Development Corpus; this report is not confirmation evidence.",
        environment,
        source_audit_sha256: CONSUMED_V5_AUDIT_SHA256,
        historical_full_wall_ns: HISTORICAL_V5_FULL_WALL_NS,
        historical_bootstrap_wall_ns: HISTORICAL_V5_BOOTSTRAP_WALL_NS,
        full_is_twice_as_fast_as_v5,
        full_is_faster_than_v5_bootstrap,
        outcomes_match_v5,
        phase_reports,
        runs,
    };
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&output, serde_json::to_vec_pretty(&report)?)?;
    std::fs::remove_file(full)?;
    std::fs::remove_file(corpus_path)?;
    std::fs::remove_dir(work)?;
    println!("wrote {}", output.display());
    if !report.full_is_twice_as_fast_as_v5
        || !report.full_is_faster_than_v5_bootstrap
        || !report.outcomes_match_v5
        || report.runs.iter().any(|run| run.failure.is_some())
    {
        return Err("development Full performance or v5 semantic gate failed".into());
    }
    Ok(())
}

fn v5_outcome_matches(run: &RecordedRun) -> bool {
    let Some(result) = run.result.as_ref() else {
        return false;
    };
    let (aggregates, artifact, knowledge, model) = match run.treatment {
        Treatment::Full => (
            Aggregates {
                node_count: 74_464,
                depth: 50_521,
                peak_live_temporaries: 24_576,
                encoded_bytes: 420_675,
                evaluator_operations: 74_464,
                evaluation_nanoseconds: 0,
            },
            "b6ff0c64e1d1c5f757af661f9e4f61237d376cbaef3cc377aafc5781bc80bbf7",
            "d37a3c443d3bd8716f3ea67c51ca64889f057aa708406de84a50cd7a60d27630",
            "8d3aa2f6d589c13512bee7441054e6f36e2f518a466542e655bdc0761c46993c",
        ),
        Treatment::Bootstrap => (
            Aggregates {
                node_count: 76_381,
                depth: 51_865,
                peak_live_temporaries: 24_576,
                encoded_bytes: 433_917,
                evaluator_operations: 76_381,
                evaluation_nanoseconds: 0,
            },
            "931db3adf299ec538b0544088e32639906799bba8500446c4394b8644a3d2ff9",
            "49d0c999b3f2055aa593151fe27b27376d0e1722f03ec2978315a9a5b6ff4d26",
            "c4770e92e369d55315a49fde7bebacc3662818173ab6025e90ed6f9f392acf3d",
        ),
        Treatment::NoModel | Treatment::NoDerived => return false,
    };
    result.evaluation_valid
        && result.recovery_valid
        && result.pareto_artifacts == CASES_PER_REPLICATE
        && result
            .aggregates
            .same_deterministic_measurements(aggregates)
        && result.audit_artifact_sha256 == artifact
        && result.knowledge_revision == knowledge
        && result.model_revision == model
}

#[expect(
    clippy::too_many_lines,
    reason = "the fixed causal confirmation protocol remains contiguous and auditable"
)]
pub(super) fn run_confirm(arguments: &[String]) -> Result<(), AnyError> {
    let (output, specification, specification_sha256, environment) =
        confirm_configuration(arguments)?;
    let work = std::env::current_dir()?.join("target/reflex-causal-confirmation-v6");
    if work.exists() {
        return Err(format!(
            "causal work directory already exists; preserve and inspect it before proceeding: {}",
            work.display()
        )
        .into());
    }
    std::fs::create_dir_all(&work)?;
    let output = match resolve_report_output(&output, &work) {
        Ok(output) => output,
        Err(error) => {
            std::fs::remove_dir(&work)?;
            return Err(error);
        }
    };
    let full = work.join("full.bundle");
    let bootstrap_revision = work.join("bootstrap-revision.bundle");
    build_training_bundles(&full, &bootstrap_revision)?;
    let treatments = prepare_treatment_bundles(&work, &full, &bootstrap_revision)?;
    let audit_corpora = generate_audit_corpora(&treatments.audit_exposure)?;
    let no_model = treatments.no_model;
    let no_derived = treatments.no_derived;
    let audit_corpus_sha256 = hash_json(&audit_corpora)?;
    persist_audit_corpora(&work, &audit_corpora)?;
    let executable = std::env::current_exe()?;
    let mut runs = Vec::with_capacity(REPLICATES * Treatment::ALL.len());
    for replicate in 0..audit_corpora.len() {
        let corpus_path = work.join(format!("audit-{replicate}.json"));
        for order in 0..Treatment::ALL.len() {
            let treatment = Treatment::ALL[(order + replicate) % Treatment::ALL.len()];
            let template = match treatment {
                Treatment::Full => Some(&full),
                Treatment::NoModel => Some(&no_model),
                Treatment::NoDerived => Some(&no_derived),
                Treatment::Bootstrap => None,
            };
            let target = work.join(format!("result-{replicate}-{}.bundle", treatment.as_str()));
            if let Some(template) = template {
                std::fs::copy(template, &target)?;
            }
            let run = run_assignment(
                &executable,
                replicate,
                treatment,
                order,
                &corpus_path,
                &target,
            )?;
            persist_raw_run(&work, &run)?;
            runs.push(run);
            if target.exists() {
                std::fs::remove_file(target)?;
            }
        }
    }
    let mut deviations = Vec::new();
    if runs.iter().any(|run| run.failure.is_some()) {
        deviations.push("one or more assigned child processes failed".into());
    }
    let contrasts = analyze(&runs, &mut deviations);
    let confirmed =
        deviations.is_empty() && contrasts.len() == 3 && contrasts.iter().all(|c| c.passed);
    let revisions = revision_ids(&std::fs::read(&full)?)?;
    let mut report = Report {
        schema: "reflex-confirmatory-report-v1",
        specification_sha256,
        specification,
        environment,
        audit_corpus_sha256,
        audit_corpora,
        training_bundle_sha256: hash_file(&full)?,
        full_knowledge_revision: hex(&revisions.0),
        full_model_revision: hex(&revisions.1),
        no_model_bundle_sha256: hash_file(&no_model)?,
        no_derived_bundle_sha256: hash_file(&no_derived)?,
        protocol_deviations: deviations,
        runs,
        contrasts,
        confirmed,
        reproduction_commands: vec![
            "cargo test --workspace --release",
            "cargo run --release -p xtask -- causal-confirm",
        ],
        content_sha256: String::new(),
    };
    report.content_sha256 = hash_json(&report)?;
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut report_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)?;
    serde_json::to_writer_pretty(&mut report_file, &report)?;
    report_file.sync_all()?;
    std::fs::File::open(
        output
            .parent()
            .ok_or("causal confirmation output has no resolved parent")?,
    )?
    .sync_all()?;
    std::fs::remove_dir_all(work)?;
    println!(
        "wrote {} ({})",
        output.display(),
        if confirmed {
            "confirmed"
        } else {
            "null result"
        }
    );
    Ok(())
}

fn resolve_report_output(output: &Path, work: &Path) -> Result<PathBuf, AnyError> {
    let output = if output.is_absolute() {
        output.to_path_buf()
    } else {
        std::env::current_dir()?.join(output)
    };
    let file_name = output
        .file_name()
        .ok_or("causal confirmation output must name a report file")?;
    let parent = output
        .parent()
        .ok_or("causal confirmation output must have a parent directory")?;
    std::fs::create_dir_all(parent)?;
    let resolved_parent = std::fs::canonicalize(parent)?;
    let resolved_work = std::fs::canonicalize(work)?;
    if resolved_parent.starts_with(&resolved_work) {
        return Err("causal confirmation output must live outside its fixed work directory".into());
    }
    Ok(resolved_parent.join(file_name))
}

fn persist_audit_corpora(work: &Path, audit_corpora: &[Vec<CorpusRecord>]) -> Result<(), AnyError> {
    std::fs::write(
        work.join("audit-corpora.json"),
        serde_json::to_vec_pretty(&audit_corpora)?,
    )?;
    for (replicate, corpus) in audit_corpora.iter().enumerate() {
        std::fs::write(
            work.join(format!("audit-{replicate}.json")),
            serde_json::to_vec(corpus)?,
        )?;
    }
    Ok(())
}

fn persist_raw_run(work: &Path, run: &RecordedRun) -> Result<(), AnyError> {
    std::fs::write(
        work.join(format!(
            "raw-{}-{}.json",
            run.replicate,
            run.treatment.as_str()
        )),
        serde_json::to_vec_pretty(run)?,
    )?;
    Ok(())
}

fn confirm_configuration(
    arguments: &[String],
) -> Result<(PathBuf, ExperimentSpec, String, HostEnvironment), AnyError> {
    require_release("causal")?;
    let output = match arguments {
        [] => PathBuf::from("docs/experiments/u8-causal-confirmation-v6.json"),
        [flag, path] if flag == "--output" => PathBuf::from(path),
        _ => return Err("causal-confirm accepts only an optional --output PATH".into()),
    };
    require_absent(&output, "confirmatory report")?;
    let specification = specification();
    let specification_sha256 = hash_json(&specification)?;
    if specification_sha256 != EXPECTED_SPEC_SHA256 {
        return Err(format!(
            "frozen specification hash mismatch: expected {EXPECTED_SPEC_SHA256}, got {specification_sha256}"
        )
        .into());
    }
    let environment = environment()?;
    require_clean(&environment, "confirmatory")?;
    validate_historical_bootstrap_calibration()?;
    Ok((output, specification, specification_sha256, environment))
}

pub(super) fn run_child(arguments: &[String]) -> Result<(), AnyError> {
    require_installed_domain_identity()?;
    let assignment = parse_child_assignment(arguments)?;
    let replicate = assignment.replicate;
    let treatment = assignment.treatment;
    let corpus: Vec<CorpusRecord> = serde_json::from_slice(&std::fs::read(assignment.corpus)?)?;
    let target = assignment.target;
    let seeds = corpus.iter().map(expression).collect::<Vec<_>>();
    let audit_origins = seeds
        .iter()
        .map(artifact_key)
        .collect::<Result<BTreeSet<_>, _>>()?;
    let plan = if treatment == Treatment::Bootstrap {
        BundlePlan::Fresh {
            target: target.clone(),
        }
    } else {
        BundlePlan::Resume {
            source: target.clone(),
            target: target.clone(),
        }
    };
    let wall = Instant::now();
    let cpu = ProcessTime::try_now()?;
    let mut trace = AuditTrace::new(&audit_origins);
    let outcome = improve(
        BitVecDomain::unary_u8(),
        request(seeds.clone(), VERIFICATION_REQUESTS, plan)?,
        |update| {
            trace.observe(&update);
            ControlFlow::Continue(())
        },
    )?;
    let wall_ns = duration_ns(wall.elapsed());
    let process_cpu_ns = duration_ns(cpu.try_elapsed()?);
    let usage = outcome.usage();
    let mut evaluation_failures = Vec::new();
    if let Err(error) = validate_evaluation(&outcome) {
        evaluation_failures.push(error.to_string());
    }
    let (pareto_artifacts, aggregates, audit_artifact_sha256, pareto_results) =
        audit_outcome(&outcome, &audit_origins);
    if pareto_artifacts != CASES_PER_REPLICATE {
        evaluation_failures.push(format!(
            "expected {CASES_PER_REPLICATE} audit Pareto Artifacts, got {pareto_artifacts}"
        ));
    }
    if let Err(error) = trace.finish(aggregates, pareto_artifacts) {
        evaluation_failures.push(error);
    }
    let bundle_sha256 = hash_file(&target)?;
    let revisions = revision_ids(&std::fs::read(&target)?)?;
    let recovery = recover_child(
        seeds,
        &target,
        &audit_origins,
        pareto_artifacts,
        aggregates,
        &audit_artifact_sha256,
    );
    let (recovery_valid, recovery_failure, recovery_artifact_sha256, recovery_requests) =
        match recovery {
            Ok((artifact, requests)) => (true, None, Some(artifact), Some(requests)),
            Err(error) => (false, Some(error.to_string()), None, None),
        };
    let evaluation_valid = evaluation_failures.is_empty();
    let evaluation_failure = (!evaluation_valid).then(|| evaluation_failures.join("; "));
    let result = ChildResult {
        replicate,
        treatment,
        completion: completion_name(outcome.completion()).into(),
        wall_ns,
        process_cpu_ns,
        reported_elapsed_ns: duration_ns(usage.elapsed_time),
        reported_cpu_ns: duration_ns(usage.cpu_time),
        resident_bytes: usage.resident_bytes,
        durable_bytes: usage.durable_bytes,
        verification_requests: usage.verification_requests,
        pareto_artifacts,
        pareto_results,
        aggregates,
        anytime_curve: trace.points,
        audit_artifact_sha256,
        bundle_sha256,
        knowledge_revision: hex(&revisions.0),
        model_revision: hex(&revisions.1),
        evaluation_valid,
        evaluation_failure,
        recovery_valid,
        recovery_failure,
        recovery_artifact_sha256,
        recovery_verification_requests: recovery_requests,
    };
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn recover_child(
    seeds: Vec<Expression>,
    target: &Path,
    audit_origins: &BTreeSet<[u8; 32]>,
    expected_count: usize,
    expected_aggregates: Aggregates,
    expected_artifact_sha256: &str,
) -> Result<(String, u64), AnyError> {
    let recovered = improve(
        BitVecDomain::unary_u8(),
        recovery_request(
            seeds,
            1_000_000,
            BundlePlan::Resume {
                source: target.to_path_buf(),
                target: target.to_path_buf(),
            },
        )?,
        |_| ControlFlow::Continue(()),
    )?;
    let (count, aggregates, artifact_sha256, _) = audit_outcome(&recovered, audit_origins);
    if recovered.completion() != Completion::SuccessConditionsSatisfied
        || count != expected_count
        || !aggregates.same_deterministic_measurements(expected_aggregates)
        || artifact_sha256 != expected_artifact_sha256
    {
        return Err("completed recovery changed the accepted audit Pareto result".into());
    }
    Ok((artifact_sha256, recovered.usage().verification_requests))
}

fn validate_evaluation(outcome: &reflex::SessionOutcome<BitVecDomain>) -> Result<(), AnyError> {
    let usage = outcome.usage();
    if outcome.completion() == Completion::ResourceEnvelopeExhausted
        && usage.verification_requests == VERIFICATION_REQUESTS
        && usage.worker_threads == 1
    {
        return Ok(());
    }
    Err(format!(
        "evaluation did not consume the exact verifier/thread envelope: completion={}, requests={}, workers={}",
        completion_name(outcome.completion()),
        usage.verification_requests,
        usage.worker_threads
    )
    .into())
}

fn parse_child_assignment(arguments: &[String]) -> Result<ChildAssignment, AnyError> {
    let value = |flag: &str| -> Result<&str, AnyError> {
        arguments
            .windows(2)
            .find(|pair| pair[0] == flag)
            .map(|pair| pair[1].as_str())
            .ok_or_else(|| format!("missing {flag}").into())
    };
    let treatment = match value("--treatment")? {
        "full" => Treatment::Full,
        "no-model" => Treatment::NoModel,
        "no-derived" => Treatment::NoDerived,
        "bootstrap" => Treatment::Bootstrap,
        _ => return Err("unknown treatment".into()),
    };
    Ok(ChildAssignment {
        replicate: value("--replicate")?.parse()?,
        treatment,
        corpus: value("--corpus")?.into(),
        target: value("--target")?.into(),
    })
}

fn specification() -> ExperimentSpec {
    ExperimentSpec {
        version: SPEC_VERSION,
        hypothesis: "under equal evaluation envelopes, shared consolidated Reflex improves unseen unary u8 semantic Campaigns more than isolated Bootstrap",
        domain_identity: DOMAIN_IDENTITY,
        development_corpus: "all x xor c semantics, the first 8190 enumerated add/rotate pilot groups, and every consumed v1, v2, v3, v4, and v5 audit semantic group",
        consumed_audit_corpus: "v1 audit sha256 7c8d87d90691502a55396e3cb70561bbd63cc7179d213879f93d6c5e9bb1a81c; v2 audit sha256 ad7b01320496b67cecabd97aea949c7e0a198945eee45faad07d811e31b2e081; v3 audit sha256 585c9e7ec1f2c64fb34fb2d9a300e72d5d29c2ea3ff34fca250807c4d990aaaf; v4 audit sha256 0eae44e4ca7a2ba050f02ab87c0e2de27fecde457d8744636e29afdd6d404787; v5 audit sha256 5e1e3ad15fb06719b79855fbd18ce057c8f536f9d5e53d8575e13c9c4671a2ce",
        historical_bootstrap_calibration: "historical v3 Semantic Identity calibration only: reflex-bootstrap-baseline-v7 report file sha256 b8a5e2c8ef87a88c4576d934e8d9e234961942cc599973d789a5fe4e29ad6daa; protocol sha256 14b108b35f336c134fa94139de5430adba0904f8471ffa44de4fb7c55fc0b880; content sha256 18af93061337a068e4c883dd149632d87b8d197d2ff33ad87e1c074aedf1f117; not an exact v4 comparator; the fresh in-protocol Bootstrap arm is authoritative",
        training_generator: "96 refuted Seeds xor(input,c) for c=1..96 followed by 96 useful Seeds xor(xor(xor(input,c),0),0) for c=97..192",
        training_verification_requests: 100_000,
        pilot_exclusion_cases: PILOT_EXCLUSION_CASES,
        pilot_generator: "lexicographic c1=1..255,r=1..7,c2=1..255; base=rotl(add(rotl(add(input,c1),r),c2),r); accepted ordinal category cycles [xor(base,31),xor(base,0),xor(xor(base,0),0)]; stop after 8190 observed cases",
        audit_generator: "sha256(seed || little-endian counter) rejection sampling into globally unique add/rotate truth-table groups; ordinal-balanced surface categories",
        audit_surface_categories: "bytes map to c1=(b0 mod 255)+1,r1=(b1 mod 7)+1,c2=(b2 mod 255)+1,r2=(b3 mod 7)+1; accepted ordinal category cycles [xor(base,31),xor(base,0),xor(xor(base,0),0)] where base=rotl(add(rotl(add(input,c1),r1),c2),r2)",
        semantic_group_digest: "sha256('reflex-u8-semantic-function-v1\\0' || outputs for inputs 0..255 in ascending order)",
        semantic_split: "reject every xor(input,c) truth-table group, every observed pilot truth-table group, every consumed v1, v2, v3, v4, and v5 audit truth-table group, and every v6 audit group accepted by an earlier replicate",
        audit_exposure: "build and validate all training and ablation bundles before generating any audit corpus; persist the complete corpus artifact and every per-replicate corpus before the first assignment; execute immediately after generation and publish every record",
        audit_seeds: AUDIT_SEEDS.to_vec(),
        independent_replicates: REPLICATES,
        cases_per_replicate: CASES_PER_REPLICATE,
        treatments: Treatment::ALL
            .iter()
            .map(|treatment| treatment.as_str())
            .collect(),
        ablations: "full=trained bundle; no-model=identical full bundle with the exact empty Bootstrap Model Ecology; no-derived=identical full bundle with executable and promotable Derived Operator Knowledge removed and canonical Knowledge Revision ID recomputed; bootstrap=fresh isolated production Runtime and the authoritative comparator",
        treatment_order: "replicate-index cyclic rotation of [full,no-model,no-derived,bootstrap]",
        worker_threads: 1,
        resident_bytes: RESIDENT_BYTES,
        durable_bytes: DURABLE_BYTES,
        elapsed_seconds: TIME_SECONDS,
        cpu_seconds: TIME_SECONDS,
        verification_requests: VERIFICATION_REQUESTS,
        child_timeout_seconds: CHILD_TIMEOUT_SECONDS,
        primary_outcome: "paired comparison of aggregate node count across exactly 8190 audit-origin Pareto Artifacts; lower is better",
        protected_outcomes: vec![
            "aggregate depth",
            "aggregate encoded bytes",
            "aggregate evaluator operations",
        ],
        diagnostic_outcomes: vec![
            "aggregate peak live temporaries (reported, not a confirmation gate)",
            "aggregate environment-pinned evaluation nanoseconds (reported, not a confirmation gate)",
        ],
        practical_thresholds: BTreeMap::from([
            ("bootstrap-minus-full", BOOTSTRAP_THRESHOLD),
            ("no-model-minus-full", MODEL_THRESHOLD),
            ("no-derived-minus-full", DERIVED_THRESHOLD),
        ]),
        uncertainty: "10000 deterministic paired bootstrap resamples; Bonferroni-adjusted one-sided percentile lower bounds at family-wise 99% confidence",
        bootstrap_algorithm: "for each comparison and resample/draw, index=u16_le(sha256('reflex-paired-bootstrap-v1\\0'||treatment||u64_le(resample)||u64_le(draw))[0..2]) mod 10; integer mean; sorted lower bound index=floor(10000/(100*3))",
        multiplicity: "three contrasts at alpha=0.01/3; all must exceed their threshold and every paired effect must be positive",
        stopping: "execute every assigned process exactly once; no outcome-dependent stopping",
        exclusions: "none; crashes, timeouts, malformed results, and deviations are retained and prevent confirmation",
        decision: "confirm only if full beats Bootstrap and both one-factor ablations above practical thresholds, all protected aggregates do not regress pairwise, every assignment recovers identically, and no Protocol Deviation occurs",
        mode: "online adaptation during each evaluation assignment; each treatment starts from its fixed pre-audit bundle and the post-audit bundle is discarded after recovery validation",
        pareto_scope: "analyze exactly final Pareto Artifacts whose origin key is one of the replicate's audit Seeds; training Artifacts are excluded from outcomes",
        anytime_trace: "record audit-origin Pareto aggregate totals at observer sequence 1, every 256th sequence, and the final sequence",
        recovery: "resume each completed output with the same Seeds, a 1000000-request replay envelope, and a trivially satisfied NodeCount<=u64::MAX Success Condition; require SuccessConditionsSatisfied before search plus identical audit count, aggregates, and Artifact-key digest",
    }
}

fn validate_historical_bootstrap_calibration() -> Result<(), AnyError> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("xtask manifest must have a workspace parent")?;
    let report_path = workspace.join(BOOTSTRAP_COMPARATOR_REPORT);
    if hash_file(&report_path)? != BOOTSTRAP_COMPARATOR_FILE_SHA256 {
        return Err(
            "historical Bootstrap calibration does not match its registered file hash".into(),
        );
    }
    let report: BootstrapComparatorReport = serde_json::from_slice(&std::fs::read(report_path)?)?;
    let valid = report.schema == "reflex-performance-report-v1"
        && report.protocol_sha256 == BOOTSTRAP_COMPARATOR_PROTOCOL_SHA256
        && report.content_sha256 == BOOTSTRAP_COMPARATOR_CONTENT_SHA256
        && !report.environment.git_dirty
        && report.protocol_deviations.is_empty()
        && report.semantic_outcome_sha256.as_deref() == Some(BOOTSTRAP_COMPARATOR_SEMANTIC_SHA256)
        && report.runs.len() == 96
        && report.runs.iter().all(|run| {
            run.failure.is_none()
                && run.result.as_ref().is_some_and(|result| {
                    result.recovery_valid
                        && result.semantic_outcome_sha256 == BOOTSTRAP_COMPARATOR_SEMANTIC_SHA256
                        && result.recovery_semantic_outcome_sha256.as_deref()
                            == Some(BOOTSTRAP_COMPARATOR_SEMANTIC_SHA256)
                })
        });
    if !valid {
        return Err(
            "historical Bootstrap calibration is incomplete or contains a Protocol Deviation"
                .into(),
        );
    }
    Ok(())
}

fn require_installed_domain_identity() -> Result<(), AnyError> {
    let installed = BitVecDomain::unary_u8().semantic_identity();
    if installed.as_str() != DOMAIN_IDENTITY {
        return Err(format!(
            "causal protocol requires Domain Semantic Identity {DOMAIN_IDENTITY}, installed {}",
            installed.as_str(),
        )
        .into());
    }
    Ok(())
}

fn build_training_bundles(full: &Path, bootstrap: &Path) -> Result<(), AnyError> {
    require_installed_domain_identity()?;
    improve(
        BitVecDomain::unary_u8(),
        request(
            training_seeds(),
            100_000,
            BundlePlan::Fresh {
                target: full.into(),
            },
        )?,
        |_| ControlFlow::Continue(()),
    )?;
    improve(
        BitVecDomain::unary_u8(),
        request(
            vec![Expression::input()],
            100_000,
            BundlePlan::Fresh {
                target: bootstrap.into(),
            },
        )?,
        |_| ControlFlow::Break(()),
    )?;
    Ok(())
}

fn training_seeds() -> Vec<Expression> {
    let refuted = (1..=96)
        .map(|constant| Expression::xor(Expression::input(), Expression::constant(constant)));
    let useful = (97..=192).map(|constant| {
        let base = Expression::xor(Expression::input(), Expression::constant(constant));
        Expression::xor(
            Expression::xor(base, Expression::constant(0)),
            Expression::constant(0),
        )
    });
    refuted.chain(useful).collect()
}

struct AuditExposureAuthority;

struct PreparedTreatments {
    no_model: PathBuf,
    no_derived: PathBuf,
    audit_exposure: AuditExposureAuthority,
}

fn prepare_treatment_bundles(
    work: &Path,
    full: &Path,
    bootstrap_revision: &Path,
) -> Result<PreparedTreatments, AnyError> {
    validate_treatment_bundle(full)?;
    let no_model = work.join("no-model.bundle");
    let no_derived = work.join("no-derived.bundle");
    ablate_causal_bundle(full, &no_model, Some(bootstrap_revision), false)?;
    ablate_causal_bundle(full, &no_derived, None, true)?;
    validate_treatment_bundle(&no_model)?;
    validate_treatment_bundle(&no_derived)?;
    Ok(PreparedTreatments {
        no_model,
        no_derived,
        audit_exposure: AuditExposureAuthority,
    })
}

fn generate_audit_corpora(
    _authority: &AuditExposureAuthority,
) -> Result<Vec<Vec<CorpusRecord>>, AnyError> {
    require_installed_domain_identity()?;
    let mut excluded = development_semantics();
    excluded.extend(pilot_semantics());
    excluded.extend(consumed_v1_semantics()?);
    excluded.extend(consumed_v2_semantics()?);
    excluded.extend(consumed_v3_semantics()?);
    excluded.extend(consumed_v4_semantics()?);
    excluded.extend(consumed_v5_semantics()?);
    let mut global = excluded.clone();
    let mut corpora = Vec::with_capacity(REPLICATES);
    for seed in AUDIT_SEEDS {
        let seed = decode_hex_32(seed)?;
        let mut corpus = Vec::with_capacity(CASES_PER_REPLICATE);
        let mut counter = 0_u64;
        while corpus.len() < CASES_PER_REPLICATE {
            let mut digest = Sha256::new();
            digest.update(seed);
            digest.update(counter.to_le_bytes());
            counter = counter.saturating_add(1);
            let bytes: [u8; 32] = digest.finalize().into();
            let mut record = CorpusRecord {
                first_constant: bytes[0] % 255 + 1,
                first_rotation: bytes[1] % 7 + 1,
                second_constant: bytes[2] % 255 + 1,
                second_rotation: bytes[3] % 7 + 1,
                category: u8::try_from(corpus.len() % 3).unwrap(),
                semantic_sha256: String::new(),
            };
            let semantic = truth_digest(&expression(&record));
            if !global.insert(semantic) {
                continue;
            }
            record.semantic_sha256 = hex(&semantic);
            corpus.push(record);
        }
        corpora.push(corpus);
    }
    if global.len() != excluded.len() + REPLICATES * CASES_PER_REPLICATE {
        return Err("audit Semantic Split contains overlap".into());
    }
    Ok(corpora)
}

fn consumed_v1_semantics() -> Result<BTreeSet<[u8; 32]>, AnyError> {
    consumed_semantics(CONSUMED_V1_REPORT, CONSUMED_V1_AUDIT_SHA256)
}

fn consumed_v2_semantics() -> Result<BTreeSet<[u8; 32]>, AnyError> {
    consumed_semantics(CONSUMED_V2_REPORT, CONSUMED_V2_AUDIT_SHA256)
}

fn consumed_v3_semantics() -> Result<BTreeSet<[u8; 32]>, AnyError> {
    consumed_semantics(CONSUMED_V3_REPORT, CONSUMED_V3_AUDIT_SHA256)
}

fn consumed_v4_semantics() -> Result<BTreeSet<[u8; 32]>, AnyError> {
    consumed_semantics(CONSUMED_V4_REPORT, CONSUMED_V4_AUDIT_SHA256)
}

fn consumed_v5_semantics() -> Result<BTreeSet<[u8; 32]>, AnyError> {
    consumed_semantics(CONSUMED_V5_REPORT, CONSUMED_V5_AUDIT_SHA256)
}

fn consumed_semantics(
    report_path: &str,
    expected_audit_sha256: &str,
) -> Result<BTreeSet<[u8; 32]>, AnyError> {
    let report = load_consumed_report(report_path, expected_audit_sha256)?;
    let mut semantics = BTreeSet::new();
    for record in report.audit_corpora.into_iter().flatten() {
        let registered = decode_hex_32(&record.semantic_sha256)?;
        if truth_digest(&expression(&record)) != registered || !semantics.insert(registered) {
            return Err("consumed audit corpus is invalid or semantically duplicated".into());
        }
    }
    Ok(semantics)
}

fn load_consumed_report(
    report_path: &str,
    expected_audit_sha256: &str,
) -> Result<ConsumedReport, AnyError> {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .ok_or("xtask manifest must have a workspace parent")?;
    let report: ConsumedReport =
        serde_json::from_slice(&std::fs::read(workspace.join(report_path))?)?;
    if report.audit_corpus_sha256 != expected_audit_sha256
        || hash_json(&report.audit_corpora)? != expected_audit_sha256
        || report.audit_corpora.len() != REPLICATES
        || report
            .audit_corpora
            .iter()
            .any(|corpus| corpus.len() != CASES_PER_REPLICATE)
    {
        return Err("consumed audit corpus does not match its registered identity".into());
    }
    Ok(report)
}

fn development_semantics() -> BTreeSet<[u8; 32]> {
    (0..=u8::MAX)
        .map(|constant| {
            truth_digest(&Expression::xor(
                Expression::input(),
                Expression::constant(constant),
            ))
        })
        .collect()
}

fn pilot_semantics() -> BTreeSet<[u8; 32]> {
    let mut semantics = BTreeSet::new();
    let mut ordinal = 0_usize;
    'outer: for first_constant in 1..=u8::MAX {
        for rotation in 1..=7 {
            for second_constant in 1..=u8::MAX {
                let record = CorpusRecord {
                    first_constant,
                    first_rotation: rotation,
                    second_constant,
                    second_rotation: rotation,
                    category: u8::try_from(ordinal % 3).unwrap(),
                    semantic_sha256: String::new(),
                };
                semantics.insert(truth_digest(&expression(&record)));
                ordinal += 1;
                if ordinal == PILOT_EXCLUSION_CASES {
                    break 'outer;
                }
            }
        }
    }
    semantics
}

fn expression(record: &CorpusRecord) -> Expression {
    let base = Expression::rotate_left(
        Expression::wrapping_add(
            Expression::rotate_left(
                Expression::wrapping_add(
                    Expression::input(),
                    Expression::constant(record.first_constant),
                ),
                record.first_rotation,
            ),
            Expression::constant(record.second_constant),
        ),
        record.second_rotation,
    );
    match record.category {
        0 => Expression::xor(base, Expression::constant(31)),
        1 => Expression::xor(base, Expression::constant(0)),
        2 => Expression::xor(
            Expression::xor(base, Expression::constant(0)),
            Expression::constant(0),
        ),
        _ => unreachable!("canonical corpus category"),
    }
}

fn truth_digest(expression: &Expression) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"reflex-u8-semantic-function-v1\0");
    for input in 0..=u8::MAX {
        digest.update([expression.evaluate(input)]);
    }
    digest.finalize().into()
}

fn request(
    seeds: Vec<Expression>,
    verification_requests: u64,
    bundle: BundlePlan,
) -> Result<ImprovementRequest<BitVecDomain>, AnyError> {
    request_with_success(seeds, verification_requests, bundle, false)
}

fn recovery_request(
    seeds: Vec<Expression>,
    verification_requests: u64,
    bundle: BundlePlan,
) -> Result<ImprovementRequest<BitVecDomain>, AnyError> {
    request_with_success(seeds, verification_requests, bundle, true)
}

fn request_with_success(
    seeds: Vec<Expression>,
    verification_requests: u64,
    bundle: BundlePlan,
    stop_after_replay: bool,
) -> Result<ImprovementRequest<BitVecDomain>, AnyError> {
    let metrics = NonEmpty::try_from_iter([
        Metric::NodeCount,
        Metric::Depth,
        Metric::PeakLiveTemporaries,
        Metric::EncodedBytes,
        Metric::EvaluatorOperations,
    ])
    .map_err(|_| "metric literal must be non-empty")?;
    let objectives = NonEmpty::try_from_iter(
        metrics
            .as_slice()
            .iter()
            .copied()
            .map(|metric| Objective::new(metric, Direction::Minimize)),
    )
    .map_err(|_| "objective iterator must be non-empty")?;
    let preference = Preference::tiered(NonEmpty::one(metrics), [])?;
    let success = stop_after_replay.then(|| {
        SuccessCondition::all(NonEmpty::one(MeasurementConstraint::new(
            Metric::NodeCount,
            ThresholdRelation::AtMost,
            u64::MAX,
        )))
    });
    Ok(ImprovementRequest::new(
        GoalSet::one(OptimizationGoal::new([], objectives, preference, success)?),
        SeedScope::new(
            NonEmpty::try_from_iter(seeds).map_err(|_| "causal run seeds must be non-empty")?,
        ),
        ResourceEnvelope::new(
            NonZeroUsize::new(1).unwrap(),
            NonZeroU64::new(RESIDENT_BYTES).unwrap(),
            NonZeroU64::new(DURABLE_BYTES).unwrap(),
            NonZeroDuration::new(Duration::from_secs(TIME_SECONDS)).unwrap(),
            NonZeroDuration::new(Duration::from_secs(TIME_SECONDS)).unwrap(),
            NonZeroU64::new(verification_requests).unwrap(),
        ),
        bundle,
    )?)
}

fn artifact_key(expression: &Expression) -> Result<[u8; 32], AnyError> {
    let domain = BitVecDomain::unary_u8();
    let identity = domain.semantic_identity();
    let mut canonical = Vec::new();
    domain
        .structure()
        .encode_canonical(expression, &mut canonical, &mut ())?;
    let mut digest = Sha256::new();
    digest.update(b"reflex-artifact-v1\0");
    digest.update((identity.as_str().len() as u64).to_le_bytes());
    digest.update(identity.as_str().as_bytes());
    digest.update(canonical);
    Ok(digest.finalize().into())
}

fn audit_outcome(
    outcome: &reflex::SessionOutcome<BitVecDomain>,
    origins: &BTreeSet<[u8; 32]>,
) -> (usize, Aggregates, String, Vec<ParetoArtifactResult>) {
    let mut keys = Vec::new();
    let mut pareto_results = Vec::new();
    let mut aggregates = Aggregates {
        node_count: 0,
        depth: 0,
        peak_live_temporaries: 0,
        encoded_bytes: 0,
        evaluator_operations: 0,
        evaluation_nanoseconds: 0,
    };
    let artifacts = outcome
        .pareto()
        .artifacts()
        .iter()
        .filter(|artifact| origins.contains(artifact.origin_key().as_bytes()))
        .collect::<Vec<_>>();
    for artifact in &artifacts {
        keys.push(artifact.key());
        let mut artifact_measurements = Aggregates::default();
        for measurement in artifact.measurements() {
            match measurement.metric {
                Metric::NodeCount => artifact_measurements.node_count = measurement.observation,
                Metric::Depth => artifact_measurements.depth = measurement.observation,
                Metric::PeakLiveTemporaries => {
                    artifact_measurements.peak_live_temporaries = measurement.observation;
                }
                Metric::EncodedBytes => {
                    artifact_measurements.encoded_bytes = measurement.observation;
                }
                Metric::EvaluatorOperations => {
                    artifact_measurements.evaluator_operations = measurement.observation;
                }
                Metric::EvaluationNanoseconds => {
                    artifact_measurements.evaluation_nanoseconds = measurement.observation;
                }
            }
        }
        add_aggregates(&mut aggregates, artifact_measurements);
        pareto_results.push(ParetoArtifactResult {
            artifact_key: hex(artifact.key().as_bytes()),
            origin_key: hex(artifact.origin_key().as_bytes()),
            measurements: artifact_measurements,
        });
    }
    keys.sort_unstable();
    pareto_results.sort_unstable_by(|left, right| left.artifact_key.cmp(&right.artifact_key));
    let mut digest = Sha256::new();
    for key in keys {
        digest.update(key.as_bytes());
    }
    (
        artifacts.len(),
        aggregates,
        hex(&digest.finalize()),
        pareto_results,
    )
}

struct AuditTrace<'a> {
    origins: &'a BTreeSet<[u8; 32]>,
    frontier: BTreeMap<[u8; 32], ([u8; 32], Aggregates)>,
    aggregates: Aggregates,
    pareto_artifacts: usize,
    last_sequence: u64,
    points: Vec<AnytimePoint>,
}

impl<'a> AuditTrace<'a> {
    fn new(origins: &'a BTreeSet<[u8; 32]>) -> Self {
        Self {
            origins,
            frontier: BTreeMap::new(),
            aggregates: Aggregates::default(),
            pareto_artifacts: 0,
            last_sequence: 0,
            points: Vec::new(),
        }
    }

    fn observe(&mut self, update: &reflex::ParetoUpdate<'_, BitVecDomain>) {
        for key in update.removed() {
            if let Some((origin, measurements)) = self.frontier.remove(key.as_bytes())
                && self.origins.contains(&origin)
            {
                self.pareto_artifacts -= 1;
                subtract_aggregates(&mut self.aggregates, measurements);
            }
        }
        for artifact in update.added() {
            let key = *artifact.key().as_bytes();
            if let Some((origin, measurements)) = self.frontier.remove(&key)
                && self.origins.contains(&origin)
            {
                self.pareto_artifacts -= 1;
                subtract_aggregates(&mut self.aggregates, measurements);
            }
            let origin = *artifact.origin_key().as_bytes();
            let measurements = artifact_aggregates(artifact);
            self.frontier.insert(key, (origin, measurements));
            if self.origins.contains(&origin) {
                self.pareto_artifacts += 1;
                add_aggregates(&mut self.aggregates, measurements);
            }
        }
        self.last_sequence = update.sequence();
        if update.sequence() == 1 || update.sequence().is_multiple_of(256) {
            self.push_point();
        }
    }

    fn finish(&mut self, aggregates: Aggregates, pareto_artifacts: usize) -> Result<(), String> {
        if self.aggregates != aggregates || self.pareto_artifacts != pareto_artifacts {
            return Err("observer trace did not match the completed outcome".into());
        }
        if self
            .points
            .last()
            .is_none_or(|point| point.observer_sequence != self.last_sequence)
        {
            self.push_point();
        }
        Ok(())
    }

    fn push_point(&mut self) {
        self.points.push(AnytimePoint {
            observer_sequence: self.last_sequence,
            pareto_artifacts: self.pareto_artifacts,
            aggregates: self.aggregates,
        });
    }
}

fn artifact_aggregates(artifact: &reflex::VerifiedArtifact<BitVecDomain>) -> Aggregates {
    let mut aggregates = Aggregates::default();
    for measurement in artifact.measurements() {
        match measurement.metric {
            Metric::NodeCount => aggregates.node_count = measurement.observation,
            Metric::Depth => aggregates.depth = measurement.observation,
            Metric::PeakLiveTemporaries => {
                aggregates.peak_live_temporaries = measurement.observation;
            }
            Metric::EncodedBytes => aggregates.encoded_bytes = measurement.observation,
            Metric::EvaluatorOperations => {
                aggregates.evaluator_operations = measurement.observation;
            }
            Metric::EvaluationNanoseconds => {
                aggregates.evaluation_nanoseconds = measurement.observation;
            }
        }
    }
    aggregates
}

fn add_aggregates(total: &mut Aggregates, value: Aggregates) {
    total.node_count += value.node_count;
    total.depth += value.depth;
    total.peak_live_temporaries += value.peak_live_temporaries;
    total.encoded_bytes += value.encoded_bytes;
    total.evaluator_operations += value.evaluator_operations;
    total.evaluation_nanoseconds += value.evaluation_nanoseconds;
}

fn subtract_aggregates(total: &mut Aggregates, value: Aggregates) {
    total.node_count -= value.node_count;
    total.depth -= value.depth;
    total.peak_live_temporaries -= value.peak_live_temporaries;
    total.encoded_bytes -= value.encoded_bytes;
    total.evaluator_operations -= value.evaluator_operations;
    total.evaluation_nanoseconds -= value.evaluation_nanoseconds;
}

fn run_assignment(
    executable: &Path,
    replicate: usize,
    treatment: Treatment,
    order: usize,
    corpus: &Path,
    target: &Path,
) -> Result<RecordedRun, AnyError> {
    run_assignment_capture(
        executable, replicate, treatment, order, corpus, target, None,
    )
    .map(|(run, _)| run)
}

fn run_assignment_capture(
    executable: &Path,
    replicate: usize,
    treatment: Treatment,
    order: usize,
    corpus: &Path,
    target: &Path,
    phase_report_prefix: Option<&Path>,
) -> Result<(RecordedRun, Vec<String>), AnyError> {
    let arguments = [
        OsString::from("causal-child"),
        OsString::from("--replicate"),
        OsString::from(replicate.to_string()),
        OsString::from("--treatment"),
        OsString::from(treatment.as_str()),
        OsString::from("--corpus"),
        corpus.as_os_str().to_owned(),
        OsString::from("--target"),
        target.as_os_str().to_owned(),
    ];
    let child_environment = phase_report_prefix.map_or_else(Vec::new, |prefix| {
        vec![(
            OsString::from("REFLEX_INTERNAL_PHASE_REPORT_PREFIX"),
            prefix.as_os_str().to_owned(),
        )]
    });
    let capture = capture_large_campaign_child(
        executable,
        &arguments,
        target,
        Some(Duration::from_secs(CHILD_TIMEOUT_SECONDS)),
        None,
        &child_environment,
    )?;
    let (result, failure) = if capture.timed_out {
        (
            None,
            Some(format!(
                "child exceeded the {CHILD_TIMEOUT_SECONDS}-second process timeout"
            )),
        )
    } else if capture.output_limit_exceeded {
        (
            None,
            Some("child exceeded the bounded diagnostic-output allowance".into()),
        )
    } else if capture.status.success() {
        match serde_json::from_str::<ChildResult>(capture.stdout.trim()) {
            Ok(result) if result.replicate == replicate && result.treatment == treatment => {
                let failure = if !result.evaluation_valid {
                    Some(format!(
                        "invalid evaluation: {}",
                        result
                            .evaluation_failure
                            .as_deref()
                            .unwrap_or("unspecified")
                    ))
                } else if !result.recovery_valid {
                    Some(format!(
                        "recovery failed: {}",
                        result.recovery_failure.as_deref().unwrap_or("unspecified")
                    ))
                } else {
                    None
                };
                (Some(result), failure)
            }
            Ok(_) => (None, Some("child assignment identity mismatch".into())),
            Err(error) => (None, Some(format!("malformed child output: {error}"))),
        }
    } else {
        (None, Some(format!("child failed: {}", capture.stderr)))
    };
    let phase_reports = phase_report_prefix.map_or_else(Vec::new, |prefix| {
        (0..2_u32)
            .filter_map(|ordinal| {
                let mut path = prefix.to_path_buf();
                path.set_extension(format!("{ordinal}.phase"));
                let report = std::fs::read_to_string(&path).ok()?;
                let _ = std::fs::remove_file(path);
                Some(report)
            })
            .collect()
    });
    let failure = failure.or_else(|| {
        (phase_report_prefix.is_some() && phase_reports.len() != 2)
            .then(|| "instrumented child did not emit both aggregate phase reports".into())
    });
    Ok((
        RecordedRun {
            replicate,
            treatment,
            order,
            exit_code: capture.status.code(),
            stdout: capture.stdout,
            stderr: capture.stderr,
            result,
            failure,
        },
        phase_reports,
    ))
}

fn analyze(runs: &[RecordedRun], deviations: &mut Vec<String>) -> Vec<Contrast> {
    let mut by_assignment = BTreeMap::new();
    for run in runs {
        if let Some(result) = &run.result
            && result.evaluation_valid
            && result.recovery_valid
        {
            by_assignment.insert((run.replicate, run.treatment), result);
        }
    }
    let mut contrasts = Vec::new();
    for (comparison, threshold) in [
        (Treatment::Bootstrap, BOOTSTRAP_THRESHOLD),
        (Treatment::NoModel, MODEL_THRESHOLD),
        (Treatment::NoDerived, DERIVED_THRESHOLD),
    ] {
        let pairs = (0..REPLICATES)
            .map(|replicate| {
                Some((
                    *by_assignment.get(&(replicate, Treatment::Full))?,
                    *by_assignment.get(&(replicate, comparison))?,
                ))
            })
            .collect::<Option<Vec<_>>>();
        let Some(pairs) = pairs else {
            deviations.push(format!(
                "incomplete paired data for {}",
                comparison.as_str()
            ));
            continue;
        };
        let effects = pairs
            .iter()
            .map(|(full, other)| {
                i64::try_from(other.aggregates.node_count).unwrap_or(i64::MAX)
                    - i64::try_from(full.aggregates.node_count).unwrap_or(i64::MAX)
            })
            .collect::<Vec<_>>();
        let protected = pairs.iter().all(|(full, other)| {
            full.aggregates.depth <= other.aggregates.depth
                && full.aggregates.encoded_bytes <= other.aggregates.encoded_bytes
                && full.aggregates.evaluator_operations <= other.aggregates.evaluator_operations
        });
        let lower = bootstrap_lower(&effects, comparison);
        let all_positive = effects.iter().all(|effect| *effect > 0);
        contrasts.push(Contrast {
            comparison,
            threshold,
            mean_effect: effects
                .iter()
                .map(|effect| f64::from(i32::try_from(*effect).expect("bounded node effect")))
                .sum::<f64>()
                / f64::from(u32::try_from(effects.len()).expect("bounded replicate count")),
            familywise_99_percent_lower: lower,
            all_positive,
            protected_nonregression: protected,
            passed: all_positive && protected && lower >= threshold,
            paired_effects: effects,
        });
    }
    contrasts
}

fn bootstrap_lower(effects: &[i64], treatment: Treatment) -> i64 {
    let mut means = Vec::with_capacity(RESAMPLES);
    for resample in 0..RESAMPLES {
        let mut total = 0_i64;
        for draw in 0..effects.len() {
            let mut digest = Sha256::new();
            digest.update(b"reflex-paired-bootstrap-v1\0");
            digest.update(treatment.as_str().as_bytes());
            digest.update((resample as u64).to_le_bytes());
            digest.update((draw as u64).to_le_bytes());
            let bytes: [u8; 32] = digest.finalize().into();
            let index = usize::from(u16::from_le_bytes([bytes[0], bytes[1]])) % effects.len();
            total += effects[index];
        }
        means.push(total / i64::try_from(effects.len()).unwrap());
    }
    means.sort_unstable();
    means[RESAMPLES / (100 * 3)]
}

fn validate_treatment_bundle(path: &Path) -> Result<(), AnyError> {
    let target = path.with_extension("validation.bundle");
    std::fs::copy(path, &target)?;
    improve(
        BitVecDomain::unary_u8(),
        request(
            vec![Expression::input()],
            1_000_000,
            BundlePlan::Resume {
                source: target.clone(),
                target: target.clone(),
            },
        )?,
        |_| ControlFlow::Break(()),
    )?;
    std::fs::remove_file(target)?;
    Ok(())
}

fn ablate_causal_bundle(
    source: &Path,
    target: &Path,
    model_template: Option<&Path>,
    derived: bool,
) -> Result<(), AnyError> {
    ablate_bundle(source, target, model_template, derived, RESIDENT_BYTES)
}

pub(super) fn ablate_bundle(
    source: &Path,
    target: &Path,
    model_template: Option<&Path>,
    derived: bool,
    maximum_logical_bytes: u64,
) -> Result<(), AnyError> {
    let mut bundle = CanonicalBundle::decode(&std::fs::read(source)?, maximum_logical_bytes)?;
    let identity = bundle.identity().to_vec();
    let artifacts = bundle.segment(SegmentKind::Artifacts).to_vec();
    let revisions = bundle.segment(SegmentKind::Revisions);
    let checkpoint = current_intelligence_checkpoint(revisions)?;
    let template_bundle = if let Some(template) = model_template {
        Some(CanonicalBundle::decode(
            &std::fs::read(template)?,
            maximum_logical_bytes,
        )?)
    } else {
        None
    };
    let template_checkpoint = template_bundle
        .as_ref()
        .map(|template| {
            current_intelligence_checkpoint(template.segment(SegmentKind::Revisions))
                .map(|checkpoint| IntelligenceTreatmentSource::new(template.identity(), checkpoint))
        })
        .transpose()?;
    let treatment = ablate_intelligence_checkpoint(
        IntelligenceTreatmentSource::new(&identity, checkpoint),
        template_checkpoint,
        derived,
    )?;
    let knowledge_id =
        current_knowledge_revision_id(&identity, &artifacts, treatment.knowledge_product)?;
    let mut payload = Vec::new();
    payload.extend_from_slice(&knowledge_id);
    payload.extend_from_slice(&treatment.model_revision);
    payload.extend_from_slice(&treatment.runtime_policy_revision);
    payload.extend_from_slice(&treatment.intelligence_revision);
    push_sized(&mut payload, &treatment.checkpoint);
    bundle.replace_segment(SegmentKind::Revisions, payload);
    rebind_completed_session_restart_root(&mut bundle)?;
    publish_treatment_bundle(target, &bundle.encode())?;
    Ok(())
}

fn publish_treatment_bundle(target: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut file = AtomicWriteFile::open(target)?;
    if let Err(error) = file.write_all(bytes) {
        let _ = file.discard();
        return Err(error);
    }
    file.commit()
}

fn rebind_completed_session_restart_root(bundle: &mut CanonicalBundle) -> Result<(), AnyError> {
    const DISPOSITION_BYTES: usize = 1;
    const RUNTIME_REVISION_BYTES: usize = 8;
    const RESTART_ROOT_BYTES: usize = 32;
    let root_start = DISPOSITION_BYTES + RUNTIME_REVISION_BYTES;
    let root_end = root_start + RESTART_ROOT_BYTES;
    let mut session = bundle.segment(SegmentKind::Session).to_vec();
    if session.first() != Some(&1) || session.get(root_start..root_end).is_none() {
        return Err("causal ablations require a completed current Session".into());
    }
    session[root_start..root_end].copy_from_slice(&bundle.restart_state_root());
    bundle.replace_segment(SegmentKind::Session, session);
    Ok(())
}

fn current_intelligence_checkpoint(revisions: &[u8]) -> Result<&[u8], AnyError> {
    const CURRENT_REVISION_IDS_BYTES: usize = 4 * 32;
    let mut input = revisions
        .get(CURRENT_REVISION_IDS_BYTES..)
        .ok_or("truncated current Revisions header")?;
    let checkpoint = take_sized(&mut input)?;
    if !checkpoint.starts_with(b"RFIC") || !input.is_empty() {
        return Err("invalid current Intelligence Revisions payload".into());
    }
    Ok(checkpoint)
}

fn current_knowledge_revision_id(
    identity: &[u8],
    artifacts: &[u8],
    knowledge_product: [u8; 32],
) -> Result<[u8; 32], AnyError> {
    let mut input = artifacts;
    let count = read_u64(&mut input)?;
    let mut records = Vec::new();
    for _ in 0..count {
        let canonical = take_sized(&mut input)?;
        let mut key = Sha256::new();
        key.update(b"reflex-artifact-v1\0");
        key.update((identity.len() as u64).to_le_bytes());
        key.update(identity);
        key.update(canonical);
        take_sized(&mut input)?;
        take_sized(&mut input)?;
        take(&mut input, 8)?;
        let origin: [u8; 32] = take(&mut input, 32)?.try_into()?;
        let parent: Option<[u8; 32]> = match take(&mut input, 1)?[0] {
            0 => None,
            1 => Some(take(&mut input, 32)?.try_into()?),
            _ => return Err("invalid parent marker".into()),
        };
        let provenance = take_sized(&mut input)?.to_vec();
        records.push((<[u8; 32]>::from(key.finalize()), origin, parent, provenance));
    }
    if !input.is_empty() {
        return Err("trailing bytes in current Artifacts payload".into());
    }
    records.sort_unstable_by_key(|record| record.0);
    let mut digest = Sha256::new();
    digest.update(b"reflex-knowledge-revision-v1\0");
    digest.update((identity.len() as u64).to_le_bytes());
    digest.update(identity);
    for (key, origin, parent, provenance) in records {
        digest.update(key);
        digest.update(origin);
        if let Some(parent) = parent {
            digest.update([1]);
            digest.update(parent);
        } else {
            digest.update([0]);
        }
        digest.update((provenance.len() as u64).to_le_bytes());
        digest.update(provenance);
    }
    digest.update(knowledge_product);
    Ok(digest.finalize().into())
}

fn revision_ids(bytes: &[u8]) -> Result<([u8; 32], [u8; 32]), AnyError> {
    let bundle = CanonicalBundle::decode(bytes, RESIDENT_BYTES)?;
    let revisions = bundle.segment(SegmentKind::Revisions);
    Ok((revisions[..32].try_into()?, revisions[32..64].try_into()?))
}

fn decode_hex_32(value: &str) -> Result<[u8; 32], AnyError> {
    if value.len() != 64 {
        return Err("expected 64 hex digits".into());
    }
    let mut output = [0_u8; 32];
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)?;
    }
    Ok(output)
}

fn push_sized(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&(value.len() as u64).to_le_bytes());
    output.extend_from_slice(value);
}

fn take_sized<'a>(input: &mut &'a [u8]) -> Result<&'a [u8], AnyError> {
    let count = usize::try_from(read_u64(input)?)?;
    take(input, count)
}

fn read_u64(input: &mut &[u8]) -> Result<u64, AnyError> {
    Ok(u64::from_le_bytes(take(input, 8)?.try_into()?))
}

fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], AnyError> {
    if input.len() < count {
        return Err("truncated canonical data".into());
    }
    let (value, remainder) = input.split_at(count);
    *input = remainder;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specification_is_content_addressed() {
        let spec = specification();
        assert_eq!(spec.version, "reflex-u8-causal-confirmation-v6");
        assert_eq!(
            spec.domain_identity,
            "reflex-bitvec/u8/unary/full-ops/masked-shifts/select-nonzero/canonical-dag/v4",
        );
        assert!(spec.ablations.contains("Model Ecology"));
        assert_eq!(
            AUDIT_SEEDS[0],
            "1f9944407c25de385e09287d6dd98f96ff160a9144d1f25ad692f07ac20e5a52",
        );
        require_installed_domain_identity().unwrap();
        assert_eq!(hash_json(&spec).unwrap(), EXPECTED_SPEC_SHA256);
    }

    #[test]
    fn successor_audit_seeds_are_fresh_and_fixed() {
        for (index, registered) in AUDIT_SEEDS.iter().enumerate() {
            let mut digest = Sha256::new();
            digest.update(b"reflex-u8-causal-confirmation-v6-seed\0");
            digest.update(index.to_string().as_bytes());
            assert_eq!(
                <[u8; 32]>::from(digest.finalize()),
                decode_hex_32(registered).unwrap()
            );
        }
    }

    #[test]
    fn confirmation_output_cannot_live_inside_the_fixed_work_directory() {
        let root =
            std::env::temp_dir().join(format!("reflex-causal-output-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        let reports = root.join("reports");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&reports).unwrap();
        assert!(resolve_report_output(&work.join("report.json"), &work).is_err());
        assert_eq!(
            resolve_report_output(&reports.join("report.json"), &work).unwrap(),
            std::fs::canonicalize(&reports).unwrap().join("report.json")
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&work, root.join("work-alias")).unwrap();
            assert!(resolve_report_output(&root.join("work-alias/report.json"), &work).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn historical_bootstrap_calibration_is_complete_and_content_addressed() {
        validate_historical_bootstrap_calibration().unwrap();
    }

    #[test]
    fn paired_bootstrap_is_deterministic_and_directional() {
        let effects = [500, 600, 700, 800, 900, 1_000, 1_100, 1_200, 1_300, 1_400];
        let first = bootstrap_lower(&effects, Treatment::Bootstrap);
        assert_eq!(first, bootstrap_lower(&effects, Treatment::Bootstrap));
        assert!((500..=1_400).contains(&first));
    }

    #[test]
    fn completed_recovery_replays_without_resuming_search() {
        let target = std::env::temp_dir().join(format!(
            "reflex-causal-recovery-development-{}.bundle",
            std::process::id()
        ));
        let seed = Expression::xor(
            Expression::xor(Expression::input(), Expression::constant(0)),
            Expression::constant(0),
        );
        let origins = BTreeSet::from([artifact_key(&seed).unwrap()]);
        let original = improve(
            BitVecDomain::unary_u8(),
            request(
                vec![seed.clone()],
                64,
                BundlePlan::Fresh {
                    target: target.clone(),
                },
            )
            .unwrap(),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
        let expected = audit_outcome(&original, &origins);
        let recovered = improve(
            BitVecDomain::unary_u8(),
            recovery_request(
                vec![seed],
                10_000,
                BundlePlan::Resume {
                    source: target.clone(),
                    target: target.clone(),
                },
            )
            .unwrap(),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
        assert_eq!(
            recovered.completion(),
            Completion::SuccessConditionsSatisfied
        );
        let actual = audit_outcome(&recovered, &origins);
        assert_eq!(actual.0, expected.0);
        assert!(actual.1.same_deterministic_measurements(expected.1));
        assert_eq!(actual.2, expected.2);
        std::fs::remove_file(target).unwrap();
    }

    #[test]
    fn consumed_v1_audit_is_complete_and_content_addressed() {
        assert_eq!(
            consumed_v1_semantics().unwrap().len(),
            REPLICATES * CASES_PER_REPLICATE
        );
    }

    #[test]
    fn consumed_v2_audit_is_complete_and_content_addressed() {
        assert_eq!(
            consumed_v2_semantics().unwrap().len(),
            REPLICATES * CASES_PER_REPLICATE
        );
    }

    #[test]
    fn consumed_v3_audit_is_complete_and_content_addressed() {
        assert_eq!(
            consumed_v3_semantics().unwrap().len(),
            REPLICATES * CASES_PER_REPLICATE
        );
    }

    #[test]
    fn consumed_v4_audit_is_complete_and_content_addressed() {
        assert_eq!(
            consumed_v4_semantics().unwrap().len(),
            REPLICATES * CASES_PER_REPLICATE
        );
    }

    #[test]
    fn consumed_v5_audit_is_complete_and_content_addressed() {
        assert_eq!(
            consumed_v5_semantics().unwrap().len(),
            REPLICATES * CASES_PER_REPLICATE
        );
    }

    #[test]
    fn treatment_ablations_pass_production_import() {
        let directory = std::env::temp_dir().join(format!(
            "reflex-causal-ablation-development-{}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let full = directory.join("full.bundle");
        let bootstrap = directory.join("bootstrap.bundle");
        let no_model = directory.join("no-model.bundle");
        let no_derived = directory.join("no-derived.bundle");
        let cross_domain_template = directory.join("cross-domain-template.bundle");
        let rejected = directory.join("rejected.bundle");
        build_training_bundles(&full, &bootstrap).unwrap();
        let invalid_work = directory.join("invalid-full");
        std::fs::create_dir(&invalid_work).unwrap();
        let invalid_full = invalid_work.join("full.bundle");
        let mut truncated = std::fs::read(&full).unwrap();
        truncated.truncate(truncated.len() / 2);
        std::fs::write(&invalid_full, truncated).unwrap();
        assert!(
            prepare_treatment_bundles(&invalid_work, &invalid_full, &bootstrap).is_err(),
            "Full must pass production import before an ablation or audit authority exists"
        );
        assert!(!invalid_work.join("no-model.bundle").exists());
        assert!(!invalid_work.join("no-derived.bundle").exists());
        let bootstrap_bundle =
            CanonicalBundle::decode(&std::fs::read(&bootstrap).unwrap(), RESIDENT_BYTES).unwrap();
        assert_invalid_model_templates(&full, &bootstrap_bundle, &cross_domain_template, &rejected);
        let bootstrap_revision = revision_ids(&std::fs::read(&bootstrap).unwrap()).unwrap().1;
        assert!(
            ablate_bundle(&full, &no_model, Some(&bootstrap), false, 1).is_err(),
            "the caller-owned logical limit must govern ablation imports"
        );
        ablate_bundle(&full, &no_model, Some(&bootstrap), false, RESIDENT_BYTES).unwrap();
        ablate_bundle(&full, &no_derived, None, true, RESIDENT_BYTES).unwrap();
        validate_treatment_bundle(&no_model).unwrap();
        validate_treatment_bundle(&no_derived).unwrap();
        let full_bundle =
            CanonicalBundle::decode(&std::fs::read(&full).unwrap(), RESIDENT_BYTES).unwrap();
        let no_model_bundle =
            CanonicalBundle::decode(&std::fs::read(&no_model).unwrap(), RESIDENT_BYTES).unwrap();
        let no_derived_bundle =
            CanonicalBundle::decode(&std::fs::read(&no_derived).unwrap(), RESIDENT_BYTES).unwrap();
        assert_treatment_isolation(
            &bootstrap_bundle,
            &full_bundle,
            &no_model_bundle,
            &no_derived_bundle,
        );
        assert_eq!(
            revision_ids(&std::fs::read(&no_model).unwrap()).unwrap().1,
            bootstrap_revision
        );
        assert!(
            inspect_knowledge_revision_segment(no_derived_bundle.segment(SegmentKind::Revisions))
                .unwrap()
                .derived
                .is_empty(),
            "the no-Derived-Operator treatment must contain no executable Derived Operators"
        );

        assert_atomic_same_path_ablation(&directory, &full, &full_bundle);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn assert_invalid_model_templates(
        full: &Path,
        bootstrap: &CanonicalBundle,
        cross_domain_template: &Path,
        rejected: &Path,
    ) {
        let cross_domain = CanonicalBundle::new(
            b"reflex-test/another-domain".to_vec(),
            bootstrap.segment(SegmentKind::Session).to_vec(),
            bootstrap.segment(SegmentKind::Revisions).to_vec(),
            bootstrap.segment(SegmentKind::Artifacts).to_vec(),
            bootstrap.segment(SegmentKind::Experience).to_vec(),
            bootstrap.segment(SegmentKind::Recovery).to_vec(),
        );
        std::fs::write(cross_domain_template, cross_domain.encode()).unwrap();
        assert!(
            ablate_bundle(
                full,
                rejected,
                Some(cross_domain_template),
                false,
                RESIDENT_BYTES,
            )
            .is_err(),
            "a no-model template from another Domain must be rejected",
        );
        assert!(!rejected.exists());
        assert!(
            ablate_bundle(full, rejected, Some(full), false, RESIDENT_BYTES).is_err(),
            "a no-model template must contain the exact empty Bootstrap Model Ecology",
        );
        assert!(!rejected.exists());
    }

    fn assert_treatment_isolation(
        bootstrap: &CanonicalBundle,
        full: &CanonicalBundle,
        no_model: &CanonicalBundle,
        no_derived: &CanonicalBundle,
    ) {
        let bootstrap_core = inspect_bundle_intelligence_components(bootstrap);
        let full_core = inspect_bundle_intelligence_components(full);
        let no_model_core = inspect_bundle_intelligence_components(no_model);
        let no_derived_core = inspect_bundle_intelligence_components(no_derived);
        assert_eq!(no_model_core.model_ecology, bootstrap_core.model_ecology);
        assert_ne!(no_model_core.model_ecology, full_core.model_ecology);
        assert_eq!(no_model_core.causal_experience, full_core.causal_experience);
        assert_eq!(
            no_model_core.knowledge_compiler,
            full_core.knowledge_compiler
        );
        assert_eq!(no_model_core.runtime_policy, full_core.runtime_policy);
        assert_eq!(no_derived_core.model_ecology, full_core.model_ecology);
        assert_eq!(
            no_derived_core.causal_experience,
            full_core.causal_experience
        );
        assert_eq!(no_derived_core.runtime_policy, full_core.runtime_policy);
        assert_eq!(no_derived_core.knowledge_records, 0);
        assert!(!no_derived_core.pending_knowledge_verification);
        assert_eq!(no_derived_core.active_derived_operators, 0);
        for treatment in [no_model, no_derived] {
            for kind in [
                SegmentKind::Artifacts,
                SegmentKind::Experience,
                SegmentKind::Recovery,
            ] {
                assert_eq!(treatment.segment(kind), full.segment(kind));
            }
        }
    }

    fn assert_atomic_same_path_ablation(
        directory: &Path,
        full: &Path,
        full_bundle: &CanonicalBundle,
    ) {
        let in_place = directory.join("in-place.bundle");
        std::fs::copy(full, &in_place).unwrap();
        ablate_bundle(&in_place, &in_place, None, true, RESIDENT_BYTES).unwrap();
        validate_treatment_bundle(&in_place).unwrap();

        let malformed = directory.join("malformed.bundle");
        let mut malformed_bundle = full_bundle.clone();
        let mut malformed_artifacts = malformed_bundle.segment(SegmentKind::Artifacts).to_vec();
        malformed_artifacts.push(0);
        malformed_bundle.replace_segment(SegmentKind::Artifacts, malformed_artifacts);
        let malformed_bytes = malformed_bundle.encode();
        std::fs::write(&malformed, &malformed_bytes).unwrap();
        assert!(
            ablate_bundle(&malformed, &malformed, None, true, RESIDENT_BYTES).is_err(),
            "the ablation adapter must consume the complete Artifact framing",
        );
        assert_eq!(std::fs::read(&malformed).unwrap(), malformed_bytes);
    }

    fn inspect_bundle_intelligence_components(
        bundle: &CanonicalBundle,
    ) -> IntelligenceComponentInspection {
        inspect_intelligence_components(
            current_intelligence_checkpoint(bundle.segment(SegmentKind::Revisions)).unwrap(),
        )
        .unwrap()
    }

    #[test]
    #[ignore = "full consumed-corpus recovery gate; run explicitly before successor confirmation"]
    fn consumed_v4_full_treatment_recovers_with_derived_operators() {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
        let report: ConsumedReport =
            serde_json::from_slice(&std::fs::read(workspace.join(CONSUMED_V4_REPORT)).unwrap())
                .unwrap();
        assert_eq!(report.audit_corpus_sha256, CONSUMED_V4_AUDIT_SHA256);
        let directory = std::env::temp_dir().join(format!(
            "reflex-causal-derived-recovery-development-{}",
            std::process::id()
        ));
        std::fs::create_dir(&directory).unwrap();
        let full = directory.join("full.bundle");
        let bootstrap = directory.join("bootstrap.bundle");
        build_training_bundles(&full, &bootstrap).unwrap();
        let seeds = report.audit_corpora[0]
            .iter()
            .map(expression)
            .collect::<Vec<_>>();
        let origins = seeds
            .iter()
            .map(artifact_key)
            .collect::<Result<BTreeSet<_>, _>>()
            .unwrap();
        let evaluated = improve(
            BitVecDomain::unary_u8(),
            request(
                seeds.clone(),
                VERIFICATION_REQUESTS,
                BundlePlan::Resume {
                    source: full.clone(),
                    target: full.clone(),
                },
            )
            .unwrap(),
            |_| ControlFlow::Continue(()),
        )
        .unwrap();
        let (count, aggregates, digest, _) = audit_outcome(&evaluated, &origins);
        recover_child(seeds, &full, &origins, count, aggregates, &digest).unwrap();
        std::fs::remove_dir_all(directory).unwrap();
    }
}
