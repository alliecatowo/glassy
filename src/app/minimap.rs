//! Scrollback minimap / overview strip (P2).
//!
//! A thin strip painted at the right edge of the terminal grid that gives a
//! downsampled colour-per-row overview of the ENTIRE buffer (the scrollback
//! history plus the visible screen). Clicking or dragging on the strip jumps the
//! viewport to the corresponding position in the scrollback.
//!
//! 0%-idle invariant: the per-line colour cache ([`MinimapCache`]) is maintained
//! INCREMENTALLY. The bulk of the buffer (scrollback history — potentially tens
//! of thousands of lines) is downsampled ONCE and cached; on each painting frame
//! only the at-most-`screen` lines currently on screen are re-downsampled, and
//! newly-streamed history lines are APPENDED (never a full re-scan). `render()`
//! is never called while the terminal is idle, so the strip costs nothing at
//! rest. The cache is bounded by the buffer's line count (one `[f32; 4]` per
//! line), so it cannot balloon RAM.

use super::*;
use alacritty_terminal::term::Term;
use alacritty_terminal::term::color::Colors;

/// Strip width in physical pixels. Wide enough to read as an overview rail
/// (an 8px sliver was effectively invisible).
const STRIP_W: f32 = 28.0;
/// Right-edge margin in physical pixels.
const STRIP_MARGIN: f32 = 2.0;
/// Alpha applied to the strip background (a dark rail behind the rows). Kept
/// high enough that the rail is clearly distinguishable from the terminal bg.
const STRIP_BG_ALPHA: f32 = 0.42;
/// Alpha applied to the viewport indicator (the brighter band marking the
/// currently-visible region within the whole buffer).
const VIEWPORT_ALPHA: f32 = 0.30;

/// An incrementally-maintained downsample of the scrollback: one representative
/// colour per buffer line, ordered top (oldest scrollback) to bottom (newest
/// visible row). Only dirty/new lines are recomputed each frame.
#[derive(Default)]
pub(crate) struct MinimapCache {
    /// One colour per buffer line, top-to-bottom. `colors[0]` is the topmost
    /// scrollback line; the last entry is the bottom visible row.
    colors: Vec<[f32; 4]>,
    /// The buffer dimensions the cache was last built against. When the total
    /// line count or column count changes, the cache is rebuilt wholesale.
    last_total: usize,
    last_cols: usize,
}

impl MinimapCache {
    /// Number of cached buffer lines.
    pub fn len(&self) -> usize {
        self.colors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.colors.is_empty()
    }

