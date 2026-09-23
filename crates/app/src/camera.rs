//! Where the player is looking: a fractional cell position with velocity, so
//! held keys glide the view instead of snapping cell by cell.
//!
//! App state, not sim state: it is driven by wall-clock frame time and never
//! feeds back into the simulation, so nothing here affects determinism.

use std::fs;
use std::path::Path;

use bevy::prelude::Resource;
use sim_core::Pos;

/// Cruising speed, cells per second.
const SPEED: f64 = 14.0;
/// With Shift held.
const FAST_SPEED: f64 = 56.0;
/// Time constant of the velocity easing, seconds: ~63% of the way to the
/// target speed after this long, ~95% after three times it.
const SMOOTHING: f64 = 0.07;
/// Below this speed with no input the camera snaps to rest.
const REST: f64 = 0.02;
/// Keeps `cell()` inside `i32` with room for a viewport around it.
const LIMIT: f64 = 1e9;

/// Direction the player is pushing: each axis -1, 0 or 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Input {
    pub dx: i8,
    pub dy: i8,
    pub fast: bool,
}

#[derive(Resource, Debug, Clone, Copy, PartialEq)]
pub struct ViewCamera {
    /// Centre of the view, in cells; `(0.0, 0.0)` is the top-left corner of cell (0, 0).
    pub x: f64,
    pub y: f64,
    vx: f64,
    vy: f64,
}

impl ViewCamera {
    pub fn new(center: Pos) -> Self {
        Self {
            x: f64::from(center.x) + 0.5,
            y: f64::from(center.y) + 0.5,
            vx: 0.0,
            vy: 0.0,
        }
    }

    /// The cell under the centre of the view; the streaming focus.
    pub fn cell(&self) -> Pos {
        Pos::new(self.x.floor() as i32, self.y.floor() as i32)
    }

    pub fn moving(&self) -> bool {
        self.vx != 0.0 || self.vy != 0.0
    }

    /// Advance by `dt` seconds under `input`. Diagonals move at the same
    /// speed as axes; speed eases in and out over [`SMOOTHING`].
    pub fn update(&mut self, dt: f64, input: Input) {
        let (mut tx, mut ty) = (f64::from(input.dx), f64::from(input.dy));
        if tx != 0.0 && ty != 0.0 {
            tx *= std::f64::consts::FRAC_1_SQRT_2;
            ty *= std::f64::consts::FRAC_1_SQRT_2;
        }
        let speed = if input.fast { FAST_SPEED } else { SPEED };
        let k = 1.0 - (-dt / SMOOTHING).exp();
        self.vx += (tx * speed - self.vx) * k;
        self.vy += (ty * speed - self.vy) * k;
        if input.dx == 0 && input.dy == 0 && self.vx.hypot(self.vy) < REST {
            self.vx = 0.0;
            self.vy = 0.0;
        }
        self.x = (self.x + self.vx * dt).clamp(-LIMIT, LIMIT);
        self.y = (self.y + self.vy * dt).clamp(-LIMIT, LIMIT);
    }

    /// The camera is app state, not sim state, so it gets its own tiny file
    /// in the save directory rather than a slot in the world meta.
    pub fn load(dir: &Path) -> Option<Self> {
        let text = fs::read_to_string(dir.join("camera.txt")).ok()?;
        let mut it = text.split_whitespace().map(str::parse::<f64>);
        let x = it.next()?.ok()?;
        let y = it.next()?.ok()?;
        (x.is_finite() && y.is_finite()).then_some(Self {
            x,
            y,
            vx: 0.0,
            vy: 0.0,
        })
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        fs::write(dir.join("camera.txt"), format!("{} {}\n", self.x, self.y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cam: &mut ViewCamera, secs: f64, input: Input) {
        let dt = 1.0 / 120.0;
        let mut t = 0.0;
        while t < secs {
            cam.update(dt, input);
            t += dt;
        }
    }

    #[test]
    fn diagonal_is_as_fast_as_straight() {
        let mut a = ViewCamera::new(Pos::new(0, 0));
        let mut d = ViewCamera::new(Pos::new(0, 0));
        run(
            &mut a,
            2.0,
            Input {
                dx: 1,
                dy: 0,
                fast: false,
            },
        );
        run(
            &mut d,
            2.0,
            Input {
                dx: 1,
                dy: 1,
                fast: false,
            },
        );
        let straight = a.x - 0.5;
        let diag = ((d.x - 0.5).powi(2) + (d.y - 0.5).powi(2)).sqrt();
        assert!((straight - diag).abs() < 1e-6, "{straight} vs {diag}");
        // Cruising speed reached: 2 s minus the ease-in is close to 2*SPEED.
        assert!(straight > 1.9 * SPEED && straight < 2.0 * SPEED);
    }

    #[test]
    fn eases_in_and_comes_to_rest() {
        let mut c = ViewCamera::new(Pos::new(0, 0));
        c.update(
            1.0 / 120.0,
            Input {
                dx: 0,
                dy: -1,
                fast: false,
            },
        );
        assert!(c.moving());
        assert!(
            c.y < 0.5 && c.y > 0.5 - SPEED / 120.0,
            "first frame is eased, not full speed"
        );
        run(
            &mut c,
            0.5,
            Input {
                dx: 0,
                dy: -1,
                fast: false,
            },
        );
        run(&mut c, 1.0, Input::default());
        assert!(!c.moving());
        let y_rest = c.y;
        run(&mut c, 1.0, Input::default());
        assert_eq!(c.y, y_rest);
    }

    #[test]
    fn fast_is_faster_and_cell_floors() {
        let mut c = ViewCamera::new(Pos::new(-3, 7));
        assert_eq!(c.cell(), Pos::new(-3, 7));
        run(
            &mut c,
            1.0,
            Input {
                dx: -1,
                dy: 0,
                fast: true,
            },
        );
        assert!(c.x < -3.0 - 0.9 * FAST_SPEED);
        assert_eq!(c.cell(), Pos::new(c.x.floor() as i32, 7));
    }

    #[test]
    fn save_load_roundtrip_and_legacy_ints() {
        let dir = std::env::temp_dir().join(format!("wmc-cam-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut c = ViewCamera::new(Pos::new(1, 2));
        run(
            &mut c,
            0.3,
            Input {
                dx: 1,
                dy: 1,
                fast: false,
            },
        );
        c.save(&dir).unwrap();
        let back = ViewCamera::load(&dir).unwrap();
        assert_eq!((back.x, back.y), (c.x, c.y));
        assert!(!back.moving());
        fs::write(dir.join("camera.txt"), "10 -4\n").unwrap();
        assert_eq!(ViewCamera::load(&dir).unwrap().cell(), Pos::new(10, -4));
        fs::remove_dir_all(&dir).unwrap();
    }
}
