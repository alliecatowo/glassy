//! Input handling: PTY exit, cursor blink, bell, mouse, selection, clipboard.

use super::*;

impl App {
    /// Handle a child shell exit for pane `id`: close exactly that pane. A
    /// non-focused pane of the active tab collapses out of the split; the active
    /// focused pane (or a single-pane tab) closes the tab. Background-tab panes are
    /// dropped from their group (or the whole tab when it was their last pane).
    pub(crate) fn handle_child_exit(&mut self, id: usize, event_loop: &ActiveEventLoop) {
        // A non-focused pane of the ACTIVE tab: drop it from the split.
        if id != self.active_id
            && let Some(g) = self.panes.as_mut()
            && g.others.contains_key(&id)
        {
            if let Some(p) = g.others.remove(&id) {
                p.shutdown();
            }
            g.layout.close(id);
            let collapsed = g.layout.len() == 1;
            if collapsed {
                self.panes = None;
            }
            self.resize_panes();
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
            return;
        }
        // The active focused pane: if the tab is split, close just that pane;
        // otherwise close the whole tab (the original single-pane behaviour). After
        // a split the focused leaf id != active_id, so match active_focused_id().
        if id == self.active_id || id == self.active_focused_id() {
            // The exiting pane's blinking content vanishes; disarm the text-blink
            // timer so the event loop doesn't keep waking every BLINK_INTERVAL with
            // nothing to blink (0%-idle violation). Re-arms on the next
            // TextBlinkPresent if the surviving content actually blinks.
            self.text_blink_active = false;
            if self.is_split() {
                self.close_pane(event_loop);
            } else {
                self.close_active_tab(event_loop);
            }
            return;
        }
        // A background tab's pane.
        for s in self.background.iter_mut() {
            if let Some(g) = s.panes.as_mut()
                && g.others.contains_key(&id)
            {
                if let Some(p) = g.others.remove(&id) {
                    p.shutdown();
                }
                g.layout.close(id);
                if g.layout.len() == 1 {
                    s.panes = None;
                }
                return;
            }
        }
        // A background tab's focused pane (id == tab id): drop the whole tab,
        // shutting down any sibling panes it owned. (Matches the pre-split
        // behaviour, which left `tab_order` untouched here.)
        if let Some(bi) = self.background.iter().position(|s| s.id == id) {
            let s = self.background.remove(bi);
            if let Some(g) = s.panes {
                for (_, p) in g.others {
                    p.shutdown();
                }
            }
            self.update_window_title();
        }
    }

    /// Reset the blink to its visible phase and restart the timer. Called on
    /// keypress so the cursor is solid while actively typing, matching every
    /// mainstream terminal.
    pub(crate) fn reset_blink(&mut self) {
        self.blink_on = true;
        self.blink_at = Instant::now() + BLINK_INTERVAL;
    }

    /// React to a terminal bell. The visual bell starts (or extends) a brief
    /// window flash; the audible bell rings a soft beep. Both are gated by config
    /// (default: visual on, audible off). `user_event` marks the screen dirty
    /// after this, so the flash paints on the next frame.
    pub(crate) fn trigger_bell(&mut self) {
        if self.config.bell_visual {
            // (Re)arm the flash window. A burst of bells just keeps it lit rather
            // than stuttering. Force a full rebuild so every cell picks up the
            // tint this frame and drops it when the flash ends.
            self.bell_flash_until = Some(Instant::now() + Duration::from_millis(bell::FLASH_MS));
            self.force_full_redraw = true;
        }
        if self.config.bell_audible {
            self.audio_bell.ring();
        }
    }

    /// Mark the screen dirty and schedule a redraw no sooner than `next_frame`.
    pub(crate) fn mark_dirty(&mut self, event_loop: &ActiveEventLoop) {
        self.dirty = true;
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }

