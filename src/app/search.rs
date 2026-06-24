//! In-terminal search (Ctrl+Shift+F): a bottom find bar with a query field,
//! alacritty `RegexSearch` over the active pane's scrollback, all-match
//! highlighting (drawn as overlay rects), next/prev jumping, and a live match
//! count.
//!
//! Idle stays at 0%: the find bar paints only while open and only repaints on a
//! real change (a keystroke in the query, a jump, a viewport scroll). No timer
//! and no `Poll` is introduced — the bar is static between interactions.

use super::*;

use alacritty_terminal::index::Direction;
use alacritty_terminal::term::search::{Match, RegexSearch};

/// All state for the find bar. Owned by `App` (as `Option<SearchState>`); `Some`
/// exactly while the bar is open. The compiled `RegexSearch` and the collected
/// match list are rebuilt whenever the query string changes.
pub(crate) struct SearchState {
    /// The live query text the user is typing.
    pub query: String,
    /// Every match in the focused pane's grid + scrollback, top-to-bottom. Each
    /// is an inclusive `start..=end` grid `Point` range. Empty when the query is
    /// empty or compiles to no matches.
    pub matches: Vec<Match>,
    /// Index into `matches` of the currently-focused match (the one the viewport
    /// is jumped to and drawn in the accent "current" color). `None` when there
    /// are no matches.
    pub current: Option<usize>,
    /// True when the last non-empty query failed to compile as a regex, so the
    /// bar can show an error tint instead of a misleading "0 matches".
    pub bad_regex: bool,
}

impl SearchState {
    fn new() -> Self {
        SearchState {
            query: String::new(),
            matches: Vec::new(),
            current: None,
            bad_regex: false,
        }
    }
}

/// Height of the find bar in physical px, derived from the cell height so it
/// scales with the font exactly like the rest of the chrome.
pub(crate) fn find_bar_h(cell_h: f32) -> f32 {
    (cell_h * 1.9).round()
}

