//! GPU cursor trail / smear (config `cursor_trail`, default OFF).
//!
//! When enabled, the cursor does not jump instantly between cells: its drawn
//! pixel position eases toward the target cell over a few frames, and a short
//! fading "smear" is painted along the path it is travelling. This is purely a
//! renderer-side animation driven by the existing per-frame solid-quad overlay
//! path — it pushes ordinary [`BgInstance`]s (via `push_solid`) so it needs no
//! new pipeline and composites exactly like the static cursor.
//!
//! STRICTLY idle-safe: the animation only advances while the drawn position has
//! not reached the target. The host (`app`) calls [`CursorTrail::animating`]
//! after each frame and schedules a `WaitUntil` redraw ONLY while that is true,
//! hard-stopping to `ControlFlow::Wait` (0% idle) the moment the cursor settles.
//! When the config flag is off the whole module is dormant: `set_target` snaps
//! instantly and `animating` is always false, so the default build is unchanged.

/// Per-frame animation state for the smeared cursor. All positions are in
/// physical pixels (the top-left of the cursor's cell box) so the smear can be
/// drawn between cells at sub-cell precision.
#[derive(Default)]
pub(crate) struct CursorTrail {
    /// Whether the feature is enabled (config `cursor_trail`). When false the
    /// trail never animates and `set_target` snaps instantly.
    enabled: bool,
    /// The current eased pixel position (top-left of the cursor box).
    cur: Option<[f32; 2]>,
    /// The target pixel position the cursor is easing toward.
    target: [f32; 2],
    /// Cursor box size in physical px `(w, h)`, captured from the last target so
    /// the smear quads match the cell size even across a font resize.
    size: [f32; 2],
}

