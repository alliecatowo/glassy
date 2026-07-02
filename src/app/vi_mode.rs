//! Keyboard copy-mode ("vi mode"): select and copy text without a mouse.
//!
//! glassy drives [`alacritty_terminal`]'s built-in vi-mode cursor + motions, so
//! the heavy lifting (cursor tracking, word/line/paragraph motions, selection
//! recompute, viewport follow) is delegated to the terminal layer. This module
//! is the glassy-side state machine + key router that:
//!
//!   * toggles the mode (default bind `Ctrl+Shift+Space`, action `ViMode`),
//!   * maps vi keys (`hjkl`, `w`/`b`/`e`, `0`/`$`/`^`, `gg`/`G`, `H`/`M`/`L`,
//!     `{`/`}`, `%`) to [`ViMotion`] dispatches,
//!   * starts/cancels a visual selection (`v` charwise, `V` linewise,
//!     `Ctrl+v` blockwise),
//!   * yanks the selection to the clipboard (`y`) and exits, and
//!   * exits on `Esc` / a second toggle.
//!
//! While the mode is active the terminal renders the vi cursor automatically:
//! [`alacritty_terminal`]'s `RenderableCursor` reports `vi_mode_cursor.point`
//! whenever `TermMode::VI` is set, so the existing glassy cursor-overlay path in
//! `app/render.rs` draws the keyboard cursor with no extra wiring.
//!
//! # Headless hook
//!
//! `GLASSY_VIMODE=1` enters the mode at startup and starts a charwise visual
//! selection, so the keyboard cursor + selection can be captured by a
//! `GLASSY_CAPTURE` frame. `GLASSY_VIMODE=line` / `GLASSY_VIMODE=block` pick the
//! linewise / blockwise visual kind instead.

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vi_mode::ViMotion;
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};

/// The kind of visual (range) selection a `v`/`V`/`Ctrl+v` press started, or
/// `None` when the vi cursor is free-moving with no active range.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum VisualKind {
    /// No range selection; motions just move the cursor.
    #[default]
    None,
    /// Charwise (`v`): a [`SelectionType::Simple`] span.
    Char,
    /// Linewise (`V`): whole rows ([`SelectionType::Lines`]).
    Line,
    /// Blockwise (`Ctrl+v`): a rectangular column block ([`SelectionType::Block`]).
    Block,
}

/// glassy-side vi/copy-mode state. The authoritative cursor + selection live in
/// the terminal (`Term::vi_mode_cursor` / `Term::selection`); this only tracks
/// whether the mode is on and which visual kind is in flight, plus the pending
/// first key of a two-key motion (`g`).
#[derive(Clone, Copy, Default)]
pub(crate) struct ViState {
    /// Whether keyboard copy-mode is active (mirrors `TermMode::VI`).
    pub active: bool,
    /// The active visual-selection kind (None when only the cursor moves).
    pub visual: VisualKind,
    /// True after a lone `g` press, awaiting the second key of `gg`.
    pub pending_g: bool,
}

impl super::App {
    /// Toggle keyboard copy-mode (the `ViMode` key action). Entering snaps the
    /// view so the vi cursor is visible and starts with no range; exiting clears
    /// any in-flight selection and snaps back to the prompt.
    pub(crate) fn vi_toggle(&mut self, event_loop: &ActiveEventLoop) {
        if self.vi.active {
            self.vi_exit(event_loop);
        } else {
            self.vi_enter(event_loop);
        }
    }

