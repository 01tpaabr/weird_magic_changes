//! Rendering: `Stage` -> tiles, in two pure phases plus the GPU.
//!
//! ```text
//! phase 1  cells::render_cells   chunk lookup + viewport -> CellFrame (glyph, fg, bg per cell)   row-band-parallel
//! phase 2  grid::upload          CellFrame -> TilemapChunkTileData (bg tint, glyph layer + fg tint)
//! GPU      bevy_sprite_render    one draw per layer: tileset array texture sampled per tile
//! ```
//! Phase 1 writes disjoint rows per task and reads only immutable state, so
//! the frame is bit-identical for any thread count. `ascii` is a text view of
//! phase 1 for `wmc show` and tests. `atlas` builds the tileset. `palette` is
//! the one place a tile becomes (glyph, fg, bg). `sim-core` knows nothing of
//! this.
pub mod ascii;
pub mod atlas;
pub mod cells;
pub mod grid;
pub mod palette;
