//! Kitty-style "hints" mode (a.k.a. "hyperlink hints" / "quick select").
//!
//! A keyboard-triggered overlay that scans the visible terminal grid for
//! actionable targets — URLs, file paths, git SHAs, and IP addresses — labels
//! each with a short home-row mnemonic (`a`, `s`, `d`, `f`, …), and performs the
//! target's natural action when its label is typed:
//!
//!   * URL  (`http(s)://`, `file://`)  → open via the system handler (`open_url`).
//!   * Path (`/foo`, `~/bar`)          → copy the path to the clipboard.
//!   * git SHA (7–40 hex)              → copy to the clipboard.
//!   * IPv4 address                    → copy to the clipboard.
//!
//! The URL/path scan reuses the hand-rolled scanner in [`super::selection`]
//! (`scan_row_for_links`); the git-SHA / IP detection lives here as pure
//! functions so it is trivially unit-testable and never touches the grid lock.
//!
//! Idle-safe: the mode is only active while [`App::hints`] is `Some`, and every
//! state change marks the frame dirty exactly once — there is no animation, so
//! the loop returns to `ControlFlow::Wait` (0% idle) the moment the overlay is
//! painted. Closing the mode forces a full redraw to wipe the labels cleanly.
//!
//! Headless hook: `GLASSY_HINTS=1` opens the mode at startup for a
//! `GLASSY_CAPTURE` frame.

use super::*;
use winit::keyboard::{Key, NamedKey};

/// What a labelled hint target does when its label is typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HintAction {
    /// Open a URL with the system handler (`http(s)://`, `file://`).
    OpenUrl(String),
    /// Copy a string (file path / git SHA / IP) to the clipboard.
    Copy(String),
}

/// One labelled match on the visible grid.
#[derive(Debug, Clone)]
pub(crate) struct Hint {
    /// The assigned label (lowercase, e.g. `a`, `sf`).
    pub label: String,
    /// Screen cell column the label is anchored at (start of the match).
    pub col: usize,
    /// Screen cell row the label is anchored at.
    pub row: usize,
    /// The action performed when this hint's label is fully typed.
    pub action: HintAction,
}

/// Live state for an open hints overlay. Present exactly while the mode is on.
pub(crate) struct HintsState {
    /// All labelled matches found on the visible grid when the mode opened.
    pub hints: Vec<Hint>,
    /// The label characters typed so far (lowercase), narrowing the candidates.
    pub typed: String,
}

impl HintsState {
    /// Whether any hints are left to draw at all (used to skip the overlay).
    fn is_empty(&self) -> bool {
        self.hints.is_empty()
    }
}

/// The default label alphabet: home-row first, then the rest of the QWERTY
/// letters, so a handful of targets get single comfortable keys before any 2-char
/// labels.
const LABEL_ALPHABET: &str = "asdfghjklqwertyuiopzxcvbnm";