    /// Rebuild the cache incrementally from the terminal grid. The scrollback
    /// HISTORY (the bulk of the buffer — potentially tens of thousands of lines)
    /// is downsampled once and cached; only the at-most-`screen` lines currently
    /// ON SCREEN are re-downsampled per frame, since those are the only buffer
    /// lines that can change without a structural (resize / scroll / clear) event.
    /// `full` forces a wholesale rebuild (resize / scroll / theme change).
    ///
    /// This is what keeps the minimap off the 0%-idle critical path: when the
    /// terminal is idle `render()` is never called, and when it IS painting we
    /// touch only the visible window, not the whole history. Returns true if any
    /// cached colour changed (so the strip needs re-emitting).
    ///
    /// Reads under the already-held term lock in the render path.
    pub fn refresh<T>(
        &mut self,
        term: &Term<T>,
        colors: &Colors,
        display_offset: i32,
        full: bool,
    ) -> bool {
        let grid = term.grid();
        let cols = grid.columns();
        let hist = grid.history_size();
        let screen = grid.screen_lines();
        let total = hist + screen;
        if total == 0 || cols == 0 {
            let changed = !self.colors.is_empty();
            self.colors.clear();
            return changed;
        }

        // A column resize, history TRIM (total shrank), explicit full redraw, or an
        // empty cache all force a wholesale rebuild: line indices no longer line up
        // with the cache, so it must be rebuilt from scratch.
        let rebuild_all =
            full || self.last_cols != cols || self.colors.is_empty() || total < self.last_total;

        if rebuild_all {
            self.colors.clear();
            self.colors.reserve(total);
            // Buffer lines run Line(-hist) (top of scrollback) .. Line(screen-1)
            // (bottom visible). Push top-to-bottom.
            for i in 0..total {
                let line = Line(i as i32 - hist as i32);
                self.colors.push(line_color(term, line, cols, colors));
            }
            self.last_total = total;
            self.last_cols = cols;
            return true;
        }

        // History grew (streaming output pushed lines off the top of the screen
        // into scrollback): the existing cached colours stay valid (their content
        // is unchanged), so APPEND only the newly-added tail lines instead of
        // re-downsampling the whole buffer — the key to keeping streaming cheap.
        let mut changed = false;
        if total > self.last_total {
            let added = total - self.last_total;
            // The new lines occupy the bottom `added` buffer slots; downsample
            // exactly those (grid lines screen-added .. screen-1, plus any that
            // landed in history are equivalently the last `added` buffer indices).
            self.colors.reserve(added);
            for k in 0..added {
                let buf_idx = self.last_total + k; // new slot index
                let line = Line(buf_idx as i32 - hist as i32);
                self.colors.push(line_color(term, line, cols, colors));
            }
            self.last_total = total;
            changed = true;
        }

        // Incremental path: the visible window can have changed in place (cursor
        // moves, redraws) without a structural event. Visible screen row `r` shows
        // buffer line index `hist - display_offset + r`; re-downsample that window.
        for r in 0..screen {
            let idx = hist as i32 - display_offset + r as i32;
            if idx < 0 || idx as usize >= self.colors.len() {
                continue;
            }
            // The grid line for that visible row: r - display_offset (the same
            // translation the cell loop uses: point.line + display_offset == row).
            let line = Line(r as i32 - display_offset);
            let new = line_color(term, line, cols, colors);
            let slot = &mut self.colors[idx as usize];
            if *slot != new {
                *slot = new;
                changed = true;
            }
        }
        changed
    }
}

/// Downsample a single buffer line to one representative colour: the most
/// visually salient non-background foreground colour on the row, falling back to
/// the row's background when the line is blank. Cheap: one pass over the row's
/// columns, no allocation.
fn line_color<T>(term: &Term<T>, line: Line, cols: usize, colors: &Colors) -> [f32; 4] {
    let grid = term.grid();
    let bg = color::default_bg();
    // Accumulate the average foreground of non-blank cells; this reads as the
    // row's "ink density / colour" at a glance and downsamples smoothly.
    let mut sum = [0.0f32; 3];
    let mut n = 0u32;
    let row = &grid[line];
    for c in 0..cols {
        let cell = &row[Column(c)];
        let ch = cell.c;
        if ch == ' ' || ch == '\0' || cell.flags.contains(Flags::HIDDEN) {
            // A coloured (non-default) background still contributes — e.g. a
            // selected/highlighted block — but a plain blank does not.
            let cbg = color::resolve(cell.bg, colors);
            if cbg != bg {
                sum[0] += cbg[0];
                sum[1] += cbg[1];
                sum[2] += cbg[2];
                n += 1;
            }
            continue;
        }
        let fg = color::resolve(cell.fg, colors);
        sum[0] += fg[0];
        sum[1] += fg[1];
        sum[2] += fg[2];
        n += 1;
    }
    if n == 0 {
        // Blank line: a barely-visible tint so the strip still shows structure.
        return [bg[0], bg[1], bg[2], 1.0];
    }
    let inv = 1.0 / n as f32;
    [sum[0] * inv, sum[1] * inv, sum[2] * inv, 1.0]
}

impl App {
    /// Whether the minimap strip should be drawn this frame: enabled in config,
    /// not split (single-pane only), no full-screen modal up, and a PTY exists.
    pub(crate) fn minimap_active(&self) -> bool {
        self.config.minimap
            && !self.is_split()
            && self.pty.is_some()
            && !self.help_open
            && !self.settings_open
    }

