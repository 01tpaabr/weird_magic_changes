//! Entry point.
//!
//! ```text
//! wmc                                          the main menu (`app::menu`), over the gallery
//! wmc show [width] [height] [seed]             print the initial region once and exit
//! wmc play <save_dir> [width height seed]      open a window: WASD camera, streaming, saves
//! wmc run <save_dir> <time> [width height seed]  step the world headless, print rate + checksum
//! wmc lint <pack>...                           compile rule packs and print what they hold
//! wmc scenario <file>                          a scenario test: make its world, then its
//!                                              `run` and `expect` lines; exit 1 if one fails
//!                                              (on each of its seeds: --seeds N, --pass M)
//! wmc why [-v] <save_dir> <x> <y> [time [w h seed]]
//!                                              step `time`, then explain the next think of
//!                                              the actor at (x, y); `-v` lists every op
//! wmc lab <file>                               the scenario test in <file> for every combination
//!                                              of `--set NAME=a,b,c` values, on each of its seeds,
//!                                              many runs at once (`app::lab`); --seeds N, --pass M,
//!                                              --jobs N, --full (no early stop), --csv <file>
//! wmc rates [pack...]                         each kind's numbers in day units: thinks a day,
//!                                              how long its needs last, per rule its dice a
//!                                              think and a day, lifespans (`app::rates`);
//!                                              --kind K (repeatable)
//! wmc sprite render <file.sprite> --png <out.png>
//!                                              every frame of every pose on one sheet, at the
//!                                              cell size and enlarged 8x (`app::sprite::sheet`);
//!                                              --sprite NAME, --pose V (the pose drawn for that
//!                                              look), --cell PX (24), --bg "#rrggbb" (the soil);
//!                                              --rules gives the kinds' colours and sizes
//! wmc sprite score <file.sprite> --target <img>  how close a sprite's first frame is to an
//!                                              image: shape, colour, detail, stamps
//!                                              (`app::sprite::score`); --sprite NAME,
//!                                              --diff out.png, --key "#rrggbb" (the background)
//! wmc sprite fit <image> --kind K -o out.sprite  a first draft of a sprite from an image
//!                                              (`app::sprite::fit`); --stamps N (8),
//!                                              --colors N (3), --chars "...", --size W H,
//!                                              --seed S, --key "#rrggbb"
//! wmc observe <time> [w h seed]                make the scenario's world, step it for <time>
//!                                              (ticks, or 90min, 2h, 4d) and report how its
//!                                              kinds live and die (`app::observe`); takes
//!                                              --every <time>, --csv <file>, --sample N,
//!                                              --kind K (repeatable), --gates (per rule:
//!                                              reached, fired, the term that stopped it)
//! ```
//! A time is ticks, or one in units as a scenario's `run` line writes it
//! (90min, 2h, 4d).
//! Any command takes `--rules <pack>` (repeatable), `--scenario <file>`,
//! `--set NAME=V` (repeatable: the rules' `const NAME` takes the value V)
//! and `--threads N`, anywhere after its name. A pack is a directory of `*.rules` files or one
//! file; packs compile as one rule set, in the order given. Without
//! `--rules`, `WMC_RULES=<pack>:<pack>...` names them, else a saved world
//! uses the packs it was last played with, else the built-in rules.
//! `--scenario` makes a new world from that scenario (seed, size, terrain,
//! where kinds start) instead of the built-in `scenarios/default.scenario`;
//! `[width height seed]` override its size and seed. `lint` checks the
//! scenario against the rules.
//! `play`, `run` and `why` open the world in `save_dir` if one exists
//! (scenario and size/seed args are then ignored), otherwise make it (`play`
//! creates the directory; `run` and `why` create nothing on disk). A save opens
//! under rules that number its kinds differently (matched by name). `run`
//! never saves: run it twice, or with `WMC_THREADS=1` and again without,
//! and the checksums must match. `show` and `run` are headless: a bare
//! `bevy_ecs` world, no `App`.

// A narrowing `as` either is a checked conversion or says why the value
// fits (or that the truncation is the point). Tests may narrow freely.
#![warn(clippy::cast_possible_truncation)]
#![cfg_attr(test, allow(clippy::cast_possible_truncation))]
use std::io::Write;
use std::time::Instant;

use anyhow::{Context, bail};
use sim_core::actors::{Tally, life};
use sim_core::scenario::{Check, MAX_SIZE_CHUNKS, Placement, Start};
use sim_core::time::Clock;
use sim_core::{ChunkActors, ChunkCells, Feature, Ground, Kinds, LoadPolicy, Pos, Stage, Store};
use sim_core::{Scenario, par, sim, stage};

use bevy::prelude::World;

use app::play;
use app::render::ascii::render;
use app::render::cells::Viewport;

/// What the command line says about a world: its rule packs, and the
/// scenario a new one is made from.
struct Setup {
    /// `--rules` paths, in order (none: `WMC_RULES`, a save's packs, or the
    /// built-in rules).
    packs: Vec<String>,
    scenario: Scenario,
    /// The scenario's file, for errors.
    name: String,
}

/// A command's save directory argument: there, and not an empty path (which
/// would be the working directory).
fn save_dir<'a>(arg: Option<&'a String>, command: &str) -> anyhow::Result<&'a str> {
    let dir = arg.with_context(|| format!("{command} needs a save directory"))?;
    if dir.is_empty() {
        bail!("{command} needs a save directory, not an empty path");
    }
    Ok(dir)
}

