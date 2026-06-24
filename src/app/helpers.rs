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
        "glassy".to_string()
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
}

/// One placed tab-bar item with its pixel rect. The label is carried for the tab
/// body (it is what gets drawn / measured); control buttons carry an empty label.
#[derive(Clone, Debug)]
pub(crate) struct StripSeg {
    pub(crate) item: StripItem,
    pub(crate) label: String,
    pub(crate) rect: gui::Rect,
}

/// A tab descriptor in stable display order: (title, is_active, has_activity).
pub(crate) type TabDesc<'a> = (&'a str, bool, bool);

/// Lay out the real GUI tab bar across the pixel-wide bar `[0, bar_w)` at height
/// `bar_h`, from tab descriptors in stable order. Produces, left→right: the glassy
/// mark slot, a `+` new-tab button, the tab chips (each a body rect + an embedded
/// close-box rect in multi-tab mode), and right-aligned `?` help, `*` settings,
/// `#` menu icon buttons. The active tab keeps its position. Pure (pixel math
/// only) so the painter and the click hit-test agree, and so it is unit-testable.
pub(crate) fn strip_layout(tabs: &[TabDesc], bar_w: f32, bar_h: f32, cell_w: f32) -> Vec<StripSeg> {
    let mut segs = Vec::new();
    if bar_w <= 0.0 || bar_h <= 0.0 {
        return segs;
    }
    // Tab chips are inset vertically so the active chip's top accent rail and the
    // inactive chips' recess read clearly against the bar.
    let chip_y = ((bar_h - bar_h * 0.82) * 0.5).round();
    let chip_h = (bar_h - chip_y).max(1.0); // flush to the bar bottom (connector)
    let ctrl_y = ((bar_h - CTRL_BTN) * 0.5).round().max(0.0);

    // Right-aligned control buttons: help, settings, menu (in that visual order).
    let right_btns = [StripItem::Help, StripItem::Settings, StripItem::Menu];
    let right_w = CTRL_BTN * right_btns.len() as f32;
    let right_start = (bar_w - right_w - TAB_GAP).max(0.0);

    // Decorative mark on the far left (the " ◆ " brand), then the + button.
    let mark_w = (cell_w * 3.0).round();
    let mut x = mark_w;

    // New-tab button sits right after the mark.
    let plus = gui::Rect::new(x, ctrl_y, CTRL_BTN, CTRL_BTN);
    if plus.x + plus.w <= right_start {
        segs.push(StripSeg { item: StripItem::NewTab, label: String::new(), rect: plus });
    }
    x += CTRL_BTN + TAB_GAP * 2.0;

    let tabs_left = x;
    let avail = (right_start - tabs_left - TAB_GAP).max(0.0);
    let n = tabs.len().max(1);

    if tabs.len() <= 1 {
        // Single tab: one wide chip spanning the available width (no close box —
        // closing it quits). It still reads as a real connected tab.
        let w = avail.clamp(0.0, TAB_MAX_W * 1.6);
        if w > 0.0 {
            segs.push(StripSeg {
                item: StripItem::Tab(0),
                label: tabs.first().map(|t| t.0.to_string()).unwrap_or_default(),
                rect: gui::Rect::new(tabs_left, chip_y, w, chip_h),
            });
        }
    } else {
        // Multi-tab: equal-width chips clamped to [MIN, MAX], each with a close box.
        let per = ((avail + TAB_GAP) / n as f32 - TAB_GAP).clamp(0.0, TAB_MAX_W);
        let tw = per.max(TAB_MIN_W.min(per.max(0.0))); // never below MIN unless squeezed
        let tw = if per < TAB_MIN_W { per } else { tw };
        let mut tx = tabs_left;
        for (i, (title, _a, _b)) in tabs.iter().enumerate() {
            if tx + tw > right_start + 0.5 {
                break; // out of room before the controls
            }
            let body = gui::Rect::new(tx, chip_y, tw, chip_h);
            segs.push(StripSeg { item: StripItem::Tab(i), label: title.to_string(), rect: body });
            // Close box anchored to the chip's right edge, vertically centered.
            let cb = CLOSE_BOX.min(tw * 0.5);
            let close = gui::Rect::new(
                tx + tw - cb - TAB_PAD_X * 0.5,
                chip_y + (chip_h - cb) * 0.5,
                cb,
                cb,
            );
            segs.push(StripSeg {
                item: StripItem::TabClose(i),
                label: String::new(),
                rect: close,
            });
            tx += tw + TAB_GAP;
        }
    }

    // Right controls.
    let mut rx = right_start;
    for item in right_btns {
        segs.push(StripSeg {
            item,
            label: String::new(),
            rect: gui::Rect::new(rx, ctrl_y, CTRL_BTN, CTRL_BTN),
        });
        rx += CTRL_BTN;
    }
    segs
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

/// Lighten an RGB color toward white by `amount`, keeping alpha. Used for the
/// raised help-panel surface.
/// Percent-encode a filesystem path for embedding in a `file://` URI. Keeps the
/// unreserved set (RFC 3986) plus `/` (path separator) verbatim; everything else
/// (spaces, `#`, `?`, non-ASCII bytes, …) is `%XX`-escaped so the URI can't be
/// truncated or reinterpreted by the URL handler.
pub(crate) fn percent_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for &b in path.as_bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~');
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
            out.push(char::from_digit((b & 0xf) as u32, 16).unwrap().to_ascii_uppercase());
        }
    }
    out
}

