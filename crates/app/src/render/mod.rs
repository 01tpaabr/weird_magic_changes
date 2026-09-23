//! Rendering: `Stage` -> pixels, in two pure phases.
//!
//! ```text
//! phase 1  cells::render_cells   Stage viewport -> CellFrame (glyph, fg, bg per cell)   row-parallel
//! phase 2  blit::blit            CellFrame + GlyphAtlas -> RGBA8 pixels (Zig kernel)     band-parallel
//! ```
//! Each phase writes disjoint slices per task and reads only immutable state,
//! so the picture is bit-identical for any thread count. `ascii` is a text
//! view of phase 1 for `wmc show` and tests. `sim-core` knows nothing of this.
pub mod ascii;
pub mod atlas;
pub mod blit;
pub mod cells;
pub mod palette;
