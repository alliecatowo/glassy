//! Warp-style command blocks: exit-status badges, durations, and output folding
//! driven by OSC 133 shell-integration marks.
//!
//! The per-session [`crate::pty::PromptTracker`] records one
//! [`crate::pty::CommandBlock`] per prompt zone (prompt row, output row, end row,
//! exit code, start/end time). This module renders those blocks as unobtrusive
//! gutter affordances and tracks which blocks the user has folded.
//!
//! What ships here:
//! - **Exit-status badge + duration**: a right-aligned `✓ 1.2s` / `✗ 1 3.4s`
//!   chip on each visible prompt row, green for success and red for failure.
//! - **Fold state + caret**: a fold caret in the left gutter of a finished
//!   command's prompt row; toggling it collapses that command's output into a
//!   single summary line. Folding is *partial* (see [`fold-status`](#folding)).
//! - **Opt-in card chrome** (`command_blocks = cards`, [`crate::app::CommandBlocksMode`]):
//!   a subtle glass band + accent rail behind each finished block's full row
//!   range, purely a presentation layer over the same tracking data — see
//!   [`build_chrome_bands`].
//! - **Gutter click-to-select**: clicking the left-margin strip next to a
//!   finished block's rows selects its output as one unit — see
//!   [`App::command_block_gutter_click_at`] / [`App::select_command_output`].
//!
//! # Folding
//!
//! A true fold would re-map screen rows so the collapsed output occupies zero
//! height. glassy's single-pane render loop iterates alacritty's
//! `display_iter` 1:1 with screen rows, so a zero-height collapse would require
//! re-flowing the whole viewport. Instead this implements a *visual* fold: the
//! folded output rows are dimmed and overlaid with a "… N lines hidden" summary
//! bar, so the block reads as collapsed without disturbing the grid geometry.
//! Full row-elision is tracked as a follow-up.

use super::*;
use crate::pty::CommandBlock;

/// Set of folded command blocks for one session, keyed by the block's prompt
/// row (a stable-enough identity within a session's scrollback lifetime).
#[derive(Default)]
pub(crate) struct FoldState {
    /// Absolute prompt rows whose command output is folded.
    folded: std::collections::HashSet<i32>,
}

impl FoldState {
    /// Whether the block anchored at `prompt_row` is currently folded.
    pub(crate) fn is_folded(&self, prompt_row: i32) -> bool {
        self.folded.contains(&prompt_row)
    }

    /// Toggle the fold state of the block at `prompt_row`. Returns the new state.
    pub(crate) fn toggle(&mut self, prompt_row: i32) -> bool {
        if self.folded.remove(&prompt_row) {
            false
        } else {
            self.folded.insert(prompt_row);
            true
        }
    }

    /// Whether any block is folded (drives whether the render path does the extra
    /// per-row fold work at all).
    pub(crate) fn any(&self) -> bool {
        !self.folded.is_empty()
    }
}

/// Format a command duration compactly: `820ms`, `1.2s`, `3m04s`. Sub-second is
/// shown in ms; under a minute in fractional seconds; longer in `Mm SSs`.
pub(crate) fn format_duration(d: std::time::Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let secs = d.as_secs();
        format!("{}m{:02}s", secs / 60, secs % 60)
    }
}

/// One visible command-block badge to paint: the viewport row of the prompt, the
/// label text, and its color. Computed under the `&self` borrow (so the render
/// path can hand the renderer plain owned data).
pub(crate) struct BadgePaint {
    /// Viewport row (0-based) of the prompt line the badge sits on.
    pub vp_row: usize,
    /// Badge text, e.g. `✓ 1.2s` or `✗ 1 · 3.4s`.
    pub text: String,
    /// Badge color (success green / failure red).
    pub color: [f32; 4],
    /// Whether this block is foldable (finished + has an output range) and its
    /// current fold state, for the gutter caret.
    pub foldable: bool,
    pub folded: bool,
}

