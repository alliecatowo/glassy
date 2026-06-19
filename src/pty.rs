//! PTY + VT integration via `alacritty_terminal`.
//!
//! A background OS thread (owned by `alacritty_terminal::event_loop::EventLoop`)
//! reads the PTY, runs the VT parser, and mutates the shared `Term` behind a
//! `FairMutex`. When new content is ready it emits `Event::Wakeup`, which we
//! forward to the winit UI thread via an `EventLoopProxy`. The UI thread never
//! touches the PTY fd directly; it only reads `Term` state on a wakeup and
//! writes input bytes through the `Notifier`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener, Notify, WindowSize};
use alacritty_terminal::event_loop::{EventLoop as PtyEventLoop, Msg, Notifier};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use winit::event_loop::EventLoopProxy;

/// Events delivered from the PTY thread (and timers) into the winit loop.
#[derive(Debug, Clone)]
pub enum UserEvent {
    /// New terminal content is ready to render.
    Wakeup,
    /// OSC title change.
    Title(String),
    /// Terminal bell.
    Bell,
    /// The child process exited; we should close.
    ChildExit,
}

/// Bridges `alacritty_terminal`'s `EventListener` to the winit event loop.
///
/// `EventLoopProxy<T>` is `Clone + Send` when `T: Send`, so this is safe to move
/// into the PTY reader thread.
#[derive(Clone)]
pub struct EventProxy(pub EventLoopProxy<UserEvent>);

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let mapped = match event {
            Event::Wakeup => Some(UserEvent::Wakeup),
            Event::Title(title) => Some(UserEvent::Title(title)),
            Event::Bell => Some(UserEvent::Bell),
            Event::ChildExit(_) | Event::Exit => Some(UserEvent::ChildExit),
            // Clipboard / color / cursor-blink / mouse-dirty events are not
            // needed for the minimal feature set.
            _ => None,
        };
        if let Some(user_event) = mapped {
            // The loop may already be gone during shutdown; ignore the error.
            let _ = self.0.send_event(user_event);
        }
    }
}

/// Trivial `Dimensions` implementation for sizing the grid. `alacritty_terminal`
/// only ships a public `TermSize` inside its `test` module, so we provide our own.
#[derive(Copy, Clone, Debug)]
pub struct GridSize {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// Owns the shared terminal state and the input channel to the PTY thread.
pub struct Pty {
    /// Shared VT state. Lock briefly to read damage/renderable content.
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    /// Input path: keystrokes are written here immediately (single syscall),
    /// pixels come back asynchronously as PTY echo -> `Event::Wakeup`.
    notifier: Notifier,
}

impl Pty {
    /// Spawn the shell, wire up the VT parser thread, and return a handle.
    ///
    /// `cell_width`/`cell_height` are in physical pixels (used by programs that
    /// query pixel dimensions, e.g. for sixel/image protocols).
    pub fn spawn(
        proxy: EventLoopProxy<UserEvent>,
        cols: usize,
        rows: usize,
        cell_width: u16,
        cell_height: u16,
        shell: Option<Shell>,
        working_directory: Option<PathBuf>,
        scrollback: usize,
    ) -> anyhow::Result<Pty> {
        // Sets COLORTERM=truecolor and a base TERM on glassy's own environment,
        // which the child inherits.
        tty::setup_env();

        // Per-child environment. glassy ships no terminfo of its own, so we
        // advertise the universally-available `xterm-256color` (a strict subset
        // of what our VT parser handles) plus 24-bit color and a program identity.
        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "glassy".to_string());
        env.insert(
            "TERM_PROGRAM_VERSION".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        );

        let event_proxy = EventProxy(proxy);
        let grid = GridSize { cols, rows };

        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            config,
            &grid,
            event_proxy.clone(),
        )));

        let window_size = WindowSize {
            num_cols: cols as u16,
            num_lines: rows as u16,
            cell_width,
            cell_height,
        };

        let pty_options = PtyOptions {
            shell,
            working_directory,
            drain_on_exit: false,
            env,
            ..PtyOptions::default()
        };

        // `window_id` is only used to set $WINDOWID; 0 is fine for a single window.
        let pty = tty::new(&pty_options, window_size, 0)?;

        // drain_on_exit = false, ref_test = false.
        let pty_loop = PtyEventLoop::new(term.clone(), event_proxy, pty, false, false)?;
        let notifier = Notifier(pty_loop.channel());
        // Spawn the reader/parser thread. The handle is detached; the thread
        // shuts down when it receives `Msg::Shutdown` or the child exits.
        let _handle = pty_loop.spawn();

        Ok(Pty { term, notifier })
    }

    /// Write raw bytes to the PTY master (keyboard input, paste, mouse reports).
    pub fn write<B: Into<Cow<'static, [u8]>>>(&self, bytes: B) {
        self.notifier.notify(bytes);
    }

    /// Paste clipboard text into the child.
    ///
    /// When the application has enabled bracketed paste (DECSET 2004), the text
    /// is wrapped in `ESC[200~` .. `ESC[201~` so the program can distinguish
    /// pasted content from typed input. Any embedded `ESC[201~` is stripped so a
    /// hostile clipboard cannot break out of the paste and inject commands.
    pub fn paste(&self, text: &str, bracketed: bool) {
        if text.is_empty() {
            return;
        }
        if bracketed {
            let sanitized = text.replace("\x1b[201~", "");
            let mut out = Vec::with_capacity(sanitized.len() + 12);
            out.extend_from_slice(b"\x1b[200~");
            out.extend_from_slice(sanitized.as_bytes());
            out.extend_from_slice(b"\x1b[201~");
            self.write(out);
        } else {
            self.write(text.as_bytes().to_vec());
        }
    }

    /// Inform the PTY + terminal of a new grid size.
    pub fn resize(&self, cols: usize, rows: usize, cell_width: u16, cell_height: u16) {
        let window_size = WindowSize {
            num_cols: cols as u16,
            num_lines: rows as u16,
            cell_width,
            cell_height,
        };
        let _ = self.notifier.0.send(Msg::Resize(window_size));
        self.term.lock().resize(GridSize { cols, rows });
    }

    /// Ask the PTY thread to shut down cleanly.
    pub fn shutdown(&self) {
        let _ = self.notifier.0.send(Msg::Shutdown);
    }
}
