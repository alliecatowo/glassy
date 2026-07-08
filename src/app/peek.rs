//! Inline file peek: a small frosted "preview card" the shell can summon for a
//! markdown / text / source file via an OSC 1337 `Peek=<path>` request (e.g. a
//! `glassy-peek <file>` helper). The card shows the file's name plus a capped
//! head of its lines anchored to the bottom of the focused pane, and is
//! dismissed by the next keystroke / Esc / click.
//!
//! Deliberately modest (see the feature notes): this is a *plain-text / markdown*
//! peek, not a full syntax-highlighted viewer or an image-style scrollable
//! placement. It reuses the existing `push_overlay_rrect_px` /
//! `push_overlay_glyph_px_str` overlay primitives (same as `toast.rs`) so it
//! adds no GPU state, and it holds a static snapshot — no timer, no animation —
//! so the 0%-idle invariant is preserved (the card only repaints on the events
//! that also dismiss it).

use crate::app::App;
use crate::color;
use crate::gui;
use crate::renderer::Renderer;
use winit::event_loop::ActiveEventLoop;

/// Maximum bytes read from the previewed file (keeps a huge/looping file from
/// stalling the UI thread; we only ever show the first few lines anyway).
const MAX_BYTES: usize = 64 * 1024;
/// Maximum number of lines shown in the card body.
const MAX_LINES: usize = 14;
/// Maximum characters kept per line (longer lines are ellipsized).
const MAX_LINE_CHARS: usize = 100;

/// Corner radius of the peek card (px).
const CARD_RADIUS: f32 = 8.0;
/// Inner padding (px).
const PAD_X: f32 = 14.0;
const PAD_Y: f32 = 10.0;
/// Margin from the focused-pane edges (px).
const MARGIN: f32 = 16.0;

/// A built inline-preview snapshot: the title (file name) and the capped,
/// sanitized body lines to display. Owned, so it survives independently of the
/// file on disk and never re-reads.
pub(crate) struct Peek {
    /// Display title — the file's name (basename), or the full path if it has none.
    pub title: String,
    /// The body lines, already truncated to [`MAX_LINES`] and per-line clipped.
    pub lines: Vec<String>,
    /// Set when the source had more lines than we show, so the card can hint "…".
    pub truncated: bool,
}

