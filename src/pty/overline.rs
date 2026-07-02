//! SGR 53/55 overline tracking.
//!
//! `alacritty_terminal`'s `Flags` bitfield has no spare bit for overline, and the
//! underlying `vte` SGR parser silently drops SGR 53 (overline on) and SGR 55
//! (overline off). So glassy maintains overline coverage in a side table here,
//! the same way it taps the byte stream for modifyOtherKeys / sync-output / blink.
//!
//! The hard part is knowing *which* cells an overlined run printed to without
//! re-implementing the terminal. We sidestep that by letting `alacritty_terminal`
//! remain the source of truth for the cursor: [`OverlineTracker::advance`] splits
//! a VT byte run into segments at SGR boundaries, feeds each segment to the real
//! parser, and — for any segment fed while overline is active — marks every grid
//! cell the cursor swept over (read from the term before and after the segment).
//! Wrapping, tabs, and cursor motion are therefore handled by alacritty itself.
//!
//! Coverage is keyed by *absolute* grid row (`cursor.line.0 + display_offset`),
//! exactly the coordinate the command-block tracker uses, so the render path can
//! look it up with the absolute `row` it already computes and translate to a
//! viewport row via `row - display_offset`. Like command blocks, this drifts if
//! content scrolls out of the viewport while pinned to the bottom; that is an
//! accepted limitation shared with the existing absolute-row features.

use alacritty_terminal::Term;
use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::vte::ansi::Processor;
use std::collections::HashSet;

/// Maximum number of overlined cells retained. Overline is rare; this cap keeps a
/// pathological `printf '\e[53m'`-spammer from growing the set without bound. When
/// exceeded the whole set is cleared (cheap, and overline simply re-accrues).
const MAX_OVERLINE_CELLS: usize = 1 << 16;

/// Tracks SGR-53 overline coverage across a PTY byte stream.
#[derive(Default)]
pub struct OverlineTracker {
    /// Whether overline is currently active (last seen SGR 53 with no later 55/0).
    active: bool,
    /// Absolute `(row, col)` of every cell printed while overline was active.
    cells: HashSet<(i32, usize)>,
}

/// One slice of a VT byte run, split at SGR sequences that change overline state.
enum Seg<'a> {
    /// Ordinary bytes to feed to the parser; may move/print the cursor.
    Bytes(&'a [u8]),
    /// An SGR sequence that toggles overline: `true` = on (53), `false` = off
    /// (55 or a reset 0). The bytes are *also* in an adjacent `Bytes` segment so
    /// the parser still sees the full original stream — this only records intent.
    Toggle(bool),
}

