//! Chrome painting: status bar, settings overlay, tab-rename editor,
//! confirm-close modal. Tab-bar and chip painting are in tab_paint.rs.

use crate::app::StatusBarSegment;

use super::*;

/// Format a [`std::time::SystemTime`] using a minimal `strftime`-style pattern.
///
/// Supported tokens (only the most common clock tokens):
/// - `%H` — 24-hour hour (00–23)
/// - `%I` — 12-hour hour (01–12)
/// - `%M` — minute (00–59)
/// - `%S` — second (00–59)
/// - `%p` — AM/PM
/// - `%%` — literal `%`
///
/// Anything else is passed through verbatim. Pure function for unit testing.
fn format_time(t: std::time::SystemTime, fmt: &str) -> String {
    // Convert to seconds-since-epoch and decompose to H:M:S.
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days and within-day offset (UTC; no local-time conversion available without
    // chrono/time, so we use UTC which is common for server/embedded terminals).
    let day_secs = secs % 86_400;
    let h = (day_secs / 3600) as u8;
    let min = ((day_secs % 3600) / 60) as u8;
    let sec = (day_secs % 60) as u8;
    let h12 = match h % 12 {
        0 => 12,
        n => n,
    };
    let ampm = if h < 12 { "AM" } else { "PM" };

    let mut out = String::with_capacity(fmt.len() + 4);
    let mut chars = fmt.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            match chars.next() {
                Some('H') => {
                    out.push_str(&format!("{h:02}"));
                }
                Some('I') => {
                    out.push_str(&format!("{h12:02}"));
                }
                Some('M') => {
                    out.push_str(&format!("{min:02}"));
                }
                Some('S') => {
                    out.push_str(&format!("{sec:02}"));
                }
                Some('p') => {
                    out.push_str(ampm);
                }
                Some('%') => out.push('%'),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod chrome_tests {
    use super::*;

    #[test]
    fn format_time_hhmm() {
        // noon UTC = 43200 s since epoch
        let noon = std::time::UNIX_EPOCH + std::time::Duration::from_secs(43200);
        assert_eq!(format_time(noon, "%H:%M"), "12:00");
    }

    #[test]
    fn format_time_12h() {
        let noon = std::time::UNIX_EPOCH + std::time::Duration::from_secs(43200);
        assert_eq!(format_time(noon, "%I:%M %p"), "12:00 PM");
    }

    #[test]
    fn format_time_midnight() {
        let midnight = std::time::UNIX_EPOCH;
        assert_eq!(format_time(midnight, "%H:%M"), "00:00");
        assert_eq!(format_time(midnight, "%I:%M %p"), "12:00 AM");
    }
}

/// Result returned from [`App::paint_confirm_close`] each frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfirmCloseResult {
    /// The modal is still open (user has not interacted yet).
    Pending,
    /// The user clicked "Close" — proceed with the close.
    Confirm,
    /// The user clicked "Cancel" — abort the close.
    Cancel,
}

/// A `CONFIG_TOGGLES` entry's accessor: returns the `&mut bool` a toggle flips.
type ConfigToggleField = fn(&mut Config) -> &mut bool;

/// Plain boolean settings-form toggles: `(widget id, config field accessor)`.
/// Consulted by [`App::apply_settings_events`] for every id in
/// `SettingsEvents::toggled` that isn't one of the toggles matched explicitly
/// there because it drives an extra live side effect beyond the flip itself
/// (status bar / pane headers reflow the grid; ligatures / cursor trail push to
/// the renderer; restore-session marks the session dirty). Adding a new plain
/// boolean setting is then one `RowKind::Toggle` push (`settings_panel.rs`) +
/// one row here — no new `SettingsEvents` field, no new `if` block.
const CONFIG_TOGGLES: &[(&str, ConfigToggleField)] = &[
    ("settings/follow_system", |c| &mut c.follow_system),
    ("settings/cursor_blink", |c| &mut c.cursor_blink),
    ("settings/copy_on_select", |c| &mut c.copy_on_select),
    ("settings/minimap", |c| &mut c.minimap),
    ("settings/command_badges", |c| &mut c.command_badges),
    ("settings/title_show_cwd", |c| &mut c.title_show_cwd),
    ("settings/title_show_count", |c| &mut c.title_show_count),
    ("settings/dim_unfocused", |c| &mut c.dim_unfocused),
    ("settings/copy_html", |c| &mut c.copy_html),
    ("settings/notify_command_finish", |c| {
        &mut c.notify_command_finish
    }),
    // Quake mode is armed once in `App::init_quake`, called only from
    // `resumed()` at startup — flipping `config.quake` after the window
    // already exists has no live effect (no quake window is created/torn
    // down). It is still a plain flip: no OTHER live side effect exists to
    // replicate (the Settings UI labels this "(restart required)" — see
    // `SettingsSection::Quake` in `settings_panel.rs`), and the value is
    // still persisted on Save for the next launch.
    ("settings/quake", |c| &mut c.quake),
];

// --- Status bar: data-driven segment table --------------------------------
//
// Each built-in segment (except `Progress` and `Custom`, both special-cased in
// `paint_status_bar` itself — see the comments there) has exactly one entry in
// `SEGMENT_SPECS` below: which side it flows from, its right-side fixed-width
// slot (left segments have none), and a pure `&StatusBarInputs -> Option<(text,
// color)>` render fn. This replaces the old per-variant `is_left_seg`/
// `is_right_seg` matches plus the two paint loops' own per-arm match blocks —
// adding a built-in segment is now one array entry + one render fn, not five
// call-site edits.

/// One frame's worth of every value `paint_status_bar`/`SEGMENT_SPECS`'
/// render fns might need. Built once per frame by `App::status_bar_inputs`
/// (under `&self`, before the disjoint `&mut self.renderer` borrow the render
/// paths take) and shared by both call sites (`render.rs`'s single-pane path,
/// `multipane.rs`'s split path) — previously each independently unpacked the
/// same 11+ locals and threaded them through `paint_status_bar`'s 14
/// positional parameters.
pub(crate) struct StatusBarInputs {
    pub surface_h: u32,
    pub term_mode: TermMode,
    pub display_offset: i32,
    pub history_size: usize,
    pub sel_len: usize,
    pub win_focused: bool,
    pub cwd: Option<std::path::PathBuf>,
    pub git_branch: Option<String>,
    pub progress: Option<crate::image::ProgressState>,
    pub broadcast: bool,
    pub fg_process: Option<String>,
    pub time_format: String,
    pub exit_status: Option<i32>,
    pub segments: Option<Vec<StatusBarSegment>>,
    /// w15: total tab count — `StatusBarSegment::TabCount`.
    pub tab_count: usize,
    /// w15: whether the focused pane is zoomed — `StatusBarSegment::Zoom`.
    pub zoomed: bool,
    /// w15: the active `[profile.NAME]` section, if any —
    /// `StatusBarSegment::Profile`.
    pub active_profile: Option<String>,
    /// w15: whether the focused pane's most recent command block has started
    /// but not finished — `StatusBarSegment::Busy`.
    pub busy: bool,
    /// w15: this machine's hostname, cached at startup —
    /// `StatusBarSegment::Hostname`.
    pub hostname: Option<String>,
    /// w15: custom segments pushed via `glassy @ set-segment` (Phase 1 plugin
    /// surface, see `docs/plugins.md`), in insertion order.
    pub custom_segments: Vec<CustomSegment>,
}

/// Which side of the bar a segment flows from: left segments are placed
/// left-to-right starting at the left margin; right segments are placed
/// right-to-left starting at the right margin (see the "fixed slot" doc on
/// `SegmentSpec::slot_chars` for why the two sides are treated differently).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Right,
}

/// A segment's resolved text color, expressed as a role rather than a raw
/// `[f32; 4]` so render fns need no `Renderer`/focus-dim-state access — just
/// plain data. Resolved against the already focus-dimmed palette computed
/// once at the top of `paint_status_bar`.
#[derive(Clone, Copy)]
enum SegColor {
    Fg,
    FgDim,
    Accent,
    Danger,
}

impl SegColor {
    fn resolve(
        self,
        fg: [f32; 4],
        fg_dim: [f32; 4],
        accent: [f32; 4],
        danger: [f32; 4],
    ) -> [f32; 4] {
        match self {
            SegColor::Fg => fg,
            SegColor::FgDim => fg_dim,
            SegColor::Accent => accent,
            SegColor::Danger => danger,
        }
    }
}

/// A segment's content: `None` means "render nothing this frame" (e.g. no git
/// branch known, no active selection) — the segment still occupies its
/// right-side fixed slot (if any) but draws no glyphs, exactly like the
/// pre-table per-arm code did.
type SegRenderFn = fn(&StatusBarInputs) -> Option<(String, SegColor)>;

/// One `SEGMENT_SPECS` entry. `slot_chars` is the right-side fixed-width slot
/// reserved in cell units regardless of the actual rendered text width (so a
/// segment's content changing length doesn't jitter its neighbors) — `0.0`
/// for left-side segments, which have no slot and simply advance by their
/// measured width (see the module doc on left/right asymmetry).
struct SegmentSpec {
    seg: StatusBarSegment,
    side: Side,
    slot_chars: f32,
    render: SegRenderFn,
}

fn segment_spec(seg: StatusBarSegment) -> Option<&'static SegmentSpec> {
    SEGMENT_SPECS.iter().find(|s| s.seg == seg)
}

