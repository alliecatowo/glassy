//! Power Mode: an opt-in, fun typing effect (inspired by the VSCode
//! "activate-power-mode" extension).
//!
//! When enabled, every text-producing keystroke bursts a handful of small glow
//! particles out of the terminal cursor, and rapid successive keystrokes build a
//! **streak** that intensifies the effect: more/larger/faster particles and a
//! decaying screen **shake** (the rendered grid content jitters — never the OS
//! window).
//!
//! ## Idle-safety (the 0%-idle invariant)
//!
//! glassy only repaints on damage; it does not run a continuous render loop. This
//! module mirrors [`crate::app::toast`] exactly: while any particle is alive OR
//! the shake is still settling, [`PowerState::active`] returns `true`, and the
//! event loop (`about_to_wait`) keeps requesting frames on `Poll`. The instant the
//! last particle dies and the shake decays to zero, `active` returns `false` and
//! the loop parks back on `Wait` (0% CPU). When the feature is OFF the whole
//! system is inert: no keystroke spawns anything, `active` is always `false`, and
//! the resting frame is byte-identical to a build without Power Mode.
//!
//! ## Rendering
//!
//! Particles reuse the existing overlay primitive
//! [`crate::renderer::Renderer::push_overlay_rrect_px`] (a rounded quad = a soft
//! dot) — no new GPU pipeline. Each particle fades its alpha over its lifetime.
//!
//! ## Randomness
//!
//! No wall-clock RNG seed is used: a tiny deterministic xorshift PRNG is seeded
//! from a monotonically-incrementing spawn counter mixed with the cursor cell, so
//! bursts vary but the system stays reproducible for tests/captures.

use std::time::{Duration, Instant};

use winit::event_loop::ActiveEventLoop;

use super::App;
use crate::renderer::Renderer;

/// Keystrokes closer together than this grow the streak; a longer gap decays it.
const STREAK_WINDOW: Duration = Duration::from_millis(600);
/// Streak value at (and above) which the effect is at full intensity.
const STREAK_MAX: u32 = 40;
/// Lifetime of a single particle.
const PARTICLE_LIFE: Duration = Duration::from_millis(650);
/// Hard cap on live particles so a long hold can never accumulate unbounded work.
const MAX_PARTICLES: usize = 400;
/// Base particles per burst (at streak 0); grows with the streak.
const BURST_MIN: usize = 4;
/// Additional particles per burst at full streak.
const BURST_EXTRA: usize = 10;
/// Peak screen-shake amplitude in physical px (at full streak).
const SHAKE_MAX_PX: f32 = 7.0;
/// How fast the shake amplitude decays per second (exponential-ish, applied per
/// frame as `amp *= (1 - DECAY * dt)`), clamped so it always reaches zero.
const SHAKE_DECAY_PER_S: f32 = 6.0;

/// One live particle. Positions/velocities are in physical pixels; velocity is
/// px/second so motion is frame-rate independent.
#[derive(Clone, Copy, Debug)]
struct Particle {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    /// Base radius in px.
    radius: f32,
    /// Straight RGB color (alpha is derived from remaining life each frame).
    color: [f32; 3],
    /// When this particle was spawned.
    born: Instant,
}

impl Particle {
    /// Remaining life in `[0, 1]` (1 = just born, 0 = dead).
    fn life(&self, now: Instant) -> f32 {
        let elapsed = now.saturating_duration_since(self.born).as_secs_f32();
        (1.0 - elapsed / PARTICLE_LIFE.as_secs_f32()).clamp(0.0, 1.0)
    }
}

/// Runtime Power-Mode state. Lives on the `App`; entirely dormant (no particles,
/// no shake, `active()==false`) until a keystroke spawns a burst while enabled.
pub(crate) struct PowerState {
    /// Runtime enable flag. Seeded from `config.power_mode`; flipped by the
    /// command-palette "Toggle Power Mode" entry. When false, no burst is spawned.
    enabled: bool,
    /// Effect strength in `[0, 1]` scaling particle count/size/speed + shake.
    intensity: f32,
    /// Live particles (oldest first).
    particles: Vec<Particle>,
    /// Current rapid-typing streak (grows on quick keystrokes, decays on a pause).
    streak: u32,
    /// Time of the last keystroke, for streak windowing.
    last_key: Option<Instant>,
    /// Current shake amplitude in px (decays to 0 each frame).
    shake_amp: f32,
    /// Last time [`step`](PowerState::step) advanced the simulation, for dt.
    last_step: Instant,
    /// Monotonic spawn counter feeding the deterministic PRNG seed.
    spawn_seq: u64,
}

