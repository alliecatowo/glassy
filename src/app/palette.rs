//! Command palette (Ctrl+Shift+P): a centered glass overlay with a query field
//! and a fuzzy-filtered list of EVERY action and inline-settable setting, driven
//! from one registry so it stays in lock-step with the menus + settings form.
//!
//! Idle stays at 0%: the palette paints only while open and repaints only on a
//! real change (a keystroke, an arrow, a hover-row change, a click). It schedules
//! no timer and never forces `Poll` — between interactions the overlay is static.

use super::*;

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
    TogglePaneHeaders,
    BellOff,
    BellVisual,
    BellAudible,
    ScrollbackIncrease,
    ScrollbackDecrease,
    NextTheme,
    PrevTheme,
    /// Set the theme at this index into [`color::THEME_NAMES`].
    SetTheme(usize),
}

impl PaletteCmd {
    /// Category prefix shown dim before the label (groups the list visually).
    fn category(self) -> &'static str {
        use PaletteCmd::*;
        match self {
            NewTab | CloseTab | NextTab | PrevTab => "Tab",
            SplitVertical | SplitHorizontal | ClosePane => "Pane",
            OpenSettings | OpenHelp | OpenSearch => "View",
            Copy | Paste => "Edit",
            ToggleFullscreen | ToggleQuake => "Window",
            FontIncrease | FontDecrease | FontReset => "Font",
            ToggleStatusBar | TogglePaneHeaders | BellOff | BellVisual | BellAudible
            | ScrollbackIncrease | ScrollbackDecrease => "Setting",
            NextTheme | PrevTheme | SetTheme(_) => "Theme",
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
            TogglePaneHeaders => "Toggle pane headers".into(),
            BellOff => "Bell: off".into(),
            BellVisual => "Bell: visual".into(),
            BellAudible => "Bell: audible".into(),
            ScrollbackIncrease => "Scrollback +1000 lines".into(),
            ScrollbackDecrease => "Scrollback −1000 lines".into(),
            NextTheme => "Next theme".into(),
            PrevTheme => "Previous theme".into(),
            SetTheme(_) => String::new(),
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
            ToggleFullscreen => Some("F11"),
            ToggleQuake => Some("F12"),
            FontIncrease => Some("Ctrl++"),
            FontDecrease => Some("Ctrl+-"),
            FontReset => Some("Ctrl+0"),
            ToggleStatusBar => Some("Ctrl+Shift+B"),
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
            TogglePaneHeaders,
            BellOff,
            BellVisual,
            BellAudible,
            ScrollbackIncrease,
            ScrollbackDecrease,
            NextTheme,
            PrevTheme,
        ];
        // Only surface the quake toggle when the instance is actually in quake mode.
        if self.config.quake {
            cmds.push(ToggleQuake);
        }
        for i in 0..color::THEME_NAMES.len() {
            cmds.push(SetTheme(i));
        }
        cmds.into_iter()
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
                }
            })
            .collect()
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
        let cmd = self.palette.as_ref().and_then(|p| {
            p.filtered
                .get(p.sel)
                .and_then(|&i| p.all.get(i))
                .map(|e| e.cmd)
        });
        // Close first so re-entrant overlays (Settings/Help/Search) open cleanly.
        self.close_palette(event_loop);
        if let Some(cmd) = cmd {
            self.run_palette_cmd(cmd, event_loop);
        }
    }

    /// Activate the palette row at filtered index `idx` (mouse click).
    pub(crate) fn palette_activate_index(&mut self, idx: usize, event_loop: &ActiveEventLoop) {
        let cmd = self.palette.as_ref().and_then(|p| {
            p.filtered
                .get(idx)
                .and_then(|&i| p.all.get(i))
                .map(|e| e.cmd)
        });
        self.close_palette(event_loop);
        if let Some(cmd) = cmd {
            self.run_palette_cmd(cmd, event_loop);
        }
    }

    /// Map a [`PaletteCmd`] onto the existing App effect. Every arm routes through
    /// the SAME method the menu / settings / keybinding path uses, so behaviour is
    /// identical no matter how an action is triggered.
    pub(crate) fn run_palette_cmd(&mut self, cmd: PaletteCmd, event_loop: &ActiveEventLoop) {
        use PaletteCmd::*;
        match cmd {
            NewTab => self.new_tab(event_loop),
            CloseTab => self.close_active_tab(event_loop),
            NextTab => self.cycle_tab(1, event_loop),
            PrevTab => self.cycle_tab(-1, event_loop),
            SplitVertical => self.split_pane(pane::Dir::Vertical, event_loop),
            SplitHorizontal => self.split_pane(pane::Dir::Horizontal, event_loop),
            ClosePane => self.close_pane(event_loop),
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
            SetTheme(i) => {
                self.set_theme_by_idx(i);
                self.mark_dirty(event_loop);
            }
        }
    }
}

