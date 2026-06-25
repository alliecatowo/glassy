//! Unit tests for the App module. Extracted from mod.rs to keep it under 700 lines.

use super::{
    MenuAction, StripItem, WheelAction, actions_to_entries, image_dst_size, motion_button,
    move_in_order, strip_item_at, strip_layout, wheel_action,
};
use crate::gui::MenuEntry;

#[test]
fn context_menu_entries_group_with_separators() {
    // The rich right-click menu, with no selection: Copy is present but
    // disabled; separators fall on every group boundary (clipboard | buffer |
    // layout | app).
    let items = [
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
    ];
    let entries = actions_to_entries(&items, false);
    // Item count preserved; 3 group boundaries among these → 3 separators.
    let item_count = entries
        .iter()
        .filter(|e| matches!(e, MenuEntry::Item { .. }))
        .count();
    let sep_count = entries
        .iter()
        .filter(|e| matches!(e, MenuEntry::Separator))
        .count();
    assert_eq!(item_count, items.len());
    assert_eq!(sep_count, 3);
    // First item is Copy, disabled because has_selection=false.
    match &entries[0] {
        MenuEntry::Item { label, enabled, .. } => {
            assert_eq!(*label, "Copy");
            assert!(!*enabled);
        }
        _ => panic!("first entry should be the Copy item"),
    }
    // With a selection, Copy is enabled.
    let entries_sel = actions_to_entries(&items, true);
    match &entries_sel[0] {
        MenuEntry::Item { enabled, .. } => assert!(*enabled),
        _ => panic!("first entry should be the Copy item"),
    }
}

#[test]
fn hamburger_menu_groups_layout_and_destructive() {
    // The hamburger is now (NewTab, CloseTab): Settings/Help have dedicated
    // strip icons and PaneHeaders lives in the Settings form, so neither is
    // duplicated here. NewTab is in the layout group, CloseTab in the
    // destructive group → exactly one separator at that single boundary.
    let entries = actions_to_entries(MenuAction::ALL, false);
    let item_count = entries
        .iter()
        .filter(|e| matches!(e, MenuEntry::Item { .. }))
        .count();
    let sep_count = entries
        .iter()
        .filter(|e| matches!(e, MenuEntry::Separator))
        .count();
    assert_eq!(item_count, MenuAction::ALL.len());
    assert_eq!(sep_count, 1);
    // The last entry is always the destructive Close-tab item.
    match entries.last() {
        Some(MenuEntry::Item { label, .. }) => assert_eq!(*label, "Close tab"),
        _ => panic!("hamburger must end with Close tab"),
    }
    // Settings and Help are NOT in the hamburger anymore (strip icons own them).
    let labels: Vec<&str> = entries
        .iter()
        .filter_map(|e| match e {
            MenuEntry::Item { label, .. } => Some(*label),
            _ => None,
        })
        .collect();
    assert!(!labels.contains(&"Settings"));
    assert!(!labels.contains(&"Help / keys"));
    assert!(!labels.contains(&"Pane headers"));
}

#[test]
fn settings_focus_order_matches_gui_ids_and_is_distinct() {
    use crate::gui;
    let order = super::App::settings_focus_order();
    // Each entry must equal the corresponding form widget id (build_settings).
    assert_eq!(order[0], gui::id("settings/font_size"));
    assert_eq!(order[1], gui::id("settings/opacity"));
    assert_eq!(order[2], gui::id("settings/bell"));
    assert_eq!(order[3], gui::id("settings/theme"));
    assert_eq!(order[4], gui::id("settings/font_family"));
    assert_eq!(order[5], gui::id("settings/scrollback"));
    assert_eq!(order[6], gui::id("settings/padding"));
    assert_eq!(order[7], gui::id("settings/status_bar"));
    assert_eq!(order[8], gui::id("settings/pane_headers"));
    assert_eq!(order[9], gui::id("settings/follow_system"));
    assert_eq!(order[10], gui::id("settings/ligatures"));
    assert_eq!(order[11], gui::id("settings/restore_session"));
    assert_eq!(order[12], gui::id("settings/config"));
    assert_eq!(order[13], gui::id("settings/save"));
    for (i, a) in order.iter().enumerate() {
        for b in order.iter().skip(i + 1) {
            assert_ne!(a, b);
        }
    }
}

