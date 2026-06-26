//! Help panel: keybinding reference overlay.
//!
//! The displayed keybinding list is derived from the live `KeyMap` so custom
//! bindings always appear correctly and help text never drifts from reality.

use super::*;
use crate::config::{KeyAction, KeyMap, Platform};

// ---------------------------------------------------------------------------
// Wave 6 — Help / keybindings panel (§3.7)
// ---------------------------------------------------------------------------

/// A single row in the help panel.
#[derive(Clone, Copy, Debug)]
pub enum HelpRow<'a> {
    /// Section header: a dim label + a separator underneath.
    Section(&'a str),
    /// A keybinding row: left = chord description, right = action description.
    Binding { keys: &'a str, desc: &'a str },
    /// A blank visual gap (empty row).
    Gap,
}

/// Scroll-state owned by the App for the help panel (carried across frames).
#[derive(Clone, Copy, Debug, Default)]
pub struct HelpState {
    /// Current vertical scroll offset in pixels.
    pub scroll: f32,
}

/// Result of a [`build_help`] call this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct HelpResult {
    /// `true` if Esc / F1 / the ✕ button / the scrim was clicked — caller closes.
    pub close: bool,
    /// Updated scroll state for next frame.
    pub state: HelpState,
}

/// Draw the F1 keybindings help panel (§3.7): a full-screen scrim, a centered
/// glass panel with a header + ✕, two-column rows (left key-cap chips, right
/// description), section headers + separators, and a scrollbar when content
/// overflows the panel. Esc/F1/scrim/✕ all set `close`.
///
/// `surface` is `(width, height)` in physical px. `mouse`/`mouse_down`/`clicked`
/// are the current frame's pointer state. `click_pos` is the pointer position
/// captured at the moment the click edge was set (button release); this is used
/// for the click-outside-panel dismiss test so that pointer motion between the
/// release event and the render frame does not shift the hit-test result.
/// `opened_by_press` is the App's `overlay_opened_by_press` flag: when set, the
/// scrim/click-outside dismiss is suppressed for this paint so the gesture that
/// OPENED the panel (whose release lands outside the centered panel) cannot
/// immediately close it — the caller clears the flag once it consumes the click
/// edge. `state` is the caller-owned scroll position (mutable borrow; updated in
/// place). `keymap` is the effective live keymap; the displayed rows are derived
/// from it so custom bindings are shown.
#[allow(clippy::too_many_arguments)]
pub fn build_help(
    renderer: &mut Renderer,
    cell_w: f32,
    cell_h: f32,
    surface: (f32, f32),
    mouse: (f32, f32),
    click_pos: (f32, f32),
    mouse_down: bool,
    clicked: bool,
    opened_by_press: bool,
    pressed: &mut Option<WidgetId>,
    focused: &mut Option<WidgetId>,
    anims: &mut HashMap<WidgetId, Anim>,
    state: &mut HelpState,
    keymap: &KeyMap,
    platform: Platform,
) -> HelpResult {
    let mut result = HelpResult::default();
    let m = Metrics::new(cell_w, cell_h);

    // Full-screen scrim backdrop — hover/drag use live `mouse`; click-outside
    // uses `click_pos` (captured at release time) so motion between release and
    // render cannot shift the hit-test and accidentally dismiss the overlay.
    let scrim = Rect::new(0.0, 0.0, surface.0, surface.1);
    renderer.push_overlay_px(scrim.x, scrim.y, scrim.w, scrim.h, [0.0, 0.0, 0.0, 0.5]);

    // Panel sizing: ≈ 50 columns wide, tall enough for visible rows (capped at 80%).
    let pw = (cell_w * 50.0)
        .min(surface.0 - 2.0 * m.pad)
        .max(cell_w * 28.0);
    let max_ph = (surface.1 * 0.82).round();
    let header_h = m.row_h;
    let footer_h = 0.0; // no footer — ✕ in the header suffices

    // Lay out the help rows once to measure total content height.
    // Derived from the live keymap so custom bindings appear correctly.
    let rows = help_rows_from_keymap(keymap, platform);
    let row_h = m.row_h;
    let sep_h = 6.0;
    let section_h = (m.cell_h + 4.0).round();
    let content_h: f32 = rows
        .iter()
        .map(|r| match r {
            HelpRow::Section(_) => section_h + sep_h,
            HelpRow::Binding { .. } => row_h,
            HelpRow::Gap => (m.cell_h * 0.5).round(),
        })
        .sum();

    let scrollbar_w = (m.gap.max(6.0)).round();
    let body_h = content_h.min(max_ph - header_h - 2.0 * m.pad);
    let ph = (header_h + m.pad + body_h + m.pad).round().min(max_ph);
    let px = ((surface.0 - pw) * 0.5).round();
    let py = ((surface.1 - ph) * 0.5).round().max(m.pad);
    let panel = Rect::new(px, py, pw, ph);

    // Close if the click landed outside the panel (scrim click).
    // Use click_pos (pointer position at release time) — not live mouse — so
    // that motion after the release does not relocate the hit-test anchor.
    //
    // `opened_by_press` suppresses the scrim-close for exactly one paint: when
    // the help panel was opened by the press/release of a gesture (cog icon,
    // palette row) whose release lands OUTSIDE this centered panel, that stale
    // click edge would otherwise dismiss the panel as soon as the next repaint
    // flushes it (most often a motion-driven repaint → "motion dismisses help").
    // The caller clears the flag in the same render reset that consumes the
    // click edge, so the very next genuine outside click still dismisses.
    if clicked && !opened_by_press && !hit(panel, click_pos.0, click_pos.1) {
        result.close = true;
    }

    // Draw panel background + left accent rail.
    renderer.push_overlay_rrect_px(
        panel.x,
        panel.y,
        panel.w,
        panel.h,
        m.card_radius,
        glass_raised(),
    );
    renderer.push_overlay_px(panel.x, panel.y, 1.0, panel.h, rail());

    // Header: title + ✕ button.
    // U+2328 ⌨ may tofu on narrow font sets; use the plain label instead.
    let title = "glassy — keybindings";
    let th = ((header_h - cell_h) * 0.5).round();
    let ty = panel.y + m.pad + th;
    let mut cx = panel.x + m.pad * 1.5;
    for ch in title.chars() {
        renderer.push_overlay_glyph_px(cx.round(), ty, ch, fg());
        cx += cell_w;
    }

    // ✕ close button.
    let close_r = Rect::new(
        panel.x + panel.w - header_h,
        panel.y + m.pad * 0.5,
        header_h,
        header_h,
    );
    {
        let wid = id("help/close");
        let over = hit(close_r, mouse.0, mouse.1);
        if over {
            renderer.push_overlay_rrect_px(
                close_r.x + 2.0,
                close_r.y + 2.0,
                close_r.w - 4.0,
                close_r.h - 4.0,
                3.0,
                state_fill(glass_raised(), 1.0, false),
            );
        }
        let gx = close_r.x + (close_r.w - cell_w) * 0.5;
        let gy = close_r.y + (close_r.h - cell_h) * 0.5;
        renderer.push_overlay_glyph_px(
            gx.round(),
            gy.round(),
            '✕',
            if over { fg() } else { fg_dim() },
        );
        if clicked && over {
            result.close = true;
        }
        // Step hover animation.
        let a = anims.entry(wid).or_insert_with(|| Anim::new(0.0));
        a.target = if over { 1.0 } else { 0.0 };
    }

    // Header separator.
    let sep_y = panel.y + m.pad + header_h;
    renderer.push_overlay_px(panel.x, sep_y, panel.w, 1.0, hairline());

    // Scrollable body region.
    let body_top = sep_y + 1.0;
    let body_left = panel.x + m.pad;
    let body_w = panel.w - m.pad * 2.0 - scrollbar_w - 4.0;
    let _body_rect = Rect::new(body_left, body_top, body_w, body_h);

    // Clamp scroll.
    let max_scroll = (content_h - body_h).max(0.0);
    state.scroll = state.scroll.clamp(0.0, max_scroll);

    // Key-cap chip metrics.
    let chip_pad_x = (cell_w * 0.6).round();
    let chip_pad_y = 2.0;
    let chip_radius = 3.0;

    // Left column: chips width (fixed, based on longest key string).
    let max_key_chars = rows
        .iter()
        .filter_map(|r| {
            if let HelpRow::Binding { keys, .. } = r {
                Some(keys.len())
            } else {
                None
            }
        })
        .max()
        .unwrap_or(8)
        .min(18); // cap so chips don't eat the whole panel
    let chip_col_w = (max_key_chars as f32 * cell_w + chip_pad_x * 2.0).round();
    let desc_x = body_left + chip_col_w + m.gap;

    // Draw rows, scissored to [body_top, body_top + body_h).
    let mut ry = body_top - state.scroll;
    for row in &rows {
        let row_height = match row {
            HelpRow::Section(_) => section_h + sep_h,
            HelpRow::Binding { .. } => row_h,
            HelpRow::Gap => (cell_h * 0.5).round(),
        };
        // Cull rows fully outside the visible window.
        if ry + row_height <= body_top || ry >= body_top + body_h {
            ry += row_height;
            continue;
        }
        match row {
            HelpRow::Section(title) => {
                // Dim section title.
                let section_ty = (ry + (section_h - cell_h) * 0.5).round();
                if section_ty >= body_top && section_ty + cell_h <= body_top + body_h {
                    let mut cx = body_left;
                    for ch in title.chars() {
                        renderer.push_overlay_glyph_px(cx.round(), section_ty, ch, fg_dim());
                        cx += cell_w;
                    }
                }
                // Separator below the section title.
                let sep_line = (ry + section_h + 1.0).round();
                if sep_line >= body_top && sep_line < body_top + body_h {
                    renderer.push_overlay_px(
                        body_left,
                        sep_line,
                        body_w + scrollbar_w + 4.0,
                        1.0,
                        hairline(),
                    );
                }
            }
            HelpRow::Binding { keys, desc } => {
                let text_ty = (ry + (row_h - cell_h) * 0.5).round();
                if text_ty + cell_h > body_top && text_ty < body_top + body_h {
                    // Key-cap chip background.
                    let chip_h = cell_h + chip_pad_y * 2.0;
                    let chip_w = (keys.chars().count() as f32 * cell_w + chip_pad_x * 2.0).round();
                    let chip_y = ry + (row_h - chip_h) * 0.5;
                    renderer.push_overlay_rrect_px(
                        body_left,
                        chip_y,
                        chip_w,
                        chip_h,
                        chip_radius,
                        with_alpha(color::default_fg(), 0.12),
                    );
                    renderer.push_overlay_px(
                        body_left,
                        chip_y,
                        chip_w,
                        1.0,
                        with_alpha(color::default_fg(), 0.25),
                    );
                    renderer.push_overlay_px(
                        body_left,
                        chip_y + chip_h - 1.0,
                        chip_w,
                        1.0,
                        with_alpha(color::default_bg(), 0.35),
                    );

                    // Key text inside chip.
                    let kx = body_left + chip_pad_x;
                    let mut cx = kx;
                    for ch in keys.chars() {
                        renderer.push_overlay_glyph_px(cx.round(), text_ty, ch, fg());
                        cx += cell_w;
                    }

                    // Description (right of chip column).
                    let mut cx = desc_x;
                    for ch in desc.chars() {
                        if cx + cell_w > panel.x + panel.w - scrollbar_w - 4.0 {
                            break;
                        }
                        renderer.push_overlay_glyph_px(cx.round(), text_ty, ch, fg());
                        cx += cell_w;
                    }
                }
            }
            HelpRow::Gap => {}
        }
        ry += row_height;
    }

    // Scrollbar (only when content overflows).
    if max_scroll > 0.0 {
        let sb_x = panel.x + panel.w - scrollbar_w - 2.0;
        let sb_y = body_top;
        let sb_h = body_h;
        let track = Rect::new(sb_x, sb_y, scrollbar_w, sb_h);
        let thumb_ratio = (body_h / content_h).min(1.0);
        let thumb_h = (sb_h * thumb_ratio).max(m.row_h * 0.5);
        let thumb_t = if max_scroll > 0.0 {
            state.scroll / max_scroll
        } else {
            0.0
        };
        let thumb_y = sb_y + (sb_h - thumb_h) * thumb_t;

        renderer.push_overlay_rrect_px(
            track.x,
            track.y,
            track.w,
            track.h,
            track.w * 0.5,
            track_off(),
        );
        let thumb_over = hit(track, mouse.0, mouse.1);
        let thumb_col = if thumb_over || mouse_down {
            state_fill(with_alpha(fg(), 0.35), 1.0, mouse_down && thumb_over)
        } else {
            with_alpha(fg(), 0.25)
        };
        renderer.push_overlay_rrect_px(
            track.x,
            thumb_y,
            track.w,
            thumb_h,
            track.w * 0.5,
            thumb_col,
        );

        // Drag the scrollbar with a press-latch: a press inside the track claims
        // the widget; the drag then continues for as long as the button is held,
        // even if the pointer leaves the narrow track (fast drags don't drop).
        let wid = id("help/scrollbar");
        if mouse_down && (hit(track, mouse.0, mouse.1) || *pressed == Some(wid)) {
            if pressed.is_none() {
                *pressed = Some(wid);
            }
            if *pressed == Some(wid) {
                let t =
                    ((mouse.1 - sb_y - thumb_h * 0.5) / (sb_h - thumb_h).max(1.0)).clamp(0.0, 1.0);
                state.scroll = t * max_scroll;
            }
        }
        if !mouse_down && *pressed == Some(wid) {
            *pressed = None;
        }

        // Record scrollbar widget interaction for animation.
        let a = anims.entry(wid).or_insert_with(|| Anim::new(0.0));
        a.target = if thumb_over { 1.0 } else { 0.0 };
    }

    // Suppress unused-variable warnings.
    let _ = (focused, footer_h);
    result
}

