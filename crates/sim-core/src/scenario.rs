//! Scenarios: the physical world a save is generated from, and where each
//! kind starts (`docs/RULES.md` §14, decision 36). Everything else a world needs is
//! its rules. A scenario is a small text file:
//!
//! ```text
//! seed 12
//! size 256 256                                  # cells, rounded up to whole chunks
//! terrain water_level 0.18 rock_on_soil 0.02    # any GenParams field; the others default
//! start chicken 1 / 400                         # this share of walkable cells, by kind name
//! start hive at (77, 103)                       # exactly there
//! start fox at (80, 100) with (food = 2h)       # newborn, but hungry
//! map {                                         # drawn cells from (0, 0); no comments inside
//!   ~~..#..
//!   ~.C..F.
//! }
//! legend {                                      # one entry per line
//!   ~ water
//!   . soil
//!   # rock
//!   C chicken
//!   F fox with (food = 2h)
//! }
//! outside soil                                  # beyond the map: noise (the default), soil, rock, water
//! run 2d                                        # a test: step, then check what happened
//! expect count chicken >= 10
//! ```
//!
//! A scenario names kinds; resolved against a rule set it becomes a
//! [`Placement`]: the intervals worldgen cuts each walkable cell's
//! placement draw into, in the order the shares are written (so how the
//! rules number their kinds does not move anyone), and the explicit starts,
//! by chunk. A world keeps its starts by name, and its drawn map, in its
//! save header, so every chunk regenerates identically.

use std::fmt;

use crate::rules::{Diagnostic, Kinds, Level};
use crate::stage::worldgen::{GenParams, Terrain};
use crate::stage::{ChunkCoord, Feature, Ground, Pos};

/// A cell's placement draw is out of this: the top 24 bits of a cell hash,
/// exact in integers. A share `n / d` is `n * PLACE_ONE / d` of it.
pub const PLACE_ONE: u32 = 1 << 24;

/// The most cells a drawn map may hold.
const MAP_CELLS: usize = 1 << 24;

/// Where a kind starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// `start K n / d`: this share of walkable cells.
    Share { kind: String, num: u32, den: u32 },
    /// `start K at (x, y) [with (name = v, ...)]`: exactly there, newborn
    /// but for the needs and memory `with` sets.
    At {
        kind: String,
        x: i32,
        y: i32,
        with: Vec<(String, i32)>,
    },
}

impl Start {
    /// `start K at (x, y)`, newborn.
    pub fn at(kind: &str, x: i32, y: i32) -> Start {
        Start::At {
            kind: kind.to_string(),
            x,
            y,
            with: Vec::new(),
        }
    }

    pub fn kind(&self) -> &str {
        match self {
            Start::Share { kind, .. } | Start::At { kind, .. } => kind,
        }
    }

    /// A share's part of [`PLACE_ONE`]; 0 for an explicit start.
    pub fn share(&self) -> u32 {
        match self {
            Start::Share { num, den, .. } => {
                (u64::from(*num) * u64::from(PLACE_ONE) / u64::from(*den)) as u32
            }
            Start::At { .. } => 0,
        }
    }
}

impl fmt::Display for Start {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Start::Share { kind, num, den } => write!(f, "start {kind} {num} / {den}"),
            Start::At { kind, x, y, with } => {
                write!(f, "start {kind} at ({x}, {y})")?;
                if !with.is_empty() {
                    let sets: Vec<String> =
                        with.iter().map(|(n, v)| format!("{n} = {v}")).collect();
                    write!(f, " with ({})", sets.join(", "))?;
                }
                Ok(())
            }
        }
    }
}

/// A comparison in an `expect`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    fn parse(w: &str) -> Option<Op> {
        Some(match w {
            "==" => Op::Eq,
            "!=" => Op::Ne,
            "<" => Op::Lt,
            "<=" => Op::Le,
            ">" => Op::Gt,
            ">=" => Op::Ge,
            _ => return None,
        })
    }

    /// Does `a OP b` hold?
    pub fn holds(self, a: i64, b: i64) -> bool {
        match self {
            Op::Eq => a == b,
            Op::Ne => a != b,
            Op::Lt => a < b,
            Op::Le => a <= b,
            Op::Gt => a > b,
            Op::Ge => a >= b,
        }
    }
}

/// Which actors an `expect` looks at: a kind's family, or `only` it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Who {
    pub kind: String,
    pub only: bool,
}

/// How `min|max|sum` folds a need or memory over a kind's actors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agg {
    Min,
    Max,
    Sum,
}

/// What an `expect` checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// `count [only] K OP N`: actors alive now.
    Count {
        who: Who,
        op: Op,
        n: i64,
    },
    /// `born|became|eaten|died [only] K OP N`: the life counters so far
    /// (`counter` is an `actors::life` index).
    Tally {
        counter: usize,
        who: Who,
        op: Op,
        n: i64,
    },
    /// `min|max|sum NAME of [only] K OP V`: a need or memory over every
    /// actor of the kind.
    Value {
        agg: Agg,
        name: String,
        who: Who,
        op: Op,
        v: i64,
    },
    /// `at (X, Y) [only] K` or `nobody`: the standing actor there, else the
    /// cover.
    At {
        x: i32,
        y: i32,
        who: Option<Who>,
    },
    /// `checksum HEX`: the world with the rules hash; `state HEX`: without.
    Checksum(u64),
    State(u64),
}

/// A scenario test's statements, in the order written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Check {
    /// `run T`: step T ticks.
    Run(u64),
    /// `expect ...`, with its line and text for the report.
    Expect {
        line: u32,
        text: String,
        what: Expect,
    },
}

/// `[only] K` at `words[i..]`: who, and the next index.
fn who_at(words: &[&str], i: usize) -> Option<(Who, usize)> {
    let who = |kind: &str, only| Who {
        kind: kind.to_string(),
        only,
    };
    match *words.get(i)? {
        "only" => {
            let k = words.get(i + 1).filter(|w| is_name(w))?;
            Some((who(k, true), i + 2))
        }
        k if is_name(k) => Some((who(k, false), i + 1)),
        _ => None,
    }
}

