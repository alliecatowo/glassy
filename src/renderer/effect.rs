//! Window-effect modes (config `window_effect`), unified over the CRT post pass.
//!
//! The renderer already has a fullscreen post-process pipeline (see
//! [`crate::renderer::crt`]) that renders the grid to an offscreen scene texture
//! and composites it back through `fs_crt` in `shader.wgsl`. Historically that
//! pass only did the full "CRT" look. This module generalises it: a single
//! [`WindowEffect`] enum selects ONE shader-side effect, and each mode maps to a
//! parameter set `[curvature, scanline, glow, vignette]` plus a `mode` discriminant
//! consumed by `fs_crt`.
//!
//! Crucially the OFF mode (`None`) takes the SAME zero-cost path the CRT effect
//! always had: no offscreen target, no pipeline, no extra pass. So the 0%-idle
//! and memory benchmarks are untouched for the default build. Modes that need a
//! post pass (everything except `None`) lazily build it the first time they are
//! enabled, exactly like the original CRT toggle.
//!
//! Compositor note (recorded for reconcile): `frosted` and `acrylic` are the only
//! modes whose *full* intended look (a real gaussian blur of the desktop BEHIND
//! the window) requires compositor blur-behind (KWin "Blur", macOS
//! `NSVisualEffectView`, Windows acrylic). We cannot blur pixels we never sampled.
//! What this in-app pass CAN do — and does — is the foreground half of those
//! looks: a soft milky tint + subtle vignette + a gentle internal bloom that
//! reads as "frosted glass" over whatever the compositor shows through the
//! translucent background. With a blur-behind compositor enabled it layers
//! correctly on top; without one it still meaningfully shifts the look. The
//! purely-in-shader modes (`crt`, `scanlines`, `grain`, `vignette`, `bloom`) need
//! no compositor support at all.

use super::*;

/// The selectable window post-process effects. Exactly one is active at a time
/// (it is a mode, not a set of flags) so the post pass stays a single cheap
/// branch. `None` is the default and is a complete no-op (no offscreen pass).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WindowEffect {
    /// No post-process. Zero GPU cost — the grid draws straight to the surface.
    #[default]
    None,
    /// Frosted-glass tint + soft internal bloom + mild vignette. Pairs with a
    /// blur-behind compositor for the full look (see module note).
    Frosted,
    /// Acrylic: a cooler, slightly stronger frosted variant (Fluent-ish).
    Acrylic,
    /// Full retro CRT: barrel curvature, scanlines, aperture grille, glow,
    /// vignette. This is the historical `crt_effect = true` look.
    Crt,
    /// Scanlines only (no curvature/grille): a clean horizontal-line overlay.
    Scanlines,
    /// Film grain: a faint animated-noise dither over the image.
    Grain,
    /// Vignette only: gentle corner darkening for focus.
    Vignette,
    /// Bloom only: bright glyphs bleed a soft halo (no scanlines/curvature).
    Bloom,
    /// Custom: every channel (curvature, scanline, glow, vignette, grain, tint)
    /// is independently dialed from config sliders and stacked — the user builds
    /// any compatible combination. Its params come from config, not [`params`].
    Custom,
}

impl WindowEffect {
    /// The lowercase config / hook spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Frosted => "frosted",
            Self::Acrylic => "acrylic",
            Self::Crt => "crt",
            Self::Scanlines => "scanlines",
            Self::Grain => "grain",
            Self::Vignette => "vignette",
            Self::Bloom => "bloom",
            Self::Custom => "custom",
        }
    }

    /// Parse a config / `GLASSY_EFFECT` string. Unknown / empty → `None`. Also
    /// accepts boolean-ish spellings so `window_effect = true` reads as CRT (the
    /// closest "on" meaning) and `false` as none, easing migration from the old
    /// `crt_effect` bool.
    pub fn parse(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "false" | "no" | "0" | "" => Self::None,
            "frosted" | "frost" => Self::Frosted,
            "acrylic" => Self::Acrylic,
            "crt" | "true" | "on" | "yes" | "1" => Self::Crt,
            "scanlines" | "scanline" | "scan" => Self::Scanlines,
            "grain" | "noise" | "film" => Self::Grain,
            "vignette" | "vig" => Self::Vignette,
            "bloom" | "glow" => Self::Bloom,
            "custom" | "combo" => Self::Custom,
            _ => Self::None,
        }
    }

    /// Stable index for a segmented settings control (and back via [`from_index`]).
    pub fn index(self) -> usize {
        match self {
            Self::None => 0,
            Self::Frosted => 1,
            Self::Acrylic => 2,
            Self::Crt => 3,
            Self::Scanlines => 4,
            Self::Grain => 5,
            Self::Vignette => 6,
            Self::Bloom => 7,
            Self::Custom => 8,
        }
    }

    /// Inverse of [`index`]; out-of-range → `None`.
    pub fn from_index(i: usize) -> Self {
        match i {
            1 => Self::Frosted,
            2 => Self::Acrylic,
            3 => Self::Crt,
            4 => Self::Scanlines,
            5 => Self::Grain,
            6 => Self::Vignette,
            7 => Self::Bloom,
            8 => Self::Custom,
            _ => Self::None,
        }
    }

    /// Whether this mode requires the offscreen post pass at all. Only `None`
    /// skips it; every other mode samples the scene texture in `fs_crt`.
    pub fn needs_post(self) -> bool {
        !matches!(self, Self::None)
    }

    /// The `mode` discriminant handed to `fs_crt` (mirrors the WGSL branch ids).
    pub fn shader_mode(self) -> u32 {
        match self {
            Self::None => 0,
            Self::Crt => 1,
            Self::Frosted => 2,
            Self::Acrylic => 3,
            Self::Scanlines => 4,
            Self::Grain => 5,
            Self::Vignette => 6,
            Self::Bloom => 7,
            Self::Custom => 8,
        }
    }

    /// `[curvature, scanline, glow, vignette]` parameter set for this mode. The
    /// shader reads these alongside `shader_mode()`; a mode that doesn't use a
    /// given channel simply passes 0 for it. Tuned to stay legible (this is a real
    /// terminal): every effect is deliberately gentle.
    pub fn params(self) -> [f32; 4] {
        match self {
            // [curvature, scanline, glow, vignette]
            Self::None => [0.0, 0.0, 0.0, 0.0],
            Self::Crt => [0.10, 0.50, 0.35, 0.45],
            Self::Frosted => [0.0, 0.0, 0.18, 0.30],
            Self::Acrylic => [0.0, 0.0, 0.12, 0.40],
            Self::Scanlines => [0.0, 0.55, 0.0, 0.0],
            Self::Grain => [0.0, 0.0, 0.0, 0.0],
            Self::Vignette => [0.0, 0.0, 0.0, 0.55],
            Self::Bloom => [0.0, 0.0, 0.45, 0.0],
            // Custom's params come from config (see App::apply_window_effect);
            // the static path returns zero so a stray call is a harmless no-op.
            Self::Custom => [0.0, 0.0, 0.0, 0.0],
        }
    }

    /// The second param set `[grain, tint, reserved, reserved]` for this mode.
    /// Only `Grain` uses it among the presets (grain amplitude); `Custom` drives
    /// it from config. Everything else is zero (frosted/acrylic tint is applied
    /// by the mode branch in the shader, not this channel).
    pub fn params2(self) -> [f32; 4] {
        match self {
            // grain amplitude 0.4 → the historic ~0.06 dither (shader scales ×0.15)
            Self::Grain => [0.4, 0.0, 0.0, 0.0],
            _ => [0.0, 0.0, 0.0, 0.0],
        }
    }
}

