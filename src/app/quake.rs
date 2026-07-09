//! Quake / dropdown window mode.
//!
//! When `config.quake` is on, glassy launches as a borderless, top-anchored,
//! always-on-top window spanning the monitor width and `quake_height` of its
//! height. It starts hidden (parked just above the top edge) and slides down on
//! show / up on hide. The slide is driven from `about_to_wait` like every other
//! glassy animation, so it stays **idle-safe**: the loop only runs on
//! `ControlFlow::Poll` while a slide is in flight and returns to `Wait` (0% CPU)
//! the instant it settles.
//!
//! Toggling is two-pronged because Wayland has no portable global hotkey:
//!
//! * the in-app `quake_toggle` keybind ([`crate::config::KeyAction::QuakeToggle`]),
//! * an external `glassy toggle` (bound to a compositor hotkey) over the
//!   single-instance IPC socket → [`crate::pty::UserEvent::Ipc`].
//!
//! Both funnel into [`App::quake_apply`].

use super::*;
use crate::ipc::IpcCommand;

impl App {
    /// Arm quake mode at startup: reconfigure the freshly-created window to be
    /// borderless, always-on-top, top-anchored, and initially hidden (parked above
    /// the screen). Called from `resumed()` exactly when `config.quake` is set.
    /// A no-op (leaving normal windowed mode intact) otherwise.
    pub(crate) fn init_quake(&mut self, event_loop: &ActiveEventLoop) {
        if !self.config.quake {
            return;
        }
        let Some(window) = self.window.clone() else {
            return;
        };
        // Borderless + always-on-top: a quake terminal has no titlebar and floats
        // over other windows. `set_decorations(false)` drops the CSD chrome; the
        // always-on-top level keeps it above normal windows when dropped.
        window.set_decorations(false);
        window.set_window_level(winit::window::WindowLevel::AlwaysOnTop);

        let geom = self.quake_geometry(event_loop);
        // Size to the computed quake rect, then park hidden above the top edge.
        let _ = window.request_inner_size(winit::dpi::PhysicalSize::new(
            geom.monitor_w.max(1) as u32,
            geom.window_h.max(1) as u32,
        ));
        let mut st = QuakeState {
            shown: false,
            progress: 0.0,
            animating: false,
            last_step: Instant::now(),
            window_h: geom.window_h,
            origin: geom.origin,
            monitor_w: geom.monitor_w,
        };
        self.position_quake(&window, &st);
        // Begin a slide-in so launching the quake terminal reveals it (the user ran
        // `glassy` to open it). A subsequent `glassy hide`/keybind retracts it.
        st.shown = true;
        st.animating = self.config.quake_animation_ms > 0;
        if !st.animating {
            st.progress = 1.0;
        }
        st.last_step = Instant::now();
        self.quake = Some(st);
    }

    /// Resolve the target quake geometry from the window's current monitor: full
    /// monitor width, `quake_height` of its height, anchored at the monitor's
    /// top-left. Falls back to the current window size if no monitor is reported.
    fn quake_geometry(&self, _event_loop: &ActiveEventLoop) -> QuakeGeom {
        let window = self.window.as_ref();
        let monitor = window
            .and_then(|w| w.current_monitor())
            .or_else(|| window.and_then(|w| w.available_monitors().next()));
        if let Some(m) = monitor {
            let size = m.size();
            let pos = m.position();
            let frac = self.config.quake_height.clamp(0.1, 1.0);
            let window_h = ((size.height as f32) * frac).round() as i32;
            QuakeGeom {
                origin: (pos.x, pos.y),
                monitor_w: size.width as i32,
                window_h: window_h.max(1),
            }
        } else if let Some(w) = window {
            let size = w.inner_size();
            QuakeGeom {
                origin: (0, 0),
                monitor_w: size.width as i32,
                window_h: size.height as i32,
            }
        } else {
            QuakeGeom {
                origin: (0, 0),
                monitor_w: 1,
                window_h: 1,
            }
        }
    }

