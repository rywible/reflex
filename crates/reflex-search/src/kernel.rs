use reflex_domain::{
    CandidateHandle, CandidateIndex, Domain, EpisodeArena, SolvedRoot, StateHandle,
    TransitionBatch, TransitionOutcome,
};
use reflex_types::{CandidateId, Digest, StateId};
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use std::cmp::{Ordering, Reverse};
use std::collections::{BinaryHeap, HashSet};
use std::time::Instant;

use crate::batches::SearchBatchPool;
use crate::budget::{BudgetFired, BudgetSet};
use crate::cache::{SearchCaches, TranspositionHit, VisitRecord};
use crate::policy::SearchPolicy;
use crate::proof_dag::{CandidateKnowledge, ProofCycleEdge, ProofDag, ProofScc};
use crate::replay::{
    ReplayInputIdentity, SearchTranscript, TranscriptDecision, TranscriptExpansion,
    TranscriptTransition, TranscriptTransitionOutcome, TranscriptWitness,
};
use crate::{InferenceTelemetry, OrderedScore, SearchError, SearchStats};

// ── Arena index newtypes ───────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NodeIndex(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct EdgeIndex(pub u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CandidateBatchIndex(pub u32);

// ── Node status (§11.1) ───────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeStatus {
    Open,
    Closed,
    Failed,
    ObligationPending,
    ObligationComplete,
    /// Budget exhaustion — censored, not a semantic failure (INV-RFX-5).
    Censored,
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
    pub best_logical_cost: u32,
    pub depth: u32,
    pub expanded: bool,
    /// Index into [`SearchArena::candidate_batches`]. Keeping the six-vector
    /// SoA header out of every node satisfies the compact-node contract.
    pub candidates: Option<CandidateBatchIndex>,
}

// ── Edge ───────────────────────────────────────────────────────────────

pub struct Edge {
    pub parent_node: NodeIndex,
    pub candidate_id: CandidateId,
    pub candidate_handle: CandidateHandle,
    pub and_group_idx: Option<usize>,
    /// Accepted receipt for a terminal closure. Obligation edges derive their
    /// evidence from the complete solved child route.
    pub receipt: Option<Digest>,
    /// Process-tree CPU measured around candidate application when the cell
    /// configured an OS sampler. Missing measurement remains unknown.
    pub verification_cpu_ns: Option<u64>,
}

// ── AND group (§11.1, §11.2) ─────────────────────────────────────────

pub struct AndGroup {
    pub parent_edge: EdgeIndex,
    pub children: SmallVec<[NodeIndex; 4]>,
    pub remaining: u32,
    pub failed: bool,
}

// ── Search arena (§11.2) ──────────────────────────────────────────────

pub struct SearchArena {
    pub nodes: Vec<SearchNode>,
    pub edges: Vec<Edge>,
    pub and_groups: Vec<AndGroup>,
    candidate_batches: Vec<Option<reflex_domain::CandidateBatch>>,
    alternative_parents: Vec<SmallVec<[EdgeIndex; 2]>>,
    node_and_groups: Vec<SmallVec<[usize; 2]>>,
}

impl SearchArena {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            and_groups: Vec::new(),
            candidate_batches: Vec::new(),
            alternative_parents: Vec::new(),
            node_and_groups: Vec::new(),
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
            best_logical_cost: cost,
            depth,
            expanded: false,
            candidates: None,
        });
        self.alternative_parents.push(SmallVec::new());
        self.node_and_groups.push(SmallVec::new());
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
            receipt: None,
            verification_cpu_ns: None,
        });
        idx
    }

    pub fn add_and_group(
        &mut self,
        parent_edge: EdgeIndex,
        parent_node: NodeIndex,
        child_indices: &[NodeIndex],
    ) -> usize {
        let group_idx = self.and_groups.len();
        let num_children = child_indices
            .iter()
            .filter(|child| !self.nodes[child.0 as usize].status.is_solved())
            .count() as u32;
        self.and_groups.push(AndGroup {
            parent_edge,
            children: child_indices.iter().copied().collect(),
            remaining: num_children,
            failed: false,
        });
        if let Some(edge) = self.edges.get_mut(parent_edge.0 as usize) {
            edge.and_group_idx = Some(group_idx);
        }
        self.node_and_groups[parent_node.0 as usize].push(group_idx);
        group_idx
    }

    pub fn parent_edges(&self, node_idx: NodeIndex) -> SmallVec<[EdgeIndex; 2]> {
        let node = &self.nodes[node_idx.0 as usize];
        let mut edges = SmallVec::new();
        if let Some(fp) = node.first_parent {
            edges.push(fp);
        }
        for &ap in &self.alternative_parents[node_idx.0 as usize] {
            edges.push(ap);
        }
        edges
    }

    fn store_candidates(&mut self, node_idx: NodeIndex, batch: reflex_domain::CandidateBatch) {
        let batch_idx = CandidateBatchIndex(self.candidate_batches.len() as u32);
        self.candidate_batches.push(Some(batch));
        self.nodes[node_idx.0 as usize].candidates = Some(batch_idx);
    }

    pub fn node_candidates(&self, node_idx: NodeIndex) -> Option<&reflex_domain::CandidateBatch> {
        let batch_idx = self.nodes[node_idx.0 as usize].candidates?;
        self.candidate_batches
            .get(batch_idx.0 as usize)
            .and_then(Option::as_ref)
    }

    fn take_candidates(&mut self, node_idx: NodeIndex) -> Option<reflex_domain::CandidateBatch> {
        let batch_idx = self.nodes[node_idx.0 as usize].candidates.take()?;
        self.candidate_batches
            .get_mut(batch_idx.0 as usize)
            .and_then(Option::take)
    }
}

impl Default for SearchArena {
    fn default() -> Self {
        Self::new()
    }
}

// ── Frontier key (§11.3) ──────────────────────────────────────────────

#[derive(Clone, Eq, PartialEq, Debug, Serialize, Deserialize)]
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

fn take_single_transition(
    mut transitions: TransitionBatch,
    selected: usize,
) -> Result<TransitionOutcome, SearchError> {
    if transitions.outcomes.len() != selected || selected != 1 {
        return Err(SearchError::InvariantViolation(format!(
            "domain returned {} transition outcomes for {selected} selected candidates",
            transitions.outcomes.len()
        )));
    }
    Ok(transitions
        .outcomes
        .pop()
        .expect("cardinality checked above"))
}

