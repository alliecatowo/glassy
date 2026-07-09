//! Small standalone overlay painters with no supporting data tables: the
//! inline tab-rename editor and the confirm-close modal. Split out of the
//! former flat `chrome.rs` (settings-modularity stream) — see `super`'s
//! module doc.

use super::*;

/// Result returned from [`App::paint_confirm_close`] each frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmCloseResult {
    /// The modal is still open (user has not interacted yet).
    Pending,
    /// The user clicked "Close" — proceed with the close.
    Confirm,
    /// The user clicked "Cancel" — abort the close.
    Cancel,
}

impl App {
    /// Paint the inline tab-rename editor over the chip rect `r`: an opaque raised
    /// field with an accent ring, the in-progress `buffer` text (h-scrolled to keep
    /// the caret visible), the selection band, and the caret at its real column.
    /// `caret`/`selection` are char offsets into `buffer`. Associated (no `&self`)
    /// so it composes with the caller's `&mut Renderer` borrow.
    pub(crate) fn paint_tab_rename(
        renderer: &mut Renderer,
        r: gui::Rect,
        buffer: &str,
        caret: usize,
        selection: Option<(usize, usize)>,
    ) {
        let m = renderer.cell_metrics();
        let cell_w = m.width;
        let cell_h = m.height;
        let radius = gui_radius(cell_h);

        // Opaque field surface so the chip text underneath never shows through.
        renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, radius, gui::glass_float());
        // Accent focus ring (1px, softened): outer accent rrect minus an inset
        // surface rrect — a gentle halo rather than a harsh bright outline.
        let ring = {
            let a = color::accent();
            [a[0], a[1], a[2], 0.55]
        };
        renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, radius, ring);
        let inset = 1.0;
        if r.w > 2.0 * inset && r.h > 2.0 * inset {
            renderer.push_overlay_rrect_px(
                r.x + inset,
                r.y + inset,
                r.w - 2.0 * inset,
                r.h - 2.0 * inset,
                (radius - inset).max(0.0),
                gui::glass_float(),
            );
        }

        // Text area: pad in, reserve one cell for the caret. H-scroll a window so
        // the caret stays visible (matching the shared text-field model).
        let pad = (cell_w * 0.6).round();
        let ty = (r.center_y() - cell_h * 0.5).round();
        let text_x0 = r.x + pad;
        let text_w = (r.w - 2.0 * pad - cell_w).max(0.0);
        let max_chars = (text_w / cell_w).floor().max(0.0) as usize;
        let chars: Vec<char> = buffer.chars().collect();
        // First visible char so the caret column lands inside the window.
        let scroll = if max_chars == 0 || caret < max_chars {
            0
        } else {
            caret + 1 - max_chars
        };
        let end = (scroll + max_chars).min(chars.len());

        // Selection band behind the glyphs, clipped to the visible window.
        if let Some((lo, hi)) = selection {
            let vlo = lo.max(scroll);
            let vhi = hi.min(end);
            if vhi > vlo {
                let sx = text_x0 + (vlo - scroll) as f32 * cell_w;
                let sw = (vhi - vlo) as f32 * cell_w;
                let mut band = color::selection_bg();
                band[3] = 0.45;
                renderer.push_overlay_px(sx.round(), ty, sw.round(), cell_h, band);
            }
        }

        let mut cx = text_x0;
        for &ch in &chars[scroll..end] {
            renderer.push_overlay_glyph_px(cx.round(), ty, ch, gui::fg());
            cx += cell_w;
        }
        // Caret at its real column within the visible window.
        let caret_col = caret.clamp(scroll, end);
        let caret_x = text_x0 + (caret_col - scroll) as f32 * cell_w;
        renderer.push_overlay_px(caret_x.round(), ty, 2.0, cell_h, color::accent());
    }

    /// Paint the confirm-close modal: a centered frosted glass card asking the user
    /// to confirm closing when a process is still running in the tab/pane. Returns
    /// the interaction result so the caller can decide whether to proceed, cancel,
    /// or wait for a button click.
    ///
    /// Layout:
    ///   ┌─────────────────────────────────────┐
    ///   │  A process is still running.        │
    ///   │  Close this tab anyway?             │
    ///   │                                     │
    ///   │       [Cancel]      [Close]         │
    ///   └─────────────────────────────────────┘
    ///
    /// The "Close" button uses the danger color; "Cancel" is the neutral surface.
    /// Clicking outside the card is treated as Cancel. Static (no `&self`) so it
    /// composes with the caller's live `&mut Renderer` borrow.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_confirm_close(
        renderer: &mut Renderer,
        surface: (f32, f32),
        cell_w: f32,
        cell_h: f32,
        mouse: (f32, f32),
        mouse_down: bool,
        click: bool,
        gui_pressed: &mut Option<gui::WidgetId>,
        gui_anims: &mut std::collections::HashMap<gui::WidgetId, gui::Anim>,
    ) -> ConfirmCloseResult {
        let (sw, sh) = surface;

        // Full-screen dimming scrim.
        renderer.push_overlay_px(0.0, 0.0, sw, sh, [0.0, 0.0, 0.0, 0.45]);

        // Card dimensions: wide enough for two lines + buttons.
        let card_w = (cell_w * 38.0).clamp(280.0, sw * 0.9);
        let card_h = cell_h * 7.0;
        let card_x = ((sw - card_w) * 0.5).round();
        let card_y = ((sh - card_h) * 0.5).round();
        let radius = gui_radius(cell_h);

        // Glass card background.
        renderer.push_overlay_rrect_px(card_x, card_y, card_w, card_h, radius, gui::glass_float());
        // Subtle border.
        let border = {
            let h = gui::hairline();
            [h[0], h[1], h[2], h[3] * 1.5]
        };
        renderer.push_overlay_rrect_px(
            card_x - 0.5,
            card_y - 0.5,
            card_w + 1.0,
            card_h + 1.0,
            radius + 0.5,
            border,
        );
        // Repaint inside to restore the card surface (border is painted over).
        renderer.push_overlay_rrect_px(card_x, card_y, card_w, card_h, radius, gui::glass_float());

        // Body text: two lines.
        let line1 = "A process is still running.";
        let line2 = "Close this tab anyway?";
        let tx = (card_x + cell_w).round();
        let ty1 = (card_y + cell_h).round();
        let ty2 = (ty1 + cell_h * 1.5).round();
        renderer.push_overlay_glyph_px_str(tx, ty1, line1, gui::fg());
        renderer.push_overlay_glyph_px_str(tx, ty2, line2, gui::fg_dim());

        // Button row: Cancel (left) and Close/danger (right), bottom of card.
        let btn_w = (cell_w * 8.0).round();
        let btn_h = (cell_h * 1.6).round();
        let btn_pad = cell_w;
        let btn_y = (card_y + card_h - btn_h - cell_h * 0.75).round();

        // Cancel button (left-of-center).
        let cancel_id = gui::id("confirm_close/cancel");
        let cancel_x = (card_x + card_w * 0.5 - btn_w - btn_pad * 0.5).round();
        let cancel_r = gui::Rect::new(cancel_x, btn_y, btn_w, btn_h);
        let cancel_hover = gui::hit(cancel_r, mouse.0, mouse.1);
        let cancel_held = cancel_hover && *gui_pressed == Some(cancel_id) && mouse_down;
        let cancel_bg = gui::state_fill(
            gui::glass_raised(),
            if cancel_hover { 0.7 } else { 0.0 },
            cancel_held,
        );
        renderer.push_overlay_rrect_px(
            cancel_r.x, cancel_r.y, cancel_r.w, cancel_r.h, radius, cancel_bg,
        );
        let cancel_label = "Cancel";
        let lw = cancel_label.chars().count() as f32 * cell_w;
        let ltx = (cancel_r.x + (cancel_r.w - lw) * 0.5).round();
        let lty = (cancel_r.center_y() - cell_h * 0.5).round();
        renderer.push_overlay_glyph_px_str(ltx, lty, cancel_label, gui::fg());

        // Close (danger) button (right-of-center).
        let close_id = gui::id("confirm_close/close");
        let close_x = (card_x + card_w * 0.5 + btn_pad * 0.5).round();
        let close_r = gui::Rect::new(close_x, btn_y, btn_w, btn_h);
        let close_hover = gui::hit(close_r, mouse.0, mouse.1);
        let close_held = close_hover && *gui_pressed == Some(close_id) && mouse_down;
        let danger = color::danger();
        let close_bg_base = [danger[0], danger[1], danger[2], 0.85];
        let close_bg = gui::state_fill(
            close_bg_base,
            if close_hover { 0.7 } else { 0.0 },
            close_held,
        );
        renderer
            .push_overlay_rrect_px(close_r.x, close_r.y, close_r.w, close_r.h, radius, close_bg);
        let close_label = "Close";
        let clw = close_label.chars().count() as f32 * cell_w;
        let cltx = (close_r.x + (close_r.w - clw) * 0.5).round();
        let clty = (close_r.center_y() - cell_h * 0.5).round();
        // Danger button uses contrasting text.
        let dluma = color::luma(danger);
        let close_fg = if dluma > 0.4 {
            [0.06, 0.06, 0.07, 1.0]
        } else {
            [0.97, 0.97, 0.98, 1.0]
        };
        renderer.push_overlay_glyph_px_str(cltx, clty, close_label, close_fg);

        // Track press latching.
        if mouse_down {
            if cancel_hover && gui_pressed.is_none() {
                *gui_pressed = Some(cancel_id);
            } else if close_hover && gui_pressed.is_none() {
                *gui_pressed = Some(close_id);
            }
        }

        // Settle anims (no per-button animation needed; just suppress the entry).
        let _ = gui_anims;

        // Resolve a click.
        if click {
            let pressed_id = *gui_pressed;
            if pressed_id == Some(cancel_id) && cancel_hover {
                return ConfirmCloseResult::Cancel;
            }
            if pressed_id == Some(close_id) && close_hover {
                return ConfirmCloseResult::Confirm;
            }
            // Click outside the card: treat as Cancel.
            if !gui::hit(
                gui::Rect::new(card_x, card_y, card_w, card_h),
                mouse.0,
                mouse.1,
            ) {
                return ConfirmCloseResult::Cancel;
            }
        }

        ConfirmCloseResult::Pending
    }
}
