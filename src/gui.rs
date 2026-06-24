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
#[derive(Clone, Copy, Debug, Default, PartialEq)]
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

/// What a [`Ui::dropdown`] reported this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DropdownEvt {
    /// No interaction.
    #[default]
    None,
    /// The header was clicked — the caller should flip the open/closed state.
    Toggle,
}

/// What a [`Ui::list`] reported this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ListEvt {
    /// No interaction.
    #[default]
    None,
    /// Row `usize` (absolute index) was clicked.
    Clicked(usize),
    /// Row `usize` (absolute index) is hovered (no click).
    Hovered(usize),
}

/// What a [`Ui::text_field_readonly`] reported this frame (trailing icons).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum FieldEvt {
    /// No interaction.
    #[default]
    None,
    /// The copy (`⧉`) icon was clicked.
    Copy,
    /// The open (`↗`) icon was clicked.
    Open,
}

// ---------------------------------------------------------------------------
// Settings form (§3.5)
// ---------------------------------------------------------------------------

/// Which settings dropdown is currently expanded (only one at a time). Owned by
/// the App across frames so the popup list survives between paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SettingsDrop {
    /// No dropdown open.
    #[default]
    None,
    /// The theme chooser list is open.
    Theme,
    /// The font-family chooser list is open.
    Font,
}

/// The live, read-only view of state the settings form draws from. The App
/// fills this each frame from its `Config` + renderer; `build_settings` never
/// mutates it (all changes flow back through [`SettingsEvents`]).
pub struct SettingsView<'a> {
    pub font_px: f32,
    pub opacity: f32,
    /// Bell mode: 0 = Off, 1 = Visual, 2 = Audible.
    pub bell: usize,
    /// Index of the active theme within [`color::THEME_NAMES`].
    pub theme_idx: usize,
    pub theme_names: &'a [&'a str],
    /// Theme preview swatch colors, parallel to `theme_names`.
    pub theme_swatches: &'a [[f32; 4]],
    pub font_family: &'a str,
    pub font_names: &'a [&'a str],
    pub font_idx: usize,
    pub scrollback: usize,
    pub config_path: &'a str,
    /// Which dropdown (if any) is currently expanded.
    pub open: SettingsDrop,
    /// Show the transient "✓ saved" footer label.
    pub saved: bool,
}

/// Everything the settings form reported this frame. The App applies each
/// non-default field to its live effects (`resize_font`, `set_opacity`,
/// `cycle_theme`/`set_theme`, `save_settings`, …).
#[derive(Clone, Copy, Debug, Default)]
pub struct SettingsEvents {
    /// Font-size stepper delta in clicks (-1 / 0 / +1).
    pub font_delta: i32,
    /// New opacity value if the slider moved this frame.
    pub opacity: Option<f32>,
    /// New bell mode index if the segmented control changed.
    pub bell: Option<usize>,
    /// The user toggled the theme dropdown header (App flips `open`).
    pub theme_toggle: bool,
    /// A theme row was picked (absolute index into `theme_names`).
    pub theme_pick: Option<usize>,
    /// The user toggled the font dropdown header.
    pub font_toggle: bool,
    /// A font row was picked (absolute index into `font_names`).
    pub font_pick: Option<usize>,
    /// Scrollback stepper delta in clicks (-1 / 0 / +1).
    pub scrollback_delta: i32,
    /// Copy the config path to the clipboard.
    pub copy_path: bool,
    /// Open the config path in the user's editor.
    pub open_path: bool,
    /// The Save button (or Enter) fired.
    pub save: bool,
    /// The Close button (or the ✕) fired.
    pub close: bool,
    /// The bounding rect of the whole panel (for click-outside dismissal).
    pub panel: Rect,
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

    /// A dropdown header (the always-visible chooser button). Renders the current
    /// `label`, an optional left color `swatch`, and a `▾` chevron; returns
    /// [`DropdownEvt::Toggle`] when clicked so the caller flips its `open` state.
    /// The popup list itself is drawn separately via [`Ui::list`] (an E3 surface)
    /// so it composites above everything; pass `open` only to draw the pressed/
    /// active chrome here.
    pub fn dropdown(
        &mut self,
        wid: WidgetId,
        rect: Rect,
        label: &str,
        open: bool,
        swatch: Option<[f32; 4]>,
    ) -> DropdownEvt {
        let it = self.interact(wid, rect, true);
        let st = self.wstate(wid, &it, true);
        let hover_t = self.anim(
            wid,
            if open || matches!(st, WState::Hover | WState::Press) { 1.0 } else { 0.0 },
        );
        let fill = state_fill(glass_raised(), hover_t, it.pressed || open);
        self.rrect(rect, self.m.radius, fill);
        if hover_t > 0.0 && !it.pressed {
            self.quad(Rect::new(rect.x, rect.y, rect.w, 1.0), rail());
        }
        if matches!(st, WState::Focus) {
            self.focus_ring(rect, self.m.radius);
        }
        // Left swatch (e.g. theme preview), label, trailing chevron.
        let pad = self.m.pad;
        let mut tx = rect.x + pad;
        let ty = rect.center_y() - self.m.cell_h * 0.5;
        if let Some(sw) = swatch {
            let s = (self.m.cell_h * 0.8).round();
            let sy = rect.center_y() - s * 0.5;
            self.rrect(Rect::new(tx, sy, s, s), 3.0, sw);
            tx += s + self.m.gap;
        }
        self.label(tx.round(), ty.round(), label, fg());
        // Chevron flips appearance via glyph: ▴ when open, ▾ when closed.
        let chev = if open { '▴' } else { '▾' };
        let cx = rect.x + rect.w - pad - self.m.cell_w;
        self.r.push_overlay_glyph_px(cx.round(), ty.round(), chev, fg_dim());
        if it.clicked {
            DropdownEvt::Toggle
        } else {
            DropdownEvt::None
        }
    }

