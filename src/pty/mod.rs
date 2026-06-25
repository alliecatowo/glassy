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
use std::path::PathBuf;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{ClipboardType, Config, Term};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use polling::Poller;
use winit::event_loop::EventLoopProxy;

use crate::image::ImageStore;
use crate::input::ModifyOtherKeys;

pub mod r#loop;
mod scan;

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
    /// Best process name to display for this pane: the running foreground command
    /// (e.g. `vim`, `cargo`, `claude`) when one is detected, else the shell's own
    /// `comm` (e.g. `zsh`, `bash`). `shell_comm` is read from `/proc/<pid>/comm`
    /// once at spawn (passed in by the caller). Returns `None` only when nothing
    /// is known (no /proc, exited child), so callers can fall back to "shell".
    pub fn process_name<'a>(&'a self, shell_comm: Option<&'a str>) -> Option<&'a str> {
        self.foreground_comm
            .as_deref()
            .filter(|c| !c.is_empty())
            .or(shell_comm)
    }

    /// Refresh from `/proc` for the shell with `shell_pid`. Reads the pty's tty
    /// foreground pgid from `/proc/<shell_pid>/stat` field 8, then reads that
    /// process's `comm` and the shell's `cwd` symlink. All reads are best-effort
    /// (`None` on any failure).
    pub fn read(shell_pid: u32) -> Self {
        let cwd = read_proc_cwd(shell_pid);
        let foreground_comm = read_foreground_comm(shell_pid);
        // Resolve the git branch once per refresh (not per frame); the walk + HEAD
        // read is cheap at the 2 s cadence but a measurable idle-CPU cost at 60 Hz.
        let git_branch = cwd.as_deref().and_then(crate::app::read_git_branch);
        Self {
            cwd,
            foreground_comm,
            git_branch,
        }
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
                let Some(Ok(pg)) = tokens.get(2).map(|t| t.parse::<u32>()) else {
                    continue;
                };
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
    /// SGR 5 (slow blink) or SGR 6 (rapid blink) detected in the PTY byte stream
    /// for the given session. The UI thread arms the text-blink timer so cells
    /// that have blink active toggle visibility. Sent at most once per read burst
    /// (the timer keeps firing until explicitly reset to idle).
    TextBlinkPresent(usize),
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
    pub id: usize,
}

impl EventProxy {
    /// Create an EventProxy with the given proxy and session id.
    pub fn new(proxy: EventLoopProxy<UserEvent>, id: usize) -> Self {
        Self { proxy, id }
    }

