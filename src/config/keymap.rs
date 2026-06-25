//! Keybinding types and parsing: chords, actions, and the keymap.

use anyhow::{Result, bail};
use std::collections::HashMap;

/// A key chord: modifier flags + a canonical key identifier (lowercase key name
/// or named-key label). Used as the map key in [`KeyMap`].
///
/// Chords are parsed from strings like `"ctrl+shift+t"`, `"ctrl+,"`, `"f11"`.
/// The modifier bits are sorted so `ctrl+shift+t` and `shift+ctrl+t` are equal.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Chord {
    /// Whether Ctrl is held.
    pub ctrl: bool,
    /// Whether Shift is held.
    pub shift: bool,
    /// Whether Alt is held.
    pub alt: bool,
    /// Whether Super/Meta is held.
    pub meta: bool,
    /// Lowercase key label (e.g. `"t"`, `","`, `"f11"`, `"tab"`, `"space"`).
    pub key: String,
}

impl Chord {
    /// Human-readable display label, e.g. `"Ctrl+Shift+T"`.
    pub fn display(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.meta {
            parts.push("Super");
        }
        if self.shift {
            parts.push("Shift");
        }
        let key_label = match self.key.as_str() {
            "tab" => "Tab",
            "space" => "Space",
            "enter" => "Enter",
            "escape" => "Esc",
            "backspace" => "Backspace",
            "delete" => "Delete",
            "home" => "Home",
            "end" => "End",
            "pageup" => "PgUp",
            "pagedown" => "PgDn",
            "arrowup" | "up" => "Up",
            "arrowdown" | "down" => "Down",
            "arrowleft" | "left" => "Left",
            "arrowright" | "right" => "Right",
            k if k.starts_with('f') && k[1..].parse::<u32>().is_ok() => {
                // Return the uppercase F-key, e.g. "f11" → "F11".
                // We can't borrow from a local string here in a `match` arm that
                // returns `&str`, so we fall through to the owned path below.
                ""
            }
            _ => "",
        };
        if !key_label.is_empty() {
            parts.push(key_label);
            parts.join("+")
        } else if self.key.starts_with('f') && self.key[1..].parse::<u32>().is_ok() {
            let fk = format!("F{}", &self.key[1..]);
            if parts.is_empty() {
                fk
            } else {
                format!("{}+{}", parts.join("+"), fk)
            }
        } else {
            let upper_key = self.key.to_ascii_uppercase();
            parts.push(&upper_key);
            parts.join("+")
        }
    }
}

/// Parse a chord string like `"ctrl+shift+t"`, `"f11"`, `"ctrl+,"` into a
/// [`Chord`]. Case-insensitive. Returns an error on empty/unrecognized input.
pub fn parse_chord(s: &str) -> Result<Chord> {
    let s = s.trim().to_ascii_lowercase();
    if s.is_empty() {
        bail!("empty chord");
    }
    let mut ctrl = false;
    let mut shift = false;
    let mut alt = false;
    let mut meta = false;

    // Split on '+'. The tricky case is a literal '+' key, which is written as
    // the last token after splitting (e.g. "ctrl++" or "ctrl+shift++").
    let parts: Vec<&str> = s.split('+').collect();
    // Walk all but the last as modifiers; the last is the key.
    // Exception: if the last part is empty the user wrote something like
    // "ctrl++" — the key is '+'.
    let (modifier_parts, key_part) = if parts.last() == Some(&"") {
        // trailing '+' means the key is '+'
        (&parts[..parts.len() - 1], "+")
    } else {
        (&parts[..parts.len() - 1], parts[parts.len() - 1])
    };

    for m in modifier_parts {
        match *m {
            "" => {} // empty token from adjacent '+' (e.g. "ctrl++" trailing edge)
            "ctrl" | "control" => ctrl = true,
            "shift" => shift = true,
            "alt" | "option" => alt = true,
            "meta" | "super" | "cmd" | "command" | "win" | "windows" => meta = true,
            other => bail!("unrecognized modifier '{other}' in chord '{s}'"),
        }
    }

    if key_part.is_empty() {
        bail!("chord has no key: '{s}'");
    }
    let key = key_part.to_string();
    Ok(Chord {
        ctrl,
        shift,
        alt,
        meta,
        key,
    })
}

/// An action that can be bound to a chord.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KeyAction {
    NewTab,
    ClosePane,
    NextTab,
    PrevTab,
    SplitVertical,
    SplitHorizontal,
    ToggleFullscreen,
    ToggleMaximize,
    Settings,
    Help,
    Search,
    CommandPalette,
    Copy,
    Paste,
    ToggleStatusBar,
    FontIncrease,
    FontDecrease,
    FontReset,
    ScrollUp,
    ScrollDown,
    ScrollTop,
    ScrollBottom,
    ToggleMinimap,
}

