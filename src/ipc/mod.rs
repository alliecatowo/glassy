//! Single-instance IPC over a Unix domain socket.
//!
//! Wayland has **no portable global-hotkey API**: a client cannot register a
//! system-wide accelerator the way X11's `XGrabKey` allowed. The portable answer
//! a "quake / dropdown terminal" needs is therefore split in two:
//!
//! 1. The *running* glassy instance listens on a per-user Unix socket.
//! 2. A second invocation — `glassy toggle` (or `--toggle`) — is a thin **client**
//!    that connects to that socket and writes a one-line command, then exits.
//!
//! The user binds `glassy toggle` to a key in *their compositor* (Hyprland, Sway,
//! GNOME, KDE, …), which is the only layer that can own a true global hotkey on
//! Wayland. See `docs/quake-mode.md` for the per-compositor bind recipes.
//!
//! The socket path is `$XDG_RUNTIME_DIR/glassy-<uid>.sock` when the runtime dir is
//! available (the correct, auto-cleaned location on modern Linux), falling back to
//! `$TMPDIR`/`/tmp` keyed by username. Commands are newline-terminated ASCII verbs
//! (`toggle`, `show`, `hide`) so the wire format stays trivial and forward-compatible.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::time::Duration;

use winit::event_loop::EventLoopProxy;

use crate::pty::UserEvent;

pub mod control;

use control::{ControlReply, ControlRequest};

/// How long the server listener waits for the UI thread to apply a control
/// request and reply before giving up (so a busy/locked UI never wedges the
/// client connection thread forever). Generous — the UI applies on its next
/// `user_event` turn, which is essentially immediate.
const CONTROL_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// A control command delivered over the IPC socket (or the in-app keybind). Kept
/// tiny and `Copy` so it can ride inside [`UserEvent`] without allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IpcCommand {
    /// Slide the window in if hidden, out if shown.
    Toggle,
    /// Slide the window in (idempotent if already shown).
    Show,
    /// Slide the window out (idempotent if already hidden).
    Hide,
}

impl IpcCommand {
    /// Parse a wire verb (case-insensitive, trimmed) into a command.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "toggle" => Some(Self::Toggle),
            "show" => Some(Self::Show),
            "hide" => Some(Self::Hide),
            _ => None,
        }
    }

    /// The wire verb for this command (what the client writes).
    pub fn verb(self) -> &'static str {
        match self {
            Self::Toggle => "toggle",
            Self::Show => "show",
            Self::Hide => "hide",
        }
    }
}

/// The per-user socket path: `$XDG_RUNTIME_DIR/glassy-<uid>.sock` if the runtime
/// dir is set (preferred; the kernel cleans it on logout), else a `$TMPDIR`/`/tmp`
/// path keyed by `$USER` so two users on one box don't collide. Returns `None`
/// only if neither a runtime dir nor a temp dir nor a username can be resolved
/// (effectively never on a real session).
pub fn socket_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR")
        && !dir.is_empty()
    {
        // The runtime dir is already per-uid, but tag the filename with the uid
        // anyway so it stays stable and obvious. `id -u` via libc would need a
        // dep; the runtime-dir path is itself unique per user, so a fixed name is
        // safe here.
        return Some(PathBuf::from(dir).join("glassy.sock"));
    }
    // Fallback: $TMPDIR or /tmp, namespaced by username to avoid cross-user clash.
    let tmp = std::env::var_os("TMPDIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let user = std::env::var("USER").unwrap_or_else(|_| "default".to_string());
    // Sanitize the username for a filename (keep it boring + path-safe).
    let user: String = user
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    Some(tmp.join(format!("glassy-{user}.sock")))
}

