//! `wmc sprite render`: every frame of the chosen poses on one sheet, each
//! twice: at the cell size a player sees, and enlarged 8x, pixel for
//! pixel, which is what an author edits. One row of each per pose.

use std::path::Path;

use super::raster::{Picture, draw};
use super::{Frames, Pose};

/// How much the enlarged copy is enlarged.
pub const ENLARGE: usize = 8;
/// Around the sheet and between pictures, in pixels.
const MARGIN: usize = 8;
const GAP: usize = 6;
/// The sheet's colour between pictures.
const SHEET: u32 = 0x00_20_20_24;

/// A pose to put on the sheet, with what it is drawn with.
#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    pub pose: &'a Pose,
    pub palette: &'a [u32],
    /// The kind's colour (stamps without one).
    pub kind: u32,
    pub size: (u8, u8),
}

/// Over an opaque `bg`: straight alpha composed, rounded.
fn over(px: [u8; 4], bg: u32) -> [u8; 4] {
    let a = u32::from(px[3]);
    let bgc = [(bg >> 16) & 0xff, (bg >> 8) & 0xff, bg & 0xff];
    let mut out = [0, 0, 0, 255];
    for c in 0..3 {
        let v = (u32::from(px[c]) * a + bgc[c] * (255 - a) + 127) / 255;
        out[c] = u8::try_from(v).expect("a mean of bytes");
    }
    out
}

fn fill(p: &mut Picture, x0: usize, y0: usize, w: usize, h: usize, rgb: u32) {
    let px = over([0, 0, 0, 0], rgb);
    for y in y0..y0 + h {
        for x in x0..x0 + w {
            let i = (y * p.width + x) * 4;
            p.rgba[i..i + 4].copy_from_slice(&px);
        }
    }
}

/// `pic`, enlarged `k` times, over `bg`, at `(x0, y0)` on `sheet`.
fn put(sheet: &mut Picture, pic: &Picture, k: usize, x0: usize, y0: usize, bg: u32) {
    for y in 0..pic.height * k {
        for x in 0..pic.width * k {
            let px = over(pic.at(x / k, y / k), bg);
            let i = ((y0 + y) * sheet.width + x0 + x) * 4;
            sheet.rgba[i..i + 4].copy_from_slice(&px);
        }
    }
}

/// The sheet: per row, its frames at `cell`, then below them enlarged
/// [`ENLARGE`] times, each over `bg`.
pub fn sheet(rows: &[Row], cell: u32, bg: u32) -> Picture {
    let c = cell.max(1) as usize;
    let dims = |r: &Row| {
        (
            usize::from(r.size.0.max(1)) * c,
            usize::from(r.size.1.max(1)) * c,
        )
    };
    let strip = |r: &Row, k: usize| {
        let n = usize::from(r.pose.frames.n.max(1));
        n * (dims(r).0 * k + GAP) - GAP
    };
    let width = MARGIN * 2 + rows.iter().map(|r| strip(r, ENLARGE)).max().unwrap_or(0);
    let height = MARGIN * 2
        + rows
            .iter()
            .map(|r| dims(r).1 * (1 + ENLARGE) + GAP * 3)
            .sum::<usize>();
    let mut out = Picture::new(width, height);
    fill(&mut out, 0, 0, width, height, SHEET);
    let mut y = MARGIN;
    for r in rows {
        let (w, h) = dims(r);
        for f in 0..r.pose.frames.n.max(1) {
            let pic = draw(r.pose, f, r.palette, r.kind, r.size, cell);
            let i = usize::from(f);
            put(&mut out, &pic, 1, MARGIN + i * (w + GAP), y, bg);
            put(
                &mut out,
                &pic,
                ENLARGE,
                MARGIN + i * (w * ENLARGE + GAP),
                y + h + GAP,
                bg,
            );
        }
        y += h * (1 + ENLARGE) + GAP * 3;
    }
    out
}

/// A row's caption for the legend: the pose and its frames.
pub fn caption(pose: &Pose) -> String {
    let which = pose
        .look
        .map_or_else(|| "own stamps".to_string(), |v| format!("pose {v}"));
    match pose.frames {
        Frames { n: 1, .. } => format!("{which}, a still, {} stamps", pose.stamps.len()),
        Frames { n, every_ms } => format!(
            "{which}, {n} frames every {every_ms}ms, {} stamps",
            pose.stamps.len()
        ),
    }
}

/// Write `p` as a PNG.
pub fn write_png(p: &Picture, path: &Path) -> anyhow::Result<()> {
    let w = u32::try_from(p.width)?;
    let h = u32::try_from(p.height)?;
    let img = image::RgbaImage::from_raw(w, h, p.rgba.clone())
        .ok_or_else(|| anyhow::anyhow!("a picture of {w}x{h} with the wrong number of bytes"))?;
    img.save_with_format(path, image::ImageFormat::Png)
        .map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))
}
