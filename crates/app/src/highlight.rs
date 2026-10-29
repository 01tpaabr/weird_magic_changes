//! Syntax colours for a line of rules, as an editor draws code: keywords,
//! actions, built-in senses and targets, numbers and times, strings, kind
//! names, the kind's own needs and memory, sub calls, operators, comments.
//!
//! A forgiving pass over one line, not the compiler's lexer: it never
//! fails, so a line the compiler would refuse (or a fragment of one) still
//! draws. Words are told apart with the compiler's [`KEYWORDS`] and the
//! kind table.

use sim_core::rules::compile::KEYWORDS;
use sim_core::{KindDef, Kinds};

use crate::render::palette::Color;

pub const KEYWORD: Color = Color::rgb(0xc7, 0x92, 0xea);
pub const ACTION: Color = Color::rgb(0xff, 0x6e, 0x7f);
pub const BUILTIN: Color = Color::rgb(0x82, 0xaa, 0xff);
pub const NUMBER: Color = Color::rgb(0xf7, 0x8c, 0x6c);
pub const STRING: Color = Color::rgb(0xec, 0xc4, 0x8d);
pub const KIND: Color = Color::rgb(0xff, 0xcb, 0x6b);
pub const FIELD: Color = Color::rgb(0x89, 0xdd, 0xff);
pub const SUB: Color = Color::rgb(0xad, 0xdb, 0x67);
pub const NAME: Color = Color::rgb(0xd6, 0xde, 0xeb);
pub const SYMBOL: Color = Color::rgb(0x89, 0xa4, 0xbb);
pub const COMMENT: Color = Color::rgb(0x63, 0x77, 0x77);

/// Words that are an action: what a think does.
const ACTIONS: [&str; 13] = [
    "idle", "die", "become", "spawn", "move", "drink", "eat", "hit", "graze", "take", "give",
    "mark", "next",
];

/// Keywords that read the world or name a cell: functions, searches,
/// targets, outcomes, constants.
const BUILTINS: [&str; 38] = [
    "nearest",
    "count",
    "is",
    "free",
    "dist",
    "rand",
    "chance",
    "min",
    "max",
    "abs",
    "sign",
    "clamp",
    "look_of",
    "signal_of",
    "pack",
    "hi",
    "lo",
    "sniff",
    "scent",
    "toward",
    "away",
    "at",
    "here",
    "random",
    "attacker",
    "north",
    "east",
    "south",
    "west",
    "dir",
    "blocked",
    "missed",
    "refused",
    "true",
    "false",
    "only",
    "within",
    "as",
];

/// The senses (`vm::Sense`), reserved but not in [`KEYWORDS`].
pub const SENSES: [&str; 15] = [
    "light", "age", "x", "y", "hour", "day", "kind", "look", "signal", "state", "hurt", "hurt_dir",
    "result", "taken", "trapped",
];

/// The ground predicates: plain names to the compiler where a name goes.
const GROUND: [&str; 4] = ["water", "soil", "rock", "bare"];

/// `text` (one line of a rule of kind `def`) cut into runs of one colour.
pub fn spans(kinds: &Kinds, def: &KindDef, text: &str) -> Vec<(String, Color)> {
    let b = text.as_bytes();
    let mut out: Vec<(String, Color)> = Vec::new();
    let mut push = |s: &str, c: Color| match out.last_mut() {
        Some((t, last)) if *last == c => t.push_str(s),
        _ => out.push((s.to_string(), c)),
    };
    let mut i = 0;
    while i < b.len() {
        let start = i;
        let c = b[i];
        if c == b'#' {
            push(&text[i..], COMMENT);
            break;
        } else if c.is_ascii_whitespace() {
            while i < b.len() && b[i].is_ascii_whitespace() {
                i += 1;
            }
            push(&text[start..i], NAME);
        } else if c.is_ascii_digit() {
            // A number with its unit (`30min`, `2d`).
            while i < b.len() && b[i].is_ascii_alphanumeric() {
                i += 1;
            }
            push(&text[start..i], NUMBER);
        } else if c.is_ascii_alphabetic() || c == b'_' {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let word = &text[start..i];
            let called = text[i..].trim_start().starts_with('(');
            push(word, word_colour(kinds, def, word, called));
        } else if c == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                i += 1;
            }
            i = (i + 1).min(b.len());
            push(&text[start..i], STRING);
        } else {
            // Operators and punctuation, and any other character whole.
            i += text[i..].chars().next().map_or(1, char::len_utf8);
            push(&text[start..i], SYMBOL);
        }
    }
    out
}

