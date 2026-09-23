//! Text view of a [`CellFrame`]: one glyph per cell, one line per row.
//! Used by `wmc show` and by tests; the window never goes through here.

use sim_core::{ChunkCells, ChunkCoord};

use super::cells::{CellFrame, Viewport, render_cells};

/// Render `view` as text, rows separated by `\n`.
pub fn render<'c>(
    chunk: impl Fn(ChunkCoord) -> Option<&'c ChunkCells> + Sync,
    view: Viewport,
) -> String {
    let mut frame = CellFrame::new();
    frame.resize(view.width as usize, view.height as usize);
    render_cells(chunk, view, 255, &mut frame);
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
    use bevy::ecs::world::World;
    use sim_core::{ActorId, CHUNK_SIZE, ChunkCells, Feature, Ground, Pos, Stage, stage};

    #[test]
    fn renders_every_glyph_and_blank_for_unloaded() {
        sim_core::par::init_task_pool();
        let mut w = World::new();
        w.init_resource::<Stage>();
        let mut c = ChunkCells::default();
        c.ground[1] = Ground::Water;
        c.feature[2] = Feature::Rock;
        c.ground[3] = Ground::Water;
        c.feature[3] = Feature::Rock;
        c.occupant[CHUNK_SIZE as usize] = ActorId(0);
        stage::insert(&mut w, ChunkCoord::new(0, 0), c, false, 0);
        let look = |cc| stage::chunk(&w, cc);
        let v = Viewport {
            origin: Pos::new(0, 0),
            width: 4,
            height: 2,
        };
        assert_eq!(render(look, v), ".~##\n@...\n");
        // Straddles the loaded chunk and an unloaded one on the left.
        let v = Viewport {
            origin: Pos::new(-2, 0),
            width: 6,
            height: 1,
        };
        assert_eq!(render(look, v), "  .~##\n");
        // Crosses a chunk boundary on the right into nothing.
        let v = Viewport {
            origin: Pos::new(62, 1),
            width: 4,
            height: 1,
        };
        assert_eq!(render(look, v), "..  \n");
    }
}