    /// Translate a physical cursor position into a 0-based grid cell, clamped to
    /// the visible grid. The renderer insets the grid by `pad` px on all sides.
    /// When the active tab is split, the cell is taken relative to the FOCUSED
    /// pane's tile (origin = rect + pad), since selection / mouse-reporting act on
    /// the focused pane (`self.pty`) and `self.cols/self.rows` track its grid.
    pub(crate) fn px_to_cell(&self, x: f64, y: f64) -> (usize, usize) {
        let Some(renderer) = self.renderer.as_ref() else {
            return (0, 0);
        };
        let m = renderer.cell_metrics();
        let pad = renderer.pad() as f64;
        if let Some(rect) = self.focused_pane_rect() {
            // Pane-local: subtract the tile origin + the per-pane pad inset.
            let ox = rect.x as f64 + pad;
            let oy = rect.y as f64 + pad;
            let col = ((x - ox) / m.width as f64).floor();
            let row = ((y - oy) / m.height as f64).floor();
            let col = (col.max(0.0) as usize).min(self.cols.saturating_sub(1));
            let row = (row.max(0.0) as usize).min(self.rows.saturating_sub(1));
            return (col, row);
        }
        let col = ((x - pad) / m.width as f64).floor();
        // The terminal grid starts below the GUI tab bar: subtract its pixel inset
        // (0 when the strip is hidden, so the top band maps to the first grid row).
        let grid_top = pad + self.effective_tab_bar_h() as f64;
        let term_row = ((y - grid_top) / m.height as f64).floor() as i64;
        let col = (col.max(0.0) as usize).min(self.cols.saturating_sub(1));
        let row = (term_row.max(0) as usize).min(self.rows.saturating_sub(1));
        (col, row)
    }

    /// Snapshot of the terminal's current mode flags (mouse reporting, alt
    /// screen, etc.). Returns an empty set before the PTY is up.
    pub(crate) fn term_mode(&self) -> TermMode {
        match self.pty.as_ref() {
            Some(pty) => *pty.term.lock().mode(),
            None => TermMode::empty(),
        }
    }

    /// Send a DECSET 1004 focus report (`seq`) to every pane PTY whose child has
    /// enabled focus reporting (`TermMode::FOCUS_IN_OUT`). Covers the focused pane
    /// (`self.pty`) and every parked pane in a split.
    pub(crate) fn report_focus(&self, seq: &[u8]) {
        let report = |pty: &Pty| {
            if pty.term.lock().mode().contains(TermMode::FOCUS_IN_OUT) {
                pty.write(seq.to_vec());
            }
        };
        if let Some(pty) = self.pty.as_ref() {
            report(pty);
        }
        if let Some(g) = self.panes.as_ref() {
            for pty in g.others.values() {
                report(pty);
            }
        }
    }

    /// Encode and send a mouse report to the child, choosing SGR vs legacy form
    /// based on the terminal's current mode.
    pub(crate) fn report_mouse(&self, button: u8, pressed: bool, motion: bool, mode: TermMode) {
        let Some(pty) = self.pty.as_ref() else { return };
        let (col, row) = self.mouse_cell;
        let sgr = mode.contains(TermMode::SGR_MOUSE);
        // SGR-Pixel reporting (DECSET 1016): only meaningful in SGR mode. Report
        // pixel coordinates relative to the terminal content origin (top-left of
        // the cell grid, i.e. inside the pad and below the tab bar / pane origin),
        // clamped to non-negative.
        let pixel = if sgr && self.sgr_pixel_mouse {
            Some(self.mouse_content_px())
        } else {
            None
        };
        let bytes = encode_mouse(
            MouseReport {
                button,
                col,
                row,
                pressed,
                motion,
                pixel,
            },
            self.mods,
            sgr,
        );
        pty.write(bytes);
    }

    /// The current pointer position as 0-based pixel coordinates relative to the
    /// terminal content origin (top-left of the cell grid), for SGR-Pixel mouse
    /// reporting (mode 1016). In a split this is measured from the focused pane's
    /// tile origin + pad; single-pane subtracts the pad and the tab-bar inset.
    /// Clamped to [0, grid extent] so a report never lands outside the grid.
    fn mouse_content_px(&self) -> (u32, u32) {
        let pad = self
            .renderer
            .as_ref()
            .map(|r| r.pad() as f64)
            .unwrap_or(0.0);
        let (mx, my) = self.mouse_px;
        let (ox, oy) = if let Some(rect) = self.focused_pane_rect() {
            (rect.x as f64 + pad, rect.y as f64 + pad)
        } else {
            (pad, pad + self.effective_tab_bar_h() as f64)
        };
        let px = (mx - ox).max(0.0);
        let py = (my - oy).max(0.0);
        // Clamp to the grid's pixel extent so an off-grid pointer (e.g. over the
        // pad) reports the edge rather than a value past the last cell.
        let (maxx, maxy) = self
            .renderer
            .as_ref()
            .map(|r| {
                let m = r.cell_metrics();
                (
                    (self.cols as f64 * m.width as f64).max(1.0),
                    (self.rows as f64 * m.height as f64).max(1.0),
                )
            })
            .unwrap_or((1.0, 1.0));
        (px.min(maxx - 1.0) as u32, py.min(maxy - 1.0) as u32)
    }

