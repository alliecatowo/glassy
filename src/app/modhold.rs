//! Modifier-HOLD numbered tab overlay (⌘-hold on macOS / Ctrl-hold elsewhere).
//!
//! Holding the primary tab modifier alone — ⌘ on macOS, Ctrl on every other
//! platform, matching the `GoToTab(1..9)` default binds (⌘1.. / Ctrl1..) — for a
//! short dwell reveals a small number badge on each tab chip, so the held
//! modifier + a digit jumps to that tab. The jump itself is the existing
//! `GoToTab` chord; this module only adds the *visual affordance* (which tab is
//! which number) so the user doesn't have to count chips.
//!
//! Idle-safe: arming the hold schedules exactly one `WaitUntil` at the dwell
//! deadline (see [`App::mod_hold_deadline`]); when it elapses the overlay paints
//! once and the loop settles back to `Wait`. Pressing any non-modifier key, or
//! releasing the modifier, disarms it — so a forgotten hold never spins the CPU.

use super::*;
use crate::config::Platform;
use std::time::Duration;
use winit::keyboard::ModifiersState;

/// Dwell before the numbered overlay appears, so a quick ⌘C / Ctrl+C never
/// flashes tab numbers. Long enough to read as a deliberate hold.
pub(crate) const MOD_HOLD_DWELL: Duration = Duration::from_millis(400);

/// Whether `mods` is *exactly* the primary tab modifier for `platform` with no
/// other modifier mixed in. ⌘ alone on macOS; Ctrl alone elsewhere. A mixed
/// chord (e.g. ⌘⇧) does not arm the overlay. Pure for unit testing.
pub(crate) fn is_lone_primary_mod(mods: ModifiersState, platform: Platform) -> bool {
    let ctrl = mods.control_key();
    let shift = mods.shift_key();
    let alt = mods.alt_key();
    let meta = mods.super_key();
    if platform.is_mac() {
        meta && !ctrl && !shift && !alt
    } else {
        ctrl && !meta && !shift && !alt
    }
}

impl App {
    /// React to a modifiers change: arm the hold timer when the primary modifier
    /// becomes the lone held modifier, disarm it otherwise. Returns `true` when
    /// the armed state changed (so the caller can reschedule / repaint).
    pub(super) fn update_mod_hold(&mut self, event_loop: &ActiveEventLoop) {
        // Only meaningful with more than one tab to number.
        let eligible = self.tab_count() > 1 && is_lone_primary_mod(self.mods, Platform::current());
        match (eligible, self.mod_hold_since) {
            (true, None) => {
                self.mod_hold_since = Some(Instant::now());
                // Schedule the dwell wake (about_to_wait folds this into the
                // overall WaitUntil); no immediate repaint — the overlay only
                // shows after the dwell.
                self.mark_dirty(event_loop);
            }
            (false, Some(_)) => {
                let was_showing = self.mod_overlay_active(Instant::now());
                self.mod_hold_since = None;
                // Only repaint to *clear* the overlay if it was actually visible.
                if was_showing {
                    self.force_full_redraw = true;
                    self.mark_dirty(event_loop);
                }
            }
            _ => {}
        }
    }

    /// Cancel any armed modifier-hold (a non-modifier key was pressed, so the
    /// gesture is now a chord, not a bare hold). Repaints to clear the overlay if
    /// it was visible. Called from the keyboard handler on every pressed key.
    pub(super) fn cancel_mod_hold(&mut self, event_loop: &ActiveEventLoop) {
        if self.mod_hold_since.is_some() {
            let was_showing = self.mod_overlay_active(Instant::now());
            self.mod_hold_since = None;
            if was_showing {
                self.force_full_redraw = true;
                self.mark_dirty(event_loop);
            }
        }
    }

    /// Whether the numbered tab overlay should be drawn right now: the primary
    /// modifier has been held alone past the dwell. False when not armed or still
    /// within the dwell window.
    pub(crate) fn mod_overlay_active(&self, now: Instant) -> bool {
        self.mod_hold_since
            .is_some_and(|since| now.duration_since(since) >= MOD_HOLD_DWELL)
    }

    /// The instant the modifier-hold overlay becomes visible, for the wake
    /// schedule. `Some(deadline)` while armed-but-not-yet-shown (so the loop wakes
    /// to paint it); `None` once shown or when disarmed.
    pub(crate) fn mod_hold_deadline(&self) -> Option<Instant> {
        self.mod_hold_since.and_then(|since| {
            let at = since + MOD_HOLD_DWELL;
            (Instant::now() < at).then_some(at)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mods(ctrl: bool, shift: bool, alt: bool, meta: bool) -> ModifiersState {
        let mut m = ModifiersState::empty();
        if ctrl {
            m |= ModifiersState::CONTROL;
        }
        if shift {
            m |= ModifiersState::SHIFT;
        }
        if alt {
            m |= ModifiersState::ALT;
        }
        if meta {
            m |= ModifiersState::SUPER;
        }
        m
    }

    #[test]
    fn lone_ctrl_arms_on_pc_lone_meta_on_mac() {
        // PC: bare Ctrl yes, bare Cmd no.
        assert!(is_lone_primary_mod(
            mods(true, false, false, false),
            Platform::Linux
        ));
        assert!(!is_lone_primary_mod(
            mods(false, false, false, true),
            Platform::Linux
        ));
        // Mac: bare Cmd yes, bare Ctrl no.
        assert!(is_lone_primary_mod(
            mods(false, false, false, true),
            Platform::Mac
        ));
        assert!(!is_lone_primary_mod(
            mods(true, false, false, false),
            Platform::Mac
        ));
    }

    #[test]
    fn mixed_modifiers_do_not_arm() {
        // Ctrl+Shift on PC, Cmd+Shift on Mac: a chord, not a bare hold.
        assert!(!is_lone_primary_mod(
            mods(true, true, false, false),
            Platform::Linux
        ));
        assert!(!is_lone_primary_mod(
            mods(false, true, false, true),
            Platform::Mac
        ));
    }

    #[test]
    fn no_modifier_never_arms() {
        assert!(!is_lone_primary_mod(
            mods(false, false, false, false),
            Platform::Linux
        ));
        assert!(!is_lone_primary_mod(
            mods(false, false, false, false),
            Platform::Mac
        ));
    }
}
