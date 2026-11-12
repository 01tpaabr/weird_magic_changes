//! What each tile looks like: the only place glyphs and colours are chosen.
//! The ground picks the cell background, tinted by the ground cover (grass)
//! if there is one; the thing on it picks glyph + foreground. An actor's glyph and colour come from its kind
//! (`Kinds::glyphs`, `Kinds::colors`: declared in its rules file, carried
//! by the sim, never read by it).

use sim_core::rules::{FieldDef, Materials};
use sim_core::{Feature, Ground};

/// Packed `0x00RRGGBB` sRGB, one `u32` per cell: 4 bytes in the frame
/// buffers instead of Bevy's 16-byte `Color`. Converted once per cell when
/// the frame is turned into tiles ([`Color::tint`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self((r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    /// `[b, g, r, 0]`: the little-endian bytes of the packed value.
    pub const fn bytes(self) -> [u8; 4] {
        self.0.to_le_bytes()
    }

    /// `(r, g, b)` in `0..=255`, sRGB.
    pub const fn channels(self) -> (u8, u8, u8) {
        let [b, g, r, _] = self.bytes();
        (r, g, b)
    }

    /// This colour as a tile tint for `TilemapChunk`.
    ///
    /// The tilemap shader multiplies the sampled (linear) texel by the tint's
    /// 8-bit channels **without** an sRGB-to-linear step, and the 2D pipeline
    /// then encodes its linear output to sRGB. Feeding it the palette bytes as
    /// is would brighten every colour (0xC4 would come out ~0xE3). So the tint
    /// carries the palette's *linear* light in the channel bytes: what the
    /// shader treats as linear is linear, and the palette shows as designed.
    /// Cost: 8-bit quantisation of linear light, coarse in the darks, which
    /// this palette never displays alone (the void is the darkest at 0x10).
    pub fn tint(self) -> bevy::color::Color {
        let (r, g, b) = self.channels();
        bevy::color::Color::srgb(linear(r), linear(g), linear(b))
    }

    /// `t / 255` of the way from this colour to `to`, per channel, rounded.
    pub const fn mix(self, to: Color, t: u8) -> Color {
        let (r, g, b) = self.channels();
        let (r2, g2, b2) = to.channels();
        Color::rgb(lerp(r, r2, t), lerp(g, g2, t), lerp(b, b2, t))
    }

    /// Scaled towards black: `light` 255 is the colour itself, 0 is black.
    /// Per channel `(c * light + 127) / 255`, exact at both ends.
    pub const fn scaled(self, light: u8) -> Color {
        let [b, g, r, _] = self.bytes();
        Color::rgb(scale(r, light), scale(g, light), scale(b, light))
    }
}

