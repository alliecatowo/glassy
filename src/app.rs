//! The winit UI/render driver.
//!
//! Idle behaviour is `ControlFlow::Wait`: 0% CPU, no GPU submits until the PTY
//! thread (or a resize/input) wakes us. Wakeups set a dirty flag and are
//! coalesced to at most one frame per monitor refresh, so a fast producer like
//! Claude Code streaming tokens collapses into a single redraw per refresh
//! instead of one redraw per token burst.

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::tty::Shell;
use alacritty_terminal::vte::ansi::CursorShape;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::color;
use crate::input::{MouseReport, encode_key, encode_mouse};
use crate::pty::{Pty, UserEvent};
use crate::renderer::{Decorations, Renderer, UnderlineStyle};

/// Lines of scrollback to move per wheel notch when reporting to a TUI or
/// scrolling glassy's own scrollback buffer.
const WHEEL_LINES: i32 = 3;

/// Static configuration resolved at startup.
pub struct Config {
    /// Logical font size in points (scaled by the monitor's DPI factor).
    pub font_size: f32,
    /// Shell program + args; `None` uses the user's default shell.
    pub shell: Option<Shell>,
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    config: Config,

    // Created lazily in `resumed()` (winit requires the window there).
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    pty: Option<Pty>,

    cols: usize,
    rows: usize,

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

    /// Lazily-created OS clipboard handle (arboard). `None` until first use, and
    /// stays `None` if the platform clipboard is unavailable.
    clipboard: Option<arboard::Clipboard>,

    // Render-on-demand throttle state.
    dirty: bool,
    next_frame: Instant,
    refresh: Duration,

    // Headless capture: when `GLASSY_CAPTURE` is set, render after a short delay
    // (so the shell has produced output), write a PPM, and exit.
    capture: Option<std::path::PathBuf>,
    capture_deadline: Option<Instant>,
}

