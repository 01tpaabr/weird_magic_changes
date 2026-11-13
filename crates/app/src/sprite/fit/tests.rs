use super::*;
use crate::sprite::parse;

/// A `w x h` image on white, each pixel's colour from `f` (None: white).
fn image(w: usize, h: usize, f: impl Fn(f64, f64) -> Option<[u8; 3]>) -> Target {
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let (u, v) = ((x as f64 + 0.5) / w as f64, (y as f64 + 0.5) / h as f64);
            let [r, g, b] = f(u, v).unwrap_or([255, 255, 255]);
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Target::from_rgba(w, h, rgba, None)
}

/// A filled disc.
pub(crate) fn disc() -> Target {
    image(64, 64, |u, v| {
        ((u - 0.5).powi(2) + (v - 0.5).powi(2) < 0.16).then_some([200, 60, 40])
    })
}

/// A green triangle on a brown trunk.
pub(crate) fn tree() -> Target {
    image(64, 64, |u, v| {
        if v < 0.72 && (u - 0.5).abs() < v * 0.55 {
            Some([40, 140, 50])
        } else if v >= 0.72 && (u - 0.5).abs() < 0.08 {
            Some([110, 70, 30])
        } else {
            None
        }
    })
}

/// A red cap on a pale stem.
pub(crate) fn mushroom() -> Target {
    image(64, 64, |u, v| {
        if v < 0.5 && (u - 0.5).powi(2) / 0.2 + (v - 0.5).powi(2) / 0.16 < 1.0 {
            Some([210, 40, 40])
        } else if (0.5..0.9).contains(&v) && (u - 0.5).abs() < 0.12 {
            Some([235, 220, 190])
        } else {
            None
        }
    })
}

/// On three simple images a draft of up to 8 stamps beats the best single
/// character the same search finds, by a clear margin.
#[test]
fn a_draft_beats_a_single_character_on_three_images() {
    for (name, t) in [("disc", disc()), ("tree", tree()), ("mushroom", mushroom())] {
        let one = fit(
            &t,
            name,
            &Options {
                stamps: 1,
                ..Options::default()
            },
        );
        let many = fit(&t, name, &Options::default());
        let (a, b) = (one.score.shape, many.score.shape);
        eprintln!(
            "{name}: one stamp shape {a:.3}, {} stamps shape {b:.3} (value {:.3} -> {:.3})",
            many.score.stamps,
            one.score.value(),
            many.score.value()
        );
        assert!(b > a + 0.1, "{name}: {a:.3} -> {b:.3}");
        assert!(many.score.stamps <= 8 && many.score.stamps >= 2);
        // The score never fell as stamps were added.
        assert!(
            many.steps.windows(2).all(|w| w[1] >= w[0]),
            "{:?}",
            many.steps
        );
        // What it writes parses back to it.
        let text = many.file.text();
        let back = parse("fit.sprite", &text).unwrap();
        assert_eq!(back.sprites[0].poses, many.file.sprites[0].poses, "{text}");
    }
}

#[test]
fn the_same_image_and_seed_give_the_same_file() {
    let t = mushroom();
    let o = Options {
        stamps: 4,
        ..Options::default()
    };
    let a = fit(&t, "m", &o).file.text();
    assert_eq!(fit(&t, "m", &o).file.text(), a);
    let other = fit(&t, "m", &Options { seed: 2, ..o }).file.text();
    assert_ne!(other, a, "another seed, another draft");
}

#[test]
fn the_palette_is_the_subjects_colours_most_used_first() {
    let p = palette(&tree().fitted(48, 48), 2);
    assert_eq!(p.len(), 2);
    let (g, b) = (p[0], p[1]);
    assert!(g >> 8 & 0xff > g >> 16 & 0xff, "green first: {g:06x}");
    assert!(b >> 16 & 0xff > b & 0xff, "then brown: {b:06x}");
    // The disc's red comes first, whatever else the edges add.
    let d = palette(&disc().fitted(24, 24), 4);
    assert!(!d.is_empty() && d.len() <= 4);
    assert!(d[0] >> 16 & 0xff > 150 && d[0] & 0xff < 90, "{:06x}", d[0]);
    assert_eq!(palette(&Picture::new(4, 4), 3), vec![0x0080_8080]);
}
