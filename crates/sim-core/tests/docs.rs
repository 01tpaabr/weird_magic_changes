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
//!
//! Then the numbers and names the docs state, against the code: RULES §18's
//! limits table row by row, the defaults and ranges in the prose, the
//! reserved words, §16's vocabulary (parents, subs, traits, tags, scents),
//! and ACTORS.md's row records and sizes.

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

// ---- the numbers and names the docs state --------------------------------------------

/// Whitespace collapsed to single spaces, so a phrase matches across a
/// line break.
fn flat(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A doc's section from its heading (`## 18.`) to the next one.
fn section<'a>(md: &'a str, heading: &str) -> &'a str {
    let start = md
        .find(&format!("\n{heading}"))
        .unwrap_or_else(|| panic!("no section {heading}"));
    let rest = &md[start + 1..];
    &rest[..rest[3..].find("\n## ").map_or(rest.len(), |e| e + 3)]
}

/// Every phrase is in the text, whitespace aside.
fn says(file: &str, text: &str, phrases: &[String]) {
    let text = flat(text);
    let missing: Vec<&String> = phrases
        .iter()
        .filter(|p| !text.contains(p.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "{file} should say, as the code does:\n{}",
        missing
            .iter()
            .map(|p| format!("  {p}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
}

/// `21 600`: thousands apart, as the prose writes them.
fn spaced(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(c);
    }
    out
}

/// A kind that declares nothing: the defaults.
fn bare_kind() -> sim_core::KindDef {
    sim_core::rules::compile("t.rules", "kind k { }")
        .unwrap()
        .defs[0]
        .clone()
}

/// Ticks until a fresh 255 of scent is gone.
fn scent_lifetime() -> u64 {
    let (mut s, mut steps) = (255u8, 0);
    while s > 0 {
        s = sim_core::stage::scent::fade(s);
        steps += 1;
    }
    steps * sim_core::stage::scent::SCENT_CADENCE
}

/// RULES §18, row by row: the numbers before any `(` or `:` in the second
/// column are the code's limits. A row the test does not know fails, so a
/// new limit gets checked too.
#[test]
fn the_limits_table_is_the_codes() {
    use sim_core::actors::{GENE_SLOTS, MEM_SLOTS, NEED_SLOTS};
    use sim_core::rules::compile::*;
    use sim_core::rules::vm::{FRAME_LOCALS, FRAMES, STACK};
    use sim_core::scenario::MAX_SIZE_CHUNKS;
    let n = |v: usize| v as i64;
    let limits: Vec<(String, Vec<i64>)> = vec![
        (
            "needs, mem slots, genes per kind".into(),
            vec![n(NEED_SLOTS), n(MEM_SLOTS), n(GENE_SLOTS)],
        ),
        (
            "tags, scent channels per rule set".into(),
            vec![n(MAX_TAGS), n(sim_core::SCENT_CHANNELS)],
        ),
        ("states per kind".into(), vec![n(MAX_STATES)]),
        ("sight".into(), vec![i64::from(MAX_SIGHT)]),
        ("fuel per think".into(), vec![i64::from(MAX_FUEL)]),
        ("sub call depth".into(), vec![n(FRAMES)]),
        ("value stack per think".into(), vec![n(STACK)]),
        (
            "parameters and locals in one rule or sub".into(),
            vec![n(FRAME_LOCALS)],
        ),
        (
            "nesting in one rule or sub".into(),
            vec![i64::from(MAX_DEPTH)],
        ),
        (
            "compiled size of one rule, one state's rules, one sub".into(),
            vec![i64::from(i16::MAX)],
        ),
        (
            format!(
                "distinct constants outside {}..{} per rule set",
                i16::MIN,
                i16::MAX
            ),
            vec![n(MAX_POOL)],
        ),
        (
            "kinds, subs per rule set".into(),
            vec![n(MAX_KINDS), n(MAX_SUBS)],
        ),
        ("rules files per rule set".into(), vec![n(MAX_FILES)]),
        ("a chain of `extends`".into(), vec![i64::from(MAX_DEPTH)]),
        (
            "chunks a scenario's `size` generates at creation".into(),
            vec![MAX_SIZE_CHUNKS as i64],
        ),
        (
            "cells each way from (0, 0)".into(),
            vec![i64::from(sim_core::WORLD_EXTENT)],
        ),
    ];
    let md = doc("RULES.md");
    let mut seen = Vec::new();
    for row in section(&md, "## 18.")
        .lines()
        .filter(|l| l.starts_with("| "))
    {
        let cells: Vec<&str> = row
            .split(" | ")
            .map(|c| c.trim_matches(['|', ' ']))
            .collect();
        let (label, value) = (cells[0], cells[1]);
        if label.is_empty() {
            continue; // the header
        }
        let (_, want) = limits
            .iter()
            .find(|(l, _)| l == label)
            .unwrap_or_else(|| panic!("RULES.md §18: a row the test does not know: {label:?}"));
        let head = value.split(['(', ':']).next().unwrap();
        let got: Vec<i64> = head
            .split(|c: char| !c.is_ascii_digit())
            .filter(|w| !w.is_empty())
            .map(|w| w.parse().unwrap())
            .collect();
        assert_eq!(&got, want, "RULES.md §18: {label}");
        seen.push(label.to_string());
    }
    for (l, _) in &limits {
        assert!(seen.contains(l), "RULES.md §18 has no row {l:?}");
    }
}

/// Numbers the prose of RULES.md and ACTORS.md states, against the code.
#[test]
fn the_numbers_in_the_prose_are_the_codes() {
    use sim_core::rules::compile::{MAX_FUEL, MAX_SIGHT, MAX_TAGS};
    use sim_core::rules::vm::{FOR_EACH_LOCALS, FRAMES, SEARCH_DIV};
    use sim_core::scenario::{MAX_SIZE_CHUNKS, PLACE_ONE};
    use sim_core::stage::scent::SCENT_CADENCE;
    use sim_core::time::{TICKS_PER_DAY, TICKS_PER_HOUR, TICKS_PER_MINUTE};
    use sim_core::{CHUNK_CELLS, CHUNK_SIZE, SCENT_CHANNELS};
    let k = bare_kind();
    let s = Scenario::default();
    let p = &s.params;
    let side = (MAX_SIZE_CHUNKS as f64).sqrt() as i32 * CHUNK_SIZE;
    let lifetime = scent_lifetime();
    // Rounded to half hours, as "about 1.5 game hours".
    let half_hours = (lifetime * 2 + TICKS_PER_HOUR / 2) / TICKS_PER_HOUR;
    let hours = format!("{}", half_hours as f64 / 2.0);
    let r = i64::from(MAX_SIGHT);
    says(
        "RULES.md",
        &doc("RULES.md"),
        &[
            format!(
                "A day is {} ticks: {TICKS_PER_HOUR} an hour, {TICKS_PER_MINUTE} a minute",
                spaced(TICKS_PER_DAY)
            ),
            format!("| `glyph \"c\"` | `{}` |", char::from(k.glyph)),
            format!("at most {MAX_TAGS} tags in a rule set"),
            format!("| `cadence N` | {} |", k.cadence()),
            format!(
                "| `sight N` | {} | the largest radius any search reaches, 0 to {MAX_SIGHT} |",
                k.sight
            ),
            format!("| `fuel N` | {} | ops per think, 1 to {MAX_FUEL} |", k.fuel),
            format!("| `food T` | {} |", k.food),
            format!(
                "| `bite N` | {} | health taken per `eat`/`hit`/`graze`, 0 to {} |",
                k.bite,
                u8::MAX
            ),
            format!(
                "a counter, at most {} per kind",
                sim_core::actors::NEED_SLOTS
            ),
            format!(
                "memory slots, at most {} per kind",
                sim_core::actors::MEM_SLOTS
            ),
            format!(
                "own, at most {} per kind; read, never written",
                sim_core::actors::GENE_SLOTS
            ),
            format!("(at least {CHUNK_SIZE} cells each way, §11)"),
            format!(
                "Scent fades by 1/32 every {SCENT_CADENCE} ticks: gone in about {hours} game hours"
            ),
            format!("At most {SCENT_CHANNELS} channels in a rule set"),
            format!(
                "at least {CHUNK_SIZE} cells each way (up to {}, depending",
                2 * CHUNK_SIZE - 1
            ),
            format!("Calls nest up to {FRAMES} deep"),
            format!("a search costs `(2r + 1)² / {SEARCH_DIV}` more"),
            format!("radius {r} reads {}", (2 * r + 1).pow(2)),
            format!("| `seed N` | {} |", s.seed),
            format!(
                "| `size W H` | {} {} | the region generated at creation, rounded up to whole {CHUNK_SIZE}-cell chunks, at most {MAX_SIZE_CHUNKS} of them ({side} x {side} cells)",
                s.width, s.height
            ),
            format!("`water_scale` {} (lake size", p.water_scale),
            format!("`water_level` {:.2} (roughly", p.water_level),
            format!(
                "`rock_on_soil` {:.2}, `rock_on_water` {:.2}",
                p.rock_on_soil, p.rock_on_water
            ),
            format!(
                "A share is exact to one part in 2^{} (the smallest is 1 / {PLACE_ONE})",
                PLACE_ONE.trailing_zeros()
            ),
            format!("a `for each` {FOR_EACH_LOCALS}"),
            format!("{MAX_SIZE_CHUNKS} ({side} x {side} cells)"),
            format!(
                "x and y run from {} to {}. A position past",
                -sim_core::WORLD_EXTENT,
                sim_core::WORLD_EXTENT - 1
            ),
            format!(
                "(x and y from {} to {})",
                -sim_core::WORLD_EXTENT,
                sim_core::WORLD_EXTENT - 1
            ),
        ],
    );
    let actors = doc("ACTORS.md");
    let (row_pub, row_mind) = (
        size_of::<sim_core::ActorPub>(),
        size_of::<sim_core::ActorMind>(),
    );
    let (occupant, intent) = (
        size_of::<sim_core::ActorId>(),
        size_of::<sim_core::actors::Intent>(),
    );
    says(
        "ACTORS.md",
        &actors,
        &[
            format!("a {row_pub}-byte public record (`ActorPub`)"),
            format!("a {row_mind}-byte private record (`ActorMind`)"),
            format!(
                "Per actor: {} B persistent ({row_pub} + {row_mind} + {occupant} occupant) + {intent} B intent scratch",
                row_pub + row_mind + occupant
            ),
            format!("chunk file v{}", sim_core::store::FORMAT_VERSION),
            format!("`SCENT_CHANNELS` ({SCENT_CHANNELS})"),
            format!("up to 2 x {CHUNK_CELLS} rows"),
            format!("`sight <= {MAX_SIGHT}`"),
            format!("every {SCENT_CADENCE} ticks"),
            format!("gone in ~{hours} hours"),
            format!("recursion depth {FRAMES}"),
            format!("default {}, kind override up to {MAX_FUEL}", k.fuel),
            format!("({MAX_TAGS} at most"),
        ],
    );
}

/// The words inside code: every code span and fenced block of `md`.
fn code_words(md: &str) -> Vec<String> {
    let mut code = String::new();
    let mut fenced = false;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        } else if fenced {
            code.push_str(line);
            code.push(' ');
        } else {
            for (i, span) in line.split('`').enumerate() {
                if i % 2 == 1 {
                    code.push_str(span);
                    code.push(' ');
                }
            }
        }
    }
    code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

/// The backticked words of `text`, in order.
fn spans(text: &str) -> Vec<String> {
    flat(text)
        .split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

/// RULES §18's reserved words: every keyword of the compiler is somewhere
/// in the doc's code, the senses it lists are the VM's and reserved, and
/// the predicate words are what it says.
#[test]
fn the_reserved_words_are_the_compilers() {
    use sim_core::rules::compile::KEYWORDS;
    let md = doc("RULES.md");
    let words = code_words(&md);
    let absent: Vec<&&str> = KEYWORDS
        .iter()
        .filter(|k| !words.iter().any(|w| w == **k))
        .collect();
    assert!(
        absent.is_empty(),
        "RULES.md never shows the keywords {absent:?}"
    );
    let reserved = section(&md, "## 18.");
    let reserved = &reserved[reserved.find("Reserved words").unwrap()..];
    let list = |after: &str| -> Vec<String> {
        let from = &reserved[reserved.find(after).unwrap() + after.len()..];
        spans(&from[..from.find(')').unwrap()])
    };
    let is_reserved = |w: &str| {
        let e = sim_core::rules::compile("t.rules", &format!("kind k {{ mem {w} }}"));
        e.is_err_and(|e| e.msg.contains("reserved word"))
    };
    let senses = list("plus the sense names (");
    assert_eq!(
        senses.len(),
        sim_core::rules::vm::Sense::ALL.len(),
        "{senses:?}"
    );
    let functions = list("the built-in functions among them (");
    for w in senses.iter().chain(&functions).filter(|w| *w != "...") {
        assert!(is_reserved(w), "`{w}` is not reserved");
    }
    // Each sense also has a row in §10.
    let table = spans(section(&md, "## 10."));
    for s in &senses {
        assert!(
            table.iter().any(|t| t.split(", ").any(|t| t == s)),
            "§10 lacks `{s}`"
        );
    }
    for w in ["water", "soil", "rock", "bare", "food"] {
        assert!(!is_reserved(w), "`{w}` is reserved");
    }
    for w in ["water", "soil", "rock", "bare"] {
        assert!(sim_core::rules::compile("t.rules", &format!("kind {w} {{ }}")).is_err());
        assert!(sim_core::rules::compile("t.rules", &format!("kind k {{ tags {w} }}")).is_err());
    }
}

/// `mortal(5d, 6)` with its time literals in ticks, as the compiler
/// records a parent.
fn in_ticks(parent: &str) -> String {
    use sim_core::time::{TICKS_PER_DAY, TICKS_PER_HOUR, TICKS_PER_MINUTE};
    let Some((name, args)) = parent.split_once('(') else {
        return parent.to_string();
    };
    let args: Vec<String> = args
        .trim_end_matches(')')
        .split(", ")
        .map(|a| {
            let digits = a.find(|c: char| !c.is_ascii_digit()).unwrap_or(a.len());
            let (n, unit) = a.split_at(digits);
            let per = match unit {
                "" => 1,
                "min" => TICKS_PER_MINUTE,
                "h" => TICKS_PER_HOUR,
                "d" => TICKS_PER_DAY,
                _ => panic!("`{a}` is not a number or a time"),
            };
            (n.parse::<u64>().unwrap() * per).to_string()
        })
        .collect();
    format!("{name}({})", args.join(", "))
}

/// The built-in rules' items: `(word, name, text)`, split as
/// [`builtin_without`] does.
fn builtin_items() -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    for (_, text) in builtin::FILES {
        for line in text.lines() {
            if let Some(n) = item_name(line) {
                // The text starts with the rest of the header: `(t: target) {`.
                let word = line.split(' ').next().unwrap();
                let rest = &line[word.len() + 1 + n.len()..];
                out.push((word.into(), n.into(), format!("{rest}\n")));
            } else if let Some(last) = out.last_mut() {
                last.2.push_str(line);
                last.2.push('\n');
            }
        }
    }
    out
}

/// `mortal(5d, 6), fowl` split at its top-level commas.
fn parents(list: &str) -> Vec<&str> {
    let (mut out, mut depth, mut from) = (Vec::new(), 0, 0);
    for (i, c) in list.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => depth -= 1,
            ',' if depth == 0 => {
                out.push(list[from..i].trim());
                from = i + 1;
            }
            _ => {}
        }
    }
    out.push(list[from..].trim());
    out.retain(|p| !p.is_empty());
    out
}

