//! Scenario text never panics (`docs/RULES.md` §14): a generator of valid
//! scenarios over the built-in kinds, and a mutator that breaks them line
//! by line and token by token. A valid one parses; whatever parses, its
//! starts resolve or are refused, and a small world of it is made (or
//! refused) and its `run` and `expect` lines run, without a panic.
//! `make fuzz` runs many more cases.

use std::sync::LazyLock;

use proptest::prelude::*;
use proptest::sample::Index;

use super::*;
use crate::sim;
use crate::time::{TICKS_PER_DAY, TICKS_PER_HOUR, TICKS_PER_MINUTE};

static KINDS: LazyLock<Kinds> = LazyLock::new(Kinds::builtin);

/// A value as a scenario writes it: plain, or in the largest whole unit.
fn time_text(v: i64, unit: bool) -> String {
    for (per, u) in [
        (TICKS_PER_DAY, "d"),
        (TICKS_PER_HOUR, "h"),
        (TICKS_PER_MINUTE, "min"),
    ] {
        let per = per as i64;
        if unit && v != 0 && v % per == 0 {
            return format!("{}{u}", v / per);
        }
    }
    v.to_string()
}

/// A kind, and `with (...)` setting some of its needs (in range) and mems.
fn kind_with() -> impl Strategy<Value = String> {
    (
        any::<Index>(),
        prop::collection::vec((any::<Index>(), any::<u32>(), any::<bool>()), 0..3),
    )
        .prop_map(|(k, sets)| {
            let d = k.get(&KINDS.defs);
            let names: Vec<(&str, i64)> = d
                .needs
                .iter()
                .map(|n| (n.name.as_str(), i64::from(n.max)))
                .chain(d.mems.iter().map(|m| (m.as_str(), 0)))
                .collect();
            let mut with: Vec<String> = Vec::new();
            let mut used: Vec<&str> = Vec::new();
            for (i, raw, unit) in sets {
                let Some(&(name, max)) = (!names.is_empty()).then(|| i.get(&names)) else {
                    break;
                };
                if used.contains(&name) {
                    continue;
                }
                used.push(name);
                // A need in `0..=max`; a mem anything small.
                let v = if max > 0 {
                    i64::from(raw) % (max + 1)
                } else {
                    i64::from(raw % 2001) - 1000
                };
                with.push(format!("{name} = {}", time_text(v, unit)));
            }
            if with.is_empty() {
                d.name.clone()
            } else {
                format!("{} with ({})", d.name, with.join(", "))
            }
        })
}

/// `terrain` with some of its fields, in any order.
fn terrain() -> impl Strategy<Value = String> {
    let fields = prop::sample::subsequence(vec![0usize, 1, 2, 3], 1..=4).prop_shuffle();
    (fields, 0.5f32..40.0, -0.2f32..1.2, 0.0f32..0.3, 0.0f32..0.3).prop_map(
        |(fields, scale, level, soil, water)| {
            let mut line = "terrain".to_string();
            for f in fields {
                line += &match f {
                    0 => format!(" water_scale {scale}"),
                    1 => format!(" water_level {level}"),
                    2 => format!(" rock_on_soil {soil}"),
                    _ => format!(" rock_on_water {water}"),
                };
            }
            line
        },
    )
}

/// Share lines, one per kind, each at most 1/8 so they add up to at most 1.
fn shares() -> impl Strategy<Value = Vec<String>> {
    prop::collection::vec((any::<Index>(), 1u32..=250, 8u32..=2000), 0..5).prop_map(|v| {
        let mut kinds: Vec<&str> = Vec::new();
        let mut lines = Vec::new();
        for (k, num, den) in v {
            let kind = k.get(&KINDS.defs).name.as_str();
            if kinds.contains(&kind) {
                continue;
            }
            kinds.push(kind);
            let num = 1 + num % (den / 8);
            lines.push(format!("start {kind} {num} / {den}"));
        }
        lines
    })
}

