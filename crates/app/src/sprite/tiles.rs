//! A rule set's sprites as one tileset: every frame of every pose of every
//! worn sprite drawn at the cell size and cut into cell tiles, in order.
//! Built when the sprites or the cell size change, never per frame; on the
//! GPU an array texture beside the glyph atlas (colour in RGB, coverage in
//! alpha), drawn by the map's third tilemap chunk.
//!
//! Tile order: sprite by sprite (as [`Wardrobe::sprites`]), pose by pose,
//! frame by frame, then the footprint's cells row by row: the tile of part
//! `(dx, dy)` of frame `f` of a pose is `pose.first + f * W * H + dy * W +
//! dx`.

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageSampler};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use sim_core::Kinds;

use super::Wardrobe;
use super::raster::draw;
use crate::render::palette::SpriteTile;

/// A pose's place in the tileset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PoseTiles {
    /// `None`: the sprite's own stamps.
    pub look: Option<u8>,
    pub first: u16,
    pub frames: u8,
    pub every_ms: u32,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpriteTiles {
    /// The cell size the tiles are drawn at, in pixels.
    pub cell: u32,
    pub count: u32,
    /// `count * cell * cell` RGBA8, straight alpha.
    pub rgba: Vec<u8>,
    /// Per worn sprite, its poses in file order.
    pub poses: Vec<Vec<PoseTiles>>,
    /// Per kind: its picture's first tile (the own pose, frame 0) plus one
    /// and its width, for [`crate::render::palette::Looks::sprites`].
    pub table: Vec<SpriteTile>,
}

impl SpriteTiles {
    /// Draw `w`'s sprites at `cell` pixels, in the colours of the kinds
    /// they name (`kinds`). A child that wears its parent's sprite wears
    /// the parent's picture, colours and all.
    pub fn build(w: &Wardrobe, kinds: &Kinds, cell: u32) -> Self {
        let c = cell.max(1) as usize;
        let mut rgba = Vec::with_capacity(w.tiles() as usize * c * c * 4);
        let mut poses = Vec::with_capacity(w.sprites.len());
        let mut next: u32 = 0;
        for worn in &w.sprites {
            let kind = kinds
                .colors
                .get(usize::from(worn.kind))
                .copied()
                .unwrap_or(0);
            let (sw, sh) = (usize::from(worn.size.0), usize::from(worn.size.1));
            let mut mine = Vec::with_capacity(worn.sprite.poses.len());
            for p in &worn.sprite.poses {
                mine.push(PoseTiles {
                    look: p.look,
                    first: u16::try_from(next).expect("a tile budget fits u16"),
                    frames: p.frames.n,
                    every_ms: p.frames.every_ms,
                });
                for f in 0..p.frames.n {
                    let pic = draw(p, f, &worn.palette, kind, worn.size, cell);
                    for dy in 0..sh {
                        for dx in 0..sw {
                            rgba.extend_from_slice(&pic.cut(dx, dy, c).rgba);
                        }
                    }
                    next += u32::try_from(sw * sh).expect("16 x 16 at most");
                }
            }
            poses.push(mine);
        }
        let table = w
            .of_kind
            .iter()
            .map(|s| match s {
                Some(i) => {
                    let i = usize::from(*i);
                    SpriteTile {
                        first: poses[i][0].first + 1,
                        width: w.sprites[i].size.0,
                    }
                }
                None => SpriteTile::default(),
            })
            .collect();
        Self {
            cell,
            count: next,
            rgba,
            poses,
            table,
        }
    }

    /// The tiles as a GPU array texture (at least one layer: an empty one
    /// when there are none), sRGB, sampled linearly as the glyphs are.
    pub fn tileset(&self) -> Image {
        let side = self.cell.max(1);
        let layers = self.count.max(1);
        let mut data = self.rgba.clone();
        data.resize((layers * side * side * 4) as usize, 0);
        let mut image = Image::new(
            Extent3d {
                width: side,
                height: side,
                depth_or_array_layers: layers,
            },
            TextureDimension::D2,
            data,
            TextureFormat::Rgba8UnormSrgb,
            RenderAssetUsages::RENDER_WORLD,
        );
        image.sampler = ImageSampler::linear();
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sprite::{Source, load};

    /// Every frame of every pose, cut into the footprint's cells, in order;
    /// the table points each wearer at its sprite's first tile.
    #[test]
    fn tiles_are_every_frame_of_every_pose_cell_by_cell() {
        let kinds = sim_core::rules::compile_files(&[(
            "t.rules",
            "kind a { glyph \"a\" color \"#ff0000\" }\nkind b extends a { glyph \"b\" }\nkind big { glyph \"B\" size 2 2 }\nkind none { glyph \"n\" }",
        )])
        .unwrap();
        let text = "sprite a { frames 4 every 1s \"@\" at 0.5 0.5 ~x 0.2 pose 1 { \"#\" at 0.5 0.5 } }\nsprite big { \"O\" at 1 1 scale 2 }";
        let src = Source {
            label: "t.sprite".into(),
            text: text.into(),
            owner: None,
        };
        let (w, _) = load(&[src], &kinds).unwrap();
        let t = SpriteTiles::build(&w, &kinds, 8);
        // a: 2 poses x 4 frames; big: 1 x 1 x 4 cells.
        assert_eq!(t.count, w.tiles());
        assert_eq!(t.count, 12);
        assert_eq!(t.rgba.len(), 12 * 8 * 8 * 4);
        assert_eq!(t.poses[0][0].first, 0);
        assert_eq!(
            t.poses[0][1],
            PoseTiles {
                look: Some(1),
                first: 4,
                frames: 4,
                every_ms: 1000
            }
        );
        assert_eq!(t.poses[1][0].first, 8);
        let id = |n: &str| usize::from(kinds.by_name(n).unwrap().id);
        assert_eq!(t.table[id("a")], SpriteTile { first: 1, width: 1 });
        assert_eq!(t.table[id("b")], SpriteTile { first: 1, width: 1 });
        assert_eq!(t.table[id("big")], SpriteTile { first: 9, width: 2 });
        assert_eq!(t.table[id("none")], SpriteTile::default());
        // Tile 0 is a's first frame, drawn in a's red.
        let tile = |i: usize| &t.rgba[i * 256..(i + 1) * 256];
        let pic = draw(&w.sprites[0].sprite.poses[0], 0, &[], 0xff0000, (1, 1), 8);
        assert_eq!(tile(0), &pic.rgba[..]);
        assert!(tile(0).chunks(4).any(|p| p[3] > 0 && p[..3] == [255, 0, 0]));
        // Frames 0 and 1 differ (the swing, at sin 0 and 90), and big's four cells are its cut.
        assert_ne!(tile(0), tile(1));
        // big declares no colour: the default one.
        let big = draw(
            &w.sprites[1].sprite.poses[0],
            0,
            &[],
            kinds.colors[id("big")],
            (2, 2),
            8,
        );
        assert_eq!(tile(8 + 3), &big.cut(1, 1, 8).rgba[..]);
        // On the GPU: one layer a tile.
        let img = t.tileset();
        assert_eq!(img.texture_descriptor.size.depth_or_array_layers, 12);
        let empty = SpriteTiles::build(&Wardrobe::default(), &kinds, 8).tileset();
        assert_eq!(empty.texture_descriptor.size.depth_or_array_layers, 1);
    }
}
