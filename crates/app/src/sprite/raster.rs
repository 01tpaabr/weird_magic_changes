//! Stamps to pictures: a pose's frame as an RGBA box of `W*cell x H*cell`
//! pixels (straight alpha). Pure, no Bevy; the game's tiles and `wmc
//! sprite render` both come from here, so what an author sees is what a
//! player sees.
//!
//! - A stamp's character is rasterized by `fontdue` (the bundled font) at
//!   `scale` cells, supersampled 2x. Its **ink** (the glyph's bitmap box)
//!   is centred on `X Y`, so `"."` at `0.4 0.4` is a dot there whatever the
//!   font's baseline.
//! - Turns are whole degrees through an integer sine table, and the mapping
//!   is fixed point: a picture is the same on every machine, as the bundled
//!   font makes the glyphs.
//! - Swings: at frame `f` of `N`, a value gains `A * sin(360 f / N + phase)`
//!   (degrees, in sixteenths through the same table).
//! - Stamps are drawn in order, each over the last (premultiplied, then
//!   back to straight alpha when the box is shrunk to its size).

use std::sync::LazyLock;

use fontdue::{Font, FontSettings};

use super::{Ink, MAX_SCALE, MIN_SCALE, Pose, Stamp};

/// The sine table's one: `sin(90) == ONE`.
pub const ONE: i32 = 16384;

/// `sin(d) * ONE` for whole degrees 0 to 90, rounded.
const SIN: [i32; 91] = [
    0, 286, 572, 857, 1143, 1428, 1713, 1997, 2280, 2563, 2845, 3126, 3406, 3686, 3964, 4240, 4516,
    4790, 5063, 5334, 5604, 5872, 6138, 6402, 6664, 6924, 7182, 7438, 7692, 7943, 8192, 8438, 8682,
    8923, 9162, 9397, 9630, 9860, 10087, 10311, 10531, 10749, 10963, 11174, 11381, 11585, 11786,
    11982, 12176, 12365, 12551, 12733, 12911, 13085, 13255, 13421, 13583, 13741, 13894, 14044,
    14189, 14330, 14466, 14598, 14726, 14849, 14968, 15082, 15191, 15296, 15396, 15491, 15582,
    15668, 15749, 15826, 15897, 15964, 16026, 16083, 16135, 16182, 16225, 16262, 16294, 16322,
    16344, 16362, 16374, 16382, 16384,
];

/// `sin` of whole degrees, times [`ONE`].
pub fn sin_deg(d: i64) -> i32 {
    let d = usize::try_from(d.rem_euclid(360)).expect("0 to 359");
    match d {
        0..=90 => SIN[d],
        91..=180 => SIN[180 - d],
        181..=270 => -SIN[d - 180],
        _ => -SIN[360 - d],
    }
}

/// `sin` of `a` sixteenths of a degree, times [`ONE`]: the table,
/// interpolated between whole degrees.
pub fn sin16(a: i64) -> i32 {
    let a = a.rem_euclid(360 * 16);
    let (d, frac) = (a / 16, i32::try_from(a % 16).expect("0 to 15"));
    let (s0, s1) = (sin_deg(d), sin_deg(d + 1));
    s0 + (s1 - s0) * frac / 16
}

/// `v * k / ONE`, rounded to the nearest (halves up).
fn swung(v: i32, k: i32) -> i32 {
    let p = i64::from(v) * i64::from(k);
    i32::try_from((p + i64::from(ONE / 2)).div_euclid(i64::from(ONE))).expect("|v| fits")
}

/// A stamp's values at frame `f` of `n`: centre (thousandths of a cell),
/// turn (degrees), scale (thousandths).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub x: i32,
    pub y: i32,
    pub turn: i32,
    pub scale: i32,
}

