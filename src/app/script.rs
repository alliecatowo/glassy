//! Scripted-input test harness.
//!
//! GNOME 49 Wayland blocks the screenshot portal, and there is no way to drive a
//! real mouse/keyboard against the window from outside. So we synthesize input
//! INSIDE the app and route it through the *real* event handlers — the exact same
//! `handle_cursor_moved` / `handle_mouse_input` / `handle_keyboard_parts` /
//! `handle_mouse_wheel` / `render` paths a live user hits. That faithfully
//! reproduces interaction bugs (an overlay that dismisses on motion, a panic when
//! clicking `+`, a click-edge that never resolves, …).
//!
//! Activated by the `GLASSY_SCRIPT` env var, which is either an inline
//! `;`-separated command string or a path to a script file (one command per
//! line). The runner advances ONE command per `about_to_wait` wake; this keeps us
//! on `ControlFlow::Poll` only while a script is in flight, so the normal
//! interactive path and its 0%-idle invariant are untouched.
//!
//! Commands (blank lines and `#` comments are ignored):
//!   move X Y                 cursor motion to physical px (X, Y)
//!   down [left|right|middle] button press at the current cursor pos (default left)
//!   up   [left|right|middle] button release
//!   click X Y [button]       move; down; up  (a full click at a point)
//!   key NAME                 a key press+release (Escape, Enter, F1, a,
//!                            ctrl+shift+p — modifiers joined by '+')
//!   text "STR"               type a literal string (quotes optional)
//!   scroll DX DY             a mouse-wheel line delta
//!   wait N                   advance N redraw/render cycles (let animations settle)
//!   capture PATH             render one frame and write it to PATH as a PPM

use winit::dpi::PhysicalPosition;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, TouchPhase};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, ModifiersState, NamedKey, SmolStr};

/// A single parsed script command. Each maps to one real event-handler call (or a
/// short, deterministic sequence of them) when stepped by the runner.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Cmd {
    /// Cursor motion to physical pixel (x, y).
    Move(f64, f64),
    /// Button press at the current pointer position.
    Down(MouseButton),
    /// Button release at the current pointer position.
    Up(MouseButton),
    /// Move to (x, y), then press+release `button` (a full click).
    Click(f64, f64, MouseButton),
    /// A key press immediately followed by a release, with `mods` held.
    Key { key: Key, mods: ModifiersState },
    /// Type a literal string, one character key-press at a time.
    Text(String),
    /// A mouse-wheel line delta (dx, dy).
    Scroll(f32, f32),
    /// Advance `n` extra redraw/render cycles so animations/blink settle.
    Wait(u32),
    /// Render one frame and write it to `path` as a PPM.
    Capture(std::path::PathBuf),
}

/// Holds the parsed program and the cursor into it. Driven one step per wake.
pub(crate) struct ScriptRunner {
    cmds: Vec<Cmd>,
    /// Index of the next command to run.
    pc: usize,
    /// Remaining no-op render cycles for an in-progress `wait`.
    wait_left: u32,
    /// Whether the post-init settle delay has elapsed (mirrors the capture
    /// bootstrap: give the shell time to draw a prompt before the first command).
    warmed_up: bool,
}

impl ScriptRunner {
    /// Build a runner from the `GLASSY_SCRIPT` value: if it names an existing file,
    /// read it; otherwise treat the value itself as an inline `;`-separated script.
    pub(crate) fn from_env(value: &std::ffi::OsStr) -> Self {
        let raw = value.to_string_lossy();
        let source = match std::fs::read_to_string(raw.as_ref()) {
            Ok(contents) => contents,
            Err(_) => raw.to_string(),
        };
        ScriptRunner {
            cmds: parse_script(&source),
            pc: 0,
            wait_left: 0,
            warmed_up: false,
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.pc >= self.cmds.len()
    }
}

/// Parse the whole script into a command list. Commands are separated by newlines
/// AND by `;` (so an inline `"a; b; c"` string and a multi-line file behave the
/// same). Blank items and `#`-prefixed comments are skipped; an unparseable line
/// is skipped (and logged) rather than aborting the run.
pub(crate) fn parse_script(source: &str) -> Vec<Cmd> {
    source
        .lines()
        .flat_map(|line| line.split(';'))
        .filter_map(|item| {
            let trimmed = item.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                return None;
            }
            match parse_cmd(trimmed) {
                Some(cmd) => Some(cmd),
                None => {
                    log::warn!("GLASSY_SCRIPT: ignoring unparseable command: {trimmed:?}");
                    None
                }
            }
        })
        .collect()
}

