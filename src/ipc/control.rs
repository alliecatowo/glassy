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
//! Plugin-system Phase 1 (see [`docs/plugins.md`](../../docs/plugins.md)) adds
//! five more verbs aimed at configuration and scripted reactions, in the same
//! line-oriented style:
//!
//! ```text
//! glassy @ list-themes              # OK tokyo-night, catppuccin-mocha, ...
//! glassy @ reload-config            # OK  — re-reads glassy.conf from disk
//! glassy @ run-action font_increase # OK  |  ERR unknown action '<name>'
//! glassy @ get-config opacity       # OK 0.92  |  ERR unknown key '<key>'
//! glassy @ set-config opacity 0.8   # OK  — writes glassy.conf + applies live
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
//! Args are space-separated; `send-text` and `set-config`'s value take the
//! *rest of the line* verbatim (after the fixed leading tokens — `send-text`
//! has none, `set-config` has the key), so both preserve embedded spaces.
//! `send-text` additionally C-style-unescapes `\n`, `\t`, `\\`. The protocol
//! is intentionally line-oriented + ASCII so it stays shell-pipe friendly and
//! forward-compatible.
//!
//! The running instance turns a parsed request into a [`UserEvent::Control`]
//! ([`crate::pty::UserEvent`]) carrying a one-shot reply channel; the UI thread
//! applies it (`App::apply_control`) and sends a [`ControlReply`] back, which the
//! listener writes to the client socket.

use std::sync::mpsc::{Receiver, SyncSender};

use crate::app::settings_save::SAVED_KEYS;

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
    /// List every theme name the registry resolves (built-ins + the user
    /// themes dir), comma-separated.
    ListThemes,
    /// Re-read `glassy.conf` from disk and apply the live-reloadable subset —
    /// the exact path the file-watcher's `UserEvent::ConfigReload` runs.
    ReloadConfig,
    /// Run a named command-palette action ([`crate::config::keymap::parse_action`])
    /// as if invoked from a keybinding or menu item.
    RunAction(String),
    /// Read a single `glassy.conf` key's current live value.
    GetConfig(String),
    /// Write a single `glassy.conf` key: validated, persisted to disk, and
    /// applied live via the same path as [`ControlCommand::ReloadConfig`].
    SetConfig { key: String, value: String },
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
        "list-themes" => Ok(ControlCommand::ListThemes),
        "reload-config" => Ok(ControlCommand::ReloadConfig),
        "run-action" => {
            let name = rest.trim();
            if name.is_empty() {
                Err("run-action: missing action name".to_string())
            } else {
                Ok(ControlCommand::RunAction(name.to_string()))
            }
        }
        "get-config" => {
            let key = rest.trim();
            if key.is_empty() {
                Err("get-config: missing key".to_string())
            } else {
                Ok(ControlCommand::GetConfig(key.to_ascii_lowercase()))
            }
        }
        "set-config" => match rest.split_once(char::is_whitespace) {
            Some((key, value)) if !key.trim().is_empty() => Ok(ControlCommand::SetConfig {
                key: key.trim().to_ascii_lowercase(),
                // Preserve the rest of the line verbatim (spaces included),
                // mirroring `send-text`'s "rest of the line is the payload"
                // handling — only the leading whitespace that separated the
                // key is trimmed.
                value: value.trim_start().to_string(),
            }),
            Some(_) => Err("set-config: missing key".to_string()),
            None if rest.is_empty() => {
                Err("set-config: usage 'set-config <key> <value>'".to_string())
            }
            None => Err(format!("set-config: missing value for key '{rest}'")),
        },
        "" => Err("missing command".to_string()),
        other => Err(format!(
            "unknown command '{other}' (try: ls, open-tab, split, send-text, set-theme, \
             set-color, focus-tab, list-themes, reload-config, run-action, get-config, \
             set-config)"
        )),
    }
}

