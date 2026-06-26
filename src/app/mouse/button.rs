//! Mouse button press/release handling (`handle_mouse_input`).
//!
//! Owns overlay routing (settings / help / palette / search / menus), the
//! gutter-drag and pane-header / tab-strip click paths, Ctrl+click link
//! opening, the right-click context menu, and — when no application owns the
//! mouse — the glassy text-selection / paste path. Holding Alt at press time
//! starts a rectangular ([`SelectionType::Block`]) selection whose copy
//! yields per-row blocks.

use super::*;

impl App {
    /// Handle a `WindowEvent::MouseInput` event.
    pub(in crate::app) fn handle_mouse_input(
        &mut self,
        state: ElementState,
        button: MouseButton,
        event_loop: &ActiveEventLoop,
    ) {
        let base = match button {
            MouseButton::Left => 0u8,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => return,
        };
        let pressed = state == ElementState::Pressed;
        // Track the held button for drag reports regardless of mode.
        self.held_button = if pressed { Some(base) } else { None };
        // A left press dismisses any inline peek card (but does not consume the
        // click — it still focuses the pane / positions the cursor as usual).
        if pressed && button == MouseButton::Left {
            self.dismiss_peek(event_loop);
        }
        // Real-GUI chrome: capture the left press→release as a click edge for
        // the next chrome paint, and release the press latch on button-up.
        if button == MouseButton::Left {
            if pressed {
                self.gui_click_edge = false;
                self.note_gui_left_press();
            } else {
                // Set the press→release edge; the press latch (`gui_pressed`)
                // is cleared only AFTER the next paint consumes this edge so
                // the release frame can still resolve a click on the latched
                // widget (see the click-edge reset in `render`).
                self.gui_click_edge = true;
                // Capture the pointer position at the moment of release so that
                // overlay hit tests always use the click's actual position, even
                // if the pointer moves between this event and the next render.
                self.gui_click_pos = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
                // NOTE: `overlay_opened_by_press` is intentionally NOT cleared here.
                // It must be consumed only where the click edge is consulted (the
                // settings-dismiss guard below, or the help paint via the render
                // reset), so an overlay opened on this same gesture's press ignores
                // this release for click-outside dismissal. Clearing it up-front
                // (as the old code did) made the guard dead — the cog-opened
                // Settings then dismissed on its own opening release/motion.
            }
            self.mark_dirty(event_loop);
        }

        // While the inline tab-rename editor is open, a left press commits
        // the current name. If the press is on the chip being renamed it is
        // also consumed (keep editing — a re-click shouldn't switch tabs);
        // otherwise the click falls through so it can switch/act normally.
        if self.tab_rename.is_some() && button == MouseButton::Left && pressed {
            let renaming_pos = self.tab_rename.as_ref().map(|(p, _)| *p);
            let (mx, my) = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
            let on_same_chip = matches!(
                (self.strip_item_at_px(mx, my), renaming_pos),
                (Some(StripItem::Tab(p)), Some(rp)) if p == rp
            );
            self.commit_tab_rename(event_loop);
            if on_same_chip {
                self.held_button = None;
                return;
            }
        }

        // While the settings form is open it owns the mouse: the form's
        // immediate-mode widgets resolve hits during paint (from the click
        // edge captured above), so consume the event here and never fall
        // through to the terminal / tab / menu handlers. A left click well
        // outside the panel dismisses the form.
        if self.settings_open {
            // Dismiss on the RELEASE edge, consistent with the in-panel widgets
            // (which resolve on gui_click_edge). Acting on press would let a click
            // starting just outside the panel kill the form before the Ui resolves
            // an inside-click, and would break starting a slider drag from outside.
            if button == MouseButton::Left && !pressed {
                // If this release belongs to the press that OPENED the overlay,
                // consume it without dismissing — the strip button that opened
                // settings is outside the panel, so without this guard the overlay
                // would close on the very same gesture that opened it.
                if self.overlay_opened_by_press {
                    self.overlay_opened_by_press = false;
                } else {
                    // Use gui_click_pos (position at release time) so pointer
                    // motion between release and this handler does not affect
                    // the hit test.
                    let (mx, my) = self.gui_click_pos;
                    if !gui::hit(self.settings_panel, mx, my) {
                        self.settings_open = false;
                        self.settings_drop = gui::SettingsDrop::None;
                        self.force_full_redraw = true;
                        self.mark_dirty(event_loop);
                    }
                }
            }
            // Do NOT clear held_button here: the settings snapshot reads
            // `held_button == Some(0)` as `mouse_down` for immediate-mode
            // press-latch / slider-drag. Clearing it before the render frame
            // fires makes every widget see mouse_down=false, killing all
            // click and drag interactions. `held_button` is correctly set to
            // None on the release event at the top of this handler (line
            // `self.held_button = if pressed { Some(base) } else { None }`).
            return;
        }

        // The help panel (§3.7) is a full-screen scrim + floating panel that
        // owns the pointer exactly like the settings form: its ✕ close button
        // and scrollbar are immediate-mode widgets resolved during paint from
        // the click edge captured above, and a click on the scrim (outside the
        // panel) dismisses it (handled inside `build_help`). Consume the event
        // so it never falls through to start a text selection / tab-click /
        // gutter-drag beneath the panel. Do NOT clear `held_button` — the
        // scrollbar drag reads it as `mouse_down` during paint (same rule as
        // the settings block above).
        //
        // We deliberately do NOT clear `overlay_opened_by_press` here. When the
        // help panel was opened by the PRESS of this same gesture (the cog
        // icon), this release set a stale outside click edge; `build_help`
        // skips its scrim-close while `overlay_opened_by_press` is set, and the
        // render reset clears the flag once it consumes that edge — so a later
        // motion-driven repaint can no longer flush a stale dismiss.
        if self.help_open {
            return;
        }

        // The command palette owns the pointer: a left press on a listed
        // row activates it; a press anywhere else (the scrim) closes it.
        if self.palette.is_some() {
            // Act on the RELEASE edge (matching every other immediate-mode overlay,
            // which resolves via gui_click_edge): a press that arrives in the same
            // frame the palette opened must not immediately dismiss it.
            if button == MouseButton::Left && !pressed {
                let (mx, my) = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
                let hit = self
                    .palette_rows
                    .iter()
                    .find(|(_, r)| gui::hit(*r, mx, my))
                    .map(|(idx, _)| *idx);
                match hit {
                    Some(idx) => self.palette_activate_index(idx, event_loop),
                    None => self.close_palette(event_loop),
                }
            }
            self.held_button = None;
            return;
        }

        // The find bar owns the pointer too: a click outside the bottom bar
        // is consumed (no terminal selection beneath the overlay). Clicking
        // the bar itself is a no-op (text editing is keyboard-driven).
        if self.search.is_some() {
            self.held_button = None;
            return;
        }

        if !pressed {
            self.dragging_tab = None; // end any tab drag-reorder on release
            self.minimap_dragging = false; // end any minimap scrub on release
            // End any gutter drag; re-evaluate the cursor for the spot we
            // released over (still a gutter -> resize arrow, else content).
            if self.dragging_gutter.take().is_some() {
                let g = self.gutter_at(self.mouse_px.0, self.mouse_px.1);
                if g.is_some() {
                    self.apply_gutter_cursor(g.as_ref());
                } else {
                    self.apply_content_cursor();
                }
                self.hovered_gutter = g;
                self.mark_dirty(event_loop);
            }
            // Release any pressed toolbar item so its inset clears.
            if self.held_strip_item.take().is_some() {
                self.mark_dirty(event_loop);
            }
        }

        // A left press over the scrollback minimap strip jumps the viewport to
        // that position and begins a scrub-drag (subsequent CursorMoved events
        // keep jumping until release). Consumed so it never starts a text
        // selection beneath the strip. Checked before the gutter/terminal paths.
        if button == MouseButton::Left
            && pressed
            && self.minimap_active()
            && self.minimap_hit(self.mouse_px.0, self.mouse_px.1)
        {
            self.minimap_dragging = true;
            self.minimap_jump_to(self.mouse_px.1, event_loop);
            self.held_button = None;
            return;
        }

        // A left press over a pane resize gutter begins a drag and MUST NOT
        // start a text selection or focus-swap. Highest priority among
        // press handlers (the gutter sits in the inter-pane gap, not in any
        // pane's cell area, so this never steals a content click).
        if button == MouseButton::Left
            && pressed
            && let Some(handle) = self.gutter_at(self.mouse_px.0, self.mouse_px.1)
        {
            self.apply_gutter_cursor(Some(&handle));
            self.hovered_gutter = Some(handle.clone());
            self.dragging_gutter = Some(handle);
            self.held_button = None;
            self.mark_dirty(event_loop);
            return;
        }

        // A click anywhere while the dropdown is open: invoke the selected item on
        // the left RELEASE edge (consistent with the immediate-mode chrome, which
        // resolves on button-up), dismiss on a press outside the panel, and always
        // close on a right-click. The whole event is consumed either way so it
        // never falls through to start a text selection beneath the menu.
        if self.menu_open && (button == MouseButton::Left || button == MouseButton::Right) {
            let (mx, my) = self.mouse_px;
            if button == MouseButton::Right {
                // Right-click while menu is open: close without invoking.
                if pressed {
                    self.close_menu(event_loop);
                }
            } else if pressed {
                // Left press: dismiss only when it lands outside the menu; a press
                // inside keeps the menu up so the release can activate the item.
                if self.menu_hit_test(mx, my).is_none() {
                    self.close_menu(event_loop);
                }
            } else if let Some(action) = self.menu_hit_test(mx, my) {
                // Left release inside the menu: invoke the item under the pointer.
                self.invoke_menu_action(action, event_loop);
            }
            self.held_button = None;
            return;
        }

        // A click while the tab right-click menu is open: invoke the row on the
        // left RELEASE edge, dismiss on a press outside the panel, and always close
        // on a right-click. Consumed either way (never falls through to the strip /
        // terminal beneath the menu).
        if self.tab_menu_target.is_some()
            && (button == MouseButton::Left || button == MouseButton::Right)
        {
            let (mx, my) = self.mouse_px;
            if button == MouseButton::Right {
                if pressed {
                    self.close_tab_menu(event_loop);
                }
            } else if pressed {
                if self.tab_menu_hit_test(mx, my).is_none() {
                    self.close_tab_menu(event_loop);
                }
            } else if let Some(action) = self.tab_menu_hit_test(mx, my) {
                self.invoke_tab_menu_action(action, event_loop);
            }
            self.held_button = None;
            return;
        }

        // A right-click on a tab chip opens the tab context menu (Rename /
        // Duplicate / Close / Close others / Move). Takes priority over the
        // terminal context menu so the strip owns its own right-click.
        if button == MouseButton::Right && pressed {
            let (mx, my) = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
            if let Some(StripItem::Tab(pos)) | Some(StripItem::TabClose(pos)) =
                self.strip_item_at_px(mx, my)
            {
                self.open_tab_menu(pos, event_loop);
                self.held_button = None;
                return;
            }
        }

        // A left press while the pane ⋮ menu is open: invoke or dismiss.
        if button == MouseButton::Left && pressed && self.pane_menu_open.is_some() {
            let (mx, my) = self.mouse_px;
            if let Some(idx) = self.pane_menu_hit_test(mx, my) {
                self.invoke_pane_menu_action(idx, event_loop);
            } else {
                // Click outside the menu dismisses it; may still be a header
                // or content click so don't return early.
                self.pane_menu_open = None;
                self.mark_dirty(event_loop);
            }
            self.held_button = None;
            return;
        }

        // A left press on a pane title-bar header: focus the pane or toggle
        // the ⋮ menu. This must come before the gutter check (headers overlap
        // the top of each pane tile, not the inter-pane gap).
        if button == MouseButton::Left && pressed {
            let (mx, my) = self.mouse_px;
            if self.pane_header_click(mx, my, event_loop) {
                self.held_button = None;
                return;
            }
        }

        // A left click in the tab strip switches tabs; never sent onward.
        if button == MouseButton::Left && pressed && self.strip_click(event_loop) {
            self.held_button = None;
            return;
        }

        // In a split, a press over a non-focused pane focuses it first, so
        // selection / mouse-reporting below target the pane the user
        // clicked. Re-derive the (now pane-local) cell after the swap.
        if pressed && self.is_split() {
            let (mx, my) = self.mouse_px;
            if self.focus_pane_at(mx, my, event_loop) {
                self.mouse_cell = self.px_to_cell(mx, my);
            }
        }

        let mode = self.term_mode();
        // Ctrl+Click opens the link under the pointer.  OSC 8 links
        // take priority; plain-text URLs/paths are the fallback.
        if button == MouseButton::Left && pressed && self.mods.control_key() {
            let (c, r) = self.mouse_cell;
            let uri = self
                .cell_hyperlink(c, r)
                .or_else(|| self.plain_link_at(c, r));
            if let Some(uri) = uri {
                Self::open_url(&uri);
                return;
            }
        }
        // Right-click: open the context menu, gated on mouse-reporting mode.
        //   - not in MOUSE_MODE: plain right-press opens the menu.
        //   - in MOUSE_MODE: Shift+right-press opens it (terminal bypass);
        //     a bare right-press is forwarded to the application.
        if button == MouseButton::Right && pressed {
            let in_mouse_mode = mode.intersects(TermMode::MOUSE_MODE);
            if !in_mouse_mode || self.mods.shift_key() {
                self.open_context_menu(event_loop);
                self.held_button = None;
                return;
            }
            // else: fall through to report_mouse below
        }

        if mode.intersects(TermMode::MOUSE_MODE) {
            // The application owns the mouse; never start a glassy
            // selection or paste underneath it.
            self.report_mouse(base, pressed, false, mode);
            return;
        }

        match (button, pressed) {
            // Left press: start (or extend the granularity of) a glassy
            // text selection. Double/triple clicks within the same cell
            // and a short window escalate to Semantic (word) then Lines.
            // Holding Alt/Option at press time starts a rectangular (Block)
            // selection instead; copying it yields one block per row.
            (MouseButton::Left, true) => {
                const MULTI_CLICK: Duration = Duration::from_millis(300);
                let now = Instant::now();
                let count = match self.last_click {
                    Some((cell, n, t))
                        if cell == self.mouse_cell && now.duration_since(t) < MULTI_CLICK =>
                    {
                        (n % 3) + 1
                    }
                    _ => 1,
                };
                self.last_click = Some((self.mouse_cell, count, now));
                let ty = if self.mods.alt_key() {
                    // Alt-drag: rectangular block. Takes priority over the
                    // multi-click word/line escalation so an Alt-drag is
                    // always a clean column block regardless of click count.
                    SelectionType::Block
                } else {
                    match count {
                        2 => SelectionType::Semantic,
                        3 => SelectionType::Lines,
                        _ => SelectionType::Simple,
                    }
                };
                self.start_selection(ty);
                self.mark_dirty(event_loop);
            }
            // Left release: finish the drag; the selection persists for copy.
            // When `copy_on_select` is on, a completed selection is mirrored
            // to the clipboard immediately (the de-facto X11/terminal
            // convention). On Linux/X11 we also set the PRIMARY selection so
            // middle-click paste works.  A no-op when nothing was selected.
            (MouseButton::Left, false) => {
                let was_selecting = self.selecting;
                self.selecting = false;
                if was_selecting && self.config.copy_on_select {
                    self.copy_selection();
                    // On Linux/X11, mirror to PRIMARY so middle-click paste works.
                    #[cfg(target_os = "linux")]
                    self.copy_selection_to_primary();
                }
            }
            // Middle click: paste from PRIMARY selection on Linux (the standard
            // X11 "paste what you just highlighted" convention).  Falls back to
            // the standard clipboard when PRIMARY is unavailable.
            (MouseButton::Middle, true) => {
                #[cfg(target_os = "linux")]
                self.paste_primary_or_clipboard();
                #[cfg(not(target_os = "linux"))]
                self.paste_clipboard();
                self.mark_dirty(event_loop);
            }
            _ => {}
        }
    }
}
