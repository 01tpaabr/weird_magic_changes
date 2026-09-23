//! `make bench` / `cargo bench -p sim-core`
//! Baseline numbers live in docs/PERF.md; update them when you change the hot path.
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use sim_core::stage::worldgen::{GenParams, generate_many};
use sim_core::{ChunkCoord, LoadPolicy, Pos, World, WorldConfig};

fn grid(side: i32) -> Vec<ChunkCoord> {
    (0..side)
        .flat_map(|y| (0..side).map(move |x| ChunkCoord::new(x, y)))
        .collect()
}

fn bench_generate(c: &mut Criterion) {
    let mut g = c.benchmark_group("generate_many");
    let p = GenParams::default();
    for &side in &[4i32, 32] {
        let coords = grid(side);
        g.throughput(Throughput::Elements(coords.len() as u64 * 4096));
        g.bench_function(format!("{side}x{side} chunks"), |b| {
            b.iter(|| generate_many(7, &p, &coords));
        });
    }
    g.finish();
}

fn bench_checksum(c: &mut Criterion) {
    let mut g = c.benchmark_group("stage_checksum");
    let w = World::new(&WorldConfig {
        seed: 7,
        width: 2048,
        height: 2048,
        params: GenParams::default(),
    });
    g.throughput(Throughput::Elements(w.stage.loaded_count() as u64 * 4096));
    g.bench_function("32x32 chunks", |b| b.iter(|| w.stage.checksum()));
    g.finish();
}

fn bench_stream(c: &mut Criterion) {
    // A camera sweeping right one chunk per frame: the per-frame streaming cost.
    let mut g = c.benchmark_group("ensure_loaded");
    let policy = LoadPolicy { load: 2, unload: 4 };
    g.bench_function("sweep 1 chunk/frame, radius 2", |b| {
        let mut w = World::new(&WorldConfig {
            seed: 7,
            width: 64,
            height: 64,
            params: GenParams::default(),
        });
        let mut x = 0i32;
        b.iter(|| {
            x += 64;
            w.ensure_loaded(Pos::new(x, 0), policy, None).unwrap()
        });
    });
    g.finish();
}

criterion_group!(benches, bench_generate, bench_checksum, bench_stream);
criterion_main!(benches);