/// Generate `n` distinct labels from `alphabet`. The first `alphabet.len()` are
/// single characters; beyond that we fall back to fixed-width 2-character labels
/// drawn from the same alphabet (e.g. the default 26 letters cover 26 + 676 = 702
/// targets, far more than a screen ever holds). Labels are lowercase. `alphabet`
/// is assumed to hold >= 2 distinct lowercase ASCII letters (the config layer
/// guarantees this; the default does too).
pub(crate) fn make_labels_from(alphabet: &str, n: usize) -> Vec<String> {
    let alpha: Vec<char> = alphabet.chars().collect();
    let base = alpha.len();
    if base == 0 {
        return Vec::new();
    }
    if n <= base {
        return alpha.iter().take(n).map(|&c| c.to_string()).collect();
    }
    // Need 2-char labels for the whole set so no single-char label is a prefix
    // of a 2-char one (which would make the single one un-typeable).
    let mut out = Vec::with_capacity(n);
    'outer: for &a in &alpha {
        for &b in &alpha {
            out.push(format!("{a}{b}"));
            if out.len() == n {
                break 'outer;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Pure target scanners (git SHA + IPv4). URLs/paths reuse `selection.rs`.
// ---------------------------------------------------------------------------

/// A raw target found on a row, before a label is assigned.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RawTarget {
    pub col_start: usize,
    pub action: HintAction,
}

/// Scan a row's plain text for git SHAs and IPv4 addresses. `col_map[i]` maps the
/// `i`-th char of `text` to its terminal column (same contract as
/// [`super::selection::scan_plain_links`]); `\0` sentinels break runs.
pub(crate) fn scan_shas_and_ips(text: &str, col_map: &[usize]) -> Vec<RawTarget> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        // Only start a token at a word boundary (prev char is not part of a token).
        let at_boundary = i == 0 || !is_token_char(chars[i - 1]);
        if !at_boundary {
            i += 1;
            continue;
        }
        // Consume a maximal token of token chars.
        let start = i;
        while i < n && is_token_char(chars[i]) {
            i += 1;
        }
        let token: String = chars[start..i].iter().collect();
        if let Some(action) = classify_token(&token) {
            out.push(RawTarget {
                col_start: col_map[start],
                action,
            });
        }
        // Skip the separator so the next loop starts at a real boundary.
        i += 1;
    }
    out
}

/// Characters that make up a SHA / IP token (hex digits, dots for IPs).
fn is_token_char(c: char) -> bool {
    c.is_ascii_hexdigit() || c == '.'
}

/// Classify a bare token as a git SHA or an IPv4 address, if it is one.
fn classify_token(token: &str) -> Option<HintAction> {
    if is_ipv4(token) {
        return Some(HintAction::Copy(token.to_string()));
    }
    if is_git_sha(token) {
        return Some(HintAction::Copy(token.to_string()));
    }
    None
}

/// A git short/long SHA: 7–40 lowercase hex digits (uppercase-only or mixed are
/// rejected to avoid matching e.g. decimal numbers that happen to be hex, and to
/// match git's own lowercase output). Must contain at least one a–f digit so a
/// pure run of decimal digits (a line number, a port) is not mistaken for a SHA.
pub(crate) fn is_git_sha(token: &str) -> bool {
    let len = token.len();
    if !(7..=40).contains(&len) {
        return false;
    }
    let mut has_alpha = false;
    for b in token.bytes() {
        match b {
            b'0'..=b'9' => {}
            b'a'..=b'f' => has_alpha = true,
            _ => return false,
        }
    }
    has_alpha
}

/// An IPv4 dotted-quad: four 0–255 octets joined by dots, no leading-zero-padded
/// octets longer than their value would allow (we just bound each to 255).
pub(crate) fn is_ipv4(token: &str) -> bool {
    let mut parts = 0;
    for octet in token.split('.') {
        parts += 1;
        if parts > 4 {
            return false;
        }
        if octet.is_empty() || octet.len() > 3 {
            return false;
        }
        match octet.parse::<u16>() {
            Ok(v) if v <= 255 && octet.bytes().all(|b| b.is_ascii_digit()) => {}
            _ => return false,
        }
    }
    parts == 4
}

