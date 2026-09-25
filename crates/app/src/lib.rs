//! Front end of the game: the Bevy `App`, camera, sim clock, rendering,
//! input. `main.rs` is the CLI shell over this. Nothing here is visible to
//! `sim-core`, which knows no engine clock, asset or window.
//!
//! - [`camera`]: where the player looks (fractional cell, eased velocity).
//! - [`clock`]: real time -> number of sim ticks this frame (speed, pause, budget).
//! - [`render`]: `Stage` -> `CellFrame` (glyph, fg, bg per cell) -> tilemap tiles.
//! - [`play`]: the windowed game: `PlayPlugin`, its resources and systems.
//! - [`rules`]: which rules the app runs (the built-in ones, or `WMC_RULES`).
//! - [`why`]: `wmc why`, one actor's think explained.
pub mod camera;
pub mod clock;
pub mod play;
pub mod render;
pub mod why;

use anyhow::Context;
use sim_core::Kinds;

/// Where `r` (hot reload) reads rules from: `WMC_RULES`, else `./rules`
/// when the game runs from the repository. `None`: nothing to reload.
pub fn rules_dir() -> Option<std::path::PathBuf> {
    match std::env::var_os("WMC_RULES") {
        Some(dir) => Some(dir.into()),
        None => {
            let here = std::path::PathBuf::from("rules");
            here.is_dir().then_some(here)
        }
    }
}

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
