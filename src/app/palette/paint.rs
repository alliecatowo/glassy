//! Command-palette painting: the scrim + centered glass panel with the query
//! field and the fuzzy-filtered, scrollable result list. Split out of the parent
//! module to keep `palette/mod.rs` under the project's 700-line limit. The idle
//! invariant is unaffected — this only runs while the palette is open.

use super::super::*;
use super::PaletteSnapshot;

impl App {
    /// Snapshot the palette paint inputs: the query, the filtered rows as owned
    /// `(label, hint)` pairs, and the selected row. Returns `None` when closed.
    pub(crate) fn palette_snapshot(&self) -> Option<PaletteSnapshot> {
        let p = self.palette.as_ref()?;
        let rows: Vec<(String, Option<String>)> = p
            .filtered
            .iter()
            .filter_map(|&i| p.all.get(i))
            .map(|e| (e.display.clone(), e.hint.clone()))
            .collect();
        Some((p.query(), p.edit.caret(), p.edit.selection(), rows, p.sel))
    }

    /// Paint the command palette: a full-surface scrim, a centered glass panel
    /// with a query field at the top and a fuzzy-filtered, scrollable list below.
    /// The selected row is highlighted; the hovered row (under `mouse`) gets a
    /// lighter tint. Returns the `(filtered_index, row_rect)` list for the App to
    /// store for mouse hit-testing (the immediate-mode click is resolved in the
    /// mouse handler, mirroring the menu pattern).
    ///
    /// Associated fn (no `&self`) so it composes with the caller's `&mut Renderer`
    /// borrow; all `self`-derived data arrives via parameters.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_palette(
        renderer: &mut Renderer,
        surface: (f32, f32),
        query: &str,
        caret: usize,
        selection: Option<(usize, usize)>,
        rows: &[(&str, Option<&str>)],
        sel: usize,
        mouse: (f32, f32),
    ) -> Vec<(usize, gui::Rect)> {
        let cm = renderer.cell_metrics();
        let cell_w = cm.width;
        let cell_h = cm.height;
        // Reuse the shared design-system Metrics instead of re-deriving the same
        // pad/gap/radius/row-height formulas by hand, so the palette scales and
        // rounds exactly like the rest of the chrome.
        let m = gui::Metrics::new(cell_w, cell_h);
        let pad = m.pad;
        let gap = m.gap;
        let radius = m.radius;
        let row_h = m.row_h;

        // Full-surface scrim.
        renderer.push_overlay_px(0.0, 0.0, surface.0, surface.1, [0.0, 0.0, 0.0, 0.5]);

        // Centered panel. Width ~ 60 cols, capped to the surface.
        let pw = (cell_w * 60.0)
            .min(surface.0 - 2.0 * pad)
            .max(cell_w * 28.0);
        let field_h = row_h;
        // Show up to 12 list rows; the rest scroll into view around the selection.
        let max_visible = 12usize;
        let visible = rows.len().min(max_visible);
        let list_h = visible as f32 * row_h;
        let ph = (pad + field_h + gap + list_h + pad).round();
        let px = ((surface.0 - pw) * 0.5).round();
        // Anchor toward the upper third so the list grows downward like a palette.
        let py = ((surface.1 - ph) * 0.35).round().max(pad);
        let panel = gui::Rect::new(px, py, pw, ph);

        // Soft drop shadow under the floating panel (E3 depth), then the panel
        // body (E3 floating surface) + accent top rail.
        renderer.push_overlay_shadow_px(
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            m.r_lg,
            gui::SHADOW_E3_FEATHER,
            0.0,
            4.0,
            gui::shadow_e3(),
        );
        renderer.push_overlay_rrect_px(
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            m.r_lg,
            gui::glass_float(),
        );
        renderer.push_overlay_rrect_px(panel.x, panel.y, panel.w, 2.0, m.r_lg, gui::rail());

        let inner_x = panel.x + pad;
        let inner_w = panel.w - 2.0 * pad;

        // --- Query field --------------------------------------------------------
        let field = gui::Rect::new(inner_x, panel.y + pad, inner_w, field_h);
        // Theme-aware recessed track (gui::track_off()) instead of a flat black
        // fill, which on light themes painted an opaque black input box.
        renderer.push_overlay_rrect_px(
            field.x,
            field.y,
            field.w,
            field.h,
            radius,
            gui::track_off(),
        );
        let ty = (field.y + (field.h - cell_h) * 0.5).round();
        let mut cx = field.x + pad;
        // Leading prompt chevron.
        renderer.push_overlay_glyph_px(cx.round(), ty, '\u{203A}', color::accent());
        cx += cell_w * 1.6;
        // The query text starts after the chevron; used for caret + selection x.
        let text_x0 = field.x + pad + cell_w * 1.6;
        // Selection band behind the glyphs.
        if let Some((lo, hi)) = selection
            && hi > lo
        {
            let sx = text_x0 + lo as f32 * cell_w;
            let sw = (hi - lo) as f32 * cell_w;
            let mut band = color::selection_bg();
            band[3] = 0.45;
            renderer.push_overlay_px(sx.round(), ty, sw.round(), cell_h, band);
        }
        if query.is_empty() {
            for ch in "Type a command…".chars() {
                renderer.push_overlay_glyph_px(cx.round(), ty, ch, gui::fg_dim());
                cx += cell_w;
            }
        } else {
            for ch in query.chars() {
                renderer.push_overlay_glyph_px(cx.round(), ty, ch, gui::fg());
                cx += cell_w;
            }
        }
        let caret_x = text_x0 + caret as f32 * cell_w;
        renderer.push_overlay_px(caret_x.round(), ty, 2.0, cell_h, color::accent());

        // --- List ---------------------------------------------------------------
        let list_y = field.y + field.h + gap;
        // Scroll so the selection stays visible (keep a simple window).
        let first = if sel >= visible { sel + 1 - visible } else { 0 };
        let mut out = Vec::with_capacity(visible);
        for (slot, ri) in (first..rows.len()).take(visible).enumerate() {
            let (label, hint) = rows[ri];
            let ry = list_y + slot as f32 * row_h;
            let rr = gui::Rect::new(inner_x, ry, inner_w, row_h);
            let over = gui::hit(rr, mouse.0, mouse.1);
            if ri == sel {
                renderer.push_overlay_rrect_px(rr.x, rr.y, rr.w, rr.h, radius, gui::sel_bg());
            } else if over {
                let mut c = color::selection_bg();
                c[3] = 0.40;
                renderer.push_overlay_rrect_px(rr.x, rr.y, rr.w, rr.h, radius, c);
            }
            let lty = (rr.y + (rr.h - cell_h) * 0.5).round();
            // Label (left).
            let mut lx = rr.x + pad;
            for ch in label.chars() {
                renderer.push_overlay_glyph_px(lx.round(), lty, ch, gui::fg());
                lx += cell_w;
            }
            // Hint (right, dim).
            if let Some(h) = hint {
                let hw = h.chars().count() as f32 * cell_w;
                let mut hx = rr.x + rr.w - pad - hw;
                for ch in h.chars() {
                    renderer.push_overlay_glyph_px(hx.round(), lty, ch, gui::fg_dim());
                    hx += cell_w;
                }
            }
            out.push((ri, rr));
        }
        // "No matches" hint when the list is empty.
        if rows.is_empty() {
            let msg = "No matching commands";
            let mx = inner_x + pad;
            let mut cxn = mx;
            for ch in msg.chars() {
                renderer.push_overlay_glyph_px(
                    cxn.round(),
                    (list_y + (row_h - cell_h) * 0.5).round(),
                    ch,
                    gui::fg_dim(),
                );
                cxn += cell_w;
            }
        }
        out
    }
}
