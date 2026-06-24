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
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, OnResize, WindowSize};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::{ClipboardType, Config, Term};
use alacritty_terminal::tty::{self, EventedReadWrite, Options as PtyOptions, Shell};
use alacritty_terminal::vte::ansi::Processor;
use polling::{Event as PollEvent, Events, PollMode, Poller};
use winit::event_loop::EventLoopProxy;

use crate::image::ImageStore;
use crate::input::ModifyOtherKeys;

// ---- Terminfo availability check ------------------------------------------------

/// Check if a terminfo entry is available in the system terminfo database.
/// Returns true if the entry can be found, false otherwise.
/// This is a conservative check that doesn't fail; we just return false if the entry is missing.
fn terminfo_available(name: &str) -> bool {
    // Try standard terminfo paths in order:
    // 1. $TERMINFO (user override)
    // 2. $HOME/.terminfo (user's personal database)
    // 3. /usr/share/terminfo (system database)
    // 4. /lib/terminfo (fallback system location)
    // 5. /etc/terminfo (another common location)

    if let Ok(terminfo_path) = std::env::var("TERMINFO") {
        let path = PathBuf::from(terminfo_path).join(&name[0..1]).join(name);
        if path.exists() {
            return true;
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let path = PathBuf::from(home)
            .join(".terminfo")
            .join(&name[0..1])
            .join(name);
        if path.exists() {
            return true;
        }
    }

    // Check system locations
    for base in &["/usr/share/terminfo", "/lib/terminfo", "/etc/terminfo"] {
        let path = PathBuf::from(base).join(&name[0..1]).join(name);
        if path.exists() {
            return true;
        }
    }

    false
}

// ---- /proc-based pane-info helpers ------------------------------------------

/// Cheap cached snapshot of one pane's runtime identity: cwd and the name of
/// the foreground process group leader (the program currently running, e.g.
/// `vim`, `cargo`, or `bash` when idle). Reading `/proc` is cheap but not free;
/// we cache and re-read only on explicit invalidation (pane focus) or the
/// periodic 2-second background poll so idle terminals stay at 0% CPU.
#[derive(Clone, Default, Debug)]
pub struct PaneInfo {
    /// Current working directory, read from `/proc/<shell_pid>/cwd` (a symlink).
    pub cwd: Option<PathBuf>,
    /// `comm` of the tty's foreground process group leader (the name of the running
    /// command, max 15 chars per Linux). `None` when the shell is the foreground
    /// leader (idle) or the read fails.
    pub foreground_comm: Option<String>,
    /// Git branch for `cwd` (or `:<sha7>` when detached), derived by walking up to
    /// the nearest `.git/HEAD`. Cached here so the status bar reads it without a
    /// filesystem walk on every rendered frame; refreshed on the 2 s proc poll.
    pub git_branch: Option<String>,
}

impl PaneInfo {
    /// Refresh from `/proc` for the shell with `shell_pid`. Reads the pty's tty
    /// foreground pgid from `/proc/<shell_pid>/stat` field 8, then reads that
    /// process's `comm` and the shell's `cwd` symlink. All reads are best-effort
    /// (`None` on any failure).
    pub fn read(shell_pid: u32) -> Self {
        let cwd = read_proc_cwd(shell_pid);
        let foreground_comm = read_foreground_comm(shell_pid);
        // Resolve the git branch once per refresh (not per frame); the walk + HEAD
        // read is cheap at the 2 s cadence but a measurable idle-CPU cost at 60 Hz.
        let git_branch = cwd
            .as_deref()
            .and_then(crate::app::read_git_branch);
        Self { cwd, foreground_comm, git_branch }
    }
}

/// Read `/proc/<pid>/cwd` (a symlink to the process's cwd).
fn read_proc_cwd(pid: u32) -> Option<PathBuf> {
    let path = format!("/proc/{pid}/cwd");
    std::fs::read_link(&path).ok()
}

/// Read the name of the tty's foreground process group leader for `shell_pid`.
///
/// 1. Parse `/proc/<shell_pid>/stat` field index 7 (0-based) to get the tty's
///    foreground pgid (`tpgid`). This is the pgid of the process group currently
///    owning the terminal's foreground slot.
/// 2. Find a process in that pgid via `/proc/<tpgid>/comm` (most foreground
///    commands will have that as their pid). Falls back to scanning `/proc` for
///    a process whose `stat` `pgrp` field matches.
/// 3. Return the `comm` string, or `None` if the shell itself is foreground
///    (tpgid == shell's pgid) or any read fails.
fn read_foreground_comm(shell_pid: u32) -> Option<String> {
    // Parse /proc/<shell_pid>/stat to get tpgid (field index 7, 0-based after
    // stripping the comm field in parens). The stat line format is:
    //   pid (comm) state ppid pgrp session tty_nr tpgid ...
    // We parse by finding the closing ')' and counting whitespace-separated
    // tokens after it.
    let stat = std::fs::read_to_string(format!("/proc/{shell_pid}/stat")).ok()?;
    let after_comm = stat.rfind(')')?;
    let rest = &stat[after_comm + 1..];
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // After ')': state ppid pgrp session tty_nr tpgid ...
    // tpgid is at index 5 (0-based).
    let tpgid: i32 = fields.get(5)?.parse().ok()?;
    if tpgid <= 0 {
        return None;
    }
    let tpgid = tpgid as u32;
    // Get the shell's own pgid to detect "shell is foreground" (idle).
    let shell_pgid: u32 = fields.get(2)?.parse().ok()?;
    if tpgid == shell_pgid {
        return None; // shell is foreground (idle prompt)
    }
    // Try /proc/<tpgid>/comm first (works when the pgid leader has that pid).
    if let Ok(comm) = std::fs::read_to_string(format!("/proc/{tpgid}/comm")) {
        let name = comm.trim().to_string();
        if !name.is_empty() {
            return Some(name);
        }
    }
    // Fallback: scan /proc for a process whose pgid matches tpgid.
    if let Ok(rd) = std::fs::read_dir("/proc") {
        for entry in rd.flatten() {
            let fname = entry.file_name();
            let fname = fname.to_string_lossy();
            if !fname.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let pid: u32 = fname.parse().unwrap_or(0);
            if pid == 0 {
                continue;
            }
            if let Ok(s) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                // `?`/early-return here would abort the WHOLE scan on the first
                // unparseable entry; skip to the next pid instead.
                let Some(after) = s.rfind(')') else { continue };
                let tokens: Vec<&str> = s[after + 1..].split_whitespace().collect();
                let Some(Ok(pg)) = tokens.get(2).map(|t| t.parse::<u32>()) else { continue };
                if pg == tpgid
                    && let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm"))
                {
                    let name = comm.trim().to_string();
                    if !name.is_empty() {
                        return Some(name);
                    }
                }
            }
        }
    }
    None
}

