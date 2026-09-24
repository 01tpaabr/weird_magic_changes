//! Front end of the game: the Bevy `App`, camera, sim clock, rendering,
//! input. `main.rs` is the CLI shell over this. Nothing here is visible to
//! `sim-core`, which knows no engine clock, asset or window.
//!
//! - [`camera`]: where the player looks (fractional cell, eased velocity).
//! - [`clock`]: real time -> number of sim ticks this frame (speed, pause, budget).
//! - [`render`]: `Stage` -> `CellFrame` (glyph, fg, bg per cell) -> tilemap tiles.
//! - [`play`]: the windowed game: `PlayPlugin`, its resources and systems.
//! - [`rules`]: which rules the app runs (the built-in plants, or `WMC_RULES`).
pub mod camera;
pub mod clock;
pub mod play;
pub mod render;

use anyhow::Context;
use sim_core::Kinds;

/// The rule set for this session: every `*.rules` file in the directory
/// named by `WMC_RULES`, else the rules built into the binary.
pub fn rules() -> anyhow::Result<Kinds> {
    match std::env::var_os("WMC_RULES") {
        Some(dir) => sim_core::rules::compile_dir(std::path::Path::new(&dir))
            .map_err(|e| anyhow::anyhow!("{e}"))
            .with_context(|| format!("compiling rules in {}", dir.to_string_lossy())),
        None => Ok(Kinds::builtin()),
    }
}