impl CursorTrail {
    /// Distance (in px) below which the cursor is considered settled at its
    /// target — animation stops and the loop returns to `Wait`.
    const SETTLE_PX: f32 = 0.6;
    /// Fraction of the remaining distance covered per frame. At ~60 fps this is
    /// a brisk but visibly-smeared glide (a few frames per cell jump). Chosen so
    /// a single-cell typing advance settles in ~4-5 frames (<100 ms).
    const EASE: f32 = 0.42;
    /// Number of fading trail segments painted between the current and target
    /// positions. More segments = a longer, smoother smear.
    const SEGMENTS: usize = 5;

    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        if !enabled {
            // Drop any in-flight animation so toggling off settles immediately.
            self.cur = None;
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Point the trail at a new cursor cell origin (`x`, `y` top-left px) with
    /// the given cell box `size`. When disabled, or on the first ever target,
    /// the drawn position snaps instantly (no startup fly-in). Otherwise the
    /// eased position begins gliding toward it on subsequent `advance` calls.
    pub fn set_target(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.target = [x, y];
        self.size = [w, h];
        if !self.enabled || self.cur.is_none() {
            self.cur = Some([x, y]);
        }
    }

    /// Forget the animated position (e.g. the cursor was hidden or the grid was
    /// rebuilt at a new size). The next `set_target` snaps rather than flying in.
    pub fn reset(&mut self) {
        self.cur = None;
    }

    /// Step the eased position one frame toward the target. Returns true while
    /// the cursor is still in motion (so the host keeps scheduling frames), false
    /// once it has settled (so the host can park on `Wait`). A no-op returning
    /// false when disabled.
    pub fn advance(&mut self) -> bool {
        if !self.enabled {
            return false;
        }
        let Some(cur) = self.cur.as_mut() else {
            return false;
        };
        let dx = self.target[0] - cur[0];
        let dy = self.target[1] - cur[1];
        if dx.abs() <= Self::SETTLE_PX && dy.abs() <= Self::SETTLE_PX {
            *cur = self.target;
            return false;
        }
        cur[0] += dx * Self::EASE;
        cur[1] += dy * Self::EASE;
        true
    }

    /// Whether the cursor is currently mid-glide (drawn position != target).
    /// The host uses this to decide whether to schedule another animation frame.
    pub fn animating(&self) -> bool {
        if !self.enabled {
            return false;
        }
        match self.cur {
            Some(cur) => {
                (self.target[0] - cur[0]).abs() > Self::SETTLE_PX
                    || (self.target[1] - cur[1]).abs() > Self::SETTLE_PX
            }
            None => false,
        }
    }

    /// The fading smear quads to draw this frame: a list of
    /// `([x, y], [w, h], alpha)` rects from the eased position toward the target,
    /// each progressively fainter. Empty when disabled or settled (so nothing is
    /// drawn and the static cursor handles the resting frame). The host applies
    /// `alpha` to the cursor color and pushes each as a solid quad UNDER the
    /// crisp cursor head, producing the motion smear.
    pub fn smear(&self) -> Vec<([f32; 2], [f32; 2], f32)> {
        if !self.enabled {
            return Vec::new();
        }
        let Some(cur) = self.cur else {
            return Vec::new();
        };
        let dx = self.target[0] - cur[0];
        let dy = self.target[1] - cur[1];
        if dx.abs() <= Self::SETTLE_PX && dy.abs() <= Self::SETTLE_PX {
            return Vec::new();
        }
        let mut out = Vec::with_capacity(Self::SEGMENTS);
        for i in 0..Self::SEGMENTS {
            // t in (0,1]: 0 = at the eased head, 1 = at the target. Segments
            // closer to the target are fainter (the tail trailing behind).
            let t = (i as f32 + 1.0) / Self::SEGMENTS as f32;
            let x = cur[0] + dx * t;
            let y = cur[1] + dy * t;
            // Quadratic falloff so the tail fades smoothly to nothing.
            let alpha = (1.0 - t) * (1.0 - t) * 0.6;
            out.push(([x, y], self.size, alpha));
        }
        out
    }

    /// The current eased head position (top-left px), if animating. The host
    /// draws the crisp cursor head here instead of the snapped cell origin so
    /// the head itself glides too. `None` when disabled or settled (then the
    /// static cell-origin cursor is used unchanged).
    pub fn head(&self) -> Option<[f32; 2]> {
        if !self.enabled {
            return None;
        }
        let cur = self.cur?;
        let dx = self.target[0] - cur[0];
        let dy = self.target[1] - cur[1];
        if dx.abs() <= Self::SETTLE_PX && dy.abs() <= Self::SETTLE_PX {
            None
        } else {
            Some(cur)
        }
    }
}

use super::Renderer;

impl Renderer {
    /// Enable or disable the cursor trail/smear (config `cursor_trail`). When
    /// off the trail never animates and cursor moves snap instantly, so the
    /// default build is unchanged. The caller should repaint after toggling.
    pub fn set_cursor_trail(&mut self, enabled: bool) {
        self.cursor_trail.set_enabled(enabled);
    }

    /// Whether the cursor trail is enabled.
    pub fn cursor_trail_enabled(&self) -> bool {
        self.cursor_trail.enabled()
    }

    /// Point the trail at the cursor cell `(col, row)`: computes the cell's
    /// pixel origin (matching `push_cursor`) and eases toward it on subsequent
    /// frames. Call once per frame with the live cursor cell BEFORE
    /// `push_cursor_smear`. A no-op visual effect when the trail is disabled.
    pub fn aim_cursor_trail(&mut self, col: usize, row: usize) {
        let cell_w = self.metrics.width;
        let cell_h = self.metrics.height;
        let ox = (col as f32 * cell_w + self.pad + self.grid_origin_x).round();
        let oy = (row as f32 * cell_h + self.pad + self.grid_origin_y).round();
        self.cursor_trail
            .set_target(ox, oy, cell_w.round(), cell_h.round());
    }

    /// Forget the trail's animated position (cursor hidden / grid rebuilt at a
    /// new size). The next `aim_cursor_trail` snaps instead of flying in.
    pub fn reset_cursor_trail(&mut self) {
        self.cursor_trail.reset();
    }

    /// Advance the trail one frame. Returns true while the cursor is still
    /// gliding (the host keeps scheduling redraws); false once settled (the
    /// host parks on `Wait`, 0% idle). Always false when disabled.
    pub fn step_cursor_trail(&mut self) -> bool {
        self.cursor_trail.advance()
    }

    /// Whether the cursor is mid-glide right now (host scheduling hint).
    pub fn cursor_trail_animating(&self) -> bool {
        self.cursor_trail.animating()
    }

