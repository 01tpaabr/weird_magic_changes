//! Scenarios: the physical world a save is generated from, and where each
//! kind starts (`docs/PLAN-8.md` §4). Everything else a world needs is its
//! rules. A scenario is a small text file:
//!
//! ```text
//! seed 12
//! size 256 256                                  # cells, rounded up to whole chunks
//! terrain water_level 0.18 rock_on_soil 0.02    # any GenParams field; the others default
//! start chicken 1 / 400                         # this share of walkable cells, by kind name
//! start hive at (77, 103)                       # exactly there
//! ```
//!
//! A scenario names kinds; resolved against a rule set it becomes a
//! [`Placement`]: the intervals worldgen cuts each walkable cell's
//! placement draw into, in the order the shares are written (so how the
//! rules number their kinds does not move anyone), and the explicit starts,
//! by chunk. A world keeps its starts by name in its save header, so every
//! chunk regenerates identically.

use std::fmt;

use crate::rules::Kinds;
use crate::stage::worldgen::{GenParams, gen_cell};
use crate::stage::{ChunkCoord, Pos};

/// A cell's placement draw is out of this: the top 24 bits of a cell hash,
/// exact in integers. A share `n / d` is `n * PLACE_ONE / d` of it.
pub const PLACE_ONE: u32 = 1 << 24;

/// Where a kind starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Start {
    /// `start K n / d`: this share of walkable cells.
    Share { kind: String, num: u32, den: u32 },
    /// `start K at (x, y)`: exactly there.
    At { kind: String, x: i32, y: i32 },
}

impl Start {
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
            Start::At { kind, x, y } => write!(f, "start {kind} at ({x}, {y})"),
        }
    }
}

/// A world's description: seed, initial size, terrain, starts.
#[derive(Debug, Clone, PartialEq)]
pub struct Scenario {
    pub seed: u64,
    /// The initially generated region, in cells, at `[0, w) x [0, h)`.
    pub width: u32,
    pub height: u32,
    pub params: GenParams,
    pub starts: Vec<Start>,
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
        }
    }
}

impl Scenario {
    /// The scenario every build carries (`scenarios/default.scenario`): what
    /// `show`, `play` and `run` start from when given none.
    pub const BUILTIN: &'static str = include_str!("../../../scenarios/default.scenario");

    pub fn builtin() -> Scenario {
        Self::parse("default.scenario", Self::BUILTIN).expect("the built-in scenario parses")
    }

