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
            ControlCommand::ListThemes => {
                dirty = false; // a read-only query changes nothing on screen
                ControlReply::Ok(crate::color::theme_names().join(", "))
            }
            ControlCommand::ReloadConfig => match self.reload_config_from_disk() {
                // apply_config_reload flags self.dirty/force_full_redraw for
                // whatever actually changed but (unlike run_key_action) has no
                // ActiveEventLoop to reschedule a repaint with; leaving `dirty`
                // at its default `true` here is what gets that redraw
                // actually scheduled (via the outer `app.mark_dirty` call in
                // `user_event.rs`'s `UserEvent::Control` arm).
                Ok(()) => ControlReply::Ok(String::new()),
                Err(e) => {
                    dirty = false; // nothing changed; no repaint needed
                    ControlReply::Err(e)
                }
            },
            ControlCommand::RunAction(name) => {
                // Dispatch through the exact same `run_key_action` path a
                // keybinding or the macOS menu bar uses (see `mac_menu.rs`'s
                // module doc for the sibling case of that path). Unlike
                // `MenuAction`, which fires from an Objective-C callback with
                // no `ActiveEventLoop` in scope (hence its proxy round-trip
                // through `UserEvent::MenuAction`), `apply_control` already
                // runs on the UI thread WITH an `&ActiveEventLoop` (see this
                // method's signature), so no indirection is needed here.
                dirty = false; // run_key_action marks dirty itself when needed
                match crate::config::keymap::parse_action(name) {
                    Ok(Some(action)) => {
                        self.run_key_action(action, event_loop);
                        ControlReply::Ok(String::new())
                    }
                    Ok(None) => ControlReply::Err(format!(
                        "unknown action '{name}' (that name disables a bind, not something to run)"
                    )),
                    Err(_) => ControlReply::Err(format!("unknown action '{name}'")),
                }
            }
            ControlCommand::GetConfig(key) => {
                dirty = false;
                self.control_get_config(key)
            }
            ControlCommand::SetConfig { key, value } => {
                let reply = self.control_set_config(key, value);
                // Only a successful write reaches the reload_config_from_disk
                // call (see control_set_config), which needs the same
                // ActiveEventLoop-less redraw scheduling as ReloadConfig
                // above; an Err means nothing changed, so skip the repaint.
                if matches!(reply, ControlReply::Err(_)) {
                    dirty = false;
                }
                reply
            }
            ControlCommand::SetSegment { id, text } => {
                let reply = self.control_set_segment(id, text);
                if matches!(reply, ControlReply::Err(_)) {
                    dirty = false;
                }
                reply
            }
            ControlCommand::ClearSegment(id) => self.control_clear_segment(id),
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

    /// Apply a `get-config <key>` request: look `key` up in the declarative
    /// `settings_save::SAVED_KEYS` table (the same one the settings overlay's
    /// Save button and `glassy @ set-config` write through), with the
    /// `font_size` special case ([`Self::live_font_size_pt`]) `SAVED_KEYS`
    /// itself excludes for the reason documented on that table.
    fn control_get_config(&self, key: &str) -> ControlReply {
        if key == "font_size" {
            return ControlReply::Ok(format!("{:.0}", self.live_font_size_pt()));
        }
        match settings_save::SAVED_KEYS.iter().find(|sk| sk.key == key) {
            Some(sk) => ControlReply::Ok((sk.get)(&self.config)),
            None => ControlReply::Err(format!("unknown key '{key}'")),
        }
    }

    /// Apply a `set-config <key> <value>` request.
    ///
    /// Simplified write-through design (no per-key live setter to keep in
    /// sync): validate the key is one `get-config` can also read
    /// (`ipc::control::is_known_config_key`) and that the value parses at all
    /// (`config::validate_kv`, a dry run through the same parser
    /// `glassy.conf` loading uses), persist it via `config::save` — the exact
    /// mechanism the settings overlay's Save button uses — and apply it live
    /// through the identical path `reload-config` uses
    /// ([`Self::reload_config_from_disk`]). This means `set-config` always
    /// writes to `glassy.conf` on success (unlike a hypothetical
    /// apply-without-persisting verb), so the value survives a restart.
    ///
    /// A value that parses but is out of a field's valid range is persisted
    /// verbatim and silently clamped downstream in `RawConfig::into_settings`
    /// rather than rejected here — e.g. `set-config opacity 5` writes
    /// `opacity = 5` to disk while the *effective* value (live now, and again
    /// on every future load of that file) is `1.00`. `ERR` is reserved for an
    /// unknown key or a value that fails to parse at all (e.g. a non-numeric
    /// `opacity`).
    fn control_set_config(&mut self, key: &str, value: &str) -> ControlReply {
        if !crate::ipc::control::is_known_config_key(key) {
            return ControlReply::Err(format!("unknown key '{key}'"));
        }
        // apply_kv's errors already name the offending field (e.g. "opacity:
        // invalid number 'abc'"), so don't prefix the key again here.
        if let Err(e) = crate::config::validate_kv(key, value) {
            return ControlReply::Err(format!("{e:#}"));
        }
        if let Err(e) = crate::config::save(&[(key, value.to_string())]) {
            return ControlReply::Err(format!("set-config: failed to save: {e:#}"));
        }
        match self.reload_config_from_disk() {
            Ok(()) => ControlReply::Ok(String::new()),
            Err(e) => ControlReply::Err(format!("set-config: saved but reload failed: {e}")),
        }
    }

    /// Apply a `set-segment <id> <text...>` request (Phase 1 plugin surface,
    /// see `docs/plugins.md`): push/update `id`'s text in `self.custom_segments`
    /// via the bounded, unit-tested [`upsert_custom_segment`] (mod.rs). Shows
    /// wherever `custom` appears in `status_bar_segments`, or appended at the
    /// end of the left side otherwise.
    fn control_set_segment(&mut self, id: &str, text: &str) -> ControlReply {
        match upsert_custom_segment(&mut self.custom_segments, id, text) {
            Ok(()) => ControlReply::Ok(String::new()),
            Err(e) => ControlReply::Err(format!("set-segment: {e}")),
        }
    }

    /// Apply a `clear-segment <id>` request: remove a custom segment
    /// previously set by `set-segment`. Always replies `OK` — clearing an id
    /// that isn't set is not an error (idempotent, matching `RunAction`'s
    /// "already applied" tolerance elsewhere in this file).
    fn control_clear_segment(&mut self, id: &str) -> ControlReply {
        remove_custom_segment(&mut self.custom_segments, id);
        ControlReply::Ok(String::new())
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
