//! The inspector of `wmc play`: click an actor to see inside it, `f` to
//! follow it. The panel shows its kind and parents and its state, what its
//! next think would do (the rules that fire, copied out, and the decision),
//! its needs and memory, and every rule that think could reach:
//! [`crate::why`] in a side panel, refreshed every frame.
//!
//! The selection is a `uid`, so it survives moving across chunks and
//! `become`. Everything here reads the world and never writes it: the think
//! shown is a dry run (`sim::explain_slot`), so selecting, following and
//! closing change nothing in the sim.

use bevy::prelude::{Resource, World};
use sim_core::actors::systems::Explained;
use sim_core::rules::vm::{event, need_now};
use sim_core::time::stamp;
use sim_core::{ChunkActors, ChunkCells, ChunkCoord, ChunkMinds, Kinds, Pos, Stage, sim};

use crate::render::palette::{Color, TEXT_FG};
use crate::why::{Checked, checked_rules, decision, duration, result_name, state_name, writes};

/// Section titles.
pub const TITLE: Color = Color::rgb(0xf0, 0xc6, 0x74);
/// The rules that fire, in the next-think section.
pub const FIRED: Color = Color::rgb(0x9b, 0xe3, 0x8a);
/// A rule whose condition trapped, or an actor that is gone.
pub const ALERT: Color = Color::rgb(0xff, 0x7b, 0x72);
/// Secondary text: where a rule is written, the cost.
pub const DIM: Color = Color::rgb(0xa0, 0xa0, 0xa8);
/// The last state of an actor that is gone.
pub const FAINT: Color = Color::rgb(0x6a, 0x6a, 0x72);
/// The panel's last row.
pub const KEYS: &str = "click select | f follow | esc close";

/// Who is selected, if anyone.
#[derive(Resource, Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub uid: Option<u64>,
    /// The camera follows it.
    pub follow: bool,
    /// The chunk it was last seen in: where [`locate`] looks first.
    pub near: Option<ChunkCoord>,
    /// The tick it was last found missing (died, eaten, or unloaded).
    pub gone_at: Option<u64>,
}

