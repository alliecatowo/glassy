//! Top-level constants, free helper functions, and type aliases used across
//! the app submodules.

use super::*;

/// Lines of scrollback to move per wheel notch when reporting to a TUI or
/// scrolling glassy's own scrollback buffer.
pub(crate) const WHEEL_LINES: i32 = 3;

/// Legacy cell-row count, retained only by the not-yet-pixelized modal/menu draw
/// helpers (waves 5/6) for their full-screen sizing math. The terminal grid no
/// longer reserves a cell row for the strip — it is inset in PIXELS by the GUI
/// tab bar (`tab_bar_h`) via `Renderer::set_grid_origin_y`.
pub(crate) const TAB_STRIP_ROWS: usize = 1;

/// Tab-bar height in physical px, derived from the cell height so the chrome
/// scales with the font (and DPI) exactly like the cell metrics. The bar holds a
/// row of real tab shapes whose active member connects to the content surface.
pub(crate) fn tab_bar_h(cell_h: f32) -> f32 {
    (cell_h * 1.7).round().max(28.0)
}

/// Top-corner radius of a tab chip (px).
pub(crate) const TAB_RADIUS: f32 = 5.0;
/// Minimum / maximum tab width in px (multi-tab mode).
pub(crate) const TAB_MIN_W: f32 = 120.0;
pub(crate) const TAB_MAX_W: f32 = 220.0;
/// Gap between adjacent tab chips (px).
pub(crate) const TAB_GAP: f32 = 2.0;
/// Horizontal inner padding of a tab chip (px).
pub(crate) const TAB_PAD_X: f32 = 10.0;
/// Close-button hit box inside a tab (px, square).
pub(crate) const CLOSE_BOX: f32 = 16.0;
/// Square icon-button size for +/#/?/* controls (px).
pub(crate) const CTRL_BTN: f32 = 28.0;

/// Width of the borderless-window resize border zone (px). A left press within
/// this many pixels of a window edge/corner starts an OS-driven resize drag
/// (glassy owns the edge once the native decorations are off — see
/// [`resize_edge_at`]). Kept small so it doesn't steal ordinary content clicks.
pub(crate) const RESIZE_BORDER: f32 = 6.0;

/// Corner radius for tab-bar icon buttons, derived from the cell height like the
/// GUI metric scale so it tracks the font/DPI.
pub(crate) fn gui_radius(cell_h: f32) -> f32 {
    (cell_h * 0.28).round().clamp(4.0, 8.0)
}

/// Status-bar height in physical px. Fixed at 22 px (one cell-height equivalent
/// at typical DPI). Drawn at the bottom of the window; `content_area()` and
/// `grid_for()` subtract it so panes tile only between the tab bar and this bar.
pub(crate) const STATUS_BAR_H: f32 = 22.0;

/// What a wheel notch should do, given the terminal's current mode. Pure so it
/// can be unit-tested without a window or PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WheelAction {
    /// Report the wheel to the application as a mouse event (button 64/65).
    Report,
    /// Emit arrow keys (alt-screen apps: pagers, vim, bat).
    Arrows,
    /// Scroll glassy's own scrollback buffer.
    Scrollback,
}

/// Decide how the mouse wheel behaves for the given mode: applications that
/// requested mouse reporting get wheel events; other alt-screen apps get arrow
/// keys (xterm alternateScroll default); the normal screen scrolls scrollback.
pub(crate) fn wheel_action(mode: TermMode) -> WheelAction {
    if mode.intersects(TermMode::MOUSE_MODE) {
        WheelAction::Report
    } else if mode.contains(TermMode::ALT_SCREEN) {
        WheelAction::Arrows
    } else {
        WheelAction::Scrollback
    }
}

/// Pixel size to draw an inline image at, from the kitty `c=`/`r=` display
/// request (`cols`/`rows`, 0 = unset), the image's native size, and the cell
/// box. Both set -> exact cell box; one set -> scale the other preserving the
/// image aspect ratio; neither -> native pixels. Pure for unit testing.
pub(crate) fn image_dst_size(
    cols: u32,
    rows: u32,
    img_w: u32,
    img_h: u32,
    cell_w: f32,
    cell_h: f32,
) -> (f32, f32) {
    let aspect = if img_h > 0 {
        img_w as f32 / img_h as f32
    } else {
        1.0
    };
    match (cols, rows) {
        (0, 0) => (img_w as f32, img_h as f32),
        (c, 0) => {
            let w = c as f32 * cell_w;
            (w, w / aspect)
        }
        (0, r) => {
            let h = r as f32 * cell_h;
            (h * aspect, h)
        }
        (c, r) => (c as f32 * cell_w, r as f32 * cell_h),
    }
}

/// Trim a header/tab label to at most `max` chars, keeping the *tail* (the cwd or
/// git-branch end is the informative part) with a leading ellipsis when cut.
/// Empty input becomes "shell". Pure for unit testing.
pub(crate) fn fit_label(t: &str, max: usize) -> String {
    let t = t.trim();
    let base = if t.is_empty() { "shell" } else { t };
    let chars: Vec<char> = base.chars().collect();
    if chars.len() <= max {
        return base.to_string();
    }
    if max <= 1 {
        return "…".to_string();
    }
    let tail: String = chars[chars.len() - (max - 1)..].iter().collect();
    format!("…{tail}")
}

