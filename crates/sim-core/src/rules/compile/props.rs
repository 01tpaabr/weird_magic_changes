//! Properties of the compiler over programs made from the grammar
//! (`rules/gen_rules.rs`, `docs/GRAMMAR.md`): whatever the grammar derives
//! parses; well-scoped, well-typed programs compile, lint included; and no
//! text, mutated or pathological, panics, overflows the stack or takes long.
//!
//! The runs are deterministic (a fixed seed, no persistence file) and small
//! enough for `make test`. `WMC_FUZZ_CASES=n` runs `n` cases per property and
//! `WMC_FUZZ_SEED=s` draws other programs:
//!
//! ```text
//! WMC_FUZZ_CASES=100000 cargo test --profile fast -p sim-core --lib \
//!     --features bevy_ecs/multi_threaded props
//! ```

use std::time::{Duration, Instant};

use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestCaseError, TestError, TestRng, TestRunner};

use super::*;
use crate::rules::gen_rules::{self, Mode};

/// Longer than any sane compile of these texts, on a loaded machine in a
/// debug build: past it, something is superlinear.
const SLOW: Duration = Duration::from_secs(2);

/// Cases per property in `make test`: about a second of one core, all four.
const CASES: u32 = 1000;

fn runner(cases: u32) -> TestRunner {
    let env = |k: &str| std::env::var(k).ok().and_then(|v| v.parse::<u64>().ok());
    let cases = env("WMC_FUZZ_CASES").map_or(cases, |c| c as u32);
    let mut seed = [0u8; 32];
    seed[..8].copy_from_slice(&env("WMC_FUZZ_SEED").unwrap_or(1).to_le_bytes());
    let config = Config {
        cases,
        failure_persistence: None,
        ..Config::default()
    };
    TestRunner::new_with_rng(config, TestRng::from_seed(RngAlgorithm::ChaCha, &seed))
}

/// The failure with its shrunk program, printed as text.
fn check<T: std::fmt::Debug>(r: std::result::Result<(), TestError<T>>) {
    match r {
        Ok(()) => {}
        Err(TestError::Fail(why, _)) => panic!("{why}"),
        Err(e) => panic!("{e}"),
    }
}

fn parse(text: &str) -> Result<()> {
    let tokens = Lexer::new("g.rules", text).lex()?;
    let mut p = Parser {
        file: "g.rules",
        tokens,
        at: 0,
        depth: 0,
        deepest: 0,
    };
    p.file(&mut Items::default())
}

/// Compile, and fail if that panics (proptest catches it), is slow, or
/// refuses the text without a place in it.
fn compile_quickly(text: &str) -> std::result::Result<Result<Kinds>, TestCaseError> {
    let t = Instant::now();
    let r = compile("g.rules", text);
    let took = t.elapsed();
    let fail = |why: String| {
        Err(TestCaseError::fail(format!(
            "{why}\n--- program ---\n{text}"
        )))
    };
    if took > SLOW {
        return fail(format!("compiling took {took:?}"));
    }
    if let Err(e) = &r {
        let lines = text.split('\n').count() as u32;
        if e.file != "g.rules" || e.line == 0 || e.line > lines || e.col == 0 {
            return fail(format!("an error out of the text: {e:?}"));
        }
    }
    Ok(r)
}

#[test]
fn what_the_grammar_derives_parses() {
    check(runner(CASES).run(&gen_rules::program(Mode::Syntax), |p| {
        parse(&p.text).map_err(|e| TestCaseError::fail(format!("{e}\n--- program ---\n{}", p.text)))
    }));
}

#[test]
fn what_the_grammar_derives_compiles_or_is_refused() {
    check(runner(CASES).run(&gen_rules::program(Mode::Syntax), |p| {
        compile_quickly(&p.text).map(|_| ())
    }));
}

