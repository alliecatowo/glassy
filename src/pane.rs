//! Tiling layout engine — a pure binary-tree partitioning of a rectangle, in
//! the spirit of a tiling window manager. No winit/wgpu dependencies: this is
//! geometry + tree only. The running app supplies fresh leaf ids and consumes
//! the computed per-leaf pixel rectangles; everything here is unit-testable.
//!
//! Staged ahead of UI wiring: the engine is complete and unit-tested, but the
//! app doesn't drive it yet, so silence dead-code noise until it's hooked up.
#![allow(dead_code)]

/// Direction a split divides space. `Vertical` is a left|right divider (the two
/// children sit side by side); `Horizontal` is a top/bottom divider (children
/// stack). This matches "drag a vertical bar to resize horizontally".
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dir {
    Horizontal,
    Vertical,
}

/// A focus-movement direction in screen space.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Move {
    Up,
    Down,
    Left,
    Right,
}

/// Integer pixel rectangle in surface space.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// One node of the layout tree. A `Leaf` holds an opaque caller-owned id; a
/// `Split` divides its area between `first` (left/top) and `second`
/// (right/bottom), with `ratio` the fraction of the usable extent given to
/// `first`.
pub enum Node {
    Leaf(usize),
    Split {
        dir: Dir,
        ratio: f32,
        first: Box<Node>,
        second: Box<Node>,
    },
}

/// A resize handle: the divider of one `Split` node, located by the `path` of
/// first(false)/second(true) descents from the root. `dir` is the split
/// direction (a `Vertical` split has a left|right divider dragged horizontally;
/// a `Horizontal` split stacks and is dragged vertically). `axis_start`/
/// `axis_len` are the usable extent of the divider's primary axis (x for
/// vertical, y for horizontal) so the app can map a pointer position back to a
/// ratio: `ratio = (pointer_axis - axis_start) / axis_len`.
#[derive(Clone, PartialEq, Debug)]
pub struct SplitHandle {
    pub path: Vec<bool>,
    pub dir: Dir,
    pub axis_start: i32,
    pub axis_len: i32,
}