impl PowerState {
    /// Build the initial state from the config (`enabled` + `intensity`).
    pub(crate) fn new(enabled: bool, intensity: f32) -> Self {
        PowerState {
            enabled,
            intensity: intensity.clamp(0.0, 1.0),
            particles: Vec::new(),
            streak: 0,
            last_key: None,
            shake_amp: 0.0,
            last_step: Instant::now(),
            spawn_seq: 0,
        }
    }

    /// Whether Power Mode is currently enabled (runtime flag).
    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Set the effect strength live (settings-form "Power mode intensity"
    /// slider). Clamped to `[0, 1]`, mirroring [`PowerState::new`]'s clamp.
    /// Takes effect on the NEXT burst — the mutation is safe mid-effect since
    /// `intensity` is only read at burst-spawn time.
    pub(crate) fn set_intensity(&mut self, intensity: f32) {
        self.intensity = intensity.clamp(0.0, 1.0);
    }

    /// Flip the runtime enable flag; returns the new state. When turning OFF we
    /// clear any live particles + shake so the effect stops immediately and the
    /// loop can return to idle on the next frame.
    pub(crate) fn toggle(&mut self) -> bool {
        self.enabled = !self.enabled;
        if !self.enabled {
            self.particles.clear();
            self.shake_amp = 0.0;
            self.streak = 0;
            self.last_key = None;
        }
        self.enabled
    }

    /// Whether the effect currently needs continued repaints (live particles OR a
    /// non-zero shake). Returns `false` when idle so the event loop parks on
    /// `Wait`. Cheap; safe to call every `about_to_wait`.
    pub(crate) fn active(&self) -> bool {
        !self.particles.is_empty() || self.shake_amp > 0.05
    }

    /// The current shake offset `(dx, dy)` in physical px to add to the grid
    /// origin. Deterministic from the frame's shake amplitude + a rotating seed so
    /// the jitter changes direction each frame while decaying to zero. `(0, 0)`
    /// when there is no shake (so the resting frame is unshifted).
    pub(crate) fn shake_offset(&self) -> (f32, f32) {
        if self.shake_amp <= 0.05 {
            return (0.0, 0.0);
        }
        // Two independent pseudo-random draws in [-1, 1] from the spawn seq.
        let mut s = self.spawn_seq.wrapping_mul(2_654_435_761).wrapping_add(1);
        let rx = xorshift_unit(&mut s) * 2.0 - 1.0;
        let ry = xorshift_unit(&mut s) * 2.0 - 1.0;
        (rx * self.shake_amp, ry * self.shake_amp)
    }

    /// Handle one text-producing keystroke: grow/decay the streak and spawn a
    /// particle burst at the cursor's physical-pixel rect `(cx, cy, cw, ch)`.
    /// A no-op when disabled. `accent`/`fg` seed the particle palette.
    pub(crate) fn on_keystroke(
        &mut self,
        now: Instant,
        cursor_px: (f32, f32, f32, f32),
        accent: [f32; 3],
        fg: [f32; 3],
    ) {
        if !self.enabled {
            return;
        }
        // Streak windowing: a quick follow-up grows it; a pause resets it.
        self.streak = match self.last_key {
            Some(prev) if now.saturating_duration_since(prev) <= STREAK_WINDOW => {
                (self.streak + 1).min(STREAK_MAX)
            }
            _ => 1,
        };
        self.last_key = Some(now);

        let t = (self.streak as f32 / STREAK_MAX as f32).clamp(0.0, 1.0) * self.intensity;

        // Burst size + kinetic scale grow with the streak.
        let count = BURST_MIN + (BURST_EXTRA as f32 * t).round() as usize;
        let (cx, cy, cw, ch) = cursor_px;
        // Spawn from the cursor cell center.
        let ox = cx + cw * 0.5;
        let oy = cy + ch * 0.5;

        for _ in 0..count {
            if self.particles.len() >= MAX_PARTICLES {
                self.particles.remove(0);
            }
            self.spawn_seq = self.spawn_seq.wrapping_add(1);
            let mut seed = self
                .spawn_seq
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .wrapping_add((ox as i64 as u64).wrapping_mul(2_246_822_519))
                .wrapping_add((oy as i64 as u64).wrapping_mul(3_266_489_917));
            // Random direction + speed. Speed scales with the streak so a hot
            // streak throws particles farther/faster.
            let ang = xorshift_unit(&mut seed) * std::f32::consts::TAU;
            let base_speed = 40.0 + 200.0 * t;
            let speed = base_speed * (0.4 + 0.6 * xorshift_unit(&mut seed));
            // Slight upward bias so the spray reads like sparks.
            let vx = ang.cos() * speed;
            let vy = ang.sin() * speed - 30.0 * t;
            let radius = (ch * 0.09) * (1.0 + 1.6 * t) * (0.6 + 0.8 * xorshift_unit(&mut seed));
            // Palette variety: bias toward the (colorful) accent, with a minority of
            // near-fg sparks. `mix` in [0.35, 1.0] keeps most particles vivid rather
            // than washed out toward a near-white fg.
            let mix = 0.35 + 0.65 * xorshift_unit(&mut seed);
            let color = [
                accent[0] * mix + fg[0] * (1.0 - mix),
                accent[1] * mix + fg[1] * (1.0 - mix),
                accent[2] * mix + fg[2] * (1.0 - mix),
            ];
            self.particles.push(Particle {
                x: ox,
                y: oy,
                vx,
                vy,
                radius: radius.max(0.75),
                color,
                born: now,
            });
        }

        // Bump the shake amplitude with the streak. It decays in `step`.
        let target = SHAKE_MAX_PX * t;
        self.shake_amp = self.shake_amp.max(target);
    }

    /// Integrate particle motion + decay the shake by an explicit `dt` (seconds).
    /// Shared by the wall-clock [`step`](PowerState::step) and the fixed-step
    /// harness path so both use identical physics.
    fn integrate(&mut self, dt: f32) {
        // Gravity (px/s^2) pulls particles down a touch so the spray arcs.
        const GRAVITY: f32 = 220.0;
        // Air drag so fast particles ease out rather than fly forever.
        let drag = (1.0 - 2.2 * dt).clamp(0.0, 1.0);
        for p in &mut self.particles {
            p.vx *= drag;
            p.vy = p.vy * drag + GRAVITY * dt;
            p.x += p.vx * dt;
            p.y += p.vy * dt;
        }
        // Decay the shake toward zero.
        self.shake_amp *= (1.0 - SHAKE_DECAY_PER_S * dt).clamp(0.0, 1.0);
        if self.shake_amp < 0.05 {
            self.shake_amp = 0.0;
        }
    }

    /// Advance the simulation by an explicit fixed `dt` (seconds), integrating
    /// motion + decaying the shake, then dropping any wall-clock-expired particles.
    /// Used by the scripted-input harness (`settle_cycle`), which drives frames far
    /// faster than real time — a wall-clock dt there would barely move particles.
    /// Returns `true` while the effect is still active.
    pub(crate) fn step_fixed(&mut self, dt: f32, now: Instant) -> bool {
        if !self.active() {
            return false;
        }
        self.integrate(dt);
        self.particles.retain(|p| p.life(now) > 0.0);
        self.active()
    }

    /// Advance the simulation by real elapsed time: integrate particle motion,
    /// drop dead particles, and decay the shake + streak. Returns `true` while the
    /// effect is still active (caller keeps `Poll`); `false` once fully settled.
    /// A no-op (returns `false`) when nothing is live.
    pub(crate) fn step(&mut self, now: Instant) -> bool {
        // Always advance `last_step` so a long idle gap doesn't produce a huge dt
        // when the effect next starts.
        let dt = (now.saturating_duration_since(self.last_step))
            .as_secs_f32()
            .min(0.1);
        self.last_step = now;

        if !self.active() {
            return false;
        }

        self.integrate(dt);
        // Drop expired particles.
        self.particles.retain(|p| p.life(now) > 0.0);

        // Decay the streak once the keystroke window lapses (so the NEXT burst is
        // smaller after a pause even before another key lands).
        if let Some(prev) = self.last_key
            && now.saturating_duration_since(prev) > STREAK_WINDOW
        {
            self.streak = 0;
            self.last_key = None;
        }

        self.active()
    }

    /// Paint all live particles as soft overlay dots. Coordinates are already in
    /// physical px; each dot fades its alpha with remaining life. A no-op when
    /// there are no particles (so the common resting frame does zero work).
    pub(crate) fn paint(&self, renderer: &mut Renderer, now: Instant) {
        for p in &self.particles {
            let life = p.life(now);
            if life <= 0.0 {
                continue;
            }
            // Ease alpha: bright at birth, soft fade out.
            let alpha = (life * life).clamp(0.0, 1.0);
            // Shrink slightly as it fades.
            let r = (p.radius * (0.5 + 0.5 * life)).max(0.5);
            let color = [p.color[0], p.color[1], p.color[2], alpha];
            // A fully-rounded quad (radius == half the box) draws a soft dot.
            renderer.push_overlay_rrect_px(p.x - r, p.y - r, r * 2.0, r * 2.0, r, color);
        }
    }
}

/// A tiny deterministic xorshift PRNG returning a value in `[0, 1)`. Advances the
/// caller's `state` in place. Not cryptographic — used only for cosmetic jitter,
/// so a fast, allocation-free, seedable generator is exactly what we want (and it
/// avoids pulling in the `rand` crate).
fn xorshift_unit(state: &mut u64) -> f32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    // Guard against the all-zero fixed point (would stick the generator).
    if x == 0 {
        x = 0x9E37_79B9_7F4A_7C15;
    }
    *state = x;
    // Top 24 bits → [0, 1). 24 bits is plenty of precision for an f32 in [0,1).
    ((x >> 40) as f32) / ((1u64 << 24) as f32)
}

