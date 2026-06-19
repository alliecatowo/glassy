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

    // a) Named keys -> fixed control / CSI sequences.
    // Match on a reference: `Key<SmolStr>` isn't `Copy`, but `NamedKey` is.
    if let Key::Named(named) = &event.logical_key {
        let named = *named;
        let seq: &[u8] = match named {
            NamedKey::Enter => b"\r",
            NamedKey::Backspace => b"\x7f", // DEL — what real terminals send
            NamedKey::Tab => b"\t",
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
        if ctrl {
            if let Some(c) = text.chars().next() {
                if let Some(byte) = control_byte(c) {
                    let mut out = Vec::with_capacity(2);
                    if alt {
                        out.push(0x1b);
                    }
                    out.push(byte);
                    return Some(out);
                }
            }
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

/// Map a character to its C0 control byte for Ctrl-<char>, if one exists.
fn control_byte(c: char) -> Option<u8> {
    let upper = c.to_ascii_uppercase();
    match upper {
        '@'..='_' => Some((upper as u8) & 0x1f), // @ A..Z [ \ ] ^ _  ->  0x00..0x1f
        ' ' => Some(0x00),                        // Ctrl-Space -> NUL
        '?' => Some(0x7f),                        // Ctrl-? -> DEL
        _ => None,
    }
}
