use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use cpu_time::ProcessTime;
use reflex_lean::LeanSnapshotPin;
use reflex_lean::catalog::LeanCatalog;
use reflex_lean::temporal::{
    POTENTIAL_HEADS, PotentialHead, TasteModel, TemporalExample, TemporalPair, TemporalSnapshot,
    Treatment, certify_relationship, consolidate_certificates, migrate_theorems,
};
use reflex_lean::worker::{LeanWorker, LeanWorkerConfig};
use serde::Serialize;

use crate::harness::{
    AnyError, HostEnvironment, environment, hash_json, peak_process_resident_bytes, require_absent,
    require_clean, require_release,
};

const SCHEMA: &str = "reflex-lean-taste-development-v2";
const CUTOFF: &str = "2025-01-01T00:00:00Z";

struct Arguments {
    lake: PathBuf,
    june_root: PathBuf,
    september_root: PathBuf,
    december_root: PathBuf,
    june_catalog: PathBuf,
    september_catalog: PathBuf,
    december_catalog: PathBuf,
    output: PathBuf,
    certificate_limit: usize,
    migration_limit: usize,
}

#[derive(Serialize)]
struct Protocol {
    schema: &'static str,
    cutoff: &'static str,
    training_pair: [&'static str; 2],
    selection_pair: [&'static str; 2],
    treatments: Vec<&'static str>,
    baselines: Vec<&'static str>,
    ranking_limit: usize,
    certificate_limit: usize,
    migration_limit: usize,
    audit_access: &'static str,
    reuse_target: &'static str,
    descendant_target: &'static str,
}

#[derive(Serialize)]
struct PairSummary {
    earlier_environment_sha256: String,
    later_environment_sha256: String,
    examples: usize,
    later_new_declarations: usize,
    relationship_candidates: usize,
}

#[derive(Serialize)]
struct RankedResult {
    treatment: String,
    selected: usize,
    training_wall_ns: u64,
    training_cpu_ns: u64,
    ranking_wall_ns: u64,
    model_bytes: usize,
    model_sha256: String,
    strategies: [&'static str; POTENTIAL_HEADS],
    mean_targets: [f32; POTENTIAL_HEADS],
}

#[derive(Serialize)]
struct BaselineResult {
    baseline: &'static str,
    selected: usize,
    ranking_wall_ns: u64,
    ranking_cpu_ns: u64,
    mean_targets: [f32; POTENTIAL_HEADS],
}

#[derive(Serialize)]
struct RelationshipSummary {
    attempted: usize,
    certified: usize,
    rejected_or_unavailable: usize,
    exact: usize,
    definitional: usize,
    specialization: usize,
    derivation: usize,
    family_collapse: usize,
    corpus_compression: usize,
    proof_nodes_removed: usize,
}

#[derive(Serialize)]
struct MigrationSummary {
    attempted: usize,
    migrated: usize,
    failed: usize,
    failures: Vec<String>,
}

#[derive(Serialize)]
struct Economics {
    analysis_wall_ns: u64,
    controller_cpu_ns: u64,
    controller_peak_resident_bytes: u64,
    worker_lifetime_wall_ns: u64,
    worker_cpu_upper_bound_ns: u64,
    combined_resident_upper_bound_bytes: u64,
    catalog_durable_bytes: [u64; 3],
    encoded_model_durable_bytes: usize,
    report_durable_bytes: usize,
    kernel_verification_calls: usize,
    kernel_verification_wall_ns: u64,
    kernel_verification_cpu_upper_bound_ns: u64,
}

#[derive(Serialize)]
struct Report {
    schema: &'static str,
    status: &'static str,
    protocol_sha256: String,
    protocol: Protocol,
    catalogs: [String; 3],
    pairs: [PairSummary; 2],
    selection_evaluation_examples: usize,
    treatments: Vec<RankedResult>,
    baselines: Vec<BaselineResult>,
    virtual_best_mean_targets: [f32; POTENTIAL_HEADS],
    relationships: RelationshipSummary,
    migration: MigrationSummary,
    economics: Economics,
    host: HostEnvironment,
    protocol_deviations: Vec<String>,
    content_sha256: String,
}

#[expect(
    clippy::too_many_lines,
    reason = "the Development controller keeps frozen inputs, economic accounting, and complete report assembly together"
)]
pub fn run(arguments: &[String]) -> Result<(), AnyError> {
    require_release("lean-taste-development")?;
    let run_cpu = ProcessTime::now();
    let run_started = Instant::now();
    let host = environment()?;
    require_clean(&host, SCHEMA)?;
    let arguments = parse(arguments)?;
    require_absent(&arguments.output, "Lean taste Development report")?;
    let june_catalog = LeanCatalog::load(&arguments.june_catalog)?;
    let september_catalog = LeanCatalog::load(&arguments.september_catalog)?;
    let december_catalog = LeanCatalog::load(&arguments.december_catalog)?;
    let june_config = config(&arguments.lake, &arguments.june_root, "2024-06-30")?;
    let september_config = config(&arguments.lake, &arguments.september_root, "2024-09-30")?;
    let december_config = config(&arguments.lake, &arguments.december_root, "2024-12-31")?;
    june_catalog.require_environment(&june_config.environment_identity()?)?;
    september_catalog.require_environment(&september_config.environment_identity()?)?;
    december_catalog.require_environment(&december_config.environment_identity()?)?;
    let june = TemporalSnapshot::from_catalog(&june_catalog);
    let september = TemporalSnapshot::from_catalog(&september_catalog);
    let december = TemporalSnapshot::from_catalog(&december_catalog);
    let training = TemporalPair::derive(&june, &september)?;
    let selection = TemporalPair::derive(&september, &december)?;
    let selection_examples = held_out_selection(&training.examples, &selection.examples);
    if selection_examples.is_empty() {
        return Err("rolling Temporal Snapshot Pairs produced no held-out semantic groups".into());
    }
    let ranking_limit = 256.min(selection_examples.len());
    let protocol = Protocol {
        schema: SCHEMA,
        cutoff: CUTOFF,
        training_pair: ["2024-06-30", "2024-09-30"],
        selection_pair: ["2024-09-30", "2024-12-31"],
        treatments: vec![
            "full",
            "bootstrap",
            "no-model",
            "no-consolidation",
            "immediate-only",
        ],
        baselines: vec!["uniform", "dependency-light", "historical-reuse"],
        ranking_limit,
        certificate_limit: arguments.certificate_limit,
        migration_limit: arguments.migration_limit,
        audit_access: "post-cutoff declarations forbidden",
        reuse_target: "new later theorem with a direct dependency on the earlier theorem",
        descendant_target: "increase in direct inbound declaration dependencies on the earlier theorem",
    };
    let protocol_sha256 = hash_json(&protocol)?;

    let treatments = [
        Treatment::Full,
        Treatment::Bootstrap,
        Treatment::NoModel,
        Treatment::NoConsolidation,
        Treatment::ImmediateOnly,
    ]
    .into_iter()
    .map(|treatment| {
        evaluate_treatment(
            treatment,
            &training.examples,
            &selection_examples,
            ranking_limit,
        )
    })
    .collect::<Result<Vec<_>, AnyError>>()?;
    let baselines = evaluate_baselines(&selection_examples, ranking_limit);
    let virtual_best_mean_targets = std::array::from_fn(|head| {
        let values = baselines.iter().map(|baseline| baseline.mean_targets[head]);
        if head >= 5 {
            values.fold(1.0_f32, f32::min)
        } else {
            values.fold(0.0_f32, f32::max)
        }
    });

    let worker_started = Instant::now();
    let june_worker = LeanWorker::start(&june_config)?;
    let september_worker = LeanWorker::start(&september_config)?;
    let candidate_order = stratified_relationships(
        &training.relationship_candidates,
        arguments.certificate_limit,
    );
    let mut certificates = Vec::new();
    let mut kernel_verification_calls = 0_usize;
    let mut kernel_verification_wall_ns = 0_u64;
    let mut kernel_verification_cpu_upper_bound_ns = 0_u64;
    for candidate in candidate_order {
        let result = certify_relationship(&june_worker, &september_worker, candidate)?;
        if let Some(usage) = result.usage {
            kernel_verification_calls = kernel_verification_calls.saturating_add(1);
            kernel_verification_wall_ns =
                kernel_verification_wall_ns.saturating_add(duration_ns(usage.elapsed));
            kernel_verification_cpu_upper_bound_ns = kernel_verification_cpu_upper_bound_ns
                .saturating_add(duration_ns(usage.cpu_upper_bound));
        }
        if let Some(certificate) = result.certificate {
            certificates.push(certificate);
        }
    }
    let consolidated = consolidate_certificates(&certificates);
    let relationship_summary = summarize_relationships(
        arguments
            .certificate_limit
            .min(training.relationship_candidates.len()),
        &certificates,
        &consolidated,
    );
    let migration_names = training
        .examples
        .iter()
        .take(arguments.migration_limit)
        .map(|example| example.declaration.clone())
        .collect::<Vec<_>>();
    let migration_sources = june_worker.fetch(&migration_names)?;
    let (migration, migration_usage) = migrate_theorems(&migration_sources, &september_worker)?;
    kernel_verification_calls = kernel_verification_calls.saturating_add(1);
    kernel_verification_wall_ns =
        kernel_verification_wall_ns.saturating_add(duration_ns(migration_usage.elapsed));
    kernel_verification_cpu_upper_bound_ns = kernel_verification_cpu_upper_bound_ns
        .saturating_add(duration_ns(migration_usage.cpu_upper_bound));
    let migration_summary = MigrationSummary {
        attempted: migration_sources.len(),
        migrated: migration.migrated.len(),
        failed: migration.failures.len(),
        failures: migration
            .failures
            .iter()
            .map(|failure| format!("{}: {}", failure.declaration, failure.diagnostic))
            .collect(),
    };
    let verifier_resident_upper_bound_bytes = june_worker
        .resident_bytes()
        .get()
        .saturating_add(september_worker.resident_bytes().get());
    drop(june_worker);
    drop(september_worker);
    let worker_lifetime_wall_ns = duration_ns(worker_started.elapsed());
    let controller_peak_resident_bytes = peak_process_resident_bytes();
    let catalog_durable_bytes = [
        std::fs::metadata(&arguments.june_catalog)?.len(),
        std::fs::metadata(&arguments.september_catalog)?.len(),
        std::fs::metadata(&arguments.december_catalog)?.len(),
    ];
    let encoded_model_durable_bytes = treatments.iter().map(|result| result.model_bytes).sum();

    let mut report = Report {
        schema: SCHEMA,
        status: "development",
        protocol_sha256,
        protocol,
        catalogs: [
            june_catalog.content_sha256().to_owned(),
            september_catalog.content_sha256().to_owned(),
            december_catalog.content_sha256().to_owned(),
        ],
        pairs: [pair_summary(&training), pair_summary(&selection)],
        selection_evaluation_examples: selection_examples.len(),
        treatments,
        baselines,
        virtual_best_mean_targets,
        relationships: relationship_summary,
        migration: migration_summary,
        economics: Economics {
            analysis_wall_ns: duration_ns(run_started.elapsed()),
            controller_cpu_ns: duration_ns(run_cpu.elapsed()),
            controller_peak_resident_bytes,
            worker_lifetime_wall_ns,
            worker_cpu_upper_bound_ns: worker_lifetime_wall_ns.saturating_mul(2),
            combined_resident_upper_bound_bytes: controller_peak_resident_bytes
                .saturating_add(verifier_resident_upper_bound_bytes),
            catalog_durable_bytes,
            encoded_model_durable_bytes,
            report_durable_bytes: 0,
            kernel_verification_calls,
            kernel_verification_wall_ns,
            kernel_verification_cpu_upper_bound_ns,
        },
        host,
        protocol_deviations: Vec::new(),
        content_sha256: String::new(),
    };
    let encoded = finalize_report(&mut report)?;
    if let Some(parent) = arguments.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&arguments.output, encoded)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn evaluate_treatment(
    treatment: Treatment,
    training: &[TemporalExample],
    selection: &[TemporalExample],
    limit: usize,
) -> Result<RankedResult, AnyError> {
    let cpu = ProcessTime::now();
    let started = Instant::now();
    let model = TasteModel::train(training, treatment);
    let training_wall_ns = duration_ns(started.elapsed());
    let training_cpu_ns = duration_ns(cpu.elapsed());
    let rank_started = Instant::now();
    let mut selected_union = HashSet::new();
    let mut mean_targets = [0.0; POTENTIAL_HEADS];
    for head in PotentialHead::ALL {
        let selected = if treatment == Treatment::NoModel {
            uniform(selection, limit)
        } else {
            model.rank_for_head(selection, limit, head)
        };
        selected_union.extend(selected.iter().copied());
        let index = head.index();
        mean_targets[index] = means(selection, &selected)[index];
    }
    let ranking_wall_ns = duration_ns(rank_started.elapsed());
    let encoded = model.encode()?;
    let strategies = if treatment == Treatment::NoModel {
        ["uniform"; POTENTIAL_HEADS]
    } else {
        model.strategy_names()
    };
    Ok(RankedResult {
        treatment: treatment_name(treatment).into(),
        selected: selected_union.len(),
        training_wall_ns,
        training_cpu_ns,
        ranking_wall_ns,
        model_bytes: encoded.len(),
        model_sha256: model.content_sha256(),
        strategies,
        mean_targets,
    })
}

fn finalize_report(report: &mut Report) -> Result<Vec<u8>, AnyError> {
    for _ in 0..8 {
        report.content_sha256.clear();
        report.content_sha256 = hash_json(report)?;
        let encoded = serde_json::to_vec_pretty(report)?;
        if report.economics.report_durable_bytes == encoded.len() {
            return Ok(encoded);
        }
        report.economics.report_durable_bytes = encoded.len();
    }
    Err("Lean taste report durable-byte accounting did not converge".into())
}

const fn treatment_name(treatment: Treatment) -> &'static str {
    match treatment {
        Treatment::Full => "full",
        Treatment::Bootstrap => "bootstrap",
        Treatment::NoModel => "no-model",
        Treatment::NoConsolidation => "no-consolidation",
        Treatment::ImmediateOnly => "immediate-only",
    }
}