impl Peek {
    /// Build a peek for `path` by reading a capped head of the file. Returns
    /// `None` when the file can't be read or looks binary (so we never splatter
    /// control bytes into the overlay). Pure aside from the single file read; the
    /// byte→lines transform is factored into [`lines_from_bytes`] for testing.
    pub(crate) fn from_path(path: &std::path::Path) -> Option<Peek> {
        let bytes = read_head(path, MAX_BYTES)?;
        if looks_binary(&bytes) {
            return None;
        }
        let (lines, truncated) = lines_from_bytes(&bytes, MAX_LINES, MAX_LINE_CHARS);
        if lines.is_empty() {
            return None;
        }
        let title = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_string)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Some(Peek {
            title,
            lines,
            truncated,
        })
    }

    /// Paint the peek card anchored to the bottom-left of the focused pane's body
    /// rect (`area`), clamped to stay on screen. A frosted glass card with a thin
    /// accent border + left stripe, a title row, then the body lines — mirroring
    /// the toast card styling for visual consistency.
    pub(crate) fn paint(&self, renderer: &mut Renderer, area: crate::pane::Rect) {
        let m = renderer.cell_metrics();
        let cell_h = m.height;
        let cell_w = m.width;

        // Widest displayed line (title counts too) sizes the card.
        let widest = self
            .lines
            .iter()
            .map(|l| l.chars().count())
            .chain(std::iter::once(self.title.chars().count() + 2))
            .max()
            .unwrap_or(0);
        let body_rows = self.lines.len() + 1 + usize::from(self.truncated); // +title (+ ellipsis)
        let card_w = (widest as f32 * cell_w + 2.0 * PAD_X)
            .min(area.w as f32 - 2.0 * MARGIN)
            .max(cell_w * 8.0);
        let card_h = (body_rows as f32 * cell_h + 2.0 * PAD_Y).min(area.h as f32 - 2.0 * MARGIN);

        // Bottom-left of the pane body, clamped into the pane.
        let card_x = area.x as f32 + MARGIN;
        let card_y = (area.y as f32 + area.h as f32 - MARGIN - card_h).max(area.y as f32 + MARGIN);

        // Frosted glass background: the shared E3 floating-surface fill
        // (gui::glass_float()), theme-aware so the card stays legible on light
        // themes — the old `bg*0.12+0.04` recipe painted a near-black card that
        // hid theme-dark foreground text on light backgrounds.
        let card_bg = gui::glass_float();
        let accent = color::accent();
        let border_c = [accent[0], accent[1], accent[2], 0.35];
        renderer.push_overlay_rrect_px(
            card_x - 0.5,
            card_y - 0.5,
            card_w + 1.0,
            card_h + 1.0,
            CARD_RADIUS + 0.5,
            border_c,
        );
        renderer.push_overlay_rrect_px(card_x, card_y, card_w, card_h, CARD_RADIUS, card_bg);
        // Left accent stripe.
        let stripe = [accent[0], accent[1], accent[2], 0.8];
        renderer.push_overlay_rrect_px(card_x, card_y, 3.0, card_h, CARD_RADIUS, stripe);

        // How many chars fit per line inside the padding.
        let max_chars = ((card_w - 2.0 * PAD_X) / cell_w).floor() as usize;
        let tx = (card_x + PAD_X).round();
        let mut ty = (card_y + PAD_Y).round();

        // Title row (accent, prefixed with a doc glyph). U+25A4 (▤) is BMP-safe;
        // it replaces U+1F5CE 🗎 which is outside the BMP and tofus on most
        // terminal fonts (the codebase bans non-BMP UI glyphs — see widgets.rs).
        let title = clip_to(&format!("\u{25A4} {}", self.title), max_chars);
        renderer.push_overlay_glyph_px_str(tx, ty, &title, accent);
        ty += cell_h;

        // Body lines (dim foreground).
        let fg = gui::fg();
        let body_fg = [fg[0], fg[1], fg[2], 0.92];
        for line in &self.lines {
            renderer.push_overlay_glyph_px_str(tx, ty.round(), &clip_to(line, max_chars), body_fg);
            ty += cell_h;
        }
        if self.truncated {
            let dim = gui::fg_dim();
            renderer.push_overlay_glyph_px_str(tx, ty.round(), "…", dim);
        }
    }
}

/// Read up to `cap` bytes from the start of `path`. Returns `None` on any IO
/// error (missing file, permission, etc.). Reading only a bounded head keeps a
/// giant or unbounded (FIFO/device) file from stalling the UI thread.
fn read_head(path: &std::path::Path, cap: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; cap];
    let mut n = 0;
    while n < cap {
        match f.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    buf.truncate(n);
    Some(buf)
}

/// Heuristic: treat the head as binary if it contains a NUL byte. Good enough to
/// avoid previewing executables/images while accepting all UTF-8 text + source.
fn looks_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0)
}

/// Convert raw file bytes into at most `max_lines` display lines, each clipped to
/// `max_chars` characters (ellipsizing longer lines). Tabs become two spaces and
/// any other control characters are dropped so nothing escapes into the overlay.
/// Returns `(lines, truncated)` where `truncated` is true if the source had more
/// than `max_lines` lines. Pure — the unit tests drive it directly.
fn lines_from_bytes(bytes: &[u8], max_lines: usize, max_chars: usize) -> (Vec<String>, bool) {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::new();
    let mut total = 0usize;
    for raw in text.lines() {
        total += 1;
        if out.len() >= max_lines {
            continue; // keep counting to detect truncation, but stop collecting
        }
        let mut cleaned = String::new();
        for ch in raw.chars() {
            match ch {
                '\t' => cleaned.push_str("  "),
                c if c.is_control() => {} // drop stray control chars
                c => cleaned.push(c),
            }
        }
        out.push(clip_to(&cleaned, max_chars));
    }
    let truncated = total > out.len();
    (out, truncated)
}

