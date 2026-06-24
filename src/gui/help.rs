//! Help panel: keybinding reference overlay.

use super::*;

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
/// are the current frame's pointer state. `state` is the caller-owned scroll
/// position (mutable borrow; updated in place).
#[allow(clippy::too_many_arguments)]
pub fn build_help(
    renderer:   &mut Renderer,
    cell_w:     f32,
    cell_h:     f32,
    surface:    (f32, f32),
    mouse:      (f32, f32),
    mouse_down: bool,
    clicked:    bool,
    pressed:    &mut Option<WidgetId>,
    focused:    &mut Option<WidgetId>,
    anims:      &mut HashMap<WidgetId, Anim>,
    state:      &mut HelpState,
) -> HelpResult {
    let mut result = HelpResult::default();
    let m = Metrics::new(cell_w, cell_h);

    // Full-screen scrim — click on scrim closes the panel.
    let scrim = Rect::new(0.0, 0.0, surface.0, surface.1);
    renderer.push_overlay_px(scrim.x, scrim.y, scrim.w, scrim.h, [0.0, 0.0, 0.0, 0.5]);
    if clicked && hit(scrim, mouse.0, mouse.1) {
        // Will be refined: only close if click was OUTSIDE the panel (checked below).
    }

    // Panel sizing: ≈ 50 columns wide, tall enough for visible rows (capped at 80%).
    let pw = (cell_w * 50.0).min(surface.0 - 2.0 * m.pad).max(cell_w * 28.0);
    let max_ph = (surface.1 * 0.82).round();
    let header_h = m.row_h;
    let footer_h = 0.0; // no footer — ✕ in the header suffices

    // Lay out the help rows once to measure total content height.
    let rows = help_rows();
    let row_h = m.row_h;
    let sep_h = 6.0;
    let section_h = (m.cell_h + 4.0).round();
    let content_h: f32 = rows.iter().map(|r| match r {
        HelpRow::Section(_) => section_h + sep_h,
        HelpRow::Binding { .. } => row_h,
        HelpRow::Gap => (m.cell_h * 0.5).round(),
    }).sum();

    let scrollbar_w = (m.gap.max(6.0)).round();
    let body_h = content_h.min(max_ph - header_h - 2.0 * m.pad);
    let ph = (header_h + m.pad + body_h + m.pad).round().min(max_ph);
    let px = ((surface.0 - pw) * 0.5).round();
    let py = ((surface.1 - ph) * 0.5).round().max(m.pad);
    let panel = Rect::new(px, py, pw, ph);

    // Close if clicked outside the panel (scrim click).
    if clicked && !hit(panel, mouse.0, mouse.1) {
        result.close = true;
    }

    // Draw panel background + left accent rail.
    renderer.push_overlay_rrect_px(panel.x, panel.y, panel.w, panel.h, m.card_radius, glass_raised());
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
    let close_r = Rect::new(panel.x + panel.w - header_h, panel.y + m.pad * 0.5, header_h, header_h);
    {
        let wid = id("help/close");
        let over = hit(close_r, mouse.0, mouse.1);
        if over {
            renderer.push_overlay_rrect_px(
                close_r.x + 2.0, close_r.y + 2.0,
                close_r.w - 4.0, close_r.h - 4.0,
                3.0, state_fill(glass_raised(), 1.0, false),
            );
        }
        let gx = close_r.x + (close_r.w - cell_w) * 0.5;
        let gy = close_r.y + (close_r.h - cell_h) * 0.5;
        renderer.push_overlay_glyph_px(gx.round(), gy.round(), '✕', if over { fg() } else { fg_dim() });
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
    let body_top  = sep_y + 1.0;
    let body_left = panel.x + m.pad;
    let body_w    = panel.w - m.pad * 2.0 - scrollbar_w - 4.0;
    let _body_rect = Rect::new(body_left, body_top, body_w, body_h);

    // Clamp scroll.
    let max_scroll = (content_h - body_h).max(0.0);
    state.scroll = state.scroll.clamp(0.0, max_scroll);

    // Key-cap chip metrics.
    let chip_pad_x = (cell_w * 0.6).round();
    let chip_pad_y = 2.0;
    let chip_radius = 3.0;

    // Left column: chips width (fixed, based on longest key string).
    let max_key_chars = rows.iter().filter_map(|r| {
        if let HelpRow::Binding { keys, .. } = r { Some(keys.len()) } else { None }
    }).max().unwrap_or(8).min(18); // cap so chips don't eat the whole panel
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
                    renderer.push_overlay_px(body_left, sep_line, body_w + scrollbar_w + 4.0, 1.0, hairline());
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
                        body_left, chip_y, chip_w, chip_h, chip_radius,
                        with_alpha(color::default_fg(), 0.12),
                    );
                    renderer.push_overlay_px(body_left, chip_y, chip_w, 1.0, with_alpha(color::default_fg(), 0.25));
                    renderer.push_overlay_px(body_left, chip_y + chip_h - 1.0, chip_w, 1.0, with_alpha(color::default_bg(), 0.35));

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
        let thumb_t  = if max_scroll > 0.0 { state.scroll / max_scroll } else { 0.0 };
        let thumb_y  = sb_y + (sb_h - thumb_h) * thumb_t;

        renderer.push_overlay_rrect_px(track.x, track.y, track.w, track.h, track.w * 0.5, track_off());
        let thumb_over = hit(track, mouse.0, mouse.1);
        let thumb_col = if thumb_over || mouse_down {
            state_fill(with_alpha(fg(), 0.35), 1.0, mouse_down && thumb_over)
        } else {
            with_alpha(fg(), 0.25)
        };
        renderer.push_overlay_rrect_px(track.x, thumb_y, track.w, thumb_h, track.w * 0.5, thumb_col);

        // Drag the scrollbar.
        if mouse_down && hit(track, mouse.0, mouse.1) {
            let t = ((mouse.1 - sb_y - thumb_h * 0.5) / (sb_h - thumb_h).max(1.0)).clamp(0.0, 1.0);
            state.scroll = t * max_scroll;
        }

        // Record scrollbar widget interaction for animation.
        let wid = id("help/scrollbar");
        let a = anims.entry(wid).or_insert_with(|| Anim::new(0.0));
        a.target = if thumb_over { 1.0 } else { 0.0 };
    }

    // Suppress unused-variable warnings.
    let _ = (pressed, focused, footer_h);
    result
}

