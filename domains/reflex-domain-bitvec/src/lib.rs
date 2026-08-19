use reflex_canonical::{CanonicalEncode, CanonicalError, CanonicalWriter};
use reflex_domain::{
    CandidateBatch, CandidateBatchBuilder, CandidateHandle, CandidateIndex, Domain,
    DomainCapabilities, DomainError, EpisodeArena, FeatureBatch, SolvedRoot, StateHandle,
    TransitionBatch, TransitionOutcome, UtilityContext, UtilityObservation, VerifyBudget,
    VerifyError,
};
use reflex_types::{
    ActionSchemaId, CandidateId, Digest, FeatureSchemaId, MetricId, ResearchNodeId, StateId,
    TaskId, UnitId,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BvExpr {
    Var(u8),
    Const(u8),
    Add(Box<BvExpr>, Box<BvExpr>),
    Sub(Box<BvExpr>, Box<BvExpr>),
    Xor(Box<BvExpr>, Box<BvExpr>),
    And(Box<BvExpr>, Box<BvExpr>),
    Or(Box<BvExpr>, Box<BvExpr>),
}

impl BvExpr {
    pub fn eval(&self, env: &[u8]) -> u8 {
        match self {
            BvExpr::Var(idx) => env.get(*idx as usize).copied().unwrap_or(0),
            BvExpr::Const(val) => *val,
            BvExpr::Add(a, b) => a.eval(env).wrapping_add(b.eval(env)),
            BvExpr::Sub(a, b) => a.eval(env).wrapping_sub(b.eval(env)),
            BvExpr::Xor(a, b) => a.eval(env) ^ b.eval(env),
            BvExpr::And(a, b) => a.eval(env) & b.eval(env),
            BvExpr::Or(a, b) => a.eval(env) | b.eval(env),
        }
    }

    pub fn cost(&self) -> u32 {
        match self {
            BvExpr::Var(_) | BvExpr::Const(_) => 1,
            BvExpr::Add(a, b)
            | BvExpr::Sub(a, b)
            | BvExpr::Xor(a, b)
            | BvExpr::And(a, b)
            | BvExpr::Or(a, b) => 1 + a.cost() + b.cost(),
        }
    }

    pub fn depth(&self) -> u32 {
        match self {
            BvExpr::Var(_) | BvExpr::Const(_) => 1,
            BvExpr::Add(a, b)
            | BvExpr::Sub(a, b)
            | BvExpr::Xor(a, b)
            | BvExpr::And(a, b)
            | BvExpr::Or(a, b) => 1 + a.depth().max(b.depth()),
        }
    }
}

impl CanonicalEncode for BvExpr {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        match self {
            BvExpr::Var(v) => {
                out.write_u8(0)?;
                out.write_u8(*v)?;
            }
            BvExpr::Const(c) => {
                out.write_u8(1)?;
                out.write_u8(*c)?;
            }
            BvExpr::Add(a, b) => {
                out.write_u8(2)?;
                a.encode_canonical(out)?;
                b.encode_canonical(out)?;
            }
            BvExpr::Sub(a, b) => {
                out.write_u8(3)?;
                a.encode_canonical(out)?;
                b.encode_canonical(out)?;
            }
            BvExpr::Xor(a, b) => {
                out.write_u8(4)?;
                a.encode_canonical(out)?;
                b.encode_canonical(out)?;
            }
            BvExpr::And(a, b) => {
                out.write_u8(5)?;
                a.encode_canonical(out)?;
                b.encode_canonical(out)?;
            }
            BvExpr::Or(a, b) => {
                out.write_u8(6)?;
                a.encode_canonical(out)?;
                b.encode_canonical(out)?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitvecTask {
    pub initial: BvExpr,
    pub target_max_cost: u32,
}

impl CanonicalEncode for BitvecTask {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        self.initial.encode_canonical(out)?;
        out.write_u32(self.target_max_cost)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitvecArtifact {
    pub optimized: BvExpr,
    pub original: BvExpr,
}

impl CanonicalEncode for BitvecArtifact {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        self.optimized.encode_canonical(out)?;
        self.original.encode_canonical(out)?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitvecVerification {
    pub is_equivalent: bool,
    pub counterexample: Option<u8>,
}

impl CanonicalEncode for BitvecVerification {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        out.write_bool(self.is_equivalent)?;
        out.write_option(self.counterexample.as_ref())?;
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BvCandidate {
    pub resulting_expr: BvExpr,
    pub rule_name: String,
    pub cost_delta: i32,
}

impl CanonicalEncode for BvCandidate {
    fn encode_canonical(&self, out: &mut CanonicalWriter) -> Result<(), CanonicalError> {
        self.resulting_expr.encode_canonical(out)?;
        out.write_str(&self.rule_name)?;
        out.write_i32(self.cost_delta)?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct BitvecDomain {
    capabilities: DomainCapabilities,
}

impl BitvecDomain {
    pub fn new() -> Self {
        Self {
            capabilities: DomainCapabilities {
                domain_id: "bitvec-v1".to_string(),
                domain_digest: Digest::hash_blake3(b"bitvec-v1-domain"),
                action_schema: ActionSchemaId::from_digest(Digest::hash_blake3(b"bitvec-actions")),
                feature_schema: FeatureSchemaId::from_digest(Digest::hash_blake3(
                    b"bitvec-features",
                )),
                feature_dimension: 8,
                max_candidates_per_state: 64,
                deterministic_generation: true,
                supports_exact_cache: true,
            },
        }
    }
}

impl Default for BitvecDomain {
    fn default() -> Self {
        Self::new()
    }
}

/// Enumerate all legal local rewrite transformations on an AST.
pub fn generate_rewrites(expr: &BvExpr) -> Vec<BvCandidate> {
    let mut candidates = Vec::new();
    let curr_cost = expr.cost() as i32;

    // Direct root simplifications
    match expr {
        BvExpr::Xor(a, b) if a == b => {
            let res = BvExpr::Const(0);
            let delta = res.cost() as i32 - curr_cost;
            candidates.push(BvCandidate {
                resulting_expr: res,
                rule_name: "xor_same_zero".to_string(),
                cost_delta: delta,
            });
        }
        BvExpr::Sub(a, b) if a == b => {
            let res = BvExpr::Const(0);
            let delta = res.cost() as i32 - curr_cost;
            candidates.push(BvCandidate {
                resulting_expr: res,
                rule_name: "sub_same_zero".to_string(),
                cost_delta: delta,
            });
        }
        BvExpr::And(a, b) if a == b => {
            let res = (**a).clone();
            let delta = res.cost() as i32 - curr_cost;
            candidates.push(BvCandidate {
                resulting_expr: res,
                rule_name: "and_idempotent".to_string(),
                cost_delta: delta,
            });
        }
        BvExpr::Or(a, b) if a == b => {
            let res = (**a).clone();
            let delta = res.cost() as i32 - curr_cost;
            candidates.push(BvCandidate {
                resulting_expr: res,
                rule_name: "or_idempotent".to_string(),
                cost_delta: delta,
            });
        }
        BvExpr::Add(a, b) => {
            if let BvExpr::Const(0) = **b {
                let res = (**a).clone();
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: "add_zero_r".to_string(),
                    cost_delta: delta,
                });
            } else if let BvExpr::Const(0) = **a {
                let res = (**b).clone();
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: "add_zero_l".to_string(),
                    cost_delta: delta,
                });
            } else if let (BvExpr::Const(c1), BvExpr::Const(c2)) = (&**a, &**b) {
                let res = BvExpr::Const(c1.wrapping_add(*c2));
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: "add_const_fold".to_string(),
                    cost_delta: delta,
                });
            }
            // Commutativity
            let res = BvExpr::Add(b.clone(), a.clone());
            candidates.push(BvCandidate {
                resulting_expr: res,
                rule_name: "add_commute".to_string(),
                cost_delta: 0,
            });
        }
        BvExpr::Xor(a, b) => {
            if let BvExpr::Const(0) = **b {
                let res = (**a).clone();
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: "xor_zero_r".to_string(),
                    cost_delta: delta,
                });
            } else if let BvExpr::Const(0) = **a {
                let res = (**b).clone();
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: "xor_zero_l".to_string(),
                    cost_delta: delta,
                });
            }
            let res = BvExpr::Xor(b.clone(), a.clone());
            candidates.push(BvCandidate {
                resulting_expr: res,
                rule_name: "xor_commute".to_string(),
                cost_delta: 0,
            });
        }
        BvExpr::Sub(a, b) => {
            if let BvExpr::Const(0) = **b {
                let res = (**a).clone();
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: "sub_zero_r".to_string(),
                    cost_delta: delta,
                });
            }
        }
        _ => {}
    }

    // Recursive subterm rewrites for left and right children
    match expr {
        BvExpr::Add(a, b) => {
            for sub in generate_rewrites(a) {
                let res = BvExpr::Add(Box::new(sub.resulting_expr), b.clone());
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: format!("left:{}", sub.rule_name),
                    cost_delta: delta,
                });
            }
            for sub in generate_rewrites(b) {
                let res = BvExpr::Add(a.clone(), Box::new(sub.resulting_expr));
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: format!("right:{}", sub.rule_name),
                    cost_delta: delta,
                });
            }
        }
        BvExpr::Sub(a, b) => {
            for sub in generate_rewrites(a) {
                let res = BvExpr::Sub(Box::new(sub.resulting_expr), b.clone());
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: format!("left:{}", sub.rule_name),
                    cost_delta: delta,
                });
            }
            for sub in generate_rewrites(b) {
                let res = BvExpr::Sub(a.clone(), Box::new(sub.resulting_expr));
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: format!("right:{}", sub.rule_name),
                    cost_delta: delta,
                });
            }
        }
        BvExpr::Xor(a, b) => {
            for sub in generate_rewrites(a) {
                let res = BvExpr::Xor(Box::new(sub.resulting_expr), b.clone());
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: format!("left:{}", sub.rule_name),
                    cost_delta: delta,
                });
            }
            for sub in generate_rewrites(b) {
                let res = BvExpr::Xor(a.clone(), Box::new(sub.resulting_expr));
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: format!("right:{}", sub.rule_name),
                    cost_delta: delta,
                });
            }
        }
        BvExpr::And(a, b) => {
            for sub in generate_rewrites(a) {
                let res = BvExpr::And(Box::new(sub.resulting_expr), b.clone());
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: format!("left:{}", sub.rule_name),
                    cost_delta: delta,
                });
            }
            for sub in generate_rewrites(b) {
                let res = BvExpr::And(a.clone(), Box::new(sub.resulting_expr));
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: format!("right:{}", sub.rule_name),
                    cost_delta: delta,
                });
            }
        }
        BvExpr::Or(a, b) => {
            for sub in generate_rewrites(a) {
                let res = BvExpr::Or(Box::new(sub.resulting_expr), b.clone());
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: format!("left:{}", sub.rule_name),
                    cost_delta: delta,
                });
            }
            for sub in generate_rewrites(b) {
                let res = BvExpr::Or(a.clone(), Box::new(sub.resulting_expr));
                let delta = res.cost() as i32 - curr_cost;
                candidates.push(BvCandidate {
                    resulting_expr: res,
                    rule_name: format!("right:{}", sub.rule_name),
                    cost_delta: delta,
                });
            }
        }
        _ => {}
    }

    // Deduplicate candidates preserving order
    let mut seen = std::collections::HashSet::new();
    let mut deduped = Vec::new();
    for c in candidates {
        if seen.insert(c.resulting_expr.clone()) {
            deduped.push(c);
        }
    }
    deduped
}