impl App {
    /// Open hints mode: scan the visible grid for targets, assign labels, and
    /// force a full redraw so the overlay paints. A no-op (with a toast) when no
    /// targets are found, so the mode never opens to an empty screen.
    pub(crate) fn open_hints(&mut self, event_loop: &ActiveEventLoop) {
        if self.pty.is_none() {
            return;
        }
        let targets = self.collect_hint_targets();
        if targets.is_empty() {
            self.push_toast("No hints on screen");
            self.mark_dirty(event_loop);
            return;
        }
        let alphabet = self.config.hints_chars.as_deref().unwrap_or(LABEL_ALPHABET);
        let labels = make_labels_from(alphabet, targets.len());
        let hints = targets
            .into_iter()
            .zip(labels)
            .map(|((col, row, action), label)| Hint {
                label,
                col,
                row,
                action,
            })
            .collect();
        self.hints = Some(HintsState {
            hints,
            typed: String::new(),
        });
        // Labels are an overlay over the whole grid; force a full repaint so the
        // terminal pixels under them are resident this frame, then on close.
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Close hints mode (Esc / completed action / lost focus). Forces a full
    /// redraw so the labels are wiped, then returns to the 0%-idle `Wait` path.
    pub(crate) fn close_hints(&mut self, event_loop: &ActiveEventLoop) {
        if self.hints.take().is_some() {
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
        }
    }

    /// Whether the hints overlay is currently open.
    pub(crate) fn hints_open(&self) -> bool {
        self.hints.is_some()
    }

    /// Scan every visible row for actionable targets, in reading order
    /// (top-to-bottom, left-to-right). Returns `(col, row, action)` triples; the
    /// caller assigns labels. URL/path targets come from the shared link scanner;
    /// SHA/IP targets from the local pure scanner. Within a row, a column already
    /// claimed by a URL/path is not re-claimed by a SHA/IP (URL wins).
    fn collect_hint_targets(&self) -> Vec<(usize, usize, HintAction)> {
        let mut out = Vec::new();
        for row in 0..self.rows {
            // URLs + paths via the shared scanner (also yields OSC8-aware spans).
            let links = self.scan_row_for_links(row);
            // Track claimed column ranges so SHA/IP inside a URL isn't double-listed.
            let mut claimed: Vec<(usize, usize)> = Vec::new();
            for link in &links {
                let action = if link.uri.starts_with("file://") {
                    // A bare path: copy the on-disk path (decoded), not the URI.
                    HintAction::Copy(decode_file_uri(&link.uri))
                } else {
                    HintAction::OpenUrl(link.uri.clone())
                };
                out.push((link.col_start, row, action));
                claimed.push((link.col_start, link.col_end));
            }
            // SHA / IP via the local scanner, skipping anything inside a claim.
            let (text, col_map) = self.row_text(row);
            for t in scan_shas_and_ips(&text, &col_map) {
                let inside = claimed
                    .iter()
                    .any(|&(s, e)| t.col_start >= s && t.col_start < e);
                if !inside {
                    out.push((t.col_start, row, t.action));
                }
            }
        }
        // Stable reading order: row then column.
        out.sort_by_key(|&(col, row, _)| (row, col));
        out
    }

    /// Build the `(text, col_map)` pair for a visible row, matching the contract
    /// of [`super::selection::scan_plain_links`] (one char per column, `\0`
    /// sentinel for OSC8 cells). Hoisted here so the SHA/IP scan shares the exact
    /// column mapping the link scan uses.
    fn row_text(&self, row: usize) -> (String, Vec<usize>) {
        let Some(pty) = self.pty.as_ref() else {
            return (String::new(), Vec::new());
        };
        if row >= self.rows {
            return (String::new(), Vec::new());
        }
        let display_offset = pty.term.lock().grid().display_offset() as i32;
        let point_line = alacritty_terminal::index::Line(row as i32 - display_offset);
        let term = pty.term.lock();
        let grid = term.grid();
        let cols = self.cols;
        let mut text = String::with_capacity(cols);
        let mut col_map: Vec<usize> = Vec::with_capacity(cols);
        for col in 0..cols {
            let pt = alacritty_terminal::index::Point::new(
                point_line,
                alacritty_terminal::index::Column(col),
            );
            let cell = &grid[pt];
            if cell.hyperlink().is_some() {
                text.push('\0');
                col_map.push(col);
                continue;
            }
            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            text.push(ch);
            col_map.push(col);
        }
        (text, col_map)
    }

    /// Handle a key while hints mode owns the keyboard. Returns `true` if the key
    /// was consumed (it always is while the overlay is open). Esc cancels; a label
    /// char narrows the candidate set and fires the action on a unique full match;
    /// Backspace un-types; any non-label char cancels.
    pub(crate) fn handle_hints_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) -> bool {
        match key {
            Key::Named(NamedKey::Escape) => {
                self.close_hints(event_loop);
            }
            Key::Named(NamedKey::Backspace) => {
                if let Some(st) = self.hints.as_mut() {
                    st.typed.pop();
                    self.mark_dirty(event_loop);
                }
            }
            Key::Character(s) => {
                let ch = s.chars().next().unwrap_or(' ').to_ascii_lowercase();
                if ch.is_ascii_alphabetic() {
                    self.push_hint_char(ch, event_loop);
                } else {
                    // Not a label char: cancel the mode (matches kitty).
                    self.close_hints(event_loop);
                }
            }
            // Any other key (arrows, function keys, …) cancels.
            _ => self.close_hints(event_loop),
        }
        // The overlay always consumes the key while it is open.
        true
    }

