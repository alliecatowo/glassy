//! Single-pane render path.

use super::*;

impl App {
    pub(crate) fn render(&mut self) {
        // Split tabs take the dedicated multi-pane path; a single-pane tab (the
        // common case) falls through to the byte-identical fast path below.
        if self.is_split() {
            self.render_split();
            self.dirty = false;
            if !self.first_frame_done {
                self.first_frame_done = true;
            }
            return;
        }

        // The OSC8 hyperlink under the pointer, underlined for affordance.
        // Captured before the renderer borrow.
        let hovered_link = self.hovered_link.clone();
        // Plain-text link spans on the hovered row. Pre-computed here (before
        // the renderer borrow) so the cell loop can do a fast col-range check.
        // Only computed when the pointer is over a cell that has no OSC 8 link
        // but does have a plain-text link (`hovered_link` is set for both cases;
        // `plain_link_at` tells us which kind it is).
        let (hovered_cell_col, hovered_cell_row) = self.mouse_cell;
        let plain_link_spans: Vec<super::selection::PlainLink> = if hovered_link.is_some()
            && hovered_cell_row < self.rows
            && self.pty.as_ref().is_some_and(|p| {
                // Quick check: OSC 8 link → no plain spans needed.
                //
                // IMPORTANT: compute the grid Point BEFORE taking the term lock.
                // `grid_point` itself locks `pty.term` (input.rs); evaluating it
                // inside `grid()[ … ]` would build the `grid()` MutexGuard temporary
                // first and then re-lock the SAME non-reentrant `FairMutex` on this
                // UI thread while the guard is still alive → permanent deadlock
                // ("not responding"). Hoisting `grid_point` drops its guard before
                // the indexing lock is taken (mirrors `cell_hyperlink`).
                let point = self.grid_point(hovered_cell_col, hovered_cell_row);
                p.term.lock().grid()[point].hyperlink().is_none()
            }) {
            self.scan_row_for_links(hovered_cell_row)
        } else {
            Vec::new()
        };
        // Visual-bell overlay: while the flash window is open, tint the whole frame
        // toward the foreground color; otherwise clear it. Computed before the
        // renderer borrow so it can read `self.bell_flash_until`.
        let flash = if self.bell_flash_until.is_some_and(|t| Instant::now() < t) {
            // A soft accent tint rather than a stark white flash — a shell bell
            // (failed tab-completion, pager limits) fires often, so keep it gentle.
            Some(bell::FLASH_COLOR)
        } else {
            None
        };

        // Snapshot tab state + scroll position up-front, under the immutable `&self`
        // borrow, so the tab-bar painter (which holds the live `&mut Renderer`) needs
        // only owned data and never collides with the renderer borrow.
        let (strip_off, strip_hist) = match self.pty.as_ref() {
            Some(pty) => {
                let t = pty.term.lock();
                (t.grid().display_offset() as i32, t.grid().history_size())
            }
            None => (0, 0),
        };
        let tab_snapshot = self.tab_bar_snapshot();
        // Inline tab-rename editor (drawn over its chip after the tab bar). The
        // blinking caret is omitted (static) so it never forces extra repaints.
        // Resolve the chip rect here (under `&self`) so the painter just draws.
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
        // Tab-bar incremental decision (single-pane path): rebuild only when the
        // painter's inputs changed or a full redraw is forced (e.g. theme), else
        // replay the cached overlay instead of re-shaping every tab title glyph.
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
        );
        let tab_bar_rebuild = self.force_full_redraw
            || self.tab_bar_key != Some(new_tab_key)
            || !self
                .renderer
                .as_ref()
                .map(|r| r.has_tab_overlay())
                .unwrap_or(false);