    /// Enter keyboard copy-mode. A no-op (with a toast) when there is no live
    /// PTY. Resets any stale visual state and toggles `TermMode::VI` on so the
    /// terminal reports the vi cursor for rendering.
    pub(crate) fn vi_enter(&mut self, event_loop: &ActiveEventLoop) {
        let Some(pty) = self.pty.as_ref() else {
            self.push_toast("Copy mode unavailable");
            return;
        };
        {
            let mut term = pty.term.lock();
            if !term.mode().contains(TermMode::VI) {
                term.toggle_vi_mode();
            }
            term.selection = None;
        }
        self.vi.active = true;
        self.vi.visual = VisualKind::None;
        self.vi.pending_g = false;
        self.push_toast("Copy mode — hjkl move, v select, y copy, Esc exit");
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Exit keyboard copy-mode: drop any selection, toggle `TermMode::VI` off,
    /// and snap the viewport back to the prompt. Safe to call when not active.
    pub(crate) fn vi_exit(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(pty) = self.pty.as_ref() {
            let mut term = pty.term.lock();
            if term.mode().contains(TermMode::VI) {
                term.toggle_vi_mode();
            }
            term.selection = None;
            term.scroll_display(alacritty_terminal::grid::Scroll::Bottom);
        }
        self.vi.active = false;
        self.vi.visual = VisualKind::None;
        self.vi.pending_g = false;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Handle one keypress while copy-mode is active. Returns `true` when the key
    /// was consumed by copy-mode (so the caller must not forward it to the child
    /// or the keymap). Every recognized key repaints; unrecognized keys are
    /// swallowed (copy-mode owns the keyboard) but cause no state change.
    pub(crate) fn vi_handle_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) -> bool {
        if !self.vi.active {
            return false;
        }
        // Esc exits, regardless of pending state.
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.vi_exit(event_loop);
            return true;
        }

        // Ctrl+v starts (or toggles) a blockwise/rectangular visual selection.
        // Checked before the plain-char path because it is a chord, not a bare
        // `v`. (`self.mods` carries the live modifier state set by the keyboard
        // handler / script harness.)
        if self.mods.control_key()
            && let Key::Character(s) = key
            && s.as_str() == "v"
        {
            self.vi.pending_g = false;
            self.vi_start_visual(VisualKind::Block, event_loop);
            return true;
        }

        // A pending `g` consumes the next key as the second half of a 2-key
        // motion (`gg` = top of buffer). Any other key cancels the pending `g`
        // and then falls through to normal handling.
        if self.vi.pending_g {
            self.vi.pending_g = false;
            if let Key::Character(s) = key
                && s.as_str() == "g"
            {
                self.vi_goto_edge(true);
                self.mark_dirty(event_loop);
                return true;
            }
            // fall through: re-handle this key fresh below.
        }

        match key {
            // Arrow keys mirror hjkl so the mode is usable without learning vi.
            Key::Named(NamedKey::ArrowLeft) => self.vi_motion(ViMotion::Left),
            Key::Named(NamedKey::ArrowDown) => self.vi_motion(ViMotion::Down),
            Key::Named(NamedKey::ArrowUp) => self.vi_motion(ViMotion::Up),
            Key::Named(NamedKey::ArrowRight) => self.vi_motion(ViMotion::Right),
            Key::Named(NamedKey::Home) => self.vi_motion(ViMotion::First),
            Key::Named(NamedKey::End) => self.vi_motion(ViMotion::Last),
            Key::Named(NamedKey::Enter) => {
                // Enter on a started selection yanks (vi-ish "confirm").
                if self.vi.visual != VisualKind::None {
                    self.vi_yank(event_loop);
                    return true;
                }
            }
            Key::Character(s) => {
                if !self.vi_handle_char(s.as_str(), event_loop) {
                    // Unrecognized printable: swallow without changing state.
                    return true;
                }
            }
            _ => {
                // Unknown named key: swallow (copy-mode owns the keyboard).
                return true;
            }
        }
        self.mark_dirty(event_loop);
        true
    }

