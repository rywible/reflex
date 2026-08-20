use reflex_types::{CandidateId, Digest, StateId};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::collections::HashMap;

/// Resource cost retained for a receipt-backed viable edge.
///
/// Action and CPU minima are independent: two certified routes may minimize
/// different resources. `None` means that the producer did not provide a
/// measured CPU cost; it is never rewritten to zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViableCost {
    pub best_actions_to_go: u32,
    pub best_cpu_ns_to_go: Option<u64>,
}

/// A cyclic component retained outside the acyclic cost projection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofScc {
    pub component_id: u32,
    pub states: Vec<StateId>,
}

/// One edge that remains inside a cyclic proof component.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofCycleEdge {
    pub component_id: u32,
    pub state_id: StateId,
    pub candidate_id: CandidateId,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProofDagError {
    #[error("proof DAG edge limit exceeded: limit={limit}")]
    EdgeLimitExceeded { limit: usize },
}

/// Deterministically ordered record suitable for a bounded external sorted
/// run. Callers can merge several runs without depending on `HashMap` order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofEdgeRecord {
    pub state_id: StateId,
    pub candidate_id: CandidateId,
    pub knowledge: CandidateKnowledge,
    pub cost: Option<ViableCost>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateKnowledge {
    Viable {
        best_actions_to_go: u32,
        receipts: SmallVec<[Digest; 2]>,
    },
    KnownDead {
        certificate: Digest,
    },
    Unknown,
    Invalid {
        code: u32,
    },
}

pub struct ProofDag {
    edges: HashMap<(StateId, CandidateId), CandidateKnowledge>,
    viable_costs: HashMap<(StateId, CandidateId), ViableCost>,
    cycle_components: Vec<ProofScc>,
    cycle_edges: Vec<ProofCycleEdge>,
    max_edges: usize,
}

impl ProofDag {
    pub fn new() -> Self {
        Self::with_max_edges(usize::MAX)
    }

    pub fn with_max_edges(max_edges: usize) -> Self {
        Self {
            edges: HashMap::new(),
            viable_costs: HashMap::new(),
            cycle_components: Vec::new(),
            cycle_edges: Vec::new(),
            max_edges,
        }
    }

    fn reserve_edge(&self, key: (StateId, CandidateId)) -> Result<(), ProofDagError> {
        if !self.edges.contains_key(&key) && self.edges.len() >= self.max_edges {
            return Err(ProofDagError::EdgeLimitExceeded {
                limit: self.max_edges,
            });
        }
        Ok(())
    }

    pub fn mark_viable(&mut self, state_id: StateId, candidate_id: CandidateId, cost_to_go: u32) {
        self.mark_viable_with_receipt(state_id, candidate_id, cost_to_go, None);
    }

    pub fn mark_viable_with_receipt(
        &mut self,
        state_id: StateId,
        candidate_id: CandidateId,
        cost_to_go: u32,
        receipt: Option<Digest>,
    ) {
        self.try_mark_viable_with_cost(state_id, candidate_id, cost_to_go, None, receipt)
            .expect("legacy proof-DAG mutation requires available edge capacity");
    }