impl App {
    /// Open (or focus) the find bar. Seeds it from the current selection when one
    /// exists, so "select a word, Ctrl+Shift+F" pre-fills the obvious query.
    pub(crate) fn open_search(&mut self, event_loop: &ActiveEventLoop) {
        if self.search.is_none() {
            let mut st = SearchState::new();
            // Pre-fill from a single-line selection (a common "find this" gesture).
            if let Some(sel) = self
                .pty
                .as_ref()
                .and_then(|p| p.term.lock().selection_to_string())
                && !sel.is_empty()
                && !sel.contains('\n')
            {
                st.query = sel;
            }
            self.search = Some(st);
            self.recompute_search();
        }
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Close the find bar and drop its match list. The selection (if the user
    /// jumped to a match) is left intact.
    pub(crate) fn close_search(&mut self, event_loop: &ActiveEventLoop) {
        if self.search.take().is_some() {
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
        }
    }

    /// Handle a keypress while the find bar is open. Returns `true` if the key was
    /// consumed (it never reaches the child while the bar is up). Esc closes;
    /// Enter / Shift+Enter jump next / prev; Backspace edits; printable text is
    /// appended to the query.
    pub(crate) fn handle_search_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) -> bool {
        if self.search.is_none() {
            return false;
        }
        match key {
            Key::Named(NamedKey::Escape) => {
                self.close_search(event_loop);
                true
            }
            Key::Named(NamedKey::Enter) => {
                let dir = if self.mods.shift_key() {
                    Direction::Left
                } else {
                    Direction::Right
                };
                self.search_jump(dir, event_loop);
                true
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(st) = self.search.as_mut() {
                    st.query.pop();
                }
                self.recompute_search();
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                true
            }
            Key::Character(s) => {
                if let Some(st) = self.search.as_mut() {
                    st.query.push_str(s.as_str());
                }
                self.recompute_search();
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                true
            }
            Key::Named(NamedKey::Space) => {
                if let Some(st) = self.search.as_mut() {
                    st.query.push(' ');
                }
                self.recompute_search();
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                true
            }
            _ => false,
        }
    }

    /// Recompile the query and collect every match in the focused pane's grid +
    /// scrollback. Caps the result count so a pathological query (e.g. `.`) on a
    /// huge scrollback can't blow up memory or stall the frame.
    pub(crate) fn recompute_search(&mut self) {
        // Snapshot the query out of `self.search` to avoid a borrow conflict with
        // the `self.pty` lock below.
        let query = match self.search.as_ref() {
            Some(st) => st.query.clone(),
            None => return,
        };
        let mut matches: Vec<Match> = Vec::new();
        let mut bad_regex = false;
        if !query.is_empty() {
            match RegexSearch::new(&query) {
                Ok(mut regex) => {
                    if let Some(pty) = self.pty.as_ref() {
                        let term = pty.term.lock();
                        matches = collect_matches(&term, &mut regex);
                    }
                }
                Err(_) => bad_regex = true,
            }
        }
        if let Some(st) = self.search.as_mut() {
            st.matches = matches;
            st.bad_regex = bad_regex;
            // Keep the focus on the first match (closest to the bottom/prompt is
            // arguably nicer, but "first from top" is predictable and matches the
            // highlight order). Clamp any stale index.
            st.current = if st.matches.is_empty() { None } else { Some(0) };
        }
        // Reveal the focused match without changing it (no-op when none).
        self.search_reveal_current();
    }

    /// Jump the focused match one step in `dir` and scroll it into view. Wraps at
    /// the ends. A no-op when there are no matches.
    pub(crate) fn search_jump(&mut self, dir: Direction, event_loop: &ActiveEventLoop) {
        let next = match self.search.as_mut() {
            Some(st) if !st.matches.is_empty() => {
                let n = st.matches.len();
                let cur = st.current.unwrap_or(0);
                let next = match dir {
                    Direction::Right => (cur + 1) % n,
                    Direction::Left => (cur + n - 1) % n,
                };
                st.current = Some(next);
                Some(next)
            }
            _ => None,
        };
        if next.is_some() {
            self.search_reveal_current();
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
        }
    }

    /// Scroll the focused pane's viewport so the current match is visible (roughly
    /// centered), by adjusting the display offset. A no-op when no match is
    /// focused. Does not select — highlighting alone marks the match.
    fn search_reveal_current(&mut self) {
        let target_line = match self.search.as_ref() {
            Some(st) => match st.current {
                Some(i) => st.matches.get(i).map(|m| m.start().line.0),
                None => return,
            },
            None => return,
        };
        let Some(line) = target_line else { return };
        let Some(pty) = self.pty.as_ref() else { return };
        let rows = self.rows as i32;
        let mut term = pty.term.lock();
        let display_offset = term.grid().display_offset() as i32;
        // Current on-screen row of the match: line + display_offset (matches the
        // render translation). Bring it to roughly the vertical center.
        let screen_row = line + display_offset;
        let want = rows / 2;
        let delta = want - screen_row;
        if delta != 0 {
            term.scroll_display(Scroll::Delta(delta));
        }
    }

    /// Build the visible match highlights for the focused pane: translate each
    /// match's grid lines to screen rows (line + display_offset), clip to the
    /// pane's [0, rows) viewport and split multi-line matches into per-row runs,
    /// returning `(col_start, col_end, screen_row, is_current)` for each run. The
    /// caller passes these to [`Self::paint_search`]. Holds the term lock briefly.
    pub(crate) fn search_highlights(&self) -> Vec<(usize, usize, usize, bool)> {
        let Some(st) = self.search.as_ref() else { return Vec::new() };
        if st.matches.is_empty() {
            return Vec::new();
        }
        let Some(pty) = self.pty.as_ref() else { return Vec::new() };
        let term = pty.term.lock();
        let display_offset = term.grid().display_offset() as i32;
        drop(term);
        let rows = self.rows as i32;
        let cols = self.cols;
        let last_col = cols.saturating_sub(1);
        let mut out = Vec::new();
        for (i, m) in st.matches.iter().enumerate() {
            let is_cur = st.current == Some(i);
            let start = m.start();
            let end = m.end();
            // A match can span multiple grid lines; emit one run per line.
            let l0 = start.line.0;
            let l1 = end.line.0;
            for line in l0..=l1 {
                let screen = line + display_offset;
                if screen < 0 || screen >= rows {
                    continue;
                }
                let c0 = if line == l0 { start.column.0 } else { 0 };
                let c1 = if line == l1 { end.column.0 } else { last_col };
                let c0 = c0.min(last_col);
                let c1 = c1.min(last_col);
                out.push((c0, c1, screen as usize, is_cur));
            }
        }
        out
    }

    /// Snapshot of the find-bar paint inputs (count, current index, bad-regex).
    pub(crate) fn search_readout(&self) -> Option<(String, usize, Option<usize>, bool)> {
        self.search
            .as_ref()
            .map(|st| (st.query.clone(), st.matches.len(), st.current, st.bad_regex))
    }

    /// Paint the find bar (bottom of the window) + all match highlights. Called
    /// from both render paths after the cells/cursor are pushed so it composites
    /// on top. `surface` is the framebuffer size in px. Associated fn (no `&self`)
    /// so it composes with the caller's `&mut Renderer` borrow.
    ///
    /// `matches_screen` are the matches already translated to screen rows + culled
    /// to the visible viewport by the caller (which holds the term lock), as
    /// `(col_start, col_end, screen_row, is_current)` per highlighted cell-run.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_search(
        renderer: &mut Renderer,
        surface: (f32, f32),
        origin: (f32, f32),
        query: &str,
        match_count: usize,
        current: Option<usize>,
        bad_regex: bool,
        highlights: &[(usize, usize, usize, bool)],
    ) {
        let m = renderer.cell_metrics();
        let cell_w = m.width;
        let cell_h = m.height;

        // --- Match highlights (drawn first, behind the bar) ------------------
        // Each highlight is a run of cells on one screen row, positioned relative
        // to the focused grid's pixel origin (`origin`): the renderer pad + tab-bar
        // inset in single-pane, or the focused pane's body rect + pad in a split.
        // The current match gets the accent color; the rest a dimmer match tint.
        let with_alpha = |mut c: [f32; 4], a: f32| {
            c[3] = a;
            c
        };
        let cur_col = with_alpha(color::accent(), 0.55);
        let other_col = with_alpha(color::selection_bg(), 0.50);
        for &(c0, c1, row, is_cur) in highlights {
            let x = c0 as f32 * cell_w + origin.0;
            let w = ((c1 + 1).saturating_sub(c0)) as f32 * cell_w;
            let y = row as f32 * cell_h + origin.1;
            let col = if is_cur { cur_col } else { other_col };
            renderer.push_overlay_px(x, y, w, cell_h, col);
        }

        // --- Find bar (bottom edge) ------------------------------------------
        let bar_h = find_bar_h(cell_h);
        let bar_y = surface.1 - bar_h;
        // E1 chrome-bar fill + a 1px accent top rail.
        renderer.push_overlay_px(0.0, bar_y, surface.0, bar_h, gui::glass_body());
        renderer.push_overlay_px(0.0, bar_y, surface.0, 1.0, gui::rail());

        let inner_pad = (cell_w * 1.0).round();
        let ty = (bar_y + (bar_h - cell_h) * 0.5).round();
        let mut cx = inner_pad;

        // Leading prompt chevron (BMP-safe; U+2315 ⌕ tofus on most terminal fonts).
        renderer.push_overlay_glyph_px(cx.round(), ty, '\u{203A}', color::accent());
        cx += cell_w * 2.0;

        // Query text (or a dim placeholder), with a block caret at the end.
        let text_col = if bad_regex { color::danger() } else { gui::fg() };
        if query.is_empty() {
            for ch in "search…".chars() {
                renderer.push_overlay_glyph_px(cx.round(), ty, ch, gui::fg_dim());
                cx += cell_w;
            }
        } else {
            for ch in query.chars() {
                renderer.push_overlay_glyph_px(cx.round(), ty, ch, text_col);
                cx += cell_w;
            }
        }
        // Caret after the query.
        let caret_x = inner_pad + cell_w * 2.0 + query.chars().count() as f32 * cell_w;
        renderer.push_overlay_px(caret_x.round(), ty, 2.0, cell_h, color::accent());

        // Trailing match-count readout, right-aligned.
        let readout = if bad_regex {
            "bad regex".to_string()
        } else if query.is_empty() {
            String::new()
        } else if match_count == 0 {
            "no matches".to_string()
        } else {
            let idx = current.map(|i| i + 1).unwrap_or(0);
            format!("{idx}/{match_count}")
        };
        if !readout.is_empty() {
            let w = readout.chars().count() as f32 * cell_w;
            let rx = surface.0 - inner_pad - w;
            let mut rcx = rx;
            let rcol = if bad_regex { color::danger() } else { gui::fg_dim() };
            for ch in readout.chars() {
                renderer.push_overlay_glyph_px(rcx.round(), ty, ch, rcol);
                rcx += cell_w;
            }
        }
    }
}

