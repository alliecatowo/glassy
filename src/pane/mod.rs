//! Tiling layout engine — a pure binary-tree partitioning of a rectangle, in
//! the spirit of a tiling window manager. No winit/wgpu dependencies: this is
//! geometry + tree only. The running app supplies fresh leaf ids and consumes
//! the computed per-leaf pixel rectangles; everything here is unit-testable.
//!
//! Staged ahead of UI wiring: the engine is complete and unit-tested, but the
//! app doesn't drive it yet, so silence dead-code noise until it's hooked up.
#![allow(dead_code)]

mod layout;
mod tree;
mod zoom;

pub use layout::Layout;
pub use tree::{LayoutDesc, NodeDesc};
pub use zoom::Zoom;

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

/// A named split-tree shape the "cycle layout preset" action rebuilds the
/// active tab's tree into, preserving the current DFS pane order (leaf ids are
/// reassigned to the SAME set of leaves() in the SAME left-to-right order; no
/// pane is spawned, closed, or reordered — only the tree shape/ratios change).
/// [`Layout::cycle_preset`] steps through these in this declared order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LayoutPreset {
    /// All panes stacked in even horizontal bands (top to bottom).
    Rows,
    /// All panes side by side in even vertical columns (left to right).
    Columns,
    /// One "main" pane (the first leaf) takes the left half; the rest stack in
    /// even rows on the right (tmux's `main-vertical` layout).
    MainVertical,
    /// A roughly square grid: panes are grouped into `ceil(sqrt(n))` columns,
    /// each column stacking its share of panes in even rows.
    Grid,
}

impl LayoutPreset {
    /// The fixed cycle order `cycle_preset` steps through.
    const ORDER: [LayoutPreset; 4] = [
        LayoutPreset::Rows,
        LayoutPreset::Columns,
        LayoutPreset::MainVertical,
        LayoutPreset::Grid,
    ];

    /// The preset that follows this one in the cycle (wraps around).
    fn next(self) -> LayoutPreset {
        let i = Self::ORDER.iter().position(|&p| p == self).unwrap_or(0);
        Self::ORDER[(i + 1) % Self::ORDER.len()]
    }
}

