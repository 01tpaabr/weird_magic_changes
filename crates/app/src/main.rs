//! Entry point.
//!
//! ```text
//! wmc show [width] [height] [seed]             print the initial region once and exit
//! wmc play <save_dir> [width height seed]      open a window: WASD camera, streaming, saves
//! wmc run <save_dir> <ticks> [width height seed] step the world headless, print rate + checksum
//! wmc lint <rules dir or file>                 compile a rule set and print what it holds
//! wmc why [-v] <save_dir> <x> <y> [ticks [w h seed]]
//!                                              step `ticks`, then explain the next think of
//!                                              the actor at (x, y); `-v` lists every op
//! ```
//! `--scenario <file>` (any command) makes a new world from that scenario
//! (`scenarios/*.scenario`: seed, size, terrain, where kinds start) instead
//! of the built-in `scenarios/default.scenario`; `[width height seed]`
//! override its size and seed. `lint` checks the scenario against the rules.
//! `WMC_RULES=<dir>` makes `show`, `play` and `run` use that directory's
//! rules instead of the built-in ones.
//! `play` and `run` open the world in `save_dir` if one exists (scenario and
//! size/seed args are then ignored), otherwise create it. `run` never
//! saves: run it twice, or with `WMC_THREADS=1` and again without, and the
//! checksums must match. `show` and `run` are headless: a bare `bevy_ecs`
//! world, no `App`.
use std::io::Write;
use std::time::Instant;

use anyhow::{Context, bail};
use sim_core::actors::{Tally, life};
use sim_core::scenario::{Placement, Start};
use sim_core::time::Clock;
use sim_core::{ChunkActors, ChunkCells, Feature, Ground, Kinds, LoadPolicy, Pos, Stage, Store};
use sim_core::{Scenario, par, sim, stage};

use bevy::prelude::World;

use app::play;
use app::render::ascii::render;
use app::render::cells::Viewport;

fn main() -> anyhow::Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    // `--scenario <file>` may stand anywhere after the command.
    let file = match args.iter().position(|a| a == "--scenario") {
        Some(i) => {
            let f = args.get(i + 1).context("--scenario needs a file")?.clone();
            args.drain(i..i + 2);
            Some(f)
        }
        None => None,
    };
    let file = file.as_deref();
    match args.first().map(String::as_str) {
        Some("show") => show(&config(file, &args[1..])?),
        Some("play") => {
            let dir = args.get(1).context("play needs a save directory")?;
            let (scenario, name) = config(file, &args[2..])?;
            play::run(dir, &scenario, &name)
        }
        Some("run") => {
            let dir = args.get(1).context("run needs a save directory")?;
            let ticks = args
                .get(2)
                .context("run needs a tick count")?
                .parse()
                .context("bad tick count")?;
            run(dir, ticks, &config(file, &args[3..])?)
        }
        Some("why") => {
            let verbose = args.get(1).is_some_and(|a| a == "-v");
            let a = &args[1 + usize::from(verbose)..];
            let dir = a.first().context("why needs a save directory")?;
            let coord = |i: usize, what: &str| -> anyhow::Result<i32> {
                a.get(i)
                    .with_context(|| format!("why needs {what}"))?
                    .parse()
                    .with_context(|| format!("bad {what}"))
            };
            let (x, y) = (coord(1, "x")?, coord(2, "y")?);
            let ticks = a
                .get(3)
                .map_or(Ok(0), |t| t.parse())
                .context("bad tick count")?;
            why(
                dir,
                Pos::new(x, y),
                ticks,
                verbose,
                &config(file, a.get(4..).unwrap_or(&[]))?,
            )
        }
        Some("lint") => lint(
            args.get(1)
                .context("lint needs a rules directory or file")?,
            file,
        ),
        Some(other) => {
            bail!("unknown command {other:?}; use `show`, `play`, `run`, `why` or `lint`")
        }
        None => bail!(
            "usage: wmc show [w h seed] | wmc play <dir> [w h seed] | wmc run <dir> <ticks> [w h seed] | wmc why [-v] <dir> <x> <y> [ticks [w h seed]] | wmc lint <rules>; any takes --scenario <file>"
        ),
    }
}

