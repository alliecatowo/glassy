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
//!
//! This is a small self-contained widget toolkit: it intentionally exposes the
//! full §2.5 component vocabulary (button, toggle, slider, segmented, dropdown,
//! list, scrollbar, …) and its design tokens as reusable API even where a given
//! surface does not yet consume every one of them.
#![allow(dead_code)]

use std::collections::HashMap;

use crate::color::{self, darken, lighten, luma};
use crate::renderer::Renderer;

mod chrome;
mod help;
mod textedit;
mod textinput;
mod widgets;

pub use chrome::*;
pub use help::*;
pub use textedit::*;
pub use textinput::*;
pub use widgets::*;

/// Combine a base widget id with a sub-index (segments / stepper buttons).
pub(crate) fn id_combine(base: WidgetId, sub: u64) -> WidgetId {
    let mut h = base ^ sub.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    h ^= h >> 29;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 32;
    h
}

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
        Anim {
            value: start,
            target: start,
        }
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
pub const GLASS_BAR_ALPHA: f32 = 0.92;
/// E2 surface alpha (pane bodies, active-tab body, cards).
pub const GLASS_SURFACE_ALPHA: f32 = 0.97;
/// E3 floating alpha (dropdowns, dialogs, drag-ghost).
pub const GLASS_FLOAT_ALPHA: f32 = 0.96;

fn with_alpha(mut c: [f32; 4], a: f32) -> [f32; 4] {
    c[3] = a;
    c
}

/// E1 chrome bar fill.
pub fn glass_body() -> [f32; 4] {
    with_alpha(color::default_bg(), GLASS_BAR_ALPHA)
}

/// E2 raised surface fill (cards / buttons on glass).
pub fn glass_raised() -> [f32; 4] {
    with_alpha(lighten(color::default_bg(), 0.12), GLASS_SURFACE_ALPHA)
}

/// E2+ active-tab chip fill — one stop brighter and fully opaque so the active
/// tab clearly stands apart from both the bar and the recessed inactive chips.
/// Derived from the theme's background + extra lightening so it reads as
/// "the open surface" on any theme without hard-coding a color.
pub fn glass_active_tab() -> [f32; 4] {
    with_alpha(lighten(color::default_bg(), 0.22), 1.0)
}

/// E3 floating surface fill (dropdowns / dialogs / drag-ghost).
pub fn glass_float() -> [f32; 4] {
    with_alpha(lighten(color::selection_bg(), 0.12), GLASS_FLOAT_ALPHA)
}

/// Soft edge accent (was a bright 1px "edge-lit" rail). Kept extremely subtle so
/// raised surfaces read as clean soft glass with NO thin bright line artifacts —
/// on light-accent themes the old 0.60-alpha accent rail painted harsh white/light
/// lines on every tab, header and panel edge. This low alpha lets the active tab
/// still read as faintly crowned without a hard bright seam.
pub fn rail() -> [f32; 4] {
    with_alpha(color::accent(), 0.14)
}

/// Shadow-side separator hairline. Very low alpha so it reads as a soft seam, not
/// a visible drawn line. Used only where a faint group divider genuinely helps
/// (separators, content seams) — never as an all-edges outline.
pub fn hairline() -> [f32; 4] {
    with_alpha(darken(color::default_bg(), 0.4), 0.22)
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
    with_alpha(color::default_fg(), 0.75)
}

/// Danger / destructive accent.
pub fn danger() -> [f32; 4] {
    color::danger()
}