impl KeyAction {
    /// Human-readable description for the help panel.
    pub fn description(self) -> &'static str {
        use KeyAction::*;
        match self {
            NewTab => "New tab",
            ClosePane => "Close pane / tab",
            NextTab => "Next tab",
            PrevTab => "Previous tab",
            SplitVertical => "Split vertical",
            SplitHorizontal => "Split horizontal",
            ToggleFullscreen => "Toggle fullscreen",
            ToggleMaximize => "Toggle maximize",
            Settings => "Settings",
            Help => "Help (this panel)",
            Search => "Find in terminal",
            CommandPalette => "Command palette",
            Copy => "Copy selection",
            Paste => "Paste",
            ToggleStatusBar => "Toggle status bar",
            FontIncrease => "Font bigger",
            FontDecrease => "Font smaller",
            FontReset => "Font reset",
            ScrollUp => "Scroll history up",
            ScrollDown => "Scroll history down",
            ScrollTop => "Scroll to top",
            ScrollBottom => "Scroll to bottom",
            ToggleMinimap => "Toggle minimap",
        }
    }

    /// Section label for grouping in the help panel.
    pub fn section(self) -> &'static str {
        use KeyAction::*;
        match self {
            NewTab | ClosePane | NextTab | PrevTab => "Tabs",
            SplitVertical | SplitHorizontal => "Split panes",
            Copy | Paste => "Edit",
            ToggleFullscreen | ToggleMaximize | FontIncrease | FontDecrease | FontReset
            | ToggleStatusBar | ToggleMinimap | ScrollUp | ScrollDown | ScrollTop
            | ScrollBottom => "View",
            Settings | Help | Search | CommandPalette => "App",
        }
    }
}

/// Parse an action name string into a [`KeyAction`]. Returns `None` for the
/// special `"none"` value (which disables a built-in bind) and an error for
/// unrecognized names.
pub(crate) fn parse_action(s: &str) -> Result<Option<KeyAction>> {
    use KeyAction::*;
    Ok(Some(match s.trim().to_ascii_lowercase().as_str() {
        "none" | "disabled" | "disable" => return Ok(None),
        "new_tab" => NewTab,
        "close_pane" => ClosePane,
        "next_tab" => NextTab,
        "prev_tab" => PrevTab,
        "split_vertical" => SplitVertical,
        "split_horizontal" => SplitHorizontal,
        "toggle_fullscreen" => ToggleFullscreen,
        "toggle_maximize" => ToggleMaximize,
        "settings" => Settings,
        "help" => Help,
        "search" => Search,
        "command_palette" => CommandPalette,
        "copy" => Copy,
        "paste" => Paste,
        "toggle_status_bar" => ToggleStatusBar,
        "font_increase" => FontIncrease,
        "font_decrease" => FontDecrease,
        "font_reset" => FontReset,
        "scroll_up" => ScrollUp,
        "scroll_down" => ScrollDown,
        "scroll_top" => ScrollTop,
        "scroll_bottom" => ScrollBottom,
        "toggle_minimap" => ToggleMinimap,
        other => bail!("unrecognized keybinding action '{other}'"),
    }))
}

/// The effective keymap: chord → action. Built in [`build_keymap`] by layering
/// user overrides on top of the built-in defaults.
pub type KeyMap = HashMap<Chord, KeyAction>;

/// The built-in default keybindings. These are used as a base; user-supplied
/// `[keybindings]` entries override or extend them.
pub fn default_keymap() -> KeyMap {
    use KeyAction::*;
    let defaults: &[(&str, KeyAction)] = &[
        ("ctrl+shift+t", NewTab),
        ("ctrl+shift+w", ClosePane),
        ("ctrl+tab", NextTab),
        ("ctrl+shift+tab", PrevTab),
        ("ctrl+shift+e", SplitVertical),
        ("ctrl+shift+o", SplitHorizontal),
        ("f11", ToggleFullscreen),
        ("f10", ToggleMaximize),
        ("ctrl+,", Settings),
        ("f1", Help),
        ("ctrl+shift+f", Search),
        ("ctrl+shift+p", CommandPalette),
        ("ctrl+shift+c", Copy),
        ("ctrl+shift+v", Paste),
        ("ctrl+shift+b", ToggleStatusBar),
        ("ctrl+shift+m", ToggleMinimap),
        ("ctrl++", FontIncrease),
        ("ctrl+=", FontIncrease),
        ("ctrl+-", FontDecrease),
        ("ctrl+0", FontReset),
        ("shift+pageup", ScrollUp),
        ("shift+pagedown", ScrollDown),
        ("shift+home", ScrollTop),
        ("shift+end", ScrollBottom),
    ];
    let mut map = KeyMap::new();
    for (chord_str, action) in defaults {
        match parse_chord(chord_str) {
            Ok(c) => {
                map.insert(c, *action);
            }
            Err(e) => log::warn!("glassy: bad default chord '{chord_str}': {e}"),
        }
    }
    map
}

/// Apply user-supplied keybinding overrides (`[keybindings]` section) onto `base`,
/// returning the merged effective keymap. `overrides` is a list of `(chord, action)`
/// raw string pairs as parsed from the config file.
///
/// An action of `"none"` removes a chord from the map (disables the default).
pub(crate) fn build_keymap(mut base: KeyMap, overrides: &[(String, String)]) -> KeyMap {
    for (chord_str, action_str) in overrides {
        let chord = match parse_chord(chord_str) {
            Ok(c) => c,
            Err(e) => {
                log::warn!("glassy: ignoring bad keybinding chord '{chord_str}': {e}");
                continue;
            }
        };
        match parse_action(action_str) {
            Ok(Some(action)) => {
                base.insert(chord, action);
            }
            Ok(None) => {
                base.remove(&chord);
            } // "none" disables the default
            Err(e) => {
                log::warn!("glassy: ignoring bad keybinding action '{action_str}': {e}");
            }
        }
    }
    base
}
