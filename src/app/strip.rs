//! Tab-strip pixel layout + tab/title display helpers, split out of `helpers.rs`
//! to keep both files under the line cap. Pure (no `&self`) so the painter, the
//! click hit-test, and the unit tests all share one source of truth.

use super::*;

/// A tab descriptor in stable display order: (title, is_active, has_activity).
pub(crate) type TabDesc<'a> = (&'a str, bool, bool);

/// Compose the OS window-title string from the primary label (process or OSC
/// title), an optional working directory, and an optional tab count. Produces
/// `"<primary> — <cwd>"` when a cwd is given, then appends ` · N tabs` when
/// `count > 1`. An empty primary falls back to "Glassy". Pure for unit testing.
pub(crate) fn compose_window_title(
    primary: &str,
    cwd: Option<&str>,
    count: Option<usize>,
) -> String {
    let primary = primary.trim();
    let mut out = if primary.is_empty() {
        "Glassy".to_string()
    } else {
        primary.to_string()
    };
    if let Some(cwd) = cwd.map(str::trim).filter(|c| !c.is_empty()) {
        out.push_str(" — ");
        out.push_str(cwd);
    }
    if let Some(n) = count
        && n > 1
    {
        out.push_str(&format!(" · {n} tabs"));
    }
    out
}

/// Shorten a working-directory path for display in the window title: collapse a
/// leading `$HOME` to `~` and keep only the last two path components when the
/// path is deep, so the title stays readable. Pure-ish (reads `$HOME` once).
pub(crate) fn compact_cwd(path: &std::path::Path) -> String {
    let full = path.to_string_lossy();
    // Collapse $HOME → ~.
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let display: std::borrow::Cow<str> = match &home {
        Some(h) if path.starts_with(h) => {
            let rest = path.strip_prefix(h).ok();
            match rest {
                Some(r) if r.as_os_str().is_empty() => "~".into(),
                Some(r) => format!("~/{}", r.display()).into(),
                None => full.clone(),
            }
        }
        _ => full.clone(),
    };
    // Keep only the trailing two components for deep paths so the title is short.
    let parts: Vec<&str> = display.split('/').filter(|p| !p.is_empty()).collect();
    if parts.len() > 2 && !display.starts_with("~") {
        format!("…/{}", parts[parts.len() - 2..].join("/"))
    } else if parts.len() > 3 {
        // "~/a/b/c/d" → "~/…/c/d"
        format!("~/…/{}", parts[parts.len() - 2..].join("/"))
    } else {
        display.into_owned()
    }
}

/// Canonical config-file word for a [`TabBarMode`], for persistence.
pub(crate) fn tab_bar_mode_word(mode: crate::app::TabBarMode) -> &'static str {
    match mode {
        crate::app::TabBarMode::Auto => "auto",
        crate::app::TabBarMode::Always => "always",
        crate::app::TabBarMode::Never => "never",
    }
}

/// Compact glyph/text marking how many panes a tab chip contains, drawn just
/// before the title when a tab is split. `1` → no indicator (empty). `2` →
/// a split bars glyph; `>2` → an `N⊞` count. Pure for unit testing.
pub(crate) fn split_indicator(pane_count: usize) -> &'static str {
    match pane_count {
        0 | 1 => "",
        2 => "◫",
        _ => "▦",
    }
}

/// The right-aligned control buttons in the top chrome, in left→right paint
/// order. The `?`/`⚙`/`≡` icon cluster always leads; when `win_controls` is set
/// (a borderless window on non-macOS, where glassy owns the edge), the window
/// minimize / maximize / close buttons follow at the far right. macOS never sets
/// `win_controls` — it keeps its native traffic lights. Shared by the full-bar
/// layout and the floating-icon layout so both agree with the hit-test.
pub(crate) fn right_control_items(win_controls: bool) -> Vec<StripItem> {
    let mut v = vec![StripItem::Help, StripItem::Settings, StripItem::Menu];
    if win_controls {
        v.push(StripItem::WinMinimize);
        v.push(StripItem::WinMaximize);
        v.push(StripItem::WinClose);
    }
    v
}

