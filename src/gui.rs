//! Lightweight immediate-mode GUI core for glassy's real-chrome layer.
//!
//! The terminal CONTENT stays the fast, damage-tracked cell grid. The CHROME
//! (tab bar, pane headers, status bar, settings, menus, help) is drawn here as
//! pixel-positioned overlay quads + antialiased rounded rects + pixel-positioned
//! glyphs, composited in the two overlay passes that already run last in the
//! renderer (`overlay_quads` then `overlay_text`).
//!
//! This module owns NO GPU state. It emits the three renderer primitives
//! (`push_overlay_px`, `push_overlay_rrect_px`, `push_overlay_glyph_px`) and
//! returns interaction results; the persistent interaction state (pressed /
//! focused id, animation map) lives in `App` and is threaded in per frame. There
//! are zero new dependencies and zero new pipelines/bind-groups/buffer-types.
//!
//! Idle stays at 0% CPU: an [`Anim`] only steps while it is unsettled, and `App`
//! requests `ControlFlow::Poll` solely while some `Anim` is in flight.

use std::collections::HashMap;

use crate::color;
use crate::renderer::Renderer;

// ---------------------------------------------------------------------------
// Geometry + hit-testing
// ---------------------------------------------------------------------------

/// An axis-aligned rectangle in physical pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }
    /// Shrink the rect inward by `p` on every side (clamped at zero size).
    pub fn inset(self, p: f32) -> Self {
        Rect {
            x: self.x + p,
            y: self.y + p,
            w: (self.w - 2.0 * p).max(0.0),
            h: (self.h - 2.0 * p).max(0.0),
        }
    }
    pub fn center_y(self) -> f32 {
        self.y + self.h * 0.5
    }
}

/// True when `(px, py)` lies inside `r` (left/top inclusive, right/bottom
/// exclusive — matches half-open pixel coverage).
pub fn hit(r: Rect, px: f32, py: f32) -> bool {
    px >= r.x && px < r.x + r.w && py >= r.y && py < r.y + r.h
}

// ---------------------------------------------------------------------------
// Widget identity
// ---------------------------------------------------------------------------

/// A stable per-widget id. Derived from a `&'static str` declaration path (e.g.
/// `"settings/opacity"`) via FNV-1a, so widget identity is position-independent
/// and survives layout reflow across frames.
pub type WidgetId = u64;

/// FNV-1a hash of a string path → [`WidgetId`]. Cheap and allocation-free.
pub fn id(path: &str) -> WidgetId {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// The visual interaction state of a widget for the state→style rule (§2.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WState {
    Idle,
    Hover,
    Press,
    Focus,
    Disabled,
}

// ---------------------------------------------------------------------------
// Animation
// ---------------------------------------------------------------------------

/// A single change-triggered scalar animation (e.g. a hover fade). `value`
/// chases `target`; [`Anim::step`] advances it and reports whether it has
/// settled. Only unsettled anims keep the event loop on `Poll`.
#[derive(Clone, Copy, Debug)]
pub struct Anim {
    pub value: f32,
    pub target: f32,
}

impl Anim {
    pub fn new(start: f32) -> Self {
        Anim { value: start, target: start }
    }

    /// Lerp `value` toward `target` by `rate` per second over `dt` seconds.
    /// Snaps when within an imperceptible epsilon. Returns `true` once settled
    /// (so the caller can drop the entry / drop back to `ControlFlow::Wait`).
    pub fn step(&mut self, dt: f32, rate: f32) -> bool {
        let t = (dt * rate).clamp(0.0, 1.0);
        self.value += (self.target - self.value) * t;
        if (self.target - self.value).abs() < 0.004 {
            self.value = self.target;
            true
        } else {
            false
        }
    }

    pub fn is_settled(&self) -> bool {
        (self.target - self.value).abs() < 0.004
    }
}

/// Step every animation in `anims` and return `true` if ANY is still unsettled
/// (the App uses this to choose `Poll` vs `Wait`). Settled entries are retained
/// so their resting value is still readable; they simply cost nothing to step.
pub fn step_anims(anims: &mut HashMap<WidgetId, Anim>, dt: f32, rate: f32) -> bool {
    let mut unsettled = false;
    for a in anims.values_mut() {
        if !a.step(dt, rate) {
            unsettled = true;
        }
    }
    unsettled
}

