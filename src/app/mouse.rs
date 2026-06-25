//! Mouse input handlers: cursor motion, button press/release, and scroll wheel.
//! Extracted from `event_loop.rs` to keep each file under ~700 lines.

use super::*;

impl App {
    // -------------------------------------------------------------------------
    // Cursor motion
    // -------------------------------------------------------------------------

    /// Handle a `WindowEvent::CursorMoved` event.
    pub(super) fn handle_cursor_moved(
        &mut self,
        position: winit::dpi::PhysicalPosition<f64>,
        event_loop: &ActiveEventLoop,
    ) {
        self.mouse_px = (position.x, position.y);
        // Any open GUI overlay (settings, dropdown/context menu, help panel,
        // pane ⋮ menu) owns the pointer: its immediate-mode widgets compute
        // hover / press / slider-drag from `mouse_px` during paint, so every
        // motion must trigger a repaint for those highlights to track the
        // pointer. It also means motion must NOT fall through to drive
        // tab-drag, gutter-drag, terminal hover, or text selection beneath
        // the overlay. Mirror the settings treatment for all of them.
        // The dropdown / context menu (`gui::menu`) highlights the row under
        // the pointer. Mirror the hovered row into `menu_sel` so mouse hover
        // and keyboard nav share one selection, and repaint only when that
        // row actually changes — not every pixel of motion across the panel.
        if self.menu_open && !self.settings_open && !self.help_open {
            if let Some(action) = self.menu_hit_test(position.x, position.y) {
                let items: &[MenuAction] = self.menu_items.as_deref().unwrap_or(MenuAction::ALL);
                if let Some(idx) = items.iter().position(|&a| a == action)
                    && idx != self.menu_sel
                {
                    self.menu_sel = idx;
                    self.mark_dirty(event_loop);
                }
            }
            return;
        }
        // Tab right-click menu: mirror the hovered row into `tab_menu_sel` so
        // mouse hover and keyboard nav share one selection (repaint on change).
        if self.tab_menu_target.is_some() {
            if let Some(action) = self.tab_menu_hit_test(position.x, position.y)
                && let Some(idx) = self.tab_menu_actions().iter().position(|&a| a == action)
                && idx != self.tab_menu_sel
            {
                self.tab_menu_sel = idx;
                self.mark_dirty(event_loop);
            }
            return;
        }
        if self.settings_open
            || self.help_open
            || self.pane_menu_open.is_some()
            || self.palette.is_some()
            || self.search.is_some()
        {
            self.mark_dirty(event_loop);
            return;
        }
        let cell = self.px_to_cell(position.x, position.y);
        let moved = cell != self.mouse_cell;
        self.mouse_cell = cell;

        // Drag-to-reorder a tab: while a tab chip is held, move it under
        // the pointer's pixel position and lift it as a drag-ghost. Takes
        // priority over selection/hover; repaint on any motion so the ghost
        // tracks the pointer.
        if self.dragging_tab.is_some() {
            let _ = self.drag_tab_to(position.x as f32, position.y as f32);
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
            return;
        }

        // Dragging a pane resize gutter: re-tile under the pointer. Takes
        // priority over hover/selection; repaint so the divider + content
        // follow. The OS resize cursor stays set for the drag's duration.
        if self.dragging_gutter.is_some() {
            if self.drag_gutter_to(position.x, position.y) {
                self.mark_dirty(event_loop);
            }
            return;
        }

        // Gutter hover: over a split's divider band, switch the OS cursor to
        // a resize arrow and draw the divider transiently fat/bright. Only
        // costs a hit-test on motion; off any gutter restores the default.
        {
            let new_gutter = self.gutter_at(position.x, position.y);
            if new_gutter != self.hovered_gutter {
                self.apply_gutter_cursor(new_gutter.as_ref());
                self.hovered_gutter = new_gutter;
                self.mark_dirty(event_loop);
            }
            // Over a gutter, suppress tab-bar/selection hover handling below.
            if self.hovered_gutter.is_some() {
                return;
            }
        }

        // Pane header hover: repaint only on an enter/leave or ⋮-button
        // edge, not on every pixel of motion — otherwise dragging the
        // pointer across a header queues a frame per event for no visual
        // change. Track the hovered header and diff it.
        if self.is_split() {
            let new_hover = self.pane_header_at(position.x, position.y);
            if new_hover != self.hovered_pane_header {
                self.hovered_pane_header = new_hover;
                self.mark_dirty(event_loop);
            }
        } else if self.hovered_pane_header.is_some() {
            self.hovered_pane_header = None;
        }

        // Tab-bar hover highlighting: track the item under the pointer (only
        // while over the bar's pixel band), repaint when it changes.
        {
            let bar_h = self
                .renderer
                .as_ref()
                .map(|r| tab_bar_h(r.cell_metrics().height) as f64)
                .unwrap_or(0.0);
            let new_hover = if position.y < bar_h {
                self.strip_item_at_px(position.x as f32, position.y as f32)
            } else {
                None
            };
            if new_hover != self.hovered_strip_item {
                self.hovered_strip_item = new_hover;
                self.mark_dirty(event_loop);
            }
        }

        // Extend an in-progress glassy text selection while dragging.
        if self.selecting {
            self.update_selection();
            self.mark_dirty(event_loop);
        } else if moved {
            // Motion reports drive hover highlighting (e.g. the Claude
            // Code TUI highlights the element under the pointer, which
            // needs any-motion mode 1003 with no button held).
            let mode = self.term_mode();
            if let Some(button) = motion_button(mode, self.held_button) {
                self.report_mouse(button, true, true, mode);
            } else if !mode.intersects(TermMode::MOUSE_MODE) {
                // Track the hovered link so it can be underlined and
                // Ctrl+clicked.  OSC 8 links take priority; for cells
                // with no OSC 8 annotation we fall back to the
                // plain-text URL/path scanner.
                let (c, r) = self.mouse_cell;
                let link = self
                    .cell_hyperlink(c, r)
                    .or_else(|| self.plain_link_at(c, r));
                if link != self.hovered_link {
                    self.hovered_link = link;
                    self.mark_dirty(event_loop);
                }
            }
        }
    }

