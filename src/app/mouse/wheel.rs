//! Mouse wheel / touchpad scroll handling (`handle_mouse_wheel`).
//!
//! Routes the wheel to whichever surface owns it: the help-panel keybinding
//! list while it is open, the tab strip (one tab per swipe), or the focused
//! pane's content — where it scrolls scrollback, emits arrow keys on the alt
//! screen, or reports as button 64/65 in mouse mode.

use super::*;

impl App {
    /// Handle a `WindowEvent::MouseWheel` event.
    pub(in crate::app) fn handle_mouse_wheel(
        &mut self,
        delta: MouseScrollDelta,
        phase: winit::event::TouchPhase,
        event_loop: &ActiveEventLoop,
    ) {
        use winit::event::TouchPhase;
        // Help panel owns the wheel while open: scroll its keybinding list.
        // The next `build_help` clamps `scroll` against the content height,
        // so we just accumulate here (negative = scroll up toward the top).
        if self.help_open {
            let line_px = self
                .renderer
                .as_ref()
                .map(|r| r.cell_metrics().height)
                .unwrap_or(20.0)
                .max(1.0);
            let dy = match delta {
                MouseScrollDelta::LineDelta(_, y) => y * line_px * 3.0,
                MouseScrollDelta::PixelDelta(p) => p.y as f32,
            };
            // Wheel-up (positive y) moves content up = decrease scroll.
            self.help_state.scroll = (self.help_state.scroll - dy).max(0.0);
            self.mark_dirty(event_loop);
            return;
        }
        // A touchpad gesture brackets its deltas with Started/Ended; reset
        // the accumulators and the one-switch-per-swipe latch at those
        // boundaries so each gesture is independent.
        if matches!(
            phase,
            TouchPhase::Started | TouchPhase::Ended | TouchPhase::Cancelled
        ) {
            self.tab_scroll_accum = 0.0;
            self.content_scroll_accum = 0.0;
            self.swipe_consumed = false;
        }

        // Over the tab strip: a swipe/scroll switches tabs as a discrete
        // GESTURE — one tab per swipe, clamped at the ends (no wrap-around
        // carousel). Horizontal motion is preferred (natural swipe-to-switch).
        let in_strip = {
            // Honour the `show_tab_bar` policy: 0 when the strip is hidden so
            // the top band routes to the terminal instead of swiping tabs.
            let bar_h = self.effective_tab_bar_h() as f64;
            bar_h > 0.0 && self.mouse_px.1 < bar_h
        };
        if in_strip {
            const STEP: f32 = 24.0; // px of swipe travel to trigger one switch
            match delta {
                // A discrete wheel notch always steps one tab (clamped).
                MouseScrollDelta::LineDelta(x, y) => {
                    let primary = if x.abs() > y.abs() { x } else { y };
                    if primary > 0.0 {
                        self.step_tab(1, event_loop);
                    } else if primary < 0.0 {
                        self.step_tab(-1, event_loop);
                    }
                }
                // Touchpad: accumulate, fire ONCE per swipe at the threshold,
                // then latch until the gesture ends — no twitchy carousel.
                MouseScrollDelta::PixelDelta(p) => {
                    let primary = (if p.x.abs() > p.y.abs() { p.x } else { p.y }) as f32;
                    self.tab_scroll_accum += primary;
                    if !self.swipe_consumed && self.tab_scroll_accum.abs() >= STEP {
                        let dir = if self.tab_scroll_accum > 0.0 { 1 } else { -1 };
                        self.step_tab(dir, event_loop);
                        self.swipe_consumed = true;
                    }
                }
            }
            return;
        }
        self.tab_scroll_accum = 0.0;

        // In a split, the wheel targets the pane under the pointer: focus it
        // so the scroll / mouse-report below acts on that pane's PTY.
        if self.is_split() {
            let (mx, my) = self.mouse_px;
            self.focus_pane_at(mx, my, event_loop);
        }

        let lines = match delta {
            MouseScrollDelta::LineDelta(_, y) => {
                self.content_scroll_accum = 0.0;
                if y == 0.0 {
                    0
                } else {
                    (y.abs().ceil() as i32) * y.signum() as i32
                }
            }
            // Touchpads emit many sub-line pixel deltas; accumulate and step
            // by the cell height so slow scrolls register instead of being
            // truncated to zero each event (the "tiny scrolls do nothing" bug).
            MouseScrollDelta::PixelDelta(p) => {
                self.content_scroll_accum += p.y as f32;
                let step = self
                    .renderer
                    .as_ref()
                    .map(|r| r.cell_metrics().height)
                    .unwrap_or(20.0)
                    .max(1.0);
                let n = (self.content_scroll_accum / step) as i32;
                self.content_scroll_accum -= n as f32 * step;
                n
            }
        };
        if lines == 0 {
            return;
        }
        let mode = self.term_mode();
        let up = lines > 0;
        let count = lines.unsigned_abs() as usize;

        match wheel_action(mode) {
            WheelAction::Report => {
                // Wheel as button 64 (up) / 65 (down), one report per line.
                let button = if up { 64 } else { 65 };
                for _ in 0..count {
                    self.report_mouse(button, true, false, mode);
                }
            }
            WheelAction::Arrows => {
                // Alt-screen apps (pagers, bat, vim without `mouse=`) expect
                // the wheel to emit arrow keys — xterm's alternateScroll is
                // on by default and the alt screen has no scrollback of its
                // own. ~3 lines per notch.
                if let Some(pty) = &self.pty {
                    // DECCKM: when the app enabled application cursor-key
                    // mode, arrow keys go out as SS3 (ESC O X), so the wheel
                    // emulation must match or the pager sees the wrong code.
                    let seq: &[u8] = if mode.contains(TermMode::APP_CURSOR) {
                        if up { b"\x1bOA" } else { b"\x1bOB" }
                    } else if up {
                        b"\x1b[A"
                    } else {
                        b"\x1b[B"
                    };
                    let n = count * 3;
                    let mut out = Vec::with_capacity(seq.len() * n);
                    for _ in 0..n {
                        out.extend_from_slice(seq);
                    }
                    pty.write(out);
                }
            }
            WheelAction::Scrollback => {
                let delta = if up { WHEEL_LINES } else { -WHEEL_LINES } * count as i32;
                if let Some(pty) = &self.pty {
                    pty.term.lock().scroll_display(Scroll::Delta(delta));
                }
                self.mark_dirty(event_loop);
            }
        }
    }
}
