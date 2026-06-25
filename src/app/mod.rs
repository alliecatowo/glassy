//! The winit UI/render driver.
//!
//! Idle behaviour is `ControlFlow::Wait`: 0% CPU, no GPU submits until the PTY
//! thread (or a resize/input) wakes us. Wakeups set a dirty flag and are
//! coalesced to at most one frame per monitor refresh, so a fast producer like
//! Claude Code streaming tokens collapses into a single redraw per refresh
//! instead of one redraw per token burst.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alacritty_terminal::grid::{Dimensions, Indexed, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::tty::Shell;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{Window, WindowId};

use crate::bell::{self, AudioBell};
use crate::color::{self, lighten};
use crate::gui;
use crate::input::{KittyFlags, ModifyOtherKeys, MouseReport, encode_key_parts, encode_mouse};
use crate::pane;
use crate::pty::{Pty, UserEvent};
use crate::renderer::{CursorOverlay, Decorations, LigatureCell, Renderer, UnderlineStyle};

mod chrome;
mod event_loop;
mod helpers;
mod input;
mod keys;
mod mouse;
mod multipane;
mod palette;
mod panes;
mod render;
mod script;
mod search;
mod selection;
mod settings;
mod tabs;

pub(crate) use helpers::*;
pub(crate) use palette::PaletteState;
pub(crate) use search::SearchState;

/// A runtime font-size adjustment requested via Ctrl +/-/0.
#[derive(Clone, Copy)]
pub(crate) enum FontStep {
    Inc,
    Dec,
    Reset,
}

/// Static configuration resolved at startup (config file + CLI overrides).
pub struct Config {
    /// Preferred font family name; `None` uses the discovery default (FiraCode…).
    pub font_family: Option<String>,
    /// Logical font size in points (scaled by the monitor's DPI factor).
    pub font_size: f32,
    /// Window background opacity in [0, 1]. 1.0 is fully opaque.
    pub opacity: f32,
    /// Padding (grid inset) override in logical px; `None` derives it from the
    /// cell height.
    pub padding: Option<f32>,
    /// Per-side padding overrides in logical px. When set, these override the
    /// uniform `padding` for their respective sides. `None` means use `padding`
    /// or the cell-derived default.
    pub padding_top: Option<f32>,
    pub padding_bottom: Option<f32>,
    pub padding_left: Option<f32>,
    pub padding_right: Option<f32>,
    /// Lines of scrollback history to retain.
    pub scrollback: usize,
    /// Shell program + args; `None` uses the user's default shell.
    pub shell: Option<Shell>,
    /// Flash the window briefly on the terminal bell. Default true.
    pub bell_visual: bool,
    /// Play a short soft beep on the terminal bell. Default false. Only audible in
    /// a build compiled with the `bell-audio` feature.
    pub bell_audible: bool,
    /// Canonical name of the active color theme (one of `color::THEME_NAMES`),
    /// tracked so the settings overlay can show, cycle, and save it.
    pub theme: String,
    /// Follow the system light/dark color scheme: when true, the active theme is
    /// chosen at startup and on `ThemeChanged` from `theme_light` / `theme_dark`
    /// according to the OS preference, instead of pinning `theme`.
    pub follow_system: bool,
    /// Theme to use when the system prefers a LIGHT color scheme (and
    /// `follow_system` is on). Canonical [`color::THEME_NAMES`] entry.
    pub theme_light: String,
    /// Theme to use when the system prefers a DARK color scheme (and
    /// `follow_system` is on). Canonical [`color::THEME_NAMES`] entry.
    pub theme_dark: String,
    /// Show the status bar at the bottom of the window. Default false.
    pub status_bar: bool,
    /// Show per-pane title bars (with the close box / split menu) and the accent
    /// top-rail on each pane when the tab is split. Default false (off by
    /// default; enable via `pane_headers = true` in the config or the settings
    /// toggle). When false, panes use their full height with no header chrome.
    pub pane_headers: bool,
    /// Word separator characters for text selection. Whitespace chars from this string
    /// (plus the default whitespace + punctuation) are used as word boundaries.
    /// Empty string means use defaults only.
    pub word_separator: String,
    /// Enable ligature shaping: shape full cell-runs through cosmic-text so
    /// OpenType GSUB liga substitutions (e.g. `->` → `→`, `fi` ligature) are
    /// applied across cell boundaries. Default false (opt-in) because it adds a
    /// per-run shaping pass and may not be desirable for all fonts. Only takes
    /// effect when the loaded font actually carries a `liga` GSUB feature.
    pub ligatures: bool,
    /// Optional list of OpenType font feature tags to enable or disable during
    /// shaping, e.g. `["ss01", "calt=0"]`. Each entry is either a bare 4-byte
    /// tag (enabled, value=1) or `tag=<u32>` (explicit value; 0=disable).
    /// Passed directly to cosmic-text `Attrs::font_features`. Defaults to empty
    /// (all features left at their font-defined defaults).
    pub font_features: Vec<String>,
    /// Working directory for the FIRST tab's shell, from a `cwd` config key or an
    /// activated `[profile.NAME]`. `None` opens in the inherited/default directory.
    pub initial_cwd: Option<std::path::PathBuf>,
    /// Restore the previous session's tabs + splits + cwds on launch (from the
    /// state file written on exit). Default false; opt in via `restore_session`
    /// config key or `--restore-session`.
    pub restore_session: bool,
    /// The effective keybinding map (user overrides layered on the built-in
    /// defaults). Built once at config resolution time by [`crate::config`] and
    /// consulted by the keyboard handler before the hard-coded fallback paths.
    pub keymap: crate::config::KeyMap,
}

