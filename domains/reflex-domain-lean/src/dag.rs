//! Proof DAG mining for bounded M2A subset (P13.4).

use reflex_types::Digest;
use serde::{Deserialize, Serialize};
use std::str::FromStr;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProofDagEdge {
    pub from_state: Digest,
    pub to_state: Digest,
    pub tactic: String,
    pub kernel_certified: bool,
    pub replay_ref: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProofDag {
    pub subset_id: String,
    pub edges: Vec<ProofDagEdge>,
    pub viable_action_count: usize,
    pub unknown_count: usize,
    pub dead_count: usize,
}

/// DAG mining requires the Project Reflex worker and kernel replay authority.
/// A list of family names is not evidence and must never be expanded into
/// invented states, tactics, or replay references.
pub fn mine_subset_dag(_subset_id: &str, _families: &[&str]) -> Result<ProofDag, String> {
    Err(
        "VerifierUnavailable: proof-DAG mining requires externally kernel-certified Project Reflex search evidence"
            .to_string(),
    )
}

/// Validate an imported DAG before any edge is treated as viable supervision.
pub fn validate_imported_dag(dag: &ProofDag) -> Result<(), String> {
    if dag.subset_id.trim().is_empty() {
        return Err("proof DAG has an empty registered subset id".to_string());
    }
    if dag.edges.is_empty() {
        return Err("proof DAG contains no externally observed edges".to_string());
    }
    for (index, edge) in dag.edges.iter().enumerate() {
        if !edge.kernel_certified {
            return Err(format!(
                "proof DAG edge {index} is not kernel-certified and cannot be viable"
            ));
        }
        if edge.from_state == Digest::ZERO || edge.to_state == Digest::ZERO {
            return Err(format!("proof DAG edge {index} has an unbound state id"));
        }
        if edge.tactic.trim().is_empty() {
            return Err(format!("proof DAG edge {index} has an empty tactic"));
        }
        let replay = Digest::from_str(&edge.replay_ref).map_err(|error| {
            format!("proof DAG edge {index} has invalid replay digest: {error}")
        })?;
        if replay == Digest::ZERO {
            return Err(format!("proof DAG edge {index} has a zero replay digest"));
        }
    }
    if dag.viable_action_count == 0 || dag.viable_action_count > dag.edges.len() {
        return Err(
            "proof DAG viable-action count does not reconcile to certified edges".to_string(),
        );
    }
    Ok(())
}

pub fn register_subset_before_search(subset_id: &str) -> Digest {
    Digest::hash_blake3(format!("m2a-subset-{subset_id}").as_bytes())
}

pub fn coverage_report(dag: &ProofDag) -> String {
    format!(
        "subset={} viable={} unknown={} dead={} edges={}",
        dag.subset_id,
        dag.viable_action_count,
        dag.unknown_count,
        dag.dead_count,
        dag.edges.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mining_without_external_evidence_is_unavailable() {
        let error = mine_subset_dag("test", &["algebra", "ring"]).unwrap_err();
        assert!(error.contains("VerifierUnavailable"));
    }

    #[test]
    fn uncertified_edge_is_never_viable() {
        let dag = ProofDag {
            subset_id: "registered-subset".to_string(),
            edges: vec![ProofDagEdge {
                from_state: Digest::hash_blake3(b"from"),
                to_state: Digest::hash_blake3(b"to"),
                tactic: "intro".to_string(),
                kernel_certified: false,
                replay_ref: Digest::hash_blake3(b"replay").to_string(),
            }],
            viable_action_count: 1,
            unknown_count: 0,
            dead_count: 0,
        };
        assert!(
            validate_imported_dag(&dag)
                .unwrap_err()
                .contains("not kernel-certified")
        );
    }

    #[test]
    fn certified_import_requires_content_addressed_replay() {
        let dag = ProofDag {
            subset_id: "registered-subset".to_string(),
            edges: vec![ProofDagEdge {
                from_state: Digest::hash_blake3(b"from"),
                to_state: Digest::hash_blake3(b"to"),
                tactic: "intro".to_string(),
                kernel_certified: true,
                replay_ref: "not-a-digest".to_string(),
            }],
            viable_action_count: 1,
            unknown_count: 0,
            dead_count: 0,
        };
        assert!(
            validate_imported_dag(&dag)
                .unwrap_err()
                .contains("invalid replay digest")
        );
    }
}
