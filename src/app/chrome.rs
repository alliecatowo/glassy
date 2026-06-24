//! Chrome painting: tab bar, status bar, settings overlay.

use super::*;

impl App {
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
        // Active-tab chip is lifted one more stop above the bar (E2+): noticeably
        // brighter/more opaque than inactive chips so it clearly reads as "foreground".
        let active_surface = mul(gui::glass_active_tab());
        let accent = mul(color::accent());
        let danger = mul(color::danger());
        let fg = mul(gui::fg());
        let fg_dim = mul(gui::fg_dim());
        // Active-tab label color derived from the LUMA of the ACTIVE chip body so
        // it is always legible (near-black on light themes, near-white on dark ones).
        let raised = gui::glass_active_tab();
        let chip_luma = 0.2126 * raised[0] + 0.7152 * raised[1] + 0.0722 * raised[2];
        let active_fg = if chip_luma > 0.5 {
            mul([0.04, 0.04, 0.05, 1.0])
        } else {
            mul([0.97, 0.97, 0.98, 1.0])
        };

        // 1) Bar backdrop (E1) + top accent rail + bottom hairline (content seam).
        renderer.push_overlay_px(0.0, 0.0, bar_w, bar_h, bar_bg);
        renderer.push_overlay_px(0.0, 0.0, bar_w, 1.0, mul(gui::rail()));
        renderer.push_overlay_px(0.0, bar_h - 1.0, bar_w, 1.0, mul(gui::hairline()));

        // 2) Brand mark on the far left.
        let mark_y = (bar_h - m.height) * 0.5;
        renderer.push_overlay_glyph_px((m.width).round(), mark_y.round(), '◆', accent);

        // 3) Lay out the bar (pixel rects) and paint each item.
        let descs: Vec<(&str, bool, bool)> =
            snapshot.iter().map(|(t, a, b, _)| (t.as_str(), *a, *b)).collect();
        let segs = strip_layout(&descs, bar_w, bar_h, m.width);
        let multi = descs.len() > 1;
        let spin = SPINNER_FRAMES[spinner_frame % SPINNER_FRAMES.len()];

        // Helper: state-driven fill for a control/chip surface.
        let press_fill = |base: [f32; 4]| {
            [base[0] * 0.85, base[1] * 0.85, base[2] * 0.85, base[3]]
        };
        let hover_fill = |base: [f32; 4]| gui::state_fill(base, 0.7, false);

        // Track a held tab so we can defer its drag-ghost to the very top.
        let mut ghost: Option<(gui::Rect, String, usize)> = None;