    /// Which side (left/right half) of its cell a physical x-coordinate falls on.
    /// Selection uses this so the boundary cell is included or excluded based on
    /// where exactly the pointer is, matching every other terminal.
    pub(crate) fn cell_side(&self, x: f64) -> Side {
        let Some(renderer) = self.renderer.as_ref() else {
            return Side::Left;
        };
        let m = renderer.cell_metrics();
        let pad = renderer.pad() as f64;
        // In a split, measure from the focused pane's tile origin (matching
        // `px_to_cell`), so the sub-cell boundary test stays correct per pane.
        let ox = self
            .focused_pane_rect()
            .map(|r| r.x as f64 + pad)
            .unwrap_or(pad);
        let rel = (x - ox) / m.width as f64;
        let frac = rel - rel.floor();
        if frac < 0.5 { Side::Left } else { Side::Right }
    }

    /// Convert a visible screen cell (col, row) to a grid `Point`. Screen rows
    /// map to grid lines by subtracting the scrollback display offset, the
    /// inverse of the `+ display_offset` used when rendering.
    pub(crate) fn grid_point(&self, col: usize, row: usize) -> Point {
        let display_offset = match self.pty.as_ref() {
            Some(pty) => pty.term.lock().grid().display_offset() as i32,
            None => 0,
        };
        Point::new(Line(row as i32 - display_offset), Column(col))
    }

    /// Begin a text selection at the current pointer location. `ty` selects the
    /// granularity (Simple for a single click, Semantic for double, Lines for
    /// triple).
    pub(crate) fn start_selection(&mut self, ty: SelectionType) {
        let Some(pty) = self.pty.as_ref() else { return };
        let (col, row) = self.mouse_cell;
        let point = self.grid_point(col, row);
        let side = self.cell_side(self.mouse_px.0);
        pty.term.lock().selection = Some(Selection::new(ty, point, side));
        self.selecting = true;
    }

