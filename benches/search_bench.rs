use criterion::{Criterion, black_box, criterion_group, criterion_main};
use reflex_domain_bitvec::{BitvecDomain, BitvecTask, BvExpr};
use reflex_search::{BudgetSet, SearchKernel, UniformPolicy};

pub fn bench_search(c: &mut Criterion) {
    let domain = BitvecDomain::new();
    let task = BitvecTask {
        initial: BvExpr::Xor(Box::new(BvExpr::Var(0)), Box::new(BvExpr::Var(0))),
        target_max_cost: 1,
    };
    let uniform = UniformPolicy;

    c.bench_function("search_bitvec_uniform", |b| {
        b.iter(|| {
            let mut search = SearchKernel::new(&domain, &uniform, BudgetSet::default_for_test());
            black_box(search.run(black_box(&task)).unwrap());
        })
    });
}

criterion_group!(benches, bench_search);
criterion_main!(benches);
