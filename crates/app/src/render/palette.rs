//! What each tile looks like: the only place glyphs and colours are chosen.
//! The ground picks the cell background; the thing on it picks glyph + foreground.

use sim_core::{Feature, Ground};

/// Packed RGBA, `r` in the low byte. In little-endian memory that is
/// `[r, g, b, a]`, exactly the pixel buffer layout, so a `u32` store is a pixel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub u32);

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(u32::from_le_bytes([r, g, b, 0xff]))
    }

    pub const fn bytes(self) -> [u8; 4] {
        self.0.to_le_bytes()
    }
}

pub const SOIL_BG: Rgba = Rgba::rgb(0xc4, 0x9e, 0x6c); // light brown
pub const SOIL_FG: Rgba = Rgba::rgb(0x8b, 0x66, 0x3b);
pub const WATER_BG: Rgba = Rgba::rgb(0x2b, 0x6f, 0xc8); // blue
pub const WATER_FG: Rgba = Rgba::rgb(0x9f, 0xca, 0xf5);
pub const ROCK_FG: Rgba = Rgba::rgb(0x3b, 0x3b, 0x3b);
pub const ACTOR_FG: Rgba = Rgba::rgb(0xff, 0xf3, 0x9c);
pub const VOID_BG: Rgba = Rgba::rgb(0x10, 0x10, 0x12); // chunk not loaded
pub const TEXT_FG: Rgba = Rgba::rgb(0xe6, 0xe6, 0xe6);
pub const TEXT_BG: Rgba = Rgba::rgb(0x1b, 0x1b, 0x20);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    /// Printable ASCII byte.
    pub glyph: u8,
    pub fg: Rgba,
    pub bg: Rgba,
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
    fn rgba_bytes_are_pixel_order() {
        assert_eq!(Rgba::rgb(1, 2, 3).bytes(), [1, 2, 3, 0xff]);
    }

    #[test]
    fn ground_picks_background() {
        assert_eq!(style(Ground::Water, Feature::Rock, false).bg, WATER_BG);
        assert_eq!(style(Ground::Soil, Feature::Rock, false).bg, SOIL_BG);
        assert_eq!(style(Ground::Water, Feature::None, true).glyph, b'@');
    }
}
