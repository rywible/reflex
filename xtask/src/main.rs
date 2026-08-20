use clap::{Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

mod acceptance;
mod bench;
mod check;
mod dependency_policy;
mod docs;
mod sbom;
mod schema;
mod util;
mod waivers;

#[derive(Parser)]
#[command(
    name = "xtask",
    about = "Reflex framework verification, benchmarking, and supply-chain tooling"
)]
struct XtaskCli {
    #[command(subcommand)]
    command: XtaskCommand,
}

#[derive(Subcommand)]
enum XtaskCommand {
    /// Evaluate every v1 release gate without manufacturing missing evidence.
    Acceptance {
        /// Persist the evaluator result under evidence/v1/acceptance.json.
        #[arg(long)]
        write: bool,
        /// Override the acceptance report output path (requires --write).
        #[arg(long, requires = "write")]
        output: Option<String>,
    },
    /// Fast verification lane: fmt, clippy, tests, docs, SBOM, policy.
    Check {
        /// Print the commands the fast lane runs, without executing them.
        #[arg(long)]
        list_commands: bool,
        /// Hermetic self-test of the reporter, normalizer, and bench gates.
        #[arg(long)]
        self_test: bool,
    },
    /// Deep verification lane: fast lane + loom/turmoil/fuzz/recovery + timing baseline.
    CheckDeep,
    /// Validate invariants, ADRs, and schema references.
    DocsValidate {
        /// Hermetic self-test against synthetic fixture trees.
        #[arg(long)]
        self_test: bool,
    },
    /// Regenerate or verify the CycloneDX software bill of materials.
    Sbom {
        /// Print the SBOM to stdout instead of (or in addition to) a file.
        #[arg(long)]
        stdout: bool,
        /// Write the SBOM to this path (default: evidence/sbom/cyclonedx.json).
        #[arg(long)]
        output: Option<String>,
        /// Regenerate and byte-compare against the stored SBOM.
        #[arg(long)]
        verify: bool,
    },
    /// Benchmark-gate waivers and deny exception registry.
    Waivers {
        #[command(subcommand)]
        action: Option<WaiverAction>,
        /// Validate deny-exceptions.json fields and registry coverage.
        #[arg(long)]
        self_test: bool,
    },
    /// Supply-chain policy checks against deny.toml.
    DependencyPolicy {
        /// Hermetic self-test with injected policy violations.
        #[arg(long)]
        self_test: bool,
    },
    /// Regenerate or verify committed schema fingerprints (sql/, proto/).
    SchemaFingerprint {
        /// Write evidence/schema/fingerprint.json from the live tree.
        #[arg(long)]
        write: bool,
    },
}

#[derive(Subcommand)]
enum WaiverAction {
    /// List benchmark waivers with validity.
    List,
}

