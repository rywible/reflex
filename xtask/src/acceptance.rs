use crate::sbom;
use crate::util::{self, Result};
use reflex_bench::{
    BudgetRegistration, BudgetStatistic, HostCalibration, MetricMeasurement,
    PerformanceBudgetRegistry,
};
use reflex_types::{Digest, DigestAlgorithm};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const ACCEPTANCE_PATH: &str = "evidence/v1/acceptance.json";

#[derive(Clone, Debug, Serialize)]
pub struct AcceptanceGate {
    pub name: &'static str,
    pub source: String,
    pub status: &'static str,
    pub passed: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct AcceptanceReport {
    pub schema: &'static str,
    pub commit: String,
    pub release_ready: bool,
    pub gates: Vec<AcceptanceGate>,
}

const SCIENTIFIC_GATES: &[(&str, &str, &str)] = &[
    (
        "bitvec_dogfood",
        "evidence/bitvec-v1/report.json",
        "reflex.bitvec-acceptance.v1",
    ),
    (
        "portable_fault_matrix",
        "evidence/fault-matrix/report.json",
        "reflex.fault-matrix-acceptance.v1",
    ),
    (
        "wrela_campaign",
        "evidence/wrela-campaigns/report.json",
        "reflex.wrela-acceptance.v1",
    ),
    (
        "lean_m1_5",
        "evidence/lean-m1.5/reconstruction.json",
        "reflex.lean-m1.5-reconstruction.v1",
    ),
    (
        "lean_m2a",
        "evidence/lean-m2a/reconstruction.json",
        "reflex.lean-m2a-reconstruction.v1",
    ),
    (
        "lean_m2b",
        "evidence/lean-m2b/report.json",
        "reflex.lean-m2b-acceptance.v1",
    ),
    (
        "knowledge_economy",
        "evidence/knowledge-economy/report.json",
        "reflex.knowledge-economy-acceptance.v1",
    ),
    (
        "security_fault_fixtures",
        "evidence/security-audit/report.json",
        "reflex.security-audit.v1",
    ),
    (
        "new_domain_handoff",
        "evidence/new-domain-handoff/report.json",
        "reflex.new-domain-handoff.v1",
    ),
];

/// Named quantitative release gates. Each needs both a registered budget and
/// a canonical-host measurement; a generic check-lane duration is not a
/// substitute for a component performance acceptance criterion.
const PERFORMANCE_GATES: &[&str] = &[
    // Master plan §3.3 initial v1 quantitative gates.
    "candidate_metadata_per_core",
    "dense_f32_feature_packing",
    "uniform_scoring_per_core",
    "search_policy_feature_overhead_ratio",
    "event_append_per_core",
    "evidence_overhead_ratio",
    "dataset_compaction_rows_per_core",
    "training_loader_cpu_ratio",
    "mlp_3k_epoch_1m",
    "coordinator_overhead_ratio",
    "canonical_digest_1k_p95",
    "ledger_append_p99",
    "ledger_recovery_1gib",
    "cas_ingest_mib_per_sec",
    "cas_fsync_p99",
    "protocol_expand_batch_p99",
    "cell_launch_p95",
    "cell_cleanup_p95",
    "thread_permit_p95",
    "search_cpu_per_node_p95",
    "frontier_ordering_p95",
    "micro_mlp_2607_single",
    "micro_mlp_2607_batch64",
    "burn_training_epoch_p95",
    "dataset_compile_rows_per_sec",
    "checkpoint_stream_peak_bytes",
    "evaluation_groups_per_sec",
    "report_rebuild_p95",
    "cli_startup_p95",
    "cli_status_100k_p95",
    "operator_api_read_p95",
    "observability_overhead_ratio",
    "acceptance_suite_resource_accounting",
];

#[derive(Clone, Debug, Deserialize, Serialize)]
struct SourceArtifact {
    path: String,
    digest: Digest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EvidencePopulation {
    name: String,
    identity: Digest,
    size: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct EvidenceQuery {
    name: String,
    identity: Digest,
    population: Digest,
    unit: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ScientificAcceptanceArtifact {
    schema: String,
    status: String,
    reconstructed: bool,
    commit: String,
    experiment_manifest: Digest,
    query_plan: Digest,
    populations: Vec<EvidencePopulation>,
    queries: Vec<EvidenceQuery>,
    sources: Vec<SourceArtifact>,
    reconstruction_receipt: Digest,
    evidence_digest: Digest,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PerformanceEvidence {
    schema: String,
    commit: String,
    measurement: MetricMeasurement,
    calibration: HostCalibration,
    sources: Vec<SourceArtifact>,
    evidence_digest: Digest,
}

pub fn evaluate() -> Result<AcceptanceReport> {
    let commit = util::git_head()?;
    let status = util::run_cmd("git", &["status", "--porcelain", "--untracked-files=all"]);
    let clean = status.success && status.stdout.is_empty();
    let mut gates = vec![
        AcceptanceGate {
            name: "clean_exact_head",
            source: "git HEAD and complete worktree".into(),
            status: if clean { "passed" } else { "open" },
            passed: clean,
            detail: if !status.success {
                "cannot determine release worktree status".into()
            } else if clean {
                "worktree is clean; evidence is evaluated against the exact full HEAD".into()
            } else {
                "worktree is dirty; commit the exact sources and evidence before release".into()
            },
        },
        check_check_report("fast_check", "evidence/checks/fast.json", &commit),
        check_check_report("deep_check", "evidence/checks/deep.json", &commit),
        check_invariant_report(
            "invariant_closure",
            "evidence/invariants/report.json",
            &commit,
        ),
    ];
    let sbom_passed = sbom::sbom_matches(Path::new("Cargo.lock"))?;
    gates.push(AcceptanceGate {
        name: "sbom",
        source: sbom::SBOM_PATH.into(),
        status: if sbom_passed { "passed" } else { "open" },
        passed: sbom_passed,
        detail: "committed CycloneDX document must byte-match Cargo.lock reconstruction".into(),
    });
    gates.extend(
        SCIENTIFIC_GATES
            .iter()
            .map(|(name, path, schema)| check_scientific_artifact(name, path, schema, &commit)),
    );
    let registry = PerformanceBudgetRegistry::new();
    gates.extend(
        PERFORMANCE_GATES
            .iter()
            .map(|name| check_performance_gate(&registry, name, &commit)),
    );
    gates.push(aggregate_gate(
        "gate_a_foundation",
        &[
            "fast_check",
            "deep_check",
            "invariant_closure",
            "portable_fault_matrix",
        ],
        &gates,
    ));
    gates.push(aggregate_gate(
        "gate_b_search_ml",
        &["fast_check", "bitvec_dogfood"],
        &gates,
    ));
    gates.push(aggregate_gate(
        "gate_c_tutorial",
        &["bitvec_dogfood"],
        &gates,
    ));
    gates.push(aggregate_gate(
        "gate_d_resilience",
        &["portable_fault_matrix"],
        &gates,
    ));
    gates.push(aggregate_gate(
        "gate_e_real_domains",
        &["wrela_campaign", "lean_m1_5", "lean_m2a", "lean_m2b"],
        &gates,
    ));
    gates.push(aggregate_gate(
        "gate_f_knowledge",
        &["knowledge_economy"],
        &gates,
    ));
    let gate_g_passed = gates.iter().all(|gate| gate.passed);
    gates.push(AcceptanceGate {
        name: "gate_g_v1",
        source: "derived from all blocking scientific, invariant, supply-chain, security, handoff, and performance gates".into(),
        status: if gate_g_passed { "passed" } else { "open" },
        passed: gate_g_passed,
        detail: if gate_g_passed {
            "all blocking v1 gates passed".into()
        } else {
            "one or more blocking v1 gates remain open".into()
        },
    });
    let release_ready = gate_g_passed;
    Ok(AcceptanceReport {
        schema: "reflex.v1-acceptance.v1",
        commit,
        release_ready,
        gates,
    })
}

fn aggregate_gate(
    name: &'static str,
    required: &[&str],
    gates: &[AcceptanceGate],
) -> AcceptanceGate {
    let open: Vec<&str> = required
        .iter()
        .copied()
        .filter(|required_name| {
            !gates
                .iter()
                .any(|gate| gate.name == *required_name && gate.passed)
        })
        .collect();
    let passed = open.is_empty();
    AcceptanceGate {
        name,
        source: required.join(","),
        status: if passed { "passed" } else { "open" },
        passed,
        detail: if passed {
            "all constituent gates passed".into()
        } else {
            format!("constituent gates still open: {}", open.join(", "))
        },
    }
}

fn check_performance_gate(
    registry: &PerformanceBudgetRegistry,
    name: &'static str,
    commit: &str,
) -> AcceptanceGate {
    let source = format!("evidence/performance/{name}.json");
    match registry.registration(name) {
        Some(BudgetRegistration::Budget(_)) => {}
        Some(BudgetRegistration::Open(entry)) => {
            return AcceptanceGate {
                name,
                source,
                status: "open",
                passed: false,
                detail: format!("performance contract is explicitly open: {}", entry.reason),
            };
        }
        None => {
            return AcceptanceGate {
                name,
                source,
                status: "open",
                passed: false,
                detail: "required performance gate has no registry registration".into(),
            };
        }
    }
    let evidence = util::read_json(Path::new(&source)).and_then(|value| {
        serde_json::from_value::<PerformanceEvidence>(value.clone())
            .map(|evidence| (evidence, value))
            .map_err(Into::into)
    });
    match evidence {
        Ok((evidence, value))
            if performance_evidence_valid(&evidence, &value, commit, Path::new(&source))
                && registry
                    .check_measurement(&evidence.measurement)
                    .is_ok() =>
        {
            AcceptanceGate {
            name,
            source,
            status: "passed",
            passed: true,
            detail: "raw samples reconstruct and pass the registered budget on an authoritative calibrated host".into(),
        }
        }
        Ok(_) => AcceptanceGate {
            name,
            source,
            status: "open",
            passed: false,
            detail: "measurement does not satisfy its registered budget and host class".into(),
        },
        Err(error) => AcceptanceGate {
            name,
            source,
            status: "open",
            passed: false,
            detail: format!("measurement unavailable: {error}"),
        },
    }
}

fn performance_evidence_valid(
    evidence: &PerformanceEvidence,
    value: &Value,
    commit: &str,
    report_path: &Path,
) -> bool {
    let enough_samples = match evidence.measurement.statistic {
        BudgetStatistic::P95 | BudgetStatistic::P99 => {
            evidence.measurement.raw_samples.numerators.len() >= 20
        }
        BudgetStatistic::RateOfSums | BudgetStatistic::RatioOfSums | BudgetStatistic::Maximum => {
            true
        }
    };
    evidence.schema == "reflex.performance-evidence.v2"
        && evidence.commit == commit
        && evidence.measurement.validate().is_ok()
        && enough_samples
        && evidence.calibration.is_valid_for_claim()
        && evidence.calibration.host_class == evidence.measurement.host_class
        && evidence.calibration.identity.git_identity == commit
        && validate_sources(&evidence.sources, report_path)
        && self_digest_matches(value, evidence.evidence_digest)
}

fn validate_sources(sources: &[SourceArtifact], report_path: &Path) -> bool {
    if sources.is_empty() {
        return false;
    }
    let Some(parent) = report_path.parent() else {
        return false;
    };
    let parent = match parent.canonicalize() {
        Ok(parent) => parent,
        Err(_) => return false,
    };
    let mut previous = None::<&str>;
    let mut unique = BTreeSet::new();
    for source in sources {
        if source.digest == Digest::ZERO
            || source.digest.algorithm != DigestAlgorithm::Blake3
            || previous.is_some_and(|path| path >= source.path.as_str())
            || !unique.insert(source.path.as_str())
        {
            return false;
        }
        previous = Some(&source.path);
        let path = Path::new(&source.path);
        if path.is_absolute()
            || path
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return false;
        }
        let canonical = match path.canonicalize() {
            Ok(path) => path,
            Err(_) => return false,
        };
        if !canonical.starts_with(&parent)
            || hash_file(&canonical).is_none_or(|digest| digest != source.digest)
        {
            return false;
        }
    }
    exact_source_manifest(sources, report_path, &parent)
}

fn exact_source_manifest(
    sources: &[SourceArtifact],
    report_path: &Path,
    canonical_parent: &Path,
) -> bool {
    let report = match report_path.canonicalize() {
        Ok(path) => path,
        Err(_) => return false,
    };
    let repository = match std::env::current_dir().and_then(|path| path.canonicalize()) {
        Ok(path) => path,
        Err(_) => return false,
    };
    let mut pending = vec![canonical_parent.to_path_buf()];
    let mut actual = BTreeSet::new();
    while let Some(directory) = pending.pop() {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(_) => return false,
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(_) => return false,
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(_) => return false,
            };
            let path = entry.path();
            if file_type.is_symlink() {
                return false;
            }
            if file_type.is_dir() {
                pending.push(path);
            } else if file_type.is_file() && path != report {
                let relative = match path.strip_prefix(&repository) {
                    Ok(relative) => relative,
                    Err(_) => return false,
                };
                actual.insert(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let declared: BTreeSet<_> = sources.iter().map(|source| source.path.clone()).collect();
    actual == declared
}

fn hash_file(path: &Path) -> Option<Digest> {
    let mut file = File::open(path).ok()?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).ok()?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Some(Digest::from_blake3_bytes(*hasher.finalize().as_bytes()))
}

fn self_digest_matches(value: &Value, expected: Digest) -> bool {
    if expected == Digest::ZERO || expected.algorithm != DigestAlgorithm::Blake3 {
        return false;
    }
    let mut identity = value.clone();
    let Some(object) = identity.as_object_mut() else {
        return false;
    };
    object.remove("evidence_digest");
    serde_json::to_vec(&identity)
        .ok()
        .is_some_and(|bytes| Digest::hash_blake3(&bytes) == expected)
}

pub fn write(report: &AcceptanceReport, output: Option<PathBuf>) -> Result<()> {
    let path = output.unwrap_or_else(|| PathBuf::from(ACCEPTANCE_PATH));
    let value = serde_json::to_value(report)?;
    util::write_pretty(&path, &value)
}

fn check_check_report(name: &'static str, path: &str, commit: &str) -> AcceptanceGate {
    check_json(path, name, |value| {
        value.get("commit").and_then(Value::as_str) == Some(commit)
            && value.get("overall").and_then(Value::as_str) == Some("passed")
            && value
                .get("steps")
                .and_then(Value::as_array)
                .is_some_and(|steps| {
                    !steps.is_empty()
                        && steps.iter().all(|step| {
                            step.get("ok").and_then(Value::as_bool) == Some(true)
                                && step.get("skipped").and_then(Value::as_bool) != Some(true)
                        })
                })
    })
}

fn check_invariant_report(name: &'static str, path: &str, commit: &str) -> AcceptanceGate {
    check_json(path, name, |value| {
        value.get("commit").and_then(Value::as_str) == Some(commit)
            && value.get("overall").and_then(Value::as_str) == Some("passed")
    })
}

fn check_scientific_artifact(
    name: &'static str,
    path: &str,
    expected_schema: &str,
    commit: &str,
) -> AcceptanceGate {
    check_json(path, name, |value| {
        let Ok(artifact) = serde_json::from_value::<ScientificAcceptanceArtifact>(value.clone())
        else {
            return false;
        };
        artifact.schema == expected_schema
            && artifact.status == "passed"
            && artifact.reconstructed
            && artifact.commit == commit
            && artifact.experiment_manifest != Digest::ZERO
            && artifact.query_plan != Digest::ZERO
            && artifact.reconstruction_receipt != Digest::ZERO
            && artifact.experiment_manifest != artifact.query_plan
            && artifact.query_plan != artifact.reconstruction_receipt
            && artifact.experiment_manifest != artifact.reconstruction_receipt
            && scientific_identities_valid(&artifact)
            && validate_sources(&artifact.sources, Path::new(path))
            && self_digest_matches(value, artifact.evidence_digest)
    })
}

fn scientific_identities_valid(artifact: &ScientificAcceptanceArtifact) -> bool {
    if artifact.populations.is_empty() || artifact.queries.is_empty() {
        return false;
    }
    let source_digests: BTreeSet<_> = artifact
        .sources
        .iter()
        .map(|source| source.digest)
        .collect();
    if ![
        artifact.experiment_manifest,
        artifact.query_plan,
        artifact.reconstruction_receipt,
    ]
    .iter()
    .all(|digest| source_digests.contains(digest))
    {
        return false;
    }
    let mut population_names = BTreeSet::new();
    let mut population_ids = BTreeSet::new();
    if artifact.populations.iter().any(|population| {
        population.name.trim().is_empty()
            || population.identity == Digest::ZERO
            || population.size == 0
            || !population_names.insert(population.name.as_str())
            || !population_ids.insert(population.identity)
    }) {
        return false;
    }
    let mut query_names = BTreeSet::new();
    let mut query_ids = BTreeSet::new();
    !artifact.queries.iter().any(|query| {
        query.name.trim().is_empty()
            || query.unit.trim().is_empty()
            || query.identity == Digest::ZERO
            || !population_ids.contains(&query.population)
            || !query_names.insert(query.name.as_str())
            || !query_ids.insert(query.identity)
    })
}

fn check_json(
    path: &str,
    name: &'static str,
    predicate: impl FnOnce(&Value) -> bool,
) -> AcceptanceGate {
    match util::read_json(Path::new(path)) {
        Ok(value) if predicate(&value) => AcceptanceGate {
            name,
            source: path.into(),
            status: "passed",
            passed: true,
            detail: "validated".into(),
        },
        Ok(_) => AcceptanceGate {
            name,
            source: path.into(),
            status: "open",
            passed: false,
            detail: "artifact exists but does not satisfy its acceptance contract".into(),
        },
        Err(error) => AcceptanceGate {
            name,
            source: path.into(),
            status: "open",
            passed: false,
            detail: format!("artifact unavailable: {error}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_bench::{
        BudgetMetric, BudgetStatistic, BudgetUnit, HostCalibration, RawMetricSamples,
    };

    #[test]
    fn scientific_gate_rejects_zero_or_unreconstructed_claims() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("claim.json");
        std::fs::write(
            &path,
            r#"{"status":"passed","reconstructed":true,"evidence_digest":"blake3:0000000000000000000000000000000000000000000000000000000000000000"}"#,
        )
        .unwrap();
        let path = path.to_str().unwrap();
        assert!(!check_scientific_artifact("fixture", path, "reflex.fixture.v1", "commit").passed);
    }

    #[test]
    fn performance_gate_rejects_summary_without_sufficient_raw_samples() {
        let evidence = PerformanceEvidence {
            schema: "reflex.performance-evidence.v2".into(),
            commit: "commit".into(),
            measurement: MetricMeasurement {
                name: "micro_mlp_2607_batch64".into(),
                host_class: "reference-4vcpu-8gb".into(),
                metric: BudgetMetric::Latency,
                unit: BudgetUnit::Nanoseconds,
                statistic: BudgetStatistic::P95,
                value: 1.0,
                raw_samples: RawMetricSamples {
                    numerators: vec![1.0],
                    denominators: vec![],
                },
            },
            calibration: HostCalibration::calibrate_current_host(),
            sources: vec![],
            evidence_digest: Digest::ZERO,
        };
        let value = serde_json::to_value(&evidence).unwrap();
        assert!(!performance_evidence_valid(
            &evidence,
            &value,
            "commit",
            Path::new("evidence/performance/micro_mlp_2607_batch64.json"),
        ));
    }
}