// ── Search kernel ──────────────────────────────────────────────────────

pub struct SearchKernel<'a, D: Domain> {
    domain: &'a D,
    ranker: &'a dyn SearchPolicy,
    budget: BudgetSet,
    cell_seed: u64,
    arena: SearchArena,
    episode_arena: EpisodeArena,
    frontier: BinaryHeap<FrontierEntry>,
    insertion_counter: u64,
    stats: SearchStats,
    proof_dag: ProofDag,
    caches: SearchCaches,
    transcript: SearchTranscript,
    transcript_step: usize,
    batch_pool: SearchBatchPool,
    score_buffer: Vec<f32>,
}

impl<'a, D: Domain> SearchKernel<'a, D> {
    pub fn new(domain: &'a D, ranker: &'a dyn SearchPolicy, budget: impl Into<BudgetSet>) -> Self {
        Self::with_seed(domain, ranker, budget, 0)
    }

    pub fn with_seed(
        domain: &'a D,
        ranker: &'a dyn SearchPolicy,
        budget: impl Into<BudgetSet>,
        cell_seed: u64,
    ) -> Self {
        ranker
            .validate_available()
            .expect("ranker must be available before cell start");
        let cap = domain.capabilities();
        let budget = budget.into();
        let max_candidates = cap.max_candidates_per_state.max(1) as usize;
        let max_proof_edges = (budget.limits.node_count as usize).saturating_mul(max_candidates);
        let feature_cols = cap.feature_dimension.max(1);
        let batch_pool = SearchBatchPool::new(
            max_candidates,
            max_candidates,
            feature_cols,
            cap.feature_schema,
        );
        Self {
            domain,
            ranker,
            budget,
            cell_seed,
            arena: SearchArena::new(),
            episode_arena: EpisodeArena::new(),
            frontier: BinaryHeap::new(),
            insertion_counter: 0,
            stats: SearchStats::default(),
            proof_dag: ProofDag::with_max_edges(max_proof_edges),
            caches: SearchCaches::default(),
            transcript: SearchTranscript::new(cell_seed),
            transcript_step: 0,
            batch_pool,
            score_buffer: Vec::with_capacity(max_candidates),
        }
    }

