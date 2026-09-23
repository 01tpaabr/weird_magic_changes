//! ASCII renderer: one glyph per cell, one line per row.
//!
//! Glyphs: `.` soil, `~` water, `#` rock (on anything), `@` any actor.

use std::fmt::Write;

use sim_core::{Feature, Ground, Pos, Stage};

/// Rectangle of the stage to draw, in cells. Clipped to the stage.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub origin: Pos,
    pub width: u32,
    pub height: u32,
}

impl Viewport {
    /// The whole stage.
    pub fn full(stage: &Stage) -> Self {
        Self {
            origin: Pos::new(0, 0),
            width: stage.width(),
            height: stage.height(),
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

/// Render `view` into `out` (cleared first). Reuse `out` across frames to
/// avoid allocating.
pub fn render_into(stage: &Stage, view: Viewport, out: &mut String) {
    out.clear();
    let x0 = view.origin.x.min(stage.width());
    let y0 = view.origin.y.min(stage.height());
    let x1 = view.origin.x.saturating_add(view.width).min(stage.width());
    let y1 = view
        .origin
        .y
        .saturating_add(view.height)
        .min(stage.height());
    out.reserve((x1 - x0) as usize * (y1 - y0) as usize + (y1 - y0) as usize);
    for y in y0..y1 {
        let row = stage.idx(Pos::new(x0, y)).usize()..stage.idx(Pos::new(x1 - 1, y)).usize() + 1;
        let cells = stage.ground[row.clone()]
            .iter()
            .zip(&stage.feature[row.clone()])
            .zip(&stage.occupant[row]);
        for ((&g, &f), o) in cells {
            out.push(glyph(g, f, !o.is_none()));
        }
        let _ = writeln!(out);
    }
}

pub fn render(stage: &Stage, view: Viewport) -> String {
    let mut s = String::new();
    render_into(stage, view, &mut s);
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::{ActorId, CellIdx};

    #[test]
    fn renders_every_glyph_and_clips() {
        let mut s = Stage::new(4, 2);
        s.ground[1] = Ground::Water;
        s.feature[2] = Feature::Rock;
        s.ground[3] = Ground::Water;
        s.feature[3] = Feature::Rock;
        s.occupant[4] = ActorId(0);
        assert_eq!(render(&s, Viewport::full(&s)), ".~##\n@...\n");
        let v = Viewport {
            origin: Pos::new(2, 1),
            width: 10,
            height: 10,
        };
        assert_eq!(render(&s, v), "..\n");
        assert!(s.walkable(CellIdx(0)));
    }
}
