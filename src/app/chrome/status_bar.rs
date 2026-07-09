//! Status-bar painting: the data-driven `SEGMENT_SPECS` table, per-segment
//! render fns, and the `App::status_bar_inputs` / `App::paint_status_bar` /
//! `App::render_custom_segments` trio. Split out of the former flat
//! `chrome.rs` (settings-modularity stream) — see `super`'s module doc.

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
}
