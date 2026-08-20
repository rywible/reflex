use crate::sbom;
use crate::schema;
use crate::util::{self, Result};
use reflex_bench::{BenchmarkRecord, HostCalibration, PerformanceBudgetRegistry};
use serde_json::{Value, json};
use std::path::Path;
use std::time::Instant;

pub const CHECK_REPORT_DIR: &str = "evidence/checks";

pub struct LaneStep {
    pub name: String,
    pub ok: bool,
    pub skipped: bool,
    pub duration_ms: u64,
    pub detail: String,
}

pub fn fast_lane() -> Result<Vec<LaneStep>> {
    run_lane(false)
}

pub fn deep_lane() -> Result<Vec<LaneStep>> {
    run_lane(true)
}

fn run_lane(deep: bool) -> Result<Vec<LaneStep>> {
    let mut steps = vec![
        run_step("toolchain", toolchain_check),
        run_step("fmt", fmt_check),
        run_step("clippy", clippy_check),
        run_step("tests", test_check),
        run_step("doctests", doctest_check),
        run_step("unsafe-allowlist", unsafe_check),
        run_step("schema-fingerprint", schema_fingerprint_check),
        run_step("doc-links", doc_links_check),
        run_step("docs-validate", docs_check),
        run_step("sbom-verify", sbom_check),
        run_step("dependency-policy", deny_check),
    ];
    if deep {
        steps.extend([
            run_step("loom", || deep_suite("loom")),
            run_step("turmoil", || deep_suite("turmoil")),
            run_step("fuzz-smoke", || deep_suite("fuzz")),
            run_step("recovery", || deep_suite("recovery")),
        ]);
        let timing = run_step("deep-timing-baseline", || timing_check(&steps));
        steps.push(timing);
    }
    Ok(steps)
}

fn run_step<F: FnOnce() -> Result<bool>>(name: &str, f: F) -> LaneStep {
    let start = Instant::now();
    match f() {
        Ok(ok) => LaneStep {
            name: name.to_string(),
            ok,
            skipped: false,
            duration_ms: start.elapsed().as_millis() as u64,
            detail: if ok {
                "passed".to_string()
            } else {
                "failed".to_string()
            },
        },
        Err(e) => LaneStep {
            name: name.to_string(),
            ok: false,
            skipped: false,
            duration_ms: start.elapsed().as_millis() as u64,
            detail: format!("error: {e}"),
        },
    }
}

fn toolchain_check() -> Result<bool> {
    let text = std::fs::read_to_string("rust-toolchain.toml")
        .map_err(|e| util::msg(format!("rust-toolchain.toml unreadable: {e}")))?;
    let channel_ok = text.contains("channel = \"1.97.1\"");
    let metadata = util::run_cmd("cargo", &["metadata", "--locked", "--format-version", "1"]);
    if channel_ok && metadata.success {
        println!("  toolchain: channel 1.97.1 and Cargo.lock --locked OK");
        Ok(true)
    } else {
        if !channel_ok {
            println!("  toolchain: rust-toolchain.toml channel is not 1.97.1");
        }
        if !metadata.success {
            println!("  toolchain: cargo metadata --locked failed (stale or unlocked lockfile?)");
        }
        Ok(false)
    }
}

fn fmt_check() -> Result<bool> {
    Ok(util::run_cmd("cargo", &["fmt", "--check"]).success)
}

fn clippy_check() -> Result<bool> {
    Ok(util::run_cmd(
        "cargo",
        &["clippy", "--workspace", "--locked", "--", "-D", "warnings"],
    )
    .success)
}

fn test_check() -> Result<bool> {
    if util::cmd_exists("cargo-nextest") {
        Ok(util::run_cmd("cargo", &["nextest", "run", "--workspace", "--locked"]).success)
    } else {
        Ok(util::run_cmd("cargo", &["test", "--workspace", "--locked"]).success)
    }
}

fn doctest_check() -> Result<bool> {
    Ok(util::run_cmd("cargo", &["test", "--workspace", "--locked", "--doc"]).success)
}

fn unsafe_check() -> Result<bool> {
    let script = Path::new("scripts/check-unsafe.sh");
    if !script.exists() {
        println!("  unsafe-allowlist: scripts/check-unsafe.sh missing");
        return Ok(false);
    }
    Ok(util::run_cmd(
        "bash",
        &[script.to_str().unwrap_or("scripts/check-unsafe.sh")],
    )
    .success)
}

fn schema_fingerprint_check() -> Result<bool> {
    schema::verify_fingerprint(Path::new("."))
}

fn doc_links_check() -> Result<bool> {
    let (ok, broken) = schema::check_doc_links(Path::new("docs"))?;
    if ok {
        println!("  doc-links: all relative markdown links resolve");
        Ok(true)
    } else {
        for b in broken.iter().take(5) {
            println!("  doc-links: broken: {b}");
        }
        if broken.len() > 5 {
            println!("  doc-links: ... and {} more", broken.len() - 5);
        }
        Ok(false)
    }
}

