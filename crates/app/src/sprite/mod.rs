//! Sprites: a kind drawn as a small picture made of letters
//! (`docs/SPRITES.md`). A `*.sprite` file beside a pack's `*.rules` files
//! names the kinds it draws; each sprite is a list of stamps (a character
//! placed, turned, scaled and coloured), with frames and poses. The sim
//! never sees any of it: only the app reads sprite files, and only to draw.
//!
//! - [`parse`]: text to a [`SpriteFile`] (grammar: `docs/SPRITES.md` §10);
//!   [`SpriteFile::text`] writes one back.
//! - [`sources`] and [`load`]: the sprite files of a rule set's packs,
//!   matched to its kinds ([`Wardrobe`]), with warnings for `wmc lint`.

mod parse;
pub mod raster;
pub mod sheet;
pub mod tiles;

#[cfg(test)]
pub mod gen_sprites;

#[cfg(test)]
mod fuzz;

use std::fmt;
use std::path::{Path, PathBuf};

use sim_core::Kinds;
use sim_core::rules::{Diagnostic, Level};

pub use parse::{milli_text, parse, rgb};

/// Stamps in one pose (the sprite's own list is one).
pub const MAX_STAMPS: usize = 32;
/// Frames of one pose.
pub const MAX_FRAMES: u8 = 16;
/// Poses of a sprite, its own stamp list counted as one.
pub const MAX_POSES: usize = 8;
/// Colours in one file's palette.
pub const MAX_PALETTE: usize = 64;
/// The shortest and longest a frame may be shown, in milliseconds.
pub const MIN_EVERY_MS: u32 = 50;
pub const MAX_EVERY_MS: u32 = 60_000;
/// A stamp's centre, in thousandths of a cell from the top-left: 0 to 16
/// cells, the largest footprint a kind can have.
pub const MAX_XY: i32 = 16_000;
/// A stamp's height, in thousandths of a cell.
pub const MIN_SCALE: i32 = 100;
pub const MAX_SCALE: i32 = 4_000;
/// A stamp's turn, whole degrees either way.
pub const MAX_TURN: i32 = 360;
/// The widest a `~turn` swings, whole degrees each way.
pub const MAX_SWING_TURN: i32 = 180;
/// Sprite tiles every device can hold: the array-texture layers wgpu
/// guarantees. `wmc lint` warns above it; the game checks the device's own
/// limit.
pub const TILE_FLOOR: u32 = 256;

/// A sprite file's error: `file:line:col: message`, as the rules
/// compiler's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpriteError {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub msg: String,
}

impl fmt::Display for SpriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            return write!(f, "{}: {}", self.file, self.msg);
        }
        write!(f, "{}:{}:{}: {}", self.file, self.line, self.col, self.msg)
    }
}

impl std::error::Error for SpriteError {}

/// One `*.sprite` file: its palette and its sprites, in file order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SpriteFile {
    /// `(name, 0xRRGGBB)`, in file order: [`Ink::Named`] indexes it.
    pub palette: Vec<(String, u32)>,
    pub sprites: Vec<Sprite>,
}

/// `sprite NAME { ... }`: how the kind NAME is drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sprite {
    pub name: String,
    /// Where the name is, for messages.
    pub line: u32,
    pub col: u32,
    /// `poses[0]` is the sprite's own list (every `look` without a pose of
    /// its own); then each `pose V { ... }` in file order.
    pub poses: Vec<Pose>,
}

/// A list of stamps drawn in file order (later over earlier), and its
/// frames.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pose {
    /// `None` for the sprite's own list.
    pub look: Option<u8>,
    pub frames: Frames,
    /// Its own `frames` line (else a pose takes its sprite's).
    pub own_frames: bool,
    pub stamps: Vec<Stamp>,
}

impl Default for Pose {
    fn default() -> Self {
        Self {
            look: None,
            frames: Frames::STILL,
            own_frames: false,
            stamps: Vec::new(),
        }
    }
}

/// `frames N every T`: `n` pictures, each shown `every_ms` (0 for a still).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frames {
    pub n: u8,
    pub every_ms: u32,
}

impl Frames {
    pub const STILL: Frames = Frames { n: 1, every_ms: 0 };
}

/// A stamp's colour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Ink {
    /// The kind's `color`.
    #[default]
    Kind,
    /// The file palette's colour at this index.
    Named(u8),
    /// `"#rrggbb"`.
    Rgb(u32),
}

/// `"C" at X Y [turn D] [scale S] [color C] [~turn A] [~x A] [~y A]
/// [~scale A] [phase D]`. Distances and scales are thousandths of a cell
/// (exact decimals, the same on every machine); angles whole degrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    /// Printable ASCII.
    pub ch: u8,
    pub x: i32,
    pub y: i32,
    pub turn: i32,
    pub scale: i32,
    pub color: Ink,
    pub swing: Swing,
    pub phase: i32,
}

