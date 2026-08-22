use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use reflex_lean::ast::LeanExpr;
use reflex_lean::catalog::LeanCatalog;
use reflex_lean::worker::{IndexedTheorem, LeanWorker, LeanWorkerConfig, VerificationItem};
use serde::Serialize;

use crate::harness::{AnyError, environment, hash_json, require_absent};

const SCHEMA: &str = "reflex-lean-development-v1";

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
    cold_start_and_scan_wall_ms: u128,
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
        "catalog_entries={} content_sha256={} wall_ms={}",
        catalog.entries().len(),
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
        "catalog_entries={} content_sha256={} load_ms={}",
        catalog.entries().len(),
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
    for fingerprint in &fingerprints {
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
                    proposition: seed.proposition.clone(),
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
        cold_start_and_scan_wall_ms: started.elapsed().as_millis(),
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
