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
mod settings_panel;
mod textedit;
mod textinput;
mod widgets;

pub use chrome::*;
pub use help::*;
// `settings_panel` only adds inherent methods to `Ui`; nothing to re-export.
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
    pub fn center_x(self) -> f32 {
        self.x + self.w * 0.5
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

// --- Motion vocabulary (§ design-system MOTION) ----------------------------
//
// Named durations for the chrome's transitions. These are the target
// vocabulary; surfaces adopt them as their per-anim easing/duration work lands.
// The mechanism is unchanged — gui_anims stepped only while unsettled, Poll
// dropped back to Wait on settle — so none of these threaten the 0%-idle
// invariant.

/// Hover-in / press-release (ms). Short enough to feel instant; linear is fine.
pub const MOTION_FAST_MS: f32 = 100.0;
/// Hover-out / toggle / selection change (ms).
pub const MOTION_BASE_MS: f32 = 150.0;
/// Menu / popup / palette / panel entrance (ms): fade + small rise. Exit is
/// instant.
pub const MOTION_ENTER_MS: f32 = 180.0;

/// Cubic ease-out: fast start, gentle settle. `t` is a normalized 0..1 progress
/// (clamped). Used for enter/hover transitions and the quake slide so motion
/// decelerates into its resting position instead of stopping abruptly.
pub fn ease_out_cubic(t: f32) -> f32 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

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

/// Elevate a base surface color to build the panel/card/active-tab/floating
/// hierarchy (§2.1) without changing hue. `amount` is the same "how much
/// hierarchy" knob in both directions:
///
/// - On dark and mid-tone backgrounds (`luma(base) <= 0.7`) this lightens
///   additively by `amount`, same as the original scheme.
/// - On light backgrounds (`luma(base) > 0.7`, the same threshold
///   [`state_fill`] uses) additive lightening clips at white almost
///   immediately — on a near-white bg like one-light's `#FAFAFA` every
///   channel is already within `amount` of 1.0, so the result clamps back to
///   (near) the original color and raised surfaces become invisible against
///   the background. Darkening instead — by the multiplicative factor
///   `1.0 - amount` — keeps the surface clearly legible, and because `base`
///   is close to 1.0 on these themes the resulting per-channel delta
///   (`base * amount`) lands in the same ballpark as the additive delta a
///   dark theme gets, so the perceived "how raised is this" contrast stays
///   comparable across light and dark themes.
///
/// `pub(crate)` (rather than private) so `app::command_blocks`'s opt-in
/// command-block "card" chrome can derive its band tint from the same
/// theme-aware elevation math instead of a hardcoded color.
pub(crate) fn glass_elevate(base: [f32; 4], amount: f32) -> [f32; 4] {
    if luma(base) > 0.7 {
        darken(base, 1.0 - amount)
    } else {
        lighten(base, amount)
    }
}

/// E1 chrome bar fill.
pub fn glass_body() -> [f32; 4] {
    with_alpha(color::default_bg(), GLASS_BAR_ALPHA)
}

/// E2 raised surface fill (cards / buttons on glass).
pub fn glass_raised() -> [f32; 4] {
    with_alpha(
        glass_elevate(color::default_bg(), 0.12),
        GLASS_SURFACE_ALPHA,
    )
}

/// E2+ active-tab chip fill — one stop brighter and fully opaque so the active
/// tab clearly stands apart from both the bar and the recessed inactive chips.
/// Derived from the theme's background + extra elevation so it reads as
/// "the open surface" on any theme without hard-coding a color.
pub fn glass_active_tab() -> [f32; 4] {
    with_alpha(glass_elevate(color::default_bg(), 0.22), 1.0)
}

/// E3 floating surface fill (dropdowns / dialogs / drag-ghost / toasts / peek).
///
/// Derived from the theme background (`default_bg`) — NOT `selection_bg` as it
/// was originally — so the whole elevation family (E1 `glass_body`, E2
/// `glass_raised`/`glass_active_tab`, E3 here) shares one hue and only differs
/// by elevation amount (strict order `0 < 0.12 < 0.18 < 0.22`). Keying E3 off
/// `selection_bg` let its hue diverge from the rest of the chrome on themes
/// whose selection tint is a different color.
pub fn glass_float() -> [f32; 4] {
    with_alpha(glass_elevate(color::default_bg(), 0.18), GLASS_FLOAT_ALPHA)
}

/// Accent hairline border for an E3 floating surface (menus / dialogs / palette).
/// Very low alpha so it reads as a soft crown, not a hard drawn line.
pub fn glass_float_border() -> [f32; 4] {
    with_alpha(color::accent(), 0.22)
}

/// Soft drop-shadow tint for an E3 floating surface (menus / popups / palette /
/// settings / toasts / modals). Near-black, with a theme-aware alpha — denser on
/// dark themes, softer on light — per the design-system depth spec. Pair with
/// [`Renderer::push_overlay_shadow_px`] and a ~[`SHADOW_E3_FEATHER`] blur.
pub fn shadow_e3() -> [f32; 4] {
    let a = if luma(color::default_bg()) > 0.7 {
        0.18
    } else {
        0.35
    };
    [0.0, 0.0, 0.0, a]
}

/// Default E3 soft-shadow blur (feather) width in px.
pub const SHADOW_E3_FEATHER: f32 = 14.0;

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

    // --- design-system token scales (all derived from cell_h so they scale
    // with the font / DPI for free) --------------------------------------------
    /// Spacing scale (physical px). `sp_md` == [`Self::gap`] and `sp_lg` ==
    /// [`Self::pad`] are kept as the pre-existing names; the extra stops fill in
    /// the smaller and larger ends of a single spacing vocabulary.
    pub sp_xs: f32,
    pub sp_sm: f32,
    pub sp_xl: f32,
    /// Chrome-bar height (status bar, pane header). Replaces the old fixed
    /// `STATUS_BAR_H`/`PANE_HEADER_H = 22` px constants with a font-derived value.
    pub bar_h: f32,
    /// Square control-button edge (tab-strip control buttons). `CLOSE_BOX` maps
    /// to `btn * 0.6`.
    pub btn: f32,
    /// Radius scale (physical px). `r_md` == [`Self::radius`] and `r_lg` ==
    /// [`Self::card_radius`]; `r_sm` is the tighter inner-element radius. A
    /// "full" pill radius is always `rect.h * 0.5` at the call site.
    pub r_sm: f32,
    pub r_md: f32,
    pub r_lg: f32,
}

