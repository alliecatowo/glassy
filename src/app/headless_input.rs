//! Escape decoding for the `GLASSY_INPUT` headless capture hook.
//!
//! The hook lets an automated capture write raw bytes to the PTY before the
//! capture render. To exercise VT/display features that are driven by escape
//! sequences (SGR 53 overline, OSC 1337 inline images, DECSET 1016 SGR-Pixel
//! mouse), the input string supports a small set of C-style escapes so those
//! control bytes can be expressed in an env var.
//!
//! Supported escapes: `\n` (LF), `\r` (CR), `\t` (TAB), `\e` / `\E` (ESC, 0x1b),
//! `\a` (BEL, 0x07), `\\` (literal backslash), and `\xNN` (a raw hex byte). An
//! unrecognised `\X` is passed through verbatim (backslash + the char) so stray
//! backslashes do not silently vanish.

/// Decode the `GLASSY_INPUT` escape grammar into raw bytes for the PTY.
pub(crate) fn decode_input_escapes(input: &str) -> Vec<u8> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        // A trailing lone backslash: emit it literally.
        let Some(&next) = bytes.get(i + 1) else {
            out.push(b'\\');
            break;
        };
        match next {
            b'n' => {
                out.push(b'\n');
                i += 2;
            }
            b'r' => {
                out.push(b'\r');
                i += 2;
            }
            b't' => {
                out.push(b'\t');
                i += 2;
            }
            b'a' => {
                out.push(0x07);
                i += 2;
            }
            b'e' | b'E' => {
                out.push(0x1b);
                i += 2;
            }
            b'\\' => {
                out.push(b'\\');
                i += 2;
            }
            b'x' | b'X' => {
                // \xNN — two hex digits. Fall back to a literal pass-through if the
                // two following bytes are not both hex.
                let hi = bytes.get(i + 2).and_then(|b| hex_val(*b));
                let lo = bytes.get(i + 3).and_then(|b| hex_val(*b));
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    out.push(hi * 16 + lo);
                    i += 4;
                } else {
                    out.push(b'\\');
                    out.push(next);
                    i += 2;
                }
            }
            other => {
                // Unknown escape: keep the backslash and the char verbatim.
                out.push(b'\\');
                out.push(other);
                i += 2;
            }
        }
    }
    out
}

/// Hex digit value for an ASCII byte, or `None` if it is not a hex digit.
fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::decode_input_escapes;

    #[test]
    fn plain_text_unchanged() {
        assert_eq!(decode_input_escapes("hello"), b"hello");
    }

    #[test]
    fn common_escapes() {
        assert_eq!(decode_input_escapes("a\\nb\\tc\\r"), b"a\nb\tc\r");
    }

    #[test]
    fn esc_and_bel() {
        assert_eq!(decode_input_escapes("\\e[53m"), b"\x1b[53m");
        assert_eq!(decode_input_escapes("\\E[55m"), b"\x1b[55m");
        assert_eq!(decode_input_escapes("x\\a"), b"x\x07");
    }

    #[test]
    fn hex_byte() {
        assert_eq!(decode_input_escapes("\\x1b[53m"), b"\x1b[53m");
        assert_eq!(decode_input_escapes("\\x07"), &[0x07]);
    }

    #[test]
    fn literal_backslash_and_unknown_escape() {
        assert_eq!(decode_input_escapes("a\\\\b"), b"a\\b");
        // Unknown escape passes through with the backslash intact.
        assert_eq!(decode_input_escapes("\\q"), b"\\q");
        // Bad hex falls back to literal.
        assert_eq!(decode_input_escapes("\\xZZ"), b"\\xZZ");
    }

    #[test]
    fn trailing_backslash_preserved() {
        assert_eq!(decode_input_escapes("end\\"), b"end\\");
    }

    #[test]
    fn osc1337_sequence_roundtrips() {
        // A representative OSC 1337 File= header expressed via escapes decodes to
        // the real control bytes (ESC ] ... BEL), so a capture can drive it.
        let decoded = decode_input_escapes("\\e]1337;File=inline=1:AAAA\\a");
        assert_eq!(decoded, b"\x1b]1337;File=inline=1:AAAA\x07");
    }
}