/// The display order of actions in the help panel, grouped by section.
/// Each entry is `(section, action)`. Actions not bound in the keymap are
/// omitted. The fixed entries (Alt+Arrow, Right-click, F1/Esc) are appended
/// after the keymap-derived rows.
const HELP_ACTION_ORDER: &[KeyAction] = &[
    // Tabs
    KeyAction::NewTab,
    KeyAction::ClosePane,
    KeyAction::NextTab,
    KeyAction::PrevTab,
    KeyAction::GoToTab(1),
    KeyAction::MoveTabLeft,
    KeyAction::MoveTabRight,
    // Split panes
    KeyAction::SplitVertical,
    KeyAction::SplitHorizontal,
    KeyAction::ToggleZoom,
    KeyAction::RotatePanes,
    KeyAction::EqualizePanes,
    KeyAction::BroadcastInput,
    KeyAction::FocusPaneLeft,
    KeyAction::FocusPaneRight,
    KeyAction::FocusPaneUp,
    KeyAction::FocusPaneDown,
    // Edit
    KeyAction::Copy,
    KeyAction::Paste,
    // View
    KeyAction::FontIncrease,
    KeyAction::FontDecrease,
    KeyAction::FontReset,
    KeyAction::ToggleStatusBar,
    KeyAction::ToggleMinimap,
    KeyAction::ToggleFullscreen,
    KeyAction::ToggleMaximize,
    KeyAction::ScrollUp,
    KeyAction::ScrollDown,
    KeyAction::ScrollTop,
    KeyAction::ScrollBottom,
    KeyAction::JumpPrevPrompt,
    KeyAction::JumpNextPrompt,
    KeyAction::ToggleFold,
    // App
    KeyAction::Settings,
    KeyAction::CommandPalette,
    KeyAction::Search,
    KeyAction::Hints,
    KeyAction::Help,
];