/// Build the list of badges to paint for the blocks whose prompt row is within
/// the current viewport `[display_offset .. display_offset + rows)`.
///
/// `display_offset` is the live scrollback offset (absolute_row = vp_row +
/// display_offset for the bottom-anchored grid: vp_row = prompt_row -
/// display_offset). Only *finished* blocks get a badge (a running command has no
/// exit code / duration yet).
pub(crate) fn build_badges(
    blocks: &std::collections::VecDeque<CommandBlock>,
    folds: &FoldState,
    display_offset: i32,
    rows: usize,
) -> Vec<BadgePaint> {
    let mut out = Vec::new();
    for b in blocks.iter() {
        if !b.is_finished() {
            continue;
        }
        // Map the absolute prompt row to a viewport row.
        let vp = b.prompt_row - display_offset;
        if vp < 0 || vp >= rows as i32 {
            continue;
        }
        let exit = b.exit_code;
        let ok = b.succeeded();
        let color = if ok {
            color::success()
        } else {
            color::danger()
        };
        // Label: status glyph + (failing exit code) + duration.
        let mut text = if ok {
            "\u{2713}".to_string() // ✓
        } else {
            "\u{2717}".to_string() // ✗
        };
        if let Some(code) = exit.filter(|&c| c != 0) {
            text.push_str(&format!(" {code}"));
        }
        if let Some(d) = b.duration() {
            // Only show duration for commands that took long enough to matter.
            if d.as_millis() >= 50 {
                text.push_str(&format!(" {}", format_duration(d)));
            }
        }
        let foldable = b.output_row.is_some() && b.end_row.is_some();
        out.push(BadgePaint {
            vp_row: vp as usize,
            text,
            color,
            foldable,
            folded: folds.is_folded(b.prompt_row),
        });
    }
    out
}

/// A folded output range to dim + summarize, in viewport rows.
pub(crate) struct FoldRange {
    /// First viewport row of the folded output (inclusive).
    pub start_vp: usize,
    /// One past the last viewport row of the folded output.
    pub end_vp: usize,
    /// Number of source lines hidden by the fold (for the summary text).
    pub hidden_lines: usize,
}

/// Compute the viewport row ranges of folded command output, clamped to the
/// visible grid. Each finished, folded block hides its output rows
/// `(output_row + 1 ..= end_row)` (the line after the command, through the line
/// the prompt's `D` mark landed on).
pub(crate) fn build_fold_ranges(
    blocks: &std::collections::VecDeque<CommandBlock>,
    folds: &FoldState,
    display_offset: i32,
    rows: usize,
) -> Vec<FoldRange> {
    if !folds.any() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for b in blocks.iter() {
        let (Some(o), Some(e)) = (b.output_row, b.end_row) else {
            continue;
        };
        if !folds.is_folded(b.prompt_row) {
            continue;
        }
        // Hide the output body: the first output line through the row before the
        // next prompt's D mark. Keep the command line itself (output_row) visible.
        let abs_start = o + 1;
        let abs_end = e; // exclusive of the D-mark row keeps the next prompt visible
        if abs_end <= abs_start {
            continue;
        }
        let vp_start = (abs_start - display_offset).max(0);
        let vp_end = (abs_end - display_offset).clamp(0, rows as i32);
        if vp_end <= vp_start {
            continue;
        }
        out.push(FoldRange {
            start_vp: vp_start as usize,
            end_vp: vp_end as usize,
            hidden_lines: (abs_end - abs_start) as usize,
        });
    }
    out
}

/// One command-block "card" band to paint behind a finished command's full row
/// range (its prompt line through its `D`-mark line), in viewport rows. Purely
/// presentational: unlike [`FoldRange`] (only ever covers *folded* blocks), a
/// card is drawn for every finished block whenever `command_blocks = cards` is
/// on, whether or not that block also happens to be folded.
pub(crate) struct ChromeBand {
    /// First viewport row of the block (its prompt line), inclusive.
    pub start_vp: usize,
    /// One past the last viewport row of the block (its `D`-mark line).
    pub end_vp: usize,
    /// Left-rail accent color: success green / failure red, matching the
    /// badge's own coloring so the card and its badge read as one unit.
    pub accent: [f32; 4],
}

