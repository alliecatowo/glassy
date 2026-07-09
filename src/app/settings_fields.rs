//! Editable settings text fields (Word seps / Font features): focus detection,
//! key routing through the shared [`gui::TextEdit`] pipeline, and committing the
//! edited value to the live config. Split out of `settings.rs` to keep that file
//! focused on the form's adjust/save logic.

use super::*;

impl App {
    /// True when the focused settings control is one of the editable text fields
    /// (Word seps / Font features). The settings key handler routes keypresses to
    /// the field's [`gui::TextEdit`] while one of these is focused.
    pub(crate) fn settings_editing_field(&self) -> Option<gui::WidgetId> {
        let f = self.gui_focused?;
        if f == gui::id("settings/word_separator")
            || f == gui::id("settings/font_features")
            || f == gui::id("settings/custom/hex")
            || f == gui::id("settings/profile_new_name")
            || f == gui::id("settings/profile_rename")
            || f == gui::id("settings/hints_chars")
            || f == gui::id("settings/font_bold")
            || f == gui::id("settings/font_italic")
            || f == gui::id("settings/font_bold_italic")
            || f == gui::id("settings/font_symbol_map")
            || f == gui::id("settings/font_variations")
            || f == gui::id("settings/status_bar_segments")
            || f == gui::id("settings/status_bar_time_format")
            || f == gui::id("settings/wallpaper_theme")
        {
            Some(f)
        } else {
            None
        }
    }

    /// Route one keypress to the focused settings text field (Word seps / Font
    /// features). Returns `true` when consumed. Enter/Esc/Tab fall through to the
    /// normal settings nav (so Enter still saves); everything else edits the
    /// buffer and applies the value live to the config.
    pub(crate) fn handle_settings_field_key(
        &mut self,
        key: &Key,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        let Some(field) = self.settings_editing_field() else {
            return false;
        };
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let (named, text) = super::settings::key_to_text_parts(key);
        let action = gui::map_text_key(named.as_deref(), text.as_deref(), ctrl, shift);
        // Submit/Cancel/Tab navigation stay with the form-level handler.
        if matches!(
            action,
            gui::TextInputAction::Submit | gui::TextInputAction::Cancel
        ) {
            return false;
        }
        if matches!(key, Key::Named(NamedKey::Tab)) {
            return false;
        }
        // Paste text must be fetched before borrowing the field (the model can't
        // reach the OS clipboard); for non-paste actions this is a cheap None.
        let paste_text = if matches!(action, gui::TextInputAction::Paste) {
            self.clipboard_text()
        } else {
            None
        };
        let edit = if field == gui::id("settings/word_separator") {
            &mut self.settings_word_sep
        } else if field == gui::id("settings/custom/hex") {
            &mut self.settings_theme_hex
        } else if field == gui::id("settings/profile_new_name") {
            &mut self.settings_profile_new
        } else if field == gui::id("settings/profile_rename") {
            &mut self.settings_profile_rename
        } else if field == gui::id("settings/hints_chars") {
            &mut self.settings_hints_chars
        } else if field == gui::id("settings/font_bold") {
            &mut self.settings_font_bold
        } else if field == gui::id("settings/font_italic") {
            &mut self.settings_font_italic
        } else if field == gui::id("settings/font_bold_italic") {
            &mut self.settings_font_bold_italic
        } else if field == gui::id("settings/font_symbol_map") {
            &mut self.settings_font_symbol_map
        } else if field == gui::id("settings/font_variations") {
            &mut self.settings_font_variations
        } else if field == gui::id("settings/status_bar_segments") {
            &mut self.settings_status_bar_segments
        } else if field == gui::id("settings/status_bar_time_format") {
            &mut self.settings_status_bar_time_format
        } else if field == gui::id("settings/wallpaper_theme") {
            &mut self.settings_wallpaper_theme
        } else {
            &mut self.settings_font_feat
        };
        let res = gui::apply_text_action(edit, action, paste_text.as_deref());
        // Service any copy/cut clipboard request (the field borrow has ended).
        match &res.clip {
            gui::ClipReq::Copy(s) | gui::ClipReq::Cut(s) => self.copy_text_to_clipboard(s),
            gui::ClipReq::None | gui::ClipReq::Paste => {}
        }
        if res.submit || res.cancel {
            return false;
        }
        if res.changed {
            self.commit_settings_field(field);
        }
        self.settings_saved = false;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
        true
    }

