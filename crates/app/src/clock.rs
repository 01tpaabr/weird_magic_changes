//! The sim driver: turns real elapsed time into a number of ticks to run.
//!
//! Speed is a presentation concern. The sim only ever sees `World::step`, so
//! it is bit-identical at every speed and on every machine; what changes here
//! is how many steps happen per real second. Fixed-timestep accumulator:
//! ticks owed grow by `dt * tps` per frame, whole ticks run, the fraction
//! carries. A frame stops ticking when its time budget is spent and the
//! remaining debt is **dropped**, so a machine that cannot keep up runs slow
//! instead of spiralling. `Speed::Max` ticks until the budget is spent.
//!
//! Wall-clock time enters here and nowhere below.

use std::time::{Duration, Instant};

use bevy::prelude::Resource;

/// Ticks per real second at 1x. With `sim_core::time::TICKS_PER_DAY` this
/// makes a 45 minute day. Change it and nothing in the sim or its saves changes.
pub const BASE_TPS: u32 = 8;
/// Speed steps `[` and `]` walk through, then [`Speed::Max`].
const MULTIPLIERS: [u32; 5] = [1, 2, 4, 8, 16];
/// Longest real interval one frame may account for. After a stall (alt-tab,
/// a debugger) the sim resumes from where it was instead of bursting to catch up.
const MAX_DT: f64 = 0.25;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    /// `BASE_TPS * multiplier` ticks per second.
    Fixed(u32),
    /// As many ticks as the frame budget allows.
    Max,
}

#[derive(Resource, Debug, Clone, PartialEq)]
pub struct SimClock {
    paused: bool,
    /// Index into [`MULTIPLIERS`]; `MULTIPLIERS.len()` means [`Speed::Max`].
    level: usize,
    /// Fractional tick carried between frames, in `[0, 1)`.
    owed: f64,
}

impl Default for SimClock {
    /// Running at 1x.
    fn default() -> Self {
        Self {
            paused: false,
            level: 0,
            owed: 0.0,
        }
    }
}

impl SimClock {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn paused(&self) -> bool {
        self.paused
    }

    pub fn running(&self) -> bool {
        !self.paused
    }

    pub fn speed(&self) -> Speed {
        MULTIPLIERS
            .get(self.level)
            .map_or(Speed::Max, |&m| Speed::Fixed(m))
    }

