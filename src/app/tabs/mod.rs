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
            search: None,
            palette: None,
            palette_rows: Vec::new(),
            active_progress: None,
            text_blink_on: true,
            text_blink_at: Instant::now() + BLINK_INTERVAL,
            text_blink_active: false,
            toasts: Vec::new(),
            confirm_close: None,
            pending_confirm_execute: false,
        }
    }

    /// Compute grid dimensions for a physical surface size and the cell metrics.
    /// The renderer insets the grid by `pad` px on all four sides, so the usable
    /// area is reduced by `2 * pad` in each dimension.
    pub(crate) fn grid_for(
        size: PhysicalSize<u32>,
        cell_w: f32,
        cell_h: f32,
        pad: f32,
        status_bar_enabled: bool,
    ) -> (usize, usize) {
        let usable_w = (size.width as f32 - 2.0 * pad).max(0.0);
        let usable_h = (size.height as f32 - 2.0 * pad).max(0.0);
        let cols = ((usable_w / cell_w).floor() as usize).max(1);
        // Reserve the GUI tab bar at the top and the status bar at the bottom (both
        // in PIXELS). The tab-bar inset is applied via `Renderer::set_grid_origin_y`;
        // the status bar simply removes pixels from the available height when enabled.
        let status_bar_space = if status_bar_enabled {
            STATUS_BAR_H
        } else {
            0.0
        };
        let rows =
            (((usable_h - tab_bar_h(cell_h) - status_bar_space) / cell_h).floor() as usize).max(1);
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
        } else if let Some(path) = url.strip_prefix("file://") {
            // file:// is permitted for genuine local links, but terminal output
            // must not be able to hand xdg-open a path that launches arbitrary
            // handlers or pokes pseudo-filesystems. Block .desktop launchers and
            // the /proc, /dev, /sys trees outright.
            let lower = path.to_ascii_lowercase();
            !(lower.ends_with(".desktop")
                || path.starts_with("/proc")
                || path.starts_with("/dev")
                || path.starts_with("/sys"))
        } else {
            false
        };
        if allowed && let Err(e) = std::process::Command::new("xdg-open").arg(url).spawn() {
            log::warn!("failed to open {url}: {e}");
        }
    }

    /// Total number of open tabs (active + background).
    pub(crate) fn tab_count(&self) -> usize {
        self.background.len() + self.pty.is_some() as usize
    }

    /// Reflect the active tab in the native (CSD) window title.
    pub(crate) fn update_window_title(&self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        window.set_title(&os_title(&self.active_title));
    }

    /// Tab descriptors in stable display order: (title, is_active, has_activity).
    /// Shared by the tab-bar painter and the click/drag hit-tests so the drawn
    /// items and the click targets always agree.
    pub(crate) fn tab_descs(&self) -> Vec<(String, bool, bool)> {
        self.tab_order
            .iter()
            .map(|&id| {
                if id == self.active_id {
                    // A custom (renamed) title overrides the OSC title.
                    let title = self
                        .active_custom_title
                        .clone()
                        .unwrap_or_else(|| self.active_title.clone());
                    (title, true, false)
                } else {
                    self.background
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| {
                            let title = s.custom_title.clone().unwrap_or_else(|| s.title.clone());
                            (title, false, s.activity)
                        })
                        .unwrap_or((String::new(), false, false))
                }
            })
            .collect()
    }

    /// The live pixel tab-bar layout, built from the current descriptors and the
    /// renderer's surface width + cell metrics. Empty if the renderer is absent.
    pub(crate) fn tab_layout(&self) -> Vec<StripSeg> {
        let Some(r) = self.renderer.as_ref() else {
            return Vec::new();
        };
        let m = r.cell_metrics();
        let (sw, _sh) = r.surface_size();
        let descs = self.tab_descs();
        let refs: Vec<(&str, bool, bool)> =
            descs.iter().map(|(t, a, b)| (t.as_str(), *a, *b)).collect();
        strip_layout(&refs, sw as f32, tab_bar_h(m.height), m.width)
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
        let id = self.next_id;
        // Inherit the current tab's cwd (from OSC 7) so the new tab opens where the
        // user is, not in $HOME.
        let cwd = self.active_cwd.clone();
        let pty = match Pty::spawn(
            self.proxy.clone(),
            id,
            self.cols,
            self.rows,
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
        if self.panes.is_some() {
            self.resize_panes();
        }
        // Reset per-session keyboard state: the activated session manages its own
        // modifyOtherKeys level independently via XTMODKEYS negotiation.
        self.modify_other_keys = ModifyOtherKeys::default();
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
