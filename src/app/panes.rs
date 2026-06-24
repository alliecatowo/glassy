//! Pane layout, splitting, focusing, and gutter interaction.

use super::*;

impl App {
    /// The content rectangle (surface pixels) that panes tile: the whole surface
    /// below the tab strip. Each pane is internally inset by the renderer pad (the
    /// renderer adds `pad` to every cell), so this spans edge-to-edge and the pad
    /// supplies the symmetric margin within each pane. Returns `None` before the
    /// renderer exists.
    pub(crate) fn content_area(&self) -> Option<pane::Rect> {
        let r = self.renderer.as_ref()?;
        let m = r.cell_metrics();
        // The content (panes/grid) begins below the GUI tab bar and ends above the
        // status bar. Both insets are in pixels; the per-pane `pad` is applied by
        // the pane sizing math independently.
        let strip_bottom = tab_bar_h(m.height).round() as i32;
        let status_h = if self.config.status_bar { STATUS_BAR_H.round() as i32 } else { 0 };
        let (sw, sh) = r.surface_size();
        Some(pane::Rect {
            x: 0,
            y: strip_bottom,
            w: sw as i32,
            h: (sh as i32 - strip_bottom - status_h).max(0),
        })
    }

    /// Convert a pane's pixel rect into a (cols, rows) grid size for its PTY. The
    /// renderer insets cells by `pad` on the top-left, so a pane's usable extent
    /// is its rect minus one pad on each side (mirroring the whole-window inset).
    pub(crate) fn pane_grid(&self, rect: pane::Rect) -> (usize, usize) {
        let Some(r) = self.renderer.as_ref() else {
            return (1, 1);
        };
        let m = r.cell_metrics();
        let pad = r.pad();
        let cols = (((rect.w as f32 - 2.0 * pad) / m.width).floor() as usize).max(1);
        let rows = (((rect.h as f32 - 2.0 * pad) / m.height).floor() as usize).max(1);
        (cols, rows)
    }

    /// Resize every pane's PTY to match its current tiled rectangle. The FOCUSED
    /// pane drives `self.cols/self.rows` (so the single-pane render path and all
    /// cell math keep using the focused pane's grid); the others are sized to
    /// their own rects directly. A no-op (single-pane handling) when not split.
    pub(crate) fn resize_panes(&mut self) {
        let Some(area) = self.content_area() else { return };
        // Collect rects first to drop the immutable `self` borrow before mutating.
        let rects: Vec<(usize, pane::Rect)> = match self.panes.as_ref() {
            Some(g) => g.layout.rects(area, Self::PANE_GAP),
            None => return,
        };
        let Some(r) = self.renderer.as_ref() else { return };
        let m = r.cell_metrics();
        let (cw, ch) = (m.width.round() as u16, m.height.round() as u16);
        let focused = self.panes.as_ref().unwrap().layout.focused();
        let hdr_h = if self.config.pane_headers { Self::PANE_HEADER_H } else { 0 };
        for (id, rect) in rects {
            // Mirror render_split's body rect: the cell grid starts below the
            // (optional) pane header, so the PTY must be sized to that body height.
            let body = pane::Rect {
                x: rect.x,
                y: rect.y + hdr_h,
                w: rect.w,
                h: (rect.h - hdr_h).max(0),
            };
            let (cols, rows) = self.pane_grid(body);
            if id == focused {
                if let Some(pty) = &self.pty {
                    pty.resize(cols, rows, cw, ch);
                }
                // The focused pane is the one the single-pane paths read from.
                self.cols = cols;
                self.rows = rows;
            } else if let Some(pty) = self.panes.as_ref().unwrap().others.get(&id) {
                pty.resize(cols, rows, cw, ch);
            }
        }
    }

