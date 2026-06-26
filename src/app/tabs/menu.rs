//! Dropdown menu (hamburger + context menu): open, close, key-nav, hit-test,
//! and the tab-strip click dispatcher.

use super::super::*;

impl App {
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
            MenuAction::SelectAll => {
                self.select_all();
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            MenuAction::ClearScrollback => {
                self.clear_scrollback();
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            MenuAction::Search => self.open_search(event_loop),
            MenuAction::SplitRight => self.split_pane(pane::Dir::Vertical, event_loop),
            MenuAction::SplitDown => self.split_pane(pane::Dir::Horizontal, event_loop),
            MenuAction::NewTab => self.new_tab(event_loop),
            MenuAction::Settings => {
                self.open_settings();
                self.mark_dirty(event_loop);
            }
            MenuAction::Help => {
                self.help_open = true;
                self.mark_dirty(event_loop);
            }
            MenuAction::CloseTab => self.try_close_active_tab(event_loop),
        }
    }

    /// Build the selection-aware item list for the right-click context menu.
    /// Copy is included only when a non-empty selection exists; Paste and New
    /// tab are always present. Settings/Help/CloseTab are omitted from the
    /// context menu (available via the hamburger).
    pub(crate) fn context_menu_items(&self) -> Vec<MenuAction> {
        // Copy is always listed (greyed out when nothing is selected) so the
        // menu layout is stable; `actions_to_entries` reads the live selection
        // state to decide its enabled flag. Groups are separated automatically.
        vec![
            MenuAction::Copy,
            MenuAction::Paste,
            MenuAction::SelectAll,
            MenuAction::ClearScrollback,
            MenuAction::Search,
            MenuAction::SplitRight,
            MenuAction::SplitDown,
            MenuAction::NewTab,
            MenuAction::Settings,
            MenuAction::Help,
        ]
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
            + 120.0; // icon + shortcut + padding
        // Height accounts for separators (4 group boundaries in the context menu)
        // so the on-screen clamp keeps the now-taller panel fully visible.
        let est_panel_h = items.len() as f32 * 24.0 + 5.0 * 5.0 + 8.0;
        let sw = self
            .renderer
            .as_ref()
            .map(|r| r.surface_size().0 as f32)
            .unwrap_or(800.0);
        let sh = self
            .renderer
            .as_ref()
            .map(|r| r.surface_size().1 as f32)
            .unwrap_or(600.0);
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
        self.overlay_opened_by_press = false;
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
        let n = self
            .menu_items
            .as_ref()
            .map(|v| v.len())
            .unwrap_or(MenuAction::ALL.len());
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
        let has_sel = self
            .pty
            .as_ref()
            .and_then(|p| p.term.lock().selection_to_string())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let entries = actions_to_entries(items, has_sel);

        // Replicate gui::menu's row layout to find which item was hit.
        let cell_h = m.height;
        let cell_w = m.width;
        let row_h = (cell_h * 1.4).round().max(cell_h + 4.0);
        let sep_h = 5.0_f32;
        // Panel width estimation (mirrors gui::menu — just needs to be wide enough
        // that hits inside it are valid; exact width used for x-clamping).
        let label_chars = items.iter().map(|a| a.label().len()).max().unwrap_or(4);
        let hint_chars = items
            .iter()
            .filter_map(|a| a.shortcut())
            .map(|h| h.len())
            .max()
            .unwrap_or(0);
        let pad_x = (cell_w * 1.2).round();
        let icon_w = cell_w + 4.0;
        let hint_gap = (cell_w * 2.0).round();
        let panel_w = (icon_w
            + label_chars as f32 * cell_w
            + if hint_chars > 0 {
                hint_gap + hint_chars as f32 * cell_w
            } else {
                0.0
            }
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
        if self.renderer.is_none() {
            return false;
        }
        let (x, y) = self.mouse_px;
        // Hit-test against the pixel layout (the same helper the painter uses, so
        // click targets match what's drawn). When the full bar is hidden, `tab_layout`
        // returns only the floating icon segments, so clicks anywhere else return None
        // and fall through to the terminal — no separate y-range guard needed.
        let item = self.strip_item_at_px(x as f32, y as f32);
        if item.is_none() {
            // Empty space in the top chrome band: drag the window (macOS, where the
            // OS title bar is hidden and its auto-drag disabled, so we move the
            // window manually — this is what the titlebar drag used to do). Below
            // the band, fall through to the terminal.
            #[cfg(target_os = "macos")]
            if (y as f32) < self.effective_tab_bar_h()
                && let Some(w) = self.window.as_ref()
            {
                let _ = w.drag_window();
                return true;
            }
            return false;
        }
        // Record the pressed item so the strip draws it inset (released in the
        // MouseInput handler), giving the click visible tactility.
        self.held_strip_item = item;
        match item {
            Some(StripItem::Tab(pos)) => {
                // Double-click on a chip opens the inline rename editor (the second
                // click of a pair on the same chip, within the multi-click window).
                const MULTI_CLICK: Duration = Duration::from_millis(400);
                let now = Instant::now();
                let is_double = matches!(
                    self.last_tab_click,
                    Some((p, t)) if p == pos && now.duration_since(t) < MULTI_CLICK
                );
                self.last_tab_click = Some((pos, now));
                self.activate_tab(pos, event_loop);
                if is_double {
                    self.begin_tab_rename(pos, event_loop);
                    // Don't also start a drag on the rename gesture.
                    self.held_strip_item = None;
                } else {
                    // Begin a potential drag-to-reorder from this slot (the tab is
                    // now active at `pos`); CursorMoved reorders, release ends it.
                    self.dragging_tab = Some(self.active_pos());
                }
            }
            Some(StripItem::TabClose(pos)) => {
                // Guard the active tab close with a running-child check; closing
                // a background tab is always immediate (we can't easily check its
                // /proc state without activating it).
                let is_active = self.tab_order.get(pos) == Some(&self.active_id);
                if is_active && self.has_running_child() {
                    self.confirm_close = Some(ConfirmClose::ActiveTab);
                    self.force_full_redraw = true;
                    self.mark_dirty(event_loop);
                } else {
                    self.close_tab(pos, event_loop);
                }
            }
            Some(StripItem::NewTab) => self.new_tab(event_loop),
            Some(StripItem::Help) => {
                let opening = !self.help_open;
                self.help_open = opening;
                self.settings_open = false;
                self.menu_open = false;
                // When this press OPENS an overlay, the release of the same
                // button must not be treated as a click-outside-panel dismiss.
                if opening {
                    self.overlay_opened_by_press = true;
                }
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            Some(StripItem::Settings) => {
                let opening = !self.settings_open;
                if self.settings_open {
                    self.settings_open = false;
                } else {
                    self.open_settings();
                }
                self.help_open = false;
                self.menu_open = false;
                if opening {
                    self.overlay_opened_by_press = true;
                }
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            Some(StripItem::Menu) => {
                // Toggle the hamburger dropdown; close other overlays.
                let opening = !self.menu_open;
                self.menu_open = opening;
                self.menu_sel = 0;
                if self.menu_open {
                    // Hamburger: uses MenuAction::ALL; anchor top-right below strip.
                    self.menu_items = None;
                    let label_w = MenuAction::ALL
                        .iter()
                        .map(|a| a.label().len())
                        .max()
                        .unwrap_or(0);
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
                        let label_chars = MenuAction::ALL
                            .iter()
                            .map(|a| a.label().len())
                            .max()
                            .unwrap_or(4);
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
                        // Always anchor below the icon-band height, even when
                        // the full tab bar is hidden. Without this, the menu
                        // opens at y=2 (directly under the hamburger button) and
                        // the mouse release fires on the first menu item.
                        let icon_band_h = tab_bar_h(m.height);
                        self.menu_anchor_px =
                            Some(((sw as f32 - est_w).max(0.0), icon_band_h + 2.0));
                    }
                } else {
                    self.menu_items = None;
                    self.menu_anchor = None;
                    self.menu_anchor_px = None;
                }
                self.help_open = false;
                self.settings_open = false;
                // The hamburger dropdown does NOT use `overlay_opened_by_press`
                // for its dismiss (it closes on a press outside the panel via
                // `menu_hit_test`, never on the opening gesture's release), so we
                // must NOT set the flag here — leaving it set would leak into the
                // next overlay and swallow that overlay's first dismiss click.
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            None => {
                // Double-click on empty tab bar area toggles maximize.
                const MULTI_CLICK: Duration = Duration::from_millis(400);
                let now = Instant::now();
                let is_double = matches!(
                    self.last_tab_click,
                    Some((p, t)) if p == usize::MAX && now.duration_since(t) < MULTI_CLICK
                );
                // Use usize::MAX as a sentinel for "empty area" to distinguish from tab positions.
                self.last_tab_click = Some((usize::MAX, now));
                if is_double {
                    // Inert gap, but we got a double-click: toggle maximize.
                    if let Some(w) = self.window.as_ref() {
                        let maximized = w.is_maximized();
                        w.set_maximized(!maximized);
                    }
                }
            } // inert gap (native bar handles window drag for single-clicks)
        }
        true
    }
}
