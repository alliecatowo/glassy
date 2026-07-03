//! Settings-themes stream: the sectioned settings window's extra behaviours —
//! the custom-theme editor, left-sidebar section navigation, system light/dark
//! theme pickers, and the live runtime profile switch.
//!
//! Split out of `settings.rs` so that file stays focused on the original
//! adjust/save flow. The custom-theme working palette lives on `App` as
//! `settings_custom` (4 specials + 16 ANSI entries, all `Rgb`).

use super::*;
use alacritty_terminal::vte::ansi::Rgb;

/// Display labels for the 20 custom-theme entries, in editor order (matches the
/// `settings_custom` array layout: 4 specials, then ansi0..15).
pub(crate) const CUSTOM_THEME_LABELS: [&str; 20] = [
    "fg",
    "bg",
    "cursor",
    "selection",
    "ansi0",
    "ansi1",
    "ansi2",
    "ansi3",
    "ansi4",
    "ansi5",
    "ansi6",
    "ansi7",
    "ansi8",
    "ansi9",
    "ansi10",
    "ansi11",
    "ansi12",
    "ansi13",
    "ansi14",
    "ansi15",
];

/// Parse a `#rrggbb` (or `rrggbb`) hex string into an `Rgb`, returning `None` on
/// any malformed input. Local to the app so the config parser's private helper is
/// not exposed.
fn parse_hex(s: &str) -> Option<Rgb> {
    let hex = s.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some(Rgb { r, g, b })
}

impl App {
    /// Seed the working custom-theme palette from the live active theme so the
    /// editor starts from whatever is currently on screen.
    pub(crate) fn seed_custom_theme(&mut self) {
        let t = color::active_theme();
        self.settings_custom[0] = t.fg();
        self.settings_custom[1] = t.bg();
        self.settings_custom[2] = t.cursor();
        self.settings_custom[3] = t.selection_bg();
        for i in 0..16 {
            self.settings_custom[4 + i] = t.ansi(i);
        }
    }

    /// The current working custom-theme swatch colors as RGBA floats, parallel to
    /// [`CUSTOM_THEME_LABELS`], for the editor's swatch grid + the SettingsView.
    pub(crate) fn custom_theme_swatches(&self) -> [[f32; 4]; 20] {
        let mut out = [[0.0; 4]; 20];
        for (i, c) in self.settings_custom.iter().enumerate() {
            out[i] = [
                c.r as f32 / 255.0,
                c.g as f32 / 255.0,
                c.b as f32 / 255.0,
                1.0,
            ];
        }
        out
    }

    /// Select a custom-theme entry for editing: set the editing index, focus the
    /// hex field, and seed it with that entry's current hex value.
    pub(crate) fn select_custom_color(&mut self, idx: usize) {
        if idx >= self.settings_custom.len() {
            return;
        }
        self.settings_custom_editing = idx;
        let hex = color::rgb_to_hex(self.settings_custom[idx]);
        self.settings_theme_hex = gui::TextEdit::new(&hex);
        self.settings_theme_hex_ms = gui::TextInputMouse::default();
        self.gui_focused = Some(gui::id("settings/custom/hex"));
        self.settings_saved = false;
    }

    /// Parse the hex field into the currently-edited entry and live-preview the
    /// resulting custom theme. A no-op when the hex is malformed (so partial typing
    /// like "#7d" doesn't blow up the palette).
    pub(crate) fn apply_custom_hex(&mut self) {
        let idx = self.settings_custom_editing;
        if idx >= self.settings_custom.len() {
            return;
        }
        if let Some(rgb) = parse_hex(&self.settings_theme_hex.text()) {
            self.settings_custom[idx] = rgb;
            self.apply_custom_theme_preview();
        }
    }

    /// Build a [`color::Theme`] from the working custom palette.
    fn build_custom_theme(&self) -> color::Theme {
        let mut ansi16 = [Rgb { r: 0, g: 0, b: 0 }; 16];
        ansi16.copy_from_slice(&self.settings_custom[4..20]);
        color::theme_from_parts(
            self.settings_custom[0],
            self.settings_custom[1],
            self.settings_custom[2],
            self.settings_custom[3],
            ansi16,
        )
    }

    /// Install the working custom theme live (preview without persisting).
    pub(crate) fn apply_custom_theme_preview(&mut self) {
        color::set_theme(self.build_custom_theme());
        self.config.theme = "custom".to_string();
        self.force_full_redraw = true;
        self.settings_saved = false;
    }

