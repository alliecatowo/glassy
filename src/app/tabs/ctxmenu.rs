//! Tab right-click context menu: a small floating menu anchored at the pointer
//! that operates on ONE specific tab (the chip that was right-clicked), distinct
//! from the global `# hamburger` / terminal context menu in `menu.rs`.
//!
//! Actions: Rename, Duplicate, Close tab, Close others, Move left, Move right —
//! grouped with separators. The menu reuses the shared `gui::menu` painter and
//! the same press/click edge plumbing as the other dropdowns; its state lives in
//! `tab_menu_target` / `tab_menu_sel` / `tab_menu_anchor_px` so it never collides
//! with the global menu (`menu_open`).

use super::super::*;

/// One row of the tab context menu. Each variant acts on the tab position stored
/// in `App::tab_menu_target`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TabMenuAction {
    Rename,
    Duplicate,
    CloseTab,
    CloseOthers,
    MoveLeft,
    MoveRight,
}

impl TabMenuAction {
    pub(crate) fn label(self) -> &'static str {
        match self {
            TabMenuAction::Rename => "Rename",
            TabMenuAction::Duplicate => "Duplicate",
            TabMenuAction::CloseTab => "Close tab",
            TabMenuAction::CloseOthers => "Close others",
            TabMenuAction::MoveLeft => "Move left",
            TabMenuAction::MoveRight => "Move right",
        }
    }

    fn icon(self) -> char {
        match self {
            TabMenuAction::Rename => '*',
            TabMenuAction::Duplicate => '+',
            TabMenuAction::CloseTab => '✕',
            TabMenuAction::CloseOthers => '✕',
            TabMenuAction::MoveLeft => '<',
            TabMenuAction::MoveRight => '>',
        }
    }

    /// Visual group id (separator drawn between differing groups): 0 = edit,
    /// 1 = close, 2 = reorder.
    fn group(self) -> u8 {
        match self {
            TabMenuAction::Rename | TabMenuAction::Duplicate => 0,
            TabMenuAction::CloseTab | TabMenuAction::CloseOthers => 1,
            TabMenuAction::MoveLeft | TabMenuAction::MoveRight => 2,
        }
    }
}

impl App {
    /// The ordered action list for the tab context menu acting on `pos`. Move
    /// left/right are disabled at the ends; Close others is disabled with a single
    /// tab open.
    pub(crate) fn tab_menu_actions(&self) -> Vec<TabMenuAction> {
        vec![
            TabMenuAction::Rename,
            TabMenuAction::Duplicate,
            TabMenuAction::CloseTab,
            TabMenuAction::CloseOthers,
            TabMenuAction::MoveLeft,
            TabMenuAction::MoveRight,
        ]
    }

    /// Whether a given tab-menu action is enabled for target `pos`.
    fn tab_menu_enabled(&self, a: TabMenuAction, pos: usize) -> bool {
        let n = self.tab_order.len();
        match a {
            TabMenuAction::CloseOthers => n > 1,
            TabMenuAction::MoveLeft => pos > 0,
            TabMenuAction::MoveRight => pos + 1 < n,
            _ => true,
        }
    }

