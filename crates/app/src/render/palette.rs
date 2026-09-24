//! What each tile looks like: the only place glyphs and colours are chosen.
//! The ground picks the cell background; the thing on it picks glyph +
//! foreground. An actor's glyph comes from its kind (`Kinds::glyphs`, a byte
//! the sim carries but never reads).

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

/// `actor` is the glyph of the kind standing here, if any: look it up with
/// [`actor_glyph`] from the occupant id.
pub fn style(ground: Ground, feature: Feature, actor: Option<u8>) -> Style {
    let bg = match ground {
        Ground::Soil => SOIL_BG,
        Ground::Water => WATER_BG,
    };
    let (glyph, fg) = match (actor, feature, ground) {
        (Some(glyph), _, _) => (glyph, ACTOR_FG),
        (None, Feature::Rock, _) => (b'#', ROCK_FG),
        (None, Feature::None, Ground::Soil) => (b'.', SOIL_FG),
        (None, Feature::None, Ground::Water) => (b'~', WATER_FG),
    };
    Style { glyph, fg, bg }
}

/// The glyph for whoever stands on a cell: `None` for nobody, the kind's
/// glyph from `glyphs` (indexed by kind), [`UNKNOWN_ACTOR`] past its end.
#[inline]
pub fn actor_glyph(occupant: sim_core::ActorId, glyphs: &[u8]) -> Option<u8> {
    occupant.unpack().map(|(kind, _)| {
        glyphs
            .get(usize::from(kind))
            .copied()
            .unwrap_or(UNKNOWN_ACTOR)
    })
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
        assert_eq!(style(Ground::Water, Feature::Rock, None).bg, WATER_BG);
        assert_eq!(style(Ground::Soil, Feature::Rock, None).bg, SOIL_BG);
        assert_eq!(style(Ground::Water, Feature::None, Some(b'c')).glyph, b'c');
        assert_eq!(style(Ground::Soil, Feature::Rock, Some(b'c')).glyph, b'c');
        let glyphs = *b",T";
        use sim_core::ActorId;
        assert_eq!(actor_glyph(ActorId::NONE, &glyphs), None);
        assert_eq!(actor_glyph(ActorId::pack(1, 40), &glyphs), Some(b'T'));
        assert_eq!(
            actor_glyph(ActorId::pack(2, 0), &glyphs),
            Some(UNKNOWN_ACTOR)
        );
    }
}