/// Lay out the real GUI tab bar across the pixel-wide bar `[0, bar_w)` at height
/// `bar_h`, from tab descriptors in stable order. Produces, left→right: the glassy
/// mark slot, the tab chips (each a body rect + an embedded close-box rect in
/// multi-tab mode), a `+` new-tab button immediately AFTER the last visible tab,
/// and right-aligned `?` help, `*` settings, `#` menu icon buttons (plus the
/// window min/max/close controls when `win_controls` is set).
///
/// `tag_reserve` is the pixel width reserved to the LEFT of the right controls for
/// the "N tabs" / scrollback-% readout, so the rightmost tab and the `+` never
/// kiss the counter. Tab widths are clamped to `[TAB_MIN_W, TAB_MAX_W]`: shrink
/// stops at `TAB_MIN_W` (no infinite squeeze) and the strip then SCROLLS
/// horizontally so the active tab (`active_pos`) stays fully visible. Pure (pixel
/// math only) so the painter and the click hit-test agree, and unit-testable.
pub(crate) fn strip_layout_ex(
    tabs: &[TabDesc],
    bar_w: f32,
    bar_h: f32,
    cell_w: f32,
    tag_reserve: f32,
    active_pos: usize,
    left_inset: f32,
    win_controls: bool,
) -> Vec<StripSeg> {
    let mut segs = Vec::new();
    if bar_w <= 0.0 || bar_h <= 0.0 {
        return segs;
    }
    // Tab chips are inset vertically so the active chip's top accent rail and the
    // inactive chips' recess read clearly against the bar.
    let chip_y = ((bar_h - bar_h * 0.82) * 0.5).round();
    let chip_h = (bar_h - chip_y).max(1.0); // flush to the bar bottom (connector)
    let ctrl_y = ((bar_h - CTRL_BTN) * 0.5).round().max(0.0);

    // Right-aligned control buttons: help, settings, menu (in that visual order),
    // plus the window min/max/close controls when glassy owns the edge.
    let right_btns = right_control_items(win_controls);
    let right_w = CTRL_BTN * right_btns.len() as f32;
    // Reserve the control cluster AND the tag readout so tabs/`+` never overlap it.
    let right_start = (bar_w - right_w - TAB_GAP - tag_reserve.max(0.0)).max(0.0);

    // Decorative mark on the far left (the " ◆ " brand), then the tabs. `left_inset`
    // clears the macOS traffic-light buttons (0 elsewhere).
    let mark_w = (cell_w * 3.0).round();
    let tabs_left = left_inset + mark_w + TAB_GAP;
    // The `+` button sits AFTER the last tab, so reserve its width on the right of
    // the tab band.
    let plus_w = CTRL_BTN + TAB_GAP * 2.0;
    let avail = (right_start - tabs_left - TAB_GAP).max(0.0);

    if tabs.len() <= 1 {
        // Single tab: one wide chip spanning the available width minus the `+`
        // slot (no close box — closing it quits). It still reads as a connected tab.
        let w = (avail - plus_w).clamp(0.0, TAB_MAX_W * 1.6);
        let mut next_x = tabs_left;
        if w > 0.0 {
            segs.push(StripSeg {
                item: StripItem::Tab(0),
                label: tabs.first().map(|t| t.0.to_string()).unwrap_or_default(),
                rect: gui::Rect::new(tabs_left, chip_y, w, chip_h),
            });
            next_x = tabs_left + w + TAB_GAP;
        }
        push_new_tab(&mut segs, next_x, ctrl_y, right_start);
    } else {
        let n = tabs.len();
        // Equal-width chips that grow to fill the available band. Shrink stops at
        // TAB_MIN_W (no infinite squeeze); the strip scrolls when the active tab
        // would otherwise be out of view. No upper cap so two tabs fill the whole
        // bar naturally.
        let tab_band = (avail - plus_w).max(0.0);
        let per = (tab_band + TAB_GAP) / n as f32 - TAB_GAP;
        let tw = per.max(TAB_MIN_W);
        // Total width all chips want at the clamped width.
        let total = tw * n as f32 + TAB_GAP * (n as f32 - 1.0);
        // Horizontal scroll offset: 0 when everything fits, else shift so the
        // active chip is fully within [tabs_left, tabs_left + tab_band].
        let mut scroll = 0.0_f32;
        if total > tab_band {
            let active_x = active_pos as f32 * (tw + TAB_GAP);
            // Keep the active chip's left and right edges inside the visible band.
            let lo = (active_x + tw - tab_band).max(0.0); // active right edge flush
            let hi = active_x; // active left edge flush
            scroll = scroll.clamp(lo, hi.max(lo));
            // Never scroll past the end (leave the last chip flush right).
            let max_scroll = (total - tab_band).max(0.0);
            scroll = scroll.min(max_scroll);
        }
        let band_right = tabs_left + tab_band;
        let mut last_visible_right = tabs_left;
        for (i, (title, _a, _b)) in tabs.iter().enumerate() {
            let tx = tabs_left + i as f32 * (tw + TAB_GAP) - scroll;
            // Cull chips fully outside the visible band (scrolled away).
            if tx + tw <= tabs_left - 0.5 || tx >= band_right + 0.5 {
                continue;
            }
            let body = gui::Rect::new(tx, chip_y, tw, chip_h);
            segs.push(StripSeg {
                item: StripItem::Tab(i),
                label: title.to_string(),
                rect: body,
            });
            // Close box anchored to the chip's right edge, vertically centered.
            let cb = CLOSE_BOX.min(tw * 0.5);
            let close = gui::Rect::new(
                tx + tw - cb - TAB_PAD_X * 0.5,
                chip_y + (chip_h - cb) * 0.5,
                cb,
                cb,
            );
            segs.push(StripSeg {
                item: StripItem::TabClose(i),
                label: String::new(),
                rect: close,
            });
            last_visible_right = (tx + tw).min(band_right);
        }
        // `+` immediately after the last visible tab (clamped into the band).
        let plus_x = (last_visible_right + TAB_GAP * 2.0).min(band_right + TAB_GAP);
        push_new_tab(&mut segs, plus_x, ctrl_y, right_start + plus_w);
    }

    // Right controls.
    let mut rx = bar_w - right_w - TAB_GAP;
    for item in &right_btns {
        segs.push(StripSeg {
            item: *item,
            label: String::new(),
            rect: gui::Rect::new(rx, ctrl_y, CTRL_BTN, CTRL_BTN),
        });
        rx += CTRL_BTN;
    }
    segs
}

