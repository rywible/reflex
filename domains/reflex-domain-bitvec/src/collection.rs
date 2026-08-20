use crate::corpus::{frozen_eval, frozen_train};
use crate::domain::{BitvecDomain, BitvecState};
use crate::features::{BITVEC_FEATURE_DIM, extract_row};
use crate::policy::{cost_first_policy, simplification_first_policy, uniform_policy};
use crate::rewrites::generate_rewrites;
use crate::verify::verify_equivalent;
use crate::{BitvecArtifact, BitvecVerification};
use reflex_dataset::{DatasetCompiler, DecisionGroup};
use reflex_domain::{Domain, VerifyBudget};
use reflex_search::{SearchBudget, SearchKernel};
use reflex_types::{CandidateId, Digest, ModelCheckpointId, StateId, TaskId};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCollectionReport {
    pub lane: String,
    pub cells_run: usize,
    pub cells_solved: usize,
}

/// Reconstructable authority record for an optimization accepted into a
/// collection. Both the exact artifact and the exhaustive verifier output are
/// retained; either digest can therefore be recomputed without trusting a
/// summary counter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BitvecVerificationReceipt {
    pub artifact: BitvecArtifact,
    pub artifact_digest: Digest,
    pub verification: BitvecVerification,
    pub receipt_digest: Digest,
}

