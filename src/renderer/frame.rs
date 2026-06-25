//! Frame rendering: draw_block, draw_box, render(), record_passes.

use super::*;

impl Renderer {
    /// Paint a block element (U+2580..=U+259F) as exact solid foreground
    /// rectangles. All coordinates are rounded to whole pixels for crisp edges.
    ///
    /// `bg` is needed for the shade glyphs (U+2591..=U+2593): the background pass
    /// is created with `blend: None`, so an instance's alpha channel is written
    /// straight to the (opaque) surface and never composited. We therefore cannot
    /// express a shade as "fg at reduced alpha"; instead we pre-blend fg over bg
    /// by the shade coverage on the CPU and emit a fully-opaque solid color.
    pub(crate) fn draw_block(&mut self, ch: char, ox: f32, oy: f32, fg: [f32; 4], bg: [f32; 4]) {
        let cw = self.metrics.width;
        let chh = self.metrics.height;
        let l = ox.round();
        let t = oy.round();
        let r = (ox + cw).round();
        let b = (oy + chh).round();
        let w = r - l;
        let h = b - t;
        let cp = ch as u32;

        // Pre-blend fg over bg by `cov` coverage, producing an opaque color.
        // Used for the shade glyphs since the bg pass has blending disabled.
        fn shade(fg: [f32; 4], bg: [f32; 4], cov: f32) -> [f32; 4] {
            [
                bg[0] + (fg[0] - bg[0]) * cov,
                bg[1] + (fg[1] - bg[1]) * cov,
                bg[2] + (fg[2] - bg[2]) * cov,
                1.0,
            ]
        }

        // Helper closures expressed inline (no borrow of self) to compute the
        // fractional sub-rectangles, then pushed via push_solid.
        match cp {
            // Full block.
            0x2588 => self.push_solid(l, t, w, h, fg),
            // Upper half.
            0x2580 => {
                let mid = (oy + chh / 2.0).round();
                self.push_solid(l, t, w, mid - t, fg);
            }
            // Lower half.
            0x2584 => {
                let mid = (oy + chh / 2.0).round();
                self.push_solid(l, mid, w, b - mid, fg);
            }
            // Left half.
            0x258C => {
                let mid = (ox + cw / 2.0).round();
                self.push_solid(l, t, mid - l, h, fg);
            }
            // Right half.
            0x2590 => {
                let mid = (ox + cw / 2.0).round();
                self.push_solid(mid, t, r - mid, h, fg);
            }
            // Lower one-eighth through seven-eighths (U+2581..=U+2587).
            0x2581..=0x2587 => {
                let eighths = (cp - 0x2580) as f32; // 1..=7
                let top = (oy + chh * (1.0 - eighths / 8.0)).round();
                self.push_solid(l, top, w, b - top, fg);
            }
            // Left seven-eighths down to one-eighth (U+2589..=U+258F).
            0x2589..=0x258F => {
                // U+2589 = 7/8, U+258A = 6/8, ... U+258F = 1/8.
                let eighths = (8 - (cp - 0x2588)) as f32; // 7..=1
                let right = (ox + cw * (eighths / 8.0)).round();
                self.push_solid(l, t, right - l, h, fg);
            }
            // Light/medium/dark shades. The bg pass does not blend, so we mix fg
            // over bg by the shade coverage here and emit an opaque solid.
            0x2591 => self.push_solid(l, t, w, h, shade(fg, bg, 0.25)),
            0x2592 => self.push_solid(l, t, w, h, shade(fg, bg, 0.5)),
            0x2593 => self.push_solid(l, t, w, h, shade(fg, bg, 0.75)),
            // Quadrants (U+2596..=U+259F). Bit layout per quadrant:
            //   TL, TR, BL, BR. Each code point selects a subset.
            0x2596..=0x259F => {
                let mx = (ox + cw / 2.0).round();
                let my = (oy + chh / 2.0).round();
                let (tl, tr, bl, br) = match cp {
                    0x2596 => (false, false, true, false), // lower left
                    0x2597 => (false, false, false, true), // lower right
                    0x2598 => (true, false, false, false), // upper left
                    0x2599 => (true, false, true, true),   // UL+LL+LR
                    0x259A => (true, false, false, true),  // UL + LR
                    0x259B => (true, true, true, false),   // UL+UR+LL
                    0x259C => (true, true, false, true),   // UL+UR+LR
                    0x259D => (false, true, false, false), // upper right
                    0x259E => (false, true, true, false),  // UR + LL
                    0x259F => (false, true, true, true),   // UR+LL+LR
                    _ => (false, false, false, false),
                };
                if tl {
                    self.push_solid(l, t, mx - l, my - t, fg);
                }
                if tr {
                    self.push_solid(mx, t, r - mx, my - t, fg);
                }
                if bl {
                    self.push_solid(l, my, mx - l, b - my, fg);
                }
                if br {
                    self.push_solid(mx, my, r - mx, b - my, fg);
                }
            }
            // Any unhandled block code point: fill the cell so nothing is blank.
            _ => self.push_solid(l, t, w, h, fg),
        }
    }

