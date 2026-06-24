//! PTY + VT integration.
//!
//! We run our own read/parse loop (rather than `alacritty_terminal`'s
//! `EventLoop`) so we can *tap* the PTY byte stream for inline-image escape
//! sequences (kitty graphics) that the VT parser would otherwise discard —
//! while still leaning on alacritty's battle-tested `Pty`, `ansi::Processor`,
//! and `Term`. A background thread owns the `Pty`, waits on its fd with
//! `polling`, reads bytes, routes image sequences to the image store, feeds the
//! rest to the parser (mutating the shared `Term` under a `FairMutex`), and
//! wakes the winit UI thread via an `EventLoopProxy`. Input/resize/shutdown
//! flow from the UI thread over a channel; the loop is woken with
//! `Poller::notify`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::num::NonZeroUsize;
use std::os::fd::{AsRawFd, BorrowedFd};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{ClipboardType, Config, Term};
use alacritty_terminal::tty::{self, EventedReadWrite, Options as PtyOptions, Shell};
use alacritty_terminal::vte::ansi::Processor;
use polling::{Event as PollEvent, Events, PollMode, Poller};
use winit::event_loop::EventLoopProxy;

use crate::image::ImageStore;

/// Events delivered from the PTY thread (and timers) into the winit loop. Each
/// carries the id of the session (tab) it came from so the UI can route it.
#[derive(Debug, Clone)]
pub enum UserEvent {
    /// New terminal content is ready to render.
    Wakeup(usize),
    /// OSC title change.
    Title(usize, String),
    /// Terminal bell.
    Bell(usize),
    /// The child process exited; the session should close.
    ChildExit(usize),
    /// The terminal produced a reply that must be written back to the child:
    /// Device Attributes, cursor/mode reports (DSR/DECRQM), and the kitty
    /// keyboard-protocol query response. Without this the child never learns the
    /// terminal's capabilities, so feature negotiation (e.g. Shift+Enter via the
    /// kitty keyboard protocol) silently fails.
    PtyWrite(usize, String),
    /// OSC 7: the shell reported its working directory for this session. Stored so
    /// new tabs/splits can inherit the cwd (parsed out of the byte stream by the
    /// `StreamTap`, since alacritty's `Event` enum has no cwd variant).
    Cwd(usize, PathBuf),
    /// OSC 52: the application asked to write `String` to the OS clipboard
    /// (`ClipboardType::Clipboard` or `Selection`). Routed to the UI thread because
    /// arboard must be used there, not on the PTY thread.
    ClipboardStore(usize, ClipboardType, String),
    /// OSC 52 read: the application asked to read the clipboard. The UI thread reads
    /// it, runs the bytes through `formatter` (which produces the reply escape
    /// sequence), and writes the result back over the `PtyWrite` path.
    ClipboardLoad(usize, ClipboardType, ClipboardFormatter),
    /// OSC 133 shell-integration semantic mark (`A`/`B`/`C`/`D`) received from
    /// the shell. The PTY loop records the cursor row in the per-session
    /// [`PromptTracker`] and forwards this event so the UI can update any
    /// jump-to-prompt state (Shift+Up/Down).
    SemanticMark(usize, char),
    /// OSC 9 or OSC 777 desktop notification from the shell. Forwarded to the UI
    /// thread so it can fire a native notification when the window is unfocused.
    Notification(usize, String),
    /// The config file was modified; reload from disk.
    ConfigReload,
}

/// Wraps the `ClipboardLoad` reply-builder closure so `UserEvent` can keep its
/// `Debug`/`Clone` derives (the boxed `Fn` is itself `Clone` via `Arc` but not
/// `Debug`).
#[derive(Clone)]
pub struct ClipboardFormatter(pub Arc<dyn Fn(&str) -> String + Sync + Send + 'static>);

impl std::fmt::Debug for ClipboardFormatter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ClipboardFormatter(..)")
    }
}

/// Bridges `alacritty_terminal`'s `EventListener` to the winit event loop, tagging
/// each forwarded event with its session id.
#[derive(Clone)]
pub struct EventProxy {
    proxy: EventLoopProxy<UserEvent>,
    id: usize,
}

