use crate::util::{self, Result};
use serde_json::{Map, Value, json};
use std::path::Path;

pub const SBOM_PATH: &str = "evidence/sbom/cyclonedx.json";

/// Build the deterministic CycloneDX 1.5 document from Cargo.lock.
pub fn build(lock_path: &Path, commit_time: &str) -> Result<Value> {
    let text = std::fs::read_to_string(lock_path)?;
    let lock: toml::Value = toml::from_str(&text)?;
    let packages = lock
        .get("package")
        .and_then(|p| p.as_array())
        .ok_or_else(|| util::msg("Cargo.lock has no [[package]] section"))?;

    let mut components = Vec::new();
    let mut dep_map: Vec<(String, Vec<String>)> = Vec::new();
    for pkg in packages {
        let name = pkg.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let version = pkg.get("version").and_then(|v| v.as_str()).unwrap_or("");
        if name.is_empty() || version.is_empty() {
            continue;
        }
        let purl = format!("pkg:cargo/{name}@{version}");
        let mut component = Map::new();
        component.insert("type".to_string(), Value::String("library".to_string()));
        component.insert("name".to_string(), Value::String(name.to_string()));
        component.insert("version".to_string(), Value::String(version.to_string()));
        component.insert("purl".to_string(), Value::String(purl.clone()));
        components.push(component);

        let deps: Vec<String> = pkg
            .get("dependencies")
            .and_then(|d| d.as_array())
            .map(|d| {
                d.iter()
                    .filter_map(|dep| dep.as_str())
                    .map(|dep| {
                        // Lockfile format: "name version" or just "name".
                        let dep_name = dep.split(' ').next().unwrap_or(dep).to_string();
                        format!("pkg:cargo/{dep_name}")
                    })
                    .collect()
            })
            .unwrap_or_default();
        dep_map.push((purl, deps));
    }

    components.sort_by(|a, b| {
        let an = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let bn = b.get("name").and_then(|n| n.as_str()).unwrap_or("");
        an.cmp(bn).then(
            a.get("version")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .cmp(b.get("version").and_then(|v| v.as_str()).unwrap_or("")),
        )
    });

    let known_purls: std::collections::BTreeSet<String> = components
        .iter()
        .filter_map(|c| {
            c.get("purl")
                .and_then(|p| p.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    let mut dependencies = Vec::new();
    for (purl, deps) in dep_map {
        let resolved: Vec<String> = deps
            .iter()
            .filter(|d| known_purls.iter().any(|k| k.starts_with(&format!("{d}@"))))
            .cloned()
            .collect();
        let mut entry = Map::new();
        entry.insert("ref".to_string(), Value::String(purl));
        entry.insert(
            "dependsOn".to_string(),
            Value::Array(resolved.into_iter().map(Value::String).collect()),
        );
        dependencies.push(Value::Object(entry));
    }
    dependencies.sort_by(|a, b| {
        a.get("ref")
            .and_then(|r| r.as_str())
            .unwrap_or("")
            .cmp(b.get("ref").and_then(|r| r.as_str()).unwrap_or(""))
    });

    let mut doc = Map::new();
    doc.insert(
        "bomFormat".to_string(),
        Value::String("CycloneDX".to_string()),
    );
    doc.insert("specVersion".to_string(), Value::String("1.5".to_string()));
    doc.insert("version".to_string(), Value::Number(1.into()));
    doc.insert(
        "metadata".to_string(),
        json!({
            "timestamp": commit_time,
            "tools": [{"vendor": "reflex", "name": "xtask sbom", "version": "0.1.0"}],
            "component": {"type": "application", "name": "reflex", "version": "0.1.0"}
        }),
    );
    doc.insert(
        "components".to_string(),
        Value::Array(components.into_iter().map(Value::Object).collect()),
    );

    let deps_value = Value::Array(dependencies);
    doc.insert("dependencies".to_string(), deps_value);

    let mut preliminary = doc.clone();
    preliminary.remove("serialNumber");
    let canonical = serde_json::to_string(&Value::Object(preliminary))?;
    let serial = util::sha256_hex(canonical.as_bytes());
    doc.insert(
        "serialNumber".to_string(),
        Value::String(format!("urn:uuid:{}", &serial[..32])),
    );

    Ok(Value::Object(doc))
}

pub fn write_sbom(lock_path: &Path, output: &Path, stdout: bool) -> Result<()> {
    let commit_time = util::git_commit_time_iso();
    let doc = build(lock_path, &commit_time)?;
    let text = serde_json::to_string_pretty(&doc)?;
    if stdout {
        println!("{text}");
    }
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(output, text)?;
    println!("SBOM written to {}", output.display());
    Ok(())
}

/// Regenerate and byte-compare against the committed SBOM.
pub fn sbom_matches(lock_path: &Path) -> Result<bool> {
    let commit_time = util::git_commit_time_iso();
    let doc = build(lock_path, &commit_time)?;
    let regenerated = serde_json::to_string_pretty(&doc)?;
    let path = Path::new(SBOM_PATH);
    Ok(std::fs::read_to_string(path).is_ok_and(|existing| {
        existing.trim_end_matches('\n') == regenerated.trim_end_matches('\n')
    }))
}

pub fn verify_sbom(lock_path: &Path) -> Result<bool> {
    let path = Path::new(SBOM_PATH);
    let matches = sbom_matches(lock_path)?;
    if matches {
        println!(
            "SBOM verify: {} matches the regenerated document",
            path.display()
        );
    } else {
        println!(
            "SBOM verify: {} missing or stale (run `cargo xtask sbom`)",
            path.display()
        );
    }
    Ok(matches)
}
