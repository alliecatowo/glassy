//! [`Layout`] — the public tiling-layout API: split, close, focus movement,
//! rect computation, gutter hit-testing, and ratio mutation. Delegates all
//! recursive tree work to [`super::tree::Node`].

use super::tree::{LayoutDesc, Node};
use super::{Dir, Move, Rect, SplitHandle};

/// The full layout: a tree plus the currently focused leaf id.
pub struct Layout {
    pub(super) root: Node,
    pub(super) focused: usize,
}

impl Layout {
    /// A fresh layout containing a single leaf, which is focused.
    pub fn new(id: usize) -> Self {
        Layout {
            root: Node::Leaf(id),
            focused: id,
        }
    }

    /// The currently focused leaf id.
    pub fn focused(&self) -> usize {
        self.focused
    }

    /// Force the focus onto `id` if it names an existing leaf.
    pub fn focus(&mut self, id: usize) -> bool {
        if self.root.contains(id) {
            self.focused = id;
            true
        } else {
            false
        }
    }

    /// Every leaf id, in depth-first (first-then-second) order.
    pub fn leaves(&self) -> Vec<usize> {
        let mut out = Vec::new();
        self.root.collect_leaves(&mut out);
        out
    }

    /// Number of leaves.
    pub fn len(&self) -> usize {
        let mut n = 0;
        self.root.count_leaves(&mut n);
        n
    }

    pub fn is_empty(&self) -> bool {
        false // a Layout always has at least one leaf
    }

    /// Split the focused leaf in `dir`, giving the new leaf (id `new`) the
    /// second half. Focus moves to the new leaf. The split starts at 0.5.
    /// No-op returning false if `new` already exists.
    pub fn split(&mut self, dir: Dir, new: usize) -> bool {
        if self.root.contains(new) {
            return false;
        }
        let target = self.focused;
        if self.root.split_leaf(target, dir, new) {
            self.focused = new;
            true
        } else {
            false
        }
    }

    /// Close the leaf `id`, collapsing its parent split and promoting the
    /// sibling subtree in its place. Returns false if `id` is unknown or is the
    /// sole remaining leaf (a layout always keeps at least one leaf). If the
    /// closed leaf was focused, focus moves to the first leaf of the promoted
    /// sibling.
    pub fn close(&mut self, id: usize) -> bool {
        if !self.root.contains(id) {
            return false;
        }
        // Sole leaf: nothing to collapse into.
        if matches!(self.root, Node::Leaf(_)) {
            return false;
        }
        let promoted_focus = self.root.close_leaf(id);
        match promoted_focus {
            Some(new_focus) => {
                if self.focused == id {
                    self.focused = new_focus;
                }
                true
            }
            None => false,
        }
    }

    /// Move focus to the nearest leaf in direction `m`, relative to the focused
    /// leaf's rectangle. Geometry is computed from `area`/`gap` the same way
    /// `rects` does, so neighbour selection matches what the user sees. Returns
    /// the newly focused id, or `None` if there is no leaf in that direction.
    pub fn focus_move(&mut self, m: Move, area: Rect, gap: i32) -> Option<usize> {
        let rects = self.rects(area, gap);
        let cur = rects.iter().find(|(id, _)| *id == self.focused)?.1;

        // Pick the candidate whose edge lies beyond the current pane in `m`,
        // with the closest leading edge and maximal overlap on the cross axis.
        let mut best: Option<(usize, i32, i32)> = None; // (id, primary_dist, -overlap)
        for (id, r) in &rects {
            if *id == self.focused {
                continue;
            }
            let (in_dir, dist, overlap) = match m {
                Move::Left => (
                    r.x + r.w <= cur.x,
                    cur.x - (r.x + r.w),
                    cross_overlap(cur.y, cur.h, r.y, r.h),
                ),
                Move::Right => (
                    r.x >= cur.x + cur.w,
                    r.x - (cur.x + cur.w),
                    cross_overlap(cur.y, cur.h, r.y, r.h),
                ),
                Move::Up => (
                    r.y + r.h <= cur.y,
                    cur.y - (r.y + r.h),
                    cross_overlap(cur.x, cur.w, r.x, r.w),
                ),
                Move::Down => (
                    r.y >= cur.y + cur.h,
                    r.y - (cur.y + cur.h),
                    cross_overlap(cur.x, cur.w, r.x, r.w),
                ),
            };
            if !in_dir || overlap <= 0 {
                continue;
            }
            let key = (*id, dist, -overlap);
            best = Some(match best {
                None => key,
                Some(b) => {
                    // Closer in the primary axis wins; tie-break on larger overlap.
                    if (key.1, key.2) < (b.1, b.2) { key } else { b }
                }
            });
        }
        let id = best?.0;
        self.focused = id;
        Some(id)
    }

