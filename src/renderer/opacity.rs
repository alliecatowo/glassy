//! Perceptual opacity curve.
//!
//! The window-opacity control stores a plain linear value in `[0, 1]`, but a
//! LINEAR mapping from slider position to compositor alpha wastes most of the
//! travel: a translucent terminal looks materially different across `0.85..1.0`
//! (a hair of desktop bleed-through versus a noticeable wash), yet that whole
//! band is a sliver of the slider, while `0.0..0.5` (already near-invisible
//! backgrounds nobody actually runs at) eats half of it.
//!
//! We therefore reshape the stored value through a perceptual curve before it
//! reaches the compositor: a gamma applied to the *transparency* (`1 - opacity`)
//! so the dense, high-opacity end gets the most granularity. The curve is the
//! identity at both endpoints (`0 -> 0`, `1 -> 1`) so a fully-opaque or
//! fully-clear setting is unchanged, and it is monotonic so the slider still
//! moves the right direction everywhere.
//!
//! The same curve is exposed to the settings slider (so a single drag covers
//! the perceptually-even range) and to the renderer's `glass_bg` (so the curve
//! is applied exactly once, where the alpha is consumed).

/// Perceptual gamma applied to the transparency component. >1 pushes more slider
/// travel into the high-opacity (low-transparency) band where the eye is most
/// sensitive. ~2.2 mirrors the classic sRGB-ish display gamma and feels natural.
pub(crate) const OPACITY_GAMMA: f32 = 2.2;

/// Map a stored linear opacity `v` in `[0, 1]` to the perceptual alpha that is
/// actually applied to the surface. Endpoints are fixed (`0 -> 0`, `1 -> 1`).
///
/// We gamma-shape the transparency `t = 1 - v`: `t' = t^gamma`, then return
/// `1 - t'`. With `gamma > 1` this means a small reduction from full opacity
/// (e.g. `v = 0.95`, `t = 0.05`) maps to an even *smaller* transparency
/// (`t' = 0.05^2.2 ≈ 0.0017`), so the top of the range changes gently and the
/// slider's upper third covers the subtle glass looks people actually use.
pub(crate) fn perceptual(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    let t = 1.0 - v;
    let t_curved = t.powf(OPACITY_GAMMA);
    (1.0 - t_curved).clamp(0.0, 1.0)
}

/// Inverse of [`perceptual`]: given a desired applied alpha, recover the stored
/// linear value. Used so a slider can present perceptually-even steps while the
/// config keeps storing the plain `[0, 1]` number (round-trips cleanly).
#[allow(dead_code)]
pub(crate) fn perceptual_inv(applied: f32) -> f32 {
    let applied = applied.clamp(0.0, 1.0);
    let t_curved = 1.0 - applied;
    let t = t_curved.powf(1.0 / OPACITY_GAMMA);
    (1.0 - t).clamp(0.0, 1.0)
}

/// Convert a stored linear opacity to the slider position (perceptual space) the
/// settings UI should display, so equal drags produce roughly equal visual change.
/// This is the inverse direction from [`perceptual`]: the slider lives in the
/// perceptually-even domain, the config stores the plain linear value.
pub fn opacity_to_slider(linear: f32) -> f32 {
    perceptual(linear)
}

/// Convert a slider position (perceptual space) back to the stored linear opacity.
pub fn slider_to_opacity(slider: f32) -> f32 {
    perceptual_inv(slider)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_are_fixed() {
        assert!((perceptual(0.0) - 0.0).abs() < 1e-6);
        assert!((perceptual(1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn monotonic_increasing() {
        let mut prev = -1.0;
        for i in 0..=100 {
            let v = i as f32 / 100.0;
            let p = perceptual(v);
            assert!(p >= prev, "not monotonic at v={v}: {p} < {prev}");
            prev = p;
        }
    }

    #[test]
    fn high_end_has_more_granularity() {
        // Two equal-width slider steps near the top should map to a SMALLER applied
        // delta than two equal-width steps near the bottom — that's the whole point
        // of the curve (more travel == finer control at the dense end).
        let top = perceptual(1.0) - perceptual(0.9);
        let bottom = perceptual(0.1) - perceptual(0.0);
        assert!(
            top < bottom,
            "expected finer steps near opaque end: top={top} bottom={bottom}"
        );
    }

    #[test]
    fn inverse_round_trips() {
        for i in 0..=20 {
            let v = i as f32 / 20.0;
            let back = perceptual_inv(perceptual(v));
            assert!((back - v).abs() < 1e-4, "round-trip failed at {v}: {back}");
        }
    }
}
