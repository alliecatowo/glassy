//! The text-vs-emoji PRESENTATION policy: the single authoritative answer to
//! "should this codepoint ever render as color art, and is a shaped result
//! that came back color art actually legitimate to use."
//!
//! # The symptom this exists to prevent
//!
//! A single glyph (e.g. ⚙ U+2699, a Claude Code status-line icon, or any
//! plain BMP symbol) could render as a flat monochrome glyph on one frame and
//! a full-color emoji bitmap on the next — the SAME codepoint, no user input,
//! no config change. Two different symptoms share this one root cause:
//! chrome icons drawn every frame the SAME way (so they land on one of the two
//! outcomes and stay there — inconsistent but static) look merely "wrong,"
//! while PTY content shaped via the ligature-run path (whose *input* changes
//! every frame — a ticking timer next to the icon) can flip outcomes
//! frame-to-frame, which reads as flicker.
//!
//! # Root cause
//!
//! glassy has three glyph-shaping paths (single-character, grapheme-cluster,
//! and ligature-run), and only the single-character path did careful,
//! deliberate resolution: route by `font_symbol_map`, and fall back to
//! CoreText's system cascade only for genuine `.notdef` tofu. The other two
//! paths had no such care:
//!
//! - The **cluster path** (`Text::rasterize_cluster`) is fine by
//!   construction: it only takes over for ZWJ sequences, which are
//!   inherently compound emoji. Nothing to fix there.
//! - The **run path** (`Text::rasterize_run`), used for OpenType ligature
//!   substitution (`->` → `→`), shapes a whole run of characters through the
//!   PRIMARY font family only, with no per-codepoint routing. If a character
//!   swept into a run isn't actually covered by the primary font,
//!   cosmic-text's own internal fallback cascade resolves it instead — and on
//!   macOS that built-in cascade (`.SF NS`, `Menlo`, **Apple Color Emoji**,
//!   Geneva, Arial Unicode MS) tries Apple Color Emoji ahead of any plain
//!   symbol font. Apple Color Emoji carries color art for far more codepoints
//!   than Unicode's `Emoji_Presentation=Yes` set (it backs the emoji
//!   keyboard, which lets a user force-color anything `Emoji=Yes`, not just
//!   default-emoji ones) — so a default-TEXT symbol like ⚙ or ⌘ can resolve
//!   to color there even though the single-character path would have kept it
//!   flat.
//!
//! # The fix, two complementary halves
//!
//! 1. **Coverage, not just "isn't tofu."** Ligature runs are, by
//!    construction, same-font glyph substitutions: every character in a
//!    legitimate ligature is already covered by the primary font's own GSUB
//!    table. So run-eligibility (`App::liga_eligible` in `src/app/render.rs`)
//!    now requires the primary font to *actually have a glyph* for a
//!    character (`Renderer::primary_font_covers`, a real font-coverage check,
//!    not merely "the shaper didn't return notdef") before letting it into a
//!    run. Any character the primary font doesn't cover always takes the
//!    single-character path instead — the one path with controlled,
//!    deliberate resolution. This makes a given codepoint's resolution
//!    independent of whatever text happens to be shaping around it that
//!    frame, eliminating the flicker symptom by construction rather than by
//!    coincidence.
//! 2. **A real presentation gate on the single-character path itself.** Even
//!    with (1), the single-character path's own `.notdef` → CoreText fallback
//!    used to accept whatever CoreText's cascade handed back, color or not,
//!    with no check against Unicode's presentation default. [`glyph_acceptable`]
//!    is that check: a color result is legitimate only when the source text
//!    explicitly asks for it (a trailing `U+FE0F` variation selector) or the
//!    codepoint's own default presentation is emoji ([`is_default_emoji_presentation`]).
//!    Everything else must render flat — or blank, if no font in the chain
//!    offers a non-color glyph — matching how kitty/alacritty/ghostty treat
//!    the same codepoints.
//!
//! Both halves are exposed from this one module so "what counts as
//! legitimately emoji" is answered in exactly one place, not re-decided ad
//! hoc at each call site.

