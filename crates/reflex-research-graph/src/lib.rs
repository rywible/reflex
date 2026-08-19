use reflex_types::{Digest, ResearchNodeId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    #[error("node not found: {0}")]
    NodeNotFound(ResearchNodeId),
    #[error("cycle detected in acyclic lineage edge")]
    CycleDetected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EdgeType {
    GeneratedFrom,
    GeneralizedFrom,
    EnabledBy,
    RetrievedBy,
    TrainedFrom,
    InspiredBy,
    ReplacedBy,
    AppliedTo,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResearchNode {
    pub id: ResearchNodeId,
    pub kind: String,
    pub birth_generation: u32,
    pub direct_utility: f64,
    pub propagated_credit: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResearchEdge {
    pub source: ResearchNodeId,
    pub target: ResearchNodeId,
    pub edge_type: EdgeType,
    pub weight: f64,
    pub evidence: Digest,
}

pub struct ResearchGraph {
    nodes: HashMap<ResearchNodeId, ResearchNode>,
    outgoing_edges: HashMap<ResearchNodeId, Vec<ResearchEdge>>,
    incoming_edges: HashMap<ResearchNodeId, Vec<ResearchEdge>>,
}

impl ResearchGraph {
    pub fn new() -> Self {
        Self {
            nodes: HashMap::new(),
            outgoing_edges: HashMap::new(),
            incoming_edges: HashMap::new(),
        }
    }

    pub fn add_node(&mut self, node: ResearchNode) {
        let id = node.id;
        self.nodes.insert(id, node);
    }

    pub fn add_edge(&mut self, edge: ResearchEdge) {
        self.outgoing_edges
            .entry(edge.source)
            .or_default()
            .push(edge.clone());
        self.incoming_edges
            .entry(edge.target)
            .or_default()
            .push(edge);
    }

    pub fn get_node(&self, id: ResearchNodeId) -> Option<&ResearchNode> {
        self.nodes.get(&id)
    }

    pub fn propagate_delayed_credit(&mut self, discount_factor: f64, max_depth: usize) {
        // Collect direct utility values
        let mut credits: HashMap<ResearchNodeId, f64> = HashMap::new();

        for (&node_id, node) in &self.nodes {
            if node.direct_utility != 0.0 {
                // BFS backwards through incoming edges (parents)
                let mut queue = vec![(node_id, 0, 1.0f64)];
                let mut visited = HashMap::new();

                while let Some((curr, depth, weight)) = queue.pop() {
                    if depth >= max_depth {
                        continue;
                    }
                    if let Some(parents) = self.incoming_edges.get(&curr) {
                        for edge in parents {
                            let parent = edge.source;
                            let new_weight = weight * edge.weight * discount_factor;
                            let val = node.direct_utility * new_weight;
                            *credits.entry(parent).or_insert(0.0) += val;

                            if let std::collections::hash_map::Entry::Vacant(e) =
                                visited.entry(parent)
                            {
                                e.insert(depth + 1);
                                queue.push((parent, depth + 1, new_weight));
                            }
                        }
                    }
                }
            }
        }

        for (node_id, credit) in credits {
            if let Some(node) = self.nodes.get_mut(&node_id) {
                node.propagated_credit = credit;
            }
        }
    }
}

impl Default for ResearchGraph {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_delayed_credit_propagation() {
        let mut graph = ResearchGraph::new();

        let n1_id = ResearchNodeId::from_digest(Digest::hash_blake3(b"theorem_A"));
        let n2_id = ResearchNodeId::from_digest(Digest::hash_blake3(b"lemma_B"));
        let n3_id = ResearchNodeId::from_digest(Digest::hash_blake3(b"application_C"));

        graph.add_node(ResearchNode {
            id: n1_id,
            kind: "Theorem".to_string(),
            birth_generation: 1,
            direct_utility: 0.0,
            propagated_credit: 0.0,
        });

        graph.add_node(ResearchNode {
            id: n2_id,
            kind: "Lemma".to_string(),
            birth_generation: 2,
            direct_utility: 0.0,
            propagated_credit: 0.0,
        });

        graph.add_node(ResearchNode {
            id: n3_id,
            kind: "Application".to_string(),
            birth_generation: 3,
            direct_utility: 100.0, // Saved 100% work
            propagated_credit: 0.0,
        });

        // n1 -> n2 -> n3 (n2 was enabled by n1, n3 was enabled by n2)
        graph.add_edge(ResearchEdge {
            source: n1_id,
            target: n2_id,
            edge_type: EdgeType::EnabledBy,
            weight: 1.0,
            evidence: Digest::ZERO,
        });

        graph.add_edge(ResearchEdge {
            source: n2_id,
            target: n3_id,
            edge_type: EdgeType::EnabledBy,
            weight: 1.0,
            evidence: Digest::ZERO,
        });

        graph.propagate_delayed_credit(0.5, 5);

        // n2 gets 100 * 0.5 = 50.0
        assert_eq!(graph.get_node(n2_id).unwrap().propagated_credit, 50.0);
        // n1 gets 100 * 0.5 * 0.5 = 25.0
        assert_eq!(graph.get_node(n1_id).unwrap().propagated_credit, 25.0);
    }
}
