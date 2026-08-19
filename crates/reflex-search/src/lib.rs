use reflex_domain::{
    CandidateBatchBuilder, CandidateHandle, CandidateIndex, Domain, DomainError, EpisodeArena,
    FeatureBatch, SolvedRoot, StateHandle, TransitionBatch, TransitionOutcome,
};
use reflex_types::{CandidateId, Digest, ModelCheckpointId, StateId};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashMap};
use std::ops::Range;
use std::time::Instant;
use thiserror::Error;

// ── Errors ─────────────────────────────────────────────────────────────

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum SearchError {
    #[error("domain error: {0}")]
    Domain(#[from] DomainError),
    #[error("budget exhausted: {0}")]
    BudgetExhausted(String),
    #[error("non-finite policy score: {0}")]
    NonFiniteScore(u32),
    #[error("policy error: {0}")]
    Policy(#[from] PolicyError),
    #[error("search invariant violation: {0}")]
    InvariantViolation(String),
}

#[derive(Error, Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    #[error("non-finite score: {0}")]
    NonFiniteScore(u32),
    #[error("model inference failed: {0}")]
    InferenceFailed(String),
}

// ── Arena index newtypes ───────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct NodeIndex(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EdgeIndex(pub u32);

// ── Node status (§11.1) ───────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Open,
    Closed,
    Failed,
    ObligationPending,
    ObligationComplete,
}

impl NodeStatus {
    pub fn is_solved(self) -> bool {
        matches!(self, NodeStatus::Closed | NodeStatus::ObligationComplete)
    }
}

// ── Search node (§11.2) ───────────────────────────────────────────────

pub struct SearchNode {
    pub state: StateHandle,
    pub state_id: StateId,
    pub status: NodeStatus,
    pub first_parent: Option<EdgeIndex>,
    pub alternative_parents: SmallVec<[EdgeIndex; 2]>,
    pub best_logical_cost: u32,
    pub depth: u32,
    pub expanded: bool,
    pub candidates: Option<reflex_domain::CandidateBatch>,
    pub and_group_indices: Vec<usize>,
}

// ── Edge ───────────────────────────────────────────────────────────────

pub struct Edge {
    pub parent_node: NodeIndex,
    pub candidate_id: CandidateId,
    pub candidate_handle: CandidateHandle,
    pub and_group_idx: Option<usize>,
}

// ── AND group (§11.1, §11.2) ─────────────────────────────────────────

pub struct AndGroup {
    pub parent_edge: EdgeIndex,
    pub children: Range<usize>,
    pub remaining: u32,
    pub failed: bool,
}

// ── Search arena (§11.2) ──────────────────────────────────────────────

pub struct SearchArena {
    pub nodes: Vec<SearchNode>,
    pub edges: Vec<Edge>,
    pub and_groups: Vec<AndGroup>,
    pub state_to_node: HashMap<StateId, NodeIndex>,
}