pub(crate) fn lighten(c: [f32; 4], amount: f32) -> [f32; 4] {
    [
        (c[0] + amount).min(1.0),
        (c[1] + amount).min(1.0),
        (c[2] + amount).min(1.0),
        c[3],
    ]
}

/// Actions available in the # hamburger dropdown and the right-click context
/// menu. Kept as a single enum so the hit-test and keyboard dispatch share one
/// definition across both menus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MenuAction {
    Copy,
    Paste,
    NewTab,
    Settings,
    PaneHeaders,
    Help,
    CloseTab,
}

impl MenuAction {
    /// The fixed set shown by the # hamburger dropdown. The right-click context
    /// menu uses a separately-built `Vec<MenuAction>` (see `context_menu_items`).
    pub(crate) const ALL: &'static [MenuAction] = &[
        MenuAction::NewTab,
        MenuAction::Settings,
        MenuAction::PaneHeaders,
        MenuAction::Help,
        MenuAction::CloseTab,
    ];

    pub(crate) fn label(self) -> &'static str {
        match self {
            MenuAction::Copy => "Copy",
            MenuAction::Paste => "Paste",
            MenuAction::NewTab => "New tab",
            MenuAction::Settings => "Settings",
            MenuAction::PaneHeaders => "Pane headers",
            MenuAction::Help => "Help / keys",
            MenuAction::CloseTab => "Close tab",
        }
    }

    /// Left-column icon glyph for the real GUI menu (§3.6).
    pub(crate) fn icon(self) -> char {
        match self {
            MenuAction::Copy      => '◻',
            MenuAction::Paste     => '□',
            MenuAction::NewTab    => '+',
            MenuAction::Settings  => '*',
            MenuAction::PaneHeaders => '▭',
            MenuAction::Help      => '?',
            MenuAction::CloseTab  => '✕',
        }
    }

    /// Right-aligned shortcut hint for the real GUI menu (§3.6). `None` for
    /// actions that have no keybinding shown in the menu.
    pub(crate) fn shortcut(self) -> Option<&'static str> {
        match self {
            MenuAction::Copy      => Some("Ctrl+Shift+C"),
            MenuAction::Paste     => Some("Ctrl+Shift+V"),
            MenuAction::NewTab    => Some("Ctrl+Shift+T"),
            MenuAction::Settings  => Some("Ctrl+,"),
            MenuAction::PaneHeaders => None,
            MenuAction::Help      => Some("F1"),
            MenuAction::CloseTab  => Some("Ctrl+Shift+W"),
        }
    }
}

