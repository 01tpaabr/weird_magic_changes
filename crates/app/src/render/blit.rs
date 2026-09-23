//! Phase 2 of a frame: cells + atlas -> RGBA8 pixels.
//!
//! Work unit: a band of [`BAND_ROWS`] whole cell rows. Bands are fixed by
//! index, each writes a disjoint slice of the framebuffer, and inside a band
//! the Zig kernel `wmc_blit_cells` does the per-pixel work. There is no
//! reduction, so any thread count paints the same bytes.
//!
//! [`blit_reference`] is the scalar Rust definition of the same function. It
//! is the correctness oracle for the kernel and the baseline in `make bench`.

use rayon::prelude::*;

use super::atlas::GlyphAtlas;
use super::cells::CellFrame;

/// Cell rows per parallel task. A 32 px cell on a 2560-wide buffer makes one
/// row 320 KB, so 4 rows is ~1.3 MB of streaming writes per task; tune with
/// `make bench` (`blit/parallel`).
pub const BAND_ROWS: usize = 4;

/// Paint `frame` into `out` (`stride_px` pixels per row, RGBA8). Pixels to
/// the right of `cols*cell` and below `rows*cell` are left untouched.
pub fn blit(frame: &CellFrame, atlas: &GlyphAtlas, out: &mut [u8], stride_px: usize) {
    let (cols, rows, cell) = (frame.cols(), frame.rows(), atlas.cell());
    assert!(
        cols * cell <= stride_px,
        "frame wider than the pixel buffer"
    );
    let row_bytes = cell * stride_px * 4;
    let used = rows * row_bytes;
    assert!(out.len() >= used, "pixel buffer too small for the frame");
    if used == 0 {
        return;
    }
    out[..used]
        .par_chunks_mut(BAND_ROWS * row_bytes)
        .enumerate()
        .for_each(|(b, band)| {
            let row0 = b * BAND_ROWS;
            let n_rows = band.len() / row_bytes;
            let cells = row0 * cols..(row0 + n_rows) * cols;
            let grid = zig_kernels::CellGrid {
                glyph: &frame.glyph[cells.clone()],
                fg: &frame.fg[cells.clone()],
                bg: &frame.bg[cells],
                cols,
                rows: n_rows,
            };
            let atlas = zig_kernels::Atlas {
                coverage: atlas.coverage(),
                cell,
            };
            zig_kernels::blit_cells(grid, atlas, band, stride_px);
        });
}

/// Scalar reference: exactly what [`blit`] must produce, written the obvious way.
pub fn blit_reference(frame: &CellFrame, atlas: &GlyphAtlas, out: &mut [u8], stride_px: usize) {
    let (cols, rows, cell) = (frame.cols(), frame.rows(), atlas.cell());
    assert!(cols * cell <= stride_px);
    assert!(out.len() >= rows * cell * stride_px * 4);
    for r in 0..rows {
        for c in 0..cols {
            let i = r * cols + c;
            let cov = atlas.glyph(frame.glyph[i]);
            let f = frame.fg[i].to_le_bytes();
            let b = frame.bg[i].to_le_bytes();
            for y in 0..cell {
                for x in 0..cell {
                    let cv = u32::from(cov[y * cell + x]);
                    let px = ((r * cell + y) * stride_px + c * cell + x) * 4;
                    for ch in 0..4 {
                        let v = (u32::from(f[ch]) * cv + u32::from(b[ch]) * (255 - cv) + 127) / 255;
                        out[px + ch] = v as u8;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::cells::{Viewport, render_cells};
    use crate::render::palette::{Rgba, TEXT_BG, TEXT_FG};
    use sim_core::stage::worldgen::GenParams;
    use sim_core::{Pos, World, WorldConfig};

    fn sample_frame() -> CellFrame {
        let world = World::new(&WorldConfig {
            width: 128,
            height: 128,
            seed: 3,
            params: GenParams::default(),
        });
        let view = Viewport::centered(Pos::new(20, 20), 37, 11);
        let mut f = CellFrame::new();
        f.resize(37, 13);
        render_cells(&world.stage, view, &mut f);
        f.put_text(11, "status @ 12 ~#", TEXT_FG, TEXT_BG);
        f.put_text(12, "keys", Rgba::rgb(1, 2, 3), Rgba::rgb(9, 8, 7));
        f
    }

    fn with_threads<T: Send>(n: usize, f: impl FnOnce() -> T + Send) -> T {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .unwrap()
            .install(f)
    }

    #[test]
    fn zig_blit_matches_reference_and_leaves_padding() {
        let frame = sample_frame();
        let atlas = GlyphAtlas::build(9);
        let stride = 37 * 9 + 5;
        let height = 13 * 9 + 3;
        let mut want = vec![0xEEu8; stride * height * 4];
        let mut got = want.clone();
        blit_reference(&frame, &atlas, &mut want, stride);
        blit(&frame, &atlas, &mut got, stride);
        assert!(got == want, "kernel output differs from reference");
        // Padding column and bottom rows untouched.
        assert!(want[(37 * 9 * 4)..(stride * 4)].iter().all(|&b| b == 0xEE));
        assert!(want[13 * 9 * stride * 4..].iter().all(|&b| b == 0xEE));
    }

    #[test]
    fn same_pixels_on_one_and_many_threads() {
        let frame = sample_frame();
        let atlas = GlyphAtlas::build(7);
        let stride = 37 * 7;
        let mut a = vec![0u8; stride * 13 * 7 * 4];
        let mut b = a.clone();
        with_threads(1, || blit(&frame, &atlas, &mut a, stride));
        with_threads(8, || blit(&frame, &atlas, &mut b, stride));
        assert!(a == b);
    }

    #[test]
    fn empty_frame_is_a_no_op() {
        let frame = CellFrame::new();
        let atlas = GlyphAtlas::build(8);
        let mut out = vec![7u8; 64];
        blit(&frame, &atlas, &mut out, 4);
        assert!(out.iter().all(|&b| b == 7));
    }
}