        for seg in &segs {
            let r = seg.rect;
            let is_hover = hovered == Some(seg.item);
            let is_held = held == Some(seg.item);
            match seg.item {
                StripItem::Tab(i) => {
                    let (_title, active, busy) = descs.get(i).copied().unwrap_or(("", false, false));
                    let is_spinning = snapshot.get(i).map(|s| s.3).unwrap_or(false);
                    // A dragged tab is rendered last as a ghost; reserve it.
                    if dragging == Some(i) {
                        ghost = Some((r, seg.label.clone(), i));
                    }
                    Self::paint_tab_chip(
                        renderer, r, m.height, m.width, i, &seg.label, active, busy, is_spinning,
                        is_hover, is_held, spin, bar_h, surface, active_surface, accent, active_fg, fg, fg_dim,
                        dragging == Some(i), multi,
                    );
                }
                StripItem::TabClose(i) => {
                    let active = descs.get(i).map(|d| d.1).unwrap_or(false);
                    // Close fades in on hover of either the close box or its tab.
                    let tab_hover = hovered == Some(StripItem::Tab(i)) || is_hover;
                    if tab_hover && dragging.is_none() {
                        if is_hover {
                            let a = if is_held { 0.30 } else { 0.18 };
                            renderer.push_overlay_rrect_px(
                                r.x, r.y, r.w, r.h, 3.0,
                                [danger[0], danger[1], danger[2], a],
                            );
                        }
                        let cfg = if is_hover { danger } else if active { active_fg } else { fg_dim };
                        let gx = r.x + (r.w - m.width) * 0.5;
                        let gy = r.center_y() - m.height * 0.5;
                        renderer.push_overlay_glyph_px(gx.round(), gy.round(), '✕', cfg);
                    }
                }
                StripItem::NewTab | StripItem::Help | StripItem::Settings | StripItem::Menu => {
                    let glyph = match seg.item {
                        // U+002B PLUS SIGN — universally rasterized, clearly "new tab".
                        StripItem::NewTab => '+',
                        // U+003F QUESTION MARK — clean, universally supported.
                        StripItem::Help => '?',
                        // U+F013 nf-fa-cog — Nerd Font gear icon; present in FiraCode Nerd
                        // Font (the default) and any Nerd Font patched face. Falls back to
                        // U+2699 GEAR via the standard symbol fallback chain on systems
                        // without a Nerd Font installed.
                        StripItem::Settings => '\u{F013}',
                        // U+2261 IDENTICAL TO (≡) — triple bar reads as "hamburger menu";
                        // BMP, ASCII-width, universally rasterized in monospace fonts.
                        _ => '\u{2261}',
                    };
                    let base = surface;
                    if is_held {
                        renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, gui_radius(m.height), press_fill(base));
                    } else if is_hover {
                        renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, gui_radius(m.height), hover_fill(base));
                        renderer.push_overlay_px(r.x, r.y, r.w, 1.0, mul(gui::rail()));
                    }
                    let nudge = if is_held { 1.0 } else { 0.0 };
                    let cfg = if is_hover || is_held { fg } else { fg_dim };
                    let gx = r.x + (r.w - m.width) * 0.5;
                    let gy = r.center_y() - m.height * 0.5 + nudge;
                    renderer.push_overlay_glyph_px(gx.round(), gy.round(), glyph, cfg);
                }
            }
        }

        // 4) Drag-ghost: redraw the held tab lifted to the top, following the
        //    pointer's x, so it visibly floats above its siblings while reordering.
        if let Some((r, label, i)) = ghost {
            let gx = (mouse_px.0 - r.w * 0.5).clamp(0.0, bar_w - r.w);
            let gr = gui::Rect::new(gx, r.y - 2.0, r.w, r.h);
            renderer.push_overlay_rrect_px(gr.x, gr.y, gr.w, gr.h, TAB_RADIUS, mul(gui::glass_float()));
            renderer.push_overlay_px(gr.x, gr.y, gr.w, 2.0, accent);
            Self::paint_tab_label(renderer, gr, m.height, m.width, i, &label, true, false, false, spin, active_fg, active_fg, multi);
        }

        // 5) Tab-count badge + scrollback %, tucked just left of the right controls
        //    as dim labels (fixed-width so the controls never shift).
        let right_ctrl_x = bar_w - CTRL_BTN * 3.0 - TAB_GAP;
        let mut tag_right = right_ctrl_x - m.width;
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
            tag_right -= w + m.width;
        }
        if tab_count > 1 {
            let s = format!("{tab_count} tabs");
            let w = renderer.text_width_px(&s);
            renderer.push_overlay_glyph_px_str((tag_right - w).round(), ty, &s, fg_dim);
        }
    }

    /// Paint the Wave-4 status bar (§3.4): a `STATUS_BAR_H`-px E1 band at the
    /// bottom of the window. Content is laid out as fixed-width right-aligned
    /// segments so nothing jitters as values change:
    ///
    ///   `[mode]  …  [sel]  [scroll%]  [enc]`
    ///
    /// **mode** = `ALT` when the focused pane is in alt-screen, `MOUSE` when mouse
    /// reporting is active (from `TermMode`). Both can be absent at once (normal
    /// screen, no mouse reporting). **scroll%** = `⇡NN%` when scrolled back into
    /// history (`display_offset > 0`). **sel** = glyph count when there is an
    /// active text selection. **enc** = `UTF-8` (always, for now). git/cwd slots
    /// are reserved but hidden until a follow-up lands `/proc`-based data.
    ///
    /// Associated fn (no `&self`) so it composes with the caller's `&mut Renderer`
    /// borrow; all data arrives as plain parameters.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_status_bar(
        renderer: &mut Renderer,
        surface_h: u32,
        term_mode: TermMode,
        display_offset: i32,
        history_size: usize,
        sel_len: usize,
        win_focused: bool,
        cwd: Option<&std::path::Path>,
        git_branch: Option<&str>,
        progress: Option<crate::image::ProgressState>,
    ) {
        let m = renderer.cell_metrics();
        let (sw, _sh) = renderer.surface_size();
        let bar_w = sw as f32;
        let bar_h = STATUS_BAR_H;
        let bar_y = surface_h as f32 - bar_h;

        if bar_w <= 0.0 || bar_h <= 0.0 {
            return;
        }

        // Dim while window is unfocused, matching tab-bar convention.
        let fdim = if win_focused { 1.0 } else { 0.7 };
        let mul = |c: [f32; 4]| [c[0] * fdim, c[1] * fdim, c[2] * fdim, c[3]];

        let bar_bg = mul(gui::glass_body());
        let accent  = mul(color::accent());
        let fg_dim  = mul(gui::fg_dim());
        let fg      = mul(gui::fg());

        // 1) Bar backdrop + top hairline (mirrors the tab bar's bottom seam).
        renderer.push_overlay_px(0.0, bar_y, bar_w, bar_h, bar_bg);
        renderer.push_overlay_px(0.0, bar_y, bar_w, 1.0, mul(gui::hairline()));

        // Glyph vertical centre within the bar.
        let ty = (bar_y + (bar_h - m.height) * 0.5).round();

        // 2) Right-aligned fixed-width segments (right → left, each padded to a
        //    fixed character count so a value change never shifts other segments).
        //
        //    Widths (in multiples of cell_w):
        //      enc:    5 chars ("UTF-8") + 1 gap = 6 cw
        //      scroll: 6 chars ("⇡100%") + 1 gap = 7 cw   (hidden when at bottom)
        //      sel:    8 chars ("999 sel") + 1 gap = 9 cw  (hidden when no sel)
        //      mode:   7 chars ("MOUSE  " or "ALT    ") + 1 gap = 8 cw (hidden when plain)
        //
        //    Right margin: 1 cw.
        let right_margin = m.width;
        let mut rx = bar_w - right_margin;

        // Encoding (always shown, right-aligned anchor).
        {
            let s = "UTF-8";
            let w = renderer.text_width_px(s);
            renderer.push_overlay_glyph_px_str((rx - w).round(), ty, s, fg_dim);
            rx -= (6.0 * m.width).round(); // fixed 6-char slot
        }

        // Scroll percent — shown only when scrolled back into history.
        if display_offset > 0 {
            let pct = if history_size > 0 {
                ((display_offset as f32 / history_size as f32) * 100.0).round() as u32
            } else {
                100
            }
            .min(100);
            let s = format!("⇡{pct:>3}%");
            let w = renderer.text_width_px(&s);
            renderer.push_overlay_glyph_px_str((rx - w).round(), ty, &s, accent);
        }
        rx -= (7.0 * m.width).round(); // fixed 7-char slot (even when hidden)

        // Selection glyph count — shown only when a selection is active.
        if sel_len > 0 {
            let s = format!("{sel_len} sel");
            let w = renderer.text_width_px(&s);
            renderer.push_overlay_glyph_px_str((rx - w).round(), ty, &s, fg_dim);
        }
        rx -= (9.0 * m.width).round(); // fixed 9-char slot

        // Mode flags (ALT / MOUSE) — shown only when non-standard.
        {
            let alt   = term_mode.contains(TermMode::ALT_SCREEN);
            let mouse = term_mode.intersects(TermMode::MOUSE_MODE);
            if alt || mouse {
                let tag = if alt { "ALT" } else { "MOUSE" };
                let w = renderer.text_width_px(tag);
                renderer.push_overlay_glyph_px_str((rx - w).round(), ty, tag, fg);
            }
            rx -= (8.0 * m.width).round(); // fixed 8-char slot (even when hidden)
        }
        let _ = rx; // git/cwd slots reserved here for future waves

        // 3) Left section: cwd (basename) and git branch. These fill the reserved
        //    left slots that were previously empty ("git/cwd slots reserved here").
        {
            let left_margin = m.width;
            let mut lx = left_margin;

            // cwd: last path component, or "~" for $HOME, or full path if short.
            if let Some(path) = cwd {
                let cwd_str: String = if path.as_os_str().is_empty() {
                    "~".to_string()
                } else {
                    // Show last two components for context (e.g. "glassy/src").
                    let components: Vec<_> = path.components().collect();
                    let n = components.len();
                    if n >= 2 {
                        format!("{}/{}",
                            components[n - 2].as_os_str().to_string_lossy(),
                            components[n - 1].as_os_str().to_string_lossy())
                    } else if n == 1 {
                        components[0].as_os_str().to_string_lossy().to_string()
                    } else {
                        "~".to_string()
                    }
                };
                let w = renderer.text_width_px(&cwd_str);
                renderer.push_overlay_glyph_px_str(lx.round(), ty, &cwd_str, fg_dim);
                lx += w + m.width;

                // Git branch (if in a repo): " branch_name" in accent.
                if let Some(branch) = git_branch {
                    let branch_str = format!("\u{E0A0} {branch}"); // nf-pl-branch glyph
                    let w = renderer.text_width_px(&branch_str);
                    renderer.push_overlay_glyph_px_str(lx.round(), ty, &branch_str, accent);
                    lx += w;
                }
            }
            let _ = lx;
        }

        // 4) OSC 9;4 progress indicator: a thin filled bar at the very bottom of
        //    the status bar (1px tall) spanning a fraction of the bar width, colored
        //    by state (accent = active, red = error, dim = indeterminate). Subtle and
        //    non-intrusive — it sits inside the status bar's existing pixel budget.
        if let Some(prog) = progress {
            use crate::image::ProgressState;
            let bar_bottom = bar_y + bar_h - 1.0; // 1px at the very bottom
            let (pct, color) = match prog {
                ProgressState::Set(p) => (p as f32 / 100.0, accent),
                ProgressState::Error(p) => (p as f32 / 100.0, mul(color::danger())),
                ProgressState::Indeterminate => (1.0, fg_dim),
                ProgressState::Remove => (0.0, fg_dim),
            };
            if pct > 0.0 {
                let prog_w = (bar_w * pct).max(2.0);
                renderer.push_overlay_px(0.0, bar_bottom, prog_w, 1.0, color);
            }
        }

        // 5) Left margin: a small decorative separator mark.
        renderer.push_overlay_px(0.0, bar_y, 1.0, bar_h, mul(gui::rail()));
    }

    /// Paint the inline tab-rename editor over the chip rect `r`: an opaque raised
    /// field with an accent ring, the in-progress `buffer` text (tail-clipped so the
    /// caret stays visible), and a block caret at the end. Associated (no `&self`)
    /// so it composes with the caller's `&mut Renderer` borrow.
    pub(crate) fn paint_tab_rename(renderer: &mut Renderer, r: gui::Rect, buffer: &str) {
        let m = renderer.cell_metrics();
        let cell_w = m.width;
        let cell_h = m.height;
        let radius = gui_radius(cell_h);

        // Opaque field surface so the chip text underneath never shows through.
        renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, radius, gui::glass_float());
        // Accent focus ring (1px): outer accent rrect minus an inset surface rrect.
        renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, radius, color::accent());
        let inset = 1.0;
        if r.w > 2.0 * inset && r.h > 2.0 * inset {
            renderer.push_overlay_rrect_px(
                r.x + inset,
                r.y + inset,
                r.w - 2.0 * inset,
                r.h - 2.0 * inset,
                (radius - inset).max(0.0),
                gui::glass_float(),
            );
        }

        // Text area: pad in, reserve one cell for the caret. Tail-clip so the END
        // of the buffer stays visible while typing (the natural caret position).
        let pad = (cell_w * 0.6).round();
        let ty = (r.center_y() - cell_h * 0.5).round();
        let text_w = (r.w - 2.0 * pad - cell_w).max(0.0);
        let max_chars = (text_w / cell_w).floor() as usize;
        let chars: Vec<char> = buffer.chars().collect();
        let visible: String = if chars.len() <= max_chars {
            buffer.to_string()
        } else if max_chars >= 1 {
            // Keep the tail; lead with an ellipsis. max_chars >= 1 here, so the
            // subtraction never underflows.
            let tail = &chars[chars.len() - (max_chars - 1)..];
            let mut s = String::from("…");
            s.extend(tail.iter());
            s
        } else {
            String::new()
        };
        let mut cx = r.x + pad;
        for ch in visible.chars() {
            renderer.push_overlay_glyph_px(cx.round(), ty, ch, gui::fg());
            cx += cell_w;
        }
        // Block caret immediately after the last visible glyph.
        renderer.push_overlay_px(cx.round(), ty, 2.0, cell_h, color::accent());
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
    ) {
        if is_ghost {
            return; // drawn separately as the lifted ghost
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
            // Connector: extends 4px below the bar bottom hairline, matching the
            // active-tab color, so the chip visually "opens into" the content surface.
            renderer.push_overlay_px(r.x, bar_h - 2.0, r.w, 4.0, active_surface);
            // Top accent rail (full accent opacity, 3px) crowns the active chip.
            renderer.push_overlay_px(r.x, r.y, r.w, 3.0, accent);
            // Side edge highlight: faint left/right 1px strips echo the accent rail.
            let side_a = [accent[0], accent[1], accent[2], accent[3] * 0.35];
            renderer.push_overlay_px(r.x, r.y + 3.0, 1.0, r.h - 3.0, side_a);
            renderer.push_overlay_px(r.x + r.w - 1.0, r.y + 3.0, 1.0, r.h - 3.0, side_a);
        } else {
            // Inactive: strongly recessed chip — clearly subordinate to the active tab.
            // Inset by 2px top and shrink 4px total height so it visually "recedes".
            let rr = gui::Rect::new(r.x, r.y + 3.0, r.w, r.h - 5.0);
            let fill = if held {
                // Press: briefly brighten to surface level.
                [surface[0], surface[1], surface[2], surface[3] * 0.70]
            } else if hover {
                // Hover: lift partway toward active.
                [surface[0], surface[1], surface[2], surface[3] * 0.55]
            } else {
                // Rest: very recessed (alpha 0.20) so active tab contrast is obvious.
                [surface[0], surface[1], surface[2], surface[3] * 0.20]
            };
            renderer.push_overlay_rrect_px(rr.x, rr.y, rr.w, rr.h, TAB_RADIUS, fill);
            // Bottom groove anchors inactive chip visually.
            if !hover && !held {
                let h = gui::hairline();
                renderer.push_overlay_px(rr.x, rr.y + rr.h - 1.0, rr.w, 1.0, [h[0], h[1], h[2], h[3] * 0.4]);
            }
            // Hover accent rail (dim) signals interactability.
            if hover && !held {
                renderer.push_overlay_px(rr.x, rr.y, rr.w, 1.0, [accent[0], accent[1], accent[2], accent[3] * 0.6]);
            }
        }
        let label_fg = if active { active_fg } else { fg_dim };
        Self::paint_tab_label(
            renderer, r, cell_h, cell_w, idx, label, active, busy, spinning, spin, label_fg,
            accent, multi,
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
    ) {
        let ty = (r.center_y() - cell_h * 0.5).round();
        let mut tx = r.x + TAB_PAD_X;
        // Leading status glyph: spinner while streaming, dot for background activity.
        if spinning {
            renderer.push_overlay_glyph_px(tx.round(), ty, spin, if active { label_fg } else { accent });
            tx += cell_w;
        } else if busy && !active {
            renderer.push_overlay_glyph_px(tx.round(), ty, '•', accent);
            tx += cell_w;
        }
        // Fit-to-width title (tail-ellipsized). A numeric prefix is shown only in
        // multi-tab mode (a lone tab is titled by the window). Reserve room for the
        // close box on the right when one exists (multi-tab only).
        let reserve = if multi { CLOSE_BOX + TAB_PAD_X } else { TAB_PAD_X };
        let text_w = (r.w - (tx - r.x) - reserve).max(0.0);
        let max_chars = (text_w / cell_w).floor() as usize;
        let s = if multi {
            // The "N " number prefix is shown before the title; reserve its width
            // (digits + a space) from max_chars BEFORE fitting the title, so the
            // composed string never overflows the chip (prefix + fit_label must be
            // ≤ max_chars together).
            let prefix = format!("{} ", idx + 1);
            let title_max = max_chars.saturating_sub(prefix.chars().count()).max(1);
            format!("{}{}", prefix, fit_label(label, title_max))
        } else {
            fit_label(label, max_chars.max(1))
        };
        renderer.push_overlay_glyph_px_str(tx.round(), ty, &s, label_fg);
    }

    /// Paint the settings form (§3.5) as a centered glass panel over a full-screen
    /// scrim, returning the interaction events for the caller to apply. Static (no
    /// `&self`) so it composes with the live `&mut Renderer` borrow held in
    /// `render`/`render_split`, threading the App-owned persistent GUI state.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_settings(
        renderer: &mut Renderer,
        config: &Config,
        font_px: f32,
        bell_idx: usize,
        font_choices: &[String],
        font_idx: usize,
        config_path: &str,
        open: gui::SettingsDrop,
        saved: bool,
        mouse: (f32, f32),
        mouse_down: bool,
        clicked: bool,
        gui_pressed: &mut Option<gui::WidgetId>,
        gui_focused: &mut Option<gui::WidgetId>,
        gui_anims: &mut std::collections::HashMap<gui::WidgetId, gui::Anim>,
    ) -> gui::SettingsEvents {
        // Theme names + per-theme accent swatches (the cursor color each theme
        // deliberately picks to pop).
        let theme_names = color::THEME_NAMES;
        let swatches: Vec<[f32; 4]> = theme_names
            .iter()
            .map(|n| match color::theme_by_name(n) {
                Some(t) => [
                    t.cursor.r as f32 / 255.0,
                    t.cursor.g as f32 / 255.0,
                    t.cursor.b as f32 / 255.0,
                    1.0,
                ],
                None => color::accent(),
            })
            .collect();
        let theme_idx = theme_names
            .iter()
            .position(|&n| n == config.theme)
            .unwrap_or(0);
        let font_refs: Vec<&str> = font_choices.iter().map(|s| s.as_str()).collect();
        let font_display = config.font_family.as_deref().unwrap_or("default");

        let (sw, sh) = renderer.surface_size();
        let (cw, ch) = {
            let m = renderer.cell_metrics();
            (m.width, m.height)
        };
        let mut ui = gui::Ui::new(
            renderer,
            cw,
            ch,
            mouse,
            mouse_down,
            clicked,
            gui_pressed,
            gui_focused,
            gui_anims,
        );
        let view = gui::SettingsView {
            font_px,
            opacity: config.opacity,
            bell: bell_idx,
            theme_idx,
            theme_names,
            theme_swatches: &swatches,
            font_family: font_display,
            font_names: &font_refs,
            font_idx,
            scrollback: config.scrollback,
            config_path,
            open,
            saved,
            status_bar: config.status_bar,
            pane_headers: config.pane_headers,
        };
        ui.build_settings((sw as f32, sh as f32), &view)
    }

    /// Apply the settings-form events to the live config + renderer + theme. Runs
    /// after `paint_settings` (the `Ui` borrow is dropped), driving the existing
    /// effects so opacity / font / theme preview immediately. Requests a repaint
    /// directly via the window (no `event_loop` is available inside `render`).
    pub(crate) fn apply_settings_events(&mut self, ev: gui::SettingsEvents) {
        // Remember the panel bounds for click-outside dismissal next frame.
        self.settings_panel = ev.panel;
        let mut changed = false;
        if ev.font_delta > 0 {
            self.resize_font(FontStep::Inc);
            changed = true;
        } else if ev.font_delta < 0 {
            self.resize_font(FontStep::Dec);
            changed = true;
        }
        if let Some(o) = ev.opacity {
            self.config.opacity = o;
            if let Some(r) = self.renderer.as_mut() {
                r.set_opacity(o);
            }
            changed = true;
        }
        if let Some(b) = ev.bell {
            self.set_bell_index(b);
            changed = true;
        }
        if ev.theme_toggle {
            self.settings_drop = if self.settings_drop == gui::SettingsDrop::Theme {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Theme
            };
            changed = true;
        }
        if let Some(t) = ev.theme_pick {
            self.set_theme_by_idx(t);
            self.settings_drop = gui::SettingsDrop::None;
            changed = true;
        }
        if ev.font_toggle {
            self.settings_drop = if self.settings_drop == gui::SettingsDrop::Font {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Font
            };
            changed = true;
        }
        if let Some(f) = ev.font_pick {
            self.set_font_family_index(f);
            self.settings_drop = gui::SettingsDrop::None;
            changed = true;
        }
        if ev.scrollback_delta != 0 {
            self.adjust_scrollback(ev.scrollback_delta);
            changed = true;
        }
        if ev.status_bar_toggle {
            self.toggle_status_bar();
            changed = true;
        }
        if ev.pane_headers_toggle {
            self.toggle_pane_headers();
            changed = true;
        }
        if ev.copy_path {
            self.copy_config_path();
            changed = true;
        }
        if ev.open_path {
            self.open_config_path();
        }
        if ev.save {
            self.save_settings();
            changed = true;
        }
        if ev.close {
            self.settings_open = false;
            self.settings_drop = gui::SettingsDrop::None;
            changed = true;
        }
        if changed {
            self.force_full_redraw = true;
            self.dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

}