impl SearchArena {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            and_groups: Vec::new(),
            state_to_node: HashMap::new(),
        }
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn get_node(&self, idx: NodeIndex) -> Option<&SearchNode> {
        self.nodes.get(idx.0 as usize)
    }

    pub fn add_node(
        &mut self,
        state: StateHandle,
        state_id: StateId,
        status: NodeStatus,
        cost: u32,
        depth: u32,
        parent_edge: Option<EdgeIndex>,
    ) -> NodeIndex {
        let idx = NodeIndex(self.nodes.len() as u32);
        self.nodes.push(SearchNode {
            state,
            state_id,
            status,
            first_parent: parent_edge,
            alternative_parents: SmallVec::new(),
            best_logical_cost: cost,
            depth,
            expanded: false,
            candidates: None,
            and_group_indices: Vec::new(),
        });
        self.state_to_node.insert(state_id, idx);
        idx
    }

    pub fn add_edge(
        &mut self,
        parent_node: NodeIndex,
        candidate_id: CandidateId,
        candidate_handle: CandidateHandle,
    ) -> EdgeIndex {
        let idx = EdgeIndex(self.edges.len() as u32);
        self.edges.push(Edge {
            parent_node,
            candidate_id,
            candidate_handle,
            and_group_idx: None,
        });
        idx
    }

    pub fn add_and_group(
        &mut self,
        parent_edge: EdgeIndex,
        parent_node: NodeIndex,
        children_start: usize,
        num_children: u32,
    ) -> usize {
        let group_idx = self.and_groups.len();
        self.and_groups.push(AndGroup {
            parent_edge,
            children: children_start..(children_start + num_children as usize),
            remaining: num_children,
            failed: false,
        });
        if let Some(edge) = self.edges.get_mut(parent_edge.0 as usize) {
            edge.and_group_idx = Some(group_idx);
        }
        self.nodes[parent_node.0 as usize]
            .and_group_indices
            .push(group_idx);
        group_idx
    }

    pub fn parent_edges(&self, node_idx: NodeIndex) -> SmallVec<[EdgeIndex; 2]> {
        let node = &self.nodes[node_idx.0 as usize];
        let mut edges = SmallVec::new();
        if let Some(fp) = node.first_parent {
            edges.push(fp);
        }
        for &ap in &node.alternative_parents {
            edges.push(ap);
        }
        edges
    }
}

impl Default for SearchArena {
    fn default() -> Self {
        Self::new()
    }
}

// ── Ordered score (§11.3) ─────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct OrderedScore(u32);

impl OrderedScore {
    pub fn bits(&self) -> u32 {
        self.0
    }

    pub fn from_f32(score: f32) -> Result<Self, SearchError> {
        if !score.is_finite() {
            return Err(SearchError::NonFiniteScore(score.to_bits()));
        }
        let normalized = if score == 0.0 { 0.0f32 } else { score };
        let bits = normalized.to_bits();
        let order_key = if (bits & 0x8000_0000) != 0 {
            !bits
        } else {
            bits ^ 0x8000_0000
        };
        Ok(OrderedScore(order_key))
    }
}

// ── Frontier key (§11.3) ──────────────────────────────────────────────

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct FrontierKey {
    pub score_key: OrderedScore,
    pub logical_cost: Reverse<u32>,
    pub depth: Reverse<u32>,
    pub state_id: Reverse<StateId>,
    pub candidate_id: Reverse<CandidateId>,
    pub insertion_seq: Reverse<u64>,
}

impl Ord for FrontierKey {
    fn cmp(&self, other: &Self) -> Ordering {
        self.score_key
            .cmp(&other.score_key)
            .then_with(|| self.logical_cost.cmp(&other.logical_cost))
            .then_with(|| self.depth.cmp(&other.depth))
            .then_with(|| self.state_id.cmp(&other.state_id))
            .then_with(|| self.candidate_id.cmp(&other.candidate_id))
            .then_with(|| self.insertion_seq.cmp(&other.insertion_seq))
    }
}

impl PartialOrd for FrontierKey {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Eq, PartialEq, Debug)]
pub struct FrontierEntry {
    pub key: FrontierKey,
    pub node_idx: NodeIndex,
    pub candidate_local_idx: usize,
}

impl Ord for FrontierEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.key.cmp(&other.key)
    }
}

impl PartialOrd for FrontierEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

// ── Search budget (§11.4) ─────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SearchBudget {
    pub action_budget: u32,
    pub node_budget: u32,
    pub cpu_seconds: f64,
}

impl SearchBudget {
    pub fn default_for_test() -> Self {
        Self {
            action_budget: 10_000,
            node_budget: 50_000,
            cpu_seconds: 30.0,
        }
    }
}

// ── Search stats ───────────────────────────────────────────────────────

pub struct SearchStats {
    pub nodes_expanded: u32,
    pub candidates_scored: u32,
    pub ml_overhead_ns: u64,
    pub search_cpu_ns: u64,
}

