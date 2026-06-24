//! Keyboard input encoding: winit `KeyEvent` -> PTY byte sequences.
//!
//! We rely on `KeyEvent.text` for locale-correct printable input and only
//! hand-roll escape sequences for named keys (arrows, function keys, etc.) and
//! modifier combinations (Ctrl-letter control bytes, Alt = ESC prefix).

use std::fmt::Write as _;

use winit::event::KeyEvent;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Active kitty keyboard protocol flags (bitfield of TermMode bits 18-22).
///
/// Level 1 = DISAMBIGUATE_ESC_CODES only: modified named keys go as CSI-u.
/// Level 2 adds REPORT_EVENT_TYPES: release + repeat events are forwarded.
/// Level 3 adds REPORT_ALTERNATE_KEYS: base/shifted codepoints appended.
/// Level 4 adds REPORT_ALL_KEYS_AS_ESC: every key (even unmodified) as CSI-u.
/// Level 5 adds REPORT_ASSOCIATED_TEXT: associated text appended as last param.
///
/// We do not import TermMode here to keep input.rs free of alacritty deps;
/// the caller converts TermMode → KittyFlags.
#[derive(Clone, Copy, Default)]
pub struct KittyFlags {
    pub disambiguate: bool,
    pub report_event_types: bool,
    pub report_alternate_keys: bool,
    pub report_all_keys_as_esc: bool,
    pub report_associated_text: bool,
}

impl KittyFlags {
    /// Any kitty protocol bit is active (level >= 1).
    pub fn active(self) -> bool {
        self.disambiguate
    }
}

/// The xterm modifyOtherKeys level (XTMODKEYS, set via `CSI > 4 ; N m`).
///
/// Level 0 (Reset): no special encoding.
/// Level 1 (EnableExceptWellDefined): modified printable keys that are not
///   already covered by traditional Ctrl/Alt encodings get `CSI 27 ; mods ; code ~`.
/// Level 2 (EnableAll): all modified printable keys (and some named keys) use
///   that form.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum ModifyOtherKeys {
    #[default]
    Reset = 0,
    EnableExceptWellDefined = 1,
    EnableAll = 2,
}

