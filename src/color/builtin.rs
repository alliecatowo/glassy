//! The built-in color themes as `const Theme` values. The original 18 were
//! moved verbatim out of the old monolithic `color.rs`; the w14 themes-pack
//! wave added a further 42 (12 light + 30 dark), grouped below by pack. This
//! module holds only the color data — see [`super::registry`] for the single
//! source of truth mapping names/aliases to these values.

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

// Light pack (added in the w14 themes-pack wave) -- see registry.rs for the
// ThemeEntry list mapping these to canonical names/aliases.
// ===========================================================================

// Source: GitHub Light Default.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// GitHub Light: GitHub's default light editor/terminal theme -- crisp white background, near-black body text, GitHub's signature blue links/accents.
pub(crate) const GITHUB_LIGHT: Theme = Theme {
    fg: rgb(0x1F, 0x23, 0x28),
    bg: rgb(0xFF, 0xFF, 0xFF),
    cursor: rgb(0x09, 0x69, 0xDA),
    selection_bg: rgb(0xE2, 0xE2, 0xE3),
    ansi16: [
        rgb(0x24, 0x29, 0x2F), // 0  black
        rgb(0xCF, 0x22, 0x2E), // 1  red
        rgb(0x11, 0x63, 0x29), // 2  green
        rgb(0x4D, 0x2D, 0x00), // 3  yellow
        rgb(0x09, 0x69, 0xDA), // 4  blue
        rgb(0x82, 0x50, 0xDF), // 5  magenta
        rgb(0x1B, 0x7C, 0x83), // 6  cyan
        rgb(0x6E, 0x77, 0x81), // 7  white
        rgb(0x57, 0x60, 0x6A), // 8  bright black
        rgb(0xA4, 0x0E, 0x26), // 9  bright red
        rgb(0x1A, 0x7F, 0x37), // 10 bright green
        rgb(0x63, 0x3C, 0x01), // 11 bright yellow
        rgb(0x21, 0x8B, 0xFF), // 12 bright blue
        rgb(0xA4, 0x75, 0xF9), // 13 bright magenta
        rgb(0x31, 0x92, 0xAA), // 14 bright cyan
        rgb(0x8C, 0x95, 0x9F), // 15 bright white
    ],
};

// Source: canonical Solarized spec (ethanschoonover.com), cross-verified against alacritty/alacritty-theme solarized_light.toml and iTerm2 Solarized Light.conf; ANSI 0-6/8-14 match the already-shipped SOLARIZED_DARK exactly (Solarized invariant). color7 uses the iTerm2 port's #bbb5a2 (darker than base2 #eee8d5) since base2-on-base3 is nearly invisible as a 'white' swatch on a light background.
/// Solarized Light: Ethan Schoonover's precision palette on the light base3 background with base00 body text. The ANSI 16 are IDENTICAL to Solarized Dark (Solarized's defining trait: one fixed accent set, only the base end swaps) -- see SOLARIZED_DARK above. Special entries use base00 (fg) / base3 (bg); cursor follows the foreground; selection uses base2, the classic Solarized light highlight tone.
pub(crate) const SOLARIZED_LIGHT: Theme = Theme {
    fg: rgb(0x65, 0x7B, 0x83),
    bg: rgb(0xFD, 0xF6, 0xE3),
    cursor: rgb(0x65, 0x7B, 0x83),
    selection_bg: rgb(0xEE, 0xE8, 0xD5),
    ansi16: [
        rgb(0x07, 0x36, 0x42), // 0  black
        rgb(0xDC, 0x32, 0x2F), // 1  red
        rgb(0x85, 0x99, 0x00), // 2  green
        rgb(0xB5, 0x89, 0x00), // 3  yellow
        rgb(0x26, 0x8B, 0xD2), // 4  blue
        rgb(0xD3, 0x36, 0x82), // 5  magenta
        rgb(0x2A, 0xA1, 0x98), // 6  cyan
        rgb(0xBB, 0xB5, 0xA2), // 7  white
        rgb(0x00, 0x2B, 0x36), // 8  bright black
        rgb(0xCB, 0x4B, 0x16), // 9  bright red
        rgb(0x58, 0x6E, 0x75), // 10 bright green
        rgb(0x65, 0x7B, 0x83), // 11 bright yellow
        rgb(0x83, 0x94, 0x96), // 12 bright blue
        rgb(0x6C, 0x71, 0xC4), // 13 bright magenta
        rgb(0x93, 0xA1, 0xA1), // 14 bright cyan
        rgb(0xFD, 0xF6, 0xE3), // 15 bright white
    ],
};

