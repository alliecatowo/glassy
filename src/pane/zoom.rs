//! Pane-zoom state machine — a tiny, pure flag that temporarily maximizes the
//! focused leaf of a split so it fills the whole content area while the other
//! tiles are hidden. Kept separate from [`super::Layout`] (which stays a pure
//! geometry tree) because zoom is a *presentation* mode layered on top of the
//! tiling, not a structural change to the tree: unzooming restores the exact
//! prior partition.
//!
//! The state is just "on or off", but the transitions carry rules that are easy
//! to get subtly wrong, so they live here behind named methods and are unit
//! tested:
//!   * Zoom only engages when more than one pane exists (`leaf_count > 1`); a
//!     single pane already fills the area, so a toggle there is a no-op.
//!   * Any structural change that could strand the zoom — a split, a close, or
//!     a focus move to a *different* pane — clears it, so the app never renders
//!     "zoomed into" a pane that is gone or no longer focused.

/// Whether the focused pane is currently zoomed (maximized over its siblings).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Zoom {
    on: bool,
}

impl Zoom {
    /// A fresh (un-zoomed) state.
    pub fn new() -> Self {
        Zoom { on: false }
    }

    /// Whether zoom is currently active.
    pub fn is_on(self) -> bool {
        self.on
    }

    /// Toggle zoom, given the current `leaf_count` of the layout. Engaging zoom
    /// requires a split (`leaf_count > 1`); with a single pane this is a no-op and
    /// stays off. Returns the new state so the caller can decide whether anything
    /// changed (and thus whether to repaint / resize).
    #[must_use]
    pub fn toggle(self, leaf_count: usize) -> Self {
        if self.on {
            Zoom { on: false }
        } else {
            Zoom { on: leaf_count > 1 }
        }
    }

    /// Clear zoom unconditionally. Called on any structural change (split, close,
    /// focus move to a different pane) so the zoom can never outlive the pane it
    /// was framing.
    #[must_use]
    pub fn cleared(self) -> Self {
        Zoom { on: false }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_off() {
        assert!(!Zoom::new().is_on());
        assert!(!Zoom::default().is_on());
    }

    #[test]
    fn toggle_engages_only_when_split() {
        // A single pane can't zoom: toggling stays off.
        assert!(!Zoom::new().toggle(1).is_on());
        // With >1 leaf, the first toggle turns it on.
        assert!(Zoom::new().toggle(2).is_on());
    }

    #[test]
    fn toggle_off_works_regardless_of_count() {
        // Once on, a toggle turns it off even if the count says it "could" be on.
        let z = Zoom::new().toggle(3);
        assert!(z.is_on());
        assert!(!z.toggle(3).is_on());
        // And turning off does not depend on the count (a closed-down split that
        // is now a single pane still un-zooms cleanly).
        let z = Zoom { on: true };
        assert!(!z.toggle(1).is_on());
    }

    #[test]
    fn cleared_always_off() {
        assert!(!Zoom { on: true }.cleared().is_on());
        assert!(!Zoom::new().cleared().is_on());
    }

    #[test]
    fn toggle_is_idempotent_in_pairs() {
        // Two toggles return to the start (when a split exists throughout).
        let start = Zoom::new();
        assert_eq!(start.toggle(2).toggle(2), start);
    }
}