const SEGMENT_SPECS: &[SegmentSpec] = &[
    // Left-side (left-to-right, no fixed slot).
    SegmentSpec {
        seg: StatusBarSegment::Cwd,
        side: Side::Left,
        slot_chars: 0.0,
        render: render_cwd,
    },
    SegmentSpec {
        seg: StatusBarSegment::GitBranch,
        side: Side::Left,
        slot_chars: 0.0,
        render: render_git_branch,
    },
    SegmentSpec {
        seg: StatusBarSegment::Process,
        side: Side::Left,
        slot_chars: 0.0,
        render: render_process,
    },
    SegmentSpec {
        seg: StatusBarSegment::Time,
        side: Side::Left,
        slot_chars: 0.0,
        render: render_time,
    },
    SegmentSpec {
        seg: StatusBarSegment::ExitStatus,
        side: Side::Left,
        slot_chars: 0.0,
        render: render_exit_status,
    },
    SegmentSpec {
        seg: StatusBarSegment::KeyHints,
        side: Side::Left,
        slot_chars: 0.0,
        render: render_key_hints,
    },
    // Right-side (right-to-left, fixed-width slot — the "6-char slot" style
    // comments the pre-table code had on each arm).
    SegmentSpec {
        seg: StatusBarSegment::Mode,
        side: Side::Right,
        slot_chars: 8.0,
        render: render_mode,
    },
    SegmentSpec {
        seg: StatusBarSegment::Broadcast,
        side: Side::Right,
        slot_chars: 6.0,
        render: render_broadcast,
    },
    SegmentSpec {
        seg: StatusBarSegment::Selection,
        side: Side::Right,
        slot_chars: 9.0,
        render: render_selection,
    },
    SegmentSpec {
        seg: StatusBarSegment::Scroll,
        side: Side::Right,
        slot_chars: 7.0,
        render: render_scroll,
    },
    SegmentSpec {
        seg: StatusBarSegment::Encoding,
        side: Side::Right,
        slot_chars: 6.0,
        render: render_encoding,
    },
    // w15 additions: 5 new built-ins, each a cheap read of data glassy already
    // computes (tab count, pane-zoom state, active profile, OSC 133
    // running-command state, and the cached startup hostname).
    SegmentSpec {
        seg: StatusBarSegment::TabCount,
        side: Side::Left,
        slot_chars: 0.0,
        render: render_tab_count,
    },
    SegmentSpec {
        seg: StatusBarSegment::Profile,
        side: Side::Left,
        slot_chars: 0.0,
        render: render_profile,
    },
    SegmentSpec {
        seg: StatusBarSegment::Busy,
        side: Side::Left,
        slot_chars: 0.0,
        render: render_busy,
    },
    SegmentSpec {
        seg: StatusBarSegment::Hostname,
        side: Side::Left,
        slot_chars: 0.0,
        render: render_hostname,
    },
    SegmentSpec {
        seg: StatusBarSegment::Zoom,
        side: Side::Right,
        slot_chars: 6.0,
        render: render_zoom,
    },
    // `Progress` (bottom-edge bar) and `Custom` (Phase-1 plugin segments) are
    // handled outside this table — see `paint_status_bar` below.
];

fn render_cwd(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    let path = ctx.cwd.as_deref()?;
    let s = if path.as_os_str().is_empty() {
        "~".to_string()
    } else {
        let components: Vec<_> = path.components().collect();
        let n = components.len();
        if n >= 2 {
            format!(
                "{}/{}",
                components[n - 2].as_os_str().to_string_lossy(),
                components[n - 1].as_os_str().to_string_lossy()
            )
        } else if n == 1 {
            components[0].as_os_str().to_string_lossy().to_string()
        } else {
            "~".to_string()
        }
    };
    Some((s, SegColor::FgDim))
}

