//! Phase 1 of a frame: the viewport as three flat per-cell buffers.
//!
//! Rows are the parallel unit: a band of [`ROW_BAND`] rows is one task on the
//! compute pool, writes only its own slices, reads only through the chunk
//! lookup, and does one lookup per (row, chunk) span, never per cell. No
//! reduction, so the result is a pure function of `(chunks, view, light)`
//! whatever the thread count. `light` (day/night) is applied here, per cell,
//! so the status bar and the GPU never see it.

use bevy::prelude::Resource;
use bevy::tasks::ComputeTaskPool;
use sim_core::{CHUNK_SIZE, ChunkCells, ChunkCoord, Pos};

use super::palette::{Color, VOID, actor_glyph, style};

/// Frame rows per task. 160 columns x 8 rows is ~10 KB of output per task;
/// small frames run on one task and skip the pool entirely.
pub const ROW_BAND: usize = 8;

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
#[derive(Resource, Debug, Default)]
pub struct CellFrame {
    cols: usize,
    rows: usize,
    /// Printable ASCII byte per cell.
    pub glyph: Vec<u8>,
    /// Packed [`Color`] per cell.
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
    pub fn put_text(&mut self, row: usize, text: &str, fg: Color, bg: Color) {
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

/// Fill the first `view.height` rows of `frame` from the chunks `chunk`
/// returns (`None` = not loaded, drawn as void), with every colour scaled by
/// `light` (255 = the palette as is, see [`super::palette::brightness`]).
/// `glyphs` is the kind table's glyph per kind (`Kinds::glyphs`). `frame`
/// must already be `view.width` columns wide and at least `view.height` rows
/// tall (extra rows are left for the caller: status text).
pub fn render_cells<'c>(
    chunk: impl Fn(ChunkCoord) -> Option<&'c ChunkCells> + Sync,
    glyphs: &[u8],
    view: Viewport,
    light: u8,
    frame: &mut CellFrame,
) {
    let cols = frame.cols;
    let rows = view.height as usize;
    assert_eq!(cols, view.width as usize, "frame width != viewport width");
    assert!(rows <= frame.rows, "viewport taller than frame");
    let n = cols * rows;
    if n == 0 {
        return;
    }
    let band = cols * ROW_BAND;
    let bands = frame.glyph[..n]
        .chunks_mut(band)
        .zip(frame.fg[..n].chunks_mut(band))
        .zip(frame.bg[..n].chunks_mut(band))
        .enumerate();
    let chunk = &chunk;
    let render_band = move |b: usize, glyph: &mut [u8], fg: &mut [u32], bg: &mut [u32]| {
        let rows = glyph
            .chunks_mut(cols)
            .zip(fg.chunks_mut(cols))
            .zip(bg.chunks_mut(cols));
        for (i, ((g, f), bg)) in rows.enumerate() {
            render_row(chunk, glyphs, view, light, b * ROW_BAND + i, g, f, bg);
        }
    };
    if rows <= ROW_BAND {
        for (b, ((g, f), bg)) in bands {
            render_band(b, g, f, bg);
        }
        return;
    }
    ComputeTaskPool::get().scope(|s| {
        for (b, ((g, f), bg)) in bands {
            s.spawn(async move { render_band(b, g, f, bg) });
        }
    });
}

fn render_row<'c>(
    chunk: &(impl Fn(ChunkCoord) -> Option<&'c ChunkCells> + Sync),
    glyphs: &[u8],
    view: Viewport,
    light: u8,
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
        match chunk(cc) {
            Some(c) => {
                let cells = c.ground[local..local + n]
                    .iter()
                    .zip(&c.feature[local..local + n])
                    .zip(&c.occupant[local..local + n]);
                let outs = g_out.iter_mut().zip(f_out.iter_mut().zip(b_out.iter_mut()));
                for (((&g, &f), &o), (go, (fo, bo))) in cells.zip(outs) {
                    let s = style(g, f, actor_glyph(o, glyphs));
                    *go = s.glyph;
                    *fo = s.fg.scaled(light).0;
                    *bo = s.bg.scaled(light).0;
                }
            }
            None => {
                g_out.fill(VOID.glyph);
                f_out.fill(VOID.fg.scaled(light).0);
                b_out.fill(VOID.bg.scaled(light).0);
            }
        }
        x = span_end;
        i += n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bevy::ecs::world::World;
    use sim_core::stage::worldgen::GenParams;
    use sim_core::{Kinds, StageCells, WorldConfig, sim, stage};

    fn world(w: u32, h: u32, seed: u64) -> World {
        sim::new_world(&WorldConfig {
            width: w,
            height: h,
            seed,
            params: GenParams::default(),
        })
    }

    fn frame_with(world: &World, view: Viewport, light: u8) -> CellFrame {
        let mut f = CellFrame::new();
        f.resize(view.width as usize, view.height as usize + 2);
        let glyphs = &world.resource::<Kinds>().glyphs;
        render_cells(|c| stage::chunk(world, c), glyphs, view, light, &mut f);
        f
    }

    #[test]
    fn parallel_bands_match_a_per_cell_walk_and_the_system_param_view() {
        let mut world = world(200, 150, 9);
        // Straddles chunk borders and unloaded space on every side; 97 rows
        // is many bands with a ragged last one.
        let view = Viewport::centered(Pos::new(30, 40), 173, 97);
        let a = frame_with(&world, view, 200);
        let glyphs = world.resource::<Kinds>().glyphs.clone();
        let n = 173 * 97;
        let mut actors = 0;
        for r in 0..97usize {
            for c in 0..173usize {
                let p = Pos::new(view.origin.x + c as i32, view.origin.y + r as i32);
                let (cc, i) = p.split();
                let want = match stage::chunk(&world, cc) {
                    Some(ch) => {
                        actors += usize::from(!ch.occupant[i].is_none());
                        style(
                            ch.ground[i],
                            ch.feature[i],
                            actor_glyph(ch.occupant[i], &glyphs),
                        )
                    }
                    None => VOID,
                };
                let k = r * 173 + c;
                assert_eq!(a.glyph[k], want.glyph, "{p:?}");
                assert_eq!(a.fg[k], want.fg.scaled(200).0, "{p:?}");
                assert_eq!(a.bg[k], want.bg.scaled(200).0, "{p:?}");
            }
        }
        assert!(actors > 0, "worldgen seeds are drawn");
        assert_eq!(
            a.glyph[..n].iter().filter(|g| glyphs.contains(g)).count(),
            actors,
            "every actor draws its kind's glyph"
        );
        // Extra rows untouched.
        assert!(a.glyph[n..].iter().all(|&g| g == b' '));
        // The same picture through the read-only system param.
        let b = world
            .run_system_once(move |s: StageCells, k: Res<Kinds>| {
                let mut f = CellFrame::new();
                f.resize(173, 99);
                render_cells(|c| s.chunk(c), &k.glyphs, view, 200, &mut f);
                f
            })
            .unwrap();
        assert_eq!(a.glyph, b.glyph);
        assert_eq!(a.fg, b.fg);
        assert_eq!(a.bg, b.bg);
    }

    use bevy::ecs::system::{Res, RunSystemOnce};

    #[test]
    fn light_scales_every_colour_and_nothing_else() {
        let world = world(100, 100, 4);
        let view = Viewport::centered(Pos::new(90, 90), 40, 30); // includes unloaded cells
        let day = frame_with(&world, view, 255);
        let dusk = frame_with(&world, view, 128);
        let night = frame_with(&world, view, 0);
        assert_eq!(day.glyph, dusk.glyph);
        assert_eq!(day.glyph, night.glyph);
        let n = 40 * 30;
        assert!(night.fg[..n].iter().all(|&c| c == 0));
        assert!(night.bg[..n].iter().all(|&c| c == 0));
        for (&d, &k) in day.bg[..n].iter().zip(&dusk.bg[..n]) {
            assert_eq!(Color(d).scaled(128), Color(k));
        }
        assert_ne!(day.bg, dusk.bg);
    }

    #[test]
    fn put_text_pads_clips_and_ignores_bad_rows() {
        let mut f = CellFrame::new();
        f.resize(4, 2);
        f.put_text(1, "ab\u{e9}cdef", Color(1), Color(2));
        assert_eq!(&f.glyph[4..], b"ab??");
        assert_eq!(&f.fg[4..], &[1, 1, 1, 1]);
        f.put_text(0, "x", Color(3), Color(4));
        assert_eq!(&f.glyph[..4], b"x   ");
        f.put_text(7, "nope", Color(0), Color(0));
        assert_eq!(&f.glyph[..4], b"x   ");
    }

    #[test]
    fn empty_and_tiny_viewports() {
        let world = world(64, 64, 1);
        let f = frame_with(&world, Viewport::centered(Pos::new(0, 0), 0, 0), 255);
        assert!(f.glyph.is_empty());
        let f = frame_with(&world, Viewport::centered(Pos::new(5, 5), 1, 1), 255);
        assert_eq!(f.glyph.len(), 3);
        assert_ne!(f.glyph[0], VOID.glyph);
    }

    #[test]
    fn centered_viewport() {
        let v = Viewport::centered(Pos::new(10, 10), 7, 3);
        assert_eq!(v.origin, Pos::new(7, 9));
    }
}