/// A tab's split layout: the tiling tree (whose leaf ids are pty/pane ids) plus
/// the parked PTYs of every pane EXCEPT the focused one. The focused pane's PTY
/// lives in `App::pty` (active tab) or `Session::pty` (parked tab), exactly like
/// the single-pane model — so all the existing single-session code keeps working
/// unchanged, and `panes == None` is byte-identical to today's one-pane tab.
struct PaneGroup {
    layout: pane::Layout,
    /// Non-focused panes' PTYs, keyed by pane id (== leaf id == pty id).
    others: HashMap<usize, Pty>,
    /// Per-leaf OSC window title, keyed by pane id. Tracks all panes including
    /// the focused one (whose OSC title is also in `App::active_title`). New
    /// panes start with an empty string (displayed as "shell" in the header).
    others_titles: HashMap<usize, String>,
}

/// One terminal tab. The *active* tab's PTY lives directly in `App::pty` (so all
/// rendering/input code stays single-session); inactive tabs are parked here and
/// swapped in on switch.
struct Session {
    id: usize,
    pty: Pty,
    /// Split layout for this parked tab. `None` for a single-pane tab; when set,
    /// `pty` above holds the focused pane and `panes.others` the rest.
    panes: Option<PaneGroup>,
    title: String,
    /// Set when this background tab produces output; shown as a dot on its chip
    /// and cleared when the tab is activated. Lets you see which tab is busy.
    activity: bool,
    /// While this session is actively producing output, the deadline after which
    /// it counts as idle again. Re-armed on every PTY wakeup; cleared (back to
    /// `None`) once elapsed in `about_to_wait`. Drives the chip's busy spinner.
    busy_until: Option<Instant>,
    /// Last working directory reported by this session's focused pane via OSC 7,
    /// so a new tab/split opened from this tab inherits the cwd. `None` until the
    /// shell emits OSC 7 (or for shells that never do).
    last_cwd: Option<std::path::PathBuf>,
    /// User-assigned custom title (double-click rename). Overrides the OSC `title`
    /// for the chip when set. `None` uses the OSC title.
    custom_title: Option<String>,
    /// Per-pane last cwd for non-focused panes of this parked tab (focused pane's
    /// is `last_cwd`). Keyed by pane id; used for session persistence so each pane
    /// of a split restores in its own directory.
    pane_cwds: std::collections::HashMap<usize, std::path::PathBuf>,
}

pub struct App {
    proxy: EventLoopProxy<UserEvent>,
    config: Config,

    // Created lazily in `resumed()` (winit requires the window there).
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    pty: Option<Pty>,
    /// Split layout for the ACTIVE tab. `None` when the active tab has one pane
    /// (the common case — then `pty` is the sole pane and every existing path
    /// runs unchanged). When set, `pty` is the focused pane and `panes.others`
    /// holds the rest; rendering/input fan out across `panes.layout`.
    panes: Option<PaneGroup>,

