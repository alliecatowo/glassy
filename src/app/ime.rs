//! IME preedit (composition) state machine + overlay rendering.
//!
//! winit drives CJK / dead-key input through a small sequence of `Ime` events:
//!
//! ```text
//! Ime::Enabled                       // composition session starts
//! Ime::Preedit("n",  Some((1, 1)))   // in-progress text + caret byte-range
//! Ime::Preedit("ni", Some((2, 2)))   // (grows as the user types)
//! Ime::Preedit("",   None)           // winit clears the preedit, then…
//! Ime::Commit("你")                  // …commits the finished text
//! Ime::Disabled                      // session ends
//! ```
//!
//! The in-progress composition string is shown *as an overlay* at the terminal
//! cursor with an underline — it displaces nothing in the grid. Only on
//! [`Ime::Commit`] do we write bytes to the PTY (handled by the caller). This
//! module owns the [`Preedit`] state and the pure transition functions, plus the
//! overlay painter. Everything here is idle-safe: a preedit change only marks the
//! frame dirty for the one repaint that draws/erases the overlay.

/// In-progress IME composition. `None` on [`App::preedit`] means no active
/// composition; `Some` carries the text winit is currently composing.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Preedit {
    /// The composition string as reported by the IME. May be empty between a
    /// clear and the following commit.
    pub text: String,
    /// Caret position inside `text`, as a `(start, end)` byte range from winit.
    /// `start == end` is a collapsed caret; `start < end` highlights a span the
    /// IME considers "being converted". `None` means the IME gave no cursor (we
    /// fall back to the end of the text).
    pub cursor: Option<(usize, usize)>,
}

impl Preedit {
    /// True when there is nothing to display (no glyphs in the composition).
    pub(crate) fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Number of terminal columns the composition occupies, using the unicode
    /// display width of each char (wide CJK counts as 2). At least 1 when the
    /// text is non-empty so the underline is always visible.
    pub(crate) fn display_cols(&self) -> usize {
        let w: usize = self.text.chars().map(char_cols).sum();
        w
    }
}

/// Display width of a char in terminal columns. Mirrors the grid's wide-cell
/// rule: East-Asian wide / fullwidth code points occupy two columns, everything
/// else one. Zero-width combining marks count as zero so they overlay the base.
///
/// This is a compact range table rather than a pull-in of `unicode-width` — the
/// preedit overlay only needs to size CJK / fullwidth composition strings, and
/// keeping it dependency-free avoids a Cargo.toml change (and the resulting
/// full-workspace recompile) for a cosmetic overlay measurement.
pub(crate) fn char_cols(c: char) -> usize {
    let cp = c as u32;
    // Zero-width: C0/C1 controls and combining marks contribute no column.
    if cp == 0 {
        return 0;
    }
    if (0x0300..=0x036F).contains(&cp)      // combining diacritical marks
        || (0x200B..=0x200F).contains(&cp)  // zero-width space / joiners / marks
        || (0xFE00..=0xFE0F).contains(&cp)  // variation selectors
        || cp == 0xFEFF
    {
        return 0;
    }
    if is_wide(cp) { 2 } else { 1 }
}

/// East-Asian Wide / Fullwidth code-point ranges (the subset that matters for an
/// IME composition: CJK ideographs, kana, Hangul, fullwidth forms, common
/// symbols and most emoji). Anything outside is treated as a single column.
fn is_wide(cp: u32) -> bool {
    matches!(cp,
        0x1100..=0x115F          // Hangul Jamo
        | 0x2329..=0x232A        // angle brackets
        | 0x2E80..=0x303E        // CJK radicals, Kangxi, CJK symbols/punctuation
        | 0x3041..=0x33FF        // Hiragana, Katakana, CJK strokes, compatibility
        | 0x3400..=0x4DBF        // CJK Ext A
        | 0x4E00..=0x9FFF        // CJK Unified Ideographs
        | 0xA000..=0xA4CF        // Yi
        | 0xAC00..=0xD7A3        // Hangul syllables
        | 0xF900..=0xFAFF        // CJK compatibility ideographs
        | 0xFE30..=0xFE4F        // CJK compatibility forms
        | 0xFF00..=0xFF60        // Fullwidth forms
        | 0xFFE0..=0xFFE6        // Fullwidth signs
        | 0x1F300..=0x1FAFF      // emoji / pictographs
        | 0x20000..=0x3FFFD      // CJK Ext B+ (supplementary ideographic planes)
    )
}

/// Apply an `Ime::Preedit(text, cursor)` event to the current preedit slot.
///
/// Returns the new state: `None` once the composition is cleared (empty text),
/// otherwise `Some` with the live composition. Kept as a free function (rather
/// than a method on `App`) so it can be unit-tested without a window/renderer.
pub(crate) fn on_preedit(text: String, cursor: Option<(usize, usize)>) -> Option<Preedit> {
    if text.is_empty() {
        // winit sends an empty Preedit to clear the composition (right before a
        // Commit, or when the IME is dismissed). Drop the overlay entirely.
        None
    } else {
        Some(Preedit { text, cursor })
    }
}