/// Reduce an OSC window title to printable ASCII only, so the native (CSD)
/// titlebar font — which we do not control — can render every character it ever
/// receives, making "tofu" boxes structurally impossible. Non-ASCII-graphic
/// chars (CJK, emoji, Nerd-Font icons, dingbats, combining marks) are dropped;
/// runs of whitespace collapse to a single space; an empty result falls back to
/// "glassy". The full rich/Unicode title is rendered in OUR tab bar instead
/// (through the glyph atlas, which renders all of them). Pure for unit testing.
pub(crate) fn os_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut last_space = false;
    for c in title.chars() {
        if c.is_ascii_graphic() {
            out.push(c);
            last_space = false;
        } else if c == ' ' || c == '\t' {
            if !last_space && !out.is_empty() {
                out.push(' ');
            }
            last_space = true;
        }
        // everything else (non-ASCII, control) is dropped
    }
    let trimmed = out.trim_end();
    if trimmed.is_empty() {
        "Glassy".to_string()
    } else {
        trimmed.to_string()
    }
}

/// An interactive item in the real GUI tab bar. Window controls (min/max/close)
/// live in the native bar, not here. `Tab`/`TabClose` carry the tab's *stable
/// position*.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub(crate) enum StripItem {
    /// A tab chip body at stable display position `pos` (click = activate).
    Tab(usize),
    /// A tab's ✕ close affordance at stable position `pos`.
    TabClose(usize),
    NewTab,
    Help,
    Settings,
    Menu,
    /// Borderless-window controls painted in the top-right chrome on non-macOS
    /// (macOS keeps its native traffic lights). Only laid out when glassy owns the
    /// window edge — see [`right_control_items`].
    WinMinimize,
    WinMaximize,
    WinClose,
}

/// One placed tab-bar item with its pixel rect. The label is carried for the tab
/// body (it is what gets drawn / measured); control buttons carry an empty label.
#[derive(Clone, Debug)]
pub(crate) struct StripSeg {
    pub(crate) item: StripItem,
    pub(crate) label: String,
    pub(crate) rect: gui::Rect,
}

/// The tab-bar item containing pixel point `(px, py)`, if any. Close boxes are
/// tested before their parent tab body (they are pushed after, so iterate in
/// reverse to let the smaller embedded box win). Pure for unit testing.
pub(crate) fn strip_item_at(segs: &[StripSeg], px: f32, py: f32) -> Option<StripItem> {
    segs.iter()
        .rev()
        .find(|s| gui::hit(s.rect, px, py))
        .map(|s| s.item)
}

/// Move the element at index `from` to index `to`, shifting the rest. Used to
/// reorder tabs by dragging. Pure for unit testing.
pub(crate) fn move_in_order<T>(v: &mut Vec<T>, from: usize, to: usize) {
    if from < v.len() && to < v.len() && from != to {
        let item = v.remove(from);
        v.insert(to, item);
    }
}

/// Display path of the config file for the settings overlay.
pub(crate) fn config_display_path() -> String {
    crate::config::path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.config/glassy/glassy.conf".to_string())
}

