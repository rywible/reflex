use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use reflex_lean::ast::LeanExpr;
use reflex_lean::catalog::LeanCatalog;
use reflex_lean::worker::{IndexedTheorem, LeanWorker, LeanWorkerConfig, VerificationItem};
use serde::Serialize;

use crate::harness::{AnyError, environment, hash_json, require_absent};

const SCHEMA: &str = "reflex-lean-development-v2";
const LATENCY_SCHEMA: &str = "reflex-lean-fixed-latency-v1";
const FIXED_COLLAPSES: [(&str, &str); 5] = [
    (
        "ContinuousMap.compactOpen_eq_iInf_induced",
        "ContinuousMap.compactOpen_eq_sInf_induced",
    ),
    ("Matrix.linfty_opNNNorm_col", "Matrix.linfty_op_nnnorm_col"),
    ("sub_neg", "sub_lt_zero"),
    ("ContMDiffWithinAt.sub", "SmoothWithinAt.sub"),
    (
        "tendsto_norm_sub_self_nhdsNE",
        "tendsto_norm_sub_self_punctured_nhds",
    ),
];

#[derive(Serialize)]
struct Collapse {
    seed: String,
    replacement: String,
    seed_proof_nodes: usize,
    replacement_nodes: usize,
    nodes_removed: usize,
    seed_axioms: Vec<String>,
    replacement_axioms: Vec<String>,
}

#[derive(Serialize)]
struct Report {
    schema: &'static str,
    status: &'static str,
    mathlib_commit: String,
    lean_toolchain: String,
    lean_commit: String,
    offset: usize,
    requested_declarations: usize,
    indexed_declarations: usize,
    total_declarations: usize,
    exact_statement_groups: usize,
    verified_collapses: usize,
    validation_and_cold_start_wall_ms: u128,
    fingerprint_scan_wall_ms: u128,
    fetch_and_verify_wall_ms: u128,
    host: crate::harness::HostEnvironment,
    collapses: Vec<Collapse>,
    content_sha256: String,
}

struct Arguments {
    lake: PathBuf,
    mathlib: PathBuf,
    output: PathBuf,
    offset: usize,
    count: usize,
}

#[derive(Serialize)]
struct FixedLatencyReport {
    schema: &'static str,
    status: &'static str,
    cases: usize,
    replicates_per_case: usize,
    validation_and_cold_start_wall_ms: u128,
    warm_improvement_p50_ms: u128,
    warm_improvement_p95_ms: u128,
    clean_restore_and_replay_wall_ms: u128,
    restore_gate_ms: u128,
    warm_p50_gate_ms: u128,
    warm_p95_gate_ms: u128,
    host: crate::harness::HostEnvironment,
    content_sha256: String,
}

struct FixedLatencyArguments {
    lake: PathBuf,
    mathlib: PathBuf,
    output: PathBuf,
    replicates: usize,
}