/// The words after `expect`.
fn expectation(rest: &str) -> Result<Expect, String> {
    const FORMS: &str = "expected `expect count|born|became|eaten|died [only] KIND OP N`, `expect min|max|sum NAME of [only] KIND OP V`, `expect at (X, Y) [only] KIND|nobody`, or `expect checksum|state HEX`";
    let words: Vec<&str> = rest.split_whitespace().collect();
    // `OP V` ending the line at `words[i..]`.
    let op_value = |i: usize| -> Result<(Op, i64), String> {
        match words[i.min(words.len())..] {
            [op, v] => Ok((
                Op::parse(op).ok_or_else(|| format!("`{op}` is not ==, !=, <, <=, > or >="))?,
                value(v).map(i64::from).ok_or_else(|| {
                    format!("`{v}` is not a number or a time (12, 90min, 2h, 1d)")
                })?,
            )),
            _ => Err(FORMS.into()),
        }
    };
    let counter = |w: &str| match w {
        "born" => Some(crate::actors::life::BORN),
        "became" => Some(crate::actors::life::BECAME),
        "eaten" => Some(crate::actors::life::EATEN),
        "died" => Some(crate::actors::life::DIED),
        _ => None,
    };
    let agg = |w: &str| match w {
        "min" => Some(Agg::Min),
        "max" => Some(Agg::Max),
        "sum" => Some(Agg::Sum),
        _ => None,
    };
    let first = *words.first().ok_or(FORMS)?;
    if first == "count" {
        let (who, i) = who_at(&words, 1).ok_or(FORMS)?;
        let (op, n) = op_value(i)?;
        return Ok(Expect::Count { who, op, n });
    }
    if let Some(counter) = counter(first) {
        let (who, i) = who_at(&words, 1).ok_or(FORMS)?;
        let (op, n) = op_value(i)?;
        return Ok(Expect::Tally {
            counter,
            who,
            op,
            n,
        });
    }
    if let Some(agg) = agg(first) {
        let name = words.get(1).filter(|w| is_name(w)).ok_or(FORMS)?;
        if words.get(2) != Some(&"of") {
            return Err(FORMS.into());
        }
        let (who, i) = who_at(&words, 3).ok_or(FORMS)?;
        let (op, v) = op_value(i)?;
        return Ok(Expect::Value {
            agg,
            name: name.to_string(),
            who,
            op,
            v,
        });
    }
    if first == "at" {
        let tail = rest.trim_start().strip_prefix("at").unwrap_or("").trim();
        let close = tail.find(')').ok_or(FORMS)?;
        let nums: Vec<&str> = tail[..close]
            .trim_start_matches('(')
            .split(',')
            .map(str::trim)
            .collect();
        let (Ok(x), Ok(y)) = (
            nums.first().copied().unwrap_or("").parse::<i32>(),
            nums.get(1).copied().unwrap_or("").parse::<i32>(),
        ) else {
            return Err(FORMS.into());
        };
        let after: Vec<&str> = tail[close + 1..].split_whitespace().collect();
        let who = match after[..] {
            ["nobody"] => None,
            _ => match who_at(&after, 0) {
                Some((who, i)) if i == after.len() => Some(who),
                _ => return Err(FORMS.into()),
            },
        };
        return Ok(Expect::At { x, y, who });
    }
    if let ("checksum" | "state", [hex]) = (first, &words[1..]) {
        let v =
            u64::from_str_radix(hex, 16).map_err(|_| format!("`{hex}` is not a hex checksum"))?;
        return Ok(if first == "checksum" {
            Expect::Checksum(v)
        } else {
            Expect::State(v)
        });
    }
    Err(FORMS.into())
}

/// A world's description: seed, initial size, terrain, starts, and maybe a
/// drawn map.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub seed: u64,
    /// The initially generated region, in cells, at `[0, w) x [0, h)`.
    pub width: u32,
    pub height: u32,
    pub params: GenParams,
    pub starts: Vec<Start>,
    /// Drawn cells from `(0, 0)`, and what lies beyond them.
    pub map: Option<DrawnMap>,
    /// A scenario test's `run` and `expect` lines (`wmc scenario`); nothing
    /// a world keeps.
    pub checks: Vec<Check>,
}

/// A scenario that does not parse: where and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioError {
    pub file: String,
    pub line: u32,
    pub msg: String,
}

impl fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.msg)
    }
}

impl std::error::Error for ScenarioError {}

impl Default for Scenario {
    /// Seed 42, 80 x 24 cells, default terrain, nobody.
    fn default() -> Self {
        Self {
            seed: 42,
            width: 80,
            height: 24,
            params: GenParams::default(),
            starts: Vec::new(),
            map: None,
            checks: Vec::new(),
        }
    }
}

/// What a legend character stands for.
#[derive(Debug, Clone)]
enum Legend {
    /// A terrain cell, as [`DrawnMap`] bytes.
    Cell(u8),
    /// A kind on soil, and the needs and memory it starts with.
    Kind {
        kind: String,
        with: Vec<(String, i32)>,
    },
}

/// A kind, memory or need name: letters, digits and `_`, not a leading digit.
fn is_name(w: &str) -> bool {
    !w.is_empty()
        && w.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !w.starts_with(|c: char| c.is_ascii_digit())
}

/// `12`, `-3`, `90min`, `2h`, `1d`: an integer, or a time in ticks (the
/// rules' units).
fn value(w: &str) -> Option<i32> {
    let (neg, w) = match w.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, w),
    };
    let digits = w.find(|c: char| !c.is_ascii_digit()).unwrap_or(w.len());
    if digits == 0 {
        return None;
    }
    let n: u64 = w[..digits].parse().ok()?;
    let ticks = match &w[digits..] {
        "" => n,
        "min" => crate::time::minutes(n),
        "h" => crate::time::hours(n),
        "d" => crate::time::days(n),
        _ => return None,
    };
    let v = i32::try_from(ticks).ok()?;
    Some(if neg { -v } else { v })
}

/// `with (name = v, ...)`: the needs and memory a start sets, by name.
fn with_list(text: &str) -> Result<Vec<(String, i32)>, String> {
    const FORM: &str = "expected `with (NAME = VALUE, ...)`";
    let inner = text
        .trim()
        .strip_prefix("with")
        .map(str::trim)
        .and_then(|t| t.strip_prefix('('))
        .and_then(|t| t.strip_suffix(')'))
        .ok_or(FORM)?;
    let mut out: Vec<(String, i32)> = Vec::new();
    for part in inner.split(',') {
        let (name, v) = part.split_once('=').ok_or(FORM)?;
        let (name, v) = (name.trim(), v.trim());
        if !is_name(name) {
            return Err(format!("`{name}` is not a need or memory name"));
        }
        let v = value(v)
            .ok_or_else(|| format!("`{v}` is not a number or a time (12, 90min, 2h, 1d)"))?;
        if out.iter().any(|(n, _)| n == name) {
            return Err(format!("`with` sets `{name}` twice"));
        }
        out.push((name.to_string(), v));
    }
    Ok(out)
}

impl Scenario {
    /// The scenario every build carries (`scenarios/default.scenario`): what
    /// `show`, `play` and `run` start from when given none.
    pub const BUILTIN: &'static str = include_str!("../../../scenarios/default.scenario");

    pub fn builtin() -> Scenario {
        Self::parse("default.scenario", Self::BUILTIN).expect("the built-in scenario parses")
    }

