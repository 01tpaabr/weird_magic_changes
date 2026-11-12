//! Sprite files made from the grammar (`docs/SPRITES.md` §10), for property
//! tests and fuzzers. Test support only.
//!
//! A file is drawn from a list of choices, as `sim_core`'s rules generator
//! draws programs: each decision takes the next `u32`, and a list that has
//! run out answers 0, the simplest alternative (end the list, leave the
//! option out). So shrinking the list shrinks the file. Every file it
//! makes is valid: in range, within the limits, names declared.

use proptest::prelude::*;

use super::{
    Frames, Ink, MAX_EVERY_MS, MAX_FRAMES, MAX_POSES, MAX_SCALE, MAX_STAMPS, MAX_SWING_TURN,
    MAX_TURN, MAX_XY, MIN_EVERY_MS, MIN_SCALE, Pose, Sprite, SpriteFile, Stamp, Swing,
};

/// Kinds a sprite may name: built-in ones, a namespaced one, and one no
/// pack has.
const NAMES: [&str; 8] = [
    "fox",
    "chicken",
    "bee",
    "tree",
    "seed",
    "weather.cloud",
    "dragon",
    "_x9",
];

/// Characters worth drawing: dense, curved, thin, the quotes, the space.
const CHARS: &[u8] = b"O@#0()c/\\|.'^v<>\" ~xX";

/// Files from up to 800 choices.
pub fn file() -> impl Strategy<Value = SpriteFile> {
    prop::collection::vec(any::<u32>(), 0..800).prop_map(|c| generate(&c))
}

struct G<'a> {
    choices: &'a [u32],
    at: usize,
}

impl G<'_> {
    /// A choice below `n` (0 once the list runs out).
    fn below(&mut self, n: u32) -> u32 {
        let c = self.choices.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        if n == 0 { 0 } else { c % n }
    }

    /// From `lo` to `hi`, leaning to the simple end (`lo`).
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        let span = u32::try_from(hi - lo).expect("lo <= hi") + 1;
        lo + i32::try_from(self.below(span)).expect("a span of i32")
    }

    /// A value in thousandths: usually a round one.
    fn milli(&mut self, lo: i32, hi: i32) -> i32 {
        match self.below(3) {
            0 => lo,
            1 => (self.range(lo, hi) / 100) * 100,
            _ => self.range(lo, hi),
        }
        .clamp(lo, hi)
    }

    fn frames(&mut self) -> Frames {
        let n = u8::try_from(self.range(1, i32::from(MAX_FRAMES))).expect("at most 16");
        let every = self.range(MIN_EVERY_MS as i32, MAX_EVERY_MS as i32);
        // Whole milliseconds, a frame of n > 1 needs one; a still may say it.
        let every_ms = if n > 1 || self.below(2) == 1 {
            u32::try_from(every).expect("positive")
        } else {
            0
        };
        Frames { n, every_ms }
    }

    fn stamp(&mut self, palette: usize) -> Stamp {
        let ch = CHARS[self.below(CHARS.len() as u32) as usize];
        let mut s = Stamp {
            ch,
            x: self.milli(0, MAX_XY.min(2000)),
            y: self.milli(0, MAX_XY.min(2000)),
            ..Stamp::default()
        };
        if self.below(2) == 1 {
            s.turn = self.range(-MAX_TURN, MAX_TURN);
        }
        if self.below(2) == 1 {
            s.scale = self.milli(MIN_SCALE, MAX_SCALE);
        }
        s.color = match self.below(3) {
            1 if palette > 0 => {
                Ink::Named(u8::try_from(self.below(palette as u32)).expect("at most 64"))
            }
            2 => Ink::Rgb(self.below(0x0100_0000)),
            _ => Ink::Kind,
        };
        if self.below(2) == 1 {
            s.swing = Swing {
                turn: self.range(0, MAX_SWING_TURN),
                x: self.milli(0, 1000),
                y: self.milli(0, 1000),
                scale: self.milli(0, 1000),
            };
            s.phase = self.range(0, 360);
        }
        s
    }

    fn stamps(&mut self, palette: usize) -> Vec<Stamp> {
        // Now and then a full list, for the limit.
        let n = if self.below(16) == 15 {
            MAX_STAMPS
        } else {
            self.below(6) as usize
        };
        (0..n).map(|_| self.stamp(palette)).collect()
    }
}

/// The file `choices` draw.
pub fn generate(choices: &[u32]) -> SpriteFile {
    let mut g = G { choices, at: 0 };
    let mut f = SpriteFile::default();
    for i in 0..g.below(5) {
        f.palette.push((format!("c{i}"), g.below(0x0100_0000)));
    }
    let mut names: Vec<&str> = NAMES.to_vec();
    for _ in 0..g.below(4) {
        if names.is_empty() {
            break;
        }
        let name = names.remove(g.below(names.len() as u32) as usize);
        let own_frames = g.below(2) == 1;
        let frames = if own_frames {
            g.frames()
        } else {
            Frames::STILL
        };
        let mut poses = vec![Pose {
            look: None,
            frames,
            own_frames,
            stamps: g.stamps(f.palette.len()),
        }];
        let mut looks: Vec<u8> = Vec::new();
        for _ in 0..g.below(MAX_POSES as u32) {
            let v = u8::try_from(g.below(256)).expect("a byte");
            if looks.contains(&v) {
                continue;
            }
            looks.push(v);
            let own = g.below(2) == 1;
            poses.push(Pose {
                look: Some(v),
                frames: if own { g.frames() } else { frames },
                own_frames: own,
                stamps: g.stamps(f.palette.len()),
            });
        }
        f.sprites.push(Sprite {
            name: name.to_string(),
            line: 0,
            col: 0,
            poses,
        });
    }
    f
}