/// `start K at (x, y) [with (...)]`, with either spacing.
fn at_start() -> impl Strategy<Value = String> {
    (kind_with(), -4i32..130, -4i32..130, any::<bool>()).prop_map(|(k, x, y, tight)| {
        let (kind, with) = k.split_once(" with ").unwrap_or((&k, ""));
        let at = if tight {
            format!("at({x},{y})")
        } else {
            format!("at ( {x} , {y} )")
        };
        match with {
            "" => format!("start {kind} {at}"),
            w => format!("start {kind} {at} with {w}"),
        }
    })
}

/// Legend keys: terrain, then kinds.
const KEYS: &[u8] = b".~#,ABCx";

/// A drawn map, its legend (only the keys it uses), maybe `outside`, and
/// how many starts it draws.
fn map() -> impl Strategy<Value = (Vec<String>, usize, (u32, u32))> {
    (1usize..=20, 1usize..=10)
        .prop_flat_map(|(w, h)| {
            (
                Just((w, h)),
                prop::collection::vec(0..KEYS.len(), w * h),
                prop::collection::vec(kind_with(), 4),
                prop::option::of(0usize..4),
            )
        })
        .prop_map(|((w, h), cells, kinds, outside)| {
            let mut lines = vec!["map {".to_string()];
            let mut drawn = 0;
            for row in cells.chunks(w) {
                lines.push(format!(
                    "  {}",
                    row.iter().map(|&k| KEYS[k] as char).collect::<String>()
                ));
                drawn += row.iter().filter(|&&k| k >= 4).count();
            }
            lines.push("}".into());
            lines.push("legend {".into());
            for (k, &key) in KEYS.iter().enumerate() {
                if !cells.contains(&k) {
                    continue;
                }
                let what = match k {
                    0 | 3 => "soil".to_string(),
                    1 => "water".into(),
                    2 => "rock".into(),
                    _ => kinds[k - 4].clone(),
                };
                lines.push(format!("  {} {what}", key as char));
            }
            lines.push("}".into());
            if let Some(o) = outside {
                lines.push(format!("outside {}", ["noise", "soil", "rock", "water"][o]));
            }
            (lines, drawn, (w as u32, h as u32))
        })
}

/// A `run` or `expect` line.
fn check() -> impl Strategy<Value = String> {
    let ops = ["==", "!=", "<", "<=", ">", ">="];
    let counters = [
        "count", "born", "became", "eaten", "died", "thinks", "traps",
    ];
    (
        0u8..5,
        any::<Index>(),
        any::<Index>(),
        any::<Index>(),
        any::<bool>(),
        -5i64..50,
        any::<u64>(),
    )
        .prop_map(move |(form, k, a, b, only, n, hex)| {
            let d = k.get(&KINDS.defs);
            let who = if only {
                format!("only {}", d.name)
            } else {
                d.name.clone()
            };
            let op = a.get(&ops);
            match form {
                0 => format!("run {}", time_text(n.clamp(1, 3), false)),
                1 => format!("expect {} {who} {op} {n}", b.get(&counters)),
                2 => {
                    let names: Vec<&str> = d
                        .needs
                        .iter()
                        .map(|n| n.name.as_str())
                        .chain(d.mems.iter().map(String::as_str))
                        .collect();
                    let name = if names.is_empty() {
                        "food"
                    } else {
                        b.get(&names)
                    };
                    let agg = ["min", "max", "sum", "mean"][b.index(4)];
                    format!("expect {agg} {name} of {who} {op} {}", time_text(n, only))
                }
                3 if n % 2 == 0 => format!("expect at ({n}, {}) nobody", n / 2),
                3 => format!("expect at({n},{n}) {who}"),
                _ => format!("expect {} {hex:x}", ["checksum", "state"][a.index(2)]),
            }
        })
}

/// A valid scenario, and what it holds.
#[derive(Debug, Clone)]
struct Valid {
    text: String,
    seed: u64,
    starts: usize,
    checks: usize,
}

