//! Multi-pane (scissored) rendering API.

use super::*;

impl Renderer {
    /// Begin a multi-pane frame: set the clear color and reset the pane lists.
    /// Follow with one `begin_pane`/.../`end_pane` group per pane, then
    /// `render_multi`. Mirrors [`Renderer::begin_frame`]'s clear-color handling.
    #[allow(dead_code)]
    pub fn begin_multi_frame(&mut self, default_bg: [f32; 4]) {
        self.clear_color = self.glass_bg(default_bg);
        self.image_overlay.clear();
        // GUI chrome (tab bar) + modal/menu overlays are rebuilt every frame in the
        // split path too, so clear last frame's lists here (mirrors begin_frame).
        self.overlay_quads.clear();
        self.overlay_text.clear();
        self.mp.panes.clear();
        self.mp.bg.clear();
        self.mp.fg.clear();
        self.mp.cur = None;
        self.mp.reused_any = false;
    }

    /// Drop any cached panes whose ids are not in `live` (closed/merged panes), so
    /// the cache never grows beyond the current split. Call once per split frame
    /// after collecting the live pane ids. Cheap (a single retain).
    pub fn retain_panes(&mut self, live: &[usize]) {
        self.mp.cache.retain(|id, _| live.contains(id));
    }

    /// Whether pane `id` has cached instances available for reuse this frame.
    pub fn has_cached_pane(&self, id: usize) -> bool {
        self.mp.cache.contains_key(&id)
    }

    /// Re-emit a previously cached pane (unchanged content) into this frame's flat
    /// instance lists + draw order, skipping the expensive `push_pane` rebuild.
    /// The caller must have verified [`Renderer::has_cached_pane`] for `id`.
    pub fn reuse_pane(&mut self, id: usize) {
        let Some(cached) = self.mp.cache.get(&id) else {
            return;
        };
        let bg_start = self.mp.bg.len() as u32;
        let fg_start = self.mp.fg.len() as u32;
        self.mp.bg.extend_from_slice(&cached.bg);
        self.mp.fg.extend_from_slice(&cached.fg);
        self.mp.panes.push(PaneDraw {
            scissor: cached.scissor,
            bg_start,
            bg_end: self.mp.bg.len() as u32,
            fg_start,
            fg_end: self.mp.fg.len() as u32,
        });
        self.mp.reused_any = true;
    }

    /// Begin a pane occupying surface-pixel rectangle `rect`. Cells pushed until
    /// the matching [`Renderer::end_pane`] are authored with local `(col, row)`
    /// and end up positioned at `rect`'s origin under a scissor clamped to `rect`.
    /// `focused` requests the subtle accent focus border (drawn by `end_pane`).
    /// The per-pane grid is sized large enough to hold the pane's rows via the
    /// usual `begin_row`; oversized rows are clamped by the scissor.
    pub fn begin_pane(&mut self, id: usize, rect: crate::pane::Rect, focused: bool) {
        // Reset the shared scratch rows so this pane starts clean. The pane's
        // grid height is unknown here; `begin_row` grows `self.rows` on demand.
        self.rows.clear();
        self.dirty_rows.clear();
        let scissor = clamp_scissor(
            rect.x,
            rect.y,
            rect.w,
            rect.h,
            self.config.width,
            self.config.height,
        );
        self.mp.cur = Some(PaneBuild {
            id,
            origin: [rect.x as f32, rect.y as f32],
            scissor,
            focused,
        });
    }