fn word_colour(kinds: &Kinds, def: &KindDef, word: &str, called: bool) -> Color {
    if ACTIONS.contains(&word) {
        ACTION
    } else if BUILTINS.contains(&word) || SENSES.contains(&word) || GROUND.contains(&word) {
        BUILTIN
    } else if KEYWORDS.contains(&word) {
        KEYWORD
    } else if def.needs.iter().any(|n| n.name == word) || def.mems.iter().any(|m| m == word) {
        FIELD
    } else if kinds.defs.iter().any(|d| d.name == word) {
        KIND
    } else if called {
        SUB
    } else {
        NAME
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::rules::CHICKEN;

    fn colour_of(spans: &[(String, Color)], word: &str) -> Color {
        spans
            .iter()
            .find(|(s, _)| s.split_whitespace().any(|w| w == word))
            .unwrap_or_else(|| panic!("no `{word}` in {spans:?}"))
            .1
    }

    #[test]
    fn every_word_gets_the_colour_of_what_it_is() {
        let kinds = Kinds::builtin();
        let def = kinds.def(CHICKEN);
        let text =
            "when food < 20h and nearest fox within 5 as f => { heading = 3  flee(f) }  # run";
        let s = spans(&kinds, def, text);
        // Nothing is lost or reordered.
        assert_eq!(s.iter().map(|(t, _)| t.as_str()).collect::<String>(), text);
        assert_eq!(colour_of(&s, "when"), KEYWORD);
        assert_eq!(colour_of(&s, "food"), FIELD, "a need");
        assert_eq!(colour_of(&s, "heading"), FIELD, "a memory slot");
        assert_eq!(colour_of(&s, "20h"), NUMBER);
        assert_eq!(colour_of(&s, "nearest"), BUILTIN);
        assert_eq!(colour_of(&s, "fox"), KIND);
        assert_eq!(colour_of(&s, "f"), NAME, "a binding");
        assert_eq!(s.iter().find(|(t, _)| t == "flee").unwrap().1, SUB);
        assert_eq!(s.iter().find(|(t, _)| t == "=>").unwrap().1, SYMBOL);
        assert_eq!(s.last().unwrap(), &("# run".to_string(), COMMENT));
        let s = spans(
            &kinds,
            def,
            "when hour >= 20 => { look = 1  move toward at(x, y) }",
        );
        assert_eq!(colour_of(&s, "move"), ACTION);
        assert_eq!(colour_of(&s, "hour"), BUILTIN, "a sense");
        assert_eq!(colour_of(&s, "toward"), BUILTIN);
        assert_eq!(
            s.iter().find(|(t, _)| t == "at").unwrap().1,
            BUILTIN,
            "not a sub call"
        );
    }

    #[test]
    fn odd_text_still_draws_whole() {
        let kinds = Kinds::builtin();
        let def = kinds.def(CHICKEN);
        for text in ["", "\"unterminated", "é ü => 3x", "####", "a(", "{}"] {
            let s = spans(&kinds, def, text);
            assert_eq!(s.iter().map(|(t, _)| t.as_str()).collect::<String>(), text);
        }
    }

    #[test]
    fn the_senses_listed_are_the_vms() {
        assert_eq!(SENSES.len(), sim_core::rules::vm::Sense::ALL.len());
        for w in SENSES {
            let e = sim_core::rules::compile("t.rules", &format!("kind k {{ mem {w} }}"));
            assert!(e.is_err_and(|e| e.msg.contains("reserved word")), "`{w}`");
        }
        for w in ACTIONS.iter().chain(&BUILTINS) {
            assert!(KEYWORDS.contains(w), "`{w}` is not a keyword");
        }
    }
}