    /// Paint a box-drawing character (U+2500..=U+257F) as solid foreground
    /// rectangles spanning the full cell so adjacent cells join seamlessly.
    /// Returns `true` if the code point was handled procedurally; `false` if the
    /// caller should fall back to the normal glyph path.
    pub(crate) fn draw_box(&mut self, ch: char, ox: f32, oy: f32, fg: [f32; 4]) -> bool {
        let cw = self.metrics.width;
        let chh = self.metrics.height;
        let thin = (chh / 14.0).round().max(1.0);
        let heavy = (thin * 2.0).round().max(2.0);
        // Center of the cell, rounded so the cross lands on whole pixels.
        let cx = (ox + cw / 2.0).round();
        let cy = (oy + chh / 2.0).round();
        // Cell edges (rounded so neighboring cells share an exact boundary).
        let left = ox.round();
        let right = (ox + cw).round();
        let top = oy.round();
        let bot = (oy + chh).round();

        // Arm weights: 0 = absent, 1 = light, 2 = heavy, 3 = double.
        const A: u8 = 0; // absent
        const L: u8 = 1; // light
        const H: u8 = 2; // heavy
        const D: u8 = 3; // double

        // Double rails sit symmetrically about the center line, each rail offset
        // by `rail` (= light thickness + 1px) from center, and each rail is
        // `thin` thick. So the near rail center is at `c - rail` and the far rail
        // center at `c + rail`. These coordinates are identical for horizontal
        // and vertical doubling, so straight doubles (═ ║) connect across cells.
        let rail = thin + 1.0;
        // Top edges of the two horizontal rails (centered on cy) and left edges
        // of the two vertical rails (centered on cx), rounded to whole pixels.
        let hy_near = (cy - rail - thin / 2.0).round(); // upper rail top
        let hy_far = (cy + rail - thin / 2.0).round(); // lower rail top
        let vx_near = (cx - rail - thin / 2.0).round(); // left rail left
        let vx_far = (cx + rail - thin / 2.0).round(); // right rail left
        // The outer extents of the double band: where the far rail of the
        // perpendicular axis ends. Used so double corners/junctions close.
        let h_outer_lo = hy_near; // top of the horizontal band
        let h_outer_hi = (hy_far + thin).round(); // bottom of the horizontal band
        let v_outer_lo = vx_near; // left of the vertical band
        let v_outer_hi = (vx_far + thin).round(); // right of the vertical band

        // Single-line (light/heavy) arm helpers. A light/heavy arm spans the
        // full half-cell from the edge to the center, centered on the cross, so
        // neighbours join. Heavy is identical layout, only thicker. Each closure
        // takes `this: &mut Self` explicitly so they don't borrow-conflict.
        let harm = |this: &mut Self, dir_left: bool, weight: u8| {
            if weight != L && weight != H {
                return;
            }
            let th = if weight == H { heavy } else { thin };
            let y = (cy - th / 2.0).round();
            if dir_left {
                this.push_solid(left, y, cx - left, th, fg);
            } else {
                this.push_solid(cx, y, right - cx, th, fg);
            }
        };
        let varm = |this: &mut Self, dir_up: bool, weight: u8| {
            if weight != L && weight != H {
                return;
            }
            let th = if weight == H { heavy } else { thin };
            let x = (cx - th / 2.0).round();
            if dir_up {
                this.push_solid(x, top, th, cy - top, fg);
            } else {
                this.push_solid(x, cy, th, bot - cy, fg);
            }
        };

        // Decode the code point into four arm weights (left, right, up, down).
        // `None` => not a simple-arm glyph; handled by the specials block below.
        let cp = ch as u32;
        let arms: Option<(u8, u8, u8, u8)> = match cp {
            // Straight lines.
            0x2500 => Some((L, L, A, A)), // light horizontal
            0x2501 => Some((H, H, A, A)), // heavy horizontal
            0x2502 => Some((A, A, L, L)), // light vertical
            0x2503 => Some((A, A, H, H)), // heavy vertical

            // Corners (light). U+250C down+right, etc.
            0x250C => Some((A, L, A, L)), // down and right
            0x250D => Some((A, H, A, L)),
            0x250E => Some((A, L, A, H)),
            0x250F => Some((A, H, A, H)), // heavy down and right
            0x2510 => Some((L, A, A, L)), // down and left
            0x2511 => Some((H, A, A, L)),
            0x2512 => Some((L, A, A, H)),
            0x2513 => Some((H, A, A, H)),
            0x2514 => Some((A, L, L, A)), // up and right
            0x2515 => Some((A, H, L, A)),
            0x2516 => Some((A, L, H, A)),
            0x2517 => Some((A, H, H, A)),
            0x2518 => Some((L, A, L, A)), // up and left
            0x2519 => Some((H, A, L, A)),
            0x251A => Some((L, A, H, A)),
            0x251B => Some((H, A, H, A)),

            // Vertical + right (T pointing right) U+251C..U+2523.
            0x251C => Some((A, L, L, L)),
            0x251D => Some((A, H, L, L)),
            0x251E => Some((A, L, H, L)),
            0x251F => Some((A, L, L, H)),
            0x2520 => Some((A, L, H, H)),
            0x2521 => Some((A, H, H, L)),
            0x2522 => Some((A, H, L, H)),
            0x2523 => Some((A, H, H, H)),

            // Vertical + left (T pointing left) U+2524..U+252B.
            0x2524 => Some((L, A, L, L)),
            0x2525 => Some((H, A, L, L)),
            0x2526 => Some((L, A, H, L)),
            0x2527 => Some((L, A, L, H)),
            0x2528 => Some((L, A, H, H)),
            0x2529 => Some((H, A, H, L)),
            0x252A => Some((H, A, L, H)),
            0x252B => Some((H, A, H, H)),

            // Horizontal + down (T pointing down) U+252C..U+2533.
            0x252C => Some((L, L, A, L)),
            0x252D => Some((H, L, A, L)),
            0x252E => Some((L, H, A, L)),
            0x252F => Some((H, H, A, L)),
            0x2530 => Some((L, L, A, H)),
            0x2531 => Some((H, L, A, H)),
            0x2532 => Some((L, H, A, H)),
            0x2533 => Some((H, H, A, H)),

            // Horizontal + up (T pointing up) U+2534..U+253B.
            0x2534 => Some((L, L, L, A)),
            0x2535 => Some((H, L, L, A)),
            0x2536 => Some((L, H, L, A)),
            0x2537 => Some((H, H, L, A)),
            0x2538 => Some((L, L, H, A)),
            0x2539 => Some((H, L, H, A)),
            0x253A => Some((L, H, H, A)),
            0x253B => Some((H, H, H, A)),

            // Crosses U+253C..U+254B.
            0x253C => Some((L, L, L, L)),
            0x253D => Some((H, L, L, L)),
            0x253E => Some((L, H, L, L)),
            0x253F => Some((H, H, L, L)),
            0x2540 => Some((L, L, H, L)),
            0x2541 => Some((L, L, L, H)),
            0x2542 => Some((L, L, H, H)),
            0x2543 => Some((H, L, H, L)),
            0x2544 => Some((L, H, H, L)),
            0x2545 => Some((H, L, L, H)),
            0x2546 => Some((L, H, L, H)),
            0x2547 => Some((H, H, H, L)),
            0x2548 => Some((H, H, L, H)),
            0x2549 => Some((H, L, H, H)),
            0x254A => Some((L, H, H, H)),
            0x254B => Some((H, H, H, H)),

            // Rounded corners — same arms as the sharp corners; we approximate
            // the curve with square joins (visually fine at terminal sizes).
            0x256D => Some((A, L, A, L)), // arc down and right
            0x256E => Some((L, A, A, L)), // arc down and left
            0x256F => Some((L, A, L, A)), // arc up and left
            0x2570 => Some((A, L, L, A)), // arc up and right

            // Half lines (single weight). U+2574 left, U+2575 up, U+2576 right,
            // U+2577 down (light); U+2578..U+257B the same, heavy.
            0x2574 => Some((L, A, A, A)),
            0x2575 => Some((A, A, L, A)),
            0x2576 => Some((A, L, A, A)),
            0x2577 => Some((A, A, A, L)),
            0x2578 => Some((H, A, A, A)),
            0x2579 => Some((A, A, H, A)),
            0x257A => Some((A, H, A, A)),
            0x257B => Some((A, A, A, H)),

            // Mixed-weight straight lines.
            0x257C => Some((L, H, A, A)), // left light, right heavy
            0x257D => Some((A, A, L, H)), // up light, down heavy
            0x257E => Some((H, L, A, A)), // left heavy, right light
            0x257F => Some((A, A, H, L)), // up heavy, down light

            _ => None,
        };

        if let Some((al, ar, au, ad)) = arms {
            harm(self, true, al);
            harm(self, false, ar);
            varm(self, true, au);
            varm(self, false, ad);
            return true;
        }

        // --- Doubles --------------------------------------------------------
        // Decode the double-line set (U+2550..U+256C) into the same four-arm
        // model, where each arm is Absent / Light (single) / Double. Every
        // double arm is rendered as two thin rails at the fixed `vx_*`/`hy_*`
        // offsets, so straight doubles connect across cells and corners close.
        let darms: Option<(u8, u8, u8, u8)> = match cp {
            0x2550 => Some((D, D, A, A)), // ═ double horizontal
            0x2551 => Some((A, A, D, D)), // ║ double vertical
            0x2552 => Some((A, D, A, L)), // ╒ right double, down single
            0x2553 => Some((A, L, A, D)), // ╓ right single, down double
            0x2554 => Some((A, D, A, D)), // ╔ double down and right
            0x2555 => Some((D, A, A, L)), // ╕ left double, down single
            0x2556 => Some((L, A, A, D)), // ╖ left single, down double
            0x2557 => Some((D, A, A, D)), // ╗ double down and left
            0x2558 => Some((A, D, L, A)), // ╘ right double, up single
            0x2559 => Some((A, L, D, A)), // ╙ right single, up double
            0x255A => Some((A, D, D, A)), // ╚ double up and right
            0x255B => Some((D, A, L, A)), // ╛ left double, up single
            0x255C => Some((L, A, D, A)), // ╜ left single, up double
            0x255D => Some((D, A, D, A)), // ╝ double up and left
            0x255E => Some((A, D, L, L)), // ╞ vertical single, right double
            0x255F => Some((A, L, D, D)), // ╟ vertical double, right single
            0x2560 => Some((A, D, D, D)), // ╠ vertical double, right double
            0x2561 => Some((D, A, L, L)), // ╡ vertical single, left double
            0x2562 => Some((L, A, D, D)), // ╢ vertical double, left single
            0x2563 => Some((D, A, D, D)), // ╣ vertical double, left double
            0x2564 => Some((D, D, A, L)), // ╤ horizontal double, down single
            0x2565 => Some((L, L, A, D)), // ╥ horizontal single, down double
            0x2566 => Some((D, D, A, D)), // ╦ double down and horizontal
            0x2567 => Some((D, D, L, A)), // ╧ horizontal double, up single
            0x2568 => Some((L, L, D, A)), // ╨ horizontal single, up double
            0x2569 => Some((D, D, D, A)), // ╩ double up and horizontal
            0x256A => Some((D, D, L, L)), // ╪ vertical single, horizontal double
            0x256B => Some((L, L, D, D)), // ╫ vertical double, horizontal single
            0x256C => Some((D, D, D, D)), // ╬ double vertical and horizontal
            _ => None,
        };

        if let Some((al, ar, au, ad)) = darms {
            let h_double = al == D || ar == D;
            let v_double = au == D || ad == D;
            // Inner edges of the perpendicular band's rails (used to mitre).
            let vx_near_in = (vx_near + thin).round(); // right edge of left rail
            let hy_near_in = (hy_near + thin).round(); // bottom edge of top rail
            // The four pure double corners are drawn explicitly below as clean
            // outer/inner L-joins; skip the generic rail spans for them so the
            // inner notch stays open (the canonical ╔ ╗ ╚ ╝ look).
            let pure_corner = matches!(cp, 0x2554 | 0x2557 | 0x255A | 0x255D);

            // Each doubled axis is rendered as two parallel `thin` rails at the
            // fixed `hy_*`/`vx_*` offsets. The rail ENDPOINTS toward the center
            // are mitred so the band corners close: an "outer" rail wraps around
            // to its perpendicular outer rail, the "inner" rail makes the small
            // inner corner. Endpoints are chosen so straight doubles span the
            // full cell (connecting across cells) and corners/junctions close.

            // --- Horizontal rails (upper = hy_near, lower = hy_far). ---
            // Upper rail x-extent.
            let up_lo = if al == D {
                left
            } else if v_double {
                // No left arm: upper rail starts at the left vertical rail. It is
                // the outer rail for an up-and-right opening (╚/╠ etc.), inner for
                // a down-and-right opening (╔). Meet the near (left) vertical rail.
                vx_near
            } else {
                cx
            };
            let up_hi = if ar == D {
                right
            } else if v_double {
                vx_near_in
            } else {
                cx
            };
            // Lower rail x-extent.
            let lo_lo = if al == D {
                left
            } else if v_double {
                vx_near
            } else {
                cx
            };
            let lo_hi = if ar == D {
                right
            } else if v_double {
                vx_near_in
            } else {
                cx
            };
            if h_double && !pure_corner {
                self.push_solid(up_lo, hy_near, up_hi - up_lo, thin, fg);
                self.push_solid(lo_lo, hy_far, lo_hi - lo_lo, thin, fg);
            }

            // --- Vertical rails (left = vx_near, right = vx_far). ---
            let lf_lo = if au == D {
                top
            } else if h_double {
                hy_near
            } else {
                cy
            };
            let lf_hi = if ad == D {
                bot
            } else if h_double {
                hy_near_in
            } else {
                cy
            };
            let rt_lo = if au == D {
                top
            } else if h_double {
                hy_near
            } else {
                cy
            };
            let rt_hi = if ad == D {
                bot
            } else if h_double {
                hy_near_in
            } else {
                cy
            };
            if v_double && !pure_corner {
                self.push_solid(vx_near, lf_lo, thin, lf_hi - lf_lo, fg);
                self.push_solid(vx_far, rt_lo, thin, rt_hi - rt_lo, fg);
            }

            // --- Pure double corners: redraw the two rails as clean L-joins so
            // the outer/inner mitre is exact (overrides the generic spans above
            // only where it improves the join; the extra solids are harmless). ---
            match cp {
                0x2554 => {
                    // ╔ down+right: outer = top rail + left vrail; inner = bottom
                    // rail + right vrail.
                    self.push_solid(vx_near, hy_near, right - vx_near, thin, fg);
                    self.push_solid(vx_near, hy_near, thin, bot - hy_near, fg);
                    self.push_solid(vx_far, hy_far, right - vx_far, thin, fg);
                    self.push_solid(vx_far, hy_far, thin, bot - hy_far, fg);
                }
                0x2557 => {
                    // ╗ down+left.
                    self.push_solid(left, hy_near, vx_far + thin - left, thin, fg);
                    self.push_solid(vx_far, hy_near, thin, bot - hy_near, fg);
                    self.push_solid(left, hy_far, vx_near + thin - left, thin, fg);
                    self.push_solid(vx_near, hy_far, thin, bot - hy_far, fg);
                }
                0x255A => {
                    // ╚ up+right.
                    self.push_solid(vx_near, hy_far, right - vx_near, thin, fg);
                    self.push_solid(vx_near, top, thin, hy_far + thin - top, fg);
                    self.push_solid(vx_far, hy_near, right - vx_far, thin, fg);
                    self.push_solid(vx_far, top, thin, hy_near + thin - top, fg);
                }
                0x255D => {
                    // ╝ up+left.
                    self.push_solid(left, hy_far, vx_far + thin - left, thin, fg);
                    self.push_solid(vx_far, top, thin, hy_far + thin - top, fg);
                    self.push_solid(left, hy_near, vx_near + thin - left, thin, fg);
                    self.push_solid(vx_near, top, thin, hy_near + thin - top, fg);
                }
                _ => {}
            }

            // --- Single (light) arms crossing a doubled perpendicular band.
            // Draw a centered thin line that bridges the band so the single arm
            // connects to neighbours. ---
            if al == L {
                let y = (cy - thin / 2.0).round();
                let end = if v_double { v_outer_hi } else { cx };
                self.push_solid(left, y, end - left, thin, fg);
            }
            if ar == L {
                let y = (cy - thin / 2.0).round();
                let start = if v_double { v_outer_lo } else { cx };
                self.push_solid(start, y, right - start, thin, fg);
            }
            if au == L {
                let x = (cx - thin / 2.0).round();
                let end = if h_double { h_outer_hi } else { cy };
                self.push_solid(x, top, thin, end - top, fg);
            }
            if ad == L {
                let x = (cx - thin / 2.0).round();
                let start = if h_double { h_outer_lo } else { cy };
                self.push_solid(x, start, thin, bot - start, fg);
            }
            return true;
        }

        // --- Specials -------------------------------------------------------
        match cp {
            // Dashed horizontals: light/heavy double/triple/quadruple dash.
            // U+2504/2505 triple-dash, U+2508/2509 quadruple-dash (horizontal).
            0x2504 | 0x2508 => {
                self.draw_dashed_h(ox, cy, fg, thin, if cp == 0x2504 { 3 } else { 4 });
                true
            }
            0x2505 | 0x2509 => {
                self.draw_dashed_h(ox, cy, fg, heavy, if cp == 0x2505 { 3 } else { 4 });
                true
            }
            // U+2506/2507 triple-dash, U+250A/250B quadruple-dash (vertical).
            0x2506 | 0x250A => {
                self.draw_dashed_v(oy, cx, fg, thin, if cp == 0x2506 { 3 } else { 4 });
                true
            }
            0x2507 | 0x250B => {
                self.draw_dashed_v(oy, cx, fg, heavy, if cp == 0x2507 { 3 } else { 4 });
                true
            }
            // Diagonals U+2571..U+2573: hard to do well with axis-aligned
            // rectangles; fall back to the glyph path.
            0x2571..=0x2573 => false,
            // Anything else in the box range we don't explicitly handle: fall
            // back to the glyph so it never renders blank.
            _ => false,
        }
    }

