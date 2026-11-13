//! `wmc sprite fit`: a first draft of a sprite from an image, by the
//! method of Fogleman's Primitive with letters for shapes
//! (`docs/SPRITES.md` §5):
//!
//! 1. the image's subject is reduced to `colors` colours (k-means, seeded
//!    from its mean colour and then the farthest pixel): the palette;
//! 2. stamps are added one at a time: a few hundred random ones
//!    (character, place, turn, scale, palette colour) are tried over the
//!    picture so far, the best kept, then nudged while the score improves;
//! 3. a last pass nudges every stamp again with the others in place;
//! 4. it stops early when a new stamp no longer improves the score by
//!    [`MARGIN`]: fewer stamps read better.
//!
//! The score it climbs is shape less a quarter of the colour distance at
//! 24 px ([`super::score`]). Seeded: the same image, options and seed give
//! the same file. It writes a still; animation and poses are the author's.

use super::raster::{Canvas, Picture, Pos};
use super::score::{CELL, Score, Target, compare, score};
use super::{Frames, Ink, MAX_SCALE, MAX_STAMPS, MIN_SCALE, Pose, Sprite, SpriteFile, Stamp};

/// The characters tried when none are given: bodies, curves, limbs, eyes,
/// beaks and ears (the skill's vocabulary).
pub const CHARS: &str = "O@#0()c/\\|.'^v<>-=_*";
/// A new stamp must raise the score by this much to stay.
pub const MARGIN: f64 = 0.005;
/// Random stamps tried for each new one, and nudges after.
const TRIES: usize = 400;
const NUDGES: usize = 250;
/// Nudges per stamp in the last pass.
const POLISH: usize = 80;

/// What `fit` is asked for.
#[derive(Debug, Clone)]
pub struct Options {
    /// At most this many stamps (1 to 32).
    pub stamps: usize,
    /// Palette colours (1 to 8).
    pub colors: usize,
    /// The characters it may use.
    pub chars: Vec<u8>,
    /// The kind's footprint, in cells.
    pub size: (u8, u8),
    pub seed: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            stamps: 8,
            colors: 3,
            chars: CHARS.bytes().collect(),
            size: (1, 1),
            seed: 1,
        }
    }
}

/// A draft: the file, the score after each stamp it kept, and the full
/// score of the result.
#[derive(Debug, Clone)]
pub struct Fit {
    pub file: SpriteFile,
    pub steps: Vec<f64>,
    pub score: Score,
}

/// xorshift64*: small, seeded, the same everywhere.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// From `lo` to `hi`, both included.
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        if hi <= lo {
            return lo;
        }
        let span = u64::try_from(i64::from(hi) - i64::from(lo) + 1).expect("lo < hi");
        lo + i32::try_from(self.next() % span).expect("within the span")
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n.max(1)).expect("small")).expect("small")
    }
}