/// The full layout: a tree plus the currently focused leaf id.
pub struct Layout {
    root: Node,
    focused: usize,
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
                    if (key.1, key.2) < (b.1, b.2) {
                        key
                    } else {
                        b
                    }
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
    pub fn split_at(&self, area: Rect, gap: i32, px: i32, py: i32, tol: i32) -> Option<SplitHandle> {
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
            let Node::Split { dir, ratio, first, second } = node else {
                return None;
            };
            let ratio = ratio.clamp(0.0, 1.0);
            match dir {
                Dir::Vertical => {
                    let usable = (area.w - gap).max(0);
                    let fw = ((usable as f32 * ratio).round() as i32).clamp(0, usable);
                    if go_second {
                        area = Rect { x: area.x + fw + gap, y: area.y, w: usable - fw, h: area.h };
                        node = second;
                    } else {
                        area = Rect { x: area.x, y: area.y, w: fw, h: area.h };
                        node = first;
                    }
                }
                Dir::Horizontal => {
                    let usable = (area.h - gap).max(0);
                    let fh = ((usable as f32 * ratio).round() as i32).clamp(0, usable);
                    if go_second {
                        area = Rect { x: area.x, y: area.y + fh + gap, w: area.w, h: usable - fh };
                        node = second;
                    } else {
                        area = Rect { x: area.x, y: area.y, w: area.w, h: fh };
                        node = first;
                    }
                }
            }
        }
        match node {
            Node::Split { dir, ratio, .. } => {
                let ratio = ratio.clamp(0.0, 1.0);
                match dir {
                    Dir::Vertical => {
                        let usable = (area.w - gap).max(0);
                        let fw = ((usable as f32 * ratio).round() as i32).clamp(0, usable);
                        Some(area.x + fw)
                    }
                    Dir::Horizontal => {
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
}

/// Overlap of two 1-D intervals [a0, a0+al) and [b0, b0+bl). Negative/zero means
/// no overlap.
fn cross_overlap(a0: i32, al: i32, b0: i32, bl: i32) -> i32 {
    (a0 + al).min(b0 + bl) - a0.max(b0)
}

impl Node {
    fn contains(&self, id: usize) -> bool {
        match self {
            Node::Leaf(l) => *l == id,
            Node::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    fn collect_leaves(&self, out: &mut Vec<usize>) {
        match self {
            Node::Leaf(l) => out.push(*l),
            Node::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }

    fn count_leaves(&self, n: &mut usize) {
        match self {
            Node::Leaf(_) => *n += 1,
            Node::Split { first, second, .. } => {
                first.count_leaves(n);
                second.count_leaves(n);
            }
        }
    }

    /// The first (leftmost/topmost) leaf id of this subtree.
    fn first_leaf(&self) -> usize {
        match self {
            Node::Leaf(l) => *l,
            Node::Split { first, .. } => first.first_leaf(),
        }
    }

    /// Replace `Leaf(target)` with a fresh split. Returns true if found.
    fn split_leaf(&mut self, target: usize, dir: Dir, new: usize) -> bool {
        match self {
            Node::Leaf(l) if *l == target => {
                let old = *l;
                *self = Node::Split {
                    dir,
                    ratio: 0.5,
                    first: Box::new(Node::Leaf(old)),
                    second: Box::new(Node::Leaf(new)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { first, second, .. } => {
                first.split_leaf(target, dir, new) || second.split_leaf(target, dir, new)
            }
        }
    }

    /// Remove `target`, collapsing the parent split into the surviving sibling.
    /// Returns `Some(first_leaf_of_sibling)` when a collapse happened (used to
    /// repoint focus), or `None` if `target` was not found directly under a
    /// split reachable from here.
    fn close_leaf(&mut self, target: usize) -> Option<usize> {
        // If either direct child is the target leaf, collapse to the sibling.
        if let Node::Split { first, second, .. } = self {
            let first_is_target = matches!(**first, Node::Leaf(l) if l == target);
            let second_is_target = matches!(**second, Node::Leaf(l) if l == target);
            if first_is_target {
                let sibling = std::mem::replace(second.as_mut(), Node::Leaf(0));
                let focus = sibling.first_leaf();
                *self = sibling;
                return Some(focus);
            }
            if second_is_target {
                let sibling = std::mem::replace(first.as_mut(), Node::Leaf(0));
                let focus = sibling.first_leaf();
                *self = sibling;
                return Some(focus);
            }
            // Otherwise recurse into whichever subtree holds the target.
            if first.contains(target) {
                return first.close_leaf(target);
            }
            if second.contains(target) {
                return second.close_leaf(target);
            }
        }
        None
    }

    /// Mirror of [`layout`] that, instead of collecting leaf rects, hit-tests the
    /// divider bands and returns the matching [`SplitHandle`]. `path` accumulates
    /// the first(false)/second(true) descents taken to reach the current node.
    fn split_at(
        &self,
        area: Rect,
        gap: i32,
        px: i32,
        py: i32,
        tol: i32,
        path: &mut Vec<bool>,
    ) -> Option<SplitHandle> {
        let Node::Split {
            dir,
            ratio,
            first,
            second,
        } = self
        else {
            return None;
        };
        let ratio = ratio.clamp(0.0, 1.0);
        match dir {
            Dir::Vertical => {
                let usable = (area.w - gap).max(0);
                let fw = ((usable as f32 * ratio).round() as i32).clamp(0, usable);
                // Divider band on x: [area.x+fw, area.x+fw+gap), widened by `tol`.
                let div = area.x + fw;
                let within_axis = px >= div - tol && px < div + gap + tol;
                let within_cross = py >= area.y && py < area.y + area.h;
                if within_axis && within_cross {
                    return Some(SplitHandle {
                        path: path.clone(),
                        dir: Dir::Vertical,
                        axis_start: area.x,
                        axis_len: usable,
                    });
                }
                // Otherwise descend into whichever child contains the point.
                let first_rect = Rect { x: area.x, y: area.y, w: fw, h: area.h };
                let second_rect = Rect {
                    x: area.x + fw + gap,
                    y: area.y,
                    w: usable - fw,
                    h: area.h,
                };
                path.push(false);
                if let Some(h) = first.split_at(first_rect, gap, px, py, tol, path) {
                    return Some(h);
                }
                path.pop();
                path.push(true);
                if let Some(h) = second.split_at(second_rect, gap, px, py, tol, path) {
                    return Some(h);
                }
                path.pop();
                None
            }
            Dir::Horizontal => {
                let usable = (area.h - gap).max(0);
                let fh = ((usable as f32 * ratio).round() as i32).clamp(0, usable);
                let div = area.y + fh;
                let within_axis = py >= div - tol && py < div + gap + tol;
                let within_cross = px >= area.x && px < area.x + area.w;
                if within_axis && within_cross {
                    return Some(SplitHandle {
                        path: path.clone(),
                        dir: Dir::Horizontal,
                        axis_start: area.y,
                        axis_len: usable,
                    });
                }
                let first_rect = Rect { x: area.x, y: area.y, w: area.w, h: fh };
                let second_rect = Rect {
                    x: area.x,
                    y: area.y + fh + gap,
                    w: area.w,
                    h: usable - fh,
                };
                path.push(false);
                if let Some(h) = first.split_at(first_rect, gap, px, py, tol, path) {
                    return Some(h);
                }
                path.pop();
                path.push(true);
                if let Some(h) = second.split_at(second_rect, gap, px, py, tol, path) {
                    return Some(h);
                }
                path.pop();
                None
            }
        }
    }

    /// Recursively partition `area`, appending `(leaf_id, rect)` for each leaf.
    fn layout(&self, area: Rect, gap: i32, out: &mut Vec<(usize, Rect)>) {
        match self {
            Node::Leaf(l) => out.push((*l, area)),
            Node::Split {
                dir,
                ratio,
                first,
                second,
            } => {
                let ratio = ratio.clamp(0.0, 1.0);
                match dir {
                    Dir::Vertical => {
                        // Side-by-side: reserve `gap` between, split the rest.
                        let usable = (area.w - gap).max(0);
                        let fw = (usable as f32 * ratio).round() as i32;
                        let fw = fw.clamp(0, usable);
                        let sw = usable - fw;
                        let first_rect = Rect {
                            x: area.x,
                            y: area.y,
                            w: fw,
                            h: area.h,
                        };
                        let second_rect = Rect {
                            x: area.x + fw + gap,
                            y: area.y,
                            w: sw,
                            h: area.h,
                        };
                        first.layout(first_rect, gap, out);
                        second.layout(second_rect, gap, out);
                    }
                    Dir::Horizontal => {
                        // Stacked: reserve `gap` between, split the rest.
                        let usable = (area.h - gap).max(0);
                        let fh = (usable as f32 * ratio).round() as i32;
                        let fh = fh.clamp(0, usable);
                        let sh = usable - fh;
                        let first_rect = Rect {
                            x: area.x,
                            y: area.y,
                            w: area.w,
                            h: fh,
                        };
                        let second_rect = Rect {
                            x: area.x,
                            y: area.y + fh + gap,
                            w: area.w,
                            h: sh,
                        };
                        first.layout(first_rect, gap, out);
                        second.layout(second_rect, gap, out);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect {
        x: 0,
        y: 0,
        w: 1000,
        h: 600,
    };

    #[test]
    fn single_leaf_fills_area() {
        let l = Layout::new(1);
        let r = l.rects(AREA, 0);
        assert_eq!(r, vec![(1, AREA)]);
        assert_eq!(l.focused(), 1);
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn vertical_split_no_gap_tiles_exactly() {
        let mut l = Layout::new(1);
        assert!(l.split(Dir::Vertical, 2));
        let r = l.rects(AREA, 0);
        // first 0..500, second 500..1000, full height, no gaps.
        assert_eq!(r[0], (1, Rect { x: 0, y: 0, w: 500, h: 600 }));
        assert_eq!(r[1], (2, Rect { x: 500, y: 0, w: 500, h: 600 }));
        // Together they cover the whole width.
        assert_eq!(r[0].1.w + r[1].1.w, AREA.w);
    }

    #[test]
    fn horizontal_split_no_gap_tiles_exactly() {
        let mut l = Layout::new(1);
        assert!(l.split(Dir::Horizontal, 2));
        let r = l.rects(AREA, 0);
        assert_eq!(r[0], (1, Rect { x: 0, y: 0, w: 1000, h: 300 }));
        assert_eq!(r[1], (2, Rect { x: 0, y: 300, w: 1000, h: 300 }));
        assert_eq!(r[0].1.h + r[1].1.h, AREA.h);
    }

    #[test]
    fn gap_is_reserved_between_children() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        let gap = 10;
        let r = l.rects(AREA, gap);
        // usable = 990, first = 495, second = 495, gap of 10 between.
        assert_eq!(r[0].1, Rect { x: 0, y: 0, w: 495, h: 600 });
        assert_eq!(r[1].1, Rect { x: 505, y: 0, w: 495, h: 600 });
        // The second starts exactly `gap` px after the first ends.
        assert_eq!(r[1].1.x - (r[0].1.x + r[0].1.w), gap);
        // Total consumed width == area width.
        assert_eq!(r[0].1.w + gap + r[1].1.w, AREA.w);
    }

    #[test]
    fn odd_extent_rounds_and_remainder_goes_to_second() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        let area = Rect { x: 0, y: 0, w: 1001, h: 100 };
        let r = l.rects(area, 0);
        // 1001 * 0.5 = 500.5 -> rounds to 501 for first, 500 for second.
        assert_eq!(r[0].1.w, 501);
        assert_eq!(r[1].1.w, 500);
        assert_eq!(r[0].1.w + r[1].1.w, 1001);
    }

    #[test]
    fn nested_split_partitions_recursively() {
        // Split vertically (1 | 2), focus is now 2; split 2 horizontally (2 / 3).
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        assert_eq!(l.focused(), 2);
        l.split(Dir::Horizontal, 3);
        assert_eq!(l.focused(), 3);

        let r = l.rects(AREA, 0);
        let map: std::collections::HashMap<usize, Rect> = r.into_iter().collect();
        assert_eq!(map[&1], Rect { x: 0, y: 0, w: 500, h: 600 });
        assert_eq!(map[&2], Rect { x: 500, y: 0, w: 500, h: 300 });
        assert_eq!(map[&3], Rect { x: 500, y: 300, w: 500, h: 300 });
    }

    #[test]
    fn leaves_lists_all_in_dfs_order() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // focus 2
        l.split(Dir::Horizontal, 3); // focus 3, splits 2 -> (2 / 3)
        assert_eq!(l.leaves(), vec![1, 2, 3]);
        assert_eq!(l.len(), 3);
    }

    #[test]
    fn split_rejects_duplicate_id() {
        let mut l = Layout::new(1);
        assert!(!l.split(Dir::Vertical, 1));
        assert_eq!(l.len(), 1);
    }

    #[test]
    fn close_collapses_parent_and_promotes_sibling() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2), focus 2
        assert!(l.close(2));
        // Sibling 1 promoted; tree is a single leaf again.
        assert_eq!(l.leaves(), vec![1]);
        assert_eq!(l.focused(), 1); // focus moved off the closed leaf
        // Geometry collapses back to the whole area.
        assert_eq!(l.rects(AREA, 10), vec![(1, AREA)]);
    }

    #[test]
    fn close_nested_promotes_subtree() {
        // (1 | (2 / 3)) — close 1, the right subtree fills the whole area.
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2), focus 2
        l.split(Dir::Horizontal, 3); // (1 | (2 / 3)), focus 3
        assert!(l.close(1));
        assert_eq!(l.leaves(), vec![2, 3]);
        let r = l.rects(AREA, 0);
        let map: std::collections::HashMap<usize, Rect> = r.into_iter().collect();
        // The (2/3) subtree now owns the full area.
        assert_eq!(map[&2], Rect { x: 0, y: 0, w: 1000, h: 300 });
        assert_eq!(map[&3], Rect { x: 0, y: 300, w: 1000, h: 300 });
    }

    #[test]
    fn close_focused_repoints_focus_into_sibling() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2), focus 2
        l.split(Dir::Horizontal, 3); // (1 | (2 / 3)), focus 3
        // Close the focused leaf 3 -> sibling 2 is promoted, focus -> 2.
        assert!(l.close(3));
        assert_eq!(l.focused(), 2);
        assert_eq!(l.leaves(), vec![1, 2]);
    }

    #[test]
    fn cannot_close_sole_leaf() {
        let mut l = Layout::new(1);
        assert!(!l.close(1));
        assert!(!l.close(99)); // unknown id
        assert_eq!(l.leaves(), vec![1]);
    }

    #[test]
    fn focus_move_left_right() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2), focus 2
        // From 2 (right), moving Left lands on 1.
        assert_eq!(l.focus_move(Move::Left, AREA, 0), Some(1));
        assert_eq!(l.focused(), 1);
        // From 1, moving Right lands on 2.
        assert_eq!(l.focus_move(Move::Right, AREA, 0), Some(2));
        assert_eq!(l.focused(), 2);
        // No pane further right.
        assert_eq!(l.focus_move(Move::Right, AREA, 0), None);
        assert_eq!(l.focused(), 2);
    }

    #[test]
    fn focus_move_up_down() {
        let mut l = Layout::new(1);
        l.split(Dir::Horizontal, 2); // (1 / 2), focus 2 (bottom)
        assert_eq!(l.focus_move(Move::Up, AREA, 0), Some(1));
        assert_eq!(l.focused(), 1);
        assert_eq!(l.focus_move(Move::Down, AREA, 0), Some(2));
        assert_eq!(l.focused(), 2);
        assert_eq!(l.focus_move(Move::Up, AREA, 0), Some(1));
    }

    #[test]
    fn focus_move_picks_overlapping_neighbour() {
        // Layout: left column = 1, right column split top/bottom = (2 / 3).
        //   (1 | (2 / 3))
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2), focus 2
        l.split(Dir::Horizontal, 3); // (1 | (2 / 3)), focus 3 (bottom-right)

        // From 1 (full-height left), moving Right: both 2 and 3 overlap; the
        // engine picks one of them (closest leading edge / max overlap). It must
        // be a right-column pane, not stay on 1.
        l.focus(1);
        let landed = l.focus_move(Move::Right, AREA, 0).unwrap();
        assert!(landed == 2 || landed == 3);

        // From 2 (top-right), Down lands on 3.
        l.focus(2);
        assert_eq!(l.focus_move(Move::Down, AREA, 0), Some(3));
        // From 3, Up lands on 2.
        assert_eq!(l.focus_move(Move::Up, AREA, 0), Some(2));
        // From 3, Left lands on 1.
        assert_eq!(l.focus_move(Move::Left, AREA, 0), Some(1));
    }

    #[test]
    fn focus_move_no_neighbour_returns_none() {
        let l_owner = Layout::new(1);
        let mut l = l_owner;
        assert_eq!(l.focus_move(Move::Left, AREA, 0), None);
        assert_eq!(l.focus_move(Move::Up, AREA, 0), None);
    }

    #[test]
    fn focus_rejects_unknown_id() {
        let mut l = Layout::new(1);
        assert!(!l.focus(42));
        assert!(l.focus(1));
    }

    #[test]
    fn split_at_hits_vertical_divider() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2) at ratio 0.5
        let gap = 4;
        // usable = 996, fw = 498, divider band on x at [498, 502).
        let h = l.split_at(AREA, gap, 500, 300, 4).expect("on divider");
        assert_eq!(h.dir, Dir::Vertical);
        assert_eq!(h.path, vec![] as Vec<bool>); // root split
        assert_eq!(h.axis_start, 0);
        assert_eq!(h.axis_len, 996);
        // Tolerance reaches just outside the raw band.
        assert!(l.split_at(AREA, gap, 495, 300, 4).is_some());
        // Far from any divider: miss.
        assert!(l.split_at(AREA, gap, 100, 300, 4).is_none());
        assert!(l.split_at(AREA, gap, 900, 300, 4).is_none());
    }

    #[test]
    fn split_at_hits_horizontal_divider() {
        let mut l = Layout::new(1);
        l.split(Dir::Horizontal, 2); // (1 / 2)
        let gap = 4;
        let h = l.split_at(AREA, gap, 500, 300, 4).expect("on divider");
        assert_eq!(h.dir, Dir::Horizontal);
        assert_eq!(h.path, vec![] as Vec<bool>);
        assert_eq!(h.axis_start, 0);
        assert_eq!(h.axis_len, 596);
        assert!(l.split_at(AREA, gap, 500, 100, 4).is_none());
    }

    #[test]
    fn split_at_inner_divider_has_path() {
        // (1 | (2 / 3)) — the inner horizontal divider lives under second of root.
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2)
        l.split(Dir::Horizontal, 3); // splits focused 2 -> (2 / 3)
        let gap = 4;
        // Right column spans x in [502, 1000); its inner divider is at y mid.
        // usable_h there = 596, fh = 298, divider y band [298, 302).
        let h = l.split_at(AREA, gap, 750, 300, 4).expect("inner divider");
        assert_eq!(h.dir, Dir::Horizontal);
        assert_eq!(h.path, vec![true]); // descend into root's second child
    }

    #[test]
    fn divider_pos_matches_drawn_dividers() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        l.split(Dir::Horizontal, 3); // (1 | (2 / 3))
        let gap = 4;
        let r = l.rects(AREA, gap);
        let map: std::collections::HashMap<usize, Rect> = r.into_iter().collect();
        // Root vertical divider = right edge of leaf 1's rect.
        let r1 = map[&1];
        assert_eq!(l.divider_pos(AREA, gap, &[]), Some(r1.x + r1.w));
        // Inner horizontal divider (path [true]) = bottom edge of leaf 2.
        let r2 = map[&2];
        assert_eq!(l.divider_pos(AREA, gap, &[true]), Some(r2.y + r2.h));
        // A leaf path returns None.
        assert_eq!(l.divider_pos(AREA, gap, &[false]), None);
    }

    #[test]
    fn set_ratio_moves_divider() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        assert!(l.set_ratio(&[], 0.25));
        let r = l.rects(AREA, 0);
        // usable 1000, fw = 250.
        assert_eq!(r[0].1.w, 250);
        assert_eq!(r[1].1.w, 750);
        // Clamps out-of-range.
        assert!(l.set_ratio(&[], 5.0));
        let r = l.rects(AREA, 0);
        assert_eq!(r[0].1.w, 1000);
        assert_eq!(r[1].1.w, 0);
    }

