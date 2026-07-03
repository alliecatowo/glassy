//! Remote-control command application.
//!
//! The IPC listener thread parses `glassy @ <cmd>` / `glassy msg …` requests
//! (see [`crate::ipc::control`]) and forwards them as
//! [`UserEvent::Control`](crate::pty::UserEvent::Control) carrying a one-shot
//! reply channel. This module is the UI-thread side: [`App::apply_control`]
//! mutates the running window (open a tab, split, type text, switch theme, …)
//! and sends a [`ControlReply`] back to the waiting client.

use super::*;
use crate::ipc::control::{ControlCommand, ControlReply, ControlRequest, SplitDir};

impl App {
    /// Apply a remote-control request on the UI thread and reply to the client.
    ///
    /// Each command maps onto an existing user-facing action so remote control
    /// and the keyboard/menus stay behaviourally identical. The reply is a short
    /// human-readable string (e.g. the `ls` listing, or "opened tab"). Always
    /// responds exactly once, even on the error paths, so the client never hangs.
    pub(super) fn apply_control(
        &mut self,
        req: &ControlRequest,
        event_loop: &ActiveEventLoop,
    ) -> bool {
        // Whether the window needs a repaint after this command.
        let mut dirty = true;
        let reply = match &req.command {
            ControlCommand::Ls => {
                dirty = false; // a read-only query changes nothing on screen
                ControlReply::Ok(self.control_ls())
            }
            ControlCommand::OpenTab => {
                self.new_tab(event_loop);
                ControlReply::Ok(format!("opened tab {}", self.tab_count()))
            }
            ControlCommand::Split(dir) => {
                if self.pty.is_none() {
                    ControlReply::Err("no active pane to split".to_string())
                } else {
                    let d = match dir {
                        SplitDir::Vertical => pane::Dir::Vertical,
                        SplitDir::Horizontal => pane::Dir::Horizontal,
                    };
                    self.split_pane(d, event_loop);
                    ControlReply::Ok("split pane".to_string())
                }
            }
            ControlCommand::SendText(text) => {
                if self.pty.is_none() {
                    ControlReply::Err("no active pane".to_string())
                } else {
                    // Route through write_input so broadcast-to-all-panes is
                    // honoured, exactly like a real keystroke / paste.
                    self.write_input(text.clone().into_bytes());
                    dirty = false; // the PTY echo wakes us; nothing to paint yet
                    ControlReply::Ok(format!("sent {} bytes", text.len()))
                }
            }
            ControlCommand::SetTheme(name) => self.control_set_theme(name),
            ControlCommand::SetColor { target, hex } => self.control_set_color(target, hex),
            ControlCommand::FocusTab(pos1) => {
                // 1-based from the client; tab_order is 0-based.
                let pos = pos1 - 1;
                if pos >= self.tab_order.len() {
                    ControlReply::Err(format!("no tab {pos1} (have {})", self.tab_order.len()))
                } else {
                    self.activate_tab(pos, event_loop);
                    ControlReply::Ok(format!("focused tab {pos1}"))
                }
            }
        };
        req.respond(reply);
        dirty
    }