/// Compute the viewport row ranges of every finished command block's card band
/// (`prompt_row..=end_row`), clamped to the visible grid. Empty when `blocks`
/// has no finished entries in view; the caller only calls this at all when
/// `command_blocks == CommandBlocksMode::Cards` (see `render.rs`).
pub(crate) fn build_chrome_bands(
    blocks: &std::collections::VecDeque<CommandBlock>,
    display_offset: i32,
    rows: usize,
) -> Vec<ChromeBand> {
    let mut out = Vec::new();
    for b in blocks.iter() {
        let Some(e) = b.end_row else { continue };
        let abs_start = b.prompt_row;
        let abs_end = e + 1; // +1: inclusive of the D-mark row itself
        if abs_end <= abs_start {
            continue;
        }
        let vp_start = (abs_start - display_offset).max(0);
        let vp_end = (abs_end - display_offset).clamp(0, rows as i32);
        if vp_end <= vp_start {
            continue;
        }
        out.push(ChromeBand {
            start_vp: vp_start as usize,
            end_vp: vp_end as usize,
            accent: if b.succeeded() {
                color::success()
            } else {
                color::danger()
            },
        });
    }
    out
}

impl App {
    /// The prompt row of the finished command block whose output range contains
    /// (or whose prompt is at) the current bottom-of-viewport, for the
    /// fold-toggle keybind. Picks the most recent finished block at or above the
    /// cursor/viewport so `ToggleFold` collapses "the command I'm looking at".
    pub(crate) fn foldable_block_at_view(&self) -> Option<i32> {
        let pty = self.pty.as_ref()?;
        let (disp, screen_rows) = {
            let t = pty.term.lock();
            (t.grid().display_offset() as i32, self.rows as i32)
        };
        // Anchor on the last visible viewport row (absolute).
        let anchor = disp + screen_rows - 1;
        let guard = pty.prompts.lock().ok()?;
        guard
            .blocks
            .iter()
            .rev()
            .find(|b| b.is_finished() && b.output_row.is_some() && b.prompt_row <= anchor)
            .map(|b| b.prompt_row)
    }

    /// The prompt row of the finished command block whose gutter (left-margin)
    /// affordance was clicked at pixel `(x, y)`, if any. The clickable strip is
    /// the same left-padding column the fold caret glyph is drawn in
    /// (see [`Self::paint_command_blocks`]), and accepts a click anywhere along
    /// the block's full row range (not just the caret's own prompt row), so a
    /// Warp-style "click the block to select its output" gesture works whether
    /// or not `cards` chrome is on. Returns `None` when badges are disabled,
    /// the click landed inside the grid proper (not the margin), or no
    /// finished block's row range covers the clicked row.
    pub(crate) fn command_block_gutter_click_at(&self, x: f64, y: f64) -> Option<i32> {
        if !self.config.command_badges {
            return None;
        }
        let renderer = self.renderer.as_ref()?;
        let pad = renderer.pad() as f64;
        let ox = self
            .focused_pane_rect()
            .map(|r| r.x as f64 + pad)
            .unwrap_or(pad);
        // Only the padding strip immediately left of column 0 is the
        // affordance; a click inside the grid falls through to normal text
        // selection (handled by the caller).
        if x < ox - pad || x >= ox {
            return None;
        }
        let (_, row) = self.px_to_cell(x, y);
        let pty = self.pty.as_ref()?;
        let display_offset = pty.term.lock().grid().display_offset() as i32;
        let abs_row = row as i32 + display_offset;
        let guard = pty.prompts.lock().ok()?;
        block_at_abs_row(&guard.blocks, abs_row)
    }

