//! VT-tap scanner functions for inline-escape sequences.
//!
//! These functions scan VT byte runs for specific escape sequences without
//! consuming or modifying the original bytes—the caller feeds all bytes to the
//! alacritty VT parser unchanged, and we extract metadata for side effects
//! (e.g. modifyOtherKeys level, synchronized output brackets).

use crate::input::ModifyOtherKeys;

/// Scan a VT byte run for `CSI > 4 ; N m` (XTMODKEYS modifyOtherKeys).
///
/// Returns `Some(level)` if such a sequence is found in `bytes`, where `level`
/// is the `N` parameter (0=reset, 1=enable-except-well-defined, 2=enable-all).
/// The caller is responsible for side-effecting application state; the byte run
/// is still passed to the alacritty VT parser unchanged (alacritty ignores the
/// sequence since it does not implement it, but we do here).
pub fn scan_modify_other_keys(bytes: &[u8]) -> Option<ModifyOtherKeys> {
    let mut result = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            i += 1;
            continue;
        }
        // ESC >  (aka DECKPAM-alt / private CSI introducer for xterm private sequences)
        // CSI > is  ESC [ > ...  — two-byte CSI then '>'
        if bytes.get(i + 1) == Some(&b'[') && bytes.get(i + 2) == Some(&b'>') {
            // Scan the parameter list and final byte.
            let mut j = i + 3;
            let mut params = Vec::new();
            let mut cur: Option<u16> = None;
            while j < bytes.len() {
                let b = bytes[j];
                if b.is_ascii_digit() {
                    cur = Some(cur.unwrap_or(0) * 10 + (b - b'0') as u16);
                    j += 1;
                } else if b == b';' {
                    params.push(cur.unwrap_or(0));
                    cur = None;
                    j += 1;
                } else {
                    params.push(cur.unwrap_or(0));
                    j += 1;
                    // final byte
                    if b == b'm' && params.len() >= 2 && params[0] == 4 {
                        // The LAST matching sequence in the buffer wins: an app may
                        // set then reset the level within a single read; the final
                        // state is what must be applied.
                        result = Some(match params[1] {
                            0 => ModifyOtherKeys::Reset,
                            1 => ModifyOtherKeys::EnableExceptWellDefined,
                            2 => ModifyOtherKeys::EnableAll,
                            _ => ModifyOtherKeys::Reset,
                        });
                    }
                    break;
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    result
}

/// Scan a VT byte run for DECSET/DECRST 2026 (synchronized output).
/// Returns `(begin_count, end_count)` of `?2026h` / `?2026l` sequences found.
pub fn scan_sync_2026(bytes: &[u8]) -> (u32, u32) {
    let mut begin = 0u32;
    let mut end = 0u32;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            i += 1;
            continue;
        }
        if bytes.get(i + 1) == Some(&b'[') && bytes.get(i + 2) == Some(&b'?') {
            // CSI ? ... h/l — scan param
            let mut j = i + 3;
            let mut num: u32 = 0;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                num = num * 10 + (bytes[j] - b'0') as u32;
                j += 1;
            }
            // Skip any trailing sub-params (`?2026;1h`) so the final byte check
            // below lands on h/l rather than a semicolon.
            while j < bytes.len() && (bytes[j] == b';' || bytes[j].is_ascii_digit()) {
                j += 1;
            }
            if num == 2026 {
                match bytes.get(j) {
                    Some(&b'h') => begin += 1,
                    Some(&b'l') => end += 1,
                    _ => {}
                }
            }
            i = if j < bytes.len() { j + 1 } else { j };
            continue;
        }
        i += 1;
    }
    (begin, end)
}

