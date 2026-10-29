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
use sim_core::rules::RuleInfo;
use sim_core::rules::vm::{event, need_now};
use sim_core::time::stamp;
use sim_core::{ChunkActors, ChunkCells, ChunkCoord, ChunkMinds, KindDef, Kinds, Pos, Stage, sim};

use crate::highlight;
use crate::render::palette::{Color, TEXT_FG};
use crate::why::{checked_rules, decision, duration, result_name, state_name, writes};

/// Section titles.
pub const TITLE: Color = Color::rgb(0xf0, 0xc6, 0x74);
/// A rule whose condition trapped, or an actor that is gone.
pub const ALERT: Color = Color::rgb(0xff, 0x7b, 0x72);
/// Secondary text: where a rule is written, the cost.
pub const DIM: Color = Color::rgb(0xa0, 0xa0, 0xa8);
/// The last state of an actor that is gone.
pub const FAINT: Color = Color::rgb(0x6a, 0x6a, 0x72);
/// Who it is: the uid and the parents.
pub const PINK: Color = Color::rgb(0xf5, 0xa3, 0xd0);
/// The panel's last row.
pub const KEYS: &str = "click: select, or open a rule | f follow | esc close";
/// The rows an opened rule continues on start this far in.
const WRAP_INDENT: usize = 6;

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
    /// Its lines, top to bottom.
    pub lines: Vec<Line>,
}

/// One line of the panel: runs of one colour each, and the rule it shows,
/// if it is one (its `cond_pc`, unique in a rule set): a click opens it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Line {
    pub spans: Vec<(String, Color)>,
    pub rule: Option<u32>,
}

impl Line {
    pub fn plain(text: String, colour: Color) -> Self {
        Self {
            spans: vec![(text, colour)],
            rule: None,
        }
    }

    /// `prefix` in `colour`, then the rule's text in syntax colours.
    fn rule(kinds: &Kinds, def: &KindDef, prefix: &str, colour: Color, r: &RuleInfo) -> Self {
        let mut spans = vec![(prefix.to_string(), colour)];
        spans.extend(highlight::spans(kinds, def, &r.text));
        Self {
            spans,
            rule: Some(r.cond_pc),
        }
    }

    pub fn text(&self) -> String {
        self.spans.iter().map(|(t, _)| t.as_str()).collect()
    }
}

/// One row of the panel as drawn: a glyph and colour per cell, and the
/// line it belongs to (an opened rule takes several rows).
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub cells: Vec<(u8, Color)>,
    pub line: usize,
    pub rule: Option<u32>,
}