    /// Select a finished command's output (`output_row + 1 ..= end_row`, the
    /// lines after its command line through its `D` mark) as one
    /// `Lines`-granularity selection, for the gutter click-to-select
    /// affordance. `prompt_row` identifies the block, as returned by
    /// [`Self::command_block_gutter_click_at`]. Returns `false` (no-op) if the
    /// block no longer exists, hasn't finished, or has an empty output range.
    pub(crate) fn select_command_output(&mut self, prompt_row: i32) -> bool {
        let Some(pty) = self.pty.as_ref() else {
            return false;
        };
        let range = {
            let Ok(guard) = pty.prompts.lock() else {
                return false;
            };
            let Some(b) = guard.blocks.iter().find(|b| b.prompt_row == prompt_row) else {
                return false;
            };
            let (Some(o), Some(e)) = (b.output_row, b.end_row) else {
                return false;
            };
            (o, e)
        };
        let mut term = pty.term.lock();
        let cols = term.grid().columns();
        let display_offset = term.grid().display_offset() as i32;
        let Some((start, end)) = output_selection_points(range.0, range.1, display_offset, cols)
        else {
            return false;
        };
        let mut sel = Selection::new(SelectionType::Lines, start, Side::Left);
        sel.update(end, Side::Right);
        term.selection = Some(sel);
        true
    }

    /// Headless capture hook (`GLASSY_CMDBLOCK`): seed the active pane's
    /// [`PromptTracker`](crate::pty::PromptTracker) with a few synthetic, finished
    /// command blocks (one succeeding, one failing, one long-running) and fold one
    /// of them, so the badge + fold overlays can be rendered for a screenshot
    /// without sourcing a real shell-integration script.
    pub(crate) fn inject_demo_command_blocks(&mut self) {
        use crate::pty::CommandBlock;
        let now = Instant::now();
        let blocks = [
            // row, output_row, end_row, exit, duration_ms
            (1_i32, 2, 4, 0_i32, 820_u64),
            (5, 6, 12, 1, 3400),
            (13, 14, 18, 0, 64_000),
        ];
        if let Some(pty) = self.pty.as_ref()
            && let Ok(mut g) = pty.prompts.lock()
        {
            for (prow, orow, erow, exit, ms) in blocks {
                let started = now
                    .checked_sub(std::time::Duration::from_millis(ms))
                    .unwrap_or(now);
                g.rows.push_back(prow);
                g.blocks.push_back(CommandBlock {
                    prompt_row: prow,
                    output_row: Some(orow),
                    end_row: Some(erow),
                    exit_code: Some(exit),
                    started_at: Some(started),
                    ended_at: Some(now),
                });
            }
        }
        // Fold the failing block so the fold overlay is visible too. Done after
        // the `if let` (whose `self.pty` borrow ends at the closing brace of the
        // let-chain) so it does not collide with this `&mut self` access.
        self.fold_state.toggle(5);
    }

    /// Headless capture hook (`GLASSY_IMGDEMO`): synthesise a small RGBA colour
    /// swatch and inject it directly into the active pane's [`ImageStore`] so the
    /// kitty inline-image render path can be captured without a real image-producing
    /// program. Produces an 8×8 rainbow-gradient image (id = 1) placed at grid row
    /// 2, col 2. Exercises the full GPU image-quad draw path: pixels stored, placed,
    /// picked up by the renderer's image overlay pass, uploaded as a wgpu texture,
    /// and blitted over the grid.
    pub(crate) fn inject_demo_inline_image(&mut self) {
        use crate::image::DecodedImage;
        // Build an 8×8 RGBA rainbow swatch. Each pixel's hue cycles across both
        // axes so the rendered quad shows vivid colour rather than a flat block.
        const SZ: u32 = 8;
        let mut rgba = Vec::with_capacity((SZ * SZ * 4) as usize);
        for row in 0..SZ {
            for col in 0..SZ {
                // Simple HSV→RGB: hue cycles across columns, value across rows.
                let h = (col as f32 / SZ as f32) * 360.0;
                let v = 0.5 + 0.5 * (row as f32 / SZ as f32);
                let (r, g, b) = hsv_to_rgb(h, 1.0, v);
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                rgba.push(255);
            }
        }
        let img = DecodedImage {
            width: SZ,
            height: SZ,
            rgba,
        };
        if let Some(pty) = self.pty.as_ref() {
            let mut store = pty.images.lock();
            // id 1 is a low kitty-range id; use it as our canonical demo image.
            store.insert_pixels(1, img);
            // Place it at row 2, col 2, occupying 4 cells × 2 rows.
            store.place(1, 2, 2, 4, 2);
        }
    }