    /// A read-only text field with a leading-ellipsis clip (the END of `text`
    /// stays visible — ideal for paths) plus optional trailing copy (`⧉`) and
    /// open (`↗`) icon buttons. Returns which trailing icon was clicked.
    pub fn text_field_readonly(
        &mut self,
        wid: WidgetId,
        rect: Rect,
        text: &str,
        with_copy: bool,
        with_open: bool,
    ) -> FieldEvt {
        // Sunken track.
        self.rrect(rect, self.m.radius, track_off());
        if *self.focused == Some(wid) {
            self.focus_ring(rect, self.m.radius);
        }
        let pad = self.m.pad;
        // Reserve trailing icon slots.
        let icon_w = self.m.row_h;
        let mut right = rect.x + rect.w;
        let mut evt = FieldEvt::None;
        if with_open {
            right -= icon_w;
            let ir = Rect::new(right, rect.y, icon_w, rect.h);
            if self.icon_button(id_combine(wid, 2), ir, '↗').clicked {
                evt = FieldEvt::Open;
            }
        }
        if with_copy {
            right -= icon_w;
            let ir = Rect::new(right, rect.y, icon_w, rect.h);
            if self.icon_button(id_combine(wid, 1), ir, '⧉').clicked {
                evt = FieldEvt::Copy;
            }
        }
        // Text area = everything left of the icons.
        let text_w = (right - rect.x - 2.0 * pad).max(0.0);
        let max_chars = (text_w / self.m.cell_w).floor() as usize;
        let chars: Vec<char> = text.chars().collect();
        let ty = rect.center_y() - self.m.cell_h * 0.5;
        let tx = rect.x + pad;
        if chars.len() <= max_chars {
            self.label(tx.round(), ty.round(), text, fg());
        } else if max_chars >= 1 {
            // Leading ellipsis: keep the tail visible.
            let tail = &chars[chars.len() - (max_chars - 1)..];
            let mut s = String::from("…");
            s.extend(tail.iter());
            self.label(tx.round(), ty.round(), &s, fg());
        }
        evt
    }

    /// A scrollable selectable list. `rows` are the row labels; `sel` the
    /// currently-selected absolute index (highlighted); `scroll` the vertical
    /// scroll offset in px (the caller owns it and updates from the returned
    /// value of any companion [`Ui::scrollbar`]). Rows are clipped to `rect` by
    /// simple range-culling (no GPU scissor). Returns the row event this frame.
    pub fn list(
        &mut self,
        wid: WidgetId,
        rect: Rect,
        rows: &[&str],
        sel: usize,
        scroll: f32,
    ) -> ListEvt {
        let row_h = self.m.row_h;
        let mut evt = ListEvt::None;
        let first = (scroll / row_h).floor().max(0.0) as usize;
        let visible = (rect.h / row_h).ceil() as usize + 1;
        for (i, label) in rows.iter().enumerate().skip(first).take(visible) {
            let ry = rect.y + i as f32 * row_h - scroll;
            // Cull rows fully outside the viewport.
            if ry + row_h <= rect.y || ry >= rect.y + rect.h {
                continue;
            }
            let rr = Rect::new(rect.x, ry, rect.w, row_h);
            let row_id = id_combine(wid, i as u64);
            let it = self.interact(row_id, rr, true);
            if i == sel {
                self.rrect(rr.inset(1.0), self.m.radius - 1.0, sel_bg());
            } else if it.hovered {
                self.rrect(rr.inset(1.0), self.m.radius - 1.0, state_fill(track_off(), 1.0, false));
            }
            let ty = rr.center_y() - self.m.cell_h * 0.5;
            self.label((rr.x + self.m.pad).round(), ty.round(), label, fg());
            if it.clicked {
                evt = ListEvt::Clicked(i);
            } else if it.hovered && evt == ListEvt::None {
                evt = ListEvt::Hovered(i);
            }
        }
        evt
    }

    /// A vertical scrollbar bound to a scrollable region. `track` is the gutter
    /// rect; `content_h`/`view_h` size the thumb; `scroll` is the current offset
    /// in px. Returns the (possibly dragged) scroll offset, clamped to range.
    pub fn scrollbar(
        &mut self,
        wid: WidgetId,
        track: Rect,
        content_h: f32,
        view_h: f32,
        scroll: f32,
    ) -> f32 {
        let max_scroll = (content_h - view_h).max(0.0);
        let mut s = scroll.clamp(0.0, max_scroll);
        if max_scroll <= 0.0 {
            return 0.0; // nothing to scroll; draw no thumb
        }
        // Track.
        self.rrect(track, track.w * 0.5, track_off());
        let it = self.interact(wid, track, true);
        let thumb_h = (track.h * (view_h / content_h)).max(self.m.row_h * 0.6);
        let span = (track.h - thumb_h).max(0.0);
        if it.pressed && span > 0.0 {
            // Map the pointer to a scroll position (thumb-centered).
            let t = ((self.mouse.1 - track.y - thumb_h * 0.5) / span).clamp(0.0, 1.0);
            s = t * max_scroll;
        }
        let t = if max_scroll > 0.0 { s / max_scroll } else { 0.0 };
        let ty = track.y + span * t;
        let thumb = Rect::new(track.x, ty, track.w, thumb_h);
        let hover_t = self.anim(wid, if it.hovered || it.pressed { 1.0 } else { 0.0 });
        let fill = state_fill(with_alpha(fg(), 0.35), hover_t, it.pressed);
        self.rrect(thumb, track.w * 0.5, fill);
        s
    }

