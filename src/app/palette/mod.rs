//! Command palette (Ctrl+Shift+P): a centered glass overlay with a query field
//! and a fuzzy-filtered list of EVERY action and inline-settable setting, driven
//! from one registry so it stays in lock-step with the menus + settings form.
//!
//! Idle stays at 0%: the palette paints only while open and repaints only on a
//! real change (a keystroke, an arrow, a hover-row change, a click). It schedules
//! no timer and never forces `Poll` — between interactions the overlay is static.

use super::*;

use crate::config::KeyAction;
use fuzzy::fuzzy_score;
use history::{compact_home, shell_quote};

mod fuzzy;
mod history;
mod paint;
pub(crate) mod save_scrollback;

/// One palette command: a stable identifier the App maps to a concrete effect,
/// plus the display metadata (category + label) the registry builds it with.
///
/// The variants intentionally mirror the menu actions AND the settings form so a
/// single fuzzy list covers "do X" and "change setting Y" uniformly. Themes are
/// carried by index into [`color::theme_names`] so adding a theme to the
/// registry surfaces it in the palette for free.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PaletteCmd {
    // --- Tabs / panes ---
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    SplitVertical,
    SplitHorizontal,
    ClosePane,
    ToggleBroadcastInput,
    ToggleZoom,
    /// Rotate the focused pane with its split sibling (swap positions).
    RotatePanes,
    /// Reset all split ratios to an even 50/50 partition.
    EqualizePanes,
    /// Toggle dimming of unfocused pane content.
    ToggleDimUnfocused,
    /// Save the current split shape under a name (carried in the entry payload).
    SaveLayout,
    /// Restore a saved split shape by name (carried in the entry payload).
    RestoreLayout,
    // --- Overlays ---
    OpenSettings,
    OpenHelp,
    OpenSearch,
    // --- Clipboard ---
    Copy,
    Paste,
    // --- Window ---
    ToggleFullscreen,
    /// Slide the quake/dropdown window away (only registered in quake mode).
    ToggleQuake,
    // --- Font ---
    FontIncrease,
    FontDecrease,
    FontReset,
    // --- Settings: toggles / cycles ---
    ToggleStatusBar,
    ToggleMinimap,
    TogglePaneHeaders,
    /// Toggle the Power Mode typing effect (cursor particle bursts + streak shake).
    TogglePowerMode,
    BellOff,
    BellVisual,
    BellAudible,
    ScrollbackIncrease,
    ScrollbackDecrease,
    NextTheme,
    PrevTheme,
    /// Fold/unfold the output of the command block in view (OSC 133).
    ToggleFold,
    /// Set the theme at this index into [`color::theme_names`].
    SetTheme(usize),
    /// Generate a theme from the configured `wallpaper_theme` image path and apply it live.
    GenerateThemeFromWallpaper,
    // --- Opacity ---
    /// Increase window background opacity by 5%.
    IncreaseOpacity,
    /// Decrease window background opacity by 5%.
    DecreaseOpacity,
    /// Set opacity to the exact value 0..<n>/20 (stored as integer tenths×2 for
    /// lossless round-trip; 0=0.0, 20=1.0). Payload string carries the percentage.
    SetOpacity(u8),
    /// Toggle between the current opacity and 1.0 (fully opaque). Useful as a
    /// quick "make it readable / restore transparency" action.
    ToggleOpacity,
    // --- GPU effects ---
    /// Toggle the CRT/glow/scanline post-process (config `crt_effect`).
    ToggleCrtEffect,
    /// Toggle the cursor-trail smooth-glide effect (config `cursor_trail`).
    ToggleCursorTrail,
    // --- Toggle settings not yet in the palette ---
    /// Toggle cursor blink (config `cursor_blink`).
    ToggleCursorBlink,
    /// Toggle the "follow system light/dark" theme mode (config `follow_system`).
    ToggleFollowSystem,
    /// Toggle OpenType ligature shaping (config `ligatures`).
    ToggleLigatures,
    /// Toggle session restore on launch (config `restore_session`).
    ToggleRestoreSession,
    /// Toggle copy-on-select / PRIMARY selection (config `copy_on_select`).
    ToggleCopyOnSelect,
    /// Toggle OSC 133 command-block badges + fold affordance (config
    /// `command_badges`).
    ToggleCommandBadges,
    // --- Scrollback ---
    /// Write the current pane's full scrollback (terminal text, no ANSI) to a
    /// file chosen via a temp path, then echo the path so the user can open it.
    SaveScrollbackToFile,
    // --- Dynamic history sources (payload carried by the PaletteEntry) ---
    /// Re-run a command from history: paste its text into the focused pane and
    /// submit it (append a newline). The command string lives in
    /// [`PaletteEntry::payload`].
    RunCommand,
    /// Change into a recent working directory: paste `cd <dir>` and submit. The
    /// directory string lives in [`PaletteEntry::payload`].
    CdTo,
    /// Switch the live runtime profile to the `[profile.NAME]` whose name lives in
    /// [`PaletteEntry::payload`]. Applies the profile's live-applicable settings
    /// immediately (theme / opacity / bell / status / panes / word seps).
    SwitchProfile,
}