/// A scenario file, else the built-in one.
fn scenario(file: Option<&str>) -> anyhow::Result<Scenario> {
    match file {
        Some(f) => {
            let text = std::fs::read_to_string(f).with_context(|| format!("reading {f}"))?;
            Ok(Scenario::parse(f, &text)?)
        }
        None => Ok(Scenario::builtin()),
    }
}

/// The scenario for a new world, with `[width height seed]` over its own.
fn config(file: Option<&str>, args: &[String]) -> anyhow::Result<(Scenario, String)> {
    let mut s = scenario(file)?;
    let num = |i: usize, what: &str| -> anyhow::Result<Option<u64>> {
        args.get(i)
            .map(|a| a.parse().with_context(|| format!("bad {what}")))
            .transpose()
    };
    if let Some(w) = num(0, "width")? {
        s.width = u32::try_from(w).context("width")?;
    }
    if let Some(h) = num(1, "height")? {
        s.height = u32::try_from(h).context("height")?;
    }
    if let Some(seed) = num(2, "seed")? {
        s.seed = seed;
    }
    Ok((s, file.unwrap_or("default.scenario").to_string()))
}

/// A new world of `scenario` under `kinds`; says which file does not fit.
fn new_world(scenario: &(Scenario, String), kinds: Kinds) -> anyhow::Result<World> {
    sim::new_world_with(&scenario.0, kinds).map_err(|e| {
        let hint = if scenario.1 == "default.scenario" {
            " (these rules need their own scenario: --scenario <file>)"
        } else {
            ""
        };
        anyhow::anyhow!("{}: {e}{hint}", scenario.1)
    })
}

fn show(cfg: &(Scenario, String)) -> anyhow::Result<()> {
    let kinds = app::rules()?;
    let t0 = Instant::now();
    let mut world = new_world(cfg, kinds)?;
    let gen_time = t0.elapsed();
    let cfg = &cfg.0;

    let (mut water, mut rocks, mut actors, mut n) = (0usize, 0usize, 0usize, 0usize);
    for (c, a) in world.query::<(&ChunkCells, &ChunkActors)>().iter(&world) {
        n += c.ground.len();
        water += c.ground.iter().filter(|g| **g == Ground::Water).count();
        rocks += c.feature.iter().filter(|f| **f == Feature::Rock).count();
        actors += a.rows.len();
    }
    let view = Viewport {
        origin: Pos::new(0, 0),
        width: cfg.width,
        height: cfg.height,
    };
    let glyphs = world.resource::<Kinds>().glyphs.clone();
    let text = render(|c| stage::chunk(&world, c), &glyphs, view);
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
    writeln!(out, "actors:    {actors}")?;
    writeln!(
        out,
        "generate:  {gen_time:.2?} ({} threads)",
        par::thread_count()
    )?;
    writeln!(out, "checksum:  {checksum:016x}")?;
    Ok(())
}

/// The world saved in `dir` with the chunks around its camera, else a new
/// one of `cfg` (its initial region loaded). Never saved by the caller.
fn open_or_new(dir: &str, cfg: &(Scenario, String)) -> anyhow::Result<World> {
    let store = Store::open(dir).with_context(|| format!("opening save dir {dir}"))?;
    let kinds = app::rules()?;
    Ok(
        match sim::open_world_with(&store, kinds.clone()).context("reading save")? {
            Some(mut w) => {
                let camera = play::camera_for(&w, &store);
                sim::ensure_loaded(&mut w, camera.cell(), LoadPolicy::default(), Some(&store))
                    .context("streaming chunks")?;
                w
            }
            None => new_world(cfg, kinds)?,
        },
    )
}

