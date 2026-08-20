//! Loom concurrency models for cell-owned permit bookkeeping (P16.5).

#[cfg(reflex_loom)]
mod models {
    use super::super::PoolRegistry;
    use loom::sync::Arc;
    use loom::thread;

    #[test]
    fn loom_permit_registry_balances_concurrent_lease_lifetimes() {
        let mut model = loom::model::Builder::new();
        model.max_threads = 3;
        model.max_branches = 80;
        model.max_permutations = Some(10_000);
        model.preemption_bound = Some(2);

        model.check(|| {
            let registry = Arc::new(PoolRegistry::new());
            let search = Arc::clone(&registry);
            let verifier = Arc::clone(&registry);

            let search_thread = thread::spawn(move || {
                search.on_acquire("cell", 1);
                assert!(search.get_active("cell") <= 2);
                thread::yield_now();
                search.on_release("cell", 1);
            });
            let verifier_thread = thread::spawn(move || {
                verifier.on_acquire("cell", 1);
                assert!(verifier.get_active("cell") <= 2);
                thread::yield_now();
                verifier.on_release("cell", 1);
            });

            search_thread.join().unwrap();
            verifier_thread.join().unwrap();

            assert_eq!(registry.get_active("cell"), 0);
            assert_eq!(registry.diagnostics("cell").queue_depth, 0);
            registry.check_clean_shutdown().unwrap();
        });
    }
}
