//! What each tile looks like: the only place glyphs and colours are chosen.
//! The ground picks the cell background; the thing on it picks glyph + foreground.

use sim_core::{Feature, Ground};

/// Packed `0x00RRGGBB`: the `softbuffer` pixel format, one `u32` per pixel,
/// top byte ignored. In little-endian memory that is `[b, g, r, 0]`; the blit
/// kernel blends the four bytes without caring which channel is which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u32);

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self((r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    /// The four bytes as they sit in the pixel buffer.
    pub const fn bytes(self) -> [u8; 4] {
        self.0.to_le_bytes()
    }

    /// Scaled towards black: `light` 255 is the colour itself, 0 is black.
    /// Per channel `(c * light + 127) / 255`, exact at both ends.
    pub const fn scaled(self, light: u8) -> Color {
        let [b, g, r, _] = self.bytes();
        Color::rgb(scale(r, light), scale(g, light), scale(b, light))
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

pub fn style(ground: Ground, feature: Feature, occupied: bool) -> Style {
    let bg = match ground {
        Ground::Soil => SOIL_BG,
        Ground::Water => WATER_BG,
    };
    let (glyph, fg) = if occupied {
        (b'@', ACTOR_FG)
    } else {
        match (feature, ground) {
            (Feature::Rock, _) => (b'#', ROCK_FG),
            (Feature::None, Ground::Soil) => (b'.', SOIL_FG),
            (Feature::None, Ground::Water) => (b'~', WATER_FG),
        }
    };
    Style { glyph, fg, bg }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_is_softbuffer_0rgb() {
        assert_eq!(Color::rgb(1, 2, 3).0, 0x0001_0203);
        assert_eq!(Color::rgb(1, 2, 3).bytes(), [3, 2, 1, 0]);
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
    fn ground_picks_background() {
        assert_eq!(style(Ground::Water, Feature::Rock, false).bg, WATER_BG);
        assert_eq!(style(Ground::Soil, Feature::Rock, false).bg, SOIL_BG);
        assert_eq!(style(Ground::Water, Feature::None, true).glyph, b'@');
    }
}