/// Parse one already-trimmed, non-empty, non-comment command line.
fn parse_cmd(line: &str) -> Option<Cmd> {
    let mut parts = line.split_whitespace();
    let verb = parts.next()?;
    match verb {
        "move" => {
            let x = parts.next()?.parse().ok()?;
            let y = parts.next()?.parse().ok()?;
            Some(Cmd::Move(x, y))
        }
        "down" => Some(Cmd::Down(parse_button(parts.next()))),
        "up" => Some(Cmd::Up(parse_button(parts.next()))),
        "click" => {
            let x = parts.next()?.parse().ok()?;
            let y = parts.next()?.parse().ok()?;
            let button = parse_button(parts.next());
            Some(Cmd::Click(x, y, button))
        }
        "key" => {
            let spec = parts.next()?;
            let (key, mods) = parse_key_spec(spec)?;
            Some(Cmd::Key { key, mods })
        }
        "text" => {
            // Everything after the verb is the payload; strip one layer of
            // matching single/double quotes if present.
            let rest = line[verb.len()..].trim();
            let unquoted = strip_quotes(rest);
            Some(Cmd::Text(unquoted.to_string()))
        }
        "scroll" => {
            let dx = parts.next()?.parse().ok()?;
            let dy = parts.next()?.parse().ok()?;
            Some(Cmd::Scroll(dx, dy))
        }
        "wait" => {
            let n = parts.next()?.parse().ok()?;
            Some(Cmd::Wait(n))
        }
        "capture" => {
            let path = parts.next()?;
            Some(Cmd::Capture(std::path::PathBuf::from(path)))
        }
        _ => None,
    }
}

/// Map an optional `left|right|middle` token to a winit button (default left).
fn parse_button(tok: Option<&str>) -> MouseButton {
    match tok {
        Some("right") => MouseButton::Right,
        Some("middle") => MouseButton::Middle,
        _ => MouseButton::Left,
    }
}

/// Strip one matching layer of surrounding single or double quotes.
fn strip_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2
        && (bytes[0] == b'"' || bytes[0] == b'\'')
        && bytes[bytes.len() - 1] == bytes[0]
    {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Parse a `key` spec like `Escape`, `a`, `ctrl+shift+p`, `Ctrl+Enter` into a
/// logical key plus the modifier state to hold while it is pressed. The LAST
/// `+`-segment is the key; the leading segments are modifiers.
fn parse_key_spec(spec: &str) -> Option<(Key, ModifiersState)> {
    let mut mods = ModifiersState::empty();
    // A lone `+` (typing a plus) is the key itself; anything else ending in `+`
    // (e.g. `ctrl+`) is a dangling modifier with no key — reject it.
    if spec == "+" {
        return Some((Key::Character(SmolStr::new("+")), mods));
    }
    if spec.ends_with('+') {
        return None;
    }
    let mut segments: Vec<&str> = spec.split('+').filter(|s| !s.is_empty()).collect();
    if segments.is_empty() {
        return None;
    }
    let key_tok = segments.pop()?;
    for m in &segments {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= ModifiersState::CONTROL,
            "shift" => mods |= ModifiersState::SHIFT,
            "alt" | "option" => mods |= ModifiersState::ALT,
            "super" | "meta" | "cmd" | "win" => mods |= ModifiersState::SUPER,
            _ => return None,
        }
    }
    let key = named_key(key_tok).map(Key::Named).unwrap_or_else(|| {
        // A single printable character becomes Key::Character; honor an explicit
        // shift modifier by upper-casing so `shift+a` types `A`.
        let mut s = key_tok.to_string();
        if mods.shift_key() {
            s = s.to_uppercase();
        }
        Key::Character(SmolStr::new(s))
    });
    Some((key, mods))
}

