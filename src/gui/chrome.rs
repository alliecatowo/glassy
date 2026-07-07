//! Chrome-layer widgets: settings panel, dropdowns, menus.

use super::*;

/// Mutable editing models for the settings form's editable text fields, threaded
/// into [`Ui::build_settings`] alongside the read-only [`SettingsView`]. Owned by
/// the App across frames so caret / selection / scroll survive between paints.
pub struct SettingsFields<'a> {
    /// "Word seps" field model + its mouse drag state.
    pub word_sep: &'a mut TextEdit,
    pub word_sep_ms: &'a mut TextInputMouse,
    /// "Font features" field model + its mouse drag state.
    pub font_feat: &'a mut TextEdit,
    pub font_feat_ms: &'a mut TextInputMouse,
    /// Current cursor-blink phase (reuse the app blink timer).
    pub blink_on: bool,
    /// True on the frame a double-click landed (for word-select in a field).
    pub double_click: bool,
    /// Hex input for the currently-edited custom-theme color (Themes section).
    pub theme_hex: &'a mut TextEdit,
    pub theme_hex_ms: &'a mut TextInputMouse,
    /// "New profile" name field model + its mouse drag state (Profiles section).
    pub profile_name: &'a mut TextEdit,
    pub profile_name_ms: &'a mut TextInputMouse,
    /// Inline "rename this profile" field model + drag state (Profiles section),
    /// active only for the row whose index matches `SettingsView::profile_rename_idx`.
    pub profile_rename: &'a mut TextEdit,
    pub profile_rename_ms: &'a mut TextInputMouse,
    /// Terminal section: "Hint chars" field model + drag state.
    pub hints_chars: &'a mut TextEdit,
    pub hints_chars_ms: &'a mut TextInputMouse,
    /// Terminal section: "Bold font" field model + drag state.
    pub font_bold: &'a mut TextEdit,
    pub font_bold_ms: &'a mut TextInputMouse,
    /// Terminal section: "Italic font" field model + drag state.
    pub font_italic: &'a mut TextEdit,
    pub font_italic_ms: &'a mut TextInputMouse,
    /// Terminal section: "Bold-italic font" field model + drag state.
    pub font_bold_italic: &'a mut TextEdit,
    pub font_bold_italic_ms: &'a mut TextInputMouse,
    /// Terminal section: "Symbol map" field model + drag state.
    pub font_symbol_map: &'a mut TextEdit,
    pub font_symbol_map_ms: &'a mut TextInputMouse,
    /// Terminal section: "Font variations" field model + drag state.
    pub font_variations: &'a mut TextEdit,
    pub font_variations_ms: &'a mut TextInputMouse,
    /// Advanced section: "Status bar segments" field model + drag state.
    pub status_bar_segments: &'a mut TextEdit,
    pub status_bar_segments_ms: &'a mut TextInputMouse,
    /// Advanced section: "Time format" field model + drag state.
    pub status_bar_time_format: &'a mut TextEdit,
    pub status_bar_time_format_ms: &'a mut TextInputMouse,
    /// Themes section: "Wallpaper theme" path field model + drag state.
    pub wallpaper_theme: &'a mut TextEdit,
    pub wallpaper_theme_ms: &'a mut TextInputMouse,
}

impl<'r> Ui<'r> {
    /// Build the whole Ctrl+, settings form (§3.5): a full-screen scrim, one
    /// centered glass panel with a header (`glassy — settings` + ✕), labelled
    /// rows (font / opacity / bell / theme / font-family / scrollback / config
    /// path) wired to the live effects, and a footer (Save / Close + transient
    /// saved label). All widget ids share the `settings/…` namespace so they are
    /// collected into `tab_order` in declaration order for keyboard nav. Open
    /// dropdown popups (theme / font) are drawn LAST so they float over the rows.
    ///
    /// `surface` is the framebuffer size in px (for centering + the scrim). The
    /// returned [`SettingsEvents`] carry every change back to the App.
    pub fn build_settings(
        &mut self,
        surface: (f32, f32),
        v: &SettingsView,
        fields: &mut SettingsFields,
    ) -> SettingsEvents {
        // The settings UI is now the sectioned window (settings-themes stream);
        // this thin shim keeps the original `build_settings` entry point + the
        // SettingsView/SettingsEvents flow intact while the layout lives in
        // `settings_panel.rs`.
        self.build_settings_sectioned(surface, v, fields)
    }