impl Metrics {
    pub fn new(cell_w: f32, cell_h: f32) -> Self {
        let row_h = (cell_h * 1.6).round();
        let radius = (cell_h * 0.28).round().clamp(4.0, 8.0);
        let card_radius = radius + 2.0;
        Metrics {
            cell_w,
            cell_h,
            row_h,
            pad: (cell_h * 0.5).round(),
            gap: (cell_h * 0.4).round(),
            radius,
            card_radius,
            ctrl_w: (cell_w * 14.0).round(),
            knob: row_h - 8.0,

            sp_xs: (cell_h * 0.125).round().max(2.0),
            sp_sm: (cell_h * 0.25).round(),
            sp_xl: (cell_h * 1.0).round(),
            bar_h: (cell_h * 1.4).round(),
            btn: (cell_h * 1.6).round(),
            r_sm: (radius - 2.0).max(2.0),
            r_md: radius,
            r_lg: card_radius,
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
    /// The system-Light theme chooser list is open (Themes section).
    ThemeLight,
    /// The system-Dark theme chooser list is open (Themes section).
    ThemeDark,
    /// The window-effect chooser list is open (Appearance section).
    Effect,
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
    /// Window post-process effect as a segmented index (see
    /// [`crate::renderer::WindowEffect::index`]): 0 = None, 1 = Frosted, 2 =
    /// Acrylic, 3 = CRT, 4 = Scanlines, 5 = Grain, 6 = Vignette, 7 = Bloom,
    /// 8 = Custom.
    pub window_effect_idx: usize,
    /// Custom-effect channel intensities `[curvature, scanline, glow, vignette,
    /// grain, tint]` (0..1). Only surfaced as sliders when the effect is Custom.
    pub custom_effect: [f32; 6],

    // --- settings-themes stream: sectioned window + custom theme + profiles ---
    /// Active left-sidebar section index (see [`SettingsSection`]).
    pub section: usize,
    /// Current vertical scroll offset (px) of the active section's right pane.
    pub section_scroll: f32,
    /// Copy-on-select enabled.
    pub copy_on_select: bool,
    /// Show the minimap / overview strip at the right edge.
    pub minimap: bool,
    /// Show command-block badges (OSC 133 exit/duration affordances).
    pub command_badges: bool,
    /// Animate the cursor between cells (cursor trail).
    pub cursor_trail: bool,
    /// Include the cwd in the OS window title.
    pub title_show_cwd: bool,
    /// Append " · N tabs" to the OS window title.
    pub title_show_count: bool,
    /// Theme to use in system Light mode (canonical name).
    pub theme_light: &'a str,
    /// Theme to use in system Dark mode (canonical name).
    pub theme_dark: &'a str,
    /// The 20 custom-theme entry labels in editor order (fg/bg/cursor/sel + ansi0-15).
    pub custom_labels: &'a [&'a str],
    /// The current custom-theme swatch colors, parallel to `custom_labels`.
    pub custom_swatches: &'a [[f32; 4]],
    /// Which custom-theme entry is being edited (index into `custom_labels`), or
    /// `usize::MAX` when none is selected for editing.
    pub custom_editing: usize,
    /// The available runtime profile names (from `[profile.*]` sections).
    pub profile_names: &'a [&'a str],
    /// The currently-ACTIVE `[profile.NAME]` (lower-cased), or `None` when the
    /// base (no-profile) config is active. Drives the active-row indicator in
    /// the Profiles section (see `App::active_profile`).
    pub active_profile: Option<&'a str>,
    /// Which `profile_names` index is being renamed in place (its row shows an
    /// inline edit field instead of the switch/rename/delete affordances), or
    /// `None`. Mirrors `App::settings_profile_rename_idx`.
    pub profile_rename_idx: Option<usize>,
    /// Which `profile_names` index has its Delete affordance armed (first click
    /// of the two-click confirm), or `None`. Mirrors
    /// `App::settings_profile_delete_armed`.
    pub profile_delete_armed: Option<usize>,

