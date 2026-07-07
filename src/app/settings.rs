//! Settings overlay: focus management, adjustments, font/theme cycling.

use super::*;

/// Split a winit logical key into the `(named_name, character_text)` pair that
/// [`gui::map_text_key`] consumes: a lowercase name for the named keys the text
/// fields care about, or the printable string for a character key. Returns
/// `(None, None)` for keys neither editor needs.
pub(crate) fn key_to_text_parts(key: &Key) -> (Option<String>, Option<String>) {
    match key {
        Key::Named(named) => {
            let name = match named {
                NamedKey::Escape => "escape",
                NamedKey::Enter => "enter",
                NamedKey::Space => "space",
                NamedKey::Backspace => "backspace",
                NamedKey::Delete => "delete",
                NamedKey::Home => "home",
                NamedKey::End => "end",
                NamedKey::ArrowLeft => "arrowleft",
                NamedKey::ArrowRight => "arrowright",
                _ => return (None, None),
            };
            (Some(name.to_string()), None)
        }
        Key::Character(s) => (None, Some(s.to_string())),
        _ => (None, None),
    }
}

impl App {
    /// The keyboard tab order of the settings form, in declaration order. These
    /// mirror the widget ids emitted by [`gui::Ui::build_settings`] and are used
    /// for Tab / Shift+Tab / Up / Down focus movement (the form itself collects
    /// the live order each paint, but key handling runs between paints so it walks
    /// this fixed list — identical ordering keeps focus stable).
    pub(crate) fn settings_focus_order() -> [gui::WidgetId; 20] {
        [
            gui::id("settings/font_size"),
            gui::id("settings/opacity"),
            gui::id("settings/bell"),
            gui::id("settings/theme"),
            gui::id("settings/font_family"),
            gui::id("settings/scrollback"),
            gui::id("settings/padding"),
            gui::id("settings/status_bar"),
            gui::id("settings/pane_headers"),
            gui::id("settings/tab_bar"),
            gui::id("settings/follow_system"),
            gui::id("settings/ligatures"),
            gui::id("settings/restore_session"),
            gui::id("settings/word_separator"),
            gui::id("settings/font_features"),
            gui::id("settings/cursor_style"),
            gui::id("settings/cursor_blink"),
            gui::id("settings/window_effect"),
            gui::id("settings/config"),
            gui::id("settings/save"),
        ]
    }

    /// Set which settings dropdown popup is open (or close all with `None`),
    /// resetting its scroll offset — a freshly-opened list should always start
    /// scrolled to the top, and a closed popup's scroll is stale until it's
    /// reopened anyway. Every assignment to `settings_drop` goes through this
    /// (rather than setting the field directly) so a new call site can't
    /// forget the reset and leave a stale scroll bleeding into the next list.
    pub(crate) fn set_settings_drop(&mut self, drop: gui::SettingsDrop) {
        self.settings_drop = drop;
        self.settings_popup_scroll = 0.0;
    }