#[test]
fn valid_programs_compile_and_lint() {
    check(runner(CASES).run(&gen_rules::program(Mode::Valid), |p| {
        let fail = |e: String| TestCaseError::fail(format!("{e}\n--- program ---\n{}", p.text));
        parse(&p.text).map_err(|e| fail(e.to_string()))?;
        let k = compile_quickly(&p.text)?.map_err(|e| fail(e.to_string()))?;
        let again = compile("g.rules", &p.text).map_err(|e| fail(e.to_string()))?;
        if again.hash != k.hash {
            return Err(fail("two compiles of one text differ".into()));
        }
        // One file per item: names are global, and the order is kept, so
        // the kinds are the same. An item starts a line with `kind k<n>` or
        // `trait` (a `choose` weight may start one with the sense `kind`).
        let starts = |w: &str| {
            p.text
                .match_indices(w)
                .filter(|(i, _)| p.text[i + w.len()..].starts_with(|c: char| c.is_ascii_digit()))
                .map(|(i, _)| i + 1)
                .collect::<Vec<_>>()
        };
        let mut cuts = starts("\nkind k");
        cuts.extend(starts("\ntrait t"));
        cuts.sort_unstable();
        let mut texts = Vec::new();
        let mut from = 0;
        for c in cuts.into_iter().chain([p.text.len()]) {
            texts.push((format!("f{}.rules", texts.len()), &p.text[from..c]));
            from = c;
        }
        let files: Vec<(&str, &str)> = texts.iter().map(|(n, t)| (n.as_str(), *t)).collect();
        let split = compile_files(&files).map_err(|e| fail(format!("in files: {e}")))?;
        if split.hash != k.hash {
            return Err(fail(format!("{} files compile differently", files.len())));
        }
        if p.kinds.iter().any(|n| k.by_name(n).is_none()) || k.len() != p.kinds.len() {
            return Err(fail(format!(
                "kinds {:?}, compiled {:?}",
                p.kinds,
                k.names().collect::<Vec<_>>()
            )));
        }
        Ok(())
    }));
}