    // Tabs. The active tab's PTY is `pty`; inactive tabs are parked in
    // `background` (an unordered pool keyed by id). `tab_order` is the STABLE
    // left-to-right display order of all tab ids — switching tabs only moves the
    // highlight, it never reorders or renumbers (drag-reorder mutates this).
    background: Vec<Session>,
    /// Stable left-to-right display order of all tab ids (active + background).
    tab_order: Vec<usize>,
    /// Stable id of the active session.
    active_id: usize,
    /// Title reported by the active session (OSC), for the tab strip.
    active_title: String,
    /// User-assigned custom title for the ACTIVE tab (double-click rename), which
    /// overrides `active_title` in the chip. `None` uses the OSC title.
    active_custom_title: Option<String>,
    /// Per-pane last cwd for the ACTIVE tab's non-focused panes (the focused pane's
    /// is `active_cwd`). Keyed by pane id; persisted so each split pane restores in
    /// its own directory.
    active_pane_cwds: std::collections::HashMap<usize, std::path::PathBuf>,
    /// Inline tab-rename editor: `Some((pos, buffer))` while a tab chip at stable
    /// position `pos` is being renamed; the buffer is the in-progress text. Enter
    /// commits, Esc cancels. `None` when not renaming.
    tab_rename: Option<(usize, String)>,
    /// Last tab-chip click `(pos, time)`, for double-click rename detection. A
    /// second click on the same chip within the multi-click window opens the
    /// inline rename editor.
    last_tab_click: Option<(usize, Instant)>,
    /// Set when the tab/split structure changed and the session file should be
    /// re-persisted. Flushed (debounced) in `about_to_wait` so a burst of changes
    /// writes once. A no-op when `restore_session` is off.
    session_dirty: bool,
    /// Last working directory reported by the active session via OSC 7. New
    /// tabs/splits inherit it so they open where the user is, not in `$HOME`.
    /// Parked sessions keep their own in `Session::last_cwd`.
    active_cwd: Option<std::path::PathBuf>,
    /// Next session id to assign.
    next_id: usize,

    cols: usize,
    rows: usize,

    /// The configured font size in physical px, captured at startup; Ctrl 0 resets
    /// the runtime size to this. `None` until the window/renderer exist.
    base_font_px: Option<f32>,

    mods: ModifiersState,
    focused: bool,
    /// While a tab chip is held, its current stable position — set on press,
    /// updated as the pointer drags it over other slots (reorders `tab_order`),
    /// cleared on release. `None` when not dragging a tab.
    dragging_tab: Option<usize>,
    /// While a pane resize gutter is held, the handle being dragged (which split
    /// divider, and the axis geometry to map the pointer back to a ratio). Set on
    /// a left-press over a gutter, cleared on release. `None` when not dragging.
    dragging_gutter: Option<pane::SplitHandle>,
    /// The gutter currently under the pointer (not yet dragged), so the divider
    /// can be drawn transiently brighter/fatter as hover feedback. `None` off any
    /// gutter. Kept distinct from `dragging_gutter` so hover and drag share paint.
    hovered_gutter: Option<pane::SplitHandle>,
    /// The toolbar item currently under the pointer, for hover highlighting.
    hovered_strip_item: Option<StripItem>,
    /// The toolbar item the left button is currently pressed on, for the PRESSED
    /// (inset/darker) treatment. Set on press over a strip item, cleared on
    /// release — gives clicks real tactility distinct from hover.
    held_strip_item: Option<StripItem>,
    /// Accumulated scroll/swipe delta while the pointer is over the tab strip,
    /// so a touchpad 2-finger swipe (many small deltas) cycles tabs smoothly.
    tab_scroll_accum: f32,
    /// Accumulated touchpad pixel delta for terminal-content scrolling. Touchpads
    /// stream many sub-line deltas; accumulating avoids truncating each to zero.
    content_scroll_accum: f32,
    /// True once the current touchpad swipe over the strip has already switched a
    /// tab, so one continuous swipe moves exactly one tab (no twitchy carousel).
    /// Reset when the gesture starts/ends.
    swipe_consumed: bool,
    /// Wall-clock at App construction + whether the first frame has been timed,
    /// so we can log time-to-first-frame once (a startup benchmark).
    started: Instant,
    first_frame_done: bool,
    /// Whether the F1 help overlay is currently shown.
    help_open: bool,
    /// Whether the Ctrl+, settings overlay is currently shown.
    settings_open: bool,
    /// Which settings dropdown (theme / font family) is currently expanded.
    /// Keyboard focus is tracked by the shared `gui_focused` widget id.
    settings_drop: gui::SettingsDrop,
    /// Bounding rect of the settings panel from the last paint, for click-outside
    /// dismissal in the mouse handler.
    settings_panel: gui::Rect,
    /// True briefly after a successful settings save, for the overlay's status line.
    settings_saved: bool,
    /// Whether the # hamburger dropdown menu is currently shown.
    menu_open: bool,
    /// Currently-highlighted row in the dropdown menu (keyboard nav).
    menu_sel: usize,
    /// When the dropdown is the right-click context menu, the items it shows
    /// (selection-aware). `None` means the dropdown is the # hamburger (uses
    /// `MenuAction::ALL`). Drives both draw and hit-test.
    menu_items: Option<Vec<MenuAction>>,
    /// Screen-cell anchor (col, row) for the open dropdown panel. Set for both
    /// the hamburger and the context menu so the render site is branch-free.
    /// Retained for the legacy cell-based `menu_hit_test` (until fully replaced).
    menu_anchor: Option<(usize, usize)>,
    /// Pixel anchor (x, y) for the real GUI menu panel (§3.6). Replaces the
    /// cell-based `menu_anchor` for the `gui::menu` draw + hit-test path.
    menu_anchor_px: Option<(f32, f32)>,
    /// Scroll position for the F1 help panel (§3.7). Preserved across opens so
    /// the user returns to where they left off.
    help_state: gui::HelpState,

