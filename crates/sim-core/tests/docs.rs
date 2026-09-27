//! The author docs are executable: every fenced block in `docs/RULES.md`
//! and `docs/ACTORS.md` says what it holds in its info string, and what it
//! holds must work (the convention is at the top of RULES.md).
//!
//! - ```` ```rules ````: compiled with the built-in rules, an item it
//!   declares (kind, trait, sub, const) replacing the built-in one of that
//!   name, and with no warning of its own. A block of bare rules is
//!   compiled as the rules of a kind that extends `drinker(6, 2h, 3)` with
//!   `sight 8`.
//! - ```` ```scenario ````: parses, fits the rules it names (else the
//!   built-in ones), and its `run`/`expect` lines pass. One whose first line
//!   names a file of the repo (`# scenarios/tests/x.scenario`) is that file,
//!   abridged: its lines, comments dropped, appear there in order.
//! - `error` after either word: it must fail on the line marked
//!   `# error: TEXT`, with a message containing TEXT.
//! - `sh`, `text`, `rust`, `ebnf`: not run. Anything else, a bare fence
//!   too, fails.

use std::path::{Path, PathBuf};

use sim_core::rules::{Kinds, Level, builtin, compile_files, compile_packs};
use sim_core::scenario::{Check, Placement, StartError};
use sim_core::{Scenario, sim};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn doc(name: &str) -> String {
    let p = repo().join("docs").join(name);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// A fenced block: where its fence opens, its info string, its text (the
/// fence's indentation taken off every line).
struct Block {
    at: String,
    info: String,
    text: String,
}

fn blocks(file: &str, md: &str) -> Vec<Block> {
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
            Some((n, ind, info, text)) if body.starts_with("```") => {
                assert_eq!(
                    indent,
                    ind,
                    "{file}:{}: a fence closes at another indent",
                    i + 1
                );
                out.push(Block {
                    at: format!("{file}:{n}"),
                    info,
                    text,
                });
            }
            Some((n, ind, info, mut text)) => {
                let cut = line.len() - line.trim_start().len();
                text.push_str(&line[cut.min(ind)..]);
                text.push('\n');
                open = Some((n, ind, info, text));
            }
        }
    }
    assert!(open.is_none(), "{file}: a fence is never closed");
    out
}

/// `# error: TEXT` in a block: its line (1-based) and TEXT.
fn marked_error(b: &Block) -> (u32, String) {
    let marks: Vec<(u32, String)> = b
        .text
        .lines()
        .enumerate()
        .filter_map(|(i, l)| {
            l.split_once("# error:")
                .map(|(_, t)| (i as u32 + 1, t.trim().to_string()))
        })
        .collect();
    assert_eq!(
        marks.len(),
        1,
        "{}: an `error` block marks one line with `# error: TEXT`",
        b.at
    );
    marks.into_iter().next().unwrap()
}

/// Items start at column 0 with one of these words; an item runs until the
/// next one (or the end of the file).
const ITEM_WORDS: [&str; 4] = ["kind", "trait", "sub", "const"];

