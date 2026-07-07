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
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{ClipboardType, Config, Term};
use alacritty_terminal::tty::{self, Options as PtyOptions, Shell};
use polling::Poller;
use winit::event_loop::EventLoopProxy;

use crate::image::ImageStore;
use crate::input::ModifyOtherKeys;

pub mod cmdzone;
pub mod r#loop;
pub mod overline;
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

    /// Refresh the shell's cwd and the tty's foreground-process name for
    /// `shell_pid`. Linux reads `/proc`; macOS reads libproc. All reads are
    /// best-effort (`None` on any failure). `git_branch` is pure filesystem.
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

/// Read the shell's own `comm`/name (e.g. `zsh`, `bash`) — the process-name
/// fallback shown at an idle prompt. Platform-dispatched like the other reads.
pub(crate) fn read_shell_comm(shell_pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string(format!("/proc/{shell_pid}/comm"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }
    #[cfg(target_os = "macos")]
    {
        macos_procinfo::shell_comm(shell_pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = shell_pid;
        None
    }
}

/// Read the shell process's cwd. Dispatches to the platform implementation.
fn read_proc_cwd(pid: u32) -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        read_proc_cwd_linux(pid)
    }
    #[cfg(target_os = "macos")]
    {
        macos_procinfo::cwd(pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

/// Read the tty foreground process name for `shell_pid`. Dispatches to the
/// platform implementation; `None` when the shell itself is foreground (idle).
fn read_foreground_comm(shell_pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        read_foreground_comm_linux(shell_pid)
    }
    #[cfg(target_os = "macos")]
    {
        macos_procinfo::foreground_comm(shell_pid)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = shell_pid;
        None
    }
}

/// Read `/proc/<pid>/cwd` (a symlink to the process's cwd).
#[cfg(target_os = "linux")]
fn read_proc_cwd_linux(pid: u32) -> Option<PathBuf> {
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
#[cfg(target_os = "linux")]
fn read_foreground_comm_linux(shell_pid: u32) -> Option<String> {
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

/// macOS pane-info via libproc (no `/proc`). `proc_pidinfo(PROC_PIDTBSDINFO)`
/// exposes `e_tpgid` — the tty foreground pgid, the exact analog of Linux stat's
/// `tpgid` — and `pbi_pgid`, the shell's own pgid for the idle comparison. So the
/// shell pid alone suffices; the pty master fd / `tcgetpgrp` are not needed.
#[cfg(target_os = "macos")]
mod macos_procinfo {
    use std::ffi::{CStr, c_char, c_int, c_void};
    use std::path::PathBuf;

    const PROC_PIDTBSDINFO: c_int = 3;
    const PROC_PIDVNODEPATHINFO: c_int = 9;
    const MAXCOMLEN: usize = 16;
    const MAXPATHLEN: usize = 1024;
    const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * MAXPATHLEN;
    const NODEV: u32 = u32::MAX;

    // libproc is part of libSystem — no #[link] needed. proc_pidinfo's buffersize
    // is c_int; proc_name/proc_pidpath's is u32 (verified against libproc.h).
    unsafe extern "C" {
        fn proc_pidinfo(
            pid: c_int,
            flavor: c_int,
            arg: u64,
            buffer: *mut c_void,
            buffersize: c_int,
        ) -> c_int;
        fn proc_name(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
        fn proc_pidpath(pid: c_int, buffer: *mut c_void, buffersize: u32) -> c_int;
        fn proc_listchildpids(ppid: c_int, buffer: *mut c_void, buffersize: c_int) -> c_int;
    }

    #[repr(C)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        pbi_uid: u32,
        pbi_gid: u32,
        pbi_ruid: u32,
        pbi_rgid: u32,
        pbi_svuid: u32,
        pbi_svgid: u32,
        rfu_1: u32,
        pbi_comm: [c_char; MAXCOMLEN],
        pbi_name: [c_char; 2 * MAXCOMLEN],
        pbi_nfiles: u32,
        pbi_pgid: u32,
        pbi_pjobc: u32,
        e_tdev: u32,
        e_tpgid: u32,
        pbi_nice: i32,
        pbi_start_tvsec: u64,
        pbi_start_tvusec: u64,
    }

    // The cwd C-string sits at offset 152 inside vnode_info_path (the vnode_info
    // prefix is 152 bytes). Model the prefix opaquely to avoid transcribing
    // vinfo_stat; size asserts below catch any future ABI drift.
    #[repr(C)]
    struct VnodeInfoPath {
        vip_vi: [u8; 152],
        vip_path: [c_char; MAXPATHLEN],
    }
    #[repr(C)]
    struct ProcVnodePathInfo {
        pvi_cdir: VnodeInfoPath,
        pvi_rdir: VnodeInfoPath,
    }

    // Compile-time ABI guards (verified against the macOS SDK).
    const _: () = assert!(std::mem::size_of::<ProcBsdInfo>() == 136);
    const _: () = assert!(std::mem::size_of::<ProcVnodePathInfo>() == 2352);

    /// The pid to actually query for shell info. glassy's shell is often spawned
    /// behind `/usr/bin/login` (root, setuid) on macOS, which a normal-user process
    /// can't introspect via libproc. In that case the real shell is `login`'s
    /// same-uid child on the same controlling tty, so its cwd and tty foreground
    /// pgid are exactly what we want. If `pid` itself is queryable, use it directly.
    fn queryable_pid(pid: u32) -> u32 {
        if raw_bsd_info(pid).is_some() {
            return pid;
        }
        first_child_pid(pid).unwrap_or(pid)
    }

    /// First child pid of `ppid` via `proc_listchildpids`, or `None`.
    fn first_child_pid(ppid: u32) -> Option<u32> {
        // SAFETY: proc_listchildpids fills a pid_t (i32) array and returns the
        // *count* of pids written (not a byte count). We size the buffer generously
        // and read only what it reports.
        unsafe {
            let mut pids = [0i32; 64];
            let cap = (pids.len() * std::mem::size_of::<i32>()) as c_int;
            let n = proc_listchildpids(ppid as c_int, pids.as_mut_ptr() as *mut c_void, cap);
            if n <= 0 {
                return None;
            }
            let count = (n as usize).min(pids.len());
            pids[..count]
                .iter()
                .copied()
                .find(|&p| p > 0)
                .map(|p| p as u32)
        }
    }

    /// `proc_pidinfo(PROC_PIDTBSDINFO)` on `pid` exactly (no login descent).
    fn raw_bsd_info(pid: u32) -> Option<ProcBsdInfo> {
        // SAFETY: zeroed POD; proc_pidinfo writes exactly size_of bytes on success.
        unsafe {
            let mut bi: ProcBsdInfo = std::mem::zeroed();
            let size = std::mem::size_of::<ProcBsdInfo>() as c_int;
            let n = proc_pidinfo(
                pid as c_int,
                PROC_PIDTBSDINFO,
                0,
                &mut bi as *mut _ as *mut c_void,
                size,
            );
            (n == size).then_some(bi)
        }
    }

    /// `proc_pidinfo(PROC_PIDTBSDINFO)` → the shell's BSD info, descending through
    /// a `login` wrapper to the real shell when needed.
    fn bsd_info(pid: u32) -> Option<ProcBsdInfo> {
        raw_bsd_info(queryable_pid(pid))
    }

    fn cstr_field(buf: &[c_char]) -> Option<String> {
        // SAFETY: buf is a NUL-terminated C string field from a libproc struct.
        let s = unsafe { CStr::from_ptr(buf.as_ptr()) }.to_string_lossy();
        let s = s.trim();
        (!s.is_empty()).then(|| s.to_string())
    }

    /// The shell's own short name (e.g. `zsh`), from `pbi_comm`.
    pub(super) fn shell_comm(pid: u32) -> Option<String> {
        cstr_field(&bsd_info(pid)?.pbi_comm)
    }

    /// The tty foreground process name, or `None` when the shell is foreground
    /// (idle prompt) — mirrors the Linux `tpgid == shell_pgid` check exactly.
    pub(super) fn foreground_comm(shell_pid: u32) -> Option<String> {
        let bi = bsd_info(shell_pid)?;
        // No controlling tty, or shell is the foreground group (idle): nothing to show.
        if bi.e_tdev == NODEV || bi.e_tpgid == 0 || bi.e_tpgid == bi.pbi_pgid {
            return None;
        }
        // The foreground pgid leader's pid == pgid in the common case. Prefer
        // proc_name (short, like Linux comm); fall back to proc_pidpath's basename.
        proc_short_name(bi.e_tpgid).or_else(|| proc_path_basename(bi.e_tpgid))
    }

    /// The shell process's cwd, from `PROC_PIDVNODEPATHINFO`. Descends through a
    /// `login` wrapper to the real shell (same as bsd_info) so the cwd is the
    /// shell's, not login's.
    pub(super) fn cwd(pid: u32) -> Option<PathBuf> {
        let pid = queryable_pid(pid);
        // SAFETY: zeroed POD; proc_pidinfo writes the struct and returns >0 on success.
        unsafe {
            let mut vpi: ProcVnodePathInfo = std::mem::zeroed();
            let size = std::mem::size_of::<ProcVnodePathInfo>() as c_int;
            let n = proc_pidinfo(
                pid as c_int,
                PROC_PIDVNODEPATHINFO,
                0,
                &mut vpi as *mut _ as *mut c_void,
                size,
            );
            if n <= 0 {
                return None;
            }
            cstr_field(&vpi.pvi_cdir.vip_path).map(PathBuf::from)
        }
    }

    fn proc_short_name(pid: u32) -> Option<String> {
        // SAFETY: proc_name writes up to buffersize bytes and returns the count.
        unsafe {
            let mut buf = [0u8; 2 * MAXCOMLEN];
            let n = proc_name(
                pid as c_int,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
            );
            if n <= 0 {
                return None;
            }
            let s = String::from_utf8_lossy(&buf[..n as usize]);
            let s = s.trim();
            (!s.is_empty()).then(|| s.to_string())
        }
    }

    fn proc_path_basename(pid: u32) -> Option<String> {
        // SAFETY: proc_pidpath writes up to buffersize bytes and returns the count.
        unsafe {
            let mut buf = [0u8; PROC_PIDPATHINFO_MAXSIZE];
            let n = proc_pidpath(
                pid as c_int,
                buf.as_mut_ptr() as *mut c_void,
                buf.len() as u32,
            );
            if n <= 0 {
                return None;
            }
            let path = String::from_utf8_lossy(&buf[..n as usize]);
            std::path::Path::new(path.trim())
                .file_name()
                .map(|f| f.to_string_lossy().to_string())
        }
    }
}

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
    /// the shell, with the optional exit code carried by a `D` mark. The PTY loop
    /// records the cursor row + timing + exit status in the per-session
    /// [`PromptTracker`] and forwards this event so the UI can repaint the
    /// command-block badges and update jump-to-prompt state.
    SemanticMark(usize, char, Option<i32>),
    /// OSC 9 or OSC 777 desktop notification from the shell. Carries a structured
    /// [`crate::image::NotifySpec`] (title/body/icon/sound/urgency/actions).
    /// Forwarded to the UI thread so it can fire a rich native notification when
    /// the window is unfocused (and always show an in-app toast).
    Notify(usize, crate::image::NotifySpec),
    /// A remote-control request arrived over the IPC socket (`glassy @ <cmd>` /
    /// `glassy msg …`): open a tab, split, send text, set theme, list, etc. The
    /// UI thread applies it and replies over the one-shot `reply` channel. See
    /// [`crate::ipc::control`]. Carries no session id (window-level command).
    Control(crate::ipc::control::ControlRequest),
    /// OSC 9;4 progress report from the running application. The UI thread stores
    /// the latest state per-session and renders a subtle progress indicator.
    Progress(usize, crate::image::ProgressState),
    /// OSC 1337 `Peek=<path>` inline-preview request (glassy extension). The UI
    /// thread reads a small head of the file and shows a peek card near the cursor.
    Peek(usize, PathBuf),
    /// SGR 5 (slow blink) or SGR 6 (rapid blink) detected in the PTY byte stream
    /// for the given session. The UI thread arms the text-blink timer so cells
    /// that have blink active toggle visibility. Sent at most once per read burst
    /// (the timer keeps firing until explicitly reset to idle).
    TextBlinkPresent(usize),
    /// A full-screen erase (CSI 2J / 3J) or terminal reset (RIS, ESC c) was seen
    /// in the PTY byte stream for the given session, wiping the cells that any
    /// blinking text sat on. The UI thread disarms the text-blink timer for the
    /// active pane so it stops waking the event loop; it re-arms on the next
    /// `TextBlinkPresent` if the redrawn screen actually contains SGR 5/6 cells.
    TextBlinkCleared(usize),
    /// The config file was modified; reload from disk.
    ConfigReload,
    /// The running application changed the xterm modifyOtherKeys level via
    /// `CSI > 4 ; N m`. The UI thread updates `App::modify_other_keys` so
    /// subsequent key events are encoded with the correct form.
    ModifyOtherKeys(usize, ModifyOtherKeys),
    /// The running application toggled SGR-Pixel mouse reporting (DECSET/DECRST
    /// 1016). alacritty_terminal does not model mode 1016, so the PTY loop scans
    /// for it and forwards the new state (`true` = on). The UI thread updates
    /// `App::sgr_pixel_mouse` so `report_mouse` encodes pixel coordinates.
    SgrPixelMouse(usize, bool),
    /// A single-instance IPC control command (`glassy toggle/show/hide`) arrived
    /// over the Unix socket from a second invocation. Drives the quake / dropdown
    /// window's slide animation. Carries no session id — it's a window-level
    /// command, not a per-PTY event. See [`crate::ipc`].
    Ipc(crate::ipc::IpcCommand),
    /// A shell command was captured from the grid between an OSC 133 `B`
    /// (command start) and `C` (command executed) mark. The string is the final
    /// command line as it sat on the grid. The UI thread pushes it into the
    /// per-app command-history ring so the command palette can offer it for
    /// re-run. See [`cmdzone`].
    CommandRun(usize, String),
    /// A macOS global menu-bar item was selected. Carries the [`KeyAction`] the
    /// menu item maps to (New Tab / Split / Copy / Paste / Settings / Quit etc.);
    /// the UI thread runs it through the same `run_key_action` path as a keychord.
    /// Window-level, no session id. Only emitted on macOS (see `app::mac_menu`);
    /// the variant is still matched everywhere, so it is `dead_code` off-macOS.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    MenuAction(crate::config::KeyAction),
    /// An OSC 133-tracked command FINISHED (a `D` mark closed its block) in the
    /// session `usize`. Carries the (best-effort) command text, the exit code, and
    /// how long it ran. The UI thread fires a desktop notification when the
    /// command ran longer than the configured threshold while the window was
    /// unfocused — so a long background build/test alerts the user when it's done.
    /// See `config.notify_command_*`.
    CommandFinished {
        id: usize,
        command: Option<String>,
        exit: Option<i32>,
        duration: Duration,
    },
    /// Linux only: the system light/dark color-scheme preference changed (or
    /// was read for the first time at startup), as reported by
    /// `org.freedesktop.portal.Settings` over D-Bus (see
    /// `crate::app::system_theme`). winit never emits
    /// `WindowEvent::ThemeChanged` on Linux, so this is the Linux equivalent
    /// of that event; the UI thread applies it via the same
    /// `App::apply_system_theme` path. Carries no session id (window-level).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    SystemThemeChanged(winit::window::Theme),
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

/// Maximum number of [`CommandBlock`] records retained per session. Bounds the
/// memory used by Warp-style command-block tracking; the oldest block is evicted
/// when a new prompt pushes past this cap.
const MAX_COMMAND_BLOCKS: usize = 1024;

/// One Warp-style command block: the semantic zone for a single command, derived
/// from the OSC 133 `A`/`B`/`C`/`D` marks the shell emits.
///
/// Rows are *absolute* grid rows (including scrollback): `cursor.line.0 +
/// display_offset` at the moment the mark arrived. The renderer translates them
/// to viewport rows by subtracting the live `display_offset`.
///
/// A block starts on an `A` mark (prompt start). `C` records where the command's
/// output begins (and the wall-clock start time). `D` closes it, recording the
/// end time (so `duration()` is known) and the exit code.
#[derive(Clone, Debug)]
pub struct CommandBlock {
    /// Absolute grid row of the `A` (prompt start) mark.
    pub prompt_row: i32,
    /// Absolute grid row of the `C` (command executed / output start) mark, once
    /// seen. `None` between `A` and `C` (the user is still typing the command).
    pub output_row: Option<i32>,
    /// Absolute grid row of the `D` (command finished) mark, once seen. While
    /// `None` the command is still running.
    pub end_row: Option<i32>,
    /// Exit code from `133;D;<exit>`. `None` while running, or for a `D` with no
    /// numeric exit field.
    pub exit_code: Option<i32>,
    /// Wall-clock instant the command started running (`C` mark). `None` until C.
    pub started_at: Option<Instant>,
    /// Wall-clock instant the command finished (`D` mark). `None` while running.
    pub ended_at: Option<Instant>,
}

impl CommandBlock {
    /// How long the command ran, if both `C` (start) and `D` (end) were seen.
    pub fn duration(&self) -> Option<Duration> {
        match (self.started_at, self.ended_at) {
            (Some(s), Some(e)) => Some(e.saturating_duration_since(s)),
            _ => None,
        }
    }

    /// Whether the command has finished (a `D` mark was recorded).
    pub fn is_finished(&self) -> bool {
        self.end_row.is_some()
    }

    /// Whether the command succeeded (finished with exit code 0). `false` for a
    /// non-zero exit, and `false` while still running / exit unknown.
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

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
    /// Warp-style command blocks, one per prompt zone, in stream (ascending-row)
    /// order. Appended on each `A` mark; the open block's `output_row` /
    /// `end_row` / `exit_code` / timing are filled in as `C` / `D` arrive. Bounded
    /// at [`MAX_COMMAND_BLOCKS`]; the oldest is evicted past the cap.
    pub blocks: std::collections::VecDeque<CommandBlock>,
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

    /// Record an `A` (prompt-start) mark at absolute grid row `row`: also opens a
    /// new [`CommandBlock`] zone. Called from the PTY loop alongside [`push`].
    pub fn begin_block(&mut self, row: i32) {
        self.push(row);
        // Avoid a duplicate block if the same prompt re-draws at the same row
        // without an intervening command (e.g. a redraw on resize).
        if self
            .blocks
            .back()
            .is_some_and(|b| b.prompt_row == row && !b.is_finished())
        {
            return;
        }
        if self.blocks.len() >= MAX_COMMAND_BLOCKS {
            self.blocks.pop_front();
        }
        self.blocks.push_back(CommandBlock {
            prompt_row: row,
            output_row: None,
            end_row: None,
            exit_code: None,
            started_at: None,
            ended_at: None,
        });
    }

    /// Record a `C` (command-executed / output-start) mark at `row`: marks the
    /// open block's output start and start time. A `C` with no open block (e.g.
    /// integration sourced mid-session) is ignored.
    pub fn command_started(&mut self, row: i32, at: Instant) {
        if let Some(b) = self.blocks.back_mut()
            && b.output_row.is_none()
            && !b.is_finished()
        {
            b.output_row = Some(row);
            b.started_at = Some(at);
        }
    }

    /// Record a `D` (command-finished) mark at `row` with optional `exit` code:
    /// closes the open block, recording the end row, end time, and exit status.
    /// A `D` with no open running block is ignored.
    pub fn command_finished(&mut self, row: i32, exit: Option<i32>, at: Instant) {
        if let Some(b) = self.blocks.back_mut()
            && !b.is_finished()
        {
            b.end_row = Some(row);
            b.ended_at = Some(at);
            b.exit_code = exit;
        }
    }

    /// The most-recently-finished command block, if any. Kept for the planned
    /// status-bar "last command" badge (exercised by the command-block tests);
    /// not yet read by the live render path.
    #[allow(dead_code)]
    pub fn last_finished(&self) -> Option<&CommandBlock> {
        self.blocks.iter().rev().find(|b| b.is_finished())
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
    /// SGR 53/55 overline coverage for this session. The PTY loop records which
    /// cells were printed while overline was active (alacritty_terminal has no
    /// overline flag); the render path reads it via `overline.lock()` to set the
    /// per-cell `Decorations::overline` bit. See [`overline::OverlineTracker`].
    pub overline: Arc<Mutex<overline::OverlineTracker>>,
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
            ..term_config_base()
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
        // When launched as a .app bundle (e.g. from Finder/DMG), the process CWD
        // is "/" rather than the user's home. Default to $HOME so the shell opens
        // in a useful directory instead of the filesystem root.
        let working_directory = working_directory.or_else(|| {
            let cwd = std::env::current_dir().ok()?;
            if cwd == std::path::Path::new("/") {
                std::env::var_os("HOME").map(std::path::PathBuf::from)
            } else {
                None
            }
        });

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
        let overline = Arc::new(Mutex::new(overline::OverlineTracker::new()));

        let loop_term = term.clone();
        let loop_poller = poller.clone();
        let loop_images = images.clone();
        let loop_prompts = prompts.clone();
        let loop_overline = overline.clone();
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
                    loop_overline,
                );
            })?;

        // Read the initial cwd eagerly so the pane header shows the right path on
        // the first frame (before the shell emits its first OSC 7).
        let pane_info = PaneInfo::read(shell_pid);
        // The shell's own comm (zsh/bash/…) is the process-name fallback at an
        // idle prompt; read it once here since the shell pid is stable.
        let shell_comm = read_shell_comm(shell_pid);
        Ok(Pty {
            term,
            images,
            prompts,
            overline,
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

    /// Update the terminal's DEFAULT cursor style (shape + blink) live, e.g. from
    /// the settings cursor picker. Rebuilds the Term `Config` exactly as `spawn`
    /// did (same scrollback + word separators) with the new cursor default and
    /// applies it via `Term::set_options`, which preserves the grid/scrollback and
    /// just re-damages. Takes effect immediately unless the child has pushed its
    /// own DECSCUSR style (that override wins, as it should).
    pub fn set_default_cursor(
        &self,
        shape: alacritty_terminal::vte::ansi::CursorShape,
        blinking: bool,
        scrollback: usize,
        word_separator: &str,
    ) {
        let semantic_escape_chars = merge_word_separators(
            alacritty_terminal::term::SEMANTIC_ESCAPE_CHARS,
            word_separator,
        );
        let config = Config {
            scrolling_history: scrollback,
            semantic_escape_chars,
            default_cursor_style: alacritty_terminal::vte::ansi::CursorStyle { shape, blinking },
            ..term_config_base()
        };
        self.term.lock().set_options(config);
    }

    /// Ask the PTY loop to shut down cleanly.
    pub fn shutdown(&self) {
        self.send(LoopMsg::Shutdown);
    }
}

/// The base `alacritty_terminal` [`Config`] every glassy PTY starts from.
///
/// This exists so the non-default fields we ALWAYS want are set in exactly one
/// place — most importantly `kitty_keyboard: true`. That flag enables the kitty
/// keyboard protocol at the root: without it the `Term` silently drops every
/// `set`/`push`/`pop`/`report` keyboard-mode request (each early-returns on this
/// flag in alacritty_terminal), so the `DISAMBIGUATE_ESC_CODES` mode bits never
/// latch (leaving our CSI-u encoder permanently inert) AND the `CSI ? u`
/// progressive-enhancement query is never answered — so apps like Claude Code
/// can't detect support and Shift+Enter degrades to Ctrl+J.
///
/// Every `Config { .. }` literal in glassy must spread `..term_config_base()`
/// (never `..Config::default()`); otherwise a resize or a settings change silently
/// resets the flag to its `false` default and kills the protocol again.
pub(crate) fn term_config_base() -> Config {
    Config {
        kitty_keyboard: true,
        ..Config::default()
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
    use std::time::{Duration, Instant};

    // ---- kitty keyboard protocol negotiation (wire-level) --------------------

    /// Capturing `EventListener` that records the text of every `PtyWrite` the
    /// `Term` emits back toward the PTY — the reply channel apps read to detect
    /// and drive the kitty keyboard protocol.
    struct CaptureListener {
        writes: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    }
    impl alacritty_terminal::event::EventListener for CaptureListener {
        fn send_event(&self, event: alacritty_terminal::event::Event) {
            if let alacritty_terminal::event::Event::PtyWrite(text) = event {
                self.writes.borrow_mut().push(text);
            }
        }
    }

    #[test]
    fn kitty_keyboard_query_is_answered() {
        // The progressive-enhancement QUERY an app (e.g. Claude Code) sends to
        // detect support is `CSI ? u`. With kitty_keyboard enabled at the root
        // (`term_config_base`), the Term must reply `CSI ? <flags> u` — here 0
        // (NO_MODE), since nothing has been pushed. Before the root fix this reply
        // never came, so apps concluded glassy had no kitty support and degraded.
        use super::{GridSize, term_config_base};
        use alacritty_terminal::Term;
        use alacritty_terminal::vte::ansi::Processor;

        let writes = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let listener = CaptureListener {
            writes: writes.clone(),
        };
        let size = GridSize { cols: 80, rows: 24 };
        let mut term = Term::new(term_config_base(), &size, listener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[?u");
        assert_eq!(writes.borrow().as_slice(), &["\x1b[?0u".to_string()]);
    }

    #[test]
    fn kitty_keyboard_push_latches_disambiguate_mode() {
        // Pushing flags via `CSI > 1 u` (DISAMBIGUATE_ESC_CODES) must set the
        // matching TermMode bit — the exact bit our CSI-u encoder keys off to
        // encode Shift+Enter as `\x1b[13;2u`. Before the root fix the push handler
        // early-returned on `!config.kitty_keyboard`, so the bit never latched.
        use super::{GridSize, term_config_base};
        use alacritty_terminal::Term;
        use alacritty_terminal::term::TermMode;
        use alacritty_terminal::vte::ansi::Processor;

        let writes = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let listener = CaptureListener { writes };
        let size = GridSize { cols: 80, rows: 24 };
        let mut term = Term::new(term_config_base(), &size, listener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\x1b[>1u");
        assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
    }

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

    // ---- PromptTracker command-block tests -----------------------------------

    #[test]
    fn command_block_lifecycle_records_zone_timing_exit() {
        let mut t = PromptTracker::new();
        let start = Instant::now();
        t.begin_block(10); // A: prompt start at row 10
        t.command_started(11, start); // C: output starts at row 11
        t.command_finished(20, Some(0), start + Duration::from_millis(500)); // D

        assert_eq!(t.blocks.len(), 1);
        let b = &t.blocks[0];
        assert_eq!(b.prompt_row, 10);
        assert_eq!(b.output_row, Some(11));
        assert_eq!(b.end_row, Some(20));
        assert_eq!(b.exit_code, Some(0));
        assert!(b.is_finished());
        assert!(b.succeeded());
        assert_eq!(b.duration(), Some(Duration::from_millis(500)));
        // A also feeds the row list used by jump-to-prompt.
        assert!(t.rows.contains(&10));
    }

    #[test]
    fn command_block_failure_is_not_success() {
        let mut t = PromptTracker::new();
        t.begin_block(0);
        t.command_started(1, Instant::now());
        t.command_finished(5, Some(130), Instant::now());
        assert!(!t.blocks[0].succeeded());
        assert_eq!(t.blocks[0].exit_code, Some(130));
        assert!(t.last_finished().is_some());
    }

    #[test]
    fn stray_c_and_d_without_a_are_ignored() {
        let mut t = PromptTracker::new();
        // No begin_block first → no open block to mutate.
        t.command_started(1, Instant::now());
        t.command_finished(5, Some(0), Instant::now());
        assert!(t.blocks.is_empty());
        assert!(t.last_finished().is_none());
    }

    #[test]
    fn repeated_a_at_same_row_does_not_duplicate_open_block() {
        let mut t = PromptTracker::new();
        t.begin_block(7);
        t.begin_block(7); // prompt redraw at the same row (e.g. resize)
        assert_eq!(
            t.blocks.len(),
            1,
            "same-row redraw must not open a new block"
        );
    }

    #[test]
    fn new_prompt_after_finish_opens_a_fresh_block() {
        let mut t = PromptTracker::new();
        t.begin_block(0);
        t.command_finished(3, Some(0), Instant::now());
        t.begin_block(4); // next prompt
        assert_eq!(t.blocks.len(), 2);
        assert!(!t.blocks[1].is_finished());
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