pub fn simplify_expr(expr: &BvExpr) -> BvExpr {
    match expr {
        BvExpr::Var(v) => BvExpr::Var(*v),
        BvExpr::Const(c) => BvExpr::Const(*c),
        BvExpr::Xor(a, b) => {
            let sa = simplify_expr(a);
            let sb = simplify_expr(b);
            if sa == sb {
                BvExpr::Const(0)
            } else if let BvExpr::Const(0) = sb {
                sa
            } else if let BvExpr::Const(0) = sa {
                sb
            } else {
                BvExpr::Xor(Box::new(sa), Box::new(sb))
            }
        }
        BvExpr::Sub(a, b) => {
            let sa = simplify_expr(a);
            let sb = simplify_expr(b);
            if sa == sb {
                BvExpr::Const(0)
            } else if let BvExpr::Const(0) = sb {
                sa
            } else {
                BvExpr::Sub(Box::new(sa), Box::new(sb))
            }
        }
        BvExpr::And(a, b) => {
            let sa = simplify_expr(a);
            let sb = simplify_expr(b);
            if sa == sb {
                sa
            } else {
                BvExpr::And(Box::new(sa), Box::new(sb))
            }
        }
        BvExpr::Or(a, b) => {
            let sa = simplify_expr(a);
            let sb = simplify_expr(b);
            if sa == sb {
                sa
            } else {
                BvExpr::Or(Box::new(sa), Box::new(sb))
            }
        }
        BvExpr::Add(a, b) => {
            let sa = simplify_expr(a);
            let sb = simplify_expr(b);
            if let BvExpr::Const(0) = sb {
                sa
            } else if let BvExpr::Const(0) = sa {
                sb
            } else if let (BvExpr::Const(c1), BvExpr::Const(c2)) = (&sa, &sb) {
                BvExpr::Const(c1.wrapping_add(*c2))
            } else {
                BvExpr::Add(Box::new(sa), Box::new(sb))
            }
        }
    }
}