    #[test]
    fn set_ratio_addresses_inner_split() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2)
        l.split(Dir::Horizontal, 3); // (1 | (2 / 3))
        // Inner split is at path [true]; set its ratio.
        assert!(l.set_ratio(&[true], 0.25));
        let r = l.rects(AREA, 0);
        let map: std::collections::HashMap<usize, Rect> = r.into_iter().collect();
        // Right column h=600; first(2) gets 150, second(3) gets 450.
        assert_eq!(map[&2].h, 150);
        assert_eq!(map[&3].h, 450);
        // A leaf path is rejected.
        assert!(!l.set_ratio(&[false], 0.5)); // [false] is leaf 1
        assert!(!l.set_ratio(&[true, false, true], 0.5)); // too deep
    }

    #[test]
    fn gap_with_nested_splits_stays_within_area() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        l.split(Dir::Horizontal, 3);
        let gap = 8;
        let r = l.rects(AREA, gap);
        // Every rect must stay inside the outer area and be non-negative.
        for (_, rc) in &r {
            assert!(rc.x >= AREA.x);
            assert!(rc.y >= AREA.y);
            assert!(rc.x + rc.w <= AREA.x + AREA.w);
            assert!(rc.y + rc.h <= AREA.y + AREA.h);
            assert!(rc.w >= 0 && rc.h >= 0);
        }
    }
}