/// Render `font_symbol_map` entries back to the `RANGE:Family[, RANGE:Family…]`
/// text a user would type, for the Terminal section's editable field. Inverse
/// of [`crate::config::parse::parse_symbol_map`] (round-trips through it).
pub(crate) fn symbol_map_display(entries: &[crate::config::parse::SymbolMapEntry]) -> String {
    entries
        .iter()
        .map(|e| {
            if e.start == e.end {
                format!("U+{:04X}:{}", e.start, e.family)
            } else {
                format!("U+{:04X}-U+{:04X}:{}", e.start, e.end, e.family)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Render `status_bar_segments` back to its space-joined display text for the
/// Advanced section's editable field. `None` (built-in default set) displays
/// as empty, matching `apply_kv`'s "empty clears the override" contract.
/// Inverse of [`crate::config::parse::parse_status_bar_segments`].
pub(crate) fn status_bar_segments_display(segs: Option<&[StatusBarSegment]>) -> String {
    segs.map(|segs| segs.iter().map(|s| s.token()).collect::<Vec<_>>().join(" "))
        .unwrap_or_default()
}

/// Render the resolved `shell` config value for the Terminal section's
/// read-only info row. `alacritty_terminal::tty::Shell`'s `program`/`args`
/// fields are `pub(crate)` to that crate (not visible here), so this falls
/// back to its derived `Debug` output — still a faithful "resolved value"
/// even though it isn't as clean as hand-formatted `program arg1 arg2`.
pub(crate) fn shell_display(shell: &Option<alacritty_terminal::tty::Shell>) -> String {
    match shell {
        Some(s) => format!("{s:?}"),
        None => "(default shell)".to_string(),
    }
}

/// Lighten an RGB color toward white by `amount`, keeping alpha. Used for the
/// raised help-panel surface.
/// Percent-encode a filesystem path for embedding in a `file://` URI. Keeps the
/// unreserved set (RFC 3986) plus `/` (path separator) verbatim; everything else
/// (spaces, `#`, `?`, non-ASCII bytes, …) is `%XX`-escaped so the URI can't be
/// truncated or reinterpreted by the URL handler.
pub(crate) fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~');
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(
                char::from_digit((b >> 4) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
            out.push(
                char::from_digit((b & 0xf) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
        }
    }
    out
}

/// Actions available in the # hamburger dropdown and the right-click context
/// menu. Kept as a single enum so the hit-test and keyboard dispatch share one
/// definition across both menus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuAction {
    Copy,
    Paste,
    SelectAll,
    ClearScrollback,
    Search,
    SplitRight,
    SplitDown,
    NewTab,
    Settings,
    Help,
    CloseTab,
}

impl MenuAction {
    /// The fixed set shown by the ≡ hamburger dropdown. Settings (⚙) and Help (?)
    /// are NOT listed here — they have dedicated quick-access strip icons, so
    /// duplicating them in the hamburger would give two independent UI paths to the
    /// same overlay. `PaneHeaders` is likewise omitted: it is a Settings-form (and
    /// command-palette) toggle, not a top-level menu action. The right-click context
    /// menu uses a separately-built `Vec<MenuAction>` (see `context_menu_items`).
    pub(crate) const ALL: &'static [MenuAction] = &[MenuAction::NewTab, MenuAction::CloseTab];

    pub(crate) fn label(self) -> &'static str {
        match self {
            MenuAction::Copy => "Copy",
            MenuAction::Paste => "Paste",
            MenuAction::SelectAll => "Select all",
            MenuAction::ClearScrollback => "Clear scrollback",
            MenuAction::Search => "Search",
            MenuAction::SplitRight => "Split right",
            MenuAction::SplitDown => "Split down",
            MenuAction::NewTab => "New tab",
            MenuAction::Settings => "Settings",
            MenuAction::Help => "Help / keys",
            MenuAction::CloseTab => "Close tab",
        }
    }

    /// Left-column icon glyph for the real GUI menu (§3.6).
    pub(crate) fn icon(self) -> char {
        match self {
            MenuAction::Copy => '◻',
            MenuAction::Paste => '□',
            MenuAction::SelectAll => '◼',
            MenuAction::ClearScrollback => '~',
            MenuAction::Search => '/',
            MenuAction::SplitRight => '|',
            MenuAction::SplitDown => '-',
            MenuAction::NewTab => '+',
            MenuAction::Settings => '*',
            MenuAction::Help => '?',
            MenuAction::CloseTab => '✕',
        }
    }

    /// Right-aligned shortcut hint for the real GUI menu (§3.6). `None` for
    /// actions that have no keybinding shown in the menu.
    pub(crate) fn shortcut(self) -> Option<&'static str> {
        match self {
            MenuAction::Copy => Some("Ctrl+Shift+C"),
            MenuAction::Paste => Some("Ctrl+Shift+V"),
            MenuAction::SelectAll => Some("Ctrl+Shift+A"),
            MenuAction::ClearScrollback => None,
            MenuAction::Search => Some("Ctrl+Shift+F"),
            MenuAction::SplitRight => Some("Ctrl+Shift+E"),
            MenuAction::SplitDown => Some("Ctrl+Shift+O"),
            MenuAction::NewTab => Some("Ctrl+Shift+T"),
            MenuAction::Settings => Some("Ctrl+,"),
            MenuAction::Help => Some("F1"),
            MenuAction::CloseTab => Some("Ctrl+Shift+W"),
        }
    }

    /// Visual group id used by [`actions_to_entries`] to place separators: a
    /// separator is drawn between any two consecutive items whose groups differ.
    /// 0 = clipboard, 1 = buffer/find, 2 = layout, 3 = app, 4 = destructive.
    fn group(self) -> u8 {
        match self {
            MenuAction::Copy | MenuAction::Paste | MenuAction::SelectAll => 0,
            MenuAction::ClearScrollback | MenuAction::Search => 1,
            MenuAction::SplitRight | MenuAction::SplitDown | MenuAction::NewTab => 2,
            MenuAction::Settings | MenuAction::Help => 3,
            MenuAction::CloseTab => 4,
        }
    }
}

/// Build a `Vec<MenuEntry>` for the real GUI menu from a flat list of
/// `MenuAction`s. A separator is inserted wherever two consecutive actions fall in
/// different visual groups (see [`MenuAction::group`]) — e.g. between the clipboard
/// and navigation groups in the context menu, or between NewTab (layout) and
/// CloseTab (destructive) in the hamburger. Pure for testing.
pub(crate) fn actions_to_entries(
    actions: &[MenuAction],
    has_selection: bool,
) -> Vec<gui::MenuEntry<'static>> {
    let mut v: Vec<gui::MenuEntry<'static>> = Vec::with_capacity(actions.len() + 4);
    // Each action belongs to a visual group; a separator is drawn whenever the
    // group changes between two consecutive items. Keeping the grouping on the
    // action itself (rather than pairwise prev→cur rules) means new actions slot
    // into the right group automatically in both the context menu and hamburger.
    let mut prev_group: Option<u8> = None;
    for &a in actions {
        let group = a.group();
        if prev_group.is_some_and(|g| g != group) {
            v.push(gui::MenuEntry::Separator);
        }
        let enabled = match a {
            MenuAction::Copy => has_selection,
            _ => true,
        };
        v.push(gui::MenuEntry::Item {
            icon: a.icon(),
            label: a.label(),
            hint: a.shortcut(),
            enabled,
        });
        prev_group = Some(group);
    }
    v
}

/// Which mouse-button id to report for a pointer-motion event, or `None` to stay
/// silent. `held` is the currently pressed button (0/1/2) or `None`. Mirrors
/// xterm: any-motion mode (1003) reports even with no button (id 3); button-only
/// motion (1002) reports just while a button is held; click-only (1000) never
/// reports motion. Pure for unit testing.
pub(crate) fn motion_button(mode: TermMode, held: Option<u8>) -> Option<u8> {
    match held {
        Some(b) if mode.contains(TermMode::MOUSE_DRAG) || mode.contains(TermMode::MOUSE_MOTION) => {
            Some(b)
        }
        None if mode.contains(TermMode::MOUSE_MOTION) => Some(3),
        _ => None,
    }
}

/// Cursor blink half-period: the on/off phase length. ~530ms matches the de-facto
/// terminal cadence (and the GTK/VTE default).
pub(crate) const BLINK_INTERVAL: Duration = Duration::from_millis(530);

/// Tab "busy" spinner. A session is BUSY while it is actively producing output;
/// each PTY wakeup re-arms a `BUSY_LINGER` deadline, and the chip spins until that
/// elapses with no further output (mirroring the bell-flash deadline). While any
/// tab is busy we advance one `SPINNER_FRAMES` glyph every `SPINNER_INTERVAL` and
/// schedule a finite wakeup for it; once nothing is busy we return to `Wait`.
pub(crate) const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
pub(crate) const SPINNER_INTERVAL: Duration = Duration::from_millis(100);
pub(crate) const BUSY_LINGER: Duration = Duration::from_millis(600);

// --- Grapheme-cluster reconstruction across grid cells ----------------------
// A user-perceived character (extended grapheme cluster) can span several grid
// cells: a base emoji plus a ZWJ-joined emoji (flags, family, profession), a
// skin-tone modifier, a regional-indicator flag pair, or a variation selector.
// alacritty attaches zero-width code points to one cell but places *wide* joined
// emoji and modifiers in their own cells, so we re-stitch them here before
// shaping, otherwise compound emoji render as their separate components.

pub(crate) fn is_zwj(c: char) -> bool {
    c == '\u{200D}'
}
pub(crate) fn is_variation_selector(c: char) -> bool {
    c == '\u{FE0E}' || c == '\u{FE0F}'
}
pub(crate) fn is_emoji_modifier(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}
pub(crate) fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// Number of `cells` entries occupied by the cell unit at `start`: the cell plus
/// a following wide-character spacer, if any.
pub(crate) fn unit_len(cells: &[Indexed<&Cell>], start: usize) -> usize {
    let wide = cells[start].cell.flags.contains(Flags::WIDE_CHAR);
    let has_spacer = cells
        .get(start + 1)
        .is_some_and(|c| c.cell.flags.contains(Flags::WIDE_CHAR_SPACER));
    if wide && has_spacer { 2 } else { 1 }
}

/// Reconstruct the extended grapheme cluster anchored at cell `start`, greedily
/// merging following cells on the same `line` that continue it (trailing ZWJ joins
/// anything; a leading emoji modifier / variation selector / second regional
/// indicator also joins). Returns the code points to append after the base cell's
/// char and the number of `cells` entries the whole cluster consumed (>= 1).
pub(crate) fn build_grapheme(
    cells: &[Indexed<&Cell>],
    start: usize,
    line: i32,
) -> (Vec<char>, usize) {
    let base = cells[start].cell;
    let mut combiners: Vec<char> = base.zerowidth().unwrap_or(&[]).to_vec();
    let mut consumed = unit_len(cells, start);
    let base_regional = is_regional_indicator(base.c);
    let mut paired_regional = false;

    while let Some(next) = cells.get(start + consumed) {
        if next.point.line.0 != line {
            break;
        }
        let ncell = next.cell;
        if ncell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            break;
        }
        let last = combiners.last().copied().unwrap_or(base.c);
        let joins = is_zwj(last)
            || is_emoji_modifier(ncell.c)
            || is_variation_selector(ncell.c)
            || (base_regional && is_regional_indicator(ncell.c) && !paired_regional);
        if !joins {
            break;
        }
        if is_regional_indicator(ncell.c) {
            paired_regional = true;
        }
        combiners.push(ncell.c);
        combiners.extend_from_slice(ncell.zerowidth().unwrap_or(&[]));
        consumed += unit_len(cells, start + consumed);
    }
    (combiners, consumed)
}

