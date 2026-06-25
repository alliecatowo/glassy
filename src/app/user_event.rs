//! `ApplicationHandler::user_event` dispatch. Handles all [`UserEvent`]
//! variants from the PTY threads (title, cwd, clipboard, bell, progress,
//! toasts, etc.). Called from the `user_event` method in event_loop.rs;
//! extracted to keep that file under 700 lines.

use super::*;

/// Dispatch a single [`UserEvent`] from the PTY thread. Called from
/// `ApplicationHandler::user_event` in event_loop.rs.
pub(super) fn dispatch(
    app: &mut App,
    event_loop: &winit::event_loop::ActiveEventLoop,
    event: UserEvent,
) {
    match event {
        UserEvent::Title(id, title) => {
            // The focused pane drives the chip/window title. Non-focused panes
            // of the active tab have their title stored in `others_titles` for
            // the pane title-bar headers (Wave 3). After a split the focused
            // leaf id != active_id, so resolve via active_focused_id().
            if id == app.active_focused_id() {
                // Also keep the focused pane's title in others_titles so the
                // header painter can look up any pane id uniformly.
                if let Some(g) = app.panes.as_mut() {
                    g.others_titles.insert(id, title.clone());
                }
                app.active_title = title;
                app.update_window_title();
            } else if let Some(g) = app.panes.as_mut()
                && g.others.contains_key(&id)
            {
                // Non-focused pane of the active tab.
                g.others_titles.insert(id, title);
            } else if let Some(s) = app.background.iter_mut().find(|s| s.id == id) {
                // Parked tab (single-pane or its focused pane).
                s.title = title.clone();
                // Also store in background tab's pane group if it's multi-pane.
                if let Some(g) = s.panes.as_mut() {
                    g.others_titles.insert(id, title);
                }
            }
        }
        UserEvent::ChildExit(id) => {
            app.handle_child_exit(id, event_loop);
            return;
        }
        UserEvent::Bell(id) => {
            // Ring for any pane of the active tab (the visible one).
            if app.id_in_active_tab(id) {
                app.trigger_bell();
            }
        }
        // A background tab produced output: its terminal state updated
        // silently; no redraw needed until it becomes active.
        UserEvent::Wakeup(id) => {
            // (Re)arm this session's busy window: a wakeup means it just emitted
            // output, so its chip spins until BUSY_LINGER elapses with no more.
            // about_to_wait advances the spinner and clears the deadline (and
            // keeps a finite wakeup scheduled) exactly like the bell flash.
            let busy = Instant::now() + BUSY_LINGER;
            // Output from a NON-focused pane of the ACTIVE tab is visible, so
            // mark the active tab busy and repaint just like the focused pane.
            if id != app.active_id && app.id_in_active_tab(id) {
                app.active_busy_until = Some(busy);
                app.mark_dirty(event_loop);
                return;
            }
            if id != app.active_id {
                // A background tab produced output (in any of its panes): flag
                // its chip for the activity dot. Only repaint on the false->true
                // edge so a busy background tab doesn't spam redraws.
                let owner = app
                    .tab_pos_of_pane(id)
                    .and_then(|p| app.tab_order.get(p).copied());
                if let Some(owner) = owner
                    && let Some(s) = app.background.iter_mut().find(|s| s.id == owner)
                {
                    let was_busy = s.busy_until.is_some_and(|t| Instant::now() < t);
                    s.busy_until = Some(busy);
                    if !s.activity || !was_busy {
                        s.activity = true;
                        app.mark_dirty(event_loop);
                    }
                }
                return;
            }
            app.active_busy_until = Some(busy);
        }
        UserEvent::PtyWrite(id, text) => {
            // Route the VT reply back to the exact pane that produced it (any
            // tab, any split pane); not a visual change, so no repaint.
            let bytes = text.into_bytes();
            if let Some(pty) = app.pty_by_id(id) {
                pty.write(bytes);
            }
            return;
        }
        UserEvent::Cwd(id, path) => {
            // OSC 7: record the reporting pane's cwd so new tabs/splits inherit
            // it. Only a tab's FOCUSED pane drives the inherited cwd (mirrors
            // the title handling); not a visual change, so no repaint.
            if app.id_in_active_tab(id) {
                if id == app.active_focused_id() {
                    app.active_cwd = Some(path);
                } else {
                    // A non-focused active-tab pane reports its own cwd: the
                    // focused pane's stays the tab's inherited cwd, but we still
                    // record the per-pane cwd for session persistence.
                    app.active_pane_cwds.insert(id, path);
                }
            } else {
                // A pane of a parked tab. The focused pane (id == tab id) drives
                // last_cwd; a non-focused pane records into pane_cwds.
                for s in app.background.iter_mut() {
                    if s.id == id {
                        s.last_cwd = Some(path);
                        break;
                    }
                    if s.panes.as_ref().is_some_and(|g| g.others.contains_key(&id)) {
                        s.pane_cwds.insert(id, path);
                        break;
                    }
                }
            }
            return;
        }
        UserEvent::ClipboardStore(_id, _ty, mut text) => {
            // OSC 52 copy: write to the OS clipboard on the UI thread (arboard
            // must not run on the PTY thread). arboard exposes only the standard
            // clipboard, so a Selection store also lands there. Not visual.
            // Cap the payload (~1 MiB) so a hostile / runaway program can't push
            // the clipboard to an unbounded size; truncate on a char boundary.
            const OSC52_MAX: usize = 1 << 20;
            if text.len() > OSC52_MAX {
                let mut end = OSC52_MAX;
                while end > 0 && !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                log::debug!("OSC 52 store truncated to {end} bytes");
            }
            if let Some(cb) = app.clipboard()
                && let Err(e) = cb.set_text(text)
            {
                log::debug!("OSC 52 clipboard store failed: {e}");
            }
            return;
        }
        UserEvent::ClipboardLoad(id, _ty, formatter) => {
            // OSC 52 read: read the clipboard, format the reply, and write it
            // back to the requesting pane over the PtyWrite path. Not visual.
            let text = app.clipboard().and_then(|cb| match cb.get_text() {
                Ok(t) => Some(t),
                Err(e) => {
                    log::debug!("OSC 52 clipboard load failed: {e}");
                    None
                }
            });
            if let Some(text) = text
                && let Some(pty) = app.pty_by_id(id)
            {
                pty.write(formatter.0(&text).into_bytes());
            }
            return;
        }
        UserEvent::SemanticMark(_id, _mark) => {
            // OSC 133 semantic mark: the PromptTracker on the Pty already
            // recorded the row offset for 'A' marks. No redraw needed; this
            // event is a notification hook for future UI use (e.g. prompt-count
            // in the status bar or Shift+Up/Down keybind wiring in app/input.rs).
            return;
        }
        UserEvent::Notification(_id, text) => {
            // OSC 9 / OSC 777: fire a desktop notification when the window is
            // not focused so background jobs can alert the user. Also show an
            // in-app toast so the user sees it even when focused.
            if !app.focused {
                fire_desktop_notification("glassy", &text);
            }
            // Always show an in-app toast for OSC 9/777 notifications.
            app.push_toast(text);
        }
        UserEvent::ConfigReload => {
            // Config file changed; reload from disk and apply live-reloadable settings.
            match crate::config::Settings::resolve(std::iter::empty()) {
                Ok(Some(settings)) => {
                    app.apply_config_reload(&settings.config);
                }
                Ok(None) => {
                    log::debug!("config reload: --help/--version");
                }
                Err(e) => {
                    log::warn!("config reload failed: {e}");
                }
            }
            return;
        }
        UserEvent::ModifyOtherKeys(id, level) => {
            // xterm modifyOtherKeys level changed by the running application
            // (CSI > 4 ; N m intercepted in the PTY loop). Update the field so
            // subsequent encode_key calls emit the correct encoding for modified
            // printable keys. Only the active focused pane's level applies
            // (keystrokes route to self.pty, whose id is active_focused_id()).
            if id == app.active_focused_id() {
                app.modify_other_keys = level;
            }
            return;
        }
        UserEvent::Progress(id, state) => {
            // OSC 9;4 progress report: update the active session's indicator.
            // Non-active sessions' progress is ignored (only the focused session
            // renders a progress bar). On Remove, clear the indicator. Resolve
            // the focused pane via active_focused_id() (split-aware).
            if id == app.active_focused_id() {
                app.active_progress = match state {
                    crate::image::ProgressState::Remove => None,
                    other => Some(other),
                };
            }
            // Progress changes are visual — mark dirty so the status bar repaints.
        }
    }
    app.mark_dirty(event_loop);
}