/// Map a key name (case-insensitive) to a winit `NamedKey`. Returns `None` for
/// names that should be treated as printable `Key::Character` instead.
fn named_key(name: &str) -> Option<NamedKey> {
    Some(match name.to_ascii_lowercase().as_str() {
        "escape" | "esc" => NamedKey::Escape,
        "enter" | "return" => NamedKey::Enter,
        "tab" => NamedKey::Tab,
        "space" => NamedKey::Space,
        "backspace" => NamedKey::Backspace,
        "delete" | "del" => NamedKey::Delete,
        "home" => NamedKey::Home,
        "end" => NamedKey::End,
        "pageup" | "pgup" => NamedKey::PageUp,
        "pagedown" | "pgdn" => NamedKey::PageDown,
        "up" | "arrowup" => NamedKey::ArrowUp,
        "down" | "arrowdown" => NamedKey::ArrowDown,
        "left" | "arrowleft" => NamedKey::ArrowLeft,
        "right" | "arrowright" => NamedKey::ArrowRight,
        "f1" => NamedKey::F1,
        "f2" => NamedKey::F2,
        "f3" => NamedKey::F3,
        "f4" => NamedKey::F4,
        "f5" => NamedKey::F5,
        "f6" => NamedKey::F6,
        "f7" => NamedKey::F7,
        "f8" => NamedKey::F8,
        "f9" => NamedKey::F9,
        "f10" => NamedKey::F10,
        "f11" => NamedKey::F11,
        "f12" => NamedKey::F12,
        _ => return None,
    })
}

/// The text a `Key` produces, for the printable input path. Named keys produce
/// their canonical control text where one exists (Enter -> "\r", Tab -> "\t",
/// Space -> " "); everything else relies on `Key::Character`.
fn key_text(key: &Key) -> Option<SmolStr> {
    match key {
        Key::Character(s) => Some(s.clone()),
        Key::Named(NamedKey::Enter) => Some(SmolStr::new("\r")),
        Key::Named(NamedKey::Tab) => Some(SmolStr::new("\t")),
        Key::Named(NamedKey::Space) => Some(SmolStr::new(" ")),
        _ => None,
    }
}

impl super::App {
    /// If `GLASSY_SCRIPT` is set, build the runner and switch the loop to drive it.
    /// Mirrors how the `GLASSY_CAPTURE` bootstrap arms its deadline. Called from
    /// `resumed()` after the window + renderer + pty are live.
    pub(crate) fn maybe_start_script(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(value) = std::env::var_os("GLASSY_SCRIPT") {
            self.script = Some(ScriptRunner::from_env(&value));
            // Keep getting woken so the runner can advance every step.
            event_loop.set_control_flow(winit::event_loop::ControlFlow::Poll);
        }
    }

    /// Advance the script by one step, called once per `about_to_wait` wake while a
    /// script is active. Returns `true` while the script is still running (the
    /// caller stays on `Poll`); `false` once it has finished and the loop should
    /// exit. Each input command routes through the SAME handler the winit
    /// dispatcher calls for a real event.
    pub(crate) fn step_script(&mut self, event_loop: &ActiveEventLoop) -> bool {
        // One settle pass before the first command so the shell can paint a prompt
        // (same rationale as the capture delay), giving scripted clicks real chrome
        // to hit.
        if !self.script.as_ref().map(|s| s.warmed_up).unwrap_or(true) {
            self.recompute_search();
            self.settle_cycle();
            if let Some(s) = self.script.as_mut() {
                s.warmed_up = true;
            }
            return true;
        }

        // Service an in-progress `wait`: one settle cycle per remaining tick.
        if let Some(s) = self.script.as_mut()
            && s.wait_left > 0
        {
            s.wait_left -= 1;
            self.settle_cycle();
            return true;
        }

        let next = self.script.as_mut().and_then(|s| {
            let cmd = s.cmds.get(s.pc).cloned();
            if cmd.is_some() {
                s.pc += 1;
            }
            cmd
        });

        let Some(cmd) = next else {
            // Out of commands: render one last frame and signal completion.
            self.render();
            return false;
        };

        self.run_script_cmd(cmd, event_loop);
        // Keep running unless that was the final command.
        !self.script.as_ref().map(|s| s.is_done()).unwrap_or(true)
    }