impl OverlineTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether any cell is overlined (lets the render path skip the per-cell lookup
    /// entirely when nothing is overlined — the overwhelmingly common case).
    pub fn any(&self) -> bool {
        !self.cells.is_empty()
    }

    /// Clone the overline coverage set for a frame snapshot, so the render path can
    /// release the lock before its (long) cell loop.
    pub fn snapshot(&self) -> HashSet<(i32, usize)> {
        self.cells.clone()
    }

    /// Clear all overline coverage. Called on a full-screen erase / reset, which
    /// wipes the cells overline sat on (mirrors the image/blink reset paths).
    pub fn clear(&mut self) {
        self.cells.clear();
        // A reset (RIS) also resets SGR; CSI 2J does not, but clearing `active`
        // here is the safe, conservative choice and matches a fresh screen.
    }

    /// Feed `bytes` to the real VT `processor`/`term`, recording overline coverage
    /// for any printable run made while overline (SGR 53) is active. This REPLACES
    /// the loop's direct `processor.advance(term, bytes)` call for overline-aware
    /// sessions: every original byte still reaches the parser, in order.
    pub fn advance<T: EventListener>(
        &mut self,
        term: &mut Term<T>,
        processor: &mut Processor,
        bytes: &[u8],
    ) {
        // Fast path: if overline has never been seen in this run AND is not active,
        // a quick scan avoids the per-segment cursor reads for the common stream.
        if !self.active && !run_has_overline_sgr(bytes) {
            processor.advance(term, bytes);
            return;
        }
        for seg in split_overline_sgr(bytes) {
            match seg {
                Seg::Toggle(on) => self.active = on,
                Seg::Bytes(slice) => {
                    if slice.is_empty() {
                        continue;
                    }
                    if !self.active {
                        processor.advance(term, slice);
                        continue;
                    }
                    // Overline active: bracket the print with cursor reads so we
                    // can mark exactly the cells alacritty wrote into.
                    let (before, off_before) = cursor_abs(term);
                    processor.advance(term, slice);
                    let (after, off_after) = cursor_abs(term);
                    self.mark_swept(before, off_before, after, off_after, term);
                }
            }
        }
        if self.cells.len() > MAX_OVERLINE_CELLS {
            self.cells.clear();
        }
    }

    /// Mark every cell the cursor swept from `before` to `after` (inclusive of the
    /// start cell, exclusive of the final cursor cell, which has not been written
    /// yet). Both points are absolute `(row, col)`. Handles the single-row case and
    /// multi-row wraps by filling intermediate rows edge to edge.
    fn mark_swept<T: EventListener>(
        &mut self,
        before: (i32, usize),
        _off_before: i32,
        after: (i32, usize),
        _off_after: i32,
        term: &mut Term<T>,
    ) {
        let cols = term.grid().columns().max(1);
        let (r0, c0) = before;
        let (r1, c1) = after;
        // No forward progress (e.g. a bare cursor move or a control char that did
        // not print): nothing to mark.
        if (r1, c1) <= (r0, c0) {
            return;
        }
        if r0 == r1 {
            for c in c0..c1.min(cols) {
                self.cells.insert((r0, c));
            }
            return;
        }
        // First row: from c0 to end of line.
        for c in c0..cols {
            self.cells.insert((r0, c));
        }
        // Whole intermediate rows.
        for r in (r0 + 1)..r1 {
            for c in 0..cols {
                self.cells.insert((r, c));
            }
        }
        // Last row: from start to c1.
        for c in 0..c1.min(cols) {
            self.cells.insert((r1, c));
        }
    }
}

/// Read the cursor as an absolute `(row, col)` plus the live display offset.
fn cursor_abs<T: EventListener>(term: &Term<T>) -> ((i32, usize), i32) {
    let off = term.grid().display_offset() as i32;
    let p = term.grid().cursor.point;
    ((p.line.0 + off, p.column.0), off)
}

/// Quick positive check: does this run contain any SGR sequence that could change
/// overline state (a `53`, `55`, or reset `0`/empty SGR)? A cheap pre-filter so
/// the segment-splitting cursor-bracketing only runs when overline is in play.
fn run_has_overline_sgr(bytes: &[u8]) -> bool {
    for seg in split_overline_sgr(bytes) {
        if matches!(seg, Seg::Toggle(_)) {
            return true;
        }
    }
    false
}

/// Split `bytes` into `Bytes`/`Toggle` segments. Each `CSI ... m` (SGR) whose
/// parameter list contains `53` (overline on), `55` (overline off), or a reset
/// (`0`, or an empty SGR `CSI m`) emits a `Toggle`; the SGR bytes themselves are
/// still included in the surrounding `Bytes` segments so the parser sees them.
fn split_overline_sgr(bytes: &[u8]) -> Vec<Seg<'_>> {
    let mut segs: Vec<Seg> = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        // Look for CSI = ESC [
        if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'[') {
            let mut j = i + 2;
            // Skip private-mode prefixes (we only care about plain SGR, but be
            // tolerant of leading intermediate bytes).
            while j < bytes.len() && matches!(bytes[j], b'<' | b'>' | b'=' | b'?') {
                j += 1;
            }
            let params_start = j;
            while j < bytes.len()
                && (bytes[j].is_ascii_digit() || bytes[j] == b';' || bytes[j] == b':')
            {
                j += 1;
            }
            if bytes.get(j) == Some(&b'm') {
                // This is an SGR. Inspect its params for an overline toggle.
                if let Some(on) = overline_toggle(&bytes[params_start..j]) {
                    // Emit the preceding bytes (including this SGR so the parser
                    // sees it) as one chunk, then the toggle. We deliberately let
                    // the SGR bytes ride along in the leading `Bytes` slice.
                    let chunk_end = j + 1; // include the trailing 'm'
                    if chunk_end > start {
                        segs.push(Seg::Bytes(&bytes[start..chunk_end]));
                    }
                    segs.push(Seg::Toggle(on));
                    start = chunk_end;
                }
                i = j + 1;
                continue;
            }
            i = j.max(i + 1);
            continue;
        }
        i += 1;
    }
    if start < bytes.len() {
        segs.push(Seg::Bytes(&bytes[start..]));
    }
    if segs.is_empty() {
        segs.push(Seg::Bytes(bytes));
    }
    segs
}

