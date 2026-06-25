//! Chrome painting: status bar, settings overlay, tab-rename editor,
//! confirm-close modal. Tab-bar and chip painting are in tab_paint.rs.

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
    // paint_tab_bar, paint_tab_chip, paint_tab_label live in tab_paint.rs.

    /// Paint the Wave-4 status bar (§3.4): a `STATUS_BAR_H`-px E1 band at the
    /// bottom of the window. Content is laid out as fixed-width right-aligned
    /// segments so nothing jitters as values change:
    ///
    ///   `[mode]  …  [sel]  [scroll%]  [enc]`
    ///
    /// **mode** = `ALT` when the focused pane is in alt-screen, `MOUSE` when mouse
    /// reporting is active (from `TermMode`). Both can be absent at once (normal
    /// screen, no mouse reporting). **scroll%** = `⇡NN%` when scrolled back into
    /// history (`display_offset > 0`). **sel** = glyph count when there is an
    /// active text selection. **enc** = `UTF-8` (always, for now). git/cwd slots
    /// are reserved but hidden until a follow-up lands `/proc`-based data.
    ///
    /// Associated fn (no `&self`) so it composes with the caller's `&mut Renderer`
    /// borrow; all data arrives as plain parameters.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_status_bar(
        renderer: &mut Renderer,
        surface_h: u32,
        term_mode: TermMode,
        display_offset: i32,
        history_size: usize,
        sel_len: usize,
        win_focused: bool,
        cwd: Option<&std::path::Path>,
        git_branch: Option<&str>,
        progress: Option<crate::image::ProgressState>,
    ) {
        let m = renderer.cell_metrics();
        let (sw, _sh) = renderer.surface_size();
        let bar_w = sw as f32;
        let bar_h = STATUS_BAR_H;
        let bar_y = surface_h as f32 - bar_h;

        if bar_w <= 0.0 || bar_h <= 0.0 {
            return;
        }

        // Dim while window is unfocused, matching tab-bar convention.
        let fdim = if win_focused { 1.0 } else { 0.7 };
        let mul = |c: [f32; 4]| [c[0] * fdim, c[1] * fdim, c[2] * fdim, c[3]];

        let bar_bg = mul(gui::glass_body());
        let accent = mul(color::accent());
        let fg_dim = mul(gui::fg_dim());
        let fg = mul(gui::fg());

        // 1) Bar backdrop + top hairline (mirrors the tab bar's bottom seam).
        renderer.push_overlay_px(0.0, bar_y, bar_w, bar_h, bar_bg);
        renderer.push_overlay_px(0.0, bar_y, bar_w, 1.0, mul(gui::hairline()));

        // Glyph vertical centre within the bar.
        let ty = (bar_y + (bar_h - m.height) * 0.5).round();

        // 2) Right-aligned fixed-width segments (right → left, each padded to a
        //    fixed character count so a value change never shifts other segments).
        //
        //    Widths (in multiples of cell_w):
        //      enc:    5 chars ("UTF-8") + 1 gap = 6 cw
        //      scroll: 6 chars ("⇡100%") + 1 gap = 7 cw   (hidden when at bottom)
        //      sel:    8 chars ("999 sel") + 1 gap = 9 cw  (hidden when no sel)
        //      mode:   7 chars ("MOUSE  " or "ALT    ") + 1 gap = 8 cw (hidden when plain)
        //
        //    Right margin: 1 cw.
        let right_margin = m.width;
        let mut rx = bar_w - right_margin;

        // Encoding (always shown, right-aligned anchor).
        {
            let s = "UTF-8";
            let w = renderer.text_width_px(s);
            renderer.push_overlay_glyph_px_str((rx - w).round(), ty, s, fg_dim);
            rx -= (6.0 * m.width).round(); // fixed 6-char slot
        }

        // Scroll percent — shown only when scrolled back into history.
        if display_offset > 0 {
            let pct = if history_size > 0 {
                ((display_offset as f32 / history_size as f32) * 100.0).round() as u32
            } else {
                100
            }
            .min(100);
            let s = format!("⇡{pct:>3}%");
            let w = renderer.text_width_px(&s);
            renderer.push_overlay_glyph_px_str((rx - w).round(), ty, &s, accent);
        }
        rx -= (7.0 * m.width).round(); // fixed 7-char slot (even when hidden)

        // Selection glyph count — shown only when a selection is active.
        if sel_len > 0 {
            let s = format!("{sel_len} sel");
            let w = renderer.text_width_px(&s);
            renderer.push_overlay_glyph_px_str((rx - w).round(), ty, &s, fg_dim);
        }
        rx -= (9.0 * m.width).round(); // fixed 9-char slot

        // Mode flags (ALT / MOUSE) — shown only when non-standard.
        {
            let alt = term_mode.contains(TermMode::ALT_SCREEN);
            let mouse = term_mode.intersects(TermMode::MOUSE_MODE);
            if alt || mouse {
                let tag = if alt { "ALT" } else { "MOUSE" };
                let w = renderer.text_width_px(tag);
                renderer.push_overlay_glyph_px_str((rx - w).round(), ty, tag, fg);
            }
            rx -= (8.0 * m.width).round(); // fixed 8-char slot (even when hidden)
        }
        let _ = rx; // git/cwd slots reserved here for future waves

        // 3) Left section: cwd (basename) and git branch. These fill the reserved
        //    left slots that were previously empty ("git/cwd slots reserved here").
        {
            let left_margin = m.width;
            let mut lx = left_margin;

            // cwd: last path component, or "~" for $HOME, or full path if short.
            if let Some(path) = cwd {
                let cwd_str: String = if path.as_os_str().is_empty() {
                    "~".to_string()
                } else {
                    // Show last two components for context (e.g. "glassy/src").
                    let components: Vec<_> = path.components().collect();
                    let n = components.len();
                    if n >= 2 {
                        format!(
                            "{}/{}",
                            components[n - 2].as_os_str().to_string_lossy(),
                            components[n - 1].as_os_str().to_string_lossy()
                        )
                    } else if n == 1 {
                        components[0].as_os_str().to_string_lossy().to_string()
                    } else {
                        "~".to_string()
                    }
                };
                let w = renderer.text_width_px(&cwd_str);
                renderer.push_overlay_glyph_px_str(lx.round(), ty, &cwd_str, fg_dim);
                lx += w + m.width;

                // Git branch (if in a repo): " branch_name" in accent.
                if let Some(branch) = git_branch {
                    let branch_str = format!("\u{E0A0} {branch}"); // nf-pl-branch glyph
                    let w = renderer.text_width_px(&branch_str);
                    renderer.push_overlay_glyph_px_str(lx.round(), ty, &branch_str, accent);
                    lx += w;
                }
            }
            let _ = lx;
        }

        // 4) OSC 9;4 progress indicator: a thin filled bar at the very bottom of
        //    the status bar (1px tall) spanning a fraction of the bar width, colored
        //    by state (accent = active, red = error, dim = indeterminate). Subtle and
        //    non-intrusive — it sits inside the status bar's existing pixel budget.
        if let Some(prog) = progress {
            use crate::image::ProgressState;
            let bar_bottom = bar_y + bar_h - 1.0; // 1px at the very bottom
            let (pct, color) = match prog {
                ProgressState::Set(p) => (p as f32 / 100.0, accent),
                ProgressState::Error(p) => (p as f32 / 100.0, mul(color::danger())),
                ProgressState::Indeterminate => (1.0, fg_dim),
                ProgressState::Remove => (0.0, fg_dim),
            };
            if pct > 0.0 {
                let prog_w = (bar_w * pct).max(2.0);
                renderer.push_overlay_px(0.0, bar_bottom, prog_w, 1.0, color);
            }
        }

        // 5) Left margin: a small decorative separator mark.
        renderer.push_overlay_px(0.0, bar_y, 1.0, bar_h, mul(gui::rail()));
    }

    /// Paint the inline tab-rename editor over the chip rect `r`: an opaque raised
    /// field with an accent ring, the in-progress `buffer` text (tail-clipped so the
    /// caret stays visible), and a block caret at the end. Associated (no `&self`)
    /// so it composes with the caller's `&mut Renderer` borrow.
    pub(crate) fn paint_tab_rename(renderer: &mut Renderer, r: gui::Rect, buffer: &str) {
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

        // Text area: pad in, reserve one cell for the caret. Tail-clip so the END
        // of the buffer stays visible while typing (the natural caret position).
        let pad = (cell_w * 0.6).round();
        let ty = (r.center_y() - cell_h * 0.5).round();
        let text_w = (r.w - 2.0 * pad - cell_w).max(0.0);
        let max_chars = (text_w / cell_w).floor() as usize;
        let chars: Vec<char> = buffer.chars().collect();
        let visible: String = if chars.len() <= max_chars {
            buffer.to_string()
        } else if max_chars >= 1 {
            // Keep the tail; lead with an ellipsis. max_chars >= 1 here, so the
            // subtraction never underflows.
            let tail = &chars[chars.len() - (max_chars - 1)..];
            let mut s = String::from("…");
            s.extend(tail.iter());
            s
        } else {
            String::new()
        };
        let mut cx = r.x + pad;
        for ch in visible.chars() {
            renderer.push_overlay_glyph_px(cx.round(), ty, ch, gui::fg());
            cx += cell_w;
        }
        // Block caret immediately after the last visible glyph.
        renderer.push_overlay_px(cx.round(), ty, 2.0, cell_h, color::accent());
    }

    /// Paint the settings form (§3.5) as a centered glass panel over a full-screen
    /// scrim, returning the interaction events for the caller to apply. Static (no
    /// `&self`) so it composes with the live `&mut Renderer` borrow held in
    /// `render`/`render_split`, threading the App-owned persistent GUI state.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_settings(
        renderer: &mut Renderer,
        config: &Config,
        font_px: f32,
        bell_idx: usize,
        font_choices: &[String],
        font_idx: usize,
        config_path: &str,
        open: gui::SettingsDrop,
        saved: bool,
        mouse: (f32, f32),
        mouse_down: bool,
        clicked: bool,
        gui_pressed: &mut Option<gui::WidgetId>,
        gui_focused: &mut Option<gui::WidgetId>,
        gui_anims: &mut std::collections::HashMap<gui::WidgetId, gui::Anim>,
    ) -> gui::SettingsEvents {
        // Theme names + per-theme accent swatches (the cursor color each theme
        // deliberately picks to pop).
        let theme_names = color::THEME_NAMES;
        let swatches: Vec<[f32; 4]> = theme_names
            .iter()
            .map(|n| match color::theme_by_name(n) {
                Some(t) => [
                    t.cursor.r as f32 / 255.0,
                    t.cursor.g as f32 / 255.0,
                    t.cursor.b as f32 / 255.0,
                    1.0,
                ],
                None => color::accent(),
            })
            .collect();
        let theme_idx = theme_names
            .iter()
            .position(|&n| n == config.theme)
            .unwrap_or(0);
        let font_refs: Vec<&str> = font_choices.iter().map(|s| s.as_str()).collect();
        let font_display = config.font_family.as_deref().unwrap_or("default");
        let font_features_str = config.font_features.join(" ");
        // Effective uniform padding shown in the form: the explicit `padding`
        // override if set, else 0 (meaning "cell-derived default").
        let padding_px = config.padding.unwrap_or(0.0).round().max(0.0) as u32;

        let (sw, sh) = renderer.surface_size();
        let (cw, ch) = {
            let m = renderer.cell_metrics();
            (m.width, m.height)
        };
        let mut ui = gui::Ui::new(
            renderer,
            cw,
            ch,
            mouse,
            mouse_down,
            clicked,
            gui_pressed,
            gui_focused,
            gui_anims,
        );
        let view = gui::SettingsView {
            font_px,
            opacity: config.opacity,
            bell: bell_idx,
            theme_idx,
            theme_names,
            theme_swatches: &swatches,
            font_family: font_display,
            font_names: &font_refs,
            font_idx,
            scrollback: config.scrollback,
            config_path,
            open,
            saved,
            status_bar: config.status_bar,
            pane_headers: config.pane_headers,
            follow_system: config.follow_system,
            ligatures: config.ligatures,
            restore_session: config.restore_session,
            padding: padding_px,
            word_separator: &config.word_separator,
            font_features: &font_features_str,
        };
        ui.build_settings((sw as f32, sh as f32), &view)
    }

    /// Apply the settings-form events to the live config + renderer + theme. Runs
    /// after `paint_settings` (the `Ui` borrow is dropped), driving the existing
    /// effects so opacity / font / theme preview immediately. Requests a repaint
    /// directly via the window (no `event_loop` is available inside `render`).
    pub(crate) fn apply_settings_events(&mut self, ev: gui::SettingsEvents) {
        // Remember the panel bounds for click-outside dismissal next frame.
        self.settings_panel = ev.panel;
        let mut changed = false;
        if ev.font_delta > 0 {
            self.resize_font(FontStep::Inc);
            changed = true;
        } else if ev.font_delta < 0 {
            self.resize_font(FontStep::Dec);
            changed = true;
        }
        if let Some(o) = ev.opacity {
            self.config.opacity = o;
            if let Some(r) = self.renderer.as_mut() {
                r.set_opacity(o);
            }
            changed = true;
        }
        if let Some(b) = ev.bell {
            self.set_bell_index(b);
            changed = true;
        }
        if ev.theme_toggle {
            self.settings_drop = if self.settings_drop == gui::SettingsDrop::Theme {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Theme
            };
            changed = true;
        }
        if let Some(t) = ev.theme_pick {
            self.set_theme_by_idx(t);
            self.settings_drop = gui::SettingsDrop::None;
            changed = true;
        }
        if ev.font_toggle {
            self.settings_drop = if self.settings_drop == gui::SettingsDrop::Font {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Font
            };
            changed = true;
        }
        if let Some(f) = ev.font_pick {
            self.set_font_family_index(f);
            self.settings_drop = gui::SettingsDrop::None;
            changed = true;
        }
        if ev.scrollback_delta != 0 {
            self.adjust_scrollback(ev.scrollback_delta);
            changed = true;
        }
        if ev.status_bar_toggle {
            self.toggle_status_bar();
            changed = true;
        }
        if ev.pane_headers_toggle {
            self.toggle_pane_headers();
            changed = true;
        }
        if ev.follow_system_toggle {
            self.config.follow_system = !self.config.follow_system;
            self.settings_saved = false;
            changed = true;
        }
        if ev.ligatures_toggle {
            self.config.ligatures = !self.config.ligatures;
            if let Some(r) = self.renderer.as_mut() {
                r.set_ligatures(self.config.ligatures);
            }
            self.settings_saved = false;
            changed = true;
        }
        if ev.restore_session_toggle {
            self.config.restore_session = !self.config.restore_session;
            self.session_dirty = true;
            self.settings_saved = false;
            changed = true;
        }
        if ev.padding_delta != 0 {
            self.adjust_padding(ev.padding_delta);
            changed = true;
        }
        if ev.copy_path {
            self.copy_config_path();
            changed = true;
        }
        if ev.open_path {
            self.open_config_path();
        }
        if ev.save {
            self.save_settings();
            changed = true;
        }
        if ev.close {
            self.settings_open = false;
            self.settings_drop = gui::SettingsDrop::None;
            self.overlay_opened_by_press = false;
            changed = true;
        }
        if changed {
            self.force_full_redraw = true;
            self.dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
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
        let dluma = 0.2126 * danger[0] + 0.7152 * danger[1] + 0.0722 * danger[2];
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
