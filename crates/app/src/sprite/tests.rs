use super::*;

pub(crate) const CRAB: &str = r##"
palette {
  shell "#d9623b"
  eye   "#101010"
}

sprite crab {
  frames 4 every 200ms            # 4 frames, each shown 200 ms
  "c" at 0.30 0.55 turn 0   ~turn 20      # a claw that swings 20 degrees each way
  "c" at 0.70 0.55 turn 180 ~turn 20 phase 180
  "O" at 0.50 0.50 scale 0.8 color shell
  "." at 0.42 0.38 scale 0.5 color eye
  "." at 0.58 0.38 scale 0.5 color eye

  pose 1 {                        # drawn while the crab's `look` is 1
    "O" at 0.50 0.60 scale 0.6 color shell
  }
}
"##;

fn err(text: &str) -> String {
    parse("t.sprite", text).unwrap_err().to_string()
}

#[test]
fn the_crab_parses_to_what_it_says() {
    let f = parse("crab.sprite", CRAB).unwrap();
    assert_eq!(
        f.palette,
        vec![("shell".into(), 0xd9623b), ("eye".into(), 0x101010)]
    );
    let s = &f.sprites[0];
    assert_eq!((s.name.as_str(), s.line, s.col), ("crab", 7, 8));
    assert_eq!(s.poses.len(), 2);
    let own = &s.poses[0];
    assert_eq!(
        own.frames,
        Frames {
            n: 4,
            every_ms: 200
        }
    );
    assert_eq!(own.stamps.len(), 5);
    assert_eq!(
        own.stamps[1],
        Stamp {
            ch: b'c',
            x: 700,
            y: 550,
            turn: 180,
            swing: Swing {
                turn: 20,
                ..Swing::default()
            },
            phase: 180,
            ..Stamp::default()
        }
    );
    assert_eq!(own.stamps[2].color, Ink::Named(0));
    assert_eq!(own.stamps[3].scale, 500);
    assert_eq!(own.stamps[4].color, Ink::Named(1));
    // The pose takes the sprite's frames.
    let p = &s.poses[1];
    assert_eq!(p.look, Some(1));
    assert!(!p.own_frames);
    assert_eq!(p.frames, own.frames);
    assert_eq!(p.stamps[0].y, 600);
}

#[test]
fn text_parses_back_to_the_same_file() {
    let f = parse("crab.sprite", CRAB).unwrap();
    let back = parse("crab.sprite", &f.text()).unwrap();
    let strip = |mut f: SpriteFile| {
        for s in &mut f.sprites {
            (s.line, s.col) = (0, 0);
        }
        f
    };
    assert_eq!(strip(f), strip(back));
}