fn render_git_branch(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    let branch = ctx.git_branch.as_deref()?;
    Some((format!("\u{E0A0} {branch}"), SegColor::Accent))
}

fn render_process(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    ctx.fg_process
        .as_deref()
        .map(|p| (p.to_string(), SegColor::Fg))
}

fn render_time(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    let now = std::time::SystemTime::now();
    Some((format_time(now, &ctx.time_format), SegColor::FgDim))
}

fn render_exit_status(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    let code = ctx.exit_status?;
    if code == 0 {
        Some(("\u{2713}".to_string(), SegColor::FgDim)) // ✓
    } else {
        Some((format!("\u{2717}{code}"), SegColor::Danger)) // ✗N
    }
}

fn render_key_hints(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    let hints = if ctx.term_mode.contains(TermMode::ALT_SCREEN) {
        "F1:help  q:quit"
    } else {
        "F1:help  Ctrl+P:palette"
    };
    Some((hints.to_string(), SegColor::FgDim))
}

fn render_mode(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    let alt = ctx.term_mode.contains(TermMode::ALT_SCREEN);
    let mouse = ctx.term_mode.intersects(TermMode::MOUSE_MODE);
    if !alt && !mouse {
        return None;
    }
    Some((if alt { "ALT" } else { "MOUSE" }.to_string(), SegColor::Fg))
}

fn render_broadcast(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    ctx.broadcast
        .then(|| ("BCAST".to_string(), SegColor::Danger))
}

fn render_selection(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    if ctx.sel_len == 0 {
        return None;
    }
    Some((format!("{} sel", ctx.sel_len), SegColor::FgDim))
}

fn render_scroll(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    if ctx.display_offset <= 0 {
        return None;
    }
    let pct = if ctx.history_size > 0 {
        ((ctx.display_offset as f32 / ctx.history_size as f32) * 100.0).round() as u32
    } else {
        100
    }
    .min(100);
    Some((format!("⇡{pct:>3}%"), SegColor::Accent))
}

fn render_encoding(_ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    Some(("UTF-8".to_string(), SegColor::FgDim))
}

fn render_tab_count(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    let n = ctx.tab_count;
    Some((
        format!("{n} tab{}", if n == 1 { "" } else { "s" }),
        SegColor::FgDim,
    ))
}

fn render_profile(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    ctx.active_profile
        .as_deref()
        .map(|p| (format!("[{p}]"), SegColor::FgDim))
}

fn render_busy(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    ctx.busy
        .then(|| ("\u{25CF} running".to_string(), SegColor::Accent)) // ● running
}

fn render_hostname(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    ctx.hostname
        .as_deref()
        .map(|h| (h.to_string(), SegColor::FgDim))
}

fn render_zoom(ctx: &StatusBarInputs) -> Option<(String, SegColor)> {
    ctx.zoomed.then(|| ("ZOOM".to_string(), SegColor::Accent))
}

impl App {
    // paint_tab_bar, paint_tab_chip, paint_tab_label live in tab_paint.rs.

    /// Snapshot every value `paint_status_bar` might need for this frame. Must
    /// be called under `&self`, before the disjoint `&mut self.renderer` borrow
    /// the render paths take — see the call sites in `render.rs`/`multipane.rs`,
    /// both of which now call this instead of independently unpacking the same
    /// locals.
    pub(crate) fn status_bar_inputs(&self) -> StatusBarInputs {
        let (term_mode, display_offset, history_size, sel_len) = match self.pty.as_ref() {
            Some(pty) => {
                let t = pty.term.lock();
                let mode = *t.mode();
                let disp = t.grid().display_offset() as i32;
                let hist = t.grid().history_size();
                let sel = t
                    .selection_to_string()
                    .map(|s| s.chars().count())
                    .unwrap_or(0);
                (mode, disp, hist, sel)
            }
            None => (TermMode::empty(), 0, 0, 0),
        };
        // cwd + git branch read from the active PTY's cached PaneInfo (refreshed
        // at most every 2 s — PROC_REFRESH_INTERVAL — so this does no filesystem
        // walk per frame).
        let cwd = self
            .pty
            .as_ref()
            .and_then(|p| p.pane_info.cwd.clone())
            .or_else(|| self.active_cwd.clone()); // fallback to OSC 7 path
        let git_branch = self
            .pty
            .as_ref()
            .and_then(|p| p.pane_info.git_branch.clone());
        let fg_process = self
            .pty
            .as_ref()
            .and_then(|p| p.pane_info.process_name(None).map(str::to_owned));
        // Last command exit status from the OSC 133 block store (most recent block).
        let exit_status = self.pty.as_ref().and_then(|p| {
            p.prompts
                .lock()
                .ok()
                .and_then(|g| g.blocks.iter().rev().find_map(|b| b.exit_code))
        });
        // Busy: the focused pane's most recent command block has started (`C`
        // mark, `output_row` set) but not finished (`D` mark, `end_row` unset) —
        // reads the same `PromptTracker` lock `exit_status` above already takes.
        let busy = self.pty.as_ref().is_some_and(|p| {
            p.prompts.lock().ok().is_some_and(|g| {
                g.blocks
                    .back()
                    .is_some_and(|b| b.output_row.is_some() && b.end_row.is_none())
            })
        });
        let zoomed = self.panes.as_ref().is_some_and(|g| g.zoom.is_on());
        StatusBarInputs {
            surface_h: self
                .renderer
                .as_ref()
                .map(|r| r.surface_size().1)
                .unwrap_or(0),
            term_mode,
            display_offset,
            history_size,
            sel_len,
            win_focused: self.focused,
            cwd,
            git_branch,
            progress: self.active_progress,
            broadcast: self.broadcast_input,
            fg_process,
            time_format: self.config.status_bar_time_format.clone(),
            exit_status,
            segments: self.config.status_bar_segments.clone(),
            tab_count: self.tab_count(),
            zoomed,
            active_profile: self.active_profile.clone(),
            busy,
            hostname: self.hostname.clone(),
            custom_segments: self.custom_segments.clone(),
        }
    }