    /// Build the whole Ctrl+, settings form (§3.5): a full-screen scrim, one
    /// centered glass panel with a header (`glassy — settings` + ✕), labelled
    /// rows (font / opacity / bell / theme / font-family / scrollback / config
    /// path) wired to the live effects, and a footer (Save / Close + transient
    /// saved label). All widget ids share the `settings/…` namespace so they are
    /// collected into `tab_order` in declaration order for keyboard nav. Open
    /// dropdown popups (theme / font) are drawn LAST so they float over the rows.
    ///
    /// `surface` is the framebuffer size in px (for centering + the scrim). The
    /// returned [`SettingsEvents`] carry every change back to the App.
    pub fn build_settings(&mut self, surface: (f32, f32), v: &SettingsView) -> SettingsEvents {
        let mut ev = SettingsEvents::default();
        let m = self.m;

        // Full-screen scrim (dim the chrome + terminal beneath).
        self.quad(Rect::new(0.0, 0.0, surface.0, surface.1), [0.0, 0.0, 0.0, 0.5]);

        // Centered panel. Width ≈ 40 columns; height grows with the row count.
        let pw = (m.cell_w * 42.0).min(surface.0 - 2.0 * m.pad).max(m.cell_w * 24.0);
        const ROWS: usize = 7; // font, opacity, bell, theme, font, scrollback, path
        let header_h = m.row_h;
        let footer_h = m.row_h + m.gap;
        let body_h = ROWS as f32 * (m.row_h + m.gap);
        let ph = (header_h + m.gap + body_h + m.gap + footer_h + 2.0 * m.pad).round();
        let px = ((surface.0 - pw) * 0.5).round();
        let py = ((surface.1 - ph) * 0.5).round().max(m.pad);
        let panel = Rect::new(px, py, pw, ph);
        ev.panel = panel;
        let inner = self.panel(panel, m.card_radius);

        // Header row: title + close (✕) at the right.
        let title_y = (inner.y + (m.row_h - m.cell_h) * 0.5).round();
        self.label(inner.x.round(), title_y, "glassy — settings", fg());
        let close_r = Rect::new(inner.x + inner.w - m.row_h, inner.y, m.row_h, m.row_h);
        if self.icon_button(id("settings/close"), close_r, '✕').clicked {
            ev.close = true;
        }

        // Each row: a left label column + a right control column.
        let label_w = (m.cell_w * 12.0).round();
        let ctrl_x = inner.x + label_w;
        let ctrl_w = (inner.w - label_w).min(m.ctrl_w * 1.6).max(m.ctrl_w);
        let mut y = inner.y + header_h + m.gap;
        let step = m.row_h + m.gap;
        let row_label = |ui: &mut Self, y: f32, text: &str| {
            let ly = (y + (m.row_h - m.cell_h) * 0.5).round();
            ui.label(inner.x.round(), ly, text, fg_dim());
        };
        let ctrl_h = m.row_h - m.gap;
        let ctrl_rect = |y: f32, w: f32| Rect::new(ctrl_x, y, w, ctrl_h);

        // -- Font size (stepper) ---------------------------------------------
        row_label(self, y, "Font size");
        let fs_txt = format!("{:.0} px", v.font_px);
        ev.font_delta = self.stepper(id("settings/font_size"), ctrl_rect(y, m.ctrl_w), &fs_txt);
        y += step;

        // -- Opacity (slider) ------------------------------------------------
        row_label(self, y, "Opacity");
        let sl = ctrl_rect(y, ctrl_w - m.cell_w * 6.0);
        let nv = self.slider(id("settings/opacity"), sl, v.opacity, 0.0, 1.0, 0.05);
        if (nv - v.opacity).abs() > f32::EPSILON {
            ev.opacity = Some(nv);
        }
        self.label_right(ctrl_x + ctrl_w, (y + (m.row_h - m.cell_h) * 0.5).round(), &format!("{nv:.2}"), fg());
        y += step;

        // -- Bell (segmented) ------------------------------------------------
        row_label(self, y, "Bell");
        let bv = self.segmented(
            id("settings/bell"),
            ctrl_rect(y, ctrl_w),
            &["Off", "Visual", "Audible"],
            v.bell.min(2),
        );
        if bv != v.bell {
            ev.bell = Some(bv);
        }
        y += step;

        // -- Theme (dropdown + swatch) ---------------------------------------
        row_label(self, y, "Theme");
        let theme_rect = ctrl_rect(y, ctrl_w);
        let theme_name = v.theme_names.get(v.theme_idx).copied().unwrap_or("");
        let swatch = v.theme_swatches.get(v.theme_idx).copied();
        if self.dropdown(id("settings/theme"), theme_rect, theme_name, v.open == SettingsDrop::Theme, swatch)
            == DropdownEvt::Toggle
        {
            ev.theme_toggle = true;
        }
        y += step;

        // -- Font family (dropdown) ------------------------------------------
        row_label(self, y, "Font");
        let font_rect = ctrl_rect(y, ctrl_w);
        if self.dropdown(id("settings/font_family"), font_rect, v.font_family, v.open == SettingsDrop::Font, None)
            == DropdownEvt::Toggle
        {
            ev.font_toggle = true;
        }
        y += step;

        // -- Scrollback (stepper) --------------------------------------------
        row_label(self, y, "Scrollback");
        let sb_txt = format!("{} lines", v.scrollback);
        ev.scrollback_delta = self.stepper(id("settings/scrollback"), ctrl_rect(y, m.ctrl_w), &sb_txt);
        y += step;

        // -- Config path (readonly + copy/open) ------------------------------
        row_label(self, y, "Config");
        let field_rect = ctrl_rect(y, ctrl_w);
        match self.text_field_readonly(id("settings/config"), field_rect, v.config_path, true, true) {
            FieldEvt::Copy => ev.copy_path = true,
            FieldEvt::Open => ev.open_path = true,
            FieldEvt::None => {}
        }
        y += step;

        // -- Footer: separator + Save (accent) + Close + transient saved ------
        let sep_y = (y + m.gap * 0.5).round();
        self.separator(inner.x, sep_y, inner.w);
        let fy = sep_y + m.gap;
        let bw = (m.cell_w * 9.0).round();
        let close_btn = Rect::new(inner.x + inner.w - bw, fy, bw, m.row_h);
        let save_btn = Rect::new(close_btn.x - bw - m.gap, fy, bw, m.row_h);
        if self.accent_button(id("settings/save"), save_btn, "Save").clicked {
            ev.save = true;
        }
        if self.button(id("settings/close_btn"), close_btn, "Close").clicked {
            ev.close = true;
        }
        if v.saved {
            let ly = (fy + (m.row_h - m.cell_h) * 0.5).round();
            self.label(inner.x.round(), ly, "✓ saved", fill_on());
        }

        // -- Floating dropdown popups (drawn LAST so they overlap the rows) ---
        match v.open {
            SettingsDrop::Theme => {
                let pick = self.dropdown_popup(
                    id("settings/theme/list"),
                    theme_rect,
                    v.theme_names,
                    v.theme_idx,
                    Some(v.theme_swatches),
                );
                ev.theme_pick = pick;
            }
            SettingsDrop::Font => {
                let pick = self.dropdown_popup(
                    id("settings/font/list"),
                    font_rect,
                    v.font_names,
                    v.font_idx,
                    None,
                );
                ev.font_pick = pick;
            }
            SettingsDrop::None => {}
        }

        ev
    }