    // --- Tab right-click context menu ----------------------------------------
    /// When a tab chip is right-clicked: the stable `tab_order` position the menu
    /// acts on (Close / Close others / Rename / Duplicate / Move left/right).
    /// `None` when no tab menu is open. Drawn with `gui::menu`, anchored at the
    /// pointer in `tab_menu_anchor_px`.
    tab_menu_target: Option<usize>,
    /// Currently-highlighted row in the open tab context menu (keyboard nav).
    tab_menu_sel: usize,
    /// Pixel anchor (x, y) for the tab context-menu panel.
    tab_menu_anchor_px: Option<(f32, f32)>,

    // --- Pane title-bar ⋮ menu -----------------------------------------------
    /// The pane header currently under the pointer: `(pane id, in ⋮ button)`, or
    /// `None` when the pointer is off every header. Cached so `CursorMoved` only
    /// repaints on an enter/leave or button-edge change rather than every pixel.
    hovered_pane_header: Option<(usize, bool)>,
    /// When the ⋮ pane-menu is open: the pane id that owns it. The menu shows
    /// Split V / Split H / Close pane. `None` when no pane menu is open.
    pane_menu_open: Option<usize>,
    /// Currently-highlighted row in the open pane menu (0-based, keyboard nav).
    pane_menu_sel: usize,

    // Mouse reporting state.
    /// Last known cursor cell (col, row), clamped to the grid.
    mouse_cell: (usize, usize),
    /// Last raw cursor position in physical pixels (for sub-cell side tests).
    mouse_px: (f64, f64),
    /// Button currently held (for drag reports); base id 0=L/1=M/2=R.
    held_button: Option<u8>,
    /// True while the left button drives a glassy-side text selection (i.e. a
    /// press that started while NOT in mouse-reporting mode).
    selecting: bool,
    /// Click-chain state for double/triple click detection: (cell, count, time).
    last_click: Option<((usize, usize), u32, Instant)>,
    /// URI of the OSC8 hyperlink currently under the pointer (for hover underline).
    hovered_link: Option<String>,

    /// Lazily-created OS clipboard handle (arboard). `None` until first use, and
    /// stays `None` if the platform clipboard is unavailable.
    clipboard: Option<arboard::Clipboard>,

    // Render-on-demand throttle state.
    dirty: bool,
    next_frame: Instant,
    refresh: Duration,

    // Visual-bell flash state: when set, the renderer overlays a low-alpha flash
    // tint until this instant, then we restore. Driven by the render-on-demand
    // WaitUntil timer; cleared (back to ControlFlow::Wait) once elapsed.
    bell_flash_until: Option<Instant>,
    /// Audible-bell player (lazy; holds the audio device open after the first
    /// ring). A no-op when built without the `bell-audio` feature.
    audio_bell: AudioBell,

    // Cursor blink state. `blink_on` is the current visible phase; `blink_at` is
    // when the next phase flip is due. Blinking only runs while focused and the
    // child requested a blinking cursor; otherwise we stay on `ControlFlow::Wait`
    // (0% idle) and keep the cursor solid.
    blink_on: bool,
    blink_at: Instant,
    /// Whether the focused pane's child requested a blinking, non-hidden cursor.
    /// Cached from the last render (where the term lock is already held) so
    /// `about_to_wait` — which fires on every CursorMoved/Wakeup — does not take
    /// the term lock just to decide whether to keep the blink timer running,
    /// avoiding lock contention with the PTY thread during output bursts.
    cursor_blinks: bool,

    // Tab busy-spinner state. `active_busy_until` is the active session's busy
    // deadline (the parked sessions keep their own in `Session::busy_until`).
    // `spinner_at` is when the next spinner frame is due and `spinner_frame` the
    // current glyph index. The spinner only animates while some tab is busy;
    // otherwise we never schedule a wakeup for it (preserving the 0%-idle path).
    active_busy_until: Option<Instant>,
    spinner_frame: usize,
    spinner_at: Instant,