    /// The author lint's scenario check: the kinds that never appear in a
    /// world of this scenario under `kinds`. Nothing here starts them, and
    /// nothing that appears spawns or becomes them.
    pub fn unseen(&self, kinds: &Kinds) -> Vec<Diagnostic> {
        let mut seen = vec![false; kinds.len()];
        let mut stack: Vec<u16> = self
            .starts
            .iter()
            .filter_map(|s| kinds.by_name(s.kind()).map(|d| d.id))
            .collect();
        while let Some(k) = stack.pop() {
            if !std::mem::replace(&mut seen[usize::from(k)], true)
                && let Some(made) = kinds.debug.makes.get(usize::from(k))
            {
                stack.extend(made);
            }
        }
        (0..kinds.len())
            .filter(|&k| !seen[k])
            .map(|k| {
                let (file, line, col) = kinds.debug.kind_at.get(k).copied().unwrap_or_default();
                Diagnostic {
                    level: Level::Warning,
                    file: kinds
                        .debug
                        .files
                        .get(usize::from(file))
                        .cloned()
                        .unwrap_or_default(),
                    line,
                    col,
                    msg: format!(
                        "`{}` never appears: the scenario starts none, and nothing that appears spawns or becomes one",
                        kinds.defs[k].name
                    ),
                }
            })
            .collect()
    }

