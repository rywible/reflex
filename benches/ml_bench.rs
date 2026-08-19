use criterion::{Criterion, black_box, criterion_group, criterion_main};
use reflex_ml_micro::MicroMlp;

pub fn bench_micro_mlp(c: &mut Criterion) {
    let mlp = MicroMlp::random(64, 32, 42);
    let features = vec![0.5f32; 64 * 64];
    let mut out = vec![0.0f32; 64];
    let mut scratch = vec![0.0f32; 64 * 32];

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
