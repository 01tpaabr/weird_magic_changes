//! `make bench` / `cargo bench -p sim-core`
//! Baseline numbers live in docs/PERF.md; update them when you change the hot path.
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use sim_core::stage::worldgen::{GenParams, generate};

fn bench_generate(c: &mut Criterion) {
    let mut g = c.benchmark_group("stage_generate");
    let p = GenParams::default();
    for &side in &[256u32, 2048] {
        g.throughput(Throughput::Elements(u64::from(side) * u64::from(side)));
        g.bench_function(format!("{side}x{side}"), |b| {
            b.iter(|| generate(side, side, 7, &p));
        });
    }
    g.finish();
}

fn bench_checksum(c: &mut Criterion) {
    let mut g = c.benchmark_group("stage_checksum");
    let s = generate(2048, 2048, 7, &GenParams::default());
    g.throughput(Throughput::Elements(s.len() as u64));
    g.bench_function("2048x2048", |b| b.iter(|| s.checksum()));
    g.finish();
}

criterion_group!(benches, bench_generate, bench_checksum);
criterion_main!(benches);
