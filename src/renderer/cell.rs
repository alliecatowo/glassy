//! Per-cell rendering: push_cell, glyph placement, row management.

use super::*;

impl Renderer {
    /// Enable or disable ligature run-shaping. Only has an effect when the loaded
    /// font was detected to carry an OpenType GSUB liga feature (see
    /// `font_has_ligatures`). When ligatures are disabled (or the font lacks liga),
    /// each cell is shaped individually as before.
    pub fn set_ligatures(&mut self, enabled: bool) {
        self.ligatures_enabled = enabled;
    }

    /// Returns true when both the config flag is set AND the loaded font was
    /// detected to have OpenType GSUB ligatures. The render loop uses this to
    /// decide whether to accumulate cell runs and call `push_ligature_run`.
    pub fn ligatures_active(&self) -> bool {
        self.ligatures_enabled && self.font_has_ligatures
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        // Skip the (expensive) surface.configure() call when the size is unchanged.
        // This is common at startup when the app calls resize() twice: once with the
        // initial 1×1 placeholder and once with the real window size, plus any
        // redundant resize events the compositor fires. configure() can stall the
        // driver for a swapchain recreation even when the size hasn't changed.
        if width == self.config.width && height == self.config.height {
            return;
        }
        self.config.width = width;
        self.config.height = height;
        self.surface.configure(&self.device, &self.config);
        self.queue.write_buffer(
            &self.uniform_buffer,
            0,
            bytemuck::bytes_of(&Uniform {
                screen: [width as f32, height as f32, 0.0, 0.0],
            }),
        );
        // Keep the CRT offscreen target sized to the surface (no-op when off).
        self.crt_on_resize();
    }

    pub fn cell_metrics(&self) -> CellMetrics {
        self.metrics
    }

    /// Physical-pixel inset applied to the grid on all sides. The app must
    /// account for this when computing how many cells fit in the surface.
    pub fn pad(&self) -> f32 {
        self.pad
    }

    /// Total horizontal padding (left + right) in physical px. Use this — not
    /// `2 * pad()` — when computing how many columns fit: the sides can differ
    /// (default left inset, or per-side config overrides), and assuming symmetry
    /// over-counts columns and clips the right edge.
    pub fn pad_x(&self) -> f32 {
        self.pad_left() + self.pad_right()
    }

    /// Total vertical padding (top + bottom) in physical px. The grid origin adds
    /// the tab-strip/status-bar bands separately; this is just the window inset.
    pub fn pad_y(&self) -> f32 {
        self.pad_top() + self.pad_bottom()
    }

    /// Get the effective padding on each side (physical px). Per-side overrides
    /// take precedence; otherwise the uniform `pad` is used.
    #[allow(dead_code)]
    pub fn pad_top(&self) -> f32 {
        self.pad_top.unwrap_or(self.pad)
    }

    #[allow(dead_code)]
    pub fn pad_bottom(&self) -> f32 {
        self.pad_bottom.unwrap_or(self.pad)
    }

    #[allow(dead_code)]
    pub fn pad_left(&self) -> f32 {
        self.pad_left.unwrap_or(self.pad)
    }

    #[allow(dead_code)]
    pub fn pad_right(&self) -> f32 {
        self.pad_right.unwrap_or(self.pad)
    }

    /// The current font size in physical pixels.
    pub fn font_px(&self) -> f32 {
        self.font_px
    }

    /// The current surface size in physical pixels `(width, height)`. Used by the
    /// split-pane layout to tile the full surface below the tab strip.
    pub fn surface_size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Override the grid padding (inset) with an explicit physical-pixel value,
    /// preserved across runtime font resizes. The caller must recompute the grid.
    pub fn set_pad(&mut self, pad: f32) {
        let pad = pad.max(0.0);
        self.pad_override = Some(pad);
        self.pad = pad;
    }

    /// Set per-side padding overrides at runtime (physical px). When set, these
    /// override the uniform `pad` for their respective sides during grid layout.
    pub fn set_pad_top(&mut self, pad: f32) {
        self.pad_top = Some(pad.max(0.0));
    }

    pub fn set_pad_bottom(&mut self, pad: f32) {
        self.pad_bottom = Some(pad.max(0.0));
    }

    pub fn set_pad_left(&mut self, pad: f32) {
        self.pad_left = Some(pad.max(0.0));
    }

