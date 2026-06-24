//! Session persistence: serialize the open tabs (each tab's pane tree + per-pane
//! cwd + custom title) to `$XDG_STATE_HOME/glassy/session.json` on exit/change, and
//! reload it on launch (behind the `restore_session` opt). Hand-rolled JSON keeps
//! glassy dependency-light — no serde — and the format is small and stable.
//!
//! The data model is deliberately decoupled from `App`: it carries only the layout
//! descriptors, cwds, and titles needed to rebuild the tabs, so it stays unit-
//! testable and never reaches into renderer/pty internals.

use std::path::PathBuf;

use crate::pane::{Dir, LayoutDesc, NodeDesc};

/// One persisted pane: its session-relative leaf id (matching the ids in the
/// tab's [`LayoutDesc`]) plus the working directory its shell should reopen in.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneState {
    pub id: usize,
    pub cwd: Option<String>,
}

/// One persisted tab: the split layout (a single-pane tab is a one-leaf tree), the
/// per-pane cwds, and an optional user-assigned custom title.
#[derive(Clone, Debug, PartialEq)]
pub struct TabState {
    pub layout: LayoutDesc,
    pub panes: Vec<PaneState>,
    /// A custom (renamed) title that overrides the OSC title, if the user set one.
    pub custom_title: Option<String>,
}

/// The full persisted session: the tabs in display order and which one was active.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    pub tabs: Vec<TabState>,
    pub active: usize,
}

/// Resolved state-file path: `$XDG_STATE_HOME/glassy/session.json`, falling back to
/// `~/.local/state/glassy/session.json`. `None` when neither env var is set.
pub fn path() -> Option<PathBuf> {
    if let Some(state) = std::env::var_os("XDG_STATE_HOME")
        && !state.is_empty()
    {
        return Some(PathBuf::from(state).join("glassy/session.json"));
    }
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/state/glassy/session.json"))
}

impl Session {
    /// Write the session to the state file (creating the directory). Best-effort:
    /// errors are logged, not propagated, since persistence must never block exit.
    pub fn save(&self) {
        let Some(path) = path() else {
            return;
        };
        if let Some(dir) = path.parent()
            && let Err(e) = std::fs::create_dir_all(dir)
        {
            log::warn!("session: could not create {}: {e}", dir.display());
            return;
        }
        let json = self.to_json();
        if let Err(e) = std::fs::write(&path, json) {
            log::warn!("session: could not write {}: {e}", path.display());
        }
    }

    /// Load the session from the state file, or `None` if it is missing/unreadable/
    /// malformed. A parse failure is logged and treated as "no session".
    pub fn load() -> Option<Session> {
        let path = path()?;
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => {
                log::warn!("session: could not read {}: {e}", path.display());
                return None;
            }
        };
        match parse(&text) {
            Ok(s) if !s.tabs.is_empty() => Some(s),
            Ok(_) => None,
            Err(e) => {
                log::warn!("session: ignoring malformed {}: {e}", path.display());
                None
            }
        }
    }

    /// Delete the state file (called on a clean exit when restore is off, so a
    /// stale session never resurrects). Best-effort; missing file is fine.
    pub fn clear() {
        if let Some(path) = path() {
            let _ = std::fs::remove_file(path);
        }
    }

    // --- JSON serialization (hand-rolled, no serde) ----------------------

    fn to_json(&self) -> String {
        let mut s = String::new();
        s.push_str("{\"version\":1,\"active\":");
        s.push_str(&self.active.to_string());
        s.push_str(",\"tabs\":[");
        for (i, tab) in self.tabs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            tab.write_json(&mut s);
        }
        s.push_str("]}");
        s
    }
}