/// Fill `out` with display row `row_u`'s cells exactly as `Grid::display_iter`
/// would yield them — same `Point` (line + column) and the same `&Cell`
/// references, in the same left-to-right order — by indexing the grid row
/// directly (O(cols)) instead of materializing the whole viewport.
///
/// `out` is `clear()`ed first, so one buffer can be reused across every row in a
/// frame. Returns `false` (leaving `out` empty) when `row_u` lies outside the
/// grid's visible lines, i.e. the row `display_iter` would never yield; the
/// caller skips it, matching the old full-collect loop's bounds behaviour.
///
/// Mapping (verified against alacritty_terminal 0.26 `Grid::display_iter`, which
/// walks `Line(-display_offset)..=Line(screen_lines - 1 - display_offset)` over
/// columns `0..columns`): display row `row_u` ⇔ `Line(row_u - display_offset)`.
pub fn collect_display_row<'a>(
    grid: &'a alacritty_terminal::grid::Grid<Cell>,
    row_u: usize,
    display_offset: i32,
    out: &mut Vec<Indexed<&'a Cell>>,
) -> bool {
    out.clear();
    if row_u >= grid.screen_lines() {
        return false;
    }
    let line = Line(row_u as i32 - display_offset);
    let row = &grid[line];
    // `display_iter` bounds columns by `grid.columns()` (== the row's width); use
    // the same source so the buffer is a byte-for-byte match of the iterator.
    for c in 0..grid.columns() {
        let col = Column(c);
        out.push(Indexed {
            point: Point::new(line, col),
            cell: &row[col],
        });
    }
    true
}

