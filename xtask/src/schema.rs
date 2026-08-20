use crate::util::{self, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const FINGERPRINT_PATH: &str = "evidence/schema/fingerprint.json";

/// Deterministic digest over sorted paths under `sql/` and `proto/`.
pub fn compute_fingerprint(root: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for dir in ["sql", "proto"] {
        let base = root.join(dir);
        if !base.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        collect_files(&base, &mut files);
        files.sort();
        let mut hasher_input = String::new();
        for path in files {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let content = std::fs::read(&path).unwrap_or_default();
            hasher_input.push_str(&rel.display().to_string());
            hasher_input.push('\0');
            hasher_input.push_str(&util::sha256_hex(&content));
            hasher_input.push('\n');
        }
        out.insert(dir.to_string(), util::sha256_hex(hasher_input.as_bytes()));
    }
    out
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, out);
        } else {
            out.push(path);
        }
    }
}

pub fn load_committed_fingerprint() -> Result<Value> {
    util::read_json(Path::new(FINGERPRINT_PATH))
}

pub fn write_fingerprint(root: &Path, output: &Path) -> Result<()> {
    let hashes = compute_fingerprint(root);
    let migrations = list_migrations(root);
    let doc = json!({
        "commit": util::git_head_short(),
        "sql": hashes.get("sql").cloned().unwrap_or_default(),
        "proto": hashes.get("proto").cloned().unwrap_or_default(),
        "migrations": migrations,
    });
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    util::write_pretty(output, &doc)
}

fn list_migrations(root: &Path) -> Vec<String> {
    let out = Vec::new();
    let sql = root.join("sql");
    if !sql.is_dir() {
        return out;
    }
    let mut paths = Vec::new();
    collect_files(&sql, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| p.strip_prefix(root).ok().map(|r| r.display().to_string()))
        .filter(|p| p.ends_with(".sql"))
        .collect()
}

/// Fast lane: committed fingerprint must match the live tree.
pub fn verify_fingerprint(root: &Path) -> Result<bool> {
    let current = compute_fingerprint(root);
    let committed = match load_committed_fingerprint() {
        Ok(v) => v,
        Err(_) => {
            println!(
                "  schema-fingerprint: {FINGERPRINT_PATH} missing (run `cargo xtask schema-fingerprint`)"
            );
            return Ok(false);
        }
    };
    let mut ok = true;
    for key in ["sql", "proto"] {
        let live = current.get(key).map(String::as_str).unwrap_or("");
        let stored = committed.get(key).and_then(|v| v.as_str()).unwrap_or("");
        if live != stored {
            println!("  schema-fingerprint: {key} hash mismatch (live={live} stored={stored})");
            ok = false;
        }
    }
    if ok {
        println!("  schema-fingerprint: {FINGERPRINT_PATH} matches live sql/ and proto/");
    }
    Ok(ok)
}

/// Docs validation: schema drift without ADR + numbered migration fails.
pub fn check_schema_drift(root: &Path, adr_dir: &Path) -> Result<(bool, String)> {
    let current = compute_fingerprint(root);
    let committed = load_committed_fingerprint().unwrap_or(json!({}));
    let mut drifted = false;
    for key in ["sql", "proto"] {
        let live = current.get(key).map(String::as_str).unwrap_or("");
        let stored = committed.get(key).and_then(|v| v.as_str()).unwrap_or("");
        if !stored.is_empty() && live != stored {
            drifted = true;
        }
    }
    if !drifted {
        return Ok((
            true,
            "schema fingerprints match committed baseline".to_string(),
        ));
    }

    let migrations = list_migrations(root);
    let has_numbered_migration = migrations.iter().any(|m| {
        m.split('/')
            .any(|seg| seg.chars().take(4).all(|c| c.is_ascii_digit()) && seg.contains('_'))
    });
    if !has_numbered_migration {
        return Ok((
            false,
            "schema drift detected but no numbered migration under sql/".to_string(),
        ));
    }

    let adr_text = adr_files_text(adr_dir);
    let adr_refs_schema = adr_text.contains("schema") || adr_text.contains("migration");
    if !adr_refs_schema {
        return Ok((
            false,
            "schema drift detected but no ADR mentions schema or migration".to_string(),
        ));
    }

    Ok((
        false,
        "schema drift detected: update evidence/schema/fingerprint.json after ADR + migration"
            .to_string(),
    ))
}

fn adr_files_text(adr_dir: &Path) -> String {
    let Ok(entries) = std::fs::read_dir(adr_dir) else {
        return String::new();
    };
    let mut text = String::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("md")
            && let Ok(body) = std::fs::read_to_string(&path)
        {
            text.push_str(&body);
            text.push('\n');
        }
    }
    text
}

/// Scan markdown under `docs/` for relative links and verify targets exist.
pub fn check_doc_links(docs_root: &Path) -> Result<(bool, Vec<String>)> {
    let mut broken = Vec::new();
    let mut files = Vec::new();
    collect_files(docs_root, &mut files);
    for path in files {
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        for link in extract_markdown_links(&text) {
            if link.starts_with("http://")
                || link.starts_with("https://")
                || link.starts_with('#')
                || link.starts_with("mailto:")
            {
                continue;
            }
            let target = docs_root.join(link.trim_start_matches('/'));
            if !target.exists() {
                broken.push(format!(
                    "{} -> {} (from {})",
                    link,
                    target.display(),
                    path.strip_prefix(docs_root).unwrap_or(&path).display()
                ));
            }
        }
    }
    broken.sort();
    broken.dedup();
    Ok((broken.is_empty(), broken))
}

fn extract_markdown_links(text: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        if let Some(end) = after.find(')') {
            let url = after[..end].trim();
            if !url.is_empty() {
                links.push(url.to_string());
            }
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
    links
}
