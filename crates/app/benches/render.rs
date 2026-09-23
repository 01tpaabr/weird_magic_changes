//! Frame pipeline benches: `render_cells` (phase 1) and `grid::upload`
//! (phase 2), sized like a 2560x1440 window at a 16 px cell. Thread count
//! comes from `WMC_THREADS` (default: all cores); run twice to compare.

use bevy::sprite_render::TilemapChunkTileData;
use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};

use app::render::cells::{CellFrame, Viewport, render_cells};
use app::render::grid;
use sim_core::stage::worldgen::GenParams;
use sim_core::{Pos, WorldConfig, par, sim, stage};

const COLS: u32 = 160;
const ROWS: u32 = 90;

fn world() -> bevy::ecs::world::World {
    sim::new_world(&WorldConfig {
        width: 512,
        height: 256,
        seed: 7,
        params: GenParams::default(),
    })
}

fn bench_cells(c: &mut Criterion) {
    let world = world();
    let threads = par::thread_count();
    let view = Viewport::centered(Pos::new(200, 100), COLS, ROWS);
    let mut frame = CellFrame::new();
    frame.resize(COLS as usize, ROWS as usize);
    let mut g = c.benchmark_group("render_cells");
    g.throughput(Throughput::Elements(u64::from(COLS * ROWS)));
    g.bench_with_input(BenchmarkId::new("160x90", threads), &threads, |b, _| {
        b.iter(|| render_cells(|cc| stage::chunk(&world, cc), view, 200, &mut frame));
    });
    g.finish();
}

fn bench_upload(c: &mut Criterion) {
    let world = world();
    let view = Viewport::centered(Pos::new(200, 100), COLS, ROWS);
    let mut frame = CellFrame::new();
    frame.resize(COLS as usize, ROWS as usize);
    render_cells(|cc| stage::chunk(&world, cc), view, 255, &mut frame);
    let n = (COLS * ROWS) as usize;
    let mut bg = TilemapChunkTileData(vec![None; n]);
    let mut fg = TilemapChunkTileData(vec![None; n]);
    let mut g = c.benchmark_group("grid_upload");
    g.throughput(Throughput::Elements(u64::from(COLS * ROWS)));
    g.bench_function("160x90", |b| {
        b.iter(|| grid::upload(&frame, &mut bg, &mut fg))
    });
    g.finish();
}

criterion_group!(benches, bench_cells, bench_upload);
criterion_main!(benches);
