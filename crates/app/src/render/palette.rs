//! What each tile looks like: the only place glyphs and colours are chosen.
//! The ground picks the cell background, tinted by the ground cover (grass)
//! if there is one; the thing on it picks glyph + foreground. An actor's glyph and colour come from its kind
//! (`Kinds::glyphs`, `Kinds::colors`: declared in its rules file, carried
//! by the sim, never read by it).

use sim_core::{Feature, Ground, SCENT_CHANNELS};

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

const fn lerp(a: u8, b: u8, t: u8) -> u8 {
    ((a as u32 * (255 - t as u32) + b as u32 * t as u32 + 127) / 255) as u8
}

const fn scale(c: u8, light: u8) -> u8 {
    ((c as u32 * light as u32 + 127) / 255) as u8
}

/// Map brightness at deep night: dim, still readable.
pub const NIGHT_FLOOR: u8 = 0x66;

/// Map brightness for a sim daylight level (`sim_core::time::daylight`):
/// full sun draws the palette as is, night draws it at [`NIGHT_FLOOR`].
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
/// Scent channel colours, in channel order: amber, violet, teal, rose.
pub const SCENT: [Color; SCENT_CHANNELS] = [
    Color::rgb(0xff, 0xc4, 0x30),
    Color::rgb(0xb4, 0x78, 0xff),
    Color::rgb(0x30, 0xd8, 0xc8),
    Color::rgb(0xff, 0x5a, 0x8c),
];
/// How far full scent (255) tints a cell toward its channel's colour.
pub const SCENT_TINT: u8 = 0x68;

/// `bg` tinted toward each channel's colour by that channel's scent: a
/// trail shows as a faint wash that fades with it.
pub fn scented(bg: Color, scent: [u8; SCENT_CHANNELS]) -> Color {
    scent.iter().zip(SCENT).fold(bg, |c, (&s, col)| {
        if s == 0 {
            c
        } else {
            c.mix(col, (u32::from(s) * u32::from(SCENT_TINT) / 255) as u8)
        }
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

/// `actor` is the glyph and colour of the kind standing here, `cover` of the
/// ground cover under it, if any: look both up with [`Looks::actor`]. The
/// cover tints the background and shows its glyph when nobody stands on it.
pub fn style(
    ground: Ground,
    feature: Feature,
    actor: Option<(u8, Color)>,
    cover: Option<(u8, Color)>,
) -> Style {
    let bg = match ground {
        Ground::Soil => SOIL_BG,
        Ground::Water => WATER_BG,
    };
    let bg = match cover {
        Some((_, c)) => bg.mix(c, COVER_TINT),
        None => bg,
    };
    let (glyph, fg) = match (actor.or(cover), feature, ground) {
        (Some(look), _, _) => look,
        (None, Feature::Rock, _) => (b'#', ROCK_FG),
        (None, Feature::None, Ground::Soil) => (b'.', SOIL_FG),
        (None, Feature::None, Ground::Water) => (b'~', WATER_FG),
    };
    Style { glyph, fg, bg }
}

/// How each kind is drawn: its glyph and `0xRRGGBB` colour, indexed by kind
/// (the kind table's `glyphs` and `colors`).
#[derive(Debug, Clone, Copy, Default)]
pub struct Looks<'a> {
    pub glyphs: &'a [u8],
    pub colors: &'a [u32],
}

impl<'a> Looks<'a> {
    pub fn of(kinds: &'a sim_core::Kinds) -> Self {
        Self {
            glyphs: &kinds.glyphs,
            colors: &kinds.colors,
        }
    }

    /// Glyph and colour of whoever stands on a cell: `None` for nobody;
    /// [`UNKNOWN_ACTOR`] past the end of the table, [`ACTOR_FG`] without a
    /// colour.
    #[inline]
    pub fn actor(&self, occupant: sim_core::ActorId) -> Option<(u8, Color)> {
        occupant.unpack().map(|(kind, _)| {
            let k = usize::from(kind);
            let glyph = self.glyphs.get(k).copied().unwrap_or(UNKNOWN_ACTOR);
            let color = self.colors.get(k).map_or(ACTOR_FG, |&c| Color(c));
            (glyph, color)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(style(Ground::Water, Feature::Rock, None, None).bg, WATER_BG);
        assert_eq!(style(Ground::Soil, Feature::Rock, None, None).bg, SOIL_BG);
        let red = Color::rgb(255, 0, 0);
        let s = style(Ground::Water, Feature::None, Some((b'c', red)), None);
        assert_eq!((s.glyph, s.fg, s.bg), (b'c', red, WATER_BG));
        // Ground cover: tints the background, shows only when nobody stands on it.
        let green = Color::rgb(0, 255, 0);
        let s = style(Ground::Soil, Feature::None, None, Some((b'\'', green)));
        assert_eq!(
            (s.glyph, s.fg, s.bg),
            (b'\'', green, SOIL_BG.mix(green, COVER_TINT))
        );
        let s = style(
            Ground::Soil,
            Feature::None,
            Some((b'c', red)),
            Some((b'\'', green)),
        );
        assert_eq!(
            (s.glyph, s.fg, s.bg),
            (b'c', red, SOIL_BG.mix(green, COVER_TINT))
        );
        assert_eq!(red.mix(green, 0), red);
        assert_eq!(red.mix(green, 255), green);
        assert_eq!(red.mix(green, 128), Color::rgb(127, 128, 0));
        // Scent: none leaves the background alone, full scent tints it.
        assert_eq!(scented(SOIL_BG, [0; SCENT_CHANNELS]), SOIL_BG);
        assert_eq!(
            scented(SOIL_BG, [255, 0, 0, 0]),
            SOIL_BG.mix(SCENT[0], SCENT_TINT)
        );
        assert_ne!(scented(SOIL_BG, [0, 40, 0, 0]), SOIL_BG);
        assert_ne!(scented(SOIL_BG, [0, 0, 0, 40]), SOIL_BG);
        assert_eq!(
            style(Ground::Soil, Feature::Rock, Some((b'c', red)), None).glyph,
            b'c'
        );
        let looks = Looks {
            glyphs: b",T",
            colors: &[0x00_11_22_33],
        };
        use sim_core::ActorId;
        assert_eq!(looks.actor(ActorId::NONE), None);
        assert_eq!(
            looks.actor(ActorId::pack(0, 3)),
            Some((b',', Color(0x112233)))
        );
        assert_eq!(looks.actor(ActorId::pack(1, 40)), Some((b'T', ACTOR_FG)));
        assert_eq!(
            looks.actor(ActorId::pack(2, 0)),
            Some((UNKNOWN_ACTOR, ACTOR_FG))
        );
    }
}