/// True if any animation in the map has not yet settled (no stepping).
pub fn any_unsettled(anims: &HashMap<WidgetId, Anim>) -> bool {
    anims.values().any(|a| !a.is_settled())
}

// ---------------------------------------------------------------------------
// Frosted-glass tokens (theme-derived, §2.1)
// ---------------------------------------------------------------------------

/// E1 chrome-bar alpha (tab bar, status bar) — matches the existing modal alpha.
pub const GLASS_BAR_ALPHA: f32 = 0.82;
/// E2 surface alpha (pane bodies, active-tab body, cards).
pub const GLASS_SURFACE_ALPHA: f32 = 0.92;
/// E3 floating alpha (dropdowns, dialogs, drag-ghost).
pub const GLASS_FLOAT_ALPHA: f32 = 0.96;

fn with_alpha(mut c: [f32; 4], a: f32) -> [f32; 4] {
    c[3] = a;
    c
}

fn lighten(c: [f32; 4], amount: f32) -> [f32; 4] {
    [
        (c[0] + amount).min(1.0),
        (c[1] + amount).min(1.0),
        (c[2] + amount).min(1.0),
        c[3],
    ]
}

fn darken(c: [f32; 4], f: f32) -> [f32; 4] {
    [c[0] * f, c[1] * f, c[2] * f, c[3]]
}

fn luma(c: [f32; 4]) -> f32 {
    0.299 * c[0] + 0.587 * c[1] + 0.114 * c[2]
}

/// E1 chrome bar fill.
pub fn glass_body() -> [f32; 4] {
    with_alpha(color::default_bg(), GLASS_BAR_ALPHA)
}

/// E2 raised surface fill (cards / buttons on glass).
pub fn glass_raised() -> [f32; 4] {
    with_alpha(lighten(color::default_bg(), 0.06), GLASS_SURFACE_ALPHA)
}

/// E3 floating surface fill (dropdowns / dialogs / drag-ghost).
pub fn glass_float() -> [f32; 4] {
    with_alpha(lighten(color::selection_bg(), 0.12), GLASS_FLOAT_ALPHA)
}

/// Edge-lit accent rail (top edge of raised surfaces).
pub fn rail() -> [f32; 4] {
    with_alpha(color::accent(), 0.60)
}

/// Shadow-side hairline (bottom / right edge of raised surfaces).
pub fn hairline() -> [f32; 4] {
    with_alpha(darken(color::default_bg(), 0.6), 0.50)
}

/// Off-state control track.
pub fn track_off() -> [f32; 4] {
    with_alpha(color::default_bg(), 0.55)
}

/// On-state control fill.
pub fn fill_on() -> [f32; 4] {
    color::accent()
}

/// List / menu row highlight.
pub fn sel_bg() -> [f32; 4] {
    with_alpha(color::selection_bg(), 0.85)
}

/// Primary foreground (labels).
pub fn fg() -> [f32; 4] {
    color::default_fg()
}

/// Dimmed foreground (secondary labels, shortcut hints).
pub fn fg_dim() -> [f32; 4] {
    with_alpha(color::default_fg(), 0.55)
}

/// Danger / destructive accent.
pub fn danger() -> [f32; 4] {
    color::danger()
}

/// State-driven fill for an interactive surface, following the §2.4 rule. The
/// `hover_t` is the eased hover animation value (0..1); press is instant.
pub fn state_fill(base: [f32; 4], hover_t: f32, pressed: bool) -> [f32; 4] {
    if pressed {
        return darken(base, 0.85);
    }
    if hover_t <= 0.0 {
        return base;
    }
    // On near-white surfaces, lightening does nothing — darken to keep the hover
    // perceptible (mirrors the app's active_chip_bg reasoning).
    let target = if luma(base) > 0.7 {
        darken(base, 0.92)
    } else {
        lighten(base, 0.06)
    };
    [
        base[0] + (target[0] - base[0]) * hover_t,
        base[1] + (target[1] - base[1]) * hover_t,
        base[2] + (target[2] - base[2]) * hover_t,
        base[3] + (target[3] - base[3]) * hover_t,
    ]
}