// ── Inference telemetry ────────────────────────────────────────────────

pub struct InferenceTelemetry {
    pub cpu_ns: u64,
    pub model_id: ModelCheckpointId,
}

// ── Ranker trait (§11.5) ──────────────────────────────────────────────

pub trait Ranker: Send + Sync {
    fn model_id(&self) -> ModelCheckpointId;
    fn score_batch(
        &self,
        features: &FeatureBatch,
        output: &mut [f32],
        telemetry: &mut InferenceTelemetry,
    ) -> Result<(), PolicyError>;
}

// ── Uniform ranker ─────────────────────────────────────────────────────

pub struct UniformRanker {
    model_id: ModelCheckpointId,
}

impl UniformRanker {
    pub fn new(model_id: ModelCheckpointId) -> Self {
        Self { model_id }
    }
}

impl Ranker for UniformRanker {
    fn model_id(&self) -> ModelCheckpointId {
        self.model_id
    }

    fn score_batch(
        &self,
        features: &FeatureBatch,
        output: &mut [f32],
        _telemetry: &mut InferenceTelemetry,
    ) -> Result<(), PolicyError> {
        for out_val in output.iter_mut().take(features.rows) {
            *out_val = 1.0;
        }
        Ok(())
    }
}

// ── Heuristic ranker ───────────────────────────────────────────────────

pub struct HeuristicRanker<F: Fn(&[f32]) -> f32 + Send + Sync> {
    model_id: ModelCheckpointId,
    func: F,
}

impl<F: Fn(&[f32]) -> f32 + Send + Sync> HeuristicRanker<F> {
    pub fn new(model_id: ModelCheckpointId, func: F) -> Self {
        Self { model_id, func }
    }
}

impl<F: Fn(&[f32]) -> f32 + Send + Sync> Ranker for HeuristicRanker<F> {
    fn model_id(&self) -> ModelCheckpointId {
        self.model_id
    }

    fn score_batch(
        &self,
        features: &FeatureBatch,
        output: &mut [f32],
        _telemetry: &mut InferenceTelemetry,
    ) -> Result<(), PolicyError> {
        for (i, out_val) in output.iter_mut().enumerate().take(features.rows) {
            *out_val = (self.func)(features.row(i));
        }
        Ok(())
    }
}

// ── Candidate knowledge (§11.6) ───────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
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

// ── Proof DAG (§11.6) ─────────────────────────────────────────────────

pub struct ProofDag {
    edges: HashMap<(StateId, CandidateId), CandidateKnowledge>,
}

impl ProofDag {
    pub fn new() -> Self {
        Self {
            edges: HashMap::new(),
        }
    }

    pub fn mark_viable(
        &mut self,
        state_id: StateId,
        candidate_id: CandidateId,
        cost_to_go: u32,
    ) {
        let entry = self
            .edges
            .entry((state_id, candidate_id))
            .or_insert_with(|| CandidateKnowledge::Viable {
                best_actions_to_go: cost_to_go,
                receipts: SmallVec::new(),
            });
        if let CandidateKnowledge::Viable {
            best_actions_to_go, ..
        } = entry
            && cost_to_go < *best_actions_to_go
        {
            *best_actions_to_go = cost_to_go;
        }
    }

    pub fn mark_known_dead(
        &mut self,
        state_id: StateId,
        candidate_id: CandidateId,
        cert: Digest,
    ) {
        self.edges.insert(
            (state_id, candidate_id),
            CandidateKnowledge::KnownDead {
                certificate: cert,
            },
        );
    }

    pub fn mark_invalid(
        &mut self,
        state_id: StateId,
        candidate_id: CandidateId,
        code: u32,
    ) {
        self.edges.insert(
            (state_id, candidate_id),
            CandidateKnowledge::Invalid { code },
        );
    }

    pub fn get_knowledge(
        &self,
        state_id: StateId,
        candidate_id: CandidateId,
    ) -> Option<&CandidateKnowledge> {
        self.edges.get(&(state_id, candidate_id))
    }