    /// Toggle the fold state of the command block currently in view. Bound to the
    /// `ToggleFold` key action and the command palette. No-op when there is no
    /// finished, foldable block in view, or when output folding is disabled via
    /// the `command_fold` config key.
    pub(crate) fn toggle_command_fold(&mut self, event_loop: &ActiveEventLoop) {
        if !self.config.command_fold {
            return;
        }
        if let Some(prompt_row) = self.foldable_block_at_view() {
            self.fold_state.toggle(prompt_row);
            self.force_full_redraw = true;
            self.mark_dirty(event_loop);
        }
    }

    /// Paint command-block overlays: the opt-in card chrome band behind each
    /// finished block, the dim + "… N lines hidden" summary for each folded
    /// output range, and the right-aligned exit-status/duration badge (with a
    /// fold caret) on each visible finished prompt row. Associated fn (no
    /// `&self`) so it composes with the caller's live `&mut Renderer` borrow.
    pub(crate) fn paint_command_blocks(
        renderer: &mut Renderer,
        cols: usize,
        rows: usize,
        badges: &[BadgePaint],
        folds: &[FoldRange],
        bands: &[ChromeBand],
    ) {
        if badges.is_empty() && folds.is_empty() && bands.is_empty() {
            return;
        }
        let m = renderer.cell_metrics();
        let cw = m.width;
        let chh = m.height;
        let pad = renderer.pad();
        let goy = renderer.grid_origin_y();
        let (sw, _sh) = renderer.surface_size();
        let grid_w = cols as f32 * cw;

        // 0) Card chrome bands (`command_blocks = cards`): a subtle glass tint
        //    across the block's width plus a colored left rail, painted first so
        //    the fold-dim scrim and badge text (below) still read cleanly on top.
        //    These composite over already-rendered glyphs (this pass runs after
        //    the cell/text pass), so the tint alpha is kept low enough that text
        //    stays fully legible — the same trick the fold dim scrim already
        //    relies on, just much subtler.
        for band in bands {
            if band.start_vp >= rows {
                continue;
            }
            let y0 = band.start_vp as f32 * chh + pad + goy;
            let h = (band.end_vp.min(rows) - band.start_vp) as f32 * chh;
            if h <= 0.0 {
                continue;
            }
            // Theme-derived tint (never a hardcoded white): the same elevation
            // math the GUI's own raised-surface fills use, at a much lower
            // alpha since this sits over live terminal text rather than chrome.
            let tint = gui::glass_elevate(color::default_bg(), 0.10);
            renderer.push_overlay_px(pad, y0, grid_w, h, [tint[0], tint[1], tint[2], 0.10]);
            // Left accent rail in the gutter, colored by exit status (mirrors
            // the badge's own success/failure coloring) so the card and its
            // badge read as one unit.
            let rail_w = (cw * 0.25).max(2.0);
            let rx = (pad - rail_w).max(0.0);
            renderer.push_overlay_px(
                rx,
                y0,
                rail_w,
                h,
                [band.accent[0], band.accent[1], band.accent[2], 0.55],
            );
        }

        // 1) Folded output ranges: dim the body and stamp a summary bar on the
        //    first hidden row so the block reads as collapsed.
        for fr in folds {
            if fr.start_vp >= rows {
                continue;
            }
            let y0 = fr.start_vp as f32 * chh + pad + goy;
            let h = (fr.end_vp.min(rows) - fr.start_vp) as f32 * chh;
            if h <= 0.0 {
                continue;
            }
            // Dim scrim over the hidden output.
            renderer.push_overlay_px(pad, y0, grid_w, h, [0.0, 0.0, 0.0, 0.55]);
            // Summary text on the first hidden row.
            let summary = format!("\u{25B8} {} lines hidden", fr.hidden_lines); // ▸
            let tx = (pad + cw).round();
            let ty = y0.round();
            renderer.push_overlay_glyph_px_str(tx, ty, &summary, gui::fg_dim());
        }

        // 2) Exit-status badges: a right-aligned chip on each prompt row.
        for b in badges {
            if b.vp_row >= rows {
                continue;
            }
            let y = b.vp_row as f32 * chh + pad + goy;
            // Fold caret in the left gutter (within pad) for foldable blocks.
            if b.foldable {
                let caret = if b.folded { '\u{25B8}' } else { '\u{25BE}' }; // ▸ / ▾
                let gx = (pad - cw).max(0.0);
                renderer.push_overlay_glyph_px(gx.round(), y.round(), caret, gui::fg_dim());
            }
            // Right-aligned badge text inside the grid's right edge.
            let w = renderer.text_width_px(&b.text);
            let bx = (pad + grid_w - w - cw * 0.5).max(pad);
            // Keep the badge on-screen even with narrow surfaces.
            let bx = bx.min(sw as f32 - w);
            renderer.push_overlay_glyph_px_str(bx.round(), y.round(), &b.text, b.color);
        }
    }
}

