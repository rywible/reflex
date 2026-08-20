//! M2B confirmatory matrix importer. Evidence is external or explicitly unavailable.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const M2B_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M2bCell {
    pub cell_id: String,
    pub family: String,
    pub stratum: String,
    pub expected: bool,
    pub observed: bool,
    pub accepted: bool,
    pub excluded: bool,
    pub exclusion_reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct M2bMatrix {
    pub version: u32,
    pub cells: Vec<M2bCell>,
    pub m2a_preserved: bool,
}

pub fn default_evidence_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../evidence/lean-m2b")
}

pub fn load_m2b_matrix(base: &Path) -> Result<M2bMatrix, String> {
    let path = base.join("matrix.json");
    let data = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let matrix = serde_json::from_str(&data).map_err(|e| e.to_string())?;
    reconcile_matrix(&matrix)?;
    Ok(matrix)
}

pub fn reconcile_matrix(matrix: &M2bMatrix) -> Result<(), String> {
    if matrix.version != M2B_VERSION {
        return Err(format!("unsupported M2B matrix version {}", matrix.version));
    }
    if matrix.cells.is_empty() {
        return Err("M2B matrix incomplete: no cells".to_string());
    }
    let mut cell_ids = HashSet::new();
    for cell in &matrix.cells {
        if cell.cell_id.trim().is_empty()
            || cell.family.trim().is_empty()
            || cell.stratum.trim().is_empty()
        {
            return Err("M2B cell identity, family, and stratum must be nonempty".to_string());
        }
        if !cell_ids.insert(cell.cell_id.as_str()) {
            return Err(format!("duplicate M2B cell id {}", cell.cell_id));
        }
        if cell.accepted && (!cell.expected || !cell.observed) {
            return Err(format!(
                "cell {} accepted without being expected and observed",
                cell.cell_id
            ));
        }
        if cell.accepted && cell.excluded {
            return Err(format!("cell {} both accepted and excluded", cell.cell_id));
        }
        if cell.accepted && cell.exclusion_reason.is_some() {
            return Err(format!(
                "accepted cell {} has an exclusion reason",
                cell.cell_id
            ));
        }
        if cell.excluded
            && cell
                .exclusion_reason
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
        {
            return Err(format!("cell {} excluded without a reason", cell.cell_id));
        }
        if cell.expected && !cell.accepted && !cell.excluded {
            return Err(format!(
                "M2B matrix incomplete: expected cell {} is neither accepted nor excluded",
                cell.cell_id
            ));
        }
    }
    if !matrix.m2a_preserved {
        return Err("M2A result must be preserved alongside M2B".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn incomplete_expected_cell_is_not_a_pass() {
        let matrix = M2bMatrix {
            version: 1,
            cells: vec![M2bCell {
                cell_id: "synthetic-test-cell".to_string(),
                family: "synthetic-test-family".to_string(),
                stratum: "synthetic-test-stratum".to_string(),
                expected: true,
                observed: true,
                accepted: false,
                excluded: false,
                exclusion_reason: None,
            }],
            m2a_preserved: true,
        };
        assert!(
            reconcile_matrix(&matrix)
                .unwrap_err()
                .contains("incomplete")
        );
    }

    #[test]
    fn unexpected_observation_cannot_be_accepted() {
        let matrix = M2bMatrix {
            version: M2B_VERSION,
            cells: vec![M2bCell {
                cell_id: "exploratory-cell".to_string(),
                family: "family".to_string(),
                stratum: "stratum".to_string(),
                expected: false,
                observed: true,
                accepted: true,
                excluded: false,
                exclusion_reason: None,
            }],
            m2a_preserved: true,
        };
        assert!(
            reconcile_matrix(&matrix)
                .unwrap_err()
                .contains("expected and observed")
        );
    }

    #[test]
    fn duplicate_cells_cannot_inflate_confirmation() {
        let cell = M2bCell {
            cell_id: "registered-cell".to_string(),
            family: "family".to_string(),
            stratum: "stratum".to_string(),
            expected: true,
            observed: true,
            accepted: true,
            excluded: false,
            exclusion_reason: None,
        };
        let matrix = M2bMatrix {
            version: M2B_VERSION,
            cells: vec![cell.clone(), cell],
            m2a_preserved: true,
        };
        assert!(
            reconcile_matrix(&matrix)
                .unwrap_err()
                .contains("duplicate M2B cell")
        );
    }
}
