//! Rich desktop-notification specification + parsers for the OSC 9 / OSC 777
//! notification escape sequences.
//!
//! The legacy path forwarded only a flat message string. This module upgrades
//! that to a structured [`NotifySpec`] carrying a title, body, and the optional
//! presentation fields the FreeDesktop notification spec (and `notify-rust`)
//! support: an **icon** name/path, a **sound** name, an **urgency** level, and
//! one or more **actions** (button id + label). The UI thread (see
//! `app::helpers::fire_notification`) renders a `NotifySpec` through `notify-rust`.
//!
//! ## Wire formats understood
//!
//! Two notification OSCs are observed in the byte stream (the raw bytes are
//! always also passed through to the VT parser; this only side-channels them):
//!
//! - **OSC 9** — iTerm2 / ConEmu: `ESC ] 9 ; <body> ST`. A plain body. glassy
//!   additionally accepts a `key=value;…` *prefix* before the body so scripts
//!   can opt into rich fields without a new OSC, e.g.
//!   `9;title=Build;icon=dialog-information;sound=complete;urgency=critical;Done`.
//!   A body with no recognised `key=` prefix is treated verbatim (back-compat).
//! - **OSC 777** — terminal-notifier / kitty: `ESC ] 777 ; notify ; <title> ;
//!   <body> [; key=value …] ST`. The first two fields are title + body; any
//!   trailing `key=value` fields add icon/sound/urgency/action.
//!
//! Action fields use `action=<id>:<label>` and may repeat. Unknown keys are
//! ignored so the format stays forward-compatible.

/// Notification urgency, mirroring the FreeDesktop `urgency` hint
/// (0=low, 1=normal, 2=critical). `notify-rust` exposes the same three levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum NotifyUrgency {
    Low,
    #[default]
    Normal,
    Critical,
}

impl NotifyUrgency {
    /// Parse a `urgency=` value: a name (`low`/`normal`/`critical`) or the
    /// numeric FreeDesktop level (`0`/`1`/`2`). Returns `None` for anything else.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "low" | "0" => Some(Self::Low),
            "normal" | "1" => Some(Self::Normal),
            "critical" | "high" | "2" => Some(Self::Critical),
            _ => None,
        }
    }
}

/// A structured desktop-notification request, parsed from an OSC 9 / OSC 777
/// sequence (or synthesized internally, e.g. for the command-finish alert).
///
/// Cloneable + owned so it can ride inside `UserEvent` from the PTY thread to the
/// UI thread without borrowing the byte stream.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotifySpec {
    /// Summary line. Empty means the caller (UI) supplies a default (`glassy`).
    pub title: String,
    /// Body text (may be empty if the title carries the whole message).
    pub body: String,
    /// FreeDesktop icon name or absolute path (e.g. `dialog-information`). `None`
    /// leaves the daemon's default icon.
    pub icon: Option<String>,
    /// Sound name (XDG sound-naming-spec, e.g. `message-new-instant`,
    /// `complete`). `None` plays the daemon default (or silence).
    pub sound: Option<String>,
    /// Urgency level. Defaults to Normal.
    pub urgency: NotifyUrgency,
    /// Action buttons as `(id, label)` pairs. Empty for a plain notification.
    pub actions: Vec<(String, String)>,
}

impl NotifySpec {
    /// A plain notification with just a body (back-compat with the old flat path).
    pub fn message(body: impl Into<String>) -> Self {
        Self {
            body: body.into(),
            ..Self::default()
        }
    }

