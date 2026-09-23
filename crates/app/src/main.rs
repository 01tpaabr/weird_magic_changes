//! Entry point.
//!
//! ```text
//! wmc show [width] [height] [seed]             print the initial region once and exit
//! wmc play <save_dir> [width height seed]      open a window: WASD camera, streaming, saves
//! wmc run <save_dir> <ticks> [width height seed] step the world headless, print rate + checksum
//! ```
//! `play` and `run` open the world in `save_dir` if one exists (size/seed args
//! are then ignored), otherwise create it. Defaults: 80 24 42. `run` never
//! saves: run it twice, or with `WMC_THREADS=1` and again without, and the
//! checksums must match. `show` and `run` are headless: a bare `bevy_ecs`
//! world, no `App`.
use std::io::Write;
use std::time::Instant;

use anyhow::{Context, bail};
use sim_core::stage::worldgen::GenParams;
use sim_core::time::Clock;
use sim_core::{ChunkCells, Feature, Ground, LoadPolicy, Pos, Stage, Store, WorldConfig};
use sim_core::{par, sim, stage};

use app::play;
use app::render::ascii::render;
use app::render::cells::Viewport;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("show") => show(&config(&args[1..])?),
        Some("play") => {
            let dir = args.get(1).context("play needs a save directory")?;
            play::run(dir, &config(&args[2..])?)
        }
        Some("run") => {
            let dir = args.get(1).context("run needs a save directory")?;
            let ticks = args
                .get(2)
                .context("run needs a tick count")?
                .parse()
                .context("bad tick count")?;
            run(dir, ticks, &config(&args[3..])?)
        }
        Some(other) => bail!("unknown command {other:?}; use `show`, `play` or `run`"),
        None => bail!(
            "usage: wmc show [w h seed] | wmc play <dir> [w h seed] | wmc run <dir> <ticks> [w h seed]"
        ),
    }
}

fn config(args: &[String]) -> anyhow::Result<WorldConfig> {
    let num = |i: usize, default: u64, what: &str| -> anyhow::Result<u64> {
        args.get(i)
            .map_or(Ok(default), |s| s.parse())
            .with_context(|| format!("bad {what}"))
    };
    Ok(WorldConfig {
        width: u32::try_from(num(0, 80, "width")?).context("width")?,
        height: u32::try_from(num(1, 24, "height")?).context("height")?,
        seed: num(2, 42, "seed")?,
        params: GenParams::default(),
    })
}

fn show(cfg: &WorldConfig) -> anyhow::Result<()> {
    let t0 = Instant::now();
    let mut world = sim::new_world(cfg);
    let gen_time = t0.elapsed();

    let (mut water, mut rocks, mut n) = (0usize, 0usize, 0usize);
    for c in world.query::<&ChunkCells>().iter(&world) {
        n += c.ground.len();
        water += c.ground.iter().filter(|g| **g == Ground::Water).count();
        rocks += c.feature.iter().filter(|f| **f == Feature::Rock).count();
    }
    let view = Viewport {
        origin: Pos::new(0, 0),
        width: cfg.width,
        height: cfg.height,
    };
    let text = render(|c| stage::chunk(&world, c), view);
    let loaded = world.resource::<Stage>().loaded_count();
    let checksum = sim::checksum(&mut world);

    let mut out = std::io::stdout().lock();
    out.write_all(text.as_bytes())?;
    writeln!(out)?;
    writeln!(
        out,
        "stage:     {}x{} seed={} ({loaded} chunks, {n} cells)",
        cfg.width, cfg.height, cfg.seed,
    )?;
    writeln!(
        out,
        "water:     {water} ({:.1}%)",
        100.0 * water as f64 / n as f64
    )?;
    writeln!(out, "rocks:     {rocks}")?;
    writeln!(
        out,
        "generate:  {gen_time:.2?} ({} threads)",
        par::thread_count()
    )?;
    writeln!(out, "checksum:  {checksum:016x}")?;
    Ok(())
}

/// Headless: the determinism check and the tick benchmark. A saved world
/// loads the chunks around its camera; a new one keeps its initial region.
fn run(dir: &str, ticks: u64, cfg: &WorldConfig) -> anyhow::Result<()> {
    let store = Store::open(dir).with_context(|| format!("opening save dir {dir}"))?;
    let mut world = match sim::open_world(&store).context("reading save")? {
        Some(mut w) => {
            let camera = play::camera_for(&w, &store);
            sim::ensure_loaded(&mut w, camera.cell(), LoadPolicy::default(), Some(&store))
                .context("streaming chunks")?;
            w
        }
        None => sim::new_world(cfg),
    };
    let from = sim::tick(&world);
    let t0 = Instant::now();
    for _ in 0..ticks {
        sim::step(&mut world);
    }
    let wall = t0.elapsed();
    let to = sim::tick(&world);
    let loaded = world.resource::<Stage>().loaded_count();
    let checksum = sim::checksum(&mut world);

    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "ticks:     {ticks} ({from} -> {to}; {} -> {})",
        Clock::at(from),
        Clock::at(to)
    )?;
    writeln!(out, "chunks:    {loaded}")?;
    writeln!(
        out,
        "wall:      {wall:.2?} ({:.2} µs/tick, {} threads)",
        wall.as_secs_f64() * 1e6 / ticks.max(1) as f64,
        par::thread_count()
    )?;
    writeln!(out, "checksum:  {checksum:016x}")?;
    Ok(())
}