    /// The world's ground: its seed's noise under its drawn map.
    pub fn terrain(&self) -> Terrain<'_> {
        Terrain {
            seed: self.seed,
            params: &self.params,
            map: self.map.as_ref(),
        }
    }

    /// Parse a scenario's text. Unknown statements and terrain fields,
    /// shares that are not `0 < n / d <= 1`, two shares for one kind,
    /// shares summing above one, and a map that does not fit its legend or
    /// its size are errors. Kind, need and memory names are checked later,
    /// against the rules ([`Placement::resolve`]).
    pub fn parse(file: &str, text: &str) -> Result<Scenario, ScenarioError> {
        let mut s = Scenario::default();
        let mut total = 0u64;
        let mut size_line: Option<u32> = None;
        let mut rows: Option<(u32, Vec<(u32, String)>)> = None;
        let mut legend: Option<(u32, Vec<(u8, Legend, u32)>)> = None;
        let mut outside: Option<(u32, Option<u8>)> = None;
        let mut lines = text.lines().enumerate();
        while let Some((n, raw)) = lines.next() {
            let line_no = n as u32 + 1;
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let err = |line: u32, msg: String| ScenarioError {
                file: file.to_string(),
                line,
                msg,
            };
            let mut words = line.split_whitespace();
            let head = words.next().unwrap_or("");
            let rest: Vec<&str> = words.collect();
            let int = |w: Option<&&str>, what: &str| -> Result<i64, ScenarioError> {
                w.and_then(|w| w.parse::<i64>().ok())
                    .ok_or_else(|| err(line_no, format!("expected {what}")))
            };
            match head {
                "seed" => {
                    s.seed = rest
                        .first()
                        .and_then(|w| w.parse().ok())
                        .ok_or_else(|| err(line_no, "expected `seed N`".into()))?;
                    if rest.len() != 1 {
                        return Err(err(line_no, "expected `seed N`".into()));
                    }
                }
                "size" => {
                    let (w, h) = (
                        int(rest.first(), "`size W H`")?,
                        int(rest.get(1), "`size W H`")?,
                    );
                    if rest.len() != 2
                        || w < 1
                        || h < 1
                        || w > i64::from(u32::MAX)
                        || h > i64::from(u32::MAX)
                    {
                        return Err(err(line_no, "expected `size W H`, both at least 1".into()));
                    }
                    (s.width, s.height) = (w as u32, h as u32);
                    size_line = Some(line_no);
                }
                "terrain" => {
                    if rest.is_empty() || !rest.len().is_multiple_of(2) {
                        return Err(err(line_no, "expected `terrain NAME VALUE ...`".into()));
                    }
                    for pair in rest.chunks(2) {
                        let v: f32 = pair[1]
                            .parse()
                            .ok()
                            .filter(|v: &f32| v.is_finite())
                            .ok_or_else(|| {
                                err(line_no, format!("`{}` is not a number", pair[1]))
                            })?;
                        if pair[0] == "water_scale" && v <= 0.0 {
                            return Err(err(
                                line_no,
                                "water_scale is a size in cells, above 0".into(),
                            ));
                        }
                        let field = match pair[0] {
                            "water_scale" => &mut s.params.water_scale,
                            "water_level" => &mut s.params.water_level,
                            "rock_on_soil" => &mut s.params.rock_on_soil,
                            "rock_on_water" => &mut s.params.rock_on_water,
                            other => {
                                return Err(err(
                                    line_no,
                                    format!(
                                        "unknown terrain field `{other}` (water_scale, water_level, rock_on_soil, rock_on_water)"
                                    ),
                                ));
                            }
                        };
                        *field = v;
                    }
                }
                "start" => {
                    let Some(kind) = rest.first() else {
                        return Err(err(
                            line_no,
                            "expected `start KIND N / D` or `start KIND at (X, Y)`".into(),
                        ));
                    };
                    if !is_name(kind) {
                        return Err(err(line_no, format!("`{kind}` is not a kind name")));
                    }
                    let tail = rest[1..].join(" ");
                    if let Some(at) = tail.strip_prefix("at") {
                        let (pos, with) = match at.find("with") {
                            Some(i) => {
                                (&at[..i], with_list(&at[i..]).map_err(|m| err(line_no, m))?)
                            }
                            None => (at, Vec::new()),
                        };
                        let nums: Vec<&str> = pos
                            .trim()
                            .trim_start_matches('(')
                            .trim_end_matches(')')
                            .split(',')
                            .map(str::trim)
                            .collect();
                        let (x, y) = match nums[..] {
                            [x, y] => (x.parse::<i32>(), y.parse::<i32>()),
                            _ => {
                                return Err(err(line_no, "expected `start KIND at (X, Y)`".into()));
                            }
                        };
                        let (Ok(x), Ok(y)) = (x, y) else {
                            return Err(err(line_no, "expected `start KIND at (X, Y)`".into()));
                        };
                        s.starts.push(Start::At {
                            kind: kind.to_string(),
                            x,
                            y,
                            with,
                        });
                    } else {
                        let parts: Vec<&str> = tail.split('/').map(str::trim).collect();
                        let (num, den) = match parts[..] {
                            [n, d] => (n.parse::<u32>(), d.parse::<u32>()),
                            _ => return Err(err(line_no, "expected `start KIND N / D`".into())),
                        };
                        let (Ok(num), Ok(den)) = (num, den) else {
                            return Err(err(line_no, "expected `start KIND N / D`".into()));
                        };
                        if num == 0 || den == 0 || num > den {
                            return Err(err(line_no, "a share is N / D with 0 < N <= D".into()));
                        }
                        if s.starts
                            .iter()
                            .any(|o| matches!(o, Start::Share { kind: k, .. } if k == kind))
                        {
                            return Err(err(line_no, format!("two shares for `{kind}`")));
                        }
                        let start = Start::Share {
                            kind: kind.to_string(),
                            num,
                            den,
                        };
                        total += u64::from(start.share());
                        if total > u64::from(PLACE_ONE) {
                            return Err(err(line_no, "the shares add up to more than 1".into()));
                        }
                        s.starts.push(start);
                    }
                }
                "map" => {
                    if rest != ["{"] {
                        return Err(err(
                            line_no,
                            "expected `map {`, then the rows, then `}` on a line of its own".into(),
                        ));
                    }
                    if rows.is_some() {
                        return Err(err(line_no, "a second map".into()));
                    }
                    let mut drawn = Vec::new();
                    loop {
                        let Some((m, raw)) = lines.next() else {
                            return Err(err(line_no, "`map {` is never closed".into()));
                        };
                        let row = raw.trim();
                        if row == "}" {
                            break;
                        }
                        if !row.is_empty() {
                            drawn.push((m as u32 + 1, row.to_string()));
                        }
                    }
                    if drawn.is_empty() {
                        return Err(err(line_no, "an empty map".into()));
                    }
                    rows = Some((line_no, drawn));
                }
                "legend" => {
                    if rest != ["{"] {
                        return Err(err(
                            line_no,
                            "expected `legend {`, then one entry per line, then `}`".into(),
                        ));
                    }
                    if legend.is_some() {
                        return Err(err(line_no, "a second legend".into()));
                    }
                    let mut entries: Vec<(u8, Legend, u32)> = Vec::new();
                    loop {
                        let Some((m, raw)) = lines.next() else {
                            return Err(err(line_no, "`legend {` is never closed".into()));
                        };
                        let at = m as u32 + 1;
                        let entry = raw.trim();
                        if entry == "}" {
                            break;
                        }
                        if entry.is_empty() {
                            continue;
                        }
                        let key = entry.as_bytes()[0];
                        if !key.is_ascii_graphic() || key == b'{' || key == b'}' {
                            return Err(err(
                                at,
                                "a legend entry is one printable character (not `{` or `}`), a space, then what it stands for".into(),
                            ));
                        }
                        // After the character, `#` starts a comment.
                        let body = &entry[1..];
                        if !body.starts_with(char::is_whitespace) {
                            return Err(err(
                                at,
                                "a legend entry is one character, a space, then what it stands for"
                                    .into(),
                            ));
                        }
                        let body = body.split('#').next().unwrap_or("").trim();
                        let (name, tail) =
                            body.split_once(char::is_whitespace).unwrap_or((body, ""));
                        let cell = |g, f| Legend::Cell(DrawnMap::encode(g, f));
                        let what = match name {
                            "soil" => cell(Ground::Soil, Feature::None),
                            "water" => cell(Ground::Water, Feature::None),
                            "rock" => cell(Ground::Soil, Feature::Rock),
                            k if is_name(k) => Legend::Kind {
                                kind: k.to_string(),
                                with: if tail.trim().is_empty() {
                                    Vec::new()
                                } else {
                                    with_list(tail).map_err(|m| err(at, m))?
                                },
                            },
                            _ => {
                                return Err(err(
                                    at,
                                    format!("`{name}` is not soil, water, rock or a kind name"),
                                ));
                            }
                        };
                        if matches!(what, Legend::Cell(_)) && !tail.trim().is_empty() {
                            return Err(err(at, "`with` is for kinds, not terrain".into()));
                        }
                        if let Some((_, _, first)) = entries.iter().find(|e| e.0 == key) {
                            return Err(err(
                                at,
                                format!(
                                    "`{}` stands for two things (first at line {first})",
                                    char::from(key)
                                ),
                            ));
                        }
                        entries.push((key, what, at));
                    }
                    legend = Some((line_no, entries));
                }
                "outside" => {
                    let fill = match rest[..] {
                        ["noise"] => None,
                        ["soil"] => Some(DrawnMap::encode(Ground::Soil, Feature::None)),
                        ["water"] => Some(DrawnMap::encode(Ground::Water, Feature::None)),
                        ["rock"] => Some(DrawnMap::encode(Ground::Soil, Feature::Rock)),
                        _ => {
                            return Err(err(
                                line_no,
                                "expected `outside noise`, `soil`, `rock` or `water`".into(),
                            ));
                        }
                    };
                    outside = Some((line_no, fill));
                }
                "run" => {
                    let t = match rest[..] {
                        [t] => value(t).filter(|&t| t > 0),
                        _ => None,
                    }
                    .ok_or_else(|| {
                        err(
                            line_no,
                            "expected `run T`: ticks, or a time (90min, 2h, 1d)".into(),
                        )
                    })?;
                    s.checks.push(Check::Run(t as u64));
                }
                "expect" => {
                    let what = expectation(&rest.join(" ")).map_err(|m| err(line_no, m))?;
                    s.checks.push(Check::Expect {
                        line: line_no,
                        text: line.to_string(),
                        what,
                    });
                }
                other => {
                    return Err(err(
                        line_no,
                        format!(
                            "unknown statement `{other}` (seed, size, terrain, start, map, legend, outside, run, expect)"
                        ),
                    ));
                }
            }
        }
        let err = |line: u32, msg: String| ScenarioError {
            file: file.to_string(),
            line,
            msg,
        };
        let Some((map_line, drawn)) = rows else {
            if let Some((line, _)) = legend {
                return Err(err(line, "a legend without a map".into()));
            }
            if let Some((line, _)) = outside {
                return Err(err(line, "`outside` without a map".into()));
            }
            return Ok(s);
        };
        let Some((_, entries)) = legend else {
            return Err(err(map_line, "a map needs a legend".into()));
        };
        let width = drawn[0].1.len();
        for (at, row) in &drawn {
            if row.len() != width {
                return Err(err(
                    *at,
                    format!(
                        "this row has {} cells, the map's first row {width}",
                        row.len()
                    ),
                ));
            }
        }
        let height = drawn.len();
        if width * height > MAP_CELLS {
            return Err(err(map_line, format!("a map of at most {MAP_CELLS} cells")));
        }
        let (w, h) = (width as u32, height as u32);
        if let Some(at) = size_line
            && (s.width < w || s.height < h)
        {
            return Err(err(
                at,
                format!(
                    "`size {} {}` is smaller than the map, which is {w} x {h} (leave `size` out)",
                    s.width, s.height
                ),
            ));
        }
        let mut cells = Vec::with_capacity(width * height);
        for (y, (at, row)) in drawn.iter().enumerate() {
            for (x, b) in row.bytes().enumerate() {
                let Some((_, what, _)) = entries.iter().find(|e| e.0 == b) else {
                    return Err(err(
                        *at,
                        format!(
                            "`{}` at ({x}, {y}) is not in the legend (a map has no comments)",
                            char::from(b)
                        ),
                    ));
                };
                match what {
                    Legend::Cell(c) => cells.push(*c),
                    Legend::Kind { kind, with } => {
                        cells.push(DrawnMap::encode(Ground::Soil, Feature::None));
                        s.starts.push(Start::At {
                            kind: kind.clone(),
                            x: x as i32,
                            y: y as i32,
                            with: with.clone(),
                        });
                    }
                }
            }
        }
        if size_line.is_none() {
            (s.width, s.height) = (w, h);
        }
        s.map = Some(DrawnMap {
            width: w,
            height: h,
            cells,
            outside: outside.and_then(|(_, fill)| fill),
        });
        Ok(s)
    }
}