    /// Advance one settle cycle: step GUI chrome animations (hover/press fades,
    /// toggle slides) by a fixed dt so they converge deterministically without
    /// relying on wall-clock between `Poll` wakes, then render a frame. Mirrors the
    /// animation step `about_to_wait` runs on the live path, so a scripted `wait`
    /// settles the same widgets a real idle would.
    fn settle_cycle(&mut self) {
        // 1/60 s per cycle at the chrome's 12.0 spring rate converges a fade in a
        // handful of cycles, matching a human pausing between actions.
        crate::gui::step_anims(&mut self.gui_anims, 1.0 / 60.0, 12.0);
        self.gui_anims.retain(|_, a| !a.is_settled());
        self.render();
    }

    /// Execute one parsed command against the real handlers.
    fn run_script_cmd(&mut self, cmd: Cmd, event_loop: &ActiveEventLoop) {
        match cmd {
            Cmd::Move(x, y) => {
                self.handle_cursor_moved(PhysicalPosition::new(x, y), event_loop);
            }
            Cmd::Down(button) => self.synth_button(ElementState::Pressed, button, event_loop),
            Cmd::Up(button) => self.synth_button(ElementState::Released, button, event_loop),
            Cmd::Click(x, y, button) => {
                self.handle_cursor_moved(PhysicalPosition::new(x, y), event_loop);
                self.synth_button(ElementState::Pressed, button, event_loop);
                self.synth_button(ElementState::Released, button, event_loop);
            }
            Cmd::Key { key, mods } => self.synth_key(key, mods, event_loop),
            Cmd::Text(s) => {
                for ch in s.chars() {
                    let key = Key::Character(SmolStr::new(ch.to_string()));
                    self.synth_key(key, ModifiersState::empty(), event_loop);
                }
            }
            Cmd::Scroll(dx, dy) => {
                self.handle_mouse_wheel(
                    MouseScrollDelta::LineDelta(dx, dy),
                    TouchPhase::Moved,
                    event_loop,
                );
            }
            Cmd::Wait(n) => {
                // Consume one tick now (this step) and queue the rest.
                if let Some(s) = self.script.as_mut() {
                    s.wait_left = n.saturating_sub(1);
                }
                self.settle_cycle();
            }
            Cmd::Capture(path) => {
                // Mirror the GLASSY_CAPTURE readback exactly: build the frame, then
                // dump it via the existing GPU readback (split-aware).
                let split = self.is_split();
                self.render();
                if let Some(renderer) = self.renderer.as_mut() {
                    let res = if split {
                        renderer.capture_multi(&path)
                    } else {
                        renderer.capture(&path)
                    };
                    match res {
                        Ok(()) => log::info!("GLASSY_SCRIPT: captured frame to {}", path.display()),
                        Err(e) => log::error!("GLASSY_SCRIPT: capture failed: {e:#}"),
                    }
                }
            }
        }
    }

    /// Feed a synthetic mouse button event through the real handler. `self.mods`
    /// is left as-is (clicks carry no modifiers in this harness).
    fn synth_button(
        &mut self,
        state: ElementState,
        button: MouseButton,
        event_loop: &ActiveEventLoop,
    ) {
        self.handle_mouse_input(state, button, event_loop);
    }