impl EventProxy {
    /// Forward a pre-built `UserEvent` (already tagged with this proxy's session
    /// id) straight to the winit loop. Used for events the PTY loop derives itself
    /// rather than receiving from alacritty's `EventListener` (e.g. OSC 7 cwd,
    /// which alacritty's `Event` enum does not model).
    fn send_user(&self, event: UserEvent) {
        let _ = self.proxy.send_event(event);
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let mapped = match event {
            Event::Wakeup => Some(UserEvent::Wakeup(self.id)),
            Event::Title(title) => Some(UserEvent::Title(self.id, title)),
            Event::Bell => Some(UserEvent::Bell(self.id)),
            Event::ChildExit(_) | Event::Exit => Some(UserEvent::ChildExit(self.id)),
            // VT replies (DA / DSR / DECRQM / kitty-keyboard query) must be
            // written back to the child so capability negotiation completes.
            Event::PtyWrite(text) => Some(UserEvent::PtyWrite(self.id, text)),
            // OSC 4 / 10 / 11 / 12 color queries. The child blocks waiting for a
            // reply (hyfetch etc. stall ~1s per query otherwise), so resolve the
            // requested palette/named index against the active theme — the same
            // index→RGB mapping the renderer draws cells with — and write the
            // caller-supplied reply (`formatter`) back over the PtyWrite path.
            Event::ColorRequest(index, formatter) => {
                let rgb = crate::color::query_index(index);
                Some(UserEvent::PtyWrite(self.id, formatter(rgb)))
            }
            // OSC 52 clipboard. arboard must run on the UI thread (as app.rs does),
            // not here on the PTY thread, so forward both store and load to it.
            Event::ClipboardStore(ty, text) => {
                Some(UserEvent::ClipboardStore(self.id, ty, text))
            }
            Event::ClipboardLoad(ty, formatter) => {
                Some(UserEvent::ClipboardLoad(self.id, ty, ClipboardFormatter(formatter)))
            }
            // TextAreaSizeRequest needs the cell-pixel + grid geometry, which the
            // EventProxy doesn't carry; left unanswered (not needed for the color
            // queries this fixes).
            _ => None,
        };
        if let Some(user_event) = mapped {
            let _ = self.proxy.send_event(user_event);
        }
    }
}

/// Maximum number of prompt-line offsets retained per session. Oldest offsets
/// are evicted when the list grows beyond this, keeping memory bounded while
/// still providing a deep enough history for realistic interactive use.
const MAX_PROMPT_OFFSETS: usize = 1024;

/// Tracks the terminal row of every `OSC 133 ; A` (prompt start) mark received
/// from the shell for one PTY session. The rows are *absolute* grid rows
/// (including scrollback): `cursor.line.0 + display_offset`. Jump-to-prompt
/// (Shift+Up / Shift+Down in the UI) reads this list.
///
/// The list is sorted ascending by row (marks arrive in stream order) and is
/// capped at [`MAX_PROMPT_OFFSETS`] entries.
///
/// The navigation methods are not yet called by the UI (pending the Shift+Up/Down
/// keybind wiring in `app/input.rs`); they are `#[allow(dead_code)]` until then.
#[allow(dead_code)]
#[derive(Default)]
pub struct PromptTracker {
    /// Absolute grid rows of `A` (prompt-start) marks, sorted ascending.
    pub rows: Vec<i32>,
}

#[allow(dead_code)]
impl PromptTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a prompt-start row. Duplicate rows (same prompt, re-drawn) are
    /// silently deduped; the list is kept sorted and bounded.
    pub fn push(&mut self, row: i32) {
        if self.rows.last() == Some(&row) {
            return; // skip exact duplicate at the tail (common on prompt redraws)
        }
        if self.rows.len() >= MAX_PROMPT_OFFSETS {
            self.rows.remove(0);
        }
        self.rows.push(row);
    }

    /// Return the row of the previous prompt relative to `current_row`, or
    /// `None` if there is no earlier prompt.
    ///
    /// Wire-up: `app/input.rs` — bind Shift+Up to
    /// `pty.prompts.lock().prev_prompt(display_offset)` and scroll there.
    pub fn prev_prompt(&self, current_row: i32) -> Option<i32> {
        self.rows.iter().rev().find(|&&r| r < current_row).copied()
    }

    /// Return the row of the next prompt relative to `current_row`, or `None`
    /// if there is no later prompt.
    ///
    /// Wire-up: `app/input.rs` — bind Shift+Down to
    /// `pty.prompts.lock().next_prompt(display_offset)` and scroll there.
    pub fn next_prompt(&self, current_row: i32) -> Option<i32> {
        self.rows.iter().find(|&&r| r > current_row).copied()
    }
}