    pub fn mark_verified_route(&mut self, edges: &[(StateId, CandidateId, u32)]) {
        for &(state_id, candidate_id, cost) in edges {
            self.mark_viable(state_id, candidate_id, cost);
        }
    }
}

impl Default for ProofDag {
    fn default() -> Self {
        Self::new()
    }
}

// ── Search kernel ──────────────────────────────────────────────────────

pub struct SearchKernel<'a, D: Domain> {
    domain: &'a D,
    ranker: &'a dyn Ranker,
    budget: SearchBudget,
    arena: SearchArena,
    episode_arena: EpisodeArena,
    frontier: BinaryHeap<FrontierEntry>,
    insertion_counter: u64,
    actions_taken: u32,
    stats: SearchStats,
    proof_dag: ProofDag,
}

impl<'a, D: Domain> SearchKernel<'a, D> {
    pub fn new(domain: &'a D, ranker: &'a dyn Ranker, budget: SearchBudget) -> Self {
        Self {
            domain,
            ranker,
            budget,
            arena: SearchArena::new(),
            episode_arena: EpisodeArena::new(),
            frontier: BinaryHeap::new(),
            insertion_counter: 0,
            actions_taken: 0,
            stats: SearchStats {
                nodes_expanded: 0,
                candidates_scored: 0,
                ml_overhead_ns: 0,
                search_cpu_ns: 0,
            },
            proof_dag: ProofDag::new(),
        }
    }