#[test]
fn numbers_units_and_strings() {
    let f = parse(
        "t",
        "sprite a { frames 2 every 1.5s\n'\"' at 1 0.005 turn -90 color \"#ABCDEF\" ~scale 0.25 }",
    )
    .unwrap();
    let p = &f.sprites[0].poses[0];
    assert_eq!(
        p.frames,
        Frames {
            n: 2,
            every_ms: 1500
        }
    );
    let s = p.stamps[0];
    assert_eq!((s.ch, s.x, s.y, s.turn), (b'"', 1000, 5, -90));
    assert_eq!(s.color, Ink::Rgb(0xabcdef));
    assert_eq!(s.swing.scale, 250);
    // A backslash is a stamp as it is: no escapes.
    let f = parse("t", r#"sprite a { "\" at 0.5 0.5 }"#).unwrap();
    assert_eq!(f.sprites[0].poses[0].stamps[0].ch, b'\\');
    // A namespaced kind.
    let f = parse("t", "sprite weather.cloud { }").unwrap();
    assert_eq!(f.sprites[0].name, "weather.cloud");
    assert_eq!(milli_text(-1250), "-1.25");
    assert_eq!(milli_text(300), "0.3");
    assert_eq!(milli_text(4000), "4");
}

#[test]
fn bad_files_say_where() {
    let mut bad = Vec::new();
    for (text, want) in [
        (
            "sprite a {",
            "t.sprite:1:10: this sprite's `{` is never closed",
        ),
        (
            "sprite a { \"ab\" at 0 0 }",
            "t.sprite:1:12: \"ab\": a stamp is one printable ASCII character",
        ),
        (
            "sprite a {\n  \"x\" at 0.5 17 }",
            "t.sprite:2:14: y must be from 0 to 16",
        ),
        (
            "sprite a { \"x\" at 0.5 0.5 scale 5 }",
            "t.sprite:1:33: scale must be from 0.1 to 4",
        ),
        (
            "sprite a { \"x\" at 0.5 0.5 turn 1.5 }",
            "t.sprite:1:32: turn is a whole number",
        ),
        (
            "sprite a { \"x\" at 0.5 0.5 turn 1 turn 2 }",
            "t.sprite:1:34: `turn` given twice",
        ),
        (
            "sprite a { \"x\" at 0.5 0.5 ~spin 3 }",
            "t.sprite:1:28: `~spin`: a swing is",
        ),
        (
            "sprite a { \"x\" at 0.5 0.5 color red }",
            "t.sprite:1:33: no colour `red` in this file's palette",
        ),
        (
            "sprite a { \"x\" at 0.5 0.5 color \"red\" }",
            "t.sprite:1:33: \"red\" is not a colour",
        ),
        (
            "sprite a { frames 4 }",
            "t.sprite:1:12: `frames 4` needs `every T`",
        ),
        (
            "sprite a { frames 2 every 10ms }",
            "t.sprite:1:27: `every` must be from 50ms to 60s",
        ),
        (
            "sprite a { frames 2 every 200 }",
            "t.sprite:1:27: `every` takes a time with its unit",
        ),
        (
            "sprite a { frames 17 every 1s }",
            "t.sprite:1:19: the number of frames must be from 1 to 16",
        ),
        (
            "sprite a { frames 1 frames 1 }",
            "t.sprite:1:21: `frames` given twice",
        ),
        (
            "sprite a { pose 1 { } pose 1 { } }",
            "t.sprite:1:23: pose 1 is drawn twice",
        ),
        (
            "sprite a { pose 1 { pose 2 { } } }",
            "t.sprite:1:21: a pose holds no pose",
        ),
        (
            "sprite a { pose 256 { } }",
            "t.sprite:1:17: a pose's look must be from 0 to 255",
        ),
        (
            "sprite a { } sprite a { }",
            "t.sprite:1:21: sprite `a` is drawn twice",
        ),
        (
            "palette { a \"#000000\" a \"#111111\" }",
            "t.sprite:1:23: colour `a` is named twice",
        ),
        (
            "kind a { }",
            "t.sprite:1:1: expected `palette` or `sprite`, found `kind`",
        ),
        (
            "sprite a { \"x\" at 3w 0 }",
            "t.sprite:1:19: unknown unit `w` (ms, s)",
        ),
        (
            "sprite a { \"x\" at 0.1234 0 }",
            "t.sprite:1:19: at most 3 decimals",
        ),
        (
            "sprite a { \"x\" at 1. 0 }",
            "t.sprite:1:19: a decimal point needs digits",
        ),
        ("sprite a { \"x }", "t.sprite:1:12: unterminated string"),
        ("sprite a { ! }", "t.sprite:1:12: unexpected character `!`"),
        (
            "sprite a { \"x\" at - }",
            "t.sprite:1:19: a `-` must start a number",
        ),
        ("sprite a. { }", "t.sprite:1:8: bad name `a.`"),
        (
            "sprite a { \"x\" at 99999999999 0 }",
            "t.sprite:1:19: number too large",
        ),
        ("sprite a { \"x\" 0 0 }", "t.sprite:1:16: expected `at X Y`"),
        (
            "sprite a { \u{e9} }",
            "t.sprite:1:12: unexpected character `\u{e9}`",
        ),
    ] {
        let e = err(text);
        if !e.starts_with(want) {
            bad.push(format!("{text:?}\n got: {e}\nwant: {want}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

#[test]
fn limits_are_checked_with_their_counts() {
    let stamps = "\"x\" at 0 0\n".repeat(MAX_STAMPS + 1);
    let e = err(&format!("sprite a {{\n{stamps}}}"));
    assert!(
        e.contains(&format!("at most {MAX_STAMPS} stamps a pose")),
        "{e}"
    );
    assert!(
        e.starts_with(&format!("t.sprite:{}:1:", MAX_STAMPS + 2)),
        "{e}"
    );
    let poses: String = (0..MAX_POSES)
        .map(|v| format!("pose {v} {{ }}\n"))
        .collect();
    let e = err(&format!("sprite a {{\n{poses}}}"));
    assert!(
        e.contains("at most 8 poses a sprite") && e.contains("has 9"),
        "{e}"
    );
    let pal: String = (0..=MAX_PALETTE)
        .map(|i| format!("c{i} \"#000000\"\n"))
        .collect();
    let e = err(&format!("palette {{\n{pal}}}"));
    assert!(e.contains("at most 64 colours"), "{e}");
}

fn source(label: &str, text: &str, owner: Option<&str>) -> Source {
    Source {
        label: label.into(),
        text: text.into(),
        owner: owner.map(str::to_string),
    }
}

#[test]
fn sprites_match_kinds_children_wear_their_parents() {
    let kinds = Kinds::builtin();
    let fox = kinds.by_name("fox").unwrap().id;
    let text = "sprite fox { \"f\" at 0.5 0.5 }\nsprite dragon { \"d\" at 0.5 0.5 }\nsprite chicken { frames 2 every 1s \"c\" at 3 0.5 pose 1 { } }";
    let (w, warn) = load(&[source("a.sprite", text, None)], &kinds).unwrap();
    assert_eq!(w.sprites.len(), 2);
    assert_eq!(w.of_kind[usize::from(fox)], Some(0));
    // The chicken's 2 poses of 2 frames, the fox's 1: one cell each.
    assert_eq!(w.tiles(), 5);
    assert_eq!(warn.len(), 2, "{warn:?}");
    assert!(
        warn[0]
            .to_string()
            .starts_with("a.sprite:2:8: warning: sprite `dragon`: no loaded pack"),
        "{}",
        warn[0]
    );
    assert!(
        warn[1]
            .to_string()
            .contains("centred outside the kind's 1 x 1 cells"),
        "{}",
        warn[1]
    );
    // Every child of the fox, if any, wears its sprite.
    for d in &kinds.defs {
        let mut p = d.parent;
        while let Some(k) = p {
            if k == fox {
                assert_eq!(w.of_kind[usize::from(d.id)], Some(0), "{}", d.name);
            }
            p = kinds.def(k).parent;
        }
    }
    // Two sprites for one kind.
    let e = load(
        &[
            source("a.sprite", "sprite fox { }", None),
            source("b.sprite", "\nsprite fox { }", None),
        ],
        &kinds,
    )
    .unwrap_err();
    assert_eq!(
        e.to_string(),
        "b.sprite:2:8: kind `fox` has a sprite already (a.sprite:1)"
    );
    // A player's pack names its own kinds.
    let (w, warn) = load(&[source("r.sprite", "sprite fox { }", Some("red"))], &kinds).unwrap();
    assert!(w.is_empty());
    assert!(warn[0].msg.contains("`red.fox`"), "{warn:?}");
}

#[test]
fn a_child_wears_its_parents_sprite() {
    let kinds = sim_core::rules::compile_files(&[(
        "t.rules",
        "kind a { glyph \"a\" }\nkind b extends a { glyph \"b\" }\nkind c extends b { glyph \"c\" }\nkind d { glyph \"d\" }",
    )])
    .unwrap();
    let (w, _) = load(
        &[source("t.sprite", "sprite a { \"A\" at 0.5 0.5 }", None)],
        &kinds,
    )
    .unwrap();
    let id = |n: &str| usize::from(kinds.by_name(n).unwrap().id);
    assert_eq!(w.of_kind[id("a")], Some(0));
    assert_eq!(w.of_kind[id("b")], Some(0));
    assert_eq!(w.of_kind[id("c")], Some(0));
    assert_eq!(w.of_kind[id("d")], None);
    // The nearest one wins.
    let (w, _) = load(
        &[source("t.sprite", "sprite a { }\nsprite b { }", None)],
        &kinds,
    )
    .unwrap();
    assert_eq!(w.of_kind[id("c")], Some(1));
}

#[test]
fn sources_read_a_packs_sprite_files_in_name_order() {
    let dir = std::env::temp_dir().join(format!("wmc-sprite-src-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("p")).unwrap();
    for (f, t) in [
        ("p/b.sprite", "# b"),
        ("p/a.sprite", "# a"),
        ("p/x.rules", ""),
        ("one.rules", ""),
        ("one.sprite", "# one"),
    ] {
        std::fs::write(dir.join(f), t).unwrap();
    }
    let p = dir.join("p").to_string_lossy().into_owned();
    let s = sources(std::slice::from_ref(&p)).unwrap();
    let got: Vec<(&str, &str)> = s
        .iter()
        .map(|s| (s.label.as_str(), s.text.as_str()))
        .collect();
    assert_eq!(got, [("a.sprite", "# a"), ("b.sprite", "# b")]);
    let one = dir.join("one.rules").to_string_lossy().into_owned();
    let s = sources(&[format!("red={p}"), one]).unwrap();
    let top = dir.file_name().unwrap().to_str().unwrap();
    let got: Vec<(String, Option<&str>)> = s
        .iter()
        .map(|s| (s.label.clone(), s.owner.as_deref()))
        .collect();
    let want: Vec<(String, Option<&str>)> = vec![
        ("p/a.sprite".into(), Some("red")),
        ("p/b.sprite".into(), Some("red")),
        (format!("{top}/one.sprite"), None),
    ];
    assert_eq!(got, want);
    std::fs::remove_dir_all(&dir).unwrap();
}