/// `wmc why`: step `ticks`, then wait (up to a day) for the actor at `p`
/// to be due, explain that think, and run it to show where it went.
fn why(dir: &str, p: Pos, ticks: u64, ops: bool, cfg: &(Scenario, String)) -> anyhow::Result<()> {
    let mut world = open_or_new(dir, cfg)?;
    for _ in 0..ticks {
        sim::step(&mut world);
    }
    let Some(first) = sim::explain(&world, p) else {
        bail!(
            "nobody at ({}, {}) at tick {} (not loaded, or an empty cell)",
            p.x,
            p.y,
            sim::tick(&world)
        );
    };
    let uid = first.before.uid;
    let mut at = p;
    let mut waited = 0u64;
    while !sim::explain(&world, at).is_some_and(|e| e.due) {
        if waited == sim_core::TICKS_PER_DAY {
            bail!("it did not think within a day");
        }
        sim::step(&mut world);
        waited += 1;
        at =
            sim::find_uid(&mut world, uid).context("it died or was eaten before its next think")?;
    }
    let mut out = std::io::stdout().lock();
    if waited > 0 {
        writeln!(out, "(stepped {waited} ticks to its next think)")?;
    }
    write!(
        out,
        "{}",
        app::why::report(&world, at, ops).expect("found it there")
    )?;
    sim::step(&mut world);
    writeln!(out, "after   {}", app::why::after(&mut world, uid))?;
    Ok(())
}

/// Headless: the determinism check and the tick benchmark. A saved world
/// loads the chunks around its camera; a new one keeps its initial region.
fn run(dir: &str, ticks: u64, cfg: &(Scenario, String)) -> anyhow::Result<()> {
    let mut world = open_or_new(dir, cfg)?;
    let from = sim::tick(&world);
    let t0 = Instant::now();
    for _ in 0..ticks {
        sim::step(&mut world);
    }
    let wall = t0.elapsed();
    let to = sim::tick(&world);
    let loaded = world.resource::<Stage>().loaded_count();
    let kinds = world.resource::<Kinds>().clone();
    let mut per_kind = vec![0usize; kinds.len()];
    for a in world.query::<&ChunkActors>().iter(&world) {
        for r in &a.rows {
            per_kind[usize::from(r.kind)] += 1;
        }
    }
    let actors: usize = per_kind.iter().sum();
    let by_kind: Vec<String> = kinds
        .names()
        .zip(&per_kind)
        .map(|(n, c)| format!("{n} {c}"))
        .collect();
    let checksum = sim::checksum(&mut world);
    // The world alone, without the rules hash: equal across rule sets that
    // compile to the same behaviour (the step-8 gates compare this).
    let state = stage::checksum(&mut world);

    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "ticks:     {ticks} ({from} -> {to}; {} -> {})",
        Clock::at(from),
        Clock::at(to)
    )?;
    writeln!(
        out,
        "chunks:    {loaded} ({actors} actors: {})",
        by_kind.join(", ")
    )?;
    writeln!(
        out,
        "wall:      {wall:.2?} ({:.2} µs/tick, {} threads)",
        wall.as_secs_f64() * 1e6 / ticks.max(1) as f64,
        par::thread_count()
    )?;
    writeln!(out, "checksum:  {checksum:016x}")?;
    writeln!(out, "state:     {state:016x}")?;
    let tally = world.resource::<Tally>();
    writeln!(
        out,
        "life:      {:<10} {:>8} {:>8} {:>8} {:>8} {:>8} {:>10} {:>9} {:>6}",
        "kind", "alive", "born", "became", "eaten", "died", "thinks", "ops/think", "traps"
    )?;
    for (i, name) in kinds.names().enumerate() {
        let k = i as u16;
        let thinks = tally.get(k, life::THINKS);
        writeln!(
            out,
            "           {name:<10} {:>8} {:>8} {:>8} {:>8} {:>8} {:>10} {:>9.1} {:>6}",
            per_kind[i],
            tally.get(k, life::BORN),
            tally.get(k, life::BECAME),
            tally.get(k, life::EATEN),
            tally.get(k, life::DIED),
            thinks,
            tally.get(k, life::OPS) as f64 / thinks.max(1) as f64,
            tally.get(k, life::TRAPS),
        )?;
    }
    Ok(())
}