    /// Finish the current pane: flush its authored rows (offset by the pane's
    /// pixel origin) into the flat instance lists, optionally adding the focus
    /// border, and record the pane's scissored draw range.
    pub fn end_pane(&mut self) {
        let Some(build) = self.mp.cur.take() else {
            return;
        };
        let [ox, oy] = build.origin;
        // Build this pane's origin-translated instances into a fresh CachedPane.
        // The cells were laid out at local pixel coords (col*cell_w + pad, ...), so
        // adding the pane's top-left gives absolute surface pixels. Reuse the
        // previous CachedPane's allocations (clear + refill) to avoid per-frame
        // Vec churn on the changed pane.
        let mut cached = self.mp.cache.remove(&build.id).unwrap_or_default();
        cached.bg.clear();
        cached.fg.clear();
        cached.scissor = build.scissor;
        for row in &self.rows {
            for b in &row.bg {
                cached.bg.push(BgInstance {
                    pos: [b.pos[0] + ox, b.pos[1] + oy],
                    size: b.size,
                    color: b.color,
                });
            }
        }
        for row in &self.rows {
            for f in &row.fg {
                let mut f = *f;
                f.pos = [f.pos[0] + ox, f.pos[1] + oy];
                cached.fg.push(f);
            }
        }

        // Focused-pane marker: a single 1px accent LEFT rail just inside the pane
        // rect, in the bg pass (after the pane's cell backgrounds, before glyphs)
        // and clipped by this pane's scissor. Downgraded from the former four-rail
        // box now that pane headers carry the primary focus chrome (GUI layer).
        // Cached with the pane so the focus rail is preserved on reuse; the caller
        // rebuilds the pane (focus changed ⇒ content/marker changed) on focus moves.
        if build.focused {
            let th = (self.metrics.height / 12.0).round().max(1.0);
            let s = build.scissor;
            if s.w > 0 && s.h > 0 {
                let x = s.x as f32;
                let y = s.y as f32;
                let h = s.h as f32;
                let c = crate::color::accent();
                cached.bg.push(BgInstance {
                    pos: [x, y],
                    size: [th, h],
                    color: c,
                }); // left
            }
        }

        // Append the freshly-built instances to this frame's flat lists + draw
        // order, then stash the CachedPane for reuse on subsequent unchanged frames.
        let bg_start = self.mp.bg.len() as u32;
        let fg_start = self.mp.fg.len() as u32;
        self.mp.bg.extend_from_slice(&cached.bg);
        self.mp.fg.extend_from_slice(&cached.fg);
        self.mp.panes.push(PaneDraw {
            scissor: build.scissor,
            bg_start,
            bg_end: self.mp.bg.len() as u32,
            fg_start,
            fg_end: self.mp.fg.len() as u32,
        });
        self.mp.cache.insert(build.id, cached);
        // Leave `self.rows` cleared so the next pane (or a return to the
        // single-grid path via `resize_grid`) starts fresh.
        self.rows.clear();
        self.dirty_rows.clear();
    }

    /// Emit a thin 1px divider rectangle at surface-pixel rect `(x, y, w, h)` in
    /// `color`. Call between `end_pane` and `render_multi` (e.g. once per gap
    /// between adjacent panes). Drawn full-screen scissored in the bg pass.
    #[allow(dead_code)]
    pub fn push_divider(&mut self, x: i32, y: i32, w: i32, h: i32, color: [f32; 4]) {
        if w <= 0 || h <= 0 {
            return;
        }
        // A divider belongs to no pane; record it as its own single-quad draw
        // with a full-surface scissor so it is never clipped by a pane rect.
        let bg_start = self.mp.bg.len() as u32;
        self.mp.bg.push(BgInstance {
            pos: [x as f32, y as f32],
            size: [w as f32, h as f32],
            color,
        });
        self.mp.panes.push(PaneDraw {
            scissor: clamp_scissor(
                0,
                0,
                self.config.width as i32,
                self.config.height as i32,
                self.config.width,
                self.config.height,
            ),
            bg_start,
            bg_end: self.mp.bg.len() as u32,
            fg_start: 0,
            fg_end: 0,
        });
    }