    /// Build the `gui::MenuEntry` list for the tab context menu (with separators
    /// between groups and per-action enabled flags).
    pub(crate) fn tab_menu_entries(&self, pos: usize) -> Vec<gui::MenuEntry<'static>> {
        let actions = self.tab_menu_actions();
        let mut v: Vec<gui::MenuEntry<'static>> = Vec::with_capacity(actions.len() + 2);
        let mut prev_group: Option<u8> = None;
        for a in actions {
            if prev_group.is_some_and(|g| g != a.group()) {
                v.push(gui::MenuEntry::Separator);
            }
            v.push(gui::MenuEntry::Item {
                icon: a.icon(),
                label: a.label(),
                hint: None,
                enabled: self.tab_menu_enabled(a, pos),
            });
            prev_group = Some(a.group());
        }
        v
    }

    /// Open the tab context menu for the tab at stable position `pos`, anchored at
    /// the current pointer (clamped on-screen). Closes conflicting overlays.
    pub(crate) fn open_tab_menu(&mut self, pos: usize, event_loop: &ActiveEventLoop) {
        if pos >= self.tab_order.len() {
            return;
        }
        let (mx, my) = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
        let actions = self.tab_menu_actions();
        // Conservative panel-size estimate for on-screen clamping (the exact size
        // is computed by `gui::menu` at draw time from live metrics).
        let label_w = actions.iter().map(|a| a.label().len()).max().unwrap_or(0) as f32;
        let est_w = label_w * 8.0 + 48.0;
        let est_h = actions.len() as f32 * 22.0 + 12.0;
        let (sw, sh) = self
            .renderer
            .as_ref()
            .map(|r| r.surface_size())
            .map(|(w, h)| (w as f32, h as f32))
            .unwrap_or((800.0, 600.0));
        let ax = mx.min(sw - est_w).max(0.0);
        let ay = my.min(sh - est_h).max(0.0);

        self.tab_menu_target = Some(pos);
        self.tab_menu_sel = 0;
        self.tab_menu_anchor_px = Some((ax, ay));
        // The tab menu owns the keyboard/pointer; dismiss the global overlays.
        self.menu_open = false;
        self.menu_items = None;
        self.pane_menu_open = None;
        self.help_open = false;
        self.settings_open = false;
        self.overlay_opened_by_press = false;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Close the tab context menu and clear its state.
    pub(crate) fn close_tab_menu(&mut self, event_loop: &ActiveEventLoop) {
        if self.tab_menu_target.is_none() {
            return;
        }
        self.tab_menu_target = None;
        self.tab_menu_anchor_px = None;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Snapshot for the painter: `(entries, ax, ay, sel)` when the menu is open.
    pub(crate) fn tab_menu_snapshot(
        &self,
    ) -> Option<(Vec<gui::MenuEntry<'static>>, f32, f32, usize)> {
        let pos = self.tab_menu_target?;
        let (ax, ay) = self.tab_menu_anchor_px?;
        Some((self.tab_menu_entries(pos), ax, ay, self.tab_menu_sel))
    }

    /// Map an `item` index (0-based among non-separator rows) to its action,
    /// using the same ordering the painter draws.
    fn tab_menu_action_at(&self, item: usize) -> Option<TabMenuAction> {
        self.tab_menu_actions().into_iter().nth(item)
    }

    /// Hit-test a click at physical pixel `(x, y)` against the open tab menu,
    /// returning the action if an enabled row was hit. Mirrors the row geometry of
    /// `gui::menu` (and `menu_hit_test`).
    pub(crate) fn tab_menu_hit_test(&self, x: f64, y: f64) -> Option<TabMenuAction> {
        let pos = self.tab_menu_target?;
        let (ax, ay) = self.tab_menu_anchor_px?;
        let renderer = self.renderer.as_ref()?;
        let m = renderer.cell_metrics();
        let cell_h = m.height;
        let cell_w = m.width;
        let row_h = (cell_h * 1.4).round().max(cell_h + 4.0);
        let sep_h = 5.0_f32;
        let entries = self.tab_menu_entries(pos);

        // Panel width (mirror gui::menu) for the x-bounds check.
        let label_chars = entries
            .iter()
            .filter_map(|e| match e {
                gui::MenuEntry::Item { label, .. } => Some(label.len()),
                _ => None,
            })
            .max()
            .unwrap_or(4);
        let pad_x = (cell_w * 1.2).round();
        let icon_w = cell_w + 4.0;
        let panel_w = (icon_w + label_chars as f32 * cell_w + pad_x * 2.0)
            .max(cell_w * 8.0)
            .ceil();

        let x = x as f32;
        let y = y as f32;
        if x < ax || x >= ax + panel_w || y < ay {
            return None;
        }
        let mut ry = ay + 2.0;
        let mut item_idx: usize = 0;
        for entry in &entries {
            match entry {
                gui::MenuEntry::Separator => ry += sep_h,
                gui::MenuEntry::Item { enabled, .. } => {
                    if y >= ry && y < ry + row_h {
                        if !enabled {
                            return None;
                        }
                        return self.tab_menu_action_at(item_idx);
                    }
                    item_idx += 1;
                    ry += row_h;
                }
            }
        }
        None
    }

    /// Invoke a tab-menu action on the stored target tab, then close the menu.
    pub(crate) fn invoke_tab_menu_action(
        &mut self,
        action: TabMenuAction,
        event_loop: &ActiveEventLoop,
    ) {
        let Some(pos) = self.tab_menu_target else {
            return;
        };
        // Close first so the action's own overlay/state changes win (e.g. Rename
        // opens the inline editor, which must not be re-dismissed by close).
        self.tab_menu_target = None;
        self.tab_menu_anchor_px = None;
        self.force_full_redraw = true;
        match action {
            TabMenuAction::Rename => self.begin_tab_rename(pos, event_loop),
            TabMenuAction::Duplicate => {
                // Activate the target first so the new tab inherits its cwd, then
                // open a fresh tab (new_tab already inherits the active cwd).
                self.activate_tab(pos, event_loop);
                self.new_tab(event_loop);
            }
            TabMenuAction::CloseTab => self.close_tab(pos, event_loop),
            TabMenuAction::CloseOthers => self.close_other_tabs(pos, event_loop),
            TabMenuAction::MoveLeft => self.move_tab(pos, -1, event_loop),
            TabMenuAction::MoveRight => self.move_tab(pos, 1, event_loop),
        }
        self.mark_dirty(event_loop);
    }

    /// Handle a keypress while the tab menu is open. Returns true if consumed.
    /// Up/Down move (skipping separators implicitly by item count), Enter invokes
    /// the highlighted enabled row, Esc closes.
    pub(crate) fn handle_tab_menu_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) -> bool {
        if self.tab_menu_target.is_none() {
            return false;
        }
        let n = self.tab_menu_actions().len();
        match key {
            Key::Named(NamedKey::ArrowUp) => {
                self.tab_menu_sel = (self.tab_menu_sel + n - 1) % n;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.tab_menu_sel = (self.tab_menu_sel + 1) % n;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                true
            }
            Key::Named(NamedKey::Enter) => {
                if let (Some(pos), Some(a)) = (
                    self.tab_menu_target,
                    self.tab_menu_action_at(self.tab_menu_sel),
                ) && self.tab_menu_enabled(a, pos)
                {
                    self.invoke_tab_menu_action(a, event_loop);
                }
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.close_tab_menu(event_loop);
                true
            }
            _ => false,
        }
    }

    /// Close every tab EXCEPT the one at stable position `keep`. Activates the
    /// kept tab, then shuts down all others.
    pub(crate) fn close_other_tabs(&mut self, keep: usize, event_loop: &ActiveEventLoop) {
        let Some(&keep_id) = self.tab_order.get(keep) else {
            return;
        };
        if self.tab_order.len() < 2 {
            return;
        }
        // Make the kept tab active so the remaining single tab is the foreground.
        self.activate_tab(keep, event_loop);
        // Tear down every parked background session.
        for s in self.background.drain(..) {
            s.pty.shutdown();
            if let Some(g) = s.panes {
                for (_, p) in g.others {
                    p.shutdown();
                }
            }
        }
        self.tab_order.retain(|&id| id == keep_id);
        self.reset_pointer_state();
        self.update_window_title();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Move the tab at stable position `pos` by `dir` (-1 left / +1 right) in the
    /// display order, clamped at the ends. Keeps the same tab active.
    pub(crate) fn move_tab(&mut self, pos: usize, dir: i32, event_loop: &ActiveEventLoop) {
        let n = self.tab_order.len();
        let to = pos as i32 + dir;
        if to < 0 || to as usize >= n {
            return;
        }
        move_in_order(&mut self.tab_order, pos, to as usize);
        self.session_dirty = true;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }
}
