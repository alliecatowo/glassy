//! Plain-text URL/path detection.
//!
//! Scans the visible terminal grid for `http://`, `https://`, `file://`, and
//! bare UNIX paths (starting with `/` or `~/`).  When the pointer hovers over a
//! cell that falls inside a detected span, `hovered_link` is set to the URL so
//! the render path underlines it exactly like an OSC 8 link.  Ctrl+Click opens
//! the URL via the same `open_url` path.
//!
//! Cells that already carry an OSC 8 hyperlink are skipped — the OSC 8 path
//! takes priority and is already handled elsewhere.
//!
//! The scanner is hand-rolled (no regex dependency):
//!   1. Collect the row into a `Vec<(col, char)>` span, stopping at cell
//!      boundaries (end-of-row, OSC 8 cell).
//!   2. Slide over the string looking for scheme prefixes or a leading `/`/`~/`.
//!   3. Extend the match forward while the char is a valid URL/path byte
//!      (URL-safe set: alphanumeric, `-`, `_`, `.`, `~`, `%`, `+`, `=`, `?`,
//!      `#`, `@`, `!`, `$`, `&`, `'`, `*`, `,`, `;`, `:`, `[`, `]`, `(`,
//!      `)`, `@` — essentially RFC 3986 minus whitespace and bare quotes).
//!   4. Trim a trailing `)` / `]` / `.` / `,` / `:` that almost certainly
//!      belongs to the surrounding sentence rather than the URL.
//!
//! # Word-separator integration
//!
//! `Config.word_separator` is wired into `alacritty_terminal::term::Config`'s
//! `semantic_escape_chars` at `Pty::spawn` time via `pty::merge_word_separators`,
//! so the configured extra characters act as word boundaries for double-click
//! semantic selection from the very first frame and on every new pane/tab.

use super::*;

// ---------------------------------------------------------------------------
// Plain-text link detection
// ---------------------------------------------------------------------------

/// A detected plain-text link span on a single terminal row.
#[derive(Debug, Clone)]
pub(crate) struct PlainLink {
    /// The resolved URI (`http://…`, `https://…`, or `file://…`).
    pub uri: String,
    /// Start column (inclusive).
    pub col_start: usize,
    /// End column (exclusive).
    pub col_end: usize,
}

impl App {
    /// Scan the visible row `row` for plain-text URLs and bare paths.
    /// Cells that already carry an OSC 8 annotation are skipped.
    /// Returns all detected `PlainLink` spans for that row.
    pub(crate) fn scan_row_for_links(&self, row: usize) -> Vec<PlainLink> {
        let Some(pty) = self.pty.as_ref() else { return Vec::new() };
        if row >= self.rows { return Vec::new(); }

        let display_offset = pty.term.lock().grid().display_offset() as i32;
        let point_line = alacritty_terminal::index::Line(row as i32 - display_offset);

        // Collect (col, char) for the row, skipping cells with OSC 8 links.
        // We stop accumulating when we hit an OSC 8 cell and restart after the
        // link span ends.  Within each OSC8-free segment the chars are joined
        // into a string and scanned for plain-text URLs.
        let term = pty.term.lock();
        let grid = term.grid();
        let cols = self.cols;

        // Build the row string and a col-index map.
        let mut text = String::with_capacity(cols);
        let mut col_map: Vec<usize> = Vec::with_capacity(cols); // text char index → col
        for col in 0..cols {
            let pt = alacritty_terminal::index::Point::new(
                point_line,
                alacritty_terminal::index::Column(col),
            );
            let cell = &grid[pt];
            // Skip OSC 8 cells — they're already handled by the hyperlink path.
            if cell.hyperlink().is_some() {
                // Push a sentinel space to break the text run.
                text.push('\0');
                col_map.push(col);
                continue;
            }
            let ch = if cell.c == '\0' { ' ' } else { cell.c };
            text.push(ch);
            col_map.push(col);
        }
        drop(term);

        scan_plain_links(&text, &col_map)
    }