/// A drawn map's terrain: `width x height` cells from `(0, 0)`, one byte
/// each (ground in the low nibble, feature in the high one), and what lies
/// outside it: `None` is the seed's noise, else that cell byte everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawnMap {
    pub width: u32,
    pub height: u32,
    pub cells: Vec<u8>,
    pub outside: Option<u8>,
}

impl DrawnMap {
    /// A cell as a map byte.
    pub const fn encode(g: Ground, f: Feature) -> u8 {
        g as u8 | (f as u8) << 4
    }

    /// A map byte as a cell; `None` for a byte no cell encodes to.
    pub const fn decode(b: u8) -> Option<(Ground, Feature)> {
        let g = match b & 0x0F {
            0 => Ground::Soil,
            1 => Ground::Water,
            _ => return None,
        };
        let f = match b >> 4 {
            0 => Feature::None,
            1 => Feature::Rock,
            _ => return None,
        };
        Some((g, f))
    }

    /// Every byte a cell, and one per cell.
    pub fn validate(&self) -> Result<(), String> {
        if self.cells.len() != self.width as usize * self.height as usize {
            return Err(format!(
                "a {} x {} map with {} cells",
                self.width,
                self.height,
                self.cells.len()
            ));
        }
        match self
            .cells
            .iter()
            .chain(&self.outside)
            .find(|&&b| Self::decode(b).is_none())
        {
            Some(b) => Err(format!("map byte {b:#04x} is no cell")),
            None => Ok(()),
        }
    }

    /// The cell at `(x, y)`: drawn, else the outside fill; `None` where the
    /// seed's noise decides.
    #[inline]
    pub fn at(&self, x: i32, y: i32) -> Option<(Ground, Feature)> {
        let inside = x >= 0 && y >= 0 && (x as u32) < self.width && (y as u32) < self.height;
        let b = if inside {
            self.cells[y as usize * self.width as usize + x as usize]
        } else {
            self.outside?
        };
        Some(Self::decode(b).expect("map bytes are validated"))
    }
}

/// The starts naming a kind `kinds` defines (hot reload drops the others:
/// a kind that is gone starts nowhere).
pub fn present(starts: &[Start], kinds: &Kinds) -> Vec<Start> {
    starts
        .iter()
        .filter(|s| kinds.by_name(s.kind()).is_some())
        .cloned()
        .collect()
}

/// A kind as worldgen places it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub kind: u16,
    /// Ground cover: goes in the cell's cover layer.
    pub cover: bool,
}

/// An explicit start, resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Explicit {
    pub chunk: ChunkCoord,
    /// Its local cell.
    pub cell: u16,
    pub placed: Placed,
    /// What `with` sets, by slot: `(need, value)` and `(mem, value)`.
    pub needs: Vec<(u8, i32)>,
    pub mems: Vec<(u8, i32)>,
}

/// A scenario's starts resolved against a kind table: what worldgen places.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placement {
    /// Cumulative upper bounds in the order the shares are written: a
    /// walkable cell whose draw is below a bound, and not below the
    /// previous one, starts that kind.
    bounds: Vec<(u32, Placed)>,
    /// Sorted by chunk `(y, x)`, then cell.
    explicit: Vec<Explicit>,
}

impl Placement {
    /// Resolve `starts` against `kinds` for a world of `terrain`. A start
    /// naming a kind the rules do not define (all of them are listed) or a
    /// trait, one on a cell that is not walkable, two on one cell, and a
    /// `with` naming what the kind lacks or a need beyond its range refuse
    /// it.
    pub fn resolve(
        starts: &[Start],
        kinds: &Kinds,
        terrain: &Terrain,
    ) -> Result<Placement, String> {
        let mut missing: Vec<&str> = Vec::new();
        let mut shares: Vec<(Placed, u32)> = Vec::new();
        let mut explicit = Vec::new();
        for s in starts {
            let Some(def) = kinds.by_name(s.kind()) else {
                if kinds.debug.traits.iter().any(|t| t == s.kind()) {
                    return Err(format!("`{s}`: `{}` is a trait, not a kind", s.kind()));
                }
                if !missing.contains(&s.kind()) {
                    missing.push(s.kind());
                }
                continue;
            };
            let placed = Placed {
                kind: def.id,
                cover: def.cover,
            };
            match s {
                Start::Share { .. } => shares.push((placed, s.share())),
                Start::At { x, y, with, .. } => {
                    let (g, f) = terrain.cell(*x, *y);
                    if f.blocks() {
                        return Err(format!("`{s}` is on rock"));
                    }
                    if !g.walkable() {
                        return Err(format!("`{s}` is on water"));
                    }
                    let (mut needs, mut mems) = (Vec::new(), Vec::new());
                    for (name, v) in with {
                        if let Some(i) = def.need_named(name) {
                            let max = def.needs[i].max;
                            if !(0..=max).contains(v) {
                                return Err(format!("`{s}`: `{name}` holds 0 to {max}"));
                            }
                            needs.push((i as u8, *v));
                        } else if let Some(i) = def.mems.iter().position(|m| m == name) {
                            mems.push((i as u8, *v));
                        } else {
                            return Err(format!(
                                "`{s}`: `{}` has no need or memory `{name}`",
                                def.name
                            ));
                        }
                    }
                    let (chunk, i) = Pos::new(*x, *y).split();
                    explicit.push(Explicit {
                        chunk,
                        cell: i as u16,
                        placed,
                        needs,
                        mems,
                    });
                }
            }
        }
        if !missing.is_empty() {
            return Err(format!(
                "the scenario starts kinds the rules do not define: {}",
                missing.join(", ")
            ));
        }
        let mut bounds = Vec::with_capacity(shares.len());
        let mut upto = 0u64;
        for (placed, share) in shares {
            upto += u64::from(share);
            if upto > u64::from(PLACE_ONE) {
                return Err("the shares add up to more than 1".into());
            }
            bounds.push((upto as u32, placed));
        }
        explicit.sort_by_key(|e| (e.chunk.y, e.chunk.x, e.cell));
        if let Some(w) = explicit
            .windows(2)
            .find(|w| (w[0].chunk, w[0].cell) == (w[1].chunk, w[1].cell))
        {
            let p = w[0].chunk.cell(usize::from(w[0].cell));
            return Err(format!("two starts at ({}, {})", p.x, p.y));
        }
        Ok(Placement { bounds, explicit })
    }

