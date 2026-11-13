//! `wmc sprite score`: how close a sprite's first frame is to an image, as
//! numbers an agent can compare between tries (`docs/SPRITES.md` §5).
//!
//! - `shape`: the overlap of the two silhouettes (intersection over union)
//!   at 24 px a cell; 1 is the same outline.
//! - `colour`: the mean colour distance where both are, at 24 px; 0 is the
//!   same colours, 1 black against white.
//! - `detail`: the same two at 96 px, a tie-breaker weighted low:
//!   readability first.
//! - `stamps`: how many it took.
//!
//! The image is fitted into the sprite's box (its kind's footprint), its
//! aspect kept, centred; its background is its alpha channel when it has
//! one, else the colour of its corners (or a key colour given).

use std::path::Path;

use super::raster::{Picture, draw};
use super::{Pose, Sprite};

/// A cell at the size a player sees, and at the size of the detail.
pub const CELL: u32 = 24;
pub const DETAIL_CELL: u32 = 96;
/// How far (RGB distance, of 441) a pixel may be from the key colour and
/// still be background.
pub const KEY_TOLERANCE: f64 = 48.0;
/// A pixel is in a silhouette from this alpha on.
const SOLID: u8 = 128;
/// Black to white.
const MAX_DISTANCE: f64 = 441.672_955_930_063_7;

/// An image to score against: straight-alpha RGBA, its background clear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub picture: Picture,
    /// The background colour keyed out, `None` when the image's own alpha
    /// says what is background.
    pub key: Option<u32>,
}

impl Target {
    /// `rgba` (`width x height`, straight alpha) as a target: with
    /// transparency, its alpha is the silhouette; without, every pixel
    /// within [`KEY_TOLERANCE`] of `key` (else of its corners' colour) is
    /// background.
    pub fn from_rgba(width: usize, height: usize, rgba: Vec<u8>, key: Option<u32>) -> Self {
        let mut p = Picture {
            width,
            height,
            rgba,
        };
        let clear = p.rgba.chunks(4).any(|px| px[3] < 255);
        let mut keyed = None;
        if !clear && width > 0 && height > 0 {
            let key = key.unwrap_or_else(|| corner_colour(&p));
            keyed = Some(key);
            let k = [(key >> 16) & 0xff, (key >> 8) & 0xff, key & 0xff].map(f64::from);
            for px in p.rgba.chunks_mut(4) {
                let d = (0..3)
                    .map(|c| (f64::from(px[c]) - k[c]).powi(2))
                    .sum::<f64>()
                    .sqrt();
                px[3] = if d > KEY_TOLERANCE { 255 } else { 0 };
            }
        }
        Self {
            picture: p,
            key: keyed,
        }
    }

    /// An image file (PNG or JPEG).
    pub fn open(path: &Path, key: Option<u32>) -> anyhow::Result<Self> {
        let img = image::open(path)
            .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?
            .to_rgba8();
        let (w, h) = (img.width() as usize, img.height() as usize);
        if w == 0 || h == 0 {
            anyhow::bail!("{}: an empty image", path.display());
        }
        Ok(Self::from_rgba(w, h, img.into_raw(), key))
    }

    /// The image fitted into a `width x height` box: scaled to fit, its
    /// aspect kept, centred, the rest clear. Each pixel averages a 4 x 4
    /// grid of samples (premultiplied), so it shrinks and grows smoothly.
    pub fn fitted(&self, width: usize, height: usize) -> Picture {
        let src = &self.picture;
        let mut out = Picture::new(width, height);
        if src.width == 0 || src.height == 0 || width == 0 || height == 0 {
            return out;
        }
        let s = (width as f64 / src.width as f64).min(height as f64 / src.height as f64);
        let (dw, dh) = (src.width as f64 * s, src.height as f64 * s);
        let (ox, oy) = ((width as f64 - dw) / 2.0, (height as f64 - dh) / 2.0);
        const N: usize = 4;
        for y in 0..height {
            for x in 0..width {
                let mut acc = [0f64; 4];
                for j in 0..N {
                    for i in 0..N {
                        let fx = (x as f64 + (i as f64 + 0.5) / N as f64 - ox) / s;
                        let fy = (y as f64 + (j as f64 + 0.5) / N as f64 - oy) / s;
                        if fx < 0.0 || fy < 0.0 {
                            continue;
                        }
                        // In range: floor of a small positive number.
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let (sx, sy) = (fx as usize, fy as usize);
                        if sx >= src.width || sy >= src.height {
                            continue;
                        }
                        let px = src.at(sx, sy);
                        let a = f64::from(px[3]) / 255.0;
                        for c in 0..3 {
                            acc[c] += f64::from(px[c]) * a;
                        }
                        acc[3] += a;
                    }
                }
                if acc[3] <= 0.0 {
                    continue;
                }
                let o = (y * width + x) * 4;
                for c in 0..3 {
                    out.rgba[o + c] = byte(acc[c] / acc[3]);
                }
                out.rgba[o + 3] = byte(acc[3] / (N * N) as f64 * 255.0);
            }
        }
        out
    }
}