// ---- /proc-based pane-info helpers (end) ------------------------------------

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
    /// OSC 9;4 progress report from the running application. The UI thread stores
    /// the latest state per-session and renders a subtle progress indicator.
    Progress(usize, crate::image::ProgressState),
    /// The config file was modified; reload from disk.
    ConfigReload,
    /// The running application changed the xterm modifyOtherKeys level via
    /// `CSI > 4 ; N m`. The UI thread updates `App::modify_other_keys` so
    /// subsequent key events are encoded with the correct form.
    ModifyOtherKeys(usize, ModifyOtherKeys),
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
    /// Absolute grid rows of `A` (prompt-start) marks, kept sorted ascending. A
    /// `VecDeque` so the bounded-capacity eviction pops the front in O(1) rather
    /// than memmoving the whole backing buffer.
    pub rows: std::collections::VecDeque<i32>,
}

#[allow(dead_code)]
impl PromptTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a prompt-start row. Duplicate rows (same prompt, re-drawn) are
    /// silently deduped; the list is kept sorted and bounded.
    pub fn push(&mut self, row: i32) {
        // Maintain the sorted-ascending invariant that prev_prompt/next_prompt
        // rely on. Marks usually arrive in ascending order (append to the tail),
        // but a backward scroll can produce a lower absolute row; binary_search
        // places it correctly and dedups exact duplicates.
        if let Err(pos) = self.rows.binary_search(&row) {
            if self.rows.len() >= MAX_PROMPT_OFFSETS {
                self.rows.pop_front();
                // The eviction shifted indices by one; re-find the slot.
                let pos = self.rows.binary_search(&row).unwrap_or_else(|p| p);
                self.rows.insert(pos, row);
            } else {
                self.rows.insert(pos, row);
            }
        }
        // Ok(_) => row already present; skip the duplicate.
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
    /// PID of the spawned shell process. Used to read `/proc/<shell_pid>/cwd`
    /// and the tty foreground pgid for the pane header and status bar.
    pub shell_pid: u32,
    /// Cached `/proc`-based pane info (cwd + foreground comm). Refreshed on
    /// pane focus and periodically (see `App::refresh_proc_info`). Read under
    /// the UI thread only; no locking needed.
    pub pane_info: PaneInfo,
    /// When the cached `pane_info` was last refreshed. Used by the periodic
    /// background poller to avoid re-reading on every frame.
    pub pane_info_at: Instant,
    tx: Sender<LoopMsg>,
    poller: Arc<Poller>,
}

