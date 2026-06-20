//! Keyboard input encoding: winit `KeyEvent` -> PTY byte sequences.
//!
//! We rely on `KeyEvent.text` for locale-correct printable input and only
//! hand-roll escape sequences for named keys (arrows, function keys, etc.) and
//! modifier combinations (Ctrl-letter control bytes, Alt = ESC prefix).

use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Encode a key press into the bytes a terminal application expects.
///
/// Returns `None` for keys that produce no input (pure modifiers, unhandled
/// named keys, key releases).
pub fn encode_key(event: &KeyEvent, mods: ModifiersState) -> Option<Vec<u8>> {
    // Terminals act on press (and OS autorepeat), never release.
    if !event.state.is_pressed() {
        return None;
    }

    let ctrl = mods.control_key();
    let alt = mods.alt_key();
    let shift = mods.shift_key();

    // a) Named keys -> fixed control / CSI sequences.
    // Match on a reference: `Key<SmolStr>` isn't `Copy`, but `NamedKey` is.
    if let Key::Named(named) = &event.logical_key {
        let named = *named;
        let seq: &[u8] = match named {
            NamedKey::Enter => b"\r",
            NamedKey::Backspace => b"\x7f", // DEL — what real terminals send
            // Shift+Tab is back-tab (CBT, ESC [ Z); plain Tab is HT. Used by TUIs
            // and shell completion menus for reverse field/candidate navigation.
            NamedKey::Tab => {
                if shift {
                    b"\x1b[Z".as_slice()
                } else {
                    b"\t".as_slice()
                }
            }
            NamedKey::Escape => b"\x1b",
            NamedKey::ArrowUp => b"\x1b[A",
            NamedKey::ArrowDown => b"\x1b[B",
            NamedKey::ArrowRight => b"\x1b[C",
            NamedKey::ArrowLeft => b"\x1b[D",
            NamedKey::Home => b"\x1b[H",
            NamedKey::End => b"\x1b[F",
            NamedKey::PageUp => b"\x1b[5~",
            NamedKey::PageDown => b"\x1b[6~",
            NamedKey::Delete => b"\x1b[3~",
            NamedKey::Insert => b"\x1b[2~",
            NamedKey::F1 => b"\x1bOP",
            NamedKey::F2 => b"\x1bOQ",
            NamedKey::F3 => b"\x1bOR",
            NamedKey::F4 => b"\x1bOS",
            NamedKey::F5 => b"\x1b[15~",
            NamedKey::F6 => b"\x1b[17~",
            NamedKey::F7 => b"\x1b[18~",
            NamedKey::F8 => b"\x1b[19~",
            NamedKey::F9 => b"\x1b[20~",
            NamedKey::F10 => b"\x1b[21~",
            NamedKey::F11 => b"\x1b[23~",
            NamedKey::F12 => b"\x1b[24~",
            NamedKey::Space => b" ",
            _ => b"",
        };
        if seq.is_empty() {
            return None;
        }
        let mut out = Vec::with_capacity(seq.len() + 1);
        if alt {
            out.push(0x1b); // Alt = ESC prefix
        }
        out.extend_from_slice(seq);
        return Some(out);
    }

    // b) Printable input. winit gives the locale-correct text directly.
    if let Some(text) = &event.text {
        if text.is_empty() {
            return None;
        }

        // Ctrl-<key> -> control byte (Ctrl-A = 0x01 .. Ctrl-Z = 0x1a, and the
        // C0 controls for @ [ \ ] ^ _ and space).
        if ctrl
            && let Some(c) = text.chars().next()
            && let Some(byte) = control_byte(c)
        {
            let mut out = Vec::with_capacity(2);
            if alt {
                out.push(0x1b);
            }
            out.push(byte);
            return Some(out);
        }

        let mut out = Vec::with_capacity(text.len() + 1);
        if alt {
            out.push(0x1b);
        }
        out.extend_from_slice(text.as_bytes());
        return Some(out);
    }

    None
}