        // Status-bar snapshot: term mode, scroll position, selection count.
        // Taken here (under the `&self` borrow) before we take `&mut self.renderer`.
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
        // Status-bar cwd + git branch: read from the active PTY's cached PaneInfo.
        // Evaluated here (before the renderer borrow) once per frame; PaneInfo is
        // refreshed at most every 2 s (PROC_REFRESH_INTERVAL) by about_to_wait.
        let sb_cwd: Option<std::path::PathBuf> = self
            .pty
            .as_ref()
            .and_then(|p| p.pane_info.cwd.clone())
            .or_else(|| self.active_cwd.clone()); // fallback to OSC 7 path
        // Branch is precomputed in PaneInfo (refreshed on the 2 s proc poll), so
        // the render path does no filesystem walk.
        let sb_git_branch: Option<String> = self
            .pty
            .as_ref()
            .and_then(|p| p.pane_info.git_branch.clone());
        let sb_progress = self.active_progress;
        let sb_broadcast = self.broadcast_input;

        // Settings-form inputs (whole-`self` method calls) snapshotted BEFORE the
        // disjoint `renderer`/`pty` borrows below.
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

        // Menu/help overlay inputs snapshotted before the renderer borrow.
        // `has_selection` lets Copy be greyed when nothing is selected.
        let has_selection_for_menu = self
            .pty
            .as_ref()
            .and_then(|p| p.term.lock().selection_to_string())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        let menu_snapshot = if self.menu_open {
            let actions: &[MenuAction] = self.menu_items.as_deref().unwrap_or(MenuAction::ALL);
            let entries = actions_to_entries(actions, has_selection_for_menu);
            let (ax, ay) = self.menu_anchor_px.unwrap_or_else(|| {
                // Fallback: derive pixel anchor from cell-based legacy anchor.
                let (left, top) = self.menu_anchor.unwrap_or((0, TAB_STRIP_ROWS));
                if let Some(r) = self.renderer.as_ref() {
                    let m = r.cell_metrics();
                    let pad = r.pad();
                    (left as f32 * m.width + pad, top as f32 * m.height + pad)
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
        let tab_menu_snapshot = self.tab_menu_snapshot().map(|(entries, ax, ay, sel)| {
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

        // Find-bar (search) inputs snapshotted before the disjoint renderer/pty
        // borrows. `search_highlights` + `search_readout` both lock the term.
        let search_inputs = self.search_readout().map(|r| (r, self.search_highlights()));
        // Command-palette inputs snapshotted before the borrow.
        let palette_inputs = self.palette_snapshot();

        let (Some(renderer), Some(pty)) = (self.renderer.as_mut(), self.pty.as_ref()) else {
            return;
        };
        renderer.set_flash(flash);
        // The single-pane terminal grid is inset below the GUI tab bar in PIXELS.
        renderer.set_grid_origin_y(tab_bar_h(renderer.cell_metrics().height));

        // Hold the terminal lock only long enough to copy out renderable state.
        let mut term = pty.term.lock();

        // Collect the terminal's per-line damage BEFORE reading renderable
        // content. `damage()` borrows `term` mutably and reports which viewport
        // rows changed since the last frame (it also damages the previous and
        // current terminal-cursor rows). We translate that into a per-row dirty
        // mask; rows not marked are reused from the renderer's persistent storage.
        // `TermDamage::Full` (entered insert mode, scrollback scroll, reset, etc.)
        // forces a full rebuild. After reading we must `reset_damage()`.
        let rows = self.rows;
        let mut dirty = vec![false; rows];
        let mut full = self.force_full_redraw;
        match term.damage() {
            alacritty_terminal::term::TermDamage::Full => full = true,
            alacritty_terminal::term::TermDamage::Partial(it) => {
                for line in it {
                    if line.line < rows {
                        dirty[line.line] = true;
                    }
                }
            }
        }
        term.reset_damage();

        // The child's requested cursor style (shape is also mirrored in
        // `content.cursor.shape`); `blinking` decides whether the blink timer runs.
        // Read before `renderable_content()` so both immutable borrows of `term`
        // can coexist with the content's `display_iter`.
        let cursor_blinking = term.cursor_style().blinking;
        let content = term.renderable_content();
        let colors = content.colors;
        let display_offset = content.display_offset as i32;
        let cursor = content.cursor;
        let selection = content.selection;
        let cursor_color = color::resolve(Color::Named(NamedColor::Cursor), colors);

        // The cursor is suppressed only when Hidden, or mid-blink off-phase. It is
        // still drawn (as a hollow outline) when the window is unfocused. The block
        // shape inverts the cell beneath it; all other shapes are overlay rects.
        let blink_off = self.focused && cursor_blinking && !self.blink_on;
        let cursor_shown = cursor.shape != CursorShape::Hidden && !blink_off;
        let cursor_row = cursor.point.line.0 + display_offset;
        let cursor_col = cursor.point.column.0 as i32;
        // A focused, on-phase block cursor inverts its cell in the loop below;
        // every other drawn case is an overlay pushed after the cells.
        let invert_block = cursor_shown && self.focused && cursor.shape == CursorShape::Block;

        // --- Decide what to rebuild this frame. ---
        // A scrollback scroll moves every row; alacritty reports Full for it, but
        // guard explicitly too. A selection spans arbitrary rows and is not part
        // of terminal damage, so any change forces a full rebuild.
        let has_selection = selection.is_some();
        if display_offset != self.prev_display_offset
            || has_selection != self.prev_has_selection
            || (has_selection && self.prev_has_selection)
        {
            full = true;
        }
        // glassy's cursor overlay/invert (and its blink/focus state) is not part of
        // alacritty's damage, so always repaint the row the cursor sits on and the
        // row it occupied last frame. (alacritty damages the terminal-cursor rows
        // itself, but blink phase flips and focus changes produce no damage.)
        let cur_cursor_cell = if cursor_shown
            && cursor_row >= 0
            && cursor_row < rows as i32
            && cursor_col >= 0
            && cursor_col < self.cols as i32
        {
            Some((cursor_col as usize, cursor_row as usize))
        } else {
            None
        };
        if let Some((_, r)) = cur_cursor_cell.filter(|&(_, r)| r < rows) {
            dirty[r] = true;
        }
        if let Some((_, r)) = self.prev_cursor.filter(|&(_, r)| r < rows) {
            dirty[r] = true;
        }

        renderer.begin_frame(color::default_bg());

        // The renderer keeps per-row instances persistently; on a full rebuild we
        // clear/resize that storage so every row is repushed below.
        if full {
            // The terminal grid spans exactly `rows`; the tab bar is a pixel inset
            // (grid_origin_y), not a reserved cell row, so no +1 here.
            renderer.resize_grid(rows);
            for d in dirty.iter_mut() {
                *d = true;
            }
        }

        // Track which rows we have begun this frame so a row's first cell triggers
        // `begin_row` and the cursor overlay can re-target it later.
        let mut row_started = vec![false; rows];

        // Collect the visible cells so we can look ahead across cells when
        // reconstructing grapheme clusters (compound emoji span several cells).
        let cells: Vec<_> = content.display_iter.collect();

        // Ligature run accumulator. When ligature shaping is active we buffer
        // consecutive simple (no-combiner, single-cell, same bold/italic/row)
        // characters into a run and flush them as a unit via `push_ligature_run`.
        // This lets cosmic-text apply GSUB liga substitutions across cell boundaries
        // so e.g. `->`  shapes to `→` when the font has that ligature.
        //
        // A run breaks on: style change, row change, hidden cell, wide cell,
        // grapheme cluster (combiners present), box-drawing/block-element ranges
        // (which take the procedural path), or cursor inversion. All those cases
        // fall through to the individual `push_cell` path after flushing.
        let use_liga = renderer.ligatures_active();
        // Per-run accumulated state.
        let mut run_text = String::new();
        let mut run_cells: Vec<LigatureCell> = Vec::new();
        let mut run_row: usize = 0;
        let mut run_bold: bool = false;
        let mut run_italic: bool = false;

        // Flush the pending ligature run to the renderer.
        // Called when the run breaks or at the end of the cell loop.
        // NOTE: this is a local macro rather than a closure so it can mutably
        // borrow both `renderer` and the run state simultaneously.
        macro_rules! flush_run {
            () => {
                if !run_text.is_empty() {
                    if run_cells.len() == 1 {
                        // Single-cell run: skip the ligature path (no benefit)
                        // and emit directly via push_cell.
                        let lc = &run_cells[0];
                        let ch = run_text.chars().next().unwrap_or(' ');
                        renderer.push_cell(
                            lc.col,
                            run_row,
                            ch,
                            &[],
                            lc.fg,
                            lc.bg,
                            run_bold,
                            run_italic,
                            lc.wide,
                            lc.decorations,
                        );
                    } else {
                        renderer.push_ligature_run(
                            run_row, &run_text, &run_cells, run_bold, run_italic,
                        );
                    }
                    run_text.clear();
                    run_cells.clear();
                }
            };
        }

        let mut ci = 0;
        while ci < cells.len() {
            let indexed = &cells[ci];
            let cell = indexed.cell;

            // The right half of a wide character is a spacer; a base cell consumes
            // its own spacer below, so any spacer reached here is stray — skip it.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                ci += 1;
                continue;
            }

            let row = indexed.point.line.0 + display_offset;
            let col = indexed.point.column.0 as i32;
            if row < 0 || row >= self.rows as i32 || col < 0 || col >= self.cols as i32 {
                ci += 1;
                continue;
            }
            let row_u = row as usize;
            // Skip cells in rows that did not change: their instances are reused.
            if !dirty[row_u] {
                flush_run!();
                ci += unit_len(&cells, ci);
                continue;
            }
            // First cell of a dirty row: clear it and begin pushing into it. The
            // tab-bar inset is applied in pixels by the renderer (grid_origin_y),
            // so the screen row equals the terminal row.
            let srow = row_u;
            if !row_started[srow] {
                renderer.begin_row(srow);
                row_started[srow] = true;
            }

            let mut fg = color::resolve(cell.fg, colors);
            let mut bg = color::resolve(cell.bg, colors);

            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = [fg[0] * 0.66, fg[1] * 0.66, fg[2] * 0.66, fg[3]];
            }
            // Tint selected cells. Done before the cursor inversion so the cursor
            // still reads clearly when it sits inside the selection.
            if selection.is_some_and(|range| range.contains(indexed.point)) {
                bg = color::selection_bg();
            }
            // Block cursor: invert the cell beneath it.
            if invert_block && row == cursor_row && col == cursor_col {
                std::mem::swap(&mut fg, &mut bg);
            }
            let hidden = cell.flags.contains(Flags::HIDDEN);

            let bold = cell.flags.contains(Flags::BOLD) || cell.flags.contains(Flags::BOLD_ITALIC);
            let italic =
                cell.flags.contains(Flags::ITALIC) || cell.flags.contains(Flags::BOLD_ITALIC);
            // A double-width (CJK / wide-emoji) cell spans two columns; the
            // trailing spacer column is skipped above. The renderer lays the
            // glyph out across the full two-cell box when this is set.
            let wide = cell.flags.contains(Flags::WIDE_CHAR);

            // Text decorations. Hidden cells draw nothing, so suppress strokes
            // too. Underline styles are mutually exclusive (latest SGR wins);
            // map the cell flags to a single style. The decoration color is the
            // SGR 58 underline color when set, else the cell foreground, so e.g.
            // a red LSP curl sits under default-fg text.
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

            // Underline the hovered link's cells as a Ctrl+click affordance.
            // OSC 8 links: match by URI stored in the cell's hyperlink field.
            // Plain-text links: match by pre-computed col range on the hovered row.
            if !hidden && matches!(decorations.underline, UnderlineStyle::None) {
                let is_hovered_osc8 = hovered_link
                    .as_deref()
                    .is_some_and(|hov| cell.hyperlink().is_some_and(|h| h.uri() == hov));
                let is_hovered_plain = !plain_link_spans.is_empty()
                    && row_u == hovered_cell_row
                    && plain_link_spans
                        .iter()
                        .any(|s| (col as usize) >= s.col_start && (col as usize) < s.col_end);
                if is_hovered_osc8 || is_hovered_plain {
                    decorations.underline = UnderlineStyle::Single;
                }
            }

            let ch = if hidden || cell.c == '\0' {
                ' '
            } else {
                cell.c
            };
            // Reconstruct the grapheme cluster, merging this cell's combining /
            // ZWJ code points with any following cells joined by ZWJ, a skin-tone
            // modifier, a regional-indicator pair, or a variation selector — so
            // compound emoji (flags, families, professions) shape into one glyph.
            let (combiners, consumed) = if hidden {
                (Vec::new(), unit_len(&cells, ci))
            } else {
                build_grapheme(&cells, ci, indexed.point.line.0)
            };
            // A cluster that spans 2+ grid cells (a wide CJK char, but also an
            // emoji whose base code point is *narrow* yet joins following cells —
            // e.g. the trans flag 🏳️‍⚧️ = narrow white-flag + ZWJ + symbol) must get
            // a 2-cell box so its color glyph fills the space instead of being
            // squished into one cell.
            let wide = wide || consumed >= 2;

            // Determine whether this cell is eligible for ligature run accumulation.
            // A cell is ineligible if it has combiners, is wide, is hidden, is a
            // space/null (blank), or falls in the box-drawing / block-element ranges
            // (those take the procedural path in push_cell and must not be shaped as
            // part of a text run).
            let cp = ch as u32;
            let is_box_or_block = (0x2500..=0x259F).contains(&cp);
            let liga_eligible = use_liga
                && !hidden
                && combiners.is_empty()
                && !wide
                && ch != ' '
                && ch != '\0'
                && !is_box_or_block;

            if liga_eligible {
                // Check if this cell is compatible with the current open run.
                // A run break occurs on: row change, style change, or cursor
                // inversion (cursor-inverted cells must not join a ligature because
                // their fg/bg are swapped individually).
                let is_cursor_cell = invert_block && row == cursor_row && col == cursor_col;
                let run_continues = !run_cells.is_empty()
                    && run_row == srow
                    && run_bold == bold
                    && run_italic == italic
                    && !is_cursor_cell;

                if !run_continues {
                    // Flush any open run before starting a new one.
                    flush_run!();
                    run_row = srow;
                    run_bold = bold;
                    run_italic = italic;
                }

                // Append this cell to the run.
                run_text.push(ch);
                run_cells.push(LigatureCell {
                    col: col as usize,
                    fg,
                    bg,
                    wide: false, // we checked !wide above
                    decorations,
                });
                ci += consumed;
                continue;
            }

            // Non-ligature path: flush any open ligature run first.
            flush_run!();

            renderer.push_cell(
                col as usize,
                srow,
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

        // Flush any remaining ligature run at end of cell list.
        flush_run!();

        // The real GUI tab bar is painted as a pixel overlay near the END of the
        // frame (after the cursor + images), so it composites above everything in
        // its band; see the `paint_tab_bar` call below.

        // Cursor overlay. Drawn after the cells so the bars/outline land on top of
        // the cell background (glyphs still paint over them in the fg pass). The
        // overlay is in addition to — or instead of — the block invert above:
        //   - focused Block: handled by the invert; no overlay.
        //   - focused Beam/Underline: an fg-colored bar.
        //   - focused HollowBlock: an outline box.
        //   - unfocused (any non-hidden shape): an outline box, so an idle window
        //     still shows where the cursor is.
        if let Some((cc, cr)) = cur_cursor_cell {
            let overlay = if !self.focused {
                Some(CursorOverlay::Hollow)
            } else {
                match cursor.shape {
                    CursorShape::Beam => Some(CursorOverlay::Beam),
                    CursorShape::Underline => Some(CursorOverlay::Underline),
                    CursorShape::HollowBlock => Some(CursorOverlay::Hollow),
                    // Block is drawn by the cell invert; Hidden is unreachable here.
                    CursorShape::Block | CursorShape::Hidden => None,
                }
            };
            if let Some(overlay) = overlay {
                // The cursor row is usually (re)built above, in which case we
                // re-target it WITHOUT clearing so the overlay appends on top of
                // that row's cell backgrounds. On a partial-dirty frame (e.g. a
                // resize) the cursor's row can have no in-bounds cells and so was
                // never begun this frame — begin it now so the overlay still
                // paints instead of being dropped for a frame.
                let scr = cr;
                if cr < rows {
                    if row_started[scr] {
                        renderer.set_cur_row(scr);
                    } else {
                        renderer.begin_row(scr);
                        row_started[scr] = true;
                    }
                    renderer.push_cursor(cc, scr, overlay, cursor_color);
                }
            }
        }

        drop(term); // release before GPU submit / present

        // Cache whether the cursor should blink (style requested + not hidden) so
        // about_to_wait can decide the blink timer without re-taking the term lock
        // on every event.
        self.cursor_blinks = cursor_blinking && cursor.shape != CursorShape::Hidden;

        // Inline images (kitty graphics). Drawn as an overlay every frame from the
        // live placement list, anchored to the cell they were displayed at. The
        // stored row is viewport-relative at display time; translate by the current
        // scroll offset so images move with the buffer as the user scrolls.
        // Suppressed while a modal or dropdown is up so images don't punch through it.
        if !self.help_open && !self.settings_open && !self.menu_open && self.palette.is_none() {
            let store = pty.images.lock();
            if !store.placements().is_empty() {
                let m = renderer.cell_metrics();
                let pad = renderer.pad();
                for p in store.placements() {
                    let Some(img) = store.image(p.id) else {
                        continue;
                    };
                    let screen_vp = p.row - display_offset;
                    if screen_vp < 0 || screen_vp >= rows as i32 || p.col >= self.cols {
                        continue;
                    }
                    let screen_row = screen_vp as usize;
                    let x = p.col as f32 * m.width + pad;
                    // Match push_cell's pixel origin: grid rows are inset below the
                    // GUI tab bar in pixels (grid_origin_y).
                    let y = screen_row as f32 * m.height + pad + renderer.grid_origin_y();
                    // Honor the kitty c=/r= display size (in cells); otherwise draw
                    // at the image's native pixel size.
                    let (dst_w, dst_h) =
                        image_dst_size(p.cols, p.rows, img.width, img.height, m.width, m.height);
                    renderer.draw_image(p.id, &img.rgba, img.width, img.height, x, y, dst_w, dst_h);
                }
            }
        }

        // Real GUI tab bar (§3.1): painted over the pixel band [0, tab_bar_h) as
        // overlay quads + atlas glyphs, so it composites above the grid. Drawn
        // before any modal so a modal scrim dims it too. Rebuilt only when its
        // inputs changed; otherwise the cached overlay is replayed.
        if tab_bar_rebuild {
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
            );
            renderer.commit_tab_overlay();
        } else {
            renderer.replay_tab_overlay();
        }

        // Inline tab-rename editor: an opaque text field drawn over the chip being
        // renamed, on top of the (cached) tab bar so the caret/edits are live.
        if let Some((rect, buf, caret, sel)) = &rename_inputs {
            Self::paint_tab_rename(renderer, *rect, buf, *caret, *sel);
        }

        // Status bar (§3.4): E1 bar at the very bottom, always above the terminal
        // content because it is an overlay (drawn last, not a reserved cell row).
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
                sb_broadcast,
            );
        }

        // Find bar (Ctrl+Shift+F): match highlights + bottom bar. Drawn above the
        // grid/status bar but below the palette/settings/help modals (a modal scrim
        // dims it like everything else). Highlights are anchored to the grid by the
        // same pixel origin as the cells (grid_origin_y).
        if let Some(((query, caret, selection, count, current, bad_regex), highlights)) =
            &search_inputs
        {
            let (sw, sh) = renderer.surface_size();
            let pad = renderer.pad();
            let goy = renderer.grid_origin_y();
            Self::paint_search(
                renderer,
                (sw as f32, sh as f32),
                (pad, pad + goy),
                query,
                *caret,
                *selection,
                *count,
                *current,
                *bad_regex,
                highlights,
            );
        }

        // Command palette (Ctrl+Shift+P): centered fuzzy action list over a scrim.
        // Topmost modal. The row rects it returns are stored for mouse hit-testing
        // (mouse_px is read up-front so no `self` borrow collides with `renderer`).
        if let Some((query, caret, selection, rows, sel)) = &palette_inputs {
            let (sw, sh) = renderer.surface_size();
            let mouse = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
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
                mouse,
            );
        }

        // Modal overlays (help / settings): centered panels over a dimmed backdrop.
        // The settings form is a real GUI panel (§3.5) drawn via the overlay
        // pipeline; its events are captured here (disjoint field borrows coexist
        // with the live `renderer` borrow) and applied after the GPU submit below.
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
            // Real GUI help panel (§3.7): scrollable two-column keybindings over
            // a scrim. `help_state` carries scroll position across frames.
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
            );
            if help_result.close {
                self.help_open = false;
                self.overlay_opened_by_press = false;
                self.force_full_redraw = true;
            }
        }