/// Physical-pixel step per Ctrl +/- font-size adjustment.
pub(crate) const FONT_STEP_PX: f32 = 2.0;

/// Apply live-reloadable config settings. Called when UserEvent::ConfigReload is
/// received (config file was modified). Only opacity/bell_visual/status_bar/
/// pane_headers/word_separator apply live; font changes require a full reload.
impl App {
    /// Re-read `glassy.conf` (+ CLI-less defaults) from disk and apply the
    /// live-reloadable subset via [`Self::apply_config_reload`]. This is the
    /// exact path `UserEvent::ConfigReload` runs when the file watcher notices
    /// an edit (see `user_event.rs`) — factored out here so the `glassy @
    /// reload-config` and `glassy @ set-config` remote-control verbs
    /// (`remote.rs`) can trigger the identical reload without duplicating the
    /// match. Returns `Err` with a message suitable for an IPC `ERR` reply on
    /// a resolve failure; `Ok(None)` from `Settings::resolve` (the
    /// `--help`/`--version` early-exit path) can't happen with an empty CLI
    /// arg iterator, but is handled defensively rather than panicking.
    pub(crate) fn reload_config_from_disk(&mut self) -> Result<(), String> {
        match crate::config::Settings::resolve(std::iter::empty()) {
            Ok(Some(settings)) => {
                self.apply_config_reload(&settings.config);
                Ok(())
            }
            Ok(None) => Err("config reload produced no settings".to_string()),
            Err(e) => Err(format!("config reload failed: {e:#}")),
        }
    }

    pub(crate) fn apply_config_reload(&mut self, new_config: &Config) {
        // Opacity changes take effect immediately in the renderer.
        if new_config.opacity != self.config.opacity {
            self.config.opacity = new_config.opacity;
            if let Some(r) = &mut self.renderer {
                r.set_opacity(self.config.opacity);
            }
            self.dirty = true;
        }

        // Unfocused-dim strength applies on the next split frame: the overlay is
        // re-emitted per frame from the renderer's live value (cached panes too).
        if new_config.unfocused_dim != self.config.unfocused_dim {
            self.config.unfocused_dim = new_config.unfocused_dim;
            if let Some(r) = &mut self.renderer {
                r.set_pane_dim(self.config.unfocused_dim);
            }
            self.dirty = true;
        }

        // Opacity scope bakes into cached per-row glyph instances at push time,
        // so flipping it needs a full repaint, not just a redraw.
        if new_config.opacity_text != self.config.opacity_text {
            self.config.opacity_text = new_config.opacity_text;
            if let Some(r) = &mut self.renderer {
                r.set_text_opacity(self.config.opacity_text);
            }
            self.dirty = true;
            self.force_full_redraw = true;
        }

        // Window effect changes hot-swap the post-process mode. Switching to/from
        // None toggles the offscreen pass; the renderer rebuilds resources lazily.
        if new_config.window_effect != self.config.window_effect {
            self.config.window_effect = new_config.window_effect;
            let custom = self.config.custom_effect;
            if let Some(r) = &mut self.renderer {
                r.set_window_effect_resolved(self.config.window_effect, custom);
            }
            self.dirty = true;
            self.force_full_redraw = true;
        }

        // Bell flags can be toggled without a reload.
        if new_config.bell_visual != self.config.bell_visual {
            self.config.bell_visual = new_config.bell_visual;
        }
        if new_config.bell_audible != self.config.bell_audible {
            self.config.bell_audible = new_config.bell_audible;
        }

        // Status bar toggle: resize layout and force a full redraw.
        // Note: we can't call handle_resize here since it needs the ActiveEventLoop,
        // so we rely on the next window event to trigger a resize update naturally.
        if new_config.status_bar != self.config.status_bar {
            self.config.status_bar = new_config.status_bar;
            self.dirty = true;
            self.force_full_redraw = true;
        }

        // Pane headers toggle: affects split panes and forces a redraw.
        if new_config.pane_headers != self.config.pane_headers {
            self.config.pane_headers = new_config.pane_headers;
            self.dirty = true;
            self.force_full_redraw = true;
        }

        // Command-history capacity: update the cap and trim the existing ring if
        // it shrank. Not a visual change (the palette rebuilds on next open).
        if new_config.command_history != self.config.command_history {
            self.config.command_history = new_config.command_history;
            self.trim_command_history();
        }

        // Word separator for selection: update the config and push the merged
        // semantic_escape_chars to all live PTYs so double-click word selection
        // honours the new setting immediately without restarting.
        if new_config.word_separator != self.config.word_separator {
            self.config.word_separator = new_config.word_separator.clone();
            let escape_chars = crate::pty::merge_word_separators(
                alacritty_terminal::term::SEMANTIC_ESCAPE_CHARS,
                &self.config.word_separator,
            );
            // Push to all PTYs: active pane, non-focused panes of the active
            // tab, and every parked background tab.
            let push = |pty: &crate::pty::Pty, escape: &str| {
                use alacritty_terminal::term::Config as TermConfig;
                let scrollback = pty.term.lock().grid().history_size();
                let new_term_cfg = TermConfig {
                    scrolling_history: scrollback,
                    semantic_escape_chars: escape.to_owned(),
                    ..TermConfig::default()
                };
                pty.term.lock().set_options(new_term_cfg);
            };
            if let Some(pty) = self.pty.as_ref() {
                push(pty, &escape_chars);
            }
            if let Some(g) = self.panes.as_ref() {
                for pty in g.others.values() {
                    push(pty, &escape_chars);
                }
            }
            for s in &self.background {
                push(&s.pty, &escape_chars);
                if let Some(g) = s.panes.as_ref() {
                    for pty in g.others.values() {
                        push(pty, &escape_chars);
                    }
                }
            }
        }

        // Theme: hot-swap the global theme. If follow_system is on, also recompute
        // the active theme based on the system preference.
        if new_config.follow_system {
            self.config.follow_system = new_config.follow_system;
            self.config.theme_light = new_config.theme_light.clone();
            self.config.theme_dark = new_config.theme_dark.clone();
            if let Some(window) = &self.window
                && self.apply_system_theme(window.theme())
            {
                self.force_full_redraw = true;
                self.dirty = true;
            }
        } else if new_config.theme != self.config.theme {
            self.config.theme = new_config.theme.clone();
            self.force_full_redraw = true;
            self.dirty = true;
            // Theme is a global, so we need to notify the color module.
            if let Some(theme) = crate::color::theme_by_name(&self.config.theme) {
                crate::color::set_theme(theme);
            }
        }

        // Command-finish notification + folding settings: plain flags/values,
        // applied immediately (not a visual change, so no redraw needed).
        self.config.notify_command_finish = new_config.notify_command_finish;
        self.config.notify_command_threshold_ms = new_config.notify_command_threshold_ms;
        if new_config.command_fold != self.config.command_fold {
            self.config.command_fold = new_config.command_fold;
            // If folding was just turned off, clear any active folds so the view
            // reverts to fully-expanded output.
            if !self.config.command_fold && self.fold_state.any() {
                self.fold_state = command_blocks::FoldState::default();
                self.dirty = true;
                self.force_full_redraw = true;
            }
        }
    }
}