impl App {
    /// Force Power Mode on/off at runtime (used by the headless `GLASSY_POWER`
    /// capture hook so a frame can be captured with the effect active).
    pub(crate) fn set_power_mode(&mut self, on: bool) {
        if self.power.enabled() != on {
            self.power.toggle();
        }
    }

    /// The focused terminal cursor's physical-pixel rect `(x, y, w, h)`, matching
    /// [`crate::renderer::Renderer::push_cursor`]'s pixel math (grid pad + the
    /// tab-strip `grid_origin_y` inset). Reads the LIVE cursor cell from the term so
    /// the burst spawns from where the caret actually is this instant (not the
    /// last-rendered position). Called off the keystroke path where no term lock is
    /// held, so locking here is safe (the `FairMutex` is non-reentrant). `None`
    /// before the renderer/pty exist or when the cursor is off-screen.
    fn cursor_px_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let renderer = self.renderer.as_ref()?;
        let pty = self.pty.as_ref()?;
        let m = renderer.cell_metrics();
        let pad = renderer.pad();
        // Cursor grid point + scroll offset → visible screen cell (mirrors the
        // render path's `cursor_row = cursor.point.line + display_offset`).
        let (col, screen_row) = {
            let term = pty.term.lock();
            let point = term.grid().cursor.point;
            let display_offset = term.grid().display_offset() as i32;
            (point.column.0 as i32, point.line.0 + display_offset)
        };
        if col < 0 || screen_row < 0 || col >= self.cols as i32 || screen_row >= self.rows as i32 {
            return None;
        }
        let x = col as f32 * m.width + pad;
        let y = screen_row as f32 * m.height + pad + renderer.grid_origin_y();
        Some((x, y, m.width, m.height))
    }

    /// React to a text-producing keystroke while Power Mode is on: spawn a particle
    /// burst at the cursor and grow the typing streak (which intensifies the effect
    /// and screen shake). A no-op when disabled or before the first frame. Schedules
    /// a repaint so the animation starts immediately; `about_to_wait` then keeps
    /// stepping it until it settles (0%-idle-safe).
    pub(crate) fn power_on_keystroke(&mut self, event_loop: &ActiveEventLoop) {
        if !self.power.enabled() {
            return;
        }
        let Some(rect) = self.cursor_px_rect() else {
            return;
        };
        let accent = {
            let a = crate::color::accent();
            [a[0], a[1], a[2]]
        };
        let fg = {
            let f = crate::gui::fg();
            [f[0], f[1], f[2]]
        };
        self.power.on_keystroke(Instant::now(), rect, accent, fg);
        self.mark_dirty(event_loop);
    }

    /// Toggle Power Mode at runtime (command-palette entry). Flips the flag, shows
    /// a confirming toast, and repaints so a live effect stops instantly when
    /// turned off.
    ///
    /// Also mirrors the new state into `config.power_mode` — before the
    /// settings-form "Power mode" toggle existed, `config.power_mode` was only
    /// ever READ (to seed `self.power` at startup), so this runtime toggle
    /// silently drifting from it was invisible. Now that the settings form
    /// displays `config.power_mode` and `App::save_settings` persists it, the
    /// two must stay in sync or the settings toggle would show a stale value
    /// right after a palette toggle.
    pub(crate) fn toggle_power_mode(&mut self, event_loop: &ActiveEventLoop) {
        let on = self.power.toggle();
        self.config.power_mode = on;
        self.push_toast(if on {
            "Power mode: ON — type to feel the power ✦"
        } else {
            "Power mode: OFF"
        });
        // The toast + any effect teardown are overlays not covered by terminal
        // damage; repaint so they appear/clear this frame.
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Advance the Power-Mode simulation one frame from `about_to_wait`. Returns
    /// `true` while the effect is still live (caller keeps the loop on `Poll`);
    /// `false` once it fully settles (back to `Wait`, 0% idle). When `active`, the
    /// frame is marked dirty so the particles repaint.
    pub(crate) fn step_power(&mut self, now: Instant) -> bool {
        let active = self.power.step(now);
        if active {
            self.dirty = true;
        }
        active
    }

    /// Advance the Power-Mode sim by a fixed `dt` for the scripted-input harness
    /// (`settle_cycle`), which runs frames far faster than real time. Mirrors how
    /// the harness already fixed-steps the GUI anims + cursor trail so a scripted
    /// `wait` animates the burst deterministically. Marks the frame dirty while
    /// live so the next scripted render repaints the moved particles.
    pub(crate) fn step_power_fixed(&mut self, dt: f32) -> bool {
        let active = self.power.step_fixed(dt, Instant::now());
        if active {
            self.dirty = true;
        }
        active
    }

    /// The current screen-shake offset `(dx, dy)` in physical px to add to the grid
    /// origin. `(0, 0)` when idle. Consumed by the render path.
    pub(crate) fn power_shake_offset(&self) -> (f32, f32) {
        self.power.shake_offset()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor() -> (f32, f32, f32, f32) {
        (100.0, 100.0, 8.0, 16.0)
    }

    #[test]
    fn disabled_spawns_nothing() {
        let mut p = PowerState::new(false, 0.6);
        p.on_keystroke(Instant::now(), cursor(), [1.0; 3], [1.0; 3]);
        assert!(!p.active(), "disabled power mode must stay inert");
        assert_eq!(p.shake_offset(), (0.0, 0.0));
    }

    #[test]
    fn enabled_keystroke_spawns_and_activates() {
        let mut p = PowerState::new(true, 1.0);
        p.on_keystroke(Instant::now(), cursor(), [0.2, 0.6, 1.0], [1.0; 3]);
        assert!(p.active(), "a burst must make the effect active");
    }

    #[test]
    fn toggle_off_clears_effect() {
        let mut p = PowerState::new(true, 1.0);
        p.on_keystroke(Instant::now(), cursor(), [1.0; 3], [1.0; 3]);
        assert!(p.active());
        let now_on = p.toggle();
        assert!(!now_on, "toggle from on returns off");
        assert!(!p.active(), "turning off clears particles + shake");
    }

    #[test]
    fn streak_grows_within_window_and_resets_after_pause() {
        let mut p = PowerState::new(true, 1.0);
        let t0 = Instant::now();
        p.on_keystroke(t0, cursor(), [1.0; 3], [1.0; 3]);
        assert_eq!(p.streak, 1);
        // A quick follow-up (well within STREAK_WINDOW) grows the streak.
        p.on_keystroke(t0 + Duration::from_millis(50), cursor(), [1.0; 3], [1.0; 3]);
        assert_eq!(p.streak, 2);
        // A long pause resets it back to 1 on the next key.
        p.on_keystroke(t0 + Duration::from_secs(5), cursor(), [1.0; 3], [1.0; 3]);
        assert_eq!(p.streak, 1);
    }

    #[test]
    fn particles_die_and_effect_settles() {
        let mut p = PowerState::new(true, 1.0);
        let t0 = Instant::now();
        p.on_keystroke(t0, cursor(), [1.0; 3], [1.0; 3]);
        assert!(p.active());
        // Drive the sim forward in bounded (dt-clamped) steps, as `about_to_wait`
        // does frame by frame, until it reports settled. It must reach zero within
        // a couple of seconds of simulated time (particle life + shake decay).
        let mut settled = true;
        for i in 1..=200 {
            settled = p.step(t0 + Duration::from_millis(i * 20));
            if !settled {
                break;
            }
        }
        assert!(!settled, "step must report settled once all particles die");
        assert!(!p.active(), "no live particles or shake after settling");
    }

    #[test]
    fn shake_offset_zero_when_idle() {
        let p = PowerState::new(true, 1.0);
        assert_eq!(p.shake_offset(), (0.0, 0.0));
    }

    #[test]
    fn particle_count_never_exceeds_cap() {
        let mut p = PowerState::new(true, 1.0);
        let t0 = Instant::now();
        // Hammer many bursts in one window; the cap must hold.
        for i in 0..200 {
            p.on_keystroke(t0 + Duration::from_millis(i), cursor(), [1.0; 3], [1.0; 3]);
        }
        assert!(p.particles.len() <= MAX_PARTICLES);
    }

    #[test]
    fn xorshift_stays_in_unit_range() {
        let mut s = 12345u64;
        for _ in 0..1000 {
            let v = xorshift_unit(&mut s);
            assert!((0.0..1.0).contains(&v), "xorshift out of [0,1): {v}");
        }
    }
}