/// Build a `Vec<MenuEntry>` for the real GUI menu from a flat list of
/// `MenuAction`s. Context menus with Copy+Paste get a separator between the
/// clipboard group and the navigation group; the hamburger menu has its own
/// separator after Settings (before CloseTab). Pure for testing.
pub(crate) fn actions_to_entries(actions: &[MenuAction], has_selection: bool) -> Vec<gui::MenuEntry<'static>> {
    let mut v: Vec<gui::MenuEntry<'static>> = Vec::with_capacity(actions.len() + 2);
    // We need to detect group boundaries and insert separators.
    // Simple rule: insert a separator before NewTab when Copy/Paste precede it
    // (context menu), and before CloseTab when Settings precedes it (hamburger).
    let mut prev: Option<MenuAction> = None;
    for &a in actions {
        // Separator before group boundary.
        let sep = match (prev, a) {
            // Context menu: clipboard group → navigation group.
            (Some(MenuAction::Copy | MenuAction::Paste), MenuAction::NewTab) => true,
            // Hamburger: app group → destructive group.
            (Some(MenuAction::Help), MenuAction::CloseTab) => true,
            _ => false,
        };
        if sep {
            v.push(gui::MenuEntry::Separator);
        }
        let enabled = match a {
            MenuAction::Copy  => has_selection,
            MenuAction::Paste => true,
            _                 => true,
        };
        v.push(gui::MenuEntry::Item {
            icon:    a.icon(),
            label:   a.label(),
            hint:    a.shortcut(),
            enabled,
        });
        prev = Some(a);
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
pub(crate) fn build_grapheme(cells: &[Indexed<&Cell>], start: usize, line: i32) -> (Vec<char>, usize) {
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

/// Physical-pixel step per Ctrl +/- font-size adjustment.
pub(crate) const FONT_STEP_PX: f32 = 2.0;

/// Apply live-reloadable config settings. Called when UserEvent::ConfigReload is
/// received (config file was modified). Only opacity/bell_visual/status_bar/
/// pane_headers/word_separator apply live; font changes require a full reload.
impl App {
    pub(crate) fn apply_config_reload(&mut self, new_config: &Config) {
        // Opacity changes take effect immediately in the renderer.
        if new_config.opacity != self.config.opacity {
            self.config.opacity = new_config.opacity;
            if let Some(r) = &mut self.renderer {
                r.set_opacity(self.config.opacity);
            }
            self.dirty = true;
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
    }
}

/// Fire a desktop notification with `app_name` as the summary prefix and `body`
/// as the message. Called on the UI thread when an OSC 9 / OSC 777 notification
/// arrives and the window is unfocused.
///
/// Uses a detached thread so the D-Bus round-trip never blocks the UI loop.
/// Errors are logged at debug level (a missing notification daemon is a
/// non-fatal, common desktop configuration).
pub(super) fn fire_desktop_notification(app_name: &str, body: &str) {
    use notify_rust::Notification;
    let summary = app_name.to_string();
    let body = body.to_string();
    std::thread::Builder::new()
        .name("glassy-notify".to_string())
        .spawn(move || {
            let result = Notification::new()
                .summary(&summary)
                .body(&body)
                .appname("glassy")
                .timeout(notify_rust::Timeout::Milliseconds(5000))
                .show();
            if let Err(e) = result {
                log::debug!("desktop notification failed: {e}");
            }
        })
        .ok(); // ignore thread-spawn failure (extremely unlikely)
}

/// Spawn a background thread to watch the config file for changes.
/// Uses notify crate (debounced) to avoid spamming reloads during rapid edits.
pub(super) fn spawn_config_watcher(
    config_path: std::path::PathBuf,
    proxy: winit::event_loop::EventLoopProxy<crate::pty::UserEvent>,
) {
    use notify::{Watcher, RecursiveMode, Result as NotifyResult};
    use notify::recommended_watcher;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    use std::thread;

    thread::spawn(move || {
        let last_reload = Arc::new(Mutex::new(Instant::now() - Duration::from_secs(10)));
        let config_dir = config_path.parent().unwrap_or_else(|| std::path::Path::new("."));
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

