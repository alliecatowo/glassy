//! Keybinding types and parsing: chords, actions, and the keymap.

use super::platform::Platform;
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

    /// Platform-correct display label. On macOS this is the Apple HIG symbol run
    /// (⌃⌥⇧⌘ printed in canonical order with no separators, e.g. `⇧⌘T`); on
    /// every other platform it is the `+`-joined form from [`Chord::display`].
    pub fn display_for(&self, platform: Platform) -> String {
        if !platform.is_mac() {
            return self.display();
        }
        // HIG modifier order: Control, Option, Shift, Command — printed together.
        let mut out = String::new();
        if self.ctrl {
            out.push('⌃');
        }
        if self.alt {
            out.push('⌥');
        }
        if self.shift {
            out.push('⇧');
        }
        if self.meta {
            out.push('⌘');
        }
        out.push_str(&self.key_label());
        out
    }

    /// The bare key portion of the chord (no modifiers), e.g. `"T"`, `"F11"`,
    /// `"Tab"`, `","`. Shared by [`Chord::display`] and [`Chord::display_for`].
    fn key_label(&self) -> String {
        let named = match self.key.as_str() {
            "tab" => Some("Tab"),
            "space" => Some("Space"),
            "enter" => Some("Enter"),
            "escape" => Some("Esc"),
            "backspace" => Some("Backspace"),
            "delete" => Some("Delete"),
            "home" => Some("Home"),
            "end" => Some("End"),
            "pageup" => Some("PgUp"),
            "pagedown" => Some("PgDn"),
            "arrowup" | "up" => Some("Up"),
            "arrowdown" | "down" => Some("Down"),
            "arrowleft" | "left" => Some("Left"),
            "arrowright" | "right" => Some("Right"),
            _ => None,
        };
        if let Some(n) = named {
            n.to_string()
        } else if self.key.starts_with('f') && self.key[1..].parse::<u32>().is_ok() {
            format!("F{}", &self.key[1..])
        } else {
            self.key.to_ascii_uppercase()
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
    /// Select the entire terminal buffer (scrollback + visible screen).
    SelectAll,
    ToggleStatusBar,
    FontIncrease,
    FontDecrease,
    FontReset,
    ScrollUp,
    ScrollDown,
    ScrollTop,
    ScrollBottom,
    /// Scroll the viewport to the previous OSC 133 prompt mark (jump-to-prompt).
    JumpPrevPrompt,
    /// Scroll the viewport to the next OSC 133 prompt mark (jump-to-prompt).
    JumpNextPrompt,
    /// Activate the tab at the given 1-based position (Ctrl/Cmd+1..9).
    GoToTab(u8),
    /// Move the active tab one slot left in the tab order.
    MoveTabLeft,
    /// Move the active tab one slot right in the tab order.
    MoveTabRight,
    /// Toggle broadcast input: typed keys/pastes go to every pane of the
    /// active tab at once.
    BroadcastInput,
    /// Open kitty-style hints mode: label every URL/path/git-SHA/IP on screen and
    /// act on the one whose label is typed.
    Hints,
    /// Toggle folding (collapse/expand output) of the command block currently in
    /// view — a Warp-style command-block affordance driven by OSC 133 marks.
    ToggleFold,
    /// Toggle the scrollback minimap / overview strip on the right edge.
    ToggleMinimap,
    /// Toggle the quake/dropdown window (slide it down/up).
    QuakeToggle,
    /// Temporarily maximize the focused split pane (hide the others); toggle
    /// again to restore the tiling. A no-op when the active tab is not split.
    ToggleZoom,
    /// Move focus to the tiled pane to the LEFT of the focused one. A no-op when
    /// the active tab is not split (the chord then falls through to the child).
    FocusPaneLeft,
    /// Move focus to the tiled pane to the RIGHT of the focused one.
    FocusPaneRight,
    /// Move focus to the tiled pane ABOVE the focused one.
    FocusPaneUp,
    /// Move focus to the tiled pane BELOW the focused one.
    FocusPaneDown,
    /// Rotate the focused pane with its split sibling (swap their positions).
    RotatePanes,
    /// Reset every split ratio to an even 50/50 partition.
    EqualizePanes,
    /// Toggle keyboard copy-mode ("vi mode"): a keyboard-driven cursor for
    /// selecting + copying text without a mouse (hjkl/word/line motions, `v`/`V`
    /// visual, `y` to yank, Esc to exit).
    ViMode,
    /// Increase window background opacity by 5%.
    IncreaseOpacity,
    /// Decrease window background opacity by 5%.
    DecreaseOpacity,
    /// Toggle between 1.0 (fully opaque) and the last non-opaque opacity.
    ToggleOpacity,
    /// Save the active pane's scrollback history to a temporary file and print its path.
    SaveScrollback,
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
            SelectAll => "Select all",
            ToggleStatusBar => "Toggle status bar",
            FontIncrease => "Font bigger",
            FontDecrease => "Font smaller",
            FontReset => "Font reset",
            ScrollUp => "Scroll history up",
            ScrollDown => "Scroll history down",
            ScrollTop => "Scroll to top",
            ScrollBottom => "Scroll to bottom",
            JumpPrevPrompt => "Jump to previous prompt",
            JumpNextPrompt => "Jump to next prompt",
            GoToTab(_) => "Go to tab N",
            MoveTabLeft => "Move tab left",
            MoveTabRight => "Move tab right",
            BroadcastInput => "Broadcast input to all panes",
            Hints => "Hints (label & open links)",
            ToggleFold => "Fold/unfold command output",
            ToggleMinimap => "Toggle minimap",
            QuakeToggle => "Toggle quake dropdown",
            ToggleZoom => "Zoom focused pane",
            FocusPaneLeft => "Focus pane left",
            FocusPaneRight => "Focus pane right",
            FocusPaneUp => "Focus pane up",
            FocusPaneDown => "Focus pane down",
            RotatePanes => "Rotate panes",
            EqualizePanes => "Equalize splits",
            ViMode => "Copy mode (keyboard selection)",
            IncreaseOpacity => "Increase opacity",
            DecreaseOpacity => "Decrease opacity",
            ToggleOpacity => "Toggle opacity (transparent ↔ opaque)",
            SaveScrollback => "Save scrollback to file",
        }
    }

    /// Section label for grouping in the help panel.
    pub fn section(self) -> &'static str {
        use KeyAction::*;
        match self {
            NewTab | ClosePane | NextTab | PrevTab | GoToTab(_) | MoveTabLeft | MoveTabRight => {
                "Tabs"
            }
            SplitVertical | SplitHorizontal | BroadcastInput | ToggleZoom | FocusPaneLeft
            | FocusPaneRight | FocusPaneUp | FocusPaneDown | RotatePanes | EqualizePanes => {
                "Split panes"
            }
            Copy | Paste | SelectAll | ViMode => "Edit",
            ToggleFullscreen | ToggleMaximize | FontIncrease | FontDecrease | FontReset
            | ToggleStatusBar | ToggleMinimap | ScrollUp | ScrollDown | ScrollTop
            | ScrollBottom | JumpPrevPrompt | JumpNextPrompt | ToggleFold | QuakeToggle
            | IncreaseOpacity | DecreaseOpacity | ToggleOpacity => "View",
            Settings | Help | Search | CommandPalette | Hints => "App",
            SaveScrollback => "Edit",
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
        "select_all" => SelectAll,
        "toggle_status_bar" => ToggleStatusBar,
        "font_increase" => FontIncrease,
        "font_decrease" => FontDecrease,
        "font_reset" => FontReset,
        "scroll_up" => ScrollUp,
        "scroll_down" => ScrollDown,
        "scroll_top" => ScrollTop,
        "scroll_bottom" => ScrollBottom,
        "jump_prev_prompt" | "prev_prompt" => JumpPrevPrompt,
        "jump_next_prompt" | "next_prompt" => JumpNextPrompt,
        "move_tab_left" => MoveTabLeft,
        "move_tab_right" => MoveTabRight,
        "broadcast_input" => BroadcastInput,
        "hints" => Hints,
        "toggle_fold" => ToggleFold,
        "toggle_minimap" => ToggleMinimap,
        "quake_toggle" => QuakeToggle,
        "toggle_zoom" | "zoom" => ToggleZoom,
        "focus_pane_left" => FocusPaneLeft,
        "focus_pane_right" => FocusPaneRight,
        "focus_pane_up" => FocusPaneUp,
        "focus_pane_down" => FocusPaneDown,
        "rotate_panes" | "rotate" => RotatePanes,
        "equalize_panes" | "equalize" => EqualizePanes,
        "vi_mode" | "copy_mode" => ViMode,
        "increase_opacity" | "opacity_up" => IncreaseOpacity,
        "decrease_opacity" | "opacity_down" => DecreaseOpacity,
        "toggle_opacity" => ToggleOpacity,
        "save_scrollback" | "scrollback_to_file" => SaveScrollback,
        // go_to_tab_1 .. go_to_tab_9 select a tab by 1-based position.
        s if s.starts_with("go_to_tab_") => match s["go_to_tab_".len()..].parse::<u8>() {
            Ok(n @ 1..=9) => GoToTab(n),
            _ => bail!("go_to_tab_N requires N in 1..=9, got '{s}'"),
        },
        other => bail!("unrecognized keybinding action '{other}'"),
    }))
}

