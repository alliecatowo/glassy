//! Font shaping and glyph rasterization via `cosmic-text` + `swash`.
//! This layer is pure CPU work, free of GPU/windowing dependencies. The
//! renderer calls in on a cache miss; all repeated cells are cheap lookups
//! higher up.

use anyhow::Result;
use cosmic_text::{
    Attrs, Buffer, Family, FeatureTag, FontFeatures, FontSystem, Metrics, Shaping, Style,
    SwashCache, SwashContent, Weight, fontdb,
};

use super::discover::{FamilyOwned, build_font_system, discover_font_producers};

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
    /// Pre-parsed OpenType font features applied to every shaping call.  Empty
    /// when the user did not request any features. Built once at load time from
    /// the config's `font_features` list; re-applied on every `build_attrs` call.
    font_features: FontFeatures,
    /// Family name of the emoji font in the database. On non-macOS hosts, ZWJ
    /// clusters are forced into this single font run so the GSUB ZWJ ligature
    /// resolves — shaping 🏳️‍⚧️ across two fonts (JetBrains for ⚧, Color Emoji
    /// for 🏳) silently drops the join. macOS shapes ZWJ via CoreText instead.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    emoji_family: Option<String>,
    /// Physical pixel size of the loaded font. Read only by the macOS CoreText
    /// ZWJ path, to render at the right size without threading it through callers.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    font_px: f32,
}

/// Build shaping attributes for the given style against the resolved family,
/// applying any caller-supplied font features (OpenType feature overrides).
fn build_attrs<'a>(
    family: Family<'a>,
    bold: bool,
    italic: bool,
    features: &FontFeatures,
) -> Attrs<'a> {
    let mut attrs = Attrs::new();
    attrs.family = family;
    attrs.weight = if bold { Weight::BOLD } else { Weight::NORMAL };
    attrs.style = if italic { Style::Italic } else { Style::Normal };
    if !features.features.is_empty() {
        attrs = attrs.font_features(features.clone());
    }
    attrs
}

