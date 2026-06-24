//! Font shaping and glyph rasterization via `cosmic-text` + `swash`.
//! This layer is pure CPU work, free of GPU/windowing dependencies. The
//! renderer calls in on a cache miss; all repeated cells are cheap lookups
//! higher up.

use anyhow::Result;
use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, Style, SwashCache, SwashContent, Weight,
    fontdb,
};

use super::discover::{
    FamilyOwned, build_font_system, discover_font_producers,
};

/// Per-cell layout metrics, all in physical pixels.
#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    pub width: f32,
    pub height: f32,
    pub ascent: f32,
    /// Y of the underline stroke's top edge, measured DOWN from the cell top
    /// (so the renderer can draw it directly at `origin_y + underline_y`).
    pub underline_y: f32,
    /// Y of the strikethrough stroke's top edge, measured down from the cell top.
    pub strikeout_y: f32,
    /// Thickness of decoration strokes (underline / strikeout), in pixels.
    pub decoration_thickness: f32,
}

/// A single rasterized glyph bitmap plus its placement relative to the pen.
pub struct RasterizedGlyph {
    /// Bitmap size in pixels (both 0 for blank cells with nothing to draw).
    pub width: u32,
    pub height: u32,
    /// Horizontal offset from the pen origin to the bitmap's left edge.
    pub left: i32,
    /// Vertical offset from the baseline UP to the bitmap's top edge
    /// (positive = above the baseline), per swash's placement convention.
    pub top: i32,
    /// `true` when `data` is the glyph's own color (e.g. emoji); `false` when it
    /// is a single-channel coverage mask.
    pub is_color: bool,
    /// Glyph pixels. For a coverage mask (`is_color == false`) this is one byte
    /// per pixel (R8 coverage), length == `width * height`. For a color glyph
    /// (`is_color == true`) it is RGBA8, length == `width * height * 4`. Empty
    /// when either dimension is 0.
    pub data: Vec<u8>,
    /// Pen advance (horizontal) for this glyph in physical pixels, as reported by
    /// the shaper. Used for Nerd-font wide-icon detection: if `advance` is more
    /// than 1.1× the cell width, the glyph should be promoted to a WIDE (2-cell)
    /// slot to avoid clipping. 0.0 for glyphs from the color path where the
    /// advance is irrelevant (color emoji are always drawn by size, not advance).
    pub advance: f32,
}

/// Per-input-cell slot produced by [`Text::rasterize_run`]. The `glyphs` vec is
/// non-empty for the *first* cell of each shaped glyph (even a ligature that
/// visually spans multiple cells is anchored to its first cell); subsequent cells
/// that belong to the same ligature carry an empty `glyphs` vec (they must be
/// rendered as blank/background-only). `advance_cells` is how many input cells
/// this shaped output consumes (always 1 for ordinary glyphs; >1 for a ligature
/// or a wide glyph that spans more than one cell).
pub struct RunGlyph {
    /// The rasterized bitmaps for this cell slot. Empty on continuation cells.
    pub glyphs: Vec<RasterizedGlyph>,
    /// How many grid cells this output occupies (1 = normal, 2+ = ligature/wide).
    pub advance_cells: usize,
}

/// Owns the shaping/rasterization state. Rasterized glyphs are *not* cached here;
/// the renderer caches the packed atlas glyphs and only calls in on a miss.
pub struct Text {
    pub(super) font_system: FontSystem,
    swash_cache: SwashCache,
    /// Reused for every shaping call to avoid reallocating line buffers.
    buffer: Buffer,
    /// The resolved font family. We store the name as an owned `String` because
    /// `Family::Name` borrows; `attrs()` rebuilds the borrowing `Family` per call.
    pub(super) family: FamilyOwned,
}

/// Build shaping attributes for the given style against the resolved family.
fn build_attrs<'a>(family: Family<'a>, bold: bool, italic: bool) -> Attrs<'a> {
    let mut attrs = Attrs::new();
    attrs.family = family;
    attrs.weight = if bold { Weight::BOLD } else { Weight::NORMAL };
    attrs.style = if italic { Style::Italic } else { Style::Normal };
    attrs
}

