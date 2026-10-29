//! Glyph coverage atlas: every printable ASCII glyph rasterized once into a
//! `width x height` box, one coverage byte per pixel (0 = background,
//! 255 = glyph), plus one solid box for cell backgrounds. The map's boxes are
//! square ([`GlyphAtlas::build`]); text boxes are a glyph's advance wide
//! ([`GlyphAtlas::text`], the inspector), so twice the columns fit. Built when the
//! physical cell size changes (zoom, DPI), never per frame, then handed to
//! the GPU as a 2D array texture ([`GlyphAtlas::tileset`]): one layer per
//! box, white, coverage in alpha, so a per-tile tint gives the glyph colour.
//!
//! The font is bundled (`assets/JetBrainsMonoNL-Regular.ttf`, OFL) so the
//! binary is self-contained and the picture is identical on every machine.

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageSampler};
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use fontdue::{Font, FontSettings};

pub const FIRST_GLYPH: u8 = b' ';
pub const LAST_GLYPH: u8 = b'~';
pub const GLYPH_COUNT: usize = (LAST_GLYPH - FIRST_GLYPH + 1) as usize;
/// Tileset layer of the fully covered box (cell backgrounds).
pub const SOLID: u16 = (LAST_GLYPH - FIRST_GLYPH) as u16 + 1;
/// Layers in the tileset: the glyphs and the solid box.
pub const LAYERS: usize = GLYPH_COUNT + 1;

static FONT: &[u8] = include_bytes!("../../assets/JetBrainsMonoNL-Regular.ttf");

/// Font pixel size as a fraction of the cell. JetBrains Mono's line box is
/// 1.32 em and its advance 0.6 em, so 0.9 makes glyphs 0.54 cells wide with
/// caps 0.66 cells tall; only the extremes of ascenders/descenders clip.
const FONT_SCALE: f32 = 0.9;

/// A text box's width over its height: the font's advance (0.6 em) at
/// [`FONT_SCALE`] is 0.54 of the height, and the rest is spacing.
pub const TEXT_ASPECT: f32 = 0.6;

#[derive(Debug)]
pub struct GlyphAtlas {
    width: usize,
    height: usize,
    /// `GLYPH_COUNT * width * height`, box `i` = glyph `FIRST_GLYPH + i`.
    coverage: Vec<u8>,
}

impl GlyphAtlas {
    /// Square `cell x cell` boxes: the map.
    pub fn build(cell_px: u32) -> Self {
        Self::boxes(cell_px, cell_px)
    }

    /// Boxes `height` tall and [`TEXT_ASPECT`] of that wide: running text.
    pub fn text(height_px: u32) -> Self {
        Self::boxes(Self::text_width(height_px), height_px)
    }

    /// The width of a text box `height_px` tall, at least 1.
    pub fn text_width(height_px: u32) -> u32 {
        // A box of a few dozen pixels: far inside u32.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let w = (height_px as f32 * TEXT_ASPECT).round() as u32;
        w.max(1)
    }

    fn boxes(width_px: u32, height_px: u32) -> Self {
        let (width, height) = (width_px.max(1) as usize, height_px.max(1) as usize);
        let font = Font::from_bytes(FONT, FontSettings::default()).expect("bundled font parses");
        let px = height as f32 * FONT_SCALE;
        let line = font
            .horizontal_line_metrics(px)
            .expect("bundled font has horizontal metrics");
        let advance = font.metrics('M', px).advance_width;
        // Centre the font's line box vertically and its advance box horizontally.
        let baseline = ((height as f32 - (line.ascent - line.descent)) / 2.0 + line.ascent).round();
        let (w_i, h_i) = (
            i32::try_from(width).expect("a box fits i32"),
            i32::try_from(height).expect("a box fits i32"),
        );
        // Offsets within a box or two of 0, and the box fits i32.
        #[allow(clippy::cast_possible_truncation)]
        let (baseline, x_pad) = (
            baseline as i32,
            ((width as f32 - advance) / 2.0).round() as i32,
        );

        let n = width * height;
        let mut coverage = vec![0u8; GLYPH_COUNT * n];
        for (ch, boxed) in (FIRST_GLYPH..=LAST_GLYPH).zip(coverage.chunks_exact_mut(n)) {
            let ch = char::from(ch);
            let (m, bitmap) = font.rasterize(ch, px);
            if m.width == 0 || m.height == 0 {
                continue;
            }
            let x0 = x_pad + m.xmin;
            // fontdue: `ymin` is the bitmap's bottom edge above the baseline; rows top-down.
            let height = i32::try_from(m.height).expect("a glyph is a few cells tall");
            let y0 = baseline - (m.ymin + height);
            for (y, line) in (y0..).zip(bitmap.chunks_exact(m.width)) {
                if y < 0 || y >= h_i {
                    continue;
                }
                for (x, &v) in (x0..).zip(line) {
                    if x < 0 || x >= w_i {
                        continue;
                    }
                    boxed[y as usize * width + x as usize] = v;
                }
            }
        }
        Self {
            width,
            height,
            coverage,
        }
    }

    /// A box's width and height in pixels.
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    pub fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    pub fn index(byte: u8) -> usize {
        usize::from(Self::offset(byte))
    }