pub fn at_frame(s: &Stamp, f: u8, n: u8) -> Pos {
    let a = 360 * 16 * i64::from(f) / i64::from(n.max(1)) + i64::from(s.phase) * 16;
    let k = sin16(a);
    Pos {
        x: s.x + swung(s.swing.x, k),
        y: s.y + swung(s.swing.y, k),
        turn: s.turn + swung(s.swing.turn, k),
        scale: (s.scale + swung(s.swing.scale, k)).clamp(MIN_SCALE, MAX_SCALE),
    }
}

static FONT: LazyLock<Font> = LazyLock::new(|| {
    Font::from_bytes(crate::render::atlas::FONT, FontSettings::default())
        .expect("bundled font parses")
});

/// A picture: `width x height` RGBA8, straight alpha, rows top-down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picture {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Picture {
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            rgba: vec![0; width * height * 4],
        }
    }

    /// The pixel at `(x, y)`.
    pub fn at(&self, x: usize, y: usize) -> [u8; 4] {
        let i = (y * self.width + x) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// The `cell x cell` box whose top-left is cell `(cx, cy)`.
    pub fn cut(&self, cx: usize, cy: usize, cell: usize) -> Picture {
        let mut out = Picture::new(cell, cell);
        for y in 0..cell {
            let src = ((cy * cell + y) * self.width + cx * cell) * 4;
            out.rgba[y * cell * 4..][..cell * 4].copy_from_slice(&self.rgba[src..][..cell * 4]);
        }
        out
    }
}

/// The colour a stamp is drawn in.
pub fn ink(i: Ink, palette: &[u32], kind: u32) -> u32 {
    match i {
        Ink::Kind => kind,
        Ink::Named(n) => palette.get(usize::from(n)).copied().unwrap_or(kind),
        Ink::Rgb(c) => c,
    }
}

/// Frame `f` of `pose` as a picture of `size` cells of `cell` pixels:
/// `palette` is its file's, `kind` the kind's colour (for stamps without
/// one).
pub fn draw(pose: &Pose, f: u8, palette: &[u32], kind: u32, size: (u8, u8), cell: u32) -> Picture {
    let mut c = Canvas::new(size, cell);
    for s in &pose.stamps {
        c.stamp(
            s.ch,
            at_frame(s, f, pose.frames.n),
            ink(s.color, palette, kind),
        );
    }
    c.picture()
}

/// A picture being drawn: premultiplied RGBA at twice its size, stamps
/// added one by one over what is there ([`draw`] is one of these; `wmc
/// sprite fit` keeps one per try).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Canvas {
    cell: usize,
    w: usize,
    h: usize,
    px: Vec<[u32; 4]>,
}

impl Canvas {
    /// An empty canvas of `size` cells of `cell` pixels.
    pub fn new(size: (u8, u8), cell: u32) -> Self {
        let cell = cell.max(1) as usize;
        let (w, h) = (
            usize::from(size.0.max(1)) * cell,
            usize::from(size.1.max(1)) * cell,
        );
        Self {
            cell,
            w,
            h,
            px: vec![[0u32; 4]; w * h * 4],
        }
    }

    /// One stamp, `ch` at `p` in `rgb`, over what is drawn.
    pub fn stamp(&mut self, ch: u8, p: Pos, rgb: u32) {
        stamp(&mut self.px, self.w * 2, self.h * 2, ch, p, rgb, self.cell);
    }

    /// The canvas at its size: 2x2 boxes averaged, then straight alpha.
    pub fn picture(&self) -> Picture {
        let (w, h, w2) = (self.w, self.h, self.w * 2);
        let mut out = Picture::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let mut sum = [0u32; 4];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let px = self.px[(y * 2 + dy) * w2 + x * 2 + dx];
                    for c in 0..4 {
                        sum[c] += px[c];
                    }
                }
                let a = (sum[3] + 2) / 4;
                let o = (y * w + x) * 4;
                if a == 0 {
                    continue;
                }
                for (dst, s) in out.rgba[o..o + 3].iter_mut().zip(sum) {
                    let v = ((s + 2) / 4 * 255 + a / 2) / a;
                    *dst = u8::try_from(v.min(255)).expect("clamped");
                }
                out.rgba[o + 3] = u8::try_from(a.min(255)).expect("clamped");
            }
        }
        out
    }
}