    // Headless capture: when `GLASSY_CAPTURE` is set, render after a short delay
    // (so the shell has produced output), write a PPM, and exit.
    capture: Option<std::path::PathBuf>,
    capture_deadline: Option<Instant>,

    // Scripted-input test harness: when `GLASSY_SCRIPT` is set, a queue of parsed
    // commands drives the REAL mouse/keyboard/render handlers headlessly (one step
    // per `about_to_wait` wake), then exits. `None` on the normal interactive path,
    // so the 0%-idle invariant is untouched. See `app/script.rs`.
    script: Option<script::ScriptRunner>,

    // --- Per-frame damage tracking (drives the renderer's per-row updates). ---
    /// Force a full grid rebuild on the next frame regardless of terminal damage.
    /// Set on resize / font change / first frame, where the per-row layout or all
    /// content changes at once.
    force_full_redraw: bool,
    /// Digest of the tab-bar painter inputs the last time it was rebuilt. The tab
    /// bar overlay is otherwise re-shaped (every tab title glyph) every frame even
    /// while only a pane's cells change. When this frame's computed key matches, the
    /// renderer replays the cached tab-bar overlay instead of repainting. `None`
    /// forces a rebuild (first frame / cache invalidated).
    tab_bar_key: Option<u64>,
    /// The cursor cell (col, row) drawn last frame, so we can repaint the row it
    /// vacated (alacritty's own cursor damage covers the terminal cursor move, but
    /// glassy's blink/focus/selection overlays are not part of that damage).
    prev_cursor: Option<(usize, usize)>,
    /// Scrollback display offset rendered last frame; a change means the whole
    /// viewport scrolled and every row must be rebuilt.
    prev_display_offset: i32,
    /// Whether a text selection existed last frame. A selection spans arbitrary
    /// rows and is not part of terminal damage, so any change forces a full
    /// rebuild (selections only change during interactive drags, which are rare
    /// relative to streaming output).
    prev_has_selection: bool,

    // --- Real-GUI chrome layer (immediate-mode; see src/gui.rs). ---
    /// The widget currently latched by a left-button press, carried across frames
    /// so press→release resolves on the same widget.
    gui_pressed: Option<gui::WidgetId>,
    /// The widget holding keyboard focus (Tab/arrow nav), carried across frames.
    gui_focused: Option<gui::WidgetId>,
    /// Per-widget animations (hover fades, toggle slides). The event loop stays on
    /// `ControlFlow::Poll` only while some entry here is unsettled; otherwise it
    /// parks on `Wait` (0% idle).
    gui_anims: std::collections::HashMap<gui::WidgetId, gui::Anim>,
    /// Press→release click edge captured by the MouseInput handler and consumed by
    /// the next chrome paint. Set on left-release, cleared after the GUI frame.
    gui_click_edge: bool,
    /// Mouse position (physical px) at the instant `gui_click_edge` was set (i.e.
    /// at the moment of the button release). Overlay hit tests use this position
    /// instead of the current `mouse_px` so that pointer motion between the release
    /// event and the next render frame does not shift the hit-test result — the most
    /// common cause of "overlay closes immediately after opening" and
    /// "motion dismisses help" bugs.
    gui_click_pos: (f32, f32),
    /// Set when an overlay (settings / help) was opened by a gesture whose RELEASE
    /// lands OUTSIDE the overlay panel — the cog/`?` strip icons (opened on the
    /// press, released outside) and the command-palette rows (activated on a
    /// release outside the centered panel). It marks that opening release so it is
    /// NOT treated as a click-outside-the-panel dismiss: the settings-dismiss guard
    /// (`handle_mouse_input`) consumes it, and `build_help` skips its scrim-close
    /// while it is set. It is cleared exactly where the click edge is consumed (the
    /// render reset) and on every overlay close (Esc, ✕, opening another overlay),
    /// so it can never linger past the gesture that set it and swallow a later
    /// genuine dismiss. NOT set for keyboard opens (F1/Ctrl+,) or hamburger-menu
    /// rows, whose opening release never flows through a dismiss/paint that reads it.
    overlay_opened_by_press: bool,
    /// Last instant the GUI animations were stepped, for dt computation.
    gui_anim_last: Instant,

    /// xterm modifyOtherKeys level tracked from CSI > 4 ; N m sequences intercepted
    /// in the PTY byte stream. Forwarded to encode_key so modified printable keys
    /// emit the CSI 27 ; mods ; code ~ form expected by legacy-mode TUIs.
    modify_other_keys: ModifyOtherKeys,