// ---------------------------------------------------------------------------
// Free helpers
// ---------------------------------------------------------------------------

/// The prompt row of the finished command block (if any) whose row range
/// (`prompt_row..=end_row`) contains the absolute grid row `abs_row`. Pure row
/// math split out from [`App::command_block_gutter_click_at`] so it is
/// unit-testable without a live renderer/pty.
pub(crate) fn block_at_abs_row(
    blocks: &std::collections::VecDeque<CommandBlock>,
    abs_row: i32,
) -> Option<i32> {
    blocks
        .iter()
        .rev()
        .find(|b| {
            b.is_finished()
                && b.output_row.is_some()
                && abs_row >= b.prompt_row
                && abs_row <= b.end_row.unwrap_or(i32::MIN)
        })
        .map(|b| b.prompt_row)
}

/// Compute the `(start, end)` grid points for a `Lines`-granularity selection
/// spanning a finished command's output body (`output_row + 1 ..= end_row`),
/// translating the stored ABSOLUTE rows to the live-viewport `Line` coordinates
/// `Selection` uses. Returns `None` for an empty output range (the command
/// produced no output) or a zero-width grid.
///
/// The translation is TWO steps, matching `App::grid_point` / alacritty's
/// `viewport_to_point`: first absolute → viewport (`abs - display_offset`, the
/// inverse of the gutter click's `viewport + display_offset`), then viewport →
/// `Line` (another `- display_offset`). Composing both is equivalent to
/// subtracting `2 * display_offset`. Doing only one subtraction lands the
/// selection on the wrong rows whenever the user has scrolled
/// (`display_offset != 0`).
pub(crate) fn output_selection_points(
    output_row: i32,
    end_row: i32,
    display_offset: i32,
    cols: usize,
) -> Option<(Point, Point)> {
    if end_row <= output_row || cols == 0 {
        return None;
    }
    // abs -> viewport -> Line (two `- display_offset` steps).
    let start_vp = output_row + 1 - display_offset;
    let end_vp = end_row - display_offset;
    let start = Point::new(Line(start_vp - display_offset), Column(0));
    let end = Point::new(Line(end_vp - display_offset), Column(cols - 1));
    Some((start, end))
}