impl Domain for BitvecDomain {
    type Task = BitvecTask;
    type State = BvExpr;
    type Candidate = BvCandidate;
    type Transition = BvExpr;
    type Artifact = BitvecArtifact;
    type Verification = BitvecVerification;

    fn capabilities(&self) -> DomainCapabilities {
        self.capabilities.clone()
    }

    fn task_id(&self, task: &Self::Task) -> Result<TaskId, DomainError> {
        let digest = reflex_canonical::content_id(b"bitvec.task.v1", task)
            .map_err(|e| DomainError::InvalidTask(e.to_string()))?;
        Ok(TaskId::from_digest(digest))
    }

    fn initial_state(
        &self,
        task: &Self::Task,
        arena: &mut EpisodeArena,
    ) -> Result<StateHandle, DomainError> {
        let digest = reflex_canonical::content_id(b"bitvec.state.v1", &task.initial)
            .map_err(|e| DomainError::InvalidTask(e.to_string()))?;
        let state_id = StateId::from_digest(digest);
        Ok(arena.insert_state(task.initial.clone(), state_id))
    }

    fn state_id(&self, state: StateHandle, arena: &EpisodeArena) -> Result<StateId, DomainError> {
        arena
            .get_state_id(state)
            .ok_or(DomainError::InvalidStateHandle(state.0))
    }