fn evaluate_baselines(examples: &[TemporalExample], limit: usize) -> Vec<BaselineResult> {
    ["uniform", "dependency-light", "historical-reuse"]
        .into_iter()
        .map(|baseline| {
            let cpu = ProcessTime::now();
            let started = Instant::now();
            let selected = match baseline {
                "uniform" => uniform(examples, limit),
                "dependency-light" => rank_by(examples, limit, |example| -example.features[1]),
                "historical-reuse" => rank_by(examples, limit, |example| example.features[2]),
                _ => unreachable!("baseline list is exhaustive"),
            };
            BaselineResult {
                baseline,
                selected: selected.len(),
                ranking_wall_ns: duration_ns(started.elapsed()),
                ranking_cpu_ns: duration_ns(cpu.elapsed()),
                mean_targets: means(examples, &selected),
            }
        })
        .collect()
}

fn uniform(examples: &[TemporalExample], limit: usize) -> Vec<usize> {
    let mut indexes = (0..examples.len()).collect::<Vec<_>>();
    indexes.sort_unstable_by_key(|index| examples[*index].semantic_group);
    indexes.truncate(limit);
    indexes
}

fn rank_by(
    examples: &[TemporalExample],
    limit: usize,
    score: impl Fn(&TemporalExample) -> f32,
) -> Vec<usize> {
    let mut ranked = examples
        .iter()
        .enumerate()
        .map(|(index, example)| (index, score(example)))
        .collect::<Vec<_>>();
    ranked.sort_unstable_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    ranked.truncate(limit);
    ranked.into_iter().map(|entry| entry.0).collect()
}