    /// Present the multi-pane frame: upload the flat instance lists and issue one
    /// scissored bg + fg draw per pane. Mirrors [`Renderer::render`]'s surface
    /// acquisition / self-heal handling.
    #[allow(dead_code)]
    pub fn render_multi(&mut self) -> RenderResult {
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

        Self::upload_mp_buffer::<BgInstance>(
            &self.device,
            &self.queue,
            &self.mp.bg,
            &mut self.mp.bg_buffer,
            &mut self.mp.bg_capacity,
            "mp-bg-instances",
        );
        Self::upload_mp_buffer::<FgInstance>(
            &self.device,
            &self.queue,
            &self.mp.fg,
            &mut self.mp.fg_buffer,
            &mut self.mp.fg_capacity,
            "mp-fg-instances",
        );
        self.upload_overlay_buffers();

        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("frame-encoder-mp"),
            });
        self.record_multi_passes(&view, &mut encoder);
        self.queue.submit(std::iter::once(encoder.finish()));
        frame.present();
        Ok(())
    }

    /// Upload a flat multi-pane instance list, growing the buffer if needed.
    pub(crate) fn upload_mp_buffer<T: bytemuck::Pod>(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        data: &[T],
        buffer: &mut Option<wgpu::Buffer>,
        capacity: &mut usize,
        label: &str,
    ) {
        if data.is_empty() {
            return;
        }
        let stride = std::mem::size_of::<T>();
        if buffer.is_none() || data.len() > *capacity {
            let cap = data.len().next_power_of_two().max(INITIAL_INSTANCES);
            *buffer = Some(device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (cap * stride) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }));
            *capacity = cap;
        }
        queue.write_buffer(buffer.as_ref().unwrap(), 0, bytemuck::cast_slice(data));
    }

    /// Record the multi-pane passes: clear once, then for each pane set its
    /// scissor and draw its bg + fg instance sub-ranges. The image overlay (if
    /// any) draws full-surface on top, matching the single-grid path.
    pub(crate) fn record_multi_passes(
        &self,
        view: &wgpu::TextureView,
        encoder: &mut wgpu::CommandEncoder,
    ) {
        let [r, g, b, a] = self.clear_color;
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("grid-pass-mp"),
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

        for p in &self.mp.panes {
            let s = p.scissor;
            if s.w == 0 || s.h == 0 {
                continue;
            }
            pass.set_scissor_rect(s.x, s.y, s.w, s.h);
            if p.bg_end > p.bg_start
                && let Some(buf) = &self.mp.bg_buffer
            {
                pass.set_pipeline(&self.bg_pipeline);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_vertex_buffer(0, self.unit_quad.slice(..));
                pass.set_vertex_buffer(1, buf.slice(..));
                pass.draw(0..4, p.bg_start..p.bg_end);
            }
            if p.fg_end > p.fg_start
                && let Some(buf) = &self.mp.fg_buffer
            {
                pass.set_pipeline(&self.fg_pipeline);
                pass.set_bind_group(0, &self.uniform_bind_group, &[]);
                pass.set_bind_group(1, &self.atlas_bind_group, &[]);
                pass.set_vertex_buffer(0, self.unit_quad.slice(..));
                pass.set_vertex_buffer(1, buf.slice(..));
                pass.draw(0..4, p.fg_start..p.fg_end);
            }
        }

        // GUI chrome (tab bar) + modal/menu overlays: drawn full-surface (scissor
        // reset) AFTER every pane so they composite over the whole split, mirroring
        // the single-pane path. Same buffers, same pipelines.
        pass.set_scissor_rect(0, 0, self.config.width, self.config.height);
        if self.overlay_count > 0 {
            pass.set_pipeline(&self.overlay_pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_vertex_buffer(0, self.unit_quad.slice(..));
            pass.set_vertex_buffer(1, self.overlay_buffer.slice(..));
            pass.draw(0..4, 0..self.overlay_count);
        }
        if self.overlay_text_count > 0 {
            pass.set_pipeline(&self.fg_pipeline);
            pass.set_bind_group(0, &self.uniform_bind_group, &[]);
            pass.set_bind_group(1, &self.atlas_bind_group, &[]);
            pass.set_vertex_buffer(0, self.unit_quad.slice(..));
            pass.set_vertex_buffer(1, self.overlay_text_buffer.slice(..));
            pass.draw(0..4, 0..self.overlay_text_count);
        }
    }
}
