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

use std::sync::atomic::{AtomicPtr, Ordering};

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
pub(crate) const TOKYO_NIGHT: Theme = Theme {
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

/// Catppuccin Macchiato: a slightly warmer, less-contrasty sibling of Mocha.
/// Special entries use Text (fg), Base (bg) and Rosewater (cursor); the ANSI
/// palette follows the project's published terminal mapping.
const CATPPUCCIN_MACCHIATO: Theme = Theme {
    fg: rgb(0xCA, 0xD3, 0xF5),     // Text
    bg: rgb(0x24, 0x27, 0x3A),     // Base
    cursor: rgb(0xF4, 0xDB, 0xD6), // Rosewater
    selection_bg: rgb(0x44, 0x47, 0x5A),
    ansi16: [
        rgb(0x49, 0x4D, 0x64), // 0  black   (Surface1)
        rgb(0xED, 0x87, 0x96), // 1  red     (Red)
        rgb(0xA6, 0xDA, 0x95), // 2  green   (Green)
        rgb(0xEE, 0xD4, 0x9F), // 3  yellow  (Yellow)
        rgb(0x8A, 0xAD, 0xF4), // 4  blue    (Blue)
        rgb(0xF5, 0xBD, 0xE6), // 5  magenta (Pink)
        rgb(0x8B, 0xD5, 0xCA), // 6  cyan    (Teal)
        rgb(0xB8, 0xC0, 0xE0), // 7  white   (Subtext1)
        rgb(0x5B, 0x60, 0x78), // 8  bright black  (Surface2)
        rgb(0xED, 0x87, 0x96), // 9  bright red
        rgb(0xA6, 0xDA, 0x95), // 10 bright green
        rgb(0xEE, 0xD4, 0x9F), // 11 bright yellow
        rgb(0x8A, 0xAD, 0xF4), // 12 bright blue
        rgb(0xF5, 0xBD, 0xE6), // 13 bright magenta
        rgb(0x8B, 0xD5, 0xCA), // 14 bright cyan
        rgb(0xA5, 0xAD, 0xCB), // 15 bright white (Subtext0)
    ],
};

/// Gruvbox Dark: the classic retro-warm theme with a brown-black background and
/// earthy, high-legibility accents. Uses the "dark" (medium) background and the
/// standard fg1/bg0 special entries; cursor follows the foreground.
const GRUVBOX_DARK: Theme = Theme {
    fg: rgb(0xEB, 0xDB, 0xB2),     // fg1
    bg: rgb(0x28, 0x28, 0x28),     // bg0
    cursor: rgb(0xEB, 0xDB, 0xB2), // fg1
    selection_bg: rgb(0x50, 0x49, 0x45),
    ansi16: [
        rgb(0x28, 0x28, 0x28), // 0  black         (bg0)
        rgb(0xCC, 0x24, 0x1D), // 1  red           (neutral red)
        rgb(0x98, 0x97, 0x1A), // 2  green         (neutral green)
        rgb(0xD7, 0x99, 0x21), // 3  yellow        (neutral yellow)
        rgb(0x45, 0x85, 0x88), // 4  blue          (neutral blue)
        rgb(0xB1, 0x62, 0x86), // 5  magenta       (neutral purple)
        rgb(0x68, 0x9D, 0x6A), // 6  cyan          (neutral aqua)
        rgb(0xA8, 0x99, 0x84), // 7  white         (fg4 / gray)
        rgb(0x92, 0x83, 0x74), // 8  bright black   (gray)
        rgb(0xFB, 0x49, 0x34), // 9  bright red
        rgb(0xB8, 0xBB, 0x26), // 10 bright green
        rgb(0xFA, 0xBD, 0x2F), // 11 bright yellow
        rgb(0x83, 0xA5, 0x98), // 12 bright blue
        rgb(0xD3, 0x86, 0x9B), // 13 bright magenta
        rgb(0x8E, 0xC0, 0x7C), // 14 bright cyan
        rgb(0xEB, 0xDB, 0xB2), // 15 bright white   (fg1)
    ],
};

/// Dracula: the famous dark theme with a desaturated indigo background and vivid,
/// candy-bright accents. Special entries use Foreground/Background; cursor follows
/// the foreground per the published spec.
const DRACULA: Theme = Theme {
    fg: rgb(0xF8, 0xF8, 0xF2),     // Foreground
    bg: rgb(0x28, 0x2A, 0x36),     // Background
    cursor: rgb(0xF8, 0xF8, 0xF2), // Foreground
    selection_bg: rgb(0x44, 0x47, 0x5A),
    ansi16: [
        rgb(0x21, 0x22, 0x2C), // 0  black
        rgb(0xFF, 0x55, 0x55), // 1  red
        rgb(0x50, 0xFA, 0x7B), // 2  green
        rgb(0xF1, 0xFA, 0x8C), // 3  yellow
        rgb(0xBD, 0x93, 0xF9), // 4  blue   (Purple)
        rgb(0xFF, 0x79, 0xC6), // 5  magenta(Pink)
        rgb(0x8B, 0xE9, 0xFD), // 6  cyan
        rgb(0xF8, 0xF8, 0xF2), // 7  white
        rgb(0x62, 0x72, 0xA4), // 8  bright black  (Comment)
        rgb(0xFF, 0x6E, 0x6E), // 9  bright red
        rgb(0x69, 0xFF, 0x94), // 10 bright green
        rgb(0xFF, 0xFF, 0xA5), // 11 bright yellow
        rgb(0xD6, 0xAC, 0xFF), // 12 bright blue
        rgb(0xFF, 0x92, 0xDF), // 13 bright magenta
        rgb(0xA4, 0xFF, 0xFF), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

/// Nord: an arctic, bluish color palette with low-contrast frost/aurora accents.
/// Special entries use nord4 (fg) / nord0 (bg); cursor follows the foreground.
const NORD: Theme = Theme {
    fg: rgb(0xD8, 0xDE, 0xE9),     // nord4
    bg: rgb(0x2E, 0x34, 0x40),     // nord0
    cursor: rgb(0xD8, 0xDE, 0xE9), // nord4
    selection_bg: rgb(0x43, 0x4C, 0x5E),
    ansi16: [
        rgb(0x3B, 0x42, 0x52), // 0  black         (nord1)
        rgb(0xBF, 0x61, 0x6A), // 1  red           (nord11)
        rgb(0xA3, 0xBE, 0x8C), // 2  green         (nord14)
        rgb(0xEB, 0xCB, 0x8B), // 3  yellow        (nord13)
        rgb(0x81, 0xA1, 0xC1), // 4  blue          (nord9)
        rgb(0xB4, 0x8E, 0xAD), // 5  magenta       (nord15)
        rgb(0x88, 0xC0, 0xD0), // 6  cyan          (nord8)
        rgb(0xE5, 0xE9, 0xF0), // 7  white         (nord5)
        rgb(0x4C, 0x56, 0x6A), // 8  bright black   (nord3)
        rgb(0xBF, 0x61, 0x6A), // 9  bright red     (nord11)
        rgb(0xA3, 0xBE, 0x8C), // 10 bright green   (nord14)
        rgb(0xEB, 0xCB, 0x8B), // 11 bright yellow  (nord13)
        rgb(0x81, 0xA1, 0xC1), // 12 bright blue    (nord9)
        rgb(0xB4, 0x8E, 0xAD), // 13 bright magenta (nord15)
        rgb(0x8F, 0xBC, 0xBB), // 14 bright cyan    (nord7)
        rgb(0xEC, 0xEF, 0xF4), // 15 bright white   (nord6)
    ],
};

/// Solarized Dark: Ethan Schoonover's precision palette on the dark base03
/// background with base0 body text. Special entries use base0 (fg) / base03 (bg);
/// cursor follows the foreground.
const SOLARIZED_DARK: Theme = Theme {
    fg: rgb(0x83, 0x94, 0x96),     // base0
    bg: rgb(0x00, 0x2B, 0x36),     // base03
    cursor: rgb(0x83, 0x94, 0x96), // base0
    selection_bg: rgb(0x07, 0x36, 0x42),
    ansi16: [
        rgb(0x07, 0x36, 0x42), // 0  black         (base02)
        rgb(0xDC, 0x32, 0x2F), // 1  red
        rgb(0x85, 0x99, 0x00), // 2  green
        rgb(0xB5, 0x89, 0x00), // 3  yellow
        rgb(0x26, 0x8B, 0xD2), // 4  blue
        rgb(0xD3, 0x36, 0x82), // 5  magenta
        rgb(0x2A, 0xA1, 0x98), // 6  cyan
        rgb(0xEE, 0xE8, 0xD5), // 7  white         (base2)
        rgb(0x00, 0x2B, 0x36), // 8  bright black   (base03)
        rgb(0xCB, 0x4B, 0x16), // 9  bright red     (orange)
        rgb(0x58, 0x6E, 0x75), // 10 bright green   (base01)
        rgb(0x65, 0x7B, 0x83), // 11 bright yellow  (base00)
        rgb(0x83, 0x94, 0x96), // 12 bright blue    (base0)
        rgb(0x6C, 0x71, 0xC4), // 13 bright magenta (violet)
        rgb(0x93, 0xA1, 0xA1), // 14 bright cyan    (base1)
        rgb(0xFD, 0xF6, 0xE3), // 15 bright white   (base3)
    ],
};

/// Rose Pine: a soho-vibes theme with a muted rose-tinted dark base and gentle
/// pastel accents. Special entries use Text (fg) / Base (bg); cursor follows the
/// Highlight High tint per the published terminal palette.
const ROSE_PINE: Theme = Theme {
    fg: rgb(0xE0, 0xDE, 0xF4),           // Text
    bg: rgb(0x19, 0x17, 0x24),           // Base
    cursor: rgb(0x52, 0x4F, 0x67),       // Highlight High
    selection_bg: rgb(0x2A, 0x28, 0x3E), // Highlight Med
    ansi16: [
        rgb(0x26, 0x23, 0x3A), // 0  black         (Overlay)
        rgb(0xEB, 0x6F, 0x92), // 1  red           (Love)
        rgb(0x31, 0x74, 0x8F), // 2  green         (Pine)
        rgb(0xF6, 0xC1, 0x77), // 3  yellow        (Gold)
        rgb(0x9C, 0xCF, 0xD8), // 4  blue          (Foam)
        rgb(0xC4, 0xA7, 0xE7), // 5  magenta       (Iris)
        rgb(0xEB, 0xBC, 0xBA), // 6  cyan          (Rose)
        rgb(0xE0, 0xDE, 0xF4), // 7  white         (Text)
        rgb(0x6E, 0x6A, 0x86), // 8  bright black   (Subtle)
        rgb(0xEB, 0x6F, 0x92), // 9  bright red     (Love)
        rgb(0x31, 0x74, 0x8F), // 10 bright green   (Pine)
        rgb(0xF6, 0xC1, 0x77), // 11 bright yellow  (Gold)
        rgb(0x9C, 0xCF, 0xD8), // 12 bright blue    (Foam)
        rgb(0xC4, 0xA7, 0xE7), // 13 bright magenta (Iris)
        rgb(0xEB, 0xBC, 0xBA), // 14 bright cyan    (Rose)
        rgb(0xE0, 0xDE, 0xF4), // 15 bright white   (Text)
    ],
};

/// Rosé Pine Dawn: the official LIGHT sibling of Rosé Pine — a warm, low-glare
/// off-white base with the same muted-pastel accent family, darkened for legible
/// contrast on a light surface. Special entries use Text (fg) / Base (bg); cursor
/// follows the Highlight High tint per the published terminal palette.
const ROSE_PINE_DAWN: Theme = Theme {
    fg: rgb(0x57, 0x52, 0x79),           // Text
    bg: rgb(0xFA, 0xF4, 0xED),           // Base
    cursor: rgb(0x57, 0x52, 0x79),       // Text (dark on light)
    selection_bg: rgb(0xDF, 0xDA, 0xD9), // Highlight Med
    ansi16: [
        rgb(0xF2, 0xE9, 0xE1), // 0  black         (Overlay, light)
        rgb(0xB4, 0x63, 0x7A), // 1  red           (Love)
        rgb(0x28, 0x69, 0x83), // 2  green         (Pine)
        rgb(0xEA, 0x9D, 0x34), // 3  yellow        (Gold)
        rgb(0x56, 0x94, 0x9F), // 4  blue          (Foam)
        rgb(0x90, 0x7A, 0xA9), // 5  magenta       (Iris)
        rgb(0xD7, 0x82, 0x7E), // 6  cyan          (Rose)
        rgb(0x57, 0x52, 0x79), // 7  white         (Text)
        rgb(0x9D, 0x96, 0xB8), // 8  bright black   (Subtle)
        rgb(0xB4, 0x63, 0x7A), // 9  bright red     (Love)
        rgb(0x28, 0x69, 0x83), // 10 bright green   (Pine)
        rgb(0xEA, 0x9D, 0x34), // 11 bright yellow  (Gold)
        rgb(0x56, 0x94, 0x9F), // 12 bright blue    (Foam)
        rgb(0x90, 0x7A, 0xA9), // 13 bright magenta (Iris)
        rgb(0xD7, 0x82, 0x7E), // 14 bright cyan    (Rose)
        rgb(0x57, 0x52, 0x79), // 15 bright white   (Text)
    ],
};

/// Catppuccin Latte: the LIGHT member of the Catppuccin family — a crisp, bright
/// off-white base (Base) with Text body color and the published light terminal
/// accents, darkened so reds/greens/blues stay readable on white. Cursor uses
/// Rosewater per the published spec.
const CATPPUCCIN_LATTE: Theme = Theme {
    fg: rgb(0x4C, 0x4F, 0x69),     // Text
    bg: rgb(0xEF, 0xF1, 0xF5),     // Base
    cursor: rgb(0xDC, 0x8A, 0x78), // Rosewater
    selection_bg: rgb(0xCC, 0xD0, 0xDA),
    ansi16: [
        rgb(0x5C, 0x5F, 0x77), // 0  black   (Subtext1)
        rgb(0xD2, 0x0F, 0x39), // 1  red     (Red)
        rgb(0x40, 0xA0, 0x2B), // 2  green   (Green)
        rgb(0xDF, 0x8E, 0x1D), // 3  yellow  (Yellow)
        rgb(0x1E, 0x66, 0xF5), // 4  blue    (Blue)
        rgb(0xEA, 0x76, 0xCB), // 5  magenta (Pink)
        rgb(0x17, 0x92, 0x99), // 6  cyan    (Teal)
        rgb(0xAC, 0xB0, 0xBE), // 7  white   (Surface2)
        rgb(0x6C, 0x6F, 0x85), // 8  bright black  (Subtext0)
        rgb(0xD2, 0x0F, 0x39), // 9  bright red
        rgb(0x40, 0xA0, 0x2B), // 10 bright green
        rgb(0xDF, 0x8E, 0x1D), // 11 bright yellow
        rgb(0x1E, 0x66, 0xF5), // 12 bright blue
        rgb(0xEA, 0x76, 0xCB), // 13 bright magenta
        rgb(0x17, 0x92, 0x99), // 14 bright cyan
        rgb(0xBC, 0xC0, 0xCC), // 15 bright white (Surface1)
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
        "catppuccinmacchiato" | "macchiato" => Some(CATPPUCCIN_MACCHIATO),
        "gruvboxdark" | "gruvbox" => Some(GRUVBOX_DARK),
        "dracula" => Some(DRACULA),
        "nord" => Some(NORD),
        "solarizeddark" | "solarized" => Some(SOLARIZED_DARK),
        "rosepine" | "rose" => Some(ROSE_PINE),
        "rosepinedawn" | "dawn" => Some(ROSE_PINE_DAWN),
        "catppuccinlatte" | "latte" => Some(CATPPUCCIN_LATTE),
        _ => None,
    }
}

/// Canonical theme names in display order, for the settings overlay to cycle.
pub const THEME_NAMES: &[&str] = &[
    "tokyo-night",
    "catppuccin-mocha",
    "catppuccin-macchiato",
    "gruvbox-dark",
    "dracula",
    "nord",
    "solarized-dark",
    "rose-pine",
    "rose-pine-dawn",
    "catppuccin-latte",
];

/// Whether a named theme is a LIGHT theme (light background, dark text). Used to
/// pick a sensible default when following the system color scheme. Unknown names
/// are treated as dark (every original built-in is dark).
#[allow(dead_code)]
pub fn is_light(name: &str) -> bool {
    matches!(canonical_name(name), "rose-pine-dawn" | "catppuccin-latte")
}

/// Map any accepted theme name/alias to its canonical [`THEME_NAMES`] entry,
/// defaulting to `tokyo-night`. Lets the app store + cycle + save a stable name.
pub fn canonical_name(input: &str) -> &'static str {
    let key: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match key.as_str() {
        "catppuccinmocha" | "catppuccin" | "mocha" => "catppuccin-mocha",
        "catppuccinmacchiato" | "macchiato" => "catppuccin-macchiato",
        "gruvboxdark" | "gruvbox" => "gruvbox-dark",
        "dracula" => "dracula",
        "nord" => "nord",
        "solarizeddark" | "solarized" => "solarized-dark",
        "rosepine" | "rose" => "rose-pine",
        "rosepinedawn" | "dawn" => "rose-pine-dawn",
        "catppuccinlatte" | "latte" => "catppuccin-latte",
        _ => "tokyo-night",
    }
}

/// The process-wide active theme. An `AtomicPtr` to a leaked `Theme` so reads
/// (per cell, on the UI thread) are a single relaxed load + deref — no lock —
/// while `set_theme` can swap it live (settings overlay). Null means "default".
static ACTIVE: AtomicPtr<Theme> = AtomicPtr::new(std::ptr::null_mut());

/// Install the active theme. Safe to call repeatedly (startup + live changes).
/// Frees the previous theme instead of leaking it.
pub fn set_theme(theme: Theme) {
    let ptr = Box::into_raw(Box::new(theme));
    let old_ptr = ACTIVE.swap(ptr, Ordering::AcqRel);
    // Free the previous theme if it was set (not null and not the static defaults).
    if !old_ptr.is_null() {
        // SAFETY: `old_ptr` is a pointer produced by `Box::into_raw` in a prior
        // `set_theme` call, so it is valid and safe to drop.
        let _ = unsafe { Box::from_raw(old_ptr) };
    }
}

/// The active theme, defaulting to Tokyo Night before `set_theme` is called.
fn active() -> &'static Theme {
    let ptr = ACTIVE.load(Ordering::Acquire);
    if ptr.is_null() {
        &TOKYO_NIGHT
    } else {
        // SAFETY: `ptr` is either null (handled above) or a pointer produced by
        // `Box::into_raw` in `set_theme` and never freed, so it is valid for the
        // life of the process.
        unsafe { &*ptr }
    }
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

/// The UI accent of the active theme, derived from its cursor color — the one
/// entry every theme picks deliberately to "pop". Used by the inline toolbar
/// (active chip fill, mark, new-tab `+`) so accents follow the theme instead of
/// a hardcoded blue that clashes on Gruvbox / Solarized / etc.
pub fn accent() -> [f32; 4] {
    to_f32(active().cursor)
}

/// The UI danger color of the active theme, derived from ANSI red — used for
/// destructive affordances (the hovered tab-close ✕) so "danger" reads as red
/// in whatever red the theme actually uses.
#[allow(dead_code)]
pub fn danger() -> [f32; 4] {
    to_f32(active().ansi16[1])
}

/// Resolve a terminal `Color` (named / indexed / direct) to RGBA in [0, 1].
pub fn resolve(color: Color, colors: &Colors) -> [f32; 4] {
    let rgb = match color {
        Color::Spec(rgb) => rgb,
        Color::Named(named) => colors[named].unwrap_or_else(|| default_named(named)),
        Color::Indexed(idx) => colors[idx as usize].unwrap_or_else(|| default_indexed(idx)),
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
    default_named_index(named as usize)
}

/// Resolve a raw color-query index to an `Rgb` from the active theme.
///
/// `alacritty_terminal`'s `Event::ColorRequest` carries a `usize` index that is
/// either an 8-bit palette slot (`0..=255`, OSC 4) or a `NamedColor` discriminant
/// (`256` Foreground / `257` Background / `258` Cursor, …; OSC 10/11/12). We
/// mirror the same defaults the renderer draws with so a query answer matches the
/// glyphs on screen. (The dynamic OSC-override palette isn't reachable from the
/// `EventProxy`, so overridden entries report the theme default — the common,
/// un-overridden case is exact.)
pub fn query_index(index: usize) -> Rgb {
    match index {
        0..=255 => default_indexed(index as u8),
        _ => default_named_index(index),
    }
}

/// Theme default for a `NamedColor` discriminant given as a raw `usize`, sharing
/// the mapping used by [`default_named`].
fn default_named_index(index: usize) -> Rgb {
    let theme = active();
    match index {
        i @ 0..=15 => theme.ansi16[i],
        256 | 267 => theme.fg, // Foreground, BrightForeground
        257 => theme.bg,       // Background
        258 => theme.cursor,   // Cursor
        i @ 259..=266 => dim(theme.ansi16[i - 259]), // DimBlack..DimWhite
        268 => dim(theme.fg),  // DimForeground
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
            Rgb {
                r: level(r),
                g: level(g),
                b: level(b),
            }
        }
        232..=255 => {
            // 24-step grayscale ramp.
            let v = 8 + (idx - 232) * 10;
            Rgb { r: v, g: v, b: v }
        }
    }
}

#[cfg(test)]
mod query_index_tests {
    use super::*;
    #[test]
    fn named_and_palette_resolve_to_active_theme() {
        let bg = query_index(257);
        assert_eq!((bg.r, bg.g, bg.b), (0x1A, 0x1B, 0x26));
        let fg = query_index(256);
        assert_eq!((fg.r, fg.g, fg.b), (0xC0, 0xCA, 0xF5));
        let cur = query_index(258);
        assert_eq!((cur.r, cur.g, cur.b), (0x7D, 0xCF, 0xFF));
        let red = query_index(1);
        assert_eq!((red.r, red.g, red.b), (0xF7, 0x76, 0x8E));
        let cube = query_index(196); // pure red in 6x6x6 cube
        assert_eq!((cube.r, cube.g, cube.b), (255, 0, 0));
    }

    #[test]
    fn light_themes_are_light_and_named() {
        // Both light themes resolve and are flagged light; every dark built-in is
        // flagged dark.
        for name in ["rose-pine-dawn", "dawn", "catppuccin-latte", "latte"] {
            assert!(theme_by_name(name).is_some(), "{name} should resolve");
            assert!(is_light(name), "{name} should be light");
        }
        for name in THEME_NAMES.iter().filter(|n| !is_light(n)) {
            let t = theme_by_name(name).expect("theme resolves");
            // A dark theme's background should be darker than its foreground.
            let lum = |c: Rgb| c.r as u32 + c.g as u32 + c.b as u32;
            assert!(lum(t.bg) < lum(t.fg), "{name} bg should be darker than fg");
        }
        // A light theme's background is brighter than its foreground.
        let dawn = theme_by_name("rose-pine-dawn").unwrap();
        let lum = |c: Rgb| c.r as u32 + c.g as u32 + c.b as u32;
        assert!(lum(dawn.bg) > lum(dawn.fg));
        // Every THEME_NAMES entry resolves (catches typos / missing arms).
        for name in THEME_NAMES {
            assert!(theme_by_name(name).is_some(), "{name} in THEME_NAMES resolves");
        }
    }
}