/// Convert HSV (hue 0..360, saturation/value 0..1) to an sRGB `(r, g, b)` triple
/// with each component in `0..=255`. Used by the `GLASSY_IMGDEMO` swatch generator.
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (u8, u8, u8) {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;
    let (r1, g1, b1) = match (h as u32 / 60) % 6 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pty::CommandBlock;
    use std::collections::VecDeque;
    use std::time::Duration;

    fn block(prow: i32, orow: Option<i32>, erow: Option<i32>, exit: Option<i32>) -> CommandBlock {
        CommandBlock {
            prompt_row: prow,
            output_row: orow,
            end_row: erow,
            exit_code: exit,
            started_at: None,
            ended_at: None,
        }
    }

    #[test]
    fn duration_formats_compactly() {
        assert_eq!(format_duration(Duration::from_millis(0)), "0ms");
        assert_eq!(format_duration(Duration::from_millis(820)), "820ms");
        assert_eq!(format_duration(Duration::from_millis(1234)), "1.2s");
        assert_eq!(format_duration(Duration::from_millis(59_900)), "59.9s");
        assert_eq!(format_duration(Duration::from_secs(64)), "1m04s");
        assert_eq!(format_duration(Duration::from_secs(605)), "10m05s");
    }

    #[test]
    fn fold_state_toggles_and_tracks() {
        let mut f = FoldState::default();
        assert!(!f.any());
        assert!(f.toggle(5)); // now folded
        assert!(f.is_folded(5));
        assert!(f.any());
        assert!(!f.toggle(5)); // now unfolded
        assert!(!f.is_folded(5));
        assert!(!f.any());
    }

    #[test]
    fn badges_skip_running_and_offscreen_blocks() {
        let mut blocks = VecDeque::new();
        // Finished, in view.
        blocks.push_back(block(2, Some(3), Some(6), Some(0)));
        // Still running (no end row) → no badge.
        blocks.push_back(block(8, Some(9), None, None));
        // Finished but scrolled off the top (row < display_offset) → no badge.
        blocks.push_back(block(-5, Some(-4), Some(-1), Some(1)));
        let folds = FoldState::default();
        let badges = build_badges(&blocks, &folds, 0, 24);
        assert_eq!(
            badges.len(),
            1,
            "only the in-view finished block gets a badge"
        );
        assert_eq!(badges[0].vp_row, 2);
        assert!(badges[0].foldable);
    }

    #[test]
    fn badge_text_marks_success_and_failure() {
        let mut blocks = VecDeque::new();
        let mut ok = block(0, Some(1), Some(3), Some(0));
        ok.started_at = Some(Instant::now() - Duration::from_millis(1500));
        ok.ended_at = Some(Instant::now());
        let mut bad = block(4, Some(5), Some(7), Some(2));
        bad.started_at = Some(Instant::now() - Duration::from_millis(1500));
        bad.ended_at = Some(Instant::now());
        blocks.push_back(ok);
        blocks.push_back(bad);
        let folds = FoldState::default();
        let badges = build_badges(&blocks, &folds, 0, 24);
        assert!(badges[0].text.starts_with('\u{2713}'), "success uses ✓");
        assert!(badges[1].text.starts_with('\u{2717}'), "failure uses ✗");
        assert!(badges[1].text.contains('2'), "failure shows the exit code");
    }

    #[test]
    fn fold_ranges_cover_output_body() {
        let mut blocks = VecDeque::new();
        // prompt 2, output 3, end 8 → hides rows 4..8 (5 lines).
        blocks.push_back(block(2, Some(3), Some(8), Some(0)));
        let mut folds = FoldState::default();
        folds.toggle(2);
        let ranges = build_fold_ranges(&blocks, &folds, 0, 24);
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0].start_vp, 4);
        assert_eq!(ranges[0].end_vp, 8);
        assert_eq!(ranges[0].hidden_lines, 4);
    }

    #[test]
    fn fold_ranges_empty_when_nothing_folded() {
        let mut blocks = VecDeque::new();
        blocks.push_back(block(2, Some(3), Some(8), Some(0)));
        let folds = FoldState::default();
        assert!(build_fold_ranges(&blocks, &folds, 0, 24).is_empty());
    }

    #[test]
    fn chrome_bands_span_whole_finished_block() {
        let mut blocks = VecDeque::new();
        // prompt 2, end 8 → band covers rows 2..=8 (inclusive of the D-mark row).
        blocks.push_back(block(2, Some(3), Some(8), Some(0)));
        let bands = build_chrome_bands(&blocks, 0, 24);
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].start_vp, 2);
        assert_eq!(bands[0].end_vp, 9);
        assert_eq!(bands[0].accent, color::success());
    }

    #[test]
    fn chrome_bands_skip_running_blocks_and_use_failure_accent() {
        let mut blocks = VecDeque::new();
        // Still running (no end row) → no band.
        blocks.push_back(block(0, Some(1), None, None));
        // Finished with a non-zero exit → failure accent.
        blocks.push_back(block(2, Some(3), Some(5), Some(1)));
        let bands = build_chrome_bands(&blocks, 0, 24);
        assert_eq!(bands.len(), 1, "the running block gets no band");
        assert_eq!(bands[0].accent, color::danger());
    }

    #[test]
    fn chrome_bands_clamp_to_viewport() {
        let mut blocks = VecDeque::new();
        // Block spans rows -3..=10; a display_offset of 5 puts its start above
        // the top of the viewport, so the band clamps to start at row 0.
        blocks.push_back(block(-3, Some(-2), Some(10), Some(0)));
        let bands = build_chrome_bands(&blocks, 5, 24);
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].start_vp, 0, "clamped at the top of the viewport");
        assert_eq!(bands[0].end_vp, 6); // (10 + 1 - 5)

        // A block extending past the bottom of a short viewport clamps there too.
        let mut blocks_bottom = VecDeque::new();
        blocks_bottom.push_back(block(20, Some(21), Some(30), Some(0)));
        let bands_bottom = build_chrome_bands(&blocks_bottom, 0, 24);
        assert_eq!(bands_bottom.len(), 1);
        assert_eq!(bands_bottom[0].start_vp, 20);
        assert_eq!(
            bands_bottom[0].end_vp, 24,
            "clamped at the bottom (rows=24)"
        );
    }

    #[test]
    fn block_at_abs_row_finds_containing_finished_block() {
        let mut blocks = VecDeque::new();
        blocks.push_back(block(2, Some(3), Some(8), Some(0)));
        blocks.push_back(block(10, Some(11), None, None)); // still running
        // Inside the first block's range.
        assert_eq!(block_at_abs_row(&blocks, 2), Some(2));
        assert_eq!(block_at_abs_row(&blocks, 5), Some(2));
        assert_eq!(block_at_abs_row(&blocks, 8), Some(2));
        // Just outside the range.
        assert_eq!(block_at_abs_row(&blocks, 9), None);
        // Inside the still-running block's rows: not finished, so no match.
        assert_eq!(block_at_abs_row(&blocks, 11), None);
    }

    #[test]
    fn output_selection_points_covers_output_body() {
        // prompt 2, output 3, end 8, no scroll, 40-col grid → selects rows 4..=8.
        let (start, end) = output_selection_points(3, 8, 0, 40).expect("some range");
        assert_eq!(start, Point::new(Line(4), Column(0)));
        assert_eq!(end, Point::new(Line(8), Column(39)));
    }

    #[test]
    fn output_selection_points_applies_display_offset_twice_when_scrolled() {
        // REGRESSION: the abs->Line conversion is two `- display_offset` steps
        // (abs->viewport, viewport->Line). With the user scrolled up 5 lines, an
        // output body at absolute rows 4..=8 must resolve to Lines shifted by
        // 2*5 = 10, not just 5 — otherwise a historical block's output selects the
        // wrong rows. start = 3 + 1 - 2*5 = -6; end = 8 - 2*5 = -2.
        let (start, end) = output_selection_points(3, 8, 5, 40).expect("some range");
        assert_eq!(start, Point::new(Line(-6), Column(0)));
        assert_eq!(end, Point::new(Line(-2), Column(39)));
    }

    #[test]
    fn output_selection_points_rejects_empty_or_zero_width() {
        // Empty output (command produced nothing between output_row and end_row).
        assert!(output_selection_points(5, 5, 0, 40).is_none());
        assert!(output_selection_points(6, 5, 0, 40).is_none());
        // Zero columns (no live grid) is never selectable.
        assert!(output_selection_points(3, 8, 0, 0).is_none());
    }
}
