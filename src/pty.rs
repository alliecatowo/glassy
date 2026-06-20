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
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{Config, Term};
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
}

/// Bridges `alacritty_terminal`'s `EventListener` to the winit event loop, tagging
/// each forwarded event with its session id.
#[derive(Clone)]
pub struct EventProxy {
    proxy: EventLoopProxy<UserEvent>,
    id: usize,
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        let mapped = match event {
            Event::Wakeup => Some(UserEvent::Wakeup(self.id)),
            Event::Title(title) => Some(UserEvent::Title(self.id, title)),
            Event::Bell => Some(UserEvent::Bell(self.id)),
            Event::ChildExit(_) | Event::Exit => Some(UserEvent::ChildExit(self.id)),
            _ => None,
        };
        if let Some(user_event) = mapped {
            let _ = self.proxy.send_event(user_event);
        }
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

        let window_size = WindowSize {
            num_cols: cols as u16,
            num_lines: rows as u16,
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

        let loop_term = term.clone();
        let loop_poller = poller.clone();
        let loop_images = images.clone();
        std::thread::Builder::new()
            .name(format!("glassy-pty-{id}"))
            .spawn(move || {
                run_loop(pty, loop_term, event_proxy, rx, loop_poller, loop_images);
            })?;

        Ok(Pty { term, images, tx, poller })
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
    /// application enabled DECSET 2004 and stripping any embedded end marker.
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
                    let mut j = i + 2;
                    while j < bytes.len() && (bytes[j].is_ascii_digit() || bytes[j] == b';') {
                        j += 1;
                    }
                    if bytes.get(j) == Some(&b'J') {
                        let param = &bytes[i + 2..j];
                        if param == b"2" || param == b"3" {
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
/// available bytes, taps image sequences, and feeds the rest to the VT parser.
fn run_loop(
    mut pty: tty::Pty,
    term: Arc<FairMutex<Term<EventProxy>>>,
    proxy: EventProxy,
    rx: Receiver<LoopMsg>,
    poller: Arc<Poller>,
    images: Arc<FairMutex<ImageStore>>,
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