    /// Open the settings form: focus the first control and clear transient state.
    /// Forces a full rebuild so the glass panel composites over freshly-painted
    /// terminal rows (the `push_overlay_px` invariant).
    pub(crate) fn open_settings(&mut self) {
        self.settings_open = true;
        self.set_settings_drop(gui::SettingsDrop::None);
        self.settings_saved = false;
        self.gui_focused = Some(Self::settings_focus_order()[0]);
        // Seed the editable text fields from the live config.
        self.settings_word_sep = gui::TextEdit::new(&self.config.word_separator);
        self.settings_word_sep_ms = gui::TextInputMouse::default();
        self.settings_font_feat = gui::TextEdit::new(&self.config.font_features.join(" "));
        self.settings_font_feat_ms = gui::TextInputMouse::default();
        self.settings_hints_chars =
            gui::TextEdit::new(self.config.hints_chars.as_deref().unwrap_or(""));
        self.settings_hints_chars_ms = gui::TextInputMouse::default();
        self.settings_font_bold =
            gui::TextEdit::new(self.config.font_bold.as_deref().unwrap_or(""));
        self.settings_font_bold_ms = gui::TextInputMouse::default();
        self.settings_font_italic =
            gui::TextEdit::new(self.config.font_italic.as_deref().unwrap_or(""));
        self.settings_font_italic_ms = gui::TextInputMouse::default();
        self.settings_font_bold_italic =
            gui::TextEdit::new(self.config.font_bold_italic.as_deref().unwrap_or(""));
        self.settings_font_bold_italic_ms = gui::TextInputMouse::default();
        self.settings_font_symbol_map =
            gui::TextEdit::new(&symbol_map_display(&self.config.font_symbol_map));
        self.settings_font_symbol_map_ms = gui::TextInputMouse::default();
        self.settings_font_variations = gui::TextEdit::new(&self.config.font_variations.join(" "));
        self.settings_font_variations_ms = gui::TextInputMouse::default();
        self.settings_status_bar_segments = gui::TextEdit::new(&status_bar_segments_display(
            self.config.status_bar_segments.as_deref(),
        ));
        self.settings_status_bar_segments_ms = gui::TextInputMouse::default();
        self.settings_status_bar_time_format =
            gui::TextEdit::new(&self.config.status_bar_time_format);
        self.settings_status_bar_time_format_ms = gui::TextInputMouse::default();
        self.settings_wallpaper_theme = gui::TextEdit::new(
            &self
                .config
                .wallpaper_theme
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        self.settings_wallpaper_theme_ms = gui::TextInputMouse::default();
        // Seed the custom-theme working palette from the active theme + refresh the
        // runtime profile list, and reset the section + scroll + hex editor.
        self.seed_custom_theme();
        // Start with the first swatch (fg) selected so the custom-theme editor
        // card is visible + clickable the moment you open the Themes section.
        // (usize::MAX hid the card entirely — there was nothing to click.)
        self.settings_custom_editing = 0;
        self.settings_theme_hex = gui::TextEdit::default();
        self.settings_theme_hex_ms = gui::TextInputMouse::default();
        self.settings_profiles = crate::config::profile_names();
        self.settings_profile_new = gui::TextEdit::default();
        self.settings_profile_new_ms = gui::TextInputMouse::default();
        self.settings_profile_rename_idx = None;
        self.settings_profile_rename = gui::TextEdit::default();
        self.settings_profile_rename_ms = gui::TextInputMouse::default();
        self.settings_profile_delete_armed = None;
        self.settings_section_scroll = 0.0;
        // Seed the click-outside hit rect to the WHOLE surface so a stray press
        // landing in the same frame the form opens (before the first paint sets
        // the real panel rect at `render`) is treated as "inside" and never
        // dismisses the form. After the first paint `settings_panel` becomes the
        // true (smaller) panel rect, so genuine outside clicks dismiss correctly.
        let (sw, sh) = self
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .unwrap_or((u32::MAX, u32::MAX));
        self.settings_panel = gui::Rect::new(0.0, 0.0, sw as f32, sh as f32);
        self.force_full_redraw = true;
    }

    /// Move settings keyboard focus by `dir` (+1 forward / -1 back) through
    /// [`Self::settings_focus_order`], wrapping at the ends.
    pub(crate) fn settings_move_focus(&mut self, dir: i32) {
        let order = Self::settings_focus_order();
        let cur = order
            .iter()
            .position(|&w| Some(w) == self.gui_focused)
            .unwrap_or(0);
        let n = order.len() as i32;
        let next = (cur as i32 + dir).rem_euclid(n) as usize;
        self.gui_focused = Some(order[next]);
        self.settings_saved = false;
    }

    /// Handle a keypress while the settings form is open: Tab/Shift+Tab + Up/Down
    /// move focus, Left/Right (and -/+) adjust the focused control, Enter saves,
    /// and Esc (handled by the caller) closes. Other keys are consumed.
    pub(crate) fn handle_settings_key(&mut self, key: Key, event_loop: &ActiveEventLoop) {
        let shift = self.mods.shift_key();
        match key {
            Key::Named(NamedKey::Tab) => self.settings_move_focus(if shift { -1 } else { 1 }),
            Key::Named(NamedKey::ArrowUp) => self.settings_move_focus(-1),
            Key::Named(NamedKey::ArrowDown) => self.settings_move_focus(1),
            Key::Named(NamedKey::ArrowLeft) => self.settings_adjust_focused(-1),
            Key::Named(NamedKey::ArrowRight) => self.settings_adjust_focused(1),
            Key::Character(ref s) if s.as_str() == "-" => self.settings_adjust_focused(-1),
            Key::Character(ref s) if s.as_str() == "+" || s.as_str() == "=" => {
                self.settings_adjust_focused(1)
            }
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space) => {
                self.settings_activate_focused()
            }
            _ => return,
        }
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Adjust the currently-focused settings control by `dir` (-1/+1). Font,
    /// opacity, theme, and font-family apply live; bell + scrollback update the
    /// config. None persist until [`App::save_settings`].
    pub(crate) fn settings_adjust_focused(&mut self, dir: i32) {
        self.settings_saved = false;
        let f = self.gui_focused;
        if f == Some(gui::id("settings/font_size")) {
            self.resize_font(if dir > 0 {
                FontStep::Inc
            } else {
                FontStep::Dec
            });
        } else if f == Some(gui::id("settings/opacity")) {
            let o = (self.config.opacity + dir as f32 * 0.05).clamp(0.0, 1.0);
            self.config.opacity = o;
            if let Some(r) = self.renderer.as_mut() {
                r.set_opacity(o);
            }
        } else if f == Some(gui::id("settings/bell")) {
            let cur = self.bell_index() as i32;
            let n = 3;
            self.set_bell_index(((cur + dir).rem_euclid(n)) as usize);
        } else if f == Some(gui::id("settings/theme")) {
            self.cycle_theme(dir);
        } else if f == Some(gui::id("settings/font_family")) {
            self.cycle_font_family(dir);
        } else if f == Some(gui::id("settings/scrollback")) {
            self.adjust_scrollback(dir);
        } else if f == Some(gui::id("settings/padding")) {
            self.adjust_padding(dir);
        } else if f == Some(gui::id("settings/cursor_style")) {
            let cur = self.cursor_style_index() as i32;
            self.set_cursor_style_index(((cur + dir).rem_euclid(3)) as usize);
        } else if f == Some(gui::id("settings/window_effect")) {
            // 9 modes (None..Custom); wrap with rem_euclid so Left at 0 lands on
            // Custom (off-by-one: there are 9 variants, index 0..=8, not 8).
            let cur = self.config.window_effect.index() as i32;
            self.set_window_effect_index(((cur + dir).rem_euclid(9)) as usize);
        }
    }

    /// Enter/Space on the focused control: Save activates, the config field copies
    /// its path, the dropdowns toggle open, the segmented bell control advances,
    /// the status-bar toggle flips. Only a control with no activation of its own
    /// falls through to Save.
    pub(crate) fn settings_activate_focused(&mut self) {
        let f = self.gui_focused;
        if f == Some(gui::id("settings/save")) {
            self.save_settings();
        } else if f == Some(gui::id("settings/config")) {
            self.copy_config_path();
        } else if f == Some(gui::id("settings/theme")) {
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::Theme {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Theme
            });
        } else if f == Some(gui::id("settings/font_family")) {
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::Font {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Font
            });
        } else if f == Some(gui::id("settings/bell")) {
            // Segmented control: Enter/Space advances to the next mode (wraps),
            // matching a click cycling Off → Visual → Audible → Off.
            let cur = self.bell_index() as i32;
            self.set_bell_index(((cur + 1).rem_euclid(3)) as usize);
        } else if f == Some(gui::id("settings/status_bar")) {
            self.toggle_status_bar();
        } else if f == Some(gui::id("settings/pane_headers")) {
            self.toggle_pane_headers();
        } else if f == Some(gui::id("settings/tab_bar")) {
            // Segmented Auto → Always → Never → Auto.
            let cur = self.tab_bar_mode_index() as i32;
            self.set_tab_bar_mode_index(((cur + 1).rem_euclid(3)) as usize);
        } else if f == Some(gui::id("settings/follow_system")) {
            self.config.follow_system = !self.config.follow_system;
            self.settings_saved = false;
        } else if f == Some(gui::id("settings/ligatures")) {
            self.config.ligatures = !self.config.ligatures;
            if let Some(r) = self.renderer.as_mut() {
                r.set_ligatures(self.config.ligatures);
            }
            self.settings_saved = false;
        } else if f == Some(gui::id("settings/restore_session")) {
            self.config.restore_session = !self.config.restore_session;
            self.session_dirty = true;
            self.settings_saved = false;
        } else if f == Some(gui::id("settings/cursor_style")) {
            let cur = self.cursor_style_index() as i32;
            self.set_cursor_style_index(((cur + 1).rem_euclid(3)) as usize);
        } else if f == Some(gui::id("settings/cursor_blink")) {
            self.config.cursor_blink = !self.config.cursor_blink;
            self.settings_saved = false;
        } else if f == Some(gui::id("settings/window_effect")) {
            // Segmented: Enter/Space advances to the next of the 9 effect modes.
            let cur = self.config.window_effect.index() as i32;
            self.set_window_effect_index(((cur + 1).rem_euclid(9)) as usize);
        } else if f == Some(gui::id("settings/profile_new_name")) {
            // Commit-on-enter: Enter in the "duplicate as profile" name field
            // creates the profile, mirroring the custom-theme hex field's Enter
            // behaviour of committing its pending value rather than falling
            // through to the general Save.
            self.create_profile_from_current();
        } else if f == Some(gui::id("settings/profile_rename")) {
            // Commit-on-enter for the inline rename field.
            self.commit_profile_rename();
        } else {
            self.save_settings();
        }
    }

    /// Flip the status bar on/off and reflow the grid to reclaim/reserve its row.
    /// Shared by the settings toggle (mouse + keyboard) and the menu action.
    pub(crate) fn toggle_status_bar(&mut self) {
        self.config.status_bar = !self.config.status_bar;
        let strip_h = self.effective_tab_bar_h();
        if let Some(window) = self.window.as_ref() {
            let size = window.inner_size();
            if let Some(r) = self.renderer.as_ref() {
                let m = r.cell_metrics();
                let (cols, rows) = Self::grid_for(
                    size,
                    m.width,
                    m.height,
                    r.pad_x(),
                    r.pad_y(),
                    self.config.status_bar,
                    strip_h,
                );
                self.cols = cols;
                self.rows = rows;
                if let Some(pty) = self.pty.as_mut() {
                    pty.resize(cols, rows, m.width.round() as u16, m.height.round() as u16);
                }
            }
        }
        self.force_full_redraw = true;
    }

    /// Flip per-pane title bars on/off and reflow split panes so they reclaim or
    /// reserve the header band. Shared by the settings toggle (mouse + keyboard)
    /// and the menu action. A no-op for the grid in single-pane mode beyond the
    /// flag flip (single-pane tabs never paint pane headers).
    pub(crate) fn toggle_pane_headers(&mut self) {
        self.config.pane_headers = !self.config.pane_headers;
        // Reflow split panes: their body height (and thus PTY grid) depends on the
        // header band, so the per-pane PTYs must be resized when it appears/vanishes.
        if self.is_split() {
            self.resize_panes();
        }
        self.force_full_redraw = true;
    }

    /// Flip the dim-unfocused-pane-content setting. Forces a full redraw so every
    /// pane is rebuilt with the new dim state (cached tiles carry a stale dim flag
    /// otherwise). A no-op beyond the flag flip when the active tab isn't split.
    pub(crate) fn toggle_dim_unfocused(&mut self) {
        self.config.dim_unfocused = !self.config.dim_unfocused;
        self.force_full_redraw = true;
    }

    /// Tab-bar mode as a segmented index: 0 = Auto, 1 = Always, 2 = Never.
    pub(crate) fn tab_bar_mode_index(&self) -> usize {
        match self.config.show_tab_bar {
            crate::app::TabBarMode::Auto => 0,
            crate::app::TabBarMode::Always => 1,
            crate::app::TabBarMode::Never => 2,
        }
    }

    /// Set the tab-bar mode from a segmented index, then reflow the grid since the
    /// strip's visibility (and thus the available content height) may have changed.
    pub(crate) fn set_tab_bar_mode_index(&mut self, idx: usize) {
        let mode = match idx {
            1 => crate::app::TabBarMode::Always,
            2 => crate::app::TabBarMode::Never,
            _ => crate::app::TabBarMode::Auto,
        };
        if mode == self.config.show_tab_bar {
            return;
        }
        self.config.show_tab_bar = mode;
        self.reflow_grid();
        self.settings_saved = false;
        self.force_full_redraw = true;
    }

    /// Set the window post-process effect from a segmented index (see
    /// [`crate::renderer::WindowEffect::from_index`]). Applies live in the renderer
    /// (which lazily builds / tears down the offscreen pass) and forces a repaint.
    pub(crate) fn set_window_effect_index(&mut self, idx: usize) {
        let effect = crate::renderer::WindowEffect::from_index(idx);
        if effect == self.config.window_effect {
            return;
        }
        self.config.window_effect = effect;
        let p = self.config.custom_effect;
        if let Some(r) = self.renderer.as_mut() {
            if effect == crate::renderer::WindowEffect::Custom {
                r.set_window_effect_custom([p[0], p[1], p[2], p[3]], [p[4], p[5], 0.0, 0.0]);
            } else {
                r.set_window_effect(effect);
            }
        }
        self.settings_saved = false;
        self.force_full_redraw = true;
    }

    /// Re-push the live `Custom` effect channel intensities to the post shader.
    /// Called when an Appearance → Custom slider moves (only meaningful while the
    /// active effect is `Custom`; a no-op-ish push otherwise since the mode gates
    /// it). Forces a repaint so the change shows immediately.
    pub(crate) fn apply_custom_effect(&mut self) {
        let p = self.config.custom_effect;
        if self.config.window_effect == crate::renderer::WindowEffect::Custom
            && let Some(r) = self.renderer.as_mut()
        {
            r.set_window_effect_custom([p[0], p[1], p[2], p[3]], [p[4], p[5], 0.0, 0.0]);
        }
        self.settings_saved = false;
        self.force_full_redraw = true;
    }

    /// Bell mode as a segmented index: 0 = Off, 1 = Visual, 2 = Audible.
    pub(crate) fn bell_index(&self) -> usize {
        if self.config.bell_audible {
            2
        } else if self.config.bell_visual {
            1
        } else {
            0
        }
    }

    /// Set the bell mode from a segmented index (Off / Visual / Audible).
    pub(crate) fn set_bell_index(&mut self, idx: usize) {
        match idx {
            0 => {
                self.config.bell_visual = false;
                self.config.bell_audible = false;
            }
            2 => {
                self.config.bell_visual = false;
                self.config.bell_audible = true;
            }
            _ => {
                self.config.bell_visual = true;
                self.config.bell_audible = false;
            }
        }
    }

    /// The font-family choices shown in the settings dropdown: a small curated set
    /// of common monospace families, always including the active selection so the
    /// current value is selectable + has a checkmark. Index 0 is "default".
    pub(crate) fn font_family_choices(&self) -> Vec<String> {
        let mut names: Vec<String> = vec!["default".to_string()];
        for f in [
            "FiraCode Nerd Font",
            "JetBrainsMono Nerd Font",
            "Hack Nerd Font",
            "DejaVu Sans Mono",
            "Liberation Mono",
            "monospace",
        ] {
            names.push(f.to_string());
        }
        if let Some(cur) = self.config.font_family.as_deref()
            && !names.iter().any(|n| n == cur)
        {
            names.push(cur.to_string());
        }
        names
    }

    /// The index of the active font family within [`Self::font_family_choices`]
    /// (0 = "default" when unset).
    pub(crate) fn font_family_index(&self) -> usize {
        let choices = self.font_family_choices();
        match self.config.font_family.as_deref() {
            None => 0,
            Some(cur) => choices.iter().position(|n| n == cur).unwrap_or(0),
        }
    }

    /// Select a font family by its index into [`Self::font_family_choices`].
    /// Index 0 clears to the discovery default. Applies LIVE: the renderer reloads
    /// the whole font stack (family + features) at the current size and the grid +
    /// PTY reflow for the new cell metrics, so the change is visible immediately
    /// without a restart. Persisted on Save.
    pub(crate) fn set_font_family_index(&mut self, idx: usize) {
        let choices = self.font_family_choices();
        let Some(name) = choices.get(idx) else { return };
        let family = if name == "default" {
            None
        } else {
            Some(name.clone())
        };
        // No-op if the family didn't actually change (avoids a needless font reload
        // + full grid reflow when re-picking the current selection).
        if family == self.config.font_family {
            self.settings_saved = false;
            return;
        }
        self.config.font_family = family;
        self.settings_saved = false;
        self.apply_font_family_live();
    }

    /// Reload the renderer font for the live `config.font_family` (+ font_features)
    /// at the current size, then reflow the grid + PTY for the new cell metrics.
    /// The `None` family reloads the discovery default. A no-op before the renderer
    /// exists. On a bad family the renderer keeps the previous font (logged), and
    /// the reflow below then just re-derives the unchanged grid — never a panic.
    pub(crate) fn apply_font_family_live(&mut self) {
        // Build the family/features from the live config (the same inputs the
        // startup path feeds `Renderer::new`) and reload the font stack.
        let family = self.config.font_family.clone();
        let features = self.config.font_features.clone();
        if let Some(r) = self.renderer.as_mut() {
            r.reload_fonts(family.as_deref(), &features);
        } else {
            return;
        }
        self.reflow_after_font_change();
    }

    /// Recompute the grid for the (possibly changed) cell metrics after a font
    /// reload and inform the PTY / panes / background tabs, then force a full
    /// redraw. Shared by the live font-family path; mirrors the reflow half of
    /// [`Self::resize_font`] but derives the strip height + grid from the current
    /// renderer metrics (which the font reload just refreshed).
    pub(crate) fn reflow_after_font_change(&mut self) {
        // Strip height tracks the (new) cell height; effective_tab_bar_h reads the
        // renderer's current (post-reload) metrics, so this is already correct.
        let strip_h = self.effective_tab_bar_h();
        if let (Some(window), Some(renderer)) = (self.window.as_ref(), self.renderer.as_ref()) {
            let size = window.inner_size();
            if size.width != 0 && size.height != 0 {
                let m = renderer.cell_metrics();
                let (cols, rows) = Self::grid_for(
                    size,
                    m.width,
                    m.height,
                    renderer.pad_x(),
                    renderer.pad_y(),
                    self.config.status_bar,
                    strip_h,
                );
                let (cw, ch) = (m.width.round() as u16, m.height.round() as u16);
                if self.panes.is_some() {
                    self.resize_panes();
                } else {
                    self.cols = cols;
                    self.rows = rows;
                    if let Some(pty) = self.pty.as_ref() {
                        pty.resize(cols, rows, cw, ch);
                    }
                }
                // Keep NON-split background tabs in sync so switching to one is correct.
                for s in &self.background {
                    if s.panes.is_none() {
                        s.pty.resize(cols, rows, cw, ch);
                    }
                }
            }
        }
        // The cell box changed, so every glyph position + per-row storage must be
        // rebuilt next frame; snap the cursor trail to the new metrics (no-op off).
        self.force_full_redraw = true;
        if let Some(r) = self.renderer.as_mut() {
            r.reset_cursor_trail();
        }
    }

    /// Cycle the font family by `dir` through [`Self::font_family_choices`].
    pub(crate) fn cycle_font_family(&mut self, dir: i32) {
        let choices = self.font_family_choices();
        let n = choices.len() as i32;
        if n == 0 {
            return;
        }
        let cur = self.font_family_index() as i32;
        let next = (cur + dir).rem_euclid(n) as usize;
        self.set_font_family_index(next);
    }

    /// Adjust scrollback by `dir` in 1000-line steps, clamped to a sane range.
    pub(crate) fn adjust_scrollback(&mut self, dir: i32) {
        let step = 1000i64;
        let cur = self.config.scrollback as i64;
        let next = (cur + dir as i64 * step).clamp(0, 1_000_000);
        self.config.scrollback = next as usize;
        self.settings_saved = false;
    }

    /// Adjust the uniform grid padding by `dir` (-1/+1) in 2-logical-px steps,
    /// clamped to a sane range. Applies live to the renderer (scaled to physical
    /// px) and reflows the grid + PTY. Persisted on Save.
    pub(crate) fn adjust_padding(&mut self, dir: i32) {
        let step = 2.0_f32;
        let cur = self.config.padding.unwrap_or(0.0);
        let next = (cur + dir as f32 * step).clamp(0.0, 64.0);
        self.config.padding = Some(next);
        self.settings_saved = false;
        let scale = self.dpi_scale();
        if let Some(r) = self.renderer.as_mut() {
            r.set_pad(next * scale);
        }
        self.reflow_after_padding_change();
    }

    /// The window's current DPI scale factor, `1.0` before the window exists.
    /// Shared by every padding adjuster to scale a logical-px step to physical.
    fn dpi_scale(&self) -> f32 {
        self.window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0)
            .max(0.1)
    }

    /// Recompute the grid for the current (possibly changed) padding/inset and
    /// inform the PTY / panes, then force a full redraw. Shared tail of every
    /// padding adjuster (uniform + per-side) — the inset changed, so the grid
    /// size must be recomputed exactly like a resize.
    fn reflow_after_padding_change(&mut self) {
        let strip_h = self.effective_tab_bar_h();
        if let (Some(window), Some(renderer)) = (self.window.as_ref(), self.renderer.as_ref()) {
            let size = window.inner_size();
            let m = renderer.cell_metrics();
            let (cols, rows) = Self::grid_for(
                size,
                m.width,
                m.height,
                renderer.pad_x(),
                renderer.pad_y(),
                self.config.status_bar,
                strip_h,
            );
            self.cols = cols;
            self.rows = rows;
            if self.panes.is_some() {
                self.resize_panes();
            } else if let Some(pty) = self.pty.as_ref() {
                pty.resize(cols, rows, m.width.round() as u16, m.height.round() as u16);
            }
        }
        self.force_full_redraw = true;
    }

    /// Adjust one per-side padding override (Advanced section steppers) by
    /// `dir` (-1/+1) in 2-logical-px steps. UI-clamped to `[0, 64]` — the same
    /// self-imposed bound [`Self::adjust_padding`] uses for the uniform
    /// stepper; `apply_kv` itself only rejects a NEGATIVE per-side value (no
    /// upper bound), so this stays a strict subset of what the config file
    /// accepts. Always writes an explicit `Some(px)` override once touched
    /// (mirrors the uniform stepper's sentinel: `0` reads back as "no override"
    /// at startup — see the `> 0.0` guards in `event_loop.rs`'s per-side init).
    fn adjust_padding_side(
        &mut self,
        dir: i32,
        get: fn(&Config) -> Option<f32>,
        set: fn(&mut Config, Option<f32>),
        set_renderer: fn(&mut Renderer, f32),
    ) {
        let step = 2.0_f32;
        let cur = get(&self.config).unwrap_or(0.0);
        let next = (cur + dir as f32 * step).clamp(0.0, 64.0);
        set(&mut self.config, Some(next));
        self.settings_saved = false;
        let scale = self.dpi_scale();
        if let Some(r) = self.renderer.as_mut() {
            set_renderer(r, next * scale);
        }
        self.reflow_after_padding_change();
    }

    pub(crate) fn adjust_padding_top(&mut self, dir: i32) {
        self.adjust_padding_side(
            dir,
            |c| c.padding_top,
            |c, v| c.padding_top = v,
            |r, v| r.set_pad_top(v),
        );
    }

    pub(crate) fn adjust_padding_bottom(&mut self, dir: i32) {
        self.adjust_padding_side(
            dir,
            |c| c.padding_bottom,
            |c, v| c.padding_bottom = v,
            |r, v| r.set_pad_bottom(v),
        );
    }

    pub(crate) fn adjust_padding_left(&mut self, dir: i32) {
        self.adjust_padding_side(
            dir,
            |c| c.padding_left,
            |c, v| c.padding_left = v,
            |r, v| r.set_pad_left(v),
        );
    }

    pub(crate) fn adjust_padding_right(&mut self, dir: i32) {
        self.adjust_padding_side(
            dir,
            |c| c.padding_right,
            |c, v| c.padding_right = v,
            |r, v| r.set_pad_right(v),
        );
    }

    /// Adjust the quake slide-animation duration (Quake section stepper) by
    /// `dir` (-1/+1) in 20ms steps, clamped to
    /// `[0, QUAKE_ANIMATION_MS_MAX]` (matching `apply_kv`'s clamp exactly).
    pub(crate) fn adjust_quake_animation_ms(&mut self, dir: i32) {
        let step = 20i64;
        let cur = self.config.quake_animation_ms as i64;
        let max = crate::config::parse::QUAKE_ANIMATION_MS_MAX as i64;
        let next = (cur + dir as i64 * step).clamp(0, max);
        self.config.quake_animation_ms = next as u64;
        self.settings_saved = false;
    }

    /// Adjust the command-finish notification threshold (Notifications section
    /// stepper) by `dir` (-1/+1) in 1000ms (1s) steps, clamped to
    /// `[0, NOTIFY_COMMAND_THRESHOLD_MS_MAX]` (matching `apply_kv`'s clamp
    /// exactly).
    pub(crate) fn adjust_notify_threshold_ms(&mut self, dir: i32) {
        let step = 1_000i64;
        let cur = self.config.notify_command_threshold_ms as i64;
        let max = crate::config::parse::NOTIFY_COMMAND_THRESHOLD_MS_MAX as i64;
        let next = (cur + dir as i64 * step).clamp(0, max);
        self.config.notify_command_threshold_ms = next as u64;
        self.settings_saved = false;
    }

    /// Copy the config-file path to the OS clipboard (settings ⧉ button).
    pub(crate) fn copy_config_path(&mut self) {
        let path = config_display_path();
        if let Some(cb) = self.clipboard()
            && let Err(e) = cb.set_text(path)
        {
            log::debug!("clipboard copy (config path) failed: {e}");
        }
    }

    /// Open the config file in the user's editor / file handler (settings ↗).
    /// `open_url` only launches http(s)/file URLs, so wrap the absolute path in a
    /// `file://` URI (after best-effort tilde expansion).
    pub(crate) fn open_config_path(&mut self) {
        let path = crate::config::path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(config_display_path);
        let abs = if path.starts_with('/') {
            path
        } else if let Some(rest) = path.strip_prefix("~/") {
            match std::env::var("HOME") {
                Ok(home) => format!("{home}/{rest}"),
                Err(_) => return,
            }
        } else {
            return;
        };
        // Percent-encode the path so a $HOME (or config dir) containing spaces or
        // other reserved chars produces a valid file:// URI rather than a truncated
        // / misinterpreted one. '/' is preserved as the path separator.
        let uri = format!("file://{}", percent_encode_path(&abs));
        Self::open_url(&uri);
    }

    /// Current cursor style as a segmented-control index: 0=Block, 1=Beam, 2=Underline.
    pub(crate) fn cursor_style_index(&self) -> usize {
        match self.config.cursor_style {
            CursorStyleConfig::Block => 0,
            CursorStyleConfig::Beam => 1,
            CursorStyleConfig::Underline => 2,
        }
    }

    /// Set the cursor style from a segmented-control index, marking config unsaved.
    pub(crate) fn set_cursor_style_index(&mut self, idx: usize) {
        self.config.cursor_style = match idx {
            1 => CursorStyleConfig::Beam,
            2 => CursorStyleConfig::Underline,
            _ => CursorStyleConfig::Block,
        };
        // Apply live to every open pane's terminal (was config-only before, so
        // the change never showed). The child's own DECSCUSR still wins if set.
        let shape = self.config.cursor_style.to_cursor_shape();
        let blink = self.config.cursor_blink;
        let sb = self.config.scrollback;
        let ws = self.config.word_separator.clone();
        if let Some(pty) = &self.pty {
            pty.set_default_cursor(shape, blink, sb, &ws);
        }
        if let Some(panes) = &self.panes {
            for pty in panes.others.values() {
                pty.set_default_cursor(shape, blink, sb, &ws);
            }
        }
        self.settings_saved = false;
        self.force_full_redraw = true;
    }

    /// Cycle the active theme by `dir` through `color::theme_names()`, applying
    /// it live (swap the global theme + full redraw).
    pub(crate) fn cycle_theme(&mut self, dir: i32) {
        let names = color::theme_names();
        let cur = names
            .iter()
            .position(|&n| n == self.config.theme)
            .unwrap_or(0);
        let next = (cur as i32 + dir).rem_euclid(names.len() as i32) as usize;
        let name = names[next];
        if let Some(theme) = color::theme_by_name(name) {
            color::set_theme(theme);
            self.config.theme = name.to_string();
            // The renderer reads theme colors fresh each frame; a full rebuild
            // repaints every cell + the clear color in the new palette.
            self.force_full_redraw = true;
        }
    }

    /// Install the theme at absolute index `idx` within `color::theme_names()`,
    /// applying it live (settings theme-dropdown click). No-op if out of range.
    pub(crate) fn set_theme_by_idx(&mut self, idx: usize) {
        let names = color::theme_names();
        let Some(&name) = names.get(idx) else {
            return;
        };
        if let Some(theme) = color::theme_by_name(name) {
            color::set_theme(theme);
            self.config.theme = name.to_string();
            self.force_full_redraw = true;
            self.settings_saved = false;
        }
    }

    /// Pick and install the theme that matches the system color scheme when
    /// `follow_system` is on: `theme_light` in Light mode, `theme_dark` in Dark
    /// mode (defaulting to dark when the OS doesn't report a preference). A no-op
    /// when follow-system is off, so a pinned `theme` is left untouched. Returns
    /// whether the active theme actually changed (so callers can skip a redundant
    /// full redraw). The GUI tokens derive from the active theme, so the whole UI
    /// adapts automatically once the palette swaps.
    pub(crate) fn apply_system_theme(&mut self, scheme: Option<winit::window::Theme>) -> bool {
        if !self.config.follow_system {
            return false;
        }
        let want_light = matches!(scheme, Some(winit::window::Theme::Light));
        let name = if want_light {
            &self.config.theme_light
        } else {
            &self.config.theme_dark
        };
        if *name == self.config.theme {
            return false;
        }
        if let Some(theme) = color::theme_by_name(name) {
            color::set_theme(theme);
            self.config.theme = name.clone();
            true
        } else {
            false
        }
    }

    /// Generate a [`crate::color::Theme`] from the configured `wallpaper_theme`
    /// image path (or the one supplied as an override) and apply it live.
    ///
    /// Shows a toast with the result. If no wallpaper path is configured, shows
    /// a hint toast instead.
    pub(crate) fn generate_theme_from_wallpaper(&mut self, event_loop: &ActiveEventLoop) {
        let path = match &self.config.wallpaper_theme {
            Some(p) => p.clone(),
            None => {
                self.push_toast(
                    "No wallpaper_theme configured. Add `wallpaper_theme = /path/to/image.png` to glassy.conf",
                );
                self.mark_dirty(event_loop);
                return;
            }
        };
        let path_str = path.to_string_lossy().into_owned();
        match crate::config::theme_gen::from_image_path(&path_str) {
            Ok(generated) => {
                color::set_theme(generated);
                self.config.theme = "wallpaper".to_string();
                self.force_full_redraw = true;
                let short = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path_str.clone());
                self.push_toast(format!("Theme generated from {short}"));
                self.mark_dirty(event_loop);
            }
            Err(e) => {
                self.push_toast(format!("Theme generation failed: {e}"));
                self.mark_dirty(event_loop);
            }
        }
    }

    /// Apply a generated theme from the given image path directly (called from
    /// external tooling, OSC hooks, or the scripted test harness via
    /// `GLASSY_THEME_GEN_IMAGE`). A no-op with a log warning on decode failure.
    pub(crate) fn apply_theme_from_image_path(&mut self, path: &str, event_loop: &ActiveEventLoop) {
        match crate::config::theme_gen::from_image_path(path) {
            Ok(generated) => {
                color::set_theme(generated);
                self.config.theme = "wallpaper".to_string();
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            Err(e) => {
                log::warn!("glassy: theme-gen from '{path}' failed: {e}");
            }
        }
    }

    /// Persist every live-adjustable setting to the config file, preserving
    /// every other key/comment. Driven by the [`settings_save::SAVED_KEYS`]
    /// table (one `(key, Config -> String)` entry per settings-UI-mutable
    /// field) so a new settings-form control can't silently ship without being
    /// saved — see that module's doc comment for why this used to be a
    /// hand-maintained list that drifted out of sync.
    ///
    /// `font_size` is the one exception: it is special-cased here rather than
    /// in the table because the live, user-visible size lives in the
    /// renderer's effective font px (Ctrl +/-/0 and the settings stepper both
    /// drive it there directly), not in `Config::font_size`, which only
    /// reflects the size at startup.
    pub(crate) fn save_settings(&mut self) {
        let pt = self.live_font_size_pt();
        let mut updates: Vec<(&str, String)> =
            Vec::with_capacity(settings_save::SAVED_KEYS.len() + 1);
        updates.push(("font_size", format!("{pt:.0}")));
        for entry in settings_save::SAVED_KEYS {
            updates.push((entry.key, (entry.get)(&self.config)));
        }
        match crate::config::save(&updates) {
            Ok(()) => {
                self.settings_saved = true;
                log::info!("settings saved to config");
            }
            Err(e) => {
                // `self.settings_saved` stays false here, identical to the
                // pre-save "not yet saved" state — without a toast, a real
                // failure (read-only file, HOME/XDG unset) is indistinguishable
                // in the UI from "hasn't saved yet". Surface it explicitly.
                log::error!("settings save failed: {e:#}");
                self.push_toast(format!("Settings not saved: {e:#}"));
            }
        }
    }

    /// The current live font size in points, exactly the number
    /// [`Self::save_settings`] writes for the `font_size` key: the renderer's
    /// effective px (Ctrl +/-/0 and the settings stepper both drive it there
    /// directly) converted back through the window's scale factor — NOT
    /// `self.config.font_size`, which only reflects the size at startup (see
    /// this module's `SAVED_KEYS` doc comment for why `font_size` is excluded
    /// from that table). Shared with the `get-config`/`set-config`
    /// remote-control verbs (`remote.rs`) so a script reads back the exact
    /// same number the settings overlay shows.
    pub(crate) fn live_font_size_pt(&self) -> f32 {
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0)
            .max(0.1);
        let px = self
            .renderer
            .as_ref()
            .map(|r| r.font_px())
            .unwrap_or(self.config.font_size);
        (px / scale).max(1.0)
    }

    /// Apply a runtime font-size change (Ctrl +/-/0): reload the font in the
    /// renderer, recompute the grid for the new cell box + padding, and resize the
    /// PTY. A no-op before the renderer/PTY exist.
    pub(crate) fn resize_font(&mut self, step: FontStep) {
        // Read visibility (and the chrome inset — see effective_tab_bar_h's doc
        // comment) before the mutable renderer borrow below.
        let strip_visible = self.tab_bar_visible();
        let chrome_inset = self.chrome_top_inset();
        let base_font_px = self.base_font_px;
        let (Some(renderer), Some(pty)) = (self.renderer.as_mut(), self.pty.as_ref()) else {
            return;
        };
        let target = match step {
            FontStep::Inc => renderer.font_px() + FONT_STEP_PX,
            FontStep::Dec => renderer.font_px() - FONT_STEP_PX,
            FontStep::Reset => base_font_px.unwrap_or_else(|| renderer.font_px()),
        };
        renderer.set_font_size(target);

        // Recompute the grid for the new cell metrics + padding against the
        // current surface, and inform the PTY. The strip height tracks the (new)
        // cell height, so re-derive it here from the post-resize metrics. Without
        // .max(chrome_inset), a hidden tab bar under-reserves the top band by the
        // traffic-light inset, computing one row too many (see handle_resize).
        let strip_h = if strip_visible {
            tab_bar_h(renderer.cell_metrics().height)
        } else {
            0.0
        }
        .max(chrome_inset);
        if let Some(window) = self.window.as_ref() {
            let size = window.inner_size();
            let m = renderer.cell_metrics();
            let (cols, rows) = Self::grid_for(
                size,
                m.width,
                m.height,
                renderer.pad_x(),
                renderer.pad_y(),
                self.config.status_bar,
                strip_h,
            );
            self.cols = cols;
            self.rows = rows;
            pty.resize(cols, rows, m.width.round() as u16, m.height.round() as u16);
        }
        // The cell box changed, so every glyph position and the per-row storage
        // must be rebuilt next frame.
        self.force_full_redraw = true;
    }

    /// The earliest timed wakeup we must schedule when otherwise idle: the blink
    /// phase boundary and/or the visual-bell flash deadline, whichever is sooner.
    /// `None` means nothing is pending and the loop can park on `ControlFlow::Wait`
    /// (0% idle).
    pub(crate) fn next_wake(
        &self,
        blink_active: bool,
        flash_active: bool,
        spin_active: bool,
    ) -> Option<Instant> {
        let blink = blink_active.then_some(self.blink_at);
        let flash = flash_active.then_some(self.bell_flash_until).flatten();
        let spin = spin_active.then_some(self.spinner_at);
        // Text blink (SGR 5/6) adds its own deadline when the timer is running.
        let text_blink = (self.text_blink_active && self.focused).then_some(self.text_blink_at);
        [blink, flash, spin, text_blink].into_iter().flatten().min()
    }

    /// Whether any tab is currently busy. While true the spinner must keep
    /// animating (a finite, self-extending wakeup); when false we return to `Wait`.
    pub(crate) fn any_tab_busy(&self, now: Instant) -> bool {
        self.active_busy_until.is_some_and(|t| now < t)
            || self
                .background
                .iter()
                .any(|s| s.busy_until.is_some_and(|t| now < t))
    }

    pub(crate) fn handle_resize(&mut self, event_loop: &ActiveEventLoop, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        let strip_visible = self.tab_bar_visible();
        // Read before the mutable renderer borrow below — see effective_tab_bar_h's
        // doc comment. Without this .max(), a hidden tab bar under-reserves the top
        // band by the traffic-light inset, so grid_for computes one row too many
        // and the last line (e.g. the shell prompt) renders partly below the
        // window's bottom edge.
        let chrome_inset = self.chrome_top_inset();
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        renderer.resize(size.width, size.height);
        let m = renderer.cell_metrics();
        let strip_h = if strip_visible {
            tab_bar_h(m.height)
        } else {
            0.0
        }
        .max(chrome_inset);
        let (cols, rows) = Self::grid_for(
            size,
            m.width,
            m.height,
            renderer.pad_x(),
            renderer.pad_y(),
            self.config.status_bar,
            strip_h,
        );
        let (cw, ch) = (m.width.round() as u16, m.height.round() as u16);

        if self.panes.is_some() {
            // Active tab is split: fan each pane out to its new tile rectangle.
            // (This also re-points self.cols/self.rows at the focused pane.)
            self.resize_panes();
        } else if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            if let Some(pty) = self.pty.as_ref() {
                pty.resize(cols, rows, cw, ch);
            }
        }
        // Keep NON-split background tabs in sync so switching to one shows the
        // correct layout; split background tabs are re-laid-out on activation.
        for s in &self.background {
            if s.panes.is_none() {
                s.pty.resize(cols, rows, cw, ch);
            }
        }
        // Reproject + repaint the whole grid against the new surface; the per-row
        // storage is resized to match in the next frame's full rebuild.
        self.force_full_redraw = true;
        // The cursor-trail eased position is in physical pixels; after a resize /
        // font change the cell metrics shifted, so snap it to the new cell next
        // frame rather than gliding across stale coordinates. (No-op when off.)
        if let Some(r) = self.renderer.as_mut() {
            r.reset_cursor_trail();
        }
        self.mark_dirty(event_loop);
    }
}