    pub fn run(&mut self, task: &D::Task) -> Result<Option<SolvedRoot>, SearchError> {
        let start_time = Instant::now();

        self.recycle_all_arena_candidates();
        self.arena = SearchArena::new();
        self.episode_arena.clear();
        self.frontier.clear();
        self.insertion_counter = 0;
        self.stats = SearchStats::default();
        let max_proof_edges = (self.budget.limits.node_count as usize)
            .saturating_mul(self.domain.capabilities().max_candidates_per_state.max(1) as usize);
        self.proof_dag = ProofDag::with_max_edges(max_proof_edges);
        self.transcript = SearchTranscript::new(self.cell_seed);
        self.transcript_step = 0;
        self.budget.reset();
        // Arena indices are episode-local. Exact reflex entries may survive,
        // but transposition records must never point into a previous arena.
        self.caches.transposition.clear();

        self.transcript.inputs = Some(ReplayInputIdentity {
            domain_digest: self.domain.capabilities().domain_digest,
            task_id: self.domain.task_id(task)?,
            policy_id: self.ranker.model_id(),
            budget_limits: self.budget.limits.clone(),
        });

        let initial_handle = self.domain.initial_state(task, &mut self.episode_arena)?;
        let initial_id = self.domain.state_id(initial_handle, &self.episode_arena)?;

        let root_idx =
            self.arena
                .add_node(initial_handle, initial_id, NodeStatus::Open, 0, 0, None);
        self.budget.record_node();

        self.expand_node(root_idx)?;
        if let Some(fired) = self.budget.first_fired.clone() {
            return self.finish_censored(fired, initial_handle, start_time);
        }

        while let Some(entry) = self.frontier.pop() {
            if let Some(fired) = self.budget.check_all() {
                return self.finish_censored(fired, initial_handle, start_time);
            }

            let node_idx = entry.node_idx;
            {
                let node = &self.arena.nodes[node_idx.0 as usize];
                if node.status.is_solved()
                    || node.status == NodeStatus::Failed
                    || node.status == NodeStatus::Censored
                {
                    continue;
                }
            }

            self.transcript.record_decision(TranscriptDecision {
                step: self.transcript_step,
                score_bits: entry.key.score_key.bits(),
                logical_cost: entry.key.logical_cost.0,
                depth: entry.key.depth.0,
                state_id: entry.key.state_id.0,
                candidate_id: entry.key.candidate_id.0,
                insertion_seq: entry.key.insertion_seq.0,
            });
            self.transcript_step += 1;

            let cand_local_idx = entry.candidate_local_idx;
            let (node_state, node_depth, parent_state_id, cand_id, cand_handle) = {
                let node = &self.arena.nodes[node_idx.0 as usize];
                let Some(batch) = self.arena.node_candidates(node_idx) else {
                    continue;
                };
                if cand_local_idx >= batch.len() {
                    continue;
                }
                (
                    node.state,
                    node.depth,
                    node.state_id,
                    batch.ids[cand_local_idx],
                    batch.payload_handles[cand_local_idx],
                )
            };

            let selection = vec![CandidateIndex(cand_local_idx)];
            let mut transitions = TransitionBatch::new();
            let cand_batch = self
                .arena
                .node_candidates(node_idx)
                .expect("candidate batch present");
            let verification_cpu_start = self.budget.sample_process_cpu_ns();
            self.domain.apply_candidates(
                node_state,
                cand_batch,
                &selection,
                &mut self.episode_arena,
                &mut transitions,
            )?;
            let verification_cpu_ns = verification_cpu_start
                .zip(self.budget.sample_process_cpu_ns())
                .map(|(start, end)| end.saturating_sub(start));
            self.budget.record_verified_action();
            let outcome = take_single_transition(transitions, selection.len())?;
            let transcript_outcome = match &outcome {
                TransitionOutcome::Closed { witness } => TranscriptTransitionOutcome::Closed {
                    witness: TranscriptWitness {
                        artifact: witness.artifact,
                        verification: witness.verification,
                    },
                },
                TransitionOutcome::Obligations { group } => {
                    let children = group
                        .children
                        .iter()
                        .map(|&child| self.domain.state_id(child, &self.episode_arena))
                        .collect::<Result<Vec<_>, _>>()?;
                    TranscriptTransitionOutcome::Obligations {
                        group_id: group.group_id,
                        children,
                    }
                }
                TransitionOutcome::Contradiction { certificate } => {
                    TranscriptTransitionOutcome::Contradiction {
                        certificate: certificate.as_ref().map(|witness| TranscriptWitness {
                            artifact: witness.artifact,
                            verification: witness.verification,
                        }),
                    }
                }
                TransitionOutcome::Invalid { code } => {
                    TranscriptTransitionOutcome::Invalid { code: code.index() }
                }
                TransitionOutcome::Unresolved { code } => {
                    TranscriptTransitionOutcome::Unresolved { code: *code }
                }
            };
            self.transcript.record_transition(TranscriptTransition {
                state_id: parent_state_id,
                candidate_id: cand_id,
                outcome: transcript_outcome,
            });
            match outcome {
                TransitionOutcome::Closed { witness } => {
                    let Some(receipt) = witness
                        .verification
                        .filter(|receipt| *receipt != Digest::ZERO)
                    else {
                        // A claimed closure without an accepted receipt is
                        // unresolved evidence, never success (§10.5).
                        continue;
                    };
                    let edge_idx = self.arena.add_edge(node_idx, cand_id, cand_handle);
                    self.arena.edges[edge_idx.0 as usize].receipt = Some(receipt);
                    self.arena.edges[edge_idx.0 as usize].verification_cpu_ns = verification_cpu_ns;
                    self.arena.nodes[node_idx.0 as usize].status = NodeStatus::Closed;
                    self.cache_node_status(node_idx);
                    self.recycle_node_candidates(node_idx);
                    self.on_child_solved(node_idx);
                    self.propagate_closed(node_idx);
                }
                TransitionOutcome::Obligations { group } => {
                    let edge_idx = self.arena.add_edge(node_idx, cand_id, cand_handle);
                    self.arena.edges[edge_idx.0 as usize].verification_cpu_ns = verification_cpu_ns;
                    let parent_cost = self.arena.nodes[node_idx.0 as usize].best_logical_cost;
                    let next_cost = parent_cost.saturating_add(1);

                    let and_child_states = group.children;
                    let mut child_indices = SmallVec::<[NodeIndex; 4]>::new();

                    for child_handle in and_child_states {
                        let child_id = self.domain.state_id(child_handle, &self.episode_arena)?;

                        if let Some(hit) = self
                            .caches
                            .transposition
                            .lookup(child_id, self.remaining_verified_actions())
                        {
                            match hit {
                                TranspositionHit::Reuse(existing_idx) => {
                                    child_indices.push(existing_idx);
                                    self.arena.alternative_parents[existing_idx.0 as usize]
                                        .push(edge_idx);
                                }
                                TranspositionHit::Blocked => {
                                    self.budget.record_successor_goal();
                                    let child_idx = self.arena.add_node(
                                        child_handle,
                                        child_id,
                                        NodeStatus::ObligationPending,
                                        next_cost,
                                        node_depth + 1,
                                        Some(edge_idx),
                                    );
                                    self.budget.record_node();
                                    child_indices.push(child_idx);
                                    self.caches.transposition.insert(
                                        child_id,
                                        VisitRecord {
                                            node: child_idx,
                                            best_remaining_budget: self
                                                .remaining_verified_actions(),
                                            visit_budget: self.budget.counters().verified_actions,
                                            status: NodeStatus::ObligationPending,
                                        },
                                    );
                                }
                            }
                        } else {
                            self.budget.record_successor_goal();
                            let child_idx = self.arena.add_node(
                                child_handle,
                                child_id,
                                NodeStatus::ObligationPending,
                                next_cost,
                                node_depth + 1,
                                Some(edge_idx),
                            );
                            self.budget.record_node();
                            child_indices.push(child_idx);
                            self.caches.transposition.insert(
                                child_id,
                                VisitRecord {
                                    node: child_idx,
                                    best_remaining_budget: self.remaining_verified_actions(),
                                    visit_budget: self.budget.counters().verified_actions,
                                    status: NodeStatus::ObligationPending,
                                },
                            );
                        }
                    }

                    self.arena.add_and_group(edge_idx, node_idx, &child_indices);

                    let all_already_solved = !child_indices.is_empty()
                        && child_indices
                            .iter()
                            .all(|&c| self.arena.nodes[c.0 as usize].status.is_solved());

                    if all_already_solved {
                        self.arena.nodes[node_idx.0 as usize].status = NodeStatus::Closed;
                        self.cache_node_status(node_idx);
                        self.recycle_node_candidates(node_idx);
                        self.on_child_solved(node_idx);
                        self.propagate_closed(node_idx);
                    } else {
                        for &child_idx in &child_indices {
                            let child_status = self.arena.nodes[child_idx.0 as usize].status;
                            if (child_status == NodeStatus::Open
                                || child_status == NodeStatus::ObligationPending)
                                && !self.arena.nodes[child_idx.0 as usize].expanded
                            {
                                self.expand_node(child_idx)?;
                            }
                        }
                    }
                }
                TransitionOutcome::Contradiction { certificate } => {
                    if let Some(cert) = certificate
                        .and_then(|c| c.verification)
                        .filter(|cert| *cert != Digest::ZERO)
                    {
                        self.proof_dag
                            .try_mark_known_dead(parent_state_id, cand_id, cert)
                            .map_err(|error| SearchError::InvariantViolation(error.to_string()))?;
                        self.try_fail_or_node(node_idx);
                    }
                }
                TransitionOutcome::Invalid { code } => {
                    self.proof_dag
                        .try_mark_invalid(parent_state_id, cand_id, code.index())
                        .map_err(|error| SearchError::InvariantViolation(error.to_string()))?;
                    self.try_fail_or_node(node_idx);
                }
                TransitionOutcome::Unresolved { .. } => {}
            }

            if self.arena.nodes[0].status.is_solved() {
                return self.finish_solved(initial_handle, start_time);
            }
        }

        if self.arena.nodes[0].status.is_solved() {
            return self.finish_solved(initial_handle, start_time);
        }

        if let Some(fired) = self.budget.first_fired.clone() {
            return self.finish_censored(fired, initial_handle, start_time);
        }

        self.stats.search_cpu_ns = start_time.elapsed().as_nanos() as u64;
        self.transcript.solved = false;
        self.transcript.budget_counters = self.budget.counters().clone();
        if self.arena.nodes[0].status != NodeStatus::Failed {
            for node in &mut self.arena.nodes {
                if matches!(
                    node.status,
                    NodeStatus::Open | NodeStatus::ObligationPending
                ) {
                    node.status = NodeStatus::Censored;
                }
            }
        }
        Ok(None)
    }

