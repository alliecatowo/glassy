//! The declarative table [`SAVED_KEYS`] driving [`App::save_settings`]
//! (`settings.rs`): every `glassy.conf` key the settings UI can live-mutate,
//! paired with a `Config -> String` getter that renders the live value the same
//! way `config::save` expects to write it.
//!
//! This exists because `save_settings` used to hand-list a fixed set of
//! (key, value) pairs that drifted out of sync with `apply_settings_events`
//! (`chrome.rs`) as new settings-form controls were added — Save reported
//! success but silently dropped anything not in the hand-list, so the value
//! reverted on restart. Driving the save from one table, and asserting its
//! coverage in a test, makes that class of bug fail CI instead of shipping.

use super::*;

/// One persisted settings key: its `glassy.conf` name plus how to render the
/// current live [`Config`] value into the string [`crate::config::save`] writes
/// to disk (via [`crate::config::parse::apply_kv`] round-tripping it back on the
/// next launch — see the `saved_keys_round_trip_through_apply_kv` test in
/// `config/mod.rs`).
pub(crate) struct SavedKey {
    pub key: &'static str,
    pub get: fn(&Config) -> String,
}

/// Every `glassy.conf` key that the settings UI can live-mutate, EXCEPT
/// `font_size`: `App::save_settings` special-cases that one because the live,
/// user-visible size lives in the renderer's effective font px (Ctrl +/-/0 and
/// the settings stepper both drive it there), not in `Config::font_size`, which
/// only reflects the size at startup. Everything else here is read straight off
/// `Config` because the settings UI writes straight through to `Config` live
/// (see `App::apply_settings_events` in `chrome.rs`, `App::commit_settings_field`
/// in `settings_fields.rs`, and the system Light/Dark pickers in
/// `settings_themes.rs`).
///
/// [`saved_keys_cover_every_live_settable_field`] cross-checks this list against
/// a hand-maintained enumeration of every field those three places can mutate,
/// so a new settings-form control that forgets to add its key here fails CI
/// instead of silently reverting on restart.
pub(crate) const SAVED_KEYS: &[SavedKey] = &[
    SavedKey {
        key: "opacity",
        get: |c| format!("{:.2}", c.opacity),
    },
    SavedKey {
        key: "bell_visual",
        get: |c| c.bell_visual.to_string(),
    },
    SavedKey {
        key: "bell_audible",
        get: |c| c.bell_audible.to_string(),
    },
    SavedKey {
        key: "theme",
        get: |c| c.theme.clone(),
    },
    SavedKey {
        key: "font_family",
        get: |c| c.font_family.clone().unwrap_or_default(),
    },
    SavedKey {
        key: "scrollback",
        get: |c| c.scrollback.to_string(),
    },
    SavedKey {
        key: "status_bar",
        get: |c| c.status_bar.to_string(),
    },
    SavedKey {
        key: "pane_headers",
        get: |c| c.pane_headers.to_string(),
    },
    SavedKey {
        key: "show_tab_bar",
        get: |c| tab_bar_mode_word(c.show_tab_bar).to_string(),
    },
    SavedKey {
        key: "follow_system",
        get: |c| c.follow_system.to_string(),
    },
    SavedKey {
        key: "ligatures",
        get: |c| c.ligatures.to_string(),
    },
    SavedKey {
        key: "restore_session",
        get: |c| c.restore_session.to_string(),
    },
    SavedKey {
        key: "padding",
        get: |c| format!("{:.0}", c.padding.unwrap_or(0.0)),
    },
    SavedKey {
        key: "cursor_style",
        get: |c| c.cursor_style.as_str().to_string(),
    },
    SavedKey {
        key: "cursor_blink",
        get: |c| c.cursor_blink.to_string(),
    },
    SavedKey {
        key: "window_effect",
        get: |c| c.window_effect.as_str().to_string(),
    },
    // --- previously dropped silently by the hand-list (the bug this table fixes) ---
    SavedKey {
        key: "copy_on_select",
        get: |c| c.copy_on_select.to_string(),
    },
    SavedKey {
        key: "minimap",
        get: |c| c.minimap.to_string(),
    },
    SavedKey {
        key: "command_badges",
        get: |c| c.command_badges.to_string(),
    },
    SavedKey {
        key: "cursor_trail",
        get: |c| c.cursor_trail.to_string(),
    },
    SavedKey {
        key: "title_show_cwd",
        get: |c| c.title_show_cwd.to_string(),
    },
    SavedKey {
        key: "title_show_count",
        get: |c| c.title_show_count.to_string(),
    },
    SavedKey {
        key: "theme_light",
        get: |c| c.theme_light.clone(),
    },
    SavedKey {
        key: "theme_dark",
        get: |c| c.theme_dark.clone(),
    },
    SavedKey {
        key: "word_separator",
        get: |c| c.word_separator.clone(),
    },
    SavedKey {
        key: "font_features",
        get: |c| c.font_features.join(" "),
    },
    // Custom window-effect channel intensities — one key per slider in
    // `gui::settings_panel::CUSTOM_FX_SLIDERS`, same channel order
    // `[curvature, scanline, glow, vignette, grain, tint]`. Parsed back in
    // `config::parse::apply_kv`.
    SavedKey {
        key: "fx_curvature",
        get: |c| format!("{:.2}", c.custom_effect[0]),
    },
    SavedKey {
        key: "fx_scanline",
        get: |c| format!("{:.2}", c.custom_effect[1]),
    },
    SavedKey {
        key: "fx_glow",
        get: |c| format!("{:.2}", c.custom_effect[2]),
    },
    SavedKey {
        key: "fx_vignette",
        get: |c| format!("{:.2}", c.custom_effect[3]),
    },
    SavedKey {
        key: "fx_grain",
        get: |c| format!("{:.2}", c.custom_effect[4]),
    },
    SavedKey {
        key: "fx_tint",
        get: |c| format!("{:.2}", c.custom_effect[5]),
    },
    // --- settings-sections stream: Terminal / Effects / Quake / Notifications /
    // Advanced additions --------------------------------------------------------
    SavedKey {
        key: "padding_top",
        get: |c| format!("{:.0}", c.padding_top.unwrap_or(0.0)),
    },
    SavedKey {
        key: "padding_bottom",
        get: |c| format!("{:.0}", c.padding_bottom.unwrap_or(0.0)),
    },
    SavedKey {
        key: "padding_left",
        get: |c| format!("{:.0}", c.padding_left.unwrap_or(0.0)),
    },
    SavedKey {
        key: "padding_right",
        get: |c| format!("{:.0}", c.padding_right.unwrap_or(0.0)),
    },
    // Quake mode is restart-only (the window is armed once in `App::init_quake`
    // at startup — see `chrome.rs`'s CONFIG_TOGGLES entry doc), but the toggle
    // still writes through `Config` live so Save persists it for the next launch.
    SavedKey {
        key: "quake",
        get: |c| c.quake.to_string(),
    },
    SavedKey {
        key: "quake_height",
        get: |c| format!("{:.2}", c.quake_height),
    },
    SavedKey {
        key: "quake_animation_ms",
        get: |c| c.quake_animation_ms.to_string(),
    },
    // Decorations is restart-only (the frame is chosen once at window creation in
    // `resumed()`), but like `quake` the toggle writes through `Config` live so
    // Save persists it for the next launch.
    SavedKey {
        key: "decorations",
        get: |c| c.decorations.to_string(),
    },
    SavedKey {
        key: "power_mode",
        get: |c| c.power_mode.to_string(),
    },
    SavedKey {
        key: "power_mode_intensity",
        get: |c| format!("{:.2}", c.power_mode_intensity),
    },
    SavedKey {
        key: "dim_unfocused",
        get: |c| c.dim_unfocused.to_string(),
    },
    SavedKey {
        key: "unfocused_dim",
        get: |c| format!("{:.2}", c.unfocused_dim),
    },
    SavedKey {
        key: "opacity_scope",
        get: |c| {
            if c.opacity_text {
                "text".to_string()
            } else {
                "background".to_string()
            }
        },
    },
    SavedKey {
        key: "copy_html",
        get: |c| c.copy_html.to_string(),
    },
    SavedKey {
        key: "status_bar_segments",
        get: |c| status_bar_segments_display(c.status_bar_segments.as_deref()),
    },
    SavedKey {
        key: "status_bar_time_format",
        get: |c| c.status_bar_time_format.clone(),
    },
    SavedKey {
        key: "font_bold",
        get: |c| c.font_bold.clone().unwrap_or_default(),
    },
    SavedKey {
        key: "font_italic",
        get: |c| c.font_italic.clone().unwrap_or_default(),
    },
    SavedKey {
        key: "font_bold_italic",
        get: |c| c.font_bold_italic.clone().unwrap_or_default(),
    },
    SavedKey {
        key: "font_symbol_map",
        get: |c| symbol_map_display(&c.font_symbol_map),
    },
    SavedKey {
        key: "font_variations",
        get: |c| c.font_variations.join(" "),
    },
    SavedKey {
        key: "notify_command_finish",
        get: |c| c.notify_command_finish.to_string(),
    },
    SavedKey {
        key: "notify_command_threshold_ms",
        get: |c| c.notify_command_threshold_ms.to_string(),
    },
    SavedKey {
        key: "command_fold",
        get: |c| c.command_fold.to_string(),
    },
    SavedKey {
        key: "hints_chars",
        get: |c| c.hints_chars.clone().unwrap_or_default(),
    },
    SavedKey {
        key: "wallpaper_theme",
        get: |c| {
            c.wallpaper_theme
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        },
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `Config` field that the settings UI can live-mutate, hand-enumerated
    /// by reading `App::apply_settings_events` (`chrome.rs`),
    /// `App::commit_settings_field` (`settings_fields.rs`), and the system
    /// Light/Dark theme pickers (`settings_themes.rs`) end to end. `font_size` is
    /// intentionally excluded — see [`SAVED_KEYS`]'s doc comment.
    ///
    /// This is the coverage gate: when a new settings-form control starts
    /// live-mutating a `Config` field, it must be added BOTH here and to
    /// `SAVED_KEYS`, or this test fails — a forgotten `SAVED_KEYS` entry can no
    /// longer ship silently.
    const LIVE_SETTABLE_KEYS: &[&str] = &[
        "opacity",
        "bell_visual",
        "bell_audible",
        "theme",
        "font_family",
        "scrollback",
        "status_bar",
        "pane_headers",
        "show_tab_bar",
        "follow_system",
        "ligatures",
        "restore_session",
        "padding",
        "cursor_style",
        "cursor_blink",
        "window_effect",
        "copy_on_select",
        "minimap",
        "command_badges",
        "cursor_trail",
        "title_show_cwd",
        "title_show_count",
        "theme_light",
        "theme_dark",
        "word_separator",
        "font_features",
        "fx_curvature",
        "fx_scanline",
        "fx_glow",
        "fx_vignette",
        "fx_grain",
        "fx_tint",
        "padding_top",
        "padding_bottom",
        "padding_left",
        "padding_right",
        "quake",
        "quake_height",
        "quake_animation_ms",
        "decorations",
        "power_mode",
        "power_mode_intensity",
        "dim_unfocused",
        "unfocused_dim",
        "opacity_scope",
        "copy_html",
        "status_bar_segments",
        "status_bar_time_format",
        "font_bold",
        "font_italic",
        "font_bold_italic",
        "font_symbol_map",
        "font_variations",
        "notify_command_finish",
        "notify_command_threshold_ms",
        "command_fold",
        "hints_chars",
        "wallpaper_theme",
    ];

    #[test]
    fn saved_keys_cover_every_live_settable_field() {
        let mut saved: Vec<&str> = SAVED_KEYS.iter().map(|k| k.key).collect();
        saved.sort_unstable();
        let mut want: Vec<&str> = LIVE_SETTABLE_KEYS.to_vec();
        want.sort_unstable();
        assert_eq!(
            saved, want,
            "SAVED_KEYS must cover exactly the live-settable Config fields \
             (font_size is special-cased in App::save_settings)"
        );
    }

    #[test]
    fn saved_keys_has_no_duplicate_keys() {
        let mut keys: Vec<&str> = SAVED_KEYS.iter().map(|k| k.key).collect();
        let n = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), n, "duplicate key in SAVED_KEYS");
    }
}