    /// Handle one printable copy-mode key. Returns `true` if it produced an
    /// effect that warrants a repaint, `false` if it was a no-op to be swallowed.
    fn vi_handle_char(&mut self, c: &str, event_loop: &ActiveEventLoop) -> bool {
        match c {
            // --- motion: hjkl ------------------------------------------------
            "h" => self.vi_motion(ViMotion::Left),
            "j" => self.vi_motion(ViMotion::Down),
            "k" => self.vi_motion(ViMotion::Up),
            "l" => self.vi_motion(ViMotion::Right),
            // --- motion: word ------------------------------------------------
            "w" => self.vi_motion(ViMotion::WordRight),
            "b" => self.vi_motion(ViMotion::WordLeft),
            "e" => self.vi_motion(ViMotion::WordRightEnd),
            // --- motion: line ------------------------------------------------
            "0" => self.vi_motion(ViMotion::First),
            "$" => self.vi_motion(ViMotion::Last),
            "^" => self.vi_motion(ViMotion::FirstOccupied),
            // --- motion: screen / buffer -------------------------------------
            "H" => self.vi_motion(ViMotion::High),
            "M" => self.vi_motion(ViMotion::Middle),
            "L" => self.vi_motion(ViMotion::Low),
            "G" => self.vi_goto_edge(false),
            "g" => {
                // First half of `gg`; await the next key.
                self.vi.pending_g = true;
            }
            // --- motion: paragraph / bracket ---------------------------------
            "{" => self.vi_motion(ViMotion::ParagraphUp),
            "}" => self.vi_motion(ViMotion::ParagraphDown),
            "%" => self.vi_motion(ViMotion::Bracket),
            // --- visual selection start / cancel -----------------------------
            "v" => self.vi_start_visual(VisualKind::Char, event_loop),
            "V" => self.vi_start_visual(VisualKind::Line, event_loop),
            // --- yank --------------------------------------------------------
            "y" => {
                self.vi_yank(event_loop);
            }
            // --- exit --------------------------------------------------------
            "q" => self.vi_exit(event_loop),
            _ => return false,
        }
        true
    }

    /// Start (or restart) a visual range selection anchored at the current vi
    /// cursor. Pressing the same kind again cancels the selection (vi-style
    /// toggle); a different kind switches kind in place.
    pub(crate) fn vi_start_visual(&mut self, kind: VisualKind, event_loop: &ActiveEventLoop) {
        let Some(pty) = self.pty.as_ref() else { return };
        if self.vi.visual == kind {
            // Toggle off: cancel the range, keep the cursor where it is.
            pty.term.lock().selection = None;
            self.vi.visual = VisualKind::None;
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
            return;
        }
        let ty = match kind {
            VisualKind::Char => SelectionType::Simple,
            VisualKind::Line => SelectionType::Lines,
            VisualKind::Block => SelectionType::Block,
            VisualKind::None => return,
        };
        {
            let mut term = pty.term.lock();
            let point = term.vi_mode_cursor.point;
            // Anchor on the LEFT side of the cell and extend to its RIGHT side so
            // the range is immediately NON-empty (one cell). This matters because
            // alacritty's `vi_mode_recompute_selection` only extends a selection
            // that is already non-empty, so a zero-width anchor would never grow
            // on the first motion. The single-cell seed also matches vi's `v`,
            // which selects the cell under the cursor before any motion.
            let mut sel = Selection::new(ty, point, Side::Left);
            sel.update(point, Side::Right);
            term.selection = Some(sel);
        }
        self.vi.visual = kind;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Dispatch a [`ViMotion`] to the terminal's vi cursor. The terminal
    /// recomputes any active selection to follow the cursor.
    fn vi_motion(&mut self, motion: ViMotion) {
        if let Some(pty) = self.pty.as_ref() {
            pty.term.lock().vi_motion(motion);
        }
        self.force_full_redraw = true;
    }

    /// Jump the vi cursor to the top (`gg`, `top == true`) or bottom (`G`) of the
    /// whole buffer (scrollback included), scrolling the viewport to follow.
    /// `vi_goto_point` recomputes any active selection just like a motion does.
    fn vi_goto_edge(&mut self, top: bool) {
        if let Some(pty) = self.pty.as_ref() {
            let mut term = pty.term.lock();
            let point = if top {
                // Topmost history line, column 0.
                Point::new(Line(-(term.grid().history_size() as i32)), Column(0))
            } else {
                // Bottommost visible line, last column.
                let last_col = term.grid().columns().saturating_sub(1);
                Point::new(
                    Line(term.grid().screen_lines() as i32 - 1),
                    Column(last_col),
                )
            };
            term.vi_goto_point(point);
        }
        self.force_full_redraw = true;
    }

    /// Yank (copy) the active selection to the clipboard and exit copy-mode. A
    /// no-op (but still exits) when there is no selection. Mirrors to PRIMARY on
    /// Linux so middle-click paste works, exactly like a mouse copy.
    pub(crate) fn vi_yank(&mut self, event_loop: &ActiveEventLoop) {
        let had_selection = self
            .pty
            .as_ref()
            .map(|p| p.term.lock().selection.is_some())
            .unwrap_or(false);
        if had_selection {
            self.copy_selection();
            #[cfg(target_os = "linux")]
            self.copy_selection_to_primary();
            self.push_toast("Yanked to clipboard");
        }
        self.vi_exit(event_loop);
    }

    /// Headless hook: when `GLASSY_VIMODE` is set, enter copy-mode at startup and
    /// start a visual selection so the keyboard cursor + range can be captured.
    /// The value picks the visual kind: `line` / `block`, anything else → char.
    pub(crate) fn maybe_headless_vimode(&mut self, event_loop: &ActiveEventLoop) {
        let Some(val) = std::env::var_os("GLASSY_VIMODE") else {
            return;
        };
        self.vi_enter(event_loop);
        // Move the cursor up + right a little so a non-trivial range is visible,
        // then drag a selection of the requested kind a few cells.
        let kind = match val.to_string_lossy().to_ascii_lowercase().as_str() {
            "line" => VisualKind::Line,
            "block" => VisualKind::Block,
            _ => VisualKind::Char,
        };
        // Seed the vi cursor a few rows up from the bottom so there is room to
        // grow a selection downward for the capture.
        if let Some(pty) = self.pty.as_ref() {
            let mut term = pty.term.lock();
            let off = term.grid().display_offset() as i32;
            let target = Point::new(Line(-off), Column(0));
            term.vi_goto_point(target);
        }
        self.vi_start_visual(kind, event_loop);
        // Extend the range a couple of cells/rows so it is visibly non-empty.
        self.vi_motion(ViMotion::Right);
        self.vi_motion(ViMotion::Right);
        self.vi_motion(ViMotion::Down);
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }
}

#[cfg(test)]
mod tests {
    use super::VisualKind;
    use alacritty_terminal::Term;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point, Side};
    use alacritty_terminal::selection::{Selection, SelectionType};
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::term::{Config, TermMode};
    use alacritty_terminal::vi_mode::ViMotion;
    use alacritty_terminal::vte::ansi::Handler;