/// Draw one stamp over a premultiplied canvas of `w2 x h2` pixels (twice
/// the cell size `cell`).
fn stamp(canvas: &mut [[u32; 4]], w2: usize, h2: usize, ch: u8, p: Pos, rgb: u32, cell: usize) {
    let px = p.scale as f32 / 1000.0 * (cell * 2) as f32;
    let (m, bitmap) = FONT.rasterize(char::from(ch), px);
    if m.width == 0 || m.height == 0 {
        return;
    }
    const FP: i64 = 1 << 16;
    let cell2 = i64::try_from(cell * 2).expect("a cell fits");
    // The ink's centre on the canvas and in the bitmap, 16.16 fixed point.
    let (cx, cy) = (
        i64::from(p.x) * cell2 * FP / 1000,
        i64::from(p.y) * cell2 * FP / 1000,
    );
    let (bw, bh) = (
        i64::try_from(m.width).expect("a glyph fits"),
        i64::try_from(m.height).expect("a glyph fits"),
    );
    let (bcx, bcy) = (bw * FP / 2, bh * FP / 2);
    let (sin, cos) = (
        i64::from(sin_deg(i64::from(p.turn))),
        i64::from(sin_deg(i64::from(p.turn) + 90)),
    );
    let one = i64::from(ONE);
    // Every canvas pixel the turned bitmap can reach.
    let r = (bw + bh) / 2 + 2;
    let (x0, x1) = (
        (cx / FP - r).max(0),
        (cx / FP + r).min(i64::try_from(w2).expect("fits")),
    );
    let (y0, y1) = (
        (cy / FP - r).max(0),
        (cy / FP + r).min(i64::try_from(h2).expect("fits")),
    );
    let cov = |u: i64, v: i64| -> i64 {
        if u < 0 || v < 0 || u >= bw || v >= bh {
            return 0;
        }
        i64::from(bitmap[usize::try_from(v * bw + u).expect("inside")])
    };
    let [r8, g8, b8] = [(rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff].map(u64::from);
    for y in y0..y1 {
        for x in x0..x1 {
            // The pixel's centre, from the ink's centre, turned back
            // (clockwise on screen is the matrix [cos -sin; sin cos]).
            let (dx, dy) = (x * FP + FP / 2 - cx, y * FP + FP / 2 - cy);
            let u = (dx * cos + dy * sin) / one + bcx - FP / 2;
            let v = (-dx * sin + dy * cos) / one + bcy - FP / 2;
            let (u0, v0) = (u.div_euclid(FP), v.div_euclid(FP));
            let (fu, fv) = ((u.rem_euclid(FP)) >> 8, (v.rem_euclid(FP)) >> 8);
            let top = cov(u0, v0) * (256 - fu) + cov(u0 + 1, v0) * fu;
            let bottom = cov(u0, v0 + 1) * (256 - fu) + cov(u0 + 1, v0 + 1) * fu;
            let a = (top * (256 - fv) + bottom * fv) >> 16;
            if a <= 0 {
                continue;
            }
            let a = u64::try_from(a.min(255)).expect("0 to 255");
            let px = &mut canvas
                [usize::try_from(y).expect("inside") * w2 + usize::try_from(x).expect("inside")];
            let keep = 255 - a;
            for (c, src) in px
                .iter_mut()
                .zip([r8 * a / 255, g8 * a / 255, b8 * a / 255, a])
            {
                let v = src + (u64::from(*c) * keep + 127) / 255;
                *c = u32::try_from(v.min(255)).expect("clamped");
            }
        }
    }
}

#[cfg(test)]
mod tests;