    /// A primary (accent-filled) button — same interaction as [`Ui::button`] but
    /// filled with the accent color and dark-on-accent text. Used for Save.
    pub fn accent_button(&mut self, wid: WidgetId, rect: Rect, text: &str) -> Interaction {
        let it = self.interact(wid, rect, true);
        let st = self.wstate(wid, &it, true);
        let hover_t = self.anim(wid, if matches!(st, WState::Hover | WState::Press) { 1.0 } else { 0.0 });
        let fill = state_fill(fill_on(), hover_t, it.pressed);
        self.rrect(rect, self.m.radius, fill);
        if matches!(st, WState::Focus) {
            self.focus_ring(rect, self.m.radius);
        }
        let nudge = if it.pressed { 1.0 } else { 0.0 };
        let mut content = rect;
        content.y += nudge;
        self.label_centered(content, text, color::default_bg());
        it
    }

    /// The floating popup list for a dropdown (E3 surface anchored just below
    /// `anchor`). Each row shows an optional swatch, the option name, and a `✓`
    /// on the current selection. Returns the absolute index if a row was clicked.
    /// Drawn after the form body so it composites above everything.
    fn dropdown_popup(
        &mut self,
        wid: WidgetId,
        anchor: Rect,
        rows: &[&str],
        sel: usize,
        swatches: Option<&[[f32; 4]]>,
    ) -> Option<usize> {
        let m = self.m;
        let row_h = m.row_h;
        // Cap the popup height; tall lists would overflow the panel.
        let max_rows = rows.len().min(8);
        let h = (max_rows as f32 * row_h + 2.0).round();
        let rect = Rect::new(anchor.x, anchor.y + anchor.h + 2.0, anchor.w, h);
        self.rrect(rect, m.radius, glass_float());
        self.edge_light(rect);
        let mut picked = None;
        for (i, name) in rows.iter().enumerate().take(max_rows) {
            let ry = rect.y + 1.0 + i as f32 * row_h;
            let rr = Rect::new(rect.x + 1.0, ry, rect.w - 2.0, row_h);
            let it = self.interact(id_combine(wid, i as u64), rr, true);
            if i == sel {
                self.rrect(rr.inset(1.0), m.radius - 1.0, sel_bg());
            } else if it.hovered {
                self.rrect(rr.inset(1.0), m.radius - 1.0, state_fill(track_off(), 1.0, false));
            }
            let mut tx = rr.x + m.pad;
            let ty = (rr.center_y() - m.cell_h * 0.5).round();
            if let Some(sw) = swatches.and_then(|s| s.get(i).copied()) {
                let s = (m.cell_h * 0.8).round();
                let sy = rr.center_y() - s * 0.5;
                self.rrect(Rect::new(tx, sy, s, s), 3.0, sw);
                tx += s + m.gap;
            }
            self.label(tx.round(), ty, name, fg());
            if i == sel {
                self.r.push_overlay_glyph_px(
                    (rr.x + rr.w - m.pad - m.cell_w).round(),
                    ty,
                    '✓',
                    fill_on(),
                );
            }
            if it.clicked {
                picked = Some(i);
            }
        }
        picked
    }
}

