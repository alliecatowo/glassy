//! Overlay push methods: quads, glyphs, cursor, decorations.

use super::*;

impl Renderer {
    /// Queue a translucent overlay quad (panel body, dim backdrop, border rail) in
    /// PIXEL coordinates. `color` is straight RGBA; it is premultiplied here so the
    /// overlay pipeline (premultiplied blend) composites it over the terminal.
    /// Drawn after the grid + images, so it always lands on top.
    ///
    /// INVARIANT: callers (modal/menu paint) rely on the terminal grid under the
    /// panel having been freshly painted this frame. Every modal/menu open+close
    /// path sets `App::force_full_redraw`, which repaints all rows — so the terminal
    /// pixels are resident before these quads composite over them. New panel paths
    /// must keep that contract or the area under the glass may show stale content.
    pub fn push_overlay_px(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let a = color[3];
        self.overlay_quads.push(BgInstance {
            pos: [x, y],
            size: [w, h],
            color: [color[0] * a, color[1] * a, color[2] * a, a],
        });
    }

    /// Cell-rect convenience for overlay quads: covers `cols` x `rows` cells from
    /// cell origin (`col`,`row`). Matches `push_cell`'s pixel math.
    #[allow(dead_code)]
    pub fn push_overlay_cells(
        &mut self,
        col: usize,
        row: usize,
        cols: usize,
        rows: usize,
        color: [f32; 4],
    ) {
        let cw = self.metrics.width;
        let ch = self.metrics.height;
        let pad = self.pad;
        let x = col as f32 * cw + pad;
        let y = row as f32 * ch + pad;
        self.push_overlay_px(x, y, cols as f32 * cw, rows as f32 * ch, color);
    }

