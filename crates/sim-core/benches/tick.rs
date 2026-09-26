//! `make bench` / `cargo bench -p sim-core`
//! Baseline numbers live in docs/PERF.md; update them when you change the hot path.
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use sim_core::scenario::Placement;
use sim_core::stage::worldgen::generate_many;
use sim_core::{ChunkCoord, Kinds, LoadPolicy, Pos, Scenario, Stage, sim};

/// The built-in scenario (its starts and terrain) at seed 7 and this size.
fn scenario(width: u32, height: u32) -> Scenario {
    Scenario {
        seed: 7,
        width,
        height,
        ..Scenario::builtin()
    }
}

fn grid(side: i32) -> Vec<ChunkCoord> {
    (0..side)
        .flat_map(|y| (0..side).map(move |x| ChunkCoord::new(x, y)))
        .collect()
}

fn bench_generate(c: &mut Criterion) {
    sim_core::par::init_task_pool();
    let mut g = c.benchmark_group("generate_many");
    let s = scenario(0, 0);
    let placement = Placement::resolve(&s.starts, &Kinds::builtin(), &s.terrain()).unwrap();
    for &side in &[4i32, 32] {
        let coords = grid(side);
        g.throughput(Throughput::Elements(coords.len() as u64 * 4096));
        g.bench_function(format!("{side}x{side} chunks"), |b| {
            b.iter(|| generate_many(&s.terrain(), &placement, &coords));
        });
    }
    g.finish();
}

fn bench_checksum(c: &mut Criterion) {
    let mut g = c.benchmark_group("stage_checksum");
    let mut w = sim::new_world(&scenario(2048, 2048));
    let n = w.resource::<Stage>().loaded_count();
    g.throughput(Throughput::Elements(n as u64 * 4096));
    g.bench_function("32x32 chunks", |b| {
        b.iter(|| sim_core::stage::checksum(&mut w))
    });
    g.finish();
}

fn bench_stream(c: &mut Criterion) {
    // A camera sweeping right one chunk per frame: the per-frame streaming cost.
    let mut g = c.benchmark_group("ensure_loaded");
    let policy = LoadPolicy { load: 2, unload: 4 };
    g.bench_function("sweep 1 chunk/frame, radius 2", |b| {
        let mut w = sim::new_world(&scenario(64, 64));
        let mut x = 0i32;
        b.iter(|| {
            x += 64;
            sim::ensure_loaded(&mut w, Pos::new(x, 0), policy, None).unwrap()
        });
    });
    g.finish();
}

fn bench_step(c: &mut Criterion) {
    // One tick over the initial region: the number every new system moves.
    let mut g = c.benchmark_group("step");
    let mut w = sim::new_world(&scenario(1024, 1024));
    g.throughput(Throughput::Elements(256 * 4096));
    g.bench_function("16x16 chunks", |b| b.iter(|| sim::step(&mut w)));
    g.finish();
}

criterion_group!(
    benches,
    bench_generate,
    bench_checksum,
    bench_stream,
    bench_step
);
criterion_main!(benches);
