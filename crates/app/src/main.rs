//! Entry point.
//!
//! ```text
//! wmc show [width] [height] [seed]             print the initial region once and exit
//! wmc play <save_dir> [width height seed]      open a window: WASD camera, streaming, saves
//! wmc run <save_dir> <ticks> [width height seed] step the world headless, print rate + checksum
//! ```
//! `play` and `run` open the world in `save_dir` if one exists (size/seed args
//! are then ignored), otherwise create it. Defaults: 80 24 42. `run` never
//! saves: run it twice, or with `RAYON_NUM_THREADS=1` and again without, and
//! the checksums must match.
use std::io::Write;
use std::time::Instant;

use anyhow::{Context, bail};
use sim_core::stage::worldgen::GenParams;
use sim_core::time::Clock;
use sim_core::{Feature, Ground, LoadPolicy, Pos, Store, World, WorldConfig};

use app::camera::Camera;
use app::render::ascii::render;
use app::render::cells::Viewport;
use app::window;

fn main() -> anyhow::Result<()> {
    if let Err(v) = zig_kernels::check_abi() {
        bail!(
            "Zig kernel ABI mismatch: rust={} zig={v}",
            zig_kernels::ABI_VERSION
        );
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("show") => show(&config(&args[1..])?),
        Some("play") => {
            let dir = args.get(1).context("play needs a save directory")?;
            play(dir, &config(&args[2..])?)
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
    let world = World::new(cfg);
    let gen_time = t0.elapsed();

    let stage = &world.stage;
    let (mut water, mut rocks, mut n) = (0usize, 0usize, 0usize);
    for &s in stage.active() {
        let c = &stage.cells[s as usize];
        n += c.ground.len();
        water += c.ground.iter().filter(|g| **g == Ground::Water).count();
        rocks += c.feature.iter().filter(|f| **f == Feature::Rock).count();
    }
    let view = Viewport {
        origin: Pos::new(0, 0),
        width: cfg.width,
        height: cfg.height,
    };

    let mut out = std::io::stdout().lock();
    out.write_all(render(stage, view).as_bytes())?;
    writeln!(out)?;
    writeln!(
        out,
        "stage:     {}x{} seed={} ({} chunks, {n} cells)",
        cfg.width,
        cfg.height,
        cfg.seed,
        stage.loaded_count()
    )?;
    writeln!(
        out,
        "water:     {water} ({:.1}%)",
        100.0 * water as f64 / n as f64
    )?;
    writeln!(out, "rocks:     {rocks}")?;
    writeln!(out, "generate:  {gen_time:.2?}")?;
    writeln!(out, "checksum:  {:016x}", world.checksum())?;
    Ok(())
}

/// The saved camera, or the middle of the initial region.
fn camera_for(world: &World, store: &Store) -> Camera {
    Camera::load(store.dir()).unwrap_or_else(|| {
        Camera::new(Pos::new(
            i32::try_from(world.initial_width / 2).unwrap_or(0),
            i32::try_from(world.initial_height / 2).unwrap_or(0),
        ))
    })
}

fn play(dir: &str, cfg: &WorldConfig) -> anyhow::Result<()> {
    let store = Store::open(dir).with_context(|| format!("opening save dir {dir}"))?;
    let mut world = match World::open(&store).context("reading save")? {
        Some(w) => w,
        None => {
            let w = World::new(cfg);
            store.write_meta(&w.meta()).context("writing save meta")?;
            w
        }
    };
    let mut camera = camera_for(&world, &store);
    window::run(&mut world, &mut camera, &store)
}

/// Headless: the determinism check and the tick benchmark. A saved world
/// loads the chunks around its camera; a new one keeps its initial region.
fn run(dir: &str, ticks: u64, cfg: &WorldConfig) -> anyhow::Result<()> {
    let store = Store::open(dir).with_context(|| format!("opening save dir {dir}"))?;
    let mut world = match World::open(&store).context("reading save")? {
        Some(mut w) => {
            let camera = camera_for(&w, &store);
            w.ensure_loaded(camera.cell(), LoadPolicy::default(), Some(&store))
                .context("streaming chunks")?;
            w
        }
        None => World::new(cfg),
    };
    let from = world.tick;
    let t0 = Instant::now();
    for _ in 0..ticks {
        world.step();
    }
    let wall = t0.elapsed();

    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "ticks:     {ticks} ({from} -> {}; {} -> {})",
        world.tick,
        Clock::at(from),
        Clock::at(world.tick)
    )?;
    writeln!(out, "chunks:    {}", world.stage.loaded_count())?;
    writeln!(
        out,
        "wall:      {wall:.2?} ({:.2} µs/tick, {} threads)",
        wall.as_secs_f64() * 1e6 / ticks.max(1) as f64,
        rayon::current_num_threads()
    )?;
    writeln!(out, "checksum:  {:016x}", world.checksum())?;
    Ok(())
}