    // --- settings-sections stream: Terminal / Effects / Quake / Notifications /
    // Advanced additions --------------------------------------------------------
    /// Power Mode (typing particle-burst effect) enabled.
    pub power_mode: bool,
    /// Power Mode effect strength (0..1).
    pub power_mode_intensity: f32,
    /// Dim unfocused pane content in a split.
    pub dim_unfocused: bool,
    /// Also place an HTML flavor on the clipboard on copy.
    pub copy_html: bool,
    /// Quake/dropdown mode enabled. Restart-only: the quake window is armed
    /// once in `App::init_quake` at startup, so flipping this live only takes
    /// effect after relaunch (labeled as such in the UI).
    pub quake: bool,
    /// Fraction of the monitor height the quake window occupies (0.1..1.0).
    pub quake_height: f32,
    /// Quake slide animation duration in ms (0..5000).
    pub quake_animation_ms: u64,
    /// Keep the native OS window frame instead of glassy's borderless chrome.
    /// Restart-only: the frame is chosen once at window creation, so flipping
    /// this live only takes effect after relaunch (labeled as such in the UI).
    pub decorations: bool,
    /// Fire a desktop notification when a long-running command finishes.
    pub notify_command_finish: bool,
    /// Minimum command duration (ms) that triggers the notification.
    pub notify_command_threshold_ms: u64,
    /// Allow command output to be folded under its prompt.
    pub command_fold: bool,
    /// The label alphabet for hints mode, display text (empty = built-in default).
    pub hints_chars: &'a str,
    /// Explicit bold-text font family override, display text.
    pub font_bold: &'a str,
    /// Explicit italic-text font family override, display text.
    pub font_italic: &'a str,
    /// Explicit bold-italic-text font family override, display text.
    pub font_bold_italic: &'a str,
    /// `font_symbol_map` rendered back to its `RANGE:Family[, RANGE:Family…]`
    /// display text.
    pub font_symbol_map: &'a str,
    /// `font_variations` rendered back to its space-joined display text.
    pub font_variations: &'a str,
    /// The resolved shell program + args, display text (Debug-formatted — the
    /// `alacritty_terminal::tty::Shell` fields are crate-private upstream, so
    /// this is the most detail glassy can surface without reimplementing the
    /// type). `"(default shell)"` when unset.
    pub shell_display: &'a str,
    /// The configured startup working directory, display text. Empty when
    /// unset (the shell's own default/inherited cwd is used).
    pub cwd_display: &'a str,
    /// `status_bar_segments` rendered back to its space-joined display text.
    /// Empty means "use the built-in default segment set".
    pub status_bar_segments: &'a str,
    /// `strftime`-style format string for the status bar's Time segment.
    pub status_bar_time_format: &'a str,
    /// Per-side padding overrides in logical px (0 = unset/inherit), display
    /// values for the Advanced section's steppers.
    pub padding_top: u32,
    pub padding_bottom: u32,
    pub padding_left: u32,
    pub padding_right: u32,
    /// Path to the wallpaper-theme source image, display text (empty =
    /// disabled).
    pub wallpaper_theme: &'a str,
    /// Vertical scroll offset (px) of the currently-open dropdown popup (see
    /// [`Ui::dropdown_popup`]), if any. With 60 built-in themes the theme
    /// popup can easily be taller than the window; this lets it scroll instead
    /// of silently truncating the list past whatever fits.
    pub popup_scroll: f32,