/// CLIENT: connect to a running instance's socket and send `cmd`. Returns
/// `Ok(true)` if a running instance accepted the command, `Ok(false)` if no
/// instance is listening (the caller should then probably start one or print a
/// hint), or an error on an unexpected I/O failure.
pub fn send_command(cmd: IpcCommand) -> std::io::Result<bool> {
    let Some(path) = socket_path() else {
        return Ok(false);
    };
    match UnixStream::connect(&path) {
        Ok(mut stream) => {
            stream.write_all(cmd.verb().as_bytes())?;
            stream.write_all(b"\n")?;
            stream.flush()?;
            Ok(true)
        }
        // No live server (connection refused / no such file) — not an error, just
        // "nobody is home". A stale socket file left by a crashed instance also
        // lands here as ConnectionRefused, which the next server start cleans up.
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// CLIENT: send a kitty-style remote-control request line (`@ <verb> …`) to the
/// running instance and read its single-line reply. Returns the parsed
/// [`ControlReply`], or `Ok(None)` if no instance is listening (so the caller can
/// print a hint), or an I/O error.
///
/// The request line is written verbatim (the running instance parses it); the
/// reply is read back as one line and parsed into `OK …` / `ERR …`.
pub fn send_control(request_line: &str) -> std::io::Result<Option<ControlReply>> {
    let Some(path) = socket_path() else {
        return Ok(None);
    };
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e)
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    // Mark the line as control by prefixing `@` if the caller didn't (the server
    // routes any line that isn't a bare toggle/show/hide verb to the control
    // parser anyway, but the explicit prefix keeps the wire self-describing).
    let line = request_line.trim();
    let wire = if line.starts_with('@') {
        line.to_string()
    } else {
        format!("@ {line}")
    };
    stream.write_all(wire.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()?;
    // Read the single reply line.
    let mut reader = BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply)?;
    let reply = reply.trim();
    if let Some(rest) = reply.strip_prefix("OK ") {
        Ok(Some(ControlReply::Ok(rest.to_string())))
    } else if reply == "OK" {
        Ok(Some(ControlReply::Ok(String::new())))
    } else if let Some(rest) = reply.strip_prefix("ERR ") {
        Ok(Some(ControlReply::Err(rest.to_string())))
    } else if reply.is_empty() {
        Ok(Some(ControlReply::Err(
            "no reply from instance".to_string(),
        )))
    } else {
        Ok(Some(ControlReply::Err(reply.to_string())))
    }
}

/// SERVER: bind the single-instance socket and spawn a listener thread that turns
/// each received verb into a [`UserEvent::Ipc`] delivered to the winit loop.
///
/// A stale socket left behind by a crashed instance (one that no live process is
/// `connect`-able on) is unlinked and rebound, so a crash never permanently wedges
/// the toggle. If another *live* instance already owns the socket, this returns
/// `Ok(false)` — the caller is then a secondary instance and may choose to forward
/// its launch intent and exit. On success the listening thread runs for the
/// process lifetime; the socket file is unlinked in [`cleanup`] on exit.
pub fn start_server(proxy: EventLoopProxy<UserEvent>) -> std::io::Result<bool> {
    let Some(path) = socket_path() else {
        return Ok(false);
    };

    // If a socket file already exists, probe it: a successful connect means a live
    // instance owns it (we're a secondary — don't steal it); a refused connect
    // means it is stale, so remove and rebind.
    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(_) => return Ok(false), // a live instance owns the socket
            Err(_) => {
                let _ = std::fs::remove_file(&path);
            }
        }
    }

    let listener = UnixListener::bind(&path)?;
    // Lock the socket down to the owner (0600). In the `$XDG_RUNTIME_DIR` case the
    // dir is already 0700, but the `/tmp` fallback (always, e.g. on macOS where
    // `$XDG_RUNTIME_DIR` is unset) lives in a world-writable dir — without this any
    // local user could connect and drive this instance's quake window. Done after
    // bind because the file doesn't exist until then.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Err(e) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
            // Non-fatal: log and continue (the socket still works; this only
            // tightens access). A failure here shouldn't wedge the toggle feature.
            log::warn!("ipc: could not chmod 0600 {}: {e}", path.display());
        }
    }
    log::info!("ipc: listening on {}", path.display());

    std::thread::Builder::new()
        .name("glassy-ipc".into())
        .spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => handle_client(stream, &proxy),
                    Err(e) => {
                        log::debug!("ipc: accept error: {e}");
                        // A transient accept error shouldn't kill the listener.
                        continue;
                    }
                }
            }
        })?;
    Ok(true)
}