/// Collect every match for `regex` across the focused pane's full grid +
/// scrollback, top-to-bottom, capped at [`MAX_MATCHES`]. Walks `search_next`
/// rightward from the topmost cell, advancing past each match's end so the same
/// match is never re-emitted, and stops when a search wraps back above the
/// previous match (no more forward progress).
fn collect_matches<T>(
    term: &alacritty_terminal::term::Term<T>,
    regex: &mut RegexSearch,
) -> Vec<Match> {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Point, Side};

    /// Hard cap so a `.`-style query over a million-line scrollback can't OOM or
    /// stall the frame. Beyond it the count saturates and highlighting stops.
    const MAX_MATCHES: usize = 5000;

    let mut out: Vec<Match> = Vec::new();
    let topmost = term.topmost_line();
    let last_col = term.last_column();
    let bottommost = term.bottommost_line();
    // Walk forward from the very top. `search_next` returns the next match at or
    // after the origin and WRAPS when none remain ahead, so we stop as soon as a
    // returned match isn't strictly after the previous one (the wrap, or no
    // forward progress) — that also bounds the loop on degenerate queries.
    let mut origin = Point::new(topmost, Column(0));
    // A plain `loop` (not `while let`): the body has several distinct break points
    // (wrap-around, match cap, end-of-grid) past the initial search, so collapsing
    // to a `while let` would obscure them.
    #[allow(clippy::while_let_loop)]
    loop {
        let Some(m) = term.search_next(regex, origin, Direction::Right, Side::Left, None) else {
            break;
        };
        let start = *m.start();
        let end = *m.end();
        if let Some(prev) = out.last() {
            let prev_start = *prev.start();
            let forward = start.line > prev_start.line
                || (start.line == prev_start.line && start.column > prev_start.column);
            if !forward {
                break; // wrapped or stalled: every distinct match is collected
            }
        }
        out.push(start..=end);
        if out.len() >= MAX_MATCHES {
            break;
        }
        // Advance one cell past the match end for the next search.
        origin = if end.column < last_col {
            Point::new(end.line, end.column + 1)
        } else if end.line < bottommost {
            Point::new(end.line + 1, Column(0))
        } else {
            break; // reached the very last cell of the grid
        };
    }
    out
}