pub fn fixed_latency(arguments: &[String]) -> Result<(), AnyError> {
    let arguments = parse_fixed_latency(arguments)?;
    require_absent(&arguments.output, "Lean fixed latency report")?;
    let config = LeanWorkerConfig::pinned(&arguments.lake, &arguments.mathlib);
    let cold_started = Instant::now();
    let worker = LeanWorker::start(&config)?;
    let validation_and_cold_start_wall_ms = cold_started.elapsed().as_millis();
    let names = FIXED_COLLAPSES
        .iter()
        .flat_map(|(seed, replacement)| [seed, replacement])
        .map(|name| reflex_lean::ast::LeanName::from_dotted(name))
        .collect::<Vec<_>>();
    let theorems = worker.fetch(&names)?;
    let by_name = theorems
        .into_iter()
        .map(|theorem| (theorem.name.to_string(), theorem))
        .collect::<HashMap<_, _>>();
    let items = FIXED_COLLAPSES
        .iter()
        .map(|(seed_name, replacement_name)| {
            let seed = by_name
                .get(*seed_name)
                .ok_or_else(|| format!("fixed seed {seed_name} is unavailable"))?;
            let replacement = by_name
                .get(*replacement_name)
                .ok_or_else(|| format!("fixed replacement {replacement_name} is unavailable"))?;
            if replacement.proof_term.node_count() >= seed.proof_term.node_count() {
                return Err(format!(
                    "fixed case {seed_name} is not a proof-node improvement"
                ));
            }
            Ok(VerificationItem {
                level_params: seed.level_params.clone(),
                claim_proposition: seed.proposition.clone(),
                candidate_proposition: replacement.proposition.clone(),
                proof_term: replacement.proof_term.clone(),
                allowed_axioms: seed.axioms.clone(),
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    verify_fixed_items(&worker, &items)?;
    verify_fixed_items(&worker, &items)?;
    let mut latencies = Vec::with_capacity(items.len() * arguments.replicates);
    for _ in 0..arguments.replicates {
        for item in &items {
            let started = Instant::now();
            verify_fixed_items(&worker, std::slice::from_ref(item))?;
            latencies.push(started.elapsed().as_millis());
        }
    }
    latencies.sort_unstable();
    let p50 = nearest_rank(&latencies, 50);
    let p95 = nearest_rank(&latencies, 95);
    drop(worker);

    let restore_started = Instant::now();
    let restored = LeanWorker::start(&config)?;
    verify_fixed_items(&restored, std::slice::from_ref(&items[0]))?;
    let restore = restore_started.elapsed().as_millis();
    let restore_gate = 60_000;
    let p50_gate = 1_000;
    let p95_gate = 5_000;
    if restore > restore_gate || p50 > p50_gate || p95 > p95_gate {
        return Err(format!(
            "Lean latency gate failed: restore={restore}ms p50={p50}ms p95={p95}ms"
        )
        .into());
    }
    let mut report = FixedLatencyReport {
        schema: LATENCY_SCHEMA,
        status: "development",
        cases: items.len(),
        replicates_per_case: arguments.replicates,
        validation_and_cold_start_wall_ms,
        warm_improvement_p50_ms: p50,
        warm_improvement_p95_ms: p95,
        clean_restore_and_replay_wall_ms: restore,
        restore_gate_ms: restore_gate,
        warm_p50_gate_ms: p50_gate,
        warm_p95_gate_ms: p95_gate,
        host: environment()?,
        content_sha256: String::new(),
    };
    report.content_sha256 = hash_json(&report)?;
    if let Some(parent) = arguments.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&arguments.output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn verify_fixed_items(worker: &LeanWorker, items: &[VerificationItem]) -> Result<(), AnyError> {
    let (results, _) = worker.verify(items)?;
    for result in results {
        if !result.accepted {
            return Err(format!("fixed Lean collapse was rejected: {}", result.diagnostic).into());
        }
    }
    Ok(())
}

fn nearest_rank(sorted: &[u128], percentile: usize) -> u128 {
    let rank = sorted
        .len()
        .saturating_mul(percentile)
        .div_ceil(100)
        .saturating_sub(1);
    sorted[rank]
}

fn parse_fixed_latency(arguments: &[String]) -> Result<FixedLatencyArguments, AnyError> {
    let mut lake = None;
    let mut mathlib = None;
    let mut output = None;
    let mut replicates = 20;
    let mut index = 0;
    while index < arguments.len() {
        let flag = &arguments[index];
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--lake" => lake = Some(PathBuf::from(value)),
            "--mathlib" => mathlib = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--replicates" => replicates = value.parse()?,
            _ => return Err(format!("unknown lean-fixed-latency argument: {flag}").into()),
        }
        index += 2;
    }
    if replicates == 0 {
        return Err("lean-fixed-latency requires positive replicates".into());
    }
    Ok(FixedLatencyArguments {
        lake: lake.ok_or("lean-fixed-latency requires --lake PATH")?,
        mathlib: mathlib.ok_or("lean-fixed-latency requires --mathlib PATH")?,
        output: output.ok_or("lean-fixed-latency requires --output PATH")?,
        replicates,
    })
}

pub fn build_catalog(arguments: &[String]) -> Result<(), AnyError> {
    let arguments = parse(arguments)?;
    require_absent(&arguments.output, "Lean catalog")?;
    let started = Instant::now();
    let worker = LeanWorker::start(&LeanWorkerConfig::pinned(
        &arguments.lake,
        &arguments.mathlib,
    ))?;
    let catalog = LeanCatalog::build(&worker, 4096)?;
    catalog.save_new(&arguments.output)?;
    println!(
        "catalog_entries={} eligible_entries={} content_sha256={} wall_ms={}",
        catalog.entries().len(),
        catalog.eligible_entries().count(),
        catalog.content_sha256(),
        started.elapsed().as_millis()
    );
    Ok(())
}

pub fn check_catalog(arguments: &[String]) -> Result<(), AnyError> {
    let [flag, path] = arguments else {
        return Err("lean-catalog-check requires --input PATH".into());
    };
    if flag != "--input" {
        return Err("lean-catalog-check requires --input PATH".into());
    }
    let started = Instant::now();
    let catalog = LeanCatalog::load(&PathBuf::from(path))?;
    println!(
        "catalog_entries={} eligible_entries={} content_sha256={} load_ms={}",
        catalog.entries().len(),
        catalog.eligible_entries().count(),
        catalog.content_sha256(),
        started.elapsed().as_millis()
    );
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the exploratory controller keeps corpus selection, kernel checks, and report assembly auditable"
)]
pub fn run_development(arguments: &[String]) -> Result<(), AnyError> {
    let arguments = parse(arguments)?;
    require_absent(&arguments.output, "Lean development report")?;
    let started = Instant::now();
    let worker = LeanWorker::start(&LeanWorkerConfig::pinned(
        &arguments.lake,
        &arguments.mathlib,
    ))?;
    let validation_and_cold_start_wall_ms = started.elapsed().as_millis();
    let scan_started = Instant::now();
    let mut fingerprints = Vec::new();
    let mut next = arguments.offset;
    let end = arguments.offset.saturating_add(arguments.count);
    let mut total = 0;
    while next < end {
        let limit = (end - next).min(4096);
        let page = worker.fingerprint_page(next, limit)?;
        total = page.total;
        fingerprints.extend(page.fingerprints);
        next = next.saturating_add(limit);
        if next >= total {
            break;
        }
    }

    let mut hash_groups: HashMap<String, Vec<reflex_lean::ast::LeanName>> = HashMap::new();
    let fingerprint_scan_wall_ms = scan_started.elapsed().as_millis();
    let verify_started = Instant::now();
    for fingerprint in fingerprints
        .iter()
        .filter(|fingerprint| fingerprint.kind == "theorem" && fingerprint.locally_eligible)
    {
        hash_groups
            .entry(fingerprint.statement_hash.clone())
            .or_default()
            .push(fingerprint.name.clone());
    }
    let possible_names = hash_groups
        .into_values()
        .filter(|group| group.len() > 1)
        .flat_map(|group| group.into_iter().take(32))
        .collect::<Vec<_>>();
    let full = possible_names
        .chunks(32)
        .map(|names| worker.fetch(names))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut groups: HashMap<LeanExpr, Vec<IndexedTheorem>> = HashMap::new();
    for theorem in full.iter().cloned() {
        groups
            .entry(theorem.proposition.clone())
            .or_default()
            .push(theorem);
    }
    let duplicate_groups = groups
        .into_values()
        .filter(|group| group.len() > 1)
        .collect::<Vec<_>>();
    let full_by_name = full
        .iter()
        .cloned()
        .map(|theorem| (theorem.name.clone(), theorem))
        .collect::<HashMap<_, _>>();

    let mut collapses = Vec::new();
    for group in &duplicate_groups {
        for seed in group {
            let Some(full_seed) = full_by_name.get(&seed.name) else {
                continue;
            };
            for replacement in group {
                if replacement.name == seed.name {
                    continue;
                }
                let (results, _) = worker.verify(&[VerificationItem {
                    level_params: full_seed.level_params.clone(),
                    claim_proposition: seed.proposition.clone(),
                    candidate_proposition: replacement.proposition.clone(),
                    proof_term: replacement.proof_term.clone(),
                    allowed_axioms: seed.axioms.clone(),
                }])?;
                let result = &results[0];
                if result.accepted {
                    let seed_nodes = full_seed.proof_term.node_count();
                    let replacement_nodes = replacement.proof_term.node_count();
                    if replacement_nodes < seed_nodes {
                        collapses.push(Collapse {
                            seed: seed.name.to_string(),
                            replacement: replacement.name.to_string(),
                            seed_proof_nodes: seed_nodes,
                            replacement_nodes,
                            nodes_removed: seed_nodes - replacement_nodes,
                            seed_axioms: seed.axioms.iter().map(ToString::to_string).collect(),
                            replacement_axioms: result
                                .axioms
                                .iter()
                                .map(ToString::to_string)
                                .collect(),
                        });
                    }
                }
            }
        }
    }
    collapses.sort_by_key(|collapse| std::cmp::Reverse(collapse.nodes_removed));
    let fetch_and_verify_wall_ms = verify_started.elapsed().as_millis();

    let host = environment()?;
    let mut report = Report {
        schema: SCHEMA,
        status: "exploratory",
        mathlib_commit: worker.environment().mathlib_commit.clone(),
        lean_toolchain: worker.environment().lean_toolchain.clone(),
        lean_commit: worker.environment().lean_commit.clone(),
        offset: arguments.offset,
        requested_declarations: arguments.count,
        indexed_declarations: fingerprints.len(),
        total_declarations: total,
        exact_statement_groups: duplicate_groups.len(),
        verified_collapses: collapses.len(),
        validation_and_cold_start_wall_ms,
        fingerprint_scan_wall_ms,
        fetch_and_verify_wall_ms,
        host,
        collapses,
        content_sha256: String::new(),
    };
    report.content_sha256 = hash_json(&report)?;
    if let Some(parent) = arguments.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&arguments.output, serde_json::to_vec_pretty(&report)?)?;
    println!("{}", serde_json::to_string(&report)?);
    Ok(())
}

fn parse(arguments: &[String]) -> Result<Arguments, AnyError> {
    let mut lake = None;
    let mut mathlib = None;
    let mut output = None;
    let mut offset = 0;
    let mut count = 512;
    let mut index = 0;
    while index < arguments.len() {
        let flag = &arguments[index];
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--lake" => lake = Some(PathBuf::from(value)),
            "--mathlib" => mathlib = Some(PathBuf::from(value)),
            "--output" => output = Some(PathBuf::from(value)),
            "--offset" => offset = value.parse()?,
            "--count" => count = value.parse()?,
            _ => return Err(format!("unknown lean-development argument: {flag}").into()),
        }
        index += 2;
    }
    Ok(Arguments {
        lake: lake.ok_or("lean-development requires --lake PATH")?,
        mathlib: mathlib.ok_or("lean-development requires --mathlib PATH")?,
        output: output.ok_or("lean-development requires --output PATH")?,
        offset,
        count,
    })
}
