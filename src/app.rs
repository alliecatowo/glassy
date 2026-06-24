//! The winit UI/render driver.
//!
//! Idle behaviour is `ControlFlow::Wait`: 0% CPU, no GPU submits until the PTY
//! thread (or a resize/input) wakes us. Wakeups set a dirty flag and are
//! coalesced to at most one frame per monitor refresh, so a fast producer like
//! Claude Code streaming tokens collapses into a single redraw per refresh
//! instead of one redraw per token burst.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions, Indexed, Scroll};
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
use crate::gui;
use crate::input::{MouseReport, encode_key, encode_mouse};
use crate::pane;
use crate::pty::{Pty, UserEvent};
use crate::renderer::{CursorOverlay, Decorations, Renderer, UnderlineStyle};

/// Lines of scrollback to move per wheel notch when reporting to a TUI or
/// scrolling glassy's own scrollback buffer.
const WHEEL_LINES: i32 = 3;

/// Legacy cell-row count, retained only by the not-yet-pixelized modal/menu draw
/// helpers (waves 5/6) for their full-screen sizing math. The terminal grid no
/// longer reserves a cell row for the strip — it is inset in PIXELS by the GUI
/// tab bar (`tab_bar_h`) via `Renderer::set_grid_origin_y`.
const TAB_STRIP_ROWS: usize = 1;

/// Tab-bar height in physical px, derived from the cell height so the chrome
/// scales with the font (and DPI) exactly like the cell metrics. The bar holds a
/// row of real tab shapes whose active member connects to the content surface.
fn tab_bar_h(cell_h: f32) -> f32 {
    (cell_h * 1.7).round().max(28.0)
}

/// Top-corner radius of a tab chip (px).
const TAB_RADIUS: f32 = 5.0;
/// Minimum / maximum tab width in px (multi-tab mode).
const TAB_MIN_W: f32 = 120.0;
const TAB_MAX_W: f32 = 220.0;
/// Gap between adjacent tab chips (px).
const TAB_GAP: f32 = 2.0;
/// Horizontal inner padding of a tab chip (px).
const TAB_PAD_X: f32 = 10.0;
/// Close-button hit box inside a tab (px, square).
const CLOSE_BOX: f32 = 16.0;
/// Square icon-button size for +/≡/?/⚙ controls (px).
const CTRL_BTN: f32 = 28.0;

/// Corner radius for tab-bar icon buttons, derived from the cell height like the
/// GUI metric scale so it tracks the font/DPI.
fn gui_radius(cell_h: f32) -> f32 {
    (cell_h * 0.28).round().clamp(4.0, 8.0)
}

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

/// Reduce an OSC window title to printable ASCII only, so the native (CSD)
/// titlebar font — which we do not control — can render every character it ever
/// receives, making "tofu" boxes structurally impossible. Non-ASCII-graphic
/// chars (CJK, emoji, Nerd-Font icons, dingbats, combining marks) are dropped;
/// runs of whitespace collapse to a single space; an empty result falls back to
/// "glassy". The full rich/Unicode title is rendered in OUR tab bar instead
/// (through the glyph atlas, which renders all of them). Pure for unit testing.
fn os_title(title: &str) -> String {
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
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum StripItem {
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
struct StripSeg {
    item: StripItem,
    label: String,
    rect: gui::Rect,
}

/// A tab descriptor in stable display order: (title, is_active, has_activity).
type TabDesc<'a> = (&'a str, bool, bool);

/// Lay out the real GUI tab bar across the pixel-wide bar `[0, bar_w)` at height
/// `bar_h`, from tab descriptors in stable order. Produces, left→right: the glassy
/// mark slot, a `+` new-tab button, the tab chips (each a body rect + an embedded
/// close-box rect in multi-tab mode), and right-aligned `?` help, `⚙` settings,
/// `≡` menu icon buttons. The active tab keeps its position. Pure (pixel math
/// only) so the painter and the click hit-test agree, and so it is unit-testable.
fn strip_layout(tabs: &[TabDesc], bar_w: f32, bar_h: f32, cell_w: f32) -> Vec<StripSeg> {
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
        let w = avail.min(TAB_MAX_W * 1.6).max(0.0);
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
fn strip_item_at(segs: &[StripSeg], px: f32, py: f32) -> Option<StripItem> {
    segs.iter()
        .rev()
        .find(|s| gui::hit(s.rect, px, py))
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
    "  Ctrl+Shift+W      Close pane / tab",
    "  Ctrl+Tab          Next tab",
    "  Ctrl+Shift+Tab    Previous tab",
    "  Ctrl+Shift+C / V  Copy / Paste",
    "  Ctrl  +  /  -  / 0  Font bigger / smaller / reset",
    "  Shift+PgUp/PgDn   Scroll history",
    "  Shift+Home/End    Scroll top / bottom",
    "  Ctrl+Click        Open hyperlink",
    "  Ctrl+,            Settings",
    "",
    "  Ctrl+Shift+E      Split pane (vertical)",
    "  Ctrl+Shift+O      Split pane (horizontal)",
    "  Alt+Arrow         Move focus to adjacent pane",
    "",
    "  Right-click       Copy / Paste / New tab menu",
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

    // Glass palette: a dim full-screen backdrop, a translucent dark panel body, and
    // a thin accent border. No cream interior, no per-row wipe — the panel composites
    // over the live terminal via the overlay pipeline (drawn after the grid). Colors
    // are straight RGBA; `push_overlay_*` premultiplies.
    // Backdrop is dim enough that the modal text clearly wins over the live
    // terminal underneath (0.30 left the bright `ls` filenames legible).
    let backdrop = [0.0, 0.0, 0.0, 0.50];
    let body = {
        let b = color::default_bg();
        [b[0], b[1], b[2], 0.82]
    };
    // Translucent border: the accent at 0.6 composites as a glass rail instead of
    // a solid opaque cream band (accent == cursor, near-white on Dracula et al).
    let border = {
        let a = color::accent();
        [a[0], a[1], a[2], 0.6]
    };
    let text_fg = color::default_fg();
    let title_fg = lighten(color::accent(), 0.1);

    let content_w = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0);
    let panel_w = (content_w + 4).min(cols.max(1));
    let panel_h = (lines.len() + 2).min(total_rows.max(1));
    let left = (cols.saturating_sub(panel_w)) / 2;
    let top = (total_rows.saturating_sub(panel_h)) / 2;

    // 1) Dim the whole screen.
    renderer.push_overlay_cells(0, 0, cols, total_rows, backdrop);
    // 2) Translucent panel body.
    renderer.push_overlay_cells(left, top, panel_w, panel_h, body);
    // 3) Thin accent border rails (1 cell thick), over the body.
    renderer.push_overlay_cells(left, top, panel_w, 1, border); // top
    renderer.push_overlay_cells(left, top + panel_h - 1, panel_w, 1, border); // bottom
    renderer.push_overlay_cells(left, top, 1, panel_h, border); // left
    renderer.push_overlay_cells(left + panel_w - 1, top, 1, panel_h, border); // right

    // 4) Panel text — glyphs only, drawn ON TOP of the glass via the overlay-text
    //    channel so they stay crisp.
    for (li, line) in lines.iter().enumerate() {
        let row = top + 1 + li;
        if row >= top + panel_h - 1 {
            break;
        }
        let fg = if li == 0 { title_fg } else { text_fg };
        for (ci, ch) in line.chars().enumerate() {
            let col = left + 1 + ci;
            if col >= left + panel_w - 1 {
                break;
            }
            renderer.push_overlay_glyph(col, row, ch, fg);
        }
    }
}

/// Actions available in the ≡ hamburger dropdown and the right-click context
/// menu. Kept as a single enum so the hit-test and keyboard dispatch share one
/// definition across both menus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MenuAction {
    Copy,
    Paste,
    NewTab,
    Settings,
    Help,
    CloseTab,
}

impl MenuAction {
    /// The fixed set shown by the ≡ hamburger dropdown. The right-click context
    /// menu uses a separately-built `Vec<MenuAction>` (see `context_menu_items`).
    const ALL: &'static [MenuAction] =
        &[MenuAction::NewTab, MenuAction::Settings, MenuAction::Help, MenuAction::CloseTab];

    fn label(self) -> &'static str {
        match self {
            MenuAction::Copy => "Copy",
            MenuAction::Paste => "Paste",
            MenuAction::NewTab => "New tab",
            MenuAction::Settings => "Settings",
            MenuAction::Help => "Help",
            MenuAction::CloseTab => "Close tab",
        }
    }
}