impl PaletteCmd {
    /// Category prefix shown dim before the label (groups the list visually).
    fn category(self) -> &'static str {
        use PaletteCmd::*;
        match self {
            NewTab | CloseTab | NextTab | PrevTab => "Tab",
            SplitVertical | SplitHorizontal | ClosePane | ToggleBroadcastInput | ToggleZoom
            | RotatePanes | EqualizePanes | ToggleDimUnfocused | SaveLayout | RestoreLayout => {
                "Pane"
            }
            OpenSettings | OpenHelp | OpenSearch => "View",
            Copy | Paste => "Edit",
            ToggleFullscreen | ToggleQuake => "Window",
            FontIncrease | FontDecrease | FontReset => "Font",
            ToggleStatusBar | ToggleMinimap | TogglePaneHeaders | TogglePowerMode | BellOff
            | BellVisual | BellAudible | ScrollbackIncrease | ScrollbackDecrease => "Setting",
            ToggleFold => "View",
            NextTheme | PrevTheme | SetTheme(_) | GenerateThemeFromWallpaper => "Theme",
            IncreaseOpacity | DecreaseOpacity | SetOpacity(_) | ToggleOpacity => "Opacity",
            ToggleCrtEffect | ToggleCursorTrail => "Effects",
            ToggleCursorBlink | ToggleFollowSystem | ToggleLigatures | ToggleRestoreSession
            | ToggleCopyOnSelect | ToggleCommandBadges => "Setting",
            SaveScrollbackToFile => "Edit",
            RunCommand => "History",
            CdTo => "Cwd",
            SwitchProfile => "Profile",
        }
    }

    /// Human label shown in the list (also the fuzzy-match haystack, together with
    /// the category). For `SetTheme` the label is filled in by the registry from
    /// the theme name, so this returns an empty owned string for that variant.
    fn label(self) -> String {
        use PaletteCmd::*;
        match self {
            NewTab => "New tab".into(),
            CloseTab => "Close tab".into(),
            NextTab => "Next tab".into(),
            PrevTab => "Previous tab".into(),
            SplitVertical => "Split vertical (left | right)".into(),
            SplitHorizontal => "Split horizontal (top / bottom)".into(),
            ClosePane => "Close pane".into(),
            ToggleBroadcastInput => "Toggle broadcast input (all panes)".into(),
            ToggleZoom => "Zoom / unzoom focused pane".into(),
            RotatePanes => "Rotate panes (swap with sibling)".into(),
            EqualizePanes => "Equalize splits (even sizes)".into(),
            ToggleDimUnfocused => "Toggle dim unfocused panes".into(),
            // Save/Restore labels are filled in by the registry from the payload.
            SaveLayout | RestoreLayout => String::new(),
            OpenSettings => "Settings".into(),
            OpenHelp => "Help / keybindings".into(),
            OpenSearch => "Find in terminal".into(),
            Copy => "Copy selection".into(),
            Paste => "Paste".into(),
            ToggleFullscreen => "Toggle fullscreen".into(),
            ToggleQuake => "Toggle quake dropdown".into(),
            FontIncrease => "Increase font size".into(),
            FontDecrease => "Decrease font size".into(),
            FontReset => "Reset font size".into(),
            ToggleStatusBar => "Toggle status bar".into(),
            ToggleMinimap => "Toggle minimap".into(),
            TogglePaneHeaders => "Toggle pane headers".into(),
            TogglePowerMode => "Toggle Power Mode (typing effect)".into(),
            BellOff => "Bell: off".into(),
            BellVisual => "Bell: visual".into(),
            BellAudible => "Bell: audible".into(),
            ScrollbackIncrease => "Scrollback +1000 lines".into(),
            ScrollbackDecrease => "Scrollback −1000 lines".into(),
            NextTheme => "Next theme".into(),
            PrevTheme => "Previous theme".into(),
            ToggleFold => "Fold/unfold command output".into(),
            SetTheme(_) => String::new(),
            GenerateThemeFromWallpaper => "Generate theme from wallpaper image".into(),
            IncreaseOpacity => "Increase opacity (+5%)".into(),
            DecreaseOpacity => "Decrease opacity (−5%)".into(),
            SetOpacity(v) => format!("Set opacity {}%", v * 5),
            ToggleOpacity => "Toggle opacity (current ↔ 100%)".into(),
            ToggleCrtEffect => "Toggle CRT/glow/scanline effect".into(),
            ToggleCursorTrail => "Toggle cursor trail (smooth glide)".into(),
            ToggleCursorBlink => "Toggle cursor blink".into(),
            ToggleFollowSystem => "Toggle follow system light/dark theme".into(),
            ToggleLigatures => "Toggle ligature shaping".into(),
            ToggleRestoreSession => "Toggle restore session on launch".into(),
            ToggleCopyOnSelect => "Toggle copy-on-select".into(),
            ToggleCommandBadges => "Toggle command badges (OSC 133)".into(),
            SaveScrollbackToFile => "Save scrollback to file".into(),
            // History/cwd/profile labels are filled in by the registry from the payload.
            RunCommand | CdTo | SwitchProfile => String::new(),
        }
    }

    /// The [`KeyAction`] this command corresponds to, if any — used to look up
    /// its live, platform-correct chord in the keymap (see [`PaletteCmd::hint`]).
    /// `F11`/`F12`/`Ctrl+,`-style chords are shared across platforms and are
    /// still resolved through the keymap rather than hardcoded, so a user
    /// override is reflected here too.
    fn key_action(self) -> Option<KeyAction> {
        use PaletteCmd::*;
        Some(match self {
            NewTab => KeyAction::NewTab,
            CloseTab => KeyAction::ClosePane,
            NextTab => KeyAction::NextTab,
            PrevTab => KeyAction::PrevTab,
            SplitVertical => KeyAction::SplitVertical,
            SplitHorizontal => KeyAction::SplitHorizontal,
            OpenSettings => KeyAction::Settings,
            OpenHelp => KeyAction::Help,
            OpenSearch => KeyAction::Search,
            Copy => KeyAction::Copy,
            Paste => KeyAction::Paste,
            ToggleBroadcastInput => KeyAction::BroadcastInput,
            ToggleZoom => KeyAction::ToggleZoom,
            ToggleFullscreen => KeyAction::ToggleFullscreen,
            ToggleQuake => KeyAction::QuakeToggle,
            FontIncrease => KeyAction::FontIncrease,
            FontDecrease => KeyAction::FontDecrease,
            FontReset => KeyAction::FontReset,
            ToggleStatusBar => KeyAction::ToggleStatusBar,
            ToggleFold => KeyAction::ToggleFold,
            ToggleMinimap => KeyAction::ToggleMinimap,
            IncreaseOpacity => KeyAction::IncreaseOpacity,
            DecreaseOpacity => KeyAction::DecreaseOpacity,
            _ => return None,
        })
    }

    /// Optional right-aligned shortcut hint (dim), mirroring the menus. Looked
    /// up from the live keymap (via `chord_map`) instead of hardcoded, so it
    /// always matches the actual bound chord on the current platform — and any
    /// user override — rather than a stale Ctrl-based guess shown on macOS too.
    fn hint(self, chord_map: &std::collections::HashMap<KeyAction, String>) -> Option<String> {
        self.key_action().and_then(|a| chord_map.get(&a)).cloned()
    }
}

