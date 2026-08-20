use reflex_canonical::{CanonicalError, CanonicalWriter};
use reflex_economics::{BetterDirection, UtilityObservation};
use reflex_types::{Digest, ResearchNodeId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use thiserror::Error;

const SHARD_IDENTITY_DOMAIN: &str = "reflex.research-graph.shard.v1";

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum GraphError {
    #[error("node not found: {0}")]
    NodeNotFound(ResearchNodeId),
    #[error("cycle detected in acyclic lineage edge")]
    CycleDetected,
    #[error("invalid graph evidence: {0}")]
    InvalidEvidence(String),
    #[error("invalid graph value: {0}")]
    InvalidValue(String),
    #[error("duplicate node: {0}")]
    DuplicateNode(ResearchNodeId),
    #[error("shard parent mismatch: expected {expected:?}, got {actual:?}")]
    ParentMismatch {
        expected: Option<Digest>,
        actual: Option<Digest>,
    },
    #[error("shard identity mismatch: expected {expected}, got {actual}")]
    IdentityMismatch { expected: Digest, actual: Digest },
    #[error("canonical graph encoding failed: {0}")]
    Canonical(String),
}

impl From<CanonicalError> for GraphError {
    fn from(error: CanonicalError) -> Self {
        Self::Canonical(error.to_string())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

impl EdgeType {
    pub fn must_be_acyclic(self) -> bool {
        matches!(
            self,
            Self::GeneratedFrom
                | Self::GeneralizedFrom
                | Self::EnabledBy
                | Self::TrainedFrom
                | Self::ReplacedBy
        )
    }

    fn canonical_tag(self) -> u8 {
        match self {
            Self::GeneratedFrom => 0,
            Self::GeneralizedFrom => 1,
            Self::EnabledBy => 2,
            Self::RetrievedBy => 3,
            Self::TrainedFrom => 4,
            Self::InspiredBy => 5,
            Self::ReplacedBy => 6,
            Self::AppliedTo => 7,
        }
    }

    fn credit_factor(self) -> f64 {
        match self {
            Self::GeneratedFrom | Self::GeneralizedFrom | Self::EnabledBy => 1.0,
            Self::TrainedFrom | Self::AppliedTo => 0.8,
            Self::RetrievedBy => 0.6,
            Self::InspiredBy => 0.4,
            Self::ReplacedBy => 0.25,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Replaceability {
    Essential,
    Interchangeable { equivalent_paths: u32 },
    Supporting,
}

impl Replaceability {
    fn factor(self) -> Result<f64, GraphError> {
        match self {
            Self::Essential => Ok(1.0),
            Self::Interchangeable { equivalent_paths } if equivalent_paths > 0 => {
                Ok(1.0 / f64::from(equivalent_paths))
            }
            Self::Interchangeable { .. } => Err(GraphError::InvalidValue(
                "interchangeable edge requires at least one equivalent path".into(),
            )),
            Self::Supporting => Ok(0.5),
        }
    }

    fn encode(self, out: &mut CanonicalWriter<'_>) -> Result<(), CanonicalError> {
        match self {
            Self::Essential => out.write_u8(0),
            Self::Interchangeable { equivalent_paths } => {
                out.write_u8(1)?;
                out.write_u32(equivalent_paths)
            }
            Self::Supporting => out.write_u8(2),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResearchNode {
    pub id: ResearchNodeId,
    pub kind: String,
    pub birth_generation: u32,
    pub provenance_evidence: Digest,
    pub creator_state: Digest,
    pub model_state: Digest,
    pub knowledge_state: Digest,
}

impl ResearchNode {
    fn validate(&self) -> Result<(), GraphError> {
        if self.id.digest() == &Digest::ZERO {
            return Err(GraphError::InvalidEvidence("zero research node id".into()));
        }
        if self.kind.trim().is_empty() || self.kind != self.kind.trim() {
            return Err(GraphError::InvalidValue(
                "research node kind must be non-empty canonical text".into(),
            ));
        }
        require_digest(self.provenance_evidence, "node provenance")?;
        require_digest(self.creator_state, "node creator state")?;
        require_digest(self.model_state, "node model state")?;
        require_digest(self.knowledge_state, "node knowledge state")?;
        Ok(())
    }

    fn encode(&self, out: &mut CanonicalWriter<'_>) -> Result<(), CanonicalError> {
        out.write_digest(self.id.digest())?;
        out.write_str(self.kind.trim())?;
        out.write_u32(self.birth_generation)?;
        out.write_digest(&self.provenance_evidence)?;
        out.write_digest(&self.creator_state)?;
        out.write_digest(&self.model_state)?;
        out.write_digest(&self.knowledge_state)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResearchEdge {
    pub source: ResearchNodeId,
    pub target: ResearchNodeId,
    pub edge_type: EdgeType,
    pub replaceability: Replaceability,
    pub causal_confidence: f64,
    pub evidence: Digest,
}

impl ResearchEdge {
    fn validate(&self) -> Result<(), GraphError> {
        if self.source.digest() == &Digest::ZERO || self.target.digest() == &Digest::ZERO {
            return Err(GraphError::InvalidEvidence("zero edge endpoint".into()));
        }
        require_digest(self.evidence, "edge evidence")?;
        self.replaceability.factor()?;
        if !self.causal_confidence.is_finite() || !(0.0..=1.0).contains(&self.causal_confidence) {
            return Err(GraphError::InvalidValue(
                "causal confidence must be finite and in [0, 1]".into(),
            ));
        }
        Ok(())
    }

    fn encode(&self, out: &mut CanonicalWriter<'_>) -> Result<(), CanonicalError> {
        out.write_digest(self.source.digest())?;
        out.write_digest(self.target.digest())?;
        out.write_u8(self.edge_type.canonical_tag())?;
        self.replaceability.encode(out)?;
        out.write_f64(self.causal_confidence)?;
        out.write_digest(&self.evidence)
    }

    fn sort_key(
        &self,
    ) -> (
        ResearchNodeId,
        ResearchNodeId,
        EdgeType,
        Replaceability,
        u64,
        Digest,
    ) {
        (
            self.source,
            self.target,
            self.edge_type,
            self.replaceability,
            if self.causal_confidence == 0.0 {
                0
            } else {
                self.causal_confidence.to_bits()
            },
            self.evidence,
        )
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GraphShard {
    pub digest: Digest,
    pub nodes: Vec<ResearchNode>,
    pub edges: Vec<ResearchEdge>,
    pub parent_shard: Option<Digest>,
}

impl GraphShard {
    pub fn build(
        nodes: Vec<ResearchNode>,
        edges: Vec<ResearchEdge>,
        parent_shard: Option<Digest>,
    ) -> Result<Self, GraphError> {
        let mut shard = Self {
            digest: Digest::ZERO,
            nodes,
            edges,
            parent_shard,
        };
        shard.digest = shard.identity()?;
        Ok(shard)
    }

    pub fn identity(&self) -> Result<Digest, GraphError> {
        if self.nodes.is_empty() && self.edges.is_empty() {
            return Err(GraphError::InvalidValue("empty graph shard".into()));
        }
        if self.parent_shard == Some(Digest::ZERO) {
            return Err(GraphError::InvalidEvidence(
                "zero parent shard digest".into(),
            ));
        }
        let mut node_ids = BTreeSet::new();
        for node in &self.nodes {
            node.validate()?;
            if !node_ids.insert(node.id) {
                return Err(GraphError::DuplicateNode(node.id));
            }
        }
        let mut edge_ids = BTreeSet::new();
        for edge in &self.edges {
            edge.validate()?;
            if !edge_ids.insert(edge.sort_key()) {
                return Err(GraphError::InvalidEvidence(
                    "duplicate research edge".into(),
                ));
            }
        }
        let mut nodes: Vec<_> = self.nodes.iter().collect();
        nodes.sort_unstable_by_key(|node| node.id);
        let mut edges: Vec<_> = self.edges.iter().collect();
        edges.sort_unstable_by_key(|edge| edge.sort_key());
        let mut bytes = Vec::new();
        let mut out = CanonicalWriter::new(&mut bytes);
        out.write_str(SHARD_IDENTITY_DOMAIN)?;
        match self.parent_shard {
            Some(parent) => {
                out.write_u8(1)?;
                out.write_digest(&parent)?;
            }
            None => out.write_u8(0)?,
        }
        let node_count = nodes.len();
        out.write_u32(
            u32::try_from(node_count).map_err(|_| CanonicalError::LengthOverflow {
                length: node_count,
                limit: 32,
            })?,
        )?;
        for node in nodes {
            node.encode(&mut out)?;
        }
        let edge_count = edges.len();
        out.write_u32(
            u32::try_from(edge_count).map_err(|_| CanonicalError::LengthOverflow {
                length: edge_count,
                limit: 32,
            })?,
        )?;
        for edge in edges {
            edge.encode(&mut out)?;
        }
        Ok(Digest::hash_blake3(&bytes))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PropagatedCredit {
    pub node_id: ResearchNodeId,
    pub measure: Option<CreditMeasure>,
    pub direct_value: f64,
    pub descendant_value: f64,
    pub option_value: f64,
    pub source_observations: Vec<Digest>,
    pub causal_confidence: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreditMeasure {
    pub evaluator: reflex_types::EvaluatorId,
    pub metric: reflex_types::MetricId,
    pub unit: reflex_types::UnitId,
    pub population: Digest,
    pub direction: BetterDirection,
    pub restricted_work: bool,
}

impl CreditMeasure {
    fn from_observation(observation: &UtilityObservation) -> Self {
        Self {
            evaluator: observation.evaluator,
            metric: observation.metric,
            unit: observation.unit,
            population: observation.population,
            direction: observation.direction,
            restricted_work: observation.restricted_work,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreditConfig {
    pub discount_factor: f64,
    pub option_value_fraction: f64,
    pub max_depth: usize,
    pub max_iterations: usize,
}

impl Default for CreditConfig {
    fn default() -> Self {
        Self {
            discount_factor: 0.5,
            option_value_fraction: 0.1,
            max_depth: 8,
            max_iterations: 32,
        }
    }
}

impl CreditConfig {
    fn validate(&self) -> Result<(), GraphError> {
        if !self.discount_factor.is_finite() || !(0.0..=1.0).contains(&self.discount_factor) {
            return Err(GraphError::InvalidValue(
                "discount factor must be finite and in [0, 1]".into(),
            ));
        }
        if !self.option_value_fraction.is_finite()
            || !(0.0..=1.0).contains(&self.option_value_fraction)
        {
            return Err(GraphError::InvalidValue(
                "option-value fraction must be finite and in [0, 1]".into(),
            ));
        }
        if self.max_depth == 0 || self.max_iterations == 0 {
            return Err(GraphError::InvalidValue(
                "credit traversal bounds must be non-zero".into(),
            ));
        }
        Ok(())
    }
}

pub struct ResearchGraph {
    shards: Vec<GraphShard>,
    nodes: BTreeMap<ResearchNodeId, ResearchNode>,
    outgoing_edges: BTreeMap<ResearchNodeId, Vec<ResearchEdge>>,
    incoming_edges: BTreeMap<ResearchNodeId, Vec<ResearchEdge>>,
    propagated: BTreeMap<ResearchNodeId, PropagatedCredit>,
}

impl ResearchGraph {
    pub fn new() -> Self {
        Self {
            shards: Vec::new(),
            nodes: BTreeMap::new(),
            outgoing_edges: BTreeMap::new(),
            incoming_edges: BTreeMap::new(),
            propagated: BTreeMap::new(),
        }
    }

    pub fn append_shard(&mut self, shard: GraphShard) -> Result<Digest, GraphError> {
        let expected_parent = self.shards.last().map(|previous| previous.digest);
        if shard.parent_shard != expected_parent {
            return Err(GraphError::ParentMismatch {
                expected: expected_parent,
                actual: shard.parent_shard,
            });
        }
        let expected_identity = shard.identity()?;
        if shard.digest != expected_identity {
            return Err(GraphError::IdentityMismatch {
                expected: expected_identity,
                actual: shard.digest,
            });
        }
        let shard_node_ids: BTreeSet<_> = shard.nodes.iter().map(|node| node.id).collect();
        for node in &shard.nodes {
            if self.nodes.contains_key(&node.id) {
                return Err(GraphError::DuplicateNode(node.id));
            }
        }
        let mut staged_outgoing: BTreeMap<ResearchNodeId, Vec<ResearchEdge>> = BTreeMap::new();
        for edge in &shard.edges {
            if !self.nodes.contains_key(&edge.source) && !shard_node_ids.contains(&edge.source) {
                return Err(GraphError::NodeNotFound(edge.source));
            }
            if !self.nodes.contains_key(&edge.target) && !shard_node_ids.contains(&edge.target) {
                return Err(GraphError::NodeNotFound(edge.target));
            }
            if edge.edge_type.must_be_acyclic()
                && would_cycle(
                    &self.outgoing_edges,
                    &staged_outgoing,
                    edge.source,
                    edge.target,
                )
            {
                return Err(GraphError::CycleDetected);
            }
            staged_outgoing
                .entry(edge.source)
                .or_default()
                .push(edge.clone());
        }

        for node in &shard.nodes {
            self.nodes.insert(node.id, node.clone());
        }
        let mut touched_sources = BTreeSet::new();
        let mut touched_targets = BTreeSet::new();
        for edge in &shard.edges {
            touched_sources.insert(edge.source);
            touched_targets.insert(edge.target);
            self.outgoing_edges
                .entry(edge.source)
                .or_default()
                .push(edge.clone());
            self.incoming_edges
                .entry(edge.target)
                .or_default()
                .push(edge.clone());
        }
        for source in touched_sources {
            self.outgoing_edges
                .get_mut(&source)
                .expect("source was inserted")
                .sort_unstable_by_key(ResearchEdge::sort_key);
        }
        for target in touched_targets {
            self.incoming_edges
                .get_mut(&target)
                .expect("target was inserted")
                .sort_unstable_by_key(ResearchEdge::sort_key);
        }
        self.shards.push(shard);
        Ok(expected_identity)
    }

    pub fn from_shards(shards: impl IntoIterator<Item = GraphShard>) -> Result<Self, GraphError> {
        let mut graph = Self::new();
        for shard in shards {
            graph.append_shard(shard)?;
        }
        Ok(graph)
    }

    pub fn get_node(&self, id: ResearchNodeId) -> Option<&ResearchNode> {
        self.nodes.get(&id)
    }

    pub fn traverse_ancestors(
        &self,
        start: ResearchNodeId,
        max_depth: usize,
        budget: usize,
    ) -> Result<Vec<ResearchNodeId>, GraphError> {
        if !self.nodes.contains_key(&start) {
            return Err(GraphError::NodeNotFound(start));
        }
        Ok(traverse(
            &self.incoming_edges,
            start,
            max_depth,
            budget,
            |edge| edge.source,
        ))
    }

    pub fn traverse_descendants(
        &self,
        start: ResearchNodeId,
        max_depth: usize,
        budget: usize,
    ) -> Result<Vec<ResearchNodeId>, GraphError> {
        if !self.nodes.contains_key(&start) {
            return Err(GraphError::NodeNotFound(start));
        }
        Ok(traverse(
            &self.outgoing_edges,
            start,
            max_depth,
            budget,
            |edge| edge.target,
        ))
    }

    pub fn propagate_delayed_credit(
        &mut self,
        config: &CreditConfig,
        observations: &[UtilityObservation],
    ) -> Result<BTreeMap<ResearchNodeId, PropagatedCredit>, GraphError> {
        config.validate()?;
        let mut sources: BTreeMap<ResearchNodeId, Vec<(f64, Vec<Digest>)>> = BTreeMap::new();
        let mut measure = None;
        for observation in observations {
            let observation_measure = CreditMeasure::from_observation(observation);
            if let Some(expected) = measure {
                if expected != observation_measure {
                    return Err(GraphError::InvalidValue(
                        "credit observations must share evaluator, metric, unit, population, direction, and restriction class".into(),
                    ));
                }
            } else {
                measure = Some(observation_measure);
            }
            let value = observation
                .value
                .as_f64()
                .map_err(|error| GraphError::InvalidValue(error.to_string()))?;
            if !value.is_finite() {
                return Err(GraphError::InvalidValue(
                    "non-finite utility observation".into(),
                ));
            }
            if !self.nodes.contains_key(&observation.subject) {
                return Err(GraphError::NodeNotFound(observation.subject));
            }
            if observation.evaluator.digest() == &Digest::ZERO
                || observation.metric.digest() == &Digest::ZERO
                || observation.unit.digest() == &Digest::ZERO
                || observation.population == Digest::ZERO
                || observation.observed_at_generation.digest() == &Digest::ZERO
            {
                return Err(GraphError::InvalidEvidence(
                    "utility observation identity fields must be non-zero".into(),
                ));
            }
            if observation.evidence.is_empty() || observation.evidence.contains(&Digest::ZERO) {
                return Err(GraphError::InvalidEvidence(
                    "propagated utility requires non-zero source evidence".into(),
                ));
            }
            let unique_evidence: BTreeSet<_> = observation.evidence.iter().copied().collect();
            if unique_evidence.len() != observation.evidence.len() {
                return Err(GraphError::InvalidEvidence(
                    "utility observation contains duplicate evidence".into(),
                ));
            }
            let directed_value = match observation.direction {
                BetterDirection::HigherIsBetter => value,
                BetterDirection::LowerIsBetter => -value,
            };
            sources
                .entry(observation.subject)
                .or_default()
                .push((directed_value, observation.evidence.clone()));
        }
        let mut credits: BTreeMap<_, _> = self
            .nodes
            .values()
            .map(|node| {
                (
                    node.id,
                    PropagatedCredit {
                        node_id: node.id,
                        measure,
                        direct_value: 0.0,
                        descendant_value: 0.0,
                        option_value: 0.0,
                        source_observations: Vec::new(),
                        causal_confidence: 0.0,
                    },
                )
            })
            .collect();
        let mut confidence_mass: BTreeMap<ResearchNodeId, (f64, f64)> = BTreeMap::new();
        for (source, source_values) in sources {
            for (utility, evidence) in source_values {
                let source_credit = credits
                    .get_mut(&source)
                    .ok_or(GraphError::NodeNotFound(source))?;
                source_credit.direct_value += utility;
                source_credit
                    .source_observations
                    .extend(evidence.iter().copied());
                let mut frontier = VecDeque::from([(source, 0usize, 1.0, 1.0)]);
                let mut iterations = 0usize;
                while let Some((current, depth, path_weight, path_confidence)) =
                    frontier.pop_front()
                {
                    if depth >= config.max_depth || iterations >= config.max_iterations {
                        continue;
                    }
                    iterations += 1;
                    let Some(parents) = self.incoming_edges.get(&current) else {
                        continue;
                    };
                    for edge in parents {
                        let edge_weight = config.discount_factor
                            * edge.edge_type.credit_factor()
                            * edge.replaceability.factor()?;
                        let weight = path_weight * edge_weight;
                        let confidence = path_confidence * edge.causal_confidence;
                        let contribution = utility * weight * confidence;
                        let credit = credits
                            .get_mut(&edge.source)
                            .ok_or(GraphError::NodeNotFound(edge.source))?;
                        credit.descendant_value += contribution;
                        credit.source_observations.extend(evidence.iter().copied());
                        let mass = confidence_mass.entry(edge.source).or_default();
                        mass.0 += contribution.abs() * confidence;
                        mass.1 += contribution.abs();
                        frontier.push_back((edge.source, depth + 1, weight, confidence));
                    }
                }
            }
        }
        for (node_id, credit) in &mut credits {
            credit.source_observations.sort_unstable();
            credit.source_observations.dedup();
            credit.option_value = credit.descendant_value.max(0.0) * config.option_value_fraction;
            if let Some((weighted, total)) = confidence_mass.get(node_id)
                && *total > 0.0
            {
                credit.causal_confidence = weighted / total;
            }
        }
        self.propagated = credits.clone();
        Ok(credits)
    }

    pub fn propagated_credit(&self, id: ResearchNodeId) -> Option<&PropagatedCredit> {
        self.propagated.get(&id)
    }

    pub fn reconstruct_from_shards(&self) -> (Vec<ResearchNode>, Vec<ResearchEdge>) {
        let nodes = self.nodes.values().cloned().collect();
        let mut edges: Vec<_> = self
            .shards
            .iter()
            .flat_map(|shard| shard.edges.iter().cloned())
            .collect();
        edges.sort_unstable_by_key(ResearchEdge::sort_key);
        (nodes, edges)
    }
}

impl Default for ResearchGraph {
    fn default() -> Self {
        Self::new()
    }
}

fn require_digest(digest: Digest, field: &str) -> Result<(), GraphError> {
    if digest == Digest::ZERO {
        Err(GraphError::InvalidEvidence(format!(
            "zero digest for {field}"
        )))
    } else {
        Ok(())
    }
}

fn would_cycle(
    outgoing: &BTreeMap<ResearchNodeId, Vec<ResearchEdge>>,
    staged: &BTreeMap<ResearchNodeId, Vec<ResearchEdge>>,
    source: ResearchNodeId,
    target: ResearchNodeId,
) -> bool {
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([target]);
    while let Some(current) = queue.pop_front() {
        if current == source {
            return true;
        }
        if !seen.insert(current) {
            continue;
        }
        if let Some(edges) = outgoing.get(&current) {
            for edge in edges {
                if edge.edge_type.must_be_acyclic() {
                    queue.push_back(edge.target);
                }
            }
        }
        if let Some(edges) = staged.get(&current) {
            for edge in edges {
                if edge.edge_type.must_be_acyclic() {
                    queue.push_back(edge.target);
                }
            }
        }
    }
    false
}

fn traverse(
    edges_by_node: &BTreeMap<ResearchNodeId, Vec<ResearchEdge>>,
    start: ResearchNodeId,
    max_depth: usize,
    budget: usize,
    endpoint: impl Fn(&ResearchEdge) -> ResearchNodeId,
) -> Vec<ResearchNodeId> {
    let mut output = Vec::new();
    let mut queue = VecDeque::from([(start, 0usize)]);
    let mut seen = BTreeSet::from([start]);
    while let Some((current, depth)) = queue.pop_front() {
        if output.len() >= budget || depth >= max_depth {
            continue;
        }
        if let Some(edges) = edges_by_node.get(&current) {
            for edge in edges {
                if output.len() >= budget {
                    break;
                }
                let next = endpoint(edge);
                if seen.insert(next) {
                    output.push(next);
                    queue.push_back((next, depth + 1));
                }
            }
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use reflex_economics::{BetterDirection, ConfidenceClass, RationalOrFloat};
    use reflex_types::{EvaluatorId, GenerationId, MetricId, UnitId};

    fn digest(seed: &[u8]) -> Digest {
        Digest::hash_blake3(seed)
    }

    fn node(seed: &[u8]) -> ResearchNode {
        ResearchNode {
            id: ResearchNodeId::from_digest(digest(seed)),
            kind: "theorem".into(),
            birth_generation: 1,
            provenance_evidence: digest(b"provenance"),
            creator_state: digest(b"creator"),
            model_state: digest(b"model-or-not-applicable"),
            knowledge_state: digest(b"knowledge"),
        }
    }

    fn edge(source: ResearchNodeId, target: ResearchNodeId) -> ResearchEdge {
        ResearchEdge {
            source,
            target,
            edge_type: EdgeType::EnabledBy,
            replaceability: Replaceability::Essential,
            causal_confidence: 1.0,
            evidence: digest(b"edge-evidence"),
        }
    }

    fn observation(subject: ResearchNodeId, value: f64, evidence: Digest) -> UtilityObservation {
        UtilityObservation {
            subject,
            evaluator: EvaluatorId::from_digest(digest(b"evaluator")),
            metric: MetricId::from_digest(digest(b"reuse")),
            value: RationalOrFloat::Float(value),
            unit: UnitId::from_digest(digest(b"uses")),
            direction: BetterDirection::HigherIsBetter,
            population: digest(b"population"),
            evidence: vec![evidence],
            confidence: ConfidenceClass::ObservedExact,
            observed_at_generation: GenerationId::from_digest(digest(b"generation")),
            restricted_work: false,
        }
    }

    #[test]
    fn shard_identity_is_order_independent_and_reconstructable() {
        let a = node(b"a");
        let b = node(b"b");
        let shard_ab =
            GraphShard::build(vec![a.clone(), b.clone()], vec![edge(a.id, b.id)], None).unwrap();
        let shard_ba =
            GraphShard::build(vec![b, a], vec![shard_ab.edges[0].clone()], None).unwrap();
        assert_eq!(shard_ab.digest, shard_ba.digest);
        let graph = ResearchGraph::from_shards([shard_ab.clone()]).unwrap();
        let rebuilt = ResearchGraph::from_shards([shard_ab]).unwrap();
        assert_eq!(
            graph.reconstruct_from_shards(),
            rebuilt.reconstruct_from_shards()
        );
    }

    #[test]
    fn append_is_atomic_and_never_silently_drops_cycles() {
        let a = node(b"a");
        let b = node(b"b");
        let shard = GraphShard::build(
            vec![a.clone(), b.clone()],
            vec![edge(a.id, b.id), edge(b.id, a.id)],
            None,
        )
        .unwrap();
        let mut graph = ResearchGraph::new();
        assert_eq!(graph.append_shard(shard), Err(GraphError::CycleDetected));
        assert!(graph.get_node(a.id).is_none());
        assert!(graph.reconstruct_from_shards().1.is_empty());
    }

    #[test]
    fn append_rejects_zero_evidence_and_identity_edits() {
        let mut invalid = node(b"invalid");
        invalid.provenance_evidence = Digest::ZERO;
        assert!(matches!(
            GraphShard::build(vec![invalid], vec![], None),
            Err(GraphError::InvalidEvidence(_))
        ));
        let valid = node(b"valid");
        let mut shard = GraphShard::build(vec![valid], vec![], None).unwrap();
        shard.digest = digest(b"forged");
        assert!(matches!(
            ResearchGraph::new().append_shard(shard),
            Err(GraphError::IdentityMismatch { .. })
        ));
    }

    #[test]
    fn delayed_credit_uses_type_replaceability_confidence_and_evidence() {
        let a = node(b"a");
        let b = node(b"b");
        let child = node(b"child");
        let mut first = edge(a.id, child.id);
        first.replaceability = Replaceability::Interchangeable {
            equivalent_paths: 2,
        };
        first.causal_confidence = 0.5;
        let mut second = edge(b.id, child.id);
        second.edge_type = EdgeType::InspiredBy;
        second.replaceability = Replaceability::Supporting;
        let shard = GraphShard::build(
            vec![a.clone(), b.clone(), child.clone()],
            vec![first, second],
            None,
        )
        .unwrap();
        let mut graph = ResearchGraph::from_shards([shard]).unwrap();
        let source = digest(b"source-observation");
        let credits = graph
            .propagate_delayed_credit(
                &CreditConfig {
                    discount_factor: 0.5,
                    option_value_fraction: 0.1,
                    max_depth: 4,
                    max_iterations: 32,
                },
                &[observation(child.id, 100.0, source)],
            )
            .unwrap();
        assert!((credits[&a.id].descendant_value - 12.5).abs() < 1e-9);
        assert!((credits[&b.id].descendant_value - 10.0).abs() < 1e-9);
        assert_eq!(credits[&a.id].source_observations, vec![source]);
        assert_eq!(credits[&child.id].direct_value, 100.0);
        assert_eq!(
            credits[&child.id].measure.unwrap().metric,
            MetricId::from_digest(digest(b"reuse"))
        );
    }

    #[test]
    fn credit_rejects_mixed_utility_measures() {
        let child = node(b"child");
        let shard = GraphShard::build(vec![child.clone()], vec![], None).unwrap();
        let mut graph = ResearchGraph::from_shards([shard]).unwrap();
        let first = observation(child.id, 1.0, digest(b"one"));
        let mut second = observation(child.id, 2.0, digest(b"two"));
        second.metric = MetricId::from_digest(digest(b"different-metric"));
        assert!(matches!(
            graph.propagate_delayed_credit(&CreditConfig::default(), &[first, second]),
            Err(GraphError::InvalidValue(_))
        ));
    }

    #[test]
    fn theory_cycles_are_explicitly_bounded() {
        let a = node(b"a");
        let b = node(b"b");
        let mut ab = edge(a.id, b.id);
        ab.edge_type = EdgeType::InspiredBy;
        let mut ba = edge(b.id, a.id);
        ba.edge_type = EdgeType::InspiredBy;
        let shard = GraphShard::build(vec![a.clone(), b.clone()], vec![ab, ba], None).unwrap();
        let mut graph = ResearchGraph::from_shards([shard]).unwrap();
        let credits = graph
            .propagate_delayed_credit(
                &CreditConfig {
                    max_depth: 3,
                    max_iterations: 3,
                    ..CreditConfig::default()
                },
                &[observation(b.id, 10.0, digest(b"cycle-source"))],
            )
            .unwrap();
        assert!(credits[&a.id].descendant_value.is_finite());
        assert!(credits[&b.id].descendant_value.is_finite());
    }

    #[test]
    fn traversal_is_deterministic_and_budgeted() {
        let a = node(b"a");
        let b = node(b"b");
        let c = node(b"c");
        let shard = GraphShard::build(
            vec![c.clone(), a.clone(), b.clone()],
            vec![edge(a.id, c.id), edge(b.id, c.id)],
            None,
        )
        .unwrap();
        let graph = ResearchGraph::from_shards([shard]).unwrap();
        let first = graph.traverse_ancestors(c.id, 2, 1).unwrap();
        assert_eq!(first.len(), 1);
        assert_eq!(first, graph.traverse_ancestors(c.id, 2, 1).unwrap());
    }
}