    /// The strip's pixel rect for the current surface: `(x, y, w, h)`. The strip
    /// spans the terminal grid band (below the tab bar, above any status bar).
    pub(crate) fn minimap_rect(&self) -> Option<(f32, f32, f32, f32)> {
        let r = self.renderer.as_ref()?;
        let (sw, sh) = r.surface_size();
        if sw == 0 || sh == 0 {
            return None;
        }
        let m = r.cell_metrics();
        let top = tab_bar_h(m.height);
        let bottom = if self.config.status_bar {
            // Keep the strip clear of the status bar band (one cell tall).
            sh as f32 - m.height
        } else {
            sh as f32
        };
        let h = (bottom - top).max(0.0);
        if h <= 0.0 {
            return None;
        }
        let x = sw as f32 - STRIP_W - STRIP_MARGIN;
        Some((x, top, STRIP_W, h))
    }

    /// Emit the minimap strip's overlay quads for this frame into `renderer`: the
    /// rail background, the downsampled per-line colour bars, and the viewport
    /// indicator band. Reads only the cached colours (no term lock). `rect` is the
    /// strip rect from [`App::minimap_rect`]; `display_offset` positions the
    /// viewport indicator.
    ///
    /// An associated function (not `&mut self`) so the render path can call it
    /// while it already holds `&mut self.renderer` / `&self.pty` borrows.
    pub(crate) fn paint_minimap(
        renderer: &mut Renderer,
        cache: &MinimapCache,
        rect: (f32, f32, f32, f32),
        display_offset: i32,
        screen_lines: usize,
    ) {
        let (x, y, w, h) = rect;
        if cache.is_empty() || h <= 0.0 {
            return;
        }
        let total = cache.len();

        // Faint rail behind the rows so the strip reads as a defined region.
        let bg = color::default_fg();
        renderer.push_overlay_px(x, y, w, h, [bg[0], bg[1], bg[2], STRIP_BG_ALPHA * 0.4]);

        // Each buffer line maps to a `h/total`-tall slice. When there are more
        // lines than pixels (the common case for deep scrollback), several lines
        // collapse onto one pixel row; we accumulate them so no line is dropped.
        let per_line = h / total as f32;
        if per_line >= 1.0 {
            // Enough vertical room: one quad per line.
            for (i, c) in cache.colors.iter().enumerate() {
                let ry = y + i as f32 * per_line;
                let rh = per_line.max(1.0);
                renderer.push_overlay_px(x, ry, w, rh, [c[0], c[1], c[2], 0.85]);
            }
        } else {
            // More lines than pixels: bucket lines into pixel rows, averaging.
            let px_rows = h.floor().max(1.0) as usize;
            for p in 0..px_rows {
                let lo = p * total / px_rows;
                let hi = ((p + 1) * total / px_rows).max(lo + 1).min(total);
                let mut acc = [0.0f32; 3];
                let mut n = 0u32;
                for c in &cache.colors[lo..hi] {
                    acc[0] += c[0];
                    acc[1] += c[1];
                    acc[2] += c[2];
                    n += 1;
                }
                if n == 0 {
                    continue;
                }
                let inv = 1.0 / n as f32;
                let ry = y + p as f32;
                renderer.push_overlay_px(
                    x,
                    ry,
                    w,
                    1.0,
                    [acc[0] * inv, acc[1] * inv, acc[2] * inv, 0.85],
                );
            }
        }

        // Viewport indicator: a brighter band marking the visible region inside
        // the whole buffer. The visible window is `screen_lines` of `total`,
        // positioned by the display offset (offset 0 = bottom).
        let hist = total.saturating_sub(screen_lines);
        let top_line = (hist as i32 - display_offset).max(0) as f32;
        let band_y = y + (top_line / total as f32) * h;
        let band_h = ((screen_lines as f32 / total as f32) * h).max(3.0).min(h);
        let band_y = band_y.min(y + h - band_h).max(y);
        let acc = color::accent();
        renderer.push_overlay_px(
            x,
            band_y,
            w,
            band_h,
            [acc[0], acc[1], acc[2], VIEWPORT_ALPHA],
        );
        // Thin bright edge on the inner side of the band for definition.
        renderer.push_overlay_px(x, band_y, 1.5, band_h, [acc[0], acc[1], acc[2], 0.7]);
    }