impl TabState {
    fn write_json(&self, s: &mut String) {
        s.push_str("{\"layout\":");
        write_node(&self.layout.root, s);
        s.push_str(",\"focused\":");
        s.push_str(&self.layout.focused.to_string());
        s.push_str(",\"panes\":[");
        for (i, p) in self.panes.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            s.push_str("{\"id\":");
            s.push_str(&p.id.to_string());
            if let Some(cwd) = &p.cwd {
                s.push_str(",\"cwd\":");
                write_str(cwd, s);
            }
            s.push('}');
        }
        s.push(']');
        if let Some(t) = &self.custom_title {
            s.push_str(",\"title\":");
            write_str(t, s);
        }
        s.push('}');
    }
}

fn write_node(node: &NodeDesc, s: &mut String) {
    match node {
        NodeDesc::Leaf(id) => {
            s.push_str("{\"leaf\":");
            s.push_str(&id.to_string());
            s.push('}');
        }
        NodeDesc::Split { dir, ratio, first, second } => {
            s.push_str("{\"split\":");
            s.push_str(match dir {
                Dir::Vertical => "\"v\"",
                Dir::Horizontal => "\"h\"",
            });
            s.push_str(",\"ratio\":");
            // Finite, reasonable precision; clamp guards against NaN poisoning.
            let r = if ratio.is_finite() { ratio.clamp(0.0, 1.0) } else { 0.5 };
            s.push_str(&format!("{r:.4}"));
            s.push_str(",\"first\":");
            write_node(first, s);
            s.push_str(",\"second\":");
            write_node(second, s);
            s.push('}');
        }
    }
}

fn write_str(v: &str, s: &mut String) {
    s.push('"');
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\r' => s.push_str("\\r"),
            '\t' => s.push_str("\\t"),
            c if (c as u32) < 0x20 => s.push_str(&format!("\\u{:04x}", c as u32)),
            c => s.push(c),
        }
    }
    s.push('"');
}

// --- JSON parsing (minimal recursive-descent over the subset we emit) -------

fn parse(text: &str) -> Result<Session, String> {
    let mut p = Parser { b: text.as_bytes(), i: 0 };
    p.ws();
    let v = p.value()?;
    let obj = v.as_object()?;
    let active = field(obj, "active").and_then(Json::as_usize).unwrap_or(0);
    let tabs_v = field(obj, "tabs").ok_or("missing tabs")?;
    let arr = tabs_v.as_array()?;
    let mut tabs = Vec::new();
    for t in arr {
        tabs.push(parse_tab(t)?);
    }
    Ok(Session { tabs, active })
}

fn parse_tab(v: &Json) -> Result<TabState, String> {
    let obj = v.as_object()?;
    let root = parse_node(field(obj, "layout").ok_or("tab missing layout")?)?;
    let focused = field(obj, "focused").and_then(Json::as_usize).unwrap_or(0);
    let mut panes = Vec::new();
    if let Some(pv) = field(obj, "panes") {
        for p in pv.as_array()? {
            let po = p.as_object()?;
            let id = field(po, "id").and_then(Json::as_usize).ok_or("pane missing id")?;
            let cwd = field(po, "cwd").and_then(Json::as_string);
            panes.push(PaneState { id, cwd });
        }
    }
    let custom_title = field(obj, "title").and_then(Json::as_string);
    Ok(TabState {
        layout: LayoutDesc { root, focused },
        panes,
        custom_title,
    })
}

fn parse_node(v: &Json) -> Result<NodeDesc, String> {
    let obj = v.as_object()?;
    if let Some(leaf) = field(obj, "leaf").and_then(Json::as_usize) {
        return Ok(NodeDesc::Leaf(leaf));
    }
    let dir = match field(obj, "split").and_then(Json::as_str) {
        Some("v") => Dir::Vertical,
        Some("h") => Dir::Horizontal,
        _ => return Err("node missing leaf/split".into()),
    };
    let ratio = field(obj, "ratio").and_then(Json::as_f32).unwrap_or(0.5);
    let first = parse_node(field(obj, "first").ok_or("split missing first")?)?;
    let second = parse_node(field(obj, "second").ok_or("split missing second")?)?;
    Ok(NodeDesc::Split {
        dir,
        ratio,
        first: Box::new(first),
        second: Box::new(second),
    })
}

