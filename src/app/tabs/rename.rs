//! Inline tab-rename editor: begin / key-handle / commit / cancel.

use super::super::*;

/// Snapshot of the inline rename editor for the painter: `(pos, buffer, caret,
/// selection)`, where `caret`/`selection` are char offsets into `buffer`.
pub(crate) type RenameSnapshot = (usize, String, usize, Option<(usize, usize)>);

impl App {
    /// The custom title for the tab at stable position `pos`, if the user set one.
    pub(crate) fn custom_title_at(&self, pos: usize) -> Option<&str> {
        let id = *self.tab_order.get(pos)?;
        if id == self.active_id {
            self.active_custom_title.as_deref()
        } else {
            self.background
                .iter()
                .find(|s| s.id == id)
                .and_then(|s| s.custom_title.as_deref())
        }
    }

    /// Open the inline rename editor for the tab at stable position `pos`, seeding
    /// the buffer from the current custom title (or empty to type a fresh name).
    /// Closes any conflicting overlay so the chip editor owns the keyboard.
    pub(crate) fn begin_tab_rename(&mut self, pos: usize, event_loop: &ActiveEventLoop) {
        if pos >= self.tab_order.len() {
            return;
        }
        let seed = self.custom_title_at(pos).unwrap_or("").to_string();
        // Cap the title so a held key / paste can't grow it unbounded.
        self.tab_rename = Some((pos, gui::TextEdit::with_max_len(&seed, 64)));
        // The rename editor owns the keyboard; dismiss other overlays/menus.
        self.menu_open = false;
        self.menu_items = None;
        self.pane_menu_open = None;
        self.help_open = false;
        self.settings_open = false;
        self.overlay_opened_by_press = false;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Whether the inline tab-rename editor is currently open.
    pub(crate) fn is_renaming_tab(&self) -> bool {
        self.tab_rename.is_some()
    }

    /// Snapshot of the rename editor for the painter (see [`RenameSnapshot`]).
    pub(crate) fn tab_rename_state(&self) -> Option<RenameSnapshot> {
        self.tab_rename
            .as_ref()
            .map(|(p, e)| (*p, e.text(), e.caret(), e.selection()))
    }

    /// Set (or clear) the custom title of the tab at stable position `pos`. An
    /// empty/whitespace title clears the override (reverts to the OSC title).
    pub(super) fn set_custom_title(&mut self, pos: usize, title: Option<String>) {
        let Some(&id) = self.tab_order.get(pos) else {
            return;
        };
        let title = title.filter(|t| !t.trim().is_empty());
        if id == self.active_id {
            self.active_custom_title = title;
            self.update_window_title();
        } else if let Some(s) = self.background.iter_mut().find(|s| s.id == id) {
            s.custom_title = title;
        }
    }

    /// Commit the inline rename (Enter): apply the buffer as the tab's custom title
    /// and close the editor. An empty buffer clears any existing custom title.
    pub(crate) fn commit_tab_rename(&mut self, event_loop: &ActiveEventLoop) {
        if let Some((pos, edit)) = self.tab_rename.take() {
            self.set_custom_title(pos, Some(edit.text()));
            self.save_session();
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
        }
    }

    /// Cancel the inline rename (Esc): discard the buffer, keep the prior title.
    pub(crate) fn cancel_tab_rename(&mut self, event_loop: &ActiveEventLoop) {
        if self.tab_rename.take().is_some() {
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
        }
    }

    /// Handle a keypress while the inline rename editor is open. Returns `true` if
    /// the key was consumed (it never reaches the child). Enter commits, Esc
    /// cancels; all other editing (caret nav, selection, word-jump, clipboard)
    /// flows through the shared [`gui::TextEdit`] path every glassy field uses.
    pub(crate) fn handle_rename_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) -> bool {
        if self.tab_rename.is_none() {
            return false;
        }
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let (named, text) = super::super::settings::key_to_text_parts(key);
        let action = gui::map_text_key(named.as_deref(), text.as_deref(), ctrl, shift);
        match action {
            gui::TextInputAction::Cancel => {
                self.cancel_tab_rename(event_loop);
                return true;
            }
            gui::TextInputAction::Submit => {
                self.commit_tab_rename(event_loop);
                return true;
            }
            gui::TextInputAction::None => return false,
            _ => {}
        }
        let paste_text = if matches!(action, gui::TextInputAction::Paste) {
            self.clipboard_text()
        } else {
            None
        };
        let Some((_, edit)) = self.tab_rename.as_mut() else {
            return false;
        };
        let res = gui::apply_text_action(edit, action, paste_text.as_deref());
        match &res.clip {
            gui::ClipReq::Copy(s) | gui::ClipReq::Cut(s) => {
                let owned = s.clone();
                self.copy_text_to_clipboard(&owned);
            }
            gui::ClipReq::None | gui::ClipReq::Paste => {}
        }
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
        true
    }
}