impl reflex_canonical::CanonicalEncode for BitvecVerificationReceipt {
    fn encode_canonical(
        &self,
        out: &mut reflex_canonical::CanonicalWriter,
    ) -> Result<(), reflex_canonical::CanonicalError> {
        self.artifact.encode_canonical(out)?;
        out.write_digest(&self.artifact_digest)?;
        self.verification.encode_canonical(out)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionReport {
    pub cells_run: usize,
    pub cells_solved: usize,
    pub decision_groups: usize,
    pub rows: usize,
    /// This describes the registered task sets, not the currently collected
    /// rows: no frozen-train `TaskId` occurs in frozen-eval.
    pub train_eval_task_sets_disjoint: bool,
    pub lanes: Vec<LaneCollectionReport>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CollectionOutput {
    pub report: CollectionReport,
    pub groups: Vec<DecisionGroup>,
    pub features: HashMap<StateId, Vec<f32>>,
    pub verification_receipts: Vec<BitvecVerificationReceipt>,
}

pub fn train_eval_task_ids(
    domain: &BitvecDomain,
) -> Result<(HashSet<TaskId>, HashSet<TaskId>), String> {
    let mut train = HashSet::new();
    let mut eval = HashSet::new();
    for t in frozen_train() {
        train.insert(domain.task_id(t).map_err(|e| e.to_string())?);
    }
    for t in frozen_eval() {
        eval.insert(domain.task_id(t).map_err(|e| e.to_string())?);
    }
    Ok((train, eval))
}

pub fn check_train_eval_disjoint(domain: &BitvecDomain) -> Result<bool, String> {
    let (train, eval) = train_eval_task_ids(domain)?;
    Ok(train.intersection(&eval).count() == 0)
}

fn process_solved(
    domain: &BitvecDomain,
    search: &mut SearchKernel<'_, BitvecDomain>,
    root: &reflex_domain::SolvedRoot,
    compiler: &mut DatasetCompiler,
    features_by_state: &mut HashMap<StateId, Vec<f32>>,
    recorded_states: &mut HashSet<StateId>,
    receipts: &mut BTreeMap<Digest, BitvecVerificationReceipt>,
) -> Result<(), String> {
    let artifact = domain
        .reconstruct_artifact(root.clone(), search.episode_arena())
        .map_err(|e| e.to_string())?;
    let receipt = domain
        .verify(
            &artifact,
            VerifyBudget {
                max_cpu_ns: 10_000_000,
                max_wall_ns: 10_000_000,
                max_memory_bytes: 1 << 20,
            },
        )
        .map_err(|e| e.to_string())?;
    if !receipt.is_equivalent {
        return Err("accepted optimization failed exhaustive replay".to_string());
    }
    let artifact_digest = reflex_canonical::content_id(b"bitvec.artifact.v1", &artifact)
        .map_err(|error| error.to_string())?;
    let mut accepted_receipt = BitvecVerificationReceipt {
        artifact: artifact.clone(),
        artifact_digest,
        verification: receipt.clone(),
        receipt_digest: Digest::ZERO,
    };
    let verification_digest =
        reflex_canonical::content_id(b"bitvec.verification-receipt.v1", &accepted_receipt)
            .map_err(|error| error.to_string())?;
    accepted_receipt.receipt_digest = verification_digest;
    receipts
        .entry(verification_digest)
        .or_insert(accepted_receipt);

    for (s_h, c_h, _) in &root.solved_edges {
        let s_id = domain
            .state_id(*s_h, search.episode_arena())
            .map_err(|e| e.to_string())?;
        if !recorded_states.insert(s_id) {
            continue;
        }
        let state: &BitvecState = search
            .episode_arena()
            .get_state(*s_h)
            .ok_or_else(|| "missing state".to_string())?;
        let expr = &state.expr;
        let rewrites = generate_rewrites(expr);
        let chosen_idx = c_h.0 as usize;
        let rw = rewrites
            .get(chosen_idx)
            .ok_or_else(|| "bad candidate handle".to_string())?;
        let cand_id = CandidateId::from_digest(
            reflex_canonical::content_id(b"bitvec.cand.v1", rw).map_err(|e| e.to_string())?,
        );

        let mut candidate_ids = vec![cand_id];
        let mut feature_rows = vec![extract_row(expr, Some(rw))];

        if let Some((_, alt_rw)) = rewrites
            .iter()
            .enumerate()
            .find(|(i, r)| *i != chosen_idx && r.resulting_expr != rw.resulting_expr)
        {
            let alt_id = CandidateId::from_digest(
                reflex_canonical::content_id(b"bitvec.cand.v1", alt_rw)
                    .map_err(|e| e.to_string())?,
            );
            let alt_verification = verify_equivalent(expr, &alt_rw.resulting_expr);
            let alt_artifact = BitvecArtifact {
                original: expr.clone(),
                optimized: alt_rw.resulting_expr.clone(),
            };
            let alt_artifact_digest =
                reflex_canonical::content_id(b"bitvec.artifact.v1", &alt_artifact)
                    .map_err(|error| error.to_string())?;
            let mut alt_receipt = BitvecVerificationReceipt {
                artifact: alt_artifact,
                artifact_digest: alt_artifact_digest,
                verification: alt_verification.clone(),
                receipt_digest: Digest::ZERO,
            };
            let cert =
                reflex_canonical::content_id(b"bitvec.verification-receipt.v1", &alt_receipt)
                    .map_err(|error| error.to_string())?;
            alt_receipt.receipt_digest = cert;
            receipts.entry(cert).or_insert(alt_receipt);
            candidate_ids.push(alt_id);
            feature_rows.push(extract_row(expr, Some(alt_rw)));
            if alt_verification.is_equivalent {
                compiler
                    .record_verified_route(s_id, alt_id, 2, cert)
                    .map_err(|error| error.to_string())?;
            } else {
                compiler
                    .record_known_dead(s_id, alt_id, cert)
                    .map_err(|error| error.to_string())?;
            }
        }

        compiler.record_state_candidates(s_id, candidate_ids, "observed");
        compiler
            .record_verified_route(s_id, cand_id, 1, verification_digest)
            .map_err(|error| error.to_string())?;

        let mut flat = Vec::with_capacity(feature_rows.len() * BITVEC_FEATURE_DIM);
        for row in &feature_rows {
            flat.extend_from_slice(row);
        }
        features_by_state.insert(s_id, flat);
    }
    Ok(())
}

#[allow(clippy::type_complexity)]
pub fn run_first_collection(seed: u64) -> Result<CollectionOutput, String> {
    let domain = BitvecDomain::new();
    if !check_train_eval_disjoint(&domain)? {
        return Err("train/eval task overlap detected".to_string());
    }

    run_collection(&domain, frozen_train(), seed)
}

/// Collect independently verified labels for the frozen held-out split.
/// Training code must never consume this output; it exists only for offline
/// qualification and promotion decisions.
pub fn run_evaluation_collection(seed: u64) -> Result<CollectionOutput, String> {
    let domain = BitvecDomain::new();
    if !check_train_eval_disjoint(&domain)? {
        return Err("train/eval task overlap detected".to_string());
    }
    run_collection(&domain, frozen_eval(), seed)
}

fn run_collection(
    domain: &BitvecDomain,
    tasks: &[crate::corpus::BitvecTask],
    seed: u64,
) -> Result<CollectionOutput, String> {
    let mut compiler = DatasetCompiler::new();
    let mut features_by_state: HashMap<StateId, Vec<f32>> = HashMap::new();
    let mut recorded_states: HashSet<StateId> = HashSet::new();
    let mut receipts = BTreeMap::new();
    let mut lane_counts = BTreeMap::from([
        ("heuristic", (0usize, 0usize)),
        ("stable", (0usize, 0usize)),
        ("uniform", (0usize, 0usize)),
    ]);

    let mut cells_run = 0usize;
    let mut cells_solved = 0usize;

    for (idx, task) in tasks.iter().enumerate() {
        for policy_name in ["stable", "uniform", "heuristic"] {
            cells_run += 1;
            lane_counts.get_mut(policy_name).expect("registered lane").0 += 1;
            let cell_seed = seed
                .wrapping_add(idx as u64)
                .wrapping_add(policy_name.len() as u64);
            if policy_name == "stable" {
                let ranker = simplification_first_policy(ModelCheckpointId::from_digest(
                    Digest::hash_blake3(b"stable-simplification-first-v1"),
                ));
                let mut search = SearchKernel::with_seed(
                    domain,
                    &ranker,
                    SearchBudget::default_for_test(),
                    cell_seed,
                );
                if let Some(root) = search.run(task).map_err(|e| e.to_string())? {
                    cells_solved += 1;
                    lane_counts.get_mut(policy_name).expect("registered lane").1 += 1;
                    process_solved(
                        domain,
                        &mut search,
                        &root,
                        &mut compiler,
                        &mut features_by_state,
                        &mut recorded_states,
                        &mut receipts,
                    )?;
                }
            } else if policy_name == "heuristic" {
                let ranker = cost_first_policy(ModelCheckpointId::from_digest(
                    Digest::hash_blake3(b"heuristic-cost-first-v1"),
                ));
                let mut search = SearchKernel::with_seed(
                    domain,
                    &ranker,
                    SearchBudget::default_for_test(),
                    cell_seed,
                );
                if let Some(root) = search.run(task).map_err(|e| e.to_string())? {
                    cells_solved += 1;
                    lane_counts.get_mut(policy_name).expect("registered lane").1 += 1;
                    process_solved(
                        domain,
                        &mut search,
                        &root,
                        &mut compiler,
                        &mut features_by_state,
                        &mut recorded_states,
                        &mut receipts,
                    )?;
                }
            } else {
                let ranker = uniform_policy(
                    ModelCheckpointId::from_digest(Digest::hash_blake3(b"uniform-collect")),
                    cell_seed,
                );
                let mut search = SearchKernel::with_seed(
                    domain,
                    &ranker,
                    SearchBudget::default_for_test(),
                    cell_seed,
                );
                if let Some(root) = search.run(task).map_err(|e| e.to_string())? {
                    cells_solved += 1;
                    lane_counts.get_mut(policy_name).expect("registered lane").1 += 1;
                    process_solved(
                        domain,
                        &mut search,
                        &root,
                        &mut compiler,
                        &mut features_by_state,
                        &mut recorded_states,
                        &mut receipts,
                    )?;
                }
            }
        }
    }

    let groups = compiler.finalize_labels();
    let rows: usize = groups.iter().map(|g| g.candidate_ids.len()).sum();
    let report = CollectionReport {
        cells_run,
        cells_solved,
        decision_groups: groups.len(),
        rows,
        train_eval_task_sets_disjoint: check_train_eval_disjoint(domain)?,
        lanes: lane_counts
            .into_iter()
            .map(|(lane, (cells_run, cells_solved))| LaneCollectionReport {
                lane: lane.to_string(),
                cells_run,
                cells_solved,
            })
            .collect(),
    };
    let _ = BITVEC_FEATURE_DIM;
    Ok(CollectionOutput {
        report,
        groups,
        features: features_by_state,
        verification_receipts: receipts.into_values().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collection_reconciles_counts() {
        let output = run_first_collection(42).expect("collection");
        assert!(output.report.cells_solved > 0);
        assert_eq!(output.report.decision_groups, output.groups.len());
        let rows: usize = output.groups.iter().map(|g| g.candidate_ids.len()).sum();
        assert_eq!(output.report.rows, rows);
        assert_eq!(output.report.lanes.len(), 3);
        assert!(!output.verification_receipts.is_empty());
        for receipt in output.verification_receipts {
            assert_eq!(
                receipt.artifact_digest,
                reflex_canonical::content_id(b"bitvec.artifact.v1", &receipt.artifact).unwrap()
            );
            assert_eq!(
                receipt.receipt_digest,
                reflex_canonical::content_id(b"bitvec.verification-receipt.v1", &receipt).unwrap()
            );
            assert!(receipt.verification.is_equivalent);
        }
    }

    #[test]
    fn held_out_collection_is_nonempty_and_disjoint() {
        let output = run_evaluation_collection(99).expect("held-out collection");
        assert!(output.report.cells_solved > 0);
        assert!(!output.groups.is_empty());
        assert!(check_train_eval_disjoint(&BitvecDomain::new()).unwrap());
    }

    #[test]
    fn alternate_seed_is_bounded_even_when_search_discovers_a_cycle() {
        let _bounded_result = run_first_collection(43);
    }
}