    /// Return the plain-text link URI at screen cell `(col, row)`, if any.
    /// Only called when `cell_hyperlink(col, row)` returns `None`.
    pub(crate) fn plain_link_at(&self, col: usize, row: usize) -> Option<String> {
        let links = self.scan_row_for_links(row);
        links.into_iter().find(|l| col >= l.col_start && col < l.col_end).map(|l| l.uri)
    }

}

// ---------------------------------------------------------------------------
// Internal scanner (pure function — no `self` needed, easy to unit-test)
// ---------------------------------------------------------------------------

/// Scan a row's text string for plain-text URL/path spans.
///
/// `text` is a char-by-char rendering of the row; `col_map[i]` maps the `i`-th
/// character of `text` back to its terminal column.  Null bytes (`\0`) are
/// sentinel separators that break the scan (used for OSC 8 cells above).
pub(crate) fn scan_plain_links(text: &str, col_map: &[usize]) -> Vec<PlainLink> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let mut links = Vec::new();
    let mut i = 0;

    while i < n {
        // Skip sentinels.
        if chars[i] == '\0' {
            i += 1;
            continue;
        }

        // Try to match a URL scheme or bare path at position i.
        if let Some((scheme_len, uri_kind)) = match_scheme(&chars, i) {
            // Scheme matched; collect the body.
            let start_char = i;
            i += scheme_len;
            let body_start = i;
            while i < n && is_url_char(chars[i]) {
                i += 1;
            }
            // Trim trailing punctuation that's likely sentence syntax.
            let mut end_char = i;
            while end_char > body_start {
                match chars[end_char - 1] {
                    ')' | ']' | '.' | ',' | ':' | ';' | '\'' | '"' => end_char -= 1,
                    _ => break,
                }
            }
            if end_char > body_start {
                let body: String = chars[body_start..end_char].iter().collect();
                let uri = format!("{}{}", uri_kind, body);
                let col_start = col_map[start_char];
                let col_end = col_map[end_char - 1] + 1;
                links.push(PlainLink { uri, col_start, col_end });
            }
            i = end_char;
            continue;
        }

        // Bare path: must start at a word boundary (prev char is not url-char).
        if chars[i] == '/' || (chars[i] == '~' && chars.get(i + 1) == Some(&'/')) {
            let prev_ok = i == 0 || !is_url_char(chars[i - 1]) || chars[i - 1] == '\0';
            if prev_ok {
                let start_char = i;
                while i < n && is_path_char(chars[i]) {
                    i += 1;
                }
                let mut end_char = i;
                while end_char > start_char {
                    match chars[end_char - 1] {
                        ')' | ']' | '.' | ',' | ':' | ';' | '\'' | '"' => end_char -= 1,
                        _ => break,
                    }
                }
                // A bare `/` alone or `~/` alone is not a link.
                let len = end_char - start_char;
                if len >= 2 {
                    let raw_path: String = chars[start_char..end_char].iter().collect();
                    let col_start = col_map[start_char];
                    let col_end = col_map[end_char - 1] + 1;
                    let uri = if raw_path.starts_with('~') {
                        // Expand ~ to file:// form with literal ~; xdg-open handles it.
                        // Actually encode the raw path as a file URI for open_url.
                        if let Ok(home) = std::env::var("HOME") {
                            let expanded = raw_path.replacen('~', &home, 1);
                            format!("file://{}", super::percent_encode_path(&expanded))
                        } else {
                            // Can't expand; still register so Ctrl+Click tries.
                            format!("file://{}", super::percent_encode_path(&raw_path))
                        }
                    } else {
                        format!("file://{}", super::percent_encode_path(&raw_path))
                    };
                    links.push(PlainLink { uri, col_start, col_end });
                }
                i = end_char;
                continue;
            }
        }

        i += 1;
    }

    links
}