/// The effective keymap: chord → action. Built in [`build_keymap`] by layering
/// user overrides on top of the built-in defaults.
pub type KeyMap = HashMap<Chord, KeyAction>;

/// A multi-key chord *sequence* (a "leader" binding), e.g. `ctrl+a` then `n`.
/// The map key is the full ordered list of chords; the first chord acts as the
/// leader/prefix. Built from `[keybindings]` entries whose chord token contains a
/// space (e.g. `"ctrl+a n" = next_tab`). Empty by default (no leader keys).
pub type SequenceMap = HashMap<Vec<Chord>, KeyAction>;

/// Parse a space-separated chord *sequence* like `"ctrl+a n"` into an ordered
/// list of [`Chord`]s. Each whitespace-delimited token is parsed with
/// [`parse_chord`]. Returns an error if any token is invalid or the sequence has
/// fewer than two chords (a single chord belongs in the flat [`KeyMap`]).
pub fn parse_sequence(s: &str) -> Result<Vec<Chord>> {
    let chords: Result<Vec<Chord>> = s.split_whitespace().map(parse_chord).collect();
    let chords = chords?;
    if chords.len() < 2 {
        bail!("a key sequence needs at least two chords (got '{s}')");
    }
    Ok(chords)
}

/// Split the user `[keybindings]` overrides into single-chord binds (the chord
/// token has no internal whitespace) and multi-chord sequence binds (it does).
/// The single-chord list keeps the original order for [`build_keymap`]; the
/// sequence list is parsed into a [`SequenceMap`] by [`build_sequence_map`].
/// Keeps the parser additive: one `[keybindings]` section now feeds both maps.
pub(crate) fn split_overrides(
    overrides: &[(String, String)],
) -> (Vec<(String, String)>, SequenceMap) {
    let mut singles: Vec<(String, String)> = Vec::new();
    let mut sequences: SequenceMap = HashMap::new();
    for (chord_str, action_str) in overrides {
        if chord_str.split_whitespace().count() > 1 {
            let seq = match parse_sequence(chord_str) {
                Ok(s) => s,
                Err(e) => {
                    log::warn!("glassy: ignoring bad key-sequence '{chord_str}': {e}");
                    continue;
                }
            };
            match parse_action(action_str) {
                Ok(Some(action)) => {
                    sequences.insert(seq, action);
                }
                Ok(None) => {
                    sequences.remove(&seq);
                }
                Err(e) => {
                    log::warn!("glassy: ignoring bad key-sequence action '{action_str}': {e}");
                }
            }
        } else {
            singles.push((chord_str.clone(), action_str.clone()));
        }
    }
    (singles, sequences)
}