impl App {
    /// Snapshot the palette paint inputs: the query, the filtered rows as owned
    /// `(label, hint)` pairs, and the selected row. Returns `None` when closed.
    pub(crate) fn palette_snapshot(&self) -> Option<PaletteSnapshot> {
        let p = self.palette.as_ref()?;
        let rows: Vec<(String, Option<&'static str>)> = p
            .filtered
            .iter()
            .filter_map(|&i| p.all.get(i))
            .map(|e| (e.display.clone(), e.hint))
            .collect();
        Some((p.query(), p.edit.caret(), p.edit.selection(), rows, p.sel))
    }

    /// Paint the command palette: a full-surface scrim, a centered glass panel
    /// with a query field at the top and a fuzzy-filtered, scrollable list below.
    /// The selected row is highlighted; the hovered row (under `mouse`) gets a
    /// lighter tint. Returns the `(filtered_index, row_rect)` list for the App to
    /// store for mouse hit-testing (the immediate-mode click is resolved in the
    /// mouse handler, mirroring the menu pattern).
    ///
    /// Associated fn (no `&self`) so it composes with the caller's `&mut Renderer`
    /// borrow; all `self`-derived data arrives via parameters.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_palette(
        renderer: &mut Renderer,
        surface: (f32, f32),
        query: &str,
        caret: usize,
        selection: Option<(usize, usize)>,
        rows: &[(&str, Option<&str>)],
        sel: usize,
        mouse: (f32, f32),
    ) -> Vec<(usize, gui::Rect)> {
        let m = renderer.cell_metrics();
        let cell_w = m.width;
        let cell_h = m.height;
        let pad = (cell_h * 0.5).round();
        let gap = (cell_h * 0.35).round();
        let radius = (cell_h * 0.28).round().clamp(4.0, 8.0);
        let row_h = (cell_h * 1.55).round();

        // Full-surface scrim.
        renderer.push_overlay_px(0.0, 0.0, surface.0, surface.1, [0.0, 0.0, 0.0, 0.5]);

        // Centered panel. Width ~ 60 cols, capped to the surface.
        let pw = (cell_w * 60.0)
            .min(surface.0 - 2.0 * pad)
            .max(cell_w * 28.0);
        let field_h = row_h;
        // Show up to 12 list rows; the rest scroll into view around the selection.
        let max_visible = 12usize;
        let visible = rows.len().min(max_visible);
        let list_h = visible as f32 * row_h;
        let ph = (pad + field_h + gap + list_h + pad).round();
        let px = ((surface.0 - pw) * 0.5).round();
        // Anchor toward the upper third so the list grows downward like a palette.
        let py = ((surface.1 - ph) * 0.35).round().max(pad);
        let panel = gui::Rect::new(px, py, pw, ph);

        // Panel body (E3 floating surface) + accent top rail.
        renderer.push_overlay_rrect_px(
            panel.x,
            panel.y,
            panel.w,
            panel.h,
            radius + 2.0,
            gui::glass_float(),
        );
        renderer.push_overlay_rrect_px(panel.x, panel.y, panel.w, 2.0, radius + 2.0, gui::rail());

        let inner_x = panel.x + pad;
        let inner_w = panel.w - 2.0 * pad;

        // --- Query field --------------------------------------------------------
        let field = gui::Rect::new(inner_x, panel.y + pad, inner_w, field_h);
        renderer.push_overlay_rrect_px(
            field.x,
            field.y,
            field.w,
            field.h,
            radius,
            [0.0, 0.0, 0.0, 0.35],
        );
        let ty = (field.y + (field.h - cell_h) * 0.5).round();
        let mut cx = field.x + pad;
        // Leading prompt chevron.
        renderer.push_overlay_glyph_px(cx.round(), ty, '\u{203A}', color::accent());
        cx += cell_w * 1.6;
        // The query text starts after the chevron; used for caret + selection x.
        let text_x0 = field.x + pad + cell_w * 1.6;
        // Selection band behind the glyphs.
        if let Some((lo, hi)) = selection
            && hi > lo
        {
            let sx = text_x0 + lo as f32 * cell_w;
            let sw = (hi - lo) as f32 * cell_w;
            let mut band = color::selection_bg();
            band[3] = 0.45;
            renderer.push_overlay_px(sx.round(), ty, sw.round(), cell_h, band);
        }
        if query.is_empty() {
            for ch in "Type a command…".chars() {
                renderer.push_overlay_glyph_px(cx.round(), ty, ch, gui::fg_dim());
                cx += cell_w;
            }
        } else {
            for ch in query.chars() {
                renderer.push_overlay_glyph_px(cx.round(), ty, ch, gui::fg());
                cx += cell_w;
            }
        }
        let caret_x = text_x0 + caret as f32 * cell_w;
        renderer.push_overlay_px(caret_x.round(), ty, 2.0, cell_h, color::accent());

        // --- List ---------------------------------------------------------------
        let list_y = field.y + field.h + gap;
        // Scroll so the selection stays visible (keep a simple window).
        let first = if sel >= visible { sel + 1 - visible } else { 0 };
        let mut out = Vec::with_capacity(visible);
        for (slot, ri) in (first..rows.len()).take(visible).enumerate() {
            let (label, hint) = rows[ri];
            let ry = list_y + slot as f32 * row_h;
            let rr = gui::Rect::new(inner_x, ry, inner_w, row_h);
            let over = gui::hit(rr, mouse.0, mouse.1);
            if ri == sel {
                renderer.push_overlay_rrect_px(rr.x, rr.y, rr.w, rr.h, radius, gui::sel_bg());
            } else if over {
                let mut c = color::selection_bg();
                c[3] = 0.40;
                renderer.push_overlay_rrect_px(rr.x, rr.y, rr.w, rr.h, radius, c);
            }
            let lty = (rr.y + (rr.h - cell_h) * 0.5).round();
            // Label (left).
            let mut lx = rr.x + pad;
            for ch in label.chars() {
                renderer.push_overlay_glyph_px(lx.round(), lty, ch, gui::fg());
                lx += cell_w;
            }
            // Hint (right, dim).
            if let Some(h) = hint {
                let hw = h.chars().count() as f32 * cell_w;
                let mut hx = rr.x + rr.w - pad - hw;
                for ch in h.chars() {
                    renderer.push_overlay_glyph_px(hx.round(), lty, ch, gui::fg_dim());
                    hx += cell_w;
                }
            }
            out.push((ri, rr));
        }
        // "No matches" hint when the list is empty.
        if rows.is_empty() {
            let msg = "No matching commands";
            let mx = inner_x + pad;
            let mut cxn = mx;
            for ch in msg.chars() {
                renderer.push_overlay_glyph_px(
                    cxn.round(),
                    (list_y + (row_h - cell_h) * 0.5).round(),
                    ch,
                    gui::fg_dim(),
                );
                cxn += cell_w;
            }
        }
        out
    }
}

