//! M2B dataset variants (P13.5).

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DatasetVariant {
    pub variant_id: String,
    pub supervision_change: String,
    pub feature_change: Option<String>,
    pub label_flip_routes: Vec<String>,
}

pub fn m2b_dataset_variants() -> Vec<DatasetVariant> {
    vec![
        DatasetVariant {
            variant_id: "baseline".to_string(),
            supervision_change: "single_route".to_string(),
            feature_change: None,
            label_flip_routes: vec![],
        },
        DatasetVariant {
            variant_id: "dag_supervision".to_string(),
            supervision_change: "multi_proof_dag".to_string(),
            feature_change: None,
            // Populated only from externally kernel-certified route evidence.
            label_flip_routes: vec![],
        },
    ]
}
