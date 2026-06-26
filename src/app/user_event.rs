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
            // Also feed the cwd-history ring for the command palette's recent-dirs
            // source (only the focused pane's reports, to avoid noise from busy
            // background panes).
            if id == app.active_focused_id() {
                app.record_cwd_history(path.clone());
            }
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
        UserEvent::SemanticMark(id, mark, _exit) => {
            // OSC 133 semantic mark: the PromptTracker on the Pty already recorded
            // the row offset, timing, and exit status (command-block tracking).
            // A `D` mark finishes a command, which changes the exit-status badge +
            // duration the active pane draws — repaint it. A/B/C don't change what
            // is currently visible, so skip the redraw for those to preserve the
            // 0%-idle invariant during heavy prompt churn.
            if mark == 'D' && id == app.active_focused_id() {
                app.mark_dirty(event_loop);
            }
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
        UserEvent::Ipc(cmd) => {
            // A `glassy toggle/show/hide` from a compositor hotkey (or a second
            // launch) arrived over the single-instance socket. Drive the quake
            // window's slide; `quake_apply` is a no-op in normal windowed mode
            // (it just raises/focuses the window so the bind isn't a dead key).
            app.quake_apply(cmd, event_loop);
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
        UserEvent::SgrPixelMouse(id, on) => {
            // DECSET/DECRST 1016 (SGR-Pixel mouse) toggled by the running app.
            // Only the active focused pane's state applies (mouse reports route to
            // self.pty). Not a visual change, so no repaint.
            if id == app.active_focused_id() {
                app.sgr_pixel_mouse = on;
            }
            return;
        }
        UserEvent::CommandRun(_id, cmd) => {
            // OSC 133 command-zone capture: record the run command into the
            // history ring for the command palette (any pane's commands are
            // useful history). Not a visual change, so no repaint.
            app.record_command_history(cmd);
            return;
        }
        UserEvent::Progress(id, state) => {
            // OSC 9;4 progress report: update the active session's indicator.
            // Non-active sessions' progress is ignored (only the focused session
            // renders a progress bar). On Remove, clear the indicator. Resolve
            // the focused pane via active_focused_id() (split-aware).
            if id != app.active_focused_id() {
                // A background pane's progress isn't drawn anywhere, so updating it
                // changes nothing on screen — return WITHOUT marking dirty to avoid
                // spurious repaints (and event-loop wakeups) from busy background
                // panes that emit OSC 9;4.
                return;
            }
            app.active_progress = match state {
                crate::image::ProgressState::Remove => None,
                other => Some(other),
            };
            // Progress changes are visual — mark dirty so the status bar repaints.
        }
        UserEvent::Peek(id, path) => {
            // OSC 1337 Peek: only the active focused pane's request shows a card
            // (a background pane peeking would be confusing). Resolve relative
            // paths against the session's cwd, read a capped head, and stash the
            // card; the next keystroke/Esc/click dismisses it.
            if id == app.active_focused_id() {
                app.show_peek(&path);
            }
        }
        UserEvent::TextBlinkPresent(id) => {
            // SGR 5/6 (text blink) detected in the byte stream of the active
            // session. Arm the text-blink timer so `about_to_wait` drives phase
            // flips and periodic redraws (like the cursor-blink timer). Only the
            // active focused pane can have its blinking cells visible on screen.
            if id == app.active_focused_id() && !app.text_blink_active {
                app.text_blink_active = true;
                app.text_blink_on = true;
                app.text_blink_at = Instant::now() + BLINK_INTERVAL;
            }
            // Already active or not our session: still mark dirty for the redraw.
        }
        UserEvent::TextBlinkCleared(id) => {
            // The active pane's screen was erased/reset, wiping any blinking cells.
            // Disarm the timer so about_to_wait can return to ControlFlow::Wait
            // (0% idle). A later TextBlinkPresent re-arms it if blink reappears.
            // Only the active focused pane drives the visible timer.
            if id == app.active_focused_id() && app.text_blink_active {
                app.text_blink_active = false;
            } else {
                // Not the active pane (or already idle): nothing visual changed for
                // the blink timer; skip the repaint this event would otherwise force.
                return;
            }
            // Repaint so any now-solid (formerly mid-blink-off) cells show.
        }
        UserEvent::MenuAction(action) => {
            // A macOS global menu-bar item was clicked: run it through the exact
            // same dispatch as the equivalent keychord, so the menu and the
            // keyboard can never diverge. `run_key_action` marks dirty itself.
            app.run_key_action(action, event_loop);
            return;
        }
    }
    app.mark_dirty(event_loop);
}
