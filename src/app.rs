//! The winit UI/render driver.
//!
//! Idle behaviour is `ControlFlow::Wait`: 0% CPU, no GPU submits until the PTY
//! thread (or a resize/input) wakes us. Wakeups set a dirty flag and are
//! coalesced to at most one frame per monitor refresh, so a fast producer like
//! Claude Code streaming tokens collapses into a single redraw per refresh
//! instead of one redraw per token burst.

use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::tty::Shell;
use alacritty_terminal::vte::ansi::CursorShape;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::window::{Window, WindowId};

use crate::color;
use crate::input::encode_key;
use crate::pty::{Pty, UserEvent};
use crate::renderer::Renderer;

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
            // Block cursor: invert the cell beneath it.
            if cursor_visible && row == cursor_row && col == cursor_col {
                std::mem::swap(&mut fg, &mut bg);
            }
            let hidden = cell.flags.contains(Flags::HIDDEN);

            let bold = cell.flags.contains(Flags::BOLD) || cell.flags.contains(Flags::BOLD_ITALIC);
            let italic =
                cell.flags.contains(Flags::ITALIC) || cell.flags.contains(Flags::BOLD_ITALIC);

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
                if !is_synthetic {
                    if let Some(bytes) = encode_key(&event, self.mods) {
                        if let Some(pty) = &self.pty {
                            pty.write(bytes);
                        }
                    }
                }
            }
            WindowEvent::Ime(winit::event::Ime::Commit(text)) => {
                if let Some(pty) = &self.pty {
                    pty.write(text.into_bytes());
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                // Translate vertical scroll into arrow-key reports as a simple
                // default (works in pagers / Claude Code's scroll regions).
                let lines = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y as i32,
                    MouseScrollDelta::PixelDelta(p) => (p.y / 20.0) as i32,
                };
                if lines != 0 {
                    if let Some(pty) = &self.pty {
                        let seq: &[u8] = if lines > 0 { b"\x1b[A" } else { b"\x1b[B" };
                        let mut out = Vec::new();
                        for _ in 0..lines.abs().min(5) {
                            out.extend_from_slice(seq);
                        }
                        pty.write(out);
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
