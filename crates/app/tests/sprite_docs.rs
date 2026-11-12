//! `docs/SPRITES.md` is executable, as `RULES.md` is (`sim-core/tests/docs.rs`):
//!
//! - ```` ```sprite ````: parses; one whose first line names a file of
//!   the repo (`# packs/crabs/crabs.sprite`) is that file, comments aside.
//! - ```` ```sprite error ````: fails on the line marked `# error: TEXT`,
//!   with a message containing TEXT.
//! - `ebnf`, `text`, `sh`, `rust`: not run. Anything else, a bare fence
//!   too, fails.
//!
//! Then the limits the prose states, against the code's constants.

use std::path::Path;

use app::sprite::{
    MAX_EVERY_MS, MAX_FRAMES, MAX_PALETTE, MAX_POSES, MAX_SCALE, MAX_STAMPS, MAX_SWING_TURN,
    MAX_TURN, MAX_XY, MIN_EVERY_MS, MIN_SCALE, milli_text, parse,
};

fn doc() -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/SPRITES.md");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Fenced blocks: (line of the opening fence, info string, text).
fn blocks(md: &str) -> Vec<(usize, String, String)> {
    let mut out = Vec::new();
    let mut open: Option<(usize, usize, String, String)> = None;
    for (i, line) in md.lines().enumerate() {
        let body = line.trim_start();
        let indent = line.len() - body.len();
        match open.take() {
            None => {
                if let Some(info) = body.strip_prefix("```") {
                    open = Some((i + 1, indent, info.trim().to_string(), String::new()));
                }
            }
            Some((n, _, info, text)) if body.starts_with("```") => out.push((n, info, text)),
            Some((n, ind, info, mut text)) => {
                text.push_str(&line[indent.min(ind)..]);
                text.push('\n');
                open = Some((n, ind, info, text));
            }
        }
    }
    assert!(open.is_none(), "SPRITES.md: a fence is never closed");
    out
}

/// A block whose first line names a file of the repo (`# packs/x/x.sprite`)
/// is that file: their lines, comments and blank lines dropped, are equal.
fn is_the_file(at: &str, file: &str, block: &str) {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(file);
    let real = std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{at}: {file}: {e}"));
    let code = |t: &str| -> Vec<String> {
        t.lines()
            .map(|l| {
                // A comment starts at a `#` outside a string.
                let mut quote = None;
                let end = l
                    .char_indices()
                    .find(|&(_, c)| match quote {
                        Some(q) if c == q => {
                            quote = None;
                            false
                        }
                        Some(_) => false,
                        None if c == '"' || c == '\'' => {
                            quote = Some(c);
                            false
                        }
                        None => c == '#',
                    })
                    .map_or(l.len(), |(i, _)| i);
                l[..end].trim_end().to_string()
            })
            .filter(|l| !l.trim().is_empty())
            .collect()
    };
    assert_eq!(code(block), code(&real), "{at}: the block is not {file}");
}

#[test]
fn every_sprite_block_parses_and_every_error_block_fails_where_it_says() {
    let md = doc();
    let (mut ok, mut errors) = (0, 0);
    for (at, info, text) in blocks(&md) {
        let at = format!("SPRITES.md:{at}");
        match info.as_str() {
            "sprite" => {
                parse("doc.sprite", &text).unwrap_or_else(|e| panic!("{at}: {e}\n{text}"));
                if let Some(file) = text.lines().next().and_then(|l| l.strip_prefix("# ")) {
                    is_the_file(&at, file, &text);
                }
                ok += 1;
            }
            "sprite error" => {
                let marks: Vec<(u32, &str)> = text
                    .lines()
                    .zip(1u32..)
                    .filter_map(|(l, i)| l.split_once("# error:").map(|(_, t)| (i, t.trim())))
                    .collect();
                assert_eq!(marks.len(), 1, "{at}: mark one line `# error: TEXT`");
                let (line, want) = marks[0];
                let e = parse("doc.sprite", &text)
                    .err()
                    .unwrap_or_else(|| panic!("{at}: parses, but should fail"));
                assert!(
                    e.line == line && e.msg.contains(want),
                    "{at}: wanted line {line} `{want}`, got {e}"
                );
                errors += 1;
            }
            "ebnf" | "text" | "sh" | "rust" => {}
            other => panic!(
                "{at}: a fence labelled {other:?}: use sprite, sprite error, ebnf, text, sh or rust"
            ),
        }
    }
    assert!(
        ok >= 1 && errors >= 2,
        "{ok} sprite blocks, {errors} error blocks"
    );
}

/// The numbers the prose states are the code's.
#[test]
fn the_limits_in_the_prose_are_the_codes() {
    let md = doc();
    let has = |s: &str| assert!(md.contains(s), "SPRITES.md does not say {s:?}");
    has(&format!("{MAX_STAMPS} stamps a pose"));
    has(&format!("{MAX_FRAMES} frames"));
    has(&format!("{MAX_POSES} poses a sprite"));
    has(&format!("{MAX_PALETTE} colours a palette"));
    has(&format!("{MIN_EVERY_MS}ms to {}s", MAX_EVERY_MS / 1000));
    has(&format!(
        "| `turn` | -{MAX_TURN} to {MAX_TURN}, whole degrees"
    ));
    has(&format!("| `~turn` | 0 to {MAX_SWING_TURN}, whole degrees"));
    let (lo, hi) = (
        milli_text(i64::from(MIN_SCALE)),
        milli_text(i64::from(MAX_SCALE)),
    );
    has(&format!("| `scale` | {lo} to {hi}, in cells"));
    has(&format!(
        "| `at X Y` | 0 to {}, in cells",
        milli_text(i64::from(MAX_XY))
    ));
}