/// A text as pieces: a run of name characters, a run of whitespace, or one
/// other character.
fn pieces(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let class = |c: char| {
        if c.is_ascii_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    for c in text.chars() {
        match out.last_mut() {
            Some(last) if class(c) != 2 && last.chars().next().map(class) == Some(class(c)) => {
                last.push(c);
            }
            _ => out.push(c.to_string()),
        }
    }
    out
}

const WORDS: [&str; 30] = [
    "kind", "trait", "sub", "const", "when", "=>", "{", "}", "(", ")", "return", "\n", "#", "\"",
    "inherit", "state", "not", "-", "toward", "extends", ";", ".", ":", ",", "and", "nearest",
    "as", "within", "choose", "if",
];
const NUMBERS: [&str; 7] = [
    "0",
    "2147483647",
    "2147483648",
    "99999999999999999999",
    "2147483647d",
    "143165577min",
    "-2147483647",
];
const NESTS: [(&str, &str); 9] = [
    ("(", ")"),
    ("{", "}"),
    ("not ", ""),
    ("- ", ""),
    ("toward ", ""),
    ("if a { ", "}"),
    ("f(", ")"),
    ("choose { 1: ", "}"),
    ("a + ", ""),
];

/// Apply one mutation, chosen and placed by `(what, a, b)`.
fn mutate(p: &mut Vec<String>, (what, a, b): (u8, u32, u32)) {
    let len = p.len().max(1);
    let (i, j) = (a as usize % len, b as usize % len);
    match what % 8 {
        0 if !p.is_empty() => {
            p.remove(i);
        }
        1 if !p.is_empty() => {
            let t = p[i].clone();
            p.insert(i, t);
        }
        2 if !p.is_empty() => p.swap(i, j),
        3 => p.truncate(i),
        4 => {
            // Any characters: controls, quotes, non-ASCII.
            let s: String = [a, b, a ^ b]
                .iter()
                .filter_map(|&c| char::from_u32(c % 0x3000))
                .collect();
            p.insert(i.min(p.len()), s);
        }
        5 => p.insert(i.min(p.len()), WORDS[j % WORDS.len()].to_string()),
        6 => {
            let n = NUMBERS[j % NUMBERS.len()].to_string();
            match p
                .iter()
                .position(|t| t.starts_with(|c: char| c.is_ascii_digit()))
            {
                Some(k) => p[k] = n,
                None => p.insert(i.min(p.len()), n),
            }
        }
        _ => {
            let (open, close) = NESTS[j % NESTS.len()];
            let n = 1 + (b as usize >> 8) % 3000;
            p.insert(i.min(p.len()), open.repeat(n) + "a " + &close.repeat(n));
        }
    }
}

#[test]
fn mutated_programs_never_panic_and_stay_fast() {
    let strategy = (
        prop_oneof![
            gen_rules::program(Mode::Syntax),
            gen_rules::program(Mode::Valid)
        ],
        prop::collection::vec((any::<u8>(), any::<u32>(), any::<u32>()), 1..6),
    )
        .prop_map(|(p, ms)| {
            let mut pieces = pieces(&p.text);
            for m in ms {
                mutate(&mut pieces, m);
            }
            pieces.concat()
        });
    check(runner(CASES).run(&strategy, |text| compile_quickly(&text).map(|_| ())));
}

/// Texts no generator makes: nesting far past the limit, chains of
/// `extends` and calls, thousands of items. Each is refused or compiled,
/// quickly and on a test thread's stack.
#[test]
fn pathological_texts_are_refused_quickly() {
    let n = |k: usize, s: &str| s.repeat(k);
    let chain = |k: usize, word: &str| {
        let mut t = format!("{word} x0 {{ }}\n");
        for i in 1..k {
            t += &format!("{word} x{i} extends x{} {{ }}\n", i - 1);
        }
        t
    };
    // Nine subs, each nesting 60 deep around a call of the next: the
    // analyses follow calls 8 deep, through every level.
    let calls = |func: bool| {
        let mut t = String::new();
        for i in 0..9 {
            let call = match (func, i == 8) {
                (true, true) => "return 1".to_string(),
                (true, false) => format!("return s{}()", i + 1),
                (false, true) => "idle".to_string(),
                (false, false) => format!("s{}()", i + 1),
            };
            let tail = if func { " return 0" } else { "" };
            let (open, close) = (n(60, "if true { "), n(60, "}"));
            t += &format!("sub s{i}() {{ {open}{call} {close}{tail} }}\n");
        }
        t + if func {
            "kind k { mem m\n when s0() > 0 => { m = s0() } }"
        } else {
            "kind k { when true => { s0()  idle } }"
        }
    };
    let traits = (0..500).map(|i| format!("t{i}")).collect::<Vec<_>>();
    let wide = traits
        .iter()
        .map(|t| format!("trait {t} {{ when true => idle }}\n"))
        .collect::<String>()
        + &format!("kind k extends {} {{ }}", traits.join(", "));
    let rule = |body: &str| format!("kind k {{ mem m\n when true => {{ {body} }} }}");
    let cases = [
        format!(
            "kind k {{ when {}1{} => idle }}",
            n(100_000, "("),
            n(100_000, ")")
        ),
        rule(&(n(10_000, "if true { ") + &n(10_000, "}"))),
        format!("kind k {{ when 1{} > 0 => idle }}", n(100_000, " + 1")),
        format!("kind k {{ when {}true => idle }}", n(100_000, "not ")),
        format!("kind k {{ when {}1 > 0 => idle }}", n(100_000, "- ")),
        format!(
            "kind k {{ when true => move {}here }}",
            n(100_000, "toward ")
        ),
        rule(&format!(
            "if m == 0 {{ }} {}",
            n(10_000, "else if m == 1 { } ")
        )),
        rule(&n(50_000, "m = m + 1 ")),
        rule(
            &(0..20_000)
                .map(|i| format!("m = {} ", 100_000 + i))
                .collect::<String>(),
        ),
        chain(3000, "kind"),
        chain(3000, "trait") + "kind k extends x2999 { }",
        calls(true),
        calls(false),
        wide,
        (0..2000).map(|i| format!("kind k{i} {{ }}\n")).collect(),
        (0..2000)
            .map(|i| format!("sub s{i}() {{ s{}() }}\n", (i + 1) % 2000))
            .collect::<String>()
            + "kind k { when true => s0() }",
    ];
    for text in &cases {
        let t = Instant::now();
        let _ = compile("p.rules", text);
        let took = t.elapsed();
        let head: String = text.chars().take(80).collect();
        assert!(took < SLOW, "{took:?} compiling {head}...");
    }
}