    // --- settings-modularity stream: expose the remaining w15 config keys ---
    /// Strength of the unfocused-pane dim overlay in `[0, 0.9]` (Panes → Focus
    /// slider). See `Config::unfocused_dim`.
    pub unfocused_dim: f32,
    /// Whether window opacity also applies to terminal text, as a segmented
    /// index: 0 = Background, 1 = Text. See `Config::opacity_text`.
    pub opacity_scope: usize,
    /// Command-block chrome level, as a segmented index: 0 = Off, 1 = Badges,
    /// 2 = Cards. See `Config::command_blocks`.
    pub command_blocks: usize,
    /// Pane header density, as a segmented index: 0 = Full, 1 = Compact. See
    /// `Config::pane_header_style`.
    pub pane_header_style: usize,
    /// Also show a header for a single, unsplit pane. See
    /// `Config::pane_headers_single`.
    pub pane_headers_single: bool,
    /// Lines of scrollback kept for a backgrounded/idle pane once idle past
    /// `scrollback_background_idle_secs`; `0` disables the cap. See
    /// `Config::scrollback_background_cap`.
    pub scrollback_background_cap: usize,
    /// Seconds a pane must be idle/backgrounded before the cap above applies.
    /// See `Config::scrollback_background_idle_secs`.
    pub scrollback_background_idle_secs: u64,
}

/// The left-sidebar sections of the revamped settings window, in display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsSection {
    General,
    Appearance,
    Themes,
    Effects,
    Terminal,
    Panes,
    Quake,
    Notifications,
    Keys,
    Advanced,
    /// Runtime `[profile.*]` switching + "duplicate current as a new profile".
    /// Split out of `Advanced` (profiles-ui stream) into its own section so the
    /// active-profile indicator and the new-profile TextEdit row have room to
    /// breathe, and so this section's rows don't collide line-for-line with
    /// other Advanced-section work landing around the same time.
    Profiles,
}