        // Real GUI menu (§3.6): drawn AFTER the scrim-bearing overlays so it always
        // floats on top. Captured into a local so we can invoke the action after the
        // renderer borrow ends (invoking mutates `self`).
        let menu_clicked_action: Option<MenuAction> = if let Some((
            ref entries,
            ax,
            ay,
            sel_item,
            mouse,
            mouse_down,
            click,
        )) = menu_snapshot
        {
            let m = renderer.cell_metrics();
            let item_idx = gui::menu(
                renderer, m.width, m.height, mouse, mouse_down, click, ax, ay, entries, sel_item,
            );
            item_idx.and_then(|i| {
                self.menu_items
                    .as_deref()
                    .unwrap_or(MenuAction::ALL)
                    .get(i)
                    .copied()
            })
        } else {
            None
        };

        // Tab right-click context menu: drawn after the global menu so it floats
        // on top. The click is resolved in the MouseInput handler (via
        // `tab_menu_hit_test`); this draw only provides hover/keyboard feedback.
        if let Some((ref entries, ax, ay, sel_item, mouse, mouse_down, click)) = tab_menu_snapshot {
            let m = renderer.cell_metrics();
            gui::menu(
                renderer, m.width, m.height, mouse, mouse_down, click, ax, ay, entries, sel_item,
            );
        }