// Rounded and clamped to a byte.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn byte(v: f64) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// The colour most of the four corners have (the top-left one on a tie).
fn corner_colour(p: &Picture) -> u32 {
    let (w, h) = (p.width - 1, p.height - 1);
    let rgb = |x, y| {
        let [r, g, b, _] = p.at(x, y);
        u32::from(r) << 16 | u32::from(g) << 8 | u32::from(b)
    };
    let c = [rgb(0, 0), rgb(w, 0), rgb(0, h), rgb(w, h)];
    *c.iter()
        .max_by_key(|&&k| (c.iter().filter(|&&o| o == k).count(), k == c[0]))
        .expect("four corners")
}

/// `shape` and `colour` of `sprite` against `target`, two pictures of one
/// size.
pub fn compare(target: &Picture, sprite: &Picture) -> (f64, f64) {
    let (mut both, mut either, mut dist) = (0u64, 0u64, 0f64);
    for (t, s) in target.rgba.chunks(4).zip(sprite.rgba.chunks(4)) {
        let (a, b) = (t[3] >= SOLID, s[3] >= SOLID);
        either += u64::from(a || b);
        if a && b {
            both += 1;
            dist += (0..3)
                .map(|c| (f64::from(t[c]) - f64::from(s[c])).powi(2))
                .sum::<f64>()
                .sqrt();
        }
    }
    let shape = if either == 0 {
        0.0
    } else {
        both as f64 / either as f64
    };
    let colour = if both == 0 {
        1.0
    } else {
        dist / both as f64 / MAX_DISTANCE
    };
    (shape, colour)
}

/// A sprite's numbers against an image.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Score {
    pub shape: f64,
    pub colour: f64,
    pub detail_shape: f64,
    pub detail_colour: f64,
    pub stamps: usize,
}

impl Score {
    /// One number to climb (`wmc sprite fit`): shape at 24 px, less a
    /// quarter of the colour distance, nine parts in ten; the same at 96 px
    /// for the tenth.
    pub fn value(&self) -> f64 {
        0.9 * (self.shape - self.colour / 4.0)
            + 0.1 * (self.detail_shape - self.detail_colour / 4.0)
    }
}

/// The first frame of `pose` against `target`, at [`CELL`] and
/// [`DETAIL_CELL`].
pub fn score(pose: &Pose, palette: &[u32], kind: u32, size: (u8, u8), target: &Target) -> Score {
    let at = |cell: u32| {
        let s = draw(pose, 0, palette, kind, size, cell);
        let t = target.fitted(s.width, s.height);
        compare(&t, &s)
    };
    let (shape, colour) = at(CELL);
    let (detail_shape, detail_colour) = at(DETAIL_CELL);
    Score {
        shape,
        colour,
        detail_shape,
        detail_colour,
        stamps: pose.stamps.len(),
    }
}

/// The cells a sprite's stamps reach, 1 to 16 each way: its box when no
/// rules say its kind's size.
pub fn reach(s: &Sprite) -> (u8, u8) {
    let max = |v: fn(&super::Stamp) -> i32| {
        let m = s
            .poses
            .iter()
            .flat_map(|p| &p.stamps)
            .map(v)
            .max()
            .unwrap_or(0);
        u8::try_from(((m + 999) / 1000).clamp(1, 16)).expect("1 to 16")
    };
    (max(|st| st.x), max(|st| st.y))
}

/// Target, sprite and their difference side by side at [`DETAIL_CELL`]:
/// the first two over `bg`; in the third, grey where both are, red where
/// only the image is, blue where only the sprite is.
pub fn diff(
    pose: &Pose,
    palette: &[u32],
    kind: u32,
    size: (u8, u8),
    target: &Target,
    bg: u32,
) -> Picture {
    const GAP: usize = 6;
    let s = draw(pose, 0, palette, kind, size, DETAIL_CELL);
    let t = target.fitted(s.width, s.height);
    let (w, h) = (s.width, s.height);
    let mut out = Picture::new(w * 3 + GAP * 4, h + GAP * 2);
    let put = |out: &mut Picture, x0: usize, x: usize, y: usize, px: [u8; 4]| {
        let o = ((y + GAP) * out.width + x0 + x) * 4;
        out.rgba[o..o + 4].copy_from_slice(&px);
    };
    let over = |px: [u8; 4]| {
        let a = u32::from(px[3]);
        let k = [(bg >> 16) & 0xff, (bg >> 8) & 0xff, bg & 0xff];
        let mut o = [0, 0, 0, 255];
        for c in 0..3 {
            o[c] = u8::try_from((u32::from(px[c]) * a + k[c] * (255 - a) + 127) / 255)
                .expect("a mean of bytes");
        }
        o
    };
    for px in out.rgba.chunks_mut(4) {
        px.copy_from_slice(&[0x20, 0x20, 0x24, 255]);
    }
    for y in 0..h {
        for x in 0..w {
            let (tp, sp) = (t.at(x, y), s.at(x, y));
            put(&mut out, GAP, x, y, over(tp));
            put(&mut out, GAP * 2 + w, x, y, over(sp));
            let d = match (tp[3] >= SOLID, sp[3] >= SOLID) {
                (true, true) => [0x80, 0x80, 0x80, 255],
                (true, false) => [0xd0, 0x30, 0x30, 255],
                (false, true) => [0x30, 0x60, 0xd0, 255],
                (false, false) => [0x10, 0x10, 0x12, 255],
            };
            put(&mut out, GAP * 3 + w * 2, x, y, d);
        }
    }
    out
}

#[cfg(test)]
mod tests;