    /// What a walkable cell whose placement draw is `u` (`0..PLACE_ONE`)
    /// starts as, by the shares.
    #[inline]
    pub fn placed(&self, u: u32) -> Option<Placed> {
        self.bounds
            .iter()
            .find(|&&(upto, _)| u < upto)
            .map(|&(_, p)| p)
    }

    /// Any shares at all?
    pub fn has_shares(&self) -> bool {
        !self.bounds.is_empty()
    }

    /// The explicit starts in chunk `c`, by cell.
    pub fn explicit_in(&self, c: ChunkCoord) -> &[Explicit] {
        let key = c.key();
        let lo = self.explicit.partition_point(|e| e.chunk.key() < key);
        let hi = self.explicit.partition_point(|e| e.chunk.key() <= key);
        &self.explicit[lo..hi]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::CHUNK_CELLS;
    use crate::stage::worldgen::gen_cell;

    fn noise(seed: u64, p: &GenParams) -> Terrain<'_> {
        Terrain {
            seed,
            params: p,
            map: None,
        }
    }

    #[test]
    fn a_scenario_parses_with_comments_defaults_and_any_order() {
        let s = Scenario::parse(
            "t.scenario",
            "# a test world\nseed 12\nsize 256 128   # cells\nterrain water_level 0.3 rock_on_soil 0.01\n\nstart hive at (77, -3)\nstart chicken 1 / 400\nstart grass 1/20\nstart fox at (1, 2) with (food = 2h, chase = -3)\n",
        )
        .unwrap();
        assert_eq!((s.seed, s.width, s.height), (12, 256, 128));
        assert_eq!(s.params.water_level, 0.3);
        assert_eq!(s.params.rock_on_soil, 0.01);
        assert_eq!(s.params.water_scale, GenParams::default().water_scale);
        let share = |kind: &str, num, den| Start::Share {
            kind: kind.into(),
            num,
            den,
        };
        assert_eq!(
            s.starts,
            [
                Start::at("hive", 77, -3),
                share("chicken", 1, 400),
                share("grass", 1, 20),
                Start::At {
                    kind: "fox".into(),
                    x: 1,
                    y: 2,
                    with: vec![("food".into(), 1800), ("chase".into(), -3)],
                },
            ]
        );
        assert_eq!(s.starts[1].share(), PLACE_ONE / 400);
        assert_eq!(
            s.starts[3].to_string(),
            "start fox at (1, 2) with (food = 1800, chase = -3)"
        );
        assert_eq!(s.map, None);
        assert_eq!(Scenario::parse("e", "").unwrap(), Scenario::default());
    }

    /// A scenario test's lines, every form, in the order written.
    #[test]
    fn run_and_expect_lines_parse_in_order() {
        let s = Scenario::parse(
            "t",
            "run 2d
             expect count chicken == 0
             expect eaten only chick >= 2      # the tally
             run 90
             expect max food of fox > 23h
             expect at (77, -3) hive
             expect at (1, 2) nobody
             expect checksum 8e1fd4fd7f84a868
             expect state 00ff",
        )
        .unwrap();
        let who = |kind: &str, only| Who {
            kind: kind.into(),
            only,
        };
        let whats: Vec<&Expect> = s
            .checks
            .iter()
            .filter_map(|c| match c {
                Check::Expect { what, .. } => Some(what),
                Check::Run(_) => None,
            })
            .collect();
        assert_eq!(s.checks[0], Check::Run(crate::time::days(2)));
        assert_eq!(s.checks[3], Check::Run(90));
        assert_eq!(
            whats,
            [
                &Expect::Count {
                    who: who("chicken", false),
                    op: Op::Eq,
                    n: 0
                },
                &Expect::Tally {
                    counter: crate::actors::life::EATEN,
                    who: who("chick", true),
                    op: Op::Ge,
                    n: 2
                },
                &Expect::Value {
                    agg: Agg::Max,
                    name: "food".into(),
                    who: who("fox", false),
                    op: Op::Gt,
                    v: crate::time::hours(23) as i64
                },
                &Expect::At {
                    x: 77,
                    y: -3,
                    who: Some(who("hive", false))
                },
                &Expect::At {
                    x: 1,
                    y: 2,
                    who: None
                },
                &Expect::Checksum(0x8e1f_d4fd_7f84_a868),
                &Expect::State(0xff),
            ]
        );
        match &s.checks[2] {
            Check::Expect { line, text, .. } => {
                assert_eq!((*line, text.as_str()), (3, "expect eaten only chick >= 2"));
            }
            other => panic!("{other:?}"),
        }
        assert!(Op::Le.holds(3, 3) && !Op::Lt.holds(3, 3) && Op::Ne.holds(1, 2));
        for (text, want) in [
            ("run", "expected `run T`"),
            ("run 0", "expected `run T`"),
            ("expect", "expected `expect count"),
            ("expect count chicken", "expected `expect count"),
            (
                "expect count chicken = 3",
                "`=` is not ==, !=, <, <=, > or >=",
            ),
            (
                "expect count chicken == lots",
                "`lots` is not a number or a time",
            ),
            ("expect max food chicken > 1", "expected `expect count"),
            ("expect at 3 chicken", "expected `expect count"),
            ("expect at (1, 2) hen fox", "expected `expect count"),
            ("expect checksum zz", "`zz` is not a hex checksum"),
            ("expect nothing", "expected `expect count"),
        ] {
            let e = Scenario::parse("t", text).unwrap_err().to_string();
            assert!(e.contains(want), "{text}: {e}");
        }
    }