    /// Feed a synthetic key press+release through the real keyboard handler,
    /// holding `mods` for the duration. Sets `self.mods` (the field the handler and
    /// `encode_key_parts` read) so chords like `ctrl+shift+p` resolve correctly,
    /// then restores it.
    fn synth_key(&mut self, key: Key, mods: ModifiersState, event_loop: &ActiveEventLoop) {
        let prev = self.mods;
        self.mods = mods;
        let text = key_text(&key);
        self.handle_keyboard_parts(
            key.clone(),
            text.clone(),
            ElementState::Pressed,
            false,
            event_loop,
        );
        self.handle_keyboard_parts(key, text, ElementState::Released, false, event_loop);
        self.mods = prev;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_semicolon_script() {
        let cmds = parse_script("click 1160 20; capture /tmp/h1.ppm; move 600 300");
        assert_eq!(
            cmds,
            vec![
                Cmd::Click(1160.0, 20.0, MouseButton::Left),
                Cmd::Capture(std::path::PathBuf::from("/tmp/h1.ppm")),
                Cmd::Move(600.0, 300.0),
            ]
        );
    }

    #[test]
    fn parses_multiline_with_comments_and_blanks() {
        let src = "# a comment\n\nmove 10 20\n  # indented comment\ndown right\nup\n";
        let cmds = parse_script(src);
        assert_eq!(
            cmds,
            vec![
                Cmd::Move(10.0, 20.0),
                Cmd::Down(MouseButton::Right),
                Cmd::Up(MouseButton::Left),
            ]
        );
    }

    #[test]
    fn defaults_button_to_left() {
        assert_eq!(parse_cmd("down"), Some(Cmd::Down(MouseButton::Left)));
        assert_eq!(parse_cmd("up middle"), Some(Cmd::Up(MouseButton::Middle)));
        assert_eq!(
            parse_cmd("click 5 6"),
            Some(Cmd::Click(5.0, 6.0, MouseButton::Left))
        );
    }

    #[test]
    fn parses_named_keys_and_modifiers() {
        // Bare named key, no modifiers.
        assert_eq!(
            parse_cmd("key Escape"),
            Some(Cmd::Key {
                key: Key::Named(NamedKey::Escape),
                mods: ModifiersState::empty(),
            })
        );
        // Function key, case-insensitive.
        assert_eq!(
            parse_cmd("key f1"),
            Some(Cmd::Key {
                key: Key::Named(NamedKey::F1),
                mods: ModifiersState::empty(),
            })
        );
        // Chord with two modifiers; last segment is the character key.
        let Some(Cmd::Key { key, mods }) = parse_cmd("key ctrl+shift+p") else {
            panic!("expected a key command");
        };
        assert!(mods.control_key() && mods.shift_key() && !mods.alt_key());
        // shift uppercases the produced character.
        assert_eq!(key, Key::Character(SmolStr::new("P")));
    }

    #[test]
    fn parses_text_with_and_without_quotes() {
        assert_eq!(
            parse_cmd("text \"hello world\""),
            Some(Cmd::Text("hello world".to_string()))
        );
        assert_eq!(parse_cmd("text bare"), Some(Cmd::Text("bare".to_string())));
        // Single quotes also strip.
        assert_eq!(
            parse_cmd("text 'ls -la'"),
            Some(Cmd::Text("ls -la".to_string()))
        );
    }

    #[test]
    fn parses_scroll_wait_capture() {
        assert_eq!(parse_cmd("scroll 0 -3"), Some(Cmd::Scroll(0.0, -3.0)));
        assert_eq!(parse_cmd("wait 5"), Some(Cmd::Wait(5)));
        assert_eq!(
            parse_cmd("capture /tmp/x.ppm"),
            Some(Cmd::Capture(std::path::PathBuf::from("/tmp/x.ppm")))
        );
    }

    #[test]
    fn rejects_malformed_lines() {
        assert_eq!(parse_cmd("move"), None); // missing coords
        assert_eq!(parse_cmd("move 1"), None); // missing y
        assert_eq!(parse_cmd("move a b"), None); // non-numeric
        assert_eq!(parse_cmd("bogus 1 2"), None); // unknown verb
        assert_eq!(parse_cmd("key ctrl+"), None); // dangling modifier, no key
        // Bad lines are dropped from a whole-script parse, good ones survive.
        let cmds = parse_script("move 1 2\nbogus\nwait 1");
        assert_eq!(cmds, vec![Cmd::Move(1.0, 2.0), Cmd::Wait(1)]);
    }
}
