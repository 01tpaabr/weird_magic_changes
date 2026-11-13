use super::*;
use crate::sprite::parse;

fn crab() -> (Pose, Vec<u32>) {
    let f = parse("crab", crate::sprite::tests::CRAB).unwrap();
    let pal = f.palette.iter().map(|(_, c)| *c).collect();
    (f.sprites[0].poses[0].clone(), pal)
}

/// A sprite against its own render: the same outline and colours, and
/// the same numbers every time.
#[test]
fn a_sprite_scores_one_against_its_own_render() {
    let (pose, pal) = crab();
    let own = draw(&pose, 0, &pal, 0x00ff_ffff, (1, 1), CELL);
    let target = Target::from_rgba(own.width, own.height, own.rgba.clone(), None);
    let s = score(&pose, &pal, 0x00ff_ffff, (1, 1), &target);
    assert_eq!(s.shape, 1.0);
    assert!(s.colour < 0.02, "{s:?}");
    assert_eq!(s.stamps, 5);
    // At 96 px the image is the 24 px one enlarged: close, not the same.
    assert!(s.detail_shape > 0.6 && s.detail_shape < 1.0, "{s:?}");
    assert_eq!(score(&pose, &pal, 0x00ff_ffff, (1, 1), &target), s);
    assert!(s.value() > 0.9);
}

#[test]
fn a_background_is_the_corners_colour_or_the_key() {
    // A red square on white, 10 x 10.
    let mut rgba = Vec::new();
    for y in 0..10 {
        for x in 0..10 {
            let inside = (3..7).contains(&x) && (3..7).contains(&y);
            rgba.extend_from_slice(if inside {
                &[255, 0, 0, 255]
            } else {
                &[250, 250, 250, 255]
            });
        }
    }
    let t = Target::from_rgba(10, 10, rgba.clone(), None);
    let solid = t.picture.rgba.chunks(4).filter(|p| p[3] == 255).count();
    assert_eq!(solid, 16);
    assert_eq!(t.key, Some(0x00fa_fafa));
    // Keyed on red instead: the white is the subject.
    let t = Target::from_rgba(10, 10, rgba, Some(0xff0000));
    assert_eq!(t.picture.rgba.chunks(4).filter(|p| p[3] == 255).count(), 84);
}

#[test]
fn an_image_is_fitted_with_its_aspect_kept() {
    // 20 x 10, all red: in a 24 x 24 box it is 24 x 12, rows 6 to 17.
    let t = Target::from_rgba(20, 10, [255u8, 0, 0, 200].repeat(200), None);
    let p = t.fitted(24, 24);
    let rows: Vec<usize> = (0..24).filter(|&y| p.at(12, y)[3] > 0).collect();
    assert_eq!(rows, (6..18).collect::<Vec<_>>());
    assert_eq!(p.at(0, 12), [255, 0, 0, 200]);
    assert_eq!(p.at(12, 2), [0, 0, 0, 0]);
}

#[test]
fn compare_counts_overlap_and_colour_where_both_are() {
    let mut a = Picture::new(2, 1);
    let mut b = Picture::new(2, 1);
    a.rgba.copy_from_slice(&[0, 0, 0, 255, 0, 0, 0, 0]);
    b.rgba.copy_from_slice(&[0, 0, 0, 0, 255, 255, 255, 255]);
    assert_eq!(compare(&a, &b), (0.0, 1.0));
    b.rgba[3] = 255;
    let (shape, colour) = compare(&a, &b);
    assert_eq!(shape, 0.5);
    assert_eq!(colour, 0.0);
    assert_eq!(
        compare(&Picture::new(2, 2), &Picture::new(2, 2)),
        (0.0, 1.0)
    );
}

#[test]
fn reach_is_the_cells_the_stamps_reach() {
    let f = parse(
        "t",
        "sprite a { \"x\" at 2.5 0.4 \"y\" at 0 1 pose 1 { \"z\" at 0 3 } }",
    )
    .unwrap();
    assert_eq!(reach(&f.sprites[0]), (3, 3));
    let f = parse("t", "sprite a { }").unwrap();
    assert_eq!(reach(&f.sprites[0]), (1, 1));
}

#[test]
fn the_diff_shows_target_sprite_and_difference() {
    let (pose, pal) = crab();
    let own = draw(&pose, 0, &pal, 0x00ff_ffff, (1, 1), CELL);
    let t = Target::from_rgba(own.width, own.height, own.rgba, None);
    let d = diff(&pose, &pal, 0x00ff_ffff, (1, 1), &t, 0x00c4_9e6c);
    assert_eq!((d.width, d.height), (96 * 3 + 24, 96 + 12));
}