// ---------------------------------------------------------------------------
// Wave 6 — Real menus (§3.6)
// ---------------------------------------------------------------------------

/// A single entry in a dropdown/context menu. The caller builds a `Vec<MenuEntry>`
/// and passes it to [`menu`]; the returned index (if any) identifies which *item*
/// (non-separator entry, 0-based among items only) was clicked.
///
/// # Variant summary
/// - `Item` — a normal actionable row: left icon glyph, label, optional right-
///   aligned dim shortcut hint, and an `enabled` flag (disabled = greyed, no click).
/// - `Separator` — a 1 px hairline dividing groups of items (not focusable).
#[derive(Clone, Debug)]
pub enum MenuEntry<'a> {
    /// An actionable menu row.
    Item {
        /// Single-character icon drawn to the left of the label (e.g. `'+'`).
        icon:     char,
        /// Row label shown in the primary foreground colour (or greyed if disabled).
        label:    &'a str,
        /// Optional right-aligned shortcut hint (e.g. `"Ctrl+T"`), drawn dim.
        hint:     Option<&'a str>,
        /// `false` → row is drawn greyed and never fires a click.
        enabled:  bool,
    },
    /// A 1 px hairline dividing groups (not selectable, skipped by keyboard nav).
    Separator,
}

/// Draw an E3 floating menu panel anchored at pixel `(ax, ay)` (top-left of the
/// panel). `entries` mixes [`MenuEntry::Item`] rows with [`MenuEntry::Separator`]
/// dividers; `sel_item` is the *item* index (0-based among non-separator rows)
/// currently highlighted by keyboard navigation.
///
/// Returns the *item* index if an enabled row was clicked this frame, `None`
/// otherwise. The caller maps the item index back to a `MenuAction`.
///
/// Menus never have a scrim — the terminal stays visible beside them (matching
/// today's behaviour). The panel is drawn last so it composites above everything.
/// `ax`/`ay` is the top-left anchor in physical pixels.
pub fn menu(
    renderer:   &mut Renderer,
    cell_w:     f32,
    cell_h:     f32,
    mouse:      (f32, f32),
    mouse_down: bool,
    clicked:    bool,
    ax:         f32,
    ay:         f32,
    entries:    &[MenuEntry<'_>],
    sel_item:   usize,
) -> Option<usize> {
    let row_h   = (cell_h * 1.4).round().max(cell_h + 4.0);
    let sep_h   = 5.0; // 1 px hairline + 2 px padding each side
    let pad_x   = (cell_w * 1.2).round();
    let icon_w  = cell_w + 4.0;
    let hint_gap = (cell_w * 2.0).round();

    // Measure the widest label + widest hint to size the panel.
    let label_chars = entries
        .iter()
        .filter_map(|e| if let MenuEntry::Item { label, .. } = e { Some(label.len()) } else { None })
        .max()
        .unwrap_or(4);
    let hint_chars = entries
        .iter()
        .filter_map(|e| {
            if let MenuEntry::Item { hint: Some(h), .. } = e { Some(h.len()) } else { None }
        })
        .max()
        .unwrap_or(0);
    let panel_w = (icon_w
        + label_chars as f32 * cell_w
        + if hint_chars > 0 { hint_gap + hint_chars as f32 * cell_w } else { 0.0 }
        + pad_x * 2.0)
        .max(cell_w * 8.0)
        .ceil();

    // Compute total panel height.
    let item_count = entries.iter().filter(|e| matches!(e, MenuEntry::Item { .. })).count();
    let sep_count  = entries.iter().filter(|e| matches!(e, MenuEntry::Separator)).count();
    let panel_h    = (item_count as f32 * row_h + sep_count as f32 * sep_h + 4.0).ceil();

    // E3 floating panel + thin accent border + edge-lit rails.
    let float_fill  = glass_float();
    let border_col  = with_alpha(color::accent(), 0.5);
    let hairline_c  = hairline();
    renderer.push_overlay_rrect_px(ax, ay, panel_w, panel_h, 4.0, float_fill);
    renderer.push_overlay_px(ax, ay, panel_w, 1.0, border_col);
    renderer.push_overlay_px(ax, ay + panel_h - 1.0, panel_w, 1.0, border_col);
    renderer.push_overlay_px(ax, ay, 1.0, panel_h, border_col);
    renderer.push_overlay_px(ax + panel_w - 1.0, ay, 1.0, panel_h, border_col);

    let mut result: Option<usize> = None;
    let mut item_idx: usize = 0; // index among non-separator entries
    let mut y = ay + 2.0;

    for entry in entries {
        match entry {
            MenuEntry::Separator => {
                // Thin hairline with 2 px vertical padding.
                y += 2.0;
                renderer.push_overlay_px(ax + 4.0, y, panel_w - 8.0, 1.0, hairline_c);
                y += 3.0;
            }
            MenuEntry::Item { icon, label, hint, enabled } => {
                let rr = Rect::new(ax + 1.0, y, panel_w - 2.0, row_h);
                let over = hit(rr, mouse.0, mouse.1) && *enabled;
                let is_sel = item_idx == sel_item && *enabled;

                // Highlight: keyboard selection OR hover.
                if over || is_sel {
                    renderer.push_overlay_rrect_px(
                        rr.x, rr.y, rr.w, rr.h, 3.0, sel_bg(),
                    );
                }

                let text_col = if *enabled { fg() } else { fg_dim() };
                let ty = (y + (row_h - cell_h) * 0.5).round();

                // Left icon.
                let ix = ax + pad_x;
                renderer.push_overlay_glyph_px(ix.round(), ty, *icon, if *enabled { fg() } else { fg_dim() });

                // Label.
                let lx = ix + icon_w;
                let mut cx = lx;
                for ch in label.chars() {
                    renderer.push_overlay_glyph_px(cx.round(), ty, ch, text_col);
                    cx += cell_w;
                }

                // Right-aligned shortcut hint.
                if let Some(h) = hint {
                    let hint_w = h.chars().count() as f32 * cell_w;
                    let hx = ax + panel_w - pad_x - hint_w;
                    let mut cx2 = hx;
                    for ch in h.chars() {
                        renderer.push_overlay_glyph_px(cx2.round(), ty, ch, fg_dim());
                        cx2 += cell_w;
                    }
                }

                // Click detection.
                if *enabled && clicked && over {
                    result = Some(item_idx);
                }
                item_idx += 1;
                y += row_h;
            }
        }
    }

    let _ = mouse_down; // retained for future press-highlight; clicks are the gate
    result
}

// ---------------------------------------------------------------------------
// Wave 6 — Help / keybindings panel (§3.7)
// ---------------------------------------------------------------------------

/// A single row in the help panel.
#[derive(Clone, Copy, Debug)]
pub enum HelpRow<'a> {
    /// Section header: a dim label + a separator underneath.
    Section(&'a str),
    /// A keybinding row: left = chord description, right = action description.
    Binding { keys: &'a str, desc: &'a str },
    /// A blank visual gap (empty row).
    Gap,
}

/// Scroll-state owned by the App for the help panel (carried across frames).
#[derive(Clone, Copy, Debug, Default)]
pub struct HelpState {
    /// Current vertical scroll offset in pixels.
    pub scroll: f32,
}

/// Result of a [`build_help`] call this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct HelpResult {
    /// `true` if Esc / F1 / the ✕ button / the scrim was clicked — caller closes.
    pub close: bool,
    /// Updated scroll state for next frame.
    pub state: HelpState,
}