/// Pixel width to reserve to the LEFT of the right control cluster for the
/// scrollback-% badge ("⇡100%") that appears while scrolled back. The "N tabs"
/// readout was removed (the chips convey the count), so this only covers the
/// ~6-char percent badge plus a cell of slack. Pure.
pub(crate) fn tab_tag_reserve(tab_count: usize, cell_w: f32) -> f32 {
    if tab_count <= 1 {
        return 0.0;
    }
    // "⇡100%" = 5 chars + 1 cell of leading slack.
    (6.0 * cell_w).round()
}

/// Pixel layout for just the three floating icon buttons (Help / Settings / Menu)
/// used when the full tab bar is hidden. Reuses the same right-aligned positions
/// as the full bar so hit-testing and painting share one coordinate source.
pub(crate) fn floating_icon_segs(bar_w: f32, bar_h: f32, win_controls: bool) -> Vec<StripSeg> {
    let right_btns = right_control_items(win_controls);
    let right_w = CTRL_BTN * right_btns.len() as f32;
    let ctrl_y = ((bar_h - CTRL_BTN) * 0.5).round().max(0.0);
    let mut segs = Vec::with_capacity(right_btns.len());
    let mut rx = bar_w - right_w - TAB_GAP;
    for item in &right_btns {
        segs.push(StripSeg {
            item: *item,
            label: String::new(),
            rect: gui::Rect::new(rx, ctrl_y, CTRL_BTN, CTRL_BTN),
        });
        rx += CTRL_BTN;
    }
    segs
}

/// The width (px, measured in from the window's right edge) reserved by the
/// floating Help/Settings/Menu icon cluster when the tab strip is hidden —
/// including its trailing [`TAB_GAP`] and the pill backdrop's left pad (see
/// [`floating_icon_segs`] and `App::paint_floating_icons`). Content painted
/// full-width at `y == 0` (the single-pane header, which shares that top band
/// when the strip is hidden) must inset its right edge by this much so it does
/// not wash over the icons. Mirrors the icon cluster's own geometry so the two
/// share one coordinate source.
pub(crate) fn floating_icons_reserved_w(win_controls: bool) -> f32 {
    const PILL_PAD: f32 = 4.0; // matches `paint_floating_icons`' pill backdrop pad
    let n = right_control_items(win_controls).len() as f32;
    CTRL_BTN * n + TAB_GAP + PILL_PAD
}