/// Integer pixel rectangle in surface space.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
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
        assert_eq!(
            r[0],
            (
                1,
                Rect {
                    x: 0,
                    y: 0,
                    w: 500,
                    h: 600
                }
            )
        );
        assert_eq!(
            r[1],
            (
                2,
                Rect {
                    x: 500,
                    y: 0,
                    w: 500,
                    h: 600
                }
            )
        );
        // Together they cover the whole width.
        assert_eq!(r[0].1.w + r[1].1.w, AREA.w);
    }

    #[test]
    fn horizontal_split_no_gap_tiles_exactly() {
        let mut l = Layout::new(1);
        assert!(l.split(Dir::Horizontal, 2));
        let r = l.rects(AREA, 0);
        assert_eq!(
            r[0],
            (
                1,
                Rect {
                    x: 0,
                    y: 0,
                    w: 1000,
                    h: 300
                }
            )
        );
        assert_eq!(
            r[1],
            (
                2,
                Rect {
                    x: 0,
                    y: 300,
                    w: 1000,
                    h: 300
                }
            )
        );
        assert_eq!(r[0].1.h + r[1].1.h, AREA.h);
    }

    #[test]
    fn gap_is_reserved_between_children() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        let gap = 10;
        let r = l.rects(AREA, gap);
        // usable = 990, first = 495, second = 495, gap of 10 between.
        assert_eq!(
            r[0].1,
            Rect {
                x: 0,
                y: 0,
                w: 495,
                h: 600
            }
        );
        assert_eq!(
            r[1].1,
            Rect {
                x: 505,
                y: 0,
                w: 495,
                h: 600
            }
        );
        // The second starts exactly `gap` px after the first ends.
        assert_eq!(r[1].1.x - (r[0].1.x + r[0].1.w), gap);
        // Total consumed width == area width.
        assert_eq!(r[0].1.w + gap + r[1].1.w, AREA.w);
    }

    #[test]
    fn odd_extent_rounds_and_remainder_goes_to_second() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        let area = Rect {
            x: 0,
            y: 0,
            w: 1001,
            h: 100,
        };
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
        assert_eq!(
            map[&1],
            Rect {
                x: 0,
                y: 0,
                w: 500,
                h: 600
            }
        );
        assert_eq!(
            map[&2],
            Rect {
                x: 500,
                y: 0,
                w: 500,
                h: 300
            }
        );
        assert_eq!(
            map[&3],
            Rect {
                x: 500,
                y: 300,
                w: 500,
                h: 300
            }
        );
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
        assert_eq!(
            map[&2],
            Rect {
                x: 0,
                y: 0,
                w: 1000,
                h: 300
            }
        );
        assert_eq!(
            map[&3],
            Rect {
                x: 0,
                y: 300,
                w: 1000,
                h: 300
            }
        );
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
    fn layout_desc_round_trips_with_id_remap() {
        // Build (1 | (2 / 3)), focus 3, then serialize with a session-relative
        // remap and rebuild with a fresh live-id remap; the geometry must match.
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        l.split(Dir::Horizontal, 3); // (1 | (2/3)), focus 3
        l.set_ratio(&[], 0.4);
        // Map live ids 1,2,3 -> session ids 0,1,2 (DFS order).
        let leaves = l.leaves();
        let to_sess = |live: usize| leaves.iter().position(|&x| x == live).unwrap();
        let desc = l.to_desc(&to_sess);
        assert_eq!(desc.focused, to_sess(3));
        assert_eq!(desc.leaves(), vec![0, 1, 2]);

        // Rebuild with a remap to brand-new live ids 10,11,12.
        let new_ids = [10usize, 11, 12];
        let from_sess = |sess: usize| new_ids[sess];
        let rebuilt = Layout::from_desc(&desc, &from_sess);
        assert_eq!(rebuilt.leaves(), vec![10, 11, 12]);
        assert_eq!(rebuilt.focused(), 12);
        // Same partition shape (ratios + dirs preserved).
        let orig = l.rects(AREA, 0);
        let new = rebuilt.rects(AREA, 0);
        let orig_geo: Vec<Rect> = orig.iter().map(|(_, r)| *r).collect();
        let new_geo: Vec<Rect> = new.iter().map(|(_, r)| *r).collect();
        assert_eq!(orig_geo, new_geo);
    }

    #[test]
    fn single_leaf_desc_round_trips() {
        let l = Layout::new(7);
        let desc = l.to_desc(&|_| 0);
        assert_eq!(desc.root, NodeDesc::Leaf(0));
        let rebuilt = Layout::from_desc(&desc, &|_| 42);
        assert_eq!(rebuilt.leaves(), vec![42]);
        assert_eq!(rebuilt.focused(), 42);
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

    // ---- LayoutDesc id-remap edge cases ------------------------------------

    #[test]
    fn from_desc_unknown_focused_falls_back_to_first_leaf() {
        // If the focused id is not in the rebuilt tree, focus falls to the first leaf.
        let desc = LayoutDesc {
            root: NodeDesc::Split {
                dir: Dir::Vertical,
                ratio: 0.5,
                first: Box::new(NodeDesc::Leaf(0)),
                second: Box::new(NodeDesc::Leaf(1)),
            },
            focused: 99, // does not exist
        };
        // Map 0->10, 1->11; id 99 is not in the tree.
        let rebuilt = Layout::from_desc(&desc, &|id| id + 10);
        // first_leaf of the rebuilt tree is 10.
        assert_eq!(rebuilt.focused(), 10);
    }

    #[test]
    fn to_desc_and_from_desc_with_identity_map_preserves_geometry() {
        let mut l = Layout::new(0);
        l.split(Dir::Vertical, 1);
        l.split(Dir::Horizontal, 2);
        let desc = l.to_desc(&|x| x); // identity remap
        let rebuilt = Layout::from_desc(&desc, &|x| x);
        let orig = l.rects(AREA, 0);
        let new = rebuilt.rects(AREA, 0);
        assert_eq!(orig, new);
    }

    #[test]
    fn layout_with_zero_area_produces_zero_size_rects() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        let zero_area = Rect {
            x: 0,
            y: 0,
            w: 0,
            h: 0,
        };
        let r = l.rects(zero_area, 0);
        for (_, rc) in &r {
            assert_eq!(rc.w, 0);
            assert_eq!(rc.h, 0);
        }
    }

    #[test]
    fn cross_overlap_positive_when_intervals_overlap() {
        use crate::pane::layout::cross_overlap;
        // [0, 100) and [50, 150) overlap by 50.
        assert_eq!(cross_overlap(0, 100, 50, 100), 50);
    }

    #[test]
    fn cross_overlap_zero_when_adjacent() {
        use crate::pane::layout::cross_overlap;
        // [0, 100) and [100, 200): touching but not overlapping.
        assert_eq!(cross_overlap(0, 100, 100, 100), 0);
    }

    #[test]
    fn cross_overlap_negative_when_disjoint() {
        use crate::pane::layout::cross_overlap;
        // [0, 50) and [100, 200): gap of 50.
        let v = cross_overlap(0, 50, 100, 100);
        assert!(v <= 0, "disjoint intervals must have non-positive overlap");
    }

    // ---- swap / rotate / equalize -----------------------------------------

    #[test]
    fn swap_exchanges_leaf_positions() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2), focus 2
        l.set_ratio(&[], 0.25); // first gets 25%
        // Before: leaf 1 on the (narrow) left, leaf 2 on the (wide) right.
        let before: std::collections::HashMap<usize, Rect> = l.rects(AREA, 0).into_iter().collect();
        assert!(before[&1].w < before[&2].w);
        // Swap ids: now 2 sits in the left slot, 1 in the right.
        assert!(l.swap(1, 2));
        let after: std::collections::HashMap<usize, Rect> = l.rects(AREA, 0).into_iter().collect();
        assert_eq!(after[&2].x, before[&1].x); // 2 moved to 1's slot
        assert_eq!(after[&1].x, before[&2].x); // 1 moved to 2's slot
        // Focus follows the id, which now lives where 1 used to be.
        assert_eq!(l.focused(), 2);
        // Swapping unknown ids is a no-op (false).
        assert!(!l.swap(1, 99));
        assert!(!l.swap(7, 7));
    }

    #[test]
    fn rotate_swaps_children_and_preserves_partition() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2)
        l.set_ratio(&[], 0.3);
        let before: std::collections::HashMap<usize, Rect> = l.rects(AREA, 0).into_iter().collect();
        assert!(l.rotate(&[]));
        let after: std::collections::HashMap<usize, Rect> = l.rects(AREA, 0).into_iter().collect();
        // The two leaves traded slots; geometry of each SLOT is unchanged.
        assert_eq!(after[&2].x, before[&1].x);
        assert_eq!(after[&2].w, before[&1].w);
        assert_eq!(after[&1].x, before[&2].x);
        assert_eq!(after[&1].w, before[&2].w);
        // A leaf path can't be rotated.
        assert!(!l.rotate(&[false]));
    }

    #[test]
    fn rotate_focused_uses_parent_split() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2), focus 2
        l.split(Dir::Horizontal, 3); // (1 | (2 / 3)), focus 3
        // 3's parent split is path [true]; rotating it swaps 2 and 3.
        assert_eq!(l.parent_path_of(3), Some(vec![true]));
        let before: std::collections::HashMap<usize, Rect> = l.rects(AREA, 0).into_iter().collect();
        assert!(l.rotate_focused());
        let after: std::collections::HashMap<usize, Rect> = l.rects(AREA, 0).into_iter().collect();
        assert_eq!(after[&3].y, before[&2].y);
        assert_eq!(after[&2].y, before[&3].y);
    }

    #[test]
    fn rotate_focused_sole_leaf_is_noop() {
        let mut l = Layout::new(1);
        assert!(!l.rotate_focused());
        assert_eq!(l.parent_path_of(1), None);
    }

    #[test]
    fn reshape_applies_saved_shape_to_live_panes() {
        // Save a 2-pane vertical shape at ratio 0.3.
        let mut saved_src = Layout::new(1);
        saved_src.split(Dir::Vertical, 2);
        saved_src.set_ratio(&[], 0.3);
        let leaves = saved_src.leaves();
        let to_sess = |live: usize| leaves.iter().position(|&x| x == live).unwrap();
        let desc = saved_src.to_desc(&to_sess);

        // A DIFFERENT live 2-pane tab (ids 10, 11) at a different ratio.
        let mut live = Layout::new(10);
        live.split(Dir::Vertical, 11);
        live.set_ratio(&[], 0.5);
        assert!(live.reshape_from_desc(&desc));
        // The saved 0.3 ratio is applied to the live panes (ids preserved by DFS).
        let r = live.rects(AREA, 0);
        assert_eq!(r[0].1.w, 300);
        assert_eq!(r[0].0, 10);
        assert_eq!(r[1].0, 11);
    }

    #[test]
    fn reshape_rejects_mismatched_leaf_count() {
        let mut saved_src = Layout::new(1);
        saved_src.split(Dir::Vertical, 2);
        saved_src.split(Dir::Horizontal, 3); // 3 leaves
        let leaves = saved_src.leaves();
        let to_sess = |live: usize| leaves.iter().position(|&x| x == live).unwrap();
        let desc = saved_src.to_desc(&to_sess);

        let mut live = Layout::new(10); // 1 leaf
        assert!(!live.reshape_from_desc(&desc));
        // Untouched: still a single leaf.
        assert_eq!(live.leaves(), vec![10]);
    }

    #[test]
    fn equalize_resets_all_ratios() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2)
        l.split(Dir::Horizontal, 3); // (1 | (2 / 3))
        l.set_ratio(&[], 0.2);
        l.set_ratio(&[true], 0.8);
        l.equalize();
        let map: std::collections::HashMap<usize, Rect> = l.rects(AREA, 0).into_iter().collect();
        // Root even: left half is 500 wide.
        assert_eq!(map[&1].w, 500);
        // Inner even: each of 2 and 3 is half the right column's height.
        assert_eq!(map[&2].h, 300);
        assert_eq!(map[&3].h, 300);
    }

    // ---- layout preset cycle -----------------------------------------------

    #[test]
    fn preset_rows_stacks_evenly_preserving_order() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2); // (1 | 2)
        l.split(Dir::Horizontal, 3); // (1 | (2 / 3)) — arbitrary starting shape
        assert!(l.apply_preset(LayoutPreset::Rows));
        // DFS pane order is preserved: leaves() still reads 1, 2, 3.
        assert_eq!(l.leaves(), vec![1, 2, 3]);
        let map: std::collections::HashMap<usize, Rect> = l.rects(AREA, 0).into_iter().collect();
        // Three even horizontal bands, top to bottom, full width each.
        assert_eq!(map[&1], Rect { x: 0, y: 0, w: 1000, h: 200 });
        assert_eq!(map[&2], Rect { x: 0, y: 200, w: 1000, h: 200 });
        assert_eq!(map[&3], Rect { x: 0, y: 400, w: 1000, h: 200 });
        assert_eq!(l.classify_preset(), Some(LayoutPreset::Rows));
    }

    #[test]
    fn preset_columns_splits_evenly_side_by_side() {
        let mut l = Layout::new(1);
        l.split(Dir::Horizontal, 2);
        l.split(Dir::Horizontal, 3);
        assert!(l.apply_preset(LayoutPreset::Columns));
        assert_eq!(l.leaves(), vec![1, 2, 3]);
        let map: std::collections::HashMap<usize, Rect> = l.rects(AREA, 0).into_iter().collect();
        assert_eq!(map[&1], Rect { x: 0, y: 0, w: 333, h: 600 });
        assert_eq!(map[&2], Rect { x: 333, y: 0, w: 334, h: 600 });
        assert_eq!(map[&3], Rect { x: 667, y: 0, w: 333, h: 600 });
        assert_eq!(l.classify_preset(), Some(LayoutPreset::Columns));
    }

    #[test]
    fn preset_main_vertical_gives_first_leaf_the_main_slot() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        l.split(Dir::Vertical, 3);
        assert!(l.apply_preset(LayoutPreset::MainVertical));
        assert_eq!(l.leaves(), vec![1, 2, 3]);
        let map: std::collections::HashMap<usize, Rect> = l.rects(AREA, 0).into_iter().collect();
        // Leaf 1 is the main pane: full height, left half.
        assert_eq!(map[&1], Rect { x: 0, y: 0, w: 500, h: 600 });
        // 2 and 3 stack evenly in the right column.
        assert_eq!(map[&2].x, 500);
        assert_eq!(map[&3].x, 500);
        assert_eq!(map[&2].h, 300);
        assert_eq!(map[&3].h, 300);
        assert_eq!(l.classify_preset(), Some(LayoutPreset::MainVertical));
    }

    #[test]
    fn preset_grid_arranges_four_panes_two_by_two() {
        let mut l = Layout::new(1);
        l.split(Dir::Vertical, 2);
        l.split(Dir::Horizontal, 3);
        l.split(Dir::Horizontal, 4);
        assert!(l.apply_preset(LayoutPreset::Grid));
        assert_eq!(l.leaves(), vec![1, 2, 3, 4]);
        let map: std::collections::HashMap<usize, Rect> = l.rects(AREA, 0).into_iter().collect();
        // ceil(sqrt(4)) = 2 columns of 2 rows each: every tile is a quarter.
        for id in [1, 2, 3, 4] {
            assert_eq!(map[&id].w, 500);
            assert_eq!(map[&id].h, 300);
        }
        assert_eq!(l.classify_preset(), Some(LayoutPreset::Grid));
    }

    #[test]
    fn cycle_preset_steps_through_fixed_order_and_wraps() {
        let mut l = Layout::new(1);
        // Outer Horizontal / inner Vertical matches none of the four presets
        // (each of Rows/Columns/MainVertical/Grid has a fixed outer `dir` for
        // 3 leaves, and none of them is outer-Horizontal-inner-Vertical).
        l.split(Dir::Horizontal, 2);
        l.split(Dir::Vertical, 3);
        assert_eq!(l.classify_preset(), None);
        assert_eq!(l.cycle_preset(), Some(LayoutPreset::Rows));
        assert_eq!(l.cycle_preset(), Some(LayoutPreset::Columns));
        assert_eq!(l.cycle_preset(), Some(LayoutPreset::MainVertical));
        assert_eq!(l.cycle_preset(), Some(LayoutPreset::Grid));
        assert_eq!(l.cycle_preset(), Some(LayoutPreset::Rows)); // wraps around
        // Pane order and focus are untouched by any of this.
        assert_eq!(l.leaves(), vec![1, 2, 3]);
    }

    #[test]
    fn cycle_preset_sole_leaf_is_noop() {
        let mut l = Layout::new(1);
        assert_eq!(l.cycle_preset(), None);
        assert!(matches!(l.leaves().as_slice(), [1]));
    }
}