    /// Persist the working custom theme to the config file as `color.*` overrides
    /// (plus a note that the theme is now custom). Shows a toast with the result.
    pub(crate) fn save_custom_theme(&mut self) {
        let c = &self.settings_custom;
        let mut updates: Vec<(&str, String)> = vec![
            ("color.fg", color::rgb_to_hex(c[0])),
            ("color.bg", color::rgb_to_hex(c[1])),
            ("color.cursor", color::rgb_to_hex(c[2])),
            ("color.selection_bg", color::rgb_to_hex(c[3])),
        ];
        // ansi0..15 — keep the &'static str keys alive for the save call.
        const ANSI_KEYS: [&str; 16] = [
            "color.ansi0",
            "color.ansi1",
            "color.ansi2",
            "color.ansi3",
            "color.ansi4",
            "color.ansi5",
            "color.ansi6",
            "color.ansi7",
            "color.ansi8",
            "color.ansi9",
            "color.ansi10",
            "color.ansi11",
            "color.ansi12",
            "color.ansi13",
            "color.ansi14",
            "color.ansi15",
        ];
        for (i, key) in ANSI_KEYS.iter().enumerate() {
            updates.push((key, color::rgb_to_hex(c[4 + i])));
        }
        match crate::config::save(&updates) {
            Ok(()) => {
                self.settings_saved = true;
                self.push_toast("Custom theme saved to config");
            }
            Err(e) => {
                log::error!("custom theme save failed: {e:#}");
                self.push_toast("Custom theme save failed");
            }
        }
    }

    /// Set the active settings sidebar section, resetting the right-pane scroll.
    pub(crate) fn settings_set_section(&mut self, idx: usize) {
        let n = gui::SettingsSection::ALL.len();
        if idx < n && idx != self.settings_section {
            self.settings_section = idx;
            self.settings_section_scroll = 0.0;
            self.force_full_redraw = true;
        }
    }

    /// Set the system Light-mode theme (Themes section dropdown). Applies live if
    /// follow_system is on and the OS currently prefers Light.
    pub(crate) fn set_theme_light_by_idx(&mut self, idx: usize) {
        if let Some(&name) = color::theme_names().get(idx) {
            self.config.theme_light = name.to_string();
            self.settings_saved = false;
            if self.config.follow_system {
                if let Some(window) = &self.window
                    && self.apply_system_theme(window.theme())
                {
                    self.force_full_redraw = true;
                }
            } else if let Some(theme) = color::theme_by_name(name) {
                // Not following the system: a Light-theme pick would otherwise have
                // no visible effect. Apply it as the active theme immediately — this
                // is also how you pick light-vs-dark when follow-system is off.
                color::set_theme(theme);
                self.config.theme = name.to_string();
                self.force_full_redraw = true;
            }
        }
    }

    /// Set the system Dark-mode theme (Themes section dropdown). Applies live if
    /// follow_system is on and the OS currently prefers Dark.
    pub(crate) fn set_theme_dark_by_idx(&mut self, idx: usize) {
        if let Some(&name) = color::theme_names().get(idx) {
            self.config.theme_dark = name.to_string();
            self.settings_saved = false;
            if self.config.follow_system {
                if let Some(window) = &self.window
                    && self.apply_system_theme(window.theme())
                {
                    self.force_full_redraw = true;
                }
            } else if let Some(theme) = color::theme_by_name(name) {
                // Not following the system: apply the picked Dark theme as active
                // immediately so it has a visible effect (and gives light/dark
                // selection without follow-system).
                color::set_theme(theme);
                self.config.theme = name.to_string();
                self.force_full_redraw = true;
            }
        }
    }

    /// Switch the live runtime profile by index into the cached profile-name list.
    /// Re-resolves the config file with the profile activated and applies the
    /// live-applicable settings (theme / opacity / bell / status bar / pane headers
    /// / word separators). Font-size and shell changes require a relaunch and are
    /// noted in the toast. A no-op for an out-of-range index.
    pub(crate) fn switch_profile_by_idx(&mut self, idx: usize) {
        let Some(name) = self.settings_profiles.get(idx).cloned() else {
            return;
        };
        self.switch_profile_by_name(&name);
    }