/// Best-effort fallback advance when shaping yields no measurable glyph.
fn font_fallback_width(line_height: f32) -> f32 {
    (line_height * 0.5).round().max(1.0)
}

impl Text {
    /// Discover a monospace font, load it, and measure the cell box for `font_px`.
    ///
    /// `family` is an optional preferred family name (from config/CLI). When set
    /// it is tried first (resolved via fontconfig, verified to actually be that
    /// family); discovery then falls back to the curated list and the rest of the
    /// chain so a typo'd or absent family still yields a usable monospace font.
    pub fn load(family: Option<&str>, font_px: f32) -> Result<(Text, CellMetrics)> {
        // Gather candidate *producers* in priority order (explicit override,
        // requested family, curated families verified via fontconfig, generic
        // monospace, known paths). Each producer is a closure that only runs its
        // (potentially expensive: an `fc-match` subprocess + a font read) work
        // when actually polled — so once an early candidate loads as a usable
        // monospace face we stop and never pay for the rest of the chain. This is
        // a large startup win: the common case (FiraCode present) used to run an
        // `fc-match` for every curated family before picking the first one.
        let producers = discover_font_producers(family);

        // Build a `FontSystem` from the first producer that yields a usable
        // monospaced face. A non-monospaced face is accepted only as a last
        // resort (no producer after it yields a usable face).
        let mut loaded = None;
        let mut fallback = None;
        for producer in producers {
            let Some(candidate) = producer() else {
                continue;
            };
            match build_font_system(candidate.bytes, candidate.path) {
                Some(found) => {
                    if found.is_monospaced {
                        loaded = Some(found);
                        break;
                    }
                    log::debug!(
                        "glassy: candidate '{}' is not monospaced; keeping as fallback",
                        candidate.source_label
                    );
                    fallback.get_or_insert(found);
                }
                None => {
                    log::debug!(
                        "glassy: candidate '{}' had no usable face, trying next",
                        candidate.source_label
                    );
                }
            }
        }
        let loaded = loaded.or(fallback);

        // If nothing loaded, fall back to a full system font scan.
        // This is intentionally a last resort — it walks every font on the
        // system and is O(hundreds of files) on a typical Linux install, so
        // it adds hundreds of milliseconds to startup. Warn so the user (or
        // packager) knows the curated discovery chain failed entirely.
        let (font_system, family) = match loaded {
            Some(found) => (found.font_system, found.family),
            None => {
                log::warn!(
                    "glassy: no usable monospace font found via fc-match or probe paths; \
                     falling back to full system font scan (slow). \
                     Install a monospace font (e.g. JetBrains Mono, DejaVu Sans Mono) \
                     or set GLASSY_FONT=<path> to suppress this."
                );
                (FontSystem::new(), FamilyOwned::Monospace)
            }
        };

        // Line height is the cell height; round so cells land on pixel
        // boundaries. 1.30x the em gives comfortable vertical spacing.
        let line_height = (font_px * 1.30).round();
        let metrics = Metrics::new(font_px, line_height);

        let mut text = Text {
            font_system,
            swash_cache: SwashCache::new(),
            buffer: Buffer::new_empty(metrics),
            family,
        };

        let cell = text.measure_cell(font_px, line_height);
        let family_name = match &text.family {
            FamilyOwned::Named(n) => n.as_str(),
            FamilyOwned::Monospace => "<system monospace>",
        };
        log::info!(
            "font='{}' font_px={:.1} cell={}x{} ascent={}",
            family_name,
            font_px,
            cell.width,
            cell.height,
            cell.ascent
        );
        Ok((text, cell))
    }

