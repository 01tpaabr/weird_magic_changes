//! Safe wrappers over the Zig kernels in `zig/src/root.zig`.
//!
//! Every `extern "C"` declaration here must match an `export fn` in Zig
//! **exactly** (name, argument order, types). The ABI version check in
//! [`check_abi`] catches stale builds but not signature drift, so when you change
//! a signature, change both sides in the same commit and add a test.
//!
//! Layering rule: this crate is the *only* place in the workspace allowed to
//! write `unsafe extern "C"`. Everything above it sees plain safe Rust.

/// ABI version this crate was written against. Must equal `abi_version` in `zig/src/root.zig`.
pub const ABI_VERSION: u32 = 2;

mod ffi {
    unsafe extern "C" {
        pub fn wmc_abi_version() -> u32;
        pub fn wmc_saxpy_f32(a: f32, x: *const f32, y: *mut f32, n: usize);
        pub fn wmc_sum_f32(x: *const f32, n: usize) -> f32;
        pub fn wmc_atlas_glyphs() -> usize;
        pub fn wmc_blit_cells(
            glyph: *const u8,
            fg: *const u32,
            bg: *const u32,
            cols: usize,
            rows: usize,
            atlas: *const u8,
            cell: usize,
            out: *mut u8,
            stride_px: usize,
        );
    }
}

/// Glyph boxes per atlas: printable ASCII `' '..='~'`. Checked against the Zig
/// side in a test; [`blit_cells`] asserts the atlas length against it.
pub const ATLAS_GLYPHS: usize = 95;

/// Returns `Err` with the Zig-side version if the linked library does not match [`ABI_VERSION`].
pub fn check_abi() -> Result<(), u32> {
    // SAFETY: no arguments, no side effects.
    let v = unsafe { ffi::wmc_abi_version() };
    if v == ABI_VERSION { Ok(()) } else { Err(v) }
}

/// `y[i] = a * x[i] + y[i]`. Panics if lengths differ.
#[inline]
pub fn saxpy(a: f32, x: &[f32], y: &mut [f32]) {
    assert_eq!(x.len(), y.len(), "saxpy: length mismatch");
    // SAFETY: both slices are valid for `len` elements and `y` is uniquely borrowed.
    unsafe { ffi::wmc_saxpy_f32(a, x.as_ptr(), y.as_mut_ptr(), x.len()) }
}

/// Deterministic sum of `x`.
#[inline]
pub fn sum(x: &[f32]) -> f32 {
    // SAFETY: slice is valid for `len` elements; the kernel only reads.
    unsafe { ffi::wmc_sum_f32(x.as_ptr(), x.len()) }
}

/// Glyph boxes per atlas as the linked kernel expects them (see [`ATLAS_GLYPHS`]).
pub fn atlas_glyphs() -> usize {
    // SAFETY: no arguments, no side effects.
    unsafe { ffi::wmc_atlas_glyphs() }
}

/// A row-major grid of cells for [`blit_cells`]: printable ASCII glyph bytes
/// (others draw `?`) and colours whose little-endian bytes are the pixel's
/// four bytes (channel order is the caller's choice).
#[derive(Debug, Clone, Copy)]
pub struct CellGrid<'a> {
    pub glyph: &'a [u8],
    pub fg: &'a [u32],
    pub bg: &'a [u32],
    pub cols: usize,
    pub rows: usize,
}

/// [`ATLAS_GLYPHS`] boxes of `cell*cell` coverage bytes (0 = background, 255 = glyph).
#[derive(Debug, Clone, Copy)]
pub struct Atlas<'a> {
    pub coverage: &'a [u8],
    pub cell: usize,
}