/// `lines` as rows `width` cells wide. A line that does not fit ends in
/// `...`, unless it is a rule `open` says is open: that one wraps, at a
/// space where it can, onto rows indented by [`WRAP_INDENT`].
pub fn rows(lines: &[Line], width: usize, open: impl Fn(u32) -> bool) -> Vec<Row> {
    let mut out = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        let cells: Vec<(u8, Color)> = l
            .spans
            .iter()
            .flat_map(|(t, c)| {
                t.chars()
                    .map(move |ch| (u8::try_from(ch).unwrap_or(b'?'), *c))
            })
            .collect();
        let row = |cells: Vec<(u8, Color)>| Row {
            cells,
            line: i,
            rule: l.rule,
        };
        if cells.len() <= width {
            out.push(row(cells));
        } else if l.rule.is_some_and(&open) && width > WRAP_INDENT + 8 {
            let mut rest = &cells[..];
            let mut first = true;
            while !rest.is_empty() {
                let room = if first { width } else { width - WRAP_INDENT };
                let mut take = rest.len().min(room);
                if take < rest.len()
                    && let Some(sp) = rest[..take].iter().rposition(|(b, _)| *b == b' ')
                    && sp > room / 2
                {
                    take = sp + 1;
                }
                let mut cells = if first {
                    Vec::new()
                } else {
                    vec![(b' ', TEXT_FG); WRAP_INDENT]
                };
                cells.extend_from_slice(&rest[..take]);
                out.push(row(cells));
                rest = &rest[take..];
                first = false;
            }
        } else {
            let keep = width.saturating_sub(3);
            let mut cut = cells[..keep].to_vec();
            cut.extend(std::iter::repeat_n((b'.', DIM), width - keep));
            out.push(row(cut));
        }
    }
    out
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
/// wide (memory and writes wrap to it; [`rows`] cuts or wraps the rest).
pub fn panel(
    kinds: &Kinds,
    tick: u64,
    p: Pos,
    e: &Explained,
    follow: bool,
    width: usize,
) -> Vec<Line> {
    let def = kinds.def(e.row.kind);
    let b = &e.before;
    let mut out: Vec<Line> = Vec::new();
    let line = |out: &mut Vec<Line>, s: String, c: Color| out.push(Line::plain(s, c));

    line(
        &mut out,
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
    line(&mut out, format!("uid {:016x}", b.uid), PINK);
    if let Some(parents) = kinds.debug.parents.get(usize::from(e.row.kind))
        && !parents.is_empty()
    {
        line(&mut out, format!("extends {}", parents.join(", ")), PINK);
    }
    line(
        &mut out,
        format!(
            "age {} | state {}",
            duration(i64::from(stamp(tick).wrapping_sub(b.born))),
            state_name(kinds, e.row.kind, b.state)
        ),
        TEXT_FG,
    );
    line(
        &mut out,
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
        &mut out,
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
    line(&mut out, String::new(), TEXT_FG);
    line(&mut out, "next think".into(), TITLE);
    if e.starved {
        line(
            &mut out,
            "  none: a vital need is empty, it dies".into(),
            ALERT,
        );
    }
    for c in rules.iter().filter(|c| c.verdict == "FIRED") {
        out.push(Line::rule(kinds, def, "  ", TEXT_FG, c.rule));
    }
    for c in rules.iter().filter(|c| c.verdict == "TRAPPED") {
        out.push(Line::rule(kinds, def, "  trapped in: ", ALERT, c.rule));
    }
    if !e.starved
        && rules
            .iter()
            .all(|c| c.verdict != "FIRED" && c.verdict != "TRAPPED")
    {
        line(&mut out, "  no rule fired".into(), DIM);
    }
    line(
        &mut out,
        format!("  decides {}", decision(kinds, e)),
        TEXT_FG,
    );
    let writes = writes(kinds, tick, e);
    for row in wrap(&writes, width.saturating_sub(9)) {
        line(&mut out, format!("  writes {row}"), TEXT_FG);
    }
    let left = e.trace.last().map_or(def.fuel, |st| st.fuel);
    line(
        &mut out,
        format!(
            "  cost {} ops, {} of {} fuel",
            e.outcome.used,
            def.fuel - left,
            def.fuel
        ),
        DIM,
    );
    if let Some(t) = e.outcome.trap {
        line(
            &mut out,
            format!("  TRAPPED: {t:?} (the think becomes idle)"),
            ALERT,
        );
    }

    if !def.needs.is_empty() {
        line(&mut out, String::new(), TEXT_FG);
        line(&mut out, "needs".into(), TITLE);
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
                &mut out,
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
        line(&mut out, String::new(), TEXT_FG);
        line(&mut out, "memory".into(), TITLE);
        let words: Vec<String> = def
            .mems
            .iter()
            .enumerate()
            .map(|(i, m)| format!("{m} {}", b.mem[i]))
            .collect();
        for row in wrap(&words, width.saturating_sub(2)) {
            line(&mut out, format!("  {row}"), TEXT_FG);
        }
    }

    // Every rule the think could reach, in the order it tries them.
    line(&mut out, String::new(), TEXT_FG);
    line(&mut out, "rules".into(), TITLE);
    if rules.is_empty() {
        line(&mut out, "  (a hand-assembled kind: no source)".into(), DIM);
    }
    let mut shown_state = None;
    for c in &rules {
        let r = c.rule;
        if r.state.is_some() && shown_state != r.state {
            shown_state = r.state;
            line(
                &mut out,
                format!(" state {}", state_name(kinds, e.row.kind, b.state)),
                TITLE,
            );
        }
        out.push(Line::rule(kinds, def, "  ", TEXT_FG, r));
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
        let text: Vec<String> = lines.iter().map(Line::text).collect();
        assert!(
            text[0].starts_with("chicken 'C' at") && text[0].ends_with("FOLLOWING"),
            "{text:?}"
        );
        let pink = |prefix: &str| {
            lines
                .iter()
                .find(|l| l.text().starts_with(prefix))
                .unwrap()
                .spans[0]
                .1
        };
        assert_eq!(pink("uid "), PINK);
        assert_eq!(pink("extends "), PINK);
        assert!(
            text.iter()
                .any(|l| l.trim_start().starts_with("food ") && l.contains("[##########]")),
            "{text:?}"
        );
        assert!(text.iter().any(|l| l == "memory"), "{text:?}");
        // The rules that fire are copied into the next-think section, with
        // no location, and listed again under the rules: the same rule, so
        // opening one opens both.
        let next = text.iter().position(|l| l == "next think").unwrap();
        let rules = text.iter().position(|l| l == "rules").unwrap();
        assert!(next < rules);
        let fired = &lines[next + 1];
        assert!(fired.text().starts_with("  when "), "{text:?}");
        assert!(fired.rule.is_some());
        assert!(
            !text[next..rules].iter().any(|l| l.contains(".rules:")),
            "{text:?}"
        );
        assert!(lines[rules + 1..].iter().any(|l| l == fired), "{text:?}");
        assert!(
            text[next..rules]
                .iter()
                .any(|l| l.starts_with("  decides ")),
            "{text:?}"
        );
        // Rule text is in syntax colours: `when` is a keyword.
        assert!(
            fired
                .spans
                .iter()
                .any(|(t, c)| t == "when" && *c == highlight::KEYWORD)
        );
        // Every rule of the list is a rule line.
        assert!(
            lines[rules + 1..]
                .iter()
                .all(|l| l.rule.is_some() || l.text().starts_with(" state "))
        );
    }

    #[test]
    fn long_lines_end_in_dots_and_an_open_rule_wraps() {
        let rule = Line {
            spans: vec![(
                "  when aaaa bbbb cccc dddd eeee ffff gggg => idle".into(),
                TEXT_FG,
            )],
            rule: Some(7),
        };
        let lines = vec![Line::plain("short".into(), TEXT_FG), rule.clone()];
        let shown = |rows: &[Row]| -> Vec<String> {
            rows.iter()
                .map(|r| r.cells.iter().map(|(b, _)| char::from(*b)).collect())
                .collect()
        };
        let closed = rows(&lines, 24, |_| false);
        assert_eq!(shown(&closed), ["short", "  when aaaa bbbb cccc..."]);
        assert_eq!(closed[1].cells[23].1, DIM);
        let open = rows(&lines, 24, |r| r == 7);
        assert_eq!(
            shown(&open),
            [
                "short",
                "  when aaaa bbbb cccc ",
                "      dddd eeee ffff ",
                "      gggg => idle"
            ]
        );
        assert!(open[1..].iter().all(|r| r.line == 1 && r.rule == Some(7)));
        // Every character of the rule is on some row, in order.
        let back: String = shown(&open[1..])
            .iter()
            .map(|r| r.trim_start())
            .collect::<Vec<_>>()
            .join("");
        assert_eq!(back.replace(' ', ""), rule.text().replace(' ', ""));
        // A plain line never wraps; a fitting one is whole.
        let wide = vec![Line::plain("x".repeat(30), TEXT_FG)];
        assert_eq!(rows(&wide, 24, |_| true).len(), 1);
        assert_eq!(shown(&rows(&wide, 40, |_| true)), ["x".repeat(30)]);
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