    pub fn set_pad_right(&mut self, pad: f32) {
        self.pad_right = Some(pad.max(0.0));
    }

    /// Set the window background opacity at runtime. Only has a visible effect on
    /// a transparent surface (the compositor must composite alpha); the caller
    /// should trigger a full redraw afterward so every cell background repaints.
    pub fn set_opacity(&mut self, opacity: f32) {
        self.opacity = opacity.clamp(0.0, 1.0);
    }

    /// Reload the font at a new physical pixel size, recomputing the cell metrics
    /// and padding and rebuilding the glyph atlas. On failure the previous font
    /// is retained (the error is logged) so a bad size never breaks rendering.
    ///
    /// The caller is responsible for re-querying `cell_metrics()`/`pad()` and
    /// resizing the PTY grid afterward (the renderer does not know the grid).
    pub fn set_font_size(&mut self, font_px: f32) {
        let font_px = font_px.clamp(4.0, 300.0);
        if (font_px - self.font_px).abs() < 0.01 {
            return;
        }
        let (text, metrics) =
            match Text::load(self.font_family.as_deref(), font_px, &self.font_features) {
                Ok(loaded) => loaded,
                Err(e) => {
                    log::warn!("glassy: font resize to {font_px:.1}px failed: {e:#}");
                    return;
                }
            };
        self.text = text;
        self.metrics = metrics;
        self.pad = self.pad_override.unwrap_or_else(|| pad_for(metrics.height));
        self.font_px = font_px;

        // The atlases hold glyphs rasterized at the old size; reset both packers
        // and caches so glyphs are re-rasterized at the new size on demand.
        self.packer.reset();
        self.color_packer.reset();
        self.glyph_cache.clear();
        self.cluster_cache.clear();
        self.ligature_run_cache.clear();
        self.wide_char_set.clear();

        // Re-probe ligature support: the new font file might be different from
        // the previous size's file (unlikely but possible after GLASSY_FONT change).
        self.font_has_ligatures = self.text.has_ligatures();

        // Pre-warm printable ASCII (regular + bold) at the new size for a
        // rasterize-free first frame after a font-size change.
        for byte in 0x20u8..=0x7E {
            self.ensure_glyphs(byte as char, false, false);
            self.ensure_glyphs(byte as char, true, false);
        }
    }

    /// Take the "atlas was repacked" flag (clearing it). When true, the caller
    /// should force a full row rebuild + repaint so no row keeps stale glyph UVs.
    pub fn pull_atlas_reset(&mut self) -> bool {
        std::mem::take(&mut self.atlas_reset)
    }

    /// Size the persistent per-row instance storage to `rows` grid rows, clearing
    /// every row. The app calls this whenever the grid height changes (resize /
    /// font resize) so subsequent per-row rebuilds index a correctly-sized table.
    /// Forces a full re-upload on the next `end_frame` (offsets are invalidated).
    pub fn resize_grid(&mut self, rows: usize) {
        // Drop or grow to exactly `rows` entries, clearing all so a fresh full
        // rebuild populates them. `RowInstances::default` is empty.
        self.rows.clear();
        self.rows.resize_with(rows, RowInstances::default);
        // Offsets no longer describe the buffer; clearing them forces end_frame to
        // reflatten and reupload everything.
        self.bg_row_offsets.clear();
        self.fg_row_offsets.clear();
        self.dirty_rows.clear();
    }

    /// Begin a frame: set the clear color. Unlike a full rebuild this does NOT
    /// clear the per-row instance storage — only the rows the app re-pushes via
    /// [`Renderer::begin_row`] are rewritten; the rest are reused from last frame.
    pub fn begin_frame(&mut self, default_bg: [f32; 4]) {
        // The clear color paints the (translucent) window backdrop, so it takes
        // the window opacity (and the visual-bell flash) just like the per-cell
        // default-background quads.
        self.clear_color = self.glass_bg(default_bg);
        // Image quads are not damage-tracked; they are rebuilt from live
        // placements every frame, so clear last frame's overlay here.
        self.image_overlay.clear();
        // Panel overlay (modals / menus) is likewise rebuilt every frame.
        self.overlay_quads.clear();
        self.overlay_text.clear();
    }