    pub fn run(&mut self, task: &D::Task) -> Result<Option<SolvedRoot>, SearchError> {
        let start_time = Instant::now();

        self.arena = SearchArena::new();
        self.episode_arena.clear();
        self.frontier.clear();
        self.insertion_counter = 0;
        self.actions_taken = 0;
        self.stats = SearchStats {
            nodes_expanded: 0,
            candidates_scored: 0,
            ml_overhead_ns: 0,
            search_cpu_ns: 0,
        };
        self.proof_dag = ProofDag::new();

        let initial_handle = self.domain.initial_state(task, &mut self.episode_arena)?;
        let initial_id = self.domain.state_id(initial_handle, &self.episode_arena)?;

        let root_idx = self
            .arena
            .add_node(initial_handle, initial_id, NodeStatus::Open, 0, 0, None);

        self.expand_node(root_idx)?;

        while let Some(entry) = self.frontier.pop() {
            if self.actions_taken >= self.budget.action_budget {
                return Err(SearchError::BudgetExhausted("action_budget".to_string()));
            }
            if self.arena.node_count() as u32 >= self.budget.node_budget {
                return Err(SearchError::BudgetExhausted("node_budget".to_string()));
            }
            if start_time.elapsed().as_secs_f64() >= self.budget.cpu_seconds {
                return Err(SearchError::BudgetExhausted("cpu_seconds".to_string()));
            }

            let node_idx = entry.node_idx;
            {
                let node = &self.arena.nodes[node_idx.0 as usize];
                if node.status.is_solved() || node.status == NodeStatus::Failed {
                    continue;
                }
            }

            let cand_local_idx = entry.candidate_local_idx;
            let (cand_batch, node_state, node_depth) = {
                let node = &self.arena.nodes[node_idx.0 as usize];
                match &node.candidates {
                    Some(b) => (b.clone(), node.state, node.depth),
                    None => continue,
                }
            };
            if cand_local_idx >= cand_batch.len() {
                continue;
            }

            let cand_id = cand_batch.ids[cand_local_idx];
            let cand_handle = cand_batch.payload_handles[cand_local_idx];

            let selection = vec![CandidateIndex(cand_local_idx)];
            let mut transitions = TransitionBatch::new();
            self.domain.apply_candidates(
                node_state,
                &cand_batch,
                &selection,
                &mut self.episode_arena,
                &mut transitions,
            )?;
            self.actions_taken += 1;

            if let Some(outcome) = transitions.outcomes.into_iter().next() {
                match outcome {
                    TransitionOutcome::Closed => {
                        let _edge_idx = self
                            .arena
                            .add_edge(node_idx, cand_id, cand_handle);
                        self.arena.nodes[node_idx.0 as usize].status = NodeStatus::Closed;
                        self.propagate_closed(node_idx, start_time);
                    }
                    TransitionOutcome::Obligations {
                        and_child_states,
                    } => {
                        let edge_idx = self
                            .arena
                            .add_edge(node_idx, cand_id, cand_handle);
                        let parent_cost = self.arena.nodes[node_idx.0 as usize].best_logical_cost;
                        let next_cost = parent_cost.saturating_add(1);

                        let num_children = and_child_states.len() as u32;
                        let children_start = self.arena.nodes.len();

                        let mut child_indices = Vec::new();
                        for child_handle in and_child_states {
                            let child_id =
                                self.domain.state_id(child_handle, &self.episode_arena)?;

                            if let Some(&existing_idx) = self.arena.state_to_node.get(&child_id) {
                                child_indices.push(existing_idx);
                                let node =
                                    &mut self.arena.nodes[existing_idx.0 as usize];
                                node.alternative_parents.push(edge_idx);
                            } else {
                                let child_idx = self.arena.add_node(
                                    child_handle,
                                    child_id,
                                    NodeStatus::ObligationPending,
                                    next_cost,
                                    node_depth + 1,
                                    Some(edge_idx),
                                );
                                child_indices.push(child_idx);
                            }
                        }

                        self.arena.add_and_group(
                            edge_idx,
                            node_idx,
                            children_start,
                            num_children,
                        );

                        let all_already_solved = !child_indices.is_empty()
                            && child_indices
                                .iter()
                                .all(|&c| self.arena.nodes[c.0 as usize].status.is_solved());

                        if all_already_solved {
                            self.arena.nodes[node_idx.0 as usize].status = NodeStatus::Closed;
                            self.propagate_closed(node_idx, start_time);
                        } else {
                            for &child_idx in &child_indices {
                                let child_status =
                                    self.arena.nodes[child_idx.0 as usize].status;
                                if (child_status == NodeStatus::Open
                                    || child_status == NodeStatus::ObligationPending)
                                    && !self.arena.nodes[child_idx.0 as usize].expanded
                                {
                                    self.expand_node(child_idx)?;
                                }
                            }
                        }
                    }
                    TransitionOutcome::Contradiction => {
                        self.arena
                            .add_edge(node_idx, cand_id, cand_handle);
                        self.arena.nodes[node_idx.0 as usize].status = NodeStatus::Failed;
                        self.propagate_failed(node_idx, start_time);
                    }
                    TransitionOutcome::Invalid { .. } => {}
                    TransitionOutcome::Unresolved { .. } => {}
                }
            }

            if self.arena.nodes[0].status.is_solved() {
                self.stats.search_cpu_ns = start_time.elapsed().as_nanos() as u64;
                let solved_root = self.build_solved_root(initial_handle);
                return Ok(Some(solved_root));
            }
        }

        self.stats.search_cpu_ns = start_time.elapsed().as_nanos() as u64;
        if self.arena.nodes[0].status.is_solved() {
            let solved_root = self.build_solved_root(initial_handle);
            return Ok(Some(solved_root));
        }

        Ok(None)
    }

