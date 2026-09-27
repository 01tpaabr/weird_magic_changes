//! Properties of the compiler over programs made from the grammar
//! (`rules/gen_rules.rs`, `docs/GRAMMAR.md`): whatever the grammar derives
//! parses; well-scoped, well-typed programs compile, lint included; and
//! nothing panics or takes long.
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

/// Compile, and fail if that panics (proptest catches it) or is slow.
fn compile_quickly(text: &str) -> std::result::Result<Result<Kinds>, TestCaseError> {
    let t = Instant::now();
    let r = compile("g.rules", text);
    let took = t.elapsed();
    if took > SLOW {
        return Err(TestCaseError::fail(format!(
            "compiling took {took:?}\n--- program ---\n{text}"
        )));
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
