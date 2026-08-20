// Embeds build-time identity (git commit, target triple, profile, schema
// compatibility range) into the `reflex` binary via cargo rustc-env.
//
// The embedded values are read by `bins/version.rs` and printed by
// `reflex --version` (P0.1 AC: version output includes commit, target
// triple, profile, and schema compatibility range).

const SCHEMA_COMPAT: &str = "arena:1 bundle:1 ledger:1 proto:1";

fn git_commit() -> String {
    if let Some(commit) = std::env::var("GIT_COMMIT").ok().filter(|c| !c.is_empty()) {
        return commit;
    }
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

fn working_tree_dirty() -> bool {
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

fn main() {
    let commit = git_commit();
    let commit_line = if working_tree_dirty() {
        format!("{commit}-dirty")
    } else {
        commit
    };

    println!("cargo:rustc-env=REFLEX_GIT_COMMIT={commit_line}");
    println!(
        "cargo:rustc-env=REFLEX_TARGET_TRIPLE={}",
        std::env::var("TARGET").unwrap_or_else(|_| "unknown".to_string())
    );
    println!(
        "cargo:rustc-env=REFLEX_PROFILE={}",
        std::env::var("PROFILE").unwrap_or_else(|_| "unknown".to_string())
    );
    println!("cargo:rustc-env=REFLEX_SCHEMA_COMPAT={SCHEMA_COMPAT}");

    // Rebuild when the git HEAD moves (loose refs, packed refs, or HEAD
    // re-pointing) or when an explicit GIT_COMMIT override is provided.
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/");
    println!("cargo:rerun-if-changed=.git/packed-refs");
    println!("cargo:rerun-if-env-changed=GIT_COMMIT");
}