/// The item a line at column 0 starts, by name.
fn item_name(line: &str) -> Option<&str> {
    let (word, rest) = line.split_once(' ')?;
    if !ITEM_WORDS.contains(&word) {
        return None;
    }
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The built-in rule files less the items named in `drop`.
fn builtin_without(drop: &[&str]) -> Vec<(String, String)> {
    builtin::FILES
        .iter()
        .map(|(name, text)| {
            let mut out = String::new();
            let mut keep = true;
            for line in text.lines() {
                if let Some(n) = item_name(line) {
                    keep = !drop.contains(&n);
                }
                // A dropped item's lines become blank: line numbers stay.
                if keep {
                    out.push_str(line);
                }
                out.push('\n');
            }
            (name.to_string(), out)
        })
        .collect()
}

/// A rules block's source as a file, and how many lines come before the
/// block's first line in it.
fn rules_source(b: &Block) -> (String, u32) {
    let first = b
        .text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("");
    if ITEM_WORDS.contains(&first.split(' ').next().unwrap_or("")) {
        (b.text.clone(), 0)
    } else {
        (
            format!(
                "kind doc_example extends drinker(6, 2h, 3) {{ sight 8\n{}}}\n",
                b.text
            ),
            1,
        )
    }
}

fn compile_block(b: &Block) -> Result<Kinds, sim_core::rules::CompileError> {
    let (src, _) = rules_source(b);
    let declared: Vec<&str> = src.lines().filter_map(item_name).collect();
    let mut files = builtin_without(&declared);
    files.push(("doc.rules".into(), src.clone()));
    let files: Vec<(&str, &str)> = files
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    compile_files(&files)
}

fn check_rules(b: &Block, error: bool) {
    let (_, offset) = rules_source(b);
    match (compile_block(b), error) {
        (Ok(kinds), false) => {
            let mine: Vec<String> = kinds
                .debug
                .diagnostics
                .iter()
                .filter(|d| d.level == Level::Warning && d.file == "doc.rules")
                .map(|d| d.to_string())
                .collect();
            assert!(mine.is_empty(), "{}: warnings:\n{}", b.at, mine.join("\n"));
        }
        (Err(e), false) => panic!("{}: does not compile: {e}\n{}", b.at, b.text),
        (Ok(_), true) => panic!("{}: an `error` block compiles", b.at),
        (Err(e), true) => {
            let (line, text) = marked_error(b);
            assert_eq!(e.file, "doc.rules", "{}: {e}", b.at);
            assert_eq!(
                e.line - offset,
                line,
                "{}: fails on another line: {e}",
                b.at
            );
            assert!(e.msg.contains(&text), "{}: fails otherwise: {e}", b.at);
        }
    }
}

/// The rules a scenario block names (relative to `scenarios/`, where its
/// file would be), else the built-in ones.
fn scenario_kinds(s: &Scenario, at: &str) -> Kinds {
    if s.packs.is_empty() {
        return Kinds::builtin();
    }
    let dir = repo().join("scenarios");
    let packs: Vec<PathBuf> = s.packs.iter().map(|p| dir.join(p)).collect();
    let packs: Vec<&Path> = packs.iter().map(PathBuf::as_path).collect();
    compile_packs(&packs).unwrap_or_else(|e| panic!("{at}: its rules: {e}"))
}

/// A scenario block's world and checks: `Err((line, message))` where it fails.
fn run_scenario(b: &Block) -> Result<(), (u32, String)> {
    let s = Scenario::parse("doc.scenario", &b.text).map_err(|e| (e.line, e.msg))?;
    let kinds = scenario_kinds(&s, &b.at);
    let line_of = |e: StartError| (s.line_of(&e).unwrap_or(0), e.msg);
    Placement::resolve(&s.starts, &kinds, &s.terrain()).map_err(line_of)?;
    for c in &s.checks {
        if let Check::Expect { line, what, .. } = c {
            sim::check_expect(&kinds, what).map_err(|e| (*line, e))?;
        }
    }
    if !s.checks.iter().any(|c| matches!(c, Check::Run(_))) {
        return Ok(());
    }
    let mut world = sim::new_world_with(&s, kinds).map_err(line_of)?;
    for c in &s.checks {
        match c {
            Check::Run(t) => (0..*t).for_each(|_| sim::step(&mut world)),
            Check::Expect { line, text, what } => match sim::expect(&mut world, what) {
                Ok((true, _)) => {}
                Ok((false, got)) => return Err((*line, format!("{text}  (found {got})"))),
                Err(e) => return Err((*line, e)),
            },
        }
    }
    Ok(())
}

/// Lines without comments or blanks, trimmed.
fn code_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.split('#').next().unwrap().trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// A block whose first line is `# path` naming a file of the repo is that
/// file, abridged: its code lines appear in the file's, in order. A path
/// no file has is an example's made-up name (`# scenarios/meadow.scenario`).
fn check_abridged(b: &Block) {
    let Some(path) = b.text.lines().next().and_then(|l| l.strip_prefix("# ")) else {
        return;
    };
    let path = repo().join(path.trim());
    let Ok(file) = std::fs::read_to_string(&path) else {
        return;
    };
    let file = code_lines(&file);
    let mut at = file.iter();
    for l in code_lines(&b.text) {
        assert!(
            at.any(|f| *f == l),
            "{}: `{l}` is not in {} (or not in this order)",
            b.at,
            path.display()
        );
    }
}

fn check_scenario(b: &Block, error: bool) {
    match (run_scenario(b), error) {
        (Ok(()), false) => check_abridged(b),
        (Err((line, e)), false) => panic!("{}: line {line}: {e}\n{}", b.at, b.text),
        (Ok(()), true) => panic!("{}: an `error` block works", b.at),
        (Err((line, e)), true) => {
            let (want_line, text) = marked_error(b);
            assert_eq!(line, want_line, "{}: fails on another line: {e}", b.at);
            assert!(e.contains(&text), "{}: fails otherwise: {e}", b.at);
        }
    }
}

fn check_md(file: &str, md: &str) -> usize {
    let mut checked = 0;
    for b in blocks(file, md) {
        let words: Vec<&str> = b.info.split_whitespace().collect();
        match words[..] {
            ["rules"] => check_rules(&b, false),
            ["rules", "error"] => check_rules(&b, true),
            ["scenario"] => check_scenario(&b, false),
            ["scenario", "error"] => check_scenario(&b, true),
            ["sh" | "text" | "rust" | "ebnf"] => continue,
            _ => panic!(
                "{}: a fence says what it holds: rules, scenario (either with `error`), sh, text, rust or ebnf; not {:?}",
                b.at, b.info
            ),
        }
        checked += 1;
    }
    checked
}

#[test]
fn every_example_in_the_rules_reference_works() {
    assert!(check_md("RULES.md", &doc("RULES.md")) >= 10);
}

#[test]
fn every_rules_example_in_the_actors_design_compiles() {
    assert!(check_md("ACTORS.md", &doc("ACTORS.md")) >= 1);
}

/// The harness itself: blocks are found at any indent, a fragment is
/// wrapped, an error block must fail where it says, and an unknown fence
/// fails.
#[test]
fn the_harness_classifies_and_checks_blocks() {
    let md = "a\n```rules\nkind k { glyph \"k\" }\n```\n  ```rules error\n  when true => { idle  idle }   # error: second action\n  ```\n```scenario error\nseed 1\nstart wolf 1 / 2   # error: do not define: wolf\n```\n";
    let bs = blocks("t.md", md);
    assert_eq!(bs.len(), 3);
    assert_eq!(bs[1].at, "t.md:5");
    check_rules(&bs[0], false);
    check_rules(&bs[1], true);
    check_scenario(&bs[2], true);
    let unknown = std::panic::catch_unwind(|| check_md("t.md", "```\nx\n```\n"));
    assert!(unknown.is_err());
}