impl App {
    /// Build and show an inline peek card for `path` (from an OSC 1337 request).
    /// Relative paths are resolved against the active session's cwd so a
    /// `glassy-peek README.md` works from wherever the shell sits. A no-op (with a
    /// debug log) when the file can't be read or looks binary, so a bad request
    /// never disrupts the session.
    pub(crate) fn show_peek(&mut self, path: &std::path::Path) {
        let resolved = if path.is_absolute() {
            path.to_path_buf()
        } else if let Some(cwd) = self.active_cwd.as_ref() {
            cwd.join(path)
        } else {
            path.to_path_buf()
        };
        match Peek::from_path(&resolved) {
            Some(peek) => {
                self.peek = Some(peek);
                self.force_full_redraw = true;
            }
            None => {
                log::debug!("glassy: peek skipped (unreadable/binary): {resolved:?}");
            }
        }
    }

    /// Dismiss the inline peek card if one is showing. Returns `true` when a card
    /// was actually cleared (so the caller can force a repaint). Called on the
    /// next keystroke / Esc / click after a peek is summoned.
    pub(crate) fn dismiss_peek(&mut self, event_loop: &ActiveEventLoop) -> bool {
        if self.peek.take().is_some() {
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
            true
        } else {
            false
        }
    }

    /// Paint the active peek card (if any) over the focused pane's content rect.
    /// Called from both the single-pane and split render paths after the cell grid
    /// is drawn. A no-op when no peek is active.
    pub(crate) fn paint_peek(renderer: &mut Renderer, peek: &Peek, area: crate::pane::Rect) {
        peek.paint(renderer, area);
    }
}

/// Clip `s` to at most `max` characters, appending an ellipsis when it overflows.
fn clip_to(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_caps_count_and_flags_truncation() {
        let src = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (lines, truncated) = lines_from_bytes(src.as_bytes(), 5, 80);
        assert_eq!(lines.len(), 5);
        assert!(truncated);
        assert_eq!(lines[0], "line 0");
    }

    #[test]
    fn lines_not_truncated_when_within_cap() {
        let (lines, truncated) = lines_from_bytes(b"a\nb\nc", 10, 80);
        assert_eq!(lines, vec!["a", "b", "c"]);
        assert!(!truncated);
    }

    #[test]
    fn long_line_is_ellipsized() {
        let long = "x".repeat(200);
        let (lines, _) = lines_from_bytes(long.as_bytes(), 5, 10);
        assert_eq!(lines[0].chars().count(), 10);
        assert!(lines[0].ends_with('…'));
    }

    #[test]
    fn tabs_expand_and_controls_drop() {
        let (lines, _) = lines_from_bytes(b"a\tb\x07c", 5, 80);
        // Tab -> two spaces, BEL (\x07) dropped.
        assert_eq!(lines[0], "a  bc");
    }

    #[test]
    fn binary_detected_by_nul() {
        assert!(looks_binary(b"\x00\x01\x02"));
        assert!(!looks_binary(b"plain text"));
    }

    #[test]
    fn clip_to_handles_edges() {
        assert_eq!(clip_to("hello", 0), "");
        assert_eq!(clip_to("hello", 10), "hello");
        assert_eq!(clip_to("hello", 3), "he…");
    }

    #[test]
    fn from_path_reads_a_real_temp_file() {
        // Write a small file into the session scratch dir and peek it.
        let dir = std::env::temp_dir();
        let path = dir.join(format!("glassy-peek-test-{}.md", std::process::id()));
        std::fs::write(&path, b"# Title\n\nsome body\n").unwrap();
        let peek = Peek::from_path(&path).expect("peek built");
        assert!(peek.title.ends_with(".md"));
        assert_eq!(peek.lines[0], "# Title");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn from_path_rejects_missing_file() {
        let path = std::path::Path::new("/nonexistent/glassy/peek/path.md");
        assert!(Peek::from_path(path).is_none());
    }
}
