//! Kitty-style remote control over the single-instance Unix socket.
//!
//! In addition to the window-level `toggle`/`show`/`hide` verbs (see the parent
//! [`mod`](super)), a second invocation can drive the *running* glassy with a
//! richer command surface, kitty-`@`-style:
//!
//! ```text
//! glassy @ ls                       # list windows / tabs / panes (text)
//! glassy @ open-tab                 # open a new tab
//! glassy @ split [vertical|horizontal]
//! glassy @ send-text "hello\n"      # type text into the focused pane
//! glassy @ set-theme tokyo-night    # switch the active color theme
//! glassy @ set-color fg #ffffff     # (reserved) set a palette color
//! glassy @ focus-tab 2              # activate tab at 1-based position
//! ```
//!
//! `glassy msg <cmd> …` is accepted as a synonym for `glassy @ <cmd> …`.
//!
//! ## Wire protocol
//!
//! The client writes a single newline-terminated request line and reads a single
//! newline-terminated reply line back, then closes:
//!
//! ```text
//! C→S:  @ <verb> [args…]\n
//! S→C:  OK [text]\n   |   ERR <message>\n
//! ```
//!
//! Args are space-separated; `send-text` takes the *rest of the line* verbatim
//! (after C-style unescaping of `\n`, `\t`, `\\`). The protocol is intentionally
//! line-oriented + ASCII so it stays shell-pipe friendly and forward-compatible.
//!
//! The running instance turns a parsed request into a [`UserEvent::Control`]
//! ([`crate::pty::UserEvent`]) carrying a one-shot reply channel; the UI thread
//! applies it (`App::apply_control`) and sends a [`ControlReply`] back, which the
//! listener writes to the client socket.

use std::sync::mpsc::{Receiver, SyncSender};

/// A parsed remote-control command. Drives `App::apply_control` on the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlCommand {
    /// List windows/tabs/panes as human-readable text.
    Ls,
    /// Open a new tab in the running window.
    OpenTab,
    /// Split the focused pane. `vertical` (side-by-side) or `horizontal` (stacked).
    Split(SplitDir),
    /// Type text into the focused pane (already unescaped).
    SendText(String),
    /// Switch the active color theme by canonical name.
    SetTheme(String),
    /// Set a named palette color (`target` ∈ fg/bg/cursor, `hex` like `#rrggbb`).
    /// Reserved surface; applied where the renderer supports a live override.
    SetColor { target: String, hex: String },
    /// Activate the tab at a 1-based display position.
    FocusTab(usize),
}

/// Split direction for the `split` remote command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDir {
    Vertical,
    Horizontal,
}

impl SplitDir {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "v" | "vert" | "vertical" | "right" => Some(Self::Vertical),
            "h" | "horiz" | "horizontal" | "down" => Some(Self::Horizontal),
            _ => None,
        }
    }
}

/// The reply the UI thread sends back for an applied [`ControlCommand`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlReply {
    /// Success, with optional human-readable text (e.g. the `ls` listing).
    Ok(String),
    /// Failure, with a message explaining why.
    Err(String),
}

impl ControlReply {
    /// Serialize to the single wire line the client reads (no trailing newline).
    pub fn to_wire(&self) -> String {
        match self {
            Self::Ok(s) if s.is_empty() => "OK".to_string(),
            Self::Ok(s) => format!("OK {s}"),
            Self::Err(s) => format!("ERR {s}"),
        }
    }
}

/// A remote-control request in flight: the parsed command plus the one-shot
/// channel the UI thread replies on. Rides inside `UserEvent::Control`.
///
/// `Clone`/`Eq` are derived only so `UserEvent` keeps its derives; the reply
/// sender is wrapped so those derives compile. Cloning shares the same sender
/// (`SyncSender` is `Clone`), and equality compares only the command — two
/// requests are "equal" when their commands match, which the event loop never
/// actually relies on.
#[derive(Debug, Clone)]
pub struct ControlRequest {
    pub command: ControlCommand,
    reply: SyncSender<ControlReply>,
}

impl PartialEq for ControlRequest {
    fn eq(&self, other: &Self) -> bool {
        self.command == other.command
    }
}
impl Eq for ControlRequest {}

impl ControlRequest {
    /// Build a request + the receiving half the listener thread blocks on.
    pub fn new(command: ControlCommand) -> (Self, Receiver<ControlReply>) {
        // A rendezvous (bound-1) channel: the UI thread's reply is delivered to
        // the waiting listener thread without unbounded buffering.
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        (Self { command, reply: tx }, rx)
    }

    /// Send the reply back to the waiting client listener. A dropped receiver
    /// (client disconnected) is ignored.
    pub fn respond(&self, reply: ControlReply) {
        let _ = self.reply.send(reply);
    }
}

