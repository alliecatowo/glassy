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
        let mut ev = SettingsEvents::default();
        let m = self.m;

        // Full-screen scrim (dim the chrome + terminal beneath).
        self.quad(
            Rect::new(0.0, 0.0, surface.0, surface.1),
            [0.0, 0.0, 0.0, 0.5],
        );

        // Centered panel. Width ≈ 48 columns (room for the longer labels +
        // controls now that the form covers most config keys).
        let pw = (m.cell_w * 48.0)
            .min(surface.0 - 2.0 * m.pad)
            .max(m.cell_w * 28.0);
        // font, opacity, bell, theme, font, scrollback, padding, status_bar,
        // pane_headers, follow_system, ligatures, restore_session, word_sep,
        // font_features, config path.
        const ROWS: usize = 15;
        let header_h = m.row_h;
        let footer_h = m.row_h + m.gap;
        // Adaptive row step: the natural step is `row_h + gap`, but if the form
        // would overflow the surface we shrink the step so every row stays visible
        // (no clipping needed — the panel "coordinates" its own height to the
        // window). The control heights still derive from `row_h`, so controls keep
        // their size while rows pack closer.
        let natural_step = m.row_h + m.gap;
        let avail = (surface.1 - 2.0 * m.pad).max(m.row_h * 4.0);
        let fixed_h = header_h + m.gap + m.gap + footer_h + 2.0 * m.pad;
        let max_body = (avail - fixed_h).max(m.row_h);
        let step = natural_step.min(max_body / ROWS as f32).max(m.cell_h + 2.0);
        let body_h = ROWS as f32 * step;
        let ph = (header_h + m.gap + body_h + m.gap + footer_h + 2.0 * m.pad).round();
        let px = ((surface.0 - pw) * 0.5).round();
        let py = ((surface.1 - ph) * 0.5).round().max(m.pad);
        let panel = Rect::new(px, py, pw, ph);
        ev.panel = panel;
        let inner = self.panel(panel, m.card_radius);

        // Header row: title + close (✕) at the right.
        let title_y = (inner.y + (m.row_h - m.cell_h) * 0.5).round();
        self.label(inner.x.round(), title_y, "glassy — settings", fg());
        let close_r = Rect::new(inner.x + inner.w - m.row_h, inner.y, m.row_h, m.row_h);
        if self.icon_button(id("settings/close"), close_r, '✕').clicked {
            ev.close = true;
        }

        // Each row: a left label column + a right control column. The label column
        // is wide enough for the longest label ("Restore session").
        let label_w = (m.cell_w * 17.0).round();
        let ctrl_x = inner.x + label_w;
        // Control column width: prefer up to 1.6× the natural control width, but
        // never exceed the space left after the label column. The lower floor is
        // `ctrl_w * 0.5` (not `ctrl_w`): a `ctrl_w` floor could push
        // `ctrl_x + ctrl_w` PAST the panel's inner right edge on a narrow window
        // (`inner.w - label_w < ctrl_w`), making every full-width control — and the
        // opacity value label anchored at the column's right edge — overflow the
        // glass. `label_w (17 cells) + ctrl_w*0.5 (7 cells) = 24 cells` stays within
        // the minimum panel width.
        let ctrl_w = (inner.w - label_w).min(m.ctrl_w * 1.6).max(m.ctrl_w * 0.5);
        let mut y = inner.y + header_h + m.gap;
        // The drawn height of a single row's control band: the smaller of the
        // natural control height and the (possibly compressed) step, so adjacent
        // rows never overlap when the form is packed to fit a short window.
        let ctrl_h = (m.row_h - m.gap).min((step - 2.0).max(m.cell_h));
        // Row labels are limited to the label column width; clip with trailing
        // ellipsis if a label is ever too long for its slot.
        let label_clip_w = label_w - m.gap;
        let row_label = |ui: &mut Self, y: f32, text: &str| {
            let ly = (y + (ctrl_h - m.cell_h) * 0.5).round();
            ui.label_clip(inner.x.round(), ly, text, label_clip_w, fg_dim());
        };
        let ctrl_rect = |y: f32, w: f32| Rect::new(ctrl_x, y, w, ctrl_h);

        // -- Font size (stepper) ---------------------------------------------
        row_label(self, y, "Font size");
        let fs_txt = format!("{:.0} px", v.font_px);
        ev.font_delta = self.stepper(id("settings/font_size"), ctrl_rect(y, m.ctrl_w), &fs_txt);
        y += step;

        // -- Opacity (slider) ------------------------------------------------
        row_label(self, y, "Opacity");
        // Reserve ~6 cells at the right of the control column for the value label
        // ("1.00"); floor the track width so the slider stays usable even when
        // `ctrl_w` is squeezed to its `ctrl_w*0.5` floor on a narrow window.
        let sl = ctrl_rect(y, (ctrl_w - m.cell_w * 6.0).max(m.cell_w * 4.0));
        let nv = self.slider(id("settings/opacity"), sl, v.opacity, 0.0, 1.0, 0.05);
        if (nv - v.opacity).abs() > f32::EPSILON {
            ev.opacity = Some(nv);
        }
        self.label_right(
            ctrl_x + ctrl_w,
            (y + (ctrl_h - m.cell_h) * 0.5).round(),
            &format!("{nv:.2}"),
            fg(),
        );
        y += step;

        // -- Bell (segmented) ------------------------------------------------
        row_label(self, y, "Bell");
        let bv = self.segmented(
            id("settings/bell"),
            ctrl_rect(y, ctrl_w),
            &["Off", "Visual", "Audible"],
            v.bell.min(2),
        );
        if bv != v.bell {
            ev.bell = Some(bv);
        }
        y += step;

        // -- Theme (dropdown + swatch) ---------------------------------------
        row_label(self, y, "Theme");
        let theme_rect = ctrl_rect(y, ctrl_w);
        let theme_name = v.theme_names.get(v.theme_idx).copied().unwrap_or("");
        let swatch = v.theme_swatches.get(v.theme_idx).copied();
        if self.dropdown(
            id("settings/theme"),
            theme_rect,
            theme_name,
            v.open == SettingsDrop::Theme,
            swatch,
        ) == DropdownEvt::Toggle
        {
            ev.theme_toggle = true;
        }
        y += step;

        // -- Font family (dropdown) ------------------------------------------
        row_label(self, y, "Font");
        let font_rect = ctrl_rect(y, ctrl_w);
        if self.dropdown(
            id("settings/font_family"),
            font_rect,
            v.font_family,
            v.open == SettingsDrop::Font,
            None,
        ) == DropdownEvt::Toggle
        {
            ev.font_toggle = true;
        }
        y += step;

        // -- Scrollback (stepper) --------------------------------------------
        row_label(self, y, "Scrollback");
        // Just the number — the "Scrollback" row label already says what it is.
        // The old "{} lines" suffix made "10000 lines" (11 chars) overflow the
        // stepper's middle cell (≈ ctrl_w - 2*button ≈ 11.6 cells), so the centered
        // label started left of its cell and overpainted the − and + buttons
        // ("−10000 lines+"). Max scrollback (1_000_000 → "1000000", 7 chars) fits.
        let sb_txt = format!("{}", v.scrollback);
        ev.scrollback_delta =
            self.stepper(id("settings/scrollback"), ctrl_rect(y, m.ctrl_w), &sb_txt);
        y += step;

        // -- Padding (stepper) -----------------------------------------------
        row_label(self, y, "Padding");
        let pad_txt = format!("{} px", v.padding);
        ev.padding_delta = self.stepper(id("settings/padding"), ctrl_rect(y, m.ctrl_w), &pad_txt);
        y += step;

        // -- Status bar (toggle) -----------------------------------------------
        // Width = max(cell_h*2, ctrl_w*0.25) so the knob has meaningful travel.
        row_label(self, y, "Status bar");
        let toggle_w = (m.cell_h * 2.0).max(m.ctrl_w * 0.25).round();
        let toggle_h = m.cell_h;
        let toggle_rect = Rect::new(
            ctrl_x,
            (y + (ctrl_h - toggle_h) * 0.5).round(),
            toggle_w,
            toggle_h,
        );
        let new_status_bar = self.toggle(id("settings/status_bar"), toggle_rect, v.status_bar);
        if new_status_bar != v.status_bar {
            ev.status_bar_toggle = true;
        }
        y += step;

        // -- Pane headers (toggle) -------------------------------------------
        row_label(self, y, "Pane headers");
        let ph_toggle_rect = Rect::new(
            ctrl_x,
            (y + (ctrl_h - toggle_h) * 0.5).round(),
            toggle_w,
            toggle_h,
        );
        let new_pane_headers =
            self.toggle(id("settings/pane_headers"), ph_toggle_rect, v.pane_headers);
        if new_pane_headers != v.pane_headers {
            ev.pane_headers_toggle = true;
        }
        y += step;

        // -- Follow system theme (toggle) ------------------------------------
        let toggle_at = |y: f32| {
            Rect::new(
                ctrl_x,
                (y + (ctrl_h - toggle_h) * 0.5).round(),
                toggle_w,
                toggle_h,
            )
        };
        row_label(self, y, "Follow system");
        if self.toggle(id("settings/follow_system"), toggle_at(y), v.follow_system)
            != v.follow_system
        {
            ev.follow_system_toggle = true;
        }
        y += step;

        // -- Ligatures (toggle) ----------------------------------------------
        row_label(self, y, "Ligatures");
        if self.toggle(id("settings/ligatures"), toggle_at(y), v.ligatures) != v.ligatures {
            ev.ligatures_toggle = true;
        }
        y += step;

        // -- Restore session (toggle) ----------------------------------------
        row_label(self, y, "Restore session");
        if self.toggle(
            id("settings/restore_session"),
            toggle_at(y),
            v.restore_session,
        ) != v.restore_session
        {
            ev.restore_session_toggle = true;
        }
        y += step;

        // -- Word separators (editable) --------------------------------------
        row_label(self, y, "Word seps");
        self.text_input(
            id("settings/word_separator"),
            ctrl_rect(y, ctrl_w),
            fields.word_sep,
            fields.word_sep_ms,
            "(default)",
            fields.blink_on,
            fields.double_click,
        );
        y += step;

        // -- Font features (editable) ----------------------------------------
        row_label(self, y, "Font features");
        self.text_input(
            id("settings/font_features"),
            ctrl_rect(y, ctrl_w),
            fields.font_feat,
            fields.font_feat_ms,
            "ss01 calt=0 …",
            fields.blink_on,
            fields.double_click,
        );
        y += step;

        // -- Config path (readonly + copy/open) ------------------------------
        row_label(self, y, "Config");
        let field_rect = ctrl_rect(y, ctrl_w);
        match self.text_field_readonly(id("settings/config"), field_rect, v.config_path, true, true)
        {
            FieldEvt::Copy => ev.copy_path = true,
            FieldEvt::Open => ev.open_path = true,
            FieldEvt::None => {}
        }
        y += step;

        // -- Footer: separator + Save (accent) + Close + transient saved ------
        let sep_y = (y + m.gap * 0.5).round();
        self.separator(inner.x, sep_y, inner.w);
        let fy = sep_y + m.gap;
        let bw = (m.cell_w * 9.0).round();
        let close_btn = Rect::new(inner.x + inner.w - bw, fy, bw, m.row_h);
        let save_btn = Rect::new(close_btn.x - bw - m.gap, fy, bw, m.row_h);
        if self
            .accent_button(id("settings/save"), save_btn, "Save")
            .clicked
        {
            ev.save = true;
        }
        if self
            .button(id("settings/close_btn"), close_btn, "Close")
            .clicked
        {
            ev.close = true;
        }
        if v.saved {
            let ly = (fy + (m.row_h - m.cell_h) * 0.5).round();
            self.label(inner.x.round(), ly, "✓ saved", fill_on());
        }

        // -- Floating dropdown popups (drawn LAST so they overlap the rows) ---
        let surface_h = surface.1;
        match v.open {
            SettingsDrop::Theme => {
                let pick = self.dropdown_popup(
                    id("settings/theme/list"),
                    theme_rect,
                    v.theme_names,
                    v.theme_idx,
                    Some(v.theme_swatches),
                    surface_h,
                );
                ev.theme_pick = pick;
            }
            SettingsDrop::Font => {
                let pick = self.dropdown_popup(
                    id("settings/font/list"),
                    font_rect,
                    v.font_names,
                    v.font_idx,
                    None,
                    surface_h,
                );
                ev.font_pick = pick;
            }
            SettingsDrop::None => {}
        }

        ev
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
    /// current selection. Returns the absolute index if a row was clicked.
    /// Drawn after the form body so it composites above everything.
    pub(crate) fn dropdown_popup(
        &mut self,
        wid: WidgetId,
        anchor: Rect,
        rows: &[&str],
        sel: usize,
        swatches: Option<&[[f32; 4]]>,
        surface_h: f32,
    ) -> Option<usize> {
        let m = self.m;
        let row_h = m.row_h;
        // Cap the popup height; tall lists would overflow the panel.
        let max_rows = rows.len().min(8);
        let h = (max_rows as f32 * row_h + 2.0).round();
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
        let mut picked = None;
        for (i, name) in rows.iter().enumerate().take(max_rows) {
            let ry = rect.y + 1.0 + i as f32 * row_h;
            let rr = Rect::new(rect.x + 1.0, ry, rect.w - 2.0, row_h);
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
        picked
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