fn valid() -> impl Strategy<Value = Valid> {
    (
        any::<u64>(),
        prop::option::of(terrain()),
        shares(),
        prop::collection::vec(at_start(), 0..4),
        prop::option::of(map()),
        (any::<bool>(), 0u32..60, 0u32..60, 1u32..=128, 1u32..=128),
        prop::collection::vec(check(), 0..4),
        any::<bool>(),
        prop::collection::vec(0u8..4, 16),
        prop::option::of((1u32..=8, 1u32..=8)),
    )
        .prop_flat_map(
            |(seed, terrain, shares, ats, map, size, checks, rules, deco, mutation)| {
                let mut blocks: Vec<Vec<String>> = vec![vec![format!("seed {seed}")]];
                blocks.extend(terrain.map(|t| vec![t]));
                blocks.extend(mutation.map(|(n, d)| vec![format!("mutation {} / {d}", n.min(d))]));
                let starts = shares.len() + ats.len() + map.as_ref().map_or(0, |m| m.1);
                // A size fits the map; without one the map sets it.
                let (with_size, dw, dh, w, h) = size;
                match &map {
                    Some((_, _, (mw, mh))) if with_size => {
                        blocks.push(vec![format!(
                            "size {} {}",
                            (mw + dw).min(128),
                            (mh + dh).min(128)
                        )]);
                    }
                    Some(_) => {}
                    None => blocks.push(vec![format!("size {w} {h}")]),
                }
                if let Some((lines, _, _)) = map {
                    // `map` and `legend` are blocks; `outside` may go anywhere.
                    let legend = lines.iter().position(|l| l == "legend {").unwrap();
                    let close = lines.iter().rposition(|l| l == "}").unwrap();
                    blocks.push(lines[..legend].to_vec());
                    blocks.push(lines[legend..=close].to_vec());
                    blocks.extend(lines[close + 1..].iter().map(|l| vec![l.clone()]));
                }
                blocks.extend(shares.into_iter().chain(ats).map(|l| vec![l]));
                if rules {
                    blocks.push(vec!["rules packs/one ../two".into()]);
                }
                let n_checks = checks.len();
                // Starts and checks keep their order; the rest goes anywhere.
                let fixed = checks.into_iter().map(|l| vec![l]);
                (
                    Just(seed),
                    Just(blocks).prop_shuffle(),
                    Just(fixed.collect::<Vec<_>>()),
                    Just(starts),
                    Just(n_checks),
                    Just(deco),
                )
            },
        )
        .prop_map(|(seed, blocks, checks, starts, n_checks, deco)| {
            let mut text = String::new();
            for (i, block) in blocks.iter().chain(&checks).enumerate() {
                let d = deco[i % deco.len()];
                if d == 1 {
                    text += "\n# a comment\n";
                }
                for (j, line) in block.iter().enumerate() {
                    let indent = if d == 2 { "   " } else { "" };
                    text += indent;
                    text += line;
                    // No comments inside a map or a legend.
                    if d == 3 && block.len() == 1 && j == 0 {
                        text += "   # note";
                    }
                    text += "\n";
                }
            }
            Valid {
                text,
                seed,
                starts,
                checks: n_checks,
            }
        })
}

/// One way to break a scenario's text.
#[derive(Debug, Clone)]
enum Mutation {
    DeleteLine(Index),
    DupLine(Index, Index),
    SwapLines(Index, Index),
    DeleteToken(Index, Index),
    DupToken(Index, Index),
    SwapTokens(Index, Index, Index),
    /// A number token (else any) becomes an extreme.
    Number(Index, Index, Index),
    /// A time suffix, good or bad, after a token.
    Suffix(Index, Index, Index),
    /// Something odd inside a line, at a character.
    Insert(Index, Index, Index),
    /// A character of a line gone: a ragged map row, a broken word.
    DeleteChar(Index, Index),
    /// An odd line of its own.
    Line(Index, Index),
    Bom,
    Truncate(Index),
}

const EXTREMES: &[&str] = &[
    "0",
    "-0",
    "-1",
    "1",
    "255",
    "65536",
    "16777216",
    "2147483647",
    "2147483648",
    "-2147483648",
    "-2147483649",
    "4294967295",
    "4294967296",
    "9223372036854775807",
    "18446744073709551615",
    "18446744073709551616",
    "999999999999999999999999999",
    "4096",
    "4097",
    "65",
    "1e9",
    "1e-45",
    "3.4e38",
    "NaN",
    "inf",
    "-inf",
    "0.0",
    "-0.5",
    "0x10",
];

const SUFFIXES: &[&str] = &["min", "h", "d", "s", "ms", "hh", "D", "é", "d2"];