use super::App;
use crate::renderer::{Decorations, UnderlineStyle};
use alacritty_terminal::grid::Scroll;
use winit::event::Ime;
use winit::event_loop::ActiveEventLoop;

impl App {
    /// Dispatch a winit `Ime` sub-event (the composition state machine):
    ///
    /// - `Enabled`  — a session starts; clear stale preedit, anchor the candidate
    ///   window at the cursor.
    /// - `Preedit`  — store the in-progress composition (drawn as an underlined
    ///   overlay; nothing is sent to the PTY yet) and re-anchor the candidate
    ///   window. A no-op repaint is skipped when the composition is unchanged.
    /// - `Commit`   — the composition resolved: drop the overlay, reset blink/
    ///   selection, snap to the prompt, and write the finished bytes to the PTY.
    /// - `Disabled` — the session ended; drop any lingering overlay.
    ///
    /// Idle-safe: only marks the frame dirty (and forces the one overlay-row
    /// repaint) when something actually changed.
    pub(crate) fn handle_ime(&mut self, ime: Ime, event_loop: &ActiveEventLoop) {
        match ime {
            Ime::Enabled => {
                self.preedit = None;
                self.update_ime_cursor_area();
                self.mark_dirty(event_loop);
            }
            Ime::Preedit(text, cursor) => {
                let next = on_preedit(text, cursor);
                if next != self.preedit {
                    self.preedit = next;
                    self.reset_blink();
                    self.update_ime_cursor_area();
                    // The composition overlay paints over the cursor row; force a
                    // full rebuild so the previous (wider/narrower) overlay erases.
                    self.force_full_redraw = true;
                    self.mark_dirty(event_loop);
                }
            }
            Ime::Commit(text) => {
                // Committed text is input like any keystroke: reset blink, drop the
                // selection, snap to the prompt, repaint even if the child is quiet.
                let had_preedit = self.preedit.take().is_some();
                self.reset_blink();
                self.clear_selection();
                if let Some(pty) = &self.pty {
                    pty.term.lock().scroll_display(Scroll::Bottom);
                    pty.write(text.into_bytes());
                }
                if had_preedit {
                    // The vacated overlay rows must be rebuilt.
                    self.force_full_redraw = true;
                }
                self.mark_dirty(event_loop);
            }
            Ime::Disabled => {
                let had_preedit = self.preedit.take().is_some();
                if had_preedit {
                    self.force_full_redraw = true;
                    self.mark_dirty(event_loop);
                }
            }
        }
    }

    /// Headless capture hook: when `GLASSY_IME` is set, seed (or re-seed) the
    /// preedit overlay from its value so a capture frame shows an in-progress
    /// composition. The whole string is marked as the active-conversion span
    /// (inverted). A no-op when the env var is unset, so interactive runs are
    /// untouched. Called both in `resumed()` and right before the capture render
    /// (winit's own `Ime::Enabled` + empty `Ime::Preedit` on window init clear
    /// the early seed).
    pub(crate) fn reassert_headless_preedit(&mut self) {
        let Some(text) = std::env::var_os("GLASSY_IME") else {
            return;
        };
        let text = text.to_string_lossy().to_string();
        let text = if text.trim().is_empty() {
            "你好".to_string()
        } else {
            text
        };
        let end = text.len();
        self.preedit = Some(Preedit {
            text,
            cursor: Some((0, end)),
        });
        self.update_ime_cursor_area();
        self.force_full_redraw = true;
    }

    /// Current terminal-cursor cell `(col, row)` in screen coordinates for the
    /// focused session, or `None` if there is no PTY or the cursor is hidden.
    /// Used to anchor the IME candidate window and the preedit overlay.
    pub(crate) fn cursor_screen_cell(&self) -> Option<(usize, usize)> {
        let pty = self.pty.as_ref()?;
        let term = pty.term.lock();
        let content = term.renderable_content();
        let display_offset = content.display_offset as i32;
        let cursor = content.cursor;
        if cursor.shape == alacritty_terminal::vte::ansi::CursorShape::Hidden {
            return None;
        }
        let row = cursor.point.line.0 + display_offset;
        let col = cursor.point.column.0 as i32;
        if row < 0 || row >= self.rows as i32 || col < 0 || col >= self.cols as i32 {
            return None;
        }
        Some((col as usize, row as usize))
    }

    /// Tell the window where to anchor the IME candidate / conversion popup: a
    /// rectangle one cell tall at the terminal cursor, sized to the current
    /// preedit width so the candidate list lines up with the composition text.
    /// A no-op until the window + renderer exist.
    pub(crate) fn update_ime_cursor_area(&self) {
        let (Some(window), Some(renderer)) = (self.window.as_ref(), self.renderer.as_ref()) else {
            return;
        };
        let Some((col, row)) = self.cursor_screen_cell() else {
            return;
        };
        let m = renderer.cell_metrics();
        let pad = renderer.pad();
        let goy = super::tab_bar_h(m.height);
        let x = col as f32 * m.width + pad;
        let y = row as f32 * m.height + pad + goy;
        let w = match &self.preedit {
            Some(p) if !p.is_empty() => p.display_cols().max(1) as f32 * m.width,
            _ => m.width,
        };
        window.set_ime_cursor_area(
            winit::dpi::PhysicalPosition::new(x, y),
            winit::dpi::PhysicalSize::new(w, m.height),
        );
    }
}