/// Hit-test the window-edge resize border for a borderless window. Returns the
/// [`winit::window::ResizeDirection`] when the pointer `(x, y)` lies within
/// `border` px of an edge/corner of the `w`×`h` surface, else `None`. Corners win
/// over edges (a corner is where two edge strips overlap). Points outside the
/// surface rect return `None`. Pure for unit testing.
#[cfg(not(target_os = "macos"))]
pub(crate) fn resize_edge_at(
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    border: f32,
) -> Option<winit::window::ResizeDirection> {
    use winit::window::ResizeDirection as Rd;
    if w <= 0.0 || h <= 0.0 || x < 0.0 || y < 0.0 || x > w || y > h {
        return None;
    }
    let left = x <= border;
    let right = x >= w - border;
    let top = y <= border;
    let bottom = y >= h - border;
    match (top, bottom, left, right) {
        (true, _, true, _) => Some(Rd::NorthWest),
        (true, _, _, true) => Some(Rd::NorthEast),
        (_, true, true, _) => Some(Rd::SouthWest),
        (_, true, _, true) => Some(Rd::SouthEast),
        (true, ..) => Some(Rd::North),
        (_, true, ..) => Some(Rd::South),
        (_, _, true, _) => Some(Rd::West),
        (_, _, _, true) => Some(Rd::East),
        _ => None,
    }
}

/// Push the `+` new-tab button at `x` if it fits left of `limit`.
fn push_new_tab(segs: &mut Vec<StripSeg>, x: f32, ctrl_y: f32, limit: f32) {
    let plus = gui::Rect::new(x, ctrl_y, CTRL_BTN, CTRL_BTN);
    if plus.x + plus.w <= limit + 0.5 {
        segs.push(StripSeg {
            item: StripItem::NewTab,
            label: String::new(),
            rect: plus,
        });
    }
}

impl App {
    /// Whether the tab strip should be drawn right now. `Auto` (the default) hides
    /// it while only one tab is open and reveals it once a second tab exists;
    /// `Always`/`Never` are unconditional. Centralizes the policy so the layout
    /// (grid sizing + pixel origin), the painter, and every pixel hit-test agree.
    pub(crate) fn tab_bar_visible(&self) -> bool {
        match self.config.show_tab_bar {
            crate::app::TabBarMode::Always => true,
            crate::app::TabBarMode::Never => false,
            crate::app::TabBarMode::Auto => self.tab_count() > 1,
        }
    }

    /// macOS reserves a top band even when the tab bar is hidden: with the OS
    /// title bar removed (fullsize content view), the traffic-light buttons float
    /// over the top-left, so content must start below them. ≈28 logical pt covers
    /// the button zone. 0 on other platforms (normal title bar above the content).
    pub(crate) fn chrome_top_inset(&self) -> f32 {
        #[cfg(target_os = "macos")]
        {
            let scale = self
                .window
                .as_ref()
                .map(|w| w.scale_factor() as f32)
                .unwrap_or(1.0);
            (28.0 * scale).round()
        }
        #[cfg(not(target_os = "macos"))]
        {
            0.0
        }
    }

    /// Left inset for the top chrome band's left-aligned content (brand mark, tab
    /// chips) on macOS, clearing the floating traffic-light buttons (≈78 logical pt
    /// wide). 0 elsewhere. Right-aligned content (the icon cluster) needs no inset.
    pub(crate) fn chrome_left_inset(&self) -> f32 {
        #[cfg(target_os = "macos")]
        {
            let scale = self
                .window
                .as_ref()
                .map(|w| w.scale_factor() as f32)
                .unwrap_or(1.0);
            (78.0 * scale).round()
        }
        #[cfg(not(target_os = "macos"))]
        {
            0.0
        }
    }

    /// The current pixel height the top chrome band occupies. The tab strip's full
    /// [`tab_bar_h`] when visible, else 0 — except on macOS, where the traffic-light
    /// inset is always reserved (see [`chrome_top_inset`]) so the integrated
    /// titlebar look never lets content slide under the window buttons. Read by the
    /// render origin, grid sizing, mouse hit-tests, menus, and toasts.
    pub(crate) fn effective_tab_bar_h(&self) -> f32 {
        let bar = if self.tab_bar_visible() {
            self.renderer
                .as_ref()
                .map(|r| tab_bar_h(r.cell_metrics().height))
                .unwrap_or(0.0)
        } else {
            0.0
        };
        bar.max(self.chrome_top_inset())
    }

