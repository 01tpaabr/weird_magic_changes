//! Front end of the game: the Bevy `App`, camera, sim clock, rendering,
//! input. `main.rs` is the CLI shell over this. Nothing here is visible to
//! `sim-core`, which knows no engine clock, asset or window.
//!
//! - [`camera`]: where the player looks (fractional cell, eased velocity).
//! - [`clock`]: real time -> number of sim ticks this frame (speed, pause, budget).
//! - [`render`]: `Stage` -> `CellFrame` (glyph, fg, bg per cell) -> tilemap tiles.
//! - [`play`]: the windowed game: `PlayPlugin`, its resources and systems.
pub mod camera;
pub mod clock;
pub mod play;
pub mod render;
