//! Frame pipeline benches: `render_cells` (phase 1) and `blit` (phase 2), the
//! latter as scalar Rust reference, Zig kernel on one thread, and the
//! band-parallel driver. Sized like a 2560x1440 window at a 16 px cell.

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use app::render::atlas::GlyphAtlas;
use app::render::blit::{blit, blit_reference};
use app::render::cells::{CellFrame, Viewport, render_cells};
use sim_core::stage::worldgen::GenParams;
use sim_core::{Pos, World, WorldConfig};

const COLS: u32 = 160;
const ROWS: u32 = 90;
const CELL: u32 = 16;

fn world() -> World {
    World::new(&WorldConfig {
        width: 512,
        height: 256,
        seed: 7,
        params: GenParams::default(),
    })
}

fn pool(threads: usize) -> rayon::ThreadPool {
    rayon::ThreadPoolBuilder::new()
        .num_threads(threads)
        .build()
        .unwrap()
}

fn bench_cells(c: &mut Criterion) {
    let world = world();
    let view = Viewport::centered(Pos::new(200, 100), COLS, ROWS);
    let mut frame = CellFrame::new();
    frame.resize(COLS as usize, ROWS as usize);
    let mut g = c.benchmark_group("render_cells");
    g.throughput(Throughput::Elements(u64::from(COLS * ROWS)));
    for threads in [1, 8] {
        let p = pool(threads);
        g.bench_with_input(BenchmarkId::new("160x90", threads), &threads, |b, _| {
            b.iter(|| p.install(|| render_cells(&world.stage, view, &mut frame)));
        });
    }
    g.finish();
}

fn bench_blit(c: &mut Criterion) {
    let world = world();
    let view = Viewport::centered(Pos::new(200, 100), COLS, ROWS);
    let mut frame = CellFrame::new();
    frame.resize(COLS as usize, ROWS as usize);
    render_cells(&world.stage, view, &mut frame);
    let atlas = GlyphAtlas::build(CELL);
    let stride = (COLS * CELL) as usize;
    let mut out = vec![0u8; stride * (ROWS * CELL) as usize * 4];

    let mut g = c.benchmark_group("blit");
    g.throughput(Throughput::Elements(u64::from(COLS * CELL * ROWS * CELL)));
    g.bench_function("reference/1", |b| {
        b.iter(|| blit_reference(&frame, &atlas, &mut out, stride));
    });
    for threads in [1, 8] {
        let p = pool(threads);
        g.bench_with_input(BenchmarkId::new("zig", threads), &threads, |b, _| {
            b.iter(|| p.install(|| blit(&frame, &atlas, &mut out, stride)));
        });
    }
    g.finish();
}

criterion_group!(benches, bench_cells, bench_blit);
criterion_main!(benches);
