//! Settings-form painting + apply: `App::paint_settings` builds the
//! [`gui::SettingsView`] snapshot and hands off to `gui::Ui::build_settings`;
//! `App::apply_settings_events` reads the resulting [`gui::SettingsEvents`]
//! back and drives every live effect. Split out of the former flat
//! `chrome.rs` (settings-modularity stream) — see `super`'s module doc.

use super::*;

/// A `CONFIG_TOGGLES` entry's accessor: returns the `&mut bool` a toggle flips.
type ConfigToggleField = fn(&mut Config) -> &mut bool;

/// Plain boolean settings-form toggles: `(widget id, config field accessor)`.
/// Consulted by [`App::apply_settings_events`] for every id in
/// `SettingsEvents::toggled` that isn't one of the toggles matched explicitly
/// there because it drives an extra live side effect beyond the flip itself
/// (status bar / pane headers reflow the grid; ligatures / cursor trail push to
/// the renderer; restore-session marks the session dirty). Adding a new plain
/// boolean setting is then one `RowKind::Toggle` push (`settings_panel.rs`) +
/// one row here — no new `SettingsEvents` field, no new `if` block.
const CONFIG_TOGGLES: &[(&str, ConfigToggleField)] = &[
    ("settings/follow_system", |c| &mut c.follow_system),
    ("settings/cursor_blink", |c| &mut c.cursor_blink),
    ("settings/copy_on_select", |c| &mut c.copy_on_select),
    ("settings/minimap", |c| &mut c.minimap),
    ("settings/command_badges", |c| &mut c.command_badges),
    ("settings/title_show_cwd", |c| &mut c.title_show_cwd),
    ("settings/title_show_count", |c| &mut c.title_show_count),
    ("settings/dim_unfocused", |c| &mut c.dim_unfocused),
    ("settings/copy_html", |c| &mut c.copy_html),
    ("settings/pane_headers_single", |c| {
        &mut c.pane_headers_single
    }),
    ("settings/notify_command_finish", |c| {
        &mut c.notify_command_finish
    }),
    // Quake mode is armed once in `App::init_quake`, called only from
    // `resumed()` at startup — flipping `config.quake` after the window
    // already exists has no live effect (no quake window is created/torn
    // down). It is still a plain flip: no OTHER live side effect exists to
    // replicate (the Settings UI labels this "(restart required)" — see
    // `SettingsSection::Quake` in `settings_panel.rs`), and the value is
    // still persisted on Save for the next launch.
    ("settings/quake", |c| &mut c.quake),
    // Like quake, `decorations` only takes effect at window creation
    // (`resumed()`), so flipping it live has no immediate effect — the Settings
    // UI labels it "(restart required)". The value is still persisted on Save.
    ("settings/decorations", |c| &mut c.decorations),
];

