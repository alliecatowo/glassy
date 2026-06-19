//! Resolve `alacritty_terminal` cell colors to linear-ish RGBA floats.
//!
//! `RenderableContent` carries a dynamic `Colors` palette (OSC overrides, etc.);
//! we consult it first and fall back to a built-in xterm-style 256-color table
//! plus a dark default theme for the special foreground/background entries.
//!
//! The default theme is a Tokyo Night-inspired modern dark scheme: a deep,
//! slightly cool near-black background, a soft lavender-gray foreground, a
//! bright cyan cursor accent, and a cohesive, saturated-but-soft ANSI palette.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

/// Default foreground (soft lavender-gray ~#c0caf5).
pub const DEFAULT_FG: [f32; 4] = [0.752_941_2, 0.792_156_9, 0.960_784_3, 1.0];
/// Default background (deep, cool near-black ~#1a1b26).
pub const DEFAULT_BG: [f32; 4] = [0.101_960_786, 0.105_882_354, 0.149_019_61, 1.0];

const FG: Rgb = Rgb { r: 0xC0, g: 0xCA, b: 0xF5 };
const BG: Rgb = Rgb { r: 0x1A, g: 0x1B, b: 0x26 };
/// Bright cyan cursor accent that stands out against the deep background.
const CURSOR: Rgb = Rgb { r: 0x7D, g: 0xCF, b: 0xFF };

/// Tokyo Night ANSI 16-color palette (8 normal + 8 bright).
const ANSI16: [Rgb; 16] = [
    Rgb { r: 0x15, g: 0x16, b: 0x1E }, // 0  black
    Rgb { r: 0xF7, g: 0x76, b: 0x8E }, // 1  red
    Rgb { r: 0x9E, g: 0xCE, b: 0x6A }, // 2  green
    Rgb { r: 0xE0, g: 0xAF, b: 0x68 }, // 3  yellow
    Rgb { r: 0x7A, g: 0xA2, b: 0xF7 }, // 4  blue
    Rgb { r: 0xBB, g: 0x9A, b: 0xF7 }, // 5  magenta
    Rgb { r: 0x7D, g: 0xCF, b: 0xFF }, // 6  cyan
    Rgb { r: 0xA9, g: 0xB1, b: 0xD6 }, // 7  white
    Rgb { r: 0x41, g: 0x48, b: 0x68 }, // 8  bright black
    Rgb { r: 0xFF, g: 0x9E, b: 0x64 }, // 9  bright red
    Rgb { r: 0x9E, g: 0xCE, b: 0x6A }, // 10 bright green
    Rgb { r: 0xFA, g: 0xBD, b: 0x2F }, // 11 bright yellow
    Rgb { r: 0x7A, g: 0xA2, b: 0xF7 }, // 12 bright blue
    Rgb { r: 0xBB, g: 0x9A, b: 0xF7 }, // 13 bright magenta
    Rgb { r: 0x0D, g: 0xB9, b: 0xD7 }, // 14 bright cyan
    Rgb { r: 0xC0, g: 0xCA, b: 0xF5 }, // 15 bright white
];

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
    match named as usize {
        i @ 0..=15 => ANSI16[i],
        256 | 267 => FG,                       // Foreground, BrightForeground
        257 => BG,                             // Background
        258 => CURSOR,                         // Cursor
        i @ 259..=266 => dim(ANSI16[i - 259]), // DimBlack..DimWhite
        268 => dim(FG),                        // DimForeground
        _ => FG,
    }
}

/// Default value for an 8-bit indexed color (xterm 256-color scheme).
fn default_indexed(idx: u8) -> Rgb {
    match idx {
        0..=15 => ANSI16[idx as usize],
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