impl Renderer {
    /// Select the active window effect at runtime. `None` reverts to the
    /// zero-cost direct-to-surface path; any other mode lazily builds the post
    /// pipeline + offscreen target (reusing the CRT machinery) and updates the
    /// per-mode params. The caller should force a full repaint after changing it.
    pub fn set_window_effect(&mut self, effect: WindowEffect) {
        if effect == self.window_effect {
            return;
        }
        self.window_effect = effect;
        // The CRT pass is the single shared post pass; route every mode through it.
        // `set_crt_mode` (re)builds resources only when post is needed and pushes
        // both the params and the mode discriminant to the uniform.
        self.set_crt_mode(
            effect.needs_post(),
            effect.shader_mode(),
            effect.params(),
            effect.params2(),
        );
    }

    /// Apply the `Custom` window effect with explicit per-channel intensities
    /// (from config sliders): `params = [curvature, scanline, glow, vignette]`,
    /// `params2 = [grain, tint, _, _]`. Routes through the same post pass as the
    /// presets so any compatible combination stacks. Force a full repaint after.
    pub fn set_window_effect_custom(&mut self, params: [f32; 4], params2: [f32; 4]) {
        self.window_effect = WindowEffect::Custom;
        self.set_crt_mode(true, WindowEffect::Custom.shader_mode(), params, params2);
    }

    /// Apply `effect`, sourcing the `Custom` mode's channels from `custom`
    /// (`[curvature, scanline, glow, vignette, grain, tint]`). The single entry
    /// point every app call site should use so `Custom` never falls back to its
    /// zero static params.
    pub fn set_window_effect_resolved(&mut self, effect: WindowEffect, custom: [f32; 6]) {
        if effect == WindowEffect::Custom {
            self.set_window_effect_custom(
                [custom[0], custom[1], custom[2], custom[3]],
                [custom[4], custom[5], 0.0, 0.0],
            );
        } else {
            self.set_window_effect(effect);
        }
    }

    /// The currently selected window effect.
    #[allow(dead_code)]
    pub fn window_effect(&self) -> WindowEffect {
        self.window_effect
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_via_str() {
        for e in [
            WindowEffect::None,
            WindowEffect::Frosted,
            WindowEffect::Acrylic,
            WindowEffect::Crt,
            WindowEffect::Scanlines,
            WindowEffect::Grain,
            WindowEffect::Vignette,
            WindowEffect::Bloom,
            WindowEffect::Custom,
        ] {
            assert_eq!(WindowEffect::parse(e.as_str()), e);
            assert_eq!(WindowEffect::from_index(e.index()), e);
        }
    }

    #[test]
    fn bool_spellings_migrate() {
        assert_eq!(WindowEffect::parse("true"), WindowEffect::Crt);
        assert_eq!(WindowEffect::parse("false"), WindowEffect::None);
        assert_eq!(WindowEffect::parse("garbage"), WindowEffect::None);
    }

    #[test]
    fn only_none_skips_post() {
        assert!(!WindowEffect::None.needs_post());
        assert!(WindowEffect::Crt.needs_post());
        assert!(WindowEffect::Frosted.needs_post());
    }

    #[test]
    fn shader_modes_are_distinct() {
        let modes: Vec<u32> = [
            WindowEffect::None,
            WindowEffect::Crt,
            WindowEffect::Frosted,
            WindowEffect::Acrylic,
            WindowEffect::Scanlines,
            WindowEffect::Grain,
            WindowEffect::Vignette,
            WindowEffect::Bloom,
            WindowEffect::Custom,
        ]
        .iter()
        .map(|e| e.shader_mode())
        .collect();
        for (i, a) in modes.iter().enumerate() {
            for b in modes.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }
}
