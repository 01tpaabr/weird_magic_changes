//! `wmc why`: what one actor is thinking. [`report`] re-runs the think of
//! whoever stands on a cell (else its ground cover) against the world as it
//! is, with a trace, and prints its state, every rule the think checked and
//! whether it fired, what it decided and what it wrote. Nothing in the
//! world changes (`sim::explain`).

use std::fmt::Write;

use bevy::prelude::World;
use sim_core::actors::systems::Explained;
use sim_core::rules::vm::{Action, OpCode, result};
use sim_core::time::{Clock, TICKS_PER_DAY};
use sim_core::{Kinds, Pos, sim};

/// The report for the actor at `p`, or `None` if nobody is there. `ops`
/// adds every executed op, with the rule it belongs to.
pub fn report(world: &World, p: Pos, ops: bool) -> Option<String> {
    let e = sim::explain(world, p)?;
    let kinds = world.resource::<Kinds>();
    let tick = world.resource::<sim_core::Tick>().0;
    Some(format(kinds, tick, p, &e, ops))
}

/// Ticks as `1d 2h 03m` (whole minutes; `0m` for less).
pub fn duration(ticks: i64) -> String {
    let neg = ticks < 0;
    let mins = ticks.unsigned_abs() / sim_core::time::minutes(1);
    let (d, h, m) = (
        mins / (TICKS_PER_DAY / sim_core::time::minutes(1)),
        mins / 60 % 24,
        mins % 60,
    );
    let s = match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, _) => format!("{h}h {m:02}m"),
        _ => format!("{d}d {h}h {m:02}m"),
    };
    if neg { format!("-{s}") } else { s }
}

fn result_name(code: u8) -> &'static str {
    match code & result::MASK {
        result::OK => "OK",
        result::BLOCKED => "BLOCKED",
        result::MISSED => "MISSED",
        result::REFUSED => "REFUSED",
        _ => "none",
    }
}

fn state_name(kinds: &Kinds, kind: u16, state: u8) -> String {
    kinds
        .debug
        .states
        .get(usize::from(kind))
        .and_then(|s| s.get(usize::from(state)))
        .cloned()
        .unwrap_or_else(|| {
            if state == 0 {
                "-".into()
            } else {
                state.to_string()
            }
        })
}

fn kind_name(kinds: &Kinds, kind: u16) -> String {
    kinds
        .defs
        .get(usize::from(kind))
        .map_or_else(|| format!("kind {kind}"), |d| d.name.clone())
}