// Source: One Half Light.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// One Half Light: the light sibling of the One Half family -- a soft off-white background with the same balanced, slightly desaturated accents as One Half Dark.
pub(crate) const ONE_HALF_LIGHT: Theme = Theme {
    fg: rgb(0x38, 0x3A, 0x42),
    bg: rgb(0xFA, 0xFA, 0xFA),
    cursor: rgb(0xA5, 0xB4, 0xE5),
    selection_bg: rgb(0xE1, 0xE1, 0xE2),
    ansi16: [
        rgb(0x38, 0x3A, 0x42), // 0  black
        rgb(0xE4, 0x56, 0x49), // 1  red
        rgb(0x50, 0xA1, 0x4F), // 2  green
        rgb(0xC1, 0x84, 0x01), // 3  yellow
        rgb(0x01, 0x84, 0xBC), // 4  blue
        rgb(0xA6, 0x26, 0xA4), // 5  magenta
        rgb(0x09, 0x97, 0xB3), // 6  cyan
        rgb(0xBA, 0xBA, 0xBA), // 7  white
        rgb(0x4F, 0x52, 0x5E), // 8  bright black
        rgb(0xE0, 0x6C, 0x75), // 9  bright red
        rgb(0x98, 0xC3, 0x79), // 10 bright green
        rgb(0xD8, 0xB3, 0x6E), // 11 bright yellow
        rgb(0x61, 0xAF, 0xEF), // 12 bright blue
        rgb(0xC6, 0x78, 0xDD), // 13 bright magenta
        rgb(0x56, 0xB6, 0xC2), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: official folke/tokyonight.nvim kitty export (extras/kitty/tokyonight_day.conf)
/// Tokyo Night Day: the official LIGHT variant of Tokyo Night -- a cool, slightly blue-gray paper background with the same saturated accent family darkened for contrast.
pub(crate) const TOKYO_NIGHT_DAY: Theme = Theme {
    fg: rgb(0x37, 0x60, 0xBF),
    bg: rgb(0xE1, 0xE2, 0xE7),
    cursor: rgb(0x37, 0x60, 0xBF),
    selection_bg: rgb(0xB7, 0xC1, 0xE3),
    ansi16: [
        rgb(0xB4, 0xB5, 0xB9), // 0  black
        rgb(0xF5, 0x2A, 0x65), // 1  red
        rgb(0x58, 0x75, 0x39), // 2  green
        rgb(0x8C, 0x6C, 0x3E), // 3  yellow
        rgb(0x2E, 0x7D, 0xE9), // 4  blue
        rgb(0x98, 0x54, 0xF1), // 5  magenta
        rgb(0x00, 0x71, 0x97), // 6  cyan
        rgb(0x61, 0x72, 0xB0), // 7  white
        rgb(0xA1, 0xA6, 0xC5), // 8  bright black
        rgb(0xFF, 0x47, 0x74), // 9  bright red
        rgb(0x5C, 0x85, 0x24), // 10 bright green
        rgb(0xA2, 0x76, 0x29), // 11 bright yellow
        rgb(0x35, 0x8A, 0xFF), // 12 bright blue
        rgb(0xA4, 0x63, 0xFF), // 13 bright magenta
        rgb(0x00, 0x7E, 0xA8), // 14 bright cyan
        rgb(0x37, 0x60, 0xBF), // 15 bright white
    ],
};

// Source: official rebelot/kanagawa.nvim kitty export (extras/kitty/kanagawa_light.conf, the Lotus variant)
/// Kanagawa Lotus: the official LIGHT variant of Kanagawa -- a warm paper-cream background with the same ink-wash accent family re-tuned for legibility on light.
pub(crate) const KANAGAWA_LOTUS: Theme = Theme {
    fg: rgb(0x54, 0x54, 0x64),
    bg: rgb(0xF2, 0xEC, 0xBC),
    cursor: rgb(0x43, 0x43, 0x6C),
    selection_bg: rgb(0xC9, 0xCB, 0xD1),
    ansi16: [
        rgb(0x1F, 0x1F, 0x28), // 0  black
        rgb(0xC8, 0x40, 0x53), // 1  red
        rgb(0x6F, 0x89, 0x4E), // 2  green
        rgb(0x77, 0x71, 0x3F), // 3  yellow
        rgb(0x4D, 0x69, 0x9B), // 4  blue
        rgb(0xB3, 0x5B, 0x79), // 5  magenta
        rgb(0x59, 0x7B, 0x75), // 6  cyan
        rgb(0x54, 0x54, 0x64), // 7  white
        rgb(0x8A, 0x89, 0x80), // 8  bright black
        rgb(0xD7, 0x47, 0x4B), // 9  bright red
        rgb(0x6E, 0x91, 0x5F), // 10 bright green
        rgb(0x83, 0x6F, 0x4A), // 11 bright yellow
        rgb(0x66, 0x93, 0xBF), // 12 bright blue
        rgb(0x62, 0x4C, 0x83), // 13 bright magenta
        rgb(0x5E, 0x85, 0x7A), // 14 bright cyan
        rgb(0x43, 0x43, 0x6C), // 15 bright white
    ],
};

// Source: alacritty/alacritty-theme papercolor_light.toml (port of NLKNguyen/papercolor-theme)
/// PaperColor Light: NLKNguyen's print-inspired theme -- a neutral light-gray 'paper' background with muted, high-legibility accents.
pub(crate) const PAPERCOLOR_LIGHT: Theme = Theme {
    fg: rgb(0x44, 0x44, 0x44),
    bg: rgb(0xEE, 0xEE, 0xEE),
    cursor: rgb(0x44, 0x44, 0x44),
    selection_bg: rgb(0xD8, 0xD8, 0xD8),
    ansi16: [
        rgb(0xEE, 0xEE, 0xEE), // 0  black
        rgb(0xAF, 0x00, 0x00), // 1  red
        rgb(0x00, 0x87, 0x00), // 2  green
        rgb(0x5F, 0x87, 0x00), // 3  yellow
        rgb(0x00, 0x87, 0xAF), // 4  blue
        rgb(0x87, 0x87, 0x87), // 5  magenta
        rgb(0x00, 0x5F, 0x87), // 6  cyan
        rgb(0x44, 0x44, 0x44), // 7  white
        rgb(0xBC, 0xBC, 0xBC), // 8  bright black
        rgb(0xD7, 0x00, 0x00), // 9  bright red
        rgb(0xD7, 0x00, 0x87), // 10 bright green
        rgb(0x87, 0x00, 0xAF), // 11 bright yellow
        rgb(0xD7, 0x5F, 0x00), // 12 bright blue
        rgb(0xD7, 0x5F, 0x00), // 13 bright magenta
        rgb(0x00, 0x5F, 0xAF), // 14 bright cyan
        rgb(0x00, 0x5F, 0x87), // 15 bright white
    ],
};

// Source: Modus Operandi.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Modus Operandi: Protesilaos Stavrou's WCAG-AAA-accessible light theme -- pure white background, pure black text, and deliberately high-contrast accents.
pub(crate) const MODUS_OPERANDI: Theme = Theme {
    fg: rgb(0x00, 0x00, 0x00),
    bg: rgb(0xFF, 0xFF, 0xFF),
    cursor: rgb(0x00, 0x00, 0x00),
    selection_bg: rgb(0xDE, 0xDE, 0xDE),
    ansi16: [
        rgb(0x00, 0x00, 0x00), // 0  black
        rgb(0xA6, 0x00, 0x00), // 1  red
        rgb(0x00, 0x68, 0x00), // 2  green
        rgb(0x6F, 0x55, 0x00), // 3  yellow
        rgb(0x00, 0x31, 0xA9), // 4  blue
        rgb(0x72, 0x10, 0x45), // 5  magenta
        rgb(0x00, 0x5E, 0x8B), // 6  cyan
        rgb(0xA6, 0xA6, 0xA6), // 7  white
        rgb(0x59, 0x59, 0x59), // 8  bright black
        rgb(0x97, 0x25, 0x00), // 9  bright red
        rgb(0x00, 0x66, 0x3F), // 10 bright green
        rgb(0x88, 0x49, 0x00), // 11 bright yellow
        rgb(0x35, 0x48, 0xCF), // 12 bright blue
        rgb(0x53, 0x1A, 0xB6), // 13 bright magenta
        rgb(0x00, 0x5F, 0x5F), // 14 bright cyan
        rgb(0x59, 0x59, 0x59), // 15 bright white
    ],
};

// Source: Flexoki Light.conf (mbadolato/iTerm2-Color-Schemes kitty export), matches kepano/flexoki published palette
/// Flexoki Light: Steph Ango's ink-on-paper theme -- a warm off-white 'paper' background with muted, printer-ink-inspired accents.
pub(crate) const FLEXOKI_LIGHT: Theme = Theme {
    fg: rgb(0x10, 0x0F, 0x0F),
    bg: rgb(0xFF, 0xFC, 0xF0),
    cursor: rgb(0x10, 0x0F, 0x0F),
    selection_bg: rgb(0xE0, 0xDD, 0xD3),
    ansi16: [
        rgb(0x10, 0x0F, 0x0F), // 0  black
        rgb(0xAF, 0x30, 0x29), // 1  red
        rgb(0x66, 0x80, 0x0B), // 2  green
        rgb(0xAD, 0x83, 0x01), // 3  yellow
        rgb(0x20, 0x5E, 0xA6), // 4  blue
        rgb(0xA0, 0x2F, 0x6F), // 5  magenta
        rgb(0x24, 0x83, 0x7B), // 6  cyan
        rgb(0x6F, 0x6E, 0x69), // 7  white
        rgb(0xB7, 0xB5, 0xAC), // 8  bright black
        rgb(0xD1, 0x4D, 0x41), // 9  bright red
        rgb(0x87, 0x9A, 0x39), // 10 bright green
        rgb(0xD0, 0xA2, 0x15), // 11 bright yellow
        rgb(0x43, 0x85, 0xBE), // 12 bright blue
        rgb(0xCE, 0x5D, 0x97), // 13 bright magenta
        rgb(0x3A, 0xA9, 0x9F), // 14 bright cyan
        rgb(0xCE, 0xCD, 0xC3), // 15 bright white
    ],
};

// Source: official antfu/vscode-theme-vitesse theme JSON (themes/vitesse-light.json terminal.ansi* keys)
/// Vitesse Light: Anthony Fu's soft light theme -- a pure white background with muted, slightly desaturated accents (the light sibling of Vitesse Dark).
pub(crate) const VITESSE_LIGHT: Theme = Theme {
    fg: rgb(0x39, 0x3A, 0x34),
    bg: rgb(0xFF, 0xFF, 0xFF),
    cursor: rgb(0x39, 0x3A, 0x34),
    selection_bg: rgb(0xEA, 0xEA, 0xEA),
    ansi16: [
        rgb(0x12, 0x12, 0x12), // 0  black
        rgb(0xAB, 0x59, 0x59), // 1  red
        rgb(0x1E, 0x75, 0x4F), // 2  green
        rgb(0xBD, 0xA4, 0x37), // 3  yellow
        rgb(0x29, 0x6A, 0xA3), // 4  blue
        rgb(0xA1, 0x38, 0x65), // 5  magenta
        rgb(0x29, 0x93, 0xA3), // 6  cyan
        rgb(0xDB, 0xD7, 0xCA), // 7  white
        rgb(0xAA, 0xAA, 0xAA), // 8  bright black
        rgb(0xAB, 0x59, 0x59), // 9  bright red
        rgb(0x1E, 0x75, 0x4F), // 10 bright green
        rgb(0xBD, 0xA4, 0x37), // 11 bright yellow
        rgb(0x29, 0x6A, 0xA3), // 12 bright blue
        rgb(0xA1, 0x38, 0x65), // 13 bright magenta
        rgb(0x29, 0x93, 0xA3), // 14 bright cyan
        rgb(0xDD, 0xDD, 0xDD), // 15 bright white
    ],
};

// Source: official EdenEast/nightfox.nvim kitty export (extra/dayfox/kitty.conf)
/// Dayfox: the LIGHT member of the Nightfox family -- a warm cream background with the same muted, nature-toned accent family as Nightfox.
pub(crate) const DAYFOX: Theme = Theme {
    fg: rgb(0x3D, 0x2B, 0x5A),
    bg: rgb(0xF6, 0xF2, 0xEE),
    cursor: rgb(0x3D, 0x2B, 0x5A),
    selection_bg: rgb(0xE7, 0xD2, 0xBE),
    ansi16: [
        rgb(0x35, 0x2C, 0x24), // 0  black
        rgb(0xA5, 0x22, 0x2F), // 1  red
        rgb(0x39, 0x68, 0x47), // 2  green
        rgb(0xAC, 0x54, 0x02), // 3  yellow
        rgb(0x28, 0x48, 0xA9), // 4  blue
        rgb(0x6E, 0x33, 0xCE), // 5  magenta
        rgb(0x28, 0x79, 0x80), // 6  cyan
        rgb(0xF2, 0xE9, 0xE1), // 7  white
        rgb(0x53, 0x4C, 0x45), // 8  bright black
        rgb(0xB3, 0x43, 0x4E), // 9  bright red
        rgb(0x57, 0x7F, 0x63), // 10 bright green
        rgb(0xB8, 0x6E, 0x28), // 11 bright yellow
        rgb(0x48, 0x63, 0xB6), // 12 bright blue
        rgb(0x84, 0x52, 0xD5), // 13 bright magenta
        rgb(0x48, 0x8D, 0x93), // 14 bright cyan
        rgb(0xF4, 0xEC, 0xE6), // 15 bright white
    ],
};

// Source: Selenized Light.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Selenized Light: Jan Warchoł's contrast-tuned light theme -- a warm cream background with carefully balanced-luminance accents.
pub(crate) const SELENIZED_LIGHT: Theme = Theme {
    fg: rgb(0x53, 0x67, 0x6D),
    bg: rgb(0xFB, 0xF3, 0xDB),
    cursor: rgb(0x53, 0x67, 0x6D),
    selection_bg: rgb(0xE5, 0xE1, 0xCD),
    ansi16: [
        rgb(0xEC, 0xE3, 0xCC), // 0  black
        rgb(0xD2, 0x21, 0x2D), // 1  red
        rgb(0x48, 0x91, 0x00), // 2  green
        rgb(0xAD, 0x89, 0x00), // 3  yellow
        rgb(0x00, 0x72, 0xD4), // 4  blue
        rgb(0xCA, 0x48, 0x98), // 5  magenta
        rgb(0x00, 0x9C, 0x8F), // 6  cyan
        rgb(0x53, 0x67, 0x6D), // 7  white
        rgb(0x90, 0x99, 0x95), // 8  bright black
        rgb(0xCC, 0x17, 0x29), // 9  bright red
        rgb(0x42, 0x8B, 0x00), // 10 bright green
        rgb(0xA7, 0x83, 0x00), // 11 bright yellow
        rgb(0x00, 0x6D, 0xCE), // 12 bright blue
        rgb(0xC4, 0x43, 0x92), // 13 bright magenta
        rgb(0x00, 0x97, 0x8A), // 14 bright cyan
        rgb(0x3A, 0x4D, 0x53), // 15 bright white
    ],
};

// Source: Alabaster.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Alabaster: a minimalist near-white theme -- a bright neutral background, pure-black text, and a restrained, low-saturation accent set.
pub(crate) const ALABASTER: Theme = Theme {
    fg: rgb(0x00, 0x00, 0x00),
    bg: rgb(0xF7, 0xF7, 0xF7),
    cursor: rgb(0x00, 0x7A, 0xCC),
    selection_bg: rgb(0xD7, 0xD7, 0xD7),
    ansi16: [
        rgb(0x00, 0x00, 0x00), // 0  black
        rgb(0xAA, 0x37, 0x31), // 1  red
        rgb(0x44, 0x8C, 0x27), // 2  green
        rgb(0xCB, 0x90, 0x00), // 3  yellow
        rgb(0x32, 0x5C, 0xC0), // 4  blue
        rgb(0x7A, 0x3E, 0x9D), // 5  magenta
        rgb(0x00, 0x83, 0xB2), // 6  cyan
        rgb(0xB7, 0xB7, 0xB7), // 7  white
        rgb(0x77, 0x77, 0x77), // 8  bright black
        rgb(0xF0, 0x50, 0x50), // 9  bright red
        rgb(0x60, 0xCB, 0x00), // 10 bright green
        rgb(0xF2, 0xAF, 0x50), // 11 bright yellow
        rgb(0x00, 0x7A, 0xCC), // 12 bright blue
        rgb(0xE6, 0x4C, 0xE6), // 13 bright magenta
        rgb(0x00, 0xAA, 0xCB), // 14 bright cyan
        rgb(0xF7, 0xF7, 0xF7), // 15 bright white
    ],
};

// ===========================================================================
// Dark pack (added in the w14 themes-pack wave) -- see registry.rs for the
// ThemeEntry list mapping these to canonical names/aliases.
// ===========================================================================

// Source: GitHub Dark Default.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// GitHub Dark: GitHub's default dark editor/terminal theme -- a deep near-black background with GitHub's signature blue accent and muted status colors.
pub(crate) const GITHUB_DARK: Theme = Theme {
    fg: rgb(0xE6, 0xED, 0xF3),
    bg: rgb(0x0D, 0x11, 0x17),
    cursor: rgb(0x2F, 0x81, 0xF7),
    selection_bg: rgb(0x34, 0x39, 0x3F),
    ansi16: [
        rgb(0x48, 0x4F, 0x58), // 0  black
        rgb(0xFF, 0x7B, 0x72), // 1  red
        rgb(0x3F, 0xB9, 0x50), // 2  green
        rgb(0xD2, 0x99, 0x22), // 3  yellow
        rgb(0x58, 0xA6, 0xFF), // 4  blue
        rgb(0xBC, 0x8C, 0xFF), // 5  magenta
        rgb(0x39, 0xC5, 0xCF), // 6  cyan
        rgb(0xB1, 0xBA, 0xC4), // 7  white
        rgb(0x6E, 0x76, 0x81), // 8  bright black
        rgb(0xFF, 0xA1, 0x98), // 9  bright red
        rgb(0x56, 0xD3, 0x64), // 10 bright green
        rgb(0xE3, 0xB3, 0x41), // 11 bright yellow
        rgb(0x79, 0xC0, 0xFF), // 12 bright blue
        rgb(0xD2, 0xA8, 0xFF), // 13 bright magenta
        rgb(0x56, 0xD4, 0xDD), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: Monokai Classic.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Monokai: Wimer Hazenberg's classic -- a warm dark-olive background with the iconic hot-pink/lime/orange accent trio.
pub(crate) const MONOKAI: Theme = Theme {
    fg: rgb(0xFD, 0xFF, 0xF1),
    bg: rgb(0x27, 0x28, 0x22),
    cursor: rgb(0xC0, 0xC1, 0xB5),
    selection_bg: rgb(0x4E, 0x4F, 0x47),
    ansi16: [
        rgb(0x27, 0x28, 0x22), // 0  black
        rgb(0xF9, 0x26, 0x72), // 1  red
        rgb(0xA6, 0xE2, 0x2E), // 2  green
        rgb(0xE6, 0xDB, 0x74), // 3  yellow
        rgb(0xFD, 0x97, 0x1F), // 4  blue
        rgb(0xAE, 0x81, 0xFF), // 5  magenta
        rgb(0x66, 0xD9, 0xEF), // 6  cyan
        rgb(0xFD, 0xFF, 0xF1), // 7  white
        rgb(0x6E, 0x70, 0x66), // 8  bright black
        rgb(0xF9, 0x26, 0x72), // 9  bright red
        rgb(0xA6, 0xE2, 0x2E), // 10 bright green
        rgb(0xE6, 0xDB, 0x74), // 11 bright yellow
        rgb(0xFD, 0x97, 0x1F), // 12 bright blue
        rgb(0xAE, 0x81, 0xFF), // 13 bright magenta
        rgb(0x66, 0xD9, 0xEF), // 14 bright cyan
        rgb(0xFD, 0xFF, 0xF1), // 15 bright white
    ],
};

// Source: Monokai Pro.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Monokai Pro: the official modern refresh of Monokai -- a cooler charcoal background with softened, slightly pastel versions of the classic accents.
pub(crate) const MONOKAI_PRO: Theme = Theme {
    fg: rgb(0xFC, 0xFC, 0xFA),
    bg: rgb(0x2D, 0x2A, 0x2E),
    cursor: rgb(0xC1, 0xC0, 0xC0),
    selection_bg: rgb(0x52, 0x50, 0x53),
    ansi16: [
        rgb(0x2D, 0x2A, 0x2E), // 0  black
        rgb(0xFF, 0x61, 0x88), // 1  red
        rgb(0xA9, 0xDC, 0x76), // 2  green
        rgb(0xFF, 0xD8, 0x66), // 3  yellow
        rgb(0xFC, 0x98, 0x67), // 4  blue
        rgb(0xAB, 0x9D, 0xF2), // 5  magenta
        rgb(0x78, 0xDC, 0xE8), // 6  cyan
        rgb(0xFC, 0xFC, 0xFA), // 7  white
        rgb(0x72, 0x70, 0x72), // 8  bright black
        rgb(0xFF, 0x61, 0x88), // 9  bright red
        rgb(0xA9, 0xDC, 0x76), // 10 bright green
        rgb(0xFF, 0xD8, 0x66), // 11 bright yellow
        rgb(0xFC, 0x98, 0x67), // 12 bright blue
        rgb(0xAB, 0x9D, 0xF2), // 13 bright magenta
        rgb(0x78, 0xDC, 0xE8), // 14 bright cyan
        rgb(0xFC, 0xFC, 0xFA), // 15 bright white
    ],
};

// Source: Material Dark.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Material: the classic Material Theme dark variant -- a warm charcoal background with Google Material Design's muted primary-color accents.
pub(crate) const MATERIAL: Theme = Theme {
    fg: rgb(0xE5, 0xE5, 0xE5),
    bg: rgb(0x23, 0x23, 0x22),
    cursor: rgb(0x16, 0xAF, 0xCA),
    selection_bg: rgb(0x46, 0x46, 0x45),
    ansi16: [
        rgb(0x21, 0x21, 0x21), // 0  black
        rgb(0xB7, 0x14, 0x1F), // 1  red
        rgb(0x45, 0x7B, 0x24), // 2  green
        rgb(0xF6, 0x98, 0x1E), // 3  yellow
        rgb(0x13, 0x4E, 0xB2), // 4  blue
        rgb(0x70, 0x1A, 0xA2), // 5  magenta
        rgb(0x0E, 0x71, 0x7C), // 6  cyan
        rgb(0xEF, 0xEF, 0xEF), // 7  white
        rgb(0x4F, 0x4F, 0x4F), // 8  bright black
        rgb(0xE8, 0x3B, 0x3F), // 9  bright red
        rgb(0x7A, 0xBA, 0x3A), // 10 bright green
        rgb(0xFF, 0xEA, 0x2E), // 11 bright yellow
        rgb(0x54, 0xA4, 0xF3), // 12 bright blue
        rgb(0xAA, 0x4D, 0xBC), // 13 bright magenta
        rgb(0x26, 0xBB, 0xD1), // 14 bright cyan
        rgb(0xD9, 0xD9, 0xD9), // 15 bright white
    ],
};

// Source: Material Darker.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Material Darker: the near-black variant of the Material Theme family -- deeper background, brighter and more saturated accents than plain Material.
pub(crate) const MATERIAL_DARKER: Theme = Theme {
    fg: rgb(0xEE, 0xFF, 0xFF),
    bg: rgb(0x21, 0x21, 0x21),
    cursor: rgb(0xFF, 0xFF, 0xFF),
    selection_bg: rgb(0x46, 0x49, 0x49),
    ansi16: [
        rgb(0x00, 0x00, 0x00), // 0  black
        rgb(0xFF, 0x53, 0x70), // 1  red
        rgb(0xC3, 0xE8, 0x8D), // 2  green
        rgb(0xFF, 0xCB, 0x6B), // 3  yellow
        rgb(0x82, 0xAA, 0xFF), // 4  blue
        rgb(0xC7, 0x92, 0xEA), // 5  magenta
        rgb(0x89, 0xDD, 0xFF), // 6  cyan
        rgb(0xFF, 0xFF, 0xFF), // 7  white
        rgb(0x54, 0x54, 0x54), // 8  bright black
        rgb(0xFF, 0x53, 0x70), // 9  bright red
        rgb(0xC3, 0xE8, 0x8D), // 10 bright green
        rgb(0xFF, 0xCB, 0x6B), // 11 bright yellow
        rgb(0x82, 0xAA, 0xFF), // 12 bright blue
        rgb(0xC7, 0x92, 0xEA), // 13 bright magenta
        rgb(0x89, 0xDD, 0xFF), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: Night Owl.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Night Owl: Sarah Drasner's low-glare theme tuned for late-night coding -- a deep blue-black background with a soft violet cursor accent.
pub(crate) const NIGHT_OWL: Theme = Theme {
    fg: rgb(0xD6, 0xDE, 0xEB),
    bg: rgb(0x01, 0x16, 0x27),
    cursor: rgb(0x7E, 0x57, 0xC2),
    selection_bg: rgb(0x27, 0x3A, 0x4A),
    ansi16: [
        rgb(0x01, 0x16, 0x27), // 0  black
        rgb(0xEF, 0x53, 0x50), // 1  red
        rgb(0x22, 0xDA, 0x6E), // 2  green
        rgb(0xAD, 0xDB, 0x67), // 3  yellow
        rgb(0x82, 0xAA, 0xFF), // 4  blue
        rgb(0xC7, 0x92, 0xEA), // 5  magenta
        rgb(0x21, 0xC7, 0xA8), // 6  cyan
        rgb(0xFF, 0xFF, 0xFF), // 7  white
        rgb(0x57, 0x56, 0x56), // 8  bright black
        rgb(0xEF, 0x53, 0x50), // 9  bright red
        rgb(0x22, 0xDA, 0x6E), // 10 bright green
        rgb(0xFF, 0xEB, 0x95), // 11 bright yellow
        rgb(0x82, 0xAA, 0xFF), // 12 bright blue
        rgb(0xC7, 0x92, 0xEA), // 13 bright magenta
        rgb(0x7F, 0xDB, 0xCA), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: Snazzy.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Snazzy: Hyper's punchy dark theme -- a near-black background with vivid, candy-bright accents in the Dracula/Nord tradition.
pub(crate) const SNAZZY: Theme = Theme {
    fg: rgb(0xEB, 0xEC, 0xE6),
    bg: rgb(0x1E, 0x1F, 0x29),
    cursor: rgb(0xE4, 0xE4, 0xE4),
    selection_bg: rgb(0x43, 0x44, 0x4B),
    ansi16: [
        rgb(0x00, 0x00, 0x00), // 0  black
        rgb(0xFC, 0x43, 0x46), // 1  red
        rgb(0x50, 0xFB, 0x7C), // 2  green
        rgb(0xF0, 0xFB, 0x8C), // 3  yellow
        rgb(0x49, 0xBA, 0xFF), // 4  blue
        rgb(0xFC, 0x4C, 0xB4), // 5  magenta
        rgb(0x8B, 0xE9, 0xFE), // 6  cyan
        rgb(0xED, 0xED, 0xEC), // 7  white
        rgb(0x55, 0x55, 0x55), // 8  bright black
        rgb(0xFC, 0x43, 0x46), // 9  bright red
        rgb(0x50, 0xFB, 0x7C), // 10 bright green
        rgb(0xF0, 0xFB, 0x8C), // 11 bright yellow
        rgb(0x49, 0xBA, 0xFF), // 12 bright blue
        rgb(0xFC, 0x4C, 0xB4), // 13 bright magenta
        rgb(0x8B, 0xE9, 0xFE), // 14 bright cyan
        rgb(0xED, 0xED, 0xEC), // 15 bright white
    ],
};

// Source: Horizon.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Horizon: a warm, sunset-toned dark theme -- coral, gold, and violet accents on a deep plum-black background.
pub(crate) const HORIZON_DARK: Theme = Theme {
    fg: rgb(0xD5, 0xD8, 0xDA),
    bg: rgb(0x1C, 0x1E, 0x26),
    cursor: rgb(0x6C, 0x6F, 0x93),
    selection_bg: rgb(0x3D, 0x3F, 0x46),
    ansi16: [
        rgb(0x00, 0x00, 0x00), // 0  black
        rgb(0xE9, 0x56, 0x78), // 1  red
        rgb(0x29, 0xD3, 0x98), // 2  green
        rgb(0xFA, 0xB7, 0x95), // 3  yellow
        rgb(0x26, 0xBB, 0xD9), // 4  blue
        rgb(0xEE, 0x64, 0xAC), // 5  magenta
        rgb(0x59, 0xE1, 0xE3), // 6  cyan
        rgb(0xE5, 0xE5, 0xE5), // 7  white
        rgb(0x66, 0x66, 0x66), // 8  bright black
        rgb(0xEC, 0x6A, 0x88), // 9  bright red
        rgb(0x3F, 0xDA, 0xA4), // 10 bright green
        rgb(0xFB, 0xC3, 0xA7), // 11 bright yellow
        rgb(0x3F, 0xC4, 0xDE), // 12 bright blue
        rgb(0xF0, 0x75, 0xB5), // 13 bright magenta
        rgb(0x6B, 0xE4, 0xE6), // 14 bright cyan
        rgb(0xE5, 0xE5, 0xE5), // 15 bright white
    ],
};

// Source: Oceanic Next.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Oceanic Next: a cool blue-gray dark theme -- a deep teal-black background with muted ocean-toned accents.
pub(crate) const OCEANIC_NEXT: Theme = Theme {
    fg: rgb(0xC0, 0xC5, 0xCE),
    bg: rgb(0x16, 0x2C, 0x35),
    cursor: rgb(0xC0, 0xC5, 0xCE),
    selection_bg: rgb(0x35, 0x48, 0x51),
    ansi16: [
        rgb(0x16, 0x2C, 0x35), // 0  black
        rgb(0xEC, 0x5F, 0x67), // 1  red
        rgb(0x99, 0xC7, 0x94), // 2  green
        rgb(0xFA, 0xC8, 0x63), // 3  yellow
        rgb(0x66, 0x99, 0xCC), // 4  blue
        rgb(0xC5, 0x94, 0xC5), // 5  magenta
        rgb(0x5F, 0xB3, 0xB3), // 6  cyan
        rgb(0xFF, 0xFF, 0xFF), // 7  white
        rgb(0x65, 0x73, 0x7E), // 8  bright black
        rgb(0xEC, 0x5F, 0x67), // 9  bright red
        rgb(0x99, 0xC7, 0x94), // 10 bright green
        rgb(0xFA, 0xC8, 0x63), // 11 bright yellow
        rgb(0x66, 0x99, 0xCC), // 12 bright blue
        rgb(0xC5, 0x94, 0xC5), // 13 bright magenta
        rgb(0x5F, 0xB3, 0xB3), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: alacritty/alacritty-theme palenight.toml, cursor + selection cross-checked against the original JonathanSpeek/palenight-iterm2 .itermcolors
/// Palenight: Jonathan Speek's iTerm2 port of the Material Theme's Palenight variant -- a muted slate-indigo background with the familiar Material accent set and a signature gold cursor.
pub(crate) const PALENIGHT: Theme = Theme {
    fg: rgb(0xD0, 0xD0, 0xD0),
    bg: rgb(0x29, 0x2D, 0x3E),
    cursor: rgb(0xFF, 0xCC, 0x00),
    selection_bg: rgb(0x60, 0x7D, 0x8B),
    ansi16: [
        rgb(0x29, 0x2D, 0x3E), // 0  black
        rgb(0xF0, 0x71, 0x78), // 1  red
        rgb(0xC3, 0xE8, 0x8D), // 2  green
        rgb(0xFF, 0xCB, 0x6B), // 3  yellow
        rgb(0x82, 0xAA, 0xFF), // 4  blue
        rgb(0xC7, 0x92, 0xEA), // 5  magenta
        rgb(0x89, 0xDD, 0xFF), // 6  cyan
        rgb(0xD0, 0xD0, 0xD0), // 7  white
        rgb(0x43, 0x47, 0x58), // 8  bright black
        rgb(0xFF, 0x8B, 0x92), // 9  bright red
        rgb(0xDD, 0xFF, 0xA7), // 10 bright green
        rgb(0xFF, 0xE5, 0x85), // 11 bright yellow
        rgb(0x9C, 0xC4, 0xFF), // 12 bright blue
        rgb(0xE1, 0xAC, 0xFF), // 13 bright magenta
        rgb(0xA3, 0xF7, 0xFF), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: Zenburn.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Zenburn: the venerable low-contrast theme -- a soft neutral-gray background with muted, easy-on-the-eyes pastel accents.
pub(crate) const ZENBURN: Theme = Theme {
    fg: rgb(0xDC, 0xDC, 0xCC),
    bg: rgb(0x3F, 0x3F, 0x3F),
    cursor: rgb(0x73, 0x63, 0x5A),
    selection_bg: rgb(0x5B, 0x5B, 0x58),
    ansi16: [
        rgb(0x4D, 0x4D, 0x4D), // 0  black
        rgb(0x7D, 0x5D, 0x5D), // 1  red
        rgb(0x60, 0xB4, 0x8A), // 2  green
        rgb(0xF0, 0xDF, 0xAF), // 3  yellow
        rgb(0x5D, 0x6D, 0x7D), // 4  blue
        rgb(0xDC, 0x8C, 0xC3), // 5  magenta
        rgb(0x8C, 0xD0, 0xD3), // 6  cyan
        rgb(0xDC, 0xDC, 0xCC), // 7  white
        rgb(0x70, 0x90, 0x80), // 8  bright black
        rgb(0xDC, 0xA3, 0xA3), // 9  bright red
        rgb(0xC3, 0xBF, 0x9F), // 10 bright green
        rgb(0xE0, 0xCF, 0x9F), // 11 bright yellow
        rgb(0x94, 0xBF, 0xF3), // 12 bright blue
        rgb(0xEC, 0x93, 0xD3), // 13 bright magenta
        rgb(0x93, 0xE0, 0xE3), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: Iceberg Dark.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Iceberg: a cool blue-gray dark theme -- a muted arctic palette with low-saturation, high-legibility accents (the dark variant).
pub(crate) const ICEBERG_DARK: Theme = Theme {
    fg: rgb(0xC6, 0xC8, 0xD1),
    bg: rgb(0x16, 0x18, 0x21),
    cursor: rgb(0xC6, 0xC8, 0xD1),
    selection_bg: rgb(0x36, 0x38, 0x41),
    ansi16: [
        rgb(0x1E, 0x21, 0x32), // 0  black
        rgb(0xE2, 0x78, 0x78), // 1  red
        rgb(0xB4, 0xBE, 0x82), // 2  green
        rgb(0xE2, 0xA4, 0x78), // 3  yellow
        rgb(0x84, 0xA0, 0xC6), // 4  blue
        rgb(0xA0, 0x93, 0xC7), // 5  magenta
        rgb(0x89, 0xB8, 0xC2), // 6  cyan
        rgb(0xC6, 0xC8, 0xD1), // 7  white
        rgb(0x6B, 0x70, 0x89), // 8  bright black
        rgb(0xE9, 0x89, 0x89), // 9  bright red
        rgb(0xC0, 0xCA, 0x8E), // 10 bright green
        rgb(0xE9, 0xB1, 0x89), // 11 bright yellow
        rgb(0x91, 0xAC, 0xD1), // 12 bright blue
        rgb(0xAD, 0xA0, 0xD3), // 13 bright magenta
        rgb(0x95, 0xC4, 0xCE), // 14 bright cyan
        rgb(0xD2, 0xD4, 0xDE), // 15 bright white
    ],
};

// Source: official EdenEast/nightfox.nvim kitty export (extra/nightfox/kitty.conf)
/// Nightfox: EdenEast's flagship dark theme -- a cool blue-black background with a muted, nature-toned accent family shared across the whole Nightfox family (see Dayfox above).
pub(crate) const NIGHTFOX: Theme = Theme {
    fg: rgb(0xCD, 0xCE, 0xCF),
    bg: rgb(0x19, 0x23, 0x30),
    cursor: rgb(0xCD, 0xCE, 0xCF),
    selection_bg: rgb(0x2B, 0x3B, 0x51),
    ansi16: [
        rgb(0x39, 0x3B, 0x44), // 0  black
        rgb(0xC9, 0x4F, 0x6D), // 1  red
        rgb(0x81, 0xB2, 0x9A), // 2  green
        rgb(0xDB, 0xC0, 0x74), // 3  yellow
        rgb(0x71, 0x9C, 0xD6), // 4  blue
        rgb(0x9D, 0x79, 0xD6), // 5  magenta
        rgb(0x63, 0xCD, 0xCF), // 6  cyan
        rgb(0xDF, 0xDF, 0xE0), // 7  white
        rgb(0x57, 0x58, 0x60), // 8  bright black
        rgb(0xD1, 0x69, 0x83), // 9  bright red
        rgb(0x8E, 0xBA, 0xA4), // 10 bright green
        rgb(0xE0, 0xC9, 0x89), // 11 bright yellow
        rgb(0x86, 0xAB, 0xDC), // 12 bright blue
        rgb(0xBA, 0xA1, 0xE2), // 13 bright magenta
        rgb(0x7A, 0xD5, 0xD6), // 14 bright cyan
        rgb(0xE4, 0xE4, 0xE5), // 15 bright white
    ],
};

// Source: official antfu/vscode-theme-vitesse theme JSON (themes/vitesse-dark.json terminal.ansi* keys)
/// Vitesse Dark: Anthony Fu's soft dark theme -- a near-black background with muted, slightly desaturated accents (normal and bright ANSI colors are identical by design, matching the theme's minimalist terminal port).
pub(crate) const VITESSE_DARK: Theme = Theme {
    fg: rgb(0xDB, 0xD7, 0xCA),
    bg: rgb(0x12, 0x12, 0x12),
    cursor: rgb(0xDB, 0xD7, 0xCA),
    selection_bg: rgb(0x27, 0x27, 0x27),
    ansi16: [
        rgb(0x39, 0x3A, 0x34), // 0  black
        rgb(0xCB, 0x76, 0x76), // 1  red
        rgb(0x4D, 0x93, 0x75), // 2  green
        rgb(0xE6, 0xCC, 0x77), // 3  yellow
        rgb(0x63, 0x94, 0xBF), // 4  blue
        rgb(0xD9, 0x73, 0x9F), // 5  magenta
        rgb(0x5E, 0xAA, 0xB5), // 6  cyan
        rgb(0xDB, 0xD7, 0xCA), // 7  white
        rgb(0x77, 0x77, 0x77), // 8  bright black
        rgb(0xCB, 0x76, 0x76), // 9  bright red
        rgb(0x4D, 0x93, 0x75), // 10 bright green
        rgb(0xE6, 0xCC, 0x77), // 11 bright yellow
        rgb(0x63, 0x94, 0xBF), // 12 bright blue
        rgb(0xD9, 0x73, 0x9F), // 13 bright magenta
        rgb(0x5E, 0xAA, 0xB5), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: Flexoki Dark.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Flexoki Dark: the dark sibling of Flexoki -- an ink-black background with the same muted, printer-ink-inspired accent family.
pub(crate) const FLEXOKI_DARK: Theme = Theme {
    fg: rgb(0xCE, 0xCD, 0xC3),
    bg: rgb(0x10, 0x0F, 0x0F),
    cursor: rgb(0xCE, 0xCD, 0xC3),
    selection_bg: rgb(0x32, 0x31, 0x2F),
    ansi16: [
        rgb(0x10, 0x0F, 0x0F), // 0  black
        rgb(0xD1, 0x4D, 0x41), // 1  red
        rgb(0x87, 0x9A, 0x39), // 2  green
        rgb(0xD0, 0xA2, 0x15), // 3  yellow
        rgb(0x43, 0x85, 0xBE), // 4  blue
        rgb(0xCE, 0x5D, 0x97), // 5  magenta
        rgb(0x3A, 0xA9, 0x9F), // 6  cyan
        rgb(0x87, 0x85, 0x80), // 7  white
        rgb(0x57, 0x56, 0x53), // 8  bright black
        rgb(0xAF, 0x30, 0x29), // 9  bright red
        rgb(0x66, 0x80, 0x0B), // 10 bright green
        rgb(0xAD, 0x83, 0x01), // 11 bright yellow
        rgb(0x20, 0x5E, 0xA6), // 12 bright blue
        rgb(0xA0, 0x2F, 0x6F), // 13 bright magenta
        rgb(0x24, 0x83, 0x7B), // 14 bright cyan
        rgb(0xCE, 0xCD, 0xC3), // 15 bright white
    ],
};

// Source: official Everblush/terminal-emulators kitty export (src/kitty/Everblush.conf)
/// Everblush: a dark, vibrant theme -- a deep teal-black background with soft coral/green/blue accents.
pub(crate) const EVERBLUSH: Theme = Theme {
    fg: rgb(0xDA, 0xDA, 0xDA),
    bg: rgb(0x14, 0x1B, 0x1E),
    cursor: rgb(0x2D, 0x34, 0x37),
    selection_bg: rgb(0x2D, 0x34, 0x37),
    ansi16: [
        rgb(0x23, 0x2A, 0x2D), // 0  black
        rgb(0xE5, 0x74, 0x74), // 1  red
        rgb(0x8C, 0xCF, 0x7E), // 2  green
        rgb(0xE5, 0xC7, 0x6B), // 3  yellow
        rgb(0x67, 0xB0, 0xE8), // 4  blue
        rgb(0xC4, 0x7F, 0xD5), // 5  magenta
        rgb(0x6C, 0xBF, 0xBF), // 6  cyan
        rgb(0xB3, 0xB9, 0xB8), // 7  white
        rgb(0x2D, 0x34, 0x37), // 8  bright black
        rgb(0xEF, 0x7E, 0x7E), // 9  bright red
        rgb(0x96, 0xD9, 0x88), // 10 bright green
        rgb(0xF4, 0xD6, 0x7A), // 11 bright yellow
        rgb(0x71, 0xBA, 0xF2), // 12 bright blue
        rgb(0xCE, 0x89, 0xDF), // 13 bright magenta
        rgb(0x67, 0xCB, 0xE7), // 14 bright cyan
        rgb(0xBD, 0xC3, 0xC2), // 15 bright white
    ],
};

// Source: official savq/melange-nvim kitty export (term/kitty/melange_dark.conf)
/// Melange Dark: Savitha's warm, earthy dark theme -- a warm near-black background with muted clay/sage/dusty-blue accents.
pub(crate) const MELANGE_DARK: Theme = Theme {
    fg: rgb(0xEC, 0xE1, 0xD7),
    bg: rgb(0x29, 0x25, 0x22),
    cursor: rgb(0xEC, 0xE1, 0xD7),
    selection_bg: rgb(0x40, 0x3A, 0x36),
    ansi16: [
        rgb(0x34, 0x30, 0x2C), // 0  black
        rgb(0xBD, 0x81, 0x83), // 1  red
        rgb(0x78, 0x99, 0x7A), // 2  green
        rgb(0xE4, 0x9B, 0x5D), // 3  yellow
        rgb(0x7F, 0x91, 0xB2), // 4  blue
        rgb(0xB3, 0x80, 0xB0), // 5  magenta
        rgb(0x7B, 0x96, 0x95), // 6  cyan
        rgb(0xC1, 0xA7, 0x8E), // 7  white
        rgb(0x86, 0x74, 0x62), // 8  bright black
        rgb(0xD4, 0x77, 0x66), // 9  bright red
        rgb(0x85, 0xB6, 0x95), // 10 bright green
        rgb(0xEB, 0xC0, 0x6D), // 11 bright yellow
        rgb(0xA3, 0xA9, 0xCE), // 12 bright blue
        rgb(0xCF, 0x9B, 0xC2), // 13 bright magenta
        rgb(0x89, 0xB3, 0xB6), // 14 bright cyan
        rgb(0xEC, 0xE1, 0xD7), // 15 bright white
    ],
};

// Source: alacritty/alacritty-theme synthwave_84.toml, cross-verified against official robb0wen/synthwave-vscode terminal.* keys
/// SynthWave '84: Robb Owen's retro-futuristic neon theme -- a deep purple background with glowing cyan/magenta/yellow accents.
pub(crate) const SYNTHWAVE_84: Theme = Theme {
    fg: rgb(0xFF, 0xFF, 0xFF),
    bg: rgb(0x26, 0x23, 0x35),
    cursor: rgb(0x03, 0xED, 0xF9),
    selection_bg: rgb(0x41, 0x3F, 0x4E),
    ansi16: [
        rgb(0x26, 0x23, 0x35), // 0  black
        rgb(0xFE, 0x44, 0x50), // 1  red
        rgb(0x72, 0xF1, 0xB8), // 2  green
        rgb(0xF3, 0xE7, 0x0F), // 3  yellow
        rgb(0x03, 0xED, 0xF9), // 4  blue
        rgb(0xFF, 0x7E, 0xDB), // 5  magenta
        rgb(0x03, 0xED, 0xF9), // 6  cyan
        rgb(0xFF, 0xFF, 0xFF), // 7  white
        rgb(0x61, 0x4D, 0x85), // 8  bright black
        rgb(0xFE, 0x44, 0x50), // 9  bright red
        rgb(0x72, 0xF1, 0xB8), // 10 bright green
        rgb(0xFE, 0xDE, 0x5D), // 11 bright yellow
        rgb(0x03, 0xED, 0xF9), // 12 bright blue
        rgb(0xFF, 0x7E, 0xDB), // 13 bright magenta
        rgb(0x03, 0xED, 0xF9), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: official catppuccin/kitty kitty export (themes/frappe.conf)
/// Catppuccin Frappe: the medium-contrast dark member of the Catppuccin family -- warmer and softer than Mocha. Special entries use Text (fg), Base (bg) and Rosewater (cursor); selection uses Surface1 (rather than the official kitty port's Rosewater selection, which would put light text on a light highlight -- see CATPPUCCIN_MOCHA's selection for the same convention).
pub(crate) const CATPPUCCIN_FRAPPE: Theme = Theme {
    fg: rgb(0xC6, 0xD0, 0xF5),
    bg: rgb(0x30, 0x34, 0x46),
    cursor: rgb(0xF2, 0xD5, 0xCF),
    selection_bg: rgb(0x51, 0x57, 0x6D),
    ansi16: [
        rgb(0x51, 0x57, 0x6D), // 0  black
        rgb(0xE7, 0x82, 0x84), // 1  red
        rgb(0xA6, 0xD1, 0x89), // 2  green
        rgb(0xE5, 0xC8, 0x90), // 3  yellow
        rgb(0x8C, 0xAA, 0xEE), // 4  blue
        rgb(0xF4, 0xB8, 0xE4), // 5  magenta
        rgb(0x81, 0xC8, 0xBE), // 6  cyan
        rgb(0xB5, 0xBF, 0xE2), // 7  white
        rgb(0x62, 0x68, 0x80), // 8  bright black
        rgb(0xE7, 0x82, 0x84), // 9  bright red
        rgb(0xA6, 0xD1, 0x89), // 10 bright green
        rgb(0xE5, 0xC8, 0x90), // 11 bright yellow
        rgb(0x8C, 0xAA, 0xEE), // 12 bright blue
        rgb(0xF4, 0xB8, 0xE4), // 13 bright magenta
        rgb(0x81, 0xC8, 0xBE), // 14 bright cyan
        rgb(0xA5, 0xAD, 0xCE), // 15 bright white
    ],
};

// Source: official folke/tokyonight.nvim kitty export (extras/kitty/tokyonight_storm.conf)
/// Tokyo Night Storm: the softer, slightly-lighter-background sibling of Tokyo Night -- same accent family, a lifted navy background instead of near-black.
pub(crate) const TOKYO_NIGHT_STORM: Theme = Theme {
    fg: rgb(0xC0, 0xCA, 0xF5),
    bg: rgb(0x24, 0x28, 0x3B),
    cursor: rgb(0xC0, 0xCA, 0xF5),
    selection_bg: rgb(0x2E, 0x3C, 0x64),
    ansi16: [
        rgb(0x1D, 0x20, 0x2F), // 0  black
        rgb(0xF7, 0x76, 0x8E), // 1  red
        rgb(0x9E, 0xCE, 0x6A), // 2  green
        rgb(0xE0, 0xAF, 0x68), // 3  yellow
        rgb(0x7A, 0xA2, 0xF7), // 4  blue
        rgb(0xBB, 0x9A, 0xF7), // 5  magenta
        rgb(0x7D, 0xCF, 0xFF), // 6  cyan
        rgb(0xA9, 0xB1, 0xD6), // 7  white
        rgb(0x41, 0x48, 0x68), // 8  bright black
        rgb(0xFF, 0x89, 0x9D), // 9  bright red
        rgb(0x9F, 0xE0, 0x44), // 10 bright green
        rgb(0xFA, 0xBA, 0x4A), // 11 bright yellow
        rgb(0x8D, 0xB0, 0xFF), // 12 bright blue
        rgb(0xC7, 0xA9, 0xFF), // 13 bright magenta
        rgb(0xA4, 0xDA, 0xFF), // 14 bright cyan
        rgb(0xC0, 0xCA, 0xF5), // 15 bright white
    ],
};

// Source: Gruvbox Material Dark.conf (mbadolato/iTerm2-Color-Schemes kitty export); selection is bg3 from sainnhe/gruvbox-material's published medium-contrast background ramp
/// Gruvbox Material: sainnhe's softened, lower-contrast take on Gruvbox -- same warm retro-earthy family with gentler saturation.
pub(crate) const GRUVBOX_MATERIAL: Theme = Theme {
    fg: rgb(0xD4, 0xBE, 0x98),
    bg: rgb(0x28, 0x28, 0x28),
    cursor: rgb(0xD4, 0xBE, 0x98),
    selection_bg: rgb(0x45, 0x40, 0x3D),
    ansi16: [
        rgb(0x28, 0x28, 0x28), // 0  black
        rgb(0xEA, 0x69, 0x62), // 1  red
        rgb(0xA9, 0xB6, 0x65), // 2  green
        rgb(0xD8, 0xA6, 0x57), // 3  yellow
        rgb(0x7D, 0xAE, 0xA3), // 4  blue
        rgb(0xD3, 0x86, 0x9B), // 5  magenta
        rgb(0x89, 0xB4, 0x82), // 6  cyan
        rgb(0xD4, 0xBE, 0x98), // 7  white
        rgb(0x7C, 0x6F, 0x64), // 8  bright black
        rgb(0xEA, 0x69, 0x62), // 9  bright red
        rgb(0xA9, 0xB6, 0x65), // 10 bright green
        rgb(0xD8, 0xA6, 0x57), // 11 bright yellow
        rgb(0x7D, 0xAE, 0xA3), // 12 bright blue
        rgb(0xD3, 0x86, 0x9B), // 13 bright magenta
        rgb(0x89, 0xB4, 0x82), // 14 bright cyan
        rgb(0xDD, 0xC7, 0xA1), // 15 bright white
    ],
};

// Source: One Half Dark.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// One Half Dark: sonph's balanced dark theme -- a cool slate background with a restrained, evenly-weighted accent set (the dark sibling of One Half Light).
pub(crate) const ONE_HALF_DARK: Theme = Theme {
    fg: rgb(0xDC, 0xDF, 0xE4),
    bg: rgb(0x28, 0x2C, 0x34),
    cursor: rgb(0xA3, 0xB3, 0xCC),
    selection_bg: rgb(0x48, 0x4C, 0x54),
    ansi16: [
        rgb(0x28, 0x2C, 0x34), // 0  black
        rgb(0xE0, 0x6C, 0x75), // 1  red
        rgb(0x98, 0xC3, 0x79), // 2  green
        rgb(0xE5, 0xC0, 0x7B), // 3  yellow
        rgb(0x61, 0xAF, 0xEF), // 4  blue
        rgb(0xC6, 0x78, 0xDD), // 5  magenta
        rgb(0x56, 0xB6, 0xC2), // 6  cyan
        rgb(0xDC, 0xDF, 0xE4), // 7  white
        rgb(0x5D, 0x67, 0x7A), // 8  bright black
        rgb(0xE0, 0x6C, 0x75), // 9  bright red
        rgb(0x98, 0xC3, 0x79), // 10 bright green
        rgb(0xE5, 0xC0, 0x7B), // 11 bright yellow
        rgb(0x61, 0xAF, 0xEF), // 12 bright blue
        rgb(0xC6, 0x78, 0xDD), // 13 bright magenta
        rgb(0x56, 0xB6, 0xC2), // 14 bright cyan
        rgb(0xDC, 0xDF, 0xE4), // 15 bright white
    ],
};

// Source: Ayu Mirage.conf (mbadolato/iTerm2-Color-Schemes kitty export)
/// Ayu Mirage: the medium-contrast middle sibling of the Ayu family -- softer than Ayu Dark's near-black, with the same warm amber cursor accent.
pub(crate) const AYU_MIRAGE: Theme = Theme {
    fg: rgb(0xCC, 0xCA, 0xC2),
    bg: rgb(0x1F, 0x24, 0x30),
    cursor: rgb(0xFF, 0xCC, 0x66),
    selection_bg: rgb(0x3E, 0x42, 0x4A),
    ansi16: [
        rgb(0x17, 0x1B, 0x24), // 0  black
        rgb(0xED, 0x82, 0x74), // 1  red
        rgb(0x87, 0xD9, 0x6C), // 2  green
        rgb(0xFA, 0xCC, 0x6E), // 3  yellow
        rgb(0x6D, 0xCB, 0xFA), // 4  blue
        rgb(0xDA, 0xBA, 0xFA), // 5  magenta
        rgb(0x90, 0xE1, 0xC6), // 6  cyan
        rgb(0xC7, 0xC7, 0xC7), // 7  white
        rgb(0x68, 0x68, 0x68), // 8  bright black
        rgb(0xF2, 0x87, 0x79), // 9  bright red
        rgb(0xD5, 0xFF, 0x80), // 10 bright green
        rgb(0xFF, 0xD1, 0x73), // 11 bright yellow
        rgb(0x73, 0xD0, 0xFF), // 12 bright blue
        rgb(0xDF, 0xBF, 0xFF), // 13 bright magenta
        rgb(0x95, 0xE6, 0xCB), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: official rose-pine/kitty kitty export (dist/rose-pine-moon.conf)
/// Rose Pine Moon: the medium-contrast middle sibling of Rose Pine -- a lifted midnight-blue background between Rose Pine's near-black and Dawn's off-white.
pub(crate) const ROSE_PINE_MOON: Theme = Theme {
    fg: rgb(0xE0, 0xDE, 0xF4),
    bg: rgb(0x23, 0x21, 0x36),
    cursor: rgb(0x56, 0x52, 0x6E),
    selection_bg: rgb(0x44, 0x41, 0x5A),
    ansi16: [
        rgb(0x39, 0x35, 0x52), // 0  black
        rgb(0xEB, 0x6F, 0x92), // 1  red
        rgb(0x3E, 0x8F, 0xB0), // 2  green
        rgb(0xF6, 0xC1, 0x77), // 3  yellow
        rgb(0x9C, 0xCF, 0xD8), // 4  blue
        rgb(0xC4, 0xA7, 0xE7), // 5  magenta
        rgb(0xEA, 0x9A, 0x97), // 6  cyan
        rgb(0xE0, 0xDE, 0xF4), // 7  white
        rgb(0x6E, 0x6A, 0x86), // 8  bright black
        rgb(0xEB, 0x6F, 0x92), // 9  bright red
        rgb(0x3E, 0x8F, 0xB0), // 10 bright green
        rgb(0xF6, 0xC1, 0x77), // 11 bright yellow
        rgb(0x9C, 0xCF, 0xD8), // 12 bright blue
        rgb(0xC4, 0xA7, 0xE7), // 13 bright magenta
        rgb(0xEA, 0x9A, 0x97), // 14 bright cyan
        rgb(0xE0, 0xDE, 0xF4), // 15 bright white
    ],
};

// Source: official rebelot/kanagawa.nvim kitty export (extras/kitty/kanagawa_dragon.conf); selection matches the same winterBlue token already used by the shipped KANAGAWA (Wave) entry
/// Kanagawa Dragon: the darker, higher-contrast sibling of Kanagawa Wave -- a warm near-black background with the same ink-wash accent family.
pub(crate) const KANAGAWA_DRAGON: Theme = Theme {
    fg: rgb(0xC5, 0xC9, 0xC5),
    bg: rgb(0x18, 0x16, 0x16),
    cursor: rgb(0xC8, 0xC0, 0x93),
    selection_bg: rgb(0x2D, 0x4F, 0x67),
    ansi16: [
        rgb(0x0D, 0x0C, 0x0C), // 0  black
        rgb(0xC4, 0x74, 0x6E), // 1  red
        rgb(0x8A, 0x9A, 0x7B), // 2  green
        rgb(0xC4, 0xB2, 0x8A), // 3  yellow
        rgb(0x8B, 0xA4, 0xB0), // 4  blue
        rgb(0xA2, 0x92, 0xA3), // 5  magenta
        rgb(0x8E, 0xA4, 0xA2), // 6  cyan
        rgb(0xC8, 0xC0, 0x93), // 7  white
        rgb(0xA6, 0xA6, 0x9C), // 8  bright black
        rgb(0xE4, 0x68, 0x76), // 9  bright red
        rgb(0x87, 0xA9, 0x87), // 10 bright green
        rgb(0xE6, 0xC3, 0x84), // 11 bright yellow
        rgb(0x7F, 0xB4, 0xCA), // 12 bright blue
        rgb(0x93, 0x8A, 0xA9), // 13 bright magenta
        rgb(0x7A, 0xA8, 0x9F), // 14 bright cyan
        rgb(0xC5, 0xC9, 0xC5), // 15 bright white
    ],
};

// Source: computed from official craftzdog/solarized-osaka.nvim Lua HSL palette + theme.lua's vim.g.terminal_color_* mapping. NOTE: this corrects mbadolato/iTerm2-Color-Schemes' 'Solarized Osaka Night.conf', which was cross-checked here and found to just duplicate Tokyo Night Night's hex values rather than Solarized Osaka's actual (much darker, teal-black) palette.
/// Solarized Osaka: craftzdog's Tokyo-Night-structured hybrid -- Solarized's precision accent hues (recomputed from the theme's own HSL definitions) on a near-black teal background instead of Solarized's usual base03.
pub(crate) const SOLARIZED_OSAKA: Theme = Theme {
    fg: rgb(0x83, 0x94, 0x95),
    bg: rgb(0x00, 0x14, 0x1A),
    cursor: rgb(0x83, 0x94, 0x95),
    selection_bg: rgb(0x00, 0x2D, 0x38),
    ansi16: [
        rgb(0x00, 0x10, 0x15), // 0  black
        rgb(0xDC, 0x31, 0x2E), // 1  red
        rgb(0x85, 0x99, 0x00), // 2  green
        rgb(0xB2, 0x86, 0x00), // 3  yellow
        rgb(0x27, 0x8B, 0xD3), // 4  blue
        rgb(0xD3, 0x36, 0x82), // 5  magenta
        rgb(0x2A, 0xA2, 0x98), // 6  cyan
        rgb(0x83, 0x94, 0x95), // 7  white
        rgb(0x00, 0x10, 0x15), // 8  bright black
        rgb(0xDC, 0x31, 0x2E), // 9  bright red
        rgb(0x85, 0x99, 0x00), // 10 bright green
        rgb(0xB2, 0x86, 0x00), // 11 bright yellow
        rgb(0x27, 0x8B, 0xD3), // 12 bright blue
        rgb(0xD3, 0x36, 0x82), // 13 bright magenta
        rgb(0x2A, 0xA2, 0x98), // 14 bright cyan
        rgb(0x83, 0x94, 0x95), // 15 bright white
    ],
};

// Source: official olivercederborg/poimandres.nvim WezTerm export (extra/wezterm/poimandres.toml). NOTE: this corrects mbadolato/iTerm2-Color-Schemes' 'Poimandres.conf', which was cross-checked here and found to carry an incorrect foreground (#a6accd instead of the official #E4F0FB).
/// Poimandres: Oliver Cederborg's cool, teal-accented dark theme -- a deep blue-black background with soft pink/teal/cream accents.
pub(crate) const POIMANDRES: Theme = Theme {
    fg: rgb(0xE4, 0xF0, 0xFB),
    bg: rgb(0x1B, 0x1E, 0x28),
    cursor: rgb(0xA6, 0xAC, 0xCD),
    selection_bg: rgb(0x50, 0x64, 0x77),
    ansi16: [
        rgb(0x17, 0x19, 0x22), // 0  black
        rgb(0xD0, 0x67, 0x9D), // 1  red
        rgb(0x5D, 0xE4, 0xC7), // 2  green
        rgb(0xFF, 0xFA, 0xC2), // 3  yellow
        rgb(0x89, 0xDD, 0xFF), // 4  blue
        rgb(0xFC, 0xC5, 0xE9), // 5  magenta
        rgb(0x89, 0xDD, 0xFF), // 6  cyan
        rgb(0xFF, 0xFF, 0xFF), // 7  white
        rgb(0x50, 0x64, 0x77), // 8  bright black
        rgb(0xD0, 0x67, 0x9D), // 9  bright red
        rgb(0x5D, 0xE4, 0xC7), // 10 bright green
        rgb(0xFF, 0xFA, 0xC2), // 11 bright yellow
        rgb(0xAD, 0xD7, 0xFF), // 12 bright blue
        rgb(0xFC, 0xC5, 0xE9), // 13 bright magenta
        rgb(0xAD, 0xD7, 0xFF), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: official EliverLara/Andromeda VS Code theme JSON (themes/Andromeda-color-theme.json terminal.ansi*/selection keys; color8 uses the theme's published comment-gray #A0A1A7). NOTE: this corrects mbadolato/iTerm2-Color-Schemes' 'Andromeda.conf', which was cross-checked here and found to carry generic default terminal colors rather than Andromeda's actual palette.
/// Andromeda: EliverLara's vivid dark theme -- a cool charcoal background with a distinctive hot-pink/cyan/gold accent set and a signature yellow cursor.
pub(crate) const ANDROMEDA: Theme = Theme {
    fg: rgb(0xD5, 0xCE, 0xD9),
    bg: rgb(0x23, 0x26, 0x2E),
    cursor: rgb(0xFF, 0xE6, 0x6D),
    selection_bg: rgb(0x3D, 0x43, 0x52),
    ansi16: [
        rgb(0x00, 0x00, 0x00), // 0  black
        rgb(0xEE, 0x5D, 0x43), // 1  red
        rgb(0x96, 0xE0, 0x72), // 2  green
        rgb(0xFF, 0xE6, 0x6D), // 3  yellow
        rgb(0x7C, 0xB7, 0xFF), // 4  blue
        rgb(0xFF, 0x00, 0xAA), // 5  magenta
        rgb(0x00, 0xE8, 0xC6), // 6  cyan
        rgb(0xD5, 0xCE, 0xD9), // 7  white
        rgb(0xA0, 0xA1, 0xA7), // 8  bright black
        rgb(0xEE, 0x5D, 0x43), // 9  bright red
        rgb(0x96, 0xE0, 0x72), // 10 bright green
        rgb(0xFF, 0xE6, 0x6D), // 11 bright yellow
        rgb(0x7C, 0xB7, 0xFF), // 12 bright blue
        rgb(0xFF, 0x00, 0xAA), // 13 bright magenta
        rgb(0x00, 0xE8, 0xC6), // 14 bright cyan
        rgb(0xFF, 0xFF, 0xFF), // 15 bright white
    ],
};

// Source: Aura Dark.conf (mbadolato/iTerm2-Color-Schemes kitty export), cross-verified against daltonmenezes/aura-theme's official Konsole export (packages/konsole/aura-theme.colorscheme)
/// Aura: Dalton Menezes' vivid dark theme -- a deep violet-black background with a signature purple/cyan/coral accent trio.
pub(crate) const AURA: Theme = Theme {
    fg: rgb(0xCD, 0xCC, 0xCE),
    bg: rgb(0x15, 0x14, 0x1B),
    cursor: rgb(0xA2, 0x77, 0xFF),
    selection_bg: rgb(0x36, 0x35, 0x3B),
    ansi16: [
        rgb(0x15, 0x14, 0x1B), // 0  black
        rgb(0xFF, 0x67, 0x67), // 1  red
        rgb(0x61, 0xFF, 0xCA), // 2  green
        rgb(0xFF, 0xCA, 0x85), // 3  yellow
        rgb(0xA2, 0x77, 0xFF), // 4  blue
        rgb(0x61, 0xFF, 0xCA), // 5  magenta
        rgb(0xA2, 0x77, 0xFF), // 6  cyan
        rgb(0xCD, 0xCC, 0xCE), // 7  white
        rgb(0x46, 0x46, 0x46), // 8  bright black
        rgb(0xFF, 0xCA, 0x85), // 9  bright red
        rgb(0xA2, 0x77, 0xFF), // 10 bright green
        rgb(0xFF, 0xCA, 0x85), // 11 bright yellow
        rgb(0xA2, 0x77, 0xFF), // 12 bright blue
        rgb(0x61, 0xFF, 0xCA), // 13 bright magenta
        rgb(0x61, 0xFF, 0xCA), // 14 bright cyan
        rgb(0xED, 0xEC, 0xEE), // 15 bright white
    ],
};

// Source: official challenger-deep-theme/kitty repo (challenger-deep.conf). NOTE: this corrects mbadolato/iTerm2-Color-Schemes' 'Challenger Deep.conf', which was cross-checked here and found to have its normal/bright ANSI rows swapped relative to the official file; selection uses the official color0 instead of the official file's near-white #fbfcfc (which would sit fg-on-bright and be unreadable under this renderer's selection model).
/// Challenger Deep: a deep-sea-toned dark theme -- a dark indigo background with soft pastel accents.
pub(crate) const CHALLENGER_DEEP: Theme = Theme {
    fg: rgb(0xCB, 0xE3, 0xE7),
    bg: rgb(0x1B, 0x18, 0x2C),
    cursor: rgb(0x91, 0xDD, 0xFF),
    selection_bg: rgb(0x56, 0x55, 0x75),
    ansi16: [
        rgb(0x56, 0x55, 0x75), // 0  black
        rgb(0xFF, 0x80, 0x80), // 1  red
        rgb(0x95, 0xFF, 0xA4), // 2  green
        rgb(0xFF, 0xE9, 0xAA), // 3  yellow
        rgb(0x91, 0xDD, 0xFF), // 4  blue
        rgb(0xC9, 0x91, 0xE1), // 5  magenta
        rgb(0xAA, 0xFF, 0xE4), // 6  cyan
        rgb(0xCB, 0xE3, 0xE7), // 7  white
        rgb(0xA6, 0xB3, 0xCC), // 8  bright black
        rgb(0xFF, 0x54, 0x58), // 9  bright red
        rgb(0x62, 0xD1, 0x96), // 10 bright green
        rgb(0xFF, 0xB3, 0x78), // 11 bright yellow
        rgb(0x65, 0xB2, 0xFF), // 12 bright blue
        rgb(0x90, 0x6C, 0xFF), // 13 bright magenta
        rgb(0x63, 0xF2, 0xF1), // 14 bright cyan
        rgb(0xA6, 0xB3, 0xCC), // 15 bright white
    ],
};