/// The canonical set of help rows shown in the F1 panel. Separated into sections
/// with `HelpRow::Section` headers and `HelpRow::Gap` spacers.
fn help_rows() -> Vec<HelpRow<'static>> {
    vec![
        HelpRow::Gap,
        HelpRow::Section("Tabs"),
        HelpRow::Binding { keys: "Ctrl+Shift+T",   desc: "New tab" },
        HelpRow::Binding { keys: "Ctrl+Shift+W",   desc: "Close pane / tab" },
        HelpRow::Binding { keys: "Ctrl+Tab",        desc: "Next tab" },
        HelpRow::Binding { keys: "Ctrl+Shift+Tab",  desc: "Previous tab" },
        HelpRow::Gap,
        HelpRow::Section("Split panes"),
        HelpRow::Binding { keys: "Ctrl+Shift+E",   desc: "Split vertical" },
        HelpRow::Binding { keys: "Ctrl+Shift+O",   desc: "Split horizontal" },
        HelpRow::Binding { keys: "Alt+Arrow",       desc: "Focus adjacent pane" },
        HelpRow::Gap,
        HelpRow::Section("Edit"),
        HelpRow::Binding { keys: "Ctrl+Shift+C",   desc: "Copy selection" },
        HelpRow::Binding { keys: "Ctrl+Shift+V",   desc: "Paste" },
        HelpRow::Binding { keys: "Ctrl+Click",      desc: "Open hyperlink" },
        HelpRow::Gap,
        HelpRow::Section("View"),
        HelpRow::Binding { keys: "Ctrl  +",         desc: "Font bigger" },
        HelpRow::Binding { keys: "Ctrl  -",         desc: "Font smaller" },
        HelpRow::Binding { keys: "Ctrl  0",         desc: "Font reset" },
        HelpRow::Binding { keys: "Ctrl+Shift+B",    desc: "Toggle status bar" },
        HelpRow::Binding { keys: "Shift+PgUp",      desc: "Scroll history up" },
        HelpRow::Binding { keys: "Shift+PgDn",      desc: "Scroll history down" },
        HelpRow::Binding { keys: "Shift+Home",      desc: "Scroll to top" },
        HelpRow::Binding { keys: "Shift+End",       desc: "Scroll to bottom" },
        HelpRow::Gap,
        HelpRow::Section("App"),
        HelpRow::Binding { keys: "Ctrl+,",          desc: "Settings" },
        HelpRow::Binding { keys: "Right-click",     desc: "Context menu" },
        HelpRow::Binding { keys: "F1  /  Esc",      desc: "Close this panel" },
        HelpRow::Gap,
    ]
}