#[test]
fn move_in_order_reorders() {
    let mut v = vec![10, 20, 30, 40];
    move_in_order(&mut v, 0, 2); // drag first to index 2
    assert_eq!(v, vec![20, 30, 10, 40]);
    move_in_order(&mut v, 3, 0); // drag last to front
    assert_eq!(v, vec![40, 20, 30, 10]);
    move_in_order(&mut v, 1, 1); // no-op
    assert_eq!(v, vec![40, 20, 30, 10]);
    move_in_order(&mut v, 9, 0); // out of range: no-op
    assert_eq!(v, vec![40, 20, 30, 10]);
}

use alacritty_terminal::term::TermMode;

use super::os_title;
const CW: f32 = 8.0; // a representative monospace cell width for layout tests
const BH: f32 = 34.0; // tab-bar height

#[test]
fn strip_hit_test_matches_layout() {
    // Two tabs (tab 1 active) + their ✕ + a + button + right-hand ?/*/#. The
    // hit-test resolves to the same items the painter draws (pixel rects).
    let segs = strip_layout(
        &[("zsh", true, false), ("vim", false, false)],
        1200.0,
        BH,
        CW,
    );
    // Probe each tab body at its center and its close box, plus the controls.
    let center = |it: StripItem| {
        segs.iter().find(|s| s.item == it).map(|s| {
            let r = s.rect;
            (r.x + r.w * 0.5, r.y + r.h * 0.5)
        })
    };
    let (tx0, ty0) = center(StripItem::Tab(0)).unwrap();
    assert_eq!(strip_item_at(&segs, tx0, ty0), Some(StripItem::Tab(0)));
    let (cx0, cy0) = center(StripItem::TabClose(0)).unwrap();
    // The close box wins over its tab body (tested in reverse order).
    assert_eq!(strip_item_at(&segs, cx0, cy0), Some(StripItem::TabClose(0)));
    let (tx1, ty1) = center(StripItem::Tab(1)).unwrap();
    assert_eq!(strip_item_at(&segs, tx1, ty1), Some(StripItem::Tab(1)));
    let (nx, ny) = center(StripItem::NewTab).unwrap();
    assert_eq!(strip_item_at(&segs, nx, ny), Some(StripItem::NewTab));
    let (hx, hy) = center(StripItem::Help).unwrap();
    assert_eq!(strip_item_at(&segs, hx, hy), Some(StripItem::Help));
    let (sx, sy) = center(StripItem::Settings).unwrap();
    assert_eq!(strip_item_at(&segs, sx, sy), Some(StripItem::Settings));
    let (mx, my) = center(StripItem::Menu).unwrap();
    assert_eq!(strip_item_at(&segs, mx, my), Some(StripItem::Menu));
    // Below the bar there are no items.
    assert_eq!(strip_item_at(&segs, tx0, BH + 5.0), None);
}

#[test]
fn single_tab_has_no_close() {
    // One tab is a single wide chip — no ✕ (closing it = quit).
    let segs = strip_layout(&[("shell", true, false)], 1000.0, BH, CW);
    assert!(segs.iter().any(|s| s.item == StripItem::Tab(0)));
    assert!(
        !segs
            .iter()
            .any(|s| matches!(s.item, StripItem::TabClose(_)))
    );
    let title = &segs
        .iter()
        .find(|s| s.item == StripItem::Tab(0))
        .unwrap()
        .label;
    assert_eq!(title, "shell");
}

