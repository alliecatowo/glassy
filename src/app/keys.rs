//! Keyboard input handler extracted from `event_loop.rs`.
//!
//! Mouse input (cursor motion, buttons, scroll wheel) lives in `mouse.rs`.
//! The dispatcher in `event_loop.rs` calls `handle_keyboard` after consuming
//! synthetic key events.

use super::*;

impl App {
    /// Handle a `WindowEvent::KeyboardInput` event. Returns early (without
    /// marking dirty) when the event is fully consumed internally; callers
    /// must check the `mark_dirty` path themselves only when needed.
    pub(super) fn handle_keyboard(
        &mut self,
        event: winit::event::KeyEvent,
        event_loop: &ActiveEventLoop,
    ) {
        // F11 toggles borderless fullscreen. Handled first so it works
        // whether or not an overlay owns the keyboard, and never reaches
        // the child.
        if event.state.is_pressed()
            && matches!(&event.logical_key, Key::Named(NamedKey::F11))
        {
            if let Some(w) = self.window.as_ref() {
                let fs = if w.fullscreen().is_some() {
                    None
                } else {
                    Some(winit::window::Fullscreen::Borderless(None))
                };
                w.set_fullscreen(fs);
            }
            return;
        }

        // The command palette and the find bar own the keyboard while
        // open: every key is routed to them (query edit, list nav, jump,
        // Esc) and never reaches the child or the chrome shortcuts below.
        // Checked before the Ctrl+Shift block so typing letters into the
        // query isn't stolen by the clipboard/tab combos.
        if event.state.is_pressed() && self.palette.is_some() {
            if self.handle_palette_key(&event.logical_key, event_loop) {
                return;
            }
            return; // consume everything while the palette is up
        }
        if event.state.is_pressed() && self.search.is_some() {
            if self.handle_search_key(&event.logical_key, event_loop) {
                return;
            }
            return; // consume everything while the find bar is up
        }
        // The inline tab-rename editor owns the keyboard while open: text
        // edits the title, Enter commits, Esc cancels. Consume everything.
        if event.state.is_pressed() && self.is_renaming_tab() {
            self.handle_rename_key(&event.logical_key, event_loop);
            return;
        }

        // Ctrl+Shift clipboard combos are consumed by glassy and never
        // reach the child. Intercepted before `encode_key` so the control
        // byte for C/V isn't sent to the PTY.
        if event.state.is_pressed()
            && self.mods.control_key()
            && self.mods.shift_key()
            && let Key::Character(s) = &event.logical_key
        {
            match s.as_str() {
                "C" | "c" => {
                    self.copy_selection();
                    return;
                }
                "V" | "v" => {
                    self.paste_clipboard();
                    self.mark_dirty(event_loop);
                    return;
                }
                "T" | "t" => {
                    self.new_tab(event_loop);
                    return;
                }
                "W" | "w" => {
                    // Close the focused pane; falls back to closing the tab
                    // when the tab has only a single pane.
                    self.close_pane(event_loop);
                    return;
                }
                // Ctrl+Shift+P opens the command palette (fuzzy action +
                // settings list). Ctrl+Shift+F opens the in-terminal find
                // bar. Both own the keyboard while open; handled below.
                "P" | "p" => {
                    self.open_palette(event_loop);
                    return;
                }
                "F" | "f" => {
                    self.open_search(event_loop);
                    return;
                }
                // Split the focused pane: E = vertical (left|right),
                // O = horizontal (top/bottom). Mirrors common terminals.
                "E" | "e" => {
                    self.split_pane(pane::Dir::Vertical, event_loop);
                    return;
                }
                "O" | "o" => {
                    self.split_pane(pane::Dir::Horizontal, event_loop);
                    return;
                }
                _ => {}
            }
        }

        // Alt+Arrow moves focus between tiled panes (no-op when not split,
        // so a single-pane tab passes Alt+Arrow through to the child).
        if event.state.is_pressed()
            && self.mods.alt_key()
            && !self.mods.control_key()
            && self.is_split()
            && let Key::Named(named) = &event.logical_key
        {
            let m = match named {
                NamedKey::ArrowLeft => Some(pane::Move::Left),
                NamedKey::ArrowRight => Some(pane::Move::Right),
                NamedKey::ArrowUp => Some(pane::Move::Up),
                NamedKey::ArrowDown => Some(pane::Move::Down),
                _ => None,
            };
            if let Some(m) = m {
                self.focus_pane(m, event_loop);
                return;
            }
        }

        // Ctrl+Tab / Ctrl+Shift+Tab cycle between tabs.
        if event.state.is_pressed()
            && self.mods.control_key()
            && let Key::Named(NamedKey::Tab) = &event.logical_key
        {
            let delta = if self.mods.shift_key() { -1 } else { 1 };
            self.cycle_tab(delta, event_loop);
            return;
        }

        // Ctrl +/-/0 adjusts the font size at runtime (and Ctrl 0 resets
        // to the configured size). Intercepted before `encode_key` so the
        // control bytes for these keys never reach the child. Matches the
        // de-facto terminal/browser zoom convention. Shift is allowed (so
        // Ctrl+Shift+'=' i.e. Ctrl+'+' works) but not required.
        if event.state.is_pressed()
            && self.mods.control_key()
            && !self.mods.alt_key()
            && let Key::Character(s) = &event.logical_key
        {
            let step = match s.as_str() {
                "+" | "=" => Some(FontStep::Inc),
                "-" | "_" => Some(FontStep::Dec),
                "0" => Some(FontStep::Reset),
                _ => None,
            };
            if let Some(step) = step {
                self.resize_font(step);
                self.mark_dirty(event_loop);
                return;
            }
        }

        // Ctrl+Shift+B toggles the status bar (with Shift the char arrives
        // upper-case on most layouts, so accept either case).
        if event.state.is_pressed()
            && self.mods.control_key()
            && self.mods.shift_key()
            && let Key::Character(s) = &event.logical_key
            && matches!(s.as_str(), "b" | "B")
        {
            self.toggle_status_bar();
            self.mark_dirty(event_loop);
            return;
        }

        // Shift + PageUp/PageDown/Home/End drives glassy's own scrollback
        // (the primary screen only) and is consumed before the child sees
        // it. This mirrors the de-facto terminal convention.
        if event.state.is_pressed()
            && self.mods.shift_key()
            && !self.term_mode().contains(TermMode::ALT_SCREEN)
            && let Key::Named(named) = &event.logical_key
        {
            let scroll = match named {
                NamedKey::PageUp => Some(Scroll::PageUp),
                NamedKey::PageDown => Some(Scroll::PageDown),
                NamedKey::Home => Some(Scroll::Top),
                NamedKey::End => Some(Scroll::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                if let Some(pty) = &self.pty {
                    pty.term.lock().scroll_display(scroll);
                }
                self.mark_dirty(event_loop);
                return;
            }
        }

        // While the dropdown is open, Up/Down/Enter/Esc navigate it.
        // All other keys close it and pass through to the normal handler.
        if event.state.is_pressed() && self.menu_open {
            let key = &event.logical_key;
            if self.handle_menu_key(key, event_loop) {
                return;
            }
            // Any key that didn't navigate the menu closes it.
            self.close_menu(event_loop);
            // Fall through: let the keypress reach the child below.
        }

        // While the pane ⋮ menu is open, Up/Down/Enter/Esc navigate it.
        if event.state.is_pressed() && self.pane_menu_open.is_some() {
            let n = Self::PANE_MENU_ITEMS.len();
            let key = &event.logical_key;
            match key {
                Key::Named(NamedKey::ArrowUp) => {
                    self.pane_menu_sel = (self.pane_menu_sel + n - 1) % n;
                    self.mark_dirty(event_loop);
                    return;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.pane_menu_sel = (self.pane_menu_sel + 1) % n;
                    self.mark_dirty(event_loop);
                    return;
                }
                Key::Named(NamedKey::Enter) => {
                    let idx = self.pane_menu_sel;
                    self.invoke_pane_menu_action(idx, event_loop);
                    return;
                }
                Key::Named(NamedKey::Escape) => {
                    self.pane_menu_open = None;
                    self.mark_dirty(event_loop);
                    return;
                }
                _ => {
                    // Any other key closes the menu; let it fall through.
                    self.pane_menu_open = None;
                    self.mark_dirty(event_loop);
                }
            }
        }

        // While an overlay is open it owns the keyboard — nothing reaches
        // the child. Esc / F1 / Ctrl+, close it; settings handles nav/edit.
        if event.state.is_pressed() && (self.help_open || self.settings_open) {
            let key = &event.logical_key;
            let toggle_settings = self.mods.control_key()
                && matches!(key, Key::Character(s) if s.as_str() == ",");
            // Esc inside settings closes an open dropdown first; only a
            // second Esc (or F1 / Ctrl+,) closes the whole panel.
            if self.settings_open
                && matches!(key, Key::Named(NamedKey::Escape))
                && self.settings_drop != gui::SettingsDrop::None
            {
                self.settings_drop = gui::SettingsDrop::None;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                return;
            }
            if matches!(key, Key::Named(NamedKey::Escape | NamedKey::F1))
                || toggle_settings
            {
                self.help_open = false;
                self.settings_open = false;
                self.settings_drop = gui::SettingsDrop::None;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                return;
            }
            if self.settings_open {
                self.handle_settings_key(key.clone(), event_loop);
            }
            return; // consume all other keys while an overlay is up
        }

        // Open an overlay (only when none is up).
        if event.state.is_pressed() {
            if let Key::Named(NamedKey::F1) = &event.logical_key {
                self.help_open = true;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                return;
            }
            if self.mods.control_key()
                && matches!(&event.logical_key, Key::Character(s) if s.as_str() == ",")
            {
                self.open_settings();
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                return;
            }
        }

        // Build kitty keyboard protocol flags from the terminal's current mode.
        // Level 1 (DISAMBIGUATE_ESC_CODES) makes modified named keys go as
        // CSI-u. Higher levels add repeat/release events, alternate keys, and
        // the all-keys-as-esc form required by Helix, Neovim, etc.
        let mode = self.term_mode();
        let kitty = KittyFlags {
            disambiguate:           mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
            report_event_types:     mode.contains(TermMode::REPORT_EVENT_TYPES),
            report_alternate_keys:  mode.contains(TermMode::REPORT_ALTERNATE_KEYS),
            report_all_keys_as_esc: mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
            report_associated_text: mode.contains(TermMode::REPORT_ASSOCIATED_TEXT),
        };
        // DECCKM: arrows/Home/End go out as SS3 (ESC O X) for full-screen
        // apps (vim, less, ncurses) that enable application cursor-key mode.
        let app_cursor = mode.contains(TermMode::APP_CURSOR);
        if let Some(bytes) = encode_key(&event, self.mods, kitty, app_cursor, self.modify_other_keys) {
            // Typing resets the blink to solid-on so the cursor doesn't
            // wink out mid-keystroke, matching every mainstream terminal.
            self.reset_blink();
            // Typing dismisses any active selection, matching the de-facto
            // terminal convention.
            self.clear_selection();
            if let Some(pty) = &self.pty {
                // A typed key snaps the view back to the prompt, matching
                // every mainstream terminal.
                pty.term.lock().scroll_display(Scroll::Bottom);
                pty.write(bytes);
            }
            // The snap-to-bottom (and the cursor/selection reset above) are
            // visual changes even when the child emits nothing back — e.g.
            // typing while scrolled up into a paused/blocked program. Repaint
            // unconditionally so the view never stays frozen in scrollback.
            self.mark_dirty(event_loop);
        }
    }
}
