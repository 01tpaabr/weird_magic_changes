//! Front end of the game: the Bevy `App`, camera, sim clock, rendering,
//! input. `main.rs` is the CLI shell over this. Nothing here is visible to
//! `sim-core`, which knows no engine clock, asset or window.
//!
//! - [`camera`]: where the player looks (fractional cell, eased velocity).
//! - [`clock`]: real time -> number of sim ticks this frame (speed, pause, budget).
//! - [`render`]: `Stage` -> `CellFrame` (glyph, fg, bg per cell) -> tilemap tiles.
//! - [`play`]: the windowed game: `PlayPlugin`, its resources and systems.
//! - [`packs`], [`compile`], [`rules_for`]: which rules the app runs (packs
//!   from `--rules` or `WMC_RULES`, a save's own packs, or the built-in ones).
//! - [`why`]: `wmc why`, one actor's think explained.
pub mod camera;
pub mod clock;
pub mod play;
pub mod render;
pub mod why;

use std::path::{Path, PathBuf};

use anyhow::Context;
use sim_core::{Kinds, Store};

/// The rule packs a session asked for: the `--rules` paths, else
/// `WMC_RULES` (paths separated by `:`), else none.
pub fn packs(cli: &[String]) -> Vec<PathBuf> {
    if !cli.is_empty() {
        return cli.iter().map(PathBuf::from).collect();
    }
    std::env::var_os("WMC_RULES").map_or_else(Vec::new, |v| std::env::split_paths(&v).collect())
}

/// Compile `packs` as one rule set (`sim_core::rules::compile_packs`);
/// none is the rules built into the binary.
pub fn compile(packs: &[PathBuf]) -> anyhow::Result<Kinds> {
    if packs.is_empty() {
        return Ok(Kinds::builtin());
    }
    let paths: Vec<&Path> = packs.iter().map(PathBuf::as_path).collect();
    // Errors name their file (`file:line:col: ...`, or the path that failed).
    sim_core::rules::compile_packs(&paths).map_err(|e| anyhow::anyhow!("{e}"))
}

/// The rules for the world in `store`: the session's packs, else (opening
/// a save) the packs it was last played with, if they all still exist,
/// else the built-in rules. Says on stderr when it is not the session's
/// choice, and when the rules changed since the save was last played.
pub fn rules_for(store: &Store, cli: &[String]) -> anyhow::Result<Kinds> {
    let meta = store.read_meta().context("reading save")?;
    let mut packs = packs(cli);
    if let Some(m) = meta
        .as_ref()
        .filter(|m| packs.is_empty() && !m.packs.is_empty())
    {
        let saved: Vec<PathBuf> = m.packs.iter().map(PathBuf::from).collect();
        let gone: Vec<&PathBuf> = saved.iter().filter(|p| !p.exists()).collect();
        if gone.is_empty() {
            eprintln!("rules: the save's packs, {}", shown(&saved));
            packs = saved;
        } else {
            eprintln!(
                "rules: the save's packs are missing ({}); using the built-in rules",
                gone.iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    let kinds = compile(&packs)?;
    warn(&kinds);
    if let Some(m) = meta.filter(|m| m.rules_hash != kinds.hash) {
        eprintln!(
            "rules: changed since this save was last played (hash {:016x}, was {:016x})",
            kinds.hash, m.rules_hash
        );
    }
    Ok(kinds)
}

/// The author lint's warnings about `kinds`, once on stderr (`wmc lint`
/// prints them with the notes).
pub fn warn(kinds: &Kinds) {
    let warnings: Vec<_> = kinds
        .debug
        .diagnostics
        .iter()
        .filter(|d| d.level == sim_core::rules::Level::Warning)
        .collect();
    if !warnings.is_empty() {
        eprintln!(
            "rules: {} warnings (wmc lint shows them with notes)",
            warnings.len()
        );
        for d in warnings {
            eprintln!("  {d}");
        }
    }
}

/// Paths for a message: `a, b`.
pub fn shown(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
