//! Lean 4 kernel verification — real replay or fail-closed (INV-RFX-1).

use reflex_domain::VerifyError;
use reflex_runtime::SandboxPolicy;
use reflex_types::Digest;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Instant;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KernelReceipt {
    pub kernel_certified: bool,
    pub axioms_used: Vec<String>,
    pub kernel_cpu_ns: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeanAvailability {
    Available { version: String },
    Unavailable { reason: String },
}

#[derive(Clone, Debug)]
pub struct KernelSandboxConfig {
    pub policy: SandboxPolicy,
    pub executable: PathBuf,
    pub writable_scratch: PathBuf,
    pub executable_digest: Digest,
}

pub fn probe_lean() -> LeanAvailability {
    LeanAvailability::Unavailable {
        reason: "Lean probing requires an explicit KernelSandboxConfig".into(),
    }
}

pub fn probe_lean_sandboxed(config: &KernelSandboxConfig) -> LeanAvailability {
    match sandboxed_command(config, &["--version".into()]).and_then(|mut command| {
        command
            .output()
            .map_err(|error| VerifyError::Unresolved(error.to_string()))
    }) {
        Ok(out) if out.status.success() => {
            let version = String::from_utf8_lossy(&out.stdout).trim().to_string();
            LeanAvailability::Available { version }
        }
        Ok(out) => LeanAvailability::Unavailable {
            reason: format!(
                "lean exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ),
        },
        Err(e) => LeanAvailability::Unavailable {
            reason: format!("lean binary not found: {e}"),
        },
    }
}

pub fn is_lean_available() -> bool {
    matches!(probe_lean(), LeanAvailability::Available { .. })
}

/// Normalize a goal string into Lean 4 theorem type syntax.
fn normalize_statement(statement: &str) -> String {
    let s = statement.trim();
    if s.starts_with("forall ") {
        format!("∀ {}", s.strip_prefix("forall ").unwrap_or(s).trim())
    } else {
        s.to_string()
    }
}

/// Verify proof via Lean 4 kernel against the declared theorem statement.
/// NEVER wraps the claim as `True`. NEVER returns `kernel_certified: true`
/// without kernel success. Fail-closed when Lean is unavailable.
pub fn verify_with_kernel(
    theorem_name: &str,
    theorem_statement: &str,
    proof_script: &str,
) -> Result<KernelReceipt, VerifyError> {
    validate_claim(theorem_name, theorem_statement, proof_script)?;
    Err(VerifyError::Unresolved(
        "VerifierUnavailable: explicit KernelSandboxConfig is required".into(),
    ))
}

pub fn verify_with_kernel_sandboxed(
    config: &KernelSandboxConfig,
    theorem_name: &str,
    theorem_statement: &str,
    proof_script: &str,
) -> Result<KernelReceipt, VerifyError> {
    validate_claim(theorem_name, theorem_statement, proof_script)?;
    let name = theorem_name.trim();
    let statement = normalize_statement(theorem_statement);
    let script = proof_script.trim();

    let start = Instant::now();
    let lean_source = format!("theorem {name} : {statement} := by\n  {script}\n");

    let mut command = sandboxed_command(config, &["--run".into(), "-".into()])?;
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let output = command.spawn();

    let result = match output {
        Ok(mut child) => {
            use std::io::Write;
            if let Some(stdin) = child.stdin.as_mut() {
                let _ = stdin.write_all(lean_source.as_bytes());
            }
            child.wait_with_output()
        }
        Err(e) => {
            return Err(VerifyError::Unresolved(format!(
                "VerifierUnavailable: failed to spawn lean: {e}"
            )));
        }
    };

    let elapsed = start.elapsed().as_nanos() as u64;

    match result {
        Ok(out) if out.status.success() => Ok(KernelReceipt {
            kernel_certified: true,
            axioms_used: extract_axioms(&String::from_utf8_lossy(&out.stderr)),
            kernel_cpu_ns: elapsed,
        }),
        Ok(out) => Err(VerifyError::Failed(format!(
            "Lean kernel rejected proof for `{name} : {statement}`: {}",
            String::from_utf8_lossy(&out.stderr)
        ))),
        Err(e) => Err(VerifyError::Unresolved(format!(
            "VerifierUnavailable: lean process error: {e}"
        ))),
    }
}

fn validate_claim(
    theorem_name: &str,
    theorem_statement: &str,
    proof_script: &str,
) -> Result<(), VerifyError> {
    if theorem_name.trim().is_empty() {
        return Err(VerifyError::Failed("empty theorem name".to_string()));
    }
    let statement = normalize_statement(theorem_statement);
    if statement.is_empty() {
        return Err(VerifyError::Failed(
            "empty theorem statement: refuse to verify against True placeholder".to_string(),
        ));
    }
    if statement == "True" || statement == "⊤" {
        return Err(VerifyError::Failed(
            "refusing to certify theorem at type True; provide the real goal statement".to_string(),
        ));
    }
    if proof_script.trim().is_empty() {
        return Err(VerifyError::Failed("empty proof script".to_string()));
    }
    Ok(())
}

fn sandboxed_command(
    config: &KernelSandboxConfig,
    arguments: &[String],
) -> Result<std::process::Command, VerifyError> {
    if config.executable_digest == Digest::ZERO {
        return Err(VerifyError::Unresolved(
            "VerifierUnavailable: Lean executable digest is not pinned".into(),
        ));
    }
    let mapping = config
        .policy
        .map_executable(&config.executable)
        .map_err(|error| VerifyError::Unresolved(format!("VerifierUnavailable: {error}")))?;
    let actual = hash_file(&mapping.host_path)?;
    if actual != config.executable_digest {
        return Err(VerifyError::Unresolved(format!(
            "VerifierUnavailable: Lean executable digest mismatch: expected {}, got {}",
            config.executable_digest, actual
        )));
    }
    config
        .policy
        .prepare_linux_command(
            &config.executable,
            &config.writable_scratch,
            arguments,
            &BTreeMap::new(),
        )
        .map_err(|error| VerifyError::Unresolved(format!("VerifierUnavailable: {error}")))
}

fn hash_file(path: &Path) -> Result<Digest, VerifyError> {
    let mut file = std::fs::File::open(path)
        .map_err(|error| VerifyError::Unresolved(format!("VerifierUnavailable: {error}")))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|error| VerifyError::Unresolved(format!("VerifierUnavailable: {error}")))?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(Digest::from_blake3_bytes(*hasher.finalize().as_bytes()))
}

fn extract_axioms(stderr: &str) -> Vec<String> {
    let mut axioms = Vec::new();
    for line in stderr.lines() {
        if (line.contains("axiom") || line.contains("propext")) && line.contains("propext") {
            axioms.push("propext".to_string());
        }
    }
    if axioms.is_empty() {
        axioms.push("kernel".to_string());
    }
    axioms.sort();
    axioms.dedup();
    axioms
}

#[allow(dead_code)]
pub fn kernel_receipt_digest(receipt: &KernelReceipt) -> Digest {
    Digest::hash_blake3(
        format!(
            "lean-kernel:{}:{}:{}",
            receipt.kernel_certified,
            receipt.axioms_used.join(","),
            receipt.kernel_cpu_ns
        )
        .as_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_never_fabricate_without_lean() {
        if is_lean_available() {
            return;
        }
        let res = verify_with_kernel(
            "Nat.add_comm",
            "∀ (a b : Nat), a + b = b + a",
            "intro a b; omega",
        );
        assert!(res.is_err());
        match res {
            Err(VerifyError::Unresolved(msg)) => {
                assert!(msg.contains("VerifierUnavailable"));
            }
            _ => panic!("expected Unresolved VerifierUnavailable"),
        }
    }

    #[test]
    fn test_empty_script_rejected() {
        let res = verify_with_kernel("test", "∀ (a : Nat), a = a", "");
        assert!(matches!(res, Err(VerifyError::Failed(_))));
    }

    #[test]
    fn test_refuse_true_placeholder() {
        let res = verify_with_kernel("anything", "True", "trivial");
        assert!(matches!(res, Err(VerifyError::Failed(msg)) if msg.contains("True")));
    }

    #[test]
    fn test_empty_statement_rejected() {
        let res = verify_with_kernel("test", "  ", "rfl");
        assert!(matches!(res, Err(VerifyError::Failed(_))));
    }
}
