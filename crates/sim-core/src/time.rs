//! Game time. **Time is integers**: the world clock is a `u64` tick count,
//! durations are tick counts, progress is an integer accumulator. No `f32`
//! time anywhere in the sim, so nothing drifts and nothing depends on
//! reduction order.
//!
//! Three separate ideas, kept separate on purpose:
//! - **tick**: the atomic sim step (`World::tick`). Everything happens at tick
//!   boundaries; the sim never sees a wall clock.
//! - **cadence**: how often a system or entity does work: every `k` ticks with
//!   `k` a power of two, staggered by a hash of the chunk coordinate (never the
//!   slab slot) so each tick touches `1/k` of the chunks. Lands with the first
//!   system that needs it.
//! - **speed**: real ticks per second. An `app` concern (`app::clock`); the sim
//!   is identical at every speed.
//!
//! The day is [`TICKS_PER_DAY`] ticks: at the default 8 ticks per real second
//! that is a 45 minute day, and a 24 h x 60 min clock falls on whole ticks
//! (15 per in-game minute). Content durations go through [`minutes`],
//! [`hours`], [`days`], never a bare number.

/// Ticks per in-game minute.
pub const TICKS_PER_MINUTE: u64 = 15;
/// Ticks per in-game hour.
pub const TICKS_PER_HOUR: u64 = 60 * TICKS_PER_MINUTE;
/// Ticks per in-game day. Saved in the world header; a build with a different
/// value refuses the save.
pub const TICKS_PER_DAY: u64 = 24 * TICKS_PER_HOUR;
/// Tick a new world starts at: 06:00 on day 0, so the first thing a player
/// sees is dawn, not the dark.
pub const START_TICK: u64 = 6 * TICKS_PER_HOUR;

/// In-game minutes as ticks.
#[inline]
pub const fn minutes(n: u64) -> u64 {
    n * TICKS_PER_MINUTE
}

/// In-game hours as ticks.
#[inline]
pub const fn hours(n: u64) -> u64 {
    n * TICKS_PER_HOUR
}

/// In-game days as ticks.
#[inline]
pub const fn days(n: u64) -> u64 {
    n * TICKS_PER_DAY
}

/// A tick broken into calendar parts. Derived, never stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Clock {
    /// Days since the world began, from 0.
    pub day: u64,
    /// `0..24`.
    pub hour: u8,
    /// `0..60`.
    pub minute: u8,
    /// Ticks since midnight, `0..TICKS_PER_DAY`.
    pub tick_of_day: u32,
}

impl Clock {
    pub const fn at(tick: u64) -> Self {
        let tick_of_day = tick % TICKS_PER_DAY;
        Self {
            day: tick / TICKS_PER_DAY,
            hour: (tick_of_day / TICKS_PER_HOUR) as u8,
            minute: (tick_of_day % TICKS_PER_HOUR / TICKS_PER_MINUTE) as u8,
            tick_of_day: tick_of_day as u32,
        }
    }
}

impl std::fmt::Display for Clock {
    /// `day 3 06:00`
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "day {} {:02}:{:02}", self.day, self.hour, self.minute)
    }
}

/// When the sun is up. Light ramps linearly over one hour at each end.
pub const SUNRISE: u64 = 6 * TICKS_PER_HOUR;
pub const SUNSET: u64 = 18 * TICKS_PER_HOUR;
const RAMP: u64 = TICKS_PER_HOUR;

/// Sunlight at `tick`, `0` (night) to `255` (full day). Integer, piecewise
/// linear: dark until [`SUNRISE`], full by one hour later, full until one
/// hour before [`SUNSET`], dark from [`SUNSET`]. Sim quantity: systems that
/// care about light (plants) read this; the renderer maps it to brightness.
pub const fn daylight(tick: u64) -> u8 {
    let t = tick % TICKS_PER_DAY;
    if t < SUNRISE || t >= SUNSET {
        0
    } else if t < SUNRISE + RAMP {
        ((t - SUNRISE) * 255 / RAMP) as u8
    } else if t >= SUNSET - RAMP {
        ((SUNSET - t) * 255 / RAMP) as u8
    } else {
        255
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_is_45_real_minutes_at_8_tps() {
        assert_eq!(TICKS_PER_DAY, 21_600);
        assert_eq!(TICKS_PER_DAY / 8, 45 * 60);
        assert_eq!(days(1), hours(24));
        assert_eq!(hours(1), minutes(60));
    }

    #[test]
    fn clock_breaks_ticks_into_calendar_parts() {
        assert_eq!(
            Clock::at(0),
            Clock {
                day: 0,
                hour: 0,
                minute: 0,
                tick_of_day: 0
            }
        );
        let c = Clock::at(START_TICK);
        assert_eq!((c.day, c.hour, c.minute), (0, 6, 0));
        let c = Clock::at(days(3) + hours(23) + minutes(59) + 14);
        assert_eq!((c.day, c.hour, c.minute), (3, 23, 59));
        assert_eq!(c.tick_of_day, (TICKS_PER_DAY - 1) as u32);
        assert_eq!(Clock::at(days(4)).day, 4);
        assert_eq!(Clock::at(days(3) + hours(7)).to_string(), "day 3 07:00");
    }

    #[test]
    fn daylight_is_dark_at_night_full_at_noon_and_ramps_monotonically() {
        assert_eq!(daylight(0), 0);
        assert_eq!(daylight(hours(5) + minutes(59)), 0);
        assert_eq!(daylight(SUNRISE), 0);
        assert_eq!(daylight(SUNRISE + RAMP), 255);
        assert_eq!(daylight(hours(12)), 255);
        assert_eq!(daylight(SUNSET - RAMP), 255);
        assert_eq!(daylight(SUNSET), 0);
        assert_eq!(daylight(hours(23) + minutes(59)), 0);
        // Day 5 looks like day 0.
        assert_eq!(
            daylight(days(5) + hours(6) + minutes(30)),
            daylight(hours(6) + minutes(30))
        );
        let mut prev = 0;
        for t in SUNRISE..=SUNRISE + RAMP {
            assert!(daylight(t) >= prev);
            prev = daylight(t);
        }
        let mut prev = 255;
        for t in SUNSET - RAMP..=SUNSET {
            assert!(daylight(t) <= prev);
            prev = daylight(t);
        }
    }
}
