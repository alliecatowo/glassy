//! In-app toast notifications: frosted cards that fade in, stay ~4 s, then fade
//! out and are removed. Drawn as a real GUI overlay in the top-right corner,
//! stacking downward. Uses the existing `push_overlay_rrect_px` +
//! `push_overlay_glyph_px_str` primitives — no new GPU state.
//!
//! Toast lifecycle (driven by the render path + `App::about_to_wait`):
//!   1. `push_toast(msg)` appends a [`Toast`] in the `FadeIn` phase.
//!   2. Each frame, `paint_toasts` is called (before the settings overlay so
//!      toasts float on top of the terminal but under the modal).  It advances
//!      every toast's phase based on elapsed time and paints live alphas.
//!   3. When a toast reaches `Dead`, the render path removes it from the vec.
//!
//! The toast queue lives in `App.toasts`; `App::push_toast` is the sole entry
//! point from the rest of the app.

use std::time::{Duration, Instant};

use crate::color;
use crate::gui;
use crate::renderer::Renderer;

/// Fade-in duration.
const FADE_IN: Duration = Duration::from_millis(200);
/// Visible (full alpha) hold time.
const HOLD: Duration = Duration::from_millis(3_600);
/// Fade-out duration.
const FADE_OUT: Duration = Duration::from_millis(400);
/// Total toast lifetime.
const TOTAL: Duration = Duration::from_millis(4_200);

/// Maximum number of toasts visible at once. Older ones are evicted when the
/// stack would exceed this (prevents runaway accumulation under rapid fire).
const MAX_TOASTS: usize = 5;

/// Corner radius of the toast card (px).
const CARD_RADIUS: f32 = 8.0;
/// Horizontal padding inside the card (px).
const PAD_X: f32 = 14.0;
/// Vertical padding inside the card.
const PAD_Y: f32 = 8.0;
/// Minimum card width (px).
const MIN_W: f32 = 160.0;
/// Maximum card width (px, ~60 chars at 8 px/char).
const MAX_W: f32 = 480.0;
/// Right margin from the window edge (px).
const MARGIN_RIGHT: f32 = 18.0;
/// Top margin from the tab bar bottom (px).
const MARGIN_TOP: f32 = 12.0;
/// Vertical gap between stacked toasts (px).
const GAP: f32 = 6.0;

/// The phase of a single toast card.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Phase {
    FadeIn,
    Hold,
    FadeOut,
    Dead,
}

/// One in-app toast notification.
#[derive(Debug, Clone)]
pub struct Toast {
    /// The message text (may contain Unicode/emoji; drawn through the glyph atlas).
    pub message: String,
    /// When this toast was created.
    pub created: Instant,
    /// Current phase (updated by `paint_toasts`).
    pub phase: Phase,
}

impl Toast {
    /// Create a new toast starting in `FadeIn`.
    pub fn new(message: impl Into<String>) -> Self {
        Toast {
            message: message.into(),
            created: Instant::now(),
            phase: Phase::FadeIn,
        }
    }

