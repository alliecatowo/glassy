//! Pane layout, splitting, focusing, and gutter interaction.

use super::*;

/// Pane header information density (config key `pane_header_style`). `Full` is
/// the original title+cwd+comm+grip+menu strip; `Compact` slims the strip down
/// to just the focus dot, pane index, and title (no cwd/comm annotation), at a
/// shorter [`App::PANE_HEADER_H_COMPACT`] height. Both styles render as an
/// overlay band (see [`App::pane_header_h`]) and never reserve PTY grid rows.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PaneHeaderStyle {
    #[default]
    Full,
    Compact,
}

impl PaneHeaderStyle {
    /// Parse a config-file value (`full` | `compact`); unrecognized strings
    /// fall back to `Full` at the call site, matching the config layer's usual
    /// forgiving-default pattern for enum-like keys.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "full" => Some(Self::Full),
            "compact" => Some(Self::Compact),
            _ => None,
        }
    }

    /// Round-trips `parse`'s output back to a config-file value. Not called yet
    /// (there's no settings-UI row for this key — see the house rule against
    /// adding one outside a dedicated UI-exposure stream — so nothing persists it
    /// via `SAVED_KEYS`), kept for API symmetry and that future stream.
    #[allow(dead_code)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Compact => "compact",
        }
    }
}