    /// Whether glassy paints its own window minimize / maximize / close controls
    /// in the top-right chrome. True only when glassy owns the window edge: a
    /// borderless window (`decorations = false`) on a platform without native
    /// traffic lights. macOS always returns false (it keeps its native buttons via
    /// the fullsize-content-view path); a quake dropdown returns false too (it has
    /// no titlebar affordances). Read by the strip layout, the painter, and the
    /// floating-icon reserve so all three agree.
    pub(crate) fn show_window_controls(&self) -> bool {
        #[cfg(target_os = "macos")]
        {
            false
        }
        #[cfg(not(target_os = "macos"))]
        {
            !self.config.decorations && self.quake.is_none()
        }
    }

    /// Recompute the grid for the current strip visibility and resize every PTY.
    /// Called after a tab open/close that may have toggled the Auto-mode strip
    /// (1↔2 tabs): the strip appearing/vanishing changes the available height, so
    /// the grid must reflow or the new/promoted tab is sized for the wrong height.
    /// A no-op-safe early return if the renderer/window are absent.
    pub(crate) fn reflow_grid(&mut self) {
        // Top band height incl. the macOS traffic-light inset; matches the render
        // origin so grid sizing and painting agree.
        let strip_h = self.effective_tab_bar_h();
        let (Some(window), Some(renderer)) = (self.window.as_ref(), self.renderer.as_ref()) else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let m = renderer.cell_metrics();
        let (cols, rows) = Self::grid_for(
            size,
            m.width,
            m.height,
            renderer.pad_x(),
            renderer.pad_y(),
            self.config.status_bar,
            strip_h,
        );
        let (cw, ch) = (m.width.round() as u16, m.height.round() as u16);
        if self.panes.is_some() {
            self.resize_panes();
        } else {
            self.cols = cols;
            self.rows = rows;
            if let Some(pty) = self.pty.as_ref() {
                pty.resize(cols, rows, cw, ch);
            }
        }
        // Keep NON-split background tabs in sync so switching to one is correct.
        for s in &self.background {
            if s.panes.is_none() {
                s.pty.resize(cols, rows, cw, ch);
            }
        }
    }

    /// Best process name for the active focused pane: the running foreground
    /// command (`vim`/`cargo`/`claude`) or the shell name (`zsh`) at an idle
    /// prompt. `None` only when no PTY exists. Used for the process-aware tab
    /// label and OS window title.
    pub(crate) fn active_process_name(&self) -> Option<String> {
        let pty = self.pty.as_ref()?;
        pty.pane_info
            .process_name(pty.shell_comm.as_deref())
            .map(str::to_string)
    }

    /// Process-aware fallback title for a session whose OSC/custom title is empty:
    /// the foreground process or shell name. Pure helper so the tab-descriptor
    /// builders stay terse.
    pub(crate) fn proc_label_for(pty: &Pty) -> String {
        pty.pane_info
            .process_name(pty.shell_comm.as_deref())
            .unwrap_or("shell")
            .to_string()
    }

    /// Reflect the active tab in the native (CSD) window title. Composes
    /// `<process> — <cwd>` (cwd optional via `title_show_cwd`) with an optional
    /// ` · N tabs` suffix (`title_show_count`). A custom (renamed) title or an OSC
    /// title takes precedence over the derived process name; an explicit OSC title
    /// is shown verbatim (still cwd/count-decorated).
    pub(crate) fn update_window_title(&self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        window.set_title(&os_title(&self.composed_window_title()));
    }

    /// Build the rich (pre-ASCII-fold) window title string. Split out from
    /// [`update_window_title`] so it is pure and unit-testable via
    /// [`compose_window_title`].
    pub(crate) fn composed_window_title(&self) -> String {
        // Title precedence: a renamed/custom title, else the OSC title, else the
        // derived process name (vim/cargo/zsh…).
        let primary = self
            .active_custom_title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| {
                let t = self.active_title.trim();
                (!t.is_empty()).then(|| t.to_string())
            })
            .or_else(|| self.active_process_name())
            .unwrap_or_else(|| "Glassy".to_string());
        let cwd = if self.config.title_show_cwd {
            self.pty
                .as_ref()
                .and_then(|p| p.pane_info.cwd.clone())
                .or_else(|| self.active_cwd.clone())
                .map(|p| compact_cwd(&p))
        } else {
            None
        };
        let count = self.config.title_show_count.then(|| self.tab_count());
        compose_window_title(&primary, cwd.as_deref(), count)
    }
}