// ---------------------------------------------------------------------------
// Metric scale (derived from cell_h so chrome scales with the font)
// ---------------------------------------------------------------------------

/// Physical-pixel metrics for the GUI layer, derived from the cell size so the
/// chrome scales with the font exactly like the existing `pad_for`.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub cell_w: f32,
    pub cell_h: f32,
    pub row_h: f32,
    pub pad: f32,
    pub gap: f32,
    pub radius: f32,
    pub card_radius: f32,
    pub ctrl_w: f32,
    pub knob: f32,
}

impl Metrics {
    pub fn new(cell_w: f32, cell_h: f32) -> Self {
        let row_h = (cell_h * 1.6).round();
        Metrics {
            cell_w,
            cell_h,
            row_h,
            pad: (cell_h * 0.5).round(),
            gap: (cell_h * 0.4).round(),
            radius: (cell_h * 0.28).round().clamp(4.0, 8.0),
            card_radius: (cell_h * 0.28).round().clamp(4.0, 8.0) + 2.0,
            ctrl_w: (cell_w * 14.0).round(),
            knob: row_h - 8.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Elevation primitives (edge-lit surfaces)
// ---------------------------------------------------------------------------

/// Result of one widget interaction this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Interaction {
    pub hovered: bool,
    pub pressed: bool,
    /// A full press→release click landed on this widget this frame.
    pub clicked: bool,
    /// The widget's value changed this frame (sliders/toggles/etc.).
    pub changed: bool,
}

// ---------------------------------------------------------------------------
// Immediate-mode Ui
// ---------------------------------------------------------------------------

/// The per-frame immediate-mode context. Borrows the renderer (to emit
/// primitives) plus the App-owned persistent interaction state (pressed /
/// focused id, animation map). Construct it once per chrome paint, call the
/// component methods, then drop it.
pub struct Ui<'r> {
    pub r: &'r mut Renderer,
    pub m: Metrics,
    mouse: (f32, f32),
    mouse_down: bool,
    /// Press→release edge observed this frame (set by App from MouseInput).
    clicked: bool,
    hovered: Option<WidgetId>,
    pressed: &'r mut Option<WidgetId>,
    focused: &'r mut Option<WidgetId>,
    tab_order: Vec<WidgetId>,
    anims: &'r mut HashMap<WidgetId, Anim>,
}

impl<'r> Ui<'r> {
    /// Begin a chrome paint frame. `mouse` is the cursor in physical px,
    /// `mouse_down` the current left-button state, `clicked` the press→release
    /// edge captured this frame by the App's MouseInput handler.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        r: &'r mut Renderer,
        cell_w: f32,
        cell_h: f32,
        mouse: (f32, f32),
        mouse_down: bool,
        clicked: bool,
        pressed: &'r mut Option<WidgetId>,
        focused: &'r mut Option<WidgetId>,
        anims: &'r mut HashMap<WidgetId, Anim>,
    ) -> Self {
        let m = Metrics::new(cell_w, cell_h);
        Ui {
            r,
            m,
            mouse,
            mouse_down,
            clicked,
            hovered: None,
            pressed,
            focused,
            tab_order: Vec::new(),
            anims,
        }
    }

    /// The collected keyboard tab order (declaration order) for this frame.
    pub fn tab_order(&self) -> &[WidgetId] {
        &self.tab_order
    }

    fn anim(&mut self, wid: WidgetId, target: f32) -> f32 {
        let a = self.anims.entry(wid).or_insert_with(|| Anim::new(target));
        a.target = target;
        a.value
    }

    /// Core hit/interaction resolution for a clickable widget rect. Records the
    /// widget in the tab order, updates pressed/hovered, and returns the result.
    fn interact(&mut self, wid: WidgetId, rect: Rect, enabled: bool) -> Interaction {
        self.tab_order.push(wid);
        if !enabled {
            return Interaction::default();
        }
        let over = hit(rect, self.mouse.0, self.mouse.1);
        if over {
            self.hovered = Some(wid);
        }
        // Press latch: claim the widget on button-down over it.
        if over && self.mouse_down && self.pressed.is_none() {
            *self.pressed = Some(wid);
            *self.focused = Some(wid);
        }
        let pressed = *self.pressed == Some(wid) && self.mouse_down;
        let clicked = self.clicked && over && *self.pressed == Some(wid);
        Interaction {
            hovered: over,
            pressed,
            clicked,
            changed: false,
        }
    }

