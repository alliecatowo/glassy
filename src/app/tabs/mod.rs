//! Tab and session management, menu handling.

use super::*;

mod ctxmenu;
mod menu;
mod rename;
mod session;

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>, config: Config) -> Self {
        Self {
            proxy,
            config,
            window: None,
            renderer: None,
            pty: None,
            panes: None,
            background: Vec::new(),
            tab_order: vec![0], // the first tab (spawned in resumed) is id 0
            active_id: 0,
            active_title: String::new(),
            active_custom_title: None,
            active_pane_cwds: std::collections::HashMap::new(),
            tab_rename: None,
            last_tab_click: None,
            session_dirty: false,
            active_cwd: None,
            next_id: 1,
            cols: 0,
            rows: 0,
            base_font_px: None,
            mods: ModifiersState::empty(),
            focused: true,
            started: Instant::now(),
            first_frame_done: false,
            dragging_tab: None,
            dragging_gutter: None,
            hovered_gutter: None,
            hovered_strip_item: None,
            held_strip_item: None,
            tab_scroll_accum: 0.0,
            content_scroll_accum: 0.0,
            swipe_consumed: false,
            help_open: false,
            settings_open: false,
            settings_drop: gui::SettingsDrop::None,
            settings_panel: gui::Rect::default(),
            settings_saved: false,
            settings_word_sep: gui::TextEdit::default(),
            settings_word_sep_ms: gui::TextInputMouse::default(),
            settings_font_feat: gui::TextEdit::default(),
            settings_font_feat_ms: gui::TextInputMouse::default(),
            menu_open: false,
            menu_sel: 0,
            menu_items: None,
            menu_anchor: None,
            menu_anchor_px: None,
            help_state: gui::HelpState::default(),
            tab_menu_target: None,
            tab_menu_sel: 0,
            tab_menu_anchor_px: None,
            hovered_pane_header: None,
            pane_menu_open: None,
            pane_menu_sel: 0,
            mouse_cell: (0, 0),
            mouse_px: (0.0, 0.0),
            held_button: None,
            selecting: false,
            last_click: None,
            hovered_link: None,
            clipboard: None,
            bell_flash_until: None,
            audio_bell: AudioBell::new(),
            dirty: false,
            next_frame: Instant::now(),
            refresh: Duration::from_micros(16_666), // 60 Hz default until queried
            blink_on: true,
            blink_at: Instant::now() + BLINK_INTERVAL,
            cursor_blinks: false,
            active_busy_until: None,
            spinner_frame: 0,
            spinner_at: Instant::now() + SPINNER_INTERVAL,
            capture: std::env::var_os("GLASSY_CAPTURE").map(std::path::PathBuf::from),
            capture_deadline: None,
            script: None,
            force_full_redraw: true,
            tab_bar_key: None,
            prev_cursor: None,
            prev_display_offset: 0,
            prev_has_selection: false,
            gui_pressed: None,
            gui_focused: None,
            gui_anims: std::collections::HashMap::new(),
            gui_click_edge: false,
            gui_double_click: false,
            gui_last_press: None,
            gui_click_pos: (0.0, 0.0),
            overlay_opened_by_press: false,
            gui_anim_last: Instant::now(),
            modify_other_keys: ModifyOtherKeys::default(),
            sgr_pixel_mouse: false,
            search: None,
            palette: None,
            palette_rows: Vec::new(),
            active_progress: None,
            text_blink_on: true,
            text_blink_at: Instant::now() + BLINK_INTERVAL,
            text_blink_active: false,
            toasts: Vec::new(),
            peek: None,
            confirm_close: None,
            pending_confirm_execute: false,
            broadcast_input: false,
            hints: None,
            fold_state: command_blocks::FoldState::default(),
            minimap_cache: Default::default(),
            minimap_dragging: false,
            // Quake state is armed lazily in `resumed()` once the window exists
            // (only when `config.quake` is set); `None` keeps normal mode untouched.
            quake: None,
            preedit: None,
            cmd_history: std::collections::VecDeque::new(),
            cwd_history: std::collections::VecDeque::new(),
        }
    }

    /// Compute grid dimensions for a physical surface size and the cell metrics.
    /// The renderer insets the grid by `pad` px on all four sides, so the usable
    /// area is reduced by `2 * pad` in each dimension.
    pub(crate) fn grid_for(
        size: PhysicalSize<u32>,
        cell_w: f32,
        cell_h: f32,
        pad_x: f32,
        pad_y: f32,
        status_bar_enabled: bool,
        tab_strip_h: f32,
    ) -> (usize, usize) {
        // `pad_x`/`pad_y` are the TOTAL horizontal/vertical insets (left+right,
        // top+bottom) — not doubled here, since the sides can differ.
        let usable_w = (size.width as f32 - pad_x).max(0.0);
        let usable_h = (size.height as f32 - pad_y).max(0.0);
        let cols = ((usable_w / cell_w).floor() as usize).max(1);
        // Reserve the GUI tab bar at the top and the status bar at the bottom (both
        // in PIXELS). `tab_strip_h` is the strip's pixel height (0 when hidden); the
        // inset is applied via `Renderer::set_grid_origin_y`. The status bar simply
        // removes pixels from the available height when enabled.
        let status_bar_space = if status_bar_enabled {
            STATUS_BAR_H
        } else {
            0.0
        };
        let rows = (((usable_h - tab_strip_h - status_bar_space) / cell_h).floor() as usize).max(1);
        (cols, rows)
    }

    /// The OSC8 hyperlink URI at a visible screen cell, if the cell carries one.
    pub(crate) fn cell_hyperlink(&self, col: usize, row: usize) -> Option<String> {
        let pty = self.pty.as_ref()?;
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let point = self.grid_point(col, row);
        let term = pty.term.lock();
        term.grid()[point].hyperlink().map(|h| h.uri().to_owned())
    }

    /// Open a URL with the system handler, detached. Restricted to web/file
    /// schemes so terminal output can't launch arbitrary URI handlers.
    pub(crate) fn open_url(url: &str) {
        let allowed = if url.starts_with("http://") || url.starts_with("https://") {
            true
        } else if let Some(raw_path) = url.strip_prefix("file://") {
            // file:// is permitted for genuine local links, but terminal output
            // must not be able to hand the opener a path that launches arbitrary
            // handlers or pokes pseudo-filesystems. The blocklist must run on the
            // DECODED, normalized path — otherwise `%2e%2e`, `%2f`, or a
            // percent-encoded `/proc` slips straight past a raw-string check.
            file_url_path_allowed(raw_path)
        } else {
            false
        };
        if !allowed {
            log::warn!("refusing to open {url}: scheme/path not allowed");
            return;
        }
        // Per-platform system opener. (The scheme/path allowlist above runs first.)
        #[cfg(target_os = "macos")]
        let mut cmd = {
            let mut c = std::process::Command::new("open");
            c.arg(url);
            c
        };
        #[cfg(target_os = "windows")]
        let mut cmd = {
            // `start` is a cmd builtin; the empty "" is the window-title arg so a
            // quoted URL isn't mistaken for the title.
            let mut c = std::process::Command::new("cmd");
            c.args(["/C", "start", "", url]);
            c
        };
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let mut cmd = {
            let mut c = std::process::Command::new("xdg-open");
            c.arg(url);
            c
        };
        if let Err(e) = cmd.spawn() {
            log::warn!("failed to open {url}: {e}");
        }
    }

    /// Total number of open tabs (active + background).
    pub(crate) fn tab_count(&self) -> usize {
        self.background.len() + self.pty.is_some() as usize
    }

    /// Tab descriptors in stable display order: (title, is_active, has_activity).
    /// Shared by the tab-bar painter and the click/drag hit-tests so the drawn
    /// items and the click targets always agree.
    pub(crate) fn tab_descs(&self) -> Vec<(String, bool, bool)> {
        self.tab_order
            .iter()
            .map(|&id| {
                if id == self.active_id {
                    // Title precedence: custom (renamed) > OSC > foreground process
                    // name (vim/cargo/zsh) so an idle tab shows its shell rather
                    // than a bare "shell" placeholder.
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
                    (title, true, false)
                } else {
                    self.background
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| {
                            let title = s
                                .custom_title
                                .clone()
                                .filter(|t| !t.trim().is_empty())
                                .or_else(|| (!s.title.trim().is_empty()).then(|| s.title.clone()))
                                .unwrap_or_else(|| Self::proc_label_for(&s.pty));
                            (title, false, s.activity)
                        })
                        .unwrap_or((String::new(), false, false))
                }
            })
            .collect()
    }

    /// The live pixel tab-bar layout, built from the current descriptors and the
    /// renderer's surface width + cell metrics. Empty if the renderer is absent or
    /// the strip is hidden (so no hidden hit-targets linger). Uses the same tag
    /// reserve + active-position the painter uses so clicks land where drawn.
    pub(crate) fn tab_layout(&self) -> Vec<StripSeg> {
        let Some(r) = self.renderer.as_ref() else {
            return Vec::new();
        };
        let m = r.cell_metrics();
        let (sw, _sh) = r.surface_size();
        let bar_h = tab_bar_h(m.height);
        if !self.tab_bar_visible() {
            // Bar hidden: still return the icon button segments so clicks on
            // the floating Help/Settings/Menu buttons are correctly hit-tested.
            return floating_icon_segs(sw as f32, bar_h);
        }
        let descs = self.tab_descs();
        let refs: Vec<(&str, bool, bool)> =
            descs.iter().map(|(t, a, b)| (t.as_str(), *a, *b)).collect();
        let tag_reserve = tab_tag_reserve(self.tab_count(), m.width);
        strip_layout_ex(
            &refs,
            sw as f32,
            bar_h,
            m.width,
            tag_reserve,
            self.active_pos(),
            self.chrome_left_inset(),
        )
    }

    /// The tab-bar item at physical pixel `(px, py)`, if any. Shared by click +
    /// drag-reorder so they agree with what's painted.
    pub(crate) fn strip_item_at_px(&self, px: f32, py: f32) -> Option<StripItem> {
        strip_item_at(&self.tab_layout(), px, py)
    }

    /// While a tab is held (`dragging_tab`), reorder it under the pointer at pixel
    /// `(px, py)`: if the pointer is over a different tab slot, move the dragged
    /// tab there in `tab_order`. Returns true if a reorder happened (repaint).
    pub(crate) fn drag_tab_to(&mut self, px: f32, py: f32) -> bool {
        let Some(from) = self.dragging_tab else {
            return false;
        };
        let to = match self.strip_item_at_px(px, py) {
            Some(StripItem::Tab(p)) | Some(StripItem::TabClose(p)) => p,
            _ => return false,
        };
        if to == from || from >= self.tab_order.len() || to >= self.tab_order.len() {
            return false;
        }
        move_in_order(&mut self.tab_order, from, to);
        self.dragging_tab = Some(to);
        true
    }

    /// Clear transient pointer/selection state. Called when switching tabs so an
    /// in-progress drag or hovered link from the old tab doesn't bleed into the new.
    /// Also flags the session for re-persist: every tab/split structural mutation
    /// funnels through here, so this is the single hook for session-dirty tracking.
    pub(crate) fn reset_pointer_state(&mut self) {
        self.session_dirty = true;
        self.selecting = false;
        self.held_button = None;
        self.hovered_link = None;
        self.last_click = None;
        self.dragging_tab = None;
        // Drop any gutter drag/hover (layout may have changed) and restore the
        // default OS cursor; the next CursorMoved re-arms feedback if warranted.
        if self.dragging_gutter.take().is_some() || self.hovered_gutter.take().is_some() {
            self.apply_gutter_cursor(None);
        }
        // Dismiss the pane ⋮ menu: layout or focus changed.
        self.pane_menu_open = None;
    }

    /// Open a new tab and make it active, parking the current tab in `background`.
    pub(crate) fn new_tab(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let m = renderer.cell_metrics();
        let (pad_x, pad_y) = (renderer.pad_x(), renderer.pad_y());
        let id = self.next_id;
        // Inherit the current tab's cwd (from OSC 7) so the new tab opens where the
        // user is, not in $HOME.
        let cwd = self.active_cwd.clone();
        // Spawn at the FULL single-pane grid, not self.cols/self.rows — those hold
        // the focused pane's (possibly half-width) dims when the current tab is
        // split, which would make the new single-pane tab's shell start half-width
        // and paint over only half until the next resize. The new tab makes the
        // strip visible (≥2 tabs / Always), so reserve the strip + macOS inset.
        let (spawn_cols, spawn_rows) = match self.window.as_ref().map(|w| w.inner_size()) {
            Some(sz) if sz.width > 0 && sz.height > 0 => {
                let strip_h = tab_bar_h(m.height).max(self.chrome_top_inset());
                Self::grid_for(
                    sz,
                    m.width,
                    m.height,
                    pad_x,
                    pad_y,
                    self.config.status_bar,
                    strip_h,
                )
            }
            _ => (self.cols, self.rows),
        };
        let pty = match Pty::spawn(
            self.proxy.clone(),
            id,
            spawn_cols,
            spawn_rows,
            m.width.round() as u16,
            m.height.round() as u16,
            self.config.shell.clone(),
            cwd.clone(),
            self.config.scrollback,
            &self.config.word_separator,
            self.config.cursor_style.to_cursor_shape(),
            self.config.cursor_blink,
        ) {
            Ok(p) => p,
            Err(e) => {
                log::error!("failed to spawn tab: {e:#}");
                return;
            }
        };
        self.next_id += 1;
        // Park the current active into the background pool, append the new tab to
        // the stable order, and make it active.
        if let Some(old) = self.pty.take() {
            self.background.push(Session {
                id: self.active_id,
                pty: old,
                panes: self.panes.take(),
                title: std::mem::take(&mut self.active_title),
                activity: false,
                // Carry the parked session's busy state so its chip keeps spinning.
                busy_until: self.active_busy_until.take(),
                last_cwd: self.active_cwd.take(),
                custom_title: self.active_custom_title.take(),
                pane_cwds: std::mem::take(&mut self.active_pane_cwds),
            });
        }
        self.tab_order.push(id);
        self.pty = Some(pty);
        self.active_id = id;
        self.active_title.clear();
        self.active_custom_title = None;
        self.active_busy_until = None;
        // The new tab starts at the inherited cwd; OSC 7 updates it as the user cd's.
        self.active_cwd = cwd;
        // New session always starts with the default modifyOtherKeys level.
        self.modify_other_keys = ModifyOtherKeys::default();
        self.reset_pointer_state();
        // Opening the 2nd tab reveals the Auto-mode strip; reflow so the grid (and
        // the just-spawned tab) account for the strip's reclaimed height.
        self.reflow_grid();
        self.update_window_title();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Switch to the next/previous tab in the stable order (wrapping).
    pub(crate) fn cycle_tab(&mut self, delta: isize, event_loop: &ActiveEventLoop) {
        let n = self.tab_order.len();
        if n < 2 {
            return;
        }
        let pos = self.active_pos();
        let next = (((pos as isize + delta) % n as isize + n as isize) % n as isize) as usize;
        self.activate_tab(next, event_loop);
    }

    /// Move one tab in `tab_order` WITHOUT wrapping. A swipe gesture clamps at the
    /// first/last tab instead of spinning around like an infinite carousel.
    pub(crate) fn step_tab(&mut self, dir: isize, event_loop: &ActiveEventLoop) {
        let pos = self.active_pos();
        let next = pos as isize + dir;
        if next < 0 || next >= self.tab_order.len() as isize {
            return;
        }
        self.activate_tab(next as usize, event_loop);
    }

    /// Reorder the active tab one slot left (`dir < 0`) or right (`dir > 0`) in
    /// `tab_order`, without wrapping. The highlight follows the moved tab; clamps
    /// (no-op) at the first/last slot. Used by the MoveTabLeft/MoveTabRight key
    /// actions. Marks the session dirty so the new order is persisted.
    pub(crate) fn move_active_tab(&mut self, dir: isize, event_loop: &ActiveEventLoop) {
        let from = self.active_pos();
        let to = from as isize + dir;
        if to < 0 || to >= self.tab_order.len() as isize {
            return;
        }
        let to = to as usize;
        move_in_order(&mut self.tab_order, from, to);
        // Only the order changed (active_id is unchanged), so no PTY swap is
        // needed — just flag a repaint + persist.
        self.session_dirty = true;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Position of the active tab within `tab_order`.
    pub(crate) fn active_pos(&self) -> usize {
        self.tab_order
            .iter()
            .position(|&id| id == self.active_id)
            .unwrap_or(0)
    }

    /// Make the tab at stable position `pos` active. The display order is NOT
    /// changed — only the highlight moves. No-op if it's already active.
    pub(crate) fn activate_tab(&mut self, pos: usize, event_loop: &ActiveEventLoop) {
        let Some(&target_id) = self.tab_order.get(pos) else {
            return;
        };
        if target_id == self.active_id {
            return;
        }
        let Some(bi) = self.background.iter().position(|s| s.id == target_id) else {
            return;
        };
        // Park the current active, then swap in the target (clearing its activity).
        if let Some(cur) = self.pty.take() {
            self.background.push(Session {
                id: self.active_id,
                pty: cur,
                panes: self.panes.take(),
                title: std::mem::take(&mut self.active_title),
                activity: false,
                // Carry the parked session's busy state so its chip keeps spinning.
                busy_until: self.active_busy_until.take(),
                last_cwd: self.active_cwd.take(),
                custom_title: self.active_custom_title.take(),
                pane_cwds: std::mem::take(&mut self.active_pane_cwds),
            });
        }
        let bi = self
            .background
            .iter()
            .position(|s| s.id == target_id)
            .unwrap_or(bi);
        let target = self.background.remove(bi);
        self.pty = Some(target.pty);
        self.panes = target.panes;
        self.active_id = target.id;
        self.active_title = target.title;
        self.active_custom_title = target.custom_title;
        self.active_pane_cwds = target.pane_cwds;
        // Inherit the activated session's busy deadline (it streams in the fg now).
        self.active_busy_until = target.busy_until;
        // Restore the activated session's cwd so a new tab/split inherits it.
        self.active_cwd = target.last_cwd;
        // A split tab may have been parked at a different window size; re-tile it.
        // A single-pane tab needs the full grid: self.cols/rows may still hold the
        // previously-active tab's focused-pane (e.g. half) width, which would render
        // the activated shell over only part of the window until the next resize.
        if self.panes.is_some() {
            self.resize_panes();
        } else {
            self.reflow_grid();
        }
        // Reset per-session keyboard state: the activated session manages its own
        // modifyOtherKeys level independently via XTMODKEYS negotiation.
        self.modify_other_keys = ModifyOtherKeys::default();
        // Disarm the text-blink timer: it tracks the *active* pane's SGR 5/6 cells,
        // and the newly-activated tab may have none. Leaving it armed would wake the
        // event loop every BLINK_INTERVAL forever (0%-idle violation). It re-arms on
        // the next TextBlinkPresent from this session if its content actually blinks.
        self.text_blink_active = false;
        self.reset_pointer_state();
        self.update_window_title();
        // A full repaint so the new tab's grid replaces the old one's persisted
        // rows (otherwise stale content from the other tab bleeds through).
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// True when the active focused pane has a running foreground child (best-
    /// effort via `PaneInfo.foreground_comm` which is polled from `/proc`).
    /// Returns `false` when the shell is idle (prompt), no PTY, or the read fails.
    pub(crate) fn has_running_child(&self) -> bool {
        self.pty
            .as_ref()
            .and_then(|p| p.pane_info.foreground_comm.as_deref())
            .map(|comm| !comm.is_empty())
            .unwrap_or(false)
    }

    /// Like `close_pane` but checks for a running child first. When one is
    /// detected, sets `confirm_close` (shows the modal) instead of closing
    /// immediately. The modal's "Close" button then calls `close_pane` directly.
    pub(crate) fn try_close_pane(&mut self, event_loop: &ActiveEventLoop) {
        if self.has_running_child() {
            self.confirm_close = Some(ConfirmClose::ActivePane);
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
        } else {
            self.close_pane(event_loop);
        }
    }

    /// Like `close_active_tab` but checks for a running child first.
    pub(crate) fn try_close_active_tab(&mut self, event_loop: &ActiveEventLoop) {
        if self.has_running_child() {
            self.confirm_close = Some(ConfirmClose::ActiveTab);
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
        } else {
            self.close_active_tab(event_loop);
        }
    }

    /// Close the active tab; activate the neighbor at its position, else exit.
    pub(crate) fn close_active_tab(&mut self, event_loop: &ActiveEventLoop) {
        self.close_tab(self.active_pos(), event_loop);
    }

    /// Close the tab at stable position `pos`. If it's the active tab, activate
    /// the neighbor that slides into its slot; if the last tab closes, exit.
    pub(crate) fn close_tab(&mut self, pos: usize, event_loop: &ActiveEventLoop) {
        let Some(&id) = self.tab_order.get(pos) else {
            return;
        };
        let was_active = id == self.active_id;
        self.tab_order.remove(pos);

        if was_active {
            if let Some(pty) = &self.pty {
                pty.shutdown();
            }
            // Shut down every other pane of this tab too.
            if let Some(g) = self.panes.take() {
                for (_, p) in g.others {
                    p.shutdown();
                }
            }
            self.pty = None;
            self.active_title.clear();
            self.active_custom_title = None;
            self.active_pane_cwds.clear();
            self.active_cwd = None; // the closed tab's cwd is gone
            if self.tab_order.is_empty() {
                event_loop.exit();
                return;
            }
            // Activate whatever tab now occupies the closed slot (clamped).
            let new_pos = pos.min(self.tab_order.len() - 1);
            let new_id = self.tab_order[new_pos];
            if let Some(bi) = self.background.iter().position(|s| s.id == new_id) {
                let next = self.background.remove(bi);
                self.pty = Some(next.pty);
                self.panes = next.panes;
                self.active_id = next.id;
                self.active_title = next.title;
                self.active_custom_title = next.custom_title;
                self.active_pane_cwds = next.pane_cwds;
                self.active_cwd = next.last_cwd;
                // Mirror activate_tab: carry the promoted tab's busy-spinner state
                // and reset the modifyOtherKeys level (it is per-session and must
                // not leak the closed tab's negotiated encoding to the new one).
                self.active_busy_until = next.busy_until;
                self.modify_other_keys = ModifyOtherKeys::default();
                if self.panes.is_some() {
                    self.resize_panes();
                }
            }
        } else if let Some(bi) = self.background.iter().position(|s| s.id == id) {
            // Closing a background tab: shut it (and all its panes) down and drop it.
            let s = self.background.remove(bi);
            s.pty.shutdown();
            if let Some(g) = s.panes {
                for (_, p) in g.others {
                    p.shutdown();
                }
            }
        }
        self.reset_pointer_state();
        // Closing back to a single tab hides the Auto-mode strip; reflow so the
        // surviving tab reclaims the strip's height.
        self.reflow_grid();
        self.update_window_title();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    // --- Split panes -------------------------------------------------------
    //
    // The active tab may be tiled into several panes via `self.panes`. The
    // FOCUSED pane's PTY is always `self.pty`, so every single-pane code path
    // (input, selection, scrollback, mouse-report, cursor) automatically targets
    // the focused pane with no changes. `panes == None` is the one-pane case and
    // is byte-identical to the pre-split app.

    /// Pixel gutter reserved between tiled panes (also the divider thickness).
    pub(crate) const PANE_GAP: i32 = 1;

    /// Height of each pane's title bar in physical px (split mode only; the
    /// single-pane path skips headers entirely). Carved from the top of each
    /// leaf rect before grid layout and scissor so the cell grid sits below it.
    pub(crate) const PANE_HEADER_H: i32 = 22;

    /// Horizontal inner padding for the pane header text (px).
    pub(crate) const PANE_HEADER_PAD: f32 = 8.0;
}

/// Whether a `file://` URL's path (the part after the scheme, still possibly
/// percent-encoded) is safe to hand to the system opener. Terminal output is
/// untrusted, so we percent-decode and normalize FIRST, then reject `.desktop`
/// launchers and the `/proc`, `/dev`, `/sys` pseudo-filesystems. Decoding before
/// the check closes the `%2e%2e` / `%2f` / encoded-`/proc` bypass.
fn file_url_path_allowed(raw_path: &str) -> bool {
    // Drop a `?query`/`#fragment` so they can't smuggle in a blocked suffix and
    // so the extension check sees the real trailing component.
    let raw_path = raw_path.split(['?', '#']).next().unwrap_or(raw_path);
    let decoded = decode_percent_lossy(raw_path);
    // Normalize `..`/`.` segments so an encoded or literal `/foo/../proc` can't
    // resolve into a blocked tree after the textual prefix check.
    let normalized = normalize_path_segments(&decoded);
    let lower = normalized.to_ascii_lowercase();
    !(lower.ends_with(".desktop")
        || lower == "/proc"
        || lower.starts_with("/proc/")
        || lower == "/dev"
        || lower.starts_with("/dev/")
        || lower == "/sys"
        || lower.starts_with("/sys/"))
}

/// Percent-decode (`%XX` -> byte) into a `String`, lossily for non-UTF-8. Invalid
/// escapes pass through literally. Used to canonicalize untrusted `file://` paths
/// before the security blocklist so encoded separators can't hide blocked trees.
fn decode_percent_lossy(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Collapse `.`/`..` segments in an absolute-style path textually (no filesystem
/// access). A leading `/` is preserved; `..` pops the previous segment (never
/// above root). This canonicalizes `/foo/../proc` to `/proc` so the blocklist
/// can't be walked around with traversal segments.
fn normalize_path_segments(path: &str) -> String {
    let is_abs = path.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                stack.pop();
            }
            other => stack.push(other),
        }
    }
    let joined = stack.join("/");
    if is_abs { format!("/{joined}") } else { joined }
}

#[cfg(test)]
mod url_tests {
    use super::{decode_percent_lossy, file_url_path_allowed, normalize_path_segments};

    #[test]
    fn allows_ordinary_local_files() {
        assert!(file_url_path_allowed("/home/alice/notes.txt"));
        assert!(file_url_path_allowed("/home/alice/My%20Code/main.rs"));
        assert!(file_url_path_allowed("/tmp/report.pdf"));
    }

    #[test]
    fn blocks_pseudo_filesystems_and_desktop_launchers() {
        assert!(!file_url_path_allowed("/proc/self/mem"));
        assert!(!file_url_path_allowed("/dev/sda"));
        assert!(!file_url_path_allowed("/sys/kernel"));
        assert!(!file_url_path_allowed("/home/alice/evil.desktop"));
        assert!(!file_url_path_allowed("/home/alice/Evil.DESKTOP")); // case-insensitive
    }

    #[test]
    fn percent_encoded_bypass_is_closed() {
        // The whole point of the fix: encoded separators / dots must not slip a
        // blocked tree past the blocklist. Each of these decodes to a /proc path.
        assert!(!file_url_path_allowed("/%70roc/self/mem")); // %70 == 'p'
        assert!(!file_url_path_allowed("%2Fproc/self/mem")); // leading %2F == '/'
        assert!(!file_url_path_allowed("/proc%2Fself")); // encoded inner slash
        // Encoded .desktop suffix.
        assert!(!file_url_path_allowed("/home/alice/evil%2Edesktop"));
    }

    #[test]
    fn dot_dot_traversal_into_blocked_tree_is_closed() {
        // A path that textually starts safe but resolves into /proc must be caught.
        assert!(!file_url_path_allowed("/home/alice/../../proc/self/mem"));
        assert!(!file_url_path_allowed("/var/../proc"));
        // Encoded `..` (%2e%2e) combined with traversal.
        assert!(!file_url_path_allowed("/home/%2e%2e/%2e%2e/proc/cpuinfo"));
    }

    #[test]
    fn query_and_fragment_are_stripped_before_check() {
        // A blocked path can't be hidden behind a ? or # (and a benign one with a
        // fragment still passes).
        assert!(file_url_path_allowed("/home/alice/doc.txt?x=1#frag"));
        assert!(!file_url_path_allowed("/proc/self#frag"));
    }

    #[test]
    fn decode_percent_lossy_basics() {
        assert_eq!(decode_percent_lossy("a%20b"), "a b");
        assert_eq!(decode_percent_lossy("%2Fproc"), "/proc");
        // A malformed trailing escape is passed through literally, not dropped.
        assert_eq!(decode_percent_lossy("end%2"), "end%2");
        assert_eq!(decode_percent_lossy("nopercent"), "nopercent");
    }

    #[test]
    fn normalize_collapses_dot_segments() {
        assert_eq!(normalize_path_segments("/a/b/../c"), "/a/c");
        assert_eq!(normalize_path_segments("/a/./b"), "/a/b");
        assert_eq!(normalize_path_segments("/a/../../b"), "/b"); // can't pop past root
        assert_eq!(normalize_path_segments("/proc/../proc"), "/proc");
    }
}