/// Trivial `Dimensions` implementation for sizing the grid.
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

/// Control messages from the UI thread to the PTY loop.
enum LoopMsg {
    Input(Cow<'static, [u8]>),
    Resize(WindowSize),
    Shutdown,
}

/// Owns the shared terminal state and the channel to the PTY loop thread.
pub struct Pty {
    /// Shared VT state. Lock briefly to read damage/renderable content.
    pub term: Arc<FairMutex<Term<EventProxy>>>,
    /// Decoded inline images received from the PTY, for the renderer to draw.
    pub images: Arc<FairMutex<ImageStore>>,
    /// OSC 133 prompt-start row offsets, for Shift+Up/Down jump-to-prompt. The
    /// PTY loop records a new row each time an `A` mark is seen; the UI reads
    /// this via `prompts.lock()` without taking any other lock.
    ///
    /// Wire-up (CAPABILITIES backlog — OSC 133 p1 item): in `app/input.rs`
    /// handle Shift+Up/Shift+Down by calling `pty.prompts.lock().prev_prompt` /
    /// `next_prompt` against the current `display_offset` and issuing a
    /// `term.scroll_display(Scroll::Delta)` to jump there.
    #[allow(dead_code)]
    pub prompts: Arc<Mutex<PromptTracker>>,
    tx: Sender<LoopMsg>,
    poller: Arc<Poller>,
}

impl Pty {
    /// Spawn the shell + the read/parse loop thread, returning a handle.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn(
        proxy: EventLoopProxy<UserEvent>,
        id: usize,
        cols: usize,
        rows: usize,
        cell_width: u16,
        cell_height: u16,
        shell: Option<Shell>,
        working_directory: Option<PathBuf>,
        scrollback: usize,
    ) -> anyhow::Result<Pty> {
        tty::setup_env();

        let mut env = HashMap::new();
        env.insert("TERM".to_string(), "xterm-256color".to_string());
        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "glassy".to_string());
        env.insert(
            "TERM_PROGRAM_VERSION".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        );

        let event_proxy = EventProxy { proxy, id };
        let grid = GridSize { cols, rows };
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &grid, event_proxy.clone())));

        // Safely convert cols/rows from usize to u16, capping at u16::MAX if needed.
        let window_size = WindowSize {
            num_cols: u16::try_from(cols).unwrap_or(u16::MAX),
            num_lines: u16::try_from(rows).unwrap_or(u16::MAX),
            cell_width,
            cell_height,
        };
        // `..default()` covers the windows-only `escape_args` field; on unix every
        // field is set explicitly, so silence the resulting needless-update lint.
        #[allow(clippy::needless_update)]
        let pty_options = PtyOptions {
            shell,
            working_directory,
            drain_on_exit: false,
            env,
            ..PtyOptions::default()
        };
        let pty = tty::new(&pty_options, window_size, id as u64)?;

        let (tx, rx) = channel::<LoopMsg>();
        let poller = Arc::new(Poller::new()?);
        let images = Arc::new(FairMutex::new(ImageStore::new()));
        let prompts = Arc::new(Mutex::new(PromptTracker::new()));

        let loop_term = term.clone();
        let loop_poller = poller.clone();
        let loop_images = images.clone();
        let loop_prompts = prompts.clone();
        std::thread::Builder::new()
            .name(format!("glassy-pty-{id}"))
            .spawn(move || {
                run_loop(pty, loop_term, event_proxy, rx, loop_poller, loop_images, loop_prompts);
            })?;

        Ok(Pty { term, images, prompts, tx, poller })
    }

    fn send(&self, msg: LoopMsg) {
        if self.tx.send(msg).is_ok() {
            let _ = self.poller.notify();
        }
    }

    /// Write raw bytes to the PTY master (keyboard input, mouse reports).
    pub fn write<B: Into<Cow<'static, [u8]>>>(&self, bytes: B) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            self.send(LoopMsg::Input(bytes));
        }
    }

    /// Paste clipboard text, wrapping it in bracketed-paste markers when the
    /// application enabled DECSET 2004 and stripping any embedded markers and
    /// broader C1/ESC control sequences.
    pub fn paste(&self, text: &str, bracketed: bool) {
        if text.is_empty() {
            return;
        }
        if bracketed {
            // Strip both bracketed-paste markers (200~ and 201~) and broader C1/ESC
            // sequences that could interfere with correct pasting.
            let step1 = text.replace("\x1b[200~", "").replace("\x1b[201~", "");
            // Filter out ESC-based control sequences (OSC, CSI, etc.) that shouldn't
            // be pasted, preserving only safe printable/whitespace characters.
            let mut sanitized = String::new();
            let mut i = 0;
            let bytes = step1.as_bytes();
            while i < bytes.len() {
                if bytes[i] == 0x1b {
                    // Skip ESC-based sequences: ESC X ... terminator
                    i += 1;
                    while i < bytes.len() && bytes[i] < 0x40 {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1; // Skip the terminator
                    }
                } else if bytes[i] < 0x20 && bytes[i] != b'\t' && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    // Skip other control characters except tab, newline, carriage return
                    i += 1;
                } else {
                    sanitized.push(bytes[i] as char);
                    i += 1;
                }
            }
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
        // Safely convert cols/rows from usize to u16, capping at u16::MAX if needed.
        let window_size = WindowSize {
            num_cols: u16::try_from(cols).unwrap_or(u16::MAX),
            num_lines: u16::try_from(rows).unwrap_or(u16::MAX),
            cell_width,
            cell_height,
        };
        self.send(LoopMsg::Resize(window_size));
        self.term.lock().resize(GridSize { cols, rows });
    }

    /// Ask the PTY loop to shut down cleanly.
    pub fn shutdown(&self) {
        self.send(LoopMsg::Shutdown);
    }
}