/// Fire a rich desktop notification from a [`NotifySpec`](crate::image::NotifySpec):
/// title/body plus the optional icon, sound, urgency, and action buttons. Called
/// on the UI thread when an OSC 9 / OSC 777 notification arrives and the window
/// is unfocused, or for the command-finish alert.
///
/// Uses a detached thread so the D-Bus round-trip (and, when actions are present,
/// the blocking wait for the user's button press) never stalls the UI loop. An
/// empty title defaults to "glassy". Errors are logged at debug level (a missing
/// notification daemon is a non-fatal, common desktop configuration).
pub(super) fn fire_notification(spec: &crate::image::NotifySpec) {
    use notify_rust::{Notification, Timeout};
    // Apply urgency only where notify-rust supports it: Linux/BSD + Windows expose
    // `.urgency()`; macOS gates it behind an unstable preview feature, so its
    // variant is a no-op. Taking the field on both keeps `spec.urgency` read.
    #[cfg(not(target_os = "macos"))]
    fn apply_urgency(n: &mut Notification, urgency: crate::image::NotifyUrgency) {
        use notify_rust::Urgency;
        n.urgency(match urgency {
            crate::image::NotifyUrgency::Low => Urgency::Low,
            crate::image::NotifyUrgency::Normal => Urgency::Normal,
            crate::image::NotifyUrgency::Critical => Urgency::Critical,
        });
    }
    #[cfg(target_os = "macos")]
    fn apply_urgency(_n: &mut Notification, _urgency: crate::image::NotifyUrgency) {}
    let spec = spec.clone();
    std::thread::Builder::new()
        .name("glassy-notify".to_string())
        .spawn(move || {
            let summary = if spec.title.is_empty() {
                "Glassy".to_string()
            } else {
                spec.title.clone()
            };
            let mut n = Notification::new();
            n.summary(&summary)
                .body(&spec.body)
                .appname("Glassy")
                .timeout(Timeout::Milliseconds(5000));
            // Urgency is applied via a cfg'd helper (below): notify-rust only
            // exposes `.urgency()` on Linux/BSD + Windows — on macOS it lives
            // behind an unstable preview feature we don't enable. Routing through
            // the helper (which always *takes* the field) keeps the mac build
            // compiling AND keeps `spec.urgency` "read" on every platform.
            apply_urgency(&mut n, spec.urgency);
            if let Some(icon) = &spec.icon {
                n.icon(icon);
            }
            if let Some(sound) = &spec.sound {
                n.sound_name(sound);
            }
            for (id, label) in &spec.actions {
                n.action(id, label);
            }
            match n.show() {
                Ok(handle) => {
                    // If the notification carries action buttons, block this
                    // (detached) thread waiting for the user to click one and log
                    // which fired. A future enhancement can route the chosen
                    // action back into the app (e.g. focus a tab); for now we
                    // surface the buttons + record the choice.
                    if !spec.actions.is_empty() {
                        handle.wait_for_action(|action| {
                            log::debug!("notification action invoked: {action}");
                        });
                    }
                }
                Err(e) => log::debug!("desktop notification failed: {e}"),
            }
        })
        .ok(); // ignore thread-spawn failure (extremely unlikely)
}