    /// Build the `ls` listing: one line per tab with its panes. The active tab is
    /// marked with `*`; each pane shows its id, focus marker, and title.
    fn control_ls(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("window: {} tab(s)\n", self.tab_count()));
        let active_pos = self.active_pos();
        for (pos, &tab_id) in self.tab_order.iter().enumerate() {
            let active = pos == active_pos;
            let marker = if active { "*" } else { " " };
            let title = self
                .tab_descs()
                .get(pos)
                .map(|(t, _, _)| t.clone())
                .unwrap_or_default();
            let title = if title.trim().is_empty() {
                "shell".to_string()
            } else {
                title
            };
            out.push_str(&format!("{marker} tab {} [{title}]\n", pos + 1));
            // Pane breakdown for this tab.
            let panes = self.control_panes_for(tab_id, active);
            for (pane_id, focused, ptitle) in panes {
                let fm = if focused { ">" } else { " " };
                out.push_str(&format!("    {fm} pane {pane_id} [{ptitle}]\n"));
            }
        }
        // Replace inner newlines with a marker the line-oriented wire keeps: the
        // client prints it verbatim, so embed real newlines via the OK payload.
        out.trim_end().to_string()
    }

    /// `(pane_id, is_focused, title)` for every pane of the tab whose stable id is
    /// `tab_id`. `active` selects the live (`self`) vs a parked session.
    fn control_panes_for(&self, tab_id: usize, active: bool) -> Vec<(usize, bool, String)> {
        let mut out = Vec::new();
        if active {
            let focused = self.active_focused_id();
            match self.panes.as_ref() {
                Some(g) => {
                    for id in g.layout.leaves() {
                        let title = g
                            .others_titles
                            .get(&id)
                            .cloned()
                            .filter(|t| !t.trim().is_empty())
                            .unwrap_or_else(|| "shell".to_string());
                        out.push((id, id == focused, title));
                    }
                }
                None => {
                    let title = if self.active_title.trim().is_empty() {
                        "shell".to_string()
                    } else {
                        self.active_title.clone()
                    };
                    out.push((focused, true, title));
                }
            }
        } else if let Some(s) = self.background.iter().find(|s| s.id == tab_id) {
            match s.panes.as_ref() {
                Some(g) => {
                    let focused = g.layout.focused();
                    for id in g.layout.leaves() {
                        let title = g
                            .others_titles
                            .get(&id)
                            .cloned()
                            .filter(|t| !t.trim().is_empty())
                            .unwrap_or_else(|| "shell".to_string());
                        out.push((id, id == focused, title));
                    }
                }
                None => {
                    let title = if s.title.trim().is_empty() {
                        "shell".to_string()
                    } else {
                        s.title.clone()
                    };
                    out.push((s.id, true, title));
                }
            }
        }
        out
    }

    /// Apply a `set-theme` request: validate the name against the known themes
    /// (via the canonical-name resolver) and install it live.
    fn control_set_theme(&mut self, name: &str) -> ControlReply {
        // `canonical_name` maps anything unknown to "tokyo-night", so a literal
        // tokyo-night request must additionally pass the strict name check to
        // avoid silently accepting arbitrary junk.
        let canon = crate::color::canonical_name(name);
        let accepted = canon != "tokyo-night" || name_matches_tokyo(name);
        let theme_names = crate::color::theme_names();
        if !accepted {
            return ControlReply::Err(format!(
                "unknown theme '{name}' (try one of: {})",
                theme_names.join(", ")
            ));
        }
        match theme_names.iter().position(|&n| n == canon) {
            Some(idx) => {
                self.set_theme_by_idx(idx);
                ControlReply::Ok(format!("theme set to {canon}"))
            }
            None => ControlReply::Err(format!("unknown theme '{name}'")),
        }
    }

    /// Apply a `set-color` request. The renderer reads palette colors from the
    /// active theme each frame, so a live per-component override is not wired as a
    /// standalone runtime path yet; report that clearly rather than silently
    /// no-op. `set-theme` is the supported live color path today.
    fn control_set_color(&mut self, target: &str, hex: &str) -> ControlReply {
        // Validate the hex so a future wire-up gets clean input + the client gets
        // a useful error now.
        if !is_valid_hex_color(hex) {
            return ControlReply::Err(format!("invalid color '{hex}' (use #rrggbb)"));
        }
        match target {
            "fg" | "bg" | "cursor" => ControlReply::Err(format!(
                "set-color {target} not yet live; use 'set-theme <name>' to change colors"
            )),
            other => ControlReply::Err(format!("unknown color target '{other}' (fg|bg|cursor)")),
        }
    }
}

/// Whether `s` is a `#rgb` / `#rrggbb` hex color (with the leading `#`). Used to
/// validate `set-color` input without depending on the `config` crate internals.
fn is_valid_hex_color(s: &str) -> bool {
    let Some(hex) = s.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Whether the literal input names Tokyo Night (so we don't accept arbitrary
/// junk that `canonical_name` maps to the tokyo-night default).
fn name_matches_tokyo(name: &str) -> bool {
    let key: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    matches!(key.as_str(), "tokyonight" | "tokyo" | "night")
}

#[cfg(test)]
mod tests {
    use super::{is_valid_hex_color, name_matches_tokyo};

    #[test]
    fn tokyo_name_match_is_strict() {
        assert!(name_matches_tokyo("tokyo-night"));
        assert!(name_matches_tokyo("TokyoNight"));
        assert!(name_matches_tokyo("tokyo"));
        assert!(!name_matches_tokyo("gibberish"));
        assert!(!name_matches_tokyo("nord"));
    }

    #[test]
    fn hex_color_validation() {
        assert!(is_valid_hex_color("#fff"));
        assert!(is_valid_hex_color("#ffffff"));
        assert!(is_valid_hex_color("#0A1b2C"));
        assert!(!is_valid_hex_color("ffffff")); // missing #
        assert!(!is_valid_hex_color("#ffff")); // wrong length
        assert!(!is_valid_hex_color("#gggggg")); // non-hex
    }
}