    fn enumerate_candidates(
        &self,
        state: StateHandle,
        arena: &EpisodeArena,
        output: &mut CandidateBatchBuilder,
    ) -> Result<(), DomainError> {
        let expr: &BvExpr = arena
            .get_state(state)
            .ok_or(DomainError::InvalidStateHandle(state.0))?;

        let rewrites = generate_rewrites(expr);
        for (idx, r) in rewrites.into_iter().enumerate() {
            let digest = reflex_canonical::content_id(b"bitvec.cand.v1", &r)
                .map_err(|e| DomainError::Enumeration(e.to_string()))?;
            let cand_id = CandidateId::from_digest(digest);
            let handle = CandidateHandle(idx as u32);
            let class = if r.cost_delta < 0 { 0 } else { 1 };
            output.add(cand_id, class, idx as u64, handle, 0);
        }

        Ok(())
    }

    fn extract_features(
        &self,
        states: &[StateHandle],
        candidates: &CandidateBatch,
        arena: &EpisodeArena,
        output: &mut FeatureBatch,
    ) -> Result<(), DomainError> {
        if states.is_empty() {
            return Ok(());
        }
        let state_handle = states[0];
        let expr: &BvExpr = arena
            .get_state(state_handle)
            .ok_or(DomainError::InvalidStateHandle(state_handle.0))?;

        let rewrites = generate_rewrites(expr);

        for row in 0..candidates.len() {
            let row_slice = output.row_mut(row);
            let cand_opt = rewrites.get(row);

            row_slice[0] = expr.cost() as f32;
            row_slice[1] = expr.depth() as f32;
            row_slice[2] = match expr {
                BvExpr::Var(_) => 1.0,
                _ => 0.0,
            };
            row_slice[3] = match expr {
                BvExpr::Const(_) => 1.0,
                _ => 0.0,
            };
            row_slice[4] = match expr {
                BvExpr::Add(_, _) => 1.0,
                _ => 0.0,
            };
            row_slice[5] = match expr {
                BvExpr::Xor(_, _) => 1.0,
                _ => 0.0,
            };
            row_slice[6] = match expr {
                BvExpr::And(_, _) => 1.0,
                _ => 0.0,
            };
            // 7: expected cost reduction heuristic
            row_slice[7] = cand_opt
                .map(|c| (-c.cost_delta).max(0) as f32)
                .unwrap_or(0.0);
        }
        Ok(())
    }

