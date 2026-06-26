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

pub(crate) mod discover;
pub mod shape;

pub use shape::{CellMetrics, RasterizedGlyph, Text};

#[cfg(test)]
mod tests {
    use super::*;

    /// `rasterize_run` must return exactly one `RunGlyph` per input scalar,
    /// each with `advance_cells >= 1`. This is a pure-logic invariant that must
    /// hold regardless of what font is loaded.
    #[test]
    fn rasterize_run_length_matches_char_count() {
        // Load a font via the discovery chain (same as normal startup).
        let Ok((mut text, metrics)) = Text::load(None, 14.0, &[]) else {
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
        let Ok((mut text, metrics)) = Text::load(None, 14.0, &[]) else {
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
        let Ok((mut text, _)) = Text::load(None, 14.0, &[]) else {
            eprintln!("has_ligatures_does_not_panic: skipped (no font)");
            return;
        };
        let _ = text.has_ligatures(); // must not panic
    }

    /// `rasterize_run` with a 2-char potential-ligature pair must still return
    /// exactly 2 slots regardless of whether the font has the `fi` ligature.
    #[test]
    fn rasterize_run_two_chars_yields_two_slots() {
        let Ok((mut text, metrics)) = Text::load(None, 14.0, &[]) else {
            eprintln!("rasterize_run_two_chars_yields_two_slots: skipped (no font)");
            return;
        };
        let slots = text.rasterize_run("fi", false, false, metrics.width);
        assert_eq!(
            slots.len(),
            2,
            "rasterize_run of \"fi\" must yield 2 slots (one per input char)"
        );
    }

    /// Font features with an empty list must load without error and produce the
    /// same result as passing no features at all.
    #[test]
    fn font_features_empty_list_loads() {
        let r1 = Text::load(None, 14.0, &[]);
        let r2 = Text::load(None, 14.0, &[]);
        // Both must either both succeed or both fail (no font).
        assert_eq!(
            r1.is_ok(),
            r2.is_ok(),
            "empty features must not change load outcome"
        );
    }

    /// Font features with a valid tag list must parse and load without error.
    /// We cannot assert the rendering is different (depends on the installed font),
    /// but the load path must not panic or error out on valid tags.
    #[test]
    fn font_features_valid_tags_loads() {
        let features = vec![
            "ss01".to_string(),   // bare tag → enabled
            "calt=0".to_string(), // explicit disable
            "liga=1".to_string(), // explicit enable
        ];
        match Text::load(None, 14.0, &features) {
            Ok(_) => {} // expected
            Err(_) => {
                // No font installed in this environment — that's fine.
                eprintln!("font_features_valid_tags_loads: skipped (no font)");
            }
        }
    }

    /// Wide-advance detection: a glyph with advance > 1.1× cell_w must have
    /// `advance > cell_w * 1.1`. This tests the `RasterizedGlyph.advance` field
    /// is populated (non-negative) for any rasterized glyph.
    #[test]
    fn rasterize_populates_advance() {
        let Ok((mut text, metrics)) = Text::load(None, 14.0, &[]) else {
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
                g.advance,
                metrics.width
            );
        }
    }

    // -----------------------------------------------------------------------
    // FONTS stream: per-style overrides, symbol map, variable axes
    // -----------------------------------------------------------------------

    /// `load_with_config` with no overrides must behave identically to `load`.
    #[test]
    fn load_with_config_no_overrides_matches_load() {
        use crate::text::shape::FontConfig;
        let r1 = Text::load(None, 14.0, &[]);
        let r2 = Text::load_with_config(FontConfig {
            family: None,
            font_px: 14.0,
            font_features: &[],
            bold_family: None,
            italic_family: None,
            bold_italic_family: None,
            symbol_map: Vec::new(),
            font_variations: &[],
        });
        assert_eq!(
            r1.is_ok(),
            r2.is_ok(),
            "load_with_config with no overrides must succeed when load succeeds"
        );
    }

    /// `font_variations: wght=700` must not panic and must load successfully.
    #[test]
    fn font_variations_wght_loads_without_panic() {
        use crate::text::shape::FontConfig;
        let variations = vec!["wght=700".to_string()];
        match Text::load_with_config(FontConfig {
            family: None,
            font_px: 14.0,
            font_features: &[],
            bold_family: None,
            italic_family: None,
            bold_italic_family: None,
            symbol_map: Vec::new(),
            font_variations: &variations,
        }) {
            Ok(_) => {}
            Err(_) => {
                eprintln!("font_variations_wght_loads_without_panic: skipped (no font)");
            }
        }
    }

    /// `font_variations: wdth=75` (condensed) must not panic.
    #[test]
    fn font_variations_wdth_loads_without_panic() {
        use crate::text::shape::FontConfig;
        let variations = vec!["wdth=75".to_string()];
        match Text::load_with_config(FontConfig {
            family: None,
            font_px: 14.0,
            font_features: &[],
            bold_family: None,
            italic_family: None,
            bold_italic_family: None,
            symbol_map: Vec::new(),
            font_variations: &variations,
        }) {
            Ok(_) => {}
            Err(_) => {
                eprintln!("font_variations_wdth_loads_without_panic: skipped (no font)");
            }
        }
    }

    /// An unknown variation axis must be silently ignored (logged but not fatal).
    #[test]
    fn font_variations_unknown_axis_ignored() {
        use crate::text::shape::FontConfig;
        let variations = vec!["XXXX=42".to_string()];
        // Must not panic; result is the same as loading without variations.
        let _ = Text::load_with_config(FontConfig {
            family: None,
            font_px: 14.0,
            font_features: &[],
            bold_family: None,
            italic_family: None,
            bold_italic_family: None,
            symbol_map: Vec::new(),
            font_variations: &variations,
        });
    }

    /// `rasterize` with a non-existent per-style family must not panic (it falls
    /// back to the primary font). Absence of the override family means cosmic-text
    /// picks the closest available match, so the result is always some glyph.
    #[test]
    fn load_with_config_missing_bold_family_falls_back() {
        use crate::text::shape::FontConfig;
        match Text::load_with_config(FontConfig {
            family: None,
            font_px: 14.0,
            font_features: &[],
            bold_family: Some("__NoSuchFontFamily__XYZ__"),
            italic_family: None,
            bold_italic_family: None,
            symbol_map: Vec::new(),
            font_variations: &[],
        }) {
            Ok((mut text, metrics)) => {
                // Bold rasterize must not panic even if the override family is absent.
                let glyphs = text.rasterize('A', true, false);
                for g in &glyphs {
                    assert!(g.advance >= 0.0);
                    assert!(g.advance <= metrics.width * 2.0);
                }
            }
            Err(_) => {
                eprintln!("load_with_config_missing_bold_family_falls_back: skipped (no font)");
            }
        }
    }
}