/// Inspect an SGR parameter byte slice (between `CSI` and the final `m`) for an
/// overline state change. Returns `Some(true)` for 53, `Some(false)` for 55 or a
/// reset (0 / empty), or `None` if this SGR is unrelated to overline.
///
/// The last relevant token in the list wins (matching how a terminal applies a
/// compound SGR left to right), so `CSI 0;53 m` enables overline and
/// `CSI 53;55 m` leaves it off.
fn overline_toggle(params: &[u8]) -> Option<bool> {
    let s = std::str::from_utf8(params).ok()?;
    let mut result: Option<bool> = None;
    // Treat an empty SGR (`CSI m`) as a reset.
    if s.is_empty() {
        return Some(false);
    }
    for tok in s.split(';') {
        // A colon-subparameter group (e.g. `4:3`) — take the leading number.
        let head = tok.split(':').next().unwrap_or("");
        match head {
            "" | "0" => result = Some(false), // reset clears overline
            "53" => result = Some(true),
            "55" => result = Some(false),
            _ => {}
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_detects_53_55() {
        assert_eq!(overline_toggle(b"53"), Some(true));
        assert_eq!(overline_toggle(b"55"), Some(false));
    }

    #[test]
    fn toggle_reset_clears() {
        assert_eq!(overline_toggle(b"0"), Some(false));
        assert_eq!(overline_toggle(b""), Some(false));
    }

    #[test]
    fn toggle_unrelated_sgr_is_none() {
        assert_eq!(overline_toggle(b"1"), None); // bold
        assert_eq!(overline_toggle(b"31"), None); // red fg
        assert_eq!(overline_toggle(b"4"), None); // underline
    }

    #[test]
    fn toggle_compound_last_relevant_wins() {
        // bold + overline-on
        assert_eq!(overline_toggle(b"1;53"), Some(true));
        // overline-on then overline-off
        assert_eq!(overline_toggle(b"53;55"), Some(false));
        // reset then overline-on
        assert_eq!(overline_toggle(b"0;53"), Some(true));
    }

    #[test]
    fn toggle_colon_subparams_tolerated() {
        // underline curly + overline on: the 4:3 token is ignored, 53 wins.
        assert_eq!(overline_toggle(b"4:3;53"), Some(true));
    }

    #[test]
    fn split_round_trips_bytes() {
        // The concatenation of all Bytes segments must equal the original input,
        // so the parser still receives every byte in order.
        let input = b"abc\x1b[53mDEF\x1b[55mghi".as_slice();
        let mut reassembled = Vec::new();
        let mut toggles = Vec::new();
        for seg in split_overline_sgr(input) {
            match seg {
                Seg::Bytes(b) => reassembled.extend_from_slice(b),
                Seg::Toggle(on) => toggles.push(on),
            }
        }
        assert_eq!(reassembled, input);
        assert_eq!(toggles, vec![true, false]);
    }

    #[test]
    fn split_no_sgr_is_single_chunk() {
        let input = b"plain text".as_slice();
        let segs = split_overline_sgr(input);
        assert_eq!(segs.len(), 1);
        assert!(matches!(segs[0], Seg::Bytes(b) if b == input));
        assert!(!run_has_overline_sgr(input));
    }

    #[test]
    fn run_has_overline_detects() {
        assert!(run_has_overline_sgr(b"x\x1b[53my"));
        assert!(run_has_overline_sgr(b"x\x1b[0my")); // reset is overline-relevant
        assert!(!run_has_overline_sgr(b"x\x1b[1my")); // bold only
    }
}