#[cfg(test)]
mod floating_icon_tests {
    use super::{CTRL_BTN, TAB_GAP, floating_icon_segs, floating_icons_reserved_w};

    #[test]
    fn single_pane_header_inset_clears_the_floating_icon_cluster() {
        // REGRESSION: with the tab strip hidden, the single-pane header is painted
        // full-width at y=0, the same band as the floating Help/Settings/Menu icon
        // cluster (top-right). Insetting the header's right edge by
        // `floating_icons_reserved_w()` must keep it clear of the icons.
        for &sw in &[800.0_f32, 1024.0, 1920.0, 640.0] {
            let bar_h = 32.0;
            let segs = floating_icon_segs(sw, bar_h, false);
            assert_eq!(segs.len(), 3, "Help/Settings/Menu");

            // The header's inset right edge (header starts at x=0, full width sw).
            let header_right = sw - floating_icons_reserved_w(false);

            // The leftmost pixel actually touched by the icon cluster is its pill
            // backdrop, which starts 4px left of the first icon seg (see
            // `paint_floating_icons`). The header must stop at or before it.
            let leftmost_icon_x = segs.iter().map(|s| s.rect.x).fold(f32::INFINITY, f32::min);
            let pill_left = leftmost_icon_x - 4.0;
            assert!(
                header_right <= pill_left + f32::EPSILON,
                "header right {header_right} overlaps icon pill left {pill_left} (sw={sw})"
            );
        }
    }

    #[test]
    fn reserved_width_matches_the_cluster_geometry() {
        // The reserved width equals the three icon buttons + trailing gap + pill
        // pad, so it stays in lock-step with `floating_icon_segs`' own layout.
        assert_eq!(
            floating_icons_reserved_w(false),
            CTRL_BTN * 3.0 + TAB_GAP + 4.0
        );
        // With glassy's own window controls the cluster gains three more buttons.
        assert_eq!(
            floating_icons_reserved_w(true),
            CTRL_BTN * 6.0 + TAB_GAP + 4.0
        );
    }
}

#[cfg(all(test, not(target_os = "macos")))]
mod resize_edge_tests {
    use super::resize_edge_at;
    use winit::window::ResizeDirection as Rd;

    const W: f32 = 800.0;
    const H: f32 = 600.0;
    const B: f32 = 6.0;

    #[test]
    fn corners_take_priority_over_edges() {
        assert_eq!(resize_edge_at(1.0, 1.0, W, H, B), Some(Rd::NorthWest));
        assert_eq!(resize_edge_at(W - 1.0, 1.0, W, H, B), Some(Rd::NorthEast));
        assert_eq!(resize_edge_at(1.0, H - 1.0, W, H, B), Some(Rd::SouthWest));
        assert_eq!(
            resize_edge_at(W - 1.0, H - 1.0, W, H, B),
            Some(Rd::SouthEast)
        );
    }

    #[test]
    fn edges_resolve_to_cardinal_directions() {
        assert_eq!(resize_edge_at(W * 0.5, 1.0, W, H, B), Some(Rd::North));
        assert_eq!(resize_edge_at(W * 0.5, H - 1.0, W, H, B), Some(Rd::South));
        assert_eq!(resize_edge_at(1.0, H * 0.5, W, H, B), Some(Rd::West));
        assert_eq!(resize_edge_at(W - 1.0, H * 0.5, W, H, B), Some(Rd::East));
    }

    #[test]
    fn interior_and_out_of_bounds_return_none() {
        assert_eq!(resize_edge_at(W * 0.5, H * 0.5, W, H, B), None);
        assert_eq!(resize_edge_at(-1.0, -1.0, W, H, B), None);
        assert_eq!(resize_edge_at(W + 5.0, H * 0.5, W, H, B), None);
        assert_eq!(resize_edge_at(10.0, 10.0, 0.0, 0.0, B), None);
    }
}