    /// Draw a dashed horizontal line of the given thickness centered on `cy`,
    /// broken into `segments` dashes with gaps.
    pub(crate) fn draw_dashed_h(&mut self, ox: f32, cy: f32, fg: [f32; 4], th: f32, segments: u32) {
        let cw = self.metrics.width;
        let left = ox.round();
        let y = (cy - th / 2.0).round();
        let n = segments as f32;
        // Dash occupies ~70% of each slot, gap the rest.
        let slot = cw / n;
        let dash = (slot * 0.7).round().max(1.0);
        for i in 0..segments {
            let x = (left + slot * i as f32).round();
            self.push_solid(x, y, dash, th, fg);
        }
    }

    /// Draw a dashed vertical line of the given thickness centered on `cx`.
    pub(crate) fn draw_dashed_v(&mut self, oy: f32, cx: f32, fg: [f32; 4], th: f32, segments: u32) {
        let chh = self.metrics.height;
        let top = oy.round();
        let x = (cx - th / 2.0).round();
        let n = segments as f32;
        let slot = chh / n;
        let dash = (slot * 0.7).round().max(1.0);
        for i in 0..segments {
            let y = (top + slot * i as f32).round();
            self.push_solid(x, y, th, dash, fg);
        }
    }

    /// Paint a Powerline separator glyph procedurally as a series of scanline
    /// quads, without requiring a Powerline-patched font.
    ///
    /// Handled code points (Private Use Area, Powerline Extra symbols):
    ///   U+E0B0  filled right-pointing triangle (→ separator arrow, solid)
    ///   U+E0B1  right-pointing chevron outline (→ separator, stroke only)
    ///   U+E0B2  filled left-pointing triangle (← separator arrow, solid)
    ///   U+E0B3  left-pointing chevron outline (← separator, stroke only)
    ///
    /// Returns `true` when the code point was handled; `false` otherwise (the
    /// caller falls through to the normal glyph path).
    ///
    /// Rendering strategy: approximate the diagonal edge with integer-snapped
    /// horizontal scanline quads (one per pixel row). At typical terminal cell
    /// sizes (8–20 px tall) the staircase is indistinguishable from a true
    /// anti-aliased line; the fills are exact, producing sharp, gapless
    /// separators that perfectly match adjacent solid-colored backgrounds.
    ///
    /// The `bg` color is used to "erase" the triangle's complementary region for
    /// the solid variants (E0B0/E0B2), so the opposite half of the cell shows the
    /// cell background rather than the foreground color.
    pub(crate) fn draw_powerline(
        &mut self,
        ch: char,
        ox: f32,
        oy: f32,
        fg: [f32; 4],
        bg: [f32; 4],
    ) -> bool {
        let cw = self.metrics.width;
        let chh = self.metrics.height;
        let cp = ch as u32;

        // Stroke thickness for the chevron outline variants.
        let stroke = (chh / 12.0).round().max(1.0);

        // Integer-pixel cell boundaries.
        let left = ox.round() as i32;
        let top = oy.round() as i32;
        let right = (ox + cw).round() as i32;
        let bot = (oy + chh).round() as i32;
        let cell_w = (right - left).max(1);
        let cell_h = (bot - top).max(1);

        // Helper: push a single scanline row quad clamped inside the cell.
        // `x0`..`x1` and `y0`..`y1` are integer pixel coords.
        let push = |this: &mut Self, x0: i32, x1: i32, y0: i32, y1: i32, color: [f32; 4]| {
            let x0 = x0.max(left).min(right);
            let x1 = x1.max(left).min(right);
            let y0 = y0.max(top).min(bot);
            let y1 = y1.max(top).min(bot);
            if x1 > x0 && y1 > y0 {
                this.push_solid(
                    x0 as f32,
                    y0 as f32,
                    (x1 - x0) as f32,
                    (y1 - y0) as f32,
                    color,
                );
            }
        };

        match cp {
            // E0B0: right-pointing solid triangle.
            // Row `r` (0-indexed from top): the filled fg region occupies columns
            //   [left, left + (r+1)*cw/cell_h) on the upper half
            //   (mirrored on the lower half via row distance from centre).
            // Equivalently, for row r the fg x-extent is:
            //   x_edge = left + round( (r + 0.5) * cell_w / cell_h )
            // The "upper half" rows converge toward the right tip; the "lower half"
            // mirrors.  But a right-pointing triangle is simply: for row r (0..h),
            //   fill [left, left + round((r + 0.5) * cw / h)) for rows above the
            //   midpoint, and [left, left + round((h - r - 0.5) * cw / h)) for below.
            // This simplifies: x_edge = left + round( min(r+1, h-r) * cw / h ).
            // Actually the standard Powerline right arrow is:
            //   for row y in [0, h): fill x in [left, left + round((y+0.5)*cw/h)]
            //   for the top half, and mirror for the bottom half.
            // The cleanest description: it's a right triangle with vertices at
            //   (left, top), (left, bot), (right, mid).
            // For each row r: the rightmost pixel is interpolated from left→right
            // as r goes top→mid, then right→left as r goes mid→bot.
            0xE0B0 => {
                for row in 0..cell_h {
                    let y0 = top + row;
                    let y1 = y0 + 1;
                    // Fraction along the left→right edge (0 at top or bot, 1 at mid).
                    let dist_from_mid = (row as f32 - (cell_h as f32 - 1.0) / 2.0).abs();
                    let frac = 1.0 - dist_from_mid / ((cell_h as f32) / 2.0);
                    let x_edge = left + (frac * cell_w as f32).round() as i32;
                    // Fill fg region [left, x_edge).
                    push(self, left, x_edge.min(right), y0, y1, fg);
                    // Fill bg region [x_edge, right) so the full cell is painted.
                    push(self, x_edge.min(right), right, y0, y1, bg);
                }
                true
            }

            // E0B2: left-pointing solid triangle.
            // Mirror of E0B0: vertices at (right, top), (right, bot), (left, mid).
            0xE0B2 => {
                for row in 0..cell_h {
                    let y0 = top + row;
                    let y1 = y0 + 1;
                    let dist_from_mid = (row as f32 - (cell_h as f32 - 1.0) / 2.0).abs();
                    let frac = 1.0 - dist_from_mid / ((cell_h as f32) / 2.0);
                    let x_edge = right - (frac * cell_w as f32).round() as i32;
                    // Fill bg region [left, x_edge).
                    push(self, left, x_edge.max(left), y0, y1, bg);
                    // Fill fg region [x_edge, right).
                    push(self, x_edge.max(left), right, y0, y1, fg);
                }
                true
            }

            // E0B1: right-pointing chevron outline.
            // A ">" stroke from (left,top)→(right,mid)→(left,bot), `stroke` px wide.
            0xE0B1 => {
                for row in 0..cell_h {
                    let y0 = top + row;
                    let y1 = y0 + 1;
                    // Distance from the nearer half-diagonal (upper or lower).
                    let half = (cell_h as f32 - 1.0) / 2.0;
                    let dist_from_mid = (row as f32 - half).abs();
                    let frac = 1.0 - dist_from_mid / (cell_h as f32 / 2.0);
                    let x_center = left + (frac * cell_w as f32).round() as i32;
                    let x0 = (x_center - stroke as i32).max(left);
                    let x1 = (x_center + 1).min(right);
                    push(self, x0, x1, y0, y1, fg);
                }
                true
            }

            // E0B3: left-pointing chevron outline (mirror of E0B1).
            // A "<" stroke from (right,top)→(left,mid)→(right,bot), `stroke` px wide.
            0xE0B3 => {
                for row in 0..cell_h {
                    let y0 = top + row;
                    let y1 = y0 + 1;
                    let half = (cell_h as f32 - 1.0) / 2.0;
                    let dist_from_mid = (row as f32 - half).abs();
                    let frac = 1.0 - dist_from_mid / (cell_h as f32 / 2.0);
                    let x_center = right - (frac * cell_w as f32).round() as i32;
                    let x0 = left.max(x_center - 1);
                    let x1 = (x_center + stroke as i32).min(right);
                    push(self, x0, x1, y0, y1, fg);
                }
                true
            }

            _ => false,
        }
    }