/// Take `--flag` out of `args`: was it there?
fn take_switch(args: &mut Vec<String>, flag: &str) -> bool {
    let before = args.len();
    args.retain(|a| a != flag);
    args.len() != before
}

/// Take every `--flag value` pair out of `args`. A value is never another
/// flag (`--csv --gates` lacks one; it does not write a file `--gates`).
fn take_flag(args: &mut Vec<String>, flag: &str) -> anyhow::Result<Vec<String>> {
    let mut values = Vec::new();
    while let Some(i) = args.iter().position(|a| a == flag) {
        let v = args
            .get(i + 1)
            .filter(|v| !v.starts_with("--"))
            .with_context(|| format!("{flag} needs a value"))?
            .clone();
        args.drain(i..i + 2);
        values.push(v);
    }
    Ok(values)
}

fn main() -> anyhow::Result<()> {
    // `env::args` panics on one that is not UTF-8.
    let mut args: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| {
            a.into_string()
                .map_err(|a| anyhow::anyhow!("argument {a:?} is not UTF-8"))
        })
        .collect::<anyhow::Result<_>>()?;
    // `wmc` alone opens the main menu; flags alone still need a command.
    let alone = args.is_empty();
    let packs = take_flag(&mut args, "--rules")?;
    let file = take_flag(&mut args, "--scenario")?.pop();
    let strict = take_switch(&mut args, "--strict");
    let every = take_flag(&mut args, "--every")?.pop();
    let csv = take_flag(&mut args, "--csv")?.pop();
    let sample = take_flag(&mut args, "--sample")?.pop();
    let only = take_flag(&mut args, "--kind")?;
    let count = |flag: &str, v: Option<String>| -> anyhow::Result<Option<u32>> {
        v.map(|v| {
            v.parse::<u32>()
                .ok()
                .filter(|n| (1..=sim_core::scenario::MAX_SEEDS).contains(n))
                .with_context(|| {
                    format!(
                        "{flag} needs a count from 1 to {}",
                        sim_core::scenario::MAX_SEEDS
                    )
                })
        })
        .transpose()
    };
    let seeds = count("--seeds", take_flag(&mut args, "--seeds")?.pop())?;
    let pass = count("--pass", take_flag(&mut args, "--pass")?.pop())?;
    let jobs = take_flag(&mut args, "--jobs")?
        .pop()
        .map(|n| {
            n.parse::<usize>()
                .ok()
                .filter(|n| (1..=1024).contains(n))
                .context("--jobs needs a count from 1 to 1024")
        })
        .transpose()?;
    let full = take_switch(&mut args, "--full");
    let gates = take_switch(&mut args, "--gates");
    let mut sets: Vec<(String, Vec<(String, i32)>)> = Vec::new();
    for set in take_flag(&mut args, "--set")? {
        let (name, values) = parse_set(&set)?;
        if sets.iter().any(|(n, _)| *n == name) {
            bail!("--set {name}: given twice");
        }
        sets.push((name, values));
    }
    let threads = take_flag(&mut args, "--threads")?.pop();
    let render = app::sprite::cli::Opts {
        png: take_flag(&mut args, "--png")?.pop(),
        sprite: take_flag(&mut args, "--sprite")?.pop(),
        pose: take_flag(&mut args, "--pose")?.pop(),
        cell: take_flag(&mut args, "--cell")?.pop(),
        bg: take_flag(&mut args, "--bg")?.pop(),
        target: take_flag(&mut args, "--target")?.pop(),
        diff: take_flag(&mut args, "--diff")?.pop(),
        key: take_flag(&mut args, "--key")?.pop(),
        kind: None,
        out: take_flag(&mut args, "-o")?.pop(),
        stamps: take_flag(&mut args, "--stamps")?.pop(),
        colors: take_flag(&mut args, "--colors")?.pop(),
        chars: take_flag(&mut args, "--chars")?.pop(),
        size: match args.iter().position(|a| a == "--size") {
            Some(i) => {
                let w = args.get(i + 1).filter(|v| !v.starts_with("--")).cloned();
                let h = args.get(i + 2).filter(|v| !v.starts_with("--")).cloned();
                let (Some(w), Some(h)) = (w, h) else {
                    bail!("--size needs two values: W H");
                };
                args.drain(i..i + 3);
                Some((w, h))
            }
            None => None,
        },
        seed: take_flag(&mut args, "--seed")?.pop(),
    };
    // `-v` and negative coordinates start with one `-`, a flag with two.
    if let Some(f) = args.iter().find(|a| a.starts_with("--")) {
        bail!("unknown flag {f}");
    }
    if strict && args.first().map(String::as_str) != Some("lint") {
        bail!("--strict is only for lint");
    }
    let command = args.first().map(String::as_str);
    if command != Some("sprite") && render.any() {
        bail!(
            "--png, --sprite, --pose, --cell, --bg, --target, --diff, --key, -o, --stamps, --colors, --chars, --size and --seed are only for sprite"
        );
    }
    if let Some(n) = threads {
        let n: usize = n
            .parse()
            .ok()
            .filter(|n| (1..=par::MAX_THREADS).contains(n))
            .with_context(|| format!("--threads needs a count from 1 to {}", par::MAX_THREADS))?;
        par::init_task_pool_with(Some(n));
    } else if !matches!(command, Some("lint" | "rates" | "sprite")) {
        // Checked here, where threads run (`lint`, `rates` and `sprite` make no
        // world): the pool reads it when a world is made, and panics.
        par::env_threads().map_err(anyhow::Error::msg)?;
    }
    if command != Some("observe") && (every.is_some() || sample.is_some()) {
        bail!("--every and --sample are only for observe");
    }
    if !matches!(command, Some("observe" | "rates" | "sprite")) && !only.is_empty() {
        bail!("--kind is only for observe, rates and sprite fit");
    }
    if !matches!(command, Some("observe" | "lab")) && csv.is_some() {
        bail!("--csv is only for observe and lab");
    }
    if !matches!(command, Some("scenario" | "lab")) && (seeds.is_some() || pass.is_some()) {
        bail!("--seeds and --pass are only for scenario and lab");
    }
    if command != Some("lab") && (jobs.is_some() || full) {
        bail!("--jobs and --full are only for lab");
    }
    if command != Some("lab") {
        // One value each: every rule set this session compiles takes them.
        if let Some((name, _)) = sets.iter().find(|(_, v)| v.len() > 1) {
            bail!("--set {name}=a,b,...: a list of values is for lab; one value here");
        }
        app::set_consts(
            sets.iter()
                .map(|(name, v)| (name.clone(), v[0].1))
                .collect(),
        );
    }
    let file = file.as_deref();
    let setup = |rest: &[String]| config(packs.clone(), file, rest);
    match args.first().map(String::as_str) {
        Some("show") => show(&setup(&args[1..])?),
        Some("menu") => play::menu(),
        Some("play") => {
            let dir = save_dir(args.get(1), "play")?;
            let s = setup(&args[2..])?;
            play::run(dir, &s.scenario, &s.name, &s.packs)
        }
        Some("run") => {
            let dir = save_dir(args.get(1), "run")?;
            let ticks = time_arg(
                args.get(2)
                    .context("run needs a time: ticks, or 90min, 2h, 4d")?,
                true,
            )?;
            run(dir, ticks, &setup(&args[3..])?)
        }
        Some("why") => {
            let verbose = args.get(1).is_some_and(|a| a == "-v");
            let a = &args[1 + usize::from(verbose)..];
            let dir = save_dir(a.first(), "why")?;
            let coord = |i: usize, what: &str| -> anyhow::Result<i32> {
                a.get(i)
                    .with_context(|| format!("why needs {what}"))?
                    .parse()
                    .with_context(|| format!("bad {what}"))
            };
            let (x, y) = (coord(1, "x")?, coord(2, "y")?);
            let ticks = a.get(3).map_or(Ok(0), |t| time_arg(t, true))?;
            why(
                dir,
                Pos::new(x, y),
                ticks,
                verbose,
                &setup(a.get(4..).unwrap_or(&[]))?,
            )
        }
        Some("observe") => {
            let ticks = time_arg(
                args.get(1)
                    .context("observe needs a time: ticks, or 90min, 2h, 4d")?,
                false,
            )?;
            let mut opts = app::observe::Options::default();
            if let Some(e) = &every {
                opts.every = time_arg(e, false)?;
            }
            if let Some(n) = &sample {
                opts.sample = n
                    .parse()
                    .ok()
                    .filter(|n| *n <= 100_000)
                    .context("--sample needs a count from 0 to 100000")?;
            }
            observe(
                ticks,
                &setup(&args[2..])?,
                opts,
                csv.as_deref(),
                &only,
                gates,
            )
        }
        Some("rates") => {
            // The packs as lint finds them: these, else `WMC_RULES`, else the
            // scenario's; none is the built-in rules.
            let cli: Vec<String> = args[1..].iter().chain(&packs).cloned().collect();
            let from_scenario = match file {
                Some(f) => scenario(Some(f))?.packs,
                None => Vec::new(),
            };
            let kinds = app::compile(&app::packs_or(&cli, &from_scenario))?;
            if let Some(k) = only.iter().find(|k| kinds.by_name(k).is_none()) {
                bail!("--kind {k}: the rules have no such kind");
            }
            print!("{}", app::rates::report(&kinds, &only));
            Ok(())
        }
        Some("lab") => {
            let f = args.get(1).context("lab needs a scenario file")?;
            if let Some(a) = args.get(2) {
                bail!("lab takes one file; unexpected {a:?}");
            }
            let runs = LabRuns {
                seeds,
                pass,
                jobs,
                full,
            };
            lab(f, &packs, &sets, runs, csv.as_deref())
        }
        Some("scenario") => {
            let f = args.get(1).context("scenario needs a scenario file")?;
            if let Some(a) = args.get(2) {
                bail!("scenario takes one file; unexpected {a:?}");
            }
            scenario_test(f, &packs, seeds, pass)
        }
        Some("sprite") => {
            if only.len() > 1 {
                bail!("--kind: sprite fit takes one");
            }
            let render = app::sprite::cli::Opts {
                kind: only.first().cloned(),
                ..render
            };
            app::sprite::cli::run(&args[1..], &packs, &render)
        }
        Some("lint") => {
            // As for a new world: these, else `WMC_RULES`, else the scenario's.
            let cli: Vec<String> = args[1..].iter().chain(&packs).cloned().collect();
            let from_scenario = match file {
                Some(f) => scenario(Some(f))?.packs,
                None => Vec::new(),
            };
            let packs = app::packs_or(&cli, &from_scenario);
            if packs.is_empty() {
                bail!(
                    "lint needs a rules directory or file (or WMC_RULES), or a scenario that names its rules"
                );
            }
            lint(&packs, file, strict)
        }
        Some(other) => {
            bail!(
                "unknown command {other:?}; use `menu`, `show`, `play`, `run`, `why`, `observe`, `scenario`, `lab`, `rates`, `lint` or `sprite`"
            )
        }
        None if alone => play::menu(),
        None => bail!(
            "usage: wmc (the menu) | wmc show [w h seed] | wmc play <dir> [w h seed] | wmc run <dir> <time> [w h seed] | wmc why [-v] <dir> <x> <y> [time [w h seed]] | wmc observe <time> [w h seed] [--gates] | wmc lint [--strict] <pack>... | wmc scenario <file> [--seeds N] [--pass M] | wmc lab <file> [--set NAME=a,b,...]... [--seeds N] [--pass M] [--jobs N] [--full] [--csv f] | wmc rates [pack...] [--kind K] | wmc sprite render <file.sprite> --png <out.png> [--sprite NAME] [--pose V] [--cell PX] [--bg \"#rrggbb\"] | wmc sprite score <file.sprite> --target <image> [--sprite NAME] [--diff out.png] [--key \"#rrggbb\"] | wmc sprite fit <image> --kind K -o out.sprite [--stamps N] [--colors N] [--chars ...] [--size W H] [--seed S]; any takes --rules <pack> (repeatable), --scenario <file>, --set NAME=V, --threads N"
        ),
    }
}

