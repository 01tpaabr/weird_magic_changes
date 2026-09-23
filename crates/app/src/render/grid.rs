//! Phase 2 of a frame: a [`CellFrame`] as GPU tiles.
//!
//! A [`Layer`] is two `TilemapChunk` entities the size of the frame: the
//! **bg** chunk draws the solid tileset box tinted by each cell's background,
//! the **fg** chunk draws the glyph box tinted by the foreground, blended on
//! top. Bevy re-uploads a chunk's tile data when the component changes and
//! draws each chunk in one call; we write `cols x rows` tiles per layer per
//! frame, no pixels.
//!
//! Tile order inside a chunk is row-major with **row 0 at the bottom** (the
//! shader flips), so frame row `r` lands in tile row `rows - 1 - r`. A chunk
//! mesh is centred on its `Transform`; [`Layer::place`] takes the screen
//! rectangle instead.

use bevy::prelude::*;
use bevy::sprite_render::{AlphaMode2d, TileData, TilemapChunk, TilemapChunkTileData};

use super::atlas::{GlyphAtlas, SOLID};
use super::cells::CellFrame;

/// Two tilemap chunks (bg under fg) covering one [`CellFrame`].
#[derive(Debug, Clone, Copy)]
pub struct Layer {
    pub bg: Entity,
    pub fg: Entity,
    /// Grid the chunks are currently built for; `0 x 0` until configured.
    pub cols: u32,
    pub rows: u32,
    /// Tile edge in world units (physical pixels) the chunks are built for.
    pub cell: u32,
    /// Depth of the bg chunk; fg is `z + 0.5`.
    pub z: f32,
}

impl Layer {
    /// Spawn the two entities, unconfigured (nothing drawn until
    /// [`Layer::configure`]).
    pub fn spawn(commands: &mut Commands, z: f32) -> Self {
        let bg = commands.spawn(Transform::from_xyz(0.0, 0.0, z)).id();
        let fg = commands.spawn(Transform::from_xyz(0.0, 0.0, z + 0.5)).id();
        Self {
            bg,
            fg,
            cols: 0,
            rows: 0,
            cell: 0,
            z,
        }
    }

    /// (Re)build both chunks for a `cols x rows` grid of `cell`-sized tiles
    /// drawn from `tileset`. `TilemapChunk` is immutable, so a new grid size,
    /// cell size or tileset means inserting fresh components; the tile data
    /// is replaced with an empty grid of the right length in the same insert
    /// (the chunk's insert hook checks it). Cheap and rare: resize, zoom, DPI.
    pub fn configure(
        &mut self,
        commands: &mut Commands,
        cols: u32,
        rows: u32,
        cell: u32,
        tileset: &Handle<Image>,
    ) {
        self.cols = cols;
        self.rows = rows;
        self.cell = cell;
        let n = (cols * rows) as usize;
        for (entity, alpha) in [(self.bg, AlphaMode2d::Blend), (self.fg, AlphaMode2d::Blend)] {
            commands.entity(entity).insert((
                TilemapChunk {
                    chunk_size: UVec2::new(cols, rows),
                    tile_display_size: UVec2::splat(cell),
                    tileset: tileset.clone(),
                    alpha_mode: alpha,
                },
                TilemapChunkTileData(vec![None; n]),
            ));
        }
    }

    /// Does this layer draw `frame` as built?
    pub fn fits(&self, frame: &CellFrame) -> bool {
        self.cols as usize == frame.cols() && self.rows as usize == frame.rows()
    }

    /// Where the two chunks go so that the grid's top-left tile corner sits at
    /// screen pixel `(left, top)` (y down, origin the window's top-left) in a
    /// `width x height` window, all in world units (physical pixels; the
    /// camera is at the window centre, y up).
    pub fn place(&self, left: f32, top: f32, width: f32, height: f32) -> (Vec3, Vec3) {
        let w = (self.cols * self.cell) as f32;
        let h = (self.rows * self.cell) as f32;
        let cx = left + w / 2.0 - width / 2.0;
        let cy = height / 2.0 - (top + h / 2.0);
        (Vec3::new(cx, cy, self.z), Vec3::new(cx, cy, self.z + 0.5))
    }
}