    fn apply_candidates(
        &self,
        state: StateHandle,
        _candidates: &CandidateBatch,
        selection: &[CandidateIndex],
        arena: &mut EpisodeArena,
        output: &mut TransitionBatch,
    ) -> Result<(), DomainError> {
        let expr: BvExpr = arena
            .get_state(state)
            .cloned()
            .ok_or(DomainError::InvalidStateHandle(state.0))?;

        let rewrites = generate_rewrites(&expr);

        for &CandidateIndex(idx) in selection {
            if let Some(cand) = rewrites.get(idx) {
                let next_expr = cand.resulting_expr.clone();
                let digest = reflex_canonical::content_id(b"bitvec.state.v1", &next_expr)
                    .map_err(|e| DomainError::Application(e.to_string()))?;
                let child_id = StateId::from_digest(digest);
                let child_handle = arena.insert_state(next_expr.clone(), child_id);

                if next_expr.cost() <= 1 {
                    output.add(TransitionOutcome::Closed);
                } else {
                    output.add(TransitionOutcome::Obligations {
                        and_child_states: vec![child_handle],
                    });
                }
            } else {
                output.add(TransitionOutcome::Closed);
            }
        }
        Ok(())
    }

    fn reconstruct_artifact(
        &self,
        solved: SolvedRoot,
        arena: &EpisodeArena,
    ) -> Result<Self::Artifact, DomainError> {
        let original: &BvExpr = arena
            .get_state(solved.root_state)
            .ok_or(DomainError::InvalidStateHandle(solved.root_state.0))?;

        let optimized = simplify_expr(original);
        Ok(BitvecArtifact {
            optimized,
            original: original.clone(),
        })
    }

    fn verify(
        &self,
        artifact: &Self::Artifact,
        _budget: VerifyBudget,
    ) -> Result<Self::Verification, VerifyError> {
        // Exhaustive verification over all 256 u8 inputs
        for val in 0..=255u8 {
            let env = [val, val.wrapping_add(1), val ^ 0x55, val & 0x0f];
            let out_opt = artifact.optimized.eval(&env);
            let out_orig = artifact.original.eval(&env);
            if out_opt != out_orig {
                return Ok(BitvecVerification {
                    is_equivalent: false,
                    counterexample: Some(val),
                });
            }
        }
        Ok(BitvecVerification {
            is_equivalent: true,
            counterexample: None,
        })
    }

    fn evaluate_utility(
        &self,
        artifact: &Self::Artifact,
        verification: &Self::Verification,
        _context: &UtilityContext,
        output: &mut Vec<UtilityObservation>,
    ) -> Result<(), DomainError> {
        if verification.is_equivalent {
            let cost_orig = artifact.original.cost();
            let cost_opt = artifact.optimized.cost();
            let saved = if cost_orig > cost_opt {
                (cost_orig - cost_opt) as f64
            } else {
                0.0
            };

            output.push(UtilityObservation {
                subject: ResearchNodeId::from_digest(Digest::ZERO),
                metric: MetricId::from_digest(Digest::hash_blake3(b"ops_saved")),
                value: saved,
                unit: UnitId::from_digest(Digest::hash_blake3(b"operations")),
                direction: "Maximize".to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bitvec_exhaustive_verification() {
        let domain = BitvecDomain::new();

        // Valid simplification: x ^ x == 0
        let artifact_valid = BitvecArtifact {
            original: BvExpr::Xor(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0))),
            optimized: BvExpr::Const(0),
        };
        let res_valid = domain
            .verify(
                &artifact_valid,
                VerifyBudget {
                    max_cpu_ns: 1_000_000,
                    max_wall_ns: 1_000_000,
                    max_memory_bytes: 1024,
                },
            )
            .unwrap();
        assert!(res_valid.is_equivalent);
        assert_eq!(res_valid.counterexample, None);

        // Invalid simplification: x + x != 0
        let artifact_invalid = BitvecArtifact {
            original: BvExpr::Add(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0))),
            optimized: BvExpr::Const(0),
        };
        let res_invalid = domain
            .verify(
                &artifact_invalid,
                VerifyBudget {
                    max_cpu_ns: 1_000_000,
                    max_wall_ns: 1_000_000,
                    max_memory_bytes: 1024,
                },
            )
            .unwrap();
        assert!(!res_invalid.is_equivalent);
        assert!(res_invalid.counterexample.is_some());
    }
}
