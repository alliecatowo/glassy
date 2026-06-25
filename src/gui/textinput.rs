//! The reusable single-line editable [`Ui::text_input`] widget.
//!
//! Immediate-mode like the rest of the toolkit: the editable model
//! ([`TextEdit`]) is owned by the caller and threaded in per frame. This method
//! draws the field (sunken track, selection highlight, scrolled text, blinking
//! caret, focus ring, placeholder) and resolves *mouse* interaction
//! (click-to-place caret, click-drag to select, double-click word-select).
//! Keyboard editing is driven by the host translating keypresses into
//! [`EditOp`]s on the same [`TextEdit`] — keys arrive between frames, not during
//! paint, exactly like the search / palette / rename editors today.
//!
//! The widget owns NO clipboard access; a Ctrl+C/X/V the host routes in as an
//! `EditOp`/`ClipReq` is serviced via the app's existing `arboard` plumbing.

use super::*;

/// Persistent per-field mouse state the caller carries across frames so a
/// click-drag selection and a double-click word-select work in immediate mode
/// (where each frame is otherwise stateless). One per editable field.
#[derive(Clone, Copy, Debug, Default)]
pub struct TextInputMouse {
    /// True while a left-drag selection is in progress (press latched on us).
    pub dragging: bool,
}

impl<'r> Ui<'r> {
    /// Draw + drive an editable single-line text field bound to `edit`.
    ///
    /// * `wid` — stable widget id (focus + press latch).
    /// * `rect` — the field bounds in physical px.
    /// * `edit` — the caller-owned editing model (caret / selection / scroll).
    /// * `ms` — caller-owned mouse drag state (carried across frames).
    /// * `placeholder` — dim hint shown when the buffer is empty.
    /// * `blink_on` — current cursor-blink phase (reuse the app blink timing);
    ///   pass `true` for a steady caret.
    /// * `double_click` — true on the frame a double-click landed on this field
    ///   (the caller computes it from its existing click-chain timing); selects
    ///   the word under the pointer.
    ///
    /// Returns whether the field is hovered (so the caller can set an I-beam
    /// cursor). All text/clipboard/submit signals flow through the host's key
    /// handler, not here — this method is mouse + paint only.
    #[allow(clippy::too_many_arguments)]
    pub fn text_input(
        &mut self,
        wid: WidgetId,
        rect: Rect,
        edit: &mut TextEdit,
        ms: &mut TextInputMouse,
        placeholder: &str,
        blink_on: bool,
        double_click: bool,
    ) -> bool {
        let it = self.interact(wid, rect, true);
        let m = self.m;
        let pad = m.pad;
        let cell_w = m.cell_w;
        let focused = self.is_focused(wid);

        // Inner text region (inset by pad on each side; reserve a caret column on
        // the right so the caret at end isn't clipped).
        let text_x = rect.x + pad;
        let text_w = (rect.w - 2.0 * pad).max(0.0);
        let visible_cols = (text_w / cell_w).floor().max(0.0) as usize;

        // --- Mouse: caret placement / drag-select / double-click ------------
        let (mx, _my) = self.mouse_pos();
        // Char index under pointer x, given the current scroll.
        let col_at = |px: f32| -> usize {
            let rel = ((px - text_x) / cell_w).round();
            let rel = rel.max(0.0) as usize;
            (edit.scroll + rel).min(edit.len())
        };
        if double_click && it.hovered {
            let pos = col_at(mx);
            let (lo, hi) = edit.word_bounds(pos);
            edit.select_range(lo, hi);
            ms.dragging = false;
        } else if it.pressed {
            // Press began on us this frame → start a fresh caret/selection; a held
            // press → extend the selection (drag).
            let pos = col_at(mx);
            if !ms.dragging {
                edit.place_caret(pos, false);
                ms.dragging = true;
            } else {
                edit.place_caret(pos, true);
            }
        } else {
            ms.dragging = false;
        }

        // --- Keep the caret in view, then resolve the visible window --------
        let scroll = edit.ensure_caret_visible(visible_cols);
        let chars: Vec<char> = edit.text().chars().collect();
        let end = (scroll + visible_cols).min(chars.len());
        let visible: String = chars[scroll..end].iter().collect();

        // --- Paint: sunken track + (focus ring) -----------------------------
        self.rrect(rect, m.radius, track_off());
        if focused {
            self.focus_ring(rect, m.radius);
        }

        let ty = (rect.center_y() - m.cell_h * 0.5).round();

        // Selection highlight (behind the glyphs), clipped to the visible window.
        if let Some((lo, hi)) = edit.selection() {
            let vlo = lo.max(scroll);
            let vhi = hi.min(end);
            if vhi > vlo {
                let sx = text_x + (vlo - scroll) as f32 * cell_w;
                let sw = (vhi - vlo) as f32 * cell_w;
                self.rrect(Rect::new(sx.round(), ty, sw, m.cell_h), 2.0, sel_bg());
            }
        }

        // Text or placeholder.
        if chars.is_empty() && !placeholder.is_empty() {
            self.label(text_x.round(), ty, placeholder, fg_dim());
        } else {
            self.label(text_x.round(), ty, &visible, fg());
        }

        // Blinking caret (only when focused). A thin 2px accent bar at the caret
        // column; hidden on the blink-off phase so it reuses the app's blink
        // timer without introducing a new one.
        if focused && (blink_on || ms.dragging) {
            let cc = edit.caret().clamp(scroll, end);
            let cx = text_x + (cc - scroll) as f32 * cell_w;
            self.quad(Rect::new(cx.round(), ty, 2.0, m.cell_h), color::accent());
        }

        it.hovered
    }
}