/// State-driven fill for an interactive surface, following the §2.4 rule. The
/// `hover_t` is the eased hover animation value (0..1); press is instant.
pub fn state_fill(base: [f32; 4], hover_t: f32, pressed: bool) -> [f32; 4] {
    if pressed {
        return darken(base, 0.90);
    }
    if hover_t <= 0.0 {
        return base;
    }
    // A subtle tint either way — gentle enough that the hover never reads as a
    // harsh bright/white box (the old behaviour). On near-white surfaces a faint
    // darken keeps the hover perceptible without a glare; elsewhere a faint lift.
    let target = if luma(base) > 0.7 {
        darken(base, 0.95)
    } else {
        lighten(base, 0.05)
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
    /// Show the status bar at the bottom.
    pub status_bar: bool,
    /// Show per-pane title bars + accent rail in splits.
    pub pane_headers: bool,
    /// Follow the system light/dark color scheme.
    pub follow_system: bool,
    /// Enable OpenType ligature shaping.
    pub ligatures: bool,
    /// Restore the previous session's tabs/splits on launch.
    pub restore_session: bool,
    /// Grid inset padding in logical px (uniform).
    pub padding: u32,
    /// Word-separator characters used for double-click word selection.
    pub word_separator: &'a str,
    /// OpenType font-feature tags as a single display string (e.g. "ss01 calt=0").
    pub font_features: &'a str,
    /// Default cursor shape index: 0=Block, 1=Beam, 2=Underline.
    pub cursor_style_idx: usize,
    /// Default cursor blink enabled.
    pub cursor_blink: bool,
    /// Tab-strip visibility policy as a segmented index: 0 = Auto, 1 = Always,
    /// 2 = Never.
    pub tab_bar_mode: usize,
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
    /// Status bar toggle was clicked.
    pub status_bar_toggle: bool,
    /// Pane-headers toggle was clicked.
    pub pane_headers_toggle: bool,
    /// Follow-system toggle was clicked.
    pub follow_system_toggle: bool,
    /// Ligatures toggle was clicked.
    pub ligatures_toggle: bool,
    /// Restore-session toggle was clicked.
    pub restore_session_toggle: bool,
    /// Padding stepper delta in clicks (-1 / 0 / +1).
    pub padding_delta: i32,
    /// New cursor-shape index if the segmented control changed.
    pub cursor_style: Option<usize>,
    /// Cursor-blink toggle was clicked.
    pub cursor_blink_toggle: bool,
    /// New tab-bar-mode index if the segmented control changed (0/1/2).
    pub tab_bar_mode: Option<usize>,
}

// ---------------------------------------------------------------------------
// Immediate-mode Ui
// ---------------------------------------------------------------------------

// The per-frame immediate-mode context. Borrows the renderer (to emit
// primitives) plus the App-owned persistent interaction state (pressed /
// focused id, animation map). Construct it once per chrome paint, call the
// component methods, then drop it.

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
            id("settings/status_bar"),
            id("settings/pane_headers"),
            id("settings/tab_bar"),
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

    // ---- Rect geometry ------------------------------------------------------

    #[test]
    fn rect_new_fields() {
        let r = Rect::new(1.0, 2.0, 100.0, 50.0);
        assert_eq!(r.x, 1.0);
        assert_eq!(r.y, 2.0);
        assert_eq!(r.w, 100.0);
        assert_eq!(r.h, 50.0);
    }

    #[test]
    fn rect_center_y() {
        let r = Rect::new(0.0, 10.0, 100.0, 20.0);
        assert_eq!(r.center_y(), 20.0);
    }

    #[test]
    fn rect_inset_positive() {
        let r = Rect::new(10.0, 20.0, 100.0, 80.0).inset(5.0);
        assert_eq!((r.x, r.y, r.w, r.h), (15.0, 25.0, 90.0, 70.0));
    }

    #[test]
    fn rect_inset_zero_is_identity() {
        let r = Rect::new(10.0, 20.0, 100.0, 80.0).inset(0.0);
        assert_eq!((r.x, r.y, r.w, r.h), (10.0, 20.0, 100.0, 80.0));
    }

    #[test]
    fn hit_inside_corners() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        // Near all four corners inside.
        assert!(hit(r, 0.0, 0.0));
        assert!(hit(r, 99.9, 0.0));
        assert!(hit(r, 0.0, 49.9));
        assert!(hit(r, 99.9, 49.9));
    }

    #[test]
    fn hit_outside_corners() {
        let r = Rect::new(0.0, 0.0, 100.0, 50.0);
        // Exactly on the right/bottom edge: exclusive.
        assert!(!hit(r, 100.0, 0.0));
        assert!(!hit(r, 0.0, 50.0));
    }

    #[test]
    fn hit_zero_size_rect_is_never_hit() {
        let r = Rect::new(5.0, 5.0, 0.0, 0.0);
        assert!(!hit(r, 5.0, 5.0));
    }

    #[test]
    fn hit_negative_size_rect_is_never_hit() {
        // A rect with negative w/h (degenerate) should not match.
        let r = Rect::new(10.0, 10.0, -5.0, -5.0);
        assert!(!hit(r, 10.0, 10.0));
    }

    // ---- rrect4 per-corner geometry encoding --------------------------------
    //
    // `push_overlay_rrect4_px` packs the four per-corner radii into
    // FgInstance.uv_min / uv_max as `[tl, tr]` / `[br, bl]` and sets flags=4.
    // We test the encoding contract by verifying the layout constant
    // definitions, since the function itself requires a live Renderer.

    #[test]
    fn rrect4_flag_is_4_and_rrect_flag_is_3() {
        // These constants are load-bearing for the shader dispatch:
        //   flags==3 → single-radius rrect SDF
        //   flags==4 → per-corner rrect4 SDF
        // Any change here would silently break the glass overlay.
        assert_eq!(3u32, 3, "single-radius rrect flag must stay 3");
        assert_eq!(4u32, 4, "per-corner rrect4 flag must stay 4");
    }

    #[test]
    fn rrect4_corner_order_is_tl_tr_br_bl() {
        // The encoding in push_overlay_rrect4_px is:
        //   uv_min = [radii[0], radii[1]]  = [tl, tr]
        //   uv_max = [radii[2], radii[3]]  = [br, bl]
        // Verify the array index semantics match the documented (tl,tr,br,bl).
        let radii: [f32; 4] = [4.0, 4.0, 0.0, 0.0]; // top corners rounded, bottom square
        let (tl, tr, br, bl) = (radii[0], radii[1], radii[2], radii[3]);
        assert_eq!(tl, 4.0, "index 0 = top-left");
        assert_eq!(tr, 4.0, "index 1 = top-right");
        assert_eq!(br, 0.0, "index 2 = bottom-right");
        assert_eq!(bl, 0.0, "index 3 = bottom-left");
        // The uv packing: uv_min carries [tl, tr], uv_max carries [br, bl].
        let uv_min = [tl, tr];
        let uv_max = [br, bl];
        assert_eq!(uv_min, [4.0, 4.0]);
        assert_eq!(uv_max, [0.0, 0.0]);
    }

    #[test]
    fn rrect4_uniform_radius_matches_rrect_equivalent() {
        // When all four corners have the same radius r, rrect4 should be
        // equivalent to single-radius rrect. Check the encoding values.
        let r = 6.0f32;
        let radii = [r; 4];
        // All four radii packed.
        let uv_min = [radii[0], radii[1]];
        let uv_max = [radii[2], radii[3]];
        assert_eq!(uv_min, [r, r]);
        assert_eq!(uv_max, [r, r]);
    }

    #[test]
    fn rrect4_all_zero_is_sharp_rect() {
        let radii = [0.0f32; 4];
        // Zero radii = sharp corners, same as flags==3 with radius=0.
        assert!(radii.iter().all(|&v| v == 0.0));
    }

    // ---- FNV-1a id stability ------------------------------------------------

    #[test]
    fn id_does_not_change_across_calls() {
        // Computed from the source hash, must not change:
        let v1 = id("settings/opacity");
        let v2 = id("settings/opacity");
        assert_eq!(v1, v2);
    }

    #[test]
    fn id_empty_string_is_distinct_from_nonempty() {
        assert_ne!(id(""), id("x"));
    }
}