    /// Paint the status bar (§3.4): a `STATUS_BAR_H`-px E1 band at the bottom of
    /// the window, with configurable segments in configurable order.
    ///
    /// The default segment set and order is:
    ///   left: `[cwd] [git_branch]`
    ///   right: `[mode] [broadcast] [selection] [scroll%] [encoding]`
    ///   bottom: `[progress]`
    ///
    /// When `inputs.segments` is `Some`, only the listed segments are shown in
    /// the listed order, each dispatched through `SEGMENT_SPECS` (module-level
    /// above) rather than a per-variant match: left segments flow left-to-right
    /// with no fixed slot; right segments flow right-to-left, each reserving a
    /// fixed-width slot so its neighbors don't jitter as its content changes.
    /// `Progress` is always a decorative 1-px bar at the bottom edge, handled
    /// outside the table. `Custom` renders every active
    /// `glassy @ set-segment` segment at that position in the left-side order
    /// (or appended at the end of the left side if the token is absent but a
    /// custom segment is set) — also outside the table, since it fans out to a
    /// variable number of entries rather than one.
    ///
    /// Associated fn (no `&self`) so it composes with the caller's `&mut Renderer`
    /// borrow; all data arrives via `inputs` (built by `status_bar_inputs`).
    pub(crate) fn paint_status_bar(renderer: &mut Renderer, inputs: &StatusBarInputs) {
        let m = renderer.cell_metrics();
        let (sw, _sh) = renderer.surface_size();
        let bar_w = sw as f32;
        let bar_h = STATUS_BAR_H;
        let bar_y = inputs.surface_h as f32 - bar_h;

        if bar_w <= 0.0 || bar_h <= 0.0 {
            return;
        }

        // Dim while window is unfocused, matching tab-bar convention.
        let fdim = if inputs.win_focused { 1.0 } else { 0.7 };
        let mul = |c: [f32; 4]| [c[0] * fdim, c[1] * fdim, c[2] * fdim, c[3]];

        let bar_bg = mul(gui::glass_body());
        let accent = mul(color::accent());
        let fg_dim = mul(gui::fg_dim());
        let fg = mul(gui::fg());
        let danger = mul(color::danger());

        // 1) Bar backdrop + top hairline (mirrors the tab bar's bottom seam).
        renderer.push_overlay_px(0.0, bar_y, bar_w, bar_h, bar_bg);
        renderer.push_overlay_px(0.0, bar_y, bar_w, 1.0, mul(gui::hairline()));

        // Glyph vertical centre within the bar.
        let ty = (bar_y + (bar_h - m.height) * 0.5).round();

        // Resolve the segment list: explicit config or built-in defaults.
        const DEFAULT_SEGMENTS: &[StatusBarSegment] = &[
            StatusBarSegment::Cwd,
            StatusBarSegment::GitBranch,
            StatusBarSegment::Mode,
            StatusBarSegment::Broadcast,
            StatusBarSegment::Selection,
            StatusBarSegment::Scroll,
            StatusBarSegment::Encoding,
            StatusBarSegment::Progress,
        ];
        let seg_list: &[StatusBarSegment] = inputs.segments.as_deref().unwrap_or(DEFAULT_SEGMENTS);

        let right_margin = m.width;
        let mut rx = bar_w - right_margin;

        // 2) Right-aligned segments — each has a fixed-width slot to avoid jitter.
        for seg in seg_list.iter().rev() {
            let Some(spec) = segment_spec(*seg) else {
                continue;
            };
            if spec.side != Side::Right {
                continue;
            }
            if let Some((text, color)) = (spec.render)(inputs) {
                let col = color.resolve(fg, fg_dim, accent, danger);
                let w = renderer.text_width_px(&text);
                renderer.push_overlay_glyph_px_str((rx - w).round(), ty, &text, col);
            }
            rx -= (spec.slot_chars * m.width).round();
        }
        let _ = rx;

        // 3) Left-side segments (left-to-right).
        {
            let left_margin = m.width;
            let mut lx = left_margin;
            let mut custom_at_marker = false;

            for seg in seg_list.iter() {
                if *seg == StatusBarSegment::Custom {
                    lx = Self::render_custom_segments(
                        renderer,
                        &inputs.custom_segments,
                        lx,
                        ty,
                        m.width,
                        fg,
                    );
                    custom_at_marker = true;
                    continue;
                }
                let Some(spec) = segment_spec(*seg) else {
                    continue;
                };
                if spec.side != Side::Left {
                    continue;
                }
                if let Some((text, color)) = (spec.render)(inputs) {
                    let col = color.resolve(fg, fg_dim, accent, danger);
                    let w = renderer.text_width_px(&text);
                    renderer.push_overlay_glyph_px_str(lx.round(), ty, &text, col);
                    lx += w + m.width;
                }
            }
            // A custom segment set via `glassy @ set-segment` still shows even
            // when `status_bar_segments` doesn't list the `custom` token —
            // appended at the end of the left side, so a script's output isn't
            // silently dropped just because the user hasn't edited their config.
            if !custom_at_marker && !inputs.custom_segments.is_empty() {
                lx = Self::render_custom_segments(
                    renderer,
                    &inputs.custom_segments,
                    lx,
                    ty,
                    m.width,
                    fg,
                );
            }
            let _ = lx;
        }

        // 4) Progress bar (1-px bottom edge): always rendered when the segment is in
        //    the list and a progress state is active.
        if seg_list.contains(&StatusBarSegment::Progress)
            && let Some(prog) = inputs.progress
        {
            use crate::image::ProgressState;
            let bar_bottom = bar_y + bar_h - 1.0;
            let (pct, color) = match prog {
                ProgressState::Set(p) => (p as f32 / 100.0, accent),
                ProgressState::Error(p) => (p as f32 / 100.0, danger),
                ProgressState::Indeterminate => (1.0, fg_dim),
                ProgressState::Remove => (0.0, fg_dim),
            };
            if pct > 0.0 {
                let prog_w = (bar_w * pct).max(2.0);
                renderer.push_overlay_px(0.0, bar_bottom, prog_w, 1.0, color);
            }
        }

        // 5) Left margin decorative rail.
        renderer.push_overlay_px(0.0, bar_y, 1.0, bar_h, mul(gui::rail()));
    }

    /// Render every active custom (Phase-1 plugin) segment left-to-right from
    /// `lx`, each in `color` and followed by one cell's margin. Shared by the
    /// explicit `custom` token placement and the "append at the end if unset"
    /// fallback in `paint_status_bar` above. Returns the advanced `lx`.
    fn render_custom_segments(
        renderer: &mut Renderer,
        segs: &[CustomSegment],
        mut lx: f32,
        ty: f32,
        margin: f32,
        color: [f32; 4],
    ) -> f32 {
        for seg in segs {
            if seg.text.is_empty() {
                continue;
            }
            let w = renderer.text_width_px(&seg.text);
            renderer.push_overlay_glyph_px_str(lx.round(), ty, &seg.text, color);
            lx += w + margin;
        }
        lx
    }