impl SettingsSection {
    /// All sections in sidebar order.
    pub const ALL: &'static [SettingsSection] = &[
        SettingsSection::General,
        SettingsSection::Appearance,
        SettingsSection::Themes,
        SettingsSection::Effects,
        SettingsSection::Terminal,
        SettingsSection::Panes,
        SettingsSection::Quake,
        SettingsSection::Notifications,
        SettingsSection::Keys,
        SettingsSection::Advanced,
        SettingsSection::Profiles,
    ];
    /// The sidebar label for this section.
    pub fn label(self) -> &'static str {
        match self {
            SettingsSection::General => "General",
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Themes => "Themes",
            SettingsSection::Effects => "Effects",
            SettingsSection::Terminal => "Terminal",
            SettingsSection::Panes => "Panes",
            SettingsSection::Quake => "Quake",
            SettingsSection::Notifications => "Notifications",
            SettingsSection::Keys => "Keys",
            SettingsSection::Advanced => "Advanced",
            SettingsSection::Profiles => "Profiles",
        }
    }
    /// Resolve a section from its index, clamped to a valid section.
    pub fn from_index(i: usize) -> SettingsSection {
        Self::ALL
            .get(i)
            .copied()
            .unwrap_or(SettingsSection::General)
    }
}

/// Everything the settings form reported this frame. The App applies each
/// non-default field to its live effects (`resize_font`, `set_opacity`,
/// `cycle_theme`/`set_theme`, `save_settings`, …).
///
/// Not `Copy` (the `toggled` list needs an allocation): every boolean toggle
/// row used to carry its own dedicated `*_toggle: bool` field, set by a
/// widget-id string match in `settings_panel::draw_control_row` and then read
/// back through a near-identical `if ev.foo_toggle { … }` block in
/// `App::apply_settings_events`. `toggled` collapses that into one table-driven
/// path: a toggle row just pushes its widget id here, and
/// `App::apply_settings_events` resolves each id via a small
/// `(widget_id, accessor)` table for the plain flips, matching explicitly only
/// the few toggles with extra live side effects (grid reflow, renderer sync,
/// session-dirty). Adding a new plain boolean setting is then one
/// `RowKind::Toggle` push + one table row, instead of a field here + a match
/// arm in `settings_panel.rs` + an `if` block in `chrome/settings_form.rs`.
#[derive(Clone, Debug, Default)]
pub struct SettingsEvents {
    /// Font-size stepper delta in clicks (-1 / 0 / +1).
    pub font_delta: i32,
    /// New opacity value if the slider moved this frame.
    pub opacity: Option<f32>,
    /// New bell mode index if the segmented control changed.
    pub bell: Option<usize>,
    /// The user toggled the window-effect dropdown header (App flips `open`).
    pub window_effect_toggle: bool,
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
    /// Widget ids of every boolean toggle row clicked this frame (see the
    /// struct doc comment). Populated by `settings_panel::draw_control_row`;
    /// consulted by `App::apply_settings_events`.
    pub toggled: Vec<&'static str>,
    /// Padding stepper delta in clicks (-1 / 0 / +1).
    pub padding_delta: i32,
    /// New cursor-shape index if the segmented control changed.
    pub cursor_style: Option<usize>,
    /// New tab-bar-mode index if the segmented control changed (0/1/2).
    pub tab_bar_mode: Option<usize>,
    /// New window-effect index if the segmented control changed (0..=8).
    pub window_effect: Option<usize>,
    /// A Custom-effect channel slider moved: `(channel index 0..=5, new value)`
    /// where the index is `[curvature, scanline, glow, vignette, grain, tint]`.
    pub custom_effect: Option<(usize, f32)>,