impl Selection {
    pub fn select(&mut self, uid: u64, near: ChunkCoord) {
        *self = Self {
            uid: Some(uid),
            follow: self.follow && self.uid == Some(uid),
            near: Some(near),
            gone_at: None,
        };
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// What the panel shows this frame.
#[derive(Resource, Debug, Default, Clone)]
pub struct Inspector {
    /// Where the selected actor stands (`None`: nobody selected, or gone).
    pub at: Option<Pos>,
    /// Text rows, top to bottom, each in one colour.
    pub lines: Vec<(String, Color)>,
}

/// The actor to select on the cell at `p`: the standing one, else the
/// ground cover. Clicking the selected one again picks the other one, so
/// both layers of a cell can be inspected. Its uid and chunk; `None` if the
/// cell is empty or not loaded.
pub fn pick(world: &World, p: Pos, selected: Option<u64>) -> Option<(u64, ChunkCoord)> {
    if !p.in_world() {
        return None;
    }
    let (cc, i) = p.split();
    let e = world.resource::<Stage>().entity(cc)?;
    let cells = world.get::<ChunkCells>(e)?;
    let minds = world.get::<ChunkMinds>(e)?;
    let uid = |id: sim_core::ActorId| {
        id.unpack()
            .and_then(|(_, slot)| minds.rows.get(usize::from(slot)))
            .map(|m| m.uid)
    };
    let (standing, cover) = (uid(cells.occupant[i]), uid(cells.cover[i]));
    let uid = match (standing, cover) {
        (Some(s), Some(c)) if selected == Some(s) => c,
        (Some(s), _) => s,
        (None, c) => c?,
    };
    Some((uid, cc))
}

/// Where the actor `uid` is: its cell, chunk and row. First in chunk `near`
/// and the eight around it (an actor steps a cell per tick, so that is
/// where it is unless a frame ran many ticks), else anywhere loaded
/// (`sim::find_uid`, a scan of every row).
pub fn locate(world: &mut World, uid: u64, near: ChunkCoord) -> Option<(Pos, ChunkCoord, u16)> {
    let stage = world.resource::<Stage>();
    for k in 0..9 {
        let cc = ChunkCoord::new(near.x + k % 3 - 1, near.y + k / 3 - 1);
        let Some(e) = stage.entity(cc) else { continue };
        let (Some(minds), Some(pubs)) = (world.get::<ChunkMinds>(e), world.get::<ChunkActors>(e))
        else {
            continue;
        };
        if let Some(i) = minds.rows.iter().position(|m| m.uid == uid) {
            let slot = sim_core::actors::slot_u16(i);
            return Some((cc.cell(usize::from(pubs.rows[i].cell)), cc, slot));
        }
    }
    sim::find_uid(world, uid)
}

/// The panel for the actor at `p` whose next think is `e`, `width` columns
/// wide (longer lines are cut by the frame).
pub fn panel(
    kinds: &Kinds,
    tick: u64,
    p: Pos,
    e: &Explained,
    follow: bool,
    width: usize,
) -> Vec<(String, Color)> {
    let def = kinds.def(e.row.kind);
    let b = &e.before;
    let mut out: Vec<(String, Color)> = Vec::new();
    let mut line = |s: String, c: Color| out.push((s, c));

    line(
        format!(
            "{} '{}' at ({}, {}){}",
            def.name,
            char::from(def.glyph),
            p.x,
            p.y,
            if follow { "  FOLLOWING" } else { "" }
        ),
        TITLE,
    );
    line(format!("uid {:016x}", b.uid), DIM);
    if let Some(parents) = kinds.debug.parents.get(usize::from(e.row.kind))
        && !parents.is_empty()
    {
        line(format!("extends {}", parents.join(", ")), DIM);
    }
    line(
        format!(
            "age {} | state {}",
            duration(i64::from(stamp(tick).wrapping_sub(b.born))),
            state_name(kinds, e.row.kind, b.state)
        ),
        TEXT_FG,
    );
    line(
        format!(
            "look {} | signal {} | last {} | hurt {}{}",
            e.row.look,
            e.row.signal,
            result_name(b.events),
            b.hurt,
            if b.events & event::TAKEN != 0 {
                " | taken from"
            } else {
                ""
            }
        ),
        TEXT_FG,
    );
    line(
        format!(
            "thinks every {} ticks{}",
            def.cadence(),
            if e.due { ", due now" } else { "" }
        ),
        TEXT_FG,
    );

    // What it does next and why, first: the rules that fired, copied out
    // of the list below, then the decision.
    let rules = checked_rules(kinds, e);
    line(String::new(), TEXT_FG);
    line("next think".into(), TITLE);
    let at = |c: &Checked<'_>| {
        let via = c
            .rule
            .via
            .as_ref()
            .map_or(String::new(), |v| format!(", via {v}"));
        format!("    {}:{}{via}", c.file, c.rule.line)
    };
    if e.starved {
        line("  none: a vital need is empty, it dies".into(), ALERT);
    }
    for c in rules.iter().filter(|c| c.verdict == "FIRED") {
        line(format!("  {}", c.rule.text), FIRED);
        line(at(c), DIM);
    }
    for c in rules.iter().filter(|c| c.verdict == "TRAPPED") {
        line(format!("  trapped in: {}", c.rule.text), ALERT);
        line(at(c), DIM);
    }
    if !e.starved
        && rules
            .iter()
            .all(|c| c.verdict != "FIRED" && c.verdict != "TRAPPED")
    {
        line("  no rule fired".into(), DIM);
    }
    line(format!("  decides {}", decision(kinds, e)), TEXT_FG);
    let writes = writes(kinds, tick, e);
    for row in wrap(&writes, width.saturating_sub(9)) {
        line(format!("  writes {row}"), TEXT_FG);
    }
    let left = e.trace.last().map_or(def.fuel, |st| st.fuel);
    line(
        format!(
            "  cost {} ops, {} of {} fuel",
            e.outcome.used,
            def.fuel - left,
            def.fuel
        ),
        DIM,
    );
    if let Some(t) = e.outcome.trap {
        line(format!("  TRAPPED: {t:?} (the think becomes idle)"), ALERT);
    }

    if !def.needs.is_empty() {
        line(String::new(), TEXT_FG);
        line("needs".into(), TITLE);
        let names = def.needs.iter().map(|n| n.name.len()).max().unwrap_or(0);
        for (i, n) in def.needs.iter().enumerate() {
            // As the think sees it: decayed since the last one.
            let v = need_now(b.needs[i], n.decays, tick, b.last_think);
            let value = if n.decays {
                format!(
                    "{} / {}",
                    duration(i64::from(v)),
                    duration(i64::from(n.max))
                )
            } else {
                format!("{v} / {}", n.max)
            };
            let colour = if n.vital && v * 5 < n.max {
                ALERT
            } else {
                TEXT_FG
            };
            line(
                format!(
                    "  {:<names$} {} {value}{}",
                    n.name,
                    bar(v, n.max),
                    if n.vital { " vital" } else { "" }
                ),
                colour,
            );
        }
    }
    if !def.mems.is_empty() {
        line(String::new(), TEXT_FG);
        line("memory".into(), TITLE);
        let words: Vec<String> = def
            .mems
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{m} {}", b.mem[i]))
            .collect();
        for row in wrap(&words, width.saturating_sub(2)) {
            line(format!("  {row}"), TEXT_FG);
        }
    }

    // Every rule the think could reach, in the order it tries them.
    line(String::new(), TEXT_FG);
    line("rules".into(), TITLE);
    if rules.is_empty() {
        line("  (a hand-assembled kind: no source)".into(), DIM);
    }
    let mut shown_state = None;
    for c in &rules {
        let r = c.rule;
        if r.state.is_some() && shown_state != r.state {
            shown_state = r.state;
            line(
                format!(" state {}", state_name(kinds, e.row.kind, b.state)),
                TITLE,
            );
        }
        line(format!("  {}", r.text), TEXT_FG);
    }
    out
}

/// `[#####.....]`: `v` of `max` in ten steps.
fn bar(v: i32, max: i32) -> String {
    let filled = if max > 0 {
        usize::try_from((i64::from(v.clamp(0, max)) * 10 / i64::from(max)).clamp(0, 10))
            .unwrap_or(0)
    } else {
        0
    };
    format!("[{}{}]", "#".repeat(filled), ".".repeat(10 - filled))
}

/// `words` joined by two spaces into rows of at most `width` columns (a
/// longer word gets a row of its own).
fn wrap(words: &[String], width: usize) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    for w in words {
        match rows.last_mut() {
            Some(row) if row.len() + 2 + w.len() <= width => {
                row.push_str("  ");
                row.push_str(w);
            }
            _ => rows.push(w.clone()),
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim_core::actors::systems::newborn;
    use sim_core::rules::{CHICKEN, GRASS};

    fn world() -> (World, Kinds) {
        let kinds = Kinds::builtin();
        let cfg = sim_core::Scenario {
            width: 128,
            height: 64,
            seed: 5,
            ..Default::default()
        };
        (sim::new_world_with(&cfg, kinds.clone()).unwrap(), kinds)
    }

    /// A walkable cell with nobody on it in either layer, from `from` on.
    fn empty_cell(w: &World, from: i32) -> Pos {
        (from..128 * 64)
            .map(|i| Pos::new(i % 128, i / 128))
            .find(|&p| {
                let (cc, i) = p.split();
                let e = w.resource::<Stage>().entity(cc).unwrap();
                let c = w.get::<ChunkCells>(e).unwrap();
                c.walkable(i) && c.occupant[i].is_none() && c.cover[i].is_none()
            })
            .expect("an empty walkable cell")
    }

    #[test]
    fn a_click_picks_the_standing_actor_then_the_cover_under_it() {
        let (mut w, kinds) = world();
        let now = sim::tick(&w);
        let p = empty_cell(&w, 0);
        assert_eq!(pick(&w, p, None), None, "nobody there");
        assert!(sim::place_actor(
            &mut w,
            p,
            GRASS,
            newborn(&kinds, GRASS, 0x11, now)
        ));
        assert_eq!(pick(&w, p, None).unwrap().0, 0x11, "the cover alone");
        assert!(sim::place_actor(
            &mut w,
            p,
            CHICKEN,
            newborn(&kinds, CHICKEN, 0x22, now)
        ));
        assert_eq!(pick(&w, p, None).unwrap().0, 0x22, "standing first");
        assert_eq!(pick(&w, p, Some(0x22)).unwrap().0, 0x11, "again: the cover");
        assert_eq!(pick(&w, p, Some(0x11)).unwrap().0, 0x22, "and back");
        assert_eq!(pick(&w, p, None).unwrap().1, p.split().0);
        assert_eq!(
            pick(&w, Pos::new(i32::MAX, 0), None),
            None,
            "outside the world"
        );
    }

    #[test]
    fn locate_finds_an_actor_near_its_hint_and_far_from_it() {
        let (mut w, kinds) = world();
        let now = sim::tick(&w);
        // Chunk (1, 0): far from a hint at (-5, -5), next to one at (0, 0).
        let p = empty_cell(&w, 64 + 10 * 128);
        assert_eq!(p.split().0, ChunkCoord::new(1, 0));
        assert!(sim::place_actor(
            &mut w,
            p,
            CHICKEN,
            newborn(&kinds, CHICKEN, 0x33, now)
        ));
        let near = locate(&mut w, 0x33, ChunkCoord::new(0, 0)).unwrap();
        let far = locate(&mut w, 0x33, ChunkCoord::new(-5, -5)).unwrap();
        assert_eq!(near, far);
        assert_eq!((near.0, near.1), (p, ChunkCoord::new(1, 0)));
        assert_eq!(locate(&mut w, 0x44, ChunkCoord::new(0, 0)), None);
    }

    #[test]
    fn the_panel_shows_needs_memory_and_the_rule_that_fires() {
        let (mut w, kinds) = world();
        let now = sim::tick(&w);
        let p = empty_cell(&w, 0);
        assert!(sim::place_actor(
            &mut w,
            p,
            CHICKEN,
            newborn(&kinds, CHICKEN, 0xAB, now)
        ));
        let (_, cc, slot) = locate(&mut w, 0xAB, p.split().0).unwrap();
        let e = sim::explain_slot(&w, cc, slot).unwrap();
        let lines = panel(&kinds, sim::tick(&w), p, &e, true, 48);
        let text: Vec<&str> = lines.iter().map(|(s, _)| s.as_str()).collect();
        assert!(
            text[0].starts_with("chicken 'C' at") && text[0].ends_with("FOLLOWING"),
            "{text:?}"
        );
        assert!(text.iter().any(|l| l.starts_with("extends ")), "{text:?}");
        assert!(
            text.iter()
                .any(|l| l.trim_start().starts_with("food ") && l.contains("[##########]")),
            "{text:?}"
        );
        assert!(text.contains(&"memory"), "{text:?}");
        // The rule that fires is copied into the next-think section, and
        // listed again, plainly, under the rules.
        let next = text
            .iter()
            .position(|l| *l == "next think")
            .expect("a next-think section");
        let rules = text
            .iter()
            .position(|l| *l == "rules")
            .expect("a rules section");
        assert!(next < rules);
        let fired = &lines[next + 1];
        assert_eq!(fired.1, FIRED, "{text:?}");
        assert!(fired.0.starts_with("  when "), "{text:?}");
        assert!(text[next + 2].contains(".rules:"), "{text:?}");
        assert!(text[rules + 1..].contains(&fired.0.as_str()), "{text:?}");
        assert!(
            lines[rules + 1..]
                .iter()
                .all(|(l, c)| *c != FIRED && !l.contains("FIRED"))
        );
        assert!(
            text.iter()
                .all(|l| !l.starts_with("FIRED") && !l.starts_with("no "))
        );
        assert!(
            text[next..rules]
                .iter()
                .any(|l| l.starts_with("  decides ")),
            "{text:?}"
        );
    }

    #[test]
    fn bars_and_wrapping() {
        assert_eq!(bar(0, 10), "[..........]");
        assert_eq!(bar(5, 10), "[#####.....]");
        assert_eq!(bar(99, 10), "[##########]");
        assert_eq!(bar(-3, 10), "[..........]");
        assert_eq!(bar(1, 0), "[..........]");
        let words: Vec<String> = ["aa", "bbb", "cccc", "a-very-long-word"]
            .map(String::from)
            .into();
        assert_eq!(wrap(&words, 9), ["aa  bbb", "cccc", "a-very-long-word"]);
        assert!(wrap(&[], 9).is_empty());
    }

    #[test]
    fn selecting_another_actor_stops_following() {
        let mut s = Selection::default();
        s.select(1, ChunkCoord::new(0, 0));
        s.follow = true;
        s.select(1, ChunkCoord::new(1, 0));
        assert!(s.follow, "the same one again: still following");
        s.select(2, ChunkCoord::new(1, 0));
        assert!(!s.follow);
        s.clear();
        assert_eq!(s, Selection::default());
    }
}
