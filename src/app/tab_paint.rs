//! Tab-bar painting: `paint_tab_bar`, `paint_tab_chip`, `paint_tab_label`,
//! `paint_floating_icons`.
//! Extracted from chrome.rs to keep both files under 700 lines.

use super::*;

impl App {
    /// Paint the three floating icon buttons (Help / Settings / Menu) when the
    /// full tab bar is hidden. No bar background — just a subtle pill behind the
    /// group and the icon glyphs at the top-right corner of the window.
    pub(crate) fn paint_floating_icons(
        renderer: &mut Renderer,
        hovered: Option<StripItem>,
        held: Option<StripItem>,
        focused: bool,
        win_controls: bool,
    ) {
        let m = renderer.cell_metrics();
        let (sw, _sh) = renderer.surface_size();
        let bar_h = tab_bar_h(m.height);
        let segs = floating_icon_segs(sw as f32, bar_h, win_controls);

        let fdim = if focused { 1.0 } else { 0.7 };
        let mul = |c: [f32; 4]| [c[0] * fdim, c[1] * fdim, c[2] * fdim, c[3]];
        let fg = mul(gui::fg());
        let fg_dim = mul(gui::fg_dim());
        let surface = mul(gui::glass_raised());
        let radius = gui_radius(m.height);

        // Subtle pill backdrop behind the icon group.
        if let (Some(first), Some(last)) = (segs.first(), segs.last()) {
            let pill_pad = 4.0;
            let px = first.rect.x - pill_pad;
            let py = (first.rect.y - pill_pad * 0.5).max(0.0);
            let pw = last.rect.x + last.rect.w - first.rect.x + pill_pad * 2.0;
            let ph = CTRL_BTN + pill_pad;
            renderer.push_overlay_rrect_px(
                px,
                py,
                pw,
                ph,
                radius,
                [surface[0], surface[1], surface[2], surface[3] * 0.55],
            );
        }

        // Softened press darkening (matches the full tab bar's gentler press).
        let press_fill = |base: [f32; 4]| [base[0] * 0.92, base[1] * 0.92, base[2] * 0.92, base[3]];

        for seg in &segs {
            let r = seg.rect;
            let is_hover = hovered == Some(seg.item);
            let is_held = held == Some(seg.item);
            let glyph = match seg.item {
                StripItem::Help => '?',
                // U+2699 GEAR is BMP and covered by Noto Sans Symbols2 / Apple
                // Symbols (already in the fallback set), unlike the previous PUA
                // U+F013 which tofus without a Nerd Font configured.
                StripItem::Settings => '\u{2699}',
                StripItem::WinMinimize => '\u{2013}', // – en dash
                StripItem::WinMaximize => '\u{25A1}', // □ white square
                StripItem::WinClose => '\u{2715}',    // ✕ multiplication x
                _ => '\u{2261}',
            };
            if is_held {
                renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, radius, press_fill(surface));
            } else if is_hover {
                renderer.push_overlay_rrect_px(
                    r.x,
                    r.y,
                    r.w,
                    r.h,
                    radius,
                    gui::state_fill(surface, 0.7, false),
                );
            }
            let cfg = if is_hover || is_held { fg } else { fg_dim };
            let gx = r.x + (r.w - m.width) * 0.5;
            let gy = r.center_y() - m.height * 0.5;
            renderer.push_overlay_glyph_px(gx.round(), gy.round(), glyph, cfg);
        }
    }

    /// Paint the real GUI tab bar (§3.1) into the pixel band `[0, tab_bar_h)`. The
    /// ACTIVE tab is an E2 raised chip whose top corners are rounded and whose body
    /// is flush to the bar bottom, with a 3px connector quad that overpaints the
    /// content hairline so the tab "opens into" the content surface, plus a top
    /// accent rail. Inactive tabs are recessed E1 chips sitting above the bar
    /// bottom. The close button fades in on hover with its own danger-tinted
    /// hover/press state. +/?/* / # are icon buttons. The rich (Unicode) title is
    /// drawn through the glyph atlas via `push_overlay_glyph_px` (tofu-proof). A
    /// held tab is lifted to a drag-ghost following the pointer.
    ///
    /// Associated (not `&self`) so it composes with the caller's `&mut Renderer`
    /// borrow; all `self`-derived data arrives via `snapshot` + scalars.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_tab_bar(
        renderer: &mut Renderer,
        snapshot: &[(String, bool, bool, bool)],
        focused: bool,
        hovered: Option<StripItem>,
        held: Option<StripItem>,
        dragging: Option<usize>,
        mouse_px: (f32, f32),
        spinner_frame: usize,
        tab_count: usize,
        display_offset: i32,
        history_size: usize,
        pane_counts: &[usize],
        active_pos: usize,
        left_inset: f32,
        win_controls: bool,
    ) {
        let m = renderer.cell_metrics();
        let (sw, _sh) = renderer.surface_size();
        let bar_w = sw as f32;
        let bar_h = tab_bar_h(m.height);
        if bar_w <= 0.0 || bar_h <= 0.0 {
            return;
        }

        // Dim the whole bar while unfocused so an idle window reads as "asleep".
        let fdim = if focused { 1.0 } else { 0.7 };
        let mul = |c: [f32; 4]| [c[0] * fdim, c[1] * fdim, c[2] * fdim, c[3]];

        let bar_bg = mul(gui::glass_body());
        let surface = mul(gui::glass_raised());
        let active_surface = mul(gui::glass_active_tab());
        let accent = mul(color::accent());
        let danger = mul(color::danger());
        let fg = mul(gui::fg());
        let fg_dim = mul(gui::fg_dim());
        let raised = gui::glass_active_tab();
        let chip_luma = color::luma(raised);
        let active_fg = if chip_luma > 0.5 {
            mul([0.04, 0.04, 0.05, 1.0])
        } else {
            mul([0.97, 0.97, 0.98, 1.0])
        };

        // 1) Bar backdrop (E1).
        renderer.push_overlay_px(0.0, 0.0, bar_w, bar_h, bar_bg);

        // 2) Brand mark on the far left, past any left inset (macOS traffic lights).
        let mark_y = (bar_h - m.height) * 0.5;
        renderer.push_overlay_glyph_px((left_inset + m.width).round(), mark_y.round(), '◆', accent);

        // 3) Lay out the bar (pixel rects) and paint each item.
        let descs: Vec<(&str, bool, bool)> = snapshot
            .iter()
            .map(|(t, a, b, _)| (t.as_str(), *a, *b))
            .collect();
        let tag_reserve = tab_tag_reserve(tab_count, m.width);
        let segs = strip_layout_ex(
            &descs,
            bar_w,
            bar_h,
            m.width,
            tag_reserve,
            active_pos,
            left_inset,
            win_controls,
        );
        let multi = descs.len() > 1;
        let spin = SPINNER_FRAMES[spinner_frame % SPINNER_FRAMES.len()];

        // Softened press darkening (was ×0.85, which gave icon buttons a heavy
        // click "thunk"); ×0.92 keeps tactility without the slam.
        let press_fill = |base: [f32; 4]| [base[0] * 0.92, base[1] * 0.92, base[2] * 0.92, base[3]];
        let hover_fill = |base: [f32; 4]| gui::state_fill(base, 0.7, false);

        // Track a held tab so we can defer its drag-ghost to the very top.
        let mut ghost: Option<(gui::Rect, String, usize)> = None;

        for seg in &segs {
            let r = seg.rect;
            let is_hover = hovered == Some(seg.item);
            let is_held = held == Some(seg.item);
            match seg.item {
                StripItem::Tab(i) => {
                    let (_title, active, busy) =
                        descs.get(i).copied().unwrap_or(("", false, false));
                    let is_spinning = snapshot.get(i).map(|s| s.3).unwrap_or(false);
                    let panes = pane_counts.get(i).copied().unwrap_or(1);
                    if dragging == Some(i) {
                        ghost = Some((r, seg.label.clone(), i));
                    }
                    Self::paint_tab_chip(
                        renderer,
                        r,
                        m.height,
                        m.width,
                        i,
                        &seg.label,
                        active,
                        busy,
                        is_spinning,
                        is_hover,
                        is_held,
                        spin,
                        bar_h,
                        surface,
                        active_surface,
                        accent,
                        active_fg,
                        fg,
                        fg_dim,
                        dragging == Some(i),
                        multi,
                        panes,
                    );
                }
                StripItem::TabClose(i) => {
                    let active = descs.get(i).map(|d| d.1).unwrap_or(false);
                    let tab_hover = hovered == Some(StripItem::Tab(i)) || is_hover;
                    if tab_hover && dragging.is_none() {
                        if is_hover {
                            let a = if is_held { 0.30 } else { 0.18 };
                            renderer.push_overlay_rrect_px(
                                r.x,
                                r.y,
                                r.w,
                                r.h,
                                3.0,
                                [danger[0], danger[1], danger[2], a],
                            );
                        }
                        let cfg = if is_hover {
                            danger
                        } else if active {
                            active_fg
                        } else {
                            fg_dim
                        };
                        let gx = r.x + (r.w - m.width) * 0.5;
                        let gy = r.center_y() - m.height * 0.5;
                        renderer.push_overlay_glyph_px(gx.round(), gy.round(), '✕', cfg);
                    }
                }
                StripItem::NewTab
                | StripItem::Help
                | StripItem::Settings
                | StripItem::Menu
                | StripItem::WinMinimize
                | StripItem::WinMaximize
                | StripItem::WinClose => {
                    let glyph = match seg.item {
                        StripItem::NewTab => '+',
                        StripItem::Help => '?',
                        StripItem::Settings => '\u{2699}',
                        StripItem::WinMinimize => '\u{2013}', // – en dash
                        StripItem::WinMaximize => '\u{25A1}', // □ white square
                        StripItem::WinClose => '\u{2715}',    // ✕ multiplication x
                        _ => '\u{2261}',
                    };
                    // The window-close button gets a danger-tinted hover/press so
                    // it reads like a close affordance (mirrors the tab close box).
                    let is_close = seg.item == StripItem::WinClose;
                    if is_close && (is_hover || is_held) {
                        let a = if is_held { 0.30 } else { 0.18 };
                        renderer.push_overlay_rrect_px(
                            r.x,
                            r.y,
                            r.w,
                            r.h,
                            gui_radius(m.height),
                            [danger[0], danger[1], danger[2], a],
                        );
                    } else if is_held {
                        renderer.push_overlay_rrect_px(
                            r.x,
                            r.y,
                            r.w,
                            r.h,
                            gui_radius(m.height),
                            press_fill(surface),
                        );
                    } else if is_hover {
                        renderer.push_overlay_rrect_px(
                            r.x,
                            r.y,
                            r.w,
                            r.h,
                            gui_radius(m.height),
                            hover_fill(surface),
                        );
                    }
                    let nudge = if is_held { 1.0 } else { 0.0 };
                    let cfg = if is_close && (is_hover || is_held) {
                        danger
                    } else if is_hover || is_held {
                        fg
                    } else {
                        fg_dim
                    };
                    let gx = r.x + (r.w - m.width) * 0.5;
                    let gy = r.center_y() - m.height * 0.5 + nudge;
                    renderer.push_overlay_glyph_px(gx.round(), gy.round(), glyph, cfg);
                }
            }
        }

        // 4) Drag-ghost: redraw the held tab lifted to the top, following the pointer.
        if let Some((r, label, i)) = ghost {
            let gx = (mouse_px.0 - r.w * 0.5).clamp(0.0, bar_w - r.w);
            let gr = gui::Rect::new(gx, r.y - 2.0, r.w, r.h);
            renderer.push_overlay_rrect_px(
                gr.x,
                gr.y,
                gr.w,
                gr.h,
                TAB_RADIUS,
                mul(gui::glass_float()),
            );
            renderer.push_overlay_px(
                gr.x,
                gr.y,
                gr.w,
                2.0,
                [accent[0], accent[1], accent[2], accent[3] * 0.5],
            );
            Self::paint_tab_label(
                renderer, gr, m.height, m.width, i, &label, true, false, false, spin, active_fg,
                active_fg, multi, 1,
            );
        }

        // 5) Scrollback % (when scrolled back), tucked just left of the right
        // controls. The tab-count badge ("N tabs") is intentionally omitted — the
        // tab chips themselves already convey the count.
        let _ = tab_count;
        let right_ctrl_x = bar_w - CTRL_BTN * 3.0 - TAB_GAP;
        let tag_right = right_ctrl_x - m.width;
        let ty = ((bar_h - m.height) * 0.5).round();
        if display_offset > 0 {
            let pct = if history_size > 0 {
                ((display_offset as f32 / history_size as f32) * 100.0).round() as u32
            } else {
                100
            }
            .min(100);
            let s = format!("⇡{pct:>3}%");
            let w = renderer.text_width_px(&s);
            renderer.push_overlay_glyph_px_str((tag_right - w).round(), ty, &s, accent);
        }
    }

    /// Paint the modifier-HOLD numbered overlay: a small accent number badge
    /// centered on each visible tab chip (1-based, only 1..=9 are jumpable via the
    /// modifier+digit chord). Drawn on TOP of the (possibly cached) tab bar each
    /// frame the overlay is active, mirroring the rename-editor's draw-after-cache
    /// pattern. `chips` is the list of `(rect, position)` for visible Tab segs.
    pub(crate) fn paint_tab_hold_numbers(renderer: &mut Renderer, chips: &[(gui::Rect, usize)]) {
        let m = renderer.cell_metrics();
        let accent = color::accent();
        let radius = gui_radius(m.height);
        // Scrim under the badge so the number reads over any chip title.
        let scrim = {
            let g = gui::glass_float();
            [g[0], g[1], g[2], (g[3] * 0.92).min(1.0)]
        };
        for (r, pos) in chips {
            // Only 1..=9 are reachable via the modifier+digit chord; number the
            // rest with no badge (they remain clickable).
            let n = pos + 1;
            if n > 9 {
                continue;
            }
            let label = n.to_string();
            let bw = (m.width + 8.0).max(m.height);
            let bh = (m.height + 4.0).min(r.h);
            let bx = (r.center_x() - bw * 0.5).round();
            let by = (r.center_y() - bh * 0.5).round();
            renderer.push_overlay_rrect_px(bx, by, bw, bh, radius, scrim);
            // Accent ring for a chip-like badge.
            let ring = [accent[0], accent[1], accent[2], 0.6];
            renderer.push_overlay_rrect_px(bx, by, bw, bh, radius, ring);
            let inset = 1.5;
            if bw > 2.0 * inset && bh > 2.0 * inset {
                renderer.push_overlay_rrect_px(
                    bx + inset,
                    by + inset,
                    bw - 2.0 * inset,
                    bh - 2.0 * inset,
                    (radius - inset).max(0.0),
                    scrim,
                );
            }
            let gx = (r.center_x() - m.width * 0.5).round();
            let gy = (r.center_y() - m.height * 0.5).round();
            renderer.push_overlay_glyph_px_str(gx, gy, &label, accent);
        }
    }

    /// Paint one tab chip's surface (connector + rail for active, recess for
    /// inactive) and its label. Split out so the drag-ghost can reuse the label
    /// pass without the surface.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_tab_chip(
        renderer: &mut Renderer,
        r: gui::Rect,
        cell_h: f32,
        cell_w: f32,
        idx: usize,
        label: &str,
        active: bool,
        busy: bool,
        spinning: bool,
        hover: bool,
        held: bool,
        spin: char,
        bar_h: f32,
        surface: [f32; 4],
        active_surface: [f32; 4],
        accent: [f32; 4],
        active_fg: [f32; 4],
        fg: [f32; 4],
        fg_dim: [f32; 4],
        is_ghost: bool,
        multi: bool,
        pane_count: usize,
    ) {
        if is_ghost {
            return;
        }
        let _ = busy;
        if active {
            // E2+ raised body: brighter/more opaque than the bar and inactive chips
            // so the active tab clearly reads as "in focus". Only the TOP corners are
            // rounded; the bottom edge is square and flush to the content seam. The
            // per-corner rrect means there is no bottom corner feather to leak
            // background through, so the connector patch below is gap-free.
            renderer.push_overlay_rrect4_px(
                r.x,
                r.y,
                r.w,
                r.h,
                [TAB_RADIUS, TAB_RADIUS, 0.0, 0.0],
                active_surface,
            );
            // Connector: a thin patch over the seam, capped at the bar bottom edge
            // (y = bar_h - 2 .. bar_h) so the active chip is FLUSH with the bar and
            // never drops below it. (The old `bar_h - 2, h = 4` ran 2px into the
            // content area, making the active tab look 2px taller than the bar.)
            // The chip body's bottom is square (no corner feather), so a flush
            // patch leaves no background hairline at the seam.
            renderer.push_overlay_px(r.x, bar_h - 2.0, r.w, 2.0, active_surface);
            // Soft accent crown (2px, low alpha). Inset horizontally by the corner
            // radius so the bar spans only the FLAT top edge between the two rounded
            // corners and never enters the corner zones — there is therefore no way
            // for the accent to bleed outside the chip's rounded top corners onto the
            // bar background. (The old full-width sharp quad filled those corner
            // zones and, showing through the body's transparent rounded corners, read
            // as a white/light rectangular fill behind the corners.) Drawn into
            // `overlay_text` like the body so the pass order is consistent.
            let crown = [accent[0], accent[1], accent[2], accent[3] * 0.5];
            let crown_w = (r.w - 2.0 * TAB_RADIUS).max(0.0);
            if crown_w > 0.0 {
                renderer.push_overlay_rrect4_px(
                    r.x + TAB_RADIUS,
                    r.y,
                    crown_w,
                    2.0,
                    [0.0, 0.0, 0.0, 0.0],
                    crown,
                );
            }
        } else {
            let rr = gui::Rect::new(r.x, r.y + 3.0, r.w, r.h - 5.0);
            // Softened press: the held fill is only a touch above hover (was a hard
            // 0.55→0.70 jump that read as a heavy "thunk" on click). The chip now
            // settles into focus rather than slamming.
            let fill = if held {
                [surface[0], surface[1], surface[2], surface[3] * 0.60]
            } else if hover {
                [surface[0], surface[1], surface[2], surface[3] * 0.55]
            } else {
                [surface[0], surface[1], surface[2], surface[3] * 0.20]
            };
            renderer.push_overlay_rrect_px(rr.x, rr.y, rr.w, rr.h, TAB_RADIUS, fill);
            if !hover && !held {
                let h = gui::hairline();
                renderer.push_overlay_px(
                    rr.x,
                    rr.y + rr.h - 1.0,
                    rr.w,
                    1.0,
                    [h[0], h[1], h[2], h[3] * 0.4],
                );
            }
            if hover && !held {
                renderer.push_overlay_px(
                    rr.x,
                    rr.y,
                    rr.w,
                    1.0,
                    [accent[0], accent[1], accent[2], accent[3] * 0.25],
                );
            }
        }
        let label_fg = if active { active_fg } else { fg_dim };
        Self::paint_tab_label(
            renderer, r, cell_h, cell_w, idx, label, active, busy, spinning, spin, label_fg,
            accent, multi, pane_count,
        );
        let _ = fg;
    }

    /// Draw a tab chip's status glyph + numbered title, clipped to the chip's text
    /// area (leaving room for the close box on the right).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_tab_label(
        renderer: &mut Renderer,
        r: gui::Rect,
        cell_h: f32,
        cell_w: f32,
        idx: usize,
        label: &str,
        active: bool,
        busy: bool,
        spinning: bool,
        spin: char,
        label_fg: [f32; 4],
        accent: [f32; 4],
        multi: bool,
        pane_count: usize,
    ) {
        let ty = (r.center_y() - cell_h * 0.5).round();
        let mut tx = r.x + TAB_PAD_X;
        if spinning {
            renderer.push_overlay_glyph_px(
                tx.round(),
                ty,
                spin,
                if active { label_fg } else { accent },
            );
            tx += cell_w;
        } else if busy && !active {
            renderer.push_overlay_glyph_px(tx.round(), ty, '•', accent);
            tx += cell_w;
        }
        // Split indicator: a small pane-layout glyph (◫ for 2 panes, ▦ for >2) just
        // before the title when this tab is tiled. Drawn in the accent color so the
        // split status reads at a glance without crowding the label.
        let indicator = split_indicator(pane_count);
        if !indicator.is_empty() {
            renderer.push_overlay_glyph_px_str(tx.round(), ty, indicator, accent);
            tx += cell_w;
            // For >2 panes, append the count (e.g. "▦3") so dense tilings are legible.
            if pane_count > 2 {
                let n = pane_count.to_string();
                renderer.push_overlay_glyph_px_str(tx.round(), ty, &n, accent);
                tx += cell_w * n.chars().count() as f32;
            }
        }
        let reserve = if multi {
            CLOSE_BOX + TAB_PAD_X
        } else {
            TAB_PAD_X
        };
        let text_w = (r.w - (tx - r.x) - reserve).max(0.0);
        let max_chars = (text_w / cell_w).floor() as usize;
        let s = if multi {
            let prefix = format!("{} ", idx + 1);
            let title_max = max_chars.saturating_sub(prefix.chars().count()).max(1);
            format!("{}{}", prefix, fit_label(label, title_max))
        } else {
            fit_label(label, max_chars.max(1))
        };
        renderer.push_overlay_glyph_px_str(tx.round(), ty, &s, label_fg);
    }
}