impl App {
    /// The content rectangle (surface pixels) that panes tile: the whole surface
    /// below the tab strip. Each pane is internally inset by the renderer pad (the
    /// renderer adds `pad` to every cell), so this spans edge-to-edge and the pad
    /// supplies the symmetric margin within each pane. Returns `None` before the
    /// renderer exists.
    pub(crate) fn content_area(&self) -> Option<pane::Rect> {
        let r = self.renderer.as_ref()?;
        // The content (panes/grid) begins below the GUI tab bar and ends above the
        // status bar. Both insets are in pixels; the per-pane `pad` is applied by
        // the pane sizing math independently. The strip height is 0 when hidden so
        // panes reclaim the band.
        let strip_bottom = self.effective_tab_bar_h().round() as i32;
        let status_h = if self.config.status_bar {
            STATUS_BAR_H.round() as i32
        } else {
            0
        };
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
        let Some(area) = self.content_area() else {
            return;
        };
        // Collect rects first to drop the immutable `self` borrow before mutating.
        // Zoom-aware: a zoomed pane sizes its PTY to the full content area; the
        // hidden panes keep their last tiled size (they're not in this list, so
        // they aren't resized — they snap back on unzoom via the next resize).
        let rects: Vec<(usize, pane::Rect)> = match self.panes.as_ref() {
            Some(g) => g.rects(area, Self::PANE_GAP),
            None => return,
        };
        let Some(r) = self.renderer.as_ref() else {
            return;
        };
        let m = r.cell_metrics();
        let (cw, ch) = (m.width.round() as u16, m.height.round() as u16);
        let focused = self.panes.as_ref().unwrap().layout.focused();
        for (id, rect) in rects {
            let (cols, rows) = self.pane_grid(Self::pane_body_rect(rect));
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
            self.config.cursor_style.to_cursor_shape(),
            self.config.cursor_blink,
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
                zoom: pane::Zoom::new(),
            });
        }
        // The pane currently in `self.pty` is the focused leaf; park it as an
        // "other" and make the freshly-spawned pane the new focused `self.pty`.
        let g = self.panes.as_mut().unwrap();
        // Splitting introduces a new tile, so any active zoom is stale — clear it
        // so the fresh split renders tiled rather than maximized into one pane.
        g.zoom = g.zoom.cleared();
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
        g.others_titles
            .entry(prev_focus)
            .or_insert_with(|| self.active_title.clone());
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
        let Some(area) = self.content_area() else {
            return;
        };
        let Some(g) = self.panes.as_mut() else { return };
        let prev = g.layout.focused();
        // Focus movement is computed against the TILED geometry, not the zoomed
        // (single-pane) view — otherwise there'd be no neighbour to move to. Use
        // the layout directly here; clearing zoom below restores the tiling.
        let Some(next) = g.layout.focus_move(m, area, Self::PANE_GAP) else {
            return;
        };
        if next == prev {
            return;
        }
        // Moving focus to a different pane reveals the tiling: clear zoom so the
        // newly-focused pane isn't immediately hidden behind a stale maximize.
        g.zoom = g.zoom.cleared();
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
        // Closing the (focused) pane collapses the tree; drop any zoom so the
        // promoted sibling renders at its real tiled size, not a stale maximize.
        g.zoom = g.zoom.cleared();
        let closing = g.layout.focused();
        if !g.layout.close(closing) {
            return;
        }
        let new_focus = g.layout.focused();
        // Shut down the closed pane's shell (it was the focused `self.pty`).
        if let Some(old) = self.pty.take() {
            old.shutdown();
        }
        // Bring the promoted pane's PTY in as the new focus. The newly-focused leaf
        // is always one of the non-focused panes parked in `others`, so the remove
        // must hit — guard it so a broken invariant degrades to "tab keeps its old
        // focus pane" rather than silently leaving the tab with no live PTY.
        debug_assert!(
            g.others.contains_key(&new_focus),
            "promoted pane {new_focus} must have a parked PTY in others"
        );
        match g.others.remove(&new_focus) {
            Some(p) => self.pty = Some(p),
            None => {
                log::error!(
                    "close_pane: promoted pane {new_focus} had no PTY; tab left without focus pane"
                );
            }
        }
        // If the user closed the pane whose id equals active_id (the tab's stable
        // identity — usually the original primary pane), that id now names a dead
        // pane. Re-point active_id and its tab_order slot at a surviving pane so
        // event routing (id_in_active_tab / tab_pos_of_pane) keeps finding the tab.
        // The survivor is new_focus when collapsing to single-pane; while still
        // split, active_id only needs to name SOME live pane, and new_focus is one.
        if closing == self.active_id {
            if let Some(pos) = self.tab_order.iter().position(|&id| id == self.active_id) {
                self.tab_order[pos] = new_focus;
            }
            self.active_id = new_focus;
        }
        // Collapse back to single-pane if only one leaf remains.
        if g.layout.len() == 1 {
            self.panes = None;
            // Drive the full resize path (renderer.resize + grid_for + pty.resize +
            // full-redraw + projection rebuild), exactly as a window resize does.
            // reflow_grid alone left the renderer's row storage sized for the split,
            // so the collapsed pane rendered half-width until the next real resize.
            if let Some(size) = self.window.as_ref().map(|w| w.inner_size()) {
                self.handle_resize(event_loop, size);
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
        // Zoom-aware: only the (full-area) focused pane is hit-testable while zoomed.
        g.rects(area, Self::PANE_GAP)
            .into_iter()
            .find(|(_, r)| xi >= r.x && xi < r.x + r.w && yi >= r.y && yi < r.y + r.h)
    }

    /// Whether the active tab is currently split into more than one pane.
    pub(crate) fn is_split(&self) -> bool {
        self.panes.as_ref().is_some_and(|g| g.layout.len() > 1)
    }

    /// Toggle pane zoom: maximize the focused pane to fill the content area
    /// (hiding the others), or restore the tiling if already zoomed. A no-op when
    /// the active tab is a single pane (nothing to maximize over). All geometry
    /// (render, PTY sizing, hit-testing) routes through [`PaneGroup::rects`], so
    /// flipping this one flag re-tiles everything; we then resize PTYs so the
    /// (un)maximized panes get correct grid dimensions and force a full redraw.
    pub(crate) fn toggle_zoom(&mut self, event_loop: &ActiveEventLoop) {
        let Some(g) = self.panes.as_mut() else {
            return; // single-pane tab: no PaneGroup, nothing to zoom.
        };
        let next = g.zoom.toggle(g.layout.len());
        if next == g.zoom {
            return; // unchanged (e.g. tried to zoom a sole leaf): skip the repaint.
        }
        g.zoom = next;
        // Re-size every (visible) pane's PTY to its new rect: the focused pane
        // grows to the full area on zoom and the panes snap back to their tiles on
        // unzoom. `resize_panes` reads the zoom-aware rects, so this is correct in
        // both directions (the hidden panes simply aren't in the zoomed list and
        // keep their last size until the unzoom resize restores them).
        self.resize_panes();
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// The pixel rect of the FOCUSED pane in the active split. `None` when not
    /// split. Used to translate pointer positions into focused-pane-local cells.
    pub(crate) fn focused_pane_rect(&self) -> Option<pane::Rect> {
        let area = self.content_area()?;
        let g = self.panes.as_ref()?;
        let f = g.layout.focused();
        // Zoom-aware: the focused pane spans the whole `area` while zoomed.
        g.rects(area, Self::PANE_GAP)
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
        let Some(g) = self.panes.as_mut() else {
            return false;
        };
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

    /// Floor for the [`PaneHeaderStyle::Compact`] overlay strip height (px). The
    /// live height is derived from the cell height (see [`Self::compact_header_h`])
    /// so it always fully contains a centered header glyph; this is only the lower
    /// bound used at small font sizes.
    pub(crate) const PANE_HEADER_H_COMPACT: i32 = 14;

    /// Extra vertical padding (px) added to the cell height when sizing the
    /// compact header, so the centered glyph has a hair of breathing room top and
    /// bottom rather than exactly filling the band.
    const COMPACT_HEADER_PAD: f32 = 2.0;

    /// Compact-header overlay height for a given cell height. The compact style is
    /// deliberately short, but it MUST be at least as tall as a terminal cell's
    /// glyph (`round(font_px * 1.30)` ≈ 18px at the default font) — otherwise the
    /// header glyphs, which are vertically centered within the band, get a
    /// NEGATIVE top offset and bleed up into the pane above (worse on HiDPI).
    /// Derived from the live cell height like [`tab_bar_h`], with
    /// [`Self::PANE_HEADER_H_COMPACT`] as a floor. Pure so it's unit-testable.
    pub(crate) fn compact_header_h(cell_h: f32) -> i32 {
        ((cell_h + Self::COMPACT_HEADER_PAD).ceil() as i32).max(Self::PANE_HEADER_H_COMPACT)
    }

    /// The current pane-header overlay height in px: `0` when `pane_headers` is
    /// off, else [`Self::PANE_HEADER_H`] or the cell-height-derived compact height
    /// ([`Self::compact_header_h`]) depending on `pane_header_style`. Headers are
    /// painted as an overlay band on top of the grid's own top row(s) — this
    /// height drives hit-testing (grip/menu/click zones) and the paint pass, but
    /// NEVER insets the PTY grid (see `resize_panes`, which sizes every pane to
    /// its full tiled rect).
    pub(crate) fn pane_header_h(&self) -> i32 {
        // Live cell height; a `0.0` fallback (no renderer yet) resolves compact to
        // its floor, matching the old fixed value when nothing is painted anyway.
        let cell_h = self
            .renderer
            .as_ref()
            .map(|r| r.cell_metrics().height)
            .unwrap_or(0.0);
        Self::header_h_for(
            self.config.pane_headers,
            self.config.pane_header_style,
            cell_h,
        )
    }

    /// Pure config→height resolution behind [`Self::pane_header_h`]: `0` when
    /// headers are off, else [`Self::PANE_HEADER_H`] or the cell-height-derived
    /// compact height depending on `style`. Split out from the `&self` method so
    /// the mapping is unit-testable without a renderer-backed `App`.
    pub(crate) fn header_h_for(pane_headers: bool, style: PaneHeaderStyle, cell_h: f32) -> i32 {
        if !pane_headers {
            return 0;
        }
        match style {
            PaneHeaderStyle::Full => Self::PANE_HEADER_H,
            PaneHeaderStyle::Compact => Self::compact_header_h(cell_h),
        }
    }

    /// The PTY body rect for a pane's tile: ALWAYS the full tile rect. Pane
    /// headers are an overlay band painted ON TOP of the grid's own top row(s)
    /// (see [`Self::pane_header_h`]), never a reservation — so a pane's terminal
    /// grid is NEVER shrunk by turning headers on, regardless of
    /// `pane_headers`/`pane_header_style`. Kept as its own (trivial) pure
    /// function, used by both [`Self::resize_panes`] (PTY sizing) and
    /// `render_split` (render sizing), so that invariant stays in lock-step
    /// between the two AND is directly unit-testable.
    pub(crate) fn pane_body_rect(rect: pane::Rect) -> pane::Rect {
        rect
    }

    /// Hit-test the resize gutters of the active split at pointer `(x, y)`,
    /// returning the handle under it (within [`GUTTER_TOL`]). `None` when not split
    /// or off any gutter.
    pub(crate) fn gutter_at(&self, x: f64, y: f64) -> Option<pane::SplitHandle> {
        if !self.is_split() {
            return None;
        }
        let area = self.content_area()?;
        let g = self.panes.as_ref()?;
        // No visible dividers while zoomed (one pane fills the area), so no gutters.
        if g.zoom.is_on() {
            return None;
        }
        g.layout.split_at(
            area,
            Self::PANE_GAP,
            x.round() as i32,
            y.round() as i32,
            Self::GUTTER_TOL,
        )
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
        // Zoom-aware: only the focused pane's header exists while zoomed.
        let rects = g.rects(area, Self::PANE_GAP);
        let (xi, yi) = (x as f32, y as f32);
        let hdr_h = self.pane_header_h() as f32;
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
    pub(crate) fn pane_header_click(
        &mut self,
        x: f64,
        y: f64,
        event_loop: &ActiveEventLoop,
    ) -> bool {
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
        let rects = g.rects(area, Self::PANE_GAP);
        let (id, r) = rects.into_iter().find(|(id, _)| *id == open_pane)?;
        let _ = id;
        let m = self.renderer.as_ref()?.cell_metrics();
        let hdr_h = self.pane_header_h() as f32;
        let menu_btn_w = hdr_h;
        let ax = r.x as f32 + r.w as f32 - menu_btn_w;
        let ay = r.y as f32 + hdr_h;
        let max_label = Self::PANE_MENU_ITEMS
            .iter()
            .map(|s| s.len())
            .max()
            .unwrap_or(4) as f32;
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

    /// Invoke the selected pane-menu action. Indices follow [`App::PANE_MENU_ITEMS`]:
    /// 0 = Split V, 1 = Split H, 2 = Zoom, 3 = Rotate, 4 = Equalize, 5 = Close.
    pub(crate) fn invoke_pane_menu_action(&mut self, idx: usize, event_loop: &ActiveEventLoop) {
        self.pane_menu_open = None;
        match idx {
            0 => self.split_pane(pane::Dir::Vertical, event_loop),
            1 => self.split_pane(pane::Dir::Horizontal, event_loop),
            2 => self.toggle_zoom(event_loop),
            3 => self.rotate_panes(event_loop),
            4 => self.equalize_panes(event_loop),
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
        let Some(g) = self.panes.as_mut() else {
            return false;
        };
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
        if id == self.active_id || id == self.active_focused_id() {
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

    /// The id of the active tab's *focused* pane — the one whose PTY lives in
    /// `self.pty`. For a single-pane tab this equals `active_id`, but after a
    /// split the focused leaf has a freshly-allocated id that is neither
    /// `active_id` nor a key in `g.others`, so events keyed by it must resolve
    /// through here.
    pub(crate) fn active_focused_id(&self) -> usize {
        self.panes
            .as_ref()
            .map(|g| g.layout.focused())
            .unwrap_or(self.active_id)
    }

    /// Whether `id` names a pane (focused or not) of the ACTIVE tab — i.e. one
    /// whose output is currently visible and should trigger a repaint.
    pub(crate) fn id_in_active_tab(&self, id: usize) -> bool {
        id == self.active_id
            || id == self.active_focused_id()
            || self
                .panes
                .as_ref()
                .is_some_and(|g| g.others.contains_key(&id))
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

#[cfg(test)]
mod header_geometry_tests {
    use super::*;

    #[test]
    fn header_off_is_always_zero_height() {
        assert_eq!(App::header_h_for(false, PaneHeaderStyle::Full, 18.0), 0);
        assert_eq!(App::header_h_for(false, PaneHeaderStyle::Compact, 18.0), 0);
    }

    #[test]
    fn header_on_picks_style_height() {
        assert_eq!(
            App::header_h_for(true, PaneHeaderStyle::Full, 18.0),
            App::PANE_HEADER_H
        );
        // Compact derives from the live cell height (not the bare floor constant).
        assert_eq!(
            App::header_h_for(true, PaneHeaderStyle::Compact, 18.0),
            App::compact_header_h(18.0)
        );
        // Compact still stays shorter than full at the representative cell height.
        assert!(App::compact_header_h(18.0) < App::PANE_HEADER_H);
    }

    #[test]
    fn compact_header_contains_a_centered_cell_glyph() {
        // REGRESSION: the compact header was a fixed 14px while the header glyph is
        // a full cell tall (round(font_px*1.30) ≈ 18px at the default font). The
        // header must be AT LEAST as tall as the cell so the vertically-centered
        // glyph's top offset `(hdr_h - cell_h) / 2` never goes negative (which made
        // the glyph bleed up into the pane above).
        for &cell_h in &[14.0_f32, 16.0, 18.0, 24.0, 32.0] {
            let hdr_h = App::compact_header_h(cell_h);
            assert!(
                hdr_h as f32 >= cell_h,
                "compact header {hdr_h} shorter than cell {cell_h}"
            );
            let centering_offset = (hdr_h as f32 - cell_h) * 0.5;
            assert!(
                centering_offset >= 0.0,
                "negative centering offset {centering_offset} at cell_h {cell_h}"
            );
        }
        // Below the floor cell height, the compact height clamps to the floor.
        assert_eq!(App::compact_header_h(0.0), App::PANE_HEADER_H_COMPACT);
    }

    #[test]
    fn pane_body_rect_never_insets_for_a_header() {
        // The core "overlay, don't steal rows" invariant: the PTY body rect for
        // a pane's tile is ALWAYS the full tile rect, regardless of whether a
        // header would be painted on top of it. A future edit that reintroduces
        // a header inset (`y += hdr_h, h -= hdr_h`) here would fail this test.
        let full = pane::Rect {
            x: 3,
            y: 5,
            w: 200,
            h: 100,
        };
        assert_eq!(App::pane_body_rect(full), full);
        // Even a tiny pane (smaller than a header would be) is untouched.
        let tiny = pane::Rect {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
        };
        assert_eq!(App::pane_body_rect(tiny), tiny);
    }
}