/// Read the git branch name for a directory by reading
/// `.git/HEAD` relative to the closest ancestor that contains a `.git` entry.
/// This is a pure filesystem read — no child process, no blocking I/O beyond
/// a few small file reads — so it is safe to call from the UI thread at low
/// frequency.
///
/// Returns `None` when the directory is not inside a git repository or the
/// branch name cannot be determined.
pub(crate) fn read_git_branch(cwd: &std::path::Path) -> Option<String> {
    // Walk upward from cwd until we find a directory containing `.git`.
    let mut dir = Some(cwd);
    while let Some(d) = dir {
        let git_head = d.join(".git/HEAD");
        if git_head.exists() {
            // Parse the HEAD file: `ref: refs/heads/<branch>` or a bare SHA.
            if let Ok(content) = std::fs::read_to_string(&git_head) {
                let trimmed = content.trim();
                if let Some(branch) = trimmed.strip_prefix("ref: refs/heads/") {
                    return Some(branch.to_string());
                }
                // Detached HEAD — show first 7 chars of the SHA.
                if trimmed.len() >= 7 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Some(format!(":{}", &trimmed[..7]));
                }
            }
            return None;
        }
        dir = d.parent();
    }
    None
}

/// How often (minimum) to re-read `/proc` for pane info. 2 seconds is cheap
/// enough to feel live but never blocks idle frames at 0% CPU.
pub(super) const PROC_REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

impl App {
    /// Fire a desktop notification for a finished OSC 133-tracked command when it
    /// ran long enough AND the window is unfocused. A no-op when the feature is
    /// disabled (`notify_command_finish = false`) or the command was faster than
    /// the configured threshold — so quick commands never spam, and a focused
    /// user (who can already see the result) is never interrupted.
    ///
    /// The notification's urgency reflects the exit status: a non-zero exit is
    /// `Critical` (a build/test failed); success is `Normal`. The body names the
    /// command (truncated) and its duration.
    pub(crate) fn notify_command_finished(
        &self,
        command: Option<String>,
        exit: Option<i32>,
        duration: std::time::Duration,
    ) {
        if !self.config.notify_command_finish || self.focused {
            return;
        }
        let threshold = std::time::Duration::from_millis(self.config.notify_command_threshold_ms);
        // A zero threshold means "always notify" (duration is never < ZERO);
        // otherwise the command must have run at least `threshold`.
        if duration < threshold {
            return;
        }
        let ok = exit.map(|c| c == 0).unwrap_or(true);
        let dur = crate::app::command_blocks::format_duration(duration);
        // Truncate the command for the body so a giant one-liner stays readable.
        let cmd = command.unwrap_or_default();
        let cmd_short: String = if cmd.chars().count() > 80 {
            let mut s: String = cmd.chars().take(77).collect();
            s.push('…');
            s
        } else {
            cmd
        };
        let (title, body) = if ok {
            (
                "Command finished".to_string(),
                if cmd_short.is_empty() {
                    format!("Done in {dur}")
                } else {
                    format!("{cmd_short}\nDone in {dur}")
                },
            )
        } else {
            let code = exit.map(|c| format!(" (exit {c})")).unwrap_or_default();
            (
                "Command failed".to_string(),
                if cmd_short.is_empty() {
                    format!("Failed{code} after {dur}")
                } else {
                    format!("{cmd_short}\nFailed{code} after {dur}")
                },
            )
        };
        let spec = crate::image::NotifySpec {
            title,
            body,
            icon: Some(if ok {
                "dialog-information".to_string()
            } else {
                "dialog-error".to_string()
            }),
            urgency: if ok {
                crate::image::NotifyUrgency::Normal
            } else {
                crate::image::NotifyUrgency::Critical
            },
            ..crate::image::NotifySpec::default()
        };
        fire_notification(&spec);
    }

    /// Refresh the cached `/proc` pane info for a single PTY when the cache is
    /// stale (older than `PROC_REFRESH_INTERVAL`). Called on pane focus and in
    /// `about_to_wait` for the periodic background poll. Cheap: skipped if the
    /// cache is fresh.
    pub(crate) fn maybe_refresh_proc_info(pty: &mut crate::pty::Pty) {
        let now = std::time::Instant::now();
        if now.duration_since(pty.pane_info_at) >= PROC_REFRESH_INTERVAL {
            pty.pane_info = crate::pty::PaneInfo::read(pty.shell_pid);
            pty.pane_info_at = now;
        }
    }
}

