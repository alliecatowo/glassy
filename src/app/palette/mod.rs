//! Command palette (Ctrl+Shift+P): a centered glass overlay with a query field
//! and a fuzzy-filtered list of EVERY action and inline-settable setting, driven
//! from one registry so it stays in lock-step with the menus + settings form.
//!
//! Idle stays at 0%: the palette paints only while open and repaints only on a
//! real change (a keystroke, an arrow, a hover-row change, a click). It schedules
//! no timer and never forces `Poll` — between interactions the overlay is static.

use super::*;

use fuzzy::fuzzy_score;
use history::{compact_home, shell_quote};

mod fuzzy;
mod history;
mod paint;

/// One palette command: a stable identifier the App maps to a concrete effect,
/// plus the display metadata (category + label) the registry builds it with.
///
/// The variants intentionally mirror the menu actions AND the settings form so a
/// single fuzzy list covers "do X" and "change setting Y" uniformly. Themes are
/// carried by index into [`color::THEME_NAMES`] so adding a theme there surfaces
/// it in the palette for free.
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
    BellOff,
    BellVisual,
    BellAudible,
    ScrollbackIncrease,
    ScrollbackDecrease,
    NextTheme,
    PrevTheme,
    /// Fold/unfold the output of the command block in view (OSC 133).
    ToggleFold,
    /// Set the theme at this index into [`color::THEME_NAMES`].
    SetTheme(usize),
    /// Generate a theme from the configured `wallpaper_theme` image path and apply it live.
    GenerateThemeFromWallpaper,
    // --- Dynamic history sources (payload carried by the PaletteEntry) ---
    /// Re-run a command from history: paste its text into the focused pane and
    /// submit it (append a newline). The command string lives in
    /// [`PaletteEntry::payload`].
    RunCommand,
    /// Change into a recent working directory: paste `cd <dir>` and submit. The
    /// directory string lives in [`PaletteEntry::payload`].
    CdTo,
}

impl PaletteCmd {
    /// Category prefix shown dim before the label (groups the list visually).
    fn category(self) -> &'static str {
        use PaletteCmd::*;
        match self {
            NewTab | CloseTab | NextTab | PrevTab => "Tab",
            SplitVertical | SplitHorizontal | ClosePane | ToggleBroadcastInput => "Pane",
            OpenSettings | OpenHelp | OpenSearch => "View",
            Copy | Paste => "Edit",
            ToggleFullscreen | ToggleQuake => "Window",
            FontIncrease | FontDecrease | FontReset => "Font",
            ToggleStatusBar | ToggleMinimap | TogglePaneHeaders | BellOff | BellVisual
            | BellAudible | ScrollbackIncrease | ScrollbackDecrease => "Setting",
            ToggleFold => "View",
            NextTheme | PrevTheme | SetTheme(_) | GenerateThemeFromWallpaper => "Theme",
            RunCommand => "History",
            CdTo => "Cwd",
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
            // History/cwd labels are filled in by the registry from the payload.
            RunCommand | CdTo => String::new(),
        }
    }

    /// Optional right-aligned shortcut hint (dim), mirroring the menus.
    fn hint(self) -> Option<&'static str> {
        use PaletteCmd::*;
        match self {
            NewTab => Some("Ctrl+Shift+T"),
            CloseTab => Some("Ctrl+Shift+W"),
            NextTab => Some("Ctrl+Tab"),
            PrevTab => Some("Ctrl+Shift+Tab"),
            SplitVertical => Some("Ctrl+Shift+E"),
            SplitHorizontal => Some("Ctrl+Shift+O"),
            OpenSettings => Some("Ctrl+,"),
            OpenHelp => Some("F1"),
            OpenSearch => Some("Ctrl+Shift+F"),
            Copy => Some("Ctrl+Shift+C"),
            Paste => Some("Ctrl+Shift+V"),
            ToggleBroadcastInput => Some("Ctrl+Shift+I"),
            ToggleFullscreen => Some("F11"),
            ToggleQuake => Some("F12"),
            FontIncrease => Some("Ctrl++"),
            FontDecrease => Some("Ctrl+-"),
            FontReset => Some("Ctrl+0"),
            ToggleStatusBar => Some("Ctrl+Shift+B"),
            ToggleFold => Some("Ctrl+Shift+Z"),
            ToggleMinimap => Some("Ctrl+Shift+M"),
            _ => None,
        }
    }
}

/// The owned snapshot the palette paint reads: `(query, caret, selection, rows,
/// selected)`, where each row is `(display_label, optional_shortcut_hint)` and
/// `caret`/`selection` are char offsets into `query` for the editable field.
pub(crate) type PaletteSnapshot = (
    String,
    usize,
    Option<(usize, usize)>,
    Vec<(String, Option<&'static str>)>,
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
    pub hint: Option<&'static str>,
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
            BellOff,
            BellVisual,
            BellAudible,
            ScrollbackIncrease,
            ScrollbackDecrease,
            ToggleFold,
            NextTheme,
            PrevTheme,
            GenerateThemeFromWallpaper,
        ];
        // Only surface the quake toggle when the instance is actually in quake mode.
        if self.config.quake {
            cmds.push(ToggleQuake);
        }
        for i in 0..color::THEME_NAMES.len() {
            cmds.push(SetTheme(i));
        }
        let mut entries: Vec<PaletteEntry> = cmds
            .into_iter()
            .map(|cmd| {
                let label = match cmd {
                    SetTheme(i) => format!("Set theme: {}", color::THEME_NAMES[i]),
                    other => other.label(),
                };
                let display = format!("{}  {}", cmd.category(), label);
                let haystack = display.to_lowercase();
                PaletteEntry {
                    cmd,
                    display,
                    haystack,
                    hint: cmd.hint(),
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
        entries
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
            NewTab => self.new_tab(event_loop),
            CloseTab => self.close_active_tab(event_loop),
            NextTab => self.cycle_tab(1, event_loop),
            PrevTab => self.cycle_tab(-1, event_loop),
            SplitVertical => self.split_pane(pane::Dir::Vertical, event_loop),
            SplitHorizontal => self.split_pane(pane::Dir::Horizontal, event_loop),
            ClosePane => self.close_pane(event_loop),
            ToggleBroadcastInput => self.toggle_broadcast_input(event_loop),
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
            BellOff,
            BellVisual,
            BellAudible,
            ScrollbackIncrease,
            ScrollbackDecrease,
            ToggleFold,
            NextTheme,
            PrevTheme,
            GenerateThemeFromWallpaper,
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