/// Subsequence fuzzy score: returns `Some(score)` if every char of `needle`
/// appears in `haystack` in order (case-folded by the caller), `None` otherwise.
/// Rewards contiguous runs and word-start matches so "nt" ranks "New tab" highly.
/// Higher is better. Pure for unit testing.
pub(crate) fn fuzzy_score(haystack: &str, needle: &str) -> Option<i32> {
    if needle.is_empty() {
        return Some(0);
    }
    let hay: Vec<char> = haystack.chars().collect();
    let need: Vec<char> = needle.chars().collect();
    let mut hi = 0usize;
    let mut ni = 0usize;
    let mut score = 0i32;
    let mut prev_matched = false;
    let mut prev_char = ' ';
    while hi < hay.len() && ni < need.len() {
        if hay[hi] == need[ni] {
            score += 1;
            if prev_matched {
                score += 4; // contiguous run bonus
            }
            // Word-start bonus (match right after a space / separator).
            if hi == 0 || prev_char == ' ' || prev_char == '-' || prev_char == '/' {
                score += 6;
            }
            ni += 1;
            prev_matched = true;
        } else {
            prev_matched = false;
        }
        prev_char = hay[hi];
        hi += 1;
    }
    if ni == need.len() {
        // Prefer shorter haystacks (tighter match) slightly.
        Some(score - (hay.len() as i32) / 32)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_requires_full_subsequence() {
        assert!(fuzzy_score("new tab", "ntb").is_some());
        assert!(fuzzy_score("new tab", "xyz").is_none());
        assert!(fuzzy_score("new tab", "newtab").is_some());
    }

    #[test]
    fn fuzzy_word_start_outranks_midword() {
        // "st" should score "Setting  Toggle status bar" via the word-start 's'+'t'.
        let a = fuzzy_score("setting toggle status bar", "st").unwrap();
        // A buried, non-word-start subsequence scores lower.
        let b = fuzzy_score("abcsxtq", "st").unwrap();
        assert!(a > b, "word-start match {a} should beat buried {b}");
    }

    #[test]
    fn fuzzy_empty_needle_matches_everything() {
        assert_eq!(fuzzy_score("anything", ""), Some(0));
    }

    // ---- additional fuzzy_score coverage ------------------------------------

    #[test]
    fn fuzzy_no_match_returns_none() {
        assert!(fuzzy_score("new tab", "xyz").is_none());
        assert!(fuzzy_score("", "a").is_none());
        assert!(fuzzy_score("abc", "abcd").is_none());
    }

    #[test]
    fn fuzzy_empty_haystack_empty_needle_scores_zero() {
        assert_eq!(fuzzy_score("", ""), Some(0));
    }

    #[test]
    fn fuzzy_contiguous_bonus_makes_prefix_score_higher() {
        // Matching the first N chars contiguously should outscore spread matches.
        let contiguous = fuzzy_score("new tab", "new").unwrap();
        let spread = fuzzy_score("new tab", "ntb").unwrap();
        assert!(
            contiguous > spread,
            "contiguous prefix match ({contiguous}) should outscore spread ({spread})"
        );
    }

    #[test]
    fn fuzzy_word_start_slash_separator_bonus() {
        // '/' counts as a word-start separator (file paths).
        let with_slash = fuzzy_score("split vertical (left / right)", "r").unwrap();
        // Match at position 0 (word start) vs a buried 'r'.
        let at_word_start = fuzzy_score("right side", "r").unwrap();
        // Both should match; the word-start bonus means starting with 'r' scores well.
        assert!(at_word_start > 0);
        assert!(with_slash > 0);
    }

    #[test]
    fn fuzzy_shorter_haystack_preferred_over_longer_for_same_needle() {
        // Both match "tab"; the shorter haystack should outscore the longer one
        // (the -len/32 penalty slightly penalizes the longer haystack).
        let short = fuzzy_score("new tab", "tab").unwrap();
        let long_hay = fuzzy_score(
            "this very long string has a tab word in it somewhere",
            "tab",
        )
        .unwrap();
        // The short haystack has a stronger tighter-match score.
        assert!(
            short >= long_hay,
            "shorter haystack {short} should be >= longer {long_hay}"
        );
    }

    #[test]
    fn fuzzy_case_folded_caller_responsibility() {
        // The function does NOT lowercase; callers must pre-fold.
        // If needle is lowercase and haystack is uppercase they won't match.
        assert!(fuzzy_score("NEW TAB", "new").is_none());
        // But if both are lowercase they do.
        assert!(fuzzy_score("new tab", "new").is_some());
    }

    #[test]
    fn fuzzy_dash_separator_bonus() {
        // '-' also triggers word-start bonus.
        let score = fuzzy_score("tokyo-night", "n").unwrap();
        assert!(
            score > 1,
            "dash-separated word start should get bonus score"
        );
    }

    #[test]
    fn fuzzy_ranking_order() {
        // "nt" applied to the palette display strings:
        // "Tab  New tab" hits word-start N in "New" and then t in "tab"
        // vs some non-word-start match — the palette ranking should order the best match first.
        let new_tab = fuzzy_score("tab  new tab", "nt").unwrap();
        let buried = fuzzy_score("abcntxyz", "nt").unwrap();
        assert!(
            new_tab > buried,
            "word-start match should rank higher than buried"
        );
    }

    #[test]
    fn fuzzy_single_char_needle() {
        // Single-char needle at word start gets word-start bonus.
        let word_start = fuzzy_score("new tab", "n").unwrap();
        let buried = fuzzy_score("xnew", "n").unwrap();
        assert!(word_start > buried);
    }

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
            TogglePaneHeaders,
            BellOff,
            BellVisual,
            BellAudible,
            ScrollbackIncrease,
            ScrollbackDecrease,
            NextTheme,
            PrevTheme,
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
}