    /// A primary (accent-filled) button — same interaction as [`Ui::button`] but
    /// filled with the accent color and dark-on-accent text. Used for Save.
    pub fn accent_button(&mut self, wid: WidgetId, rect: Rect, text: &str) -> Interaction {
        let it = self.interact(wid, rect, true);
        let st = self.wstate(wid, &it, true);
        let hover_t = self.anim(
            wid,
            if matches!(st, WState::Hover | WState::Press) {
                1.0
            } else {
                0.0
            },
        );
        let fill = state_fill(fill_on(), hover_t, it.pressed);
        self.rrect(rect, self.m.radius, fill);
        if matches!(st, WState::Focus) {
            self.focus_ring(rect, self.m.radius);
        }
        let nudge = if it.pressed { 1.0 } else { 0.0 };
        let mut content = rect;
        content.y += nudge;
        self.label_centered(content, text, color::default_bg());
        it
    }

    /// The floating popup list for a dropdown (E3 surface anchored just below
    /// `anchor`, or flipped above if it would overflow the bottom of the surface).
    /// Each row shows an optional swatch, the option name, and a `✓` on the
    /// current selection. Drawn after the form body so it composites above
    /// everything.
    ///
    /// The popup's own height is capped to what fits the surface (never fewer
    /// than 4 rows), but ALL of `rows` remains reachable: when there are more
    /// rows than fit, a scrollbar appears on the right edge and the visible
    /// window scrolls by `scroll` (px, owned by the caller across frames — see
    /// [`super::SettingsView::popup_scroll`]). A fixed visible cap with no
    /// scroll would silently strand any row past it (the 60-theme dropdown is
    /// the case that matters today, but this is generic to any long list).
    ///
    /// Returns `(picked, new_scroll)`: `picked` is the absolute row index if a
    /// row was clicked this frame; `new_scroll` is `scroll` clamped to the
    /// valid range, or moved if the scrollbar thumb was dragged.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn dropdown_popup(
        &mut self,
        wid: WidgetId,
        anchor: Rect,
        rows: &[&str],
        sel: usize,
        swatches: Option<&[[f32; 4]]>,
        surface_h: f32,
        scroll: f32,
    ) -> (Option<usize>, f32) {
        let m = self.m;
        let row_h = m.row_h;
        // Visible row count: what actually fits the surface (leaving a small
        // margin), never fewer than 4. A fixed cap of 8 used to silently hide
        // the 9th effect ("Custom") and any theme past the 8th; sizing to the
        // window instead shows as many as fit, with the rest reachable via scroll.
        let fit = ((surface_h - 4.0 * m.pad) / row_h).floor().max(4.0) as usize;
        let visible_rows = rows.len().min(fit);
        let scrollable = rows.len() > fit;
        let scrollbar_w = if scrollable {
            m.gap.max(6.0).round()
        } else {
            0.0
        };
        let h = (visible_rows as f32 * row_h + 2.0).round();
        // Anchor below by default; flip above when it would overflow the surface.
        let below_y = anchor.y + anchor.h + 2.0;
        let popup_y = if below_y + h > surface_h - m.pad {
            // Flip above: anchor top minus popup height minus gap.
            (anchor.y - h - 2.0).max(m.pad)
        } else {
            below_y
        };
        let rect = Rect::new(anchor.x, popup_y, anchor.w, h);
        self.rrect(rect, m.radius, glass_float());
        self.edge_light(rect);

        let content_h = rows.len() as f32 * row_h;
        let view_h = h - 2.0;
        let max_scroll = (content_h - view_h).max(0.0);
        let scroll = scroll.clamp(0.0, max_scroll);
        let row_area_w = rect.w - scrollbar_w;

        let first = (scroll / row_h).floor().max(0.0) as usize;
        // +1 so a row that's only partially scrolled into view still paints.
        let take = visible_rows + 1;
        let mut picked = None;
        for (i, name) in rows.iter().enumerate().skip(first).take(take) {
            let ry = rect.y + 1.0 + i as f32 * row_h - scroll;
            // Cull rows fully outside the popup's viewport.
            if ry + row_h <= rect.y || ry >= rect.y + rect.h {
                continue;
            }
            let rr = Rect::new(rect.x + 1.0, ry, row_area_w - 2.0, row_h);
            let it = self.interact(id_combine(wid, i as u64), rr, true);
            if i == sel {
                self.rrect(rr.inset(1.0), m.radius - 1.0, sel_bg());
            } else if it.hovered {
                self.rrect(
                    rr.inset(1.0),
                    m.radius - 1.0,
                    state_fill(track_off(), 1.0, false),
                );
            }
            let mut tx = rr.x + m.pad;
            let ty = (rr.center_y() - m.cell_h * 0.5).round();
            if let Some(sw) = swatches.and_then(|s| s.get(i).copied()) {
                let s = (m.cell_h * 0.8).round();
                let sy = rr.center_y() - s * 0.5;
                self.rrect(Rect::new(tx, sy, s, s), 3.0, sw);
                tx += s + m.gap;
            }
            self.label(tx.round(), ty, name, fg());
            if i == sel {
                self.r.push_overlay_glyph_px(
                    (rr.x + rr.w - m.pad - m.cell_w).round(),
                    ty,
                    '✓',
                    fill_on(),
                );
            }
            if it.clicked {
                picked = Some(i);
            }
        }

        let new_scroll = if scrollable {
            let track = Rect::new(
                rect.x + rect.w - scrollbar_w,
                rect.y + 1.0,
                scrollbar_w,
                view_h,
            );
            self.scrollbar(id_combine(wid, u64::MAX), track, content_h, view_h, scroll)
        } else {
            scroll
        };
        (picked, new_scroll)
    }
}