/// Parse a request line (`@ <verb> [args…]`, the leading `@`/`msg` already
/// stripped by the caller, or present) into a [`ControlCommand`]. Returns an
/// error string suitable for an `ERR` reply.
pub fn parse_request(line: &str) -> Result<ControlCommand, String> {
    // Tolerate a leading `@` or `msg` token if the raw line still carries it.
    let line = line.trim();
    let line = line
        .strip_prefix('@')
        .map(str::trim_start)
        .or_else(|| line.strip_prefix("msg").map(str::trim_start))
        .unwrap_or(line);

    let (verb, rest) = match line.split_once(char::is_whitespace) {
        Some((v, r)) => (v, r.trim_start()),
        None => (line, ""),
    };
    match verb.to_ascii_lowercase().as_str() {
        "ls" | "list" => Ok(ControlCommand::Ls),
        "open-tab" | "new-tab" | "tab" => Ok(ControlCommand::OpenTab),
        "split" => SplitDir::parse(rest)
            .map(ControlCommand::Split)
            .ok_or_else(|| format!("split: expected vertical|horizontal, got '{rest}'")),
        "send-text" | "send" => {
            if rest.is_empty() {
                Err("send-text: missing text".to_string())
            } else {
                Ok(ControlCommand::SendText(unescape(rest)))
            }
        }
        "set-theme" | "theme" => {
            if rest.is_empty() {
                Err("set-theme: missing theme name".to_string())
            } else {
                Ok(ControlCommand::SetTheme(rest.trim().to_string()))
            }
        }
        "set-color" | "color" => {
            let mut it = rest.split_whitespace();
            match (it.next(), it.next()) {
                (Some(target), Some(hex)) => Ok(ControlCommand::SetColor {
                    target: target.to_ascii_lowercase(),
                    hex: hex.to_string(),
                }),
                _ => Err("set-color: usage 'set-color <fg|bg|cursor> <#rrggbb>'".to_string()),
            }
        }
        "focus-tab" | "goto-tab" => rest
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|&n| n >= 1)
            .map(ControlCommand::FocusTab)
            .ok_or_else(|| format!("focus-tab: expected a 1-based position, got '{rest}'")),
        "" => Err("missing command".to_string()),
        other => Err(format!("unknown command '{other}'")),
    }
}

/// C-style unescape for `send-text`: turns `\n`, `\t`, `\r`, `\\`, and `\e`
/// (ESC) into their byte equivalents. An unknown escape keeps the backslash.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('e') => out.push('\x1b'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ls_and_open_tab() {
        assert_eq!(parse_request("@ ls"), Ok(ControlCommand::Ls));
        assert_eq!(parse_request("ls"), Ok(ControlCommand::Ls));
        assert_eq!(parse_request("msg open-tab"), Ok(ControlCommand::OpenTab));
        assert_eq!(parse_request("new-tab"), Ok(ControlCommand::OpenTab));
    }

    #[test]
    fn parses_split_directions() {
        assert_eq!(
            parse_request("split"),
            Ok(ControlCommand::Split(SplitDir::Vertical))
        );
        assert_eq!(
            parse_request("@ split horizontal"),
            Ok(ControlCommand::Split(SplitDir::Horizontal))
        );
        assert_eq!(
            parse_request("split h"),
            Ok(ControlCommand::Split(SplitDir::Horizontal))
        );
        assert!(parse_request("split sideways").is_err());
    }

    #[test]
    fn send_text_takes_rest_and_unescapes() {
        assert_eq!(
            parse_request(r"send-text hello\nworld"),
            Ok(ControlCommand::SendText("hello\nworld".to_string()))
        );
        // Spaces are preserved verbatim.
        assert_eq!(
            parse_request("send echo  spaced"),
            Ok(ControlCommand::SendText("echo  spaced".to_string()))
        );
        assert!(parse_request("send-text").is_err());
    }

    #[test]
    fn parses_theme_and_color_and_focus() {
        assert_eq!(
            parse_request("set-theme tokyo-night"),
            Ok(ControlCommand::SetTheme("tokyo-night".to_string()))
        );
        assert_eq!(
            parse_request("set-color fg #ffffff"),
            Ok(ControlCommand::SetColor {
                target: "fg".to_string(),
                hex: "#ffffff".to_string()
            })
        );
        assert_eq!(
            parse_request("focus-tab 3"),
            Ok(ControlCommand::FocusTab(3))
        );
        assert!(parse_request("focus-tab 0").is_err());
        assert!(parse_request("focus-tab x").is_err());
    }

    #[test]
    fn unknown_and_empty_rejected() {
        assert!(parse_request("frobnicate").is_err());
        assert!(parse_request("").is_err());
        assert!(parse_request("@").is_err());
    }

    #[test]
    fn reply_wire_format() {
        assert_eq!(ControlReply::Ok(String::new()).to_wire(), "OK");
        assert_eq!(ControlReply::Ok("two tabs".into()).to_wire(), "OK two tabs");
        assert_eq!(ControlReply::Err("nope".into()).to_wire(), "ERR nope");
    }

    #[test]
    fn request_roundtrips_reply() {
        let (req, rx) = ControlRequest::new(ControlCommand::Ls);
        req.respond(ControlReply::Ok("hi".into()));
        assert_eq!(rx.recv().unwrap(), ControlReply::Ok("hi".into()));
    }

    #[test]
    fn unescape_handles_esc_and_unknown() {
        assert_eq!(unescape(r"a\eb"), "a\x1bb");
        assert_eq!(unescape(r"keep\xthis"), r"keep\xthis");
        assert_eq!(unescape(r"trailing\"), "trailing\\");
    }
}