    /// Begin (re)building grid row `row`: subsequent `push_cell`/`push_cursor`
    /// calls land in this row's instance storage, replacing its previous contents.
    /// Out-of-range rows are ignored (clamped to a scratch slot) so a stale cursor
    /// row past a shrink never panics.
    pub fn begin_row(&mut self, row: usize) {
        if row >= self.rows.len() {
            // Should not happen if the app keeps the grid in sync, but stay safe:
            // grow so the index is valid rather than panicking mid-frame.
            self.rows.resize_with(row + 1, RowInstances::default);
            self.bg_row_offsets.clear();
            self.fg_row_offsets.clear();
        }
        self.cur_row = row;
        self.rows[row].bg.clear();
        self.rows[row].fg.clear();
        self.dirty_rows.push(row);
    }

    /// Re-target pushes at an already-built row WITHOUT clearing it, so callers can
    /// append to a row after its cells were laid down (the cursor overlay is pushed
    /// this way so it lands on top of the cursor row's cell backgrounds). The row
    /// must already have been built via [`Renderer::begin_row`] this frame.
    pub fn set_cur_row(&mut self, row: usize) {
        if row < self.rows.len() {
            self.cur_row = row;
        }
    }

    /// Apply the window opacity to a cell-background color.
    ///
    /// For `PreMultiplied` surfaces (Vulkan/Linux) RGB is scaled by alpha so the
    /// compositor blends `src_rgb + dst * (1 - src_alpha)` directly.
    /// For `PostMultiplied` surfaces (Metal/macOS) the compositor premultiplies
    /// internally, so we output straight alpha `[R, G, B, opacity]` — premultiplying
    /// the RGB here would cause the compositor to double-multiply them.
    /// A no-op (fully opaque) when the compositor can't composite alpha at all.
    pub(crate) fn glass_bg(&self, color: [f32; 4]) -> [f32; 4] {
        let color = self.apply_flash(color);
        if !self.transparent {
            return color;
        }
        let a = color[3] * self.opacity;
        if self.premultiplied_surface {
            [color[0] * a, color[1] * a, color[2] * a, a]
        } else {
            [color[0], color[1], color[2], a]
        }
    }

    /// Blend the active visual-bell flash (straight RGBA over) onto a straight
    /// (non-premultiplied) background color, preserving its alpha. A no-op when no
    /// flash is active. Applied to cell backgrounds and the clear color so the
    /// whole window tints uniformly toward the flash color for the flash window.
    pub(crate) fn apply_flash(&self, color: [f32; 4]) -> [f32; 4] {
        match self.flash {
            None => color,
            Some([fr, fg, fb, fa]) => [
                color[0] + (fr - color[0]) * fa,
                color[1] + (fg - color[1]) * fa,
                color[2] + (fb - color[2]) * fa,
                color[3],
            ],
        }
    }

    /// Set (or clear) the visual-bell flash overlay color. `Some([r, g, b, a])`
    /// blends that straight RGBA over the background for the next frames; `None`
    /// restores the normal appearance. The caller drives the flash duration.
    pub fn set_flash(&mut self, flash: Option<[f32; 4]>) {
        self.flash = flash;
    }

    /// Save the wgpu pipeline cache to disk. Call once before the application
    /// exits so the next launch can skip shader compilation. Failures are logged
    /// but never fatal. On backends that don't support `PIPELINE_CACHE` this is
    /// a no-op.
    pub fn save_pipeline_cache(&self) {
        if let Some(cache) = &self.pipeline_cache {
            save_pipeline_cache(cache, &self.adapter_info);
        }
    }