/// The built-in default keybindings for `platform`. These are used as a base;
/// user-supplied `[keybindings]` entries override or extend them.
///
/// On macOS the primary modifier is ⌘ (Super/Meta) and follows Apple HIG
/// conventions (⌘C/⌘V/⌘T/⌘W, ⌘1-9, ⌘, for Settings, ⌘F for Find); every other
/// platform keeps the familiar Ctrl / Ctrl+Shift chords. Cross-platform binds
/// (F-keys, Shift+Page navigation) are identical on every platform and are
/// listed once in [`shared_default_binds`].
pub fn default_keymap(platform: Platform) -> KeyMap {
    let mut map = KeyMap::new();
    let push = |map: &mut KeyMap, chord_str: &str, action: KeyAction| match parse_chord(chord_str) {
        Ok(c) => {
            map.insert(c, action);
        }
        Err(e) => log::warn!("glassy: bad default chord '{chord_str}': {e}"),
    };
    if platform.is_mac() {
        for (chord_str, action) in mac_default_binds() {
            push(&mut map, chord_str, *action);
        }
    } else {
        for (chord_str, action) in pc_default_binds() {
            push(&mut map, chord_str, *action);
        }
    }
    for (chord_str, action) in shared_default_binds() {
        push(&mut map, chord_str, *action);
    }
    map
}