    /// Apply a single `key=value` rich field to this spec. Recognised keys:
    /// `title`, `body`, `icon`, `sound`, `urgency`, `action` (repeatable,
    /// `id:label`). Unknown keys are ignored. Returns whether the key was known.
    fn apply_field(&mut self, key: &str, value: &str) -> bool {
        match key.trim().to_ascii_lowercase().as_str() {
            "title" => {
                self.title = value.to_string();
                true
            }
            "body" => {
                self.body = value.to_string();
                true
            }
            "icon" => {
                if !value.is_empty() {
                    self.icon = Some(value.to_string());
                }
                true
            }
            "sound" => {
                if !value.is_empty() {
                    self.sound = Some(value.to_string());
                }
                true
            }
            "urgency" => {
                if let Some(u) = NotifyUrgency::parse(value) {
                    self.urgency = u;
                }
                true
            }
            "action" => {
                // `id:label`. A bare id (no colon) reuses the id as the label.
                if !value.is_empty() {
                    let (id, label) = value.split_once(':').unwrap_or((value, value));
                    if !id.is_empty() {
                        self.actions.push((id.to_string(), label.to_string()));
                    }
                }
                true
            }
            _ => false,
        }
    }

    /// The text shown by the in-app toast fallback: `title — body`, or whichever
    /// of the two is present.
    pub fn toast_text(&self) -> String {
        match (self.title.is_empty(), self.body.is_empty()) {
            (false, false) => format!("{} — {}", self.title, self.body),
            (false, true) => self.title.clone(),
            (true, _) => self.body.clone(),
        }
    }
}

/// Parse an OSC 9 notification body (`9;<rest>`) into a [`NotifySpec`].
///
/// `<rest>` may be a plain message (legacy) or a leading run of `key=value;`
/// rich fields followed by the body. A leading field run is only consumed while
/// every `;`-separated segment is a recognised `key=value`; the first segment
/// that is not becomes (the start of) the body — so a message that happens to
/// contain `=` is never mangled. Returns `None` if the body is empty.
pub fn parse_osc9(body: &[u8]) -> Option<NotifySpec> {
    let body = std::str::from_utf8(body).ok()?;
    let rest = body.strip_prefix("9;")?;
    if rest.is_empty() {
        return None;
    }
    Some(parse_fields_then_body(rest))
}

/// Parse an OSC 777 notification body (`777;notify;<title>;<body>[;key=value…]`)
/// into a [`NotifySpec`]. The first field after `notify;` is the title, the
/// second the body; any further `key=value` fields add rich attributes. Returns
/// `None` if the body is not an OSC 777 notify sequence or carries no content.
pub fn parse_osc777(body: &[u8]) -> Option<NotifySpec> {
    let body = std::str::from_utf8(body).ok()?;
    let rest = body.strip_prefix("777;notify;")?;
    let mut parts = rest.split(';');
    let title = parts.next().unwrap_or("").to_string();
    let mut spec = NotifySpec {
        title,
        ..NotifySpec::default()
    };
    // Second field is the body; but it may itself be a `key=value` (a title-only
    // notification with rich fields). Pull it, then treat the remainder as fields.
    if let Some(second) = parts.next() {
        if let Some((k, v)) = split_field(second) {
            if !spec.apply_field(k, v) {
                spec.body = second.to_string();
            }
        } else {
            spec.body = second.to_string();
        }
    }
    for seg in parts {
        if let Some((k, v)) = split_field(seg) {
            spec.apply_field(k, v);
        }
    }
    if spec.title.is_empty() && spec.body.is_empty() {
        return None;
    }
    Some(spec)
}

/// Split a `;`-delimited string into leading recognised `key=value` fields and a
/// trailing free-form body. Stops field consumption at the first segment that is
/// not a recognised `key=value`; everything from there on (rejoined with `;`) is
/// the body.
fn parse_fields_then_body(rest: &str) -> NotifySpec {
    let mut spec = NotifySpec::default();
    let mut segments: Vec<&str> = rest.split(';').collect();
    let mut consumed = 0;
    for seg in &segments {
        match split_field(seg) {
            Some((k, v)) if spec.apply_field(k, v) => consumed += 1,
            _ => break,
        }
    }
    // The rest (rejoined) is the body. If we consumed at least one field AND a
    // `body=`/`title=` already set text, leftover segments still append as body.
    if consumed < segments.len() {
        let body = segments.split_off(consumed).join(";");
        if !body.is_empty() {
            // A `body=`/`title=` field may have already set text; the trailing
            // free-form text wins as the body if present.
            spec.body = body;
        }
    }
    spec
}