    /// Paint the inline tab-rename editor over the chip rect `r`: an opaque raised
    /// field with an accent ring, the in-progress `buffer` text (h-scrolled to keep
    /// the caret visible), the selection band, and the caret at its real column.
    /// `caret`/`selection` are char offsets into `buffer`. Associated (no `&self`)
    /// so it composes with the caller's `&mut Renderer` borrow.
    pub(crate) fn paint_tab_rename(
        renderer: &mut Renderer,
        r: gui::Rect,
        buffer: &str,
        caret: usize,
        selection: Option<(usize, usize)>,
    ) {
        let m = renderer.cell_metrics();
        let cell_w = m.width;
        let cell_h = m.height;
        let radius = gui_radius(cell_h);

        // Opaque field surface so the chip text underneath never shows through.
        renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, radius, gui::glass_float());
        // Accent focus ring (1px, softened): outer accent rrect minus an inset
        // surface rrect — a gentle halo rather than a harsh bright outline.
        let ring = {
            let a = color::accent();
            [a[0], a[1], a[2], 0.55]
        };
        renderer.push_overlay_rrect_px(r.x, r.y, r.w, r.h, radius, ring);
        let inset = 1.0;
        if r.w > 2.0 * inset && r.h > 2.0 * inset {
            renderer.push_overlay_rrect_px(
                r.x + inset,
                r.y + inset,
                r.w - 2.0 * inset,
                r.h - 2.0 * inset,
                (radius - inset).max(0.0),
                gui::glass_float(),
            );
        }

        // Text area: pad in, reserve one cell for the caret. H-scroll a window so
        // the caret stays visible (matching the shared text-field model).
        let pad = (cell_w * 0.6).round();
        let ty = (r.center_y() - cell_h * 0.5).round();
        let text_x0 = r.x + pad;
        let text_w = (r.w - 2.0 * pad - cell_w).max(0.0);
        let max_chars = (text_w / cell_w).floor().max(0.0) as usize;
        let chars: Vec<char> = buffer.chars().collect();
        // First visible char so the caret column lands inside the window.
        let scroll = if max_chars == 0 || caret < max_chars {
            0
        } else {
            caret + 1 - max_chars
        };
        let end = (scroll + max_chars).min(chars.len());

        // Selection band behind the glyphs, clipped to the visible window.
        if let Some((lo, hi)) = selection {
            let vlo = lo.max(scroll);
            let vhi = hi.min(end);
            if vhi > vlo {
                let sx = text_x0 + (vlo - scroll) as f32 * cell_w;
                let sw = (vhi - vlo) as f32 * cell_w;
                let mut band = color::selection_bg();
                band[3] = 0.45;
                renderer.push_overlay_px(sx.round(), ty, sw.round(), cell_h, band);
            }
        }