impl Pty {
    /// Spawn the shell + the read/parse loop thread, returning a handle.
    ///
    /// `word_separator` is merged with alacritty's default `SEMANTIC_ESCAPE_CHARS`
    /// so the configured extra characters act as word boundaries for double-click
    /// semantic selection.
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
        word_separator: &str,
    ) -> anyhow::Result<Pty> {
        tty::setup_env();

        let mut env = HashMap::new();

        // Attempt to use glassy-256color terminfo if available, else fall back to xterm-256color.
        // The terminfo is installed by the package; if not available, the app still works correctly.
        let term = if terminfo_available("glassy-256color") {
            "glassy-256color".to_string()
        } else {
            "xterm-256color".to_string()
        };
        env.insert("TERM".to_string(), term);

        env.insert("COLORTERM".to_string(), "truecolor".to_string());
        env.insert("TERM_PROGRAM".to_string(), "glassy".to_string());
        env.insert(
            "TERM_PROGRAM_VERSION".to_string(),
            env!("CARGO_PKG_VERSION").to_string(),
        );

        // Set WINDOWID and GLASSY_WINDOW_ID for compatibility with shell integration and X11 tools.
        // WINDOWID is commonly used by X11 applications; GLASSY_WINDOW_ID is glassy-specific.
        // The actual window ID would be set by the app after window creation (not available at spawn time).
        // For now, these are placeholders that can be overridden by the application or environment.
        env.insert("GLASSY_WINDOW_ID".to_string(), "".to_string());
        // WINDOWID traditionally holds an X11 window ID (as hex or decimal). We leave it empty/unset
        // unless provided by the environment, since the window may not exist at spawn time.
        // Applications can check for GLASSY_WINDOW_ID to detect running under glassy.

        let event_proxy = EventProxy { proxy, id };
        let grid = GridSize { cols, rows };
        // Merge user-configured word separators with the default set, deduped.
        let semantic_escape_chars = merge_word_separators(
            alacritty_terminal::term::SEMANTIC_ESCAPE_CHARS,
            word_separator,
        );
        let config = Config {
            scrolling_history: scrollback,
            semantic_escape_chars,
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

        // Capture the shell PID before moving `pty` into the PTY loop thread.
        let shell_pid = pty.child().id();

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

        // Read the initial cwd eagerly so the pane header shows the right path on
        // the first frame (before the shell emits its first OSC 7).
        let pane_info = PaneInfo::read(shell_pid);
        Ok(Pty { term, images, prompts, shell_pid, pane_info, pane_info_at: Instant::now(), tx, poller })
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
            // Operate on chars (not raw bytes): casting a UTF-8 continuation byte
            // via `as char` would mangle non-ASCII clipboard content into Latin-1.
            let mut sanitized = String::new();
            let mut chars = step1.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    // Skip an ESC-based sequence: ESC X ... terminator (>= 0x40).
                    for c2 in chars.by_ref() {
                        if (c2 as u32) >= 0x40 {
                            break;
                        }
                    }
                } else if (c as u32) < 0x20 && c != '\t' && c != '\n' && c != '\r' {
                    // Skip other control characters except tab, newline, carriage return.
                } else {
                    sanitized.push(c);
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

/// Maximum time to hold a synchronized-output buffer before forcibly waking the
/// UI. 16 ms gives ~1 frame at 60 Hz; applications should close the bracket well
/// within this, but we never stall longer.
const SYNC_TIMEOUT: Duration = Duration::from_millis(16);

/// Scan a VT byte run for `CSI > 4 ; N m` (XTMODKEYS modifyOtherKeys).
///
/// Returns `Some(level)` if such a sequence is found in `bytes`, where `level`
/// is the `N` parameter (0=reset, 1=enable-except-well-defined, 2=enable-all).
/// The caller is responsible for side-effecting application state; the byte run
/// is still passed to the alacritty VT parser unchanged (alacritty ignores the
/// sequence since it does not implement it, but we do here).
fn scan_modify_other_keys(bytes: &[u8]) -> Option<ModifyOtherKeys> {
    let mut result = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            i += 1;
            continue;
        }
        // ESC >  (aka DECKPAM-alt / private CSI introducer for xterm private sequences)
        // CSI > is  ESC [ > ...  — two-byte CSI then '>'
        if bytes.get(i + 1) == Some(&b'[') && bytes.get(i + 2) == Some(&b'>') {
            // Scan the parameter list and final byte.
            let mut j = i + 3;
            let mut params = Vec::new();
            let mut cur: Option<u16> = None;
            while j < bytes.len() {
                let b = bytes[j];
                if b.is_ascii_digit() {
                    cur = Some(cur.unwrap_or(0) * 10 + (b - b'0') as u16);
                    j += 1;
                } else if b == b';' {
                    params.push(cur.unwrap_or(0));
                    cur = None;
                    j += 1;
                } else {
                    params.push(cur.unwrap_or(0));
                    j += 1;
                    // final byte
                    if b == b'm' && params.len() >= 2 && params[0] == 4 {
                        // The LAST matching sequence in the buffer wins: an app may
                        // set then reset the level within a single read; the final
                        // state is what must be applied.
                        result = Some(match params[1] {
                            0 => ModifyOtherKeys::Reset,
                            1 => ModifyOtherKeys::EnableExceptWellDefined,
                            2 => ModifyOtherKeys::EnableAll,
                            _ => ModifyOtherKeys::Reset,
                        });
                    }
                    break;
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    result
}

/// Scan a VT byte run for DECSET/DECRST 2026 (synchronized output).
/// Returns `(begin_count, end_count)` of `?2026h` / `?2026l` sequences found.
fn scan_sync_2026(bytes: &[u8]) -> (u32, u32) {
    let mut begin = 0u32;
    let mut end = 0u32;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            i += 1;
            continue;
        }
        if bytes.get(i + 1) == Some(&b'[') && bytes.get(i + 2) == Some(&b'?') {
            // CSI ? ... h/l — scan param
            let mut j = i + 3;
            let mut num: u32 = 0;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                num = num * 10 + (bytes[j] - b'0') as u32;
                j += 1;
            }
            // Skip any trailing sub-params (`?2026;1h`) so the final byte check
            // below lands on h/l rather than a semicolon.
            while j < bytes.len() && (bytes[j] == b';' || bytes[j].is_ascii_digit()) {
                j += 1;
            }
            if num == 2026 {
                match bytes.get(j) {
                    Some(&b'h') => begin += 1,
                    Some(&b'l') => end += 1,
                    _ => {}
                }
            }
            i = if j < bytes.len() { j + 1 } else { j };
            continue;
        }
        i += 1;
    }
    (begin, end)
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
///
/// New VT features handled here (beyond alacritty_terminal's own parser):
///
/// **modifyOtherKeys** (XTMODKEYS, `CSI > 4 ; N m`): alacritty_terminal 0.26
/// does not implement `set_modify_other_keys`, so we scan VT byte runs for the
/// sequence and forward a `UserEvent::ModifyOtherKeys` to the UI thread, which
/// updates `App::modify_other_keys`; `encode_key` then uses that state.
///
/// **Synchronized Output** (DECSET 2026, `CSI ? 2026 h/l`): when an
/// application opens a synchronized update (`?2026h`) we hold the UI wakeup
/// until the matching `?2026l` end marker or at most `SYNC_TIMEOUT` (16 ms),
/// whichever comes first. This avoids waking the renderer mid-frame and tearing
/// complex full-screen redraws. The VT parser still receives all bytes eagerly
/// (so terminal state stays up to date) — we only delay the `Wakeup` event.
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

    // ---- Synchronized output state (DECSET 2026) --------------------------------
    // `sync_depth > 0` means the application has opened a synchronized-output
    // bracket (`?2026h`) without a matching close (`?2026l`). We suppress the UI
    // Wakeup while depth > 0, emitting it only when the bracket closes or when
    // `sync_deadline` elapses (hard cap to avoid stalling the UI indefinitely).
    let mut sync_depth: u32 = 0;
    let mut sync_deadline: Option<Instant> = None;
    // Tracks whether any terminal data was processed in the current sync bracket
    // (so we know whether to send a Wakeup when the bracket closes).
    let mut sync_pending_wakeup = false;
    // ---- End sync state ---------------------------------------------------------

    'main: loop {
        // Compute the poll timeout: while inside a sync bracket, wake at the
        // deadline so we never stall the UI longer than SYNC_TIMEOUT even if
        // `?2026l` never arrives.
        let timeout = if sync_depth > 0 {
            sync_deadline.map(|d| d.saturating_duration_since(Instant::now()))
        } else {
            None // block until ready
        };

        events.clear();
        if poller.wait(&mut events, timeout).is_err() {
            break;
        }

        // Check for sync timeout expiry: if the deadline passed and we're still
        // inside a bracket, force-close it and wake the UI.
        if sync_depth > 0
            && sync_deadline.is_some_and(|d| Instant::now() >= d)
        {
            sync_depth = 0;
            sync_deadline = None;
            if sync_pending_wakeup {
                sync_pending_wakeup = false;
                proxy.send_event(Event::Wakeup);
            }
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
                    let tap_events = tap.process(&buf[..n], &images);
                    let mut term = term.lock();
                    let mut did_process = false;

                    for ev in tap_events {
                        match ev {
                            crate::image::TapEvent::Vt(bytes) => {
                                // ---- modifyOtherKeys interception ----------------
                                // Scan for `CSI > 4 ; N m` before feeding to the
                                // alacritty parser (which ignores the sequence).
                                // Forward the level to the UI thread so encode_key
                                // can emit the CSI 27 ; mods ; code ~ form.
                                if let Some(level) = scan_modify_other_keys(&bytes) {
                                    proxy.send_user(UserEvent::ModifyOtherKeys(proxy.id, level));
                                }

                                // ---- Synchronized output interception ------------
                                // Scan for ?2026h (begin) and ?2026l (end) before
                                // feeding to the parser. The bytes still go to
                                // alacritty (which handles DECSET/DECRST for its own
                                // purposes; 2026 is unknown to it and ignored).
                                let (begin_count, end_count) = scan_sync_2026(&bytes);
                                if begin_count > 0 {
                                    if sync_depth == 0 {
                                        // Arm the deadline on the first open.
                                        sync_deadline = Some(Instant::now() + SYNC_TIMEOUT);
                                    }
                                    sync_depth = sync_depth.saturating_add(begin_count);
                                }
                                if end_count > 0 {
                                    sync_depth = sync_depth.saturating_sub(end_count);
                                    if sync_depth == 0 {
                                        sync_deadline = None;
                                    }
                                }

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
                                did_process = true;
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
                                did_process = true;
                            }
                            crate::image::TapEvent::Delete(id) => {
                                images.lock().delete(id);
                                did_process = true;
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
                            crate::image::TapEvent::Progress(state) => {
                                // OSC 9;4: progress report — forward to the UI thread
                                // so it can render the progress indicator in the status
                                // bar / tab chip.
                                proxy.send_user(UserEvent::Progress(proxy.id, state));
                            }
                        }
                    }
                    drop(term);

                    // Wake the UI only when we are outside a synchronized-output
                    // bracket. Inside the bracket, set `sync_pending_wakeup` so the
                    // wakeup is sent when the bracket closes (or times out).
                    if did_process {
                        if sync_depth == 0 {
                            proxy.send_event(Event::Wakeup);
                        } else {
                            sync_pending_wakeup = true;
                        }
                    }
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

/// Merge extra word-separator characters into alacritty's default
/// `SEMANTIC_ESCAPE_CHARS` string, deduplicating. Used at `Pty::spawn` time
/// so the configured separators act as word boundaries for double-click
/// semantic selection from the first frame.
pub fn merge_word_separators(defaults: &str, extras: &str) -> String {
    if extras.is_empty() {
        return defaults.to_owned();
    }
    let mut chars: Vec<char> = defaults.chars().collect();
    for c in extras.chars() {
        if !chars.contains(&c) {
            chars.push(c);
        }
    }
    chars.iter().collect()
}

#[cfg(test)]
mod tests {
    use super::{merge_word_separators, scan_modify_other_keys, scan_sync_2026, ModifyOtherKeys};

    // ---- scan_modify_other_keys tests ----------------------------------------

    #[test]
    fn scan_mok_level2() {
        // CSI > 4 ; 2 m  (enable-all)
        let seq = b"\x1b[>4;2m";
        assert_eq!(scan_modify_other_keys(seq), Some(ModifyOtherKeys::EnableAll));
    }

    #[test]
    fn scan_mok_level1() {
        let seq = b"\x1b[>4;1m";
        assert_eq!(
            scan_modify_other_keys(seq),
            Some(ModifyOtherKeys::EnableExceptWellDefined)
        );
    }

    #[test]
    fn scan_mok_reset() {
        let seq = b"\x1b[>4;0m";
        assert_eq!(scan_modify_other_keys(seq), Some(ModifyOtherKeys::Reset));
    }

    #[test]
    fn scan_mok_not_found_in_normal_text() {
        assert_eq!(scan_modify_other_keys(b"hello world"), None);
    }

    #[test]
    fn scan_mok_embedded_in_longer_run() {
        // Normal output before + after the CSI > 4 ; 2 m sequence.
        let seq = b"abc\x1b[>4;2mdef";
        assert_eq!(scan_modify_other_keys(seq), Some(ModifyOtherKeys::EnableAll));
    }

    #[test]
    fn scan_mok_different_param_not_4_ignored() {
        // CSI > 5 ; 2 m — different resource (not modifyOtherKeys)
        let seq = b"\x1b[>5;2m";
        assert_eq!(scan_modify_other_keys(seq), None);
    }

    // ---- scan_sync_2026 tests ------------------------------------------------

    #[test]
    fn scan_sync_begin_only() {
        let seq = b"\x1b[?2026h";
        let (begin, end) = scan_sync_2026(seq);
        assert_eq!(begin, 1);
        assert_eq!(end, 0);
    }

    #[test]
    fn scan_sync_end_only() {
        let seq = b"\x1b[?2026l";
        let (begin, end) = scan_sync_2026(seq);
        assert_eq!(begin, 0);
        assert_eq!(end, 1);
    }

    #[test]
    fn scan_sync_begin_and_end_pair() {
        // A complete synchronized update bracket in one buffer.
        let seq = b"\x1b[?2026h...content...\x1b[?2026l";
        let (begin, end) = scan_sync_2026(seq);
        assert_eq!(begin, 1);
        assert_eq!(end, 1);
    }

    #[test]
    fn scan_sync_no_match_in_normal_text() {
        let (begin, end) = scan_sync_2026(b"hello\r\n");
        assert_eq!((begin, end), (0, 0));
    }

    #[test]
    fn scan_sync_other_private_mode_ignored() {
        // DECSET 1049 (alt screen) must not count.
        let (begin, end) = scan_sync_2026(b"\x1b[?1049h\x1b[?1049l");
        assert_eq!((begin, end), (0, 0));
    }

    // ---- merge_word_separators tests -----------------------------------------

    #[test]
    fn merge_word_seps_empty_extras_returns_defaults() {
        assert_eq!(merge_word_separators(",│", ""), ",│");
    }

    #[test]
    fn merge_word_seps_appends_and_dedups() {
        let out = merge_word_separators(",│", ",@");
        assert!(out.contains('@'));
        assert_eq!(out.matches(',').count(), 1);
    }
}