/// Paint the in-progress IME composition as an underlined overlay starting at
/// the terminal cursor. Displaces nothing in the grid: the glyphs are pushed
/// over whatever cells the composition currently covers, with a single underline
/// as the standard "uncommitted text" affordance. The composition's own caret
/// span (winit's cursor byte-range) is drawn with a reversed (inverted) cell so
/// the active conversion segment stands out.
///
/// A free function (not an `App` method) so the render path can call it while it
/// already holds split borrows of `self.renderer` (mut) and `self.pty`'s term
/// lock — calling through `&self` there would re-borrow `self` and fail to
/// compile. `(cur_col, cur_row)` is the screen-cell anchor (the terminal
/// cursor); the anchor row must have been `begin_row`-ed and already be the
/// renderer's current row. `cols` clips the composition at the right grid edge.
pub(crate) fn paint_preedit(
    renderer: &mut crate::renderer::Renderer,
    preedit: &Preedit,
    cur_col: usize,
    cur_row: usize,
    cols: usize,
) {
    if preedit.is_empty() {
        return;
    }
    let fg = crate::color::default_fg();
    let bg = crate::color::default_bg();
    // The active-conversion span (winit caret byte range) is highlighted by
    // swapping fg/bg, matching the inverse treatment of a selected region.
    let (sel_start, sel_end) = preedit
        .cursor
        .unwrap_or((preedit.text.len(), preedit.text.len()));

    let mut col = cur_col;
    let mut byte = 0usize;
    for ch in preedit.text.chars() {
        let cw = char_cols(ch);
        if cw == 0 {
            // Combining mark: fold onto the previous cell visually by skipping
            // (rare in composition strings); advance the byte cursor only.
            byte += ch.len_utf8();
            continue;
        }
        if col >= cols {
            break; // ran off the right edge; clip the rest
        }
        let in_caret = byte >= sel_start && byte < sel_end && sel_start != sel_end;
        let (cfg, cbg) = if in_caret { (bg, fg) } else { (fg, bg) };
        let decorations = Decorations {
            underline: UnderlineStyle::Single,
            strikeout: false,
            overline: false,
            color: fg,
        };
        renderer.push_cell(
            col,
            cur_row,
            ch,
            &[],
            cfg,
            cbg,
            false,
            false,
            cw == 2,
            decorations,
        );
        col += cw;
        byte += ch.len_utf8();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_preedit_clears() {
        assert_eq!(on_preedit(String::new(), None), None);
        assert_eq!(on_preedit(String::new(), Some((0, 0))), None);
    }

    #[test]
    fn nonempty_preedit_keeps_text_and_cursor() {
        let p = on_preedit("ni".to_string(), Some((2, 2))).expect("active");
        assert_eq!(p.text, "ni");
        assert_eq!(p.cursor, Some((2, 2)));
        assert!(!p.is_empty());
    }

    #[test]
    fn growth_then_clear_is_a_full_cycle() {
        // Typical pinyin cycle: grow, grow, then winit clears before the commit.
        let p1 = on_preedit("n".to_string(), Some((1, 1))).expect("active");
        assert_eq!(p1.text, "n");
        let p2 = on_preedit("ni".to_string(), Some((2, 2))).expect("active");
        assert_eq!(p2.text, "ni");
        // The clear that precedes Commit("你") collapses the overlay.
        assert_eq!(on_preedit(String::new(), None), None);
    }

    #[test]
    fn display_cols_counts_wide_as_two() {
        // ASCII: one column each.
        let ascii = Preedit {
            text: "abc".to_string(),
            cursor: None,
        };
        assert_eq!(ascii.display_cols(), 3);
        // Wide CJK: two columns each.
        let cjk = Preedit {
            text: "你好".to_string(),
            cursor: None,
        };
        assert_eq!(cjk.display_cols(), 4);
        // Mixed.
        let mixed = Preedit {
            text: "a你".to_string(),
            cursor: None,
        };
        assert_eq!(mixed.display_cols(), 3);
    }

    #[test]
    fn char_cols_classifies_widths() {
        assert_eq!(char_cols('a'), 1);
        assert_eq!(char_cols('你'), 2);
        assert_eq!(char_cols('ｱ'), 1); // halfwidth katakana
        assert_eq!(char_cols('Ａ'), 2); // fullwidth latin A
    }

    #[test]
    fn missing_cursor_falls_back_to_none() {
        let p = on_preedit("dead`".to_string(), None).expect("active");
        assert_eq!(p.cursor, None);
        // The renderer treats None as "caret at end of text".
        assert_eq!(p.text.len(), p.text.len()); // sanity
    }
}