/// Draw the F1 keybindings help panel (§3.7): a full-screen scrim, a centered
/// glass panel with a header + ✕, two-column rows (left key-cap chips, right
/// description), section headers + separators, and a scrollbar when content
/// overflows the panel. Esc/F1/scrim/✕ all set `close`.
///
/// `surface` is `(width, height)` in physical px. `mouse`/`mouse_down`/`clicked`
/// are the current frame's pointer state. `state` is the caller-owned scroll
/// position (mutable borrow; updated in place).
#[allow(clippy::too_many_arguments)]
pub fn build_help(
    renderer:   &mut Renderer,
    cell_w:     f32,
    cell_h:     f32,
    surface:    (f32, f32),
    mouse:      (f32, f32),
    mouse_down: bool,
    clicked:    bool,
    pressed:    &mut Option<WidgetId>,
    focused:    &mut Option<WidgetId>,
    anims:      &mut HashMap<WidgetId, Anim>,
    state:      &mut HelpState,
) -> HelpResult {
    let mut result = HelpResult::default();
    let m = Metrics::new(cell_w, cell_h);

    // Full-screen scrim — click on scrim closes the panel.
    let scrim = Rect::new(0.0, 0.0, surface.0, surface.1);
    renderer.push_overlay_px(scrim.x, scrim.y, scrim.w, scrim.h, [0.0, 0.0, 0.0, 0.5]);
    if clicked && hit(scrim, mouse.0, mouse.1) {
        // Will be refined: only close if click was OUTSIDE the panel (checked below).
    }

    // Panel sizing: ≈ 50 columns wide, tall enough for visible rows (capped at 80%).
    let pw = (cell_w * 50.0).min(surface.0 - 2.0 * m.pad).max(cell_w * 28.0);
    let max_ph = (surface.1 * 0.82).round();
    let header_h = m.row_h;
    let footer_h = 0.0; // no footer — ✕ in the header suffices

    // Lay out the help rows once to measure total content height.
    let rows = help_rows();
    let row_h = m.row_h;
    let sep_h = 6.0;
    let section_h = (m.cell_h + 4.0).round();
    let content_h: f32 = rows.iter().map(|r| match r {
        HelpRow::Section(_) => section_h + sep_h,
        HelpRow::Binding { .. } => row_h,
        HelpRow::Gap => (m.cell_h * 0.5).round(),
    }).sum();

    let scrollbar_w = (m.gap.max(6.0)).round();
    let body_h = content_h.min(max_ph - header_h - 2.0 * m.pad);
    let ph = (header_h + m.pad + body_h + m.pad).round().min(max_ph);
    let px = ((surface.0 - pw) * 0.5).round();
    let py = ((surface.1 - ph) * 0.5).round().max(m.pad);
    let panel = Rect::new(px, py, pw, ph);

    // Close if clicked outside the panel (scrim click).
    if clicked && !hit(panel, mouse.0, mouse.1) {
        result.close = true;
    }

    // Draw panel background + left accent rail.
    renderer.push_overlay_rrect_px(panel.x, panel.y, panel.w, panel.h, m.card_radius, glass_raised());
    renderer.push_overlay_px(panel.x, panel.y, 1.0, panel.h, rail());

    // Header: title + ✕ button.
    let title = "⌨  glassy — keybindings";
    let th = ((header_h - cell_h) * 0.5).round();
    let ty = panel.y + m.pad + th;
    let mut cx = panel.x + m.pad * 1.5;
    for ch in title.chars() {
        renderer.push_overlay_glyph_px(cx.round(), ty, ch, fg());
        cx += cell_w;
    }

    // ✕ close button.
    let close_r = Rect::new(panel.x + panel.w - header_h, panel.y + m.pad * 0.5, header_h, header_h);
    {
        let wid = id("help/close");
        let over = hit(close_r, mouse.0, mouse.1);
        if over {
            renderer.push_overlay_rrect_px(
                close_r.x + 2.0, close_r.y + 2.0,
                close_r.w - 4.0, close_r.h - 4.0,
                3.0, state_fill(glass_raised(), 1.0, false),
            );
        }
        let gx = close_r.x + (close_r.w - cell_w) * 0.5;
        let gy = close_r.y + (close_r.h - cell_h) * 0.5;
        renderer.push_overlay_glyph_px(gx.round(), gy.round(), '✕', if over { fg() } else { fg_dim() });
        if clicked && over {
            result.close = true;
        }
        // Step hover animation.
        let a = anims.entry(wid).or_insert_with(|| Anim::new(0.0));
        a.target = if over { 1.0 } else { 0.0 };
    }

    // Header separator.
    let sep_y = panel.y + m.pad + header_h;
    renderer.push_overlay_px(panel.x, sep_y, panel.w, 1.0, hairline());

    // Scrollable body region.
    let body_top  = sep_y + 1.0;
    let body_left = panel.x + m.pad;
    let body_w    = panel.w - m.pad * 2.0 - scrollbar_w - 4.0;
    let _body_rect = Rect::new(body_left, body_top, body_w, body_h);

    // Clamp scroll.
    let max_scroll = (content_h - body_h).max(0.0);
    state.scroll = state.scroll.clamp(0.0, max_scroll);

    // Key-cap chip metrics.
    let chip_pad_x = (cell_w * 0.6).round();
    let chip_pad_y = 2.0;
    let chip_radius = 3.0;

    // Left column: chips width (fixed, based on longest key string).
    let max_key_chars = rows.iter().filter_map(|r| {
        if let HelpRow::Binding { keys, .. } = r { Some(keys.len()) } else { None }
    }).max().unwrap_or(8).min(18); // cap so chips don't eat the whole panel
    let chip_col_w = (max_key_chars as f32 * cell_w + chip_pad_x * 2.0).round();
    let desc_x = body_left + chip_col_w + m.gap;

    // Draw rows, scissored to [body_top, body_top + body_h).
    let mut ry = body_top - state.scroll;
    for row in &rows {
        let row_height = match row {
            HelpRow::Section(_) => section_h + sep_h,
            HelpRow::Binding { .. } => row_h,
            HelpRow::Gap => (cell_h * 0.5).round(),
        };
        // Cull rows fully outside the visible window.
        if ry + row_height <= body_top || ry >= body_top + body_h {
            ry += row_height;
            continue;
        }
        match row {
            HelpRow::Section(title) => {
                // Dim section title.
                let section_ty = (ry + (section_h - cell_h) * 0.5).round();
                if section_ty >= body_top && section_ty + cell_h <= body_top + body_h {
                    let mut cx = body_left;
                    for ch in title.chars() {
                        renderer.push_overlay_glyph_px(cx.round(), section_ty, ch, fg_dim());
                        cx += cell_w;
                    }
                }
                // Separator below the section title.
                let sep_line = (ry + section_h + 1.0).round();
                if sep_line >= body_top && sep_line < body_top + body_h {
                    renderer.push_overlay_px(body_left, sep_line, body_w + scrollbar_w + 4.0, 1.0, hairline());
                }
            }
            HelpRow::Binding { keys, desc } => {
                let text_ty = (ry + (row_h - cell_h) * 0.5).round();
                if text_ty + cell_h > body_top && text_ty < body_top + body_h {
                    // Key-cap chip background.
                    let chip_h = cell_h + chip_pad_y * 2.0;
                    let chip_w = (keys.chars().count() as f32 * cell_w + chip_pad_x * 2.0).round();
                    let chip_y = ry + (row_h - chip_h) * 0.5;
                    renderer.push_overlay_rrect_px(
                        body_left, chip_y, chip_w, chip_h, chip_radius,
                        with_alpha(color::default_fg(), 0.12),
                    );
                    renderer.push_overlay_px(body_left, chip_y, chip_w, 1.0, with_alpha(color::default_fg(), 0.25));
                    renderer.push_overlay_px(body_left, chip_y + chip_h - 1.0, chip_w, 1.0, with_alpha(color::default_bg(), 0.35));

                    // Key text inside chip.
                    let kx = body_left + chip_pad_x;
                    let mut cx = kx;
                    for ch in keys.chars() {
                        renderer.push_overlay_glyph_px(cx.round(), text_ty, ch, fg());
                        cx += cell_w;
                    }

                    // Description (right of chip column).
                    let mut cx = desc_x;
                    for ch in desc.chars() {
                        if cx + cell_w > panel.x + panel.w - scrollbar_w - 4.0 {
                            break;
                        }
                        renderer.push_overlay_glyph_px(cx.round(), text_ty, ch, fg());
                        cx += cell_w;
                    }
                }
            }
            HelpRow::Gap => {}
        }
        ry += row_height;
    }

    // Scrollbar (only when content overflows).
    if max_scroll > 0.0 {
        let sb_x = panel.x + panel.w - scrollbar_w - 2.0;
        let sb_y = body_top;
        let sb_h = body_h;
        let track = Rect::new(sb_x, sb_y, scrollbar_w, sb_h);
        let thumb_ratio = (body_h / content_h).min(1.0);
        let thumb_h = (sb_h * thumb_ratio).max(m.row_h * 0.5);
        let thumb_t  = if max_scroll > 0.0 { state.scroll / max_scroll } else { 0.0 };
        let thumb_y  = sb_y + (sb_h - thumb_h) * thumb_t;

        renderer.push_overlay_rrect_px(track.x, track.y, track.w, track.h, track.w * 0.5, track_off());
        let thumb_over = hit(track, mouse.0, mouse.1);
        let thumb_col = if thumb_over || mouse_down {
            state_fill(with_alpha(fg(), 0.35), 1.0, mouse_down && thumb_over)
        } else {
            with_alpha(fg(), 0.25)
        };
        renderer.push_overlay_rrect_px(track.x, thumb_y, track.w, thumb_h, track.w * 0.5, thumb_col);

        // Drag the scrollbar.
        if mouse_down && hit(track, mouse.0, mouse.1) {
            let t = ((mouse.1 - sb_y - thumb_h * 0.5) / (sb_h - thumb_h).max(1.0)).clamp(0.0, 1.0);
            state.scroll = t * max_scroll;
        }

        // Record scrollbar widget interaction for animation.
        let wid = id("help/scrollbar");
        let a = anims.entry(wid).or_insert_with(|| Anim::new(0.0));
        a.target = if thumb_over { 1.0 } else { 0.0 };
    }

    // Suppress unused-variable warnings.
    let _ = (pressed, focused, footer_h);
    result
}

