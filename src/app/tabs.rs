//! Tab and session management, menu handling.

use super::*;

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
            menu_open: false,
            menu_sel: 0,
            menu_items: None,
            menu_anchor: None,
            menu_anchor_px: None,
            help_state: gui::HelpState::default(),
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
            active_busy_until: None,
            spinner_frame: 0,
            spinner_at: Instant::now() + SPINNER_INTERVAL,
            capture: std::env::var_os("GLASSY_CAPTURE").map(std::path::PathBuf::from),
            capture_deadline: None,
            force_full_redraw: true,
            tab_bar_key: None,
            prev_cursor: None,
            prev_display_offset: 0,
            prev_has_selection: false,
            gui_pressed: None,
            gui_focused: None,
            gui_anims: std::collections::HashMap::new(),
            gui_click_edge: false,
            gui_anim_last: Instant::now(),
            search: None,
            palette: None,
            palette_rows: Vec::new(),
        }
    }

    /// Compute grid dimensions for a physical surface size and the cell metrics.
    /// The renderer insets the grid by `pad` px on all four sides, so the usable
    /// area is reduced by `2 * pad` in each dimension.
    pub(crate) fn grid_for(size: PhysicalSize<u32>, cell_w: f32, cell_h: f32, pad: f32, status_bar_enabled: bool) -> (usize, usize) {
        let usable_w = (size.width as f32 - 2.0 * pad).max(0.0);
        let usable_h = (size.height as f32 - 2.0 * pad).max(0.0);
        let cols = ((usable_w / cell_w).floor() as usize).max(1);
        // Reserve the GUI tab bar at the top and the status bar at the bottom (both
        // in PIXELS). The tab-bar inset is applied via `Renderer::set_grid_origin_y`;
        // the status bar simply removes pixels from the available height when enabled.
        let status_bar_space = if status_bar_enabled { STATUS_BAR_H } else { 0.0 };
        let rows = (((usable_h - tab_bar_h(cell_h) - status_bar_space) / cell_h).floor() as usize)
            .max(1);
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
        if allowed
            && let Err(e) = std::process::Command::new("xdg-open").arg(url).spawn()
        {
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
                    (self.active_title.clone(), true, false)
                } else {
                    self.background
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| (s.title.clone(), false, s.activity))
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

    /// Invoke a menu action and close the dropdown.
    pub(crate) fn invoke_menu_action(&mut self, action: MenuAction, event_loop: &ActiveEventLoop) {
        self.menu_open = false;
        self.menu_items = None;
        self.menu_anchor = None;
        self.menu_anchor_px = None;
        self.force_full_redraw = true;
        match action {
            MenuAction::Copy => {
                self.copy_selection();
                self.mark_dirty(event_loop);
            }
            MenuAction::Paste => {
                self.paste_clipboard();
                self.mark_dirty(event_loop);
            }
            MenuAction::NewTab => self.new_tab(event_loop),
            MenuAction::Settings => {
                self.open_settings();
                self.mark_dirty(event_loop);
            }
            MenuAction::PaneHeaders => {
                self.toggle_pane_headers();
                self.mark_dirty(event_loop);
            }
            MenuAction::Help => {
                self.help_open = true;
                self.mark_dirty(event_loop);
            }
            MenuAction::CloseTab => self.close_active_tab(event_loop),
        }
    }

    /// Build the selection-aware item list for the right-click context menu.
    /// Copy is included only when a non-empty selection exists; Paste and New
    /// tab are always present. Settings/Help/CloseTab are omitted from the
    /// context menu (available via the hamburger).
    pub(crate) fn context_menu_items(&self) -> Vec<MenuAction> {
        let mut v = Vec::new();
        let has_sel = self
            .pty
            .as_ref()
            .and_then(|p| p.term.lock().selection_to_string())
            .filter(|s| !s.is_empty())
            .is_some();
        if has_sel {
            v.push(MenuAction::Copy);
        }
        v.push(MenuAction::Paste);
        v.push(MenuAction::NewTab);
        v
    }

    /// Open the right-click context menu anchored at the current pointer position
    /// in physical pixels, clamped so the panel stays fully on-screen.
    pub(crate) fn open_context_menu(&mut self, event_loop: &ActiveEventLoop) {
        let items = self.context_menu_items();
        if items.is_empty() {
            return; // guard: Paste+NewTab are always present, so never fires
        }

        // Pixel anchor: pointer position, with a rough panel-size estimate for
        // clamp. The exact panel size is not known until draw time (gui::menu
        // measures labels), so we use a conservative estimate.
        let (mx, my) = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
        let est_panel_w = items.iter().map(|a| a.label().len()).max().unwrap_or(0) as f32
            * 8.0   // approximate cell_w
            + 80.0; // icon + shortcut + padding
        let est_panel_h = items.len() as f32 * 22.0 + 8.0;
        let sw = self.renderer.as_ref().map(|r| r.surface_size().0 as f32).unwrap_or(800.0);
        let sh = self.renderer.as_ref().map(|r| r.surface_size().1 as f32).unwrap_or(600.0);
        let ax = mx.min(sw - est_panel_w).max(0.0);
        let ay = my.min(sh - est_panel_h).max(0.0);

        // Also keep legacy cell-based anchor for the old menu_hit_test path
        // (used by the mouse handler until fully replaced).
        let (col, term_row) = self.px_to_cell(self.mouse_px.0, self.mouse_px.1);
        let total_rows = self.rows + TAB_STRIP_ROWS;
        let label_w = items.iter().map(|a| a.label().len()).max().unwrap_or(0);
        let panel_h_c = items.len() + 2;
        let left = col.min(self.cols.saturating_sub(label_w + 4));
        let top = (term_row + TAB_STRIP_ROWS)
            .min(total_rows.saturating_sub(panel_h_c))
            .max(TAB_STRIP_ROWS);

        self.menu_items = Some(items);
        self.menu_anchor = Some((left, top));
        self.menu_anchor_px = Some((ax, ay));
        self.menu_sel = 0;
        self.menu_open = true;
        self.help_open = false;
        self.settings_open = false;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Close the dropdown menu and clear all associated state. Use this
    /// everywhere `menu_open` is set false so `menu_items`/`menu_anchor` never
    /// drift out of sync.
    pub(crate) fn close_menu(&mut self, event_loop: &ActiveEventLoop) {
        self.menu_open = false;
        self.menu_items = None;
        self.menu_anchor = None;
        self.menu_anchor_px = None;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Handle a keypress while the dropdown menu is open. Returns true if the
    /// key was consumed (caller should not forward to the child). Uses the live
    /// item list so navigation wraps correctly for both the hamburger (fixed 4
    /// items) and the context menu (variable length).
    pub(crate) fn handle_menu_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) -> bool {
        let n = self.menu_items.as_ref().map(|v| v.len()).unwrap_or(MenuAction::ALL.len());
        match key {
            Key::Named(NamedKey::ArrowUp) => {
                self.menu_sel = (self.menu_sel + n - 1) % n;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.menu_sel = (self.menu_sel + 1) % n;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                true
            }
            Key::Named(NamedKey::Enter) => {
                let items = self.menu_items.clone();
                let action = match &items {
                    Some(v) => v.get(self.menu_sel).copied(),
                    None => MenuAction::ALL.get(self.menu_sel).copied(),
                };
                if let Some(a) = action {
                    self.invoke_menu_action(a, event_loop);
                }
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.close_menu(event_loop);
                true
            }
            _ => false,
        }
    }

    /// Hit-test a mouse click at physical pixel `(x, y)` against the open
    /// dropdown menu. Returns the `MenuAction` if an enabled item was clicked,
    /// `None` otherwise. Uses the pixel anchor set by `menu_anchor_px` so the
    /// hit-area exactly matches what `gui::menu` draws (§3.6).
    pub(crate) fn menu_hit_test(&self, x: f64, y: f64) -> Option<MenuAction> {
        let renderer = self.renderer.as_ref()?;
        let m = renderer.cell_metrics();
        let (ax, ay) = if let Some(p) = self.menu_anchor_px {
            p
        } else if let Some((left, top)) = self.menu_anchor {
            // Legacy fallback: convert cell-based anchor to pixels.
            let pad = renderer.pad();
            (left as f32 * m.width + pad, top as f32 * m.height + pad)
        } else {
            return None;
        };

        let items: &[MenuAction] = self.menu_items.as_deref().unwrap_or(MenuAction::ALL);
        let has_sel = self.pty.as_ref()
            .and_then(|p| p.term.lock().selection_to_string())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let entries = actions_to_entries(items, has_sel);

        // Replicate gui::menu's row layout to find which item was hit.
        let cell_h = m.height;
        let cell_w = m.width;
        let row_h = (cell_h * 1.4).round().max(cell_h + 4.0);
        let sep_h  = 5.0_f32;
        // Panel width estimation (mirrors gui::menu — just needs to be wide enough
        // that hits inside it are valid; exact width used for x-clamping).
        let label_chars = items.iter().map(|a| a.label().len()).max().unwrap_or(4);
        let hint_chars  = items.iter().filter_map(|a| a.shortcut()).map(|h| h.len()).max().unwrap_or(0);
        let pad_x = (cell_w * 1.2).round();
        let icon_w = cell_w + 4.0;
        let hint_gap = (cell_w * 2.0).round();
        let panel_w = (icon_w
            + label_chars as f32 * cell_w
            + if hint_chars > 0 { hint_gap + hint_chars as f32 * cell_w } else { 0.0 }
            + pad_x * 2.0)
            .max(cell_w * 8.0)
            .ceil();

        let x = x as f32;
        let y = y as f32;
        if x < ax || x >= ax + panel_w {
            return None;
        }
        if y < ay {
            return None;
        }

        let mut ry = ay + 2.0;
        let mut item_idx: usize = 0;
        for entry in &entries {
            match entry {
                gui::MenuEntry::Separator => {
                    ry += sep_h;
                }
                gui::MenuEntry::Item { enabled, .. } => {
                    if y >= ry && y < ry + row_h {
                        if !enabled {
                            return None; // greyed out
                        }
                        return items.get(item_idx).copied();
                    }
                    item_idx += 1;
                    ry += row_h;
                }
            }
        }
        None
    }

    pub(crate) fn strip_click(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let Some(renderer) = self.renderer.as_ref() else {
            return false;
        };
        let m = renderer.cell_metrics();
        let (x, y) = self.mouse_px;
        // The tab bar occupies the pixel band [0, tab_bar_h).
        if y >= tab_bar_h(m.height) as f64 {
            return false;
        }
        // Hit-test against the pixel tab-bar layout (the same helper the painter
        // uses, so click targets match what's drawn).
        let item = self.strip_item_at_px(x as f32, y as f32);
        // Record the pressed item so the strip draws it inset (released in the
        // MouseInput handler), giving the click visible tactility.
        self.held_strip_item = item;
        match item {
            Some(StripItem::Tab(pos)) => {
                self.activate_tab(pos, event_loop);
                // Begin a potential drag-to-reorder from this slot (the tab is now
                // active at `pos`); CursorMoved reorders, release ends it.
                self.dragging_tab = Some(self.active_pos());
            }
            Some(StripItem::TabClose(pos)) => self.close_tab(pos, event_loop),
            Some(StripItem::NewTab) => self.new_tab(event_loop),
            Some(StripItem::Help) => {
                self.help_open = !self.help_open;
                self.settings_open = false;
                self.menu_open = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            Some(StripItem::Settings) => {
                if self.settings_open {
                    self.settings_open = false;
                } else {
                    self.open_settings();
                }
                self.help_open = false;
                self.menu_open = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            Some(StripItem::Menu) => {
                // Toggle the hamburger dropdown; close other overlays.
                self.menu_open = !self.menu_open;
                self.menu_sel = 0;
                if self.menu_open {
                    // Hamburger: uses MenuAction::ALL; anchor top-right below strip.
                    self.menu_items = None;
                    let label_w =
                        MenuAction::ALL.iter().map(|a| a.label().len()).max().unwrap_or(0);
                    let panel_w = label_w + 4;
                    self.menu_anchor = Some((self.cols.saturating_sub(panel_w), TAB_STRIP_ROWS));
                    // Pixel anchor: below the # button, at the right of the window.
                    if let Some(r) = self.renderer.as_ref() {
                        let (sw, _sh) = r.surface_size();
                        // Panel width derived from LIVE cell metrics, mirroring the
                        // formula in `gui::menu` (icon + widest label + widest
                        // shortcut + padding), so the right-anchored dropdown lines
                        // up exactly with the painted panel at any font size — no
                        // hardcoded 220px estimate that drifts with DPI/font.
                        let m = r.cell_metrics();
                        let cell_w = m.width;
                        let label_chars =
                            MenuAction::ALL.iter().map(|a| a.label().len()).max().unwrap_or(4);
                        let hint_chars = MenuAction::ALL
                            .iter()
                            .filter_map(|a| a.shortcut().map(|h| h.len()))
                            .max()
                            .unwrap_or(0);
                        let pad_x = (cell_w * 1.2).round();
                        let icon_w = cell_w + 4.0;
                        let hint_gap = (cell_w * 2.0).round();
                        let est_w = (icon_w
                            + label_chars as f32 * cell_w
                            + if hint_chars > 0 {
                                hint_gap + hint_chars as f32 * cell_w
                            } else {
                                0.0
                            }
                            + pad_x * 2.0)
                            .max(cell_w * 8.0)
                            .ceil();
                        let bar_h = tab_bar_h(m.height);
                        self.menu_anchor_px = Some(((sw as f32 - est_w).max(0.0), bar_h + 2.0));
                    }
                } else {
                    self.menu_items = None;
                    self.menu_anchor = None;
                    self.menu_anchor_px = None;
                }
                self.help_open = false;
                self.settings_open = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            None => {} // inert gap (native bar handles window drag)
        }
        true
    }

    /// Clear transient pointer/selection state. Called when switching tabs so an
    /// in-progress drag or hovered link from the old tab doesn't bleed into the new.
    pub(crate) fn reset_pointer_state(&mut self) {
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
            });
        }
        self.tab_order.push(id);
        self.pty = Some(pty);
        self.active_id = id;
        self.active_title.clear();
        self.active_busy_until = None;
        // The new tab starts at the inherited cwd; OSC 7 updates it as the user cd's.
        self.active_cwd = cwd;
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
            });
        }
        let bi = self.background.iter().position(|s| s.id == target_id).unwrap_or(bi);
        let target = self.background.remove(bi);
        self.pty = Some(target.pty);
        self.panes = target.panes;
        self.active_id = target.id;
        self.active_title = target.title;
        // Inherit the activated session's busy deadline (it streams in the fg now).
        self.active_busy_until = target.busy_until;
        // Restore the activated session's cwd so a new tab/split inherits it.
        self.active_cwd = target.last_cwd;
        // A split tab may have been parked at a different window size; re-tile it.
        if self.panes.is_some() {
            self.resize_panes();
        }
        self.reset_pointer_state();
        self.update_window_title();
        // A full repaint so the new tab's grid replaces the old one's persisted
        // rows (otherwise stale content from the other tab bleeds through).
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
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
                self.active_cwd = next.last_cwd;
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