/// The result of mapping one keypress against an editable field: either an
/// in-buffer [`EditOp`], a clipboard request, a commit/cancel, or nothing (the
/// host should let the key fall through). Lets all four editors share one key
/// table instead of each re-implementing Backspace / arrows / word-jump.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TextInputAction {
    /// Apply this edit to the buffer.
    Edit(EditOp),
    /// Copy the current selection to the clipboard (Ctrl+C).
    Copy,
    /// Cut the current selection (Ctrl+X): copy then delete.
    Cut,
    /// Paste (Ctrl+V): host fetches clipboard text and inserts it.
    Paste,
    /// Enter — commit.
    Submit,
    /// Esc — cancel.
    Cancel,
    /// Not a field key; the host should ignore / pass it through.
    None,
}

/// A clipboard request raised when applying a [`TextInputAction`]. The editor
/// model can't touch the OS clipboard, so it reports the intent and the caller
/// pumps text through the app's existing `arboard` plumbing: on
/// [`Self::Copy`]/[`Self::Cut`] the caller copies `text` out; on a paste it
/// fetches clipboard text and feeds it back (via the `paste_text` argument of
/// [`apply_text_action`]).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum ClipReq {
    /// No clipboard interaction this frame.
    #[default]
    None,
    /// Copy this string (the current selection) to the OS clipboard.
    Copy(String),
    /// Copy this string to the clipboard; the selection was already removed.
    Cut(String),
    /// The user pressed paste; the caller should fetch clipboard text and insert.
    Paste,
}

/// Outcome of applying one [`TextInputAction`] to a [`TextEdit`]: what changed
/// plus any clipboard work the host must service. Returned by
/// [`apply_text_action`] so every editor (settings fields, search, palette,
/// rename) drives its buffer through one shared code path.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextActionResult {
    /// The buffer text changed (insert / delete / paste / cut).
    pub changed: bool,
    /// Enter — the host should commit.
    pub submit: bool,
    /// Esc — the host should cancel.
    pub cancel: bool,
    /// A clipboard request the host services with its `arboard` plumbing.
    pub clip: ClipReq,
}