    /// A minimal [`Dimensions`] for building a test [`Term`] without a PTY.
    struct Size {
        cols: usize,
        lines: usize,
    }
    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.lines
        }
        fn screen_lines(&self) -> usize {
            self.lines
        }
        fn columns(&self) -> usize {
            self.cols
        }
    }

    fn term(cols: usize, lines: usize) -> Term<VoidListener> {
        Term::new(Config::default(), &Size { cols, lines }, VoidListener)
    }

    /// Type a literal string into the term via the VTE handler, breaking on `\n`.
    fn type_str(t: &mut Term<VoidListener>, s: &str) {
        for ch in s.chars() {
            if ch == '\n' {
                t.linefeed();
                t.carriage_return();
            } else {
                t.input(ch);
            }
        }
    }

    #[test]
    fn visual_kind_maps_to_selection_type() {
        // The glassy-side VisualKind enum maps 1:1 to alacritty selection types;
        // this guards the mapping used by `vi_start_visual`.
        let expect = |k: VisualKind| match k {
            VisualKind::Char => Some(SelectionType::Simple),
            VisualKind::Line => Some(SelectionType::Lines),
            VisualKind::Block => Some(SelectionType::Block),
            VisualKind::None => None,
        };
        assert_eq!(expect(VisualKind::Char), Some(SelectionType::Simple));
        assert_eq!(expect(VisualKind::Line), Some(SelectionType::Lines));
        assert_eq!(expect(VisualKind::Block), Some(SelectionType::Block));
        assert_eq!(expect(VisualKind::None), None);
    }

    #[test]
    fn toggle_vi_mode_sets_mode_flag() {
        let mut t = term(20, 5);
        assert!(!t.mode().contains(TermMode::VI));
        t.toggle_vi_mode();
        assert!(t.mode().contains(TermMode::VI));
        t.toggle_vi_mode();
        assert!(!t.mode().contains(TermMode::VI));
    }

    #[test]
    fn charwise_visual_then_motion_selects_text() {
        // Type a word, enter vi mode, anchor a charwise selection at col 0 of the
        // last typed row, then move right across the word and copy.
        let mut t = term(20, 3);
        type_str(&mut t, "hello");
        t.toggle_vi_mode();
        // Park the vi cursor at the start of "hello".
        t.vi_goto_point(Point::new(Line(0), Column(0)));
        let anchor = t.vi_mode_cursor.point;
        // Seed a non-empty single-cell selection (Left→Right), exactly as
        // `vi_start_visual` does, so the recompute extends it on each motion.
        let mut sel = Selection::new(SelectionType::Simple, anchor, Side::Left);
        sel.update(anchor, Side::Right);
        t.selection = Some(sel);
        // Move to the end of the word (4 rights → 'o').
        for _ in 0..4 {
            t.vi_motion(ViMotion::Right);
        }
        let copied = t.selection_to_string().unwrap_or_default();
        assert_eq!(copied, "hello");
    }

    #[test]
    fn word_motion_jumps_by_word() {
        let mut t = term(40, 3);
        type_str(&mut t, "foo bar baz");
        t.toggle_vi_mode();
        t.vi_goto_point(Point::new(Line(0), Column(0)));
        // One WordRight lands on the start of "bar".
        t.vi_motion(ViMotion::WordRight);
        assert_eq!(t.vi_mode_cursor.point.column, Column(4));
    }

    #[test]
    fn copy_trims_trailing_whitespace() {
        // A row is space-padded to the grid width; selecting the whole row must
        // not copy the trailing padding (fidelity requirement (a1)).
        let mut t = term(20, 3);
        type_str(&mut t, "hi");
        let start = Point::new(Line(0), Column(0));
        let end = Point::new(Line(0), Column(19));
        let mut sel = Selection::new(SelectionType::Simple, start, Side::Left);
        sel.update(end, Side::Right);
        t.selection = Some(sel);
        let copied = t.selection_to_string().unwrap_or_default();
        assert_eq!(copied, "hi", "trailing pad must be trimmed");
    }

    #[test]
    fn copy_unwraps_soft_wrapped_line() {
        // A logical line wrapped across two rows (WRAPLINE flag on the last cell
        // of the first row) copies as ONE line — no inserted newline (fidelity
        // requirement (a2)).
        let mut t = term(5, 4);
        // Fill row 0 fully ("abcde") and mark it as wrapped, then "fg" on row 1.
        type_str(&mut t, "abcde");
        t.grid_mut()[Line(0)][Column(4)]
            .flags
            .insert(Flags::WRAPLINE);
        type_str(&mut t, "fg");
        let start = Point::new(Line(0), Column(0));
        let end = Point::new(Line(1), Column(1));
        let mut sel = Selection::new(SelectionType::Simple, start, Side::Left);
        sel.update(end, Side::Right);
        t.selection = Some(sel);
        let copied = t.selection_to_string().unwrap_or_default();
        assert_eq!(
            copied, "abcdefg",
            "soft-wrapped rows must join into one line"
        );
    }

    #[test]
    fn copy_keeps_genuine_linefeed() {
        // Two rows separated by a genuine line-feed (no WRAPLINE) keep the newline.
        let mut t = term(10, 4);
        type_str(&mut t, "ab\ncd");
        let start = Point::new(Line(0), Column(0));
        let end = Point::new(Line(1), Column(1));
        let mut sel = Selection::new(SelectionType::Simple, start, Side::Left);
        sel.update(end, Side::Right);
        t.selection = Some(sel);
        let copied = t.selection_to_string().unwrap_or_default();
        assert_eq!(copied, "ab\ncd", "real line-feeds keep their newline");
    }

    #[test]
    fn block_selection_yields_one_line_per_row() {
        // A rectangular (block) selection copies each row as its own line
        // (fidelity requirement (a3)). Build a 3x2 block over two rows.
        let mut t = term(10, 4);
        type_str(&mut t, "abcdef\nABCDEF");
        let start = Point::new(Line(0), Column(1));
        let end = Point::new(Line(1), Column(3));
        let mut sel = Selection::new(SelectionType::Block, start, Side::Left);
        sel.update(end, Side::Right);
        t.selection = Some(sel);
        let copied = t.selection_to_string().unwrap_or_default();
        // Columns 1..=3 of each row → "bcd" and "BCD", each on its own line.
        assert_eq!(copied, "bcd\nBCD");
    }
}