    /// Append `ch` to the typed prefix; if it now uniquely identifies a hint, fire
    /// its action and close. If no hint matches the new prefix, cancel.
    fn push_hint_char(&mut self, ch: char, event_loop: &ActiveEventLoop) {
        let Some(st) = self.hints.as_mut() else {
            return;
        };
        let mut candidate = st.typed.clone();
        candidate.push(ch);
        let remaining: Vec<&Hint> = st
            .hints
            .iter()
            .filter(|h| h.label.starts_with(&candidate))
            .collect();
        if remaining.is_empty() {
            // Dead end: nothing starts with this prefix — cancel cleanly.
            self.close_hints(event_loop);
            return;
        }
        // Exact unique match: fire it.
        if let Some(exact) = remaining.iter().find(|h| h.label == candidate)
            && remaining.len() == 1
        {
            let action = exact.action.clone();
            self.close_hints(event_loop);
            self.perform_hint_action(action);
            return;
        }
        // Still ambiguous: commit the char and repaint.
        st.typed = candidate;
        self.mark_dirty(event_loop);
    }

    /// Perform a hint's action: open a URL, or copy text to the clipboard (with a
    /// confirming toast). Mirrors the Ctrl+Click / copy paths so behaviour is
    /// identical no matter how a target is activated.
    fn perform_hint_action(&mut self, action: HintAction) {
        match action {
            HintAction::OpenUrl(uri) => {
                Self::open_url(&uri);
                self.push_toast(format!("Opened {}", truncate_for_toast(&uri)));
            }
            HintAction::Copy(text) => {
                if let Some(cb) = self.clipboard()
                    && let Err(e) = cb.set_text(text.clone())
                {
                    log::debug!("hints copy failed: {e}");
                }
                self.push_toast(format!("Copied {}", truncate_for_toast(&text)));
            }
        }
    }

    /// Snapshot the live hints for the render path: `(label, col, row, dimmed)`,
    /// where `dimmed` is true for hints whose label no longer matches the typed
    /// prefix (drawn faded so the user sees what's being narrowed). Returns
    /// `None` when the mode is closed. Cheap clone so the renderer borrow is free
    /// of `self`.
    pub(crate) fn hints_snapshot(&self) -> Option<Vec<(String, usize, usize, bool)>> {
        let st = self.hints.as_ref()?;
        if st.is_empty() {
            return Some(Vec::new());
        }
        Some(
            st.hints
                .iter()
                .map(|h| {
                    let active = h.label.starts_with(&st.typed);
                    (h.label.clone(), h.col, h.row, !active)
                })
                .collect(),
        )
    }
}