/// Apply one [`TextInputAction`] to `edit`, returning what happened (see
/// [`TextActionResult`]). Clipboard requests are reported, not performed: on a
/// [`TextInputAction::Cut`] the selection is removed here and its prior text is
/// handed back via [`ClipReq::Cut`]; on a [`TextInputAction::Paste`] the host
/// passes the fetched clipboard text in `paste_text` (already fetched before the
/// call, since the model can't touch the OS clipboard). Centralising this keeps
/// every glassy text field's edit/clipboard semantics identical.
pub fn apply_text_action(
    edit: &mut TextEdit,
    action: TextInputAction,
    paste_text: Option<&str>,
) -> TextActionResult {
    let mut r = TextActionResult::default();
    match action {
        TextInputAction::Edit(op) => {
            r.changed = edit.apply(op);
        }
        TextInputAction::Copy => {
            if let Some(sel) = edit.selected_text() {
                r.clip = ClipReq::Copy(sel);
            }
        }
        TextInputAction::Cut => {
            if let Some(sel) = edit.selected_text() {
                edit.apply(EditOp::Backspace);
                r.changed = true;
                r.clip = ClipReq::Cut(sel);
            }
        }
        TextInputAction::Paste => {
            if let Some(t) = paste_text {
                r.changed = edit.apply(EditOp::Insert(t.to_string()));
            } else {
                r.clip = ClipReq::Paste;
            }
        }
        TextInputAction::Submit => r.submit = true,
        TextInputAction::Cancel => r.cancel = true,
        TextInputAction::None => {}
    }
    r
}