/// A tiny JSON value tree, just enough to decode what `to_json` emits.
enum Json {
    Null,
    Bool,
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn as_object(&self) -> Result<&[(String, Json)], String> {
        match self {
            Json::Obj(o) => Ok(o),
            _ => Err("expected object".into()),
        }
    }
    fn as_array(&self) -> Result<&[Json], String> {
        match self {
            Json::Arr(a) => Ok(a),
            _ => Err("expected array".into()),
        }
    }
    fn as_usize(&self) -> Option<usize> {
        match self {
            Json::Num(n) if *n >= 0.0 && n.is_finite() => Some(*n as usize),
            _ => None,
        }
    }
    fn as_f32(&self) -> Option<f32> {
        match self {
            Json::Num(n) => Some(*n as f32),
            _ => None,
        }
    }
    fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }
    fn as_string(&self) -> Option<String> {
        self.as_str().map(str::to_string)
    }
}

/// Look up an object field by key (named `field` so it never collides with the
/// slice's built-in `get`, which resolves first and only indexes by usize/range).
fn field<'a>(obj: &'a [(String, Json)], key: &str) -> Option<&'a Json> {
    obj.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        match self.peek() {
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') | Some(b'f') => self.boolean(),
            Some(b'n') => self.null(),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            _ => Err(format!("unexpected byte at {}", self.i)),
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.i += 1; // {
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(out));
        }
        loop {
            self.ws();
            let key = self.string()?;
            self.ws();
            if self.peek() != Some(b':') {
                return Err(format!("expected ':' at {}", self.i));
            }
            self.i += 1;
            let val = self.value()?;
            out.push((key, val));
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    break;
                }
                _ => return Err(format!("expected ',' or '}}' at {}", self.i)),
            }
        }
        Ok(Json::Obj(out))
    }

    fn array(&mut self) -> Result<Json, String> {
        self.i += 1; // [
        let mut out = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(out));
        }
        loop {
            let val = self.value()?;
            out.push(val);
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b']') => {
                    self.i += 1;
                    break;
                }
                _ => return Err(format!("expected ',' or ']' at {}", self.i)),
            }
        }
        Ok(Json::Arr(out))
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some(b'"') {
            return Err(format!("expected string at {}", self.i));
        }
        self.i += 1;
        let mut out = String::new();
        while let Some(c) = self.peek() {
            self.i += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let e = self.peek().ok_or("unterminated escape")?;
                    self.i += 1;
                    match e {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hex = self
                                .b
                                .get(self.i..self.i + 4)
                                .ok_or("bad \\u escape")?;
                            let code = u32::from_str_radix(
                                std::str::from_utf8(hex).map_err(|_| "bad \\u hex")?,
                                16,
                            )
                            .map_err(|_| "bad \\u hex")?;
                            self.i += 4;
                            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
                        }
                        _ => return Err("bad escape".into()),
                    }
                }
                // Multi-byte UTF-8: copy the continuation bytes verbatim.
                _ => {
                    let start = self.i - 1;
                    let len = utf8_len(c);
                    let end = start + len;
                    let slice = self.b.get(start..end).ok_or("bad utf-8")?;
                    out.push_str(std::str::from_utf8(slice).map_err(|_| "bad utf-8")?);
                    self.i = end;
                }
            }
        }
        Err("unterminated string".into())
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || c == b'.' || c == b'e' || c == b'E' || c == b'+' || c == b'-' {
                self.i += 1;
            } else {
                break;
            }
        }
        let s = std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "bad number")?;
        s.parse::<f64>().map(Json::Num).map_err(|_| "bad number".into())
    }

    fn boolean(&mut self) -> Result<Json, String> {
        if self.b[self.i..].starts_with(b"true") {
            self.i += 4;
            Ok(Json::Bool)
        } else if self.b[self.i..].starts_with(b"false") {
            self.i += 5;
            Ok(Json::Bool)
        } else {
            Err("bad bool".into())
        }
    }

    fn null(&mut self) -> Result<Json, String> {
        if self.b[self.i..].starts_with(b"null") {
            self.i += 4;
            Ok(Json::Null)
        } else {
            Err("bad null".into())
        }
    }
}