/// The owned snapshot the palette paint reads: `(query, caret, selection, rows,
/// selected)`, where each row is `(display_label, optional_shortcut_hint)` and
/// `caret`/`selection` are char offsets into `query` for the editable field.
pub(crate) type PaletteSnapshot = (
    String,
    usize,
    Option<(usize, usize)>,
    Vec<(String, Option<String>)>,
    usize,
);

/// One built registry row: a command + its resolved display label (category and
/// label pre-joined for matching) + hint.
pub(crate) struct PaletteEntry {
    pub cmd: PaletteCmd,
    /// `"Category  Label"` as drawn (also the fuzzy haystack).
    pub display: String,
    /// Lower-cased haystack for case-insensitive matching (cached).
    pub haystack: String,
    /// The live, platform-correct chord display string (e.g. "⌘⇧T" on macOS,
    /// "Ctrl+Shift+T" elsewhere), resolved from the keymap at registry-build
    /// time — see [`PaletteCmd::hint`].
    pub hint: Option<String>,
    /// Owned action data for dynamic entries: the command text for
    /// [`PaletteCmd::RunCommand`] or the directory for [`PaletteCmd::CdTo`].
    /// `None` for the static action/setting/theme rows.
    pub payload: Option<String>,
}

/// All state for an open palette. Owned by `App` as `Option<PaletteState>`.
pub(crate) struct PaletteState {
    /// The live query, as an editable model (caret, selection, word-jump,
    /// clipboard) shared with every other glassy text field via [`gui::TextEdit`].
    pub edit: gui::TextEdit,
    /// The full registry, built once on open (cheap; ~30 rows).
    pub all: Vec<PaletteEntry>,
    /// Indices into `all` that pass the current filter, best-first.
    pub filtered: Vec<usize>,
    /// Selected row within `filtered` (keyboard highlight).
    pub sel: usize,
}