/// A time on the command line (`run`, `why`, `observe`, `--every`), as a
/// scenario's `run` line writes one: ticks, or 90min, 2h, 4d. Zero only
/// where `zero` allows it.
fn time_arg(t: &str, zero: bool) -> anyhow::Result<u64> {
    t.parse::<u64>()
        .ok()
        .or_else(|| sim_core::scenario::ticks_of(t))
        .filter(|&n| zero || n > 0)
        .with_context(|| {
            format!(
                "bad time {t:?}: {}ticks, or a time (90min, 2h, 4d)",
                if zero { "" } else { "more than 0 " }
            )
        })
}

/// A scenario file, else the built-in one. The packs a file names are
/// relative to it.
fn scenario(file: Option<&str>) -> anyhow::Result<Scenario> {
    match file {
        Some(f) => app::read_scenario(f),
        None => Ok(Scenario::builtin()),
    }
}

/// The setup for a world: its packs, and its scenario with `[width height
/// seed]` over the scenario's own.
fn config(packs: Vec<String>, file: Option<&str>, args: &[String]) -> anyhow::Result<Setup> {
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
    if let Some(a) = args.get(3) {
        bail!("unexpected argument {a:?} after [width height seed]");
    }
    // What a `size` line is held to (the scenario's own already was).
    if s.width == 0 || s.height == 0 {
        bail!("width and height are at least 1");
    }
    let chunks = u64::from(s.width.div_ceil(64)) * u64::from(s.height.div_ceil(64));
    if chunks > MAX_SIZE_CHUNKS {
        bail!(
            "{}x{} is {chunks} chunks; a new world generates at most {MAX_SIZE_CHUNKS} up front",
            s.width,
            s.height
        );
    }
    if let Some(m) = &s.map
        && (s.width < m.width || s.height < m.height)
    {
        bail!(
            "{}x{} is smaller than the map of {}, which is {}x{}",
            s.width,
            s.height,
            file.unwrap_or("the scenario"),
            m.width,
            m.height
        );
    }
    Ok(Setup {
        packs,
        scenario: s,
        name: file.unwrap_or(app::DEFAULT_SCENARIO).to_string(),
    })
}

