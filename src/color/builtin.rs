//! The 18 built-in color themes as `const Theme` values, moved verbatim out of
//! the old monolithic `color.rs`. This module holds only the color data — see
//! [`super::registry`] for the single source of truth mapping names/aliases to
//! these values.

use alacritty_terminal::vte::ansi::Rgb;

use super::Theme;

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
pub(crate) const CATPPUCCIN_MOCHA: Theme = Theme {
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
pub(crate) const CATPPUCCIN_MACCHIATO: Theme = Theme {
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
pub(crate) const GRUVBOX_DARK: Theme = Theme {
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
pub(crate) const DRACULA: Theme = Theme {
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
pub(crate) const NORD: Theme = Theme {
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
pub(crate) const SOLARIZED_DARK: Theme = Theme {
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
pub(crate) const ROSE_PINE: Theme = Theme {
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
pub(crate) const ROSE_PINE_DAWN: Theme = Theme {
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
pub(crate) const CATPPUCCIN_LATTE: Theme = Theme {
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

/// Everforest Dark (medium): a comfortable, low-saturation green-tinted dark
/// theme. Special entries use fg / bg-dim-medium; cursor follows the foreground.
pub(crate) const EVERFOREST_DARK: Theme = Theme {
    fg: rgb(0xD3, 0xC6, 0xAA),
    bg: rgb(0x2D, 0x35, 0x3B),
    cursor: rgb(0xD3, 0xC6, 0xAA),
    selection_bg: rgb(0x47, 0x52, 0x58),
    ansi16: [
        rgb(0x47, 0x4D, 0x4F), // 0  black
        rgb(0xE6, 0x7E, 0x80), // 1  red
        rgb(0xA7, 0xC0, 0x80), // 2  green
        rgb(0xDB, 0xBC, 0x7F), // 3  yellow
        rgb(0x7F, 0xBB, 0xB3), // 4  blue
        rgb(0xD6, 0x99, 0xB6), // 5  magenta
        rgb(0x83, 0xC0, 0x92), // 6  cyan
        rgb(0xD3, 0xC6, 0xAA), // 7  white
        rgb(0x5C, 0x63, 0x70), // 8  bright black
        rgb(0xE6, 0x7E, 0x80), // 9  bright red
        rgb(0xA7, 0xC0, 0x80), // 10 bright green
        rgb(0xDB, 0xBC, 0x7F), // 11 bright yellow
        rgb(0x7F, 0xBB, 0xB3), // 12 bright blue
        rgb(0xD6, 0x99, 0xB6), // 13 bright magenta
        rgb(0x83, 0xC0, 0x92), // 14 bright cyan
        rgb(0xE9, 0xE3, 0xD0), // 15 bright white
    ],
};

/// Everforest Light (medium): the warm off-white sibling of Everforest, soft on
/// the eyes with the same muted accent family darkened for legibility on light.
pub(crate) const EVERFOREST_LIGHT: Theme = Theme {
    fg: rgb(0x5C, 0x6A, 0x72),
    bg: rgb(0xFD, 0xF6, 0xE3),
    cursor: rgb(0x5C, 0x6A, 0x72),
    selection_bg: rgb(0xEA, 0xDF, 0xC4),
    ansi16: [
        rgb(0x5C, 0x6A, 0x72), // 0  black
        rgb(0xF8, 0x55, 0x52), // 1  red
        rgb(0x8D, 0xA1, 0x01), // 2  green
        rgb(0xDF, 0xA0, 0x00), // 3  yellow
        rgb(0x3A, 0x94, 0xC5), // 4  blue
        rgb(0xDF, 0x69, 0xBA), // 5  magenta
        rgb(0x35, 0xA7, 0x7C), // 6  cyan
        rgb(0x93, 0x9F, 0x91), // 7  white
        rgb(0xA6, 0xB0, 0xA0), // 8  bright black
        rgb(0xF8, 0x55, 0x52), // 9  bright red
        rgb(0x8D, 0xA1, 0x01), // 10 bright green
        rgb(0xDF, 0xA0, 0x00), // 11 bright yellow
        rgb(0x3A, 0x94, 0xC5), // 12 bright blue
        rgb(0xDF, 0x69, 0xBA), // 13 bright magenta
        rgb(0x35, 0xA7, 0x7C), // 14 bright cyan
        rgb(0x5C, 0x6A, 0x72), // 15 bright white
    ],
};

/// Kanagawa Wave: a dark theme inspired by Katsushika Hokusai's "The Great Wave",
/// a deep desaturated indigo base with muted ink-wash accents. Cursor follows fg.
pub(crate) const KANAGAWA: Theme = Theme {
    fg: rgb(0xDC, 0xD7, 0xBA),
    bg: rgb(0x1F, 0x1F, 0x28),
    cursor: rgb(0xC8, 0xC0, 0x93),
    selection_bg: rgb(0x2D, 0x4F, 0x67),
    ansi16: [
        rgb(0x16, 0x16, 0x1D), // 0  black
        rgb(0xC3, 0x40, 0x43), // 1  red
        rgb(0x76, 0x94, 0x6A), // 2  green
        rgb(0xC0, 0xA3, 0x6E), // 3  yellow
        rgb(0x7E, 0x9C, 0xD8), // 4  blue
        rgb(0x95, 0x7F, 0xB8), // 5  magenta
        rgb(0x6A, 0x95, 0x89), // 6  cyan
        rgb(0xC8, 0xC0, 0x93), // 7  white
        rgb(0x72, 0x71, 0x69), // 8  bright black
        rgb(0xE8, 0x2C, 0x42), // 9  bright red
        rgb(0x98, 0xBB, 0x6C), // 10 bright green
        rgb(0xE6, 0xC3, 0x84), // 11 bright yellow
        rgb(0x7F, 0xB4, 0xCA), // 12 bright blue
        rgb(0x93, 0x8A, 0xA9), // 13 bright magenta
        rgb(0x7A, 0xA8, 0x9F), // 14 bright cyan
        rgb(0xDC, 0xD7, 0xBA), // 15 bright white
    ],
};

/// One Dark: the Atom-derived modern classic — a balanced cool-gray base with
/// crisp, slightly desaturated accents. Cursor follows the foreground.
pub(crate) const ONE_DARK: Theme = Theme {
    fg: rgb(0xAB, 0xB2, 0xBF),
    bg: rgb(0x28, 0x2C, 0x34),
    cursor: rgb(0x52, 0x8B, 0xFF),
    selection_bg: rgb(0x3E, 0x44, 0x51),
    ansi16: [
        rgb(0x28, 0x2C, 0x34), // 0  black
        rgb(0xE0, 0x6C, 0x75), // 1  red
        rgb(0x98, 0xC3, 0x79), // 2  green
        rgb(0xE5, 0xC0, 0x7B), // 3  yellow
        rgb(0x61, 0xAF, 0xEF), // 4  blue
        rgb(0xC6, 0x78, 0xDD), // 5  magenta
        rgb(0x56, 0xB6, 0xC2), // 6  cyan
        rgb(0xAB, 0xB2, 0xBF), // 7  white
        rgb(0x54, 0x5B, 0x68), // 8  bright black
        rgb(0xE0, 0x6C, 0x75), // 9  bright red
        rgb(0x98, 0xC3, 0x79), // 10 bright green
        rgb(0xE5, 0xC0, 0x7B), // 11 bright yellow
        rgb(0x61, 0xAF, 0xEF), // 12 bright blue
        rgb(0xC6, 0x78, 0xDD), // 13 bright magenta
        rgb(0x56, 0xB6, 0xC2), // 14 bright cyan
        rgb(0xC8, 0xCE, 0xD9), // 15 bright white
    ],
};

/// One Light: the official light sibling of One Dark — a clean near-white base
/// with the same accent family darkened for contrast on light. Cursor uses blue.
pub(crate) const ONE_LIGHT: Theme = Theme {
    fg: rgb(0x38, 0x3A, 0x42),
    bg: rgb(0xFA, 0xFA, 0xFA),
    cursor: rgb(0x40, 0x78, 0xF2),
    selection_bg: rgb(0xE5, 0xE5, 0xE6),
    ansi16: [
        rgb(0x38, 0x3A, 0x42), // 0  black
        rgb(0xE4, 0x50, 0x49), // 1  red
        rgb(0x50, 0xA1, 0x4F), // 2  green
        rgb(0xC1, 0x84, 0x01), // 3  yellow
        rgb(0x40, 0x78, 0xF2), // 4  blue
        rgb(0xA6, 0x26, 0xA4), // 5  magenta
        rgb(0x01, 0x84, 0xBC), // 6  cyan
        rgb(0xA0, 0xA1, 0xA7), // 7  white
        rgb(0x69, 0x6C, 0x77), // 8  bright black
        rgb(0xE4, 0x50, 0x49), // 9  bright red
        rgb(0x50, 0xA1, 0x4F), // 10 bright green
        rgb(0xC1, 0x84, 0x01), // 11 bright yellow
        rgb(0x40, 0x78, 0xF2), // 12 bright blue
        rgb(0xA6, 0x26, 0xA4), // 13 bright magenta
        rgb(0x01, 0x84, 0xBC), // 14 bright cyan
        rgb(0x38, 0x3A, 0x42), // 15 bright white
    ],
};

/// Ayu Dark: a deep near-black slate base with warm amber accents — high
/// legibility, low glare. Cursor uses the signature amber accent.
pub(crate) const AYU_DARK: Theme = Theme {
    fg: rgb(0xBF, 0xBD, 0xB6),
    bg: rgb(0x0B, 0x0E, 0x14),
    cursor: rgb(0xE6, 0xB4, 0x50),
    selection_bg: rgb(0x1A, 0x23, 0x35),
    ansi16: [
        rgb(0x11, 0x15, 0x1C), // 0  black
        rgb(0xEA, 0x6C, 0x73), // 1  red
        rgb(0xAA, 0xD9, 0x4C), // 2  green
        rgb(0xFF, 0xB4, 0x54), // 3  yellow
        rgb(0x59, 0xC2, 0xFF), // 4  blue
        rgb(0xD2, 0xA6, 0xFF), // 5  magenta
        rgb(0x95, 0xE6, 0xCB), // 6  cyan
        rgb(0xBF, 0xBD, 0xB6), // 7  white
        rgb(0x3D, 0x42, 0x4D), // 8  bright black
        rgb(0xF0, 0x71, 0x78), // 9  bright red
        rgb(0xAA, 0xD9, 0x4C), // 10 bright green
        rgb(0xFF, 0xB4, 0x54), // 11 bright yellow
        rgb(0x73, 0xB8, 0xFF), // 12 bright blue
        rgb(0xD2, 0xA6, 0xFF), // 13 bright magenta
        rgb(0x95, 0xE6, 0xCB), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

/// Ayu Light: a warm off-white base with the same amber accent family darkened
/// for contrast — the official light Ayu variant. Cursor uses amber-orange.
pub(crate) const AYU_LIGHT: Theme = Theme {
    fg: rgb(0x5C, 0x61, 0x66),
    bg: rgb(0xFC, 0xFC, 0xFC),
    cursor: rgb(0xFF, 0x9A, 0x40),
    selection_bg: rgb(0xE7, 0xE8, 0xE9),
    ansi16: [
        rgb(0x68, 0x6B, 0x6E), // 0  black
        rgb(0xE6, 0x5A, 0x4C), // 1  red
        rgb(0x6C, 0xBF, 0x43), // 2  green
        rgb(0xE6, 0xA8, 0x00), // 3  yellow
        rgb(0x39, 0x9E, 0xE6), // 4  blue
        rgb(0xA3, 0x7A, 0xCC), // 5  magenta
        rgb(0x4C, 0xBF, 0x99), // 6  cyan
        rgb(0x80, 0x83, 0x86), // 7  white
        rgb(0x8A, 0x8D, 0x90), // 8  bright black
        rgb(0xE6, 0x5A, 0x4C), // 9  bright red
        rgb(0x6C, 0xBF, 0x43), // 10 bright green
        rgb(0xE6, 0xA8, 0x00), // 11 bright yellow
        rgb(0x39, 0x9E, 0xE6), // 12 bright blue
        rgb(0xA3, 0x7A, 0xCC), // 13 bright magenta
        rgb(0x4C, 0xBF, 0x99), // 14 bright cyan
        rgb(0x5C, 0x61, 0x66), // 15 bright white
    ],
};

/// Gruvbox Light (medium): the warm cream sibling of Gruvbox Dark — high-contrast
/// retro accents on a paper-like base. Cursor follows the dark foreground.
pub(crate) const GRUVBOX_LIGHT: Theme = Theme {
    fg: rgb(0x3C, 0x38, 0x36),
    bg: rgb(0xFB, 0xF1, 0xC7),
    cursor: rgb(0x3C, 0x38, 0x36),
    selection_bg: rgb(0xEB, 0xDB, 0xB2),
    ansi16: [
        rgb(0xFB, 0xF1, 0xC7), // 0  black (bg0)
        rgb(0xCC, 0x24, 0x1D), // 1  red
        rgb(0x98, 0x97, 0x1A), // 2  green
        rgb(0xD7, 0x99, 0x21), // 3  yellow
        rgb(0x45, 0x85, 0x88), // 4  blue
        rgb(0xB1, 0x62, 0x86), // 5  magenta
        rgb(0x68, 0x9D, 0x6A), // 6  cyan
        rgb(0x7C, 0x6F, 0x64), // 7  white
        rgb(0x92, 0x83, 0x74), // 8  bright black
        rgb(0x9D, 0x00, 0x06), // 9  bright red
        rgb(0x79, 0x74, 0x0E), // 10 bright green
        rgb(0xB5, 0x76, 0x14), // 11 bright yellow
        rgb(0x07, 0x66, 0x78), // 12 bright blue
        rgb(0x8F, 0x3F, 0x71), // 13 bright magenta
        rgb(0x42, 0x7B, 0x58), // 14 bright cyan
        rgb(0x3C, 0x38, 0x36), // 15 bright white (fg)
    ],
};