/// Split a segment into `(key, value)` if it looks like `key=value` with a
/// non-empty key. Returns `None` for a segment with no `=` (a body fragment).
fn split_field(seg: &str) -> Option<(&str, &str)> {
    let (k, v) = seg.split_once('=')?;
    if k.is_empty() {
        return None;
    }
    Some((k, v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc9_plain_message_is_verbatim() {
        let s = parse_osc9(b"9;Build finished").unwrap();
        assert_eq!(s.body, "Build finished");
        assert!(s.title.is_empty());
        assert!(s.actions.is_empty());
    }

    #[test]
    fn osc9_message_with_equals_is_not_mangled() {
        // No leading key=value -> the whole thing is the body even with `=`.
        let s = parse_osc9(b"9;x=1 means one").unwrap();
        assert_eq!(s.body, "x=1 means one");
    }

    #[test]
    fn osc9_rich_prefix_fields() {
        let s = parse_osc9(
            b"9;title=Build;icon=dialog-information;sound=complete;urgency=critical;Done!",
        )
        .unwrap();
        assert_eq!(s.title, "Build");
        assert_eq!(s.body, "Done!");
        assert_eq!(s.icon.as_deref(), Some("dialog-information"));
        assert_eq!(s.sound.as_deref(), Some("complete"));
        assert_eq!(s.urgency, NotifyUrgency::Critical);
    }

    #[test]
    fn osc9_action_fields_repeat() {
        let s = parse_osc9(b"9;action=open:Open;action=dismiss:Dismiss;Ready").unwrap();
        assert_eq!(s.body, "Ready");
        assert_eq!(
            s.actions,
            vec![
                ("open".to_string(), "Open".to_string()),
                ("dismiss".to_string(), "Dismiss".to_string()),
            ]
        );
    }

    #[test]
    fn osc777_title_body() {
        let s = parse_osc777(b"777;notify;Title here;Body here").unwrap();
        assert_eq!(s.title, "Title here");
        assert_eq!(s.body, "Body here");
    }

    #[test]
    fn osc777_with_rich_fields() {
        let s = parse_osc777(b"777;notify;Job;Finished;icon=emblem-ok;urgency=low").unwrap();
        assert_eq!(s.title, "Job");
        assert_eq!(s.body, "Finished");
        assert_eq!(s.icon.as_deref(), Some("emblem-ok"));
        assert_eq!(s.urgency, NotifyUrgency::Low);
    }

    #[test]
    fn osc777_title_only_with_fields() {
        let s = parse_osc777(b"777;notify;Just a title;icon=face-smile").unwrap();
        assert_eq!(s.title, "Just a title");
        assert_eq!(s.body, "");
        assert_eq!(s.icon.as_deref(), Some("face-smile"));
    }

    #[test]
    fn osc9_empty_is_none() {
        assert!(parse_osc9(b"9;").is_none());
        assert!(parse_osc9(b"7;foo").is_none());
    }

    #[test]
    fn urgency_parses_names_and_numbers() {
        assert_eq!(NotifyUrgency::parse("low"), Some(NotifyUrgency::Low));
        assert_eq!(NotifyUrgency::parse("2"), Some(NotifyUrgency::Critical));
        assert_eq!(NotifyUrgency::parse("nope"), None);
    }

    #[test]
    fn toast_text_combines() {
        let mut s = NotifySpec::message("just body");
        assert_eq!(s.toast_text(), "just body");
        s.title = "T".into();
        assert_eq!(s.toast_text(), "T — just body");
    }
}