    pub fn try_mark_viable_with_cost(
        &mut self,
        state_id: StateId,
        candidate_id: CandidateId,
        actions_to_go: u32,
        cpu_ns_to_go: Option<u64>,
        receipt: Option<Digest>,
    ) -> Result<(), ProofDagError> {
        let Some(receipt) = receipt.filter(|receipt| *receipt != Digest::ZERO) else {
            return Ok(());
        };
        let key = (state_id, candidate_id);
        self.reserve_edge(key)?;
        let entry = self
            .edges
            .entry(key)
            .or_insert_with(|| CandidateKnowledge::Viable {
                best_actions_to_go: actions_to_go,
                receipts: SmallVec::new(),
            });
        match entry {
            CandidateKnowledge::Viable {
                best_actions_to_go,
                receipts,
            } => {
                if actions_to_go < *best_actions_to_go {
                    *best_actions_to_go = actions_to_go;
                }
                if !receipts.contains(&receipt) {
                    receipts.push(receipt);
                }
            }
            _ => {
                *entry = CandidateKnowledge::Viable {
                    best_actions_to_go: actions_to_go,
                    receipts: smallvec::smallvec![receipt],
                };
            }
        }
        let cost = self.viable_costs.entry(key).or_insert(ViableCost {
            best_actions_to_go: actions_to_go,
            best_cpu_ns_to_go: cpu_ns_to_go,
        });
        cost.best_actions_to_go = cost.best_actions_to_go.min(actions_to_go);
        if let Some(cpu_ns) = cpu_ns_to_go {
            cost.best_cpu_ns_to_go = Some(
                cost.best_cpu_ns_to_go
                    .map_or(cpu_ns, |known| known.min(cpu_ns)),
            );
        }
        Ok(())
    }

    pub fn attach_receipt(
        &mut self,
        state_id: StateId,
        candidate_id: CandidateId,
        receipt: Digest,
    ) {
        if receipt == Digest::ZERO {
            return;
        }
        if let Some(CandidateKnowledge::Viable { receipts, .. }) =
            self.edges.get_mut(&(state_id, candidate_id))
            && !receipts.contains(&receipt)
        {
            receipts.push(receipt);
        }
    }

    pub fn mark_known_dead(&mut self, state_id: StateId, candidate_id: CandidateId, cert: Digest) {
        self.try_mark_known_dead(state_id, candidate_id, cert)
            .expect("legacy proof-DAG mutation requires available edge capacity");
    }

    pub fn try_mark_known_dead(
        &mut self,
        state_id: StateId,
        candidate_id: CandidateId,
        cert: Digest,
    ) -> Result<(), ProofDagError> {
        if cert == Digest::ZERO {
            return Ok(());
        }
        self.reserve_edge((state_id, candidate_id))?;
        self.edges.insert(
            (state_id, candidate_id),
            CandidateKnowledge::KnownDead { certificate: cert },
        );
        Ok(())
    }

    pub fn mark_invalid(&mut self, state_id: StateId, candidate_id: CandidateId, code: u32) {
        self.try_mark_invalid(state_id, candidate_id, code)
            .expect("legacy proof-DAG mutation requires available edge capacity");
    }

    pub fn try_mark_invalid(
        &mut self,
        state_id: StateId,
        candidate_id: CandidateId,
        code: u32,
    ) -> Result<(), ProofDagError> {
        self.reserve_edge((state_id, candidate_id))?;
        self.edges.insert(
            (state_id, candidate_id),
            CandidateKnowledge::Invalid { code },
        );
        Ok(())
    }

    pub fn get_knowledge(
        &self,
        state_id: StateId,
        candidate_id: CandidateId,
    ) -> Option<&CandidateKnowledge> {
        self.edges.get(&(state_id, candidate_id))
    }