/// Read one command line from a client connection and forward it to the loop.
///
/// A line that is a bare `toggle`/`show`/`hide` verb drives the quake window
/// ([`UserEvent::Ipc`], no reply). Anything else — including the explicit `@ …`
/// / `msg …` forms — is a kitty-style remote-control request: it is parsed,
/// dispatched as a [`UserEvent::Control`] carrying a one-shot reply channel, and
/// the UI thread's [`ControlReply`] is written back to the client.
fn handle_client(mut stream: UnixStream, proxy: &EventLoopProxy<UserEvent>) {
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        match reader.read_line(&mut line) {
            Ok(0) => return, // client closed without sending
            Ok(_) => {}
            Err(e) => {
                log::debug!("ipc: read error: {e}");
                return;
            }
        }
    }
    let trimmed = line.trim();

    // Bare window verb (no reply expected by the client).
    let is_control = trimmed.starts_with('@') || trimmed.starts_with("msg ") || trimmed == "msg";
    if !is_control && let Some(cmd) = IpcCommand::parse(trimmed) {
        if let Err(e) = proxy.send_event(UserEvent::Ipc(cmd)) {
            log::debug!("ipc: failed to forward {cmd:?}: {e}");
        }
        return;
    }

    // Remote-control request: parse, dispatch, and write the reply back.
    let reply = match control::parse_request(trimmed) {
        Ok(command) => {
            let (req, rx) = ControlRequest::new(command);
            if proxy.send_event(UserEvent::Control(req)).is_err() {
                ControlReply::Err("instance is shutting down".to_string())
            } else {
                match rx.recv_timeout(CONTROL_REPLY_TIMEOUT) {
                    Ok(r) => r,
                    Err(_) => ControlReply::Err("timed out waiting for instance".to_string()),
                }
            }
        }
        Err(msg) => ControlReply::Err(msg),
    };
    let _ = stream.write_all(reply.to_wire().as_bytes());
    let _ = stream.write_all(b"\n");
    let _ = stream.flush();
}

/// Unlink the single-instance socket file on a clean exit so the next launch binds
/// fresh (a crash leaves a stale file, which `start_server`'s probe handles too).
pub fn cleanup() {
    if let Some(path) = socket_path() {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_roundtrips_through_verb() {
        for cmd in [IpcCommand::Toggle, IpcCommand::Show, IpcCommand::Hide] {
            assert_eq!(IpcCommand::parse(cmd.verb()), Some(cmd));
        }
    }

    #[test]
    fn parse_is_case_insensitive_and_trims() {
        assert_eq!(IpcCommand::parse("  TOGGLE \n"), Some(IpcCommand::Toggle));
        assert_eq!(IpcCommand::parse("Show"), Some(IpcCommand::Show));
        assert_eq!(IpcCommand::parse("HIDE"), Some(IpcCommand::Hide));
    }

    #[test]
    fn parse_rejects_unknown() {
        assert_eq!(IpcCommand::parse("frobnicate"), None);
        assert_eq!(IpcCommand::parse(""), None);
    }

    #[test]
    fn socket_path_prefers_runtime_dir() {
        // Save + restore the env so the test is hermetic.
        let prev_runtime = std::env::var_os("XDG_RUNTIME_DIR");
        // SAFETY: single-threaded test; we restore the var before returning.
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/run/user/test-xyz");
        }
        let p = socket_path().unwrap();
        assert_eq!(p, PathBuf::from("/run/user/test-xyz/glassy.sock"));
        unsafe {
            match prev_runtime {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    fn socket_path_falls_back_to_tmp() {
        let prev_runtime = std::env::var_os("XDG_RUNTIME_DIR");
        let prev_tmp = std::env::var_os("TMPDIR");
        let prev_user = std::env::var_os("USER");
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
            std::env::set_var("TMPDIR", "/tmp/glassy-test");
            std::env::set_var("USER", "alice");
        }
        let p = socket_path().unwrap();
        assert_eq!(p, PathBuf::from("/tmp/glassy-test/glassy-alice.sock"));
        unsafe {
            match prev_runtime {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
            match prev_tmp {
                Some(v) => std::env::set_var("TMPDIR", v),
                None => std::env::remove_var("TMPDIR"),
            }
            match prev_user {
                Some(v) => std::env::set_var("USER", v),
                None => std::env::remove_var("USER"),
            }
        }
    }

    #[test]
    fn send_command_returns_false_when_no_server() {
        // Point at a path that no server is listening on.
        let prev_runtime = std::env::var_os("XDG_RUNTIME_DIR");
        let prev_tmp = std::env::var_os("TMPDIR");
        unsafe {
            std::env::remove_var("XDG_RUNTIME_DIR");
            std::env::set_var("TMPDIR", std::env::temp_dir());
            std::env::set_var("USER", "nobody-glassy-test-xyz");
        }
        // Make sure no stale file is in the way.
        if let Some(p) = socket_path() {
            let _ = std::fs::remove_file(p);
        }
        assert!(!send_command(IpcCommand::Toggle).unwrap());
        unsafe {
            match prev_runtime {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
            match prev_tmp {
                Some(v) => std::env::set_var("TMPDIR", v),
                None => std::env::remove_var("TMPDIR"),
            }
        }
    }
}
