//! Immediate-mode UI core: Ui struct and basic widget methods.

use super::*;

pub struct Ui<'r> {
    pub r: &'r mut Renderer,
    pub m: Metrics,
    mouse: (f32, f32),
    mouse_down: bool,
    /// Press→release edge observed this frame (set by App from MouseInput).
    clicked: bool,
    hovered: Option<WidgetId>,
    pressed: &'r mut Option<WidgetId>,
    focused: &'r mut Option<WidgetId>,
    tab_order: Vec<WidgetId>,
    anims: &'r mut HashMap<WidgetId, Anim>,
}

impl<'r> Ui<'r> {
    /// Begin a chrome paint frame. `mouse` is the cursor in physical px,
    /// `mouse_down` the current left-button state, `clicked` the press→release
    /// edge captured this frame by the App's MouseInput handler.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        r: &'r mut Renderer,
        cell_w: f32,
        cell_h: f32,
        mouse: (f32, f32),
        mouse_down: bool,
        clicked: bool,
        pressed: &'r mut Option<WidgetId>,
        focused: &'r mut Option<WidgetId>,
        anims: &'r mut HashMap<WidgetId, Anim>,
    ) -> Self {
        let m = Metrics::new(cell_w, cell_h);
        Ui {
            r,
            m,
            mouse,
            mouse_down,
            clicked,
            hovered: None,
            pressed,
            focused,
            tab_order: Vec::new(),
            anims,
        }
    }

    /// The collected keyboard tab order (declaration order) for this frame.
    pub fn tab_order(&self) -> &[WidgetId] {
        &self.tab_order
    }

    pub(crate) fn anim(&mut self, wid: WidgetId, target: f32) -> f32 {
        let a = self.anims.entry(wid).or_insert_with(|| Anim::new(target));
        a.target = target;
        a.value
    }

    /// Core hit/interaction resolution for a clickable widget rect. Records the
    /// widget in the tab order, updates pressed/hovered, and returns the result.
    pub(crate) fn interact(&mut self, wid: WidgetId, rect: Rect, enabled: bool) -> Interaction {
        self.tab_order.push(wid);
        if !enabled {
            return Interaction::default();
        }
        let over = hit(rect, self.mouse.0, self.mouse.1);
        if over {
            self.hovered = Some(wid);
        }
        // Press latch: claim the widget on button-down over it.
        if over && self.mouse_down && self.pressed.is_none() {
            *self.pressed = Some(wid);
            *self.focused = Some(wid);
        }
        let pressed = *self.pressed == Some(wid) && self.mouse_down;
        let clicked = self.clicked && over && *self.pressed == Some(wid);
        Interaction {
            hovered: over,
            pressed,
            clicked,
            changed: false,
        }
    }

    pub(crate) fn wstate(&self, wid: WidgetId, it: &Interaction, enabled: bool) -> WState {
        if !enabled {
            WState::Disabled
        } else if it.pressed {
            WState::Press
        } else if it.hovered {
            WState::Hover
        } else if *self.focused == Some(wid) {
            WState::Focus
        } else {
            WState::Idle
        }
    }

    // -- low-level emit helpers -------------------------------------------

    pub(crate) fn quad(&mut self, r: Rect, color: [f32; 4]) {
        self.r.push_overlay_px(r.x, r.y, r.w, r.h, color);
    }

    pub(crate) fn rrect(&mut self, r: Rect, radius: f32, color: [f32; 4]) {
        self.r
            .push_overlay_rrect_px(r.x, r.y, r.w, r.h, radius, color);
    }

    /// Edge-lit signature: a 1px `rail` on the TOP edge + a 1px `hairline` on the
    /// BOTTOM edge of a raised surface (two quads), reading as a beveled pane.
    pub(crate) fn edge_light(&mut self, r: Rect) {
        self.quad(Rect::new(r.x, r.y, r.w, 1.0), rail());
        self.quad(Rect::new(r.x, r.y + r.h - 1.0, r.w, 1.0), hairline());
    }

    /// A 1 px accent outline rrect — the keyboard-focus ring. Drawn as the
    /// outer rrect minus a 1 px inset inner rrect (SDF alpha difference), which
    /// produces clean rounded corners instead of the previous 4-quad approach
    /// that left mitre gaps.
    pub(crate) fn focus_ring(&mut self, r: Rect, radius: f32) {
        let c = color::accent();
        // Outer filled rrect.
        self.rrect(r, radius, c);
        // Inner filled rrect in the panel background — subtracts to leave a 1 px ring.
        let inner = r.inset(1.0);
        if inner.w > 0.0 && inner.h > 0.0 {
            // Use the same glass_raised fill so the ring appears as a 1 px halo.
            self.rrect(inner, (radius - 1.0).max(0.0), glass_raised());
        }
    }

    // -- text -------------------------------------------------------------

    /// Width in px of `s` in the panel font (monospace, exact).
    pub fn text_width(&self, s: &str) -> f32 {
        self.r.text_width_px(s)
    }

    /// Draw `text` left-aligned with its cell-box top at `(x, y)`.
    pub fn label(&mut self, x: f32, y: f32, text: &str, color: [f32; 4]) {
        let mut cx = x;
        for ch in text.chars() {
            self.r.push_overlay_glyph_px(cx, y, ch, color);
            cx += self.m.cell_w;
        }
    }

    /// Draw `text` so its right edge ends at `x_right`, top at `y`.
    pub fn label_right(&mut self, x_right: f32, y: f32, text: &str, color: [f32; 4]) {
        let w = self.text_width(text);
        self.label(x_right - w, y, text, color);
    }

    /// Draw `text` centered horizontally in `[x, x+w)`, vertically within `h`.
    pub fn label_centered(&mut self, rect: Rect, text: &str, color: [f32; 4]) {
        let tw = self.text_width(text);
        let tx = rect.x + (rect.w - tw) * 0.5;
        let ty = rect.center_y() - self.m.cell_h * 0.5;
        self.label(tx.round(), ty.round(), text, color);
    }

    // -- containers -------------------------------------------------------

    /// A raised surface panel with a left accent rail (E2). Returns the inner
    /// content rect (inset by `pad`).
    ///
    /// The rail is drawn as a 1 px rrect that matches the panel's shape so it
    /// doesn't bleed into the transparent corner feather zone that the rrect
    /// SDF leaves (the old flat quad extended into those corners and produced a
    /// coloured fringe against the terminal background).
    pub fn panel(&mut self, rect: Rect, radius: f32) -> Rect {
        self.rrect(rect, radius, glass_raised());
        // Left accent rail — draw as a narrow rrect with the same radius so its
        // corners are clipped identically to the panel background, preventing
        // colour bleed into the transparent corner feather.
        let rail_rect = Rect::new(rect.x, rect.y, (1.0_f32).max(radius * 0.5), rect.h);
        self.rrect(rail_rect, radius, rail());
        rect.inset(self.m.pad)
    }

    /// A lighter card surface on glass (E2), no rail.
    pub fn card(&mut self, rect: Rect, radius: f32) {
        self.rrect(rect, radius, lighten(glass_raised(), 0.04));
        self.edge_light(rect);
    }

    /// A thin separator line at `(x, y)` of width `w`.
    pub fn separator(&mut self, x: f32, y: f32, w: f32) {
        self.quad(Rect::new(x, y, w, 1.0), hairline());
    }

    // -- controls ---------------------------------------------------------

    /// A labelled push button. Returns its interaction.
    pub fn button(&mut self, wid: WidgetId, rect: Rect, text: &str) -> Interaction {
        let it = self.interact(wid, rect, true);
        let st = self.wstate(wid, &it, true);
        let hover_t = self.anim(
            wid,
            if matches!(st, WState::Hover | WState::Press) {
                1.0
            } else {
                0.0
            },
        );
        let fill = state_fill(glass_raised(), hover_t, it.pressed);
        self.rrect(rect, self.m.radius, fill);
        if hover_t > 0.0 && !it.pressed {
            self.quad(Rect::new(rect.x, rect.y, rect.w, 1.0), rail());
        }
        if matches!(st, WState::Focus) {
            self.focus_ring(rect, self.m.radius);
        }
        let nudge = if it.pressed { 1.0 } else { 0.0 };
        let mut content = rect;
        content.y += nudge;
        self.label_centered(content, text, fg());
        it
    }

    /// An icon button (single glyph). Returns its interaction.
    pub fn icon_button(&mut self, wid: WidgetId, rect: Rect, glyph: char) -> Interaction {
        let it = self.interact(wid, rect, true);
        let st = self.wstate(wid, &it, true);
        let hover_t = self.anim(
            wid,
            if matches!(st, WState::Hover | WState::Press) {
                1.0
            } else {
                0.0
            },
        );
        if hover_t > 0.0 || it.pressed {
            let fill = state_fill(glass_raised(), hover_t, it.pressed);
            self.rrect(rect, self.m.radius, fill);
        }
        if matches!(st, WState::Focus) {
            self.focus_ring(rect, self.m.radius);
        }
        let nudge = if it.pressed { 1.0 } else { 0.0 };
        let cx = rect.x + (rect.w - self.m.cell_w) * 0.5;
        let cy = rect.center_y() - self.m.cell_h * 0.5 + nudge;
        self.r
            .push_overlay_glyph_px(cx.round(), cy.round(), glyph, fg());
        it
    }

    /// A toggle switch. Returns the (possibly flipped) value.
    pub fn toggle(&mut self, wid: WidgetId, rect: Rect, value: bool) -> bool {
        let it = self.interact(wid, rect, true);
        let mut v = value;
        if it.clicked {
            v = !v;
        }
        let on_t = self.anim(wid, if v { 1.0 } else { 0.0 });
        let track = if v {
            // blend track_off -> fill_on by on_t
            let a = track_off();
            let b = fill_on();
            [
                a[0] + (b[0] - a[0]) * on_t,
                a[1] + (b[1] - a[1]) * on_t,
                a[2] + (b[2] - a[2]) * on_t,
                a[3] + (b[3] - a[3]) * on_t,
            ]
        } else {
            track_off()
        };
        let rr = rect.h * 0.5;
        self.rrect(rect, rr, track);
        if *self.focused == Some(wid) {
            self.focus_ring(rect, rr);
        }
        // Knob.
        let pad = 2.0;
        let k = rect.h - 2.0 * pad;
        let kx = rect.x + pad + (rect.w - 2.0 * pad - k) * on_t;
        self.rrect(Rect::new(kx, rect.y + pad, k, k), k * 0.5, fg());
        v
    }

    /// A segmented control (radio row). Returns the selected index.
    pub fn segmented(&mut self, wid: WidgetId, rect: Rect, options: &[&str], sel: usize) -> usize {
        let n = options.len().max(1);
        self.rrect(rect, self.m.radius, track_off());
        let seg_w = rect.w / n as f32;
        let mut chosen = sel;
        for (i, opt) in options.iter().enumerate() {
            let seg = Rect::new(rect.x + seg_w * i as f32, rect.y, seg_w, rect.h);
            let seg_id = id_combine(wid, i as u64);
            let it = self.interact(seg_id, seg, true);
            if it.clicked {
                chosen = i;
            }
            if i == sel {
                self.rrect(seg.inset(2.0), self.m.radius - 1.0, fill_on());
            } else if it.hovered {
                self.rrect(
                    seg.inset(2.0),
                    self.m.radius - 1.0,
                    state_fill(track_off(), 1.0, false),
                );
            }
            let tc = if i == sel { color::default_bg() } else { fg() };
            self.label_centered(seg, opt, tc);
        }
        if *self.focused == Some(wid) {
            self.focus_ring(rect, self.m.radius);
        }
        chosen
    }

    /// A horizontal slider. Returns the (possibly dragged) value, snapped to
    /// `step` and clamped to `[min, max]`.
    pub fn slider(
        &mut self,
        wid: WidgetId,
        rect: Rect,
        value: f32,
        min: f32,
        max: f32,
        step: f32,
    ) -> f32 {
        let it = self.interact(wid, rect, true);
        let mut v = value.clamp(min, max);
        if it.pressed && max > min {
            let t = ((self.mouse.0 - rect.x) / rect.w).clamp(0.0, 1.0);
            let raw = min + t * (max - min);
            v = if step > 0.0 {
                (raw / step).round() * step
            } else {
                raw
            }
            .clamp(min, max);
        }
        let t = if max > min {
            (v - min) / (max - min)
        } else {
            0.0
        };
        // Track.
        let mid = rect.center_y();
        let th = 4.0;
        let track = Rect::new(rect.x, mid - th * 0.5, rect.w, th);
        self.rrect(track, th * 0.5, track_off());
        // Filled portion.
        self.rrect(
            Rect::new(rect.x, mid - th * 0.5, rect.w * t, th),
            th * 0.5,
            fill_on(),
        );
        // Knob.
        let k = rect.h * 0.6;
        let kx = rect.x + rect.w * t - k * 0.5;
        self.rrect(Rect::new(kx, mid - k * 0.5, k, k), k * 0.5, fg());
        if *self.focused == Some(wid) {
            self.focus_ring(rect, rect.h * 0.5);
        }
        v
    }

    /// A `[− value +]` stepper. Returns the delta to apply (-1, 0, or +1 step
    /// clicks), letting the caller drive its own live effect. `text` is the
    /// rendered value between the buttons.
    pub fn stepper(&mut self, wid: WidgetId, rect: Rect, text: &str) -> i32 {
        let bw = rect.h;
        let dec = Rect::new(rect.x, rect.y, bw, rect.h);
        let inc = Rect::new(rect.x + rect.w - bw, rect.y, bw, rect.h);
        let mid = Rect::new(rect.x + bw, rect.y, rect.w - 2.0 * bw, rect.h);
        let d_it = self.button(id_combine(wid, 1), dec, "−");
        let i_it = self.button(id_combine(wid, 2), inc, "+");
        self.rrect(mid, self.m.radius, track_off());
        self.label_centered(mid, text, fg());
        if i_it.clicked {
            1
        } else if d_it.clicked {
            -1
        } else {
            0
        }
    }

    /// A dropdown header (the always-visible chooser button). Renders the current
    /// `label`, an optional left color `swatch`, and a `▾` chevron; returns
    /// [`DropdownEvt::Toggle`] when clicked so the caller flips its `open` state.
    /// The popup list itself is drawn separately via [`Ui::list`] (an E3 surface)
    /// so it composites above everything; pass `open` only to draw the pressed/
    /// active chrome here.
    pub fn dropdown(
        &mut self,
        wid: WidgetId,
        rect: Rect,
        label: &str,
        open: bool,
        swatch: Option<[f32; 4]>,
    ) -> DropdownEvt {
        let it = self.interact(wid, rect, true);
        let st = self.wstate(wid, &it, true);
        let hover_t = self.anim(
            wid,
            if open || matches!(st, WState::Hover | WState::Press) {
                1.0
            } else {
                0.0
            },
        );
        let fill = state_fill(glass_raised(), hover_t, it.pressed || open);
        self.rrect(rect, self.m.radius, fill);
        if hover_t > 0.0 && !it.pressed {
            self.quad(Rect::new(rect.x, rect.y, rect.w, 1.0), rail());
        }
        if matches!(st, WState::Focus) {
            self.focus_ring(rect, self.m.radius);
        }
        // Left swatch (e.g. theme preview), label, trailing chevron.
        let pad = self.m.pad;
        let mut tx = rect.x + pad;
        let ty = rect.center_y() - self.m.cell_h * 0.5;
        if let Some(sw) = swatch {
            let s = (self.m.cell_h * 0.8).round();
            let sy = rect.center_y() - s * 0.5;
            self.rrect(Rect::new(tx, sy, s, s), 3.0, sw);
            tx += s + self.m.gap;
        }
        self.label(tx.round(), ty.round(), label, fg());
        // Chevron flips appearance via glyph: ▴ when open, ▾ when closed.
        let chev = if open { '▴' } else { '▾' };
        let cx = rect.x + rect.w - pad - self.m.cell_w;
        self.r
            .push_overlay_glyph_px(cx.round(), ty.round(), chev, fg_dim());
        if it.clicked {
            DropdownEvt::Toggle
        } else {
            DropdownEvt::None
        }
    }

    /// A read-only text field with a leading-ellipsis clip (the END of `text`
    /// stays visible — ideal for paths) plus optional trailing copy (`⧉`) and
    /// open (`↗`) icon buttons. Returns which trailing icon was clicked.
    pub fn text_field_readonly(
        &mut self,
        wid: WidgetId,
        rect: Rect,
        text: &str,
        with_copy: bool,
        with_open: bool,
    ) -> FieldEvt {
        // Sunken track.
        self.rrect(rect, self.m.radius, track_off());
        if *self.focused == Some(wid) {
            self.focus_ring(rect, self.m.radius);
        }
        let pad = self.m.pad;
        // Reserve trailing icon slots.
        let icon_w = self.m.row_h;
        let mut right = rect.x + rect.w;
        let mut evt = FieldEvt::None;
        if with_open {
            right -= icon_w;
            let ir = Rect::new(right, rect.y, icon_w, rect.h);
            if self.icon_button(id_combine(wid, 2), ir, '↗').clicked {
                evt = FieldEvt::Open;
            }
        }
        if with_copy {
            right -= icon_w;
            let ir = Rect::new(right, rect.y, icon_w, rect.h);
            // U+29C9 ⧉ is BMP-safe (replaces U+1F5D0 🗐 which tofu on most terminal fonts).
            if self.icon_button(id_combine(wid, 1), ir, '\u{29C9}').clicked {
                evt = FieldEvt::Copy;
            }
        }
        // Text area = everything left of the icons.
        let text_w = (right - rect.x - 2.0 * pad).max(0.0);
        let max_chars = (text_w / self.m.cell_w).floor() as usize;
        let chars: Vec<char> = text.chars().collect();
        let ty = rect.center_y() - self.m.cell_h * 0.5;
        let tx = rect.x + pad;
        if chars.len() <= max_chars {
            self.label(tx.round(), ty.round(), text, fg());
        } else if max_chars >= 1 {
            // Leading ellipsis: keep the tail visible.
            let tail = &chars[chars.len() - (max_chars - 1)..];
            let mut s = String::from("…");
            s.extend(tail.iter());
            self.label(tx.round(), ty.round(), &s, fg());
        }
        evt
    }

    /// A scrollable selectable list. `rows` are the row labels; `sel` the
    /// currently-selected absolute index (highlighted); `scroll` the vertical
    /// scroll offset in px (the caller owns it and updates from the returned
    /// value of any companion [`Ui::scrollbar`]). Rows are clipped to `rect` by
    /// simple range-culling (no GPU scissor). Returns the row event this frame.
    pub fn list(
        &mut self,
        wid: WidgetId,
        rect: Rect,
        rows: &[&str],
        sel: usize,
        scroll: f32,
    ) -> ListEvt {
        let row_h = self.m.row_h;
        let mut evt = ListEvt::None;
        let first = (scroll / row_h).floor().max(0.0) as usize;
        let visible = (rect.h / row_h).ceil() as usize + 1;
        for (i, label) in rows.iter().enumerate().skip(first).take(visible) {
            let ry = rect.y + i as f32 * row_h - scroll;
            // Cull rows fully outside the viewport.
            if ry + row_h <= rect.y || ry >= rect.y + rect.h {
                continue;
            }
            let rr = Rect::new(rect.x, ry, rect.w, row_h);
            let row_id = id_combine(wid, i as u64);
            let it = self.interact(row_id, rr, true);
            if i == sel {
                self.rrect(rr.inset(1.0), self.m.radius - 1.0, sel_bg());
            } else if it.hovered {
                self.rrect(
                    rr.inset(1.0),
                    self.m.radius - 1.0,
                    state_fill(track_off(), 1.0, false),
                );
            }
            let ty = rr.center_y() - self.m.cell_h * 0.5;
            self.label((rr.x + self.m.pad).round(), ty.round(), label, fg());
            if it.clicked {
                evt = ListEvt::Clicked(i);
            } else if it.hovered && evt == ListEvt::None {
                evt = ListEvt::Hovered(i);
            }
        }
        evt
    }

    /// A vertical scrollbar bound to a scrollable region. `track` is the gutter
    /// rect; `content_h`/`view_h` size the thumb; `scroll` is the current offset
    /// in px. Returns the (possibly dragged) scroll offset, clamped to range.
    pub fn scrollbar(
        &mut self,
        wid: WidgetId,
        track: Rect,
        content_h: f32,
        view_h: f32,
        scroll: f32,
    ) -> f32 {
        let max_scroll = (content_h - view_h).max(0.0);
        let mut s = scroll.clamp(0.0, max_scroll);
        if max_scroll <= 0.0 {
            return 0.0; // nothing to scroll; draw no thumb
        }
        // Track.
        self.rrect(track, track.w * 0.5, track_off());
        let it = self.interact(wid, track, true);
        let thumb_h = (track.h * (view_h / content_h)).max(self.m.row_h * 0.6);
        let span = (track.h - thumb_h).max(0.0);
        if it.pressed && span > 0.0 {
            // Map the pointer to a scroll position (thumb-centered).
            let t = ((self.mouse.1 - track.y - thumb_h * 0.5) / span).clamp(0.0, 1.0);
            s = t * max_scroll;
        }
        let t = if max_scroll > 0.0 {
            s / max_scroll
        } else {
            0.0
        };
        let ty = track.y + span * t;
        let thumb = Rect::new(track.x, ty, track.w, thumb_h);
        let hover_t = self.anim(wid, if it.hovered || it.pressed { 1.0 } else { 0.0 });
        let fill = state_fill(with_alpha(fg(), 0.35), hover_t, it.pressed);
        self.rrect(thumb, track.w * 0.5, fill);
        s
    }
}