/// A kind's or trait's tags with its parents' (`Tags add up`), from the
/// rules text.
fn tags_of(name: &str, items: &[(String, String, String)]) -> Vec<String> {
    let (_, _, text) = items.iter().find(|(_, n, _)| n == name).unwrap();
    let mut tags: Vec<String> = Vec::new();
    for line in text.lines() {
        if let Some(t) = line.trim().strip_prefix("tags ") {
            tags.extend(
                t.split('#')
                    .next()
                    .unwrap()
                    .split_whitespace()
                    .map(String::from),
            );
        }
    }
    let head = text.lines().next().unwrap_or("");
    if let Some((_, list)) = head.split_once(" extends ") {
        for p in parents(list.trim_end_matches(['{', ' '])) {
            tags.extend(tags_of(p.split('(').next().unwrap(), items));
        }
    }
    tags.sort();
    tags.dedup();
    tags
}

/// RULES §16 against the built-in rules: the reading list is each kind's
/// parents, the subs and traits are `lib.rules`' (and `fowl`), the tags
/// are carried by the kinds it names, the scents are the ones marked.
#[test]
fn the_vocabulary_is_the_built_in_rules() {
    let md = doc("RULES.md");
    let vocab = section(&md, "## 16.");
    let para = |head: &str| -> String {
        let from = &vocab[vocab
            .find(head)
            .unwrap_or_else(|| panic!("§16 lacks {head}"))..];
        from[..from.find("\n\n").unwrap_or(from.len())].to_string()
    };
    let kinds = Kinds::builtin();

    // The reading list: every built-in kind, with its parents as written.
    let list = spans(&para("The built-in kinds, as a reading list"));
    let mut named = Vec::new();
    for entry in &list {
        let (kind, list) = entry.split_once(" extends ").unwrap_or((entry, ""));
        let def = kinds
            .by_name(kind)
            .unwrap_or_else(|| panic!("§16 lists `{kind}`, not a built-in kind"));
        let want: Vec<String> = parents(list).into_iter().map(in_ticks).collect();
        assert_eq!(
            kinds.debug.parents[usize::from(def.id)],
            want,
            "§16: `{entry}`"
        );
        named.push(kind.to_string());
    }
    for k in kinds.names() {
        assert!(
            named.iter().any(|n| n == k),
            "§16's reading list lacks `{k}`"
        );
    }

    // Subs and traits, by signature (a sub's parameter types aside).
    let items = builtin_items();
    let signature = |word: &str, name: &str| -> String {
        let (_, _, text) = items
            .iter()
            .find(|(w, n, _)| w == word && n == name)
            .unwrap_or_else(|| {
                panic!("§16 names the {word} `{name}`: the built-in rules have none")
            });
        let head = text.lines().next().unwrap_or("");
        let params = head
            .strip_prefix('(')
            .map(|p| &p[..p.find(')').unwrap()])
            .map(|p| {
                p.split(", ")
                    .map(|a| a.split(':').next().unwrap().trim())
                    .collect::<Vec<_>>()
                    .join(", ")
            });
        match params {
            Some(p) => format!("{name}({p})"),
            None => name.to_string(),
        }
    };
    let subs: Vec<String> = spans(&para("**Subs**"))
        .into_iter()
        .filter(|s| s.contains('('))
        .collect();
    for s in &subs {
        assert_eq!(
            *s,
            signature("sub", s.split('(').next().unwrap()),
            "§16 subs"
        );
    }
    let file_subs = items.iter().filter(|(w, ..)| w == "sub").count();
    assert_eq!(subs.len(), file_subs, "§16 lists {subs:?}");
    let traits: Vec<String> = section(&md, "## 16.")
        .lines()
        .filter_map(|l| l.strip_prefix("| `"))
        .map(|l| l[..l.find('`').unwrap()].to_string())
        .collect();
    for t in &traits {
        assert_eq!(
            *t,
            signature("trait", t.split('(').next().unwrap()),
            "§16 traits"
        );
    }
    assert_eq!(
        traits.len(),
        kinds.debug.traits.len(),
        "§16 lists {traits:?}"
    );

    // Tags: `animal` (chickens, chicks, ...), `meat` (what foxes hunt: ...).
    let tags = para("**Tags.**");
    let tags = flat(&tags);
    for part in tags
        .split("), ")
        .map(|p| p.trim_start_matches("**Tags.** "))
    {
        let (tag, who) = part.split_once(" (").unwrap();
        let tag = tag.trim_matches('`');
        let who = who.trim_end_matches(").");
        let who = who.rsplit(": ").next().unwrap();
        let mut want: Vec<String> = who
            .split(',')
            .flat_map(|w| w.split(" and "))
            .map(|w| {
                let w = w.trim();
                let plural = [
                    w,
                    w.strip_suffix('s').unwrap_or(w),
                    w.strip_suffix("es").unwrap_or(w),
                ];
                plural
                    .into_iter()
                    .find(|w| kinds.by_name(w).is_some())
                    .unwrap_or_else(|| panic!("§16 tags: `{w}` is no kind"))
                    .to_string()
            })
            .collect();
        want.sort();
        let mut got: Vec<String> = kinds
            .names()
            .filter(|k| tags_of(k, &items).iter().any(|t| t == tag))
            .map(String::from)
            .collect();
        got.sort();
        assert_eq!(got, want, "§16: the kinds tagged `{tag}`");
    }

    // Scents.
    let scents: Vec<String> = spans(&para("**Scents.**"));
    assert_eq!(scents, kinds.scents, "§16 scents");
}