fn main() -> ExitCode {
    let cli = XtaskCli::parse();
    match dispatch(cli.command) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn dispatch(command: XtaskCommand) -> Result<ExitCode, util::XtaskError> {
    match command {
        XtaskCommand::Acceptance { write, output } => {
            let report = acceptance::evaluate()?;
            if write {
                acceptance::write(&report, output.map(PathBuf::from))?;
            }
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(if report.release_ready {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        XtaskCommand::Check {
            list_commands,
            self_test,
        } => {
            if list_commands {
                for c in check::list_commands(false) {
                    println!("{c}");
                }
                return Ok(ExitCode::SUCCESS);
            }
            if self_test {
                let mut results = check::self_test()?;
                results.extend(bench::self_test());
                return report_selftest(results);
            }
            let steps = check::fast_lane()?;
            check::write_reports(&steps, false)?;
            let ok = steps.iter().all(|s| s.ok);
            for s in &steps {
                println!(
                    "  [{:>4}] {} ({} ms) {}",
                    if s.ok { "ok" } else { "FAIL" },
                    s.name,
                    s.duration_ms,
                    if s.ok { "" } else { &s.detail }
                );
            }
            println!("==> check lane {}", if ok { "PASSED" } else { "FAILED" });
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        XtaskCommand::CheckDeep => {
            let steps = check::deep_lane()?;
            check::write_reports(&steps, true)?;
            let ok = steps.iter().all(|s| s.ok);
            for s in &steps {
                println!(
                    "  [{:>4}] {} ({} ms) {}",
                    if s.ok { "ok" } else { "FAIL" },
                    s.name,
                    s.duration_ms,
                    if s.ok { "" } else { &s.detail }
                );
            }
            println!(
                "==> check-deep lane {}",
                if ok { "PASSED" } else { "FAILED" }
            );
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        XtaskCommand::DocsValidate { self_test } => {
            if self_test {
                return report_selftest(docs::self_test()?);
            }
            let report = docs::validate_docs(
                Path::new(docs::INVARIANTS_PATH),
                Path::new(docs::ADR_DIR),
                Path::new("tests"),
            )?;
            docs::write_report(&report)?;
            println!(
                "==> docs-validate: {} ({} invariants, {} ADRs, report: {})",
                if report.ok { "PASSED" } else { "FAILED" },
                report.invariants.len(),
                report.adrs.len(),
                docs::REPORT_PATH
            );
            for item in &report.items {
                let name = item.get("check").and_then(|c| c.as_str()).unwrap_or("");
                let ok = item.get("ok").and_then(|o| o.as_bool()).unwrap_or(false);
                let detail = item.get("detail").and_then(|d| d.as_str()).unwrap_or("");
                println!(
                    "  [{:>4}] {} {}",
                    if ok { "ok" } else { "FAIL" },
                    name,
                    if ok { "" } else { detail }
                );
            }
            Ok(if report.ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
        XtaskCommand::Sbom {
            stdout,
            output,
            verify,
        } => {
            if verify {
                let ok = sbom::verify_sbom(Path::new("Cargo.lock"))?;
                Ok(if ok {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                })
            } else {
                let out = output
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(sbom::SBOM_PATH));
                sbom::write_sbom(Path::new("Cargo.lock"), &out, stdout)?;
                Ok(ExitCode::SUCCESS)
            }
        }
        XtaskCommand::Waivers { action, self_test } => {
            if self_test {
                let mut results = waivers::self_test()?;
                for item in bench::waivers::self_test() {
                    results.push(item);
                }
                return report_selftest(results);
            }
            match action.unwrap_or(WaiverAction::List) {
                WaiverAction::List => {
                    let listed = bench::waivers::list();
                    if listed.is_empty() {
                        println!("no waivers under waivers/");
                    }
                    for (path, w, valid) in listed {
                        println!(
                            "  {} benchmark={} scope={} owner={} expiry={} {}",
                            path,
                            w.benchmark,
                            w.scope,
                            w.owner,
                            w.expiry_date,
                            if valid { "VALID" } else { "EXPIRED/INVALID" }
                        );
                    }
                    Ok(ExitCode::SUCCESS)
                }
            }
        }
        XtaskCommand::SchemaFingerprint { write } => {
            let root = Path::new(".");
            let out = Path::new(schema::FINGERPRINT_PATH);
            if write {
                schema::write_fingerprint(root, out)?;
                println!("==> schema-fingerprint: wrote {}", out.display());
                Ok(ExitCode::SUCCESS)
            } else {
                let ok = schema::verify_fingerprint(root)?;
                Ok(if ok {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                })
            }
        }
        XtaskCommand::DependencyPolicy { self_test } => {
            if self_test {
                return report_selftest(dependency_policy::self_test()?);
            }
            let ok = dependency_policy::parse_policy();
            let denied = util::run_cmd("cargo", &["deny", "check"]);
            if !denied.success {
                println!("  cargo deny check: FAILED");
            }
            let ok = ok && denied.success;
            println!(
                "==> dependency-policy: {}",
                if ok { "PASSED" } else { "FAILED" }
            );
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            })
        }
    }
}

fn report_selftest<S: Into<String>>(
    results: Vec<(S, bool, String)>,
) -> Result<ExitCode, util::XtaskError> {
    let mut failed = 0;
    for (name, ok, detail) in results {
        let name = name.into();
        if !ok {
            failed += 1;
        }
        println!(
            "  [{}] {} {}",
            if ok { "ok" } else { "FAIL" },
            name,
            if ok { "" } else { &detail }
        );
    }
    if failed == 0 {
        println!("==> self-test: PASSED");
        Ok(ExitCode::SUCCESS)
    } else {
        println!("==> self-test: FAILED ({failed} fixture(s))");
        Ok(ExitCode::FAILURE)
    }
}