    fn expand_node(&mut self, node_idx: NodeIndex) -> Result<(), SearchError> {
        self.arena.nodes[node_idx.0 as usize].expanded = true;
        self.stats.nodes_expanded += 1;

        let state_handle = self.arena.nodes[node_idx.0 as usize].state;
        let depth = self.arena.nodes[node_idx.0 as usize].depth;

        let mut builder = CandidateBatchBuilder::new();
        self.domain
            .enumerate_candidates(state_handle, &self.episode_arena, &mut builder)?;
        let batch = builder.build();

        if batch.is_empty() {
            self.arena.nodes[node_idx.0 as usize].status = NodeStatus::Failed;
            self.propagate_failed(node_idx, Instant::now());
            return Ok(());
        }

        let cap = self.domain.capabilities();
        let cols = cap.feature_dimension.max(1);
        let mut features = FeatureBatch::new(batch.len(), cols, cap.feature_schema);
        self.domain.extract_features(
            &[state_handle],
            &batch,
            &self.episode_arena,
            &mut features,
        )?;

        let mut scores = vec![0.0f32; batch.len()];
        let mut telemetry = InferenceTelemetry {
            cpu_ns: 0,
            model_id: self.ranker.model_id(),
        };
        let ml_start = Instant::now();
        self.ranker
            .score_batch(&features, &mut scores, &mut telemetry)?;
        self.stats.ml_overhead_ns += ml_start.elapsed().as_nanos() as u64;
        self.stats.candidates_scored += batch.len() as u32;

        let parent_cost = self.arena.nodes[node_idx.0 as usize].best_logical_cost;
        let parent_state_id = self.arena.nodes[node_idx.0 as usize].state_id;

        for (cand_idx, &score) in scores.iter().enumerate().take(batch.len()) {
            let cand_id = batch.ids[cand_idx];
            let next_cost = parent_cost.saturating_add(1);
            let score_key = OrderedScore::from_f32(score)?;

            let entry = FrontierEntry {
                key: FrontierKey {
                    score_key,
                    logical_cost: Reverse(next_cost),
                    depth: Reverse(depth),
                    state_id: Reverse(parent_state_id),
                    candidate_id: Reverse(cand_id),
                    insertion_seq: Reverse(self.insertion_counter),
                },
                node_idx,
                candidate_local_idx: cand_idx,
            };
            self.insertion_counter += 1;
            self.frontier.push(entry);
        }

        self.arena.nodes[node_idx.0 as usize].candidates = Some(batch);
        Ok(())
    }

    fn propagate_closed(&mut self, closed_idx: NodeIndex, _start_time: Instant) {
        let mut worklist = vec![closed_idx];
        let mut visited = std::collections::HashSet::new();

        while let Some(curr) = worklist.pop() {
            if !visited.insert(curr) {
                continue;
            }

            let parent_edges = self.arena.parent_edges(curr);

            for parent_edge_idx in parent_edges {
                let edge = &self.arena.edges[parent_edge_idx.0 as usize];
                let parent_node_idx = edge.parent_node;

                if let Some(group_idx) = edge.and_group_idx {
                    let group = &self.arena.and_groups[group_idx];
                    if group.failed {
                        continue;
                    }
                    let all_children_closed = (group.children.start..group.children.end)
                        .all(|c| self.arena.nodes[c].status.is_solved());
                    if all_children_closed {
                        let parent_status =
                            self.arena.nodes[parent_node_idx.0 as usize].status;
                        if !parent_status.is_solved() {
                            self.arena.nodes[parent_node_idx.0 as usize].status =
                                NodeStatus::Closed;
                            worklist.push(parent_node_idx);
                        }
                    }
                }
            }
        }
    }

    fn propagate_failed(&mut self, failed_idx: NodeIndex, _start_time: Instant) {
        let mut worklist = vec![failed_idx];
        let mut visited = std::collections::HashSet::new();

        while let Some(curr) = worklist.pop() {
            if !visited.insert(curr) {
                continue;
            }

            let parent_edges = self.arena.parent_edges(curr);

            for parent_edge_idx in parent_edges {
                let edge = &self.arena.edges[parent_edge_idx.0 as usize];
                let parent_node_idx = edge.parent_node;

                if let Some(group_idx) = edge.and_group_idx {
                    self.arena.and_groups[group_idx].failed = true;
                }

                let parent = &self.arena.nodes[parent_node_idx.0 as usize];
                if parent.status == NodeStatus::Failed {
                    continue;
                }

                let all_groups_failed = !parent.and_group_indices.is_empty()
                    && parent
                        .and_group_indices
                        .iter()
                        .all(|&g| self.arena.and_groups[g].failed);

                if all_groups_failed {
                    self.arena.nodes[parent_node_idx.0 as usize].status = NodeStatus::Failed;
                    worklist.push(parent_node_idx);
                }
            }
        }
    }