    pub fn render(&mut self) -> RenderResult {
        // wgpu 29 returns a `CurrentSurfaceTexture` enum (there is no `SurfaceError`
        // in this version). `Success`/`Suboptimal` give us a frame; the remaining
        // variants are transient acquisition failures. We self-heal `Lost`/`Outdated`
        // by reconfiguring with the stored size and skip the frame; other states are
        // skipped silently and retried next redraw.
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(f)
            | wgpu::CurrentSurfaceTexture::Suboptimal(f) => f,
            wgpu::CurrentSurfaceTexture::Lost | wgpu::CurrentSurfaceTexture::Outdated => {
                let (w, h) = (self.config.width, self.config.height);
                self.resize(w, h);
                return Err(SurfaceError::Outdated);
            }
            wgpu::CurrentSurfaceTexture::Timeout => return Err(SurfaceError::Timeout),
            wgpu::CurrentSurfaceTexture::Occluded => return Err(SurfaceError::Occluded),
            wgpu::CurrentSurfaceTexture::Validation => return Err(SurfaceError::Validation),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        // Flatten changed rows + upload only the dirty sub-ranges (or grow + full
        // upload when a row's count shifted the layout).
        self.end_frame();
        let bg_count = self.bg_count;
        let fg_count = self.fg_count;

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder"),
            });
        self.record_passes(&view, &mut encoder, bg_count, fg_count);

        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }

    /// Record the bg + fg instanced draws into `view`. Shared by the on-screen
    /// `render()` path and the offscreen `capture()` path.
    pub(crate) fn record_passes(
        &self,
        view: &wgpu::TextureView,
        encoder: &mut wgpu::CommandEncoder,
        bg_count: u32,
        fg_count: u32,
    ) {
        let [r, g, b, a] = self.clear_color;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("grid-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: r as f64,
                        g: g as f64,
                        b: b as f64,
                        a: a as f64,
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });

        if bg_count > 0 {
            pass.set_pipeline(&self.bg_pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_vertex_buffer(0, self.unit_quad.slice(..));
            pass.set_vertex_buffer(1, self.bg_buffer.slice(..));
            pass.draw(0..4, 0..bg_count);
        }
        if fg_count > 0 {
            pass.set_pipeline(&self.fg_pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_bind_group(1, &self.atlas_bind_group, &[]);
            pass.set_vertex_buffer(0, self.unit_quad.slice(..));
            pass.set_vertex_buffer(1, self.fg_buffer.slice(..));
            pass.draw(0..4, 0..fg_count);
        }
        // Inline images overlay on top of the grid, reusing the fg pipeline with
        // the image atlas bound in the color slot (image quads use flags == 1).
        // The fg pass above already bound the fg pipeline, uniform group, and the
        // unit-quad vertex buffer identically, so when it ran we only need to swap
        // the atlas bind group + instance buffer here.
        if self.image_count > 0 {
            // image_bind_group is guaranteed Some when image_count > 0 because
            // draw_image lazily initializes it before pushing any image instance.
            if let Some(image_bg) = &self.image_bind_group {
                if fg_count == 0 {
                    pass.set_pipeline(&self.fg_pipeline);
                    pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                    pass.set_vertex_buffer(0, self.unit_quad.slice(..));
                }
                pass.set_bind_group(1, image_bg, &[]);
                pass.set_vertex_buffer(1, self.image_buffer.slice(..));
                pass.draw(0..4, 0..self.image_count);
            }
        }
        // Translucent panel overlay (modals / menus): drawn last so it composites
        // over the finished grid + images. Body / backdrop / border rails use the
        // bg vertex layout with the premultiplied-blend overlay pipeline.
        if self.overlay_count > 0 {
            pass.set_pipeline(&self.overlay_pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_vertex_buffer(0, self.unit_quad.slice(..));
            pass.set_vertex_buffer(1, self.overlay_buffer.slice(..));
            pass.draw(0..4, 0..self.overlay_count);
        }
        // Panel text-on-glass: glyphs drawn after the overlay quads so they stay
        // crisp on top of the translucent body. Reuses the fg pipeline + text atlas.
        if self.overlay_text_count > 0 {
            pass.set_pipeline(&self.fg_pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_bind_group(1, &self.atlas_bind_group, &[]);
            pass.set_vertex_buffer(0, self.unit_quad.slice(..));
            pass.set_vertex_buffer(1, self.overlay_text_buffer.slice(..));
            pass.draw(0..4, 0..self.overlay_text_count);
        }
    }

    // --- Multi-pane (split) render path ------------------------------------
    //
    // The single-grid path above (begin_frame/begin_row/push_cell/render) is the
    // zero-overhead default used whenever a tab has one pane. The methods below
    // are a SEPARATE path for tiled splits: each pane is built relative to its
    // own pixel origin and drawn under its own scissor rect, so panes never bleed
    // into one another. This path forgoes per-row damage tracking (it rebuilds
    // every frame) — splitting is rare, and the simplicity is worth it.
    //
    // Usage per frame:
    //   begin_multi_frame(default_bg);
    //   for each pane:
    //     begin_pane(rect, focused);
    //     begin_row(r); push_cell(...); ...        // local (col,row) as usual
    //     push_cursor(...);                         // local (col,row)
    //     end_pane();
    //   draw_divider(rect, ...) / focus border emitted by end_pane;
    //   render_multi();
    //
    // `begin_row`/`push_cell`/`push_cursor` are reused verbatim: a pane authors
    // its cells into `self.rows` exactly like the single-grid path, and `end_pane`
    // translates those instances by the pane's pixel origin into the flat
    // per-pane lists. So all the box-drawing / decoration / glyph / cursor logic
    // is shared with zero duplication.
}