    fn wstate(&self, wid: WidgetId, it: &Interaction, enabled: bool) -> WState {
        if !enabled {
            WState::Disabled
        } else if it.pressed {
            WState::Press
        } else if it.hovered {
            WState::Hover
        } else if *self.focused == Some(wid) {
            WState::Focus
        } else {
            WState::Idle
        }
    }

    // -- low-level emit helpers -------------------------------------------

    fn quad(&mut self, r: Rect, color: [f32; 4]) {
        self.r.push_overlay_px(r.x, r.y, r.w, r.h, color);
    }

    fn rrect(&mut self, r: Rect, radius: f32, color: [f32; 4]) {
        self.r.push_overlay_rrect_px(r.x, r.y, r.w, r.h, radius, color);
    }

    /// Edge-lit signature: a 1px `rail` on the TOP edge + a 1px `hairline` on the
    /// BOTTOM edge of a raised surface (two quads), reading as a beveled pane.
    fn edge_light(&mut self, r: Rect) {
        self.quad(Rect::new(r.x, r.y, r.w, 1.0), rail());
        self.quad(Rect::new(r.x, r.y + r.h - 1.0, r.w, 1.0), hairline());
    }

    /// A 1px accent border drawn as four thin rrects — the keyboard-focus ring,
    /// visible even when not hovered.
    fn focus_ring(&mut self, r: Rect, radius: f32) {
        let c = color::accent();
        self.rrect(Rect::new(r.x, r.y, r.w, 1.0), 0.0, c);
        self.rrect(Rect::new(r.x, r.y + r.h - 1.0, r.w, 1.0), 0.0, c);
        self.rrect(Rect::new(r.x, r.y, 1.0, r.h), 0.0, c);
        self.rrect(Rect::new(r.x + r.w - 1.0, r.y, 1.0, r.h), 0.0, c);
        let _ = radius;
    }

    // -- text -------------------------------------------------------------

    /// Width in px of `s` in the panel font (monospace, exact).
    pub fn text_width(&self, s: &str) -> f32 {
        self.r.text_width_px(s)
    }

    /// Draw `text` left-aligned with its cell-box top at `(x, y)`.
    pub fn label(&mut self, x: f32, y: f32, text: &str, color: [f32; 4]) {
        let mut cx = x;
        for ch in text.chars() {
            self.r.push_overlay_glyph_px(cx, y, ch, color);
            cx += self.m.cell_w;
        }
    }

    /// Draw `text` so its right edge ends at `x_right`, top at `y`.
    pub fn label_right(&mut self, x_right: f32, y: f32, text: &str, color: [f32; 4]) {
        let w = self.text_width(text);
        self.label(x_right - w, y, text, color);
    }

    /// Draw `text` centered horizontally in `[x, x+w)`, vertically within `h`.
    pub fn label_centered(&mut self, rect: Rect, text: &str, color: [f32; 4]) {
        let tw = self.text_width(text);
        let tx = rect.x + (rect.w - tw) * 0.5;
        let ty = rect.center_y() - self.m.cell_h * 0.5;
        self.label(tx.round(), ty.round(), text, color);
    }

    // -- containers -------------------------------------------------------

    /// A raised surface panel with a left accent rail (E2). Returns the inner
    /// content rect (inset by `pad`).
    pub fn panel(&mut self, rect: Rect, radius: f32) -> Rect {
        self.rrect(rect, radius, glass_raised());
        // Left accent rail.
        self.quad(Rect::new(rect.x, rect.y, 1.0, rect.h), rail());
        rect.inset(self.m.pad)
    }

    /// A lighter card surface on glass (E2), no rail.
    pub fn card(&mut self, rect: Rect, radius: f32) {
        self.rrect(rect, radius, lighten(glass_raised(), 0.04));
        self.edge_light(rect);
    }