impl Default for Stamp {
    fn default() -> Self {
        Self {
            ch: b'?',
            x: 500,
            y: 500,
            turn: 0,
            scale: 1000,
            color: Ink::Kind,
            swing: Swing::default(),
            phase: 0,
        }
    }
}

/// How far each value swings over a pose's frames: `A * sin(2 pi f / N +
/// phase)` added at frame `f`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Swing {
    pub turn: i32,
    pub x: i32,
    pub y: i32,
    pub scale: i32,
}

impl SpriteFile {
    /// The file as text that parses back to it (positions aside).
    pub fn text(&self) -> String {
        use std::fmt::Write;
        let mut out = String::new();
        if !self.palette.is_empty() {
            out.push_str("palette {\n");
            for (name, c) in &self.palette {
                let _ = writeln!(out, "  {name} \"#{c:06x}\"");
            }
            out.push_str("}\n");
        }
        for s in &self.sprites {
            let _ = writeln!(out, "\nsprite {} {{", s.name);
            for (i, p) in s.poses.iter().enumerate() {
                let indent = if i == 0 { "  " } else { "    " };
                if let Some(v) = p.look {
                    let _ = writeln!(out, "\n  pose {v} {{");
                }
                if p.own_frames {
                    let _ = write!(out, "{indent}frames {}", p.frames.n);
                    if p.frames.every_ms > 0 {
                        let _ = write!(out, " every {}ms", p.frames.every_ms);
                    }
                    out.push('\n');
                }
                for st in &p.stamps {
                    out.push_str(indent);
                    out.push_str(&self.stamp_text(st));
                    out.push('\n');
                }
                if p.look.is_some() {
                    out.push_str("  }\n");
                }
            }
            out.push_str("}\n");
        }
        out
    }

    fn stamp_text(&self, s: &Stamp) -> String {
        let m = |v: i32| milli_text(i64::from(v));
        let q = if s.ch == b'"' { '\'' } else { '"' };
        let mut t = format!("{q}{}{q} at {} {}", char::from(s.ch), m(s.x), m(s.y));
        if s.turn != 0 {
            t += &format!(" turn {}", s.turn);
        }
        if s.scale != 1000 {
            t += &format!(" scale {}", m(s.scale));
        }
        match s.color {
            Ink::Kind => {}
            Ink::Named(i) => {
                let name = self
                    .palette
                    .get(usize::from(i))
                    .map_or("?", |(n, _)| n.as_str());
                t += &format!(" color {name}");
            }
            Ink::Rgb(c) => t += &format!(" color \"#{c:06x}\""),
        }
        if s.swing.turn != 0 {
            t += &format!(" ~turn {}", s.swing.turn);
        }
        if s.swing.x != 0 {
            t += &format!(" ~x {}", m(s.swing.x));
        }
        if s.swing.y != 0 {
            t += &format!(" ~y {}", m(s.swing.y));
        }
        if s.swing.scale != 0 {
            t += &format!(" ~scale {}", m(s.swing.scale));
        }
        if s.phase != 0 {
            t += &format!(" phase {}", s.phase);
        }
        t
    }
}

/// The built-in kinds' sprite files (`rules/*.sprite`), as the built-in
/// rules are carried: none yet.
pub const BUILTIN: [(&str, &str); 0] = [];

/// A sprite file to load: its label for messages, its text, and the player
/// whose pack it is in (its sprite names are that player's kinds).
#[derive(Debug, Clone)]
pub struct Source {
    pub label: String,
    pub text: String,
    pub owner: Option<String>,
}

/// The sprite files of `packs` (as `Kinds::debug.packs` holds them:
/// paths, `PLAYER=path` for a player's): every `*.sprite` in a pack that is a
/// directory, in name order; `NAME.sprite` beside a pack that is the file
/// `NAME.rules`. No pack is the built-in rules: [`BUILTIN`]. Labelled as
/// the rules are: the file name, with its directory when there are
/// several packs.
pub fn sources(packs: &[String]) -> Result<Vec<Source>, String> {
    if packs.is_empty() {
        return Ok(BUILTIN
            .iter()
            .map(|(label, text)| Source {
                label: (*label).to_string(),
                text: (*text).to_string(),
                owner: None,
            })
            .collect());
    }
    let mut out: Vec<Source> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();
    for pack in packs {
        let (owner, path) = sim_core::rules::player_of(pack);
        let path = Path::new(path);
        let files: Vec<PathBuf> = if path.is_dir() {
            let mut v: Vec<PathBuf> = std::fs::read_dir(path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "sprite"))
                .collect();
            v.sort();
            v
        } else {
            let beside = path.with_extension("sprite");
            if beside.is_file() {
                vec![beside]
            } else {
                Vec::new()
            }
        };
        for f in files {
            if seen.contains(&f) {
                continue;
            }
            let text = std::fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
            let parts: Vec<String> = f
                .components()
                .rev()
                .take(if packs.len() > 1 { 2 } else { 1 })
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect();
            let label = parts.into_iter().rev().collect::<Vec<_>>().join("/");
            out.push(Source {
                label,
                text,
                owner: owner.map(str::to_string),
            });
            seen.push(f);
        }
    }
    Ok(out)
}