/// For each [`KeyAction`] bound in `keymap`, the display string of its
/// shortest-modifier chord, rendered for `platform` (⌘-symbol runs on macOS,
/// `+`-joined labels elsewhere via [`Chord::display_for`]). Shared by the help
/// panel and the command palette so both reflect the live keymap — including
/// user overrides — instead of a hardcoded chord string that silently drifts
/// from whatever the actual per-platform default (or user override) is.
pub fn action_chord_display_map(keymap: &KeyMap, platform: Platform) -> HashMap<KeyAction, String> {
    let mut action_chord: HashMap<KeyAction, String> = HashMap::new();
    let mut entries: Vec<(Chord, KeyAction)> =
        keymap.iter().map(|(c, &a)| (c.clone(), a)).collect();
    // Prefer the chord with fewest modifiers per action (deterministic when a
    // user override adds a second chord for the same action).
    entries.sort_by_key(|(c, _)| {
        let mods = (c.ctrl as u8) + (c.alt as u8) + (c.meta as u8) + (c.shift as u8);
        (mods, c.key.clone())
    });
    for (chord, action) in entries {
        action_chord
            .entry(action)
            .or_insert_with(|| chord.display_for(platform));
    }
    action_chord
}

/// Platform-agnostic default binds shared by every platform: function-key
/// toggles, Shift+Page scrollback navigation, and OSC 133 prompt jumps.
fn shared_default_binds() -> &'static [(&'static str, KeyAction)] {
    use KeyAction::*;
    &[
        ("f11", ToggleFullscreen),
        ("f10", ToggleMaximize),
        ("f1", Help),
        ("shift+pageup", ScrollUp),
        ("shift+pagedown", ScrollDown),
        ("shift+home", ScrollTop),
        ("shift+end", ScrollBottom),
        ("ctrl+shift+h", Hints),
        ("ctrl+shift+z", ToggleFold),
        ("ctrl+shift+m", ToggleMinimap),
        // Keyboard copy-mode ("vi mode"): keyboard-only select + copy.
        ("ctrl+shift+space", ViMode),
        // Quake/dropdown toggle. F12 is the de-facto dropdown-terminal key
        // (guake/yakuake) and is otherwise unbound. Only meaningful when
        // `quake = true`; in normal mode `quake_toggle` is a harmless no-op.
        ("f12", QuakeToggle),
        // Opacity control: use Ctrl+Shift+] / Ctrl+Shift+[ to nudge transparency.
        // ToggleOpacity has no default chord (it is in the command palette).
        ("ctrl+shift+]", IncreaseOpacity),
        ("ctrl+shift+[", DecreaseOpacity),
    ]
}

/// Linux / Windows default binds: Ctrl / Ctrl+Shift chords.
fn pc_default_binds() -> &'static [(&'static str, KeyAction)] {
    use KeyAction::*;
    &[
        ("ctrl+shift+t", NewTab),
        ("ctrl+shift+w", ClosePane),
        ("ctrl+tab", NextTab),
        ("ctrl+shift+tab", PrevTab),
        ("ctrl+shift+e", SplitVertical),
        ("ctrl+shift+o", SplitHorizontal),
        ("ctrl+shift+i", BroadcastInput),
        ("ctrl+shift+enter", ToggleZoom),
        ("ctrl+shift+r", RotatePanes),
        ("ctrl+shift+x", EqualizePanes),
        ("ctrl+,", Settings),
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
        ("ctrl+shift+pageup", MoveTabLeft),
        ("ctrl+shift+pagedown", MoveTabRight),
        ("ctrl+shift+up", JumpPrevPrompt),
        ("ctrl+shift+down", JumpNextPrompt),
        // Pane focus navigation: Ctrl+arrow moves between tiled panes on PC.
        // No-op (falls through to the child) when the active tab is not split.
        ("ctrl+left", FocusPaneLeft),
        ("ctrl+right", FocusPaneRight),
        ("ctrl+up", FocusPaneUp),
        ("ctrl+down", FocusPaneDown),
        ("ctrl+1", GoToTab(1)),
        ("ctrl+2", GoToTab(2)),
        ("ctrl+3", GoToTab(3)),
        ("ctrl+4", GoToTab(4)),
        ("ctrl+5", GoToTab(5)),
        ("ctrl+6", GoToTab(6)),
        ("ctrl+7", GoToTab(7)),
        ("ctrl+8", GoToTab(8)),
        ("ctrl+9", GoToTab(9)),
    ]
}

