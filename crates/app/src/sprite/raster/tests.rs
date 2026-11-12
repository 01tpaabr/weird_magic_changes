use super::*;
use crate::sprite::{Frames, Swing, parse};

fn pose(text: &str) -> (Pose, Vec<u32>) {
    let f = parse("t", text).unwrap();
    let pal = f.palette.iter().map(|(_, c)| *c).collect();
    (f.sprites[0].poses[0].clone(), pal)
}

/// FNV-1a over a picture's bytes: a golden value pins every pixel.
fn digest(p: &Picture) -> u64 {
    p.rgba.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The ink's extent: (min x, min y, max x, max y) of pixels with alpha
/// above `t`.
fn ink_box(p: &Picture, t: u8) -> Option<(usize, usize, usize, usize)> {
    let mut b: Option<(usize, usize, usize, usize)> = None;
    for y in 0..p.height {
        for x in 0..p.width {
            if p.at(x, y)[3] > t {
                b = Some(match b {
                    None => (x, y, x, y),
                    Some((a, c, d, e)) => (a.min(x), c.min(y), d.max(x), e.max(y)),
                });
            }
        }
    }
    b
}

#[test]
fn the_sine_table_is_a_sine() {
    assert_eq!(sin_deg(0), 0);
    assert_eq!(sin_deg(90), ONE);
    assert_eq!(sin_deg(180), 0);
    assert_eq!(sin_deg(270), -ONE);
    assert_eq!(sin_deg(-90), -ONE);
    assert_eq!(sin_deg(450), ONE);
    for d in -720..720 {
        let want = (f64::from(d).to_radians().sin() * f64::from(ONE)).round() as i32;
        assert!((sin_deg(d.into()) - want).abs() <= 1, "{d}");
        assert_eq!(sin16(i64::from(d) * 16), sin_deg(d.into()));
    }
    // Between whole degrees: between their sines.
    let (a, b) = (sin_deg(30), sin_deg(31));
    assert!((a..=b).contains(&sin16(30 * 16 + 8)));
}

#[test]
fn swings_follow_the_sine_over_the_frames() {
    let s = Stamp {
        x: 500,
        turn: 10,
        scale: 1000,
        swing: Swing {
            turn: 20,
            x: 100,
            y: 0,
            scale: 200,
        },
        ..Stamp::default()
    };
    // 4 frames: 0, 90, 180, 270 degrees.
    let at: Vec<Pos> = (0..4).map(|f| at_frame(&s, f, 4)).collect();
    assert_eq!((at[0].x, at[0].turn, at[0].scale), (500, 10, 1000));
    assert_eq!((at[1].x, at[1].turn, at[1].scale), (600, 30, 1200));
    assert_eq!((at[2].x, at[2].turn, at[2].scale), (500, 10, 1000));
    assert_eq!((at[3].x, at[3].turn, at[3].scale), (400, -10, 800));
    // A phase of 90 starts at the top.
    let p = Stamp { phase: 90, ..s };
    assert_eq!(at_frame(&p, 0, 4).x, 600);
    // A scale never swings below its floor.
    let small = Stamp {
        scale: 200,
        swing: Swing {
            scale: 1000,
            ..Swing::default()
        },
        ..Stamp::default()
    };
    assert_eq!(at_frame(&small, 3, 4).scale, MIN_SCALE);
}

/// Exact bytes for a small box: one bar, its colour, its place. Any change
/// to the rasterizer shows here first.
#[test]
fn golden_a_bar_in_a_four_pixel_cell() {
    let (p, pal) = pose("sprite a { \"|\" at 0.5 0.5 color \"#ff0000\" }");
    let pic = draw(&p, 0, &pal, 0, (1, 1), 4);
    let alpha: Vec<u8> = pic.rgba.chunks(4).map(|px| px[3]).collect();
    assert_eq!(alpha, GOLDEN_BAR, "{alpha:?}");
    for px in pic.rgba.chunks(4).filter(|px| px[3] > 0) {
        assert_eq!(&px[..3], &[255, 0, 0]);
    }
}
const GOLDEN_BAR: [u8; 16] = [0, 38, 38, 0, 0, 46, 46, 0, 0, 46, 46, 0, 0, 43, 43, 0];

#[test]
fn golden_the_crab_at_24_px() {
    let f = parse("crab", crate::sprite::tests::CRAB).unwrap();
    let s = &f.sprites[0];
    let pal: Vec<u32> = f.palette.iter().map(|(_, c)| *c).collect();
    let sums: Vec<u64> = (0..4)
        .map(|fr| digest(&draw(&s.poses[0], fr, &pal, 0xffffff, (1, 1), 24)))
        .collect();
    assert_eq!(sums, GOLDEN_CRAB, "{sums:x?}");
}
// Frames 0 and 2 are the same picture: the swings are at sin 0 and sin 180.
const GOLDEN_CRAB: [u64; 4] = [
    0xa4e2_bc15_d861_924e,
    0xd65d_c91e_fd33_471a,
    0xa4e2_bc15_d861_924e,
    0x2519_4b82_0e3f_1897,
];

#[test]
fn ink_is_centred_on_x_y_and_turns_about_it() {
    let (p, pal) = pose("sprite a { \"|\" at 0.5 0.5 }");
    let up = draw(&p, 0, &pal, 0xffffff, (1, 1), 32);
    let (x0, y0, x1, y1) = ink_box(&up, 64).unwrap();
    assert!(
        y1 - y0 > 3 * (x1 - x0),
        "a bar stands: {:?}",
        (x0, y0, x1, y1)
    );
    assert!(
        (x0 + x1).abs_diff(31) <= 2 && (y0 + y1).abs_diff(31) <= 2,
        "centred"
    );
    let (p, _) = pose("sprite a { \"|\" at 0.5 0.5 turn 90 }");
    let side = draw(&p, 0, &pal, 0xffffff, (1, 1), 32);
    let (a0, b0, a1, b1) = ink_box(&side, 64).unwrap();
    assert!(
        a1 - a0 > 3 * (b1 - b0),
        "turned, it lies: {:?}",
        (a0, b0, a1, b1)
    );
    assert!((b0 + b1).abs_diff(31) <= 2, "still centred");
    // A whole turn is no turn.
    let (p, _) = pose("sprite a { \"F\" at 0.4 0.6 turn 360 }");
    let (q, _) = pose("sprite a { \"F\" at 0.4 0.6 }");
    assert_eq!(
        draw(&p, 0, &pal, 1, (1, 1), 24),
        draw(&q, 0, &pal, 1, (1, 1), 24)
    );
    // Moved a quarter cell right: the ink moves 8 pixels at 32.
    let (p, _) = pose("sprite a { \"|\" at 0.75 0.5 }");
    let (m0, _, m1, _) = ink_box(&draw(&p, 0, &pal, 1, (1, 1), 32), 64).unwrap();
    assert_eq!((m0 + m1) / 2, (x0 + x1) / 2 + 8);
}

#[test]
fn later_stamps_cover_earlier_ones_and_colours_come_from_the_palette() {
    let text = "palette { red \"#ff0000\" }\nsprite a { \"@\" at 0.5 0.5 scale 1.5 color red\n\"@\" at 0.5 0.5 scale 1.5 }";
    let (p, pal) = pose(text);
    let pic = draw(&p, 0, &pal, 0x00ff00, (1, 1), 24);
    // Where both are solid, the second (the kind's green) shows: all but
    // a trace of red at the edges of strokes, from the 2x2 average.
    let solid: Vec<[u8; 4]> = (0..24 * 24)
        .map(|i| pic.at(i % 24, i / 24))
        .filter(|px| px[3] == 255)
        .collect();
    assert!(!solid.is_empty());
    assert!(
        solid.iter().all(|px| px[0] < 16 && px[1] > 240),
        "{solid:?}"
    );
    let (p, pal) =
        pose("palette { red \"#ff0000\" }\nsprite a { \"@\" at 0.5 0.5 scale 1.5 color red }");
    let pic = draw(&p, 0, &pal, 0x00ff00, (1, 1), 24);
    assert!(
        (0..24 * 24)
            .map(|i| pic.at(i % 24, i / 24))
            .filter(|px| px[3] == 255)
            .all(|px| px[..3] == [255, 0, 0])
    );
}

#[test]
fn a_big_sprite_spans_its_footprint_and_cuts_into_cells() {
    let (p, pal) = pose("sprite a { \"O\" at 1.5 1.5 scale 3 }");
    let pic = draw(&p, 0, &pal, 0xffffff, (3, 3), 16);
    assert_eq!((pic.width, pic.height), (48, 48));
    let (x0, y0, x1, y1) = ink_box(&pic, 64).unwrap();
    assert!(
        x0 < 16 && x1 >= 32 && y0 < 16 && y1 >= 32,
        "{:?}",
        (x0, y0, x1, y1)
    );
    let mid = pic.cut(1, 0, 16);
    assert_eq!((mid.width, mid.height), (16, 16));
    assert_eq!(mid.at(3, 5), pic.at(16 + 3, 5));
    // The middle of an O is empty.
    assert_eq!(pic.cut(1, 1, 16).at(8, 8)[3], 0);
}

#[test]
fn spaces_empty_poses_and_tiny_cells_draw_nothing_and_never_panic() {
    let (p, pal) = pose("sprite a { \" \" at 0.5 0.5 }");
    assert!(
        draw(&p, 0, &pal, 1, (1, 1), 24)
            .rgba
            .iter()
            .all(|&b| b == 0)
    );
    let empty = Pose::default();
    assert_eq!(draw(&empty, 0, &[], 1, (2, 1), 8).rgba.len(), 16 * 8 * 4);
    let (p, pal) = pose("sprite a { \"@\" at 16 16 scale 4 turn -360 \"#\" at 0 0 scale 0.1 }");
    for cell in [1, 2, 3, 7] {
        let _ = draw(&p, 0, &pal, 1, (1, 1), cell);
    }
    let frames = Pose {
        frames: Frames {
            n: 16,
            every_ms: 50,
        },
        ..p
    };
    for f in 0..16 {
        let _ = draw(&frames, f, &pal, 1, (16, 16), 2);
    }
}
