//! Pane rearrange + layout operations: swap / rotate / equalize, named layout
//! save+restore, and the header drag-grip lifecycle (drop one pane onto another
//! to swap). Split out of `app/panes.rs` to keep that file under the size budget;
//! everything here is plain `App` methods over the active tab's [`PaneGroup`].

use super::*;

impl App {
    /// Swap two panes' positions in the active split: their leaf ids trade slots in
    /// the tiling tree, so each pane's content moves to the other's tile while every
    /// PTY stays with its own id (no PTY moves; the id→PTY map is untouched). Re-tiles
    /// the PTYs to their new rects. A no-op when not split or either id is unknown.
    /// Drives pane drag-rearrange (drop one pane onto another).
    pub(crate) fn swap_panes(&mut self, a: usize, b: usize, event_loop: &ActiveEventLoop) -> bool {
        if a == b {
            return false;
        }
        let Some(g) = self.panes.as_mut() else {
            return false;
        };
        // A swap can strand a zoom (the zoomed id now sits elsewhere); clear it so
        // the rearranged tiling is visible.
        g.zoom = g.zoom.cleared();
        if !g.layout.swap(a, b) {
            return false;
        }
        self.resize_panes();
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
        true
    }

    /// Rotate the focused pane with its split sibling (swap their positions within
    /// the innermost split). A no-op when the active tab is a single pane. Useful as
    /// a keyboard/palette alternative to dragging panes around.
    pub(crate) fn rotate_panes(&mut self, event_loop: &ActiveEventLoop) {
        let Some(g) = self.panes.as_mut() else {
            return;
        };
        g.zoom = g.zoom.cleared();
        if !g.layout.rotate_focused() {
            return;
        }
        self.resize_panes();
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Reset every split ratio in the active tab to an even 50/50 partition. A no-op
    /// when not split. The "equalize panes" action.
    pub(crate) fn equalize_panes(&mut self, event_loop: &ActiveEventLoop) {
        if !self.is_split() {
            return;
        }
        let Some(g) = self.panes.as_mut() else {
            return;
        };
        g.zoom = g.zoom.cleared();
        g.layout.equalize();
        self.resize_panes();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Save the active tab's current split SHAPE (structure + ratios, leaf ids in
    /// session-relative DFS order) under `name`, overwriting any existing entry.
    /// A no-op (returns false) when the active tab isn't split (nothing to save).
    /// Restore later with [`App::restore_layout`].
    pub(crate) fn save_layout(&mut self, name: &str) -> bool {
        let Some(g) = self.panes.as_ref() else {
            return false;
        };
        if g.layout.len() <= 1 {
            return false;
        }
        let leaves = g.layout.leaves();
        let session_id = move |live: usize| leaves.iter().position(|&l| l == live).unwrap_or(0);
        let desc = g.layout.to_desc(&session_id);
        self.named_layouts.insert(name.to_string(), desc);
        true
    }

    /// Re-apply a previously [`save_layout`]-d split shape (by `name`) onto the
    /// active tab's live panes. Requires the saved shape to have the SAME number of
    /// panes as the live tab (a 3-pane shape can't reshape a 2-pane tab); returns
    /// false otherwise or when the name is unknown / the tab isn't split. PTYs are
    /// re-tiled to their new rects; no pane is spawned or closed.
    pub(crate) fn restore_layout(&mut self, name: &str, event_loop: &ActiveEventLoop) -> bool {
        let Some(desc) = self.named_layouts.get(name).cloned() else {
            return false;
        };
        let Some(g) = self.panes.as_mut() else {
            return false;
        };
        g.zoom = g.zoom.cleared();
        if !g.layout.reshape_from_desc(&desc) {
            return false;
        }
        self.resize_panes();
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
        true
    }

    /// Names of all saved layouts, sorted for stable display in menus/palette.
    pub(crate) fn saved_layout_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.named_layouts.keys().cloned().collect();
        names.sort();
        names
    }

    /// Hit-test the drag-grip handle (left edge) of a pane header. Returns the pane
    /// id when `(x, y)` is inside a header's grip zone. `None` otherwise. Used to
    /// start a pane drag-rearrange.
    pub(crate) fn pane_grip_at(&self, x: f64, y: f64) -> Option<usize> {
        if !self.is_split() || !self.config.pane_headers {
            return None;
        }
        let area = self.content_area()?;
        let g = self.panes.as_ref()?;
        let rects = g.rects(area, Self::PANE_GAP);
        let (xi, yi) = (x as f32, y as f32);
        let hdr_h = Self::PANE_HEADER_H as f32;
        for (id, r) in rects {
            let rx = r.x as f32;
            let ry = r.y as f32;
            if xi >= rx && xi < rx + Self::PANE_GRIP_W && yi >= ry && yi < ry + hdr_h {
                return Some(id);
            }
        }
        None
    }

    /// The pane id currently being drag-rearranged, but only once the pointer has
    /// moved past [`PANE_DRAG_THRESHOLD`] from the press point (so a click on the
    /// grip still focuses without an accidental swap). `None` when not dragging or
    /// still within the threshold.
    pub(crate) fn active_pane_drag_id(&self) -> Option<usize> {
        let (src, (px, py)) = self.dragging_pane?;
        let dx = self.mouse_px.0 - px;
        let dy = self.mouse_px.1 - py;
        if (dx * dx + dy * dy).sqrt() >= Self::PANE_DRAG_THRESHOLD {
            Some(src)
        } else {
            None
        }
    }

    /// Finish a pane drag-rearrange: if the pointer is over a different pane and the
    /// threshold was crossed, swap the two panes. Always clears the drag state.
    /// Returns true when a swap happened (caller repaints). Called on button release.
    pub(crate) fn finish_pane_drag(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let Some(src) = self.active_pane_drag_id() else {
            self.dragging_pane = None;
            return false;
        };
        let target = self
            .pane_at(self.mouse_px.0, self.mouse_px.1)
            .map(|(id, _)| id)
            .filter(|&id| id != src);
        self.dragging_pane = None;
        match target {
            Some(dst) => self.swap_panes(src, dst, event_loop),
            None => false,
        }
    }
}
