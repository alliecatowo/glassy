//! Pure, renderer-free editing model for the single-line [`Ui::text_input`]
//! widget. All caret / selection / edit-op logic lives here so it can be unit
//! tested without a GPU, a window, or an event loop. The widget in
//! [`super::widgets`] owns one of these (via the caller) and renders it; key and
//! mouse handling call the methods below.
//!
//! Indices are **character** offsets (not bytes): a caret position `c` means
//! "before the c-th char" so `0..=len` are valid. Selection is an optional
//! `(anchor, caret)` pair; the visible span is `min..max`. Word boundaries use a
//! simple alnum/underscore-vs-other rule, matching common single-line fields.

/// One editing action the host translates a keypress into. Kept abstract so the
/// pure model never depends on winit key types.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditOp {
    /// Insert a string at the caret (replacing any selection).
    Insert(String),
    /// Delete backward one char (or the selection).
    Backspace,
    /// Delete forward one char (or the selection).
    Delete,
    /// Delete the word before the caret (or the selection).
    DeleteWordBack,
    /// Delete the word after the caret (or the selection).
    DeleteWordForward,
    /// Move caret left one char. `select` extends the selection.
    Left { select: bool },
    /// Move caret right one char. `select` extends the selection.
    Right { select: bool },
    /// Move caret to the previous word boundary. `select` extends.
    WordLeft { select: bool },
    /// Move caret to the next word boundary. `select` extends.
    WordRight { select: bool },
    /// Move caret to the start. `select` extends.
    Home { select: bool },
    /// Move caret to the end. `select` extends.
    End { select: bool },
    /// Select the whole buffer.
    SelectAll,
}

/// The editable model behind a text field: the text plus the caret, optional
/// selection anchor, and horizontal scroll offset (in chars) that the widget
/// keeps in sync so the caret stays visible.
#[derive(Clone, Debug, Default)]
pub struct TextEdit {
    /// The current value as a flat char vector (so caret math is O(1) and never
    /// splits a UTF-8 sequence).
    chars: Vec<char>,
    /// Caret position: number of chars before it (`0..=chars.len()`).
    caret: usize,
    /// Selection anchor when a selection is active; `None` for no selection.
    anchor: Option<usize>,
    /// Horizontal scroll offset in chars (first visible char index). Owned here
    /// so it survives between frames; the widget updates it to keep the caret in
    /// the visible window.
    pub scroll: usize,
    /// Optional hard cap on the char count (e.g. tab titles). `None` = unbounded.
    pub max_len: Option<usize>,
}