/// Compile a rule set and print its kind table: the fast way to check a
/// rules file before a world runs it. With a scenario, check that its
/// starts fit the rules too.
fn lint(path: &str, scenario_file: Option<&str>) -> anyhow::Result<()> {
    let p = std::path::Path::new(path);
    let kinds = if p.is_dir() {
        sim_core::rules::compile_dir(p).map_err(|e| anyhow::anyhow!("{e}"))?
    } else {
        let text = std::fs::read_to_string(p).with_context(|| format!("reading {path}"))?;
        let name = p
            .file_name()
            .map_or(path.to_string(), |n| n.to_string_lossy().into_owned());
        sim_core::rules::compile(&name, &text)?
    };
    let mut out = std::io::stdout().lock();
    if !kinds.debug.traits.is_empty() {
        writeln!(out, "traits {}", kinds.debug.traits.join(", "))?;
    }
    for k in &kinds.defs {
        let needs: Vec<String> = k
            .needs
            .iter()
            .map(|n| {
                format!(
                    "{} max {}{}{}",
                    n.name,
                    n.max,
                    if n.decays { "" } else { " decay 0" },
                    if n.vital { " vital" } else { "" }
                )
            })
            .collect();
        writeln!(
            out,
            "kind {:<12} glyph {:?} cadence {:<5} sight {:<2} fuel {:<4} entry {:<5} needs [{}] mem [{}]",
            k.name,
            char::from(k.glyph),
            k.cadence(),
            k.sight,
            k.fuel,
            k.entry,
            needs.join(", "),
            k.mems.join(", "),
        )?;
        let parents = kinds
            .debug
            .parents
            .get(usize::from(k.id))
            .filter(|p| !p.is_empty());
        let end = kinds.family_end[usize::from(k.id)];
        if parents.is_some() || end > k.id + 1 {
            let family: Vec<&str> = (k.id + 1..end)
                .map(|d| kinds.defs[usize::from(d)].name.as_str())
                .collect();
            let mut line = format!("     {:<12}", "");
            if let Some(p) = parents {
                line.push_str(&format!(" extends {}", p.join(", ")));
            }
            if !family.is_empty() {
                line.push_str(&format!(" family: {}", family.join(", ")));
            }
            writeln!(out, "{line}")?;
        }
    }
    for d in &kinds.debug.diagnostics {
        writeln!(out, "{d}")?;
    }
    let warnings = kinds
        .debug
        .diagnostics
        .iter()
        .filter(|d| d.level == sim_core::rules::Level::Warning)
        .count();
    writeln!(
        out,
        "{} kinds, {} traits, {} ops, {} consts, {} subs, {warnings} warnings, hash {:016x}",
        kinds.len(),
        kinds.debug.traits.len(),
        kinds.code.len(),
        kinds.consts.len(),
        kinds.subs.len(),
        kinds.hash
    )?;
    if let Some(f) = scenario_file {
        let s = scenario(Some(f))?;
        Placement::resolve(&s.starts, &kinds, s.seed, &s.params)
            .map_err(|e| anyhow::anyhow!("{f}: {e}"))?;
        let (mut share, mut at) = (0.0, 0);
        for st in &s.starts {
            match st {
                Start::Share { num, den, .. } => share += f64::from(*num) / f64::from(*den),
                Start::At { .. } => at += 1,
            }
        }
        writeln!(
            out,
            "scenario {f}: seed {} size {}x{}, {} shares ({:.2}% of walkable cells), {at} explicit starts",
            s.seed,
            s.width,
            s.height,
            s.starts.len() - at,
            share * 100.0
        )?;
    }
    Ok(())
}
