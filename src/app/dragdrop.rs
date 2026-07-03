//! File drag-and-drop: OS-level `WindowEvent::HoveredFile`/`DroppedFile` support.
//!
//! winit reports a drag-and-drop of N files as N separate `DroppedFile` events
//! with no "batch end" marker, so `event_loop.rs` just queues each path into
//! `App::pending_drop_files` and this module's `flush_dropped_files` — called
//! once per `about_to_wait` wakeup — joins whatever queued up into ONE paste,
//! reusing the exact clipboard-paste primitive (`Pty::paste` + bracketed-paste +
//! broadcast fan-out) so a multi-file drop lands as a single space-separated,
//! shell-quoted paste rather than N separate ones.

use super::*;

/// Quote `path` as a single POSIX shell word: wrap it in single quotes, and
/// escape any embedded single quote as `'\''` (close the quote, emit a
/// backslash-escaped literal quote, reopen the quote). This is the standard
/// POSIX-safe quoting — unlike backslash-escaping alone, it needs no special
/// handling for spaces, globs, `$`, backticks, or other shell metacharacters:
/// everything between the quotes is taken literally except the quote itself.
pub(crate) fn quote_path_for_shell(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for ch in s.chars() {
        if ch == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(ch);
        }
    }
    out.push('\'');
    out
}

impl App {
    /// Flush every path queued in `pending_drop_files` (from one or more
    /// `WindowEvent::DroppedFile`) as a single paste: space-join each path
    /// (shell-quoted), then send it through the exact same path as a clipboard
    /// paste (`paste_clipboard` in `input.rs`) — same bracketed-paste check,
    /// same broadcast-to-every-pane fan-out, same target (`self.pty`, i.e. the
    /// ACTIVE tab's FOCUSED pane). Keeping this identical to `paste_clipboard`
    /// means a dropped file behaves exactly like a typed/pasted path: it is
    /// sanitized the same way (`Pty::paste` strips bracketed-paste markers and
    /// control sequences) and scrolls the viewport to the bottom the same way.
    /// A no-op if nothing is queued.
    pub(crate) fn flush_dropped_files(&mut self) {
        let files = std::mem::take(&mut self.pending_drop_files);
        if files.is_empty() {
            return;
        }
        let text = files
            .iter()
            .map(|p| quote_path_for_shell(p))
            .collect::<Vec<_>>()
            .join(" ");
        let bracketed = self.term_mode().contains(TermMode::BRACKETED_PASTE);
        if let Some(pty) = self.pty.as_ref() {
            pty.term.lock().scroll_display(Scroll::Bottom);
            // Honor broadcast input, exactly like `paste_clipboard`: a drop while
            // broadcasting reaches every pane of the active tab, not just the
            // focused one.
            if self.broadcast_input
                && let Some(g) = self.panes.as_ref()
            {
                pty.paste(&text, bracketed);
                for other in g.others.values() {
                    other.paste(&text, bracketed);
                }
            } else {
                pty.paste(&text, bracketed);
            }
        }
    }

    /// Paint the drop-hover affordance: a subtle accent-tinted rounded rect over
    /// `body` (the focused pane's content rect) while a file is being dragged
    /// over the window. Mirrors `paint_zoom_badge`'s translucent-accent-fill
    /// technique. Drawn as a low-alpha fill plus a slightly stronger ring so the
    /// affordance reads without overwhelming the terminal content beneath it.
    pub(crate) fn paint_drop_hover(renderer: &mut Renderer, body: pane::Rect) {
        let x = body.x as f32;
        let y = body.y as f32;
        let w = body.w as f32;
        let h = body.h as f32;
        let radius = 10.0;
        let a = color::accent();
        // Ring: a full-rect low-alpha accent fill underneath...
        renderer.push_overlay_rrect_px(x, y, w, h, radius, [a[0], a[1], a[2], 0.35]);
        // ...with a fainter inset fill on top, leaving a ring-like border effect
        // (mirrors the inset-rect technique `paint_tab_rename` uses for its focus
        // ring, in `chrome.rs`).
        let inset = 2.0;
        renderer.push_overlay_rrect_px(
            x + inset,
            y + inset,
            (w - 2.0 * inset).max(0.0),
            (h - 2.0 * inset).max(0.0),
            (radius - inset).max(0.0),
            [a[0], a[1], a[2], 0.08],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn quotes_plain_path() {
        assert_eq!(
            quote_path_for_shell(Path::new("/usr/bin/env")),
            "'/usr/bin/env'"
        );
    }

    #[test]
    fn quotes_path_with_spaces() {
        assert_eq!(
            quote_path_for_shell(Path::new("/home/user/my file.txt")),
            "'/home/user/my file.txt'"
        );
    }

    #[test]
    fn quotes_embedded_single_quote() {
        // it's.txt -> 'it'\''s.txt' (close, escaped literal quote, reopen).
        assert_eq!(
            quote_path_for_shell(Path::new("/home/user/it's.txt")),
            "'/home/user/it'\\''s.txt'"
        );
    }

    #[test]
    fn quotes_unicode_path() {
        assert_eq!(
            quote_path_for_shell(Path::new("/home/user/日本語ファイル.txt")),
            "'/home/user/日本語ファイル.txt'"
        );
    }

    #[test]
    fn quotes_empty_path() {
        assert_eq!(quote_path_for_shell(Path::new("")), "''");
    }

    #[test]
    fn quotes_path_starting_with_dash() {
        // A leading `-` must not be left unquoted where it could be misread as a
        // flag by whatever consumes the pasted text.
        assert_eq!(quote_path_for_shell(Path::new("-rf")), "'-rf'");
    }

    #[test]
    fn quotes_multiple_embedded_quotes() {
        assert_eq!(quote_path_for_shell(Path::new("''")), "''\\'''\\'''");
    }
}