    /// Draw a thin scrollbar thumb in the right gutter of the terminal grid.
    ///
    /// `scroll_off` is the current scrollback offset (0 = at bottom, positive =
    /// scrolled up by that many rows).  `scroll_hist` is the total history rows
    /// available. `visible_rows` is the number of rows currently on screen.
    /// `surface_h` is the surface height in physical pixels.
    ///
    /// The thumb is painted via `push_overlay_px` (premultiplied-blend overlay
    /// pipeline) so it composites over the terminal content without touching the
    /// cell data.  It is 2 px wide in the right-most gutter, invisible (no-op)
    /// when there is nothing to scroll.
    #[allow(dead_code)] // public API; wired in by app.rs in a subsequent wave
    pub fn push_scrollbar_thumb(
        &mut self,
        scroll_off: usize,
        scroll_hist: usize,
        visible_rows: usize,
        surface_h: f32,
        color: [f32; 4],
    ) {
        let total = visible_rows + scroll_hist;
        if total <= visible_rows || surface_h <= 0.0 {
            return;
        }
        let track_h = surface_h;
        let thumb_h = (track_h * visible_rows as f32 / total as f32)
            .round()
            .max(4.0)
            .min(track_h);
        // scroll_off=0 → thumb at bottom; scroll_off=scroll_hist → thumb at top.
        let max_off = scroll_hist as f32;
        let thumb_y = if scroll_hist == 0 {
            track_h - thumb_h
        } else {
            let frac = scroll_off as f32 / max_off;
            (track_h - thumb_h) * (1.0 - frac)
        };
        let sw = self.config.width as f32;
        let thumb_w = 2.0_f32.max(1.0);
        let x = sw - thumb_w - 1.0; // 1 px margin from window edge
        let y = thumb_y.max(0.0).min(track_h - thumb_h);
        self.push_overlay_px(x, y, thumb_w, thumb_h, color);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push_cell(
        &mut self,
        col: usize,
        row: usize,
        ch: char,
        combiners: &[char],
        fg: [f32; 4],
        bg: [f32; 4],
        bold: bool,
        italic: bool,
        wide: bool,
        decorations: Decorations,
    ) {
        let cell_w = self.metrics.width;
        let cell_h = self.metrics.height;
        let pad = self.pad;
        let pad_l = self.pad_left.unwrap_or(pad);
        // Grid origin of this cell, offset by the window padding (inset) and the
        // GUI tab-bar inset (`grid_origin_y`, zero in the multi-pane path where
        // each pane carries its own pixel origin).
        let origin_x = col as f32 * cell_w + pad_l + self.grid_origin_x;
        let origin_y = row as f32 * cell_h + pad + self.grid_origin_y;

        // A double-width (CJK / wide-emoji) cell occupies two columns: its advance
        // box spans `2 * cell_w`. The grid skips the trailing spacer cell, so we
        // lay the glyph out across the full two-cell box here. Single-width cells
        // keep the ordinary one-cell box.
        let mut box_w = if wide { cell_w * 2.0 } else { cell_w };

        // Push the cell background, but skip cells whose background equals the
        // frame's clear color — the clear already paints those, so emitting a quad
        // for every default cell is pure overdraw (the common case is most of the
        // grid). Decorations and procedural box/block segments are separate
        // instances pushed afterward, so they are unaffected by the skip. A wide
        // cell's background spans both columns. Backgrounds take the window opacity
        // (premultiplied) so the desktop shows through uniformly.
        let glass = self.glass_bg(bg);
        if glass != self.clear_color {
            self.rows[self.cur_row].bg.push(BgInstance {
                pos: [origin_x, origin_y],
                size: [box_w, cell_h],
                color: glass,
            });
        }

        // Underline / strikethrough strokes span the full cell width so they join
        // seamlessly across adjacent decorated cells. Pushed here (after the cell
        // background, in the bg pass) so they paint over the background in the
        // decoration color; glyphs draw on top in the later fg pass. The curly
        // underline is emitted as a foreground decoration instance instead.
        self.draw_decorations(origin_x, origin_y, decorations);

        let baseline = origin_y + self.metrics.ascent;

        // Grapheme cluster path: a base char with combining marks / ZWJ-joined
        // codepoints (e.g. compound emoji like the trans flag). Shape the whole
        // cluster as one unit so it resolves to its single combined glyph.
        if !combiners.is_empty() {
            let mut cluster = String::with_capacity(ch.len_utf8() + combiners.len() * 4);
            cluster.push(ch);
            cluster.extend(combiners.iter());
            self.ensure_cluster_glyphs(&cluster, bold, italic);
            if let Some(glyphs) = self.cluster_cache.get(&(cluster, bold, italic)) {
                let cur = self.cur_row;
                Self::place_glyphs(
                    &mut self.rows[cur].fg,
                    glyphs,
                    fg,
                    origin_x,
                    baseline,
                    cell_w,
                    box_w,
                    cell_h,
                    self.metrics.ascent,
                );
            }
            return;
        }

        if ch == ' ' || ch == '\0' {
            return;
        }

        // PROCEDURAL box-drawing / block elements. Drawing these as font glyphs
        // leaves hairline gaps between adjacent cells (lines fail to connect), so
        // we paint them as solid foreground rectangles that span the full cell.
        // These quads are pushed AFTER this cell's background quad, so in the
        // painter-order bg pass they draw on top of it in the foreground color.
        let cp = ch as u32;
        let is_box = (0x2500..=0x257F).contains(&cp);
        let is_block = (0x2580..=0x259F).contains(&cp);
        // Powerline separator glyphs (Private Use Area, E0B0–E0B3). Render these
        // procedurally so they are always crisp even without a patched font.
        // NOTE: E0B0/E0B2 fill the whole cell with two colored regions (fg + bg),
        // so we must NOT push the cell background quad before this check. However,
        // the background was already pushed unconditionally above. The `draw_powerline`
        // method handles the full cell paint (bg region included) internally, so
        // the pre-pushed background quad underneath is invisible (same color or
        // overdrawn by our scanlines). This is correct: the bg quads are opaque.
        let is_powerline = matches!(cp, 0xE0B0..=0xE0B3);
        if is_block {
            self.draw_block(ch, origin_x, origin_y, fg, bg);
            return;
        }
        if is_box {
            // `draw_box` returns false for code points it does not handle, in
            // which case we fall through to the normal glyph path so nothing
            // renders blank.
            if self.draw_box(ch, origin_x, origin_y, fg) {
                return;
            }
        }
        if is_powerline && self.draw_powerline(ch, origin_x, origin_y, fg, bg) {
            return;
        }

        self.ensure_glyphs(ch, bold, italic);

        // Nerd-font wide-icon promotion: if the shaper measured this glyph's
        // advance as > 1.1× cell_w, and the cell was not already promoted to
        // wide by alacritty's WIDE_CHAR flag, use a 2-cell box so the icon
        // doesn't clip its right edge. This only applies to single-cell glyphs
        // (combiners and wide-char spacers are handled above); we also skip it
        // when the cell is already wide (would double-promote).
        if !wide && self.wide_char_set.contains(&(ch, bold, italic)) {
            box_w = cell_w * 2.0;
        }

        if let Some(glyphs) = self.glyph_cache.get(&(ch, bold, italic)) {
            let cur = self.cur_row;
            Self::place_glyphs(
                &mut self.rows[cur].fg,
                glyphs,
                fg,
                origin_x,
                baseline,
                cell_w,
                box_w,
                cell_h,
                self.metrics.ascent,
            );
        }
    }

    /// Render a ligature-shaped multi-cell run. `cells` is a slice of
    /// `(col, ch, fg, bg, bold, italic, wide, decorations)` for each cell in the
    /// run, all belonging to the same `row`. The run has already been shaped by
    /// `ensure_run_glyphs`; this method looks up the per-cell atlas entries and
    /// pushes instances, distributing the shaped output glyphs to the correct
    /// cell origins.
    ///
    /// Cells whose atlas slot is empty (ligature continuation cells, or cells with
    /// no drawable coverage) still get their background and decorations pushed.
    #[allow(clippy::too_many_arguments)]
    pub fn push_ligature_run(
        &mut self,
        row: usize,
        run_text: &str,
        cells: &[LigatureCell],
        bold: bool,
        italic: bool,
    ) {
        self.ensure_run_glyphs(run_text, bold, italic);

        let cell_w = self.metrics.width;
        let cell_h = self.metrics.height;
        let pad = self.pad;

        // Snapshot the per-cell atlas entries so we can release the borrow before
        // the mutable push loop below. Runs are short (typically 2-8 cells) so the
        // clone is cheap.
        let cached: Vec<Vec<AtlasGlyph>> = self
            .ligature_run_cache
            .get(&(run_text.to_string(), bold, italic))
            .cloned()
            .unwrap_or_default();

        let run_len = cells.len().min(cached.len());

        for i in 0..run_len {
            let cell = &cells[i];
            let origin_x = cell.col as f32 * cell_w + pad + self.grid_origin_x;
            let origin_y = row as f32 * cell_h + pad + self.grid_origin_y;
            let box_w = if cell.wide { cell_w * 2.0 } else { cell_w };

            // Push the cell background.
            let glass = self.glass_bg(cell.bg);
            if glass != self.clear_color {
                self.rows[self.cur_row].bg.push(BgInstance {
                    pos: [origin_x, origin_y],
                    size: [box_w, cell_h],
                    color: glass,
                });
            }

            // Decorations (underline, strikethrough).
            self.draw_decorations(origin_x, origin_y, cell.decorations);

            let baseline = origin_y + self.metrics.ascent;
            let glyphs = &cached[i];
            if !glyphs.is_empty() {
                let cur = self.cur_row;
                Self::place_glyphs(
                    &mut self.rows[cur].fg,
                    glyphs,
                    cell.fg,
                    origin_x,
                    baseline,
                    cell_w,
                    box_w,
                    cell_h,
                    self.metrics.ascent,
                );
            }
            // Continuation cells (empty glyph slot): background + decorations
            // already pushed above; no glyph instance needed.
        }
    }

    /// Compute the on-screen quad placement (`pos`, `size`) for each atlas glyph
    /// of a cell, given the cell's pen `origin_x`, text `baseline`, the advance
    /// box width `box_w` (one cell, or two for a double-width cell), and the cell
    /// height `cell_h`.
    ///
    /// The natural placement anchors a glyph at the single cell origin via its
    /// left/top bearings (`origin_x + left`, `baseline - top`). That is correct
    /// for a one-cell box. For a double-width box (`box_w == 2 * cell_w`) the
    /// glyph should instead sit centered across both cells:
    ///
    ///   * Mask glyphs (CJK text, monochrome emoji) keep their rasterized size and
    ///     their font bearings, plus a horizontal shift of `(box_w - cell_w) / 2`
    ///     that recenters the single-cell-anchored glyph in the two-cell box. For
    ///     a single-width cell that shift is zero, so ordinary text is unchanged.
    ///   * Color emoji are scaled to fit the box height (preserving aspect, capped
    ///     to the box width) and then centered both horizontally and vertically in
    ///     the box, so a square emoji bitmap fills its box without overflowing a
    ///     neighbour or clipping at the top/bottom.
    ///
    /// `cell_w` is the single-cell advance; `box_w` is this cell's advance box.
    /// `fg` is the glyph color. Pushes one [`FgInstance`] per atlas glyph directly
    /// into `out` (no per-cell temporary allocation — this runs for nearly every
    /// rebuilt cell, so the hot path stays alloc-free).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn place_glyphs(
        out: &mut Vec<FgInstance>,
        glyphs: &[AtlasGlyph],
        fg: [f32; 4],
        origin_x: f32,
        baseline: f32,
        cell_w: f32,
        box_w: f32,
        cell_h: f32,
        ascent: f32,
    ) {
        // Horizontal recentering for a wide box (0 for a single-width cell).
        let center_dx = (box_w - cell_w) * 0.5;
        // Top of the cell in the same Y-down coordinate space as baseline.
        let origin_y = baseline - ascent;
        for g in glyphs {
            let (pos, size) = if g.is_color {
                // Color emoji: scale to fit the box height-first (preserving
                // aspect), capped to the box width, then center in the cell.
                let scale = if g.px_h > 0.0 {
                    let s = cell_h / g.px_h;
                    if g.px_w * s > box_w && g.px_w > 0.0 {
                        box_w / g.px_w
                    } else {
                        s
                    }
                } else {
                    1.0
                };
                let w = g.px_w * scale;
                let h = g.px_h * scale;
                let x = origin_x + (box_w - w) * 0.5;
                // Center vertically within the cell (origin_y is cell top).
                // The old formula `baseline - cell_h + …` went above origin_y
                // when ascent < cell_h (e.g. 28/36), raising emoji above midline.
                let y = origin_y + (cell_h - h) * 0.5;
                ([x, y], [w, h])
            } else {
                // Mask glyph: keep its size and bearings; shift right to recenter
                // the single-cell-anchored glyph in the box.
                let x = origin_x + g.left as f32 + center_dx;
                let y = baseline - g.top as f32;
                ([x, y], [g.px_w, g.px_h])
            };
            out.push(FgInstance {
                pos,
                size,
                uv_min: g.uv_min,
                uv_max: g.uv_max,
                color: fg,
                flags: if g.is_color { 1 } else { 0 },
                _pad: [0; 3],
            });
        }
    }
}