const ODD: &[&str] = &[
    "é", "日本", "\u{feff}", "\u{0}", "🦊", "e\u{301}", "\t", "\r", "{", "}", "(", ")", ",", "=",
    "/", "#", "-", "--", "∞", "\u{202e}", " ", "at", "with", "only", "of",
];

const LINES: &[&str] = &[
    "map {",
    "legend {",
    "}",
    "{",
    "start",
    "start fox",
    "start fox at",
    "start fox at (1, 2) with",
    "start fox at (1, 2) with ()",
    "start fox at (1, 2) with (food =)",
    "start fox 1 /",
    "start fox / 2",
    "start fox 0 / 0",
    "start 1fox 1 / 2",
    "size",
    "size 1",
    "size 0 0",
    "size 4096 4096",
    "size 4097 64",
    "seed",
    "seed -1",
    "terrain",
    "terrain water_scale",
    "terrain water_scale 0",
    "outside",
    "outside lava",
    "rules",
    "mutation",
    "mutation 0 / 1",
    "mutation 2 / 1",
    "mutation 1 /",
    "run",
    "run 0",
    "run -1d",
    "expect",
    "expect at",
    "expect at (1,",
    "expect count",
    "expect min of fox > 1",
    "expect checksum zz",
    "  ~.#",
    "é soil",
    "\u{feff}seed 1",
];

fn mutation() -> impl Strategy<Value = Mutation> {
    let i = any::<Index>;
    prop_oneof![
        i().prop_map(Mutation::DeleteLine),
        (i(), i()).prop_map(|(a, b)| Mutation::DupLine(a, b)),
        (i(), i()).prop_map(|(a, b)| Mutation::SwapLines(a, b)),
        (i(), i()).prop_map(|(a, b)| Mutation::DeleteToken(a, b)),
        (i(), i()).prop_map(|(a, b)| Mutation::DupToken(a, b)),
        (i(), i(), i()).prop_map(|(a, b, c)| Mutation::SwapTokens(a, b, c)),
        (i(), i(), i()).prop_map(|(a, b, c)| Mutation::Number(a, b, c)),
        (i(), i(), i()).prop_map(|(a, b, c)| Mutation::Suffix(a, b, c)),
        (i(), i(), i()).prop_map(|(a, b, c)| Mutation::Insert(a, b, c)),
        (i(), i()).prop_map(|(a, b)| Mutation::DeleteChar(a, b)),
        (i(), i()).prop_map(|(a, b)| Mutation::Line(a, b)),
        Just(Mutation::Bom),
        i().prop_map(Mutation::Truncate),
    ]
}

/// The byte offset of character `at` of `s` (its length past the last).
fn char_at(s: &str, at: &Index) -> usize {
    let n = s.chars().count();
    s.char_indices()
        .map(|(b, _)| b)
        .nth(at.index(n + 1))
        .unwrap_or(s.len())
}