        let mut cx = text_x0;
        for &ch in &chars[scroll..end] {
            renderer.push_overlay_glyph_px(cx.round(), ty, ch, gui::fg());
            cx += cell_w;
        }
        // Caret at its real column within the visible window.
        let caret_col = caret.clamp(scroll, end);
        let caret_x = text_x0 + (caret_col - scroll) as f32 * cell_w;
        renderer.push_overlay_px(caret_x.round(), ty, 2.0, cell_h, color::accent());
    }

    /// Paint the settings form (§3.5) as a centered glass panel over a full-screen
    /// scrim, returning the interaction events for the caller to apply. Static (no
    /// `&self`) so it composes with the live `&mut Renderer` borrow held in
    /// `render`/`render_split`, threading the App-owned persistent GUI state.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_settings(
        renderer: &mut Renderer,
        config: &Config,
        font_px: f32,
        bell_idx: usize,
        font_choices: &[String],
        font_idx: usize,
        config_path: &str,
        open: gui::SettingsDrop,
        saved: bool,
        mouse: (f32, f32),
        mouse_down: bool,
        clicked: bool,
        gui_pressed: &mut Option<gui::WidgetId>,
        gui_focused: &mut Option<gui::WidgetId>,
        gui_anims: &mut std::collections::HashMap<gui::WidgetId, gui::Anim>,
        fields: &mut gui::SettingsFields,
        section: usize,
        section_scroll: f32,
        custom_swatches: &[[f32; 4]],
        custom_editing: usize,
        profile_names: &[String],
        active_profile: Option<&str>,
        popup_scroll: f32,
    ) -> gui::SettingsEvents {
        // Theme names + per-theme accent swatches (the cursor color each theme
        // deliberately picks to pop), sourced from the registry's single
        // built-ins+user-themes snapshot so both lists always agree.
        let theme_entries = color::theme_entries();
        let theme_names: Vec<&str> = theme_entries.iter().map(|e| e.canonical).collect();
        let swatches: Vec<[f32; 4]> = theme_entries
            .iter()
            .map(|e| {
                let c = e.theme.cursor;
                [
                    c.r as f32 / 255.0,
                    c.g as f32 / 255.0,
                    c.b as f32 / 255.0,
                    1.0,
                ]
            })
            .collect();
        let theme_idx = theme_names
            .iter()
            .position(|&n| n == config.theme)
            .unwrap_or(0);
        let font_refs: Vec<&str> = font_choices.iter().map(|s| s.as_str()).collect();
        let font_display = config.font_family.as_deref().unwrap_or("default");
        let font_features_str = config.font_features.join(" ");
        // Effective uniform padding shown in the form: the explicit `padding`
        // override if set, else 0 (meaning "cell-derived default").
        let padding_px = config.padding.unwrap_or(0.0).round().max(0.0) as u32;
        // Tab-bar policy as a segmented index (Auto / Always / Never).
        let tab_bar_mode = match config.show_tab_bar {
            crate::app::TabBarMode::Auto => 0,
            crate::app::TabBarMode::Always => 1,
            crate::app::TabBarMode::Never => 2,
        };

        let (sw, sh) = renderer.surface_size();
        let (cw, ch) = {
            let m = renderer.cell_metrics();
            (m.width, m.height)
        };
        let mut ui = gui::Ui::new(
            renderer,
            cw,
            ch,
            mouse,
            mouse_down,
            clicked,
            gui_pressed,
            gui_focused,
            gui_anims,
        );
        let cursor_style_idx = match config.cursor_style {
            crate::app::CursorStyleConfig::Block => 0,
            crate::app::CursorStyleConfig::Beam => 1,
            crate::app::CursorStyleConfig::Underline => 2,
        };
        // Custom-theme editor view data + the runtime profile names.
        let custom_labels: Vec<&str> = crate::app::settings_themes::CUSTOM_THEME_LABELS.to_vec();
        let profile_refs: Vec<&str> = profile_names.iter().map(|s| s.as_str()).collect();

        // settings-sections stream: Terminal / Effects / Quake / Notifications /
        // Advanced display strings + per-side padding (0 = unset, same sentinel
        // convention as the uniform `padding_px` above).
        let font_symbol_map_str = symbol_map_display(&config.font_symbol_map);
        let font_variations_str = config.font_variations.join(" ");
        let status_bar_segments_str =
            status_bar_segments_display(config.status_bar_segments.as_deref());
        let shell_str = shell_display(&config.shell);
        let cwd_str = config
            .initial_cwd
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let hints_chars_str = config.hints_chars.clone().unwrap_or_default();
        let font_bold_str = config.font_bold.clone().unwrap_or_default();
        let font_italic_str = config.font_italic.clone().unwrap_or_default();
        let font_bold_italic_str = config.font_bold_italic.clone().unwrap_or_default();
        let wallpaper_theme_str = config
            .wallpaper_theme
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let padding_top_px = config.padding_top.unwrap_or(0.0).round().max(0.0) as u32;
        let padding_bottom_px = config.padding_bottom.unwrap_or(0.0).round().max(0.0) as u32;
        let padding_left_px = config.padding_left.unwrap_or(0.0).round().max(0.0) as u32;
        let padding_right_px = config.padding_right.unwrap_or(0.0).round().max(0.0) as u32;

        let view = gui::SettingsView {
            font_px,
            opacity: config.opacity,
            bell: bell_idx,
            theme_idx,
            theme_names: &theme_names,
            theme_swatches: &swatches,
            font_family: font_display,
            font_names: &font_refs,
            font_idx,
            scrollback: config.scrollback,
            config_path,
            open,
            saved,
            status_bar: config.status_bar,
            pane_headers: config.pane_headers,
            follow_system: config.follow_system,
            ligatures: config.ligatures,
            restore_session: config.restore_session,
            padding: padding_px,
            word_separator: &config.word_separator,
            font_features: &font_features_str,
            cursor_style_idx,
            cursor_blink: config.cursor_blink,
            tab_bar_mode,
            window_effect_idx: config.window_effect.index(),
            custom_effect: config.custom_effect,
            section,
            section_scroll,
            copy_on_select: config.copy_on_select,
            minimap: config.minimap,
            command_badges: config.command_badges,
            cursor_trail: config.cursor_trail,
            title_show_cwd: config.title_show_cwd,
            title_show_count: config.title_show_count,
            theme_light: &config.theme_light,
            theme_dark: &config.theme_dark,
            custom_labels: &custom_labels,
            custom_swatches,
            custom_editing,
            profile_names: &profile_refs,
            active_profile,
            power_mode: config.power_mode,
            power_mode_intensity: config.power_mode_intensity,
            dim_unfocused: config.dim_unfocused,
            copy_html: config.copy_html,
            quake: config.quake,
            quake_height: config.quake_height,
            quake_animation_ms: config.quake_animation_ms,
            notify_command_finish: config.notify_command_finish,
            notify_command_threshold_ms: config.notify_command_threshold_ms,
            command_fold: config.command_fold,
            hints_chars: &hints_chars_str,
            font_bold: &font_bold_str,
            font_italic: &font_italic_str,
            font_bold_italic: &font_bold_italic_str,
            font_symbol_map: &font_symbol_map_str,
            font_variations: &font_variations_str,
            shell_display: &shell_str,
            cwd_display: &cwd_str,
            status_bar_segments: &status_bar_segments_str,
            status_bar_time_format: &config.status_bar_time_format,
            padding_top: padding_top_px,
            padding_bottom: padding_bottom_px,
            padding_left: padding_left_px,
            padding_right: padding_right_px,
            wallpaper_theme: &wallpaper_theme_str,
            popup_scroll,
        };
        ui.build_settings((sw as f32, sh as f32), &view, fields)
    }

    /// Apply the settings-form events to the live config + renderer + theme. Runs
    /// after `paint_settings` (the `Ui` borrow is dropped), driving the existing
    /// effects so opacity / font / theme preview immediately. Requests a repaint
    /// directly via the window (no `event_loop` is available inside `render`).
    pub(crate) fn apply_settings_events(&mut self, ev: gui::SettingsEvents) {
        // Remember the panel bounds for click-outside dismissal next frame.
        self.settings_panel = ev.panel;
        let mut changed = false;
        if ev.font_delta > 0 {
            self.resize_font(FontStep::Inc);
            changed = true;
        } else if ev.font_delta < 0 {
            self.resize_font(FontStep::Dec);
            changed = true;
        }
        if let Some(o) = ev.opacity {
            self.config.opacity = o;
            if let Some(r) = self.renderer.as_mut() {
                r.set_opacity(o);
            }
            changed = true;
        }
        if let Some(b) = ev.bell {
            self.set_bell_index(b);
            changed = true;
        }
        if ev.theme_toggle {
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::Theme {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Theme
            });
            changed = true;
        }
        if let Some(t) = ev.theme_pick {
            self.set_theme_by_idx(t);
            self.set_settings_drop(gui::SettingsDrop::None);
            changed = true;
        }
        if ev.font_toggle {
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::Font {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Font
            });
            changed = true;
        }
        if let Some(f) = ev.font_pick {
            self.set_font_family_index(f);
            self.set_settings_drop(gui::SettingsDrop::None);
            changed = true;
        }
        if ev.scrollback_delta != 0 {
            self.adjust_scrollback(ev.scrollback_delta);
            changed = true;
        }
        if let Some(idx) = ev.tab_bar_mode {
            self.set_tab_bar_mode_index(idx);
            changed = true;
        }
        if ev.window_effect_toggle {
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::Effect {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::Effect
            });
            changed = true;
        }
        if let Some(idx) = ev.window_effect {
            self.set_window_effect_index(idx);
            self.set_settings_drop(gui::SettingsDrop::None);
            changed = true;
        }
        if let Some((ch, val)) = ev.custom_effect
            && ch < self.config.custom_effect.len()
        {
            self.config.custom_effect[ch] = val.clamp(0.0, 1.0);
            self.apply_custom_effect();
            changed = true;
        }
        if ev.padding_delta != 0 {
            self.adjust_padding(ev.padding_delta);
            changed = true;
        }
        if ev.padding_top_delta != 0 {
            self.adjust_padding_top(ev.padding_top_delta);
            changed = true;
        }
        if ev.padding_bottom_delta != 0 {
            self.adjust_padding_bottom(ev.padding_bottom_delta);
            changed = true;
        }
        if ev.padding_left_delta != 0 {
            self.adjust_padding_left(ev.padding_left_delta);
            changed = true;
        }
        if ev.padding_right_delta != 0 {
            self.adjust_padding_right(ev.padding_right_delta);
            changed = true;
        }
        if ev.quake_animation_delta != 0 {
            self.adjust_quake_animation_ms(ev.quake_animation_delta);
            changed = true;
        }
        if ev.notify_threshold_delta != 0 {
            self.adjust_notify_threshold_ms(ev.notify_threshold_delta);
            changed = true;
        }
        if let Some(h) = ev.quake_height {
            self.config.quake_height = h.clamp(
                crate::config::parse::QUAKE_HEIGHT_MIN,
                crate::config::parse::QUAKE_HEIGHT_MAX,
            );
            self.settings_saved = false;
            changed = true;
        }
        if let Some(i) = ev.power_mode_intensity {
            let i = i.clamp(0.0, 1.0);
            self.config.power_mode_intensity = i;
            self.power.set_intensity(i);
            self.settings_saved = false;
            changed = true;
        }
        if let Some(cs_idx) = ev.cursor_style {
            self.set_cursor_style_index(cs_idx);
            changed = true;
        }
        // --- settings-themes stream events ---
        if let Some(idx) = ev.section_pick {
            self.settings_set_section(idx);
            changed = true;
        }
        if let Some(s) = ev.section_scroll {
            self.settings_section_scroll = s;
            changed = true;
        }
        if let Some(s) = ev.popup_scroll {
            self.settings_popup_scroll = s;
            changed = true;
        }
        // Every boolean toggle row fired this frame (see `SettingsEvents::toggled`'s
        // doc comment for why this replaced a dedicated `*_toggle: bool` field +
        // `if` block per toggle). Toggles with extra live side effects (grid
        // reflow, renderer sync, session-dirty) are matched explicitly; everything
        // else is a plain flip resolved via `CONFIG_TOGGLES`.
        for &wid in &ev.toggled {
            match wid {
                "settings/status_bar" => {
                    self.toggle_status_bar();
                }
                "settings/pane_headers" => {
                    self.toggle_pane_headers();
                }
                "settings/ligatures" => {
                    self.config.ligatures = !self.config.ligatures;
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_ligatures(self.config.ligatures);
                    }
                    self.settings_saved = false;
                }
                "settings/restore_session" => {
                    self.config.restore_session = !self.config.restore_session;
                    self.session_dirty = true;
                    self.settings_saved = false;
                }
                "settings/cursor_trail" => {
                    self.config.cursor_trail = !self.config.cursor_trail;
                    if let Some(r) = self.renderer.as_mut() {
                        r.set_cursor_trail(self.config.cursor_trail);
                    }
                    self.settings_saved = false;
                }
                "settings/power_mode" => {
                    // Runtime `PowerState::enabled` is separate from
                    // `config.power_mode` (seeded from it once at `App::new`, then
                    // independently runtime-toggled by the command palette) — flip
                    // both so a live settings toggle and Save agree with the
                    // palette's own toggle. `set_power_mode` also clears any live
                    // particles/shake when turning off.
                    self.config.power_mode = !self.config.power_mode;
                    self.set_power_mode(self.config.power_mode);
                    self.settings_saved = false;
                }
                "settings/command_fold" => {
                    // Mirrors `apply_config_reload`'s (helpers.rs) `command_fold`
                    // side effect: clearing an active fold state when the feature
                    // is turned off, so the view reverts to fully-expanded output
                    // instead of leaving stale folds the user can no longer toggle.
                    self.config.command_fold = !self.config.command_fold;
                    if !self.config.command_fold && self.fold_state.any() {
                        self.fold_state = command_blocks::FoldState::default();
                        self.force_full_redraw = true;
                    }
                    self.settings_saved = false;
                }
                other => {
                    if let Some((_, apply)) = CONFIG_TOGGLES.iter().find(|(id, _)| *id == other) {
                        *apply(&mut self.config) ^= true;
                        self.settings_saved = false;
                    } else {
                        log::debug!("glassy: settings toggle: unknown widget id '{other}'");
                    }
                }
            }
            changed = true;
        }
        if ev.theme_light_toggle {
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::ThemeLight {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::ThemeLight
            });
            changed = true;
        }
        if let Some(idx) = ev.theme_light_pick {
            self.set_theme_light_by_idx(idx);
            self.set_settings_drop(gui::SettingsDrop::None);
            changed = true;
        }
        if ev.theme_dark_toggle {
            self.set_settings_drop(if self.settings_drop == gui::SettingsDrop::ThemeDark {
                gui::SettingsDrop::None
            } else {
                gui::SettingsDrop::ThemeDark
            });
            changed = true;
        }
        if let Some(idx) = ev.theme_dark_pick {
            self.set_theme_dark_by_idx(idx);
            self.set_settings_drop(gui::SettingsDrop::None);
            changed = true;
        }
        if let Some(idx) = ev.custom_color_pick {
            self.select_custom_color(idx);
            changed = true;
        }
        if ev.custom_apply {
            self.apply_custom_theme_preview();
            changed = true;
        }
        if ev.custom_save {
            self.save_custom_theme();
            changed = true;
        }
        if let Some(idx) = ev.profile_pick {
            self.switch_profile_by_idx(idx);
            changed = true;
        }
        if ev.profile_pick_default {
            self.switch_to_base_profile();
            changed = true;
        }
        if ev.profile_create {
            self.create_profile_from_current();
            changed = true;
        }
        if ev.copy_path {
            self.copy_config_path();
            changed = true;
        }
        if ev.open_path {
            self.open_config_path();
        }
        if ev.save {
            self.save_settings();
            changed = true;
        }
        if ev.close {
            self.settings_open = false;
            self.set_settings_drop(gui::SettingsDrop::None);
            self.overlay_opened_by_press = false;
            changed = true;
        }
        if changed {
            self.force_full_redraw = true;
            self.dirty = true;
            if let Some(w) = &self.window {
                w.request_redraw();
            }
        }
    }

    /// Paint the confirm-close modal: a centered frosted glass card asking the user
    /// to confirm closing when a process is still running in the tab/pane. Returns
    /// the interaction result so the caller can decide whether to proceed, cancel,
    /// or wait for a button click.
    ///
    /// Layout:
    ///   ┌─────────────────────────────────────┐
    ///   │  A process is still running.        │
    ///   │  Close this tab anyway?             │
    ///   │                                     │
    ///   │       [Cancel]      [Close]         │
    ///   └─────────────────────────────────────┘
    ///
    /// The "Close" button uses the danger color; "Cancel" is the neutral surface.
    /// Clicking outside the card is treated as Cancel. Static (no `&self`) so it
    /// composes with the caller's live `&mut Renderer` borrow.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn paint_confirm_close(
        renderer: &mut Renderer,
        surface: (f32, f32),
        cell_w: f32,
        cell_h: f32,
        mouse: (f32, f32),
        mouse_down: bool,
        click: bool,
        gui_pressed: &mut Option<gui::WidgetId>,
        gui_anims: &mut std::collections::HashMap<gui::WidgetId, gui::Anim>,
    ) -> ConfirmCloseResult {
        let (sw, sh) = surface;

        // Full-screen dimming scrim.
        renderer.push_overlay_px(0.0, 0.0, sw, sh, [0.0, 0.0, 0.0, 0.45]);

        // Card dimensions: wide enough for two lines + buttons.
        let card_w = (cell_w * 38.0).clamp(280.0, sw * 0.9);
        let card_h = cell_h * 7.0;
        let card_x = ((sw - card_w) * 0.5).round();
        let card_y = ((sh - card_h) * 0.5).round();
        let radius = gui_radius(cell_h);

        // Glass card background.
        renderer.push_overlay_rrect_px(card_x, card_y, card_w, card_h, radius, gui::glass_float());
        // Subtle border.
        let border = {
            let h = gui::hairline();
            [h[0], h[1], h[2], h[3] * 1.5]
        };
        renderer.push_overlay_rrect_px(
            card_x - 0.5,
            card_y - 0.5,
            card_w + 1.0,
            card_h + 1.0,
            radius + 0.5,
            border,
        );
        // Repaint inside to restore the card surface (border is painted over).
        renderer.push_overlay_rrect_px(card_x, card_y, card_w, card_h, radius, gui::glass_float());

        // Body text: two lines.
        let line1 = "A process is still running.";
        let line2 = "Close this tab anyway?";
        let tx = (card_x + cell_w).round();
        let ty1 = (card_y + cell_h).round();
        let ty2 = (ty1 + cell_h * 1.5).round();
        renderer.push_overlay_glyph_px_str(tx, ty1, line1, gui::fg());
        renderer.push_overlay_glyph_px_str(tx, ty2, line2, gui::fg_dim());

        // Button row: Cancel (left) and Close/danger (right), bottom of card.
        let btn_w = (cell_w * 8.0).round();
        let btn_h = (cell_h * 1.6).round();
        let btn_pad = cell_w;
        let btn_y = (card_y + card_h - btn_h - cell_h * 0.75).round();

        // Cancel button (left-of-center).
        let cancel_id = gui::id("confirm_close/cancel");
        let cancel_x = (card_x + card_w * 0.5 - btn_w - btn_pad * 0.5).round();
        let cancel_r = gui::Rect::new(cancel_x, btn_y, btn_w, btn_h);
        let cancel_hover = gui::hit(cancel_r, mouse.0, mouse.1);
        let cancel_held = cancel_hover && *gui_pressed == Some(cancel_id) && mouse_down;
        let cancel_bg = gui::state_fill(
            gui::glass_raised(),
            if cancel_hover { 0.7 } else { 0.0 },
            cancel_held,
        );
        renderer.push_overlay_rrect_px(
            cancel_r.x, cancel_r.y, cancel_r.w, cancel_r.h, radius, cancel_bg,
        );
        let cancel_label = "Cancel";
        let lw = cancel_label.chars().count() as f32 * cell_w;
        let ltx = (cancel_r.x + (cancel_r.w - lw) * 0.5).round();
        let lty = (cancel_r.center_y() - cell_h * 0.5).round();
        renderer.push_overlay_glyph_px_str(ltx, lty, cancel_label, gui::fg());

        // Close (danger) button (right-of-center).
        let close_id = gui::id("confirm_close/close");
        let close_x = (card_x + card_w * 0.5 + btn_pad * 0.5).round();
        let close_r = gui::Rect::new(close_x, btn_y, btn_w, btn_h);
        let close_hover = gui::hit(close_r, mouse.0, mouse.1);
        let close_held = close_hover && *gui_pressed == Some(close_id) && mouse_down;
        let danger = color::danger();
        let close_bg_base = [danger[0], danger[1], danger[2], 0.85];
        let close_bg = gui::state_fill(
            close_bg_base,
            if close_hover { 0.7 } else { 0.0 },
            close_held,
        );
        renderer
            .push_overlay_rrect_px(close_r.x, close_r.y, close_r.w, close_r.h, radius, close_bg);
        let close_label = "Close";
        let clw = close_label.chars().count() as f32 * cell_w;
        let cltx = (close_r.x + (close_r.w - clw) * 0.5).round();
        let clty = (close_r.center_y() - cell_h * 0.5).round();
        // Danger button uses contrasting text.
        let dluma = 0.2126 * danger[0] + 0.7152 * danger[1] + 0.0722 * danger[2];
        let close_fg = if dluma > 0.4 {
            [0.06, 0.06, 0.07, 1.0]
        } else {
            [0.97, 0.97, 0.98, 1.0]
        };
        renderer.push_overlay_glyph_px_str(cltx, clty, close_label, close_fg);

        // Track press latching.
        if mouse_down {
            if cancel_hover && gui_pressed.is_none() {
                *gui_pressed = Some(cancel_id);
            } else if close_hover && gui_pressed.is_none() {
                *gui_pressed = Some(close_id);
            }
        }

        // Settle anims (no per-button animation needed; just suppress the entry).
        let _ = gui_anims;

        // Resolve a click.
        if click {
            let pressed_id = *gui_pressed;
            if pressed_id == Some(cancel_id) && cancel_hover {
                return ConfirmCloseResult::Cancel;
            }
            if pressed_id == Some(close_id) && close_hover {
                return ConfirmCloseResult::Confirm;
            }
            // Click outside the card: treat as Cancel.
            if !gui::hit(
                gui::Rect::new(card_x, card_y, card_w, card_h),
                mouse.0,
                mouse.1,
            ) {
                return ConfirmCloseResult::Cancel;
            }
        }

        ConfirmCloseResult::Pending
    }
}