fn means(examples: &[TemporalExample], selected: &[usize]) -> [f32; POTENTIAL_HEADS] {
    if selected.is_empty() {
        return [0.0; POTENTIAL_HEADS];
    }
    let mut means = [0.0; POTENTIAL_HEADS];
    for index in selected {
        for (mean, target) in means.iter_mut().zip(examples[*index].targets) {
            *mean += target;
        }
    }
    let count = f32::from(u16::try_from(selected.len()).unwrap_or(u16::MAX));
    means.map(|mean| mean / count)
}

fn held_out_selection(
    training: &[TemporalExample],
    selection: &[TemporalExample],
) -> Vec<TemporalExample> {
    let training_groups = training
        .iter()
        .map(|example| example.semantic_group)
        .collect::<HashSet<_>>();
    selection
        .iter()
        .filter(|example| !training_groups.contains(&example.semantic_group))
        .cloned()
        .collect()
}

fn pair_summary(pair: &TemporalPair) -> PairSummary {
    PairSummary {
        earlier_environment_sha256: pair.earlier_environment_sha256.clone(),
        later_environment_sha256: pair.later_environment_sha256.clone(),
        examples: pair.examples.len(),
        later_new_declarations: pair.later_new_declarations,
        relationship_candidates: pair.relationship_candidates.len(),
    }
}

