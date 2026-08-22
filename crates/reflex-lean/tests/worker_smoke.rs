use reflex_lean::worker::{LeanWorker, LeanWorkerConfig, VerificationItem};

#[test]
#[ignore = "requires the pinned Lean and mathlib installations"]
fn pinned_worker_indexes_and_kernel_checks_a_seed() {
    let lake = std::env::var_os("REFLEX_LEAN_LAKE").expect("REFLEX_LEAN_LAKE is required");
    let mathlib = std::env::var_os("REFLEX_LEAN_MATHLIB").expect("REFLEX_LEAN_MATHLIB is required");
    let worker = LeanWorker::start(&LeanWorkerConfig::pinned(lake, mathlib)).unwrap();
    worker.ping().unwrap();
    let page = worker.index_page(0, 1).unwrap();
    assert!(page.total > 300_000);
    assert_eq!(page.offset, 0);
    let theorem = &page.artifacts[0];
    let fetched = worker.fetch(std::slice::from_ref(&theorem.name)).unwrap();
    let theorem = &fetched[0];
    let (results, usage) = worker
        .verify(&[VerificationItem {
            level_params: theorem.level_params.clone(),
            claim_proposition: theorem.proposition.clone(),
            candidate_proposition: theorem.proposition.clone(),
            proof_term: theorem.proof_term.clone(),
            allowed_axioms: theorem.axioms.clone(),
        }])
        .unwrap();
    assert!(results[0].accepted, "{}", results[0].diagnostic);
    assert!(usage.resident_upper_bound > 0);
}