    /// Split the focused pane in `dir`, spawning a fresh shell for the new pane
    /// and focusing it. Promotes a single-pane tab into a `PaneGroup` on the
    /// first split. Re-points `self.pty` at the (new) focused pane.
    pub(crate) fn split_pane(&mut self, dir: pane::Dir, event_loop: &ActiveEventLoop) {
        if self.renderer.is_none() || self.pty.is_none() {
            return;
        }
        let new_id = self.next_id;
        let m = self.renderer.as_ref().unwrap().cell_metrics();
        // The new pane inherits the focused pane's cwd (from OSC 7).
        let cwd = self.active_cwd.clone();
        let pty = match Pty::spawn(
            self.proxy.clone(),
            new_id,
            self.cols,
            self.rows,
            m.width.round() as u16,
            m.height.round() as u16,
            self.config.shell.clone(),
            cwd,
            self.config.scrollback,
            &self.config.word_separator,
        ) {
            Ok(p) => p,
            Err(e) => {
                log::error!("failed to spawn pane: {e:#}");
                return;
            }
        };
        self.next_id += 1;

        // Promote to a PaneGroup whose sole leaf is the current focused pane.
        if self.panes.is_none() {
            self.panes = Some(PaneGroup {
                layout: pane::Layout::new(self.active_id),
                others: HashMap::new(),
                others_titles: HashMap::new(),
            });
        }
        // The pane currently in `self.pty` is the focused leaf; park it as an
        // "other" and make the freshly-spawned pane the new focused `self.pty`.
        let g = self.panes.as_mut().unwrap();
        let prev_focus = g.layout.focused();
        if !g.layout.split(dir, new_id) {
            // Couldn't split (shouldn't happen for a fresh id); drop the new pty.
            pty.shutdown();
            return;
        }
        if let Some(old) = self.pty.take() {
            g.others.insert(prev_focus, old);
        }
        // Seed the previous focused pane's current title so its header displays
        // immediately (OSC updates will overwrite as the shell prompts).
        g.others_titles.entry(prev_focus).or_insert_with(|| self.active_title.clone());
        // The new pane starts with the same title; it will update via OSC once
        // the shell emits one.
        g.others_titles.entry(new_id).or_default();
        self.pty = Some(pty);

        self.resize_panes();
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Move focus to the neighbouring pane in direction `m` (Alt+Arrow). Swaps
    /// `self.pty` with the newly-focused pane's parked PTY. No-op when not split.
    pub(crate) fn focus_pane(&mut self, m: pane::Move, event_loop: &ActiveEventLoop) {
        let Some(area) = self.content_area() else { return };
        let Some(g) = self.panes.as_mut() else { return };
        let prev = g.layout.focused();
        let Some(next) = g.layout.focus_move(m, area, Self::PANE_GAP) else {
            return;
        };
        if next == prev {
            return;
        }
        // Swap the previously-focused PTY out and the newly-focused one in.
        if let Some(old) = self.pty.take() {
            g.others.insert(prev, old);
        }
        if let Some(p) = g.others.remove(&next) {
            self.pty = Some(p);
        }
        // The focused pane defines the active grid dims; re-sync them.
        self.resize_panes();
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Close the focused pane. When more than one pane remains, the focused pane's
    /// shell is shut down, the layout collapses, and focus moves to the promoted
    /// sibling. When only one pane is left, falls back to closing the whole tab.
    pub(crate) fn close_pane(&mut self, event_loop: &ActiveEventLoop) {
        let n = self.panes.as_ref().map(|g| g.layout.len()).unwrap_or(1);
        if n <= 1 {
            self.close_active_tab(event_loop);
            return;
        }
        let g = self.panes.as_mut().unwrap();
        let closing = g.layout.focused();
        if !g.layout.close(closing) {
            return;
        }
        let new_focus = g.layout.focused();
        // Shut down the closed pane's shell (it was the focused `self.pty`).
        if let Some(old) = self.pty.take() {
            old.shutdown();
        }
        // Bring the promoted pane's PTY in as the new focus.
        if let Some(p) = g.others.remove(&new_focus) {
            self.pty = Some(p);
        }
        // Collapse back to single-pane if only one leaf remains.
        if g.layout.len() == 1 {
            self.panes = None;
            // The PTY now in `self.pty` is the sole pane; resize it to the full
            // content area (the single-pane resize uses self.cols/self.rows, which
            // handle_resize keeps current).
            if let Some(area) = self.content_area()
                && let Some(pty) = &self.pty
            {
                let (cols, rows) = self.pane_grid(area);
                let m = self.renderer.as_ref().unwrap().cell_metrics();
                pty.resize(cols, rows, m.width.round() as u16, m.height.round() as u16);
                self.cols = cols;
                self.rows = rows;
            }
        } else {
            self.resize_panes();
        }
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Look up a pane id in the active tab by the pointer position, returning the
    /// pane id and its rect. `None` when not split or the pointer is outside any
    /// pane. Used to route wheel/clicks to the pane under the cursor.
    pub(crate) fn pane_at(&self, x: f64, y: f64) -> Option<(usize, pane::Rect)> {
        let area = self.content_area()?;
        let g = self.panes.as_ref()?;
        let (xi, yi) = (x.round() as i32, y.round() as i32);
        g.layout
            .rects(area, Self::PANE_GAP)
            .into_iter()
            .find(|(_, r)| xi >= r.x && xi < r.x + r.w && yi >= r.y && yi < r.y + r.h)
    }

    /// Whether the active tab is currently split into more than one pane.
    pub(crate) fn is_split(&self) -> bool {
        self.panes.as_ref().is_some_and(|g| g.layout.len() > 1)
    }

    /// The pixel rect of the FOCUSED pane in the active split. `None` when not
    /// split. Used to translate pointer positions into focused-pane-local cells.
    pub(crate) fn focused_pane_rect(&self) -> Option<pane::Rect> {
        let area = self.content_area()?;
        let g = self.panes.as_ref()?;
        let f = g.layout.focused();
        g.layout
            .rects(area, Self::PANE_GAP)
            .into_iter()
            .find(|(id, _)| *id == f)
            .map(|(_, r)| r)
    }

    /// Focus the pane under the pointer (if any) when split, swapping `self.pty`.
    /// Returns true when focus actually changed (caller should repaint). No-op
    /// (false) when not split or the pointer is over the already-focused pane.
    pub(crate) fn focus_pane_at(&mut self, x: f64, y: f64, event_loop: &ActiveEventLoop) -> bool {
        let Some((id, _)) = self.pane_at(x, y) else {
            return false;
        };
        let Some(g) = self.panes.as_mut() else { return false };
        let prev = g.layout.focused();
        if id == prev {
            return false;
        }
        if !g.layout.focus(id) {
            return false;
        }
        if let Some(old) = self.pty.take() {
            g.others.insert(prev, old);
        }
        if let Some(p) = g.others.remove(&id) {
            self.pty = Some(p);
        }
        self.resize_panes();
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
        true
    }

    /// Pixel tolerance around a 1px gutter for hit-testing / cursor feedback. The
    /// drawn divider stays crisp at `PANE_GAP`; this widens only the grab zone so a
    /// ~9px band (±4px) is draggable, matching the "thin line, fat hitbox" pattern.
    pub(crate) const GUTTER_TOL: i32 = 4;

    /// Minimum pane extents (px) enforced while dragging a gutter, so a drag can
    /// never crush a pane below a usable size.
    pub(crate) const PANE_MIN_PX: i32 = 120;

    /// Hit-test the resize gutters of the active split at pointer `(x, y)`,
    /// returning the handle under it (within [`GUTTER_TOL`]). `None` when not split
    /// or off any gutter.
    pub(crate) fn gutter_at(&self, x: f64, y: f64) -> Option<pane::SplitHandle> {
        if !self.is_split() {
            return None;
        }
        let area = self.content_area()?;
        let g = self.panes.as_ref()?;
        g.layout
            .split_at(area, Self::PANE_GAP, x.round() as i32, y.round() as i32, Self::GUTTER_TOL)
    }

    /// Hit-test the pane headers in the current split. Returns `(pane_id, in_menu_btn)`
    /// when `(x, y)` is inside a pane's header strip. `in_menu_btn` is `true` when
    /// the pointer is in the right-edge ⋮ button hit-zone. `None` when not split,
    /// or when the pointer is outside all header strips.
    pub(crate) fn pane_header_at(&self, x: f64, y: f64) -> Option<(usize, bool)> {
        if !self.is_split() || !self.config.pane_headers {
            return None;
        }
        let area = self.content_area()?;
        let g = self.panes.as_ref()?;
        let rects = g.layout.rects(area, Self::PANE_GAP);
        let (xi, yi) = (x as f32, y as f32);
        let hdr_h = Self::PANE_HEADER_H as f32;
        for (id, r) in rects {
            let rx = r.x as f32;
            let ry = r.y as f32;
            let rw = r.w as f32;
            // Is the point inside the header band?
            if xi >= rx && xi < rx + rw && yi >= ry && yi < ry + hdr_h {
                // Is it in the ⋮ button (right-edge square)?
                let btn_x = rx + rw - hdr_h; // menu_btn_w == hdr_h
                let in_menu = xi >= btn_x;
                return Some((id, in_menu));
            }
        }
        None
    }

    /// Handle a left-click on a pane header. Returns `true` if the click was
    /// consumed (so the caller should skip further mouse processing).
    pub(crate) fn pane_header_click(&mut self, x: f64, y: f64, event_loop: &ActiveEventLoop) -> bool {
        let Some((id, in_menu_btn)) = self.pane_header_at(x, y) else {
            return false;
        };
        if in_menu_btn {
            // Toggle the ⋮ pane menu for this pane.
            if self.pane_menu_open == Some(id) {
                self.pane_menu_open = None;
            } else {
                self.pane_menu_open = Some(id);
                self.pane_menu_sel = 0;
                // Focus the clicked pane first so menu actions target it.
                self.focus_pane_at(x, y, event_loop);
            }
            self.mark_dirty(event_loop);
        } else {
            // Click on the header body: just focus the pane.
            self.pane_menu_open = None;
            self.focus_pane_at(x, y, event_loop);
        }
        true
    }

    /// Hit-test the open pane ⋮ dropdown. Returns the index of the hit item or
    /// `None`. Mirrors the layout in `paint_pane_menu`.
    pub(crate) fn pane_menu_hit_test(&self, x: f64, y: f64) -> Option<usize> {
        let open_pane = self.pane_menu_open?;
        let area = self.content_area()?;
        let g = self.panes.as_ref()?;
        let rects = g.layout.rects(area, Self::PANE_GAP);
        let (id, r) = rects.into_iter().find(|(id, _)| *id == open_pane)?;
        let _ = id;
        let m = self.renderer.as_ref()?.cell_metrics();
        let hdr_h = Self::PANE_HEADER_H as f32;
        let menu_btn_w = hdr_h;
        let ax = r.x as f32 + r.w as f32 - menu_btn_w;
        let ay = r.y as f32 + hdr_h;
        let max_label = Self::PANE_MENU_ITEMS.iter().map(|s| s.len()).max().unwrap_or(4) as f32;
        let panel_w = (max_label * m.width + 24.0).ceil();
        let row_h = (m.height + 6.0).ceil();
        let xi = x as f32;
        let yi = y as f32;
        for (i, _) in Self::PANE_MENU_ITEMS.iter().enumerate() {
            let row_y = ay + 2.0 + i as f32 * row_h;
            if gui::hit(gui::Rect::new(ax, row_y, panel_w, row_h), xi, yi) {
                return Some(i);
            }
        }
        None
    }

    /// Invoke the selected pane-menu action (0 = Split V, 1 = Split H, 2 = Close).
    pub(crate) fn invoke_pane_menu_action(&mut self, idx: usize, event_loop: &ActiveEventLoop) {
        self.pane_menu_open = None;
        match idx {
            0 => self.split_pane(pane::Dir::Vertical, event_loop),
            1 => self.split_pane(pane::Dir::Horizontal, event_loop),
            _ => self.close_pane(event_loop),
        }
        self.mark_dirty(event_loop);
    }

    /// While `dragging_gutter` is held, map the pointer to a new ratio for that
    /// divider and re-tile. The ratio is clamped so neither side falls below
    /// [`PANE_MIN_PX`]. Returns true when the layout changed (caller repaints).
    pub(crate) fn drag_gutter_to(&mut self, x: f64, y: f64) -> bool {
        let Some(handle) = self.dragging_gutter.clone() else {
            return false;
        };
        if handle.axis_len <= 0 {
            return false;
        }
        let pointer = match handle.dir {
            pane::Dir::Vertical => x.round() as i32,
            pane::Dir::Horizontal => y.round() as i32,
        };
        let raw = (pointer - handle.axis_start) as f32 / handle.axis_len as f32;
        // Clamp in ratio space so both children keep at least PANE_MIN_PX.
        let min_r = Self::PANE_MIN_PX as f32 / handle.axis_len as f32;
        let lo = min_r;
        let hi = 1.0 - min_r;
        let ratio = if lo <= hi { raw.clamp(lo, hi) } else { 0.5 };
        let Some(g) = self.panes.as_mut() else { return false };
        if g.layout.set_ratio(&handle.path, ratio) {
            self.resize_panes();
            self.force_full_redraw = true;
            true
        } else {
            false
        }
    }

    /// Set (or clear) the OS pointer cursor to a resize arrow for a gutter handle.
    pub(crate) fn apply_gutter_cursor(&self, handle: Option<&pane::SplitHandle>) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        use winit::window::CursorIcon;
        let icon = match handle.map(|h| h.dir) {
            Some(pane::Dir::Vertical) => CursorIcon::ColResize,
            Some(pane::Dir::Horizontal) => CursorIcon::RowResize,
            None => CursorIcon::Default,
        };
        window.set_cursor(icon);
    }

    /// Find a PTY by its pane/pty id anywhere it might live: the active focused
    /// pane, a non-focused pane of the active tab, or any pane (focused or not) of
    /// a background tab. Used to route PTY-keyed events (VT replies, etc.) to the
    /// correct pane regardless of which tab or split it belongs to.
    pub(crate) fn pty_by_id(&self, id: usize) -> Option<&Pty> {
        if id == self.active_id {
            return self.pty.as_ref();
        }
        if let Some(g) = self.panes.as_ref()
            && let Some(p) = g.others.get(&id)
        {
            return Some(p);
        }
        for s in &self.background {
            if s.id == id {
                return Some(&s.pty);
            }
            if let Some(g) = s.panes.as_ref()
                && let Some(p) = g.others.get(&id)
            {
                return Some(p);
            }
        }
        None
    }

    /// Whether `id` names a pane (focused or not) of the ACTIVE tab — i.e. one
    /// whose output is currently visible and should trigger a repaint.
    pub(crate) fn id_in_active_tab(&self, id: usize) -> bool {
        id == self.active_id || self.panes.as_ref().is_some_and(|g| g.others.contains_key(&id))
    }

    /// The display position (in `tab_order`) of the tab that owns pane `id`, where
    /// the tab is identified by its FOCUSED pane id (== the tab's stable id). A
    /// non-focused pane resolves to its owning tab. `None` if unknown.
    pub(crate) fn tab_pos_of_pane(&self, id: usize) -> Option<usize> {
        // Active tab.
        if self.id_in_active_tab(id) {
            return Some(self.active_pos());
        }
        // Background tabs: the tab id is the focused pane id; a non-focused pane is
        // found in that session's group.
        for s in &self.background {
            let owns = s.id == id || s.panes.as_ref().is_some_and(|g| g.others.contains_key(&id));
            if owns {
                return self.tab_order.iter().position(|&t| t == s.id);
            }
        }
        None
    }

}
