//! M2A evidence importer — reads historical artifacts, never synthesizes claims.

use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;

const M2A_VERSION: u32 = 1;
const M2A_EXPECTED_BUNDLES: usize = 30;
const M2A_EXPECTED_ARMS: usize = 180;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M2aArmRecord {
    pub bundle_id: u32,
    pub arm_id: u32,
    pub policy: String,
    pub solved: bool,
    pub replay_ref: String,
    pub accepted_attempt: bool,
    pub action_count: u64,
    pub cpu_ns: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M2aBundle {
    pub bundle_id: u32,
    pub arms: Vec<M2aArmRecord>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M2aManifest {
    pub version: u32,
    pub total_bundles: usize,
    pub total_arms: usize,
    pub bundles: Vec<M2aBundle>,
    pub incidents: Vec<M2aIncident>,
    /// One entry per preregistered neural-vs-uniform comparison.
    pub registered_neural_vs_uniform: Vec<bool>,
    pub authoritative_outcome: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M2aIncident {
    pub incident_id: String,
    pub kind: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M2aReconstructionResult {
    pub total_bundles: usize,
    pub total_arms: usize,
    pub uniform_solve_rate: f64,
    pub mlp_2607_solve_rate: f64,
    pub mlp_99902_solve_rate: f64,
    pub neural_vs_uniform_pass_count: usize,
    pub registered_outcome: String,
    pub total_action_count: u64,
    pub total_cpu_ns: u64,
    pub accepted_attempts: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum M2aImportError {
    #[error("evidence not found: {0}")]
    NotFound(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("reconstruction mismatch: {0}")]
    Mismatch(String),
}

pub fn default_evidence_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evidence/lean-m2a")
}

pub fn load_manifest(base: &Path) -> Result<M2aManifest, M2aImportError> {
    let path = base.join("manifest.json");
    let data = std::fs::read_to_string(&path)
        .map_err(|e| M2aImportError::NotFound(format!("{path:?}: {e}")))?;
    serde_json::from_str(&data).map_err(|e| M2aImportError::Parse(e.to_string()))
}

pub fn reconstruct_m2a_from_evidence(
    base: &Path,
) -> Result<M2aReconstructionResult, M2aImportError> {
    let manifest = load_manifest(base)?;
    validate_manifest_shape(&manifest)?;

    let mut uniform_solved = 0usize;
    let mut uniform_total = 0usize;
    let mut mlp2607_solved = 0usize;
    let mut mlp2607_total = 0usize;
    let mut mlp99902_solved = 0usize;
    let mut mlp99902_total = 0usize;
    let mut total_action_count = 0u64;
    let mut total_cpu_ns = 0u64;
    let mut accepted_attempts = 0usize;
    let mut seen_bundles = HashSet::new();
    let mut observed_arms = 0usize;

    for bundle in &manifest.bundles {
        if !seen_bundles.insert(bundle.bundle_id) {
            return Err(M2aImportError::Mismatch(format!(
                "duplicate bundle id {}",
                bundle.bundle_id
            )));
        }
        let mut seen_arms = HashSet::new();
        for arm in &bundle.arms {
            observed_arms = observed_arms
                .checked_add(1)
                .ok_or_else(|| M2aImportError::Mismatch("arm count overflow".to_string()))?;
            if arm.bundle_id != bundle.bundle_id || !seen_arms.insert(arm.arm_id) {
                return Err(M2aImportError::Mismatch(format!(
                    "invalid or duplicate arm identity {}:{}",
                    arm.bundle_id, arm.arm_id
                )));
            }
            if arm.solved {
                let replay = Digest::from_str(&arm.replay_ref).map_err(|e| {
                    M2aImportError::Mismatch(format!(
                        "solved arm {}:{} has invalid replay reference: {e}",
                        arm.bundle_id, arm.arm_id
                    ))
                })?;
                if replay == Digest::ZERO {
                    return Err(M2aImportError::Mismatch(format!(
                        "solved arm {}:{} has zero replay reference",
                        arm.bundle_id, arm.arm_id
                    )));
                }
            }
            total_action_count = total_action_count
                .checked_add(arm.action_count)
                .ok_or_else(|| {
                    M2aImportError::Mismatch("total action count overflow".to_string())
                })?;
            total_cpu_ns = total_cpu_ns
                .checked_add(arm.cpu_ns)
                .ok_or_else(|| M2aImportError::Mismatch("total CPU overflow".to_string()))?;
            accepted_attempts = accepted_attempts
                .checked_add(usize::from(arm.accepted_attempt))
                .ok_or_else(|| {
                    M2aImportError::Mismatch("accepted attempt count overflow".into())
                })?;
            if arm.policy.trim().is_empty() {
                return Err(M2aImportError::Mismatch(format!(
                    "arm {}:{} has an empty policy",
                    arm.bundle_id, arm.arm_id
                )));
            }
            match arm.policy.as_str() {
                "uniform" => {
                    uniform_total += 1;
                    if arm.solved {
                        uniform_solved += 1;
                    }
                }
                "mlp_2607" => {
                    mlp2607_total += 1;
                    if arm.solved {
                        mlp2607_solved += 1;
                    }
                }
                "mlp_99902" => {
                    mlp99902_total += 1;
                    if arm.solved {
                        mlp99902_solved += 1;
                    }
                }
                _ => {}
            }
        }
    }

    if manifest.total_bundles != manifest.bundles.len() {
        return Err(M2aImportError::Mismatch(format!(
            "declared bundle count {} != observed {}",
            manifest.total_bundles,
            manifest.bundles.len()
        )));
    }
    if manifest.total_arms != observed_arms {
        return Err(M2aImportError::Mismatch(format!(
            "declared arm count {} != observed {}",
            manifest.total_arms, observed_arms
        )));
    }
    if uniform_total == 0 || mlp2607_total == 0 || mlp99902_total == 0 {
        return Err(M2aImportError::Mismatch(
            "M2A evidence is missing one or more registered comparison policies".to_string(),
        ));
    }

    // Rates use floor-millis rounding per policy arm count
    let rate_floor3 = |solved: usize, total: usize| -> f64 {
        if total == 0 {
            return 0.0;
        }
        ((solved as u64 * 1000) / total as u64) as f64 / 1000.0
    };

    let result = M2aReconstructionResult {
        total_bundles: manifest.total_bundles,
        total_arms: manifest.total_arms,
        uniform_solve_rate: rate_floor3(uniform_solved, uniform_total),
        mlp_2607_solve_rate: rate_floor3(mlp2607_solved, mlp2607_total),
        mlp_99902_solve_rate: rate_floor3(mlp99902_solved, mlp99902_total),
        neural_vs_uniform_pass_count: manifest
            .registered_neural_vs_uniform
            .iter()
            .filter(|passed| **passed)
            .count(),
        registered_outcome: manifest.authoritative_outcome.clone(),
        total_action_count,
        total_cpu_ns,
        accepted_attempts,
    };

    validate_against_authoritative(base, &result)?;
    Ok(result)
}

fn validate_manifest_shape(manifest: &M2aManifest) -> Result<(), M2aImportError> {
    if manifest.version != M2A_VERSION {
        return Err(M2aImportError::Mismatch(format!(
            "unsupported M2A manifest version {}",
            manifest.version
        )));
    }
    if manifest.total_bundles != M2A_EXPECTED_BUNDLES
        || manifest.bundles.len() != M2A_EXPECTED_BUNDLES
        || manifest.total_arms != M2A_EXPECTED_ARMS
    {
        return Err(M2aImportError::Mismatch(format!(
            "complete M2A evidence requires {M2A_EXPECTED_BUNDLES} bundles and {M2A_EXPECTED_ARMS} arms"
        )));
    }
    if manifest.registered_neural_vs_uniform.is_empty()
        || manifest.authoritative_outcome.trim().is_empty()
    {
        return Err(M2aImportError::Mismatch(
            "M2A registration or authoritative outcome is missing".to_string(),
        ));
    }
    let mut incident_ids = HashSet::new();
    for incident in &manifest.incidents {
        if incident.incident_id.trim().is_empty()
            || incident.kind.trim().is_empty()
            || incident.description.trim().is_empty()
            || !incident_ids.insert(incident.incident_id.as_str())
        {
            return Err(M2aImportError::Mismatch(
                "M2A incident history is incomplete or has duplicate identities".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_against_authoritative(
    base: &Path,
    result: &M2aReconstructionResult,
) -> Result<(), M2aImportError> {
    let path = base.join("authoritative.json");
    let data = std::fs::read_to_string(&path).map_err(|e| {
        M2aImportError::NotFound(format!(
            "authoritative comparison unavailable at {path:?}: {e}"
        ))
    })?;
    let auth: M2aReconstructionResult =
        serde_json::from_str(&data).map_err(|e| M2aImportError::Parse(e.to_string()))?;

    if result.total_bundles != auth.total_bundles {
        return Err(M2aImportError::Mismatch(format!(
            "bundle count {} != {}",
            result.total_bundles, auth.total_bundles
        )));
    }
    if result.total_arms != auth.total_arms {
        return Err(M2aImportError::Mismatch(format!(
            "arm count {} != {}",
            result.total_arms, auth.total_arms
        )));
    }
    if result.uniform_solve_rate != auth.uniform_solve_rate {
        return Err(M2aImportError::Mismatch(format!(
            "uniform rate {} != {}",
            result.uniform_solve_rate, auth.uniform_solve_rate
        )));
    }
    if result.registered_outcome != auth.registered_outcome {
        return Err(M2aImportError::Mismatch(format!(
            "outcome {} != {}",
            result.registered_outcome, auth.registered_outcome
        )));
    }
    if result.mlp_2607_solve_rate != auth.mlp_2607_solve_rate
        || result.mlp_99902_solve_rate != auth.mlp_99902_solve_rate
        || result.neural_vs_uniform_pass_count != auth.neural_vs_uniform_pass_count
        || result.total_action_count != auth.total_action_count
        || result.total_cpu_ns != auth.total_cpu_ns
        || result.accepted_attempts != auth.accepted_attempts
    {
        return Err(M2aImportError::Mismatch(
            "reconstructed metrics do not exactly match authoritative evidence".to_string(),
        ));
    }
    Ok(())
}

/// Public entry — reads evidence or fails closed (no hardcoded constants).
pub fn reconstruct_m2a_result() -> Result<M2aReconstructionResult, M2aImportError> {
    reconstruct_m2a_from_evidence(&default_evidence_dir())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_historical_inputs_are_unavailable_not_pass() {
        let result = reconstruct_m2a_from_evidence(Path::new("/missing/reflex/m2a/evidence"));
        assert!(matches!(result, Err(M2aImportError::NotFound(_))));
    }

    #[test]
    fn incomplete_manifest_cannot_be_shaped_like_m2a() {
        let manifest = M2aManifest {
            version: M2A_VERSION,
            total_bundles: 1,
            total_arms: 1,
            bundles: Vec::new(),
            incidents: Vec::new(),
            registered_neural_vs_uniform: vec![true],
            authoritative_outcome: "negative".to_string(),
        };
        assert!(
            validate_manifest_shape(&manifest)
                .unwrap_err()
                .to_string()
                .contains("30 bundles and 180 arms")
        );
    }
}