/// `colors` colours for the subject of `t` (the pixels at alpha 128 and
/// up), most used first.
pub fn palette(t: &Picture, colors: usize) -> Vec<u32> {
    let px: Vec<[f64; 3]> = t
        .rgba
        .chunks(4)
        .filter(|p| p[3] >= 128)
        .map(|p| [f64::from(p[0]), f64::from(p[1]), f64::from(p[2])])
        .collect();
    if px.is_empty() {
        return vec![0x0080_8080];
    }
    let d2 = |a: &[f64; 3], b: &[f64; 3]| (0..3).map(|c| (a[c] - b[c]).powi(2)).sum::<f64>();
    let mean = |set: &[&[f64; 3]]| {
        let n = set.len().max(1) as f64;
        let mut m = [0.0; 3];
        for p in set {
            for c in 0..3 {
                m[c] += p[c] / n;
            }
        }
        m
    };
    let all: Vec<&[f64; 3]> = px.iter().collect();
    let mut centres = vec![mean(&all)];
    while centres.len() < colors.max(1) {
        // The pixel farthest from every centre (the first of equals).
        let far = px
            .iter()
            .map(|p| centres.iter().map(|c| d2(p, c)).fold(f64::MAX, f64::min))
            .enumerate()
            .fold(
                (0, -1.0),
                |best, (i, d)| if d > best.1 { (i, d) } else { best },
            );
        if far.1 <= 0.0 {
            break;
        }
        centres.push(px[far.0]);
    }
    let mut counts = vec![0usize; centres.len()];
    for _ in 0..12 {
        let mut groups: Vec<Vec<&[f64; 3]>> = vec![Vec::new(); centres.len()];
        for p in &px {
            let k = (0..centres.len())
                .min_by(|&a, &b| d2(p, &centres[a]).total_cmp(&d2(p, &centres[b])))
                .expect("a centre");
            groups[k].push(p);
        }
        for (k, g) in groups.iter().enumerate() {
            counts[k] = g.len();
            if !g.is_empty() {
                centres[k] = mean(g);
            }
        }
    }
    let mut order: Vec<usize> = (0..centres.len()).filter(|&k| counts[k] > 0).collect();
    order.sort_by_key(|&k| std::cmp::Reverse(counts[k]));
    let byte = |v: f64| -> u32 {
        // Rounded into a byte.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let b = v.round().clamp(0.0, 255.0) as u32;
        b
    };
    order
        .iter()
        .map(|&k| byte(centres[k][0]) << 16 | byte(centres[k][1]) << 8 | byte(centres[k][2]))
        .collect()
}

/// A candidate: a stamp and its palette colour.
#[derive(Debug, Clone, Copy)]
struct Cand {
    ch: u8,
    x: i32,
    y: i32,
    turn: i32,
    scale: i32,
    colour: usize,
}

impl Cand {
    fn pos(&self) -> Pos {
        Pos {
            x: self.x,
            y: self.y,
            turn: self.turn,
            scale: self.scale,
        }
    }
}

struct Search<'a> {
    target: Picture,
    palette: &'a [u32],
    o: &'a Options,
    rng: Rng,
    max_scale: i32,
}

impl Search<'_> {
    /// Shape less a quarter of the colour distance, at 24 px.
    fn energy(&self, c: &Canvas) -> f64 {
        let (shape, colour) = compare(&self.target, &c.picture());
        shape - colour / 4.0
    }

    fn with(&self, base: &Canvas, s: &Cand) -> f64 {
        let mut c = base.clone();
        c.stamp(s.ch, s.pos(), self.palette[s.colour]);
        self.energy(&c)
    }

    fn random(&mut self) -> Cand {
        let (w, h) = (i32::from(self.o.size.0), i32::from(self.o.size.1));
        Cand {
            ch: self.o.chars[self.rng.below(self.o.chars.len())],
            x: self.rng.range(0, w * 1000),
            y: self.rng.range(0, h * 1000),
            turn: if self.rng.below(3) == 0 {
                0
            } else {
                self.rng.range(-180, 180)
            },
            scale: self.rng.range(MIN_SCALE * 2, self.max_scale),
            colour: self.rng.below(self.palette.len()),
        }
    }

    /// `s` with one value moved by up to `step` thousandths of its range.
    fn nudge(&mut self, s: &Cand, step: i32) -> Cand {
        let mut n = *s;
        let (w, h) = (i32::from(self.o.size.0), i32::from(self.o.size.1));
        let d = |rng: &mut Rng, span: i32| rng.range(-span, span).max(-span);
        match self.rng.below(6) {
            0 => n.x = (n.x + d(&mut self.rng, step * w)).clamp(0, w * 1000),
            1 => n.y = (n.y + d(&mut self.rng, step * h)).clamp(0, h * 1000),
            2 => n.turn = (n.turn + d(&mut self.rng, (step * 36 / 100).max(1))).clamp(-360, 360),
            3 => n.scale = (n.scale + d(&mut self.rng, step * 2)).clamp(MIN_SCALE, self.max_scale),
            4 => n.ch = self.o.chars[self.rng.below(self.o.chars.len())],
            _ => n.colour = self.rng.below(self.palette.len()),
        }
        n
    }

    /// Climb from `s` over `base`: `tries` nudges, each kept if it scores
    /// better, the step shrinking as it goes.
    fn climb(&mut self, base: &Canvas, mut s: Cand, mut e: f64, tries: usize) -> (Cand, f64) {
        for t in 0..tries {
            let step = i32::try_from(200 - 180 * t / tries.max(1)).expect("small");
            let n = self.nudge(&s, step);
            let ne = self.with(base, &n);
            if ne > e {
                (s, e) = (n, ne);
            }
        }
        (s, e)
    }
}