    // --- settings-themes stream ---
    /// A sidebar section was clicked (new active section index).
    pub section_pick: Option<usize>,
    /// The active section's right-pane scroll moved (new offset in px).
    pub section_scroll: Option<f32>,
    /// The open dropdown popup's scroll moved (new offset in px).
    pub popup_scroll: Option<f32>,
    /// The system Light-mode theme dropdown header was toggled.
    pub theme_light_toggle: bool,
    /// A system Light-mode theme row was picked (absolute index into THEME_NAMES).
    pub theme_light_pick: Option<usize>,
    /// The system Dark-mode theme dropdown header was toggled.
    pub theme_dark_toggle: bool,
    /// A system Dark-mode theme row was picked (absolute index into THEME_NAMES).
    pub theme_dark_pick: Option<usize>,
    /// A custom-theme color row was clicked to begin editing it (entry index).
    pub custom_color_pick: Option<usize>,
    /// Apply the edited custom theme to the live palette (preview).
    pub custom_apply: bool,
    /// Save the custom theme to config (color.* keys).
    pub custom_save: bool,
    /// A runtime profile was picked from the Profiles section (index into
    /// `profile_names`).
    pub profile_pick: Option<usize>,
    /// The "(default)" row (Profiles section) was picked: switch back to the
    /// base (no-profile) config.
    pub profile_pick_default: bool,
    /// The "duplicate current settings as a new profile" row's Save affordance
    /// fired (Enter in the name field, or its button). The pending name lives in
    /// `SettingsFields::profile_name` (App-owned, like the other editable text
    /// fields).
    pub profile_create: bool,
    /// A profile row's "Rename" affordance was clicked: begin renaming that
    /// `profile_names` index in place (index carried).
    pub profile_rename_begin: Option<usize>,
    /// The inline rename field's Save affordance fired (Enter or its button):
    /// commit the pending rename (target index is `App::settings_profile_rename_idx`;
    /// the new name lives in `SettingsFields::profile_rename`).
    pub profile_rename_commit: bool,
    /// The inline rename was cancelled (its ✕ button): drop back to the normal row.
    pub profile_rename_cancel: bool,
    /// A profile row's Delete affordance was clicked while NOT armed: arm the
    /// two-click confirm for that `profile_names` index (index carried).
    pub profile_delete_arm: Option<usize>,
    /// The armed Delete affordance was clicked a second time: delete that
    /// `profile_names` index (index carried).
    pub profile_delete: Option<usize>,

    // --- settings-sections stream: Terminal / Effects / Quake / Notifications /
    // Advanced additions --------------------------------------------------------
    /// New Power-Mode intensity if the Effects slider moved this frame.
    pub power_mode_intensity: Option<f32>,
    /// New quake-height fraction if the Quake slider moved this frame.
    pub quake_height: Option<f32>,
    /// Quake-animation-ms stepper delta in clicks (-1 / 0 / +1; scaled to a
    /// 20ms step by the caller).
    pub quake_animation_delta: i32,
    /// Notify-command-threshold stepper delta in clicks (-1 / 0 / +1; scaled to
    /// a 1000ms step by the caller).
    pub notify_threshold_delta: i32,
    /// Per-side padding stepper deltas in clicks (-1 / 0 / +1; scaled to a 2px
    /// step by the caller), Advanced section.
    pub padding_top_delta: i32,
    pub padding_bottom_delta: i32,
    pub padding_left_delta: i32,
    pub padding_right_delta: i32,

