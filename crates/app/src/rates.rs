//! `wmc rates`: each kind's numbers in day units, read off the compiled
//! rules (`sim_core::rules::RuleShape`): thinks a day, how long each need
//! lasts, what eating one gives; per rule, its dice as odds a think and
//! times a day, its other gates, the age it waits for, and what it does. A
//! `die` past an age becomes a mean life left.
//!
//! It says what a rule does *when reached*: whether a think reaches it (no
//! rule above it acted first) and its gates hold depends on the world, and
//! `wmc observe` measures that (`fires`).

use std::fmt::Write as _;

use sim_core::Kinds;
use sim_core::scenario::span;
use sim_core::time::TICKS_PER_DAY;

use crate::why::state_name;

/// `x` to three significant figures, without trailing zeros: `0.32`,
/// `216`, `0.003`, `10800`.
fn sig(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    // A number of digits: far inside i32.
    #[allow(clippy::cast_possible_truncation)]
    let digits = (2 - x.abs().log10().floor() as i32).max(0) as usize;
    let s = format!("{x:.digits$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Every kind (or those named in `only`), as described above.
pub fn report(kinds: &Kinds, only: &[String]) -> String {
    let mut s = String::new();
    for (k, def) in kinds.defs.iter().enumerate() {
        if !only.is_empty() && !only.contains(&def.name) {
            continue;
        }
        let kind = u16::try_from(k).expect("kind ids are u16");
        let thinks = TICKS_PER_DAY as f64 / def.cadence() as f64;
        let parents = kinds
            .debug
            .parents
            .get(k)
            .filter(|p| !p.is_empty())
            .map_or(String::new(), |p| format!(" (extends {})", p.join(", ")));
        let _ = writeln!(
            s,
            "{}{parents}: {} thinks a day (every {} ticks); sight {}, fuel {}, bite {}{}",
            def.name,
            sig(thinks),
            def.cadence(),
            def.sight,
            def.fuel,
            def.bite,
            if def.food > 0 {
                format!(
                    "; eaten whole, gives {} of food",
                    span(def.food.unsigned_abs().into())
                )
            } else {
                String::new()
            }
        );
        if !def.needs.is_empty() {
            let needs: Vec<String> = def
                .needs
                .iter()
                .map(|n| {
                    let v = if n.decays {
                        format!("{} lasts {}", n.name, span(n.max.unsigned_abs().into()))
                    } else {
                        format!("{} {} points", n.name, n.max)
                    };
                    if n.vital { format!("{v}, vital") } else { v }
                })
                .collect();
            let _ = writeln!(s, "  needs   {}", needs.join("; "));
        }
        let rules: Vec<_> = kinds
            .debug
            .rules
            .iter()
            .filter(|r| r.kind == kind)
            .collect();
        if rules.is_empty() {
            continue;
        }
        let _ = writeln!(
            s,
            "  {:<22} {:<14} {:>9} {:>9} {:>5}  rule",
            "where", "does", "a think", "a day", "gates"
        );
        let mut state = None;
        for r in rules {
            if r.state.is_some() && r.state != state {
                state = r.state;
                let name = state_name(kinds, kind, r.state.unwrap_or(0));
                let _ = writeln!(s, "  state {name}");
            }
            let sh = &r.shape;
            let p = sh.dice_odds();
            let file = kinds
                .debug
                .files
                .get(usize::from(r.file))
                .map_or("?", String::as_str);
            let (odds, a_day) = if sh.dice == (1, 1) {
                let every = if sh.gates == 0 {
                    sig(thinks)
                } else {
                    "-".into()
                };
                ("-".to_string(), every)
            } else {
                (format!("{}%", sig(p * 100.0)), sig(p * thinks))
            };
            let mut note = String::new();
            if let Some(age) = sh.after_age {
                let age = span(age.unsigned_abs().into());
                match sh.acts.as_deref() {
                    Some("die") if sh.dice != (1, 1) => {
                        let _ = write!(
                            note,
                            "   (past {age}: {} days left on average)",
                            sig(1.0 / (p * thinks))
                        );
                    }
                    _ => {
                        let _ = write!(note, "   (from age {age})");
                    }
                }
            }
            if let Some(via) = &r.via {
                let _ = write!(note, "   [{via}]");
            }
            let text: Vec<&str> = r.text.split_whitespace().collect();
            let _ = writeln!(
                s,
                "  {:<22} {:<14} {odds:>9} {a_day:>9} {:>5}  {}{note}",
                format!("{file}:{}", r.line),
                sh.acts.as_deref().unwrap_or("goes on"),
                sh.gates,
                text.join(" ")
            );
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn figures_read_short() {
        assert_eq!(sig(0.324), "0.324");
        assert_eq!(sig(3.0864), "3.09");
        assert_eq!(sig(216.0), "216");
        assert_eq!(sig(10800.0), "10800");
        assert_eq!(sig(0.003), "0.003");
        assert_eq!(sig(84.375), "84.4");
        assert_eq!(sig(0.0), "0");
    }

    #[test]
    fn the_built_in_fox_in_day_units() {
        let text = report(&Kinds::builtin(), &["fox".into()]);
        // Parents as the rule table keeps them: arguments in ticks.
        assert!(
            text.starts_with(
                "fox (extends mortal(129600, 3), drinker(8, 5400, 2)): 10800 thinks a day (every 2 ticks); sight 16, fuel 512, bite 10; eaten whole, gives 1d of food\n"
            ),
            "{text}"
        );
        assert!(
            text.contains(
                "  needs   water lasts 1d, vital; food lasts 1d, vital; health 40 points, vital"
            ),
            "{text}"
        );
        // Old age: 3 in 100 000 a think, 10 800 thinks a day.
        let mortal = text
            .lines()
            .find(|l| l.contains("rand(100000) < odds"))
            .unwrap();
        assert!(
            mortal.contains(" die ") && mortal.contains("0.003%") && mortal.contains("0.324"),
            "{mortal}"
        );
        assert!(
            mortal.contains("(past 6d: 3.09 days left on average)") && mortal.ends_with("[mortal]"),
            "{mortal}"
        );
        // A litter: chance(2), five more gates.
        let litter = text.lines().find(|l| l.contains("spawn fox at c")).unwrap();
        assert!(
            litter.contains("spawn fox") && litter.contains("2%") && litter.contains("216"),
            "{litter}"
        );
        assert!(litter.contains("(from age 2d)"), "{litter}");
        // A rule with no dice and no gates runs at every think it reaches.
        let awake = text
            .lines()
            .find(|l| l.contains("when true => look = 0"))
            .unwrap();
        assert!(
            awake.contains("goes on") && awake.contains("10800"),
            "{awake}"
        );
        assert!(!report(&Kinds::builtin(), &[]).is_empty());
        assert!(report(&Kinds::builtin(), &["nobody".into()]).is_empty());
    }
}