    /// A thin separator line at `(x, y)` of width `w`.
    pub fn separator(&mut self, x: f32, y: f32, w: f32) {
        self.quad(Rect::new(x, y, w, 1.0), hairline());
    }

    // -- controls ---------------------------------------------------------

    /// A labelled push button. Returns its interaction.
    pub fn button(&mut self, wid: WidgetId, rect: Rect, text: &str) -> Interaction {
        let it = self.interact(wid, rect, true);
        let st = self.wstate(wid, &it, true);
        let hover_t = self.anim(wid, if matches!(st, WState::Hover | WState::Press) { 1.0 } else { 0.0 });
        let fill = state_fill(glass_raised(), hover_t, it.pressed);
        self.rrect(rect, self.m.radius, fill);
        if hover_t > 0.0 && !it.pressed {
            self.quad(Rect::new(rect.x, rect.y, rect.w, 1.0), rail());
        }
        if matches!(st, WState::Focus) {
            self.focus_ring(rect, self.m.radius);
        }
        let nudge = if it.pressed { 1.0 } else { 0.0 };
        let mut content = rect;
        content.y += nudge;
        self.label_centered(content, text, fg());
        it
    }

    /// An icon button (single glyph). Returns its interaction.
    pub fn icon_button(&mut self, wid: WidgetId, rect: Rect, glyph: char) -> Interaction {
        let it = self.interact(wid, rect, true);
        let st = self.wstate(wid, &it, true);
        let hover_t = self.anim(wid, if matches!(st, WState::Hover | WState::Press) { 1.0 } else { 0.0 });
        if hover_t > 0.0 || it.pressed {
            let fill = state_fill(glass_raised(), hover_t, it.pressed);
            self.rrect(rect, self.m.radius, fill);
        }
        if matches!(st, WState::Focus) {
            self.focus_ring(rect, self.m.radius);
        }
        let nudge = if it.pressed { 1.0 } else { 0.0 };
        let cx = rect.x + (rect.w - self.m.cell_w) * 0.5;
        let cy = rect.center_y() - self.m.cell_h * 0.5 + nudge;
        self.r.push_overlay_glyph_px(cx.round(), cy.round(), glyph, fg());
        it
    }

    /// A toggle switch. Returns the (possibly flipped) value.
    pub fn toggle(&mut self, wid: WidgetId, rect: Rect, value: bool) -> bool {
        let it = self.interact(wid, rect, true);
        let mut v = value;
        if it.clicked {
            v = !v;
        }
        let on_t = self.anim(wid, if v { 1.0 } else { 0.0 });
        let track = if v {
            // blend track_off -> fill_on by on_t
            let a = track_off();
            let b = fill_on();
            [
                a[0] + (b[0] - a[0]) * on_t,
                a[1] + (b[1] - a[1]) * on_t,
                a[2] + (b[2] - a[2]) * on_t,
                a[3] + (b[3] - a[3]) * on_t,
            ]
        } else {
            track_off()
        };
        let rr = rect.h * 0.5;
        self.rrect(rect, rr, track);
        if *self.focused == Some(wid) {
            self.focus_ring(rect, rr);
        }
        // Knob.
        let pad = 2.0;
        let k = rect.h - 2.0 * pad;
        let kx = rect.x + pad + (rect.w - 2.0 * pad - k) * on_t;
        self.rrect(Rect::new(kx, rect.y + pad, k, k), k * 0.5, fg());
        v
    }

    /// A segmented control (radio row). Returns the selected index.
    pub fn segmented(&mut self, wid: WidgetId, rect: Rect, options: &[&str], sel: usize) -> usize {
        let n = options.len().max(1);
        self.rrect(rect, self.m.radius, track_off());
        let seg_w = rect.w / n as f32;
        let mut chosen = sel;
        for (i, opt) in options.iter().enumerate() {
            let seg = Rect::new(rect.x + seg_w * i as f32, rect.y, seg_w, rect.h);
            let seg_id = id_combine(wid, i as u64);
            let it = self.interact(seg_id, seg, true);
            if it.clicked {
                chosen = i;
            }
            if i == sel {
                self.rrect(seg.inset(2.0), self.m.radius - 1.0, fill_on());
            } else if it.hovered {
                self.rrect(seg.inset(2.0), self.m.radius - 1.0, state_fill(track_off(), 1.0, false));
            }
            let tc = if i == sel { color::default_bg() } else { fg() };
            self.label_centered(seg, opt, tc);
        }
        if *self.focused == Some(wid) {
            self.focus_ring(rect, self.m.radius);
        }
        chosen
    }