fn redraw(size: (u8, u8), stamps: &[Cand], palette: &[u32]) -> Canvas {
    let mut c = Canvas::new(size, CELL);
    for s in stamps {
        c.stamp(s.ch, s.pos(), palette[s.colour]);
    }
    c
}

/// Fit a sprite named `kind` to `target`.
pub fn fit(target: &Target, kind: &str, o: &Options) -> Fit {
    let (w, h) = (
        usize::from(o.size.0.max(1)) * CELL as usize,
        usize::from(o.size.1.max(1)) * CELL as usize,
    );
    let small = target.fitted(w, h);
    let palette = palette(&small, o.colors.clamp(1, 8));
    let mut s = Search {
        target: small,
        palette: &palette,
        o,
        rng: Rng::new(o.seed),
        max_scale: (1600 * i32::from(o.size.0.min(o.size.1).max(1))).min(MAX_SCALE),
    };
    let mut stamps: Vec<Cand> = Vec::new();
    let mut canvas = Canvas::new(o.size, CELL);
    let mut e = s.energy(&canvas);
    let mut steps = Vec::new();
    for _ in 0..o.stamps.clamp(1, MAX_STAMPS) {
        let mut best = s.random();
        let mut be = s.with(&canvas, &best);
        for _ in 1..TRIES {
            let c = s.random();
            let ce = s.with(&canvas, &c);
            if ce > be {
                (best, be) = (c, ce);
            }
        }
        (best, be) = s.climb(&canvas, best, be, NUDGES);
        if !stamps.is_empty() && be - e < MARGIN {
            break;
        }
        canvas.stamp(best.ch, best.pos(), palette[best.colour]);
        stamps.push(best);
        e = be;
        steps.push(e);
    }
    // Each stamp again, with the others in place.
    for i in 0..stamps.len() {
        for t in 0..POLISH {
            let step = i32::try_from(80 - 70 * t / POLISH).expect("small");
            let mut tried = stamps.clone();
            tried[i] = s.nudge(&stamps[i], step);
            let te = s.energy(&redraw(o.size, &tried, &palette));
            if te > e {
                stamps = tried;
                e = te;
            }
        }
    }
    let pose = Pose {
        look: None,
        frames: Frames::STILL,
        own_frames: false,
        stamps: stamps
            .iter()
            .map(|c| Stamp {
                ch: c.ch,
                x: c.x,
                y: c.y,
                turn: c.turn,
                scale: c.scale,
                color: Ink::Named(u8::try_from(c.colour).expect("at most 8 colours")),
                ..Stamp::default()
            })
            .collect(),
    };
    let sc = score(&pose, &palette, 0, o.size, target);
    let file = SpriteFile {
        palette: palette
            .iter()
            .enumerate()
            .map(|(i, &c)| (format!("c{}", i + 1), c))
            .collect(),
        sprites: vec![Sprite {
            name: kind.to_string(),
            line: 0,
            col: 0,
            poses: vec![pose],
        }],
    };
    Fit {
        file,
        steps,
        score: sc,
    }
}

#[cfg(test)]
mod tests;