    // -------------------------------------------------------------------------
    // Mouse buttons
    // -------------------------------------------------------------------------

    /// Handle a `WindowEvent::MouseInput` event.
    pub(super) fn handle_mouse_input(
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
        // Real-GUI chrome: capture the left press→release as a click edge for
        // the next chrome paint, and release the press latch on button-up.
        if button == MouseButton::Left {
            if pressed {
                self.gui_click_edge = false;
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
                // If an overlay was opened by the PRESS of this same button, the
                // release belongs to that opening gesture — do not treat it as a
                // click-outside-the-panel dismiss.
                self.overlay_opened_by_press = false;
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
            // End any gutter drag; re-evaluate the cursor for the spot we
            // released over (still a gutter -> resize arrow, else default).
            if self.dragging_gutter.take().is_some() {
                let g = self.gutter_at(self.mouse_px.0, self.mouse_px.1);
                self.apply_gutter_cursor(g.as_ref());
                self.hovered_gutter = g;
                self.mark_dirty(event_loop);
            }
            // Release any pressed toolbar item so its inset clears.
            if self.held_strip_item.take().is_some() {
                self.mark_dirty(event_loop);
            }
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
                let ty = match count {
                    2 => SelectionType::Semantic,
                    3 => SelectionType::Lines,
                    _ => SelectionType::Simple,
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

    // -------------------------------------------------------------------------
    // Mouse wheel / touchpad scroll
    // -------------------------------------------------------------------------

    /// Handle a `WindowEvent::MouseWheel` event.
    pub(super) fn handle_mouse_wheel(
        &mut self,
        delta: MouseScrollDelta,
        phase: winit::event::TouchPhase,
        event_loop: &ActiveEventLoop,
    ) {
        use winit::event::TouchPhase;
        // Help panel owns the wheel while open: scroll its keybinding list.
        // The next `build_help` clamps `scroll` against the content height,
        // so we just accumulate here (negative = scroll up toward the top).
        if self.help_open {
            let line_px = self
                .renderer
                .as_ref()
                .map(|r| r.cell_metrics().height)
                .unwrap_or(20.0)
                .max(1.0);
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y * line_px * 3.0,
                MouseScrollDelta::PixelDelta(p) => p.y as f32,
            };
            // Wheel-up (positive y) moves content up = decrease scroll.
            self.help_state.scroll = (self.help_state.scroll - dy).max(0.0);
            self.mark_dirty(event_loop);
            return;
        }
        // A touchpad gesture brackets its deltas with Started/Ended; reset
        // the accumulators and the one-switch-per-swipe latch at those
        // boundaries so each gesture is independent.
        if matches!(
            phase,
            TouchPhase::Started | TouchPhase::Ended | TouchPhase::Cancelled
        ) {
            self.tab_scroll_accum = 0.0;
            self.content_scroll_accum = 0.0;
            self.swipe_consumed = false;
        }

        // Over the tab strip: a swipe/scroll switches tabs as a discrete
        // GESTURE — one tab per swipe, clamped at the ends (no wrap-around
        // carousel). Horizontal motion is preferred (natural swipe-to-switch).
        let in_strip = {
            let bar_h = self
                .renderer
                .as_ref()
                .map(|r| tab_bar_h(r.cell_metrics().height) as f64)
                .unwrap_or(0.0);
            self.mouse_px.1 < bar_h
        };
        if in_strip {
            const STEP: f32 = 24.0; // px of swipe travel to trigger one switch
            match delta {
                // A discrete wheel notch always steps one tab (clamped).
                MouseScrollDelta::LineDelta(x, y) => {
                    let primary = if x.abs() > y.abs() { x } else { y };
                    if primary > 0.0 {
                        self.step_tab(1, event_loop);
                    } else if primary < 0.0 {
                        self.step_tab(-1, event_loop);
                    }
                }
                // Touchpad: accumulate, fire ONCE per swipe at the threshold,
                // then latch until the gesture ends — no twitchy carousel.
                MouseScrollDelta::PixelDelta(p) => {
                    let primary = (if p.x.abs() > p.y.abs() { p.x } else { p.y }) as f32;
                    self.tab_scroll_accum += primary;
                    if !self.swipe_consumed && self.tab_scroll_accum.abs() >= STEP {
                        let dir = if self.tab_scroll_accum > 0.0 { 1 } else { -1 };
                        self.step_tab(dir, event_loop);
                        self.swipe_consumed = true;
                    }
                }
            }
            return;
        }
        self.tab_scroll_accum = 0.0;

        // In a split, the wheel targets the pane under the pointer: focus it
        // so the scroll / mouse-report below acts on that pane's PTY.
        if self.is_split() {
            let (mx, my) = self.mouse_px;
            self.focus_pane_at(mx, my, event_loop);
        }

        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                self.content_scroll_accum = 0.0;
                if y == 0.0 {
                    0
                } else {
                    (y.abs().ceil() as i32) * y.signum() as i32
                }
            }
            // Touchpads emit many sub-line pixel deltas; accumulate and step
            // by the cell height so slow scrolls register instead of being
            // truncated to zero each event (the "tiny scrolls do nothing" bug).
            MouseScrollDelta::PixelDelta(p) => {
                self.content_scroll_accum += p.y as f32;
                let step = self
                    .renderer
                    .as_ref()
                    .map(|r| r.cell_metrics().height)
                    .unwrap_or(20.0)
                    .max(1.0);
                let n = (self.content_scroll_accum / step) as i32;
                self.content_scroll_accum -= n as f32 * step;
                n
            }
        };
        if lines == 0 {
            return;
        }
        let mode = self.term_mode();
        let up = lines > 0;
        let count = lines.unsigned_abs() as usize;

        match wheel_action(mode) {
            WheelAction::Report => {
                // Wheel as button 64 (up) / 65 (down), one report per line.
                let button = if up { 64 } else { 65 };
                for _ in 0..count {
                    self.report_mouse(button, true, false, mode);
                }
            }
            WheelAction::Arrows => {
                // Alt-screen apps (pagers, bat, vim without `mouse=`) expect
                // the wheel to emit arrow keys — xterm's alternateScroll is
                // on by default and the alt screen has no scrollback of its
                // own. ~3 lines per notch.
                if let Some(pty) = &self.pty {
                    // DECCKM: when the app enabled application cursor-key
                    // mode, arrow keys go out as SS3 (ESC O X), so the wheel
                    // emulation must match or the pager sees the wrong code.
                    let seq: &[u8] = if mode.contains(TermMode::APP_CURSOR) {
                        if up { b"\x1bOA" } else { b"\x1bOB" }
                    } else if up {
                        b"\x1b[A"
                    } else {
                        b"\x1b[B"
                    };
                    let n = count * 3;
                    let mut out = Vec::with_capacity(seq.len() * n);
                    for _ in 0..n {
                        out.extend_from_slice(seq);
                    }
                    pty.write(out);
                }
            }
            WheelAction::Scrollback => {
                let delta = if up { WHEEL_LINES } else { -WHEEL_LINES } * count as i32;
                if let Some(pty) = &self.pty {
                    pty.term.lock().scroll_display(Scroll::Delta(delta));
                }
                self.mark_dirty(event_loop);
            }
        }
    }
}
