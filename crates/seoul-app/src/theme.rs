use gpui::App;
use seoul_workspace::settings::SettingsStore;

/// Resolved color palette for rendering.
#[derive(Debug, Clone)]
pub struct ThemeColors {
    // -- Base tones --
    pub mantle: u32,
    pub base: u32,
    pub surface0: u32,
    pub surface1: u32,
    pub surface2: u32,
    // -- Overlays --
    pub overlay0: u32,
    pub overlay2: u32,
    // -- Text tones --
    pub subtext0: u32,
    pub text: u32,
    // -- Accent colors --
    pub rosewater: u32,
    pub flamingo: u32,
    pub mauve: u32,
    pub red: u32,
    pub peach: u32,
    pub yellow: u32,
    pub green: u32,
    pub teal: u32,
    pub sky: u32,
    pub sapphire: u32,
    pub blue: u32,
    pub lavender: u32,
    // -- Alpha variants --
    pub selection_bg: u32,
    pub active_line_bg: u32,
    pub hover_bg_subtle: u32,
}

impl ThemeColors {
    pub fn modern_dark() -> Self {
        Self {
            mantle: 0x07090c,
            base: 0x0a0c10,
            surface0: 0x16181d,
            surface1: 0x1f2229,
            surface2: 0x2a2f3c,
            overlay0: 0x3d4252,
            overlay2: 0x6b6e7a,
            subtext0: 0x8a8f98,
            text: 0xe8e8ec,
            rosewater: 0xffb2b2,
            flamingo: 0xfbcaa1,
            mauve: 0xa991f4,
            red: 0xeb5757,
            peach: 0xf2994a,
            yellow: 0xf2c94c,
            green: 0x4cb782,
            teal: 0x4ca8a3,
            sky: 0x6db4f4,
            sapphire: 0x4a90e2,
            blue: 0x5e6ad2,
            lavender: 0x8294e4,
            selection_bg: 0x2a2f3c99,
            active_line_bg: 0xffffff06,
            hover_bg_subtle: 0x1f2229,
        }
    }

    pub fn catppuccin_mocha() -> Self {
        Self {
            mantle: 0x181825,
            base: 0x1e1e2e,
            surface0: 0x313244,
            surface1: 0x45475a,
            surface2: 0x585b70,
            overlay0: 0x6c7086,
            overlay2: 0x9399b2,
            subtext0: 0xa6adc8,
            text: 0xcdd6f4,
            rosewater: 0xf5e0dc,
            flamingo: 0xf2cdcd,
            mauve: 0xcba6f7,
            red: 0xf38ba8,
            peach: 0xfab387,
            yellow: 0xf9e2af,
            green: 0xa6e3a1,
            teal: 0x94e2d5,
            sky: 0x89dceb,
            sapphire: 0x74c7ec,
            blue: 0x89b4fa,
            lavender: 0xb4befe,
            selection_bg: 0x45475a80,
            active_line_bg: 0x26263718,
            hover_bg_subtle: 0x2a2b3d,
        }
    }
}

/// Get theme colors from global settings.
pub fn theme(cx: &App) -> ThemeColors {
    let settings = cx.global::<SettingsStore>();
    resolve_theme(&settings.global().theme.name)
}

/// Get theme colors for a specific project context.
#[allow(dead_code)]
pub fn theme_for_project(project_id: Option<uuid::Uuid>, cx: &App) -> ThemeColors {
    let settings = cx.global::<SettingsStore>();
    resolve_theme(&settings.get(project_id).theme.name)
}

fn resolve_theme(name: &str) -> ThemeColors {
    match name {
        "catppuccin-mocha" => ThemeColors::catppuccin_mocha(),
        _ => ThemeColors::modern_dark(),
    }
}

/// Palette hex → rgba u32 (opaque). E.g. `0xcba6f7` → `0xcba6f7ff`.
pub const fn opaque(hex: u32) -> u32 {
    (hex << 8) | 0xff
}