    /// Push a single panel glyph at cell (`col`,`row`) in color `fg`, into the
    /// text-on-glass channel so it draws AFTER the overlay quads (and stays crisp
    /// over the glass body). Mirrors `push_cell`'s ordinary glyph path; box/block
    /// procedural drawing and decorations are intentionally omitted (panel text is
    /// plain labels). No background quad is emitted (the glass shows through).
    #[allow(dead_code)]
    pub fn push_overlay_glyph(&mut self, col: usize, row: usize, ch: char, fg: [f32; 4]) {
        if ch == ' ' || ch == '\0' {
            return;
        }
        let cell_w = self.metrics.width;
        let box_w = cell_w;
        let cell_h = self.metrics.height;
        let pad = self.pad;
        let origin_x = col as f32 * cell_w + pad;
        let origin_y = row as f32 * cell_h + pad;
        let baseline = origin_y + self.metrics.ascent;
        self.ensure_glyphs(ch, false, false);
        if let Some(glyphs) = self.glyph_cache.get(&(ch, false, false)) {
            Self::place_glyphs(
                &mut self.overlay_text,
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

    /// Queue a rounded-rectangle overlay fill in PIXEL coordinates. Emits ONE
    /// [`FgInstance`] with `flags == 3` into the text-on-glass channel (so it
    /// composites above `push_overlay_px` quads, like overlay glyphs). The shader
    /// draws an antialiased SDF rounded rect; `radius` is the corner radius in px
    /// (clamped to the box in the shader, so 0 = sharp rect). `color` is straight
    /// RGBA; the shader premultiplies. This is the GUI layer's surface primitive.
    pub fn push_overlay_rrect_px(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: [f32; 4],
    ) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        // Atlas UVs are unused for flags==3; we smuggle the radius through uv_min.x
        // (see vs_fg), and set uv to a clean 0..1 local coord via uv_min/uv_max.
        self.overlay_text.push(FgInstance {
            pos: [x, y],
            size: [w, h],
            uv_min: [radius, 0.0],
            uv_max: [radius, 1.0],
            color,
            flags: 3,
            _pad: [0; 3],
        });
    }

    /// Queue a rounded-rectangle overlay fill with INDEPENDENT per-corner radii.
    /// `radii` is (top-left, top-right, bottom-right, bottom-left) in px. Emits one
    /// `FgInstance` with `flags == 4`; the four radii are smuggled through
    /// `uv_min`/`uv_max` (the atlas UVs are unused on this path — see vs_fg). This
    /// lets the active tab round only its top corners while keeping its bottom edge
    /// square and flush to the content seam, so the connector patch no longer leaks
    /// background through the rrect corner feather. A single radius via
    /// [`push_overlay_rrect_px`] remains the common path.
    pub fn push_overlay_rrect4_px(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radii: [f32; 4],
        color: [f32; 4],
    ) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        self.overlay_text.push(FgInstance {
            pos: [x, y],
            size: [w, h],
            uv_min: [radii[0], radii[1]],
            uv_max: [radii[2], radii[3]],
            color,
            flags: 4,
            _pad: [0; 3],
        });
    }

    /// Begin capturing the tab-bar's overlay instances. Records the current overlay
    /// list lengths so [`Renderer::commit_tab_overlay`] can snapshot exactly the
    /// instances the tab-bar painter pushed. The tab bar is always painted first
    /// (before menus/settings/status), so its captured region is a clean prefix.
    pub fn begin_tab_overlay(&mut self) {
        self.tab_overlay_mark = Some((self.overlay_quads.len(), self.overlay_text.len()));
    }

    /// Finish capturing the tab-bar overlay region: copy the instances pushed since
    /// [`Renderer::begin_tab_overlay`] into the persistent tab-overlay cache, so a
    /// later unchanged frame can replay them without re-shaping tab titles.
    pub fn commit_tab_overlay(&mut self) {
        let Some((q0, t0)) = self.tab_overlay_mark.take() else {
            return;
        };
        self.tab_overlay_quads.clear();
        self.tab_overlay_text.clear();
        self.tab_overlay_quads
            .extend_from_slice(&self.overlay_quads[q0..]);
        self.tab_overlay_text
            .extend_from_slice(&self.overlay_text[t0..]);
    }

    /// Replay the cached tab-bar overlay instances into this frame's overlay lists,
    /// skipping the (glyph-shaping) tab-bar painter. Used when nothing tab-relevant
    /// changed since the last rebuild. A no-op if the cache is empty (the App falls
    /// back to a full rebuild in that case).
    pub fn replay_tab_overlay(&mut self) {
        self.overlay_quads
            .extend_from_slice(&self.tab_overlay_quads);
        self.overlay_text.extend_from_slice(&self.tab_overlay_text);
    }

    /// Whether a tab-bar overlay snapshot is cached and available for replay.
    pub fn has_tab_overlay(&self) -> bool {
        !self.tab_overlay_quads.is_empty() || !self.tab_overlay_text.is_empty()
    }

    /// Push a single panel glyph at an arbitrary PIXEL position (top-left of the
    /// glyph's cell box), in color `fg`, into the text-on-glass channel. This is
    /// the pixel-positioned counterpart of [`Renderer::push_overlay_glyph`] — it
    /// frees chrome text from the cell grid so the GUI layer can place labels at
    /// any sub-cell coordinate. No background quad is emitted.
    pub fn push_overlay_glyph_px(&mut self, x: f32, y: f32, ch: char, fg: [f32; 4]) {
        if ch == ' ' || ch == '\0' {
            return;
        }
        let cell_w = self.metrics.width;
        let cell_h = self.metrics.height;
        let baseline = y + self.metrics.ascent;
        self.ensure_glyphs(ch, false, false);
        if let Some(glyphs) = self.glyph_cache.get(&(ch, false, false)) {
            Self::place_glyphs(
                &mut self.overlay_text,
                glyphs,
                fg,
                x,
                baseline,
                cell_w,
                cell_w,
                cell_h,
                self.metrics.ascent,
            );
        }
    }

    /// Draw a whole string starting at PIXEL `(x, y)` (top-left of the first
    /// glyph's cell box), one monospace cell advance per char, in color `fg`.
    /// Convenience over [`push_overlay_glyph_px`] for GUI labels.
    pub fn push_overlay_glyph_px_str(&mut self, x: f32, y: f32, s: &str, fg: [f32; 4]) {
        let cw = self.metrics.width;
        let mut cx = x;
        for ch in s.chars() {
            self.push_overlay_glyph_px(cx, y, ch, fg);
            cx += cw;
        }
    }

    /// Width in physical px that `s` occupies when drawn with the panel glyph
    /// path. The font is monospace, so this is exact: one cell advance per char.
    /// Used by the GUI layer for centering / right-alignment of labels.
    pub fn text_width_px(&self, s: &str) -> f32 {
        s.chars().count() as f32 * self.metrics.width
    }

    /// Reserve `px` physical pixels above the terminal grid for the GUI tab bar.
    /// Pass 0 to restore the legacy (no-chrome) layout. Added to every grid cell's
    /// (and the cursor's) pixel origin in [`Renderer::push_cell`]/[`push_cursor`],
    /// so the single-pane terminal starts below the tab bar without any cell-row
    /// reservation. The multi-pane path leaves this at 0 (each pane carries its own
    /// pixel origin via `begin_pane`).
    pub fn set_grid_origin_y(&mut self, px: f32) {
        self.grid_origin_y = px.max(0.0);
    }

    /// The current grid top inset in physical px (see [`set_grid_origin_y`]).
    pub fn grid_origin_y(&self) -> f32 {
        self.grid_origin_y
    }

    /// Set a transient horizontal shake offset (physical px) added to every grid
    /// cell + the cursor. Used by Power Mode's "screen rock"; pass 0 to reset (the
    /// default resting value). Chrome/overlays are unaffected — only the terminal
    /// content shifts, so the tab bar/status bar stay pinned during a shake.
    pub fn set_grid_origin_x(&mut self, px: f32) {
        self.grid_origin_x = px;
    }

    /// Push a single solid-color rectangle as a [`BgInstance`]. Coordinates are
    /// physical pixels. Because the bg pass draws instances in insertion order
    /// with no depth test, a quad pushed here after a cell's background quad
    /// paints on top of it — that is how procedural box/block segments land in
    /// the foreground color over the cell background.
    pub(crate) fn push_solid(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        // Defense-in-depth: `cur_row` is kept in range by `begin_row`/`set_cur_row`
        // (which clamp/self-heal), so this index is normally valid. Guard it anyway
        // so a future path that leaves `cur_row` stale (e.g. a row count change
        // between `resize_grid` and the next `begin_row`) degrades to a dropped quad
        // rather than an out-of-bounds panic — cheap insurance against a latent crash.
        let Some(row) = self.rows.get_mut(self.cur_row) else {
            return;
        };
        row.bg.push(BgInstance {
            pos: [x, y],
            size: [w, h],
            color,
        });
    }

    /// Paint a cursor overlay for the cell at `(col, row)` in the cursor color.
    /// Pushed via `push_solid` (bg pass) after every cell, so the bars/outline
    /// land on top of the cell background; glyphs still draw over them in the fg
    /// pass, keeping the character under the cursor legible.
    pub fn push_cursor(&mut self, col: usize, row: usize, overlay: CursorOverlay, color: [f32; 4]) {
        let cell_w = self.metrics.width;
        let cell_h = self.metrics.height;
        let ox = (col as f32 * cell_w + self.pad + self.grid_origin_x).round();
        let oy = (row as f32 * cell_h + self.pad + self.grid_origin_y).round();
        let w = cell_w.round();
        let h = cell_h.round();
        // Bar thickness for beam/underline and the outline rails.
        let th = (cell_h / 12.0).round().max(1.0);

        match overlay {
            CursorOverlay::Beam => self.push_solid(ox, oy, th, h, color),
            CursorOverlay::Underline => self.push_solid(ox, oy + h - th, w, th, color),
            CursorOverlay::Hollow => {
                self.push_solid(ox, oy, w, th, color); // top
                self.push_solid(ox, oy + h - th, w, th, color); // bottom
                self.push_solid(ox, oy, th, h, color); // left
                self.push_solid(ox + w - th, oy, th, h, color); // right
            }
        }
    }

    /// Paint a cell's underline + strikethrough strokes in the decoration color,
    /// using the font's recommended stroke positions and thickness from
    /// `CellMetrics`. Straight strokes are solid rectangles in the bg pass; the
    /// curly underline is a foreground decoration instance (procedural coverage).
    pub(crate) fn draw_decorations(&mut self, ox: f32, oy: f32, dec: Decorations) {
        if dec.underline == UnderlineStyle::None && !dec.strikeout {
            return;
        }
        let c = dec.color;
        let w = self.metrics.width;
        let th = self.metrics.decoration_thickness;
        let x = ox.round();
        let cell_h = self.metrics.height;

        if dec.strikeout {
            let y = (oy + self.metrics.strikeout_y).round();
            self.push_solid(x, y, w, th, c);
        }

        // Underline baseline y, clamped to stay inside the cell.
        let uy = ((oy + self.metrics.underline_y).round())
            .min(oy + cell_h - th)
            .max(oy);
        match dec.underline {
            UnderlineStyle::None => {}
            UnderlineStyle::Single => self.push_solid(x, uy, w, th, c),
            UnderlineStyle::Double => {
                // Two thin rails: the lower at the single-underline position, the
                // upper one stroke + gap above it, both kept inside the cell.
                let gap = th.max(1.0);
                let lower = uy;
                let upper = (lower - th - gap).max(oy);
                self.push_solid(x, upper, w, th, c);
                self.push_solid(x, lower, w, th, c);
            }
            UnderlineStyle::Dotted => {
                // ~2px dots with ~2px gaps along the underline row.
                let dot = th.max(1.0);
                let step = (dot * 2.0).max(2.0);
                let mut dx = x;
                while dx < x + w {
                    self.push_solid(dx, uy, dot, th, c);
                    dx += step;
                }
            }
            UnderlineStyle::Dashed => {
                // Three dashes per cell, ~70% on / 30% off, joining across cells.
                let slot = w / 3.0;
                let dash = (slot * 0.7).round().max(1.0);
                for i in 0..3 {
                    let dx = (x + slot * i as f32).round();
                    self.push_solid(dx, uy, dash, th, c);
                }
            }
            UnderlineStyle::Curl => {
                // A foreground decoration quad spanning the cell width, tall
                // enough for a visible wave; the fragment shader computes the
                // sine coverage. Center the band on the single-underline row.
                let band_h = (th * 3.0).max(4.0).min(cell_h - 1.0);
                let cy = uy + th * 0.5;
                let top = (cy - band_h * 0.5).max(oy).min(oy + cell_h - band_h);
                self.push_undercurl(x, top, w, band_h, c);
            }
        }
    }

    /// Push a foreground decoration instance for the curly underline. It carries
    /// no atlas glyph (uv is unused on the `flags == 2` path); the shader derives
    /// the wave from the quad's interpolated UV and pixel size.
    pub(crate) fn push_undercurl(&mut self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        // UV spans the full 0..1 quad so the shader can derive the local position
        // (and thus the sine wave) from the interpolated `uv` on the curl path.
        self.rows[self.cur_row].fg.push(FgInstance {
            pos: [x, y],
            size: [w, h],
            uv_min: [0.0, 0.0],
            uv_max: [1.0, 1.0],
            color,
            flags: 2,
            _pad: [0; 3],
        });
    }
}
