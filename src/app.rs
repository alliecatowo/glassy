//! The winit UI/render driver.
//!
//! Idle behaviour is `ControlFlow::Wait`: 0% CPU, no GPU submits until the PTY
//! thread (or a resize/input) wakes us. Wakeups set a dirty flag and are
//! coalesced to at most one frame per monitor refresh, so a fast producer like
//! Claude Code streaming tokens collapses into a single redraw per refresh
//! instead of one redraw per token burst.

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Indexed, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::tty::Shell;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::bell::{self, AudioBell};
use crate::color;
use crate::input::{MouseReport, encode_key, encode_mouse};
use crate::pty::{Pty, UserEvent};
use crate::renderer::{CursorOverlay, Decorations, Renderer, UnderlineStyle};

/// Lines of scrollback to move per wheel notch when reporting to a TUI or
/// scrolling glassy's own scrollback buffer.
const WHEEL_LINES: i32 = 3;

/// Screen rows reserved at the top for the tab strip. The strip doubles as a
/// title bar (it always shows, even with one tab), so this is a constant and the
/// terminal grid simply starts one row down — no resize churn when tabs change.
const TAB_STRIP_ROWS: usize = 1;

/// What a wheel notch should do, given the terminal's current mode. Pure so it
/// can be unit-tested without a window or PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WheelAction {
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
fn wheel_action(mode: TermMode) -> WheelAction {
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
fn image_dst_size(
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
fn fit_label(t: &str, max: usize) -> String {
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

/// Cells occupied by the left-hand " ◆ " mark before the tab chips.
const HEADER_MARK_COLS: usize = 3;

/// An interactive item in the inline app toolbar (the strip *under* the native
/// OS titlebar). Window controls (min/max/close) intentionally live in the
/// native bar, not here. `Tab`/`TabClose` carry the tab's *stable position*.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StripItem {
    /// A tab chip body at stable display position `pos` (click = activate).
    Tab(usize),
    /// A tab's ✕ close affordance at stable position `pos`.
    TabClose(usize),
    NewTab,
    Help,
    Menu,
}

/// One placed strip item with its label and half-open cell range `[start, end)`.
#[derive(Clone)]
struct StripSeg {
    item: StripItem,
    label: String,
    start: usize,
    end: usize,
}

/// Each toolbar control button is this many cells (a glyph padded by a space).
const STRIP_BTN_W: usize = 3;

/// A tab descriptor in stable display order: (title, is_active, has_activity).
type TabDesc<'a> = (&'a str, bool, bool);

/// Lay out the inline toolbar across `cols` from tab descriptors in stable order:
/// the glassy mark, then either the title (one tab) or numbered chips with a per-
/// chip ✕ (multiple), a `+` new-tab button, and right-aligned `?` help + `≡` menu.
/// The active tab keeps its position — only its highlight differs. Shared by the
/// renderer and the click hit-test so drawn items and click targets always agree.
fn strip_layout(tabs: &[TabDesc], cols: usize) -> Vec<StripSeg> {
    let right_btns = [(StripItem::Help, " ? "), (StripItem::Menu, " ≡ ")];
    let right_w = STRIP_BTN_W * right_btns.len();
    let right_start = cols.saturating_sub(right_w);

    let mut segs = Vec::new();
    let mut col = HEADER_MARK_COLS; // after the decorative " ◆ " mark

    if tabs.len() <= 1 {
        // Single tab: just the title (no number, no ✕ — closing it = quit).
        let title = tabs.first().map(|t| t.0).unwrap_or("");
        let budget = right_start.saturating_sub(col + STRIP_BTN_W + 2).max(8);
        let label = format!(" {} ", fit_label(title, budget));
        let w = label.chars().count().min(right_start.saturating_sub(col));
        segs.push(StripSeg { item: StripItem::Tab(0), label, start: col, end: col + w });
        col += w;
    } else {
        for (i, (title, _active, _activity)) in tabs.iter().enumerate() {
            let body = format!(" {} {} ", i + 1, fit_label(title, 14));
            let bw = body.chars().count();
            // chip body + ✕ + a 1-col gap so chips read as distinct pills.
            if col + bw + 3 > right_start {
                break; // out of room before the controls
            }
            segs.push(StripSeg { item: StripItem::Tab(i), label: body, start: col, end: col + bw });
            col += bw;
            segs.push(StripSeg {
                item: StripItem::TabClose(i),
                label: "✕ ".to_string(),
                start: col,
                end: col + 2,
            });
            col += 2 + 1; // ✕ (2) + inter-chip gap (1)
        }
    }
    if col + STRIP_BTN_W <= right_start {
        segs.push(StripSeg {
            item: StripItem::NewTab,
            label: " + ".to_string(),
            start: col,
            end: col + STRIP_BTN_W,
        });
    }
    if right_start >= col {
        let mut rc = right_start;
        for (item, lbl) in right_btns {
            segs.push(StripSeg { item, label: lbl.to_string(), start: rc, end: rc + STRIP_BTN_W });
            rc += STRIP_BTN_W;
        }
    }
    segs
}

/// The toolbar item containing `click_col`, if any. Pure for unit testing.
fn strip_item_at(segs: &[StripSeg], click_col: usize) -> Option<StripItem> {
    segs.iter()
        .find(|s| click_col >= s.start && click_col < s.end)
        .map(|s| s.item)
}

/// Move the element at index `from` to index `to`, shifting the rest. Used to
/// reorder tabs by dragging. Pure for unit testing.
fn move_in_order<T>(v: &mut Vec<T>, from: usize, to: usize) {
    if from < v.len() && to < v.len() && from != to {
        let item = v.remove(from);
        v.insert(to, item);
    }
}

/// Lines shown in the F1 help overlay (left column = keys, right = action). Kept
/// as static text so the overlay costs nothing until it is opened.
const HELP_LINES: &[&str] = &[
    "  glassy — keybindings",
    "",
    "  Ctrl+Shift+T      New tab",
    "  Ctrl+Shift+W      Close tab",
    "  Ctrl+Tab          Next tab",
    "  Ctrl+Shift+Tab    Previous tab",
    "  Ctrl+Shift+C / V  Copy / Paste",
    "  Ctrl  +  /  -  / 0  Font bigger / smaller / reset",
    "  Shift+PgUp/PgDn   Scroll history",
    "  Shift+Home/End    Scroll top / bottom",
    "  Ctrl+Click        Open hyperlink",
    "  Ctrl+,            Settings",
    "",
    "  F1 or Esc         Close this help",
];

/// Build the lines shown in the Ctrl+, settings overlay from the live config and
/// the renderer's current physical font size. Read-only for now: it surfaces the
/// effective settings + where to change them. `&Config` is taken by reference so
/// the caller can pass `&self.config` alongside a live `&mut Renderer` borrow.
fn settings_lines(config: &Config, font_px: f32, sel: usize, saved: bool) -> Vec<String> {
    let family = config
        .font_family
        .as_deref()
        .unwrap_or("FiraCode Nerd Font (default)");
    let bell = if config.bell_visual { "visual" } else { "off" };
    // The three adjustable rows; the selected one gets a ▸ cursor.
    let mark = |row: usize| if row == sel { "▸" } else { " " };
    let saved_line = if saved {
        "  ✓ saved to config"
    } else {
        "  ↑↓ select · ←→ change · Enter save · Esc close"
    };
    vec![
        "  glassy — settings".to_string(),
        String::new(),
        format!("{} Font size    {font_px:.0} px", mark(0)),
        format!("{} Opacity      {:.2}", mark(1), config.opacity),
        format!("{} Bell         {bell}", mark(2)),
        format!("{} Theme        {}", mark(3), config.theme),
        String::new(),
        format!("  Font         {family}"),
        format!("  Scrollback   {} lines", config.scrollback),
        format!("  Config       {}", config_display_path()),
        String::new(),
        saved_line.to_string(),
    ]
}

/// Display path of the config file for the settings overlay.
fn config_display_path() -> String {
    crate::config::path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "~/.config/glassy/glassy.conf".to_string())
}

/// Darken an RGB color toward black by `f` (0 = black, 1 = unchanged), keeping
/// alpha. Used for the help-overlay backdrop.
fn darken(c: [f32; 4], f: f32) -> [f32; 4] {
    [c[0] * f, c[1] * f, c[2] * f, c[3]]
}

/// Lighten an RGB color toward white by `amount`, keeping alpha. Used for the
/// raised help-panel surface.
fn lighten(c: [f32; 4], amount: f32) -> [f32; 4] {
    [
        (c[0] + amount).min(1.0),
        (c[1] + amount).min(1.0),
        (c[2] + amount).min(1.0),
        c[3],
    ]
}

/// Draw a centered modal overlay (`lines`) over a dimmed full-screen backdrop.
/// The first line is rendered in the accent color as a title. Rebuilds every
/// screen row (`rows` terminal rows + the strip) so terminal content underneath
/// is fully replaced. Associated (not `&self`) so it composes with the active
/// `&mut Renderer` borrow in `render`. Used by the F1 help and Ctrl+, settings
/// overlays.
fn draw_modal(renderer: &mut Renderer, rows: usize, cols: usize, lines: &[&str]) {
    let total_rows = rows + TAB_STRIP_ROWS;
    let backdrop = darken(color::default_bg(), 0.45);
    let panel_bg = lighten(color::default_bg(), 0.07);
    let border_bg = [0.45, 0.68, 1.0, 1.0]; // accent
    let text_fg = color::default_fg();
    let title_fg = [0.55, 0.75, 1.0, 1.0];

    let content_w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let panel_w = (content_w + 4).min(cols.max(1));
    let panel_h = (lines.len() + 2).min(total_rows.max(1));
    let left = (cols.saturating_sub(panel_w)) / 2;
    let top = (total_rows.saturating_sub(panel_h)) / 2;

    for row in 0..total_rows {
        renderer.begin_row(row);
        for col in 0..cols {
            let in_panel =
                row >= top && row < top + panel_h && col >= left && col < left + panel_w;
            let (ch, fg, bg) = if in_panel {
                let prow = row - top;
                let pcol = col - left;
                if prow == 0 || prow == panel_h - 1 || pcol == 0 || pcol == panel_w - 1 {
                    (' ', text_fg, border_bg) // 1-cell accent border
                } else {
                    let li = prow - 1;
                    let line = lines.get(li).copied().unwrap_or("");
                    let tcol = pcol - 1; // 1-cell interior pad
                    let c = line.chars().nth(tcol).unwrap_or(' ');
                    let fg = if li == 0 { title_fg } else { text_fg };
                    (c, fg, panel_bg)
                }
            } else {
                (' ', backdrop, backdrop)
            };
            renderer.push_cell(col, row, ch, &[], fg, bg, false, false, false, Decorations::default());
        }
    }
}