fn format(kinds: &Kinds, tick: u64, p: Pos, e: &Explained, ops: bool) -> String {
    let def = kinds.def(e.row.kind);
    let (b, a) = (&e.before, &e.after);
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{} at ({}, {}), chunk ({}, {}) slot {}, uid {:016x}",
        def.name, p.x, p.y, e.coord.x, e.coord.y, e.slot, b.uid
    );
    let _ = writeln!(
        s,
        "  tick {tick} ({}): thinks every {} ticks, {}",
        Clock::at(tick),
        def.cadence(),
        if e.due {
            "due now"
        } else {
            "not due now (shown anyway)"
        }
    );
    let _ = writeln!(
        s,
        "  age {} | state {} | look {} | signal {} | last result {} | hurt {}{}",
        duration(i64::from((tick as u32).wrapping_sub(b.born))),
        state_name(kinds, e.row.kind, b.state),
        e.row.look,
        e.row.signal,
        result_name(b.events),
        b.hurt,
        if b.events & sim_core::rules::vm::event::TAKEN != 0 {
            " | taken from"
        } else {
            ""
        },
    );
    if !def.needs.is_empty() {
        let needs: Vec<String> = def
            .needs
            .iter()
            .enumerate()
            .map(|(i, n)| {
                // As the think saw them: decayed, before its writes.
                let v = decayed(b.needs[i], n.decays, tick, b.last_think);
                if n.decays {
                    format!(
                        "{} {} / {}",
                        n.name,
                        duration(i64::from(v)),
                        duration(i64::from(n.max))
                    )
                } else {
                    format!("{} {v} / {}", n.name, n.max)
                }
            })
            .collect();
        let _ = writeln!(s, "  needs  {}", needs.join(" | "));
    }
    if !def.mems.is_empty() {
        let mems: Vec<String> = def
            .mems
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{m} {}", b.mem[i]))
            .collect();
        let _ = writeln!(s, "  mem    {}", mems.join(" | "));
    }

    let visited = |pc: u32| e.trace.iter().any(|st| st.pc == pc);
    let rules: Vec<_> = kinds
        .debug
        .rules
        .iter()
        .filter(|r| r.kind == e.row.kind)
        .collect();
    if e.starved {
        let _ = writeln!(
            s,
            "rules  none ran: a vital need is empty, so the think is `die`"
        );
    } else if rules.is_empty() {
        let _ = writeln!(s, "rules  (no source positions: hand-assembled kind)");
    } else {
        let _ = writeln!(
            s,
            "rules  (FIRED: its body ran; no: its condition was false; blank: not reached)"
        );
        let mut shown_state = None;
        for r in &rules {
            if r.state.is_some_and(|st| st != b.state) {
                continue;
            }
            if r.state.is_some() && shown_state != r.state {
                shown_state = r.state;
                let _ = writeln!(s, "  state {}", state_name(kinds, e.row.kind, b.state));
            }
            let verdict = if visited(r.body_pc) {
                "FIRED"
            } else if visited(r.cond_pc) {
                "no"
            } else {
                ""
            };
            let file = kinds
                .debug
                .files
                .get(usize::from(r.file))
                .map_or("?", String::as_str);
            let via = r
                .via
                .as_ref()
                .map_or(String::new(), |v| format!("   [via {v}]"));
            let _ = writeln!(
                s,
                "  {:<20} {verdict:<6} {}{via}",
                format!("{file}:{}", r.line),
                r.text
            );
        }
    }

    let _ = writeln!(s, "decides {}", decision(kinds, e));
    let mut writes = Vec::new();
    for (i, n) in def.needs.iter().enumerate() {
        // Decay is not a write: compare with the decayed value the rules saw.
        let seen = decayed(b.needs[i], n.decays, tick, b.last_think);
        if a.needs[i] != seen {
            writes.push(format!("{} {} -> {}", n.name, seen, a.needs[i]));
        }
    }
    for (i, m) in def.mems.iter().enumerate() {
        if a.mem[i] != b.mem[i] {
            writes.push(format!("{m} {} -> {}", b.mem[i], a.mem[i]));
        }
    }
    if a.state != b.state {
        writes.push(format!(
            "state {} -> {}",
            state_name(kinds, e.row.kind, b.state),
            state_name(kinds, e.row.kind, a.state)
        ));
    }
    if !writes.is_empty() {
        let _ = writeln!(s, "writes  {}", writes.join(" | "));
    }
    let _ = writeln!(
        s,
        "cost    {} ops of {} fuel{}",
        e.outcome.used,
        def.fuel,
        e.outcome.trap.map_or(String::new(), |t| format!(
            " | TRAPPED: {t:?} (the think became idle)"
        ))
    );

    if ops {
        let _ = writeln!(s, "ops");
        for st in &e.trace {
            if let Some(r) = rules.iter().find(|r| r.cond_pc == st.pc) {
                let _ = writeln!(s, "  -- line {}: {}", r.line, r.text);
            }
            let sub = kinds
                .subs
                .iter()
                .position(|&entry| entry == st.pc)
                .and_then(|i| kinds.debug.subs.get(i));
            if let Some(name) = sub {
                let _ = writeln!(s, "  -- sub {name}");
            }
            let _ = writeln!(
                s,
                "  {:>5}  {:<10} a={:<3} imm={:<6} -> {:<8} fuel {}",
                st.pc,
                format!("{:?}", st.op.code),
                st.op.a,
                st.op.imm,
                st.top.map_or("-".to_string(), |v| v.to_string()),
                st.fuel
            );
            if st.op.code == OpCode::Halt {
                break;
            }
        }
    }
    s
}

/// A need as the think saw it: decayed by the ticks since the last think.
fn decayed(v: i32, decays: bool, tick: u64, last: u32) -> i32 {
    if decays {
        (i64::from(v) - i64::from((tick as u32).wrapping_sub(last))).max(0) as i32
    } else {
        v
    }
}

