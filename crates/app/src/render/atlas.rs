//! Glyph coverage atlas: every printable ASCII glyph rasterized once into a
//! square `cell x cell` box, one coverage byte per pixel (0 = background,
//! 255 = glyph). Built when the cell size changes (zoom, DPI), never per frame.
//!
//! The font is bundled (`assets/JetBrainsMonoNL-Regular.ttf`, OFL) so the
//! binary is self-contained and the picture is identical on every machine.

use fontdue::{Font, FontSettings};

pub const FIRST_GLYPH: u8 = b' ';
pub const LAST_GLYPH: u8 = b'~';
pub const GLYPH_COUNT: usize = (LAST_GLYPH - FIRST_GLYPH + 1) as usize;
const _: () = assert!(GLYPH_COUNT == zig_kernels::ATLAS_GLYPHS);

static FONT: &[u8] = include_bytes!("../../assets/JetBrainsMonoNL-Regular.ttf");

/// Font pixel size as a fraction of the cell. JetBrains Mono's line box is
/// 1.32 em and its advance 0.6 em, so 0.9 makes glyphs 0.54 cells wide with
/// caps 0.66 cells tall; only the extremes of ascenders/descenders clip.
const FONT_SCALE: f32 = 0.9;

#[derive(Debug)]
pub struct GlyphAtlas {
    cell: usize,
    /// `GLYPH_COUNT * cell * cell`, box `i` = glyph `FIRST_GLYPH + i`.
    coverage: Vec<u8>,
}

impl GlyphAtlas {
    pub fn build(cell_px: u32) -> Self {
        let cell = cell_px.max(1) as usize;
        let font = Font::from_bytes(FONT, FontSettings::default()).expect("bundled font parses");
        let px = cell as f32 * FONT_SCALE;
        let line = font
            .horizontal_line_metrics(px)
            .expect("bundled font has horizontal metrics");
        let advance = font.metrics('M', px).advance_width;
        // Centre the font's line box vertically and its advance box horizontally.
        let baseline = ((cell as f32 - (line.ascent - line.descent)) / 2.0 + line.ascent).round();
        let baseline = baseline as i32;
        let x_pad = ((cell as f32 - advance) / 2.0).round() as i32;
        let cell_i = i32::try_from(cell).expect("cell fits i32");

        let mut coverage = vec![0u8; GLYPH_COUNT * cell * cell];
        for (i, boxed) in coverage.chunks_exact_mut(cell * cell).enumerate() {
            let ch = char::from(FIRST_GLYPH + i as u8);
            let (m, bitmap) = font.rasterize(ch, px);
            if m.width == 0 || m.height == 0 {
                continue;
            }
            let x0 = x_pad + m.xmin;
            // fontdue: `ymin` is the bitmap's bottom edge above the baseline; rows top-down.
            let y0 = baseline - (m.ymin + m.height as i32);
            for (r, line) in bitmap.chunks_exact(m.width).enumerate() {
                let y = y0 + r as i32;
                if y < 0 || y >= cell_i {
                    continue;
                }
                for (c, &v) in line.iter().enumerate() {
                    let x = x0 + c as i32;
                    if x < 0 || x >= cell_i {
                        continue;
                    }
                    boxed[y as usize * cell + x as usize] = v;
                }
            }
        }
        Self { cell, coverage }
    }

    /// Cell edge in pixels.
    pub fn cell(&self) -> usize {
        self.cell
    }

    pub fn coverage(&self) -> &[u8] {
        &self.coverage
    }

    pub fn index(byte: u8) -> usize {
        let b = if (FIRST_GLYPH..=LAST_GLYPH).contains(&byte) {
            byte
        } else {
            b'?'
        };
        usize::from(b - FIRST_GLYPH)
    }

    /// The `cell*cell` coverage box for `byte`.
    pub fn glyph(&self, byte: u8) -> &[u8] {
        let n = self.cell * self.cell;
        &self.coverage[Self::index(byte) * n..][..n]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atlas_has_every_glyph_in_its_box() {
        let a = GlyphAtlas::build(16);
        assert_eq!(a.cell(), 16);
        assert_eq!(a.coverage().len(), GLYPH_COUNT * 256);
        assert!(a.glyph(b' ').iter().all(|&v| v == 0));
        for g in [b'.', b'~', b'#', b'@', b'M', b'g'] {
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
}
