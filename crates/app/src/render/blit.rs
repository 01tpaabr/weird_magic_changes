//! Phase 2 of a frame: cells + atlas -> pixels (4 bytes each).
//!
//! Work unit: a band of [`BAND_ROWS`] whole cell rows, clipped to the target
//! window. Bands are fixed by index, each writes a disjoint range of pixel
//! rows, and inside a band the Zig kernel `wmc_blit_cells` does the per-pixel
//! work. There is no reduction, so any thread count paints the same bytes.
//!
//! The grid may sit at any pixel offset (negative too): that is how the view
//! scrolls smoothly at sub-cell precision without touching the cell phase.
//!
//! [`blit_reference`] is the scalar Rust definition of the same function. It
//! is the correctness oracle for the kernel and the baseline in `make bench`.

use rayon::prelude::*;
pub use zig_kernels::Target;

use super::atlas::GlyphAtlas;
use super::cells::CellFrame;

/// Cell rows per parallel task. A 32 px cell on a 2560-wide buffer makes one
/// row 320 KB, so 4 rows is ~1.3 MB of streaming writes per task; tune with
/// `make bench` (`blit/zig`).
pub const BAND_ROWS: usize = 4;

/// Paint `frame` into `target`. Window pixels the grid does not cover are
/// left untouched.
pub fn blit(frame: &CellFrame, atlas: &GlyphAtlas, target: Target) {
    let (cols, rows, cell) = (frame.cols(), frame.rows(), atlas.cell());
    let Target {
        pixels,
        stride_px,
        width,
        height,
        origin_x,
        origin_y,
    } = target;
    assert!(
        width <= stride_px,
        "window wider than the pixel buffer stride"
    );
    assert!(
        pixels.len() >= height * stride_px * 4,
        "pixel buffer too small for the window"
    );
    if cols == 0 || rows == 0 || width == 0 || height == 0 {
        return;
    }
    let row_bytes = stride_px * 4;

    // Carve the window into one disjoint slice of pixel rows per band.
    let mut rest = &mut pixels[..height * row_bytes];
    let mut consumed = 0usize; // pixel rows handed out so far
    let mut bands = Vec::with_capacity(rows.div_ceil(BAND_ROWS));
    for r0 in (0..rows).step_by(BAND_ROWS) {
        let n = (rows - r0).min(BAND_ROWS);
        let grid_py0 = origin_y + (r0 * cell) as isize;
        let py0 = grid_py0.max(0) as usize;
        let py1 = (origin_y + ((r0 + n) * cell) as isize).min(height as isize);
        if py1 <= py0 as isize {
            continue;
        }
        let py1 = py1 as usize;
        let (_, tail) = std::mem::take(&mut rest).split_at_mut((py0 - consumed) * row_bytes);
        let (band, tail) = tail.split_at_mut((py1 - py0) * row_bytes);
        rest = tail;
        consumed = py1;
        bands.push((r0, n, band, grid_py0 - py0 as isize));
    }

    bands
        .into_par_iter()
        .for_each(|(r0, n, band, band_origin_y)| {
            let cells = r0 * cols..(r0 + n) * cols;
            let grid = zig_kernels::CellGrid {
                glyph: &frame.glyph[cells.clone()],
                fg: &frame.fg[cells.clone()],
                bg: &frame.bg[cells],
                cols,
                rows: n,
            };
            let atlas = zig_kernels::Atlas {
                coverage: atlas.coverage(),
                cell,
            };
            let height = band.len() / row_bytes;
            zig_kernels::blit_cells(
                grid,
                atlas,
                Target {
                    pixels: band,
                    stride_px,
                    width,
                    height,
                    origin_x,
                    origin_y: band_origin_y,
                },
            );
        });
}