impl PaletteState {
    /// The current query text (what the fuzzy filter + painter consume).
    pub fn query(&self) -> String {
        self.edit.text()
    }
}

impl App {
    /// Build the full action/setting registry. Driven from the same enums the
    /// menus + settings form use, so it can never drift out of sync: every
    /// MenuAction-equivalent and every inline-settable setting is represented,
    /// plus one row per theme (so the theme list stays auto-complete).
    fn palette_registry(&self) -> Vec<PaletteEntry> {
        use PaletteCmd::*;
        let mut cmds: Vec<PaletteCmd> = vec![
            NewTab,
            CloseTab,
            NextTab,
            PrevTab,
            SplitVertical,
            SplitHorizontal,
            ClosePane,
            ToggleBroadcastInput,
            ToggleZoom,
            RotatePanes,
            EqualizePanes,
            ToggleDimUnfocused,
            OpenSettings,
            OpenHelp,
            OpenSearch,
            Copy,
            Paste,
            ToggleFullscreen,
            FontIncrease,
            FontDecrease,
            FontReset,
            ToggleStatusBar,
            ToggleMinimap,
            TogglePaneHeaders,
            TogglePowerMode,
            BellOff,
            BellVisual,
            BellAudible,
            ScrollbackIncrease,
            ScrollbackDecrease,
            ToggleFold,
            NextTheme,
            PrevTheme,
            GenerateThemeFromWallpaper,
            // Opacity
            IncreaseOpacity,
            DecreaseOpacity,
            ToggleOpacity,
            // Effects
            ToggleCrtEffect,
            ToggleCursorTrail,
            // Remaining settings toggles
            ToggleCursorBlink,
            ToggleFollowSystem,
            ToggleLigatures,
            ToggleRestoreSession,
            ToggleCopyOnSelect,
            ToggleCommandBadges,
            // Scrollback
            SaveScrollbackToFile,
        ];
        // Opacity presets: 5 steps at 25 / 50 / 75 / 90 / 100%.
        // The step value maps to opacity as `v * 5 / 100` (v is stored as
        // "5% units": 5 → 25%, 10 → 50%, 15 → 75%, 18 → 90%, 20 → 100%).
        for v in [5u8, 10, 15, 18, 20] {
            cmds.push(SetOpacity(v));
        }
        // Only surface the quake toggle when the instance is actually in quake mode.
        if self.config.quake {
            cmds.push(ToggleQuake);
        }
        let theme_names = color::theme_names();
        for i in 0..theme_names.len() {
            cmds.push(SetTheme(i));
        }
        // Resolved once from the live keymap so hints show the actual bound
        // chord (⌘-based on macOS) instead of a hardcoded Ctrl-based guess.
        let chord_map = crate::config::keymap::action_chord_display_map(
            &self.config.keymap,
            crate::config::Platform::display_override(),
        );
        let mut entries: Vec<PaletteEntry> = cmds
            .into_iter()
            .map(|cmd| {
                let label = match cmd {
                    SetTheme(i) => format!("Set theme: {}", theme_names[i]),
                    other => other.label(),
                };
                let display = format!("{}  {}", cmd.category(), label);
                let haystack = display.to_lowercase();
                PaletteEntry {
                    cmd,
                    display,
                    haystack,
                    hint: cmd.hint(&chord_map),
                    payload: None,
                }
            })
            .collect();
        // Append the dynamic history sources. Newest-first so a `nt` query that
        // happens to also match a recent command surfaces the freshest one near
        // the top once the (small) action set is ranked alongside.
        for cmd in self.cmd_history.iter().rev() {
            let display = format!("History  {cmd}");
            let haystack = display.to_lowercase();
            entries.push(PaletteEntry {
                cmd: RunCommand,
                display,
                haystack,
                hint: None,
                payload: Some(cmd.clone()),
            });
        }
        for dir in self.cwd_history.iter().rev() {
            let shown = compact_home(dir);
            let display = format!("Cwd  {shown}");
            let haystack = display.to_lowercase();
            entries.push(PaletteEntry {
                cmd: CdTo,
                display,
                haystack,
                hint: None,
                payload: Some(dir.to_string_lossy().into_owned()),
            });
        }
        // Runtime profile switches: one entry per `[profile.NAME]` section.
        for name in crate::config::profile_names() {
            let display = format!("Profile  Switch to {name}");
            let haystack = display.to_lowercase();
            entries.push(PaletteEntry {
                cmd: SwitchProfile,
                display,
                haystack,
                hint: None,
                payload: Some(name),
            });
        }
        // One restore row per saved layout (only meaningful while split). Saving is
        // offered as a quick "Save layout: <auto-name>" row when the tab is split.
        for name in self.saved_layout_names() {
            let display = format!("Pane  Restore layout: {name}");
            let haystack = display.to_lowercase();
            entries.push(PaletteEntry {
                cmd: RestoreLayout,
                display,
                haystack,
                hint: None,
                payload: Some(name),
            });
        }
        if self.is_split() {
            let name = self.next_layout_name();
            let display = format!("Pane  Save layout: {name}");
            let haystack = display.to_lowercase();
            entries.push(PaletteEntry {
                cmd: SaveLayout,
                display,
                haystack,
                hint: None,
                payload: Some(name),
            });
        }
        entries
    }

