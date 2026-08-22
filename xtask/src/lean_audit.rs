use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

use cpu_time::ProcessTime;
use reflex_lean::catalog::LeanCatalog;
use reflex_lean::temporal::{
    POTENTIAL_HEADS, PotentialHead, TasteModel, TemporalExample, TemporalPair, TemporalSnapshot,
    Treatment,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::harness::{
    AnyError, HostEnvironment, duration_ns, environment, hash_json, parse_flag_values,
    require_absent, require_clean, require_release,
};

const SCHEMA: &str = "reflex-lean-temporal-audit-freeze-v1";
const CUTOFF: &str = "2025-01-01T00:00:00Z";
const CANDIDATES_PER_HEAD: usize = 256;
const CPU_CHECKPOINT_SECONDS: [u64; 4] = [3_600, 14_400, 57_600, 230_400];
const AUDIT_BOUNDARY: &str = "2026-07-01T00:00:00Z";
const BOUNDARY_EVIDENCE_URL: &str = "https://api.github.com/repos/leanprover-community/mathlib4/commits?sha=master&until=2026-06-30T23%3A59%3A59Z&per_page=1";

struct FreezeArguments {
    june_catalog: PathBuf,
    september_catalog: PathBuf,
    december_catalog: PathBuf,
    output: PathBuf,
}

struct LockArguments {
    manifest: PathBuf,
    output: PathBuf,
    mathlib_commit: String,
    lean_toolchain: String,
    lean_toolchain_alias: String,
    lean_version: String,
    lean_commit: String,
}

#[derive(Deserialize)]
struct ManifestIdentity {
    schema: String,
    protocol_sha256: String,
    audit_artifact_set_sha256: String,
    content_sha256: String,
}

#[derive(Serialize)]
struct AuditLock {
    schema: &'static str,
    status: &'static str,
    freeze_manifest_file_sha256: String,
    freeze_manifest_content_sha256: String,
    protocol_sha256: String,
    audit_artifact_set_sha256: String,
    audit_boundary: &'static str,
    mathlib_commit: String,
    mathlib_commit_timestamp: String,
    boundary_evidence_url: &'static str,
    boundary_evidence_sha256: String,
    lean_toolchain: String,
    lean_toolchain_alias: String,
    lean_version: String,
    lean_commit: String,
    checkout_access: &'static str,
    host: HostEnvironment,
    content_sha256: String,
}

#[derive(Serialize)]
struct Protocol {
    schema: &'static str,
    cutoff: &'static str,
    training_pairs: [[&'static str; 2]; 2],
    audit_artifact_snapshot: &'static str,
    audit_artifact_exclusion: &'static str,
    audit_artifacts_per_head: usize,
    heads: [&'static str; POTENTIAL_HEADS],
    treatments: [&'static str; 8],
    cpu_checkpoints_seconds: [u64; 4],
    lanes: usize,
    in_process_lanes: usize,
    verifier_processes: usize,
    resident_bytes: u64,
    wall_seconds: u64,
    statistical_unit: &'static str,
    interval: &'static str,
    bootstrap_resamples: usize,
    kernel_replay_artifacts_per_head: usize,
    relationship_certificate_limit: usize,
    critique_items: usize,
    stopping_rule: &'static str,
    confirmation_rule: &'static str,
    causal_ablation_rule: &'static str,
    time_to_utility_rule: &'static str,
    no_regression_rule: &'static str,
    recovery_rule: &'static str,
    audit_snapshot_rule: &'static str,
    execution_rule: &'static str,
    critique_rule: &'static str,
}

#[derive(Serialize)]
struct PairSummary {
    earlier_environment_sha256: String,
    later_environment_sha256: String,
    examples: usize,
    later_new_declarations: usize,
    relationship_candidates: usize,
}

struct BoundaryEvidence {
    commit_timestamp: String,
    response_sha256: String,
}

#[derive(Deserialize)]
struct GithubCommit {
    sha: String,
    commit: GithubCommitBody,
}

#[derive(Deserialize)]
struct GithubCommitBody {
    committer: GithubCommitter,
}

#[derive(Deserialize)]
struct GithubCommitter {
    date: String,
}

#[derive(Clone, Serialize)]
struct FrozenArtifact {
    declaration: String,
    module: String,
    semantic_family: String,
}

#[derive(Serialize)]
struct FrozenRanking {
    treatment: &'static str,
    model_sha256: Option<String>,
    model_bytes: usize,
    training_wall_ns: u64,
    training_cpu_ns: u64,
    ranking_wall_ns: [u64; POTENTIAL_HEADS],
    ranking_cpu_ns: [u64; POTENTIAL_HEADS],
    strategies: [&'static str; POTENTIAL_HEADS],
    heads: [Vec<FrozenArtifact>; POTENTIAL_HEADS],
}

#[derive(Serialize)]
struct FreezeManifest {
    schema: &'static str,
    status: &'static str,
    protocol_sha256: String,
    protocol: Protocol,
    catalog_sha256: [String; 3],
    catalog_durable_bytes: [u64; 3],
    pairs: [PairSummary; 2],
    training_examples: usize,
    audit_artifacts: usize,
    audit_artifact_set_sha256: String,
    rankings: Vec<FrozenRanking>,
    host: HostEnvironment,
    protocol_deviations: Vec<String>,
    content_sha256: String,
}

pub fn freeze(arguments: &[String]) -> Result<(), AnyError> {
    require_release("lean-temporal-audit-freeze")?;
    let host = environment()?;
    require_clean(&host, SCHEMA)?;
    let arguments = parse_freeze(arguments)?;
    require_absent(&arguments.output, "Lean Temporal Audit freeze manifest")?;

    let june_catalog = LeanCatalog::load(&arguments.june_catalog)?;
    let september_catalog = LeanCatalog::load(&arguments.september_catalog)?;
    let december_catalog = LeanCatalog::load(&arguments.december_catalog)?;
    let june = TemporalSnapshot::from_catalog(&june_catalog);
    let september = TemporalSnapshot::from_catalog(&september_catalog);
    let december = TemporalSnapshot::from_catalog(&december_catalog);
    let first = TemporalPair::derive(&june, &september)?;
    let second = TemporalPair::derive(&september, &december)?;
    let mut experience = first.examples.clone();
    experience.extend(second.examples.iter().cloned());
    let seen = experience
        .iter()
        .map(|example| example.semantic_group)
        .collect::<HashSet<_>>();
    let audit_artifacts = december
        .forecast_artifacts()
        .into_iter()
        .filter(|artifact| !seen.contains(&artifact.semantic_group))
        .collect::<Vec<_>>();
    if audit_artifacts.len() < CANDIDATES_PER_HEAD {
        return Err("pre-cutoff semantic-family exclusion leaves too few audit artifacts".into());
    }

    let protocol = frozen_protocol();
    let protocol_sha256 = hash_json(&protocol)?;
    let audit_artifact_set_sha256 = audit_artifact_set_hash(&audit_artifacts);
    let mut rankings = Vec::new();
    for (name, treatment) in [
        ("full", Treatment::Full),
        ("bootstrap", Treatment::Bootstrap),
        ("no-model", Treatment::NoModel),
        ("no-consolidation", Treatment::NoConsolidation),
        ("immediate-only", Treatment::ImmediateOnly),
    ] {
        rankings.push(freeze_treatment(
            name,
            treatment,
            &experience,
            &audit_artifacts,
        )?);
    }
    for baseline in ["uniform", "dependency-light", "historical-reuse"] {
        rankings.push(freeze_baseline(baseline, &audit_artifacts));
    }

    let mut manifest = FreezeManifest {
        schema: SCHEMA,
        status: "frozen-before-audit-exposure",
        protocol_sha256,
        protocol,
        catalog_sha256: [
            june_catalog.content_sha256().into(),
            september_catalog.content_sha256().into(),
            december_catalog.content_sha256().into(),
        ],
        catalog_durable_bytes: [
            std::fs::metadata(&arguments.june_catalog)?.len(),
            std::fs::metadata(&arguments.september_catalog)?.len(),
            std::fs::metadata(&arguments.december_catalog)?.len(),
        ],
        pairs: [pair_summary(&first), pair_summary(&second)],
        training_examples: experience.len(),
        audit_artifacts: audit_artifacts.len(),
        audit_artifact_set_sha256,
        rankings,
        host,
        protocol_deviations: Vec::new(),
        content_sha256: String::new(),
    };
    manifest.content_sha256 = hash_json(&manifest)?;
    if let Some(parent) = arguments.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&arguments.output, serde_json::to_vec_pretty(&manifest)?)?;
    println!(
        "protocol_sha256={} content_sha256={} audit_artifact_set_sha256={} audit_artifacts={} durable_bytes={}",
        manifest.protocol_sha256,
        manifest.content_sha256,
        manifest.audit_artifact_set_sha256,
        manifest.audit_artifacts,
        std::fs::metadata(&arguments.output)?.len()
    );
    Ok(())
}

pub fn lock(arguments: &[String]) -> Result<(), AnyError> {
    require_release("lean-temporal-audit-lock")?;
    let host = environment()?;
    require_clean(&host, "reflex-lean-temporal-audit-lock-v1")?;
    let arguments = parse_lock(arguments)?;
    require_absent(&arguments.output, "Lean Temporal Audit lock")?;
    let manifest_bytes = std::fs::read(&arguments.manifest)?;
    let identity: ManifestIdentity = serde_json::from_slice(&manifest_bytes)?;
    if identity.schema != SCHEMA {
        return Err("Lean Temporal Audit freeze manifest schema differs".into());
    }
    validate_commit(&arguments.mathlib_commit)?;
    validate_commit(&arguments.lean_commit)?;
    let boundary = fetch_audit_boundary(&arguments.mathlib_commit)?;
    if arguments.lean_toolchain.trim().is_empty()
        || arguments.lean_toolchain_alias.trim().is_empty()
        || arguments.lean_version.trim().is_empty()
    {
        return Err("Lean Temporal Audit toolchain pin is incomplete".into());
    }
    let mut lock = AuditLock {
        schema: "reflex-lean-temporal-audit-lock-v1",
        status: "locked-before-audit-checkout",
        freeze_manifest_file_sha256: crate::harness::hash_file(&arguments.manifest)?,
        freeze_manifest_content_sha256: identity.content_sha256,
        protocol_sha256: identity.protocol_sha256,
        audit_artifact_set_sha256: identity.audit_artifact_set_sha256,
        audit_boundary: "strictly before 2026-07-01T00:00:00Z",
        mathlib_commit: arguments.mathlib_commit,
        mathlib_commit_timestamp: boundary.commit_timestamp,
        boundary_evidence_url: BOUNDARY_EVIDENCE_URL,
        boundary_evidence_sha256: boundary.response_sha256,
        lean_toolchain: arguments.lean_toolchain,
        lean_toolchain_alias: arguments.lean_toolchain_alias,
        lean_version: arguments.lean_version,
        lean_commit: arguments.lean_commit,
        checkout_access: "forbidden until this lock and its code revision are committed",
        host,
        content_sha256: String::new(),
    };
    lock.content_sha256 = hash_json(&lock)?;
    if let Some(parent) = arguments.output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&arguments.output, serde_json::to_vec_pretty(&lock)?)?;
    println!(
        "content_sha256={} audit_mathlib_commit={} freeze_manifest_file_sha256={}",
        lock.content_sha256, lock.mathlib_commit, lock.freeze_manifest_file_sha256
    );
    Ok(())
}

fn frozen_protocol() -> Protocol {
    Protocol {
        schema: SCHEMA,
        cutoff: CUTOFF,
        training_pairs: [["2024-06-30", "2024-09-30"], ["2024-09-30", "2024-12-31"]],
        audit_artifact_snapshot: "2024-12-31",
        audit_artifact_exclusion: "exclude every semantic family observed in either training pair",
        audit_artifacts_per_head: CANDIDATES_PER_HEAD,
        heads: PotentialHead::ALL.map(PotentialHead::name),
        treatments: [
            "full",
            "bootstrap",
            "no-model",
            "no-consolidation",
            "immediate-only",
            "uniform",
            "dependency-light",
            "historical-reuse",
        ],
        cpu_checkpoints_seconds: CPU_CHECKPOINT_SECONDS,
        lanes: 8,
        in_process_lanes: 6,
        verifier_processes: 2,
        resident_bytes: 48 * 1024 * 1024 * 1024,
        wall_seconds: 24 * 60 * 60,
        statistical_unit: "semantic theorem family nested in exact Lean source module; paired by frozen family identity",
        interval: "module-clustered paired percentile bootstrap, simultaneous one-sided 99% lower bounds",
        bootstrap_resamples: 10_000,
        kernel_replay_artifacts_per_head: 16,
        relationship_certificate_limit: 64,
        critique_items: 32,
        stopping_rule: "carry the final anytime value forward after all 256 frozen artifacts for a head are exhausted; never burn CPU to fill a checkpoint",
        confirmation_rule: "Full weakly improves every head versus the virtual-best baseline and strictly improves at least one",
        causal_ablation_rule: "Full weakly improves every head and strictly improves at least one head versus each registered learned-system ablation",
        time_to_utility_rule: "for each head use the lower of Full and virtual-best final directional utility over 16 kernel-replayed artifacts as the common target; CPU includes frozen training, ranking, fetch, and kernel replay; require geometric-mean baseline/Full time at least 10x with simultaneous 99% lower bound above 3x",
        no_regression_rule: "zero regression in kernel migration, protected elegance measurements, recovery, or resource limits",
        recovery_rule: "cold reconstruction reproduces every mechanical decision and kernel certificate hash",
        audit_snapshot_rule: "latest mathlib commit with commit timestamp strictly before 2026-07-01T00:00:00Z; exact commit and Lean pin locked before checkout",
        execution_rule: "one valid execution; every failure and deviation is retained; no favorable rerun",
        critique_rule: "mechanical report is sealed before treatment-blinded mathematical critique; critique cannot alter the mechanical decision",
    }
}

fn freeze_treatment(
    name: &'static str,
    treatment: Treatment,
    experience: &[TemporalExample],
    artifacts: &[TemporalExample],
) -> Result<FrozenRanking, AnyError> {
    let training_cpu = ProcessTime::now();
    let training_started = Instant::now();
    let model = TasteModel::train(experience, treatment);
    let training_wall_ns = duration_ns(training_started.elapsed());
    let training_cpu_ns = duration_ns(training_cpu.elapsed());
    let mut ranking_wall_ns = [0; POTENTIAL_HEADS];
    let mut ranking_cpu_ns = [0; POTENTIAL_HEADS];
    let heads = std::array::from_fn(|index| {
        let head = PotentialHead::ALL[index];
        let ranking_cpu = ProcessTime::now();
        let ranking_started = Instant::now();
        let indexes = if treatment == Treatment::NoModel {
            baseline_indexes(artifacts, "uniform")
        } else {
            model.rank_for_head(artifacts, CANDIDATES_PER_HEAD, head)
        };
        let frozen = freeze_artifacts(artifacts, &indexes);
        ranking_wall_ns[index] = duration_ns(ranking_started.elapsed());
        ranking_cpu_ns[index] = duration_ns(ranking_cpu.elapsed());
        frozen
    });
    let encoded = model.encode()?;
    Ok(FrozenRanking {
        treatment: name,
        model_sha256: Some(model.content_sha256()),
        model_bytes: encoded.len(),
        training_wall_ns,
        training_cpu_ns,
        ranking_wall_ns,
        ranking_cpu_ns,
        strategies: if treatment == Treatment::NoModel {
            ["uniform"; POTENTIAL_HEADS]
        } else {
            model.strategy_names()
        },
        heads,
    })
}

fn freeze_baseline(name: &'static str, artifacts: &[TemporalExample]) -> FrozenRanking {
    let ranking_cpu = ProcessTime::now();
    let ranking_started = Instant::now();
    let indexes = baseline_indexes(artifacts, name);
    let frozen = freeze_artifacts(artifacts, &indexes);
    FrozenRanking {
        treatment: name,
        model_sha256: None,
        model_bytes: 0,
        training_wall_ns: 0,
        training_cpu_ns: 0,
        ranking_wall_ns: [duration_ns(ranking_started.elapsed()); POTENTIAL_HEADS],
        ranking_cpu_ns: [duration_ns(ranking_cpu.elapsed()); POTENTIAL_HEADS],
        strategies: [name; POTENTIAL_HEADS],
        heads: std::array::from_fn(|_| frozen.clone()),
    }
}

fn baseline_indexes(examples: &[TemporalExample], name: &str) -> Vec<usize> {
    let mut ranked = (0..examples.len()).collect::<Vec<_>>();
    match name {
        "uniform" => ranked.sort_unstable_by_key(|index| examples[*index].semantic_group),
        "dependency-light" => ranked.sort_unstable_by(|left, right| {
            examples[*left].features[1]
                .total_cmp(&examples[*right].features[1])
                .then_with(|| left.cmp(right))
        }),
        "historical-reuse" => ranked.sort_unstable_by(|left, right| {
            examples[*right].features[2]
                .total_cmp(&examples[*left].features[2])
                .then_with(|| left.cmp(right))
        }),
        _ => unreachable!("frozen baseline list is exhaustive"),
    }
    ranked.truncate(CANDIDATES_PER_HEAD);
    ranked
}

fn freeze_artifacts(examples: &[TemporalExample], indexes: &[usize]) -> Vec<FrozenArtifact> {
    indexes
        .iter()
        .map(|index| {
            let example = &examples[*index];
            FrozenArtifact {
                declaration: example.declaration.to_string(),
                module: example.module.to_string(),
                semantic_family: hex(&example.semantic_group),
            }
        })
        .collect()
}

fn audit_artifact_set_hash(artifacts: &[TemporalExample]) -> String {
    let mut digest = Sha256::new();
    digest.update(b"reflex-lean-audit-artifacts-v1\0");
    for artifact in artifacts {
        digest.update(artifact.declaration.to_string().as_bytes());
        digest.update([0]);
        digest.update(artifact.module.to_string().as_bytes());
        digest.update([0]);
        digest.update(artifact.semantic_group);
    }
    hex(&digest.finalize())
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

fn parse_freeze(arguments: &[String]) -> Result<FreezeArguments, AnyError> {
    let values = parse_flag_values(
        arguments,
        &[
            "--june-catalog",
            "--september-catalog",
            "--december-catalog",
            "--output",
        ],
        "lean-temporal-audit-freeze",
    )?;
    let path = |flag| -> Result<PathBuf, AnyError> {
        values
            .get(flag)
            .map(|value| PathBuf::from(*value))
            .ok_or_else(|| format!("lean-temporal-audit-freeze requires {flag}").into())
    };
    Ok(FreezeArguments {
        june_catalog: path("--june-catalog")?,
        september_catalog: path("--september-catalog")?,
        december_catalog: path("--december-catalog")?,
        output: path("--output")?,
    })
}

fn parse_lock(arguments: &[String]) -> Result<LockArguments, AnyError> {
    let values = parse_flag_values(
        arguments,
        &[
            "--manifest",
            "--output",
            "--mathlib-commit",
            "--lean-toolchain",
            "--lean-toolchain-alias",
            "--lean-version",
            "--lean-commit",
        ],
        "lean-temporal-audit-lock",
    )?;
    let value = |flag| -> Result<String, AnyError> {
        values
            .get(flag)
            .map(|value| (*value).to_owned())
            .ok_or_else(|| format!("lean-temporal-audit-lock requires {flag}").into())
    };
    Ok(LockArguments {
        manifest: PathBuf::from(value("--manifest")?),
        output: PathBuf::from(value("--output")?),
        mathlib_commit: value("--mathlib-commit")?,
        lean_toolchain: value("--lean-toolchain")?,
        lean_toolchain_alias: value("--lean-toolchain-alias")?,
        lean_version: value("--lean-version")?,
        lean_commit: value("--lean-commit")?,
    })
}

fn validate_commit(commit: &str) -> Result<(), AnyError> {
    if commit.len() == 40
        && commit
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err("Lean Temporal Audit commits must be exact lowercase SHA-1 values".into())
    }
}

fn fetch_audit_boundary(expected_commit: &str) -> Result<BoundaryEvidence, AnyError> {
    let output = Command::new("curl")
        .args([
            "--fail",
            "--silent",
            "--show-error",
            "--max-time",
            "30",
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "X-GitHub-Api-Version: 2022-11-28",
            BOUNDARY_EVIDENCE_URL,
        ])
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "GitHub boundary evidence request failed with {}",
            output.status
        )
        .into());
    }
    parse_audit_boundary(expected_commit, &output.stdout)
}

fn parse_audit_boundary(
    expected_commit: &str,
    response: &[u8],
) -> Result<BoundaryEvidence, AnyError> {
    let commits: Vec<GithubCommit> = serde_json::from_slice(response)?;
    let [commit] = commits.as_slice() else {
        return Err("GitHub boundary query must return exactly one latest commit".into());
    };
    if commit.sha != expected_commit || commit.commit.committer.date.as_str() >= AUDIT_BOUNDARY {
        return Err(
            "GitHub's latest pre-boundary mathlib commit differs from the requested pin".into(),
        );
    }
    Ok(BoundaryEvidence {
        commit_timestamp: commit.commit.committer.date.clone(),
        response_sha256: hex(&Sha256::digest(response)),
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut output, byte| {
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
        output
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_pins_require_exact_lowercase_commits() {
        assert!(validate_commit("0123456789abcdef0123456789abcdef01234567").is_ok());
        assert!(validate_commit("0123456789ABCDEF0123456789ABCDEF01234567").is_err());
        assert!(validate_commit("0123").is_err());
    }

    #[test]
    fn frozen_protocol_keeps_all_seven_heads_and_eight_treatments() {
        let protocol = frozen_protocol();
        assert_eq!(protocol.heads.len(), POTENTIAL_HEADS);
        assert_eq!(protocol.treatments.len(), 8);
        assert_eq!(protocol.cpu_checkpoints_seconds, CPU_CHECKPOINT_SECONDS);
        assert_eq!(protocol.lanes, 8);
        assert_eq!(protocol.in_process_lanes + protocol.verifier_processes, 8);
    }

    #[test]
    fn audit_boundary_is_parsed_from_primary_github_metadata() {
        let commit = "0123456789abcdef0123456789abcdef01234567";
        let response = format!(
            r#"[{{"sha":"{commit}","commit":{{"committer":{{"date":"2026-06-30T23:59:59Z"}}}}}}]"#
        );
        let evidence = parse_audit_boundary(commit, response.as_bytes())
            .expect("matching primary evidence is accepted");
        assert_eq!(evidence.commit_timestamp, "2026-06-30T23:59:59Z");
        assert!(parse_audit_boundary("bad", response.as_bytes()).is_err());

        let late = response.replace("2026-06-30T23:59:59Z", AUDIT_BOUNDARY);
        assert!(parse_audit_boundary(commit, late.as_bytes()).is_err());
    }
}