    /// Apply the editable field's current text to the live config (so the value
    /// is effective before Save persists it). Word separators re-derive the
    /// terminal's semantic-escape chars live; font features are parsed into tags.
    fn commit_settings_field(&mut self, field: gui::WidgetId) {
        if field == gui::id("settings/word_separator") {
            self.config.word_separator = self.settings_word_sep.text();
            let escape_chars = crate::pty::merge_word_separators(
                alacritty_terminal::term::SEMANTIC_ESCAPE_CHARS,
                &self.config.word_separator,
            );
            // Push the merged semantic-escape chars to every live PTY so
            // double-click word selection honours the new setting immediately.
            let push = |pty: &crate::pty::Pty, escape: &str| {
                use alacritty_terminal::term::Config as TermConfig;
                let scrollback = pty.term.lock().grid().history_size();
                pty.term.lock().set_options(TermConfig {
                    scrolling_history: scrollback,
                    semantic_escape_chars: escape.to_owned(),
                    // Spread the shared base (NOT TermConfig::default()) so this
                    // word-separator push can't silently disable kitty_keyboard.
                    ..crate::pty::term_config_base()
                });
            };
            if let Some(pty) = self.pty.as_ref() {
                push(pty, &escape_chars);
            }
            if let Some(g) = self.panes.as_ref() {
                for pty in g.others.values() {
                    push(pty, &escape_chars);
                }
            }
            for s in &self.background {
                push(&s.pty, &escape_chars);
                if let Some(g) = s.panes.as_ref() {
                    for pty in g.others.values() {
                        push(pty, &escape_chars);
                    }
                }
            }
        } else if field == gui::id("settings/font_features") {
            let text = self.settings_font_feat.text();
            self.config.font_features = text.split_whitespace().map(|s| s.to_string()).collect();
        } else if field == gui::id("settings/custom/hex") {
            // Live-parse the hex into the working custom-theme entry + preview it.
            self.apply_custom_hex();
        } else if field == gui::id("settings/hints_chars") {
            // Mirrors `apply_kv`'s finalization rule exactly (ASCII letters only,
            // >= 2 chars or fall back to the built-in default) via the shared
            // helper. Already-live: `App::open_hints` reads `config.hints_chars`
            // fresh every time hints mode is invoked, so this needs no renderer
            // sync — the very next Ctrl+Shift+H uses the new alphabet.
            let text = self.settings_hints_chars.text();
            self.config.hints_chars = crate::config::parse::normalize_hints_chars(&text);
        } else if field == gui::id("settings/font_bold") {
            // NOTE: font_bold/italic/bold_italic/symbol_map/font_variations are
            // baked into the font stack once at startup (`Renderer::new_with_fonts`
            // via `Text::load_with_config`); `Renderer::reload_fonts` (the live
            // family/features reload path) does not thread them through. There is
            // no live renderer-sync path for these today — see
            // `apply_config_reload` (helpers.rs), which likewise does not cover
            // them. Committing here still updates the live `Config` (so Save
            // persists the typed value and the change takes effect on restart),
            // matching the honest "restart required" labeling in the Terminal
            // section.
            let text = self.settings_font_bold.text();
            self.config.font_bold = (!text.is_empty()).then_some(text);
        } else if field == gui::id("settings/font_italic") {
            let text = self.settings_font_italic.text();
            self.config.font_italic = (!text.is_empty()).then_some(text);
        } else if field == gui::id("settings/font_bold_italic") {
            let text = self.settings_font_bold_italic.text();
            self.config.font_bold_italic = (!text.is_empty()).then_some(text);
        } else if field == gui::id("settings/font_symbol_map") {
            let text = self.settings_font_symbol_map.text();
            self.config.font_symbol_map = if text.trim().is_empty() {
                Vec::new()
            } else {
                crate::config::parse::parse_symbol_map(&text)
            };
        } else if field == gui::id("settings/font_variations") {
            let text = self.settings_font_variations.text();
            self.config.font_variations = if text.trim().is_empty() {
                Vec::new()
            } else {
                crate::config::parse::parse_font_variations(&text)
            };
        } else if field == gui::id("settings/status_bar_segments") {
            // Already-live: the status-bar paint call clones `status_bar_segments`
            // fresh every frame (see `render.rs`/`multipane.rs`), so no extra sync.
            let text = self.settings_status_bar_segments.text();
            self.config.status_bar_segments = if text.is_empty() {
                None
            } else {
                Some(crate::config::parse::parse_status_bar_segments(&text))
            };
        } else if field == gui::id("settings/status_bar_time_format") {
            let text = self.settings_status_bar_time_format.text();
            self.config.status_bar_time_format = if text.is_empty() {
                "%H:%M".to_string()
            } else {
                text
            };
        } else if field == gui::id("settings/wallpaper_theme") {
            // Path-only: does NOT re-run theme generation on every keystroke (that
            // would decode + resample an image on each character typed). Use the
            // existing "Generate theme from wallpaper" palette action once the
            // path is set, or restart, to apply it.
            let text = self.settings_wallpaper_theme.text();
            self.config.wallpaper_theme = (!text.is_empty()).then(|| text.into());
        }
    }
}
