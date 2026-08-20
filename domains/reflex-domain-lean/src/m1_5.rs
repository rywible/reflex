//! M1.5 historical result importer. This module never synthesizes evidence.

use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M15ModelRecord {
    pub name: String,
    pub parameter_count: usize,
    pub solve_rate: f64,
    pub checkpoint_digest: Digest,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M15Manifest {
    pub version: u32,
    pub models: Vec<M15ModelRecord>,
    pub capacity_floor: usize,
    pub training_signal_conclusion: String,
}

#[allow(dead_code)]
pub fn default_evidence_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evidence/lean-m1.5")
}

pub fn load_m15_manifest(base: &Path) -> Result<M15Manifest, String> {
    let path = base.join("manifest.json");
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let manifest: M15Manifest = serde_json::from_str(&data).map_err(|e| e.to_string())?;
    if manifest.models.is_empty() {
        return Err("incomplete M1.5 evidence: no model records".to_string());
    }
    for model in &manifest.models {
        if model.name.trim().is_empty()
            || model.parameter_count == 0
            || !model.solve_rate.is_finite()
            || !(0.0..=1.0).contains(&model.solve_rate)
            || model.checkpoint_digest == Digest::ZERO
        {
            return Err(format!(
                "incomplete M1.5 evidence: invalid model record '{}'",
                model.name
            ));
        }
    }
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_external_evidence_is_explicit() {
        let err = load_m15_manifest(Path::new("/missing/reflex/m1.5/evidence")).unwrap_err();
        assert!(!err.is_empty());
    }
}