    pub fn mark_verified_route(&mut self, edges: &[(StateId, CandidateId, u32, Digest)]) {
        for &(state_id, candidate_id, cost, receipt) in edges {
            self.mark_viable_with_receipt(state_id, candidate_id, cost, Some(receipt));
        }
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn viable_cost(&self, state_id: StateId, candidate_id: CandidateId) -> Option<ViableCost> {
        self.viable_costs.get(&(state_id, candidate_id)).copied()
    }

    pub fn set_cycles(&mut self, mut components: Vec<ProofScc>, mut edges: Vec<ProofCycleEdge>) {
        for component in &mut components {
            component.states.sort_unstable();
            component.states.dedup();
        }
        components.sort_unstable_by_key(|component| component.component_id);
        edges.sort_unstable_by_key(|edge| (edge.component_id, edge.state_id, edge.candidate_id));
        edges.dedup();
        self.cycle_components = components;
        self.cycle_edges = edges;
    }

    pub fn cycle_components(&self) -> &[ProofScc] {
        &self.cycle_components
    }

    pub fn cycle_edges(&self) -> &[ProofCycleEdge] {
        &self.cycle_edges
    }

    /// Returns at most `limit` records in canonical key order. A caller that
    /// reaches the limit knows it must spill/merge another sorted run.
    pub fn sorted_edge_run(&self, limit: usize) -> Vec<ProofEdgeRecord> {
        let mut records: Vec<_> = self
            .edges
            .iter()
            .map(|(&(state_id, candidate_id), knowledge)| ProofEdgeRecord {
                state_id,
                candidate_id,
                knowledge: knowledge.clone(),
                cost: self.viable_costs.get(&(state_id, candidate_id)).copied(),
            })
            .collect();
        records.sort_unstable_by_key(|record| (record.state_id, record.candidate_id));
        records.truncate(limit);
        records
    }

    pub fn viable_edges(&self) -> impl Iterator<Item = ((StateId, CandidateId), u32, &[Digest])> {
        self.edges.iter().filter_map(|(key, knowledge)| {
            if let CandidateKnowledge::Viable {
                best_actions_to_go,
                receipts,
            } = knowledge
                && receipts.iter().any(|receipt| *receipt != Digest::ZERO)
            {
                Some((*key, *best_actions_to_go, receipts.as_slice()))
            } else {
                None
            }
        })
    }
}

impl Default for ProofDag {
    fn default() -> Self {
        Self::new()
    }
}

/// Compute softmax targets over viable labels (cost-to-go supervision).
pub fn viable_target(
    labels: &[CandidateKnowledge],
    temperature: f32,
    out: &mut [f32],
) -> Result<bool, String> {
    if out.len() < labels.len() {
        return Err(format!(
            "output buffer length {} < candidate count {}",
            out.len(),
            labels.len()
        ));
    }
    out.fill(0.0);
    let mut max_logit = f32::NEG_INFINITY;
    for label in labels {
        if let CandidateKnowledge::Viable {
            best_actions_to_go,
            receipts,
        } = label
            && receipts.iter().any(|receipt| *receipt != Digest::ZERO)
        {
            max_logit = max_logit.max(-(*best_actions_to_go as f32) / temperature);
        }
    }
    if !max_logit.is_finite() {
        return Ok(false);
    }
    let mut sum = 0.0;
    for (index, label) in labels.iter().enumerate() {
        if let CandidateKnowledge::Viable {
            best_actions_to_go,
            receipts,
        } = label
            && receipts.iter().any(|receipt| *receipt != Digest::ZERO)
        {
            let logit = -(*best_actions_to_go as f32) / temperature;
            let prob = (logit - max_logit).exp();
            out[index] = prob;
            sum += prob;
        }
    }
    if sum > 0.0 {
        for v in out.iter_mut().take(labels.len()) {
            if *v > 0.0 {
                *v /= sum;
            }
        }
    }
    Ok(sum > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proof_dag_mark_viable() {
        let mut dag = ProofDag::new();
        let sid = StateId::from_digest(Digest::hash_blake3(b"s1"));
        let cid = CandidateId::from_digest(Digest::hash_blake3(b"c1"));
        dag.mark_viable(sid, cid, 5);
        assert!(dag.get_knowledge(sid, cid).is_none());
        dag.mark_viable_with_receipt(sid, cid, 3, Some(Digest::hash_blake3(b"rcpt")));
        match dag.get_knowledge(sid, cid).unwrap() {
            CandidateKnowledge::Viable {
                best_actions_to_go,
                receipts,
            } => {
                assert_eq!(*best_actions_to_go, 3);
                assert_eq!(receipts.len(), 1);
            }
            _ => panic!("Expected Viable"),
        }
    }

    #[test]
    fn test_viable_target_calculation() {
        let labels = vec![
            CandidateKnowledge::Viable {
                best_actions_to_go: 2,
                receipts: smallvec::smallvec![Digest::hash_blake3(b"r1")],
            },
            CandidateKnowledge::Viable {
                best_actions_to_go: 4,
                receipts: smallvec::smallvec![Digest::hash_blake3(b"r2")],
            },
            CandidateKnowledge::Unknown,
            CandidateKnowledge::KnownDead {
                certificate: Digest::ZERO,
            },
        ];
        let mut targets = vec![0.0f32; 4];
        let has_supervision = viable_target(&labels, 1.0, &mut targets).unwrap();
        assert!(has_supervision);
        assert!(targets[0] > targets[1]);
        assert_eq!(targets[2], 0.0);
        assert_eq!(targets[3], 0.0);
        assert!((targets[0] + targets[1] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_viable_requires_receipt() {
        let mut dag = ProofDag::new();
        let sid = StateId::from_digest(Digest::hash_blake3(b"s"));
        let cid = CandidateId::from_digest(Digest::hash_blake3(b"c"));
        let receipt = Digest::hash_blake3(b"verified");
        dag.mark_viable_with_receipt(sid, cid, 1, Some(receipt));
        match dag.get_knowledge(sid, cid).unwrap() {
            CandidateKnowledge::Viable { receipts, .. } => {
                assert_eq!(receipts[0], receipt);
            }
            _ => panic!("expected viable with receipt"),
        }
    }

    #[test]
    fn test_zero_receipts_and_certificates_are_not_evidence() {
        let mut dag = ProofDag::new();
        let sid = StateId::from_digest(Digest::hash_blake3(b"s"));
        let cid = CandidateId::from_digest(Digest::hash_blake3(b"c"));
        dag.mark_viable_with_receipt(sid, cid, 1, Some(Digest::ZERO));
        dag.mark_known_dead(sid, cid, Digest::ZERO);
        assert!(dag.get_knowledge(sid, cid).is_none());

        let labels = [CandidateKnowledge::Viable {
            best_actions_to_go: 1,
            receipts: smallvec::smallvec![Digest::ZERO],
        }];
        let mut target = [1.0];
        assert!(!viable_target(&labels, 1.0, &mut target).unwrap());
        assert_eq!(target, [0.0]);
    }

    #[test]
    fn test_viable_cost_keeps_independent_action_and_cpu_minima() {
        let mut dag = ProofDag::new();
        let sid = StateId::from_digest(Digest::hash_blake3(b"state"));
        let cid = CandidateId::from_digest(Digest::hash_blake3(b"candidate"));
        dag.try_mark_viable_with_cost(
            sid,
            cid,
            3,
            Some(90),
            Some(Digest::hash_blake3(b"short-route")),
        )
        .unwrap();
        dag.try_mark_viable_with_cost(
            sid,
            cid,
            5,
            Some(40),
            Some(Digest::hash_blake3(b"fast-route")),
        )
        .unwrap();
        assert_eq!(
            dag.viable_cost(sid, cid),
            Some(ViableCost {
                best_actions_to_go: 3,
                best_cpu_ns_to_go: Some(40),
            })
        );
    }

    #[test]
    fn test_bounded_dag_fails_closed_and_exports_sorted_run() {
        let mut dag = ProofDag::with_max_edges(1);
        let sid = StateId::from_digest(Digest::hash_blake3(b"state"));
        let first = CandidateId::from_digest(Digest::hash_blake3(b"first"));
        let second = CandidateId::from_digest(Digest::hash_blake3(b"second"));
        dag.try_mark_invalid(sid, first, 1).unwrap();
        assert_eq!(
            dag.try_mark_invalid(sid, second, 2),
            Err(ProofDagError::EdgeLimitExceeded { limit: 1 })
        );
        let run = dag.sorted_edge_run(1);
        assert_eq!(run.len(), 1);
        assert_eq!(run[0].candidate_id, first);
    }
}
