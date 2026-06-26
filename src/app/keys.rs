//! Keyboard input handler extracted from `event_loop.rs`.
//!
//! Mouse input (cursor motion, buttons, scroll wheel) lives in `mouse.rs`.
//! The dispatcher in `event_loop.rs` calls `handle_keyboard` after consuming
//! synthetic key events.

use super::*;
use crate::config::{Chord, KeyAction};

/// Convert a winit key event + current modifiers into a [`Chord`] for keymap
/// lookup. Returns `None` for modifier-only keypresses (the key IS a modifier).
fn chord_from_event(logical_key: &Key, mods: ModifiersState) -> Option<Chord> {
    let ctrl = mods.control_key();
    let shift = mods.shift_key();
    let alt = mods.alt_key();
    let meta = mods.super_key();

    let key = match logical_key {
        Key::Character(s) => s.to_lowercase(),
        Key::Named(n) => named_key_to_str(n)?.to_string(),
        _ => return None,
    };
    Some(Chord {
        ctrl,
        shift,
        alt,
        meta,
        key,
    })
}

/// Map a winit `NamedKey` to the lowercase string used in chord parsing.
/// Returns `None` for keys that are purely modifiers (Shift, Ctrl, etc.).
fn named_key_to_str(key: &NamedKey) -> Option<&'static str> {
    Some(match key {
        NamedKey::Tab => "tab",
        NamedKey::Space => "space",
        NamedKey::Enter => "enter",
        NamedKey::Escape => "escape",
        NamedKey::Backspace => "backspace",
        NamedKey::Delete => "delete",
        NamedKey::Home => "home",
        NamedKey::End => "end",
        NamedKey::PageUp => "pageup",
        NamedKey::PageDown => "pagedown",
        NamedKey::ArrowUp => "arrowup",
        NamedKey::ArrowDown => "arrowdown",
        NamedKey::ArrowLeft => "arrowleft",
        NamedKey::ArrowRight => "arrowright",
        NamedKey::F1 => "f1",
        NamedKey::F2 => "f2",
        NamedKey::F3 => "f3",
        NamedKey::F4 => "f4",
        NamedKey::F5 => "f5",
        NamedKey::F6 => "f6",
        NamedKey::F7 => "f7",
        NamedKey::F8 => "f8",
        NamedKey::F9 => "f9",
        NamedKey::F10 => "f10",
        NamedKey::F11 => "f11",
        NamedKey::F12 => "f12",
        NamedKey::F13 => "f13",
        NamedKey::F14 => "f14",
        NamedKey::F15 => "f15",
        NamedKey::F16 => "f16",
        NamedKey::F17 => "f17",
        NamedKey::F18 => "f18",
        NamedKey::F19 => "f19",
        NamedKey::F20 => "f20",
        // Modifier-only keys: skip.
        NamedKey::Shift
        | NamedKey::Control
        | NamedKey::Alt
        | NamedKey::Super
        | NamedKey::Hyper
        | NamedKey::Meta
        | NamedKey::CapsLock
        | NamedKey::NumLock
        | NamedKey::ScrollLock => return None,
        _ => return None,
    })
}

impl App {
    /// Handle a `WindowEvent::KeyboardInput` event. Returns early (without
    /// marking dirty) when the event is fully consumed internally; callers
    /// must check the `mark_dirty` path themselves only when needed.
    pub(super) fn handle_keyboard(
        &mut self,
        event: winit::event::KeyEvent,
        event_loop: &ActiveEventLoop,
    ) {
        self.handle_keyboard_parts(
            event.logical_key,
            event.text,
            event.state,
            event.repeat,
            event_loop,
        );
    }