/// A mouse event to report to the terminal application.
pub struct MouseReport {
    /// Base button id: 0=left, 1=middle, 2=right, 64=wheel-up, 65=wheel-down.
    pub button: u8,
    /// 0-based grid column and row.
    pub col: usize,
    pub row: usize,
    /// True for press / wheel; false for release.
    pub pressed: bool,
    /// True when this is a motion (drag) event.
    pub motion: bool,
}

/// Encode a mouse event as a terminal report. Uses SGR (1006) form when `sgr`
/// is set (no coordinate limit), otherwise the legacy X10 (1000) form.
pub fn encode_mouse(report: MouseReport, mods: ModifiersState, sgr: bool) -> Vec<u8> {
    let mut cb = report.button as u32;
    if report.motion {
        cb += 32;
    }
    if mods.shift_key() {
        cb += 4;
    }
    if mods.alt_key() {
        cb += 8;
    }
    if mods.control_key() {
        cb += 16;
    }

    if sgr {
        let kind = if report.pressed { 'M' } else { 'm' };
        return format!("\x1b[<{};{};{}{}", cb, report.col + 1, report.row + 1, kind).into_bytes();
    }

    // Legacy X10: ESC [ M  Cb  Cx  Cy, each offset by 32. Release reports the
    // low two button bits as 0b11. Coordinates are clamped to a single byte.
    let cb_legacy = if report.pressed || report.button >= 64 {
        cb
    } else {
        (cb & !0b11) | 0b11
    };
    let enc = |v: usize| -> u8 { (32 + (v + 1).min(223)) as u8 };
    vec![
        0x1b,
        b'[',
        b'M',
        (32 + cb_legacy).min(255) as u8,
        enc(report.col),
        enc(report.row),
    ]
}

/// Map a character to its C0 control byte for Ctrl-<char>, if one exists.
fn control_byte(c: char) -> Option<u8> {
    let upper = c.to_ascii_uppercase();
    match upper {
        '@'..='_' => Some((upper as u8) & 0x1f), // @ A..Z [ \ ] ^ _  ->  0x00..0x1f
        ' ' => Some(0x00),                       // Ctrl-Space -> NUL
        '?' => Some(0x7f),                       // Ctrl-? -> DEL
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{MouseReport, encode_mouse};
    use winit::keyboard::ModifiersState;

    fn rep(button: u8, col: usize, row: usize, pressed: bool, motion: bool) -> MouseReport {
        MouseReport {
            button,
            col,
            row,
            pressed,
            motion,
        }
    }

    #[test]
    fn sgr_left_press_and_release() {
        let m = ModifiersState::empty();
        assert_eq!(
            encode_mouse(rep(0, 0, 0, true, false), m, true),
            b"\x1b[<0;1;1M"
        );
        assert_eq!(
            encode_mouse(rep(0, 4, 2, false, false), m, true),
            b"\x1b[<0;5;3m"
        );
    }

    #[test]
    fn sgr_wheel_up_down() {
        let m = ModifiersState::empty();
        assert_eq!(
            encode_mouse(rep(64, 0, 0, true, false), m, true),
            b"\x1b[<64;1;1M"
        );
        assert_eq!(
            encode_mouse(rep(65, 9, 9, true, false), m, true),
            b"\x1b[<65;10;10M"
        );
    }

    #[test]
    fn sgr_bare_motion_uses_no_button_code() {
        // button 3 ("no button") + 32 (motion) = 35; drives hover reporting.
        let m = ModifiersState::empty();
        assert_eq!(
            encode_mouse(rep(3, 0, 0, true, true), m, true),
            b"\x1b[<35;1;1M"
        );
    }

    #[test]
    fn sgr_ctrl_modifier_sets_bit() {
        // Ctrl adds 16 to the button code.
        let m = ModifiersState::CONTROL;
        assert_eq!(
            encode_mouse(rep(0, 0, 0, true, false), m, true),
            b"\x1b[<16;1;1M"
        );
    }

    #[test]
    fn legacy_x10_form() {
        let m = ModifiersState::empty();
        // ESC [ M  Cb(32)  Cx(32+1)  Cy(32+1)
        assert_eq!(
            encode_mouse(rep(0, 0, 0, true, false), m, false),
            vec![0x1b, b'[', b'M', 32, 33, 33]
        );
    }
}