    /// A fresh default name for the "Save layout" palette row: `layout-1`,
    /// `layout-2`, … skipping any already taken.
    fn next_layout_name(&self) -> String {
        (1..)
            .map(|n| format!("layout-{n}"))
            .find(|n| !self.named_layouts.contains_key(n))
            .unwrap_or_else(|| "layout".to_string())
    }

    /// Open the command palette (or no-op if already open). Builds the registry,
    /// resets the query, and shows the full list.
    pub(crate) fn open_palette(&mut self, event_loop: &ActiveEventLoop) {
        if self.palette.is_none() {
            let all = self.palette_registry();
            let filtered: Vec<usize> = (0..all.len()).collect();
            self.palette = Some(PaletteState {
                edit: gui::TextEdit::default(),
                all,
                filtered,
                sel: 0,
            });
        }
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Close the palette.
    pub(crate) fn close_palette(&mut self, event_loop: &ActiveEventLoop) {
        if self.palette.take().is_some() {
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
        }
    }

    /// Re-run the fuzzy filter against the current query, ranking matches best
    /// first and clamping the selection. With an empty query the whole registry
    /// is shown in declaration order.
    pub(crate) fn refilter_palette(&mut self) {
        let Some(p) = self.palette.as_mut() else {
            return;
        };
        let q = p.query().to_lowercase();
        if q.is_empty() {
            p.filtered = (0..p.all.len()).collect();
        } else {
            let mut scored: Vec<(i32, usize)> = p
                .all
                .iter()
                .enumerate()
                .filter_map(|(i, e)| fuzzy_score(&e.haystack, &q).map(|s| (s, i)))
                .collect();
            // Higher score first; ties keep registry order (stable sort on index).
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            p.filtered = scored.into_iter().map(|(_, i)| i).collect();
        }
        if p.sel >= p.filtered.len() {
            p.sel = p.filtered.len().saturating_sub(1);
        }
    }

    /// Handle a keypress while the palette is open. Returns `true` if consumed.
    pub(crate) fn handle_palette_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) -> bool {
        if self.palette.is_none() {
            return false;
        }
        // Up/Down move the result selection (not the caret); Enter activates the
        // selected row. These belong to the list, so intercept before the shared
        // text-field path consumes arrows as caret moves.
        match key {
            Key::Named(NamedKey::ArrowDown) => {
                if let Some(p) = self.palette.as_mut()
                    && !p.filtered.is_empty()
                {
                    p.sel = (p.sel + 1) % p.filtered.len();
                }
                self.mark_dirty(event_loop);
                return true;
            }
            Key::Named(NamedKey::ArrowUp) => {
                if let Some(p) = self.palette.as_mut()
                    && !p.filtered.is_empty()
                {
                    let n = p.filtered.len();
                    p.sel = (p.sel + n - 1) % n;
                }
                self.mark_dirty(event_loop);
                return true;
            }
            _ => {}
        }
        let ctrl = self.mods.control_key();
        let shift = self.mods.shift_key();
        let (named, text) = super::settings::key_to_text_parts(key);
        let action = gui::map_text_key(named.as_deref(), text.as_deref(), ctrl, shift);
        match action {
            // Esc closes; Enter activates the selected command.
            gui::TextInputAction::Cancel => {
                self.close_palette(event_loop);
                return true;
            }
            gui::TextInputAction::Submit => {
                self.palette_activate_sel(event_loop);
                return true;
            }
            gui::TextInputAction::None => return false,
            _ => {}
        }
        let paste_text = if matches!(action, gui::TextInputAction::Paste) {
            self.clipboard_text()
        } else {
            None
        };
        let Some(p) = self.palette.as_mut() else {
            return false;
        };
        let res = gui::apply_text_action(&mut p.edit, action, paste_text.as_deref());
        if res.changed {
            p.sel = 0;
        }
        match &res.clip {
            gui::ClipReq::Copy(s) | gui::ClipReq::Cut(s) => {
                let owned = s.clone();
                self.copy_text_to_clipboard(&owned);
            }
            gui::ClipReq::None | gui::ClipReq::Paste => {}
        }
        if res.changed {
            self.refilter_palette();
        }
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
        true
    }

