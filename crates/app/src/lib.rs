//! Front end of the game: camera, sim clock, rendering pipeline, window.
//! `main.rs` is the CLI shell over this. Nothing here is visible to `sim-core`.
pub mod camera;
pub mod clock;
pub mod render;
pub mod window;
