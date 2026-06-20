//! Resolve `alacritty_terminal` cell colors to linear-ish RGBA floats.
//!
//! `RenderableContent` carries a dynamic `Colors` palette (OSC overrides, etc.);
//! we consult it first and fall back to the active named theme's built-in
//! 16-color ANSI palette (extended to the xterm 256-color cube/grayscale) plus
//! the theme's special foreground/background/cursor entries.
//!
//! The theme is selected once at startup from config/CLI (`set_theme`) and read
//! globally thereafter, so the hot `resolve` path and the cell-drawing code can
//! reach it without threading a `&Theme` through every call.

use std::sync::OnceLock;

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

/// A complete color theme: the special fg/bg/cursor entries, the selection
/// background tint, and the 16-entry ANSI palette (8 normal + 8 bright).
#[derive(Clone, Copy)]
pub struct Theme {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub selection_bg: Rgb,
    pub ansi16: [Rgb; 16],
}

const fn rgb(r: u8, g: u8, b: u8) -> Rgb {
    Rgb { r, g, b }
}

/// Tokyo Night: a deep, slightly cool near-black background, a soft lavender-gray
/// foreground, a bright cyan cursor accent, and a cohesive saturated-but-soft
/// ANSI palette. The default theme.
const TOKYO_NIGHT: Theme = Theme {
    fg: rgb(0xC0, 0xCA, 0xF5),
    bg: rgb(0x1A, 0x1B, 0x26),
    cursor: rgb(0x7D, 0xCF, 0xFF),
    selection_bg: rgb(0x28, 0x34, 0x57),
    ansi16: [
        rgb(0x15, 0x16, 0x1E), // 0  black
        rgb(0xF7, 0x76, 0x8E), // 1  red
        rgb(0x9E, 0xCE, 0x6A), // 2  green
        rgb(0xE0, 0xAF, 0x68), // 3  yellow
        rgb(0x7A, 0xA2, 0xF7), // 4  blue
        rgb(0xBB, 0x9A, 0xF7), // 5  magenta
        rgb(0x7D, 0xCF, 0xFF), // 6  cyan
        rgb(0xA9, 0xB1, 0xD6), // 7  white
        rgb(0x41, 0x48, 0x68), // 8  bright black
        rgb(0xFF, 0x9E, 0x64), // 9  bright red
        rgb(0x9E, 0xCE, 0x6A), // 10 bright green
        rgb(0xFA, 0xBD, 0x2F), // 11 bright yellow
        rgb(0x7A, 0xA2, 0xF7), // 12 bright blue
        rgb(0xBB, 0x9A, 0xF7), // 13 bright magenta
        rgb(0x0D, 0xB9, 0xD7), // 14 bright cyan
        rgb(0xC0, 0xCA, 0xF5), // 15 bright white
    ],
};

/// Catppuccin Mocha: a warm, soft dark theme with pastel accents.
/// Special entries use Text (fg), Base (bg) and Rosewater (cursor); the ANSI
/// palette follows the project's published terminal mapping.
const CATPPUCCIN_MOCHA: Theme = Theme {
    fg: rgb(0xCD, 0xD6, 0xF4),     // Text
    bg: rgb(0x1E, 0x1E, 0x2E),     // Base
    cursor: rgb(0xF5, 0xE0, 0xDC), // Rosewater
    selection_bg: rgb(0x41, 0x45, 0x59),
    ansi16: [
        rgb(0x45, 0x47, 0x5A), // 0  black  (Surface1)
        rgb(0xF3, 0x8B, 0xA8), // 1  red    (Red)
        rgb(0xA6, 0xE3, 0xA1), // 2  green  (Green)
        rgb(0xF9, 0xE2, 0xAF), // 3  yellow (Yellow)
        rgb(0x89, 0xB4, 0xFA), // 4  blue   (Blue)
        rgb(0xF5, 0xC2, 0xE7), // 5  magenta(Pink)
        rgb(0x94, 0xE2, 0xD5), // 6  cyan   (Teal)
        rgb(0xBA, 0xC2, 0xDE), // 7  white  (Subtext1)
        rgb(0x58, 0x5B, 0x70), // 8  bright black  (Surface2)
        rgb(0xF3, 0x8B, 0xA8), // 9  bright red
        rgb(0xA6, 0xE3, 0xA1), // 10 bright green
        rgb(0xF9, 0xE2, 0xAF), // 11 bright yellow
        rgb(0x89, 0xB4, 0xFA), // 12 bright blue
        rgb(0xF5, 0xC2, 0xE7), // 13 bright magenta
        rgb(0x94, 0xE2, 0xD5), // 14 bright cyan
        rgb(0xA6, 0xAD, 0xC8), // 15 bright white (Subtext0)
    ],
};