/// Which mouse-button id to report for a pointer-motion event, or `None` to stay
/// silent. `held` is the currently pressed button (0/1/2) or `None`. Mirrors
/// xterm: any-motion mode (1003) reports even with no button (id 3); button-only
/// motion (1002) reports just while a button is held; click-only (1000) never
/// reports motion. Pure for unit testing.
fn motion_button(mode: TermMode, held: Option<u8>) -> Option<u8> {
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
const BLINK_INTERVAL: Duration = Duration::from_millis(530);

// --- Grapheme-cluster reconstruction across grid cells ----------------------
// A user-perceived character (extended grapheme cluster) can span several grid
// cells: a base emoji plus a ZWJ-joined emoji (flags, family, profession), a
// skin-tone modifier, a regional-indicator flag pair, or a variation selector.
// alacritty attaches zero-width code points to one cell but places *wide* joined
// emoji and modifiers in their own cells, so we re-stitch them here before
// shaping, otherwise compound emoji render as their separate components.

fn is_zwj(c: char) -> bool {
    c == '\u{200D}'
}
fn is_variation_selector(c: char) -> bool {
    c == '\u{FE0E}' || c == '\u{FE0F}'
}
fn is_emoji_modifier(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}
fn is_regional_indicator(c: char) -> bool {
    ('\u{1F1E6}'..='\u{1F1FF}').contains(&c)
}

/// Number of `cells` entries occupied by the cell unit at `start`: the cell plus
/// a following wide-character spacer, if any.
fn unit_len(cells: &[Indexed<&Cell>], start: usize) -> usize {
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
fn build_grapheme(cells: &[Indexed<&Cell>], start: usize, line: i32) -> (Vec<char>, usize) {
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
const FONT_STEP_PX: f32 = 2.0;

/// A runtime font-size adjustment requested via Ctrl +/-/0.
#[derive(Clone, Copy)]
enum FontStep {
    Inc,
    Dec,
    Reset,
}

/// Static configuration resolved at startup (config file + CLI overrides).
pub struct Config {
    /// Preferred font family name; `None` uses the discovery default (FiraCode…).
    pub font_family: Option<String>,
    /// Logical font size in points (scaled by the monitor's DPI factor).
    pub font_size: f32,
    /// Window background opacity in [0, 1]. 1.0 is fully opaque.
    pub opacity: f32,
    /// Padding (grid inset) override in logical px; `None` derives it from the
    /// cell height.
    pub padding: Option<f32>,
    /// Lines of scrollback history to retain.
    pub scrollback: usize,
    /// Shell program + args; `None` uses the user's default shell.
    pub shell: Option<Shell>,
    /// Flash the window briefly on the terminal bell. Default true.
    pub bell_visual: bool,
    /// Play a short soft beep on the terminal bell. Default false. Only audible in
    /// a build compiled with the `bell-audio` feature.
    pub bell_audible: bool,
    /// Canonical name of the active color theme (one of `color::THEME_NAMES`),
    /// tracked so the settings overlay can show, cycle, and save it.
    pub theme: String,
}

/// One terminal tab. The *active* tab's PTY lives directly in `App::pty` (so all
/// rendering/input code stays single-session); inactive tabs are parked here and
/// swapped in on switch.
struct Session {
    id: usize,
    pty: Pty,
    title: String,
    /// Set when this background tab produces output; shown as a dot on its chip
    /// and cleared when the tab is activated. Lets you see which tab is busy.
    activity: bool,
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    config: Config,

    // Created lazily in `resumed()` (winit requires the window there).
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    pty: Option<Pty>,

    // Tabs. The active tab's PTY is `pty`; inactive tabs are parked in
    // `background` (an unordered pool keyed by id). `tab_order` is the STABLE
    // left-to-right display order of all tab ids — switching tabs only moves the
    // highlight, it never reorders or renumbers (drag-reorder mutates this).
    background: Vec<Session>,
    /// Stable left-to-right display order of all tab ids (active + background).
    tab_order: Vec<usize>,
    /// Stable id of the active session.
    active_id: usize,
    /// Title reported by the active session (OSC), for the tab strip.
    active_title: String,
    /// Next session id to assign.
    next_id: usize,

    cols: usize,
    rows: usize,

    /// The configured font size in physical px, captured at startup; Ctrl 0 resets
    /// the runtime size to this. `None` until the window/renderer exist.
    base_font_px: Option<f32>,

    mods: ModifiersState,
    focused: bool,
    /// While a tab chip is held, its current stable position — set on press,
    /// updated as the pointer drags it over other slots (reorders `tab_order`),
    /// cleared on release. `None` when not dragging a tab.
    dragging_tab: Option<usize>,
    /// The toolbar item currently under the pointer, for hover highlighting.
    hovered_strip_item: Option<StripItem>,
    /// Accumulated scroll/swipe delta while the pointer is over the tab strip,
    /// so a touchpad 2-finger swipe (many small deltas) cycles tabs smoothly.
    tab_scroll_accum: f32,
    /// Accumulated touchpad pixel delta for terminal-content scrolling. Touchpads
    /// stream many sub-line deltas; accumulating avoids truncating each to zero.
    content_scroll_accum: f32,
    /// True once the current touchpad swipe over the strip has already switched a
    /// tab, so one continuous swipe moves exactly one tab (no twitchy carousel).
    /// Reset when the gesture starts/ends.
    swipe_consumed: bool,
    /// Wall-clock at App construction + whether the first frame has been timed,
    /// so we can log time-to-first-frame once (a startup benchmark).
    started: Instant,
    first_frame_done: bool,
    /// Whether the F1 help overlay is currently shown.
    help_open: bool,
    /// Whether the Ctrl+, settings overlay is currently shown.
    settings_open: bool,
    /// Selected adjustable row in the settings overlay (0=font, 1=opacity, 2=bell).
    settings_sel: usize,
    /// True briefly after a successful settings save, for the overlay's status line.
    settings_saved: bool,

    // Mouse reporting state.
    /// Last known cursor cell (col, row), clamped to the grid.
    mouse_cell: (usize, usize),
    /// Last raw cursor position in physical pixels (for sub-cell side tests).
    mouse_px: (f64, f64),
    /// Button currently held (for drag reports); base id 0=L/1=M/2=R.
    held_button: Option<u8>,
    /// True while the left button drives a glassy-side text selection (i.e. a
    /// press that started while NOT in mouse-reporting mode).
    selecting: bool,
    /// Click-chain state for double/triple click detection: (cell, count, time).
    last_click: Option<((usize, usize), u32, Instant)>,
    /// URI of the OSC8 hyperlink currently under the pointer (for hover underline).
    hovered_link: Option<String>,

    /// Lazily-created OS clipboard handle (arboard). `None` until first use, and
    /// stays `None` if the platform clipboard is unavailable.
    clipboard: Option<arboard::Clipboard>,

    // Render-on-demand throttle state.
    dirty: bool,
    next_frame: Instant,
    refresh: Duration,

    // Visual-bell flash state: when set, the renderer overlays a low-alpha flash
    // tint until this instant, then we restore. Driven by the render-on-demand
    // WaitUntil timer; cleared (back to ControlFlow::Wait) once elapsed.
    bell_flash_until: Option<Instant>,
    /// Audible-bell player (lazy; holds the audio device open after the first
    /// ring). A no-op when built without the `bell-audio` feature.
    audio_bell: AudioBell,

    // Cursor blink state. `blink_on` is the current visible phase; `blink_at` is
    // when the next phase flip is due. Blinking only runs while focused and the
    // child requested a blinking cursor; otherwise we stay on `ControlFlow::Wait`
    // (0% idle) and keep the cursor solid.
    blink_on: bool,
    blink_at: Instant,

    // Headless capture: when `GLASSY_CAPTURE` is set, render after a short delay
    // (so the shell has produced output), write a PPM, and exit.
    capture: Option<std::path::PathBuf>,
    capture_deadline: Option<Instant>,

    // --- Per-frame damage tracking (drives the renderer's per-row updates). ---
    /// Force a full grid rebuild on the next frame regardless of terminal damage.
    /// Set on resize / font change / first frame, where the per-row layout or all
    /// content changes at once.
    force_full_redraw: bool,
    /// The cursor cell (col, row) drawn last frame, so we can repaint the row it
    /// vacated (alacritty's own cursor damage covers the terminal cursor move, but
    /// glassy's blink/focus/selection overlays are not part of that damage).
    prev_cursor: Option<(usize, usize)>,
    /// Scrollback display offset rendered last frame; a change means the whole
    /// viewport scrolled and every row must be rebuilt.
    prev_display_offset: i32,
    /// Whether a text selection existed last frame. A selection spans arbitrary
    /// rows and is not part of terminal damage, so any change forces a full
    /// rebuild (selections only change during interactive drags, which are rare
    /// relative to streaming output).
    prev_has_selection: bool,
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>, config: Config) -> Self {
        Self {
            proxy,
            config,
            window: None,
            renderer: None,
            pty: None,
            background: Vec::new(),
            tab_order: vec![0], // the first tab (spawned in resumed) is id 0
            active_id: 0,
            active_title: String::new(),
            next_id: 1,
            cols: 0,
            rows: 0,
            base_font_px: None,
            mods: ModifiersState::empty(),
            focused: true,
            started: Instant::now(),
            first_frame_done: false,
            dragging_tab: None,
            hovered_strip_item: None,
            tab_scroll_accum: 0.0,
            content_scroll_accum: 0.0,
            swipe_consumed: false,
            help_open: false,
            settings_open: false,
            settings_sel: 0,
            settings_saved: false,
            mouse_cell: (0, 0),
            mouse_px: (0.0, 0.0),
            held_button: None,
            selecting: false,
            last_click: None,
            hovered_link: None,
            clipboard: None,
            bell_flash_until: None,
            audio_bell: AudioBell::new(),
            dirty: false,
            next_frame: Instant::now(),
            refresh: Duration::from_micros(16_666), // 60 Hz default until queried
            blink_on: true,
            blink_at: Instant::now() + BLINK_INTERVAL,
            capture: std::env::var_os("GLASSY_CAPTURE").map(std::path::PathBuf::from),
            capture_deadline: None,
            force_full_redraw: true,
            prev_cursor: None,
            prev_display_offset: 0,
            prev_has_selection: false,
        }
    }

    /// Compute grid dimensions for a physical surface size and the cell metrics.
    /// The renderer insets the grid by `pad` px on all four sides, so the usable
    /// area is reduced by `2 * pad` in each dimension.
    fn grid_for(size: PhysicalSize<u32>, cell_w: f32, cell_h: f32, pad: f32) -> (usize, usize) {
        let usable_w = (size.width as f32 - 2.0 * pad).max(0.0);
        let usable_h = (size.height as f32 - 2.0 * pad).max(0.0);
        let cols = ((usable_w / cell_w).floor() as usize).max(1);
        // Reserve the top row(s) for the tab strip; the terminal grid is the rest.
        let window_rows = (usable_h / cell_h).floor() as usize;
        let rows = window_rows.saturating_sub(TAB_STRIP_ROWS).max(1);
        (cols, rows)
    }

    /// The OSC8 hyperlink URI at a visible screen cell, if the cell carries one.
    fn cell_hyperlink(&self, col: usize, row: usize) -> Option<String> {
        let pty = self.pty.as_ref()?;
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let point = self.grid_point(col, row);
        let term = pty.term.lock();
        term.grid()[point].hyperlink().map(|h| h.uri().to_owned())
    }

    /// Open a URL with the system handler, detached. Restricted to web/file
    /// schemes so terminal output can't launch arbitrary URI handlers.
    fn open_url(url: &str) {
        if (url.starts_with("http://") || url.starts_with("https://") || url.starts_with("file://"))
            && let Err(e) = std::process::Command::new("xdg-open").arg(url).spawn()
        {
            log::warn!("failed to open {url}: {e}");
        }
    }

    /// Total number of open tabs (active + background).
    fn tab_count(&self) -> usize {
        self.background.len() + self.pty.is_some() as usize
    }

    /// Reflect the active tab + tab count in the window title.
    fn update_window_title(&self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        // Sanitize the OSC title before it reaches the native (CSD) titlebar:
        // strip control chars, zero-width / directional marks, and variation
        // selectors, which the titlebar font renders as a blank "tofu" box.
        let cleaned: String = self
            .active_title
            .chars()
            .filter(|&c| {
                !c.is_control()
                    && !('\u{200b}'..='\u{200f}').contains(&c)
                    && !('\u{fe00}'..='\u{fe0f}').contains(&c)
                    && c != '\u{feff}'
            })
            .collect();
        let base = cleaned.trim();
        let base = if base.is_empty() { "glassy" } else { base };
        let total = self.tab_count();
        if total > 1 {
            window.set_title(&format!("{base}  \u{00b7}  {total} tabs"));
        } else {
            window.set_title(base);
        }
    }

    /// Handle a click in the tab strip (screen row 0). Returns true if the click
    /// landed in the strip (and was consumed), so the caller skips selection/paste.
    /// Background tabs are hit-tested by equal-width slots; clicking the active
    /// tab's slot is a no-op.
    /// The toolbar item at strip column `col`, built from the live (stable-order)
    /// tab descriptors. Shared by click + drag-reorder so they agree with render.
    fn strip_item_at_col(&self, col: usize) -> Option<StripItem> {
        let descs: Vec<(String, bool, bool)> = self
            .tab_order
            .iter()
            .map(|&id| {
                if id == self.active_id {
                    (self.active_title.clone(), true, false)
                } else {
                    self.background
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| (s.title.clone(), false, s.activity))
                        .unwrap_or((String::new(), false, false))
                }
            })
            .collect();
        let refs: Vec<(&str, bool, bool)> =
            descs.iter().map(|(t, a, b)| (t.as_str(), *a, *b)).collect();
        strip_item_at(&strip_layout(&refs, self.cols), col)
    }

    /// While a tab is held (`dragging_tab`), reorder it under the pointer at strip
    /// column `col`: if the pointer is over a different tab slot, move the dragged
    /// tab there in `tab_order`. Returns true if a reorder happened (repaint).
    fn drag_tab_to(&mut self, col: usize) -> bool {
        let Some(from) = self.dragging_tab else {
            return false;
        };
        let to = match self.strip_item_at_col(col) {
            Some(StripItem::Tab(p)) | Some(StripItem::TabClose(p)) => p,
            _ => return false,
        };
        if to == from || from >= self.tab_order.len() || to >= self.tab_order.len() {
            return false;
        }
        move_in_order(&mut self.tab_order, from, to);
        self.dragging_tab = Some(to);
        true
    }

    fn strip_click(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let Some(renderer) = self.renderer.as_ref() else {
            return false;
        };
        let m = renderer.cell_metrics();
        let pad = renderer.pad() as f64;
        let (x, y) = self.mouse_px;
        let screen_row = ((y - pad) / m.height as f64).floor();
        if !(0.0..TAB_STRIP_ROWS as f64).contains(&screen_row) {
            return false;
        }
        // Hit-test against the actual toolbar layout (the same helper the renderer
        // uses, so click targets match what's drawn).
        let col = ((x - pad) / m.width as f64).floor().max(0.0) as usize;
        match self.strip_item_at_col(col) {
            Some(StripItem::Tab(pos)) => {
                self.activate_tab(pos, event_loop);
                // Begin a potential drag-to-reorder from this slot (the tab is now
                // active at `pos`); CursorMoved reorders, release ends it.
                self.dragging_tab = Some(self.active_pos());
            }
            Some(StripItem::TabClose(pos)) => self.close_tab(pos, event_loop),
            Some(StripItem::NewTab) => self.new_tab(event_loop),
            Some(StripItem::Help) => {
                self.help_open = !self.help_open;
                self.settings_open = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            Some(StripItem::Menu) => {
                self.settings_open = !self.settings_open;
                self.help_open = false;
                self.settings_sel = 0;
                self.settings_saved = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            None => {} // inert gap (native bar handles window drag)
        }
        true
    }

    /// Clear transient pointer/selection state. Called when switching tabs so an
    /// in-progress drag or hovered link from the old tab doesn't bleed into the new.
    fn reset_pointer_state(&mut self) {
        self.selecting = false;
        self.held_button = None;
        self.hovered_link = None;
        self.last_click = None;
        self.dragging_tab = None;
    }

    /// Open a new tab and make it active, parking the current tab in `background`.
    fn new_tab(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let m = renderer.cell_metrics();
        let id = self.next_id;
        let pty = match Pty::spawn(
            self.proxy.clone(),
            id,
            self.cols,
            self.rows,
            m.width.round() as u16,
            m.height.round() as u16,
            self.config.shell.clone(),
            None,
            self.config.scrollback,
        ) {
            Ok(p) => p,
            Err(e) => {
                log::error!("failed to spawn tab: {e:#}");
                return;
            }
        };
        self.next_id += 1;
        // Park the current active into the background pool, append the new tab to
        // the stable order, and make it active.
        if let Some(old) = self.pty.take() {
            self.background.push(Session {
                id: self.active_id,
                pty: old,
                title: std::mem::take(&mut self.active_title),
                activity: false,
            });
        }
        self.tab_order.push(id);
        self.pty = Some(pty);
        self.active_id = id;
        self.active_title.clear();
        self.reset_pointer_state();
        self.update_window_title();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Switch to the next/previous tab in the stable order (wrapping).
    fn cycle_tab(&mut self, delta: isize, event_loop: &ActiveEventLoop) {
        let n = self.tab_order.len();
        if n < 2 {
            return;
        }
        let pos = self.active_pos();
        let next = (((pos as isize + delta) % n as isize + n as isize) % n as isize) as usize;
        self.activate_tab(next, event_loop);
    }

    /// Move one tab in `tab_order` WITHOUT wrapping. A swipe gesture clamps at the
    /// first/last tab instead of spinning around like an infinite carousel.
    fn step_tab(&mut self, dir: isize, event_loop: &ActiveEventLoop) {
        let pos = self.active_pos();
        let next = pos as isize + dir;
        if next < 0 || next >= self.tab_order.len() as isize {
            return;
        }
        self.activate_tab(next as usize, event_loop);
    }

    /// Position of the active tab within `tab_order`.
    fn active_pos(&self) -> usize {
        self.tab_order
            .iter()
            .position(|&id| id == self.active_id)
            .unwrap_or(0)
    }

    /// Make the tab at stable position `pos` active. The display order is NOT
    /// changed — only the highlight moves. No-op if it's already active.
    fn activate_tab(&mut self, pos: usize, event_loop: &ActiveEventLoop) {
        let Some(&target_id) = self.tab_order.get(pos) else {
            return;
        };
        if target_id == self.active_id {
            return;
        }
        let Some(bi) = self.background.iter().position(|s| s.id == target_id) else {
            return;
        };
        // Park the current active, then swap in the target (clearing its activity).
        if let Some(cur) = self.pty.take() {
            self.background.push(Session {
                id: self.active_id,
                pty: cur,
                title: std::mem::take(&mut self.active_title),
                activity: false,
            });
        }
        let bi = self.background.iter().position(|s| s.id == target_id).unwrap_or(bi);
        let target = self.background.remove(bi);
        self.pty = Some(target.pty);
        self.active_id = target.id;
        self.active_title = target.title;
        self.reset_pointer_state();
        self.update_window_title();
        // A full repaint so the new tab's grid replaces the old one's persisted
        // rows (otherwise stale content from the other tab bleeds through).
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Close the active tab; activate the neighbor at its position, else exit.
    fn close_active_tab(&mut self, event_loop: &ActiveEventLoop) {
        self.close_tab(self.active_pos(), event_loop);
    }

    /// Close the tab at stable position `pos`. If it's the active tab, activate
    /// the neighbor that slides into its slot; if the last tab closes, exit.
    fn close_tab(&mut self, pos: usize, event_loop: &ActiveEventLoop) {
        let Some(&id) = self.tab_order.get(pos) else {
            return;
        };
        let was_active = id == self.active_id;
        self.tab_order.remove(pos);

        if was_active {
            if let Some(pty) = &self.pty {
                pty.shutdown();
            }
            self.pty = None;
            self.active_title.clear();
            if self.tab_order.is_empty() {
                event_loop.exit();
                return;
            }
            // Activate whatever tab now occupies the closed slot (clamped).
            let new_pos = pos.min(self.tab_order.len() - 1);
            let new_id = self.tab_order[new_pos];
            if let Some(bi) = self.background.iter().position(|s| s.id == new_id) {
                let next = self.background.remove(bi);
                self.pty = Some(next.pty);
                self.active_id = next.id;
                self.active_title = next.title;
            }
        } else if let Some(bi) = self.background.iter().position(|s| s.id == id) {
            // Closing a background tab: shut it down and drop it.
            let s = self.background.remove(bi);
            s.pty.shutdown();
        }
        self.reset_pointer_state();
        self.update_window_title();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Whether the cursor should currently blink: the child requested a blinking
    /// style and the cursor is not hidden. Returns false before the PTY is up.
    fn cursor_blinking(&self) -> bool {
        match self.pty.as_ref() {
            Some(pty) => {
                let term = pty.term.lock();
                term.cursor_style().blinking
                    && term.renderable_content().cursor.shape != CursorShape::Hidden
            }
            None => false,
        }
    }

    /// Reset the blink to its visible phase and restart the timer. Called on
    /// keypress so the cursor is solid while actively typing, matching every
    /// mainstream terminal.
    fn reset_blink(&mut self) {
        self.blink_on = true;
        self.blink_at = Instant::now() + BLINK_INTERVAL;
    }

    /// React to a terminal bell. The visual bell starts (or extends) a brief
    /// window flash; the audible bell rings a soft beep. Both are gated by config
    /// (default: visual on, audible off). `user_event` marks the screen dirty
    /// after this, so the flash paints on the next frame.
    fn trigger_bell(&mut self) {
        if self.config.bell_visual {
            // (Re)arm the flash window. A burst of bells just keeps it lit rather
            // than stuttering. Force a full rebuild so every cell picks up the
            // tint this frame and drops it when the flash ends.
            self.bell_flash_until = Some(Instant::now() + Duration::from_millis(bell::FLASH_MS));
            self.force_full_redraw = true;
        }
        if self.config.bell_audible {
            self.audio_bell.ring();
        }
    }

    /// Mark the screen dirty and schedule a redraw no sooner than `next_frame`.
    fn mark_dirty(&mut self, event_loop: &ActiveEventLoop) {
        self.dirty = true;
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }

    /// Translate a physical cursor position into a 0-based grid cell, clamped to
    /// the visible grid. The renderer insets the grid by `pad` px on all sides.
    fn px_to_cell(&self, x: f64, y: f64) -> (usize, usize) {
        let Some(renderer) = self.renderer.as_ref() else {
            return (0, 0);
        };
        let m = renderer.cell_metrics();
        let pad = renderer.pad() as f64;
        let col = ((x - pad) / m.width as f64).floor();
        // Screen rows include the tab strip; terminal rows start below it.
        let term_row = ((y - pad) / m.height as f64).floor() as i64 - TAB_STRIP_ROWS as i64;
        let col = (col.max(0.0) as usize).min(self.cols.saturating_sub(1));
        let row = (term_row.max(0) as usize).min(self.rows.saturating_sub(1));
        (col, row)
    }

    /// Snapshot of the terminal's current mode flags (mouse reporting, alt
    /// screen, etc.). Returns an empty set before the PTY is up.
    fn term_mode(&self) -> TermMode {
        match self.pty.as_ref() {
            Some(pty) => *pty.term.lock().mode(),
            None => TermMode::empty(),
        }
    }

    /// Encode and send a mouse report to the child, choosing SGR vs legacy form
    /// based on the terminal's current mode.
    fn report_mouse(&self, button: u8, pressed: bool, motion: bool, mode: TermMode) {
        let Some(pty) = self.pty.as_ref() else { return };
        let (col, row) = self.mouse_cell;
        let bytes = encode_mouse(
            MouseReport {
                button,
                col,
                row,
                pressed,
                motion,
            },
            self.mods,
            mode.contains(TermMode::SGR_MOUSE),
        );
        pty.write(bytes);
    }

    /// Which side (left/right half) of its cell a physical x-coordinate falls on.
    /// Selection uses this so the boundary cell is included or excluded based on
    /// where exactly the pointer is, matching every other terminal.
    fn cell_side(&self, x: f64) -> Side {
        let Some(renderer) = self.renderer.as_ref() else {
            return Side::Left;
        };
        let m = renderer.cell_metrics();
        let pad = renderer.pad() as f64;
        let rel = (x - pad) / m.width as f64;
        let frac = rel - rel.floor();
        if frac < 0.5 { Side::Left } else { Side::Right }
    }

    /// Convert a visible screen cell (col, row) to a grid `Point`. Screen rows
    /// map to grid lines by subtracting the scrollback display offset, the
    /// inverse of the `+ display_offset` used when rendering.
    fn grid_point(&self, col: usize, row: usize) -> Point {
        let display_offset = match self.pty.as_ref() {
            Some(pty) => pty.term.lock().grid().display_offset() as i32,
            None => 0,
        };
        Point::new(Line(row as i32 - display_offset), Column(col))
    }

    /// Begin a text selection at the current pointer location. `ty` selects the
    /// granularity (Simple for a single click, Semantic for double, Lines for
    /// triple).
    fn start_selection(&mut self, ty: SelectionType) {
        let Some(pty) = self.pty.as_ref() else { return };
        let (col, row) = self.mouse_cell;
        let point = self.grid_point(col, row);
        let side = self.cell_side(self.mouse_px.0);
        pty.term.lock().selection = Some(Selection::new(ty, point, side));
        self.selecting = true;
    }

    /// Extend the in-progress selection to the current pointer location.
    fn update_selection(&mut self) {
        let Some(pty) = self.pty.as_ref() else { return };
        let (col, row) = self.mouse_cell;
        let point = self.grid_point(col, row);
        let side = self.cell_side(self.mouse_px.0);
        let mut term = pty.term.lock();
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, side);
        }
    }

    /// Clear any active selection (e.g. a plain click away from it).
    fn clear_selection(&mut self) {
        if let Some(pty) = self.pty.as_ref() {
            pty.term.lock().selection = None;
        }
    }

    /// Copy the current selection to the OS clipboard.
    fn copy_selection(&mut self) {
        let text = match self.pty.as_ref() {
            Some(pty) => pty.term.lock().selection_to_string(),
            None => None,
        };
        let Some(text) = text else { return };
        if text.is_empty() {
            return;
        }
        let cb = self.clipboard();
        if let Some(cb) = cb
            && let Err(e) = cb.set_text(text)
        {
            log::debug!("clipboard copy failed: {e}");
        }
    }

    /// Paste the OS clipboard contents into the child, honoring bracketed paste.
    fn paste_clipboard(&mut self) {
        let bracketed = self.term_mode().contains(TermMode::BRACKETED_PASTE);
        let text = self.clipboard().and_then(|cb| match cb.get_text() {
            Ok(t) => Some(t),
            Err(e) => {
                log::debug!("clipboard paste failed: {e}");
                None
            }
        });
        if let (Some(text), Some(pty)) = (text, self.pty.as_ref()) {
            pty.term.lock().scroll_display(Scroll::Bottom);
            pty.paste(&text, bracketed);
        }
    }

    /// Lazily open the OS clipboard. Returns `None` if it is unavailable.
    fn clipboard(&mut self) -> Option<&mut arboard::Clipboard> {
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(cb) => self.clipboard = Some(cb),
                Err(e) => {
                    log::debug!("clipboard unavailable: {e}");
                    return None;
                }
            }
        }
        self.clipboard.as_mut()
    }

    fn render(&mut self) {
        // The OSC8 hyperlink under the pointer, underlined for affordance.
        // Captured before the renderer borrow.
        let hovered_link = self.hovered_link.clone();
        // Visual-bell overlay: while the flash window is open, tint the whole frame
        // toward the foreground color; otherwise clear it. Computed before the
        // renderer borrow so it can read `self.bell_flash_until`.
        let flash = if self.bell_flash_until.is_some_and(|t| Instant::now() < t) {
            // A soft accent tint rather than a stark white flash — a shell bell
            // (failed tab-completion, pager limits) fires often, so keep it gentle.
            Some(bell::FLASH_COLOR)
        } else {
            None
        };

        let (Some(renderer), Some(pty)) = (self.renderer.as_mut(), self.pty.as_ref()) else {
            return;
        };
        renderer.set_flash(flash);

        // Hold the terminal lock only long enough to copy out renderable state.
        let mut term = pty.term.lock();

        // Collect the terminal's per-line damage BEFORE reading renderable
        // content. `damage()` borrows `term` mutably and reports which viewport
        // rows changed since the last frame (it also damages the previous and
        // current terminal-cursor rows). We translate that into a per-row dirty
        // mask; rows not marked are reused from the renderer's persistent storage.
        // `TermDamage::Full` (entered insert mode, scrollback scroll, reset, etc.)
        // forces a full rebuild. After reading we must `reset_damage()`.
        let rows = self.rows;
        let mut dirty = vec![false; rows];
        let mut full = self.force_full_redraw;
        match term.damage() {
            alacritty_terminal::term::TermDamage::Full => full = true,
            alacritty_terminal::term::TermDamage::Partial(it) => {
                for line in it {
                    if line.line < rows {
                        dirty[line.line] = true;
                    }
                }
            }
        }
        term.reset_damage();

        // The child's requested cursor style (shape is also mirrored in
        // `content.cursor.shape`); `blinking` decides whether the blink timer runs.
        // Read before `renderable_content()` so both immutable borrows of `term`
        // can coexist with the content's `display_iter`.
        let cursor_blinking = term.cursor_style().blinking;
        let content = term.renderable_content();
        let colors = content.colors;
        let display_offset = content.display_offset as i32;
        let cursor = content.cursor;
        let selection = content.selection;
        let cursor_color = color::resolve(Color::Named(NamedColor::Cursor), colors);

        // The cursor is suppressed only when Hidden, or mid-blink off-phase. It is
        // still drawn (as a hollow outline) when the window is unfocused. The block
        // shape inverts the cell beneath it; all other shapes are overlay rects.
        let blink_off = self.focused && cursor_blinking && !self.blink_on;
        let cursor_shown = cursor.shape != CursorShape::Hidden && !blink_off;
        let cursor_row = cursor.point.line.0 + display_offset;
        let cursor_col = cursor.point.column.0 as i32;
        // A focused, on-phase block cursor inverts its cell in the loop below;
        // every other drawn case is an overlay pushed after the cells.
        let invert_block = cursor_shown && self.focused && cursor.shape == CursorShape::Block;

        // --- Decide what to rebuild this frame. ---
        // A scrollback scroll moves every row; alacritty reports Full for it, but
        // guard explicitly too. A selection spans arbitrary rows and is not part
        // of terminal damage, so any change forces a full rebuild.
        let has_selection = selection.is_some();
        if display_offset != self.prev_display_offset
            || has_selection != self.prev_has_selection
            || (has_selection && self.prev_has_selection)
        {
            full = true;
        }
        // glassy's cursor overlay/invert (and its blink/focus state) is not part of
        // alacritty's damage, so always repaint the row the cursor sits on and the
        // row it occupied last frame. (alacritty damages the terminal-cursor rows
        // itself, but blink phase flips and focus changes produce no damage.)
        let cur_cursor_cell = if cursor_shown
            && cursor_row >= 0
            && cursor_row < rows as i32
            && cursor_col >= 0
            && cursor_col < self.cols as i32
        {
            Some((cursor_col as usize, cursor_row as usize))
        } else {
            None
        };
        if let Some((_, r)) = cur_cursor_cell.filter(|&(_, r)| r < rows) {
            dirty[r] = true;
        }
        if let Some((_, r)) = self.prev_cursor.filter(|&(_, r)| r < rows) {
            dirty[r] = true;
        }

        renderer.begin_frame(color::default_bg());

        // The renderer keeps per-row instances persistently; on a full rebuild we
        // clear/resize that storage so every row is repushed below.
        if full {
            // +strip: the renderer grid spans the whole window (strip row + terminal).
            renderer.resize_grid(rows + TAB_STRIP_ROWS);
            for d in dirty.iter_mut() {
                *d = true;
            }
        }

        // Track which *screen* rows we have begun this frame (terminal content is
        // offset down by the strip), so a row's first cell triggers `begin_row` and
        // the cursor overlay can re-target it later.
        let mut row_started = vec![false; rows + TAB_STRIP_ROWS];

        // Collect the visible cells so we can look ahead across cells when
        // reconstructing grapheme clusters (compound emoji span several cells).
        let cells: Vec<_> = content.display_iter.collect();
        let mut ci = 0;
        while ci < cells.len() {
            let indexed = &cells[ci];
            let cell = indexed.cell;

            // The right half of a wide character is a spacer; a base cell consumes
            // its own spacer below, so any spacer reached here is stray — skip it.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                ci += 1;
                continue;
            }

            let row = indexed.point.line.0 + display_offset;
            let col = indexed.point.column.0 as i32;
            if row < 0 || row >= self.rows as i32 || col < 0 || col >= self.cols as i32 {
                ci += 1;
                continue;
            }
            let row_u = row as usize;
            // Skip cells in rows that did not change: their instances are reused.
            if !dirty[row_u] {
                ci += unit_len(&cells, ci);
                continue;
            }
            // Screen row = terminal row + strip offset. First cell of a dirty row:
            // clear it and begin pushing into it.
            let srow = row_u + TAB_STRIP_ROWS;
            if !row_started[srow] {
                renderer.begin_row(srow);
                row_started[srow] = true;
            }

            let mut fg = color::resolve(cell.fg, colors);
            let mut bg = color::resolve(cell.bg, colors);

            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = [fg[0] * 0.66, fg[1] * 0.66, fg[2] * 0.66, fg[3]];
            }
            // Tint selected cells. Done before the cursor inversion so the cursor
            // still reads clearly when it sits inside the selection.
            if selection.is_some_and(|range| range.contains(indexed.point)) {
                bg = color::selection_bg();
            }
            // Block cursor: invert the cell beneath it.
            if invert_block && row == cursor_row && col == cursor_col {
                std::mem::swap(&mut fg, &mut bg);
            }
            let hidden = cell.flags.contains(Flags::HIDDEN);

            let bold = cell.flags.contains(Flags::BOLD) || cell.flags.contains(Flags::BOLD_ITALIC);
            let italic =
                cell.flags.contains(Flags::ITALIC) || cell.flags.contains(Flags::BOLD_ITALIC);
            // A double-width (CJK / wide-emoji) cell spans two columns; the
            // trailing spacer column is skipped above. The renderer lays the
            // glyph out across the full two-cell box when this is set.
            let wide = cell.flags.contains(Flags::WIDE_CHAR);

            // Text decorations. Hidden cells draw nothing, so suppress strokes
            // too. Underline styles are mutually exclusive (latest SGR wins);
            // map the cell flags to a single style. The decoration color is the
            // SGR 58 underline color when set, else the cell foreground, so e.g.
            // a red LSP curl sits under default-fg text.
            let mut decorations = if hidden {
                Decorations::default()
            } else {
                let underline = if cell.flags.contains(Flags::UNDERCURL) {
                    UnderlineStyle::Curl
                } else if cell.flags.contains(Flags::DOTTED_UNDERLINE) {
                    UnderlineStyle::Dotted
                } else if cell.flags.contains(Flags::DASHED_UNDERLINE) {
                    UnderlineStyle::Dashed
                } else if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
                    UnderlineStyle::Double
                } else if cell.flags.contains(Flags::UNDERLINE) {
                    UnderlineStyle::Single
                } else {
                    UnderlineStyle::None
                };
                let color = cell
                    .underline_color()
                    .map(|c| color::resolve(c, colors))
                    .unwrap_or(fg);
                Decorations {
                    underline,
                    strikeout: cell.flags.contains(Flags::STRIKEOUT),
                    color,
                }
            };

            // Underline the hovered hyperlink's cells (only when not already
            // underlined by the app), as a click affordance.
            if !hidden
                && matches!(decorations.underline, UnderlineStyle::None)
                && let Some(ref hov) = hovered_link
                && cell.hyperlink().is_some_and(|h| h.uri() == hov)
            {
                decorations.underline = UnderlineStyle::Single;
            }

            let ch = if hidden || cell.c == '\0' {
                ' '
            } else {
                cell.c
            };
            // Reconstruct the grapheme cluster, merging this cell's combining /
            // ZWJ code points with any following cells joined by ZWJ, a skin-tone
            // modifier, a regional-indicator pair, or a variation selector — so
            // compound emoji (flags, families, professions) shape into one glyph.
            let (combiners, consumed) = if hidden {
                (Vec::new(), unit_len(&cells, ci))
            } else {
                build_grapheme(&cells, ci, indexed.point.line.0)
            };
            // A cluster that spans 2+ grid cells (a wide CJK char, but also an
            // emoji whose base code point is *narrow* yet joins following cells —
            // e.g. the trans flag 🏳️‍⚧️ = narrow white-flag + ZWJ + symbol) must get
            // a 2-cell box so its color glyph fills the space instead of being
            // squished into one cell.
            let wide = wide || consumed >= 2;
            renderer.push_cell(
                col as usize,
                srow,
                ch,
                &combiners,
                fg,
                bg,
                bold,
                italic,
                wide,
                decorations,
            );
            ci += consumed;
        }

        // --- Inline app toolbar (screen row 0): sits UNDER the native OS titlebar
        // (winit decorations own min/max/close + window drag). Holds the glassy
        // mark, tab chips (or the title with one tab), a + new-tab button, and
        // right-aligned help / menu buttons. Drawn from `strip_layout` so the
        // click hit-test matches exactly. ---
        {
            let bar_bg = color::selection_bg();
            let base_fg = color::default_fg();
            let base_bg = color::default_bg();
            let dim_fg = [base_fg[0] * 0.55, base_fg[1] * 0.55, base_fg[2] * 0.55, base_fg[3]];
            let accent = [0.45, 0.68, 1.0, 1.0];
            // Raised chip surfaces: idle chips sit slightly above the bar, the
            // active chip is an accent fill, hovered chips brighten — giving the
            // inline strip tactile "chips" instead of a flat run of text.
            let chip_idle = lighten(bar_bg, 0.05);
            let chip_hover = lighten(bar_bg, 0.13);
            let active_bg = accent;
            let active_fg = base_bg; // dark text on the accent chip

            // The toolbar's own backdrop is the terminal bg (so the chips read as
            // raised surfaces against it), with a 1px-feel accent on the mark.
            let mut bar: Vec<(char, [f32; 4], [f32; 4])> = vec![(' ', base_fg, bar_bg); self.cols];
            let put = |bar: &mut Vec<(char, [f32; 4], [f32; 4])>, start: usize, s: &str, fg, bg| {
                for (k, ch) in s.chars().enumerate() {
                    if let Some(cell) = bar.get_mut(start + k) {
                        *cell = (ch, fg, bg);
                    }
                }
            };
            put(&mut bar, 0, " ◆ ", accent, bar_bg);

            // Tab descriptors in STABLE order: the active tab keeps its position,
            // only the highlight follows it. (Disjoint fields from the renderer
            // borrow, so this is allowed alongside it.)
            let descs: Vec<(&str, bool, bool)> = self
                .tab_order
                .iter()
                .map(|&id| {
                    if id == self.active_id {
                        (self.active_title.as_str(), true, false)
                    } else {
                        self.background
                            .iter()
                            .find(|s| s.id == id)
                            .map(|s| (s.title.as_str(), false, s.activity))
                            .unwrap_or(("", false, false))
                    }
                })
                .collect();
            let multi = descs.len() > 1;
            let hov = self.hovered_strip_item;
            for seg in strip_layout(&descs, self.cols) {
                let is_active = matches!(seg.item, StripItem::Tab(i) | StripItem::TabClose(i) if descs.get(i).is_some_and(|d| d.1));
                let is_busy = matches!(seg.item, StripItem::Tab(i) if descs.get(i).is_some_and(|d| d.2));
                let hovered = hov == Some(seg.item);
                let (fg, sbg) = match seg.item {
                    _ if is_active => (active_fg, active_bg), // accent-filled active chip
                    StripItem::Tab(_) if !multi => (base_fg, bar_bg), // single-tab title (no chip)
                    StripItem::Tab(_) => (
                        if is_busy { accent } else { base_fg },
                        if hovered { chip_hover } else { chip_idle },
                    ),
                    StripItem::TabClose(_) => (
                        if hovered { [0.95, 0.45, 0.45, 1.0] } else { dim_fg },
                        if hovered { chip_hover } else { chip_idle },
                    ),
                    StripItem::NewTab => (accent, if hovered { chip_hover } else { bar_bg }),
                    StripItem::Help | StripItem::Menu => (
                        if hovered { base_fg } else { dim_fg },
                        if hovered { chip_hover } else { bar_bg },
                    ),
                };
                put(&mut bar, seg.start, &seg.label, fg, sbg);
            }

            // Scrollback-position indicator, tucked in left of the buttons.
            if display_offset > 0 {
                let s = format!("⇡ {display_offset} ");
                let n = s.chars().count();
                let right_w = STRIP_BTN_W * 2;
                if self.cols > right_w + n + 2 {
                    put(&mut bar, self.cols - right_w - n, &s, accent, bar_bg);
                }
            }

            renderer.begin_row(0);
            for (col, (ch, cfg, cbg)) in bar.into_iter().enumerate() {
                renderer.push_cell(
                    col,
                    0,
                    ch,
                    &[],
                    cfg,
                    cbg,
                    false,
                    false,
                    false,
                    Decorations::default(),
                );
            }
        }

        // Cursor overlay. Drawn after the cells so the bars/outline land on top of
        // the cell background (glyphs still paint over them in the fg pass). The
        // overlay is in addition to — or instead of — the block invert above:
        //   - focused Block: handled by the invert; no overlay.
        //   - focused Beam/Underline: an fg-colored bar.
        //   - focused HollowBlock: an outline box.
        //   - unfocused (any non-hidden shape): an outline box, so an idle window
        //     still shows where the cursor is.
        if let Some((cc, cr)) = cur_cursor_cell {
            let overlay = if !self.focused {
                Some(CursorOverlay::Hollow)
            } else {
                match cursor.shape {
                    CursorShape::Beam => Some(CursorOverlay::Beam),
                    CursorShape::Underline => Some(CursorOverlay::Underline),
                    CursorShape::HollowBlock => Some(CursorOverlay::Hollow),
                    // Block is drawn by the cell invert; Hidden is unreachable here.
                    CursorShape::Block | CursorShape::Hidden => None,
                }
            };
            if let Some(overlay) = overlay {
                // The cursor row was (re)built above; re-target it without clearing
                // so the overlay appends on top of that row's cell backgrounds.
                let scr = cr + TAB_STRIP_ROWS;
                if cr < rows && row_started[scr] {
                    renderer.set_cur_row(scr);
                    renderer.push_cursor(cc, scr, overlay, cursor_color);
                }
            }
        }

        drop(term); // release before GPU submit / present

        // Inline images (kitty graphics). Drawn as an overlay every frame from the
        // live placement list, anchored to the cell they were displayed at. The
        // stored row is viewport-relative at display time; translate by the current
        // scroll offset so images move with the buffer as the user scrolls.
        // Suppressed while a modal is up so images don't punch through it.
        if !self.help_open && !self.settings_open {
            let store = pty.images.lock();
            if !store.placements().is_empty() {
                let m = renderer.cell_metrics();
                let pad = renderer.pad();
                for p in store.placements() {
                    let Some(img) = store.image(p.id) else { continue };
                    let screen_vp = p.row - display_offset;
                    if screen_vp < 0 || screen_vp >= rows as i32 || p.col >= self.cols {
                        continue;
                    }
                    let screen_row = screen_vp as usize + TAB_STRIP_ROWS;
                    let x = p.col as f32 * m.width + pad;
                    let y = screen_row as f32 * m.height + pad;
                    // Honor the kitty c=/r= display size (in cells); otherwise draw
                    // at the image's native pixel size.
                    let (dst_w, dst_h) =
                        image_dst_size(p.cols, p.rows, img.width, img.height, m.width, m.height);
                    renderer.draw_image(p.id, &img.rgba, img.width, img.height, x, y, dst_w, dst_h);
                }
            }
        }

        // Modal overlays (help / settings): centered panels over a dimmed backdrop.
        // Rebuild every screen row (cheap — only while open, and the screen is
        // static), replacing terminal content so nothing bleeds through. Settings
        // wins if both are somehow set.
        if self.settings_open {
            // `&self.config` is a disjoint field borrow, so it coexists with the
            // live `renderer` (self.renderer) mutable borrow.
            let lines =
                settings_lines(&self.config, renderer.font_px(), self.settings_sel, self.settings_saved);
            let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
            draw_modal(renderer, self.rows, self.cols, &refs);
        } else if self.help_open {
            draw_modal(renderer, self.rows, self.cols, HELP_LINES);
        }

        // Record the state this frame drew from, so the next frame can repaint only
        // what changed (the cursor's old/new row, selection, scroll position).
        self.prev_cursor = cur_cursor_cell;
        self.prev_display_offset = display_offset;
        self.prev_has_selection = has_selection;
        self.force_full_redraw = false;

        // The renderer self-heals lost/outdated surfaces internally. If a frame is
        // dropped (e.g. transient surface loss), the damage we consumed + the rows
        // we built may not have reached the GPU, so re-arm a full rebuild and ask
        // for another frame — otherwise that content stays missing until the next
        // resize. (Root cause of the "blank until you resize" reports.)
        if let Err(err) = renderer.render() {
            log::debug!("frame dropped, forcing full repaint next frame: {err:?}");
            self.force_full_redraw = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }

        // Startup benchmark: log time-to-first-frame once.
        if !self.first_frame_done {
            self.first_frame_done = true;
            log::info!(
                "glassy time-to-first-frame: {:.1} ms",
                self.started.elapsed().as_secs_f64() * 1000.0
            );
        }

        // If the glyph atlas overflowed and was repacked this frame, every cached
        // glyph's UVs changed; persisted rows now hold stale UVs. Force a full
        // rebuild and schedule one more frame to repaint cleanly.
        if renderer.pull_atlas_reset() {
            self.force_full_redraw = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }

        self.dirty = false;
    }

    /// Handle a keypress while the settings overlay is open: arrow-key navigation
    /// + adjustment, Enter/`s` to save. Other keys are consumed (ignored).
    fn handle_settings_key(&mut self, key: Key, event_loop: &ActiveEventLoop) {
        const ROWS: usize = 4; // font, opacity, bell, theme
        match key {
            Key::Named(NamedKey::ArrowUp) => {
                self.settings_sel = (self.settings_sel + ROWS - 1) % ROWS;
                self.settings_saved = false;
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.settings_sel = (self.settings_sel + 1) % ROWS;
                self.settings_saved = false;
            }
            Key::Named(NamedKey::ArrowLeft) => self.adjust_setting(-1),
            Key::Named(NamedKey::ArrowRight) => self.adjust_setting(1),
            Key::Named(NamedKey::Enter) => self.save_settings(),
            Key::Character(ref s) if s.as_str() == "s" => self.save_settings(),
            _ => return,
        }
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Adjust the selected setting by `dir` (-1/+1). Font + opacity apply live;
    /// bell toggles. None persist until [`App::save_settings`].
    fn adjust_setting(&mut self, dir: i32) {
        self.settings_saved = false;
        match self.settings_sel {
            0 => self.resize_font(if dir > 0 { FontStep::Inc } else { FontStep::Dec }),
            1 => {
                let o = (self.config.opacity + dir as f32 * 0.05).clamp(0.0, 1.0);
                self.config.opacity = o;
                if let Some(r) = self.renderer.as_mut() {
                    r.set_opacity(o);
                }
            }
            2 => self.config.bell_visual = !self.config.bell_visual,
            3 => self.cycle_theme(dir),
            _ => {}
        }
    }

    /// Cycle the active theme by `dir` through `color::THEME_NAMES`, applying it
    /// live (swap the global theme + full redraw).
    fn cycle_theme(&mut self, dir: i32) {
        let names = color::THEME_NAMES;
        let cur = names.iter().position(|&n| n == self.config.theme).unwrap_or(0);
        let next = (cur as i32 + dir).rem_euclid(names.len() as i32) as usize;
        let name = names[next];
        if let Some(theme) = color::theme_by_name(name) {
            color::set_theme(theme);
            self.config.theme = name.to_string();
            // The renderer reads theme colors fresh each frame; a full rebuild
            // repaints every cell + the clear color in the new palette.
            self.force_full_redraw = true;
        }
    }

    /// Persist the live-adjustable settings (font size in pt, opacity, bell) to
    /// the config file, preserving every other key/comment.
    fn save_settings(&mut self) {
        let scale = self
            .window
            .as_ref()
            .map(|w| w.scale_factor() as f32)
            .unwrap_or(1.0)
            .max(0.1);
        let px = self
            .renderer
            .as_ref()
            .map(|r| r.font_px())
            .unwrap_or(self.config.font_size);
        let pt = (px / scale).max(1.0);
        let updates = [
            ("font_size", format!("{pt:.0}")),
            ("opacity", format!("{:.2}", self.config.opacity)),
            ("bell_visual", self.config.bell_visual.to_string()),
            ("theme", self.config.theme.clone()),
        ];
        match crate::config::save(&updates) {
            Ok(()) => {
                self.settings_saved = true;
                log::info!("settings saved to config");
            }
            Err(e) => log::error!("settings save failed: {e:#}"),
        }
    }

    /// Apply a runtime font-size change (Ctrl +/-/0): reload the font in the
    /// renderer, recompute the grid for the new cell box + padding, and resize the
    /// PTY. A no-op before the renderer/PTY exist.
    fn resize_font(&mut self, step: FontStep) {
        let (Some(renderer), Some(pty)) = (self.renderer.as_mut(), self.pty.as_ref()) else {
            return;
        };
        let target = match step {
            FontStep::Inc => renderer.font_px() + FONT_STEP_PX,
            FontStep::Dec => renderer.font_px() - FONT_STEP_PX,
            FontStep::Reset => self.base_font_px.unwrap_or_else(|| renderer.font_px()),
        };
        renderer.set_font_size(target);

        // Recompute the grid for the new cell metrics + padding against the
        // current surface, and inform the PTY.
        if let Some(window) = self.window.as_ref() {
            let size = window.inner_size();
            let m = renderer.cell_metrics();
            let (cols, rows) = Self::grid_for(size, m.width, m.height, renderer.pad());
            self.cols = cols;
            self.rows = rows;
            pty.resize(cols, rows, m.width.round() as u16, m.height.round() as u16);
        }
        // The cell box changed, so every glyph position and the per-row storage
        // must be rebuilt next frame.
        self.force_full_redraw = true;
    }

    /// The earliest timed wakeup we must schedule when otherwise idle: the blink
    /// phase boundary and/or the visual-bell flash deadline, whichever is sooner.
    /// `None` means nothing is pending and the loop can park on `ControlFlow::Wait`
    /// (0% idle).
    fn next_wake(&self, blink_active: bool, flash_active: bool) -> Option<Instant> {
        let blink = blink_active.then_some(self.blink_at);
        let flash = flash_active.then_some(self.bell_flash_until).flatten();
        match (blink, flash) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn handle_resize(&mut self, event_loop: &ActiveEventLoop, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        let (Some(renderer), Some(pty)) = (self.renderer.as_mut(), self.pty.as_ref()) else {
            return;
        };
        renderer.resize(size.width, size.height);
        let m = renderer.cell_metrics();
        let (cols, rows) = Self::grid_for(size, m.width, m.height, renderer.pad());
        if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            let (cw, ch) = (m.width.round() as u16, m.height.round() as u16);
            pty.resize(cols, rows, cw, ch);
            // Keep background tabs in sync so switching to one shows correct layout.
            for s in &self.background {
                s.pty.resize(cols, rows, cw, ch);
            }
        }
        // Reproject + repaint the whole grid against the new surface; the per-row
        // storage is resized to match in the next frame's full rebuild.
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return; // resumed can fire repeatedly; init exactly once
        }

        let attrs = Window::default_attributes()
            .with_title("glassy")
            .with_inner_size(LogicalSize::new(960.0, 600.0))
            // Request a translucent window (the "glassy" namesake). The renderer
            // drives the backdrop alpha from its configured opacity when the
            // compositor supports a transparent surface; on platforms that don't,
            // this is a harmless no-op and the window stays opaque.
            .with_transparent(true)
            .with_visible(false); // shown after the first frame to avoid a flash
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };
        window.set_ime_allowed(true);
        let ms = |t: Instant| t.elapsed().as_secs_f64() * 1000.0;
        log::info!("startup: window created at {:.1} ms", ms(self.started));

        // Query the monitor refresh rate for the frame-coalescing throttle.
        if let Some(hz) = window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
            && hz > 0
        {
            self.refresh = Duration::from_secs_f64(1000.0 / hz as f64);
        }

        let scale = window.scale_factor() as f32;
        let font_px = self.config.font_size * scale;
        self.base_font_px = Some(font_px);

        let mut renderer = match Renderer::new(
            window.clone(),
            self.config.font_family.clone(),
            font_px,
            self.config.opacity,
        ) {
            Ok(r) => r,
            Err(e) => {
                log::error!("failed to initialize renderer: {e:#}");
                event_loop.exit();
                return;
            }
        };
        log::info!("startup: renderer+GPU+font ready at {:.1} ms", ms(self.started));
        // Apply an explicit padding override (logical px scaled to physical).
        if let Some(pad) = self.config.padding {
            renderer.set_pad(pad * scale);
        }

        let size = window.inner_size();
        renderer.resize(size.width, size.height);
        let m = renderer.cell_metrics();
        let (cols, rows) = Self::grid_for(size, m.width, m.height, renderer.pad());
        self.cols = cols;
        self.rows = rows;

        let pty = match Pty::spawn(
            self.proxy.clone(),
            0,
            cols,
            rows,
            m.width.round() as u16,
            m.height.round() as u16,
            self.config.shell.clone(),
            None,
            self.config.scrollback,
        ) {
            Ok(p) => p,
            Err(e) => {
                log::error!("failed to spawn shell: {e:#}");
                event_loop.exit();
                return;
            }
        };

        log::info!("startup: shell spawned at {:.1} ms", ms(self.started));
        self.window = Some(window);
        self.renderer = Some(renderer);
        self.pty = Some(pty);

        // Headless input/resize harness (used with GLASSY_CAPTURE for autonomous
        // verification of the custom PTY loop's write + resize paths):
        //   GLASSY_INPUT  - bytes to write through the real input channel; `\n`
        //                   and `\t` escapes are honored. Exercises the loop's
        //                   `write_all` on the blocking master fd round-trip.
        //   GLASSY_RESIZE - "COLSxROWS" to drive a grid resize (LoopMsg::Resize
        //                   -> on_resize) before the capture deadline.
        if let Some(pty) = &self.pty {
            if let Ok(spec) = std::env::var("GLASSY_RESIZE")
                && let Some((c, r)) = spec.split_once('x')
                && let (Ok(cols), Ok(rows)) = (c.parse::<usize>(), r.parse::<usize>())
            {
                let m = self.renderer.as_ref().unwrap().cell_metrics();
                pty.resize(cols, rows, m.width as u16, m.height as u16);
                self.cols = cols;
                self.rows = rows;
                self.force_full_redraw = true;
            }
            if let Ok(input) = std::env::var("GLASSY_INPUT") {
                let bytes = input.replace("\\n", "\n").replace("\\t", "\t").into_bytes();
                pty.write(bytes);
            }
        }
        // Headless: open an overlay at startup for capture verification.
        if std::env::var_os("GLASSY_HELP").is_some() {
            self.help_open = true;
            self.force_full_redraw = true;
        }
        if std::env::var_os("GLASSY_SETTINGS").is_some() {
            self.settings_open = true;
            self.force_full_redraw = true;
        }
        // Headless: open N tabs at startup to capture the multi-tab toolbar.
        if let Ok(n) = std::env::var("GLASSY_TABS")
            && let Ok(n) = n.parse::<usize>()
        {
            for _ in 1..n.min(12) {
                self.new_tab(event_loop);
            }
        }

        // Draw the first frame, then reveal the window (avoids a white flash).
        self.next_frame = Instant::now();
        self.render();
        if let Some(window) = &self.window {
            window.set_visible(true);
        }

        if self.capture.is_some() {
            // Delay before capturing so the shell + prompt (e.g. zsh + starship)
            // have time to initialize. Override with GLASSY_CAPTURE_MS.
            let ms: u64 = std::env::var("GLASSY_CAPTURE_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(700);
            let deadline = Instant::now() + Duration::from_millis(ms);
            self.capture_deadline = Some(deadline);
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Title(id, title) => {
                if id == self.active_id {
                    self.active_title = title;
                    self.update_window_title();
                } else if let Some(s) = self.background.iter_mut().find(|s| s.id == id) {
                    s.title = title;
                }
            }
            UserEvent::ChildExit(id) => {
                if id == self.active_id {
                    self.close_active_tab(event_loop);
                } else {
                    self.background.retain(|s| s.id != id);
                    self.update_window_title();
                }
                return;
            }
            UserEvent::Bell(id) => {
                if id == self.active_id {
                    self.trigger_bell();
                }
            }
            // A background tab produced output: its terminal state updated
            // silently; no redraw needed until it becomes active.
            UserEvent::Wakeup(id) => {
                if id != self.active_id {
                    // A background tab produced output: flag it for the header
                    // activity dot. Only repaint on the false->true edge so a busy
                    // background tab (e.g. a build) doesn't spam redraws.
                    if let Some(s) = self.background.iter_mut().find(|s| s.id == id)
                        && !s.activity
                    {
                        s.activity = true;
                        self.mark_dirty(event_loop);
                    }
                    return;
                }
            }
            UserEvent::PtyWrite(id, text) => {
                // Route the VT reply back to the session that produced it (active
                // or a background tab); not a visual change, so no repaint.
                let bytes = text.into_bytes();
                if id == self.active_id {
                    if let Some(pty) = &self.pty {
                        pty.write(bytes);
                    }
                } else if let Some(s) = self.background.iter().find(|s| s.id == id) {
                    s.pty.write(bytes);
                }
                return;
            }
        }
        self.mark_dirty(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if let Some(pty) = &self.pty {
                    pty.shutdown();
                }
                event_loop.exit();
            }
            WindowEvent::Focused(focused) => {
                self.focused = focused;
                // Restart the blink solid-on so a freshly-focused window shows the
                // cursor immediately; the cadence resumes from about_to_wait.
                self.reset_blink();
                self.mark_dirty(event_loop);
            }
            WindowEvent::ThemeChanged(_) => {
                // The system light/dark color-scheme changed at runtime. Repaint so
                // winit's client-side decorations (sctk-adwaita titlebar) re-theme to
                // match — previously glassy only picked it up at launch.
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            WindowEvent::ModifiersChanged(mods) => {
                self.mods = mods.state();
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic,
                ..
            } => {
                // Synthetic events are injected on focus change for held keys.
                if is_synthetic {
                    return;
                }

                // Ctrl+Shift clipboard combos are consumed by glassy and never
                // reach the child. Intercepted before `encode_key` so the control
                // byte for C/V isn't sent to the PTY.
                if event.state.is_pressed()
                    && self.mods.control_key()
                    && self.mods.shift_key()
                    && let Key::Character(s) = &event.logical_key
                {
                    match s.as_str() {
                        "C" | "c" => {
                            self.copy_selection();
                            return;
                        }
                        "V" | "v" => {
                            self.paste_clipboard();
                            self.mark_dirty(event_loop);
                            return;
                        }
                        "T" | "t" => {
                            self.new_tab(event_loop);
                            return;
                        }
                        "W" | "w" => {
                            self.close_active_tab(event_loop);
                            return;
                        }
                        _ => {}
                    }
                }

                // Ctrl+Tab / Ctrl+Shift+Tab cycle between tabs.
                if event.state.is_pressed()
                    && self.mods.control_key()
                    && let Key::Named(NamedKey::Tab) = &event.logical_key
                {
                    let delta = if self.mods.shift_key() { -1 } else { 1 };
                    self.cycle_tab(delta, event_loop);
                    return;
                }

                // Ctrl +/-/0 adjusts the font size at runtime (and Ctrl 0 resets
                // to the configured size). Intercepted before `encode_key` so the
                // control bytes for these keys never reach the child. Matches the
                // de-facto terminal/browser zoom convention. Shift is allowed (so
                // Ctrl+Shift+'=' i.e. Ctrl+'+' works) but not required.
                if event.state.is_pressed()
                    && self.mods.control_key()
                    && !self.mods.alt_key()
                    && let Key::Character(s) = &event.logical_key
                {
                    let step = match s.as_str() {
                        "+" | "=" => Some(FontStep::Inc),
                        "-" | "_" => Some(FontStep::Dec),
                        "0" => Some(FontStep::Reset),
                        _ => None,
                    };
                    if let Some(step) = step {
                        self.resize_font(step);
                        self.mark_dirty(event_loop);
                        return;
                    }
                }

                // Shift + PageUp/PageDown/Home/End drives glassy's own scrollback
                // (the primary screen only) and is consumed before the child sees
                // it. This mirrors the de-facto terminal convention.
                if event.state.is_pressed()
                    && self.mods.shift_key()
                    && !self.term_mode().contains(TermMode::ALT_SCREEN)
                    && let Key::Named(named) = &event.logical_key
                {
                    let scroll = match named {
                        NamedKey::PageUp => Some(Scroll::PageUp),
                        NamedKey::PageDown => Some(Scroll::PageDown),
                        NamedKey::Home => Some(Scroll::Top),
                        NamedKey::End => Some(Scroll::Bottom),
                        _ => None,
                    };
                    if let Some(scroll) = scroll {
                        if let Some(pty) = &self.pty {
                            pty.term.lock().scroll_display(scroll);
                        }
                        self.mark_dirty(event_loop);
                        return;
                    }
                }

                // While an overlay is open it owns the keyboard — nothing reaches
                // the child. Esc / F1 / Ctrl+, close it; settings handles nav/edit.
                if event.state.is_pressed() && (self.help_open || self.settings_open) {
                    let key = &event.logical_key;
                    let toggle_settings = self.mods.control_key()
                        && matches!(key, Key::Character(s) if s.as_str() == ",");
                    if matches!(key, Key::Named(NamedKey::Escape | NamedKey::F1))
                        || toggle_settings
                    {
                        self.help_open = false;
                        self.settings_open = false;
                        self.force_full_redraw = true;
                        self.mark_dirty(event_loop);
                        return;
                    }
                    if self.settings_open {
                        self.handle_settings_key(key.clone(), event_loop);
                    }
                    return; // consume all other keys while an overlay is up
                }

                // Open an overlay (only when none is up).
                if event.state.is_pressed() {
                    if let Key::Named(NamedKey::F1) = &event.logical_key {
                        self.help_open = true;
                        self.force_full_redraw = true;
                        self.mark_dirty(event_loop);
                        return;
                    }
                    if self.mods.control_key()
                        && matches!(&event.logical_key, Key::Character(s) if s.as_str() == ",")
                    {
                        self.settings_open = true;
                        self.settings_sel = 0;
                        self.settings_saved = false;
                        self.force_full_redraw = true;
                        self.mark_dirty(event_loop);
                        return;
                    }
                }

                // When the application has enabled the kitty keyboard protocol,
                // encode modified keys in CSI-u form so it can disambiguate them
                // (this is what makes Shift+Enter distinct from Enter).
                let kitty = self
                    .term_mode()
                    .contains(TermMode::DISAMBIGUATE_ESC_CODES);
                if let Some(bytes) = encode_key(&event, self.mods, kitty) {
                    // Typing resets the blink to solid-on so the cursor doesn't
                    // wink out mid-keystroke, matching every mainstream terminal.
                    self.reset_blink();
                    // Typing dismisses any active selection, matching the de-facto
                    // terminal convention.
                    self.clear_selection();
                    if let Some(pty) = &self.pty {
                        // A typed key snaps the view back to the prompt, matching
                        // every mainstream terminal.
                        pty.term.lock().scroll_display(Scroll::Bottom);
                        pty.write(bytes);
                    }
                    // The snap-to-bottom (and the cursor/selection reset above) are
                    // visual changes even when the child emits nothing back — e.g.
                    // typing while scrolled up into a paused/blocked program. Repaint
                    // unconditionally so the view never stays frozen in scrollback.
                    self.mark_dirty(event_loop);
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                // Committed IME text is input like any keystroke: reset the blink,
                // drop the selection, snap to the prompt, and repaint even if the
                // child stays quiet.
                self.reset_blink();
                self.clear_selection();
                if let Some(pty) = &self.pty {
                    pty.term.lock().scroll_display(Scroll::Bottom);
                    pty.write(text.into_bytes());
                }
                self.mark_dirty(event_loop);
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_px = (position.x, position.y);
                let cell = self.px_to_cell(position.x, position.y);
                let moved = cell != self.mouse_cell;
                self.mouse_cell = cell;

                // Drag-to-reorder a tab: while a tab chip is held, move it under
                // the pointer's column. Takes priority over selection/hover.
                if self.dragging_tab.is_some() {
                    if let Some(r) = self.renderer.as_ref() {
                        let col = ((position.x - r.pad() as f64) / r.cell_metrics().width as f64)
                            .floor()
                            .max(0.0) as usize;
                        if self.drag_tab_to(col) {
                            self.force_full_redraw = true;
                            self.mark_dirty(event_loop);
                        }
                    }
                    return;
                }

                // Tab-strip hover highlighting: track the toolbar item under the
                // pointer (only while over row 0), repaint when it changes.
                {
                    let (pad, ch_w, ch_h) = self
                        .renderer
                        .as_ref()
                        .map(|r| (r.pad() as f64, r.cell_metrics().width as f64, r.cell_metrics().height as f64))
                        .unwrap_or((0.0, 1.0, 1.0));
                    let screen_row = ((position.y - pad) / ch_h).floor();
                    let new_hover = if (0.0..TAB_STRIP_ROWS as f64).contains(&screen_row) {
                        let col = ((position.x - pad) / ch_w).floor().max(0.0) as usize;
                        self.strip_item_at_col(col)
                    } else {
                        None
                    };
                    if new_hover != self.hovered_strip_item {
                        self.hovered_strip_item = new_hover;
                        self.mark_dirty(event_loop);
                    }
                }

                // Extend an in-progress glassy text selection while dragging.
                if self.selecting {
                    self.update_selection();
                    self.mark_dirty(event_loop);
                } else if moved {
                    // Motion reports drive hover highlighting (e.g. the Claude
                    // Code TUI highlights the element under the pointer, which
                    // needs any-motion mode 1003 with no button held).
                    let mode = self.term_mode();
                    if let Some(button) = motion_button(mode, self.held_button) {
                        self.report_mouse(button, true, true, mode);
                    } else if !mode.intersects(TermMode::MOUSE_MODE) {
                        // Track the hovered OSC8 hyperlink so it can be underlined.
                        let (c, r) = self.mouse_cell;
                        let link = self.cell_hyperlink(c, r);
                        if link != self.hovered_link {
                            self.hovered_link = link;
                            self.mark_dirty(event_loop);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let base = match button {
                    MouseButton::Left => 0u8,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                let pressed = state == ElementState::Pressed;
                // Track the held button for drag reports regardless of mode.
                self.held_button = if pressed { Some(base) } else { None };
                if !pressed {
                    self.dragging_tab = None; // end any tab drag-reorder on release
                }

                // A left click in the tab strip switches tabs; never sent onward.
                if button == MouseButton::Left && pressed && self.strip_click(event_loop) {
                    self.held_button = None;
                    return;
                }

                let mode = self.term_mode();
                // Ctrl+Left opens an OSC8 hyperlink under the pointer, overriding
                // application mouse handling (the common terminal convention).
                if button == MouseButton::Left && pressed && self.mods.control_key() {
                    let (c, r) = self.mouse_cell;
                    if let Some(uri) = self.cell_hyperlink(c, r) {
                        Self::open_url(&uri);
                        return;
                    }
                }
                if mode.intersects(TermMode::MOUSE_MODE) {
                    // The application owns the mouse; never start a glassy
                    // selection or paste underneath it.
                    self.report_mouse(base, pressed, false, mode);
                    return;
                }

                match (button, pressed) {
                    // Left press: start (or extend the granularity of) a glassy
                    // text selection. Double/triple clicks within the same cell
                    // and a short window escalate to Semantic (word) then Lines.
                    (MouseButton::Left, true) => {
                        const MULTI_CLICK: Duration = Duration::from_millis(300);
                        let now = Instant::now();
                        let count = match self.last_click {
                            Some((cell, n, t))
                                if cell == self.mouse_cell
                                    && now.duration_since(t) < MULTI_CLICK =>
                            {
                                (n % 3) + 1
                            }
                            _ => 1,
                        };
                        self.last_click = Some((self.mouse_cell, count, now));
                        let ty = match count {
                            2 => SelectionType::Semantic,
                            3 => SelectionType::Lines,
                            _ => SelectionType::Simple,
                        };
                        self.start_selection(ty);
                        self.mark_dirty(event_loop);
                    }
                    // Left release: finish the drag; the selection persists for copy.
                    (MouseButton::Left, false) => {
                        self.selecting = false;
                    }
                    // Middle click pastes the clipboard (primary on X11 would be
                    // ideal, but arboard exposes only the standard clipboard).
                    (MouseButton::Middle, true) => {
                        self.paste_clipboard();
                        self.mark_dirty(event_loop);
                    }
                    _ => {}
                }
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                use winit::event::TouchPhase;
                // A touchpad gesture brackets its deltas with Started/Ended; reset
                // the accumulators and the one-switch-per-swipe latch at those
                // boundaries so each gesture is independent.
                if matches!(
                    phase,
                    TouchPhase::Started | TouchPhase::Ended | TouchPhase::Cancelled
                ) {
                    self.tab_scroll_accum = 0.0;
                    self.content_scroll_accum = 0.0;
                    self.swipe_consumed = false;
                }

                // Over the tab strip: a swipe/scroll switches tabs as a discrete
                // GESTURE — one tab per swipe, clamped at the ends (no wrap-around
                // carousel). Horizontal motion is preferred (natural swipe-to-switch).
                let in_strip = {
                    let (pad, ch_h) = self
                        .renderer
                        .as_ref()
                        .map(|r| (r.pad() as f64, r.cell_metrics().height as f64))
                        .unwrap_or((0.0, 1.0));
                    let row = ((self.mouse_px.1 - pad) / ch_h).floor();
                    (0.0..TAB_STRIP_ROWS as f64).contains(&row)
                };
                if in_strip {
                    const STEP: f32 = 24.0; // px of swipe travel to trigger one switch
                    match delta {
                        // A discrete wheel notch always steps one tab (clamped).
                        MouseScrollDelta::LineDelta(x, y) => {
                            let primary = if x.abs() > y.abs() { x } else { y };
                            if primary > 0.0 {
                                self.step_tab(1, event_loop);
                            } else if primary < 0.0 {
                                self.step_tab(-1, event_loop);
                            }
                        }
                        // Touchpad: accumulate, fire ONCE per swipe at the threshold,
                        // then latch until the gesture ends — no twitchy carousel.
                        MouseScrollDelta::PixelDelta(p) => {
                            let primary = (if p.x.abs() > p.y.abs() { p.x } else { p.y }) as f32;
                            self.tab_scroll_accum += primary;
                            if !self.swipe_consumed && self.tab_scroll_accum.abs() >= STEP {
                                let dir = if self.tab_scroll_accum > 0.0 { 1 } else { -1 };
                                self.step_tab(dir, event_loop);
                                self.swipe_consumed = true;
                            }
                        }
                    }
                    return;
                }
                self.tab_scroll_accum = 0.0;

                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        self.content_scroll_accum = 0.0;
                        if y == 0.0 {
                            0
                        } else {
                            (y.abs().ceil() as i32) * y.signum() as i32
                        }
                    }
                    // Touchpads emit many sub-line pixel deltas; accumulate and step
                    // by the cell height so slow scrolls register instead of being
                    // truncated to zero each event (the "tiny scrolls do nothing" bug).
                    MouseScrollDelta::PixelDelta(p) => {
                        self.content_scroll_accum += p.y as f32;
                        let step = self
                            .renderer
                            .as_ref()
                            .map(|r| r.cell_metrics().height as f32)
                            .unwrap_or(20.0)
                            .max(1.0);
                        let n = (self.content_scroll_accum / step) as i32;
                        self.content_scroll_accum -= n as f32 * step;
                        n
                    }
                };
                if lines == 0 {
                    return;
                }
                let mode = self.term_mode();
                let up = lines > 0;
                let count = lines.unsigned_abs() as usize;

                match wheel_action(mode) {
                    WheelAction::Report => {
                        // Wheel as button 64 (up) / 65 (down), one report per line.
                        let button = if up { 64 } else { 65 };
                        for _ in 0..count {
                            self.report_mouse(button, true, false, mode);
                        }
                    }
                    WheelAction::Arrows => {
                        // Alt-screen apps (pagers, bat, vim without `mouse=`) expect
                        // the wheel to emit arrow keys — xterm's alternateScroll is
                        // on by default and the alt screen has no scrollback of its
                        // own. ~3 lines per notch.
                        if let Some(pty) = &self.pty {
                            let seq: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
                            let n = count * 3;
                            let mut out = Vec::with_capacity(seq.len() * n);
                            for _ in 0..n {
                                out.extend_from_slice(seq);
                            }
                            pty.write(out);
                        }
                    }
                    WheelAction::Scrollback => {
                        let delta = if up { WHEEL_LINES } else { -WHEEL_LINES } * count as i32;
                        if let Some(pty) = &self.pty {
                            pty.term.lock().scroll_display(Scroll::Delta(delta));
                        }
                        self.mark_dirty(event_loop);
                    }
                }
            }
            WindowEvent::Resized(size) => self.handle_resize(event_loop, size),
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(w) = &self.window {
                    self.handle_resize(event_loop, w.inner_size());
                }
            }
            WindowEvent::RedrawRequested => self.render(),
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Headless capture path: at the deadline, render the latest content,
        // dump it to disk, and exit.
        if let Some(deadline) = self.capture_deadline {
            if Instant::now() >= deadline {
                self.render();
                if let (Some(renderer), Some(path)) =
                    (self.renderer.as_mut(), self.capture.as_ref())
                {
                    match renderer.capture(path) {
                        Ok(()) => log::info!("captured frame to {}", path.display()),
                        Err(e) => log::error!("capture failed: {e:#}"),
                    }
                }
                event_loop.exit();
                return;
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(deadline));
            return;
        }

        let now = Instant::now();

        // Cursor blink: only runs while focused and the child asked for a blinking
        // cursor. When that holds, advance the phase at each `blink_at` deadline and
        // mark dirty so the cursor redraws; otherwise the cursor stays solid and we
        // never schedule a wakeup for it (preserving the 0%-idle `Wait` path).
        let blink_active = self.focused && self.cursor_blinking();
        if blink_active {
            if now >= self.blink_at {
                self.blink_on = !self.blink_on;
                self.blink_at = now + BLINK_INTERVAL;
                self.dirty = true;
            }
        } else {
            // Settle to the solid (visible) phase so re-focusing shows the cursor.
            self.blink_on = true;
        }

        // Visual-bell flash: while the flash window is open, keep redrawing so the
        // overlay is painted; once it elapses, restore (a full rebuild drops the
        // tint from every cell) and repaint one last frame. This is a short, finite
        // wake; idle returns to `Wait` afterward.
        let flash_active = match self.bell_flash_until {
            Some(until) if now < until => true,
            Some(_) => {
                // Flash just ended: clear it and force the restore frame.
                self.bell_flash_until = None;
                self.force_full_redraw = true;
                self.dirty = true;
                false
            }
            None => false,
        };

        if !self.dirty {
            // Idle: stay parked on `Wait` (0% CPU) unless a blink flip or a flash
            // boundary is pending, in which case wake at the earliest deadline.
            match self.next_wake(blink_active, flash_active) {
                Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
                None => event_loop.set_control_flow(ControlFlow::Wait),
            }
            return;
        }

        if now >= self.next_frame {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            self.next_frame = now + self.refresh;
            // RedrawRequested will clear `dirty`. Keep a wakeup scheduled for the
            // next blink flip or flash boundary; otherwise wait for the next event.
            match self.next_wake(blink_active, flash_active) {
                Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
                None => event_loop.set_control_flow(ControlFlow::Wait),
            }
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        StripItem, WheelAction, image_dst_size, motion_button, move_in_order, strip_item_at,
        strip_layout, wheel_action,
    };

    #[test]
    fn move_in_order_reorders() {
        let mut v = vec![10, 20, 30, 40];
        move_in_order(&mut v, 0, 2); // drag first to index 2
        assert_eq!(v, vec![20, 30, 10, 40]);
        move_in_order(&mut v, 3, 0); // drag last to front
        assert_eq!(v, vec![40, 20, 30, 10]);
        move_in_order(&mut v, 1, 1); // no-op
        assert_eq!(v, vec![40, 20, 30, 10]);
        move_in_order(&mut v, 9, 0); // out of range: no-op
        assert_eq!(v, vec![40, 20, 30, 10]);
    }
    use alacritty_terminal::term::TermMode;

    #[test]
    fn strip_hit_test_matches_layout() {
        // Two tabs (tab 1 active) + their ✕ + a + button + right-hand ? / ≡.
        let segs = strip_layout(&[("zsh", true, false), ("vim", false, false)], 120);
        // Chip 0 body " 1 zsh " 3..10, "✕ " 10..12, gap@12; chip 1 " 2 vim " 13..20,
        // "✕ " 20..22, gap@22; then " + " at 23..26.
        assert_eq!(strip_item_at(&segs, 4), Some(StripItem::Tab(0)));
        assert_eq!(strip_item_at(&segs, 11), Some(StripItem::TabClose(0)));
        assert_eq!(strip_item_at(&segs, 12), None); // inter-chip gap
        assert_eq!(strip_item_at(&segs, 14), Some(StripItem::Tab(1)));
        assert_eq!(strip_item_at(&segs, 20), Some(StripItem::TabClose(1)));
        assert_eq!(strip_item_at(&segs, 24), Some(StripItem::NewTab));
        // Right buttons are the last 6 cols (114..120): " ? " then " ≡ ".
        assert_eq!(strip_item_at(&segs, 115), Some(StripItem::Help));
        assert_eq!(strip_item_at(&segs, 118), Some(StripItem::Menu));
        assert_eq!(strip_item_at(&segs, 60), None); // inert gap
    }

    #[test]
    fn single_tab_has_no_number_or_close() {
        // One tab shows just the title — no "1", no ✕ (closing it = quit).
        let segs = strip_layout(&[("shell", true, false)], 100);
        assert!(segs.iter().any(|s| s.item == StripItem::Tab(0)));
        assert!(!segs.iter().any(|s| matches!(s.item, StripItem::TabClose(_))));
        let title = &segs.iter().find(|s| s.item == StripItem::Tab(0)).unwrap().label;
        assert!(title.contains("shell") && !title.contains('1'));
    }

    #[test]
    fn strip_layout_numbers_by_stable_position() {
        // Numbering follows display position, NOT which tab is active: with tab 2
        // active, chips are still "1 a" then "2 b".
        let segs = strip_layout(&[("a", false, false), ("b", true, false)], 120);
        let lbl = |it| segs.iter().find(|s| s.item == it).map(|s| s.label.clone()).unwrap();
        assert!(lbl(StripItem::Tab(0)).contains("1 a"));
        assert!(lbl(StripItem::Tab(1)).contains("2 b"));
    }

    #[test]
    fn wheel_normal_screen_scrolls_scrollback() {
        assert_eq!(wheel_action(TermMode::empty()), WheelAction::Scrollback);
    }

    #[test]
    fn image_size_native_when_unsized() {
        assert_eq!(image_dst_size(0, 0, 64, 32, 10.0, 20.0), (64.0, 32.0));
    }

    #[test]
    fn image_size_exact_cell_box_when_both_given() {
        // 4 cols x 3 rows at a 10x20 cell box.
        assert_eq!(image_dst_size(4, 3, 64, 32, 10.0, 20.0), (40.0, 60.0));
    }

    #[test]
    fn image_size_preserves_aspect_with_one_dim() {
        // 2:1 image, only cols=20 at cell_w=10 -> 200px wide, 100px tall (2:1).
        assert_eq!(image_dst_size(20, 0, 64, 32, 10.0, 20.0), (200.0, 100.0));
        // 2:1 image, only rows=5 at cell_h=20 -> 100px tall, 200px wide (2:1).
        assert_eq!(image_dst_size(0, 5, 64, 32, 10.0, 20.0), (200.0, 100.0));
    }

    #[test]
    fn wheel_alt_screen_emits_arrows() {
        // bat/less/vim without mouse: alt screen, no mouse reporting.
        assert_eq!(wheel_action(TermMode::ALT_SCREEN), WheelAction::Arrows);
    }

    #[test]
    fn wheel_mouse_mode_reports_to_app() {
        // vim with `mouse=a`, htop, claude: app owns the wheel.
        assert_eq!(
            wheel_action(TermMode::MOUSE_REPORT_CLICK),
            WheelAction::Report
        );
        assert_eq!(
            wheel_action(TermMode::ALT_SCREEN | TermMode::MOUSE_MOTION),
            WheelAction::Report
        );
    }

    #[test]
    fn hover_reports_only_under_any_motion() {
        // Any-motion (1003) reports bare moves (id 3) -> drives hover highlight.
        assert_eq!(motion_button(TermMode::MOUSE_MOTION, None), Some(3));
        // Button-motion (1002) stays silent without a held button.
        assert_eq!(motion_button(TermMode::MOUSE_DRAG, None), None);
        // Click-only (1000) never reports motion.
        assert_eq!(motion_button(TermMode::MOUSE_REPORT_CLICK, None), None);
        assert_eq!(motion_button(TermMode::empty(), None), None);
    }

    #[test]
    fn drag_reports_held_button_under_motion_modes() {
        assert_eq!(motion_button(TermMode::MOUSE_DRAG, Some(0)), Some(0));
        assert_eq!(motion_button(TermMode::MOUSE_MOTION, Some(2)), Some(2));
        // Click-only mode does not report drags.
        assert_eq!(motion_button(TermMode::MOUSE_REPORT_CLICK, Some(0)), None);
    }
}
