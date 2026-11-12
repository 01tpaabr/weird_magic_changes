//! Sprite text never panics: generated files ([`super::gen_sprites`]) parse
//! to what they say; mutated ones parse or are refused with a position
//! inside the text, and whatever parses writes back to itself and loads
//! against the built-in kinds. `make fuzz` runs many more cases.

use std::sync::LazyLock;

use proptest::prelude::*;
use proptest::sample::Index;

use super::gen_sprites::file;
use super::{Source, SpriteFile, load, parse};

static KINDS: LazyLock<sim_core::Kinds> = LazyLock::new(sim_core::Kinds::builtin);

/// `WMC_FUZZ_CASES` cases (`make fuzz`), else `cases`; from `WMC_FUZZ_SEED`,
/// else a fixed seed.
fn fuzz_config(cases: u32) -> ProptestConfig {
    let var = |name: &str| std::env::var(name).ok();
    let seed = var("WMC_FUZZ_SEED").and_then(|v| v.parse().ok());
    ProptestConfig {
        cases: var("WMC_FUZZ_CASES")
            .and_then(|v| v.parse().ok())
            .unwrap_or(cases),
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(seed.unwrap_or(0x5EED)),
        ..ProptestConfig::default()
    }
}

fn strip(mut f: SpriteFile) -> SpriteFile {
    for s in &mut f.sprites {
        (s.line, s.col) = (0, 0);
    }
    f
}

/// Bytes and words a mutation splices in: every token's first character,
/// the keywords, numbers at and past the limits, broken strings.
const SPLICE: &[&str] = &[
    "{",
    "}",
    "~",
    "\"",
    "'",
    "#",
    "-",
    ".",
    "\n",
    " ",
    "0",
    "palette",
    "sprite",
    "pose",
    "frames",
    "every",
    "at",
    "turn",
    "scale",
    "color",
    "phase",
    "~turn",
    "~x",
    "~y",
    "~scale",
    "16.001",
    "4.001",
    "0.099",
    "-0",
    "-361",
    "999999999",
    "1000000000",
    "0.0001",
    "49ms",
    "60001ms",
    "1.5ms",
    "60s",
    "3w",
    "\"#zzzzzz\"",
    "\"#fff\"",
    "\"ab\"",
    "\"\"",
    "''",
    "\u{e9}",
    "\0",
    "a.",
    "a..b",
    "255",
    "256",
    "17",
];

fn mutate(text: &str, edits: &[(Index, Index, u8)]) -> String {
    let mut b = text.as_bytes().to_vec();
    for (at, what, op) in edits {
        let i = if b.is_empty() {
            0
        } else {
            at.index(b.len() + 1)
        };
        match op % 4 {
            0 => {
                b.truncate(i);
            }
            1 => {
                let s = what.get(SPLICE).as_bytes();
                b.splice(i..i, s.iter().copied());
            }
            2 => {
                let end = (i + 1 + what.index(8)).min(b.len());
                if i < end {
                    b.drain(i..end);
                }
            }
            _ => {
                // A line said twice.
                let line_start = b[..i]
                    .iter()
                    .rposition(|&c| c == b'\n')
                    .map_or(0, |p| p + 1);
                let line_end = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |p| i + p);
                let line = b[line_start..line_end].to_vec();
                b.splice(line_end..line_end, std::iter::once(b'\n').chain(line));
            }
        }
    }
    // Splices are whole characters, cuts may split one: lossy is fine.
    String::from_utf8_lossy(&b).into_owned()
}

#[test]
fn generated_files_parse_to_what_they_say() {
    proptest!(fuzz_config(256), |(f in file())| {
        let text = f.text();
        let back = parse("g.sprite", &text).map_err(|e| TestCaseError::fail(format!("{e}\n{text}")))?;
        prop_assert_eq!(strip(back), f, "{}", text);
    });
}

#[test]
fn mutated_files_parse_or_say_where_and_never_panic() {
    proptest!(fuzz_config(512), |(f in file(), edits in prop::collection::vec((any::<Index>(), any::<Index>(), any::<u8>()), 1..6))| {
        let text = mutate(&f.text(), &edits);
        match parse("m.sprite", &text) {
            Ok(file) => {
                // Whatever parses writes back to itself, and loads.
                let again = parse("m.sprite", &file.text()).map_err(|e| TestCaseError::fail(format!("{e}\n{}", file.text())))?;
                prop_assert_eq!(strip(again), strip(file));
                let _ = load(&[Source { label: "m.sprite".into(), text: text.clone(), owner: None }], &KINDS);
            }
            Err(e) => {
                let lines = u32::try_from(text.split('\n').count()).unwrap();
                prop_assert!(e.line >= 1 && e.line <= lines, "{e} ({lines} lines)\n{text}");
                prop_assert!(e.col >= 1, "{e}");
                prop_assert!(e.to_string().starts_with("m.sprite:"), "{e}");
            }
        }
    });
}

#[test]
fn generated_files_load_against_the_built_in_kinds() {
    proptest!(fuzz_config(128), |(f in file(), owner in any::<bool>())| {
        let src = Source { label: "g.sprite".into(), text: f.text(), owner: owner.then(|| "red".into()) };
        let (w, _) = load(&[src], &KINDS).map_err(|e| TestCaseError::fail(e.to_string()))?;
        prop_assert_eq!(w.of_kind.len(), KINDS.defs.len());
        for i in w.of_kind.iter().flatten() {
            prop_assert!(usize::from(*i) < w.sprites.len());
        }
    });
}

/// Long and deep texts are refused or parsed quickly, without a stack
/// overflow (the grammar nests two deep).
#[test]
fn pathological_texts_are_quick() {
    let start = std::time::Instant::now();
    let many = "sprite a { pose 1 { ".repeat(10_000);
    assert!(parse("p", &many).is_err());
    let braces = "{".repeat(100_000);
    assert!(parse("p", &braces).is_err());
    let comments = "# x\n".repeat(100_000);
    assert!(parse("p", &comments).unwrap().sprites.is_empty());
    let stamps: String = (0..20_000)
        .map(|i| format!("sprite s{i} {{ \"x\" at 0.5 0.5 }}\n"))
        .collect();
    assert_eq!(parse("p", &stamps).unwrap().sprites.len(), 20_000);
    assert!(start.elapsed() < std::time::Duration::from_secs(10));
}