/// Resolve a theme by (case-insensitive, separator-insensitive) name. Returns
/// `None` for an unknown name so the caller can warn and keep the default.
pub fn theme_by_name(name: &str) -> Option<Theme> {
    let key: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match key.as_str() {
        "tokyonight" | "tokyo" => Some(TOKYO_NIGHT),
        "catppuccinmocha" | "catppuccin" | "mocha" => Some(CATPPUCCIN_MOCHA),
        _ => None,
    }
}

/// The process-wide active theme, set once at startup. Reads before `set_theme`
/// (there are none in practice) fall back to the default.
static ACTIVE: OnceLock<Theme> = OnceLock::new();

/// Install the active theme. Idempotent: only the first call wins (startup).
pub fn set_theme(theme: Theme) {
    let _ = ACTIVE.set(theme);
}

/// The active theme, defaulting to Tokyo Night before `set_theme` is called.
fn active() -> &'static Theme {
    ACTIVE.get().unwrap_or(&TOKYO_NIGHT)
}

/// Default background of the active theme.
pub fn default_bg() -> [f32; 4] {
    to_f32(active().bg)
}

/// Default foreground of the active theme (used e.g. for the visual-bell flash).
pub fn default_fg() -> [f32; 4] {
    to_f32(active().fg)
}

/// Selection-tint background of the active theme.
pub fn selection_bg() -> [f32; 4] {
    to_f32(active().selection_bg)
}

/// Resolve a terminal `Color` (named / indexed / direct) to RGBA in [0, 1].
pub fn resolve(color: Color, colors: &Colors) -> [f32; 4] {
    let rgb = match color {
        Color::Spec(rgb) => rgb,
        Color::Named(named) => colors[named].unwrap_or_else(|| default_named(named)),
        Color::Indexed(idx) => {
            colors[idx as usize].unwrap_or_else(|| default_indexed(idx))
        }
    };
    to_f32(rgb)
}

fn to_f32(rgb: Rgb) -> [f32; 4] {
    [
        rgb.r as f32 / 255.0,
        rgb.g as f32 / 255.0,
        rgb.b as f32 / 255.0,
        1.0,
    ]
}

fn dim(rgb: Rgb) -> Rgb {
    Rgb {
        r: (rgb.r as u16 * 2 / 3) as u8,
        g: (rgb.g as u16 * 2 / 3) as u8,
        b: (rgb.b as u16 * 2 / 3) as u8,
    }
}

fn default_named(named: NamedColor) -> Rgb {
    let theme = active();
    match named as usize {
        i @ 0..=15 => theme.ansi16[i],
        256 | 267 => theme.fg,                         // Foreground, BrightForeground
        257 => theme.bg,                               // Background
        258 => theme.cursor,                           // Cursor
        i @ 259..=266 => dim(theme.ansi16[i - 259]),   // DimBlack..DimWhite
        268 => dim(theme.fg),                          // DimForeground
        _ => theme.fg,
    }
}

/// Default value for an 8-bit indexed color (xterm 256-color scheme), using the
/// active theme's 16-color base.
fn default_indexed(idx: u8) -> Rgb {
    match idx {
        0..=15 => active().ansi16[idx as usize],
        16..=231 => {
            // 6x6x6 color cube.
            let i = idx - 16;
            let r = i / 36;
            let g = (i % 36) / 6;
            let b = i % 6;
            let level = |v: u8| if v == 0 { 0 } else { v * 40 + 55 };
            Rgb { r: level(r), g: level(g), b: level(b) }
        }
        232..=255 => {
            // 24-step grayscale ramp.
            let v = 8 + (idx - 232) * 10;
            Rgb { r: v, g: v, b: v }
        }
    }
}
