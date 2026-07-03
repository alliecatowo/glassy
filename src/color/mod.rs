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

mod builtin;

pub(crate) use builtin::TOKYO_NIGHT;

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

/// Resolve a theme by (case-insensitive, separator-insensitive) name. Returns
/// `None` for an unknown name so the caller can warn and keep the default.
pub fn theme_by_name(name: &str) -> Option<Theme> {
    let key: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    match key.as_str() {
        "tokyonight" | "tokyo" => Some(builtin::TOKYO_NIGHT),
        "catppuccinmocha" | "catppuccin" | "mocha" => Some(builtin::CATPPUCCIN_MOCHA),
        "catppuccinmacchiato" | "macchiato" => Some(builtin::CATPPUCCIN_MACCHIATO),
        "gruvboxdark" | "gruvbox" => Some(builtin::GRUVBOX_DARK),
        "dracula" => Some(builtin::DRACULA),
        "nord" => Some(builtin::NORD),
        "solarizeddark" | "solarized" => Some(builtin::SOLARIZED_DARK),
        "rosepine" | "rose" => Some(builtin::ROSE_PINE),
        "rosepinedawn" | "dawn" => Some(builtin::ROSE_PINE_DAWN),
        "catppuccinlatte" | "latte" => Some(builtin::CATPPUCCIN_LATTE),
        "everforestdark" | "everforest" => Some(builtin::EVERFOREST_DARK),
        "everforestlight" => Some(builtin::EVERFOREST_LIGHT),
        "kanagawa" | "kanagawawave" => Some(builtin::KANAGAWA),
        "onedark" | "one" => Some(builtin::ONE_DARK),
        "onelight" => Some(builtin::ONE_LIGHT),
        "ayudark" | "ayu" => Some(builtin::AYU_DARK),
        "ayulight" => Some(builtin::AYU_LIGHT),
        "gruvboxlight" => Some(builtin::GRUVBOX_LIGHT),
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
    "everforest-dark",
    "everforest-light",
    "kanagawa",
    "one-dark",
    "one-light",
    "ayu-dark",
    "ayu-light",
    "gruvbox-light",
];

/// Whether a named theme is a LIGHT theme (light background, dark text). Used to
/// pick a sensible default when following the system color scheme. Unknown names
/// are treated as dark (every original built-in is dark).
#[allow(dead_code)]
pub fn is_light(name: &str) -> bool {
    matches!(
        canonical_name(name),
        "rose-pine-dawn"
            | "catppuccin-latte"
            | "everforest-light"
            | "one-light"
            | "ayu-light"
            | "gruvbox-light"
    )
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
        "everforestdark" | "everforest" => "everforest-dark",
        "everforestlight" => "everforest-light",
        "kanagawa" | "kanagawawave" => "kanagawa",
        "onedark" | "one" => "one-dark",
        "onelight" => "one-light",
        "ayudark" | "ayu" => "ayu-dark",
        "ayulight" => "ayu-light",
        "gruvboxlight" => "gruvbox-light",
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
        &builtin::TOKYO_NIGHT
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

/// The UI success color of the active theme, derived from ANSI green — used for
/// affirmative affordances (the command-block exit-0 badge ✓) so "success"
/// reads as green in whatever green the theme actually uses.
#[allow(dead_code)]
pub fn success() -> [f32; 4] {
    to_f32(active().ansi16[2])
}

/// Format an `Rgb` as a `#rrggbb` hex string (used by the custom-theme editor to
/// seed its text fields from a base theme + serialize edits back to config).
pub fn rgb_to_hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// The current active theme (a copy), so the custom-theme editor can seed its
/// fields from whatever palette is live (named theme, wallpaper, or overrides).
pub fn active_theme() -> Theme {
    *active()
}

/// Construct a [`Theme`] directly from raw `Rgb` parts: the four specials plus the
/// 16 ANSI entries. Used by the custom-theme editor to build a live preview from
/// the edited hex fields without going through the config parser.
pub fn theme_from_parts(
    fg: Rgb,
    bg: Rgb,
    cursor: Rgb,
    selection_bg: Rgb,
    ansi16: [Rgb; 16],
) -> Theme {
    Theme {
        fg,
        bg,
        cursor,
        selection_bg,
        ansi16,
    }
}

/// Accessors for a [`Theme`]'s component colors, exposed so the custom-theme
/// editor can read a base theme's entries to seed its fields.
impl Theme {
    pub fn fg(&self) -> Rgb {
        self.fg
    }
    pub fn bg(&self) -> Rgb {
        self.bg
    }
    pub fn cursor(&self) -> Rgb {
        self.cursor
    }
    pub fn selection_bg(&self) -> Rgb {
        self.selection_bg
    }
    pub fn ansi(&self, i: usize) -> Rgb {
        self.ansi16[i.min(15)]
    }
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

/// Lighten a color by adding a linear amount to each channel, clamped to [0, 1].
/// Used by GUI surfaces to create elevated hierarchy without changing the hue.
pub fn lighten(c: [f32; 4], amount: f32) -> [f32; 4] {
    [
        (c[0] + amount).min(1.0),
        (c[1] + amount).min(1.0),
        (c[2] + amount).min(1.0),
        c[3],
    ]
}

/// Darken a color by multiplying each channel by a factor in [0, 1].
/// Used by GUI surfaces for shadows and hover states.
pub fn darken(c: [f32; 4], f: f32) -> [f32; 4] {
    [c[0] * f, c[1] * f, c[2] * f, c[3]]
}

/// Compute the relative luminance of a color using the standard formula
/// (0.299*R + 0.587*G + 0.114*B), used to determine whether to lighten or
/// darken a surface for contrast on near-white/light backgrounds.
pub fn luma(c: [f32; 4]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
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
        for name in [
            "rose-pine-dawn",
            "dawn",
            "catppuccin-latte",
            "latte",
            "everforest-light",
            "one-light",
            "ayu-light",
            "gruvbox-light",
        ] {
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
            assert!(
                theme_by_name(name).is_some(),
                "{name} in THEME_NAMES resolves"
            );
        }
    }
}