    /// Hit-test a pixel position against the minimap strip. Returns true when the
    /// pointer is over the strip rect (so the click is consumed as a jump).
    pub(crate) fn minimap_hit(&self, px: f64, py: f64) -> bool {
        let Some((x, y, w, h)) = self.minimap_rect() else {
            return false;
        };
        let px = px as f32;
        let py = py as f32;
        px >= x && px < x + w && py >= y && py < y + h
    }

    /// Jump the scrollback viewport so the buffer line under pixel-y `py` is
    /// centered in the visible window. Used for click + drag on the strip.
    pub(crate) fn minimap_jump_to(&mut self, py: f64, event_loop: &ActiveEventLoop) {
        let Some((_, y, _, h)) = self.minimap_rect() else {
            return;
        };
        let Some(pty) = self.pty.as_ref() else {
            return;
        };
        // Hold the lock once: read dimensions + current offset, compute the
        // absolute target offset, and apply it as a single relative delta.
        {
            let mut t = pty.term.lock();
            let (hist, screen, cur) = {
                let g = t.grid();
                (
                    g.history_size(),
                    g.screen_lines(),
                    g.display_offset() as i32,
                )
            };
            if hist == 0 {
                return;
            }
            let total = hist + screen;
            // Fraction down the strip the pointer landed at.
            let frac = (((py as f32) - y) / h).clamp(0.0, 1.0);
            // Target buffer line at that fraction, then center the visible window
            // on it: the top visible line should be `target - screen/2`.
            let target = (frac * total as f32) as i32;
            let top_line = (target - screen as i32 / 2).clamp(0, hist as i32);
            // display_offset 0 = bottom; offset = hist - top_line.
            let offset = (hist as i32 - top_line).clamp(0, hist as i32);
            let delta = offset - cur;
            if delta != 0 {
                t.scroll_display(Scroll::Delta(delta));
            }
        }
        self.force_full_redraw = true;
        self.mark_dirty(event_loop);
    }

    /// Toggle the minimap on/off at runtime (settings toggle / palette / key).
    pub(crate) fn toggle_minimap(&mut self) {
        self.config.minimap = !self.config.minimap;
        // Drop the cache so a fresh full rebuild happens on the next paint.
        self.minimap_cache = MinimapCache::default();
        self.force_full_redraw = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_starts_empty() {
        let c = MinimapCache::default();
        assert!(c.is_empty());
        assert_eq!(c.len(), 0);
    }

    #[test]
    fn viewport_band_math_stays_in_strip() {
        // Simulate the band math from paint_minimap for a deep buffer.
        let total = 1000usize;
        let screen = 40usize;
        let hist = total - screen;
        let h = 600.0f32;
        let y = 10.0f32;
        for &offset in &[0i32, 100, 500, hist as i32] {
            let top_line = (hist as i32 - offset).max(0) as f32;
            let band_y = y + (top_line / total as f32) * h;
            let band_h = ((screen as f32 / total as f32) * h).max(3.0).min(h);
            let band_y = band_y.min(y + h - band_h).max(y);
            assert!(band_y >= y, "band above strip for offset {offset}");
            assert!(
                band_y + band_h <= y + h + 0.01,
                "band below strip for offset {offset}: {} > {}",
                band_y + band_h,
                y + h
            );
        }
    }

    #[test]
    fn jump_fraction_maps_to_valid_offset() {
        // The offset derived from a click fraction must stay within [0, hist].
        let hist = 900i32;
        let screen = 40i32;
        let total = (hist + screen) as f32;
        for &frac in &[0.0f32, 0.25, 0.5, 0.75, 1.0] {
            let target = (frac * total) as i32;
            let top_line = (target - screen / 2).clamp(0, hist);
            let offset = (hist - top_line).clamp(0, hist);
            assert!((0..=hist).contains(&offset), "offset {offset} out of range");
        }
    }
}
