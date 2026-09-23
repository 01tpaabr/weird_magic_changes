//! `make bench` / `cargo bench -p sim-core`
//! Baseline numbers live in docs/PERF.md; update them when you change the hot path.
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use sim_core::World;

fn bench_step(c: &mut Criterion) {
    let mut g = c.benchmark_group("world_step");
    for &n in &[10_000usize, 1_000_000] {
        g.throughput(Throughput::Elements(n as u64));
        g.bench_function(format!("n={n}"), |b| {
            let mut w = World::new(n, 7);
            b.iter(|| w.step(1.0 / 60.0));
        });
    }
    g.finish();
}

criterion_group!(benches, bench_step);
criterion_main!(benches);