/// Try to match a URL scheme at position `i` in `chars`.
/// Returns `(scheme_char_len, uri_prefix)` where `uri_prefix` is what gets
/// prepended to the body.
fn match_scheme(chars: &[char], i: usize) -> Option<(usize, &'static str)> {
    let remaining = &chars[i..];
    // Check for "https://"
    if starts_with_ci(remaining, &['h','t','t','p','s',':','/','/']) {
        return Some((8, "https://"));
    }
    // Check for "http://" (must not already be "https://")
    if starts_with_ci(remaining, &['h','t','t','p',':','/','/']) {
        return Some((7, "http://"));
    }
    // Check for "file://"
    if starts_with_ci(remaining, &['f','i','l','e',':','/','/']) {
        return Some((7, "file://"));
    }
    None
}

fn starts_with_ci(chars: &[char], prefix: &[char]) -> bool {
    if chars.len() < prefix.len() { return false; }
    chars[..prefix.len()].iter().zip(prefix.iter()).all(|(a, b)| a.to_ascii_lowercase() == *b)
}

/// Characters allowed inside a URL body (RFC 3986 unreserved + reserved,
/// minus whitespace and bare `<>`).
fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c,
            '-' | '_' | '.' | '~' | '%' | '+' | '='
            | '?' | '#' | '@' | '!' | '$' | '&'
            | '\'' | '*' | ',' | ';' | ':' | '['
            | ']' | '(' | ')' | '/'
        )
}