/// Byte length of the UTF-8 sequence whose lead byte is `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Session {
        Session {
            active: 1,
            tabs: vec![
                TabState {
                    layout: LayoutDesc {
                        root: NodeDesc::Leaf(0),
                        focused: 0,
                    },
                    panes: vec![PaneState { id: 0, cwd: Some("/home/me".into()) }],
                    custom_title: Some("build".into()),
                },
                TabState {
                    layout: LayoutDesc {
                        root: NodeDesc::Split {
                            dir: Dir::Vertical,
                            ratio: 0.5,
                            first: Box::new(NodeDesc::Leaf(0)),
                            second: Box::new(NodeDesc::Leaf(1)),
                        },
                        focused: 1,
                    },
                    panes: vec![
                        PaneState { id: 0, cwd: Some("/tmp".into()) },
                        PaneState { id: 1, cwd: None },
                    ],
                    custom_title: None,
                },
            ],
        }
    }

    #[test]
    fn round_trips_through_json() {
        let s = sample();
        let json = s.to_json();
        let back = parse(&json).expect("parse");
        assert_eq!(s, back);
    }

    #[test]
    fn escapes_special_chars_in_titles() {
        let s = Session {
            active: 0,
            tabs: vec![TabState {
                layout: LayoutDesc { root: NodeDesc::Leaf(0), focused: 0 },
                panes: vec![PaneState { id: 0, cwd: Some("a\"b\\c\nd".into()) }],
                custom_title: Some("tab \"one\"".into()),
            }],
        };
        let json = s.to_json();
        let back = parse(&json).expect("parse");
        assert_eq!(s, back);
    }

    #[test]
    fn parses_unicode_titles() {
        let s = Session {
            active: 0,
            tabs: vec![TabState {
                layout: LayoutDesc { root: NodeDesc::Leaf(0), focused: 0 },
                panes: vec![PaneState { id: 0, cwd: None }],
                custom_title: Some("日本語 ✻".into()),
            }],
        };
        let back = parse(&s.to_json()).expect("parse");
        assert_eq!(s, back);
    }

    #[test]
    fn malformed_json_is_an_error() {
        assert!(parse("{not json").is_err());
        assert!(parse("{\"tabs\":").is_err());
    }

    #[test]
    fn nested_layout_round_trips() {
        let root = NodeDesc::Split {
            dir: Dir::Vertical,
            ratio: 0.3,
            first: Box::new(NodeDesc::Leaf(0)),
            second: Box::new(NodeDesc::Split {
                dir: Dir::Horizontal,
                ratio: 0.7,
                first: Box::new(NodeDesc::Leaf(1)),
                second: Box::new(NodeDesc::Leaf(2)),
            }),
        };
        let s = Session {
            active: 0,
            tabs: vec![TabState {
                layout: LayoutDesc { root: root.clone(), focused: 2 },
                panes: vec![
                    PaneState { id: 0, cwd: None },
                    PaneState { id: 1, cwd: None },
                    PaneState { id: 2, cwd: None },
                ],
                custom_title: None,
            }],
        };
        assert_eq!(s.layout_leaves(0), vec![0, 1, 2]);
        let back = parse(&s.to_json()).expect("parse");
        assert_eq!(s, back);
    }
}

impl Session {
    /// Test helper: the leaf ids of tab `i`'s layout in DFS order.
    #[cfg(test)]
    fn layout_leaves(&self, i: usize) -> Vec<usize> {
        self.tabs[i].layout.leaves()
    }
}