    /// In-terminal find bar (Ctrl+Shift+F). `Some` exactly while it is open; it
    /// owns the keyboard and paints a bottom bar + match highlights. See
    /// [`search`].
    search: Option<SearchState>,
    /// Command palette (Ctrl+Shift+P). `Some` exactly while it is open; it owns
    /// the keyboard and paints a centered fuzzy action list. See [`palette`].
    palette: Option<PaletteState>,
    /// Filtered-row rects of the palette list from the last paint, for mouse
    /// hover/click hit-testing. Each is `(filtered_index, rect)`. Rebuilt every
    /// palette paint; empty when the palette is closed.
    palette_rows: Vec<(usize, gui::Rect)>,

    /// Latest OSC 9;4 progress state for the active session. `None` once
    /// `ProgressState::Remove` is received or the session exits.
    active_progress: Option<crate::image::ProgressState>,

    // --- Text-blink SGR 5/6 state -----------------------------------------------
    /// Current text-blink phase: `true` = cells visible, `false` = cells hidden.
    /// Mirrors `blink_on` for the cursor, but controls the SGR 5/6 text-blink
    /// timer (driven at the same cadence). Only active while `text_blink_active`.
    text_blink_on: bool,
    /// When the next text-blink phase flip is due. Lazily seeded the first time
    /// a `TextBlinkPresent` event is received; thereafter advanced by `BLINK_INTERVAL`.
    text_blink_at: Instant,
    /// True while the active session has blinking text (SGR 5/6 cells present).
    /// Armed by `UserEvent::TextBlinkPresent`; cleared when the grid is cleared
    /// (RIS / CSI 2J / screen erase) or the session exits.
    text_blink_active: bool,
}

#[cfg(test)]
mod tests {
    use super::{
        MenuAction, StripItem, WheelAction, actions_to_entries, image_dst_size, motion_button,
        move_in_order, strip_item_at, strip_layout, wheel_action,
    };
    use crate::gui::MenuEntry;

    #[test]
    fn context_menu_entries_group_with_separators() {
        // The rich right-click menu, with no selection: Copy is present but
        // disabled; separators fall on every group boundary (clipboard | buffer |
        // layout | app).
        let items = [
            MenuAction::Copy,
            MenuAction::Paste,
            MenuAction::SelectAll,
            MenuAction::ClearScrollback,
            MenuAction::Search,
            MenuAction::SplitRight,
            MenuAction::SplitDown,
            MenuAction::NewTab,
            MenuAction::Settings,
            MenuAction::Help,
        ];
        let entries = actions_to_entries(&items, false);
        // Item count preserved; 3 group boundaries among these → 3 separators.
        let item_count = entries
            .iter()
            .filter(|e| matches!(e, MenuEntry::Item { .. }))
            .count();
        let sep_count = entries
            .iter()
            .filter(|e| matches!(e, MenuEntry::Separator))
            .count();
        assert_eq!(item_count, items.len());
        assert_eq!(sep_count, 3);
        // First item is Copy, disabled because has_selection=false.
        match &entries[0] {
            MenuEntry::Item { label, enabled, .. } => {
                assert_eq!(*label, "Copy");
                assert!(!*enabled);
            }
            _ => panic!("first entry should be the Copy item"),
        }
        // With a selection, Copy is enabled.
        let entries_sel = actions_to_entries(&items, true);
        match &entries_sel[0] {
            MenuEntry::Item { enabled, .. } => assert!(*enabled),
            _ => panic!("first entry should be the Copy item"),
        }
    }

    #[test]
    fn hamburger_menu_groups_layout_and_destructive() {
        // The hamburger is now (NewTab, CloseTab): Settings/Help have dedicated
        // strip icons and PaneHeaders lives in the Settings form, so neither is
        // duplicated here. NewTab is in the layout group, CloseTab in the
        // destructive group → exactly one separator at that single boundary.
        let entries = actions_to_entries(MenuAction::ALL, false);
        let item_count = entries
            .iter()
            .filter(|e| matches!(e, MenuEntry::Item { .. }))
            .count();
        let sep_count = entries
            .iter()
            .filter(|e| matches!(e, MenuEntry::Separator))
            .count();
        assert_eq!(item_count, MenuAction::ALL.len());
        assert_eq!(sep_count, 1);
        // The last entry is always the destructive Close-tab item.
        match entries.last() {
            Some(MenuEntry::Item { label, .. }) => assert_eq!(*label, "Close tab"),
            _ => panic!("hamburger must end with Close tab"),
        }
        // Settings and Help are NOT in the hamburger anymore (strip icons own them).
        let labels: Vec<&str> = entries
            .iter()
            .filter_map(|e| match e {
                MenuEntry::Item { label, .. } => Some(*label),
                _ => None,
            })
            .collect();
        assert!(!labels.contains(&"Settings"));
        assert!(!labels.contains(&"Help / keys"));
        assert!(!labels.contains(&"Pane headers"));
    }