fn summarize_relationships(
    attempted: usize,
    certificates: &[reflex_lean::temporal::CertifiedRelationship],
    consolidated: &[reflex_lean::temporal::ConsolidatedRelationship],
) -> RelationshipSummary {
    use reflex_lean::temporal::RelationshipKind;
    let count = |kind| certificates.iter().filter(|item| item.kind == kind).count();
    RelationshipSummary {
        attempted,
        certified: certificates.len(),
        rejected_or_unavailable: attempted.saturating_sub(certificates.len()),
        exact: count(RelationshipKind::Exact),
        definitional: count(RelationshipKind::Definitional),
        specialization: count(RelationshipKind::Specialization),
        derivation: count(RelationshipKind::Derivation),
        family_collapse: consolidated
            .iter()
            .filter(|item| item.kind == RelationshipKind::FamilyCollapse)
            .count(),
        corpus_compression: consolidated
            .iter()
            .filter(|item| item.kind == RelationshipKind::CorpusCompression)
            .count(),
        proof_nodes_removed: certificates
            .iter()
            .map(|item| item.proof_nodes_removed)
            .sum(),
    }
}

fn stratified_relationships(
    candidates: &[reflex_lean::temporal::RelationshipCandidate],
    limit: usize,
) -> Vec<&reflex_lean::temporal::RelationshipCandidate> {
    use reflex_lean::temporal::RelationshipKind;
    let exact_limit = limit.saturating_mul(2).div_ceil(3);
    let mut selected = candidates
        .iter()
        .filter(|candidate| candidate.expected == RelationshipKind::Exact)
        .take(exact_limit)
        .collect::<Vec<_>>();
    let remaining = limit.saturating_sub(selected.len());
    selected.extend(
        candidates
            .iter()
            .filter(|candidate| candidate.expected == RelationshipKind::Specialization)
            .take(remaining.div_ceil(2)),
    );
    selected.extend(
        candidates
            .iter()
            .filter(|candidate| candidate.expected == RelationshipKind::Derivation)
            .take(limit.saturating_sub(selected.len())),
    );
    selected
}

