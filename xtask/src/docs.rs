use crate::schema;
use crate::util::{self, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const INVARIANTS_PATH: &str = "docs/invariants.md";
pub const ADR_DIR: &str = "docs/adr";
pub const REPORT_PATH: &str = "evidence/invariants/report.json";

pub const ADR_REQUIRED_SECTIONS: &[&str] =
    &["## Status", "## Context", "## Decision", "## Consequences"];

#[derive(Debug)]
pub struct DocsReport {
    pub ok: bool,
    pub items: Vec<Value>,
    pub invariants: Vec<InvariantCheck>,
    pub adrs: Vec<AdrCheck>,
}

#[derive(Debug)]
pub struct InvariantCheck {
    pub id: String,
    pub test: Option<String>,
    pub test_found: bool,
    pub test_path: Option<String>,
    pub evidence_path: String,
}

#[derive(Debug)]
pub struct AdrCheck {
    pub file: String,
    pub sections_ok: bool,
    pub indexed: bool,
}

/// Validate docs: invariants <=> executable tests, ADR front matter and
/// index, schema files referenced by ADRs.
pub fn validate_docs(
    invariants_path: &Path,
    adr_dir: &Path,
    tests_dir: &Path,
) -> Result<DocsReport> {
    let mut items = Vec::new();
    let mut all_ok = true;

    // ---- invariants -------------------------------------------------------
    let mut invariants = Vec::new();
    let mut seen_ids = std::collections::BTreeSet::new();
    let inv_text = std::fs::read_to_string(invariants_path)
        .map_err(|e| util::msg(format!("cannot read {}: {e}", invariants_path.display())))?;
    for line in inv_text.lines() {
        if !line.starts_with("| INV-RFX-") {
            continue;
        }
        let cols: Vec<&str> = line.split('|').map(|c| c.trim()).collect();
        let id = cols[1].to_string();
        if !seen_ids.insert(id.clone()) {
            all_ok = false;
            items.push(json!({
                "check": "invariant-duplicate-ids",
                "ok": false,
                "detail": format!("duplicate invariant ID {id}")
            }));
            continue;
        }
        let test_col = cols.get(6).copied().unwrap_or("");
        let tests: Vec<String> = test_col
            .split(',')
            .flat_map(|t| t.split(" or "))
            .map(|t| t.trim().trim_matches('`').to_string())
            .filter(|t| !t.is_empty() && t != "—" && t != "-")
            .collect();
        let test = tests.first().cloned();
        let test_path = test.as_ref().and_then(|t| find_test_file(t, tests_dir));
        let test_found = test_path.is_some();
        invariants.push(InvariantCheck {
            id: id.clone(),
            test: test.clone(),
            test_found,
            test_path: test_path.clone(),
            evidence_path: format!(
                "tests/invariants_test.rs#{}",
                test.as_deref().unwrap_or("missing")
            ),
        });
    }
    let uncovered: Vec<&InvariantCheck> = invariants.iter().filter(|i| !i.test_found).collect();
    if uncovered.is_empty() {
        items.push(json!({
            "check": "invariant-test-coverage",
            "ok": true,
            "detail": format!("{} invariants each map to an executable test", invariants.len())
        }));
    } else {
        all_ok = false;
        items.push(json!({
            "check": "invariant-test-coverage",
            "ok": false,
            "detail": format!(
                "uncovered invariants: {}",
                uncovered.iter().map(|i| i.id.as_str()).collect::<Vec<_>>().join(", ")
            )
        }));
    }

    // ---- ADRs --------------------------------------------------------------
    let mut adrs = Vec::new();
    let readme = adr_dir.join("README.md");
    let readme_text = std::fs::read_to_string(&readme).unwrap_or_default();
    let index_present = readme.exists();
    let mut adr_files: Vec<PathBuf> = std::fs::read_dir(adr_dir)
        .map_err(|e| util::msg(format!("cannot read {}: {e}", adr_dir.display())))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .filter(|p| {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name != "README.md" && name != "TEMPLATE.md"
        })
        .collect();
    adr_files.sort();
    for path in adr_files {
        let file = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let sections_ok = ADR_REQUIRED_SECTIONS.iter().all(|s| text.contains(s));
        let number = adr_number(&file);
        let indexed = number
            .map(|n| readme_text.contains(&format!("| {n} ")) || readme_text.contains(&n))
            .unwrap_or(false);
        let indexed = indexed && index_present;
        adrs.push(AdrCheck {
            file,
            sections_ok,
            indexed,
        });
    }
    let bad_adrs: Vec<&AdrCheck> = adrs
        .iter()
        .filter(|a| !a.sections_ok || !a.indexed)
        .collect();
    if bad_adrs.is_empty() && index_present {
        items.push(json!({
            "check": "adr-front-matter-and-index",
            "ok": true,
            "detail": format!("{} ADRs have required sections and are indexed", adrs.len())
        }));
    } else {
        all_ok = false;
        let mut detail = Vec::new();
        if !index_present {
            detail.push("docs/adr/README.md (ADR index) is missing".to_string());
        }
        for a in &bad_adrs {
            if !a.sections_ok {
                detail.push(format!("{} missing required sections", a.file));
            }
            if !a.indexed {
                detail.push(format!("{} not listed in the ADR index", a.file));
            }
        }
        items.push(json!({"check": "adr-front-matter-and-index", "ok": false, "detail": detail.join("; ")}));
    }

    // ---- schema files referenced by ADRs -----------------------------------
    let mut missing_refs = Vec::new();
    for path in adr_files_iter(adr_dir) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for word in text.split_whitespace() {
            let clean = word.trim_matches(|c: char| {
                !c.is_alphanumeric() && c != '/' && c != '.' && c != '-' && c != '_'
            });
            if (clean.starts_with("sql/")
                || clean.starts_with("proto/")
                || clean.starts_with("deploy/"))
                && clean.len() > 5
                && !clean.ends_with('/')
                && !Path::new(clean).exists()
            {
                missing_refs.push(format!("{clean} (referenced in {:?})", path.file_name()));
            }
        }
    }
    if missing_refs.is_empty() {
        items.push(json!({"check": "adr-schema-references", "ok": true, "detail": "all schema files referenced by ADRs exist"}));
    } else {
        all_ok = false;
        items.push(json!({"check": "adr-schema-references", "ok": false, "detail": missing_refs.join("; ")}));
    }

    // ---- schema drift (committed fingerprint vs live tree) ----------------
    match schema::check_schema_drift(Path::new("."), adr_dir) {
        Ok((ok, detail)) => {
            if ok {
                items.push(json!({"check": "schema-drift", "ok": true, "detail": detail}));
            } else {
                all_ok = false;
                items.push(json!({"check": "schema-drift", "ok": false, "detail": detail}));
            }
        }
        Err(e) => {
            all_ok = false;
            items.push(json!({"check": "schema-drift", "ok": false, "detail": format!("{e}")}));
        }
    }

    Ok(DocsReport {
        ok: all_ok,
        items,
        invariants,
        adrs,
    })
}