/// The fields of `struct NAME { ... }` in Rust text, in order.
fn fields(text: &str, name: &str) -> Vec<String> {
    let from = text
        .find(&format!("pub struct {name} {{"))
        .unwrap_or_else(|| panic!("no struct {name}"));
    let body = &text[from..];
    let body = &body[body.find('{').unwrap() + 1..body.find("\n}").unwrap()];
    body.lines()
        .map(|l| l.split("//").next().unwrap())
        .flat_map(|l| l.split(','))
        .filter_map(|f| f.split_once(':'))
        .map(|(n, _)| n.trim().trim_start_matches("pub ").to_string())
        .filter(|n| !n.is_empty() && !n.starts_with('#'))
        .collect()
}

/// ACTORS.md §2's row records are the code's, field by field.
#[test]
fn the_rows_in_the_actors_design_are_the_codes() {
    let md = doc("ACTORS.md");
    let rust = blocks("ACTORS.md", &md)
        .into_iter()
        .find(|b| b.info == "rust")
        .expect("ACTORS.md shows the rows in a rust block");
    let code = std::fs::read_to_string(repo().join("crates/sim-core/src/actors/mod.rs")).unwrap();
    for (name, n) in [("ActorPub", 7), ("ActorMind", 11)] {
        let want = fields(&code, name);
        assert_eq!(want.len(), n, "{name}: {want:?}");
        assert_eq!(fields(&rust.text, name), want, "{name}");
    }
}