    /// Forward a pre-built `UserEvent` (already tagged with this proxy's session
    /// id) straight to the winit loop. Used for events the PTY loop derives itself
    /// rather than receiving from alacritty's `EventListener` (e.g. OSC 7 cwd,
    /// which alacritty's `Event` enum does not model).
    pub fn send_user(&self, event: UserEvent) {
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
            Event::ClipboardStore(ty, text) => Some(UserEvent::ClipboardStore(self.id, ty, text)),
            Event::ClipboardLoad(ty, formatter) => Some(UserEvent::ClipboardLoad(
                self.id,
                ty,
                ClipboardFormatter(formatter),
            )),
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
/// The navigation methods ([`PromptTracker::prev_prompt`] /
/// [`PromptTracker::next_prompt`]) are wired into the JumpPrevPrompt /
/// JumpNextPrompt key actions via `App::jump_prompt` in `app/keys.rs`.
#[derive(Default)]
pub struct PromptTracker {
    /// Absolute grid rows of `A` (prompt-start) marks, kept sorted ascending. A
    /// `VecDeque` so the bounded-capacity eviction pops the front in O(1) rather
    /// than memmoving the whole backing buffer.
    pub rows: std::collections::VecDeque<i32>,
}

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
pub(crate) enum LoopMsg {
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
    /// OSC 133 prompt-start row offsets, for jump-to-prompt. The PTY loop records
    /// a new row each time an `A` mark is seen; the UI reads this via
    /// `prompts.lock()` without taking any other lock.
    ///
    /// Consumed by the JumpPrevPrompt / JumpNextPrompt key actions: `App::jump_prompt`
    /// (in `app/keys.rs`) calls `pty.prompts.lock().prev_prompt` / `next_prompt`
    /// against the live `display_offset` and issues a `scroll_display(Scroll::Delta)`
    /// to jump there.
    pub prompts: Arc<Mutex<PromptTracker>>,
    /// PID of the spawned shell process. Used to read `/proc/<shell_pid>/cwd`
    /// and the tty foreground pgid for the pane header and status bar.
    pub shell_pid: u32,
    /// The shell's own `comm` (e.g. `zsh`, `bash`, `fish`), read once at spawn.
    /// Used as the process-name fallback for the tab label / window title while
    /// the shell sits at an idle prompt (no foreground child). `None` if the read
    /// failed.
    pub shell_comm: Option<String>,
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
        default_cursor_shape: alacritty_terminal::vte::ansi::CursorShape,
        default_cursor_blink: bool,
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

        let event_proxy = EventProxy::new(proxy, id);
        let grid = GridSize { cols, rows };
        // Merge user-configured word separators with the default set, deduped.
        let semantic_escape_chars = merge_word_separators(
            alacritty_terminal::term::SEMANTIC_ESCAPE_CHARS,
            word_separator,
        );
        let config = Config {
            scrolling_history: scrollback,
            semantic_escape_chars,
            default_cursor_style: alacritty_terminal::vte::ansi::CursorStyle {
                shape: default_cursor_shape,
                blinking: default_cursor_blink,
            },
            ..Config::default()
        };
        let term = Arc::new(FairMutex::new(Term::new(
            config,
            &grid,
            event_proxy.clone(),
        )));

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
        // 256 KiB stack: the PTY read/parse loop has no deep recursion (only a
        // fixed read buffer + a handful of local variables), so the default
        // 8 MiB OS stack is wasteful. See `r#loop::PTY_THREAD_STACK`.
        std::thread::Builder::new()
            .name(format!("glassy-pty-{id}"))
            .stack_size(r#loop::PTY_THREAD_STACK)
            .spawn(move || {
                r#loop::run_loop(
                    pty,
                    loop_term,
                    event_proxy,
                    rx,
                    loop_poller,
                    loop_images,
                    loop_prompts,
                );
            })?;

        // Read the initial cwd eagerly so the pane header shows the right path on
        // the first frame (before the shell emits its first OSC 7).
        let pane_info = PaneInfo::read(shell_pid);
        // The shell's own comm (zsh/bash/…) is the process-name fallback at an
        // idle prompt; read it once here since the shell pid is stable.
        let shell_comm = std::fs::read_to_string(format!("/proc/{shell_pid}/comm"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(Pty {
            term,
            images,
            prompts,
            shell_pid,
            shell_comm,
            pane_info,
            pane_info_at: Instant::now(),
            tx,
            poller,
        })
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
    use super::{PaneInfo, PromptTracker, merge_word_separators};

    // ---- PromptTracker (OSC 133 jump-to-prompt) tests ------------------------

    #[test]
    fn prompt_tracker_keeps_rows_sorted_and_deduped() {
        let mut t = PromptTracker::new();
        // Insert out of order with a duplicate.
        for r in [30, 10, 20, 10, 40] {
            t.push(r);
        }
        let rows: Vec<i32> = t.rows.iter().copied().collect();
        assert_eq!(rows, vec![10, 20, 30, 40], "sorted + deduped");
    }

    #[test]
    fn prompt_tracker_prev_next_find_neighbors() {
        let mut t = PromptTracker::new();
        for r in [10, 20, 30] {
            t.push(r);
        }
        // From row 25 (between 20 and 30): prev=20, next=30.
        assert_eq!(t.prev_prompt(25), Some(20));
        assert_eq!(t.next_prompt(25), Some(30));
        // Exactly on a mark is excluded (strict </>), so we step past it.
        assert_eq!(t.prev_prompt(20), Some(10));
        assert_eq!(t.next_prompt(20), Some(30));
        // Beyond the ends: no neighbor in that direction.
        assert_eq!(t.prev_prompt(10), None);
        assert_eq!(t.next_prompt(30), None);
    }

    #[test]
    fn prompt_tracker_empty_returns_none() {
        let t = PromptTracker::new();
        assert_eq!(t.prev_prompt(5), None);
        assert_eq!(t.next_prompt(5), None);
    }

    // ---- PaneInfo::process_name tests ----------------------------------------

    #[test]
    fn process_name_prefers_foreground_comm() {
        let info = PaneInfo {
            foreground_comm: Some("vim".into()),
            ..Default::default()
        };
        assert_eq!(info.process_name(Some("zsh")), Some("vim"));
    }

    #[test]
    fn process_name_falls_back_to_shell_at_idle_prompt() {
        let info = PaneInfo::default(); // foreground_comm = None
        assert_eq!(info.process_name(Some("bash")), Some("bash"));
    }

    #[test]
    fn process_name_ignores_empty_foreground_comm() {
        let info = PaneInfo {
            foreground_comm: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(info.process_name(Some("fish")), Some("fish"));
    }

    #[test]
    fn process_name_none_when_nothing_known() {
        let info = PaneInfo::default();
        assert_eq!(info.process_name(None), None);
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

    #[test]
    fn merge_word_seps_empty_defaults_with_extras() {
        let out = merge_word_separators("", "@#");
        assert!(out.contains('@'));
        assert!(out.contains('#'));
    }

    #[test]
    fn merge_word_seps_both_empty() {
        assert_eq!(merge_word_separators("", ""), "");
    }

    #[test]
    fn merge_word_seps_no_duplicates_when_extras_overlap() {
        let out = merge_word_separators("abc", "bcd");
        // 'b', 'c' from extras already in defaults → not duplicated.
        // 'd' is new → added.
        assert_eq!(out.matches('a').count(), 1);
        assert_eq!(out.matches('b').count(), 1);
        assert_eq!(out.matches('c').count(), 1);
        assert_eq!(out.matches('d').count(), 1);
    }

    #[test]
    fn merge_word_seps_preserves_defaults_order() {
        // Default characters appear first, in original order.
        let out = merge_word_separators("xyz", "aw");
        assert!(out.starts_with('x'), "defaults come first: {out:?}");
    }

    #[test]
    fn merge_word_seps_unicode_chars_handled() {
        // Multibyte chars (│ is U+2502, 3 bytes in UTF-8) must not be double-counted.
        let out = merge_word_separators(",│", "│");
        assert_eq!(out.matches('│').count(), 1, "│ should not be duplicated");
    }

    #[test]
    fn merge_word_seps_all_duplicates_no_change_in_length() {
        // If every extra char is already in defaults, length stays the same.
        let defaults = ",.:";
        let extras = ",:";
        let out = merge_word_separators(defaults, extras);
        assert_eq!(out.chars().count(), defaults.chars().count());
    }
}