    /// Shape a representative string to derive the monospace advance width and
    /// the baseline (ascent) for one line, plus the decoration (underline /
    /// strikeout) stroke positions read from the primary font's metrics.
    fn measure_cell(&mut self, font_px: f32, line_height: f32) -> CellMetrics {
        let attrs = build_attrs(self.family.as_family(), false, false);
        self.buffer.set_text("MM", &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        let mut cell_w = font_fallback_width(line_height);
        let mut ascent = line_height.round();
        // The font id of the shaped face, so we can read its decoration metrics.
        let mut font_id = None;

        if let Some(run) = self.buffer.layout_runs().next() {
            ascent = run.line_y.round();
            // Two identical glyphs let us read the pen advance directly as the
            // gap between their origins; for a single glyph fall back to its width.
            if run.glyphs.len() >= 2 {
                cell_w = (run.glyphs[1].x - run.glyphs[0].x).round().max(1.0);
            } else if let Some(g) = run.glyphs.first() {
                cell_w = g.w.ceil().max(1.0);
            }
            font_id = run.glyphs.first().map(|g| (g.font_id, g.font_weight));
        }

        let (underline_y, strikeout_y, decoration_thickness) =
            self.decoration_metrics(font_id, font_px, ascent, line_height);

        CellMetrics {
            width: cell_w,
            height: line_height,
            ascent,
            underline_y,
            strikeout_y,
            decoration_thickness,
        }
    }

    /// Read the underline/strikeout stroke positions and thickness from the
    /// shaped face's swash metrics, falling back to sensible em-relative values
    /// when the font reports nothing usable.
    ///
    /// swash reports `underline_offset` / `strikeout_offset` as the distance
    /// from the baseline UP to the top of the stroke (positive = above the
    /// baseline). Underlines sit below the baseline, so that offset is normally
    /// negative; we convert to a top-of-cell-relative Y with `ascent - offset`.
    /// Returned Ys are the stroke's top edge, measured down from the cell top.
    fn decoration_metrics(
        &mut self,
        font_id: Option<(fontdb::ID, Weight)>,
        font_px: f32,
        ascent: f32,
        line_height: f32,
    ) -> (f32, f32, f32) {
        // Em-relative fallbacks used when the font lacks usable metrics.
        let fallback_thickness = (line_height / 16.0).round().max(1.0);
        let fallback_underline = (ascent + fallback_thickness * 2.0).round();
        let fallback_strikeout = (ascent - (ascent * 0.30)).round();

        let Some((id, weight)) = font_id else {
            return (fallback_underline, fallback_strikeout, fallback_thickness);
        };
        let Some(font) = self.font_system.get_font(id, weight) else {
            return (fallback_underline, fallback_strikeout, fallback_thickness);
        };

        // `metrics(&[])` yields the unscaled (font-unit) metrics; `scale(px)`
        // converts to pixels for our em size (`font_px`).
        let m = font.as_swash().metrics(&[]).scale(font_px);

        let thickness = if m.stroke_size > 0.0 {
            m.stroke_size.round().max(1.0)
        } else {
            fallback_thickness
        };

        // Underline: top edge = ascent - underline_offset (offset is negative).
        let underline_y = if m.underline_offset != 0.0 {
            (ascent - m.underline_offset).round()
        } else {
            fallback_underline
        };
        // Strikeout: offset is positive (above baseline). Its value is the
        // CENTER/top per the OS/2 table; we treat it as the stroke top.
        let strikeout_y = if m.strikeout_offset != 0.0 {
            (ascent - m.strikeout_offset).round()
        } else {
            fallback_strikeout
        };

        // Clamp the underline so it stays inside the cell (some faces report an
        // offset that would push the stroke past the descender region).
        let max_y = (line_height - thickness).max(0.0);
        let underline_y = underline_y.min(max_y).max(0.0);
        let strikeout_y = strikeout_y.min(max_y).max(0.0);

        (underline_y, strikeout_y, thickness)
    }

    /// Return the glyph bitmap(s) needed to draw `ch` in the given style.
    ///
    /// Blank cells (spaces and anything with no drawable coverage) yield an empty
    /// Vec. Not cached here: the renderer caches the *packed atlas glyphs* and only
    /// calls this on a cache miss, so a second Text-level cache would just retain a
    /// duplicate bitmap that is never read.
    pub fn rasterize(&mut self, ch: char, bold: bool, italic: bool) -> Vec<RasterizedGlyph> {
        let mut tmp = [0u8; 4];
        self.build_glyphs(ch.encode_utf8(&mut tmp), bold, italic)
    }

    /// Rasterize a full grapheme cluster (a base character plus its combining
    /// marks / ZWJ-joined codepoints) as a single shaped unit, so compound
    /// emoji (flags, ZWJ sequences, skin-tone modifiers) and combining
    /// sequences resolve to their single combined glyph. Used only for cells that
    /// actually carry combiners; the common single-character path stays on
    /// `rasterize`. Uncached for the same reason as [`Text::rasterize`].
    pub fn rasterize_cluster(&mut self, cluster: &str, bold: bool, italic: bool) -> Vec<RasterizedGlyph> {
        self.build_glyphs(cluster, bold, italic)
    }

    /// Shape and rasterize a single character into owned RGBA bitmaps.
    fn build_glyphs(&mut self, text: &str, bold: bool, italic: bool) -> Vec<RasterizedGlyph> {
        // `family` borrows `self`, so capture the borrowed `Family` before the
        // `&mut self.font_system` borrows below; they touch disjoint fields.
        let attrs = build_attrs(self.family.as_family(), bold, italic);
        self.buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        let mut out = Vec::new();
        for run in self.buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let advance = glyph.w;
                let pg = glyph.physical((0.0, 0.0), 1.0);
                // Clone the Option so the FontSystem borrow ends before we read
                // the image and build the glyph below.
                let img_opt = self
                    .swash_cache
                    .get_image(&mut self.font_system, pg.cache_key)
                    .clone();
                let Some(img) = img_opt else { continue };

                let (w, h) = (img.placement.width, img.placement.height);
                if w == 0 || h == 0 {
                    continue;
                }
                let pixels = (w * h) as usize;

                let (data, is_color) = match img.content {
                    // 1 byte/px coverage: keep it as-is for the R8 mask atlas.
                    SwashContent::Mask => (img.data.clone(), false),
                    // 3 bytes/px subpixel coverage; collapse to a single coverage
                    // byte for the R8 mask atlas.
                    SwashContent::SubpixelMask => {
                        let mut d = Vec::with_capacity(pixels);
                        for px in img.data.chunks_exact(3) {
                            d.push(px[0].max(px[1]).max(px[2]));
                        }
                        (d, false)
                    }
                    // Already RGBA (color emoji): keep as-is for the color atlas.
                    SwashContent::Color => (img.data.clone(), true),
                };

                out.push(RasterizedGlyph {
                    width: w,
                    height: h,
                    left: img.placement.left,
                    top: img.placement.top,
                    is_color,
                    data,
                    advance,
                });
            }
        }
        out
    }

    /// Shape and rasterize a multi-cell run of characters as a single shaping
    /// unit so OpenType ligatures (GSUB liga) are resolved across cell boundaries.
    ///
    /// Returns one [`RunGlyph`] per input *character* (Unicode scalar, not byte).
    /// The first character of each shaped output glyph carries the rasterized
    /// bitmaps; subsequent cells consumed by the same output shape (ligature
    /// continuations) carry empty bitmaps and `advance_cells == 0`. The caller
    /// must blank-render those continuation cells.
    ///
    /// `cell_w` is the nominal single-cell advance in physical pixels, used to
    /// compute `advance_cells` for each output glyph.
    pub fn rasterize_run(
        &mut self,
        text: &str,
        bold: bool,
        italic: bool,
        cell_w: f32,
    ) -> Vec<RunGlyph> {
        let char_count = text.chars().count();
        if char_count == 0 {
            return Vec::new();
        }

        let attrs = build_attrs(self.family.as_family(), bold, italic);
        self.buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        // We need to map shaped output glyphs back to input character positions.
        // Each LayoutGlyph carries `start`/`end` byte offsets into the input string.
        // Build a byte_offset → char_index lookup for all scalar boundaries.
        let char_starts: Vec<usize> = text
            .char_indices()
            .map(|(byte_off, _)| byte_off)
            .collect();
        // Map byte offset → char index. We'll do a linear search since runs are short.
        let byte_to_char = |byte_off: usize| -> usize {
            char_starts
                .iter()
                .position(|&b| b == byte_off)
                .unwrap_or(0)
        };

        // Allocate output slots — one per input character.
        let mut slots: Vec<RunGlyph> = (0..char_count)
            .map(|_| RunGlyph {
                glyphs: Vec::new(),
                advance_cells: 0,
            })
            .collect();

        // For each shaped glyph, deposit its rasterized bitmap in the output slot
        // that corresponds to the first input character of the glyph's byte range.
        for run in self.buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                let advance = glyph.w;
                let char_idx = byte_to_char(glyph.start);
                // How many input cells does this advance span?
                let span = if cell_w > 0.0 {
                    ((advance / cell_w).round() as usize).max(1)
                } else {
                    1
                };

                let pg = glyph.physical((0.0, 0.0), 1.0);
                let img_opt = self
                    .swash_cache
                    .get_image(&mut self.font_system, pg.cache_key)
                    .clone();
                let Some(img) = img_opt else {
                    // Advance-only glyph (space, control char): mark the slot
                    // so the caller knows this cell was shaped but blank.
                    if char_idx < slots.len() {
                        slots[char_idx].advance_cells = slots[char_idx].advance_cells.max(span);
                    }
                    continue;
                };

                let (w, h) = (img.placement.width, img.placement.height);
                if w == 0 || h == 0 {
                    if char_idx < slots.len() {
                        slots[char_idx].advance_cells = slots[char_idx].advance_cells.max(span);
                    }
                    continue;
                }
                let pixels = (w * h) as usize;
                let (data, is_color) = match img.content {
                    SwashContent::Mask => (img.data.clone(), false),
                    SwashContent::SubpixelMask => {
                        let mut d = Vec::with_capacity(pixels);
                        for px in img.data.chunks_exact(3) {
                            d.push(px[0].max(px[1]).max(px[2]));
                        }
                        (d, false)
                    }
                    SwashContent::Color => (img.data.clone(), true),
                };

                if char_idx < slots.len() {
                    slots[char_idx].glyphs.push(RasterizedGlyph {
                        width: w,
                        height: h,
                        left: img.placement.left,
                        top: img.placement.top,
                        is_color,
                        data,
                        advance,
                    });
                    slots[char_idx].advance_cells = slots[char_idx].advance_cells.max(span);
                }
            }
        }

        // Ensure every slot has advance_cells >= 1 so callers can advance.
        for slot in &mut slots {
            if slot.advance_cells == 0 {
                slot.advance_cells = 1;
            }
        }

        slots
    }

    /// Probe whether the primary font loaded by this `Text` instance carries an
    /// OpenType GSUB `liga` (standard ligatures) feature, which is a prerequisite
    /// for run-level shaping to produce any ligature output.
    ///
    /// This is a best-effort heuristic: if the font system has no shaped run from
    /// which we can read the underlying font's feature set we fall back to `false`.
    /// False negatives (returning `false` for a font that does have liga) are safe
    /// — they just disable the ligature path and fall back to per-char shaping.
    /// False positives (returning `true` for a font without liga) are also safe:
    /// run-level shaping simply returns the same per-char result.
    ///
    /// Detection strategy: shape "fi" and check whether the resulting shaped output
    /// contains *fewer* glyphs than the two input characters. A real `liga` font
    /// collapses "fi" into a single ligature glyph; a font without liga returns two
    /// separate glyph records.
    pub fn has_ligatures(&mut self) -> bool {
        let attrs = build_attrs(self.family.as_family(), false, false);
        self.buffer.set_text("fi", &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        let glyph_count: usize = self
            .buffer
            .layout_runs()
            .map(|r| r.glyphs.len())
            .sum();
        // "fi" has 2 input characters; a liga font collapses them into 1 glyph.
        glyph_count < 2
    }
}