    /// A horizontal slider. Returns the (possibly dragged) value, snapped to
    /// `step` and clamped to `[min, max]`.
    pub fn slider(&mut self, wid: WidgetId, rect: Rect, value: f32, min: f32, max: f32, step: f32) -> f32 {
        let it = self.interact(wid, rect, true);
        let mut v = value.clamp(min, max);
        if it.pressed && max > min {
            let t = ((self.mouse.0 - rect.x) / rect.w).clamp(0.0, 1.0);
            let raw = min + t * (max - min);
            v = if step > 0.0 {
                (raw / step).round() * step
            } else {
                raw
            }
            .clamp(min, max);
        }
        let t = if max > min { (v - min) / (max - min) } else { 0.0 };
        // Track.
        let mid = rect.center_y();
        let th = 4.0;
        let track = Rect::new(rect.x, mid - th * 0.5, rect.w, th);
        self.rrect(track, th * 0.5, track_off());
        // Filled portion.
        self.rrect(Rect::new(rect.x, mid - th * 0.5, rect.w * t, th), th * 0.5, fill_on());
        // Knob.
        let k = rect.h * 0.6;
        let kx = rect.x + rect.w * t - k * 0.5;
        self.rrect(Rect::new(kx, mid - k * 0.5, k, k), k * 0.5, fg());
        if *self.focused == Some(wid) {
            self.focus_ring(rect, rect.h * 0.5);
        }
        v
    }

    /// A `[− value +]` stepper. Returns the delta to apply (-1, 0, or +1 step
    /// clicks), letting the caller drive its own live effect. `text` is the
    /// rendered value between the buttons.
    pub fn stepper(&mut self, wid: WidgetId, rect: Rect, text: &str) -> i32 {
        let bw = rect.h;
        let dec = Rect::new(rect.x, rect.y, bw, rect.h);
        let inc = Rect::new(rect.x + rect.w - bw, rect.y, bw, rect.h);
        let mid = Rect::new(rect.x + bw, rect.y, rect.w - 2.0 * bw, rect.h);
        let d_it = self.button(id_combine(wid, 1), dec, "−");
        let i_it = self.button(id_combine(wid, 2), inc, "+");
        self.rrect(mid, self.m.radius, track_off());
        self.label_centered(mid, text, fg());
        if i_it.clicked {
            1
        } else if d_it.clicked {
            -1
        } else {
            0
        }
    }
}

/// Combine a base widget id with a sub-index (segments / stepper buttons).
fn id_combine(base: WidgetId, sub: u64) -> WidgetId {
    let mut h = base ^ sub.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_is_half_open() {
        let r = Rect::new(10.0, 10.0, 20.0, 20.0);
        assert!(hit(r, 10.0, 10.0));
        assert!(hit(r, 29.9, 29.9));
        assert!(!hit(r, 30.0, 20.0)); // right edge exclusive
        assert!(!hit(r, 20.0, 30.0)); // bottom edge exclusive
        assert!(!hit(r, 9.0, 20.0));
    }

    #[test]
    fn id_is_stable_and_distinct() {
        assert_eq!(id("settings/opacity"), id("settings/opacity"));
        assert_ne!(id("settings/opacity"), id("settings/font"));
    }

    #[test]
    fn anim_settles() {
        let mut a = Anim::new(0.0);
        a.target = 1.0;
        let mut steps = 0;
        while !a.step(0.016, 12.0) && steps < 1000 {
            steps += 1;
        }
        assert!(a.is_settled());
        assert!((a.value - 1.0).abs() < 0.01);
        assert!(steps < 1000);
    }

    #[test]
    fn rect_inset_clamps() {
        let r = Rect::new(0.0, 0.0, 4.0, 4.0).inset(10.0);
        assert_eq!(r.w, 0.0);
        assert_eq!(r.h, 0.0);
    }
}
