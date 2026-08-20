use criterion::{Criterion, criterion_group, criterion_main};
use reflex_ml_micro::MicroMlp;
use std::hint::black_box;

pub fn bench_micro_mlp(c: &mut Criterion) {
    let mlp = MicroMlp::reference_mlp_2607(42);
    assert_eq!(mlp.parameter_count(), 2607);
    let features = vec![0.5f32; 64 * 64];
    let mut out = vec![0.0f32; 64];
    let mut scratch = vec![0.0f32; 64 * 49 * 2];

    c.bench_function("micro_mlp_2607_batch64", |b| {
        b.iter(|| {
            mlp.score_rows(
                black_box(&features),
                black_box(64),
                black_box(&mut out),
                black_box(&mut scratch),
            );
        })
    });
}

criterion_group!(benches, bench_micro_mlp);
criterion_main!(benches);