    /// Decomposed form of [`handle_keyboard`] used by both the real winit
    /// dispatch and the scripted-input test harness (`app/script.rs`), which
    /// cannot construct a winit `KeyEvent` (its `platform_specific` field is
    /// crate-private). Routing both paths through one body guarantees synthetic
    /// keys exercise the exact same overlay / keymap / encode logic as real ones.
    pub(super) fn handle_keyboard_parts(
        &mut self,
        logical_key: Key,
        text: Option<winit::keyboard::SmolStr>,
        state: ElementState,
        repeat: bool,
        event_loop: &ActiveEventLoop,
    ) {
        // Reconstruct the field-access shape the original body used so the logic
        // below is untouched: `event.state` / `event.logical_key` / `event.text`.
        struct Ev {
            logical_key: Key,
            text: Option<winit::keyboard::SmolStr>,
            state: ElementState,
            repeat: bool,
        }
        let event = Ev {
            logical_key,
            text,
            state,
            repeat,
        };
        // A pressed non-modifier key turns a bare modifier HOLD into a chord, so
        // disarm the numbered tab overlay. (Bare modifier presses come through as
        // `ModifiersChanged`, not here, so this only fires for real keys.) Done
        // before any consume-and-return path so the overlay always clears.
        if event.state.is_pressed() && !event.repeat {
            self.cancel_mod_hold(event_loop);
        }
        // Window-level shortcuts (fullscreen / maximize) are handled first,
        // before overlays, using the keymap so they can be rebound.
        // We consult the keymap for ToggleFullscreen and ToggleMaximize before
        // anything else so they work even when overlays are open.
        if event.state.is_pressed()
            && let Some(chord) = chord_from_event(&event.logical_key, self.mods)
        {
            match self.config.keymap.get(&chord) {
                Some(KeyAction::ToggleFullscreen) => {
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
                Some(KeyAction::ToggleMaximize) => {
                    if let Some(w) = self.window.as_ref() {
                        let maximized = w.is_maximized();
                        w.set_maximized(!maximized);
                    }
                    return;
                }
                _ => {}
            }
        }

        // An inline peek card is dismissed by the next keypress. Esc only
        // dismisses (it's consumed so it doesn't also reach the child); any other
        // key clears the card and falls through to its normal handling below.
        if event.state.is_pressed() && self.peek.is_some() {
            let is_esc = matches!(event.logical_key, Key::Named(NamedKey::Escape));
            self.dismiss_peek(event_loop);
            if is_esc {
                return;
            }
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
        // Hints mode owns the keyboard while open: every key narrows/fires a label
        // or cancels. Checked before the keymap dispatch so a bare label letter is
        // never stolen by a chord. (The open action itself is a keymap chord that
        // can only fire when the mode is closed.)
        if event.state.is_pressed() && self.hints_open() {
            self.handle_hints_key(&event.logical_key, event_loop);
            return; // consume everything while hints are up
        }
        // The inline tab-rename editor owns the keyboard while open: text
        // edits the title, Enter commits, Esc cancels. Consume everything.
        if event.state.is_pressed() && self.is_renaming_tab() {
            self.handle_rename_key(&event.logical_key, event_loop);
            return;
        }

        // --------------------------------------------------------------------
        // Leader / multi-key chord sequences: an armed prefix (or a chord that
        // begins one and has no flat action) is handled here BEFORE the flat
        // keymap dispatch and the child-encode path, so leader keys never leak
        // to the PTY. A no-op (returns false) when no sequences are configured.
        // --------------------------------------------------------------------
        if event.state.is_pressed()
            && let Some(chord) = chord_from_event(&event.logical_key, self.mods)
            && self.handle_key_sequence(&chord, event_loop)
        {
            return;
        }

        // --------------------------------------------------------------------
        // Keymap dispatch: consult the user keymap (which includes defaults)
        // FIRST, before any hard-coded path below. This lets custom bindings
        // override every built-in chord.
        // --------------------------------------------------------------------
        if event.state.is_pressed()
            && let Some(chord) = chord_from_event(&event.logical_key, self.mods)
            && let Some(&action) = self.config.keymap.get(&chord)
        {
            // Scroll actions are suppressed on the alt-screen (let the
            // app handle Shift+Page keys itself).
            let is_scroll = matches!(
                action,
                KeyAction::ScrollUp
                    | KeyAction::ScrollDown
                    | KeyAction::ScrollTop
                    | KeyAction::ScrollBottom
            );
            // The default `quake_toggle` bind (F12) must NOT swallow F12 from
            // terminal apps when this instance isn't in quake mode — there it is a
            // no-op, so let the keypress fall through to the child instead.
            let inert_quake = action == KeyAction::QuakeToggle && self.quake.is_none();
            // Pane-focus chords (Cmd/Ctrl+arrow) are no-ops on a single-pane tab —
            // there they MUST fall through so the arrow reaches the child (e.g.
            // Ctrl+Left = word-jump in a shell). Only swallow them when split.
            let is_focus_pane = matches!(
                action,
                KeyAction::FocusPaneLeft
                    | KeyAction::FocusPaneRight
                    | KeyAction::FocusPaneUp
                    | KeyAction::FocusPaneDown
            );
            let inert_focus_pane = is_focus_pane && !self.is_split();
            if !inert_quake
                && !inert_focus_pane
                && (!is_scroll || !self.term_mode().contains(TermMode::ALT_SCREEN))
            {
                self.run_key_action(action, event_loop);
                return;
            }
        }

        // Alt+Arrow moves focus between tiled panes (no-op when not split,
        // so a single-pane tab passes Alt+Arrow through to the child).
        // This is NOT in the keymap because it is not a simple chord: it only
        // fires when a split exists, and falls through to the child otherwise.
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

        // While the tab right-click menu is open, Up/Down/Enter/Esc navigate it.
        if event.state.is_pressed() && self.tab_menu_target.is_some() {
            let key = &event.logical_key;
            if self.handle_tab_menu_key(key, event_loop) {
                return;
            }
            // Any other key closes it and falls through to the child.
            self.close_tab_menu(event_loop);
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
        // Note: Help / Settings open actions are handled by the keymap dispatch
        // above; the close-overlay path below is overlay-navigation-specific.
        if event.state.is_pressed() && (self.help_open || self.settings_open) {
            let key = &event.logical_key;
            let toggle_settings =
                self.mods.control_key() && matches!(key, Key::Character(s) if s.as_str() == ",");
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
            if matches!(key, Key::Named(NamedKey::Escape | NamedKey::F1)) || toggle_settings {
                self.help_open = false;
                self.settings_open = false;
                self.settings_drop = gui::SettingsDrop::None;
                // Clear the opening-gesture guard on a keyboard close so a stale
                // `true` (e.g. opened by cog, then closed by Esc before any click
                // edge consumed it) cannot leak into the next overlay and swallow
                // that overlay's first outside-click dismiss.
                self.overlay_opened_by_press = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                return;
            }
            if self.settings_open {
                // An editable text field (Word seps / Font features) gets first
                // crack at the key; only nav/commit keys fall through to the form.
                if self.handle_settings_field_key(key, event_loop) {
                    return;
                }
                self.handle_settings_key(key.clone(), event_loop);
            }
            return; // consume all other keys while an overlay is up
        }

        // Build kitty keyboard protocol flags from the terminal's current mode.
        // Level 1 (DISAMBIGUATE_ESC_CODES) makes modified named keys go as
        // CSI-u. Higher levels add repeat/release events, alternate keys, and
        // the all-keys-as-esc form required by Helix, Neovim, etc.
        let mode = self.term_mode();
        let kitty = KittyFlags {
            disambiguate: mode.contains(TermMode::DISAMBIGUATE_ESC_CODES),
            report_event_types: mode.contains(TermMode::REPORT_EVENT_TYPES),
            report_alternate_keys: mode.contains(TermMode::REPORT_ALTERNATE_KEYS),
            report_all_keys_as_esc: mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC),
            report_associated_text: mode.contains(TermMode::REPORT_ASSOCIATED_TEXT),
        };
        // DECCKM: arrows/Home/End go out as SS3 (ESC O X) for full-screen
        // apps (vim, less, ncurses) that enable application cursor-key mode.
        // Under any active kitty bit (DISAMBIGUATE_ESC_CODES etc.) the spec
        // forbids the ambiguous SS3 form for cursor keys, so suppress app-cursor
        // mode when kitty is active — those keys then fall to unambiguous CSI.
        let app_cursor = mode.contains(TermMode::APP_CURSOR) && !kitty.active();
        if let Some(bytes) = encode_key_parts(
            &event.logical_key,
            event.text.as_deref(),
            event.state.is_pressed(),
            event.repeat,
            self.mods,
            kitty,
            app_cursor,
            self.modify_other_keys,
        ) {
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
                // `write_input` fans the bytes out to every pane when broadcast
                // input is active (else just the focused pane).
                self.write_input(bytes);
            }
            // The snap-to-bottom (and the cursor/selection reset above) are
            // visual changes even when the child emits nothing back — e.g.
            // typing while scrolled up into a paused/blocked program. Repaint
            // unconditionally so the view never stays frozen in scrollback.
            self.mark_dirty(event_loop);
        }
    }

    /// Execute a [`KeyAction`] looked up from the keymap. Every arm routes
    /// through the same method the palette / menu path uses, keeping behaviour
    /// identical no matter how an action is triggered.
    pub(super) fn run_key_action(&mut self, action: KeyAction, event_loop: &ActiveEventLoop) {
        use KeyAction::*;
        match action {
            NewTab => self.new_tab(event_loop),
            ClosePane => self.try_close_pane(event_loop),
            NextTab => self.cycle_tab(1, event_loop),
            PrevTab => self.cycle_tab(-1, event_loop),
            SplitVertical => self.split_pane(pane::Dir::Vertical, event_loop),
            SplitHorizontal => self.split_pane(pane::Dir::Horizontal, event_loop),
            ToggleFullscreen => {
                if let Some(w) = self.window.as_ref() {
                    let fs = if w.fullscreen().is_some() {
                        None
                    } else {
                        Some(winit::window::Fullscreen::Borderless(None))
                    };
                    w.set_fullscreen(fs);
                }
            }
            ToggleMaximize => {
                if let Some(w) = self.window.as_ref() {
                    let maximized = w.is_maximized();
                    w.set_maximized(!maximized);
                }
            }
            Settings => {
                self.open_settings();
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            Help => {
                self.help_open = true;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            Search => self.open_search(event_loop),
            CommandPalette => self.open_palette(event_loop),
            Copy => {
                self.copy_selection();
                self.mark_dirty(event_loop);
            }
            Paste => {
                self.paste_clipboard();
                self.mark_dirty(event_loop);
            }
            ToggleStatusBar => {
                self.toggle_status_bar();
                self.mark_dirty(event_loop);
            }
            ToggleMinimap => {
                self.toggle_minimap();
                self.mark_dirty(event_loop);
            }
            FontIncrease => {
                self.resize_font(FontStep::Inc);
                self.mark_dirty(event_loop);
            }
            FontDecrease => {
                self.resize_font(FontStep::Dec);
                self.mark_dirty(event_loop);
            }
            FontReset => {
                self.resize_font(FontStep::Reset);
                self.mark_dirty(event_loop);
            }
            ScrollUp => {
                if let Some(pty) = &self.pty {
                    pty.term.lock().scroll_display(Scroll::PageUp);
                }
                self.mark_dirty(event_loop);
            }
            ScrollDown => {
                if let Some(pty) = &self.pty {
                    pty.term.lock().scroll_display(Scroll::PageDown);
                }
                self.mark_dirty(event_loop);
            }
            ScrollTop => {
                if let Some(pty) = &self.pty {
                    pty.term.lock().scroll_display(Scroll::Top);
                }
                self.mark_dirty(event_loop);
            }
            ScrollBottom => {
                if let Some(pty) = &self.pty {
                    pty.term.lock().scroll_display(Scroll::Bottom);
                }
                self.mark_dirty(event_loop);
            }
            JumpPrevPrompt => self.jump_prompt(-1, event_loop),
            JumpNextPrompt => self.jump_prompt(1, event_loop),
            GoToTab(n) => {
                // 1-based position from the chord → 0-based index into tab_order.
                self.activate_tab((n as usize).saturating_sub(1), event_loop);
            }
            MoveTabLeft => self.move_active_tab(-1, event_loop),
            MoveTabRight => self.move_active_tab(1, event_loop),
            BroadcastInput => self.toggle_broadcast_input(event_loop),
            Hints => self.open_hints(event_loop),
            ToggleFold => self.toggle_command_fold(event_loop),
            QuakeToggle => {
                // In quake mode this slides the window away (the in-app "hide"
                // key); a fresh `glassy toggle`/keybind brings it back. In normal
                // mode `quake` is None so this is a no-op.
                self.quake_apply(crate::ipc::IpcCommand::Toggle, event_loop);
            }
            ToggleZoom => self.toggle_zoom(event_loop),
            FocusPaneLeft => self.focus_pane(pane::Move::Left, event_loop),
            FocusPaneRight => self.focus_pane(pane::Move::Right, event_loop),
            FocusPaneUp => self.focus_pane(pane::Move::Up, event_loop),
            FocusPaneDown => self.focus_pane(pane::Move::Down, event_loop),
            RotatePanes => self.rotate_panes(event_loop),
            EqualizePanes => self.equalize_panes(event_loop),
        }
    }

    /// Scroll the viewport to the previous (`dir < 0`) or next (`dir > 0`) OSC 133
    /// prompt-start mark recorded by the focused pane's [`PromptTracker`].
    ///
    /// Prompt rows are stored in the same anchored coordinate space the renderer
    /// uses for image placements: `stored_row = screen_line + display_offset` at
    /// record time. The row currently at the TOP of the viewport therefore sits at
    /// stored-row `display_offset`, so we query the tracker with the live
    /// `display_offset` and scroll so the found prompt lands at the top (clamping
    /// the resulting offset into the valid `[0, history_size]` range). A no-op when
    /// there is no prompt in the requested direction.
    pub(super) fn jump_prompt(&mut self, dir: i32, event_loop: &ActiveEventLoop) {
        let Some(pty) = &self.pty else {
            return;
        };
        let mut term = pty.term.lock();
        let display_offset = term.grid().display_offset() as i32;
        let history = term.grid().history_size() as i32;
        let target = {
            let prompts = match pty.prompts.lock() {
                Ok(p) => p,
                Err(_) => return,
            };
            if dir < 0 {
                prompts.prev_prompt(display_offset)
            } else {
                prompts.next_prompt(display_offset)
            }
        };
        let Some(target_row) = target else {
            return;
        };
        // Anchor the found prompt at the top of the viewport.
        let target_offset = target_row.clamp(0, history);
        let delta = target_offset - display_offset;
        if delta != 0 {
            term.scroll_display(Scroll::Delta(delta));
        }
        drop(term);
        self.mark_dirty(event_loop);
    }
}