    /// Parse a scenario's text. Unknown statements and terrain fields,
    /// shares that are not `0 < n / d <= 1`, two shares for one kind and
    /// shares summing above one are errors. Kind names are checked later,
    /// against the rules ([`Placement::resolve`]).
    pub fn parse(file: &str, text: &str) -> Result<Scenario, ScenarioError> {
        let mut s = Scenario::default();
        let mut total = 0u64;
        for (n, raw) in text.lines().enumerate() {
            let line = raw.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let err = |msg: String| ScenarioError {
                file: file.to_string(),
                line: n as u32 + 1,
                msg,
            };
            let mut words = line.split_whitespace();
            let head = words.next().unwrap_or("");
            let rest: Vec<&str> = words.collect();
            let int = |w: Option<&&str>, what: &str| -> Result<i64, ScenarioError> {
                w.and_then(|w| w.parse::<i64>().ok())
                    .ok_or_else(|| err(format!("expected {what}")))
            };
            match head {
                "seed" => {
                    s.seed = rest
                        .first()
                        .and_then(|w| w.parse().ok())
                        .ok_or_else(|| err("expected `seed N`".into()))?;
                    if rest.len() != 1 {
                        return Err(err("expected `seed N`".into()));
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
                        return Err(err("expected `size W H`, both at least 1".into()));
                    }
                    (s.width, s.height) = (w as u32, h as u32);
                }
                "terrain" => {
                    if rest.is_empty() || !rest.len().is_multiple_of(2) {
                        return Err(err("expected `terrain NAME VALUE ...`".into()));
                    }
                    for pair in rest.chunks(2) {
                        let v: f32 = pair[1]
                            .parse()
                            .ok()
                            .filter(|v: &f32| v.is_finite())
                            .ok_or_else(|| err(format!("`{}` is not a number", pair[1])))?;
                        if pair[0] == "water_scale" && v <= 0.0 {
                            return Err(err("water_scale is a size in cells, above 0".into()));
                        }
                        let field = match pair[0] {
                            "water_scale" => &mut s.params.water_scale,
                            "water_level" => &mut s.params.water_level,
                            "rock_on_soil" => &mut s.params.rock_on_soil,
                            "rock_on_water" => &mut s.params.rock_on_water,
                            other => {
                                return Err(err(format!(
                                    "unknown terrain field `{other}` (water_scale, water_level, rock_on_soil, rock_on_water)"
                                )));
                            }
                        };
                        *field = v;
                    }
                }
                "start" => {
                    let Some(kind) = rest.first() else {
                        return Err(err(
                            "expected `start KIND N / D` or `start KIND at (X, Y)`".into()
                        ));
                    };
                    if !kind.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                        return Err(err(format!("`{kind}` is not a kind name")));
                    }
                    let tail = rest[1..].join(" ");
                    if let Some(at) = tail.strip_prefix("at") {
                        let nums: Vec<&str> = at
                            .trim()
                            .trim_start_matches('(')
                            .trim_end_matches(')')
                            .split(',')
                            .map(str::trim)
                            .collect();
                        let (x, y) = match nums[..] {
                            [x, y] => (x.parse::<i32>(), y.parse::<i32>()),
                            _ => return Err(err("expected `start KIND at (X, Y)`".into())),
                        };
                        let (Ok(x), Ok(y)) = (x, y) else {
                            return Err(err("expected `start KIND at (X, Y)`".into()));
                        };
                        s.starts.push(Start::At {
                            kind: kind.to_string(),
                            x,
                            y,
                        });
                    } else {
                        let parts: Vec<&str> = tail.split('/').map(str::trim).collect();
                        let (num, den) = match parts[..] {
                            [n, d] => (n.parse::<u32>(), d.parse::<u32>()),
                            _ => return Err(err("expected `start KIND N / D`".into())),
                        };
                        let (Ok(num), Ok(den)) = (num, den) else {
                            return Err(err("expected `start KIND N / D`".into()));
                        };
                        if num == 0 || den == 0 || num > den {
                            return Err(err("a share is N / D with 0 < N <= D".into()));
                        }
                        if s.starts
                            .iter()
                            .any(|o| matches!(o, Start::Share { kind: k, .. } if k == kind))
                        {
                            return Err(err(format!("two shares for `{kind}`")));
                        }
                        let start = Start::Share {
                            kind: kind.to_string(),
                            num,
                            den,
                        };
                        total += u64::from(start.share());
                        if total > u64::from(PLACE_ONE) {
                            return Err(err("the shares add up to more than 1".into()));
                        }
                        s.starts.push(start);
                    }
                }
                other => {
                    return Err(err(format!(
                        "unknown statement `{other}` (seed, size, terrain, start)"
                    )));
                }
            }
        }
        Ok(s)
    }
}

/// A drawn map's terrain, for the save header (`docs/PLAN-8.md` §7, step
/// 8e): `width x height` cells from `(0, 0)`, one byte each (ground in the
/// low nibble, feature in the high one), and what lies outside it: `None`
/// is the seed's noise, else that cell byte everywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DrawnMap {
    pub width: u32,
    pub height: u32,
    pub cells: Vec<u8>,
    pub outside: Option<u8>,
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

/// A scenario's starts resolved against a kind table: what worldgen places.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placement {
    /// Cumulative upper bounds in the order the shares are written: a
    /// walkable cell whose draw is below a bound, and not below the
    /// previous one, starts that kind.
    bounds: Vec<(u32, Placed)>,
    /// `(chunk, local cell, kind)`, sorted by chunk `(y, x)` then cell.
    explicit: Vec<(ChunkCoord, u16, Placed)>,
}