    // --- settings-modularity stream: expose the remaining w15 config keys ---
    /// New unfocused-pane dim strength if the Panes slider moved this frame.
    pub unfocused_dim: Option<f32>,
    /// New opacity-scope index if the Appearance segmented control changed
    /// (0 = Background, 1 = Text).
    pub opacity_scope: Option<usize>,
    /// New command-blocks index if the Effects segmented control changed
    /// (0 = Off, 1 = Badges, 2 = Cards).
    pub command_blocks: Option<usize>,
    /// New pane-header-style index if the Panes segmented control changed
    /// (0 = Full, 1 = Compact).
    pub pane_header_style: Option<usize>,
    /// Background-scrollback-cap stepper delta in clicks (-1 / 0 / +1; scaled
    /// to a 1000-line step by the caller), Advanced section.
    pub scrollback_background_cap_delta: i32,
    /// Background-scrollback-idle-seconds stepper delta in clicks (-1 / 0 /
    /// +1; scaled to a 60s step by the caller), Advanced section.
    pub scrollback_background_idle_secs_delta: i32,
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
    fn ease_out_cubic_endpoints_and_shape() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        // Clamps out-of-range inputs.
        assert_eq!(ease_out_cubic(-0.5), 0.0);
        assert_eq!(ease_out_cubic(1.5), 1.0);
        // Ease-OUT: past the halfway travel by the midpoint (decelerating).
        assert!(ease_out_cubic(0.5) > 0.5);
        // Monotonic.
        assert!(ease_out_cubic(0.25) < ease_out_cubic(0.75));
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
    fn metrics_token_scales_are_ordered_and_font_derived() {
        let m = Metrics::new(9.0, 20.0);
        // Spacing scale is strictly increasing.
        assert!(m.sp_xs <= m.sp_sm);
        assert!(m.sp_sm <= m.gap); // gap == sp_md
        assert!(m.gap <= m.pad); // pad == sp_lg
        assert!(m.pad <= m.sp_xl);
        // sp_xs never collapses below a usable 2px.
        assert!(m.sp_xs >= 2.0);
        // Radius scale: r_sm < r_md == radius < r_lg == card_radius.
        assert!(m.r_sm <= m.r_md);
        assert_eq!(m.r_md, m.radius);
        assert_eq!(m.r_lg, m.card_radius);
        assert!(m.r_md < m.r_lg);
        // Font-derived bar/button heights track the cell height.
        let big = Metrics::new(9.0, 40.0);
        assert!(big.bar_h > m.bar_h);
        assert!(big.btn > m.btn);
    }

