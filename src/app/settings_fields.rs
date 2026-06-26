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
                    ..TermConfig::default()
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
        }
    }
}
