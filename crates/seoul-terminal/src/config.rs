use libghostty_vt::style::RgbColor as GhosttyRgb;

/// Terminal configuration.
#[derive(Debug, Clone)]
pub struct TerminalConfig {
    pub font_family: String,
    pub font_size: f32,
    pub line_height_multiplier: f32,
    pub scrollback_lines: usize,
    pub cols: u16,
    pub rows: u16,
    pub theme: Theme,
    pub padding: f32,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            font_family: "Menlo".to_string(),
            font_size: 13.0,
            line_height_multiplier: 1.0,
            scrollback_lines: 10_000,
            cols: 80,
            rows: 24,
            theme: Theme::default(),
            padding: 4.0,
        }
    }
}

/// Terminal color theme — modern neutral dark by default.
#[derive(Debug, Clone)]
pub struct Theme {
    pub background: RgbColor,
    pub foreground: RgbColor,
    pub cursor: RgbColor,
    pub selection_bg: RgbColor,
    pub selection_fg: RgbColor,
    pub ansi: [RgbColor; 16],
}

impl Default for Theme {
    fn default() -> Self {
        Self::modern_dark()
    }
}

impl Theme {
    pub fn modern_dark() -> Self {
        Self {
            background: RgbColor(0x0a, 0x0c, 0x10),
            foreground: RgbColor(0xe8, 0xe8, 0xec),
            cursor: RgbColor(0xe8, 0xe8, 0xec),
            selection_bg: RgbColor(0x2a, 0x2f, 0x3c),
            selection_fg: RgbColor(0xe8, 0xe8, 0xec),
            ansi: [
                RgbColor(0x16, 0x18, 0x1d), // 0  black
                RgbColor(0xeb, 0x57, 0x57), // 1  red
                RgbColor(0x4c, 0xb7, 0x82), // 2  green
                RgbColor(0xf2, 0xc9, 0x4c), // 3  yellow
                RgbColor(0x5e, 0x6a, 0xd2), // 4  blue (Linear signature)
                RgbColor(0xa9, 0x91, 0xf4), // 5  magenta
                RgbColor(0x56, 0xb6, 0xc2), // 6  cyan
                RgbColor(0xc1, 0xc4, 0xd1), // 7  white (dim)
                RgbColor(0x4d, 0x55, 0x66), // 8  bright black
                RgbColor(0xff, 0x74, 0x74), // 9  bright red
                RgbColor(0x6e, 0xd1, 0x97), // 10 bright green
                RgbColor(0xff, 0xd8, 0x66), // 11 bright yellow
                RgbColor(0x82, 0x94, 0xe4), // 12 bright blue
                RgbColor(0xc0, 0xac, 0xf7), // 13 bright magenta
                RgbColor(0x7c, 0xd0, 0xe0), // 14 bright cyan
                RgbColor(0xe8, 0xe8, 0xec), // 15 bright white
            ],
        }
    }

    pub fn catppuccin_mocha() -> Self {
        Self {
            background: RgbColor(0x1e, 0x1e, 0x2e),
            foreground: RgbColor(0xcd, 0xd6, 0xf4),
            cursor: RgbColor(0xf5, 0xe0, 0xdc),
            selection_bg: RgbColor(0x58, 0x5b, 0x70),
            selection_fg: RgbColor(0xcd, 0xd6, 0xf4),
            ansi: [
                RgbColor(0x45, 0x47, 0x5a), // 0  black
                RgbColor(0xf3, 0x8b, 0xa8), // 1  red
                RgbColor(0xa6, 0xe3, 0xa1), // 2  green
                RgbColor(0xf9, 0xe2, 0xaf), // 3  yellow
                RgbColor(0x89, 0xb4, 0xfa), // 4  blue
                RgbColor(0xf5, 0xc2, 0xe7), // 5  magenta
                RgbColor(0x94, 0xe2, 0xd5), // 6  cyan
                RgbColor(0xba, 0xc2, 0xde), // 7  white
                RgbColor(0x58, 0x5b, 0x70), // 8  bright black
                RgbColor(0xf3, 0x8b, 0xa8), // 9  bright red
                RgbColor(0xa6, 0xe3, 0xa1), // 10 bright green
                RgbColor(0xf9, 0xe2, 0xaf), // 11 bright yellow
                RgbColor(0x89, 0xb4, 0xfa), // 12 bright blue
                RgbColor(0xf5, 0xc2, 0xe7), // 13 bright magenta
                RgbColor(0x94, 0xe2, 0xd5), // 14 bright cyan
                RgbColor(0xa6, 0xad, 0xc8), // 15 bright white
            ],
        }
    }

    /// Build the full 256-color palette for libghostty.
    pub fn build_palette_256(&self) -> [GhosttyRgb; 256] {
        let mut palette = [GhosttyRgb { r: 0, g: 0, b: 0 }; 256];

        for (i, c) in self.ansi.iter().enumerate() {
            palette[i] = c.to_ghostty();
        }

        // 6x6x6 color cube (16-231)
        for r in 0..6u8 {
            for g in 0..6u8 {
                for b in 0..6u8 {
                    let idx = 16 + (r as usize) * 36 + (g as usize) * 6 + (b as usize);
                    palette[idx] = GhosttyRgb {
                        r: if r == 0 { 0 } else { 55 + 40 * r },
                        g: if g == 0 { 0 } else { 55 + 40 * g },
                        b: if b == 0 { 0 } else { 55 + 40 * b },
                    };
                }
            }
        }

        // Grayscale ramp (232-255)
        for i in 0..24u8 {
            let v = 8 + 10 * i;
            palette[232 + i as usize] = GhosttyRgb { r: v, g: v, b: v };
        }

        palette
    }

    pub fn indexed_color(&self, idx: u8) -> RgbColor {
        match idx {
            0..=15 => self.ansi[idx as usize],
            16..=231 => {
                let n = idx - 16;
                let r = (n / 36) * 51;
                let g = ((n % 36) / 6) * 51;
                let b = (n % 6) * 51;
                RgbColor(r, g, b)
            }
            232..=255 => {
                let gray = 8 + (idx - 232) * 10;
                RgbColor(gray, gray, gray)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RgbColor(pub u8, pub u8, pub u8);

impl RgbColor {
    pub fn to_u32(self) -> u32 {
        ((self.0 as u32) << 16) | ((self.1 as u32) << 8) | (self.2 as u32)
    }

    pub fn to_rgba_u32(self) -> u32 {
        ((self.0 as u32) << 24) | ((self.1 as u32) << 16) | ((self.2 as u32) << 8) | 0xFF
    }

    pub fn to_ghostty(self) -> GhosttyRgb {
        GhosttyRgb {
            r: self.0,
            g: self.1,
            b: self.2,
        }
    }
}

impl From<GhosttyRgb> for RgbColor {
    fn from(c: GhosttyRgb) -> Self {
        Self(c.r, c.g, c.b)
    }
}