/// Spawn a background thread to watch the config file for changes.
/// Uses notify crate (debounced) to avoid spamming reloads during rapid edits.
pub(super) fn spawn_config_watcher(
    config_path: std::path::PathBuf,
    proxy: winit::event_loop::EventLoopProxy<crate::pty::UserEvent>,
) {
    use notify::recommended_watcher;
    use notify::{RecursiveMode, Result as NotifyResult, Watcher};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    thread::spawn(move || {
        let last_reload = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(10)));
        let config_dir = config_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let config_filename = config_path.file_name().unwrap_or_default().to_owned();
        let (tx, rx) = std::sync::mpsc::channel();

        let mut watcher = match recommended_watcher(move |res: NotifyResult<_>| {
            let _ = tx.send(res);
        }) {
            Ok(w) => w,
            Err(e) => {
                log::warn!("failed to create config watcher: {e}");
                return;
            }
        };

        if let Err(e) = watcher.watch(config_dir, RecursiveMode::NonRecursive) {
            log::warn!("failed to watch config dir {}: {e}", config_dir.display());
            return;
        }

        for event_result in rx {
            match event_result {
                Ok(event) => {
                    let is_config_change = event
                        .paths
                        .iter()
                        .any(|p| p.file_name().is_some_and(|n| n == config_filename));
                    if is_config_change {
                        let now = Instant::now();
                        let mut last = last_reload.lock().unwrap();
                        if now.duration_since(*last) >= Duration::from_millis(500) {
                            *last = now;
                            drop(last);
                            if let Err(e) = proxy.send_event(crate::pty::UserEvent::ConfigReload) {
                                log::debug!("failed to send ConfigReload: {e}");
                            }
                        }
                    }
                }
                Err(e) => {
                    log::debug!("config watcher error: {e}");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::Term;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config as TermConfig;
    use alacritty_terminal::vte::ansi::Handler;

    /// Minimal [`Dimensions`] for building a PTY-less test [`Term`] (mirrors the
    /// vi_mode tests). `total_lines` matches `screen_lines` (no fixed scrollback
    /// cap; history grows as lines are pushed off the top).
    struct Size {
        cols: usize,
        lines: usize,
    }
    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            self.lines
        }
        fn screen_lines(&self) -> usize {
            self.lines
        }
        fn columns(&self) -> usize {
            self.cols
        }
    }

    fn make_term(cols: usize, lines: usize) -> Term<VoidListener> {
        Term::new(TermConfig::default(), &Size { cols, lines }, VoidListener)
    }

    /// Type a literal string through the VTE handler, breaking on `\n`.
    fn type_str(t: &mut Term<VoidListener>, s: &str) {
        for ch in s.chars() {
            if ch == '\n' {
                t.linefeed();
                t.carriage_return();
            } else {
                t.input(ch);
            }
        }
    }

    /// THE refactor invariant. For every display row, `collect_display_row` must
    /// reproduce exactly the cells `Grid::display_iter` yields for that row: the
    /// identical `Point` (line + column) and the identical `&Cell` reference
    /// (compared by address), in the same left-to-right order. This is precisely
    /// what the old flat `display_iter.collect()` fed the render loops, so matching
    /// it row-by-row pins that the per-row buffers are a byte-for-byte substitute —
    /// including under a non-zero `display_offset`, where the row↔line mapping bites.
    fn assert_row_buffers_match_display_iter(t: &Term<VoidListener>) {
        let grid = t.grid();
        let display_offset = grid.display_offset() as i32;
        let screen_lines = grid.screen_lines();

        // Reference: bucket display_iter's output by display row.
        let mut expected: Vec<Vec<(Point, *const Cell)>> = vec![Vec::new(); screen_lines];
        for indexed in grid.display_iter() {
            let row_u = (indexed.point.line.0 + display_offset) as usize;
            expected[row_u].push((indexed.point, indexed.cell as *const Cell));
        }

        // Actual: the production per-row helper, buffer reused across rows.
        let mut buf: Vec<Indexed<&Cell>> = Vec::new();
        for (row_u, expected_row) in expected.iter().enumerate() {
            let present = collect_display_row(grid, row_u, display_offset, &mut buf);
            assert!(present, "row {row_u} within screen_lines must be present");
            let actual: Vec<(Point, *const Cell)> = buf
                .iter()
                .map(|c| (c.point, c.cell as *const Cell))
                .collect();
            assert_eq!(
                &actual, expected_row,
                "row {row_u} buffer differs from display_iter (offset {display_offset})"
            );
        }
    }

    #[test]
    fn row_buffers_equal_display_iter_unscrolled() {
        let mut t = make_term(10, 5);
        type_str(&mut t, "hello\nworld\nfoo\nbar\nbaz");
        assert_eq!(t.grid().display_offset(), 0);
        assert_row_buffers_match_display_iter(&t);
    }

    #[test]
    fn row_buffers_equal_display_iter_scrolled() {
        // Produce scrollback, then scroll up so display_offset > 0 — the case the
        // `line + display_offset` mapping has to get exactly right.
        let mut t = make_term(8, 4);
        for i in 0..20 {
            type_str(&mut t, &format!("row{i}\n"));
        }
        t.scroll_display(Scroll::Delta(3));
        assert!(
            t.grid().display_offset() > 0,
            "test setup: expected a non-zero scrollback offset"
        );
        assert_row_buffers_match_display_iter(&t);
    }

    #[test]
    fn row_buffers_equal_display_iter_with_wide_and_combining() {
        // Wide CJK + a combining mark exercise the row-scoped lookahead
        // (`unit_len` spacer handling and `build_grapheme`) over the buffer; the
        // helper must still mirror the grid cell-for-cell.
        let mut t = make_term(20, 3);
        type_str(&mut t, "aｗb\n"); // full-width 'ｗ' → WIDE_CHAR + trailing spacer
        type_str(&mut t, "e\u{0301}x"); // 'e' + combining acute accent
        assert_row_buffers_match_display_iter(&t);
    }

    #[test]
    fn row_out_of_range_is_absent() {
        // A display row past the grid's visible lines is never yielded by
        // display_iter; the helper reports it absent and leaves the buffer empty
        // (so the render loops skip it, matching the old bounds check).
        let t = make_term(6, 3);
        let grid = t.grid();
        let mut buf: Vec<Indexed<&Cell>> = Vec::new();
        assert!(!collect_display_row(grid, 3, 0, &mut buf));
        assert!(buf.is_empty());
        assert!(!collect_display_row(grid, 99, 0, &mut buf));
        assert!(buf.is_empty());
    }

    #[test]
    fn display_row_to_line_mapping_is_exact() {
        // Pin the display_offset arithmetic exhaustively: display row `row_u` maps
        // to `Line(row_u - display_offset)` and inverts back to `row_u`.
        for display_offset in 0..12i32 {
            for row_u in 0..40usize {
                let line = Line(row_u as i32 - display_offset);
                assert_eq!(line.0 + display_offset, row_u as i32);
            }
        }
    }
}