    /// Current alpha [0..1] based on elapsed time.
    pub fn alpha(&self) -> f32 {
        let elapsed = self.created.elapsed();
        if elapsed < FADE_IN {
            (elapsed.as_secs_f32() / FADE_IN.as_secs_f32()).clamp(0.0, 1.0)
        } else if elapsed < FADE_IN + HOLD {
            1.0
        } else if elapsed < TOTAL {
            let fade_elapsed = elapsed - FADE_IN - HOLD;
            let t = fade_elapsed.as_secs_f32() / FADE_OUT.as_secs_f32();
            (1.0 - t).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Advance the phase based on elapsed time. Returns true if the toast is now Dead.
    pub fn advance(&mut self) -> bool {
        let elapsed = self.created.elapsed();
        self.phase = if elapsed < FADE_IN {
            Phase::FadeIn
        } else if elapsed < FADE_IN + HOLD {
            Phase::Hold
        } else if elapsed < TOTAL {
            Phase::FadeOut
        } else {
            Phase::Dead
        };
        self.phase == Phase::Dead
    }

    /// Whether this toast still needs a wakeup (i.e. is not yet Dead).
    pub fn alive(&self) -> bool {
        self.created.elapsed() < TOTAL
    }

    /// Deadline for the next visual change (phase transition), for scheduling
    /// `ControlFlow::WaitUntil`. Returns `None` if already dead.
    pub fn next_deadline(&self) -> Option<Instant> {
        let elapsed = self.created.elapsed();
        if elapsed >= TOTAL {
            return None;
        }
        let phase_end = if elapsed < FADE_IN {
            self.created + FADE_IN
        } else if elapsed < FADE_IN + HOLD {
            self.created + FADE_IN + HOLD
        } else {
            self.created + TOTAL
        };
        Some(phase_end)
    }
}

/// Push a new toast onto the stack, evicting the oldest if `MAX_TOASTS` would
/// be exceeded.  Called from the app's `push_toast` method.
pub(crate) fn push(toasts: &mut Vec<Toast>, message: impl Into<String>) {
    if toasts.len() >= MAX_TOASTS {
        toasts.remove(0);
    }
    toasts.push(Toast::new(message));
}

/// Paint all live toasts and advance their phases.  Dead toasts are removed.
/// Returns `true` if any toast is still alive (caller should keep `Poll`/`WaitUntil`).
///
/// `tab_bar_h` is the pixel height of the tab bar so toasts are placed just below it.
pub(crate) fn paint_toasts(
    renderer: &mut Renderer,
    toasts: &mut Vec<Toast>,
    tab_bar_h: f32,
) -> bool {
    // Advance all, collect dead indices (in reverse to avoid index shift).
    let mut dead: Vec<usize> = Vec::new();
    for (i, t) in toasts.iter_mut().enumerate() {
        if t.advance() {
            dead.push(i);
        }
    }
    for i in dead.into_iter().rev() {
        toasts.remove(i);
    }

    if toasts.is_empty() {
        return false;
    }

    let (sw, _sh) = renderer.surface_size();
    let m = renderer.cell_metrics();
    let cell_h = m.height;

    // Stack from the top-right, most recent at the top.
    let mut y = tab_bar_h + MARGIN_TOP;
    for toast in toasts.iter().rev() {
        let alpha = toast.alpha();
        if alpha <= 0.001 {
            continue;
        }

        // Measure the text to size the card.  We use a simple char-count * cell_w
        // approximation since we can't render-measure without a full shape pass;
        // the result looks fine at typical font sizes.
        let msg_chars = toast.message.chars().count();
        let text_w = msg_chars as f32 * m.width;
        let card_w = (text_w + 2.0 * PAD_X)
            .clamp(MIN_W, MAX_W)
            .min(sw as f32 - 2.0 * MARGIN_RIGHT);
        let card_h = cell_h + 2.0 * PAD_Y;
        let card_x = sw as f32 - MARGIN_RIGHT - card_w;
        let card_y = y;

        // Glass card background: frosted at current alpha.
        let bg_base = color::default_bg();
        let card_bg = [
            bg_base[0] * 0.12 + 0.04, // slightly lighter than the terminal bg
            bg_base[1] * 0.12 + 0.04,
            bg_base[2] * 0.14 + 0.06,
            alpha * gui::GLASS_FLOAT_ALPHA,
        ];
        renderer.push_overlay_rrect_px(card_x, card_y, card_w, card_h, CARD_RADIUS, card_bg);

        // Thin accent border (very subtle).
        let accent = color::accent();
        let border_c = [accent[0], accent[1], accent[2], alpha * 0.35];
        renderer.push_overlay_rrect_px(
            card_x - 0.5,
            card_y - 0.5,
            card_w + 1.0,
            card_h + 1.0,
            CARD_RADIUS + 0.5,
            border_c,
        );
        renderer.push_overlay_rrect_px(card_x, card_y, card_w, card_h, CARD_RADIUS, card_bg);

        // Left accent stripe (3 px wide, rounded on the left).
        let stripe_c = [accent[0], accent[1], accent[2], alpha * 0.8];
        renderer.push_overlay_rrect_px(card_x, card_y, 3.0, card_h, CARD_RADIUS, stripe_c);

        // Toast message text.
        let fg = gui::fg();
        let text_fg = [fg[0], fg[1], fg[2], alpha];
        let tx = (card_x + PAD_X).round();
        let ty = (card_y + (card_h - cell_h) * 0.5).round();
        // Clip the message to MAX_W minus padding.
        let max_chars = ((card_w - 2.0 * PAD_X) / m.width).floor() as usize;
        let displayed: String = if toast.message.chars().count() <= max_chars {
            toast.message.clone()
        } else {
            let tail: String = toast
                .message
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect();
            format!("{tail}…")
        };
        renderer.push_overlay_glyph_px_str(tx, ty, &displayed, text_fg);

        y += card_h + GAP;
    }

    // Any toast still alive means we need continued redraws.
    toasts.iter().any(|t| t.alive())
}

/// The earliest wakeup deadline among all live toasts (for `ControlFlow::WaitUntil`).
pub(crate) fn next_deadline(toasts: &[Toast]) -> Option<Instant> {
    toasts.iter().filter_map(|t| t.next_deadline()).min()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn toast_fades_in_and_out() {
        let t = Toast::new("hello");
        // Immediately after creation: alpha is near zero (fade-in not complete).
        let a0 = t.alpha();
        assert!((0.0..=1.0).contains(&a0), "alpha must be in [0,1]: {a0}");
    }

    #[test]
    fn toast_alpha_after_hold_starts_at_one() {
        let mut t = Toast::new("msg");
        // Fake the creation time to be in the HOLD phase.
        t.created = Instant::now() - (FADE_IN + Duration::from_millis(100));
        assert!(
            (t.alpha() - 1.0).abs() < 0.01,
            "alpha during hold must be 1"
        );
    }

    #[test]
    fn toast_phase_advances_to_dead() {
        let mut t = Toast::new("msg");
        // Fake the creation time to be past TOTAL.
        t.created = Instant::now() - (TOTAL + Duration::from_millis(100));
        let is_dead = t.advance();
        assert!(is_dead, "past TOTAL must be Dead");
        assert_eq!(t.phase, Phase::Dead);
    }

    #[test]
    fn push_evicts_when_at_max() {
        let mut toasts: Vec<Toast> = Vec::new();
        for i in 0..=MAX_TOASTS {
            push(&mut toasts, format!("msg {i}"));
        }
        assert_eq!(toasts.len(), MAX_TOASTS, "stack must not exceed MAX_TOASTS");
        // The first toast was evicted; "msg 1" is now at index 0.
        assert_eq!(toasts[0].message, "msg 1");
    }

    #[test]
    fn push_multiple_below_max() {
        let mut toasts: Vec<Toast> = Vec::new();
        push(&mut toasts, "a");
        push(&mut toasts, "b");
        assert_eq!(toasts.len(), 2);
    }

    #[test]
    fn next_deadline_is_none_when_empty() {
        let toasts: Vec<Toast> = Vec::new();
        assert!(next_deadline(&toasts).is_none());
    }

    #[test]
    fn next_deadline_is_some_when_alive() {
        let toasts = vec![Toast::new("hi")];
        assert!(next_deadline(&toasts).is_some());
    }

    #[test]
    fn alive_returns_true_for_fresh_toast() {
        let t = Toast::new("fresh");
        assert!(t.alive());
    }

    #[test]
    fn alive_returns_false_when_past_total() {
        let mut t = Toast::new("old");
        t.created = Instant::now() - (TOTAL + Duration::from_millis(50));
        assert!(!t.alive());
    }
}