    /// Compute the integer pixel rectangle of every leaf, given the outer area
    /// and a `gap` (border/gutter) in pixels reserved at each split. Returned in
    /// depth-first order. Rounding is integer-stable: the second child takes
    /// exactly the remainder so leaves tile the area with no gaps or overlaps
    /// beyond the reserved gutters.
    pub fn rects(&self, area: Rect, gap: i32) -> Vec<(usize, Rect)> {
        let mut out = Vec::new();
        self.root.layout(area, gap, &mut out);
        out
    }

    /// Hit-test the resize gutters. Walks the same recursive partition as
    /// [`rects`], and at each split returns the [`SplitHandle`] whose divider
    /// band the point `(px, py)` falls within `tol` pixels of (measured on the
    /// split's primary axis, and inside the divider's cross-axis span). Inner
    /// (deeper) splits win over outer ones because the descent tests the matching
    /// child's subtree only. Returns `None` over no gutter.
    pub fn split_at(
        &self,
        area: Rect,
        gap: i32,
        px: i32,
        py: i32,
        tol: i32,
    ) -> Option<SplitHandle> {
        let mut path = Vec::new();
        self.root.split_at(area, gap, px, py, tol, &mut path)
    }

    /// The primary-axis pixel coordinate of the divider of the split addressed by
    /// `path` (x for a `Vertical` split, y for a `Horizontal` one) — i.e. the left
    /// edge of the gutter. Mirrors [`rects`]' partition so it matches the drawn
    /// dividers exactly. Returns `None` if `path` does not name a split node.
    pub fn divider_pos(&self, area: Rect, gap: i32, path: &[bool]) -> Option<i32> {
        let mut node = &self.root;
        let mut area = area;
        for &go_second in path {
            let Node::Split {
                dir,
                ratio,
                first,
                second,
            } = node
            else {
                return None;
            };
            let ratio = ratio.clamp(0.0, 1.0);
            match dir {
                super::Dir::Vertical => {
                    let usable = (area.w - gap).max(0);
                    let fw = ((usable as f32 * ratio).round() as i32).clamp(0, usable);
                    if go_second {
                        area = Rect {
                            x: area.x + fw + gap,
                            y: area.y,
                            w: usable - fw,
                            h: area.h,
                        };
                        node = second;
                    } else {
                        area = Rect {
                            x: area.x,
                            y: area.y,
                            w: fw,
                            h: area.h,
                        };
                        node = first;
                    }
                }
                super::Dir::Horizontal => {
                    let usable = (area.h - gap).max(0);
                    let fh = ((usable as f32 * ratio).round() as i32).clamp(0, usable);
                    if go_second {
                        area = Rect {
                            x: area.x,
                            y: area.y + fh + gap,
                            w: area.w,
                            h: usable - fh,
                        };
                        node = second;
                    } else {
                        area = Rect {
                            x: area.x,
                            y: area.y,
                            w: area.w,
                            h: fh,
                        };
                        node = first;
                    }
                }
            }
        }
        match node {
            Node::Split { dir, ratio, .. } => {
                let ratio = ratio.clamp(0.0, 1.0);
                match dir {
                    super::Dir::Vertical => {
                        let usable = (area.w - gap).max(0);
                        let fw = ((usable as f32 * ratio).round() as i32).clamp(0, usable);
                        Some(area.x + fw)
                    }
                    super::Dir::Horizontal => {
                        let usable = (area.h - gap).max(0);
                        let fh = ((usable as f32 * ratio).round() as i32).clamp(0, usable);
                        Some(area.y + fh)
                    }
                }
            }
            Node::Leaf(_) => None,
        }
    }

    /// Set the divider `ratio` (fraction given to the first child) of the split
    /// addressed by `path` (a sequence of first(false)/second(true) descents).
    /// The value is clamped to `[0, 1]`. Returns false if `path` does not name a
    /// split node.
    pub fn set_ratio(&mut self, path: &[bool], ratio: f32) -> bool {
        let mut node = &mut self.root;
        for &go_second in path {
            match node {
                Node::Split { first, second, .. } => {
                    node = if go_second { second } else { first };
                }
                Node::Leaf(_) => return false,
            }
        }
        match node {
            Node::Split { ratio: r, .. } => {
                *r = ratio.clamp(0.0, 1.0);
                true
            }
            Node::Leaf(_) => false,
        }
    }

    /// Swap the positions of panes `a` and `b` (their leaf ids trade slots in the
    /// tree; the partition shape is unchanged). Focus stays on the same id, which
    /// now sits where the other pane was. Returns true when both leaves exist.
    /// Used by pane drag-rearrange.
    pub fn swap(&mut self, a: usize, b: usize) -> bool {
        self.root.swap_leaves(a, b)
    }

