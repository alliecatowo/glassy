//! Tab-strip pixel layout + tab/title display helpers, split out of `helpers.rs`
//! to keep both files under the line cap. Pure (no `&self`) so the painter, the
//! click hit-test, and the unit tests all share one source of truth.

use super::*;

/// A tab descriptor in stable display order: (title, is_active, has_activity).
pub(crate) type TabDesc<'a> = (&'a str, bool, bool);

/// Compose the OS window-title string from the primary label (process or OSC
/// title), an optional working directory, and an optional tab count. Produces
/// `"<primary> — <cwd>"` when a cwd is given, then appends ` · N tabs` when
/// `count > 1`. An empty primary falls back to "glassy". Pure for unit testing.
pub(crate) fn compose_window_title(
    primary: &str,
    cwd: Option<&str>,
    count: Option<usize>,
) -> String {
    let primary = primary.trim();
    let mut out = if primary.is_empty() {
        "glassy".to_string()
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

/// Lay out the real GUI tab bar across the pixel-wide bar `[0, bar_w)` at height
/// `bar_h`, from tab descriptors in stable order. Produces, left→right: the glassy
/// mark slot, the tab chips (each a body rect + an embedded close-box rect in
/// multi-tab mode), a `+` new-tab button immediately AFTER the last visible tab,
/// and right-aligned `?` help, `*` settings, `#` menu icon buttons.
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

    // Right-aligned control buttons: help, settings, menu (in that visual order).
    let right_btns = [StripItem::Help, StripItem::Settings, StripItem::Menu];
    let right_w = CTRL_BTN * right_btns.len() as f32;
    // Reserve the control cluster AND the tag readout so tabs/`+` never overlap it.
    let right_start = (bar_w - right_w - TAB_GAP - tag_reserve.max(0.0)).max(0.0);

    // Decorative mark on the far left (the " ◆ " brand), then the tabs.
    let mark_w = (cell_w * 3.0).round();
    let tabs_left = mark_w + TAB_GAP;
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
    for item in right_btns {
        segs.push(StripSeg {
            item,
            label: String::new(),
            rect: gui::Rect::new(rx, ctrl_y, CTRL_BTN, CTRL_BTN),
        });
        rx += CTRL_BTN;
    }
    segs
}

/// Pixel width to reserve to the LEFT of the right control cluster for the
/// "N tabs" readout (and a little slack for the scrollback-% badge that appears
/// while scrolled). 0 for a single tab (no counter shown). Estimated from the
/// digit count so the rightmost tab never kisses the counter. Pure.
pub(crate) fn tab_tag_reserve(tab_count: usize, cell_w: f32) -> f32 {
    if tab_count <= 1 {
        return 0.0;
    }
    // "<n> tabs" = digits + " tabs" (5) + 1 cell of leading slack.
    let digits = tab_count.to_string().len();
    let chars = digits + 6;
    (chars as f32 * cell_w).round()
}

/// Pixel layout for just the three floating icon buttons (Help / Settings / Menu)
/// used when the full tab bar is hidden. Reuses the same right-aligned positions
/// as the full bar so hit-testing and painting share one coordinate source.
pub(crate) fn floating_icon_segs(bar_w: f32, bar_h: f32) -> Vec<StripSeg> {
    let right_btns = [StripItem::Help, StripItem::Settings, StripItem::Menu];
    let right_w = CTRL_BTN * right_btns.len() as f32;
    let ctrl_y = ((bar_h - CTRL_BTN) * 0.5).round().max(0.0);
    let mut segs = Vec::with_capacity(right_btns.len());
    let mut rx = bar_w - right_w - TAB_GAP;
    for item in right_btns {
        segs.push(StripSeg {
            item,
            label: String::new(),
            rect: gui::Rect::new(rx, ctrl_y, CTRL_BTN, CTRL_BTN),
        });
        rx += CTRL_BTN;
    }
    segs
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

    /// The current pixel height the tab strip occupies: the full [`tab_bar_h`] when
    /// visible, else 0.0 (the grid/content reclaims the band). Read by the render
    /// origin, grid sizing, mouse hit-tests, menus, and toasts so the strip can be
    /// hidden coherently without sprinkling visibility checks everywhere.
    pub(crate) fn effective_tab_bar_h(&self) -> f32 {
        if self.tab_bar_visible() {
            self.renderer
                .as_ref()
                .map(|r| tab_bar_h(r.cell_metrics().height))
                .unwrap_or(0.0)
        } else {
            0.0
        }
    }

    /// Recompute the grid for the current strip visibility and resize every PTY.
    /// Called after a tab open/close that may have toggled the Auto-mode strip
    /// (1↔2 tabs): the strip appearing/vanishing changes the available height, so
    /// the grid must reflow or the new/promoted tab is sized for the wrong height.
    /// A no-op-safe early return if the renderer/window are absent.
    pub(crate) fn reflow_grid(&mut self) {
        let strip_visible = self.tab_bar_visible();
        let (Some(window), Some(renderer)) = (self.window.as_ref(), self.renderer.as_ref()) else {
            return;
        };
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        let m = renderer.cell_metrics();
        let strip_h = if strip_visible {
            tab_bar_h(m.height)
        } else {
            0.0
        };
        let (cols, rows) = Self::grid_for(
            size,
            m.width,
            m.height,
            renderer.pad(),
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
            .unwrap_or_else(|| "glassy".to_string());
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