impl App {
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
        fields: &mut gui::SettingsFields,
        section: usize,
        section_scroll: f32,
        custom_swatches: &[[f32; 4]],
        custom_editing: usize,
        profile_names: &[String],
        active_profile: Option<&str>,
        profile_rename_idx: Option<usize>,
        profile_delete_armed: Option<usize>,
        popup_scroll: f32,
    ) -> gui::SettingsEvents {
        // Theme names + per-theme accent swatches (the cursor color each theme
        // deliberately picks to pop), sourced from the registry's single
        // built-ins+user-themes snapshot so both lists always agree.
        let theme_entries = color::theme_entries();
        let theme_names: Vec<&str> = theme_entries.iter().map(|e| e.canonical).collect();
        let swatches: Vec<[f32; 4]> = theme_entries
            .iter()
            .map(|e| {
                let c = e.theme.cursor;
                [
                    c.r as f32 / 255.0,
                    c.g as f32 / 255.0,
                    c.b as f32 / 255.0,
                    1.0,
                ]
            })
            .collect();
        let theme_idx = theme_names
            .iter()
            .position(|&n| n == config.theme)
            .unwrap_or(0);
        let font_refs: Vec<&str> = font_choices.iter().map(|s| s.as_str()).collect();
        let font_display = config.font_family.as_deref().unwrap_or("default");
        // Owned (not borrowed): `resolved_font_family` reads from `renderer`
        // now, before `gui::Ui::new` below takes it mutably for the rest of
        // this function, and the value must outlive that mutable borrow.
        let resolved_font_family_str = match renderer.resolved_font_family() {
            Some(name) => format!("Loaded font: {name}"),
            None => "Loaded font: unresolved (generic monospace)".to_string(),
        };
        let font_features_str = config.font_features.join(" ");
        // Effective uniform padding shown in the form: the explicit `padding`
        // override if set, else 0 (meaning "cell-derived default").
        let padding_px = config.padding.unwrap_or(0.0).round().max(0.0) as u32;
        // Tab-bar policy as a segmented index (Auto / Always / Never).
        let tab_bar_mode = match config.show_tab_bar {
            crate::app::TabBarMode::Auto => 0,
            crate::app::TabBarMode::Always => 1,
            crate::app::TabBarMode::Never => 2,
        };

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
        let cursor_style_idx = match config.cursor_style {
            crate::app::CursorStyleConfig::Block => 0,
            crate::app::CursorStyleConfig::Beam => 1,
            crate::app::CursorStyleConfig::Underline => 2,
        };
        // Custom-theme editor view data + the runtime profile names.
        let custom_labels: Vec<&str> = crate::app::settings_themes::CUSTOM_THEME_LABELS.to_vec();
        let profile_refs: Vec<&str> = profile_names.iter().map(|s| s.as_str()).collect();

        // settings-sections stream: Terminal / Effects / Quake / Notifications /
        // Advanced display strings + per-side padding (0 = unset, same sentinel
        // convention as the uniform `padding_px` above).
        let font_symbol_map_str = symbol_map_display(&config.font_symbol_map);
        let font_variations_str = config.font_variations.join(" ");
        let status_bar_segments_str =
            status_bar_segments_display(config.status_bar_segments.as_deref());
        let shell_str = shell_display(&config.shell);
        let cwd_str = config
            .initial_cwd
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let hints_chars_str = config.hints_chars.clone().unwrap_or_default();
        let font_bold_str = config.font_bold.clone().unwrap_or_default();
        let font_italic_str = config.font_italic.clone().unwrap_or_default();
        let font_bold_italic_str = config.font_bold_italic.clone().unwrap_or_default();
        let wallpaper_theme_str = config
            .wallpaper_theme
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let padding_top_px = config.padding_top.unwrap_or(0.0).round().max(0.0) as u32;
        let padding_bottom_px = config.padding_bottom.unwrap_or(0.0).round().max(0.0) as u32;
        let padding_left_px = config.padding_left.unwrap_or(0.0).round().max(0.0) as u32;
        let padding_right_px = config.padding_right.unwrap_or(0.0).round().max(0.0) as u32;

        let view = gui::SettingsView {
            font_px,
            opacity: config.opacity,
            bell: bell_idx,
            theme_idx,
            theme_names: &theme_names,
            theme_swatches: &swatches,
            font_family: font_display,
            resolved_font_family: &resolved_font_family_str,
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
            cursor_style_idx,
            cursor_blink: config.cursor_blink,
            tab_bar_mode,
            window_effect_idx: config.window_effect.index(),
            custom_effect: config.custom_effect,
            section,
            section_scroll,
            copy_on_select: config.copy_on_select,
            minimap: config.minimap,
            command_badges: config.command_badges,
            cursor_trail: config.cursor_trail,
            title_show_cwd: config.title_show_cwd,
            title_show_count: config.title_show_count,
            theme_light: &config.theme_light,
            theme_dark: &config.theme_dark,
            custom_labels: &custom_labels,
            custom_swatches,
            custom_editing,
            profile_names: &profile_refs,
            active_profile,
            profile_rename_idx,
            profile_delete_armed,
            power_mode: config.power_mode,
            power_mode_intensity: config.power_mode_intensity,
            dim_unfocused: config.dim_unfocused,
            copy_html: config.copy_html,
            quake: config.quake,
            quake_height: config.quake_height,
            quake_animation_ms: config.quake_animation_ms,
            decorations: config.decorations,
            notify_command_finish: config.notify_command_finish,
            notify_command_threshold_ms: config.notify_command_threshold_ms,
            command_fold: config.command_fold,
            hints_chars: &hints_chars_str,
            font_bold: &font_bold_str,
            font_italic: &font_italic_str,
            font_bold_italic: &font_bold_italic_str,
            font_symbol_map: &font_symbol_map_str,
            font_variations: &font_variations_str,
            shell_display: &shell_str,
            cwd_display: &cwd_str,
            status_bar_segments: &status_bar_segments_str,
            status_bar_time_format: &config.status_bar_time_format,
            padding_top: padding_top_px,
            padding_bottom: padding_bottom_px,
            padding_left: padding_left_px,
            padding_right: padding_right_px,
            wallpaper_theme: &wallpaper_theme_str,
            popup_scroll,
            unfocused_dim: config.unfocused_dim,
            opacity_scope: if config.opacity_text { 1 } else { 0 },
            command_blocks: match config.command_blocks {
                crate::app::CommandBlocksMode::Off => 0,
                crate::app::CommandBlocksMode::Badges => 1,
                crate::app::CommandBlocksMode::Cards => 2,
            },
            pane_header_style: match config.pane_header_style {
                crate::app::panes::PaneHeaderStyle::Full => 0,
                crate::app::panes::PaneHeaderStyle::Compact => 1,
            },
            pane_headers_single: config.pane_headers_single,
            scrollback_background_cap: config.scrollback_background_cap,
            scrollback_background_idle_secs: config.scrollback_background_idle_secs,
        };
        ui.build_settings((sw as f32, sh as f32), &view, fields)
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
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::Theme {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Theme
            });
            changed = true;
        }
        if let Some(t) = ev.theme_pick {
            self.set_theme_by_idx(t);
            self.set_settings_drop(gui::SettingsDrop::None);
            changed = true;
        }
        if ev.font_toggle {
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::Font {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Font
            });
            changed = true;
        }
        if let Some(f) = ev.font_pick {
            self.set_font_family_index(f);
            self.set_settings_drop(gui::SettingsDrop::None);
            changed = true;
        }
        if ev.scrollback_delta != 0 {
            self.adjust_scrollback(ev.scrollback_delta);
            changed = true;
        }
        if let Some(idx) = ev.tab_bar_mode {
            self.set_tab_bar_mode_index(idx);
            changed = true;
        }
        if ev.window_effect_toggle {
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::Effect {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Effect
            });
            changed = true;
        }
        if let Some(idx) = ev.window_effect {
            self.set_window_effect_index(idx);
            self.set_settings_drop(gui::SettingsDrop::None);
            changed = true;
        }
        if let Some((ch, val)) = ev.custom_effect
            && ch < self.config.custom_effect.len()
        {
            self.config.custom_effect[ch] = val.clamp(0.0, 1.0);
            self.apply_custom_effect();
            changed = true;
        }
        if ev.padding_delta != 0 {
            self.adjust_padding(ev.padding_delta);
            changed = true;
        }
        if ev.padding_top_delta != 0 {
            self.adjust_padding_top(ev.padding_top_delta);
            changed = true;
        }
        if ev.padding_bottom_delta != 0 {
            self.adjust_padding_bottom(ev.padding_bottom_delta);
            changed = true;
        }
        if ev.padding_left_delta != 0 {
            self.adjust_padding_left(ev.padding_left_delta);
            changed = true;
        }
        if ev.padding_right_delta != 0 {
            self.adjust_padding_right(ev.padding_right_delta);
            changed = true;
        }
        if ev.quake_animation_delta != 0 {
            self.adjust_quake_animation_ms(ev.quake_animation_delta);
            changed = true;
        }
        if ev.notify_threshold_delta != 0 {
            self.adjust_notify_threshold_ms(ev.notify_threshold_delta);
            changed = true;
        }
        if let Some(h) = ev.quake_height {
            self.config.quake_height = h.clamp(
                crate::config::parse::QUAKE_HEIGHT_MIN,
                crate::config::parse::QUAKE_HEIGHT_MAX,
            );
            self.settings_saved = false;
            changed = true;
        }
        if let Some(i) = ev.power_mode_intensity {
            let i = i.clamp(0.0, 1.0);
            self.config.power_mode_intensity = i;
            self.power.set_intensity(i);
            self.settings_saved = false;
            changed = true;
        }
        if let Some(cs_idx) = ev.cursor_style {
            self.set_cursor_style_index(cs_idx);
            changed = true;
        }
        // --- settings-modularity stream: remaining w15 config keys ---
        if let Some(d) = ev.unfocused_dim {
            self.config.unfocused_dim = d.clamp(0.0, 0.9);
            self.settings_saved = false;
            changed = true;
        }
        if let Some(idx) = ev.opacity_scope {
            self.config.opacity_text = idx != 0;
            self.settings_saved = false;
            changed = true;
        }
        if let Some(idx) = ev.command_blocks {
            self.config.command_blocks = match idx {
                0 => crate::app::CommandBlocksMode::Off,
                2 => crate::app::CommandBlocksMode::Cards,
                _ => crate::app::CommandBlocksMode::Badges,
            };
            self.settings_saved = false;
            changed = true;
        }
        if let Some(idx) = ev.pane_header_style {
            self.config.pane_header_style = if idx == 1 {
                crate::app::panes::PaneHeaderStyle::Compact
            } else {
                crate::app::panes::PaneHeaderStyle::Full
            };
            self.settings_saved = false;
            changed = true;
        }
        if ev.scrollback_background_cap_delta != 0 {
            self.adjust_scrollback_background_cap(ev.scrollback_background_cap_delta);
            changed = true;
        }
        if ev.scrollback_background_idle_secs_delta != 0 {
            self.adjust_scrollback_background_idle_secs(ev.scrollback_background_idle_secs_delta);
            changed = true;
        }
        // --- settings-themes stream events ---
        if let Some(idx) = ev.section_pick {
            self.settings_set_section(idx);
            changed = true;
        }
        if let Some(s) = ev.section_scroll {
            self.settings_section_scroll = s;
            changed = true;
        }
        if let Some(s) = ev.popup_scroll {
            self.settings_popup_scroll = s;
            changed = true;
        }
        // Every boolean toggle row fired this frame (see `SettingsEvents::toggled`'s
        // doc comment for why this replaced a dedicated `*_toggle: bool` field +
        // `if` block per toggle). Toggles with extra live side effects (grid
        // reflow, renderer sync, session-dirty) are matched explicitly; everything
        // else is a plain flip resolved via `CONFIG_TOGGLES`.
        for &wid in &ev.toggled {
            match wid {
                "settings/status_bar" => {
                    self.toggle_status_bar();
                }
                "settings/pane_headers" => {
                    self.toggle_pane_headers();
                }
                "settings/ligatures" => {
                    self.config.ligatures = !self.config.ligatures;
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_ligatures(self.config.ligatures);
                    }
                    self.settings_saved = false;
                }
                "settings/restore_session" => {
                    self.config.restore_session = !self.config.restore_session;
                    self.session_dirty = true;
                    self.settings_saved = false;
                }
                "settings/cursor_trail" => {
                    self.config.cursor_trail = !self.config.cursor_trail;
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_cursor_trail(self.config.cursor_trail);
                    }
                    self.settings_saved = false;
                }
                "settings/power_mode" => {
                    // Runtime `PowerState::enabled` is separate from
                    // `config.power_mode` (seeded from it once at `App::new`, then
                    // independently runtime-toggled by the command palette) — flip
                    // both so a live settings toggle and Save agree with the
                    // palette's own toggle. `set_power_mode` also clears any live
                    // particles/shake when turning off.
                    self.config.power_mode = !self.config.power_mode;
                    self.set_power_mode(self.config.power_mode);
                    self.settings_saved = false;
                }
                "settings/command_fold" => {
                    // Mirrors `apply_config_reload`'s (helpers.rs) `command_fold`
                    // side effect: clearing an active fold state when the feature
                    // is turned off, so the view reverts to fully-expanded output
                    // instead of leaving stale folds the user can no longer toggle.
                    self.config.command_fold = !self.config.command_fold;
                    if !self.config.command_fold && self.fold_state.any() {
                        self.fold_state = command_blocks::FoldState::default();
                        self.force_full_redraw = true;
                    }
                    self.settings_saved = false;
                }
                other => {
                    if let Some((_, apply)) = CONFIG_TOGGLES.iter().find(|(id, _)| *id == other) {
                        *apply(&mut self.config) ^= true;
                        self.settings_saved = false;
                    } else {
                        log::debug!("glassy: settings toggle: unknown widget id '{other}'");
                    }
                }
            }
            changed = true;
        }
        if ev.theme_light_toggle {
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::ThemeLight {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::ThemeLight
            });
            changed = true;
        }
        if let Some(idx) = ev.theme_light_pick {
            self.set_theme_light_by_idx(idx);
            self.set_settings_drop(gui::SettingsDrop::None);
            changed = true;
        }
        if ev.theme_dark_toggle {
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::ThemeDark {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::ThemeDark
            });
            changed = true;
        }
        if let Some(idx) = ev.theme_dark_pick {
            self.set_theme_dark_by_idx(idx);
            self.set_settings_drop(gui::SettingsDrop::None);
            changed = true;
        }
        if let Some(idx) = ev.custom_color_pick {
            self.select_custom_color(idx);
            changed = true;
        }
        if ev.custom_apply {
            self.apply_custom_theme_preview();
            changed = true;
        }
        if ev.custom_save {
            self.save_custom_theme();
            changed = true;
        }
        if let Some(idx) = ev.profile_pick {
            self.switch_profile_by_idx(idx);
            changed = true;
        }
        if ev.profile_pick_default {
            self.switch_to_base_profile();
            changed = true;
        }
        if ev.profile_create {
            self.create_profile_from_current();
            changed = true;
        }
        if let Some(idx) = ev.profile_rename_begin {
            self.begin_profile_rename(idx);
            changed = true;
        }
        if ev.profile_rename_commit {
            self.commit_profile_rename();
            changed = true;
        }
        if ev.profile_rename_cancel {
            self.cancel_profile_rename();
            changed = true;
        }
        if let Some(idx) = ev.profile_delete_arm {
            self.arm_profile_delete(idx);
            changed = true;
        }
        if let Some(idx) = ev.profile_delete {
            self.delete_profile(idx);
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
            self.set_settings_drop(gui::SettingsDrop::None);
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
}