    /// Switch the live runtime profile by name (palette entry). See
    /// [`Self::switch_profile_by_idx`] for the applied-settings details.
    pub(crate) fn switch_profile_by_name(&mut self, name: &str) {
        match crate::config::Settings::resolve_with_profile(name) {
            Ok(settings) => {
                // Apply the live-applicable config deltas (opacity / bell / status /
                // panes / word seps) via the shared reload path FIRST. That path may
                // re-resolve the theme from its name; we then install the profile's
                // fully-resolved theme (which bakes in any `color.*` overrides) last
                // so those overrides are not clobbered.
                self.apply_config_reload(&settings.config);
                self.config.theme = settings.config.theme.clone();
                color::set_theme(settings.theme);
                self.active_profile = Some(name.to_ascii_lowercase());
                self.force_full_redraw = true;
                self.push_toast(format!(
                    "Switched to profile '{name}' (font/shell need relaunch)"
                ));
            }
            Err(e) => {
                log::warn!("profile switch '{name}' failed: {e:#}");
                self.push_toast(format!("Profile '{name}' switch failed"));
            }
        }
    }

    /// Switch back to the BASE (no-profile) config: re-resolves the on-disk file
    /// with no profile activated and applies it live via the same
    /// `apply_config_reload` path [`Self::switch_profile_by_name`] uses. This is
    /// the settings panel's "(default)" row — the only way back to the base
    /// config once a profile has been activated (there was previously no such
    /// path at runtime; only a relaunch without `--profile` could clear it).
    pub(crate) fn switch_to_base_profile(&mut self) {
        match crate::config::Settings::resolve_base() {
            Ok(settings) => {
                self.apply_config_reload(&settings.config);
                self.config.theme = settings.config.theme.clone();
                color::set_theme(settings.theme);
                self.active_profile = None;
                self.force_full_redraw = true;
                self.push_toast("Switched to default config (font/shell need relaunch)");
            }
            Err(e) => {
                log::warn!("switch to default config failed: {e:#}");
                self.push_toast("Switch to default config failed");
            }
        }
    }

    /// "Duplicate current settings as a new profile" (Profiles section): validate
    /// the pending name in `settings_profile_new`, then write the CURRENT live
    /// `Config` as a new `[profile.<name>]` section via
    /// [`crate::config::parse::save_into_section`], using the same
    /// `settings_save::SAVED_KEYS` table `App::save_settings` uses (so a profile
    /// snapshot always covers exactly the keys the settings UI can live-mutate —
    /// see that table's module doc for why it's the single source of truth).
    /// Refreshes the cached profile list on success so the new row shows
    /// immediately without reopening settings. Errors surface as a toast (never
    /// a panic) — this must never corrupt or lose the user's config file.
    pub(crate) fn create_profile_from_current(&mut self) {
        let name = self.settings_profile_new.text().trim().to_ascii_lowercase();
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            self.push_toast("Profile name must be letters, numbers, - or _");
            return;
        }
        // Re-read the profile list fresh (rather than trusting the cached
        // `settings_profiles`, which could be stale if the file changed since
        // settings opened) so the uniqueness check is against the real file.
        let existing = crate::config::profile_names();
        if existing.iter().any(|p| p.eq_ignore_ascii_case(&name)) {
            self.push_toast(format!("Profile '{name}' already exists"));
            return;
        }
        let updates: Vec<(&str, String)> = settings_save::SAVED_KEYS
            .iter()
            .map(|entry| (entry.key, (entry.get)(&self.config)))
            .collect();
        match crate::config::parse::save_into_section(Some(&name), &updates) {
            Ok(()) => {
                self.settings_profiles = crate::config::profile_names();
                self.settings_profile_new = gui::TextEdit::default();
                self.settings_profile_new_ms = gui::TextInputMouse::default();
                self.settings_saved = false;
                self.force_full_redraw = true;
                self.push_toast(format!("Profile '{name}' created"));
            }
            Err(e) => {
                log::error!("create profile '{name}' failed: {e:#}");
                self.push_toast(format!("Create profile '{name}' failed"));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parse_hex;

    #[test]
    fn parse_hex_accepts_with_and_without_hash() {
        assert_eq!(
            parse_hex("#7dcfff").map(|c| (c.r, c.g, c.b)),
            Some((0x7d, 0xcf, 0xff))
        );
        assert_eq!(
            parse_hex("1a1b26").map(|c| (c.r, c.g, c.b)),
            Some((0x1a, 0x1b, 0x26))
        );
    }

    #[test]
    fn parse_hex_rejects_malformed() {
        assert!(parse_hex("#7d").is_none());
        assert!(parse_hex("#zzzzzz").is_none());
        assert!(parse_hex("").is_none());
        assert!(parse_hex("#1234567").is_none());
    }

    #[test]
    fn custom_labels_cover_all_entries() {
        assert_eq!(super::CUSTOM_THEME_LABELS.len(), 20);
    }
}
