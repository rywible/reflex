#![forbid(unsafe_code)]

use clap::{Parser, Subcommand};
use reflex_bench::HostCalibration;
use reflex_types::{Digest, ExperimentId};
use std::path::PathBuf;
use std::str::FromStr;

mod config;
mod local_experiment;

#[path = "../../version.rs"]
mod version;

#[derive(Parser)]
#[command(
    name = "reflex",
    version = version::VERSION_LINE,
    about = "Verified self-improving search, learning, and discovery"
)]
struct Cli {
    #[arg(long, default_value = "text")]
    format: String,

    #[arg(long, default_value_t = false)]
    dry_run: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    Init,
    Doctor,
    Domain {
        #[command(subcommand)]
        sub: DomainCommands,
    },
    Experiment {
        #[command(subcommand)]
        sub: ExperimentCommands,
    },
    Cell {
        #[command(subcommand)]
        sub: CellCommands,
    },
    Artifact {
        #[command(subcommand)]
        sub: ArtifactCommands,
    },
    Dataset {
        #[command(subcommand)]
        sub: DatasetCommands,
    },
    Train {
        #[arg(long, default_value = "config/train.toml")]
        config: String,
    },
    Evaluate {
        #[arg(long)]
        checkpoint: String,
    },
    Model {
        #[command(subcommand)]
        sub: ModelCommands,
    },
    Knowledge {
        #[command(subcommand)]
        sub: KnowledgeCommands,
    },
    Report {
        #[arg(long)]
        experiment: String,
    },
    Bench {
        #[command(subcommand)]
        sub: Option<BenchCommands>,
    },
}

#[derive(Subcommand)]
enum BenchCommands {
    Model {
        #[arg(long, default_value_t = 25)]
        iterations: usize,
        #[arg(long, default_value_t = 1)]
        threads: usize,
    },
}

#[derive(Subcommand)]
enum DomainCommands {
    New { name: String },
}

#[derive(Subcommand)]
enum CellCommands {
    Replay {
        #[arg(long)]
        id: String,
    },
}

#[derive(Subcommand)]
enum ArtifactCommands {
    Inspect {
        #[arg(long)]
        digest: String,
    },
}

#[derive(Subcommand)]
enum DatasetCommands {
    Compile {
        #[arg(long, default_value = "config/dataset.toml")]
        config: String,
    },
}

#[derive(Subcommand)]
enum ModelCommands {
    Promote {
        #[arg(long)]
        candidate: String,
    },
}

#[derive(Subcommand)]
enum KnowledgeCommands {
    Build {
        #[arg(long, default_value = "config/knowledge.toml")]
        config: String,
    },
}

#[derive(Subcommand)]
enum ExperimentCommands {
    Plan {
        #[arg(long, default_value = "config/experiment.toml")]
        config: String,
    },
    Run {
        #[arg(long, default_value = "config/bitvec-tutorial.toml")]
        config: String,
    },
    Status {
        #[arg(long)]
        id: String,
        #[arg(long)]
        after: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
    },
    Stop {
        #[arg(long)]
        id: String,
    },
    Resume {
        #[arg(long)]
        id: String,
    },
}