/// True for chars that are part of a "word" (alphanumeric or `_`). Everything
/// else (spaces, punctuation) is a word separator for Ctrl+Left/Right jumps and
/// word-delete.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl TextEdit {
    /// A fresh editor seeded with `initial`, caret at the end, no selection.
    pub fn new(initial: &str) -> Self {
        let chars: Vec<char> = initial.chars().collect();
        let caret = chars.len();
        TextEdit {
            chars,
            caret,
            anchor: None,
            scroll: 0,
            max_len: None,
        }
    }

    /// A fresh editor with a max char count.
    pub fn with_max_len(initial: &str, max_len: usize) -> Self {
        let mut e = Self::new(initial);
        e.max_len = Some(max_len);
        e
    }

    /// The current value as an owned `String`.
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// True when the buffer is empty (drives placeholder rendering).
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    /// Char length of the buffer.
    pub fn len(&self) -> usize {
        self.chars.len()
    }

    /// The caret position in chars.
    pub fn caret(&self) -> usize {
        self.caret
    }

    /// The active selection as `(start, end)` char offsets (start <= end), or
    /// `None` when there is no selection (or it is empty).
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        let (lo, hi) = (a.min(self.caret), a.max(self.caret));
        if lo == hi { None } else { Some((lo, hi)) }
    }

    /// The currently-selected substring, or `None` when nothing is selected.
    pub fn selected_text(&self) -> Option<String> {
        self.selection()
            .map(|(lo, hi)| self.chars[lo..hi].iter().collect())
    }

    /// Replace the whole value (used when the host seeds/clears the field).
    pub fn set_text(&mut self, s: &str) {
        self.chars = s.chars().collect();
        self.caret = self.chars.len();
        self.anchor = None;
        self.clamp();
    }

    /// Clamp caret + anchor into range (after any external mutation).
    fn clamp(&mut self) {
        let n = self.chars.len();
        if self.caret > n {
            self.caret = n;
        }
        if let Some(a) = self.anchor
            && a > n
        {
            self.anchor = Some(n);
        }
        if self.scroll > n {
            self.scroll = n;
        }
    }

    /// Start (or continue) a selection extend: ensure an anchor exists at the
    /// current caret before the caret moves.
    fn ensure_anchor(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some(self.caret);
        }
    }

    /// Delete the active selection, leaving the caret at its start. Returns
    /// `true` if anything was removed.
    fn delete_selection(&mut self) -> bool {
        if let Some((lo, hi)) = self.selection() {
            self.chars.drain(lo..hi);
            self.caret = lo;
            self.anchor = None;
            true
        } else {
            self.anchor = None;
            false
        }
    }

    /// Index of the previous word boundary from `from` (skipping trailing
    /// separators, then the word). Returns 0 at the start.
    fn prev_word(&self, from: usize) -> usize {
        let mut i = from;
        while i > 0 && !is_word_char(self.chars[i - 1]) {
            i -= 1;
        }
        while i > 0 && is_word_char(self.chars[i - 1]) {
            i -= 1;
        }
        i
    }

    /// Index of the next word boundary from `from` (skipping the current word,
    /// then following separators). Returns `len` at the end. Used for caret
    /// movement (Ctrl+Right), which lands *after* the trailing separators.
    fn next_word(&self, from: usize) -> usize {
        let n = self.chars.len();
        let mut i = from;
        while i < n && is_word_char(self.chars[i]) {
            i += 1;
        }
        while i < n && !is_word_char(self.chars[i]) {
            i += 1;
        }
        i
    }

    /// End index of the run forward-word-delete should remove from `from`: the
    /// contiguous run of the same char class (word vs separator) starting at
    /// `from`. Unlike [`Self::next_word`] this does NOT also eat the following
    /// separators, so Ctrl+Delete removes one word (or one separator run) at a
    /// time, mirroring [`Self::prev_word`]'s single-word backward delete.
    fn word_delete_end(&self, from: usize) -> usize {
        let n = self.chars.len();
        if from >= n {
            return n;
        }
        let on_word = is_word_char(self.chars[from]);
        let mut i = from;
        while i < n && is_word_char(self.chars[i]) == on_word {
            i += 1;
        }
        i
    }

    /// The `(start, end)` char range of the word under / adjacent to `pos`
    /// (used by double-click word-select). When `pos` is on a separator the
    /// run of separators is selected instead, matching common editors.
    pub fn word_bounds(&self, pos: usize) -> (usize, usize) {
        let n = self.chars.len();
        if n == 0 {
            return (0, 0);
        }
        let p = pos.min(n.saturating_sub(1));
        let on_word = is_word_char(self.chars[p]);
        let mut lo = p;
        while lo > 0 && is_word_char(self.chars[lo - 1]) == on_word {
            lo -= 1;
        }
        let mut hi = p;
        while hi < n && is_word_char(self.chars[hi]) == on_word {
            hi += 1;
        }
        (lo, hi)
    }

    /// Place the caret at char index `pos`, optionally extending the selection
    /// (click vs shift-click / drag).
    pub fn place_caret(&mut self, pos: usize, select: bool) {
        let pos = pos.min(self.chars.len());
        if select {
            self.ensure_anchor();
        } else {
            self.anchor = None;
        }
        self.caret = pos;
    }

    /// Select the char range `[lo, hi]` and place the caret at `hi` (double-click
    /// word-select / drag-select). A zero-width range clears the selection.
    pub fn select_range(&mut self, lo: usize, hi: usize) {
        let n = self.chars.len();
        let lo = lo.min(n);
        let hi = hi.min(n);
        if lo == hi {
            self.anchor = None;
            self.caret = lo;
        } else {
            self.anchor = Some(lo);
            self.caret = hi;
        }
    }

    /// Apply one [`EditOp`], returning `true` when the buffer text changed (so
    /// the host knows to re-run any dependent effect). Pure moves return `false`.
    pub fn apply(&mut self, op: EditOp) -> bool {
        match op {
            EditOp::Insert(s) => {
                self.delete_selection();
                let mut changed = false;
                for c in s.chars() {
                    if c == '\n' || c == '\r' {
                        continue; // single-line field: ignore newlines
                    }
                    if let Some(max) = self.max_len
                        && self.chars.len() >= max
                    {
                        break;
                    }
                    self.chars.insert(self.caret, c);
                    self.caret += 1;
                    changed = true;
                }
                changed
            }
            EditOp::Backspace => {
                if self.delete_selection() {
                    return true;
                }
                if self.caret > 0 {
                    self.caret -= 1;
                    self.chars.remove(self.caret);
                    true
                } else {
                    false
                }
            }
            EditOp::Delete => {
                if self.delete_selection() {
                    return true;
                }
                if self.caret < self.chars.len() {
                    self.chars.remove(self.caret);
                    true
                } else {
                    false
                }
            }
            EditOp::DeleteWordBack => {
                if self.delete_selection() {
                    return true;
                }
                let to = self.prev_word(self.caret);
                if to < self.caret {
                    self.chars.drain(to..self.caret);
                    self.caret = to;
                    true
                } else {
                    false
                }
            }
            EditOp::DeleteWordForward => {
                if self.delete_selection() {
                    return true;
                }
                let to = self.word_delete_end(self.caret);
                if to > self.caret {
                    self.chars.drain(self.caret..to);
                    true
                } else {
                    false
                }
            }
            EditOp::Left { select } => {
                if select {
                    self.ensure_anchor();
                    self.caret = self.caret.saturating_sub(1);
                } else if let Some((lo, _)) = self.selection() {
                    self.caret = lo;
                    self.anchor = None;
                } else {
                    self.caret = self.caret.saturating_sub(1);
                }
                false
            }
            EditOp::Right { select } => {
                let n = self.chars.len();
                if select {
                    self.ensure_anchor();
                    self.caret = (self.caret + 1).min(n);
                } else if let Some((_, hi)) = self.selection() {
                    self.caret = hi;
                    self.anchor = None;
                } else {
                    self.caret = (self.caret + 1).min(n);
                }
                false
            }
            EditOp::WordLeft { select } => {
                if select {
                    self.ensure_anchor();
                } else {
                    self.anchor = None;
                }
                self.caret = self.prev_word(self.caret);
                false
            }
            EditOp::WordRight { select } => {
                if select {
                    self.ensure_anchor();
                } else {
                    self.anchor = None;
                }
                self.caret = self.next_word(self.caret);
                false
            }
            EditOp::Home { select } => {
                if select {
                    self.ensure_anchor();
                } else {
                    self.anchor = None;
                }
                self.caret = 0;
                false
            }
            EditOp::End { select } => {
                if select {
                    self.ensure_anchor();
                } else {
                    self.anchor = None;
                }
                self.caret = self.chars.len();
                false
            }
            EditOp::SelectAll => {
                if self.chars.is_empty() {
                    self.anchor = None;
                } else {
                    self.anchor = Some(0);
                    self.caret = self.chars.len();
                }
                false
            }
        }
    }

    /// Recompute the horizontal scroll so the caret stays within a window of
    /// `visible_cols` chars, and return the (possibly updated) scroll offset.
    /// Called by the widget once the field width is known.
    pub fn ensure_caret_visible(&mut self, visible_cols: usize) -> usize {
        if visible_cols == 0 {
            self.scroll = self.caret;
            return self.scroll;
        }
        if self.caret < self.scroll {
            self.scroll = self.caret;
        } else if self.caret >= self.scroll + visible_cols {
            self.scroll = self.caret + 1 - visible_cols;
        }
        // Don't scroll past what's needed to show the tail. The caret occupies
        // its own column past the last char, so the effective content length is
        // `len + 1` — clamping to bare `len` would hide a caret sitting at the end.
        let max_scroll = (self.chars.len() + 1).saturating_sub(visible_cols);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
        self.scroll
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ins(e: &mut TextEdit, s: &str) -> bool {
        e.apply(EditOp::Insert(s.to_string()))
    }

    #[test]
    fn new_seeds_caret_at_end() {
        let e = TextEdit::new("hello");
        assert_eq!(e.text(), "hello");
        assert_eq!(e.caret(), 5);
        assert!(e.selection().is_none());
    }

    #[test]
    fn insert_at_caret() {
        let mut e = TextEdit::new("");
        assert!(ins(&mut e, "ab"));
        assert_eq!(e.text(), "ab");
        e.place_caret(1, false);
        ins(&mut e, "X");
        assert_eq!(e.text(), "aXb");
        assert_eq!(e.caret(), 2);
    }

    #[test]
    fn insert_ignores_newlines() {
        let mut e = TextEdit::new("");
        ins(&mut e, "a\nb\rc");
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn max_len_caps_insert() {
        let mut e = TextEdit::with_max_len("", 3);
        ins(&mut e, "abcdef");
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn backspace_and_delete() {
        let mut e = TextEdit::new("abc");
        e.place_caret(2, false);
        assert!(e.apply(EditOp::Backspace));
        assert_eq!(e.text(), "ac");
        assert_eq!(e.caret(), 1);
        assert!(e.apply(EditOp::Delete));
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut e = TextEdit::new("abc");
        e.place_caret(0, false);
        assert!(!e.apply(EditOp::Backspace));
        assert_eq!(e.text(), "abc");
    }

    #[test]
    fn shift_left_right_select() {
        let mut e = TextEdit::new("hello");
        e.place_caret(5, false);
        e.apply(EditOp::Left { select: true });
        e.apply(EditOp::Left { select: true });
        assert_eq!(e.selection(), Some((3, 5)));
        assert_eq!(e.selected_text().as_deref(), Some("lo"));
        // Plain right collapses to the right edge of the selection.
        e.apply(EditOp::Right { select: false });
        assert!(e.selection().is_none());
        assert_eq!(e.caret(), 5);
    }

    #[test]
    fn home_end_with_select() {
        let mut e = TextEdit::new("abcdef");
        e.place_caret(3, false);
        e.apply(EditOp::Home { select: true });
        assert_eq!(e.selection(), Some((0, 3)));
        e.apply(EditOp::End { select: true });
        // Anchor stayed at 3; caret moved to end.
        assert_eq!(e.selection(), Some((3, 6)));
    }

    #[test]
    fn typing_replaces_selection() {
        let mut e = TextEdit::new("hello");
        e.select_range(0, 5);
        assert!(ins(&mut e, "bye"));
        assert_eq!(e.text(), "bye");
        assert!(e.selection().is_none());
    }

    #[test]
    fn word_jump_left_right() {
        let mut e = TextEdit::new("foo bar baz");
        e.place_caret(11, false);
        e.apply(EditOp::WordLeft { select: false });
        assert_eq!(e.caret(), 8); // start of "baz"
        e.apply(EditOp::WordLeft { select: false });
        assert_eq!(e.caret(), 4); // start of "bar"
        e.apply(EditOp::WordRight { select: false });
        assert_eq!(e.caret(), 8); // past "bar" + space
    }

    #[test]
    fn delete_word_back_forward() {
        let mut e = TextEdit::new("foo bar baz");
        e.place_caret(7, false); // after "bar"
        assert!(e.apply(EditOp::DeleteWordBack));
        assert_eq!(e.text(), "foo  baz");
        let mut e2 = TextEdit::new("foo bar baz");
        e2.place_caret(0, false);
        assert!(e2.apply(EditOp::DeleteWordForward));
        assert_eq!(e2.text(), " bar baz");
    }

    #[test]
    fn word_bounds_double_click() {
        let e = TextEdit::new("foo bar baz");
        assert_eq!(e.word_bounds(5), (4, 7)); // "bar"
        assert_eq!(e.word_bounds(0), (0, 3)); // "foo"
        // On a separator: selects the separator run.
        assert_eq!(e.word_bounds(3), (3, 4));
    }

    #[test]
    fn select_all() {
        let mut e = TextEdit::new("hello");
        e.apply(EditOp::SelectAll);
        assert_eq!(e.selection(), Some((0, 5)));
        let mut empty = TextEdit::new("");
        empty.apply(EditOp::SelectAll);
        assert!(empty.selection().is_none());
    }

    #[test]
    fn delete_selection_via_backspace() {
        let mut e = TextEdit::new("hello world");
        e.select_range(5, 11); // " world"
        assert!(e.apply(EditOp::Backspace));
        assert_eq!(e.text(), "hello");
        assert_eq!(e.caret(), 5);
    }

    #[test]
    fn scroll_keeps_caret_visible() {
        let mut e = TextEdit::new("0123456789");
        e.place_caret(10, false);
        let s = e.ensure_caret_visible(4);
        // Caret at 10, window of 4 → first visible char is 7.
        assert_eq!(s, 7);
        e.place_caret(0, false);
        let s = e.ensure_caret_visible(4);
        assert_eq!(s, 0);
    }

    #[test]
    fn left_collapses_selection_to_left_edge() {
        let mut e = TextEdit::new("hello");
        e.select_range(1, 4);
        e.apply(EditOp::Left { select: false });
        assert!(e.selection().is_none());
        assert_eq!(e.caret(), 1);
    }

    #[test]
    fn set_text_resets_caret_and_selection() {
        let mut e = TextEdit::new("hello");
        e.select_range(0, 5);
        e.set_text("hi");
        assert_eq!(e.text(), "hi");
        assert_eq!(e.caret(), 2);
        assert!(e.selection().is_none());
    }

    #[test]
    fn delete_word_forward_takes_one_word_not_the_space() {
        // Forward word-delete removes the word run only, leaving the separator —
        // mirroring backward word-delete (regression guard).
        let mut e = TextEdit::new("foo bar baz");
        e.place_caret(0, false);
        assert!(e.apply(EditOp::DeleteWordForward));
        assert_eq!(e.text(), " bar baz");
        // On a separator run it removes just the separators.
        let mut e2 = TextEdit::new("foo   bar");
        e2.place_caret(3, false);
        assert!(e2.apply(EditOp::DeleteWordForward));
        assert_eq!(e2.text(), "foobar");
    }

    #[test]
    fn right_collapses_selection_to_right_edge() {
        let mut e = TextEdit::new("hello");
        e.select_range(1, 4);
        e.apply(EditOp::Right { select: false });
        assert!(e.selection().is_none());
        assert_eq!(e.caret(), 4);
    }

    #[test]
    fn scroll_shows_caret_at_end_within_window() {
        // A caret sitting past the last char must stay visible (its own column),
        // so scroll is allowed to exceed len - visible_cols (regression guard).
        let mut e = TextEdit::new("0123456789");
        e.place_caret(10, false);
        assert_eq!(e.ensure_caret_visible(4), 7);
        // A caret left of the current window pulls the scroll back to it.
        e.place_caret(2, false);
        assert_eq!(e.ensure_caret_visible(4), 2);
        // From a reset scroll, a mid-buffer caret inside the window needs none.
        e.scroll = 0;
        e.place_caret(2, false);
        assert_eq!(e.ensure_caret_visible(4), 0);
    }
}
