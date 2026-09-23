//! Where the player is looking. Drives both rendering and chunk streaming.

use sim_core::Pos;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Camera {
    /// Cell at the centre of the view.
    pub center: Pos,
}

impl Camera {
    pub fn new(center: Pos) -> Self {
        Self { center }
    }

    pub fn pan(&mut self, dx: i32, dy: i32) {
        self.center = Pos::new(
            self.center.x.saturating_add(dx),
            self.center.y.saturating_add(dy),
        );
    }

    /// The camera is app state, not sim state, so it gets its own tiny file
    /// in the save directory rather than a slot in the world meta.
    pub fn load(dir: &Path) -> Option<Self> {
        let text = fs::read_to_string(dir.join("camera.txt")).ok()?;
        let mut it = text.split_whitespace().map(str::parse::<i32>);
        let x = it.next()?.ok()?;
        let y = it.next()?.ok()?;
        Some(Self::new(Pos::new(x, y)))
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        fs::write(
            dir.join("camera.txt"),
            format!("{} {}\n", self.center.x, self.center.y),
        )
    }
}