    pub fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.owed = 0.0;
    }

    pub fn pause(&mut self) {
        self.paused = true;
        self.owed = 0.0;
    }

    pub fn faster(&mut self) {
        self.level = (self.level + 1).min(MULTIPLIERS.len());
    }

    pub fn slower(&mut self) {
        self.level = self.level.saturating_sub(1);
    }

    /// For the status line: `paused`, `1x` .. `16x`, `max`.
    pub fn label(&self) -> String {
        match (self.paused, self.speed()) {
            (true, _) => "paused".to_string(),
            (false, Speed::Fixed(m)) => format!("{m}x"),
            (false, Speed::Max) => "max".to_string(),
        }
    }

    /// Run the ticks owed for a frame `dt` seconds long, stopping early once
    /// `budget` has elapsed (at least one tick runs if any is owed). Returns
    /// how many ran.
    pub fn run(&mut self, dt: f64, budget: Duration, mut tick: impl FnMut()) -> u32 {
        let planned = match (self.paused, self.speed()) {
            (true, _) => 0,
            (false, Speed::Fixed(m)) => {
                self.owed += dt.clamp(0.0, MAX_DT) * f64::from(BASE_TPS * m);
                self.owed.floor() as u32
            }
            (false, Speed::Max) => u32::MAX,
        };
        let start = Instant::now();
        let mut ran = 0;
        while ran < planned {
            tick();
            ran += 1;
            if start.elapsed() >= budget {
                break;
            }
        }
        // Whole ticks are either run or dropped; only the fraction carries.
        self.owed = self.owed.fract();
        ran
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_LIMIT: Duration = Duration::from_secs(3600);

    fn total(clock: &mut SimClock, frames: u32, dt: f64, budget: Duration) -> u32 {
        (0..frames).map(|_| clock.run(dt, budget, || {})).sum()
    }

    #[test]
    fn one_second_at_1x_is_base_tps_ticks() {
        let mut c = SimClock::new();
        assert_eq!(total(&mut c, 8, 0.125, NO_LIMIT), BASE_TPS);
        // Frames shorter than a tick still add up (1/16 s is exact in binary).
        assert_eq!(total(&mut c, 16, 1.0 / 16.0, NO_LIMIT), BASE_TPS);
        c.faster();
        c.faster();
        assert_eq!(c.speed(), Speed::Fixed(4));
        assert_eq!(total(&mut c, 16, 1.0 / 16.0, NO_LIMIT), 4 * BASE_TPS);
    }

    #[test]
    fn fraction_carries_between_frames() {
        let mut c = SimClock::new();
        // 1/8 s at 1x is exactly one tick; 3/32 s is 0.75 of one.
        assert_eq!(c.run(3.0 / 32.0, NO_LIMIT, || {}), 0);
        assert_eq!(c.run(3.0 / 32.0, NO_LIMIT, || {}), 1);
        assert_eq!(c.run(3.0 / 32.0, NO_LIMIT, || {}), 1);
        assert_eq!(c.run(3.0 / 32.0, NO_LIMIT, || {}), 1);
        assert_eq!(c.run(0.0, NO_LIMIT, || {}), 0);
    }

    #[test]
    fn debt_beyond_the_budget_is_dropped() {
        let mut c = SimClock::new();
        // A whole second owed but no budget: one tick runs, the rest is forgotten.
        assert_eq!(c.run(1.0, Duration::ZERO, || {}), 1);
        assert_eq!(c.run(0.0, NO_LIMIT, || {}), 0);
        assert_eq!(c.owed, 0.0);
    }

    #[test]
    fn a_stall_is_clamped() {
        let mut c = SimClock::new();
        assert_eq!(
            c.run(10.0, NO_LIMIT, || {}),
            (MAX_DT * f64::from(BASE_TPS)) as u32
        );
    }

    #[test]
    fn max_speed_ticks_until_the_budget_is_spent() {
        let mut c = SimClock::new();
        for _ in 0..MULTIPLIERS.len() {
            c.faster();
        }
        assert_eq!(c.speed(), Speed::Max);
        c.faster();
        assert_eq!(c.speed(), Speed::Max);
        assert_eq!(c.run(0.0, Duration::ZERO, || {}), 1);
        let mut n = 0;
        c.run(0.0, Duration::from_millis(2), || n += 1);
        assert!(n > 1);
        c.slower();
        assert_eq!(c.speed(), Speed::Fixed(16));
    }

    #[test]
    fn paused_runs_nothing_and_forgets_debt() {
        let mut c = SimClock::new();
        c.run(3.0 / 32.0, NO_LIMIT, || {});
        c.toggle_pause();
        assert!(c.paused());
        assert_eq!(c.run(5.0, NO_LIMIT, || {}), 0);
        c.toggle_pause();
        assert_eq!(c.run(0.0, NO_LIMIT, || {}), 0);
        assert_eq!(c.run(0.125, NO_LIMIT, || {}), 1);
        c.pause();
        assert!(c.paused());
    }

    #[test]
    fn labels() {
        let mut c = SimClock::new();
        assert_eq!(c.label(), "1x");
        c.slower();
        assert_eq!(c.label(), "1x");
        c.faster();
        assert_eq!(c.label(), "2x");
        for _ in 0..9 {
            c.faster();
        }
        assert_eq!(c.label(), "max");
        c.pause();
        assert_eq!(c.label(), "paused");
    }
}