/// Characters allowed in a bare path (more restrictive: no `?`, `#`, `@`, etc.)
fn is_path_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '-' | '_' | '.' | '~' | '%' | '+' | '/' | '=' | ':' | '@' | ',')
        || (!c.is_ascii() && c != '\0')
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn col_map_identity(n: usize) -> Vec<usize> {
        (0..n).collect()
    }

    #[test]
    fn detects_https_url() {
        let text = "visit https://example.com/foo?bar=1 now";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "https://example.com/foo?bar=1");
        // start col is at 'h' of https
        assert_eq!(links[0].col_start, 6);
    }

    #[test]
    fn detects_http_url() {
        let text = "http://example.com";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "http://example.com");
    }

    #[test]
    fn trims_trailing_punctuation() {
        let text = "see https://example.com. next";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert!(!links[0].uri.ends_with('.'));
        assert_eq!(links[0].uri, "https://example.com");
    }

    #[test]
    fn detects_bare_path() {
        let text = "edit /etc/hosts and";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert!(links[0].uri.starts_with("file://"));
        assert!(links[0].uri.contains("/etc/hosts"));
    }

    #[test]
    fn skips_osc8_sentinel() {
        // A \0 sentinel breaks the scan; a scheme that straddles it is not found.
        let text = "htt\0ps://example.com";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        // "htt" doesn't match any scheme; "ps://..." doesn't either.
        assert!(links.is_empty());
    }

    #[test]
    fn no_false_positive_on_short_path() {
        let text = "use /";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert!(links.is_empty(), "bare '/' should not be a link");
    }

    #[test]
    fn trims_trailing_paren() {
        let text = "(see https://example.com/page)";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert!(!links[0].uri.ends_with(')'));
    }

    #[test]
    fn multiple_urls_on_row() {
        let text = "http://a.com and https://b.org";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].uri, "http://a.com");
        assert_eq!(links[1].uri, "https://b.org");
    }

    // ---- additional URL/path detection edge cases ---------------------------

    #[test]
    fn file_scheme_url_detected() {
        let text = "file:///tmp/test.log";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "file:///tmp/test.log");
    }

    #[test]
    fn url_with_fragment_detected() {
        let text = "https://docs.rs/crate#section";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert!(links[0].uri.contains('#'));
    }

    #[test]
    fn url_with_query_params_detected() {
        let text = "https://search.engine/?q=foo+bar&lang=en";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert!(links[0].uri.contains('?'));
        assert!(links[0].uri.contains('='));
    }

    #[test]
    fn url_at_start_of_text() {
        let text = "https://example.com is a site";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].col_start, 0);
    }

    #[test]
    fn url_at_end_of_text() {
        let text = "see https://example.com";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].uri, "https://example.com");
        assert_eq!(links[0].col_end, text.chars().count());
    }

    #[test]
    fn trims_trailing_colon() {
        // A trailing colon (common in "see URL:" sentences) must be trimmed.
        let text = "https://example.com:";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        // Port numbers ("https://example.com:8080") would NOT be trimmed because
        // ':' is in is_url_char and the trailing-trim loop removes trailing colons.
        // "https://example.com:" → trim the trailing ':'.
        assert_eq!(links[0].uri, "https://example.com");
    }

    #[test]
    fn trims_trailing_semicolon() {
        let text = "https://example.com;";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links[0].uri, "https://example.com");
    }

    #[test]
    fn trims_trailing_single_quote() {
        let text = "https://example.com'";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links[0].uri, "https://example.com");
    }

    #[test]
    fn trims_trailing_double_quote() {
        let text = "https://example.com\"";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links[0].uri, "https://example.com");
    }

    #[test]
    fn false_positive_rejection_plain_words() {
        // Plain words that don't start with a URL scheme or / must not be detected.
        let text = "just plain text without any links";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert!(links.is_empty(), "plain text must produce no links");
    }

    #[test]
    fn false_positive_rejection_bare_slash_in_path() {
        // A lone '/' is not a link.
        let text = "a/b";
        let col_map = col_map_identity(text.chars().count());
        // 'a' before '/' means it's not a word-boundary start.
        let links = scan_plain_links(text, &col_map);
        assert!(links.is_empty(), "'a/b' not at word boundary: no link");
    }

    #[test]
    fn bare_path_tilde_slash_detected() {
        let text = "edit ~/projects/foo";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        // The URI must be a file:// form.
        assert!(links[0].uri.starts_with("file://"), "tilde path must be file:// URI: {}", links[0].uri);
    }

    #[test]
    fn bare_path_tilde_alone_not_a_link() {
        let text = "~ is home";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        // "~" alone (not "~/") is not a path prefix.
        assert!(links.is_empty(), "'~' alone should not be a link");
    }

    #[test]
    fn col_map_maps_correctly_for_unicode() {
        // When the text contains multibyte chars the col_map must still produce
        // valid column indices (identity in this case, since we pass identical sizes).
        let text = "https://café.example.com";
        let n = text.chars().count();
        let col_map = col_map_identity(n);
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].col_start, 0);
    }

    #[test]
    fn url_body_trims_multiple_trailing_punctuation() {
        // Multiple trailing "),." should all be stripped.
        let text = "https://example.com).";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links[0].uri, "https://example.com");
    }

    #[test]
    fn no_link_in_empty_text() {
        let links = scan_plain_links("", &[]);
        assert!(links.is_empty());
    }

    #[test]
    fn https_case_insensitive_detection() {
        // The scheme match is case-insensitive.
        let text = "HTTPS://example.com/path";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert!(links[0].uri.contains("example.com"));
    }

    #[test]
    fn sentinel_null_breaks_url_scan_mid_scheme() {
        // A null byte inside the scheme must prevent matching.
        let text = "http\0://example.com";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert!(links.is_empty(), "null sentinel must break the scheme match");
    }

    #[test]
    fn path_starting_after_sentinel_is_detected() {
        // A valid path starting immediately after a sentinel must be detected.
        let text = "\0/etc/passwd";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert!(links[0].uri.contains("/etc/passwd"));
    }

    #[test]
    fn col_span_is_correct() {
        // Verify that col_start and col_end point to the right columns.
        let text = "see http://x.io here";
        let col_map = col_map_identity(text.chars().count());
        let links = scan_plain_links(text, &col_map);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].col_start, 4, "URL starts at col 4 ('h' of 'http')");
        // "http://x.io" has 11 chars; col_end = 4 + 11 = 15.
        assert_eq!(links[0].col_end, 15);
    }
}
