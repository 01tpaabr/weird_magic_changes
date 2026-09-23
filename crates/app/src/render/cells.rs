//! Phase 1 of a frame: the viewport as three flat per-cell buffers.
//!
//! Rows are the parallel unit: row `r` of the frame is written by exactly one
//! task, reads only `&Stage`, and does one chunk lookup per (row, chunk) span,
//! never per cell. No reduction, so the result is a pure function of
//! `(stage, view)` whatever the thread count.

use rayon::prelude::*;
use sim_core::{CHUNK_SIZE, Pos, Stage};

use super::palette::{Rgba, VOID, style};

/// Rectangle of the world to draw, in cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    /// Top-left cell.
    pub origin: Pos,
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    /// A viewport of `width x height` centred on `center`.
    pub fn centered(center: Pos, width: u32, height: u32) -> Self {
        let half = |n: u32| i32::try_from(n / 2).expect("viewport fits i32");
        Self {
            origin: Pos::new(
                center.x.saturating_sub(half(width)),
                center.y.saturating_sub(half(height)),
            ),
            width,
            height,
        }
    }
}

/// `cols x rows` cells, structure of arrays, row-major. Reused across frames;
/// only a size change reallocates.
#[derive(Debug, Default)]
pub struct CellFrame {
    cols: usize,
    rows: usize,
    /// Printable ASCII byte per cell.
    pub glyph: Vec<u8>,
    /// Packed [`Rgba`] per cell.
    pub fg: Vec<u32>,
    pub bg: Vec<u32>,
}

impl CellFrame {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let n = cols * rows;
        self.cols = cols;
        self.rows = rows;
        self.glyph.resize(n, VOID.glyph);
        self.fg.resize(n, VOID.fg.0);
        self.bg.resize(n, VOID.bg.0);
    }

    /// Write `text` into row `row` starting at column 0, padding the rest of
    /// the row with spaces. Rows outside the frame are ignored, text is
    /// clipped, non-ASCII bytes draw as `?`.
    pub fn put_text(&mut self, row: usize, text: &str, fg: Rgba, bg: Rgba) {
        if row >= self.rows {
            return;
        }
        let range = row * self.cols..(row + 1) * self.cols;
        let mut src = text.bytes().map(|b| {
            if b.is_ascii_graphic() || b == b' ' {
                b
            } else {
                b'?'
            }
        });
        for g in &mut self.glyph[range.clone()] {
            *g = src.next().unwrap_or(b' ');
        }
        self.fg[range.clone()].fill(fg.0);
        self.bg[range].fill(bg.0);
    }
}

/// Fill the first `view.height` rows of `frame` from `stage`.
/// `frame` must already be `view.width` columns wide and at least
/// `view.height` rows tall (extra rows are left for the caller: status text).
pub fn render_cells(stage: &Stage, view: Viewport, frame: &mut CellFrame) {
    let cols = frame.cols;
    let rows = view.height as usize;
    assert_eq!(cols, view.width as usize, "frame width != viewport width");
    assert!(rows <= frame.rows, "viewport taller than frame");
    let n = cols * rows;
    frame.glyph[..n]
        .par_chunks_mut(cols)
        .zip(frame.fg[..n].par_chunks_mut(cols))
        .zip(frame.bg[..n].par_chunks_mut(cols))
        .enumerate()
        .for_each(|(row, ((glyph, fg), bg))| render_row(stage, view, row, glyph, fg, bg));
}

fn render_row(
    stage: &Stage,
    view: Viewport,
    row: usize,
    glyph: &mut [u8],
    fg: &mut [u32],
    bg: &mut [u32],
) {
    let w = i32::try_from(glyph.len()).expect("row width fits i32");
    let y = view
        .origin
        .y
        .saturating_add(i32::try_from(row).expect("row fits i32"));
    let x_end = view.origin.x.saturating_add(w);
    let mut x = view.origin.x;
    let mut i = 0usize;
    while x < x_end {
        let (cc, local) = Pos::new(x, y).split();
        let span_end = (cc.origin().x + CHUNK_SIZE).min(x_end);
        let n = (span_end - x) as usize;
        let (g_out, f_out, b_out) = (&mut glyph[i..i + n], &mut fg[i..i + n], &mut bg[i..i + n]);
        match stage.chunk(cc) {
            Some(c) => {
                let cells = c.ground[local..local + n]
                    .iter()
                    .zip(&c.feature[local..local + n])
                    .zip(&c.occupant[local..local + n]);
                let outs = g_out.iter_mut().zip(f_out.iter_mut().zip(b_out.iter_mut()));
                for (((&g, &f), o), (go, (fo, bo))) in cells.zip(outs) {
                    let s = style(g, f, !o.is_none());
                    *go = s.glyph;
                    *fo = s.fg.0;
                    *bo = s.bg.0;
                }
            }
            None => {
                g_out.fill(VOID.glyph);
                f_out.fill(VOID.fg.0);
                b_out.fill(VOID.bg.0);
            }
        }
        x = span_end;
        i += n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::stage::worldgen::GenParams;
    use sim_core::{World, WorldConfig};

    fn frame_with(threads: usize, view: Viewport, world: &World) -> CellFrame {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap()
            .install(|| {
                let mut f = CellFrame::new();
                f.resize(view.width as usize, view.height as usize + 2);
                render_cells(&world.stage, view, &mut f);
                f
            })
    }

    #[test]
    fn same_cells_on_one_and_many_threads() {
        let world = World::new(&WorldConfig {
            width: 200,
            height: 150,
            seed: 9,
            params: GenParams::default(),
        });
        // Straddles chunk borders and unloaded space on every side.
        let view = Viewport::centered(Pos::new(30, 40), 173, 97);
        let a = frame_with(1, view, &world);
        let b = frame_with(8, view, &world);
        assert_eq!(a.glyph, b.glyph);
        assert_eq!(a.fg, b.fg);
        assert_eq!(a.bg, b.bg);
        // Extra rows untouched.
        let n = 173 * 97;
        assert!(a.glyph[n..].iter().all(|&g| g == b' '));
    }

    #[test]
    fn put_text_pads_clips_and_ignores_bad_rows() {
        let mut f = CellFrame::new();
        f.resize(4, 2);
        f.put_text(1, "ab\u{e9}cdef", Rgba(1), Rgba(2));
        assert_eq!(&f.glyph[4..], b"ab??");
        assert_eq!(&f.fg[4..], &[1, 1, 1, 1]);
        f.put_text(0, "x", Rgba(3), Rgba(4));
        assert_eq!(&f.glyph[..4], b"x   ");
        f.put_text(7, "nope", Rgba(0), Rgba(0));
        assert_eq!(&f.glyph[..4], b"x   ");
    }

    #[test]
    fn centered_viewport() {
        let v = Viewport::centered(Pos::new(10, 10), 7, 3);
        assert_eq!(v.origin, Pos::new(7, 9));
    }
}