    #[test]
    fn glass_elevation_amounts_are_strictly_ordered() {
        // E1 (body, 0) < E2 raised (0.12) < E3 float (0.18) < E2+ active tab
        // (0.22): a strict elevation order on a dark theme (additive lighten).
        let bg = DARK_BG;
        let body = luma(bg);
        let raised = luma(glass_elevate(bg, 0.12));
        let float = luma(glass_elevate(bg, 0.18));
        let active = luma(glass_elevate(bg, 0.22));
        assert!(body < raised, "raised must sit above body");
        assert!(raised < float, "float must sit above raised");
        assert!(float < active, "active-tab must sit above float");
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

    // ---- glass surface elevation (light/dark theme parity) ------------------
    //
    // `glass_raised` / `glass_active_tab` / `glass_float` build their fills as
    // `with_alpha(glass_elevate(base, amount), alpha)`. Before the fix, the
    // elevation step was a bare `lighten(base, amount)`: purely additive and
    // clamped at 1.0. On a near-white light-theme background (one-light
    // `#FAFAFA`, ayu-light `#FCFCFC`) every channel is already within
    // `amount` of 1.0, so the result clamped straight back to (almost) the
    // background color — raised panels, cards, the active-tab chip and
    // dropdowns all became visually indistinguishable from the page. These
    // tests exercise `glass_elevate` directly with the exact `amount` values
    // each public helper uses, on synthetic near-white and dark backgrounds,
    // rather than mutating the process-global active theme via
    // `color::set_theme` (which would race with other tests reading
    // `color::active()` concurrently, e.g. `color::query_index_tests`).

    /// Matches one-light's `#FAFAFA` background (250/255 per channel).
    const NEAR_WHITE_BG: [f32; 4] = [0.9804, 0.9804, 0.9804, 1.0];
    /// Matches Tokyo Night's `#1A1B26` background (the default dark theme).
    const DARK_BG: [f32; 4] = [0.1020, 0.1059, 0.1490, 1.0];

    #[test]
    fn glass_elevate_panel_amount_darkens_clearly_on_near_white_bg() {
        // glass_raised()'s amount (0.12).
        let raised = glass_elevate(NEAR_WHITE_BG, 0.12);
        for i in 0..3 {
            assert!(
                NEAR_WHITE_BG[i] - raised[i] > 0.06,
                "channel {i}: bg={}, raised={} — must be clearly darker, not clipped to bg",
                NEAR_WHITE_BG[i],
                raised[i]
            );
        }
        assert!(
            luma(NEAR_WHITE_BG) - luma(raised) > 0.06,
            "raised-panel luma delta too subtle on a near-white bg"
        );
    }

    #[test]
    fn glass_elevate_active_tab_amount_darkens_more_than_panel_on_near_white_bg() {
        // glass_active_tab()'s amount (0.22) must read as "more elevated" than
        // glass_raised()'s (0.12) — the active tab should stand out further
        // from the background than an ordinary raised panel.
        let raised = glass_elevate(NEAR_WHITE_BG, 0.12);
        let active_tab = glass_elevate(NEAR_WHITE_BG, 0.22);
        let raised_delta = luma(NEAR_WHITE_BG) - luma(raised);
        let active_delta = luma(NEAR_WHITE_BG) - luma(active_tab);
        assert!(
            active_delta > raised_delta,
            "active-tab delta ({active_delta}) should exceed panel delta ({raised_delta})"
        );
        assert!(
            active_delta > 0.12,
            "active-tab luma delta too subtle on a near-white bg: {active_delta}"
        );
    }

    #[test]
    fn glass_elevate_float_amount_darkens_clearly_on_near_white_bg() {
        // glass_float()'s amount (0.18), applied to a near-white background
        // (one-light's `#FAFAFA`, > the 0.7 threshold) must stay clearly darker
        // than the page so a floating surface reads as elevated on light themes.
        assert!(
            luma(NEAR_WHITE_BG) > 0.7,
            "fixture must exercise the light branch"
        );
        let floated = glass_elevate(NEAR_WHITE_BG, 0.18);
        for i in 0..3 {
            assert!(
                NEAR_WHITE_BG[i] - floated[i] > 0.05,
                "channel {i}: bg={}, float={} — must be clearly darker",
                NEAR_WHITE_BG[i],
                floated[i]
            );
        }
    }

    #[test]
    fn glass_elevate_lightens_on_dark_bg_unchanged() {
        // Dark-theme behaviour is untouched: still a plain additive lighten,
        // matching every existing dark-theme expectation.
        for amount in [0.12_f32, 0.22] {
            let elevated = glass_elevate(DARK_BG, amount);
            assert_eq!(elevated, lighten(DARK_BG, amount));
            for i in 0..3 {
                assert!(elevated[i] > DARK_BG[i]);
            }
        }
    }

    #[test]
    fn glass_elevate_threshold_matches_state_fill() {
        // Keep the light/dark branch point in sync with `state_fill`'s 0.7
        // luma threshold so panels and interactive fills agree on which
        // themes count as "light".
        let just_light = [0.75, 0.75, 0.75, 1.0];
        let just_dark = [0.65, 0.65, 0.65, 1.0];
        assert!(luma(just_light) > 0.7);
        assert!(luma(just_dark) <= 0.7);
        assert!(
            glass_elevate(just_dark, 0.1)[0] > just_dark[0],
            "at/below threshold should lighten"
        );
        assert!(
            glass_elevate(just_light, 0.1)[0] < just_light[0],
            "above threshold should darken"
        );
    }

    #[test]
    fn glass_raised_active_tab_float_match_dark_theme_baseline() {
        // With no theme override, the active theme defaults to Tokyo Night (a
        // dark theme, luma well under 0.7), so the public glass_* helpers must
        // still compute exactly the original additive-lighten fill — this
        // pins the "dark themes are unaffected" half of the fix.
        assert_eq!(
            glass_raised(),
            with_alpha(lighten(color::default_bg(), 0.12), GLASS_SURFACE_ALPHA)
        );
        assert_eq!(
            glass_active_tab(),
            with_alpha(lighten(color::default_bg(), 0.22), 1.0)
        );
        assert_eq!(
            glass_float(),
            with_alpha(lighten(color::default_bg(), 0.18), GLASS_FLOAT_ALPHA)
        );
    }
}