    #[test]
    fn settings_focus_order_matches_gui_ids_and_is_distinct() {
        use crate::gui;
        let order = super::App::settings_focus_order();
        // Each entry must equal the corresponding form widget id (build_settings).
        assert_eq!(order[0], gui::id("settings/font_size"));
        assert_eq!(order[1], gui::id("settings/opacity"));
        assert_eq!(order[2], gui::id("settings/bell"));
        assert_eq!(order[3], gui::id("settings/theme"));
        assert_eq!(order[4], gui::id("settings/font_family"));
        assert_eq!(order[5], gui::id("settings/scrollback"));
        assert_eq!(order[6], gui::id("settings/padding"));
        assert_eq!(order[7], gui::id("settings/status_bar"));
        assert_eq!(order[8], gui::id("settings/pane_headers"));
        assert_eq!(order[9], gui::id("settings/follow_system"));
        assert_eq!(order[10], gui::id("settings/ligatures"));
        assert_eq!(order[11], gui::id("settings/restore_session"));
        assert_eq!(order[12], gui::id("settings/config"));
        assert_eq!(order[13], gui::id("settings/save"));
        for (i, a) in order.iter().enumerate() {
            for b in order.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }

    #[test]
    fn move_in_order_reorders() {
        let mut v = vec![10, 20, 30, 40];
        move_in_order(&mut v, 0, 2); // drag first to index 2
        assert_eq!(v, vec![20, 30, 10, 40]);
        move_in_order(&mut v, 3, 0); // drag last to front
        assert_eq!(v, vec![40, 20, 30, 10]);
        move_in_order(&mut v, 1, 1); // no-op
        assert_eq!(v, vec![40, 20, 30, 10]);
        move_in_order(&mut v, 9, 0); // out of range: no-op
        assert_eq!(v, vec![40, 20, 30, 10]);
    }
    use alacritty_terminal::term::TermMode;

    use super::os_title;
    const CW: f32 = 8.0; // a representative monospace cell width for layout tests
    const BH: f32 = 34.0; // tab-bar height

    #[test]
    fn strip_hit_test_matches_layout() {
        // Two tabs (tab 1 active) + their ✕ + a + button + right-hand ?/*/#. The
        // hit-test resolves to the same items the painter draws (pixel rects).
        let segs = strip_layout(
            &[("zsh", true, false), ("vim", false, false)],
            1200.0,
            BH,
            CW,
        );
        // Probe each tab body at its center and its close box, plus the controls.
        let center = |it: StripItem| {
            segs.iter().find(|s| s.item == it).map(|s| {
                let r = s.rect;
                (r.x + r.w * 0.5, r.y + r.h * 0.5)
            })
        };
        let (tx0, ty0) = center(StripItem::Tab(0)).unwrap();
        assert_eq!(strip_item_at(&segs, tx0, ty0), Some(StripItem::Tab(0)));
        let (cx0, cy0) = center(StripItem::TabClose(0)).unwrap();
        // The close box wins over its tab body (tested in reverse order).
        assert_eq!(strip_item_at(&segs, cx0, cy0), Some(StripItem::TabClose(0)));
        let (tx1, ty1) = center(StripItem::Tab(1)).unwrap();
        assert_eq!(strip_item_at(&segs, tx1, ty1), Some(StripItem::Tab(1)));
        let (nx, ny) = center(StripItem::NewTab).unwrap();
        assert_eq!(strip_item_at(&segs, nx, ny), Some(StripItem::NewTab));
        let (hx, hy) = center(StripItem::Help).unwrap();
        assert_eq!(strip_item_at(&segs, hx, hy), Some(StripItem::Help));
        let (sx, sy) = center(StripItem::Settings).unwrap();
        assert_eq!(strip_item_at(&segs, sx, sy), Some(StripItem::Settings));
        let (mx, my) = center(StripItem::Menu).unwrap();
        assert_eq!(strip_item_at(&segs, mx, my), Some(StripItem::Menu));
        // Below the bar there are no items.
        assert_eq!(strip_item_at(&segs, tx0, BH + 5.0), None);
    }

    #[test]
    fn single_tab_has_no_close() {
        // One tab is a single wide chip — no ✕ (closing it = quit).
        let segs = strip_layout(&[("shell", true, false)], 1000.0, BH, CW);
        assert!(segs.iter().any(|s| s.item == StripItem::Tab(0)));
        assert!(
            !segs
                .iter()
                .any(|s| matches!(s.item, StripItem::TabClose(_)))
        );
        let title = &segs
            .iter()
            .find(|s| s.item == StripItem::Tab(0))
            .unwrap()
            .label;
        assert_eq!(title, "shell");
    }

    #[test]
    fn strip_layout_carries_titles_by_position() {
        // Each chip carries its raw title in stable display position; the numeric
        // prefix is added at paint time, so the label is just the title here.
        let segs = strip_layout(&[("a", false, false), ("b", true, false)], 1200.0, BH, CW);
        let lbl = |it| {
            segs.iter()
                .find(|s| s.item == it)
                .map(|s| s.label.clone())
                .unwrap()
        };
        assert_eq!(lbl(StripItem::Tab(0)), "a");
        assert_eq!(lbl(StripItem::Tab(1)), "b");
    }

    #[test]
    fn os_title_is_printable_ascii_only() {
        // CJK / emoji / Nerd-Font icons / dingbats are dropped (tofu-proof).
        assert_eq!(os_title("vim  src/main.rs"), "vim src/main.rs");
        assert_eq!(os_title("✻ thinking…"), "thinking");
        assert_eq!(os_title("日本語 build"), "build");
        assert_eq!(os_title("   "), "glassy");
        assert_eq!(os_title(""), "glassy");
        // No char in the output is ever non-ASCII-graphic-or-space.
        let t = os_title("a\u{f00c}b 😀 c");
        assert!(t.chars().all(|c| c.is_ascii_graphic() || c == ' '));
    }

    #[test]
    fn wheel_normal_screen_scrolls_scrollback() {
        assert_eq!(wheel_action(TermMode::empty()), WheelAction::Scrollback);
    }

    #[test]
    fn image_size_native_when_unsized() {
        assert_eq!(image_dst_size(0, 0, 64, 32, 10.0, 20.0), (64.0, 32.0));
    }

    #[test]
    fn image_size_exact_cell_box_when_both_given() {
        // 4 cols x 3 rows at a 10x20 cell box.
        assert_eq!(image_dst_size(4, 3, 64, 32, 10.0, 20.0), (40.0, 60.0));
    }

    #[test]
    fn image_size_preserves_aspect_with_one_dim() {
        // 2:1 image, only cols=20 at cell_w=10 -> 200px wide, 100px tall (2:1).
        assert_eq!(image_dst_size(20, 0, 64, 32, 10.0, 20.0), (200.0, 100.0));
        // 2:1 image, only rows=5 at cell_h=20 -> 100px tall, 200px wide (2:1).
        assert_eq!(image_dst_size(0, 5, 64, 32, 10.0, 20.0), (200.0, 100.0));
    }

    #[test]
    fn wheel_alt_screen_emits_arrows() {
        // bat/less/vim without mouse: alt screen, no mouse reporting.
        assert_eq!(wheel_action(TermMode::ALT_SCREEN), WheelAction::Arrows);
    }

    #[test]
    fn wheel_mouse_mode_reports_to_app() {
        // vim with `mouse=a`, htop, claude: app owns the wheel.
        assert_eq!(
            wheel_action(TermMode::MOUSE_REPORT_CLICK),
            WheelAction::Report
        );
        assert_eq!(
            wheel_action(TermMode::ALT_SCREEN | TermMode::MOUSE_MOTION),
            WheelAction::Report
        );
    }

    #[test]
    fn hover_reports_only_under_any_motion() {
        // Any-motion (1003) reports bare moves (id 3) -> drives hover highlight.
        assert_eq!(motion_button(TermMode::MOUSE_MOTION, None), Some(3));
        // Button-motion (1002) stays silent without a held button.
        assert_eq!(motion_button(TermMode::MOUSE_DRAG, None), None);
        // Click-only (1000) never reports motion.
        assert_eq!(motion_button(TermMode::MOUSE_REPORT_CLICK, None), None);
        assert_eq!(motion_button(TermMode::empty(), None), None);
    }

    #[test]
    fn drag_reports_held_button_under_motion_modes() {
        assert_eq!(motion_button(TermMode::MOUSE_DRAG, Some(0)), Some(0));
        assert_eq!(motion_button(TermMode::MOUSE_MOTION, Some(2)), Some(2));
        // Click-only mode does not report drags.
        assert_eq!(motion_button(TermMode::MOUSE_REPORT_CLICK, Some(0)), None);
    }
}
