//! Multi-pane (split) render path, pane headers, and push_pane.

use super::*;

impl App {
    /// Render a split (multi-pane) tab via the renderer's scissored multi-pane
    /// path: one pane per leaf clipped to its tile, the tab strip on top, the
    /// focused-pane border, and dividers between tiles. Forgoes the per-row damage
    /// machinery (rebuilds every frame) — splitting is rare and this keeps the
    /// fast single-pane path untouched.
    pub(crate) fn render_split(&mut self) {
        let flash = if self.bell_flash_until.is_some_and(|t| Instant::now() < t) {
            Some(bell::FLASH_COLOR)
        } else {
            None
        };
        let Some(area) = self.content_area() else {
            return;
        };
        let focused_pane = match self.panes.as_ref() {
            Some(g) => g.layout.focused(),
            None => return,
        };
        // Precompute every leaf's rect + grid size (whole-`self` method calls)
        // BEFORE taking disjoint field borrows for the render loop below.
        let rects = self
            .panes
            .as_ref()
            .unwrap()
            .layout
            .rects(area, Self::PANE_GAP);
        // Per-pane header chrome is runtime-configurable. When off, panes get their
        // full height and no header is painted (hdr_h == 0 collapses the body inset).
        let pane_headers = self.config.pane_headers;
        let hdr_h = if pane_headers { Self::PANE_HEADER_H } else { 0 };
        // Each pane_spec carries: (id, full_rect, body_rect, cols, rows).
        // The body_rect is the full rect minus the PANE_HEADER_H header at the top;
        // `pane_grid` and `begin_pane` receive the body rect so the cell grid
        // starts below the header. `full_rect` is kept for header painting.
        let pane_specs: Vec<(usize, pane::Rect, pane::Rect, usize, usize)> = rects
            .iter()
            .map(|(id, r)| {
                let body = pane::Rect {
                    x: r.x,
                    y: r.y + hdr_h,
                    w: r.w,
                    h: (r.h - hdr_h).max(0),
                };
                let (c, rw) = self.pane_grid(body);
                (*id, *r, body, c, rw)
            })
            .collect();

        // Focused pane's scroll position for the strip % readout.
        let (strip_off, strip_hist) = match self.pty.as_ref() {
            Some(pty) => {
                let t = pty.term.lock();
                (t.grid().display_offset() as i32, t.grid().history_size())
            }
            None => (0, 0),
        };

        // Status-bar snapshot: term mode, scroll position, selection count.
        // All taken here under the immutable `&self` borrow.
        let (sb_mode, sb_disp_off, sb_hist, sb_sel_len) = match self.pty.as_ref() {
            Some(pty) => {
                let t = pty.term.lock();
                let mode = *t.mode();
                let disp = t.grid().display_offset() as i32;
                let hist = t.grid().history_size();
                let sel = t
                    .selection_to_string()
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
                (mode, disp, hist, sel)
            }
            None => (TermMode::empty(), 0, 0, 0),
        };
        let sb_focused = self.focused;
        let sb_surface_h = self
            .renderer
            .as_ref()
            .map(|r| r.surface_size().1)
            .unwrap_or(0);
        // Status-bar cwd + git branch (same as single-pane path in render.rs).
        let sb_cwd: Option<std::path::PathBuf> = self
            .pty
            .as_ref()
            .and_then(|p| p.pane_info.cwd.clone())
            .or_else(|| self.active_cwd.clone());
        // Branch is precomputed in PaneInfo (refreshed on the 2 s proc poll).
        let sb_git_branch: Option<String> = self
            .pty
            .as_ref()
            .and_then(|p| p.pane_info.git_branch.clone());
        let sb_progress = self.active_progress;

        // Tab-bar state snapshot (owned data) for the pixel-overlay painter, taken
        // under the immutable `&self` borrow.
        let tab_snapshot = self.tab_bar_snapshot();
        let rename_inputs = self.tab_rename_state().and_then(|(pos, buf, caret, sel)| {
            self.tab_layout()
                .into_iter()
                .find(|s| s.item == StripItem::Tab(pos))
                .map(|s| (s.rect, buf, caret, sel))
        });
        let tab_focused = self.focused;
        let tab_hovered = self.hovered_strip_item;
        let tab_held = self.held_strip_item;
        let tab_dragging = self.dragging_tab;
        let tab_mouse = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
        let tab_spinner = self.spinner_frame;
        let tab_count = self.tab_count();
        let win_focused = self.focused;
        let blink_on = self.blink_on;
        let hovered_link = self.hovered_link.clone();

        // Pane-header snapshot: per-leaf titles + proc info (cwd, foreground comm).
        // The proc info is read from the cached PaneInfo on each pane's Pty so we
        // never hold a term lock here. OSC title is still used as the primary title
        // (fallback = empty); the cwd and comm are displayed as a secondary subtitle.
        let pane_header_titles: Vec<(usize, String)> = {
            let g = self.panes.as_ref().unwrap();
            pane_specs
                .iter()
                .map(|(id, _, _, _, _)| {
                    let title = g.others_titles.get(id).cloned().unwrap_or_default();
                    (*id, title)
                })
                .collect()
        };
        // Per-pane cwd + foreground comm subtitle for the pane header.
        // Tuple: (pane_id, cwd_last_component, Option<comm>)
        let pane_header_proc: Vec<(usize, Option<String>, Option<String>)> = {
            let g = self.panes.as_ref().unwrap();
            let focused_pane_id = g.layout.focused();
            pane_specs
                .iter()
                .map(|(id, _, _, _, _)| {
                    let pty = if *id == focused_pane_id {
                        self.pty.as_ref()
                    } else {
                        g.others.get(id)
                    };
                    let (cwd, comm) = pty
                        .map(|p| {
                            let cwd = p.pane_info.cwd.as_ref().map(|path| {
                                // Show only the last component (basename) of the path to fit
                                // the narrow header. The full path is in the status bar.
                                path.file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("~")
                                    .to_string()
                            });
                            let comm = p.pane_info.foreground_comm.clone();
                            (cwd, comm)
                        })
                        .unwrap_or((None, None));
                    (*id, cwd, comm)
                })
                .collect()
        };
        let pane_menu_open = self.pane_menu_open;
        let pane_menu_sel = self.pane_menu_sel;
        let mouse_px_f = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
        let divider = lighten(color::selection_bg(), 0.18);
        // The gutter to draw transiently emphasised (drag wins over mere hover).
        let active_gutter = self
            .dragging_gutter
            .clone()
            .or_else(|| self.hovered_gutter.clone());
        // Its divider's primary-axis pixel position, derived from the live layout,
        // so the highlight tracks the exact line being hovered/dragged.
        let active_div = active_gutter.as_ref().and_then(|h| {
            // Re-resolve the handle's divider coordinate from the live layout so
            // the highlight tracks the exact line being hovered/dragged.
            self.panes
                .as_ref()?
                .layout
                .divider_pos(area, Self::PANE_GAP, &h.path)
                .map(|pos| (h.dir, pos))
        });
        let gutter_glow = {
            let mut c = color::accent();
            c[3] = 0.8;
            c
        };
        let gutter_tol = Self::GUTTER_TOL;

        // Settings-form inputs (whole-`self` method calls) snapshotted BEFORE the
        // disjoint field borrows below, so the form can be painted via the live
        // renderer borrow without routing through `self`.
        let settings_inputs = if self.settings_open {
            Some((
                self.font_family_choices(),
                self.font_family_index(),
                self.bell_index(),
                config_display_path(),
                self.settings_drop,
                self.settings_saved,
                (self.mouse_px.0 as f32, self.mouse_px.1 as f32),
                self.held_button == Some(0),
                self.gui_click_edge,
            ))
        } else {
            None
        };

        // Menu/help overlay snapshot (same as the single-pane path).
        let has_selection_for_menu2 = self
            .pty
            .as_ref()
            .and_then(|p| p.term.lock().selection_to_string())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let menu_snapshot2 = if self.menu_open {
            let actions: &[MenuAction] = self.menu_items.as_deref().unwrap_or(MenuAction::ALL);
            let entries = actions_to_entries(actions, has_selection_for_menu2);
            let (ax, ay) = self.menu_anchor_px.unwrap_or_else(|| {
                let (left, top) = self.menu_anchor.unwrap_or((0, TAB_STRIP_ROWS));
                if let Some(r) = self.renderer.as_ref() {
                    let m2 = r.cell_metrics();
                    let pad = r.pad();
                    (left as f32 * m2.width + pad, top as f32 * m2.height + pad)
                } else {
                    (left as f32 * 8.0, top as f32 * 16.0)
                }
            });
            Some((
                entries,
                ax,
                ay,
                self.menu_sel,
                (self.mouse_px.0 as f32, self.mouse_px.1 as f32),
                self.held_button == Some(0),
                self.gui_click_edge,
            ))
        } else {
            None
        };

        // Tab right-click context menu snapshot (drawn after the global menu).
        let tab_menu_snapshot2 = self.tab_menu_snapshot().map(|(entries, ax, ay, sel)| {
            (
                entries,
                ax,
                ay,
                sel,
                (self.mouse_px.0 as f32, self.mouse_px.1 as f32),
                self.held_button == Some(0),
                self.gui_click_edge,
            )
        });

        // Find-bar + palette inputs, snapshotted BEFORE the disjoint borrows
        // (both may lock the focused term). The find-bar highlights are positioned
        // relative to the focused pane's body rect (its on-screen pixel origin).
        let search_inputs = self.search_readout().map(|r| (r, self.search_highlights()));
        let search_origin = {
            // The focused pane's body rect origin + pad, mirroring px_to_cell.
            let pad = self.renderer.as_ref().map(|r| r.pad()).unwrap_or(0.0);
            pane_specs
                .iter()
                .find(|(id, ..)| *id == focused_pane)
                .map(|(_, _full, body, _, _)| (body.x as f32 + pad, body.y as f32 + pad))
                .unwrap_or((pad, pad))
        };
        let palette_inputs = self.palette_snapshot();

        // Damage/incremental decision for each pane, made BEFORE the disjoint
        // borrows (it locks the panes' terms). A pane is rebuilt only when:
        //   * a full redraw is forced (layout change / resize / theme / toggle), OR
        //   * it is the focused pane (it owns the blinking cursor + selection, which
        //     are NOT part of alacritty's damage and so must repaint every frame), OR
        //   * it has no cached instances yet (first split frame after a change), OR
        //   * its child produced output since the last frame (term damage).
        // Every other pane is reused verbatim from the renderer's per-pane cache,
        // which skips the expensive term-lock + cell-iterate + glyph-shape rebuild.
        // `pane_damaged` reads+resets damage, so it must run for every non-focused
        // pane exactly once per frame regardless of the rebuild decision (otherwise
        // damage would accumulate and the pane would never settle).
        let force_full = self.force_full_redraw;
        // Tab-bar incremental decision: rebuild only when its inputs changed (or a
        // full redraw is forced, e.g. theme change). Computed here so it can update
        // `self.tab_bar_key` after the renderer borrow ends.
        let tab_pane_counts = self.tab_pane_counts();
        let tab_active_pos = self.active_pos();
        let tab_strip_visible = self.tab_bar_visible();
        let new_tab_key = Self::tab_bar_key(
            &tab_snapshot,
            tab_focused,
            tab_hovered,
            tab_held,
            tab_dragging,
            tab_mouse,
            tab_spinner,
            tab_count,
            strip_off,
            strip_hist,
            &tab_pane_counts,
            tab_active_pos,
        );
        let tab_bar_rebuild = force_full
            || self.tab_bar_key != Some(new_tab_key)
            || !self
                .renderer
                .as_ref()
                .map(|r| r.has_tab_overlay())
                .unwrap_or(false);
        let live_ids: Vec<usize> = pane_specs.iter().map(|(id, ..)| *id).collect();
        let rebuild: Vec<bool> = {
            let g = self.panes.as_ref().unwrap();
            pane_specs
                .iter()
                .map(|(id, ..)| {
                    let is_focused = *id == focused_pane;
                    let has_cache = self
                        .renderer
                        .as_ref()
                        .map(|r| r.has_cached_pane(*id))
                        .unwrap_or(false);
                    // Resolve the pane's pty to read its damage (always, to reset it).
                    let pty = if is_focused {
                        self.pty.as_ref()
                    } else {
                        g.others.get(id)
                    };
                    let damaged = pty.map(Self::pane_damaged).unwrap_or(true);
                    force_full || is_focused || !has_cache || damaged
                })
                .collect()
        };

        // Disjoint field borrows: `renderer` (mut), `panes`/`pty` (shared) are
        // distinct fields, so the borrow checker allows them together as long as
        // we don't route through a whole-`self` method past this point.
        let g = self.panes.as_ref().unwrap();
        let focused_pty = self.pty.as_ref();
        let renderer = match self.renderer.as_mut() {
            Some(r) => r,
            None => return,
        };
        renderer.set_flash(flash);
        // Multi-pane: each pane carries its own pixel origin (content_area already
        // insets below the tab bar), so the per-cell grid_origin_y must be zero.
        renderer.set_grid_origin_y(0.0);
        renderer.begin_multi_frame(color::default_bg());
        // Evict cache entries for panes that no longer exist (closed/merged).
        renderer.retain_panes(&live_ids);

        // Each leaf pane, clipped to its body rect (below the header). Changed panes
        // are rebuilt; unchanged panes are re-emitted from cache (the typing-lag fix).
        for (i, (id, _full_rect, body_rect, cols, prows)) in pane_specs.iter().enumerate() {
            let is_focused = *id == focused_pane;
            if rebuild[i] || !renderer.has_cached_pane(*id) {
                let pty = if is_focused {
                    focused_pty
                } else {
                    g.others.get(id)
                };
                let Some(pty) = pty else { continue };
                renderer.begin_pane(*id, *body_rect, is_focused);
                Self::push_pane(
                    renderer,
                    pty,
                    *cols,
                    *prows,
                    win_focused,
                    blink_on,
                    hovered_link.as_deref(),
                );
                renderer.end_pane();
            } else {
                renderer.reuse_pane(*id);
            }
        }

        // Dividers in the gutters between adjacent tiles (drawn full-surface
        // scissored so they are never clipped by a pane rect).
        if Self::PANE_GAP > 0 {
            for (i, (_, a)) in rects.iter().enumerate() {
                for (_, b) in rects.iter().skip(i + 1) {
                    // Vertical gutter: b sits to the right of a, sharing a column.
                    if b.x == a.x + a.w + Self::PANE_GAP {
                        let y0 = a.y.max(b.y);
                        let y1 = (a.y + a.h).min(b.y + b.h);
                        if y1 > y0 {
                            let dx = a.x + a.w;
                            // Hovered/dragged gutter: brighter + fatter (±tol).
                            if active_div == Some((pane::Dir::Vertical, dx)) {
                                renderer.push_divider(
                                    dx - gutter_tol,
                                    y0,
                                    Self::PANE_GAP + 2 * gutter_tol,
                                    y1 - y0,
                                    gutter_glow,
                                );
                            } else {
                                renderer.push_divider(dx, y0, Self::PANE_GAP, y1 - y0, divider);
                            }
                        }
                    }
                    // Horizontal gutter: b sits below a, sharing a row.
                    if b.y == a.y + a.h + Self::PANE_GAP {
                        let x0 = a.x.max(b.x);
                        let x1 = (a.x + a.w).min(b.x + b.w);
                        if x1 > x0 {
                            let dy = a.y + a.h;
                            if active_div == Some((pane::Dir::Horizontal, dy)) {
                                renderer.push_divider(
                                    x0,
                                    dy - gutter_tol,
                                    x1 - x0,
                                    Self::PANE_GAP + 2 * gutter_tol,
                                    gutter_glow,
                                );
                            } else {
                                renderer.push_divider(
                                    x0,
                                    a.y + a.h,
                                    x1 - x0,
                                    Self::PANE_GAP,
                                    divider,
                                );
                            }
                        }
                    }
                }
            }
        }

        // Real GUI tab bar over the pixel band [0, tab_bar_h), composited over the
        // split via the overlay pass added to record_multi_passes. Rebuilt only when
        // its inputs changed; otherwise the cached overlay is replayed (no glyph
        // re-shaping) — the second half of the split typing-lag fix.
        if !tab_strip_visible {
            renderer.begin_tab_overlay();
            renderer.commit_tab_overlay();
        } else if tab_bar_rebuild {
            renderer.begin_tab_overlay();
            Self::paint_tab_bar(
                renderer,
                &tab_snapshot,
                tab_focused,
                tab_hovered,
                tab_held,
                tab_dragging,
                tab_mouse,
                tab_spinner,
                tab_count,
                strip_off,
                strip_hist,
                &tab_pane_counts,
                tab_active_pos,
            );
            renderer.commit_tab_overlay();
        } else {
            renderer.replay_tab_overlay();
        }

        // Inline tab-rename editor, drawn over its chip on top of the tab bar.
        if let Some((rect, buf, caret, sel)) = &rename_inputs {
            Self::paint_tab_rename(renderer, *rect, buf, *caret, *sel);
        }

        // Pane title bars: one per leaf, drawn as overlay quads+glyphs so they
        // composite above the cell grid but below the tab bar. Single-pane case
        // is handled by the caller never reaching render_split, so zero cost.
        // Gated on the runtime-configurable `pane_headers` setting.
        if pane_headers {
            Self::paint_pane_headers(
                renderer,
                &pane_specs,
                &pane_header_titles,
                &pane_header_proc,
                focused_pane,
                win_focused,
                pane_menu_open,
                pane_menu_sel,
                mouse_px_f,
            );
        }

        // Status bar (§3.4): same as the single-pane path.
        // Only painted when enabled in the config.
        if self.config.status_bar {
            Self::paint_status_bar(
                renderer,
                sb_surface_h,
                sb_mode,
                sb_disp_off,
                sb_hist,
                sb_sel_len,
                sb_focused,
                sb_cwd.as_deref(),
                sb_git_branch.as_deref(),
                sb_progress,
            );
        }

        // Settings form (§3.5): drawn over the split via the overlay pipeline,
        // events captured here (disjoint `&self.config` borrow) and applied after
        // the GPU submit below.
        let mut settings_events: Option<gui::SettingsEvents> = None;
        if let Some((
            ref font_choices,
            font_idx,
            bell_idx,
            ref cfg_path,
            drop,
            saved,
            mouse,
            mouse_down,
            click_edge,
        )) = settings_inputs
        {
            let font_px = renderer.font_px();
            let mut fields = gui::SettingsFields {
                word_sep: &mut self.settings_word_sep,
                word_sep_ms: &mut self.settings_word_sep_ms,
                font_feat: &mut self.settings_font_feat,
                font_feat_ms: &mut self.settings_font_feat_ms,
                blink_on: self.blink_on,
                double_click: self.gui_double_click,
            };
            settings_events = Some(Self::paint_settings(
                renderer,
                &self.config,
                font_px,
                bell_idx,
                font_choices,
                font_idx,
                cfg_path,
                drop,
                saved,
                mouse,
                mouse_down,
                click_edge,
                &mut self.gui_pressed,
                &mut self.gui_focused,
                &mut self.gui_anims,
                &mut fields,
            ));
        } else if self.help_open {
            // Real GUI help panel (§3.7) in split mode.
            let (sw, sh) = renderer.surface_size();
            let m = renderer.cell_metrics();
            let mouse = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
            let help_result = gui::build_help(
                renderer,
                m.width,
                m.height,
                (sw as f32, sh as f32),
                mouse,
                self.gui_click_pos,
                self.held_button == Some(0),
                self.gui_click_edge,
                self.overlay_opened_by_press,
                &mut self.gui_pressed,
                &mut self.gui_focused,
                &mut self.gui_anims,
                &mut self.help_state,
                &self.config.keymap,
                crate::config::Platform::display_override(),
            );
            if help_result.close {
                self.help_open = false;
                self.overlay_opened_by_press = false;
                self.force_full_redraw = true;
            }
        }

        // Real GUI menu (§3.6) in split mode.
        if let Some((ref entries2, ax2, ay2, sel2, mouse2, md2, click2)) = menu_snapshot2 {
            let m = renderer.cell_metrics();
            let _ = gui::menu(
                renderer, m.width, m.height, mouse2, md2, click2, ax2, ay2, entries2, sel2,
            );
        }

        // Tab right-click context menu in split mode (drawn after the global menu).
        if let Some((ref entries, ax, ay, sel_item, mouse, md, click)) = tab_menu_snapshot2 {
            let m = renderer.cell_metrics();
            let _ = gui::menu(
                renderer, m.width, m.height, mouse, md, click, ax, ay, entries, sel_item,
            );
        }

        // Find bar + match highlights (Ctrl+Shift+F) in split mode. Highlights are
        // anchored to the focused pane's body rect (search targets the focused pane).
        if let Some(((query, caret, selection, count, current, bad_regex), highlights)) =
            &search_inputs
        {
            let (sw, sh) = renderer.surface_size();
            Self::paint_search(
                renderer,
                (sw as f32, sh as f32),
                search_origin,
                query,
                *caret,
                *selection,
                *count,
                *current,
                *bad_regex,
                highlights,
            );
        }

        // Command palette (Ctrl+Shift+P) in split mode: topmost modal.
        if let Some((query, caret, selection, rows, sel)) = &palette_inputs {
            let (sw, sh) = renderer.surface_size();
            let row_refs: Vec<(&str, Option<&str>)> =
                rows.iter().map(|(l, h)| (l.as_str(), *h)).collect();
            self.palette_rows = Self::paint_palette(
                renderer,
                (sw as f32, sh as f32),
                query,
                *caret,
                *selection,
                &row_refs,
                *sel,
                mouse_px_f,
            );
        }

        // This frame consumed the forced-full-redraw request (every pane was
        // rebuilt above when it was set). Clear it so subsequent split frames take
        // the incremental path; the drop-frame branch below re-arms it on error.
        self.force_full_redraw = false;
        self.tab_bar_key = Some(new_tab_key);
        if let Err(err) = renderer.render_multi() {
            log::debug!("split frame dropped: {err:?}");
            self.force_full_redraw = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }

        // The chrome paint consumed this frame's click edge (the single-pane path
        // resets it too); on a release edge also drop the press latch. Clear
        // `overlay_opened_by_press` alongside it (see the single-pane reset for the
        // rationale): the opening gesture's release is now fully consumed.
        if self.gui_click_edge {
            self.gui_pressed = None;
            self.overlay_opened_by_press = false;
        }
        self.gui_click_edge = false;
        // Consume the chrome double-click edge (one frame of word-select).
        self.gui_double_click = false;

        // Cache the focused pane's blink state (single lock here, on an actual
        // repaint) so about_to_wait never takes the term lock per event.
        self.cursor_blinks = self
            .pty
            .as_ref()
            .map(|pty| {
                let term = pty.term.lock();
                term.cursor_style().blinking
                    && term.renderable_content().cursor.shape != CursorShape::Hidden
            })
            .unwrap_or(false);

        // Apply settings-form interactions now the renderer borrow has ended.
        if let Some(ev) = settings_events {
            self.apply_settings_events(ev);
        }
    }

