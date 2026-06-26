//! Binary-tree node operations: recursion into `Node` for contains, layout,
//! split, close, and gutter hit-testing. Everything here is pure tree/geometry
//! logic with no external dependencies.

use super::{Dir, Rect, SplitHandle};

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

impl Node {
    pub(super) fn contains(&self, id: usize) -> bool {
        match self {
            Node::Leaf(l) => *l == id,
            Node::Split { first, second, .. } => first.contains(id) || second.contains(id),
        }
    }

    pub(super) fn collect_leaves(&self, out: &mut Vec<usize>) {
        match self {
            Node::Leaf(l) => out.push(*l),
            Node::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }

    pub(super) fn count_leaves(&self, n: &mut usize) {
        match self {
            Node::Leaf(_) => *n += 1,
            Node::Split { first, second, .. } => {
                first.count_leaves(n);
                second.count_leaves(n);
            }
        }
    }

    /// The first (leftmost/topmost) leaf id of this subtree.
    pub(super) fn first_leaf(&self) -> usize {
        match self {
            Node::Leaf(l) => *l,
            Node::Split { first, .. } => first.first_leaf(),
        }
    }

    /// Replace `Leaf(target)` with a fresh split. Returns true if found.
    pub(super) fn split_leaf(&mut self, target: usize, dir: Dir, new: usize) -> bool {
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
    pub(super) fn close_leaf(&mut self, target: usize) -> Option<usize> {
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
    pub(super) fn split_at(
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
                let first_rect = Rect {
                    x: area.x,
                    y: area.y,
                    w: fw,
                    h: area.h,
                };
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
    pub(super) fn layout(&self, area: Rect, gap: i32, out: &mut Vec<(usize, Rect)>) {
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

    /// Swap the ids of leaves `a` and `b` in place, leaving the tree shape
    /// (splits + ratios) untouched. Returns true when BOTH leaves were found.
    /// Used by pane drag-rearrange to move a pane onto another's slot.
    pub(super) fn swap_leaves(&mut self, a: usize, b: usize) -> bool {
        if a == b {
            return false;
        }
        let mut found_a = false;
        let mut found_b = false;
        self.swap_walk(a, b, &mut found_a, &mut found_b);
        found_a && found_b
    }

    fn swap_walk(&mut self, a: usize, b: usize, found_a: &mut bool, found_b: &mut bool) {
        match self {
            Node::Leaf(l) => {
                if *l == a {
                    *l = b;
                    *found_a = true;
                } else if *l == b {
                    *l = a;
                    *found_b = true;
                }
            }
            Node::Split { first, second, .. } => {
                first.swap_walk(a, b, found_a, found_b);
                second.swap_walk(a, b, found_a, found_b);
            }
        }
    }

    /// Rotate the split node addressed by `path`: swap its `first` and `second`
    /// subtrees while KEEPING the ratio, so each child takes over the other's exact
    /// slot geometry (the divider line does not move). Returns true when `path`
    /// names a split. An empty `path` rotates the root.
    pub(super) fn rotate_split(&mut self, path: &[bool]) -> bool {
        let mut node = self;
        for &go_second in path {
            match node {
                Node::Split { first, second, .. } => {
                    node = if go_second { second } else { first };
                }
                Node::Leaf(_) => return false,
            }
        }
        match node {
            Node::Split { first, second, .. } => {
                std::mem::swap(first, second);
                true
            }
            Node::Leaf(_) => false,
        }
    }

    /// Reset every split ratio in this subtree to 0.5 (even partition).
    pub(super) fn equalize(&mut self) {
        if let Node::Split {
            ratio,
            first,
            second,
            ..
        } = self
        {
            *ratio = 0.5;
            first.equalize();
            second.equalize();
        }
    }

    pub(super) fn to_desc(&self, id_of: &impl Fn(usize) -> usize) -> NodeDesc {
        match self {
            Node::Leaf(l) => NodeDesc::Leaf(id_of(*l)),
            Node::Split {
                dir,
                ratio,
                first,
                second,
            } => NodeDesc::Split {
                dir: *dir,
                ratio: *ratio,
                first: Box::new(first.to_desc(id_of)),
                second: Box::new(second.to_desc(id_of)),
            },
        }
    }

    pub(super) fn from_desc(desc: &NodeDesc, id_of: &impl Fn(usize) -> usize) -> Node {
        match desc {
            NodeDesc::Leaf(l) => Node::Leaf(id_of(*l)),
            NodeDesc::Split {
                dir,
                ratio,
                first,
                second,
            } => Node::Split {
                dir: *dir,
                ratio: *ratio,
                first: Box::new(Node::from_desc(first, id_of)),
                second: Box::new(Node::from_desc(second, id_of)),
            },
        }
    }
}

/// A serializable snapshot of a [`super::Layout`] tree for session persistence.
/// Leaf ids are session-relative (assigned by the caller's `id_of` remap) so they
/// round-trip independently of the live pane-id counter.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutDesc {
    pub root: NodeDesc,
    pub focused: usize,
}

/// Serializable form of one [`Node`].
#[derive(Clone, Debug, PartialEq)]
pub enum NodeDesc {
    Leaf(usize),
    Split {
        dir: Dir,
        ratio: f32,
        first: Box<NodeDesc>,
        second: Box<NodeDesc>,
    },
}

impl LayoutDesc {
    /// Every leaf id in this descriptor, in depth-first order.
    pub fn leaves(&self) -> Vec<usize> {
        let mut out = Vec::new();
        self.root.collect_leaves(&mut out);
        out
    }
}

impl NodeDesc {
    fn collect_leaves(&self, out: &mut Vec<usize>) {
        match self {
            NodeDesc::Leaf(l) => out.push(*l),
            NodeDesc::Split { first, second, .. } => {
                first.collect_leaves(out);
                second.collect_leaves(out);
            }
        }
    }
}