/// Map a logical key + modifier flags onto a [`TextInputAction`]. Pure (no
/// winit dependency beyond the borrowed key string) so it is unit-testable and
/// shared by every editor. `ctrl`/`shift` are the live modifier flags; `text`
/// is the printable string for a `Key::Character` (already filtered of control
/// chars by the caller), or `None` for named keys.
///
/// `named` is a lowercase name for the named key (e.g. "backspace", "arrowleft",
/// "home"); callers pass `None` for character keys.
pub fn map_text_key(
    named: Option<&str>,
    text: Option<&str>,
    ctrl: bool,
    shift: bool,
) -> TextInputAction {
    use TextInputAction as A;
    if let Some(name) = named {
        return match name {
            "escape" => A::Cancel,
            "enter" => A::Submit,
            "space" => A::Edit(EditOp::Insert(" ".to_string())),
            "backspace" if ctrl => A::Edit(EditOp::DeleteWordBack),
            "backspace" => A::Edit(EditOp::Backspace),
            "delete" if ctrl => A::Edit(EditOp::DeleteWordForward),
            "delete" => A::Edit(EditOp::Delete),
            "arrowleft" if ctrl => A::Edit(EditOp::WordLeft { select: shift }),
            "arrowleft" => A::Edit(EditOp::Left { select: shift }),
            "arrowright" if ctrl => A::Edit(EditOp::WordRight { select: shift }),
            "arrowright" => A::Edit(EditOp::Right { select: shift }),
            "home" => A::Edit(EditOp::Home { select: shift }),
            "end" => A::Edit(EditOp::End { select: shift }),
            _ => A::None,
        };
    }
    if let Some(s) = text {
        // Ctrl shortcuts: copy / cut / paste / select-all. Compared case-folded
        // so Ctrl+Shift+C (an uppercase 'C') still maps.
        if ctrl {
            return match s.to_ascii_lowercase().as_str() {
                "c" => A::Copy,
                "x" => A::Cut,
                "v" => A::Paste,
                "a" => A::Edit(EditOp::SelectAll),
                _ => A::None,
            };
        }
        if !s.is_empty() {
            return A::Edit(EditOp::Insert(s.to_string()));
        }
    }
    A::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_named_keys() {
        assert_eq!(
            map_text_key(Some("escape"), None, false, false),
            TextInputAction::Cancel
        );
        assert_eq!(
            map_text_key(Some("enter"), None, false, false),
            TextInputAction::Submit
        );
        assert_eq!(
            map_text_key(Some("backspace"), None, false, false),
            TextInputAction::Edit(EditOp::Backspace)
        );
        assert_eq!(
            map_text_key(Some("backspace"), None, true, false),
            TextInputAction::Edit(EditOp::DeleteWordBack)
        );
        assert_eq!(
            map_text_key(Some("arrowleft"), None, false, true),
            TextInputAction::Edit(EditOp::Left { select: true })
        );
        assert_eq!(
            map_text_key(Some("arrowright"), None, true, false),
            TextInputAction::Edit(EditOp::WordRight { select: false })
        );
        assert_eq!(
            map_text_key(Some("home"), None, false, true),
            TextInputAction::Edit(EditOp::Home { select: true })
        );
    }

    #[test]
    fn map_clipboard_keys() {
        assert_eq!(
            map_text_key(None, Some("c"), true, false),
            TextInputAction::Copy
        );
        assert_eq!(
            map_text_key(None, Some("x"), true, false),
            TextInputAction::Cut
        );
        assert_eq!(
            map_text_key(None, Some("v"), true, false),
            TextInputAction::Paste
        );
        assert_eq!(
            map_text_key(None, Some("a"), true, false),
            TextInputAction::Edit(EditOp::SelectAll)
        );
        // Ctrl+Shift+C (uppercase) still copies.
        assert_eq!(
            map_text_key(None, Some("C"), true, true),
            TextInputAction::Copy
        );
    }

    #[test]
    fn map_printable_inserts() {
        assert_eq!(
            map_text_key(None, Some("a"), false, false),
            TextInputAction::Edit(EditOp::Insert("a".to_string()))
        );
        // Unknown ctrl char falls through.
        assert_eq!(
            map_text_key(None, Some("z"), true, false),
            TextInputAction::None
        );
    }

    #[test]
    fn text_input_mouse_default_is_idle() {
        let ms = TextInputMouse::default();
        assert!(!ms.dragging);
    }

    #[test]
    fn clip_req_default_is_none() {
        assert_eq!(ClipReq::default(), ClipReq::None);
    }

    // ---- apply_text_action: the shared key→buffer pipe every editor uses -----

    #[test]
    fn action_edit_inserts_and_reports_changed() {
        let mut e = TextEdit::new("");
        let r = apply_text_action(&mut e, map_text_key(None, Some("z"), false, false), None);
        assert!(r.changed);
        assert_eq!(e.text(), "z");
        assert_eq!(r.clip, ClipReq::None);
        assert!(!r.submit && !r.cancel);
    }

    #[test]
    fn action_submit_and_cancel_flagged_not_changed() {
        let mut e = TextEdit::new("hi");
        let submit = apply_text_action(&mut e, TextInputAction::Submit, None);
        assert!(submit.submit && !submit.changed);
        let cancel = apply_text_action(&mut e, TextInputAction::Cancel, None);
        assert!(cancel.cancel && !cancel.changed);
        assert_eq!(e.text(), "hi"); // neither mutated the buffer
    }

    #[test]
    fn action_copy_reports_selection_without_mutating() {
        let mut e = TextEdit::new("hello");
        e.select_range(0, 3);
        let r = apply_text_action(&mut e, TextInputAction::Copy, None);
        assert_eq!(r.clip, ClipReq::Copy("hel".to_string()));
        assert!(!r.changed);
        assert_eq!(e.text(), "hello");
    }

    #[test]
    fn action_cut_removes_selection_and_reports_text() {
        let mut e = TextEdit::new("hello");
        e.select_range(0, 3);
        let r = apply_text_action(&mut e, TextInputAction::Cut, None);
        assert_eq!(r.clip, ClipReq::Cut("hel".to_string()));
        assert!(r.changed);
        assert_eq!(e.text(), "lo");
    }

    #[test]
    fn action_paste_inserts_supplied_text() {
        let mut e = TextEdit::new("ab");
        e.place_caret(1, false);
        let r = apply_text_action(&mut e, TextInputAction::Paste, Some("XY"));
        assert!(r.changed);
        assert_eq!(e.text(), "aXYb");
        assert_eq!(r.clip, ClipReq::None);
    }

    #[test]
    fn action_paste_without_text_requests_clipboard() {
        let mut e = TextEdit::new("ab");
        let r = apply_text_action(&mut e, TextInputAction::Paste, None);
        assert_eq!(r.clip, ClipReq::Paste);
        assert!(!r.changed);
        assert_eq!(e.text(), "ab");
    }

    #[test]
    fn action_none_is_inert() {
        let mut e = TextEdit::new("ab");
        let r = apply_text_action(&mut e, TextInputAction::None, None);
        assert_eq!(r, TextActionResult::default());
        assert_eq!(e.text(), "ab");
    }

    #[test]
    fn ctrl_a_then_paste_replaces_whole_buffer() {
        // The end-to-end "select-all, paste" path the editors rely on.
        let mut e = TextEdit::new("old value");
        apply_text_action(&mut e, map_text_key(None, Some("a"), true, false), None);
        assert_eq!(e.selection(), Some((0, 9)));
        let r = apply_text_action(&mut e, TextInputAction::Paste, Some("new"));
        assert!(r.changed);
        assert_eq!(e.text(), "new");
    }
}