/// Whether `key` is a `glassy.conf` key the `get-config`/`set-config`
/// remote-control verbs can read or write: any key in the declarative
/// [`SAVED_KEYS`](crate::app::settings_save::SAVED_KEYS) table (the same one
/// `App::save_settings`/the settings overlay's Save button writes), or the
/// special-cased `font_size` (its live value lives in the renderer's
/// effective px, not `Config::font_size` — see that table's doc comment).
pub(crate) fn is_known_config_key(key: &str) -> bool {
    key == "font_size" || SAVED_KEYS.iter().any(|k| k.key == key)
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
    fn unknown_verb_error_mentions_new_verbs() {
        // The unknown-command ERR text doubles as a usage hint (there is no
        // dedicated `help` verb), so it must mention the Phase 1 additions
        // alongside the pre-existing verbs.
        let err = parse_request("frobnicate").unwrap_err();
        for verb in [
            "list-themes",
            "reload-config",
            "run-action",
            "get-config",
            "set-config",
        ] {
            assert!(err.contains(verb), "error '{err}' should mention '{verb}'");
        }
    }

    #[test]
    fn parses_list_themes_and_reload_config() {
        assert_eq!(
            parse_request("@ list-themes"),
            Ok(ControlCommand::ListThemes)
        );
        assert_eq!(parse_request("list-themes"), Ok(ControlCommand::ListThemes));
        assert_eq!(
            parse_request("@ reload-config"),
            Ok(ControlCommand::ReloadConfig)
        );
        assert_eq!(
            parse_request("reload-config"),
            Ok(ControlCommand::ReloadConfig)
        );
    }

    #[test]
    fn parses_run_action() {
        assert_eq!(
            parse_request("run-action font_increase"),
            Ok(ControlCommand::RunAction("font_increase".to_string()))
        );
        assert_eq!(
            parse_request("@ run-action new_tab"),
            Ok(ControlCommand::RunAction("new_tab".to_string()))
        );
        // Missing name is a parse-time error (unlike the actual unknown-action
        // check, which happens later on the UI thread against the live
        // keymap action table).
        assert!(parse_request("run-action").is_err());
        assert!(parse_request("run-action   ").is_err());
    }

    #[test]
    fn parses_get_config() {
        assert_eq!(
            parse_request("get-config opacity"),
            Ok(ControlCommand::GetConfig("opacity".to_string()))
        );
        assert_eq!(
            parse_request("@ get-config FONT_SIZE"),
            Ok(ControlCommand::GetConfig("font_size".to_string()))
        );
        assert!(parse_request("get-config").is_err());
        assert!(parse_request("get-config   ").is_err());
    }

    #[test]
    fn parses_set_config_single_word_value() {
        assert_eq!(
            parse_request("set-config opacity 0.9"),
            Ok(ControlCommand::SetConfig {
                key: "opacity".to_string(),
                value: "0.9".to_string(),
            })
        );
        assert_eq!(
            parse_request("@ set-config THEME tokyo-night"),
            Ok(ControlCommand::SetConfig {
                key: "theme".to_string(),
                value: "tokyo-night".to_string(),
            })
        );
    }

    #[test]
    fn parses_set_config_multi_word_value_preserves_spaces() {
        // The shell already stripped any quoting by the time this reaches the
        // wire (see `main.rs`'s `rest.join(" ")`); the value is simply
        // "everything after the key", spaces included — mirrors send-text.
        assert_eq!(
            parse_request("set-config font_features calt=0 liga"),
            Ok(ControlCommand::SetConfig {
                key: "font_features".to_string(),
                value: "calt=0 liga".to_string(),
            })
        );
        assert_eq!(
            parse_request("set-config word_separator a  b"),
            Ok(ControlCommand::SetConfig {
                key: "word_separator".to_string(),
                value: "a  b".to_string(), // internal double space preserved
            })
        );
    }

    #[test]
    fn set_config_missing_key_or_value_rejected() {
        assert!(parse_request("set-config").is_err());
        assert!(parse_request("set-config   ").is_err());
        // A key with no value token at all is a usage error, not an implicit
        // empty value (the wire can't represent a trailing empty value: both
        // client and server trim the line, see `main.rs`/`ipc::mod`).
        assert!(parse_request("set-config theme").is_err());
    }

    #[test]
    fn is_known_config_key_covers_saved_keys_and_font_size() {
        assert!(is_known_config_key("font_size"));
        assert!(is_known_config_key("opacity"));
        assert!(is_known_config_key("theme"));
        assert!(is_known_config_key("font_features"));
        assert!(!is_known_config_key("not_a_real_key"));
        assert!(!is_known_config_key(""));
        // Every SAVED_KEYS entry must be recognized (guards the helper from
        // drifting out of sync with the table it reads).
        for sk in SAVED_KEYS {
            assert!(is_known_config_key(sk.key), "missing key '{}'", sk.key);
        }
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