/// Whether `ch`'s default Unicode presentation is EMOJI (color art), per UTR51
/// `Emoji_Presentation=Yes`. Bounded, hand-maintained ranges rather than a full
/// ICU/Unicode-data dependency: the legacy BMP symbols grandfathered as
/// default-emoji before variation selectors existed (☀⌚⛄…), flag regional
/// indicators, and the modern pictograph super-blocks (🦀🎉😀…). NOT
/// exhaustive to the newest Unicode release, but that only means a handful of
/// brand-new pictographs render flat until this list is updated — the safe
/// direction to be wrong in, since the alternative (treating unknown symbols as
/// emoji-eligible) is what causes glyphs like ⚙ ⌘ ⓘ ✕ to intermittently flash
/// color. Everything NOT in this table defaults to TEXT presentation.
pub(crate) fn is_default_emoji_presentation(ch: char) -> bool {
    let cp = ch as u32;
    matches!(cp,
        // Legacy BMP symbols grandfathered as default-emoji.
        0x231A..=0x231B
            | 0x23E9..=0x23EC
            | 0x23F0
            | 0x23F3
            | 0x25FD..=0x25FE
            | 0x2614..=0x2615
            | 0x2648..=0x2653
            | 0x267F
            | 0x2693
            | 0x26A1
            | 0x26AA..=0x26AB
            | 0x26BD..=0x26BE
            | 0x26C4..=0x26C5
            | 0x26CE
            | 0x26D4
            | 0x26EA
            | 0x26F2..=0x26F3
            | 0x26F5
            | 0x26FA
            | 0x26FD
            | 0x2705
            | 0x270A..=0x270B
            | 0x2728
            | 0x274C
            | 0x274E
            | 0x2753..=0x2755
            | 0x2757
            | 0x2795..=0x2797
            | 0x27B0
            | 0x27BF
            | 0x2B1B..=0x2B1C
            | 0x2B50
            | 0x2B55
            // Flags (regional indicator pairs) — always emoji presentation.
            | 0x1F1E6..=0x1F1FF
            // Legacy pictographs (mahjong tile, playing card).
            | 0x1F004
            | 0x1F0CF
            // Modern emoji super-blocks: emoticons, misc symbols & pictographs,
            // transport & map, supplemental symbols, extended pictographs.
            | 0x1F300..=0x1F5FF
            | 0x1F600..=0x1F64F
            | 0x1F680..=0x1F6FF
            | 0x1F7E0..=0x1F7EB
            | 0x1F90C..=0x1F9FF
            | 0x1FA70..=0x1FAFF
    )
}

/// Whether a color (emoji) glyph is a legitimate rendering for `text`, vs. an
/// unsanctioned font-fallback accident.
///
/// Color art is allowed only when the source explicitly asks for it (a
/// trailing VS16 `U+FE0F`) or the leading codepoint's own default presentation
/// is emoji ([`is_default_emoji_presentation`]). Everything else must render
/// flat even if some font in the fallback cascade happens to carry color art
/// for it — e.g. Apple Color Emoji covers far more codepoints than
/// `Emoji_Presentation=Yes`, which is how a plain BMP symbol like ⚙ or ⌘ ends
/// up flickering between a flat glyph and a color one depending on which font
/// in the cascade a given shaping call happens to land on.
pub(crate) fn color_presentation_allowed(text: &str) -> bool {
    if text.contains('\u{FE0F}') {
        return true;
    }
    text.chars()
        .next()
        .is_some_and(is_default_emoji_presentation)
}

/// Whether a shaped result is acceptable to use at all: a non-color (mask)
/// glyph is always fine — presentation only constrains color art. Convenience
/// wrapper over [`color_presentation_allowed`] for call sites that already
/// have a `SwashContent`/CoreText color flag in hand.
pub(crate) fn glyph_acceptable(text: &str, is_color: bool) -> bool {
    !is_color || color_presentation_allowed(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_symbols_default_to_text_presentation() {
        // Emoji=Yes but Emoji_Presentation=No: must never be treated as emoji-default.
        for ch in ['⚙', '⌘', 'ⓘ', '✕', '☺', '✈', '✉'] {
            assert!(
                !is_default_emoji_presentation(ch),
                "{ch:?} should default to text presentation"
            );
        }
    }

    #[test]
    fn modern_pictographs_default_to_emoji_presentation() {
        for ch in ['🦀', '🎉', '😀', '🚀', '🧠'] {
            assert!(
                is_default_emoji_presentation(ch),
                "{ch:?} should default to emoji presentation"
            );
        }
    }

    #[test]
    fn legacy_bmp_symbols_default_to_emoji_presentation() {
        for ch in ['⌚', '⌛', '☔', '⚡', '⛄'] {
            assert!(
                is_default_emoji_presentation(ch),
                "{ch:?} is a grandfathered legacy default-emoji symbol"
            );
        }
    }

    #[test]
    fn plain_ascii_and_box_drawing_are_never_emoji_presentation() {
        for ch in ['A', '1', ' ', '─', '█'] {
            assert!(!is_default_emoji_presentation(ch));
        }
    }

    #[test]
    fn color_allowed_for_default_emoji_codepoint_alone() {
        assert!(color_presentation_allowed("🦀"));
    }

    #[test]
    fn color_allowed_when_explicit_vs16_present() {
        // ⚙ defaults to text, but an explicit VS16 opts into color.
        assert!(color_presentation_allowed("\u{2699}\u{FE0F}"));
    }

    #[test]
    fn color_rejected_for_default_text_codepoint_without_vs16() {
        assert!(!color_presentation_allowed("\u{2699}"));
        assert!(!color_presentation_allowed("\u{2318}"));
        assert!(!color_presentation_allowed("\u{24D8}"));
    }

    #[test]
    fn glyph_acceptable_matches_color_presentation_allowed() {
        assert!(glyph_acceptable("\u{2699}", false));
        assert!(!glyph_acceptable("\u{2699}", true));
        assert!(glyph_acceptable("🦀", true));
    }
}
