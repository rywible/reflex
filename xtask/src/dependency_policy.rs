use crate::util::{self, Result};
use std::path::Path;

pub const DENY_TOML: &str = "deny.toml";

/// P0.3 supply-chain policy checks (deny.toml parse + cargo-deny run when
/// installed). Returns true when policy constraints hold.
pub fn parse_policy() -> bool {
    let path = Path::new(DENY_TOML);
    let Ok(text) = std::fs::read_to_string(path) else {
        println!("  dependency-policy: {DENY_TOML} missing");
        return false;
    };
    let Ok(config) = toml::from_str::<toml::Value>(&text) else {
        println!("  dependency-policy: {DENY_TOML} is invalid TOML");
        return false;
    };
    let mut problems = Vec::new();

    // [advisories] yanked must be denied.
    match config
        .get("advisories")
        .and_then(|a| a.get("yanked"))
        .and_then(|y| y.as_str())
    {
        Some("deny") => {}
        other => problems.push(format!("advisories.yanked must be 'deny', got {other:?}")),
    }

    // [sources] unknown-git denied, allow-git empty, crates.io only registry.
    let sources = config.get("sources");
    match sources
        .and_then(|s| s.get("unknown-git"))
        .and_then(|v| v.as_str())
    {
        Some("deny") => {}
        other => problems.push(format!("sources.unknown-git must be 'deny', got {other:?}")),
    }
    if let Some(git) = sources
        .and_then(|s| s.get("allow-git"))
        .and_then(|v| v.as_array())
        && !git.is_empty()
    {
        problems.push(format!(
            "sources.allow-git must be empty (git sources require an accepted ADR), found {} entries",
            git.len()
        ));
    }
    let registries = sources
        .and_then(|s| s.get("allow-registry"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();
    if registries != ["https://github.com/rust-lang/crates.io-index"] {
        problems.push(format!(
            "sources.allow-registry must be exactly [crates.io index], got {registries:?}"
        ));
    }

    // [licenses] permissive allow-list must be present.
    let licenses = config.get("licenses");
    let allow = licenses
        .and_then(|l| l.get("allow"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        .unwrap_or_default();
    for required in ["MIT", "Apache-2.0"] {
        if !allow.contains(&required) {
            problems.push(format!("licenses.allow missing {required}"));
        }
    }

    if problems.is_empty() {
        println!("  dependency-policy: deny.toml policy constraints hold");
        true
    } else {
        for p in &problems {
            println!("  dependency-policy: {p}");
        }
        false
    }
}

/// Hermetic self-test: injected policy violations must be detected.
pub fn self_test() -> Result<Vec<(&'static str, bool, String)>> {
    let mut results = Vec::new();
    let dir = std::env::temp_dir().join(format!("xtask_policy_selftest_{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;

    let good = dir.join("good.toml");
    std::fs::write(&good, good_policy())?;
    let bad = dir.join("bad.toml");
    std::fs::write(&bad, bad_policy())?;

    results.push((
        "good policy passes",
        check_file(&good),
        if check_file(&good) {
            "passed"
        } else {
            "failed"
        }
        .to_string(),
    ));
    results.push((
        "bad policy is rejected",
        !check_file(&bad),
        if check_file(&bad) {
            "not rejected"
        } else {
            "rejected"
        }
        .to_string(),
    ));

    std::fs::remove_dir_all(&dir)?;

    // Live cargo-deny must reject an unapproved Git dependency fixture.
    let git_fixture = Path::new("xtask/fixtures/deny-unapproved-git");
    if util::cmd_exists("cargo-deny") && git_fixture.join("Cargo.toml").exists() {
        let deny = util::run_cmd_in_dir(git_fixture, "cargo", &["deny", "check"]);
        results.push((
            "cargo-deny rejects unapproved git fixture",
            !deny.success,
            if deny.success {
                "unexpected pass".to_string()
            } else {
                "rejected".to_string()
            },
        ));
    } else {
        results.push((
            "cargo-deny rejects unapproved git fixture",
            false,
            "cargo-deny missing or fixture absent".to_string(),
        ));
    }

    Ok(results)
}

fn check_file(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(config) = toml::from_str::<toml::Value>(&text) else {
        return false;
    };
    let ok_yanked = config
        .get("advisories")
        .and_then(|a| a.get("yanked"))
        .and_then(|y| y.as_str())
        == Some("deny");
    let ok_git = config
        .get("sources")
        .and_then(|s| s.get("unknown-git"))
        .and_then(|v| v.as_str())
        == Some("deny");
    let ok_registry = config
        .get("sources")
        .and_then(|s| s.get("allow-registry"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
        == Some(vec!["https://github.com/rust-lang/crates.io-index"]);
    let ok_allow = config
        .get("licenses")
        .and_then(|l| l.get("allow"))
        .and_then(|v| v.as_array())
        .map(|a| a.iter().any(|v| v.as_str() == Some("MIT")))
        .unwrap_or(false);
    ok_yanked && ok_git && ok_registry && ok_allow
}

fn good_policy() -> String {
    "[advisories]\nyanked = \"deny\"\n\n[sources]\nunknown-registry = \"deny\"\nunknown-git = \"deny\"\nallow-registry = [\"https://github.com/rust-lang/crates.io-index\"]\nallow-git = []\n\n[licenses]\nallow = [\"MIT\", \"Apache-2.0\"]\n".to_string()
}

fn bad_policy() -> String {
    "[advisories]\nyanked = \"warn\"\n\n[sources]\nunknown-registry = \"deny\"\nunknown-git = \"allow\"\nallow-registry = [\"https://github.com/rust-lang/crates.io-index\"]\nallow-git = [\"https://github.com/someone/somewhere\"]\n\n[licenses]\nallow = [\"MIT\"]\n".to_string()
}