/// Build the help rows from the live keymap. The displayed chord for each
/// action is the first chord in the map that maps to that action, rendered with
/// [`crate::config::Chord::display_for`] so macOS shows ⌘-symbol runs and other
/// platforms show `+`-joined labels. If an action has no binding it is omitted
/// (user may have set it to `none`). Static extras (pane nav, right-click) are
/// appended at the end.
fn help_rows_from_keymap(keymap: &KeyMap, platform: Platform) -> Vec<HelpRow<'static>> {
    // Build action → first chord mapping. Sort entries for determinism:
    // prefer shorter display strings (fewer modifier bits) so "Ctrl+T" wins
    // over "Ctrl+Shift+Ctrl+T" if the map somehow has duplicates.
    let mut action_chord: std::collections::HashMap<KeyAction, String> =
        std::collections::HashMap::new();

    // Collect all chords, sort them so output is deterministic.
    let mut entries: Vec<(crate::config::Chord, KeyAction)> =
        keymap.iter().map(|(c, &a)| (c.clone(), a)).collect();
    // Sort: fewer modifiers first, then alphabetical key name.
    entries.sort_by_key(|(c, _)| {
        let mods = (c.ctrl as u8) + (c.alt as u8) + (c.meta as u8) + (c.shift as u8);
        (mods, c.key.clone())
    });
    for (chord, action) in entries {
        // Keep the first (fewest-modifier) chord per action, rendered for the
        // host platform (⌘-symbol run on macOS, `+`-joined elsewhere).
        action_chord
            .entry(action)
            .or_insert_with(|| chord.display_for(platform));
    }

    let mut rows: Vec<HelpRow<'static>> = Vec::new();
    let mut last_section: &'static str = "";

    for &action in HELP_ACTION_ORDER {
        let Some(chord_str) = action_chord.get(&action) else {
            continue;
        };
        let section = action.section();
        if section != last_section {
            rows.push(HelpRow::Gap);
            rows.push(HelpRow::Section(section));
            last_section = section;
        }
        // Leak the chord string for the 'static lifetime required by HelpRow.
        // These strings are short, few, and rebuilt only on open — acceptable.
        let keys: &'static str = Box::leak(chord_str.clone().into_boxed_str());
        let desc: &'static str = action.description();
        rows.push(HelpRow::Binding { keys, desc });
    }

    // Fixed non-keymap entries appended at the end.
    if last_section != "Split panes" && !last_section.is_empty() {
        rows.push(HelpRow::Gap);
    }
    // Alt+Arrow pane navigation is not in the keymap (context-sensitive).
    // Append it in the "Split panes" section if that section was emitted.
    // We find the index of the last "Split panes" binding and insert after.
    // Simpler: append at end as a separate block.
    rows.push(HelpRow::Gap);
    rows.push(HelpRow::Section("Navigation"));
    // Pane focus: Alt+Arrow on PC, ⌥Arrow on macOS (the focus_pane handler reads
    // the alt modifier regardless of platform; this is just the displayed label).
    rows.push(HelpRow::Binding {
        keys: if platform.is_mac() {
            "⌥Arrow"
        } else {
            "Alt+Arrow"
        },
        desc: "Focus adjacent pane",
    });
    // Hyperlink open: Cmd+Click on macOS, Ctrl+Click elsewhere.
    rows.push(HelpRow::Binding {
        keys: if platform.is_mac() {
            "⌘Click"
        } else {
            "Ctrl+Click"
        },
        desc: "Open hyperlink",
    });
    rows.push(HelpRow::Binding {
        keys: "Right-click",
        desc: "Context menu",
    });
    rows.push(HelpRow::Binding {
        keys: "F1 / Esc",
        desc: "Close this panel",
    });
    rows.push(HelpRow::Gap);

    rows
}