    /// Place the quake window at its current slide progress: at progress 1.0 the
    /// top edge sits at the monitor top; at 0.0 it is parked one full window-height
    /// above (off-screen). `progress` advances linearly in time (see
    /// [`Self::step_quake`]); the displayed position is cubic-eased so the drop
    /// decelerates into its resting edge in BOTH directions — for a slide-in the
    /// revealed fraction eases out toward fully-shown, and for a slide-out the
    /// hidden fraction eases out toward fully-parked.
    fn position_quake(&self, window: &Window, st: &QuakeState) {
        let (ox, oy) = st.origin;
        let revealed = if st.shown {
            gui::ease_out_cubic(st.progress)
        } else {
            1.0 - gui::ease_out_cubic(1.0 - st.progress)
        };
        // hidden_y = oy - window_h (fully above the top edge); shown_y = oy.
        let y = oy - ((1.0 - revealed) * st.window_h as f32).round() as i32;
        window.set_outer_position(winit::dpi::PhysicalPosition::new(ox, y));
    }

    /// Apply an IPC / keybind command to the quake window. Starts (or reverses) the
    /// slide as needed. A no-op when not in quake mode. Returns true if the request
    /// changed state (so the caller can schedule the animation).
    pub(crate) fn quake_apply(&mut self, cmd: IpcCommand, event_loop: &ActiveEventLoop) -> bool {
        if self.quake.is_none() {
            // Not a quake-mode instance: a `show`/`toggle` still usefully raises and
            // focuses the existing window so a compositor bind isn't a dead key.
            if let Some(w) = self.window.as_ref()
                && !matches!(cmd, IpcCommand::Hide)
            {
                w.set_visible(true);
                w.focus_window();
            }
            return false;
        }
        // Recompute geometry in case the monitor / DPI changed since launch.
        let geom = self.quake_geometry(event_loop);
        let instant = self.config.quake_animation_ms == 0;

        // Mutate the quake state in a tight scope, returning a small plan of what to
        // do to the window so the `&mut self.quake` borrow is released before we
        // touch `self.window` / call `&self` helpers.
        enum Plan {
            None,
            Refocus,
            Snapshot { snapshot: QuakeState, show: bool },
            Slide { show: bool },
        }
        let plan = {
            let Some(st) = self.quake.as_mut() else {
                return false;
            };
            st.window_h = geom.window_h;
            st.origin = geom.origin;
            st.monitor_w = geom.monitor_w;

            let want_shown = match cmd {
                IpcCommand::Toggle => !st.shown,
                IpcCommand::Show => true,
                IpcCommand::Hide => false,
            };
            let target = if want_shown { 1.0 } else { 0.0 };
            if want_shown == st.shown && (st.progress - target).abs() < 1e-3 {
                // Already resting in the requested state.
                if want_shown {
                    Plan::Refocus
                } else {
                    Plan::None
                }
            } else {
                st.shown = want_shown;
                st.last_step = Instant::now();
                if instant {
                    st.progress = target;
                    st.animating = false;
                    Plan::Snapshot {
                        snapshot: *st,
                        show: want_shown,
                    }
                } else {
                    st.animating = true;
                    Plan::Slide { show: want_shown }
                }
            }
        };

        match plan {
            Plan::None => return false,
            Plan::Refocus => {
                if let Some(w) = self.window.as_ref() {
                    w.focus_window();
                }
                return false;
            }
            Plan::Snapshot { snapshot, show } => {
                if let Some(w) = self.window.clone() {
                    self.position_quake(&w, &snapshot);
                    if show {
                        w.set_visible(true);
                        w.focus_window();
                    } else {
                        w.set_visible(false);
                    }
                }
            }
            Plan::Slide { show } => {
                // Reveal immediately so the slide-in is visible from frame one.
                if show && let Some(w) = self.window.as_ref() {
                    w.set_visible(true);
                    w.focus_window();
                }
            }
        }
        self.mark_dirty(event_loop);
        true
    }