fn docs_check() -> Result<bool> {
    let report = crate::docs::validate_docs(
        Path::new(crate::docs::INVARIANTS_PATH),
        Path::new(crate::docs::ADR_DIR),
        Path::new("tests"),
    )?;
    if report.ok {
        crate::docs::write_report(&report)?;
    } else {
        println!("  docs-validate: validation failed; not updating committed report");
    }
    Ok(report.ok)
}

fn sbom_check() -> Result<bool> {
    sbom::verify_sbom(Path::new("Cargo.lock"))
}

fn deny_check() -> Result<bool> {
    if !util::cmd_exists("cargo-deny") {
        println!("  dependency-policy: cargo-deny is required but not installed");
        return Ok(false);
    }
    let r = util::run_cmd("cargo", &["deny", "check"]);
    let policy_ok = crate::dependency_policy::parse_policy();
    Ok(r.success && policy_ok)
}

fn deep_suite(suite: &str) -> Result<bool> {
    let (args, cfg): (Vec<&str>, Option<&str>) = match suite {
        "loom" => (
            vec![
                "test",
                "-p",
                "reflex-runtime",
                "--locked",
                "--release",
                "--lib",
                "loom_",
            ],
            Some("reflex_loom"),
        ),
        "turmoil" => (
            vec![
                "test",
                "-p",
                "reflex-scheduler",
                "--locked",
                "--lib",
                "turmoil_",
            ],
            Some("turmoil"),
        ),
        "fuzz" => (vec!["test", "-p", "reflex-fuzz", "--locked"], None),
        "recovery" => (
            vec![
                "test",
                "-p",
                "reflex-integration-tests",
                "--locked",
                "--test",
                "recovery_test",
            ],
            None,
        ),
        _ => return Ok(false),
    };
    let r = if let Some(cfg) = cfg {
        let inherited = std::env::var("RUSTFLAGS").unwrap_or_default();
        let rustflags = format!("{inherited} --cfg {cfg}");
        util::run_cmd_with_env("cargo", &args, &[("RUSTFLAGS", rustflags.trim())])
    } else {
        util::run_cmd("cargo", &args)
    };
    if !r.success {
        let combined = format!("{}{}", r.stdout, r.stderr);
        if combined.contains("no test target named")
            || combined.contains("could not find `")
            || combined.contains("unknown filter")
        {
            println!("  deep {suite}: required test target missing (fail closed)");
            return Ok(false);
        }
    }
    Ok(r.success)
}

fn timing_check(steps: &[LaneStep]) -> Result<bool> {
    let host = HostCalibration::calibrate_current_host();
    if !host.is_valid_for_claim() {
        println!(
            "  deep-timing: host calibration is not valid for a performance claim: {}",
            host.noncanonical_reasons.join(", ")
        );
        return Ok(false);
    }

    let registry = PerformanceBudgetRegistry::new();
    let suites = [
        ("loom", "deep_loom"),
        ("turmoil", "deep_turmoil"),
        ("fuzz-smoke", "deep_fuzz_smoke"),
        ("recovery", "deep_recovery"),
    ];
    let mut ok = true;
    for (step_name, budget_name) in suites {
        let Some(step) = steps.iter().find(|step| step.name == step_name) else {
            println!("  deep-timing: required suite `{step_name}` was not run");
            ok = false;
            continue;
        };
        if step.duration_ms == 0 {
            println!("  deep-timing: {step_name} reported a zero duration");
            ok = false;
            continue;
        }
        let duration_ns = step.duration_ms as f64 * 1_000_000.0;
        let record = BenchmarkRecord {
            name: budget_name.to_string(),
            host_class: host.host_class.clone(),
            p50_ns: duration_ns,
            p95_ns: duration_ns,
            throughput_units_per_sec: 1_000.0 / step.duration_ms as f64,
            raw_samples: None,
        };
        match registry.check_regression(&record, 0.20) {
            Ok(()) => println!(
                "  deep-timing: {step_name} {} ms within accepted {} baseline",
                step.duration_ms, host.host_class
            ),
            Err(error) => {
                println!("  deep-timing: {step_name} failed: {error}");
                ok = false;
            }
        }
    }
    Ok(ok)
}

pub fn write_reports(steps: &[LaneStep], deep: bool) -> Result<()> {
    let report = make_report(steps, deep);
    let normalized = normalize_report(report);
    let file = Path::new(CHECK_REPORT_DIR).join(if deep { "deep.json" } else { "fast.json" });
    util::write_pretty(&file, &normalized)?;
    println!("report: {}", file.display());
    Ok(())
}

/// Normalize a report so local and CI runs at the same commit produce
/// byte-identical JSON after path relativization and timestamp stripping.
fn normalize_report(mut report: Value) -> Value {
    if let Some(obj) = report.as_object_mut() {
        obj.remove("timestamp");
        if let Ok(home) = std::env::var("HOME") {
            for value in obj.values_mut() {
                normalize_strings(value, &home, "$HOME");
            }
        }
        let root = std::env::current_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        if !root.is_empty() {
            for value in obj.values_mut() {
                normalize_strings(value, &root, ".");
            }
        }
    }
    report
}