/// Draw a dropdown menu panel anchored at `(left, top)` in screen-row/col
/// coordinates. `items` is the list to display; `sel` is the keyboard-
/// highlighted row (0-based). `left`/`top` come from either the hamburger
/// (top-right, below strip) or the context menu (pointer-anchored, clamped).
/// Only repaints the rows the panel occupies; the rest keep their content.
fn draw_dropdown_menu(
    renderer: &mut Renderer,
    rows: usize,
    cols: usize,
    items: &[MenuAction],
    sel: usize,
    left: usize,
    top: usize,
) {
    // Glass dropdown: translucent body + thin accent border, composited over the
    // live terminal via the overlay pipeline. Unlike a modal, a menu does NOT dim
    // the whole screen — the terminal stays visible beside it. Straight RGBA here;
    // `push_overlay_*` premultiplies.
    let body = {
        let b = color::default_bg();
        [b[0], b[1], b[2], 0.82]
    };
    // Translucent border (see draw_modal): glass rail, not an opaque cream band.
    let border = {
        let a = color::accent();
        [a[0], a[1], a[2], 0.6]
    };
    let text_fg = color::default_fg();
    // Selection highlight uses the theme's chromatic selection tint, not the
    // (often near-white) cursor-derived accent — so the selected row reads as a
    // brand-colored bar instead of flat grey on light-accent themes (Dracula).
    let sel_bg = {
        let s = color::selection_bg();
        [s[0], s[1], s[2], 0.85]
    };
    let sel_fg = color::default_fg();

    // Clamp the panel to the screen so rails / text stay on-grid.
    let total_rows = rows + TAB_STRIP_ROWS;
    let panel_w = (items.iter().map(|a| a.label().len()).max().unwrap_or(0) + 4)
        .min(cols.saturating_sub(left).max(1));
    let panel_h = (items.len() + 2).min(total_rows.saturating_sub(top).max(1));

    renderer.push_overlay_cells(left, top, panel_w, panel_h, body);
    renderer.push_overlay_cells(left, top, panel_w, 1, border); // top
    renderer.push_overlay_cells(left, top + panel_h - 1, panel_w, 1, border); // bottom
    renderer.push_overlay_cells(left, top, 1, panel_h, border); // left
    renderer.push_overlay_cells(left + panel_w - 1, top, 1, panel_h, border); // right

    for (li, item) in items.iter().enumerate() {
        let row = top + 1 + li;
        if row >= top + panel_h - 1 {
            break;
        }
        let fg = if li == sel {
            renderer.push_overlay_cells(left + 1, row, panel_w.saturating_sub(2), 1, sel_bg);
            sel_fg
        } else {
            text_fg
        };
        for (ci, ch) in item.label().chars().enumerate() {
            let col = left + 2 + ci; // 2-cell left pad (matches old layout)
            if col >= left + panel_w - 1 {
                break;
            }
            renderer.push_overlay_glyph(col, row, ch, fg);
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

/// Tab "busy" spinner. A session is BUSY while it is actively producing output;
/// each PTY wakeup re-arms a `BUSY_LINGER` deadline, and the chip spins until that
/// elapses with no further output (mirroring the bell-flash deadline). While any
/// tab is busy we advance one `SPINNER_FRAMES` glyph every `SPINNER_INTERVAL` and
/// schedule a finite wakeup for it; once nothing is busy we return to `Wait`.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
const SPINNER_INTERVAL: Duration = Duration::from_millis(100);
const BUSY_LINGER: Duration = Duration::from_millis(600);

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
    /// Follow the system light/dark color scheme: when true, the active theme is
    /// chosen at startup and on `ThemeChanged` from `theme_light` / `theme_dark`
    /// according to the OS preference, instead of pinning `theme`.
    pub follow_system: bool,
    /// Theme to use when the system prefers a LIGHT color scheme (and
    /// `follow_system` is on). Canonical [`color::THEME_NAMES`] entry.
    pub theme_light: String,
    /// Theme to use when the system prefers a DARK color scheme (and
    /// `follow_system` is on). Canonical [`color::THEME_NAMES`] entry.
    pub theme_dark: String,
}

/// A tab's split layout: the tiling tree (whose leaf ids are pty/pane ids) plus
/// the parked PTYs of every pane EXCEPT the focused one. The focused pane's PTY
/// lives in `App::pty` (active tab) or `Session::pty` (parked tab), exactly like
/// the single-pane model — so all the existing single-session code keeps working
/// unchanged, and `panes == None` is byte-identical to today's one-pane tab.
struct PaneGroup {
    layout: pane::Layout,
    /// Non-focused panes' PTYs, keyed by pane id (== leaf id == pty id).
    others: HashMap<usize, Pty>,
}

/// One terminal tab. The *active* tab's PTY lives directly in `App::pty` (so all
/// rendering/input code stays single-session); inactive tabs are parked here and
/// swapped in on switch.
struct Session {
    id: usize,
    pty: Pty,
    /// Split layout for this parked tab. `None` for a single-pane tab; when set,
    /// `pty` above holds the focused pane and `panes.others` the rest.
    panes: Option<PaneGroup>,
    title: String,
    /// Set when this background tab produces output; shown as a dot on its chip
    /// and cleared when the tab is activated. Lets you see which tab is busy.
    activity: bool,
    /// While this session is actively producing output, the deadline after which
    /// it counts as idle again. Re-armed on every PTY wakeup; cleared (back to
    /// `None`) once elapsed in `about_to_wait`. Drives the chip's busy spinner.
    busy_until: Option<Instant>,
    /// Last working directory reported by this session's focused pane via OSC 7,
    /// so a new tab/split opened from this tab inherits the cwd. `None` until the
    /// shell emits OSC 7 (or for shells that never do).
    last_cwd: Option<std::path::PathBuf>,
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    config: Config,

    // Created lazily in `resumed()` (winit requires the window there).
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    pty: Option<Pty>,
    /// Split layout for the ACTIVE tab. `None` when the active tab has one pane
    /// (the common case — then `pty` is the sole pane and every existing path
    /// runs unchanged). When set, `pty` is the focused pane and `panes.others`
    /// holds the rest; rendering/input fan out across `panes.layout`.
    panes: Option<PaneGroup>,

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
    /// Last working directory reported by the active session via OSC 7. New
    /// tabs/splits inherit it so they open where the user is, not in `$HOME`.
    /// Parked sessions keep their own in `Session::last_cwd`.
    active_cwd: Option<std::path::PathBuf>,
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
    /// While a pane resize gutter is held, the handle being dragged (which split
    /// divider, and the axis geometry to map the pointer back to a ratio). Set on
    /// a left-press over a gutter, cleared on release. `None` when not dragging.
    dragging_gutter: Option<pane::SplitHandle>,
    /// The gutter currently under the pointer (not yet dragged), so the divider
    /// can be drawn transiently brighter/fatter as hover feedback. `None` off any
    /// gutter. Kept distinct from `dragging_gutter` so hover and drag share paint.
    hovered_gutter: Option<pane::SplitHandle>,
    /// The toolbar item currently under the pointer, for hover highlighting.
    hovered_strip_item: Option<StripItem>,
    /// The toolbar item the left button is currently pressed on, for the PRESSED
    /// (inset/darker) treatment. Set on press over a strip item, cleared on
    /// release — gives clicks real tactility distinct from hover.
    held_strip_item: Option<StripItem>,
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
    /// Whether the ≡ hamburger dropdown menu is currently shown.
    menu_open: bool,
    /// Currently-highlighted row in the dropdown menu (keyboard nav).
    menu_sel: usize,
    /// When the dropdown is the right-click context menu, the items it shows
    /// (selection-aware). `None` means the dropdown is the ≡ hamburger (uses
    /// `MenuAction::ALL`). Drives both draw and hit-test.
    menu_items: Option<Vec<MenuAction>>,
    /// Screen-cell anchor (col, row) for the open dropdown panel. Set for both
    /// the hamburger and the context menu so the render site is branch-free.
    menu_anchor: Option<(usize, usize)>,

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

    // Tab busy-spinner state. `active_busy_until` is the active session's busy
    // deadline (the parked sessions keep their own in `Session::busy_until`).
    // `spinner_at` is when the next spinner frame is due and `spinner_frame` the
    // current glyph index. The spinner only animates while some tab is busy;
    // otherwise we never schedule a wakeup for it (preserving the 0%-idle path).
    active_busy_until: Option<Instant>,
    spinner_frame: usize,
    spinner_at: Instant,

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

    // --- Real-GUI chrome layer (immediate-mode; see src/gui.rs). ---
    /// The widget currently latched by a left-button press, carried across frames
    /// so press→release resolves on the same widget.
    gui_pressed: Option<gui::WidgetId>,
    /// The widget holding keyboard focus (Tab/arrow nav), carried across frames.
    gui_focused: Option<gui::WidgetId>,
    /// Per-widget animations (hover fades, toggle slides). The event loop stays on
    /// `ControlFlow::Poll` only while some entry here is unsettled; otherwise it
    /// parks on `Wait` (0% idle).
    gui_anims: std::collections::HashMap<gui::WidgetId, gui::Anim>,
    /// Press→release click edge captured by the MouseInput handler and consumed by
    /// the next chrome paint. Set on left-release, cleared after the GUI frame.
    gui_click_edge: bool,
    /// Last instant the GUI animations were stepped, for dt computation.
    gui_anim_last: Instant,
    /// Temporary: render the Wave-0 GUI-primitive demo (GLASSY_GUI_DEMO set).
    gui_demo: bool,
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>, config: Config) -> Self {
        Self {
            proxy,
            config,
            window: None,
            renderer: None,
            pty: None,
            panes: None,
            background: Vec::new(),
            tab_order: vec![0], // the first tab (spawned in resumed) is id 0
            active_id: 0,
            active_title: String::new(),
            active_cwd: None,
            next_id: 1,
            cols: 0,
            rows: 0,
            base_font_px: None,
            mods: ModifiersState::empty(),
            focused: true,
            started: Instant::now(),
            first_frame_done: false,
            dragging_tab: None,
            dragging_gutter: None,
            hovered_gutter: None,
            hovered_strip_item: None,
            held_strip_item: None,
            tab_scroll_accum: 0.0,
            content_scroll_accum: 0.0,
            swipe_consumed: false,
            help_open: false,
            settings_open: false,
            settings_sel: 0,
            settings_saved: false,
            menu_open: false,
            menu_sel: 0,
            menu_items: None,
            menu_anchor: None,
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
            active_busy_until: None,
            spinner_frame: 0,
            spinner_at: Instant::now() + SPINNER_INTERVAL,
            capture: std::env::var_os("GLASSY_CAPTURE").map(std::path::PathBuf::from),
            capture_deadline: None,
            force_full_redraw: true,
            prev_cursor: None,
            prev_display_offset: 0,
            prev_has_selection: false,
            gui_pressed: None,
            gui_focused: None,
            gui_anims: std::collections::HashMap::new(),
            gui_click_edge: false,
            gui_anim_last: Instant::now(),
            gui_demo: std::env::var_os("GLASSY_GUI_DEMO").is_some(),
        }
    }

    /// Compute grid dimensions for a physical surface size and the cell metrics.
    /// The renderer insets the grid by `pad` px on all four sides, so the usable
    /// area is reduced by `2 * pad` in each dimension.
    fn grid_for(size: PhysicalSize<u32>, cell_w: f32, cell_h: f32, pad: f32) -> (usize, usize) {
        let usable_w = (size.width as f32 - 2.0 * pad).max(0.0);
        let usable_h = (size.height as f32 - 2.0 * pad).max(0.0);
        let cols = ((usable_w / cell_w).floor() as usize).max(1);
        // Reserve the GUI tab bar (in PIXELS) at the top; the terminal grid fills
        // the rest. The bar is inset via `Renderer::set_grid_origin_y`.
        let rows = (((usable_h - tab_bar_h(cell_h)) / cell_h).floor() as usize).max(1);
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

    /// Reflect the active tab in the native (CSD) window title.
    fn update_window_title(&self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        window.set_title(&os_title(&self.active_title));
    }

    /// Tab descriptors in stable display order: (title, is_active, has_activity).
    /// Shared by the tab-bar painter and the click/drag hit-tests so the drawn
    /// items and the click targets always agree.
    fn tab_descs(&self) -> Vec<(String, bool, bool)> {
        self.tab_order
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
            .collect()
    }

    /// The live pixel tab-bar layout, built from the current descriptors and the
    /// renderer's surface width + cell metrics. Empty if the renderer is absent.
    fn tab_layout(&self) -> Vec<StripSeg> {
        let Some(r) = self.renderer.as_ref() else {
            return Vec::new();
        };
        let m = r.cell_metrics();
        let (sw, _sh) = r.surface_size();
        let descs = self.tab_descs();
        let refs: Vec<(&str, bool, bool)> =
            descs.iter().map(|(t, a, b)| (t.as_str(), *a, *b)).collect();
        strip_layout(&refs, sw as f32, tab_bar_h(m.height), m.width)
    }

    /// The tab-bar item at physical pixel `(px, py)`, if any. Shared by click +
    /// drag-reorder so they agree with what's painted.
    fn strip_item_at_px(&self, px: f32, py: f32) -> Option<StripItem> {
        strip_item_at(&self.tab_layout(), px, py)
    }

    /// While a tab is held (`dragging_tab`), reorder it under the pointer at pixel
    /// `(px, py)`: if the pointer is over a different tab slot, move the dragged
    /// tab there in `tab_order`. Returns true if a reorder happened (repaint).
    fn drag_tab_to(&mut self, px: f32, py: f32) -> bool {
        let Some(from) = self.dragging_tab else {
            return false;
        };
        let to = match self.strip_item_at_px(px, py) {
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

    /// Invoke a menu action and close the dropdown.
    fn invoke_menu_action(&mut self, action: MenuAction, event_loop: &ActiveEventLoop) {
        self.menu_open = false;
        self.menu_items = None;
        self.menu_anchor = None;
        self.force_full_redraw = true;
        match action {
            MenuAction::Copy => {
                self.copy_selection();
                self.mark_dirty(event_loop);
            }
            MenuAction::Paste => {
                self.paste_clipboard();
                self.mark_dirty(event_loop);
            }
            MenuAction::NewTab => self.new_tab(event_loop),
            MenuAction::Settings => {
                self.settings_open = true;
                self.settings_sel = 0;
                self.settings_saved = false;
                self.mark_dirty(event_loop);
            }
            MenuAction::Help => {
                self.help_open = true;
                self.mark_dirty(event_loop);
            }
            MenuAction::CloseTab => self.close_active_tab(event_loop),
        }
    }

    /// Build the selection-aware item list for the right-click context menu.
    /// Copy is included only when a non-empty selection exists; Paste and New
    /// tab are always present. Settings/Help/CloseTab are omitted from the
    /// context menu (available via the hamburger).
    fn context_menu_items(&self) -> Vec<MenuAction> {
        let mut v = Vec::new();
        let has_sel = self
            .pty
            .as_ref()
            .and_then(|p| p.term.lock().selection_to_string())
            .filter(|s| !s.is_empty())
            .is_some();
        if has_sel {
            v.push(MenuAction::Copy);
        }
        v.push(MenuAction::Paste);
        v.push(MenuAction::NewTab);
        v
    }

    /// Open the right-click context menu anchored at the current pointer cell,
    /// with screen-edge clamping so the panel stays fully on-screen.
    fn open_context_menu(&mut self, event_loop: &ActiveEventLoop) {
        let items = self.context_menu_items();
        if items.is_empty() {
            return; // guard: Paste+NewTab are always present, so never fires
        }

        // Anchor at the pointer cell in screen coordinates (strip row included).
        let (col, term_row) = self.px_to_cell(self.mouse_px.0, self.mouse_px.1);
        let anchor_col = col;
        let anchor_row = term_row + TAB_STRIP_ROWS;

        // Clamp so the panel stays on-screen.
        let label_w = items.iter().map(|a| a.label().len()).max().unwrap_or(0);
        let panel_w = label_w + 4;
        let panel_h = items.len() + 2;
        let total_rows = self.rows + TAB_STRIP_ROWS;
        let left = anchor_col.min(self.cols.saturating_sub(panel_w));
        let top = anchor_row
            .min(total_rows.saturating_sub(panel_h))
            .max(TAB_STRIP_ROWS);

        self.menu_items = Some(items);
        self.menu_anchor = Some((left, top));
        self.menu_sel = 0;
        self.menu_open = true;
        self.help_open = false;
        self.settings_open = false;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Close the dropdown menu and clear all associated state. Use this
    /// everywhere `menu_open` is set false so `menu_items`/`menu_anchor` never
    /// drift out of sync.
    fn close_menu(&mut self, event_loop: &ActiveEventLoop) {
        self.menu_open = false;
        self.menu_items = None;
        self.menu_anchor = None;
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Handle a keypress while the dropdown menu is open. Returns true if the
    /// key was consumed (caller should not forward to the child). Uses the live
    /// item list so navigation wraps correctly for both the hamburger (fixed 4
    /// items) and the context menu (variable length).
    fn handle_menu_key(&mut self, key: &Key, event_loop: &ActiveEventLoop) -> bool {
        let n = self.menu_items.as_ref().map(|v| v.len()).unwrap_or(MenuAction::ALL.len());
        match key {
            Key::Named(NamedKey::ArrowUp) => {
                self.menu_sel = (self.menu_sel + n - 1) % n;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                true
            }
            Key::Named(NamedKey::ArrowDown) => {
                self.menu_sel = (self.menu_sel + 1) % n;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
                true
            }
            Key::Named(NamedKey::Enter) => {
                let items = self.menu_items.clone();
                let action = match &items {
                    Some(v) => v.get(self.menu_sel).copied(),
                    None => MenuAction::ALL.get(self.menu_sel).copied(),
                };
                if let Some(a) = action {
                    self.invoke_menu_action(a, event_loop);
                }
                true
            }
            Key::Named(NamedKey::Escape) => {
                self.close_menu(event_loop);
                true
            }
            _ => false,
        }
    }

    /// Hit-test a mouse click at physical pixel `(x, y)` against the open
    /// dropdown menu. Returns the action if a row was clicked, `None` otherwise.
    /// Reads `menu_items`/`menu_anchor` so it works for both the hamburger and
    /// the pointer-anchored context menu.
    fn menu_hit_test(&self, x: f64, y: f64) -> Option<MenuAction> {
        let renderer = self.renderer.as_ref()?;
        let m = renderer.cell_metrics();
        let pad = renderer.pad() as f64;
        let items: &[MenuAction] = self.menu_items.as_deref().unwrap_or(MenuAction::ALL);
        let (left, top) = self.menu_anchor?;
        let label_w = items.iter().map(|a| a.label().len()).max().unwrap_or(0);
        let panel_w = label_w + 4;

        // Panel occupies screen columns [left, left+panel_w).
        let px_left = left as f64 * m.width as f64 + pad;
        let px_right = (left + panel_w) as f64 * m.width as f64 + pad;
        if x < px_left || x >= px_right {
            return None;
        }
        // Items occupy screen rows [top+1 .. top+1+items.len()) (top row = border).
        let top_px = (top as f64 + 1.0) * m.height as f64 + pad;
        let row_rel = ((y - top_px) / m.height as f64).floor();
        if row_rel < 0.0 || row_rel >= items.len() as f64 {
            return None;
        }
        items.get(row_rel as usize).copied()
    }

    fn strip_click(&mut self, event_loop: &ActiveEventLoop) -> bool {
        let Some(renderer) = self.renderer.as_ref() else {
            return false;
        };
        let m = renderer.cell_metrics();
        let (x, y) = self.mouse_px;
        // The tab bar occupies the pixel band [0, tab_bar_h).
        if y >= tab_bar_h(m.height) as f64 {
            return false;
        }
        // Hit-test against the pixel tab-bar layout (the same helper the painter
        // uses, so click targets match what's drawn).
        let item = self.strip_item_at_px(x as f32, y as f32);
        // Record the pressed item so the strip draws it inset (released in the
        // MouseInput handler), giving the click visible tactility.
        self.held_strip_item = item;
        match item {
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
                self.menu_open = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            Some(StripItem::Settings) => {
                self.settings_open = !self.settings_open;
                self.settings_sel = 0;
                self.settings_saved = false;
                self.help_open = false;
                self.menu_open = false;
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
            Some(StripItem::Menu) => {
                // Toggle the hamburger dropdown; close other overlays.
                self.menu_open = !self.menu_open;
                self.menu_sel = 0;
                if self.menu_open {
                    // Hamburger: uses MenuAction::ALL; anchor top-right below strip.
                    self.menu_items = None;
                    let label_w =
                        MenuAction::ALL.iter().map(|a| a.label().len()).max().unwrap_or(0);
                    let panel_w = label_w + 4;
                    self.menu_anchor = Some((self.cols.saturating_sub(panel_w), TAB_STRIP_ROWS));
                } else {
                    self.menu_items = None;
                    self.menu_anchor = None;
                }
                self.help_open = false;
                self.settings_open = false;
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
        // Drop any gutter drag/hover (layout may have changed) and restore the
        // default OS cursor; the next CursorMoved re-arms feedback if warranted.
        if self.dragging_gutter.take().is_some() || self.hovered_gutter.take().is_some() {
            self.apply_gutter_cursor(None);
        }
    }

    /// Open a new tab and make it active, parking the current tab in `background`.
    fn new_tab(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_ref() else {
            return;
        };
        let m = renderer.cell_metrics();
        let id = self.next_id;
        // Inherit the current tab's cwd (from OSC 7) so the new tab opens where the
        // user is, not in $HOME.
        let cwd = self.active_cwd.clone();
        let pty = match Pty::spawn(
            self.proxy.clone(),
            id,
            self.cols,
            self.rows,
            m.width.round() as u16,
            m.height.round() as u16,
            self.config.shell.clone(),
            cwd.clone(),
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
                panes: self.panes.take(),
                title: std::mem::take(&mut self.active_title),
                activity: false,
                // Carry the parked session's busy state so its chip keeps spinning.
                busy_until: self.active_busy_until.take(),
                last_cwd: self.active_cwd.take(),
            });
        }
        self.tab_order.push(id);
        self.pty = Some(pty);
        self.active_id = id;
        self.active_title.clear();
        self.active_busy_until = None;
        // The new tab starts at the inherited cwd; OSC 7 updates it as the user cd's.
        self.active_cwd = cwd;
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
                panes: self.panes.take(),
                title: std::mem::take(&mut self.active_title),
                activity: false,
                // Carry the parked session's busy state so its chip keeps spinning.
                busy_until: self.active_busy_until.take(),
                last_cwd: self.active_cwd.take(),
            });
        }
        let bi = self.background.iter().position(|s| s.id == target_id).unwrap_or(bi);
        let target = self.background.remove(bi);
        self.pty = Some(target.pty);
        self.panes = target.panes;
        self.active_id = target.id;
        self.active_title = target.title;
        // Inherit the activated session's busy deadline (it streams in the fg now).
        self.active_busy_until = target.busy_until;
        // Restore the activated session's cwd so a new tab/split inherits it.
        self.active_cwd = target.last_cwd;
        // A split tab may have been parked at a different window size; re-tile it.
        if self.panes.is_some() {
            self.resize_panes();
        }
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
            // Shut down every other pane of this tab too.
            if let Some(g) = self.panes.take() {
                for (_, p) in g.others {
                    p.shutdown();
                }
            }
            self.pty = None;
            self.active_title.clear();
            self.active_cwd = None; // the closed tab's cwd is gone
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
                self.panes = next.panes;
                self.active_id = next.id;
                self.active_title = next.title;
                self.active_cwd = next.last_cwd;
                if self.panes.is_some() {
                    self.resize_panes();
                }
            }
        } else if let Some(bi) = self.background.iter().position(|s| s.id == id) {
            // Closing a background tab: shut it (and all its panes) down and drop it.
            let s = self.background.remove(bi);
            s.pty.shutdown();
            if let Some(g) = s.panes {
                for (_, p) in g.others {
                    p.shutdown();
                }
            }
        }
        self.reset_pointer_state();
        self.update_window_title();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    // --- Split panes -------------------------------------------------------
    //
    // The active tab may be tiled into several panes via `self.panes`. The
    // FOCUSED pane's PTY is always `self.pty`, so every single-pane code path
    // (input, selection, scrollback, mouse-report, cursor) automatically targets
    // the focused pane with no changes. `panes == None` is the one-pane case and
    // is byte-identical to the pre-split app.

    /// Pixel gutter reserved between tiled panes (also the divider thickness).
    const PANE_GAP: i32 = 1;

    /// The content rectangle (surface pixels) that panes tile: the whole surface
    /// below the tab strip. Each pane is internally inset by the renderer pad (the
    /// renderer adds `pad` to every cell), so this spans edge-to-edge and the pad
    /// supplies the symmetric margin within each pane. Returns `None` before the
    /// renderer exists.
    fn content_area(&self) -> Option<pane::Rect> {
        let r = self.renderer.as_ref()?;
        let m = r.cell_metrics();
        // The content (panes/grid) begins below the GUI tab bar. The pixel inset
        // is the bar height; the per-pane `pad` is applied by the pane sizing math.
        let strip_bottom = tab_bar_h(m.height).round() as i32;
        let (sw, sh) = r.surface_size();
        Some(pane::Rect {
            x: 0,
            y: strip_bottom,
            w: sw as i32,
            h: (sh as i32 - strip_bottom).max(0),
        })
    }

    /// Convert a pane's pixel rect into a (cols, rows) grid size for its PTY. The
    /// renderer insets cells by `pad` on the top-left, so a pane's usable extent
    /// is its rect minus one pad on each side (mirroring the whole-window inset).
    fn pane_grid(&self, rect: pane::Rect) -> (usize, usize) {
        let Some(r) = self.renderer.as_ref() else {
            return (1, 1);
        };
        let m = r.cell_metrics();
        let pad = r.pad();
        let cols = (((rect.w as f32 - 2.0 * pad) / m.width).floor() as usize).max(1);
        let rows = (((rect.h as f32 - 2.0 * pad) / m.height).floor() as usize).max(1);
        (cols, rows)
    }

    /// Resize every pane's PTY to match its current tiled rectangle. The FOCUSED
    /// pane drives `self.cols/self.rows` (so the single-pane render path and all
    /// cell math keep using the focused pane's grid); the others are sized to
    /// their own rects directly. A no-op (single-pane handling) when not split.
    fn resize_panes(&mut self) {
        let Some(area) = self.content_area() else { return };
        // Collect rects first to drop the immutable `self` borrow before mutating.
        let rects: Vec<(usize, pane::Rect)> = match self.panes.as_ref() {
            Some(g) => g.layout.rects(area, Self::PANE_GAP),
            None => return,
        };
        let Some(r) = self.renderer.as_ref() else { return };
        let m = r.cell_metrics();
        let (cw, ch) = (m.width.round() as u16, m.height.round() as u16);
        let focused = self.panes.as_ref().unwrap().layout.focused();
        for (id, rect) in rects {
            let (cols, rows) = self.pane_grid(rect);
            if id == focused {
                if let Some(pty) = &self.pty {
                    pty.resize(cols, rows, cw, ch);
                }
                // The focused pane is the one the single-pane paths read from.
                self.cols = cols;
                self.rows = rows;
            } else if let Some(pty) = self.panes.as_ref().unwrap().others.get(&id) {
                pty.resize(cols, rows, cw, ch);
            }
        }
    }

    /// Split the focused pane in `dir`, spawning a fresh shell for the new pane
    /// and focusing it. Promotes a single-pane tab into a `PaneGroup` on the
    /// first split. Re-points `self.pty` at the (new) focused pane.
    fn split_pane(&mut self, dir: pane::Dir, event_loop: &ActiveEventLoop) {
        if self.renderer.is_none() || self.pty.is_none() {
            return;
        }
        let new_id = self.next_id;
        let m = self.renderer.as_ref().unwrap().cell_metrics();
        // The new pane inherits the focused pane's cwd (from OSC 7).
        let cwd = self.active_cwd.clone();
        let pty = match Pty::spawn(
            self.proxy.clone(),
            new_id,
            self.cols,
            self.rows,
            m.width.round() as u16,
            m.height.round() as u16,
            self.config.shell.clone(),
            cwd,
            self.config.scrollback,
        ) {
            Ok(p) => p,
            Err(e) => {
                log::error!("failed to spawn pane: {e:#}");
                return;
            }
        };
        self.next_id += 1;

        // Promote to a PaneGroup whose sole leaf is the current focused pane.
        if self.panes.is_none() {
            self.panes = Some(PaneGroup {
                layout: pane::Layout::new(self.active_id),
                others: HashMap::new(),
            });
        }
        // The pane currently in `self.pty` is the focused leaf; park it as an
        // "other" and make the freshly-spawned pane the new focused `self.pty`.
        let g = self.panes.as_mut().unwrap();
        let prev_focus = g.layout.focused();
        if !g.layout.split(dir, new_id) {
            // Couldn't split (shouldn't happen for a fresh id); drop the new pty.
            pty.shutdown();
            return;
        }
        if let Some(old) = self.pty.take() {
            g.others.insert(prev_focus, old);
        }
        self.pty = Some(pty);

        self.resize_panes();
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Move focus to the neighbouring pane in direction `m` (Alt+Arrow). Swaps
    /// `self.pty` with the newly-focused pane's parked PTY. No-op when not split.
    fn focus_pane(&mut self, m: pane::Move, event_loop: &ActiveEventLoop) {
        let Some(area) = self.content_area() else { return };
        let Some(g) = self.panes.as_mut() else { return };
        let prev = g.layout.focused();
        let Some(next) = g.layout.focus_move(m, area, Self::PANE_GAP) else {
            return;
        };
        if next == prev {
            return;
        }
        // Swap the previously-focused PTY out and the newly-focused one in.
        if let Some(old) = self.pty.take() {
            g.others.insert(prev, old);
        }
        if let Some(p) = g.others.remove(&next) {
            self.pty = Some(p);
        }
        // The focused pane defines the active grid dims; re-sync them.
        self.resize_panes();
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Close the focused pane. When more than one pane remains, the focused pane's
    /// shell is shut down, the layout collapses, and focus moves to the promoted
    /// sibling. When only one pane is left, falls back to closing the whole tab.
    fn close_pane(&mut self, event_loop: &ActiveEventLoop) {
        let n = self.panes.as_ref().map(|g| g.layout.len()).unwrap_or(1);
        if n <= 1 {
            self.close_active_tab(event_loop);
            return;
        }
        let g = self.panes.as_mut().unwrap();
        let closing = g.layout.focused();
        if !g.layout.close(closing) {
            return;
        }
        let new_focus = g.layout.focused();
        // Shut down the closed pane's shell (it was the focused `self.pty`).
        if let Some(old) = self.pty.take() {
            old.shutdown();
        }
        // Bring the promoted pane's PTY in as the new focus.
        if let Some(p) = g.others.remove(&new_focus) {
            self.pty = Some(p);
        }
        // Collapse back to single-pane if only one leaf remains.
        if g.layout.len() == 1 {
            self.panes = None;
            // The PTY now in `self.pty` is the sole pane; resize it to the full
            // content area (the single-pane resize uses self.cols/self.rows, which
            // handle_resize keeps current).
            if let Some(area) = self.content_area()
                && let Some(pty) = &self.pty
            {
                let (cols, rows) = self.pane_grid(area);
                let m = self.renderer.as_ref().unwrap().cell_metrics();
                pty.resize(cols, rows, m.width.round() as u16, m.height.round() as u16);
                self.cols = cols;
                self.rows = rows;
            }
        } else {
            self.resize_panes();
        }
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Look up a pane id in the active tab by the pointer position, returning the
    /// pane id and its rect. `None` when not split or the pointer is outside any
    /// pane. Used to route wheel/clicks to the pane under the cursor.
    fn pane_at(&self, x: f64, y: f64) -> Option<(usize, pane::Rect)> {
        let area = self.content_area()?;
        let g = self.panes.as_ref()?;
        let (xi, yi) = (x.round() as i32, y.round() as i32);
        g.layout
            .rects(area, Self::PANE_GAP)
            .into_iter()
            .find(|(_, r)| xi >= r.x && xi < r.x + r.w && yi >= r.y && yi < r.y + r.h)
    }

    /// Whether the active tab is currently split into more than one pane.
    fn is_split(&self) -> bool {
        self.panes.as_ref().is_some_and(|g| g.layout.len() > 1)
    }

    /// The pixel rect of the FOCUSED pane in the active split. `None` when not
    /// split. Used to translate pointer positions into focused-pane-local cells.
    fn focused_pane_rect(&self) -> Option<pane::Rect> {
        let area = self.content_area()?;
        let g = self.panes.as_ref()?;
        let f = g.layout.focused();
        g.layout
            .rects(area, Self::PANE_GAP)
            .into_iter()
            .find(|(id, _)| *id == f)
            .map(|(_, r)| r)
    }

    /// Focus the pane under the pointer (if any) when split, swapping `self.pty`.
    /// Returns true when focus actually changed (caller should repaint). No-op
    /// (false) when not split or the pointer is over the already-focused pane.
    fn focus_pane_at(&mut self, x: f64, y: f64, event_loop: &ActiveEventLoop) -> bool {
        let Some((id, _)) = self.pane_at(x, y) else {
            return false;
        };
        let Some(g) = self.panes.as_mut() else { return false };
        let prev = g.layout.focused();
        if id == prev {
            return false;
        }
        if !g.layout.focus(id) {
            return false;
        }
        if let Some(old) = self.pty.take() {
            g.others.insert(prev, old);
        }
        if let Some(p) = g.others.remove(&id) {
            self.pty = Some(p);
        }
        self.resize_panes();
        self.reset_pointer_state();
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
        true
    }

    /// Pixel tolerance around a 1px gutter for hit-testing / cursor feedback. The
    /// drawn divider stays crisp at `PANE_GAP`; this widens only the grab zone so a
    /// ~9px band (±4px) is draggable, matching the "thin line, fat hitbox" pattern.
    const GUTTER_TOL: i32 = 4;

    /// Minimum pane extents (px) enforced while dragging a gutter, so a drag can
    /// never crush a pane below a usable size.
    const PANE_MIN_PX: i32 = 120;

    /// Hit-test the resize gutters of the active split at pointer `(x, y)`,
    /// returning the handle under it (within [`GUTTER_TOL`]). `None` when not split
    /// or off any gutter.
    fn gutter_at(&self, x: f64, y: f64) -> Option<pane::SplitHandle> {
        if !self.is_split() {
            return None;
        }
        let area = self.content_area()?;
        let g = self.panes.as_ref()?;
        g.layout
            .split_at(area, Self::PANE_GAP, x.round() as i32, y.round() as i32, Self::GUTTER_TOL)
    }

    /// While `dragging_gutter` is held, map the pointer to a new ratio for that
    /// divider and re-tile. The ratio is clamped so neither side falls below
    /// [`PANE_MIN_PX`]. Returns true when the layout changed (caller repaints).
    fn drag_gutter_to(&mut self, x: f64, y: f64) -> bool {
        let Some(handle) = self.dragging_gutter.clone() else {
            return false;
        };
        if handle.axis_len <= 0 {
            return false;
        }
        let pointer = match handle.dir {
            pane::Dir::Vertical => x.round() as i32,
            pane::Dir::Horizontal => y.round() as i32,
        };
        let raw = (pointer - handle.axis_start) as f32 / handle.axis_len as f32;
        // Clamp in ratio space so both children keep at least PANE_MIN_PX.
        let min_r = Self::PANE_MIN_PX as f32 / handle.axis_len as f32;
        let lo = min_r;
        let hi = 1.0 - min_r;
        let ratio = if lo <= hi { raw.clamp(lo, hi) } else { 0.5 };
        let Some(g) = self.panes.as_mut() else { return false };
        if g.layout.set_ratio(&handle.path, ratio) {
            self.resize_panes();
            self.force_full_redraw = true;
            true
        } else {
            false
        }
    }

    /// Set (or clear) the OS pointer cursor to a resize arrow for a gutter handle.
    fn apply_gutter_cursor(&self, handle: Option<&pane::SplitHandle>) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        use winit::window::CursorIcon;
        let icon = match handle.map(|h| h.dir) {
            Some(pane::Dir::Vertical) => CursorIcon::ColResize,
            Some(pane::Dir::Horizontal) => CursorIcon::RowResize,
            None => CursorIcon::Default,
        };
        window.set_cursor(icon);
    }

    /// Find a PTY by its pane/pty id anywhere it might live: the active focused
    /// pane, a non-focused pane of the active tab, or any pane (focused or not) of
    /// a background tab. Used to route PTY-keyed events (VT replies, etc.) to the
    /// correct pane regardless of which tab or split it belongs to.
    fn pty_by_id(&self, id: usize) -> Option<&Pty> {
        if id == self.active_id {
            return self.pty.as_ref();
        }
        if let Some(g) = self.panes.as_ref()
            && let Some(p) = g.others.get(&id)
        {
            return Some(p);
        }
        for s in &self.background {
            if s.id == id {
                return Some(&s.pty);
            }
            if let Some(g) = s.panes.as_ref()
                && let Some(p) = g.others.get(&id)
            {
                return Some(p);
            }
        }
        None
    }

    /// Whether `id` names a pane (focused or not) of the ACTIVE tab — i.e. one
    /// whose output is currently visible and should trigger a repaint.
    fn id_in_active_tab(&self, id: usize) -> bool {
        id == self.active_id || self.panes.as_ref().is_some_and(|g| g.others.contains_key(&id))
    }

    /// The display position (in `tab_order`) of the tab that owns pane `id`, where
    /// the tab is identified by its FOCUSED pane id (== the tab's stable id). A
    /// non-focused pane resolves to its owning tab. `None` if unknown.
    fn tab_pos_of_pane(&self, id: usize) -> Option<usize> {
        // Active tab.
        if self.id_in_active_tab(id) {
            return Some(self.active_pos());
        }
        // Background tabs: the tab id is the focused pane id; a non-focused pane is
        // found in that session's group.
        for s in &self.background {
            let owns = s.id == id || s.panes.as_ref().is_some_and(|g| g.others.contains_key(&id));
            if owns {
                return self.tab_order.iter().position(|&t| t == s.id);
            }
        }
        None
    }

    /// Handle a child shell exit for pane `id`: close exactly that pane. A
    /// non-focused pane of the active tab collapses out of the split; the active
    /// focused pane (or a single-pane tab) closes the tab. Background-tab panes are
    /// dropped from their group (or the whole tab when it was their last pane).
    fn handle_child_exit(&mut self, id: usize, event_loop: &ActiveEventLoop) {
        // A non-focused pane of the ACTIVE tab: drop it from the split.
        if id != self.active_id
            && let Some(g) = self.panes.as_mut()
            && g.others.contains_key(&id)
        {
            if let Some(p) = g.others.remove(&id) {
                p.shutdown();
            }
            g.layout.close(id);
            let collapsed = g.layout.len() == 1;
            if collapsed {
                self.panes = None;
            }
            self.resize_panes();
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
            return;
        }
        // The active focused pane: if the tab is split, close just that pane;
        // otherwise close the whole tab (the original single-pane behaviour).
        if id == self.active_id {
            if self.is_split() {
                self.close_pane(event_loop);
            } else {
                self.close_active_tab(event_loop);
            }
            return;
        }
        // A background tab's pane.
        for s in self.background.iter_mut() {
            if let Some(g) = s.panes.as_mut()
                && g.others.contains_key(&id)
            {
                if let Some(p) = g.others.remove(&id) {
                    p.shutdown();
                }
                g.layout.close(id);
                if g.layout.len() == 1 {
                    s.panes = None;
                }
                return;
            }
        }
        // A background tab's focused pane (id == tab id): drop the whole tab,
        // shutting down any sibling panes it owned. (Matches the pre-split
        // behaviour, which left `tab_order` untouched here.)
        if let Some(bi) = self.background.iter().position(|s| s.id == id) {
            let s = self.background.remove(bi);
            if let Some(g) = s.panes {
                for (_, p) in g.others {
                    p.shutdown();
                }
            }
            self.update_window_title();
        }
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
    /// When the active tab is split, the cell is taken relative to the FOCUSED
    /// pane's tile (origin = rect + pad), since selection / mouse-reporting act on
    /// the focused pane (`self.pty`) and `self.cols/self.rows` track its grid.
    fn px_to_cell(&self, x: f64, y: f64) -> (usize, usize) {
        let Some(renderer) = self.renderer.as_ref() else {
            return (0, 0);
        };
        let m = renderer.cell_metrics();
        let pad = renderer.pad() as f64;
        if let Some(rect) = self.focused_pane_rect() {
            // Pane-local: subtract the tile origin + the per-pane pad inset.
            let ox = rect.x as f64 + pad;
            let oy = rect.y as f64 + pad;
            let col = ((x - ox) / m.width as f64).floor();
            let row = ((y - oy) / m.height as f64).floor();
            let col = (col.max(0.0) as usize).min(self.cols.saturating_sub(1));
            let row = (row.max(0.0) as usize).min(self.rows.saturating_sub(1));
            return (col, row);
        }
        let col = ((x - pad) / m.width as f64).floor();
        // The terminal grid starts below the GUI tab bar: subtract its pixel inset.
        let grid_top = pad + tab_bar_h(m.height) as f64;
        let term_row = ((y - grid_top) / m.height as f64).floor() as i64;
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
        // In a split, measure from the focused pane's tile origin (matching
        // `px_to_cell`), so the sub-cell boundary test stays correct per pane.
        let ox = self
            .focused_pane_rect()
            .map(|r| r.x as f64 + pad)
            .unwrap_or(pad);
        let rel = (x - ox) / m.width as f64;
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

    /// Build the inline app toolbar (screen row 0) cells: the glassy mark, tab
    /// chips (or the single-tab title), the +/help/menu buttons, and the
    /// scrollback-position readout. Returns one `(char, fg, bg)` per column so
    /// both the single-pane and split render paths push an identical strip. Takes
    /// the focused pane's `display_offset`/`history_size` for the % readout.
    /// Snapshot the tab state needed by [`paint_tab_bar`] under the live `&self`
    /// borrow, so the painter (which holds `&mut Renderer`, a split borrow of
    /// `self.renderer`) needs only owned data. Returns per-tab (title, active,
    /// busy-dot, spinning) tuples in stable display order.
    fn tab_bar_snapshot(&self) -> Vec<(String, bool, bool, bool)> {
        let now = Instant::now();
        self.tab_order
            .iter()
            .map(|&id| {
                if id == self.active_id {
                    let spinning = self.active_busy_until.is_some_and(|t| now < t);
                    (self.active_title.clone(), true, false, spinning)
                } else {
                    self.background
                        .iter()
                        .find(|s| s.id == id)
                        .map(|s| {
                            let spinning = s.busy_until.is_some_and(|t| now < t);
                            (s.title.clone(), false, s.activity, spinning)
                        })
                        .unwrap_or((String::new(), false, false, false))
                }
            })
            .collect()
    }

    /// Paint the real GUI tab bar (§3.1) into the pixel band `[0, tab_bar_h)`. The
    /// ACTIVE tab is an E2 raised chip whose top corners are rounded and whose body
    /// is flush to the bar bottom, with a 3px connector quad that overpaints the
    /// content hairline so the tab "opens into" the content surface, plus a top
    /// accent rail. Inactive tabs are recessed E1 chips sitting above the bar
    /// bottom. The close button fades in on hover with its own danger-tinted
    /// hover/press state. +/?/⚙/≡ are icon buttons. The rich (Unicode) title is
    /// drawn through the glyph atlas via `push_overlay_glyph_px` (tofu-proof). A
    /// held tab is lifted to a drag-ghost following the pointer.
    ///
    /// Associated (not `&self`) so it composes with the caller's `&mut Renderer`
    /// borrow; all `self`-derived data arrives via `snapshot` + scalars.
    #[allow(clippy::too_many_arguments)]
    fn paint_tab_bar(
        renderer: &mut Renderer,
        snapshot: &[(String, bool, bool, bool)],
        focused: bool,
        hovered: Option<StripItem>,
        held: Option<StripItem>,
        dragging: Option<usize>,
        mouse_px: (f32, f32),
        spinner_frame: usize,
        tab_count: usize,
        display_offset: i32,
        history_size: usize,
    ) {
        let m = renderer.cell_metrics();
        let (sw, _sh) = renderer.surface_size();
        let bar_w = sw as f32;
        let bar_h = tab_bar_h(m.height);
        if bar_w <= 0.0 || bar_h <= 0.0 {
            return;
        }

        // Dim the whole bar while unfocused so an idle window reads as "asleep".
        let fdim = if focused { 1.0 } else { 0.7 };
        let mul = |c: [f32; 4]| [c[0] * fdim, c[1] * fdim, c[2] * fdim, c[3]];

        let bar_bg = mul(gui::glass_body());
        let surface = mul(gui::glass_raised());
        let accent = mul(color::accent());
        let danger = mul(color::danger());
        let fg = mul(gui::fg());
        let fg_dim = mul(gui::fg_dim());
        let active_fg = mul(color::default_bg()); // dark text on the accent rail tab

        // 1) Bar backdrop (E1) + top accent rail + bottom hairline (content seam).
        renderer.push_overlay_px(0.0, 0.0, bar_w, bar_h, bar_bg);
        renderer.push_overlay_px(0.0, 0.0, bar_w, 1.0, mul(gui::rail()));
        renderer.push_overlay_px(0.0, bar_h - 1.0, bar_w, 1.0, mul(gui::hairline()));

        // 2) Brand mark on the far left.
        let mark_y = (bar_h - m.height) * 0.5;
        renderer.push_overlay_glyph_px((m.width).round(), mark_y.round(), '◆', accent);

        // 3) Lay out the bar (pixel rects) and paint each item.
        let descs: Vec<(&str, bool, bool)> =
            snapshot.iter().map(|(t, a, b, _)| (t.as_str(), *a, *b)).collect();
        let segs = strip_layout(&descs, bar_w, bar_h, m.width);
        let multi = descs.len() > 1;
        let spin = SPINNER_FRAMES[spinner_frame % SPINNER_FRAMES.len()];

        // Helper: state-driven fill for a control/chip surface.
        let press_fill = |base: [f32; 4]| {
            [base[0] * 0.85, base[1] * 0.85, base[2] * 0.85, base[3]]
        };
        let hover_fill = |base: [f32; 4]| gui::state_fill(base, 0.7, false);

        // Track a held tab so we can defer its drag-ghost to the very top.
        let mut ghost: Option<(gui::Rect, String, usize)> = None;

        for seg in &segs {
            let r = seg.rect;
            let is_hover = hovered == Some(seg.item);
            let is_held = held == Some(seg.item);
            match seg.item {
                StripItem::Tab(i) => {
                    let (_title, active, busy) = descs.get(i).copied().unwrap_or(("", false, false));
                    let is_spinning = snapshot.get(i).map(|s| s.3).unwrap_or(false);
                    // A dragged tab is rendered last as a ghost; reserve it.
                    if dragging == Some(i) {
                        ghost = Some((r, seg.label.clone(), i));
                    }
                    Self::paint_tab_chip(
                        renderer, r, m.height, m.width, i, &seg.label, active, busy, is_spinning,
                        is_hover, is_held, spin, bar_h, surface, accent, active_fg, fg, fg_dim,
                        dragging == Some(i), multi,
                    );
                }
                StripItem::TabClose(i) => {
                    let active = descs.get(i).map(|d| d.1).unwrap_or(false);
                    // Close fades in on hover of either the close box or its tab.
                    let tab_hover = hovered == Some(StripItem::Tab(i)) || is_hover;
                    if tab_hover && dragging.is_none() {
                        if is_hover {
                            let a = if is_held { 0.30 } else { 0.18 };
                            renderer.push_overlay_rrect_px(
                                r.x, r.y, r.w, r.h, 3.0,
                                [danger[0], danger[1], danger[2], a],
                            );
                        }
                        let cfg = if is_hover { danger } else if active { active_fg } else { fg_dim };
                        let gx = r.x + (r.w - m.width) * 0.5;
                        let gy = r.center_y() - m.height * 0.5;
                        renderer.push_overlay_glyph_px(gx.round(), gy.round(), '✕', cfg);
                    }
                }
                StripItem::NewTab | StripItem::Help | StripItem::Settings | StripItem::Menu => {
                    let glyph = match seg.item {
                        StripItem::NewTab => '+',
                        StripItem::Help => '?',
                        StripItem::Settings => '⚙',
                        _ => '≡',
                    };
                    let base = surface;
                    if is_held {
                        renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, gui_radius(m.height), press_fill(base));
                    } else if is_hover {
                        renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, gui_radius(m.height), hover_fill(base));
                        renderer.push_overlay_px(r.x, r.y, r.w, 1.0, mul(gui::rail()));
                    }
                    let nudge = if is_held { 1.0 } else { 0.0 };
                    let cfg = if is_hover || is_held { fg } else { fg_dim };
                    let gx = r.x + (r.w - m.width) * 0.5;
                    let gy = r.center_y() - m.height * 0.5 + nudge;
                    renderer.push_overlay_glyph_px(gx.round(), gy.round(), glyph, cfg);
                }
            }
        }

        // 4) Drag-ghost: redraw the held tab lifted to the top, following the
        //    pointer's x, so it visibly floats above its siblings while reordering.
        if let Some((r, label, i)) = ghost {
            let gx = (mouse_px.0 - r.w * 0.5).clamp(0.0, bar_w - r.w);
            let gr = gui::Rect::new(gx, r.y - 2.0, r.w, r.h);
            renderer.push_overlay_rrect_px(gr.x, gr.y, gr.w, gr.h, TAB_RADIUS, mul(gui::glass_float()));
            renderer.push_overlay_px(gr.x, gr.y, gr.w, 2.0, accent);
            Self::paint_tab_label(renderer, gr, m.height, m.width, i, &label, true, false, false, spin, active_fg, active_fg, multi);
        }

        // 5) Tab-count badge + scrollback %, tucked just left of the right controls
        //    as dim labels (fixed-width so the controls never shift).
        let right_ctrl_x = bar_w - CTRL_BTN * 3.0 - TAB_GAP;
        let mut tag_right = right_ctrl_x - m.width;
        let ty = ((bar_h - m.height) * 0.5).round();
        if display_offset > 0 {
            let pct = if history_size > 0 {
                ((display_offset as f32 / history_size as f32) * 100.0).round() as u32
            } else {
                100
            }
            .min(100);
            let s = format!("⇡{pct:>3}%");
            let w = renderer.text_width_px(&s);
            renderer.push_overlay_glyph_px_str((tag_right - w).round(), ty, &s, accent);
            tag_right -= w + m.width;
        }
        if tab_count > 1 {
            let s = format!("{tab_count} tabs");
            let w = renderer.text_width_px(&s);
            renderer.push_overlay_glyph_px_str((tag_right - w).round(), ty, &s, fg_dim);
        }
    }

    /// Paint one tab chip's surface (connector + rail for active, recess for
    /// inactive) and its label. Split out so the drag-ghost can reuse the label
    /// pass without the surface.
    #[allow(clippy::too_many_arguments)]
    fn paint_tab_chip(
        renderer: &mut Renderer,
        r: gui::Rect,
        cell_h: f32,
        cell_w: f32,
        idx: usize,
        label: &str,
        active: bool,
        busy: bool,
        spinning: bool,
        hover: bool,
        held: bool,
        spin: char,
        bar_h: f32,
        surface: [f32; 4],
        accent: [f32; 4],
        active_fg: [f32; 4],
        fg: [f32; 4],
        fg_dim: [f32; 4],
        is_ghost: bool,
        multi: bool,
    ) {
        if is_ghost {
            return; // drawn separately as the lifted ghost
        }
        let _ = busy;
        if active {
            // E2 raised body, top corners rounded, flush to the bar bottom; a 3px
            // connector quad overpaints the content seam so the tab "opens into"
            // the content surface, and a top accent rail crowns it.
            renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, TAB_RADIUS, surface);
            // Square off the bottom so it sits flush (rrect rounds all corners): a
            // small fill over the bottom band.
            let bb = TAB_RADIUS;
            renderer.push_overlay_px(r.x, r.y + r.h - bb, r.w, bb, surface);
            // Connector: overpaint the bar's bottom hairline for the chip width.
            renderer.push_overlay_px(r.x, bar_h - 2.0, r.w, 3.0, surface);
            // Top accent rail.
            renderer.push_overlay_px(r.x, r.y, r.w, 2.0, accent);
        } else {
            // Inactive: recessed E1 chip sitting a touch above the bar bottom.
            let rr = gui::Rect::new(r.x, r.y + 2.0, r.w, r.h - 4.0);
            let fill = if held {
                [surface[0] * 0.85, surface[1] * 0.85, surface[2] * 0.85, surface[3]]
            } else if hover {
                gui::state_fill(surface, 0.7, false)
            } else {
                [surface[0], surface[1], surface[2], surface[3] * 0.75]
            };
            renderer.push_overlay_rrect_px(rr.x, rr.y, rr.w, rr.h, TAB_RADIUS, fill);
            if hover && !held {
                renderer.push_overlay_px(rr.x, rr.y, rr.w, 1.0, accent);
            }
        }
        let label_fg = if active { active_fg } else { fg };
        Self::paint_tab_label(
            renderer, r, cell_h, cell_w, idx, label, active, busy, spinning, spin, label_fg,
            accent, multi,
        );
        let _ = fg_dim;
    }

    /// Draw a tab chip's status glyph + numbered title, clipped to the chip's text
    /// area (leaving room for the close box on the right).
    #[allow(clippy::too_many_arguments)]
    fn paint_tab_label(
        renderer: &mut Renderer,
        r: gui::Rect,
        cell_h: f32,
        cell_w: f32,
        idx: usize,
        label: &str,
        active: bool,
        busy: bool,
        spinning: bool,
        spin: char,
        label_fg: [f32; 4],
        accent: [f32; 4],
        multi: bool,
    ) {
        let ty = (r.center_y() - cell_h * 0.5).round();
        let mut tx = r.x + TAB_PAD_X;
        // Leading status glyph: spinner while streaming, dot for background activity.
        if spinning {
            renderer.push_overlay_glyph_px(tx.round(), ty, spin, if active { label_fg } else { accent });
            tx += cell_w;
        } else if busy && !active {
            renderer.push_overlay_glyph_px(tx.round(), ty, '•', accent);
            tx += cell_w;
        }
        // Fit-to-width title (tail-ellipsized). A numeric prefix is shown only in
        // multi-tab mode (a lone tab is titled by the window). Reserve room for the
        // close box on the right when one exists (multi-tab only).
        let reserve = if multi { CLOSE_BOX + TAB_PAD_X } else { TAB_PAD_X };
        let text_w = (r.w - (tx - r.x) - reserve).max(0.0);
        let max_chars = (text_w / cell_w).floor() as usize;
        let s = if multi {
            let title = fit_label(label, max_chars.saturating_sub(2).max(1));
            format!("{} {}", idx + 1, title)
        } else {
            fit_label(label, max_chars.max(1))
        };
        renderer.push_overlay_glyph_px_str(tx.round(), ty, &s, label_fg);
    }

    /// Temporary Wave-0 demo: lay out a button, an icon button, a toggle, a
    /// segmented control, a slider and a stepper on a floating panel, exercising
    /// every primitive (AA rounded rects, edge-lit rails, focus rings, pixel
    /// glyphs) plus the hover/press/focus state machine and animations. Gated by
    /// GLASSY_GUI_DEMO; not part of the normal chrome path.
    #[allow(clippy::too_many_arguments)]
    fn paint_gui_demo(
        renderer: &mut Renderer,
        cell_w: f32,
        cell_h: f32,
        mouse: (f32, f32),
        mouse_down: bool,
        clicked: bool,
        gui_pressed: &mut Option<gui::WidgetId>,
        gui_focused: &mut Option<gui::WidgetId>,
        gui_anims: &mut std::collections::HashMap<gui::WidgetId, gui::Anim>,
    ) {
        let mut ui = gui::Ui::new(
            renderer,
            cell_w,
            cell_h,
            mouse,
            mouse_down,
            clicked,
            gui_pressed,
            gui_focused,
            gui_anims,
        );

        let met = ui.m;
        let panel = gui::Rect::new(60.0, 60.0, met.cell_w * 38.0, met.row_h * 12.0 + met.pad * 2.0);
        let inner = ui.panel(panel, met.card_radius);
        ui.label(inner.x, inner.y, "glassy — gui demo", gui::fg());

        let mut y = inner.y + met.row_h;
        let row = |y: f32| gui::Rect::new(inner.x, y, inner.w, met.row_h - met.gap);

        let _ = ui.button(gui::id("demo/button"), row(y), "Button");
        y += met.row_h;

        let r = row(y);
        let _ = ui.icon_button(gui::id("demo/icon"), gui::Rect::new(r.x, r.y, met.row_h, r.h), '⚙');
        y += met.row_h;

        let r = row(y);
        let _ = ui.toggle(gui::id("demo/toggle"), gui::Rect::new(r.x, r.y, met.row_h * 2.0, r.h), true);
        y += met.row_h;

        let r = row(y);
        let _ = ui.segmented(gui::id("demo/seg"), gui::Rect::new(r.x, r.y, met.ctrl_w, r.h), &["Off", "Visual", "Audible"], 1);
        y += met.row_h;

        let r = row(y);
        let _ = ui.slider(gui::id("demo/slider"), gui::Rect::new(r.x, r.y, met.ctrl_w, r.h), 0.6, 0.0, 1.0, 0.05);
        y += met.row_h;

        let r = row(y);
        let _ = ui.stepper(gui::id("demo/step"), gui::Rect::new(r.x, r.y, met.ctrl_w, r.h), "14px");
        y += met.row_h;

        let r = row(y);
        let _ = ui.dropdown(
            gui::id("demo/dropdown"),
            gui::Rect::new(r.x, r.y, met.ctrl_w, r.h),
            "Solarized",
            false,
            Some(gui::fill_on()),
        );
        y += met.row_h;

        let r = row(y);
        let _ = ui.text_field_readonly(
            gui::id("demo/field"),
            gui::Rect::new(r.x, r.y, met.ctrl_w * 1.4, r.h),
            "/home/allie/.config/glassy/glassy.toml",
            true,
            true,
        );
        y += met.row_h;

        // List + scrollbar in a small scrolling region.
        let list_h = met.row_h * 2.0;
        let bar_w = met.gap.max(6.0);
        let list_rect = gui::Rect::new(inner.x, y, met.ctrl_w, list_h);
        let _ = ui.list(
            gui::id("demo/list"),
            list_rect,
            &["one", "two", "three", "four", "five"],
            1,
            0.0,
        );
        let track = gui::Rect::new(list_rect.x + list_rect.w + 2.0, y, bar_w, list_h);
        let _ = ui.scrollbar(gui::id("demo/scroll"), track, met.row_h * 5.0, list_h, 0.0);
    }

    fn render(&mut self) {
        // Split tabs take the dedicated multi-pane path; a single-pane tab (the
        // common case) falls through to the byte-identical fast path below.
        if self.is_split() {
            self.render_split();
            self.dirty = false;
            if !self.first_frame_done {
                self.first_frame_done = true;
            }
            return;
        }

        // The GUI demo's glass panel must sit over freshly-painted terminal rows
        // (the push_overlay_px invariant), so force a full rebuild while it is on.
        if self.gui_demo {
            self.force_full_redraw = true;
        }

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

        // Snapshot tab state + scroll position up-front, under the immutable `&self`
        // borrow, so the tab-bar painter (which holds the live `&mut Renderer`) needs
        // only owned data and never collides with the renderer borrow.
        let (strip_off, strip_hist) = match self.pty.as_ref() {
            Some(pty) => {
                let t = pty.term.lock();
                (t.grid().display_offset() as i32, t.grid().history_size())
            }
            None => (0, 0),
        };
        let tab_snapshot = self.tab_bar_snapshot();
        let tab_focused = self.focused;
        let tab_hovered = self.hovered_strip_item;
        let tab_held = self.held_strip_item;
        let tab_dragging = self.dragging_tab;
        let tab_mouse = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
        let tab_spinner = self.spinner_frame;
        let tab_count = self.tab_count();

        let (Some(renderer), Some(pty)) = (self.renderer.as_mut(), self.pty.as_ref()) else {
            return;
        };
        renderer.set_flash(flash);
        // The single-pane terminal grid is inset below the GUI tab bar in PIXELS.
        renderer.set_grid_origin_y(tab_bar_h(renderer.cell_metrics().height));

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
            // The terminal grid spans exactly `rows`; the tab bar is a pixel inset
            // (grid_origin_y), not a reserved cell row, so no +1 here.
            renderer.resize_grid(rows);
            for d in dirty.iter_mut() {
                *d = true;
            }
        }

        // Track which rows we have begun this frame so a row's first cell triggers
        // `begin_row` and the cursor overlay can re-target it later.
        let mut row_started = vec![false; rows];

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
            // First cell of a dirty row: clear it and begin pushing into it. The
            // tab-bar inset is applied in pixels by the renderer (grid_origin_y),
            // so the screen row equals the terminal row.
            let srow = row_u;
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

        // The real GUI tab bar is painted as a pixel overlay near the END of the
        // frame (after the cursor + images), so it composites above everything in
        // its band; see the `paint_tab_bar` call below.

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
                // The cursor row is usually (re)built above, in which case we
                // re-target it WITHOUT clearing so the overlay appends on top of
                // that row's cell backgrounds. On a partial-dirty frame (e.g. a
                // resize) the cursor's row can have no in-bounds cells and so was
                // never begun this frame — begin it now so the overlay still
                // paints instead of being dropped for a frame.
                let scr = cr;
                if cr < rows {
                    if row_started[scr] {
                        renderer.set_cur_row(scr);
                    } else {
                        renderer.begin_row(scr);
                        row_started[scr] = true;
                    }
                    renderer.push_cursor(cc, scr, overlay, cursor_color);
                }
            }
        }

        drop(term); // release before GPU submit / present

        // Inline images (kitty graphics). Drawn as an overlay every frame from the
        // live placement list, anchored to the cell they were displayed at. The
        // stored row is viewport-relative at display time; translate by the current
        // scroll offset so images move with the buffer as the user scrolls.
        // Suppressed while a modal or dropdown is up so images don't punch through it.
        if !self.help_open && !self.settings_open && !self.menu_open {
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
                    let screen_row = screen_vp as usize;
                    let x = p.col as f32 * m.width + pad;
                    // Match push_cell's pixel origin: grid rows are inset below the
                    // GUI tab bar in pixels (grid_origin_y).
                    let y = screen_row as f32 * m.height + pad + renderer.grid_origin_y();
                    // Honor the kitty c=/r= display size (in cells); otherwise draw
                    // at the image's native pixel size.
                    let (dst_w, dst_h) =
                        image_dst_size(p.cols, p.rows, img.width, img.height, m.width, m.height);
                    renderer.draw_image(p.id, &img.rgba, img.width, img.height, x, y, dst_w, dst_h);
                }
            }
        }

        // Real GUI tab bar (§3.1): painted over the pixel band [0, tab_bar_h) as
        // overlay quads + atlas glyphs, so it composites above the grid. Drawn
        // before any modal so a modal scrim dims it too.
        Self::paint_tab_bar(
            renderer,
            &tab_snapshot,
            tab_focused,
            tab_hovered,
            tab_held,
            tab_dragging,
            tab_mouse,
            tab_spinner,
            tab_count,
            strip_off,
            strip_hist,
        );

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
        } else if self.menu_open {
            // Dropdown menu (hamburger or context): anchored panel floating above
            // terminal content. Both share the same draw function; the anchor and
            // item list differ between them.
            let items: &[MenuAction] = self.menu_items.as_deref().unwrap_or(MenuAction::ALL);
            let (left, top) = self.menu_anchor.unwrap_or((
                self.cols.saturating_sub(
                    items.iter().map(|a| a.label().len()).max().unwrap_or(0) + 4,
                ),
                TAB_STRIP_ROWS,
            ));
            draw_dropdown_menu(renderer, self.rows, self.cols, items, self.menu_sel, left, top);
        }

        // Temporary GUI-primitive demo (gated behind GLASSY_GUI_DEMO) — proves the
        // Wave-0 primitives render with correct AA corners + hover/press/focus. No
        // user-visible chrome in the normal path. Inlined here (disjoint field
        // borrows) so it can reuse the live `renderer` borrow.
        if self.gui_demo {
            let m = renderer.cell_metrics();
            let mouse = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
            let mouse_down = self.held_button == Some(0);
            Self::paint_gui_demo(
                renderer,
                m.width,
                m.height,
                mouse,
                mouse_down,
                self.gui_click_edge,
                &mut self.gui_pressed,
                &mut self.gui_focused,
                &mut self.gui_anims,
            );
        }

        // Record the state this frame drew from, so the next frame can repaint only
        // what changed (the cursor's old/new row, selection, scroll position).
        self.prev_cursor = cur_cursor_cell;
        self.prev_display_offset = display_offset;
        self.prev_has_selection = has_selection;
        self.force_full_redraw = false;
        // The chrome paint consumed this frame's click edge.
        self.gui_click_edge = false;

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

    /// Render a split (multi-pane) tab via the renderer's scissored multi-pane
    /// path: one pane per leaf clipped to its tile, the tab strip on top, the
    /// focused-pane border, and dividers between tiles. Forgoes the per-row damage
    /// machinery (rebuilds every frame) — splitting is rare and this keeps the
    /// fast single-pane path untouched.
    fn render_split(&mut self) {
        let flash = if self.bell_flash_until.is_some_and(|t| Instant::now() < t) {
            Some(bell::FLASH_COLOR)
        } else {
            None
        };
        let Some(area) = self.content_area() else { return };
        let focused_pane = match self.panes.as_ref() {
            Some(g) => g.layout.focused(),
            None => return,
        };
        // Precompute every leaf's rect + grid size (whole-`self` method calls)
        // BEFORE taking disjoint field borrows for the render loop below.
        let rects = self.panes.as_ref().unwrap().layout.rects(area, Self::PANE_GAP);
        let pane_specs: Vec<(usize, pane::Rect, usize, usize)> = rects
            .iter()
            .map(|(id, r)| {
                let (c, rw) = self.pane_grid(*r);
                (*id, *r, c, rw)
            })
            .collect();

        // Focused pane's scroll position for the strip % readout.
        let (strip_off, strip_hist) = match self.pty.as_ref() {
            Some(pty) => {
                let t = pty.term.lock();
                (t.grid().display_offset() as i32, t.grid().history_size())
            }
            None => (0, 0),
        };
        // Tab-bar state snapshot (owned data) for the pixel-overlay painter, taken
        // under the immutable `&self` borrow.
        let tab_snapshot = self.tab_bar_snapshot();
        let tab_focused = self.focused;
        let tab_hovered = self.hovered_strip_item;
        let tab_held = self.held_strip_item;
        let tab_dragging = self.dragging_tab;
        let tab_mouse = (self.mouse_px.0 as f32, self.mouse_px.1 as f32);
        let tab_spinner = self.spinner_frame;
        let tab_count = self.tab_count();
        let win_focused = self.focused;
        let blink_on = self.blink_on;
        let hovered_link = self.hovered_link.clone();
        let divider = lighten(color::selection_bg(), 0.18);
        // The gutter to draw transiently emphasised (drag wins over mere hover).
        let active_gutter = self
            .dragging_gutter
            .clone()
            .or_else(|| self.hovered_gutter.clone());
        // Its divider's primary-axis pixel position, derived from the live layout,
        // so the highlight tracks the exact line being hovered/dragged.
        let active_div = active_gutter.as_ref().and_then(|h| {
            // Re-resolve the handle's divider coordinate from the live layout so
            // the highlight tracks the exact line being hovered/dragged.
            self.panes
                .as_ref()?
                .layout
                .divider_pos(area, Self::PANE_GAP, &h.path)
                .map(|pos| (h.dir, pos))
        });
        let gutter_glow = {
            let mut c = color::accent();
            c[3] = 0.8;
            c
        };
        let gutter_tol = Self::GUTTER_TOL;

        // Disjoint field borrows: `renderer` (mut), `panes`/`pty` (shared) are
        // distinct fields, so the borrow checker allows them together as long as
        // we don't route through a whole-`self` method past this point.
        let g = self.panes.as_ref().unwrap();
        let focused_pty = self.pty.as_ref();
        let renderer = match self.renderer.as_mut() {
            Some(r) => r,
            None => return,
        };
        renderer.set_flash(flash);
        // Multi-pane: each pane carries its own pixel origin (content_area already
        // insets below the tab bar), so the per-cell grid_origin_y must be zero.
        renderer.set_grid_origin_y(0.0);
        renderer.begin_multi_frame(color::default_bg());

        // Each leaf pane, clipped to its tile.
        for (id, rect, cols, prows) in &pane_specs {
            let is_focused = *id == focused_pane;
            let pty = if is_focused {
                focused_pty
            } else {
                g.others.get(id)
            };
            let Some(pty) = pty else { continue };
            renderer.begin_pane(*rect, is_focused);
            Self::push_pane(renderer, pty, *cols, *prows, win_focused, blink_on, hovered_link.as_deref());
            renderer.end_pane();
        }

        // Dividers in the gutters between adjacent tiles (drawn full-surface
        // scissored so they are never clipped by a pane rect).
        if Self::PANE_GAP > 0 {
            for (i, (_, a)) in rects.iter().enumerate() {
                for (_, b) in rects.iter().skip(i + 1) {
                    // Vertical gutter: b sits to the right of a, sharing a column.
                    if b.x == a.x + a.w + Self::PANE_GAP {
                        let y0 = a.y.max(b.y);
                        let y1 = (a.y + a.h).min(b.y + b.h);
                        if y1 > y0 {
                            let dx = a.x + a.w;
                            // Hovered/dragged gutter: brighter + fatter (±tol).
                            if active_div == Some((pane::Dir::Vertical, dx)) {
                                renderer.push_divider(
                                    dx - gutter_tol,
                                    y0,
                                    Self::PANE_GAP + 2 * gutter_tol,
                                    y1 - y0,
                                    gutter_glow,
                                );
                            } else {
                                renderer.push_divider(dx, y0, Self::PANE_GAP, y1 - y0, divider);
                            }
                        }
                    }
                    // Horizontal gutter: b sits below a, sharing a row.
                    if b.y == a.y + a.h + Self::PANE_GAP {
                        let x0 = a.x.max(b.x);
                        let x1 = (a.x + a.w).min(b.x + b.w);
                        if x1 > x0 {
                            let dy = a.y + a.h;
                            if active_div == Some((pane::Dir::Horizontal, dy)) {
                                renderer.push_divider(
                                    x0,
                                    dy - gutter_tol,
                                    x1 - x0,
                                    Self::PANE_GAP + 2 * gutter_tol,
                                    gutter_glow,
                                );
                            } else {
                                renderer.push_divider(x0, a.y + a.h, x1 - x0, Self::PANE_GAP, divider);
                            }
                        }
                    }
                }
            }
        }

        // Real GUI tab bar over the pixel band [0, tab_bar_h), composited over the
        // split via the overlay pass added to record_multi_passes.
        Self::paint_tab_bar(
            renderer,
            &tab_snapshot,
            tab_focused,
            tab_hovered,
            tab_held,
            tab_dragging,
            tab_mouse,
            tab_spinner,
            tab_count,
            strip_off,
            strip_hist,
        );

        if let Err(err) = renderer.render_multi() {
            log::debug!("split frame dropped: {err:?}");
            self.force_full_redraw = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    /// Author one pane's terminal grid into the renderer's current pane (between
    /// `begin_pane`/`end_pane`) using LOCAL `(col, row)` coords. A self-contained
    /// version of the single-pane cell loop: full rebuild (no damage), cells +
    /// selection + cursor overlay. `win_focused`/`blink_on` drive the cursor
    /// style; `hovered_link` underlines the hovered OSC8 link.
    #[allow(clippy::too_many_arguments)]
    fn push_pane(
        renderer: &mut Renderer,
        pty: &Pty,
        cols: usize,
        rows: usize,
        win_focused: bool,
        blink_on: bool,
        hovered_link: Option<&str>,
    ) {
        let term = pty.term.lock();
        let content = term.renderable_content();
        let colors = content.colors;
        let display_offset = content.display_offset as i32;
        let cursor = content.cursor;
        let selection = content.selection;
        let cursor_color = color::resolve(Color::Named(NamedColor::Cursor), colors);

        let cursor_shown = cursor.shape != CursorShape::Hidden;
        let cursor_row = cursor.point.line.0 + display_offset;
        let cursor_col = cursor.point.column.0 as i32;
        // A focused window's block cursor inverts the cell beneath it; the pane is
        // always treated as "containing" the cursor (focus is window-level here).
        let invert_block = cursor_shown && win_focused && cursor.shape == CursorShape::Block;

        let cells: Vec<_> = content.display_iter.collect();
        let mut row_started = vec![false; rows];
        let mut ci = 0;
        while ci < cells.len() {
            let indexed = &cells[ci];
            let cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                ci += 1;
                continue;
            }
            let row = indexed.point.line.0 + display_offset;
            let col = indexed.point.column.0 as i32;
            if row < 0 || row >= rows as i32 || col < 0 || col >= cols as i32 {
                ci += 1;
                continue;
            }
            let row_u = row as usize;
            if !row_started[row_u] {
                renderer.begin_row(row_u);
                row_started[row_u] = true;
            }

            let mut fg = color::resolve(cell.fg, colors);
            let mut bg = color::resolve(cell.bg, colors);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::DIM) {
                fg = [fg[0] * 0.66, fg[1] * 0.66, fg[2] * 0.66, fg[3]];
            }
            if selection.is_some_and(|range| range.contains(indexed.point)) {
                bg = color::selection_bg();
            }
            if invert_block && row == cursor_row && col == cursor_col {
                std::mem::swap(&mut fg, &mut bg);
            }
            let hidden = cell.flags.contains(Flags::HIDDEN);
            let bold = cell.flags.contains(Flags::BOLD) || cell.flags.contains(Flags::BOLD_ITALIC);
            let italic =
                cell.flags.contains(Flags::ITALIC) || cell.flags.contains(Flags::BOLD_ITALIC);
            let wide = cell.flags.contains(Flags::WIDE_CHAR);

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
            if !hidden
                && matches!(decorations.underline, UnderlineStyle::None)
                && let Some(hov) = hovered_link
                && cell.hyperlink().is_some_and(|h| h.uri() == hov)
            {
                decorations.underline = UnderlineStyle::Single;
            }

            let ch = if hidden || cell.c == '\0' { ' ' } else { cell.c };
            let (combiners, consumed) = if hidden {
                (Vec::new(), unit_len(&cells, ci))
            } else {
                build_grapheme(&cells, ci, indexed.point.line.0)
            };
            let wide = wide || consumed >= 2;
            renderer.push_cell(col as usize, row_u, ch, &combiners, fg, bg, bold, italic, wide, decorations);
            ci += consumed;
        }

        // Cursor overlay (same precedence as the single-pane path).
        if cursor_shown && cursor_row >= 0 && cursor_row < rows as i32 && cursor_col >= 0 && cursor_col < cols as i32 {
            let blink_off = win_focused && cursor.shape != CursorShape::Hidden && !blink_on
                && term.cursor_style().blinking;
            if !blink_off {
                let overlay = if !win_focused {
                    Some(CursorOverlay::Hollow)
                } else {
                    match cursor.shape {
                        CursorShape::Beam => Some(CursorOverlay::Beam),
                        CursorShape::Underline => Some(CursorOverlay::Underline),
                        CursorShape::HollowBlock => Some(CursorOverlay::Hollow),
                        CursorShape::Block | CursorShape::Hidden => None,
                    }
                };
                if let Some(overlay) = overlay {
                    let r = cursor_row as usize;
                    if row_started[r] {
                        renderer.set_cur_row(r);
                    } else {
                        renderer.begin_row(r);
                    }
                    renderer.push_cursor(cursor_col as usize, r, overlay, cursor_color);
                }
            }
        }
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

    /// Pick and install the theme that matches the system color scheme when
    /// `follow_system` is on: `theme_light` in Light mode, `theme_dark` in Dark
    /// mode (defaulting to dark when the OS doesn't report a preference). A no-op
    /// when follow-system is off, so a pinned `theme` is left untouched. Returns
    /// whether the active theme actually changed (so callers can skip a redundant
    /// full redraw). The GUI tokens derive from the active theme, so the whole UI
    /// adapts automatically once the palette swaps.
    fn apply_system_theme(&mut self, scheme: Option<winit::window::Theme>) -> bool {
        if !self.config.follow_system {
            return false;
        }
        let want_light = matches!(scheme, Some(winit::window::Theme::Light));
        let name = if want_light {
            &self.config.theme_light
        } else {
            &self.config.theme_dark
        };
        if *name == self.config.theme {
            return false;
        }
        if let Some(theme) = color::theme_by_name(name) {
            color::set_theme(theme);
            self.config.theme = name.clone();
            true
        } else {
            false
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
    fn next_wake(&self, blink_active: bool, flash_active: bool, spin_active: bool) -> Option<Instant> {
        let blink = blink_active.then_some(self.blink_at);
        let flash = flash_active.then_some(self.bell_flash_until).flatten();
        let spin = spin_active.then_some(self.spinner_at);
        [blink, flash, spin].into_iter().flatten().min()
    }

    /// Whether any tab is currently busy. While true the spinner must keep
    /// animating (a finite, self-extending wakeup); when false we return to `Wait`.
    fn any_tab_busy(&self, now: Instant) -> bool {
        self.active_busy_until.is_some_and(|t| now < t)
            || self.background.iter().any(|s| s.busy_until.is_some_and(|t| now < t))
    }

    fn handle_resize(&mut self, event_loop: &ActiveEventLoop, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            return;
        }
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        renderer.resize(size.width, size.height);
        let m = renderer.cell_metrics();
        let (cols, rows) = Self::grid_for(size, m.width, m.height, renderer.pad());
        let (cw, ch) = (m.width.round() as u16, m.height.round() as u16);

        if self.panes.is_some() {
            // Active tab is split: fan each pane out to its new tile rectangle.
            // (This also re-points self.cols/self.rows at the focused pane.)
            self.resize_panes();
        } else if cols != self.cols || rows != self.rows {
            self.cols = cols;
            self.rows = rows;
            if let Some(pty) = self.pty.as_ref() {
                pty.resize(cols, rows, cw, ch);
            }
        }
        // Keep NON-split background tabs in sync so switching to one shows the
        // correct layout; split background tabs are re-laid-out on activation.
        for s in &self.background {
            if s.panes.is_none() {
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

        // Honor the system light/dark preference at startup (when follow_system is
        // on): pick theme_light/theme_dark before the renderer reads the clear
        // color, so the very first frame already matches the OS scheme.
        if self.apply_system_theme(window.theme()) {
            self.force_full_redraw = true;
        }

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
        if std::env::var_os("GLASSY_MENU").is_some() {
            self.menu_open = true;
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
        // Headless: split the active tab at startup to capture the multi-pane path.
        //   v = one vertical (left|right) split, h = one horizontal (top/bottom),
        //   grid = both (a 2x2 quad).
        if let Ok(spec) = std::env::var("GLASSY_SPLIT") {
            match spec.as_str() {
                "v" => self.split_pane(pane::Dir::Vertical, event_loop),
                "h" => self.split_pane(pane::Dir::Horizontal, event_loop),
                "grid" => {
                    self.split_pane(pane::Dir::Vertical, event_loop);
                    self.split_pane(pane::Dir::Horizontal, event_loop);
                    self.focus_pane(pane::Move::Left, event_loop);
                    self.split_pane(pane::Dir::Horizontal, event_loop);
                }
                _ => {}
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
                // Only a tab's FOCUSED pane drives the chip/window title; a
                // non-focused pane's title is not surfaced (kept simple).
                if id == self.active_id {
                    self.active_title = title;
                    self.update_window_title();
                } else if let Some(s) = self.background.iter_mut().find(|s| s.id == id) {
                    s.title = title;
                }
            }
            UserEvent::ChildExit(id) => {
                self.handle_child_exit(id, event_loop);
                return;
            }
            UserEvent::Bell(id) => {
                // Ring for any pane of the active tab (the visible one).
                if self.id_in_active_tab(id) {
                    self.trigger_bell();
                }
            }
            // A background tab produced output: its terminal state updated
            // silently; no redraw needed until it becomes active.
            UserEvent::Wakeup(id) => {
                // (Re)arm this session's busy window: a wakeup means it just emitted
                // output, so its chip spins until BUSY_LINGER elapses with no more.
                // about_to_wait advances the spinner and clears the deadline (and
                // keeps a finite wakeup scheduled) exactly like the bell flash.
                let busy = Instant::now() + BUSY_LINGER;
                // Output from a NON-focused pane of the ACTIVE tab is visible, so
                // mark the active tab busy and repaint just like the focused pane.
                if id != self.active_id && self.id_in_active_tab(id) {
                    self.active_busy_until = Some(busy);
                    self.mark_dirty(event_loop);
                    return;
                }
                if id != self.active_id {
                    // A background tab produced output (in any of its panes): flag
                    // its chip for the activity dot. Only repaint on the false->true
                    // edge so a busy background tab doesn't spam redraws.
                    let owner = self.tab_pos_of_pane(id).and_then(|p| self.tab_order.get(p).copied());
                    if let Some(owner) = owner
                        && let Some(s) = self.background.iter_mut().find(|s| s.id == owner)
                    {
                        let was_busy = s.busy_until.is_some_and(|t| Instant::now() < t);
                        s.busy_until = Some(busy);
                        if !s.activity || !was_busy {
                            s.activity = true;
                            self.mark_dirty(event_loop);
                        }
                    }
                    return;
                }
                self.active_busy_until = Some(busy);
            }
            UserEvent::PtyWrite(id, text) => {
                // Route the VT reply back to the exact pane that produced it (any
                // tab, any split pane); not a visual change, so no repaint.
                let bytes = text.into_bytes();
                if let Some(pty) = self.pty_by_id(id) {
                    pty.write(bytes);
                }
                return;
            }
            UserEvent::Cwd(id, path) => {
                // OSC 7: record the reporting pane's cwd so new tabs/splits inherit
                // it. Only a tab's FOCUSED pane drives the inherited cwd (mirrors
                // the title handling); not a visual change, so no repaint.
                if self.id_in_active_tab(id) {
                    if id == self.active_id {
                        self.active_cwd = Some(path);
                    }
                    // A non-focused active-tab pane reports its own cwd; we keep the
                    // focused pane's as the tab's inherited cwd, so ignore it.
                } else if let Some(s) = self.background.iter_mut().find(|s| s.id == id) {
                    s.last_cwd = Some(path);
                }
                return;
            }
            UserEvent::ClipboardStore(_id, _ty, text) => {
                // OSC 52 copy: write to the OS clipboard on the UI thread (arboard
                // must not run on the PTY thread). arboard exposes only the standard
                // clipboard, so a Selection store also lands there. Not visual.
                if let Some(cb) = self.clipboard()
                    && let Err(e) = cb.set_text(text)
                {
                    log::debug!("OSC 52 clipboard store failed: {e}");
                }
                return;
            }
            UserEvent::ClipboardLoad(id, _ty, formatter) => {
                // OSC 52 read: read the clipboard, format the reply, and write it
                // back to the requesting pane over the PtyWrite path. Not visual.
                let text = self.clipboard().and_then(|cb| match cb.get_text() {
                    Ok(t) => Some(t),
                    Err(e) => {
                        log::debug!("OSC 52 clipboard load failed: {e}");
                        None
                    }
                });
                if let Some(text) = text
                    && let Some(pty) = self.pty_by_id(id)
                {
                    pty.write(formatter.0(&text).into_bytes());
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
            WindowEvent::ThemeChanged(scheme) => {
                // The system light/dark color-scheme changed at runtime. When
                // `follow_system` is on, swap to `theme_light`/`theme_dark` to match
                // — glassy now ships real LIGHT themes, so Light mode actually goes
                // light. When following is off we keep the pinned `theme` but still
                // re-assert it (safe, repeatable) so winit's re-themed CSD titlebar
                // stays coherent with our palette.
                if !self.apply_system_theme(Some(scheme)) {
                    if let Some(theme) = color::theme_by_name(&self.config.theme) {
                        color::set_theme(theme);
                    }
                }
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
                            // Close the focused pane; falls back to closing the tab
                            // when the tab has only a single pane.
                            self.close_pane(event_loop);
                            return;
                        }
                        // Split the focused pane: E = vertical (left|right),
                        // O = horizontal (top/bottom). Mirrors common terminals.
                        "E" | "e" => {
                            self.split_pane(pane::Dir::Vertical, event_loop);
                            return;
                        }
                        "O" | "o" => {
                            self.split_pane(pane::Dir::Horizontal, event_loop);
                            return;
                        }
                        _ => {}
                    }
                }

                // Alt+Arrow moves focus between tiled panes (no-op when not split,
                // so a single-pane tab passes Alt+Arrow through to the child).
                if event.state.is_pressed()
                    && self.mods.alt_key()
                    && !self.mods.control_key()
                    && self.is_split()
                    && let Key::Named(named) = &event.logical_key
                {
                    let m = match named {
                        NamedKey::ArrowLeft => Some(pane::Move::Left),
                        NamedKey::ArrowRight => Some(pane::Move::Right),
                        NamedKey::ArrowUp => Some(pane::Move::Up),
                        NamedKey::ArrowDown => Some(pane::Move::Down),
                        _ => None,
                    };
                    if let Some(m) = m {
                        self.focus_pane(m, event_loop);
                        return;
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

                // While the dropdown is open, Up/Down/Enter/Esc navigate it.
                // All other keys close it and pass through to the normal handler.
                if event.state.is_pressed() && self.menu_open {
                    let key = &event.logical_key;
                    if self.handle_menu_key(key, event_loop) {
                        return;
                    }
                    // Any key that didn't navigate the menu closes it.
                    self.close_menu(event_loop);
                    // Fall through: let the keypress reach the child below.
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
                // DECCKM: arrows/Home/End go out as SS3 (ESC O X) for full-screen
                // apps (vim, less, ncurses) that enable application cursor-key mode.
                let app_cursor = self.term_mode().contains(TermMode::APP_CURSOR);
                if let Some(bytes) = encode_key(&event, self.mods, kitty, app_cursor) {
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
                // GUI demo: repaint so hover/press tracking follows the pointer.
                if self.gui_demo {
                    self.mark_dirty(event_loop);
                }
                let cell = self.px_to_cell(position.x, position.y);
                let moved = cell != self.mouse_cell;
                self.mouse_cell = cell;

                // Drag-to-reorder a tab: while a tab chip is held, move it under
                // the pointer's pixel position and lift it as a drag-ghost. Takes
                // priority over selection/hover; repaint on any motion so the ghost
                // tracks the pointer.
                if self.dragging_tab.is_some() {
                    let _ = self.drag_tab_to(position.x as f32, position.y as f32);
                    self.force_full_redraw = true;
                    self.mark_dirty(event_loop);
                    return;
                }

                // Dragging a pane resize gutter: re-tile under the pointer. Takes
                // priority over hover/selection; repaint so the divider + content
                // follow. The OS resize cursor stays set for the drag's duration.
                if self.dragging_gutter.is_some() {
                    if self.drag_gutter_to(position.x, position.y) {
                        self.mark_dirty(event_loop);
                    }
                    return;
                }

                // Gutter hover: over a split's divider band, switch the OS cursor to
                // a resize arrow and draw the divider transiently fat/bright. Only
                // costs a hit-test on motion; off any gutter restores the default.
                {
                    let new_gutter = self.gutter_at(position.x, position.y);
                    if new_gutter != self.hovered_gutter {
                        self.apply_gutter_cursor(new_gutter.as_ref());
                        self.hovered_gutter = new_gutter;
                        self.mark_dirty(event_loop);
                    }
                    // Over a gutter, suppress tab-bar/selection hover handling below.
                    if self.hovered_gutter.is_some() {
                        return;
                    }
                }

                // Tab-bar hover highlighting: track the item under the pointer (only
                // while over the bar's pixel band), repaint when it changes.
                {
                    let bar_h = self
                        .renderer
                        .as_ref()
                        .map(|r| tab_bar_h(r.cell_metrics().height) as f64)
                        .unwrap_or(0.0);
                    let new_hover = if position.y < bar_h {
                        self.strip_item_at_px(position.x as f32, position.y as f32)
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
                // Real-GUI chrome: capture the left press→release as a click edge for
                // the next chrome paint, and release the press latch on button-up.
                if button == MouseButton::Left {
                    if pressed {
                        self.gui_click_edge = false;
                    } else {
                        self.gui_click_edge = true;
                        self.gui_pressed = None;
                    }
                    self.mark_dirty(event_loop);
                }
                if !pressed {
                    self.dragging_tab = None; // end any tab drag-reorder on release
                    // End any gutter drag; re-evaluate the cursor for the spot we
                    // released over (still a gutter -> resize arrow, else default).
                    if self.dragging_gutter.take().is_some() {
                        let g = self.gutter_at(self.mouse_px.0, self.mouse_px.1);
                        self.apply_gutter_cursor(g.as_ref());
                        self.hovered_gutter = g;
                        self.mark_dirty(event_loop);
                    }
                    // Release any pressed toolbar item so its inset clears.
                    if self.held_strip_item.take().is_some() {
                        self.mark_dirty(event_loop);
                    }
                }

                // A left press over a pane resize gutter begins a drag and MUST NOT
                // start a text selection or focus-swap. Highest priority among
                // press handlers (the gutter sits in the inter-pane gap, not in any
                // pane's cell area, so this never steals a content click).
                if button == MouseButton::Left && pressed {
                    if let Some(handle) = self.gutter_at(self.mouse_px.0, self.mouse_px.1) {
                        self.apply_gutter_cursor(Some(&handle));
                        self.hovered_gutter = Some(handle.clone());
                        self.dragging_gutter = Some(handle);
                        self.held_button = None;
                        self.mark_dirty(event_loop);
                        return;
                    }
                }

                // A click anywhere while the dropdown is open: either invoke the
                // selected item (left-click inside panel) or dismiss the menu.
                // A right-click always closes the menu (second right-click = close).
                if pressed && self.menu_open
                    && (button == MouseButton::Left || button == MouseButton::Right)
                {
                    let (mx, my) = self.mouse_px;
                    if button == MouseButton::Left {
                        if let Some(action) = self.menu_hit_test(mx, my) {
                            self.invoke_menu_action(action, event_loop);
                        } else {
                            self.close_menu(event_loop);
                        }
                    } else {
                        // Right-click while menu is open: close without invoking.
                        self.close_menu(event_loop);
                    }
                    self.held_button = None;
                    return;
                }

                // A left click in the tab strip switches tabs; never sent onward.
                if button == MouseButton::Left && pressed && self.strip_click(event_loop) {
                    self.held_button = None;
                    return;
                }

                // In a split, a press over a non-focused pane focuses it first, so
                // selection / mouse-reporting below target the pane the user
                // clicked. Re-derive the (now pane-local) cell after the swap.
                if pressed && self.is_split() {
                    let (mx, my) = self.mouse_px;
                    if self.focus_pane_at(mx, my, event_loop) {
                        self.mouse_cell = self.px_to_cell(mx, my);
                    }
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
                // Right-click: open the context menu, gated on mouse-reporting mode.
                //   - not in MOUSE_MODE: plain right-press opens the menu.
                //   - in MOUSE_MODE: Shift+right-press opens it (terminal bypass);
                //     a bare right-press is forwarded to the application.
                if button == MouseButton::Right && pressed {
                    let in_mouse_mode = mode.intersects(TermMode::MOUSE_MODE);
                    if !in_mouse_mode || self.mods.shift_key() {
                        self.open_context_menu(event_loop);
                        self.held_button = None;
                        return;
                    }
                    // else: fall through to report_mouse below
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
                    let bar_h = self
                        .renderer
                        .as_ref()
                        .map(|r| tab_bar_h(r.cell_metrics().height) as f64)
                        .unwrap_or(0.0);
                    self.mouse_px.1 < bar_h
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

                // In a split, the wheel targets the pane under the pointer: focus it
                // so the scroll / mouse-report below acts on that pane's PTY.
                if self.is_split() {
                    let (mx, my) = self.mouse_px;
                    self.focus_pane_at(mx, my, event_loop);
                }

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
                            .map(|r| r.cell_metrics().height)
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
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // Moving to a different-DPI monitor changes the logical->physical
                // ratio. Reload the font at the new physical px first (otherwise
                // glyphs stay rasterized at the old DPI), then let handle_resize
                // reproject the grid against the new surface.
                let scale = scale_factor as f32;
                let font_px = self.config.font_size * scale;
                if let Some(r) = self.renderer.as_mut() {
                    r.set_font_size(font_px);
                    self.base_font_px = Some(font_px);
                }
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
                let split = self.is_split();
                self.render();
                if let (Some(renderer), Some(path)) =
                    (self.renderer.as_mut(), self.capture.as_ref())
                {
                    // A split tab builds the multi-pane instance lists; capture
                    // those, otherwise the single-grid path.
                    let res = if split {
                        renderer.capture_multi(path)
                    } else {
                        renderer.capture(path)
                    };
                    match res {
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

        // Real-GUI chrome animations: while any widget animation (hover fade,
        // toggle slide) is unsettled, advance it and keep the frame dirty so the
        // chrome repaints. This is the ONLY case where we run `ControlFlow::Poll`;
        // once everything settles we fall back to `Wait` (0% idle).
        let gui_active = if gui::any_unsettled(&self.gui_anims) {
            let dt = (now - self.gui_anim_last).as_secs_f32().min(0.1);
            gui::step_anims(&mut self.gui_anims, dt, 12.0);
            self.gui_anim_last = now;
            self.dirty = true;
            true
        } else {
            self.gui_anim_last = now;
            false
        };

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

        // Tab busy-spinner: while any tab is busy, advance one glyph at each
        // `spinner_at` deadline and repaint so the chip animates. Once a session's
        // busy window lapses, clear it (so its chip stops spinning) and repaint one
        // last frame. This is a finite, self-extending wake; when nothing is busy
        // we never schedule a spinner wakeup and idle returns to `Wait`.
        let mut busy_lapsed = false;
        if self.active_busy_until.is_some_and(|t| now >= t) {
            self.active_busy_until = None;
            busy_lapsed = true;
        }
        for s in &mut self.background {
            if s.busy_until.is_some_and(|t| now >= t) {
                s.busy_until = None;
                busy_lapsed = true;
            }
        }
        let spin_active = self.any_tab_busy(now);
        if spin_active {
            if now >= self.spinner_at {
                self.spinner_frame = self.spinner_frame.wrapping_add(1);
                self.spinner_at = now + SPINNER_INTERVAL;
                self.dirty = true;
            }
        } else {
            // Settle the phase so the next busy burst starts on the first frame.
            self.spinner_frame = 0;
        }
        if busy_lapsed {
            self.dirty = true;
        }

        if !self.dirty {
            // Idle: stay parked on `Wait` (0% CPU) unless a blink flip, a flash
            // boundary, or a spinner frame is pending — then wake at the earliest.
            // A live GUI animation overrides everything with `Poll` until it settles.
            if gui_active {
                event_loop.set_control_flow(ControlFlow::Poll);
            } else {
                match self.next_wake(blink_active, flash_active, spin_active) {
                    Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
                    None => event_loop.set_control_flow(ControlFlow::Wait),
                }
            }
            return;
        }

        if now >= self.next_frame {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            self.next_frame = now + self.refresh;
            // RedrawRequested will clear `dirty`. Keep a wakeup scheduled for the
            // next blink flip, flash boundary, or spinner frame; else wait for an
            // event. A live GUI animation keeps us on `Poll` until it settles.
            if gui_active {
                event_loop.set_control_flow(ControlFlow::Poll);
            } else {
                match self.next_wake(blink_active, flash_active, spin_active) {
                    Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
                    None => event_loop.set_control_flow(ControlFlow::Wait),
                }
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

    use super::os_title;
    const CW: f32 = 8.0; // a representative monospace cell width for layout tests
    const BH: f32 = 34.0; // tab-bar height

    #[test]
    fn strip_hit_test_matches_layout() {
        // Two tabs (tab 1 active) + their ✕ + a + button + right-hand ?/⚙/≡. The
        // hit-test resolves to the same items the painter draws (pixel rects).
        let segs = strip_layout(&[("zsh", true, false), ("vim", false, false)], 1200.0, BH, CW);
        // Probe each tab body at its center and its close box, plus the controls.
        let center = |it: StripItem| {
            segs.iter().find(|s| s.item == it).map(|s| {
                let r = s.rect;
                (r.x + r.w * 0.5, r.y + r.h * 0.5)
            })
        };
        let (tx0, ty0) = center(StripItem::Tab(0)).unwrap();
        assert_eq!(strip_item_at(&segs, tx0, ty0), Some(StripItem::Tab(0)));
        let (cx0, cy0) = center(StripItem::TabClose(0)).unwrap();
        // The close box wins over its tab body (tested in reverse order).
        assert_eq!(strip_item_at(&segs, cx0, cy0), Some(StripItem::TabClose(0)));
        let (tx1, ty1) = center(StripItem::Tab(1)).unwrap();
        assert_eq!(strip_item_at(&segs, tx1, ty1), Some(StripItem::Tab(1)));
        let (nx, ny) = center(StripItem::NewTab).unwrap();
        assert_eq!(strip_item_at(&segs, nx, ny), Some(StripItem::NewTab));
        let (hx, hy) = center(StripItem::Help).unwrap();
        assert_eq!(strip_item_at(&segs, hx, hy), Some(StripItem::Help));
        let (sx, sy) = center(StripItem::Settings).unwrap();
        assert_eq!(strip_item_at(&segs, sx, sy), Some(StripItem::Settings));
        let (mx, my) = center(StripItem::Menu).unwrap();
        assert_eq!(strip_item_at(&segs, mx, my), Some(StripItem::Menu));
        // Below the bar there are no items.
        assert_eq!(strip_item_at(&segs, tx0, BH + 5.0), None);
    }

    #[test]
    fn single_tab_has_no_close() {
        // One tab is a single wide chip — no ✕ (closing it = quit).
        let segs = strip_layout(&[("shell", true, false)], 1000.0, BH, CW);
        assert!(segs.iter().any(|s| s.item == StripItem::Tab(0)));
        assert!(!segs.iter().any(|s| matches!(s.item, StripItem::TabClose(_))));
        let title = &segs.iter().find(|s| s.item == StripItem::Tab(0)).unwrap().label;
        assert_eq!(title, "shell");
    }

    #[test]
    fn strip_layout_carries_titles_by_position() {
        // Each chip carries its raw title in stable display position; the numeric
        // prefix is added at paint time, so the label is just the title here.
        let segs = strip_layout(&[("a", false, false), ("b", true, false)], 1200.0, BH, CW);
        let lbl = |it| segs.iter().find(|s| s.item == it).map(|s| s.label.clone()).unwrap();
        assert_eq!(lbl(StripItem::Tab(0)), "a");
        assert_eq!(lbl(StripItem::Tab(1)), "b");
    }

    #[test]
    fn os_title_is_printable_ascii_only() {
        // CJK / emoji / Nerd-Font icons / dingbats are dropped (tofu-proof).
        assert_eq!(os_title("vim  src/main.rs"), "vim src/main.rs");
        assert_eq!(os_title("✻ thinking…"), "thinking");
        assert_eq!(os_title("日本語 build"), "build");
        assert_eq!(os_title("   "), "glassy");
        assert_eq!(os_title(""), "glassy");
        // No char in the output is ever non-ASCII-graphic-or-space.
        let t = os_title("a\u{f00c}b 😀 c");
        assert!(t.chars().all(|c| c.is_ascii_graphic() || c == ' '));
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