        // In-app toast notifications: painted above the terminal chrome but below
        // any modal (settings/help). Toasts are transient overlays that do not block
        // interaction. They are only painted when there are live toasts.
        {
            let tbh = tab_bar_h(renderer.cell_metrics().height);
            let still_alive = crate::app::toast::paint_toasts(renderer, &mut self.toasts, tbh);
            if still_alive {
                // Schedule another frame so the fade animations tick.
                self.dirty = true;
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
        }

        // Confirm-close modal: a simple "Close tab? A process is still running"
        // dialog drawn above everything else when a close was intercepted.
        let confirm_result = if self.confirm_close.is_some() {
            let (sw, sh) = renderer.surface_size();
            let m = renderer.cell_metrics();
            Some(Self::paint_confirm_close(
                renderer,
                (sw as f32, sh as f32),
                m.width,
                m.height,
                (self.mouse_px.0 as f32, self.mouse_px.1 as f32),
                self.held_button == Some(0),
                self.gui_click_edge,
                &mut self.gui_pressed,
                &mut self.gui_anims,
            ))
        } else {
            None
        };

        // Record the state this frame drew from, so the next frame can repaint only
        // what changed (the cursor's old/new row, selection, scroll position).
        self.prev_cursor = cur_cursor_cell;
        self.prev_display_offset = display_offset;
        self.prev_has_selection = has_selection;
        self.force_full_redraw = false;
        self.tab_bar_key = Some(new_tab_key);
        // The chrome paint consumed this frame's click edge; if it was a release
        // edge, also drop the press latch now that the click has been resolved.
        // Clear `overlay_opened_by_press` at the SAME moment the edge is consumed:
        // the opening gesture's release is now fully accounted for (the help paint
        // skipped its scrim-close, the settings guard absorbed it), so the next
        // genuinely-outside click is free to dismiss.
        if self.gui_click_edge {
            self.gui_pressed = None;
            self.overlay_opened_by_press = false;
        }
        self.gui_click_edge = false;
        // The double-click edge for chrome text fields is consumed by this same
        // paint (it drives one frame of word-select); drop it now.
        self.gui_double_click = false;

        // The renderer self-heals lost/outdated surfaces internally. If a frame is
        // dropped (e.g. transient surface loss), the damage we consumed + the rows
        // we built may not have reached the GPU, so re-arm a full rebuild and ask
        // for another frame — otherwise that content stays missing until the next
        // resize. (Root cause of the "blank until you resize" reports.)
        if let Err(err) = renderer.render() {
            log::debug!("frame dropped, forcing full repaint next frame: {err:?}");
            self.force_full_redraw = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }

        // Startup benchmark: log time-to-first-frame once.
        if !self.first_frame_done {
            self.first_frame_done = true;
            log::info!(
                "glassy time-to-first-frame: {:.1} ms",
                self.started.elapsed().as_secs_f64() * 1000.0
            );
        }

        // If the glyph atlas overflowed and was repacked this frame, every cached
        // glyph's UVs changed; persisted rows now hold stale UVs. Force a full
        // rebuild and schedule one more frame to repaint cleanly.
        if renderer.pull_atlas_reset() {
            self.force_full_redraw = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }

        self.dirty = false;

        // Apply settings-form interactions now the renderer borrow has ended, so
        // opacity / font / theme preview live and Save/Close take effect. Deferred
        // here (after present) because applying mutates `self.renderer`/config.
        if let Some(ev) = settings_events {
            self.apply_settings_events(ev);
        }

        // The menu click result from gui::menu is used to provide visual feedback
        // only; the actual invocation happens in the MouseInput handler via
        // `menu_hit_test` (which now uses pixel coordinates matching the drawn panel).
        let _ = menu_clicked_action;

        // Apply confirm-close modal result (renderer borrow has ended here).
        if let Some(result) = confirm_result {
            use chrome::ConfirmCloseResult::*;
            match result {
                Confirm => {
                    // User confirmed: actually perform the close.
                    let pending = self.confirm_close.take();
                    self.force_full_redraw = true;
                    // We need an event_loop reference to close; since we can't
                    // get it from here, queue it as a deferred action. For now
                    // we set a flag that the next event will pick up.
                    // NOTE: This is handled by a deferred field read in event_loop.rs.
                    // See `pending_confirm_close` logic.
                    match pending {
                        Some(ConfirmClose::ActiveTab) => {
                            // We can't call event_loop here; use a flag.
                            self.confirm_close = Some(ConfirmClose::ActiveTab);
                            self.pending_confirm_execute = true;
                        }
                        Some(ConfirmClose::ActivePane) => {
                            self.confirm_close = Some(ConfirmClose::ActivePane);
                            self.pending_confirm_execute = true;
                        }
                        None => {}
                    }
                }
                Cancel => {
                    self.confirm_close = None;
                    self.force_full_redraw = true;
                }
                Pending => {}
            }
        }
    }
}