// ---------------------------------------------------------------------------
// Wave 6 — Real menus (§3.6)
// ---------------------------------------------------------------------------

/// A single entry in a dropdown/context menu. The caller builds a `Vec<MenuEntry>`
/// and passes it to [`menu`]; the returned index (if any) identifies which *item*
/// (non-separator entry, 0-based among items only) was clicked.
///
/// # Variant summary
/// - `Item` — a normal actionable row: left icon glyph, label, optional right-
///   aligned dim shortcut hint, and an `enabled` flag (disabled = greyed, no click).
/// - `Separator` — a 1 px hairline dividing groups of items (not focusable).
#[derive(Clone, Debug)]
pub enum MenuEntry<'a> {
    /// An actionable menu row.
    Item {
        /// Single-character icon drawn to the left of the label (e.g. `'+'`).
        icon: char,
        /// Row label shown in the primary foreground colour (or greyed if disabled).
        label: &'a str,
        /// Optional right-aligned shortcut hint (e.g. `"Ctrl+T"`), drawn dim.
        hint: Option<&'a str>,
        /// `false` → row is drawn greyed and never fires a click.
        enabled: bool,
    },
    /// A 1 px hairline dividing groups (not selectable, skipped by keyboard nav).
    Separator,
}

/// Draw an E3 floating menu panel anchored at pixel `(ax, ay)` (top-left of the
/// panel). `entries` mixes [`MenuEntry::Item`] rows with [`MenuEntry::Separator`]
/// dividers; `sel_item` is the *item* index (0-based among non-separator rows)
/// currently highlighted by keyboard navigation.
///
/// Returns the *item* index if an enabled row was clicked this frame, `None`
/// otherwise. The caller maps the item index back to a `MenuAction`.
///
/// Menus never have a scrim — the terminal stays visible beside them (matching
/// today's behaviour). The panel is drawn last so it composites above everything.
/// `ax`/`ay` is the top-left anchor in physical pixels.
#[allow(clippy::too_many_arguments)]
pub fn menu(
    renderer: &mut Renderer,
    cell_w: f32,
    cell_h: f32,
    mouse: (f32, f32),
    mouse_down: bool,
    clicked: bool,
    ax: f32,
    ay: f32,
    entries: &[MenuEntry<'_>],
    sel_item: usize,
) -> Option<usize> {
    let row_h = (cell_h * 1.4).round().max(cell_h + 4.0);
    let sep_h = 5.0; // 1 px hairline + 2 px padding each side
    let pad_x = (cell_w * 1.2).round();
    let icon_w = cell_w + 4.0;
    let hint_gap = (cell_w * 2.0).round();

    // Measure the widest label + widest hint to size the panel.
    let label_chars = entries
        .iter()
        .filter_map(|e| {
            if let MenuEntry::Item { label, .. } = e {
                Some(label.len())
            } else {
                None
            }
        })
        .max()
        .unwrap_or(4);
    let hint_chars = entries
        .iter()
        .filter_map(|e| {
            if let MenuEntry::Item { hint: Some(h), .. } = e {
                Some(h.len())
            } else {
                None
            }
        })
        .max()
        .unwrap_or(0);
    let panel_w = (icon_w
        + label_chars as f32 * cell_w
        + if hint_chars > 0 {
            hint_gap + hint_chars as f32 * cell_w
        } else {
            0.0
        }
        + pad_x * 2.0)
        .max(cell_w * 8.0)
        .ceil();

    // Compute total panel height.
    let item_count = entries
        .iter()
        .filter(|e| matches!(e, MenuEntry::Item { .. }))
        .count();
    let sep_count = entries
        .iter()
        .filter(|e| matches!(e, MenuEntry::Separator))
        .count();
    let panel_h = (item_count as f32 * row_h + sep_count as f32 * sep_h + 4.0).ceil();

    // E3 floating panel with a 1 px accent rrect border (outer minus inner so
    // the border follows the rounded shape and does not bleed into corners).
    let float_fill = glass_float();
    let border_col = with_alpha(color::accent(), 0.22);
    let hairline_c = hairline();
    let menu_radius = 4.0_f32;
    // Border: outer rrect in accent, then inner rrect in glass to carve out the fill.
    renderer.push_overlay_rrect_px(ax, ay, panel_w, panel_h, menu_radius, border_col);
    renderer.push_overlay_rrect_px(
        ax + 1.0,
        ay + 1.0,
        panel_w - 2.0,
        panel_h - 2.0,
        (menu_radius - 1.0).max(0.0),
        float_fill,
    );

    let mut result: Option<usize> = None;
    let mut item_idx: usize = 0; // index among non-separator entries
    let mut y = ay + 2.0;

    for entry in entries {
        match entry {
            MenuEntry::Separator => {
                // Thin hairline with 2 px vertical padding.
                y += 2.0;
                renderer.push_overlay_px(ax + 4.0, y, panel_w - 8.0, 1.0, hairline_c);
                y += 3.0;
            }
            MenuEntry::Item {
                icon,
                label,
                hint,
                enabled,
            } => {
                let rr = Rect::new(ax + 1.0, y, panel_w - 2.0, row_h);
                let over = hit(rr, mouse.0, mouse.1) && *enabled;
                let is_sel = item_idx == sel_item && *enabled;

                // Highlight: keyboard selection OR hover.
                if over || is_sel {
                    renderer.push_overlay_rrect_px(rr.x, rr.y, rr.w, rr.h, 3.0, sel_bg());
                }

                let text_col = if *enabled { fg() } else { fg_dim() };
                let ty = (y + (row_h - cell_h) * 0.5).round();

                // Left icon.
                let ix = ax + pad_x;
                renderer.push_overlay_glyph_px(
                    ix.round(),
                    ty,
                    *icon,
                    if *enabled { fg() } else { fg_dim() },
                );

                // Label — clipped to the space before the shortcut hint (or the
                // right pad) so long labels never overwrite the hint column.
                let lx = ix + icon_w;
                let hint_reserve = if hint.is_some() {
                    hint_gap + hint_chars as f32 * cell_w
                } else {
                    0.0
                };
                let label_max_w = (ax + panel_w - pad_x - hint_reserve - lx).max(0.0);
                let label_max_chars = (label_max_w / cell_w).floor() as usize;
                let label_chars_vec: Vec<char> = label.chars().collect();
                let label_display: String = if label_chars_vec.len() <= label_max_chars {
                    (*label).to_string()
                } else if label_max_chars >= 2 {
                    let keep = label_max_chars - 1;
                    let mut s: String = label_chars_vec[..keep].iter().collect();
                    s.push('…');
                    s
                } else {
                    String::new()
                };
                let mut cx = lx;
                for ch in label_display.chars() {
                    renderer.push_overlay_glyph_px(cx.round(), ty, ch, text_col);
                    cx += cell_w;
                }

                // Right-aligned shortcut hint.
                if let Some(h) = hint {
                    let hint_w = h.chars().count() as f32 * cell_w;
                    let hx = ax + panel_w - pad_x - hint_w;
                    let mut cx2 = hx;
                    for ch in h.chars() {
                        renderer.push_overlay_glyph_px(cx2.round(), ty, ch, fg_dim());
                        cx2 += cell_w;
                    }
                }

                // Click detection.
                if *enabled && clicked && over {
                    result = Some(item_idx);
                }
                item_idx += 1;
                y += row_h;
            }
        }
    }

    let _ = mouse_down; // retained for future press-highlight; clicks are the gate
    result
}