/// sRGB byte -> linear light in `[0, 1]` (the IEC 61966-2-1 transfer curve).
fn linear(c: u8) -> f32 {
    let c = f32::from(c) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

// A weighted mean of two bytes (weights summing to 255, over 255): a byte.
#[allow(clippy::cast_possible_truncation)]
const fn lerp(a: u8, b: u8, t: u8) -> u8 {
    ((a as u32 * (255 - t as u32) + b as u32 * t as u32 + 127) / 255) as u8
}

// A byte times a byte over 255: a byte.
#[allow(clippy::cast_possible_truncation)]
const fn scale(c: u8, light: u8) -> u8 {
    ((c as u32 * light as u32 + 127) / 255) as u8
}

/// Map brightness at deep night: dim, still readable.
pub const NIGHT_FLOOR: u8 = 0x66;

/// Map brightness for a sim daylight level (`sim_core::time::daylight`):
/// full sun draws the palette as is, night draws it at [`NIGHT_FLOOR`].
// `span * daylight / 255` is at most `span`, and `NIGHT_FLOOR + span` is 255.
#[allow(clippy::cast_possible_truncation)]
pub const fn brightness(daylight: u8) -> u8 {
    let span = (255 - NIGHT_FLOOR) as u32;
    NIGHT_FLOOR + ((span * daylight as u32 + 127) / 255) as u8
}

pub const SOIL_BG: Color = Color::rgb(0xc4, 0x9e, 0x6c); // light brown
pub const SOIL_FG: Color = Color::rgb(0x8b, 0x66, 0x3b);
pub const WATER_BG: Color = Color::rgb(0x2b, 0x6f, 0xc8); // blue
pub const WATER_FG: Color = Color::rgb(0x9f, 0xca, 0xf5);
pub const ROCK_FG: Color = Color::rgb(0x3b, 0x3b, 0x3b);
pub const ACTOR_FG: Color = Color::rgb(0xff, 0xf3, 0x9c);
pub const VOID_BG: Color = Color::rgb(0x10, 0x10, 0x12); // chunk not loaded
pub const TEXT_FG: Color = Color::rgb(0xe6, 0xe6, 0xe6);
pub const TEXT_BG: Color = Color::rgb(0x1b, 0x1b, 0x20);
/// How far a cell with ground cover is tinted toward the cover's colour: a
/// meadow reads as a green patch, and whoever stands in it stays drawn.
pub const COVER_TINT: u8 = 0x70;
/// The most a slope lightens or darkens a cell (`relief`).
pub const RELIEF_TINT: u8 = 0x70;

/// `bg` shaded by the slope of a `relief` field: lit (toward white) where the
/// ground falls away toward the north-west light, shadowed where it rises:
/// `nw` and `se` are the heights of those neighbours, `max` the field's.
pub fn relieved(bg: Color, nw: i32, se: i32, max: u16) -> Color {
    let d = nw - se;
    if d == 0 {
        return bg;
    }
    // A slope of a sixteenth of the max across two cells is a full shade.
    let t = (d.unsigned_abs() * 16 * u32::from(RELIEF_TINT) / u32::from(max.max(1)))
        .min(u32::from(RELIEF_TINT));
    let t = u8::try_from(t).expect("at most RELIEF_TINT");
    if d < 0 {
        bg.mix(Color::rgb(0xff, 0xff, 0xff), t)
    } else {
        bg.mix(Color::rgb(0, 0, 0), t)
    }
}

/// How far a field at its max tints a cell toward the field's colour.
pub const FIELD_TINT: u8 = 0xb0;

/// `bg` tinted toward each drawn field's colour by its value at local cell
/// `i` (a fraction of its max): sugar shows as hills of colour.
pub fn fielded(
    bg: Color,
    defs: &[FieldDef],
    layers: &[[u16; sim_core::CHUNK_CELLS]],
    i: usize,
) -> Color {
    defs.iter()
        .zip(layers)
        .fold(bg, |c, (def, layer)| match def.color {
            Some(col) if layer[i] > 0 => {
                let t = u32::from(layer[i]) * u32::from(FIELD_TINT) / u32::from(def.max.max(1));
                c.mix(
                    Color(col),
                    u8::try_from(t.min(255)).expect("clamped to a byte"),
                )
            }
            _ => c,
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// Printable ASCII byte.
    pub glyph: u8,
    pub fg: Color,
    pub bg: Color,
}

/// Cells whose chunk is not loaded.
pub const VOID: Style = Style {
    glyph: b' ',
    fg: VOID_BG,
    bg: VOID_BG,
};

/// Glyph drawn for a kind the table does not know (a save from a newer build).
pub const UNKNOWN_ACTOR: u8 = b'?';

/// The ground's colours: the background and the dots of bare soil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Soil {
    pub bg: Color,
    pub fg: Color,
}

/// Light brown, as the game shows it.
pub const SOIL: Soil = Soil {
    bg: SOIL_BG,
    fg: SOIL_FG,
};

/// A purple black, behind the menu's gallery: the pictures stand out.
pub const NIGHT_SOIL: Soil = Soil {
    bg: Color::rgb(0x14, 0x10, 0x1c),
    fg: Color::rgb(0x26, 0x1f, 0x33),
};

/// A ground's background, and the glyph and colour of a bare cell of it:
/// the palette's soil (or `soil`, the menu's) and water for the engine's two
/// unless the rules give a colour; a declared ground in its own colour.
pub fn ground_look(g: Ground, soil: Soil, m: &Materials) -> (Color, u8, Color) {
    let def = m.grounds.get(usize::from(g.0));
    let glyph = def.map_or(b'.', |d| d.glyph);
    match (def.and_then(|d| d.color), g) {
        (Some(c), _) => (Color(c), glyph, Color(c).mix(TEXT_FG, 0x50)),
        (None, Ground::Water) => (WATER_BG, glyph, WATER_FG),
        (None, _) => (soil.bg, glyph, soil.fg),
    }
}

/// A feature's glyph and colour; `None` for no feature.
pub fn feature_look(f: Feature, m: &Materials) -> Option<(u8, Color)> {
    let def = m.features.get(usize::from(f.0).checked_sub(1)?)?;
    Some((def.glyph, def.color.map_or(ROCK_FG, Color)))
}

/// `actor` is the glyph, colour and `solid` of the kind standing here,
/// `cover` of the ground cover under it, if any: look both up with
/// [`Looks::actor`]. The cover tints the background and shows its glyph when
/// nobody stands on it; a solid kind fills its cell with its colour and
/// shows no glyph (a solid cover is the background of whoever stands on it).
/// With the engine's three materials as they come.
pub fn style(
    ground: Ground,
    feature: Feature,
    actor: Option<(u8, Color, bool)>,
    cover: Option<(u8, Color, bool)>,
    soil: Soil,
) -> Style {
    let m = &*DEFAULT_MATERIALS;
    style_of(
        ground_look(ground, soil, m),
        feature_look(feature, m),
        actor,
        cover,
    )
}

static DEFAULT_MATERIALS: std::sync::LazyLock<Materials> =
    std::sync::LazyLock::new(Materials::default);

/// [`style`] from a ground's and a feature's looks ([`ground_look`],
/// [`feature_look`]).
pub fn style_of(
    (ground_bg, glyph, ground_fg): (Color, u8, Color),
    feature: Option<(u8, Color)>,
    actor: Option<(u8, Color, bool)>,
    cover: Option<(u8, Color, bool)>,
) -> Style {
    let bg = match cover {
        Some((_, c, true)) => c,
        Some((_, c, false)) => ground_bg.mix(c, COVER_TINT),
        None => ground_bg,
    };
    let (glyph, fg, bg) = match (actor.or(cover), feature) {
        (Some((_, c, true)), _) => (b' ', c, c),
        (Some((g, c, false)), _) => (g, c, bg),
        (None, Some((g, c))) => (g, c, bg),
        (None, None) => (glyph, ground_fg, bg),
    };
    Style { glyph, fg, bg }
}

/// How each kind is drawn: its glyph, `0xRRGGBB` colour and whether it is
/// `solid`, indexed by kind (the kind table's `glyphs`, `colors` and
/// `solids`), and the soil's colours.
#[derive(Debug, Clone, Copy)]
pub struct Looks<'a> {
    pub glyphs: &'a [u8],
    pub colors: &'a [u32],
    pub solids: &'a [bool],
    pub soil: Soil,
    /// Cells too small to read a glyph in: whoever stands on one is drawn
    /// solid, a square of its colour ([`Looks::occupant`]).
    pub tiny: bool,
    /// The rules' fields: those with a colour tint the cells they fill.
    pub fields: &'a [FieldDef],
    /// The rules' grounds and features.
    pub materials: &'a Materials,
    /// Per kind: its sprite's tile, if it wears one (`crate::sprite`);
    /// empty when no kind does or the cells are too small
    /// ([`SPRITE_MIN`]).
    pub sprites: &'a [SpriteTile],
}

/// Where a kind's picture is in the sprite tileset: `first` is its first
/// tile plus one (0: no sprite, draw the glyph), `width` its footprint's
/// width, so a big actor's part `(dx, dy)` is tile `first - 1 + dy *
/// width + dx`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SpriteTile {
    pub first: u16,
    pub width: u8,
}