impl App {
    /// Paint the hints overlay: a small rounded chip with the label glyphs at the
    /// start cell of each match. Matches still consistent with the typed prefix
    /// draw in the accent color on a bright chip; narrowed-out matches draw dimmed
    /// so the user sees the set shrinking. Static (takes `&mut Renderer` + an owned
    /// snapshot) so it composes inside the live renderer borrow in `render`.
    ///
    /// `snapshot` is `(label, col, row, dimmed)` from [`App::hints_snapshot`].
    pub(crate) fn paint_hints(renderer: &mut Renderer, snapshot: &[(String, usize, usize, bool)]) {
        if snapshot.is_empty() {
            return;
        }
        let m = renderer.cell_metrics();
        let cell_w = m.width;
        let cell_h = m.height;
        let pad = renderer.pad();
        let goy = renderer.grid_origin_y();
        // Chip colors: bright accent-tinted float for active, recessed for dimmed.
        let chip_active = gui::glass_float();
        let chip_dim = {
            let mut c = gui::glass_float();
            c[3] *= 0.5;
            c
        };
        let fg_active = color::accent();
        let fg_dim = {
            let mut c = color::default_fg();
            c[3] *= 0.45;
            c
        };
        // Draw dimmed chips first so active chips composite on top of any overlap.
        for pass_dimmed in [true, false] {
            for (label, col, row, dimmed) in snapshot {
                if *dimmed != pass_dimmed {
                    continue;
                }
                let x = *col as f32 * cell_w + pad;
                let y = *row as f32 * cell_h + pad + goy;
                let label_w = label.chars().count() as f32 * cell_w;
                // A snug chip with a small horizontal inset so it reads as a pill.
                let chip_pad = (cell_w * 0.25).round();
                let (chip, fg) = if *dimmed {
                    (chip_dim, fg_dim)
                } else {
                    (chip_active, fg_active)
                };
                renderer.push_overlay_rrect_px(
                    x - chip_pad,
                    y,
                    label_w + chip_pad * 2.0,
                    cell_h,
                    (cell_h * 0.22).round().max(2.0),
                    chip,
                );
                renderer.push_overlay_glyph_px_str(x, y, &label.to_uppercase(), fg);
            }
        }
    }
}

/// Truncate a long string for a toast body so the toast stays one line.
fn truncate_for_toast(s: &str) -> String {
    const MAX: usize = 48;
    if s.chars().count() <= MAX {
        return s.to_string();
    }
    let head: String = s.chars().take(MAX - 1).collect();
    format!("{head}…")
}