/// Scalar reference: exactly what [`blit`] must produce, written the obvious
/// way (one window pixel at a time).
pub fn blit_reference(frame: &CellFrame, atlas: &GlyphAtlas, target: Target) {
    let (cols, rows, cell) = (frame.cols(), frame.rows(), atlas.cell());
    let Target {
        pixels,
        stride_px,
        width,
        height,
        origin_x,
        origin_y,
    } = target;
    assert!(width <= stride_px);
    assert!(pixels.len() >= height * stride_px * 4);
    let cell_i = cell as isize;
    for py in 0..height {
        let gy = py as isize - origin_y;
        if gy < 0 || gy >= (rows * cell) as isize {
            continue;
        }
        for px in 0..width {
            let gx = px as isize - origin_x;
            if gx < 0 || gx >= (cols * cell) as isize {
                continue;
            }
            let i = (gy / cell_i) as usize * cols + (gx / cell_i) as usize;
            let cov = atlas.glyph(frame.glyph[i]);
            let cv = u32::from(cov[(gy % cell_i) as usize * cell + (gx % cell_i) as usize]);
            let f = frame.fg[i].to_le_bytes();
            let b = frame.bg[i].to_le_bytes();
            let out = &mut pixels[(py * stride_px + px) * 4..][..4];
            for ch in 0..4 {
                let v = (u32::from(f[ch]) * cv + u32::from(b[ch]) * (255 - cv) + 127) / 255;
                out[ch] = v as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::cells::{Viewport, render_cells};
    use crate::render::palette::{Color, TEXT_BG, TEXT_FG};
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
        f.put_text(12, "keys", Color::rgb(1, 2, 3), Color::rgb(9, 8, 7));
        f
    }

    fn with_threads<T: Send>(n: usize, f: impl FnOnce() -> T + Send) -> T {
        rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .unwrap()
            .install(f)
    }

    fn target(
        buf: &mut [u8],
        stride: usize,
        w: usize,
        h: usize,
        ox: isize,
        oy: isize,
    ) -> Target<'_> {
        Target {
            pixels: buf,
            stride_px: stride,
            width: w,
            height: h,
            origin_x: ox,
            origin_y: oy,
        }
    }

    #[test]
    fn zig_blit_matches_reference_at_origin_and_leaves_padding() {
        let frame = sample_frame();
        let atlas = GlyphAtlas::build(9);
        let (stride, w, h) = (37 * 9 + 5, 37 * 9, 13 * 9);
        let mut want = vec![0xEEu8; stride * (h + 3) * 4];
        let mut got = want.clone();
        blit_reference(&frame, &atlas, target(&mut want, stride, w, h, 0, 0));
        blit(&frame, &atlas, target(&mut got, stride, w, h, 0, 0));
        assert!(got == want, "kernel output differs from reference");
        assert!(want[(w * 4)..(stride * 4)].iter().all(|&b| b == 0xEE));
        assert!(want[h * stride * 4..].iter().all(|&b| b == 0xEE));
    }

    #[test]
    fn zig_blit_matches_reference_scrolled_and_clipped() {
        let frame = sample_frame();
        let atlas = GlyphAtlas::build(9);
        // Window smaller than the grid, grid shifted up-left by a fraction of
        // a cell and also hanging off the right/bottom.
        for (ox, oy) in [(-4, -7), (-8, -1), (3, 5), (-400, -120)] {
            let (stride, w, h) = (300, 297, 100);
            let mut want = vec![0xEEu8; stride * h * 4];
            let mut got = want.clone();
            blit_reference(&frame, &atlas, target(&mut want, stride, w, h, ox, oy));
            blit(&frame, &atlas, target(&mut got, stride, w, h, ox, oy));
            assert!(got == want, "mismatch at origin ({ox},{oy})");
            if ox == -4 {
                // Fully covered window: no sentinel survives inside it.
                for py in 0..h {
                    assert!(want[py * stride * 4..][..w * 4].iter().all(|&b| b != 0xEE));
                }
            }
        }
    }

    #[test]
    fn same_pixels_on_one_and_many_threads() {
        let frame = sample_frame();
        let atlas = GlyphAtlas::build(7);
        let (stride, w, h) = (37 * 7, 37 * 7 - 3, 13 * 7 - 5);
        let mut a = vec![0u8; stride * h * 4];
        let mut b = a.clone();
        with_threads(1, || {
            blit(&frame, &atlas, target(&mut a, stride, w, h, -3, -2))
        });
        with_threads(8, || {
            blit(&frame, &atlas, target(&mut b, stride, w, h, -3, -2))
        });
        assert!(a == b);
    }

    #[test]
    fn empty_frame_or_window_is_a_no_op() {
        let atlas = GlyphAtlas::build(8);
        let mut out = vec![7u8; 64];
        blit(&CellFrame::new(), &atlas, target(&mut out, 4, 4, 4, 0, 0));
        assert!(out.iter().all(|&b| b == 7));
        let frame = sample_frame();
        blit(&frame, &atlas, target(&mut out, 4, 0, 0, 0, 0));
        assert!(out.iter().all(|&b| b == 7));
    }
}