/// Parse a list of raw feature-tag strings (from the config) into a
/// `FontFeatures` struct. Each entry is either:
///   - a bare 4-char tag, e.g. `"ss01"` (enabled, value = 1), or
///   - `"tag=N"` where N is a `u32`, e.g. `"calt=0"` (disabled).
///
/// Entries that cannot be parsed are logged and skipped.
fn parse_font_features(raw: &[String]) -> FontFeatures {
    let mut ff = FontFeatures::new();
    for entry in raw {
        let (tag_str, val) = if let Some((t, v)) = entry.split_once('=') {
            let v: u32 = match v.trim().parse() {
                Ok(n) => n,
                Err(_) => {
                    log::warn!(
                        "glassy: font_features: invalid value in '{}' (expected u32 after '='); skipping",
                        entry
                    );
                    continue;
                }
            };
            (t.trim(), v)
        } else {
            (entry.trim(), 1u32)
        };
        let bytes = tag_str.as_bytes();
        if bytes.len() != 4 || !tag_str.is_ascii() {
            log::warn!(
                "glassy: font_features: tag '{}' must be exactly 4 ASCII characters; skipping",
                tag_str
            );
            continue;
        }
        let tag = FeatureTag::new(bytes.try_into().unwrap());
        ff.set(tag, val);
        log::debug!("glassy: font_feature '{}' = {}", tag_str, val);
    }
    ff
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
    ///
    /// `font_features` is an optional list of raw OpenType feature-tag strings
    /// from the `font_features` config key (e.g. `["ss01", "calt=0"]`). When
    /// non-empty, each feature is parsed and applied to every shaping call so
    /// callers do not need to post-process `Attrs` themselves. `None` / empty
    /// slice means "use the font's defaults" (nothing forced on or off).
    pub fn load(
        family: Option<&str>,
        font_px: f32,
        font_features: &[String],
    ) -> Result<(Text, CellMetrics)> {
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
        let (font_system, family, emoji_family) = match loaded {
            Some(found) => (found.font_system, found.family, found.emoji_family),
            None => {
                log::warn!(
                    "glassy: no usable monospace font found via fc-match or probe paths; \
                     falling back to full system font scan (slow). \
                     Install a monospace font (e.g. JetBrains Mono, DejaVu Sans Mono) \
                     or set GLASSY_FONT=<path> to suppress this."
                );
                (FontSystem::new(), FamilyOwned::Monospace, None)
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
            font_features: parse_font_features(font_features),
            emoji_family,
            font_px,
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
        let attrs = build_attrs(self.family.as_family(), false, false, &self.font_features);
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
    pub fn rasterize_cluster(
        &mut self,
        cluster: &str,
        bold: bool,
        italic: bool,
    ) -> Vec<RasterizedGlyph> {
        // ZWJ compound emoji (🏳️‍⚧️, 👨‍👩‍👧 etc.): rustybuzz doesn't apply Apple
        // Color Emoji's GSUB ZWJ lookup chain, so the sequence shatters into its
        // components — cosmic-text returns real (non-.notdef) glyphs, so the
        // `.notdef` fallback in build_glyphs won't catch it. Route ZWJ explicitly
        // through the CoreText system cascade, which resolves the ligature.
        #[cfg(target_os = "macos")]
        if cluster.contains('\u{200D}')
            && let Some(g) = render_coretext(cluster, self.font_px)
        {
            return vec![g];
        }

        // Non-macOS: force the emoji family so a component glyph the primary Nerd
        // Font happens to cover (e.g. ⚧ U+26A7) can't split the ZWJ shaping run.
        #[cfg(not(target_os = "macos"))]
        if cluster.contains('\u{200D}')
            && let Some(ref ef) = self.emoji_family.clone()
        {
            return self.build_glyphs_with_family(cluster, ef, bold, italic);
        }

        // Everything else: shape with the primary family. build_glyphs handles the
        // `.notdef` → CoreText cascade fallback for code points no loaded font covers.
        self.build_glyphs(cluster, bold, italic)
    }

    /// Shape using an explicit family name rather than the primary font. Used for
    /// ZWJ emoji clusters that must stay in a single font run.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    fn build_glyphs_with_family(
        &mut self,
        text: &str,
        family: &str,
        bold: bool,
        italic: bool,
    ) -> Vec<RasterizedGlyph> {
        let attrs = build_attrs(Family::Name(family), bold, italic, &self.font_features);
        self.buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);
        self.collect_glyphs()
    }

    /// Shape and rasterize a single character into owned RGBA bitmaps.
    fn build_glyphs(&mut self, text: &str, bold: bool, italic: bool) -> Vec<RasterizedGlyph> {
        // `family` borrows `self`, so capture the borrowed `Family` before the
        // `&mut self.font_system` borrows below; they touch disjoint fields.
        let attrs = build_attrs(self.family.as_family(), bold, italic, &self.font_features);
        self.buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        // `.notdef` (glyph_id 0) means no loaded font covers this code point — the
        // shaper would otherwise hand us a tofu box. On macOS, defer to CoreText's
        // system cascade (the same path Terminal.app uses) so e.g. ⏵ U+23F5 resolves
        // to its real glyph in STIX Two Math instead of rendering as an empty square.
        #[cfg(target_os = "macos")]
        {
            let has_notdef = self
                .buffer
                .layout_runs()
                .any(|run| run.glyphs.iter().any(|g| g.glyph_id == 0));
            if has_notdef && let Some(g) = render_coretext(text, self.font_px) {
                return vec![g];
            }
        }

        self.collect_glyphs()
    }

    /// Collect rasterized glyphs from the shaped buffer. Shared by all shaping paths.
    fn collect_glyphs(&mut self) -> Vec<RasterizedGlyph> {
        let mut out = Vec::new();
        for run in self.buffer.layout_runs() {
            for glyph in run.glyphs.iter() {
                // `.notdef` (glyph_id 0): the shaper found no real glyph in any
                // loaded font. Skip it so we draw nothing rather than a tofu box.
                // (macOS reroutes these to CoreText in build_glyphs before reaching
                // here; this guards the non-macOS path and any CoreText miss.)
                if glyph.glyph_id == 0 {
                    continue;
                }
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

        let attrs = build_attrs(self.family.as_family(), bold, italic, &self.font_features);
        self.buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        // We need to map shaped output glyphs back to input character positions.
        // Each LayoutGlyph carries `start`/`end` byte offsets into the input string.
        // Build a byte_offset → char_index lookup for all scalar boundaries.
        let char_starts: Vec<usize> = text.char_indices().map(|(byte_off, _)| byte_off).collect();
        // Map byte offset → char index. We'll do a linear search since runs are short.
        let byte_to_char = |byte_off: usize| -> usize {
            char_starts.iter().position(|&b| b == byte_off).unwrap_or(0)
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

                // `.notdef` (glyph_id 0): no font covers this code point. Reserve
                // the cell's advance but draw nothing (no tofu box). The single-char
                // path (build_glyphs) reroutes these to CoreText; this run path is
                // ligature/ASCII shaping where a miss should just stay blank.
                if glyph.glyph_id == 0 {
                    if char_idx < slots.len() {
                        slots[char_idx].advance_cells = slots[char_idx].advance_cells.max(span);
                    }
                    continue;
                }

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
        let attrs = build_attrs(self.family.as_family(), false, false, &self.font_features);
        self.buffer.set_text("fi", &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(&mut self.font_system, false);

        let glyph_count: usize = self.buffer.layout_runs().map(|r| r.glyphs.len()).sum();
        // "fi" has 2 input characters; a liga font collapses them into 1 glyph.
        glyph_count < 2
    }
}

/// Render a cluster through CoreText's system font cascade — the universal macOS
/// fallback, used whenever cosmic-text/rustybuzz can't render a cluster correctly:
///
///   1. ZWJ compound emoji (🏳️‍⚧️): rustybuzz doesn't apply Apple Color Emoji's
///      GSUB ZWJ lookup chain, so the sequence shatters into components.
///   2. Code points absent from glassy's curated font chain (e.g. ⏵ U+23F5, which
///      lives in STIX Two Math): cosmic-text returns `.notdef` and would draw tofu.
///
/// CoreText shapes through the same per-codepoint cascade Terminal.app uses, so it
/// resolves both cases. We render with a monospace base font and let CoreText
/// cascade to whatever system font covers each character.
///
/// The rendered bitmap is inspected: a purely grayscale result is returned as a
/// single-channel coverage **mask** (`is_color = false`) so the renderer tints it
/// with the cell's foreground color (correct for text symbols like ⏵); a result
/// with any chroma is returned as straight **RGBA** (`is_color = true`) for color
/// emoji. Returns `None` if CoreText also produced nothing.
#[cfg(target_os = "macos")]
fn render_coretext(cluster: &str, font_px: f32) -> Option<RasterizedGlyph> {
    use core_foundation::attributed_string::CFMutableAttributedString;
    use core_foundation::base::{CFRange, TCFType};
    use core_foundation::string::CFString;
    use core_graphics::base::{kCGBitmapByteOrder32Host, kCGImageAlphaPremultipliedFirst};
    use core_graphics::color_space::CGColorSpace;
    use core_graphics::context::CGContext;
    use core_text::line::CTLine;
    use core_text::string_attributes::kCTFontAttributeName;

    // Menlo is the macOS terminal default monospace; CoreText cascades from it to
    // whatever system font covers each code point (Apple Color Emoji, STIX, …).
    let font = core_text::font::new_from_name("Menlo", font_px as f64).ok()?;

    let cf_str = CFString::new(cluster);
    let mut attr = CFMutableAttributedString::new();
    attr.replace_str(&cf_str, CFRange::init(0, 0));
    // char_len() is the UTF-16 length, which is what CFRange expects.
    let full = CFRange::init(0, cf_str.char_len());
    // SAFETY: kCTFontAttributeName is an extern "C" static from CoreText — the
    // documented way to name the font attribute on a CFAttributedString.
    unsafe {
        attr.set_attribute(full, kCTFontAttributeName, &font);
    }

    let line = CTLine::new_with_attributed_string(attr.as_concrete_TypeRef());

    // Measure inked bounds against a 1×1 probe context (bounds don't depend on size).
    let rgb = CGColorSpace::create_device_rgb();
    let bitmap_info = kCGBitmapByteOrder32Host | kCGImageAlphaPremultipliedFirst;
    let probe = CGContext::create_bitmap_context(None, 1, 1, 8, 4, &rgb, bitmap_info);
    let bounds = line.get_image_bounds(&probe);

    let w = bounds.size.width.ceil() as usize;
    let h = bounds.size.height.ceil() as usize;
    if w == 0 || h == 0 {
        return None;
    }

    let stride = w * 4;
    let mut ctx = CGContext::create_bitmap_context(None, w, h, 8, stride, &rgb, bitmap_info);

    // Flip CTM so the backing buffer ends up top-left (row 0 = top), matching the
    // atlas/RasterizedGlyph convention (CoreGraphics origin is otherwise bottom-left).
    ctx.translate(0.0, h as f64);
    ctx.scale(1.0, -1.0);
    ctx.set_should_antialias(true);
    // Fill text white so a monochrome glyph's coverage lands in the alpha channel
    // (premultiplied white → R=G=B=A=coverage). Color glyphs ignore the fill.
    ctx.set_rgb_fill_color(1.0, 1.0, 1.0, 1.0);

    // Shift so the glyph's inked box starts at the buffer origin (bounds.origin can
    // be negative — ink left of the pen / below the baseline).
    ctx.set_text_position(-bounds.origin.x, -bounds.origin.y);
    line.draw(&ctx);

    // ctx.data() is premultiplied BGRA (kCGBitmapByteOrder32Host + AlphaPremultipliedFirst
    // on little-endian). Detect whether the glyph carries chroma: a text symbol comes
    // back grayscale (R==G==B per pixel), a color emoji does not.
    let src = ctx.data();
    let is_color = src
        .chunks_exact(4)
        .any(|px| px[3] != 0 && (px[0] != px[1] || px[1] != px[2]));

    let left = bounds.origin.x.floor() as i32;
    // top = distance baseline → top of bitmap (positive above baseline). CoreText
    // origin.y = baseline → bottom of ink, so top = origin.y + h.
    let top = (bounds.origin.y + h as f64).round() as i32;

    if is_color {
        // Un-premultiply BGRA → straight RGBA for the color atlas (Rgba8Unorm).
        let mut data = vec![0u8; w * h * 4];
        for (dst, px) in data.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
            let (b, g, r, a) = (px[0], px[1], px[2], px[3]);
            if a == 0 {
                continue;
            }
            let unpre =
                |c: u8| -> u8 { ((c as u16 * 255 + a as u16 / 2) / a as u16).min(255) as u8 };
            dst[0] = unpre(r);
            dst[1] = unpre(g);
            dst[2] = unpre(b);
            dst[3] = a;
        }
        Some(RasterizedGlyph {
            width: w as u32,
            height: h as u32,
            left,
            top,
            is_color: true,
            data,
            advance: 0.0,
        })
    } else {
        // Monochrome: take the alpha channel as an R8 coverage mask so the renderer
        // tints it with the cell's foreground color.
        let data: Vec<u8> = src.chunks_exact(4).map(|px| px[3]).collect();
        Some(RasterizedGlyph {
            width: w as u32,
            height: h as u32,
            left,
            top,
            is_color: false,
            data,
            advance: 0.0,
        })
    }
}