fn adr_number(file: &str) -> Option<String> {
    let stem = file.strip_suffix(".md").unwrap_or(file);
    if let Some(rest) = stem.strip_prefix("ADR-") {
        let num = rest.split('-').next().unwrap_or(rest);
        if num.len() == 4 && num.chars().all(|c| c.is_ascii_digit()) {
            return Some(num.to_string());
        }
    }
    stem.split_once('-')
        .map(|(n, _)| n.to_string())
        .filter(|n| n.len() == 4 && n.chars().all(|c| c.is_ascii_digit()))
}

fn adr_files_iter(adr_dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(adr_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .collect()
}

fn find_test_file(t: &str, tests_dir: &Path) -> Option<String> {
    let needle = format!("fn {t}");
    for p in walk_files(tests_dir) {
        if std::fs::read_to_string(&p)
            .map(|s| s.contains(&needle))
            .unwrap_or(false)
        {
            return Some(p.display().to_string());
        }
    }
    None
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk_files(&path));
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
    out
}

pub fn write_report(report: &DocsReport) -> Result<()> {
    let invariants: Vec<Value> = report
        .invariants
        .iter()
        .map(|i| {
            json!({
                "id": i.id,
                "test": i.test,
                "test_found": i.test_found,
                "test_path": i.test_path,
                "evidence_path": i.evidence_path,
            })
        })
        .collect();
    let adrs: Vec<Value> = report
        .adrs
        .iter()
        .map(|a| {
            json!({
                "file": a.file,
                "sections_ok": a.sections_ok,
                "indexed": a.indexed,
            })
        })
        .collect();
    let report_value = json!({
        "commit": util::git_head().expect("exact git identity is required for invariant evidence"),
        "timestamp": util::now_utc_iso(),
        "overall": if report.ok { "passed" } else { "failed" },
        "invariants": invariants,
        "adrs": adrs,
        "checks": report.items,
    });
    util::write_pretty(Path::new(REPORT_PATH), &report_value)
}