fn unavailable(operation: &str, reason: impl std::fmt::Display) -> Box<dyn std::error::Error> {
    format!("{operation}Unavailable: {reason}; refusing to invent state or evidence").into()
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            if cli.dry_run {
                println!("Would initialize the local .reflex workspace.");
                return Ok(());
            }
            let reflex_dir = PathBuf::from(".reflex");
            std::fs::create_dir_all(reflex_dir.join("evidence"))?;
            std::fs::create_dir_all(reflex_dir.join("scratch"))?;
            let config_content = r#"schema = "reflex.config.v1"
resource_class = "local-reference"
execution = "single-process-memory-primary"
arena_total_bytes = 51539607552
evidence_root = ".reflex/evidence"
"#;
            std::fs::write(reflex_dir.join("config.toml"), config_content)?;

            if cli.format == "json" {
                println!(
                    r#"{{"status":"initialized","dir":".reflex","execution":"single-process-memory-primary","arena_bytes":51539607552,"evidence":".reflex/evidence"}}"#
                );
            } else {
                println!("Initialized .reflex for a 48 GiB in-memory artifact arena.");
                println!("Atomic scientific evidence bundles commit under .reflex/evidence.");
            }
        }
        Commands::Doctor => {
            let calibration = HostCalibration::calibrate_current_host();
            let evidence = if PathBuf::from(".reflex/evidence").exists() {
                "ready"
            } else {
                "not initialized"
            };
            if cli.format == "json" {
                println!(
                    r#"{{"status":"healthy","host_class":"{}","cpus":{},"mops":{},"host_fingerprint":"{}","execution":"single-process-memory-primary","arena_bytes":51539607552,"evidence_bundles":"{}"}}"#,
                    calibration.host_class,
                    calibration.cpus,
                    calibration.single_core_score_mops,
                    calibration.host_fingerprint.to_hex(),
                    evidence
                );
            } else {
                println!("Reflex Doctor Status: Healthy");
                println!("Host Class: {}", calibration.host_class);
                println!("CPUs: {}", calibration.cpus);
                println!(
                    "Performance Score: {:.2} MOPS",
                    calibration.single_core_score_mops
                );
                println!(
                    "Host Fingerprint: {}",
                    calibration.host_fingerprint.to_hex()
                );
                println!("Execution: single-process, memory-primary (48 GiB bounded arena)");
                println!("Evidence Bundles: {evidence}");
            }
        }
        Commands::Domain { sub } => match sub {
            DomainCommands::New { name } => {
                if name.is_empty()
                    || !name.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                    })
                {
                    return Err(
                        "domain name must contain only lowercase ASCII, digits, and hyphens".into(),
                    );
                }
                let domain_dir = PathBuf::from(format!("domains/reflex-domain-{name}"));
                if domain_dir.exists() {
                    return Err(
                        format!("domain path already exists: {}", domain_dir.display()).into(),
                    );
                }
                if cli.dry_run {
                    println!("Would create {}.", domain_dir.display());
                    return Ok(());
                }
                std::fs::create_dir_all(domain_dir.join("src"))?;
                let cargo_toml = format!(
                    r#"[package]
name = "reflex-domain-{name}"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true

[dependencies]
reflex-domain.workspace = true
reflex-types.workspace = true
reflex-canonical.workspace = true
reflex-economics.workspace = true
serde.workspace = true

[lints]
workspace = true
"#
                );
                std::fs::write(domain_dir.join("Cargo.toml"), cargo_toml)?;
                std::fs::write(
                    domain_dir.join("src/lib.rs"),
                    include_str!("../../../crates/reflex-domain/examples/template_domain.rs"),
                )?;
                println!("Created domain template at {}.", domain_dir.display());
            }
        },
        Commands::Experiment { sub } => match sub {
            ExperimentCommands::Status { id, after, limit } => {
                if limit == 0 || limit >= 10_000 {
                    return Err("status limit must be in 1..=9999".into());
                }
                let experiment_id = ExperimentId::from_str(&id)?;
                let status = local_experiment::status(
                    PathBuf::from(".reflex").as_path(),
                    experiment_id,
                    after.as_deref(),
                    limit,
                )?;
                if cli.format == "json" {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                } else {
                    println!(
                        "{}: stage={}, completed={}/{} generations",
                        status.experiment_id,
                        status.stage,
                        status.completed_generations,
                        status.configured_generations
                    );
                    for generation in &status.generations {
                        println!(
                            "generation {}: {} ({})",
                            generation.ordinal, generation.state, generation.generation_id
                        );
                    }
                    println!("Current evidence bundle: {}", status.evidence_bundle);
                    if let Some(cursor) = &status.next_cursor {
                        println!("Next page: --after {cursor}");
                    }
                }
            }
            ExperimentCommands::Plan { config } => {
                let plan = config::load_experiment_manifest(&config)?;
                let digest = plan.manifest_digest()?;
                if cli.format == "json" {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "schema": "reflex.cli.experiment-plan.v1",
                            "manifest_digest": digest,
                            "manifest": plan,
                            "durable_mutation": false
                        }))?
                    );
                } else {
                    println!("Experiment manifest: {}", digest.to_hex());
                    println!("Mode: {:?}", plan.mode);
                    println!("Seeds: {}", plan.seeds.len());
                    println!("Dry plan only; no durable state was mutated.");
                }
            }
            ExperimentCommands::Run { config } => {
                if cli.dry_run {
                    let plan = config::load_experiment_manifest(&config)?;
                    let digest = plan.manifest_digest()?;
                    if cli.format == "json" {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "schema": "reflex.cli.experiment-dry-run.v1",
                                "manifest_digest": digest,
                                "would_mutate": false,
                                "would_create_experiment": true
                            }))?
                        );
                    } else {
                        println!("Would run experiment manifest {}.", digest.to_hex());
                        println!(
                            "Dry-run: no arena was allocated and no evidence bundle was written."
                        );
                    }
                    return Ok(());
                }
                let config = config::load_local_run_config(&config)?;
                let outcome =
                    local_experiment::run(PathBuf::from(".reflex").as_path(), config).await?;
                if cli.format == "json" {
                    println!("{}", serde_json::to_string_pretty(&outcome)?);
                } else {
                    println!("Experiment: {}", outcome.experiment_id);
                    println!("Completed generations: {}", outcome.generations.len());
                    for generation in &outcome.generations {
                        println!(
                            "Generation {}: {}; held-out promotion rejected ({})",
                            generation.ordinal, generation.state, generation.promotion_blocker
                        );
                    }
                    println!("Current evidence bundle: {}", outcome.evidence_bundle);
                }
            }
            ExperimentCommands::Stop { id } => {
                return Err(unavailable(
                    "ExperimentStop",
                    format!(
                        "experiment `{id}` has no cross-process daemon; interrupt the foreground process and resume from CURRENT"
                    ),
                ));
            }
            ExperimentCommands::Resume { id } => {
                let experiment_id = ExperimentId::from_str(&id)?;
                let status =
                    local_experiment::resume(PathBuf::from(".reflex").as_path(), experiment_id)
                        .await?;
                if cli.format == "json" {
                    println!("{}", serde_json::to_string_pretty(&status)?);
                } else {
                    println!(
                        "{}; generation {} ended {}.",
                        status.action,
                        status
                            .outcome
                            .generations
                            .last()
                            .map(|generation| generation.ordinal)
                            .unwrap_or(0),
                        status
                            .outcome
                            .generations
                            .last()
                            .map(|generation| generation.state.as_str())
                            .unwrap_or("no generations")
                    );
                }
            }
        },
        Commands::Cell {
            sub: CellCommands::Replay { id },
        } => {
            return Err(unavailable(
                "CellReplay",
                format!(
                    "cell `{id}` has not been loaded from immutable ledger and evidence-bundle inputs"
                ),
            ));
        }
        Commands::Artifact {
            sub: ArtifactCommands::Inspect { digest },
        } => {
            let digest = Digest::from_str(&digest)?;
            let metadata =
                local_experiment::inspect_bundle(PathBuf::from(".reflex").as_path(), digest)?;
            if cli.format == "json" {
                println!("{}", serde_json::to_string_pretty(&metadata)?);
            } else {
                println!(
                    "Evidence bundle {}: generation={}, size={} bytes, artifacts={}",
                    metadata.bundle_digest.to_hex(),
                    metadata.archive_generation,
                    metadata.total_bytes,
                    metadata.artifacts.len()
                );
                for artifact in &metadata.artifacts {
                    println!(
                        "{}: {} bytes ({})",
                        artifact.name, artifact.length, artifact.digest
                    );
                }
            }
        }
        Commands::Dataset {
            sub: DatasetCommands::Compile { config },
        } => {
            return Err(unavailable(
                "DatasetCompile",
                format!("`{config}` is not wired to validated ledger segments"),
            ));
        }
        Commands::Train { config } => {
            return Err(unavailable(
                "Training",
                format!("`{config}` is not wired to a verified dataset manifest"),
            ));
        }
        Commands::Evaluate { checkpoint } => {
            return Err(unavailable(
                "Evaluation",
                format!("checkpoint `{checkpoint}` has not been loaded and digest-verified"),
            ));
        }
        Commands::Model {
            sub: ModelCommands::Promote { candidate },
        } => {
            return Err(unavailable(
                "ModelPromotion",
                format!(
                    "candidate `{candidate}` lacks explicit evaluation and benchmark evidence arguments"
                ),
            ));
        }
        Commands::Knowledge {
            sub: KnowledgeCommands::Build { config },
        } => {
            return Err(unavailable(
                "KnowledgeBuild",
                format!("`{config}` requires externally accepted proof/certificate receipts"),
            ));
        }
        Commands::Report { experiment } => {
            let experiment_id = ExperimentId::from_str(&experiment)?;
            let report = local_experiment::status(
                PathBuf::from(".reflex").as_path(),
                experiment_id,
                None,
                9_999,
            )?;
            if cli.format == "json" {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("Experiment: {}", report.experiment_id);
                println!("Manifest: {}", report.resolved_manifest);
                println!("Evidence bundle: {}", report.evidence_bundle);
                for generation in &report.generations {
                    println!(
                        "Generation {}: {}; promotion eligible: {}; evaluation: {}",
                        generation.ordinal,
                        generation.state,
                        generation.promotion_eligible,
                        generation.evaluation_artifact
                    );
                }
            }
        }
        Commands::Bench { sub } => match sub {
            None => {
                let calibration = HostCalibration::calibrate_current_host();
                println!(
                    "Single-core Score: {:.2} MOPS (host: {}, CPUs: {})",
                    calibration.single_core_score_mops, calibration.host_class, calibration.cpus
                );
            }
            Some(BenchCommands::Model {
                iterations,
                threads,
            }) => {
                let report = reflex_bench::run_model_backend_sweep(iterations, threads)?;
                if cli.format == "json" {
                    println!("{}", serde_json::to_string_pretty(&report)?);
                } else {
                    println!(
                        "Model backend: {} ({:.0} ns aggregate p95, evidence {})",
                        report.recommendation.backend,
                        report.recommendation.total_p95_ns,
                        report.evidence_digest
                    );
                    for unavailable in &report.unavailable {
                        println!(
                            "Unavailable: {} ({})",
                            unavailable.backend, unavailable.reason
                        );
                    }
                }
            }
        },
    }

    Ok(())
}