#[test]
fn strip_layout_carries_titles_by_position() {
    // Each chip carries its raw title in stable display position; the numeric
    // prefix is added at paint time, so the label is just the title here.
    let segs = strip_layout(&[("a", false, false), ("b", true, false)], 1200.0, BH, CW);
    let lbl = |it| {
        segs.iter()
            .find(|s| s.item == it)
            .map(|s| s.label.clone())
            .unwrap()
    };
    assert_eq!(lbl(StripItem::Tab(0)), "a");
    assert_eq!(lbl(StripItem::Tab(1)), "b");
}

#[test]
fn os_title_is_printable_ascii_only() {
    // CJK / emoji / Nerd-Font icons / dingbats are dropped (tofu-proof).
    assert_eq!(os_title("vim  src/main.rs"), "vim src/main.rs");
    assert_eq!(os_title("✻ thinking…"), "thinking");
    assert_eq!(os_title("日本語 build"), "build");
    assert_eq!(os_title("   "), "glassy");
    assert_eq!(os_title(""), "glassy");
    // No char in the output is ever non-ASCII-graphic-or-space.
    let t = os_title("a\u{f00c}b 😀 c");
    assert!(t.chars().all(|c| c.is_ascii_graphic() || c == ' '));
}

#[test]
fn wheel_normal_screen_scrolls_scrollback() {
    assert_eq!(wheel_action(TermMode::empty()), WheelAction::Scrollback);
}

#[test]
fn image_size_native_when_unsized() {
    assert_eq!(image_dst_size(0, 0, 64, 32, 10.0, 20.0), (64.0, 32.0));
}

#[test]
fn image_size_exact_cell_box_when_both_given() {
    // 4 cols x 3 rows at a 10x20 cell box.
    assert_eq!(image_dst_size(4, 3, 64, 32, 10.0, 20.0), (40.0, 60.0));
}

#[test]
fn image_size_preserves_aspect_with_one_dim() {
    // 2:1 image, only cols=20 at cell_w=10 -> 200px wide, 100px tall (2:1).
    assert_eq!(image_dst_size(20, 0, 64, 32, 10.0, 20.0), (200.0, 100.0));
    // 2:1 image, only rows=5 at cell_h=20 -> 100px tall, 200px wide (2:1).
    assert_eq!(image_dst_size(0, 5, 64, 32, 10.0, 20.0), (200.0, 100.0));
}

#[test]
fn wheel_alt_screen_emits_arrows() {
    // bat/less/vim without mouse: alt screen, no mouse reporting.
    assert_eq!(wheel_action(TermMode::ALT_SCREEN), WheelAction::Arrows);
}

#[test]
fn wheel_mouse_mode_reports_to_app() {
    // vim with `mouse=a`, htop, claude: app owns the wheel.
    assert_eq!(
        wheel_action(TermMode::MOUSE_REPORT_CLICK),
        WheelAction::Report
    );
    assert_eq!(
        wheel_action(TermMode::ALT_SCREEN | TermMode::MOUSE_MOTION),
        WheelAction::Report
    );
}

#[test]
fn hover_reports_only_under_any_motion() {
    // Any-motion (1003) reports bare moves (id 3) -> drives hover highlight.
    assert_eq!(motion_button(TermMode::MOUSE_MOTION, None), Some(3));
    // Button-motion (1002) stays silent without a held button.
    assert_eq!(motion_button(TermMode::MOUSE_DRAG, None), None);
    // Click-only (1000) never reports motion.
    assert_eq!(motion_button(TermMode::MOUSE_REPORT_CLICK, None), None);
    assert_eq!(motion_button(TermMode::empty(), None), None);
}

#[test]
fn drag_reports_held_button_under_motion_modes() {
    assert_eq!(motion_button(TermMode::MOUSE_DRAG, Some(0)), Some(0));
    assert_eq!(motion_button(TermMode::MOUSE_MOTION, Some(2)), Some(2));
    // Click-only mode does not report drags.
    assert_eq!(motion_button(TermMode::MOUSE_REPORT_CLICK, Some(0)), None);
}