/// A new world of the setup's scenario under `kinds`; says which file does
/// not fit.
fn new_world(setup: &Setup, kinds: Kinds) -> anyhow::Result<World> {
    sim::new_world_with(&setup.scenario, kinds)
        .map_err(|e| app::new_world_error(&setup.name, setup.scenario.line_of(&e), e))
}

fn show(setup: &Setup) -> anyhow::Result<()> {
    let kinds = app::compile(&app::packs_or(&setup.packs, &setup.scenario.packs))?;
    app::warn(&kinds);
    let t0 = Instant::now();
    let mut world = new_world(setup, kinds)?;
    let gen_time = t0.elapsed();
    let cfg = &setup.scenario;

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
/// one of the setup (its initial region loaded). Never saved by the caller,
/// so it creates nothing on disk: no world there is a world in memory.
fn open_or_new(dir: &str, setup: &Setup) -> anyhow::Result<World> {
    let store = Store::at(dir);
    let kinds = app::rules_for(&store, &setup.packs, &setup.scenario.packs)?;
    Ok(
        match sim::open_world_with(&store, kinds.clone()).context("reading save")? {
            Some(mut w) => {
                app::say_skipped_starts(&w);
                let camera = play::camera_for(&w, &store);
                sim::ensure_loaded(&mut w, camera.cell(), LoadPolicy::default(), Some(&store))
                    .context("streaming chunks")?;
                w
            }
            None => new_world(setup, kinds)?,
        },
    )
}

/// The longest `wmc why` steps a world waiting for a think: about three
/// days. A kind that thinks more seldom (`cadence 2^30`) would hold it for
/// hours; past this it says when the think comes instead.
const WHY_WAIT: u64 = 1 << 16;

/// Ticks from `now` to `row`'s next think by its cadence (a wake-up may
/// come sooner), if the sim's own test (`systems::due`) agrees there.
fn next_think(now: u64, row: &sim_core::ActorPub, cadence: u64) -> Option<u64> {
    let shift = cadence.trailing_zeros().saturating_sub(16);
    let phase = now.wrapping_add(u64::from(row.stagger) << shift) & (cadence - 1);
    let k = cadence.wrapping_sub(phase) & (cadence - 1);
    sim_core::actors::systems::due(now + k, row, cadence).then_some(k)
}

/// `wmc why`: step `ticks`, then wait (up to its cadence, at least a day,
/// at most [`WHY_WAIT`]) for the actor at `p` to be due, explain that
/// think, and run it to show where it went.
fn why(dir: &str, p: Pos, ticks: u64, ops: bool, setup: &Setup) -> anyhow::Result<()> {
    let mut world = open_or_new(dir, setup)?;
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
    let mut waited = 0u64;
    let (at, e) = loop {
        let (at, cc, slot) =
            sim::find_uid(&mut world, uid).context("it died or was eaten before its next think")?;
        let e = sim::explain_slot(&world, cc, slot).expect("a loaded row");
        if e.due {
            break (at, e);
        }
        // Its own cadence (a `become` may change it), at least a day, at
        // most WHY_WAIT: a think further off is said, not waited for.
        let cadence = world.resource::<Kinds>().def(e.row.kind).cadence();
        let limit = cadence.clamp(sim_core::TICKS_PER_DAY, WHY_WAIT);
        if let Some(k) = next_think(sim::tick(&world), &e.row, cadence)
            && waited + k > WHY_WAIT
        {
            bail!(
                "it thinks every {cadence} ticks, its next think {k} ticks on, and `why` waits at most {WHY_WAIT} for one: give `why` {} ticks to step first",
                ticks + waited + k
            );
        }
        if waited >= limit {
            bail!("it did not think within {limit} ticks");
        }
        sim::step(&mut world);
        waited += 1;
    };
    let mut out = std::io::stdout().lock();
    if waited > 0 {
        writeln!(out, "(stepped {waited} ticks to its next think)")?;
    }
    write!(out, "{}", app::why::report_of(&world, at, &e, ops))?;
    sim::step(&mut world);
    writeln!(out, "after   {}", app::why::after(&mut world, uid))?;
    Ok(())
}

/// Headless: the determinism check and the tick benchmark. A saved world
/// loads the chunks around its camera; a new one keeps its initial region.
fn run(dir: &str, ticks: u64, setup: &Setup) -> anyhow::Result<()> {
    let mut world = open_or_new(dir, setup)?;
    let from = sim::tick(&world);
    let t0 = Instant::now();
    for _ in 0..ticks {
        sim::step(&mut world);
    }
    let wall = t0.elapsed();
    let to = sim::tick(&world);
    let loaded = world.resource::<Stage>().loaded_count();
    let kinds = world.resource::<Kinds>().clone();
    // Heads, as born and died count them: a row each, a group's its `heads`.
    let mut per_kind = vec![0u64; kinds.len()];
    let mut rows = 0usize;
    for a in world.query::<&ChunkActors>().iter(&world) {
        rows += a.rows.len();
        for r in &a.rows {
            per_kind[usize::from(r.kind)] += u64::from(r.heads);
        }
    }
    let actors: u64 = per_kind.iter().sum();
    let in_rows = if rows as u64 == actors {
        String::new()
    } else {
        format!(" in {rows} rows")
    };
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
        "chunks:    {loaded} ({actors} actors{in_rows}: {})",
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
        let k = u16::try_from(i).expect("kind ids are u16");
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

/// `wmc observe`: make the scenario's world (nowhere on disk; its `run` and
/// `expect` lines are not run), step it for `ticks`, and print what
/// [`app::observe`] measured; the samples go to `csv` too, if given.
/// `only`: the kinds to detail (none: all).
fn observe(
    ticks: u64,
    s: &Setup,
    opts: app::observe::Options,
    csv: Option<&str>,
    only: &[String],
    gates: bool,
) -> anyhow::Result<()> {
    let kinds = app::compile(&app::packs_or(&s.packs, &s.scenario.packs))?;
    app::warn(&kinds);
    if let Some(k) = only
        .iter()
        .find(|k| !kinds.defs.iter().any(|d| d.name == **k))
    {
        bail!("--kind {k}: the rules have no such kind");
    }
    let mut world = sim::new_world_with(&s.scenario, kinds)
        .map_err(|e| app::new_world_error(&s.name, s.scenario.line_of(&e), e))?;
    // Made before the run: a path it cannot write fails now, not after it.
    let csv = csv
        .map(|path| {
            std::fs::File::create(path)
                .map(|f| (path, std::io::BufWriter::new(f)))
                .with_context(|| format!("writing {path}"))
        })
        .transpose()?;
    let from = sim::tick(&world);
    let started = Instant::now();
    let report = app::observe::observe(&mut world, ticks, opts);
    let wall = started.elapsed();
    let kinds = world.resource::<Kinds>();
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "observe   {}, seed {}, {}x{}: {} to {}, a sample every {} ticks, up to {} thinks explained per kind",
        s.name,
        s.scenario.seed,
        s.scenario.width,
        s.scenario.height,
        Clock::at(from),
        Clock::at(sim::tick(&world)),
        opts.every,
        opts.sample
    )?;
    if let Some((path, mut file)) = csv {
        app::observe::write_csv(&report, kinds, &mut file)
            .and_then(|()| file.flush())
            .with_context(|| format!("writing {path}"))?;
        writeln!(out, "csv       {path} ({} samples)", report.samples.len())?;
    }
    writeln!(out)?;
    write!(out, "{}", app::observe::summary(&report, kinds, only))?;
    if gates {
        writeln!(out)?;
        write!(out, "{}", app::observe::rule_report(&report, kinds, only))?;
    }
    writeln!(
        out,
        "\nwall      {wall:.2?} ({} threads)",
        par::thread_count()
    )?;
    Ok(())
}

/// `--set NAME=V` or (for lab) `NAME=a,b,c`: the name, and each value as
/// written and as a number (an integer, or a time in ticks).
fn parse_set(set: &str) -> anyhow::Result<(String, Vec<(String, i32)>)> {
    let (name, values) = set.split_once('=').with_context(|| {
        format!("--set {set}: NAME=VALUE, the value a number or a time (12, -3, 90min, 2h, 1d)")
    })?;
    if name.is_empty()
        || name.starts_with(|c: char| c.is_ascii_digit())
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        bail!("--set {set}: `{name}` is not a const's name");
    }
    let values = values
        .split(',')
        .map(|v| {
            sim_core::scenario::value_of(v)
                .map(|n| (v.to_string(), n))
                .with_context(|| {
                    format!("--set {set}: `{v}` is not a number or a time (12, -3, 90min, 2h, 1d)")
                })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok((name.to_string(), values))
}

/// How `wmc lab` runs: on how many seeds and how many must pass (else the
/// scenario's), how many at once (else one per core), and whether to run
/// every seed to its end.
struct LabRuns {
    seeds: Option<u32>,
    pass: Option<u32>,
    jobs: Option<usize>,
    full: bool,
}

/// Many worlds at once (`lab`, a multi-seed `scenario`): the runs are the
/// parallelism, so a world gets one thread unless `--threads` or
/// `WMC_THREADS` says how many.
fn one_thread_a_world() -> anyhow::Result<()> {
    let n = par::env_threads().map_err(anyhow::Error::msg)?.unwrap_or(1);
    // The first initialiser wins: `--threads` set it already.
    par::init_task_pool_with(Some(n));
    Ok(())
}

/// At most this many variants (combinations of `--set` values) in a lab.
const MAX_VARIANTS: usize = 1000;

/// `wmc lab`: the scenario test in `file` for every combination of the
/// `sets` values (one rule set each), on each of its seeds, many runs at
/// once (`app::lab`): which variants pass, and each expectation's seeds and
/// numbers. Each world runs on one thread unless `--threads` or
/// `WMC_THREADS` says more; a
/// seed stops at its first sure failure unless `full`. Writes every verdict
/// to `csv` too, if given.
fn lab(
    file: &str,
    packs: &[String],
    sets: &[(String, Vec<(String, i32)>)],
    runs: LabRuns,
    csv: Option<&str>,
) -> anyhow::Result<()> {
    let s = scenario(Some(file))?;
    if !s.checks.iter().any(|c| matches!(c, Check::Expect { .. })) {
        bail!("{file}: no `expect` lines: a lab judges a scenario test (RULES §14)");
    }
    let seeds = runs.seeds.unwrap_or(s.seeds);
    let pass = runs.pass.or(s.pass).unwrap_or(seeds);
    if pass > seeds {
        bail!("{file}: it needs {pass} seeds to pass, but runs on {seeds}");
    }
    let combos = app::lab::variants(sets);
    if combos.len() > MAX_VARIANTS {
        bail!(
            "{} combinations of --set values: at most {MAX_VARIANTS}",
            combos.len()
        );
    }
    let packs = app::packs_or(packs, &s.packs);
    let variants = combos
        .into_iter()
        .map(|(label, consts)| {
            let kinds = app::compile_with(&packs, &consts)?;
            Ok(app::lab::Variant { label, kinds })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let kinds = &variants[0].kinds;
    app::warn(kinds);
    // What every run would refuse: the starts, and what the expectations name.
    Placement::resolve(&s.starts, kinds, &s.terrain()).map_err(|e| match s.line_of(&e) {
        Some(line) => anyhow::anyhow!("{file}:{line}: {e}"),
        None => anyhow::anyhow!("{file}: {e}"),
    })?;
    for c in &s.checks {
        if let Check::Expect { line, what, .. } = c {
            sim::check_expect(kinds, what).map_err(|e| anyhow::anyhow!("{file}:{line}: {e}"))?;
        }
    }
    one_thread_a_world()?;
    // Made before the runs: a path it cannot write fails now, not after them.
    let csv = csv
        .map(|path| {
            std::fs::File::create(path)
                .map(|f| (path, std::io::BufWriter::new(f)))
                .with_context(|| format!("writing {path}"))
        })
        .transpose()?;
    let jobs = runs.jobs.unwrap_or_else(|| {
        std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get)
    });
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "lab       {file}: {} variants x {seeds} seeds from {}, {pass} to pass; {jobs} runs at a time, {} thread a world; {}",
        variants.len(),
        s.seed,
        par::thread_count(),
        if runs.full {
            "every run to its end, every seed"
        } else {
            "a run stops at its first sure failure, a variant once it cannot pass"
        }
    )?;
    if cfg!(debug_assertions) {
        writeln!(
            out,
            "          a dev build: `cargo build --profile fast`, then target/fast/wmc, runs labs about twice as fast"
        )?;
    }
    let started = Instant::now();
    let total = variants.len() * seeds as usize;
    let step = (total / 10).max(1);
    let results = app::lab::run(
        &s,
        &variants,
        seeds,
        jobs,
        runs.full,
        pass,
        |done, total| {
            if done % step == 0 || done == total {
                eprintln!("lab: {done} of {total} runs");
            }
        },
    );
    writeln!(out)?;
    write!(
        out,
        "{}",
        app::lab::report(file, &variants, &results, seeds, pass)
    )?;
    if let Some((path, mut f)) = csv {
        app::lab::write_csv(&variants, &results, &mut f)
            .and_then(|()| f.flush())
            .with_context(|| format!("writing {path}"))?;
        writeln!(out, "csv       {path}")?;
    }
    writeln!(out, "wall      {:.2?}", started.elapsed())?;
    Ok(())
}

/// `wmc scenario`: make the scenario's world (no save), then its `run` and
/// `expect` lines in order (`sim::run_checks`: windows watch the runs).
/// Prints every expectation with what it found, and the checksums at the
/// end (equal at any thread count); fails if any expectation does. With
/// several seeds (`seeds N` in the file, or `seeds`), it runs once per
/// seed, `seed`, `seed + 1`, ..., prints each seed's verdict, then how many
/// seeds each expectation held on; it fails when fewer than `pass` seeds
/// (all, if not given) held every expectation.
fn scenario_test(
    file: &str,
    packs: &[String],
    seeds: Option<u32>,
    pass: Option<u32>,
) -> anyhow::Result<()> {
    let s = scenario(Some(file))?;
    let kinds = app::compile(&app::packs_or(packs, &s.packs))?;
    app::warn(&kinds);
    let seeds = seeds.unwrap_or(s.seeds);
    let pass = pass.or(s.pass).unwrap_or(seeds);
    if pass > seeds {
        bail!("{file}: it needs {pass} seeds to pass, but runs on {seeds}");
    }
    let new_world = |seed: u64| {
        let one = Scenario { seed, ..s.clone() };
        sim::new_world_with(&one, kinds.clone()).map_err(|e| match s.line_of(&e) {
            Some(line) => anyhow::anyhow!("{file}:{line}: {e}"),
            None => anyhow::anyhow!("{file}: {e}"),
        })
    };
    let mut out = std::io::stdout().lock();
    let verdict = |result: &Result<(bool, String), String>| match result {
        Ok((true, got)) => (true, got.clone()),
        Ok((false, got)) => (false, got.clone()),
        Err(e) => (false, e.clone()),
    };
    if seeds == 1 {
        let mut world = new_world(s.seed)?;
        let start = sim::tick(&world);
        let (mut ticks, mut checked, mut failed) = (0u64, 0, 0);
        let mut written = Ok(());
        sim::run_checks(&mut world, &s.checks, |e| {
            let line = match e {
                sim::Event::Ran(t) => {
                    ticks += t;
                    format!("      run {t} ticks, to {}", Clock::at(start + ticks))
                }
                sim::Event::Expected {
                    line, text, result, ..
                } => {
                    checked += 1;
                    let (ok, got) = verdict(&result);
                    failed += usize::from(!ok);
                    let mark = if ok { "ok  " } else { "FAIL" };
                    format!("{mark}  {file}:{line}: {text}  ({got})")
                }
            };
            if written.is_ok() {
                written = writeln!(out, "{line}");
            }
        });
        written?;
        writeln!(out, "checksum:  {:016x}", sim::checksum(&mut world))?;
        writeln!(out, "state:     {:016x}", stage::checksum(&mut world))?;
        writeln!(
            out,
            "{checked} expectations, {failed} failed ({} threads)",
            par::thread_count()
        )?;
        if failed > 0 {
            bail!("{file}: {failed} of {checked} expectations failed");
        }
        return Ok(());
    }

    // Several seeds: run at once, a world each (`app::lab`, every run to
    // its end), then a line per seed in seed order, and a line per
    // expectation. The same report as one after another, only sooner.
    one_thread_a_world()?;
    let jobs = std::thread::available_parallelism().map_or(4, std::num::NonZeroUsize::get);
    let variant = app::lab::Variant {
        label: String::new(),
        kinds: kinds.clone(),
    };
    let runs = app::lab::run(&s, &[variant], seeds, jobs, true, pass, |_, _| {})
        .pop()
        .unwrap_or_default();
    if let Some(r) = runs.iter().find(|r| r.error.is_some()) {
        let e = r.error.as_deref().unwrap_or_default();
        bail!(match r.error_line {
            Some(line) => format!("{file}:{line}: seed {}: {e}", r.seed),
            None => format!("{file}: seed {}: {e}", r.seed),
        });
    }
    let (mut passed, mut sums, mut states) = (0u32, 0u64, 0u64);
    let mut held: Vec<(u32, String, u32)> = Vec::new();
    for r in &runs {
        let mut failures = Vec::new();
        for (i, v) in r.verdicts.iter().enumerate() {
            if held.len() <= i {
                held.push((v.line, v.text.clone(), 0));
            }
            if v.held {
                held[i].2 += 1;
            } else {
                failures.push(format!("FAIL  {file}:{}: {}  ({})", v.line, v.text, v.saw));
            }
        }
        sums = sim_core::rng::splitmix64(sums ^ r.checksum);
        states = sim_core::rng::splitmix64(states ^ r.state);
        passed += u32::from(failures.is_empty());
        let mark = if failures.is_empty() { "ok  " } else { "FAIL" };
        writeln!(
            out,
            "seed {:<10} {mark}  {} of {} held   checksum {:016x}",
            r.seed,
            r.verdicts.len() - failures.len(),
            r.verdicts.len(),
            r.checksum
        )?;
        for f in failures {
            writeln!(out, "                 {f}")?;
        }
    }
    writeln!(out, "per expectation, over {seeds} seeds:")?;
    for (line, text, n) in &held {
        writeln!(out, "  {n:>4}/{seeds}  {file}:{line}: {text}")?;
    }
    writeln!(out, "checksum:  {sums:016x}")?;
    writeln!(out, "state:     {states:016x}")?;
    writeln!(
        out,
        "{passed} of {seeds} seeds held every expectation, {pass} needed ({} threads)",
        par::thread_count()
    )?;
    if passed < pass {
        bail!("{file}: {passed} of {seeds} seeds passed, {pass} needed");
    }
    Ok(())
}

/// Compile rule packs and print their kind table and the author lint: the
/// fast way to check a rules file before a world runs it. With a scenario,
/// check that its starts and its `expect` lines fit the rules too, which
/// kinds never appear, and which starts lie outside the initial region.
/// `strict`: any warning fails (for CI).
fn lint(
    packs: &[std::path::PathBuf],
    scenario_file: Option<&str>,
    strict: bool,
) -> anyhow::Result<()> {
    let kinds = app::compile(packs)?;
    let scenario = match scenario_file {
        Some(f) => {
            let s = scenario(Some(f))?;
            Placement::resolve(&s.starts, &kinds, &s.terrain()).map_err(|e| {
                match s.line_of(&e) {
                    Some(line) => anyhow::anyhow!("{f}:{line}: {e}"),
                    None => anyhow::anyhow!("{f}: {e}"),
                }
            })?;
            for c in &s.checks {
                if let Check::Expect { line, what, .. } = c {
                    sim::check_expect(&kinds, what)
                        .map_err(|e| anyhow::anyhow!("{f}:{line}: {e}"))?;
                }
            }
            Some((f, s))
        }
        None => None,
    };
    let mut diagnostics = kinds.debug.diagnostics.clone();
    // The packs' sprite files: a parse error fails, the rest are warnings.
    let (wardrobe, sprite_warnings) = app::sprite::read(&kinds)?;
    diagnostics.extend(sprite_warnings);
    if wardrobe.tiles() > app::sprite::TILE_FLOOR {
        diagnostics.push(sim_core::rules::Diagnostic {
            level: sim_core::rules::Level::Warning,
            file: "sprites".into(),
            line: 0,
            col: 0,
            msg: format!(
                "{} sprite tiles: above the {} every device holds (a frame of a pose of a cell each)",
                wardrobe.tiles(),
                app::sprite::TILE_FLOOR
            ),
        });
    }
    if let Some((f, s)) = &scenario {
        diagnostics.extend(s.unseen(&kinds));
        diagnostics.extend(s.outside_region(f));
    }
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
    for w in &wardrobe.sprites {
        let own = &w.sprite.poses[0];
        let wearers: Vec<&str> = wardrobe
            .of_kind
            .iter()
            .enumerate()
            .filter(|&(k, s)| {
                k != usize::from(w.kind)
                    && s.is_some_and(|i| wardrobe.sprites[usize::from(i)].kind == w.kind)
            })
            .map(|(k, _)| kinds.defs[k].name.as_str())
            .collect();
        writeln!(
            out,
            "sprite {:<12} {} poses, {} frames, {} stamps, {} tiles{}",
            kinds.def(w.kind).name,
            w.sprite.poses.len(),
            own.frames.n,
            own.stamps.len(),
            w.tiles(),
            if wearers.is_empty() {
                String::new()
            } else {
                format!(", worn by {}", wearers.join(", "))
            }
        )?;
    }
    if !wardrobe.is_empty() {
        writeln!(
            out,
            "{} sprites, {} tiles (every device holds {})",
            wardrobe.sprites.len(),
            wardrobe.tiles(),
            app::sprite::TILE_FLOOR
        )?;
    }
    for d in &diagnostics {
        writeln!(out, "{d}")?;
    }
    let warnings = diagnostics
        .iter()
        .filter(|d| d.level == sim_core::rules::Level::Warning)
        .count();
    writeln!(
        out,
        // The VM's constant pool, not the `const` lines: the compiled
        // rules do not keep those.
        "{} kinds, {} traits, {} ops, const pool {}, {} subs, {warnings} warnings, hash {:016x}",
        kinds.len(),
        kinds.debug.traits.len(),
        kinds.code.len(),
        kinds.consts.len(),
        kinds.subs.len(),
        kinds.hash
    )?;
    if let Some((f, s)) = &scenario {
        let (mut share, mut at, mut regions) = (0.0, 0, 0);
        for st in &s.starts {
            match st {
                Start::Share {
                    region: Some(_), ..
                } => regions += 1,
                Start::Share { num, den, .. } => share += f64::from(*num) / f64::from(*den),
                Start::At { .. } => at += 1,
            }
        }
        let map = match &s.map {
            None => String::new(),
            Some(m) => format!(
                ", a drawn map {}x{} with {} outside",
                m.width,
                m.height,
                match m.outside.and_then(sim_core::scenario::DrawnMap::decode) {
                    None => "noise",
                    Some((_, sim_core::Feature::Rock)) => "rock",
                    Some((sim_core::Ground::Water, _)) => "water",
                    Some(_) => "soil",
                }
            ),
        };
        writeln!(
            out,
            "scenario {f}: seed {} size {}x{}, {} shares ({:.2}% of walkable cells), {regions} in regions, {at} explicit starts{map}",
            s.seed,
            s.width,
            s.height,
            s.starts.len() - at - regions,
            share * 100.0
        )?;
    }
    if strict && warnings > 0 {
        bail!("{warnings} warnings (--strict)");
    }
    Ok(())
}
