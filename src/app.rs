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

/// Which mouse-button id to report for a pointer-motion event, or `None` to stay
/// silent. `held` is the currently pressed button (0/1/2) or `None`. Mirrors
/// xterm: any-motion mode (1003) reports even with no button (id 3); button-only
/// motion (1002) reports just while a button is held; click-only (1000) never
/// reports motion. Pure for unit testing.
fn motion_button(mode: TermMode, held: Option<u8>) -> Option<u8> {
    match held {
        Some(b)
            if mode.contains(TermMode::MOUSE_DRAG)
                || mode.contains(TermMode::MOUSE_MOTION) =>
        {
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
}

/// One terminal tab. The *active* tab's PTY lives directly in `App::pty` (so all
/// rendering/input code stays single-session); inactive tabs are parked here and
/// swapped in on switch.
struct Session {
    id: usize,
    pty: Pty,
    title: String,
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    config: Config,

    // Created lazily in `resumed()` (winit requires the window there).
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    pty: Option<Pty>,

    // Tabs. The active tab is `pty`; inactive tabs are parked in `background`.
    background: Vec<Session>,
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
            active_id: 0,
            active_title: String::new(),
            next_id: 1,
            cols: 0,
            rows: 0,
            base_font_px: None,
            mods: ModifiersState::empty(),
            focused: true,
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
        let rows = ((usable_h / cell_h).floor() as usize).max(1);
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
        if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("file://")
        {
            if let Err(e) = std::process::Command::new("xdg-open").arg(url).spawn() {
                log::warn!("failed to open {url}: {e}");
            }
        }
    }

    /// Total number of open tabs (active + background).
    fn tab_count(&self) -> usize {
        self.background.len() + self.pty.is_some() as usize
    }

    /// Reflect the active tab + tab count in the window title.
    fn update_window_title(&self) {
        let Some(window) = self.window.as_ref() else { return };
        let base = if self.active_title.is_empty() {
            "glassy"
        } else {
            self.active_title.as_str()
        };
        let total = self.tab_count();
        if total > 1 {
            window.set_title(&format!("{base}  \u{00b7}  {total} tabs"));
        } else {
            window.set_title(base);
        }
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
        if let Some(old) = self.pty.take() {
            self.background.push(Session {
                id: self.active_id,
                pty: old,
                title: std::mem::take(&mut self.active_title),
            });
        }
        self.pty = Some(pty);
        self.active_id = id;
        self.active_title.clear();
        self.update_window_title();
        self.mark_dirty(event_loop);
    }

    /// Switch tabs by `delta` in the ring (active first, then background order).
    fn cycle_tab(&mut self, delta: isize, event_loop: &ActiveEventLoop) {
        let total = self.tab_count();
        if total < 2 {
            return;
        }
        let target = (((delta % total as isize) + total as isize) % total as isize) as usize;
        if target != 0 {
            self.activate_background(target - 1, event_loop);
        }
    }

    /// Park the active tab and bring background tab `idx` forward.
    fn activate_background(&mut self, idx: usize, event_loop: &ActiveEventLoop) {
        if idx >= self.background.len() {
            return;
        }
        let Some(cur) = self.pty.take() else {
            return;
        };
        let parked = Session {
            id: self.active_id,
            pty: cur,
            title: std::mem::take(&mut self.active_title),
        };
        let target = std::mem::replace(&mut self.background[idx], parked);
        self.pty = Some(target.pty);
        self.active_id = target.id;
        self.active_title = target.title;
        self.update_window_title();
        self.mark_dirty(event_loop);
    }

    /// Close the active tab; activate another if any remain, else exit.
    fn close_active_tab(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(pty) = &self.pty {
            pty.shutdown();
        }
        match self.background.pop() {
            Some(next) => {
                self.pty = Some(next.pty);
                self.active_id = next.id;
                self.active_title = next.title;
                self.update_window_title();
                self.mark_dirty(event_loop);
            }
            None => event_loop.exit(),
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
    fn px_to_cell(&self, x: f64, y: f64) -> (usize, usize) {
        let Some(renderer) = self.renderer.as_ref() else {
            return (0, 0);
        };
        let m = renderer.cell_metrics();
        let pad = renderer.pad() as f64;
        let col = ((x - pad) / m.width as f64).floor();
        let row = ((y - pad) / m.height as f64).floor();
        let col = (col.max(0.0) as usize).min(self.cols.saturating_sub(1));
        let row = (row.max(0.0) as usize).min(self.rows.saturating_sub(1));
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
            MouseReport { button, col, row, pressed, motion },
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
        if let Some(cb) = cb {
            if let Err(e) = cb.set_text(text) {
                log::debug!("clipboard copy failed: {e}");
            }
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
            let [r, g, b, _] = color::default_fg();
            Some([r, g, b, bell::FLASH_ALPHA])
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
        let invert_block =
            cursor_shown && self.focused && cursor.shape == CursorShape::Block;

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
            renderer.resize_grid(rows);
            for d in dirty.iter_mut() {
                *d = true;
            }
        }

        // Track which rows we have begun this frame, so a row's first cell triggers
        // `begin_row` (clearing it) and the cursor overlay can re-target it later.
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
            // First cell of a dirty row: clear it and begin pushing into it.
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
                Decorations { underline, strikeout: cell.flags.contains(Flags::STRIKEOUT), color }
            };

            // Underline the hovered hyperlink's cells (only when not already
            // underlined by the app), as a click affordance.
            if !hidden && matches!(decorations.underline, UnderlineStyle::None) {
                if let Some(ref hov) = hovered_link {
                    if cell.hyperlink().is_some_and(|h| h.uri() == hov) {
                        decorations.underline = UnderlineStyle::Single;
                    }
                }
            }

            let ch = if hidden || cell.c == '\0' { ' ' } else { cell.c };
            // Reconstruct the grapheme cluster, merging this cell's combining /
            // ZWJ code points with any following cells joined by ZWJ, a skin-tone
            // modifier, a regional-indicator pair, or a variation selector — so
            // compound emoji (flags, families, professions) shape into one glyph.
            let (combiners, consumed) = if hidden {
                (Vec::new(), unit_len(&cells, ci))
            } else {
                build_grapheme(&cells, ci, indexed.point.line.0)
            };
            renderer.push_cell(
                col as usize,
                row as usize,
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
                if cr < rows && row_started[cr] {
                    renderer.set_cur_row(cr);
                    renderer.push_cursor(cc, cr, overlay, cursor_color);
                }
            }
        }

        drop(term); // release before GPU submit / present

        // Record the state this frame drew from, so the next frame can repaint only
        // what changed (the cursor's old/new row, selection, scroll position).
        self.prev_cursor = cur_cursor_cell;
        self.prev_display_offset = display_offset;
        self.prev_has_selection = has_selection;
        self.force_full_redraw = false;

        // The renderer self-heals lost/outdated surfaces internally; a transient
        // skip just waits for the next wakeup or resize to repaint.
        if let Err(err) = renderer.render() {
            log::debug!("frame skipped: {err:?}");
        }

        self.dirty = false;
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

        // Query the monitor refresh rate for the frame-coalescing throttle.
        if let Some(hz) = window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
        {
            if hz > 0 {
                self.refresh = Duration::from_secs_f64(1000.0 / hz as f64);
            }
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

        self.window = Some(window);
        self.renderer = Some(renderer);
        self.pty = Some(pty);

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
                    return;
                }
            }
        }
        self.mark_dirty(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _id: WindowId,
        event: WindowEvent,
    ) {
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
            WindowEvent::ModifiersChanged(mods) => {
                self.mods = mods.state();
            }
            WindowEvent::KeyboardInput { event, is_synthetic, .. } => {
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
                {
                    if let Key::Character(s) = &event.logical_key {
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
                }

                // Ctrl+Tab / Ctrl+Shift+Tab cycle between tabs.
                if event.state.is_pressed() && self.mods.control_key() {
                    if let Key::Named(NamedKey::Tab) = &event.logical_key {
                        let delta = if self.mods.shift_key() { -1 } else { 1 };
                        self.cycle_tab(delta, event_loop);
                        return;
                    }
                }

                // Ctrl +/-/0 adjusts the font size at runtime (and Ctrl 0 resets
                // to the configured size). Intercepted before `encode_key` so the
                // control bytes for these keys never reach the child. Matches the
                // de-facto terminal/browser zoom convention. Shift is allowed (so
                // Ctrl+Shift+'=' i.e. Ctrl+'+' works) but not required.
                if event.state.is_pressed()
                    && self.mods.control_key()
                    && !self.mods.alt_key()
                {
                    if let Key::Character(s) = &event.logical_key {
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
                }

                // Shift + PageUp/PageDown/Home/End drives glassy's own scrollback
                // (the primary screen only) and is consumed before the child sees
                // it. This mirrors the de-facto terminal convention.
                if event.state.is_pressed()
                    && self.mods.shift_key()
                    && !self.term_mode().contains(TermMode::ALT_SCREEN)
                {
                    if let Key::Named(named) = &event.logical_key {
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
                }

                if let Some(bytes) = encode_key(&event, self.mods) {
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
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                if let Some(pty) = &self.pty {
                    pty.write(text.into_bytes());
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.mouse_px = (position.x, position.y);
                let cell = self.px_to_cell(position.x, position.y);
                let moved = cell != self.mouse_cell;
                self.mouse_cell = cell;

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
                                if cell == self.mouse_cell && now.duration_since(t) < MULTI_CLICK =>
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
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => {
                        if y == 0.0 { 0 } else { (y.abs().ceil() as i32) * y.signum() as i32 }
                    }
                    MouseScrollDelta::PixelDelta(p) => (p.y / 20.0) as i32,
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
    use super::{WheelAction, motion_button, wheel_action};
    use alacritty_terminal::term::TermMode;

    #[test]
    fn wheel_normal_screen_scrolls_scrollback() {
        assert_eq!(wheel_action(TermMode::empty()), WheelAction::Scrollback);
    }

    #[test]
    fn wheel_alt_screen_emits_arrows() {
        // bat/less/vim without mouse: alt screen, no mouse reporting.
        assert_eq!(wheel_action(TermMode::ALT_SCREEN), WheelAction::Arrows);
    }

    #[test]
    fn wheel_mouse_mode_reports_to_app() {
        // vim with `mouse=a`, htop, claude: app owns the wheel.
        assert_eq!(wheel_action(TermMode::MOUSE_REPORT_CLICK), WheelAction::Report);
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