/// Paint `cells` into a 4-bytes-per-pixel buffer.
///
/// `out` holds `rows*cell` pixel rows of `stride_px` pixels; only the first
/// `cols*cell` pixels of each row are written. Output is a pure function of
/// the inputs. Panics on any size mismatch.
pub fn blit_cells(cells: CellGrid, atlas: Atlas, out: &mut [u8], stride_px: usize) {
    let CellGrid {
        glyph,
        fg,
        bg,
        cols,
        rows,
    } = cells;
    let Atlas { coverage, cell } = atlas;
    let n = cols * rows;
    assert_eq!(glyph.len(), n, "blit: glyph length");
    assert_eq!(fg.len(), n, "blit: fg length");
    assert_eq!(bg.len(), n, "blit: bg length");
    assert_eq!(
        coverage.len(),
        ATLAS_GLYPHS * cell * cell,
        "blit: atlas size"
    );
    assert!(cols * cell <= stride_px, "blit: cells wider than stride");
    assert!(
        out.len() >= rows * cell * stride_px * 4,
        "blit: output buffer too small"
    );
    if n == 0 || cell == 0 {
        return;
    }
    // SAFETY: every length the kernel derives from (cols, rows, cell, stride_px)
    // was asserted above against the slices it will index; `out` is uniquely
    // borrowed and does not overlap the read-only inputs; the kernel retains
    // no pointers.
    unsafe {
        ffi::wmc_blit_cells(
            glyph.as_ptr(),
            fg.as_ptr(),
            bg.as_ptr(),
            cols,
            rows,
            coverage.as_ptr(),
            cell,
            out.as_mut_ptr(),
            stride_px,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn abi_matches() {
        check_abi().expect("Zig ABI version mismatch: rebuild or bump ABI_VERSION on both sides");
    }

    #[test]
    fn saxpy_matches_reference() {
        let n = 37; // non-multiple of any SIMD width
        let x: Vec<f32> = (0..n).map(|i| i as f32).collect();
        let mut y = vec![1.5f32; n];
        let mut expected = y.clone();
        saxpy(2.0, &x, &mut y);
        for (e, xi) in expected.iter_mut().zip(&x) {
            *e += 2.0 * xi;
        }
        assert_eq!(y, expected);
    }

    #[test]
    fn sum_is_deterministic() {
        let x: Vec<f32> = (0..1000).map(|i| (i as f32).sin()).collect();
        assert_eq!(sum(&x).to_bits(), sum(&x).to_bits());
        assert!((sum(&x) - x.iter().sum::<f32>()).abs() < 1e-2);
    }

    #[test]
    fn atlas_glyph_count_matches_zig() {
        assert_eq!(atlas_glyphs(), ATLAS_GLYPHS);
    }

    #[test]
    fn blit_matches_reference() {
        let cell = 3;
        let (cols, rows, stride) = (4, 2, 4 * 3 + 2);
        // Atlas: glyph i has coverage (i*7 + x + y*3) % 256, mixing 0/255/partials.
        let atlas: Vec<u8> = (0..ATLAS_GLYPHS * cell * cell)
            .map(|k| ((k * 7 + 13) % 256) as u8)
            .collect();
        let glyph = *b"a.~\x00#@?z";
        let fg: Vec<u32> = (0..8).map(|i| 0xff00_0000 | (i * 0x0102_0304)).collect();
        let bg: Vec<u32> = (0..8)
            .map(|i| 0xff00_0000 | (0x00ff_ffff - i * 0x0003_0303))
            .collect();
        let mut out = vec![0xEEu8; rows * cell * stride * 4];
        let cells = CellGrid {
            glyph: &glyph,
            fg: &fg,
            bg: &bg,
            cols,
            rows,
        };
        let atlas_ref = Atlas {
            coverage: &atlas,
            cell,
        };
        blit_cells(cells, atlas_ref, &mut out, stride);
        for py in 0..rows * cell {
            for px in 0..stride {
                let got = &out[(py * stride + px) * 4..][..4];
                if px >= cols * cell {
                    assert_eq!(got, [0xEE; 4], "padding touched at ({px},{py})");
                    continue;
                }
                let i = (py / cell) * cols + px / cell;
                let g = glyph[i];
                let g = if !(b' '..=b'~').contains(&g) { b'?' } else { g };
                let cov = u32::from(
                    atlas[usize::from(g - b' ') * cell * cell + (py % cell) * cell + px % cell],
                );
                let f = fg[i].to_le_bytes();
                let b = bg[i].to_le_bytes();
                let want: Vec<u8> = (0..4)
                    .map(|c| {
                        ((u32::from(f[c]) * cov + u32::from(b[c]) * (255 - cov) + 127) / 255) as u8
                    })
                    .collect();
                assert_eq!(got, &want[..], "pixel ({px},{py})");
            }
        }
    }

    #[test]
    fn empty_slices() {
        assert_eq!(sum(&[]), 0.0);
        saxpy(1.0, &[], &mut []);
    }
}