fn mutate(text: &str, ms: &[Mutation]) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    for m in ms {
        if lines.is_empty() {
            lines.push(String::new());
        }
        let n = lines.len();
        let tokens =
            |l: &String| -> Vec<String> { l.split_whitespace().map(str::to_string).collect() };
        match m {
            Mutation::DeleteLine(a) => {
                lines.remove(a.index(n));
            }
            Mutation::DupLine(a, b) => {
                let l = lines[a.index(n)].clone();
                lines.insert(b.index(n + 1), l);
            }
            Mutation::SwapLines(a, b) => lines.swap(a.index(n), b.index(n)),
            Mutation::DeleteToken(a, b)
            | Mutation::DupToken(a, b)
            | Mutation::SwapTokens(a, b, _)
            | Mutation::Number(a, b, _)
            | Mutation::Suffix(a, b, _) => {
                let l = &mut lines[a.index(n)];
                let mut t = tokens(l);
                if t.is_empty() {
                    continue;
                }
                let k = b.index(t.len());
                match m {
                    Mutation::DeleteToken(..) => {
                        t.remove(k);
                    }
                    Mutation::DupToken(..) => t.insert(k, t[k].clone()),
                    Mutation::SwapTokens(_, _, c) => {
                        let j = c.index(t.len());
                        t.swap(k, j);
                    }
                    Mutation::Number(_, _, c) => {
                        let numbers: Vec<usize> = (0..t.len())
                            .filter(|&j| t[j].contains(|c: char| c.is_ascii_digit()))
                            .collect();
                        let j = if numbers.is_empty() {
                            k
                        } else {
                            numbers[b.index(numbers.len())]
                        };
                        // Keep what surrounds the digits: `(3,` stays `(X,`.
                        let tok = &t[j];
                        let from = tok.find(|c: char| c.is_ascii_digit() || c == '-');
                        let to = tok.rfind(|c: char| c.is_ascii_digit()).map(|e| e + 1);
                        let x = c.get(EXTREMES);
                        t[j] = match (from, to) {
                            (Some(f), Some(e)) if f < e => {
                                format!("{}{x}{}", &tok[..f], &tok[e..])
                            }
                            _ => x.to_string(),
                        };
                    }
                    Mutation::Suffix(_, _, c) => t[k] += c.get(SUFFIXES),
                    _ => unreachable!(),
                }
                *l = t.join(" ");
            }
            Mutation::Insert(a, b, c) => {
                let l = &mut lines[a.index(n)];
                let at = char_at(l, b);
                l.insert_str(at, c.get(ODD));
            }
            Mutation::DeleteChar(a, b) => {
                let l = &mut lines[a.index(n)];
                let at = char_at(l, b);
                if at < l.len() {
                    l.remove(at);
                }
            }
            Mutation::Line(a, b) => lines.insert(a.index(n + 1), b.get(LINES).to_string()),
            Mutation::Bom => lines[0].insert(0, '\u{feff}'),
            Mutation::Truncate(a) => lines.truncate(a.index(n) + 1),
        }
    }
    lines.join("\n")
}

/// Everything a parsed scenario goes through, none of which may panic:
/// the lint's checks, resolving its starts, and, when it is small, a world
/// of it with its `run` (a few ticks) and `expect` lines.
fn exercise(s: &Scenario) {
    let kinds = &*KINDS;
    let _ = s.unseen(kinds);
    let _ = s.outside_region("f");
    for c in &s.checks {
        if let Check::Expect { what, .. } = c {
            let _ = check_expect(kinds, what);
        }
    }
    let saved = present(&s.starts, kinds);
    let _ = absent(&saved, kinds);
    let _ = Placement::resolve_saved(&saved, kinds, &s.terrain());
    let resolved = Placement::resolve(&s.starts, kinds, &s.terrain());
    if let Err(e) = &resolved {
        let _ = s.line_of(e);
    }
    let chunks = u64::from(s.width.div_ceil(64)) * u64::from(s.height.div_ceil(64));
    if chunks > 4 {
        return;
    }
    let Ok(mut world) = sim::new_world_with(s, kinds.clone()) else {
        assert!(resolved.is_err(), "resolves, but no world");
        return;
    };
    for c in &s.checks {
        match c {
            Check::Run(t) => {
                for _ in 0..(*t).min(3) {
                    sim::step(&mut world);
                }
            }
            Check::Expect { what, .. } => {
                let _ = sim::expect(&mut world, what);
            }
        }
    }
}

proptest! {
    #![proptest_config(crate::fuzz_config(64))]

    /// A generated scenario parses to what it says, and survives the rest.
    #[test]
    fn a_valid_scenario_parses(v in valid()) {
        let s = Scenario::parse("v.scenario", &v.text)
            .unwrap_or_else(|e| panic!("{e}\n{}", v.text));
        prop_assert_eq!(s.seed, v.seed);
        prop_assert_eq!(s.starts.len(), v.starts);
        prop_assert_eq!(s.start_lines.len(), v.starts);
        prop_assert_eq!(s.checks.len(), v.checks);
        exercise(&s);
    }
}

proptest! {
    #![proptest_config(crate::fuzz_config(256))]

    /// A broken scenario is an error or a scenario, never a panic; what
    /// parses survives the rest.
    #[test]
    fn a_mutated_scenario_never_panics(
        v in valid(),
        ms in prop::collection::vec(mutation(), 1..6),
    ) {
        let text = mutate(&v.text, &ms);
        if let Ok(s) = Scenario::parse("m.scenario", &text) {
            exercise(&s);
        }
    }
}