fn config(lake: &PathBuf, root: &PathBuf, snapshot: &str) -> Result<LeanWorkerConfig, AnyError> {
    let pin = match snapshot {
        "2024-06-30" => LeanSnapshotPin::new(
            "454c40501feacb5aef56e707d6b348fc68897dce",
            "leanprover/lean4:v4.9.0-rc3",
            "4.9.0-rc3",
            "4.9.0",
            "141856d6e6d808a85b9147a530294fee8e48e15f",
        ),
        "2024-09-30" => LeanSnapshotPin::new(
            "37814caf0b1b93a00743c1dd7af97ceb6b092b40",
            "leanprover/lean4:v4.12.0-rc1",
            "4.12.0-rc1",
            "4.12.0",
            "e9e858a4484905a0bfe97c4f05c3924ead02eed8",
        ),
        "2024-12-31" => LeanSnapshotPin::final_pre_2025(),
        _ => return Err(format!("unknown frozen snapshot {snapshot}").into()),
    };
    Ok(LeanWorkerConfig::for_snapshot(lake, root, pin))
}

fn parse(arguments: &[String]) -> Result<Arguments, AnyError> {
    let mut values = std::collections::HashMap::<&str, &str>::new();
    let mut certificate_limit = 64;
    let mut migration_limit = 64;
    let mut index = 0;
    while index < arguments.len() {
        let flag = arguments[index].as_str();
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag {
            "--certificate-limit" => certificate_limit = value.parse()?,
            "--migration-limit" => migration_limit = value.parse()?,
            "--lake"
            | "--june-root"
            | "--september-root"
            | "--december-root"
            | "--june-catalog"
            | "--september-catalog"
            | "--december-catalog"
            | "--output" => {
                if values.insert(flag, value).is_some() {
                    return Err(format!("duplicate argument {flag}").into());
                }
            }
            _ => return Err(format!("unknown lean-taste-development argument {flag}").into()),
        }
        index += 2;
    }
    if certificate_limit == 0 || migration_limit == 0 {
        return Err("certificate and migration limits must be positive".into());
    }
    let path = |flag| -> Result<PathBuf, AnyError> {
        values
            .get(flag)
            .map(|value| PathBuf::from(*value))
            .ok_or_else(|| -> AnyError { format!("lean-taste-development requires {flag}").into() })
    };
    Ok(Arguments {
        lake: path("--lake")?,
        june_root: path("--june-root")?,
        september_root: path("--september-root")?,
        december_root: path("--december-root")?,
        june_catalog: path("--june-catalog")?,
        september_catalog: path("--september-catalog")?,
        december_catalog: path("--december-catalog")?,
        output: path("--output")?,
        certificate_limit,
        migration_limit,
    })
}

fn duration_ns(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[allow(dead_code)]
fn _heads_are_stable() -> [PotentialHead; POTENTIAL_HEADS] {
    PotentialHead::ALL
}