    #[test]
    fn scenario_errors_name_the_line_and_the_problem() {
        for (text, want) in [
            ("seed x", "t:1: expected `seed N`"),
            ("\nsize 0 5", "t:2: expected `size W H`"),
            ("terrain lava 1", "unknown terrain field `lava`"),
            ("terrain water_level", "expected `terrain NAME VALUE"),
            ("start chicken 0 / 4", "0 < N <= D"),
            ("start chicken 5 / 4", "0 < N <= D"),
            (
                "start chicken 1 / 2\nstart chicken 1 / 3",
                "t:2: two shares for `chicken`",
            ),
            ("start a 1 / 2\nstart b 2 / 3", "add up to more than 1"),
            ("start hive at 3", "expected `start KIND at (X, Y)`"),
            ("spawn fox", "unknown statement `spawn`"),
            ("terrain water_level nan", "`nan` is not a number"),
            ("terrain water_scale 0", "above 0"),
            (
                "start fox at (1, 2) with food = 2h",
                "expected `with (NAME = VALUE, ...)`",
            ),
            (
                "start fox at (1, 2) with (food = soon)",
                "`soon` is not a number or a time",
            ),
            (
                "start fox at (1, 2) with (food = 1h, food = 2h)",
                "`with` sets `food` twice",
            ),
            ("start 9lives at (1, 2)", "`9lives` is not a kind name"),
        ] {
            let e = Scenario::parse("t", text).unwrap_err().to_string();
            assert!(e.contains(want), "{text}: {e}");
        }
        // Exactly one is fine.
        Scenario::parse("t", "start a 1 / 2\nstart b 1 / 2").unwrap();
    }

    const PEN: &str = "seed 3
outside soil          # beyond the map
map {
  ~~.....
  .###...
  .FC#..#
  .###...
}
legend {
  . soil
  # rock            # rock on soil
  ~ water
  C chicken
  F fox with (food = 2h)
}
";

    #[test]
    fn a_drawn_map_parses_into_cells_and_starts() {
        let s = Scenario::parse("pen.scenario", PEN).unwrap();
        let m = s.map.as_ref().unwrap();
        assert_eq!((s.width, s.height, m.width, m.height), (7, 4, 7, 4));
        let (soil, rock, water) = (
            DrawnMap::encode(Ground::Soil, Feature::None),
            DrawnMap::encode(Ground::Soil, Feature::Rock),
            DrawnMap::encode(Ground::Water, Feature::None),
        );
        assert_eq!(&m.cells[..7], [water, water, soil, soil, soil, soil, soil]);
        assert_eq!(m.cells[2 * 7 + 1], soil, "a kind stands on soil");
        assert_eq!(m.cells[2 * 7 + 6], rock);
        assert_eq!(m.outside, Some(soil));
        assert_eq!(
            s.starts,
            [
                Start::At {
                    kind: "fox".into(),
                    x: 1,
                    y: 2,
                    with: vec![("food".into(), 1800)],
                },
                Start::at("chicken", 2, 2),
            ]
        );
        assert_eq!(m.at(1, 1), Some((Ground::Soil, Feature::Rock)));
        assert_eq!(m.at(0, 0), Some((Ground::Water, Feature::None)));
        assert_eq!(m.at(-5, 90), Some((Ground::Soil, Feature::None)), "outside");
        let noise = DrawnMap {
            outside: None,
            ..m.clone()
        };
        assert_eq!(noise.at(7, 0), None, "noise beyond the map");
        assert!(m.validate().is_ok());
        let bad = DrawnMap {
            cells: vec![0x22; 28],
            ..m.clone()
        };
        assert!(bad.validate().unwrap_err().contains("0x22"));
        // A size that matches is fine; the terrain under a map is the map.
        let sized = Scenario::parse("t", &format!("size 7 4\n{PEN}")).unwrap();
        assert_eq!(sized.map, s.map);
        let p = GenParams::default();
        let t = s.terrain();
        assert_eq!(t.cell(3, 1), (Ground::Soil, Feature::Rock));
        assert_eq!(t.cell(500, 500), (Ground::Soil, Feature::None));
        let n = Terrain {
            map: Some(&noise),
            ..t
        };
        assert_eq!(n.cell(500, 500), gen_cell(3, &p, 500, 500));
    }

    #[test]
    fn map_errors_name_the_line() {
        let legend = "legend {\n  . soil\n  C chicken\n}\n";
        for (text, want) in [
            (
                format!("map {{\n..\n...\n}}\n{legend}"),
                "t:3: this row has 3 cells, the map's first row 2",
            ),
            (
                format!("map {{\n.x\n}}\n{legend}"),
                "t:2: `x` at (1, 0) is not in the legend",
            ),
            (
                format!("size 1 1\nmap {{\n..\n}}\n{legend}"),
                "t:1: `size 1 1` is smaller than the map, which is 2 x 1",
            ),
            ("map {\n..\n}\n".to_string(), "t:1: a map needs a legend"),
            (legend.to_string(), "t:1: a legend without a map"),
            ("outside soil\n".to_string(), "`outside` without a map"),
            ("map {\n..\n".to_string(), "`map {` is never closed"),
            ("map {\n}\n".to_string(), "an empty map"),
            ("map { .. }\n".to_string(), "expected `map {`"),
            ("legend { . soil }\n".to_string(), "expected `legend {`"),
            (
                "map {\n..\n}\nlegend {\n  . soil\n  . water\n}".to_string(),
                "t:6: `.` stands for two things (first at line 5)",
            ),
            (
                "map {\n..\n}\nlegend {\n  .soil\n}".to_string(),
                "one character, a space",
            ),
            (
                "map {\n..\n}\nlegend {\n  . lava!\n}".to_string(),
                "`lava!` is not soil, water, rock or a kind name",
            ),
            (
                "map {\n..\n}\nlegend {\n  . soil with (food = 1)\n}".to_string(),
                "`with` is for kinds",
            ),
            (
                format!("map {{\n..\n}}\n{legend}outside lava\n"),
                "expected `outside noise`",
            ),
            (
                format!("map {{\n..\n}}\nmap {{\n..\n}}\n{legend}"),
                "a second map",
            ),
        ] {
            let e = Scenario::parse("t", &text).unwrap_err().to_string();
            assert!(e.contains(want), "{text}: {e}");
        }
    }