/// The smallest cell, in physical pixels, a sprite is drawn in; below it
/// kinds are drawn by their glyphs.
pub const SPRITE_MIN: u32 = 16;

/// The smallest cell, in physical pixels, a glyph is drawn in; below it
/// standing kinds are drawn solid.
pub const GLYPH_MIN: u32 = 8;

impl Default for Looks<'_> {
    fn default() -> Self {
        Self {
            glyphs: &[],
            colors: &[],
            solids: &[],
            soil: SOIL,
            tiny: false,
            fields: &[],
            materials: &DEFAULT_MATERIALS,
            sprites: &[],
        }
    }
}

impl<'a> Looks<'a> {
    pub fn of(kinds: &'a sim_core::Kinds) -> Self {
        Self {
            glyphs: &kinds.glyphs,
            colors: &kinds.colors,
            solids: &kinds.solids,
            soil: SOIL,
            tiny: false,
            fields: &kinds.fields,
            materials: &kinds.materials,
            sprites: &[],
        }
    }

    /// [`actor`](Self::actor) for whoever stands on a cell: solid on
    /// [`tiny`](Self::tiny) cells, whatever the kind says.
    #[inline]
    pub fn occupant(&self, occupant: sim_core::ActorId) -> Option<(u8, Color, bool)> {
        self.actor(occupant)
            .map(|(g, c, solid)| (g, c, solid || self.tiny))
    }