    /// Execute the currently-selected palette row's command, then close the
    /// palette. A no-op (just closes) when the filtered list is empty.
    pub(crate) fn palette_activate_sel(&mut self, event_loop: &ActiveEventLoop) {
        let sel = self.palette.as_ref().map(|p| p.sel);
        if let Some(sel) = sel {
            self.palette_activate_index(sel, event_loop);
        }
    }

    /// Activate the palette row at filtered index `idx` (mouse click).
    pub(crate) fn palette_activate_index(&mut self, idx: usize, event_loop: &ActiveEventLoop) {
        let picked = self.palette.as_ref().and_then(|p| {
            p.filtered
                .get(idx)
                .and_then(|&i| p.all.get(i))
                .map(|e| (e.cmd, e.payload.clone()))
        });
        // Close first so re-entrant overlays (Settings/Help/Search) open cleanly.
        self.close_palette(event_loop);
        if let Some((cmd, payload)) = picked {
            self.run_palette_cmd_with(cmd, payload, event_loop);
        }
    }

    /// Map a [`PaletteCmd`] (plus its optional owned `payload` for the dynamic
    /// history/cwd rows) onto the existing App effect. Every static arm routes
    /// through the SAME method the menu / settings / keybinding path uses, so
    /// behaviour is identical no matter how an action is triggered.
    pub(crate) fn run_palette_cmd_with(
        &mut self,
        cmd: PaletteCmd,
        payload: Option<String>,
        event_loop: &ActiveEventLoop,
    ) {
        use PaletteCmd::*;
        match cmd {
            RunCommand => {
                if let Some(text) = payload {
                    self.palette_submit_line(&text, event_loop);
                }
            }
            CdTo => {
                if let Some(dir) = payload {
                    let line = format!("cd {}", shell_quote(&dir));
                    self.palette_submit_line(&line, event_loop);
                }
            }
            SwitchProfile => {
                if let Some(name) = payload {
                    self.switch_profile_by_name(&name);
                    self.mark_dirty(event_loop);
                }
            }
            NewTab => self.new_tab(event_loop),
            CloseTab => self.close_active_tab(event_loop),
            NextTab => self.cycle_tab(1, event_loop),
            PrevTab => self.cycle_tab(-1, event_loop),
            SplitVertical => self.split_pane(pane::Dir::Vertical, event_loop),
            SplitHorizontal => self.split_pane(pane::Dir::Horizontal, event_loop),
            ClosePane => self.close_pane(event_loop),
            ToggleBroadcastInput => self.toggle_broadcast_input(event_loop),
            ToggleZoom => self.toggle_zoom(event_loop),
            RotatePanes => self.rotate_panes(event_loop),
            EqualizePanes => self.equalize_panes(event_loop),
            ToggleDimUnfocused => {
                self.toggle_dim_unfocused();
                self.mark_dirty(event_loop);
            }
            SaveLayout => {
                if let Some(name) = payload {
                    self.save_layout(&name);
                }
            }
            RestoreLayout => {
                if let Some(name) = payload {
                    self.restore_layout(&name, event_loop);
                }
            }
            OpenSettings => {
                self.open_settings();
                // Palette rows are activated on a left RELEASE whose click edge /
                // gui_click_pos is the palette row — OUTSIDE the centered overlay
                // panel. Guard that opening release so it is not treated as a
                // click-outside dismiss of the overlay it just opened.
                self.overlay_opened_by_press = true;
                self.mark_dirty(event_loop);
            }
            OpenHelp => {
                self.help_open = true;
                self.force_full_redraw = true;
                // See OpenSettings: the activating release lands outside the help
                // panel; guard it so the overlay survives its own opening gesture.
                self.overlay_opened_by_press = true;
                self.mark_dirty(event_loop);
            }
            OpenSearch => self.open_search(event_loop),
            Copy => {
                self.copy_selection();
                self.mark_dirty(event_loop);
            }
            Paste => {
                self.paste_clipboard();
                self.mark_dirty(event_loop);
            }
            ToggleFullscreen => {
                if let Some(w) = self.window.as_ref() {
                    let fs = if w.fullscreen().is_some() {
                        None
                    } else {
                        Some(winit::window::Fullscreen::Borderless(None))
                    };
                    w.set_fullscreen(fs);
                }
            }
            ToggleQuake => {
                self.quake_apply(crate::ipc::IpcCommand::Toggle, event_loop);
            }
            FontIncrease => {
                self.resize_font(FontStep::Inc);
                self.mark_dirty(event_loop);
            }
            FontDecrease => {
                self.resize_font(FontStep::Dec);
                self.mark_dirty(event_loop);
            }
            FontReset => {
                self.resize_font(FontStep::Reset);
                self.mark_dirty(event_loop);
            }
            ToggleStatusBar => {
                self.toggle_status_bar();
                self.mark_dirty(event_loop);
            }
            ToggleMinimap => {
                self.toggle_minimap();
                self.mark_dirty(event_loop);
            }
            TogglePaneHeaders => {
                self.toggle_pane_headers();
                self.mark_dirty(event_loop);
            }
            TogglePowerMode => self.toggle_power_mode(event_loop),
            BellOff => {
                self.set_bell_index(0);
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            BellVisual => {
                self.set_bell_index(1);
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            BellAudible => {
                self.set_bell_index(2);
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            ScrollbackIncrease => {
                self.adjust_scrollback(1);
                self.mark_dirty(event_loop);
            }
            ScrollbackDecrease => {
                self.adjust_scrollback(-1);
                self.mark_dirty(event_loop);
            }
            NextTheme => {
                self.cycle_theme(1);
                self.mark_dirty(event_loop);
            }
            PrevTheme => {
                self.cycle_theme(-1);
                self.mark_dirty(event_loop);
            }
            ToggleFold => self.toggle_command_fold(event_loop),
            SetTheme(i) => {
                self.set_theme_by_idx(i);
                self.mark_dirty(event_loop);
            }
            GenerateThemeFromWallpaper => {
                self.generate_theme_from_wallpaper(event_loop);
            }
            // --- Opacity ---
            IncreaseOpacity => {
                let o = (self.config.opacity + 0.05).clamp(0.0, 1.0);
                self.apply_opacity(o, event_loop);
            }
            DecreaseOpacity => {
                let o = (self.config.opacity - 0.05).clamp(0.0, 1.0);
                self.apply_opacity(o, event_loop);
            }
            SetOpacity(v) => {
                let o = (v as f32 * 5.0 / 100.0).clamp(0.0, 1.0);
                self.apply_opacity(o, event_loop);
            }
            ToggleOpacity => {
                // Toggle between 1.0 and the last non-1.0 opacity.
                self.toggle_opacity(event_loop);
            }
            // --- Effects ---
            ToggleCrtEffect => {
                self.toggle_crt_effect();
                self.mark_dirty(event_loop);
            }
            ToggleCursorTrail => {
                self.toggle_cursor_trail();
                self.mark_dirty(event_loop);
            }
            // --- Additional setting toggles ---
            ToggleCursorBlink => {
                self.config.cursor_blink = !self.config.cursor_blink;
                self.settings_saved = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            ToggleFollowSystem => {
                self.config.follow_system = !self.config.follow_system;
                self.settings_saved = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            ToggleLigatures => {
                self.config.ligatures = !self.config.ligatures;
                if let Some(r) = self.renderer.as_mut() {
                    r.set_ligatures(self.config.ligatures);
                }
                self.settings_saved = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            ToggleRestoreSession => {
                self.config.restore_session = !self.config.restore_session;
                self.session_dirty = true;
                self.settings_saved = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            ToggleCopyOnSelect => {
                self.config.copy_on_select = !self.config.copy_on_select;
                self.settings_saved = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            ToggleCommandBadges => {
                self.config.command_badges = !self.config.command_badges;
                self.settings_saved = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            // --- Scrollback ---
            SaveScrollbackToFile => {
                self.save_scrollback_to_file(event_loop);
            }
        }
    }

    /// Send a line of text to the focused pane's shell and submit it (paste the
    /// text honoring bracketed paste, then send a carriage return). Used by the
    /// palette's history (`RunCommand`) and cwd (`CdTo`) rows. Scrolls the pane to
    /// the bottom first so the result is visible. A no-op with no focused pane.
    pub(crate) fn palette_submit_line(&mut self, line: &str, event_loop: &ActiveEventLoop) {
        if line.is_empty() {
            return;
        }
        let bracketed = self.term_mode().contains(TermMode::BRACKETED_PASTE);
        if let Some(pty) = self.pty.as_ref() {
            pty.term
                .lock()
                .scroll_display(alacritty_terminal::grid::Scroll::Bottom);
            // Paste sanitizes embedded control sequences; then a CR submits it.
            pty.paste(line, bracketed);
            pty.write(b"\r".to_vec());
        }
        self.mark_dirty(event_loop);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn palette_cmd_category_label_consistency() {
        // Every non-SetTheme PaletteCmd should have a non-empty label.
        use PaletteCmd::*;
        let cmds = [
            NewTab,
            CloseTab,
            NextTab,
            PrevTab,
            SplitVertical,
            SplitHorizontal,
            ClosePane,
            ToggleBroadcastInput,
            ToggleZoom,
            RotatePanes,
            EqualizePanes,
            ToggleDimUnfocused,
            OpenSettings,
            OpenHelp,
            OpenSearch,
            Copy,
            Paste,
            ToggleFullscreen,
            ToggleQuake,
            FontIncrease,
            FontDecrease,
            FontReset,
            ToggleStatusBar,
            ToggleMinimap,
            TogglePaneHeaders,
            TogglePowerMode,
            BellOff,
            BellVisual,
            BellAudible,
            ScrollbackIncrease,
            ScrollbackDecrease,
            ToggleFold,
            NextTheme,
            PrevTheme,
            GenerateThemeFromWallpaper,
            IncreaseOpacity,
            DecreaseOpacity,
            ToggleOpacity,
            ToggleCrtEffect,
            ToggleCursorTrail,
            ToggleCursorBlink,
            ToggleFollowSystem,
            ToggleLigatures,
            ToggleRestoreSession,
            ToggleCopyOnSelect,
            ToggleCommandBadges,
            SaveScrollbackToFile,
        ];
        for cmd in cmds {
            assert!(
                !cmd.label().is_empty(),
                "{cmd:?} should have a non-empty label"
            );
            assert!(
                !cmd.category().is_empty(),
                "{cmd:?} should have a non-empty category"
            );
        }
    }

    #[test]
    fn set_theme_label_is_empty() {
        // SetTheme defers label generation to the registry; the bare method returns "".
        assert_eq!(PaletteCmd::SetTheme(0).label(), "");
    }

    // ---- history/cwd source helpers ----------------------------------------

    #[test]
    fn run_command_and_cd_to_labels_deferred_to_registry() {
        // Both dynamic variants carry their label via the payload, not the method.
        assert_eq!(PaletteCmd::RunCommand.label(), "");
        assert_eq!(PaletteCmd::CdTo.label(), "");
        assert_eq!(PaletteCmd::RunCommand.category(), "History");
        assert_eq!(PaletteCmd::CdTo.category(), "Cwd");
    }
}