    #[test]
    fn placement_cuts_shares_in_line_order_and_checks_starts() {
        let k =
            crate::rules::compile("t.rules", "kind a { }\nkind b { cover }\ntrait t { }").unwrap();
        let p = GenParams::default();
        let t = noise(7, &p);
        let parse = |text: &str| Scenario::parse("t", text).unwrap().starts;
        let pl = Placement::resolve(&parse("start b 1 / 4\nstart a 1 / 2"), &k, &t).unwrap();
        // In the order written, whatever the kind ids; `b` is cover.
        let (a, b) = (
            Placed {
                kind: 0,
                cover: false,
            },
            Placed {
                kind: 1,
                cover: true,
            },
        );
        assert_eq!(pl.placed(0), Some(b));
        assert_eq!(pl.placed(PLACE_ONE / 4 - 1), Some(b));
        assert_eq!(pl.placed(PLACE_ONE / 4), Some(a));
        assert_eq!(pl.placed(PLACE_ONE / 4 * 3 - 1), Some(a));
        assert_eq!(pl.placed(PLACE_ONE / 4 * 3), None);
        assert!(pl.has_shares() && !Placement::default().has_shares());
        let e = Placement::resolve(
            &parse("start wolf 1 / 9\nstart fox at (1, 1)\nstart wolf at (2, 2)"),
            &k,
            &t,
        )
        .unwrap_err();
        assert!(e.contains("do not define: wolf, fox"), "{e}");
        let e = Placement::resolve(&parse("start t 1 / 9"), &k, &t).unwrap_err();
        assert!(e.contains("`t` is a trait"), "{e}");
        // Explicit starts: walkable cells only, one start per cell, found by chunk.
        let find = |c: ChunkCoord, want: fn(Ground, Feature) -> bool| {
            (0..CHUNK_CELLS)
                .map(|i| c.cell(i))
                .find(|q| {
                    let (g, f) = gen_cell(7, &p, q.x, q.y);
                    want(g, f)
                })
                .map(|q| (q.x, q.y))
                .unwrap()
        };
        let (here, there) = (ChunkCoord::new(0, 0), ChunkCoord::new(-2, 3));
        let dry = find(here, |g, f| g.walkable() && !f.blocks());
        let far = find(there, |g, f| g.walkable() && !f.blocks());
        let wet = find(here, |g, f| !g.walkable() && !f.blocks());
        let rock = find(here, |_, f| f.blocks());
        let at = |(x, y): (i32, i32)| Start::at("b", x, y);
        let pl = Placement::resolve(&[at(far), at(dry)], &k, &t).unwrap();
        let local = |(x, y): (i32, i32)| Pos::new(x, y).split().1 as u16;
        let only = |chunk, cell| Explicit {
            chunk,
            cell,
            placed: b,
            needs: vec![],
            mems: vec![],
        };
        assert_eq!(pl.explicit_in(here), [only(here, local(dry))]);
        assert_eq!(pl.explicit_in(there), [only(there, local(far))]);
        assert!(pl.explicit_in(ChunkCoord::new(1, 0)).is_empty());
        for (cell, what) in [(wet, "is on water"), (rock, "is on rock")] {
            let e = Placement::resolve(&[at(cell)], &k, &t).unwrap_err();
            assert!(e.contains(what), "{e}");
        }
        let e = Placement::resolve(&[at(dry), at(dry)], &k, &t).unwrap_err();
        assert_eq!(e, format!("two starts at ({}, {})", dry.0, dry.1));
    }

    /// `with` on a start names needs and memory; the map's terrain decides
    /// where a start may stand.
    #[test]
    fn starts_set_needs_and_memory_by_name_on_the_map() {
        let k = crate::rules::compile(
            "t.rules",
            "kind chicken { mem seen } kind fox { need food max 1d vital  mem chase }",
        )
        .unwrap();
        let s = Scenario::parse(
            "pen.scenario",
            &format!("{PEN}start fox at (4, 0) with (chase = 7)\n"),
        )
        .unwrap();
        let pl = Placement::resolve(&s.starts, &k, &s.terrain()).unwrap();
        let e = pl.explicit_in(ChunkCoord::new(0, 0));
        assert_eq!(e.len(), 3);
        assert_eq!(
            (e[0].cell, &e[0].needs[..], &e[0].mems[..]),
            (4, &[][..], &[(0, 7)][..])
        );
        assert_eq!((e[1].cell, &e[1].needs[..]), (2 * 64 + 1, &[(0, 1800)][..]));
        assert_eq!((e[2].cell, e[2].placed.kind), (2 * 64 + 2, 0));
        for (extra, want) in [
            ("start fox at (0, 0)", "`start fox at (0, 0)` is on water"),
            ("start fox at (3, 2)", "is on rock"),
            (
                "start fox at (5, 1) with (food = 2d)",
                "`food` holds 0 to 21600",
            ),
            (
                "start fox at (5, 1) with (sleep = 1)",
                "`fox` has no need or memory `sleep`",
            ),
            ("start chicken at (2, 2)", "two starts at (2, 2)"),
        ] {
            let s = Scenario::parse("t", &format!("{PEN}{extra}\n")).unwrap();
            let e = Placement::resolve(&s.starts, &k, &s.terrain()).unwrap_err();
            assert!(e.contains(want), "{extra}: {e}");
        }
    }

    #[test]
    fn present_keeps_the_starts_of_kinds_there() {
        let k = crate::rules::compile("t.rules", "kind a { }").unwrap();
        let starts = Scenario::parse(
            "t",
            "start a 1 / 2\nstart b 1 / 4\nstart b at (0, 0)\nstart a at (1, 1)",
        )
        .unwrap()
        .starts;
        assert_eq!(present(&starts, &k), [starts[0].clone(), starts[3].clone()]);
    }

    /// The scenarios in docs/RULES.md §14 parse and fit the built-in rules.
    #[test]
    fn the_rules_md_scenarios_parse_and_resolve() {
        let doc = include_str!("../../../docs/RULES.md");
        let from = doc.find("## 14. Scenarios").unwrap();
        let to = doc.find("## 15. Packs").unwrap();
        let blocks: Vec<&str> = doc[from..to].split("```").skip(1).step_by(2).collect();
        assert_eq!(blocks.len(), 3);
        for text in blocks {
            let s = Scenario::parse("RULES.md", text).unwrap_or_else(|e| panic!("{e}"));
            Placement::resolve(&s.starts, &Kinds::builtin(), &s.terrain())
                .unwrap_or_else(|e| panic!("{e}"));
        }
    }

    /// Every built-in kind appears in the default world; the fox pen starts
    /// hens and foxes, which lay eggs that hatch into chicks, and nothing
    /// else.
    #[test]
    fn unseen_kinds_follow_starts_spawns_and_becomes() {
        let k = Kinds::builtin();
        assert!(Scenario::builtin().unseen(&k).is_empty());
        let pen = Scenario::parse(
            "p",
            include_str!("../../../scenarios/tests/fox_pen.scenario"),
        )
        .unwrap();
        let never: Vec<String> = pen.unseen(&k).iter().map(|d| d.msg.clone()).collect();
        let names: Vec<&str> = never.iter().map(|m| m.split('`').nth(1).unwrap()).collect();
        assert_eq!(names, ["flower", "hive", "bee", "grass", "seed", "tree"]);
        let d = &pen.unseen(&k)[0];
        assert_eq!((d.file.as_str(), d.line), ("bees.rules", 18));
    }

    #[test]
    fn the_builtin_scenario_parses_and_resolves_against_the_builtin_rules() {
        let s = Scenario::builtin();
        assert_eq!((s.seed, s.width, s.height), (42, 80, 24));
        assert_eq!(s.params, GenParams::default());
        assert_eq!(s.starts.len(), 6);
        let pl = Placement::resolve(&s.starts, &Kinds::builtin(), &s.terrain()).unwrap();
        assert!(pl.has_shares());
    }
}