    /// Push the fading smear quads for this frame, in the cursor `color`, as
    /// translucent OVERLAY quads (premultiplied blend) so they fade over the
    /// terminal content rather than overwriting it (the bg pass does not blend).
    /// Each segment is a faded rect along the cursor's travel path. A no-op when
    /// the trail is disabled or settled. The crisp cursor head is still drawn via
    /// `push_cursor`; the gliding body block is drawn from `cursor_trail_head`.
    pub fn push_cursor_smear(&mut self, color: [f32; 4]) {
        let smear = self.cursor_trail.smear();
        for (pos, size, alpha) in smear {
            let c = [color[0], color[1], color[2], color[3] * alpha];
            self.push_overlay_px(pos[0], pos[1], size[0], size[1], c);
        }
    }

    /// Push a soft, translucent full-cell "head" block at the eased cursor
    /// position (overlay/premultiplied blend) so the cursor body visibly chases
    /// its target cell while gliding. `alpha` scales the cursor color. A no-op
    /// when settled / disabled (the static cursor handles the resting frame).
    pub fn push_cursor_head(&mut self, color: [f32; 4], alpha: f32) {
        if let Some([hx, hy]) = self.cursor_trail.head() {
            let (hw, hh) = self.cursor_cell_px();
            let c = [color[0], color[1], color[2], color[3] * alpha];
            self.push_overlay_px(hx, hy, hw, hh, c);
        }
    }

    /// Cursor box size in physical px `(w, h)` for drawing a smear/head quad —
    /// the rounded cell metrics, matching `push_cursor`.
    fn cursor_cell_px(&self) -> (f32, f32) {
        (self.metrics.width.round(), self.metrics.height.round())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_never_animates() {
        let mut t = CursorTrail::default();
        t.set_target(0.0, 0.0, 10.0, 20.0);
        t.set_target(100.0, 0.0, 10.0, 20.0);
        assert!(!t.animating());
        assert!(!t.advance());
        assert!(t.smear().is_empty());
        assert!(t.head().is_none());
    }

    #[test]
    fn first_target_snaps() {
        let mut t = CursorTrail::default();
        t.set_enabled(true);
        t.set_target(50.0, 50.0, 10.0, 20.0);
        // No previous position, so the first target snaps (no startup fly-in).
        assert!(!t.animating());
        assert!(t.head().is_none());
    }

    #[test]
    fn glides_then_settles() {
        let mut t = CursorTrail::default();
        t.set_enabled(true);
        t.set_target(0.0, 0.0, 10.0, 20.0); // snap to origin
        t.set_target(100.0, 0.0, 10.0, 20.0); // now jump far away
        assert!(t.animating(), "should animate after a far jump");
        assert!(!t.smear().is_empty());
        assert!(t.head().is_some());
        // A bounded number of frames must settle it (geometric decay).
        let mut frames = 0;
        while t.advance() {
            frames += 1;
            assert!(frames < 200, "trail must settle in finite frames");
        }
        assert!(!t.animating());
        assert!(t.smear().is_empty());
    }

    #[test]
    fn disabling_mid_flight_settles() {
        let mut t = CursorTrail::default();
        t.set_enabled(true);
        t.set_target(0.0, 0.0, 10.0, 20.0);
        t.set_target(100.0, 0.0, 10.0, 20.0);
        assert!(t.animating());
        t.set_enabled(false);
        assert!(!t.animating());
        assert!(!t.advance());
    }

    #[test]
    fn smear_alpha_fades_toward_target() {
        let mut t = CursorTrail::default();
        t.set_enabled(true);
        t.set_target(0.0, 0.0, 10.0, 20.0);
        t.set_target(200.0, 0.0, 10.0, 20.0);
        let smear = t.smear();
        assert_eq!(smear.len(), CursorTrail::SEGMENTS);
        // Alphas strictly decrease from head to tail.
        for w in smear.windows(2) {
            assert!(w[0].2 >= w[1].2);
        }
        // Every alpha is in a sane premultiply-friendly range.
        for (_, _, a) in &smear {
            assert!(*a >= 0.0 && *a <= 1.0);
        }
    }
}
