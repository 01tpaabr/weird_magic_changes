//! Front end of the game: the Bevy `App`, camera, sim clock, rendering,
//! input. `main.rs` is the CLI shell over this. Nothing here is visible to
//! `sim-core`, which knows no engine clock, asset or window.
//!
//! - [`camera`]: where the player looks (fractional cell, eased velocity).
//! - [`clock`]: real time -> number of sim ticks this frame (speed, pause, budget).
//! - [`render`]: `Stage` -> `CellFrame` (glyph, fg, bg per cell) -> tilemap tiles.
//! - [`play`]: the windowed game: `PlayPlugin`, its resources and systems.
//! - [`packs`], [`compile`], [`rules_for`]: which rules the app runs (packs
//!   from `--rules` or `WMC_RULES`, a save's own packs, or the built-in ones),
//!   with the constants `--set` gives ([`set_consts`]).
//! - [`why`]: `wmc why`, one actor's think explained.
//! - [`inspect`]: the inspector panel of `wmc play`.
//! - [`observe`]: `wmc observe`, how each kind lives and dies over a run.
//! - [`lab`]: `wmc lab`, a scenario's goals across const values and seeds.

// A narrowing `as` either is a checked conversion or says why the value
// fits (or that the truncation is the point). Tests may narrow freely.
#![warn(clippy::cast_possible_truncation)]
#![cfg_attr(test, allow(clippy::cast_possible_truncation))]
pub mod camera;
pub mod clock;
pub mod highlight;
pub mod inspect;
pub mod lab;
pub mod observe;
pub mod play;
pub mod render;
pub mod why;

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Context;
use sim_core::rules::ConstSet;
use sim_core::{Kinds, Store};

static CONSTS: OnceLock<Vec<ConstSet>> = OnceLock::new();

/// The constants the command line sets (`--set NAME=V`) for every rule set
/// this session compiles, a hot reload's too. `main` sets them once,
/// before anything compiles; later calls change nothing.
pub fn set_consts(consts: Vec<ConstSet>) {
    let _ = CONSTS.set(consts);
}

/// The session's constants ([`set_consts`]); none unless set.
pub fn consts() -> &'static [ConstSet] {
    CONSTS.get().map_or(&[], Vec::as_slice)
}

/// The rule packs a session asked for: the `--rules` paths, else
/// `WMC_RULES` (paths separated by `:`; empty entries are skipped, so an
/// empty `WMC_RULES` is none), else none.
pub fn packs(cli: &[String]) -> Vec<PathBuf> {
    if !cli.is_empty() {
        return cli.iter().map(PathBuf::from).collect();
    }
    std::env::var_os("WMC_RULES").map_or_else(Vec::new, |v| {
        std::env::split_paths(&v)
            .filter(|p| !p.as_os_str().is_empty())
            .collect()
    })
}

/// The packs for a new world: the session's ([`packs`]), else the ones its
/// scenario names (`rules` lines, relative to the scenario file).
pub fn packs_or(cli: &[String], scenario: &[String]) -> Vec<PathBuf> {
    let session = packs(cli);
    if session.is_empty() {
        scenario.iter().map(PathBuf::from).collect()
    } else {
        session
    }
}

/// Compile `packs` as one rule set (`sim_core::rules::compile_packs`),
/// with the session's constants ([`consts`]); no pack is the rules built
/// into the binary.
pub fn compile(packs: &[PathBuf]) -> anyhow::Result<Kinds> {
    compile_with(packs, consts())
}

/// [`compile`] with these constants set (`wmc lab`'s variants).
pub fn compile_with(packs: &[PathBuf], consts: &[ConstSet]) -> anyhow::Result<Kinds> {
    // Errors name their file (`file:line:col: ...`, or the path that failed).
    if packs.is_empty() {
        if consts.is_empty() {
            return Ok(Kinds::builtin());
        }
        return sim_core::rules::compile_files_with(&sim_core::rules::builtin::FILES, consts)
            .map_err(|e| anyhow::anyhow!("{e}"));
    }
    let paths: Vec<&Path> = packs.iter().map(PathBuf::as_path).collect();
    sim_core::rules::compile_packs_with(&paths, consts).map_err(|e| anyhow::anyhow!("{e}"))
}

/// The rules for the world in `store`: the session's packs, else (opening
/// a save) the packs it was last played with, if they all still exist, or
/// (making one) the packs its scenario names, else the built-in rules. Says
/// on stderr when it is not the session's choice, and when the rules
/// changed since the save was last played.
pub fn rules_for(store: &Store, cli: &[String], scenario: &[String]) -> anyhow::Result<Kinds> {
    let meta = store.read_meta().context("reading save")?;
    let mut packs = packs(cli);
    if packs.is_empty() && meta.is_none() {
        packs = scenario.iter().map(PathBuf::from).collect();
    }
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
                "rules: the save's packs are missing ({}); using the built-in rules \
                 (saving will keep them)",
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

/// Says on stderr which of an opened save's starts name a kind the rules
/// lack: the save keeps them, and they place nobody until the kind is back.
pub fn say_skipped_starts(world: &bevy::prelude::World) {
    let starts = &world.resource::<sim_core::SimConfig>().starts;
    let skipped = sim_core::scenario::absent(starts, world.resource::<Kinds>());
    if !skipped.is_empty() {
        eprintln!(
            "starts: skipped, the rules lack their kinds (the save keeps them): {}",
            skipped.join(", ")
        );
    }
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

/// The name of the built-in scenario, for messages.
pub const DEFAULT_SCENARIO: &str = "default.scenario";

/// A new world from the scenario `name` failed with `e`: say which file
/// (and line, when `e` is about one start: [`Scenario::line_of`]) does not
/// fit, and when it is the built-in one, that the rules need their own.
///
/// [`Scenario::line_of`]: sim_core::Scenario::line_of
pub fn new_world_error(name: &str, line: Option<u32>, e: impl std::fmt::Display) -> anyhow::Error {
    let hint = if name == DEFAULT_SCENARIO {
        " (these rules need their own scenario: --scenario <file>)"
    } else {
        ""
    };
    match line {
        Some(line) => anyhow::anyhow!("{name}:{line}: {e}{hint}"),
        None => anyhow::anyhow!("{name}: {e}{hint}"),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Rules that do not fit the built-in scenario are told to bring their
    /// own; a scenario file that does not fit is only named.
    #[test]
    fn a_new_world_error_hints_at_a_scenario_for_the_default_one() {
        let e = new_world_error(DEFAULT_SCENARIO, None, "x").to_string();
        assert!(
            e.starts_with("default.scenario: x") && e.contains("--scenario <file>"),
            "{e}"
        );
        assert_eq!(
            new_world_error("a.scenario", None, "x").to_string(),
            "a.scenario: x"
        );
        assert_eq!(
            new_world_error("a.scenario", Some(4), "x").to_string(),
            "a.scenario:4: x"
        );
    }
}