/// Whether a VT byte run contains a full-screen erase (`CSI 2J` or `CSI 3J`) or a
/// terminal reset (`ESC c`, RIS) — the signals that the screen content (and thus
/// any inline images anchored to it) is being wiped, e.g. by `clear`/`reset`.
fn clears_screen(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            match bytes.get(i + 1) {
                Some(b'c') => return true, // RIS
                Some(b'[') => {
                    // CSI ... J — scan the (numeric) parameter up to the final 'J'.
                    // Handle variants like CSI 2J, CSI ;2J, or CSI 0;2J.
                    let mut j = i + 2;
                    let mut params = String::new();
                    while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                        params.push(bytes[j] as char);
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b'J') {
                        // Parse parameters numerically: empty or 0 = display,
                        // 2 = all lines, 3 = scrollback+display. Check for 2 or 3.
                        let has_erase_all = params.is_empty()
                            || params.split(';').any(|p| {
                                p.parse::<u32>().map(|v| v == 2 || v == 3).unwrap_or(false)
                            });
                        if has_erase_all {
                            return true;
                        }
                    }
                    i = j;
                    continue;
                }
                _ => {}
            }
        }
        i += 1;
    }
    false
}

/// PTY read/parse loop: waits on the master fd, drains control messages, reads
/// available bytes, taps image/OSC sequences, and feeds the rest to the VT parser.
fn run_loop(
    mut pty: tty::Pty,
    term: Arc<FairMutex<Term<EventProxy>>>,
    proxy: EventProxy,
    rx: Receiver<LoopMsg>,
    poller: Arc<Poller>,
    images: Arc<FairMutex<ImageStore>>,
    prompts: Arc<Mutex<PromptTracker>>,
) {
    const PTY_KEY: usize = 1;
    let mut processor: Processor = Processor::new();
    let mut tap = crate::image::StreamTap::new();

    let fd = pty.reader().as_raw_fd();
    // alacritty opens the master non-blocking; since we only read after a poll
    // readiness event (so reads never block) and want writes to never drop input
    // on EAGAIN, switch the fd to blocking mode for reliable write_all.
    let _ = rustix::io::ioctl_fionbio(unsafe { BorrowedFd::borrow_raw(fd) }, false);
    let mode = if poller.supports_level() {
        PollMode::Level
    } else {
        PollMode::Oneshot
    };
    // SAFETY: `fd` is owned by `pty`, which this thread owns for the whole loop;
    // we delete it from the poller before `pty` is dropped at return.
    if unsafe { poller.add_with_mode(fd, PollEvent::readable(PTY_KEY), mode) }.is_err() {
        return;
    }

    let mut events = Events::with_capacity(NonZeroUsize::new(64).unwrap());
    let mut buf = vec![0u8; 65536];
    // Set true only on the paths that actually mean the child is gone (EOF / read
    // error). A transient poller error or a UI-initiated shutdown must NOT report
    // a child exit, which would wrongly close the session.
    let mut child_exited = false;

    'main: loop {
        events.clear();
        if poller.wait(&mut events, None).is_err() {
            break;
        }

        // Drain control messages (input/resize/shutdown).
        loop {
            match rx.try_recv() {
                Ok(LoopMsg::Input(b)) => {
                    let _ = pty.writer().write_all(&b);
                }
                Ok(LoopMsg::Resize(ws)) => pty.on_resize(ws),
                Ok(LoopMsg::Shutdown) => {
                    child_exited = false;
                    break 'main;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    child_exited = false;
                    break 'main;
                }
            }
        }

        // Read pending output if the fd signalled readable.
        if events.iter().any(|ev| ev.key == PTY_KEY) {
            match pty.reader().read(&mut buf) {
                Ok(0) => {
                    child_exited = true; // EOF: child gone
                    break 'main;
                }
                Ok(n) => {
                    // Tap inline-image (kitty graphics) sequences out of the
                    // stream, yielding VT byte runs interleaved with image display
                    // points. Advance the parser for each run, then anchor each
                    // image at the cursor cell it occupies at that point.
                    let events = tap.process(&buf[..n], &images);
                    let mut term = term.lock();
                    for ev in events {
                        match ev {
                            crate::image::TapEvent::Vt(bytes) => {
                                // A full screen erase (CSI 2J / 3J) or terminal
                                // reset (RIS, ESC c) wipes the content images sit
                                // on, so drop placements at that point in the
                                // stream (ordered: images later in this read
                                // survive, since the tap split them into their own
                                // Display events after this Vt run).
                                if clears_screen(&bytes) {
                                    images.lock().delete(0);
                                }
                                processor.advance(&mut *term, &bytes);
                            }
                            crate::image::TapEvent::Display(p) => {
                                let (row, col) = {
                                    let c = term.renderable_content();
                                    (
                                        c.cursor.point.line.0 + c.display_offset as i32,
                                        c.cursor.point.column.0,
                                    )
                                };
                                images.lock().place(p.id, row, col, p.cols, p.rows);
                            }
                            crate::image::TapEvent::Delete(id) => {
                                images.lock().delete(id);
                            }
                            crate::image::TapEvent::Cwd(path) => {
                                // OSC 7: surface the shell's cwd to the UI thread so
                                // new tabs/splits of this session can inherit it.
                                proxy.send_user(UserEvent::Cwd(proxy.id, path));
                            }
                            crate::image::TapEvent::SemanticMark(mark) => {
                                // OSC 133: record prompt-start rows (mark 'A') in the
                                // shared PromptTracker so the UI can jump to them via
                                // Shift+Up/Down. Other marks (B/C/D) are forwarded to
                                // the UI for potential future use (e.g. command timing).
                                if mark == 'A' {
                                    // Capture the current cursor row as an absolute
                                    // grid offset (display_offset + cursor.line). `term`
                                    // is already locked (the MutexGuard from above), so
                                    // read through the guard directly.
                                    let row = {
                                        let c = term.renderable_content();
                                        c.cursor.point.line.0 + c.display_offset as i32
                                    };
                                    if let Ok(mut p) = prompts.lock() {
                                        p.push(row);
                                    }
                                }
                                proxy.send_user(UserEvent::SemanticMark(proxy.id, mark));
                            }
                            crate::image::TapEvent::Notification(text) => {
                                // OSC 9 / OSC 777: forward to the UI thread so it can
                                // fire a native desktop notification when unfocused.
                                proxy.send_user(UserEvent::Notification(proxy.id, text));
                            }
                        }
                    }
                    drop(term);
                    proxy.send_event(Event::Wakeup);
                }
                Err(ref e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => {}
                Err(_) => {
                    child_exited = true; // e.g. EIO when the child exits
                    break 'main;
                }
            }
        }

        if mode == PollMode::Oneshot {
            let _ = poller.modify_with_mode(
                unsafe { BorrowedFd::borrow_raw(fd) },
                PollEvent::readable(PTY_KEY),
                PollMode::Oneshot,
            );
        }
    }

    let _ = poller.delete(unsafe { BorrowedFd::borrow_raw(fd) });
    if child_exited {
        proxy.send_event(Event::Exit);
    }
}
