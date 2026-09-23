//! ASCII renderer: one glyph per cell, one line per row.
//!
//! Glyphs: `.` soil, `~` water, `#` rock (on anything), `@` any actor,
//! space for cells whose chunk is not loaded.

use sim_core::{CHUNK_SIZE, Feature, Ground, Pos, Stage};

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

pub fn glyph(ground: Ground, feature: Feature, occupied: bool) -> char {
    if occupied {
        return '@';
    }
    match (feature, ground) {
        (Feature::Rock, _) => '#',
        (Feature::None, Ground::Soil) => '.',
        (Feature::None, Ground::Water) => '~',
    }
}

/// Render `view` into `out` (cleared first), rows separated by `newline`.
/// Reuse `out` across frames to avoid allocating. Works span by span: one
/// chunk lookup per (row, chunk) pair, never per cell.
pub fn render_into(stage: &Stage, view: Viewport, newline: &str, out: &mut String) {
    out.clear();
    let w = i32::try_from(view.width).expect("viewport width fits i32");
    let h = i32::try_from(view.height).expect("viewport height fits i32");
    out.reserve((view.width as usize + newline.len()) * view.height as usize);
    for row in 0..h {
        let y = view.origin.y.saturating_add(row);
        let x_end = view.origin.x.saturating_add(w);
        let mut x = view.origin.x;
        while x < x_end {
            let (cc, local) = Pos::new(x, y).split();
            let chunk_end = cc.origin().x + CHUNK_SIZE;
            let span_end = chunk_end.min(x_end);
            let n = (span_end - x) as usize;
            match stage.chunk(cc) {
                Some(c) => {
                    let cells = c.ground[local..local + n]
                        .iter()
                        .zip(&c.feature[local..local + n])
                        .zip(&c.occupant[local..local + n]);
                    for ((&g, &f), o) in cells {
                        out.push(glyph(g, f, !o.is_none()));
                    }
                }
                None => out.extend(std::iter::repeat_n(' ', n)),
            }
            x = span_end;
        }
        out.push_str(newline);
    }
}

pub fn render(stage: &Stage, view: Viewport) -> String {
    let mut s = String::new();
    render_into(stage, view, "\n", &mut s);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::{ActorId, ChunkCells, ChunkCoord};

    #[test]
    fn renders_every_glyph_and_blank_for_unloaded() {
        let mut s = Stage::new();
        let mut c = ChunkCells::default();
        c.ground[1] = Ground::Water;
        c.feature[2] = Feature::Rock;
        c.ground[3] = Ground::Water;
        c.feature[3] = Feature::Rock;
        c.occupant[CHUNK_SIZE as usize] = ActorId(0);
        s.insert(ChunkCoord::new(0, 0), c, false);
        let v = Viewport {
            origin: Pos::new(0, 0),
            width: 4,
            height: 2,
        };
        assert_eq!(render(&s, v), ".~##\n@...\n");
        // Straddles the loaded chunk and an unloaded one on the left.
        let v = Viewport {
            origin: Pos::new(-2, 0),
            width: 6,
            height: 1,
        };
        assert_eq!(render(&s, v), "  .~##\n");
        // Crosses a chunk boundary on the right into nothing.
        let v = Viewport {
            origin: Pos::new(62, 1),
            width: 4,
            height: 1,
        };
        assert_eq!(render(&s, v), "..  \n");
    }

    #[test]
    fn centered_viewport() {
        let v = Viewport::centered(Pos::new(10, 10), 7, 3);
        assert_eq!(v.origin, Pos::new(7, 9));
    }
}