/// Write `frame` into a layer's tile data. Backgrounds are the solid box
/// tinted; glyphs are their box tinted, and blank cells are empty tiles
/// (nothing to blend). Both vectors must be `cols x rows` long.
pub fn upload(frame: &CellFrame, bg: &mut TilemapChunkTileData, fg: &mut TilemapChunkTileData) {
    let (cols, rows) = (frame.cols(), frame.rows());
    let n = cols * rows;
    assert_eq!(bg.len(), n, "bg tile data length");
    assert_eq!(fg.len(), n, "fg tile data length");
    for r in 0..rows {
        let src = r * cols..(r + 1) * cols;
        let dst = (rows - 1 - r) * cols;
        let cells = frame.glyph[src.clone()]
            .iter()
            .zip(&frame.fg[src.clone()])
            .zip(&frame.bg[src]);
        for (i, ((&g, &f), &b)) in cells.enumerate() {
            bg[dst + i] = Some(TileData {
                tileset_index: SOLID,
                color: super::palette::Color(b).tint(),
                ..TileData::default()
            });
            fg[dst + i] = (g != b' ').then(|| TileData {
                tileset_index: GlyphAtlas::layer(g),
                color: super::palette::Color(f).tint(),
                ..TileData::default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::palette::Color;

    fn layer(cols: u32, rows: u32, cell: u32) -> Layer {
        Layer {
            bg: Entity::PLACEHOLDER,
            fg: Entity::PLACEHOLDER,
            cols,
            rows,
            cell,
            z: 0.0,
        }
    }

    #[test]
    fn upload_flips_rows_and_leaves_blanks_empty() {
        let mut f = CellFrame::new();
        f.resize(3, 2);
        f.put_text(0, "a c", Color::rgb(1, 2, 3), Color::rgb(9, 9, 9));
        f.put_text(1, "#", Color::rgb(0, 0, 0), Color::rgb(0, 0, 0));
        let mut bg = TilemapChunkTileData(vec![None; 6]);
        let mut fg = TilemapChunkTileData(vec![None; 6]);
        upload(&f, &mut bg, &mut fg);
        // Frame row 1 ("#  ") is tile row 0 (bottom).
        assert_eq!(fg[0].unwrap().tileset_index, GlyphAtlas::layer(b'#'));
        assert!(fg[1].is_none() && fg[2].is_none());
        // Frame row 0 ("a c") is tile row 1.
        assert_eq!(fg[3].unwrap().tileset_index, GlyphAtlas::layer(b'a'));
        assert!(fg[4].is_none());
        assert_eq!(fg[5].unwrap().tileset_index, GlyphAtlas::layer(b'c'));
        assert!(bg.iter().all(|t| t.unwrap().tileset_index == SOLID));
        assert_eq!(fg[3].unwrap().color, Color::rgb(1, 2, 3).tint());
        assert_eq!(bg[3].unwrap().color, Color::rgb(9, 9, 9).tint());
    }

    #[test]
    fn place_puts_the_top_left_corner_where_asked() {
        // 4x2 tiles of 10 px in a 100x50 window, corner at (-3, 7).
        let (bg, fg) = layer(4, 2, 10).place(-3.0, 7.0, 100.0, 50.0);
        // Centre: x = -3 + 20 - 50 = -33; y = 25 - (7 + 10) = 8.
        assert_eq!(bg, Vec3::new(-33.0, 8.0, 0.0));
        assert_eq!(fg, Vec3::new(-33.0, 8.0, 0.5));
        // A grid exactly the window, corner at the origin, is centred.
        let (bg, _) = layer(10, 5, 10).place(0.0, 0.0, 100.0, 50.0);
        assert_eq!(bg, Vec3::ZERO);
    }

    #[test]
    fn fits_compares_grid_size() {
        let mut f = CellFrame::new();
        f.resize(4, 2);
        assert!(layer(4, 2, 16).fits(&f));
        assert!(!layer(4, 3, 16).fits(&f));
    }
}
