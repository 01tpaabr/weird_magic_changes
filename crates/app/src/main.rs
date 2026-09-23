//! Entry point. Generates a stage and prints it as ASCII.
//!
//! `wmc [width] [height] [seed]`   (defaults: 80 24 42)
mod render;

use std::io::Write;
use std::time::Instant;

use anyhow::{Context, bail};
use sim_core::stage::worldgen::GenParams;
use sim_core::{Feature, Ground, World};

use render::ascii::{Viewport, render};

fn main() -> anyhow::Result<()> {
    if let Err(v) = zig_kernels::check_abi() {
        bail!(
            "Zig kernel ABI mismatch: rust={} zig={v}",
            zig_kernels::ABI_VERSION
        );
    }

    let arg = |i: usize, default: u64, what: &str| -> anyhow::Result<u64> {
        std::env::args()
            .nth(i)
            .map_or(Ok(default), |s| s.parse())
            .with_context(|| format!("bad {what}"))
    };
    let width = u32::try_from(arg(1, 80, "width")?).context("width")?;
    let height = u32::try_from(arg(2, 24, "height")?).context("height")?;
    let seed = arg(3, 42, "seed")?;

    let t0 = Instant::now();
    let world = World::generate(width, height, seed, &GenParams::default());
    let gen_time = t0.elapsed();

    let stage = &world.stage;
    let water = stage.ground.iter().filter(|g| **g == Ground::Water).count();
    let rocks = stage
        .feature
        .iter()
        .filter(|f| **f == Feature::Rock)
        .count();

    let mut out = std::io::stdout().lock();
    out.write_all(render(stage, Viewport::full(stage)).as_bytes())?;
    writeln!(out)?;
    writeln!(out, "stage:     {width}x{height} seed={seed}")?;
    writeln!(
        out,
        "water:     {water} ({:.1}%)",
        100.0 * water as f64 / stage.len() as f64
    )?;
    writeln!(out, "rocks:     {rocks}")?;
    writeln!(out, "generate:  {gen_time:.2?}")?;
    writeln!(out, "checksum:  {:016x}", world.checksum())?;
    Ok(())
}