/// macOS default binds: ⌘-based chords following Apple HIG. Cmd parses to the
/// `meta` modifier bit (see [`parse_chord`]'s `cmd`/`command`/`super` aliases).
fn mac_default_binds() -> &'static [(&'static str, KeyAction)] {
    use KeyAction::*;
    &[
        ("cmd+t", NewTab),
        ("cmd+w", ClosePane),
        ("ctrl+tab", NextTab),
        ("ctrl+shift+tab", PrevTab),
        ("cmd+d", SplitVertical),
        ("cmd+shift+d", SplitHorizontal),
        ("cmd+shift+enter", ToggleZoom),
        ("cmd+shift+r", RotatePanes),
        ("cmd+shift+x", EqualizePanes),
        ("cmd+,", Settings),
        ("cmd+f", Search),
        ("cmd+shift+p", CommandPalette),
        ("cmd+c", Copy),
        ("cmd+v", Paste),
        ("cmd+a", SelectAll),
        ("cmd+shift+b", ToggleStatusBar),
        ("cmd++", FontIncrease),
        ("cmd+=", FontIncrease),
        ("cmd+-", FontDecrease),
        ("cmd+0", FontReset),
        ("cmd+shift+[", MoveTabLeft),
        ("cmd+shift+]", MoveTabRight),
        ("cmd+shift+up", JumpPrevPrompt),
        ("cmd+shift+down", JumpNextPrompt),
        // Pane focus navigation: Cmd+arrow moves between tiled panes on macOS.
        // No-op (falls through to the child) when the active tab is not split.
        ("cmd+left", FocusPaneLeft),
        ("cmd+right", FocusPaneRight),
        ("cmd+up", FocusPaneUp),
        ("cmd+down", FocusPaneDown),
        ("cmd+1", GoToTab(1)),
        ("cmd+2", GoToTab(2)),
        ("cmd+3", GoToTab(3)),
        ("cmd+4", GoToTab(4)),
        ("cmd+5", GoToTab(5)),
        ("cmd+6", GoToTab(6)),
        ("cmd+7", GoToTab(7)),
        ("cmd+8", GoToTab(8)),
        ("cmd+9", GoToTab(9)),
    ]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_input_action_round_trips() {
        // The new action parses from its config name and reports a description
        // + section like every other action.
        assert_eq!(
            parse_action("broadcast_input").unwrap(),
            Some(KeyAction::BroadcastInput)
        );
        assert!(!KeyAction::BroadcastInput.description().is_empty());
        assert_eq!(KeyAction::BroadcastInput.section(), "Split panes");
    }

    #[test]
    fn broadcast_input_has_default_binding() {
        // Ctrl+Shift+I is bound to BroadcastInput out of the box.
        let map = default_keymap(Platform::Linux);
        let chord = parse_chord("ctrl+shift+i").unwrap();
        assert_eq!(map.get(&chord), Some(&KeyAction::BroadcastInput));
    }

    #[test]
    fn toggle_zoom_action_round_trips() {
        // The zoom action parses from both its canonical and short config names
        // and reports a description + section like every other action.
        assert_eq!(
            parse_action("toggle_zoom").unwrap(),
            Some(KeyAction::ToggleZoom)
        );
        assert_eq!(parse_action("zoom").unwrap(), Some(KeyAction::ToggleZoom));
        assert!(!KeyAction::ToggleZoom.description().is_empty());
        assert_eq!(KeyAction::ToggleZoom.section(), "Split panes");
    }

    #[test]
    fn toggle_zoom_has_platform_default_binding() {
        // Ctrl+Shift+Enter (pc) and Cmd+Shift+Enter (mac) toggle zoom out of the box.
        let pc = default_keymap(Platform::Linux);
        assert_eq!(
            pc.get(&parse_chord("ctrl+shift+enter").unwrap()),
            Some(&KeyAction::ToggleZoom)
        );
        let mac = default_keymap(Platform::Mac);
        assert_eq!(
            mac.get(&parse_chord("cmd+shift+enter").unwrap()),
            Some(&KeyAction::ToggleZoom)
        );
    }

    #[test]
    fn focus_pane_actions_round_trip() {
        // Each direction parses from its config name and reports a description.
        assert_eq!(
            parse_action("focus_pane_left").unwrap(),
            Some(KeyAction::FocusPaneLeft)
        );
        assert_eq!(
            parse_action("focus_pane_right").unwrap(),
            Some(KeyAction::FocusPaneRight)
        );
        assert_eq!(
            parse_action("focus_pane_up").unwrap(),
            Some(KeyAction::FocusPaneUp)
        );
        assert_eq!(
            parse_action("focus_pane_down").unwrap(),
            Some(KeyAction::FocusPaneDown)
        );
        assert_eq!(KeyAction::FocusPaneLeft.section(), "Split panes");
        assert!(!KeyAction::FocusPaneDown.description().is_empty());
    }

    #[test]
    fn focus_pane_has_platform_default_binds() {
        // Ctrl+arrow (pc) and Cmd+arrow (mac) navigate panes out of the box.
        let pc = default_keymap(Platform::Linux);
        assert_eq!(
            pc.get(&parse_chord("ctrl+left").unwrap()),
            Some(&KeyAction::FocusPaneLeft)
        );
        assert_eq!(
            pc.get(&parse_chord("ctrl+down").unwrap()),
            Some(&KeyAction::FocusPaneDown)
        );
        let mac = default_keymap(Platform::Mac);
        assert_eq!(
            mac.get(&parse_chord("cmd+right").unwrap()),
            Some(&KeyAction::FocusPaneRight)
        );
        assert_eq!(
            mac.get(&parse_chord("cmd+up").unwrap()),
            Some(&KeyAction::FocusPaneUp)
        );
    }

    #[test]
    fn vi_mode_action_round_trips() {
        // The copy-mode action parses from both config names and reports a
        // description + section like every other action.
        assert_eq!(parse_action("vi_mode").unwrap(), Some(KeyAction::ViMode));
        assert_eq!(parse_action("copy_mode").unwrap(), Some(KeyAction::ViMode));
        assert!(!KeyAction::ViMode.description().is_empty());
        assert_eq!(KeyAction::ViMode.section(), "Edit");
    }

    #[test]
    fn vi_mode_has_default_binding() {
        // Ctrl+Shift+Space toggles copy-mode out of the box on every platform.
        for p in [Platform::Linux, Platform::Mac, Platform::Windows] {
            let map = default_keymap(p);
            let chord = parse_chord("ctrl+shift+space").unwrap();
            assert_eq!(map.get(&chord), Some(&KeyAction::ViMode), "platform {p:?}");
        }
    }

    #[test]
    fn vi_mode_binding_can_be_disabled() {
        let map = build_keymap(
            default_keymap(Platform::Linux),
            &[("ctrl+shift+space".into(), "none".into())],
        );
        assert!(!map.values().any(|&a| a == KeyAction::ViMode));
    }

    #[test]
    fn broadcast_input_binding_can_be_disabled() {
        // A user "none" override removes the default bind.
        let map = build_keymap(
            default_keymap(Platform::Linux),
            &[("ctrl+shift+i".into(), "none".into())],
        );
        assert!(!map.values().any(|&a| a == KeyAction::BroadcastInput));
    }

    #[test]
    fn select_all_action_round_trips() {
        // The select-all action parses from its config name and reports a
        // description + section like every other action.
        assert_eq!(
            parse_action("select_all").unwrap(),
            Some(KeyAction::SelectAll)
        );
        assert!(!KeyAction::SelectAll.description().is_empty());
        assert_eq!(KeyAction::SelectAll.section(), "Edit");
    }

    #[test]
    fn select_all_has_mac_default_binding_but_not_pc() {
        // Cmd+A selects all on macOS out of the box, following Apple HIG. It has
        // no default bind on Linux/Windows: Ctrl+A is shell-critical (readline's
        // "move to start of line"), so binding it would break every shell.
        let mac = default_keymap(Platform::Mac);
        assert_eq!(
            mac.get(&parse_chord("cmd+a").unwrap()),
            Some(&KeyAction::SelectAll)
        );
        let pc = default_keymap(Platform::Linux);
        assert!(!pc.values().any(|&a| a == KeyAction::SelectAll));
    }
}