/// Encode a key press (or release / repeat when `report_event_types` is set)
/// into the bytes a terminal application expects.
///
/// `app_cursor` reflects DECCKM (terminal application cursor-key mode): when set
/// — and the kitty protocol is not active — arrows/Home/End go out in SS3 form
/// (`ESC O X`) instead of CSI (`ESC [ X`), which is what full-screen apps (vim,
/// less, readline vi-mode, ncurses) expect.
///
/// `kitty` carries all five kitty keyboard protocol flags. When `.active()` is
/// true (DISAMBIGUATE_ESC_CODES set), we encode in kitty CSI-u form as needed.
///
/// `modify_other_keys` is the xterm modifyOtherKeys level. When != Reset,
/// modified printable keys (Ctrl/Alt + letter etc.) that kitty does NOT handle
/// are emitted as `CSI 27 ; mods ; code ~` instead of the traditional encoding.
///
/// Returns `None` for keys that produce no input (pure modifiers, unhandled
/// named keys, key releases when report_event_types is off).
pub fn encode_key(
    event: &KeyEvent,
    mods: ModifiersState,
    kitty: KittyFlags,
    app_cursor: bool,
    modify_other_keys: ModifyOtherKeys,
) -> Option<Vec<u8>> {
    let pressed = event.state.is_pressed();
    let repeat  = event.repeat;

    // Without REPORT_EVENT_TYPES, only forward key-press (including OS repeat).
    if !pressed && !kitty.report_event_types {
        return None;
    }

    let ctrl  = mods.control_key();
    let alt   = mods.alt_key();
    let shift = mods.shift_key();
    let super_ = mods.super_key();

    // Kitty modifier encoding: 1 + shift + alt*2 + ctrl*4 + super*8.
    // 1 is the base (unmodified = 1, shifted = 2, alt = 3, etc.).
    let kitty_mods = 1u8
        + (shift as u8)
        + (alt as u8) * 2
        + (ctrl as u8) * 4
        + (super_ as u8) * 8;

    // Kitty event type parameter: 1=press, 2=repeat, 3=release.
    let event_type: u8 = if !pressed { 3 } else if repeat { 2 } else { 1 };

    // Build the kitty CSI-u sequence, optionally with event-type, alternate-keys,
    // and associated-text sub-parameters.
    //
    // Full kitty form: CSI <code> ; <mods> : <event_type> ; <shifted> : <base> ; <text> u
    // (sub-params after ';' are optional trailing params; absent trailing params
    //  with ':' are included only when a higher sub-param is present.)
    let kitty_csi_u = |code: u32, text: Option<&str>| -> Vec<u8> {
        // Mods sub-param (always present when any kitty bit is set)
        let mut s = format!("\x1b[{code}");

        // Second parameter group: mods[:event_type]
        if kitty.report_event_types && event_type != 1 {
            // Include event_type only when it carries information (not plain press=1)
            let _ = write!(s, ";{}:{}", kitty_mods, event_type);
        } else if kitty_mods != 1 || kitty.report_all_keys_as_esc {
            let _ = write!(s, ";{}", kitty_mods);
        } else {
            // omit mods param when unmodified and not forcing all-keys form
            s.push(';');
        }

        // Third parameter group: shifted_key[:base_key] (REPORT_ALTERNATE_KEYS)
        let need_text = kitty.report_associated_text && text.is_some();
        if kitty.report_alternate_keys {
            // We don't have alternate key codepoints from winit; emit empty sub-param
            // which applications interpret as "same as primary key".
            if need_text { s.push(';'); } else { /* omit */ }
        }

        // Fourth parameter group: associated text Unicode codepoints (REPORT_ASSOCIATED_TEXT)
        if need_text {
            let codepoints: Vec<u32> = text.unwrap().chars().map(|c| c as u32).collect();
            let cp_str: Vec<String> = codepoints.iter().map(|c| c.to_string()).collect();
            let _ = write!(s, ";{}", cp_str.join(":"));
        }

        s.push('u');
        s.into_bytes()
    };

    // a) Named keys -> fixed control / CSI sequences.
    if let Key::Named(named) = &event.logical_key {
        let named = *named;

        // Under REPORT_ALL_KEYS_AS_ESC every named key (including unmodified Enter,
        // Backspace etc.) goes as CSI-u so the application can distinguish them
        // from their text output. We also force kitty form for any event_type != 1
        // (repeat/release) when REPORT_EVENT_TYPES is set.
        let force_kitty = kitty.report_all_keys_as_esc
            || (kitty.report_event_types && !pressed);

        if kitty.active() {
            // Try the named-key kitty form first (modified OR forced).
            let has_mods = kitty_mods != 1;
            if (has_mods || force_kitty)
                && let Some(seq) = kitty_named(named, kitty_mods, event_type,
                                               kitty.report_event_types,
                                               kitty.report_all_keys_as_esc) {
                return Some(seq);
            }
        }

        // For release events under REPORT_EVENT_TYPES (but no kitty form matched),
        // still need to send something for named keys that have a kitty code.
        if kitty.report_event_types && !pressed {
            if let Some(seq) = kitty_named(named, kitty_mods, event_type,
                                           kitty.report_event_types,
                                           kitty.report_all_keys_as_esc) {
                return Some(seq);
            }
            return None; // no kitty form → nothing on release
        }

        let seq: &[u8] = match named {
            NamedKey::Enter => b"\r",
            NamedKey::Backspace => b"\x7f",
            NamedKey::Tab => {
                if shift {
                    b"\x1b[Z".as_slice()
                } else {
                    b"\t".as_slice()
                }
            }
            NamedKey::Escape => b"\x1b",
            NamedKey::ArrowUp    => ss3_or_csi(app_cursor, b"\x1bOA", b"\x1b[A"),
            NamedKey::ArrowDown  => ss3_or_csi(app_cursor, b"\x1bOB", b"\x1b[B"),
            NamedKey::ArrowRight => ss3_or_csi(app_cursor, b"\x1bOC", b"\x1b[C"),
            NamedKey::ArrowLeft  => ss3_or_csi(app_cursor, b"\x1bOD", b"\x1b[D"),
            NamedKey::Home       => ss3_or_csi(app_cursor, b"\x1bOH", b"\x1b[H"),
            NamedKey::End        => ss3_or_csi(app_cursor, b"\x1bOF", b"\x1b[F"),
            NamedKey::PageUp     => b"\x1b[5~",
            NamedKey::PageDown   => b"\x1b[6~",
            NamedKey::Delete     => b"\x1b[3~",
            NamedKey::Insert     => b"\x1b[2~",
            NamedKey::F1  => b"\x1bOP",
            NamedKey::F2  => b"\x1bOQ",
            NamedKey::F3  => b"\x1bOR",
            NamedKey::F4  => b"\x1bOS",
            NamedKey::F5  => b"\x1b[15~",
            NamedKey::F6  => b"\x1b[17~",
            NamedKey::F7  => b"\x1b[18~",
            NamedKey::F8  => b"\x1b[19~",
            NamedKey::F9  => b"\x1b[20~",
            NamedKey::F10 => b"\x1b[21~",
            NamedKey::F11 => b"\x1b[23~",
            NamedKey::F12 => b"\x1b[24~",
            // F13-F20 (backlog item — added here as legacy tilde sequences)
            NamedKey::F13 => b"\x1b[25~",
            NamedKey::F14 => b"\x1b[26~",
            NamedKey::F15 => b"\x1b[28~",
            NamedKey::F16 => b"\x1b[29~",
            NamedKey::F17 => b"\x1b[31~",
            NamedKey::F18 => b"\x1b[32~",
            NamedKey::F19 => b"\x1b[33~",
            NamedKey::F20 => b"\x1b[34~",
            NamedKey::Space => b" ",
            _ => b"",
        };
        if seq.is_empty() {
            return None;
        }
        let mut out = Vec::with_capacity(seq.len() + 1);
        if alt && !kitty.active() {
            out.push(0x1b);
        }
        out.extend_from_slice(seq);
        return Some(out);
    }

    // b) Printable input. winit gives the locale-correct text directly.
    if let Some(text) = &event.text {
        if text.is_empty() {
            return None;
        }

        // Release events with REPORT_EVENT_TYPES: emit kitty CSI-u for the key's
        // Unicode codepoint so the application sees the release.
        if !pressed && kitty.report_event_types {
            let c = text.chars().next()?;
            return Some(kitty_csi_u(c as u32, Some(text.as_str())));
        }

        // REPORT_ALL_KEYS_AS_ESC: unmodified printable keys also go as CSI-u.
        if kitty.active() && kitty.report_all_keys_as_esc && !ctrl {
            let c = text.chars().next()?;
            let assoc = if kitty.report_associated_text { Some(text.as_str()) } else { None };
            return Some(kitty_csi_u(c as u32, assoc));
        }

        // Ctrl-<key> -> control byte.
        if ctrl && let Some(c) = text.chars().next() {
            // Under kitty DISAMBIGUATE_ESC_CODES (or higher), Ctrl+letter goes as
            // CSI-u so the application can distinguish Ctrl-I from Tab etc.
            if kitty.active() {
                let assoc = if kitty.report_associated_text { Some(text.as_str()) } else { None };
                return Some(kitty_csi_u(c as u32, assoc));
            }

            if let Some(byte) = control_byte(c) {
                // modifyOtherKeys level 2: ALL modified printable keys get CSI 27.
                // level 1: only those not covered by traditional encoding.
                let mok_emit = match modify_other_keys {
                    ModifyOtherKeys::Reset => false,
                    ModifyOtherKeys::EnableAll => true,
                    ModifyOtherKeys::EnableExceptWellDefined => {
                        // Traditional Ctrl+alpha/symbol encodings are well-defined;
                        // skip them at level 1 (only emit for unusual combos).
                        // For simplicity: level 1 still suppresses standard C0 range.
                        false
                    }
                };
                if mok_emit {
                    let mok_mods = xterm_mod_param(shift, alt, ctrl, super_);
                    return Some(
                        format!("\x1b[27;{};{}~", mok_mods, byte as u32).into_bytes()
                    );
                }
                let mut out = Vec::with_capacity(2);
                if alt {
                    out.push(0x1b);
                }
                out.push(byte);
                return Some(out);
            }
        }

        // modifyOtherKeys level 2 for Alt+printable combos not handled by kitty:
        // emit CSI 27 ; mods ; codepoint ~.
        if !kitty.active() && (alt || (ctrl && shift)) && modify_other_keys == ModifyOtherKeys::EnableAll
            && let Some(c) = text.chars().next() {
            let mok_mods = xterm_mod_param(shift, alt, ctrl, super_);
            return Some(
                format!("\x1b[27;{};{}~", mok_mods, c as u32).into_bytes()
            );
        }

        let mut out = Vec::with_capacity(text.len() + 1);
        if alt && !kitty.active() {
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

    // Legacy X10: ESC [ M  Cb  Cx  Cy, each offset by 32.
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

/// Full kitty keyboard protocol encoding for a named key.
///
/// Returns the encoded sequence if `mods != 1` (modified) OR if `force_all` is
/// set (REPORT_ALL_KEYS_AS_ESC). Returns `None` if there is no kitty codepoint
/// for this named key or neither modifier nor force is set.
fn kitty_named(
    named: NamedKey,
    mods: u8,
    event_type: u8,
    report_event_types: bool,
    force_all: bool,
) -> Option<Vec<u8>> {
    // Build the CSI suffix: either `;mods[:event_type]u` or `1;mods[:event_type]<final>`.
    let need_event = report_event_types && event_type != 1;

    // CSI <code> ; <mods>[:event_type] u
    let csi_u = |code: u32| -> Vec<u8> {
        if need_event {
            format!("\x1b[{};{}:{}u", code, mods, event_type).into_bytes()
        } else if mods != 1 || force_all {
            format!("\x1b[{};{}u", code, mods).into_bytes()
        } else {
            format!("\x1b[{};1u", code).into_bytes()
        }
    };

    // CSI 1 ; <mods>[:event_type] <final>
    let csi_final = |tail: char| -> Vec<u8> {
        if need_event {
            format!("\x1b[1;{}:{}{}", mods, event_type, tail).into_bytes()
        } else if mods != 1 || force_all {
            format!("\x1b[1;{}{}", mods, tail).into_bytes()
        } else {
            format!("\x1b[1;1{}", tail).into_bytes()
        }
    };

    // CSI <n> ; <mods>[:event_type] ~
    let csi_tilde = |n: u32| -> Vec<u8> {
        if need_event {
            format!("\x1b[{};{}:{}~", n, mods, event_type).into_bytes()
        } else if mods != 1 || force_all {
            format!("\x1b[{};{}~", n, mods).into_bytes()
        } else {
            format!("\x1b[{};1~", n).into_bytes()
        }
    };

    // Only emit kitty form when there's something to distinguish OR we're forced.
    if mods == 1 && !force_all && !need_event {
        return None;
    }

    match named {
        // CSI-u functional keys
        NamedKey::Enter     => Some(csi_u(13)),
        NamedKey::Tab       => Some(csi_u(9)),
        NamedKey::Backspace => Some(csi_u(127)),
        NamedKey::Escape    => Some(csi_u(27)),
        NamedKey::Space     => Some(csi_u(32)),
        // Cursor keys: CSI 1 ; mods [final]
        NamedKey::ArrowUp    => Some(csi_final('A')),
        NamedKey::ArrowDown  => Some(csi_final('B')),
        NamedKey::ArrowRight => Some(csi_final('C')),
        NamedKey::ArrowLeft  => Some(csi_final('D')),
        NamedKey::Home       => Some(csi_final('H')),
        NamedKey::End        => Some(csi_final('F')),
        // Tilde keys
        NamedKey::PageUp   => Some(csi_tilde(5)),
        NamedKey::PageDown => Some(csi_tilde(6)),
        NamedKey::Insert   => Some(csi_tilde(2)),
        NamedKey::Delete   => Some(csi_tilde(3)),
        // Function keys F1-F4 use SS3 form in normal mode but CSI-final in kitty.
        NamedKey::F1  => Some(csi_final('P')),
        NamedKey::F2  => Some(csi_final('Q')),
        NamedKey::F3  => Some(csi_final('R')),
        NamedKey::F4  => Some(csi_final('S')),
        // F5-F12 tilde
        NamedKey::F5  => Some(csi_tilde(15)),
        NamedKey::F6  => Some(csi_tilde(17)),
        NamedKey::F7  => Some(csi_tilde(18)),
        NamedKey::F8  => Some(csi_tilde(19)),
        NamedKey::F9  => Some(csi_tilde(20)),
        NamedKey::F10 => Some(csi_tilde(21)),
        NamedKey::F11 => Some(csi_tilde(23)),
        NamedKey::F12 => Some(csi_tilde(24)),
        // F13-F20 (kitty uses csi_tilde for these too)
        NamedKey::F13 => Some(csi_tilde(25)),
        NamedKey::F14 => Some(csi_tilde(26)),
        NamedKey::F15 => Some(csi_tilde(28)),
        NamedKey::F16 => Some(csi_tilde(29)),
        NamedKey::F17 => Some(csi_tilde(31)),
        NamedKey::F18 => Some(csi_tilde(32)),
        NamedKey::F19 => Some(csi_tilde(33)),
        NamedKey::F20 => Some(csi_tilde(34)),
        _            => None,
    }
}

/// Pick the SS3 form when DECCKM is active, otherwise the CSI form.
fn ss3_or_csi(app_cursor: bool, ss3: &'static [u8], csi: &'static [u8]) -> &'static [u8] {
    if app_cursor { ss3 } else { csi }
}

/// Map a character to its C0 control byte for Ctrl-<char>, if one exists.
fn control_byte(c: char) -> Option<u8> {
    let upper = c.to_ascii_uppercase();
    match upper {
        '@'..='_' => Some((upper as u8) & 0x1f),
        ' ' => Some(0x00),
        '?' => Some(0x7f),
        _ => None,
    }
}

/// Compute the xterm modifyOtherKeys / CSI-27 modifier parameter.
/// xterm uses: 1 + shift + alt*2 + ctrl*4 + (meta/super*8 omitted here).
fn xterm_mod_param(shift: bool, alt: bool, ctrl: bool, super_: bool) -> u8 {
    1 + (shift as u8) + (alt as u8) * 2 + (ctrl as u8) * 4 + (super_ as u8) * 8
}


#[cfg(test)]
mod tests {
    use super::{
        KittyFlags, ModifyOtherKeys, MouseReport, encode_mouse,
    };
    use winit::keyboard::ModifiersState;

    fn rep(button: u8, col: usize, row: usize, pressed: bool, motion: bool) -> MouseReport {
        MouseReport { button, col, row, pressed, motion }
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
        let m = ModifiersState::empty();
        assert_eq!(
            encode_mouse(rep(3, 0, 0, true, true), m, true),
            b"\x1b[<35;1;1M"
        );
    }

    #[test]
    fn sgr_ctrl_modifier_sets_bit() {
        let m = ModifiersState::CONTROL;
        assert_eq!(
            encode_mouse(rep(0, 0, 0, true, false), m, true),
            b"\x1b[<16;1;1M"
        );
    }

    #[test]
    fn legacy_x10_form() {
        let m = ModifiersState::empty();
        assert_eq!(
            encode_mouse(rep(0, 0, 0, true, false), m, false),
            vec![0x1b, b'[', b'M', 32, 33, 33]
        );
    }

    // ---- KittyFlags / ModifyOtherKeys unit tests ----

    #[test]
    fn kitty_flags_active_only_when_disambiguate_set() {
        let k = KittyFlags::default();
        assert!(!k.active());
        let k2 = KittyFlags { disambiguate: true, ..Default::default() };
        assert!(k2.active());
    }

    #[test]
    fn modify_other_keys_default_is_reset() {
        assert_eq!(ModifyOtherKeys::default(), ModifyOtherKeys::Reset);
    }

    // ---- kitty_named unit tests ----

    #[test]
    fn kitty_named_unmodified_returns_none() {
        use winit::keyboard::NamedKey;
        use super::kitty_named;
        // mods == 1 means unmodified; kitty_named should return None unless forced.
        assert!(kitty_named(NamedKey::Enter, 1, 1, false, false).is_none());
        assert!(kitty_named(NamedKey::ArrowUp, 1, 1, false, false).is_none());
    }

    #[test]
    fn kitty_named_ctrl_enter_is_csi_u() {
        use winit::keyboard::NamedKey;
        use super::kitty_named;
        // Ctrl = mods 5 (1 + ctrl*4), press = event_type 1.
        let seq = kitty_named(NamedKey::Enter, 5, 1, false, false).unwrap();
        assert_eq!(seq, b"\x1b[13;5u");
    }

    #[test]
    fn kitty_named_shift_alt_arrow_up() {
        use winit::keyboard::NamedKey;
        use super::kitty_named;
        // Shift+Alt = mods 3 (1 + shift + alt*2).
        let seq = kitty_named(NamedKey::ArrowUp, 3, 1, false, false).unwrap();
        assert_eq!(seq, b"\x1b[1;3A");
    }

    #[test]
    fn kitty_named_report_event_types_release() {
        use winit::keyboard::NamedKey;
        use super::kitty_named;
        // Ctrl Enter release (event_type 3) with REPORT_EVENT_TYPES.
        let seq = kitty_named(NamedKey::Enter, 5, 3, true, false).unwrap();
        assert_eq!(seq, b"\x1b[13;5:3u");
    }

    #[test]
    fn kitty_named_force_all_keys_unmodified_enter() {
        use winit::keyboard::NamedKey;
        use super::kitty_named;
        // Unmodified Enter with REPORT_ALL_KEYS_AS_ESC must still emit sequence.
        let seq = kitty_named(NamedKey::Enter, 1, 1, false, true).unwrap();
        assert_eq!(seq, b"\x1b[13;1u");
    }

    #[test]
    fn kitty_named_f5_tilde_modified() {
        use winit::keyboard::NamedKey;
        use super::kitty_named;
        // Shift F5 = mods 2.
        let seq = kitty_named(NamedKey::F5, 2, 1, false, false).unwrap();
        assert_eq!(seq, b"\x1b[15;2~");
    }
}