    fn build_solved_root(&self, initial_handle: StateHandle) -> SolvedRoot {
        let solved_edges = self.extract_solved_edges(NodeIndex(0));
        SolvedRoot {
            root_state: initial_handle,
            solved_edges,
        }
    }

    fn extract_solved_edges(
        &self,
        root_idx: NodeIndex,
    ) -> Vec<(StateHandle, CandidateHandle, Vec<StateHandle>)> {
        let mut edges = Vec::new();
        let mut queue = vec![root_idx];
        let mut visited = std::collections::HashSet::new();

        while let Some(curr) = queue.pop() {
            if !visited.insert(curr) {
                continue;
            }
            let node = &self.arena.nodes[curr.0 as usize];

            for e in &self.arena.edges {
                if e.parent_node != curr {
                    continue;
                }

                if let Some(group_idx) = e.and_group_idx {
                    let group = &self.arena.and_groups[group_idx];
                    if group.failed {
                        continue;
                    }
                    let all_children_solved = (group.children.start..group.children.end)
                        .all(|c| self.arena.nodes[c].status.is_solved());
                    if all_children_solved {
                        let child_handles: Vec<StateHandle> = (group.children.start
                            ..group.children.end)
                            .map(|c| self.arena.nodes[c].state)
                            .collect();
                        edges.push((node.state, e.candidate_handle, child_handles));
                        for c in group.children.start..group.children.end {
                            queue.push(NodeIndex(c as u32));
                        }
                        break;
                    }
                } else if node.status.is_solved() {
                    edges.push((node.state, e.candidate_handle, Vec::new()));
                    break;
                }
            }
        }
        edges
    }

    pub fn arena(&self) -> &SearchArena {
        &self.arena
    }

    pub fn episode_arena(&self) -> &EpisodeArena {
        &self.episode_arena
    }

    pub fn stats(&self) -> &SearchStats {
        &self.stats
    }

    pub fn proof_dag(&self) -> &ProofDag {
        &self.proof_dag
    }
}

// ── Tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ordered_score_monotonicity() {
        let s1 = OrderedScore::from_f32(0.1).unwrap();
        let s2 = OrderedScore::from_f32(0.5).unwrap();
        let s3 = OrderedScore::from_f32(10.0).unwrap();
        assert!(s1 < s2);
        assert!(s2 < s3);

        let zero_pos = OrderedScore::from_f32(0.0).unwrap();
        let zero_neg = OrderedScore::from_f32(-0.0).unwrap();
        assert_eq!(zero_pos, zero_neg);
    }

    #[test]
    fn test_search_arena_basics() {
        let mut arena = SearchArena::new();
        let handle = StateHandle(0);
        let id = StateId::from_digest(Digest::hash_blake3(b"test"));
        let idx = arena.add_node(handle, id, NodeStatus::Open, 0, 0, None);
        assert_eq!(arena.node_count(), 1);
        assert_eq!(arena.get_node(idx).unwrap().status, NodeStatus::Open);
    }

    #[test]
    fn test_frontier_ordering() {
        let k1 = FrontierKey {
            score_key: OrderedScore::from_f32(0.1).unwrap(),
            logical_cost: Reverse(1),
            depth: Reverse(0),
            state_id: Reverse(StateId::from_digest(Digest::hash_blake3(b"a"))),
            candidate_id: Reverse(CandidateId::from_digest(Digest::hash_blake3(b"c1"))),
            insertion_seq: Reverse(0),
        };
        let k2 = FrontierKey {
            score_key: OrderedScore::from_f32(0.5).unwrap(),
            logical_cost: Reverse(1),
            depth: Reverse(0),
            state_id: Reverse(StateId::from_digest(Digest::hash_blake3(b"a"))),
            candidate_id: Reverse(CandidateId::from_digest(Digest::hash_blake3(b"c2"))),
            insertion_seq: Reverse(1),
        };
        assert!(k1 < k2);
    }

    #[test]
    fn test_proof_dag_mark_viable() {
        let mut dag = ProofDag::new();
        let sid = StateId::from_digest(Digest::hash_blake3(b"s1"));
        let cid = CandidateId::from_digest(Digest::hash_blake3(b"c1"));
        dag.mark_viable(sid, cid, 5);
        match dag.get_knowledge(sid, cid).unwrap() {
            CandidateKnowledge::Viable {
                best_actions_to_go, ..
            } => {
                assert_eq!(*best_actions_to_go, 5);
            }
            _ => panic!("Expected Viable"),
        }
        dag.mark_viable(sid, cid, 3);
        match dag.get_knowledge(sid, cid).unwrap() {
            CandidateKnowledge::Viable {
                best_actions_to_go, ..
            } => {
                assert_eq!(*best_actions_to_go, 3);
            }
            _ => panic!("Expected Viable"),
        }
    }

    #[test]
    fn test_budget_default() {
        let b = SearchBudget::default_for_test();
        assert_eq!(b.action_budget, 10_000);
        assert_eq!(b.node_budget, 50_000);
        assert!((b.cpu_seconds - 30.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_node_status_is_solved() {
        assert!(NodeStatus::Closed.is_solved());
        assert!(NodeStatus::ObligationComplete.is_solved());
        assert!(!NodeStatus::Open.is_solved());
        assert!(!NodeStatus::Failed.is_solved());
        assert!(!NodeStatus::ObligationPending.is_solved());
    }

    #[test]
    fn test_and_group_creation() {
        let mut arena = SearchArena::new();
        let h = StateHandle(0);
        let id = StateId::from_digest(Digest::hash_blake3(b"parent"));
        let parent = arena.add_node(h, id, NodeStatus::Open, 0, 0, None);

        let edge = arena.add_edge(
            parent,
            CandidateId::from_digest(Digest::hash_blake3(b"cand")),
            CandidateHandle(0),
        );

        let ch1_id = StateId::from_digest(Digest::hash_blake3(b"ch1"));
        let ch1 = arena.add_node(
            StateHandle(1),
            ch1_id,
            NodeStatus::ObligationPending,
            1,
            1,
            Some(edge),
        );
        let ch2_id = StateId::from_digest(Digest::hash_blake3(b"ch2"));
        let ch2 = arena.add_node(
            StateHandle(2),
            ch2_id,
            NodeStatus::ObligationPending,
            1,
            1,
            Some(edge),
        );

        let group_idx = arena.add_and_group(edge, parent, arena.nodes.len() - 2, 2);
        assert_eq!(arena.and_groups[group_idx].children.start, ch1.0 as usize);
        assert_eq!(arena.and_groups[group_idx].children.end, ch2.0 as usize + 1);
        assert_eq!(arena.and_groups[group_idx].remaining, 2);
    }

    #[test]
    fn test_candidate_knowledge_variants() {
        let viable = CandidateKnowledge::Viable {
            best_actions_to_go: 3,
            receipts: SmallVec::new(),
        };
        let dead = CandidateKnowledge::KnownDead {
            certificate: Digest::hash_blake3(b"cert"),
        };
        let unknown = CandidateKnowledge::Unknown;
        let invalid = CandidateKnowledge::Invalid { code: 42 };

        assert!(matches!(viable, CandidateKnowledge::Viable { .. }));
        assert!(matches!(dead, CandidateKnowledge::KnownDead { .. }));
        assert!(matches!(unknown, CandidateKnowledge::Unknown));
        assert!(matches!(invalid, CandidateKnowledge::Invalid { .. }));
    }
}