    /// Advance the quake slide by `now - last_step`. Returns true while the slide is
    /// still in flight (caller keeps `ControlFlow::Poll`); false once it settles
    /// (caller may return to `Wait`). Idle-safe: does nothing unless `animating`.
    pub(crate) fn step_quake(&mut self, now: Instant) -> bool {
        let dur_ms = self.config.quake_animation_ms.max(1) as f32;
        let Some(window) = self.window.clone() else {
            return false;
        };
        let Some(st) = self.quake.as_mut() else {
            return false;
        };
        if !st.animating {
            return false;
        }
        let dt_ms = (now - st.last_step).as_secs_f32() * 1000.0;
        st.last_step = now;
        let delta = (dt_ms / dur_ms).clamp(0.0, 1.0);
        let target = if st.shown { 1.0 } else { 0.0 };
        if st.progress < target {
            st.progress = (st.progress + delta).min(target);
        } else {
            st.progress = (st.progress - delta).max(target);
        }
        let settled = (st.progress - target).abs() < 1e-3;
        if settled {
            st.progress = target;
            st.animating = false;
        }
        let snapshot = *st;
        self.position_quake(&window, &snapshot);
        if settled && !snapshot.shown {
            // Fully retracted: hide the window so it's off the compositor entirely
            // (saves a surface + lets click-through to whatever is behind it).
            window.set_visible(false);
        }
        self.dirty = true; // repaint while sliding
        !settled
    }

    /// Whether a quake slide is currently in flight (drives `Poll`).
    pub(crate) fn quake_animating(&self) -> bool {
        self.quake.as_ref().is_some_and(|q| q.animating)
    }

    /// Re-derive quake geometry after a monitor / DPI change and reposition. Called
    /// from the scale-factor / resize paths so the drop stays pinned to the top.
    pub(crate) fn quake_refresh_geometry(&mut self, event_loop: &ActiveEventLoop) {
        if self.quake.is_none() {
            return;
        }
        let geom = self.quake_geometry(event_loop);
        let snapshot = if let Some(st) = self.quake.as_mut() {
            st.window_h = geom.window_h;
            st.origin = geom.origin;
            st.monitor_w = geom.monitor_w;
            *st
        } else {
            return;
        };
        if let Some(w) = self.window.clone() {
            self.position_quake(&w, &snapshot);
        }
    }
}

/// Resolved quake target geometry (monitor-relative, physical px).
struct QuakeGeom {
    origin: (i32, i32),
    monitor_w: i32,
    window_h: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_state(shown: bool, progress: f32) -> QuakeState {
        QuakeState {
            shown,
            progress,
            animating: true,
            last_step: Instant::now(),
            window_h: 600,
            origin: (0, 0),
            monitor_w: 1920,
        }
    }

    // Pure slide-math probe mirroring `step_quake`'s progress update, so the
    // easing curve is testable without a window.
    fn advance(progress: f32, target: f32, delta: f32) -> f32 {
        if progress < target {
            (progress + delta).min(target)
        } else {
            (progress - delta).max(target)
        }
    }

    #[test]
    fn slide_in_increases_to_one() {
        let st = test_state(true, 0.0);
        let p = advance(st.progress, 1.0, 0.25);
        assert!((p - 0.25).abs() < 1e-6);
        let p = advance(p, 1.0, 0.9);
        assert!((p - 1.0).abs() < 1e-6, "clamps at fully shown");
    }

    #[test]
    fn slide_out_decreases_to_zero() {
        let st = test_state(false, 1.0);
        let p = advance(st.progress, 0.0, 0.4);
        assert!((p - 0.6).abs() < 1e-6);
        let p = advance(p, 0.0, 0.9);
        assert!(p.abs() < 1e-6, "clamps at fully hidden");
    }

    #[test]
    fn hidden_y_is_one_height_above_origin() {
        // At progress 0 the window top sits one full height above the monitor top.
        let st = test_state(false, 0.0);
        let y = st.origin.1 - ((1.0 - st.progress) * st.window_h as f32).round() as i32;
        assert_eq!(y, -600);
    }

    #[test]
    fn shown_y_is_at_origin() {
        let st = test_state(true, 1.0);
        let y = st.origin.1 - ((1.0 - st.progress) * st.window_h as f32).round() as i32;
        assert_eq!(y, 0);
    }

    #[test]
    fn midway_y_is_half_height_above() {
        let st = test_state(true, 0.5);
        let y = st.origin.1 - ((1.0 - st.progress) * st.window_h as f32).round() as i32;
        assert_eq!(y, -300);
    }

    #[test]
    fn copy_preserves_fields() {
        // QuakeState is Copy so a snapshot can be taken across the &mut self borrow.
        let st = test_state(true, 0.42);
        let c = st; // copy, not move
        assert_eq!(c.shown, st.shown);
        assert!((c.progress - st.progress).abs() < 1e-6);
        assert_eq!(c.window_h, st.window_h);
        assert_eq!(c.monitor_w, st.monitor_w);
    }
}
