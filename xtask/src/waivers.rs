use crate::util::{self, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const DENY_EXCEPTIONS_PATH: &str = "deny-exceptions.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenyExceptionEntry {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub advisory_id: Option<String>,
    pub owner: String,
    pub rationale: String,
    pub expiry_date: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DenyExceptionsFile {
    pub entries: Vec<DenyExceptionEntry>,
}

pub fn load_exceptions() -> Result<Vec<DenyExceptionEntry>> {
    let path = Path::new(DENY_EXCEPTIONS_PATH);
    let text = std::fs::read_to_string(path)
        .map_err(|e| util::msg(format!("cannot read {DENY_EXCEPTIONS_PATH}: {e}")))?;
    let file: DenyExceptionsFile = serde_json::from_str(&text)
        .map_err(|e| util::msg(format!("{DENY_EXCEPTIONS_PATH} invalid JSON: {e}")))?;
    Ok(file.entries)
}

pub fn validate_entry(entry: &DenyExceptionEntry) -> Vec<String> {
    let mut problems = Vec::new();
    if entry.id.trim().is_empty() {
        problems.push("missing id".to_string());
    }
    if entry.kind.trim().is_empty() {
        problems.push("missing kind".to_string());
    }
    if entry.owner.trim().is_empty() {
        problems.push("missing owner".to_string());
    }
    if entry.rationale.trim().is_empty() {
        problems.push("missing rationale".to_string());
    }
    if entry.expiry_date.trim().is_empty() {
        problems.push("missing expiry_date".to_string());
    } else if entry.expiry_date.len() < 10 {
        problems.push(format!("invalid expiry_date '{}'", entry.expiry_date));
    }
    if entry.evidence.is_empty() {
        problems.push("missing evidence links".to_string());
    } else {
        for (i, ev) in entry.evidence.iter().enumerate() {
            if ev.trim().is_empty() {
                problems.push(format!("evidence[{i}] is empty"));
            }
        }
    }
    problems
}

pub fn validate_registry() -> Result<Vec<String>> {
    let entries = load_exceptions()?;
    let mut problems = Vec::new();
    for entry in &entries {
        for p in validate_entry(entry) {
            problems.push(format!("{}: {p}", entry.id));
        }
    }

    let deny_text = std::fs::read_to_string("deny.toml").unwrap_or_default();
    if deny_text.contains("RUSTSEC-2024-0436")
        && !entries
            .iter()
            .any(|e| e.advisory_id.as_deref() == Some("RUSTSEC-2024-0436"))
    {
        problems.push(
            "deny.toml ignores RUSTSEC-2024-0436 but deny-exceptions.json has no matching entry"
                .to_string(),
        );
    }
    if deny_text.contains("ndarray@0.17.2")
        && !entries
            .iter()
            .any(|e| e.name.as_deref() == Some("ndarray") && e.kind == "multiple-versions")
    {
        problems.push(
            "deny.toml skip-tree ndarray@0.17.2 has no matching deny-exceptions.json entry"
                .to_string(),
        );
    }
    for name in ["colored", "option-ext"] {
        if deny_text.contains(&format!("name = \"{name}\""))
            && !entries.iter().any(|e| e.name.as_deref() == Some(name))
        {
            problems.push(format!(
                "deny.toml license exception for {name} has no deny-exceptions.json entry"
            ));
        }
    }

    Ok(problems)
}

pub fn self_test() -> Result<Vec<(&'static str, bool, String)>> {
    let mut results = Vec::new();

    let problems = validate_registry()?;
    results.push((
        "live deny-exceptions.json passes field validation",
        problems.is_empty(),
        if problems.is_empty() {
            "ok".to_string()
        } else {
            problems.join("; ")
        },
    ));

    let bad = DenyExceptionEntry {
        id: "bad".to_string(),
        kind: "test".to_string(),
        name: None,
        advisory_id: None,
        owner: String::new(),
        rationale: String::new(),
        expiry_date: String::new(),
        evidence: vec![],
    };
    let bad_problems = validate_entry(&bad);
    results.push((
        "entry missing owner/rationale/expiry/evidence is rejected",
        bad_problems.len() >= 4,
        format!("{} problems", bad_problems.len()),
    ));

    let good = DenyExceptionEntry {
        id: "good".to_string(),
        kind: "test".to_string(),
        name: Some("foo".to_string()),
        advisory_id: None,
        owner: "platform".to_string(),
        rationale: "fixture".to_string(),
        expiry_date: "2099-01-01".to_string(),
        evidence: vec!["bench.json".to_string()],
    };
    results.push((
        "complete entry passes validation",
        validate_entry(&good).is_empty(),
        if validate_entry(&good).is_empty() {
            "ok".to_string()
        } else {
            "failed".to_string()
        },
    ));

    Ok(results)
}