fn normalize_strings(value: &mut Value, needle: &str, replacement: &str) {
    match value {
        Value::String(s) => {
            *s = s.replace(needle, replacement);
        }
        Value::Array(items) => {
            for item in items {
                normalize_strings(item, needle, replacement);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                normalize_strings(item, needle, replacement);
            }
        }
        _ => {}
    }
}

pub fn self_test() -> Result<Vec<(&'static str, bool, String)>> {
    let mut results = Vec::new();

    let fail_steps = vec![
        LaneStep {
            name: "a".to_string(),
            ok: true,
            skipped: false,
            duration_ms: 1,
            detail: String::new(),
        },
        LaneStep {
            name: "b".to_string(),
            ok: false,
            skipped: false,
            duration_ms: 1,
            detail: String::new(),
        },
    ];
    let report = make_report(&fail_steps, false);
    let overall = report.get("overall").and_then(|o| o.as_str()).unwrap_or("");
    results.push((
        "overall=failed when any step fails",
        overall == "failed",
        format!("overall={overall}"),
    ));

    let pass_steps = vec![LaneStep {
        name: "a".to_string(),
        ok: true,
        skipped: false,
        duration_ms: 1,
        detail: String::new(),
    }];
    let report = make_report(&pass_steps, false);
    let overall = report.get("overall").and_then(|o| o.as_str()).unwrap_or("");
    results.push((
        "overall=passed when all steps pass",
        overall == "passed",
        format!("overall={overall}"),
    ));

    let mut report_a = make_report(&pass_steps, false);
    let mut report_b = make_report(&pass_steps, false);
    if let (Some(a), Some(b)) = (report_a.as_object_mut(), report_b.as_object_mut()) {
        a.insert("path".to_string(), json!("/abs/fake/root/target/out"));
        b.insert("path".to_string(), json!("/abs/fake/root/target/out"));
    }
    let norm_a = normalize_report_with(report_a, "/abs/fake/root", "<root>");
    let norm_b = normalize_report_with(report_b, "/abs/fake/root", "<root>");
    let path_a = norm_a.get("path").and_then(|p| p.as_str()).unwrap_or("");
    results.push((
        "normalization relativizes absolute paths",
        path_a == "<root>/target/out",
        format!("path={path_a}"),
    ));

    let serialized_a = serde_json::to_string(&norm_a).unwrap_or_default();
    let serialized_b = serde_json::to_string(&norm_b).unwrap_or_default();
    results.push((
        "normalized reports are byte-identical (paths + no timestamp)",
        serialized_a == serialized_b && !serialized_a.contains("timestamp"),
        format!("len={}", serialized_a.len()),
    ));

    let fast_cmds = list_commands(false);
    let deep_markers = ["recovery_test", "reflex-fuzz", "loom", "turmoil", "deep"];
    let fast_clean = !fast_cmds
        .iter()
        .any(|c| deep_markers.iter().any(|m| c.contains(m)));
    results.push((
        "fast list_commands excludes deep-only suites",
        fast_clean,
        fast_cmds.join("; "),
    ));

    Ok(results)
}

fn make_report(steps: &[LaneStep], deep: bool) -> Value {
    let results: Vec<Value> = steps
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "ok": s.ok,
                "skipped": s.skipped,
                "detail": s.detail,
            })
        })
        .collect();
    let overall = steps.iter().all(|s| s.ok) && !steps.is_empty();
    json!({
        "lane": if deep { "deep" } else { "fast" },
        "commit": util::git_head().expect("exact git identity is required for check evidence"),
        "overall": if overall { "passed" } else { "failed" },
        "steps": results,
    })
}

fn normalize_report_with(mut report: Value, needle: &str, replacement: &str) -> Value {
    if let Some(obj) = report.as_object_mut() {
        obj.remove("timestamp");
        for value in obj.values_mut() {
            normalize_strings(value, needle, replacement);
        }
    }
    report
}

pub fn list_commands(deep: bool) -> Vec<&'static str> {
    let mut cmds = vec![
        "cargo metadata --locked (toolchain gate)",
        "cargo fmt --check",
        "cargo clippy --workspace --locked -- -D warnings",
        "cargo nextest run --workspace --locked | cargo test --workspace --locked",
        "cargo test --workspace --locked --doc",
        "scripts/check-unsafe.sh",
        "schema fingerprint verify (evidence/schema/fingerprint.json)",
        "doc link check (docs/)",
        "cargo xtask docs-validate",
        "cargo xtask sbom --verify",
        "cargo deny check",
    ];
    if deep {
        cmds.extend([
            "RUSTFLAGS='--cfg reflex_loom' cargo test -p reflex-runtime --locked --release --lib loom_",
            "RUSTFLAGS='--cfg turmoil' cargo test -p reflex-scheduler --locked --lib turmoil_",
            "cargo test -p reflex-fuzz --locked",
            "cargo test -p reflex-integration-tests --locked --test recovery_test",
            "deep-timing-baseline compare",
        ]);
    }
    cmds
}