    /// The sprite tile plus one for `who` (an anchor or a part of a big
    /// actor), 0 when its kind wears no sprite: one load and a branch for a
    /// kind without one.
    #[inline]
    pub fn sprite(&self, who: sim_core::ActorId) -> u16 {
        let Some((kind, slot)) = who.unpack() else {
            return 0;
        };
        let Some(t) = self.sprites.get(usize::from(kind)).filter(|t| t.first != 0) else {
            return 0;
        };
        if slot & sim_core::ActorId::PART == 0 {
            return t.first;
        }
        // A part: its offset from the anchor is in its slot.
        let (dx, dy) = (slot >> 4 & 15, slot & 15);
        t.first + dy * u16::from(t.width) + dx
    }

    /// Glyph, colour and solidity of whoever stands on a cell: `None` for
    /// nobody; [`UNKNOWN_ACTOR`] past the end of the table, [`ACTOR_FG`]
    /// without a colour, not solid without the flag.
    #[inline]
    pub fn actor(&self, occupant: sim_core::ActorId) -> Option<(u8, Color, bool)> {
        occupant.unpack().map(|(kind, _)| {
            let k = usize::from(kind);
            let glyph = self.glyphs.get(k).copied().unwrap_or(UNKNOWN_ACTOR);
            let color = self.colors.get(k).map_or(ACTOR_FG, |&c| Color(c));
            let solid = self.solids.get(k).copied().unwrap_or(false);
            (glyph, color, solid)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An anchor shows its sprite's first tile, a part of a big actor its
    /// offset from it, a kind without one 0.
    #[test]
    fn a_sprite_tile_for_anchors_and_parts() {
        use sim_core::ActorId;
        let table = [
            SpriteTile::default(),
            SpriteTile {
                first: 10,
                width: 3,
            },
        ];
        let looks = Looks {
            sprites: &table,
            ..Looks::default()
        };
        assert_eq!(looks.sprite(ActorId::NONE), 0);
        assert_eq!(looks.sprite(ActorId::pack(0, 5)), 0);
        assert_eq!(looks.sprite(ActorId::pack(1, 5)), 10);
        assert_eq!(looks.sprite(ActorId::part(1, 2, 1)), 10 + 3 + 2);
        assert_eq!(looks.sprite(ActorId::part(1, 0, 2)), 10 + 6);
        // Past the table's end: no sprite.
        assert_eq!(looks.sprite(ActorId::pack(9, 0)), 0);
        assert_eq!(Looks::default().sprite(ActorId::pack(1, 0)), 0);
    }

    #[test]
    fn color_is_packed_0rgb() {
        assert_eq!(Color::rgb(1, 2, 3).0, 0x0001_0203);
        assert_eq!(Color::rgb(1, 2, 3).bytes(), [3, 2, 1, 0]);
        assert_eq!(Color::rgb(1, 2, 3).channels(), (1, 2, 3));
    }

    #[test]
    fn tint_carries_linear_light_in_the_channels() {
        let t = Color::rgb(255, 0, 0x80).tint().to_srgba();
        assert!((t.red - 1.0).abs() < 1e-6);
        assert_eq!(t.green, 0.0);
        // 0x80 sRGB is ~0.216 linear.
        assert!((t.blue - 0.2158).abs() < 1e-3, "{}", t.blue);
        assert!((linear(0x0A) - 0.003_035).abs() < 1e-5);
    }

    #[test]
    fn scaling_is_exact_at_the_ends_and_rounds_in_between() {
        let c = Color::rgb(200, 100, 3);
        assert_eq!(c.scaled(255), c);
        assert_eq!(c.scaled(0), Color::rgb(0, 0, 0));
        assert_eq!(c.scaled(128), Color::rgb(100, 50, 2));
        assert_eq!(brightness(255), 255);
        assert_eq!(brightness(0), NIGHT_FLOOR);
        assert!(brightness(128) > NIGHT_FLOOR && brightness(128) < 255);
    }

    #[test]
    fn ground_picks_background_and_the_kind_picks_the_glyph() {
        assert_eq!(
            style(Ground::Water, Feature::Rock, None, None, SOIL).bg,
            WATER_BG
        );
        assert_eq!(
            style(Ground::Soil, Feature::Rock, None, None, SOIL).bg,
            SOIL_BG
        );
        let red = Color::rgb(255, 0, 0);
        let s = style(
            Ground::Water,
            Feature::None,
            Some((b'c', red, false)),
            None,
            SOIL,
        );
        assert_eq!((s.glyph, s.fg, s.bg), (b'c', red, WATER_BG));
        // Ground cover: tints the background, shows only when nobody stands on it.
        let green = Color::rgb(0, 255, 0);
        let s = style(
            Ground::Soil,
            Feature::None,
            None,
            Some((b'\'', green, false)),
            SOIL,
        );
        assert_eq!(
            (s.glyph, s.fg, s.bg),
            (b'\'', green, SOIL_BG.mix(green, COVER_TINT))
        );
        let s = style(
            Ground::Soil,
            Feature::None,
            Some((b'c', red, false)),
            Some((b'\'', green, false)),
            SOIL,
        );
        assert_eq!(
            (s.glyph, s.fg, s.bg),
            (b'c', red, SOIL_BG.mix(green, COVER_TINT))
        );
        assert_eq!(red.mix(green, 0), red);
        assert_eq!(red.mix(green, 255), green);
        assert_eq!(red.mix(green, 128), Color::rgb(127, 128, 0));
        // Fields: a coloured one tints by its value against its max, an
        // empty cell or a field without a colour leaves the cell alone.
        let def = |color| FieldDef {
            name: "f".into(),
            max: 10,
            policy: sim_core::rules::Policy::Free,
            init: sim_core::rules::FieldInit::Value(0),
            color,
            outside: None,
            law: None,
            relief: false,
        };
        let defs = [def(Some(0x00ff_0000)), def(None)];
        let mut layers = vec![[0u16; sim_core::CHUNK_CELLS]; 2];
        assert_eq!(fielded(SOIL_BG, &defs, &layers, 3), SOIL_BG);
        layers[1][3] = 10;
        assert_eq!(fielded(SOIL_BG, &defs, &layers, 3), SOIL_BG, "no colour");
        layers[0][3] = 10;
        let full = SOIL_BG.mix(Color(0x00ff_0000), FIELD_TINT);
        assert_eq!(fielded(SOIL_BG, &defs, &layers, 3), full);
        layers[0][3] = 5;
        let half = SOIL_BG.mix(Color(0x00ff_0000), FIELD_TINT / 2);
        assert_eq!(fielded(SOIL_BG, &defs, &layers, 3), half);
        // Relief: flat leaves the cell alone; a rise toward the light
        // darkens it, a fall lightens it, a full shade at most.
        assert_eq!(relieved(SOIL_BG, 5, 5, 100), SOIL_BG);
        let lit = relieved(SOIL_BG, 0, 50, 100);
        assert_eq!(lit, SOIL_BG.mix(Color::rgb(0xff, 0xff, 0xff), RELIEF_TINT));
        assert_ne!(relieved(SOIL_BG, 3, 0, 100), SOIL_BG);
        assert_eq!(
            relieved(SOIL_BG, 50, 0, 100),
            SOIL_BG.mix(Color::rgb(0, 0, 0), RELIEF_TINT)
        );
        assert_eq!(
            style(
                Ground::Soil,
                Feature::Rock,
                Some((b'c', red, false)),
                None,
                SOIL
            )
            .glyph,
            b'c'
        );
        let looks = Looks {
            glyphs: b",T",
            colors: &[0x00_11_22_33],
            solids: &[true],
            ..Looks::default()
        };
        use sim_core::ActorId;
        assert_eq!(looks.actor(ActorId::NONE), None);
        assert_eq!(
            looks.actor(ActorId::pack(0, 3)),
            Some((b',', Color(0x112233), true))
        );
        assert_eq!(
            looks.actor(ActorId::pack(1, 40)),
            Some((b'T', ACTOR_FG, false))
        );
        assert_eq!(
            looks.actor(ActorId::pack(2, 0)),
            Some((UNKNOWN_ACTOR, ACTOR_FG, false))
        );
    }

    /// A solid kind fills its cell, no glyph; a solid cover is the
    /// background of whoever stands on it; the soil's colours are a choice.
    #[test]
    fn a_solid_kind_fills_its_cell() {
        let (red, green) = (Color::rgb(255, 0, 0), Color::rgb(0, 255, 0));
        let s = style(
            Ground::Soil,
            Feature::None,
            Some((b'c', red, true)),
            None,
            SOIL,
        );
        assert_eq!((s.glyph, s.fg, s.bg), (b' ', red, red));
        let s = style(
            Ground::Soil,
            Feature::None,
            None,
            Some((b'g', green, true)),
            SOIL,
        );
        assert_eq!((s.glyph, s.bg), (b' ', green));
        let s = style(
            Ground::Soil,
            Feature::None,
            Some((b'c', red, false)),
            Some((b'g', green, true)),
            SOIL,
        );
        assert_eq!((s.glyph, s.fg, s.bg), (b'c', red, green));
        let s = style(Ground::Soil, Feature::None, None, None, NIGHT_SOIL);
        assert_eq!((s.glyph, s.fg, s.bg), (b'.', NIGHT_SOIL.fg, NIGHT_SOIL.bg));
        let s = style(Ground::Water, Feature::None, None, None, NIGHT_SOIL);
        assert_eq!(s.bg, WATER_BG, "only soil changes");
        // Tiny cells: whoever stands is solid; a cover keeps its own way.
        let tiny = Looks {
            glyphs: b"cg",
            colors: &[0x00ff_0000, 0x0000_ff00],
            solids: &[false, false],
            tiny: true,
            ..Looks::default()
        };
        use sim_core::ActorId;
        assert_eq!(tiny.occupant(ActorId::pack(0, 1)), Some((b'c', red, true)));
        assert_eq!(tiny.actor(ActorId::pack(1, 1)), Some((b'g', green, false)));
        let normal = Looks {
            tiny: false,
            ..tiny
        };
        assert_eq!(
            normal.occupant(ActorId::pack(0, 1)),
            Some((b'c', red, false))
        );
    }
}