    /// Rotate the split addressed by `path`: its two children trade places while
    /// the ratio is kept, so each pane takes over the other's exact slot geometry
    /// (the divider line does not move). With an empty path, rotates the root split.
    /// Returns false if `path` is not a split.
    pub fn rotate(&mut self, path: &[bool]) -> bool {
        self.root.rotate_split(path)
    }

    /// Rotate the split that is the PARENT of the focused leaf (the innermost split
    /// containing it), swapping the focused pane with its sibling subtree. A
    /// convenience for a "rotate panes" action that needs no explicit path. Returns
    /// false when the focused leaf is the sole pane (no parent split).
    pub fn rotate_focused(&mut self) -> bool {
        match self.parent_path_of(self.focused) {
            Some(path) => self.rotate(&path),
            None => false,
        }
    }

    /// The path to the split node that is the direct parent of leaf `id` (i.e. the
    /// path whose addressed node is a `Split` having `id` as a direct child).
    /// `None` if `id` is the root leaf or not found.
    pub fn parent_path_of(&self, id: usize) -> Option<Vec<bool>> {
        let mut path = Vec::new();
        Self::find_parent(&self.root, id, &mut path).then_some(path)
    }

    fn find_parent(node: &Node, id: usize, path: &mut Vec<bool>) -> bool {
        let Node::Split { first, second, .. } = node else {
            return false;
        };
        if matches!(**first, Node::Leaf(l) if l == id)
            || matches!(**second, Node::Leaf(l) if l == id)
        {
            return true;
        }
        path.push(false);
        if Self::find_parent(first, id, path) {
            return true;
        }
        path.pop();
        path.push(true);
        if Self::find_parent(second, id, path) {
            return true;
        }
        path.pop();
        false
    }

    /// Reset every split ratio to 0.5, producing an even partition across the whole
    /// tree. Focus and structure are untouched. Used by the "equalize splits" action.
    pub fn equalize(&mut self) {
        self.root.equalize();
    }

    /// Serialize the tree into a flat [`LayoutDesc`] for session persistence. Leaf
    /// ids are remapped through `id_of`, which the caller uses to translate live
    /// pane ids into stable session-relative indices (and back on restore). The
    /// focused leaf is recorded so focus is restored too.
    pub fn to_desc(&self, id_of: &impl Fn(usize) -> usize) -> LayoutDesc {
        LayoutDesc {
            root: self.root.to_desc(id_of),
            focused: id_of(self.focused),
        }
    }

    /// Re-apply a saved layout's SHAPE (split structure + ratios) onto the CURRENT
    /// set of panes, without spawning or closing anything. The current leaf ids are
    /// assigned, in depth-first order, to the saved descriptor's leaf slots (also in
    /// DFS order). Returns false (leaving the layout untouched) when the leaf counts
    /// differ — a saved 3-pane shape can't be applied to a live 2-pane tab. Focus is
    /// re-pointed to the saved focused slot's now-live id. This is how a *named*
    /// layout is restored against the live panes.
    pub fn reshape_from_desc(&mut self, desc: &LayoutDesc) -> bool {
        let live = self.leaves();
        let saved = desc.leaves();
        if live.len() != saved.len() {
            return false;
        }
        // Map each saved (session-relative) leaf id to a live id by DFS position.
        let pos_of = |sess: usize| saved.iter().position(|&s| s == sess).unwrap_or(0);
        let from_sess = |sess: usize| live[pos_of(sess)];
        self.root = Node::from_desc(&desc.root, &from_sess);
        let focused = from_sess(desc.focused);
        self.focused = if self.root.contains(focused) {
            focused
        } else {
            self.root.first_leaf()
        };
        true
    }

    /// Rebuild a layout from a [`LayoutDesc`], remapping the stored leaf ids back
    /// to live pane ids through `id_of`. The focused leaf is restored when it names
    /// an existing leaf; otherwise focus falls back to the first leaf.
    pub fn from_desc(desc: &LayoutDesc, id_of: &impl Fn(usize) -> usize) -> Self {
        let root = Node::from_desc(&desc.root, id_of);
        let focused = id_of(desc.focused);
        let mut layout = Layout { root, focused };
        if !layout.root.contains(focused) {
            layout.focused = layout.root.first_leaf();
        }
        layout
    }
}

/// Overlap of two 1-D intervals [a0, a0+al) and [b0, b0+bl). Negative/zero means
/// no overlap.
pub(super) fn cross_overlap(a0: i32, al: i32, b0: i32, bl: i32) -> i32 {
    (a0 + al).min(b0 + bl) - a0.max(b0)
}