    /// Pane-menu entries (for the ⋮ button in each pane header). Kept as a
    /// static slice so the menu shape is stable and hit-testing is index-based.
    pub(crate) const PANE_MENU_ITEMS: &'static [&'static str] =
        &["Split vertical", "Split horizontal", "Close pane"];

    /// Paint per-pane title bars for all leaves in split mode. Each header is
    /// `PANE_HEADER_H` px tall at the top of the leaf rect and contains (L→R):
    ///
    ///   · focus dot (●/·)  · OSC title (ellipsized)  · [cwd slot, reserved]
    ///   · ⋮ pane-menu button (opens a mini dropdown)
    ///
    /// The focused header is drawn with the E2 glass surface fill + a 2 px accent
    /// top rail (focus chrome). Unfocused headers use the E1 body + dimmed text.
    /// When the ⋮ menu is open for a pane, a small E3 dropdown is drawn below the
    /// ⋮ button containing Split V / Split H / Close pane entries.
    ///
    /// Associated fn (no `&self`) so it composes with the caller's `&mut Renderer`
    /// borrow; all `self`-derived data arrives via parameters.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_pane_headers(
        renderer: &mut Renderer,
        pane_specs: &[(usize, pane::Rect, pane::Rect, usize, usize)],
        titles: &[(usize, String)],
        proc_info: &[(usize, Option<String>, Option<String>)],
        focused_pane: usize,
        win_focused: bool,
        pane_menu_open: Option<usize>,
        pane_menu_sel: usize,
        mouse_px: (f32, f32),
    ) {
        let m = renderer.cell_metrics();
        let hdr_h = Self::PANE_HEADER_H as f32;
        let pad = Self::PANE_HEADER_PAD;

        // Dim everything when window is unfocused (matches tab-bar behaviour).
        let fdim = if win_focused { 1.0 } else { 0.7 };
        let mul = |c: [f32; 4]| [c[0] * fdim, c[1] * fdim, c[2] * fdim, c[3]];

        let accent = mul(color::accent());
        let fg = mul(gui::fg());
        let fg_dim = mul(gui::fg_dim());
        let body_e1 = mul(gui::glass_body());
        let body_e2 = mul(gui::glass_raised());
        let hairline = mul(gui::hairline());

        // ⋮ button: a square hit-target flush to the right edge of each header.
        let menu_btn_w = hdr_h; // square

        for (id, full_rect, _body_rect, _cols, _rows) in pane_specs {
            let is_focused = *id == focused_pane;
            let rx = full_rect.x as f32;
            let ry = full_rect.y as f32;
            let rw = full_rect.w as f32;

            // Header fill: E2 (focused) or E1 (unfocused).
            let fill = if is_focused { body_e2 } else { body_e1 };
            renderer.push_overlay_px(rx, ry, rw, hdr_h, fill);

            // Soft accent crown on the focused header (low alpha, no harsh bright
            // edge line). Unfocused headers get no top line at all so edges stay
            // clean; focus is carried by the brighter E2 fill + soft crown.
            if is_focused {
                let crown = [accent[0], accent[1], accent[2], accent[3] * 0.5];
                renderer.push_overlay_px(rx, ry, rw, 2.0, crown);
            }

            // Soft bottom seam separating header from cell body.
            renderer.push_overlay_px(rx, ry + hdr_h - 1.0, rw, 1.0, hairline);

            // Resolve the per-pane title from the snapshot.
            let title = titles
                .iter()
                .find(|(tid, _)| *tid == *id)
                .map(|(_, t)| t.as_str())
                .unwrap_or("");
            let dot = if is_focused { '●' } else { '·' };
            let dot_fg = if is_focused { accent } else { fg_dim };
            let text_fg = if is_focused { fg } else { fg_dim };

            // Vertical centering: glyph top = (hdr_h - cell_h) / 2.
            let ty = (ry + (hdr_h - m.height) * 0.5).round();

            // /proc cwd + foreground comm for this pane.
            let (pi_cwd, pi_comm) = proc_info
                .iter()
                .find(|(tid, _, _)| *tid == *id)
                .map(|(_, c, k)| (c.as_deref(), k.as_deref()))
                .unwrap_or((None, None));
            // Build a compact right-side annotation: "cwd" or "cwd  comm" (dim).
            // Reserve space for this annotation before computing the title width.
            let annotation: Option<String> = match (pi_cwd, pi_comm) {
                (Some(c), Some(k)) => Some(format!("{c}  {k}")),
                (Some(c), None) => Some(c.to_string()),
                (None, Some(k)) => Some(k.to_string()),
                (None, None) => None,
            };
            // Annotation goes right-aligned, just left of the ⋮ button.
            let annotation_w = annotation
                .as_deref()
                .map(|s| {
                    let nchars = s.chars().count() as f32;
                    nchars * m.width + pad
                })
                .unwrap_or(0.0);

            // Focus dot.
            let mut tx = rx + pad;
            renderer.push_overlay_glyph_px(tx.round(), ty, dot, dot_fg);
            tx += m.width + 2.0;

            // Title (fit to available width, leaving room for annotation + ⋮ button).
            let avail = rw - (tx - rx) - annotation_w - menu_btn_w - pad;
            let max_chars = (avail / m.width).floor() as usize;
            let label = fit_label(title, max_chars.max(1));
            renderer.push_overlay_glyph_px_str(tx.round(), ty, &label, text_fg);

            // Proc annotation (cwd + comm): dim, right-aligned before ⋮.
            if let Some(ann) = &annotation {
                let ann_x = rx + rw - menu_btn_w - annotation_w;
                renderer.push_overlay_glyph_px_str(ann_x.round(), ty, ann, fg_dim);
            }

            // ⋮ pane-menu button (right-aligned in the header).
            let btn_x = rx + rw - menu_btn_w;
            let btn_y = ry;
            let is_menu_open = pane_menu_open == Some(*id);
            // Hover highlight when the pointer is inside the button.
            let btn_hovered = gui::hit(
                gui::Rect::new(btn_x, btn_y, menu_btn_w, hdr_h),
                mouse_px.0,
                mouse_px.1,
            );
            if btn_hovered || is_menu_open {
                let hi = [
                    accent[0],
                    accent[1],
                    accent[2],
                    if is_menu_open { 0.25 } else { 0.15 },
                ];
                renderer.push_overlay_rrect_px(
                    btn_x + 2.0,
                    btn_y + 2.0,
                    menu_btn_w - 4.0,
                    hdr_h - 4.0,
                    3.0,
                    hi,
                );
            }
            let glyph_fg = if btn_hovered || is_menu_open {
                accent
            } else {
                fg_dim
            };
            let gx = btn_x + (menu_btn_w - m.width) * 0.5;
            renderer.push_overlay_glyph_px(gx.round(), ty, '⋯', glyph_fg);

            // If this pane's ⋮ menu is open, draw the dropdown below the button.
            if is_menu_open {
                Self::paint_pane_menu(renderer, btn_x, btn_y + hdr_h, pane_menu_sel, mouse_px, &m);
            }
        }
    }

    /// Draw the small pane ⋮ menu anchored at `(ax, ay)` (top-left of the dropdown).
    /// Entries: "Split vertical" / "Split horizontal" / "Close pane".
    pub(crate) fn paint_pane_menu(
        renderer: &mut Renderer,
        ax: f32,
        ay: f32,
        sel: usize,
        mouse_px: (f32, f32),
        m: &crate::text::CellMetrics,
    ) {
        let items = Self::PANE_MENU_ITEMS;
        let max_label = items.iter().map(|s| s.len()).max().unwrap_or(4) as f32;
        let panel_w = (max_label * m.width + 24.0).ceil();
        let row_h = (m.height + 6.0).ceil();
        let panel_h = items.len() as f32 * row_h + 4.0;

        // Edge-clamp: keep the panel inside the rendered surface. The surface
        // width is available via the renderer; clamp left so the right edge
        // stays on-screen, and never go left of 0. This prevents the menu on
        // the right pane from extending past the window boundary.
        let (sw, _sh) = renderer.surface_size();
        let ax = ax.min(sw as f32 - panel_w).max(0.0);

        // E3 floating panel with a soft rounded accent border carved as an
        // outer-minus-inner rrect, so it follows the rounded shape and reads as a
        // gentle halo instead of a hard 4-edge box of bright lines.
        let float_fill = gui::glass_float();
        let border = {
            let a = color::accent();
            [a[0], a[1], a[2], 0.22]
        };
        renderer.push_overlay_rrect_px(ax, ay, panel_w, panel_h, 4.0, border);
        renderer.push_overlay_rrect_px(
            ax + 1.0,
            ay + 1.0,
            panel_w - 2.0,
            panel_h - 2.0,
            3.0,
            float_fill,
        );

        let fg = gui::fg();
        let sel_bg = gui::sel_bg();

        for (i, label) in items.iter().enumerate() {
            let row_y = ay + 2.0 + i as f32 * row_h;
            // Highlight hovered or keyboard-selected row.
            let mouse_on_row = gui::hit(
                gui::Rect::new(ax, row_y, panel_w, row_h),
                mouse_px.0,
                mouse_px.1,
            );
            if mouse_on_row || i == sel {
                // Rounded highlight inset from the panel edge so it doesn't square
                // off over the panel's own rounded corners (matches gui::menu).
                renderer.push_overlay_rrect_px(ax + 2.0, row_y, panel_w - 4.0, row_h, 3.0, sel_bg);
            }
            let ty = (row_y + (row_h - m.height) * 0.5).round();
            renderer.push_overlay_glyph_px_str((ax + 8.0).round(), ty, label, fg);
        }
    }

    /// Author one pane's terminal grid into the renderer's current pane (between
    /// `begin_pane`/`end_pane`) using LOCAL `(col, row)` coords. A self-contained
    /// version of the single-pane cell loop: full rebuild (no damage), cells +
    /// selection + cursor overlay. `win_focused`/`blink_on` drive the cursor
    /// style; `hovered_link` underlines the hovered OSC8 link.
    #[allow(clippy::too_many_arguments)]
    /// Whether a (non-focused) pane's terminal changed since the last split frame.
    /// Reads and RESETS the term's damage in a brief lock; returns true when the
    /// child produced any output (Full or Partial damage). Used by `render_split`
    /// to skip re-running the expensive `push_pane` rebuild for unchanged panes.
    /// The focused pane is always rebuilt (it owns the blinking cursor + selection,
    /// which are not part of alacritty's damage), so this is only consulted for the
    /// non-focused panes.
    pub(crate) fn pane_damaged(pty: &Pty) -> bool {
        let mut term = pty.term.lock();
        let damaged = match term.damage() {
            alacritty_terminal::term::TermDamage::Full => true,
            alacritty_terminal::term::TermDamage::Partial(mut it) => it.next().is_some(),
        };
        term.reset_damage();
        damaged
    }

    pub(crate) fn push_pane(
        renderer: &mut Renderer,
        pty: &Pty,
        cols: usize,
        rows: usize,
        win_focused: bool,
        blink_on: bool,
        hovered_link: Option<&str>,
    ) {
        let term = pty.term.lock();
        let content = term.renderable_content();
        let colors = content.colors;
        let display_offset = content.display_offset as i32;
        let cursor = content.cursor;
        let selection = content.selection;
        let cursor_color = color::resolve(Color::Named(NamedColor::Cursor), colors);

        let cursor_shown = cursor.shape != CursorShape::Hidden;
        let cursor_row = cursor.point.line.0 + display_offset;
        let cursor_col = cursor.point.column.0 as i32;
        // A focused window's block cursor inverts the cell beneath it; the pane is
        // always treated as "containing" the cursor (focus is window-level here).
        let invert_block = cursor_shown && win_focused && cursor.shape == CursorShape::Block;

        let cells: Vec<_> = content.display_iter.collect();
        let mut row_started = vec![false; rows];
        let mut ci = 0;
        while ci < cells.len() {
            let indexed = &cells[ci];
            let cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                ci += 1;
                continue;
            }
            let row = indexed.point.line.0 + display_offset;
            let col = indexed.point.column.0 as i32;
            if row < 0 || row >= rows as i32 || col < 0 || col >= cols as i32 {
                ci += 1;
                continue;
            }
            let row_u = row as usize;
            if !row_started[row_u] {
                renderer.begin_row(row_u);
                row_started[row_u] = true;
            }

            let mut fg = color::resolve(cell.fg, colors);
            let mut bg = color::resolve(cell.bg, colors);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = [fg[0] * 0.66, fg[1] * 0.66, fg[2] * 0.66, fg[3]];
            }
            if selection.is_some_and(|range| range.contains(indexed.point)) {
                bg = color::selection_bg();
            }
            if invert_block && row == cursor_row && col == cursor_col {
                std::mem::swap(&mut fg, &mut bg);
            }
            let hidden = cell.flags.contains(Flags::HIDDEN);
            let bold = cell.flags.contains(Flags::BOLD) || cell.flags.contains(Flags::BOLD_ITALIC);
            let italic =
                cell.flags.contains(Flags::ITALIC) || cell.flags.contains(Flags::BOLD_ITALIC);
            let wide = cell.flags.contains(Flags::WIDE_CHAR);

            let mut decorations = if hidden {
                Decorations::default()
            } else {
                let underline = if cell.flags.contains(Flags::UNDERCURL) {
                    UnderlineStyle::Curl
                } else if cell.flags.contains(Flags::DOTTED_UNDERLINE) {
                    UnderlineStyle::Dotted
                } else if cell.flags.contains(Flags::DASHED_UNDERLINE) {
                    UnderlineStyle::Dashed
                } else if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
                    UnderlineStyle::Double
                } else if cell.flags.contains(Flags::UNDERLINE) {
                    UnderlineStyle::Single
                } else {
                    UnderlineStyle::None
                };
                let color = cell
                    .underline_color()
                    .map(|c| color::resolve(c, colors))
                    .unwrap_or(fg);
                Decorations {
                    underline,
                    strikeout: cell.flags.contains(Flags::STRIKEOUT),
                    color,
                }
            };
            if !hidden
                && matches!(decorations.underline, UnderlineStyle::None)
                && let Some(hov) = hovered_link
                && cell.hyperlink().is_some_and(|h| h.uri() == hov)
            {
                decorations.underline = UnderlineStyle::Single;
            }

            let ch = if hidden || cell.c == '\0' {
                ' '
            } else {
                cell.c
            };
            let (combiners, consumed) = if hidden {
                (Vec::new(), unit_len(&cells, ci))
            } else {
                build_grapheme(&cells, ci, indexed.point.line.0)
            };
            let wide = wide || consumed >= 2;
            renderer.push_cell(
                col as usize,
                row_u,
                ch,
                &combiners,
                fg,
                bg,
                bold,
                italic,
                wide,
                decorations,
            );
            ci += consumed;
        }

        // Cursor overlay (same precedence as the single-pane path).
        if cursor_shown
            && cursor_row >= 0
            && cursor_row < rows as i32
            && cursor_col >= 0
            && cursor_col < cols as i32
        {
            let blink_off = win_focused
                && cursor.shape != CursorShape::Hidden
                && !blink_on
                && term.cursor_style().blinking;
            if !blink_off {
                let overlay = if !win_focused {
                    Some(CursorOverlay::Hollow)
                } else {
                    match cursor.shape {
                        CursorShape::Beam => Some(CursorOverlay::Beam),
                        CursorShape::Underline => Some(CursorOverlay::Underline),
                        CursorShape::HollowBlock => Some(CursorOverlay::Hollow),
                        CursorShape::Block | CursorShape::Hidden => None,
                    }
                };
                if let Some(overlay) = overlay {
                    let r = cursor_row as usize;
                    if row_started[r] {
                        renderer.set_cur_row(r);
                    } else {
                        renderer.begin_row(r);
                    }
                    renderer.push_cursor(cursor_col as usize, r, overlay, cursor_color);
                }
            }
        }
    }
}