    fn finish_censored(
        &mut self,
        fired: BudgetFired,
        initial_handle: StateHandle,
        start_time: Instant,
    ) -> Result<Option<SolvedRoot>, SearchError> {
        self.budget.first_fired = Some(fired.clone());
        self.transcript.budget_exhaustion = Some(fired);
        self.transcript.solved = false;
        self.transcript.budget_counters = self.budget.counters().clone();
        self.arena.nodes[0].status = NodeStatus::Censored;
        self.stats.search_cpu_ns = start_time.elapsed().as_nanos() as u64;
        self.stats.budget_exhaustion = self.budget.first_fired.clone();
        let _ = initial_handle;
        Ok(None)
    }

    fn finish_solved(
        &mut self,
        initial_handle: StateHandle,
        start_time: Instant,
    ) -> Result<Option<SolvedRoot>, SearchError> {
        self.stats.search_cpu_ns = start_time.elapsed().as_nanos() as u64;
        self.transcript.solved = true;
        self.transcript.budget_counters = self.budget.counters().clone();
        self.record_solved_proof(NodeIndex(0))?;
        let mut receipts: Vec<Digest> = self
            .proof_dag
            .viable_edges()
            .flat_map(|(_, _, receipts)| receipts.iter().copied())
            .collect();
        receipts.sort_unstable_by_key(|receipt| receipt.bytes);
        receipts.dedup();
        self.transcript.receipts = receipts;
        Ok(Some(self.build_solved_root(initial_handle)))
    }