/// Hermetic self-test: run the doc checks against synthetic fixture trees.
pub fn self_test() -> Result<Vec<(&'static str, bool, String)>> {
    let mut results = Vec::new();
    let dir = std::env::temp_dir().join(format!("xtask_docs_selftest_{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;

    let good_inv = dir.join("good/invariants.md");
    let good_adr = dir.join("good/adr");
    std::fs::create_dir_all(&good_adr)?;
    std::fs::write(
        &good_inv,
        "| ID | Title | Authority | Owner Crate | Enforcing API | Verification Test | Performance Measurement |\n\
         |---|---|---|---|---|---|---|---|\n\
         | INV-RFX-1 | A | §2 | `x` | `f` | `test_inv_1_fixture` | — |\n",
    )?;
    std::fs::write(
        good_adr.join("0001-fixture.md"),
        fixture_adr("0001", "Fixture"),
    )?;
    std::fs::write(
        good_adr.join("0002-fixture.md"),
        fixture_adr("0002", "Fixture Two"),
    )?;
    std::fs::write(
        good_adr.join("README.md"),
        "## Index\n| 0001 | Fixture | Accepted | — |\n| 0002 | Fixture Two | Accepted | — |\n",
    )?;
    let test_file = dir.join("good/tests");
    std::fs::create_dir_all(&test_file)?;
    std::fs::write(test_file.join("fixture.rs"), "fn test_inv_1_fixture() {}\n")?;

    let good = validate_docs(&good_inv, &good_adr, &dir.join("good/tests"))?;
    results.push((
        "good fixture passes",
        good.ok,
        format!(
            "overall={} items={}",
            if good.ok { "passed" } else { "failed" },
            good.items.len()
        ),
    ));

    let bad_inv = dir.join("bad/invariants.md");
    let bad_adr = dir.join("bad/adr");
    std::fs::create_dir_all(&bad_adr)?;
    std::fs::write(&bad_inv, "| ID | Title |\n| INV-RFX-9 | No test |\n")?;
    std::fs::write(
        bad_adr.join("0003-bad.md"),
        "## Status\nAccepted\n\nMissing Decision section entirely.\n",
    )?;
    std::fs::write(bad_adr.join("README.md"), "## Index\n(empty)\n")?;

    let bad = validate_docs(&bad_inv, &bad_adr, &dir.join("bad/tests"))?;
    let detected =
        !bad.ok
            && bad.items.iter().any(|i| {
                i.get("check").and_then(|c| c.as_str()) == Some("invariant-test-coverage")
            })
            && bad.items.iter().any(|i| {
                i.get("check").and_then(|c| c.as_str()) == Some("adr-front-matter-and-index")
            });
    results.push((
        "broken fixture is rejected with specific checks",
        detected,
        format!(
            "overall={} items={}",
            if bad.ok { "passed" } else { "failed" },
            bad.items.len()
        ),
    ));

    let dup_inv = dir.join("dup/invariants.md");
    let dup_adr = dir.join("dup/adr");
    std::fs::create_dir_all(&dup_adr)?;
    std::fs::write(
        &dup_inv,
        "| ID | Title | Authority | Owner Crate | Enforcing API | Verification Test | Performance Measurement |\n\
         |---|---|---|---|---|---|---|\n\
         | INV-RFX-1 | A | §2 | `x` | `f` | `test_inv_1_fixture` | — |\n\
         | INV-RFX-1 | dup | §2 | `x` | `f` | `test_inv_1_fixture` | — |\n",
    )?;
    std::fs::write(
        dup_adr.join("README.md"),
        "## Index\n| 0001 | Fixture | Accepted | — |\n",
    )?;
    std::fs::write(
        dup_adr.join("0001-fixture.md"),
        fixture_adr("0001", "Fixture"),
    )?;
    let dup_tests = dir.join("dup/tests");
    std::fs::create_dir_all(&dup_tests)?;
    std::fs::write(dup_tests.join("fixture.rs"), "fn test_inv_1_fixture() {}\n")?;
    let dup = validate_docs(&dup_inv, &dup_adr, &dup_tests)?;
    let dup_detected = !dup.ok
        && dup
            .items
            .iter()
            .any(|i| i.get("check").and_then(|c| c.as_str()) == Some("invariant-duplicate-ids"));
    results.push((
        "duplicate invariant IDs are rejected",
        dup_detected,
        format!("overall={}", if dup.ok { "passed" } else { "failed" }),
    ));

    std::fs::remove_dir_all(&dir)?;
    Ok(results)
}

fn fixture_adr(number: &str, title: &str) -> String {
    format!(
        "# ADR {number}: {title}\n\n## Status\nAccepted\n\n## Context\nc\n\n## Decision\nd\n\n## Consequences\nc\n"
    )
}