    /// Extend the in-progress selection to the current pointer location.
    pub(crate) fn update_selection(&mut self) {
        let Some(pty) = self.pty.as_ref() else { return };
        let (col, row) = self.mouse_cell;
        let point = self.grid_point(col, row);
        let side = self.cell_side(self.mouse_px.0);
        let mut term = pty.term.lock();
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    /// Clear any active selection (e.g. a plain click away from it).
    pub(crate) fn clear_selection(&mut self) {
        if let Some(pty) = self.pty.as_ref() {
            pty.term.lock().selection = None;
        }
    }

    /// Select the entire terminal buffer (scrollback history + visible screen).
    /// Builds a `Lines`-granularity selection spanning the topmost history line
    /// to the bottom-right of the screen, so a subsequent Copy lifts everything.
    pub(crate) fn select_all(&mut self) {
        let Some(pty) = self.pty.as_ref() else { return };
        let mut term = pty.term.lock();
        let grid = term.grid();
        let cols = grid.columns();
        if cols == 0 {
            return;
        }
        let top = Line(-(grid.history_size() as i32));
        let bottom = Line(grid.screen_lines() as i32 - 1);
        let start = Point::new(top, Column(0));
        let end = Point::new(bottom, Column(cols - 1));
        let mut sel = Selection::new(SelectionType::Lines, start, Side::Left);
        sel.update(end, Side::Right);
        term.selection = Some(sel);
    }

    /// Clear the scrollback history (the lines above the visible screen). The
    /// visible screen is untouched, matching the common terminal "Clear
    /// scrollback" action; the display offset resets so the view snaps to the
    /// bottom.
    pub(crate) fn clear_scrollback(&mut self) {
        if let Some(pty) = self.pty.as_ref() {
            let mut term = pty.term.lock();
            term.selection = None;
            term.grid_mut().clear_history();
        }
    }

    /// Copy the current selection to the OS clipboard.
    pub(crate) fn copy_selection(&mut self) {
        let text = match self.pty.as_ref() {
            Some(pty) => pty.term.lock().selection_to_string(),
            None => None,
        };
        let Some(text) = text else { return };
        if text.is_empty() {
            return;
        }
        let cb = self.clipboard();
        if let Some(cb) = cb
            && let Err(e) = cb.set_text(text)
        {
            log::debug!("clipboard copy failed: {e}");
        }
    }

    /// Paste the OS clipboard contents into the child, honoring bracketed paste.
    pub(crate) fn paste_clipboard(&mut self) {
        let bracketed = self.term_mode().contains(TermMode::BRACKETED_PASTE);
        let text = self.clipboard().and_then(|cb| match cb.get_text() {
            Ok(t) => Some(t),
            Err(e) => {
                log::debug!("clipboard paste failed: {e}");
                None
            }
        });
        if let (Some(text), Some(pty)) = (text, self.pty.as_ref()) {
            pty.term.lock().scroll_display(Scroll::Bottom);
            // Honor broadcast input: a paste while broadcasting reaches every
            // pane of a split tab, matching the typed-input fan-out.
            if self.broadcast_input
                && let Some(g) = self.panes.as_ref()
            {
                pty.paste(&text, bracketed);
                for other in g.others.values() {
                    other.paste(&text, bracketed);
                }
            } else {
                pty.paste(&text, bracketed);
            }
        }
    }

    /// Copy the current selection to the X11/Wayland PRIMARY selection (Linux
    /// only). A no-op if the selection is empty or arboard's primary clipboard
    /// is unavailable. Designed to be called immediately after `copy_selection`
    /// so both the standard and PRIMARY clipboards are set simultaneously.
    ///
    /// Uses arboard's `SetExtLinux` trait with `LinuxClipboardKind::Primary`,
    /// which works on both X11 and Wayland (when the compositor supports it).
    #[cfg(target_os = "linux")]
    pub(crate) fn copy_selection_to_primary(&mut self) {
        let text = match self.pty.as_ref() {
            Some(pty) => pty.term.lock().selection_to_string(),
            None => None,
        };
        let Some(text) = text else { return };
        if text.is_empty() {
            return;
        }
        // Use arboard's SetExtLinux trait to target the PRIMARY selection.
        // PRIMARY is a best-effort convenience; log and swallow errors.
        use arboard::{LinuxClipboardKind, SetExtLinux};
        if let Some(cb) = self.clipboard()
            && let Err(e) = cb.set().clipboard(LinuxClipboardKind::Primary).text(text)
        {
            log::debug!("PRIMARY clipboard set failed: {e}");
        }
    }

    /// Paste from the X11/Wayland PRIMARY selection (Linux). Falls back to the
    /// standard clipboard when PRIMARY is empty or unavailable. This implements
    /// the standard X11/terminal middle-click-paste behaviour.
    #[cfg(target_os = "linux")]
    pub(crate) fn paste_primary_or_clipboard(&mut self) {
        // Try PRIMARY first via arboard's GetExtLinux trait.
        use arboard::{GetExtLinux, LinuxClipboardKind};
        let primary_text: Option<String> = if let Some(cb) = self.clipboard() {
            cb.get()
                .clipboard(LinuxClipboardKind::Primary)
                .text()
                .ok()
                .filter(|t| !t.is_empty())
        } else {
            None
        };
        if let Some(text) = primary_text {
            // Paste PRIMARY text directly.
            let bracketed = self.term_mode().contains(TermMode::BRACKETED_PASTE);
            if let Some(pty) = self.pty.as_ref() {
                pty.term
                    .lock()
                    .scroll_display(alacritty_terminal::grid::Scroll::Bottom);
                pty.paste(&text, bracketed);
            }
        } else {
            // PRIMARY unavailable: fall back to standard clipboard.
            self.paste_clipboard();
        }
    }

    /// Queue an in-app toast notification. The toast fades in, stays ~4 s, then
    /// fades out.  This is the sole entry point; toast rendering happens in the
    /// render path via `toast::paint_toasts`.
    pub(crate) fn push_toast(&mut self, message: impl Into<String>) {
        crate::app::toast::push(&mut self.toasts, message);
    }

    /// Toggle "broadcast input": when on, typed keys and pastes are mirrored to
    /// every pane of the active tab at once. A toast confirms the new state and
    /// a `BCAST` tag shows in the status bar while it is on. Repaints so the
    /// indicator updates immediately.
    pub(crate) fn toggle_broadcast_input(&mut self, event_loop: &ActiveEventLoop) {
        self.broadcast_input = !self.broadcast_input;
        let msg = if self.broadcast_input {
            "Broadcast input: ON — typing goes to all panes"
        } else {
            "Broadcast input: OFF"
        };
        self.push_toast(msg);
        // The status-bar indicator + toast are overlays not covered by terminal
        // damage; force a full rebuild so they appear/clear this frame.
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Write raw bytes to the child(ren), honoring broadcast input. When
    /// broadcast is off (or the tab is single-pane) this writes only to the
    /// focused pane, identical to a bare `pty.write`. When on AND the tab is
    /// split, the same bytes go to every pane of the active tab so all shells
    /// receive the keystrokes in lockstep.
    pub(crate) fn write_input(&self, bytes: Vec<u8>) {
        let Some(focused) = self.pty.as_ref() else {
            return;
        };
        match self.panes.as_ref() {
            Some(g) if self.broadcast_input => {
                // Fan out: focused pane + every parked pane. Clone per-pane so
                // each PTY owns its copy of the byte buffer.
                focused.write(bytes.clone());
                for pty in g.others.values() {
                    pty.write(bytes.clone());
                }
            }
            _ => focused.write(bytes),
        }
    }

    /// Note a left mouse PRESS for chrome double-click detection (drives
    /// word-select in editable text fields). A press close in px + time to the
    /// previous one sets `gui_double_click` for the next chrome paint to consume;
    /// the chain collapses after a double so a third quick press starts fresh.
    pub(crate) fn note_gui_left_press(&mut self) {
        const GUI_DBL_MS: Duration = Duration::from_millis(400);
        const GUI_DBL_PX: f32 = 6.0;
        let (px, py) = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
        let now = Instant::now();
        self.gui_double_click = matches!(
            self.gui_last_press,
            Some((lx, ly, lt))
                if now.duration_since(lt) < GUI_DBL_MS
                    && (lx - px).abs() <= GUI_DBL_PX
                    && (ly - py).abs() <= GUI_DBL_PX
        );
        self.gui_last_press = if self.gui_double_click {
            None
        } else {
            Some((px, py, now))
        };
    }

    /// Read the OS clipboard as text (best-effort), for editable text-field paste
    /// (search / palette / rename / settings fields).
    pub(crate) fn clipboard_text(&mut self) -> Option<String> {
        self.clipboard().and_then(|cb| cb.get_text().ok())
    }

    /// Copy `text` to the OS clipboard (best-effort), for editable text-field
    /// copy/cut (search / palette / rename / settings fields).
    pub(crate) fn copy_text_to_clipboard(&mut self, text: &str) {
        let owned = text.to_string();
        if let Some(cb) = self.clipboard()
            && let Err(e) = cb.set_text(owned)
        {
            log::debug!("clipboard copy (text field) failed: {e}");
        }
    }

    /// Lazily open the OS clipboard. Returns `None` if it is unavailable.
    pub(crate) fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(cb) => self.clipboard = Some(cb),
                Err(e) => {
                    log::debug!("clipboard unavailable: {e}");
                    return None;
                }
            }
        }
        self.clipboard.as_mut()
    }

    /// Build the inline app toolbar (screen row 0) cells: the glassy mark, tab
    /// chips (or the single-tab title), the +/help/menu buttons, and the
    /// scrollback-position readout. Returns one `(char, fg, bg)` per column so
    /// both the single-pane and split render paths push an identical strip. Takes
    /// the focused pane's `display_offset`/`history_size` for the % readout.
    /// Snapshot the tab state needed by [`paint_tab_bar`] under the live `&self`
    /// borrow, so the painter (which holds `&mut Renderer`, a split borrow of
    /// `self.renderer`) needs only owned data. Returns per-tab (title, active,
    /// busy-dot, spinning) tuples in stable display order.
    pub(crate) fn tab_bar_snapshot(&self) -> Vec<(String, bool, bool, bool)> {
        let now = Instant::now();
        self.tab_order
            .iter()
            .map(|&id| {
                if id == self.active_id {
                    let spinning = self.active_busy_until.is_some_and(|t| now < t);
                    // Title precedence: custom > OSC > foreground process name, so
                    // an idle tab reads "zsh"/"vim" instead of a bare placeholder.
                    let title = self
                        .active_custom_title
                        .clone()
                        .filter(|t| !t.trim().is_empty())
                        .or_else(|| {
                            (!self.active_title.trim().is_empty())
                                .then(|| self.active_title.clone())
                        })
                        .or_else(|| self.active_process_name())
                        .unwrap_or_default();
                    (title, true, false, spinning)
                } else {
                    self.background
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| {
                            let spinning = s.busy_until.is_some_and(|t| now < t);
                            let title = s
                                .custom_title
                                .clone()
                                .filter(|t| !t.trim().is_empty())
                                .or_else(|| (!s.title.trim().is_empty()).then(|| s.title.clone()))
                                .unwrap_or_else(|| Self::proc_label_for(&s.pty));
                            (title, false, s.activity, spinning)
                        })
                        .unwrap_or((String::new(), false, false, false))
                }
            })
            .collect()
    }

    /// Per-tab pane (leaf) counts in stable display order, for the split indicator
    /// on each chip. 1 means an un-split tab (no indicator). Mirrors the order of
    /// [`tab_bar_snapshot`] so the painter indexes them in parallel.
    pub(crate) fn tab_pane_counts(&self) -> Vec<usize> {
        self.tab_order
            .iter()
            .map(|&id| {
                if id == self.active_id {
                    self.panes.as_ref().map(|g| g.layout.len()).unwrap_or(1)
                } else {
                    self.background
                        .iter()
                        .find(|s| s.id == id)
                        .and_then(|s| s.panes.as_ref().map(|g| g.layout.len()))
                        .unwrap_or(1)
                }
            })
            .collect()
    }

    /// Hash digest of everything the tab-bar painter reads, so an unchanged frame
    /// can replay the cached overlay instead of re-shaping every tab title. The
    /// mouse position is only folded in while a tab is being dragged (the drag-ghost
    /// follows the pointer); otherwise pointer motion does not change the tab bar's
    /// appearance (hover is captured by `hovered`). Theme changes flow through
    /// `force_full_redraw` at the call site, not this key.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn tab_bar_key(
        snapshot: &[(String, bool, bool, bool)],
        focused: bool,
        hovered: Option<StripItem>,
        held: Option<StripItem>,
        dragging: Option<usize>,
        mouse: (f32, f32),
        spinner: usize,
        count: usize,
        strip_off: i32,
        strip_hist: usize,
        pane_counts: &[usize],
        active_pos: usize,
    ) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for (title, active, busy, spinning) in snapshot {
            title.hash(&mut h);
            active.hash(&mut h);
            busy.hash(&mut h);
            spinning.hash(&mut h);
        }
        // Split indicators + scroll-keep-active position change the drawn strip.
        pane_counts.hash(&mut h);
        active_pos.hash(&mut h);
        focused.hash(&mut h);
        hovered.hash(&mut h);
        held.hash(&mut h);
        dragging.hash(&mut h);
        // Spinner frame only matters while something is spinning; it is already
        // reflected by the per-tab `spinning` flags above, but the glyph index also
        // changes the drawn frame, so fold it in.
        spinner.hash(&mut h);
        count.hash(&mut h);
        // The scrollback % readout in the tag area changes the drawn text.
        strip_off.hash(&mut h);
        strip_hist.hash(&mut h);
        // Drag-ghost position: only relevant mid-drag.
        if dragging.is_some() {
            (mouse.0.to_bits()).hash(&mut h);
            (mouse.1.to_bits()).hash(&mut h);
        }
        h.finish()
    }
}
