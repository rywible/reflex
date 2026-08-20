use criterion::{Criterion, criterion_group, criterion_main};
use reflex_domain_bitvec::{BitvecDomain, BitvecTask, BvExpr};
use reflex_search::{SearchBudget, SearchKernel, UniformRanker};
use reflex_types::Digest;
use reflex_types::ModelCheckpointId;
use std::hint::black_box;

pub fn bench_search(c: &mut Criterion) {
    let domain = BitvecDomain::new();
    let task = BitvecTask {
        initial: BvExpr::Xor(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0))),
        target_max_cost: 1,
    };
    let model_id = ModelCheckpointId::from_digest(Digest::hash_blake3(b"uniform"));
    let ranker = UniformRanker::new(model_id);
    let budget = SearchBudget::default_for_test();

    c.bench_function("search_bitvec_uniform", |b| {
        b.iter(|| {
            let mut search = SearchKernel::new(&domain, &ranker, budget.clone());
            black_box(search.run(black_box(&task)).unwrap());
        })
    });
}

criterion_group!(benches, bench_search);
criterion_main!(benches);