/// The canonical set of help rows shown in the F1 panel. Separated into sections
/// with `HelpRow::Section` headers and `HelpRow::Gap` spacers.
fn help_rows() -> Vec<HelpRow<'static>> {
    vec![
        HelpRow::Gap,
        HelpRow::Section("Tabs"),
        HelpRow::Binding { keys: "Ctrl+Shift+T",   desc: "New tab" },
        HelpRow::Binding { keys: "Ctrl+Shift+W",   desc: "Close pane / tab" },
        HelpRow::Binding { keys: "Ctrl+Tab",        desc: "Next tab" },
        HelpRow::Binding { keys: "Ctrl+Shift+Tab",  desc: "Previous tab" },
        HelpRow::Gap,
        HelpRow::Section("Split panes"),
        HelpRow::Binding { keys: "Ctrl+Shift+E",   desc: "Split vertical" },
        HelpRow::Binding { keys: "Ctrl+Shift+O",   desc: "Split horizontal" },
        HelpRow::Binding { keys: "Alt+Arrow",       desc: "Focus adjacent pane" },
        HelpRow::Gap,
        HelpRow::Section("Edit"),
        HelpRow::Binding { keys: "Ctrl+Shift+C",   desc: "Copy selection" },
        HelpRow::Binding { keys: "Ctrl+Shift+V",   desc: "Paste" },
        HelpRow::Binding { keys: "Ctrl+Click",      desc: "Open hyperlink" },
        HelpRow::Gap,
        HelpRow::Section("View"),
        HelpRow::Binding { keys: "Ctrl  +",         desc: "Font bigger" },
        HelpRow::Binding { keys: "Ctrl  -",         desc: "Font smaller" },
        HelpRow::Binding { keys: "Ctrl  0",         desc: "Font reset" },
        HelpRow::Binding { keys: "Shift+PgUp",      desc: "Scroll history up" },
        HelpRow::Binding { keys: "Shift+PgDn",      desc: "Scroll history down" },
        HelpRow::Binding { keys: "Shift+Home",      desc: "Scroll to top" },
        HelpRow::Binding { keys: "Shift+End",       desc: "Scroll to bottom" },
        HelpRow::Gap,
        HelpRow::Section("App"),
        HelpRow::Binding { keys: "Ctrl+,",          desc: "Settings" },
        HelpRow::Binding { keys: "Right-click",     desc: "Context menu" },
        HelpRow::Binding { keys: "F1  /  Esc",      desc: "Close this panel" },
        HelpRow::Gap,
    ]
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
    fn settings_widget_ids_are_all_distinct() {
        // The settings form's keyboard tab order relies on these ids being
        // unique (App::settings_focus_order mirrors this set).
        let ids = [
            id("settings/font_size"),
            id("settings/opacity"),
            id("settings/bell"),
            id("settings/theme"),
            id("settings/font_family"),
            id("settings/scrollback"),
            id("settings/config"),
            id("settings/save"),
            id("settings/close"),
            id("settings/close_btn"),
        ];
        for (i, a) in ids.iter().enumerate() {
            for b in ids.iter().skip(i + 1) {
                assert_ne!(a, b, "settings widget ids must be unique");
            }
        }
    }

    #[test]
    fn settings_defaults_are_inert() {
        // A freshly-built event struct must drive no live effects.
        let ev = SettingsEvents::default();
        assert_eq!(ev.font_delta, 0);
        assert!(ev.opacity.is_none());
        assert!(ev.bell.is_none());
        assert!(!ev.save && !ev.close);
        assert!(ev.theme_pick.is_none() && ev.font_pick.is_none());
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