/// Whether a VT byte run contains a full-screen erase (`CSI 2J` or `CSI 3J`) or a
/// terminal reset (`ESC c`, RIS) — the signals that the screen content (and thus
/// any inline images anchored to it) is being wiped, e.g. by `clear`/`reset`.
pub fn clears_screen(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            match bytes.get(i + 1) {
                Some(b'c') => return true, // RIS
                Some(b'[') => {
                    // CSI ... J — scan the (numeric) parameter up to the final 'J'.
                    // Handle variants like CSI 2J, CSI ;2J, or CSI 0;2J.
                    let mut j = i + 2;
                    let mut params = String::new();
                    while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                        params.push(bytes[j] as char);
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b'J') {
                        // Parse parameters numerically: empty or 0 = display,
                        // 2 = all lines, 3 = scrollback+display. Check for 2 or 3.
                        let has_erase_all = params.is_empty()
                            || params.split(';').any(|p| {
                                p.parse::<u32>().map(|v| v == 2 || v == 3).unwrap_or(false)
                            });
                        if has_erase_all {
                            return true;
                        }
                    }
                    i = j;
                    continue;
                }
                _ => {}
            }
        }
        i += 1;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{scan_modify_other_keys, scan_sync_2026, ModifyOtherKeys};

    // ---- scan_modify_other_keys tests ----------------------------------------

    #[test]
    fn scan_mok_level2() {
        // CSI > 4 ; 2 m  (enable-all)
        let seq = b"\x1b[>4;2m";
        assert_eq!(scan_modify_other_keys(seq), Some(ModifyOtherKeys::EnableAll));
    }

    #[test]
    fn scan_mok_level1() {
        let seq = b"\x1b[>4;1m";
        assert_eq!(
            scan_modify_other_keys(seq),
            Some(ModifyOtherKeys::EnableExceptWellDefined)
        );
    }

    #[test]
    fn scan_mok_reset() {
        let seq = b"\x1b[>4;0m";
        assert_eq!(scan_modify_other_keys(seq), Some(ModifyOtherKeys::Reset));
    }

    #[test]
    fn scan_mok_not_found_in_normal_text() {
        assert_eq!(scan_modify_other_keys(b"hello world"), None);
    }

    #[test]
    fn scan_mok_embedded_in_longer_run() {
        // Normal output before + after the CSI > 4 ; 2 m sequence.
        let seq = b"abc\x1b[>4;2mdef";
        assert_eq!(scan_modify_other_keys(seq), Some(ModifyOtherKeys::EnableAll));
    }

    #[test]
    fn scan_mok_different_param_not_4_ignored() {
        // CSI > 5 ; 2 m — different resource (not modifyOtherKeys)
        let seq = b"\x1b[>5;2m";
        assert_eq!(scan_modify_other_keys(seq), None);
    }

    // ---- scan_sync_2026 tests ------------------------------------------------

    #[test]
    fn scan_sync_begin_only() {
        let seq = b"\x1b[?2026h";
        let (begin, end) = scan_sync_2026(seq);
        assert_eq!(begin, 1);
        assert_eq!(end, 0);
    }

    #[test]
    fn scan_sync_end_only() {
        let seq = b"\x1b[?2026l";
        let (begin, end) = scan_sync_2026(seq);
        assert_eq!(begin, 0);
        assert_eq!(end, 1);
    }

    #[test]
    fn scan_sync_begin_and_end_pair() {
        // A complete synchronized update bracket in one buffer.
        let seq = b"\x1b[?2026h...content...\x1b[?2026l";
        let (begin, end) = scan_sync_2026(seq);
        assert_eq!(begin, 1);
        assert_eq!(end, 1);
    }

    #[test]
    fn scan_sync_no_match_in_normal_text() {
        let (begin, end) = scan_sync_2026(b"hello\r\n");
        assert_eq!((begin, end), (0, 0));
    }

    #[test]
    fn scan_sync_other_private_mode_ignored() {
        // DECSET 1049 (alt screen) must not count.
        let (begin, end) = scan_sync_2026(b"\x1b[?1049h\x1b[?1049l");
        assert_eq!((begin, end), (0, 0));
    }
}