fn decision(kinds: &Kinds, e: &Explained) -> String {
    let (it, out) = (&e.intent, &e.outcome);
    let at = |dx: i8, dy: i8| format!("({dx}, {dy})");
    let need = |i: u16| {
        kinds
            .def(e.row.kind)
            .needs
            .get(usize::from(i))
            .map_or("?".to_string(), |n| n.name.clone())
    };
    let mut d = match it.action {
        Action::Idle => "idle".to_string(),
        Action::Die if e.starved => "die (a vital need is empty)".to_string(),
        Action::Die => "die".to_string(),
        Action::Become => format!("become {}", kind_name(kinds, it.kind)),
        Action::Spawn => {
            let with = if it.with == [0, 0] {
                String::new()
            } else {
                format!(" with ({}, {})", it.with[0], it.with[1])
            };
            format!(
                "spawn {} at {}{with}",
                kind_name(kinds, it.kind),
                at(it.dx, it.dy)
            )
        }
        Action::Move => format!("move {} -> step {}", at(out.dx, out.dy), at(it.dx, it.dy)),
        Action::Drink => format!(
            "drink at {}{}",
            at(it.dx, it.dy),
            if it.kind == 1 {
                ""
            } else {
                " (no water there: it will be refused)"
            }
        ),
        Action::Eat => format!("eat {}", at(it.dx, it.dy)),
        Action::Hit => format!("hit {}", at(it.dx, it.dy)),
        Action::Graze => format!("graze {}", at(it.dx, it.dy)),
        Action::Take => format!(
            "take {} {} from {}",
            it.amount,
            need(it.kind),
            at(it.dx, it.dy)
        ),
        Action::Give => format!(
            "give {} {} to {}",
            it.amount,
            need(it.kind),
            at(it.dx, it.dy)
        ),
    };
    if let Some(l) = it.look {
        let _ = write!(d, "; look = {l}");
    }
    if let Some(v) = it.signal {
        let _ = write!(d, "; signal = {v}");
    }
    if let Some((ch, v)) = it.mark {
        let name = kinds
            .scents
            .get(usize::from(ch))
            .map_or("?", String::as_str);
        let _ = write!(d, "; mark {name} {v}");
    }
    if let Some(n) = out.next {
        let _ = write!(d, "; next {}", state_name(kinds, e.row.kind, n));
    }
    d
}

/// One line on how the actor's next step went: where it is, its result.
pub fn after(world: &mut World, uid: u64) -> String {
    match sim::find_uid(world, uid) {
        None => "gone: it died or was eaten this tick".to_string(),
        Some(p) => {
            let e = sim::explain(world, p).expect("found it there");
            format!(
                "at ({}, {}), result {}",
                p.x,
                p.y,
                result_name(e.before.events)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::time::{hours, minutes};

    #[test]
    fn durations_read_as_days_hours_minutes() {
        assert_eq!(duration(0), "0m");
        assert_eq!(duration(minutes(5) as i64), "5m");
        assert_eq!(duration((hours(2) + minutes(3)) as i64), "2h 03m");
        assert_eq!(
            duration(TICKS_PER_DAY as i64 + hours(1) as i64),
            "1d 1h 00m"
        );
        assert_eq!(duration(-(minutes(1) as i64)), "-1m");
    }

    #[test]
    fn a_report_names_the_rule_that_fired() {
        use sim_core::actors::systems::newborn;
        use sim_core::rules::CHICKEN;
        let kinds = Kinds::builtin();
        let cfg = sim_core::Scenario {
            width: 64,
            height: 64,
            seed: 5,
            ..Default::default()
        };
        let mut w = sim::new_world_with(&cfg, kinds.clone()).unwrap();
        let now = sim::tick(&w);
        let at = (0..64 * 64)
            .map(|i| Pos::new(i % 64, i / 64))
            .find(|&p| sim::place_actor(&mut w, p, CHICKEN, newborn(&kinds, CHICKEN, 0xAB, now)))
            .expect("a walkable cell");
        let r = report(&w, at, true).expect("a chicken there");
        assert!(r.starts_with("chicken at"), "{r}");
        assert!(r.contains("FIRED"), "{r}");
        assert!(r.contains("animals.rules:"), "{r}");
        assert!(r.contains("decides "), "{r}");
        assert!(r.contains("\nops\n"), "{r}");
    }

    #[test]
    fn an_inherited_rule_names_where_it_came_from() {
        use sim_core::actors::systems::newborn;
        let kinds = sim_core::rules::compile(
            "t.rules",
            "trait restful {\n  when hour >= 0 => idle\n}\nkind cat extends restful { glyph \"c\" }",
        )
        .unwrap();
        let cfg = sim_core::Scenario {
            width: 64,
            height: 64,
            seed: 5,
            ..Default::default()
        };
        let mut w = sim::new_world_with(&cfg, kinds.clone()).unwrap();
        let now = sim::tick(&w);
        let at = (0..64 * 64)
            .map(|i| Pos::new(i % 64, i / 64))
            .find(|&p| sim::place_actor(&mut w, p, 0, newborn(&kinds, 0, 0xCA, now)))
            .expect("a walkable cell");
        let r = report(&w, at, false).expect("a cat there");
        assert!(r.contains("t.rules:2"), "{r}");
        assert!(
            r.contains("FIRED  when hour >= 0 => idle   [via restful]"),
            "{r}"
        );
    }
}