    /// `byte`'s box, counted from [`FIRST_GLYPH`] (unknown bytes: `?`'s).
    #[inline]
    fn offset(byte: u8) -> u8 {
        let b = if (FIRST_GLYPH..=LAST_GLYPH).contains(&byte) {
            byte
        } else {
            b'?'
        };
        b - FIRST_GLYPH
    }

    /// The `width * height` coverage box for `byte`.
    pub fn glyph(&self, byte: u8) -> &[u8] {
        let n = self.width * self.height;
        &self.coverage[Self::index(byte) * n..][..n]
    }

    /// Tileset layer for `byte` (unknown bytes draw as `?`).
    #[inline]
    pub fn layer(byte: u8) -> u16 {
        u16::from(Self::offset(byte))
    }

    /// The atlas as a GPU array texture: [`LAYERS`] layers of `width x
    /// height` RGBA8 (sRGB), white with coverage in alpha; the last layer is
    /// solid.
    /// Sampled linearly: displayed 1:1 with physical pixels it is exact, and
    /// off by a fraction of a pixel (odd scale factors) it stays smooth.
    pub fn tileset(&self) -> Image {
        let (width, height) = (
            u32::try_from(self.width).expect("a box fits u32"),
            u32::try_from(self.height).expect("a box fits u32"),
        );
        let n = self.width * self.height;
        let mut data = Vec::with_capacity(LAYERS * n * 4);
        for &cov in &self.coverage {
            data.extend_from_slice(&[255, 255, 255, cov]);
        }
        data.extend(std::iter::repeat_n([255u8; 4], n).flatten());
        let mut image = Image::new(
            Extent3d {
                width,
                height,
                depth_or_array_layers: u32::try_from(LAYERS).expect("96 layers"),
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

    #[test]
    fn atlas_has_every_glyph_in_its_box() {
        let a = GlyphAtlas::build(16);
        assert_eq!(a.size(), (16, 16));
        assert_eq!(a.coverage().len(), GLYPH_COUNT * 256);
        assert!(a.glyph(b' ').iter().all(|&v| v == 0));
        for g in *b".~#@Mg" {
            assert!(
                a.glyph(g).iter().any(|&v| v > 128),
                "glyph {} empty",
                char::from(g)
            );
        }
        // Everything drawn stays inside the middle of the square: no glyph
        // touches the left or right edge column at this size.
        for i in 0..GLYPH_COUNT {
            let b = a.glyph(FIRST_GLYPH + i as u8);
            for y in 0..16 {
                assert_eq!(b[y * 16], 0, "glyph {i} touches left edge");
                assert_eq!(b[y * 16 + 15], 0, "glyph {i} touches right edge");
            }
        }
    }

    #[test]
    fn text_boxes_are_narrow_and_keep_every_glyph_inside() {
        let a = GlyphAtlas::text(20);
        assert_eq!(a.size(), (12, 20));
        assert_eq!(GlyphAtlas::text_width(20), 12);
        assert_eq!(GlyphAtlas::text_width(1), 1);
        for g in *b"Mmw@#" {
            let b = a.glyph(g);
            assert!(b.iter().any(|&v| v > 128), "glyph {} empty", char::from(g));
        }
        // Box `i` is glyph `i`: an `l` and an `M` differ.
        assert_ne!(a.glyph(b'l'), a.glyph(b'M'));
        let img = a.tileset();
        assert_eq!((img.width(), img.height()), (12, 20));
    }

    #[test]
    fn index_clamps_to_question_mark() {
        assert_eq!(GlyphAtlas::index(b' '), 0);
        assert_eq!(GlyphAtlas::index(b'~'), GLYPH_COUNT - 1);
        assert_eq!(GlyphAtlas::index(0), GlyphAtlas::index(b'?'));
        assert_eq!(GlyphAtlas::index(200), GlyphAtlas::index(b'?'));
    }

    #[test]
    fn tiny_and_odd_cells_build() {
        for c in [1u32, 2, 5, 13, 31] {
            let a = GlyphAtlas::build(c);
            assert_eq!(a.coverage().len(), GLYPH_COUNT * (c * c) as usize);
        }
    }

    #[test]
    fn tileset_is_one_white_layer_per_box_plus_solid() {
        let a = GlyphAtlas::build(8);
        let img = a.tileset();
        assert_eq!(
            img.texture_descriptor.size.depth_or_array_layers,
            LAYERS as u32
        );
        assert_eq!((img.width(), img.height()), (8, 8));
        let data = img.data.as_ref().unwrap();
        assert_eq!(data.len(), LAYERS * 64 * 4);
        // Glyph layers: white, alpha = coverage. `#` has ink, space has none.
        let layer = |l: usize| &data[l * 64 * 4..][..64 * 4];
        assert!(
            layer(GlyphAtlas::index(b' '))
                .chunks(4)
                .all(|p| p == [255, 255, 255, 0])
        );
        assert!(layer(GlyphAtlas::index(b'#')).chunks(4).any(|p| p[3] > 128));
        assert!(
            layer(SOLID as usize)
                .chunks(4)
                .all(|p| p == [255, 255, 255, 255])
        );
        assert_eq!(GlyphAtlas::layer(b' '), 0);
        assert_eq!(GlyphAtlas::layer(200), GlyphAtlas::layer(b'?'));
    }
}