impl Placement {
    /// Resolve `starts` against `kinds` for a world of `seed` and `params`.
    /// A start naming a kind the rules do not define (all of them are
    /// listed) or a trait, one on a cell that is not walkable, and two on
    /// one cell refuse it.
    pub fn resolve(
        starts: &[Start],
        kinds: &Kinds,
        seed: u64,
        params: &GenParams,
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
                Start::At { x, y, .. } => {
                    let (g, f) = gen_cell(seed, params, *x, *y);
                    if f.blocks() {
                        return Err(format!("`{s}` is on rock"));
                    }
                    if !g.walkable() {
                        return Err(format!("`{s}` is on water"));
                    }
                    let (c, i) = Pos::new(*x, *y).split();
                    explicit.push((c, i as u16, placed));
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
        explicit.sort_by_key(|&(c, i, _)| (c.y, c.x, i));
        if let Some(w) = explicit
            .windows(2)
            .find(|w| (w[0].0, w[0].1) == (w[1].0, w[1].1))
        {
            let p = w[0].0.cell(usize::from(w[0].1));
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

    /// The explicit starts in chunk `c`, by cell: `(chunk, local cell, kind)`.
    pub fn explicit_in(&self, c: ChunkCoord) -> &[(ChunkCoord, u16, Placed)] {
        let key = c.key();
        let lo = self.explicit.partition_point(|e| e.0.key() < key);
        let hi = self.explicit.partition_point(|e| e.0.key() <= key);
        &self.explicit[lo..hi]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stage::{CHUNK_CELLS, Feature, Ground};

    #[test]
    fn a_scenario_parses_with_comments_defaults_and_any_order() {
        let s = Scenario::parse(
            "t.scenario",
            "# a test world\nseed 12\nsize 256 128   # cells\nterrain water_level 0.3 rock_on_soil 0.01\n\nstart hive at (77, -3)\nstart chicken 1 / 400\nstart grass 1/20\n",
        )
        .unwrap();
        assert_eq!((s.seed, s.width, s.height), (12, 256, 128));
        assert_eq!(s.params.water_level, 0.3);
        assert_eq!(s.params.rock_on_soil, 0.01);
        assert_eq!(s.params.water_scale, GenParams::default().water_scale);
        assert_eq!(
            s.starts,
            [
                Start::At {
                    kind: "hive".into(),
                    x: 77,
                    y: -3
                },
                Start::Share {
                    kind: "chicken".into(),
                    num: 1,
                    den: 400
                },
                Start::Share {
                    kind: "grass".into(),
                    num: 1,
                    den: 20
                },
            ]
        );
        assert_eq!(s.starts[1].share(), PLACE_ONE / 400);
        assert_eq!(Scenario::parse("e", "").unwrap(), Scenario::default());
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
        ] {
            let e = Scenario::parse("t", text).unwrap_err().to_string();
            assert!(e.contains(want), "{text}: {e}");
        }
        // Exactly one is fine.
        Scenario::parse("t", "start a 1 / 2\nstart b 1 / 2").unwrap();
    }

    #[test]
    fn placement_cuts_shares_in_line_order_and_checks_starts() {
        let k =
            crate::rules::compile("t.rules", "kind a { }\nkind b { cover }\ntrait t { }").unwrap();
        let p = GenParams::default();
        let parse = |text: &str| Scenario::parse("t", text).unwrap().starts;
        let pl = Placement::resolve(&parse("start b 1 / 4\nstart a 1 / 2"), &k, 7, &p).unwrap();
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
            7,
            &p,
        )
        .unwrap_err();
        assert!(e.contains("do not define: wolf, fox"), "{e}");
        let e = Placement::resolve(&parse("start t 1 / 9"), &k, 7, &p).unwrap_err();
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
        let at = |(x, y): (i32, i32)| Start::At {
            kind: "b".into(),
            x,
            y,
        };
        let pl = Placement::resolve(&[at(far), at(dry)], &k, 7, &p).unwrap();
        let local = |(x, y): (i32, i32)| Pos::new(x, y).split().1 as u16;
        assert_eq!(pl.explicit_in(here), [(here, local(dry), b)]);
        assert_eq!(pl.explicit_in(there), [(there, local(far), b)]);
        assert!(pl.explicit_in(ChunkCoord::new(1, 0)).is_empty());
        for (cell, what) in [(wet, "is on water"), (rock, "is on rock")] {
            let e = Placement::resolve(&[at(cell)], &k, 7, &p).unwrap_err();
            assert!(e.contains(what), "{e}");
        }
        let e = Placement::resolve(&[at(dry), at(dry)], &k, 7, &p).unwrap_err();
        assert_eq!(e, format!("two starts at ({}, {})", dry.0, dry.1));
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

    #[test]
    fn the_builtin_scenario_parses_and_resolves_against_the_builtin_rules() {
        let s = Scenario::builtin();
        assert_eq!((s.seed, s.width, s.height), (42, 80, 24));
        assert_eq!(s.params, GenParams::default());
        assert_eq!(s.starts.len(), 6);
        let pl = Placement::resolve(&s.starts, &Kinds::builtin(), s.seed, &s.params).unwrap();
        assert!(pl.has_shares());
    }
}
