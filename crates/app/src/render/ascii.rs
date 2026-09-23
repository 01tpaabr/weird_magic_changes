//! Text view of a [`CellFrame`]: one glyph per cell, one line per row.
//! Used by `wmc show` and by tests; the window never goes through here.

use sim_core::Stage;

use super::cells::{CellFrame, Viewport, render_cells};

/// Render `view` as text, rows separated by `\n`.
pub fn render(stage: &Stage, view: Viewport) -> String {
    let mut frame = CellFrame::new();
    frame.resize(view.width as usize, view.height as usize);
    render_cells(stage, view, &mut frame);
    let mut out = String::with_capacity((frame.cols() + 1) * frame.rows());
    for row in frame.glyph.chunks_exact(frame.cols().max(1)) {
        out.extend(row.iter().map(|&b| char::from(b)));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::{ActorId, CHUNK_SIZE, ChunkCells, ChunkCoord, Feature, Ground, Pos};

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
}
