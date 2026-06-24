//! Font loading, cell metrics, and on-demand glyph rasterization.
//!
//! This module is deliberately free of any GPU/windowing dependency: it shapes
//! single characters with `cosmic-text` and rasterizes them to RGBA8 bitmaps via
//! the bundled `swash` cache. The renderer uploads the resulting bitmaps into a
//! glyph atlas; everything here is pure CPU work and is fully cached per
//! `(char, bold, italic)` so repeated cells are a cheap `HashMap` lookup.
//!
//! Sub-modules:
//! - `discover` — font discovery: fc-match cache, candidate producers, fallback loading
//! - `shape`    — `Text` struct, shaping pipeline, rasterization

mod discover;
mod shape;

pub use shape::{CellMetrics, RasterizedGlyph, RunGlyph, Text};

#[cfg(test)]
mod tests {
    use super::*;

    /// `rasterize_run` must return exactly one `RunGlyph` per input scalar,
    /// each with `advance_cells >= 1`. This is a pure-logic invariant that must
    /// hold regardless of what font is loaded.
    #[test]
    fn rasterize_run_length_matches_char_count() {
        // Load a font via the discovery chain (same as normal startup).
        let Ok((mut text, metrics)) = Text::load(None, 14.0) else {
            // No font available in this CI environment — skip gracefully.
            eprintln!("rasterize_run_length_matches_char_count: skipped (no font)");
            return;
        };
        let cell_w = metrics.width;

        // A short ASCII run.
        let input = "hello";
        let slots = text.rasterize_run(input, false, false, cell_w);
        assert_eq!(
            slots.len(),
            input.chars().count(),
            "rasterize_run should yield one slot per input character"
        );
        for slot in &slots {
            assert!(
                slot.advance_cells >= 1,
                "every slot must have advance_cells >= 1"
            );
        }
    }

    /// An empty input string must yield an empty output.
    #[test]
    fn rasterize_run_empty_input() {
        let Ok((mut text, metrics)) = Text::load(None, 14.0) else {
            eprintln!("rasterize_run_empty_input: skipped (no font)");
            return;
        };
        let slots = text.rasterize_run("", false, false, metrics.width);
        assert!(slots.is_empty(), "empty input must yield empty output");
    }

    /// `has_ligatures` must return a boolean without panicking; the return value
    /// depends on the installed font and is not asserted here.
    #[test]
    fn has_ligatures_does_not_panic() {
        let Ok((mut text, _)) = Text::load(None, 14.0) else {
            eprintln!("has_ligatures_does_not_panic: skipped (no font)");
            return;
        };
        let _ = text.has_ligatures(); // must not panic
    }

    /// `rasterize_run` with a 2-char potential-ligature pair must still return
    /// exactly 2 slots regardless of whether the font has the `fi` ligature.
    #[test]
    fn rasterize_run_two_chars_yields_two_slots() {
        let Ok((mut text, metrics)) = Text::load(None, 14.0) else {
            eprintln!("rasterize_run_two_chars_yields_two_slots: skipped (no font)");
            return;
        };
        let slots = text.rasterize_run("fi", false, false, metrics.width);
        assert_eq!(
            slots.len(), 2,
            "rasterize_run of \"fi\" must yield 2 slots (one per input char)"
        );
    }

    /// Wide-advance detection: a glyph with advance > 1.1× cell_w must have
    /// `advance > cell_w * 1.1`. This tests the `RasterizedGlyph.advance` field
    /// is populated (non-negative) for any rasterized glyph.
    #[test]
    fn rasterize_populates_advance() {
        let Ok((mut text, metrics)) = Text::load(None, 14.0) else {
            eprintln!("rasterize_populates_advance: skipped (no font)");
            return;
        };
        // Rasterize a printable ASCII character; its advance must be ≥ 0.
        let glyphs = text.rasterize('A', false, false);
        for g in &glyphs {
            assert!(
                g.advance >= 0.0,
                "advance must be non-negative for a shaped glyph"
            );
            // For a monospace font, advance should be close to cell_w.
            // Allow up to 2× for any unusual font; the key invariant is
            // that the field is populated, not its exact value.
            assert!(
                g.advance <= metrics.width * 2.0,
                "advance {} should not be wildly larger than cell_w {}",
                g.advance, metrics.width
            );
        }
    }
}