/// A sprite matched to the kind it draws.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worn {
    pub kind: u16,
    /// The file it is in, for messages.
    pub file: String,
    pub sprite: Sprite,
    /// Its file's palette colours: [`Ink::Named`] indexes them.
    pub palette: Vec<u32>,
    /// The kind's footprint: the sprite is drawn over `W x H` cells.
    pub size: (u8, u8),
}

impl Worn {
    /// Its tiles: every frame of every pose, a tile a cell of the
    /// footprint.
    pub fn tiles(&self) -> u32 {
        let cells = u32::from(self.size.0) * u32::from(self.size.1);
        self.sprite
            .poses
            .iter()
            .map(|p| u32::from(p.frames.n) * cells)
            .sum()
    }
}

/// A rule set's sprites: which kinds have one, and the tiles they take.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Wardrobe {
    pub sprites: Vec<Worn>,
    /// Per kind id: its sprite in [`Self::sprites`], its own or its nearest
    /// parent's of the same size; `None` draws the kind's glyph.
    pub of_kind: Vec<Option<u16>>,
}

impl Wardrobe {
    /// The tile budget the sprites take: poses x frames x footprint, summed.
    pub fn tiles(&self) -> u32 {
        self.sprites.iter().map(Worn::tiles).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.sprites.is_empty()
    }
}

/// Parse `sources` and match each sprite to the kind it names (a player's
/// pack prefixes its player, as the compiler does). Two sprites for one
/// kind are an error; a sprite for a kind the rules lack is a warning
/// (packs load in many combinations), as is a stamp centred outside its
/// kind's footprint.
pub fn load(sources: &[Source], kinds: &Kinds) -> Result<(Wardrobe, Vec<Diagnostic>), SpriteError> {
    let mut w = Wardrobe {
        sprites: Vec::new(),
        of_kind: vec![None; kinds.defs.len()],
    };
    let mut warnings = Vec::new();
    for src in sources {
        let file = parse(&src.label, &src.text)?;
        let palette: Vec<u32> = file.palette.iter().map(|(_, c)| *c).collect();
        for s in file.sprites {
            let name = match &src.owner {
                Some(o) => format!("{o}.{}", s.name),
                None => s.name.clone(),
            };
            let warn = |msg: String| Diagnostic {
                level: Level::Warning,
                file: src.label.clone(),
                line: s.line,
                col: s.col,
                msg,
            };
            let Some(def) = kinds.by_name(&name) else {
                warnings.push(warn(format!(
                    "sprite `{name}`: no loaded pack has this kind; it draws nothing"
                )));
                continue;
            };
            if let Some(i) = w.of_kind[usize::from(def.id)] {
                let other = &w.sprites[usize::from(i)];
                return Err(SpriteError {
                    file: src.label.clone(),
                    line: s.line,
                    col: s.col,
                    msg: format!(
                        "kind `{name}` has a sprite already ({}:{})",
                        other.file, other.sprite.line
                    ),
                });
            }
            let (sw, sh) = def.size;
            let outside = s
                .poses
                .iter()
                .flat_map(|p| &p.stamps)
                .any(|st| st.x > i32::from(sw) * 1000 || st.y > i32::from(sh) * 1000);
            if outside {
                warnings.push(warn(format!(
                    "sprite `{name}`: a stamp is centred outside the kind's {sw} x {sh} cells"
                )));
            }
            w.of_kind[usize::from(def.id)] =
                Some(u16::try_from(w.sprites.len()).expect("fewer sprites than kinds"));
            w.sprites.push(Worn {
                kind: def.id,
                file: src.label.clone(),
                sprite: s,
                palette: palette.clone(),
                size: def.size,
            });
        }
    }
    // A kind without its own wears its nearest parent's, if it is the
    // same size. Parents come before their children (pre-order ids).
    for d in &kinds.defs {
        if w.of_kind[usize::from(d.id)].is_some() {
            continue;
        }
        let mut p = d.parent;
        while let Some(k) = p {
            let pd = kinds.def(k);
            if let Some(i) = w.of_kind[usize::from(k)] {
                if pd.size == d.size {
                    w.of_kind[usize::from(d.id)] = Some(i);
                }
                break;
            }
            p = pd.parent;
        }
    }
    Ok((w, warnings))
}

/// The sprites of the rule set `kinds` ([`sources`] of its packs, then
/// [`load`]).
pub fn read(kinds: &Kinds) -> anyhow::Result<(Wardrobe, Vec<Diagnostic>)> {
    let src = sources(&kinds.debug.packs).map_err(anyhow::Error::msg)?;
    load(&src, kinds).map_err(|e| anyhow::anyhow!("{e}"))
}

#[cfg(test)]
mod tests;