/// Decode a `file://` URI back to a plain on-disk path for clipboard copy. Undoes
/// the percent-encoding `selection.rs` applies; falls back to the raw remainder if
/// a sequence is malformed.
fn decode_file_uri(uri: &str) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    let bytes = path.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generate `n` labels from the built-in default alphabet (test convenience).
    fn make_labels(n: usize) -> Vec<String> {
        make_labels_from(LABEL_ALPHABET, n)
    }

    #[test]
    fn labels_single_char_under_alphabet() {
        let labels = make_labels(3);
        assert_eq!(labels, vec!["a", "s", "d"]);
    }

    #[test]
    fn labels_home_row_first() {
        let labels = make_labels(8);
        assert_eq!(labels, vec!["a", "s", "d", "f", "g", "h", "j", "k"]);
    }

    #[test]
    fn labels_switch_to_two_chars_when_over_alphabet() {
        let n = LABEL_ALPHABET.chars().count() + 5;
        let labels = make_labels(n);
        assert_eq!(labels.len(), n);
        // Every label is now exactly 2 chars (so no single-char is a prefix).
        assert!(labels.iter().all(|l| l.len() == 2));
        assert_eq!(labels[0], "aa");
        // All labels distinct.
        let mut sorted = labels.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), n);
    }

    #[test]
    fn labels_zero() {
        assert!(make_labels(0).is_empty());
    }

    #[test]
    fn labels_custom_alphabet() {
        let labels = make_labels_from("jk", 4);
        // 2-letter alphabet: 2 singles then 2-char combos (no single is a prefix).
        assert_eq!(labels, vec!["jj", "jk", "kj", "kk"]);
    }

    #[test]
    fn labels_no_single_is_prefix_of_two_char() {
        // When labels overflow to 2-char form, every label is the same width so a
        // single-char label can never shadow a 2-char one during incremental typing.
        let n = LABEL_ALPHABET.chars().count() + 1;
        let labels = make_labels(n);
        for a in &labels {
            for b in &labels {
                if a != b {
                    assert!(!b.starts_with(a.as_str()), "{a} is a prefix of {b}");
                }
            }
        }
    }

    #[test]
    fn git_sha_short_and_long() {
        assert!(is_git_sha("a1b2c3d")); // 7
        assert!(is_git_sha("2eacb59"));
        assert!(is_git_sha(&"a".repeat(40))); // 40
    }

    #[test]
    fn git_sha_rejects_too_short_long_and_decimal() {
        assert!(!is_git_sha("a1b2c")); // 5 chars
        assert!(!is_git_sha(&"a".repeat(41))); // 41
        assert!(!is_git_sha("1234567")); // all decimal, no a-f
        assert!(!is_git_sha("g123456")); // 'g' not hex
        assert!(!is_git_sha("A1B2C3D")); // uppercase rejected
    }

    #[test]
    fn ipv4_valid() {
        assert!(is_ipv4("127.0.0.1"));
        assert!(is_ipv4("255.255.255.0"));
        assert!(is_ipv4("8.8.8.8"));
    }

    #[test]
    fn ipv4_rejects_bad() {
        assert!(!is_ipv4("256.0.0.1")); // octet > 255
        assert!(!is_ipv4("1.2.3")); // 3 parts
        assert!(!is_ipv4("1.2.3.4.5")); // 5 parts
        assert!(!is_ipv4("1.2.3.")); // trailing empty
        assert!(!is_ipv4("a.b.c.d")); // non-numeric
        assert!(!is_ipv4("1.2.3.4444")); // octet too long
    }

    fn col_map_identity(n: usize) -> Vec<usize> {
        (0..n).collect()
    }

    #[test]
    fn scan_finds_sha_and_ip() {
        let text = "commit 2eacb59 at host 192.168.1.10 ok";
        let cm = col_map_identity(text.chars().count());
        let found = scan_shas_and_ips(text, &cm);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].action, HintAction::Copy("2eacb59".to_string()));
        assert_eq!(found[0].col_start, 7);
        assert_eq!(
            found[1].action,
            HintAction::Copy("192.168.1.10".to_string())
        );
    }

    #[test]
    fn scan_ignores_plain_words_and_decimals() {
        let text = "the answer is 42 and 1000000 done";
        let cm = col_map_identity(text.chars().count());
        let found = scan_shas_and_ips(text, &cm);
        assert!(found.is_empty(), "no SHA/IP in plain decimal text");
    }

    #[test]
    fn scan_respects_word_boundary() {
        // A SHA glued to a longer token (so prev char is a token char) is not a
        // standalone match — the maximal token includes the leading letters and
        // fails classification.
        let text = "x2eacb59";
        let cm = col_map_identity(text.chars().count());
        let found = scan_shas_and_ips(text, &cm);
        // 'x' is not a token char, so the token is "2eacb59" starting at col 1.
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].col_start, 1);
    }

    #[test]
    fn scan_breaks_on_sentinel() {
        let text = "2eac\0b59abc";
        let cm = col_map_identity(text.chars().count());
        let found = scan_shas_and_ips(text, &cm);
        // Neither side is a valid 7+ char SHA on its own.
        assert!(found.is_empty());
    }

    #[test]
    fn decode_file_uri_roundtrip() {
        let p = crate::app::percent_encode_path("/home/user/my file.txt");
        let uri = format!("file://{p}");
        assert_eq!(decode_file_uri(&uri), "/home/user/my file.txt");
    }

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate_for_toast("short"), "short");
        let long = "x".repeat(100);
        let t = truncate_for_toast(&long);
        assert!(t.chars().count() <= 48);
        assert!(t.ends_with('…'));
    }
}