impl App {
    pub fn new(proxy: EventLoopProxy<UserEvent>, config: Config) -> Self {
        Self {
            proxy,
            config,
            window: None,
            renderer: None,
            pty: None,
            cols: 0,
            rows: 0,
            mods: ModifiersState::empty(),
            focused: true,
            mouse_cell: (0, 0),
            mouse_px: (0.0, 0.0),
            held_button: None,
            selecting: false,
            last_click: None,
            clipboard: None,
            dirty: false,
            next_frame: Instant::now(),
            refresh: Duration::from_micros(16_666), // 60 Hz default until queried
            capture: std::env::var_os("GLASSY_CAPTURE").map(std::path::PathBuf::from),
            capture_deadline: None,
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
        let (Some(renderer), Some(pty)) = (self.renderer.as_mut(), self.pty.as_ref()) else {
            return;
        };

        // Hold the terminal lock only long enough to copy out renderable state.
        let term = pty.term.lock();
        let content = term.renderable_content();
        let colors = content.colors;
        let display_offset = content.display_offset as i32;
        let cursor = content.cursor;
        let selection = content.selection;

        let cursor_visible = self.focused && cursor.shape != CursorShape::Hidden;
        let cursor_row = cursor.point.line.0 + display_offset;
        let cursor_col = cursor.point.column.0 as i32;

        renderer.begin_frame(color::DEFAULT_BG);

        for indexed in content.display_iter {
            let cell = indexed.cell;

            // The right half of a wide character is a spacer; skip it.
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }

            let row = indexed.point.line.0 + display_offset;
            let col = indexed.point.column.0 as i32;
            if row < 0 || row >= self.rows as i32 || col < 0 || col >= self.cols as i32 {
                continue;
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
                bg = color::SELECTION_BG;
            }
            // Block cursor: invert the cell beneath it.
            if cursor_visible && row == cursor_row && col == cursor_col {
                std::mem::swap(&mut fg, &mut bg);
            }
            let hidden = cell.flags.contains(Flags::HIDDEN);

            let bold = cell.flags.contains(Flags::BOLD) || cell.flags.contains(Flags::BOLD_ITALIC);
            let italic =
                cell.flags.contains(Flags::ITALIC) || cell.flags.contains(Flags::BOLD_ITALIC);

            // Text decorations. Hidden cells draw nothing, so suppress strokes
            // too. Underline styles are mutually exclusive (latest SGR wins);
            // map the cell flags to a single style. The decoration color is the
            // SGR 58 underline color when set, else the cell foreground, so e.g.
            // a red LSP curl sits under default-fg text.
            let decorations = if hidden {
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

            let ch = if hidden || cell.c == '\0' { ' ' } else { cell.c };
            // Combining marks / ZWJ-joined codepoints (compound emoji, accents)
            // attached to this cell; shaped together so they form one glyph.
            let combiners: &[char] = if hidden { &[] } else { cell.zerowidth().unwrap_or(&[]) };
            renderer.push_cell(
                col as usize,
                row as usize,
                ch,
                combiners,
                fg,
                bg,
                bold,
                italic,
                decorations,
            );
        }

        drop(term); // release before GPU submit / present

        // The renderer self-heals lost/outdated surfaces internally; a transient
        // skip just waits for the next wakeup or resize to repaint.
        if let Err(err) = renderer.render() {
            log::debug!("frame skipped: {err:?}");
        }

        self.dirty = false;
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
            pty.resize(cols, rows, m.width.round() as u16, m.height.round() as u16);
        }
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

        let mut renderer = match Renderer::new(window.clone(), font_px) {
            Ok(r) => r,
            Err(e) => {
                log::error!("failed to initialize renderer: {e:#}");
                event_loop.exit();
                return;
            }
        };

        let size = window.inner_size();
        renderer.resize(size.width, size.height);
        let m = renderer.cell_metrics();
        let (cols, rows) = Self::grid_for(size, m.width, m.height, renderer.pad());
        self.cols = cols;
        self.rows = rows;

        let pty = match Pty::spawn(
            self.proxy.clone(),
            cols,
            rows,
            m.width.round() as u16,
            m.height.round() as u16,
            self.config.shell.clone(),
            None,
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
            UserEvent::Title(title) => {
                if let Some(w) = &self.window {
                    w.set_title(&title);
                }
            }
            UserEvent::ChildExit => {
                event_loop.exit();
                return;
            }
            UserEvent::Bell => { /* no audible/visual bell in v1 */ }
            UserEvent::Wakeup => {}
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
                            _ => {}
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
                    // Drag/motion reports: only while a button is held and the
                    // terminal asked for motion (DRAG) or any-motion (MOTION).
                    let mode = self.term_mode();
                    if let Some(button) = self.held_button {
                        let wants = (mode.contains(TermMode::MOUSE_DRAG)
                            || mode.contains(TermMode::MOUSE_MOTION))
                            && mode.intersects(TermMode::MOUSE_MODE);
                        if wants {
                            self.report_mouse(button, true, true, mode);
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

                if mode.intersects(TermMode::MOUSE_MODE) {
                    // Wheel as button 64 (up) / 65 (down), one report per line.
                    let button = if up { 64 } else { 65 };
                    for _ in 0..count {
                        self.report_mouse(button, true, false, mode);
                    }
                } else if mode.contains(TermMode::ALT_SCREEN)
                    && mode.contains(TermMode::ALTERNATE_SCROLL)
                {
                    // Alt-screen apps without mouse reporting (e.g. less) expect
                    // the wheel to emit arrow keys.
                    if let Some(pty) = &self.pty {
                        let seq: &[u8] = if up { b"\x1b[A" } else { b"\x1b[B" };
                        let mut out = Vec::with_capacity(seq.len() * count);
                        for _ in 0..count {
                            out.extend_from_slice(seq);
                        }
                        pty.write(out);
                    }
                } else {
                    // Default: scroll glassy's own scrollback buffer.
                    let delta = if up { WHEEL_LINES } else { -WHEEL_LINES } * count as i32;
                    if let Some(pty) = &self.pty {
                        pty.term.lock().scroll_display(Scroll::Delta(delta));
                    }
                    self.mark_dirty(event_loop);
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

        if !self.dirty {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        let now = Instant::now();
        if now >= self.next_frame {
            if let Some(w) = &self.window {
                w.request_redraw();
            }
            self.next_frame = now + self.refresh;
            // RedrawRequested will clear `dirty`; wait until then.
            event_loop.set_control_flow(ControlFlow::Wait);
        } else {
            event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
        }
    }
}