    fn expand_node(&mut self, node_idx: NodeIndex) -> Result<(), SearchError> {
        if let Some(fired) = self.budget.check_all() {
            self.budget.first_fired = Some(fired);
            self.arena.nodes[node_idx.0 as usize].status = NodeStatus::Censored;
            return Ok(());
        }

        self.arena.nodes[node_idx.0 as usize].expanded = true;
        self.stats.nodes_expanded += 1;

        let state_handle = self.arena.nodes[node_idx.0 as usize].state;
        let depth = self.arena.nodes[node_idx.0 as usize].depth;
        let parent_cost = self.arena.nodes[node_idx.0 as usize].best_logical_cost;
        let parent_state_id = self.arena.nodes[node_idx.0 as usize].state_id;

        let cap = self.domain.capabilities();
        let cols = cap.feature_dimension.max(1);

        let builder = self
            .batch_pool
            .begin_candidates()
            .map_err(|e| SearchError::InvariantViolation(e.to_string()))?;
        self.domain
            .enumerate_candidates(state_handle, &self.episode_arena, builder)?;
        let batch_len = self
            .batch_pool
            .finish_candidates()
            .map_err(|error| SearchError::InvariantViolation(error.to_string()))?
            .len();

        if batch_len == 0 {
            self.transcript.record_expansion(TranscriptExpansion {
                state_id: parent_state_id,
                candidate_ids: Vec::new(),
                score_bits: Vec::new(),
            });
            let empty = self.batch_pool.take_finished_candidates();
            self.batch_pool.recycle_candidates(empty);
            self.arena.nodes[node_idx.0 as usize].status = NodeStatus::Failed;
            self.caches.transposition.insert(
                parent_state_id,
                VisitRecord {
                    node: node_idx,
                    best_remaining_budget: self.remaining_verified_actions(),
                    visit_budget: self.budget.counters().verified_actions,
                    status: NodeStatus::Failed,
                },
            );
            self.propagate_failed(node_idx);
            return Ok(());
        }

        self.batch_pool
            .prepare_features(batch_len, cols, cap.feature_schema)
            .map_err(|e| SearchError::InvariantViolation(e.to_string()))?;
        {
            let (batch, features) = self.batch_pool.finished_candidates_and_features_mut();
            self.domain
                .extract_features(&[state_handle], batch, &self.episode_arena, features)?;
        }

        if self.score_buffer.len() < batch_len {
            self.score_buffer.resize(batch_len, 0.0);
        } else {
            self.score_buffer[..batch_len].fill(0.0);
        }
        let scores = &mut self.score_buffer[..batch_len];
        let mut telemetry = InferenceTelemetry {
            cpu_ns: 0,
            model_id: self.ranker.model_id(),
        };
        let ml_start = Instant::now();
        {
            let (batch, features) = self.batch_pool.finished_candidates_and_features_mut();
            self.ranker
                .score_batch(features, &batch.ids, scores, &mut telemetry)?;
        }
        self.stats.ml_overhead_ns += ml_start.elapsed().as_nanos() as u64;
        self.stats.candidates_scored += batch_len as u32;

        {
            let (batch, _) = self.batch_pool.finished_candidates_and_features_mut();
            let mut score_bits = Vec::with_capacity(batch_len);
            for score in scores.iter().take(batch_len) {
                score_bits.push(OrderedScore::from_f32(*score)?.bits());
            }
            self.transcript.record_expansion(TranscriptExpansion {
                state_id: parent_state_id,
                candidate_ids: batch.ids.clone(),
                score_bits,
            });
        }

        {
            let (batch, _) = self.batch_pool.finished_candidates_and_features_mut();
            for (cand_idx, &score) in scores.iter().enumerate().take(batch_len) {
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
        }

        let batch = self.batch_pool.take_finished_candidates();
        self.arena.store_candidates(node_idx, batch);
        Ok(())
    }

    fn on_child_solved(&mut self, child_idx: NodeIndex) {
        for parent_edge_idx in self.arena.parent_edges(child_idx) {
            let edge = &self.arena.edges[parent_edge_idx.0 as usize];
            if let Some(group_idx) = edge.and_group_idx {
                let group = &mut self.arena.and_groups[group_idx];
                if group.remaining > 0 {
                    group.remaining -= 1;
                }
            }
        }
    }

    fn try_fail_or_node(&mut self, node_idx: NodeIndex) {
        if self.all_candidates_exhausted(node_idx) {
            self.arena.nodes[node_idx.0 as usize].status = NodeStatus::Failed;
            self.cache_node_status(node_idx);
            self.recycle_node_candidates(node_idx);
            self.propagate_failed(node_idx);
        }
    }

    fn all_candidates_exhausted(&self, node_idx: NodeIndex) -> bool {
        let node = &self.arena.nodes[node_idx.0 as usize];
        let Some(batch) = self.arena.node_candidates(node_idx) else {
            return false;
        };
        batch.ids.iter().all(|&cid| {
            matches!(
                self.proof_dag.get_knowledge(node.state_id, cid),
                Some(CandidateKnowledge::KnownDead { .. })
                    | Some(CandidateKnowledge::Invalid { .. })
            )
        })
    }

    fn propagate_closed(&mut self, closed_idx: NodeIndex) {
        let mut worklist = vec![closed_idx];
        let mut visited = HashSet::new();

        while let Some(curr) = worklist.pop() {
            if !visited.insert(curr) {
                continue;
            }

            for parent_edge_idx in self.arena.parent_edges(curr) {
                let edge = &self.arena.edges[parent_edge_idx.0 as usize];
                let parent_node_idx = edge.parent_node;

                if let Some(group_idx) = edge.and_group_idx {
                    let group = &self.arena.and_groups[group_idx];
                    if group.failed {
                        continue;
                    }
                    let all_children_closed = group
                        .children
                        .iter()
                        .all(|&c| self.arena.nodes[c.0 as usize].status.is_solved());
                    if all_children_closed {
                        let parent_status = self.arena.nodes[parent_node_idx.0 as usize].status;
                        if !parent_status.is_solved() {
                            self.arena.nodes[parent_node_idx.0 as usize].status =
                                NodeStatus::Closed;
                            self.cache_node_status(parent_node_idx);
                            self.recycle_node_candidates(parent_node_idx);
                            worklist.push(parent_node_idx);
                        }
                    }
                }
            }
        }
    }

    fn propagate_failed(&mut self, failed_idx: NodeIndex) {
        let mut worklist = vec![failed_idx];
        let mut visited = HashSet::new();

        while let Some(curr) = worklist.pop() {
            if !visited.insert(curr) {
                continue;
            }

            for parent_edge_idx in self.arena.parent_edges(curr) {
                let edge = &self.arena.edges[parent_edge_idx.0 as usize];
                let parent_node_idx = edge.parent_node;

                if let Some(group_idx) = edge.and_group_idx {
                    self.arena.and_groups[group_idx].failed = true;
                }

                if self.arena.nodes[parent_node_idx.0 as usize].status == NodeStatus::Failed {
                    continue;
                }

                let groups = &self.arena.node_and_groups[parent_node_idx.0 as usize];
                let all_groups_failed =
                    !groups.is_empty() && groups.iter().all(|&g| self.arena.and_groups[g].failed);

                if all_groups_failed {
                    self.arena.nodes[parent_node_idx.0 as usize].status = NodeStatus::Failed;
                    self.cache_node_status(parent_node_idx);
                    self.recycle_node_candidates(parent_node_idx);
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
        let mut visited = HashSet::new();

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
                    let all_children_solved = group
                        .children
                        .iter()
                        .all(|&c| self.arena.nodes[c.0 as usize].status.is_solved());
                    if all_children_solved {
                        let child_handles: Vec<StateHandle> = group
                            .children
                            .iter()
                            .map(|&c| self.arena.nodes[c.0 as usize].state)
                            .collect();
                        edges.push((node.state, e.candidate_handle, child_handles));
                        for &c in &group.children {
                            queue.push(c);
                        }
                    }
                } else if e.receipt.is_some() && node.status.is_solved() {
                    edges.push((node.state, e.candidate_handle, Vec::new()));
                }
            }
        }
        edges
    }

    fn remaining_verified_actions(&self) -> u32 {
        self.budget
            .limits
            .verified_actions
            .saturating_sub(self.budget.counters().verified_actions)
    }

    fn cache_node_status(&mut self, node_idx: NodeIndex) {
        let node = &self.arena.nodes[node_idx.0 as usize];
        if node.status == NodeStatus::Censored {
            return;
        }
        self.caches.transposition.insert(
            node.state_id,
            VisitRecord {
                node: node_idx,
                best_remaining_budget: self.remaining_verified_actions(),
                visit_budget: self.budget.counters().verified_actions,
                status: node.status,
            },
        );
    }

    fn recycle_node_candidates(&mut self, node_idx: NodeIndex) {
        if let Some(batch) = self.arena.take_candidates(node_idx) {
            self.batch_pool.recycle_candidates(batch);
        }
    }

    fn recycle_all_arena_candidates(&mut self) {
        for stored in &mut self.arena.candidate_batches {
            if let Some(batch) = stored.take() {
                self.batch_pool.recycle_candidates(batch);
            }
        }
    }

    /// Mines receipt-backed routes with a non-recursive fixed point. Edges
    /// inside cyclic SCCs are retained separately and excluded from the
    /// acyclic cost projection, so a cycle can never manufacture finite
    /// cost-to-go.
    fn record_solved_proof(&mut self, root: NodeIndex) -> Result<(), SearchError> {
        #[derive(Clone, Debug)]
        struct RouteCost {
            actions: u32,
            cpu_ns: Option<u64>,
            receipts: SmallVec<[Digest; 2]>,
        }

        fn merge_receipts(
            target: &mut SmallVec<[Digest; 2]>,
            source: impl IntoIterator<Item = Digest>,
        ) {
            for receipt in source {
                if receipt != Digest::ZERO && !target.contains(&receipt) {
                    target.push(receipt);
                }
            }
        }

        let node_count = self.arena.nodes.len();
        if root.0 as usize >= node_count {
            return Err(SearchError::InvariantViolation(
                "proof root is outside the search arena".to_string(),
            ));
        }

        let mut adjacency = vec![Vec::<NodeIndex>::new(); node_count];
        let mut reverse = vec![Vec::<NodeIndex>::new(); node_count];
        for edge in &self.arena.edges {
            let Some(group_idx) = edge.and_group_idx else {
                continue;
            };
            let group = &self.arena.and_groups[group_idx];
            if group.failed {
                continue;
            }
            for &child in &group.children {
                adjacency[edge.parent_node.0 as usize].push(child);
                reverse[child.0 as usize].push(edge.parent_node);
            }
        }

        // Iterative Kosaraju: proof paths can be deep and must not consume the
        // call stack.
        let mut seen = vec![false; node_count];
        let mut finish_order = Vec::with_capacity(node_count);
        for start in 0..node_count {
            if seen[start] {
                continue;
            }
            seen[start] = true;
            let mut stack = vec![(NodeIndex(start as u32), 0usize)];
            while let Some((node, next_child)) = stack.last_mut() {
                let neighbors = &adjacency[node.0 as usize];
                if *next_child < neighbors.len() {
                    let child = neighbors[*next_child];
                    *next_child += 1;
                    if !seen[child.0 as usize] {
                        seen[child.0 as usize] = true;
                        stack.push((child, 0));
                    }
                } else {
                    let (finished, _) = stack.pop().expect("stack is non-empty");
                    finish_order.push(finished);
                }
            }
        }

        let mut component_of = vec![u32::MAX; node_count];
        let mut components = Vec::<Vec<NodeIndex>>::new();
        for &start in finish_order.iter().rev() {
            if component_of[start.0 as usize] != u32::MAX {
                continue;
            }
            let component_id = components.len() as u32;
            component_of[start.0 as usize] = component_id;
            let mut members = Vec::new();
            let mut stack = vec![start];
            while let Some(node) = stack.pop() {
                members.push(node);
                for &parent in &reverse[node.0 as usize] {
                    if component_of[parent.0 as usize] == u32::MAX {
                        component_of[parent.0 as usize] = component_id;
                        stack.push(parent);
                    }
                }
            }
            components.push(members);
        }

        let mut cyclic_component = vec![false; components.len()];
        for (component_id, members) in components.iter().enumerate() {
            cyclic_component[component_id] = members.len() > 1
                || members
                    .iter()
                    .any(|node| adjacency[node.0 as usize].iter().any(|child| child == node));
        }

        let mut cycle_components = Vec::new();
        let mut cycle_edges = Vec::new();
        for (component_id, members) in components.iter().enumerate() {
            if !cyclic_component[component_id] {
                continue;
            }
            cycle_components.push(ProofScc {
                component_id: component_id as u32,
                states: members
                    .iter()
                    .map(|node| self.arena.nodes[node.0 as usize].state_id)
                    .collect(),
            });
        }
        for edge in &self.arena.edges {
            let Some(group_idx) = edge.and_group_idx else {
                continue;
            };
            let parent_component = component_of[edge.parent_node.0 as usize];
            if !cyclic_component[parent_component as usize] {
                continue;
            }
            if self.arena.and_groups[group_idx]
                .children
                .iter()
                .any(|child| component_of[child.0 as usize] == parent_component)
            {
                cycle_edges.push(ProofCycleEdge {
                    component_id: parent_component,
                    state_id: self.arena.nodes[edge.parent_node.0 as usize].state_id,
                    candidate_id: edge.candidate_id,
                });
            }
        }
        self.proof_dag.set_cycles(cycle_components, cycle_edges);

        let mut best_node = vec![None::<RouteCost>; node_count];
        let mut edge_routes = vec![None::<RouteCost>; self.arena.edges.len()];
        let max_rounds = node_count.saturating_add(1);
        for _ in 0..max_rounds {
            let mut changed = false;
            for (edge_index, edge) in self.arena.edges.iter().enumerate() {
                let route = if let Some(receipt) = edge.receipt.filter(|r| *r != Digest::ZERO) {
                    Some(RouteCost {
                        actions: 1,
                        cpu_ns: edge.verification_cpu_ns,
                        receipts: smallvec::smallvec![receipt],
                    })
                } else if let Some(group_idx) = edge.and_group_idx {
                    let group = &self.arena.and_groups[group_idx];
                    let parent_component = component_of[edge.parent_node.0 as usize];
                    let intra_cycle = cyclic_component[parent_component as usize]
                        && group
                            .children
                            .iter()
                            .any(|child| component_of[child.0 as usize] == parent_component);
                    if group.failed || group.children.is_empty() || intra_cycle {
                        None
                    } else {
                        let mut route = RouteCost {
                            actions: 1,
                            cpu_ns: edge.verification_cpu_ns,
                            receipts: SmallVec::new(),
                        };
                        let mut complete = true;
                        for child in &group.children {
                            let Some(child_cost) = &best_node[child.0 as usize] else {
                                complete = false;
                                break;
                            };
                            route.actions = route.actions.saturating_add(child_cost.actions);
                            route.cpu_ns = route
                                .cpu_ns
                                .zip(child_cost.cpu_ns)
                                .map(|(parent, child)| parent.saturating_add(child));
                            merge_receipts(
                                &mut route.receipts,
                                child_cost.receipts.iter().copied(),
                            );
                        }
                        (complete && !route.receipts.is_empty()).then_some(route)
                    }
                } else {
                    None
                };

                let Some(route) = route else {
                    continue;
                };
                let edge_slot = &mut edge_routes[edge_index];
                let edge_improved = edge_slot.as_ref().is_none_or(|known| {
                    route.actions < known.actions
                        || route
                            .cpu_ns
                            .is_some_and(|cpu| known.cpu_ns.is_none_or(|known| cpu < known))
                        || route.receipts.iter().any(|r| !known.receipts.contains(r))
                });
                if edge_improved {
                    *edge_slot = Some(route.clone());
                }

                let node_slot = &mut best_node[edge.parent_node.0 as usize];
                match node_slot {
                    Some(known) => {
                        let old_actions = known.actions;
                        let old_cpu = known.cpu_ns;
                        let old_receipts = known.receipts.len();
                        known.actions = known.actions.min(route.actions);
                        if let Some(cpu_ns) = route.cpu_ns {
                            known.cpu_ns = Some(known.cpu_ns.map_or(cpu_ns, |cpu| cpu.min(cpu_ns)));
                        }
                        merge_receipts(&mut known.receipts, route.receipts.iter().copied());
                        changed |= old_actions != known.actions
                            || old_cpu != known.cpu_ns
                            || old_receipts != known.receipts.len();
                    }
                    None => {
                        *node_slot = Some(route);
                        changed = true;
                    }
                }
            }
            if !changed {
                break;
            }
        }

        for (edge, route) in self.arena.edges.iter().zip(edge_routes) {
            let Some(route) = route else {
                continue;
            };
            let state_id = self.arena.nodes[edge.parent_node.0 as usize].state_id;
            for receipt in route.receipts {
                self.proof_dag
                    .try_mark_viable_with_cost(
                        state_id,
                        edge.candidate_id,
                        route.actions,
                        route.cpu_ns,
                        Some(receipt),
                    )
                    .map_err(|error| SearchError::InvariantViolation(error.to_string()))?;
            }
        }
        Ok(())
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

    pub fn budget(&self) -> &BudgetSet {
        &self.budget
    }

    pub fn transcript(&self) -> &SearchTranscript {
        &self.transcript
    }

    pub fn caches(&self) -> &SearchCaches {
        &self.caches
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::{BudgetLimits, BudgetSet, SearchBudget};
    use crate::policy::UniformRanker;
    use reflex_domain::conformance::{FixtureDomain, FixtureTask};
    use reflex_types::ModelCheckpointId;

    #[test]
    fn test_search_arena_basics() {
        let mut arena = SearchArena::new();
        let handle = StateHandle(0, 0);
        let id = StateId::from_digest(Digest::hash_blake3(b"test"));
        let idx = arena.add_node(handle, id, NodeStatus::Open, 0, 0, None);
        assert_eq!(arena.node_count(), 1);
        assert_eq!(arena.get_node(idx).unwrap().status, NodeStatus::Open);
    }

    #[test]
    fn test_search_node_meets_compact_storage_budget() {
        assert!(
            std::mem::size_of::<SearchNode>() <= 96,
            "SearchNode is {} bytes",
            std::mem::size_of::<SearchNode>()
        );
    }

    #[test]
    fn test_and_group_creation() {
        let mut arena = SearchArena::new();
        let h = StateHandle(0, 0);
        let id = StateId::from_digest(Digest::hash_blake3(b"parent"));
        let parent = arena.add_node(h, id, NodeStatus::Open, 0, 0, None);

        let edge = arena.add_edge(
            parent,
            CandidateId::from_digest(Digest::hash_blake3(b"cand")),
            CandidateHandle(0),
        );

        let ch1 = arena.add_node(
            StateHandle(1, 0),
            StateId::from_digest(Digest::hash_blake3(b"ch1")),
            NodeStatus::ObligationPending,
            1,
            1,
            Some(edge),
        );
        let ch2 = arena.add_node(
            StateHandle(2, 0),
            StateId::from_digest(Digest::hash_blake3(b"ch2")),
            NodeStatus::ObligationPending,
            1,
            1,
            Some(edge),
        );

        let group_idx = arena.add_and_group(edge, parent, &[ch1, ch2]);
        assert_eq!(arena.and_groups[group_idx].children.as_slice(), &[ch1, ch2]);
        assert_eq!(arena.and_groups[group_idx].remaining, 2);
    }

    #[test]
    fn test_unverified_fixture_closure_is_censored() {
        let domain = FixtureDomain::new();
        let task = FixtureTask {
            start: 0,
            target: 3,
            candidates_per_state: 2,
            allow_duplicate_ids: false,
        };
        let ranker =
            UniformRanker::with_seed(ModelCheckpointId::from_digest(Digest::hash_blake3(b"u")), 1);
        let mut kernel = SearchKernel::new(&domain, &ranker, SearchBudget::default_for_test());
        let solved = kernel.run(&task).unwrap();
        assert!(solved.is_none());
        assert_eq!(kernel.arena().nodes[0].status, NodeStatus::Censored);
        assert_eq!(kernel.proof_dag().edge_count(), 0);
    }

    #[test]
    fn test_budget_exhaustion_censored() {
        let domain = FixtureDomain::new();
        let task = FixtureTask {
            start: 0,
            target: 100,
            candidates_per_state: 2,
            allow_duplicate_ids: false,
        };
        let ranker = UniformRanker::new(ModelCheckpointId::from_digest(Digest::hash_blake3(b"u")));
        let budget = BudgetSet::new(
            BudgetLimits {
                verified_actions: 0,
                ..BudgetLimits::default_for_test()
            },
            None,
        );
        let mut kernel = SearchKernel::new(&domain, &ranker, budget);
        let solved = kernel.run(&task).unwrap();
        assert!(solved.is_none());
        assert!(kernel.budget().first_fired.is_some());
        assert_eq!(kernel.arena().nodes[0].status, NodeStatus::Censored);
    }

    #[test]
    fn test_node_status_is_solved() {
        assert!(NodeStatus::Closed.is_solved());
        assert!(NodeStatus::ObligationComplete.is_solved());
        assert!(!NodeStatus::Open.is_solved());
        assert!(!NodeStatus::Failed.is_solved());
        assert!(!NodeStatus::Censored.is_solved());
    }

    #[test]
    fn test_reusing_kernel_resets_budget_lifecycle() {
        let domain = FixtureDomain::new();
        let task = FixtureTask {
            start: 0,
            target: 100,
            candidates_per_state: 2,
            allow_duplicate_ids: false,
        };
        let ranker = UniformRanker::new(ModelCheckpointId::from_digest(Digest::hash_blake3(b"u")));
        let budget = BudgetSet::new(
            BudgetLimits {
                verified_actions: 1,
                ..BudgetLimits::default_for_test()
            },
            None,
        );
        let mut kernel = SearchKernel::new(&domain, &ranker, budget);
        assert!(kernel.run(&task).unwrap().is_none());
        assert_eq!(kernel.budget().counters().verified_actions, 1);
        assert!(kernel.run(&task).unwrap().is_none());
        assert_eq!(kernel.budget().counters().verified_actions, 1);
    }

    #[test]
    fn test_transition_cardinality_is_exact() {
        let empty = TransitionBatch::new();
        assert!(matches!(
            take_single_transition(empty, 1),
            Err(SearchError::InvariantViolation(_))
        ));

        let mut extra = TransitionBatch::new();
        extra.add(TransitionOutcome::Unresolved {
            code: reflex_domain::UnresolvedCode::Timeout,
        });
        extra.add(TransitionOutcome::Unresolved {
            code: reflex_domain::UnresolvedCode::Timeout,
        });
        assert!(matches!(
            take_single_transition(extra, 1),
            Err(SearchError::InvariantViolation(_))
        ));
    }

    #[test]
    fn test_complete_route_mining_requires_and_retains_receipts() {
        let domain = FixtureDomain::new();
        let ranker = UniformRanker::new(ModelCheckpointId::from_digest(Digest::hash_blake3(b"u")));
        let mut kernel = SearchKernel::new(&domain, &ranker, SearchBudget::default_for_test());
        let state_id = StateId::from_digest(Digest::hash_blake3(b"state"));
        let candidate_id = CandidateId::from_digest(Digest::hash_blake3(b"candidate"));
        let receipt = Digest::hash_blake3(b"accepted-receipt");
        let node =
            kernel
                .arena
                .add_node(StateHandle(0, 0), state_id, NodeStatus::Closed, 0, 0, None);
        let edge = kernel
            .arena
            .add_edge(node, candidate_id, CandidateHandle(0));
        kernel.arena.edges[edge.0 as usize].receipt = Some(receipt);

        kernel.record_solved_proof(node).unwrap();
        assert!(matches!(
            kernel.proof_dag.get_knowledge(state_id, candidate_id),
            Some(CandidateKnowledge::Viable {
                best_actions_to_go: 1,
                receipts,
            }) if receipts.as_slice() == [receipt]
        ));
        assert_eq!(
            kernel
                .proof_dag
                .viable_cost(state_id, candidate_id)
                .unwrap()
                .best_actions_to_go,
            1
        );
    }

    #[test]
    fn test_proof_cost_sums_and_children_and_measured_cpu() {
        let domain = FixtureDomain::new();
        let ranker = UniformRanker::new(ModelCheckpointId::from_digest(Digest::hash_blake3(b"u")));
        let mut kernel = SearchKernel::new(&domain, &ranker, SearchBudget::default_for_test());
        let root_state = StateId::from_digest(Digest::hash_blake3(b"root"));
        let child_state = StateId::from_digest(Digest::hash_blake3(b"child"));
        let root_candidate = CandidateId::from_digest(Digest::hash_blake3(b"root-candidate"));
        let child_candidate = CandidateId::from_digest(Digest::hash_blake3(b"child-candidate"));
        let receipt = Digest::hash_blake3(b"accepted");
        let root = kernel.arena.add_node(
            StateHandle(0, 0),
            root_state,
            NodeStatus::Closed,
            0,
            0,
            None,
        );
        let root_edge = kernel
            .arena
            .add_edge(root, root_candidate, CandidateHandle(0));
        kernel.arena.edges[root_edge.0 as usize].verification_cpu_ns = Some(5);
        let child = kernel.arena.add_node(
            StateHandle(1, 0),
            child_state,
            NodeStatus::Closed,
            1,
            1,
            Some(root_edge),
        );
        kernel.arena.add_and_group(root_edge, root, &[child]);
        let child_edge = kernel
            .arena
            .add_edge(child, child_candidate, CandidateHandle(0));
        kernel.arena.edges[child_edge.0 as usize].receipt = Some(receipt);
        kernel.arena.edges[child_edge.0 as usize].verification_cpu_ns = Some(7);

        kernel.record_solved_proof(root).unwrap();
        assert_eq!(
            kernel
                .proof_dag
                .viable_cost(root_state, root_candidate)
                .unwrap(),
            crate::ViableCost {
                best_actions_to_go: 2,
                best_cpu_ns_to_go: Some(12),
            }
        );
    }

    #[test]
    fn test_proof_cycles_are_retained_outside_cost_projection() {
        let domain = FixtureDomain::new();
        let ranker = UniformRanker::new(ModelCheckpointId::from_digest(Digest::hash_blake3(b"u")));
        let mut kernel = SearchKernel::new(&domain, &ranker, SearchBudget::default_for_test());
        let state_a = StateId::from_digest(Digest::hash_blake3(b"a"));
        let state_b = StateId::from_digest(Digest::hash_blake3(b"b"));
        let node_a =
            kernel
                .arena
                .add_node(StateHandle(0, 0), state_a, NodeStatus::Closed, 0, 0, None);
        let a_to_b = kernel.arena.add_edge(
            node_a,
            CandidateId::from_digest(Digest::hash_blake3(b"a-to-b")),
            CandidateHandle(0),
        );
        let node_b = kernel.arena.add_node(
            StateHandle(1, 0),
            state_b,
            NodeStatus::Closed,
            1,
            1,
            Some(a_to_b),
        );
        kernel.arena.add_and_group(a_to_b, node_a, &[node_b]);
        let b_to_a = kernel.arena.add_edge(
            node_b,
            CandidateId::from_digest(Digest::hash_blake3(b"b-to-a")),
            CandidateHandle(0),
        );
        kernel.arena.add_and_group(b_to_a, node_b, &[node_a]);
        let terminal = kernel.arena.add_edge(
            node_b,
            CandidateId::from_digest(Digest::hash_blake3(b"terminal")),
            CandidateHandle(1),
        );
        kernel.arena.edges[terminal.0 as usize].receipt = Some(Digest::hash_blake3(b"accepted"));

        kernel.record_solved_proof(node_a).unwrap();
        assert_eq!(kernel.proof_dag.cycle_components().len(), 1);
        assert_eq!(kernel.proof_dag.cycle_components()[0].states.len(), 2);
        assert_eq!(kernel.proof_dag.cycle_edges().len(), 2);
        assert!(
            kernel
                .proof_dag
                .get_knowledge(
                    state_b,
                    CandidateId::from_digest(Digest::hash_blake3(b"terminal"))
                )
                .is_some()
        );
    }
}
